use std::io;
use std::path::{Path, PathBuf};
use super::*;

/// 単体圧縮ファイル（gz/bz2/xz/zstd で1ファイルを包んだだけ）の読取バックエンド。中身は1
/// エントリ（圧縮拡張子を除いた名前）として見せ、読む時に丸ごと解凍する。1エントリなので
/// `random_access: true`（一括展開には倒さない）。書込み不可。
pub struct SingleFileBackend {
    path: PathBuf,
    comp: Comp,
    inner: String,
}

impl SingleFileBackend {
    pub(crate) fn open(path: &Path, comp: Comp) -> io::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            comp,
            inner: single_inner_name(path),
        })
    }
}

impl ArchiveBackend for SingleFileBackend {
    fn caps(&self) -> Caps {
        Caps {
            random_access: true,
            ..Default::default()
        }
    }

    fn list(&self) -> io::Result<Vec<ArchiveEntry>> {
        let packed = std::fs::metadata(&self.path).ok().map(|m| m.len());
        Ok(vec![ArchiveEntry {
            path: self.inner.clone(),
            is_dir: false,
            // 展開後サイズはメタから安く取れる形式（gz/xz）だけ埋める。取れなければ None＝
            // 表示は空欄（解凍してまで数えると一覧取得で UI が固まるため詐称しない）。
            size: uncompressed_size(&self.path, self.comp),
            packed_size: packed,
            mtime: None,
            is_encrypted: false,
        }])
    }

    fn read(&self, inner: &str) -> io::Result<Vec<u8>> {
        use std::io::Read;
        if normalize_inner(inner) != self.inner {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "書庫内ファイルが見つかりません",
            ));
        }
        let mut r = decoded_reader(&self.path, self.comp)?;
        let mut buf = Vec::new();
        r.read_to_end(&mut buf)?;
        Ok(buf)
    }

    fn read_capped(&self, inner: &str, cap: usize) -> io::Result<(Vec<u8>, bool)> {
        use std::io::Read;
        if normalize_inner(inner) != self.inner {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "書庫内ファイルが見つかりません",
            ));
        }
        let r = decoded_reader(&self.path, self.comp)?;
        let mut buf = Vec::new();
        r.take(cap as u64 + 1).read_to_end(&mut buf)?;
        let truncated = buf.len() > cap;
        buf.truncate(cap);
        Ok((buf, truncated))
    }
}

/// 単体圧縮ファイルの「展開後サイズ」を、**解凍せずに**コンテナのメタから安く得る。
/// gz は末尾 ISIZE、xz は末尾 Index から求める。取れない形式（bz2/zstd 等）は `None`。
fn uncompressed_size(path: &Path, comp: Comp) -> Option<u64> {
    match comp {
        Comp::Gz => gz_isize(path),
        Comp::Xz => xz_uncompressed_size(path),
        Comp::Zstd => zstd_content_size(path),
        _ => None,
    }
}

/// gzip 末尾4バイトの ISIZE（展開後サイズ mod 2^32）。4GB 超は値が回るが実用上十分。
fn gz_isize(path: &Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::End(-4)).ok()?;
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf).ok()?;
    Some(u32::from_le_bytes(buf) as u64)
}

/// .xz の Stream Footer→Index を辿り、全ブロックの Uncompressed Size を合算する（解凍不要）。
fn xz_uncompressed_size(path: &Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let file_len = f.seek(SeekFrom::End(0)).ok()?;
    if file_len < 12 {
        return None;
    }
    // Stream Footer（末尾12バイト）：CRC32(4) | Backward Size(4) | Stream Flags(2) | "YZ"(2)。
    f.seek(SeekFrom::End(-12)).ok()?;
    let mut footer = [0u8; 12];
    f.read_exact(&mut footer).ok()?;
    if &footer[10..12] != b"YZ" {
        return None;
    }
    let backward = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]) as u64;
    let index_size = backward.checked_add(1)?.checked_mul(4)?;
    let index_pos = file_len.checked_sub(12)?.checked_sub(index_size)?;
    f.seek(SeekFrom::Start(index_pos)).ok()?;
    let mut idx = vec![0u8; index_size as usize];
    f.read_exact(&mut idx).ok()?;
    // Index：Indicator(0x00) | Number of Records(varint) | {Unpadded, Uncompressed}... | pad | CRC32。
    if idx.first().copied()? != 0 {
        return None;
    }
    let mut p = 1usize;
    let count = read_xz_varint(&idx, &mut p)?;
    let mut total = 0u64;
    for _ in 0..count {
        let _unpadded = read_xz_varint(&idx, &mut p)?;
        let uncompressed = read_xz_varint(&idx, &mut p)?;
        total = total.checked_add(uncompressed)?;
    }
    Some(total)
}

/// zstd フレームヘッダの Frame_Content_Size（展開後サイズ）を読む（解凍不要）。ヘッダに
/// サイズが無い場合（FCS_flag=0 かつ非 single-segment）は `None`。
fn zstd_content_size(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 18];
    let n = f.read(&mut buf).ok()?;
    if n < 5 || buf[0..4] != [0x28, 0xB5, 0x2F, 0xFD] {
        return None;
    }
    let desc = buf[4];
    let fcs_flag = desc >> 6;
    let single_segment = (desc >> 5) & 1;
    let did_flag = desc & 0x3;
    let mut pos = 5usize;
    if single_segment == 0 {
        pos += 1; // Window_Descriptor
    }
    pos += match did_flag {
        1 => 1,
        2 => 2,
        3 => 4,
        _ => 0,
    };
    let fcs_size = match fcs_flag {
        0 if single_segment == 1 => 1,
        0 => return None,
        1 => 2,
        2 => 4,
        3 => 8,
        _ => return None,
    };
    if pos + fcs_size > n {
        return None;
    }
    let b = &buf[pos..pos + fcs_size];
    Some(match fcs_size {
        1 => b[0] as u64,
        2 => u16::from_le_bytes([b[0], b[1]]) as u64 + 256,
        4 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64,
        _ => u64::from_le_bytes(b.try_into().ok()?),
    })
}

/// xz の可変長整数（little-endian base-128・最上位ビット=継続）。
fn read_xz_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result = 0u64;
    let mut shift = 0u32;
    loop {
        let b = *buf.get(*pos)?;
        *pos += 1;
        result |= ((b & 0x7f) as u64).checked_shl(shift)?;
        if b & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}
