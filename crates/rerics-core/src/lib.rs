//! Rerics core — UI 非依存のロジック層。
//!
//! 仮想FS・FileItem・設定・キーバインド・コマンド等を実装フェーズごとに足していく。

mod input;
pub use input::{Command, KeyChord, KeyMap, vk};

use std::path::{Path, PathBuf};

/// ファイル一覧の1エントリ（UI 非依存の最小情報）。
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// 指定ディレクトリ直下のエントリを読み出す。
///
/// 並び順はディレクトリ優先・名前昇順（本格的なソート種別は後続フェーズで実装）。
pub fn list_dir(path: impl AsRef<Path>) -> std::io::Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    for ent in std::fs::read_dir(path)? {
        let ent = ent?;
        let meta = ent.metadata()?;
        entries.push(FileEntry {
            name: ent.file_name().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
        });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(entries)
}

/// 1ペイン（片側ウィンドウ）の状態。現在パスとその直下エントリを保持する。
///
/// ナビゲーション系メソッドは読み出しに失敗した場合「移動しない」で吸収し、
/// 移動できたかを `bool` で返す（エラー通知は将来のログ機構で扱う）。
pub struct Pane {
    path: PathBuf,
    entries: Vec<FileEntry>,
}

impl Pane {
    /// 指定パスを絶対パス化して開く。読めない場合はエントリ空で開く。
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let entries = list_dir(&abs).unwrap_or_default();
        Self { path: abs, entries }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    /// 現在パスを読み直す。
    pub fn reload(&mut self) {
        self.entries = list_dir(&self.path).unwrap_or_default();
    }

    /// `index` のエントリがディレクトリならそこへ移動する。移動できたら `true`。
    pub fn enter(&mut self, index: usize) -> bool {
        let Some(e) = self.entries.get(index) else {
            return false;
        };
        if !e.is_dir {
            return false;
        }
        let target = self.path.join(&e.name);
        match list_dir(&target) {
            Ok(entries) => {
                self.path = target;
                self.entries = entries;
                true
            }
            Err(_) => false,
        }
    }

    /// 親ディレクトリへ移動する。移動できたら `true`。
    pub fn to_parent(&mut self) -> bool {
        let Some(parent) = self.path.parent().map(Path::to_path_buf) else {
            return false;
        };
        match list_dir(&parent) {
            Ok(entries) => {
                self.path = parent;
                self.entries = entries;
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_dir_reads_own_manifest_dir() {
        let entries = list_dir(env!("CARGO_MANIFEST_DIR")).unwrap();
        assert!(entries.iter().any(|e| e.name == "Cargo.toml"));
    }

    #[test]
    fn pane_open_lists_entries() {
        let p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        assert!(p.path().is_absolute());
        assert!(p.entries().iter().any(|e| e.name == "Cargo.toml"));
    }

    #[test]
    fn pane_parent_then_enter_roundtrip() {
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        let start = p.path().to_path_buf();
        assert!(p.to_parent());
        assert_eq!(p.path(), start.parent().unwrap());
        let idx = p
            .entries()
            .iter()
            .position(|e| e.name == "rerics-core" && e.is_dir)
            .unwrap();
        assert!(p.enter(idx));
        assert_eq!(p.path(), start.as_path());
    }
}
