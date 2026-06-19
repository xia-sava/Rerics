//! ペイン下部のステータスバー。左に選択情報・右にドライブ容量を1行で自前描画する。
//!
//! `LogView` と同様の GDI ダブルバッファ描画。表示する文字列は外から設定し、本体は
//! 描画に徹する。キーフォーカスは持たない（表示専用）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::Config;
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;

struct Inner {
    left: RefCell<String>,
    right: RefCell<String>,
    font_family: RefCell<String>,
    font_size: Cell<i32>,
}

/// ペイン下部のステータスバーコントロール。
#[derive(Clone)]
pub struct StatusBarView {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl StatusBarView {
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
            left: RefCell::new(String::new()),
            right: RefCell::new(String::new()),
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

    /// 左右の表示文字列（デバッグ制御サーバの状態取得用）。
    #[cfg(feature = "debug-server")]
    pub fn left_text(&self) -> String {
        self.inner.left.borrow().clone()
    }

    /// 右側の表示文字列（デバッグ制御サーバの状態取得用）。
    #[cfg(feature = "debug-server")]
    pub fn right_text(&self) -> String {
        self.inner.right.borrow().clone()
    }

    /// 左側（選択情報）の文字列を設定して再描画する。
    pub fn set_left(&self, text: &str) {
        if *self.inner.left.borrow() != text {
            *self.inner.left.borrow_mut() = text.to_owned();
            let _ = self.refresh();
        }
    }

    /// 右側（ドライブ容量）の文字列を設定して再描画する。
    pub fn set_right(&self, text: &str) {
        if *self.inner.right.borrow() != text {
            *self.inner.right.borrow_mut() = text.to_owned();
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
        crate::winutil::passive_focus(&self.wnd);

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

        dc.SetTextColor(chrome::text())?;

        // 端の余白と要素間の隙間は2文字分（等幅フォントの字幅基準）。
        let pad = dc.GetTextExtentPoint32("0")?.cx.max(1) * 2;

        // 選択情報（左）を優先して左寄せでフル表示し、その右端を覚える。
        let mut left_edge = pad;
        let left = self.inner.left.borrow();
        if !left.is_empty() {
            let flags = co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX | co::DT::END_ELLIPSIS;
            let rect = w::RECT { left: pad, top: 0, right: cw - pad, bottom: ch };
            dc.DrawText(&left, rect, flags)?;
            left_edge = pad + dc.GetTextExtentPoint32(&left)?.cx;
        }

        // ドライブ容量（右）を選択情報の右側の残り幅に右寄せで入れる（足りなければ省略）。
        let right = self.inner.right.borrow();
        if !right.is_empty() {
            let flags = co::DT::SINGLELINE | co::DT::VCENTER | co::DT::RIGHT | co::DT::NOPREFIX | co::DT::END_ELLIPSIS;
            let rect = w::RECT { left: (left_edge + pad).min(cw - pad), top: 0, right: cw - pad, bottom: ch };
            dc.DrawText(&right, rect, flags)?;
        }
        Ok(())
    }
}
