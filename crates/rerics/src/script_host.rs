//! スクリプトエンジンと GUI をつなぐ配線。
//!
//! エンジンは別スレッドで V8 を回す。GUI 状態に触れるのは UI スレッドだけなので、
//! エンジン→UI の操作（[`HostApi`]）は [`ui_marshal`] でマーシャルする。逆方向（UI→
//! エンジンへの「このコマンドを実行」要求）は [`EngineCmd`] チャネルで送る。エンジンが
//! `HostApi` 経由で UI を待つ間に UI がエンジンを待つとデッドロックするため、UI→エンジンの
//! コマンドは投げっぱなし（完了を待たない）。一覧取得だけは `HostApi` を呼ばないので同期で待てる。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

use winsafe::prelude::*;

use crate::MainWindow;
use crate::dialog::{InputMode, MessageResult, MessageStyle, input_box, list_box, message_box};
use rerics_core::{Command, Invocation};

use crate::script::{self, HostApi, PaneItem, PaneSnapshot, ScriptOp};
use crate::ui_marshal::{self, WakeQueue};
use crate::winutil::msg::SCRIPT_WAKE;

/// エンジンスレッド → UI スレッドへの要求（[`HostApi`] の各操作）。
pub enum HostCall {
    Log(String),
    CurrentDir,
    Navigate(String),
    Confirm(String),
    Prompt { message: String, default: String },
    Select { title: String, items: Vec<String> },
    PaneSnapshot { opposite: bool },
    SetSelected { is_left: bool, index: usize, selected: bool },
    ApplySelection { is_left: bool, changes: Vec<(usize, bool)> },
    Command { name: String, args: Vec<String> },
    BeginOperation {
        op: ScriptOp,
        items: Vec<String>,
        dest: String,
        events: OpDone,
    },
    CancelOperation { token: u64 },
}

/// UI スレッド → エンジンスレッドへの応答。
pub enum HostResp {
    Done,
    Dir(String),
    Bool(bool),
    Text(Option<String>),
    Index(Option<usize>),
    Snapshot(script::PaneSnapshot),
    CommandResult(Result<(), String>),
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
/// scripting 単独ビルドでは送り手（debug-server エンドポイント／将来のキーバインド）が
/// まだ無いため未使用＝その構成でのみ dead_code を許容する。
#[cfg_attr(not(feature = "debug-server"), allow(dead_code))]
pub enum EngineCmd {
    /// 登録済みコマンドを名前で実行する（投げっぱなし）。
    Invoke(String),
    /// TS/JS ソースを評価する（投げっぱなし）。
    Eval(String),
    /// 現在登録されているコマンド名を返す（同期・`HostApi` を呼ばないのでデッドロックしない）。
    ListCommands(Sender<Vec<String>>),
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
}

impl HostApi for GuiHost {
    fn log(&self, msg: &str) {
        let _ = ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::Log(msg.to_string()),
        );
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

    fn prompt(&self, message: &str, default: &str) -> Option<String> {
        match ui_marshal::call(
            &self.queue,
            self.hwnd_ptr,
            SCRIPT_WAKE.raw(),
            HostCall::Prompt {
                message: message.to_string(),
                default: default.to_string(),
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

    fn command(&self, name: &str, args: &[String]) -> Result<(), String> {
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
}

/// スクリプトエンジンを別スレッドに建てる。起動スクリプト（`data_dir()/scripts`）を読み込み、
/// 以後 [`EngineCmd`] を受けて捌くループに入る。`hwnd_ptr` は UI スレッドを起こす先。
pub fn spawn_engine(queue: ScriptQueue, hwnd_ptr: isize, cmd_rx: Receiver<EngineCmd>) {
    std::thread::spawn(move || {
        let host: Rc<dyn HostApi> = Rc::new(GuiHost { queue, hwnd_ptr });
        let mut engine = script::Engine::new(host.clone());
        let scripts = rerics_core::data_dir().join("scripts");
        for (path, msg) in script::load_dir(&mut engine, &scripts) {
            host.log(&format!("スクリプト読込エラー [{}]: {}", path.display(), msg));
        }
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                EngineCmd::Invoke(name) => {
                    if let Err(e) = engine.invoke_command(&name) {
                        host.log(&format!("コマンド実行エラー [{name}]: {e}"));
                    }
                }
                EngineCmd::Eval(code) => {
                    if let Err(e) = engine.run_ts(
                        "rerics:eval",
                        "file:///eval.ts",
                        deno_ast::MediaType::TypeScript,
                        code,
                    ) {
                        host.log(&format!("eval エラー: {e}"));
                    }
                }
                EngineCmd::ListCommands(tx) => {
                    let _ = tx.send(engine.registered_commands());
                }
                EngineCmd::FireEvent { event, arg } => {
                    if let Err(e) = engine.fire_event(&event, &arg) {
                        host.log(&format!("イベント発火エラー [{event}]: {e}"));
                    }
                }
            }
        }
    });
}

impl MainWindow {
    /// エンジンスレッドからの [`HostApi`] 要求を UI スレッドで処理する。
    pub(crate) fn drain_script_requests(&self) {
        loop {
            let item = self.script.queue.lock().unwrap().pop_front();
            let Some((req, tx)) = item else { break };
            match req {
                HostCall::Log(msg) => {
                    self.log.info(&msg);
                    let _ = tx.send(HostResp::Done);
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
                HostCall::Prompt { message, default } => {
                    let text = input_box(&self.wnd, "入力", &message, &default, InputMode::Plain);
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
    fn run_script_command(&self, name: &str, args: Vec<String>) -> Result<(), String> {
        let Some(cmd) = Command::from_token(name) else {
            return Err(format!("unknown command: {name}"));
        };
        let inv = Invocation::new(cmd, args);
        let is_left = !self.active_right.get();
        // スクリプト発のコマンド実行中は executeCommand を抑止する（無限再帰を防ぐ）。
        self.script.suppress_events.set(true);
        let result = self.exec(is_left, &inv);
        self.script.suppress_events.set(false);
        result.map_err(|e| e.to_string())
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
                selected: it.selected,
                readonly: it.readonly,
                hidden: it.hidden,
            })
            .collect();
        PaneSnapshot {
            dir,
            is_archive,
            is_left,
            cursor: s.cursor,
            items,
        }
    }

    /// `wm_create` で hwnd 確定後にエンジンスレッドを起動する（最初の 1 度だけ）。
    pub(crate) fn start_script_engine(&self) {
        if let Some(cmd_rx) = self.script.take_rx() {
            let hwnd_ptr = self.wnd.hwnd().ptr() as isize;
            spawn_engine(self.script.queue.clone(), hwnd_ptr, cmd_rx);
        }
    }

    /// エンジンスレッドへコマンドを投げる（投げっぱなし）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn script_send(&self, cmd: EngineCmd) {
        let _ = self.script.cmd_tx.send(cmd);
    }

    /// 登録済みコマンド名をエンジンから同期取得する。
    #[cfg(feature = "debug-server")]
    pub(crate) fn script_list_commands(&self) -> Vec<String> {
        let (tx, rx) = channel();
        let _ = self.script.cmd_tx.send(EngineCmd::ListCommands(tx));
        rx.recv().unwrap_or_default()
    }
}
