//! 設定/状態ファイルの保存先解決・TOML 汎用読み書き・状態データモデル・
//! 設定（config.toml）の埋め込みデフォルト＋差分マージ・作業領域クランプ。UI 非依存。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::file_list::{Colors, Column, default_columns};
use crate::input::KeyMap;

/// 設定/状態ファイルの保存先ディレクトリを返す。
///
/// 実行ファイルと同じディレクトリに `Rerics.portable` があればそのディレクトリを
/// 使う（ポータブルモード）。無ければ `%APPDATA%\Rerics`。いずれも解決できない
/// 場合はカレントディレクトリ。この関数はディレクトリを作らない。
pub fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("Rerics.portable").exists() {
                return dir.to_path_buf();
            }
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            return PathBuf::from(appdata).join("Rerics");
        }
    }
    PathBuf::from(".")
}

/// ユーザ設定ファイル（手編集想定）のパス。
pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

/// 自動更新される状態ファイルのパス。
pub fn state_path() -> PathBuf {
    data_dir().join("state.toml")
}

/// TOML ファイルを読んでデシリアライズする。ファイルが無い・読めない・
/// パースに失敗したいずれの場合も `T::default()` を返す。
pub fn load_toml<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// 値を TOML としてファイルへ書き出す。親ディレクトリは自動で用意する。
/// 直列化エラーは `InvalidData` の `io::Error` に変換する。
pub fn save_toml<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, text)
}

/// ファイル一覧などのフォント指定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontSpec {
    pub family: String,
    /// 論理 pt 相当（GUI 層で DPI スケールする）。
    pub size: i32,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self { family: "BIZ UDGothic".to_owned(), size: 13 }
    }
}

/// レイアウトの寸法（余白・各部の高さ・スクロールバー幅。すべて論理 px）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub margin: i32,
    pub gap: i32,
    pub bar_height: i32,
    pub bar_gap: i32,
    pub status_bar_height: i32,
    pub tab_height: i32,
    pub log_height: i32,
    pub log_gap: i32,
    pub scrollbar_width: i32,
    /// 左右ペイン間のスプリッタ（境界線）の幅。ここをドラッグして分割比を変える。
    pub splitter_width: i32,
    /// ペイン最大化時に反対ペインへ残す幅（原作 Other/MaxmizeMargin 相当）。
    pub maximize_margin: i32,
    /// 境界線をキーで動かす1回あたりの移動量（原作 Other/BorderUnit 相当）。
    pub border_unit: i32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            margin: 2,
            gap: 3,
            bar_height: 20,
            bar_gap: 2,
            status_bar_height: 20,
            tab_height: 24,
            log_height: 96,
            log_gap: 2,
            scrollbar_width: 7,
            splitter_width: 4,
            maximize_margin: 200,
            border_unit: 50,
        }
    }
}

/// アプリ全体の設定。デフォルトは埋め込み `default.toml`、ユーザ `config.toml` は
/// デフォルトとの差分のみを記録し、適用時に再帰マージする。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub font: FontSpec,
    pub layout: Layout,
    pub colors: Colors,
    pub columns: Vec<Column>,
    /// キーバインド（チョード文字列 → コマンドトークン）。
    pub keybinds: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: FontSpec::default(),
            layout: Layout::default(),
            colors: Colors::default(),
            columns: default_columns(),
            keybinds: KeyMap::default().to_string_map(),
        }
    }
}

/// 埋め込みのデフォルト設定（システム保持。ユーザは触れない）。
pub const DEFAULT_CONFIG_TOML: &str = include_str!("default.toml");

/// `over` を `base` へ再帰マージする（テーブルはキー単位で上書き、それ以外は丸ごと置換）。
fn deep_merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Table(b), Value::Table(o)) => {
            for (k, ov) in o {
                match b.get_mut(k) {
                    Some(bv) => deep_merge(bv, ov),
                    None => {
                        b.insert(k.clone(), ov.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

/// `eff` のうち `def` と異なる部分だけを抜き出す（テーブルは再帰、配列・スカラは丸ごと比較）。
/// 差分が無ければ `None`。
fn deep_diff(eff: &Value, def: &Value) -> Option<Value> {
    match (eff, def) {
        (Value::Table(e), Value::Table(d)) => {
            let mut out = toml::map::Map::new();
            for (k, ev) in e {
                match d.get(k) {
                    Some(dv) => {
                        if let Some(diff) = deep_diff(ev, dv) {
                            out.insert(k.clone(), diff);
                        }
                    }
                    None => {
                        out.insert(k.clone(), ev.clone());
                    }
                }
            }
            if out.is_empty() { None } else { Some(Value::Table(out)) }
        }
        (e, d) => {
            if e == d { None } else { Some(e.clone()) }
        }
    }
}

fn invalid_data<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
}

impl Config {
    /// 埋め込みデフォルトにユーザ `config.toml` を再帰マージして読み込む。
    pub fn load() -> Self {
        Self::load_from(&config_path())
    }

    /// 指定パスのユーザ設定を埋め込みデフォルトへマージして読み込む。
    pub fn load_from(path: &Path) -> Self {
        let mut base: Value =
            toml::from_str(DEFAULT_CONFIG_TOML).expect("埋め込み default.toml が不正");
        let user: Option<Value> = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok());
        if let Some(u) = user {
            deep_merge(&mut base, &u);
        }
        base.try_into().unwrap_or_default()
    }

    /// 実効値とデフォルトの差分だけを `config.toml` へ書き出す。
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&config_path())
    }

    /// 差分を指定パスへ書き出す。
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let eff = Value::try_from(self).map_err(invalid_data)?;
        let def: Value = toml::from_str(DEFAULT_CONFIG_TOML).map_err(invalid_data)?;
        let diff = deep_diff(&eff, &def).unwrap_or_else(|| Value::Table(toml::map::Map::new()));
        let text = toml::to_string_pretty(&diff).map_err(invalid_data)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)
    }

    /// 設定からキーマップを組む。
    pub fn keymap(&self) -> KeyMap {
        KeyMap::from_string_map(&self.keybinds)
    }
}

/// ウィンドウの位置・サイズ・最大化状態。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

/// 1タブ分の状態（左右ペインのパスとアクティブ側）。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TabState {
    pub left: String,
    pub right: String,
    #[serde(default)]
    pub active_right: bool,
}

/// 自動保存される全体状態。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<TabState>,
    #[serde(default)]
    pub active_tab: usize,
    /// 左ペインの幅比（0.0〜1.0）。スプリッタ位置の永続化。
    #[serde(default = "default_split_ratio")]
    pub split_ratio: f64,
}

fn default_split_ratio() -> f64 {
    0.5
}

impl Default for State {
    fn default() -> Self {
        Self {
            window: None,
            tabs: Vec::new(),
            active_tab: 0,
            split_ratio: default_split_ratio(),
        }
    }
}

impl State {
    /// 状態ファイルから読み込む（無ければ既定）。
    pub fn load() -> Self {
        load_toml(&state_path())
    }

    /// 状態ファイルへ保存する。
    pub fn save(&self) -> std::io::Result<()> {
        save_toml(&state_path(), self)
    }

    /// アクティブなタブ状態を返す（範囲外や空なら `None`）。
    pub fn active(&self) -> Option<&TabState> {
        self.tabs.get(self.active_tab)
    }
}

/// 矩形 (x, y, w, h) を作業領域 work=(left, top, right, bottom) 内に収めるよう
/// 補正した左上座標 (x, y) を返す。サイズは変えない。
/// 右/下がはみ出す場合は左/上へ寄せ、それでも左/上が作業領域より手前なら
/// left/top に合わせる。
pub fn clamp_to_work(x: i32, y: i32, w: i32, h: i32, work: (i32, i32, i32, i32)) -> (i32, i32) {
    let (left, top, right, bottom) = work;
    let nx = if right - w < left {
        left
    } else {
        (x.min(right - w)).max(left)
    };
    let ny = if bottom - h < top {
        top
    } else {
        (y.min(bottom - h)).max(top)
    };
    (nx, ny)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// デフォルト値を変えたら `cargo test -- --ignored generate_default_toml` で再生成する。
    #[test]
    #[ignore]
    fn generate_default_toml() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/default.toml");
        std::fs::write(&path, text).unwrap();
    }

    #[test]
    fn embedded_default_matches_rust_defaults() {
        let parsed: Config = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn load_missing_user_is_default() {
        let path = std::env::temp_dir().join("rerics_cfg_missing_xyz.toml");
        let _ = std::fs::remove_file(&path);
        assert_eq!(Config::load_from(&path), Config::default());
    }

    #[test]
    fn merge_overrides_single_color_and_keybind() {
        let path = std::env::temp_dir().join("rerics_cfg_partial.toml");
        std::fs::write(
            &path,
            "[colors]\ncursor = \"#ff0000\"\n\n[keybinds]\n\"Ctrl+T\" = \"Reload\"\n",
        )
        .unwrap();
        let cfg = Config::load_from(&path);
        // 上書きしたキーだけ変わり、他はデフォルトのまま。
        assert_eq!(cfg.colors.cursor, crate::Rgb::new(0xff, 0, 0));
        assert_eq!(cfg.colors.background, Config::default().colors.background);
        assert_eq!(cfg.keybinds.get("Ctrl+T").map(String::as_str), Some("Reload"));
        assert_eq!(cfg.keybinds.get("Down").map(String::as_str), Some("CursorDown"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_writes_only_diff() {
        let path = std::env::temp_dir().join("rerics_cfg_savediff.toml");
        let _ = std::fs::remove_file(&path);
        let mut cfg = Config::default();
        cfg.colors.cursor = crate::Rgb::new(0x12, 0x34, 0x56);
        cfg.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // 変更点だけが書かれる（他のセクションは出ない）。
        assert!(text.contains("cursor = \"#123456\""));
        assert!(!text.contains("[layout]"));
        assert!(!text.contains("[[columns]]"));
        assert!(!text.contains("background"));
        // 読み戻すと実効値が一致。
        let back = Config::load_from(&path);
        assert_eq!(back, cfg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_default_writes_empty() {
        let path = std::env::temp_dir().join("rerics_cfg_empty.toml");
        let _ = std::fs::remove_file(&path);
        Config::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.trim().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn config_keymap_resolves() {
        let cfg = Config::default();
        let km = cfg.keymap();
        use crate::{Command, KeyChord, vk};
        assert_eq!(km.resolve(&KeyChord::key(vk::DOWN)), Some(Command::CursorDown));
        assert_eq!(
            km.resolve(&KeyChord::new(vk::T, true, false, false)),
            Some(Command::NewTab)
        );
    }

    #[test]
    fn window_state_roundtrip() {
        let st = WindowState { x: 10, y: 20, width: 800, height: 600, maximized: true };
        let s = toml::to_string(&st).unwrap();
        let back: WindowState = toml::from_str(&s).unwrap();
        assert_eq!(st, back);
    }

    #[test]
    fn state_roundtrip() {
        let st = State {
            window: Some(WindowState { x: 1, y: 2, width: 3, height: 4, maximized: false }),
            ..State::default()
        };
        let s = toml::to_string(&st).unwrap();
        let back: State = toml::from_str(&s).unwrap();
        assert_eq!(st.window, back.window);
    }

    #[test]
    fn default_state_serializes_without_window() {
        let st = State::default();
        assert!(st.window.is_none());
        let s = toml::to_string(&st).unwrap();
        assert!(!s.contains("[window]"));
    }

    #[test]
    fn load_missing_returns_default() {
        let path = std::env::temp_dir().join("rerics_test_missing_does_not_exist.toml");
        let _ = std::fs::remove_file(&path);
        let st: State = load_toml(&path);
        assert!(st.window.is_none());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let path = std::env::temp_dir().join("rerics_test_state_roundtrip.toml");
        let _ = std::fs::remove_file(&path);
        let st = State {
            window: Some(WindowState { x: 100, y: 200, width: 960, height: 560, maximized: true }),
            ..State::default()
        };
        save_toml(&path, &st).unwrap();
        let back: State = load_toml(&path);
        assert_eq!(st.window, back.window);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn state_with_tabs_roundtrip() {
        let st = State {
            window: None,
            tabs: vec![
                TabState { left: "C:\\a".into(), right: "C:\\b".into(), active_right: false },
                TabState { left: "C:\\c".into(), right: "C:\\d".into(), active_right: true },
            ],
            active_tab: 1,
            ..State::default()
        };
        let s = toml::to_string(&st).unwrap();
        let back: State = toml::from_str(&s).unwrap();
        assert_eq!(st.tabs, back.tabs);
        assert_eq!(st.active_tab, back.active_tab);
        assert_eq!(back.active().unwrap().left, "C:\\c");
        assert!(back.active().unwrap().active_right);
    }

    #[test]
    fn default_state_has_no_tabs() {
        let st = State::default();
        assert!(st.tabs.is_empty());
        assert!(st.active().is_none());
        let s = toml::to_string(&st).unwrap();
        assert!(!s.contains("[[tabs]]"));
    }

    #[test]
    fn clamp_inside_is_unchanged() {
        let work = (0, 0, 1920, 1080);
        assert_eq!(clamp_to_work(100, 100, 400, 300, work), (100, 100));
    }

    #[test]
    fn clamp_bottom_right_overflow_moves_to_top_left() {
        let work = (0, 0, 1000, 800);
        assert_eq!(clamp_to_work(900, 700, 400, 300, work), (600, 500));
    }

    #[test]
    fn clamp_negative_origin_moves_to_work_origin() {
        let work = (0, 0, 1920, 1080);
        assert_eq!(clamp_to_work(-50, -30, 400, 300, work), (0, 0));
    }

    #[test]
    fn clamp_window_larger_than_work_pins_top_left() {
        let work = (10, 20, 410, 220);
        assert_eq!(clamp_to_work(100, 100, 800, 600, work), (10, 20));
    }
}
