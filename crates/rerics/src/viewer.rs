//! テキスト/バイナリビューアの表示パネル（自前描画）。
//!
//! 別窓は作らず、メイン領域（ペイン＋ログ）に重ねて表示する 1 枚の `WindowControl`。
//! 表示モデルは `rerics_core::ViewerModel`（折返し・ダンプ整形・エンコーディングは core 側）。
//! 本モジュールは描画・スクロール・キー操作の GUI 配線に徹する。下端に状態行（ファイル名・
//! エンコーディング・モード・行位置）を自前描画する。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{Colors, Config, DisplayLine, Rgb, ViewMode, ViewerModel};
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;

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
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// ファイル内容を読み込んで表示状態にする（モード/エンコーディングは既定へ戻す）。
    pub fn open(&self, filename: &str, mut bytes: Vec<u8>, truncated: bool) {
        bytes.truncate(MAX_VIEW_BYTES);
        *self.inner.model.borrow_mut() = ViewerModel::new(bytes);
        *self.inner.title.borrow_mut() = filename.to_owned();
        self.inner.truncated.set(truncated);
        self.inner.scroll_top.set(0);
        self.inner.dirty.set(true);
        let _ = self.refresh();
    }

    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// エンコーディングを循環切替する。
    pub fn cycle_encoding(&self, forward: bool) -> w::AnyResult<()> {
        self.inner.model.borrow_mut().cycle_encoding(forward);
        self.inner.dirty.set(true);
        self.refresh()
    }

    /// テキスト/バイナリを切替する（スクロール位置は先頭へ）。
    pub fn toggle_mode(&self) -> w::AnyResult<()> {
        self.inner.model.borrow_mut().toggle_mode();
        self.inner.scroll_top.set(0);
        self.inner.dirty.set(true);
        self.refresh()
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

    /// 本文領域の高さ（状態行を除く）。
    fn body_height(&self) -> i32 {
        let ch = self.hwnd().GetClientRect().map(|r| r.bottom - r.top).unwrap_or(0);
        (ch - self.status_height()).max(0)
    }

    fn status_height(&self) -> i32 {
        self.inner.line_height.get() + gui::dpi_y(6)
    }

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
        self.wnd.on().wm(co::WM::MOUSEACTIVATE, |_| Ok(3));

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm_mouse_wheel(move |p| {
            let dist = p.keys.raw() as i16;
            this.scroll_by_wheel(dist)?;
            Ok(())
        });
    }

    /// 折返し桁を現在のクライアント幅から算出する。
    fn wrap_cols(&self) -> usize {
        let cw = self.hwnd().GetClientRect().map(|r| r.right - r.left).unwrap_or(0);
        let cwd = self.inner.char_width.get().max(1);
        let gutter_w = (self.inner.gutter_chars.get() as i32 + 2) * cwd;
        let avail = cw - gutter_w - cwd; // 右に1文字分の余白
        ((avail / cwd).max(1)) as usize
    }

    /// 必要なら表示行を再生成する。
    fn rebuild_if_needed(&self, wrap_cols: usize) {
        if !self.inner.dirty.get() && self.inner.cached_wrap.get() == wrap_cols {
            return;
        }
        let lines = self.inner.model.borrow().lines(wrap_cols, TAB_WIDTH);
        let gutter_chars = lines.iter().map(|l| l.gutter.chars().count()).max().unwrap_or(1).max(1);
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
        let font = self.create_font()?;
        let _font_sel = mem_dc.SelectObject(&*font)?;
        // メトリクス実測（行高・文字幅）。
        if let Ok(tm) = mem_dc.GetTextMetrics() {
            self.inner.line_height.set(tm.tmHeight + gui::dpi_y(2));
            self.inner.char_width.set((tm.tmAveCharWidth).max(1));
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
        let wrap_cols = self.wrap_cols();
        self.rebuild_if_needed(wrap_cols);

        let body_h = self.body_height();
        let lh = self.inner.line_height.get().max(1);
        let cwd = self.inner.char_width.get().max(1);
        let gutter_w = (self.inner.gutter_chars.get() as i32 + 1) * cwd;

        // 背景。
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        // 本文。
        let lines = self.inner.lines.borrow();
        let top = self.inner.scroll_top.get();
        let mut y = 0i32;
        let mut i = top;
        while i < lines.len() && y < body_h {
            let line = &lines[i];
            if !line.gutter.is_empty() {
                dc.SetTextColor(rgb(colors.log_info))?;
                let rect = w::RECT { left: 0, top: y, right: gutter_w - cwd / 2, bottom: y + lh };
                dc.DrawText(&line.gutter, rect, co::DT::SINGLELINE | co::DT::RIGHT | co::DT::NOPREFIX)?;
            }
            if !line.body.is_empty() {
                dc.SetTextColor(rgb(colors.file_normal))?;
                let rect = w::RECT { left: gutter_w, top: y, right: cw, bottom: y + lh };
                dc.DrawText(&line.body, rect, co::DT::SINGLELINE | co::DT::NOPREFIX)?;
            }
            y += lh;
            i += 1;
        }

        // 下端の状態行（chrome のグレー帯）。
        let status_h = self.status_height();
        let sy = ch - status_h;
        self.draw_status(dc, cw, sy, status_h)?;
        Ok(())
    }

    fn draw_status(&self, dc: &w::HDC, cw: i32, sy: i32, sh: i32) -> w::AnyResult<()> {
        let face = chrome::face();
        let brush = w::HBRUSH::CreateSolidBrush(face)?;
        dc.FillRect(w::RECT { left: 0, top: sy, right: cw, bottom: sy + sh }, &brush)?;
        chrome::hline(dc, 0, cw, sy, chrome::highlight())?;
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
        let total = self.inner.lines.borrow().len();
        let cur = self.inner.scroll_top.get() + 1;
        let trunc = if self.inner.truncated.get() { "  [先頭のみ]" } else { "" };
        format!(
            "{}    [{}]  {}    {}/{} 行    (C:エンコ B:ダンプ Esc:閉じる){}",
            self.inner.title.borrow(),
            model.encoding.label(),
            mode,
            cur.min(total.max(1)),
            total,
            trunc,
        )
    }

}

fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}
