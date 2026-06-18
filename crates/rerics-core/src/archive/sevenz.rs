use std::io;
use std::path::{Path, PathBuf};
use super::*;

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
