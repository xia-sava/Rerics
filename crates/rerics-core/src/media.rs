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
pub trait FrameSource {
    /// 基準となる画素サイズ（回転前）。
    fn dimensions(&self) -> (u32, u32);
    /// 複数フレームを持つ（再生バーを出す）か。
    fn is_animated(&self) -> bool;
    /// 次のフレームを取り出す。列の終端では `None`。
    fn next_frame(&mut self) -> Option<Frame>;
    /// 列の先頭へ巻き戻す（ループ再生やシークの土台）。
    fn reset(&mut self);
}

/// 単一画像のフレーム列（1 枚で終わる退化ケース）。
pub struct StillImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
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
        Some(Self { width, height, rgba: rgba.into_raw(), served: false })
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
    fn rgba_to_bgra_swaps_red_blue() {
        let rgba = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let bgra = rgba_to_bgra(&rgba);
        assert_eq!(bgra, [30, 20, 10, 40, 70, 60, 50, 80]);
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
