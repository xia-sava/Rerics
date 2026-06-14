//! 動画フレーム供給元（Windows Media Foundation 経由）。
//!
//! `IMFSourceReader` で動画を開き、出力を RGB32 に変換させてフレームを 1 枚ずつ取り出す。
//! OS のコーデック（H.264 標準・HEVC/AV1/VP9 は拡張インストール依存）に委ねるので DLL 同梱は
//! 不要。デコードできない形式では `open` が `None` を返し、呼び出し側はメッセージ表示にフォールする。
//! core の `FrameSource` を実装し、静止画/アニメと同じ消費経路（タイマ駆動・シークバー）に乗る。

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Once;
use std::time::Duration;

use rerics_core::{Frame, FrameSource};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFStartup, MFVideoFormat_RGB32, MFSTARTUP_LITE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READER_MEDIASOURCE, MF_VERSION,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Variant::{VT_I8, VT_UI8};
use windows::core::{GUID, PCWSTR};

static MF_INIT: Once = Once::new();

/// Media Foundation を 1 度だけ初期化する（プロセス終了まで有効）。
fn ensure_mf() {
    MF_INIT.call_once(|| unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_LITE);
    });
}

/// 動画のフレーム供給元。
pub struct VideoSource {
    reader: IMFSourceReader,
    width: u32,
    height: u32,
    /// 出力ストライド（バイト）。正なら bottom-up＝行を反転して top-down 化する。
    stride: i32,
    duration_ms: u64,
    position_ms: u64,
}

impl VideoSource {
    /// 動画を開く。デコードできなければ `None`（呼び出し側でメッセージ表示にフォール）。
    pub fn open(path: &Path) -> Option<Self> {
        ensure_mf();
        unsafe { Self::open_inner(path).ok() }
    }

    unsafe fn open_inner(path: &Path) -> windows::core::Result<Self> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

        // ビデオ処理（任意フォーマット→RGB 変換）を許可する属性つきでリーダを作る。
        let mut attrs: Option<IMFAttributes> = None;
        unsafe { MFCreateAttributes(&mut attrs, 1)? };
        let attrs = attrs.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        unsafe { attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)? };
        let reader =
            unsafe { MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attrs)? };

        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let all = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
        unsafe {
            reader.SetStreamSelection(all, false)?;
            reader.SetStreamSelection(video, true)?;
        }

        // 出力タイプを RGB32 に要求する。
        let mt = unsafe { MFCreateMediaType()? };
        unsafe {
            mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(video, None, &mt)?;
        }

        // 実際の出力タイプから寸法とストライドを得る。
        let cur = unsafe { reader.GetCurrentMediaType(video)? };
        let frame_size = unsafe { cur.GetUINT64(&MF_MT_FRAME_SIZE)? };
        let width = (frame_size >> 32) as u32;
        let height = (frame_size & 0xFFFF_FFFF) as u32;
        if width == 0 || height == 0 {
            return Err(windows::core::Error::from(E_FAIL));
        }
        let stride = unsafe { cur.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
            .map(|s| s as i32)
            .unwrap_or(width as i32 * 4);

        // 総再生時間（PROPVARIANT VT_UI8・100ns 単位）。取れなければ 0。
        let duration_ms = unsafe {
            match reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            {
                Ok(pv) => {
                    if pv.Anonymous.Anonymous.vt == VT_UI8 {
                        pv.Anonymous.Anonymous.Anonymous.uhVal / 10_000
                    } else {
                        0
                    }
                }
                Err(_) => 0,
            }
        };

        Ok(Self { reader, width, height, stride, duration_ms, position_ms: 0 })
    }

    /// 次のフレームを RGBA（top-down）で取り出す。`(rgba, delay_ms)`。終端は `None`。
    unsafe fn read_rgba(&mut self) -> Option<(Vec<u8>, u32)> {
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        // フォーマット変更等でサンプルが来ない回がある→数回までリトライ。
        for _ in 0..32 {
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample = None;
            unsafe {
                self.reader
                    .ReadSample(video, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))
                    .ok()?
            };
            if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                return None;
            }
            let Some(sample) = sample else { continue };
            self.position_ms = (ts / 10_000).max(0) as u64;
            let delay_ms = unsafe { sample.GetSampleDuration() }
                .map(|d| (d / 10_000).max(1) as u32)
                .unwrap_or(33);
            let buffer = unsafe { sample.ConvertToContiguousBuffer().ok()? };
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut len = 0u32;
            unsafe { buffer.Lock(&mut data, None, Some(&mut len)).ok()? };
            let rgba = unsafe { self.copy_to_rgba(data, len) };
            let _ = unsafe { buffer.Unlock() };
            return Some((rgba, delay_ms));
        }
        None
    }

    /// MF の RGB32（B,G,R,X・ストライド付き）を RGBA・top-down へ変換する。
    unsafe fn copy_to_rgba(&self, data: *mut u8, len: u32) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let abs_stride = self.stride.unsigned_abs() as usize;
        let src = unsafe { std::slice::from_raw_parts(data, len as usize) };
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            // MF の規約＝正ストライドは top-down・負は bottom-up。負のときだけ行を反転する。
            let src_row = if self.stride < 0 { h - 1 - y } else { y };
            let so = src_row * abs_stride;
            if so + w * 4 > src.len() {
                break;
            }
            let src_line = &src[so..so + w * 4];
            let dst_line = &mut out[y * w * 4..(y + 1) * w * 4];
            for (d, s) in dst_line.chunks_exact_mut(4).zip(src_line.chunks_exact(4)) {
                d[0] = s[2]; // R
                d[1] = s[1]; // G
                d[2] = s[0]; // B
                d[3] = 255; // A
            }
        }
        out
    }

    /// 100ns 単位の位置へシークする。
    unsafe fn seek_100ns(&self, t: u64) -> windows::core::Result<()> {
        let mut pv: PROPVARIANT = unsafe { std::mem::zeroed() };
        // ManuallyDrop 越しの個別代入は不可なので、ユニオン要素を丸ごと差し込む。
        pv.Anonymous.Anonymous = std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
            vt: VT_I8,
            wReserved1: 0,
            wReserved2: 0,
            wReserved3: 0,
            Anonymous: PROPVARIANT_0_0_0 { hVal: t as i64 },
        });
        let fmt = GUID::from_u128(0); // GUID_NULL = 100ns 時間形式
        unsafe { self.reader.SetCurrentPosition(&fmt, &pv) }
    }
}

impl FrameSource for VideoSource {
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn is_animated(&self) -> bool {
        true
    }

    fn next_frame(&mut self) -> Option<Frame> {
        let (rgba, delay) = unsafe { self.read_rgba() }?;
        Some(Frame {
            width: self.width,
            height: self.height,
            rgba,
            delay: Some(Duration::from_millis(delay as u64)),
        })
    }

    fn reset(&mut self) {
        unsafe {
            let _ = self.seek_100ns(0);
        }
        self.position_ms = 0;
    }

    fn frame_count(&self) -> usize {
        0
    }

    fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    fn position_ms(&self) -> u64 {
        self.position_ms
    }

    fn seek(&mut self, ms: u64) {
        unsafe {
            let _ = self.seek_100ns(ms.saturating_mul(10_000));
        }
        self.position_ms = ms;
    }
}
