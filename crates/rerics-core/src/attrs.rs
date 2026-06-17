//! ファイル属性（読取専用/隠し/システム/アーカイブ）と更新日時の読み書き。
//! 名前変更ダイアログの属性・日時変更で使う。
//!
//! 日時の文字列表現は `YYYY/MM/DD HH:MM:SS`（ローカル時刻）で統一する。

use std::path::Path;
use std::time::{Duration, SystemTime};

use chrono::{Local, TimeZone};

/// 編集できるファイル属性ビット。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileAttrs {
    pub readonly: bool,
    pub hidden: bool,
    pub system: bool,
    pub archive: bool,
}

/// 日時文字列の書式（ローカル時刻）。
const TIME_FMT: &str = "%Y/%m/%d %H:%M:%S";

/// `SystemTime` をローカル時刻の `YYYY/MM/DD HH:MM:SS` に整形する。範囲外は空文字。
pub fn format_local(t: SystemTime) -> String {
    let secs = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => return String::new(),
    };
    match Local.timestamp_opt(secs, 0).single() {
        Some(dt) => dt.format(TIME_FMT).to_string(),
        None => String::new(),
    }
}

/// ローカル時刻の `YYYY/MM/DD HH:MM:SS` を `SystemTime` に解釈する。解釈できなければ `None`。
pub fn parse_local(s: &str) -> Option<SystemTime> {
    let naive = chrono::NaiveDateTime::parse_from_str(s.trim(), TIME_FMT).ok()?;
    let dt = Local.from_local_datetime(&naive).single()?;
    let secs = dt.timestamp();
    if secs < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64))
}

/// path の更新日時を読む。取れなければ `None`。
pub fn modified_time(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// path の作成日時を読む。取れなければ `None`。
pub fn created_time(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.created().ok()
}

/// `t` をローカル時刻で同日の 00:00:00 に丸めた `SystemTime` を返す（日時クイック設定の
/// 「00:00:00」用）。範囲外などで丸められなければ `t` をそのまま返す。
pub fn floor_to_local_midnight(t: SystemTime) -> SystemTime {
    let secs = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => return t,
    };
    let Some(dt) = Local.timestamp_opt(secs, 0).single() else {
        return t;
    };
    let Some(midnight) = dt.date_naive().and_hms_opt(0, 0, 0) else {
        return t;
    };
    let Some(local) = Local.from_local_datetime(&midnight).single() else {
        return t;
    };
    let s = local.timestamp();
    if s < 0 {
        return t;
    }
    SystemTime::UNIX_EPOCH + Duration::from_secs(s as u64)
}

#[cfg(windows)]
mod win {
    pub const READONLY: u32 = 0x1;
    pub const HIDDEN: u32 = 0x2;
    pub const SYSTEM: u32 = 0x4;
    pub const ARCHIVE: u32 = 0x20;
    pub const NORMAL: u32 = 0x80;
    pub const INVALID: u32 = u32::MAX;
}

/// path の編集可能属性を読む。取れなければ `None`。
#[cfg(windows)]
pub fn read_attrs(path: &Path) -> Option<FileAttrs> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileAttributesW(p: *const u16) -> u32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let a = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if a == win::INVALID {
        return None;
    }
    Some(FileAttrs {
        readonly: a & win::READONLY != 0,
        hidden: a & win::HIDDEN != 0,
        system: a & win::SYSTEM != 0,
        archive: a & win::ARCHIVE != 0,
    })
}

/// path の編集可能属性を `attrs` の通りに設定する（ディレクトリ等の他ビットは保つ）。
#[cfg(windows)]
pub fn write_attrs(path: &Path, attrs: FileAttrs) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileAttributesW(p: *const u16) -> u32;
        fn SetFileAttributesW(p: *const u16, a: u32) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let cur = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if cur == win::INVALID {
        return Err(std::io::Error::last_os_error());
    }
    let mut a = cur;
    let apply = |a: &mut u32, bit: u32, on: bool| {
        if on {
            *a |= bit;
        } else {
            *a &= !bit;
        }
    };
    apply(&mut a, win::READONLY, attrs.readonly);
    apply(&mut a, win::HIDDEN, attrs.hidden);
    apply(&mut a, win::SYSTEM, attrs.system);
    apply(&mut a, win::ARCHIVE, attrs.archive);
    if a == 0 {
        a = win::NORMAL;
    }
    let ok = unsafe { SetFileAttributesW(wide.as_ptr(), a) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// path の更新日時を設定する。読取専用・ディレクトリでも通る（属性書込み権で開く）。
#[cfg(windows)]
pub fn set_modified_time(path: &Path, t: SystemTime) -> std::io::Result<()> {
    win_set_times(path, None, Some(t))
}

/// path の作成日時を設定する。読取専用・ディレクトリでも通る（属性書込み権で開く）。
#[cfg(windows)]
pub fn set_created_time(path: &Path, t: SystemTime) -> std::io::Result<()> {
    win_set_times(path, Some(t), None)
}

/// 作成日時・更新日時を SetFileTime で設定する（`None` の欄は据え置き）。
#[cfg(windows)]
fn win_set_times(
    path: &Path,
    creation: Option<SystemTime>,
    write: Option<SystemTime>,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const INVALID_HANDLE: isize = -1;
    // UNIX エポック(1970)と FILETIME エポック(1601)の差（秒）。
    const EPOCH_DIFF_SECS: u64 = 11_644_473_600;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            path: *const u16,
            access: u32,
            share: u32,
            sec: *mut core::ffi::c_void,
            disp: u32,
            flags: u32,
            template: isize,
        ) -> isize;
        fn SetFileTime(
            handle: isize,
            creation: *const FileTime,
            access: *const FileTime,
            write: *const FileTime,
        ) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    let to_ft = |t: SystemTime| -> std::io::Result<FileTime> {
        let secs = match t.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => d.as_secs(),
            Err(_) => return Err(std::io::Error::other("時刻が UNIX エポックより前です")),
        };
        let intervals = (secs + EPOCH_DIFF_SECS) * 10_000_000;
        Ok(FileTime { low: intervals as u32, high: (intervals >> 32) as u32 })
    };
    let cft = creation.map(to_ft).transpose()?;
    let wft = write.map(to_ft).transpose()?;
    let cptr = cft.as_ref().map_or(ptr::null(), |f| f as *const FileTime);
    let wptr = wft.as_ref().map_or(ptr::null(), |f| f as *const FileTime);

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let h = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_WRITE_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        )
    };
    if h == INVALID_HANDLE {
        return Err(std::io::Error::last_os_error());
    }
    let ok = unsafe { SetFileTime(h, cptr, ptr::null(), wptr) };
    unsafe {
        CloseHandle(h);
    }
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// 非 Windows では読取専用のみ扱う。
#[cfg(not(windows))]
pub fn read_attrs(path: &Path) -> Option<FileAttrs> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileAttrs { readonly: meta.permissions().readonly(), ..Default::default() })
}

#[cfg(not(windows))]
pub fn write_attrs(path: &Path, attrs: FileAttrs) -> std::io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_readonly(attrs.readonly);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(windows))]
pub fn set_modified_time(path: &Path, t: SystemTime) -> std::io::Result<()> {
    let f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.set_modified(t)
}

/// 非 Windows では作成日時の設定はできない。
#[cfg(not(windows))]
pub fn set_created_time(_path: &Path, _t: SystemTime) -> std::io::Result<()> {
    Err(std::io::Error::other("作成日時の設定は未対応です"))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rerics_attrs_{}_{}", std::process::id(), tag));
        p
    }

    #[test]
    fn attrs_round_trip() {
        let p = temp_path("attr.txt");
        std::fs::write(&p, b"x").unwrap();
        write_attrs(&p, FileAttrs { readonly: true, hidden: true, ..Default::default() }).unwrap();
        let got = read_attrs(&p).unwrap();
        assert!(got.readonly && got.hidden);
        assert!(!got.system);
        // 読取専用を外さないと消せない。
        write_attrs(&p, FileAttrs::default()).unwrap();
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn modified_time_round_trip() {
        let p = temp_path("time.txt");
        std::fs::write(&p, b"x").unwrap();
        // 2021/06/15 12:34:56 ローカル。
        let target = parse_local("2021/06/15 12:34:56").unwrap();
        set_modified_time(&p, target).unwrap();
        let got = modified_time(&p).unwrap();
        let a = got.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        let b = target.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(a, b, "更新日時が往復で一致する");
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn created_time_round_trip() {
        let p = temp_path("ctime.txt");
        std::fs::write(&p, b"x").unwrap();
        let target = parse_local("2019/03/01 08:09:10").unwrap();
        set_created_time(&p, target).unwrap();
        let got = created_time(&p).unwrap();
        let a = got.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        let b = target.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(a, b, "作成日時が往復で一致する");
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn time_format_parse_round_trip() {
        let s = "2023/12/31 23:59:00";
        let t = parse_local(s).unwrap();
        assert_eq!(format_local(t), s);
        assert!(parse_local("not a time").is_none());
    }

    #[test]
    fn floor_to_local_midnight_zeroes_time() {
        let t = parse_local("2021/06/15 12:34:56").unwrap();
        assert_eq!(format_local(floor_to_local_midnight(t)), "2021/06/15 00:00:00");
        // すでに真夜中ならそのまま。
        let m = parse_local("2021/06/15 00:00:00").unwrap();
        assert_eq!(format_local(floor_to_local_midnight(m)), "2021/06/15 00:00:00");
    }
}
