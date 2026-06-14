//! 画像/動画ビューアの UI 非依存ロジック層。
//!
//! 表示は「時間つきフレーム列」を出す `FrameSource` を共通の入口にする。静止画は
//! 「1 フレームで終わる列」として扱い、アニメや動画も同じ trait で後付けできる形にする。
//! デコードはここ（core）で `image` crate を用いて RGBA8 へ展開し、GUI 側は受け取った
//! ピクセルバッファを GDI で描画するだけにする。フィット倍率・回転・配置の幾何計算も
//! 純関数としてここに置き、テストで固める。

use std::time::Duration;

/// デコード済み 1 フレーム。`rgba` は行優先・トップダウン・1 画素 4 バイト（R,G,B,A）。
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// 次フレームまでの表示時間。静止画（次が無い）は `None`。
    pub delay: Option<Duration>,
}

/// 「時間つきフレーム列」を引いて出す共通の入口。
///
/// 静止画は最初の `next_frame` で 1 枚返し、以降は `reset` するまで `None`。アニメ/動画は
/// フレームと表示時間を順に返す。`dimensions` は表示領域の見積りに使う基準サイズ。
/// 再生バー用の `duration_ms`/`position_ms`/`seek` は既定実装（静止画では無効）を持つ。
pub trait FrameSource {
    /// 基準となる画素サイズ（回転前）。
    fn dimensions(&self) -> (u32, u32);
    /// 複数フレームを持つ（再生バーを出す）か。
    fn is_animated(&self) -> bool;
    /// 次のフレームを取り出す。列の終端では `None`。
    fn next_frame(&mut self) -> Option<Frame>;
    /// 列の先頭へ巻き戻す（ループ再生やシークの土台）。
    fn reset(&mut self);

    /// 既知の総フレーム数（不明な動画などは 0）。
    fn frame_count(&self) -> usize {
        1
    }
    /// 総再生時間 [ms]（不明なら 0）。
    fn duration_ms(&self) -> u64 {
        0
    }
    /// 直近に取り出したフレームの再生位置 [ms]。
    fn position_ms(&self) -> u64 {
        0
    }
    /// 指定時刻 [ms] へシークする（できなければ無視）。次の `next_frame` がその位置を返す。
    fn seek(&mut self, _ms: u64) {}

    /// 透過（アルファ < 255 の画素）を含むか。市松背景の出し分けに使う。
    fn has_alpha(&self) -> bool {
        false
    }
}

/// RGBA バッファに透過画素が含まれるか。
fn buffer_has_alpha(rgba: &[u8]) -> bool {
    rgba.iter().skip(3).step_by(4).any(|&a| a < 255)
}

/// 単一画像のフレーム列（1 枚で終わる退化ケース）。
pub struct StillImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    has_alpha: bool,
    served: bool,
}

impl StillImage {
    /// バイト列をデコードする（形式はマジックバイトから自動判別）。失敗時は `None`。
    pub fn load(bytes: &[u8]) -> Option<Self> {
        let img = image::load_from_memory(bytes).ok()?;
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        if width == 0 || height == 0 {
            return None;
        }
        let rgba = rgba.into_raw();
        let has_alpha = buffer_has_alpha(&rgba);
        Some(Self { width, height, rgba, has_alpha, served: false })
    }
}

impl FrameSource for StillImage {
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn is_animated(&self) -> bool {
        false
    }

    fn next_frame(&mut self) -> Option<Frame> {
        if self.served {
            return None;
        }
        self.served = true;
        Some(Frame {
            width: self.width,
            height: self.height,
            rgba: self.rgba.clone(),
            delay: None,
        })
    }

    fn reset(&mut self) {
        self.served = false;
    }

    fn has_alpha(&self) -> bool {
        self.has_alpha
    }
}

/// デコード済みフレーム上限（巨大アニメの暴走防止）。
const MAX_ANIM_FRAMES: usize = 4096;

struct FrameData {
    rgba: Vec<u8>,
    delay_ms: u32,
}

/// アニメーション画像（GIF/WebP/APNG）のフレーム列。全フレームを先に展開して保持する。
pub struct AnimatedImage {
    width: u32,
    height: u32,
    frames: Vec<FrameData>,
    /// 各フレームの再生開始時刻 [ms]（累積）。
    starts: Vec<u64>,
    total_ms: u64,
    has_alpha: bool,
    /// 次に返すフレーム。
    idx: usize,
    /// 直近に返したフレーム（位置表示用）。
    last: usize,
}

impl AnimatedImage {
    /// バイト列をアニメーションとして展開する。アニメ非対応形式や 1 フレームのみは `None`。
    pub fn load(bytes: &[u8]) -> Option<Self> {
        let frames = collect_frames(bytes)?;
        if frames.len() < 2 {
            return None;
        }
        let (width, height) = (frames[0].0, frames[0].1);
        let frames: Vec<FrameData> = frames
            .into_iter()
            .map(|(_, _, rgba, delay_ms)| FrameData { rgba, delay_ms })
            .collect();
        let has_alpha = frames.first().map(|f| buffer_has_alpha(&f.rgba)).unwrap_or(false);
        let mut starts = Vec::with_capacity(frames.len());
        let mut acc = 0u64;
        for f in &frames {
            starts.push(acc);
            acc += f.delay_ms as u64;
        }
        Some(Self { width, height, frames, starts, total_ms: acc, has_alpha, idx: 0, last: 0 })
    }
}

impl FrameSource for AnimatedImage {
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    fn next_frame(&mut self) -> Option<Frame> {
        if self.idx >= self.frames.len() {
            return None;
        }
        let i = self.idx;
        self.last = i;
        self.idx += 1;
        let fd = &self.frames[i];
        Some(Frame {
            width: self.width,
            height: self.height,
            rgba: fd.rgba.clone(),
            delay: Some(Duration::from_millis(fd.delay_ms as u64)),
        })
    }

    fn reset(&mut self) {
        self.idx = 0;
    }

    fn frame_count(&self) -> usize {
        self.frames.len()
    }

    fn duration_ms(&self) -> u64 {
        self.total_ms
    }

    fn position_ms(&self) -> u64 {
        self.starts.get(self.last).copied().unwrap_or(0)
    }

    fn seek(&mut self, ms: u64) {
        // ms 以下で最大の開始時刻を持つフレームへ。
        let mut target = 0usize;
        for (i, &s) in self.starts.iter().enumerate() {
            if s <= ms {
                target = i;
            } else {
                break;
            }
        }
        self.idx = target;
        self.last = target;
    }

    fn has_alpha(&self) -> bool {
        self.has_alpha
    }
}

/// BGRA バッファ（行優先・幅 `width`）を市松模様の上にアルファ合成して不透明化する。
/// 透過 PNG/WebP/APNG の背景を市松表示するための前処理（描画は通常の不透明 blit で済む）。
/// `sq` は 1 マスの画素サイズ。市松色は明 0xFF・暗 0xCC のグレー。
pub fn composite_over_checker(bgra: &mut [u8], width: u32, sq: u32) {
    let w = width.max(1) as usize;
    let sq = sq.max(1) as usize;
    for (i, px) in bgra.chunks_exact_mut(4).enumerate() {
        let x = i % w;
        let y = i / w;
        let bg = if (x / sq + y / sq) % 2 == 0 { 0xFFu32 } else { 0xCCu32 };
        let a = px[3] as u32;
        for c in px.iter_mut().take(3) {
            *c = ((*c as u32 * a + bg * (255 - a)) / 255) as u8;
        }
        px[3] = 255;
    }
}

/// アニメフレームを `(幅, 高, RGBA, delay_ms)` で全展開する。アニメ非対応なら `None`。
fn collect_frames(bytes: &[u8]) -> Option<Vec<(u32, u32, Vec<u8>, u32)>> {
    use image::AnimationDecoder;
    let format = image::guess_format(bytes).ok()?;
    let data = bytes.to_vec();
    let frames = match format {
        image::ImageFormat::Gif => {
            image::codecs::gif::GifDecoder::new(std::io::Cursor::new(data)).ok()?.into_frames()
        }
        image::ImageFormat::WebP => {
            image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(data)).ok()?.into_frames()
        }
        image::ImageFormat::Png => {
            let dec = image::codecs::png::PngDecoder::new(std::io::Cursor::new(data)).ok()?;
            if !dec.is_apng().ok()? {
                return None;
            }
            dec.apng().ok()?.into_frames()
        }
        _ => return None,
    };
    let mut out = Vec::new();
    for f in frames {
        let f = f.ok()?;
        let (n, d) = f.delay().numer_denom_ms();
        // 0/極小 delay は実用上 100ms 扱い（ブラウザ慣習）。最低 20ms にクランプ。
        let raw = if d == 0 { 0 } else { n / d };
        let delay_ms = if raw < 20 { 100 } else { raw };
        let buf = f.into_buffer();
        let (w, h) = buf.dimensions();
        out.push((w, h, buf.into_raw(), delay_ms));
        if out.len() >= MAX_ANIM_FRAMES {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// バイト列を画像フレーム列として開く。アニメは `AnimatedImage`、それ以外は `StillImage`。
pub fn load_image(bytes: &[u8]) -> Option<Box<dyn FrameSource>> {
    if let Some(anim) = AnimatedImage::load(bytes) {
        return Some(Box::new(anim));
    }
    StillImage::load(bytes).map(|s| Box::new(s) as Box<dyn FrameSource>)
}

/// 拡張子から判定したメディア種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// 単一の静止画。
    Image,
    /// アニメーション可能な形式（GIF/WebP/APNG）。静止画としても開ける。
    Animation,
    /// 動画コンテナ。
    Video,
}

impl MediaKind {
    /// 拡張子（先頭ドットの有無は問わない）から種別を引く。非メディアは `None`。
    pub fn from_extension(ext: &str) -> Option<MediaKind> {
        let e = ext.trim_start_matches('.').to_ascii_lowercase();
        match e.as_str() {
            "png" | "jpg" | "jpeg" | "jpe" | "jfif" | "bmp" | "dib" | "tif" | "tiff" | "ico"
            | "tga" | "qoi" | "ppm" | "pgm" | "pbm" | "pnm" | "dds" | "hdr" | "exr" | "ff"
            | "avif" | "heic" | "heif" => Some(MediaKind::Image),
            "gif" | "webp" | "apng" => Some(MediaKind::Animation),
            "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "wmv" | "ts" | "mpg" | "mpeg" => {
                Some(MediaKind::Video)
            }
            _ => None,
        }
    }
}

/// 回転後の論理サイズ（90/270 度で幅・高さが入れ替わる）。`degrees` は 0/90/180/270。
pub fn rotated_dims(w: u32, h: u32, degrees: u32) -> (u32, u32) {
    if (degrees / 90) % 2 == 1 {
        (h, w)
    } else {
        (w, h)
    }
}

/// 原寸優先・はみ出す分だけ縮小するフィット倍率（拡大はしない＝最大 1.0）。
pub fn fit_scale(img_w: u32, img_h: u32, win_w: i32, win_h: i32) -> f64 {
    if img_w == 0 || img_h == 0 || win_w <= 0 || win_h <= 0 {
        return 1.0;
    }
    let sx = win_w as f64 / img_w as f64;
    let sy = win_h as f64 / img_h as f64;
    sx.min(sy).min(1.0)
}

/// 表示先の矩形（左上 x,y と 幅,高）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 画素サイズ・領域サイズ・倍率・パン量から表示先矩形を求める（中央寄せ＋パン）。
pub fn placement(img_w: u32, img_h: u32, win_w: i32, win_h: i32, scale: f64, pan: (f64, f64)) -> Placement {
    let w = ((img_w as f64 * scale).round() as i32).max(1);
    let h = ((img_h as f64 * scale).round() as i32).max(1);
    let x = ((win_w - w) as f64 / 2.0 + pan.0).round() as i32;
    let y = ((win_h - h) as f64 / 2.0 + pan.1).round() as i32;
    Placement { x, y, w, h }
}

/// パン量を「画像が領域から離れすぎない」範囲へ収める。画像が領域より小さい軸は中央固定（0）。
pub fn clamp_pan(disp_w: i32, disp_h: i32, win_w: i32, win_h: i32, pan: (f64, f64)) -> (f64, f64) {
    let limit = |disp: i32, win: i32, p: f64| -> f64 {
        if disp <= win {
            0.0
        } else {
            let max = ((disp - win) / 2) as f64;
            p.clamp(-max, max)
        }
    };
    (limit(disp_w, win_w, pan.0), limit(disp_h, win_h, pan.1))
}

/// RGBA バッファを時計回りに 90 度単位で回転する。戻り値は `(画素, 幅, 高)`。
pub fn rotate_rgba(rgba: &[u8], w: u32, h: u32, degrees: u32) -> (Vec<u8>, u32, u32) {
    let k = (degrees / 90) % 4;
    if k == 0 {
        return (rgba.to_vec(), w, h);
    }
    let (wu, hu) = (w as usize, h as usize);
    let (dw, dh) = if k % 2 == 1 { (hu, wu) } else { (wu, hu) };
    let mut out = vec![0u8; dw * dh * 4];
    let px = |x: usize, y: usize| -> usize { (y * wu + x) * 4 };
    for dy in 0..dh {
        for dx in 0..dw {
            let (sx, sy) = match k {
                1 => (dy, h as usize - 1 - dx),       // 90 度 時計回り
                2 => (wu - 1 - dx, hu - 1 - dy),       // 180 度
                _ => (w as usize - 1 - dy, dx),        // 270 度 時計回り
            };
            let s = px(sx, sy);
            let d = (dy * dw + dx) * 4;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (out, dw as u32, dh as u32)
}

/// RGBA を Windows DIB 用の BGRA へ変換する（R と B を入れ替えるだけ）。
pub fn rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for px in out.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_classifies_by_extension() {
        assert_eq!(MediaKind::from_extension(".png"), Some(MediaKind::Image));
        assert_eq!(MediaKind::from_extension("JPG"), Some(MediaKind::Image));
        assert_eq!(MediaKind::from_extension(".gif"), Some(MediaKind::Animation));
        assert_eq!(MediaKind::from_extension("webp"), Some(MediaKind::Animation));
        assert_eq!(MediaKind::from_extension(".mp4"), Some(MediaKind::Video));
        assert_eq!(MediaKind::from_extension(".txt"), None);
        assert_eq!(MediaKind::from_extension(""), None);
    }

    #[test]
    fn fit_shrinks_large_keeps_small() {
        assert_eq!(fit_scale(1000, 1000, 500, 500), 0.5);
        assert_eq!(fit_scale(100, 100, 500, 500), 1.0); // 原寸より拡大しない
        assert_eq!(fit_scale(400, 200, 200, 200), 0.5); // 幅が制約
        assert_eq!(fit_scale(0, 0, 200, 200), 1.0);
    }

    #[test]
    fn rotated_dims_swaps_on_quarter_turns() {
        assert_eq!(rotated_dims(4, 3, 0), (4, 3));
        assert_eq!(rotated_dims(4, 3, 90), (3, 4));
        assert_eq!(rotated_dims(4, 3, 180), (4, 3));
        assert_eq!(rotated_dims(4, 3, 270), (3, 4));
    }

    #[test]
    fn placement_centers_image() {
        let p = placement(100, 100, 300, 200, 1.0, (0.0, 0.0));
        assert_eq!(p, Placement { x: 100, y: 50, w: 100, h: 100 });
        let z = placement(100, 100, 300, 200, 2.0, (0.0, 0.0));
        assert_eq!(z, Placement { x: 50, y: 0, w: 200, h: 200 });
        // パンを足すと中央からずれる。
        let pn = placement(100, 100, 300, 200, 1.0, (30.0, -10.0));
        assert_eq!(pn, Placement { x: 130, y: 40, w: 100, h: 100 });
    }

    #[test]
    fn clamp_pan_pins_small_axis_and_bounds_large() {
        // 画像が領域より小さい軸は中央固定。
        assert_eq!(clamp_pan(100, 100, 300, 300, (50.0, 50.0)), (0.0, 0.0));
        // 大きい軸は (はみ出し/2) に収まる。400 幅を 200 窓 → ±100。
        assert_eq!(clamp_pan(400, 100, 200, 300, (250.0, 0.0)), (100.0, 0.0));
        assert_eq!(clamp_pan(400, 100, 200, 300, (-250.0, 0.0)), (-100.0, 0.0));
    }

    #[test]
    fn rotate_90_moves_left_to_top() {
        // 横並び「A B」(2x1)。A=(255,0,0,255) B=(0,255,0,255)。
        let a = [255u8, 0, 0, 255];
        let b = [0u8, 255, 0, 255];
        let src: Vec<u8> = [a, b].concat();
        let (out, w, h) = rotate_rgba(&src, 2, 1, 90);
        assert_eq!((w, h), (1, 2));
        assert_eq!(&out[0..4], &a); // 上が A
        assert_eq!(&out[4..8], &b); // 下が B
    }

    #[test]
    fn rotate_360_is_identity() {
        let src: Vec<u8> = (0..2 * 3 * 4).map(|i| i as u8).collect();
        let (out, w, h) = rotate_rgba(&src, 2, 3, 0);
        assert_eq!((w, h), (2, 3));
        assert_eq!(out, src);
    }

    #[test]
    fn composite_checker_blends_and_opaques() {
        // 幅2・sq=1 の市松＝(0,0)明0xFF・(1,0)暗0xCC・(0,1)暗・(1,1)明。
        // 完全透明画素は市松色そのものになり、アルファは 255 に。
        let mut buf = vec![0u8; 2 * 2 * 4]; // 全画素 BGRA=(0,0,0,0)
        composite_over_checker(&mut buf, 2, 1);
        assert_eq!(&buf[0..4], &[0xFF, 0xFF, 0xFF, 255]); // (0,0) 明
        assert_eq!(&buf[4..8], &[0xCC, 0xCC, 0xCC, 255]); // (1,0) 暗
        assert_eq!(&buf[8..12], &[0xCC, 0xCC, 0xCC, 255]); // (0,1) 暗
        assert_eq!(&buf[12..16], &[0xFF, 0xFF, 0xFF, 255]); // (1,1) 明
        // 不透明画素は色を保ち市松を透かさない。
        let mut opaque = [10u8, 20, 30, 255];
        composite_over_checker(&mut opaque, 1, 1);
        assert_eq!(opaque, [10, 20, 30, 255]);
    }

    #[test]
    fn rgba_to_bgra_swaps_red_blue() {
        let rgba = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let bgra = rgba_to_bgra(&rgba);
        assert_eq!(bgra, [30, 20, 10, 40, 70, 60, 50, 80]);
    }

    fn encode_anim_gif(frames: &[([u8; 4], u32)]) -> Vec<u8> {
        use image::{Delay, Frame as IFrame, Rgba, RgbaImage};
        let mut buf = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut buf);
            for (color, ms) in frames {
                let img = RgbaImage::from_pixel(2, 2, Rgba(*color));
                let frame = IFrame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(*ms, 1));
                enc.encode_frame(frame).unwrap();
            }
        }
        buf
    }

    #[test]
    fn animated_gif_timeline_and_seek() {
        let buf = encode_anim_gif(&[([255, 0, 0, 255], 100), ([0, 0, 255, 255], 100)]);
        let mut a = AnimatedImage::load(&buf).expect("animated");
        assert!(a.is_animated());
        assert_eq!(a.frame_count(), 2);
        assert_eq!(a.duration_ms(), 200);
        let f0 = a.next_frame().unwrap();
        assert_eq!((f0.width, f0.height), (2, 2));
        assert_eq!(a.position_ms(), 0);
        a.next_frame().unwrap();
        assert_eq!(a.position_ms(), 100);
        assert!(a.next_frame().is_none()); // 終端で None（ループは消費側）
        a.reset();
        assert_eq!(a.position_ms(), 100); // reset は idx のみ。位置は次の取り出しで更新
        a.next_frame().unwrap();
        assert_eq!(a.position_ms(), 0);
        a.seek(150); // starts=[0,100] → frame1
        assert_eq!(a.position_ms(), 100);
        assert!(a.next_frame().is_some());
        a.seek(50); // → frame0
        assert_eq!(a.position_ms(), 0);
    }

    #[test]
    fn load_image_picks_animated_or_still() {
        // アニメ GIF → AnimatedImage（is_animated=true）。
        let gif = encode_anim_gif(&[([1, 2, 3, 255], 50), ([4, 5, 6, 255], 50)]);
        let anim = load_image(&gif).unwrap();
        assert!(anim.is_animated());
        // 単一 PNG → StillImage（is_animated=false）。
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba([9, 9, 9, 255])))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let still = load_image(png.get_ref()).unwrap();
        assert!(!still.is_animated());
    }

    #[test]
    fn still_image_serves_once_until_reset() {
        // 最小 PNG（1x1）を image で生成してラウンドトリップ。
        let mut buf = std::io::Cursor::new(Vec::new());
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        let mut s = StillImage::load(buf.get_ref()).unwrap();
        assert_eq!(s.dimensions(), (1, 1));
        assert!(!s.is_animated());
        let f = s.next_frame().unwrap();
        assert_eq!(f.rgba, vec![1, 2, 3, 255]);
        assert!(s.next_frame().is_none());
        s.reset();
        assert!(s.next_frame().is_some());
    }
}
