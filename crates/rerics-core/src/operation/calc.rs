use std::path::{Path, PathBuf};
use super::*;

/// 進捗行を更新する走査件数の間隔。これだけ走査するごとに「使用量計算中… N件」を
/// 書き換える（毎件更新は重いので間引く）。
const CALC_REPORT_EVERY: u64 = 512;

/// `dir` 直下の `names`（ファイル/ディレクトリ）の使用量を再帰集計する。選んだ
/// ディレクトリ自身も `dirs` に数える。ファイル境界で中止/中断を確認し、走査件数を
/// インプレース更新行で随時報告する。
pub fn run_calc_size(host: &dyn OperationHost, dir: &Path, names: &[String]) -> DirInfo {
    let mut info = DirInfo::default();
    let handle = host.begin_progress(LogLevel::Normal, &crate::messages::calc_size_progress(0));
    calc_names(host, dir, names, &mut info, handle);
    host.update_progress(handle, &crate::messages::calc_size_done(info.files + info.dirs));
    info
}

/// 出自ディレクトリ別にまとまった複数グループの使用量を、ひとつの進捗行で合算する
/// （結果一覧の選択項目の情報表示用）。
pub fn run_calc_size_groups(host: &dyn OperationHost, groups: &[(PathBuf, Vec<String>)]) -> DirInfo {
    let mut info = DirInfo::default();
    let handle = host.begin_progress(LogLevel::Normal, &crate::messages::calc_size_progress(0));
    for (dir, names) in groups {
        if should_stop(host) {
            break;
        }
        calc_names(host, dir, names, &mut info, handle);
    }
    host.update_progress(handle, &crate::messages::calc_size_done(info.files + info.dirs));
    info
}

/// `dir` 直下の各 `names` を集計へ加える（進捗行は呼び側が用意した `handle` を使う）。
fn calc_names(
    host: &dyn OperationHost,
    dir: &Path,
    names: &[String],
    info: &mut DirInfo,
    handle: ProgressHandle,
) {
    for name in names {
        if should_stop(host) {
            break;
        }
        calc_into(host, &dir.join(name), info, handle);
    }
}

fn calc_into(host: &dyn OperationHost, path: &Path, info: &mut DirInfo, handle: ProgressHandle) {
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
        report_progress(host, handle, info);
        if let Ok(rd) = std::fs::read_dir(path) {
            for entry in rd.flatten() {
                if should_stop(host) {
                    return;
                }
                calc_into(host, &entry.path(), info, handle);
            }
        }
    } else {
        info.files += 1;
        info.bytes += meta.len();
        report_progress(host, handle, info);
    }
}

/// 走査件数が一定数に達するごとに進捗行を更新する。
fn report_progress(host: &dyn OperationHost, handle: ProgressHandle, info: &DirInfo) {
    let scanned = info.files + info.dirs;
    if scanned.is_multiple_of(CALC_REPORT_EVERY) {
        host.update_progress(handle, &crate::messages::calc_size_progress(scanned));
    }
}
