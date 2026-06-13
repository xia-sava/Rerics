//! ファイル操作（コピー/移動/削除）のロジック層。UI 非依存・テスト可能。
//!
//! 原作 `FilerScriptCopy` / `FilerScriptDelete` の `Main()` 相当を移植する。
//! GUI 側はワーカースレッドからこの関数を呼び、[`OperationHost`] 経由でログ追記・
//! 協調キャンセル・同名衝突の解決を橋渡しする。ペイン再読込は GUI スレッドの責務
//! なので含めない。

use std::path::Path;

use crate::LogLevel;
use crate::messages;

/// 同名ファイルが存在したときの解決方法。原作 `frmCopyOption` の選択肢に対応する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictResolution {
    /// 元が新しいときだけ上書きする。
    Newest,
    /// 上書きする。
    Overwrite,
    /// スキップする。
    Skip,
    /// 操作全体を中止する。
    Cancel,
}

/// 操作ロジックと GUI のあいだのフック。ワーカースレッドから呼ばれる。
pub trait OperationHost {
    /// ログを1行追記する。
    fn log(&self, level: LogLevel, text: &str);
    /// 中止が要求されていれば `true`（各ファイルの区切りで確認する）。
    fn cancelled(&self) -> bool;
    /// 同名ファイル `name` が衝突したときの解決方法を尋ねる。
    fn resolve_conflict(&self, name: &str) -> ConflictResolution;
}

/// 操作の結果サマリ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpSummary {
    pub ok: usize,
    pub skip: usize,
    pub err: usize,
    pub cancelled: bool,
}

/// 再帰処理の継続可否。
enum Flow {
    Continue,
    Cancel,
}

/// コピー/移動を実行する。`move_it` が `true` なら移動。
pub fn run_copy(
    host: &dyn OperationHost,
    src_dir: &Path,
    dst_dir: &Path,
    names: &[String],
    move_it: bool,
) -> OpSummary {
    let mut sum = OpSummary::default();
    for name in names {
        if host.cancelled() {
            sum.cancelled = true;
            break;
        }
        let src = src_dir.join(name);
        let dst = dst_dir.join(name);
        if src == dst {
            let line = if move_it {
                messages::same_move_path(name)
            } else {
                messages::same_copy_path(name)
            };
            host.log(LogLevel::Error, &line);
            sum.err += 1;
            continue;
        }
        if let Flow::Cancel = copy_item(host, &src, &dst, move_it, &mut sum) {
            sum.cancelled = true;
            break;
        }
    }
    let line = messages::copy_result(sum.ok, sum.skip, sum.err);
    let level = if sum.err == 0 { LogLevel::Info } else { LogLevel::Error };
    host.log(level, &line);
    sum
}

/// 1項目（ファイルまたはディレクトリ）を再帰的にコピー/移動する。
fn copy_item(
    host: &dyn OperationHost,
    src: &Path,
    dst: &Path,
    move_it: bool,
    sum: &mut OpSummary,
) -> Flow {
    let name = file_name(src);

    // 移動かつ衝突なしなら rename で一括（同一ドライブの高速路）。失敗時は個別へ。
    if move_it && !dst.exists() && std::fs::rename(src, dst).is_ok() {
        host.log(LogLevel::Normal, &messages::move_(&name));
        sum.ok += 1;
        return Flow::Continue;
    }

    if src.is_dir() {
        if dst.exists() {
            host.log(LogLevel::Warning, &messages::all_ready_exists(&name));
            sum.skip += 1;
        } else if let Err(e) = std::fs::create_dir_all(dst) {
            host.log(LogLevel::Error, &messages::create_directory_failure(&name, &e.to_string()));
            sum.err += 1;
            return Flow::Continue;
        } else {
            host.log(LogLevel::Normal, &messages::create_directory(&name));
            sum.ok += 1;
        }
        let entries = match std::fs::read_dir(src) {
            Ok(e) => e,
            Err(e) => {
                host.log(LogLevel::Error, &messages::copy_failure(&name, &e.to_string()));
                sum.err += 1;
                return Flow::Continue;
            }
        };
        for entry in entries {
            if host.cancelled() {
                return Flow::Cancel;
            }
            let Ok(entry) = entry else { continue };
            let child_dst = dst.join(entry.file_name());
            if let Flow::Cancel = copy_item(host, &entry.path(), &child_dst, move_it, sum) {
                return Flow::Cancel;
            }
        }
        if move_it {
            let _ = std::fs::remove_dir(src);
        }
        Flow::Continue
    } else {
        let do_copy = if dst.exists() {
            match host.resolve_conflict(&name) {
                ConflictResolution::Newest => is_src_newer(src, dst),
                ConflictResolution::Overwrite => true,
                ConflictResolution::Skip => false,
                ConflictResolution::Cancel => return Flow::Cancel,
            }
        } else {
            true
        };
        if !do_copy {
            host.log(LogLevel::Warning, &messages::skip(&name));
            sum.skip += 1;
            return Flow::Continue;
        }
        let line = if move_it {
            messages::move_(&name)
        } else {
            messages::copy(&name)
        };
        host.log(LogLevel::Normal, &line);
        match copy_file(src, dst, move_it) {
            Ok(()) => sum.ok += 1,
            Err(e) => {
                let reason = e.to_string();
                let line = if move_it {
                    messages::move_failure(&name, &reason)
                } else {
                    messages::copy_failure(&name, &reason)
                };
                host.log(LogLevel::Error, &line);
                sum.err += 1;
            }
        }
        Flow::Continue
    }
}

/// 削除を実行する。
pub fn run_delete(host: &dyn OperationHost, dir: &Path, names: &[String]) -> OpSummary {
    let mut sum = OpSummary::default();
    for name in names {
        if host.cancelled() {
            sum.cancelled = true;
            break;
        }
        let target = dir.join(name);
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

/// `path` のファイル名部分を取り出す。
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// src の更新時刻が dst より新しいか（読めない場合はコピー扱いで `true`）。
fn is_src_newer(src: &Path, dst: &Path) -> bool {
    match (
        std::fs::metadata(src).and_then(|m| m.modified()),
        std::fs::metadata(dst).and_then(|m| m.modified()),
    ) {
        (Ok(a), Ok(b)) => a > b,
        _ => true,
    }
}

/// 1ファイルをコピーし、移動なら元を消す。
fn copy_file(src: &Path, dst: &Path, move_it: bool) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst)?;
    if move_it {
        std::fs::remove_file(src)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// テスト専用の一時ディレクトリ。Drop で再帰削除する。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("rerics_optest_{}_{n}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        fn write_file(&self, name: &str, body: &str) {
            std::fs::write(self.join(name), body).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// ログを記録し、指定回数のチェック後に中止を返すフェイクホスト。
    struct FakeHost {
        logs: RefCell<Vec<(LogLevel, String)>>,
        cancel_after: isize,
        checks: Cell<isize>,
        conflict: ConflictResolution,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                logs: RefCell::new(Vec::new()),
                cancel_after: -1,
                checks: Cell::new(0),
                conflict: ConflictResolution::Overwrite,
            }
        }

        fn cancelling(after: isize) -> Self {
            Self { cancel_after: after, ..Self::new() }
        }

        fn with_conflict(res: ConflictResolution) -> Self {
            Self { conflict: res, ..Self::new() }
        }

        fn lines(&self) -> Vec<String> {
            self.logs.borrow().iter().map(|(_, t)| t.clone()).collect()
        }
    }

    impl OperationHost for FakeHost {
        fn log(&self, level: LogLevel, text: &str) {
            self.logs.borrow_mut().push((level, text.to_owned()));
        }

        fn cancelled(&self) -> bool {
            if self.cancel_after < 0 {
                return false;
            }
            let c = self.checks.get();
            self.checks.set(c + 1);
            c >= self.cancel_after
        }

        fn resolve_conflict(&self, _name: &str) -> ConflictResolution {
            self.conflict.clone()
        }
    }

    #[test]
    fn copy_file_succeeds() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "hello");
        let host = FakeHost::new();
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(sum, OpSummary { ok: 1, skip: 0, err: 0, cancelled: false });
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "hello");
        assert!(src.join("a.txt").exists());
    }

    #[test]
    fn move_file_removes_source() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "hello");
        let host = FakeHost::new();
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], true);
        assert_eq!(sum.ok, 1);
        assert!(!src.join("a.txt").exists());
        assert!(dst.join("a.txt").exists());
    }

    #[test]
    fn copy_missing_source_reports_error() {
        let src = TempDir::new();
        let dst = TempDir::new();
        let host = FakeHost::new();
        let sum = run_copy(&host, &src.path, &dst.path, &["nope.txt".to_owned()], false);
        assert_eq!(sum.err, 1);
        assert_eq!(sum.ok, 0);
        assert!(host.lines().iter().any(|l| l.contains("コピーに失敗しました")));
        assert!(host.lines().iter().any(|l| l == "0 Success, 0 Skip, 1 Error"));
    }

    #[test]
    fn copy_same_path_guarded() {
        let dir = TempDir::new();
        dir.write_file("a.txt", "x");
        let host = FakeHost::new();
        let sum = run_copy(&host, &dir.path, &dir.path, &["a.txt".to_owned()], false);
        assert_eq!(sum.err, 1);
        assert!(host.lines().iter().any(|l| l.starts_with("コピー先が同じです")));
    }

    #[test]
    fn delete_file_succeeds() {
        let dir = TempDir::new();
        dir.write_file("a.txt", "x");
        let host = FakeHost::new();
        let sum = run_delete(&host, &dir.path, &["a.txt".to_owned()]);
        assert_eq!(sum.ok, 1);
        assert!(!dir.join("a.txt").exists());
    }

    #[test]
    fn cancel_stops_early() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "1");
        src.write_file("b.txt", "2");
        let host = FakeHost::cancelling(1);
        let names = vec!["a.txt".to_owned(), "b.txt".to_owned()];
        let sum = run_copy(&host, &src.path, &dst.path, &names, false);
        assert!(sum.cancelled);
        assert_eq!(sum.ok, 1);
        assert!(dst.join("a.txt").exists());
        assert!(!dst.join("b.txt").exists());
    }

    #[test]
    fn conflict_skip_keeps_destination() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "new");
        dst.write_file("a.txt", "old");
        let host = FakeHost::with_conflict(ConflictResolution::Skip);
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(sum.skip, 1);
        assert_eq!(sum.ok, 0);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "old");
    }

    #[test]
    fn conflict_overwrite_replaces_destination() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "new");
        dst.write_file("a.txt", "old");
        let host = FakeHost::with_conflict(ConflictResolution::Overwrite);
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(sum.ok, 1);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "new");
    }

    #[test]
    fn conflict_cancel_aborts() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "new");
        src.write_file("b.txt", "new");
        dst.write_file("a.txt", "old");
        let host = FakeHost::with_conflict(ConflictResolution::Cancel);
        let names = vec!["a.txt".to_owned(), "b.txt".to_owned()];
        let sum = run_copy(&host, &src.path, &dst.path, &names, false);
        assert!(sum.cancelled);
        assert!(!dst.join("b.txt").exists());
    }
}
