use std::path::Path;
use std::time::SystemTime;
use crate::LogLevel;
use crate::archive::{ArchiveBackend, ArchiveEntry, entries_at};
use crate::messages;
use super::*;

/// 書庫から実FSへの取り出し（展開コピー）。`backend` の `names`（`src_inner` 直下の
/// エントリ名）を `dst_dir` 配下へ展開する。ディレクトリは再帰。衝突・中止・進捗・
/// サマリは [`run_copy`] と同じ host 経由で、ログ文面もコピーと共通にする（書込み先は
/// 実FS なので「書庫を書き換えない」方針と矛盾しない）。dst 側のパスは検証済みコンポーネント
/// だけを連結し、`..` や区切りを含む細工エントリで dst の外へ書き出すのを防ぐ（zip-slip 対策）。
pub fn run_extract(
    host: &dyn OperationHost,
    backend: &dyn ArchiveBackend,
    entries: &[ArchiveEntry],
    src_inner: &str,
    names: &[String],
    dst_dir: &Path,
) -> OpSummary {
    run_operation(host, "展開", ResultStyle::Copy, || {
        let mut sum = OpSummary::default();
        for name in names {
            if should_stop(host) {
                sum.cancelled = true;
                break;
            }
            let Some(comp) = safe_component(name) else {
                host.log(LogLevel::Error, &messages::copy_failure(name, "不正な名前です"));
                sum.err += 1;
                continue;
            };
            let inner = join_inner_seg(src_inner, name);
            let dst = dst_dir.join(comp);
            if let Flow::Cancel = extract_item(host, backend, entries, &inner, &dst, &mut sum) {
                sum.cancelled = true;
                break;
            }
        }
        sum
    })
}

/// 書庫内の1エントリ（ファイル or ディレクトリ）を再帰的に取り出す。
fn extract_item(
    host: &dyn OperationHost,
    backend: &dyn ArchiveBackend,
    entries: &[ArchiveEntry],
    inner: &str,
    dst: &Path,
    sum: &mut OpSummary,
) -> Flow {
    let name = inner.rsplit('/').next().unwrap_or(inner).to_string();

    if entry_is_dir(entries, inner) {
        if dst.exists() {
            // 既存ディレクトリへはマージする（衝突は配下のファイル単位で解決）。
        } else if let Err(e) = std::fs::create_dir_all(dst) {
            host.log(LogLevel::Error, &messages::create_directory_failure(&name, &e.to_string()));
            sum.err += 1;
            return Flow::Continue;
        } else {
            host.log(LogLevel::Normal, &messages::create_directory(&name));
            sum.ok += 1;
        }
        for child in entries_at(entries, inner) {
            if should_stop(host) {
                return Flow::Cancel;
            }
            let Some(comp) = safe_component(&child.name) else {
                host.log(LogLevel::Error, &messages::copy_failure(&child.name, "不正な名前です"));
                sum.err += 1;
                continue;
            };
            let child_inner = join_inner_seg(inner, &child.name);
            let child_dst = dst.join(comp);
            if let Flow::Cancel = extract_item(host, backend, entries, &child_inner, &child_dst, sum) {
                return Flow::Cancel;
            }
        }
        Flow::Continue
    } else {
        // 衝突解決。別名(Rename)を選んでもその別名が既存と衝突していれば、黙って上書き
        // せず改めてホストへ確認して繰り返す（既存ファイルの誤消去を防ぐ）。
        let mut target = dst.to_path_buf();
        let do_copy = loop {
            if !target.exists() {
                break true;
            }
            let cur = file_name(&target);
            match host.resolve_conflict(&cur) {
                ConflictResolution::Newest => break archive_newer(entries, inner, &target),
                ConflictResolution::Overwrite => break true,
                ConflictResolution::OverwriteForce => {
                    clear_attributes(&target);
                    break true;
                }
                ConflictResolution::Rename(new) => match safe_component(&new) {
                    Some(c) if new != cur => {
                        target = target.with_file_name(c);
                    }
                    _ => break false,
                },
                ConflictResolution::Skip => break false,
                ConflictResolution::Cancel => return Flow::Cancel,
            }
        };
        if !do_copy {
            host.log(LogLevel::Warning, &messages::skip(&name));
            sum.skip += 1;
            return Flow::Continue;
        }
        // 書庫の読取はストリーム途中で中断できないので、ファイル単位でのみ進捗を出す
        // （バイト進捗は付けない）。読取→書込みの順で実行する。
        host.log(LogLevel::Normal, &messages::copy(&name));
        match backend.read(inner) {
            Ok(bytes) => {
                if let Err(e) = write_extracted(&target, &bytes, entry_mtime(entries, inner)) {
                    host.log(LogLevel::Error, &messages::copy_failure(&name, &e.to_string()));
                    sum.err += 1;
                } else {
                    sum.ok += 1;
                }
            }
            Err(e) => {
                host.log(LogLevel::Error, &messages::copy_failure(&name, &e.to_string()));
                sum.err += 1;
            }
        }
        Flow::Continue
    }
}

/// 取り出した bytes を `target` へ書き出し、可能なら mtime を書庫の値へ合わせる。
fn write_extracted(target: &Path, bytes: &[u8], mtime: Option<SystemTime>) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, bytes)?;
    if let Some(t) = mtime {
        // mtime 復元は best-effort（失敗しても取り出し自体は成功とする）。
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(target) {
            let _ = f.set_modified(t);
        }
    }
    Ok(())
}

/// `inner` が（明示 dir エントリ、または配下にエントリを持つ）ディレクトリか。
fn entry_is_dir(entries: &[ArchiveEntry], inner: &str) -> bool {
    let pfx = format!("{inner}/");
    entries
        .iter()
        .any(|e| (e.path == inner && e.is_dir) || e.path.starts_with(&pfx))
}

/// `inner` エントリの mtime（無ければ None）。
fn entry_mtime(entries: &[ArchiveEntry], inner: &str) -> Option<SystemTime> {
    entries.iter().find(|e| e.path == inner).and_then(|e| e.mtime)
}

/// 書庫側エントリが dst より新しいか（時刻不明はコピー扱いで `true`）。
fn archive_newer(entries: &[ArchiveEntry], inner: &str, dst: &Path) -> bool {
    match (
        entry_mtime(entries, inner),
        std::fs::metadata(dst).and_then(|m| m.modified()).ok(),
    ) {
        (Some(a), Some(b)) => a > b,
        _ => true,
    }
}

/// `inner` に子セグメントを連結（"" のときは name そのもの）。
fn join_inner_seg(inner: &str, name: &str) -> String {
    if inner.is_empty() {
        name.to_string()
    } else {
        format!("{inner}/{name}")
    }
}

/// 1コンポーネントが dst 配下に安全に書けるか検証する。`..`・`.`・空・区切り文字・
/// ドライブ相対や代替データストリームになりうる ':' を含むものは弾く（書き出し先逸脱の
/// 防止）。OK なら元の文字列を返す。
fn safe_component(name: &str) -> Option<&str> {
    if crate::archive::is_safe_segment(name) {
        Some(name)
    } else {
        None
    }
}
