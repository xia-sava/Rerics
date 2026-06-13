//! バックグラウンド・ファイル操作の橋渡し。ワーカースレッドと UI スレッドの境界。
//!
//! 操作ロジック本体は [`rerics_core::operation`] 側にあり、ここはワーカー → UI の
//! イベント型と、[`rerics_core::OperationHost`] の GUI 実装（チャネル送信）を担う。

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use rerics_core::{ConflictResolution, LogLevel, OperationHost};

/// イベント取り込みタイマの ID。
pub const TASK_TIMER_ID: usize = 1;
/// 取り込みタイマの間隔（ミリ秒）。
pub const TASK_TIMER_MS: u32 = 50;

/// 操作の種別。完了時の再読込・選択解除の出し分けに使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Copy,
    Move,
    Delete,
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
    /// 操作完了。関与したディレクトリを伴う（再読込・選択解除の判定に使う）。
    Done {
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
    pub conflict_cache: RefCell<Option<ConflictResolution>>,
}

impl ChannelHost {
    pub fn new(tx: Sender<WorkerEvent>, shutdown: Arc<AtomicBool>) -> Self {
        Self { tx, shutdown, conflict_cache: RefCell::new(None) }
    }
}

impl OperationHost for ChannelHost {
    fn log(&self, level: LogLevel, text: &str) {
        let _ = self.tx.send(WorkerEvent::Log { level, text: text.to_owned() });
    }

    fn cancelled(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
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
}
