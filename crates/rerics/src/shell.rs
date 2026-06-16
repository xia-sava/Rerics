//! シェル連携（Win32 シェル API を叩く操作）。プラットフォーム依存なので core でなく
//! GUI 側に置く。第一弾はゴミ箱送り削除（`SHFileOperationW`＋FOF_ALLOWUNDO）。

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

#[allow(non_snake_case)]
#[repr(C)]
struct SHFILEOPSTRUCTW {
    hwnd: *mut c_void,
    wFunc: u32,
    pFrom: *const u16,
    pTo: *const u16,
    fFlags: u16,
    fAnyOperationsAborted: i32,
    hNameMappings: *mut c_void,
    lpszProgressTitle: *const u16,
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHFileOperationW(lpFileOp: *mut SHFILEOPSTRUCTW) -> i32;
}

const FO_DELETE: u32 = 0x0003;
const FOF_SILENT: u16 = 0x0004;
const FOF_NOCONFIRMATION: u16 = 0x0010;
const FOF_ALLOWUNDO: u16 = 0x0040;
const FOF_NOERRORUI: u16 = 0x0400;

/// 指定の絶対パス群をゴミ箱へ送る（`FOF_ALLOWUNDO`）。確認・エラー UI はアプリ側で
/// 担うため抑止する。成功で `Ok(())`、失敗でメッセージ。
pub fn send_to_recycle(paths: &[std::path::PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    // `pFrom` は「各パスを null 終端し、末尾にもう1つ null」の二重 null 終端 UTF-16 列。
    let mut buf: Vec<u16> = Vec::new();
    for p in paths {
        buf.extend(absolute(p).as_os_str().encode_wide());
        buf.push(0);
    }
    buf.push(0);

    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: buf.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        Err(format!("SHFileOperation エラー (0x{rc:X})"))
    } else if op.fAnyOperationsAborted != 0 {
        Err("中断されました".to_string())
    } else {
        Ok(())
    }
}

/// `SHFileOperation` は相対パスを CWD 基準で解釈するので絶対化しておく。
fn absolute(p: &Path) -> std::path::PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}
