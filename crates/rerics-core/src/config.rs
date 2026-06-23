//! 設定/状態ファイルの保存先解決・TOML 汎用読み書き・状態データモデル・
//! 設定（config.toml）の埋め込みデフォルト＋差分マージ・作業領域クランプ。UI 非依存。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::Value;

use crate::file_list::{Colors, Column, SizeFormat, SortType, default_columns};
use crate::input::KeyMap;

/// 設定/状態ファイルの保存先ディレクトリを返す。
///
/// 実行ファイルと同じディレクトリに `Rerics.portable` があればそのディレクトリを
/// 使う（ポータブルモード）。無ければ `%APPDATA%\Rerics`。いずれも解決できない
/// 場合はカレントディレクトリ。この関数はディレクトリを作らない。
pub fn data_dir() -> PathBuf {
    // テスト/ツール用の明示オーバーライド（最優先・本番は未設定）。
    if let Ok(dir) = std::env::var("RERICS_DATA_DIR")
        && !dir.is_empty() {
            return PathBuf::from(dir);
        }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
            && dir.join("Rerics.portable").exists() {
                return dir.to_path_buf();
            }
    if let Ok(appdata) = std::env::var("APPDATA")
        && !appdata.is_empty() {
            return PathBuf::from(appdata).join("Rerics");
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

/// 入力履歴ファイルのパス。
pub fn history_path() -> PathBuf {
    data_dir().join("history.toml")
}

/// リサイズ可能ダイアログの前回サイズを覚えるファイルのパス。
pub fn dialog_sizes_path() -> PathBuf {
    data_dir().join("dialog-sizes.toml")
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

/// レイアウトの寸法（余白・各部の高さ・スクロールバー幅）。`log_height` のみ行数で、
/// それ以外はすべて論理 px。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub margin: i32,
    pub gap: i32,
    pub bar_height: i32,
    pub bar_gap: i32,
    pub status_bar_height: i32,
    pub tab_height: i32,
    /// ログ窓の高さ（行数）。フォントの行高 × この行数でピクセル高を決めるので、
    /// フォントサイズを変えると窓高も追従する。最小 1 行。
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
            log_height: 6,
            log_gap: 2,
            scrollbar_width: 7,
            splitter_width: 4,
            maximize_margin: 200,
            border_unit: 50,
        }
    }
}

/// 配色テーマの選択。`System` は OS のライト/ダーク設定に追従する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Theme {
    Dark,
    Light,
    #[default]
    System,
}


/// `System` を OS 設定で解決した後の実テーマ（ダークかライトのいずれか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum ResolvedTheme {
    #[default]
    Dark,
    Light,
}


/// テーマ別の配色セット。実効色は解決済みテーマに応じてどちらかを選ぶ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeColors {
    pub dark: Colors,
    pub light: Colors,
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self { dark: Colors::dark(), light: Colors::light() }
    }
}

/// 登録ディレクトリ（ブックマーク）。`label` で一覧表示し、選ぶと `path` へジャンプする。
/// `shortcut` はジャンプダイアログで割り当てる1キー（原作 RegisteredPaths の Shortcut・
/// 未割当なら空）で、ダイアログ表示中にそのキーを押すと該当行へ直接ジャンプできる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub label: String,
    pub path: String,
    #[serde(default)]
    pub shortcut: String,
}

/// カーソル位置記憶（原作 Cursor/CursorHistory）の設定。ディレクトリ移動の直前に離脱
/// 位置を覚え、再訪時にカーソルを復元する。記憶はセッション内のみで、ファイルには残さない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorSettings {
    /// 位置記憶のオン/オフ（原作既定はオフ）。
    pub history: bool,
    /// 記憶するパス数の上限。超えると古いものから捨てる（原作 CursorHistoryCount・既定 100）。
    pub history_count: usize,
    /// 左右カーソルキーで親ディレクトリへ移動するか（原作 Cursor/CursorToParent・既定オフ）。
    /// オンのとき、アクティブ側ペインで外向きのカーソルキー（左ペインで左／右ペインで右）を
    /// 押すと親へ移動する。オフのときは従来どおり反対ペインへフォーカス移動するのみ。
    pub to_parent: bool,
    /// マーク反転（Space）後にカーソルを下へ動かすか（原作 Cursor/DownAfterSelect・既定オン）。
    /// オフのときはマークしてもカーソルは留まる。移動量を明示した割当（Shift+Space 等）は
    /// この設定に関わらずその量だけ動く。
    pub down_after_select: bool,
}

impl Default for CursorSettings {
    fn default() -> Self {
        Self { history: false, history_count: 100, to_parent: false, down_after_select: true }
    }
}

/// 既定ウィンドウサイズの設定。`fixed_size` が真のとき、前回サイズの復元の代わりに
/// `width`×`height`（論理 px）で毎回起動する。位置は前回値があれば踏襲する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSettings {
    /// 毎回既定サイズで起動する（オフなら前回のサイズを復元する）。
    pub fixed_size: bool,
    /// 既定の幅（論理 px）。
    pub width: i32,
    /// 既定の高さ（論理 px）。
    pub height: i32,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self { fixed_size: false, width: 960, height: 640 }
    }
}

/// 画像ビューアのマウスホイール動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WheelAction {
    /// 前後の画像へ送る（原作既定）。ホイール上＝前・下＝次。
    #[default]
    Navigate,
    /// 拡大／縮小する。ホイール上＝拡大・下＝縮小。
    Zoom,
}

/// 画像ビューアの設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageSettings {
    /// マウスホイールの動作（既定＝送り・原作准拠）。
    pub wheel: WheelAction,
    /// ズーム1段あたりの拡大率（%）。25 なら 1 段で 1.25 倍ずつ拡大／縮小する。
    pub zoom_step_percent: u32,
}

impl Default for ImageSettings {
    fn default() -> Self {
        Self { wheel: WheelAction::Navigate, zoom_step_percent: 25 }
    }
}

/// ファイル一覧アイコンの表示サイズ。`Auto` は行（フォント高）に収める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconSize {
    /// 行（フォント高）に収まるサイズ。行高を増やさない。
    #[default]
    Auto,
    Small,
    Medium,
    Large,
}

impl IconSize {
    /// 表示サイズの論理 px。`Auto` は 0 を返し、呼び出し側が行高に合わせる。
    pub fn logical_px(self) -> i32 {
        match self {
            IconSize::Auto => 0,
            IconSize::Small => 16,
            IconSize::Medium => 24,
            IconSize::Large => 32,
        }
    }
}

/// ファイル一覧のアイコン表示設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IconSettings {
    /// 一覧にシェルアイコンを表示する。
    pub show: bool,
    /// アイコンの表示サイズ。
    pub size: IconSize,
}

impl Default for IconSettings {
    fn default() -> Self {
        Self { show: true, size: IconSize::Auto }
    }
}

/// ファイル操作の事前確認ダイアログ設定。各操作の前に確認ダイアログを出すかどうかを
/// 切り替える。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileOpSettings {
    /// コピーの前に確認する（既定オフ）。
    pub ask_before_copy: bool,
    /// 移動の前に確認する（既定オフ）。
    pub ask_before_move: bool,
    /// 削除・ゴミ箱送りの前に確認する（既定オン）。
    pub ask_before_delete: bool,
    /// 書庫の展開時、書庫名のディレクトリを作ってその中へ取り出す（既定オフ）。
    pub extract_create_directory: bool,
    /// ディレクトリのコピー時、コピー先ディレクトリへ元の属性も複製する（既定オン）。
    pub copy_attribute: bool,
    /// ディレクトリのコピー時、コピー先ディレクトリへ元の作成/更新日時も複製する（既定オン）。
    pub copy_date: bool,
}

impl Default for FileOpSettings {
    fn default() -> Self {
        Self {
            ask_before_copy: false,
            ask_before_move: false,
            ask_before_delete: true,
            extract_create_directory: false,
            copy_attribute: true,
            copy_date: true,
        }
    }
}

/// アプリ全体の設定。デフォルトは埋め込み `default.toml`、ユーザ `config.toml` は
/// デフォルトとの差分のみを記録し、適用時に再帰マージする。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: Theme,
    pub font: FontSpec,
    pub layout: Layout,
    pub colors: ThemeColors,
    pub columns: Vec<Column>,
    /// ファイルサイズ列の表記スタイル。
    pub size_format: SizeFormat,
    /// 列幅を内容に合わせて自動調整する（off なら `columns` の幅をそのまま使う）。
    pub auto_adjust_columns: bool,
    /// 既定のソート種別（state が無い初回起動時の並び順）。
    pub default_sort: SortType,
    /// 日付ソートのときだけ昇降を追加で反転する（古い日付を先頭にできる）。
    pub reverse_sort_date: bool,
    /// 読込・展開の待機スピナーを出すまでの遅延（ミリ秒）。これより速く終わる処理では
    /// スピナーを出さずチラつかせない。0 で即時表示。
    pub progress_delay_ms: u64,
    /// キーバインド（チョード文字列 → コマンドトークン）。ファイラー用。
    pub keybinds: BTreeMap<String, String>,
    /// テキストビューア用キーバインド（チョード文字列 → コマンドトークン）。
    pub keybinds_textviewer: BTreeMap<String, String>,
    /// 画像・動画ビューア用キーバインド（チョード文字列 → コマンドトークン）。
    pub keybinds_imageviewer: BTreeMap<String, String>,
    /// 登録ディレクトリ（ジャンプ先）。
    pub bookmarks: Vec<Bookmark>,
    /// Edit コマンドで開く外部エディタ（実行ファイル名 or パス）。
    pub editor: String,
    /// カーソル位置記憶の設定。
    pub cursor: CursorSettings,
    /// 画像ビューアの設定。
    pub image: ImageSettings,
    /// ファイル一覧アイコンの表示設定。
    pub icons: IconSettings,
    /// ファイル操作の事前確認ダイアログ設定。
    pub file_ops: FileOpSettings,
    /// 既定ウィンドウサイズの設定（毎回固定サイズで起動するか）。
    pub window: WindowSettings,
    /// 起動時に解決した実テーマ。ファイルには保存しない（`resolve_theme` で設定）。
    #[serde(skip)]
    pub resolved: ResolvedTheme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            font: FontSpec::default(),
            layout: Layout::default(),
            colors: ThemeColors::default(),
            columns: default_columns(),
            size_format: SizeFormat::Simple2,
            auto_adjust_columns: true,
            default_sort: SortType::FileName,
            reverse_sort_date: false,
            progress_delay_ms: 1000,
            keybinds: KeyMap::default().to_string_map(),
            keybinds_textviewer: KeyMap::default_textviewer().to_string_map(),
            keybinds_imageviewer: KeyMap::default_imageviewer().to_string_map(),
            bookmarks: Vec::new(),
            editor: "notepad.exe".to_owned(),
            cursor: CursorSettings::default(),
            image: ImageSettings::default(),
            icons: IconSettings::default(),
            file_ops: FileOpSettings::default(),
            window: WindowSettings::default(),
            resolved: ResolvedTheme::default(),
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
        Self::load_from_reporting(path).0
    }

    /// 埋め込みデフォルトにユーザ `config.toml` をマージして読み込み、合わせて読込エラーの
    /// 説明文（あれば）を返す。ファイルが無いのは正常（`None`）。TOML の文法エラーや型不一致で
    /// ユーザ設定を反映できなかった場合のみ `Some(詳細)`＝呼び側が警告表示／ログに使う。
    /// **エラー時は既定（または反映できた範囲）で起動する**（黙って全無視はしない）。
    pub fn load_reporting() -> (Self, Option<String>) {
        Self::load_from_reporting(&config_path())
    }

    /// [`Config::load_reporting`] の指定パス版。
    pub fn load_from_reporting(path: &Path) -> (Self, Option<String>) {
        let mut base: Value =
            toml::from_str(DEFAULT_CONFIG_TOML).expect("埋め込み default.toml が不正");
        let mut error = None;
        // ファイル無し＝既定で起動（正常）。読めたが TOML が壊れていれば詳細を控える。
        if let Ok(s) = std::fs::read_to_string(path) {
            match toml::from_str::<Value>(&s) {
                Ok(user) => deep_merge(&mut base, &user),
                Err(e) => error = Some(e.to_string()),
            }
        }
        match base.try_into() {
            Ok(cfg) => (cfg, error),
            // マージ後の型不一致（例：未知の列挙値）も既定へ。先のエラーを優先。
            Err(e) => (Self::default(), error.or_else(|| Some(e.to_string()))),
        }
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

    /// 登録ディレクトリを追加する。同じ `path` が既にあれば `label` を更新する
    /// （割り当て済みのショートカットは保持する）。
    pub fn add_bookmark(&mut self, label: &str, path: &str) {
        if let Some(b) = self.bookmarks.iter_mut().find(|b| b.path == path) {
            b.label = label.to_owned();
        } else {
            self.bookmarks.push(Bookmark {
                label: label.to_owned(),
                path: path.to_owned(),
                shortcut: String::new(),
            });
        }
    }

    /// 設定からキーマップを組む。
    pub fn keymap(&self) -> KeyMap {
        KeyMap::from_string_map(&self.keybinds)
    }

    /// 設定からテキストビューア用キーマップを組む。
    pub fn keymap_textviewer(&self) -> KeyMap {
        KeyMap::from_string_map(&self.keybinds_textviewer)
    }

    /// 設定から画像・動画ビューア用キーマップを組む。
    pub fn keymap_imageviewer(&self) -> KeyMap {
        KeyMap::from_string_map(&self.keybinds_imageviewer)
    }

    /// OS のライト判定（`system_is_light`）を渡して実テーマを解決する。
    /// `theme=System` のときだけ判定を参照し、`Dark`/`Light` 固定なら無視する。
    pub fn resolve_theme(&mut self, system_is_light: bool) {
        self.resolved = match self.theme {
            Theme::Dark => ResolvedTheme::Dark,
            Theme::Light => ResolvedTheme::Light,
            Theme::System => {
                if system_is_light {
                    ResolvedTheme::Light
                } else {
                    ResolvedTheme::Dark
                }
            }
        };
    }

    /// 解決済みテーマに対応する実効配色。
    pub fn active_colors(&self) -> Colors {
        match self.resolved {
            ResolvedTheme::Light => self.colors.light,
            ResolvedTheme::Dark => self.colors.dark,
        }
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

/// 1タブ分の状態（左右ペインのパスとアクティブ側・各ペインのソート種別/昇降）。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TabState {
    pub left: String,
    pub right: String,
    #[serde(default)]
    pub active_right: bool,
    #[serde(default)]
    pub sort_left: SortType,
    #[serde(default)]
    pub sort_left_reverse: bool,
    #[serde(default)]
    pub sort_right: SortType,
    #[serde(default)]
    pub sort_right_reverse: bool,
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

/// 入力ダイアログの履歴上限（用途キーごと）。原作 `Other/InputHistoryCount` 相当。
const HISTORY_CAP: usize = 30;

/// パス移動履歴（訪問ログ）を `InputHistory` に同居させるときの用途キー。
pub const PATH_HISTORY_KEY: &str = "pathhistory";
/// パス移動履歴の保持上限（原作 `PathHistoryCount` は 100 だが、Rerics は back/forward と
/// 揃えて多めに持つ）。超えたら古い方から落とす。
pub const PATH_HISTORY_CAP: usize = 256;

/// 入力ダイアログの履歴ストア（用途キー別）。`history.toml` に永続。
/// 各キーの `Vec` は**古い順**で持ち、`get` は新しい順に直して返す。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InputHistory {
    #[serde(default)]
    map: std::collections::HashMap<String, Vec<String>>,
}

impl InputHistory {
    /// 履歴ファイルから読み込む（無ければ空）。
    pub fn load() -> Self {
        load_toml(&history_path())
    }

    /// 履歴ファイルへ保存する。
    pub fn save(&self) -> std::io::Result<()> {
        save_toml(&history_path(), self)
    }

    /// 指定キーの履歴を**新しい順**で返す（コンボの候補表示用）。
    pub fn get(&self, key: &str) -> Vec<String> {
        self.map
            .get(key)
            .map(|v| v.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// 値を履歴へ追加する（入力ダイアログ用・上限 [`HISTORY_CAP`]）。
    pub fn add(&mut self, key: &str, value: &str) {
        self.add_capped(key, value, HISTORY_CAP);
    }

    /// 上限を指定して値を履歴へ追加する。空（trim 後）は無視。既存の同値は末尾へ移動
    /// （重複排除＝最新に集約）。上限を超えたら古いものから落とす。パス移動履歴は入力
    /// 履歴より多く持つため、こちらを使って大きな上限を渡す。
    pub fn add_capped(&mut self, key: &str, value: &str, cap: usize) {
        let v = value.trim();
        if v.is_empty() {
            return;
        }
        let list = self.map.entry(key.to_owned()).or_default();
        if let Some(pos) = list.iter().position(|x| x == v) {
            list.remove(pos);
        }
        list.push(v.to_owned());
        if list.len() > cap {
            let n = list.len() - cap;
            list.drain(0..n);
        }
    }
}

/// リサイズ可能ダイアログの前回サイズ（用途キー別・クライアントの論理px幅高）を覚えるストア。
/// `dialog-sizes.toml` に永続。設定 UI は持たず無言で記憶する。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DialogSizes {
    #[serde(default)]
    sizes: std::collections::BTreeMap<String, (i32, i32)>,
}

impl DialogSizes {
    /// サイズ記憶ファイルから読み込む（無ければ空）。
    pub fn load() -> Self {
        load_toml(&dialog_sizes_path())
    }

    /// サイズ記憶ファイルへ保存する。
    pub fn save(&self) -> std::io::Result<()> {
        save_toml(&dialog_sizes_path(), self)
    }

    /// キーの前回サイズ（論理px幅高）を返す。
    pub fn get(&self, key: &str) -> Option<(i32, i32)> {
        self.sizes.get(key).copied()
    }

    /// キーのサイズを記録する。
    pub fn set(&mut self, key: &str, size: (i32, i32)) {
        self.sizes.insert(key.to_owned(), size);
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
            "[colors.dark]\ncursor = \"#ff0000\"\n\n[keybinds]\n\"Ctrl+T\" = \"Reload\"\n",
        )
        .unwrap();
        let cfg = Config::load_from(&path);
        // 上書きしたキーだけ変わり、他はデフォルトのまま。
        assert_eq!(cfg.colors.dark.cursor, crate::Rgb::new(0xff, 0, 0));
        assert_eq!(cfg.colors.dark.background, Config::default().colors.dark.background);
        assert_eq!(cfg.colors.light, Config::default().colors.light);
        assert_eq!(cfg.keybinds.get("Ctrl+T").map(String::as_str), Some("Reload"));
        assert_eq!(cfg.keybinds.get("Down").map(String::as_str), Some("CursorDown"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn user_empty_value_unbinds_default_key() {
        // ユーザ config で `"F4" = ""` と書くと、既定の F4 割当を打ち消せる（unbind）。
        let path = std::env::temp_dir().join("rerics_cfg_unbind.toml");
        std::fs::write(&path, "[keybinds]\n\"F4\" = \"\"\n").unwrap();
        let cfg = Config::load_from(&path);
        // マージ後の keybinds は空文字で上書きされ、
        assert_eq!(cfg.keybinds.get("F4").map(String::as_str), Some(""));
        // 実キーマップでは未バインドになる。
        let km = cfg.keymap();
        assert_eq!(km.resolve(&crate::KeyChord::key(crate::vk::F4)), None);
        // 他キーは既定のまま。
        assert_eq!(km.resolve(&crate::KeyChord::key(crate::vk::DOWN)), Some(crate::Command::CursorDown));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_writes_only_diff() {
        let path = std::env::temp_dir().join("rerics_cfg_savediff.toml");
        let _ = std::fs::remove_file(&path);
        let mut cfg = Config::default();
        cfg.colors.dark.cursor = crate::Rgb::new(0x12, 0x34, 0x56);
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
    fn add_bookmark_dedupes_by_path() {
        let mut cfg = Config::default();
        cfg.add_bookmark("home", "C:\\Users\\me");
        cfg.add_bookmark("docs", "C:\\Users\\me\\Documents");
        assert_eq!(cfg.bookmarks.len(), 2);
        // 同じ path への再追加は重複させず label を更新する。
        cfg.add_bookmark("ホーム", "C:\\Users\\me");
        assert_eq!(cfg.bookmarks.len(), 2);
        assert_eq!(cfg.bookmarks[0].label, "ホーム");
    }

    #[test]
    fn bookmarks_roundtrip_through_save() {
        let path = std::env::temp_dir().join("rerics_cfg_bookmarks.toml");
        let _ = std::fs::remove_file(&path);
        let mut cfg = Config::default();
        cfg.add_bookmark("proj", "D:\\work\\proj");
        cfg.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[bookmarks]]"), "bookmarks should be saved: {text}");
        assert!(text.contains("proj"));
        let back = Config::load_from(&path);
        assert_eq!(back.bookmarks, cfg.bookmarks);
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
    fn load_reporting_flags_broken_toml() {
        let path = std::env::temp_dir().join("rerics_test_broken_config.toml");
        // 文法エラー（未閉じ・無効エスケープ）の config.toml。
        std::fs::write(&path, "editor = \"C:\\Users\"\n").unwrap();
        let (cfg, err) = Config::load_from_reporting(&path);
        assert!(err.is_some(), "壊れた TOML はエラーを報告する");
        // 既定で起動する（黙って一部反映ではなく既定に戻る）。
        assert_eq!(cfg.editor, Config::default().editor);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_reporting_ok_for_valid_and_missing() {
        // 妥当な config はエラーなしで反映。
        let path = std::env::temp_dir().join("rerics_test_valid_config.toml");
        std::fs::write(&path, "editor = 'code.exe'\n").unwrap();
        let (cfg, err) = Config::load_from_reporting(&path);
        assert!(err.is_none());
        assert_eq!(cfg.editor, "code.exe");
        let _ = std::fs::remove_file(&path);
        // ファイル無しは正常（既定・エラーなし）。
        let missing = std::env::temp_dir().join("rerics_test_no_such_config.toml");
        let _ = std::fs::remove_file(&missing);
        let (cfg2, err2) = Config::load_from_reporting(&missing);
        assert!(err2.is_none());
        assert_eq!(cfg2.editor, Config::default().editor);
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
                TabState { left: "C:\\a".into(), right: "C:\\b".into(), active_right: false, ..Default::default() },
                TabState { left: "C:\\c".into(), right: "C:\\d".into(), active_right: true, ..Default::default() },
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
    fn tabstate_sort_roundtrip() {
        let st = State {
            window: None,
            tabs: vec![TabState {
                left: "C:\\a".into(),
                right: "C:\\b".into(),
                active_right: true,
                sort_left: SortType::Length,
                sort_left_reverse: true,
                sort_right: SortType::LastWriteTime,
                sort_right_reverse: false,
            }],
            active_tab: 0,
            ..State::default()
        };
        let s = toml::to_string(&st).unwrap();
        let back: State = toml::from_str(&s).unwrap();
        let t = &back.tabs[0];
        assert_eq!(t.sort_left, SortType::Length);
        assert!(t.sort_left_reverse);
        assert_eq!(t.sort_right, SortType::LastWriteTime);
        assert!(!t.sort_right_reverse);
    }

    #[test]
    fn tabstate_without_sort_fields_defaults() {
        // 旧 state.toml（ソートフィールド無し）を読んでも既定（FileName・昇順）に落ちる。
        let toml = "left = \"C:\\\\a\"\nright = \"C:\\\\b\"\nactive_right = false\n";
        let t: TabState = toml::from_str(toml).unwrap();
        assert_eq!(t.sort_left, SortType::FileName);
        assert!(!t.sort_left_reverse);
        assert_eq!(t.sort_right, SortType::FileName);
        assert!(!t.sort_right_reverse);
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
    fn input_history_add_dedup_order_cap() {
        let mut h = InputHistory::default();
        h.add("path", "C:\\a");
        h.add("path", "C:\\b");
        h.add("path", "C:\\a"); // 既存は末尾へ移動＝最新
        // 別キーは独立。
        h.add("mask", "*.txt");
        // 空・空白のみは無視。
        h.add("path", "   ");
        // get は新しい順。
        assert_eq!(h.get("path"), vec!["C:\\a", "C:\\b"]);
        assert_eq!(h.get("mask"), vec!["*.txt"]);
        assert!(h.get("none").is_empty());

        // 上限を超えたら古いものから落ちる（新しい順で先頭が最新）。
        let mut h2 = InputHistory::default();
        for i in 0..(HISTORY_CAP + 5) {
            h2.add("k", &format!("v{i}"));
        }
        let got = h2.get("k");
        assert_eq!(got.len(), HISTORY_CAP);
        assert_eq!(got[0], format!("v{}", HISTORY_CAP + 4));
        assert_eq!(got.last().unwrap(), &format!("v{}", 5));
    }

    #[test]
    fn add_capped_uses_custom_cap_for_path_history() {
        let mut h = InputHistory::default();
        for i in 0..(PATH_HISTORY_CAP + 5) {
            h.add_capped(PATH_HISTORY_KEY, &format!("C:\\d{i}"), PATH_HISTORY_CAP);
        }
        let got = h.get(PATH_HISTORY_KEY);
        // 入力履歴(30)ではなくパス履歴の上限(256)で頭打ち。
        assert_eq!(got.len(), PATH_HISTORY_CAP);
        assert_eq!(got[0], format!("C:\\d{}", PATH_HISTORY_CAP + 4)); // 新しい順の先頭=最後に追加
        // 既存同値の再訪は最新へ集約（重複しない）。
        h.add_capped(PATH_HISTORY_KEY, "C:\\d10", PATH_HISTORY_CAP);
        let got2 = h.get(PATH_HISTORY_KEY);
        assert_eq!(got2[0], "C:\\d10");
        assert_eq!(got2.iter().filter(|x| x.as_str() == "C:\\d10").count(), 1);
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
