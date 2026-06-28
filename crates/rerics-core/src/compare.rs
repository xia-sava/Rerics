//! ディレクトリ比較（原作 FilerScriptDirectoryCompare 相当）。
//!
//! 2つの場所（`Location`）の直下を、ファイル名（大小無視）で突き合わせて差分を求める。
//! 結果は検索・比較の結果一覧へ流す合成 `FileItem`（各々 `source`＝出自ディレクトリ・
//! `info`＝説明＋相対サブパス）として返す。GUI に依存しない純ロジックで、ワーカースレッドが
//! これを呼んで結果を結果ペインへ渡す。

use std::cmp::Ordering;

use crate::FileItem;
use crate::Sink;
use crate::vfs::Location;

/// 日付・サイズそれぞれに適用する比較条件（原作 DirectoryCompareOption 相当）。
/// `Less`/`Greater` は「src を dst と比べて」の向き：日付なら新旧、サイズなら大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompareCondition {
    /// 比較しない（この軸では絞り込まない）。
    #[default]
    None,
    /// 一致する項目を出す。
    Equals,
    /// 一致しない項目を出す。
    NotEquals,
    /// src の方が小さい（日付＝古い／サイズ＝小）。原作 Less。
    Less,
    /// src の方が大きい（日付＝新しい／サイズ＝大）。原作 Greater。
    Greater,
}

/// ディレクトリ比較のオプション一式。
#[derive(Debug, Clone, Copy, Default)]
pub struct CompareOptions {
    /// 更新日時での絞り込み。
    pub date: CompareCondition,
    /// サイズでの絞り込み。
    pub size: CompareCondition,
    /// サブディレクトリを再帰的に比較する（原作 Dir）。
    pub recurse: bool,
    /// src 側にのみ在る項目を「追加」として出す（原作 Exist）。
    pub show_added: bool,
    /// dst 側にのみ在る項目を「削除」として出す（原作 NotExist）。
    pub show_deleted: bool,
}

impl CompareOptions {
    /// 「差分を見る」既定プリセット：日付かサイズが違う一致ファイル＋片側だけの項目を出す
    /// （再帰はしない）。引数なしの `directoryCompare` コマンドで使う。
    pub fn differences() -> Self {
        Self {
            date: CompareCondition::NotEquals,
            size: CompareCondition::NotEquals,
            recurse: false,
            show_added: true,
            show_deleted: true,
        }
    }
}

/// 比較結果の集計（原作 Equals/NotEquals/Adds/Deletes）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompareCounts {
    /// 日付・サイズとも一致したファイル数。
    pub equals: usize,
    /// 両側に在るが日付かサイズが違うファイル数。
    pub not_equals: usize,
    /// src 側にのみ在った項目数。
    pub adds: usize,
    /// dst 側にのみ在った項目数。
    pub deletes: usize,
}

/// `src` と `dst` を比較し、結果項目（`source`/`info` 付き）を `sink` へ1件ずつ流す。
/// 集計を返す。`info` の相対サブパスは `src` の表示パスを基準に算出する。`sink` が
/// 中止を告げたら走査を打ち切る（それまでの集計を返す）。
pub fn directory_compare(
    src: &Location,
    dst: &Location,
    opts: &CompareOptions,
    sink: &mut Sink,
) -> CompareCounts {
    let base = src.loc_display();
    let mut counts = CompareCounts::default();
    compare_dir(Some(src), Some(dst), opts, &base, sink, &mut counts);
    counts
}

/// 片側ディレクトリ同士の突き合わせ（再帰）。`src`/`dst` の一方が `None` のときは、
/// 在る側を全て「追加」または「削除」として出す（原作の片側再帰）。
fn compare_dir(
    src: Option<&Location>,
    dst: Option<&Location>,
    opts: &CompareOptions,
    base: &str,
    sink: &mut Sink,
    counts: &mut CompareCounts,
) {
    let a = read_sorted(src);
    let b = read_sorted(dst);
    let (mut i, mut j) = (0usize, 0usize);
    loop {
        if sink.is_cancelled() {
            return;
        }
        match (a.get(i), b.get(j)) {
            (None, None) => break,
            (Some(fa), Some(fb)) => {
                let na = fa.name.to_uppercase();
                let nb = fb.name.to_uppercase();
                match na.cmp(&nb) {
                    Ordering::Equal if fa.is_dir == fb.is_dir => {
                        if fa.is_dir {
                            if opts.recurse {
                                let cs = src.map(|l| l.loc_join(&fa.name));
                                let cd = dst.map(|l| l.loc_join(&fb.name));
                                compare_dir(cs.as_ref(), cd.as_ref(), opts, base, sink, counts);
                            }
                        } else {
                            compare_files(fa, fb, opts, src, base, sink, counts);
                        }
                        i += 1;
                        j += 1;
                    }
                    Ordering::Equal => {
                        // 同名だがディレクトリ属性が食い違う。
                        add_item(sink, fa, "ディレクトリ属性不一致", src, base, false);
                        counts.not_equals += 1;
                        if opts.recurse && fa.is_dir {
                            let cs = src.map(|l| l.loc_join(&fa.name));
                            compare_dir(cs.as_ref(), None, opts, base, sink, counts);
                        }
                        if opts.recurse && fb.is_dir {
                            let cd = dst.map(|l| l.loc_join(&fb.name));
                            compare_dir(None, cd.as_ref(), opts, base, sink, counts);
                        }
                        i += 1;
                        j += 1;
                    }
                    Ordering::Less => {
                        added(sink, fa, opts, src, base, counts);
                        if opts.recurse && fa.is_dir {
                            let cs = src.map(|l| l.loc_join(&fa.name));
                            compare_dir(cs.as_ref(), None, opts, base, sink, counts);
                        }
                        i += 1;
                    }
                    Ordering::Greater => {
                        deleted(sink, fb, opts, dst, base, counts);
                        if opts.recurse && fb.is_dir {
                            let cd = dst.map(|l| l.loc_join(&fb.name));
                            compare_dir(None, cd.as_ref(), opts, base, sink, counts);
                        }
                        j += 1;
                    }
                }
            }
            (Some(fa), None) => {
                added(sink, fa, opts, src, base, counts);
                if opts.recurse && fa.is_dir {
                    let cs = src.map(|l| l.loc_join(&fa.name));
                    compare_dir(cs.as_ref(), None, opts, base, sink, counts);
                }
                i += 1;
            }
            (None, Some(fb)) => {
                deleted(sink, fb, opts, dst, base, counts);
                if opts.recurse && fb.is_dir {
                    let cd = dst.map(|l| l.loc_join(&fb.name));
                    compare_dir(None, cd.as_ref(), opts, base, sink, counts);
                }
                j += 1;
            }
        }
    }
}

/// 在る側を読み、親（".."）を除いて名前（大小無視）→種別で安定ソートする。読めなければ空。
fn read_sorted(loc: Option<&Location>) -> Vec<FileItem> {
    let mut v: Vec<FileItem> = loc.and_then(|l| l.read().ok()).unwrap_or_default();
    v.retain(|it| !it.is_parent);
    v.sort_by(|x, y| x.name.to_uppercase().cmp(&y.name.to_uppercase()).then(x.is_dir.cmp(&y.is_dir)));
    v
}

/// src 側にのみ在る項目を「追加」として記録する。
fn added(
    sink: &mut Sink,
    item: &FileItem,
    opts: &CompareOptions,
    src: Option<&Location>,
    base: &str,
    counts: &mut CompareCounts,
) {
    if opts.show_added {
        add_item(sink, item, "追加", src, base, false);
    }
    counts.adds += 1;
}

/// dst 側にのみ在る項目を「削除」として記録する（情報列は dst の絶対ディレクトリを出す）。
fn deleted(
    sink: &mut Sink,
    item: &FileItem,
    opts: &CompareOptions,
    dst: Option<&Location>,
    base: &str,
    counts: &mut CompareCounts,
) {
    if opts.show_deleted {
        add_item(sink, item, "削除", dst, base, true);
    }
    counts.deletes += 1;
}

/// 両側に在るファイルの日付・サイズを比べ、条件に合えば結果へ加える。
fn compare_files(
    fa: &FileItem,
    fb: &FileItem,
    opts: &CompareOptions,
    src: Option<&Location>,
    base: &str,
    sink: &mut Sink,
    counts: &mut CompareCounts,
) {
    // どちらの軸も比較しない設定なら、両側に在る一致候補は無条件で出す。
    let mut show = opts.date == CompareCondition::None && opts.size == CompareCondition::None;
    let mut date_desc = "";
    let mut size_desc = "";

    let date_cmp = fa.modified.cmp(&fb.modified);
    match opts.date {
        CompareCondition::None => {}
        CompareCondition::Equals => {
            if date_cmp == Ordering::Equal {
                show = true;
                date_desc = "日付一致";
            }
        }
        CompareCondition::NotEquals => {
            if date_cmp != Ordering::Equal {
                show = true;
                date_desc = if date_cmp == Ordering::Less { "古い" } else { "新しい" };
            }
        }
        CompareCondition::Less => {
            if date_cmp == Ordering::Greater {
                show = true;
                date_desc = "新しい";
            }
        }
        CompareCondition::Greater => {
            if date_cmp == Ordering::Less {
                show = true;
                date_desc = "古い";
            }
        }
    }

    let (sa, sb) = (fa.size.unwrap_or(0), fb.size.unwrap_or(0));
    match opts.size {
        CompareCondition::None => {}
        CompareCondition::Equals => {
            if sa == sb {
                show = true;
                size_desc = "サイズ一致";
            }
        }
        CompareCondition::NotEquals => {
            if sa != sb {
                show = true;
                size_desc = if sa >= sb { "大きい" } else { "小さい" };
            }
        }
        CompareCondition::Less => {
            if sa < sb {
                show = true;
                size_desc = "小さい";
            }
        }
        CompareCondition::Greater => {
            if sa > sb {
                show = true;
                size_desc = "大きい";
            }
        }
    }

    if date_cmp == Ordering::Equal && sa == sb {
        counts.equals += 1;
    } else {
        counts.not_equals += 1;
    }

    if show {
        let desc = match (date_desc.is_empty(), size_desc.is_empty()) {
            (false, false) => format!("{date_desc},{size_desc}"),
            (false, true) => date_desc.to_owned(),
            (true, false) => size_desc.to_owned(),
            (true, true) => String::new(),
        };
        add_item(sink, fa, &desc, src, base, false);
    }
}

/// 結果項目を1件加える。`source` には出自ディレクトリの `Location` を、`info` には
/// 説明（"追加" 等）と出自サブパスを合わせて入れる。`fullpath` のときは相対化せず
/// 出自ディレクトリの絶対表示を入れる（削除項目＝dst 側に使う）。
fn add_item(
    sink: &mut Sink,
    item: &FileItem,
    desc: &str,
    loc: Option<&Location>,
    base: &str,
    fullpath: bool,
) {
    let dir_display = loc.map(|l| l.loc_display()).unwrap_or_default();
    let info = if fullpath {
        join_desc(desc, &dir_display)
    } else {
        let rel = relative_from(base, &dir_display);
        if rel.is_empty() {
            desc.to_owned()
        } else {
            join_desc(desc, &rel)
        }
    };
    let mut it = item.clone();
    it.info = Some(info);
    it.source = loc.cloned();
    sink.push(it);
}

/// 説明と相対/絶対パスを ":" で連結する（説明が空ならパスのみ・パスが空なら説明のみ）。
fn join_desc(desc: &str, path: &str) -> String {
    match (desc.is_empty(), path.is_empty()) {
        (false, false) => format!("{desc}:{path}"),
        (false, true) => desc.to_owned(),
        (true, false) => path.to_owned(),
        (true, true) => String::new(),
    }
}

/// `dir` から `base` プレフィックスを外した相対サブパス（先頭区切りは落とす）。
/// `dir == base`（最上位）なら空文字。
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
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::{Duration, SystemTime};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!("rerics_cmptest_{}_{n}", std::process::id()));
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
        fn set_mtime(&self, rel: &str, secs_from_epoch: u64) {
            let p = self.path.join(rel);
            let t = SystemTime::UNIX_EPOCH + Duration::from_secs(secs_from_epoch);
            filetime_set(&p, t);
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn filetime_set(p: &std::path::Path, t: SystemTime) {
        // std だけでは mtime を設定できないため、open + set_modified を使う（Rust 1.75+）。
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    fn src_display(it: &FileItem) -> String {
        it.source.as_ref().map(|l| l.loc_display()).unwrap_or_default()
    }

    fn names_info(items: &[FileItem]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|it| (it.name.clone(), it.info.clone().unwrap_or_default()))
            .collect()
    }

    /// 比較を最後まで回し、流れてきた項目と集計を集める（中止しない）。
    fn collect(src: &Location, dst: &Location, opts: &CompareOptions) -> (Vec<FileItem>, CompareCounts) {
        let mut items = Vec::new();
        let counts = directory_compare(src, dst, opts, &mut Sink {
            emit: &mut |it| items.push(it),
            cancelled: &|| false,
        });
        (items, counts)
    }

    #[test]
    fn added_and_deleted_only_when_flagged() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write("only_src.txt", "a");
        dst.write("only_dst.txt", "b");

        // フラグ無し：差分は出ないがカウントはされる。
        let opts = CompareOptions::default();
        let (items, counts) = collect(&src.loc(), &dst.loc(), &opts);
        assert_eq!(items.len(), 0);
        assert_eq!(counts.adds, 1);
        assert_eq!(counts.deletes, 1);

        // フラグ有り：追加・削除が出る。
        let opts = CompareOptions { show_added: true, show_deleted: true, ..Default::default() };
        let (items, counts) = collect(&src.loc(), &dst.loc(), &opts);
        let got = names_info(&items);
        assert!(got.contains(&("only_src.txt".to_owned(), "追加".to_owned())), "{got:?}");
        // 削除項目の情報列は dst の絶対ディレクトリを含む。
        let del = items.iter().find(|it| it.name == "only_dst.txt").unwrap();
        let info = del.info.clone().unwrap();
        assert!(info.starts_with("削除:"), "{info}");
        assert!(info.contains(&dst.path.display().to_string()), "{info}");
        assert_eq!(counts.adds, 1);
        assert_eq!(counts.deletes, 1);
        // 出自は各自のディレクトリ。
        let add = items.iter().find(|it| it.name == "only_src.txt").unwrap();
        assert_eq!(src_display(add), src.path.display().to_string());
        assert_eq!(src_display(del), dst.path.display().to_string());
    }

    #[test]
    fn matched_files_count_equals_vs_not_equals() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write("same.txt", "hello");
        dst.write("same.txt", "hello");
        src.set_mtime("same.txt", 1_000);
        dst.set_mtime("same.txt", 1_000);
        src.write("diff.txt", "aaaa");
        dst.write("diff.txt", "bb"); // サイズ違い

        let opts = CompareOptions::default();
        let (_items, counts) = collect(&src.loc(), &dst.loc(), &opts);
        assert_eq!(counts.equals, 1, "same.txt は一致");
        assert_eq!(counts.not_equals, 1, "diff.txt はサイズ違い");
    }

    #[test]
    fn size_not_equals_filters_and_describes() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write("bigger.txt", "aaaa"); // 4
        dst.write("bigger.txt", "b"); // 1
        src.write("smaller.txt", "c"); // 1
        dst.write("smaller.txt", "dddd"); // 4
        src.write("equal.txt", "xx");
        dst.write("equal.txt", "yy");

        let opts = CompareOptions { size: CompareCondition::NotEquals, ..Default::default() };
        let (items, _counts) = collect(&src.loc(), &dst.loc(), &opts);
        let got = names_info(&items);
        // equal.txt は出ない。
        assert!(!got.iter().any(|(n, _)| n == "equal.txt"), "{got:?}");
        assert!(got.contains(&("bigger.txt".to_owned(), "大きい".to_owned())), "{got:?}");
        assert!(got.contains(&("smaller.txt".to_owned(), "小さい".to_owned())), "{got:?}");
    }

    #[test]
    fn date_newer_with_less_option() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write("f.txt", "x");
        dst.write("f.txt", "x");
        src.set_mtime("f.txt", 2_000); // src の方が新しい
        dst.set_mtime("f.txt", 1_000);

        // Less＝src>dst（src が新しい）で出す。
        let opts = CompareOptions { date: CompareCondition::Less, ..Default::default() };
        let (items, _counts) = collect(&src.loc(), &dst.loc(), &opts);
        let got = names_info(&items);
        assert_eq!(got, vec![("f.txt".to_owned(), "新しい".to_owned())]);
    }

    #[test]
    fn recurse_descends_and_relativizes_info() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write("sub/only.txt", "a"); // dst には sub/only.txt が無い
        // dst には sub ディレクトリ自体を作る（再帰対象にするため）。
        std::fs::create_dir_all(dst.path.join("sub")).unwrap();

        let opts = CompareOptions { recurse: true, show_added: true, ..Default::default() };
        let (items, counts) = collect(&src.loc(), &dst.loc(), &opts);
        let only = items.iter().find(|it| it.name == "only.txt").unwrap();
        let info = only.info.clone().unwrap();
        // 情報列は "追加:sub"（base からの相対サブパス）。
        assert_eq!(info, "追加:sub", "{info}");
        // 出自はサブディレクトリ。
        assert_eq!(src_display(only), src.loc().loc_join("sub").loc_display());
        assert_eq!(counts.adds, 1);
    }
}
