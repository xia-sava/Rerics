//! 自前描画の水平タブ帯コントロール。
//!
//! ウィンドウ上部にタブを横並びで描画し、アクティブタブを強調する。
//! クリックでタブ切替コールバックを呼ぶ。ダブルバッファでちらつきを抑える。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{Colors, Config, Rgb};
use winsafe::{self as w, co, gui, prelude::*};

struct Inner {
    labels: RefCell<Vec<String>>,
    active: Cell<usize>,
    colors: Colors,
    font_family: String,
    font_size: i32,
    font_height: Cell<i32>,
    on_click: RefCell<Option<Box<dyn Fn(usize)>>>,
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
            colors: cfg.colors,
            font_family: cfg.font.family.clone(),
            font_size: cfg.font.size,
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

    /// フォントを生成する（設定のファミリ・サイズ）。
    fn create_font(&self) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
        w::HFONT::CreateFont(
            w::SIZE { cx: 0, cy: -gui::dpi_y(self.inner.font_size) },
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

    fn setup_events(&self) {
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
        if index < n {
            if let Some(cb) = self.inner.on_click.borrow().as_ref() {
                cb(index);
            }
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
        let font = self.create_font()?;
        let _font_sel = mem_dc.SelectObject(&*font)?;
        if let Ok(tm) = mem_dc.GetTextMetrics() {
            self.inner.font_height.set(tm.tmHeight);
        }
        mem_dc.SetBkMode(co::BKMODE::TRANSPARENT)?;

        self.paint_to(&mem_dc, cw, ch)?;

        hdc.BitBlt(
            w::POINT { x: 0, y: 0 },
            w::SIZE { cx: cw, cy: ch },
            &mem_dc,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    fn paint_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let colors = self.inner.colors;

        // 背景全面。
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        let labels = self.inner.labels.borrow();
        let n = labels.len();
        if n == 0 {
            return Ok(());
        }
        let active = self.inner.active.get();

        let cell_w = cw / n as i32;
        let active_bg = w::HBRUSH::CreateSolidBrush(rgb(colors.selected_file_bg))?;
        let inactive_bg = w::HBRUSH::CreateSolidBrush(rgb(colors.background2))?;
        let border = w::COLORREF::from_rgb(0x60, 0x60, 0x60);

        for (i, label) in labels.iter().enumerate() {
            let left = (i as i32) * cell_w;
            // 端数は最後のセルで吸収する。
            let right = if i == n - 1 { cw } else { left + cell_w };
            let brush = if i == active { &active_bg } else { &inactive_bg };
            dc.FillRect(w::RECT { left, top: 0, right, bottom: ch }, brush)?;

            // 区切り線。
            let pen = w::HPEN::CreatePen(co::PS::SOLID, 1, border)?;
            let _pen_sel = dc.SelectObject(&*pen)?;
            dc.MoveToEx(right - 1, 0, None)?;
            dc.LineTo(right - 1, ch)?;

            // テキスト。
            if !label.is_empty() {
                dc.SetTextColor(rgb(colors.file_normal))?;
                let flags = co::DT::SINGLELINE
                    | co::DT::VCENTER
                    | co::DT::CENTER
                    | co::DT::NOPREFIX
                    | co::DT::END_ELLIPSIS;
                let rect = w::RECT { left: left + 4, top: 0, right: right - 4, bottom: ch };
                dc.DrawText(label, rect, flags)?;
            }
        }
        Ok(())
    }
}

/// `Rgb` を COLORREF へ変換する。
fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}
