//! バックグラウンド・ファイル操作の橋渡し。ワーカースレッドと UI スレッドの境界。
//!
//! 操作ロジック本体は [`rerics_core::operation`] 側にあり、ここはワーカー → UI の
//! イベント型と、[`rerics_core::OperationHost`] の GUI 実装（チャネル送信）を担う。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use rerics_core::{LogLevel, OperationHost};

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

/// ワーカースレッドから UI スレッドへ送るイベント。
pub enum WorkerEvent {
    /// ログ1行を追記する。
    Log { level: LogLevel, text: String },
    /// 操作完了。関与したディレクトリを伴う（再読込・選択解除の判定に使う）。
    Done {
        kind: OpKind,
        src_dir: PathBuf,
        dst_dir: PathBuf,
    },
}

/// [`OperationHost`] の GUI 実装。ログをチャネルへ送り、共有フラグで中止を伝える。
pub struct ChannelHost {
    pub tx: Sender<WorkerEvent>,
    pub shutdown: Arc<AtomicBool>,
}

impl OperationHost for ChannelHost {
    fn log(&self, level: LogLevel, text: &str) {
        let _ = self.tx.send(WorkerEvent::Log { level, text: text.to_owned() });
    }

    fn cancelled(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}
