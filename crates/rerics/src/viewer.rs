//! テキスト/バイナリビューアの表示パネル（自前描画）。
//!
//! 別窓は作らず、メイン領域（ペイン＋ログ）に重ねて表示する 1 枚の `WindowControl`。
//! 表示モデルは `rerics_core::ViewerModel`（折返し・ダンプ整形・エンコーディングは core 側）。
//! 本モジュールは描画・スクロール・キー操作の GUI 配線に徹する。下端に状態行（ファイル名・
//! エンコーディング・モード・行位置）を自前描画する。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{
    Colors, Config, DisplayLine, LineEnding, Matcher, Rgb, SearchOptions, ViewMode, ViewerModel,
    build_matcher,
};
use unicode_width::UnicodeWidthChar;
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;

/// 表示行内の位置（表示行 index, 行内の char 数オフセット）。
type Pos = (usize, usize);

/// テキストの折返し時のタブ幅。
const TAB_WIDTH: usize = 4;
/// 読み込むファイルの上限（これを超えたら先頭だけ）。
pub const MAX_VIEW_BYTES: usize = 4 * 1024 * 1024;
/// 検索履歴の保持上限（決め打ち）。
const HISTORY_CAP: usize = 32;
/// 履歴ドロップダウンに一度に見せる最大行数（超過分はスクロール）。
const HISTORY_DROPDOWN_ROWS: usize = 12;

/// 全一致リストのキャッシュ（検索語・オプション・行世代・全一致の (表示行, 開始桁)）。
type MatchCache = (String, SearchOptions, u64, Rc<Vec<(usize, usize)>>);
/// キーチョードがユーザーのビューアキーバインドに割当済みなら実行して true を返すコールバック。
type ChordHandler = Box<dyn Fn(rerics_core::KeyChord) -> bool>;
/// 右クリック時に画面座標を渡すコールバック（メニュー表示は MainWindow が担う）。
type MenuHandler = Box<dyn Fn(w::POINT)>;

struct Inner {
    model: RefCell<ViewerModel>,
    title: RefCell<String>,
    truncated: Cell<bool>,
    colors: Colors,
    font_family: String,
    font_size: i32,
    line_height: Cell<i32>,
    char_width: Cell<i32>,
    scroll_top: Cell<usize>,
    /// 折返し済み表示行のキャッシュ。
    lines: RefCell<Vec<DisplayLine>>,
    /// キャッシュ生成時の折返し桁。
    cached_wrap: Cell<usize>,
    /// gutter（行番号/オフセット）の最大桁数。
    gutter_chars: Cell<usize>,
    /// 再生成が必要か（open/エンコーディング/モード変更で立てる）。
    dirty: Cell<bool>,
    /// 検索語（小文字化はしない。一致は `search_opts` に従って判定）。
    search_term: RefCell<String>,
    /// 検索オプション（大小区別・単語一致・正規表現）。既定＝大小無視・部分一致。
    search_opts: Cell<SearchOptions>,
    /// コンパイル済みマッチャのキャッシュ（検索語＋オプションが変わるまで使い回す）。
    /// 正規表現を行ごとに再コンパイルしないための要。
    matcher_cache: RefCell<Option<(String, SearchOptions, Matcher)>>,
    /// 表示行の世代（再生成のたびに +1）。一致リストのキャッシュ無効化に使う。
    lines_gen: Cell<u64>,
    /// 全一致リストのキャッシュ（検索語・オプション・行世代が変わるまで使い回す）。
    /// 内容は起動中に変わらないので、移動のたびに全行を走査し直さないための要。
    match_cache: RefCell<Option<MatchCache>>,
    /// 現在ヒットしている表示行（ハイライト対象）。
    /// 現在の検索一致（表示行 index, 行内の開始桁）。検索ハイライト/ナビの基準。
    match_pos: Cell<Option<(usize, usize)>>,
    /// インライン検索バーが開いているか（開いている間は本文を下にずらす）。
    search_active: Cell<bool>,
    /// 検索バーを開いた時点のスクロール位置（Esc で戻す先）。
    saved_scroll: Cell<usize>,
    /// 検索バーの入力欄（本物の Edit 子コントロール）。既定は非表示で生成し、開閉で出し入れする。
    search_edit: gui::Edit,
    /// 検索バー右側の操作（マウス用）。大小無視・単語一致・正規表現のトグルと前/次ボタン。
    /// いずれも非表示で生成し、バー開閉で出し入れする。
    search_case: gui::CheckBox,
    search_word: gui::CheckBox,
    search_regex: gui::CheckBox,
    search_prev: gui::Button,
    search_next: gui::Button,
    /// 入力欄右端の履歴ドロップダウン（▼）ボタン。クリック/Alt+↑↓ で履歴リストを開く。
    search_history: gui::Button,
    /// 履歴ドロップダウンの本体（ボックス幅に合わせた ListBox）。非表示で生成し、開閉する。
    search_list: gui::ListBox,
    /// 履歴ドロップダウンが開いているか。
    list_open: Cell<bool>,
    /// 検索バーを閉じたときに呼ぶコールバック（キー入力を本体へ戻す）。MainWindow が登録する。
    on_search_close: RefCell<Option<Box<dyn Fn()>>>,
    /// キーチョードがユーザーのビューアキーバインドに割り当て済みなら実行して true を返す
    /// コールバック。検索バー内のニーモニック（Alt+C 等）と被ったらユーザー側を優先するのに使う。
    on_chord: RefCell<Option<ChordHandler>>,
    /// マウス選択の始点・終点（None なら選択なし）。
    sel_anchor: Cell<Option<Pos>>,
    sel_cursor: Cell<Option<Pos>>,
    /// ドラッグ中か。
    selecting: Cell<bool>,
    /// 右クリック時に呼ぶコールバック（画面座標）。メニュー表示は MainWindow が担う。
    on_menu: RefCell<Option<MenuHandler>>,
}

/// ビューア表示パネル。
#[derive(Clone)]
pub struct ViewerView {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl ViewerView {
    pub fn new(
        parent: &(impl GuiParent + 'static),
        position: (i32, i32),
        size: (i32, i32),
        cfg: &Config,
    ) -> Self {
        let wnd = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                class_bg_brush: gui::Brush::None,
                position,
                size,
                // 検索バーの子コントロール領域を親の描画から除外し、ミラーが本物コントロールを
                // 上書きしてチラつかないようにする（自前描画の render_to はクリップ無視で全描画）。
                style: co::WS::CHILD | co::WS::CLIPSIBLINGS | co::WS::CLIPCHILDREN,
                ..Default::default()
            },
        );
        // 検索バーの入力欄。非表示（VISIBLE を外す）で生成し、検索時のみ前面に出す。
        // イベント（en_change・サブクラス）は親窓の生成より前に登録する必要があるため、ここで作る。
        let search_edit = gui::Edit::new(
            &wnd,
            gui::EditOpts {
                control_style: co::ES::AUTOHSCROLL,
                window_style: co::WS::CHILD | co::WS::GROUP | co::WS::TABSTOP | co::WS::BORDER,
                position: gui::dpi(4, 4),
                width: gui::dpi_x(200),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        // 右側の操作（トグル3つ＋前/次ボタン）。非表示で生成。
        let hidden_cb = co::WS::CHILD | co::WS::GROUP | co::WS::TABSTOP;
        let hidden_btn = co::WS::CHILD | co::WS::TABSTOP;
        let mk_check = |text: &str| {
            gui::CheckBox::new(
                &wnd,
                gui::CheckBoxOpts {
                    text,
                    window_style: hidden_cb,
                    position: gui::dpi(0, 0),
                    size: gui::dpi(58, 22),
                    ..Default::default()
                },
            )
        };
        let search_case = mk_check("ケースを無視(&C)");
        let search_word = mk_check("単語境界(&W)");
        let search_regex = mk_check("正規表現(&R)");
        let mk_btn = |text: &str| {
            gui::Button::new(
                &wnd,
                gui::ButtonOpts {
                    text,
                    control_style: co::BS::PUSHBUTTON,
                    window_style: hidden_btn,
                    position: gui::dpi(0, 0),
                    width: gui::dpi_x(26),
                    height: gui::dpi_y(22),
                    ..Default::default()
                },
            )
        };
        let search_prev = mk_btn("↑");
        let search_next = mk_btn("↓");
        let search_history = mk_btn("▼");
        // 履歴リスト（非表示で生成・開いたとき入力欄の下へ出す）。
        let search_list = gui::ListBox::new(
            &wnd,
            gui::ListBoxOpts {
                control_style: co::LBS::NOTIFY | co::LBS::HASSTRINGS,
                window_style: co::WS::CHILD | co::WS::BORDER | co::WS::VSCROLL,
                position: gui::dpi(0, 0),
                size: gui::dpi(160, 80),
                ..Default::default()
            },
        );
        let inner = Rc::new(Inner {
            model: RefCell::new(ViewerModel::new(Vec::new())),
            title: RefCell::new(String::new()),
            truncated: Cell::new(false),
            colors: cfg.active_colors(),
            font_family: cfg.font.family.clone(),
            font_size: cfg.font.size,
            line_height: Cell::new(gui::dpi_y(cfg.font.size + 2)),
            char_width: Cell::new(gui::dpi_x(cfg.font.size).max(1)),
            scroll_top: Cell::new(0),
            lines: RefCell::new(Vec::new()),
            cached_wrap: Cell::new(0),
            gutter_chars: Cell::new(1),
            dirty: Cell::new(true),
            search_term: RefCell::new(String::new()),
            search_opts: Cell::new(SearchOptions::default()),
            matcher_cache: RefCell::new(None),
            lines_gen: Cell::new(0),
            match_cache: RefCell::new(None),
            match_pos: Cell::new(None),
            search_active: Cell::new(false),
            saved_scroll: Cell::new(0),
            search_edit,
            search_case,
            search_word,
            search_regex,
            search_prev,
            search_next,
            search_history,
            search_list,
            list_open: Cell::new(false),
            on_search_close: RefCell::new(None),
            on_chord: RefCell::new(None),
            sel_anchor: Cell::new(None),
            sel_cursor: Cell::new(None),
            selecting: Cell::new(false),
            on_menu: RefCell::new(None),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// 右クリック時のコールバック（コンテキストメニュー表示）を登録する。
    pub fn on_menu(&self, cb: impl Fn(w::POINT) + 'static) {
        *self.inner.on_menu.borrow_mut() = Some(Box::new(cb));
    }

    /// ファイル内容を読み込んで表示状態にする（エンコーディングは既定、モードは内容から自動判定）。
    pub fn open(&self, filename: &str, mut bytes: Vec<u8>, truncated: bool) {
        bytes.truncate(MAX_VIEW_BYTES);
        // バイナリらしければバイナリモードで開始する（原作准拠の自動判定）。
        let mut model = ViewerModel::open(bytes);
        // 構文ハイライト：拡張子で言語を、背景の明暗でテーマ（dark/light）を選ぶ。
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        let bg = self.inner.colors.viewer_background;
        let dark = (bg.r as u32 + bg.g as u32 + bg.b as u32) < 384;
        model.set_highlight(ext, dark);
        *self.inner.model.borrow_mut() = model;
        *self.inner.title.borrow_mut() = filename.to_owned();
        self.inner.truncated.set(truncated);
        self.inner.scroll_top.set(0);
        self.inner.dirty.set(true);
        *self.inner.search_term.borrow_mut() = String::new();
        self.inner.match_pos.set(None);
        self.reset_search_bar();
        self.clear_selection();
        let _ = self.refresh();
    }

    /// 検索バーを登録解除なしで畳む（ファイルを開き直す/ビューアを閉じる際の後始末）。
    fn reset_search_bar(&self) {
        self.inner.search_active.set(false);
        self.close_history_dropdown(false);
        for h in self.search_bar_controls() {
            h.ShowWindow(co::SW::HIDE);
        }
    }

    /// 検索バーの子コントロール一式（入力欄＋履歴▼＋トグル＋前後ボタン）。
    fn search_bar_controls(&self) -> [&w::HWND; 7] {
        [
            self.inner.search_edit.hwnd(),
            self.inner.search_history.hwnd(),
            self.inner.search_case.hwnd(),
            self.inner.search_word.hwnd(),
            self.inner.search_regex.hwnd(),
            self.inner.search_prev.hwnd(),
            self.inner.search_next.hwnd(),
        ]
    }

    /// MainWindow がキー入力先を本体へ戻すためのコールバックを登録する。
    pub fn on_search_close(&self, cb: impl Fn() + 'static) {
        *self.inner.on_search_close.borrow_mut() = Some(Box::new(cb));
    }

    /// ユーザーキーバインド照会コールバックを登録する（割り当て済みなら実行して true）。
    pub fn on_chord(&self, cb: impl Fn(rerics_core::KeyChord) -> bool + 'static) {
        *self.inner.on_chord.borrow_mut() = Some(Box::new(cb));
    }

    /// チェック状態（`checked`）を反映して再検索する。「ケースを無視」ON＝大小無視。単語/正規は
    /// 排他（片方 ON で他方 OFF）。クリックとニーモニックの共通経路。
    fn apply_option(&self, kind: OptKind, checked: bool) -> w::AnyResult<()> {
        let mut o = self.inner.search_opts.get();
        match kind {
            OptKind::Case => o.case_sensitive = !checked,
            OptKind::Word => {
                o.whole_word = checked;
                if checked {
                    o.regex = false;
                }
            }
            OptKind::Regex => {
                o.regex = checked;
                if checked {
                    o.whole_word = false;
                }
            }
        }
        self.inner.search_opts.set(o);
        self.inner.search_case.set_check(!o.case_sensitive);
        self.inner.search_word.set_check(o.whole_word);
        self.inner.search_regex.set_check(o.regex);
        self.refocus_after_toggle()
    }

    /// トグルを反転して再検索する（ニーモニック Alt+C/W/R 用）。
    fn toggle_option(&self, kind: OptKind) -> w::AnyResult<()> {
        let o = self.inner.search_opts.get();
        let cur = match kind {
            OptKind::Case => !o.case_sensitive,
            OptKind::Word => o.whole_word,
            OptKind::Regex => o.regex,
        };
        self.apply_option(kind, !cur)
    }

    fn clear_selection(&self) {
        self.inner.sel_anchor.set(None);
        self.inner.sel_cursor.set(None);
        self.inner.selecting.set(false);
    }

    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// エンコーディングを循環切替する。
    pub fn cycle_encoding(&self, forward: bool) -> w::AnyResult<()> {
        self.inner.model.borrow_mut().cycle_encoding(forward);
        self.inner.dirty.set(true);
        self.inner.match_pos.set(None);
        self.clear_selection();
        self.refresh()
    }

    /// テキスト/バイナリを切替する（スクロール位置は先頭へ）。
    pub fn toggle_mode(&self) -> w::AnyResult<()> {
        self.inner.model.borrow_mut().toggle_mode();
        self.inner.scroll_top.set(0);
        self.inner.dirty.set(true);
        self.inner.match_pos.set(None);
        self.clear_selection();
        self.refresh()
    }

    /// debug-server 観測用：検索語・現在一致 `(行, 桁, 長さ)`・可視全体の一致総数・検索オプション。
    #[cfg(feature = "debug-server")]
    pub fn debug_search_state(&self) -> (String, Option<(usize, usize, usize)>, usize, SearchOptions) {
        let term = self.inner.search_term.borrow().clone();
        let pos = self.inner.match_pos.get().map(|(l, c)| (l, c, self.current_match_len()));
        let count = self.all_matches().len();
        (term, pos, count, self.inner.search_opts.get())
    }

    /// 検索語を設定し、現在位置以降の最初の一致へジャンプする。空なら検索解除。
    pub fn set_search(&self, term: &str) -> w::AnyResult<()> {
        *self.inner.search_term.borrow_mut() = term.to_owned();
        if term.is_empty() {
            self.inner.match_pos.set(None);
            return self.refresh();
        }
        let matches = self.all_matches();
        if matches.is_empty() {
            self.inner.match_pos.set(None);
            return self.refresh();
        }
        // 現在の一致（あれば）の行、無ければ表示先頭以降の最初の一致へ。無ければ巻き戻る。
        let from = self.inner.match_pos.get().map(|(l, _)| l).unwrap_or_else(|| self.inner.scroll_top.get());
        let hit = matches.iter().copied().find(|&(l, _)| l >= from).unwrap_or(matches[0]);
        self.jump_to(hit.0, hit.1);
        self.refresh()
    }

    /// 次（`forward=false` なら前）の一致箇所へ移動する（同一行内の複数一致も順に辿る）。
    pub fn find_next(&self, forward: bool) -> w::AnyResult<()> {
        if self.inner.search_term.borrow().is_empty() {
            return Ok(());
        }
        let matches = self.all_matches();
        if matches.is_empty() {
            return Ok(());
        }
        let n = matches.len();
        let next = match self.inner.match_pos.get().and_then(|c| matches.iter().position(|&m| m == c)) {
            Some(i) => {
                if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                }
            }
            // 現在一致が一覧に無い（再ラップ等）。位置に近い一致から始める。
            None => {
                let from = self
                    .inner
                    .match_pos
                    .get()
                    .map(|(l, _)| l)
                    .unwrap_or_else(|| self.inner.scroll_top.get());
                if forward {
                    matches.iter().position(|&(l, _)| l >= from).unwrap_or(0)
                } else {
                    matches.iter().rposition(|&(l, _)| l <= from).unwrap_or(n - 1)
                }
            }
        };
        let hit = matches[next];
        self.jump_to(hit.0, hit.1);
        self.refresh()
    }

    /// 検索バーが開いているか（debug-server 観測用）。
    #[cfg(feature = "debug-server")]
    pub fn is_search_bar_open(&self) -> bool {
        self.inner.search_active.get()
    }


    /// インライン検索バーを開く。開始時のスクロール位置を控え（Esc 復帰用）、入力欄を前面に
    /// 出してフォーカスし、現在の検索語で一度検索する。既に開いていれば入力欄へ再フォーカスのみ。
    pub fn open_search_bar(&self) -> w::AnyResult<()> {
        let edit = self.inner.search_edit.hwnd();
        if self.inner.search_active.get() {
            edit.SetFocus();
            return Ok(());
        }
        self.inner.saved_scroll.set(self.inner.scroll_top.get());
        self.inner.search_active.set(true);
        let term = self.inner.search_term.borrow().clone();
        let _ = edit.SetWindowText(&term);
        for h in self.search_bar_controls() {
            h.ShowWindow(co::SW::SHOW);
        }
        // チェック状態を現在のオプションへ同期（「大小」ON＝大小無視＝case_sensitive=false）。
        let o = self.inner.search_opts.get();
        self.inner.search_case.set_check(!o.case_sensitive);
        self.inner.search_word.set_check(o.whole_word);
        self.inner.search_regex.set_check(o.regex);
        self.layout_search_bar();
        edit.SetFocus();
        // 前回の検索語を全選択して開く（中身があればそのまま打ち直しで置換できる）。
        self.inner.search_edit.set_selection(0, -1);
        self.apply_search_from_edit()?;
        self.refresh()
    }

    /// 入力欄の現在値で検索する（en_change・バー操作の共通経路）。
    fn apply_search_from_edit(&self) -> w::AnyResult<()> {
        let text = self.inner.search_edit.hwnd().GetWindowText().unwrap_or_default();
        self.set_search(&text)
    }

    /// 検索バーを閉じて現在位置を確定する（Enter）。検索語・一致はそのまま残し F3 等で続けられる。
    /// 確定したときだけ検索語を履歴へ記録する（重複は最新へ集約・上限32・`history.toml` に永続）。
    pub fn confirm_search_bar(&self) -> w::AnyResult<()> {
        if !self.inner.search_active.get() {
            return Ok(());
        }
        self.record_history();
        self.reset_search_bar();
        self.refresh()?;
        if let Some(cb) = self.inner.on_search_close.borrow().as_ref() {
            cb();
        }
        Ok(())
    }

    /// 現在の検索語を検索履歴（キー `"search"`）へ記録する。空は無視。
    fn record_history(&self) {
        let term = self.inner.search_term.borrow().clone();
        if term.trim().is_empty() {
            return;
        }
        let mut hist = rerics_core::InputHistory::load();
        hist.add_capped("search", &term, HISTORY_CAP);
        let _ = hist.save();
    }

    /// 検索履歴のドロップダウン（入力欄の下に、ボックス幅に合わせた ListBox）を開く。開いている
    /// ときに再度呼ぶと閉じる（トグル）。リストは ↑↓ で項目選択・Enter/クリックで確定・Esc で取消。
    fn open_history_dropdown(&self) -> w::AnyResult<()> {
        if self.inner.list_open.get() {
            self.close_history_dropdown(true);
            return Ok(());
        }
        let items = rerics_core::InputHistory::load().get("search");
        if items.is_empty() {
            self.inner.search_edit.hwnd().SetFocus();
            return Ok(());
        }
        let list = self.inner.search_list.hwnd();
        self.inner.search_list.items().delete_all();
        self.inner.search_list.items().add(&items)?;
        // 入力欄の左下に、ボックス（入力欄＋▼）の幅で出す。高さは項目数ぶん（上限あり）。
        let cw = self.hwnd().GetClientRect().map(|r| r.right - r.left).unwrap_or(0);
        let g = self.search_bar_geom(cw);
        let (ex, ew) = g.edit;
        let width = ew + g.history.1;
        let item_h = unsafe { list.SendMessage(w::msg::lb::GetItemHeight { index: None }) }
            .map(|h| h as i32)
            .unwrap_or(gui::dpi_y(18))
            .max(1);
        let rows = items.len().min(HISTORY_DROPDOWN_ROWS) as i32;
        let height = rows * item_h + gui::dpi_y(4);
        let top = self.search_bar_height();
        let _ = list.MoveWindow(w::POINT { x: ex, y: top }, w::SIZE { cx: width, cy: height }, true);
        unsafe {
            let _ = list.SendMessage(w::msg::lb::SetCurSel { index: Some(0) });
        }
        list.ShowWindow(co::SW::SHOW);
        list.BringWindowToTop()?;
        self.inner.list_open.set(true);
        list.SetFocus();
        Ok(())
    }

    /// 履歴ドロップダウンを閉じる。`refocus` で入力欄へフォーカスを戻す。
    fn close_history_dropdown(&self, refocus: bool) {
        if !self.inner.list_open.get() {
            return;
        }
        self.inner.list_open.set(false);
        self.inner.search_list.hwnd().ShowWindow(co::SW::HIDE);
        if refocus {
            self.inner.search_edit.hwnd().SetFocus();
        }
    }

    /// ドロップダウンの現在選択項目を確定する（入力欄へ入れて検索）。
    fn pick_history_selection(&self) -> w::AnyResult<()> {
        let list = self.inner.search_list.hwnd();
        let sel = unsafe { list.SendMessage(w::msg::lb::GetCurSel {}) };
        let text = sel.and_then(|i| self.inner.search_list.items().text(i).ok());
        self.close_history_dropdown(true);
        if let Some(text) = text {
            self.set_query(&text)?;
        }
        Ok(())
    }

    /// 入力欄に文字列を入れ（末尾キャレット）、その内容で検索する。履歴選択の共通経路。
    fn set_query(&self, text: &str) -> w::AnyResult<()> {
        let edit = self.inner.search_edit.hwnd();
        let _ = edit.SetWindowText(text);
        let caret = text.encode_utf16().count() as i32;
        self.inner.search_edit.set_selection(caret, caret);
        self.apply_search_from_edit()
    }

    /// 検索バーを閉じて開始位置へ戻す（Esc）。検索語とハイライト（黄）は残すが、現在一致
    /// （青）は手放す。次に開くと戻った表示位置から検索し直す。
    pub fn cancel_search_bar(&self) -> w::AnyResult<()> {
        if !self.inner.search_active.get() {
            return Ok(());
        }
        self.inner.scroll_top.set(self.inner.saved_scroll.get());
        self.inner.match_pos.set(None);
        self.reset_search_bar();
        self.refresh()?;
        if let Some(cb) = self.inner.on_search_close.borrow().as_ref() {
            cb();
        }
        Ok(())
    }

    /// debug-server 用：バーを（必要なら開いて）入力欄に文字列を入れ、その場で検索する。
    /// headless は run loop が止まり合成キー/通知が届かないため、ここで直接適用する。
    #[cfg(feature = "debug-server")]
    pub fn debug_set_bar_text(&self, text: &str) -> w::AnyResult<()> {
        if !self.inner.search_active.get() {
            self.open_search_bar()?;
        }
        let _ = self.inner.search_edit.hwnd().SetWindowText(text);
        self.apply_search_from_edit()
    }

    /// debug-server 用：検索オプションを名前で切り替えて再検索する（headless は実クリックが
    /// 届かないため直接適用）。未知の名前なら `false`。
    #[cfg(feature = "debug-server")]
    pub fn debug_set_option(&self, name: &str, on: bool) -> w::AnyResult<bool> {
        let mut o = self.inner.search_opts.get();
        match name {
            "case_sensitive" | "case" => o.case_sensitive = on,
            // 単語一致と正規表現は排他（片方 ON で他方 OFF）。
            "whole_word" | "word" => {
                o.whole_word = on;
                if on {
                    o.regex = false;
                }
            }
            "regex" => {
                o.regex = on;
                if on {
                    o.whole_word = false;
                }
            }
            _ => return Ok(false),
        }
        self.inner.search_opts.set(o);
        self.inner.search_case.set_check(!o.case_sensitive);
        self.inner.search_word.set_check(o.whole_word);
        self.inner.search_regex.set_check(o.regex);
        let term = self.inner.search_term.borrow().clone();
        self.set_search(&term)?;
        Ok(true)
    }

    /// debug-server 用：検索履歴（新しい順）。
    #[cfg(feature = "debug-server")]
    pub fn debug_history(&self) -> Vec<String> {
        rerics_core::InputHistory::load().get("search")
    }

    /// debug-server 用：履歴ドロップダウンが開いているか。
    #[cfg(feature = "debug-server")]
    pub fn debug_is_dropdown_open(&self) -> bool {
        self.inner.list_open.get()
    }

    /// debug-server 用：ニーモニック（c/w/r）を駆動する。Alt+キーがユーザーバインドに割り当て
    /// 済みならそちらを優先し、無ければトグルを反転する（SYSKEYDOWN 経路と同じ判断）。
    #[cfg(feature = "debug-server")]
    pub fn debug_mnemonic(&self, key: char) -> w::AnyResult<bool> {
        let (vk, kind) = match key.to_ascii_lowercase() {
            'c' => (0x43u16, OptKind::Case),
            'w' => (0x57, OptKind::Word),
            'r' => (0x52, OptKind::Regex),
            _ => return Ok(false),
        };
        let chord = rerics_core::KeyChord::new(vk, false, false, true);
        let user = self.inner.on_chord.borrow().as_ref().map(|cb| cb(chord)).unwrap_or(false);
        if !user {
            self.toggle_option(kind)?;
        }
        Ok(true)
    }

    /// debug-server 用：履歴ドロップダウンを開く/閉じる（headless でも開閉を観測できるように）。
    #[cfg(feature = "debug-server")]
    pub fn debug_dropdown(&self, open: bool) -> w::AnyResult<()> {
        if open {
            if !self.inner.search_active.get() {
                self.open_search_bar()?;
            }
            if !self.inner.list_open.get() {
                self.open_history_dropdown()?;
            }
        } else {
            self.close_history_dropdown(true);
        }
        Ok(())
    }

    /// debug-server 用：履歴の index 番目（新しい順）を入力欄へ入れて検索する。範囲外は `false`。
    #[cfg(feature = "debug-server")]
    pub fn debug_select_history(&self, index: usize) -> w::AnyResult<bool> {
        let items = rerics_core::InputHistory::load().get("search");
        match items.get(index) {
            Some(it) => {
                if !self.inner.search_active.get() {
                    self.open_search_bar()?;
                }
                self.set_query(it)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// 検索バーの高さ（本文をこのぶん下へずらす）。
    fn search_bar_height(&self) -> i32 {
        self.inner.line_height.get() + gui::dpi_y(12)
    }

    /// 本文描画の上端オフセット（検索バーが開いていればその高さ、閉じていれば 0）。
    fn body_top(&self) -> i32 {
        if self.inner.search_active.get() {
            self.search_bar_height()
        } else {
            0
        }
    }

    /// 検索バーの各要素の矩形（x, 幅）と共通の y・高さ。入力欄・トグル3・前後ボタン・
    /// カウンタを、配置（layout）と自前ミラー描画（draw）で共有する。
    fn search_bar_geom(&self, cw: i32) -> BarGeom {
        let bar_h = self.search_bar_height();
        let pad = gui::dpi_x(6);
        let gap = gui::dpi_x(4);
        // トグルはラベルに合わせて個別幅（チェック枠＋日本語ラベル＋(&X) ぶん）。
        let case_w = gui::dpi_x(140);
        let word_w = gui::dpi_x(112);
        let regex_w = gui::dpi_x(112);
        let btn_w = gui::dpi_x(26);
        let counter_w = gui::dpi_x(72);
        let h = self.inner.line_height.get().max(gui::dpi_y(18));
        let y = ((bar_h - h) / 2).max(0);
        // 入力欄は左寄せでおよそ半分の幅にし、右端に履歴▼を密着させる（コンボの矢印風）。
        // ボックス幅の目安には「クラスタを右寄せしたときの左端」を参照に使う（幅は従来どおり）。
        let hist_w = gui::dpi_x(20);
        let cluster_w = case_w + word_w + regex_w + btn_w * 2 + gap * 5;
        let ref_right = (cw - pad - counter_w - gap - cluster_w).max(pad);
        let input_span = (ref_right - gap - pad).max(gui::dpi_x(80));
        let box_total = (input_span / 2).max(gui::dpi_x(160)).min(input_span);
        let edit_w = (box_total - hist_w).max(gui::dpi_x(120));
        let hist_x = pad + edit_w;
        // トグル・ボタン・カウンタはボックスのすぐ右へ左寄せで並べる（右寄せにしない）。
        let case_x = hist_x + hist_w + gap * 2;
        let word_x = case_x + case_w + gap;
        let regex_x = word_x + word_w + gap;
        let prev_x = regex_x + regex_w + gap;
        let next_x = prev_x + btn_w + gap;
        let counter_x = next_x + btn_w + gap;
        BarGeom {
            y,
            h,
            edit: (pad, edit_w),
            history: (hist_x, hist_w),
            case: (case_x, case_w),
            word: (word_x, word_w),
            regex: (regex_x, regex_w),
            prev: (prev_x, btn_w),
            next: (next_x, btn_w),
            counter: (counter_x, counter_w),
        }
    }

    /// 検索バーの入力欄と右側の操作列（トグル3＋前/次ボタン）を配置する。
    fn layout_search_bar(&self) {
        if !self.inner.search_active.get() {
            return;
        }
        let cw = self.hwnd().GetClientRect().map(|r| r.right - r.left).unwrap_or(0);
        let g = self.search_bar_geom(cw);
        let mv = |hwnd: &w::HWND, (x, w): (i32, i32)| {
            let _ = hwnd.MoveWindow(w::POINT { x, y: g.y }, w::SIZE { cx: w, cy: g.h }, true);
        };
        mv(self.inner.search_edit.hwnd(), g.edit);
        mv(self.inner.search_history.hwnd(), g.history);
        mv(self.inner.search_case.hwnd(), g.case);
        mv(self.inner.search_word.hwnd(), g.word);
        mv(self.inner.search_regex.hwnd(), g.regex);
        mv(self.inner.search_prev.hwnd(), g.prev);
        mv(self.inner.search_next.hwnd(), g.next);
    }

    /// 検索バーの帯と、各コントロールの「ミラー」を自前描画する。実機では本物の子コントロールが
    /// 同じ位置に重なって隠すが、子コントロールは自前描画でなくスナップショットに写らないため、
    /// headless でも入力文字・トグル状態・件数が見えるようここで写し描きする。
    fn draw_search_bar(&self, dc: &w::HDC, cw: i32) -> w::AnyResult<()> {
        let bar_h = self.search_bar_height();
        let brush = w::HBRUSH::CreateSolidBrush(chrome::face())?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: bar_h }, &brush)?;
        chrome::hline(dc, 0, cw, bar_h - 1, chrome::highlight())?;

        let g = self.search_bar_geom(cw);
        let o = self.inner.search_opts.get();
        let term = self.inner.search_term.borrow().clone();
        // 入力欄ミラー（白地・枠・検索語）。
        self.draw_bar_input(dc, g.edit, g.y, g.h, &term)?;
        // トグル3つ（[x]/[ ]＋ラベル）。「大小」は ON＝大小無視。
        let sfont = self.create_font_sized((self.inner.font_size - 1).max(6))?;
        let _sel = dc.SelectObject(&*sfont)?;
        dc.SetTextColor(chrome::text())?;
        self.draw_bar_button(dc, g.history, g.y, g.h, "▼")?;
        self.draw_bar_toggle(dc, g.case, g.y, g.h, "ケースを無視", !o.case_sensitive)?;
        self.draw_bar_toggle(dc, g.word, g.y, g.h, "単語境界", o.whole_word)?;
        self.draw_bar_toggle(dc, g.regex, g.y, g.h, "正規表現", o.regex)?;
        self.draw_bar_button(dc, g.prev, g.y, g.h, "↑")?;
        self.draw_bar_button(dc, g.next, g.y, g.h, "↓")?;
        // 一致カウンタ（語が空なら出さず・一致なしは 0 件）。
        if !term.is_empty() {
            let matches = self.all_matches();
            let total = matches.len();
            let text = if total == 0 {
                "0 件".to_string()
            } else {
                let cur = self
                    .inner
                    .match_pos
                    .get()
                    .and_then(|c| matches.iter().position(|&m| m == c))
                    .map(|i| i + 1)
                    .unwrap_or(0);
                format!("{cur} / {total}")
            };
            let (cx, cwd) = g.counter;
            let rect = w::RECT { left: cx + gui::dpi_x(4), top: 0, right: cx + cwd, bottom: bar_h };
            dc.DrawText(
                &text,
                rect,
                co::DT::SINGLELINE | co::DT::VCENTER | co::DT::LEFT | co::DT::NOPREFIX,
            )?;
        }
        Ok(())
    }

    /// 入力欄ミラー：白地＋枠＋検索語（左寄せ）。
    fn draw_bar_input(&self, dc: &w::HDC, (x, w): (i32, i32), y: i32, h: i32, term: &str) -> w::AnyResult<()> {
        let r = w::RECT { left: x, top: y, right: x + w, bottom: y + h };
        let white = w::HBRUSH::CreateSolidBrush(chrome::window())?;
        dc.FillRect(r, &white)?;
        chrome::hline(dc, x, x + w, y, chrome::shadow())?;
        chrome::hline(dc, x, x + w, y + h - 1, chrome::shadow())?;
        chrome::vline(dc, x, y, y + h, chrome::shadow())?;
        chrome::vline(dc, x + w - 1, y, y + h, chrome::shadow())?;
        if !term.is_empty() {
            let efont = self.create_font_sized((self.inner.font_size - 1).max(6))?;
            let _sel = dc.SelectObject(&*efont)?;
            dc.SetTextColor(chrome::text())?;
            let pad = gui::dpi_x(4);
            let tr = w::RECT { left: x + pad, top: y, right: x + w - pad, bottom: y + h };
            dc.DrawText(term, tr, co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX | co::DT::END_ELLIPSIS)?;
        }
        Ok(())
    }

    /// トグルミラー：`[x] ラベル`／`[ ] ラベル`（ON で塗り枠を付けて目立たせる）。
    fn draw_bar_toggle(&self, dc: &w::HDC, (x, w): (i32, i32), y: i32, h: i32, label: &str, on: bool) -> w::AnyResult<()> {
        if on {
            let r = w::RECT { left: x, top: y, right: x + w, bottom: y + h };
            chrome::hline(dc, x, x + w, y, chrome::shadow())?;
            chrome::hline(dc, x, x + w, y + h - 1, chrome::shadow())?;
            chrome::vline(dc, x, y, y + h, chrome::shadow())?;
            chrome::vline(dc, x + w - 1, y, y + h, chrome::shadow())?;
            let _ = r;
        }
        let text = format!("[{}]{}", if on { "x" } else { " " }, label);
        let r = w::RECT { left: x + gui::dpi_x(2), top: y, right: x + w, bottom: y + h };
        dc.DrawText(&text, r, co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX)?;
        Ok(())
    }

    /// ボタンミラー：枠付きの矢印。
    fn draw_bar_button(&self, dc: &w::HDC, (x, w): (i32, i32), y: i32, h: i32, glyph: &str) -> w::AnyResult<()> {
        chrome::hline(dc, x, x + w, y, chrome::shadow())?;
        chrome::hline(dc, x, x + w, y + h - 1, chrome::shadow())?;
        chrome::vline(dc, x, y, y + h, chrome::shadow())?;
        chrome::vline(dc, x + w - 1, y, y + h, chrome::shadow())?;
        let r = w::RECT { left: x, top: y, right: x + w, bottom: y + h };
        dc.DrawText(glyph, r, co::DT::SINGLELINE | co::DT::VCENTER | co::DT::CENTER | co::DT::NOPREFIX)?;
        Ok(())
    }

    /// 現在の検索語＋オプションのコンパイル済みマッチャを返す（キャッシュ。変わるまで使い回す）。
    /// 正規表現を行ごとに再コンパイルしないための要。
    fn matcher(&self) -> Matcher {
        let term = self.inner.search_term.borrow();
        let opts = self.inner.search_opts.get();
        if let Some((t, o, m)) = self.inner.matcher_cache.borrow().as_ref()
            && t == &*term && *o == opts {
                return m.clone();
            }
        let m = build_matcher(&term, &opts);
        *self.inner.matcher_cache.borrow_mut() = Some((term.clone(), opts, m.clone()));
        m
    }

    /// 全表示行の検索語一致 `(行, 開始桁)` を昇順で返す（キャッシュ）。ビューア内容は起動中に
    /// 変わらないので、検索語・オプション・表示行の世代が変わるまで作り直さず使い回す。
    fn all_matches(&self) -> Rc<Vec<(usize, usize)>> {
        let term = self.inner.search_term.borrow();
        let opts = self.inner.search_opts.get();
        let generation = self.inner.lines_gen.get();
        if let Some((t, o, g, m)) = self.inner.match_cache.borrow().as_ref()
            && t == &*term && *o == opts && *g == generation {
                return m.clone();
            }
        let matches = if term.is_empty() {
            Vec::new()
        } else {
            let matcher = self.matcher();
            let lines = self.inner.lines.borrow();
            let mut out = Vec::new();
            for (li, line) in lines.iter().enumerate() {
                for (off, _len) in matcher.find(&line.body) {
                    out.push((li, off));
                }
            }
            out
        };
        let rc = Rc::new(matches);
        *self.inner.match_cache.borrow_mut() = Some((term.clone(), opts, generation, rc.clone()));
        rc
    }

    /// 現在一致の長さ（debug 観測・正規表現で可変長になるため）。無ければ 0。
    #[cfg(feature = "debug-server")]
    fn current_match_len(&self) -> usize {
        let Some((line, col)) = self.inner.match_pos.get() else {
            return 0;
        };
        let matcher = self.matcher();
        let lines = self.inner.lines.borrow();
        let Some(dl) = lines.get(line) else { return 0 };
        matcher
            .find(&dl.body)
            .into_iter()
            .find(|&(off, _)| off == col)
            .map(|(_, len)| len)
            .unwrap_or(0)
    }

    /// 指定の一致（行・桁）を現在一致にし、行が可視範囲（上から1/4の位置）に入るようスクロールする。
    fn jump_to(&self, line: usize, col: usize) {
        self.inner.match_pos.set(Some((line, col)));
        let page = self.page_rows();
        let total = self.inner.lines.borrow().len();
        let max_top = total.saturating_sub(page);
        // 一致行が見えていなければ、少し上に余白を取って表示する。
        let top = self.inner.scroll_top.get();
        if line < top || line >= top + page {
            let margin = page / 4;
            let new_top = line.saturating_sub(margin).min(max_top);
            self.inner.scroll_top.set(new_top);
        }
    }

    /// 相対行スクロール（正＝下）。
    pub fn scroll_by(&self, delta: isize) -> w::AnyResult<()> {
        let total = self.inner.lines.borrow().len();
        let page = self.page_rows();
        let max_top = total.saturating_sub(page);
        let cur = self.inner.scroll_top.get() as isize;
        let next = (cur + delta).clamp(0, max_top as isize) as usize;
        self.inner.scroll_top.set(next);
        self.refresh()
    }

    pub fn scroll_page(&self, down: bool) -> w::AnyResult<()> {
        let page = (self.page_rows() as isize - 1).max(1);
        self.scroll_by(if down { page } else { -page })
    }

    pub fn scroll_home(&self) -> w::AnyResult<()> {
        self.inner.scroll_top.set(0);
        self.refresh()
    }

    pub fn scroll_end(&self) -> w::AnyResult<()> {
        let total = self.inner.lines.borrow().len();
        let page = self.page_rows();
        self.inner.scroll_top.set(total.saturating_sub(page));
        self.refresh()
    }

    /// ホイール（1ノッチ3行）。
    pub fn scroll_by_wheel(&self, distance: i16) -> w::AnyResult<()> {
        let notches = distance as i32 / 120;
        self.scroll_by((-notches * 3) as isize)
    }

    fn page_rows(&self) -> usize {
        let lh = self.inner.line_height.get().max(1);
        let h = self.body_height();
        ((h / lh).max(1)) as usize
    }

    /// 本文領域の高さ（上の検索バーと下の状態行を除く）。
    fn body_height(&self) -> i32 {
        let ch = self.hwnd().GetClientRect().map(|r| r.bottom - r.top).unwrap_or(0);
        (ch - self.status_height() - self.body_top()).max(0)
    }

    fn status_height(&self) -> i32 {
        self.inner.line_height.get() + gui::dpi_y(6)
    }

    fn create_font(&self) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
        self.create_font_sized(self.inner.font_size)
    }

    fn create_font_sized(&self, size: i32) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
        w::HFONT::CreateFont(
            w::SIZE { cx: 0, cy: -gui::dpi_y(size) },
            0,
            0,
            co::FW::NORMAL,
            false,
            false,
            false,
            co::CHARSET::DEFAULT,
            co::OUT_PRECIS::DEFAULT,
            co::CLIP::DEFAULT_PRECIS,
            co::QUALITY::CLEARTYPE,
            co::PITCH::FIXED,
            &self.inner.font_family,
        )
    }

    /// 行番号欄の (右端 x, 縦線 x, 本文左 x) を返す（物理 px）。
    fn gutter_geometry(&self) -> (i32, i32, i32) {
        let cwd = self.inner.char_width.get().max(1);
        let g = self.inner.gutter_chars.get().max(4) as i32;
        let num_right = cwd / 2 + g * cwd;
        let sep_x = num_right + cwd / 2;
        let content_left = sep_x + cwd / 2;
        (num_right, sep_x, content_left)
    }

    fn setup_events(&self) {
        crate::winutil::passive_focus(&self.wnd);

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm_mouse_wheel(move |p| {
            let dist = p.keys.raw() as i16;
            this.scroll_by_wheel(dist)?;
            Ok(())
        });

        // マウスで範囲選択（キャレットが無いのでマウス選択が唯一の手段）。
        let this = self.clone();
        self.wnd.on().wm_l_button_down(move |p| {
            let pos = this.point_to_pos(p.coords);
            this.inner.sel_anchor.set(Some(pos));
            this.inner.sel_cursor.set(Some(pos));
            this.inner.selecting.set(true);
            std::mem::forget(this.hwnd().SetCapture());
            this.refresh()?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_move(move |p| {
            if this.inner.selecting.get() {
                let pos = this.point_to_pos(p.coords);
                this.inner.sel_cursor.set(Some(pos));
                this.refresh()?;
            }
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_up(move |_p| {
            if this.inner.selecting.get() {
                this.inner.selecting.set(false);
                drop(this.hwnd().SetCapture());
            }
            Ok(())
        });

        // ダブルクリックでカーソル下の単語を選択する。
        let this = self.clone();
        self.wnd.on().wm_l_button_dbl_clk(move |p| {
            this.select_word_at(p.coords);
            this.refresh()?;
            Ok(())
        });

        // 右クリックでコンテキストメニューを開く（表示は MainWindow が担う）。
        let this = self.clone();
        self.wnd.on().wm_r_button_down(move |p| {
            let screen = this.hwnd().ClientToScreen(p.coords).unwrap_or(p.coords);
            if let Some(cb) = this.inner.on_menu.borrow().as_ref() {
                cb(screen);
            }
            Ok(())
        });

        // リサイズ時、開いていれば検索バーを幅へ追従させる。
        let this = self.clone();
        self.wnd.on().wm_size(move |_| {
            this.layout_search_bar();
            Ok(())
        });

        // 入力中はリアルタイム検索（インクリメンタル）。
        let this = self.clone();
        self.inner.search_edit.on().en_change(move || {
            this.apply_search_from_edit()?;
            Ok(())
        });

        // 入力欄内のキー：↑↓ で一致移動・Enter 確定・Esc 取消。それ以外（左右/Home/End 等の
        // キャレット移動）は既定処理へ通す。Enter/Esc は WM_CHAR を飲んでビープを抑える。
        let this = self.clone();
        self.inner.search_edit.on_subclass().wm(co::WM::KEYDOWN, move |p| {
            let handled = match p.wparam as u16 {
                0x26 => {
                    this.find_next(false)?;
                    true
                } // VK_UP
                0x28 => {
                    this.find_next(true)?;
                    true
                } // VK_DOWN
                0x0D => {
                    this.confirm_search_bar()?;
                    true
                } // VK_RETURN
                0x1B => {
                    this.cancel_search_bar()?;
                    true
                } // VK_ESCAPE
                _ => false,
            };
            if handled {
                Ok(0)
            } else {
                Ok(unsafe { this.inner.search_edit.hwnd().DefSubclassProc(p) })
            }
        });
        let this = self.clone();
        self.inner.search_edit.on_subclass().wm(co::WM::CHAR, move |p| {
            if matches!(p.wparam as u16, 0x0D | 0x1B) {
                Ok(0)
            } else {
                Ok(unsafe { this.inner.search_edit.hwnd().DefSubclassProc(p) })
            }
        });

        // 右側トグル（マウス用）。クリックでフラグを更新→再検索→入力欄へフォーカスを戻す。
        // 「ケースを無視」ON＝大小無視。単語/正規は排他（apply_option が担う）。
        let this = self.clone();
        self.inner.search_case.on().bn_clicked(move || {
            this.apply_option(OptKind::Case, this.inner.search_case.is_checked())
        });
        let this = self.clone();
        self.inner.search_word.on().bn_clicked(move || {
            this.apply_option(OptKind::Word, this.inner.search_word.is_checked())
        });
        let this = self.clone();
        self.inner.search_regex.on().bn_clicked(move || {
            this.apply_option(OptKind::Regex, this.inner.search_regex.is_checked())
        });
        // 前/次ボタン（入力欄内の ↑↓ キーと同機能）。
        let this = self.clone();
        self.inner.search_prev.on().bn_clicked(move || {
            this.find_next(false)?;
            this.inner.search_edit.hwnd().SetFocus();
            Ok(())
        });
        let this = self.clone();
        self.inner.search_next.on().bn_clicked(move || {
            this.find_next(true)?;
            this.inner.search_edit.hwnd().SetFocus();
            Ok(())
        });
        // ▼：検索履歴のドロップダウン（メニュー）を開く。
        let this = self.clone();
        self.inner.search_history.on().bn_clicked(move || {
            this.open_history_dropdown()?;
            Ok(())
        });

        // 入力欄での Alt 併用ショートカット（WM_SYSKEYDOWN）：Alt+↑↓ で履歴ドロップダウン、
        // Alt+C/W/R でトグル。ただし同じ Alt+キーがユーザーのビューアキーバインドに割り当て済みなら
        // そちらを優先する（被ったらユーザー優先）。扱わないキーは既定処理へ。
        let this = self.clone();
        self.inner.search_edit.on_subclass().wm(co::WM::SYSKEYDOWN, move |p| {
            let vk = p.wparam as u16;
            match vk {
                0x26 | 0x28 | 0x43 | 0x57 | 0x52 => {
                    let chord = rerics_core::KeyChord::new(vk, false, false, true);
                    let user_handled = this
                        .inner
                        .on_chord
                        .borrow()
                        .as_ref()
                        .map(|cb| cb(chord))
                        .unwrap_or(false);
                    if !user_handled {
                        match vk {
                            0x26 | 0x28 => this.open_history_dropdown()?,
                            0x43 => this.toggle_option(OptKind::Case)?,
                            0x57 => this.toggle_option(OptKind::Word)?,
                            0x52 => this.toggle_option(OptKind::Regex)?,
                            _ => {}
                        }
                    }
                    Ok(0)
                }
                _ => Ok(unsafe { this.inner.search_edit.hwnd().DefSubclassProc(p) }),
            }
        });
        // Alt+C/W/R に伴う WM_SYSCHAR を食って、メニューバーのニーモニック（登録(R) 等）へ
        // 貫通させない（バーが開いている間は c/w/r を消費する。Esc で抜ければメニューは使える）。
        let this = self.clone();
        self.inner.search_edit.on_subclass().wm(co::WM::SYSCHAR, move |p| {
            if matches!((p.wparam as u8).to_ascii_lowercase(), b'c' | b'w' | b'r') {
                Ok(0)
            } else {
                Ok(unsafe { this.inner.search_edit.hwnd().DefSubclassProc(p) })
            }
        });

        // 履歴リスト内のキー：Enter で確定・Esc で取消（↑↓ はネイティブ選択移動に任せる）。
        let this = self.clone();
        self.inner.search_list.on_subclass().wm(co::WM::KEYDOWN, move |p| match p.wparam as u16 {
            0x0D => {
                this.pick_history_selection()?;
                Ok(0)
            }
            0x1B => {
                this.close_history_dropdown(true);
                Ok(0)
            }
            _ => Ok(unsafe { this.inner.search_list.hwnd().DefSubclassProc(p) }),
        });
        // クリックで確定（ボタンアップで選択が確定した後に拾う）。
        let this = self.clone();
        self.inner.search_list.on_subclass().wm(co::WM::LBUTTONUP, move |p| {
            let r = unsafe { this.inner.search_list.hwnd().DefSubclassProc(p) };
            this.pick_history_selection()?;
            Ok(r)
        });
        // リスト外をクリック等でフォーカスを失ったら閉じる。
        let this = self.clone();
        self.inner.search_list.on_subclass().wm(co::WM::KILLFOCUS, move |p| {
            this.close_history_dropdown(false);
            Ok(unsafe { this.inner.search_list.hwnd().DefSubclassProc(p) })
        });
    }

    /// トグル変更後の共通処理：現在の入力で再検索し、フォーカスを入力欄へ戻す。
    fn refocus_after_toggle(&self) -> w::AnyResult<()> {
        self.apply_search_from_edit()?;
        self.inner.search_edit.hwnd().SetFocus();
        Ok(())
    }

    /// 全選択（先頭から末尾まで）。
    pub fn select_all(&self) {
        let lines = self.inner.lines.borrow();
        if lines.is_empty() {
            return;
        }
        let last = lines.len() - 1;
        let last_col = lines[last].body.chars().count();
        drop(lines);
        self.inner.sel_anchor.set(Some((0, 0)));
        self.inner.sel_cursor.set(Some((last, last_col)));
    }

    /// `pt` 直下の単語を選択する（語＝英数字＋アンダースコアの連なり。その他は1文字）。
    fn select_word_at(&self, pt: w::POINT) {
        let (line, col) = self.point_to_pos(pt);
        let lines = self.inner.lines.borrow();
        let Some(dl) = lines.get(line) else { return };
        let (s, e) = word_bounds(&dl.body, col);
        drop(lines);
        if s == e {
            self.clear_selection();
        } else {
            self.inner.sel_anchor.set(Some((line, s)));
            self.inner.sel_cursor.set(Some((line, e)));
        }
    }

    /// マウス座標を表示行内の位置 (行, char オフセット) へ変換する。
    fn point_to_pos(&self, pt: w::POINT) -> Pos {
        let lines = self.inner.lines.borrow();
        if lines.is_empty() {
            return (0, 0);
        }
        let lh = self.inner.line_height.get().max(1);
        let top = self.inner.scroll_top.get();
        let row = ((pt.y - self.body_top()).max(0) / lh) as usize;
        let line = (top + row).min(lines.len() - 1);
        let (_, _, content_left) = self.gutter_geometry();
        let cwd = self.inner.char_width.get().max(1);
        let target = ((pt.x - content_left).max(0) + cwd / 2) / cwd; // 最寄り境界（表示セル数）
        let mut acc = 0i32;
        let mut col = 0usize;
        for ch in lines[line].body.chars() {
            let wch = UnicodeWidthChar::width(ch).unwrap_or(0) as i32;
            if target < acc + (wch + 1) / 2 {
                break;
            }
            acc += wch.max(1);
            col += 1;
        }
        (line, col)
    }

    /// 本文 1 行を描く。文字ごとに「構文色（無ければ本文色）→検索一致なら検索/選択色」で
    /// 上書きした実効色を決め、同色の連なりごとにまとめて描画する。
    #[allow(clippy::too_many_arguments)]
    fn draw_body_line(
        &self,
        dc: &w::HDC,
        line: &DisplayLine,
        spans: &[(usize, usize)],
        cur_off: Option<usize>,
        y: i32,
        content_left: i32,
        cwd: i32,
        colors: &Colors,
    ) -> w::AnyResult<()> {
        let chars: Vec<char> = line.body.chars().collect();
        let n = chars.len();
        if n == 0 {
            return Ok(());
        }
        // 基本色（構文ハイライトがあれば各文字へ展開、無ければ本文色）。
        let mut col = vec![colors.viewer_text; n];
        if !line.colors.is_empty() {
            for (r, &(start, c)) in line.colors.iter().enumerate() {
                let end = line.colors.get(r + 1).map(|(s, _)| *s).unwrap_or(n).min(n);
                for slot in col.iter_mut().take(end).skip(start) {
                    *slot = c;
                }
            }
        }
        // 検索一致の桁を上書き（現在一致＝選択文字色・他＝検索文字色）。
        for &(off, len) in spans {
            let oc = if cur_off == Some(off) { colors.selected_file } else { colors.viewer_find_text };
            let end = (off + len).min(n);
            for slot in col.iter_mut().take(end).skip(off) {
                *slot = oc;
            }
        }
        // 同色のランごとに TextOut。
        let mut p = 0;
        while p < n {
            let c0 = col[p];
            let mut q = p + 1;
            while q < n && col[q] == c0 {
                q += 1;
            }
            let sub: String = chars[p..q].iter().collect();
            let x = self.col_x(&line.body, p, content_left, cwd);
            dc.SetTextColor(rgb(c0))?;
            dc.TextOut(x, y, &sub)?;
            p = q;
        }
        Ok(())
    }

    /// content 左端からの char オフセット col の x 座標（物理 px）。
    fn col_x(&self, body: &str, col: usize, content_left: i32, cwd: i32) -> i32 {
        let cells: i32 = body
            .chars()
            .take(col)
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0) as i32)
            .sum();
        content_left + cells * cwd
    }

    /// 正規化した選択範囲（始点 <= 終点）。長さ 0 や未選択なら None。
    fn normalized_selection(&self) -> Option<(Pos, Pos)> {
        let a = self.inner.sel_anchor.get()?;
        let c = self.inner.sel_cursor.get()?;
        if a == c {
            return None;
        }
        Some(if a <= c { (a, c) } else { (c, a) })
    }

    /// 選択範囲のテキストを組み立てる（折返し継続行は改行を入れず論理行を復元）。
    fn selected_text(&self) -> String {
        let Some((s, e)) = self.normalized_selection() else {
            return String::new();
        };
        let lines = self.inner.lines.borrow();
        let mut out = String::new();
        for li in s.0..=e.0 {
            let chars: Vec<char> = lines[li].body.chars().collect();
            let from = if li == s.0 { s.1.min(chars.len()) } else { 0 };
            let to = if li == e.0 { e.1.min(chars.len()) } else { chars.len() };
            if li > s.0 && !lines[li].gutter.is_empty() {
                out.push('\n');
            }
            out.extend(&chars[from.min(to)..to]);
        }
        out
    }

    /// 選択範囲をクリップボードへコピーする（CF_UNICODETEXT）。
    pub fn copy_selection(&self) -> w::AnyResult<()> {
        let text = self.selected_text();
        if text.is_empty() {
            return Ok(());
        }
        let mut u16s: Vec<u16> = text.encode_utf16().collect();
        u16s.push(0);
        let bytes: Vec<u8> = u16s.iter().flat_map(|u| u.to_le_bytes()).collect();
        let clip = self.hwnd().OpenClipboard()?;
        clip.EmptyClipboard()?;
        clip.SetClipboardData(co::CF::UNICODETEXT, &bytes)?;
        Ok(())
    }

    /// 折返し桁を現在のクライアント幅から算出する。
    fn wrap_cols(&self) -> usize {
        let cw = self.hwnd().GetClientRect().map(|r| r.right - r.left).unwrap_or(0);
        let cwd = self.inner.char_width.get().max(1);
        let (_, _, content_left) = self.gutter_geometry();
        let avail = cw - content_left - cwd; // 右に1文字分の余白
        ((avail / cwd).max(1)) as usize
    }

    /// 必要なら表示行を再生成する。
    fn rebuild_if_needed(&self, wrap_cols: usize) {
        if !self.inner.dirty.get() && self.inner.cached_wrap.get() == wrap_cols {
            return;
        }
        let lines = self.inner.model.borrow().lines(wrap_cols, TAB_WIDTH);
        // 行番号/オフセットの桁数。テキストは最低4桁（5桁以上は収まる分に拡張）。
        let gutter_chars = lines.iter().map(|l| l.gutter.chars().count()).max().unwrap_or(1).max(4);
        self.inner.gutter_chars.set(gutter_chars);
        *self.inner.lines.borrow_mut() = lines;
        self.inner.cached_wrap.set(wrap_cols);
        self.inner.dirty.set(false);
        // 表示行が変わったので一致リストのキャッシュ世代を進める。
        self.inner.lines_gen.set(self.inner.lines_gen.get().wrapping_add(1));
        // スクロール位置をクランプ。
        let total = self.inner.lines.borrow().len();
        let page = self.page_rows();
        let max_top = total.saturating_sub(page);
        if self.inner.scroll_top.get() > max_top {
            self.inner.scroll_top.set(max_top);
        }
    }

    fn on_paint(&self) -> w::AnyResult<()> {
        let hdc = self.hwnd().BeginPaint()?;
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        let mem_dc = hdc.CreateCompatibleDC()?;
        let bmp = hdc.CreateCompatibleBitmap(cw, ch)?;
        let _bmp_sel = mem_dc.SelectObject(&*bmp)?;

        self.render_to(&mem_dc, cw, ch)?;

        hdc.BitBlt(
            w::POINT { x: 0, y: 0 },
            w::SIZE { cx: cw, cy: ch },
            &mem_dc,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    /// ターゲットビットマップ選択済みの任意 DC へ全面描画する（フォント準備＋`paint_to`）。
    pub(crate) fn render_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let font = self.create_font()?;
        let _font_sel = dc.SelectObject(&*font)?;
        // メトリクス実測（行高・文字幅）。
        if let Ok(tm) = dc.GetTextMetrics() {
            self.inner.line_height.set(tm.tmHeight + gui::dpi_y(2));
            self.inner.char_width.set((tm.tmAveCharWidth).max(1));
        }
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        self.paint_to(dc, cw, ch)
    }

    fn paint_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let colors = self.inner.colors;
        let wrap_cols = self.wrap_cols();
        self.rebuild_if_needed(wrap_cols);

        let body_h = self.body_height();
        let top_y = self.body_top();
        let lh = self.inner.line_height.get().max(1);
        let cwd = self.inner.char_width.get().max(1);
        let (num_right, sep_x, content_left) = self.gutter_geometry();
        let sel = self.normalized_selection();

        // 背景。
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.viewer_background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        // 行番号欄とコンテンツを仕切る縦線（本文領域のみ）。
        chrome::vline(dc, sep_x, top_y, top_y + body_h, rgb(colors.viewer_separator))?;

        // 本文。
        let is_text = self.inner.model.borrow().mode == ViewMode::Text;
        let lines = self.inner.lines.borrow();
        let top = self.inner.scroll_top.get();
        let match_pos = self.inner.match_pos.get();
        let has_term = !self.inner.search_term.borrow().is_empty();
        let matcher = self.matcher();
        let sel_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.selected_file_bg))?;
        let find_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.viewer_find_bg))?;
        let mut y = top_y;
        let mut i = top;
        while i < lines.len() && y < top_y + body_h {
            let line = &lines[i];
            // マウス選択のハイライト（行内の桁範囲）。選択は検索ハイライトより優先。
            let mut highlighted = false;
            if let Some((s, e)) = sel
                && i >= s.0 && i <= e.0 {
                    let left = if i == s.0 { self.col_x(&line.body, s.1, content_left, cwd) } else { content_left };
                    let right = if i == e.0 { self.col_x(&line.body, e.1, content_left, cwd) } else { cw };
                    if right > left {
                        dc.FillRect(w::RECT { left, top: y, right, bottom: y + lh }, &sel_brush)?;
                    }
                    highlighted = true;
                }
            // 検索一致（選択していない行のみ）。行内の各一致を桁単位で塗り、現在一致は選択色で区別する。
            let spans: Vec<(usize, usize)> = if highlighted || !has_term {
                Vec::new()
            } else {
                matcher.find(&line.body)
            };
            let cur_off = match match_pos {
                Some((ml, mc)) if ml == i => Some(mc),
                _ => None,
            };
            for &(off, len) in &spans {
                let left = self.col_x(&line.body, off, content_left, cwd);
                let right = self.col_x(&line.body, off + len, content_left, cwd);
                let brush = if cur_off == Some(off) { &sel_brush } else { &find_brush };
                dc.FillRect(w::RECT { left, top: y, right, bottom: y + lh }, brush)?;
            }
            if !line.gutter.is_empty() {
                dc.SetTextColor(rgb(colors.viewer_line))?;
                let rect = w::RECT { left: 0, top: y, right: num_right, bottom: y + lh };
                dc.DrawText(&line.gutter, rect, co::DT::SINGLELINE | co::DT::RIGHT | co::DT::NOPREFIX)?;
            }
            if !line.body.is_empty() {
                self.draw_body_line(dc, line, &spans, cur_off, y, content_left, cwd, &colors)?;
            }
            // 現在一致のある行に下線（検索カーソル行）。
            if cur_off.is_some() {
                chrome::hline(dc, content_left, cw, y + lh - 1, rgb(colors.viewer_cursor))?;
            }
            // 行末の改行マーク（記号色・本文とは別レイヤーなので選択・コピーには混ざらない）。
            if let Some(nl) = line.newline {
                let bx = self.col_x(&line.body, line.body.chars().count(), content_left, cwd);
                dc.SetTextColor(rgb(colors.viewer_symbol))?;
                let rect = w::RECT { left: bx, top: y, right: cw, bottom: y + lh };
                dc.DrawText(newline_glyph(nl), rect, co::DT::SINGLELINE | co::DT::NOPREFIX)?;
            }
            y += lh;
            i += 1;
        }

        // テキストモードでは本文末尾に [EOF] マーカーを出す（記号色）。
        if is_text && i == lines.len()
            && let Some(last) = lines.last() {
                let eof_y = y - lh;
                if eof_y >= 0 {
                    let end_col = last.body.chars().count();
                    // 最終行に改行マークがあれば、その分だけ右にずらして重ねない。
                    let nl_gap = if last.newline.is_some() { cwd * 2 } else { cwd };
                    let x = self.col_x(&last.body, end_col, content_left, cwd) + nl_gap;
                    dc.SetTextColor(rgb(colors.viewer_symbol))?;
                    let rect = w::RECT { left: x, top: eof_y, right: cw, bottom: eof_y + lh };
                    dc.DrawText("[EOF]", rect, co::DT::SINGLELINE | co::DT::NOPREFIX)?;
                }
            }

        // 下端の状態行（chrome のグレー帯）。
        let status_h = self.status_height();
        let sy = ch - status_h;
        self.draw_status(dc, cw, sy, status_h)?;

        // 上端の検索バー帯（入力欄は子コントロールが上に乗る）。
        if self.inner.search_active.get() {
            self.draw_search_bar(dc, cw)?;
        }
        Ok(())
    }

    fn draw_status(&self, dc: &w::HDC, cw: i32, sy: i32, sh: i32) -> w::AnyResult<()> {
        let face = chrome::face();
        let brush = w::HBRUSH::CreateSolidBrush(face)?;
        dc.FillRect(w::RECT { left: 0, top: sy, right: cw, bottom: sy + sh }, &brush)?;
        chrome::hline(dc, 0, cw, sy, chrome::highlight())?;
        // 状態行は本文より少し小さいフォントで（パスバー等と同じ流儀）。
        let sfont = self.create_font_sized((self.inner.font_size - 2).max(6))?;
        let _sfont_sel = dc.SelectObject(&*sfont)?;
        dc.SetTextColor(chrome::text())?;
        let text = self.status_text();
        let pad = self.inner.char_width.get().max(1);
        let rect = w::RECT { left: pad, top: sy, right: cw - pad, bottom: sy + sh };
        dc.DrawText(
            &text,
            rect,
            co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX | co::DT::END_ELLIPSIS,
        )?;
        Ok(())
    }

    fn status_text(&self) -> String {
        let model = self.inner.model.borrow();
        let mode = match model.mode {
            ViewMode::Text => "テキスト",
            ViewMode::Binary => "バイナリ",
        };
        // 構文ハイライトの言語（無ければ何も足さない＝素のテキスト）。
        let syntax = match model.syntax_name() {
            Some(name) => format!("・{name}"),
            None => String::new(),
        };
        let total = self.inner.lines.borrow().len();
        let cur = self.inner.scroll_top.get() + 1;
        let trunc = if self.inner.truncated.get() { "  [先頭のみ]" } else { "" };
        let find = {
            let term = self.inner.search_term.borrow();
            if term.is_empty() {
                String::new()
            } else {
                let hit = if self.inner.match_pos.get().is_some() { "" } else { " (該当なし)" };
                format!("    検索:{}{}", term, hit)
            }
        };
        format!(
            "{}    [{}]  {}{}    {}/{} 行    (C:エンコ B:ダンプ F:検索 Ctrl+C/右ク:コピー Esc:閉じる){}{}",
            self.inner.title.borrow(),
            model.encoding.label(),
            mode,
            syntax,
            cur.min(total.max(1)),
            total,
            trunc,
            find,
        )
    }

}

/// 検索トグルの種類（ニーモニック/クリック共通の切替対象）。
#[derive(Clone, Copy)]
enum OptKind {
    Case,
    Word,
    Regex,
}

/// 検索バー各要素の配置（x, 幅）と共通の y・高さ。layout と自前ミラー描画で共有する。
struct BarGeom {
    y: i32,
    h: i32,
    edit: (i32, i32),
    history: (i32, i32),
    case: (i32, i32),
    word: (i32, i32),
    regex: (i32, i32),
    prev: (i32, i32),
    next: (i32, i32),
    counter: (i32, i32),
}

fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}

/// 改行種別ごとの表示グリフ（CR=左へ復帰・LF=下へ送り・CRLF=復帰改行）。
fn newline_glyph(e: LineEnding) -> &'static str {
    match e {
        LineEnding::Cr => "←",
        LineEnding::Lf => "↓",
        LineEnding::CrLf => "↵",
    }
}

/// 文字の種別（語・空白・記号）。ダブルクリックは同種の連なりを選ぶ。
#[derive(PartialEq, Eq, Clone, Copy)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Space
    } else {
        CharClass::Other
    }
}

/// `body` の char オフセット `col` 直下の「同種の連なり」の範囲 `[s, e)` を返す。
/// 語（英数字＋アンダースコア）・空白・記号をそれぞれ別種として連なりをまとめる。
fn word_bounds(body: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = body.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let i = col.min(chars.len() - 1);
    let target = char_class(chars[i]);
    let mut s = i;
    while s > 0 && char_class(chars[s - 1]) == target {
        s -= 1;
    }
    let mut e = i + 1;
    while e < chars.len() && char_class(chars[e]) == target {
        e += 1;
    }
    (s, e)
}

#[cfg(test)]
mod tests {
    use super::word_bounds;

    #[test]
    fn word_bounds_selects_word_run() {
        let s = "the quick_brown fox";
        // "the" の中。
        assert_eq!(word_bounds(s, 1), (0, 3));
        // "quick_brown"（アンダースコア込みで1語）。
        assert_eq!(word_bounds(s, 6), (4, 15));
        // "fox" 末尾語。
        assert_eq!(word_bounds(s, 17), (16, 19));
    }

    #[test]
    fn word_bounds_groups_spaces_and_symbols() {
        let s = "a == b";
        // 空白1つ。
        assert_eq!(word_bounds(s, 1), (1, 2));
        // "==" は記号の連なり。
        assert_eq!(word_bounds(s, 3), (2, 4));
    }

    #[test]
    fn word_bounds_handles_edges() {
        assert_eq!(word_bounds("", 0), (0, 0));
        // 範囲外 col は末尾文字へクランプ。
        assert_eq!(word_bounds("ab", 9), (0, 2));
    }
}
