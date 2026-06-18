//! ファイル操作（コピー/移動/削除）のロジック層。UI 非依存・テスト可能。
//!
//! 原作 `FilerScriptCopy` / `FilerScriptDelete` の `Main()` 相当を移植する。
//! GUI 側はワーカースレッドからこの関数を呼び、[`OperationHost`] 経由でログ追記・
//! 協調キャンセル・同名衝突の解決を橋渡しする。ペイン再読込は GUI スレッドの責務
//! なので含めない。

use std::path::Path;
use std::time::Instant;

use crate::LogLevel;

mod copy;
mod extract;
mod compress;
mod archive_ops;
mod delete;
mod calc;
pub use copy::run_copy;
pub use extract::run_extract;
pub use compress::run_compress;
pub use archive_ops::{run_archive_add, run_archive_rebuild, run_archive_delete, run_archive_rename};
pub use delete::run_delete;
pub use calc::run_calc_size;

/// 同名ファイルが存在したときの解決方法。原作 `frmCopyOption` の選択肢に対応する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictResolution {
    /// 元が新しいときだけ上書きする。
    Newest,
    /// 上書きする。
    Overwrite,
    /// 読み込み専用/隠し/システム属性を消してから上書きする。
    OverwriteForce,
    /// 別名でコピーする。
    Rename(String),
    /// スキップする。
    Skip,
    /// 操作全体を中止する。
    Cancel,
}

/// 読み込み専用/隠し/システム属性ファイルの削除確認に対する回答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteWarnChoice {
    /// 属性を解除して削除する。
    Yes,
    /// 削除しない。
    No,
    /// 操作全体を中止する。
    Cancel,
}

/// インプレース更新できるログ行のハンドル。[`OperationHost::begin_progress`] が返し、
/// [`OperationHost::update_progress`] で同じ行を書き換える。中身の解釈はホスト依存。
#[derive(Debug, Clone, Copy)]
pub struct ProgressHandle(pub u64);

/// 操作ロジックと GUI のあいだのフック。ワーカースレッドから呼ばれる。
pub trait OperationHost {
    /// ログを1行追記する。
    fn log(&self, level: LogLevel, text: &str);
    /// 中止が要求されていれば `true`（各ファイルの区切りで確認する）。
    fn cancelled(&self) -> bool;
    /// 中断要求中はここでブロックする（再開/中止まで。各ファイルの区切りで呼ぶ）。
    fn wait_while_suspended(&self);
    /// 同名ファイル `name` が衝突したときの解決方法を尋ねる。
    fn resolve_conflict(&self, name: &str) -> ConflictResolution;
    /// 属性付き（`attr`＝読み込み専用/隠し/システム）ファイル `name` の削除可否を尋ねる。
    fn confirm_delete_attr(&self, name: &str, attr: &str) -> DeleteWarnChoice;

    /// インプレース更新できる行を開始する（1行追記してハンドルを返す）。
    /// 既定では通常ログとして1行出すだけ（進捗更新しないホスト向け）。
    fn begin_progress(&self, level: LogLevel, text: &str) -> ProgressHandle {
        self.log(level, text);
        ProgressHandle(0)
    }

    /// [`Self::begin_progress`] で得た行の本文を書き換える。既定では何もしない。
    fn update_progress(&self, handle: ProgressHandle, text: &str) {
        let _ = (handle, text);
    }
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

/// 1ファイルコピーの結末（完了か、バイト境界での中止か）。
enum CopyOutcome {
    Completed,
    Cancelled,
}

/// ファイル境界のチェックポイント。中断中は待機し、中止要求があれば `true` を返す。
fn should_stop(host: &dyn OperationHost) -> bool {
    if host.cancelled() {
        return true;
    }
    host.wait_while_suspended();
    host.cancelled()
}

/// ディレクトリ使用量の集計結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirInfo {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
}

/// `path` のファイル名部分を取り出す。
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}


/// dst の読み込み専用/隠し/システム属性を解除する（強制上書き用）。
#[cfg(windows)]
fn clear_attributes(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileAttributesW(path: *const u16, attrs: u32) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_NORMAL);
    }
}

/// 非 Windows では読み込み専用のみ解除する。
#[cfg(not(windows))]
fn clear_attributes(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(path, perms);
    }
}

/// コピー進捗の「3秒経過後、％が変わったときだけ通知する」判定器。
struct ProgressTracker {
    start: Instant,
    prev_pct: i32,
}

impl ProgressTracker {
    fn new() -> Self {
        Self { start: Instant::now(), prev_pct: -1 }
    }

    /// 開始から3秒経過後、`transferred/total` の百分率が前回と変わっていれば
    /// 新しい百分率を返す。まだ3秒未満・total 0・％不変なら `None`。
    fn tick(&mut self, transferred: u64, total: u64) -> Option<u32> {
        self.tick_with_elapsed(self.start.elapsed().as_secs(), transferred, total)
    }

    fn tick_with_elapsed(&mut self, elapsed_secs: u64, transferred: u64, total: u64) -> Option<u32> {
        if total == 0 || elapsed_secs < 3 {
            return None;
        }
        let pct = ((transferred as u128 * 100) / total as u128) as u32;
        if pct as i32 != self.prev_pct {
            self.prev_pct = pct as i32;
            Some(pct)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests;
