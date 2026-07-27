//! 表示中ディレクトリの更新監視（原作 FileSystemWatcher 相当の自動リロード）。
//!
//! サイドごとに専用スレッドを1本立て、`ReadDirectoryChangesW` を overlapped I/O で回す。
//! 変更を検知したら設定の静穏待ち時間（`wait_ms`）だけ無音が続くのを待ち、静まってから
//! UI スレッドへ [`RELOAD_WATCH`](crate::winutil::msg::RELOAD_WATCH) を投げて再読込させる。
//! `HANDLE` はスレッド跨ぎに送れないので、生ポインタ相当の `isize` で受け渡してスレッド内で
//! 再構成する。停止は手動リセットイベントの合図＋スレッド join で行なう。停止合図以外で監視が
//! 終わったときは原因の Win32 エラー値を添えて
//! [`WATCH_DIED`](crate::winutil::msg::WATCH_DIED) を投げ、UI 側が記録して張り直せるようにする。

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDriveTypeW, GetVolumePathNameW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE, FILE_NOTIFY_CHANGE_ATTRIBUTES,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::winutil::{self, msg};

/// 固定ディスク（`GetDriveTypeW` の `DRIVE_FIXED`）。定義が別 feature 側にあるので値を写す。
const DRIVE_FIXED: u32 = 3;

/// `WaitForMultipleObjects` に渡す待機ハンドルの添字。変更通知＝0・停止合図＝1。
const WAIT_CHANGED: u32 = 0;
const WAIT_STOP: u32 = 1;

/// 監視スレッド1本を束ねるハンドル。破棄時にスレッドを止めてイベントを閉じる。
pub(crate) struct WatchHandle {
    /// 監視中の実ディレクトリ。再アーム時に「同じ場所なら張り替えない」判定に使う。
    dir: PathBuf,
    /// この監視スレッドが使っている静穏待ち時間（ms）。設定変更時の張り替え判定に使う。
    wait_ms: u64,
    /// 停止合図用の手動リセットイベント（`HANDLE` の生ポインタを `isize` で保持）。
    stop: isize,
    thread: Option<JoinHandle<()>>,
}

impl WatchHandle {
    /// `dir` の監視を開始する。`hwnd_ptr` は再読込要求の送り先、`is_left` は対象サイド。
    /// スレッド生成やイベント作成に失敗したら `None`（監視なしで続行する）。
    pub(crate) fn start(
        dir: PathBuf,
        hwnd_ptr: isize,
        is_left: bool,
        wait_ms: u64,
    ) -> Option<WatchHandle> {
        let stop = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok()?;
        let stop_raw = stop.0 as isize;
        let thread_dir = dir.clone();
        let thread = std::thread::Builder::new()
            .name("dir-watch".to_owned())
            .spawn(move || run(thread_dir, stop_raw, hwnd_ptr, is_left, wait_ms))
            .ok();
        match thread {
            Some(thread) => Some(WatchHandle { dir, wait_ms, stop: stop_raw, thread: Some(thread) }),
            None => {
                unsafe {
                    let _ = CloseHandle(HANDLE(stop_raw as *mut c_void));
                }
                None
            }
        }
    }

    /// この監視が指す実ディレクトリ。
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// この監視スレッドが使っている静穏待ち時間（ms）。
    pub(crate) fn wait_ms(&self) -> u64 {
        self.wait_ms
    }

    /// 監視スレッドが動いているか。停止合図なしに終わっていれば偽になり、張り直しの判断に使う。
    pub(crate) fn is_alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// 監視スレッドだけを止めてハンドルは残す（監視が落ちた状態を作る検証用）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_stop_thread(&self) {
        unsafe {
            let _ = SetEvent(HANDLE(self.stop as *mut c_void));
        }
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(HANDLE(self.stop as *mut c_void));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(HANDLE(self.stop as *mut c_void));
        }
    }
}

/// `dir` を監視すべきか。固定ディスクは常に、それ以外（リムーバブル・ネットワーク・光学・UNC）
/// は `watch_non_fixed` が真のときだけ監視する（原作 AutoReload/AutoReload2 準拠）。
pub(crate) fn should_watch(dir: &Path, watch_non_fixed: bool) -> bool {
    if drive_type(dir) == DRIVE_FIXED {
        return true;
    }
    watch_non_fixed
}

/// `dir` が載っているボリュームのドライブ種別を引く。判定不能なら固定以外として扱う。
fn drive_type(dir: &Path) -> u32 {
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut root = [0u16; 260];
    unsafe {
        if GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut root).is_ok() {
            GetDriveTypeW(PCWSTR(root.as_ptr()))
        } else {
            0
        }
    }
}

/// 監視スレッド本体。1本の未完了 `ReadDirectoryChangesW` を保ちつつ、変更→静穏待ち→再読込要求を
/// 繰り返す。停止イベントが立ったらどの待機点からでも抜ける。停止合図以外で抜けるときは、原因の
/// Win32 エラー値を添えて [`msg::WATCH_DIED`] を投げてから終わる。
fn run(dir: PathBuf, stop_raw: isize, hwnd_ptr: isize, is_left: bool, wait_ms: u64) {
    let stop = HANDLE(stop_raw as *mut c_void);
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    let dir_handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    };
    let dir_handle = match dir_handle {
        Ok(h) if !h.is_invalid() => h,
        Ok(_) => {
            notify_died(hwnd_ptr, is_left, last_error());
            return;
        }
        Err(e) => {
            notify_died(hwnd_ptr, is_left, win32_code(&e));
            return;
        }
    };

    let ov_event = match unsafe { CreateEventW(None, false, false, PCWSTR::null()) } {
        Ok(h) => h,
        Err(e) => {
            unsafe {
                let _ = CloseHandle(dir_handle);
            }
            notify_died(hwnd_ptr, is_left, win32_code(&e));
            return;
        }
    };

    let mut overlapped = OVERLAPPED { hEvent: ov_event, ..Default::default() };
    // 8 KiB（u32 配列でとって DWORD アライン境界を満たす）。中身は解析せず一括再読込するので
    // バッファ溢れ（ERROR_NOTIFY_ENUM_DIR）でも「何か変わった」として扱えれば十分。
    let mut buf = vec![0u32; 2048];
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_ATTRIBUTES
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE;

    let handles = [ov_event, stop];
    // 停止合図以外で抜けたときの原因（Win32 エラー値）。
    let mut died: Option<u32> = None;
    'outer: loop {
        if let Err(e) = issue_read(dir_handle, &mut buf, filter, &mut overlapped) {
            // ディレクトリが消えた等。最後に一度だけ再読込を促して終わる。
            winutil::post_app_message_params(hwnd_ptr, msg::RELOAD_WATCH, is_left as usize, 0);
            died = Some(win32_code(&e));
            break;
        }
        // 最初の変更（または停止）を待つ。
        match unsafe { WaitForMultipleObjects(&handles, false, INFINITE) }.0 {
            WAIT_CHANGED => consume(dir_handle, &overlapped),
            WAIT_STOP => {
                cancel_and_drain(dir_handle, &mut overlapped);
                break;
            }
            _ => {
                died = Some(last_error());
                cancel_and_drain(dir_handle, &mut overlapped);
                break;
            }
        }
        // 変更を検知。静穏（wait_ms 無音）になるまで待って束ねる。
        loop {
            if issue_read(dir_handle, &mut buf, filter, &mut overlapped).is_err() {
                break;
            }
            match unsafe { WaitForMultipleObjects(&handles, false, wait_ms as u32) }.0 {
                WAIT_CHANGED => {
                    // 追加の変更。タイマを積み直して待ち直す。
                    consume(dir_handle, &overlapped);
                }
                x if x == WAIT_TIMEOUT.0 => {
                    // 静まった。未完了の読取を畳んで再読込へ。
                    cancel_and_drain(dir_handle, &mut overlapped);
                    break;
                }
                WAIT_STOP => {
                    cancel_and_drain(dir_handle, &mut overlapped);
                    break 'outer;
                }
                _ => {
                    died = Some(last_error());
                    cancel_and_drain(dir_handle, &mut overlapped);
                    break 'outer;
                }
            }
        }
        winutil::post_app_message_params(hwnd_ptr, msg::RELOAD_WATCH, is_left as usize, 0);
    }

    unsafe {
        let _ = CloseHandle(ov_event);
        let _ = CloseHandle(dir_handle);
    }
    if let Some(code) = died {
        notify_died(hwnd_ptr, is_left, code);
    }
}

/// 停止合図以外で監視が終わったことを UI へ知らせる。`code` は原因の Win32 エラー値。
fn notify_died(hwnd_ptr: isize, is_left: bool, code: u32) {
    winutil::post_app_message_params(hwnd_ptr, msg::WATCH_DIED, is_left as usize, code as isize);
}

/// 直近の Win32 エラー値。
fn last_error() -> u32 {
    unsafe { GetLastError().0 }
}

/// `windows` クレートのエラーを Win32 エラー値へ均す（`HRESULT_FROM_WIN32` の逆変換）。
/// ログに出す番号を [`last_error`] と揃えるために通す。
fn win32_code(err: &windows::core::Error) -> u32 {
    const FACILITY_WIN32: u32 = 0x8007_0000;
    let hr = err.code().0 as u32;
    if hr & 0xFFFF_0000 == FACILITY_WIN32 {
        hr & 0xFFFF
    } else {
        hr
    }
}

/// `overlapped` で1件の変更監視を発行する。成功なら未完了の読取が1つ走る。
fn issue_read(
    dir_handle: HANDLE,
    buf: &mut [u32],
    filter: FILE_NOTIFY_CHANGE,
    overlapped: &mut OVERLAPPED,
) -> windows::core::Result<()> {
    let len = std::mem::size_of_val(buf) as u32;
    unsafe {
        ReadDirectoryChangesW(
            dir_handle,
            buf.as_mut_ptr().cast::<c_void>(),
            len,
            false,
            filter,
            None,
            Some(overlapped as *mut OVERLAPPED),
            None,
        )
    }
}

/// 完了済みの読取結果を回収してイベント状態を確定させる（内容は使わない）。
fn consume(dir_handle: HANDLE, overlapped: &OVERLAPPED) {
    let mut transferred = 0u32;
    unsafe {
        let _ = GetOverlappedResult(dir_handle, overlapped, &mut transferred, false);
    }
}

/// 未完了の読取を取り消し、完了するまで待って `overlapped` を再利用可能な状態に戻す。
fn cancel_and_drain(dir_handle: HANDLE, overlapped: &mut OVERLAPPED) {
    unsafe {
        let _ = CancelIoEx(dir_handle, Some(overlapped as *const OVERLAPPED));
        let mut transferred = 0u32;
        let _ = GetOverlappedResult(dir_handle, overlapped, &mut transferred, true);
    }
}
