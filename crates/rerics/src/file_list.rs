//! 自前描画のファイル一覧コントロール。
//!
//! 状態は `rerics_core::FileListState` に持たせ、本モジュールは描画・入力・スクロールの
//! GUI 配線に徹する。ダブルバッファでちらつきを抑える。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use rerics_core::{Align, ColumnKind, Colors, Config, FileItem, FileListState, MediaKind, Rgb, SortType};
use winsafe::{self as w, co, gui, prelude::*};

use crate::icons::{ICON_LOGICAL, IconCache};

/// FileItem の更新時刻を per-file アイコンキャッシュのキー用 u64 秒へ。取得不能は 0。
fn item_mtime(it: &FileItem) -> u64 {
    it.modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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

/// content-fit で各非フレックス列に与える上限（クライアント幅に対する割合）。
const AUTOFIT_MAX_RATIO: f64 = 0.25;

/// 列ドラッグ中の状態。
#[derive(Clone, Copy)]
struct HeaderDrag {
    col: usize,
    start_x: i32,
    start_width: i32,
}

struct Inner {
    state: Rc<RefCell<FileListState>>,
    colors: Cell<Colors>,
    font_family: RefCell<String>,
    font_size: Cell<i32>,
    /// 自前スクロールバーの幅（論理 px）。
    scrollbar_width: Cell<i32>,
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
    /// 書庫の読込中はプログレスバーを重ね、一覧の代わりに進捗を表示する。
    loading: Cell<bool>,
    /// 展開済みファイル数／総数（プログレスバーの充填率に使う。total=0 は不定）。
    loading_done: Cell<u64>,
    loading_total: Cell<u64>,
    /// シェルアイコンのキャッシュ（左右ペインで共有・main 側から注入）。未設定なら描かない。
    icon_cache: RefCell<Option<Rc<IconCache>>>,
    /// 現在表示中の実FSディレクトリ（per-file アイコン取得用。書庫内など実体が無ければ None）。
    dir: RefCell<Option<PathBuf>>,
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
            colors: Cell::new(cfg.active_colors()),
            font_family: RefCell::new(cfg.font.family.clone()),
            font_size: Cell::new(cfg.font.size),
            scrollbar_width: Cell::new(cfg.layout.scrollbar_width),
            font_height: Cell::new(gui::dpi_y(cfg.font.size)),
            cursor_visible: Cell::new(false),
            mouse_event: Cell::new(MouseEvent::None),
            header_click_col: Cell::new(None),
            drag: Cell::new(None),
            sb_drag: Cell::new(None),
            on_activate: RefCell::new(None),
            on_got_focus: RefCell::new(None),
            on_wheel: RefCell::new(None),
            loading: Cell::new(false),
            loading_done: Cell::new(0),
            loading_total: Cell::new(0),
            icon_cache: RefCell::new(None),
            dir: RefCell::new(None),
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

    /// シェルアイコンのキャッシュを注入する（左右ペインで同一インスタンスを共有）。
    pub fn set_icon_cache(&self, cache: Rc<IconCache>) {
        *self.inner.icon_cache.borrow_mut() = Some(cache);
    }

    /// 現在表示中の実FSディレクトリを設定する（per-file アイコン取得の基準。書庫内は None）。
    pub fn set_dir(&self, dir: Option<PathBuf>) {
        *self.inner.dir.borrow_mut() = dir;
    }

    /// アイコンの描画サイズ（物理 px・DPI スケール済み）。
    fn icon_px(&self) -> i32 {
        // アイコンは行（フォント基準の高さ）に収まるサイズへ抑える。
        gui::dpi_x(ICON_LOGICAL).min(self.item_height())
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

    /// 読込中プログレスバーを表示開始する（書庫の一括展開待ち等・進捗は 0/0 から）。
    pub fn set_loading(&self) {
        self.inner.loading.set(true);
        self.inner.loading_done.set(0);
        self.inner.loading_total.set(0);
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 進捗（done/total）を更新する（再描画はタイマの `tick_loading` に任せる）。
    pub fn set_loading_progress(&self, done: u64, total: u64) {
        self.inner.loading_done.set(done);
        self.inner.loading_total.set(total);
    }

    /// 読込中表示を終了する。
    pub fn clear_loading(&self) {
        if self.inner.loading.get() {
            self.inner.loading.set(false);
            let _ = self.hwnd().InvalidateRect(None, false);
        }
    }

    pub fn is_loading(&self) -> bool {
        self.inner.loading.get()
    }

    /// 読込中なら再描画して進捗バーを最新にする（取り込みタイマから毎回呼ぶ）。
    pub fn tick_loading(&self) {
        if !self.inner.loading.get() {
            return;
        }
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 設定の配色・フォント・スクロールバー幅を反映して再描画する。
    pub fn apply_config(&self, cfg: &Config) {
        self.inner.colors.set(cfg.active_colors());
        *self.inner.font_family.borrow_mut() = cfg.font.family.clone();
        self.inner.font_size.set(cfg.font.size);
        self.inner.scrollbar_width.set(cfg.layout.scrollbar_width);
        let _ = self.refresh();
    }

    /// 列幅を内容に合わせて自動調整する（content-fit）。
    /// 名前列はフレックス（残り幅）なので測定を飛ばす。可変なのは拡張子列だけで、ここは
    /// 全セルを実測する。サイズ/日時/属性は内容に依らずジャンプしないよう、`column_sample` の
    /// 代表文字列を「平均的な文字 `n` の幅 × 文字数」で測った固定幅にする（プロポーショナルでも安定）。
    /// いずれもヘッダラベル幅を下限とし、物理 px で測って格納先の `width`（論理 px）へ変換する。
    pub fn autofit_columns(&self) -> w::AnyResult<()> {
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        if cw <= 0 {
            return Ok(());
        }
        let dc = self.hwnd().GetDC()?;
        let font = self.create_font()?;
        let _font_sel = dc.SelectObject(&*font)?;
        let dpi = gui::dpi_x(96).max(1);
        let to_logical = |phys: i32| (phys * 96 + dpi / 2) / dpi;
        // 代表幅の基準となる平均的な文字幅。
        let n_w = dc.GetTextExtentPoint32("n").map(|sz| sz.cx).unwrap_or(0);

        let mut s = self.inner.state.borrow_mut();
        let flex = s
            .columns
            .iter()
            .position(|c| matches!(c.kind, ColumnKind::FileName | ColumnKind::FileBaseName));
        let mut measured = vec![0i32; s.columns.len()];
        for (ci, col) in s.columns.iter().enumerate() {
            if Some(ci) == flex {
                continue;
            }
            let header_w = dc.GetTextExtentPoint32(&col.text).map(|sz| sz.cx).unwrap_or(0);
            let sample = rerics_core::column_sample(col.kind);
            let content_w = if sample.is_empty() {
                // 可変列（拡張子）：全セルを実測して最長に合わせる。
                let mut m = 0;
                for item in &s.items {
                    let text = s.cell_text(item, col.kind);
                    if text.is_empty() {
                        continue;
                    }
                    let w = dc.GetTextExtentPoint32(&text).map(|sz| sz.cx).unwrap_or(0);
                    m = m.max(w);
                }
                m
            } else {
                // 固定列：代表文字列の文字数 × 平均文字幅。
                sample.chars().count() as i32 * n_w
            };
            measured[ci] = to_logical(header_w.max(content_w));
        }
        // スクロールバー幅は常時予約する（有無で列幅がガチャガチャしないよう一定に保つ）。
        // フレックス（名前列）は「見える10文字＋左右マージン2文字＝12文字分」を最小幅とし、
        // pane がそれより狭い時は名前列を縮めず、右端の固定列が描画で画面外へはみ出る
        // （カラム幅は不変のまま右から圏外になる）。
        rerics_core::auto_adjust_columns(
            &mut s.columns,
            &measured,
            to_logical(cw),
            self.inner.scrollbar_width.get(),
            to_logical(n_w * 2),
            AUTOFIT_MAX_RATIO,
            to_logical(n_w * 12),
        );
        Ok(())
    }

    /// クライアント高から1ページ行数を算出する。
    /// 解決済みの外見情報（色・フォント・行高など、`paint_to` が読むのと同じ保持値）を JSON で返す。
    /// 設定が描画に反映されているかをピクセルなしでテストするための観測用（デバッグ制御サーバ）。
    /// `to_value` で全フィールドを自動展開するので、色項目が増えても出し忘れない。
    #[cfg(feature = "debug-server")]
    pub(crate) fn presentation(&self) -> serde_json::Value {
        serde_json::json!({
            "colors": serde_json::to_value(self.inner.colors.get()).unwrap_or_default(),
            "font": {
                "family": self.inner.font_family.borrow().clone(),
                "size": self.inner.font_size.get(),
            },
            "header_height": self.header_height(),
            "item_height": self.item_height(),
            "scrollbar_width": self.inner.scrollbar_width.get(),
        })
    }

    /// カーソル行の矩形（自コントロールのクライアント座標 `(x,y,w,h)`）。
    /// カーソルがスクロール範囲外（不可視）なら `None`。デバッグ制御サーバのスナップショット用。
    #[cfg(feature = "debug-server")]
    pub fn cursor_row_rect(&self) -> Option<(i32, i32, i32, i32)> {
        let (cursor, scroll_top) = {
            let s = self.inner.state.borrow();
            (s.cursor, s.scroll_top)
        };
        if cursor < scroll_top {
            return None;
        }
        let ih = self.item_height();
        if ih <= 0 {
            return None;
        }
        let y = self.header_height() + (cursor - scroll_top) as i32 * ih;
        let rc = self.hwnd().GetClientRect().ok()?;
        Some((0, y, rc.right - rc.left, ih))
    }

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

    /// セル左右のテキストマージン（平均的な文字 `n` の幅）。フォント選択済みの DC で測る。
    fn text_margin(dc: &w::HDC) -> i32 {
        dc.GetTextExtentPoint32("n").map(|sz| sz.cx).unwrap_or(4).max(1)
    }

    fn header_height(&self) -> i32 {
        self.font_height() + 8
    }

    fn item_height(&self) -> i32 {
        // 行高はフォント基準で詰める。アイコンは行に収まるサイズへ縮小して描く（icon_px）。
        self.font_height()
    }

    /// フォントを生成する（設定のファミリ・サイズ）。
    fn create_font(&self) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
        w::HFONT::CreateFont(
            w::SIZE { cx: 0, cy: -gui::dpi_y(self.inner.font_size.get()) },
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
        self.wnd.on().wm_get_dlg_code(move |_| {
            let _ = &this;
            let flags = co::DLGC::WANTARROWS.raw() | co::DLGC::WANTALLKEYS.raw();
            Ok(unsafe { co::DLGC::from_raw(flags) })
        });

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm_set_cursor(move |_| Ok(this.on_set_cursor()));

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
        let sbw = gui::dpi_x(self.inner.scrollbar_width.get());
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

    /// ヘッダの列境界上なら横リサイズカーソル（⟷）、それ以外は矢印にする。
    /// winsafe は false を返しても DefWindowProc を呼ばない（＝既定の矢印に戻らない）ため、
    /// 境界外でも明示的に ARROW をセットして常に true（処理済み）を返す。
    fn on_set_cursor(&self) -> bool {
        let on_border =
            self.inner.mouse_event.get() == MouseEvent::HeaderDrag || self.cursor_on_border();
        let idc = if on_border { co::IDC::SIZEWE } else { co::IDC::ARROW };
        if let Ok(mut cur) = w::HINSTANCE::NULL.LoadCursor(w::IdIdcStr::Idc(idc)) {
            unsafe { SetCursor(cur.leak().ptr()) };
        }
        true
    }

    /// 現在のカーソルがヘッダ内の列境界上にあるか。
    fn cursor_on_border(&self) -> bool {
        let Ok(pt) = w::GetCursorPos() else {
            return false;
        };
        let Ok(cp) = self.hwnd().ScreenToClient(pt) else {
            return false;
        };
        cp.y < self.header_height() && self.hit_column_border(cp.x).is_some()
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

    /// 読込中プログレスバーを中央に描く（一覧の代わり）。進捗テキスト＋充填バー。
    fn paint_loading(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let colors = self.inner.colors.get();
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        let done = self.inner.loading_done.get();
        let total = self.inner.loading_total.get();
        // done/total はバックエンド毎に意味が違う（7z=件数・tar=消費バイト）が、割合は共通。
        let text = if total > 0 {
            format!("読込中  {}%", (done.min(total) * 100 / total))
        } else {
            "読込中".to_owned()
        };

        // バー寸法：クライアント幅の 60%（120〜600 でクランプ）×フォント1行高。中央配置。
        let bar_w = (cw * 6 / 10).clamp(120, 600);
        let bar_h = (self.inner.font_height.get()).max(12);
        let bar_x = (cw - bar_w) / 2;
        let bar_y = (ch - bar_h) / 2;

        // 進捗テキストはバーの少し上に中央寄せ。
        dc.SetTextColor(rgb(colors.cursor))?;
        let sz = dc.GetTextExtentPoint32(&text).unwrap_or(w::SIZE { cx: 0, cy: 0 });
        dc.TextOut(((cw - sz.cx) / 2).max(0), (bar_y - sz.cy - 6).max(0), &text)?;

        // 枠（外周を file_normal で塗り）→ 溝（背景2）→ 充填（cursor）の三層。
        let border = w::HBRUSH::CreateSolidBrush(rgb(colors.file_normal))?;
        let track = w::HBRUSH::CreateSolidBrush(rgb(colors.background2))?;
        let fill = w::HBRUSH::CreateSolidBrush(rgb(colors.cursor))?;
        let outer = w::RECT { left: bar_x, top: bar_y, right: bar_x + bar_w, bottom: bar_y + bar_h };
        dc.FillRect(outer, &border)?;
        let inset = w::RECT {
            left: outer.left + 1,
            top: outer.top + 1,
            right: outer.right - 1,
            bottom: outer.bottom - 1,
        };
        dc.FillRect(inset, &track)?;
        if total > 0 {
            let inner_w = inset.right - inset.left;
            let filled = (inner_w as i64 * done as i64 / total as i64) as i32;
            if filled > 0 {
                dc.FillRect(
                    w::RECT { left: inset.left, top: inset.top, right: inset.left + filled, bottom: inset.bottom },
                    &fill,
                )?;
            }
        }
        Ok(())
    }

    /// ターゲットビットマップ選択済みの任意 DC へ全面描画する（フォント準備＋`paint_to`）。
    /// `on_paint` のダブルバッファと、デバッグ制御サーバの窓非依存スナップショットの両方から呼ぶ。
    pub(crate) fn render_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let font = self.create_font()?;
        let _font_sel = dc.SelectObject(&*font)?;
        // フォント高さ実測。
        if let Ok(tm) = dc.GetTextMetrics() {
            self.inner.font_height.set(tm.tmHeight);
        }
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        self.paint_to(dc, cw, ch)
    }

    fn paint_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        if self.inner.loading.get() {
            return self.paint_loading(dc, cw, ch);
        }
        let colors = self.inner.colors.get();
        let cursor_visible = self.inner.cursor_visible.get();
        let header_h = self.header_height();
        let item_h = self.item_height();

        // 1. 背景全面。
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        let margin = Self::text_margin(dc);
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
        // マーク行の背景。非アクティブ側は選択背景を背景方向へ寄せて淡くし、
        // 文字色も通常色へ戻して「どちらのペインがアクティブか」を一目で示す。
        let sel_bg_color = if cursor_visible {
            colors.selected_file_bg
        } else {
            colors.selected_file_bg.blend(colors.background, 3, 5)
        };
        let sel_bg = w::HBRUSH::CreateSolidBrush(rgb(sel_bg_color))?;
        let icon_cache = self.inner.icon_cache.borrow();
        let icon_px = self.icon_px();
        let dir = self.inner.dir.borrow();
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
                if cursor_visible { colors.selected_file } else { colors.item_color(item) }
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
                // 名前列の左にシェルアイコンを内包する（モダン方式・専用列は持たない）。
                let mut text_left = left + margin;
                let is_name_col =
                    matches!(col.kind, ColumnKind::FileName | ColumnKind::FileBaseName);
                if is_name_col {
                    if let Some(cache) = icon_cache.as_ref() {
                        let iy = y + (item_h - icon_px) / 2;
                        let mut drawn = false;
                        // 実FSのファイル（ディレクトリ・親・書庫内を除く）は per-file の固有
                        // アイコン/サムネを試み、未取得なら汎用を描いて非同期取得を依頼する。
                        if !item.is_dir && !item.is_parent {
                            if let Some(d) = dir.as_ref() {
                                let full = d.join(&item.name);
                                let mtime = item_mtime(item);
                                if cache.draw_file(dc, &full, mtime, text_left, iy, icon_px) {
                                    drawn = true;
                                } else {
                                    let thumb = matches!(
                                        MediaKind::from_extension(&item.extension),
                                        Some(MediaKind::Image)
                                    );
                                    cache.request_file(&full, mtime, thumb);
                                }
                            }
                        }
                        if !drawn {
                            cache.draw_generic(
                                dc, item.is_dir, &item.extension, text_left, iy, icon_px,
                            );
                        }
                        text_left += icon_px + gui::dpi_x(2);
                    }
                }
                // 左は n 幅マージン（＋アイコン幅）、右パディングは 0（原作の左 4/右 0 に倣う）。
                let rect = w::RECT { left: text_left, top: y, right, bottom: y + item_h };
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
        // トラック（溝）は常時描画して列幅予約分を「スクロールバーの定位置」に見せ、見た目を
        // 安定させる。thumb はスクロール可能な時だけ出す。
        let sbw = gui::dpi_x(self.inner.scrollbar_width.get());
        let bar_x = cw - sbw;
        let track_top = header_h;
        let track_h = ch - track_top;
        if track_h > 0 {
            let track_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.background2))?;
            dc.FillRect(
                w::RECT { left: bar_x, top: track_top, right: cw, bottom: ch },
                &track_brush,
            )?;
            let count = s.count();
            if count > page {
                let min_thumb = gui::dpi_y(16);
                let thumb_h = ((track_h * page as i32) / count as i32).max(min_thumb).min(track_h);
                let max_top = count - page;
                let pos = s.scroll_top.min(max_top);
                let thumb_top = track_top + ((track_h - thumb_h) * pos as i32) / max_top as i32;
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
        let margin = Self::text_margin(dc);
        dc.SetTextColor(wtext)?;
        if !col.text.is_empty() {
            let rect = w::RECT { left: left + margin, top: 4, right: right - margin, bottom: header_h };
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
            let x0 = left + margin + tw + 8;
            // 線分描画ではなく三角グリフを文字として描く（昇順=△ 上向き／降順=▽ 下向き）。
            let glyph = if s.sort_reverse { "▽" } else { "△" };
            let rect = w::RECT { left: x0, top: 0, right, bottom: header_h };
            dc.DrawText(glyph, rect, co::DT::SINGLELINE | co::DT::NOPREFIX | co::DT::VCENTER)?;
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

// winsafe 0.0.27 は `SetCursor` を公開していないので生 FFI で叩く（`SetSystemCursor` は別物）。
unsafe extern "system" {
    fn SetCursor(hcursor: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

/// `Rgb` を COLORREF へ変換する。
fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}
