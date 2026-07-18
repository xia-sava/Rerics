//! 自前描画のファイル一覧コントロール。
//!
//! 状態は `rerics_core::FileListState` に持たせ、本モジュールは描画・入力・スクロールの
//! GUI 配線に徹する。ダブルバッファでちらつきを抑える。

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use rerics_core::{
    Align, ColumnKind, Colors, Config, FileItem, FileListState, IconSize, MediaKind, Matcher, Rgb,
    SizeFormat, SortType, Spinner,
};
use winsafe::{self as w, co, gui, prelude::*};

use crate::font_fallback::FontSet;
use crate::icons::{ICON_LOGICAL, IconBox, IconCache};

/// サムネイル表示時、画像でないファイルのシェルアイコンを描く一辺の上限（論理 px）。
/// 大アイコン相当に抑えてサムネイル枠の中央へ置く。
const THUMB_SHELL_ICON_LOGICAL: i32 = 32;

/// サムネイル表示の行間の隙間（物理 px）。画像を行ピッチより，この分だけ小さく描く。
const THUMB_ROW_GAP_PX: i32 = 1;

/// ヘッダのソート三角を本文フォントサイズに対して縮小する割合（%）。
const SORT_GLYPH_SIZE_PERCENT: i32 = 70;

/// ヘッダのソート三角の最小フォントサイズ（論理 px）。
const SORT_GLYPH_MIN_SIZE: i32 = 6;

/// 既定ソートと異なるソート中に三角へ付ける強調色（朱）。ヘッダはシステム色ベースで
/// テーマの影響を受けないため固定色とする。
const SORT_GLYPH_HIGHLIGHT_RGB: (u8, u8, u8) = (208, 64, 0);

/// FileItem の更新時刻を per-file アイコンキャッシュのキー用 u64 秒へ。取得不能は 0。
fn item_mtime(it: &FileItem) -> u64 {
    it.modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// パスの先頭がドライブレター（`C:` 等）ならその文字（大文字化）を返す。UNC 等は None。
fn drive_letter(p: &Path) -> Option<u8> {
    use std::path::{Component, Prefix};
    match p.components().next() {
        Some(Component::Prefix(pfx)) => match pfx.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some(d.to_ascii_uppercase()),
            _ => None,
        },
        _ => None,
    }
}

/// 転送元・転送先が同一ドライブか（判定不能や UNC 等は false＝別ドライブ扱い）。
fn same_drive(a: &Path, b: &Path) -> bool {
    match (drive_letter(a), drive_letter(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// D&D の既定コピー/移動判定（Explorer 準拠）。Ctrl で強制コピー・Shift で強制移動、
/// 無指定は同一ドライブなら既定「移動」・別ドライブなら既定「コピー」。
fn resolve_move(sources: &[PathBuf], dst_dir: &Path, keys: co::MK) -> bool {
    if keys.has(co::MK::CONTROL) {
        return false;
    }
    if keys.has(co::MK::SHIFT) {
        return true;
    }
    sources.first().is_some_and(|src| same_drive(src, dst_dir))
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
type SelectionCb = Box<dyn Fn(u64, u64)>;
type DragQueryCb = Box<dyn Fn() -> Vec<PathBuf>>;
type DropCb = Box<dyn Fn(Vec<PathBuf>, PathBuf, bool)>;

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
    font_fallback: RefCell<Vec<String>>,
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
    /// 選択（マーク）状態が変わったときの通知先（件数と合計サイズを渡す）。
    on_selection_changed: RefCell<Option<SelectionCb>>,
    /// 直近に通知した選択サマリ (件数, 合計サイズ)。refresh 時に導出し、変化時だけ着火する。
    last_selection: Cell<Option<(u64, u64)>>,
    /// 読込・展開の待機表示。`Some` の間は一覧の代わりに待機スピナーを重ねる。
    loading: RefCell<Option<Spinner>>,
    /// 待機スピナーを出すまでの遅延（設定）。これより速い読込はチラつかせない。
    progress_delay: Cell<Duration>,
    /// 非同期読込の世代。新しい読込・タブ切替で進め、古い結果の取り込みを弾く。
    load_gen: Cell<u64>,
    /// シェルアイコンのキャッシュ（左右ペインで共有・main 側から注入）。未設定なら描かない。
    icon_cache: RefCell<Option<Rc<IconCache>>>,
    /// アイコンを一覧に表示するか（設定）。
    icon_show: Cell<bool>,
    /// アイコンの表示サイズ（設定）。
    icon_size: Cell<IconSize>,
    /// サムネイル表示（行高を広げ画像を大きく見せる）が有効か。ペインごとに独立。
    thumbnail: Cell<bool>,
    /// サムネイル表示時の行高＝サムネイルの一辺（論理 px・設定）。
    thumbnail_size: Cell<i32>,
    /// ファイルサイズ列の表記スタイル（設定）。
    size_format: Cell<SizeFormat>,
    /// 列幅を内容に合わせて自動調整するか（設定）。off なら設定幅を保つ。
    auto_adjust: Cell<bool>,
    /// 一覧セルの文字間隔（設定・論理 px・負で詰める）。描画と幅実測の両方へ効かせる。
    char_spacing: Cell<i32>,
    /// 既定ソート（設定）。現在ソートとの一致判定でヘッダのソート三角の強調色を決める。
    default_sort: Cell<SortType>,
    default_sort_reverse: Cell<bool>,
    /// 現在表示中の実FSディレクトリ（per-file アイコン取得用。書庫内など実体が無ければ None）。
    dir: RefCell<Option<PathBuf>>,
    /// 切り詰めセルの全文を出す hover ツールチップ部品（生成後に設定）。
    cell_tip: RefCell<Option<crate::winutil::CellTooltip>>,
    /// マウス左ボタン押下点（クライアント座標）＝ドラッグ開始検出の保留状態。ヘッダ／
    /// スクロールバーのドラッグとは独立（無修飾の行クリックでのみ立つ）。
    drag_start_pt: Cell<Option<w::POINT>>,
    /// OLE ドラッグ実行中フラグ（`DoDragDrop` はブロッキングなので再入防止に使う）。
    dragging: Cell<bool>,
    /// ドラッグ開始時、送信する絶対パスを問い合わせるコールバック。
    on_drag_query: RefCell<Option<DragQueryCb>>,
    /// ドロップ確定時のコールバック（絶対パス列・ドロップ先ディレクトリ・移動か）。
    on_drop: RefCell<Option<DropCb>>,
    /// 登録した OLE ドロップターゲット。生存させ続けるために保持するだけで参照はしない。
    drop_target: RefCell<Option<w::IDropTarget>>,
    /// DragEnter で読み取ったドラッグ元パス。DragOver は IDataObject を渡さないのでキャッシュする。
    drag_over_sources: RefCell<Vec<PathBuf>>,
    /// ドロップ先としてハイライトする行（ホバー中のディレクトリ行）。
    drop_hover_row: Cell<Option<usize>>,
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
            font_fallback: RefCell::new(cfg.font.fallback.clone()),
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
            on_selection_changed: RefCell::new(None),
            last_selection: Cell::new(None),
            loading: RefCell::new(None),
            progress_delay: Cell::new(Duration::from_millis(cfg.progress_delay_ms)),
            load_gen: Cell::new(0),
            icon_cache: RefCell::new(None),
            icon_show: Cell::new(cfg.icons.show),
            icon_size: Cell::new(cfg.icons.size),
            thumbnail: Cell::new(false),
            thumbnail_size: Cell::new(cfg.icons.thumbnail_size),
            size_format: Cell::new(cfg.size_format),
            auto_adjust: Cell::new(cfg.auto_adjust_columns),
            char_spacing: Cell::new(cfg.char_spacing_px),
            default_sort: Cell::new(cfg.default_sort),
            default_sort_reverse: Cell::new(cfg.default_sort_reverse),
            dir: RefCell::new(None),
            cell_tip: RefCell::new(None),
            drag_start_pt: Cell::new(None),
            dragging: Cell::new(false),
            on_drag_query: RefCell::new(None),
            on_drop: RefCell::new(None),
            drop_target: RefCell::new(None),
            drag_over_sources: RefCell::new(Vec::new()),
            drop_hover_row: Cell::new(None),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me.install_drop_target();
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

    /// アイコンを一覧に表示するか（設定 ON かつキャッシュ注入済み）。
    fn icons_visible(&self) -> bool {
        self.inner.icon_show.get() && self.inner.icon_cache.borrow().is_some()
    }

    /// サムネイル表示の行ピッチ＝サムネイルの一辺（物理 px・DPI スケール済み）。
    fn thumbnail_box_px(&self) -> i32 {
        gui::dpi_x(self.inner.thumbnail_size.get().max(1))
    }

    /// アイコンの描画サイズ（物理 px・DPI スケール済み）。
    fn icon_px(&self) -> i32 {
        if self.inner.thumbnail.get() {
            // サムネイル表示：画像は行ピッチより `THUMB_ROW_GAP_PX` 小さく描く。下に隙間が
            // 残り、行と行が地続きにならず読みやすくなる。
            return (self.thumbnail_box_px() - THUMB_ROW_GAP_PX).max(1);
        }
        match self.inner.icon_size.get().logical_px() {
            // 自動：行（フォント基準の高さ）に収まるサイズへ抑える。
            0 => gui::dpi_x(ICON_LOGICAL).min(self.font_height()),
            logical => gui::dpi_x(logical),
        }
    }

    /// シェルアイコンを描く一辺の上限（px）。サムネイル表示では枠が大きいので、画像以外の
    /// シェルアイコンは大アイコン相当に抑えて枠の中央へ置く（引き伸ばしのぼやけを避ける）。
    /// 通常表示では枠サイズと同じで、従来どおり枠いっぱいに描く。
    fn icon_cap(&self) -> i32 {
        if self.inner.thumbnail.get() {
            gui::dpi_x(THUMB_SHELL_ICON_LOGICAL)
        } else {
            self.icon_px()
        }
    }

    pub fn on_activate(&self, cb: impl Fn(usize) + 'static) {
        *self.inner.on_activate.borrow_mut() = Some(Box::new(cb));
    }

    /// フォーカス取得時のコールバック（反対ペインのカーソル消去の配線用）。
    pub fn on_got_focus(&self, cb: impl Fn() + 'static) {
        *self.inner.on_got_focus.borrow_mut() = Some(Box::new(cb));
    }

    /// 選択（マーク）状態が変わったときのコールバック（引数は件数と合計サイズ）。`refresh` を
    /// choke point に、前回通知値と異なるときだけ着火する（スクロール等での空振りを避ける）。
    pub fn on_selection_changed(&self, cb: impl Fn(u64, u64) + 'static) {
        *self.inner.on_selection_changed.borrow_mut() = Some(Box::new(cb));
    }

    /// ホイール回転時のコールバック（回転量と画面座標を渡す）。設定すると自前スクロールの
    /// 代わりにこれが呼ばれ、呼び出し側がカーソル下のペインを判定してスクロールする。
    pub fn on_wheel(&self, cb: impl Fn(i16, w::POINT) + 'static) {
        *self.inner.on_wheel.borrow_mut() = Some(Box::new(cb));
    }

    /// ドラッグ開始時、送信する絶対パスを問い合わせるコールバック。
    pub fn on_drag_query(&self, cb: impl Fn() -> Vec<PathBuf> + 'static) {
        *self.inner.on_drag_query.borrow_mut() = Some(Box::new(cb));
    }

    /// ドロップ確定時のコールバック（絶対パス列・ドロップ先ディレクトリ・移動か）。
    pub fn on_drop(&self, cb: impl Fn(Vec<PathBuf>, PathBuf, bool) + 'static) {
        *self.inner.on_drop.borrow_mut() = Some(Box::new(cb));
    }

    /// カーソル下線の表示/非表示を切り替える。
    pub fn set_cursor_visible(&self, visible: bool) {
        if self.inner.cursor_visible.get() != visible {
            self.inner.cursor_visible.set(visible);
            let _ = self.invalidate_only();
        }
    }

    /// 再描画だけを促す（選択変更の通知はしない）。カーソル下線・スピナーのような選択に
    /// 無関係な見た目の更新で使う。選択が変わりうる操作は [`refresh`](Self::refresh) を通し、
    /// ステータスバーの選択件数がサイレントに古くならないようにする。
    fn invalidate_only(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, false)?;
        Ok(())
    }

    /// 再描画を促し、選択サマリが変わっていればステータスバーへ通知する。
    pub fn refresh(&self) -> w::AnyResult<()> {
        self.invalidate_only()?;
        self.notify_selection_changed();
        Ok(())
    }

    /// 現在の選択サマリ (件数, 合計サイズ) を状態から導出し、前回通知値と異なれば
    /// `on_selection_changed` を撃つ。選択を変える全経路が最後に `refresh` を通るので、
    /// ここを唯一の通知点にできる（コマンド・マウス・スクリプトを問わず一箇所で拾う）。
    fn notify_selection_changed(&self) {
        let Ok(state) = self.inner.state.try_borrow() else {
            return;
        };
        let summary = state.selected_count_size();
        drop(state);
        if self.inner.last_selection.get() == Some(summary) {
            return;
        }
        self.inner.last_selection.set(Some(summary));
        if let Some(cb) = self.inner.on_selection_changed.borrow().as_ref() {
            cb(summary.0, summary.1);
        }
    }

    /// サムネイル表示（行高を広げ画像を大きく見せる）を切り替える。切替後の有効状態を返す。
    /// 行高が変わると 1 ページに収まる行数も変わるので、測り直してスクロール位置を範囲内へ
    /// 収めてから再描画する。
    pub fn toggle_thumbnail(&self) -> bool {
        let on = !self.inner.thumbnail.get();
        self.inner.thumbnail.set(on);
        let pr = self.page_rows();
        {
            let mut s = self.inner.state.borrow_mut();
            let top = s.scroll_top as isize;
            s.set_scroll_top(top, pr);
        }
        let _ = self.refresh();
        on
    }

    /// 待機スピナーを仕込む（読込・展開の共通）。設定の遅延（`progress_delay`）を過ぎてから
    /// 表示するので、それより速く終わる処理ではスピナーを出さず一覧がそのまま差し替わる
    /// ＝チラつかない。遅延 0 なら即時表示。
    pub fn set_loading(&self) {
        *self.inner.loading.borrow_mut() = Some(Spinner::with_delay(self.inner.progress_delay.get()));
        let _ = self.invalidate_only();
    }

    /// 非同期読込の世代を1つ進めて新しい世代値を返す（読込開始・タブ切替で呼ぶ）。
    pub fn bump_load_gen(&self) -> u64 {
        let g = self.inner.load_gen.get().wrapping_add(1);
        self.inner.load_gen.set(g);
        g
    }

    /// 現在の読込世代。取り込み時にこれと一致しない結果は古いので捨てる。
    pub fn load_gen(&self) -> u64 {
        self.inner.load_gen.get()
    }

    /// 読込中表示を終了する。
    pub fn clear_loading(&self) {
        if self.inner.loading.borrow_mut().take().is_some() {
            let _ = self.invalidate_only();
        }
    }

    pub fn is_loading(&self) -> bool {
        self.inner.loading.borrow().is_some()
    }

    /// 読込中ならコマを進めて再描画する（取り込みタイマから毎回呼ぶ）。
    pub fn tick_loading(&self) {
        match self.inner.loading.borrow_mut().as_mut() {
            Some(s) => s.tick(),
            None => return,
        }
        let _ = self.invalidate_only();
    }

    /// 設定の配色・フォント・スクロールバー幅を反映して再描画する。
    pub fn apply_config(&self, cfg: &Config) {
        let old_default =
            (self.inner.default_sort.get(), self.inner.default_sort_reverse.get());
        self.inner.colors.set(cfg.active_colors());
        *self.inner.font_family.borrow_mut() = cfg.font.family.clone();
        *self.inner.font_fallback.borrow_mut() = cfg.font.fallback.clone();
        self.inner.font_size.set(cfg.font.size);
        self.inner.scrollbar_width.set(cfg.layout.scrollbar_width);
        self.inner.icon_show.set(cfg.icons.show);
        self.inner.icon_size.set(cfg.icons.size);
        self.inner.thumbnail_size.set(cfg.icons.thumbnail_size);
        self.inner.size_format.set(cfg.size_format);
        self.inner.auto_adjust.set(cfg.auto_adjust_columns);
        self.inner.char_spacing.set(cfg.char_spacing_px);
        self.inner.default_sort.set(cfg.default_sort);
        self.inner.default_sort_reverse.set(cfg.default_sort_reverse);
        self.inner.progress_delay.set(Duration::from_millis(cfg.progress_delay_ms));
        // 列構成をライブ反映（表示中ペイン）。幅は自動調整 on なら autofit が、off なら設定値が効く。
        {
            let mut s = self.inner.state.borrow_mut();
            s.columns = cfg.columns.clone();
            // 日付ソート反転の切替と、既定のまま使っているペインの新既定への追従。
            s.apply_sort_config_change(old_default, cfg, self.page_rows());
        }
        let _ = self.autofit_columns();
        let _ = self.refresh();
    }

    /// 列幅を内容に合わせて自動調整する（content-fit）。
    /// 名前列はフレックス（残り幅）なので測定を飛ばす。可変なのは拡張子列だけで、ここは
    /// 全セルを実測する。サイズ/日時/属性は内容に依らずジャンプしないよう、`column_sample` の
    /// 代表文字列を「平均的な文字 `n` の幅 × 文字数」で測った固定幅にする（プロポーショナルでも安定）。
    /// いずれもヘッダラベル幅を下限とし、物理 px で測って格納先の `width`（論理 px）へ変換する。
    pub fn autofit_columns(&self) -> w::AnyResult<()> {
        // 自動調整 off なら設定された列幅をそのまま使う（測定・上書きしない）。
        if !self.inner.auto_adjust.get() {
            return Ok(());
        }
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        if cw <= 0 {
            return Ok(());
        }
        let dc = self.hwnd().GetDC()?;
        let fonts = self.create_fonts()?;
        let _font_sel = dc.SelectObject(fonts.primary())?;
        crate::winutil::set_char_spacing(&dc, self.inner.char_spacing.get());
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
                    let text = s.cell_text(item, col.kind, self.inner.size_format.get());
                    if text.is_empty() {
                        continue;
                    }
                    m = m.max(fonts.width(&dc, &text));
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
                "fallback": self.inner.font_fallback.borrow().clone(),
                "size": self.inner.font_size.get(),
            },
            "header_height": self.header_height(),
            "item_height": self.item_height(),
            "scrollbar_width": self.inner.scrollbar_width.get(),
            "thumbnail": self.inner.thumbnail.get(),
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

    /// メニューを開くときのアンカー画面座標（カーソル行の左下）。カーソルが不可視なら
    /// クライアント先頭付近へ落とす。`Menu` コマンドをキーで起動したときのポップアップ位置。
    pub fn menu_anchor(&self) -> w::POINT {
        let ih = self.item_height().max(1);
        let (cursor, scroll_top) = {
            let s = self.inner.state.borrow();
            (s.cursor, s.scroll_top)
        };
        let y = if cursor >= scroll_top {
            self.header_height() + (cursor - scroll_top) as i32 * ih + ih
        } else {
            self.header_height()
        };
        self.hwnd()
            .ClientToScreen(w::POINT { x: 8, y })
            .unwrap_or(w::POINT { x: 8, y })
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
        // 行高はフォント基準で詰める。アイコンを表示中で、かつアイコンがフォントより
        // 大きい（中/大サイズを選んだ）ときだけ、その分だけ行を伸ばす。
        let base = self.font_height();
        if !self.icons_visible() {
            return base;
        }
        if self.inner.thumbnail.get() {
            // サムネイル表示の行ピッチはサムネイルの一辺。画像はこれより小さく描く（icon_px）
            // ので、差のぶんが行間の隙間になる。
            return self.thumbnail_box_px();
        }
        base.max(self.icon_px())
    }

    /// 指定ファミリ・サイズのフォントを生成する（他の条件は一覧の描画用で固定）。
    fn create_font_family(
        &self,
        family: &str,
        size: i32,
    ) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
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
            family,
        )
    }

    /// 設定のファミリ＋フォールバックのフォント一式を生成する。
    fn create_fonts(&self) -> w::SysResult<FontSet> {
        let main = self.inner.font_size.get();
        FontSet::new(
            &self.inner.font_family.borrow(),
            &self.inner.font_fallback.borrow(),
            |family, size| {
                self.create_font_family(family, crate::font_fallback::effective_size(size, main, main))
            },
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
            this.on_mouse_move(p.coords, p.vkey_code)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_dbl_clk(move |p| {
            this.on_l_button_dbl_clk(p.coords)?;
            Ok(())
        });

        // 切り詰めセルの全文を hover で見せる共通部品。on_mouse_move は既存ハンドラ（列リサイズ等）の
        // 末尾から呼ぶので、ここでは leave/timer だけ配線して部品を保持する。
        let cttip = {
            let this = self.clone();
            crate::winutil::CellTooltip::new(move |pt| this.cell_tooltip_at(pt))
        };
        *self.inner.cell_tip.borrow_mut() = Some(cttip.clone());
        let tip = cttip.clone();
        let this = self.clone();
        self.wnd.on().wm_timer(crate::winutil::CellTooltip::TIMER_ID, move || {
            tip.on_timer(this.hwnd());
            Ok(())
        });
        let this = self.clone();
        self.wnd.on().wm_mouse_leave(move || {
            cttip.on_mouse_leave(this.hwnd());
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
        let notches = distance as i32 / 120;
        {
            let mut s = self.inner.state.borrow_mut();
            let pr = self.page_rows();
            let lines = wheel_lines(notches, os_wheel_scroll_lines(), pr);
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

    /// クライアント座標からドロップ先ディレクトリとホバー行を求める。`dir`（現在表示中の
    /// 実FSディレクトリ）が None（書庫内等）ならドロップ不可＝ None。ディレクトリ行の上なら
    /// その中へ、それ以外（`..` 行・ファイル行・一覧の空欄）は現在地へ。
    fn drop_target_at(&self, client_pt: w::POINT) -> Option<(PathBuf, Option<usize>)> {
        let dir = self.inner.dir.borrow().clone()?;
        match self.row_at(client_pt.y) {
            Some(row) => {
                let s = self.inner.state.borrow();
                match s.items.get(row) {
                    Some(item) if item.is_dir && !item.is_parent => {
                        Some((dir.join(&item.name), Some(row)))
                    }
                    _ => Some((dir, None)),
                }
            }
            None => Some((dir, None)),
        }
    }

    /// ホバー行を更新し、変化していれば再描画する。
    fn set_drop_hover(&self, row: Option<usize>) {
        if self.inner.drop_hover_row.get() != row {
            self.inner.drop_hover_row.set(row);
            let _ = self.invalidate_only();
        }
    }

    fn clear_drop_hover(&self) {
        self.set_drop_hover(None);
    }

    /// `DragEnter`/`DragOver` 共通：カーソル位置からドロップ先・効果（コピー/移動/不可）を
    /// 決め、`pdwEffect` へ書き戻す。ホバー行のハイライトも更新する。転送元と転送先が
    /// 同一（親ディレクトリが一致）なら意味の無い操作なので不可にする。
    fn update_drop_feedback(&self, screen_pt: w::POINT, keys: co::MK, effect: &mut co::DROPEFFECT) {
        *effect = co::DROPEFFECT::NONE;
        let Ok(client_pt) = self.hwnd().ScreenToClient(screen_pt) else {
            self.clear_drop_hover();
            return;
        };
        let Some((dst_dir, hover_row)) = self.drop_target_at(client_pt) else {
            self.clear_drop_hover();
            return;
        };
        let sources = self.inner.drag_over_sources.borrow().clone();
        let no_op = sources.is_empty()
            || sources.iter().any(|s| *s == dst_dir || s.parent() == Some(dst_dir.as_path()));
        if no_op {
            self.clear_drop_hover();
            return;
        }
        let move_it = resolve_move(&sources, &dst_dir, keys);
        *effect = if move_it { co::DROPEFFECT::MOVE } else { co::DROPEFFECT::COPY };
        self.set_drop_hover(hover_row);
    }

    /// `Drop` 確定：`update_drop_feedback` と同じ判定で転送先・効果を決め、`on_drop`
    /// コールバックへ渡す。実転送（コピー/移動）は呼び出し側（`fileops`）が行う。
    fn finish_drop(&self, sources: Vec<PathBuf>, screen_pt: w::POINT, keys: co::MK, effect: &mut co::DROPEFFECT) {
        *effect = co::DROPEFFECT::NONE;
        self.clear_drop_hover();
        let Ok(client_pt) = self.hwnd().ScreenToClient(screen_pt) else {
            return;
        };
        let Some((dst_dir, _)) = self.drop_target_at(client_pt) else {
            return;
        };
        let no_op = sources.is_empty()
            || sources.iter().any(|s| *s == dst_dir || s.parent() == Some(dst_dir.as_path()));
        if no_op {
            return;
        }
        let move_it = resolve_move(&sources, &dst_dir, keys);
        *effect = if move_it { co::DROPEFFECT::MOVE } else { co::DROPEFFECT::COPY };
        if let Some(cb) = self.inner.on_drop.borrow().as_ref() {
            cb(sources, dst_dir, move_it);
        }
    }

    /// OLE ドロップターゲットとして自身の HWND を登録する。`OleInitialize` 済み前提
    /// （呼び出し順はアプリ起動時に保証する）。登録失敗時は静かに諦める＝この一覧では
    /// ドロップ受付が効かないだけで、他の動作には影響しない。
    fn install_drop_target(&self) {
        let target = w::IDropTarget::new_impl();

        let this = self.clone();
        target.DragEnter(move |data, keys, pt, effect| {
            *this.inner.drag_over_sources.borrow_mut() = crate::dnd::hdrop_paths(data);
            this.update_drop_feedback(pt, keys, effect);
            Ok(())
        });

        let this = self.clone();
        target.DragOver(move |keys, pt, effect| {
            this.update_drop_feedback(pt, keys, effect);
            Ok(())
        });

        let this = self.clone();
        target.DragLeave(move || {
            this.inner.drag_over_sources.borrow_mut().clear();
            this.clear_drop_hover();
            Ok(())
        });

        let this = self.clone();
        target.Drop(move |data, keys, pt, effect| {
            let sources = crate::dnd::hdrop_paths(data);
            this.inner.drag_over_sources.borrow_mut().clear();
            this.finish_drop(sources, pt, keys, effect);
            Ok(())
        });

        if self.hwnd().RegisterDragDrop(&target).is_ok() {
            *self.inner.drop_target.borrow_mut() = Some(target);
        }
    }

    /// 送信側 OLE ドラッグを開始する（`DoDragDrop` はブロッキング）。送るパスが無ければ
    /// 何もしない。実転送は受け側（自ペイン／他ペイン／外部アプリ）の `Drop` が担う。
    fn start_drag(&self) {
        if self.inner.dragging.get() {
            return;
        }
        let paths = match self.inner.on_drag_query.borrow().as_ref() {
            Some(cb) => cb(),
            None => Vec::new(),
        };
        if paths.is_empty() {
            return;
        }
        self.inner.dragging.set(true);
        let _ = crate::dnd::begin_drag(&paths);
        self.inner.dragging.set(false);
    }

    fn on_l_button_down(&self, pt: w::POINT, keys: co::MK) -> w::AnyResult<()> {
        // クリックされたペインをアクティブにする（Win32 フォーカスは取らず内部状態のみ更新）。
        if let Some(cb) = self.inner.on_got_focus.borrow().as_ref() {
            cb();
        }
        let rc = self.hwnd().GetClientRect()?;
        let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
        if let Some((bar_x, _track_top, _track_h, thumb_top, thumb_h)) = self.scrollbar_geom(cw, ch)
            && pt.x >= bar_x {
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
        // 無修飾クリックのみドラッグ開始候補にする（Ctrl/Shift はマーク操作を優先）。
        // 既に選択済みの行なら（複数選択の場合）マークはそのまま保つので、
        // まとめてドラッグできる。
        if !ctrl && !shift && !self.inner.state.borrow().items[idx].is_parent {
            self.inner.drag_start_pt.set(Some(pt));
        }
        self.refresh()?;
        Ok(())
    }

    fn on_l_button_up(&self, _pt: w::POINT) -> w::AnyResult<()> {
        self.inner.drag_start_pt.set(None);
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

    fn on_mouse_move(&self, pt: w::POINT, keys: co::MK) -> w::AnyResult<()> {
        if let Some(start) = self.inner.drag_start_pt.get() {
            if !keys.has(co::MK::LBUTTON) {
                self.inner.drag_start_pt.set(None);
            } else {
                let cx = w::GetSystemMetrics(co::SM::CXDRAG).max(1);
                let cy = w::GetSystemMetrics(co::SM::CYDRAG).max(1);
                if (pt.x - start.x).abs() >= cx || (pt.y - start.y).abs() >= cy {
                    self.inner.drag_start_pt.set(None);
                    self.start_drag();
                    return Ok(());
                }
            }
        }
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
        if self.inner.mouse_event.get() == MouseEvent::HeaderDrag
            && let Some(d) = self.inner.drag.get() {
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
        if let Some(tip) = self.inner.cell_tip.borrow().clone() {
            tip.on_mouse_move(self.hwnd(), pt);
        }
        Ok(())
    }

    /// マウス位置（クライアント座標）→ 切り詰めで全文が見えていないセルの (矩形, 全文)。それ以外は
    /// `None`。hover ツールチップの resolver＝描画（`paint_to`）と同じ列左端・左マージン・名前列の
    /// アイコン幅・フォントで実測して切り詰めを判定する。
    fn cell_tooltip_at(&self, pt: w::POINT) -> Option<(w::RECT, String)> {
        let hh = self.header_height();
        let ih = self.item_height();
        if pt.y < hh || ih <= 0 {
            return None;
        }
        let s = self.inner.state.borrow();
        let row = s.scroll_top + ((pt.y - hh) / ih) as usize;
        if row >= s.count() {
            return None;
        }
        let mut left = 0i32;
        let mut hit = None;
        for (ci, col) in s.columns.iter().enumerate() {
            let right = left + gui::dpi_x(col.width);
            if pt.x >= left && pt.x < right {
                hit = Some((ci, left, right));
                break;
            }
            left = right;
        }
        let (ci, left, right) = hit?;
        let kind = s.columns[ci].kind;
        let item = &s.items[row];
        let mut text = s.cell_text(item, kind, self.inner.size_format.get());
        // 名前列のリンクはリンク先込みで全文を出す（描画が薄色で添える内容と揃える）。
        if matches!(kind, ColumnKind::FileName | ColumnKind::FileBaseName)
            && let Some(target) = &item.link_target
        {
            text = format!("{text} → {target}");
        }
        if text.is_empty() {
            return None;
        }
        let Ok(dc) = self.hwnd().GetDC() else { return None };
        let Ok(fonts) = self.create_fonts() else { return None };
        let Ok(_sel) = dc.SelectObject(fonts.primary()) else { return None };
        crate::winutil::set_char_spacing(&dc, self.inner.char_spacing.get());
        let mut text_left = left + Self::text_margin(&dc);
        if matches!(kind, ColumnKind::FileName | ColumnKind::FileBaseName) && self.icons_visible() {
            text_left += self.icon_px() + gui::dpi_x(2);
        }
        let avail = right - text_left;
        let truncated = avail <= 0 || fonts.width(&dc, &text) > avail;
        if !truncated {
            return None;
        }
        let y = hh + ((row - s.scroll_top) as i32) * ih;
        Some((w::RECT { left, top: y, right, bottom: y + ih }, text))
    }

    /// 指定セル（`row`＝行・`col`＝列）の中心のクライアント座標（debug 駆動用）。表示範囲外は `None`。
    #[cfg(feature = "debug-server")]
    fn cell_point(&self, row: usize, col: usize) -> Option<w::POINT> {
        let s = self.inner.state.borrow();
        if col >= s.columns.len() || row < s.scroll_top || row >= s.count() {
            return None;
        }
        let vi = row - s.scroll_top;
        if vi >= self.page_rows() {
            return None;
        }
        let left: i32 = (0..col).map(|c| gui::dpi_x(s.columns[c].width)).sum();
        let right = left + gui::dpi_x(s.columns[col].width);
        let y = self.header_height() + vi as i32 * self.item_height() + self.item_height() / 2;
        Some(w::POINT::with((left + right) / 2, y))
    }

    /// 指定セルが切り詰められていれば全文を返す（debug 観測用）。切り詰め無しは `None`。
    #[cfg(feature = "debug-server")]
    pub(crate) fn cell_tooltip(&self, row: usize, col: usize) -> Option<String> {
        let pt = self.cell_point(row, col)?;
        self.cell_tooltip_at(pt).map(|(_, text)| text)
    }

    /// 指定セルへ実際に hover した表示経路を駆動し (生成成功, 表示状態, 全文) を返す（debug 観測用）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn cell_hover(&self, row: usize, col: usize) -> Option<(bool, bool, String)> {
        let pt = self.cell_point(row, col)?;
        let tip = self.inner.cell_tip.borrow().clone()?;
        Some(tip.probe(self.hwnd(), pt))
    }

    fn on_l_button_dbl_clk(&self, pt: w::POINT) -> w::AnyResult<()> {
        if let Some(idx) = self.row_at(pt.y)
            && let Some(cb) = self.inner.on_activate.borrow().as_ref() {
                cb(idx);
            }
        Ok(())
    }

    /// 列のソート種別へ切替える。現在のソートが自然順なら名前・拡張子は自然順を
    /// 維持し、同じ種別への再クリックなら reverse を反転する。
    fn sort_by_column(&self, col: usize) -> w::AnyResult<()> {
        {
            let mut s = self.inner.state.borrow_mut();
            let target = s.columns[col].kind.sort_target(s.sort_type);
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

    /// 待機中の不定スピナーを中央に描く（一覧の代わり）。進捗の % はログ側に出すため、
    /// ここはぐるぐる＋ラベルだけにする。
    fn paint_loading(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let colors = self.inner.colors.get();
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        let glyph = self.inner.loading.borrow().as_ref().map(|s| s.glyph()).unwrap_or("");
        let text = format!("{}  読込中", glyph);

        dc.SetTextColor(rgb(colors.cursor))?;
        let sz = dc.GetTextExtentPoint32(&text).unwrap_or(w::SIZE { cx: 0, cy: 0 });
        dc.TextOut(((cw - sz.cx) / 2).max(0), ((ch - sz.cy) / 2).max(0), &text)?;
        Ok(())
    }

    /// ターゲットビットマップ選択済みの任意 DC へ全面描画する（フォント準備＋`paint_to`）。
    /// `on_paint` のダブルバッファと、デバッグ制御サーバの窓非依存スナップショットの両方から呼ぶ。
    pub(crate) fn render_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let fonts = self.create_fonts()?;
        let _font_sel = dc.SelectObject(fonts.primary())?;
        crate::winutil::set_char_spacing(dc, self.inner.char_spacing.get());
        // フォント高さ実測。
        if let Ok(tm) = dc.GetTextMetrics() {
            self.inner.font_height.set(tm.tmHeight);
        }
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        self.paint_to(dc, &fonts, cw, ch)
    }

    fn paint_to(&self, dc: &w::HDC, fonts: &FontSet, cw: i32, ch: i32) -> w::AnyResult<()> {
        // 閾値を過ぎた読込中なら一覧の代わりに進捗を出す（閾値前は通常の一覧を描く）。
        let show_loading = self
            .inner
            .loading
            .borrow()
            .as_ref()
            .is_some_and(|s| s.visible());
        if show_loading {
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
            self.draw_header_cell(dc, fonts, &s, left, right, header_h, &face_brush, hl, sh, wtext, Some(col))?;
        }
        // 末尾余白列。
        if total_w < cw {
            dc.FillRect(
                w::RECT { left: total_w, top: 0, right: cw, bottom: header_h },
                &face_brush,
            )?;
            self.draw_3d_frame(dc, w::RECT { left: total_w, top: 0, right: cw, bottom: header_h }, hl, sh)?;
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
        let show_icons = self.inner.icon_show.get();
        let icon_px = self.icon_px();
        let icon_cap = self.icon_cap();
        let dir = self.inner.dir.borrow();
        // 検索ハイライト用のマッチャは行ループの外で一度だけ作る（毎行作り直さない）。
        let matcher: Option<Matcher> = s.search.highlighting().then(|| s.search.matcher());
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
                let text = s.cell_text(item, col.kind, self.inner.size_format.get());
                if text.is_empty() {
                    continue;
                }
                let mut flags = co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX;
                if col.align == Align::Right {
                    flags |= co::DT::RIGHT;
                }
                // 名前列の左にシェルアイコンを内包する（モダン方式・専用列は持たない）。
                let mut text_left = left + margin;
                let is_name_col =
                    matches!(col.kind, ColumnKind::FileName | ColumnKind::FileBaseName);
                if is_name_col
                    && let Some(cache) = icon_cache.as_ref().filter(|_| show_icons) {
                        let iy = y + (item_h - icon_px) / 2;
                        let mut drawn = false;
                        // 実FSのファイル（ディレクトリ・親・書庫内を除く）のうち、拡張子ごとに
                        // アイコンが変わりうるものだけ per-file の固有アイコン/サムネを試み、
                        // 未取得なら汎用を描いて非同期取得を依頼する。書庫等の「同じ拡張子なら
                        // 常に同じ汎用アイコン」なものは同期・軽量な汎用パスだけで済ませる
                        // （非同期取得はシェルのアイコンオーバーレイハンドラを経由するため）。
                        if !item.is_dir
                            && !item.is_parent
                            && rerics_core::has_instance_icon(&item.extension)
                            && let Some(d) = dir.as_ref() {
                                let full = d.join(&item.name);
                                let mtime = item_mtime(item);
                                let dest =
                                    IconBox { x: text_left, y: iy, size: icon_px, cap: icon_cap };
                                if cache.draw_file(dc, &full, mtime, dest) {
                                    drawn = true;
                                } else {
                                    let thumb = matches!(
                                        MediaKind::from_extension(&item.extension),
                                        Some(MediaKind::Image)
                                    );
                                    cache.request_file(&full, mtime, thumb, icon_px);
                                }
                            }
                        if !drawn {
                            let dest = IconBox { x: text_left, y: iy, size: icon_px, cap: icon_cap };
                            cache.draw_generic(dc, item.is_dir, &item.extension, dest);
                            if item.is_parent {
                                // ".." の矢印もシェルアイコン同様に枠の中央へ原寸寄りで置く。
                                let s = icon_px.min(icon_cap.max(1));
                                let ox = text_left + (icon_px - s) / 2;
                                let oy = iy + (icon_px - s) / 2;
                                let _ = draw_parent_arrow(dc, ox, oy, s);
                            }
                        }
                        text_left += icon_px + gui::dpi_x(2);
                    }
                // 左は n 幅マージン（＋アイコン幅）、右パディングは 0（原作の左 4/右 0 に倣う）。
                let rect = w::RECT { left: text_left, top: y, right, bottom: y + item_h };
                let shown = fonts.elide(dc, &text, right - text_left);
                // 検索一致ハイライトは名前列・非マーク行のみ（マーク行は選択色を優先する）。
                let hit_spans = if is_name_col && !item.selected {
                    matcher.as_ref().map(|m| m.find(&shown)).unwrap_or_default()
                } else {
                    Vec::new()
                };
                if hit_spans.is_empty() {
                    fonts.draw_text(dc, &shown, rect, flags)?;
                } else {
                    self.draw_name_with_highlight(
                        dc, fonts, &shown, &hit_spans, text_left, y, item_h, text_color, &colors,
                    )?;
                }
                // 名前列のリンクは、余り幅にリンク先を薄色（行の文字色を行背景へ寄せた色）・
                // 右寄せで添える。名前が幅を先取りし、足りない分はリンク先側から削る。
                if is_name_col && let Some(target) = &item.link_target {
                    let name_w = fonts.width(dc, &shown);
                    let t_left = text_left + name_w + margin * 2;
                    let arrow = format!("→ {target}");
                    let shown_t = fonts.elide(dc, &arrow, right - t_left);
                    // 矢印すら残らない幅なら出さない（"…" だけの断片は無意味）。
                    if shown_t.chars().count() > 2 {
                        let row_bg =
                            if item.selected { sel_bg_color } else { colors.background };
                        dc.SetTextColor(rgb(text_color.blend(row_bg, 2, 5)))?;
                        let t_rect = w::RECT { left: t_left, top: y, right, bottom: y + item_h };
                        fonts.draw_text(dc, &shown_t, t_rect, flags | co::DT::RIGHT)?;
                        dc.SetTextColor(rgb(text_color))?;
                    }
                }
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

        // 5. ドロップホバー行の枠線（D&D でこの行のディレクトリへドロップしようとしている時）。
        if let Some(row) = self.inner.drop_hover_row.get()
            && row >= s.scroll_top
            && row <= bottom
            && row < s.count()
        {
            let y = header_h + ((row - s.scroll_top) as i32) * item_h;
            let hi_brush = w::HBRUSH::CreateSolidBrush(w::GetSysColor(co::COLOR::HIGHLIGHT))?;
            let outer = w::RECT { left: 0, top: y, right: total_w.max(1), bottom: y + item_h };
            dc.FrameRect(outer, &hi_brush)?;
            let inner = w::RECT {
                left: outer.left + 1,
                top: outer.top + 1,
                right: outer.right - 1,
                bottom: outer.bottom - 1,
            };
            dc.FrameRect(inner, &hi_brush)?;
        }

        // 6. 自前スクロールバー（生きている state borrow からインラインに算出）。
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
                let thumb_brush = w::HBRUSH::CreateSolidBrush(rgb(colors.scrollbar_thumb()))?;
                dc.FillRect(
                    w::RECT { left: bar_x + 1, top: thumb_top, right: cw - 1, bottom: thumb_top + thumb_h },
                    &thumb_brush,
                )?;
            }
        }
        Ok(())
    }

    /// 名前列のテキストを検索一致ハイライト付きで描く。一致区間は背景を
    /// `viewer_find_bg`・文字色を `viewer_find_text` にし、非一致区間は `base_color` で描く
    /// （地の文字色は呼び出し前に DC へ設定済みのものと同じ値を渡すこと）。
    #[allow(clippy::too_many_arguments)]
    fn draw_name_with_highlight(
        &self,
        dc: &w::HDC,
        fonts: &FontSet,
        text: &str,
        spans: &[(usize, usize)],
        left: i32,
        y: i32,
        item_h: i32,
        base_color: Rgb,
        colors: &Colors,
    ) -> w::AnyResult<()> {
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        if n == 0 {
            return Ok(());
        }
        let mut hit = vec![false; n];
        for &(off, len) in spans {
            let end = (off + len).min(n);
            for slot in hit.iter_mut().take(end).skip(off.min(n)) {
                *slot = true;
            }
        }
        let lh = self.font_height();
        let top = y + (item_h - lh) / 2;
        let find_bg = w::HBRUSH::CreateSolidBrush(rgb(colors.viewer_find_bg))?;
        let mut x = left;
        let mut p = 0;
        while p < n {
            let h0 = hit[p];
            let mut q = p + 1;
            while q < n && hit[q] == h0 {
                q += 1;
            }
            let seg: String = chars[p..q].iter().collect();
            let seg_w = fonts.width(dc, &seg);
            if h0 {
                let bg_rect = w::RECT { left: x, top: y, right: x + seg_w, bottom: y + item_h };
                dc.FillRect(bg_rect, &find_bg)?;
                dc.SetTextColor(rgb(colors.viewer_find_text))?;
                fonts.text_out(dc, x, top, &seg)?;
                dc.SetTextColor(rgb(base_color))?;
            } else {
                fonts.text_out(dc, x, top, &seg)?;
            }
            x += seg_w;
            p = q;
        }
        Ok(())
    }

    /// ヘッダ1列を描く（3D 枠＋テキスト＋ソート三角）。
    #[allow(clippy::too_many_arguments)]
    fn draw_header_cell(
        &self,
        dc: &w::HDC,
        fonts: &FontSet,
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
        self.draw_3d_frame(dc, w::RECT { left, top: 0, right, bottom: header_h }, hl, sh)?;
        let Some(col) = col else {
            return Ok(());
        };
        // テキスト。
        let margin = Self::text_margin(dc);
        dc.SetTextColor(wtext)?;
        if !col.text.is_empty() {
            let rect = w::RECT { left: left + margin, top: 4, right: right - margin, bottom: header_h };
            fonts.draw_text(dc, &col.text, rect, co::DT::SINGLELINE | co::DT::NOPREFIX)?;
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
            let tw = fonts.width(dc, &col.text);
            let x0 = left + margin + tw + 8;
            // 三角グリフを文字として描く（上向き=昇順／下向き=降順、塗り=自然順ソート）。
            let exp_like = matches!(
                s.sort_type,
                SortType::FileNameExpLike | SortType::ExtensionExpLike
            );
            let glyph = match (exp_like, s.sort_reverse) {
                (false, false) => "△",
                (false, true) => "▽",
                (true, false) => "▲",
                (true, true) => "▼",
            };
            // 既定ソートと異なるソート中は強調色にする。
            let is_default = (s.sort_type, s.sort_reverse)
                == (self.inner.default_sort.get(), self.inner.default_sort_reverse.get());
            if !is_default {
                let (r, g, b) = SORT_GLYPH_HIGHLIGHT_RGB;
                dc.SetTextColor(w::COLORREF::from_rgb(r, g, b))?;
            }
            // ラベルより控えめに、本文フォントを縮小した専用フォントで描く。
            let size = (self.inner.font_size.get() * SORT_GLYPH_SIZE_PERCENT / 100)
                .max(SORT_GLYPH_MIN_SIZE);
            let small = self.create_font_family(&self.inner.font_family.borrow(), size)?;
            let _small_sel = dc.SelectObject(&*small)?;
            let rect = w::RECT { left: x0, top: 0, right, bottom: header_h };
            dc.DrawText(glyph, rect, co::DT::SINGLELINE | co::DT::NOPREFIX | co::DT::VCENTER)?;
        }
        Ok(())
    }

    /// 明(左上)/暗(右下)の 3D 枠を `rect` の範囲に描く。
    fn draw_3d_frame(
        &self,
        dc: &w::HDC,
        rect: w::RECT,
        hl: w::COLORREF,
        sh: w::COLORREF,
    ) -> w::AnyResult<()> {
        let w::RECT { left, top, right, bottom } = rect;
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
    fn SystemParametersInfoW(
        action: u32,
        uiparam: u32,
        pvparam: *mut std::ffi::c_void,
        winini: u32,
    ) -> i32;
}

/// OS のホイール1ノッチあたりのスクロール行数（既定3）。`WHEEL_PAGESCROLL`
/// （= u32::MAX）のときは「1画面分」を表す。
pub(crate) fn os_wheel_scroll_lines() -> u32 {
    const SPI_GETWHEELSCROLLLINES: u32 = 0x0068;
    let mut lines: u32 = 3;
    unsafe {
        SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            &mut lines as *mut u32 as *mut std::ffi::c_void,
            0,
        );
    }
    lines
}

/// ノッチ数と OS 設定からスクロール行数を求める。`per_notch` が `WHEEL_PAGESCROLL`
/// （u32::MAX）なら 1 ノッチ＝1 画面（`page_rows` 行）として扱う。
fn wheel_lines(notches: i32, per_notch: u32, page_rows: usize) -> i32 {
    if per_notch == u32::MAX {
        notches * page_rows.max(1) as i32
    } else {
        notches * per_notch as i32
    }
}

/// 親（..）行のフォルダアイコンへ「上の階層」を表す上向き三角を重ねて描く。
/// 白塗り＋濃い縁取りで、フォルダ色・背景色のどちらでも視認できるようにする。
fn draw_parent_arrow(dc: &w::HDC, x: i32, y: i32, size: i32) -> w::AnyResult<()> {
    let cx = x + size / 2;
    let half = (size * 28 / 100).max(2);
    let top = y + size * 30 / 100;
    let bottom = y + size * 70 / 100;
    let pts = [
        w::POINT::with(cx, top),
        w::POINT::with(cx - half, bottom),
        w::POINT::with(cx + half, bottom),
    ];
    let fill = w::HBRUSH::CreateSolidBrush(w::COLORREF::from_rgb(0xFF, 0xFF, 0xFF))?;
    let pen = w::HPEN::CreatePen(co::PS::SOLID, 1, w::COLORREF::from_rgb(0x20, 0x20, 0x20))?;
    let _fill_sel = dc.SelectObject(&*fill)?;
    let _pen_sel = dc.SelectObject(&*pen)?;
    dc.Polygon(&pts)?;
    Ok(())
}

/// `Rgb` を COLORREF へ変換する。
fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::wheel_lines;

    #[test]
    fn wheel_lines_uses_os_setting() {
        assert_eq!(wheel_lines(1, 3, 20), 3);
        assert_eq!(wheel_lines(-2, 3, 20), -6);
        assert_eq!(wheel_lines(1, 0, 20), 0); // 0 行設定なら動かさない
        assert_eq!(wheel_lines(1, u32::MAX, 20), 20); // ページスクロール = 1 画面
        assert_eq!(wheel_lines(-1, u32::MAX, 20), -20);
    }
}
