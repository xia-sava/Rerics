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
mod spinner;
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
    Bookmark, Config, DEFAULT_CONFIG_TOML, FontSpec, IconSettings, IconSize, ImageSettings,
    InputHistory, Layout, PATH_HISTORY_CAP, PATH_HISTORY_KEY, ResolvedTheme, State, TabState,
    Theme, ThemeColors,
    WheelAction, WindowState, clamp_to_work, config_path, data_dir, history_path, load_toml,
    save_toml, state_path,
};
pub use file_list::{
    SizeFormat, format_size_styled,
    Align, Colors, Column, ColumnKind, FileItem, FileListState, NameCase, Rgb, SeqCase, SortType,
    auto_adjust_columns, column_sample, default_columns, find_match, glob_match, read_items,
    sequence_rename,
};
pub use input::{Command, Invocation, KeyChord, KeyMap, vk};
pub use log::{LogLevel, LogLine, LogState};
pub use macros::{MacroAbort, MacroCtx, MacroHost, expand_macros};
pub use media::{
    AnimatedImage, Frame, FrameSource, MediaKind, Placement, StillImage, clamp_pan,
    composite_over_checker, decode_thumbnail, fit_scale, flip_rgba, load_image, placement,
    rgba_to_bgra, rgba_to_clipboard_dib, rotate_rgba, rotated_dims,
};
pub use operation::{
    ConflictResolution, DeleteWarnChoice, DirInfo, OpSummary, OperationHost, ProgressHandle,
    run_archive_add, run_archive_delete, run_archive_rebuild, run_archive_rename, run_calc_size,
    run_compress, run_copy, run_delete, run_extract,
};
pub use spinner::{SPINNER_FRAMES, Spinner};
pub use status::{format_drive, format_selected, format_size};
pub use vfs::{Location, is_archive_path};
pub use viewer::{DisplayLine, Encoding, LineEnding, ViewMode, ViewerModel, looks_binary};

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
    /// （パス表示文字列, そのディレクトリで最後にカーソルがあったファイル名）を
    /// 新しい順で末尾に持つ。ディレクトリへ再び入った時にカーソル位置を復元する。
    /// 上限を超えると先頭（古い方）から捨てる（セッション内のみ保持）。
    cursor_memory: Vec<(String, String)>,
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
            cursor_memory: Vec::new(),
        }
    }

    /// 表示文字列（実FS or "C:\foo.zip\inner"）から書庫境界を検出して復元する。
    /// セッション復元（state.toml）に使う。書庫が消えていれば実FS パスとして開く。
    pub fn restore(display: &str) -> Self {
        Self {
            loc: Location::parse(display),
            back: Vec::new(),
            forward: Vec::new(),
            cursor_memory: Vec::new(),
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
        self.navigate_reported(loc).is_ok()
    }

    /// [`navigate`](Self::navigate) と同じだが、失敗時は読めなかった理由（io エラー）を返す。
    /// 「存在しない」と「その他の失敗」でエラーダイアログの文言を切り分けるのに使う。
    pub fn navigate_reported(&mut self, loc: Location) -> std::io::Result<()> {
        loc.read()?;
        self.record_history();
        self.loc = loc;
        Ok(())
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

    /// 現在地を訪問履歴に記録する（移動の直前に呼ぶ）。`filename` はそのディレクトリで
    /// カーソルがあったファイル名で、先頭（".."）や空は「カーソル無し」として扱う。
    /// `limit == 0` は記録しない。カーソル無しでも訪れたディレクトリ自体は記録し（ドライブ
    /// 移動時の前回位置復元に使う）、ただし同じパスに実カーソル名の記録が既にあれば空名で
    /// 上書きせず最新位置（新しさ）だけ更新する。件数が `limit` を超えたら古い方から捨てる。
    pub fn remember_cursor(&mut self, filename: &str, limit: usize) {
        if limit == 0 {
            return;
        }
        let key = self.loc.loc_display();
        let name = if filename == ".." { "" } else { filename };
        let resolved = match self.cursor_memory.iter().position(|(k, _)| *k == key) {
            Some(pos) => {
                let (_, old) = self.cursor_memory.remove(pos);
                if name.is_empty() { old } else { name.to_owned() }
            }
            None => name.to_owned(),
        };
        self.cursor_memory.push((key, resolved));
        while self.cursor_memory.len() > limit {
            self.cursor_memory.remove(0);
        }
    }

    /// 指定パス表示に対して覚えているカーソルファイル名を返す（カーソル無しの訪問記録や
    /// 未知パスは None）。
    pub fn recalled_cursor(&self, path_display: &str) -> Option<&str> {
        self.cursor_memory
            .iter()
            .rev()
            .find(|(k, _)| k == path_display)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }

    /// 指定ドライブ（`drive` の先頭1文字を大小無視で判定）で最後に居たディレクトリの
    /// 表示文字列を、カーソル履歴の新しい方から探して返す（無ければ None）。
    /// ドライブを跨いだ時に「そのドライブで前回居た場所」へ戻すのに使う。
    pub fn recalled_drive_dir(&self, drive: &str) -> Option<&str> {
        let letter = drive.chars().next()?.to_ascii_uppercase();
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        self.cursor_memory
            .iter()
            .rev()
            .find(|(path, _)| {
                let mut cs = path.chars();
                matches!((cs.next(), cs.next()), (Some(c), Some(':')) if c.to_ascii_uppercase() == letter)
            })
            .map(|(path, _)| path.as_str())
    }

    /// 移動履歴（新しい順・重複除去）を表示文字列で返す。同じ場所を何度往復しても
    /// 各場所は最後に訪れた1件だけ出す（原作 移動履歴＝MyPathHistory と同じ）。
    /// 先頭が直前の現在地。現在地そのものは含めない。
    /// 戻る/進む用の `back` スタックは全系列を保ち、この表示でのみ畳む。
    pub fn history(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.back
            .iter()
            .rev()
            .map(|l| l.loc_display())
            .filter(|d| seen.insert(d.clone()))
            .collect()
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

    #[test]
    fn pane_history_dedups_repeated_visits() {
        // rerics-core ↔ crates を往復しても、移動履歴は各場所1件（最後の訪問位置）に畳む。
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        let rc = p.loc().clone(); // .../crates/rerics-core
        let crates = Location::Real(p.path().parent().unwrap().to_path_buf());
        assert!(p.navigate(crates.clone()));
        assert!(p.navigate(rc.clone()));
        assert!(p.navigate(crates.clone()));
        assert!(p.navigate(rc.clone()));
        // back=[rc, crates, rc, crates] → 表示は新しい順・重複除去で [crates, rc]。
        let hist = p.history();
        assert_eq!(hist.len(), 2, "重複は畳まれる: {hist:?}");
        let uniq: std::collections::HashSet<&String> = hist.iter().collect();
        assert_eq!(uniq.len(), hist.len(), "履歴に重複が残ってはいけない: {hist:?}");
        assert_eq!(hist[0], crates.loc_display(), "先頭は直前の現在地（crates）");

        // 戻る/進む用の back スタックは全系列を保つ＝4回ぶん戻れる。
        assert!(p.go_back()); // -> crates
        assert!(p.go_back()); // -> rc
        assert!(p.go_back()); // -> crates
        assert!(p.go_back()); // -> rc(開始)
        assert!(!p.go_back(), "これ以上は戻れない");
    }

    #[test]
    fn pane_cursor_memory_remembers_and_recalls() {
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        let here = p.loc_display();
        p.remember_cursor("Cargo.toml", 100);
        assert_eq!(p.recalled_cursor(&here), Some("Cargo.toml"));
        // 空名・親（".."）・limit==0 は記録しない（既存の記憶を上書きしない）。
        p.remember_cursor("", 100);
        p.remember_cursor("..", 100);
        p.remember_cursor("other.txt", 0);
        assert_eq!(p.recalled_cursor(&here), Some("Cargo.toml"));
        // 未知パスは None。
        assert_eq!(p.recalled_cursor("Z:\\nope"), None);
    }

    #[test]
    fn pane_cursor_memory_evicts_oldest_over_limit() {
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        let manifest = p.loc_display();
        p.remember_cursor("a", 2);
        assert!(p.enter("src", true), "src へ入れる");
        let src = p.loc_display();
        p.remember_cursor("b", 2);
        assert!(p.to_parent().is_some(), "親（manifest）へ戻れる");
        assert!(p.to_parent().is_some(), "さらに親（crates）へ");
        let crates = p.loc_display();
        p.remember_cursor("c", 2);
        // 上限 2：最も古い manifest が捨てられ、新しい src / crates は残る。
        assert_eq!(p.recalled_cursor(&manifest), None, "古い記録は破棄される");
        assert_eq!(p.recalled_cursor(&src), Some("b"));
        assert_eq!(p.recalled_cursor(&crates), Some("c"));
    }

    #[test]
    fn pane_cursor_memory_recalls_last_dir_on_drive() {
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        let manifest = p.loc_display();
        // リポジトリが置かれたドライブ文字を実パスから取る（環境非依存）。
        let drive = manifest[..2].to_owned(); // 例 "C:"
        p.remember_cursor("a", 100);
        assert!(p.enter("src", true), "src へ入れる");
        let src = p.loc_display();
        p.remember_cursor("b", 100);
        // 同ドライブで最後に居たディレクトリ（src）が返る。大小は無視。
        assert_eq!(p.recalled_drive_dir(&drive), Some(src.as_str()));
        assert_eq!(p.recalled_drive_dir(&drive.to_ascii_lowercase()), Some(src.as_str()));
        // 記憶に無いドライブは None。
        assert_eq!(p.recalled_drive_dir("Z:"), None);
        assert_eq!(p.recalled_drive_dir(""), None);
    }

    #[test]
    fn pane_drive_recall_includes_dirs_left_on_parent() {
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        let drive = p.loc_display()[..2].to_owned();
        // カーソルを動かさず（".."のまま）侵入即離脱したディレクトリも、
        // ドライブ移動の前回位置として引ける（カーソル名は無し）。
        assert!(p.enter("src", true), "src へ入れる");
        let src = p.loc_display();
        p.remember_cursor("..", 100);
        assert_eq!(p.recalled_drive_dir(&drive), Some(src.as_str()));
        assert_eq!(p.recalled_cursor(&src), None);
    }

    #[test]
    fn pane_remember_keeps_real_cursor_when_left_on_parent() {
        let mut p = Pane::open(env!("CARGO_MANIFEST_DIR"));
        assert!(p.enter("src", true), "src へ入れる");
        let src = p.loc_display();
        p.remember_cursor("lib.rs", 100);
        assert_eq!(p.recalled_cursor(&src), Some("lib.rs"));
        // 同じディレクトリを ".." のまま再離脱しても実カーソル記憶は消えない。
        p.remember_cursor("..", 100);
        assert_eq!(p.recalled_cursor(&src), Some("lib.rs"));
    }
}
