//! 画像/動画ビューアの UI 非依存ロジック層。
//!
//! 表示は「時間つきフレーム列」を出す `FrameSource` を共通の入口にする。静止画は
//! 「1 フレームで終わる列」として扱い、アニメや動画も同じ trait で後付けできる形にする。
//! デコードはここ（core）で `image` crate を用いて RGBA8 へ展開し、GUI 側は受け取った
//! ピクセルバッファを GDI で描画するだけにする。フィット倍率・回転・配置の幾何計算も
//! 純関数としてここに置き、テストで固める。

use std::io::Cursor;
use std::time::Duration;

use image::ImageDecoder;

/// バイト列をデコードし、EXIF の Orientation に従って正立させた `DynamicImage` を返す。
///
/// JPEG/TIFF/WebP は decoder が EXIF Orientation を読む（他形式は無変換）。回転メタ付きの
/// 写真（スマホの縦撮り等）を撮影時の向きへ補正する。decoder 経由が失敗した場合は素の
/// `load_from_memory` にフォールバックする（向き補正は諦めるが画像は出す）。
fn decode_oriented(bytes: &[u8]) -> Option<image::DynamicImage> {
    fn oriented(bytes: &[u8]) -> Option<image::DynamicImage> {
        let reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut decoder = reader.into_decoder().ok()?;
        let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
        img.apply_orientation(orientation);
        Some(img)
    }
    oriented(bytes).or_else(|| image::load_from_memory(bytes).ok())
}

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
    /// バイト列をデコードする（形式はマジックバイトから自動判別・EXIF Orientation で正立補正）。失敗時は `None`。
    pub fn load(bytes: &[u8]) -> Option<Self> {
        let img = decode_oriented(bytes)?;
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
        let bg = if (x / sq + y / sq).is_multiple_of(2) { 0xFFu32 } else { 0xCCu32 };
        let a = px[3] as u32;
        for c in px.iter_mut().take(3) {
            *c = ((*c as u32 * a + bg * (255 - a)) / 255) as u8;
        }
        px[3] = 255;
    }
}

/// アニメ1フレーム＝`(幅, 高, RGBA, delay_ms)`。
type AnimFrame = (u32, u32, Vec<u8>, u32);

/// アニメフレームを `(幅, 高, RGBA, delay_ms)` で全展開する。アニメ非対応なら `None`。
fn collect_frames(bytes: &[u8]) -> Option<Vec<AnimFrame>> {
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
        let raw = n.checked_div(d).unwrap_or(0);
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

/// サムネイル用：バイト列をデコードし、長辺が `max` px 以内に収まるよう縮小した RGBA を返す
/// （アスペクト比保持）。リスト一覧の小プレビュー生成に使う。失敗時は `None`。
pub fn decode_thumbnail(bytes: &[u8], max: u32) -> Option<(u32, u32, Vec<u8>)> {
    let img = decode_oriented(bytes)?;
    let thumb = img.thumbnail(max.max(1), max.max(1));
    let rgba = thumb.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h, rgba.into_raw()))
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

/// 拡張子が「ファイルごとに異なるアイコンを持ちうる」か。画像はサムネイルとして中身を
/// 見せる価値があり、実行体・ショートカット等は埋め込みアイコンがインスタンスごとに違う。
/// それ以外（多くの文書・書庫等）は同じ拡張子ならどれも同じ汎用アイコンにしかならないので、
/// per-file の実体取得（非同期・シェルのオーバーレイハンドラ経由）を試す価値が無い。
pub fn has_instance_icon(ext: &str) -> bool {
    if MediaKind::from_extension(ext).is_some() {
        return true;
    }
    let e = ext.trim_start_matches('.').to_ascii_lowercase();
    matches!(e.as_str(), "exe" | "dll" | "lnk" | "scr" | "cpl" | "msi" | "ocx" | "url")
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

/// 幅を領域に合わせる倍率（幅が領域より大きいときだけ縮小・拡大はしない）。高さははみ出す。
pub fn fit_scale_width(img_w: u32, win_w: i32) -> f64 {
    if img_w == 0 || win_w <= 0 || (img_w as i32) <= win_w {
        return 1.0;
    }
    win_w as f64 / img_w as f64
}

/// 高さを領域に合わせる倍率（高さが領域より大きいときだけ縮小・拡大はしない）。幅ははみ出す。
pub fn fit_scale_height(img_h: u32, win_h: i32) -> f64 {
    if img_h == 0 || win_h <= 0 || (img_h as i32) <= win_h {
        return 1.0;
    }
    win_h as f64 / img_h as f64
}

/// なるべく大きく表示する倍率。各軸につき「領域より大きければ縮小・以下なら 1.0」を求め、
/// 大きい方（縮小の緩い方）を採る。結果として一辺が領域にぴったり、他辺ははみ出してスクロールになる。
pub fn fit_scale_look_large(img_w: u32, img_h: u32, win_w: i32, win_h: i32) -> f64 {
    let sw = fit_scale_width(img_w, win_w);
    let sh = fit_scale_height(img_h, win_h);
    sw.max(sh)
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

/// RGBA バッファを鏡像反転する。`horizontal` が真なら左右反転、偽なら上下反転。
pub fn flip_rgba(rgba: &[u8], w: u32, h: u32, horizontal: bool) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let mut out = vec![0u8; wu * hu * 4];
    for y in 0..hu {
        for x in 0..wu {
            let (sx, sy) = if horizontal {
                (wu - 1 - x, y)
            } else {
                (x, hu - 1 - y)
            };
            let s = (sy * wu + sx) * 4;
            let d = (y * wu + x) * 4;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    out
}

/// RGBA を CF_DIB 用バイト列へ変換する。クリップボード経由は透過を保てない消費側が多いので、
/// アルファは白背景へ合成して 24bpp・ボトムアップの DIB にする（どのアプリにも貼れる素直な形）。
pub fn rgba_to_clipboard_dib(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let stride = (wu * 3 + 3) & !3; // 各行を 4 バイト境界へ
    let img_size = stride * hu;
    let mut out = Vec::with_capacity(40 + img_size);
    // BITMAPINFOHEADER（40 バイト・リトルエンディアン）。
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(h as i32).to_le_bytes()); // biHeight（正＝ボトムアップ）
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression=BI_RGB
    out.extend_from_slice(&(img_size as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
    let mut rows = vec![0u8; img_size];
    for y in 0..hu {
        let dst_y = hu - 1 - y; // ボトムアップ
        let base = dst_y * stride;
        for x in 0..wu {
            let s = (y * wu + x) * 4;
            let a = rgba[s + 3] as u32;
            let over = |c: u8| -> u8 { ((c as u32 * a + 255 * (255 - a)) / 255) as u8 };
            let d = base + x * 3;
            rows[d] = over(rgba[s + 2]); // B
            rows[d + 1] = over(rgba[s + 1]); // G
            rows[d + 2] = over(rgba[s]); // R
        }
    }
    out.extend_from_slice(&rows);
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
    fn has_instance_icon_classifies_by_extension() {
        // 画像はサムネイル対象。
        assert!(has_instance_icon(".png"));
        // 実行体・ショートカット等は埋め込みアイコンがファイルごとに違う。
        assert!(has_instance_icon("EXE"));
        assert!(has_instance_icon(".lnk"));
        // 書庫・文書等は拡張子ごとに同じ汎用アイコンなので対象外。
        assert!(!has_instance_icon(".7z"));
        assert!(!has_instance_icon(".rar"));
        assert!(!has_instance_icon(".txt"));
        assert!(!has_instance_icon(""));
    }

    #[test]
    fn fit_shrinks_large_keeps_small() {
        assert_eq!(fit_scale(1000, 1000, 500, 500), 0.5);
        assert_eq!(fit_scale(100, 100, 500, 500), 1.0); // 原寸より拡大しない
        assert_eq!(fit_scale(400, 200, 200, 200), 0.5); // 幅が制約
        assert_eq!(fit_scale(0, 0, 200, 200), 1.0);
    }

    #[test]
    fn fit_width_shrinks_only_when_wider() {
        assert_eq!(fit_scale_width(1000, 500), 0.5); // 幅が領域超→縮小
        assert_eq!(fit_scale_width(300, 500), 1.0); // 幅が領域以下→拡大しない
        assert_eq!(fit_scale_width(0, 500), 1.0);
        assert_eq!(fit_scale_width(500, 0), 1.0);
    }

    #[test]
    fn fit_height_shrinks_only_when_taller() {
        assert_eq!(fit_scale_height(1000, 500), 0.5);
        assert_eq!(fit_scale_height(300, 500), 1.0);
        assert_eq!(fit_scale_height(0, 500), 1.0);
    }

    #[test]
    fn look_large_takes_looser_shrink() {
        // 幅 500→400(0.8)・高さ 200→160(0.8) 両方制約：緩い方＝大きい方を採る。
        assert_eq!(fit_scale_look_large(500, 200, 400, 150), 0.8);
        // 幅だけ制約・高さは収まる→ max(0.8, 1.0)=1.0（高さ基準で原寸のまま、幅ははみ出す）。
        assert_eq!(fit_scale_look_large(500, 200, 400, 300), 1.0);
        // 両方収まる→1.0（拡大しない）。
        assert_eq!(fit_scale_look_large(200, 100, 500, 500), 1.0);
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

    #[test]
    fn flip_mirrors_each_axis() {
        // 横並び「A B」(2x1)。
        let a = [255u8, 0, 0, 255];
        let b = [0u8, 255, 0, 255];
        let src: Vec<u8> = [a, b].concat();
        // 左右反転で A と B が入れ替わる。
        let hf = flip_rgba(&src, 2, 1, true);
        assert_eq!(&hf[0..4], &b);
        assert_eq!(&hf[4..8], &a);
        // 上下反転（1 行なので不変）。
        let vf = flip_rgba(&src, 2, 1, false);
        assert_eq!(vf, src);
        // 縦並び「A / B」(1x2) の上下反転で入れ替わる。
        let col: Vec<u8> = [a, b].concat();
        let vf2 = flip_rgba(&col, 1, 2, false);
        assert_eq!(&vf2[0..4], &b);
        assert_eq!(&vf2[4..8], &a);
    }

    #[test]
    fn clipboard_dib_header_and_padding() {
        // 1x1 の赤（不透明）。24bpp は 1 行 3 バイト→4 バイトへパディング。
        let rgba = [200u8, 50, 25, 255];
        let dib = rgba_to_clipboard_dib(&rgba, 1, 1);
        assert_eq!(dib.len(), 40 + 4, "ヘッダ40＋パディング後4バイト");
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40); // biSize
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 1); // biWidth
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 1); // biHeight
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 24); // biBitCount
        // 画素は BGR 並び。
        assert_eq!(&dib[40..43], &[25, 50, 200]);
    }

    #[test]
    fn clipboard_dib_alpha_over_white() {
        // 完全透明は白へ。半透明は白とのブレンド。
        let rgba = [0u8, 0, 0, 0, 0, 0, 0, 128]; // 2x1: 透明 / 半透明の黒
        let dib = rgba_to_clipboard_dib(&rgba, 2, 1);
        // 1 行 = 2px*3=6→8 バイト（パディング）。BGR×2＋詰め物2。
        assert_eq!(dib.len(), 40 + 8);
        assert_eq!(&dib[40..43], &[255, 255, 255]); // 透明→白
        // 半透明の黒（a=128）は約 (0*128+255*127)/255≈127。
        let mid = dib[43];
        assert!((125..=129).contains(&mid), "got {mid}");
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

    /// `w`×`h` の単色グラデ JPEG を作る。
    fn encode_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, _| image::Rgb([(x * 20) as u8, 100, 150]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    /// JPEG の SOI 直後に Orientation タグだけの最小 EXIF(APP1) を差し込む。
    fn with_exif_orientation(jpeg: &[u8], orient: u8) -> Vec<u8> {
        // TIFF（リトルエンディアン）：ヘッダ→IFD0（Orientation 1件）。
        let tiff: [u8; 26] = [
            0x49, 0x49, 0x2A, 0x00, // "II", 42
            0x08, 0x00, 0x00, 0x00, // IFD0 offset = 8
            0x01, 0x00, // エントリ数 1
            0x12, 0x01, // tag 0x0112 Orientation
            0x03, 0x00, // type SHORT
            0x01, 0x00, 0x00, 0x00, // count 1
            orient, 0x00, 0x00, 0x00, // value
            0x00, 0x00, 0x00, 0x00, // next IFD = 0
        ];
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff);
        let seg_len = (payload.len() + 2) as u16; // length は自身2バイトを含む
        let mut out = jpeg[0..2].to_vec(); // SOI
        out.extend_from_slice(&[0xFF, 0xE1, (seg_len >> 8) as u8, (seg_len & 0xFF) as u8]);
        out.extend_from_slice(&payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn exif_orientation_uprights_still_image() {
        // 横長 8×4。
        let jpeg = encode_jpeg(8, 4);
        // EXIF 無し → 寸法そのまま。
        let plain = StillImage::load(&jpeg).unwrap();
        assert_eq!(plain.dimensions(), (8, 4));
        // Orientation=1（無変換）→ そのまま。
        let id = StillImage::load(&with_exif_orientation(&jpeg, 1)).unwrap();
        assert_eq!(id.dimensions(), (8, 4));
        // Orientation=6（時計回り90°）→ 縦長 4×8 に正立。
        let rot = StillImage::load(&with_exif_orientation(&jpeg, 6)).unwrap();
        assert_eq!(rot.dimensions(), (4, 8));
        // Orientation=8（反時計回り90°）→ 同じく 4×8。
        let rot8 = StillImage::load(&with_exif_orientation(&jpeg, 8)).unwrap();
        assert_eq!(rot8.dimensions(), (4, 8));
    }

    #[test]
    fn exif_orientation_uprights_thumbnail() {
        // サムネイルも正立する（縦撮り写真のサムネが横倒しにならない）。
        // thumbnail は拡縮するので、寸法そのものでなく向き（縦長/横長）で判定する。
        let jpeg = encode_jpeg(8, 4); // 元は横長。
        let (w0, h0, _) = decode_thumbnail(&jpeg, 16).unwrap();
        assert!(w0 > h0, "EXIF 無し＝横長のまま ({w0}x{h0})");
        let (w, h, _) = decode_thumbnail(&with_exif_orientation(&jpeg, 6), 16).unwrap();
        assert!(h > w, "Orientation=6 で縦長に正立 ({w}x{h})");
    }

    #[test]
    fn decode_oriented_falls_back_on_broken_exif() {
        // 壊れた Orientation 値（範囲外）でも素デコードへフォールバックして画像は出す。
        let jpeg = encode_jpeg(8, 4);
        let broken = with_exif_orientation(&jpeg, 99);
        let img = StillImage::load(&broken).unwrap();
        assert_eq!(img.dimensions(), (8, 4)); // 補正されないが寸法は元のまま
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

    #[test]
    fn decode_thumbnail_fits_within_max_and_keeps_aspect() {
        // 80x40 の画像を長辺 16 に縮小。アスペクト比保持で 16x8 になり RGBA 長が一致。
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            80,
            40,
            image::Rgba([10, 20, 30, 255]),
        ))
        .write_to(&mut buf, image::ImageFormat::Png)
        .unwrap();
        let (w, h, rgba) = decode_thumbnail(buf.get_ref(), 16).unwrap();
        assert!(w <= 16 && h <= 16, "縮小後は max 以内: {w}x{h}");
        assert_eq!(w, 16);
        assert_eq!(h, 8);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        // 壊れたバイト列は None。
        assert!(decode_thumbnail(&[0, 1, 2, 3], 16).is_none());
    }
}
