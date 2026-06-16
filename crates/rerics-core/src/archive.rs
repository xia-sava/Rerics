//! 書庫（アーカイブ）を読むバックエンド層（UI 非依存）。
//!
//! `ArchiveBackend` が「能力申告(caps)・一覧(list)・1ファイル読取(read)」を提供する。
//! 書込みは別 trait `ArchiveWriter` に分離し、現状は seam（定義）のみ確保する
//! （後から書込みを足しても read 側を壊さないため）。`open_archive` が拡張子から
//! 実装を選ぶ。`entries_at` がフラットなパス列を現在地直下の1段ツリー（`FileItem`
//! 列）へ畳む。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::FileItem;

/// バックエンドの能力。形式ごとに異なってよい（UI はこれで操作可否を出し分ける）。書込みは
/// 操作別に持つ：**追加/mkdir は append で既存を壊さず CP932 安全**、削除/リネームは全体
/// リビルドが要り CP932 名の再エンコード判断が絡む（後段）。
#[derive(Clone, Copy, Debug, Default)]
pub struct Caps {
    /// 1ファイルだけを直接展開できる（順次専用なら false）。
    pub random_access: bool,
    /// 既存を壊さず新規ファイルを追加できる（append・CP932 安全）。
    pub can_add: bool,
    /// 書庫内にディレクトリを作れる（append・CP932 安全）。
    pub can_mkdir: bool,
    /// 既存エントリを削除できる（要リビルド）。
    pub can_remove: bool,
    /// 既存エントリをリネームできる（要リビルド）。
    pub can_rename: bool,
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
    /// パスワード付きで1エントリを読む。`password=None` は [`read`](Self::read) と同じ。
    /// 暗号化に対応しない backend は password を無視する（既定実装）。暗号化エントリを
    /// パスワード無し/誤りで読もうとすると backend 依存のエラーになる。
    fn read_with_password(&self, inner: &str, password: Option<&[u8]>) -> io::Result<Vec<u8>> {
        let _ = password;
        self.read(inner)
    }
    /// 全エントリを `dest` 配下へ展開する（非ランダムアクセス＝ソリッド書庫の一括展開用）。
    /// 各ファイルを展開する直前に `each(inner, done, total)` を呼び、`false` が返ったら
    /// その時点で中断する（done は中断前まで展開できた件数）。`dest` は呼び側が用意した
    /// 空ディレクトリを想定し、エントリ名は `safe_join` で zip-slip を弾く。
    ///
    /// 既定実装は `list`＋`read` のループ（任意 backend で動くが、ソリッドでは
    /// ブロックを毎回頭から復号し直すため O(n²)）。ストリーム展開できる backend
    /// （7z 等）は単一パスで `override` する。
    fn extract_all(
        &self,
        dest: &Path,
        each: &mut dyn FnMut(&str, u64, u64) -> bool,
    ) -> io::Result<()> {
        let entries = self.list()?;
        let total = entries.iter().filter(|e| !e.is_dir).count() as u64;
        for e in &entries {
            if e.is_dir {
                if let Some(p) = safe_join(dest, &e.path) {
                    std::fs::create_dir_all(p)?;
                }
            }
        }
        let mut done = 0u64;
        for e in &entries {
            if e.is_dir {
                continue;
            }
            if !each(&e.path, done, total) {
                return Ok(());
            }
            let Some(p) = safe_join(dest, &e.path) else {
                continue;
            };
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = self.read(&e.path)?;
            std::fs::write(&p, &bytes)?;
            done += 1;
        }
        Ok(())
    }
}

/// 書込みバックエンド。`caps()` の対応フラグが立つ backend だけが意味のある実装を持つ。
/// メソッド名は実FS/FTP 等の書込み操作に対応づけ、read trait を汚さない形にする。
/// add/mkdir は append（既存を触らない＝CP932 安全）、update/remove/rename は要リビルド。
pub trait ArchiveWriter {
    fn add(&mut self, inner: &str, bytes: &[u8]) -> io::Result<()>;
    fn update(&mut self, inner: &str, bytes: &[u8]) -> io::Result<()>;
    fn remove(&mut self, inner: &str) -> io::Result<()>;
    fn rename(&mut self, inner: &str, new: &str) -> io::Result<()>;
    fn mkdir(&mut self, inner: &str) -> io::Result<()>;
}

/// 書込み backend を選ぶ。対応形式（現状 zip のみ）以外は `Unsupported`。
pub fn open_archive_writer(path: &Path) -> io::Result<Box<dyn ArchiveWriter>> {
    match classify_archive(path) {
        Some(ArchiveKind::Zip) => Ok(Box::new(ZipWriter::open(path)?)),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "この書庫形式は書込み未対応",
        )),
    }
}

/// zip の書込み（append）。`new_append` で開いて新規エントリだけを足す＝**既存エントリの
/// 生バイト名を一切触らないので CP932 名も無傷**。update/remove/rename は全体リビルドが要り
/// CP932 名の再エンコード判断が絡むため、現状は未対応エラー。
pub struct ZipWriter {
    path: PathBuf,
}

impl ZipWriter {
    pub fn open(path: &Path) -> io::Result<Self> {
        // 開ける zip か確認する。
        let f = std::fs::File::open(path)?;
        zip::ZipArchive::new(f).map_err(zip_err)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// append で開いた ZipWriter を得る（既存エントリは読み込まれるが finish で生のまま書く）。
    fn appender(&self) -> io::Result<zip::ZipWriter<std::fs::File>> {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)?;
        zip::ZipWriter::new_append(f).map_err(zip_err)
    }
}

impl ArchiveWriter for ZipWriter {
    fn add(&mut self, inner: &str, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let name = normalize_inner(inner);
        if name.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "空のエントリ名"));
        }
        let mut zw = self.appender()?;
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zw.start_file(&name, opts).map_err(zip_err)?;
        zw.write_all(bytes)?;
        zw.finish().map_err(zip_err)?;
        Ok(())
    }

    fn mkdir(&mut self, inner: &str) -> io::Result<()> {
        let name = normalize_inner(inner);
        if name.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "空のディレクトリ名"));
        }
        let mut zw = self.appender()?;
        zw.add_directory(&name, zip::write::SimpleFileOptions::default())
            .map_err(zip_err)?;
        zw.finish().map_err(zip_err)?;
        Ok(())
    }

    fn update(&mut self, _inner: &str, _bytes: &[u8]) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "更新は未対応"))
    }
    fn remove(&mut self, _inner: &str) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "削除は未対応"))
    }
    fn rename(&mut self, _inner: &str, _new: &str) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "リネームは未対応"))
    }
}

/// 圧縮レイヤの種別（tar 系のラップ／単体圧縮で共有）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Comp {
    None,
    Gz,
    Bz2,
    Xz,
    Zstd,
}

/// 書庫の種別（拡張子から決定）。二重拡張子（.tar.gz 等）を見るため file_name 全体で判定する。
enum ArchiveKind {
    Zip,
    SevenZ,
    #[cfg(feature = "rar")]
    Rar,
    /// tar 本体＋ラップ圧縮（None=無圧縮）。
    Tar(Comp),
    /// 単体圧縮ファイル（1エントリ）。
    Single(Comp),
}

/// ファイル名（小文字化）から書庫種別を決める。未知は `None`。rar は feature 無効時は
/// 「書庫でない」扱い（従来どおり関連付け起動へ回す）。
fn classify_archive(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let ends = |s: &str| name.ends_with(s);
    // tar 系（二重拡張子・短縮形）。
    if ends(".tar") {
        return Some(ArchiveKind::Tar(Comp::None));
    }
    if ends(".tar.gz") || ends(".tgz") {
        return Some(ArchiveKind::Tar(Comp::Gz));
    }
    if ends(".tar.bz2") || ends(".tbz2") || ends(".tbz") {
        return Some(ArchiveKind::Tar(Comp::Bz2));
    }
    if ends(".tar.xz") || ends(".txz") {
        return Some(ArchiveKind::Tar(Comp::Xz));
    }
    if ends(".tar.zst") || ends(".tar.zstd") || ends(".tzst") {
        return Some(ArchiveKind::Tar(Comp::Zstd));
    }
    // 単体圧縮（.tar.* は上で捌け済み）。
    if ends(".gz") {
        return Some(ArchiveKind::Single(Comp::Gz));
    }
    if ends(".bz2") {
        return Some(ArchiveKind::Single(Comp::Bz2));
    }
    if ends(".xz") {
        return Some(ArchiveKind::Single(Comp::Xz));
    }
    if ends(".zst") || ends(".zstd") {
        return Some(ArchiveKind::Single(Comp::Zstd));
    }
    if ends(".zip") {
        return Some(ArchiveKind::Zip);
    }
    if ends(".7z") {
        return Some(ArchiveKind::SevenZ);
    }
    if ends(".rar") {
        #[cfg(feature = "rar")]
        return Some(ArchiveKind::Rar);
        #[cfg(not(feature = "rar"))]
        return None;
    }
    None
}

/// 既知の書庫拡張子か（GUI の「潜れる書庫」判定が実在チェックと併せて使う）。
pub fn is_known_archive(path: &Path) -> bool {
    classify_archive(path).is_some()
}

/// 拡張子から読取バックエンドを選ぶ。未対応形式は `Unsupported` エラー。
pub fn open_archive(path: &Path) -> io::Result<Box<dyn ArchiveBackend>> {
    match classify_archive(path) {
        Some(ArchiveKind::Zip) => Ok(Box::new(ZipBackend::open(path)?)),
        Some(ArchiveKind::SevenZ) => Ok(Box::new(SevenZBackend::open(path)?)),
        #[cfg(feature = "rar")]
        Some(ArchiveKind::Rar) => Ok(Box::new(RarBackend::open(path)?)),
        Some(ArchiveKind::Tar(comp)) => Ok(Box::new(TarBackend::open(path, comp)?)),
        Some(ArchiveKind::Single(comp)) => Ok(Box::new(SingleFileBackend::open(path, comp)?)),
        _ => Err(io::Error::new(io::ErrorKind::Unsupported, "未対応の書庫形式")),
    }
}

/// 任意の Read を圧縮種別に応じた解凍ストリームへラップする（`Comp::None` は素通し）。
fn wrap_comp<R: io::Read + 'static>(r: R, comp: Comp) -> io::Result<Box<dyn io::Read>> {
    Ok(match comp {
        Comp::None => Box::new(r),
        Comp::Gz => Box::new(flate2::read::GzDecoder::new(r)),
        Comp::Bz2 => Box::new(bzip2::read::BzDecoder::new(r)),
        Comp::Xz => Box::new(lzma_rust2::XzReader::new(r, true)),
        Comp::Zstd => {
            Box::new(ruzstd::decoding::StreamingDecoder::new(r).map_err(io::Error::other)?)
        }
    })
}

/// 圧縮種別に応じてファイルを解凍ストリームにラップする。
fn decoded_reader(path: &Path, comp: Comp) -> io::Result<Box<dyn io::Read>> {
    wrap_comp(std::fs::File::open(path)?, comp)
}

/// 元の読み取りバイト数を `count` に積む薄いラッパ。tar の一括展開で「圧縮ファイルを
/// どこまで消費したか」を進捗（バイト基準）に使う（順次 tar は件数を事前に数えられない）。
struct CountingReader<R> {
    inner: R,
    count: Arc<AtomicU64>,
}

impl<R: io::Read> io::Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

// unrar_sys（vendored C++）はレジストリ/トークン/Crypt 等の Win32 API を参照するが、
// その build.rs が advapi32 のリンクを指定しないため未解決シンボルになる。ここで空の
// link ブロックを置いて advapi32 を明示リンクする（`rar` feature 時のみ）。
#[cfg(all(feature = "rar", windows))]
#[link(name = "advapi32")]
unsafe extern "C" {}

/// rar 書庫の読取バックエンド（`rar` feature・unrar crate＝vendored C++）。RAR は
/// ヘッダ順次アクセスなので `random_access: false`（単体取り出しも先頭から走査する）。
/// 書込みは不可。UnRAR は非free ライセンス（展開のみ許可）。
#[cfg(feature = "rar")]
pub struct RarBackend {
    path: PathBuf,
}

#[cfg(feature = "rar")]
impl RarBackend {
    /// 一覧が取れることを確認して構築する（壊れた/未対応はここで弾く）。
    pub fn open(path: &Path) -> io::Result<Self> {
        unrar::Archive::new(path)
            .open_for_listing()
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

#[cfg(feature = "rar")]
impl ArchiveBackend for RarBackend {
    fn caps(&self) -> Caps {
        Caps {
            random_access: false,
            ..Default::default()
        }
    }

    fn list(&self) -> io::Result<Vec<ArchiveEntry>> {
        let arc = unrar::Archive::new(&self.path)
            .open_for_listing()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut out = Vec::new();
        for entry in arc {
            let h = entry.map_err(|e| io::Error::other(e.to_string()))?;
            let path = normalize_inner(&h.filename.to_string_lossy());
            if path.is_empty() {
                continue;
            }
            out.push(ArchiveEntry {
                path,
                is_dir: h.is_directory(),
                size: Some(h.unpacked_size),
                packed_size: None,
                mtime: None,
                is_encrypted: h.is_encrypted(),
            });
        }
        Ok(out)
    }

    fn read(&self, inner: &str) -> io::Result<Vec<u8>> {
        let want = normalize_inner(inner);
        let mut arc = unrar::Archive::new(&self.path)
            .open_for_processing()
            .map_err(|e| io::Error::other(e.to_string()))?;
        loop {
            let Some(cursor) = arc
                .read_header()
                .map_err(|e| io::Error::other(e.to_string()))?
            else {
                break;
            };
            let name = normalize_inner(&cursor.entry().filename.to_string_lossy());
            if name == want {
                if cursor.entry().is_directory() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "ディレクトリは読めません",
                    ));
                }
                let (bytes, _next) = cursor
                    .read()
                    .map_err(|e| io::Error::other(e.to_string()))?;
                return Ok(bytes);
            }
            arc = cursor
                .skip()
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "書庫内ファイルが見つかりません",
        ))
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
    /// `by_name` は zip 内部の UTF-8 化名を使い CP932 名と一致しないため、生バイト名（raw）で
    /// index を突き合わせてから読む。`password` 指定時は復号読み（AES/ZipCrypto）。`limit`
    /// 指定時は解凍自体を `take` で打ち切る。
    fn read_entry(
        &self,
        inner: &str,
        limit: Option<usize>,
        password: Option<&[u8]>,
    ) -> io::Result<(Vec<u8>, bool)> {
        use std::io::Read;
        let want = normalize_inner(inner);
        let mut zip = self.archive()?;
        // まず生バイト名で対象 index を特定する（暗号化エントリは by_index_raw なら復号不要）。
        let mut found: Option<usize> = None;
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i).map_err(zip_err)?;
            let name = normalize_inner(&decode_name(f.name_raw()));
            if name == want {
                if f.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "ディレクトリは読めません",
                    ));
                }
                found = Some(i);
                break;
            }
        }
        let Some(i) = found else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "書庫内ファイルが見つかりません",
            ));
        };
        let mut f = match password {
            Some(pw) => zip.by_index_decrypt(i, pw).map_err(zip_err)?,
            None => zip.by_index(i).map_err(zip_err)?,
        };
        match limit {
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
        }
    }
}

impl ArchiveBackend for ZipBackend {
    fn caps(&self) -> Caps {
        Caps {
            random_access: true,
            can_add: true,
            can_mkdir: true,
            can_remove: true,
            can_rename: true,
        }
    }

    fn list(&self) -> io::Result<Vec<ArchiveEntry>> {
        let mut zip = self.archive()?;
        let mut out = Vec::with_capacity(zip.len());
        for i in 0..zip.len() {
            // by_index_raw はメタデータのみ（復号不要）＝暗号化エントリも一覧できる。
            let f = zip.by_index_raw(i).map_err(zip_err)?;
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
        Ok(self.read_entry(inner, None, None)?.0)
    }

    fn read_capped(&self, inner: &str, cap: usize) -> io::Result<(Vec<u8>, bool)> {
        self.read_entry(inner, Some(cap), None)
    }

    fn read_with_password(&self, inner: &str, password: Option<&[u8]>) -> io::Result<Vec<u8>> {
        Ok(self.read_entry(inner, None, password)?.0)
    }
}

fn zip_err(e: zip::result::ZipError) -> io::Error {
    io::Error::other(e.to_string())
}

/// 7z 書庫の読取バックエンド（sevenz-rust2＝純Rust）。`is_solid` を開封時に控え、
/// ソリッドは `random_access: false`（単一ブロックを毎回頭から復号する＝個別取り出しが
/// 高コスト）として GUI 側の「一括展開」経路へ倒す。非ソリッドはブロック＝ファイルなので
/// `random_access: true`（per-file 取り出しが軽い）。書込みは不可。
pub struct SevenZBackend {
    path: PathBuf,
    solid: bool,
}

impl SevenZBackend {
    /// 開けることを確認し、ソリッドか否かを控えて構築する（壊れた書庫はここで弾く）。
    pub fn open(path: &Path) -> io::Result<Self> {
        let reader = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
            .map_err(sevenz_err)?;
        let solid = reader.archive().is_solid;
        Ok(Self {
            path: path.to_path_buf(),
            solid,
        })
    }

    fn reader(&self) -> io::Result<sevenz_rust2::ArchiveReader<std::fs::File>> {
        sevenz_rust2::ArchiveReader::open(&self.path, sevenz_rust2::Password::empty())
            .map_err(sevenz_err)
    }
}

impl ArchiveBackend for SevenZBackend {
    fn caps(&self) -> Caps {
        Caps {
            random_access: !self.solid,
            ..Default::default()
        }
    }

    fn list(&self) -> io::Result<Vec<ArchiveEntry>> {
        let reader = self.reader()?;
        let mut out = Vec::new();
        for f in &reader.archive().files {
            let path = normalize_inner(&f.name);
            if path.is_empty() {
                continue;
            }
            out.push(ArchiveEntry {
                path,
                is_dir: f.is_directory,
                size: Some(f.size),
                packed_size: None,
                mtime: None,
                is_encrypted: false,
            });
        }
        Ok(out)
    }

    fn read(&self, inner: &str) -> io::Result<Vec<u8>> {
        let want = normalize_inner(inner);
        let mut reader = self.reader()?;
        // 正規化名で突き合わせ、書庫が持つ生の格納名を得てから read_file する
        // （格納名は '\\' 区切りや末尾差異があり得るため）。
        let stored = reader
            .archive()
            .files
            .iter()
            .find(|f| !f.is_directory && normalize_inner(&f.name) == want)
            .map(|f| f.name.clone());
        let Some(stored) = stored else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "書庫内ファイルが見つかりません",
            ));
        };
        reader.read_file(&stored).map_err(sevenz_err)
    }

    fn extract_all(
        &self,
        dest: &Path,
        each: &mut dyn FnMut(&str, u64, u64) -> bool,
    ) -> io::Result<()> {
        use std::io::Read;
        let mut reader = self.reader()?;
        let total = reader
            .archive()
            .files
            .iter()
            .filter(|f| !f.is_directory)
            .count() as u64;
        let mut done = 0u64;
        let mut io_err: Option<io::Error> = None;
        // for_each_entries は単一パスでブロックを順次復号する（ソリッドでも一度で全展開）。
        reader
            .for_each_entries(&mut |entry: &sevenz_rust2::ArchiveEntry, rd: &mut dyn Read| {
                let path = normalize_inner(&entry.name);
                if path.is_empty() {
                    return Ok(true);
                }
                let Some(p) = safe_join(dest, &path) else {
                    return Ok(true);
                };
                if entry.is_directory {
                    if let Err(e) = std::fs::create_dir_all(&p) {
                        io_err = Some(e);
                        return Ok(false);
                    }
                    return Ok(true);
                }
                if !each(&path, done, total) {
                    return Ok(false);
                }
                if let Some(parent) = p.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        io_err = Some(e);
                        return Ok(false);
                    }
                }
                let mut buf = Vec::with_capacity(entry.size as usize);
                if let Err(e) = rd.read_to_end(&mut buf) {
                    io_err = Some(e);
                    return Ok(false);
                }
                if let Err(e) = std::fs::write(&p, &buf) {
                    io_err = Some(e);
                    return Ok(false);
                }
                done += 1;
                Ok(true)
            })
            .map_err(sevenz_err)?;
        if let Some(e) = io_err {
            return Err(e);
        }
        Ok(())
    }
}

fn sevenz_err(e: sevenz_rust2::Error) -> io::Error {
    io::Error::other(e.to_string())
}

/// `inner`（'/' 区切り・正規化済み）を `dest` 配下の実パスへ安全に合成する。各セグメントを
/// 検証し、空/"."/".."や '\\' 混入を弾く（zip-slip 対策）。`None` は不正で展開対象外。
fn safe_join(dest: &Path, inner: &str) -> Option<PathBuf> {
    let mut p = dest.to_path_buf();
    for seg in inner.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\') {
            return None;
        }
        p.push(seg);
    }
    Some(p)
}

/// tar 書庫の読取バックエンド（無圧縮 tar ＋ gz/bz2/xz/zstd ラップ）。tar は順次アクセスなので
/// `random_access: false`（個別取り出しは毎回先頭から舐める＝GUI 側で一括展開へ倒す）。書込み不可。
pub struct TarBackend {
    path: PathBuf,
    comp: Comp,
}

impl TarBackend {
    /// 構築のみ（重い展開は list/extract_all 側で。壊れた tar はそこで弾く）。
    pub(crate) fn open(path: &Path, comp: Comp) -> io::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            comp,
        })
    }

    fn archive(&self) -> io::Result<tar::Archive<Box<dyn io::Read>>> {
        Ok(tar::Archive::new(decoded_reader(&self.path, self.comp)?))
    }
}

impl ArchiveBackend for TarBackend {
    fn caps(&self) -> Caps {
        Caps {
            random_access: false,
            ..Default::default()
        }
    }

    fn list(&self) -> io::Result<Vec<ArchiveEntry>> {
        let mut ar = self.archive()?;
        let mut out = Vec::new();
        for entry in ar.entries()? {
            let entry = entry?;
            let is_dir = entry.header().entry_type().is_dir();
            let path = normalize_inner(&entry.path()?.to_string_lossy());
            if path.is_empty() {
                continue;
            }
            let mtime = entry
                .header()
                .mtime()
                .ok()
                .map(|s| UNIX_EPOCH + Duration::from_secs(s));
            out.push(ArchiveEntry {
                path,
                is_dir,
                size: Some(entry.size()),
                packed_size: None,
                mtime,
                is_encrypted: false,
            });
        }
        Ok(out)
    }

    fn read(&self, inner: &str) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let want = normalize_inner(inner);
        let mut ar = self.archive()?;
        for entry in ar.entries()? {
            let mut entry = entry?;
            if entry.header().entry_type().is_dir() {
                continue;
            }
            let path = normalize_inner(&entry.path()?.to_string_lossy());
            if path == want {
                let mut buf = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut buf)?;
                return Ok(buf);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "書庫内ファイルが見つかりません",
        ))
    }

    fn extract_all(
        &self,
        dest: &Path,
        each: &mut dyn FnMut(&str, u64, u64) -> bool,
    ) -> io::Result<()> {
        use std::io::Read;
        // 順次 tar は件数を事前に数えられないので、進捗は「圧縮ファイルの消費バイト数／
        // ファイルサイズ」で見せる（単一パス＝再解凍しない・バーが滑らかに伸びる）。
        let total = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let count = Arc::new(AtomicU64::new(0));
        let counted = CountingReader {
            inner: std::fs::File::open(&self.path)?,
            count: count.clone(),
        };
        let mut ar = tar::Archive::new(wrap_comp(counted, self.comp)?);
        for entry in ar.entries()? {
            let mut entry = entry?;
            let is_dir = entry.header().entry_type().is_dir();
            let path = normalize_inner(&entry.path()?.to_string_lossy());
            if path.is_empty() {
                continue;
            }
            let Some(p) = safe_join(dest, &path) else {
                continue;
            };
            if is_dir {
                std::fs::create_dir_all(&p)?;
                continue;
            }
            if !each(&path, count.load(Ordering::Relaxed), total) {
                return Ok(());
            }
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            std::fs::write(&p, &buf)?;
        }
        Ok(())
    }
}

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

/// 単体圧縮ファイルの内側エントリ名＝圧縮拡張子を除いたファイル名（`foo.json.xz`→`foo.json`）。
fn single_inner_name(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("data");
    let lower = name.to_ascii_lowercase();
    for suf in [".gz", ".bz2", ".xz", ".zstd", ".zst"] {
        if lower.ends_with(suf) {
            let stem = &name[..name.len() - suf.len()];
            return if stem.is_empty() {
                "data".to_owned()
            } else {
                stem.to_owned()
            };
        }
    }
    name.to_owned()
}

/// 書庫内パスを正規化：'\\' を '/' に、空セグメントと "." を除去して '/' 区切りへ。
/// 先頭/連続/末尾スラッシュが畳まれ、空文字＝ルート。".." は読取側では素の
/// セグメントとして残す（実FS への展開時のサニタイズは展開コピー側で別途行う）。
pub(crate) fn normalize_inner(s: &str) -> String {
    s.replace('\\', "/")
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// 生バイト名を文字列へ復号する。valid UTF-8 ならそのまま（UTF-8 フラグ付きの
/// 現代 zip・ASCII）、不正なら CP932(Shift_JIS) とみなす（フラグ無しの旧 zip）。
pub(crate) fn decode_name(raw: &[u8]) -> String {
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
    fn encrypted_entry_reads_with_password() {
        use std::io::Write;
        let path = temp_path("aes");
        {
            let f = std::fs::File::create(&path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .with_aes_encryption(zip::AesMode::Aes256, "secret");
            zw.start_file("secret.txt", opts).unwrap();
            zw.write_all(b"classified").unwrap();
            zw.finish().unwrap();
        }
        let be = ZipBackend::open(&path).unwrap();
        // 暗号化フラグが立つ。
        assert!(be.list().unwrap().iter().any(|e| e.path == "secret.txt" && e.is_encrypted));
        // パスワード無しでは読めない。
        assert!(be.read("secret.txt").is_err());
        // 正しいパスワードで復号できる。
        assert_eq!(
            be.read_with_password("secret.txt", Some(b"secret")).unwrap(),
            b"classified"
        );
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

    /// rar 読取（`rar` feature 時のみ）。同梱フィクスチャ version.rar を一覧・読取する。
    #[cfg(feature = "rar")]
    #[test]
    fn rar_list_and_read() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/version.rar");
        let be = RarBackend::open(&p).unwrap();
        assert!(!be.caps().random_access);
        let list = be.list().unwrap();
        assert!(list.iter().any(|e| e.path == "VERSION" && !e.is_dir));
        assert_eq!(be.read("VERSION").unwrap(), b"unrar-0.4.0");
        assert!(be.read("nope").is_err());
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// ソリッド／非ソリッドで `caps().random_access` が反転すること。
    #[test]
    fn sevenz_solid_flips_random_access() {
        assert!(!SevenZBackend::open(&fixture("solid.7z"))
            .unwrap()
            .caps()
            .random_access);
        assert!(SevenZBackend::open(&fixture("nonsolid.7z"))
            .unwrap()
            .caps()
            .random_access);
    }

    /// 7z の一覧・個別読取（'\\' 区切りの格納名を '/' へ正規化して扱う）。
    #[test]
    fn sevenz_list_and_read() {
        for name in ["solid.7z", "nonsolid.7z"] {
            let be = SevenZBackend::open(&fixture(name)).unwrap();
            let list = be.list().unwrap();
            assert!(
                list.iter().any(|e| e.path == "a.txt" && !e.is_dir && e.size == Some(3)),
                "{name}: a.txt"
            );
            assert!(list.iter().any(|e| e.path == "sub" && e.is_dir), "{name}: sub dir");
            assert!(list.iter().any(|e| e.path == "sub/c.txt"), "{name}: sub/c.txt");
            assert_eq!(be.read("a.txt").unwrap(), b"AAA", "{name}");
            assert_eq!(be.read("sub/c.txt").unwrap(), b"CCC", "{name}");
            assert_eq!(be.read("sub/d.txt").unwrap(), b"DDD", "{name}");
            assert!(be.read("missing").is_err(), "{name}");
        }
    }

    /// `extract_all` がツリーを実FSへ展開し、各ファイルでコールバックが進捗を刻むこと。
    #[test]
    fn sevenz_extract_all_writes_tree() {
        let be = SevenZBackend::open(&fixture("solid.7z")).unwrap();
        let dest = std::env::temp_dir()
            .join(format!("rerics_7z_extract_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        let mut seen = 0u64;
        be.extract_all(&dest, &mut |_name, done, total| {
            assert_eq!(total, 3);
            seen = done + 1;
            true
        })
        .unwrap();
        assert_eq!(seen, 3);
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"AAA");
        assert_eq!(std::fs::read(dest.join("sub").join("c.txt")).unwrap(), b"CCC");
        assert_eq!(std::fs::read(dest.join("sub").join("d.txt")).unwrap(), b"DDD");
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// `extract_all` のコールバックが `false` を返すと途中で止まること。
    #[test]
    fn sevenz_extract_all_cancels() {
        let be = SevenZBackend::open(&fixture("nonsolid.7z")).unwrap();
        let dest = std::env::temp_dir()
            .join(format!("rerics_7z_cancel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        be.extract_all(&dest, &mut |_name, _done, _total| false).unwrap();
        // 1件も展開していない（最初の each で false）。
        assert!(!dest.join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// tar 本体＋各圧縮ラップ（gz/bz2/xz/zstd）の一覧・読取・一括展開。すべて非RA。
    #[test]
    fn tar_family_list_read_extract() {
        for name in ["tree.tar", "tree.tar.gz", "tree.tar.bz2", "tree.tar.xz", "tree.tar.zst"] {
            let be = open_archive(&fixture(name)).unwrap();
            assert!(!be.caps().random_access, "{name}: tar は非RA");
            let list = be.list().unwrap();
            assert!(
                list.iter().any(|e| e.path == "a.txt" && !e.is_dir && e.size == Some(3)),
                "{name}: a.txt"
            );
            assert!(list.iter().any(|e| e.path == "sub/c.txt"), "{name}: sub/c.txt");
            assert_eq!(be.read("a.txt").unwrap(), b"AAA", "{name}");
            assert_eq!(be.read("sub/d.txt").unwrap(), b"DDD", "{name}");
            let dest = std::env::temp_dir().join(format!(
                "rerics_tar_{}_{}",
                std::process::id(),
                name.replace('.', "_")
            ));
            let _ = std::fs::remove_dir_all(&dest);
            std::fs::create_dir_all(&dest).unwrap();
            let mut called = false;
            be.extract_all(&dest, &mut |_p, _done, total| {
                // tar の進捗は「消費バイト数／圧縮ファイルサイズ」（total>0）。
                assert!(total > 0, "{name}: total はファイルサイズ");
                called = true;
                true
            })
            .unwrap();
            assert!(called, "{name}: コールバックが呼ばれる");
            assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"AAA", "{name}");
            assert_eq!(std::fs::read(dest.join("sub").join("c.txt")).unwrap(), b"CCC", "{name}");
            let _ = std::fs::remove_dir_all(&dest);
        }
    }

    /// 単体圧縮（gz/xz）＝1エントリ（圧縮拡張子を除いた名前）・ランダムアクセス可・読む時に解凍。
    #[test]
    fn single_file_compressed_one_entry() {
        for name in ["note.txt.xz", "note.txt.gz", "note.txt.zst"] {
            let be = open_archive(&fixture(name)).unwrap();
            assert!(be.caps().random_access, "{name}: 単体は RA");
            let list = be.list().unwrap();
            assert_eq!(list.len(), 1, "{name}");
            assert_eq!(list[0].path, "note.txt", "{name}");
            assert!(!list[0].is_dir);
            // gz/xz/zstd は展開後サイズ（"hello world"=11）をメタ/ヘッダから取れる。
            assert_eq!(list[0].size, Some(11), "{name}: 展開後サイズ");
            assert_eq!(be.read("note.txt").unwrap(), b"hello world", "{name}");
            assert!(be.read("nope").is_err(), "{name}");
            let (head, trunc) = be.read_capped("note.txt", 5).unwrap();
            assert_eq!(head, b"hello", "{name}");
            assert!(trunc, "{name}");
        }
    }

    /// zip への append：**既存の CP932 名エントリを壊さず**新規ファイル/ディレクトリを足せる。
    #[test]
    fn zip_append_preserves_cp932_names() {
        let mut cp932 = vec![0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]; // 日本語
        cp932.extend_from_slice(b".txt");
        let path = temp_path("append_cp932");
        build_stored_zip_raw(&path, &[(&cp932, b"orig")]);

        // 追加（add）と mkdir を append で実行。
        let mut w = open_archive_writer(&path).unwrap();
        w.add("added.txt", b"NEW").unwrap();
        w.mkdir("newdir").unwrap();
        // 未対応操作はエラー。
        assert!(w.remove("added.txt").is_err());
        assert!(w.rename("added.txt", "x").is_err());

        let be = ZipBackend::open(&path).unwrap();
        let list = be.list().unwrap();
        // 既存の CP932 名が壊れていない（正しくデコードできる）。
        assert!(
            list.iter().any(|e| e.path == "日本語.txt"),
            "CP932 名が保持される: {:?}",
            list.iter().map(|e| &e.path).collect::<Vec<_>>()
        );
        // 既存データも無傷、新規も読める。
        assert_eq!(be.read("日本語.txt").unwrap(), b"orig");
        assert_eq!(be.read("added.txt").unwrap(), b"NEW");
        assert!(list.iter().any(|e| e.path == "newdir" && e.is_dir));
        let _ = std::fs::remove_file(&path);
    }

    /// 書込み未対応形式（7z 等）は `open_archive_writer` がエラー。
    #[test]
    fn writer_unsupported_for_non_zip() {
        assert!(open_archive_writer(&fixture("solid.7z")).is_err());
        assert!(open_archive_writer(&fixture("tree.tar")).is_err());
    }

    /// 拡張子分類（二重拡張子・短縮形・単体・非書庫）。
    #[test]
    fn classify_known_extensions() {
        for p in ["x.tar", "x.tar.gz", "x.tgz", "x.tar.zstd", "x.json.xz", "a.zip", "a.7z"] {
            assert!(is_known_archive(Path::new(p)), "{p} は書庫のはず");
        }
        for p in ["a.txt", "a.png", "noext"] {
            assert!(!is_known_archive(Path::new(p)), "{p} は非書庫のはず");
        }
    }
}
