//! バックグラウンド・ファイル操作の橋渡し。ワーカースレッドと UI スレッドの境界。
//!
//! 操作ロジック本体は [`rerics_core::operation`] 側にあり、ここはワーカー → UI の
//! イベント型と、[`rerics_core::OperationHost`] の GUI 実装（チャネル送信）を担う。

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use rerics_core::{
    ConflictResolution, CopyOptions, DeleteWarnChoice, LogLevel, OperationHost, ProgressHandle,
};

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

/// タスクの種別。タスクマネージャでの制御の効き方を出し分ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// 通常のワーカータスク（コピー/移動/削除/検索/比較など）。中止・中断・再開すべて効く。
    Normal,
    /// スクリプト実行タスク。V8 の制約で中断/再開はできず、中止のみ（isolate の強制終了）。
    Script,
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
    /// 制御の効き方の区別（通常／スクリプト）。
    pub kind: TaskKind,
    /// この操作を開始したタブ。完了ログ（`DirInfoDone`/`ArchiveDone` など `ChannelHost` を
    /// 経由しない経路）を、切替後もそのタブのログへ届けるために覚える。
    pub origin_tab: u64,
}

/// 衝突ダイアログの回答（解決方法＋「すべてに適用」）。
pub struct ConflictReply {
    pub choice: ConflictResolution,
    pub all: bool,
}

/// 書庫一括展開（非ランダムアクセス書庫の読込）の結末。
pub enum ArchiveOutcome {
    /// 展開成功（temp_root に全エントリ展開済み）。
    Ok,
    /// 利用者が中断した（Esc／タスクマネージャ）。
    Cancelled,
    /// 展開に失敗した（理由文字列）。
    Failed(String),
}

/// ワーカースレッドから UI スレッドへ送るイベント。
pub enum WorkerEvent {
    /// ログ1行を追記する。`origin_tab` は操作を開始したタブ（切替後もそのタブへ届ける）。
    Log { level: LogLevel, text: String, origin_tab: u64 },
    /// インプレース更新できる `id` 付きの行を追記する（進捗行の開始）。行には進行表示
    /// （ぐるぐる）が付き、`LogEnd` まで回り続ける。`origin_tab` は操作を開始したタブ。
    LogLine { id: u64, level: LogLevel, text: String, origin_tab: u64 },
    /// `id` 付き行の本文を書き換える（進捗の更新）。`level` が `Some` のとき
    /// レベル（表示色）も差し替える（`None` は据え置き）。`origin_tab` は操作を開始したタブ。
    LogUpdate { id: u64, level: Option<LogLevel>, text: String, origin_tab: u64 },
    /// `id` 付き行を最終本文で確定させ、進行表示を止める。`origin_tab` は操作を開始したタブ。
    LogEnd { id: u64, level: Option<LogLevel>, text: String, origin_tab: u64 },
    /// 同名衝突の解決を UI に問い合わせる（回答を `reply` で受け取る）。
    AskConflict { name: String, reply: Sender<ConflictReply> },
    /// 属性付きファイルの削除可否を UI に問い合わせる。
    AskDeleteWarn { name: String, attr: String, reply: Sender<MessageResult> },
    /// 操作完了。タスク id と関与したディレクトリを伴う（除去・再読込の判定に使う）。
    /// `cancelled`/`failed` は結末（[`OpSummary`] 由来）で、スクリプトの `await` を成功で
    /// 解くか例外で reject するかの判定に使う。`single_name` はコピー/移動の対象が単独だった
    /// ときのその名前（移動先ペインのカーソルを届いたファイルへ寄せる。複数対象・削除は None）。
    Done {
        id: u64,
        kind: OpKind,
        src_dir: PathBuf,
        dst_dir: PathBuf,
        cancelled: bool,
        failed: bool,
        single_name: Option<String>,
    },
    /// 進捗（インプレース更新中の本文）。スクリプトの `onProgress` へ流すためタスク id を伴う。
    /// タスクマネージャのログ行更新は別経路（`LogLine`/`LogUpdate`）が担う。
    Progress { task_id: u64, text: String },
    /// 書庫一括展開の完了。成功なら `temp_root` を提供先として登録する。完了反映は
    /// 「この書庫を指して読込中のペイン」を UI 側で走査して行うため side は持たない。
    ArchiveDone {
        id: u64,
        archive: PathBuf,
        temp_root: PathBuf,
        outcome: ArchiveOutcome,
    },
    /// 書庫への書込み（追加/再構築）の完了。関与した両ペインを再読込する。
    ArchiveWriteDone {
        id: u64,
        src_is_left: bool,
    },
    /// ディレクトリ使用量計算の完了。結果をログへ出し、計算した側のペインで対象
    /// ディレクトリの一覧行へサイズを反映する（再読込までの一時表示）。
    DirInfoDone {
        id: u64,
        is_left: bool,
        label: String,
        bytes: u64,
        files: u64,
        dirs: u64,
        /// 対象ごとの集計サイズ（一覧行への反映用）。
        entries: Vec<rerics_core::CalcEntry>,
    },
    /// 検索・比較の開始。指定ペインを結果モード（".." のみ）へ切り替える。
    FindBegin { id: u64, is_left: bool },
    /// 検索・比較で見つかった項目を1件、結果一覧へ追記する。
    FindItem { id: u64, is_left: bool, item: rerics_core::FileItem },
    /// 検索・比較の完了。タスクを登録解除する。件数サマリはワーカーが進捗行
    /// （`LogLine`/`LogUpdate`）を最終サマリへ確定させて表示する。
    FindDone { id: u64, is_left: bool },
    /// スクリプトエンジン起動完了。停止（強制終了）に使う isolate ハンドルを UI へ渡す。
    ScriptEngineReady { handle: deno_core::v8::IsolateHandle },
    /// スクリプト実行の開始。指定の表示名でスクリプトタスクを登録する（直列実行＝同時に
    /// 走るのは1つなので、UI 側は「現在のスクリプトタスク」を1つだけ覚える）。
    ScriptBegin { text: String, description: String },
    /// スクリプト実行の終了。現在のスクリプトタスクを登録解除する。
    ScriptEnd,
}

/// スクリプトのログ出力（追記・インプレース更新）を運ぶイベント。`WorkerEvent` とは別チャネルで
/// 流し、`getLog` がこのチャネルだけを drain して読むことで、モーダル等の制御イベントに触れずに
/// スクリプト自身の書き込みを読み戻せる（read-your-writes）ようにする。
pub enum LogEvent {
    /// インプレース更新できる `id` 付きの行を追記する。
    Line { id: u64, level: LogLevel, text: String },
    /// `id` 付き行の本文を書き換える。`level` が `Some` のときレベル（表示色）も差し替える。
    Update { id: u64, level: Option<LogLevel>, text: String },
    /// `id` 付き行で「ぐるぐる（＋任意で百分率）」の生存表示を始める。
    ProgressStart { id: u64 },
    /// 進行表示中の行の進捗比（`done`/`total`）を設定する。
    ProgressSet { id: u64, done: u64, total: u64 },
    /// 進行表示を止める。
    ProgressStop { id: u64 },
}

/// [`OperationHost`] の GUI 実装。ログをチャネルへ送り、共有フラグで中止を伝え、
/// 衝突は UI へ往復で問い合わせる。「すべてに適用」の選択はここでキャッシュする。
pub struct ChannelHost {
    pub tx: Sender<WorkerEvent>,
    pub shutdown: Arc<AtomicBool>,
    pub control: Arc<TaskControl>,
    /// 進捗行 id の払い出し元（全タスクで共有し一意にする）。
    pub progress_seq: Arc<AtomicU64>,
    pub conflict_cache: RefCell<Option<ConflictResolution>>,
    pub delete_warn_cache: RefCell<Option<DeleteWarnChoice>>,
    /// ディレクトリコピー時の属性/日時複製の設定。既定は複製しない。
    pub copy_opts: CopyOptions,
    /// 紐づくタスク id。`Some` のとき進捗を `WorkerEvent::Progress` でも流す（スクリプトの
    /// `onProgress` 用）。スクリプトが待ち得る操作（コピー/移動/削除）でのみ設定する。
    pub task_id: Option<u64>,
    /// この操作を開始したタブ。ログはこのタブへ届ける（切替後もそのタブに残す）。
    pub origin_tab: u64,
}

impl ChannelHost {
    pub fn new(
        tx: Sender<WorkerEvent>,
        shutdown: Arc<AtomicBool>,
        control: Arc<TaskControl>,
        progress_seq: Arc<AtomicU64>,
        origin_tab: u64,
    ) -> Self {
        Self {
            tx,
            shutdown,
            control,
            progress_seq,
            conflict_cache: RefCell::new(None),
            delete_warn_cache: RefCell::new(None),
            copy_opts: CopyOptions::default(),
            task_id: None,
            origin_tab,
        }
    }

    /// ディレクトリコピー時の属性/日時複製の設定を与える（コピー/移動の起動時に config から）。
    pub fn with_copy_options(mut self, opts: CopyOptions) -> Self {
        self.copy_opts = opts;
        self
    }

    /// 進捗をスクリプトへ流すためのタスク id を紐づける。
    pub fn with_task_id(mut self, id: u64) -> Self {
        self.task_id = Some(id);
        self
    }

    /// 紐づくタスク id があれば進捗本文を `WorkerEvent::Progress` で流す（スクリプト用）。
    fn emit_script_progress(&self, text: &str) {
        if let Some(task_id) = self.task_id {
            let _ = self.tx.send(WorkerEvent::Progress { task_id, text: text.to_owned() });
        }
    }
}

impl OperationHost for ChannelHost {
    fn log(&self, level: LogLevel, text: &str) {
        let _ = self.tx.send(WorkerEvent::Log {
            level,
            text: text.to_owned(),
            origin_tab: self.origin_tab,
        });
    }

    fn copy_options(&self) -> CopyOptions {
        self.copy_opts
    }

    fn cancelled(&self) -> bool {
        self.control.is_stopped() || self.shutdown.load(Ordering::Relaxed)
    }

    fn wait_while_suspended(&self) {
        while self.control.is_suspended() && !self.shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn begin_progress(&self, level: LogLevel, text: &str) -> ProgressHandle {
        let id = self.progress_seq.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(WorkerEvent::LogLine {
            id,
            level,
            text: text.to_owned(),
            origin_tab: self.origin_tab,
        });
        self.emit_script_progress(text);
        ProgressHandle(id)
    }

    fn update_progress(&self, handle: ProgressHandle, text: &str) {
        let _ = self.tx.send(WorkerEvent::LogUpdate {
            id: handle.0,
            level: None,
            text: text.to_owned(),
            origin_tab: self.origin_tab,
        });
        self.emit_script_progress(text);
    }

    fn end_progress(&self, handle: ProgressHandle, text: &str) {
        let _ = self.tx.send(WorkerEvent::LogEnd {
            id: handle.0,
            level: None,
            text: text.to_owned(),
            origin_tab: self.origin_tab,
        });
        self.emit_script_progress(text);
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
