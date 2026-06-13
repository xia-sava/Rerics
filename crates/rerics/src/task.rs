//! バックグラウンド・ファイル操作の橋渡し。ワーカースレッドと UI スレッドの境界。
//!
//! 操作ロジック本体は [`rerics_core::operation`] 側にあり、ここはワーカー → UI の
//! イベント型と、[`rerics_core::OperationHost`] の GUI 実装（チャネル送信）を担う。

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use rerics_core::{ConflictResolution, DeleteWarnChoice, LogLevel, OperationHost};

use crate::dialog::MessageResult;

/// イベント取り込みタイマの ID。
pub const TASK_TIMER_ID: usize = 1;
/// 取り込みタイマの間隔（ミリ秒）。
pub const TASK_TIMER_MS: u32 = 50;

const TASK_RUNNING: u8 = 0;
const TASK_STOP: u8 = 1;
const TASK_SUSPEND: u8 = 2;

/// 操作の種別。完了時の再読込・選択解除の出し分けに使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Copy,
    Move,
    Delete,
}

/// ワーカーと UI（タスクマネージャ）で共有する1タスクの制御状態。
pub struct TaskControl {
    state: AtomicU8,
}

impl TaskControl {
    pub fn new() -> Self {
        Self { state: AtomicU8::new(TASK_RUNNING) }
    }

    /// 中止を要求する（ワーカーが次のファイル境界で気付く。中断中からも中止できる）。
    pub fn stop(&self) {
        self.state.store(TASK_STOP, Ordering::Relaxed);
    }

    /// 中断を要求する（実行中のときのみ）。
    pub fn suspend(&self) {
        let _ = self.state.compare_exchange(
            TASK_RUNNING,
            TASK_SUSPEND,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// 再開する（中断中のときのみ）。
    pub fn resume(&self) {
        let _ = self.state.compare_exchange(
            TASK_SUSPEND,
            TASK_RUNNING,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    pub fn is_stopped(&self) -> bool {
        self.state.load(Ordering::Relaxed) == TASK_STOP
    }

    pub fn is_suspended(&self) -> bool {
        self.state.load(Ordering::Relaxed) == TASK_SUSPEND
    }

    /// タスクマネージャ表示用の状態ラベル。
    pub fn state_label(&self) -> &'static str {
        match self.state.load(Ordering::Relaxed) {
            TASK_STOP => "中止",
            TASK_SUSPEND => "中断",
            _ => "実行中",
        }
    }
}

impl Default for TaskControl {
    fn default() -> Self {
        Self::new()
    }
}

/// 走行中タスク1件の表示・制御情報（タスクマネージャが参照する）。
pub struct TaskEntry {
    pub id: u64,
    /// タスク種別名（コピー/移動/削除）。
    pub text: String,
    /// 詳細（"先頭名他 -> 宛先" など）。
    pub description: String,
    pub control: Arc<TaskControl>,
    pub start: Instant,
}

/// 衝突ダイアログの回答（解決方法＋「すべてに適用」）。
pub struct ConflictReply {
    pub choice: ConflictResolution,
    pub all: bool,
}

/// ワーカースレッドから UI スレッドへ送るイベント。
pub enum WorkerEvent {
    /// ログ1行を追記する。
    Log { level: LogLevel, text: String },
    /// 同名衝突の解決を UI に問い合わせる（回答を `reply` で受け取る）。
    AskConflict { name: String, reply: Sender<ConflictReply> },
    /// 属性付きファイルの削除可否を UI に問い合わせる。
    AskDeleteWarn { name: String, attr: String, reply: Sender<MessageResult> },
    /// 操作完了。タスク id と関与したディレクトリを伴う（除去・再読込の判定に使う）。
    Done {
        id: u64,
        kind: OpKind,
        src_dir: PathBuf,
        dst_dir: PathBuf,
    },
}

/// [`OperationHost`] の GUI 実装。ログをチャネルへ送り、共有フラグで中止を伝え、
/// 衝突は UI へ往復で問い合わせる。「すべてに適用」の選択はここでキャッシュする。
pub struct ChannelHost {
    pub tx: Sender<WorkerEvent>,
    pub shutdown: Arc<AtomicBool>,
    pub control: Arc<TaskControl>,
    pub conflict_cache: RefCell<Option<ConflictResolution>>,
    pub delete_warn_cache: RefCell<Option<DeleteWarnChoice>>,
}

impl ChannelHost {
    pub fn new(tx: Sender<WorkerEvent>, shutdown: Arc<AtomicBool>, control: Arc<TaskControl>) -> Self {
        Self {
            tx,
            shutdown,
            control,
            conflict_cache: RefCell::new(None),
            delete_warn_cache: RefCell::new(None),
        }
    }
}

impl OperationHost for ChannelHost {
    fn log(&self, level: LogLevel, text: &str) {
        let _ = self.tx.send(WorkerEvent::Log { level, text: text.to_owned() });
    }

    fn cancelled(&self) -> bool {
        self.control.is_stopped() || self.shutdown.load(Ordering::Relaxed)
    }

    fn wait_while_suspended(&self) {
        while self.control.is_suspended() && !self.shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn resolve_conflict(&self, name: &str) -> ConflictResolution {
        if let Some(c) = self.conflict_cache.borrow().clone() {
            return c;
        }
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if self
            .tx
            .send(WorkerEvent::AskConflict { name: name.to_owned(), reply: reply_tx })
            .is_err()
        {
            return ConflictResolution::Cancel;
        }
        match reply_rx.recv() {
            Ok(reply) => {
                // Rename は各ファイルで別名が要るのでキャッシュしない（原作も all と排他）。
                let cacheable = !matches!(
                    reply.choice,
                    ConflictResolution::Cancel | ConflictResolution::Rename(_)
                );
                if reply.all && cacheable {
                    *self.conflict_cache.borrow_mut() = Some(reply.choice.clone());
                }
                reply.choice
            }
            Err(_) => ConflictResolution::Cancel,
        }
    }

    fn confirm_delete_attr(&self, name: &str, attr: &str) -> DeleteWarnChoice {
        if let Some(c) = *self.delete_warn_cache.borrow() {
            return c;
        }
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if self
            .tx
            .send(WorkerEvent::AskDeleteWarn {
                name: name.to_owned(),
                attr: attr.to_owned(),
                reply: reply_tx,
            })
            .is_err()
        {
            return DeleteWarnChoice::Cancel;
        }
        match reply_rx.recv() {
            Ok(MessageResult::YesAll) => {
                *self.delete_warn_cache.borrow_mut() = Some(DeleteWarnChoice::Yes);
                DeleteWarnChoice::Yes
            }
            Ok(MessageResult::NoAll) => {
                *self.delete_warn_cache.borrow_mut() = Some(DeleteWarnChoice::No);
                DeleteWarnChoice::No
            }
            Ok(MessageResult::Yes) => DeleteWarnChoice::Yes,
            Ok(MessageResult::No) => DeleteWarnChoice::No,
            _ => DeleteWarnChoice::Cancel,
        }
    }
}
