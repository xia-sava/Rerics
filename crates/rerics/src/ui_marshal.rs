//! 別スレッドから UI スレッドへ要求を渡す共通基盤（debug-server / scripting で共用）。
//!
//! winsafe の GUI 状態は UI スレッドでしか触れない（`!Send`）。そこで作業スレッドは
//! 要求を [`WakeQueue`] へ積み、私用ウィンドウメッセージで main 窓を起こして
//! ([`post_wake`])、応答チャネルで結果を待つ ([`call`])。実際の状態読取/操作は
//! UI スレッドの WM ハンドラがキューを drain して行う（処理本体は各呼び出し側に固有）。

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::mpsc::{RecvError, Sender, channel};
use std::sync::{Arc, Mutex};

/// 作業スレッドと UI スレッドが共有する往復キュー。`Req` が要求・`Resp` が応答で、
/// 各エントリは応答を返すための `Sender` を同梱する。
pub type WakeQueue<Req, Resp> = Arc<Mutex<VecDeque<(Req, Sender<Resp>)>>>;

/// 空の [`WakeQueue`] を作る。
pub fn new_queue<Req, Resp>() -> WakeQueue<Req, Resp> {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// 作業スレッドから main 窓を起こす（生ハンドルへ `PostMessageW`）。`msg_id` は
/// [`crate::winutil::msg`] で採番した私用メッセージの `.raw()`。
pub fn post_wake(hwnd_ptr: isize, msg_id: u32) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn PostMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
    }
    unsafe {
        PostMessageW(hwnd_ptr as *mut c_void, msg_id, 0, 0);
    }
}

/// 要求を 1 件キューへ積み、UI スレッドを起こして応答を待つ（同期往復）。
/// UI スレッドが応答前に消えた場合は [`RecvError`] を返す。
pub fn call<Req, Resp>(
    queue: &WakeQueue<Req, Resp>,
    hwnd_ptr: isize,
    msg_id: u32,
    req: Req,
) -> Result<Resp, RecvError> {
    let (tx, rx) = channel();
    queue.lock().unwrap().push_back((req, tx));
    post_wake(hwnd_ptr, msg_id);
    rx.recv()
}
