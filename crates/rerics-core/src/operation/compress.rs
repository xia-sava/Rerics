use std::path::Path;
use crate::LogLevel;
use crate::messages;
use super::*;

/// 実FSの選択項目から新しい zip を作る（圧縮作成）。`names` は `src_dir` 直下の
/// ファイル/ディレクトリ名。ディレクトリは再帰格納する。進捗（ファイル単位）・中止・
/// サマリは run_copy と同じ host 経由。`dst_zip` は上書き作成する（存在確認は呼び側）。
/// 中止された場合は作りかけの zip を消す。
pub fn run_compress(
    host: &dyn OperationHost,
    src_dir: &Path,
    names: &[String],
    dst_zip: &Path,
) -> OpSummary {
    run_operation(host, "圧縮", ResultStyle::Copy, || {
        let mut sum = OpSummary::default();
        let file = match std::fs::File::create(dst_zip) {
            Ok(f) => f,
            Err(e) => {
                host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst_zip), &e.to_string()));
                sum.err += 1;
                return sum;
            }
        };
        let mut zw = zip::ZipWriter::new(file);
        for name in names {
            if should_stop(host) {
                sum.cancelled = true;
                break;
            }
            let src = src_dir.join(name);
            if let Flow::Cancel = add_to_zip(host, &mut zw, &src, name, &mut sum) {
                sum.cancelled = true;
                break;
            }
        }
        let finished = zw.finish();
        if sum.cancelled {
            drop(finished);
            let _ = std::fs::remove_file(dst_zip);
        } else if let Err(e) = finished {
            host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst_zip), &e.to_string()));
            sum.err += 1;
        }
        sum
    })
}

/// 実FSの選択項目から新しい 7z を作る（LZMA2）。`names` は `src_dir` 直下のファイル/
/// ディレクトリ名。ディレクトリは再帰格納する。進捗（ファイル単位）・中止・サマリは
/// [`run_compress`] と同じ host 経由。`dst` は上書き作成する（存在確認は呼び側）。中止
/// された場合は作りかけを消す。
pub fn run_compress_7z(
    host: &dyn OperationHost,
    src_dir: &Path,
    names: &[String],
    dst: &Path,
) -> OpSummary {
    run_operation(host, "圧縮", ResultStyle::Copy, || {
        let mut sum = OpSummary::default();
        let mut sz = match sevenz_rust2::ArchiveWriter::create(dst) {
            Ok(w) => w,
            Err(e) => {
                host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst), &e.to_string()));
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
            if let Flow::Cancel = add_to_7z(host, &mut sz, &src, name, &mut sum) {
                sum.cancelled = true;
                break;
            }
        }
        let finished = sz.finish();
        if sum.cancelled {
            drop(finished);
            let _ = std::fs::remove_file(dst);
        } else if let Err(e) = finished {
            host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst), &e.to_string()));
            sum.err += 1;
        }
        sum
    })
}

/// 1項目を 7z へ追加する。ディレクトリは再帰（空ディレクトリもエントリとして残す）。
/// `rel` は書庫内の相対パス（'/' 区切り）。
fn add_to_7z(
    host: &dyn OperationHost,
    sz: &mut sevenz_rust2::ArchiveWriter<std::fs::File>,
    src: &Path,
    rel: &str,
    sum: &mut OpSummary,
) -> Flow {
    let name = file_name(src);
    if src.is_dir() {
        if let Err(e) = sz
            .push_archive_entry::<std::fs::File>(sevenz_rust2::ArchiveEntry::new_directory(rel), None)
        {
            host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
            sum.err += 1;
            return Flow::Continue;
        }
        let entries = match std::fs::read_dir(src) {
            Ok(e) => e,
            Err(e) => {
                host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
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
            if let Flow::Cancel = add_to_7z(host, sz, &entry.path(), &child_rel, sum) {
                return Flow::Cancel;
            }
        }
        Flow::Continue
    } else {
        let file = match std::fs::File::open(src) {
            Ok(f) => f,
            Err(e) => {
                host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
                sum.err += 1;
                return Flow::Continue;
            }
        };
        host.log(LogLevel::Normal, &messages::compress(&name));
        let entry = sevenz_rust2::ArchiveEntry::from_path(src, rel.to_string());
        if let Err(e) = sz.push_archive_entry(entry, Some(file)) {
            host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
            sum.err += 1;
        } else {
            sum.ok += 1;
        }
        Flow::Continue
    }
}

/// 実FSの単一ファイルを xz 単体圧縮する（tar なし・`<name>` 1ファイルの中身をそのまま流す）。
/// `name` は `src_dir` 直下のファイル。ディレクトリや複数対象は呼び側で tar.xz へ振り分ける
/// 前提。大ファイルでも固まらないようチャンクで流し、ファイル内のバイト境界で中止できる。
/// 中止された場合は作りかけを消す。
pub fn run_compress_xz_single(
    host: &dyn OperationHost,
    src_dir: &Path,
    name: &str,
    dst: &Path,
) -> OpSummary {
    use std::io::{Read, Write};
    run_operation(host, "圧縮", ResultStyle::Copy, || {
        let mut sum = OpSummary::default();
        let src = src_dir.join(name);
        let mut reader = match std::fs::File::open(&src) {
            Ok(f) => f,
            Err(e) => return xz_fail(host, name, &e.to_string(), &mut sum),
        };
        let out = match std::fs::File::create(dst) {
            Ok(f) => f,
            Err(e) => return xz_fail(host, &file_name(dst), &e.to_string(), &mut sum),
        };
        let mut xzw = match lzma_rust2::XzWriter::new(out, lzma_rust2::XzOptions::with_preset(6)) {
            Ok(w) => w,
            Err(e) => {
                let _ = std::fs::remove_file(dst);
                return xz_fail(host, &file_name(dst), &e.to_string(), &mut sum);
            }
        };
        host.log(LogLevel::Normal, &messages::compress(name));
        let mut buf = vec![0u8; 256 * 1024];
        let mut failed = false;
        loop {
            if should_stop(host) {
                sum.cancelled = true;
                break;
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    host.log(LogLevel::Error, &messages::compress_failure(name, &e.to_string()));
                    sum.err += 1;
                    failed = true;
                    break;
                }
            };
            if let Err(e) = xzw.write_all(&buf[..n]) {
                host.log(LogLevel::Error, &messages::compress_failure(name, &e.to_string()));
                sum.err += 1;
                failed = true;
                break;
            }
        }
        let finished = xzw.finish();
        if sum.cancelled || failed {
            drop(finished);
            let _ = std::fs::remove_file(dst);
        } else if let Err(e) = finished {
            host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst), &e.to_string()));
            sum.err += 1;
        } else {
            sum.ok += 1;
        }
        sum
    })
}

/// xz 単体圧縮の初期化失敗をログして返すヘルパ。
fn xz_fail(host: &dyn OperationHost, name: &str, reason: &str, sum: &mut OpSummary) -> OpSummary {
    host.log(LogLevel::Error, &messages::compress_failure(name, reason));
    sum.err += 1;
    *sum
}

/// 実FSの選択項目を tar でまとめて xz 圧縮する（.tar.xz）。ディレクトリは再帰。進捗（ファイル
/// 単位）・中止・サマリは [`run_compress`] と同じ host 経由。`dst` は上書き作成する（存在確認は
/// 呼び側）。中止された場合は作りかけを消す。
pub fn run_compress_tar_xz(
    host: &dyn OperationHost,
    src_dir: &Path,
    names: &[String],
    dst: &Path,
) -> OpSummary {
    run_operation(host, "圧縮", ResultStyle::Copy, || {
        let mut sum = OpSummary::default();
        let out = match std::fs::File::create(dst) {
            Ok(f) => f,
            Err(e) => {
                host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst), &e.to_string()));
                sum.err += 1;
                return sum;
            }
        };
        let xzw = match lzma_rust2::XzWriter::new(out, lzma_rust2::XzOptions::with_preset(6)) {
            Ok(w) => w,
            Err(e) => {
                let _ = std::fs::remove_file(dst);
                host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst), &e.to_string()));
                sum.err += 1;
                return sum;
            }
        };
        let mut builder = tar::Builder::new(xzw);
        for name in names {
            if should_stop(host) {
                sum.cancelled = true;
                break;
            }
            let src = src_dir.join(name);
            if let Flow::Cancel = add_to_tar(host, &mut builder, &src, name, &mut sum) {
                sum.cancelled = true;
                break;
            }
        }
        // tar トレーラを書いて内側の xz writer を取り出し、xz ストリームを閉じる。
        match builder.into_inner() {
            Ok(xzw) => {
                if let Err(e) = xzw.finish()
                    && !sum.cancelled
                {
                    host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst), &e.to_string()));
                    sum.err += 1;
                }
            }
            Err(e) => {
                if !sum.cancelled {
                    host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst), &e.to_string()));
                    sum.err += 1;
                }
            }
        }
        if sum.cancelled {
            let _ = std::fs::remove_file(dst);
        }
        sum
    })
}

/// 1項目を tar へ追加する。ディレクトリは再帰（空ディレクトリもエントリとして残す）。
/// `rel` は書庫内の相対パス（'/' 区切り）。
fn add_to_tar(
    host: &dyn OperationHost,
    builder: &mut tar::Builder<lzma_rust2::XzWriter<std::fs::File>>,
    src: &Path,
    rel: &str,
    sum: &mut OpSummary,
) -> Flow {
    let name = file_name(src);
    if src.is_dir() {
        if let Err(e) = builder.append_dir(rel, src) {
            host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
            sum.err += 1;
            return Flow::Continue;
        }
        let entries = match std::fs::read_dir(src) {
            Ok(e) => e,
            Err(e) => {
                host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
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
            if let Flow::Cancel = add_to_tar(host, builder, &entry.path(), &child_rel, sum) {
                return Flow::Cancel;
            }
        }
        Flow::Continue
    } else {
        host.log(LogLevel::Normal, &messages::compress(&name));
        if let Err(e) = builder.append_path_with_name(src, rel) {
            host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
            sum.err += 1;
        } else {
            sum.ok += 1;
        }
        Flow::Continue
    }
}

/// 1項目を zip へ追加する。ディレクトリは再帰。`rel` は zip 内の相対パス（'/' 区切り）。
fn add_to_zip(
    host: &dyn OperationHost,
    zw: &mut zip::ZipWriter<std::fs::File>,
    src: &Path,
    rel: &str,
    sum: &mut OpSummary,
) -> Flow {
    use std::io::Write;
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let name = file_name(src);
    if src.is_dir() {
        if let Err(e) = zw.add_directory(format!("{rel}/"), opts) {
            host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
            sum.err += 1;
            return Flow::Continue;
        }
        let entries = match std::fs::read_dir(src) {
            Ok(e) => e,
            Err(e) => {
                host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
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
            if let Flow::Cancel = add_to_zip(host, zw, &entry.path(), &child_rel, sum) {
                return Flow::Cancel;
            }
        }
        Flow::Continue
    } else {
        let bytes = match std::fs::read(src) {
            Ok(b) => b,
            Err(e) => {
                host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
                sum.err += 1;
                return Flow::Continue;
            }
        };
        host.log(LogLevel::Normal, &messages::compress(&name));
        let r = zw
            .start_file(rel.to_string(), opts)
            .map_err(|e| std::io::Error::other(e.to_string()))
            .and_then(|_| zw.write_all(&bytes));
        if let Err(e) = r {
            host.log(LogLevel::Error, &messages::compress_failure(&name, &e.to_string()));
            sum.err += 1;
        } else {
            sum.ok += 1;
        }
        Flow::Continue
    }
}
