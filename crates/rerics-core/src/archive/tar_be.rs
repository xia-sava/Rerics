use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};
use super::*;

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
