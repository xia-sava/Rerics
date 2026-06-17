//! Rerics core — UI 非依存のロジック層。
//!
//! 仮想FS・FileItem・設定・キーバインド・コマンド等を実装フェーズごとに足していく。

mod archive;
mod attrs;
mod config;
mod file_list;
mod input;
mod log;
mod macros;
mod media;
pub mod messages;
mod operation;
mod status;
mod vfs;
mod viewer;

pub use archive::{
    ArchiveBackend, ArchiveEntry, ArchiveWriter, Caps, open_archive, open_archive_writer,
};
pub use attrs::{
    FileAttrs, created_time, floor_to_local_midnight, format_local, modified_time, parse_local,
    read_attrs, set_created_time, set_modified_time, write_attrs,
};
pub use config::{
    Bookmark, Config, DEFAULT_CONFIG_TOML, FontSpec, Layout, ResolvedTheme, State, TabState, Theme,
    ThemeColors, WindowState, clamp_to_work, config_path, data_dir, load_toml, save_toml,
    state_path,
};
pub use file_list::{
    Align, Colors, Column, ColumnKind, FileItem, FileListState, Rgb, SortType, auto_adjust_columns,
    column_sample, default_columns, find_match, glob_match, read_items, sequence_names,
};
pub use input::{Command, Invocation, KeyChord, KeyMap, vk};
pub use log::{LogLevel, LogLine, LogState};
pub use macros::{MacroAbort, MacroCtx, MacroHost, expand_macros};
pub use media::{
    AnimatedImage, Frame, FrameSource, MediaKind, Placement, StillImage, clamp_pan,
    composite_over_checker, decode_thumbnail, fit_scale, load_image, placement, rgba_to_bgra,
    rotate_rgba, rotated_dims,
};
pub use operation::{
    ConflictResolution, DeleteWarnChoice, DirInfo, OpSummary, OperationHost, ProgressHandle,
    run_archive_add, run_archive_delete, run_archive_rebuild, run_archive_rename, run_calc_size,
    run_compress, run_copy, run_delete, run_extract,
};
pub use status::{format_drive, format_selected, format_size};
pub use vfs::{Location, is_archive_path};
pub use viewer::{DisplayLine, Encoding, ViewMode, ViewerModel};

use std::path::Path;

/// 1ペイン（片側ウィンドウ）の現在地管理。
///
/// 一覧の所有・ソート・カーソルは `FileListState` 側の責務とし、Pane は現在地
/// （実FS or 書庫内＝`Location`）の管理と直下エントリの読み出し（`read`）に徹する。
/// ナビゲーションは「移動できたか」を返し、失敗時は「移動しない」で吸収する。
pub struct Pane {
    loc: Location,
    /// 戻る履歴（古い→新しい。末尾が直前の現在地）。
    back: Vec<Location>,
    /// 進む履歴（戻った後にだけ積まれる。新しい移動で破棄）。
    forward: Vec<Location>,
}

/// 移動履歴の上限（これを超えると古い方から捨てる）。
const HISTORY_LIMIT: usize = 256;

impl Pane {
    /// 指定パス（実FS）を絶対パス化して開く。
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        Self {
            loc: Location::Real(abs),
            back: Vec::new(),
            forward: Vec::new(),
        }
    }

    /// 表示文字列（実FS or "C:\foo.zip\inner"）から書庫境界を検出して復元する。
    /// セッション復元（state.toml）に使う。書庫が消えていれば実FS パスとして開く。
    pub fn restore(display: &str) -> Self {
        Self {
            loc: Location::parse(display),
            back: Vec::new(),
            forward: Vec::new(),
        }
    }

    /// 現在地を `back` に積み、`forward` を破棄する（新しい移動の共通前処理）。
    fn record_history(&mut self) {
        self.back.push(self.loc.clone());
        if self.back.len() > HISTORY_LIMIT {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    /// 現在地（実FS or 書庫内）。
    pub fn loc(&self) -> &Location {
        &self.loc
    }

    /// 現在地をそのまま差し替える（セッション復元などで使う）。
    pub fn set_loc(&mut self, loc: Location) {
        self.loc = loc;
    }

    /// パスバー/タブ用の表示文字列。
    pub fn loc_display(&self) -> String {
        self.loc.loc_display()
    }

    /// 実FS のときのみ実パスを返す（書庫内は None）。
    pub fn as_real_path(&self) -> Option<&Path> {
        self.loc.as_real_path()
    }

    /// 書庫内かどうか。
    pub fn is_archive(&self) -> bool {
        self.loc.is_archive()
    }

    /// 後方互換: 実FS の実パスを返す（書庫内は空パス）。呼び出し側は順次
    /// `loc()`/`loc_display()`/`as_real_path()` へ移行する想定の橋渡し。
    pub fn path(&self) -> &Path {
        self.loc.as_real_path().unwrap_or_else(|| Path::new(""))
    }

    /// 現在地直下の `FileItem` 一覧を読み出す（読めなければ空）。
    pub fn read(&self) -> Vec<FileItem> {
        self.loc.read().unwrap_or_default()
    }

    /// `name` へ侵入する（dir なら降りる・書庫ファイルなら潜る）。移動できたら `true`。
    /// 侵入先が読めることを確認してから確定する（壊れた書庫/権限不足で弾く）。
    pub fn enter(&mut self, name: &str, is_dir: bool) -> bool {
        match self.loc.enter(name, is_dir) {
            Some(next) if next.read().is_ok() => {
                self.record_history();
                self.loc = next;
                true
            }
            _ => false,
        }
    }

    /// 親へ移動する。移動できたら、元いた場所の名前（書庫ルートからは書庫ファイル名）を返す。
    pub fn to_parent(&mut self) -> Option<String> {
        let (parent, prev) = self.loc.to_parent()?;
        if parent.read().is_ok() {
            self.record_history();
            self.loc = parent;
            Some(prev)
        } else {
            None
        }
    }

    /// 任意の現在地へ移動する（パス入力・ジャンプ・ドライブ変更・ルート移動の共通口）。
    /// 侵入先が読めることを確認してから確定し、確定できたら履歴に積む。
    pub fn navigate(&mut self, loc: Location) -> bool {
        if loc.read().is_ok() {
            self.record_history();
            self.loc = loc;
            true
        } else {
            false
        }
    }

    /// 戻る。読めなくなった履歴は飛ばし、読める所まで遡る。移動できたら `true`。
    pub fn go_back(&mut self) -> bool {
        while let Some(prev) = self.back.pop() {
            if prev.read().is_ok() {
                self.forward.push(std::mem::replace(&mut self.loc, prev));
                return true;
            }
        }
        false
    }

    /// 進む。読めなくなった履歴は飛ばす。移動できたら `true`。
    pub fn go_forward(&mut self) -> bool {
        while let Some(next) = self.forward.pop() {
            if next.read().is_ok() {
                self.back.push(std::mem::replace(&mut self.loc, next));
                return true;
            }
        }
        false
    }

    /// 移動履歴（新しい順）を表示文字列で返す。先頭が直前の現在地。
    /// 履歴ダイアログ用。現在地そのものは含めない。
    pub fn history(&self) -> Vec<String> {
        self.back.iter().rev().map(|l| l.loc_display()).collect()
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
        assert!(p.enter("rerics-core", true));
        assert_eq!(p.path(), start.as_path());
    }

    #[test]
    fn pane_history_back_forward() {
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        let start = p.path().to_path_buf(); // .../crates/rerics-core
        assert_eq!(p.to_parent().as_deref(), Some("rerics-core"));
        let parent = p.path().to_path_buf(); // .../crates
        assert_ne!(start, parent);

        // 戻る→進む の往復。
        assert!(p.go_back());
        assert_eq!(p.path(), start);
        assert!(p.go_forward());
        assert_eq!(p.path(), parent);

        // 戻った後の navigate は forward を破棄する。
        assert!(p.go_back()); // -> start, forward=[parent]
        assert!(p.navigate(Location::Real(parent.clone())));
        assert_eq!(p.path(), parent);
        assert!(!p.go_forward(), "navigate 後は forward が空のはず");
        assert!(p.go_back());
        assert_eq!(p.path(), start);

        // これ以上は戻れない。
        assert!(!p.go_back());
    }

    #[test]
    fn pane_history_lists_visited_newest_first() {
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        let start_disp = p.loc_display();
        assert_eq!(p.to_parent().as_deref(), Some("rerics-core"));
        // 直前の現在地（rerics-core）が履歴の先頭に出る。
        assert_eq!(p.history().first().map(String::as_str), Some(start_disp.as_str()));
    }
}
