//! シェル連携（Win32 シェル API を叩く操作）。プラットフォーム依存なので core でなく
//! GUI 側に置く。第一弾はゴミ箱送り削除（`SHFileOperationW`＋FOF_ALLOWUNDO）。

use std::ffi::c_void;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use winsafe::{self as w, co};

// シェルのファイル操作（IFileOperation）用。シグネチャで使う型だけ module 直下に置き、
// 残りは各関数内で import する（ファイル既存の流儀に合わせる）。
use windows::Win32::UI::Shell::{IFileOperation, IShellItem};

// Win32 構造体の名前をそのまま写した FFI 宣言（命名は Win32 準拠）。
#[allow(non_snake_case, clippy::upper_case_acronyms)]
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
            owner.ptr(),
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

/// CF_UNICODETEXT 用のバイト列を組む（絶対パスを CRLF 区切り・null 終端の UTF-16LE）。
/// CF_HDROP と併載し、ターミナルやエディタへ貼るとフルパス文字列として取り出せるようにする。
fn build_path_text(paths: &[PathBuf]) -> Vec<u8> {
    let joined = paths
        .iter()
        .map(|p| absolute(p).to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\r\n");
    let mut u16s: Vec<u16> = joined.encode_utf16().collect();
    u16s.push(0);
    u16s.iter().flat_map(|u| u.to_le_bytes()).collect()
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

/// クリップボードを開く。クリップボードはプロセス横断の単一所有資源で、他プロセス
/// （クリップボード履歴ツール等）が一瞬掴むと `OpenClipboard` が失敗する。そうした
/// 一過性の競合を吸収するため、短い間隔で数回リトライしてから諦める。全リトライが
/// 失敗したら、呼び出し側がユーザーへ明示できるようエラー文字列を返す（黙って空振りしない）。
fn open_clipboard_retry(owner: &w::HWND) -> Result<w::guard::CloseClipboardGuard<'_>, String> {
    const ATTEMPTS: u32 = 8;
    const INTERVAL: Duration = Duration::from_millis(25);
    let mut last = String::new();
    for attempt in 0..ATTEMPTS {
        match owner.OpenClipboard() {
            Ok(clip) => return Ok(clip),
            Err(e) => {
                last = e.to_string();
                if attempt + 1 < ATTEMPTS {
                    std::thread::sleep(INTERVAL);
                }
            }
        }
    }
    Err(format!(
        "クリップボードを開けませんでした（他のアプリが使用中の可能性）: {last}"
    ))
}

/// 選択ファイル群をクリップボードへ（`move_it` で切り取り＝移動指定）。
pub fn clip_copy_files(owner: &w::HWND, paths: &[PathBuf], move_it: bool) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let hdrop = build_hdrop(paths);
    let path_text = build_path_text(paths);
    let effect = if move_it { DROPEFFECT_MOVE } else { DROPEFFECT_COPY };
    let effect_fmt = preferred_drop_effect_format();
    let clip = open_clipboard_retry(owner)?;
    clip.EmptyClipboard().map_err(|e| e.to_string())?;
    clip.SetClipboardData(co::CF::HDROP, &hdrop).map_err(|e| e.to_string())?;
    // フルパスをテキストでも載せる＝Explorer へはファイル、ターミナル/エディタへは
    // パス文字列として貼れる。貼り付け側が扱える形式を選ぶ。
    clip.SetClipboardData(co::CF::UNICODETEXT, &path_text).map_err(|e| e.to_string())?;
    if effect_fmt != 0 {
        let fmt = unsafe { co::CF::from_raw(effect_fmt) };
        clip.SetClipboardData(fmt, &effect.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// クリップボードのファイル一覧と「移動指定か」を取り出す。クリップボードを開けない等の
/// システム的失敗は `Err`（呼び出し側がエラー表示する）。ファイル形式のデータが無い
/// （貼れるものが無い）場合は空 Vec を `Ok` で返す＝正常な「貼るもの無し」と区別する。
pub fn clip_paste_files(owner: &w::HWND) -> Result<(Vec<PathBuf>, bool), String> {
    let clip = open_clipboard_retry(owner)?;
    let bytes = match clip.GetClipboardData(co::CF::HDROP) {
        Ok(bytes) => bytes,
        // HDROP（ファイル）形式が無い＝テキストのみ/空など。システム失敗ではないので空を返す。
        Err(_) => return Ok((Vec::new(), false)),
    };
    let paths = parse_hdrop(&bytes);
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

/// テキストをクリップボードへ設定する（CF_UNICODETEXT・null 終端付き UTF-16）。
pub fn clip_set_text(owner: &w::HWND, text: &str) -> Result<(), String> {
    let mut u16s: Vec<u16> = text.encode_utf16().collect();
    u16s.push(0);
    let bytes: Vec<u8> = u16s.iter().flat_map(|u| u.to_le_bytes()).collect();
    let clip = open_clipboard_retry(owner)?;
    clip.EmptyClipboard().map_err(|e| e.to_string())?;
    clip.SetClipboardData(co::CF::UNICODETEXT, &bytes).map_err(|e| e.to_string())?;
    Ok(())
}

/// クリップボードのテキストを取り出す（CF_UNICODETEXT）。クリップボードを開けない等の
/// システム的失敗は `Err`。テキスト形式のデータが無い場合は `None` を `Ok` で返す
/// ＝正常な「テキスト無し」とシステム失敗を区別する。
pub fn clip_get_text(owner: &w::HWND) -> Result<Option<String>, String> {
    let clip = open_clipboard_retry(owner)?;
    let bytes = match clip.GetClipboardData(co::CF::UNICODETEXT) {
        Ok(bytes) => bytes,
        // UNICODETEXT 形式が無い＝ファイルのみ/空など。システム失敗ではないので None。
        Err(_) => return Ok(None),
    };
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    Ok(Some(String::from_utf16_lossy(&units)))
}

/// 画像ファイル `path` を読み込み、クリップボードへ画像（CF_DIB）として設定する。透過は
/// 失われる（24bpp BGR・ボトムアップで格納）。デコードやクリップボード操作の失敗は `Err`。
pub fn clip_set_image(owner: &w::HWND, path: &Path) -> Result<(), String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgb8();
    let (width, height) = (img.width() as usize, img.height() as usize);
    if width == 0 || height == 0 {
        return Err("画像のサイズが不正です".to_string());
    }
    // CF_DIB は BITMAPINFOHEADER＋ボトムアップの BGR ピクセル列。各行を 4 バイト境界へ詰める。
    let stride = (width * 3 + 3) & !3;
    let mut dib = Vec::with_capacity(40 + stride * height);
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    dib.extend_from_slice(&(height as i32).to_le_bytes()); // biHeight（正＝ボトムアップ）
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    dib.extend_from_slice(&((stride * height) as u32).to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
    for y in (0..height).rev() {
        let row_start = dib.len();
        for x in 0..width {
            let p = img.get_pixel(x as u32, y as u32);
            dib.push(p[2]); // B
            dib.push(p[1]); // G
            dib.push(p[0]); // R
        }
        dib.resize(row_start + stride, 0);
    }
    let clip = open_clipboard_retry(owner)?;
    clip.EmptyClipboard().map_err(|e| e.to_string())?;
    clip.SetClipboardData(co::CF::DIB, &dib).map_err(|e| e.to_string())?;
    Ok(())
}

/// クリップボードの画像（CF_DIB）を取り出し、`dest`（拡張子で形式を決める）へ保存する。
/// 画像形式が無ければ `Ok(false)`。クリップボードを開けない・保存に失敗等のシステム的失敗は `Err`。
pub fn clip_get_image(owner: &w::HWND, dest: &Path) -> Result<bool, String> {
    let dib: Vec<u8> = {
        let clip = open_clipboard_retry(owner)?;
        match clip.GetClipboardData(co::CF::DIB) {
            Ok(bytes) => bytes.to_vec(),
            // CF_DIB 形式が無い＝画像はクリップボードに無い。システム失敗ではないので false。
            Err(_) => return Ok(false),
        }
    };
    if dib.len() < 40 {
        return Err("クリップボードの画像データが不正です".to_string());
    }
    // BITMAPINFOHEADER の手前に 14 バイトの BITMAPFILEHEADER を被せて BMP として復元し、
    // image でデコードする（DIB の各種変種は BMP デコーダに任せる）。ピクセル先頭の位置は
    // ヘッダ＋色マスク（BI_BITFIELDS）＋パレットのバイト数から求める。
    let header_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]) as usize;
    let bit_count = u16::from_le_bytes([dib[14], dib[15]]) as usize;
    let compression = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
    let clr_used = u32::from_le_bytes([dib[32], dib[33], dib[34], dib[35]]) as usize;
    let mask_bytes = if compression == 3 && header_size == 40 { 12 } else { 0 };
    let palette_entries = if bit_count <= 8 {
        if clr_used != 0 { clr_used } else { 1usize << bit_count }
    } else {
        clr_used
    };
    let pixel_offset = 14 + header_size + mask_bytes + palette_entries * 4;
    let file_size = 14 + dib.len();
    let mut bmp = Vec::with_capacity(file_size);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
    bmp.extend_from_slice(&0u16.to_le_bytes()); // bfReserved1
    bmp.extend_from_slice(&0u16.to_le_bytes()); // bfReserved2
    bmp.extend_from_slice(&(pixel_offset as u32).to_le_bytes()); // bfOffBits
    bmp.extend_from_slice(&dib);
    let img = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp)
        .map_err(|e| e.to_string())?;
    img.save(dest).map_err(|e| e.to_string())?;
    Ok(true)
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

/// ファイル選択ダイアログ（COM `IFileOpenDialog`/`IFileSaveDialog`）を開き、選んだパスを返す。
/// `save` が真なら保存ダイアログ。キャンセル/失敗は `None`。`owner` は親窓の生ハンドル。
pub fn choose_file(owner: *mut c_void, title: &str, save: bool) -> Option<PathBuf> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog,
        SIGDN_FILESYSPATH,
    };
    use windows::core::{Interface, PCWSTR};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dlg: IFileDialog = if save {
            CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)
                .ok()?
                .cast()
                .ok()?
        } else {
            CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
                .ok()?
                .cast()
                .ok()?
        };
        if !title.is_empty() {
            let t: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = dlg.SetTitle(PCWSTR(t.as_ptr()));
        }
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

/// `.lnk` ショートカットのリンク先パスを解決して返す（取得できなければ `None`）。
/// リンク先が存在するかは検証しない（呼び側が用途に応じて判断する）。
pub fn resolve_shortcut(lnk: &Path) -> Option<PathBuf> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
    use windows::core::{Interface, PCWSTR};

    let lnk_w = wide(lnk);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        persist.Load(PCWSTR(lnk_w.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; 260];
        link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
            .ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if len == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

/// シェル（`IFileOperation`）操作の共通処理。COM を初期化して `IFileOperation` を作り、`queue`
/// で対象を積み、`PerformOperations` で実行する。Explorer 純正の進捗・衝突・確認ダイアログが出て、
/// 完了（または中止）までブロックする。中止されず完了で `Ok(true)`、ユーザー中止で `Ok(false)`、
/// システム失敗は `Err`。`owner` を親に据えるのでダイアログがアプリ窓に従属する。
fn run_file_op(
    owner: &w::HWND,
    queue: impl FnOnce(&IFileOperation) -> Result<(), String>,
) -> Result<bool, String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::FileOperation;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let op: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        op.SetOwnerWindow(HWND(owner.ptr())).map_err(|e| e.to_string())?;
        queue(&op)?;
        op.PerformOperations().map_err(|e| e.to_string())?;
        Ok(!op.GetAnyOperationsAborted().map_err(|e| e.to_string())?.as_bool())
    }
}

/// パスから `IShellItem` を作る（`IFileOperation` の対象指定用）。
fn shell_item(path: &Path) -> Result<IShellItem, String> {
    use windows::Win32::UI::Shell::SHCreateItemFromParsingName;
    use windows::core::PCWSTR;
    let p = wide(path);
    unsafe { SHCreateItemFromParsingName(PCWSTR(p.as_ptr()), None) }.map_err(|e| e.to_string())
}

/// 選択項目を `dst_dir` 直下へシェルコピーする（Explorer の進捗・衝突ダイアログつき）。
pub fn shell_copy(owner: &w::HWND, items: &[PathBuf], dst_dir: &Path) -> Result<bool, String> {
    use windows::core::PCWSTR;
    if items.is_empty() {
        return Ok(true);
    }
    run_file_op(owner, |op| {
        let dest = shell_item(dst_dir)?;
        for it in items {
            let psi = shell_item(it)?;
            unsafe { op.CopyItem(&psi, &dest, PCWSTR::null(), None) }.map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}

/// 選択項目を `dst_dir` 直下へシェル移動する（Explorer の進捗・衝突ダイアログつき）。
pub fn shell_move(owner: &w::HWND, items: &[PathBuf], dst_dir: &Path) -> Result<bool, String> {
    use windows::core::PCWSTR;
    if items.is_empty() {
        return Ok(true);
    }
    run_file_op(owner, |op| {
        let dest = shell_item(dst_dir)?;
        for it in items {
            let psi = shell_item(it)?;
            unsafe { op.MoveItem(&psi, &dest, PCWSTR::null(), None) }.map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}

/// 選択項目をシェル削除する（フラグ既定＝完全削除・Explorer の確認/進捗ダイアログつき）。
pub fn shell_delete(owner: &w::HWND, items: &[PathBuf]) -> Result<bool, String> {
    if items.is_empty() {
        return Ok(true);
    }
    run_file_op(owner, |op| {
        for it in items {
            let psi = shell_item(it)?;
            unsafe { op.DeleteItem(&psi, None) }.map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}

/// 1 項目をシェル改名する（同名衝突は Explorer 純正ダイアログが出る）。
pub fn shell_rename(owner: &w::HWND, item: &Path, new_name: &str) -> Result<bool, String> {
    use windows::core::PCWSTR;
    let name: Vec<u16> = new_name.encode_utf16().chain(std::iter::once(0)).collect();
    run_file_op(owner, |op| {
        let psi = shell_item(item)?;
        unsafe { op.RenameItem(&psi, PCWSTR(name.as_ptr()), None) }.map_err(|e| e.to_string())?;
        Ok(())
    })
}

/// `paths`（同一ディレクトリ内の実在ファイルの絶対パス）に対し、シェルのコンテキスト
/// メニュー（エクスプローラの右クリックメニュー）をマウス位置に表示し、選ばれた項目を
/// 実行する。メニューが閉じるまでブロックする。
pub fn show_context_menu(owner: &w::HWND, paths: &[PathBuf]) -> Result<(), String> {
    use windows::Win32::Foundation::{HANDLE, HWND, POINT};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        CMINVOKECOMMANDINFO, IContextMenu, IShellFolder, SHBindToParent, SHParseDisplayName,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreatePopupMenu, DestroyMenu, GetCursorPos, SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD,
        TPM_RIGHTBUTTON, TrackPopupMenuEx,
    };
    use windows::core::{PCSTR, PCWSTR};

    // メニュー項目に割り当てる ID の範囲（先頭・上限）と、標準のメニュー内容フラグ。
    const ID_CMD_FIRST: u32 = 1;
    const ID_CMD_LAST: u32 = 0x7fff;
    const CMF_NORMAL: u32 = 0;

    if paths.is_empty() {
        return Ok(());
    }
    let hwnd = HWND(owner.ptr());

    // 各パスを絶対 PIDL に解決する（解決できないものは飛ばす）。
    let mut pidls: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
    for p in paths {
        let name = wide(p);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let parsed = unsafe { SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut pidl, 0, None) };
        if parsed.is_ok() && !pidl.is_null() {
            pidls.push(pidl);
        }
    }

    let result: Result<(), String> = (|| unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        // 親フォルダ（最初に取れたもの）と、各項目の子 PIDL を集める。
        let mut folder: Option<IShellFolder> = None;
        let mut children: Vec<*const ITEMIDLIST> = Vec::with_capacity(pidls.len());
        for &full in &pidls {
            let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
            let psf: IShellFolder =
                match SHBindToParent(full, Some(&mut child as *mut *mut ITEMIDLIST)) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
            if folder.is_none() {
                folder = Some(psf);
            }
            if !child.is_null() {
                children.push(child as *const ITEMIDLIST);
            }
        }
        let (Some(folder), false) = (folder, children.is_empty()) else {
            return Err("コンテキストメニューの対象を取得できません".to_string());
        };

        let menu: IContextMenu = folder
            .GetUIObjectOf(hwnd, &children, None)
            .map_err(|e| e.to_string())?;
        let hmenu = CreatePopupMenu().map_err(|e| e.to_string())?;
        let _ = menu.QueryContextMenu(hmenu, 0, ID_CMD_FIRST, ID_CMD_LAST, CMF_NORMAL);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let flags = (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN).0;
        let chosen = TrackPopupMenuEx(hmenu, flags, pt.x, pt.y, hwnd, None).0;
        let _ = DestroyMenu(hmenu);

        if chosen > 0 {
            let info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                fMask: 0,
                hwnd,
                lpVerb: PCSTR((chosen as u32 - ID_CMD_FIRST) as usize as *const u8),
                lpParameters: PCSTR::null(),
                lpDirectory: PCSTR::null(),
                nShow: SW_SHOWNORMAL.0,
                dwHotKey: 0,
                hIcon: HANDLE::default(),
            };
            menu.InvokeCommand(&info).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();

    for &pidl in &pidls {
        unsafe { CoTaskMemFree(Some(pidl as *const c_void)) };
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// null 終端までの UTF-16LE バイト列を文字列へ戻す（テスト検証用）。
    fn decode_utf16le(bytes: &[u8]) -> String {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        String::from_utf16(&units[..end]).unwrap()
    }

    #[test]
    fn build_path_text_joins_absolute_paths_with_crlf() {
        let paths = [
            PathBuf::from(r"C:\dir\a.txt"),
            PathBuf::from(r"C:\dir\b c.txt"),
        ];
        let bytes = build_path_text(&paths);
        assert_eq!(&bytes[bytes.len() - 2..], &[0, 0], "null 終端で終わる");
        assert_eq!(
            decode_utf16le(&bytes),
            "C:\\dir\\a.txt\r\nC:\\dir\\b c.txt"
        );
    }

    #[test]
    fn build_path_text_single_path_has_no_separator() {
        let bytes = build_path_text(&[PathBuf::from(r"C:\x\only.png")]);
        assert_eq!(decode_utf16le(&bytes), "C:\\x\\only.png");
    }
}
