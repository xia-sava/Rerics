//! 画像/動画ビューアの表示パネル（自前描画）。
//!
//! テキストビューアと同じく、別窓を作らずメイン領域へ重ねる 1 枚の `WindowControl`。
//! デコードは core（`rerics_core::StillImage`）が RGBA へ展開し、本モジュールは回転・
//! BGRA 変換した画素を `SetDIBits`→`StretchBlt` で拡縮描画する。フィット/ズーム/パン/
//! 回転と、同ディレクトリの前後送りを受け持つ。下端に状態行（ファイル名・寸法・倍率・
//! 位置）を出し、状態行より上の本文領域は将来アニメ/動画の再生バーを差し込む余地として
//! 1 か所にまとめておく。

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rerics_core::{
    Colors, Config, FrameSource, MediaKind, Rgb, clamp_pan, composite_over_checker, fit_scale,
    fit_scale_height, fit_scale_look_large, fit_scale_width, flip_rgba, load_image, placement,
    rgba_to_bgra, rgba_to_clipboard_dib, rotate_rgba,
};

/// 透過表示の市松 1 マスの画素サイズ。
const CHECKER_SQ: u32 = 8;
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;

/// 画像として読み込むファイルサイズの上限（これを超えるファイルは読まない）。
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// ズーム倍率の下限・上限。
const MIN_SCALE: f64 = 0.02;
const MAX_SCALE: f64 = 32.0;

/// アニメ/動画の再生用タイマ ID。
const MEDIA_TIMER_ID: usize = 0x6D31;

/// 巡回 index から表示すべき実パスを解決する。実FS はパスを直に返し、書庫内エントリは
/// ここで遅延展開（既展開なら再利用）する。`None` は「読み込めない」（展開失敗等）を表す。
/// これにより `MediaView` 自身は書庫を一切知らずに前後送りできる。
pub type NavResolver = Rc<dyn Fn(usize) -> Option<PathBuf>>;

/// 画像の表示モード（倍率の決め方）。原作 ImageViewer の D1〜D5 に対応する。
/// 手動ズーム・回転後は `Manual` になり、保存した倍率で表示する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayMode {
    /// 手動ズーム（保存した倍率を使う）。
    Manual,
    /// 原寸（常に 1.0）。原作 D1=NonStretch。
    NonStretch,
    /// 全体表示（縦横比保持で領域に収める・縮小のみ）。原作 D2=Stretch。
    Stretch,
    /// 幅を領域に合わせる（高さははみ出してスクロール）。原作 D3=StretchWidth。
    StretchWidth,
    /// 高さを領域に合わせる（幅ははみ出してスクロール）。原作 D4=StretchHeight。
    StretchHeight,
    /// なるべく大きく表示（一辺ぴったり・他辺ははみ出す）。原作 D5=StretchLookLarge。
    StretchLookLarge,
}

impl DisplayMode {
    /// debug-server 観測用のトークン。
    #[cfg(feature = "debug-server")]
    fn token(self) -> &'static str {
        match self {
            DisplayMode::Manual => "manual",
            DisplayMode::NonStretch => "actual",
            DisplayMode::Stretch => "fit",
            DisplayMode::StretchWidth => "fit_width",
            DisplayMode::StretchHeight => "fit_height",
            DisplayMode::StretchLookLarge => "fit_large",
        }
    }
}

/// 右クリック時に画面座標を渡すコールバック（メニュー表示は MainWindow が担う）。
type MenuHandler = Box<dyn Fn(w::POINT)>;

struct Inner {
    /// 巡回件数と現在位置（実パスは `resolver` が index から解決する）。
    nav_len: Cell<usize>,
    nav_index: Cell<usize>,
    /// 現在 index の実パスを解決する（書庫内は遅延展開）。
    resolver: RefCell<Option<NavResolver>>,
    title: RefCell<String>,
    /// 画像が無いとき（未対応・読込失敗・動画）に中央へ出す文言。
    message: RefCell<Option<String>>,
    /// フレーム供給元（静止画/アニメ/動画）。
    source: RefCell<Option<Box<dyn FrameSource>>>,
    /// アニメ/動画か（再生バーを出す）。
    animated: Cell<bool>,
    /// 再生中か。
    playing: Cell<bool>,
    /// 現在表示中フレームの表示時間 [ms]（次フレームまでの待ち）。
    cur_delay: Cell<u32>,
    /// シークバーをドラッグ中か。
    seeking: Cell<bool>,
    /// 現在のメディアが透過を含むか（市松背景＋アルファ合成に切り替える）。
    has_alpha: Cell<bool>,
    /// 現在フレームの元画素（RGBA・回転前）と寸法。回転変更時に再回転する元にする。
    base_rgba: RefCell<Vec<u8>>,
    base_w: Cell<u32>,
    base_h: Cell<u32>,
    /// 描画用に回転・BGRA 変換済みの画素と、その寸法。
    bgra: RefCell<Vec<u8>>,
    frame_w: Cell<u32>,
    frame_h: Cell<u32>,
    /// 表示状態。
    rotation: Cell<u32>,
    /// 鏡像反転（左右／上下）。回転とは独立に保持する。
    hflip: Cell<bool>,
    vflip: Cell<bool>,
    scale: Cell<f64>,
    /// 表示モード（倍率の決め方）。手動ズーム時は `Manual`。
    mode: Cell<DisplayMode>,
    pan: Cell<(f64, f64)>,
    /// パン（ドラッグ）操作の途中状態。
    panning: Cell<bool>,
    pan_start: Cell<(i32, i32)>,
    pan_origin: Cell<(f64, f64)>,
    colors: Colors,
    font_family: String,
    font_size: i32,
    /// ズーム1段あたりの拡大率（倍率係数）。設定の `zoom_step_percent` から決まる。
    zoom_step: f64,
    /// 右クリック時に呼ぶコールバック（画面座標）。コンテキストメニュー表示は MainWindow が担う。
    on_menu: RefCell<Option<MenuHandler>>,
}

/// 画像/動画ビューア表示パネル。
#[derive(Clone)]
pub struct MediaView {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl MediaView {
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
            nav_len: Cell::new(0),
            nav_index: Cell::new(0),
            resolver: RefCell::new(None),
            title: RefCell::new(String::new()),
            message: RefCell::new(None),
            source: RefCell::new(None),
            animated: Cell::new(false),
            playing: Cell::new(false),
            cur_delay: Cell::new(0),
            seeking: Cell::new(false),
            has_alpha: Cell::new(false),
            base_rgba: RefCell::new(Vec::new()),
            base_w: Cell::new(0),
            base_h: Cell::new(0),
            bgra: RefCell::new(Vec::new()),
            frame_w: Cell::new(0),
            frame_h: Cell::new(0),
            rotation: Cell::new(0),
            hflip: Cell::new(false),
            vflip: Cell::new(false),
            scale: Cell::new(1.0),
            mode: Cell::new(DisplayMode::Stretch),
            pan: Cell::new((0.0, 0.0)),
            panning: Cell::new(false),
            pan_start: Cell::new((0, 0)),
            pan_origin: Cell::new((0.0, 0.0)),
            colors: cfg.active_colors(),
            font_family: cfg.font.family.clone(),
            font_size: cfg.font.size,
            zoom_step: 1.0 + cfg.image.zoom_step_percent.max(1) as f64 / 100.0,
            on_menu: RefCell::new(None),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    /// 右クリック時のコールバック（コンテキストメニュー表示）を登録する。
    pub fn on_menu(&self, cb: impl Fn(w::POINT) + 'static) {
        *self.inner.on_menu.borrow_mut() = Some(Box::new(cb));
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// 巡回の現在位置と総数（1始まり・空なら 0）。デバッグ制御サーバの状態取得用。
    #[cfg(feature = "debug-server")]
    pub fn nav_position(&self) -> (usize, usize) {
        let total = self.inner.nav_len.get();
        let index = if total == 0 { 0 } else { self.inner.nav_index.get() + 1 };
        (index, total)
    }

    /// 現在表示中メディアのタイトル（ファイル名）。デバッグ制御サーバの状態取得用。
    #[cfg(feature = "debug-server")]
    pub fn title(&self) -> String {
        self.inner.title.borrow().clone()
    }

    /// 現在の表示モードのトークン（debug-server 観測用）。
    #[cfg(feature = "debug-server")]
    pub fn display_mode(&self) -> &'static str {
        self.inner.mode.get().token()
    }

    /// 現在の表示倍率（％・debug-server 観測用）。直近の描画で確定した値。
    #[cfg(feature = "debug-server")]
    pub fn scale_percent(&self) -> i32 {
        (self.inner.scale.get() * 100.0).round() as i32
    }

    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// 実FS のパス列をそのまま巡回対象にして開く（実FS 用の簡易版）。
    pub fn open(&self, files: Vec<PathBuf>, index: usize) {
        let n = files.len();
        let files = Rc::new(files);
        let resolver: NavResolver = Rc::new(move |i| files.get(i).cloned());
        self.open_nav(n, index, resolver);
    }

    /// 巡回件数・初期位置・index→実パス解決器を与えて開く。書庫内メディアの遅延展開は
    /// `resolver` が担い、`MediaView` は解決済み実パスの読込/表示だけを受け持つ。
    pub fn open_nav(&self, len: usize, index: usize, resolver: NavResolver) {
        self.inner.nav_len.set(len);
        self.inner.nav_index.set(if len == 0 { 0 } else { index.min(len - 1) });
        *self.inner.resolver.borrow_mut() = Some(resolver);
        self.load_current();
    }

    /// 前後のファイルへ移動する（巡回）。書庫内は移動先のその1枚だけを resolver が展開する。
    pub fn navigate(&self, delta: isize) -> w::AnyResult<()> {
        let n = self.inner.nav_len.get();
        if n == 0 {
            return Ok(());
        }
        let cur = self.inner.nav_index.get() as isize;
        let next = (cur + delta).rem_euclid(n as isize) as usize;
        self.inner.nav_index.set(next);
        self.load_current();
        self.refresh()
    }

    fn load_current(&self) {
        let idx = self.inner.nav_index.get();
        let resolved = {
            let r = self.inner.resolver.borrow();
            r.as_ref().and_then(|f| f(idx))
        };
        match resolved {
            Some(path) => self.load_path(&path),
            None => {
                // 解決できない（書庫内エントリの展開失敗等）＝表示状態を畳んで文言表示。
                let _ = self.hwnd().KillTimer(MEDIA_TIMER_ID);
                *self.inner.source.borrow_mut() = None;
                self.inner.animated.set(false);
                self.inner.playing.set(false);
                *self.inner.title.borrow_mut() = String::new();
                self.set_message("このメディアを開けません");
                let _ = self.refresh();
            }
        }
    }

    fn load_path(&self, path: &Path) {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        *self.inner.title.borrow_mut() = name;
        // 表示状態と再生状態を初期化する。
        let _ = self.hwnd().KillTimer(MEDIA_TIMER_ID);
        *self.inner.source.borrow_mut() = None;
        self.inner.animated.set(false);
        self.inner.playing.set(false);
        self.inner.seeking.set(false);
        self.inner.rotation.set(0);
        self.inner.hflip.set(false);
        self.inner.vflip.set(false);
        self.inner.scale.set(1.0);
        self.inner.mode.set(DisplayMode::Stretch);
        self.inner.pan.set((0.0, 0.0));

        let ext = path
            .extension()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match MediaKind::from_extension(&ext) {
            Some(MediaKind::Video) => match crate::video::VideoSource::open(path) {
                Some(src) => self.set_source(Box::new(src)),
                None => self.set_message("この動画は再生できません（コーデック未対応の可能性）"),
            },
            Some(MediaKind::Image) | Some(MediaKind::Animation) => match read_capped(path, MAX_IMAGE_BYTES) {
                Some(bytes) => match load_image(&bytes) {
                    Some(src) => self.set_source(src),
                    None => self.set_message("この画像は表示できません"),
                },
                None => self.set_message("ファイルを読み込めません"),
            },
            None => self.set_message("この形式は表示できません"),
        }
        let _ = self.refresh();
    }

    /// フレーム供給元をセットし、先頭フレームを表示する。アニメ/動画なら再生を開始する。
    fn set_source(&self, src: Box<dyn FrameSource>) {
        let animated = src.is_animated();
        self.inner.has_alpha.set(src.has_alpha());
        *self.inner.source.borrow_mut() = Some(src);
        *self.inner.message.borrow_mut() = None;
        self.inner.animated.set(animated);
        self.inner.playing.set(animated);
        self.show_next(true);
        if animated {
            self.schedule_timer();
        }
    }

    /// 次フレーム（終端なら `loop_at_end` でループ）を取り出して表示用バッファへ反映する。
    fn show_next(&self, loop_at_end: bool) -> Option<()> {
        let (rgba, w, h, delay) = {
            let mut guard = self.inner.source.borrow_mut();
            let src = guard.as_mut()?;
            let frame = match src.next_frame() {
                Some(f) => Some(f),
                None if loop_at_end => {
                    src.reset();
                    src.next_frame()
                }
                None => None,
            }?;
            let delay = frame.delay.map(|d| d.as_millis() as u32).unwrap_or(0);
            (frame.rgba, frame.width, frame.height, delay)
        };
        *self.inner.base_rgba.borrow_mut() = rgba;
        self.inner.base_w.set(w);
        self.inner.base_h.set(h);
        self.inner.cur_delay.set(delay);
        self.rebuild_rotated();
        Some(())
    }

    fn schedule_timer(&self) {
        let ms = self.inner.cur_delay.get().max(20);
        let _ = self.hwnd().SetTimer(MEDIA_TIMER_ID, ms, None);
    }

    /// 再生を止める（タイマ停止）。閉じる/離れる時に呼ぶ。
    pub fn stop_playback(&self) {
        let _ = self.hwnd().KillTimer(MEDIA_TIMER_ID);
        self.inner.playing.set(false);
    }

    /// 再生/一時停止をトグルする（アニメ/動画のみ）。
    pub fn toggle_play(&self) -> w::AnyResult<()> {
        if !self.inner.animated.get() {
            return Ok(());
        }
        let playing = !self.inner.playing.get();
        self.inner.playing.set(playing);
        if playing {
            self.schedule_timer();
        } else {
            let _ = self.hwnd().KillTimer(MEDIA_TIMER_ID);
        }
        self.refresh()
    }

    fn set_message(&self, msg: &str) {
        *self.inner.message.borrow_mut() = Some(msg.to_owned());
        *self.inner.base_rgba.borrow_mut() = Vec::new();
        self.inner.bgra.borrow_mut().clear();
        self.inner.frame_w.set(0);
        self.inner.frame_h.set(0);
    }

    /// 元画素を現在の回転角で回し、BGRA へ変換して描画用バッファを作り直す。
    fn rebuild_rotated(&self) {
        let base = self.inner.base_rgba.borrow();
        if base.is_empty() {
            return;
        }
        let (bw, bh) = (self.inner.base_w.get(), self.inner.base_h.get());
        let (rgba, rw, rh) = rotate_rgba(&base, bw, bh, self.inner.rotation.get());
        let rgba = self.apply_flips(rgba, rw, rh);
        let mut bgra = rgba_to_bgra(&rgba);
        // 透過画像は市松の上へ焼き込んで不透明化する（描画は通常の blit で済む）。
        if self.inner.has_alpha.get() {
            composite_over_checker(&mut bgra, rw, CHECKER_SQ);
        }
        *self.inner.bgra.borrow_mut() = bgra;
        self.inner.frame_w.set(rw);
        self.inner.frame_h.set(rh);
    }

    fn has_image(&self) -> bool {
        !self.inner.bgra.borrow().is_empty()
    }

    /// ホイール 1 ノッチでズームする（上＝拡大）。
    pub fn on_wheel(&self, distance: i16) -> w::AnyResult<()> {
        self.zoom(distance > 0)
    }

    /// 1 段ズームする（`zoom_in` が真で拡大）。現在倍率に設定の増減率を掛け、
    /// 等倍（1.0）をまたぐときは 1.0 へスナップする。フィットを解いて手動モードへ移る。
    pub fn zoom(&self, zoom_in: bool) -> w::AnyResult<()> {
        if !self.has_image() {
            return Ok(());
        }
        let prev = self.inner.scale.get();
        let factor = if zoom_in { self.inner.zoom_step } else { 1.0 / self.inner.zoom_step };
        let mut next = (prev * factor).clamp(MIN_SCALE, MAX_SCALE);
        if (prev < 1.0 && next > 1.0) || (prev > 1.0 && next < 1.0) {
            next = 1.0;
        }
        self.inner.scale.set(next);
        self.inner.mode.set(DisplayMode::Manual);
        self.refresh()
    }

    /// 表示モードを切り替える（パンは中央へ戻す）。倍率は描画時にモードから決まる。
    fn set_mode(&self, mode: DisplayMode) -> w::AnyResult<()> {
        self.inner.mode.set(mode);
        self.inner.pan.set((0.0, 0.0));
        self.refresh()
    }

    /// 領域に合わせて全体表示にする（原作 D2=Stretch）。
    pub fn fit_to_window(&self) -> w::AnyResult<()> {
        self.set_mode(DisplayMode::Stretch)
    }

    /// 原寸（100%）表示にする（原作 D1=NonStretch）。
    pub fn actual_size(&self) -> w::AnyResult<()> {
        self.inner.scale.set(1.0);
        self.set_mode(DisplayMode::NonStretch)
    }

    /// 幅を領域に合わせる（原作 D3=StretchWidth）。
    pub fn fit_width(&self) -> w::AnyResult<()> {
        self.set_mode(DisplayMode::StretchWidth)
    }

    /// 高さを領域に合わせる（原作 D4=StretchHeight）。
    pub fn fit_height(&self) -> w::AnyResult<()> {
        self.set_mode(DisplayMode::StretchHeight)
    }

    /// なるべく大きく表示する（原作 D5=StretchLookLarge）。
    pub fn fit_look_large(&self) -> w::AnyResult<()> {
        self.set_mode(DisplayMode::StretchLookLarge)
    }

    /// 現在の鏡像反転設定を RGBA へ適用する（左右→上下の順）。
    fn apply_flips(&self, rgba: Vec<u8>, w: u32, h: u32) -> Vec<u8> {
        let rgba = if self.inner.hflip.get() {
            flip_rgba(&rgba, w, h, true)
        } else {
            rgba
        };
        if self.inner.vflip.get() {
            flip_rgba(&rgba, w, h, false)
        } else {
            rgba
        }
    }

    /// 時計回りに 90 度回転する。
    pub fn rotate(&self) -> w::AnyResult<()> {
        self.rotate_by(90)
    }

    /// 反時計回りに 90 度回転する。
    pub fn rotate_left(&self) -> w::AnyResult<()> {
        self.rotate_by(270)
    }

    fn rotate_by(&self, delta: u32) -> w::AnyResult<()> {
        if self.inner.base_rgba.borrow().is_empty() {
            return Ok(());
        }
        self.inner.rotation.set((self.inner.rotation.get() + delta) % 360);
        self.inner.pan.set((0.0, 0.0));
        self.rebuild_rotated();
        self.refresh()
    }

    /// 左右反転をトグルする。
    pub fn flip_horizontal(&self) -> w::AnyResult<()> {
        if self.inner.base_rgba.borrow().is_empty() {
            return Ok(());
        }
        self.inner.hflip.set(!self.inner.hflip.get());
        self.rebuild_rotated();
        self.refresh()
    }

    /// 上下反転をトグルする。
    pub fn flip_vertical(&self) -> w::AnyResult<()> {
        if self.inner.base_rgba.borrow().is_empty() {
            return Ok(());
        }
        self.inner.vflip.set(!self.inner.vflip.get());
        self.rebuild_rotated();
        self.refresh()
    }

    /// 表示中の画像（回転・反転を反映した原寸）をクリップボードへコピーする。
    pub fn copy_to_clipboard(&self) -> w::AnyResult<()> {
        let base = self.inner.base_rgba.borrow();
        if base.is_empty() {
            return Ok(());
        }
        let (bw, bh) = (self.inner.base_w.get(), self.inner.base_h.get());
        let (rgba, rw, rh) = rotate_rgba(&base, bw, bh, self.inner.rotation.get());
        let rgba = self.apply_flips(rgba, rw, rh);
        let dib = rgba_to_clipboard_dib(&rgba, rw, rh);
        let clip = self.hwnd().OpenClipboard()?;
        clip.EmptyClipboard()?;
        clip.SetClipboardData(co::CF::DIB, &dib)?;
        Ok(())
    }

    fn setup_events(&self) {
        crate::winutil::passive_focus(&self.wnd);

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        // 再生タイマ：表示中フレームの delay 経過で次フレームへ進める。
        let this = self.clone();
        self.wnd.on().wm_timer(MEDIA_TIMER_ID, move || {
            if this.inner.playing.get() {
                this.show_next(true);
                this.schedule_timer();
                this.refresh()?;
            }
            Ok(())
        });

        // 右ボタン：コンテキストメニューを開く（表示は MainWindow が担う）。
        let this = self.clone();
        self.wnd.on().wm_r_button_down(move |p| {
            let screen = this.hwnd().ClientToScreen(p.coords).unwrap_or(p.coords);
            if let Some(cb) = this.inner.on_menu.borrow().as_ref() {
                cb(screen);
            }
            Ok(())
        });

        // 左ボタン：シークバー上ならシーク、画像上（手動ズーム時）ならパン開始。
        let this = self.clone();
        self.wnd.on().wm_l_button_down(move |p| {
            if this.inner.animated.get() && this.in_seek_area(p.coords.y) {
                this.inner.seeking.set(true);
                std::mem::forget(this.hwnd().SetCapture());
                this.seek_to_x(p.coords.x);
            } else if this.has_image() && this.inner.mode.get() != DisplayMode::Stretch {
                this.inner.panning.set(true);
                this.inner.pan_start.set((p.coords.x, p.coords.y));
                this.inner.pan_origin.set(this.inner.pan.get());
                std::mem::forget(this.hwnd().SetCapture());
            }
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_move(move |p| {
            if this.inner.seeking.get() {
                this.seek_to_x(p.coords.x);
            } else if this.inner.panning.get() {
                let (sx, sy) = this.inner.pan_start.get();
                let (ox, oy) = this.inner.pan_origin.get();
                let dx = (p.coords.x - sx) as f64;
                let dy = (p.coords.y - sy) as f64;
                this.inner.pan.set((ox + dx, oy + dy));
                this.refresh()?;
            }
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_up(move |_p| {
            if this.inner.seeking.get() {
                this.inner.seeking.set(false);
                drop(this.hwnd().SetCapture());
            } else if this.inner.panning.get() {
                this.inner.panning.set(false);
                drop(this.hwnd().SetCapture());
            }
            Ok(())
        });
    }

    /// シークバー領域の縦範囲に `y` が入るか。
    fn in_seek_area(&self, y: i32) -> bool {
        let ch = self.hwnd().GetClientRect().map(|r| r.bottom - r.top).unwrap_or(0);
        let seek_h = self.seek_height();
        if seek_h == 0 {
            return false;
        }
        let top = ch - self.status_height() - seek_h;
        y >= top && y < ch - self.status_height()
    }

    /// シークバーのトラック x 範囲（左端, 右端）。
    fn track_range(&self, cw: i32) -> (i32, i32) {
        let pad = gui::dpi_x(self.inner.font_size).max(1);
        let label_w = gui::dpi_x(self.inner.font_size * 9);
        ((pad + label_w).min(cw), (cw - pad).max(0))
    }

    /// シークバー上の x 座標へシークする。
    fn seek_to_x(&self, x: i32) {
        let cw = self.hwnd().GetClientRect().map(|r| r.right - r.left).unwrap_or(0);
        let (x0, x1) = self.track_range(cw);
        let dur = self.inner.source.borrow().as_ref().map(|s| s.duration_ms()).unwrap_or(0);
        if dur == 0 || x1 <= x0 {
            return;
        }
        let frac = ((x - x0) as f64 / (x1 - x0) as f64).clamp(0.0, 1.0);
        let ms = (frac * dur as f64) as u64;
        if let Some(s) = self.inner.source.borrow_mut().as_mut() {
            s.seek(ms);
        }
        self.show_next(true);
        let _ = self.refresh();
    }

    /// 現在の (位置, 総時間) [ms]。
    fn progress(&self) -> (u64, u64) {
        match self.inner.source.borrow().as_ref() {
            Some(s) => (s.position_ms(), s.duration_ms()),
            None => (0, 0),
        }
    }

    fn seek_height(&self) -> i32 {
        if self.inner.animated.get() {
            gui::dpi_y(self.inner.font_size + 12)
        } else {
            0
        }
    }

    fn status_height(&self) -> i32 {
        gui::dpi_y(self.inner.font_size + 8)
    }

    fn create_font(&self, size: i32) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
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
            co::PITCH::DEFAULT,
            &self.inner.font_family,
        )
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

    /// ターゲットビットマップ選択済みの任意 DC へ全面描画する。色互換ビットマップ生成にも
    /// ターゲット DC を使う（32bpp ビットマップ選択済みなのでカラーで作られる）。
    pub(crate) fn render_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        self.paint_to(dc, dc, cw, ch)
    }

    fn paint_to(&self, hdc: &w::HDC, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let colors = self.inner.colors;
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        let status_h = self.status_height();
        let seek_h = self.seek_height();
        let body_h = (ch - status_h - seek_h).max(0);

        if self.has_image() {
            self.draw_image(hdc, dc, cw, body_h)?;
        } else {
            let msg = self.inner.message.borrow().clone();
            if let Some(msg) = msg {
                self.draw_message(dc, cw, body_h, &msg)?;
            }
        }

        if seek_h > 0 {
            self.draw_seek_bar(dc, cw, body_h, seek_h)?;
        }
        self.draw_status(dc, cw, ch - status_h, status_h)?;
        Ok(())
    }

    /// アニメ/動画の再生バー（再生状態・トラック・つまみ・時間）を描く。
    fn draw_seek_bar(&self, dc: &w::HDC, cw: i32, top: i32, h: i32) -> w::AnyResult<()> {
        let brush = w::HBRUSH::CreateSolidBrush(chrome::face())?;
        dc.FillRect(w::RECT { left: 0, top, right: cw, bottom: top + h }, &brush)?;
        chrome::hline(dc, 0, cw, top, chrome::highlight())?;

        let (pos, dur) = self.progress();
        let (x0, x1) = self.track_range(cw);
        let cy = top + h / 2;
        // トラック下地。
        chrome::hline(dc, x0, x1, cy, chrome::shadow())?;
        if dur > 0 && x1 > x0 {
            let frac = (pos as f64 / dur as f64).clamp(0.0, 1.0);
            let fx = x0 + ((x1 - x0) as f64 * frac).round() as i32;
            let fill = w::HBRUSH::CreateSolidBrush(rgb(self.inner.colors.selected_file_bg))?;
            dc.FillRect(w::RECT { left: x0, top: cy - 1, right: fx, bottom: cy + 2 }, &fill)?;
            let thumb = w::HBRUSH::CreateSolidBrush(rgb(self.inner.colors.selected_file))?;
            dc.FillRect(w::RECT { left: fx - 2, top: cy - 5, right: fx + 3, bottom: cy + 6 }, &thumb)?;
        }
        // 再生状態＋時間（トラック左の固定枠）。
        let sfont = self.create_font((self.inner.font_size - 2).max(6))?;
        let _sel = dc.SelectObject(&*sfont)?;
        dc.SetTextColor(chrome::text())?;
        let mark = if self.inner.playing.get() { "▶" } else { "‖" };
        let label = format!("{}  {} / {}", mark, fmt_time(pos), fmt_time(dur));
        let pad = gui::dpi_x(self.inner.font_size).max(1);
        let rect = w::RECT { left: pad, top, right: x0, bottom: top + h };
        dc.DrawText(&label, rect, co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX)?;
        Ok(())
    }

    fn draw_image(&self, hdc: &w::HDC, dc: &w::HDC, cw: i32, body_h: i32) -> w::AnyResult<()> {
        let fw = self.inner.frame_w.get();
        let fh = self.inner.frame_h.get();
        if fw == 0 || fh == 0 || body_h <= 0 {
            return Ok(());
        }
        // 倍率（モード指定時は領域から毎回再計算して保持・手動ズーム時のみ保存値）。
        let scale = match self.inner.mode.get() {
            DisplayMode::Manual => self.inner.scale.get(),
            DisplayMode::NonStretch => 1.0,
            DisplayMode::Stretch => fit_scale(fw, fh, cw, body_h),
            DisplayMode::StretchWidth => fit_scale_width(fw, cw),
            DisplayMode::StretchHeight => fit_scale_height(fh, body_h),
            DisplayMode::StretchLookLarge => fit_scale_look_large(fw, fh, cw, body_h),
        };
        self.inner.scale.set(scale);
        // パンを画像が離れすぎない範囲へ収める。
        let disp_w = (fw as f64 * scale).round() as i32;
        let disp_h = (fh as f64 * scale).round() as i32;
        let pan = clamp_pan(disp_w, disp_h, cw, body_h, self.inner.pan.get());
        self.inner.pan.set(pan);
        let pl = placement(fw, fh, cw, body_h, scale, pan);

        // BGRA をデバイスビットマップへ流し込む（トップダウン＝biHeight 負）。
        let hbm = hdc.CreateCompatibleBitmap(fw as i32, fh as i32)?;
        let mut bmi = w::BITMAPINFO::default();
        bmi.bmiHeader.biWidth = fw as i32;
        bmi.bmiHeader.biHeight = -(fh as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = co::BI::RGB;
        {
            let bgra = self.inner.bgra.borrow();
            hdc.SetDIBits(&hbm, 0, fh, &bgra, &bmi, co::DIB::RGB_COLORS)?;
        }
        let img_dc = hdc.CreateCompatibleDC()?;
        let _sel = img_dc.SelectObject(&*hbm)?;
        let _ = dc.SetStretchBltMode(co::STRETCH_MODE::HALFTONE);
        dc.StretchBlt(
            w::POINT { x: pl.x, y: pl.y },
            w::SIZE { cx: pl.w, cy: pl.h },
            &img_dc,
            w::POINT { x: 0, y: 0 },
            w::SIZE { cx: fw as i32, cy: fh as i32 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    fn draw_message(&self, dc: &w::HDC, cw: i32, body_h: i32, msg: &str) -> w::AnyResult<()> {
        let font = self.create_font(self.inner.font_size + 2)?;
        let _sel = dc.SelectObject(&*font)?;
        dc.SetTextColor(rgb(self.inner.colors.file_normal))?;
        let rect = w::RECT { left: 0, top: 0, right: cw, bottom: body_h };
        dc.DrawText(
            msg,
            rect,
            co::DT::SINGLELINE | co::DT::CENTER | co::DT::VCENTER | co::DT::NOPREFIX,
        )?;
        Ok(())
    }

    fn draw_status(&self, dc: &w::HDC, cw: i32, sy: i32, sh: i32) -> w::AnyResult<()> {
        let brush = w::HBRUSH::CreateSolidBrush(chrome::face())?;
        dc.FillRect(w::RECT { left: 0, top: sy, right: cw, bottom: sy + sh }, &brush)?;
        chrome::hline(dc, 0, cw, sy, chrome::highlight())?;
        let sfont = self.create_font((self.inner.font_size - 2).max(6))?;
        let _sfont_sel = dc.SelectObject(&*sfont)?;
        dc.SetTextColor(chrome::text())?;
        let text = self.status_text();
        let pad = gui::dpi_x(self.inner.font_size).max(1);
        let rect = w::RECT { left: pad, top: sy, right: cw - pad, bottom: sy + sh };
        dc.DrawText(
            &text,
            rect,
            co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX | co::DT::END_ELLIPSIS,
        )?;
        Ok(())
    }

    fn status_text(&self) -> String {
        let title = self.inner.title.borrow();
        let total = self.inner.nav_len.get();
        let idx = self.inner.nav_index.get() + 1;
        let pos = if total > 1 { format!("    [{}/{}]", idx, total) } else { String::new() };
        if !self.has_image() {
            return format!("{}{}    (←→:送り Esc:閉じる)", title, pos);
        }
        let bw = self.inner.base_w.get();
        let bh = self.inner.base_h.get();
        let zoom = (self.inner.scale.get() * 100.0).round() as i32;
        let rot = self.inner.rotation.get();
        let rot_s = if rot != 0 { format!("  {}°", rot) } else { String::new() };
        let play = if self.inner.animated.get() { " Space:再生/停止" } else { "" };
        format!(
            "{}    {}x{}  {}%{}{}    (←→:送り +/-:拡縮 1原寸 2全体 3幅 4高 5大 R:回転{} Esc:閉じる)",
            title, bw, bh, zoom, rot_s, pos, play,
        )
    }
}

/// ミリ秒を `m:ss` 形式へ。
fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// ファイルを最大 `cap` バイトまで読む（超過分は読まない）。
fn read_capped(path: &Path, cap: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(cap as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}
