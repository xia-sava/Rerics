//! テキスト/バイナリビューアの表示パネル（自前描画）。
//!
//! 別窓は作らず、メイン領域（ペイン＋ログ）に重ねて表示する 1 枚の `WindowControl`。
//! 表示モデルは `rerics_core::ViewerModel`（折返し・ダンプ整形・エンコーディングは core 側）。
//! 本モジュールは描画・スクロール・キー操作の GUI 配線に徹する。下端に状態行（ファイル名・
//! エンコーディング・モード・行位置）を自前描画する。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{
    Colors, Config, DisplayLine, LineEnding, Rgb, SearchOptions, ViewMode, ViewerModel,
    search_matches,
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
    /// 検索バーを閉じたときに呼ぶコールバック（キー入力を本体へ戻す）。MainWindow が登録する。
    on_search_close: RefCell<Option<Box<dyn Fn()>>>,
    /// マウス選択の始点・終点（None なら選択なし）。
    sel_anchor: Cell<Option<Pos>>,
    sel_cursor: Cell<Option<Pos>>,
    /// ドラッグ中か。
    selecting: Cell<bool>,
    /// 右クリック時に呼ぶコールバック（画面座標）。メニュー表示は MainWindow が担う。
    on_menu: RefCell<Option<Box<dyn Fn(w::POINT)>>>,
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
                style: co::WS::CHILD | co::WS::CLIPSIBLINGS,
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
        let search_case = mk_check("大小");
        let search_word = mk_check("単語");
        let search_regex = mk_check("正規");
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
            match_pos: Cell::new(None),
            search_active: Cell::new(false),
            saved_scroll: Cell::new(0),
            search_edit,
            search_case,
            search_word,
            search_regex,
            search_prev,
            search_next,
            on_search_close: RefCell::new(None),
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
        for h in self.search_bar_controls() {
            h.ShowWindow(co::SW::HIDE);
        }
    }

    /// 検索バーの子コントロール一式（入力欄＋トグル＋前後ボタン）。
    fn search_bar_controls(&self) -> [&w::HWND; 6] {
        [
            self.inner.search_edit.hwnd(),
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
        self.inner.search_word.hwnd().EnableWindow(!o.regex);
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
    pub fn confirm_search_bar(&self) -> w::AnyResult<()> {
        if !self.inner.search_active.get() {
            return Ok(());
        }
        self.reset_search_bar();
        self.refresh()?;
        if let Some(cb) = self.inner.on_search_close.borrow().as_ref() {
            cb();
        }
        Ok(())
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
            "whole_word" | "word" => o.whole_word = on,
            "regex" => o.regex = on,
            _ => return Ok(false),
        }
        self.inner.search_opts.set(o);
        self.inner.search_case.set_check(!o.case_sensitive);
        self.inner.search_word.set_check(o.whole_word);
        self.inner.search_regex.set_check(o.regex);
        self.inner.search_word.hwnd().EnableWindow(!o.regex);
        let term = self.inner.search_term.borrow().clone();
        self.set_search(&term)?;
        Ok(true)
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

    /// 検索バーの入力欄と右側の操作列を配置する。右端から順にカウンタ領域・前/次ボタン・
    /// トグル3つを並べ、残り幅を入力欄に割り当てる。
    fn layout_search_bar(&self) {
        if !self.inner.search_active.get() {
            return;
        }
        let cw = self.hwnd().GetClientRect().map(|r| r.right - r.left).unwrap_or(0);
        let bar_h = self.search_bar_height();
        let pad = gui::dpi_x(6);
        let gap = gui::dpi_x(4);
        let cb_w = gui::dpi_x(58);
        let btn_w = gui::dpi_x(26);
        let counter_w = gui::dpi_x(72);
        let h = self.inner.line_height.get().max(gui::dpi_y(18));
        let y = ((bar_h - h) / 2).max(0);
        let mv = |hwnd: &w::HWND, x: i32, w: i32| {
            let _ = hwnd.MoveWindow(w::POINT { x, y }, w::SIZE { cx: w, cy: h }, true);
        };
        // 操作列（トグル3＋ボタン2）の左端。カウンタ領域ぶんを右に空ける。
        let cluster_w = cb_w * 3 + btn_w * 2 + gap * 4;
        let cluster_x = (cw - pad - counter_w - gap - cluster_w).max(pad);
        let mut x = cluster_x;
        for (hwnd, w) in [
            (self.inner.search_case.hwnd(), cb_w),
            (self.inner.search_word.hwnd(), cb_w),
            (self.inner.search_regex.hwnd(), cb_w),
            (self.inner.search_prev.hwnd(), btn_w),
            (self.inner.search_next.hwnd(), btn_w),
        ] {
            mv(hwnd, x, w);
            x += w + gap;
        }
        // 入力欄は左端から操作列の手前まで。
        let ew = (cluster_x - pad - gap).max(gui::dpi_x(40));
        mv(self.inner.search_edit.hwnd(), pad, ew);
    }

    /// 検索バーの帯（背景＋下境界＋一致カウンタ）を描く。入力欄自体は子コントロールが上に乗る。
    fn draw_search_bar(&self, dc: &w::HDC, cw: i32) -> w::AnyResult<()> {
        let bar_h = self.search_bar_height();
        let face = chrome::face();
        let brush = w::HBRUSH::CreateSolidBrush(face)?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: bar_h }, &brush)?;
        chrome::hline(dc, 0, cw, bar_h - 1, chrome::highlight())?;
        // 右端に「現在 / 総数」を出す（語が空なら何も出さず、一致なしは 0 件）。
        let term = self.inner.search_term.borrow().clone();
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
            let sfont = self.create_font_sized((self.inner.font_size - 1).max(6))?;
            let _sel = dc.SelectObject(&*sfont)?;
            dc.SetTextColor(chrome::text())?;
            let pad = gui::dpi_x(8);
            let rect = w::RECT { left: cw / 2, top: 0, right: cw - pad, bottom: bar_h };
            dc.DrawText(
                &text,
                rect,
                co::DT::SINGLELINE | co::DT::VCENTER | co::DT::RIGHT | co::DT::NOPREFIX,
            )?;
        }
        Ok(())
    }

    /// 全表示行を走査して、検索語の全一致を `(行, 開始桁)` の昇順で集める。
    fn all_matches(&self) -> Vec<(usize, usize)> {
        let term = self.inner.search_term.borrow();
        if term.is_empty() {
            return Vec::new();
        }
        let lines = self.inner.lines.borrow();
        let opts = self.inner.search_opts.get();
        let mut out = Vec::new();
        for (li, line) in lines.iter().enumerate() {
            for (off, _len) in search_matches(&line.body, &term, &opts) {
                out.push((li, off));
            }
        }
        out
    }

    /// 現在一致の長さ（debug 観測・正規表現で可変長になるため）。無ければ 0。
    #[cfg(feature = "debug-server")]
    fn current_match_len(&self) -> usize {
        let Some((line, col)) = self.inner.match_pos.get() else {
            return 0;
        };
        let term = self.inner.search_term.borrow();
        let opts = self.inner.search_opts.get();
        let lines = self.inner.lines.borrow();
        let Some(dl) = lines.get(line) else { return 0 };
        search_matches(&dl.body, &term, &opts)
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
        // 「大小」ON＝大小無視（case_sensitive=false）。
        let this = self.clone();
        self.inner.search_case.on().bn_clicked(move || {
            let mut o = this.inner.search_opts.get();
            o.case_sensitive = !this.inner.search_case.is_checked();
            this.inner.search_opts.set(o);
            this.refocus_after_toggle()
        });
        let this = self.clone();
        self.inner.search_word.on().bn_clicked(move || {
            let mut o = this.inner.search_opts.get();
            o.whole_word = this.inner.search_word.is_checked();
            this.inner.search_opts.set(o);
            this.refocus_after_toggle()
        });
        // 正規表現 ON の間は単語一致を無効化（グレーアウト）する。
        let this = self.clone();
        self.inner.search_regex.on().bn_clicked(move || {
            let mut o = this.inner.search_opts.get();
            o.regex = this.inner.search_regex.is_checked();
            this.inner.search_opts.set(o);
            this.inner.search_word.hwnd().EnableWindow(!o.regex);
            this.refocus_after_toggle()
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
        let term = self.inner.search_term.borrow();
        let opts = self.inner.search_opts.get();
        let has_term = !term.is_empty();
        let sel_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.selected_file_bg))?;
        let find_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.viewer_find_bg))?;
        let mut y = top_y;
        let mut i = top;
        while i < lines.len() && y < top_y + body_h {
            let line = &lines[i];
            // マウス選択のハイライト（行内の桁範囲）。選択は検索ハイライトより優先。
            let mut highlighted = false;
            if let Some((s, e)) = sel {
                if i >= s.0 && i <= e.0 {
                    let left = if i == s.0 { self.col_x(&line.body, s.1, content_left, cwd) } else { content_left };
                    let right = if i == e.0 { self.col_x(&line.body, e.1, content_left, cwd) } else { cw };
                    if right > left {
                        dc.FillRect(w::RECT { left, top: y, right, bottom: y + lh }, &sel_brush)?;
                    }
                    highlighted = true;
                }
            }
            // 検索一致（選択していない行のみ）。行内の各一致を桁単位で塗り、現在一致は選択色で区別する。
            let spans: Vec<(usize, usize)> = if highlighted || !has_term {
                Vec::new()
            } else {
                search_matches(&line.body, &term, &opts)
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
        if is_text && i == lines.len() {
            if let Some(last) = lines.last() {
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
