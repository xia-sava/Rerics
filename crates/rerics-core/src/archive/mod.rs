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
use std::time::SystemTime;

use crate::FileItem;

mod zip_be;
mod sevenz;
mod tar_be;
mod rar;
mod single_file;
use self::zip_be::{ZipBackend, ZipWriter};
use self::sevenz::SevenZBackend;
use self::tar_be::TarBackend;
use self::rar::RarBackend;
use self::single_file::SingleFileBackend;

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
            if e.is_dir
                && let Some(p) = safe_join(dest, &e.path) {
                    std::fs::create_dir_all(p)?;
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
    Rar,
    /// tar 本体＋ラップ圧縮（None=無圧縮）。
    Tar(Comp),
    /// 単体圧縮ファイル（1エントリ）。
    Single(Comp),
}

/// ファイル名（小文字化）から書庫種別を決める。未知は `None`。
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
        return Some(ArchiveKind::Rar);
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
        Some(ArchiveKind::Rar) => Ok(Box::new(RarBackend::open(path)?)),
        Some(ArchiveKind::Tar(comp)) => Ok(Box::new(TarBackend::open(path, comp)?)),
        Some(ArchiveKind::Single(comp)) => Ok(Box::new(SingleFileBackend::open(path, comp)?)),
        _ => Err(io::Error::new(io::ErrorKind::Unsupported, "未対応の書庫形式")),
    }
}

/// `backend` の全エントリを `dest` 配下へ展開し、展開できたファイル数を返す。`dest` は
/// 無ければ作る。エントリ名は [`ArchiveBackend::extract_all`] の `safe_join` で zip-slip を
/// 弾く。UI も確認も伴わない programmatic な一括展開（スクリプトの `unpack` の実体）。
pub fn extract_all_to(backend: &dyn ArchiveBackend, dest: &Path) -> io::Result<u64> {
    extract_all_to_progress(backend, dest, &mut |_, _, _| {})
}

/// 書庫の全エントリを `dest` 配下へ展開し、展開した件数を返す。各エントリの取り出しごとに
/// `on_entry(name, done, total)` を呼ぶ（`name`＝書庫内パス・`done`/`total`＝backend が数えられた
/// 場合の進捗で、順次 tar 等の事前に総数を数えられない backend では `total` は 0）。進捗が要らない
/// なら [`extract_all_to`]。
pub fn extract_all_to_progress(
    backend: &dyn ArchiveBackend,
    dest: &Path,
    on_entry: &mut dyn FnMut(&str, u64, u64),
) -> io::Result<u64> {
    std::fs::create_dir_all(dest)?;
    let mut count = 0u64;
    backend.extract_all(dest, &mut |inner, done, total| {
        count += 1;
        on_entry(inner, done, total);
        true
    })?;
    Ok(count)
}

/// 任意の Read を圧縮種別に応じた解凍ストリームへラップする（`Comp::None` は素通し）。
fn wrap_comp<R: io::Read + 'static>(r: R, comp: Comp) -> io::Result<Box<dyn io::Read>> {
    Ok(match comp {
        Comp::None => Box::new(r),
        Comp::Gz => Box::new(flate2::read::GzDecoder::new(r)),
        Comp::Bz2 => Box::new(bzip2::read::BzDecoder::new(r)),
        Comp::Xz => Box::new(lzma_rust2::XzReader::new(r, true)),
        Comp::Zstd => Box::new(zstd::Decoder::new(r)?),
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
// link ブロックを置いて advapi32 を明示リンクする。
#[cfg(windows)]
#[link(name = "advapi32")]
unsafe extern "C" {}

/// `inner`（'/' 区切り・正規化済み）を `dest` 配下の実パスへ安全に合成する。各セグメントを
/// 検証し、空/"."/".."や '\\'・':' 混入を弾く（zip-slip 対策）。':' 拒否で "C:evil" の
/// ようなドライブ相対プレフィクスが `push` で base を捨てて外へ逃げるのを防ぐ。`None` は
/// 不正で展開対象外。
fn safe_join(dest: &Path, inner: &str) -> Option<PathBuf> {
    let mut p = dest.to_path_buf();
    for seg in inner.split('/') {
        if !is_safe_segment(seg) {
            return None;
        }
        p.push(seg);
    }
    Some(p)
}

/// 展開先の1階層分として安全なパスセグメントか。空/"."/".."、区切り文字('/'・'\\')、
/// ドライブ相対や代替データストリームになりうる ':' を弾く。
pub(crate) fn is_safe_segment(seg: &str) -> bool {
    !(seg.is_empty()
        || seg == "."
        || seg == ".."
        || seg.contains('/')
        || seg.contains('\\')
        || seg.contains(':'))
}

/// エントリ読取時の先行確保バイト数の上限。エントリが自称するサイズは信用できない
/// （細工書庫が巨大値を宣言して OOM abort を誘発しうる）ため、`Vec::with_capacity` は
/// この値でクランプする。実データがこれを超える分は読み進めながら伸長する。
pub(crate) const PREALLOC_CAP: usize = 16 * 1024 * 1024;

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
mod tests;
