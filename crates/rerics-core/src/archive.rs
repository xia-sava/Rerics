//! 書庫（アーカイブ）を読むバックエンド層（UI 非依存）。
//!
//! `ArchiveBackend` が「能力申告(caps)・一覧(list)・1ファイル読取(read)」を提供する。
//! 書込みは別 trait `ArchiveWriter` に分離し、現状は seam（定義）のみ確保する
//! （後から書込みを足しても read 側を壊さないため）。`open_archive` が拡張子から
//! 実装を選ぶ。`entries_at` がフラットなパス列を現在地直下の1段ツリー（`FileItem`
//! 列）へ畳む。

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::FileItem;

/// バックエンドの能力。形式ごとに異なってよい（UI はこれで操作可否を出し分ける）。
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// 1ファイルだけを直接展開できる（順次専用なら false）。
    pub random_access: bool,
    /// 書庫自体への書込み（追加/更新/削除）が可能。
    pub writable: bool,
}

/// 書庫内の1エントリ（フラットなパスと最小メタデータ）。
#[derive(Clone, Debug)]
pub struct ArchiveEntry {
    /// 書庫内フルパス。区切りは '/'・末尾 '/' なし・"" はルート。
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub packed_size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub is_encrypted: bool,
}

/// 読取バックエンド。初期バージョンはこれだけ実装する（read-only）。
pub trait ArchiveBackend {
    fn caps(&self) -> Caps;
    /// 全エントリ（フラットなパス列）。
    fn list(&self) -> io::Result<Vec<ArchiveEntry>>;
    /// 1エントリの bytes を取り出す。`inner` は '/' 区切り・正規化済みの書庫内パス。
    fn read(&self, inner: &str) -> io::Result<Vec<u8>>;
    /// 先頭 `cap` バイトまでを取り出す（超過していたら `truncated=true`）。ビューア表示
    /// 用途で巨大/展開爆弾エントリのフル解凍を避けるための上限付き読取。既定実装は
    /// `read` 後に切り詰めるだけ（解凍総量は減らない）＝ストリーム読みできる backend は
    /// 解凍自体を打ち切るよう override する。
    fn read_capped(&self, inner: &str, cap: usize) -> io::Result<(Vec<u8>, bool)> {
        let mut bytes = self.read(inner)?;
        let truncated = bytes.len() > cap;
        bytes.truncate(cap);
        Ok((bytes, truncated))
    }
}

/// 書込みバックエンド（後付け用の seam・現状は実装しない）。
///
/// 将来 `Caps.writable == true` の backend のみがこれを提供する。メソッド名は
/// 実FS/FTP 等の書込み操作に対応づけ、後から差し込んでも read trait を汚さない形にする。
pub trait ArchiveWriter {
    fn add(&mut self, inner: &str, bytes: &[u8]) -> io::Result<()>;
    fn update(&mut self, inner: &str, bytes: &[u8]) -> io::Result<()>;
    fn remove(&mut self, inner: &str) -> io::Result<()>;
    fn rename(&mut self, inner: &str, new: &str) -> io::Result<()>;
    fn mkdir(&mut self, inner: &str) -> io::Result<()>;
}

/// 拡張子から読取バックエンドを選ぶ。未対応形式は `Unsupported` エラー。
pub fn open_archive(path: &Path) -> io::Result<Box<dyn ArchiveBackend>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("zip") => Ok(Box::new(ZipBackend::open(path)?)),
        _ => Err(io::Error::new(io::ErrorKind::Unsupported, "未対応の書庫形式")),
    }
}

/// zip 書庫の読取バックエンド。パスのみ保持し list/read 毎に開き直す（単純・安全）。
pub struct ZipBackend {
    path: PathBuf,
}

impl ZipBackend {
    /// 開けることを確認して構築する（壊れた書庫はここで弾く）。
    pub fn open(path: &Path) -> io::Result<Self> {
        let f = std::fs::File::open(path)?;
        zip::ZipArchive::new(f).map_err(zip_err)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    fn archive(&self) -> io::Result<zip::ZipArchive<std::fs::File>> {
        let f = std::fs::File::open(&self.path)?;
        zip::ZipArchive::new(f).map_err(zip_err)
    }

    /// 名前一致するエントリを最大 `limit` バイト読む（`None` で全部）。戻りは `(bytes, truncated)`。
    /// `by_name` は zip 内部の UTF-8 化名を使い CP932 名と一致しないため、index 走査で
    /// 生バイト名を自前デコードして突き合わせる。`limit` 指定時は解凍自体を `take` で打ち切る。
    fn read_entry(&self, inner: &str, limit: Option<usize>) -> io::Result<(Vec<u8>, bool)> {
        use std::io::Read;
        let want = normalize_inner(inner);
        let mut zip = self.archive()?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(zip_err)?;
            let name = normalize_inner(&decode_name(f.name_raw()));
            if name != want {
                continue;
            }
            if f.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "ディレクトリは読めません",
                ));
            }
            return match limit {
                Some(cap) => {
                    let mut buf = Vec::new();
                    f.take(cap as u64 + 1).read_to_end(&mut buf)?;
                    let truncated = buf.len() > cap;
                    buf.truncate(cap);
                    Ok((buf, truncated))
                }
                None => {
                    // 申告サイズは未検証（壊れた/細工書庫は巨大値を書けて、事前確保だけで
                    // OOM abort し得る）。上限でクランプし、不足分は read_to_end の拡張に任せる。
                    const PREALLOC_CAP: usize = 16 * 1024 * 1024;
                    let mut buf = Vec::with_capacity((f.size() as usize).min(PREALLOC_CAP));
                    f.read_to_end(&mut buf)?;
                    Ok((buf, false))
                }
            };
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "書庫内ファイルが見つかりません",
        ))
    }
}

impl ArchiveBackend for ZipBackend {
    fn caps(&self) -> Caps {
        Caps {
            random_access: true,
            writable: false,
        }
    }

    fn list(&self) -> io::Result<Vec<ArchiveEntry>> {
        let mut zip = self.archive()?;
        let mut out = Vec::with_capacity(zip.len());
        for i in 0..zip.len() {
            let f = zip.by_index(i).map_err(zip_err)?;
            let raw = f.name_raw();
            let is_dir = f.is_dir() || raw.last() == Some(&b'/');
            let path = normalize_inner(&decode_name(raw));
            if path.is_empty() {
                continue;
            }
            out.push(ArchiveEntry {
                path,
                is_dir,
                size: Some(f.size()),
                packed_size: Some(f.compressed_size()),
                mtime: zip_mtime(f.last_modified()),
                is_encrypted: f.encrypted(),
            });
        }
        Ok(out)
    }

    fn read(&self, inner: &str) -> io::Result<Vec<u8>> {
        Ok(self.read_entry(inner, None)?.0)
    }

    fn read_capped(&self, inner: &str, cap: usize) -> io::Result<(Vec<u8>, bool)> {
        self.read_entry(inner, Some(cap))
    }
}

fn zip_err(e: zip::result::ZipError) -> io::Error {
    io::Error::other(e.to_string())
}

/// 書庫内パスを正規化：'\\' を '/' に、空セグメントと "." を除去して '/' 区切りへ。
/// 先頭/連続/末尾スラッシュが畳まれ、空文字＝ルート。".." は読取側では素の
/// セグメントとして残す（実FS への展開時のサニタイズは展開コピー側で別途行う）。
fn normalize_inner(s: &str) -> String {
    s.replace('\\', "/")
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// 生バイト名を文字列へ復号する。valid UTF-8 ならそのまま（UTF-8 フラグ付きの
/// 現代 zip・ASCII）、不正なら CP932(Shift_JIS) とみなす（フラグ無しの旧 zip）。
fn decode_name(raw: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_owned();
    }
    let (cow, _, _) = encoding_rs::SHIFT_JIS.decode(raw);
    cow.into_owned()
}

/// zip の DOS 日時（ローカル壁時計）を `SystemTime` へ。曖昧/範囲外は None。
fn zip_mtime(dt: Option<zip::DateTime>) -> Option<SystemTime> {
    use chrono::{Local, TimeZone, Utc};
    let dt = dt?;
    let naive = chrono::NaiveDate::from_ymd_opt(dt.year() as i32, dt.month() as u32, dt.day() as u32)?
        .and_hms_opt(dt.hour() as u32, dt.minute() as u32, dt.second() as u32)?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|local| local.with_timezone(&Utc).into())
}

/// フラットな全エントリ `all` から、`prefix` 直下の1段分を `FileItem` 列に畳む。
///
/// `prefix` は正規化済み（'/' 区切り・末尾 '/' なし・"" = ルート）。中間 dir の
/// 明示エントリが無くても（`a/b/c.txt` だけでも `a` を）拾う。"`..`" は付けない
/// （`read_items` と同じく呼び側が先頭に付ける）。ソートもしない（GUI 側の責務）。
pub(crate) fn entries_at(all: &[ArchiveEntry], prefix: &str) -> Vec<FileItem> {
    use std::collections::BTreeSet;
    let pfx = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    let mut files: Vec<FileItem> = Vec::new();
    for e in all {
        let rest = match e.path.strip_prefix(&pfx) {
            Some(r) if !r.is_empty() => r,
            _ => continue,
        };
        match rest.find('/') {
            Some(idx) => {
                dirs.insert(rest[..idx].to_string());
            }
            None => {
                if e.is_dir {
                    dirs.insert(rest.to_string());
                } else {
                    let mut it = FileItem::bare(rest.to_string(), false);
                    it.size = e.size;
                    it.modified = e.mtime;
                    files.push(it);
                }
            }
        }
    }
    let mut out: Vec<FileItem> = dirs.into_iter().map(|d| FileItem::bare(d, true)).collect();
    out.append(&mut files);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Location;

    /// テスト用の一意な temp パス（同プロセス内の並行テストは tag で区別）。
    fn temp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rerics_arc_{}_{}.zip", std::process::id(), tag));
        p
    }

    /// deflate 圧縮で zip を生成する（ASCII 名・UTF-8 フラグ付き）。
    fn build_zip(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        let f = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        for (name, data) in entries {
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }

    /// 標準 CRC-32（IEEE・反転多項式）。手組み stored zip の検証値用。
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    /// 無圧縮(stored)・UTF-8 フラグ無しで任意の生バイト名 zip を手組みする
    /// （CP932 名の検証用。高レベル writer は UTF-8 フラグを立ててしまうため）。
    fn build_stored_zip_raw(path: &Path, entries: &[(&[u8], &[u8])]) {
        fn u16le(v: &mut Vec<u8>, x: u16) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        fn u32le(v: &mut Vec<u8>, x: u32) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data) in entries {
            let crc = crc32(data);
            let off = out.len() as u32;
            // local file header
            u32le(&mut out, 0x0403_4b50);
            u16le(&mut out, 20); // version needed
            u16le(&mut out, 0); // flags（UTF-8 ビット無し）
            u16le(&mut out, 0); // method stored
            u16le(&mut out, 0); // time
            u16le(&mut out, 0); // date
            u32le(&mut out, crc);
            u32le(&mut out, data.len() as u32);
            u32le(&mut out, data.len() as u32);
            u16le(&mut out, name.len() as u16);
            u16le(&mut out, 0); // extra len
            out.extend_from_slice(name);
            out.extend_from_slice(data);
            // central directory header
            u32le(&mut central, 0x0201_4b50);
            u16le(&mut central, 20); // version made by
            u16le(&mut central, 20); // version needed
            u16le(&mut central, 0); // flags
            u16le(&mut central, 0); // method
            u16le(&mut central, 0); // time
            u16le(&mut central, 0); // date
            u32le(&mut central, crc);
            u32le(&mut central, data.len() as u32);
            u32le(&mut central, data.len() as u32);
            u16le(&mut central, name.len() as u16);
            u16le(&mut central, 0); // extra
            u16le(&mut central, 0); // comment
            u16le(&mut central, 0); // disk start
            u16le(&mut central, 0); // internal attrs
            u32le(&mut central, 0); // external attrs
            u32le(&mut central, off);
            central.extend_from_slice(name);
        }
        let cd_off = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        // end of central directory
        u32le(&mut out, 0x0605_4b50);
        u16le(&mut out, 0);
        u16le(&mut out, 0);
        u16le(&mut out, entries.len() as u16);
        u16le(&mut out, entries.len() as u16);
        u32le(&mut out, cd_size);
        u32le(&mut out, cd_off);
        u16le(&mut out, 0);
        std::fs::write(path, &out).unwrap();
    }

    #[test]
    fn decode_name_utf8_and_sjis() {
        assert_eq!(decode_name("日本語".as_bytes()), "日本語");
        // CP932 の "日本語"（フラグ無しの旧 zip 相当）
        assert_eq!(decode_name(&[0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]), "日本語");
        assert_eq!(decode_name(b"ascii.txt"), "ascii.txt");
    }

    #[test]
    fn normalize_inner_strips() {
        assert_eq!(normalize_inner("a/b/"), "a/b");
        assert_eq!(normalize_inner("a\\b"), "a/b");
        assert_eq!(normalize_inner("/"), "");
    }

    #[test]
    fn list_and_read_deflate() {
        let path = temp_path("deflate");
        build_zip(
            &path,
            &[("a.txt", b"AAA"), ("b/c.txt", b"CCC"), ("b/d.txt", b"DDD")],
        );
        let be = ZipBackend::open(&path).unwrap();
        let list = be.list().unwrap();
        assert!(list
            .iter()
            .any(|e| e.path == "a.txt" && !e.is_dir && e.size == Some(3)));
        assert!(list.iter().any(|e| e.path == "b/c.txt"));
        assert_eq!(be.read("a.txt").unwrap(), b"AAA");
        assert_eq!(be.read("b/c.txt").unwrap(), b"CCC");
        assert!(be.read("missing").is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_capped_truncates_and_passes_through() {
        let path = temp_path("capped");
        build_zip(&path, &[("big.txt", b"0123456789ABCDEF")]);
        let be = ZipBackend::open(&path).unwrap();
        let (head, truncated) = be.read_capped("big.txt", 4).unwrap();
        assert_eq!(head, b"0123");
        assert!(truncated);
        let (full, trunc2) = be.read_capped("big.txt", 100).unwrap();
        assert_eq!(full, b"0123456789ABCDEF");
        assert!(!trunc2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn entries_at_root_and_sub() {
        let path = temp_path("tree");
        build_zip(
            &path,
            &[("a.txt", b"A"), ("b/c.txt", b"C"), ("b/d.txt", b"D")],
        );
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        let root = entries_at(&all, "");
        // 暗黙ディレクトリ b を拾い、dir 優先で並ぶ
        assert!(root.iter().any(|i| i.name == "b" && i.is_dir));
        assert!(root.iter().any(|i| i.name == "a.txt" && !i.is_dir));
        let sub = entries_at(&all, "b");
        let names: Vec<_> = sub.iter().map(|i| i.name.clone()).collect();
        assert!(names.contains(&"c.txt".to_string()));
        assert!(names.contains(&"d.txt".to_string()));
        assert_eq!(sub.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cp932_name_end_to_end() {
        let mut name = vec![0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]; // 日本語
        name.extend_from_slice(b".txt");
        let path = temp_path("cp932");
        build_stored_zip_raw(&path, &[(&name, b"hello")]);
        let be = ZipBackend::open(&path).unwrap();
        let list = be.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, "日本語.txt");
        assert!(!list[0].is_dir);
        assert_eq!(be.read("日本語.txt").unwrap(), b"hello");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_zip_lists_nothing() {
        let path = temp_path("empty");
        build_zip(&path, &[]);
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        assert!(all.is_empty());
        assert!(entries_at(&all, "").is_empty());
        assert!(be.read("anything").is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deep_nest_folds_one_level_per_step() {
        let path = temp_path("deep");
        build_zip(&path, &[("a/b/c/d.txt", b"D")]);
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        assert!(entries_at(&all, "").iter().any(|i| i.name == "a" && i.is_dir));
        assert!(entries_at(&all, "a").iter().any(|i| i.name == "b" && i.is_dir));
        assert!(entries_at(&all, "a/b").iter().any(|i| i.name == "c" && i.is_dir));
        let leaf = entries_at(&all, "a/b/c");
        assert_eq!(leaf.len(), 1);
        assert!(leaf.iter().any(|i| i.name == "d.txt" && !i.is_dir));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn explicit_dir_entry_and_read_dir_errors() {
        use std::io::Write;
        let path = temp_path("dironly");
        {
            let f = std::fs::File::create(&path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            zw.add_directory("emptydir", opts).unwrap();
            zw.start_file("emptydir/inner.txt", opts).unwrap();
            zw.write_all(b"I").unwrap();
            zw.finish().unwrap();
        }
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        assert!(all.iter().any(|e| e.path == "emptydir" && e.is_dir));
        assert!(entries_at(&all, "").iter().any(|i| i.name == "emptydir" && i.is_dir));
        // ディレクトリを read するとエラー（ファイルではない）。
        assert!(be.read("emptydir").is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn leading_and_dot_segments_make_no_phantom_dir() {
        // 先頭スラッシュ/"." セグメント付きの生バイト名（一部ツールが生成する）。
        let path = temp_path("phantom");
        build_stored_zip_raw(&path, &[(b"/abs/file.txt", b"X"), (b"./root.txt", b"Y")]);
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        assert!(all.iter().any(|e| e.path == "abs/file.txt"));
        assert!(all.iter().any(|e| e.path == "root.txt"));
        let root = entries_at(&all, "");
        // 空名の幽霊ディレクトリが現れない。
        assert!(root.iter().all(|i| !i.name.is_empty()));
        assert!(root.iter().any(|i| i.name == "abs" && i.is_dir));
        assert!(root.iter().any(|i| i.name == "root.txt" && !i.is_dir));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn location_parse_detects_archive_boundary() {
        let dir = std::env::temp_dir().join(format!("rerics_parse_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("p.zip");
        build_zip(&zip, &[("a.txt", b"A"), ("b/c.txt", b"C")]);

        // 実在ディレクトリ → Real
        assert!(!Location::parse(&dir.to_string_lossy()).is_archive());
        // 書庫ルート → Archive{inner=""}
        let a = Location::parse(&zip.to_string_lossy());
        assert!(matches!(&a, Location::Archive { inner, .. } if inner.is_empty()));
        // 書庫内 inner（OS セパレータ）→ Archive{inner="b"}
        let sub = zip.join("b");
        let s = Location::parse(&sub.to_string_lossy());
        assert!(matches!(&s, Location::Archive { inner, .. } if inner == "b"));
        // 存在しないパス → Real フォールバック
        assert!(!Location::parse("C:\\no\\such\\dir_xyz_zzz").is_archive());

        let _ = std::fs::remove_file(&zip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn location_enter_and_parent() {
        let dir = std::env::temp_dir().join(format!("rerics_nav_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("test.zip");
        build_zip(&zip, &[("a.txt", b"A"), ("b/c.txt", b"C")]);

        let root = Location::Real(dir.clone());
        // 書庫ファイルへ潜る（is_dir=false）
        let inzip = root.enter("test.zip", false).unwrap();
        assert!(inzip.is_archive());
        let items = inzip.read().unwrap();
        assert!(items.iter().any(|i| i.is_parent));
        assert!(items.iter().any(|i| i.name == "b" && i.is_dir));
        assert!(items.iter().any(|i| i.name == "a.txt"));

        // 書庫内 dir へ潜る
        let inb = inzip.enter("b", true).unwrap();
        assert!(inb.read().unwrap().iter().any(|i| i.name == "c.txt"));

        // b の親＝書庫ルート、出てきた名前は "b"
        let (par, prev) = inb.to_parent().unwrap();
        assert_eq!(prev, "b");
        assert!(matches!(&par, Location::Archive { inner, .. } if inner.is_empty()));

        // 書庫ルートの親＝実 dir、出てきた名前は書庫ファイル名
        let (par2, prev2) = par.to_parent().unwrap();
        assert_eq!(prev2, "test.zip");
        assert_eq!(par2.as_real_path(), Some(dir.as_path()));

        let _ = std::fs::remove_file(&zip);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
