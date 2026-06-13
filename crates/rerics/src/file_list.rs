//! 自前描画のファイル一覧コントロール。
//!
//! 状態は `rerics_core::FileListState` に持たせ、本モジュールは描画・入力・スクロールの
//! GUI 配線に徹する。ダブルバッファでちらつきを抑える。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{Align, ColumnKind, Colors, Config, FileListState, Rgb, SortType};
use winsafe::{self as w, co, gui, prelude::*};

/// マウス操作の状態機械。
#[derive(Clone, Copy, PartialEq, Eq)]
enum MouseEvent {
    None,
    HeaderDrag,
    HeaderClick,
    RowClick,
}

type ActivateCb = Box<dyn Fn(usize)>;
type WheelCb = Box<dyn Fn(i16, w::POINT)>;

/// 列ドラッグ中の状態。
#[derive(Clone, Copy)]
struct HeaderDrag {
    col: usize,
    start_x: i32,
    start_width: i32,
}

struct Inner {
    state: Rc<RefCell<FileListState>>,
    colors: Colors,
    font_family: String,
    font_size: i32,
    /// 自前スクロールバーの幅（論理 px）。
    scrollbar_width: i32,
    /// フォント高さ（実測）。
    font_height: Cell<i32>,
    /// カーソル表示フラグ（アクティブペイン管理は main 側で配線）。
    cursor_visible: Cell<bool>,
    mouse_event: Cell<MouseEvent>,
    header_click_col: Cell<Option<usize>>,
    drag: Cell<Option<HeaderDrag>>,
    /// スクロールバー thumb ドラッグ中の、掴んだ位置の thumb 上端からのオフセット。
    sb_drag: Cell<Option<i32>>,
    on_activate: RefCell<Option<ActivateCb>>,
    on_got_focus: RefCell<Option<Box<dyn Fn()>>>,
    on_wheel: RefCell<Option<WheelCb>>,
}

/// ファイル一覧コントロール。
#[derive(Clone)]
pub struct FileListView {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl FileListView {
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
                style: co::WS::CHILD
                    | co::WS::VISIBLE
                    | co::WS::CLIPSIBLINGS
                    | co::WS::TABSTOP,
                ..Default::default()
            },
        );
        let inner = Rc::new(Inner {
            state: Rc::new(RefCell::new(FileListState::new())),
            colors: cfg.colors,
            font_family: cfg.font.family.clone(),
            font_size: cfg.font.size,
            scrollbar_width: cfg.layout.scrollbar_width,
            font_height: Cell::new(gui::dpi_y(cfg.font.size)),
            cursor_visible: Cell::new(false),
            mouse_event: Cell::new(MouseEvent::None),
            header_click_col: Cell::new(None),
            drag: Cell::new(None),
            sb_drag: Cell::new(None),
            on_activate: RefCell::new(None),
            on_got_focus: RefCell::new(None),
            on_wheel: RefCell::new(None),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn state(&self) -> Rc<RefCell<FileListState>> {
        self.inner.state.clone()
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    pub fn on_activate(&self, cb: impl Fn(usize) + 'static) {
        *self.inner.on_activate.borrow_mut() = Some(Box::new(cb));
    }

    /// フォーカス取得時のコールバック（反対ペインのカーソル消去の配線用）。
    pub fn on_got_focus(&self, cb: impl Fn() + 'static) {
        *self.inner.on_got_focus.borrow_mut() = Some(Box::new(cb));
    }

    /// ホイール回転時のコールバック（回転量と画面座標を渡す）。設定すると自前スクロールの
    /// 代わりにこれが呼ばれ、呼び出し側がカーソル下のペインを判定してスクロールする。
    pub fn on_wheel(&self, cb: impl Fn(i16, w::POINT) + 'static) {
        *self.inner.on_wheel.borrow_mut() = Some(Box::new(cb));
    }

    /// カーソル下線の表示/非表示を切り替える。
    pub fn set_cursor_visible(&self, visible: bool) {
        if self.inner.cursor_visible.get() != visible {
            self.inner.cursor_visible.set(visible);
            let _ = self.hwnd().InvalidateRect(None, false);
        }
    }

    /// 再描画を促す。
    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, false)?;
        Ok(())
    }

    /// クライアント高から1ページ行数を算出する。
    pub fn page_rows(&self) -> usize {
        let Ok(rc) = self.hwnd().GetClientRect() else {
            return 1;
        };
        let client_h = rc.bottom - rc.top;
        let ih = self.item_height();
        if ih <= 0 {
            return 1;
        }
        let rows = (client_h - self.header_height()) / ih;
        rows.max(1) as usize
    }

    fn font_height(&self) -> i32 {
        self.inner.font_height.get()
    }

    fn header_height(&self) -> i32 {
        self.font_height() + 8
    }

    fn item_height(&self) -> i32 {
        self.font_height()
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
        // クリックされてもフォーカスを奪わない（キー入力はキーシンクへ集約する）。
        self.wnd.on().wm(co::WM::MOUSEACTIVATE, |_| Ok(3));

        let this = self.clone();
        self.wnd.on().wm_get_dlg_code(move |_| {
            let _ = &this;
            let flags = co::DLGC::WANTARROWS.raw() | co::DLGC::WANTALLKEYS.raw();
            Ok(unsafe { co::DLGC::from_raw(flags) })
        });

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm_size(move |_| {
            {
                let pr = this.page_rows();
                let mut s = this.inner.state.borrow_mut();
                let top = s.scroll_top as isize;
                s.set_scroll_top(top, pr);
            }
            this.refresh()?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_set_focus(move |_| {
            this.set_cursor_visible(true);
            if let Some(cb) = this.inner.on_got_focus.borrow().as_ref() {
                cb();
            }
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_wheel(move |p| {
            // winsafe 0.0.27 は wheel_distance と keys を取り違えるため、回転量は
            // keys（実際は HIWORD=回転量）から取り出す。
            let dist = p.keys.raw() as i16;
            if let Some(cb) = this.inner.on_wheel.borrow().as_ref() {
                cb(dist, p.coords);
            } else {
                this.scroll_by_wheel(dist)?;
            }
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_down(move |p| {
            this.on_l_button_down(p.coords, p.vkey_code)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_up(move |p| {
            this.on_l_button_up(p.coords)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_move(move |p| {
            this.on_mouse_move(p.coords)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_dbl_clk(move |p| {
            this.on_l_button_dbl_clk(p.coords)?;
            Ok(())
        });
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
        let sbw = gui::dpi_x(self.inner.scrollbar_width);
        let bar_x = cw - sbw;
        let track_top = self.header_height();
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

    /// ホイール回転分だけスクロールする（正＝上方向）。
    pub fn scroll_by_wheel(&self, distance: i16) -> w::AnyResult<()> {
        let lines = (distance as i32 / 120) * 3;
        {
            let mut s = self.inner.state.borrow_mut();
            let pr = self.page_rows();
            let top = s.scroll_top as isize - lines as isize;
            s.set_scroll_top(top, pr);
            s.cursor_into_view(pr);
        }
        self.refresh()?;
        Ok(())
    }

    /// 列境界(右端±4px)を指している列 index を返す。
    fn hit_column_border(&self, x: i32) -> Option<usize> {
        let s = self.inner.state.borrow();
        let mut left = 0i32;
        for (i, col) in s.columns.iter().enumerate() {
            let right = left + gui::dpi_x(col.width);
            if (x - right).abs() <= 4 {
                return Some(i);
            }
            left = right;
        }
        None
    }

    /// 座標 x がどの列に属するか（ヘッダクリック判定用）。
    fn column_at(&self, x: i32) -> Option<usize> {
        let s = self.inner.state.borrow();
        let mut left = 0i32;
        for (i, col) in s.columns.iter().enumerate() {
            let right = left + gui::dpi_x(col.width);
            if x >= left && x < right {
                return Some(i);
            }
            left = right;
        }
        None
    }

    /// 行 index を座標 y から算出する（ヘッダ以下）。
    fn row_at(&self, y: i32) -> Option<usize> {
        let hh = self.header_height();
        if y < hh {
            return None;
        }
        let ih = self.item_height();
        if ih <= 0 {
            return None;
        }
        let s = self.inner.state.borrow();
        let idx = s.scroll_top + ((y - hh) / ih) as usize;
        if idx < s.count() { Some(idx) } else { None }
    }

    fn on_l_button_down(&self, pt: w::POINT, keys: co::MK) -> w::AnyResult<()> {
        // クリックされたペインをアクティブにする（Win32 フォーカスは取らず内部状態のみ更新）。
        if let Some(cb) = self.inner.on_got_focus.borrow().as_ref() {
            cb();
        }
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
                    s.cursor_into_view(pr);
                    drop(s);
                    self.refresh()?;
                }
                return Ok(());
            }
        }
        let hh = self.header_height();
        if pt.y < hh {
            if let Some(col) = self.hit_column_border(pt.x) {
                let start_width = gui::dpi_x(self.inner.state.borrow().columns[col].width);
                self.inner.drag.set(Some(HeaderDrag {
                    col,
                    start_x: pt.x,
                    start_width,
                }));
                self.inner.mouse_event.set(MouseEvent::HeaderDrag);
            } else if let Some(col) = self.column_at(pt.x) {
                self.inner.mouse_event.set(MouseEvent::HeaderClick);
                self.inner.header_click_col.set(Some(col));
            }
            return Ok(());
        }
        let Some(idx) = self.row_at(pt.y) else {
            return Ok(());
        };
        self.hwnd().SetFocus();
        let shift = keys.has(co::MK::SHIFT);
        let ctrl = keys.has(co::MK::CONTROL);
        let pr = self.page_rows();
        self.inner.mouse_event.set(MouseEvent::RowClick);
        {
            let mut s = self.inner.state.borrow_mut();
            s.cursor = idx;
            if ctrl {
                s.reverse_file(idx, pr);
            } else if shift {
                let start = s.select_start;
                s.clear_all();
                s.select_files(start, idx);
                s.set_cursor(idx as isize, pr);
            } else {
                let already = s.items[idx].selected;
                if !already {
                    s.clear_all();
                }
                s.select_file(idx, pr);
            }
        }
        self.refresh()?;
        Ok(())
    }

    fn on_l_button_up(&self, _pt: w::POINT) -> w::AnyResult<()> {
        if self.inner.sb_drag.get().is_some() {
            self.inner.sb_drag.set(None);
            return Ok(());
        }
        match self.inner.mouse_event.get() {
            MouseEvent::HeaderClick => {
                if let Some(col) = self.inner.header_click_col.get() {
                    self.sort_by_column(col)?;
                }
            }
            MouseEvent::RowClick => {
                // 修飾なしクリックはマークをその1件に確定する。
                let shift = w::GetAsyncKeyState(co::VK::SHIFT);
                let ctrl = w::GetAsyncKeyState(co::VK::CONTROL);
                if !shift && !ctrl {
                    let pr = self.page_rows();
                    {
                        let mut s = self.inner.state.borrow_mut();
                        let cur = s.cursor;
                        s.clear_all();
                        s.select_file(cur, pr);
                    }
                    self.refresh()?;
                }
            }
            _ => {}
        }
        self.inner.mouse_event.set(MouseEvent::None);
        self.inner.header_click_col.set(None);
        self.inner.drag.set(None);
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
                s.cursor_into_view(pr);
                drop(s);
                self.refresh()?;
            }
            return Ok(());
        }
        if self.inner.mouse_event.get() == MouseEvent::HeaderDrag {
            if let Some(d) = self.inner.drag.get() {
                let new_w = (d.start_width + (pt.x - d.start_x)).max(8);
                {
                    let mut s = self.inner.state.borrow_mut();
                    // 物理 px を論理 px へ戻して格納（dpi_x(96)=現在DPI）。
                    let dpi = gui::dpi_x(96).max(1);
                    let logical = (new_w * 96 + dpi / 2) / dpi;
                    s.columns[d.col].width = logical.max(8);
                }
                self.refresh()?;
            }
        }
        Ok(())
    }

    fn on_l_button_dbl_clk(&self, pt: w::POINT) -> w::AnyResult<()> {
        if let Some(idx) = self.row_at(pt.y) {
            if let Some(cb) = self.inner.on_activate.borrow().as_ref() {
                cb(idx);
            }
        }
        Ok(())
    }

    /// 列のソート種別へ切替える（同種別なら reverse 反転）。
    fn sort_by_column(&self, col: usize) -> w::AnyResult<()> {
        let kind = self.inner.state.borrow().columns[col].kind;
        let target = match kind {
            ColumnKind::FileName | ColumnKind::FileBaseName => SortType::FileName,
            ColumnKind::FileExtension => SortType::Extension,
            ColumnKind::Length => SortType::Length,
            ColumnKind::CreateTime | ColumnKind::CreateTimeS => SortType::CreateTime,
            ColumnKind::LastWriteTime | ColumnKind::LastWriteTimeS => SortType::LastWriteTime,
            ColumnKind::Attribute => SortType::Attribute,
        };
        {
            let mut s = self.inner.state.borrow_mut();
            let reverse = if s.sort_type == target { !s.sort_reverse } else { false };
            s.sort(target, reverse);
        }
        self.refresh()?;
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
        // フォント高さ実測。
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
        let cursor_visible = self.inner.cursor_visible.get();
        let header_h = self.header_height();
        let item_h = self.item_height();

        // 1. 背景全面。
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        let s = self.inner.state.borrow();

        // 各列の左端を算出（論理→物理）。
        let mut col_lefts = Vec::with_capacity(s.columns.len() + 1);
        let mut x = 0i32;
        for col in &s.columns {
            col_lefts.push(x);
            x += gui::dpi_x(col.width);
        }
        let total_w = x;
        col_lefts.push(total_w);

        // 2. ヘッダ。
        let face = w::GetSysColor(co::COLOR::BTNFACE);
        let hl = w::GetSysColor(co::COLOR::BTNHIGHLIGHT);
        let sh = w::GetSysColor(co::COLOR::BTNSHADOW);
        let wtext = w::GetSysColor(co::COLOR::WINDOWTEXT);
        let face_brush = w::HBRUSH::CreateSolidBrush(face)?;
        for (i, col) in s.columns.iter().enumerate() {
            let left = col_lefts[i];
            let right = col_lefts[i + 1];
            self.draw_header_cell(dc, &s, left, right, header_h, &face_brush, hl, sh, wtext, Some(col))?;
        }
        // 末尾余白列。
        if total_w < cw {
            dc.FillRect(
                w::RECT { left: total_w, top: 0, right: cw, bottom: header_h },
                &face_brush,
            )?;
            self.draw_3d_frame(dc, total_w, 0, cw, header_h, hl, sh)?;
        }

        // 3. 行。
        let page = self.page_rows();
        let bottom = s.scroll_bottom(page);
        let sel_bg = w::HBRUSH::CreateSolidBrush(rgb(colors.selected_file_bg))?;
        for i in s.scroll_top..=bottom {
            if i >= s.count() {
                break;
            }
            let item = &s.items[i];
            let y = header_h + ((i - s.scroll_top) as i32) * item_h;
            let text_color = if item.selected {
                dc.FillRect(
                    w::RECT { left: 0, top: y, right: total_w, bottom: y + item_h + 1 },
                    &sel_bg,
                )?;
                colors.selected_file
            } else {
                colors.item_color(item)
            };
            dc.SetTextColor(rgb(text_color))?;
            for (ci, col) in s.columns.iter().enumerate() {
                let left = col_lefts[ci];
                let right = col_lefts[ci + 1];
                let text = s.cell_text(item, col.kind);
                if text.is_empty() {
                    continue;
                }
                let mut flags = co::DT::SINGLELINE
                    | co::DT::VCENTER
                    | co::DT::NOPREFIX
                    | co::DT::END_ELLIPSIS;
                if col.align == Align::Right {
                    flags |= co::DT::RIGHT;
                }
                let rect = w::RECT { left: left + 4, top: y, right: right - 4, bottom: y + item_h };
                dc.DrawText(&text, rect, flags)?;
            }
            // 4. カーソル下線。
            if cursor_visible && i == s.cursor {
                let y_line = y + (item_h - self.font_height()) / 2 + self.font_height() - 1;
                let pen = w::HPEN::CreatePen(co::PS::SOLID, 1, rgb(colors.cursor))?;
                let _pen_sel = dc.SelectObject(&*pen)?;
                dc.MoveToEx(0, y_line, None)?;
                dc.LineTo(total_w, y_line)?;
            }
        }

        // 5. 自前スクロールバー（生きている state borrow からインラインに算出）。
        let count = s.count();
        if count > page {
            let sbw = gui::dpi_x(self.inner.scrollbar_width);
            let bar_x = cw - sbw;
            let track_top = header_h;
            let track_h = ch - track_top;
            if track_h > 0 {
                let min_thumb = gui::dpi_y(16);
                let thumb_h = ((track_h * page as i32) / count as i32).max(min_thumb).min(track_h);
                let max_top = count - page;
                let pos = s.scroll_top.min(max_top);
                let thumb_top = track_top + ((track_h - thumb_h) * pos as i32) / max_top as i32;
                let track_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.background))?;
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
        }
        Ok(())
    }

    /// ヘッダ1列を描く（3D 枠＋テキスト＋ソート三角）。
    #[allow(clippy::too_many_arguments)]
    fn draw_header_cell(
        &self,
        dc: &w::HDC,
        s: &FileListState,
        left: i32,
        right: i32,
        header_h: i32,
        face_brush: &w::HBRUSH,
        hl: w::COLORREF,
        sh: w::COLORREF,
        wtext: w::COLORREF,
        col: Option<&rerics_core::Column>,
    ) -> w::AnyResult<()> {
        dc.FillRect(w::RECT { left, top: 0, right, bottom: header_h }, face_brush)?;
        self.draw_3d_frame(dc, left, 0, right, header_h, hl, sh)?;
        let Some(col) = col else {
            return Ok(());
        };
        // テキスト。
        dc.SetTextColor(wtext)?;
        if !col.text.is_empty() {
            let rect = w::RECT { left: left + 4, top: 4, right: right - 4, bottom: header_h };
            dc.DrawText(&col.text, rect, co::DT::SINGLELINE | co::DT::NOPREFIX)?;
        }
        // ソート三角。
        let sort_match = matches!(
            (col.kind, s.sort_type),
            (ColumnKind::FileName | ColumnKind::FileBaseName, SortType::FileName)
                | (ColumnKind::FileName | ColumnKind::FileBaseName, SortType::FileNameExpLike)
                | (ColumnKind::FileExtension, SortType::Extension)
                | (ColumnKind::FileExtension, SortType::ExtensionExpLike)
                | (ColumnKind::Length, SortType::Length)
                | (ColumnKind::CreateTime | ColumnKind::CreateTimeS, SortType::CreateTime)
                | (ColumnKind::LastWriteTime | ColumnKind::LastWriteTimeS, SortType::LastWriteTime)
                | (ColumnKind::Attribute, SortType::Attribute)
        );
        if sort_match {
            let tw = dc.GetTextExtentPoint32(&col.text).map(|sz| sz.cx).unwrap_or(0);
            let x0 = left + 4 + tw + 8;
            let top = 6;
            let bot = header_h - 8;
            let pen = w::HPEN::CreatePen(co::PS::SOLID, 1, w::COLORREF::from_rgb(64, 64, 64))?;
            let _pen_sel = dc.SelectObject(&*pen)?;
            if !s.sort_reverse {
                // 昇順: 頂点上。
                dc.MoveToEx(x0, bot, None)?;
                dc.LineTo(x0 + 3, top)?;
                dc.LineTo(x0 + 6, bot)?;
                dc.MoveToEx(x0, bot, None)?;
                dc.LineTo(x0 + 6, bot)?;
            } else {
                // 降順: 頂点下。
                dc.MoveToEx(x0, top, None)?;
                dc.LineTo(x0 + 6, top)?;
                dc.MoveToEx(x0, top, None)?;
                dc.LineTo(x0 + 3, bot)?;
                dc.LineTo(x0 + 6, top)?;
            }
        }
        Ok(())
    }

    /// 明(左上)/暗(右下)の 3D 枠を描く。
    fn draw_3d_frame(
        &self,
        dc: &w::HDC,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        hl: w::COLORREF,
        sh: w::COLORREF,
    ) -> w::AnyResult<()> {
        let pen_hl = w::HPEN::CreatePen(co::PS::SOLID, 1, hl)?;
        {
            let _sel = dc.SelectObject(&*pen_hl)?;
            dc.MoveToEx(left, bottom - 1, None)?;
            dc.LineTo(left, top)?;
            dc.LineTo(right - 1, top)?;
        }
        let pen_sh = w::HPEN::CreatePen(co::PS::SOLID, 1, sh)?;
        {
            let _sel = dc.SelectObject(&*pen_sh)?;
            dc.MoveToEx(right - 1, top, None)?;
            dc.LineTo(right - 1, bottom - 1)?;
            dc.LineTo(left, bottom - 1)?;
        }
        Ok(())
    }
}

/// `Rgb` を COLORREF へ変換する。
fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}
