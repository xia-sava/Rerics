use std::path::Path;
use super::*;

/// `dir` 直下の `names`（ファイル/ディレクトリ）の使用量を再帰集計する。選んだ
/// ディレクトリ自身も `dirs` に数える。ファイル境界で中止/中断を確認する。
pub fn run_calc_size(host: &dyn OperationHost, dir: &Path, names: &[String]) -> DirInfo {
    let mut info = DirInfo::default();
    for name in names {
        if should_stop(host) {
            break;
        }
        calc_into(host, &dir.join(name), &mut info);
    }
    info
}

fn calc_into(host: &dyn OperationHost, path: &Path, info: &mut DirInfo) {
    if should_stop(host) {
        return;
    }
    host.wait_while_suspended();
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.is_dir() {
        info.dirs += 1;
        if let Ok(rd) = std::fs::read_dir(path) {
            for entry in rd.flatten() {
                if should_stop(host) {
                    return;
                }
                calc_into(host, &entry.path(), info);
            }
        }
    } else {
        info.files += 1;
        info.bytes += meta.len();
    }
}
