//! 開発専用のローカル制御・観測サーバ（`feature = "debug-server"` 下のみコンパイル）。
//!
//! `--debug-server[=PORT]` 起動時に 127.0.0.1 で小さな HTTP を立て、`GET /state` で
//! UI 状態を JSON で返す（段階2 以降でコマンド注入・スナップショット・モーダル操作）。
//!
//! winsafe の GUI 状態は UI スレッドでしか触れない（`!Send`）ため、HTTP スレッドは
//! 要求をキューへ積んで `winutil::msg::DEBUG_WAKE` を main 窓へ Post し、応答チャネルで待つ。
//! 実際の状態読取/操作は UI スレッドの WM ハンドラ（main.rs）が行う。

use crate::ui_marshal;

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

    /// キー編集ページの観測状態（debug-server で読む）。
    #[derive(serde::Serialize)]
    pub struct KeyEditorState {
        /// 検索で絞り込んだ後の行＝(コマンドのトークン名, 割り当て chord トークン群)。
        pub rows: Vec<(String, Vec<String>)>,
        /// 選択行 index（絞り込み後の `rows` 上の位置）。
        pub selected: usize,
        /// 表示先頭行（スクロール位置・見出し行を含む表示行単位）。
        pub top: usize,
        /// キャプチャ待ちか。
        pub capturing: bool,
        /// 機能ピッカー（インライン）中か。`true` の間は `rows` が機能一覧になる。
        pub picking: bool,
        /// 直近の操作結果メッセージ。
        pub status: String,
        /// 現在の検索クエリ（空なら全件）。
        pub query: String,
        /// 並べ方：`"command"`（機能順＝行は (機能, [キー…])）／`"key"`（キー順＝行は (キー, [機能])）。
        pub mode: String,
        /// 衝突＝1 つのキーに 2 機能以上＝`(キー, [機能ラベル…])`。空なら衝突なし。
        pub conflicts: Vec<(String, Vec<String>)>,
    }

    /// (コマンドのトークン名, chord トークン) を割り当てるフック。未知コマンド等は Err。
    pub type BindFn = Box<dyn Fn(&str, &str) -> Result<(), String>>;

    /// 1 引数（chord トークン）を取り Err を返し得るフック。未知キー等は Err。
    pub type ChordFn = Box<dyn Fn(&str) -> Result<(), String>>;

    /// usize 1 つを取るフック（設定ナビのページ切替など）。
    pub type IndexFn = Box<dyn Fn(usize)>;

    /// キー編集ページを UI スレッドで読み書きするフック（gui をクロージャに閉じ込める）。
    pub struct KeyEditorHooks {
        pub read: Box<dyn Fn() -> KeyEditorState>,
        pub select: Box<dyn Fn(usize)>,
        pub bind: BindFn,
        /// 選択行のコマンドの割り当てを全解除する。
        pub unbind: Box<dyn Fn()>,
        /// このページを既定キーマップへ戻す。
        pub reset: Box<dyn Fn()>,
        /// 検索クエリを適用して表示を絞り込む（機能名・キーへの部分一致）。
        pub search: Box<dyn Fn(&str)>,
        /// 並べ方を切り替える（`true`＝キー順／`false`＝機能順）。
        pub set_view: Box<dyn Fn(bool)>,
        /// 選択行のキーを、その呼び出しのまま新しいキー（chord トークン）へ移し替える。未知キーは Err。
        pub rebind: ChordFn,
        /// 選択行へ打鍵を割り当てる（行の生 value を束ねる）。引数つき組込・Script・Eval 行用。未知キーは Err。
        pub capture: ChordFn,
        /// コードを未割当 `Eval` 行として追加する（割り当ては行を選んで capture する）。
        pub add_code: Box<dyn Fn(&str)>,
        /// 選択中の組込コマンド行へ引数を付ける（割り当ては行を選んで capture する）。
        pub set_arg: Box<dyn Fn(&str)>,
        /// キー順で選択行の li 番目の機能を差し替えるピックモードへ入る（インライン機能ピッカー）。
        pub pick: Box<dyn Fn(usize)>,
        /// ピックモードで選択中の機能を確定する。
        pub pick_commit: Box<dyn Fn()>,
        /// ピックモードを中止する。
        pub pick_cancel: Box<dyn Fn()>,
        /// 表示先頭行を指定位置へ（ホイール／スクロールバーと同じ経路・範囲外はクランプ）。
        pub scroll: Box<dyn Fn(i32)>,
        /// キー順で空キー定義（機能未割当・－表示）を作る。未知キーは Err。
        pub add_keydef: ChordFn,
    }

    thread_local! {
        /// 開いている設定ダイアログのキー編集ページ（カテゴリ名→フック）。設定を開くたびに
        /// [`clear_key_editors`] で作り直す。
        static KEY_EDITORS: RefCell<Vec<(&'static str, KeyEditorHooks)>> =
            const { RefCell::new(Vec::new()) };
    }

    /// キー編集ページのフックを登録する（`KeyEditor` が生成時に呼ぶ）。
    pub fn register_key_editor(category: &'static str, hooks: KeyEditorHooks) {
        KEY_EDITORS.with(|e| e.borrow_mut().push((category, hooks)));
    }

    /// 登録済みのキー編集ページ・設定ナビを全消去する（設定ダイアログを開く直前に呼ぶ）。
    pub fn clear_key_editors() {
        KEY_EDITORS.with(|e| e.borrow_mut().clear());
        SETTINGS_NAV.with(|n| *n.borrow_mut() = None);
    }

    thread_local! {
        /// 開いている設定ダイアログの左ナビ＝pane 番号を渡すとそのページへ切り替えるフック。
        static SETTINGS_NAV: RefCell<Option<IndexFn>> = const { RefCell::new(None) };
    }

    /// 設定ナビのページ切替フックを登録する（設定ダイアログ生成時に呼ぶ）。
    pub fn register_settings_nav(cb: IndexFn) {
        SETTINGS_NAV.with(|n| *n.borrow_mut() = Some(cb));
    }

    /// 設定ナビを pane 番号で切り替える（設定ダイアログが開いていなければ `None`）。
    pub fn with_settings_nav<R>(f: impl FnOnce(&dyn Fn(usize)) -> R) -> Option<R> {
        SETTINGS_NAV.with(|n| n.borrow().as_ref().map(|cb| f(cb.as_ref())))
    }

    /// 指定カテゴリのキー編集ページに対して処理する（無ければ `None`）。
    pub fn with_key_editor<R>(category: &str, f: impl FnOnce(&KeyEditorHooks) -> R) -> Option<R> {
        KEY_EDITORS.with(|e| {
            e.borrow()
                .iter()
                .find(|(c, _)| *c == category)
                .map(|(_, h)| f(h))
        })
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
    /// `POST /view/search/history/<index>`：検索履歴の index 番目（新しい順）を選んで検索する。
    ViewSearchHistory { index: usize },
    /// `POST /view/search/dropdown/<open|close>`：履歴ドロップダウンを開く/閉じる。
    ViewSearchDropdown { open: bool },
    /// `POST /view/search/mnemonic/<c|w|r>`：トグルのニーモニック（Alt+C/W/R 相当）を駆動する。
    ViewSearchMnemonic { key: char },
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
    /// `POST /modal/resize/<w>x<h>`：開いているモーダルの窓サイズを w×h（物理px）へ変える。
    /// WM_SIZE が飛んでダイアログの再レイアウトが走るので、リサイズ追従を headless で検証できる。
    ModalResize { width: i32, height: i32 },
    /// `GET /script/commands`：登録済みスクリプトコマンド名の一覧（JSON 文字列配列）。
    ScriptCommands,
    /// `POST /script/invoke/<name>`：登録済みスクリプトコマンドを名前で実行する（投げっぱなし）。
    ScriptInvoke { name: String },
    /// `POST /script/eval`：body の TS/JS ソースをスクリプトエンジンで評価する（投げっぱなし）。
    ScriptEval { code: String },
    /// `POST /script/eval-value`：body の TS/JS コードを評価し、最後の式の値を文字列で返す（同期）。
    ScriptEvalValue { code: String },
    /// `GET /keys/<category>`：設定ダイアログのキー編集ページ状態（行・選択・キャプチャ・状態）。
    /// `category` は `filer`/`text`/`image`。設定ダイアログが開いていなければ 404。
    KeysState { category: String },
    /// `POST /keys/<category>/select/<index>`：キー編集ページの選択行を index にする。
    KeysSelect { category: String, index: usize },
    /// `POST /keys/<category>/bind`：選択不要・body の JSON 配列 `["Command","Ctrl+K"]` を割り当てる
    /// （実打鍵キャプチャと同じ assign 経路を叩く）。
    KeysBind { category: String, command: String, chord: String },
    /// `POST /keys/<category>/unbind`：選択行のコマンドの割り当てを全解除する。
    KeysUnbind { category: String },
    /// `POST /keys/<category>/reset`：このページを既定キーマップへ戻す。
    KeysReset { category: String },
    /// `POST /keys/<category>/search`：body の文字列で表示を絞り込む（機能名・キーへの部分一致・
    /// 大小無視）。空 body で全件へ戻す。
    KeysSearch { category: String, query: String },
    /// `POST /keys/<category>/view`：並べ方を切り替える。body が `key` ならキー順、それ以外は機能順。
    KeysSetView { category: String, by_key: bool },
    /// `POST /keys/<category>/rebind`：選択行のキーを body のキーへ移し替える（変更）。
    KeysRebind { category: String, chord: String },
    /// `POST /keys/<category>/capture`：選択行へ body のキーを割り当てる（行の呼び出しを束ねる）。
    KeysCapture { category: String, chord: String },
    /// `POST /keys/<category>/code`：body のコードを未割当 `Eval` 行として追加する（割り当ては capture で）。
    KeysAddCode { category: String, code: String },
    /// `POST /keys/<category>/arg`：選択中の組込コマンド行へ body の引数を付ける（割り当ては capture で）。
    KeysSetArg { category: String, arg: String },
    /// `POST /keys/<category>/pick/<labelIndex>`：キー順で選択行の機能ピッカーへ入る。
    KeysPick { category: String, label: usize },
    /// `POST /keys/<category>/pickcommit`：ピックで選択中の機能を確定する。
    KeysPickCommit { category: String },
    /// `POST /keys/<category>/pickcancel`：ピックを中止する。
    KeysPickCancel { category: String },
    /// `POST /keys/<category>/scroll/<top>`：表示先頭行を top へ（範囲外はクランプ）。
    KeysScroll { category: String, top: i32 },
    /// `POST /keys/<category>/addkeydef`：キー順で body のキーの空キー定義（機能未割当）を作る。
    KeysAddKeyDef { category: String, chord: String },
    /// `POST /settings/nav/<pane>`：設定ダイアログの左ナビを pane 番号のページへ切り替える。
    SettingsNav { pane: usize },
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
pub type SharedQueue = ui_marshal::WakeQueue<Request, Response>;

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
            queue: ui_marshal::new_queue(),
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
            } else if path == "/script/commands" {
                Some(Request::ScriptCommands)
            } else {
                path.strip_prefix("/keys/").map(|cat| Request::KeysState {
                    category: cat.trim_end_matches('/').to_string(),
                })
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
            } else if let Some(n) = path.strip_prefix("/view/search/history/") {
                n.trim_end_matches('/')
                    .parse::<usize>()
                    .ok()
                    .map(|index| Request::ViewSearchHistory { index })
            } else if let Some(s) = path.strip_prefix("/view/search/dropdown/") {
                Some(Request::ViewSearchDropdown {
                    open: s.trim_end_matches('/').eq_ignore_ascii_case("open"),
                })
            } else if let Some(s) = path.strip_prefix("/view/search/mnemonic/") {
                s.trim_end_matches('/')
                    .chars()
                    .next()
                    .map(|key| Request::ViewSearchMnemonic { key })
            } else if let Some(rest) = path.strip_prefix("/view/search/option/") {
                rest.trim_end_matches('/').rsplit_once('/').map(|(name, val)| Request::ViewSearchOption {
                        name: name.to_string(),
                        on: val.eq_ignore_ascii_case("on"),
                    })
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
            } else if let Some(wh) = path.strip_prefix("/modal/resize/") {
                wh.trim_end_matches('/').split_once('x').and_then(|(w, h)| {
                    Some(Request::ModalResize {
                        width: w.parse().ok()?,
                        height: h.parse().ok()?,
                    })
                })
            } else if let Some(name) = path.strip_prefix("/script/invoke/") {
                Some(Request::ScriptInvoke {
                    name: name.trim_end_matches('/').to_string(),
                })
            } else if path == "/script/eval" {
                let mut code = String::new();
                let _ = std::io::Read::read_to_string(req.as_reader(), &mut code);
                Some(Request::ScriptEval { code })
            } else if path == "/script/eval-value" {
                let mut code = String::new();
                let _ = std::io::Read::read_to_string(req.as_reader(), &mut code);
                Some(Request::ScriptEvalValue { code })
            } else if let Some(rest) = path.strip_prefix("/keys/") {
                let rest = rest.trim_end_matches('/');
                if let Some((cat, idx)) = rest.rsplit_once("/select/") {
                    idx.parse::<usize>().ok().map(|index| Request::KeysSelect {
                        category: cat.to_string(),
                        index,
                    })
                } else if let Some(cat) = rest.strip_suffix("/pickcommit") {
                    Some(Request::KeysPickCommit { category: cat.to_string() })
                } else if let Some(cat) = rest.strip_suffix("/pickcancel") {
                    Some(Request::KeysPickCancel { category: cat.to_string() })
                } else if let Some((cat, idx)) = rest.rsplit_once("/pick/") {
                    idx.parse::<usize>().ok().map(|label| Request::KeysPick {
                        category: cat.to_string(),
                        label,
                    })
                } else if let Some((cat, idx)) = rest.rsplit_once("/scroll/") {
                    idx.parse::<i32>().ok().map(|top| Request::KeysScroll {
                        category: cat.to_string(),
                        top,
                    })
                } else if let Some(cat) = rest.strip_suffix("/addkeydef") {
                    let mut chord = String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut chord);
                    Some(Request::KeysAddKeyDef {
                        category: cat.to_string(),
                        chord: chord.trim().to_string(),
                    })
                } else if let Some(cat) = rest.strip_suffix("/bind") {
                    let mut body = String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
                    match parse_command_args(&body) {
                        Ok(args) if args.len() == 2 => Some(Request::KeysBind {
                            category: cat.to_string(),
                            command: args[0].clone(),
                            chord: args[1].clone(),
                        }),
                        Ok(_) => {
                            let _ = req.respond(
                                tiny_http::Response::from_string("bind body must be [\"Command\",\"Chord\"]")
                                    .with_status_code(400),
                            );
                            return;
                        }
                        Err(msg) => {
                            let _ = req
                                .respond(tiny_http::Response::from_string(msg).with_status_code(400));
                            return;
                        }
                    }
                } else if let Some(cat) = rest.strip_suffix("/unbind") {
                    Some(Request::KeysUnbind { category: cat.to_string() })
                } else if let Some(cat) = rest.strip_suffix("/search") {
                    let mut query = String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut query);
                    Some(Request::KeysSearch { category: cat.to_string(), query })
                } else if let Some(cat) = rest.strip_suffix("/rebind") {
                    let mut chord = String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut chord);
                    Some(Request::KeysRebind { category: cat.to_string(), chord: chord.trim().to_string() })
                } else if let Some(cat) = rest.strip_suffix("/capture") {
                    let mut chord = String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut chord);
                    Some(Request::KeysCapture { category: cat.to_string(), chord: chord.trim().to_string() })
                } else if let Some(cat) = rest.strip_suffix("/code") {
                    let mut code = String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut code);
                    Some(Request::KeysAddCode { category: cat.to_string(), code })
                } else if let Some(cat) = rest.strip_suffix("/arg") {
                    let mut arg = String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut arg);
                    Some(Request::KeysSetArg { category: cat.to_string(), arg })
                } else if let Some(cat) = rest.strip_suffix("/view") {
                    let mut body = String::new();
                    let _ = std::io::Read::read_to_string(req.as_reader(), &mut body);
                    Some(Request::KeysSetView {
                        category: cat.to_string(),
                        by_key: body.trim() == "key",
                    })
                } else {
                    rest.strip_suffix("/reset")
                        .map(|cat| Request::KeysReset { category: cat.to_string() })
                }
            } else if let Some(p) = path.strip_prefix("/settings/nav/") {
                p.trim_end_matches('/')
                    .parse::<usize>()
                    .ok()
                    .map(|pane| Request::SettingsNav { pane })
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
    let reply = ui_marshal::call(queue, hwnd_ptr, crate::winutil::msg::DEBUG_WAKE.raw(), kind);
    match reply {
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
