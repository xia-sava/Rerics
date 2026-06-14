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
    Colors, Config, FrameSource, MediaKind, Rgb, StillImage, clamp_pan, fit_scale, placement,
    rgba_to_bgra, rotate_rgba,
};
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;

/// 画像として読み込むファイルサイズの上限（これを超えるファイルは読まない）。
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// ズーム倍率の下限・上限。
const MIN_SCALE: f64 = 0.02;
const MAX_SCALE: f64 = 32.0;

struct Inner {
    /// 巡回対象（同ディレクトリの閲覧可能ファイル）と現在位置。
    nav_files: RefCell<Vec<PathBuf>>,
    nav_index: Cell<usize>,
    title: RefCell<String>,
    /// 画像が無いとき（未対応・読込失敗・動画）に中央へ出す文言。
    message: RefCell<Option<String>>,
    /// 回転前の元画素（RGBA）と寸法。回転変更時に再回転する元にする。
    base_rgba: RefCell<Vec<u8>>,
    base_w: Cell<u32>,
    base_h: Cell<u32>,
    /// 描画用に回転・BGRA 変換済みの画素と、その寸法。
    bgra: RefCell<Vec<u8>>,
    frame_w: Cell<u32>,
    frame_h: Cell<u32>,
    /// 表示状態。
    rotation: Cell<u32>,
    scale: Cell<f64>,
    /// フィット（領域に合わせて自動縮小）か、手動ズームか。
    fit: Cell<bool>,
    pan: Cell<(f64, f64)>,
    /// パン（ドラッグ）操作の途中状態。
    panning: Cell<bool>,
    pan_start: Cell<(i32, i32)>,
    pan_origin: Cell<(f64, f64)>,
    colors: Colors,
    font_family: String,
    font_size: i32,
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
            nav_files: RefCell::new(Vec::new()),
            nav_index: Cell::new(0),
            title: RefCell::new(String::new()),
            message: RefCell::new(None),
            base_rgba: RefCell::new(Vec::new()),
            base_w: Cell::new(0),
            base_h: Cell::new(0),
            bgra: RefCell::new(Vec::new()),
            frame_w: Cell::new(0),
            frame_h: Cell::new(0),
            rotation: Cell::new(0),
            scale: Cell::new(1.0),
            fit: Cell::new(true),
            pan: Cell::new((0.0, 0.0)),
            panning: Cell::new(false),
            pan_start: Cell::new((0, 0)),
            pan_origin: Cell::new((0.0, 0.0)),
            colors: cfg.active_colors(),
            font_family: cfg.font.family.clone(),
            font_size: cfg.font.size,
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    pub fn refresh(&self) -> w::AnyResult<()> {
        self.hwnd().InvalidateRect(None, true)?;
        Ok(())
    }

    /// 巡回ファイル群と表示位置を設定して読み込む。
    pub fn open(&self, files: Vec<PathBuf>, index: usize) {
        let n = files.len();
        *self.inner.nav_files.borrow_mut() = files;
        self.inner.nav_index.set(if n == 0 { 0 } else { index.min(n - 1) });
        self.load_current();
    }

    /// 前後のファイルへ移動する（巡回）。
    pub fn navigate(&self, delta: isize) -> w::AnyResult<()> {
        let n = self.inner.nav_files.borrow().len();
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
        let path = {
            let files = self.inner.nav_files.borrow();
            match files.get(self.inner.nav_index.get()) {
                Some(p) => p.clone(),
                None => return,
            }
        };
        self.load_path(&path);
    }

    fn load_path(&self, path: &Path) {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        *self.inner.title.borrow_mut() = name;
        // 表示状態を初期化する。
        self.inner.rotation.set(0);
        self.inner.scale.set(1.0);
        self.inner.fit.set(true);
        self.inner.pan.set((0.0, 0.0));

        let ext = path
            .extension()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match MediaKind::from_extension(&ext) {
            Some(MediaKind::Video) => self.set_message("（動画は未対応です）"),
            Some(MediaKind::Image) | Some(MediaKind::Animation) => match read_capped(path, MAX_IMAGE_BYTES) {
                Some(bytes) => match StillImage::load(&bytes) {
                    Some(src) => self.set_image(src),
                    None => self.set_message("この画像は表示できません"),
                },
                None => self.set_message("ファイルを読み込めません"),
            },
            None => self.set_message("この形式は表示できません"),
        }
        let _ = self.refresh();
    }

    fn set_image(&self, mut src: StillImage) {
        let (w, h) = src.dimensions();
        let frame = match src.next_frame() {
            Some(f) => f,
            None => return,
        };
        *self.inner.base_rgba.borrow_mut() = frame.rgba;
        self.inner.base_w.set(w);
        self.inner.base_h.set(h);
        *self.inner.message.borrow_mut() = None;
        self.rebuild_rotated();
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
        *self.inner.bgra.borrow_mut() = rgba_to_bgra(&rgba);
        self.inner.frame_w.set(rw);
        self.inner.frame_h.set(rh);
    }

    fn has_image(&self) -> bool {
        !self.inner.bgra.borrow().is_empty()
    }

    /// ホイール 1 ノッチでズームする。
    pub fn on_wheel(&self, distance: i16) -> w::AnyResult<()> {
        if !self.has_image() {
            return Ok(());
        }
        self.zoom_by(if distance > 0 { 1.25 } else { 0.8 })
    }

    /// 現在倍率に `factor` を掛けてズームする（手動モードへ移る）。
    pub fn zoom_by(&self, factor: f64) -> w::AnyResult<()> {
        if !self.has_image() {
            return Ok(());
        }
        let next = (self.inner.scale.get() * factor).clamp(MIN_SCALE, MAX_SCALE);
        self.inner.scale.set(next);
        self.inner.fit.set(false);
        self.refresh()
    }

    /// 領域に合わせて全体表示へ戻す。
    pub fn fit_to_window(&self) -> w::AnyResult<()> {
        self.inner.fit.set(true);
        self.inner.pan.set((0.0, 0.0));
        self.refresh()
    }

    /// 原寸（100%）表示にする。
    pub fn actual_size(&self) -> w::AnyResult<()> {
        self.inner.fit.set(false);
        self.inner.scale.set(1.0);
        self.inner.pan.set((0.0, 0.0));
        self.refresh()
    }

    /// 時計回りに 90 度回転する。
    pub fn rotate(&self) -> w::AnyResult<()> {
        if self.inner.base_rgba.borrow().is_empty() {
            return Ok(());
        }
        self.inner.rotation.set((self.inner.rotation.get() + 90) % 360);
        self.inner.pan.set((0.0, 0.0));
        self.rebuild_rotated();
        self.refresh()
    }

    fn setup_events(&self) {
        self.wnd.on().wm(co::WM::MOUSEACTIVATE, |_| Ok(3));

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        // ドラッグでパン（手動ズームで画像が領域からはみ出している時のみ意味を持つ）。
        let this = self.clone();
        self.wnd.on().wm_l_button_down(move |p| {
            if this.has_image() && !this.inner.fit.get() {
                this.inner.panning.set(true);
                this.inner.pan_start.set((p.coords.x, p.coords.y));
                this.inner.pan_origin.set(this.inner.pan.get());
                std::mem::forget(this.hwnd().SetCapture());
            }
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_move(move |p| {
            if this.inner.panning.get() {
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
            if this.inner.panning.get() {
                this.inner.panning.set(false);
                drop(this.hwnd().SetCapture());
            }
            Ok(())
        });
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
        mem_dc.SetBkMode(co::BKMODE::TRANSPARENT)?;

        self.paint_to(&hdc, &mem_dc, cw, ch)?;

        hdc.BitBlt(
            w::POINT { x: 0, y: 0 },
            w::SIZE { cx: cw, cy: ch },
            &mem_dc,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    fn paint_to(&self, hdc: &w::HDC, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let colors = self.inner.colors;
        let bg = w::HBRUSH::CreateSolidBrush(rgb(colors.background))?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        let status_h = self.status_height();
        let body_h = (ch - status_h).max(0);

        if self.has_image() {
            self.draw_image(hdc, dc, cw, body_h)?;
        } else {
            let msg = self.inner.message.borrow().clone();
            if let Some(msg) = msg {
                self.draw_message(dc, cw, body_h, &msg)?;
            }
        }

        self.draw_status(dc, cw, ch - status_h, status_h)?;
        Ok(())
    }

    fn draw_image(&self, hdc: &w::HDC, dc: &w::HDC, cw: i32, body_h: i32) -> w::AnyResult<()> {
        let fw = self.inner.frame_w.get();
        let fh = self.inner.frame_h.get();
        if fw == 0 || fh == 0 || body_h <= 0 {
            return Ok(());
        }
        // 倍率（フィット時は領域から再計算して保持する）。
        let scale = if self.inner.fit.get() {
            let s = fit_scale(fw, fh, cw, body_h);
            self.inner.scale.set(s);
            s
        } else {
            self.inner.scale.get()
        };
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
        let total = self.inner.nav_files.borrow().len();
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
        format!(
            "{}    {}x{}  {}%{}{}    (←→:送り +/-:拡縮 0:全体 1:原寸 R:回転 Esc:閉じる)",
            title, bw, bh, zoom, rot_s, pos,
        )
    }
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
