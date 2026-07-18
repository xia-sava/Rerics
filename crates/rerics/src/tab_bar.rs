//! 自前描画の水平タブ帯コントロール。
//!
//! ウィンドウ上部にタブを横並びで描画し、アクティブタブを強調する。
//! クリックでタブ切替コールバックを呼ぶ。ダブルバッファでちらつきを抑える。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;
use crate::font_fallback::FontSet;

/// タブクリック時に index を渡すコールバック。
type ClickHandler = Box<dyn Fn(usize)>;
/// タブ右クリック時に画面座標と index を渡すコールバック（コンテキストメニュー用）。
type MenuHandler = Box<dyn Fn(w::POINT, usize)>;

/// タブ1枚の幅の下限・上限（バー幅に対する比率）。少数のタブは上限で頭打ちにして
/// 画面幅いっぱいには広げず、多数のタブは下限まで詰めてから溢れをスクロールへ回す。
const TAB_W_MIN_RATIO: f64 = 0.10;
const TAB_W_MAX_RATIO: f64 = 0.18;

/// スクロール矢印の幅（溢れたときだけ両端に出す）。
fn arrow_w() -> i32 {
    gui::dpi_x(18)
}

/// hover 中のタブに出す閉じるボタン（×）の一辺。
fn close_w() -> i32 {
    gui::dpi_x(16)
}

struct Inner {
    labels: RefCell<Vec<String>>,
    active: Cell<usize>,
    font_height: Cell<i32>,
    /// 溢れたときにスクロールで表示している先頭タブの index。
    scroll_start: Cell<usize>,
    /// アクティブタブが変わった直後で、次の `layout` 計算でそれを表示範囲へ寄せる必要があるか。
    /// これが立っていない間は、矢印クリックによる手動スクロールをそのまま維持する
    /// （常に寄せ直すと、毎描画でアクティブタブへ巻き戻って手動スクロールが効かなくなる）。
    scroll_to_active: Cell<bool>,
    /// マウスカーソルが乗っているタブの index（× ボタンの表示に使う）。
    hover_index: Cell<Option<usize>>,
    /// `WM_MOUSELEAVE` を受け取るための追跡を貼ってあるか。
    mouse_tracking: Cell<bool>,
    on_click: RefCell<Option<ClickHandler>>,
    on_close: RefCell<Option<ClickHandler>>,
    on_menu: RefCell<Option<MenuHandler>>,
}

/// 描画・当たり判定で共有するタブ配置の計算結果。
struct TabLayout {
    tab_w: i32,
    /// 全タブがバー幅に収まらず、矢印での表示範囲スクロールが要るか。
    overflow: bool,
    /// 一度に表示できるタブ枚数。
    visible_count: usize,
    /// スクロール後の先頭表示 index。
    scroll_start: usize,
    /// タブ列の描画開始 x（`overflow` なら左矢印の幅ぶん空ける）。
    content_left: i32,
}

/// 水平タブ帯コントロール。
#[derive(Clone)]
pub struct TabBar {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl TabBar {
    /// 親に子コントロールとして生成する。イベントは生成前にここで配線する。
    pub fn new(parent: &(impl GuiParent + 'static), position: (i32, i32), size: (i32, i32)) -> Self {
        let wnd = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                class_bg_brush: gui::Brush::None,
                position,
                size,
                style: co::WS::CHILD | co::WS::VISIBLE | co::WS::CLIPSIBLINGS,
                ..Default::default()
            },
        );
        let inner = Rc::new(Inner {
            labels: RefCell::new(Vec::new()),
            active: Cell::new(0),
            font_height: Cell::new(0),
            scroll_start: Cell::new(0),
            scroll_to_active: Cell::new(false),
            hover_index: Cell::new(None),
            mouse_tracking: Cell::new(false),
            on_click: RefCell::new(None),
            on_close: RefCell::new(None),
            on_menu: RefCell::new(None),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// 現在のタブラベル一覧（デバッグ制御サーバの状態取得用）。
    #[cfg(feature = "debug-server")]
    pub fn labels(&self) -> Vec<String> {
        self.inner.labels.borrow().clone()
    }

    /// タブのラベルとアクティブ index を更新する。アクティブが変わったときだけ、次の描画で
    /// それが見えるようスクロールを寄せる（変わっていなければユーザーの手動スクロールを保つ）。
    pub fn set_tabs(&self, labels: Vec<String>, active: usize) {
        if self.inner.active.get() != active {
            self.inner.scroll_to_active.set(true);
        }
        *self.inner.labels.borrow_mut() = labels;
        self.inner.active.set(active);
    }

    /// タブクリック時のコールバックを登録する。
    pub fn on_click(&self, cb: impl Fn(usize) + 'static) {
        *self.inner.on_click.borrow_mut() = Some(Box::new(cb));
    }

    /// タブの × クリック（マウスのみ）で閉じるコールバックを登録する。
    pub fn on_close(&self, cb: impl Fn(usize) + 'static) {
        *self.inner.on_close.borrow_mut() = Some(Box::new(cb));
    }

    /// タブ右クリックのコンテキストメニュー要求コールバックを登録する
    /// （画面座標・対象 index を渡す。メニューの構築・実行は呼び出し側が行う）。
    pub fn on_menu(&self, cb: impl Fn(w::POINT, usize) + 'static) {
        *self.inner.on_menu.borrow_mut() = Some(Box::new(cb));
    }

    /// 再描画を促す。
    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// システムUIフォント（chrome 共通）1本だけの `FontSet` を生成する。
    fn create_fonts(&self) -> w::SysResult<FontSet> {
        FontSet::new("", &[], |_, _| chrome::ui_font())
    }

    /// タブ幅・スクロール矢印の要否・表示範囲を計算する。`scroll_to_active` が立っている
    /// （＝直前に `set_tabs` でアクティブが変わった）ときだけ、それが見えるようスクロール
    /// 位置を寄せる。呼ぶたびに `scroll_start` を更新する。
    fn layout(&self, cw: i32) -> TabLayout {
        let n = self.inner.labels.borrow().len();
        if n == 0 || cw <= 0 {
            return TabLayout { tab_w: 0, overflow: false, visible_count: 0, scroll_start: 0, content_left: 0 };
        }
        let min_w = ((cw as f64 * TAB_W_MIN_RATIO) as i32).max(1);
        let max_w = ((cw as f64 * TAB_W_MAX_RATIO) as i32).max(min_w);
        let tab_w = (cw / n as i32).clamp(min_w, max_w);
        let overflow = tab_w * n as i32 > cw;
        let aw = arrow_w();
        let content_w = if overflow { (cw - aw * 2).max(0) } else { cw };
        let visible_count = if overflow { (content_w / tab_w).max(1) as usize } else { n };

        let active = self.inner.active.get();
        let mut scroll_start = self.inner.scroll_start.get().min(n.saturating_sub(1));
        if overflow {
            if self.inner.scroll_to_active.get() {
                if active < scroll_start {
                    scroll_start = active;
                } else if active >= scroll_start + visible_count {
                    scroll_start = active + 1 - visible_count;
                }
            }
            scroll_start = scroll_start.min(n.saturating_sub(visible_count));
        } else {
            scroll_start = 0;
        }
        self.inner.scroll_start.set(scroll_start);
        self.inner.scroll_to_active.set(false);

        let content_left = if overflow { aw } else { 0 };
        TabLayout { tab_w, overflow, visible_count, scroll_start, content_left }
    }

    /// 座標 `x` が乗っているタブの絶対 index（矢印の帯・タブが無い余白は `None`）。
    fn tab_index_at(&self, layout: &TabLayout, x: i32, cw: i32) -> Option<usize> {
        if layout.tab_w <= 0 {
            return None;
        }
        if layout.overflow && (x < layout.content_left || x >= cw - arrow_w()) {
            return None;
        }
        let slot = ((x - layout.content_left) / layout.tab_w) as usize;
        if slot >= layout.visible_count {
            return None;
        }
        let index = layout.scroll_start + slot;
        if index < self.inner.labels.borrow().len() { Some(index) } else { None }
    }

    /// タブ `index` の閉じるボタン（×）の矩形。タブが表示範囲外、または幅が狭すぎて
    /// 置けないなら `None`。
    fn close_box_rect(&self, layout: &TabLayout, index: usize, ch: i32) -> Option<w::RECT> {
        if index < layout.scroll_start {
            return None;
        }
        let slot = index - layout.scroll_start;
        if slot >= layout.visible_count {
            return None;
        }
        let left = layout.content_left + slot as i32 * layout.tab_w;
        let right = left + layout.tab_w;
        let size = close_w();
        let h_margin = gui::dpi_x(4);
        if right - left < size + h_margin * 2 + gui::dpi_x(12) {
            return None;
        }
        let v_margin = (ch - size) / 2;
        Some(w::RECT {
            left: right - h_margin - size,
            top: v_margin,
            right: right - h_margin,
            bottom: v_margin + size,
        })
    }

    fn setup_events(&self) {
        crate::winutil::passive_focus(&self.wnd);

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm_l_button_down(move |p| {
            this.on_l_button_down(p.coords)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_r_button_down(move |p| {
            this.on_r_button_down(p.coords)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_move(move |p| {
            this.on_mouse_move(p.coords)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_leave(move || {
            this.on_mouse_leave()?;
            Ok(())
        });
    }

    fn on_l_button_down(&self, pt: w::POINT) -> w::AnyResult<()> {
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        let n = self.inner.labels.borrow().len();
        if n == 0 || cw <= 0 {
            return Ok(());
        }
        let layout = self.layout(cw);
        let aw = arrow_w();

        // 溢れ時の左右矢印：クリックで1枚分スクロールする（タブ切替は起こさない）。
        if layout.overflow {
            if pt.x < aw {
                let start = layout.scroll_start.saturating_sub(1);
                self.inner.scroll_start.set(start);
                let _ = self.refresh();
                return Ok(());
            }
            if pt.x >= cw - aw {
                let max_start = n.saturating_sub(layout.visible_count);
                let start = (layout.scroll_start + 1).min(max_start);
                self.inner.scroll_start.set(start);
                let _ = self.refresh();
                return Ok(());
            }
        }

        let Some(index) = self.tab_index_at(&layout, pt.x, cw) else {
            return Ok(());
        };

        // hover 中（＝× が見えている）タブの × 内なら閉じる。それ以外はタブ切替。
        if self.inner.hover_index.get() == Some(index)
            && let Some(r) = self.close_box_rect(&layout, index, ch)
            && (r.left..r.right).contains(&pt.x)
            && (r.top..r.bottom).contains(&pt.y)
        {
            if let Some(cb) = self.inner.on_close.borrow().as_ref() {
                cb(index);
            }
            return Ok(());
        }

        if let Some(cb) = self.inner.on_click.borrow().as_ref() {
            cb(index);
        }
        Ok(())
    }

    fn on_r_button_down(&self, pt: w::POINT) -> w::AnyResult<()> {
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        if self.inner.labels.borrow().is_empty() || cw <= 0 {
            return Ok(());
        }
        let layout = self.layout(cw);
        let Some(index) = self.tab_index_at(&layout, pt.x, cw) else {
            return Ok(());
        };
        if let Some(cb) = self.inner.on_menu.borrow().as_ref() {
            let screen = self.hwnd().ClientToScreen(pt).unwrap_or(pt);
            cb(screen, index);
        }
        Ok(())
    }

    fn on_mouse_move(&self, pt: w::POINT) -> w::AnyResult<()> {
        if !self.inner.mouse_tracking.get() {
            let mut tme = w::TRACKMOUSEEVENT::default();
            tme.dwFlags = co::TME::LEAVE;
            tme.hwndTrack = unsafe { self.hwnd().raw_copy() };
            if w::TrackMouseEvent(&mut tme).is_ok() {
                self.inner.mouse_tracking.set(true);
            }
        }
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let hover = if self.inner.labels.borrow().is_empty() || cw <= 0 {
            None
        } else {
            let layout = self.layout(cw);
            self.tab_index_at(&layout, pt.x, cw)
        };
        if self.inner.hover_index.get() != hover {
            self.inner.hover_index.set(hover);
            let _ = self.refresh();
        }
        Ok(())
    }

    fn on_mouse_leave(&self) -> w::AnyResult<()> {
        self.inner.mouse_tracking.set(false);
        if self.inner.hover_index.take().is_some() {
            let _ = self.refresh();
        }
        Ok(())
    }

    fn on_paint(&self) -> w::AnyResult<()> {
        let hdc = self.hwnd().BeginPaint()?;
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        // ダブルバッファ。
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
        let fonts = self.create_fonts()?;
        let _font_sel = dc.SelectObject(fonts.primary())?;
        if let Ok(tm) = dc.GetTextMetrics() {
            self.inner.font_height.set(tm.tmHeight);
        }
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        self.paint_to(dc, &fonts, cw, ch)
    }

    fn paint_to(&self, dc: &w::HDC, fonts: &FontSet, cw: i32, ch: i32) -> w::AnyResult<()> {
        let sh = chrome::shadow();

        // タブが乗る背後（BTNFACE）。タブは固定幅で画面幅いっぱいには広げないので、
        // 埋まらない残りはこの地色のまま見える。
        let band = w::HBRUSH::CreateSolidBrush(chrome::face())?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &band)?;

        let labels = self.inner.labels.borrow();
        let n = labels.len();
        if n == 0 {
            return Ok(());
        }
        let active = self.inner.active.get();
        let hover = self.inner.hover_index.get();
        let layout = self.layout(cw);
        let aw = arrow_w();

        if layout.overflow {
            let left_rect = w::RECT { left: 0, top: 0, right: aw, bottom: ch };
            self.draw_arrow(dc, fonts, left_rect, '◀', layout.scroll_start > 0)?;
            let can_scroll_right = layout.scroll_start + layout.visible_count < n;
            let right_rect = w::RECT { left: cw - aw, top: 0, right: cw, bottom: ch };
            self.draw_arrow(dc, fonts, right_rect, '▶', can_scroll_right)?;
        }

        let visible_end = (layout.scroll_start + layout.visible_count).min(n);
        for (slot, i) in (layout.scroll_start..visible_end).enumerate() {
            let label = &labels[i];
            let left = layout.content_left + slot as i32 * layout.tab_w;
            let right = left + layout.tab_w;

            // 選択タブだけ白背景＋枠（上＋左＋右、下辺は開放して直下のバーへ繋げる）。
            // 非選択タブはバー地のグレーのままフラット。
            if i == active {
                let card = w::HBRUSH::CreateSolidBrush(chrome::window())?;
                dc.FillRect(w::RECT { left, top: 0, right, bottom: ch }, &card)?;
                chrome::hline(dc, left, right, 0, sh)?;
                chrome::vline(dc, left, 0, ch, sh)?;
                chrome::vline(dc, right - 1, 0, ch, sh)?;
            }

            // パス文字列は hover の有無に関わらず常に同じ幅で切り詰める（× のために幅を
            // 詰めると省略位置がずれ、同じタブでも hover 前後で表示内容が変わってしまう）。
            // × は文字列の上に記号として重ね描きする（下地を塗ってから太めに描く）。
            if !label.is_empty() {
                let text_col = if i == active { chrome::text() } else { chrome::gray_text() };
                dc.SetTextColor(text_col)?;
                let flags = co::DT::SINGLELINE
                    | co::DT::VCENTER
                    | co::DT::LEFT
                    | co::DT::NOPREFIX
                    | co::DT::PATH_ELLIPSIS;
                let rect = w::RECT { left: left + 6, top: 0, right: right - 6, bottom: ch };
                fonts.draw_text(dc, label, rect, flags)?;
            }

            // hover 中のタブだけ右端に × を重ねる（マウス操作専用の閉じるボタン）。
            if hover == Some(i)
                && let Some(r) = self.close_box_rect(&layout, i, ch)
            {
                let base = if i == active { chrome::window() } else { chrome::face() };
                let badge = w::HBRUSH::CreateSolidBrush(base)?;
                dc.FillRect(r, &badge)?;
                dc.SetTextColor(chrome::text())?;
                let flags = co::DT::SINGLELINE | co::DT::VCENTER | co::DT::CENTER | co::DT::NOPREFIX;
                // 疑似ボールド：1px ずらして重ね描きし、通常の記号より太く見せる。
                fonts.draw_text(dc, "×", r, flags)?;
                let shifted = w::RECT { left: r.left + 1, top: r.top, right: r.right + 1, bottom: r.bottom };
                fonts.draw_text(dc, "×", shifted, flags)?;
            }
        }
        Ok(())
    }

    /// スクロール矢印を1つ描く。`enabled=false`（その方向にこれ以上スクロールできない）
    /// なら控えめな色にする。
    fn draw_arrow(
        &self,
        dc: &w::HDC,
        fonts: &FontSet,
        rect: w::RECT,
        glyph: char,
        enabled: bool,
    ) -> w::AnyResult<()> {
        dc.SetTextColor(if enabled { chrome::text() } else { chrome::gray_text() })?;
        let flags = co::DT::SINGLELINE | co::DT::VCENTER | co::DT::CENTER | co::DT::NOPREFIX;
        fonts.draw_text(dc, &glyph.to_string(), rect, flags)
    }
}
