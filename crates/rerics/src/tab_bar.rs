//! 自前描画の水平タブ帯コントロール。
//!
//! ウィンドウ上部にタブを横並びで描画し、アクティブタブを強調する。
//! クリックでタブ切替コールバックを呼ぶ。ダブルバッファでちらつきを抑える。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::Config;
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;
use crate::font_fallback::FontSet;

/// タブクリック時に index を渡すコールバック。
type ClickHandler = Box<dyn Fn(usize)>;

struct Inner {
    labels: RefCell<Vec<String>>,
    active: Cell<usize>,
    font_family: RefCell<String>,
    font_fallback: RefCell<Vec<String>>,
    font_size: Cell<i32>,
    font_height: Cell<i32>,
    on_click: RefCell<Option<ClickHandler>>,
}

/// 水平タブ帯コントロール。
#[derive(Clone)]
pub struct TabBar {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl TabBar {
    /// 親に子コントロールとして生成する。イベントは生成前にここで配線する。
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
                style: co::WS::CHILD | co::WS::VISIBLE | co::WS::CLIPSIBLINGS,
                ..Default::default()
            },
        );
        let inner = Rc::new(Inner {
            labels: RefCell::new(Vec::new()),
            active: Cell::new(0),
            font_family: RefCell::new(cfg.font.family.clone()),
            font_fallback: RefCell::new(cfg.font.fallback.clone()),
            font_size: Cell::new(cfg.font.size),
            font_height: Cell::new(gui::dpi_y(cfg.font.size)),
            on_click: RefCell::new(None),
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

    /// タブのラベルとアクティブ index を更新する。
    pub fn set_tabs(&self, labels: Vec<String>, active: usize) {
        *self.inner.labels.borrow_mut() = labels;
        self.inner.active.set(active);
    }

    /// タブクリック時のコールバックを登録する。
    pub fn on_click(&self, cb: impl Fn(usize) + 'static) {
        *self.inner.on_click.borrow_mut() = Some(Box::new(cb));
    }

    /// 再描画を促す。
    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// 設定のフォントを反映して再描画する（chrome の色はシステム固定なので対象外）。
    pub fn apply_config(&self, cfg: &Config) {
        *self.inner.font_family.borrow_mut() = cfg.font.family.clone();
        *self.inner.font_fallback.borrow_mut() = cfg.font.fallback.clone();
        self.inner.font_size.set(cfg.font.size);
        let _ = self.refresh();
    }

    /// 指定ファミリ・サイズのフォントを生成する。
    fn create_font_family(
        &self,
        family: &str,
        size: i32,
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

    /// 設定のファミリ＋フォールバックのフォント一式を生成する。
    fn create_fonts(&self) -> w::SysResult<FontSet> {
        let main = self.inner.font_size.get();
        FontSet::new(&self.inner.font_family.borrow(), &self.inner.font_fallback.borrow(), |f, s| {
            self.create_font_family(f, crate::font_fallback::effective_size(s, main, main))
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
    }

    fn on_l_button_down(&self, pt: w::POINT) -> w::AnyResult<()> {
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let n = self.inner.labels.borrow().len();
        if n == 0 || cw <= 0 {
            return Ok(());
        }
        let index = ((pt.x * n as i32) / cw) as usize;
        if index < n
            && let Some(cb) = self.inner.on_click.borrow().as_ref() {
                cb(index);
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

        // タブが乗る背後（BTNFACE）。
        let band = w::HBRUSH::CreateSolidBrush(chrome::face())?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &band)?;

        let labels = self.inner.labels.borrow();
        let n = labels.len();
        if n == 0 {
            return Ok(());
        }
        let active = self.inner.active.get();
        let cell_w = cw / n as i32;

        for (i, label) in labels.iter().enumerate() {
            let left = (i as i32) * cell_w;
            // 端数は最後のセルで吸収する。
            let right = if i == n - 1 { cw } else { left + cell_w };

            // 選択タブだけ白背景＋枠（上＋左＋右、下辺は開放して直下のバーへ繋げる）。
            // 非選択タブはバー地のグレーのままフラット。
            if i == active {
                let card = w::HBRUSH::CreateSolidBrush(chrome::window())?;
                dc.FillRect(w::RECT { left, top: 0, right, bottom: ch }, &card)?;
                chrome::hline(dc, left, right, 0, sh)?;
                chrome::vline(dc, left, 0, ch, sh)?;
                chrome::vline(dc, right - 1, 0, ch, sh)?;
            }

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
        }
        Ok(())
    }
}
