//! ファイル操作（コピー/移動/削除）のロジック層。UI 非依存・テスト可能。
//!
//! 原作 `FilerScriptCopy` / `FilerScriptDelete` の `Main()` 相当を移植する。
//! GUI 側はワーカースレッドからこの関数を呼び、[`OperationHost`] 経由でログ追記と
//! 協調キャンセルを橋渡しする。ペイン再読込は GUI スレッドの責務なので含めない。

use std::path::Path;

use crate::LogLevel;
use crate::messages;

/// 操作ロジックと GUI のあいだのフック。ワーカースレッドから呼ばれる。
pub trait OperationHost {
    /// ログを1行追記する。
    fn log(&self, level: LogLevel, text: &str);
    /// 中止が要求されていれば `true`（各ファイルの区切りで確認する）。
    fn cancelled(&self) -> bool;
}

/// 操作の結果サマリ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpSummary {
    pub ok: usize,
    pub skip: usize,
    pub err: usize,
    pub cancelled: bool,
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
        let line = if move_it {
            messages::move_(name)
        } else {
            messages::copy(name)
        };
        host.log(LogLevel::Normal, &line);
        let result = if move_it {
            move_path(&src, &dst)
        } else {
            copy_path(&src, &dst)
        };
        match result {
            Ok(()) => sum.ok += 1,
            Err(e) => {
                let reason = e.to_string();
                let line = if move_it {
                    messages::move_failure(name, &reason)
                } else {
                    messages::copy_failure(name, &reason)
                };
                host.log(LogLevel::Error, &line);
                sum.err += 1;
            }
        }
    }
    let line = messages::copy_result(sum.ok, sum.skip, sum.err);
    let level = if sum.err == 0 { LogLevel::Info } else { LogLevel::Error };
    host.log(level, &line);
    sum
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

/// src を dst へ移動する。リネームで失敗したらコピー後に元を消す（ドライブ跨ぎ等）。
fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => copy_path(src, dst).and_then(|()| {
            if src.is_dir() {
                std::fs::remove_dir_all(src)
            } else {
                std::fs::remove_file(src)
            }
        }),
    }
}

/// src を dst へコピーする。ディレクトリは再帰的に複製する。
fn copy_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_path(&entry.path(), &dst.join(&name))?;
        }
        Ok(())
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    }
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
    }

    impl FakeHost {
        fn new() -> Self {
            Self { logs: RefCell::new(Vec::new()), cancel_after: -1, checks: Cell::new(0) }
        }

        fn cancelling(after: isize) -> Self {
            Self { logs: RefCell::new(Vec::new()), cancel_after: after, checks: Cell::new(0) }
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
}
