//! 仮想ロケーション層（実FS と 書庫内 を1つの型で扱う・UI 非依存）。
//!
//! `Location` が「いまどこを見ているか」を表す。実FS は従来どおり、書庫内は
//! `Archive{ archive, inner }` で書庫ファイルの実パスと内部相対パスを保持する。
//! `read`/`enter`/`to_parent` の分岐をここへ集約し、`Pane` から移譲する。表示や
//! 実FS との結合は `loc_display`/`as_real_path`/`loc_join` ヘルパで吸収する。

use std::path::{Path, PathBuf};

use crate::FileItem;
use crate::archive::{entries_at, open_archive};

/// ペインの現在地。実FS か 書庫内か。
#[derive(Clone, Debug)]
pub enum Location {
    /// 実ファイルシステム上のディレクトリ。
    Real(PathBuf),
    /// 書庫内。`archive`=書庫ファイルの実パス、`inner`=書庫内相対（'/' 区切り・"" = ルート）。
    Archive { archive: PathBuf, inner: String },
}

impl Location {
    /// 実FS のときのみ実パスを返す（書庫内は None）。実FS 操作の可否判定に使う。
    pub fn as_real_path(&self) -> Option<&Path> {
        match self {
            Location::Real(p) => Some(p),
            Location::Archive { .. } => None,
        }
    }

    /// 書庫内かどうか。
    pub fn is_archive(&self) -> bool {
        matches!(self, Location::Archive { .. })
    }

    /// パスバー/タブ用の表示文字列。書庫内は "C:\foo.zip\inner" 形式（OS 区切り）。
    pub fn loc_display(&self) -> String {
        match self {
            Location::Real(p) => p.display().to_string(),
            Location::Archive { archive, inner } => {
                if inner.is_empty() {
                    archive.display().to_string()
                } else {
                    let inner_os = inner.replace('/', std::path::MAIN_SEPARATOR_STR);
                    format!(
                        "{}{}{}",
                        archive.display(),
                        std::path::MAIN_SEPARATOR,
                        inner_os
                    )
                }
            }
        }
    }

    /// 子 `name` へ降りた Location（dir 名を足す）。実FS は join、書庫は inner に追記。
    /// 侵入可否は判定しない（純粋にパスを合成するだけ）。
    pub fn loc_join(&self, name: &str) -> Location {
        match self {
            Location::Real(p) => Location::Real(p.join(name)),
            Location::Archive { archive, inner } => Location::Archive {
                archive: archive.clone(),
                inner: join_inner(inner, name),
            },
        }
    }

    /// 現在地直下の一覧（先頭に "..", 実FS は `read_items` を流用）。
    pub fn read(&self) -> std::io::Result<Vec<FileItem>> {
        match self {
            Location::Real(p) => crate::read_items(p),
            Location::Archive { archive, inner } => {
                let backend = open_archive(archive)?;
                let all = backend.list()?;
                let mut items = vec![FileItem::parent()];
                items.append(&mut entries_at(&all, inner));
                Ok(items)
            }
        }
    }

    /// `name` へ侵入した Location を返す。`is_dir` は呼び側が持つエントリ種別。
    ///
    /// 実FS では dir なら実 dir へ、ファイルかつ書庫拡張子なら書庫ルートへ。
    /// 書庫内では dir のみ inner を伸ばす（ファイルは呼び側がビューア/展開に回す）。
    pub fn enter(&self, name: &str, is_dir: bool) -> Option<Location> {
        match self {
            Location::Real(p) => {
                let target = p.join(name);
                if is_dir && target.is_dir() {
                    Some(Location::Real(target))
                } else if !is_dir && is_archive_path(&target) {
                    Some(Location::Archive {
                        archive: target,
                        inner: String::new(),
                    })
                } else {
                    None
                }
            }
            Location::Archive { archive, inner } => {
                if is_dir {
                    Some(Location::Archive {
                        archive: archive.clone(),
                        inner: join_inner(inner, name),
                    })
                } else {
                    None
                }
            }
        }
    }

    /// 親へ。`Some((親 Location, 出てきた名前))`。書庫ルートから出ると実FS の親へ。
    pub fn to_parent(&self) -> Option<(Location, String)> {
        match self {
            Location::Real(p) => {
                let parent = p.parent()?.to_path_buf();
                let prev = p.file_name()?.to_string_lossy().into_owned();
                Some((Location::Real(parent), prev))
            }
            Location::Archive { archive, inner } => {
                if inner.is_empty() {
                    // 書庫を出る → 書庫ファイルのある実 dir へ。カーソルは書庫ファイル名へ。
                    let parent = archive.parent()?.to_path_buf();
                    let prev = archive.file_name()?.to_string_lossy().into_owned();
                    Some((Location::Real(parent), prev))
                } else {
                    let (head, prev) = match inner.rsplit_once('/') {
                        Some((h, t)) => (h.to_string(), t.to_string()),
                        None => (String::new(), inner.clone()),
                    };
                    Some((
                        Location::Archive {
                            archive: archive.clone(),
                            inner: head,
                        },
                        prev,
                    ))
                }
            }
        }
    }
}

/// inner に子名を連結（"" のときは name そのもの）。
fn join_inner(inner: &str, name: &str) -> String {
    if inner.is_empty() {
        name.to_string()
    } else {
        format!("{inner}/{name}")
    }
}

/// パスが「潜れる書庫ファイル」か（実在するファイル＋既知の書庫拡張子）。
pub fn is_archive_path(p: &Path) -> bool {
    p.is_file()
        && matches!(
            p.extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .as_deref(),
            Some("zip")
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_and_display_archive() {
        let a = Location::Archive {
            archive: PathBuf::from("C:\\x\\foo.zip"),
            inner: String::new(),
        };
        assert!(a.is_archive());
        assert!(a.as_real_path().is_none());

        let b = a.loc_join("sub");
        assert!(matches!(&b, Location::Archive { inner, .. } if inner == "sub"));
        let c = b.loc_join("deep");
        assert!(matches!(&c, Location::Archive { inner, .. } if inner == "sub/deep"));

        // 表示は書庫ファイル名を含み、ルートは書庫パスそのもの
        assert!(c.loc_display().contains("foo.zip"));
        assert_eq!(a.loc_display(), "C:\\x\\foo.zip");
        // inner 付きの完全形は OS セパレータで連結される
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(c.loc_display(), format!("C:\\x\\foo.zip{sep}sub{sep}deep"));
    }

    #[test]
    fn real_helpers() {
        let r = Location::Real(PathBuf::from("C:\\x\\y"));
        assert_eq!(r.as_real_path(), Some(Path::new("C:\\x\\y")));
        assert!(!r.is_archive());
        assert!(matches!(r.loc_join("z"), Location::Real(p) if p == PathBuf::from("C:\\x\\y\\z")));
    }
}
