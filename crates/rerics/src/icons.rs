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
//!   取得できるまでは汎用アイコンを表示。完了で main 窓へ `WM_ICONS_READY` を Post して再描画。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use winsafe::{self as w};

#[allow(non_snake_case)]
#[repr(C)]
struct SHFILEINFOW {
    hIcon: *mut c_void,
    iIcon: i32,
    dwAttributes: u32,
    szDisplayName: [u16; 260],
    szTypeName: [u16; 80],
}

#[allow(non_snake_case)]
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
    fn PostMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn SetDIBitsToDevice(
        hdc: *mut c_void,
        xDest: i32,
        yDest: i32,
        w: u32,
        h: u32,
        xSrc: i32,
        ySrc: i32,
        StartScan: u32,
        cLines: u32,
        lpvBits: *const c_void,
        lpbmi: *const BITMAPINFOHEADER,
        ColorUse: u32,
    ) -> i32;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(pvReserved: *mut c_void, dwCoInit: u32) -> i32;
}

const SHGFI_ICON: u32 = 0x0000_0100;
const SHGFI_SMALLICON: u32 = 0x0000_0001;
const SHGFI_USEFILEATTRIBUTES: u32 = 0x0000_0010;
const SHGFI_ADDOVERLAYS: u32 = 0x0000_0020;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

const DI_NORMAL: u32 = 0x0003;
const COINIT_APARTMENTTHREADED: u32 = 0x2;
const DIB_RGB_COLORS: u32 = 0;

/// アイコンの論理サイズ（小アイコン）。描画時に DPI スケールする。
pub const ICON_LOGICAL: i32 = 16;

/// 非同期アイコン読込完了を main 窓へ通知するカスタムメッセージ（`WM_DEBUG_WAKE`=0x8001 と別）。
pub const WM_ICONS_READY: u32 = 0x8002;

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
}

struct IconResult {
    path: PathBuf,
    mtime: u64,
    payload: Option<WorkerDrawable>,
}

/// 与えた疑似パス・属性からシステムアイコン（HICON）を1つ取得する。失敗で null。
fn fetch_icon(pseudo: &str, attrs: u32, use_attrs: bool, add_overlays: bool) -> *mut c_void {
    let wide: Vec<u16> = std::ffi::OsStr::new(pseudo)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut flags = SHGFI_ICON | SHGFI_SMALLICON;
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
            let h = fetch_icon(&path_str, FILE_ATTRIBUTE_NORMAL, false, true);
            if h.is_null() { None } else { Some(WorkerDrawable::Icon(SendIcon(h))) }
        };
        if tx
            .send(IconResult { path: req.path, mtime: req.mtime, payload })
            .is_err()
        {
            break;
        }
        unsafe {
            PostMessageW(wake as *mut c_void, WM_ICONS_READY, 0, 0);
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
    let (w, h, rgba) = rerics_core::decode_thumbnail(&bytes, ICON_LOGICAL as u32 * 2)?;
    let bgra = rerics_core::rgba_to_bgra(&rgba);
    Some(WorkerDrawable::Thumb { w, h, bgra })
}

/// 拡張子・フォルダ単位の同期キャッシュ＋実FSファイルの非同期 per-file キャッシュ。UI スレッド専用。
pub struct IconCache {
    /// 汎用（同期）。キー: "<dir>" / 拡張子（小文字・ドット込み・無拡張子 "<none>"）。
    generic: RefCell<HashMap<String, *mut c_void>>,
    /// 汎用キャッシュが所有する HICON（Drop で破棄）。
    generic_owned: RefCell<Vec<OwnedIcon>>,
    /// per-file（非同期）。キー: 実フルパス。値: (mtime, 解決結果)。None=解決済みだが取得失敗。
    per_file: RefCell<HashMap<PathBuf, (u64, Option<Drawable>)>>,
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
            pending: RefCell::new(HashSet::new()),
            req_tx: RefCell::new(None),
            res_rx: RefCell::new(None),
        }
    }

    /// 非同期ワーカを起動する。`wake_hwnd` は完了通知（`WM_ICONS_READY`）の送り先（生 HWND）。
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

    /// 汎用アイコン（ディレクトリ／拡張子別）を (x,y) に size px 角で描く。
    pub fn draw_generic(&self, dc: &w::HDC, is_dir: bool, ext: &str, x: i32, y: i32, size: i32) {
        let h = self.generic_handle(is_dir, ext);
        if !h.is_null() {
            unsafe {
                DrawIconEx(dc.ptr(), x, y, h, size, size, 0, std::ptr::null_mut(), DI_NORMAL);
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
        x: i32,
        y: i32,
        size: i32,
    ) -> bool {
        let map = self.per_file.borrow();
        let Some((m, Some(drawable))) = map.get(path) else {
            return false;
        };
        if *m != mtime {
            return false;
        }
        match drawable {
            Drawable::Icon(icon) => unsafe {
                DrawIconEx(dc.ptr(), x, y, icon.0, size, size, 0, std::ptr::null_mut(), DI_NORMAL);
            },
            Drawable::Thumb { w, h, bgra } => {
                let (iw, ih) = (*w as i32, *h as i32);
                let dx = x + (size - iw).max(0) / 2;
                let dy = y + (size - ih).max(0) / 2;
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
                    SetDIBitsToDevice(
                        dc.ptr(),
                        dx,
                        dy,
                        *w,
                        *h,
                        0,
                        0,
                        0,
                        *h,
                        bgra.as_ptr() as *const c_void,
                        &bih,
                        DIB_RGB_COLORS,
                    );
                }
            }
        }
        true
    }

    /// 実FSファイルの per-file アイコン/サムネ取得を依頼する（未取得・未解決のときのみ）。
    pub fn request_file(&self, path: &std::path::Path, mtime: u64, thumb: bool) {
        if let Some((m, _)) = self.per_file.borrow().get(path) {
            if *m == mtime {
                return; // 解決済み（成功/失敗どちらも）。
            }
        }
        if self.pending.borrow().contains(path) {
            return;
        }
        let tx = self.req_tx.borrow();
        let Some(tx) = tx.as_ref() else {
            return;
        };
        if tx
            .send(Request { path: path.to_path_buf(), mtime, thumb })
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
            self.per_file.borrow_mut().insert(res.path, (res.mtime, drawable));
        }
        any
    }
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}
