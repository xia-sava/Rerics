//! ペイン上部のパスバー。現在パスを左寄せで自前描画する。
//!
//! ステータスバーと同じ GDI ダブルバッファ描画＋共通ベベル（`chrome`）で、上下のバーが
//! 揃った帯の外見になる。キーフォーカスは持たない（表示専用）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::Config;
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;

struct Inner {
    text: RefCell<String>,
    font_family: RefCell<String>,
    font_size: Cell<i32>,
}

/// ペイン上部のパスバーコントロール。
#[derive(Clone)]
pub struct PathBarView {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl PathBarView {
    /// 親に子コントロールとして生成する。
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
            text: RefCell::new(String::new()),
            font_family: RefCell::new(cfg.font.family.clone()),
            font_size: Cell::new(cfg.font.size),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// 現在のパスバー表示文字列（デバッグ制御サーバの状態取得用）。
    #[cfg(feature = "debug-server")]
    pub fn text(&self) -> String {
        self.inner.text.borrow().clone()
    }

    /// 表示するパス文字列を設定して再描画する。
    pub fn set_path(&self, text: &str) {
        if *self.inner.text.borrow() != text {
            *self.inner.text.borrow_mut() = text.to_owned();
            let _ = self.refresh();
        }
    }

    /// 再描画を促す。
    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// 設定のフォントを反映して再描画する（chrome の色はシステム固定なので対象外）。
    pub fn apply_config(&self, cfg: &Config) {
        *self.inner.font_family.borrow_mut() = cfg.font.family.clone();
        self.inner.font_size.set(cfg.font.size);
        let _ = self.refresh();
    }

    fn create_font(&self) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
        w::HFONT::CreateFont(
            w::SIZE { cx: 0, cy: -gui::dpi_y(self.inner.font_size.get() - 2) },
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
            &self.inner.font_family.borrow(),
        )
    }

    fn setup_events(&self) {
        // クリックされてもフォーカスを奪わない（キー入力はキーシンクへ集約する）。
        self.wnd.on().wm(co::WM::MOUSEACTIVATE, |_| Ok(3));

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());
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
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        self.paint_to(dc, cw, ch)
    }

    fn paint_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        chrome::fill_bar(dc, cw, ch)?;

        let text = self.inner.text.borrow();
        if !text.is_empty() {
            dc.SetTextColor(chrome::text())?;
            // 左右端の余白は1文字分（等幅フォントの字幅基準）。
            let pad = dc.GetTextExtentPoint32("0")?.cx.max(1);
            let flags = co::DT::SINGLELINE
                | co::DT::VCENTER
                | co::DT::LEFT
                | co::DT::NOPREFIX
                | co::DT::PATH_ELLIPSIS;
            let rect = w::RECT { left: pad, top: 0, right: cw - pad, bottom: ch };
            dc.DrawText(&text, rect, flags)?;
        }
        Ok(())
    }
}
