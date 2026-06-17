//! シェル連携（Win32 シェル API を叩く操作）。プラットフォーム依存なので core でなく
//! GUI 側に置く。第一弾はゴミ箱送り削除（`SHFileOperationW`＋FOF_ALLOWUNDO）。

use std::ffi::c_void;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use winsafe::{self as w, co};

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

/// null 終端の UTF-16 列を作る（Win32 文字列引数用）。
fn wide(p: &Path) -> Vec<u16> {
    absolute(p).as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClipboardFormatW(lpsz: *const u16) -> u32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHObjectProperties(hwnd: *mut c_void, dwType: u32, szObject: *const u16, szPage: *const u16)
        -> i32;
}

const SHOP_FILEPATH: u32 = 0x0000_0002;

/// シェルのプロパティシート（モードレス）を表示する。
pub fn show_properties(owner: &w::HWND, path: &Path) -> Result<(), String> {
    let obj = wide(path);
    let ok = unsafe {
        SHObjectProperties(
            owner.ptr() as *mut c_void,
            SHOP_FILEPATH,
            obj.as_ptr(),
            std::ptr::null(),
        )
    };
    if ok != 0 {
        Ok(())
    } else {
        Err("プロパティを表示できません".to_string())
    }
}

/// 設定されたエディタで `file` を開く（外部プロセス起動・非ブロッキング）。
pub fn launch_editor(editor: &str, file: &Path) -> Result<(), String> {
    std::process::Command::new(editor)
        .arg(file)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{editor} を起動できません: {e}"))
}

const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_MOVE: u32 = 2;

/// 「Preferred DropEffect」登録クリップボード形式の ID（コピー/移動の区別用）。
fn preferred_drop_effect_format() -> u16 {
    let name: Vec<u16> = "Preferred DropEffect\0".encode_utf16().collect();
    unsafe { RegisterClipboardFormatW(name.as_ptr()) as u16 }
}

/// CF_HDROP のバイト列を組む（DROPFILES 20 バイト＋二重 null 終端の UTF-16 パス列）。
fn build_hdrop(paths: &[PathBuf]) -> Vec<u8> {
    let mut wide_list: Vec<u16> = Vec::new();
    for p in paths {
        wide_list.extend(absolute(p).as_os_str().encode_wide());
        wide_list.push(0);
    }
    wide_list.push(0);
    let mut buf = Vec::new();
    buf.extend_from_slice(&20u32.to_le_bytes()); // pFiles＝リスト開始オフセット
    buf.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    buf.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    buf.extend_from_slice(&0i32.to_le_bytes()); // fNC
    buf.extend_from_slice(&1i32.to_le_bytes()); // fWide=TRUE
    for u in wide_list {
        buf.extend_from_slice(&u.to_le_bytes());
    }
    buf
}

/// CF_HDROP のバイト列からパス一覧を取り出す（fWide 前提）。
fn parse_hdrop(bytes: &[u8]) -> Vec<PathBuf> {
    if bytes.len() < 20 {
        return Vec::new();
    }
    let p_files = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if p_files >= bytes.len() {
        return Vec::new();
    }
    let units: Vec<u16> = bytes[p_files..]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mut paths = Vec::new();
    let mut cur: Vec<u16> = Vec::new();
    for &u in &units {
        if u == 0 {
            if cur.is_empty() {
                break; // 二重 null＝終端
            }
            paths.push(PathBuf::from(std::ffi::OsString::from_wide(&cur)));
            cur.clear();
        } else {
            cur.push(u);
        }
    }
    paths
}

/// 選択ファイル群をクリップボードへ（`move_it` で切り取り＝移動指定）。
pub fn clip_copy_files(owner: &w::HWND, paths: &[PathBuf], move_it: bool) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let hdrop = build_hdrop(paths);
    let effect = if move_it { DROPEFFECT_MOVE } else { DROPEFFECT_COPY };
    let effect_fmt = preferred_drop_effect_format();
    let clip = owner.OpenClipboard().map_err(|e| e.to_string())?;
    clip.EmptyClipboard().map_err(|e| e.to_string())?;
    clip.SetClipboardData(co::CF::HDROP, &hdrop).map_err(|e| e.to_string())?;
    if effect_fmt != 0 {
        let fmt = unsafe { co::CF::from_raw(effect_fmt) };
        clip.SetClipboardData(fmt, &effect.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// クリップボードのファイル一覧と「移動指定か」を取り出す。ファイルが無ければ Err。
pub fn clip_paste_files(owner: &w::HWND) -> Result<(Vec<PathBuf>, bool), String> {
    let clip = owner.OpenClipboard().map_err(|e| e.to_string())?;
    let bytes = clip.GetClipboardData(co::CF::HDROP).map_err(|e| e.to_string())?;
    let paths = parse_hdrop(&bytes);
    if paths.is_empty() {
        return Err("クリップボードにファイルがありません".to_string());
    }
    let effect_fmt = preferred_drop_effect_format();
    let move_it = if effect_fmt != 0 {
        let fmt = unsafe { co::CF::from_raw(effect_fmt) };
        clip.GetClipboardData(fmt)
            .ok()
            .filter(|b| b.len() >= 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) == DROPEFFECT_MOVE)
            .unwrap_or(false)
    } else {
        false
    };
    Ok((paths, move_it))
}

/// フォルダ選択ダイアログ（COM `IFileOpenDialog`＋`FOS_PICKFOLDERS`）を開き、選んだパスを返す。
/// キャンセル/失敗は `None`。`owner` はモーダルの親窓の生ハンドル。
pub fn choose_folder(owner: *mut c_void, title: &str) -> Option<PathBuf> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree,
    };
    use windows::Win32::UI::Shell::{
        FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
    };
    use windows::core::PCWSTR;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dlg: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS).ok()?;
        if !title.is_empty() {
            let t: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = dlg.SetTitle(PCWSTR(t.as_ptr()));
        }
        // キャンセルは Err（ERROR_CANCELLED）→ None。
        dlg.Show(Some(HWND(owner))).ok()?;
        let item = dlg.GetResult().ok()?;
        let pw = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = pw.to_string().ok();
        CoTaskMemFree(Some(pw.0 as *const c_void));
        s.map(PathBuf::from)
    }
}

/// `target` を指すショートカット（.lnk）を `lnk` に作る（COM `IShellLink`）。
pub fn create_shortcut(target: &Path, lnk: &Path) -> Result<(), String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{Interface, PCWSTR};

    let target_w = wide(target);
    let lnk_w = wide(lnk);
    let dir_w = target.parent().map(wide);

    unsafe {
        // 既に初期化済みでも害はない（戻り値は無視する）。
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| e.to_string())?;
        link.SetPath(PCWSTR(target_w.as_ptr())).map_err(|e| e.to_string())?;
        if let Some(d) = &dir_w {
            let _ = link.SetWorkingDirectory(PCWSTR(d.as_ptr()));
        }
        let persist: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        persist
            .Save(PCWSTR(lnk_w.as_ptr()), true)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
