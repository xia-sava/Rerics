use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use super::*;

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
                let mut buf = Vec::with_capacity((f.size() as usize).min(super::PREALLOC_CAP));
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

    /// 各エントリを丸ごとメモリへ取らず、復号ストリームから直接ファイルへ流す。zip は
    /// ランダムアクセスなので既定実装でも O(n) だが、巨大エントリでメモリを食わないよう
    /// override する。
    fn extract_all(
        &self,
        dest: &Path,
        each: &mut dyn FnMut(&str, u64, u64) -> bool,
    ) -> io::Result<()> {
        let total = self.list()?.iter().filter(|e| !e.is_dir).count() as u64;
        let mut zip = self.archive()?;
        let mut done = 0u64;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(zip_err)?;
            let is_dir = f.is_dir() || f.name_raw().last() == Some(&b'/');
            let path = normalize_inner(&decode_name(f.name_raw()));
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
            if !each(&path, done, total) {
                return Ok(());
            }
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&p)?;
            std::io::copy(&mut f, &mut out)?;
            done += 1;
        }
        Ok(())
    }
}

fn zip_err(e: zip::result::ZipError) -> io::Error {
    io::Error::other(e.to_string())
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
