use std::path::Path;
use crate::LogLevel;
use crate::messages;
use super::*;

/// コピー/移動を実行する。`move_it` が `true` なら移動。
pub fn run_copy(
    host: &dyn OperationHost,
    src_dir: &Path,
    dst_dir: &Path,
    names: &[String],
    move_it: bool,
) -> OpSummary {
    let verb = if move_it { "移動" } else { "コピー" };
    host.log(LogLevel::Info, &messages::op_started(verb));
    let mut sum = OpSummary::default();
    for name in names {
        if should_stop(host) {
            sum.cancelled = true;
            break;
        }
        let src = src_dir.join(name);
        let dst = dst_dir.join(name);
        // 同一パス（Windows はケース違いも同一とみなす）はエラー。
        if same_path(&src, &dst) {
            let line = if move_it {
                messages::same_move_path(name)
            } else {
                messages::same_copy_path(name)
            };
            host.log(LogLevel::Error, &line);
            sum.err += 1;
            continue;
        }
        // ディレクトリを自分自身の配下へコピー/移動すると無限再帰になるので拒否する。
        if src.is_dir() && dst_within_src(&src, &dst) {
            host.log(LogLevel::Error, &messages::copy_into_self(verb, name));
            sum.err += 1;
            continue;
        }
        if let Flow::Cancel = copy_item(host, &src, &dst, move_it, &mut sum) {
            sum.cancelled = true;
            break;
        }
    }
    let line = messages::copy_result(sum.ok, sum.skip, sum.err);
    let level = if sum.err == 0 { LogLevel::Info } else { LogLevel::Error };
    host.log(level, &line);
    log_op_end(host, verb, &sum);
    sum
}

/// 1項目（ファイルまたはディレクトリ）を再帰的にコピー/移動する。
fn copy_item(
    host: &dyn OperationHost,
    src: &Path,
    dst: &Path,
    move_it: bool,
    sum: &mut OpSummary,
) -> Flow {
    let name = file_name(src);

    // シンボリックリンク／ジャンクションは中へ辿らず、リンク自体を dst へ張り直す。辿ると
    // リンク先の実体をコピーし、移動ではリンク先の元データを消してしまう（選んでいない場所を
    // 壊す）。再作成に失敗（symlink 作成権限が無い等）したら、辿らずスキップして実体を守る。
    if let Some(link_meta) = std::fs::symlink_metadata(src).ok().filter(meta_is_link) {
        return copy_link(host, src, dst, move_it, meta_is_dir(&link_meta), &name, sum);
    }

    // 移動かつ衝突なしなら rename で一括（同一ドライブの高速路）。失敗時は個別へ。
    if move_it && !dst.exists() && std::fs::rename(src, dst).is_ok() {
        host.log(LogLevel::Normal, &messages::move_(&name));
        sum.ok += 1;
        return Flow::Continue;
    }

    // 同名があり、片方がディレクトリで片方がファイル＝種別不一致はスキップする
    // （上書きや無駄な衝突確認をせず、その項目だけ飛ばす）。
    if dst.exists() && src.is_dir() != dst.is_dir() {
        let verb = if move_it { "移動" } else { "コピー" };
        host.log(LogLevel::Warning, &messages::unmatch_attribute(verb, &name));
        sum.skip += 1;
        return Flow::Continue;
    }

    if src.is_dir() {
        let mut created = false;
        if dst.exists() {
            host.log(LogLevel::Warning, &messages::all_ready_exists(&name));
            sum.skip += 1;
        } else if let Err(e) = std::fs::create_dir_all(dst) {
            host.log(LogLevel::Error, &messages::create_directory_failure(&name, &e.to_string()));
            sum.err += 1;
            return Flow::Continue;
        } else {
            host.log(LogLevel::Normal, &messages::create_directory(&name));
            sum.ok += 1;
            created = true;
        }
        let entries = match std::fs::read_dir(src) {
            Ok(e) => e,
            Err(e) => {
                host.log(LogLevel::Error, &messages::copy_failure(&name, &e.to_string()));
                sum.err += 1;
                return Flow::Continue;
            }
        };
        for entry in entries {
            if should_stop(host) {
                return Flow::Cancel;
            }
            let Ok(entry) = entry else { continue };
            let child_dst = dst.join(entry.file_name());
            if let Flow::Cancel = copy_item(host, &entry.path(), &child_dst, move_it, sum) {
                return Flow::Cancel;
            }
        }
        // 配下を入れ終えてからディレクトリ自身の属性/日時を複製する（子の書き込みで更新日時が
        // 変わるため最後に行う）。新規作成したディレクトリにだけ適用する。
        if created {
            apply_dir_metadata(src, dst, host.copy_options());
        }
        if move_it {
            let _ = std::fs::remove_dir(src);
        }
        Flow::Continue
    } else {
        // 衝突解決。別名(Rename)を選んでもその別名が既存と衝突していれば、黙って上書き
        // せず改めてホストへ確認して繰り返す（既存ファイルの誤消去を防ぐ）。
        let mut target = dst.to_path_buf();
        let do_copy = loop {
            if !target.exists() {
                break true;
            }
            let cur = file_name(&target);
            match host.resolve_conflict(&cur) {
                ConflictResolution::Newest => break is_src_newer(src, &target),
                ConflictResolution::Overwrite => break true,
                ConflictResolution::OverwriteForce => {
                    clear_attributes(&target);
                    break true;
                }
                ConflictResolution::Rename(new) => {
                    if new.is_empty() || new == cur {
                        break false;
                    }
                    target = target.with_file_name(&new);
                }
                ConflictResolution::Skip => break false,
                ConflictResolution::Cancel => return Flow::Cancel,
            }
        };
        if !do_copy {
            host.log(LogLevel::Warning, &messages::skip(&name));
            sum.skip += 1;
            return Flow::Continue;
        }
        let line = if move_it {
            messages::move_(&name)
        } else {
            messages::copy(&name)
        };
        let handle = host.begin_progress(LogLevel::Normal, &line);
        let mut tracker = ProgressTracker::new();
        let result = copy_file(src, &target, move_it, &mut |transferred, total| {
            if let Some(pct) = tracker.tick(transferred, total) {
                let text = if move_it {
                    messages::move_progress(&name, pct)
                } else {
                    messages::copy_progress(&name, pct)
                };
                host.update_progress(handle, &text);
            }
            !host.cancelled()
        });
        match result {
            Ok(CopyOutcome::Completed) => {
                host.update_progress(handle, &line);
                sum.ok += 1;
                Flow::Continue
            }
            Ok(CopyOutcome::Cancelled) => {
                host.update_progress(handle, &line);
                Flow::Cancel
            }
            Err(e) => {
                let reason = e.to_string();
                host.update_progress(handle, &line);
                let fail = if move_it {
                    messages::move_failure(&name, &reason)
                } else {
                    messages::copy_failure(&name, &reason)
                };
                host.log(LogLevel::Error, &fail);
                sum.err += 1;
                Flow::Continue
            }
        }
    }
}

/// リンク（symlink/junction）を1件、`dst` へ張り直す。`dst` に既存があればスキップ。移動時は
/// 張り直し成功後にリンク自体だけを除去する（リンク先の実体には触れない）。再作成に失敗
/// （symlink 作成権限が無い等）したら、辿らずスキップしてリンク先の実体を守る。
fn copy_link(
    host: &dyn OperationHost,
    src: &Path,
    dst: &Path,
    move_it: bool,
    is_dir_link: bool,
    name: &str,
    sum: &mut OpSummary,
) -> Flow {
    if dst.exists() {
        host.log(LogLevel::Warning, &messages::all_ready_exists(name));
        sum.skip += 1;
        return Flow::Continue;
    }
    match recreate_link(src, dst, is_dir_link) {
        Ok(()) => {
            let line = if move_it { messages::move_(name) } else { messages::copy(name) };
            host.log(LogLevel::Normal, &line);
            if move_it {
                remove_link(src, is_dir_link);
            }
            sum.ok += 1;
        }
        Err(_) => {
            let verb = if move_it { "移動" } else { "コピー" };
            host.log(LogLevel::Warning, &messages::skip_link(verb, name));
            sum.skip += 1;
        }
    }
    Flow::Continue
}

/// `src` のリンク先を読み、同じ先を指すシンボリックリンクを `dst` に作る。ジャンクションは
/// 同等のディレクトリシンボリックリンクとして張り直す。Windows の symlink 作成には権限
/// （開発者モード/管理者）が要るため失敗し得る。
#[cfg(windows)]
fn recreate_link(src: &Path, dst: &Path, is_dir_link: bool) -> std::io::Result<()> {
    let target = std::fs::read_link(src)?;
    if is_dir_link {
        std::os::windows::fs::symlink_dir(&target, dst)
    } else {
        std::os::windows::fs::symlink_file(&target, dst)
    }
}

#[cfg(not(windows))]
fn recreate_link(src: &Path, dst: &Path, _is_dir_link: bool) -> std::io::Result<()> {
    let target = std::fs::read_link(src)?;
    std::os::unix::fs::symlink(&target, dst)
}

/// リンク自体を除去する（リンク先は辿らない）。ディレクトリリンクは remove_dir。
fn remove_link(src: &Path, is_dir_link: bool) {
    let _ = if is_dir_link {
        std::fs::remove_dir(src)
    } else {
        std::fs::remove_file(src)
    };
}

/// パスを比較用に正規化する（絶対化・区切りを '\\' に統一・末尾区切り除去、
/// Windows では小文字化してケース非依存で比較する）。
fn norm_key(p: &Path) -> Option<String> {
    let abs = std::path::absolute(p).ok()?;
    let s = abs.to_string_lossy().replace('/', "\\");
    let s = s.trim_end_matches('\\').to_owned();
    Some(if cfg!(windows) { s.to_lowercase() } else { s })
}

/// `a` と `b` が同一パスを指すか（Windows はケース違いも同一とみなす）。
fn same_path(a: &Path, b: &Path) -> bool {
    match (norm_key(a), norm_key(b)) {
        (Some(x), Some(y)) => x == y,
        // 正規化できない場合は素の比較にフォールバック。
        _ => a == b,
    }
}

/// `dst` が `src` 自身、または `src` の配下を指すか（自己再帰コピー/移動の検出）。
fn dst_within_src(src: &Path, dst: &Path) -> bool {
    let (Some(s), Some(d)) = (norm_key(src), norm_key(dst)) else {
        return false;
    };
    d == s || d.starts_with(&format!("{s}\\"))
}

/// src の更新時刻が dst より新しいか（読めない場合はコピー扱いで `true`）。
fn is_src_newer(src: &Path, dst: &Path) -> bool {
    match (
        std::fs::metadata(src).and_then(|m| m.modified()),
        std::fs::metadata(dst).and_then(|m| m.modified()),
    ) {
        (Ok(a), Ok(b)) => a > b,
        _ => true,
    }
}

/// 1ファイルをコピーし、移動なら元を消す。コピー中は `progress(transferred, total)`
/// を適時呼んで進捗を通知する。`progress` が `false` を返したら中止する。Windows は
/// `CopyFileExW` のコールバックから随時、それ以外はコピー完了後に1度だけ呼ぶ。
/// 中止された場合は宛先ファイルを残さず [`CopyOutcome::Cancelled`] を返す。
#[cfg(windows)]
fn copy_file(
    src: &Path,
    dst: &Path,
    move_it: bool,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> std::io::Result<CopyOutcome> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    type Routine = unsafe extern "system" fn(
        i64,
        i64,
        i64,
        i64,
        u32,
        u32,
        *mut c_void,
        *mut c_void,
        *mut c_void,
    ) -> u32;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CopyFileExW(
            existing: *const u16,
            new: *const u16,
            routine: Option<Routine>,
            data: *mut c_void,
            cancel: *mut i32,
            flags: u32,
        ) -> i32;
    }

    const PROGRESS_CONTINUE: u32 = 0;
    // 中止して宛先を削除する（PROGRESS_STOP は残すが、こちらは消す＝原作と同じ）。
    const PROGRESS_CANCEL: u32 = 1;
    const ERROR_REQUEST_ABORTED: i32 = 1235;

    unsafe extern "system" fn routine(
        total: i64,
        transferred: i64,
        _stream_size: i64,
        _stream_transferred: i64,
        _stream_no: u32,
        _reason: u32,
        _hsrc: *mut c_void,
        _hdst: *mut c_void,
        data: *mut c_void,
    ) -> u32 {
        if !data.is_null() {
            let cb = unsafe { &mut *(data as *mut &mut dyn FnMut(u64, u64) -> bool) };
            if !cb(transferred.max(0) as u64, total.max(0) as u64) {
                return PROGRESS_CANCEL;
            }
        }
        PROGRESS_CONTINUE
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let src_w: Vec<u16> = src.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let dst_w: Vec<u16> = dst.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut cb: &mut dyn FnMut(u64, u64) -> bool = progress;
    let data = (&mut cb) as *mut &mut dyn FnMut(u64, u64) -> bool as *mut c_void;
    let ok = unsafe {
        CopyFileExW(src_w.as_ptr(), dst_w.as_ptr(), Some(routine), data, std::ptr::null_mut(), 0)
    };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_REQUEST_ABORTED) {
            return Ok(CopyOutcome::Cancelled);
        }
        return Err(err);
    }
    if move_it {
        std::fs::remove_file(src)?;
    }
    Ok(CopyOutcome::Completed)
}

/// 非 Windows ではバイト進捗を取れないため、完了後に1度だけ通知する。
/// `progress` が `false` を返したら宛先を消して中止扱いにする。
#[cfg(not(windows))]
fn copy_file(
    src: &Path,
    dst: &Path,
    move_it: bool,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> std::io::Result<CopyOutcome> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = std::fs::copy(src, dst)?;
    if !progress(bytes, bytes) {
        let _ = std::fs::remove_file(dst);
        return Ok(CopyOutcome::Cancelled);
    }
    if move_it {
        std::fs::remove_file(src)?;
    }
    Ok(CopyOutcome::Completed)
}
