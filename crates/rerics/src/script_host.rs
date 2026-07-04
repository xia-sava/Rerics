//! スクリプトエンジンと GUI をつなぐ配線。
//!
//! エンジンは別スレッドで V8 を回す。GUI 状態に触れるのは UI スレッドだけなので、
//! エンジン→UI の操作（[`HostApi`]）は [`ui_marshal`] でマーシャルする。逆方向（UI→
//! エンジンへの「このコマンドを実行」要求）は [`EngineCmd`] チャネルで送る。エンジンが
//! `HostApi` 経由で UI を待つ間に UI がエンジンを待つとデッドロックするため、UI→エンジンの
//! コマンドは投げっぱなし（完了を待たない）。登録内容の一覧取得と値返し Eval は同期応答を
//! 待つが、UI は待機中も [`MainWindow::recv_pumping`] でマーシャルキューを汲むので、先行
//! スクリプトの `HostApi` 往復と待ち合ってデッドロックすることはない。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use winsafe::co;
use winsafe::prelude::*;

use crate::MainWindow;
use crate::dialog::{
    InputMode, InputSelect, MessageResult, MessageStyle, input_box, input_box_select, list_box,
    message_box,
};
use crate::shell;
use crate::task::{LogEvent, WorkerEvent};
use rerics_core::{Call, Command, LogLevel};

use crate::script::{
    self, HostApi, MemberInfo, PaneItem, PaneSnapshot, RenameSummary, ScriptCommand, ScriptOp,
};
use crate::ui_marshal::{self, WakeQueue};
use crate::winutil::msg::SCRIPT_WAKE;

/// エンジンスレッド → UI スレッドへの要求（[`HostApi`] の各操作）。
pub enum HostCall {
    GetLog,
    ConfigGet(String),
    ChangeOpposite { kind: String, path: String },
    SetPathMask(String),
    CreateDirectory(String),
    Compress { kind: String, archive: String, files: Vec<String> },
    CurrentDir,
    Navigate(String),
    Confirm(String),
    Prompt { message: String, default: String, select_all: bool },
    Select { title: String, items: Vec<String> },
    PaneSnapshot { opposite: bool },
    SetSelected { is_left: bool, index: usize, selected: bool },
    ApplySelection { is_left: bool, changes: Vec<(usize, bool)> },
    Command { name: String, args: Vec<String> },
    RenameFiles { pairs: Vec<(String, String)> },
    BeginOperation {
        op: ScriptOp,
        items: Vec<String>,
        dest: String,
        events: OpDone,
    },
    CancelOperation { token: u64 },
    ShellOpen(String),
    FolderDialog(String),
    OpenDialog(String),
    SaveDialog(String),
    SetClipboard(String),
    GetClipboard,
    SetClipboardImage(String),
    GetClipboardImage(String),
}

/// 走行中の並列ワーカーの `(id, アイソレートハンドル)` を共有する登録簿。ワーカースレッドが直接
/// ロックして自身を積み下ろしし（UI 往復なし＝同期 eval 中でもデッドロックしない）、UI スレッドは
/// 停止時に全 terminate・観測時に件数を読む。`IsolateHandle` は Send+Sync。
pub type WorkerIsolates = Arc<Mutex<Vec<(u64, deno_core::v8::IsolateHandle)>>>;

/// UI スレッド → エンジンスレッドへの応答。
pub enum HostResp {
    Done,
    Dir(String),
    LogText(String),
    Json(Option<serde_json::Value>),
    Bool(bool),
    Text(Option<String>),
    Index(Option<usize>),
    Snapshot(script::PaneSnapshot),
    CommandResult(Result<serde_json::Value, String>),
    OpStarted(Result<u64, String>),
}

type ScriptQueue = WakeQueue<HostCall, HostResp>;

/// 非同期操作のイベント送り先（進捗を複数回・最後に完了を 1 度流す）。
type OpDone = script::JobSender;

/// 1 つのスクリプト操作（job）。複数ディレクトリに分かれる場合は複数タスクを束ねるので、
/// 進捗は随時流し、全タスクの完了で 1 度だけ完了イベントを送る。トークン＝先頭タスク id。
struct JobGroup {
    /// 未完了タスク数。0 で完了を通知する。
    remaining: usize,
    /// イベント送り先（進捗・完了をここへ流す）。
    events: OpDone,
    /// いずれかのタスクが中止されたか（完了イベントの error に反映）。
    cancelled: bool,
    /// いずれかのタスクが失敗したか（完了イベントの error に反映）。
    failed: bool,
    /// 束ねているタスク id 群（キャンセルで全停止する）。
    task_ids: Vec<u64>,
}

/// 進行中のスクリプト発 async 操作のレジストリ。
#[derive(Default)]
struct JobRegistry {
    /// トークン（先頭タスク id）→ job。
    jobs: HashMap<u64, JobGroup>,
    /// タスク id → トークン（どの job に属すか）。
    task_to_token: HashMap<u64, u64>,
}
type Jobs = Rc<RefCell<JobRegistry>>;

/// `SystemTime` を Unix epoch ミリ秒へ。取得不可・1970 より前は 0。
fn system_time_ms(t: Option<std::time::SystemTime>) -> u64 {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// UI スレッド → エンジンスレッドへのコマンド。
/// `Invoke` はスクリプト発のコマンド実行から、`Eval` は機能欄のスクリプト式（[`Call::Script`]）から、
/// `FireEvent` は本体イベントから送られる。`List*`/`EvalValue` は同期取得だが、UI は応答待ちの間
/// [`MainWindow::recv_pumping`] でマーシャルキューを汲むため、先行スクリプトの `HostApi` 往復と
/// デッドロックしない。`ListCommands`/`EvalValue` の送り手は debug-server エンドポイントだけなので、
/// その構成以外では当該バリアントが未使用＝dead_code を許容する。
#[cfg_attr(not(feature = "debug-server"), allow(dead_code))]
pub enum EngineCmd {
    /// 登録済みコマンドを名前で実行する（投げっぱなし）。`args` はコールバックへ転送する。
    Invoke { name: String, args: Vec<String> },
    /// TS/JS ソースを評価する（投げっぱなし）。
    Eval(String),
    /// TS/JS コードを評価し、最後の式の値を文字列で返す（同期取得）。`undefined`/`null` は空文字。
    EvalValue { code: String, tx: Sender<String> },
    /// 現在登録されているコマンドのメタ情報を返す（同期取得）。
    ListCommands(Sender<Vec<ScriptCommand>>),
    /// `r.` で呼べるメンバー（名前＋callable/arity）を返す（補完候補・同期取得）。
    ListMembers(Sender<Vec<MemberInfo>>),
    /// `globalThis` に実在する名前の一覧を返す（未解決識別子チェック用・同期取得）。
    ListGlobals(Sender<Vec<String>>),
    /// `registerMenu` で登録された名前付きメニュー定義を返す（同期取得）。
    ListMenus(Sender<Vec<rerics_core::MenuDef>>),
    /// ファイラー本体の出来事を `rerics.on` ハンドラへ配る（投げっぱなし）。
    FireEvent { event: String, arg: String },
}

/// `MainWindow` が 1 フィールドとして持つスクリプトブリッジ（UI 側の窓口）。
/// エンジンスレッドへ起動時に渡す `Receiver` は `wm_create` で 1 度だけ取り出す。
#[derive(Clone)]
pub struct ScriptBridge {
    pub queue: ScriptQueue,
    cmd_tx: Sender<EngineCmd>,
    cmd_rx: Rc<RefCell<Option<Receiver<EngineCmd>>>>,
    /// スクリプト発のコマンド実行中は true。executeCommand の自己再帰発火を抑える。
    suppress_events: Rc<Cell<bool>>,
    /// 各ペイン（[左, 右]）で最後に changeDirectory を撃った現在地。実移動の検出に使う
    /// （在席再読込・F5・操作後 Focus では現在地が変わらないので撃たない）。
    last_dir: Rc<RefCell<[String; 2]>>,
    /// 進行中のスクリプト発 async 操作。`WorkerEvent::Done` で束ねたタスクの完了を数え、全部
    /// 揃ったら `oneshot` を発火してスクリプトの `await` を解く。
    jobs: Jobs,
}

impl ScriptBridge {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = channel();
        Self {
            queue: ui_marshal::new_queue(),
            cmd_tx,
            cmd_rx: Rc::new(RefCell::new(Some(cmd_rx))),
            suppress_events: Rc::new(Cell::new(false)),
            last_dir: Rc::new(RefCell::new([String::new(), String::new()])),
            jobs: Rc::new(RefCell::new(JobRegistry::default())),
        }
    }

    /// エンジンスレッド起動用に `Receiver` を取り出す（2 度目以降は `None`）。
    fn take_rx(&self) -> Option<Receiver<EngineCmd>> {
        self.cmd_rx.borrow_mut().take()
    }
}

impl Default for ScriptBridge {
    fn default() -> Self {
        Self::new()
    }
}

/// エンジンスレッドに住む [`HostApi`] 実装。各操作を [`ui_marshal::call`] で UI スレッドへ往復する。
struct GuiHost {
    queue: ScriptQueue,
    hwnd_ptr: isize,
    worker_isolates: WorkerIsolates,
    /// ログ専用レーン。追記も更新も投げっぱなしで流し、タスクタイマ取り込み＋WM_PAINT 合体で
    /// 連射しても詰まらない。`getLog` はこのチャネルだけを drain して読み戻す。
    log_tx: Sender<LogEvent>,
    /// ログ行 id の採番元（本体の進捗行と id 空間を共有して衝突を避ける）。
    progress_seq: Arc<AtomicU64>,
}

impl HostApi for GuiHost {
    fn log(&self, level: LogLevel, msg: &str) -> u64 {
        let id = self.progress_seq.fetch_add(1, Ordering::Relaxed);
        let _ = self.log_tx.send(LogEvent::Line { id, level, text: msg.to_string() });
        id
    }

    fn log_update(&self, id: u64, level: Option<LogLevel>, msg: &str) {
        let _ = self.log_tx.send(LogEvent::Update { id, level, text: msg.to_string() });
    }

    fn log_progress_start(&self, id: u64) {
        let _ = self.log_tx.send(LogEvent::ProgressStart { id });
    }

    fn log_progress_set(&self, id: u64, done: u64, total: u64) {
        let _ = self.log_tx.send(LogEvent::ProgressSet { id, done, total });
    }

    fn log_progress_stop(&self, id: u64) {
        let _ = self.log_tx.send(LogEvent::ProgressStop { id });
    }

    fn log_text(&self) -> String {
        match ui_marshal::call(&self.queue, self.hwnd_ptr, SCRIPT_WAKE.raw(), HostCall::GetLog) {
            Ok(HostResp::LogText(text)) => text,
            _ => String::new(),
        }
    }

    fn config_get(&self, key: &str) -> Option<serde_json::Value> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::ConfigGet(key.to_string()),
        ) {
            Ok(HostResp::Json(value)) => value,
            _ => None,
        }
    }

    fn change_opposite(&self, kind: &str, path: &str) {
        let _ = ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::ChangeOpposite { kind: kind.to_string(), path: path.to_string() },
        );
    }

    fn set_path_mask(&self, mask: &str) {
        let _ = ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::SetPathMask(mask.to_string()),
        );
    }

    fn create_directory(&self, name: &str) -> Result<String, String> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::CreateDirectory(name.to_string()),
        ) {
            Ok(HostResp::CommandResult(Ok(serde_json::Value::String(p)))) => Ok(p),
            Ok(HostResp::CommandResult(Err(e))) => Err(e),
            _ => Err("ディレクトリ作成に応答がありませんでした".to_string()),
        }
    }

    fn compress(&self, kind: &str, archive: &str, files: &[String]) -> Result<(), String> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::Compress {
                kind: kind.to_string(),
                archive: archive.to_string(),
                files: files.to_vec(),
            },
        ) {
            Ok(HostResp::CommandResult(Ok(_))) => Ok(()),
            Ok(HostResp::CommandResult(Err(e))) => Err(e),
            _ => Err("圧縮の起動に応答がありませんでした".to_string()),
        }
    }

    fn current_dir(&self) -> String {
        match ui_marshal::call(&self.queue, self.hwnd_ptr, SCRIPT_WAKE.raw(), HostCall::CurrentDir) {
            Ok(HostResp::Dir(dir)) => dir,
            _ => String::new(),
        }
    }

    fn navigate(&self, path: &str) {
        let _ = ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::Navigate(path.to_string()),
        );
    }

    fn confirm(&self, message: &str) -> bool {
        matches!(
            ui_marshal::call(
                &self.queue,
                self.hwnd_ptr,
                SCRIPT_WAKE.raw(),
                HostCall::Confirm(message.to_string()),
            ),
            Ok(HostResp::Bool(true))
        )
    }

    fn prompt(&self, message: &str, default: &str, select_all: bool) -> Option<String> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::Prompt {
                message: message.to_string(),
                default: default.to_string(),
                select_all,
            },
        ) {
            Ok(HostResp::Text(text)) => text,
            _ => None,
        }
    }

    fn select(&self, title: &str, items: &[String]) -> Option<usize> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::Select {
                title: title.to_string(),
                items: items.to_vec(),
            },
        ) {
            Ok(HostResp::Index(index)) => index,
            _ => None,
        }
    }

    fn pane_snapshot(&self, opposite: bool) -> PaneSnapshot {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::PaneSnapshot { opposite },
        ) {
            Ok(HostResp::Snapshot(snap)) => snap,
            _ => PaneSnapshot::default(),
        }
    }

    fn set_selected(&self, is_left: bool, index: usize, selected: bool) {
        let _ = ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::SetSelected {
                is_left,
                index,
                selected,
            },
        );
    }

    fn apply_selection(&self, is_left: bool, changes: &[(usize, bool)]) {
        let _ = ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::ApplySelection {
                is_left,
                changes: changes.to_vec(),
            },
        );
    }

    fn command(&self, name: &str, args: &[String]) -> Result<serde_json::Value, String> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::Command {
                name: name.to_string(),
                args: args.to_vec(),
            },
        ) {
            Ok(HostResp::CommandResult(r)) => r,
            _ => Err("コマンドの実行に応答がありませんでした".to_string()),
        }
    }

    fn rename_files(&self, pairs: &[(String, String)]) -> RenameSummary {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::RenameFiles { pairs: pairs.to_vec() },
        ) {
            Ok(HostResp::CommandResult(Ok(v))) => serde_json::from_value(v).unwrap_or_default(),
            _ => RenameSummary::default(),
        }
    }

    fn begin_operation(
        &self,
        op: ScriptOp,
        items: Vec<String>,
        dest: String,
        events: OpDone,
    ) -> Result<u64, String> {
        // 起動依頼を UI へ送りトークンを得る（同期）。進捗・完了は `events`（mpsc）で後から届く。
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::BeginOperation {
                op,
                items,
                dest,
                events,
            },
        ) {
            Ok(HostResp::OpStarted(r)) => r,
            _ => Err("操作の起動に応答がありませんでした".to_string()),
        }
    }

    fn cancel_operation(&self, token: u64) {
        let _ = ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::CancelOperation { token },
        );
    }

    fn open(&self, path: &str) {
        let _ = ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::ShellOpen(path.to_string()),
        );
    }

    fn folder_dialog(&self, title: &str) -> Option<String> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::FolderDialog(title.to_string()),
        ) {
            Ok(HostResp::Text(text)) => text,
            _ => None,
        }
    }

    fn open_dialog(&self, title: &str) -> Option<String> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::OpenDialog(title.to_string()),
        ) {
            Ok(HostResp::Text(text)) => text,
            _ => None,
        }
    }

    fn modifiers(&self) -> script::Modifiers {
        // 物理キー状態はスレッド非依存なので UI 往復せず直接読む。
        script::Modifiers {
            shift: winsafe::GetAsyncKeyState(co::VK::SHIFT),
            ctrl: winsafe::GetAsyncKeyState(co::VK::CONTROL),
            alt: winsafe::GetAsyncKeyState(co::VK::MENU),
        }
    }

    fn save_dialog(&self, title: &str) -> Option<String> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::SaveDialog(title.to_string()),
        ) {
            Ok(HostResp::Text(text)) => text,
            _ => None,
        }
    }

    fn set_clipboard(&self, text: &str) {
        let _ = ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::SetClipboard(text.to_string()),
        );
    }

    fn get_clipboard(&self) -> String {
        match ui_marshal::call(&self.queue, self.hwnd_ptr, SCRIPT_WAKE.raw(), HostCall::GetClipboard)
        {
            Ok(HostResp::Text(Some(s))) => s,
            _ => String::new(),
        }
    }

    fn set_clipboard_image(&self, path: &str) -> bool {
        matches!(
            ui_marshal::call(
                &self.queue,
                self.hwnd_ptr,
                SCRIPT_WAKE.raw(),
                HostCall::SetClipboardImage(path.to_string()),
            ),
            Ok(HostResp::Bool(true))
        )
    }

    fn get_clipboard_image(&self, dest: &str) -> bool {
        matches!(
            ui_marshal::call(
                &self.queue,
                self.hwnd_ptr,
                SCRIPT_WAKE.raw(),
                HostCall::GetClipboardImage(dest.to_string()),
            ),
            Ok(HostResp::Bool(true))
        )
    }

    fn register_worker(&self, id: u64, handle: deno_core::v8::IsolateHandle) {
        self.worker_isolates.lock().unwrap().push((id, handle));
    }

    fn unregister_worker(&self, id: u64) {
        self.worker_isolates.lock().unwrap().retain(|(wid, _)| *wid != id);
    }
}

/// エディタ補完用の型ファイルを `scripts/` に整備する。`rerics.d.ts`（ホスト API）と
/// `rerics.commands.d.ts`（`Command::ALL` から生成した組込コマンド宣言）は管理対象として
/// 内容が違えば上書きし、`tsconfig.json` は無いときだけ作る（ユーザの編集を残す）。
/// 失敗しても起動を妨げない（補完が効かないだけ）。
fn ensure_script_type_files(dir: &std::path::Path) {
    const TSCONFIG: &str = include_str!("../../../scripting/tsconfig.json");
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    write_if_changed(&dir.join("rerics.d.ts"), crate::hostsig::HOST_DTS);
    write_if_changed(&dir.join("rerics.commands.d.ts"), &rerics_core::commands_dts());
    let tsconfig = dir.join("tsconfig.json");
    if !tsconfig.exists() {
        let _ = std::fs::write(tsconfig, TSCONFIG);
    }
}

/// `content` と中身が違うとき（または無いとき）だけ書く。無駄な書き換えを避ける。
fn write_if_changed(path: &std::path::Path, content: &str) {
    if std::fs::read_to_string(path).is_ok_and(|s| s == content) {
        return;
    }
    let _ = std::fs::write(path, content);
}

/// TS の識別子として使える名前か（プロパティをそのまま書けるか）。
fn is_ts_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// 登録済みスクリプトコマンドを `interface RericsCommands { ... }` へ宣言マージする TS を作る。
/// 組込・host と同名のもの（`r` に生えない）は除外。識別子にできない名前は引用キーにする。
fn script_commands_dts(metas: &[ScriptCommand]) -> String {
    let mut out = String::new();
    out.push_str("// このファイルは Rerics が起動時に登録スクリプトコマンドから自動生成する。手で編集しない。\n");
    out.push_str("// 登録コマンドを r.<名前>() で補完するための宣言。\n\n");
    out.push_str("interface RericsCommands {\n");
    for m in metas {
        if rerics_core::reserved_member(&m.name) {
            continue;
        }
        if let Some(doc) = m.summary.as_deref().or(m.label.as_deref()) {
            out.push_str(&format!("  /** {doc} */\n"));
        }
        let key = if is_ts_ident(&m.name) {
            m.name.clone()
        } else {
            format!("{:?}", m.name)
        };
        out.push_str(&format!("  {key}(...args: unknown[]): unknown;\n"));
    }
    out.push_str("}\n");
    out
}

/// スクリプトエンジンを別スレッドに建てる。起動スクリプト（`data_dir()/scripts`）を読み込み、
/// 以後 [`EngineCmd`] を受けて捌くループに入る。`hwnd_ptr` は UI スレッドを起こす先。
#[allow(clippy::too_many_arguments)] // スレッド起動時に複数のチャネル・共有状態を束ねて渡す。
pub fn spawn_engine(
    queue: ScriptQueue,
    hwnd_ptr: isize,
    cmd_rx: Receiver<EngineCmd>,
    task_tx: Sender<WorkerEvent>,
    log_tx: Sender<LogEvent>,
    worker_isolates: WorkerIsolates,
    progress_seq: Arc<AtomicU64>,
    pool_stopped: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let factory_queue = queue.clone();
        let factory_workers = worker_isolates.clone();
        let factory_log_tx = log_tx.clone();
        let factory_seq = progress_seq.clone();
        let host: Rc<dyn HostApi> = Rc::new(GuiHost {
            queue,
            hwnd_ptr,
            worker_isolates,
            log_tx,
            progress_seq,
        });
        let mut engine = script::Engine::new(host.clone());
        // 並列ワーカーは各スレッド内で UI へマーシャルするホストを建て直す（queue・登録簿・送信口・
        // 採番元はいずれも Arc/Sender で Send なので、生成器ごとスレッドへ渡せる）。
        engine.set_worker_factory(
            std::sync::Arc::new(move || -> script::Host {
                Rc::new(GuiHost {
                    queue: factory_queue.clone(),
                    hwnd_ptr,
                    worker_isolates: factory_workers.clone(),
                    log_tx: factory_log_tx.clone(),
                    progress_seq: factory_seq.clone(),
                })
            }),
            pool_stopped,
        );
        // 停止（強制終了）に使う isolate ハンドルを UI へ渡す。タスク取り込みタイマがまだ
        // 走っていなくても処理されるよう、送ったら UI を起こす。
        let _ = task_tx.send(WorkerEvent::ScriptEngineReady { handle: engine.isolate_handle() });
        ui_marshal::post_wake(hwnd_ptr, SCRIPT_WAKE.raw());
        let scripts = rerics_core::data_dir().join("scripts");
        ensure_script_type_files(&scripts);
        for (path, msg) in script::load_dir(&mut engine, &scripts) {
            host.log(LogLevel::Error, &format!("スクリプト読込エラー [{}]: {}", path.display(), msg));
        }
        // 読込後に確定した登録コマンドの型定義を書き出す（補完用）。
        write_if_changed(
            &scripts.join("rerics.scripts.d.ts"),
            &script_commands_dts(&engine.registered_command_metas()),
        );
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                EngineCmd::Invoke { name, args } => {
                    run_script_task(&mut engine, &task_tx, hwnd_ptr, format!("コマンド {name}"), |engine| {
                        if let Err(e) = engine.invoke_command(&name, &args) {
                            host.log(LogLevel::Error, &format!("コマンド実行エラー [{name}]: {e}"));
                        }
                    });
                }
                EngineCmd::Eval(code) => {
                    run_script_task(&mut engine, &task_tx, hwnd_ptr, "eval".to_owned(), |engine| {
                        if let Err(e) = engine.run_ts(
                            "rerics:eval",
                            "file:///eval.ts",
                            deno_ast::MediaType::TypeScript,
                            code,
                        ) {
                            host.log(LogLevel::Error, &format!("eval エラー: {e}"));
                        }
                    });
                }
                EngineCmd::EvalValue { code, tx } => {
                    run_script_task(&mut engine, &task_tx, hwnd_ptr, "eval".to_owned(), |engine| {
                        let value = engine
                            .eval_to_string(
                                "rerics:eval",
                                "file:///eval.ts",
                                deno_ast::MediaType::TypeScript,
                                code,
                            )
                            .unwrap_or_else(|e| {
                                host.log(LogLevel::Error, &format!("eval エラー: {e}"));
                                String::new()
                            });
                        let _ = tx.send(value);
                    });
                }
                EngineCmd::ListCommands(tx) => {
                    let _ = tx.send(engine.registered_command_metas());
                }
                EngineCmd::ListMembers(tx) => {
                    let _ = tx.send(engine.registered_members());
                }
                EngineCmd::ListGlobals(tx) => {
                    let _ = tx.send(engine.global_names());
                }
                EngineCmd::ListMenus(tx) => {
                    let _ = tx.send(engine.registered_menus());
                }
                EngineCmd::FireEvent { event, arg } => {
                    run_script_task(&mut engine, &task_tx, hwnd_ptr, format!("イベント {event}"), |engine| {
                        if let Err(e) = engine.fire_event(&event, &arg) {
                            host.log(LogLevel::Error, &format!("イベント発火エラー [{event}]: {e}"));
                        }
                    });
                }
            }
        }
    });
}

/// ユーザースクリプトの実行を、停止可能なタスクとして UI へ知らせて回す。開始通知 →
/// 直前の停止フラグ解除 → 実行 → 終了通知、の順。停止（中止）は UI 側が isolate を
/// terminate することで実現する（暴走 JS も止められる）。
fn run_script_task(
    engine: &mut script::Engine,
    task_tx: &Sender<WorkerEvent>,
    hwnd_ptr: isize,
    description: String,
    body: impl FnOnce(&mut script::Engine),
) {
    // 開始を知らせて UI を起こす（取り込みタイマがまだ走っていなくても登録される）。
    let _ = task_tx.send(WorkerEvent::ScriptBegin { text: "スクリプト".to_owned(), description });
    ui_marshal::post_wake(hwnd_ptr, SCRIPT_WAKE.raw());
    engine.clear_terminate();
    body(engine);
    let _ = task_tx.send(WorkerEvent::ScriptEnd);
    ui_marshal::post_wake(hwnd_ptr, SCRIPT_WAKE.raw());
}

impl MainWindow {
    /// エンジンスレッドからの [`HostApi`] 要求を UI スレッドで処理する。
    pub(crate) fn drain_script_requests(&self) {
        loop {
            let item = self.script.queue.lock().unwrap().pop_front();
            let Some((req, tx)) = item else { break };
            match req {
                HostCall::GetLog => {
                    // 先にログレーンを汲み切ってからスナップショットを返す（スクリプト自身の
                    // 直前の log/update を確実に読み戻す）。
                    self.drain_log_events();
                    let _ = tx.send(HostResp::LogText(self.log.text()));
                }
                HostCall::ConfigGet(key) => {
                    let value = serde_json::to_value(&*self.config.borrow()).ok();
                    let found = value.as_ref().and_then(|v| script::config_lookup(v, &key));
                    let _ = tx.send(HostResp::Json(found));
                }
                HostCall::ChangeOpposite { kind, path } => {
                    // 反対ペイン＝アクティブの逆側（is_left = active_right）。
                    let opposite = self.active_right.get();
                    let _ = match kind.as_str() {
                        "parent" => self.to_parent(opposite),
                        "root" => self.to_root(opposite),
                        _ => self.change_directory(opposite, Some(&path)),
                    };
                    let _ = tx.send(HostResp::Done);
                }
                HostCall::SetPathMask(mask) => {
                    let is_left = !self.active_right.get();
                    let m = mask.trim();
                    *self.mask(is_left).borrow_mut() =
                        if m.is_empty() || m == "*" { None } else { Some(m.to_owned()) };
                    // マスクはファイルセットを変えず表示を絞るだけなので、カーソルはファイル名で
                    // 保つ（解除でカーソル下ファイルが戻れば同じファイルに残る）。
                    let _ = self.reload_side_impl(is_left, crate::ReloadCursor::Keep);
                    let _ = tx.send(HostResp::Done);
                }
                HostCall::CreateDirectory(name) => {
                    let r = self.script_create_directory(&name).map(serde_json::Value::String);
                    let _ = tx.send(HostResp::CommandResult(r));
                }
                HostCall::Compress { kind, archive, files } => {
                    let r = self
                        .script_compress(&kind, &archive, &files)
                        .map(|()| serde_json::Value::Null);
                    let _ = tx.send(HostResp::CommandResult(r));
                }
                HostCall::CurrentDir => {
                    let is_left = !self.active_right.get();
                    let dir = self
                        .pane(is_left)
                        .borrow()
                        .as_real_path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let _ = tx.send(HostResp::Dir(dir));
                }
                HostCall::Navigate(path) => {
                    let is_left = !self.active_right.get();
                    let _ = self.change_directory(is_left, Some(&path));
                    let _ = tx.send(HostResp::Done);
                }
                HostCall::Confirm(message) => {
                    let result = message_box(&self.wnd, "確認", &message, MessageStyle::YesNo);
                    let _ = tx.send(HostResp::Bool(result == MessageResult::Yes));
                }
                HostCall::Prompt { message, default, select_all } => {
                    let text = if select_all {
                        input_box_select(
                            &self.wnd,
                            "入力",
                            &message,
                            &default,
                            InputMode::Plain,
                            InputSelect::All,
                        )
                    } else {
                        input_box(&self.wnd, "入力", &message, &default, InputMode::Plain)
                    };
                    let _ = tx.send(HostResp::Text(text));
                }
                HostCall::Select { title, items } => {
                    let index = list_box(&self.wnd, &title, "script_select", &items, 0);
                    let _ = tx.send(HostResp::Index(index));
                }
                HostCall::PaneSnapshot { opposite } => {
                    let active_left = !self.active_right.get();
                    let is_left = if opposite { !active_left } else { active_left };
                    let _ = tx.send(HostResp::Snapshot(self.build_pane_snapshot(is_left)));
                }
                HostCall::SetSelected {
                    is_left,
                    index,
                    selected,
                } => {
                    self.apply_pane_selection(is_left, &[(index, selected)]);
                    let _ = tx.send(HostResp::Done);
                }
                HostCall::ApplySelection { is_left, changes } => {
                    self.apply_pane_selection(is_left, &changes);
                    let _ = tx.send(HostResp::Done);
                }
                HostCall::Command { name, args } => {
                    let _ = tx.send(HostResp::CommandResult(self.run_script_command(&name, args)));
                }
                HostCall::RenameFiles { pairs } => {
                    let summary = self.rename_files_with_conflict(pairs);
                    let v = serde_json::to_value(summary).unwrap_or(serde_json::Value::Null);
                    let _ = tx.send(HostResp::CommandResult(Ok(v)));
                }
                HostCall::BeginOperation {
                    op,
                    items,
                    dest,
                    events,
                } => {
                    let r = self.begin_script_operation(op, items, dest, events);
                    let _ = tx.send(HostResp::OpStarted(r));
                }
                HostCall::CancelOperation { token } => {
                    self.cancel_script_operation(token);
                    let _ = tx.send(HostResp::Done);
                }
                HostCall::ShellOpen(path) => {
                    if let Err(e) = self.wnd.hwnd().ShellExecute(
                        "open",
                        &path,
                        None,
                        None,
                        co::SW::SHOWNORMAL,
                    ) {
                        self.log.error(&format!("開けません: {path}: {e}"));
                    }
                    let _ = tx.send(HostResp::Done);
                }
                HostCall::FolderDialog(title) => {
                    let picked = shell::choose_folder(self.wnd.hwnd().ptr(), &title)
                        .map(|p| p.to_string_lossy().into_owned());
                    let _ = tx.send(HostResp::Text(picked));
                }
                HostCall::OpenDialog(title) => {
                    let picked = shell::choose_file(self.wnd.hwnd().ptr(), &title, false)
                        .map(|p| p.to_string_lossy().into_owned());
                    let _ = tx.send(HostResp::Text(picked));
                }
                HostCall::SaveDialog(title) => {
                    let picked = shell::choose_file(self.wnd.hwnd().ptr(), &title, true)
                        .map(|p| p.to_string_lossy().into_owned());
                    let _ = tx.send(HostResp::Text(picked));
                }
                HostCall::SetClipboard(text) => {
                    if let Err(e) = shell::clip_set_text(self.wnd.hwnd(), &text) {
                        self.log.error(&e);
                    }
                    let _ = tx.send(HostResp::Done);
                }
                HostCall::GetClipboard => {
                    let text = match shell::clip_get_text(self.wnd.hwnd()) {
                        Ok(text) => text,
                        Err(e) => {
                            self.log.error(&e);
                            None
                        }
                    };
                    let _ = tx.send(HostResp::Text(text));
                }
                HostCall::SetClipboardImage(path) => {
                    let ok = match shell::clip_set_image(self.wnd.hwnd(), std::path::Path::new(&path)) {
                        Ok(()) => true,
                        Err(e) => {
                            self.log.error(&e);
                            false
                        }
                    };
                    let _ = tx.send(HostResp::Bool(ok));
                }
                HostCall::GetClipboardImage(dest) => {
                    let ok = match shell::clip_get_image(self.wnd.hwnd(), std::path::Path::new(&dest)) {
                        Ok(saved) => saved,
                        Err(e) => {
                            self.log.error(&e);
                            false
                        }
                    };
                    let _ = tx.send(HostResp::Bool(ok));
                }
            }
        }
    }

    /// トークンの job が束ねる全タスクを中止する（各 `TaskControl` を停止）。
    fn cancel_script_operation(&self, token: u64) {
        let task_ids: Vec<u64> = self
            .script
            .jobs
            .borrow()
            .jobs
            .get(&token)
            .map(|g| g.task_ids.clone())
            .unwrap_or_default();
        let tasks = self.tasks.borrow();
        for id in task_ids {
            if let Some(t) = tasks.iter().find(|t| t.id == id) {
                t.control.stop();
            }
        }
    }

    /// スクリプト発の非同期ファイル操作を起動する。対象＝アクティブペインの選択（無ければ
    /// カーソル）、行き先＝反対ペイン。ワーカーを起こせたらタスク id に `done` を紐づけ、完了で
    /// 発火する。起動できなければ（対象なし・書庫・起動失敗）即座に `done` へエラーを返す。
    fn begin_script_operation(
        &self,
        op: ScriptOp,
        items: Vec<String>,
        dest: String,
        events: OpDone,
    ) -> Result<u64, String> {
        let is_left = !self.active_right.get();
        let needs_dst = !matches!(op, ScriptOp::Delete);
        // 対象＝(src_dir, names) のグループ。選択ベースはアクティブペイン 1 グループ、明示ベースは
        // 与えられたパスを親ディレクトリごとにまとめる。
        let (groups, dst_dir) = if items.is_empty() {
            if self.pane(is_left).borrow().is_archive() {
                return Err("書庫の操作は未対応です".to_string());
            }
            let names = self.selected_or_cursor_names(is_left);
            if names.is_empty() {
                return Err("対象がありません".to_string());
            }
            let src = self.pane(is_left).borrow().path().to_path_buf();
            let dst = self.pane(!is_left).borrow().path().to_path_buf();
            (vec![(src, names)], dst)
        } else {
            let mut by_dir: HashMap<std::path::PathBuf, Vec<String>> = HashMap::new();
            for item in &items {
                let p = std::path::Path::new(item);
                if let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) {
                    let parent = p.parent().map(|x| x.to_path_buf()).unwrap_or_default();
                    by_dir.entry(parent).or_default().push(name);
                }
            }
            if by_dir.is_empty() {
                return Err("対象がありません".to_string());
            }
            let dst = std::path::PathBuf::from(&dest);
            if needs_dst && dest.is_empty() {
                return Err("行き先が指定されていません".to_string());
            }
            (by_dir.into_iter().collect(), dst)
        };

        // 各グループでワーカーを起こす。起動できた id を束ねて 1 job にする。
        let mut task_ids = Vec::new();
        for (src_dir, names) in groups {
            let started = match op {
                ScriptOp::Delete => self.start_delete(src_dir, names),
                ScriptOp::Copy | ScriptOp::Move => {
                    self.start_copy(src_dir, dst_dir.clone(), names, matches!(op, ScriptOp::Move))
                }
            };
            match started {
                Ok(id) => task_ids.push(id),
                Err(e) => self.log.error(&format!("操作を起動できません: {e}")),
            }
        }
        let Some(&token) = task_ids.first() else {
            return Err("操作を起動できませんでした".to_string());
        };
        let mut reg = self.script.jobs.borrow_mut();
        for &id in &task_ids {
            reg.task_to_token.insert(id, token);
        }
        reg.jobs.insert(
            token,
            JobGroup {
                remaining: task_ids.len(),
                events,
                cancelled: false,
                failed: false,
                task_ids,
            },
        );
        Ok(token)
    }

    /// ワーカー完了（`WorkerEvent::Done`）を受けたとき、その id が属する job のカウントを減らし、
    /// 中止/失敗を集約する。全タスクが揃ったら完了イベントを 1 度流す（成功なら `await` を解き、
    /// 中止/失敗なら例外にする）。スクリプト発でない id は無視される。
    pub(crate) fn notify_script_op_done(&self, id: u64, cancelled: bool, failed: bool) {
        let mut reg = self.script.jobs.borrow_mut();
        let Some(token) = reg.task_to_token.remove(&id) else {
            return;
        };
        let Some(group) = reg.jobs.get_mut(&token) else {
            return;
        };
        group.cancelled |= cancelled;
        group.failed |= failed;
        group.remaining = group.remaining.saturating_sub(1);
        if group.remaining > 0 {
            return;
        }
        // 全タスク完了。job を取り出して完了イベントを 1 度流す。
        let group = reg.jobs.remove(&token).expect("token just resolved exists");
        drop(reg);
        let err = if group.cancelled {
            Some("操作は中止されました".to_string())
        } else if group.failed {
            Some("操作に失敗しました".to_string())
        } else {
            None
        };
        let _ = group.events.send(script::JobEvent::done(err));
    }

    /// ワーカー進捗（`WorkerEvent::Progress`）を、その task_id が属する job の購読側へ流す。
    /// スクリプト発でない task_id（メニュー操作など）は無視される。
    pub(crate) fn notify_script_op_progress(&self, task_id: u64, text: &str) {
        let reg = self.script.jobs.borrow();
        let Some(&token) = reg.task_to_token.get(&task_id) else {
            return;
        };
        if let Some(group) = reg.jobs.get(&token) {
            let _ = group.events.send(script::JobEvent::progress(text.to_string()));
        }
    }

    /// スクリプトからの内蔵コマンド要求を実行する。名前を解決し、アクティブペイン文脈で
    /// `exec` する。不明な名前・実行失敗はエラー文字列にする。モーダルを開くコマンドは
    /// ネストループが SCRIPT_WAKE を回し続けるのでデッドロックしない。
    fn run_script_command(&self, name: &str, args: Vec<String>) -> Result<serde_json::Value, String> {
        let Some(cmd) = Command::from_token(name) else {
            return Err(format!("unknown command: {name}"));
        };
        let is_left = !self.active_right.get();
        // 値返しクエリ＝副作用なしの状態読み取り。アクション実行をせず値だけ返す。
        if let Some(value) = self.query_value(is_left, cmd) {
            return Ok(value);
        }
        let call = Call::Builtin {
            command: cmd,
            args: args.into_iter().map(serde_json::Value::String).collect(),
        };
        // スクリプト発のコマンド実行中は executeCommand を抑止する（無限再帰を防ぐ）。モーダルの
        // 入れ子メッセージループで並列ワーカーのコマンドが再入し得るので、クリアでなく退避/復元
        // にして、入れ子の終了が外側の抑止状態を落とさないようにする。
        let prev = self.script.suppress_events.replace(true);
        let result = self.exec(is_left, &call);
        self.script.suppress_events.set(prev);
        // アクション系コマンドは値を返さない（`undefined` 相当の null）。
        result.map(|()| serde_json::Value::Null).map_err(|e| e.to_string())
    }

    /// ペインの一覧読込が完了したとき呼ぶ。前回撃った現在地と違えば changeDirectory を配る
    /// ＝実際にディレクトリが変わったときだけ（在席再読込・F5・操作後の Focus では撃たない）。
    pub(crate) fn notify_dir_loaded(&self, is_left: bool, dir: &str) {
        let idx = if is_left { 0 } else { 1 };
        let changed = {
            let mut last = self.script.last_dir.borrow_mut();
            if last[idx] != dir {
                last[idx] = dir.to_string();
                true
            } else {
                false
            }
        };
        if changed {
            self.fire_script_event("changeDirectory", dir);
        }
    }

    /// ファイラー本体の出来事を `rerics.on` ハンドラへ届ける（投げっぱなし）。スクリプト発の
    /// コマンド実行中（`suppress_events`）は撃たない＝ハンドラからの自己再帰を断つ。
    pub(crate) fn fire_script_event(&self, event: &str, arg: &str) {
        if self.script.suppress_events.get() {
            return;
        }
        let _ = self.script.cmd_tx.send(EngineCmd::FireEvent {
            event: event.to_string(),
            arg: arg.to_string(),
        });
    }

    /// 指定側ペインの選択状態を書き戻し、まとめて 1 回だけ再描画する。範囲外 index は無視する。
    fn apply_pane_selection(&self, is_left: bool, changes: &[(usize, bool)]) {
        {
            let state = self.view(is_left).state();
            let mut s = state.borrow_mut();
            for &(index, selected) in changes {
                if let Some(it) = s.items.get_mut(index) {
                    it.selected = selected;
                }
            }
        }
        let _ = self.view(is_left).refresh();
    }

    /// 指定側ペインの現在状態をスナップショットに写す（現在地は `Pane`・項目と選択は
    /// 表示中の `FileListState` から）。スクリプトの `activePane()`/`oppositePane()` の実体。
    fn build_pane_snapshot(&self, is_left: bool) -> PaneSnapshot {
        let (dir, is_archive) = {
            let pane = self.pane(is_left).borrow();
            (pane.loc_display(), pane.is_archive())
        };
        let state = self.view(is_left).state();
        let s = state.borrow();
        let items = s
            .items
            .iter()
            .enumerate()
            .map(|(index, it)| PaneItem {
                index,
                full_name: std::path::Path::new(&dir)
                    .join(&it.name)
                    .to_string_lossy()
                    .into_owned(),
                name: it.name.clone(),
                base_name: it.base_name.clone(),
                // スクリプト API はドット無しに統一する（コアはドット付きで持つ）。
                ext: it.extension.trim_start_matches('.').to_string(),
                is_dir: it.is_dir,
                is_parent: it.is_parent,
                size: it.size.unwrap_or(0),
                mtime: system_time_ms(it.modified),
                ctime: system_time_ms(it.created),
                atime: system_time_ms(it.accessed),
                selected: it.selected,
                readonly: it.readonly,
                hidden: it.hidden,
                system: it.system,
                archive: it.archive,
                reparse: it.reparse,
                link: it.link_kind().as_token(),
                link_target: it.link_target.clone(),
                // 書庫など仮想ペインの項目はすべて仮想扱い。
                is_virtual: is_archive,
            })
            .collect();
        PaneSnapshot {
            dir,
            is_archive,
            is_left,
            cursor: s.cursor,
            items,
            sort_type: s.sort_type.as_token().to_string(),
            sort_reverse: s.sort_reverse,
            path_mask: self.mask(is_left).borrow().clone().unwrap_or_default(),
        }
    }

    /// `wm_create` で hwnd 確定後にエンジンスレッドを起動する（最初の 1 度だけ）。
    pub(crate) fn start_script_engine(&self) {
        if let Some(cmd_rx) = self.script.take_rx() {
            let hwnd_ptr = self.wnd.hwnd().ptr() as isize;
            spawn_engine(
                self.script.queue.clone(),
                hwnd_ptr,
                cmd_rx,
                self.task_tx.clone(),
                self.log_tx.clone(),
                self.script_worker_isolates.clone(),
                self.progress_seq.clone(),
                self.script_pool_stopped.clone(),
            );
        }
    }

    /// スクリプトのログレーンを汲み切ってログ窓へ反映する。タスクタイマ取り込みと `getLog` の
    /// 双方から呼ばれる（どちらも UI スレッド）。
    pub(crate) fn drain_log_events(&self) {
        while let Ok(ev) = self.log_rx.try_recv() {
            match ev {
                LogEvent::Line { id, level, text } => self.log.push_with_id(id, level, &text),
                LogEvent::Update { id, level, text } => self.log.update(id, level, &text),
                LogEvent::ProgressStart { id } => {
                    self.script_progress.borrow_mut().insert(id);
                    self.log.start_progress(id);
                }
                LogEvent::ProgressSet { id, done, total } => self.log.set_progress(id, done, total),
                LogEvent::ProgressStop { id } => {
                    self.script_progress.borrow_mut().remove(&id);
                    self.log.stop_progress(id);
                }
            }
        }
    }

    /// エンジンスレッドへコマンドを投げる（投げっぱなし）。
    pub(crate) fn script_send(&self, cmd: EngineCmd) {
        let _ = self.script.cmd_tx.send(cmd);
    }

    /// エンジンからの同期応答を、待つあいだも [`HostApi`] マーシャルキューを汲みながら受け取る。
    /// こうしないと、先行スクリプトが `HostApi` 往復で UI を待つ間に UI が応答待ちで固まる
    /// （head-of-line デッドロック）。応答が来るまでキューを汲みつつ短間隔でポーリングする。
    fn recv_pumping<T>(&self, rx: &Receiver<T>) -> Option<T> {
        use std::sync::mpsc::RecvTimeoutError;
        loop {
            // 先に溜まっている HostApi 要求を捌いてエンジンを進ませる。
            self.drain_script_requests();
            match rx.recv_timeout(std::time::Duration::from_millis(5)) {
                Ok(v) => return Some(v),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// 登録済みコマンドのメタ情報をエンジンから同期取得する。
    pub(crate) fn script_list_commands(&self) -> Vec<ScriptCommand> {
        let (tx, rx) = channel();
        let _ = self.script.cmd_tx.send(EngineCmd::ListCommands(tx));
        self.recv_pumping(&rx).unwrap_or_default()
    }

    /// スクリプトが `registerMenu` で登録した名前付きメニュー定義をエンジンから同期取得する。
    pub(crate) fn script_list_menus(&self) -> Vec<rerics_core::MenuDef> {
        let (tx, rx) = channel();
        let _ = self.script.cmd_tx.send(EngineCmd::ListMenus(tx));
        self.recv_pumping(&rx).unwrap_or_default()
    }

    /// `r.` で呼べるメンバー（補完候補）をエンジンから同期取得する。引数/コード欄の補完に使う。
    pub(crate) fn script_list_members(&self) -> Vec<MemberInfo> {
        let (tx, rx) = channel();
        let _ = self.script.cmd_tx.send(EngineCmd::ListMembers(tx));
        self.recv_pumping(&rx).unwrap_or_default()
    }

    /// `globalThis` に実在する名前の一覧をエンジンから同期取得する。式エディタの
    /// 未解決識別子チェックの許容リストに使う。
    pub(crate) fn script_list_globals(&self) -> Vec<String> {
        let (tx, rx) = channel();
        let _ = self.script.cmd_tx.send(EngineCmd::ListGlobals(tx));
        self.recv_pumping(&rx).unwrap_or_default()
    }

    /// コードを評価して最後の式の値を同期取得する（値返し Eval の検証口）。評価コードが
    /// `HostApi` を呼んでもデッドロックしないよう、待機中はマーシャルキューを汲む。
    #[cfg(feature = "debug-server")]
    pub(crate) fn script_eval_value(&self, code: String) -> String {
        let (tx, rx) = channel();
        let _ = self.script.cmd_tx.send(EngineCmd::EvalValue { code, tx });
        self.recv_pumping(&rx).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str, summary: Option<&str>) -> ScriptCommand {
        ScriptCommand {
            name: name.to_string(),
            label: None,
            genre: None,
            summary: summary.map(str::to_string),
        }
    }

    #[test]
    fn script_commands_dts_declares_registered_skips_reserved_and_quotes_nonident() {
        let metas = vec![
            cmd("actionEnter", Some("書庫は展開・他は実行")),
            cmd("cursorDown", None),        // 組込 token＝除外
            cmd("my-cmd", None),            // 識別子不可＝引用キー
            cmd("plain", None),
        ];
        let dts = script_commands_dts(&metas);
        assert!(dts.contains("interface RericsCommands {"), "{dts}");
        assert!(dts.contains("actionEnter(...args: unknown[]): unknown;"), "{dts}");
        assert!(dts.contains("/** 書庫は展開・他は実行 */"), "summary を JSDoc 化: {dts}");
        assert!(dts.contains("plain(...args: unknown[]): unknown;"), "{dts}");
        // 組込と同名は r に生えないので出さない。
        assert!(!dts.contains("cursorDown("), "組込 token は除外: {dts}");
        // 識別子にできない名前は引用キー。
        assert!(dts.contains("\"my-cmd\"(...args: unknown[]): unknown;"), "引用キー: {dts}");
    }
}
