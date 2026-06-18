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
    let mut sum = OpSummary::default();
    let file = match std::fs::File::create(dst_zip) {
        Ok(f) => f,
        Err(e) => {
            host.log(LogLevel::Error, &messages::compress_failure(&file_name(dst_zip), &e.to_string()));
            sum.err += 1;
            host.log(LogLevel::Error, &messages::copy_result(sum.ok, sum.skip, sum.err));
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
    let level = if sum.err == 0 { LogLevel::Info } else { LogLevel::Error };
    host.log(level, &messages::copy_result(sum.ok, sum.skip, sum.err));
    sum
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
