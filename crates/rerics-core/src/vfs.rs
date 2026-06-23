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

    /// 表示文字列（実FS パス or "C:\foo.zip\inner"）から Location を復元する。
    ///
    /// 実在するディレクトリならそのまま `Real`。そうでなければパスを末尾から縮め、
    /// 途中に「実在する書庫ファイル」が現れたらそこを境界に `Archive` へ分割する。
    /// どちらにも当たらなければ `Real`（存在検証/フォールバックは呼び側に委ねる）。
    pub fn parse(display: &str) -> Location {
        let p = Path::new(display);
        if p.is_dir() {
            return Location::Real(absolutize(p));
        }
        let mut inner_parts: Vec<String> = Vec::new();
        let mut cur = p.to_path_buf();
        loop {
            if is_archive_path(&cur) {
                inner_parts.reverse();
                return Location::Archive {
                    archive: absolutize(&cur),
                    inner: inner_parts.join("/"),
                };
            }
            let Some(name) = cur.file_name().map(|s| s.to_string_lossy().into_owned()) else {
                break;
            };
            let Some(parent) = cur.parent().map(|x| x.to_path_buf()) else {
                break;
            };
            if parent == cur {
                break;
            }
            inner_parts.push(name);
            cur = parent;
        }
        Location::Real(absolutize(p))
    }

    /// 同じドライブのルート（`C:\`）への Location。実FS のときのみ返す。
    /// 書庫内は対象外（None）。既にルートならルートを返す。
    pub fn to_root(&self) -> Option<Location> {
        use std::path::Component;
        let Location::Real(p) = self else {
            return None;
        };
        let mut comps = p.components();
        match comps.next() {
            // "C:" のようなドライブ等のプレフィックス＋ルート区切りでドライブルート。
            Some(Component::Prefix(pre)) => {
                let mut s = pre.as_os_str().to_os_string();
                s.push(std::path::MAIN_SEPARATOR_STR);
                Some(Location::Real(PathBuf::from(s)))
            }
            // プレフィックスの無い絶対パス（"\foo"）はそのままルート区切りへ。
            Some(Component::RootDir) => {
                Some(Location::Real(PathBuf::from(std::path::MAIN_SEPARATOR_STR)))
            }
            _ => None,
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

/// 相対パスを絶対パス化する（失敗時は元のまま）。`Pane::open` と同じ正規化を
/// `Location::parse` でも保ち、`.` のような相対表記で親移動できなくなるのを防ぐ。
fn absolutize(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
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
    p.is_file() && crate::archive::is_known_archive(p)
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
    fn parse_relative_is_absolutized() {
        // "." を相対のまま Real にすると file_name 無しで to_parent が詰まる回帰を防ぐ。
        let loc = Location::parse(".");
        let p = loc.as_real_path().expect("real");
        assert!(p.is_absolute());
        assert!(loc.to_parent().is_some());
    }

    #[test]
    fn to_root_returns_drive_root() {
        let r = Location::Real(PathBuf::from("C:\\foo\\bar\\baz"));
        assert_eq!(r.to_root().and_then(|l| l.as_real_path().map(Path::to_path_buf)),
            Some(PathBuf::from("C:\\")));
        // 既にルートでも安全にルートを返す。
        let already = Location::Real(PathBuf::from("C:\\"));
        assert_eq!(already.to_root().and_then(|l| l.as_real_path().map(Path::to_path_buf)),
            Some(PathBuf::from("C:\\")));
        // 書庫内は対象外。
        let arc = Location::Archive { archive: PathBuf::from("C:\\a.zip"), inner: "x".into() };
        assert!(arc.to_root().is_none());
    }

    #[test]
    fn to_root_unc_returns_share_root() {
        // UNC（ネットワーク共有）内の深いパスは、共有ルート \\server\share へ。
        let unc = Location::Real(PathBuf::from("\\\\server\\share\\dir\\sub"));
        let root = unc.to_root().and_then(|l| l.as_real_path().map(Path::to_path_buf));
        assert_eq!(root, Some(PathBuf::from("\\\\server\\share\\")));
        // 既に共有ルートでも安全に共有ルートを返す。
        let already = Location::Real(PathBuf::from("\\\\server\\share"));
        let root2 = already.to_root().and_then(|l| l.as_real_path().map(Path::to_path_buf));
        assert_eq!(root2, Some(PathBuf::from("\\\\server\\share\\")));
    }

    #[test]
    fn real_helpers() {
        let r = Location::Real(PathBuf::from("C:\\x\\y"));
        assert_eq!(r.as_real_path(), Some(Path::new("C:\\x\\y")));
        assert!(!r.is_archive());
        assert!(matches!(r.loc_join("z"), Location::Real(p) if p == *"C:\\x\\y\\z"));
    }
}
