//! コマンドとキーバインドの土台（UI 非依存）。
//!
//! GUI 層はキー入力を [`KeyChord`] にして [`KeyMap::resolve`] で [`Command`] に解決し、
//! その Command を自前で実行する。全コマンドが自由にリマップ可能。

use std::collections::{BTreeMap, HashMap};

/// 仮想キーコード（Win32 VK と同値の `u16`。winsafe `co::VK` とも一致）。
pub mod vk {
    pub const BACK: u16 = 0x08;
    pub const RETURN: u16 = 0x0D;
    pub const SPACE: u16 = 0x20;
    pub const PRIOR: u16 = 0x21; // PageUp
    pub const NEXT: u16 = 0x22; // PageDown
    pub const END: u16 = 0x23;
    pub const HOME: u16 = 0x24;
    pub const LEFT: u16 = 0x25;
    pub const UP: u16 = 0x26;
    pub const RIGHT: u16 = 0x27;
    pub const DOWN: u16 = 0x28;
    pub const DELETE: u16 = 0x2E;
    pub const ESCAPE: u16 = 0x1B;
    pub const F1: u16 = 0x70;
    pub const F2: u16 = 0x71;
    pub const F3: u16 = 0x72;
    pub const F4: u16 = 0x73;
    pub const F5: u16 = 0x74;
    pub const F6: u16 = 0x75;
    pub const F7: u16 = 0x76;
    pub const F8: u16 = 0x77;
    pub const F9: u16 = 0x78;
    pub const F10: u16 = 0x79;
    pub const F11: u16 = 0x7A;
    pub const F12: u16 = 0x7B;
    // テンキー（NumPad）。`NUMPAD0..9` は 0x60..0x69、除算は DIVIDE。
    pub const NUMPAD0: u16 = 0x60;
    pub const NUMPAD1: u16 = 0x61;
    pub const NUMPAD2: u16 = 0x62;
    pub const NUMPAD3: u16 = 0x63;
    pub const NUMPAD4: u16 = 0x64;
    pub const NUMPAD5: u16 = 0x65;
    pub const NUMPAD6: u16 = 0x66;
    pub const NUMPAD7: u16 = 0x67;
    pub const NUMPAD8: u16 = 0x68;
    pub const NUMPAD9: u16 = 0x69;
    pub const ADD: u16 = 0x6B; // テンキー +
    pub const SUBTRACT: u16 = 0x6D; // テンキー -
    pub const DIVIDE: u16 = 0x6F;
    pub const TAB: u16 = 0x09;
    pub const A: u16 = 0x41;
    pub const B: u16 = 0x42;
    pub const C: u16 = 0x43;
    pub const D: u16 = 0x44;
    pub const E: u16 = 0x45;
    pub const F: u16 = 0x46;
    pub const H: u16 = 0x48;
    pub const I: u16 = 0x49;
    pub const J: u16 = 0x4A;
    pub const K: u16 = 0x4B;
    pub const L: u16 = 0x4C;
    pub const M: u16 = 0x4D;
    pub const N: u16 = 0x4E;
    pub const O: u16 = 0x4F;
    pub const P: u16 = 0x50;
    pub const Q: u16 = 0x51;
    pub const R: u16 = 0x52;
    pub const S: u16 = 0x53;
    pub const T: u16 = 0x54;
    pub const U: u16 = 0x55;
    pub const V: u16 = 0x56;
    pub const W: u16 = 0x57;
    pub const X: u16 = 0x58;
    pub const Y: u16 = 0x59;
    pub const Z: u16 = 0x5A;
    pub const D0: u16 = 0x30;
    pub const D1: u16 = 0x31;
    pub const D2: u16 = 0x32;
    pub const D3: u16 = 0x33;
    pub const D4: u16 = 0x34;
    pub const D5: u16 = 0x35;
    // 記号キー（JIS 配列前提。記号⇔VK の対応は配列依存なので、既定割当に使う際は
    // 実機の VK を確認してから割り当てる）。
    pub const OEM_1: u16 = 0xBA; // JIS: ":" "*"
    pub const OEM_PLUS: u16 = 0xBB; // JIS: ";" "+"
    pub const OEM_MINUS: u16 = 0xBD; // "-" "="
    pub const OEM_3: u16 = 0xC0; // JIS: "@" "`"
    pub const OEM_4: u16 = 0xDB; // JIS: "[" "{"
    pub const OEM_5: u16 = 0xDC; // JIS: "\\" "|"（￥）
    pub const OEM_6: u16 = 0xDD; // JIS: "]" "}"
}

/// ファイラのコマンド（段階的に拡張していく）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    CursorUp,
    CursorDown,
    CursorTop,
    CursorEnd,
    CursorPageUp,
    CursorPageDown,
    SetCursorPosition,
    EnterDir,
    View,
    ToParent,
    ToRoot,
    HistoryBack,
    HistoryForward,
    PathHistoryDialog,
    ChangeDirectory,
    ChangeDirectoryDialog,
    ChangeDrive,
    ChangeDriveDialog,
    JumpDialog,
    RegisterPath,
    IncrementalSearchDialog,
    DirectoryInformation,
    RenameSequenceDialog,
    FocusLeft,
    FocusRight,
    MarkToggle,
    SelectAll,
    ClearAll,
    ReverseAll,
    SelectAllFile,
    ReverseAllFile,
    Reload,
    SortByName,
    SortByExtension,
    SortBySize,
    SortByDate,
    Sort,
    SortDialog,
    SortReverseToggle,
    PageNext,
    PagePrevious,
    NewTab,
    CloseTab,
    MakeDirectory,
    Copy,
    Move,
    SwapPath,
    OppositeToCurrent,
    CurrentToOpposite,
    Rename,
    Delete,
    SendToRecycled,
    CreateShortcut,
    ClipCopy,
    ClipCut,
    ClipPaste,
    CreateFile,
    ViewFile,
    Edit,
    PropertyDialog,
    Compress,
    Extract,
    NextDrive,
    PreviousDrive,
    PathMask,
    SelectMask,
    OpenTaskManager,
    OpenSettings,
    KeyBindsDialog,
    CommandDirect,
    CopyLog,
    ClearLog,
    MaximizeLeft,
    MaximizeRight,
    MaximizeLeftForce,
    MaximizeRightForce,
    BorderLeft,
    BorderRight,
    BorderReset,
    CursorOpposite,
    SelectFile,
    Refresh,
    Nop,
    MaximizeCurrent,
    MaximizeWindow,
    MinimizeWindow,
    End,
    Restart,
    Quit,
    /// 登録済みスクリプトコマンドを名前で実行する（引数＝コマンド名）。
    Script,
    /// 任意のスクリプトコードを評価する（引数＝ソース）。
    Eval,
    // テキストビューア
    ViewerClose,
    ViewerScrollUp,
    ViewerScrollDown,
    ViewerPageUp,
    ViewerPageDown,
    ViewerScrollTop,
    ViewerScrollBottom,
    ViewerSearchDialog,
    ViewerFindNext,
    ViewerFindPrevious,
    ViewerSelectAll,
    ViewerToggleMode,
    ViewerChangeEncoding,
    ViewerCopy,
    ViewerContextMenu,
    // 画像・動画ビューア
    ImageNext,
    ImagePrevious,
    ImageZoomIn,
    ImageZoomOut,
    ImageFitWindow,
    ImageActualSize,
    ImageFitWidth,
    ImageFitHeight,
    ImageFitLarge,
    ImageRotateRight,
    ImageRotateLeft,
    ImageFlipHorizontal,
    ImageFlipVertical,
    ImageCopy,
    MediaTogglePlay,
}

/// コマンドが有効な文脈。設定 UI のキー編集ページをこの単位で分ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandContext {
    Filer,
    TextViewer,
    ImageViewer,
}

impl Command {
    /// コマンドと「設定トークン名・表示名（日本語）」の対応表（変換の単一の出どころ）。
    const ALL: &'static [(Command, &'static str, &'static str)] = {
        use Command::*;
        &[
            (CursorUp, "CursorUp", "カーソルを上へ"),
            (CursorDown, "CursorDown", "カーソルを下へ"),
            (CursorTop, "CursorTop", "先頭行へ移動"),
            (CursorEnd, "CursorEnd", "最終行へ移動"),
            (CursorPageUp, "CursorPageUp", "1ページ上へ"),
            (CursorPageDown, "CursorPageDown", "1ページ下へ"),
            (SetCursorPosition, "SetCursorPosition", "指定位置へカーソル移動"),
            (EnterDir, "EnterDir", "開く（ディレクトリ・書庫へ）"),
            (View, "View", "ビューアで表示"),
            (ToParent, "ToParent", "親ディレクトリへ移動"),
            (ToRoot, "ToRoot", "ルートディレクトリへ移動"),
            (HistoryBack, "HistoryBack", "履歴を1つ戻る"),
            (HistoryForward, "HistoryForward", "履歴を1つ進める"),
            (PathHistoryDialog, "PathHistoryDialog", "ディレクトリ履歴"),
            (ChangeDirectory, "ChangeDirectory", "ディレクトリの変更"),
            (ChangeDirectoryDialog, "ChangeDirectoryDialog", "ディレクトリ変更ダイアログ"),
            (ChangeDrive, "ChangeDrive", "ドライブの変更"),
            (ChangeDriveDialog, "ChangeDriveDialog", "ドライブリスト"),
            (JumpDialog, "JumpDialog", "登録ディレクトリ"),
            (RegisterPath, "RegisterPath", "登録ディレクトリに追加"),
            (IncrementalSearchDialog, "IncrementalSearchDialog", "インクリメンタルサーチ"),
            (DirectoryInformation, "DirectoryInformation", "ディレクトリの容量計算"),
            (RenameSequenceDialog, "RenameSequenceDialog", "連番リネーム"),
            (FocusLeft, "FocusLeft", "左パスへ移動"),
            (FocusRight, "FocusRight", "右パスへ移動"),
            (MarkToggle, "MarkToggle", "選択／解除（カーソル位置）"),
            (SelectAll, "SelectAll", "すべて選択"),
            (ClearAll, "ClearAll", "すべて選択解除"),
            (ReverseAll, "ReverseAll", "すべて選択反転"),
            (SelectAllFile, "SelectAllFile", "ファイルのみ選択"),
            (ReverseAllFile, "ReverseAllFile", "ファイルのみ選択反転"),
            (Reload, "Reload", "最新の情報に更新"),
            (SortByName, "SortByName", "名前順で並べ替え"),
            (SortByExtension, "SortByExtension", "拡張子順で並べ替え"),
            (SortBySize, "SortBySize", "サイズ順で並べ替え"),
            (SortByDate, "SortByDate", "日付順で並べ替え"),
            (Sort, "Sort", "並べ替え方法の指定"),
            (SortDialog, "SortDialog", "ソート設定"),
            (SortReverseToggle, "SortReverseToggle", "昇順／降順を反転"),
            (PageNext, "PageNext", "次のタブへ"),
            (PagePrevious, "PagePrevious", "前のタブへ"),
            (NewTab, "NewTab", "新しいタブ"),
            (CloseTab, "CloseTab", "タブを閉じる"),
            (MakeDirectory, "MakeDirectory", "ディレクトリの作成"),
            (Copy, "Copy", "コピー"),
            (Move, "Move", "移動"),
            (SwapPath, "SwapPath", "左右のパスを入れ替え"),
            (OppositeToCurrent, "OppositeToCurrent", "反対側をカレントと同じに"),
            (CurrentToOpposite, "CurrentToOpposite", "カレントを反対側と同じに"),
            (Rename, "Rename", "名前の変更"),
            (Delete, "Delete", "削除"),
            (SendToRecycled, "SendToRecycled", "ごみ箱へ送る"),
            (CreateShortcut, "CreateShortcut", "ショートカットの作成"),
            (ClipCopy, "ClipCopy", "クリップボードにコピー"),
            (ClipCut, "ClipCut", "クリップボードに切り取り"),
            (ClipPaste, "ClipPaste", "クリップボードから貼り付け"),
            (CreateFile, "CreateFile", "新規ファイルの作成"),
            (ViewFile, "ViewFile", "ビューアで開く"),
            (Edit, "Edit", "エディタで開く"),
            (PropertyDialog, "PropertyDialog", "プロパティの表示"),
            (Compress, "Compress", "圧縮"),
            (Extract, "Extract", "解凍"),
            (NextDrive, "NextDrive", "次のドライブへ"),
            (PreviousDrive, "PreviousDrive", "前のドライブへ"),
            (PathMask, "PathMask", "パスマスク"),
            (SelectMask, "SelectMask", "マスクで選択"),
            (OpenTaskManager, "OpenTaskManager", "タスクマネージャ"),
            (OpenSettings, "OpenSettings", "設定を開く"),
            (KeyBindsDialog, "KeyBindsDialog", "キーバインドリスト"),
            (CommandDirect, "CommandDirect", "任意のコマンドを実行"),
            (CopyLog, "CopyLog", "ログをコピー"),
            (ClearLog, "ClearLog", "ログクリア"),
            (MaximizeLeft, "MaximizeLeft", "左リストを最大化"),
            (MaximizeRight, "MaximizeRight", "右リストを最大化"),
            (MaximizeLeftForce, "MaximizeLeftForce", "左リストを最大化（強制）"),
            (MaximizeRightForce, "MaximizeRightForce", "右リストを最大化（強制）"),
            (BorderLeft, "BorderLeft", "境界線を左へ"),
            (BorderRight, "BorderRight", "境界線を右へ"),
            (BorderReset, "BorderReset", "境界線を定位置へ"),
            (CursorOpposite, "CursorOpposite", "反対側パスへ移動"),
            (SelectFile, "SelectFile", "ファイルを選択（カーソル位置）"),
            (Refresh, "Refresh", "表示を更新"),
            (Nop, "Nop", "無効コマンド"),
            (MaximizeCurrent, "MaximizeCurrent", "現在のリストを最大化"),
            (MaximizeWindow, "MaximizeWindow", "ウィンドウの最大化"),
            (MinimizeWindow, "MinimizeWindow", "ウィンドウの最小化"),
            (End, "End", "アプリケーションの終了"),
            (Restart, "Restart", "再起動"),
            (Quit, "Quit", "閉じる（最後ならアプリ終了）"),
            (Script, "Script", "スクリプト実行"),
            (Eval, "Eval", "コード評価"),
            (ViewerClose, "ViewerClose", "ビューアを閉じる"),
            (ViewerScrollUp, "ViewerScrollUp", "1行上へ"),
            (ViewerScrollDown, "ViewerScrollDown", "1行下へ"),
            (ViewerPageUp, "ViewerPageUp", "1ページ上へ"),
            (ViewerPageDown, "ViewerPageDown", "1ページ下へ"),
            (ViewerScrollTop, "ViewerScrollTop", "先頭行へ移動"),
            (ViewerScrollBottom, "ViewerScrollBottom", "最終行へ移動"),
            (ViewerSearchDialog, "ViewerSearchDialog", "検索"),
            (ViewerFindNext, "ViewerFindNext", "次を検索"),
            (ViewerFindPrevious, "ViewerFindPrevious", "前を検索"),
            (ViewerSelectAll, "ViewerSelectAll", "すべて選択"),
            (ViewerToggleMode, "ViewerToggleMode", "バイナリ／テキスト切替"),
            (ViewerChangeEncoding, "ViewerChangeEncoding", "文字コードを切替"),
            (ViewerCopy, "ViewerCopy", "選択範囲をコピー"),
            (ViewerContextMenu, "ViewerContextMenu", "コンテキストメニュー"),
            (ImageNext, "ImageNext", "次の画像"),
            (ImagePrevious, "ImagePrevious", "前の画像"),
            (ImageZoomIn, "ImageZoomIn", "拡大"),
            (ImageZoomOut, "ImageZoomOut", "縮小"),
            (ImageFitWindow, "ImageFitWindow", "画面に合わせて縮小"),
            (ImageActualSize, "ImageActualSize", "等倍表示"),
            (ImageFitWidth, "ImageFitWidth", "横幅に合わせる"),
            (ImageFitHeight, "ImageFitHeight", "縦幅に合わせる"),
            (ImageFitLarge, "ImageFitLarge", "なるべく大きく表示"),
            (ImageRotateRight, "ImageRotateRight", "右回転"),
            (ImageRotateLeft, "ImageRotateLeft", "左回転"),
            (ImageFlipHorizontal, "ImageFlipHorizontal", "左右反転"),
            (ImageFlipVertical, "ImageFlipVertical", "上下反転"),
            (ImageCopy, "ImageCopy", "クリップボードにコピー"),
            (MediaTogglePlay, "MediaTogglePlay", "再生／一時停止"),
        ]
    };

    /// 設定トークン名を返す。
    pub fn as_token(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(c, _, _)| *c == self)
            .map(|(_, s, _)| *s)
            .unwrap_or("")
    }

    /// 表示名（日本語）を返す。UI でユーザに見せる親しみやすい名前。
    pub fn display_name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(c, _, _)| *c == self)
            .map(|(_, _, l)| *l)
            .unwrap_or("")
    }

    /// 設定トークン名から解釈する。
    pub fn from_token(s: &str) -> Option<Command> {
        Self::ALL
            .iter()
            .find(|(_, t, _)| *t == s)
            .map(|(c, _, _)| *c)
    }

    /// 全コマンドを列挙する（設定 UI 用）。
    pub fn all() -> impl Iterator<Item = Command> {
        Self::ALL.iter().map(|(c, _, _)| *c)
    }

    /// このコマンドが有効な文脈を返す。設定 UI はこれでページごとに候補を絞る。
    pub fn contexts(self) -> &'static [CommandContext] {
        use Command::*;
        use CommandContext::*;
        match self {
            ViewerScrollUp | ViewerScrollDown | ViewerPageUp | ViewerPageDown | ViewerScrollTop
            | ViewerScrollBottom | ViewerSearchDialog | ViewerFindNext | ViewerFindPrevious
            | ViewerSelectAll | ViewerToggleMode | ViewerChangeEncoding | ViewerCopy
            | ViewerContextMenu => &[TextViewer],
            ViewerClose => &[TextViewer, ImageViewer],
            ImageNext | ImagePrevious | ImageZoomIn | ImageZoomOut | ImageFitWindow
            | ImageActualSize | ImageFitWidth | ImageFitHeight | ImageFitLarge
            | ImageRotateRight | ImageRotateLeft | ImageFlipHorizontal
            | ImageFlipVertical | ImageCopy | MediaTogglePlay => &[ImageViewer],
            Edit | OpenSettings => &[Filer, TextViewer],
            // スクリプト系は引数（コマンド名／コード）込みでしか意味をなさず、現状のキー編集 UI
            // からは直接選べない。設定トークンとしては有効なので列挙だけ避ける。
            Script | Eval => &[],
            _ => &[Filer],
        }
    }

    /// このコマンドが指定文脈で有効か。
    pub fn available_in(self, ctx: CommandContext) -> bool {
        self.contexts().contains(&ctx)
    }
}

/// 「コマンド＋引数」一回分の呼び出し。キーバインド・メニュー・スクリプトの共通入口。
///
/// 引数なしコマンドは `args` が空。引数文字列は生の値で、先頭が `=` なら TS 式（実行直前に
/// エンジンで評価）、それ以外はリテラルとして使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    pub args: Vec<String>,
}

impl Invocation {
    /// 引数なしの呼び出し。
    pub fn bare(command: Command) -> Self {
        Self { command, args: Vec::new() }
    }

    /// 引数つきの呼び出し。
    pub fn new(command: Command, args: Vec<String>) -> Self {
        Self { command, args }
    }

    /// 設定トークンを解釈する。`Name`（引数なし）と `Name("a", "b")`（引数つき）に対応。
    /// 引数はダブルクォート区切り・`\"`/`\\` エスケープ可。解釈できなければ `None`。
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        match s.find('(') {
            None => Some(Self::bare(Command::from_token(s)?)),
            Some(open) => {
                if !s.ends_with(')') {
                    return None;
                }
                let command = Command::from_token(s[..open].trim())?;
                let args = parse_arg_list(&s[open + 1..s.len() - 1])?;
                Some(Self { command, args })
            }
        }
    }

    /// 設定トークンへ変換する。引数なしは `Name`（＝従来表記・後方互換）、ありは `Name("a", "b")`。
    pub fn to_token_string(&self) -> String {
        let name = self.command.as_token();
        if self.args.is_empty() {
            return name.to_owned();
        }
        let quoted: Vec<String> = self
            .args
            .iter()
            .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        format!("{}({})", name, quoted.join(", "))
    }
}

impl From<Command> for Invocation {
    fn from(command: Command) -> Self {
        Self::bare(command)
    }
}

/// `"a", "b"` 形式（括弧の中身）をダブルクォート区切りの引数列に分解する。
/// 空（空白のみ）なら空配列。文法に合わなければ `None`。
fn parse_arg_list(s: &str) -> Option<Vec<String>> {
    let s = s.trim();
    if s.is_empty() {
        return Some(Vec::new());
    }
    let mut args = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.next()? != '"' {
            return None;
        }
        let mut buf = String::new();
        loop {
            match chars.next()? {
                '\\' => match chars.next()? {
                    '"' => buf.push('"'),
                    '\\' => buf.push('\\'),
                    other => {
                        buf.push('\\');
                        buf.push(other);
                    }
                },
                '"' => break,
                c => buf.push(c),
            }
        }
        args.push(buf);
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.next() {
            None => break,
            Some(',') => continue,
            Some(_) => return None,
        }
    }
    Some(args)
}

/// 特殊キーの VK ⇔ トークン名の対応表。英数字は別途生成する。
const KEY_NAMES: &[(u16, &str)] = &[
    (vk::BACK, "BackSpace"),
    (vk::RETURN, "Enter"),
    (vk::SPACE, "Space"),
    (vk::PRIOR, "PageUp"),
    (vk::NEXT, "PageDown"),
    (vk::END, "End"),
    (vk::HOME, "Home"),
    (vk::LEFT, "Left"),
    (vk::UP, "Up"),
    (vk::RIGHT, "Right"),
    (vk::DOWN, "Down"),
    (vk::DELETE, "Del"),
    (vk::ESCAPE, "Esc"),
    (vk::F1, "F1"),
    (vk::F2, "F2"),
    (vk::F3, "F3"),
    (vk::F4, "F4"),
    (vk::F5, "F5"),
    (vk::F6, "F6"),
    (vk::F7, "F7"),
    (vk::F8, "F8"),
    (vk::F9, "F9"),
    (vk::F10, "F10"),
    (vk::F11, "F11"),
    (vk::F12, "F12"),
    (vk::NUMPAD0, "NumPad0"),
    (vk::NUMPAD1, "NumPad1"),
    (vk::NUMPAD2, "NumPad2"),
    (vk::NUMPAD3, "NumPad3"),
    (vk::NUMPAD4, "NumPad4"),
    (vk::NUMPAD5, "NumPad5"),
    (vk::NUMPAD6, "NumPad6"),
    (vk::NUMPAD7, "NumPad7"),
    (vk::NUMPAD8, "NumPad8"),
    (vk::NUMPAD9, "NumPad9"),
    (vk::ADD, "NumPad+"),
    (vk::SUBTRACT, "NumPad-"),
    (vk::DIVIDE, "NumPad/"),
    (vk::TAB, "Tab"),
    (vk::OEM_1, ":"),
    (vk::OEM_PLUS, ";"),
    (vk::OEM_MINUS, "-"),
    (vk::OEM_3, "@"),
    (vk::OEM_4, "["),
    (vk::OEM_5, "\\"),
    (vk::OEM_6, "]"),
];

/// VK をトークン名へ変換する（A-Z/0-9 はその文字）。
fn vk_to_name(vk: u16) -> Option<String> {
    if let Some((_, n)) = KEY_NAMES.iter().find(|(v, _)| *v == vk) {
        return Some((*n).to_owned());
    }
    if (0x41..=0x5A).contains(&vk) || (0x30..=0x39).contains(&vk) {
        return Some((vk as u8 as char).to_string());
    }
    None
}

/// トークン名を VK へ変換する。
fn name_to_vk(name: &str) -> Option<u16> {
    if let Some((v, _)) = KEY_NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)) {
        return Some(*v);
    }
    if name.len() == 1 {
        let c = name.chars().next().unwrap().to_ascii_uppercase();
        if c.is_ascii_alphabetic() || c.is_ascii_digit() {
            return Some(c as u16);
        }
    }
    None
}

/// キー＋修飾の組（将来 Ctrl/Shift/Alt も区別する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub vk: u16,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl KeyChord {
    /// 修飾なしの単キー。
    pub const fn key(vk: u16) -> Self {
        Self {
            vk,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    /// 修飾キー付きのチョード。
    pub const fn new(vk: u16, ctrl: bool, shift: bool, alt: bool) -> Self {
        Self { vk, ctrl, shift, alt }
    }

    /// `"Ctrl+Shift+Tab"` のような設定トークンを解釈する（修飾子は順不同・大小無視）。
    /// 先頭から既知の修飾子（`Ctrl+`/`Shift+`/`Alt+`）だけを順に剥がし、残りをキー名とする
    /// （キー名自体が `+` を含む `NumPad+` 等を `+` で割らないため）。
    pub fn parse(s: &str) -> Option<Self> {
        let mut rest = s.trim();
        let (mut ctrl, mut shift, mut alt) = (false, false, false);
        loop {
            let lower = rest.to_ascii_lowercase();
            if lower.starts_with("ctrl+") {
                ctrl = true;
                rest = rest[5..].trim_start();
            } else if lower.starts_with("control+") {
                ctrl = true;
                rest = rest[8..].trim_start();
            } else if lower.starts_with("shift+") {
                shift = true;
                rest = rest[6..].trim_start();
            } else if lower.starts_with("alt+") {
                alt = true;
                rest = rest[4..].trim_start();
            } else {
                break;
            }
        }
        let vk = name_to_vk(rest.trim())?;
        Some(Self { vk, ctrl, shift, alt })
    }

    /// `"Ctrl+Shift+Tab"` のような設定トークンへ変換する（未知キーは `None`）。
    pub fn to_token(&self) -> Option<String> {
        let name = vk_to_name(self.vk)?;
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        s.push_str(&name);
        Some(s)
    }
}

/// キー → コマンドの対応表。
///
/// [`KeyMap::default`] がデフォルトバインド一式、[`KeyMap::new`] が空のマップを返す。
#[derive(Debug, Clone)]
pub struct KeyMap {
    map: HashMap<KeyChord, Invocation>,
}

impl Default for KeyMap {
    /// デフォルトのキーバインド（実装済みコマンド分のみ）。未実装/引数付きの割当は
    /// 載せず、コマンド実装時に活性化する。個人設定は config.toml で上乗せする。
    fn default() -> Self {
        use Command::*;
        let mut m = Self::new();
        // カーソル移動。
        m.bind(KeyChord::key(vk::UP), CursorUp);
        m.bind(KeyChord::key(vk::DOWN), CursorDown);
        m.bind(KeyChord::key(vk::PRIOR), CursorPageUp);
        m.bind(KeyChord::key(vk::NEXT), CursorPageDown);
        m.bind(KeyChord::new(vk::HOME, true, false, false), CursorTop);
        m.bind(KeyChord::new(vk::END, true, false, false), CursorEnd);
        // Shift＋上下/PageUp/Down＝選択しながら移動。
        for (vk, cmd) in [
            (vk::UP, CursorUp),
            (vk::DOWN, CursorDown),
            (vk::PRIOR, CursorPageUp),
            (vk::NEXT, CursorPageDown),
        ] {
            m.bind_inv(
                KeyChord::new(vk, false, true, false),
                Invocation::new(cmd, vec!["select".into()]),
            );
        }
        // 侵入・親・ルート・履歴・フォーカス。
        m.bind(KeyChord::key(vk::RETURN), EnterDir);
        m.bind(KeyChord::key(vk::BACK), ToParent);
        m.bind(KeyChord::key(vk::OEM_5), ToRoot);
        m.bind(KeyChord::new(vk::LEFT, false, false, true), HistoryBack);
        m.bind(KeyChord::new(vk::RIGHT, false, false, true), HistoryForward);
        m.bind(KeyChord::key(vk::LEFT), FocusLeft);
        m.bind(KeyChord::key(vk::RIGHT), FocusRight);
        m.bind_inv(
            KeyChord::key(vk::F4),
            Invocation::new(ChangeDirectory, vec!["=r.prompt(\"ディレクトリの入力\")".into()]),
        );
        m.bind_inv(
            KeyChord::new(vk::F4, false, true, false),
            Invocation::new(ChangeDirectory, vec!["=r.folderDialog(\"ディレクトリの選択\")".into()]),
        );
        m.bind(KeyChord::key(vk::J), JumpDialog);
        m.bind(KeyChord::key(vk::Z), CommandDirect);
        // 選択。
        m.bind(KeyChord::key(vk::SPACE), MarkToggle);
        // Shift+Space＝反転＋カーソル上移動。
        m.bind_inv(
            KeyChord::new(vk::SPACE, false, true, false),
            Invocation::new(MarkToggle, vec!["-1".into()]),
        );
        m.bind(KeyChord::key(vk::A), ReverseAllFile);
        m.bind(KeyChord::new(vk::A, false, true, false), ReverseAll);
        m.bind(KeyChord::new(vk::A, true, false, false), SelectAll);
        m.bind(KeyChord::key(vk::HOME), ClearAll);
        // 検索。
        m.bind(KeyChord::key(vk::F), IncrementalSearchDialog);
        // ファイル操作。
        m.bind(KeyChord::key(vk::C), Copy);
        m.bind(KeyChord::key(vk::M), Move);
        m.bind(KeyChord::key(vk::D), Delete);
        m.bind(KeyChord::key(vk::DELETE), SendToRecycled);
        m.bind(KeyChord::key(vk::T), CreateShortcut);
        m.bind(KeyChord::new(vk::C, true, false, false), ClipCopy);
        m.bind(KeyChord::new(vk::X, true, false, false), ClipCut);
        m.bind(KeyChord::new(vk::V, true, false, false), ClipPaste);
        m.bind(KeyChord::key(vk::R), Rename);
        m.bind(KeyChord::key(vk::F2), Rename);
        m.bind(KeyChord::new(vk::R, false, true, false), RenameSequenceDialog);
        m.bind(KeyChord::key(vk::K), MakeDirectory);
        m.bind(KeyChord::key(vk::P), Compress);
        m.bind(KeyChord::key(vk::U), Extract);
        // 表示・ペイン。
        m.bind(KeyChord::key(vk::V), ViewFile);
        m.bind(KeyChord::key(vk::E), Edit);
        m.bind(KeyChord::new(vk::RETURN, false, false, true), PropertyDialog);
        m.bind(KeyChord::key(vk::O), OppositeToCurrent);
        m.bind(KeyChord::new(vk::O, false, true, false), CurrentToOpposite);
        m.bind(KeyChord::new(vk::RIGHT, true, false, false), MaximizeLeft);
        m.bind(KeyChord::new(vk::LEFT, true, false, false), MaximizeRight);
        m.bind(KeyChord::key(vk::Y), PathMask);
        m.bind(KeyChord::new(vk::P, false, true, false), PathMask);
        m.bind(KeyChord::key(vk::S), SortDialog);
        // ドライブ切替。
        m.bind(KeyChord::new(vk::LEFT, false, true, false), PreviousDrive);
        m.bind(KeyChord::new(vk::RIGHT, false, true, false), NextDrive);
        // 情報・システム。
        m.bind(KeyChord::key(vk::I), DirectoryInformation);
        m.bind(KeyChord::key(vk::ESCAPE), OpenTaskManager);
        m.bind(KeyChord::new(vk::HOME, false, true, false), Reload);
        m.bind(KeyChord::key(vk::F5), Reload);
        m.bind(KeyChord::new(vk::F1, false, true, false), OpenSettings);
        m.bind(KeyChord::key(vk::Q), Quit);
        m.bind(KeyChord::new(vk::Q, false, true, false), Restart);
        // ウィンドウ操作。
        m.bind(KeyChord::new(vk::PRIOR, false, false, true), MaximizeWindow);
        m.bind(KeyChord::new(vk::NEXT, false, false, true), MinimizeWindow);
        // タブ操作。
        m.bind(KeyChord::new(vk::TAB, true, false, false), PageNext);
        m.bind(KeyChord::new(vk::TAB, true, true, false), PagePrevious);
        m.bind(KeyChord::new(vk::T, true, false, false), NewTab);
        m.bind(KeyChord::new(vk::W, true, false, false), CloseTab);
        m
    }
}

impl KeyMap {
    /// バインドが空のマップを作る。
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    /// テキストビューアの既定キーバインド（実装済みコマンド分）。
    /// 横スクロール（Left/Right）は折返し表示のため割り当てない。個人設定は
    /// config.toml の `[keybinds_textviewer]` で上乗せする。
    pub fn default_textviewer() -> Self {
        use Command::*;
        let mut m = Self::new();
        // 終了（Enter / Esc / Q）。
        m.bind(KeyChord::key(vk::RETURN), ViewerClose);
        m.bind(KeyChord::key(vk::ESCAPE), ViewerClose);
        m.bind(KeyChord::key(vk::Q), ViewerClose);
        // スクロール（↑↓ / PageUp/Down / Home/End、Ctrl で先頭・末尾）。
        m.bind(KeyChord::key(vk::UP), ViewerScrollUp);
        m.bind(KeyChord::key(vk::DOWN), ViewerScrollDown);
        m.bind(KeyChord::key(vk::PRIOR), ViewerPageUp);
        m.bind(KeyChord::key(vk::NEXT), ViewerPageDown);
        m.bind(KeyChord::new(vk::PRIOR, true, false, false), ViewerScrollTop);
        m.bind(KeyChord::new(vk::NEXT, true, false, false), ViewerScrollBottom);
        m.bind(KeyChord::key(vk::HOME), ViewerScrollTop);
        m.bind(KeyChord::new(vk::HOME, true, false, false), ViewerScrollTop);
        m.bind(KeyChord::key(vk::END), ViewerScrollBottom);
        m.bind(KeyChord::new(vk::END, true, false, false), ViewerScrollBottom);
        // 選択・コピー（Ctrl+A / Ctrl+C）。
        m.bind(KeyChord::new(vk::A, true, false, false), ViewerSelectAll);
        m.bind(KeyChord::new(vk::C, true, false, false), ViewerCopy);
        // 表示モード・文字コード（B / C）。
        m.bind(KeyChord::key(vk::B), ViewerToggleMode);
        m.bind(KeyChord::key(vk::C), ViewerChangeEncoding);
        // 検索（Ctrl+F / Ctrl+S・次=N・前=P）。
        m.bind(KeyChord::new(vk::F, true, false, false), ViewerSearchDialog);
        m.bind(KeyChord::new(vk::S, true, false, false), ViewerSearchDialog);
        m.bind(KeyChord::key(vk::N), ViewerFindNext);
        m.bind(KeyChord::key(vk::P), ViewerFindPrevious);
        // エディタ起動（E）。
        m.bind(KeyChord::key(vk::E), Edit);
        m
    }

    /// 画像・動画ビューアの既定キーバインド。個人設定は config.toml の
    /// `[keybinds_imageviewer]` で上乗せする。
    pub fn default_imageviewer() -> Self {
        use Command::*;
        let mut m = Self::new();
        // 終了（Enter / Esc / Q）。
        m.bind(KeyChord::key(vk::RETURN), ViewerClose);
        m.bind(KeyChord::key(vk::ESCAPE), ViewerClose);
        m.bind(KeyChord::key(vk::Q), ViewerClose);
        // 再生／一時停止（動画・Space）。
        m.bind(KeyChord::key(vk::SPACE), MediaTogglePlay);
        // 前後送り（←↑PageUp＝前・→↓PageDown＝次）。
        m.bind(KeyChord::key(vk::LEFT), ImagePrevious);
        m.bind(KeyChord::key(vk::UP), ImagePrevious);
        m.bind(KeyChord::key(vk::PRIOR), ImagePrevious);
        m.bind(KeyChord::key(vk::RIGHT), ImageNext);
        m.bind(KeyChord::key(vk::DOWN), ImageNext);
        m.bind(KeyChord::key(vk::NEXT), ImageNext);
        // 拡大・縮小（+ / -、テンキー +/- も）。
        m.bind(KeyChord::key(vk::OEM_PLUS), ImageZoomIn);
        m.bind(KeyChord::key(vk::ADD), ImageZoomIn);
        m.bind(KeyChord::key(vk::OEM_MINUS), ImageZoomOut);
        m.bind(KeyChord::key(vk::SUBTRACT), ImageZoomOut);
        m.bind(KeyChord::key(vk::Z), ImageZoomIn);
        m.bind(KeyChord::key(vk::X), ImageZoomOut);
        // 表示モード（1＝原寸・2＝全体・3＝幅に合わせる・4＝高さに合わせる・5＝なるべく大きく）。
        m.bind(KeyChord::key(vk::D1), ImageActualSize);
        m.bind(KeyChord::key(vk::D2), ImageFitWindow);
        m.bind(KeyChord::key(vk::D3), ImageFitWidth);
        m.bind(KeyChord::key(vk::D4), ImageFitHeight);
        m.bind(KeyChord::key(vk::D5), ImageFitLarge);
        // 回転・反転（R＝右回転・L＝左回転・V＝左右反転・H＝上下反転）。
        m.bind(KeyChord::key(vk::R), ImageRotateRight);
        m.bind(KeyChord::key(vk::L), ImageRotateLeft);
        m.bind(KeyChord::key(vk::V), ImageFlipHorizontal);
        m.bind(KeyChord::key(vk::H), ImageFlipVertical);
        // クリップボードへコピー（Ctrl+C）。
        m.bind(KeyChord::new(vk::C, true, false, false), ImageCopy);
        m
    }

    /// 引数なしコマンドを割り当てる（既定マップ記述用の簡易版）。
    pub fn bind(&mut self, chord: KeyChord, cmd: Command) -> &mut Self {
        self.map.insert(chord, Invocation::bare(cmd));
        self
    }

    /// 引数つきの呼び出しを割り当てる。
    pub fn bind_inv(&mut self, chord: KeyChord, inv: Invocation) -> &mut Self {
        self.map.insert(chord, inv);
        self
    }

    pub fn unbind(&mut self, chord: &KeyChord) {
        self.map.remove(chord);
    }

    /// 割り当てられたコマンドだけを返す（引数は見ない簡易問い合わせ・テスト/内観用）。
    pub fn resolve(&self, chord: &KeyChord) -> Option<Command> {
        self.map.get(chord).map(|inv| inv.command)
    }

    /// 割り当てられた呼び出し（コマンド＋引数）を返す。実行配線はこちらを使う。
    pub fn resolve_inv(&self, chord: &KeyChord) -> Option<&Invocation> {
        self.map.get(chord)
    }

    /// トークン文字列のマップ（チョード→呼び出し）からキーマップを組む。
    /// 解釈できない行は無視する。**値が空文字のキーは未バインド**にする（既定を差分マージで
    /// 上書きしたうえでここで読み飛ばす＝ユーザ config で既定キーを潰す手段）。
    pub fn from_string_map(map: &BTreeMap<String, String>) -> Self {
        let mut m = Self::new();
        for (k, v) in map {
            if v.trim().is_empty() {
                continue;
            }
            if let (Some(chord), Some(inv)) = (KeyChord::parse(k), Invocation::parse(v)) {
                m.bind_inv(chord, inv);
            }
        }
        m
    }

    /// トークン文字列のマップ（チョード→呼び出し）へ変換する。
    pub fn to_string_map(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (chord, inv) in &self.map {
            if let Some(tok) = chord.to_token() {
                out.insert(tok, inv.to_token_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_binds_cursor_keys() {
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::DOWN)), Some(Command::CursorDown));
        assert_eq!(m.resolve(&KeyChord::key(vk::UP)), Some(Command::CursorUp));
        assert_eq!(m.resolve(&KeyChord::key(vk::RETURN)), Some(Command::EnterDir));
        assert_eq!(m.resolve(&KeyChord::key(vk::SPACE)), Some(Command::MarkToggle));
        assert_eq!(m.resolve(&KeyChord::key(0x00FF)), None);
    }

    #[test]
    fn default_binds_command_direct_to_z() {
        // 原作准拠で Z＝任意コマンド実行。トークン round-trip も確認する。
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::Z)), Some(Command::CommandDirect));
        assert_eq!(Command::from_token("CommandDirect"), Some(Command::CommandDirect));
        assert_eq!(Command::CommandDirect.as_token(), "CommandDirect");
        assert_eq!(Command::CommandDirect.display_name(), "任意のコマンドを実行");
    }

    #[test]
    fn default_binds_select_and_reload() {
        // A=反転（ファイル）・Shift+A=全反転・Ctrl+A=全選択、Home=選択解除、
        // Esc=タスク一覧、F5/Shift+Home=再読込。
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::A)), Some(Command::ReverseAllFile));
        assert_eq!(
            m.resolve(&KeyChord::new(vk::A, false, true, false)),
            Some(Command::ReverseAll)
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::A, true, false, false)),
            Some(Command::SelectAll)
        );
        assert_eq!(m.resolve(&KeyChord::key(vk::HOME)), Some(Command::ClearAll));
        assert_eq!(m.resolve(&KeyChord::key(vk::F5)), Some(Command::Reload));
        assert_eq!(m.resolve(&KeyChord::key(vk::ESCAPE)), Some(Command::OpenTaskManager));
    }

    #[test]
    fn default_binds_nav_and_search() {
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::OEM_5)), Some(Command::ToRoot));
        assert_eq!(
            m.resolve(&KeyChord::new(vk::LEFT, false, false, true)),
            Some(Command::HistoryBack)
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::RIGHT, false, false, true)),
            Some(Command::HistoryForward)
        );
        // F4＝入力式つき ChangeDirectory・Shift+F4＝フォルダ選択式つき。
        assert_eq!(m.resolve(&KeyChord::key(vk::F4)), Some(Command::ChangeDirectory));
        assert_eq!(
            m.resolve_inv(&KeyChord::key(vk::F4)),
            Some(&Invocation::new(
                Command::ChangeDirectory,
                vec!["=r.prompt(\"ディレクトリの入力\")".into()]
            ))
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::F4, false, true, false)),
            Some(Command::ChangeDirectory)
        );
        assert_eq!(m.resolve(&KeyChord::key(vk::J)), Some(Command::JumpDialog));
        assert_eq!(m.resolve(&KeyChord::key(vk::F)), Some(Command::IncrementalSearchDialog));
        assert_eq!(m.resolve(&KeyChord::key(vk::I)), Some(Command::DirectoryInformation));
        assert_eq!(
            m.resolve(&KeyChord::new(vk::R, false, true, false)),
            Some(Command::RenameSequenceDialog)
        );
        // 既定キーを持たないコマンドは未割当（個人 config で付ける）。
        assert_eq!(m.resolve(&KeyChord::key(vk::OEM_6)), None); // ] = PathHistoryDialog は個人設定
    }

    #[test]
    fn default_binds_tabs() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(&KeyChord::new(vk::TAB, true, false, false)),
            Some(Command::PageNext)
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::TAB, true, true, false)),
            Some(Command::PagePrevious)
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::T, true, false, false)),
            Some(Command::NewTab)
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::W, true, false, false)),
            Some(Command::CloseTab)
        );
    }

    #[test]
    fn default_binds_make_directory() {
        // K=ディレクトリ作成。
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::K)), Some(Command::MakeDirectory));
    }

    #[test]
    fn default_binds_copy_move() {
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::C)), Some(Command::Copy));
        assert_eq!(m.resolve(&KeyChord::key(vk::M)), Some(Command::Move));
    }

    #[test]
    fn default_binds_pane_sync() {
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::O)), Some(Command::OppositeToCurrent));
        assert_eq!(
            m.resolve(&KeyChord::new(vk::O, false, true, false)),
            Some(Command::CurrentToOpposite)
        );
    }

    #[test]
    fn default_binds_rename_delete() {
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::R)), Some(Command::Rename));
        assert_eq!(m.resolve(&KeyChord::key(vk::D)), Some(Command::Delete));
    }

    #[test]
    fn default_binds_compress_extract() {
        // P=圧縮・U=展開。
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::P)), Some(Command::Compress));
        assert_eq!(m.resolve(&KeyChord::key(vk::U)), Some(Command::Extract));
    }

    #[test]
    fn default_binds_drive_nav() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(&KeyChord::new(vk::RIGHT, false, true, false)),
            Some(Command::NextDrive)
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::LEFT, false, true, false)),
            Some(Command::PreviousDrive)
        );
    }

    #[test]
    fn default_binds_shift_select_move() {
        // Shift＋上下/PageUp/Down＝選択しながら移動（CursorXxx("select")）。
        let m = KeyMap::default();
        for (vk, cmd) in [
            (vk::UP, Command::CursorUp),
            (vk::DOWN, Command::CursorDown),
            (vk::PRIOR, Command::CursorPageUp),
            (vk::NEXT, Command::CursorPageDown),
        ] {
            let inv = m.resolve_inv(&KeyChord::new(vk, false, true, false)).unwrap();
            assert_eq!(inv.command, cmd);
            assert_eq!(inv.args, vec!["select".to_string()]);
        }
        // Shift+Space＝反転＋上移動（MarkToggle("-1")）。
        let space = m.resolve_inv(&KeyChord::new(vk::SPACE, false, true, false)).unwrap();
        assert_eq!(space.command, Command::MarkToggle);
        assert_eq!(space.args, vec!["-1".to_string()]);
    }

    #[test]
    fn default_binds_mask() {
        // Y・Shift+P=パスマスク。SelectMask は既定キー無し。
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::Y)), Some(Command::PathMask));
        assert_eq!(
            m.resolve(&KeyChord::new(vk::P, false, true, false)),
            Some(Command::PathMask)
        );
    }

    #[test]
    fn chord_token_roundtrip() {
        for s in [
            "Up", "Ctrl+A", "Ctrl+Shift+Tab", "Shift+F7", "C", "Ctrl+0", "Esc", ";", ":", "]",
            "\\", "@", "[", "Shift+;",
        ] {
            let chord = KeyChord::parse(s).unwrap();
            assert_eq!(chord.to_token().as_deref(), Some(s));
        }
        // 修飾子は順不同で受理する。
        assert_eq!(KeyChord::parse("Shift+Ctrl+Tab"), KeyChord::parse("Ctrl+Shift+Tab"));
        assert_eq!(KeyChord::parse("ctrl+a"), KeyChord::parse("Ctrl+A"));
        assert!(KeyChord::parse("Bogus+X").is_none());
    }

    #[test]
    fn command_token_roundtrip() {
        for c in Command::all() {
            assert_eq!(Command::from_token(c.as_token()), Some(c));
        }
        assert!(Command::from_token("Nonexistent").is_none());
    }

    #[test]
    fn every_command_has_display_name() {
        for c in Command::all() {
            assert!(!c.display_name().is_empty(), "{} has no display name", c.as_token());
        }
    }

    #[test]
    fn keymap_string_map_roundtrip() {
        let m = KeyMap::default();
        let sm = m.to_string_map();
        let back = KeyMap::from_string_map(&sm);
        assert_eq!(back.to_string_map(), sm);
        assert_eq!(back.resolve(&KeyChord::key(vk::DOWN)), Some(Command::CursorDown));
    }

    #[test]
    fn invocation_parse_bare_and_args() {
        // 引数なしは従来トークンと同義。
        assert_eq!(Invocation::parse("CursorDown"), Some(Invocation::bare(Command::CursorDown)));
        // 余分な空白も許容。
        assert_eq!(Invocation::parse("  Reload  "), Some(Invocation::bare(Command::Reload)));
        // 引数つき。
        assert_eq!(
            Invocation::parse(r#"ChangeDirectoryDialog("D:")"#),
            Some(Invocation::new(Command::ChangeDirectoryDialog, vec!["D:".into()]))
        );
        // 複数引数・引数間の空白。
        assert_eq!(
            Invocation::parse(r#"NewTab("a" ,  "b")"#),
            Some(Invocation::new(Command::NewTab, vec!["a".into(), "b".into()]))
        );
        // 空括弧は引数なし。
        assert_eq!(Invocation::parse("Reload()"), Some(Invocation::bare(Command::Reload)));
        // エスケープ（\" と \\）。
        assert_eq!(
            Invocation::parse(r#"Reload("say \"hi\"\\")"#),
            Some(Invocation::new(Command::Reload, vec!["say \"hi\"\\".into()]))
        );
    }

    #[test]
    fn invocation_parse_rejects_malformed() {
        assert!(Invocation::parse("Bogus").is_none()); // 未知コマンド
        assert!(Invocation::parse("Reload(\"a\"").is_none()); // 閉じ括弧なし
        assert!(Invocation::parse("Reload(a)").is_none()); // クォートなし引数
        assert!(Invocation::parse("Reload(\"a\" \"b\")").is_none()); // カンマなし
    }

    #[test]
    fn invocation_token_roundtrip() {
        for s in [
            "CursorDown",
            r#"ChangeDirectoryDialog("D:")"#,
            r#"NewTab("a", "b")"#,
            r#"Reload("say \"hi\"\\")"#,
            r#"Script("myCommand")"#,
            r#"Eval("rerics.log(\"hi\")")"#,
        ] {
            let inv = Invocation::parse(s).unwrap();
            assert_eq!(inv.to_token_string(), s);
            assert_eq!(Invocation::parse(&inv.to_token_string()), Some(inv));
        }
    }

    #[test]
    fn keymap_keeps_args_through_string_map() {
        let mut m = KeyMap::new();
        m.bind_inv(
            KeyChord::key(vk::F4),
            Invocation::new(Command::ChangeDirectoryDialog, vec!["D:".into()]),
        );
        let sm = m.to_string_map();
        assert_eq!(sm.get("F4").map(String::as_str), Some(r#"ChangeDirectoryDialog("D:")"#));
        let back = KeyMap::from_string_map(&sm);
        assert_eq!(
            back.resolve_inv(&KeyChord::key(vk::F4)),
            Some(&Invocation::new(Command::ChangeDirectoryDialog, vec!["D:".into()]))
        );
    }

    #[test]
    fn empty_value_unbinds_key() {
        // 値が空文字のキーは未バインドになる（既定打ち消し用）。
        let mut sm = KeyMap::default().to_string_map();
        assert_eq!(sm.get("Down").map(String::as_str), Some("CursorDown"));
        sm.insert("Down".to_string(), String::new());
        let m = KeyMap::from_string_map(&sm);
        assert_eq!(m.resolve(&KeyChord::key(vk::DOWN)), None);
        // 空にしていない他キーは残る。
        assert_eq!(m.resolve(&KeyChord::key(vk::UP)), Some(Command::CursorUp));
    }

    #[test]
    fn default_textviewer_binds_origin_keys() {
        let m = KeyMap::default_textviewer();
        assert_eq!(m.resolve(&KeyChord::key(vk::UP)), Some(Command::ViewerScrollUp));
        assert_eq!(m.resolve(&KeyChord::key(vk::DOWN)), Some(Command::ViewerScrollDown));
        assert_eq!(m.resolve(&KeyChord::key(vk::RETURN)), Some(Command::ViewerClose));
        assert_eq!(m.resolve(&KeyChord::key(vk::ESCAPE)), Some(Command::ViewerClose));
        assert_eq!(m.resolve(&KeyChord::key(vk::Q)), Some(Command::ViewerClose));
        assert_eq!(m.resolve(&KeyChord::key(vk::B)), Some(Command::ViewerToggleMode));
        assert_eq!(m.resolve(&KeyChord::key(vk::C)), Some(Command::ViewerChangeEncoding));
        assert_eq!(
            m.resolve(&KeyChord::new(vk::C, true, false, false)),
            Some(Command::ViewerCopy)
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::A, true, false, false)),
            Some(Command::ViewerSelectAll)
        );
        // 検索は Ctrl+F / Ctrl+S、次=N・前=P。
        assert_eq!(
            m.resolve(&KeyChord::new(vk::F, true, false, false)),
            Some(Command::ViewerSearchDialog)
        );
        assert_eq!(m.resolve(&KeyChord::key(vk::N)), Some(Command::ViewerFindNext));
        assert_eq!(m.resolve(&KeyChord::key(vk::P)), Some(Command::ViewerFindPrevious));
        assert_eq!(m.resolve(&KeyChord::key(vk::E)), Some(Command::Edit));
        // 横スクロールは割り当てない（折返し表示のため）。
        assert_eq!(m.resolve(&KeyChord::key(vk::LEFT)), None);
        assert_eq!(m.resolve(&KeyChord::key(vk::RIGHT)), None);
    }

    #[test]
    fn default_imageviewer_binds_current_keys() {
        let m = KeyMap::default_imageviewer();
        assert_eq!(m.resolve(&KeyChord::key(vk::SPACE)), Some(Command::MediaTogglePlay));
        assert_eq!(m.resolve(&KeyChord::key(vk::LEFT)), Some(Command::ImagePrevious));
        assert_eq!(m.resolve(&KeyChord::key(vk::RIGHT)), Some(Command::ImageNext));
        assert_eq!(m.resolve(&KeyChord::key(vk::ADD)), Some(Command::ImageZoomIn));
        assert_eq!(m.resolve(&KeyChord::key(vk::SUBTRACT)), Some(Command::ImageZoomOut));
        assert_eq!(m.resolve(&KeyChord::key(vk::R)), Some(Command::ImageRotateRight));
        assert_eq!(m.resolve(&KeyChord::key(vk::H)), Some(Command::ImageFlipVertical));
        // 表示モードは数字キー 1〜5（0 は原作に無いので未バインド）。
        assert_eq!(m.resolve(&KeyChord::key(vk::D0)), None);
        assert_eq!(m.resolve(&KeyChord::key(vk::D1)), Some(Command::ImageActualSize));
        assert_eq!(m.resolve(&KeyChord::key(vk::D2)), Some(Command::ImageFitWindow));
        assert_eq!(m.resolve(&KeyChord::key(vk::D3)), Some(Command::ImageFitWidth));
        assert_eq!(m.resolve(&KeyChord::key(vk::D4)), Some(Command::ImageFitHeight));
        assert_eq!(m.resolve(&KeyChord::key(vk::D5)), Some(Command::ImageFitLarge));
        assert_eq!(
            m.resolve(&KeyChord::new(vk::C, true, false, false)),
            Some(Command::ImageCopy)
        );
        // 終了は両ビューア共有コマンド。
        assert_eq!(m.resolve(&KeyChord::key(vk::ESCAPE)), Some(Command::ViewerClose));
    }

    #[test]
    fn imageviewer_string_map_roundtrip() {
        // テンキー +/- や OEM_- もトークン往復できる（落ちると default.toml 検証が壊れる）。
        let m = KeyMap::default_imageviewer();
        let sm = m.to_string_map();
        assert!(sm.contains_key("NumPad+"));
        assert!(sm.contains_key("NumPad-"));
        let back = KeyMap::from_string_map(&sm);
        assert_eq!(back.to_string_map(), sm);
    }

    #[test]
    fn command_contexts_partition() {
        use CommandContext::*;
        // ファイラー専用コマンドはテキストビューアでは無効。
        assert!(Command::CursorDown.available_in(Filer));
        assert!(!Command::CursorDown.available_in(TextViewer));
        // ビューア専用はビューアのみ。
        assert!(Command::ViewerToggleMode.available_in(TextViewer));
        assert!(!Command::ViewerToggleMode.available_in(Filer));
        // 共有コマンドは両方で有効。
        assert!(Command::Edit.available_in(Filer));
        assert!(Command::Edit.available_in(TextViewer));
        // 終了は両ビューアで有効。
        assert!(Command::ViewerClose.available_in(TextViewer));
        assert!(Command::ViewerClose.available_in(ImageViewer));
    }

    #[test]
    fn textviewer_string_map_roundtrip() {
        let m = KeyMap::default_textviewer();
        let sm = m.to_string_map();
        let back = KeyMap::from_string_map(&sm);
        assert_eq!(back.to_string_map(), sm);
        assert_eq!(back.resolve(&KeyChord::key(vk::B)), Some(Command::ViewerToggleMode));
    }

    #[test]
    fn rebind_inverts_updown() {
        let mut m = KeyMap::default();
        m.bind(KeyChord::key(vk::UP), Command::CursorDown);
        m.bind(KeyChord::key(vk::DOWN), Command::CursorUp);
        assert_eq!(m.resolve(&KeyChord::key(vk::DOWN)), Some(Command::CursorUp));
        assert_eq!(m.resolve(&KeyChord::key(vk::UP)), Some(Command::CursorDown));
    }
}
