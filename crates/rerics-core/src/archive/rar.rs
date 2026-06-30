use std::io;
use std::path::{Path, PathBuf};
use super::*;

// UnRAR source code may be used in any software to handle RAR archives without
// limitations free of charge, but cannot be used to develop RAR (WinRAR) compatible
// archiver and to re-create RAR compression algorithm, which is proprietary.
// Distribution of modified UnRAR source code in separate form or as a part of other
// software is permitted, provided that full text of this paragraph, starting from
// "UnRAR source code" words, is included in license, or in documentation if license
// is not available, and in source code comments of resulting package.

/// rar 書庫の読取バックエンド（unrar crate＝vendored C++）。RAR はヘッダ順次アクセスなので
/// `random_access: false`（単体取り出しも先頭から走査する）。書込みは不可。UnRAR は非free
/// ライセンス（展開のみ許可）。
pub struct RarBackend {
    path: PathBuf,
}

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

    /// 書庫を1回だけ頭から走査し、各エントリをその場で書き出す（ソリッドでも復号は一度）。
    /// 既定の `list`＋`read` ループはエントリごとに先頭から復号し直すため O(n²) になる。
    fn extract_all(
        &self,
        dest: &Path,
        each: &mut dyn FnMut(&str, u64, u64) -> bool,
    ) -> io::Result<()> {
        // 総数は listing パス（復号しない）で数える＝進捗表示用。
        let total = self.list()?.iter().filter(|e| !e.is_dir).count() as u64;
        let mut arc = unrar::Archive::new(&self.path)
            .open_for_processing()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut done = 0u64;
        loop {
            let Some(cursor) = arc
                .read_header()
                .map_err(|e| io::Error::other(e.to_string()))?
            else {
                break;
            };
            let name = normalize_inner(&cursor.entry().filename.to_string_lossy());
            if cursor.entry().is_directory() {
                if let Some(p) = safe_join(dest, &name) {
                    std::fs::create_dir_all(&p)?;
                }
                arc = cursor.skip().map_err(|e| io::Error::other(e.to_string()))?;
                continue;
            }
            // 空名・zip-slip 不正は読まずに次へ進める。
            let safe = (!name.is_empty()).then(|| safe_join(dest, &name)).flatten();
            let Some(p) = safe else {
                arc = cursor.skip().map_err(|e| io::Error::other(e.to_string()))?;
                continue;
            };
            if !each(&name, done, total) {
                return Ok(());
            }
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // ネイティブ（UnRAR）に復号からディスク書き込みまで一度で行わせる。メモリへの
            // 往復・確保が無く、書庫内の更新日時も復元される。
            arc = cursor
                .extract_to(&p)
                .map_err(|e| io::Error::other(e.to_string()))?;
            done += 1;
        }
        Ok(())
    }
}
