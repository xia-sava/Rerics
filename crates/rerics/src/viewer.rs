//! テキスト/バイナリビューアの表示パネル（自前描画）。
//!
//! 別窓は作らず、メイン領域（ペイン＋ログ）に重ねて表示する 1 枚の `WindowControl`。
//! 表示モデルは `rerics_core::ViewerModel`（折返し・ダンプ整形・エンコーディングは core 側）。
//! 本モジュールは描画・スクロール・キー操作の GUI 配線に徹する。下端に状態行（ファイル名・
//! エンコーディング・モード・行位置）を自前描画する。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{Colors, Config, DisplayLine, LineEnding, Rgb, ViewMode, ViewerModel};
use unicode_width::UnicodeWidthChar;
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;

/// 表示行内の位置（表示行 index, 行内の char 数オフセット）。
type Pos = (usize, usize);

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
    /// 検索語（小文字化はしない。一致は大小無視で判定）。
    search_term: RefCell<String>,
    /// 現在ヒットしている表示行（ハイライト対象）。
    match_line: Cell<Option<usize>>,
    /// マウス選択の始点・終点（None なら選択なし）。
    sel_anchor: Cell<Option<Pos>>,
    sel_cursor: Cell<Option<Pos>>,
    /// ドラッグ中か。
    selecting: Cell<bool>,
    /// 右クリック時に呼ぶコールバック（画面座標）。メニュー表示は MainWindow が担う。
    on_menu: RefCell<Option<Box<dyn Fn(w::POINT)>>>,
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
            search_term: RefCell::new(String::new()),
            match_line: Cell::new(None),
            sel_anchor: Cell::new(None),
            sel_cursor: Cell::new(None),
            selecting: Cell::new(false),
            on_menu: RefCell::new(None),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// 右クリック時のコールバック（コンテキストメニュー表示）を登録する。
    pub fn on_menu(&self, cb: impl Fn(w::POINT) + 'static) {
        *self.inner.on_menu.borrow_mut() = Some(Box::new(cb));
    }

    /// ファイル内容を読み込んで表示状態にする（エンコーディングは既定、モードは内容から自動判定）。
    pub fn open(&self, filename: &str, mut bytes: Vec<u8>, truncated: bool) {
        bytes.truncate(MAX_VIEW_BYTES);
        // バイナリらしければバイナリモードで開始する（原作准拠の自動判定）。
        *self.inner.model.borrow_mut() = ViewerModel::open(bytes);
        *self.inner.title.borrow_mut() = filename.to_owned();
        self.inner.truncated.set(truncated);
        self.inner.scroll_top.set(0);
        self.inner.dirty.set(true);
        *self.inner.search_term.borrow_mut() = String::new();
        self.inner.match_line.set(None);
        self.clear_selection();
        let _ = self.refresh();
    }

    fn clear_selection(&self) {
        self.inner.sel_anchor.set(None);
        self.inner.sel_cursor.set(None);
        self.inner.selecting.set(false);
    }

    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// エンコーディングを循環切替する。
    pub fn cycle_encoding(&self, forward: bool) -> w::AnyResult<()> {
        self.inner.model.borrow_mut().cycle_encoding(forward);
        self.inner.dirty.set(true);
        self.inner.match_line.set(None);
        self.clear_selection();
        self.refresh()
    }

    /// テキスト/バイナリを切替する（スクロール位置は先頭へ）。
    pub fn toggle_mode(&self) -> w::AnyResult<()> {
        self.inner.model.borrow_mut().toggle_mode();
        self.inner.scroll_top.set(0);
        self.inner.dirty.set(true);
        self.inner.match_line.set(None);
        self.clear_selection();
        self.refresh()
    }

    /// 現在の検索語を返す（検索ダイアログの初期値用）。
    pub fn search_term(&self) -> String {
        self.inner.search_term.borrow().clone()
    }

    /// 検索語を設定し、現在位置から最初の一致へジャンプする。空なら検索解除。
    pub fn set_search(&self, term: &str) -> w::AnyResult<()> {
        *self.inner.search_term.borrow_mut() = term.to_owned();
        if term.is_empty() {
            self.inner.match_line.set(None);
            return self.refresh();
        }
        let start = self.inner.match_line.get().unwrap_or_else(|| self.inner.scroll_top.get());
        if let Some(hit) = self.find_from(start, true) {
            self.jump_to(hit);
        } else {
            self.inner.match_line.set(None);
        }
        self.refresh()
    }

    /// 次（`forward=false` なら前）の一致へ移動する。
    pub fn find_next(&self, forward: bool) -> w::AnyResult<()> {
        if self.inner.search_term.borrow().is_empty() {
            return Ok(());
        }
        let total = self.inner.lines.borrow().len();
        if total == 0 {
            return Ok(());
        }
        let cur = self.inner.match_line.get().unwrap_or_else(|| self.inner.scroll_top.get());
        let start = if forward {
            (cur + 1) % total
        } else {
            (cur + total - 1) % total
        };
        if let Some(hit) = self.find_from(start, forward) {
            self.jump_to(hit);
        }
        self.refresh()
    }

    /// `start` から循環で一致行を探す（大小無視）。
    fn find_from(&self, start: usize, forward: bool) -> Option<usize> {
        let lines = self.inner.lines.borrow();
        let n = lines.len();
        if n == 0 {
            return None;
        }
        let needle = self.inner.search_term.borrow().to_lowercase();
        if needle.is_empty() {
            return None;
        }
        for k in 0..n {
            let i = if forward {
                (start + k) % n
            } else {
                (start + n - k) % n
            };
            if lines[i].body.to_lowercase().contains(&needle) {
                return Some(i);
            }
        }
        None
    }

    /// 指定表示行を可視範囲（できれば上から1/4の位置）へスクロールして強調する。
    fn jump_to(&self, line: usize) {
        self.inner.match_line.set(Some(line));
        let page = self.page_rows();
        let total = self.inner.lines.borrow().len();
        let max_top = total.saturating_sub(page);
        // 一致行が見えていなければ、少し上に余白を取って表示する。
        let top = self.inner.scroll_top.get();
        if line < top || line >= top + page {
            let margin = page / 4;
            let new_top = line.saturating_sub(margin).min(max_top);
            self.inner.scroll_top.set(new_top);
        }
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
        self.create_font_sized(self.inner.font_size)
    }

    fn create_font_sized(&self, size: i32) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
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
            &self.inner.font_family,
        )
    }

    /// 行番号欄の (右端 x, 縦線 x, 本文左 x) を返す（物理 px）。
    fn gutter_geometry(&self) -> (i32, i32, i32) {
        let cwd = self.inner.char_width.get().max(1);
        let g = self.inner.gutter_chars.get().max(4) as i32;
        let num_right = cwd / 2 + g * cwd;
        let sep_x = num_right + cwd / 2;
        let content_left = sep_x + cwd / 2;
        (num_right, sep_x, content_left)
    }

    fn setup_events(&self) {
        crate::winutil::passive_focus(&self.wnd);

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm_mouse_wheel(move |p| {
            let dist = p.keys.raw() as i16;
            this.scroll_by_wheel(dist)?;
            Ok(())
        });

        // マウスで範囲選択（キャレットが無いのでマウス選択が唯一の手段）。
        let this = self.clone();
        self.wnd.on().wm_l_button_down(move |p| {
            let pos = this.point_to_pos(p.coords);
            this.inner.sel_anchor.set(Some(pos));
            this.inner.sel_cursor.set(Some(pos));
            this.inner.selecting.set(true);
            std::mem::forget(this.hwnd().SetCapture());
            this.refresh()?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_move(move |p| {
            if this.inner.selecting.get() {
                let pos = this.point_to_pos(p.coords);
                this.inner.sel_cursor.set(Some(pos));
                this.refresh()?;
            }
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_up(move |_p| {
            if this.inner.selecting.get() {
                this.inner.selecting.set(false);
                drop(this.hwnd().SetCapture());
            }
            Ok(())
        });

        // ダブルクリックでカーソル下の単語を選択する。
        let this = self.clone();
        self.wnd.on().wm_l_button_dbl_clk(move |p| {
            this.select_word_at(p.coords);
            this.refresh()?;
            Ok(())
        });

        // 右クリックでコンテキストメニューを開く（表示は MainWindow が担う）。
        let this = self.clone();
        self.wnd.on().wm_r_button_down(move |p| {
            let screen = this.hwnd().ClientToScreen(p.coords).unwrap_or(p.coords);
            if let Some(cb) = this.inner.on_menu.borrow().as_ref() {
                cb(screen);
            }
            Ok(())
        });
    }

    /// 全選択（先頭から末尾まで）。
    pub fn select_all(&self) {
        let lines = self.inner.lines.borrow();
        if lines.is_empty() {
            return;
        }
        let last = lines.len() - 1;
        let last_col = lines[last].body.chars().count();
        drop(lines);
        self.inner.sel_anchor.set(Some((0, 0)));
        self.inner.sel_cursor.set(Some((last, last_col)));
    }

    /// `pt` 直下の単語を選択する（語＝英数字＋アンダースコアの連なり。その他は1文字）。
    fn select_word_at(&self, pt: w::POINT) {
        let (line, col) = self.point_to_pos(pt);
        let lines = self.inner.lines.borrow();
        let Some(dl) = lines.get(line) else { return };
        let (s, e) = word_bounds(&dl.body, col);
        drop(lines);
        if s == e {
            self.clear_selection();
        } else {
            self.inner.sel_anchor.set(Some((line, s)));
            self.inner.sel_cursor.set(Some((line, e)));
        }
    }

    /// マウス座標を表示行内の位置 (行, char オフセット) へ変換する。
    fn point_to_pos(&self, pt: w::POINT) -> Pos {
        let lines = self.inner.lines.borrow();
        if lines.is_empty() {
            return (0, 0);
        }
        let lh = self.inner.line_height.get().max(1);
        let top = self.inner.scroll_top.get();
        let row = (pt.y.max(0) / lh) as usize;
        let line = (top + row).min(lines.len() - 1);
        let (_, _, content_left) = self.gutter_geometry();
        let cwd = self.inner.char_width.get().max(1);
        let target = ((pt.x - content_left).max(0) + cwd / 2) / cwd; // 最寄り境界（表示セル数）
        let mut acc = 0i32;
        let mut col = 0usize;
        for ch in lines[line].body.chars() {
            let wch = UnicodeWidthChar::width(ch).unwrap_or(0) as i32;
            if target < acc + (wch + 1) / 2 {
                break;
            }
            acc += wch.max(1);
            col += 1;
        }
        (line, col)
    }

    /// content 左端からの char オフセット col の x 座標（物理 px）。
    fn col_x(&self, body: &str, col: usize, content_left: i32, cwd: i32) -> i32 {
        let cells: i32 = body
            .chars()
            .take(col)
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0) as i32)
            .sum();
        content_left + cells * cwd
    }

    /// 正規化した選択範囲（始点 <= 終点）。長さ 0 や未選択なら None。
    fn normalized_selection(&self) -> Option<(Pos, Pos)> {
        let a = self.inner.sel_anchor.get()?;
        let c = self.inner.sel_cursor.get()?;
        if a == c {
            return None;
        }
        Some(if a <= c { (a, c) } else { (c, a) })
    }

    /// 選択範囲のテキストを組み立てる（折返し継続行は改行を入れず論理行を復元）。
    fn selected_text(&self) -> String {
        let Some((s, e)) = self.normalized_selection() else {
            return String::new();
        };
        let lines = self.inner.lines.borrow();
        let mut out = String::new();
        for li in s.0..=e.0 {
            let chars: Vec<char> = lines[li].body.chars().collect();
            let from = if li == s.0 { s.1.min(chars.len()) } else { 0 };
            let to = if li == e.0 { e.1.min(chars.len()) } else { chars.len() };
            if li > s.0 && !lines[li].gutter.is_empty() {
                out.push('\n');
            }
            out.extend(&chars[from.min(to)..to]);
        }
        out
    }

    /// 選択範囲をクリップボードへコピーする（CF_UNICODETEXT）。
    pub fn copy_selection(&self) -> w::AnyResult<()> {
        let text = self.selected_text();
        if text.is_empty() {
            return Ok(());
        }
        let mut u16s: Vec<u16> = text.encode_utf16().collect();
        u16s.push(0);
        let bytes: Vec<u8> = u16s.iter().flat_map(|u| u.to_le_bytes()).collect();
        let clip = self.hwnd().OpenClipboard()?;
        clip.EmptyClipboard()?;
        clip.SetClipboardData(co::CF::UNICODETEXT, &bytes)?;
        Ok(())
    }

    /// 折返し桁を現在のクライアント幅から算出する。
    fn wrap_cols(&self) -> usize {
        let cw = self.hwnd().GetClientRect().map(|r| r.right - r.left).unwrap_or(0);
        let cwd = self.inner.char_width.get().max(1);
        let (_, _, content_left) = self.gutter_geometry();
        let avail = cw - content_left - cwd; // 右に1文字分の余白
        ((avail / cwd).max(1)) as usize
    }

    /// 必要なら表示行を再生成する。
    fn rebuild_if_needed(&self, wrap_cols: usize) {
        if !self.inner.dirty.get() && self.inner.cached_wrap.get() == wrap_cols {
            return;
        }
        let lines = self.inner.model.borrow().lines(wrap_cols, TAB_WIDTH);
        // 行番号/オフセットの桁数。テキストは最低4桁（5桁以上は収まる分に拡張）。
        let gutter_chars = lines.iter().map(|l| l.gutter.chars().count()).max().unwrap_or(1).max(4);
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
        // メトリクス実測（行高・文字幅）。
        if let Ok(tm) = dc.GetTextMetrics() {
            self.inner.line_height.set(tm.tmHeight + gui::dpi_y(2));
            self.inner.char_width.set((tm.tmAveCharWidth).max(1));
        }
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        self.paint_to(dc, cw, ch)
    }

    fn paint_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let colors = self.inner.colors;
        let wrap_cols = self.wrap_cols();
        self.rebuild_if_needed(wrap_cols);

        let body_h = self.body_height();
        let lh = self.inner.line_height.get().max(1);
        let cwd = self.inner.char_width.get().max(1);
        let (num_right, sep_x, content_left) = self.gutter_geometry();
        let sel = self.normalized_selection();

        // 背景。
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.viewer_background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        // 行番号欄とコンテンツを仕切る縦線。
        chrome::vline(dc, sep_x, 0, body_h, rgb(colors.viewer_separator))?;

        // 本文。
        let is_text = self.inner.model.borrow().mode == ViewMode::Text;
        let lines = self.inner.lines.borrow();
        let top = self.inner.scroll_top.get();
        let match_line = self.inner.match_line.get();
        let sel_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.selected_file_bg))?;
        let find_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.viewer_find_bg))?;
        let mut y = 0i32;
        let mut i = top;
        while i < lines.len() && y < body_h {
            let line = &lines[i];
            // マウス選択のハイライト（行内の桁範囲）。選択は検索ハイライトより優先。
            let mut highlighted = false;
            if let Some((s, e)) = sel {
                if i >= s.0 && i <= e.0 {
                    let left = if i == s.0 { self.col_x(&line.body, s.1, content_left, cwd) } else { content_left };
                    let right = if i == e.0 { self.col_x(&line.body, e.1, content_left, cwd) } else { cw };
                    if right > left {
                        dc.FillRect(w::RECT { left, top: y, right, bottom: y + lh }, &sel_brush)?;
                    }
                    highlighted = true;
                }
            }
            let is_match = !highlighted && match_line == Some(i);
            if is_match {
                dc.FillRect(w::RECT { left: content_left, top: y, right: cw, bottom: y + lh }, &find_brush)?;
            }
            if !line.gutter.is_empty() {
                dc.SetTextColor(rgb(colors.viewer_line))?;
                let rect = w::RECT { left: 0, top: y, right: num_right, bottom: y + lh };
                dc.DrawText(&line.gutter, rect, co::DT::SINGLELINE | co::DT::RIGHT | co::DT::NOPREFIX)?;
            }
            if !line.body.is_empty() {
                let col = if is_match { colors.viewer_find_text } else { colors.viewer_text };
                dc.SetTextColor(rgb(col))?;
                let rect = w::RECT { left: content_left, top: y, right: cw, bottom: y + lh };
                dc.DrawText(&line.body, rect, co::DT::SINGLELINE | co::DT::NOPREFIX)?;
            }
            // 行末の改行マーク（記号色・本文とは別レイヤーなので選択・コピーには混ざらない）。
            if let Some(nl) = line.newline {
                let bx = self.col_x(&line.body, line.body.chars().count(), content_left, cwd);
                dc.SetTextColor(rgb(colors.viewer_symbol))?;
                let rect = w::RECT { left: bx, top: y, right: cw, bottom: y + lh };
                dc.DrawText(newline_glyph(nl), rect, co::DT::SINGLELINE | co::DT::NOPREFIX)?;
            }
            y += lh;
            i += 1;
        }

        // テキストモードでは本文末尾に [EOF] マーカーを出す（記号色）。
        if is_text && i == lines.len() {
            if let Some(last) = lines.last() {
                let eof_y = y - lh;
                if eof_y >= 0 {
                    let end_col = last.body.chars().count();
                    // 最終行に改行マークがあれば、その分だけ右にずらして重ねない。
                    let nl_gap = if last.newline.is_some() { cwd * 2 } else { cwd };
                    let x = self.col_x(&last.body, end_col, content_left, cwd) + nl_gap;
                    dc.SetTextColor(rgb(colors.viewer_symbol))?;
                    let rect = w::RECT { left: x, top: eof_y, right: cw, bottom: eof_y + lh };
                    dc.DrawText("[EOF]", rect, co::DT::SINGLELINE | co::DT::NOPREFIX)?;
                }
            }
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
        // 状態行は本文より少し小さいフォントで（パスバー等と同じ流儀）。
        let sfont = self.create_font_sized((self.inner.font_size - 2).max(6))?;
        let _sfont_sel = dc.SelectObject(&*sfont)?;
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
        let find = {
            let term = self.inner.search_term.borrow();
            if term.is_empty() {
                String::new()
            } else {
                let hit = if self.inner.match_line.get().is_some() { "" } else { " (該当なし)" };
                format!("    検索:{}{}", term, hit)
            }
        };
        format!(
            "{}    [{}]  {}    {}/{} 行    (C:エンコ B:ダンプ F:検索 Ctrl+C/右ク:コピー Esc:閉じる){}{}",
            self.inner.title.borrow(),
            model.encoding.label(),
            mode,
            cur.min(total.max(1)),
            total,
            trunc,
            find,
        )
    }

}

fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}

/// 改行種別ごとの表示グリフ（CR=左へ復帰・LF=下へ送り・CRLF=復帰改行）。
fn newline_glyph(e: LineEnding) -> &'static str {
    match e {
        LineEnding::Cr => "←",
        LineEnding::Lf => "↓",
        LineEnding::CrLf => "↵",
    }
}

/// 文字の種別（語・空白・記号）。ダブルクリックは同種の連なりを選ぶ。
#[derive(PartialEq, Eq, Clone, Copy)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Space
    } else {
        CharClass::Other
    }
}

/// `body` の char オフセット `col` 直下の「同種の連なり」の範囲 `[s, e)` を返す。
/// 語（英数字＋アンダースコア）・空白・記号をそれぞれ別種として連なりをまとめる。
fn word_bounds(body: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = body.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let i = col.min(chars.len() - 1);
    let target = char_class(chars[i]);
    let mut s = i;
    while s > 0 && char_class(chars[s - 1]) == target {
        s -= 1;
    }
    let mut e = i + 1;
    while e < chars.len() && char_class(chars[e]) == target {
        e += 1;
    }
    (s, e)
}

#[cfg(test)]
mod tests {
    use super::word_bounds;

    #[test]
    fn word_bounds_selects_word_run() {
        let s = "the quick_brown fox";
        // "the" の中。
        assert_eq!(word_bounds(s, 1), (0, 3));
        // "quick_brown"（アンダースコア込みで1語）。
        assert_eq!(word_bounds(s, 6), (4, 15));
        // "fox" 末尾語。
        assert_eq!(word_bounds(s, 17), (16, 19));
    }

    #[test]
    fn word_bounds_groups_spaces_and_symbols() {
        let s = "a == b";
        // 空白1つ。
        assert_eq!(word_bounds(s, 1), (1, 2));
        // "==" は記号の連なり。
        assert_eq!(word_bounds(s, 3), (2, 4));
    }

    #[test]
    fn word_bounds_handles_edges() {
        assert_eq!(word_bounds("", 0), (0, 0));
        // 範囲外 col は末尾文字へクランプ。
        assert_eq!(word_bounds("ab", 9), (0, 2));
    }
}
