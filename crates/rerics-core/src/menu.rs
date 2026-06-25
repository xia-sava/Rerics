//! 名前付きメニューの定義と解決。原作の `Menu("名前")` 相当。
//!
//! メニューは「ラベル＋コマンド呼び出し」の並びをデータで持ち、名前で引いてポップアップとして
//! 開く。定義元は config の `[[menu]]` とスクリプトの `registerMenu` の2系統で、どちらも
//! [`MenuRegistry`] へ集約する（同名は後勝ち）。
//!
//! 項目のコマンドが `Menu("他名")` のときは参照式サブメニューとして展開する。展開は
//! [`MenuRegistry::resolve`] が担い、循環参照は掲示のみの無効項目に落として無限再帰を防ぐ。
//! 解決結果（[`ResolvedItem`] の木）は UI 非依存なので、GUI 側はこれをポップアップへ素直に
//! 変換するだけでよい。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::input::{Command, Invocation};

/// 名前付きメニュー1つ分の定義。`name` で参照し、`Menu("name")` で開く。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuDef {
    /// メニュー名（`Menu("name")` の引数で参照する一意名）。
    pub name: String,
    /// 項目の並び（上から順に表示）。
    #[serde(default)]
    pub items: Vec<MenuItem>,
}

/// メニュー項目1つ。ラベル＋実行するコマンド、またはセパレータ。
///
/// `command` が `Menu("他名")` のときは参照式サブメニューになる（`label` がサブメニュー見出し）。
/// `separator` が true の項目は区切り線で、`label`/`command` は無視する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuItem {
    /// 表示ラベル（`&` の次の文字がアクセスキー）。セパレータでは無視。
    #[serde(default)]
    pub label: String,
    /// 実行するコマンドトークン（[`Invocation::parse`] で解釈）。セパレータでは空。
    #[serde(default)]
    pub command: String,
    /// 区切り線なら true（`label`/`command` を無視）。
    #[serde(default)]
    pub separator: bool,
}

impl MenuItem {
    /// 実行項目を作る。
    pub fn entry(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self { label: label.into(), command: command.into(), separator: false }
    }

    /// 区切り線を作る。
    pub fn separator() -> Self {
        Self { label: String::new(), command: String::new(), separator: true }
    }
}

/// 解決済みメニュー項目。参照サブメニューを展開し、各項目を [`Invocation`] 化したもの。
/// GUI はこの木をそのままポップアップへ変換できる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedItem {
    /// 実行可能な項目：ラベルと実行する呼び出し。
    Command { label: String, invocation: Invocation },
    /// サブメニュー：見出しラベルと展開済みの子項目。
    Submenu { label: String, items: Vec<ResolvedItem> },
    /// 区切り線。
    Separator,
    /// 解釈できない項目（未知コマンド・参照先メニュー無し・循環参照）。掲示のみ。
    Invalid { label: String, reason: String },
}

/// 名前付きメニューの集合。config 由来とスクリプト登録をまとめ、名前で引く。
/// 同名の追加は後勝ちで置換する（スクリプトが config を上書きできる）。
#[derive(Debug, Clone, Default)]
pub struct MenuRegistry {
    by_name: HashMap<String, MenuDef>,
}

impl MenuRegistry {
    /// 空のレジストリ。
    pub fn new() -> Self {
        Self::default()
    }

    /// 定義の並びから作る（同名は後勝ち）。
    pub fn from_defs(defs: impl IntoIterator<Item = MenuDef>) -> Self {
        let mut reg = Self::new();
        for def in defs {
            reg.insert(def);
        }
        reg
    }

    /// 定義を追加する（同名は後勝ちで置換）。
    pub fn insert(&mut self, def: MenuDef) {
        self.by_name.insert(def.name.clone(), def);
    }

    /// 名前で定義を引く。
    pub fn get(&self, name: &str) -> Option<&MenuDef> {
        self.by_name.get(name)
    }

    /// 登録済みメニュー名を返す（順序は不定）。
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// 名前付きメニューを解決し、表示用の項目木を返す。参照サブメニューは展開し、循環参照は
    /// 無効項目へ落とす。メニューが存在しなければ `None`。
    pub fn resolve(&self, name: &str) -> Option<Vec<ResolvedItem>> {
        let def = self.get(name)?;
        let mut path = vec![name.to_owned()];
        Some(self.resolve_items(&def.items, &mut path))
    }

    /// 項目列を解決する。`path` は現在展開中のメニュー名スタック（循環検出用）。
    fn resolve_items(&self, items: &[MenuItem], path: &mut Vec<String>) -> Vec<ResolvedItem> {
        items.iter().map(|item| self.resolve_item(item, path)).collect()
    }

    fn resolve_item(&self, item: &MenuItem, path: &mut Vec<String>) -> ResolvedItem {
        if item.separator {
            return ResolvedItem::Separator;
        }
        let Some(inv) = Invocation::parse(&item.command) else {
            return ResolvedItem::Invalid {
                label: item.label.clone(),
                reason: format!("コマンドとして解釈できません: {}", item.command),
            };
        };
        // `Menu("他名")` は参照式サブメニューとして展開する。
        if inv.command == Command::Menu {
            let target = inv.args.first().map(String::as_str).unwrap_or("");
            if target.is_empty() {
                return ResolvedItem::Invalid {
                    label: item.label.clone(),
                    reason: "Menu に開くメニュー名がありません".to_owned(),
                };
            }
            if path.iter().any(|n| n == target) {
                return ResolvedItem::Invalid {
                    label: item.label.clone(),
                    reason: format!("メニューが循環参照しています: {target}"),
                };
            }
            let Some(sub) = self.get(target) else {
                return ResolvedItem::Invalid {
                    label: item.label.clone(),
                    reason: format!("メニューが見つかりません: {target}"),
                };
            };
            path.push(target.to_owned());
            let items = self.resolve_items(&sub.items, path);
            path.pop();
            return ResolvedItem::Submenu { label: item.label.clone(), items };
        }
        ResolvedItem::Command { label: item.label.clone(), invocation: inv }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(name: &str, items: Vec<MenuItem>) -> MenuDef {
        MenuDef { name: name.to_owned(), items }
    }

    #[test]
    fn resolves_commands_and_separators() {
        let reg = MenuRegistry::from_defs([menu(
            "編集",
            vec![
                MenuItem::entry("コピー(&C)", "Copy"),
                MenuItem::separator(),
                MenuItem::entry("移動(&M)", "Move"),
            ],
        )]);
        let items = reg.resolve("編集").unwrap();
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[0], ResolvedItem::Command { invocation, .. } if invocation.command == Command::Copy));
        assert!(matches!(items[1], ResolvedItem::Separator));
        assert!(matches!(&items[2], ResolvedItem::Command { invocation, .. } if invocation.command == Command::Move));
    }

    #[test]
    fn unknown_menu_is_none() {
        let reg = MenuRegistry::new();
        assert!(reg.resolve("無い").is_none());
    }

    #[test]
    fn submenu_is_expanded_by_reference() {
        let reg = MenuRegistry::from_defs([
            menu("親", vec![MenuItem::entry("子を開く", "Menu(\"子\")")]),
            menu("子", vec![MenuItem::entry("削除", "Delete")]),
        ]);
        let items = reg.resolve("親").unwrap();
        match &items[0] {
            ResolvedItem::Submenu { label, items } => {
                assert_eq!(label, "子を開く");
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], ResolvedItem::Command { invocation, .. } if invocation.command == Command::Delete));
            }
            other => panic!("サブメニューのはず: {other:?}"),
        }
    }

    #[test]
    fn cyclic_reference_becomes_invalid() {
        let reg = MenuRegistry::from_defs([
            menu("A", vec![MenuItem::entry("Bへ", "Menu(\"B\")")]),
            menu("B", vec![MenuItem::entry("Aへ", "Menu(\"A\")")]),
        ]);
        let items = reg.resolve("A").unwrap();
        // A → B まで展開し、B の中の「Aへ」が循環で無効化される。
        let ResolvedItem::Submenu { items: b_items, .. } = &items[0] else {
            panic!("Bはサブメニュー");
        };
        assert!(matches!(&b_items[0], ResolvedItem::Invalid { reason, .. } if reason.contains("循環")));
    }

    #[test]
    fn missing_reference_is_invalid() {
        let reg = MenuRegistry::from_defs([menu("親", vec![MenuItem::entry("無い子", "Menu(\"無い\")")])]);
        let items = reg.resolve("親").unwrap();
        assert!(matches!(&items[0], ResolvedItem::Invalid { reason, .. } if reason.contains("見つかりません")));
    }

    #[test]
    fn unparseable_command_is_invalid() {
        let reg = MenuRegistry::from_defs([menu("親", vec![MenuItem::entry("変な項目", "ぜんぜん違う")])]);
        let items = reg.resolve("親").unwrap();
        assert!(matches!(&items[0], ResolvedItem::Invalid { .. }));
    }

    #[test]
    fn later_definition_wins() {
        let mut reg = MenuRegistry::new();
        reg.insert(menu("M", vec![MenuItem::entry("旧", "Copy")]));
        reg.insert(menu("M", vec![MenuItem::entry("新", "Move")]));
        let items = reg.resolve("M").unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], ResolvedItem::Command { invocation, .. } if invocation.command == Command::Move));
    }
}
