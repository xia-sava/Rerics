//! ファイル一覧のシェルアイコン取得・キャッシュ・描画。プラットフォーム依存（Win32 シェル
//! API）なので core でなく GUI 側に置く。
//!
//! 二層構成：
//! - **同期・汎用**（拡張子/フォルダ単位）：`SHGFI_USEFILEATTRIBUTES` で実ファイルを触らず
//!   拡張子から引く。フォルダ=既定フォルダアイコン、登録の無い拡張子はシステムの汎用紙
//!   アイコン。書庫内エントリにも使え、即時に描けるのでプレースホルダになる。
//! - **非同期・per-file**（実FSのファイル）：バックグラウンドスレッドで実パスから
//!   `SHGFI_ADDOVERLAYS` 付きの固有アイコン（exe の埋込アイコン・ショートカット矢印等）を
//!   取得、画像ファイルは小さなサムネイルを生成する。結果はパス＋mtime でキャッシュし、
//!   取得できるまでは汎用アイコンを表示。完了で main 窓へ `winutil::msg::ICONS_READY` を Post して再描画。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use winsafe::{self as w};

// Win32 構造体の名前をそのまま写した FFI 宣言（命名は Win32 準拠）。
#[allow(non_snake_case, clippy::upper_case_acronyms)]
#[repr(C)]
struct SHFILEINFOW {
    hIcon: *mut c_void,
    iIcon: i32,
    dwAttributes: u32,
    szDisplayName: [u16; 260],
    szTypeName: [u16; 80],
}

#[allow(non_snake_case, clippy::upper_case_acronyms)]
#[repr(C)]
struct BITMAPINFOHEADER {
    biSize: u32,
    biWidth: i32,
    biHeight: i32,
    biPlanes: u16,
    biBitCount: u16,
    biCompression: u32,
    biSizeImage: u32,
    biXPelsPerMeter: i32,
    biYPelsPerMeter: i32,
    biClrUsed: u32,
    biClrImportant: u32,
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHGetFileInfoW(
        pszPath: *const u16,
        dwFileAttributes: u32,
        psfi: *mut SHFILEINFOW,
        cbSizeFileInfo: u32,
        uFlags: u32,
    ) -> usize;
}

#[allow(non_snake_case, clippy::upper_case_acronyms)]
#[repr(C)]
struct ICONINFO {
    fIcon: i32,
    xHotspot: u32,
    yHotspot: u32,
    hbmMask: *mut c_void,
    hbmColor: *mut c_void,
}

#[allow(non_snake_case, clippy::upper_case_acronyms)]
#[repr(C)]
struct BITMAP {
    bmType: i32,
    bmWidth: i32,
    bmHeight: i32,
    bmWidthBytes: i32,
    bmPlanes: u16,
    bmBitsPixel: u16,
    bmBits: *mut c_void,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn DrawIconEx(
        hdc: *mut c_void,
        xLeft: i32,
        yTop: i32,
        hIcon: *mut c_void,
        cxWidth: i32,
        cyWidth: i32,
        istepIfAniCur: u32,
        hbrFlickerFreeDraw: *mut c_void,
        diFlags: u32,
    ) -> i32;
    fn DestroyIcon(hIcon: *mut c_void) -> i32;
    fn GetIconInfo(hIcon: *mut c_void, piconinfo: *mut ICONINFO) -> i32;
    fn PostMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn GetObjectW(h: *mut c_void, c: i32, pv: *mut c_void) -> i32;
    fn GetDIBits(
        hdc: *mut c_void,
        hbm: *mut c_void,
        start: u32,
        lines: u32,
        lpvBits: *mut c_void,
        lpbmi: *mut BITMAPINFOHEADER,
        usage: u32,
    ) -> i32;
    fn CreateCompatibleDC(hdc: *mut c_void) -> *mut c_void;
    fn DeleteDC(hdc: *mut c_void) -> i32;
    fn DeleteObject(ho: *mut c_void) -> i32;
    fn StretchDIBits(
        hdc: *mut c_void,
        xDest: i32,
        yDest: i32,
        DestWidth: i32,
        DestHeight: i32,
        xSrc: i32,
        ySrc: i32,
        SrcWidth: i32,
        SrcHeight: i32,
        lpBits: *const c_void,
        lpbmi: *const BITMAPINFOHEADER,
        iUsage: u32,
        rop: u32,
    ) -> i32;
    fn SetStretchBltMode(hdc: *mut c_void, mode: i32) -> i32;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(pvReserved: *mut c_void, dwCoInit: u32) -> i32;
}

const SHGFI_ICON: u32 = 0x0000_0100;
const SHGFI_USEFILEATTRIBUTES: u32 = 0x0000_0010;
const SHGFI_ADDOVERLAYS: u32 = 0x0000_0020;
const SHGFI_SYSICONINDEX: u32 = 0x0000_4000;

/// この物理 px 以上で描くときは jumbo(256) のシェルアイコンを取り、余白をトリムして
/// サムネイルとして描く（高DPI・サムネイル表示でくっきり）。下回るときは従来の大アイコン。
const JUMBO_MIN_PX: i32 = 40;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

const DI_NORMAL: u32 = 0x0003;
const COINIT_APARTMENTTHREADED: u32 = 0x2;
const DIB_RGB_COLORS: u32 = 0;
const SRCCOPY: u32 = 0x00CC_0020;
const STRETCH_HALFTONE: i32 = 4;

/// アイコンの既定論理サイズ。自動サイズ時の上限（行に収める基準）で、描画時に DPI スケールする。
pub const ICON_LOGICAL: i32 = 16;

/// サムネイルをデコードする物理 px（サムネイル表示の既定 128 px を等倍で出せる解像度）。
/// 描画時は表示枠へ縮小する。
const THUMB_DECODE_PX: u32 = 128;

/// アイコン／サムネイルの描画先。`size` 角の枠で、左上が `(x, y)`。シェルアイコンは
/// `cap` を一辺の上限に原寸寄りで枠の中央へ置く（`cap >= size` で枠いっぱい）。画像
/// サムネイルは縦横比を保って枠いっぱいに拡縮するので `cap` は効かない。
#[derive(Clone, Copy)]
pub struct IconBox {
    pub x: i32,
    pub y: i32,
    pub size: i32,
    pub cap: i32,
}

impl IconBox {
    /// 枠内へ、一辺を `cap` 以下に抑えた正方形を中央寄せした `(原点x, 原点y, 一辺)` を返す。
    /// `cap >= size` なら枠いっぱい（中央寄せのオフセットは 0）。
    fn center_capped(self) -> (i32, i32, i32) {
        let s = self.size.min(self.cap.max(1));
        (self.x + (self.size - s) / 2, self.y + (self.size - s) / 2, s)
    }
}

/// サムネイル生成を諦めるファイルサイズ上限（巨大画像で OOM/遅延を避ける）。
const THUMB_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// 破棄責任つきの所有 HICON ラッパ。Drop で `DestroyIcon`。
struct OwnedIcon(*mut c_void);

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                DestroyIcon(self.0);
            }
        }
    }
}

/// スレッド間で HICON を移送するための Send ラッパ（HICON はプロセス共通ハンドルなので移送可）。
struct SendIcon(*mut c_void);
unsafe impl Send for SendIcon {}

/// ワーカが生成した描画素材（Send）。
enum WorkerDrawable {
    Icon(SendIcon),
    Thumb { w: u32, h: u32, bgra: Vec<u8> },
}

/// UI スレッド側の描画素材。
enum Drawable {
    Icon(OwnedIcon),
    /// トップダウン BGRA（既にアイコン枠に収まるよう縮小済み）。
    Thumb { w: u32, h: u32, bgra: Vec<u8> },
}

struct Request {
    path: PathBuf,
    mtime: u64,
    thumb: bool,
    /// jumbo(256) で取って余白トリムし、サムネとして描くか（大きく描くとき）。
    jumbo: bool,
}

struct IconResult {
    path: PathBuf,
    mtime: u64,
    payload: Option<WorkerDrawable>,
    /// jumbo として取得しようとしたか（成否によらず・再取得ループ防止の記録用）。
    jumbo: bool,
}

/// 与えた疑似パス・属性からシステムアイコン（HICON）を1つ取得する。失敗で null。
fn fetch_icon(pseudo: &str, attrs: u32, use_attrs: bool, add_overlays: bool) -> *mut c_void {
    let wide: Vec<u16> = std::ffi::OsStr::new(pseudo)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // 大アイコン（32px相当）を取得し、描画時に表示サイズへ縮小する（高DPIでもボケにくい）。
    let mut flags = SHGFI_ICON;
    if use_attrs {
        flags |= SHGFI_USEFILEATTRIBUTES;
    }
    if add_overlays {
        flags |= SHGFI_ADDOVERLAYS;
    }
    let mut info = SHFILEINFOW {
        hIcon: std::ptr::null_mut(),
        iIcon: 0,
        dwAttributes: 0,
        szDisplayName: [0; 260],
        szTypeName: [0; 80],
    };
    let rc = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            attrs,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if rc != 0 { info.hIcon } else { std::ptr::null_mut() }
}

/// ワーカスレッド本体。実パスから固有アイコン（オーバーレイ込み）や画像サムネを作って返す。
fn worker_loop(rx: Receiver<Request>, tx: Sender<IconResult>, wake: isize) {
    // シェルアイコン/オーバーレイ取得のため COM を初期化（失敗しても致命的でない）。
    unsafe {
        let _ = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
    }
    while let Ok(req) = rx.recv() {
        let payload = if req.thumb {
            make_thumb(&req.path)
        } else {
            let path_str = req.path.to_string_lossy();
            // 大きく描くときは jumbo(256) を余白トリムしてサムネ化（取れなければ大アイコン）。
            req.jumbo
                .then(|| fetch_jumbo_thumb(&path_str))
                .flatten()
                .or_else(|| {
                    let h = fetch_icon(&path_str, FILE_ATTRIBUTE_NORMAL, false, true);
                    (!h.is_null()).then_some(WorkerDrawable::Icon(SendIcon(h)))
                })
        };
        if tx
            .send(IconResult { path: req.path, mtime: req.mtime, payload, jumbo: req.jumbo })
            .is_err()
        {
            break;
        }
        unsafe {
            PostMessageW(wake as *mut c_void, crate::winutil::msg::ICONS_READY.raw(), 0, 0);
        }
    }
}

/// 実ファイルを読みサムネイル（アイコン枠に収まる BGRA・トップダウン）を作る。
fn make_thumb(path: &std::path::Path) -> Option<WorkerDrawable> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 || len > THUMB_MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let (w, h, rgba) = rerics_core::decode_thumbnail(&bytes, THUMB_DECODE_PX)?;
    let bgra = rerics_core::rgba_to_bgra(&rgba);
    Some(WorkerDrawable::Thumb { w, h, bgra })
}

/// 実パスのシェルアイコンを jumbo(256) で取得し、透明余白をトリムした BGRA サムネとして返す。
/// 取得・デコードに失敗したら None（呼び側は従来の大アイコンへ倒す）。
fn fetch_jumbo_thumb(path: &str) -> Option<WorkerDrawable> {
    use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
    use windows::Win32::UI::Shell::{SHGetImageList, SHIL_JUMBO};

    let wide: Vec<u16> = std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut info = SHFILEINFOW {
        hIcon: std::ptr::null_mut(),
        iIcon: 0,
        dwAttributes: 0,
        szDisplayName: [0; 260],
        szTypeName: [0; 80],
    };
    let rc = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            FILE_ATTRIBUTE_NORMAL,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        )
    };
    if rc == 0 {
        return None;
    }
    let hicon = unsafe {
        let list: IImageList = SHGetImageList(SHIL_JUMBO as i32).ok()?;
        list.GetIcon(info.iIcon, ILD_TRANSPARENT.0).ok()?
    };
    // 抽出後に DestroyIcon されるよう所有させる（早期 return でも Drop で確実に破棄）。
    let owned = OwnedIcon(hicon.0);
    let (w, h, bgra) = hicon_to_bgra(owned.0)?;
    let (tw, th, trimmed) = trim_transparent(w, h, &bgra)?;
    Some(WorkerDrawable::Thumb { w: tw, h: th, bgra: trimmed })
}

/// HICON を 32bpp トップダウン BGRA（アルファ込み）へ変換する。失敗で None。
fn hicon_to_bgra(hicon: *mut c_void) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(hicon, &mut ii) == 0 {
            return None;
        }
        let mut bm: BITMAP = std::mem::zeroed();
        let got = GetObjectW(
            ii.hbmColor,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bm as *mut BITMAP as *mut c_void,
        );
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let result = if got != 0 && w > 0 && h > 0 {
            let mut bgra = vec![0u8; (w * h * 4) as usize];
            let mut bi = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // トップダウン
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            let lines = GetDIBits(
                dc,
                ii.hbmColor,
                0,
                h as u32,
                bgra.as_mut_ptr() as *mut c_void,
                &mut bi,
                DIB_RGB_COLORS,
            );
            DeleteDC(dc);
            if lines != 0 { Some((w as u32, h as u32, bgra)) } else { None }
        } else {
            None
        };
        if !ii.hbmColor.is_null() {
            DeleteObject(ii.hbmColor);
        }
        if !ii.hbmMask.is_null() {
            DeleteObject(ii.hbmMask);
        }
        result
    }
}

/// BGRA からアルファのある領域の外接矩形を求めて切り出す。全透明なら None。
fn trim_transparent(w: u32, h: u32, bgra: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = (w as i32, h as i32);
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, -1, -1);
    for y in 0..h {
        for x in 0..w {
            // ほぼ透明（境界のアンチエイリアス残り）は無視する。
            if bgra[((y * w + x) * 4 + 3) as usize] > 8 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if max_x < min_x || max_y < min_y {
        return None;
    }
    let (tw, th) = ((max_x - min_x + 1) as usize, (max_y - min_y + 1) as usize);
    let row_bytes = tw * 4;
    let mut out = vec![0u8; row_bytes * th];
    for ty in 0..th {
        let src = ((min_y as usize + ty) * w as usize + min_x as usize) * 4;
        let dst = ty * row_bytes;
        out[dst..dst + row_bytes].copy_from_slice(&bgra[src..src + row_bytes]);
    }
    Some((tw as u32, th as u32, out))
}

/// per-file キャッシュの上限エントリ数。超えたら使用の古い順に落とし、HICON やサムネイルの
/// メモリ・GDI ハンドルを解放する（長時間の閲覧で無制限に膨らむのを防ぐ）。
const PER_FILE_CAP: usize = 2048;

/// per-file キャッシュの 1 エントリ。`last_used` は LRU 用の最終アクセス時刻（単調増加カウンタ）。
struct FileEntry {
    mtime: u64,
    drawable: Option<Drawable>,
    /// jumbo として取得しようとした結果か（標準アイコンを大表示へ上げ直す判定・再取得ループ防止）。
    jumbo: bool,
    last_used: Cell<u64>,
}

/// 拡張子・フォルダ単位の同期キャッシュ＋実FSファイルの非同期 per-file キャッシュ。UI スレッド専用。
pub struct IconCache {
    /// 汎用（同期）。キー: "<dir>" / 拡張子（小文字・ドット込み・無拡張子 "<none>"）。
    generic: RefCell<HashMap<String, *mut c_void>>,
    /// 汎用キャッシュが所有する HICON（Drop で破棄）。
    generic_owned: RefCell<Vec<OwnedIcon>>,
    /// per-file（非同期）。キー: 実フルパス。値: 解決結果（drawable が None=解決済みだが取得失敗）。
    per_file: RefCell<HashMap<PathBuf, FileEntry>>,
    /// LRU 用の単調増加カウンタ（アクセス・挿入のたびに進める）。
    tick: Cell<u64>,
    /// 取得中のパス（重複要求の抑止）。
    pending: RefCell<HashSet<PathBuf>>,
    req_tx: RefCell<Option<Sender<Request>>>,
    res_rx: RefCell<Option<Receiver<IconResult>>>,
}

impl IconCache {
    pub fn new() -> Self {
        Self {
            generic: RefCell::new(HashMap::new()),
            generic_owned: RefCell::new(Vec::new()),
            per_file: RefCell::new(HashMap::new()),
            tick: Cell::new(0),
            pending: RefCell::new(HashSet::new()),
            req_tx: RefCell::new(None),
            res_rx: RefCell::new(None),
        }
    }

    /// 非同期ワーカを起動する。`wake_hwnd` は完了通知（`msg::ICONS_READY`）の送り先（生 HWND）。
    pub fn start(&self, wake_hwnd: isize) {
        let (req_tx, req_rx) = channel::<Request>();
        let (res_tx, res_rx) = channel::<IconResult>();
        std::thread::Builder::new()
            .name("icon-loader".to_owned())
            .spawn(move || worker_loop(req_rx, res_tx, wake_hwnd))
            .ok();
        *self.req_tx.borrow_mut() = Some(req_tx);
        *self.res_rx.borrow_mut() = Some(res_rx);
    }

    fn generic_handle(&self, is_dir: bool, ext: &str) -> *mut c_void {
        let key = if is_dir {
            "<dir>".to_owned()
        } else if ext.is_empty() {
            "<none>".to_owned()
        } else {
            ext.to_ascii_lowercase()
        };
        if let Some(h) = self.generic.borrow().get(&key) {
            return *h;
        }
        let h = if is_dir {
            fetch_icon("folder", FILE_ATTRIBUTE_DIRECTORY, true, false)
        } else {
            let pseudo = format!("x{}", ext);
            fetch_icon(&pseudo, FILE_ATTRIBUTE_NORMAL, true, false)
        };
        if !h.is_null() {
            self.generic_owned.borrow_mut().push(OwnedIcon(h));
        }
        self.generic.borrow_mut().insert(key, h);
        h
    }

    /// 汎用アイコン（ディレクトリ／拡張子別）を `dest` の枠に描く。シェルアイコンは
    /// 引き伸ばすとぼやけるので `dest.cap` を上限に原寸寄りで枠の中央へ置く（サムネイル
    /// 表示で枠が大きいとき用。通常表示は `cap == size` で従来どおり枠いっぱいに描く）。
    pub fn draw_generic(&self, dc: &w::HDC, is_dir: bool, ext: &str, dest: IconBox) {
        let h = self.generic_handle(is_dir, ext);
        if !h.is_null() {
            let (ox, oy, s) = dest.center_capped();
            unsafe {
                DrawIconEx(dc.ptr(), ox, oy, h, s, s, 0, std::ptr::null_mut(), DI_NORMAL);
            }
        }
    }

    /// per-file 解決済みアイコン/サムネがあれば (x,y,size 枠) に描く。描けたら true。
    /// 無ければ false（呼び出し側が汎用を描き、必要なら `request_file` で取得を依頼する）。
    pub fn draw_file(
        &self,
        dc: &w::HDC,
        path: &std::path::Path,
        mtime: u64,
        dest: IconBox,
    ) -> bool {
        let map = self.per_file.borrow();
        let Some(entry) = map.get(path) else {
            return false;
        };
        if entry.mtime != mtime {
            return false;
        }
        let Some(drawable) = entry.drawable.as_ref() else {
            return false;
        };
        // 大きく描くのに標準アイコンしか無く、まだ jumbo を試していなければ未解決扱いにして
        // 上げ直させる（呼び側が汎用を描き jumbo を再依頼する）。jumbo 試行済みならそのまま使う。
        if matches!(drawable, Drawable::Icon(_)) && dest.size >= JUMBO_MIN_PX && !entry.jumbo {
            return false;
        }
        entry.last_used.set(self.next_tick());
        let IconBox { x, y, size, .. } = dest;
        match drawable {
            // シェルアイコンは枠が大きいとき原寸寄りで中央へ（サムネイル表示用）。画像サムネは
            // 縦横比を保って枠いっぱいに拡縮するので `cap` の対象外。
            Drawable::Icon(icon) => unsafe {
                let (ox, oy, s) = dest.center_capped();
                DrawIconEx(dc.ptr(), ox, oy, icon.0, s, s, 0, std::ptr::null_mut(), DI_NORMAL);
            },
            Drawable::Thumb { w, h, bgra } => {
                let (iw, ih) = (*w as i32, *h as i32);
                // 表示枠 size に長辺を合わせ、縦横比を保って縮小して中央へ置く。
                let long = iw.max(ih).max(1);
                let dw = (iw * size / long).max(1);
                let dh = (ih * size / long).max(1);
                let dx = x + (size - dw) / 2;
                let dy = y + (size - dh) / 2;
                let bih = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: iw,
                    biHeight: -ih, // トップダウン
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                };
                unsafe {
                    SetStretchBltMode(dc.ptr(), STRETCH_HALFTONE);
                    StretchDIBits(
                        dc.ptr(),
                        dx,
                        dy,
                        dw,
                        dh,
                        0,
                        0,
                        iw,
                        ih,
                        bgra.as_ptr() as *const c_void,
                        &bih,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    );
                }
            }
        }
        true
    }

    /// 実FSファイルの per-file アイコン/サムネ取得を依頼する（未取得・未解決のときのみ）。
    /// `size` は描画する物理 px で、大きければ jumbo として取得する。標準アイコンを既に持って
    /// いても、大表示で jumbo 未取得なら取り直す（モード切替で上げ直す）。
    pub fn request_file(&self, path: &std::path::Path, mtime: u64, thumb: bool, size: i32) {
        let want_jumbo = !thumb && size >= JUMBO_MIN_PX;
        if let Some(entry) = self.per_file.borrow().get(path)
            && entry.mtime == mtime
            && (!want_jumbo || entry.jumbo)
        {
            // 解決済みで、要求する解像度クラス（標準/jumbo）も満たしている。
            entry.last_used.set(self.next_tick());
            return;
        }
        if self.pending.borrow().contains(path) {
            return;
        }
        let tx = self.req_tx.borrow();
        let Some(tx) = tx.as_ref() else {
            return;
        };
        if tx
            .send(Request { path: path.to_path_buf(), mtime, thumb, jumbo: want_jumbo })
            .is_ok()
        {
            self.pending.borrow_mut().insert(path.to_path_buf());
        }
    }

    /// ワーカからの結果を取り込みキャッシュへ反映する。何か取り込んだら true（再描画の合図）。
    pub fn drain_results(&self) -> bool {
        let rx = self.res_rx.borrow();
        let Some(rx) = rx.as_ref() else {
            return false;
        };
        let mut any = false;
        while let Ok(res) = rx.try_recv() {
            any = true;
            self.pending.borrow_mut().remove(&res.path);
            let drawable = res.payload.map(|p| match p {
                WorkerDrawable::Icon(si) => Drawable::Icon(OwnedIcon(si.0)),
                WorkerDrawable::Thumb { w, h, bgra } => Drawable::Thumb { w, h, bgra },
            });
            let last_used = Cell::new(self.next_tick());
            self.per_file.borrow_mut().insert(
                res.path,
                FileEntry { mtime: res.mtime, drawable, jumbo: res.jumbo, last_used },
            );
        }
        if any {
            self.enforce_cap();
        }
        any
    }

    /// LRU 用カウンタを 1 つ進めて返す。
    fn next_tick(&self) -> u64 {
        let t = self.tick.get().wrapping_add(1);
        self.tick.set(t);
        t
    }

    /// per-file キャッシュが上限を超えていたら、使用の古い順に低水位（上限の 7/8）まで落とす。
    /// エントリの破棄で HICON（`OwnedIcon` の Drop）やサムネイルのバッファを解放する。
    fn enforce_cap(&self) {
        let mut map = self.per_file.borrow_mut();
        if map.len() <= PER_FILE_CAP {
            return;
        }
        let target = PER_FILE_CAP * 7 / 8;
        let remove = map.len() - target;
        let mut by_age: Vec<(u64, PathBuf)> =
            map.iter().map(|(p, e)| (e.last_used.get(), p.clone())).collect();
        by_age.sort_unstable_by_key(|(t, _)| *t);
        for (_, p) in by_age.into_iter().take(remove) {
            map.remove(&p);
        }
    }
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{FileEntry, IconBox, IconCache, PER_FILE_CAP};
    use std::cell::Cell;
    use std::path::PathBuf;

    #[test]
    fn per_file_cache_evicts_to_cap_keeping_recent() {
        let cache = IconCache::new();
        let n = PER_FILE_CAP + 100;
        {
            let mut map = cache.per_file.borrow_mut();
            for i in 0..n {
                map.insert(
                    PathBuf::from(format!("f{i}")),
                    FileEntry { mtime: 0, drawable: None, jumbo: false, last_used: Cell::new(i as u64) },
                );
            }
        }
        cache.enforce_cap();
        let map = cache.per_file.borrow();
        assert!(map.len() <= PER_FILE_CAP, "上限以下に収まる: {}", map.len());
        // 最も新しい（last_used 大）エントリは残り、最も古いものは落ちる。
        assert!(map.contains_key(&PathBuf::from(format!("f{}", n - 1))), "最新は残る");
        assert!(!map.contains_key(&PathBuf::from("f0")), "最古は落ちる");
    }

    #[test]
    fn trim_transparent_crops_to_opaque_bounds() {
        // 4x4 のうち (1,1)-(2,2) の 2x2 だけ不透明（BGRA・アルファ 255）。
        let (w, h) = (4u32, 4u32);
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for &(x, y) in &[(1u32, 1u32), (2, 1), (1, 2), (2, 2)] {
            let i = ((y * w + x) * 4) as usize;
            bgra[i..i + 4].copy_from_slice(&[10, 20, 30, 255]);
        }
        let (tw, th, out) = super::trim_transparent(w, h, &bgra).unwrap();
        assert_eq!((tw, th), (2, 2), "不透明領域の外接矩形へ切り出す");
        assert_eq!(out.len(), 2 * 2 * 4);
        assert_eq!(&out[0..4], &[10, 20, 30, 255], "左上が元の不透明画素");
    }

    #[test]
    fn trim_transparent_none_when_fully_transparent() {
        let bgra = vec![0u8; 4 * 4 * 4];
        assert!(super::trim_transparent(4, 4, &bgra).is_none(), "全透明は None");
    }

    #[test]
    fn center_capped_fills_frame_when_cap_not_smaller() {
        // cap が枠以上なら枠いっぱい・オフセット 0（通常表示と同じ振る舞い）。
        assert_eq!(IconBox { x: 10, y: 20, size: 32, cap: 32 }.center_capped(), (10, 20, 32));
        assert_eq!(IconBox { x: 10, y: 20, size: 32, cap: 64 }.center_capped(), (10, 20, 32));
    }

    #[test]
    fn center_capped_centers_when_cap_smaller() {
        // cap が枠より小さいと cap 角を枠の中央へ（サムネイル表示でシェルアイコンを原寸寄りに）。
        assert_eq!(IconBox { x: 0, y: 0, size: 128, cap: 32 }.center_capped(), (48, 48, 32));
        assert_eq!(IconBox { x: 100, y: 200, size: 128, cap: 32 }.center_capped(), (148, 248, 32));
    }

    #[test]
    fn center_capped_guards_nonpositive_cap() {
        // cap が 0 以下でも一辺は 1 px 以上を保つ。
        let (_, _, s) = IconBox { x: 0, y: 0, size: 128, cap: 0 }.center_capped();
        assert_eq!(s, 1);
    }
}
