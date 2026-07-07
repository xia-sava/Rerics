use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use crate::LogLevel;
use crate::archive::{decode_name, normalize_inner};
use crate::messages;
use super::*;

/// 書庫再構築の一時ファイル名を一意にするための連番。同一書庫を同時に書き換えても
/// 一時ファイルを取り合って壊し合わない（rename の後勝ちに収束する）。
static REWRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// `inner_prefix`（'/' 区切り・正規化済み・"" はルート）の下に `name` を繋いだ書庫内パス。
fn join_inner(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// 実FS の項目群を既存 zip 書庫へ追記する（append）。`inner_prefix` は書庫内の追加先
/// ディレクトリ。`new_append` で開き新規エントリだけ足す＝**既存エントリの生バイト名を
/// 一切触らないので CP932 名も無傷**。同名があっても重複エントリとして後ろに足す（多くの
/// 展開ツールは後勝ちで読む）。中断時も finish して既存書庫を壊さない。
pub fn run_archive_add(
    host: &dyn OperationHost,
    src_dir: &Path,
    names: &[String],
    dst_zip: &Path,
    inner_prefix: &str,
) -> OpSummary {
    run_operation(host, "追加", ResultStyle::Copy, || {
        let mut sum = OpSummary::default();
        let file = match std::fs::OpenOptions::new().read(true).write(true).open(dst_zip) {
            Ok(f) => f,
            Err(e) => {
                host.log(LogLevel::Error, &messages::archive_add_failure(&file_name(dst_zip), &e.to_string()));
                sum.err += 1;
                return sum;
            }
        };
        let mut zw = match zip::ZipWriter::new_append(file) {
            Ok(z) => z,
            Err(e) => {
                host.log(LogLevel::Error, &messages::archive_add_failure(&file_name(dst_zip), &e.to_string()));
                sum.err += 1;
                return sum;
            }
        };
        for name in names {
            if should_stop(host) {
                sum.cancelled = true;
                break;
            }
            let src = src_dir.join(name);
            let rel = join_inner(inner_prefix, name);
            if let Flow::Cancel = add_archive_item(host, &mut zw, &src, &rel, &mut sum) {
                sum.cancelled = true;
                break;
            }
        }
        if let Err(e) = zw.finish() {
            host.log(LogLevel::Error, &messages::archive_add_failure(&file_name(dst_zip), &e.to_string()));
            sum.err += 1;
        }
        sum
    })
}

/// 既存 zip を読み直し、各エントリに `decide` を適用して新しい一時 zip に書き戻し、元へ
/// rename で差し替える（途中失敗・中止で元書庫を壊さない）。`decide(正規化名, is_dir)` は
/// `None`=そのエントリを捨てる、`Some(out)`=その正規化名（ディレクトリは内部で末尾 '/' を
/// 付ける）で書き戻す。書き戻し後に `extra=(src_dir, names, inner_prefix)` があれば実FS の
/// 項目を足す。既存名は decode して UTF-8 で書くので **CP932 名は UTF-8 へ近代化**される。
/// 結果サマリ（err/cancelled・追加分の ok）を返す。最終的な結果ログ行は呼び出し側が出す。
fn rewrite_archive(
    host: &dyn OperationHost,
    dst_zip: &Path,
    decide: impl Fn(&str, bool) -> Option<String>,
    extra: Option<(&Path, &[String], &str)>,
) -> OpSummary {
    let mut sum = OpSummary::default();
    let zip_name = file_name(dst_zip);
    let logfail =
        |e: String| host.log(LogLevel::Error, &messages::archive_update_failure(&zip_name, &e));

    let src_file = match std::fs::File::open(dst_zip) {
        Ok(f) => f,
        Err(e) => {
            logfail(e.to_string());
            sum.err += 1;
            return sum;
        }
    };
    let mut src_zip = match zip::ZipArchive::new(src_file) {
        Ok(z) => z,
        Err(e) => {
            logfail(e.to_string());
            sum.err += 1;
            return sum;
        }
    };

    let seq = REWRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut tmp_path = dst_zip.to_path_buf();
    tmp_path.set_file_name(format!("{}.rerics-tmp-{}-{}", zip_name, std::process::id(), seq));
    let tmp_file = match std::fs::File::create(&tmp_path) {
        Ok(f) => f,
        Err(e) => {
            logfail(e.to_string());
            sum.err += 1;
            return sum;
        }
    };
    let mut zw = zip::ZipWriter::new(tmp_file);

    // 既存エントリを decide に従って書き戻す。大書庫では時間がかかるので進捗を出す。
    let total = src_zip.len();
    let handle = host.begin_progress(LogLevel::Normal, &messages::archive_rebuild());
    let mut tracker = ProgressTracker::new();
    for i in 0..total {
        if should_stop(host) {
            sum.cancelled = true;
            break;
        }
        let file = match src_zip.by_index(i) {
            Ok(f) => f,
            Err(e) => {
                logfail(e.to_string());
                sum.err += 1;
                continue;
            }
        };
        let name = normalize_inner(&decode_name(file.name_raw()));
        if name.is_empty() {
            continue;
        }
        let is_dir = file.is_dir();
        match decide(&name, is_dir) {
            None => drop(file),
            Some(out) => {
                let out_name = if is_dir { format!("{out}/") } else { out };
                if let Err(e) = zw.raw_copy_file_rename(file, out_name) {
                    logfail(e.to_string());
                    sum.err += 1;
                }
            }
        }
        if let Some(pct) = tracker.tick((i + 1) as u64, total as u64) {
            host.update_progress(handle, &messages::archive_rebuild_progress(pct));
        }
    }

    // 追加項目を足す（rebuild=追加/置換のときだけ）。
    if !sum.cancelled
        && let Some((src_dir, names, inner_prefix)) = extra {
            for name in names {
                if should_stop(host) {
                    sum.cancelled = true;
                    break;
                }
                let src = src_dir.join(name);
                let rel = join_inner(inner_prefix, name);
                if let Flow::Cancel = add_archive_item(host, &mut zw, &src, &rel, &mut sum) {
                    sum.cancelled = true;
                    break;
                }
            }
        }

    // 進捗行から % を落として確定する（成否に依らず）。
    host.end_progress(handle, &messages::archive_rebuild());

    let finished = zw.finish();
    if sum.cancelled || sum.err > 0 {
        drop(finished);
        let _ = std::fs::remove_file(&tmp_path);
        return sum;
    }
    if let Err(e) = finished {
        let _ = std::fs::remove_file(&tmp_path);
        logfail(e.to_string());
        sum.err += 1;
        return sum;
    }
    if let Err(e) = std::fs::rename(&tmp_path, dst_zip) {
        let _ = std::fs::remove_file(&tmp_path);
        logfail(e.to_string());
        sum.err += 1;
    }
    sum
}

/// 既存 zip を再構築し、追加項目と同名のエントリを除いてから追加項目を足す（同名を確実に
/// 置換・重複を残さない）。CP932 名は UTF-8 へ近代化される。
pub fn run_archive_rebuild(
    host: &dyn OperationHost,
    src_dir: &Path,
    names: &[String],
    dst_zip: &Path,
    inner_prefix: &str,
) -> OpSummary {
    // 追加項目の書庫内パス。これに一致 or その配下の既存エントリは捨てて置換する。
    let new_tops: Vec<String> = names.iter().map(|n| join_inner(inner_prefix, n)).collect();
    run_operation(host, "追加", ResultStyle::Copy, || {
        rewrite_archive(
            host,
            dst_zip,
            |name, _is_dir| {
                let replaced = new_tops
                    .iter()
                    .any(|t| name == t.as_str() || name.starts_with(&format!("{t}/")));
                if replaced { None } else { Some(name.to_string()) }
            },
            Some((src_dir, names, inner_prefix)),
        )
    })
}

/// 書庫内の `names`（`inner` 直下のエントリ名）を削除する。全体を再構築して該当エントリ
/// （ディレクトリはその配下も）を除く。CP932 名は UTF-8 へ近代化される。一時ファイルへ
/// 書いてから差し替えるので、途中失敗・中止で元書庫を壊さない。
pub fn run_archive_delete(
    host: &dyn OperationHost,
    archive: &Path,
    inner: &str,
    names: &[String],
) -> OpSummary {
    let targets: Vec<String> = names.iter().map(|n| join_inner(inner, n)).collect();
    run_operation(host, "削除", ResultStyle::Delete, || {
        let mut sum = rewrite_archive(
            host,
            archive,
            |name, _is_dir| {
                let removed = targets
                    .iter()
                    .any(|t| name == t.as_str() || name.starts_with(&format!("{t}/")));
                if removed { None } else { Some(name.to_string()) }
            },
            None,
        );
        if !sum.cancelled && sum.err == 0 {
            for name in names {
                host.log(LogLevel::Normal, &messages::delete(name));
            }
            sum.ok = names.len();
        }
        sum
    })
}

/// 書庫内の `old`（`inner` 直下）を `new` へ改名する。ディレクトリはその配下のパスも
/// まとめて付け替える。全体を再構築し、CP932 名は UTF-8 へ近代化される。`new` が既存と
/// 衝突するかの判定は呼び出し側（GUI）で行う前提。
pub fn run_archive_rename(
    host: &dyn OperationHost,
    archive: &Path,
    inner: &str,
    old: &str,
    new: &str,
) -> OpSummary {
    let from = join_inner(inner, old);
    let to = join_inner(inner, new);
    let from_pfx = format!("{from}/");
    run_operation(host, "改名", ResultStyle::Delete, || {
        let mut sum = rewrite_archive(
            host,
            archive,
            |name, _is_dir| {
                if name == from.as_str() {
                    Some(to.clone())
                } else if let Some(rest) = name.strip_prefix(&from_pfx) {
                    Some(format!("{to}/{rest}"))
                } else {
                    Some(name.to_string())
                }
            },
            None,
        );
        if !sum.cancelled && sum.err == 0 {
            host.log(LogLevel::Normal, &messages::rename(old, new));
            sum.ok = 1;
        }
        sum
    })
}

/// 1項目を zip へ追記する（ディレクトリは再帰）。`rel` は書庫内パス（'/' 区切り）。
/// [`add_to_zip`] と同じ処理だがログ文言を「追加」にする。
fn add_archive_item(
    host: &dyn OperationHost,
    zw: &mut zip::ZipWriter<std::fs::File>,
    src: &Path,
    rel: &str,
    sum: &mut OpSummary,
) -> Flow {
    use std::io::{Read, Write};
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let name = file_name(src);
    if src.is_dir() {
        if let Err(e) = zw.add_directory(format!("{rel}/"), opts) {
            host.log(LogLevel::Error, &messages::archive_add_failure(&name, &e.to_string()));
            sum.err += 1;
            return Flow::Continue;
        }
        let entries = match std::fs::read_dir(src) {
            Ok(e) => e,
            Err(e) => {
                host.log(LogLevel::Error, &messages::archive_add_failure(&name, &e.to_string()));
                sum.err += 1;
                return Flow::Continue;
            }
        };
        for entry in entries {
            if should_stop(host) {
                return Flow::Cancel;
            }
            let Ok(entry) = entry else { continue };
            let child_name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = format!("{rel}/{child_name}");
            if let Flow::Cancel = add_archive_item(host, zw, &entry.path(), &child_rel, sum) {
                return Flow::Cancel;
            }
        }
        Flow::Continue
    } else {
        let mut reader = match std::fs::File::open(src) {
            Ok(f) => f,
            Err(e) => {
                host.log(LogLevel::Error, &messages::archive_add_failure(&name, &e.to_string()));
                sum.err += 1;
                return Flow::Continue;
            }
        };
        let total = reader.metadata().map(|m| m.len()).unwrap_or(0);
        let handle = host.begin_progress(LogLevel::Normal, &messages::archive_add(&name));
        if let Err(e) = zw.start_file(rel.to_string(), opts) {
            host.end_progress(handle, &messages::archive_add(&name));
            host.log(LogLevel::Error, &messages::archive_add_failure(&name, &e.to_string()));
            sum.err += 1;
            return Flow::Continue;
        }
        // チャンクで書き、ファイル内のバイト進捗を更新する（大ファイルでも行が固まらない）。
        // 中止はファイル境界で見る（zip エントリの途中で止めると壊れるので現在のファイルは
        // 書き切る）。
        let mut buf = vec![0u8; 256 * 1024];
        let mut written = 0u64;
        let mut tracker = ProgressTracker::new();
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    host.end_progress(handle, &messages::archive_add(&name));
                    host.log(LogLevel::Error, &messages::archive_add_failure(&name, &e.to_string()));
                    sum.err += 1;
                    return Flow::Continue;
                }
            };
            if let Err(e) = zw.write_all(&buf[..n]) {
                host.end_progress(handle, &messages::archive_add(&name));
                host.log(LogLevel::Error, &messages::archive_add_failure(&name, &e.to_string()));
                sum.err += 1;
                return Flow::Continue;
            }
            written += n as u64;
            if let Some(pct) = tracker.tick(written, total) {
                host.update_progress(handle, &messages::archive_add_progress(&name, pct));
            }
        }
        host.end_progress(handle, &messages::archive_add(&name));
        sum.ok += 1;
        Flow::Continue
    }
}
