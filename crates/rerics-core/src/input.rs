//! コマンドとキーバインドの土台（UI 非依存）。
//!
//! GUI 層はキー入力を [`KeyChord`] にして [`KeyMap::resolve`] で [`Command`] に解決し、
//! その Command を自前で実行する。全コマンドが自由にリマップ可能。

use std::collections::{BTreeMap, HashMap};

use crate::call::Call;

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
    PathRegisterDialog,
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
    NewFiler,
    Exit,
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
    CreateFileDialog,
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
    Menu,
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
    // 情報取得（値返しクエリ）
    CursorName,
    CursorPath,
    MarkedCount,
    HasMarks,
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

/// 引数の型。機能欄に書けるリテラル引数と名前付きオプションを説明する語彙。
/// ダイアログ案内・補完・HTML リファレンスの単一ソースとして使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    /// 任意の文字列。
    Str,
    /// パス文字列。
    Path,
    /// 整数。
    Int,
    /// 真偽値。
    Bool,
    /// 決められた語のいずれか（`sort` の種別など）。
    Enum(&'static [&'static str]),
    /// 末尾に乗る名前付きオプションの Object（`{ select: true }`）。
    Options(&'static [OptSpec]),
}

/// 位置引数 1 つの仕様。実行時は位置で解決するが、説明・補完のために名前を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgSpec {
    /// 引数名（説明・補完用）。
    pub name: &'static str,
    /// 型。
    pub ty: ArgType,
    /// 省略可能か。
    pub required: bool,
    /// 1 行説明。
    pub doc: &'static str,
}

/// 名前付きオプション 1 つの仕様（[`ArgType::Options`] の要素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptSpec {
    /// オプション名（Object のキー）。
    pub name: &'static str,
    /// 型（スカラのみ）。
    pub ty: ArgType,
    /// 1 行説明。
    pub doc: &'static str,
}

/// コマンド 1 つのメタデータ。token・表示名・有効文脈は [`Command::ALL`]／[`Command::contexts`]
/// 側の単一ソースを引き、ここでは説明・引数仕様・使用例だけを足す（②で育てるフィールド群）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandMeta {
    /// 1 行説明（未整備のコマンドは表示名を流用＝[`CommandMeta::trivial`]）。
    pub summary: &'static str,
    /// 位置引数の仕様（先頭から順）。名前付きオプションは末尾要素に [`ArgType::Options`] で乗る。
    pub args: &'static [ArgSpec],
    /// 使用例（機能欄に書ける式）。
    pub examples: &'static [&'static str],
}

/// 「選択しながら移動」の名前付きオプション（カーソル移動コマンド共通）。
const SELECT_OPT: &[ArgSpec] = &[ArgSpec {
    name: "options",
    ty: ArgType::Options(&[OptSpec {
        name: "select",
        ty: ArgType::Bool,
        doc: "アンカーから現在位置までを選択しながら移動する",
    }]),
    required: false,
    doc: "移動の名前付きオプション",
}];

/// 「操作後にカーソルを動かす量」の名前付きオプション（マーク／選択コマンド共通）。
const CURSOR_MOVE_OPT: &[ArgSpec] = &[ArgSpec {
    name: "options",
    ty: ArgType::Options(&[OptSpec {
        name: "cursorMove",
        ty: ArgType::Int,
        doc: "操作後にカーソルを動かす量（既定は設定の down_after_select に従う）",
    }]),
    required: false,
    doc: "操作後の名前付きオプション",
}];

impl Command {
    /// コマンドと「設定トークン名・表示名（日本語）」の対応表（変換の単一の出どころ）。
    const ALL: &'static [(Command, &'static str, &'static str, &'static str)] = {
        use Command::*;
        &[
            (CursorUp, "cursorUp", "カーソルを上へ", "カーソルを 1 つ上の項目へ移動する"),
            (CursorDown, "cursorDown", "カーソルを下へ", "カーソルを 1 つ下の項目へ移動する"),
            (CursorTop, "cursorTop", "先頭行へ移動", "先頭の項目へカーソルを移動する"),
            (CursorEnd, "cursorEnd", "最終行へ移動", "末尾の項目へカーソルを移動する"),
            (CursorPageUp, "cursorPageUp", "1ページ上へ", "1 ページ分カーソルを上へ移動する"),
            (CursorPageDown, "cursorPageDown", "1ページ下へ", "1 ページ分カーソルを下へ移動する"),
            (SetCursorPosition, "setCursorPosition", "指定位置へカーソル移動", "指定した名前の項目へカーソルを移動する"),
            (EnterDir, "enterDir", "開く（ディレクトリ・書庫へ）", "カーソル位置のディレクトリや書庫を開いて中へ入る"),
            (View, "view", "ビューアで表示", "カーソル位置のファイルを内蔵ビューアで表示する"),
            (ToParent, "toParent", "親ディレクトリへ移動", "1 つ上の親ディレクトリへ移動する"),
            (ToRoot, "toRoot", "ルートディレクトリへ移動", "現在のドライブのルートへ移動する"),
            (HistoryBack, "historyBack", "履歴を1つ戻る", "ディレクトリ移動履歴を 1 つ前へ戻る"),
            (HistoryForward, "historyForward", "履歴を1つ進める", "ディレクトリ移動履歴を 1 つ先へ進める"),
            (PathHistoryDialog, "pathHistoryDialog", "ディレクトリ履歴", "ディレクトリ移動履歴の一覧を開き選んだ場所へ移動する"),
            (ChangeDirectory, "changeDirectory", "ディレクトリの変更", "指定したパスへディレクトリを移動する"),
            (ChangeDirectoryDialog, "changeDirectoryDialog", "ディレクトリ変更ダイアログ", "移動先パスを入力するダイアログを開く"),
            (ChangeDrive, "changeDrive", "ドライブの変更", "指定したドライブへ移動する"),
            (ChangeDriveDialog, "changeDriveDialog", "ドライブリスト", "ドライブの一覧を開き選んだドライブへ移動する"),
            (JumpDialog, "jumpDialog", "登録ディレクトリ", "登録済みディレクトリの一覧を開き選んだ場所へ移動する"),
            (PathRegisterDialog, "pathRegisterDialog", "登録ディレクトリに追加", "現在のディレクトリを登録ディレクトリへ追加する"),
            (IncrementalSearchDialog, "incrementalSearchDialog", "インクリメンタルサーチ", "入力に合わせて項目を絞り込むインクリメンタルサーチを開く"),
            (DirectoryInformation, "directoryInformation", "ディレクトリの容量計算", "選択したディレクトリの合計サイズを計算して表示する"),
            (RenameSequenceDialog, "renameSequenceDialog", "連番リネーム", "選択した項目を連番で一括リネームするダイアログを開く"),
            (FocusLeft, "focusLeft", "左パスへ移動", "左ペインへフォーカスを移す（設定により親へ移動）"),
            (FocusRight, "focusRight", "右パスへ移動", "右ペインへフォーカスを移す（設定により親へ移動）"),
            (MarkToggle, "markToggle", "選択／解除（カーソル位置）", "カーソル位置の項目の選択状態を反転する"),
            (SelectAll, "selectAll", "すべて選択", "ディレクトリも含めすべての項目を選択する"),
            (ClearAll, "clearAll", "すべて選択解除", "すべての項目の選択を解除する"),
            (ReverseAll, "reverseAll", "すべて選択反転", "すべての項目の選択状態を反転する"),
            (SelectAllFile, "selectAllFile", "ファイルのみ選択", "ファイルだけをすべて選択する（ディレクトリは除く）"),
            (ReverseAllFile, "reverseAllFile", "ファイルのみ選択反転", "ファイルだけの選択状態を反転する（ディレクトリは除く）"),
            (Reload, "reload", "最新の情報に更新", "両ペインのファイル一覧を読み直して最新にする"),
            (SortByName, "sortByName", "名前順で並べ替え", "ファイル名順で並べ替える"),
            (SortByExtension, "sortByExtension", "拡張子順で並べ替え", "拡張子順で並べ替える"),
            (SortBySize, "sortBySize", "サイズ順で並べ替え", "ファイルサイズ順で並べ替える"),
            (SortByDate, "sortByDate", "日付順で並べ替え", "更新日時順で並べ替える"),
            (Sort, "sort", "並べ替え方法の指定", "指定した種別で並べ替える"),
            (SortDialog, "sortDialog", "ソート設定", "並べ替えの方法を選ぶダイアログを開く"),
            (SortReverseToggle, "sortReverseToggle", "昇順／降順を反転", "現在の並べ替えの昇順・降順を反転する"),
            (PageNext, "pageNext", "次のタブへ", "次のタブへ切り替える"),
            (PagePrevious, "pagePrevious", "前のタブへ", "前のタブへ切り替える"),
            (NewFiler, "newFiler", "新しいタブ", "新しいタブを開く"),
            (Exit, "exit", "タブを閉じる", "現在のタブを閉じる"),
            (MakeDirectory, "makeDirectory", "ディレクトリの作成", "新しいディレクトリを作成する"),
            (Copy, "copy", "コピー", "選択した項目を反対側のパスへコピーする"),
            (Move, "move", "移動", "選択した項目を反対側のパスへ移動する"),
            (SwapPath, "swapPath", "左右のパスを入れ替え", "左右ペインの表示ディレクトリを入れ替える"),
            (OppositeToCurrent, "oppositeToCurrent", "反対側をカレントと同じに", "反対側ペインを現在のディレクトリと同じ場所にする"),
            (CurrentToOpposite, "currentToOpposite", "カレントを反対側と同じに", "現在のペインを反対側と同じ場所にする"),
            (Rename, "rename", "名前の変更", "カーソル位置の項目の名前を変更する"),
            (Delete, "delete", "削除", "選択した項目を完全に削除する"),
            (SendToRecycled, "sendToRecycled", "ごみ箱へ送る", "選択した項目をごみ箱へ送る"),
            (CreateShortcut, "createShortcut", "ショートカットの作成", "選択した項目のショートカットを作成する"),
            (ClipCopy, "clipCopy", "クリップボードにコピー", "選択した項目をクリップボードへコピーする"),
            (ClipCut, "clipCut", "クリップボードに切り取り", "選択した項目をクリップボードへ切り取る"),
            (ClipPaste, "clipPaste", "クリップボードから貼り付け", "クリップボードの項目を現在のディレクトリへ貼り付ける"),
            (CreateFileDialog, "createFileDialog", "新規ファイルの作成", "新しい空ファイルを作成するダイアログを開く"),
            (ViewFile, "viewFile", "ビューアで開く", "カーソル位置のファイルを内蔵ビューアで開く"),
            (Edit, "edit", "エディタで開く", "カーソル位置のファイルを外部エディタで開く"),
            (PropertyDialog, "propertyDialog", "プロパティの表示", "カーソル位置の項目のプロパティを表示する"),
            (Compress, "compress", "圧縮", "選択した項目を書庫に圧縮する"),
            (Extract, "extract", "解凍", "選択した書庫を解凍する"),
            (NextDrive, "nextDrive", "次のドライブへ", "次のドライブへ切り替える"),
            (PreviousDrive, "previousDrive", "前のドライブへ", "前のドライブへ切り替える"),
            (PathMask, "pathMask", "パスマスク", "表示する項目をマスクで絞り込む"),
            (SelectMask, "selectMask", "マスクで選択", "マスクに一致する項目をまとめて選択する"),
            (OpenTaskManager, "openTaskManager", "タスクマネージャ", "実行中のファイル操作を一覧するタスクマネージャを開く"),
            (OpenSettings, "openSettings", "設定を開く", "設定ダイアログを開く"),
            (KeyBindsDialog, "keyBindsDialog", "キーバインドリスト", "現在のキー割り当ての一覧を表示する"),
            (CommandDirect, "commandDirect", "任意のコマンドを実行", "コマンドを入力して直接実行するダイアログを開く"),
            (Menu, "menu", "名前付きメニューを開く", "指定した名前のメニューを開く"),
            (CopyLog, "copyLog", "ログをコピー", "ログの内容をクリップボードへコピーする"),
            (ClearLog, "clearLog", "ログクリア", "ログの内容を消去する"),
            (MaximizeLeft, "maximizeLeft", "左リストを最大化", "左ペインを広げて最大化する"),
            (MaximizeRight, "maximizeRight", "右リストを最大化", "右ペインを広げて最大化する"),
            (MaximizeLeftForce, "maximizeLeftForce", "左リストを最大化（強制）", "左ペインを全幅まで最大化する"),
            (MaximizeRightForce, "maximizeRightForce", "右リストを最大化（強制）", "右ペインを全幅まで最大化する"),
            (BorderLeft, "borderLeft", "境界線を左へ", "左右ペインの境界を左へ動かす"),
            (BorderRight, "borderRight", "境界線を右へ", "左右ペインの境界を右へ動かす"),
            (BorderReset, "borderReset", "境界線を定位置へ", "左右ペインの境界を中央の定位置へ戻す"),
            (CursorOpposite, "cursorOpposite", "反対側パスへ移動", "反対側ペインへフォーカスを移す"),
            (SelectFile, "selectFile", "ファイルを選択（カーソル位置）", "カーソル位置のファイルを選択する"),
            (Refresh, "refresh", "表示を更新", "表示を再描画する"),
            (Nop, "nop", "無効コマンド", "何もしない（キー割り当ての無効化に使う）"),
            (MaximizeCurrent, "maximizeCurrent", "現在のリストを最大化", "現在のペインを最大化する"),
            (MaximizeWindow, "maximizeWindow", "ウィンドウの最大化", "ウィンドウを最大化する"),
            (MinimizeWindow, "minimizeWindow", "ウィンドウの最小化", "ウィンドウを最小化する"),
            (End, "end", "アプリケーションの終了", "アプリケーションを終了する"),
            (Restart, "restart", "再起動", "アプリケーションを再起動する"),
            (Quit, "quit", "閉じる（最後ならアプリ終了）", "タブを閉じる（最後のタブならアプリを終了する）"),
            (CursorName, "cursorName", "カーソル項目名", "カーソル下の項目の名前を返す（無ければ空文字）"),
            (CursorPath, "cursorPath", "カーソル項目のパス", "カーソル下の項目のフルパスを返す（無ければ空文字）"),
            (MarkedCount, "markedCount", "マーク数", "マーク（選択）されている項目の数を返す"),
            (HasMarks, "hasMarks", "マークの有無", "マーク（選択）された項目があるかを真偽で返す"),
            (ViewerClose, "viewerClose", "ビューアを閉じる", "ビューアを閉じてファイル一覧へ戻る"),
            (ViewerScrollUp, "viewerScrollUp", "1行上へ", "ビューアを 1 行上へスクロールする"),
            (ViewerScrollDown, "viewerScrollDown", "1行下へ", "ビューアを 1 行下へスクロールする"),
            (ViewerPageUp, "viewerPageUp", "1ページ上へ", "ビューアを 1 ページ上へスクロールする"),
            (ViewerPageDown, "viewerPageDown", "1ページ下へ", "ビューアを 1 ページ下へスクロールする"),
            (ViewerScrollTop, "viewerScrollTop", "先頭行へ移動", "ビューアを先頭へスクロールする"),
            (ViewerScrollBottom, "viewerScrollBottom", "最終行へ移動", "ビューアを末尾へスクロールする"),
            (ViewerSearchDialog, "viewerSearchDialog", "検索", "ビューア内を検索するダイアログを開く"),
            (ViewerFindNext, "viewerFindNext", "次を検索", "次の検索一致へ移動する"),
            (ViewerFindPrevious, "viewerFindPrevious", "前を検索", "前の検索一致へ移動する"),
            (ViewerSelectAll, "viewerSelectAll", "すべて選択", "ビューアの全文を選択する"),
            (ViewerToggleMode, "viewerToggleMode", "バイナリ／テキスト切替", "ビューアのテキスト表示とバイナリ表示を切り替える"),
            (ViewerChangeEncoding, "viewerChangeEncoding", "文字コードを切替", "ビューアの文字コードを切り替える"),
            (ViewerCopy, "viewerCopy", "選択範囲をコピー", "ビューアの選択範囲をクリップボードへコピーする"),
            (ViewerContextMenu, "viewerContextMenu", "コンテキストメニュー", "ビューアのコンテキストメニューを開く"),
            (ImageNext, "imageNext", "次の画像", "次の画像へ送る"),
            (ImagePrevious, "imagePrevious", "前の画像", "前の画像へ戻る"),
            (ImageZoomIn, "imageZoomIn", "拡大", "画像を拡大する"),
            (ImageZoomOut, "imageZoomOut", "縮小", "画像を縮小する"),
            (ImageFitWindow, "imageFitWindow", "画面に合わせて縮小", "画像を画面に収まるよう縮小して表示する"),
            (ImageActualSize, "imageActualSize", "等倍表示", "画像を実寸（等倍）で表示する"),
            (ImageFitWidth, "imageFitWidth", "横幅に合わせる", "画像を画面の横幅に合わせて表示する"),
            (ImageFitHeight, "imageFitHeight", "縦幅に合わせる", "画像を画面の縦幅に合わせて表示する"),
            (ImageFitLarge, "imageFitLarge", "なるべく大きく表示", "画像を画面いっぱいになるべく大きく表示する"),
            (ImageRotateRight, "imageRotateRight", "右回転", "画像を右へ 90 度回転する"),
            (ImageRotateLeft, "imageRotateLeft", "左回転", "画像を左へ 90 度回転する"),
            (ImageFlipHorizontal, "imageFlipHorizontal", "左右反転", "画像を左右に反転する"),
            (ImageFlipVertical, "imageFlipVertical", "上下反転", "画像を上下に反転する"),
            (ImageCopy, "imageCopy", "クリップボードにコピー", "表示中の画像をクリップボードへコピーする"),
            (MediaTogglePlay, "mediaTogglePlay", "再生／一時停止", "動画・音声の再生と一時停止を切り替える"),
        ]
    };

    /// 設定トークン名を返す。
    pub fn as_token(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(c, _, _, _)| *c == self)
            .map(|(_, s, _, _)| *s)
            .unwrap_or("")
    }

    /// 表示名（日本語）を返す。UI でユーザに見せる短い機能名。
    pub fn display_name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(c, _, _, _)| *c == self)
            .map(|(_, _, l, _)| *l)
            .unwrap_or("")
    }

    /// 1 行の説明文（ダイアログ案内・HTML リファレンス用）。表示名より詳しく効果を述べる。
    pub fn summary(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(c, _, _, _)| *c == self)
            .map(|(_, _, _, s)| *s)
            .unwrap_or("")
    }

    /// 設定トークン名から解釈する。
    pub fn from_token(s: &str) -> Option<Command> {
        Self::ALL
            .iter()
            .find(|(_, t, _, _)| *t == s)
            .map(|(c, _, _, _)| *c)
            .or_else(|| Self::alias_token(s))
    }

    /// 原作で別名だったトークンを正式コマンドへ解決する（原作 config や移植したメニューが
    /// 原作名のまま書かれていても通す）。`as_token`/`to_token_string` は正式名を返すので、
    /// 別名は入力の解釈時にだけ受け付ける。
    fn alias_token(s: &str) -> Option<Command> {
        Some(match s {
            "CD" => Command::ChangeDirectory,
            "RegisteredPathDialog" => Command::JumpDialog,
            "UnPack" => Command::Extract,
            _ => return None,
        })
    }

    /// 全コマンドを列挙する（設定 UI 用）。
    pub fn all() -> impl Iterator<Item = Command> {
        Self::ALL.iter().map(|(c, _, _, _)| *c)
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
            _ => &[Filer],
        }
    }

    /// このコマンドが指定文脈で有効か。
    pub fn available_in(self, ctx: CommandContext) -> bool {
        self.contexts().contains(&ctx)
    }

    /// コマンドのメタデータ（説明・引数仕様・使用例）。`summary` は [`Command::ALL`] を単一
    /// ソースとして引き、引数を取るコマンドだけ引数仕様・使用例を個別に整備する。
    pub fn meta(self) -> CommandMeta {
        use Command::*;
        let summary = self.summary();
        match self {
            CursorUp => CommandMeta {
                summary,
                args: SELECT_OPT,
                examples: &["cursorUp()", "cursorUp({select:true})"],
            },
            CursorDown => CommandMeta {
                summary,
                args: SELECT_OPT,
                examples: &["cursorDown()", "cursorDown({select:true})"],
            },
            CursorTop => CommandMeta {
                summary,
                args: SELECT_OPT,
                examples: &["cursorTop()", "cursorTop({select:true})"],
            },
            CursorEnd => CommandMeta {
                summary,
                args: SELECT_OPT,
                examples: &["cursorEnd()", "cursorEnd({select:true})"],
            },
            CursorPageUp => CommandMeta {
                summary,
                args: SELECT_OPT,
                examples: &["cursorPageUp()", "cursorPageUp({select:true})"],
            },
            CursorPageDown => CommandMeta {
                summary,
                args: SELECT_OPT,
                examples: &["cursorPageDown()", "cursorPageDown({select:true})"],
            },
            SetCursorPosition => CommandMeta {
                summary,
                args: &[ArgSpec {
                    name: "name",
                    ty: ArgType::Str,
                    required: true,
                    doc: "移動先の項目名",
                }],
                examples: &[r#"setCursorPosition("readme.txt")"#],
            },
            MarkToggle => CommandMeta {
                summary,
                args: CURSOR_MOVE_OPT,
                examples: &["markToggle()", "markToggle({cursorMove:-1})"],
            },
            SelectFile => CommandMeta {
                summary,
                args: CURSOR_MOVE_OPT,
                examples: &["selectFile()", "selectFile({cursorMove:1})"],
            },
            View => CommandMeta {
                summary,
                args: &[ArgSpec {
                    name: "path",
                    ty: ArgType::Path,
                    required: false,
                    doc: "表示するファイル（省略時はカーソル位置）",
                }],
                examples: &["view()", r#"view("C:\\note.txt")"#],
            },
            ChangeDirectory => CommandMeta {
                summary,
                args: &[ArgSpec {
                    name: "path",
                    ty: ArgType::Path,
                    required: true,
                    doc: "移動先ディレクトリ",
                }],
                examples: &[r#"changeDirectory("C:\\work")"#],
            },
            ChangeDrive => CommandMeta {
                summary,
                args: &[ArgSpec {
                    name: "drive",
                    ty: ArgType::Str,
                    required: true,
                    doc: r#"ドライブ（"D:" など）"#,
                }],
                examples: &[r#"changeDrive("D:")"#],
            },
            Menu => CommandMeta {
                summary,
                args: &[ArgSpec {
                    name: "name",
                    ty: ArgType::Str,
                    required: true,
                    doc: "開くメニューの名前",
                }],
                examples: &[r#"menu("ファイル操作")"#],
            },
            Sort => CommandMeta {
                summary,
                args: &[ArgSpec {
                    name: "by",
                    ty: ArgType::Enum(&[
                        "name",
                        "extension",
                        "size",
                        "date",
                        "createtime",
                        "attribute",
                    ]),
                    required: true,
                    doc: "並べ替えの種別",
                }],
                examples: &[r#"sort("name")"#, r#"sort("size")"#],
            },
            // 値返しクエリ＝引数なし。スクリプトで `const n = r.markedCount()` のように値を使う。
            CursorName => CommandMeta { summary, args: &[], examples: &["cursorName()"] },
            CursorPath => CommandMeta { summary, args: &[], examples: &["cursorPath()"] },
            MarkedCount => CommandMeta { summary, args: &[], examples: &["markedCount()"] },
            HasMarks => CommandMeta { summary, args: &[], examples: &["hasMarks()"] },
            _ => CommandMeta { summary, args: &[], examples: &[] },
        }
    }
}

/// 組込コマンド＋リテラル引数の呼び出し。機能欄の式のうち、単一組込呼び出しに簡約できる
/// ものを「コマンド名＋文字列引数」で表す（キー編集 UI の組込行・メニュー解決で使う）。
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

    /// 式文字列へ変換する。`()` は常に付ける（機能欄は関数呼び出し形が必須）。
    /// 引数なしは `name()`、ありは `name("a", "b")`。
    pub fn to_token_string(&self) -> String {
        let name = self.command.as_token();
        if self.args.is_empty() {
            return format!("{name}()");
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
    map: HashMap<KeyChord, String>,
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
            m.bind_expr(
                KeyChord::new(vk, false, true, false),
                &format!("{}({{select:true}})", cmd.as_token()),
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
        m.bind_expr(
            KeyChord::key(vk::F4),
            r#"{ const d = r.prompt("ディレクトリの入力"); if (d) r.changeDirectory(d); }"#,
        );
        m.bind_expr(
            KeyChord::new(vk::F4, false, true, false),
            r#"{ const d = r.folderDialog("ディレクトリの選択"); if (d) r.changeDirectory(d); }"#,
        );
        m.bind(KeyChord::key(vk::J), JumpDialog);
        m.bind(KeyChord::key(vk::Z), CommandDirect);
        // 選択。
        m.bind(KeyChord::key(vk::SPACE), MarkToggle);
        // Shift+Space＝反転＋カーソル上移動。
        m.bind_expr(
            KeyChord::new(vk::SPACE, false, true, false),
            "markToggle({cursorMove:-1})",
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
        m.bind(KeyChord::new(vk::T, true, false, false), NewFiler);
        m.bind(KeyChord::new(vk::W, true, false, false), Exit);
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
        self.map.insert(chord, format!("{}()", cmd.as_token()));
        self
    }

    /// 組込呼び出し（コマンド＋リテラル引数）を割り当てる。
    pub fn bind_inv(&mut self, chord: KeyChord, inv: Invocation) -> &mut Self {
        self.map.insert(chord, inv.to_token_string());
        self
    }

    /// 任意の式文字列を割り当てる（ネスト呼び出し・スクリプトコードなど）。
    pub fn bind_expr(&mut self, chord: KeyChord, expr: &str) -> &mut Self {
        self.map.insert(chord, expr.to_owned());
        self
    }

    pub fn unbind(&mut self, chord: &KeyChord) {
        self.map.remove(chord);
    }

    /// 割り当てられた組込コマンドだけを返す（引数・スクリプト式は見ない簡易問い合わせ・
    /// テスト/内観用）。式がスクリプト送りに簡約されるバインドでは `None`。
    pub fn resolve(&self, chord: &KeyChord) -> Option<Command> {
        match self.map.get(chord).map(|s| Call::parse(s)) {
            Some(Call::Builtin { command, .. }) => Some(command),
            _ => None,
        }
    }

    /// 割り当てられた式を解釈した [`Call`] を返す。実行配線はこちらを使う。
    pub fn resolve_call(&self, chord: &KeyChord) -> Option<Call> {
        self.map.get(chord).map(|s| Call::parse(s))
    }

    /// 式文字列のマップ（チョード→式）からキーマップを組む。
    /// **値が空文字のキーは未バインド**にする（既定を差分マージで上書きしたうえでここで
    /// 読み飛ばす＝ユーザ config で既定キーを潰す手段）。
    pub fn from_string_map(map: &BTreeMap<String, String>) -> Self {
        let mut m = Self::new();
        for (k, v) in map {
            if v.trim().is_empty() {
                continue;
            }
            if let Some(chord) = KeyChord::parse(k) {
                m.map.insert(chord, v.clone());
            }
        }
        m
    }

    /// 式文字列のマップ（チョード→式）へ変換する。
    pub fn to_string_map(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (chord, expr) in &self.map {
            if let Some(tok) = chord.to_token() {
                out.insert(tok, expr.clone());
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
        assert_eq!(Command::from_token("commandDirect"), Some(Command::CommandDirect));
        assert_eq!(Command::CommandDirect.as_token(), "commandDirect");
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
        // F4＝入力ダイアログから changeDirectory・Shift+F4＝フォルダ選択から（どちらもスクリプト式）。
        assert!(matches!(m.resolve_call(&KeyChord::key(vk::F4)), Some(Call::Script { .. })));
        assert!(matches!(
            m.resolve_call(&KeyChord::new(vk::F4, false, true, false)),
            Some(Call::Script { .. })
        ));
        assert_eq!(m.resolve(&KeyChord::key(vk::J)), Some(Command::JumpDialog));
        assert_eq!(m.resolve(&KeyChord::key(vk::F)), Some(Command::IncrementalSearchDialog));
        assert_eq!(m.resolve(&KeyChord::key(vk::I)), Some(Command::DirectoryInformation));
        assert_eq!(
            m.resolve(&KeyChord::new(vk::R, false, true, false)),
            Some(Command::RenameSequenceDialog)
        );
        // 既定キーを持たないコマンドは未割当（個人 config で付ける）。
        assert_eq!(m.resolve(&KeyChord::key(vk::OEM_6)), None); // ] = pathHistoryDialog は個人設定
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
            Some(Command::NewFiler)
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::W, true, false, false)),
            Some(Command::Exit)
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
        // Shift＋上下/PageUp/Down＝選択しながら移動（cursorXxx({select:true})）。
        let m = KeyMap::default();
        for (vk, cmd) in [
            (vk::UP, Command::CursorUp),
            (vk::DOWN, Command::CursorDown),
            (vk::PRIOR, Command::CursorPageUp),
            (vk::NEXT, Command::CursorPageDown),
        ] {
            assert!(matches!(
                m.resolve_call(&KeyChord::new(vk, false, true, false)),
                Some(Call::Builtin { command, args })
                    if command == cmd && args == vec![serde_json::json!({ "select": true })]
            ));
        }
        // Shift+Space＝反転＋上移動（markToggle({cursorMove:-1})）。
        assert!(matches!(
            m.resolve_call(&KeyChord::new(vk::SPACE, false, true, false)),
            Some(Call::Builtin { command, args })
                if command == Command::MarkToggle && args == vec![serde_json::json!({ "cursorMove": -1 })]
        ));
    }

    #[test]
    fn meta_argless_command_has_summary_no_args() {
        // 引数を取らないコマンドは引数・例が空でも summary は ALL から引いた説明文を持つ。
        let m = Command::Copy.meta();
        assert_eq!(m.summary, Command::Copy.summary());
        assert!(!m.summary.is_empty());
        assert_ne!(m.summary, Command::Copy.display_name(), "summary は表示名より詳しい");
        assert!(m.args.is_empty());
        assert!(m.examples.is_empty());
    }

    #[test]
    fn every_command_has_summary() {
        // 全コマンドが空でない説明文を持つ（ALL の 4 列目の埋め忘れ検出）。
        for cmd in Command::all() {
            assert!(!cmd.summary().is_empty(), "{cmd:?} の summary が空");
        }
    }

    #[test]
    fn meta_describes_named_option() {
        // カーソル移動は末尾 Object の select オプションを持つ。
        let args = Command::CursorDown.meta().args;
        assert_eq!(args.len(), 1);
        match args[0].ty {
            ArgType::Options(opts) => {
                assert_eq!(opts.len(), 1);
                assert_eq!(opts[0].name, "select");
                assert_eq!(opts[0].ty, ArgType::Bool);
            }
            other => panic!("Options のはずが {other:?}"),
        }
        assert!(!args[0].required);
    }

    #[test]
    fn meta_sort_has_enum_arg() {
        let args = Command::Sort.meta().args;
        assert!(matches!(args[0].ty, ArgType::Enum(vs) if vs.contains(&"name") && vs.contains(&"size")));
        assert!(args[0].required);
    }

    #[test]
    fn meta_examples_parse_to_their_command() {
        // すべての使用例は、そのコマンドのリテラル引数呼び出し（Builtin）として読めること。
        for cmd in Command::all() {
            for ex in cmd.meta().examples {
                match Call::parse(ex) {
                    Call::Builtin { command, .. } => assert_eq!(
                        command, cmd,
                        "{ex} は {cmd:?} の例だが {command:?} にパースされた"
                    ),
                    Call::Script { source } => {
                        panic!("{cmd:?} の例 {source} が Builtin に簡約できない")
                    }
                }
            }
        }
    }

    #[test]
    fn default_binds_mask() {
        // Y・Shift+P=パスマスク。selectMask は既定キー無し。
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
        assert_eq!(Invocation::parse("cursorDown"), Some(Invocation::bare(Command::CursorDown)));
        // 余分な空白も許容。
        assert_eq!(Invocation::parse("  reload  "), Some(Invocation::bare(Command::Reload)));
        // 引数つき。
        assert_eq!(
            Invocation::parse(r#"changeDirectoryDialog("D:")"#),
            Some(Invocation::new(Command::ChangeDirectoryDialog, vec!["D:".into()]))
        );
        // 複数引数・引数間の空白。
        assert_eq!(
            Invocation::parse(r#"newFiler("a" ,  "b")"#),
            Some(Invocation::new(Command::NewFiler, vec!["a".into(), "b".into()]))
        );
        // 空括弧は引数なし。
        assert_eq!(Invocation::parse("reload()"), Some(Invocation::bare(Command::Reload)));
        // エスケープ（\" と \\）。
        assert_eq!(
            Invocation::parse(r#"reload("say \"hi\"\\")"#),
            Some(Invocation::new(Command::Reload, vec!["say \"hi\"\\".into()]))
        );
    }

    #[test]
    fn invocation_parse_rejects_malformed() {
        assert!(Invocation::parse("Bogus").is_none()); // 未知コマンド
        assert!(Invocation::parse("reload(\"a\"").is_none()); // 閉じ括弧なし
        assert!(Invocation::parse("reload(a)").is_none()); // クォートなし引数
        assert!(Invocation::parse("reload(\"a\" \"b\")").is_none()); // カンマなし
    }

    #[test]
    fn invocation_token_roundtrip() {
        for s in [
            "cursorDown()",
            r#"changeDirectoryDialog("D:")"#,
            r#"newFiler("a", "b")"#,
            r#"reload("say \"hi\"\\")"#,
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
        assert_eq!(sm.get("F4").map(String::as_str), Some(r#"changeDirectoryDialog("D:")"#));
        let back = KeyMap::from_string_map(&sm);
        assert!(matches!(
            back.resolve_call(&KeyChord::key(vk::F4)),
            Some(Call::Builtin { command, args })
                if command == Command::ChangeDirectoryDialog && args == vec![serde_json::json!("D:")]
        ));
    }

    #[test]
    fn empty_value_unbinds_key() {
        // 値が空文字のキーは未バインドになる（既定打ち消し用）。
        let mut sm = KeyMap::default().to_string_map();
        assert_eq!(sm.get("Down").map(String::as_str), Some("cursorDown()"));
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

    #[test]
    fn renamed_tokens_use_original_canonical_names() {
        // 原作正式名へ寄せたトークンが正式名で引け、正式名を返す。
        for (token, cmd) in [
            ("createFileDialog", Command::CreateFileDialog),
            ("pathRegisterDialog", Command::PathRegisterDialog),
            ("newFiler", Command::NewFiler),
            ("exit", Command::Exit),
        ] {
            assert_eq!(Command::from_token(token), Some(cmd));
            assert_eq!(cmd.as_token(), token);
        }
        // 原作 Exit（タブを閉じる）とアプリ終了 End は別物。
        assert_ne!(Command::Exit, Command::End);
    }

    #[test]
    fn original_alias_tokens_resolve_to_canonical() {
        // 原作で別名だったトークンは入力時に正式コマンドへ解決する（出力は正式名）。
        assert_eq!(Command::from_token("CD"), Some(Command::ChangeDirectory));
        assert_eq!(Command::from_token("RegisteredPathDialog"), Some(Command::JumpDialog));
        assert_eq!(Command::from_token("UnPack"), Some(Command::Extract));
        // 別名は出力には現れない（正式名を返す）。
        assert_eq!(Command::ChangeDirectory.as_token(), "changeDirectory");
        assert_eq!(Command::Extract.as_token(), "extract");
        // 引数つきの別名も Invocation で通る（移植したメニューが原作名で書けるように）。
        assert_eq!(
            Invocation::parse(r#"CD("C:\\tmp")"#),
            Some(Invocation::new(Command::ChangeDirectory, vec!["C:\\tmp".into()]))
        );
    }

}
