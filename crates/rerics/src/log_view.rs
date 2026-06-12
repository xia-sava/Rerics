//! 下部ログウィンドウ。`LogState` を自前描画し、レベル別色で行表示する。
//!
//! `TabBar` と同様の GDI ダブルバッファ描画。新着行が来たら末尾へ自動追従し、
//! ホイールで過去ログを遡れる。キーフォーカスは持たない（閲覧専用）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{Colors, LogLevel, LogState, Rgb};
use winsafe::{self as w, co, gui, prelude::*};

struct Inner {
    state: RefCell<LogState>,
    colors: Colors,
    /// 1行の高さ（描画時にフォントメトリクスから更新）。
    line_height: Cell<i32>,
}

/// 下部ログウィンドウコントロール。
#[derive(Clone)]
pub struct LogView {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl LogView {
    /// 親に子コントロールとして生成する。
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
            state: RefCell::new(LogState::new()),
            colors: Colors::default(),
            line_height: Cell::new(gui::dpi_y(15)),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// 情報レベルで追記する。
    pub fn info(&self, text: &str) {
        self.push(LogLevel::Info, text);
    }

    /// 警告レベルで追記する。
    pub fn warn(&self, text: &str) {
        self.push(LogLevel::Warning, text);
    }

    /// エラーレベルで追記する。
    pub fn error(&self, text: &str) {
        self.push(LogLevel::Error, text);
    }

    /// レベル付きで追記し、末尾へ追従して再描画する。
    fn push(&self, level: LogLevel, text: &str) {
        let pr = self.page_rows();
        {
            let mut s = self.inner.state.borrow_mut();
            s.push(level, text);
            s.scroll_to_bottom(pr);
        }
        let _ = self.refresh();
    }

    /// 1画面に収まる行数。
    fn page_rows(&self) -> usize {
        let lh = self.inner.line_height.get().max(1);
        let h = self
            .hwnd()
            .GetClientRect()
            .map(|r| r.bottom - r.top)
            .unwrap_or(0);
        ((h / lh).max(1)) as usize
    }

    /// ホイール回転分だけスクロールする（1ノッチ 3行）。
    pub fn scroll_by_wheel(&self, distance: i16) -> w::AnyResult<()> {
        let pr = self.page_rows();
        let notches = distance as i32 / 120;
        let delta = (-notches * 3) as isize;
        {
            let mut s = self.inner.state.borrow_mut();
            let cur = s.scroll_top as isize;
            s.set_scroll_top(cur + delta, pr);
        }
        self.refresh()
    }

    /// 再描画を促す。
    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// フォントを生成する（日本語対応モノスペース）。
    fn create_font(&self) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
        w::HFONT::CreateFont(
            w::SIZE { cx: 0, cy: -gui::dpi_y(12) },
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
            "BIZ UDGothic",
        )
    }

    fn setup_events(&self) {
        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm_mouse_wheel(move |p| {
            let dist = p.keys.raw() as i16;
            this.scroll_by_wheel(dist)?;
            Ok(())
        });
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
        let font = self.create_font()?;
        let _font_sel = mem_dc.SelectObject(&*font)?;
        if let Ok(tm) = mem_dc.GetTextMetrics() {
            self.inner.line_height.set(tm.tmHeight + gui::dpi_y(1));
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

        // 背景。
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.log_background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        // 上端の区切り線（ペインとの境界）。
        let border = w::COLORREF::from_rgb(0x40, 0x40, 0x40);
        let pen = w::HPEN::CreatePen(co::PS::SOLID, 1, border)?;
        let _pen_sel = dc.SelectObject(&*pen)?;
        dc.MoveToEx(0, 0, None)?;
        dc.LineTo(cw, 0)?;

        let lh = self.inner.line_height.get().max(1);
        let s = self.inner.state.borrow();
        let count = s.count();
        let mut i = s.scroll_top;
        let mut y = 1;
        while i < count && y < ch {
            let line = &s.lines[i];
            if !line.text.is_empty() {
                let color = match line.level {
                    LogLevel::Info => colors.log_info,
                    LogLevel::Warning => colors.log_warning,
                    LogLevel::Error => colors.log_error,
                };
                dc.SetTextColor(rgb(color))?;
                let flags = co::DT::SINGLELINE | co::DT::NOPREFIX | co::DT::END_ELLIPSIS;
                let rect = w::RECT { left: 4, top: y, right: cw - 4, bottom: y + lh };
                dc.DrawText(&line.text, rect, flags)?;
            }
            y += lh;
            i += 1;
        }
        Ok(())
    }
}

/// `Rgb` を COLORREF へ変換する。
fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}
