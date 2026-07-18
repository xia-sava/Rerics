//! ファイラ用のインライン検索バー（自前描画・headless 観測対応）。
//!
//! タブ帯とペインの間に 1 枚だけ置く共有バー。左右どちらのペインの検索状態でも
//! 使い回すため、状態は各ペインの `rerics_core::PaneSearch` が正本で、このバーは
//! [`SearchBar::set_state`] で流し込まれた値を映すだけに徹する。入力・トグル・前後
//! 移動はコールバックで `MainWindow` の表層関数へ渡す。実機では本物の子コントロールが
//! 前面に乗るが、それらは自前描画でなくスナップショットに写らないため、headless でも
//! 入力文字・トグル状態が見えるよう帯の上へ「ミラー」を写し描きする。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{Config, PaneSearch, SearchOptions};
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;
use crate::font_fallback::FontSet;

/// 検索履歴の保持上限。
const HISTORY_CAP: usize = 32;
/// 履歴ドロップダウンに一度に見せる最大行数（超過分はスクロール）。
const HISTORY_DROPDOWN_ROWS: usize = 12;
/// 検索履歴のキー（ビューアの `"search"` とは別レーン）。
const HISTORY_KEY: &str = "filesearch";

/// トグルの種類（クリック・ニーモニック共通の切替対象）。絞り込みは Case/Word/Regex とは独立。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OptKind {
    Case,
    Word,
    Regex,
    Filter,
}

type ChangeHandler = Box<dyn Fn(&str)>;
type StepHandler = Box<dyn Fn(bool)>;
type PlainHandler = Box<dyn Fn()>;
type OptionHandler = Box<dyn Fn(OptKind, bool)>;

struct Inner {
    font_family: String,
    font_fallback: Vec<String>,
    font_size: i32,
    line_height: Cell<i32>,
    /// プログラム的な状態流し込み中はコールバックを発火させないためのガード。
    syncing: Cell<bool>,
    list_open: Cell<bool>,
    /// 帯右側に出す補助表示（絞り込み件数など・[`SearchBar::set_state`] で更新）。
    status: RefCell<String>,
    /// 検索語（ミラー描画用のキャッシュ・入力欄と同期を保つ）。
    query: RefCell<String>,
    /// トグル状態（ミラー描画用のキャッシュ）。
    opts: Cell<SearchOptions>,
    filter: Cell<bool>,
    edit: gui::Edit,
    check_case: gui::CheckBox,
    check_word: gui::CheckBox,
    check_regex: gui::CheckBox,
    check_filter: gui::CheckBox,
    prev: gui::Button,
    next: gui::Button,
    history: gui::Button,
    list: gui::ListBox,
    on_change: RefCell<Option<ChangeHandler>>,
    on_step: RefCell<Option<StepHandler>>,
    on_confirm: RefCell<Option<PlainHandler>>,
    on_cancel: RefCell<Option<PlainHandler>>,
    on_option: RefCell<Option<OptionHandler>>,
}

/// ファイラのインライン検索バー。
#[derive(Clone)]
pub struct SearchBar {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl SearchBar {
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
                // 子コントロール領域を親描画から除外し、ミラーが本物コントロールを上書き
                // してチラつかないようにする。
                style: co::WS::CHILD | co::WS::CLIPSIBLINGS | co::WS::CLIPCHILDREN,
                ..Default::default()
            },
        );
        // 入力欄。イベント（en_change・サブクラス）は親生成より前に登録が要るのでここで作る。
        let edit = gui::Edit::new(
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
        let check_case = mk_check("ケースを無視(&C)");
        let check_word = mk_check("単語境界(&W)");
        let check_regex = mk_check("正規表現(&R)");
        let check_filter = mk_check("絞り込み(&O)");
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
        let prev = mk_btn("↑");
        let next = mk_btn("↓");
        let history = mk_btn("▼");
        // 履歴ドロップダウンはバー本体（高さがバー1行分しかない）ではなく、メインウィンドウの
        // 子として持つ。バーの子のままだと Win32 の子ウィンドウクリッピングにより、バーの
        // クライアント領域からはみ出す部分が実機で一切描画されない。
        let list = gui::ListBox::new(
            parent,
            gui::ListBoxOpts {
                control_style: co::LBS::NOTIFY | co::LBS::HASSTRINGS,
                window_style: co::WS::CHILD | co::WS::BORDER | co::WS::VSCROLL,
                position: gui::dpi(0, 0),
                size: gui::dpi(160, 80),
                ..Default::default()
            },
        );
        let inner = Rc::new(Inner {
            font_family: cfg.font.family.clone(),
            font_fallback: cfg.font.fallback.clone(),
            font_size: cfg.font.size,
            line_height: Cell::new(gui::dpi_y(cfg.font.size + 2)),
            syncing: Cell::new(false),
            list_open: Cell::new(false),
            status: RefCell::new(String::new()),
            query: RefCell::new(String::new()),
            opts: Cell::new(SearchOptions::default()),
            filter: Cell::new(false),
            edit,
            check_case,
            check_word,
            check_regex,
            check_filter,
            prev,
            next,
            history,
            list,
            on_change: RefCell::new(None),
            on_step: RefCell::new(None),
            on_confirm: RefCell::new(None),
            on_cancel: RefCell::new(None),
            on_option: RefCell::new(None),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// バーの高さ（本文をこのぶん下へずらす）。
    pub fn height(&self) -> i32 {
        self.inner.line_height.get() + gui::dpi_y(12)
    }

    /// 入力・トグル状態が変わったコールバックを登録する。
    pub fn on_change(&self, cb: impl Fn(&str) + 'static) {
        *self.inner.on_change.borrow_mut() = Some(Box::new(cb));
    }
    /// 前後移動（↑↓・前後ボタン）コールバックを登録する（`forward=true` が次）。
    pub fn on_step(&self, cb: impl Fn(bool) + 'static) {
        *self.inner.on_step.borrow_mut() = Some(Box::new(cb));
    }
    /// 確定（Enter）コールバックを登録する。
    pub fn on_confirm(&self, cb: impl Fn() + 'static) {
        *self.inner.on_confirm.borrow_mut() = Some(Box::new(cb));
    }
    /// 取消（Esc）コールバックを登録する。
    pub fn on_cancel(&self, cb: impl Fn() + 'static) {
        *self.inner.on_cancel.borrow_mut() = Some(Box::new(cb));
    }
    /// トグル変更コールバックを登録する。
    pub fn on_option(&self, cb: impl Fn(OptKind, bool) + 'static) {
        *self.inner.on_option.borrow_mut() = Some(Box::new(cb));
    }

    /// 子コントロール一式（入力欄＋履歴▼＋トグル4＋前後ボタン）。
    fn controls(&self) -> [&w::HWND; 8] {
        [
            self.inner.edit.hwnd(),
            self.inner.history.hwnd(),
            self.inner.check_case.hwnd(),
            self.inner.check_word.hwnd(),
            self.inner.check_regex.hwnd(),
            self.inner.check_filter.hwnd(),
            self.inner.prev.hwnd(),
            self.inner.next.hwnd(),
        ]
    }

    /// バーとその子コントロールを表示する。
    pub fn show(&self) {
        self.hwnd().ShowWindow(co::SW::SHOW);
        for h in self.controls() {
            h.ShowWindow(co::SW::SHOW);
        }
        self.layout_children();
        let _ = self.refresh();
    }

    /// バーとその子コントロール（履歴ドロップダウン含む）を隠す。
    pub fn hide(&self) {
        self.close_dropdown(false);
        for h in self.controls() {
            h.ShowWindow(co::SW::HIDE);
        }
        self.hwnd().ShowWindow(co::SW::HIDE);
    }

    /// 入力欄へフォーカスし、`select_all` なら全選択する。
    pub fn focus_edit(&self, select_all: bool) {
        self.inner.edit.hwnd().SetFocus();
        if select_all {
            self.inner.edit.set_selection(0, -1);
        }
    }

    /// ペインの検索状態をバーへ流し込む（コールバックは発火させない）。
    pub fn set_state(&self, s: &PaneSearch) {
        self.inner.syncing.set(true);
        *self.inner.query.borrow_mut() = s.query.clone();
        self.inner.opts.set(s.opts);
        self.inner.filter.set(s.filter);
        // 内容が変わるときだけ入力欄へ書き戻す（オプション切替時にキャレットを飛ばさない）。
        if self.inner.edit.hwnd().GetWindowText().unwrap_or_default() != s.query {
            let _ = self.inner.edit.hwnd().SetWindowText(&s.query);
        }
        self.inner.check_case.set_check(!s.opts.case_sensitive);
        self.inner.check_word.set_check(s.opts.whole_word);
        self.inner.check_regex.set_check(s.opts.regex);
        self.inner.check_filter.set_check(s.filter);
        *self.inner.status.borrow_mut() = if s.filtering() {
            format!("除外 {} 件", s.hidden_count())
        } else {
            String::new()
        };
        self.inner.syncing.set(false);
        let _ = self.refresh();
    }

    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// 入力欄と右側の操作列（トグル4＋前後ボタン）を現在幅で配置する。
    pub fn layout_children(&self) {
        let cw = self.hwnd().GetClientRect().map(|r| r.right - r.left).unwrap_or(0);
        let g = self.geom(cw);
        let mv = |hwnd: &w::HWND, (x, wd): (i32, i32)| {
            let _ = hwnd.MoveWindow(w::POINT { x, y: g.y }, w::SIZE { cx: wd, cy: g.h }, true);
        };
        mv(self.inner.edit.hwnd(), g.edit);
        mv(self.inner.history.hwnd(), g.history);
        mv(self.inner.check_case.hwnd(), g.case);
        mv(self.inner.check_word.hwnd(), g.word);
        mv(self.inner.check_regex.hwnd(), g.regex);
        mv(self.inner.check_filter.hwnd(), g.filter);
        mv(self.inner.prev.hwnd(), g.prev);
        mv(self.inner.next.hwnd(), g.next);
    }

    /// 各要素の矩形（x, 幅）と共通の y・高さ。配置とミラー描画で共有する。
    fn geom(&self, cw: i32) -> BarGeom {
        let bar_h = self.height();
        let pad = gui::dpi_x(6);
        let gap = gui::dpi_x(6);
        let h = self.inner.line_height.get().max(gui::dpi_y(18));
        let y = ((bar_h - h) / 2).max(0);
        let edit_w = gui::dpi_x(200);
        let hist_w = gui::dpi_x(20);
        let case_w = gui::dpi_x(140);
        let word_w = gui::dpi_x(112);
        let regex_w = gui::dpi_x(112);
        let filter_w = gui::dpi_x(112);
        let btn_w = gui::dpi_x(26);
        let edit_x = pad;
        let hist_x = edit_x + edit_w;
        let case_x = hist_x + hist_w + gap;
        let word_x = case_x + case_w + gap;
        let regex_x = word_x + word_w + gap;
        let filter_x = regex_x + regex_w + gap;
        let prev_x = filter_x + filter_w + gap;
        let next_x = prev_x + btn_w + gap;
        let status_x = next_x + btn_w + gap;
        let status_w = (cw - status_x - pad).max(0);
        BarGeom {
            y,
            h,
            edit: (edit_x, edit_w),
            history: (hist_x, hist_w),
            case: (case_x, case_w),
            word: (word_x, word_w),
            regex: (regex_x, regex_w),
            filter: (filter_x, filter_w),
            prev: (prev_x, btn_w),
            next: (next_x, btn_w),
            status: (status_x, status_w),
        }
    }

    /// 履歴ドロップダウン（入力欄の下・ボックス幅）を開く（開いていれば閉じる＝トグル）。
    fn open_dropdown(&self) -> w::AnyResult<()> {
        if self.inner.list_open.get() {
            self.close_dropdown(true);
            return Ok(());
        }
        let items = rerics_core::InputHistory::load().get(HISTORY_KEY);
        if items.is_empty() {
            self.inner.edit.hwnd().SetFocus();
            return Ok(());
        }
        let list = self.inner.list.hwnd();
        self.inner.list.items().delete_all();
        self.inner.list.items().add(&items)?;
        let cw = self.hwnd().GetClientRect().map(|r| r.right - r.left).unwrap_or(0);
        let g = self.geom(cw);
        let (ex, ew) = g.edit;
        let width = ew + g.history.1;
        let item_h = unsafe { list.SendMessage(w::msg::lb::GetItemHeight { index: None }) }
            .map(|h| h as i32)
            .unwrap_or(gui::dpi_y(18))
            .max(1);
        let rows = items.len().min(HISTORY_DROPDOWN_ROWS) as i32;
        let height = rows * item_h + gui::dpi_y(4);
        let top = self.height();
        // リストの親はバー本体ではなくメインウィンドウなので、バー内ローカル座標 (ex, top) を
        // 実際の親のクライアント座標へ変換してから配置する。
        let bar_origin = self.hwnd().ClientToScreen(w::POINT { x: 0, y: 0 })?;
        let origin = list.GetParent()?.ScreenToClient(bar_origin)?;
        let _ = list.MoveWindow(
            w::POINT { x: origin.x + ex, y: origin.y + top },
            w::SIZE { cx: width, cy: height },
            true,
        );
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
    fn close_dropdown(&self, refocus: bool) {
        if !self.inner.list_open.get() {
            return;
        }
        self.inner.list_open.set(false);
        self.inner.list.hwnd().ShowWindow(co::SW::HIDE);
        if refocus {
            self.inner.edit.hwnd().SetFocus();
        }
    }

    /// ドロップダウンの現在選択を確定する（入力欄へ入れて検索）。
    fn pick_selection(&self) -> w::AnyResult<()> {
        let list = self.inner.list.hwnd();
        let sel = unsafe { list.SendMessage(w::msg::lb::GetCurSel {}) };
        let text = sel.and_then(|i| self.inner.list.items().text(i).ok());
        self.close_dropdown(true);
        if let Some(text) = text {
            self.set_query(&text)?;
        }
        Ok(())
    }

    /// 入力欄へ文字列を入れ（末尾キャレット）、変更コールバックへ通知する。
    fn set_query(&self, text: &str) -> w::AnyResult<()> {
        let _ = self.inner.edit.hwnd().SetWindowText(text);
        let caret = text.encode_utf16().count() as i32;
        self.inner.edit.set_selection(caret, caret);
        self.notify_change();
        Ok(())
    }

    /// 現在の入力欄の値で変更コールバックを呼ぶ。
    fn notify_change(&self) {
        if self.inner.syncing.get() {
            return;
        }
        let text = self.inner.edit.hwnd().GetWindowText().unwrap_or_default();
        *self.inner.query.borrow_mut() = text.clone();
        if let Some(cb) = self.inner.on_change.borrow().as_ref() {
            cb(&text);
        }
    }

    /// トグル `kind` を `on` にしてオプションコールバックを呼ぶ（クリック・ニーモニック共通）。
    fn notify_option(&self, kind: OptKind, on: bool) {
        if self.inner.syncing.get() {
            return;
        }
        if let Some(cb) = self.inner.on_option.borrow().as_ref() {
            cb(kind, on);
        }
    }

    /// トグルを反転してオプションコールバックを呼ぶ（ニーモニック Alt+C/W/R/O 用）。`on` は
    /// `SearchOptions` の生フィールド値（Case は `case_sensitive`）で渡す。
    fn toggle_option(&self, kind: OptKind) {
        let o = self.inner.opts.get();
        let raw = match kind {
            OptKind::Case => o.case_sensitive,
            OptKind::Word => o.whole_word,
            OptKind::Regex => o.regex,
            OptKind::Filter => self.inner.filter.get(),
        };
        self.notify_option(kind, !raw);
    }

    // ------- debug-server 用（headless は run loop が止まり合成キーが届かないため直接叩く） -------

    /// 入力欄へ文字列を入れて変更コールバックを呼ぶ。
    #[cfg(feature = "debug-server")]
    pub fn debug_set_text(&self, text: &str) -> w::AnyResult<()> {
        self.set_query(text)
    }

    /// トグルのニーモニック（Alt+C/W/R/O 相当）を駆動する。未知キーは `false`。
    #[cfg(feature = "debug-server")]
    pub fn debug_mnemonic(&self, key: char) -> bool {
        let kind = match key.to_ascii_lowercase() {
            'c' => OptKind::Case,
            'w' => OptKind::Word,
            'r' => OptKind::Regex,
            'o' => OptKind::Filter,
            _ => return false,
        };
        self.toggle_option(kind);
        true
    }

    /// 履歴ドロップダウンを開く/閉じる。
    #[cfg(feature = "debug-server")]
    pub fn debug_dropdown(&self, open: bool) -> w::AnyResult<()> {
        if open {
            if !self.inner.list_open.get() {
                self.open_dropdown()?;
            }
        } else {
            self.close_dropdown(true);
        }
        Ok(())
    }

    /// 履歴ドロップダウンが開いているか。
    #[cfg(feature = "debug-server")]
    pub fn debug_is_dropdown_open(&self) -> bool {
        self.inner.list_open.get()
    }

    /// ドロップダウンが開いていれば、実際の親（メインウィンドウ）のクライアント領域に矩形が
    /// 収まっているか（Win32 の子ウィンドウクリッピングで見切れていないか）を返す。閉じていれば
    /// `None`。
    #[cfg(feature = "debug-server")]
    pub fn debug_dropdown_visible(&self) -> Option<bool> {
        if !self.inner.list_open.get() {
            return None;
        }
        let list = self.inner.list.hwnd();
        let parent = list.GetParent().ok()?;
        let list_rect = list.GetWindowRect().ok()?;
        let origin = parent.ClientToScreen(w::POINT { x: 0, y: 0 }).ok()?;
        let client = parent.GetClientRect().ok()?;
        let local_left = list_rect.left - origin.x;
        let local_top = list_rect.top - origin.y;
        let local_right = list_rect.right - origin.x;
        let local_bottom = list_rect.bottom - origin.y;
        Some(
            local_left >= 0
                && local_top >= 0
                && local_right <= client.right
                && local_bottom <= client.bottom,
        )
    }

    /// 履歴の index 番目（新しい順）を入力欄へ入れて検索する。範囲外は `false`。
    #[cfg(feature = "debug-server")]
    pub fn debug_select_history(&self, index: usize) -> w::AnyResult<bool> {
        let items = rerics_core::InputHistory::load().get(HISTORY_KEY);
        match items.get(index) {
            Some(it) => {
                self.set_query(it)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// 検索語を検索履歴へ記録する（空は無視）。確定時に呼ぶ。
    pub fn record_history(&self, term: &str) {
        if term.trim().is_empty() {
            return;
        }
        let mut hist = rerics_core::InputHistory::load();
        hist.add_capped(HISTORY_KEY, term, HISTORY_CAP);
        let _ = hist.save();
    }

    fn create_font_family(
        &self,
        size: i32,
        family: &str,
    ) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
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
            family,
        )
    }

    fn create_fonts_sized(&self, size: i32) -> w::SysResult<FontSet> {
        let main = self.inner.font_size;
        FontSet::new(&self.inner.font_family, &self.inner.font_fallback, |family, s| {
            self.create_font_family(crate::font_fallback::effective_size(s, size, main), family)
        })
    }

    fn setup_events(&self) {
        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm_size(move |_| {
            this.layout_children();
            Ok(())
        });

        // 入力中はリアルタイム検索（インクリメンタル）。
        let this = self.clone();
        self.inner.edit.on().en_change(move || {
            this.notify_change();
            Ok(())
        });

        // 入力欄内のキー：↑↓ で前後の一致・Enter 確定・Esc 取消。それ以外は既定処理へ。
        let this = self.clone();
        self.inner.edit.on_subclass().wm(co::WM::KEYDOWN, move |p| {
            let handled = match p.wparam as u16 {
                0x26 => {
                    this.emit_step(false);
                    true
                } // VK_UP
                0x28 => {
                    this.emit_step(true);
                    true
                } // VK_DOWN
                0x0D => {
                    this.emit_confirm();
                    true
                } // VK_RETURN
                0x1B => {
                    this.emit_cancel();
                    true
                } // VK_ESCAPE
                _ => false,
            };
            if handled {
                Ok(0)
            } else {
                Ok(unsafe { this.inner.edit.hwnd().DefSubclassProc(p) })
            }
        });
        // Enter/Esc の WM_CHAR を飲んでビープを抑える。
        let this = self.clone();
        self.inner.edit.on_subclass().wm(co::WM::CHAR, move |p| {
            if matches!(p.wparam as u16, 0x0D | 0x1B) {
                Ok(0)
            } else {
                Ok(unsafe { this.inner.edit.hwnd().DefSubclassProc(p) })
            }
        });

        // 入力欄での Alt 併用：Alt+↑↓ で履歴ドロップダウン、Alt+C/W/R/O でトグル。
        let this = self.clone();
        self.inner.edit.on_subclass().wm(co::WM::SYSKEYDOWN, move |p| {
            let vk = p.wparam as u16;
            match vk {
                0x26 | 0x28 => {
                    let _ = this.open_dropdown();
                    Ok(0)
                }
                0x43 => {
                    this.toggle_option(OptKind::Case);
                    Ok(0)
                }
                0x57 => {
                    this.toggle_option(OptKind::Word);
                    Ok(0)
                }
                0x52 => {
                    this.toggle_option(OptKind::Regex);
                    Ok(0)
                }
                0x4F => {
                    this.toggle_option(OptKind::Filter);
                    Ok(0)
                }
                _ => Ok(unsafe { this.inner.edit.hwnd().DefSubclassProc(p) }),
            }
        });
        // Alt+C/W/R/O に伴う WM_SYSCHAR を食い、メニューバーのニーモニックへ貫通させない。
        let this = self.clone();
        self.inner.edit.on_subclass().wm(co::WM::SYSCHAR, move |p| {
            if matches!((p.wparam as u8).to_ascii_lowercase(), b'c' | b'w' | b'r' | b'o') {
                Ok(0)
            } else {
                Ok(unsafe { this.inner.edit.hwnd().DefSubclassProc(p) })
            }
        });

        // トグル（マウス）。クリックで反映→入力欄へフォーカスを戻す。「ケースを無視」は
        // ON＝大小無視なので、生フィールド `case_sensitive` はチェックの反転で渡す。
        let this = self.clone();
        self.inner.check_case.on().bn_clicked(move || {
            this.notify_option(OptKind::Case, !this.inner.check_case.is_checked());
            this.inner.edit.hwnd().SetFocus();
            Ok(())
        });
        let this = self.clone();
        self.inner.check_word.on().bn_clicked(move || {
            this.notify_option(OptKind::Word, this.inner.check_word.is_checked());
            this.inner.edit.hwnd().SetFocus();
            Ok(())
        });
        let this = self.clone();
        self.inner.check_regex.on().bn_clicked(move || {
            this.notify_option(OptKind::Regex, this.inner.check_regex.is_checked());
            this.inner.edit.hwnd().SetFocus();
            Ok(())
        });
        let this = self.clone();
        self.inner.check_filter.on().bn_clicked(move || {
            this.notify_option(OptKind::Filter, this.inner.check_filter.is_checked());
            this.inner.edit.hwnd().SetFocus();
            Ok(())
        });

        // 前/次ボタン（入力欄内の ↑↓ キーと同機能）。
        let this = self.clone();
        self.inner.prev.on().bn_clicked(move || {
            this.emit_step(false);
            this.inner.edit.hwnd().SetFocus();
            Ok(())
        });
        let this = self.clone();
        self.inner.next.on().bn_clicked(move || {
            this.emit_step(true);
            this.inner.edit.hwnd().SetFocus();
            Ok(())
        });
        // ▼：検索履歴のドロップダウン。
        let this = self.clone();
        self.inner.history.on().bn_clicked(move || {
            this.open_dropdown()?;
            Ok(())
        });

        // 履歴リスト内のキー：Enter 確定・Esc 取消（↑↓ はネイティブ選択移動に任せる）。
        let this = self.clone();
        self.inner.list.on_subclass().wm(co::WM::KEYDOWN, move |p| match p.wparam as u16 {
            0x0D => {
                this.pick_selection()?;
                Ok(0)
            }
            0x1B => {
                this.close_dropdown(true);
                Ok(0)
            }
            _ => Ok(unsafe { this.inner.list.hwnd().DefSubclassProc(p) }),
        });
        let this = self.clone();
        self.inner.list.on_subclass().wm(co::WM::LBUTTONUP, move |p| {
            let r = unsafe { this.inner.list.hwnd().DefSubclassProc(p) };
            this.pick_selection()?;
            Ok(r)
        });
        let this = self.clone();
        self.inner.list.on_subclass().wm(co::WM::KILLFOCUS, move |p| {
            this.close_dropdown(false);
            Ok(unsafe { this.inner.list.hwnd().DefSubclassProc(p) })
        });
    }

    fn emit_step(&self, forward: bool) {
        if let Some(cb) = self.inner.on_step.borrow().as_ref() {
            cb(forward);
        }
    }
    fn emit_confirm(&self) {
        if let Some(cb) = self.inner.on_confirm.borrow().as_ref() {
            cb();
        }
    }
    fn emit_cancel(&self) {
        if let Some(cb) = self.inner.on_cancel.borrow().as_ref() {
            cb();
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

    /// ターゲットビットマップ選択済みの任意 DC へ全面描画する（親のスナップショット合成にも使う）。
    pub(crate) fn render_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let fonts = self.create_fonts_sized(self.inner.font_size)?;
        let _font_sel = dc.SelectObject(fonts.primary())?;
        if let Ok(tm) = dc.GetTextMetrics() {
            self.inner.line_height.set(tm.tmHeight + gui::dpi_y(2));
        }
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        self.paint_to(dc, cw, ch)
    }

    fn paint_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let brush = w::HBRUSH::CreateSolidBrush(chrome::face())?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &brush)?;
        chrome::hline(dc, 0, cw, ch - 1, chrome::shadow())?;

        let g = self.geom(cw);
        let o = self.inner.opts.get();
        let filter = self.inner.filter.get();
        let term = self.inner.query.borrow().clone();
        self.draw_input(dc, g.edit, g.y, g.h, &term)?;
        let sfonts = self.create_fonts_sized((self.inner.font_size - 1).max(6))?;
        let _sel = dc.SelectObject(sfonts.primary())?;
        dc.SetTextColor(chrome::text())?;
        self.draw_button(dc, g.history, g.y, g.h, "▼")?;
        self.draw_toggle(dc, g.case, g.y, g.h, "ケースを無視", !o.case_sensitive)?;
        self.draw_toggle(dc, g.word, g.y, g.h, "単語境界", o.whole_word)?;
        self.draw_toggle(dc, g.regex, g.y, g.h, "正規表現", o.regex)?;
        self.draw_toggle(dc, g.filter, g.y, g.h, "絞り込み", filter)?;
        self.draw_button(dc, g.prev, g.y, g.h, "↑")?;
        self.draw_button(dc, g.next, g.y, g.h, "↓")?;
        let status = self.inner.status.borrow().clone();
        if !status.is_empty() {
            let (sx, sw) = g.status;
            let rect = w::RECT { left: sx, top: 0, right: sx + sw, bottom: ch };
            dc.DrawText(
                &status,
                rect,
                co::DT::SINGLELINE | co::DT::VCENTER | co::DT::LEFT | co::DT::NOPREFIX,
            )?;
        }
        Ok(())
    }

    fn draw_input(&self, dc: &w::HDC, (x, wd): (i32, i32), y: i32, h: i32, term: &str) -> w::AnyResult<()> {
        let r = w::RECT { left: x, top: y, right: x + wd, bottom: y + h };
        let white = w::HBRUSH::CreateSolidBrush(chrome::window())?;
        dc.FillRect(r, &white)?;
        chrome::hline(dc, x, x + wd, y, chrome::shadow())?;
        chrome::hline(dc, x, x + wd, y + h - 1, chrome::shadow())?;
        chrome::vline(dc, x, y, y + h, chrome::shadow())?;
        chrome::vline(dc, x + wd - 1, y, y + h, chrome::shadow())?;
        if !term.is_empty() {
            let efonts = self.create_fonts_sized((self.inner.font_size - 1).max(6))?;
            let _sel = dc.SelectObject(efonts.primary())?;
            dc.SetTextColor(chrome::text())?;
            let pad = gui::dpi_x(4);
            let tr = w::RECT { left: x + pad, top: y, right: x + wd - pad, bottom: y + h };
            efonts.draw_text(
                dc,
                term,
                tr,
                co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX | co::DT::END_ELLIPSIS,
            )?;
        }
        Ok(())
    }

    fn draw_toggle(&self, dc: &w::HDC, (x, wd): (i32, i32), y: i32, h: i32, label: &str, on: bool) -> w::AnyResult<()> {
        if on {
            chrome::hline(dc, x, x + wd, y, chrome::shadow())?;
            chrome::hline(dc, x, x + wd, y + h - 1, chrome::shadow())?;
            chrome::vline(dc, x, y, y + h, chrome::shadow())?;
            chrome::vline(dc, x + wd - 1, y, y + h, chrome::shadow())?;
        }
        let text = format!("[{}]{}", if on { "x" } else { " " }, label);
        let r = w::RECT { left: x + gui::dpi_x(2), top: y, right: x + wd, bottom: y + h };
        dc.DrawText(&text, r, co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX)?;
        Ok(())
    }

    fn draw_button(&self, dc: &w::HDC, (x, wd): (i32, i32), y: i32, h: i32, glyph: &str) -> w::AnyResult<()> {
        chrome::hline(dc, x, x + wd, y, chrome::shadow())?;
        chrome::hline(dc, x, x + wd, y + h - 1, chrome::shadow())?;
        chrome::vline(dc, x, y, y + h, chrome::shadow())?;
        chrome::vline(dc, x + wd - 1, y, y + h, chrome::shadow())?;
        let r = w::RECT { left: x, top: y, right: x + wd, bottom: y + h };
        dc.DrawText(glyph, r, co::DT::SINGLELINE | co::DT::VCENTER | co::DT::CENTER | co::DT::NOPREFIX)?;
        Ok(())
    }
}

/// 各要素の配置（x, 幅）と共通の y・高さ。配置とミラー描画で共有する。
struct BarGeom {
    y: i32,
    h: i32,
    edit: (i32, i32),
    history: (i32, i32),
    case: (i32, i32),
    word: (i32, i32),
    regex: (i32, i32),
    filter: (i32, i32),
    prev: (i32, i32),
    next: (i32, i32),
    status: (i32, i32),
}
