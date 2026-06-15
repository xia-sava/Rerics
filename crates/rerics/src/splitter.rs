//! 左右ペイン間の境界線（スプリッタ）。ドラッグで分割比を変える細い縦バー。
//!
//! キーフォーカスは持たない（`MA_NOACTIVATE`）。ドラッグ中はマウスをキャプチャして、
//! カーソルのスクリーン座標→親クライアント座標に変換した「スプリッタの希望左端」を
//! コールバックへ渡す。比率の算出・レイアウト反映は呼び出し側（MainWindow）が行う。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use winsafe::{self as w, co, gui, prelude::*};

type DragCb = Box<dyn Fn(i32)>;

struct Inner {
    /// ドラッグ中か。
    dragging: Cell<bool>,
    /// 掴んだ位置のスプリッタ左端からのオフセット（スプリッタがカーソルに飛びつかないように）。
    grab_dx: Cell<i32>,
    on_drag: RefCell<Option<DragCb>>,
}

/// 境界線コントロール。
#[derive(Clone)]
pub struct SplitterView {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl SplitterView {
    pub fn new(
        parent: &(impl GuiParent + 'static),
        position: (i32, i32),
        size: (i32, i32),
    ) -> Self {
        let wnd = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                class_bg_brush: gui::Brush::None,
                class_cursor: gui::Cursor::Idc(co::IDC::SIZEWE),
                position,
                size,
                style: co::WS::CHILD | co::WS::VISIBLE | co::WS::CLIPSIBLINGS,
                ..Default::default()
            },
        );
        let inner = Rc::new(Inner {
            dragging: Cell::new(false),
            grab_dx: Cell::new(0),
            on_drag: RefCell::new(None),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// ドラッグ時のコールバック。引数は親クライアント座標でのスプリッタ希望左端（px）。
    pub fn on_drag(&self, cb: impl Fn(i32) + 'static) {
        *self.inner.on_drag.borrow_mut() = Some(Box::new(cb));
    }

    fn setup_events(&self) {
        // クリックでフォーカスを奪わない（キー入力はキーシンクへ集約）。
        self.wnd.on().wm(co::WM::MOUSEACTIVATE, |_| Ok(3));

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm_l_button_down(move |p| {
            this.inner.grab_dx.set(p.coords.x);
            this.inner.dragging.set(true);
            // ドラッグ中はマウスをキャプチャする。ガードを手放して（forget）キャプチャを維持し、
            // 解放はボタンアップ側で行う（ReleaseCaptureGuard の Drop はグローバルな
            // ReleaseCapture を呼ぶだけなので、再取得→即 Drop で解放できる）。
            std::mem::forget(this.hwnd().SetCapture());
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_move(move |_| {
            if this.inner.dragging.get() {
                this.report_drag()?;
            }
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_up(move |_| {
            if this.inner.dragging.get() {
                this.inner.dragging.set(false);
                // キャプチャ解放（再取得した使い捨てガードを即 Drop）。
                drop(this.hwnd().SetCapture());
            }
            Ok(())
        });
    }

    /// 現在のカーソル位置から親座標でのスプリッタ希望左端を求めてコールバックへ渡す。
    fn report_drag(&self) -> w::AnyResult<()> {
        let pt = w::GetCursorPos()?;
        let parent = self.hwnd().GetParent()?;
        let cp = parent.ScreenToClient(pt)?;
        let left = cp.x - self.inner.grab_dx.get();
        if let Some(cb) = self.inner.on_drag.borrow().as_ref() {
            cb(left);
        }
        Ok(())
    }

    fn on_paint(&self) -> w::AnyResult<()> {
        let hdc = self.hwnd().BeginPaint()?;
        let rc = self.hwnd().GetClientRect()?;
        self.render_to(&hdc, rc.right - rc.left, rc.bottom - rc.top)
    }

    /// 任意の DC へ境界線を描く（`on_paint` とデバッグ制御サーバのスナップショットから呼ぶ）。
    pub(crate) fn render_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        if cw > 0 && ch > 0 {
            // ヘッダと同じシステム 3D グレー。BTNFACE で塗り、左端ハイライト・右端シャドウで
            // 縦のベベル（隆起した境界線）にする。
            let face = w::GetSysColor(co::COLOR::BTNFACE);
            let hl = w::GetSysColor(co::COLOR::BTNHIGHLIGHT);
            let sh = w::GetSysColor(co::COLOR::BTNSHADOW);
            let face_brush = w::HBRUSH::CreateSolidBrush(face)?;
            dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &face_brush)?;
            let pen_hl = w::HPEN::CreatePen(co::PS::SOLID, 1, hl)?;
            {
                let _sel = dc.SelectObject(&*pen_hl)?;
                dc.MoveToEx(0, 0, None)?;
                dc.LineTo(0, ch)?;
            }
            let pen_sh = w::HPEN::CreatePen(co::PS::SOLID, 1, sh)?;
            {
                let _sel = dc.SelectObject(&*pen_sh)?;
                dc.MoveToEx(cw - 1, 0, None)?;
                dc.LineTo(cw - 1, ch)?;
            }
        }
        Ok(())
    }
}
