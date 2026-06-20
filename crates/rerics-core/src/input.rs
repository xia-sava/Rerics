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
    pub const F4: u16 = 0x73;
    pub const F5: u16 = 0x74;
    pub const F7: u16 = 0x76;
    pub const F9: u16 = 0x78;
    pub const F10: u16 = 0x79;
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
    pub const DIVIDE: u16 = 0x6F;
    pub const TAB: u16 = 0x09;
    pub const A: u16 = 0x41;
    pub const C: u16 = 0x43;
    pub const D: u16 = 0x44;
    pub const E: u16 = 0x45;
    pub const F: u16 = 0x46;
    pub const I: u16 = 0x49;
    pub const J: u16 = 0x4A;
    pub const K: u16 = 0x4B;
    pub const L: u16 = 0x4C;
    pub const M: u16 = 0x4D;
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
    pub const D0: u16 = 0x30;
    pub const D1: u16 = 0x31;
    pub const D2: u16 = 0x32;
    pub const D3: u16 = 0x33;
    pub const D4: u16 = 0x34;
    // 記号キー（JIS 配列前提。記号⇔VK の対応は配列依存なので、既定割当に使う際は
    // 実機の VK を確認してから割り当てる）。
    pub const OEM_1: u16 = 0xBA; // JIS: ":" "*"
    pub const OEM_PLUS: u16 = 0xBB; // JIS: ";" "+"
    pub const OEM_3: u16 = 0xC0; // JIS: "@" "`"
    pub const OEM_4: u16 = 0xDB; // JIS: "[" "{"
    pub const OEM_5: u16 = 0xDC; // JIS: "\\" "|"（￥）
    pub const OEM_6: u16 = 0xDD; // JIS: "]" "}"
}

/// ファイラのコマンド（段階的に拡張していく）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    ApplicationExit,
    End,
    Restart,
    Quit,
}

impl Command {
    /// コマンドと設定トークン名の対応表（双方向変換の単一の出どころ）。
    const ALL: &'static [(Command, &'static str)] = {
        use Command::*;
        &[
            (CursorUp, "CursorUp"),
            (CursorDown, "CursorDown"),
            (CursorTop, "CursorTop"),
            (CursorEnd, "CursorEnd"),
            (CursorPageUp, "CursorPageUp"),
            (CursorPageDown, "CursorPageDown"),
            (SetCursorPosition, "SetCursorPosition"),
            (EnterDir, "EnterDir"),
            (View, "View"),
            (ToParent, "ToParent"),
            (ToRoot, "ToRoot"),
            (HistoryBack, "HistoryBack"),
            (HistoryForward, "HistoryForward"),
            (PathHistoryDialog, "PathHistoryDialog"),
            (ChangeDirectory, "ChangeDirectory"),
            (ChangeDirectoryDialog, "ChangeDirectoryDialog"),
            (ChangeDrive, "ChangeDrive"),
            (ChangeDriveDialog, "ChangeDriveDialog"),
            (JumpDialog, "JumpDialog"),
            (RegisterPath, "RegisterPath"),
            (IncrementalSearchDialog, "IncrementalSearchDialog"),
            (DirectoryInformation, "DirectoryInformation"),
            (RenameSequenceDialog, "RenameSequenceDialog"),
            (FocusLeft, "FocusLeft"),
            (FocusRight, "FocusRight"),
            (MarkToggle, "MarkToggle"),
            (SelectAll, "SelectAll"),
            (ClearAll, "ClearAll"),
            (ReverseAll, "ReverseAll"),
            (SelectAllFile, "SelectAllFile"),
            (ReverseAllFile, "ReverseAllFile"),
            (Reload, "Reload"),
            (SortByName, "SortByName"),
            (SortByExtension, "SortByExtension"),
            (SortBySize, "SortBySize"),
            (SortByDate, "SortByDate"),
            (Sort, "Sort"),
            (SortDialog, "SortDialog"),
            (SortReverseToggle, "SortReverseToggle"),
            (PageNext, "PageNext"),
            (PagePrevious, "PagePrevious"),
            (NewTab, "NewTab"),
            (CloseTab, "CloseTab"),
            (MakeDirectory, "MakeDirectory"),
            (Copy, "Copy"),
            (Move, "Move"),
            (SwapPath, "SwapPath"),
            (OppositeToCurrent, "OppositeToCurrent"),
            (CurrentToOpposite, "CurrentToOpposite"),
            (Rename, "Rename"),
            (Delete, "Delete"),
            (SendToRecycled, "SendToRecycled"),
            (CreateShortcut, "CreateShortcut"),
            (ClipCopy, "ClipCopy"),
            (ClipCut, "ClipCut"),
            (ClipPaste, "ClipPaste"),
            (CreateFile, "CreateFile"),
            (ViewFile, "ViewFile"),
            (Edit, "Edit"),
            (PropertyDialog, "PropertyDialog"),
            (Compress, "Compress"),
            (Extract, "Extract"),
            (NextDrive, "NextDrive"),
            (PreviousDrive, "PreviousDrive"),
            (PathMask, "PathMask"),
            (SelectMask, "SelectMask"),
            (OpenTaskManager, "OpenTaskManager"),
            (OpenSettings, "OpenSettings"),
            (KeyBindsDialog, "KeyBindsDialog"),
            (CopyLog, "CopyLog"),
            (ClearLog, "ClearLog"),
            (MaximizeLeft, "MaximizeLeft"),
            (MaximizeRight, "MaximizeRight"),
            (MaximizeLeftForce, "MaximizeLeftForce"),
            (MaximizeRightForce, "MaximizeRightForce"),
            (BorderLeft, "BorderLeft"),
            (BorderRight, "BorderRight"),
            (BorderReset, "BorderReset"),
            (CursorOpposite, "CursorOpposite"),
            (SelectFile, "SelectFile"),
            (Refresh, "Refresh"),
            (Nop, "Nop"),
            (MaximizeCurrent, "MaximizeCurrent"),
            (MaximizeWindow, "MaximizeWindow"),
            (MinimizeWindow, "MinimizeWindow"),
            (ApplicationExit, "ApplicationExit"),
            (End, "End"),
            (Restart, "Restart"),
            (Quit, "Quit"),
        ]
    };

    /// 設定トークン名を返す。
    pub fn as_token(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(c, _)| *c == self)
            .map(|(_, s)| *s)
            .unwrap_or("")
    }

    /// 設定トークン名から解釈する。
    pub fn from_token(s: &str) -> Option<Command> {
        Self::ALL
            .iter()
            .find(|(_, t)| *t == s)
            .map(|(c, _)| *c)
    }

    /// 全コマンドを列挙する（設定 UI 用）。
    pub fn all() -> impl Iterator<Item = Command> {
        Self::ALL.iter().map(|(c, _)| *c)
    }
}

/// 「コマンド＋引数」一回分の呼び出し。キーバインド・メニュー・スクリプトの共通入口。
///
/// 引数なしコマンドは `args` が空。引数文字列はマクロ展開前の生の値（`<I:…>` 等を含み得る）で、
/// 実行直前に展開する。
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
    (vk::F4, "F4"),
    (vk::F5, "F5"),
    (vk::F7, "F7"),
    (vk::F9, "F9"),
    (vk::F10, "F10"),
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
    (vk::DIVIDE, "NumPad/"),
    (vk::TAB, "Tab"),
    (vk::OEM_1, ":"),
    (vk::OEM_PLUS, ";"),
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
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
        let (key, mods) = parts.split_last()?;
        let mut chord = Self::key(name_to_vk(key)?);
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" => chord.alt = true,
                _ => return None,
            }
        }
        Some(chord)
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
    /// デフォルトのキーバインド＝**原作 Records 標準**（実装済みコマンド分のみ）。
    /// 未実装/引数付きの原作割当は載せず、コマンド実装時に活性化する。個人設定は
    /// 実機の config.toml で上乗せする方針（`(キーバインド資料)` 参照）。
    /// タブ操作のみ原作 Dock 相当（Ctrl+Tab 等）を補う。
    fn default() -> Self {
        use Command::*;
        let mut m = Self::new();
        // カーソル移動（原作: End/Home Control / PageUp/Down）。
        m.bind(KeyChord::key(vk::UP), CursorUp);
        m.bind(KeyChord::key(vk::DOWN), CursorDown);
        m.bind(KeyChord::key(vk::PRIOR), CursorPageUp);
        m.bind(KeyChord::key(vk::NEXT), CursorPageDown);
        m.bind(KeyChord::new(vk::HOME, true, false, false), CursorTop);
        m.bind(KeyChord::new(vk::END, true, false, false), CursorEnd);
        // Shift＋上下/PageUp/Down＝選択しながら移動（原作 CursorXxx(True)）。
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
        // 侵入・親・ルート・履歴・フォーカス（原作: Enter=Open / BackSpace / Pipe / Alt+←→）。
        m.bind(KeyChord::key(vk::RETURN), EnterDir);
        m.bind(KeyChord::key(vk::BACK), ToParent);
        m.bind(KeyChord::key(vk::OEM_5), ToRoot);
        m.bind(KeyChord::new(vk::LEFT, false, false, true), HistoryBack);
        m.bind(KeyChord::new(vk::RIGHT, false, false, true), HistoryForward);
        m.bind(KeyChord::key(vk::LEFT), FocusLeft);
        m.bind(KeyChord::key(vk::RIGHT), FocusRight);
        m.bind_inv(
            KeyChord::key(vk::F4),
            Invocation::new(ChangeDirectory, vec!["<I:ディレクトリの入力>".into()]),
        );
        m.bind_inv(
            KeyChord::new(vk::F4, false, true, false),
            Invocation::new(ChangeDirectory, vec!["<FOLDERDIALOG:ディレクトリの選択>".into()]),
        );
        m.bind(KeyChord::key(vk::J), JumpDialog);
        // 選択（原作: Space=ReverseFile / A=ReverseAllFile・Shift+A=ReverseAll・Ctrl+A=SelectAll / Home=ClearAll）。
        m.bind(KeyChord::key(vk::SPACE), MarkToggle);
        // Shift+Space＝反転＋カーソル上移動（原作 ReverseFile(-1)）。
        m.bind_inv(
            KeyChord::new(vk::SPACE, false, true, false),
            Invocation::new(MarkToggle, vec!["-1".into()]),
        );
        m.bind(KeyChord::key(vk::A), ReverseAllFile);
        m.bind(KeyChord::new(vk::A, false, true, false), ReverseAll);
        m.bind(KeyChord::new(vk::A, true, false, false), SelectAll);
        m.bind(KeyChord::key(vk::HOME), ClearAll);
        // 検索（原作: F=IncrementalSearchDialog）。
        m.bind(KeyChord::key(vk::F), IncrementalSearchDialog);
        // ファイル操作（原作: C/M/D/R・Shift+R=連番・K=mkdir・P=圧縮・U=展開）。
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
        // 表示・ペイン（原作: V=View / O・Shift+O / Ctrl+←→=最大化 / Y・Shift+P=マスク）。
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
        // ドライブ（原作: Shift+←→）。
        m.bind(KeyChord::new(vk::LEFT, false, true, false), PreviousDrive);
        m.bind(KeyChord::new(vk::RIGHT, false, true, false), NextDrive);
        // 情報・システム（原作: I=使用量 / Esc=タスク / Shift+Home=再読込・F5 / Shift+F1=設定 / Q=Quit）。
        m.bind(KeyChord::key(vk::I), DirectoryInformation);
        m.bind(KeyChord::key(vk::ESCAPE), OpenTaskManager);
        m.bind(KeyChord::new(vk::HOME, false, true, false), Reload);
        m.bind(KeyChord::key(vk::F5), Reload);
        m.bind(KeyChord::new(vk::F1, false, true, false), OpenSettings);
        m.bind(KeyChord::key(vk::Q), Quit);
        m.bind(KeyChord::new(vk::Q, false, true, false), Restart);
        // ウィンドウ操作（原作: Alt+PageUp=最大化 / Alt+PageDown=最小化）。
        m.bind(KeyChord::new(vk::PRIOR, false, false, true), MaximizeWindow);
        m.bind(KeyChord::new(vk::NEXT, false, false, true), MinimizeWindow);
        // タブ（原作 Dock 相当・xia GlobalKeyBinds 準拠）。
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
    fn default_binds_select_and_reload() {
        // 原作: A=ReverseAllFile・Shift+A=ReverseAll・Ctrl+A=SelectAll、Home=ClearAll、
        // Esc=TaskManagerDialog、F5/Shift+Home=Reload。
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
        // 今セッションのコマンドが原作キーで活性化していること。
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
        // F4＝入力マクロつき ChangeDirectory・Shift+F4＝フォルダ選択マクロつき。
        assert_eq!(m.resolve(&KeyChord::key(vk::F4)), Some(Command::ChangeDirectory));
        assert_eq!(
            m.resolve_inv(&KeyChord::key(vk::F4)),
            Some(&Invocation::new(Command::ChangeDirectory, vec!["<I:ディレクトリの入力>".into()]))
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
        // 原作標準にキーが無いコマンドは既定では未割当（個人 config で付ける）。
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
        // 原作: K=MakeDirectoryDialog。
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
        // 原作: P=PackDialog（圧縮）・U=UnPack（展開）。
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
        // 原作: Y=PathMaskDialog・Shift+P=PathMaskDialog。SelectMask は標準キー無し。
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
    fn rebind_inverts_updown() {
        let mut m = KeyMap::default();
        m.bind(KeyChord::key(vk::UP), Command::CursorDown);
        m.bind(KeyChord::key(vk::DOWN), Command::CursorUp);
        assert_eq!(m.resolve(&KeyChord::key(vk::DOWN)), Some(Command::CursorUp));
        assert_eq!(m.resolve(&KeyChord::key(vk::UP)), Some(Command::CursorDown));
    }
}
