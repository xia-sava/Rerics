//! Rerics core — UI 非依存のロジック層。
//!
//! 仮想FS・FileItem・設定・キーバインド・コマンド等を実装フェーズごとに足していく。

mod config;
mod file_list;
mod input;
mod log;
pub mod messages;
mod operation;

pub use config::{
    Config, DEFAULT_CONFIG_TOML, FontSpec, Layout, State, TabState, WindowState, clamp_to_work,
    config_path, data_dir, load_toml, save_toml, state_path,
};
pub use file_list::{
    Align, Colors, Column, ColumnKind, FileItem, FileListState, Rgb, SortType, default_columns,
    glob_match, read_items,
};
pub use input::{Command, KeyChord, KeyMap, vk};
pub use log::{LogLevel, LogLine, LogState};
pub use operation::{OpSummary, OperationHost, run_copy, run_delete};

use std::path::{Path, PathBuf};

/// 1ペイン（片側ウィンドウ）の現在パス管理。
///
/// 一覧の所有・ソート・カーソルは `FileListState` 側の責務とし、Pane はパス管理と
/// その直下エントリの読み出し（`read`）に徹する。ナビゲーションは「移動できたか」を
/// 返し、失敗時は「移動しない」で吸収する。
pub struct Pane {
    path: PathBuf,
}

impl Pane {
    /// 指定パスを絶対パス化して開く。
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        Self { path: abs }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 現在パス直下の `FileItem` 一覧を読み出す（読めなければ空）。
    pub fn read(&self) -> Vec<FileItem> {
        read_items(&self.path).unwrap_or_default()
    }

    /// `name` のディレクトリへ侵入する。移動できたら `true`。
    pub fn enter(&mut self, name: &str) -> bool {
        let target = self.path.join(name);
        if read_items(&target).is_ok() && target.is_dir() {
            self.path = target;
            true
        } else {
            false
        }
    }

    /// 親ディレクトリへ移動する。移動できたら、元いたディレクトリ名を返す。
    pub fn to_parent(&mut self) -> Option<String> {
        let parent = self.path.parent().map(Path::to_path_buf)?;
        let prev = self
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned());
        if read_items(&parent).is_ok() {
            self.path = parent;
            prev
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_items_reads_own_manifest_dir() {
        let items = read_items(env!("CARGO_MANIFEST_DIR")).unwrap();
        assert!(items.iter().any(|e| e.name == "Cargo.toml"));
    }

    #[test]
    fn pane_open_reads_entries() {
        let p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        assert!(p.path().is_absolute());
        assert!(p.read().iter().any(|e| e.name == "Cargo.toml"));
    }

    #[test]
    fn pane_parent_then_enter_roundtrip() {
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        let start = p.path().to_path_buf();
        let prev = p.to_parent().unwrap();
        assert_eq!(prev, "rerics-core");
        assert_eq!(p.path(), start.parent().unwrap());
        assert!(p.enter("rerics-core"));
        assert_eq!(p.path(), start.as_path());
    }
}
