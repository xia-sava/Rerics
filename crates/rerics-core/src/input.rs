//! コマンドとキーバインドの土台（UI 非依存）。
//!
//! GUI 層はキー入力を [`KeyChord`] にして [`KeyMap::resolve`] で [`Command`] に解決し、
//! その Command を自前で実行する。全コマンドが自由にリマップ可能。

use std::collections::HashMap;

/// 仮想キーコード（Win32 VK と同値の `u16`。winsafe `co::VK` とも一致）。
pub mod vk {
    pub const BACK: u16 = 0x08;
    pub const RETURN: u16 = 0x0D;
    pub const PRIOR: u16 = 0x21; // PageUp
    pub const NEXT: u16 = 0x22; // PageDown
    pub const END: u16 = 0x23;
    pub const HOME: u16 = 0x24;
    pub const LEFT: u16 = 0x25;
    pub const UP: u16 = 0x26;
    pub const RIGHT: u16 = 0x27;
    pub const DOWN: u16 = 0x28;
}

/// ファイラのコマンド（原作 Records の体系へ段階的に拡張していく）。
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
}

/// キー → コマンドの対応表。
#[derive(Debug, Clone, Default)]
pub struct KeyMap {
    map: HashMap<KeyChord, Command>,
}

impl KeyMap {
    pub fn new() -> Self {
        Self::default()
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

    /// 原作 Records 準拠のデフォルト（現状はカーソル/ナビ系のみ。順次拡充）。
    pub fn records_default() -> Self {
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
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_binds_cursor_keys() {
        let m = KeyMap::records_default();
        assert_eq!(m.resolve(&KeyChord::key(vk::DOWN)), Some(Command::CursorDown));
        assert_eq!(m.resolve(&KeyChord::key(vk::UP)), Some(Command::CursorUp));
        assert_eq!(m.resolve(&KeyChord::key(vk::RETURN)), Some(Command::EnterDir));
        assert_eq!(m.resolve(&KeyChord::key(0x00FF)), None);
    }

    #[test]
    fn rebind_inverts_updown() {
        let mut m = KeyMap::records_default();
        m.bind(KeyChord::key(vk::UP), Command::CursorDown);
        m.bind(KeyChord::key(vk::DOWN), Command::CursorUp);
        assert_eq!(m.resolve(&KeyChord::key(vk::DOWN)), Some(Command::CursorUp));
        assert_eq!(m.resolve(&KeyChord::key(vk::UP)), Some(Command::CursorDown));
    }
}
