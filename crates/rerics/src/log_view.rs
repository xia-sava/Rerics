//! 下部ログウィンドウ。`LogState` を自前描画し、レベル別色・太字で行表示する。
//!
//! `TabBar` と同様の GDI ダブルバッファ描画。新着行が来たら末尾へ自動追従し、
//! ホイールと右端スクロールバーで過去ログを遡れる。キーフォーカスは持たない（閲覧専用）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{Colors, Config, LogLevel, LogState, Rgb};
use winsafe::{self as w, co, gui, prelude::*};

struct Inner {
    state: RefCell<LogState>,
    colors: Cell<Colors>,
    font_family: RefCell<String>,
    font_size: Cell<i32>,
    scrollbar_width: Cell<i32>,
    /// 1行の高さ（描画時にフォントメトリクスから更新）。
    line_height: Cell<i32>,
    /// スクロールバー thumb ドラッグ中の、掴んだ位置の thumb 上端からのオフセット。
    sb_drag: Cell<Option<i32>>,
}

/// 下部ログウィンドウコントロール。
#[derive(Clone)]
pub struct LogView {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl LogView {
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
            state: RefCell::new(LogState::new()),
            colors: Cell::new(cfg.active_colors()),
            font_family: RefCell::new(cfg.font.family.clone()),
            font_size: Cell::new(cfg.font.size),
            scrollbar_width: Cell::new(cfg.layout.scrollbar_width),
            line_height: Cell::new(gui::dpi_y(cfg.font.size + 2)),
            sb_drag: Cell::new(None),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// 通常レベルで追記する（操作の逐次ログ。白・非太字）。
    pub fn normal(&self, text: &str) {
        self.push(LogLevel::Normal, text);
    }

    /// 情報レベルで追記する（結果サマリ等。太字）。
    pub fn info(&self, text: &str) {
        self.push(LogLevel::Info, text);
    }

    /// 警告レベルで追記する（Skip 表示等。バックグラウンド処理の実装で使う）。
    #[allow(dead_code)]
    pub fn warn(&self, text: &str) {
        self.push(LogLevel::Warning, text);
    }

    /// エラーレベルで追記する（太字）。
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

    /// インプレース更新できる `id` 付き行を追記する（進捗行用）。末尾へ追従する。
    pub fn push_with_id(&self, id: u64, level: LogLevel, text: &str) {
        let pr = self.page_rows();
        {
            let mut s = self.inner.state.borrow_mut();
            s.push_with_id(id, level, text);
            s.scroll_to_bottom(pr);
        }
        let _ = self.refresh();
    }

    /// `id` 付き行の本文を書き換えて再描画する（スクロール位置は変えない）。
    pub fn update(&self, id: u64, text: &str) {
        self.inner.state.borrow_mut().update(id, text);
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

    /// 設定の配色・フォント・スクロールバー幅を反映して再描画する。
    pub fn apply_config(&self, cfg: &Config) {
        self.inner.colors.set(cfg.active_colors());
        *self.inner.font_family.borrow_mut() = cfg.font.family.clone();
        self.inner.font_size.set(cfg.font.size);
        self.inner.scrollbar_width.set(cfg.layout.scrollbar_width);
        let _ = self.refresh();
    }

    /// スクロールバーの (バー左端x, トラック上端y, トラック高, thumb上端y, thumb高) を返す。
    /// スクロール不要（count <= page）なら None。
    fn scrollbar_geom(&self, cw: i32, ch: i32) -> Option<(i32, i32, i32, i32, i32)> {
        let (count, scroll_top) = {
            let s = self.inner.state.borrow();
            (s.count(), s.scroll_top)
        };
        let page = self.page_rows();
        if count <= page {
            return None;
        }
        let sbw = gui::dpi_x(self.inner.scrollbar_width.get());
        let bar_x = cw - sbw;
        let track_top = 1;
        let track_h = ch - track_top;
        if track_h <= 0 {
            return None;
        }
        let min_thumb = gui::dpi_y(16);
        let thumb_h = ((track_h * page as i32) / count as i32).max(min_thumb).min(track_h);
        let max_top = count - page;
        let pos = scroll_top.min(max_top);
        let thumb_top = track_top + ((track_h - thumb_h) * pos as i32) / max_top as i32;
        Some((bar_x, track_top, track_h, thumb_top, thumb_h))
    }

    /// フォントを生成する（設定のファミリ・サイズ、太字指定）。
    fn create_font(&self, bold: bool) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
        let weight = if bold { co::FW::BOLD } else { co::FW::NORMAL };
        w::HFONT::CreateFont(
            w::SIZE { cx: 0, cy: -gui::dpi_y(self.inner.font_size.get()) },
            0,
            0,
            weight,
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

        let this = self.clone();
        self.wnd.on().wm_mouse_wheel(move |p| {
            let dist = p.keys.raw() as i16;
            this.scroll_by_wheel(dist)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_down(move |p| {
            this.on_l_button_down(p.coords)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_up(move |_p| {
            this.inner.sb_drag.set(None);
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_move(move |p| {
            this.on_mouse_move(p.coords)?;
            Ok(())
        });
    }

    fn on_l_button_down(&self, pt: w::POINT) -> w::AnyResult<()> {
        let rc = self.hwnd().GetClientRect()?;
        let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
        if let Some((bar_x, _track_top, _track_h, thumb_top, thumb_h)) = self.scrollbar_geom(cw, ch) {
            if pt.x >= bar_x {
                if pt.y >= thumb_top && pt.y < thumb_top + thumb_h {
                    self.inner.sb_drag.set(Some(pt.y - thumb_top));
                } else {
                    let pr = self.page_rows();
                    let mut s = self.inner.state.borrow_mut();
                    let cur = s.scroll_top as isize;
                    let delta = if pt.y < thumb_top { -(pr as isize) } else { pr as isize };
                    s.set_scroll_top(cur + delta, pr);
                    drop(s);
                    self.refresh()?;
                }
            }
        }
        Ok(())
    }

    fn on_mouse_move(&self, pt: w::POINT) -> w::AnyResult<()> {
        if let Some(grab) = self.inner.sb_drag.get() {
            let rc = self.hwnd().GetClientRect()?;
            let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
            if let Some((_bx, track_top, track_h, _tt, thumb_h)) = self.scrollbar_geom(cw, ch) {
                let new_thumb_top = pt.y - grab;
                let denom = (track_h - thumb_h).max(1);
                let pr = self.page_rows();
                let mut s = self.inner.state.borrow_mut();
                let max_top = s.count().saturating_sub(pr) as isize;
                let pos = ((new_thumb_top - track_top) as i64 * max_top as i64 / denom as i64) as isize;
                s.set_scroll_top(pos, pr);
                drop(s);
                self.refresh()?;
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
        let mem_dc = hdc.CreateCompatibleDC()?;
        let bmp = hdc.CreateCompatibleBitmap(cw, ch)?;
        let _bmp_sel = mem_dc.SelectObject(&*bmp)?;
        let font = self.create_font(false)?;
        let font_bold = self.create_font(true)?;
        let _font_sel = mem_dc.SelectObject(&*font)?;
        if let Ok(tm) = mem_dc.GetTextMetrics() {
            self.inner.line_height.set(tm.tmHeight + gui::dpi_y(1));
        }
        mem_dc.SetBkMode(co::BKMODE::TRANSPARENT)?;

        self.paint_to(&mem_dc, cw, ch, &font, &font_bold)?;

        hdc.BitBlt(
            w::POINT { x: 0, y: 0 },
            w::SIZE { cx: cw, cy: ch },
            &mem_dc,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    fn paint_to(
        &self,
        dc: &w::HDC,
        cw: i32,
        ch: i32,
        font: &w::HFONT,
        font_bold: &w::HFONT,
    ) -> w::AnyResult<()> {
        let colors = self.inner.colors.get();

        // 背景。
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.log_background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        // 上端の区切り線（ペインとの境界）。
        let border = w::COLORREF::from_rgb(0x40, 0x40, 0x40);
        let pen = w::HPEN::CreatePen(co::PS::SOLID, 1, border)?;
        let _pen_sel = dc.SelectObject(&*pen)?;
        dc.MoveToEx(0, 0, None)?;
        dc.LineTo(cw, 0)?;

        let sb = self.scrollbar_geom(cw, ch);
        let text_right = match sb {
            Some((bar_x, ..)) => bar_x - 2,
            None => cw - 4,
        };

        let lh = self.inner.line_height.get().max(1);
        {
            let s = self.inner.state.borrow();
            let count = s.count();
            let mut i = s.scroll_top;
            let mut y = 1;
            while i < count && y < ch {
                let line = &s.lines[i];
                if !line.text.is_empty() {
                    let (color, bold) = match line.level {
                        LogLevel::Normal => (colors.log_normal, false),
                        LogLevel::Info => (colors.log_info, true),
                        LogLevel::Warning => (colors.log_warning, false),
                        LogLevel::Error => (colors.log_error, true),
                    };
                    let _sel = dc.SelectObject(if bold { font_bold } else { font })?;
                    dc.SetTextColor(rgb(color))?;
                    let flags = co::DT::SINGLELINE | co::DT::NOPREFIX | co::DT::END_ELLIPSIS;
                    let rect = w::RECT { left: 4, top: y, right: text_right, bottom: y + lh };
                    dc.DrawText(&line.text, rect, flags)?;
                }
                y += lh;
                i += 1;
            }
        }

        // 右端スクロールバー（トラックは背景同色・thumb はグレー）。
        if let Some((bar_x, track_top, _track_h, thumb_top, thumb_h)) = sb {
            let track_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.log_background))?;
            dc.FillRect(
                w::RECT { left: bar_x, top: track_top, right: cw, bottom: ch },
                &track_brush,
            )?;
            let thumb_brush = w::HBRUSH::CreateSolidBrush(w::COLORREF::from_rgb(0x55, 0x55, 0x55))?;
            dc.FillRect(
                w::RECT { left: bar_x + 1, top: thumb_top, right: cw - 1, bottom: thumb_top + thumb_h },
                &thumb_brush,
            )?;
        }
        Ok(())
    }
}

/// `Rgb` を COLORREF へ変換する。
fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}
