//! 開発専用のローカル制御・観測サーバ（`feature = "debug-server"` 下のみコンパイル）。
//!
//! `--debug-server[=PORT]` 起動時に 127.0.0.1 で小さな HTTP を立て、`GET /state` で
//! UI 状態を JSON で返す（段階2 以降でコマンド注入・スナップショット・モーダル操作）。
//!
//! winsafe の GUI 状態は UI スレッドでしか触れない（`!Send`）ため、HTTP スレッドは
//! 要求をキューへ積んで `WM_DEBUG_WAKE` を main 窓へ Post し、応答チャネルで待つ。
//! 実際の状態読取/操作は UI スレッドの WM ハンドラ（main.rs）が行う。

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// UI スレッドを起こす自前メッセージ（`WM_APP + 1`）。main.rs の WM ハンドラと対。
pub const WM_DEBUG_WAKE: u32 = 0x8001;

/// `--debug-server` の既定ポート。
pub const DEFAULT_PORT: u16 = 8731;

/// HTTP スレッド → UI スレッドへ渡す要求。応答は同梱の `Sender` で返す。
pub enum Request {
    /// `GET /state[/<pointer>]`：UI 状態（全体 or JSON Pointer で指すサブツリー）。
    /// `pointer` は RFC6901 形式（例 `/panes/left`・空文字＝全体）。
    State { pointer: String },
    /// `POST /command/<Name>`：`Command` をアクティブ側ペインに実行（非モーダルのみ）。
    Command { name: String },
    /// `POST /view/key/<action>`：重ね表示中ビューアの操作（next/prev/close）。
    ViewKey { action: String },
}

/// UI スレッド → HTTP スレッドへの応答（Send 安全な完成データのみ）。
pub enum Response {
    Json(String),
    /// JSON Pointer がツリーに存在しなかった（404 で返す）。
    NotFound,
    /// 不正な要求（未知コマンド・モーダル禁止等。400）。
    BadRequest(String),
    /// 実行時エラー（500）。
    Error(String),
}

/// UI スレッドと HTTP スレッドが共有する要求キュー。
pub type SharedQueue = Arc<Mutex<VecDeque<(Request, Sender<Response>)>>>;

/// MainWindow が 1 フィールドとして保持するブリッジ（キュー＋起動ポート）。
#[derive(Clone)]
pub struct Bridge {
    pub queue: SharedQueue,
    /// `Some` のとき `wm_create` でサーバを起動する。
    pub port: Option<u16>,
}

impl Bridge {
    pub fn new(port: Option<u16>) -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            port,
        }
    }
}

/// コマンドライン引数から `--debug-server` / `--debug-server=PORT` を解析する。
pub fn parse_port() -> Option<u16> {
    for a in std::env::args().skip(1) {
        if a == "--debug-server" {
            return Some(DEFAULT_PORT);
        }
        if let Some(rest) = a.strip_prefix("--debug-server=") {
            return Some(rest.parse().unwrap_or(DEFAULT_PORT));
        }
    }
    None
}

/// HTTP サーバスレッドを起動する。`hwnd_ptr` は main 窓の生ハンドル（`PostMessageW` 用）。
pub fn start(port: u16, queue: SharedQueue, hwnd_ptr: isize) {
    std::thread::spawn(move || {
        let server = match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[debug-server] bind 失敗 127.0.0.1:{port}: {e}");
                return;
            }
        };
        // 発見用：実際にバインドしたアドレスを stdout に1行出す。
        println!("[debug-server] listening on http://127.0.0.1:{port}");
        for req in server.incoming_requests() {
            handle(req, &queue, hwnd_ptr);
        }
    });
}

/// 1 リクエストを処理する：ルート判定 → UI スレッドへ往復 → レスポンス書き出し。
fn handle(req: tiny_http::Request, queue: &SharedQueue, hwnd_ptr: isize) {
    // クエリ文字列を落とした生パス。
    let path = req.url().split('?').next().unwrap_or("");
    let route = match req.method() {
        tiny_http::Method::Get => {
            // `/state` 以降を JSON Pointer として扱う（`/state`→""・`/state/panes/left`→"/panes/left"）。
            if path == "/state" {
                Some(Request::State { pointer: String::new() })
            } else if let Some(rest) = path.strip_prefix("/state/") {
                Some(Request::State { pointer: format!("/{}", rest.trim_end_matches('/')) })
            } else {
                None
            }
        }
        tiny_http::Method::Post => {
            if let Some(name) = path.strip_prefix("/command/") {
                Some(Request::Command { name: name.trim_end_matches('/').to_string() })
            } else if let Some(action) = path.strip_prefix("/view/key/") {
                Some(Request::ViewKey { action: action.trim_end_matches('/').to_string() })
            } else {
                None
            }
        }
        _ => None,
    };
    let Some(kind) = route else {
        let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        return;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    queue.lock().unwrap().push_back((kind, tx));
    post_wake(hwnd_ptr);
    match rx.recv() {
        Ok(Response::Json(s)) => {
            let _ = req.respond(json_response(s));
        }
        Ok(Response::NotFound) => {
            let _ = req.respond(
                tiny_http::Response::from_string("no such path in state").with_status_code(404),
            );
        }
        Ok(Response::BadRequest(m)) => {
            let _ = req.respond(tiny_http::Response::from_string(m).with_status_code(400));
        }
        Ok(Response::Error(m)) => {
            let _ = req.respond(tiny_http::Response::from_string(m).with_status_code(500));
        }
        Err(_) => {
            // UI スレッドが応答前に消えた（終了等）。
            let _ = req.respond(
                tiny_http::Response::from_string("ui gone").with_status_code(503),
            );
        }
    }
}

fn json_response(body: String) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let header =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..])
            .expect("valid header");
    tiny_http::Response::from_string(body).with_header(header)
}

/// HTTP スレッドから main 窓を起こす（生ハンドルへ `PostMessageW`）。
fn post_wake(hwnd_ptr: isize) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn PostMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
    }
    unsafe {
        PostMessageW(hwnd_ptr as *mut c_void, WM_DEBUG_WAKE, 0, 0);
    }
}
