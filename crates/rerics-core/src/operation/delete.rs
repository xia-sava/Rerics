use std::path::Path;
use crate::LogLevel;
use crate::messages;
use super::*;

/// 削除を実行する。
pub fn run_delete(host: &dyn OperationHost, dir: &Path, names: &[String]) -> OpSummary {
    let mut sum = OpSummary::default();
    for name in names {
        if should_stop(host) {
            sum.cancelled = true;
            break;
        }
        let target = dir.join(name);
        if let Some(label) = attribute_label(&target) {
            match host.confirm_delete_attr(name, label) {
                DeleteWarnChoice::Yes => clear_attributes(&target),
                DeleteWarnChoice::No => continue,
                DeleteWarnChoice::Cancel => {
                    sum.cancelled = true;
                    break;
                }
            }
        }
        let is_dir = target.is_dir();
        let line = if is_dir {
            messages::delete_directory(name)
        } else {
            messages::delete(name)
        };
        host.log(LogLevel::Normal, &line);
        let result = if is_dir {
            std::fs::remove_dir_all(&target)
        } else {
            std::fs::remove_file(&target)
        };
        match result {
            Ok(()) => sum.ok += 1,
            Err(e) => {
                host.log(LogLevel::Error, &messages::delete_failure(name, &e.to_string()));
                sum.err += 1;
            }
        }
    }
    let line = messages::delete_result(sum.ok, sum.err);
    let level = if sum.err == 0 { LogLevel::Info } else { LogLevel::Error };
    host.log(level, &line);
    sum
}

/// path の属性ラベルを返す（優先度 システム > 隠し > 読み込み専用、無ければ `None`）。
#[cfg(windows)]
fn attribute_label(path: &Path) -> Option<&'static str> {
    use std::os::windows::ffi::OsStrExt;
    const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileAttributesW(path: *const u16) -> u32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let attrs = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attrs == INVALID_FILE_ATTRIBUTES {
        None
    } else if attrs & FILE_ATTRIBUTE_SYSTEM != 0 {
        Some("システム")
    } else if attrs & FILE_ATTRIBUTE_HIDDEN != 0 {
        Some("隠し")
    } else if attrs & FILE_ATTRIBUTE_READONLY != 0 {
        Some("読み込み専用")
    } else {
        None
    }
}

/// 非 Windows では読み込み専用のみ判定する。
#[cfg(not(windows))]
fn attribute_label(path: &Path) -> Option<&'static str> {
    match std::fs::metadata(path) {
        Ok(m) if m.permissions().readonly() => Some("読み込み専用"),
        _ => None,
    }
}
