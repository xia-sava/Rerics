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
    pub const ESCAPE: u16 = 0x1B;
    pub const F5: u16 = 0x74;
    pub const F7: u16 = 0x76;
    pub const TAB: u16 = 0x09;
    pub const A: u16 = 0x41;
    pub const C: u16 = 0x43;
    pub const M: u16 = 0x4D;
    pub const O: u16 = 0x4F;
    pub const R: u16 = 0x52;
    pub const D: u16 = 0x44;
    pub const T: u16 = 0x54;
    pub const W: u16 = 0x57;
    pub const D0: u16 = 0x30;
    pub const D1: u16 = 0x31;
    pub const D2: u16 = 0x32;
    pub const D3: u16 = 0x33;
    pub const D4: u16 = 0x34;
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
    EnterDir,
    ToParent,
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
    CreateFile,
    NextDrive,
    PreviousDrive,
    PathMask,
    SelectMask,
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
            (EnterDir, "EnterDir"),
            (ToParent, "ToParent"),
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
            (CreateFile, "CreateFile"),
            (NextDrive, "NextDrive"),
            (PreviousDrive, "PreviousDrive"),
            (PathMask, "PathMask"),
            (SelectMask, "SelectMask"),
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
    (vk::ESCAPE, "Esc"),
    (vk::F5, "F5"),
    (vk::F7, "F7"),
    (vk::TAB, "Tab"),
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
    map: HashMap<KeyChord, Command>,
}

impl Default for KeyMap {
    /// デフォルトのキーバインド（現状はカーソル/ナビ/マーク系のみ。順次拡充）。
    fn default() -> Self {
        use Command::*;
        let mut m = Self::new();
        m.bind(KeyChord::key(vk::UP), CursorUp);
        m.bind(KeyChord::key(vk::DOWN), CursorDown);
        m.bind(KeyChord::key(vk::HOME), CursorTop);
        m.bind(KeyChord::key(vk::END), CursorEnd);
        m.bind(KeyChord::key(vk::PRIOR), CursorPageUp);
        m.bind(KeyChord::key(vk::NEXT), CursorPageDown);
        m.bind(KeyChord::key(vk::RETURN), EnterDir);
        m.bind(KeyChord::key(vk::BACK), ToParent);
        m.bind(KeyChord::key(vk::LEFT), FocusLeft);
        m.bind(KeyChord::key(vk::RIGHT), FocusRight);
        m.bind(KeyChord::key(vk::SPACE), MarkToggle);
        m.bind(KeyChord::key(vk::F5), Reload);
        m.bind(KeyChord::key(vk::ESCAPE), ClearAll);
        m.bind(KeyChord::new(vk::A, true, false, false), SelectAll);
        m.bind(KeyChord::new(vk::D1, true, false, false), SortByName);
        m.bind(KeyChord::new(vk::D2, true, false, false), SortByExtension);
        m.bind(KeyChord::new(vk::D3, true, false, false), SortBySize);
        m.bind(KeyChord::new(vk::D4, true, false, false), SortByDate);
        m.bind(KeyChord::new(vk::D0, true, false, false), SortReverseToggle);
        m.bind(KeyChord::new(vk::TAB, true, false, false), PageNext);
        m.bind(KeyChord::new(vk::TAB, true, true, false), PagePrevious);
        m.bind(KeyChord::new(vk::T, true, false, false), NewTab);
        m.bind(KeyChord::new(vk::W, true, false, false), CloseTab);
        m.bind(KeyChord::key(vk::F7), MakeDirectory);
        m.bind(KeyChord::key(vk::C), Copy);
        m.bind(KeyChord::key(vk::M), Move);
        m.bind(KeyChord::key(vk::O), OppositeToCurrent);
        m.bind(KeyChord::new(vk::O, false, true, false), CurrentToOpposite);
        m.bind(KeyChord::key(vk::R), Rename);
        m.bind(KeyChord::key(vk::D), Delete);
        m.bind(KeyChord::new(vk::F7, false, true, false), CreateFile);
        m.bind(KeyChord::new(vk::RIGHT, false, true, false), NextDrive);
        m.bind(KeyChord::new(vk::LEFT, false, true, false), PreviousDrive);
        m.bind(KeyChord::key(vk::W), PathMask);
        m.bind(KeyChord::new(vk::W, false, true, false), SelectMask);
        m
    }
}

impl KeyMap {
    /// バインドが空のマップを作る。
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn bind(&mut self, chord: KeyChord, cmd: Command) -> &mut Self {
        self.map.insert(chord, cmd);
        self
    }

    pub fn unbind(&mut self, chord: &KeyChord) {
        self.map.remove(chord);
    }

    pub fn resolve(&self, chord: &KeyChord) -> Option<Command> {
        self.map.get(chord).copied()
    }

    /// トークン文字列のマップ（チョード→コマンド）からキーマップを組む。
    /// 解釈できない行は無視する。
    pub fn from_string_map(map: &BTreeMap<String, String>) -> Self {
        let mut m = Self::new();
        for (k, v) in map {
            if let (Some(chord), Some(cmd)) = (KeyChord::parse(k), Command::from_token(v)) {
                m.bind(chord, cmd);
            }
        }
        m
    }

    /// トークン文字列のマップ（チョード→コマンド）へ変換する。
    pub fn to_string_map(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (chord, cmd) in &self.map {
            if let Some(tok) = chord.to_token() {
                out.insert(tok, cmd.as_token().to_owned());
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
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(&KeyChord::new(vk::A, true, false, false)),
            Some(Command::SelectAll)
        );
        assert_eq!(m.resolve(&KeyChord::key(vk::A)), None);
        assert_eq!(m.resolve(&KeyChord::key(vk::F5)), Some(Command::Reload));
        assert_eq!(m.resolve(&KeyChord::key(vk::ESCAPE)), Some(Command::ClearAll));
    }

    #[test]
    fn default_binds_sort() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(&KeyChord::new(vk::D2, true, false, false)),
            Some(Command::SortByExtension)
        );
        assert_eq!(
            m.resolve(&KeyChord::new(vk::D0, true, false, false)),
            Some(Command::SortReverseToggle)
        );
        assert_eq!(m.resolve(&KeyChord::key(vk::D2)), None);
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
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(&KeyChord::key(vk::F7)),
            Some(Command::MakeDirectory)
        );
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
    fn default_binds_create_file() {
        let m = KeyMap::default();
        assert_eq!(
            m.resolve(&KeyChord::new(vk::F7, false, true, false)),
            Some(Command::CreateFile)
        );
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
    fn default_binds_mask() {
        let m = KeyMap::default();
        assert_eq!(m.resolve(&KeyChord::key(vk::W)), Some(Command::PathMask));
        assert_eq!(
            m.resolve(&KeyChord::new(vk::W, false, true, false)),
            Some(Command::SelectMask)
        );
    }

    #[test]
    fn chord_token_roundtrip() {
        for s in ["Up", "Ctrl+A", "Ctrl+Shift+Tab", "Shift+F7", "C", "Ctrl+0", "Esc"] {
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
    fn rebind_inverts_updown() {
        let mut m = KeyMap::default();
        m.bind(KeyChord::key(vk::UP), Command::CursorDown);
        m.bind(KeyChord::key(vk::DOWN), Command::CursorUp);
        assert_eq!(m.resolve(&KeyChord::key(vk::DOWN)), Some(Command::CursorUp));
        assert_eq!(m.resolve(&KeyChord::key(vk::UP)), Some(Command::CursorDown));
    }
}
