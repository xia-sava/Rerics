//! ファイル検索（原作 FilerScriptFindFile の名前・日付・サイズ条件のみ）。
//!
//! 起点ディレクトリ配下を再帰的に走査し、ファイル名マスク・更新日時範囲・サイズ範囲で
//! 絞り込んだ項目を、検索結果一覧へ流す合成 `FileItem`（`source`＝出自ディレクトリ・
//! `info`＝出自の相対サブパス）として返す。GUI 非依存の純ロジック。
//! 内容検索（キーワード/GREP・エンコーディング判定）は対象外（別途）。

use std::time::SystemTime;

use crate::FileItem;
use crate::Sink;
use crate::file_list::glob_match;
use crate::vfs::Location;

/// 検索条件。`includes`/`excludes` は大小無視のグロブ（`*`/`?`）パターン群。
#[derive(Debug, Clone, Default)]
pub struct FindOptions {
    /// いずれかに一致すれば名前条件を満たす（空なら名前で絞らない）。
    pub includes: Vec<String>,
    /// いずれかに一致したら除外する（`!` 接頭辞のマスク）。
    pub excludes: Vec<String>,
    /// 更新日時の下限（含む）。
    pub from_date: Option<SystemTime>,
    /// 更新日時の上限（含む）。
    pub to_date: Option<SystemTime>,
    /// サイズ下限（バイト・含む）。
    pub min_size: Option<u64>,
    /// サイズ上限（バイト・含む）。
    pub max_size: Option<u64>,
}

impl FindOptions {
    /// 「カンマまたは空白区切り・`!` で除外」のマスク文字列を includes/excludes に振り分ける。
    /// 原作 frmFindFile のファイル名欄と同じ分割（空要素は捨てる）。
    pub fn set_masks(&mut self, text: &str) {
        for tok in text.split([',', ' ', '\t']) {
            let tok = tok.trim();
            if tok.is_empty() {
                continue;
            }
            if let Some(rest) = tok.strip_prefix('!') {
                if !rest.is_empty() {
                    self.excludes.push(rest.to_owned());
                }
            } else {
                self.includes.push(tok.to_owned());
            }
        }
    }

    /// 名前・日付・サイズのいずれかで実際に絞り込むか（全条件が空なら検索する意味がない）。
    pub fn is_empty(&self) -> bool {
        self.includes.is_empty()
            && self.excludes.is_empty()
            && self.from_date.is_none()
            && self.to_date.is_none()
            && self.min_size.is_none()
            && self.max_size.is_none()
    }
}

/// `root` 配下を再帰検索し、条件に合う項目（`source`/`info` 付き）を `sink` へ1件ずつ
/// 流す。見つかった件数を返す。`sink` が中止を告げたら走査を打ち切る。
pub fn find_file(root: &Location, opts: &FindOptions, sink: &mut Sink) -> usize {
    let base = root.loc_display();
    let mut count = 0;
    walk(root, opts, &base, sink, &mut count);
    count
}

/// 1ディレクトリを走査し、一致項目を流しつつサブディレクトリへ降りる。
fn walk(dir: &Location, opts: &FindOptions, base: &str, sink: &mut Sink, count: &mut usize) {
    let Ok(mut items) = dir.read() else {
        return;
    };
    items.retain(|it| !it.is_parent);
    items.sort_by_key(|it| it.name.to_uppercase());
    let rel = relative_from(base, &dir.loc_display());
    for it in &items {
        if sink.is_cancelled() {
            return;
        }
        sink.tick();
        if matches(it, opts) {
            *count += 1;
            let mut item = it.clone();
            item.info = Some(rel.clone());
            item.source = Some(dir.clone());
            sink.push(item);
        }
        if it.is_dir {
            walk(&dir.loc_join(&it.name), opts, base, sink, count);
        }
    }
}

/// 名前・日付・サイズの全条件に合うか。
fn matches(it: &FileItem, opts: &FindOptions) -> bool {
    if !opts.includes.is_empty() && !glob_match(&it.name, &opts.includes.join(",")) {
        return false;
    }
    if !opts.excludes.is_empty() && glob_match(&it.name, &opts.excludes.join(",")) {
        return false;
    }
    if opts.from_date.is_some() || opts.to_date.is_some() {
        let Some(m) = it.modified else {
            return false;
        };
        if opts.from_date.is_some_and(|f| m < f) || opts.to_date.is_some_and(|t| m > t) {
            return false;
        }
    }
    if opts.min_size.is_some() || opts.max_size.is_some() {
        let Some(sz) = it.size else {
            return false;
        };
        if opts.min_size.is_some_and(|mn| sz < mn) || opts.max_size.is_some_and(|mx| sz > mx) {
            return false;
        }
    }
    true
}

/// `dir` から `base` プレフィックスを外した相対サブパス（先頭区切りは落とす）。
/// `dir == base`（起点直下）なら空文字。
fn relative_from(base: &str, dir: &str) -> String {
    match dir.strip_prefix(base) {
        Some(rest) => rest.trim_start_matches(['\\', '/']).to_owned(),
        None => dir.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("rerics_findtest_{}_{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
        fn loc(&self) -> Location {
            Location::Real(self.path.clone())
        }
        fn write(&self, rel: &str, body: &str) {
            let p = self.path.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, body).unwrap();
        }
        fn set_mtime(&self, rel: &str, secs: u64) {
            let p = self.path.join(rel);
            let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
            f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn names(items: &[FileItem]) -> Vec<String> {
        items.iter().map(|it| it.name.clone()).collect()
    }

    /// 走査を最後まで回し、流れてきた項目と件数を集める（中止しない）。
    fn collect(root: &Location, opts: &FindOptions) -> (Vec<FileItem>, usize) {
        let mut items = Vec::new();
        let count = find_file(root, opts, &mut Sink {
            emit: &mut |it| items.push(it),
            cancelled: &|| false,
            progress: &mut || {},
        });
        (items, count)
    }

    #[test]
    fn mask_matches_recursively_with_relative_info() {
        let t = TempDir::new();
        t.write("a.txt", "1");
        t.write("b.log", "2");
        t.write("sub/c.txt", "3");
        t.write("sub/deep/d.txt", "4");

        let mut opts = FindOptions::default();
        opts.set_masks("*.txt");
        let (items, count) = collect(&t.loc(), &opts);
        let got = names(&items);
        assert!(got.contains(&"a.txt".to_owned()));
        assert!(got.contains(&"c.txt".to_owned()));
        assert!(got.contains(&"d.txt".to_owned()));
        assert!(!got.contains(&"b.log".to_owned()), "log は除外: {got:?}");
        assert_eq!(count, 3);
        // 出自の相対サブパスが info に出る。
        let c = items.iter().find(|it| it.name == "c.txt").unwrap();
        assert_eq!(c.info.as_deref(), Some("sub"));
        let d = items.iter().find(|it| it.name == "d.txt").unwrap();
        assert_eq!(d.info.as_deref().unwrap().replace('\\', "/"), "sub/deep");
        // 起点直下は info 空。
        let a = items.iter().find(|it| it.name == "a.txt").unwrap();
        assert_eq!(a.info.as_deref(), Some(""));
    }

    #[test]
    fn cancel_stops_walk_early() {
        let t = TempDir::new();
        t.write("a.txt", "1");
        t.write("b.txt", "2");
        t.write("c.txt", "3");
        let mut opts = FindOptions::default();
        opts.set_masks("*.txt");
        // 最初の1件を流した直後に中止を告げる。
        let seen = std::cell::Cell::new(0usize);
        let mut items = Vec::new();
        let count = find_file(&t.loc(), &opts, &mut Sink {
            emit: &mut |it| {
                items.push(it);
                seen.set(seen.get() + 1);
            },
            cancelled: &|| seen.get() >= 1,
            progress: &mut || {},
        });
        assert_eq!(items.len(), 1, "最初の1件で打ち切る: {:?}", names(&items));
        assert_eq!(count, 1);
    }

    #[test]
    fn exclude_mask_filters_out() {
        let t = TempDir::new();
        t.write("keep.txt", "1");
        t.write("skip.txt", "2");
        let mut opts = FindOptions::default();
        opts.set_masks("*.txt !skip*");
        let (items, _) = collect(&t.loc(), &opts);
        let got = names(&items);
        assert!(got.contains(&"keep.txt".to_owned()), "{got:?}");
        assert!(!got.contains(&"skip.txt".to_owned()), "{got:?}");
    }

    #[test]
    fn date_range_filters() {
        let t = TempDir::new();
        t.write("old.txt", "1");
        t.write("new.txt", "2");
        t.set_mtime("old.txt", 1_000);
        t.set_mtime("new.txt", 10_000);
        let opts = FindOptions {
            from_date: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(5_000)),
            ..Default::default()
        };
        let (items, _) = collect(&t.loc(), &opts);
        assert_eq!(names(&items), vec!["new.txt".to_owned()]);
    }

    #[test]
    fn size_range_filters_and_excludes_dirs() {
        let t = TempDir::new();
        t.write("small.txt", "x"); // 1
        t.write("big.txt", "xxxxxxxx"); // 8
        t.write("sub/child.txt", "y"); // サブdir は走査されるが dir 自体はサイズ条件で出ない
        let opts = FindOptions {
            min_size: Some(4),
            ..Default::default()
        };
        let (items, _) = collect(&t.loc(), &opts);
        let got = names(&items);
        assert!(got.contains(&"big.txt".to_owned()), "{got:?}");
        assert!(!got.contains(&"small.txt".to_owned()), "{got:?}");
        assert!(!got.contains(&"sub".to_owned()), "dir はサイズ条件で出ない: {got:?}");
    }
}
