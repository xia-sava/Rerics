use std::io;
use std::path::{Path, PathBuf};
use super::*;

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
