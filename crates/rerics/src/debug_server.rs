//! 開発専用のローカル制御・観測サーバ（`feature = "debug-server"` 下のみコンパイル）。
//!
//! `--debug-server[=PORT]` 起動時に 127.0.0.1 で小さな HTTP を立て、`GET /state` で
//! UI 状態を JSON で返す（段階2 以降でコマンド注入・スナップショット・モーダル操作）。
//!
//! winsafe の GUI 状態は UI スレッドでしか触れない（`!Send`）ため、HTTP スレッドは
//! 要求をキューへ積んで `winutil::msg::DEBUG_WAKE` を main 窓へ Post し、応答チャネルで待つ。
//! 実際の状態読取/操作は UI スレッドの WM ハンドラ（main.rs）が行う。

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// `--debug-server` の既定ポート。
pub const DEFAULT_PORT: u16 = 8731;

/// 開いているモーダルダイアログのレジストリ（UI スレッド専用＝thread_local）。
/// `dialog` モジュールが開閉時に push/pop し、デバッグ制御サーバが観測・操作に使う。
/// モーダルはネスト得るのでスタックで持つ（最後＝最前面）。
pub mod modal_registry {
    use std::cell::RefCell;

    /// 多列 ListView モーダルを UI スレッドで読み書きするフック（同一スレッドなので
    /// gui コントロールをクロージャに閉じ込めて使う＝raw メッセージを避ける）。
    pub struct ListViewHooks {
        pub headers: Vec<String>,
        /// 現在の (行＝各列セル, 選択行 index) をライブで読む（プログレッシブ更新も反映される）。
        pub read: Box<dyn Fn() -> (Vec<Vec<String>>, usize)>,
        /// 指定 index の行を選択する。
        pub select: Box<dyn Fn(usize)>,
    }

    /// 1 つの開いているモーダル。`*_ptr` は HWND の生ポインタ（UI スレッド内でのみ有効）。
    pub struct ModalEntry {
        pub kind: &'static str,
        pub title: String,
        pub prompt: String,
        pub modal_ptr: isize,
        pub has_input: bool,
        /// (ラベル, ctrl_id)。OK=1・Cancel=2、その他は 100+。
        pub buttons: Vec<(String, u16)>,
        /// リスト選択モーダルの項目（リストでなければ空）。
        pub items: Vec<String>,
        /// リスト選択モーダルの初期選択行（リストでなければ 0）。
        pub selected: usize,
        /// 多列 ListView モーダルのフック（単列 ListBox や非リストなら None）。
        pub list_view: Option<ListViewHooks>,
    }

    thread_local! {
        static STACK: RefCell<Vec<ModalEntry>> = const { RefCell::new(Vec::new()) };
    }

    /// モーダルを登録する（`dialog` が wm_create で呼ぶ）。
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        kind: &'static str,
        title: &str,
        prompt: &str,
        modal_ptr: isize,
        has_input: bool,
        buttons: Vec<(String, u16)>,
    ) {
        STACK.with(|s| {
            s.borrow_mut().push(ModalEntry {
                kind,
                title: title.to_string(),
                prompt: prompt.to_string(),
                modal_ptr,
                has_input,
                buttons,
                items: Vec::new(),
                selected: 0,
                list_view: None,
            })
        });
    }

    /// 多列 ListView 選択モーダルを登録する（ドライブ選択など。`hooks` で行と選択を
    /// ライブに読み書きする）。
    pub fn push_list_view(
        kind: &'static str,
        title: &str,
        modal_ptr: isize,
        buttons: Vec<(String, u16)>,
        hooks: ListViewHooks,
    ) {
        STACK.with(|s| {
            s.borrow_mut().push(ModalEntry {
                kind,
                title: title.to_string(),
                prompt: String::new(),
                modal_ptr,
                has_input: false,
                buttons,
                items: Vec::new(),
                selected: 0,
                list_view: Some(hooks),
            })
        });
    }

    /// リスト選択モーダルを登録する（`dialog::list_box` が wm_create で呼ぶ）。
    pub fn push_list(
        kind: &'static str,
        title: &str,
        items: Vec<String>,
        selected: usize,
        modal_ptr: isize,
        buttons: Vec<(String, u16)>,
    ) {
        STACK.with(|s| {
            s.borrow_mut().push(ModalEntry {
                kind,
                title: title.to_string(),
                prompt: String::new(),
                modal_ptr,
                has_input: false,
                buttons,
                items,
                selected,
                list_view: None,
            })
        });
    }

    /// 最前面のモーダルを取り除く（`dialog` が show_modal 後に呼ぶ）。
    pub fn pop() {
        STACK.with(|s| {
            s.borrow_mut().pop();
        });
    }

    /// 最前面モーダルに対して処理する（無ければ `None` を渡す）。
    pub fn with_top<R>(f: impl FnOnce(Option<&ModalEntry>) -> R) -> R {
        STACK.with(|s| f(s.borrow().last()))
    }
}

/// HTTP スレッド → UI スレッドへ渡す要求。応答は同梱の `Sender` で返す。
pub enum Request {
    /// `GET /state[/<pointer>]`：UI 状態（全体 or JSON Pointer で指すサブツリー）。
    /// `pointer` は RFC6901 形式（例 `/panes/left`・空文字＝全体）。
    State { pointer: String },
    /// `GET /presentation[/<pointer>]`：解決済みの外見情報（色/フォント/レイアウト寸法）。
    Presentation { pointer: String },
    /// `POST /command/<Name>`：`Command` をアクティブ側ペインに実行（非モーダルのみ）。
    /// body が JSON 文字列配列（例 `["D:"]`）なら引数として渡す。空 body は引数なし。
    Command { name: String, args: Vec<String> },
    /// `POST /view/key/<action>`：重ね表示中ビューアの操作（next/prev/close）。
    ViewKey { action: String },
    /// `POST /view/search`：テキストビューアのインライン検索バーへ文字列を入れて即時検索（値は body）。
    /// バーが閉じていれば開く。インクリメンタル検索を headless から駆動するための直接経路。
    ViewSearch { value: String },
    /// `POST /view/search/key/<key>`：検索バーのキー操作（down/up＝一致移動・enter＝確定・esc＝取消）。
    ViewSearchKey { key: String },
    /// `POST /view/search/option/<name>/<on|off>`：検索オプション（case/word/regex）を切り替える。
    ViewSearchOption { name: String, on: bool },
    /// `GET /snapshot[/<spec>]`：画面 PNG。`spec` は ""（全体）・名前付き要素・
    /// `x,y-WxH`（数値範囲）・`<name>/<x,y-WxH>`（要素相対のサブ範囲）。
    Snapshot { spec: String },
    /// `POST /modal/key/<key>`：開いているモーダルへキー送出（enter/esc/y/n…）。
    ModalKey { key: String },
    /// `POST /modal/text`：開いているモーダルの入力欄へ文字列を設定（値は body）。
    ModalText { value: String },
    /// `POST /modal/command/<role>`：開いているモーダルのボタンを役割名/ラベルで押す。
    ModalCommand { role: String },
    /// `POST /modal/select/<index>`：リスト選択モーダルの選択行を index にする。
    ModalSelect { index: usize },
    /// `POST /modal/check`：開いているモーダルの最初のチェックボックスをトグルする。
    ModalCheck,
}

/// UI スレッド → HTTP スレッドへの応答（Send 安全な完成データのみ）。
pub enum Response {
    Json(String),
    /// PNG バイト列（スナップショット）。
    Png(Vec<u8>),
    /// JSON Pointer がツリーに存在しなかった（404 で返す）。
    NotFound,
    /// 不正な要求（未知コマンド・モーダル禁止等。400）。
    BadRequest(String),
    /// 実行時エラー（500）。
    Error(String),
}

/// UI スレッドと HTTP スレッドが共有する要求キュー。
pub type SharedQueue = Arc<Mutex<VecDeque<(Request, Sender<Response>)>>>;

/// MainWindow が 1 フィールドとして保持するブリッジ（キュー＋起動ポート＋書込み許可）。
#[derive(Clone)]
pub struct Bridge {
    pub queue: SharedQueue,
    /// `Some` のとき `wm_create` でサーバを起動する。
    pub port: Option<u16>,
    /// `--debug-allow-write` 指定時 true。破壊的（ファイル操作）コマンドの実行可否。
    pub allow_write: bool,
    /// `--headless` 指定時 true。窓を完全非表示で起動する（最小化でなく hidden）。
    pub headless: bool,
}

impl Bridge {
    pub fn new(port: Option<u16>, allow_write: bool, headless: bool) -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            port,
            allow_write,
            headless,
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

/// `--debug-allow-write` が指定されているか。
pub fn parse_allow_write() -> bool {
    std::env::args().skip(1).any(|a| a == "--debug-allow-write")
}

/// `--headless` が指定されているか（窓を非表示で起動）。
pub fn parse_headless() -> bool {
    std::env::args().skip(1).any(|a| a == "--headless")
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
        // 発見用：実際にバインドしたアドレスを stdout に1行出す（`--debug-server=0` で
        // OS にポートを任せたときも、ここで割り当て済みの実ポートを報告する）。
        let bound = server
            .server_addr()
            .to_ip()
            .map(|a| a.port())
            .unwrap_or(port);
        println!("[debug-server] listening on http://127.0.0.1:{bound}");
        for req in server.incoming_requests() {
            handle(req, &queue, hwnd_ptr);
        }
    });
}

/// 1 リクエストを処理する：ルート判定 → UI スレッドへ往復 → レスポンス書き出し。
fn handle(mut req: tiny_http::Request, queue: &SharedQueue, hwnd_ptr: isize) {
    // クエリ文字列を落とした生パス。
    let path = req.url().split('?').next().unwrap_or("").to_string();
    let path = path.as_str();
    let method = req.method().clone();
    let route = match method {
        tiny_http::Method::Get => {
            // `/state` 以降を JSON Pointer として扱う（`/state`→""・`/state/panes/left`→"/panes/left"）。
            if path == "/state" {
                Some(Request::State { pointer: String::new() })
            } else if let Some(rest) = path.strip_prefix("/state/") {
                Some(Request::State { pointer: format!("/{}", rest.trim_end_matches('/')) })
            } else if path == "/presentation" {
                Some(Request::Presentation { pointer: String::new() })
            } else if let Some(rest) = path.strip_prefix("/presentation/") {
                Some(Request::Presentation { pointer: format!("/{}", rest.trim_end_matches('/')) })
            } else if path == "/snapshot" || path == "/snapshot.png" {
                Some(Request::Snapshot { spec: String::new() })
            } else if let Some(rest) = path.strip_prefix("/snapshot/") {
                let spec = rest.trim_end_matches('/').trim_end_matches(".png");
                Some(Request::Snapshot { spec: spec.to_string() })
            } else {
                None
            }
        }
        tiny_http::Method::Post => {
            if let Some(name) = path.strip_prefix("/command/") {
                let mut body = String::new();
                let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
                match parse_command_args(&body) {
                    Ok(args) => Some(Request::Command {
                        name: name.trim_end_matches('/').to_string(),
                        args,
                    }),
                    Err(msg) => {
                        let _ = req
                            .respond(tiny_http::Response::from_string(msg).with_status_code(400));
                        return;
                    }
                }
            } else if let Some(rest) = path.strip_prefix("/view/search/option/") {
                match rest.trim_end_matches('/').rsplit_once('/') {
                    Some((name, val)) => Some(Request::ViewSearchOption {
                        name: name.to_string(),
                        on: val.eq_ignore_ascii_case("on"),
                    }),
                    None => None,
                }
            } else if let Some(key) = path.strip_prefix("/view/search/key/") {
                Some(Request::ViewSearchKey { key: key.trim_end_matches('/').to_string() })
            } else if path == "/view/search" {
                let mut value = String::new();
                let _ = std::io::Read::read_to_string(req.as_reader(), &mut value);
                Some(Request::ViewSearch { value })
            } else if let Some(action) = path.strip_prefix("/view/key/") {
                Some(Request::ViewKey { action: action.trim_end_matches('/').to_string() })
            } else if let Some(key) = path.strip_prefix("/modal/key/") {
                Some(Request::ModalKey { key: key.trim_end_matches('/').to_string() })
            } else if let Some(role) = path.strip_prefix("/modal/command/") {
                Some(Request::ModalCommand { role: role.trim_end_matches('/').to_string() })
            } else if let Some(n) = path.strip_prefix("/modal/select/") {
                n.trim_end_matches('/')
                    .parse::<usize>()
                    .ok()
                    .map(|index| Request::ModalSelect { index })
            } else if path == "/modal/text" {
                let mut value = String::new();
                let _ = std::io::Read::read_to_string(req.as_reader(), &mut value);
                Some(Request::ModalText { value })
            } else if path == "/modal/check" {
                Some(Request::ModalCheck)
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
        Ok(Response::Png(bytes)) => {
            let header =
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"image/png"[..])
                    .expect("valid header");
            let _ = req.respond(tiny_http::Response::from_data(bytes).with_header(header));
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

/// `POST /command/<Name>` の body を引数列に解釈する。空（空白のみ）＝引数なし。
/// 非空なら JSON 文字列配列（例 `["D:", "foo"]`）を要求する。
fn parse_command_args(body: &str) -> Result<Vec<String>, String> {
    if body.trim().is_empty() {
        return Ok(Vec::new());
    }
    let val: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("command body must be a JSON array of strings: {e}"))?;
    let arr = val
        .as_array()
        .ok_or_else(|| "command body must be a JSON array of strings".to_string())?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| "command args must all be strings".to_string())
        })
        .collect()
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
        PostMessageW(hwnd_ptr as *mut c_void, crate::winutil::msg::DEBUG_WAKE.raw(), 0, 0);
    }
}
