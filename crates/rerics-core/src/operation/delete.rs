use std::path::Path;
use crate::LogLevel;
use crate::messages;
use super::*;

/// 1項目の削除結果。親ディレクトリを削除してよいかの判断に使う。
enum Removal {
    /// 実際に削除できた（親は空に近づいた）。
    Removed,
    /// 意図的に残した／削除に失敗した（実体が残っている）。
    Kept,
    /// 操作全体を中止する。
    Cancel,
}

/// 削除を実行する。ディレクトリは配下を個別に確認・削除してから（ボトムアップで）本体を消す。
pub fn run_delete(host: &dyn OperationHost, dir: &Path, names: &[String]) -> OpSummary {
    host.log(LogLevel::Info, &messages::op_started("削除"));
    let mut sum = OpSummary::default();
    for name in names {
        if let Removal::Cancel = delete_item(host, &dir.join(name), name, &mut sum) {
            sum.cancelled = true;
            break;
        }
    }
    let line = messages::delete_result(sum.ok, sum.err);
    let level = if sum.err == 0 { LogLevel::Info } else { LogLevel::Error };
    host.log(level, &line);
    log_op_end(host, "削除", &sum);
    sum
}

/// `target`（`name` 表示）を削除する。ディレクトリなら配下を再帰的に処理してから本体を消す。
/// 属性付き（読み込み専用/隠し/システム）の項目は削除前にホストへ可否を尋ねる。
fn delete_item(host: &dyn OperationHost, target: &Path, name: &str, sum: &mut OpSummary) -> Removal {
    if should_stop(host) {
        return Removal::Cancel;
    }

    // シンボリックリンク／ジャンクションは中へ再帰せず、リンク自体だけを消す。
    let link_meta = std::fs::symlink_metadata(target).ok();
    let is_link = link_meta.as_ref().map(meta_is_link).unwrap_or(false);
    let is_dir = !is_link && target.is_dir();

    if is_dir {
        return delete_directory(host, target, name, sum);
    }

    if let Some(removal) = confirm_and_clear(host, target, name) {
        return removal;
    }
    host.log(LogLevel::Normal, &messages::delete(name));
    // ディレクトリを指すリンク（リンク先が消えていても）は remove_dir、それ以外は
    // remove_file。リンク先の有無で辿らず、リンク自体の種別で消し方を選ぶ。
    let link_is_dir = link_meta.as_ref().map(meta_is_dir).unwrap_or(false);
    let result = if is_link && link_is_dir {
        std::fs::remove_dir(target)
    } else {
        std::fs::remove_file(target)
    };
    finish_removal(host, name, result, sum)
}

/// ディレクトリ `target` を配下から順に削除し、空になったら本体を消す。
fn delete_directory(host: &dyn OperationHost, target: &Path, name: &str, sum: &mut OpSummary) -> Removal {
    let entries = match std::fs::read_dir(target) {
        Ok(rd) => rd,
        Err(e) => {
            host.log(LogLevel::Error, &messages::delete_failure(name, &e.to_string()));
            sum.err += 1;
            return Removal::Kept;
        }
    };
    let mut all_removed = true;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                all_removed = false;
                continue;
            }
        };
        let child = entry.path();
        let child_name = entry.file_name().to_string_lossy().into_owned();
        match delete_item(host, &child, &child_name, sum) {
            Removal::Removed => {}
            Removal::Kept => all_removed = false,
            Removal::Cancel => return Removal::Cancel,
        }
    }
    // 残した／消せなかった項目があるディレクトリは本体を消さない（非空エラーを避ける）。
    if !all_removed {
        return Removal::Kept;
    }
    if let Some(removal) = confirm_and_clear(host, target, name) {
        return removal;
    }
    host.log(LogLevel::Normal, &messages::delete_directory(name));
    finish_removal(host, name, std::fs::remove_dir(target), sum)
}

/// 属性付き項目なら削除可否を尋ね、許可なら属性を解除する。確認の結果として
/// 「残す（`Kept`）／中止（`Cancel`）」が確定したときだけ `Some` を返す。続行なら `None`。
fn confirm_and_clear(host: &dyn OperationHost, target: &Path, name: &str) -> Option<Removal> {
    let label = attribute_label(target)?;
    match host.confirm_delete_attr(name, label) {
        DeleteWarnChoice::Yes => {
            clear_attributes(target);
            None
        }
        DeleteWarnChoice::No => Some(Removal::Kept),
        DeleteWarnChoice::Cancel => Some(Removal::Cancel),
    }
}

/// 削除結果を集計しログに反映する。成功は `Removed`、失敗は `Kept`。
fn finish_removal(
    host: &dyn OperationHost,
    name: &str,
    result: std::io::Result<()>,
    sum: &mut OpSummary,
) -> Removal {
    match result {
        Ok(()) => {
            sum.ok += 1;
            Removal::Removed
        }
        Err(e) => {
            host.log(LogLevel::Error, &messages::delete_failure(name, &e.to_string()));
            sum.err += 1;
            Removal::Kept
        }
    }
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
