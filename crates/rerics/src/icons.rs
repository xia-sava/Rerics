//! ファイル一覧のシェルアイコン取得・キャッシュ・描画。プラットフォーム依存（Win32 シェル
//! API）なので core でなく GUI 側に置く。
//!
//! 第一段は同期取得＋拡張子単位キャッシュ：フォルダは既定フォルダアイコン、ファイルは
//! 関連付けの種別アイコン（登録の無い拡張子はシステムが返す汎用「紙」アイコン）。実ファイルを
//! 触らず `SHGFI_USEFILEATTRIBUTES` で拡張子から引くので、書庫内エントリにも使える。
//! exe 等の固有アイコン・画像サムネイル・オーバーレイは後段で非同期に載せる。

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;

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
}

const SHGFI_ICON: u32 = 0x0000_0100;
const SHGFI_SMALLICON: u32 = 0x0000_0001;
const SHGFI_USEFILEATTRIBUTES: u32 = 0x0000_0010;
const SHGFI_ADDOVERLAYS: u32 = 0x0000_0020;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

const DI_NORMAL: u32 = 0x0003;

/// アイコンの論理サイズ（小アイコン）。描画時に DPI スケールする。
pub const ICON_LOGICAL: i32 = 16;

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

/// 与えた疑似パス・属性からシステムアイコン（HICON）を1つ取得する。失敗で None。
/// `add_overlays` が真なら（実ファイルに対して）オーバーレイ合成済みアイコンを得る。
fn fetch_icon(pseudo: &str, attrs: u32, use_attrs: bool, add_overlays: bool) -> Option<OwnedIcon> {
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
    if rc != 0 && !info.hIcon.is_null() {
        Some(OwnedIcon(info.hIcon))
    } else {
        None
    }
}

/// 拡張子・フォルダ単位のアイコンキャッシュ（同期取得）。UI スレッドで使う。
pub struct IconCache {
    /// キー: "<dir>" / 拡張子（小文字・先頭ドット込み・無拡張子は "<none>"）。
    generic: RefCell<HashMap<String, Option<OwnedIcon>>>,
}

impl IconCache {
    pub fn new() -> Self {
        Self { generic: RefCell::new(HashMap::new()) }
    }

    /// 汎用アイコン（ディレクトリ＝既定フォルダ／拡張子別の種別アイコン）の HICON を返す。
    /// 取得失敗・該当無しは None。返り値はキャッシュ所有なので破棄しないこと。
    fn generic_handle(&self, is_dir: bool, ext: &str) -> *mut c_void {
        let key = if is_dir {
            "<dir>".to_owned()
        } else if ext.is_empty() {
            "<none>".to_owned()
        } else {
            ext.to_ascii_lowercase()
        };
        let mut map = self.generic.borrow_mut();
        let entry = map.entry(key).or_insert_with(|| {
            if is_dir {
                fetch_icon("folder", FILE_ATTRIBUTE_DIRECTORY, true, false)
            } else {
                // 拡張子のみ意味を持つ疑似名。登録の無い拡張子はシステムが汎用紙アイコンを返す。
                let pseudo = format!("x{}", ext);
                fetch_icon(&pseudo, FILE_ATTRIBUTE_NORMAL, true, false)
            }
        });
        entry.as_ref().map(|i| i.0).unwrap_or(std::ptr::null_mut())
    }

    /// 指定アイテムのアイコンを `dc` の (x,y) に `size` px 角で描画する。アイコンが無ければ何も
    /// しない。`size` は物理 px（DPI スケール済みを渡す）。
    pub fn draw(&self, dc: &w::HDC, is_dir: bool, ext: &str, x: i32, y: i32, size: i32) {
        let h = self.generic_handle(is_dir, ext);
        if h.is_null() {
            return;
        }
        unsafe {
            DrawIconEx(dc.ptr(), x, y, h, size, size, 0, std::ptr::null_mut(), DI_NORMAL);
        }
    }
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}
