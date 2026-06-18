use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use winsafe::{self as w, prelude::*};
use rerics_core::{LogLevel, messages};
use crate::task::{self, ArchiveOutcome, OpKind, TaskControl, TaskEntry, WorkerEvent};
use crate::{MainWindow, dialog, task_manager};

impl MainWindow {
    /// 新しいタスク ID を払い出す。
    pub(crate) fn next_id(&self) -> u64 {
        let n = self.next_task_id.get();
        self.next_task_id.set(n + 1);
        n
    }

    /// タスクをレジストリに登録し、最初の1件なら取り込みタイマを起動する。
    pub(crate) fn register_task(
        &self,
        id: u64,
        text: &str,
        description: String,
        control: Arc<TaskControl>,
    ) -> w::AnyResult<()> {
        let was_empty = self.tasks.borrow().is_empty();
        self.tasks.borrow_mut().push(TaskEntry {
            id,
            text: text.to_owned(),
            description,
            control,
            start: Instant::now(),
        });
        if was_empty {
            self.wnd
                .hwnd()
                .SetTimer(task::TASK_TIMER_ID, task::TASK_TIMER_MS, None)?;
        }
        Ok(())
    }

    /// タスクマネージャ・モーダルを開く。
    pub(crate) fn open_task_manager(&self) -> w::AnyResult<()> {
        task_manager::show(&self.wnd, &self.tasks);
        Ok(())
    }

    /// ワーカーからのイベントを取り込み、ログ反映・完了処理を行う。
    ///
    /// 衝突モーダル表示中はモーダルの内部ループから `WM_TIMER` が再入するため、
    /// `in_dialog` ガードで多重取り込みを抑止する。
    pub(crate) fn pump_tasks(&self) -> w::AnyResult<()> {
        if self.in_dialog.get() {
            return Ok(());
        }
        while let Ok(ev) = self.task_rx.try_recv() {
            match ev {
                WorkerEvent::Log { level, text } => match level {
                    LogLevel::Normal => self.log.normal(&text),
                    LogLevel::Info => self.log.info(&text),
                    LogLevel::Warning => self.log.warn(&text),
                    LogLevel::Error => self.log.error(&text),
                },
                WorkerEvent::LogLine { id, level, text } => {
                    self.log.push_with_id(id, level, &text);
                }
                WorkerEvent::LogUpdate { id, text } => {
                    self.log.update(id, &text);
                }
                WorkerEvent::AskConflict { name, reply } => {
                    self.in_dialog.set(true);
                    let (choice, all) = dialog::conflict_box(&self.wnd, &name);
                    self.in_dialog.set(false);
                    let _ = reply.send(task::ConflictReply { choice, all });
                }
                WorkerEvent::AskDeleteWarn { name, attr, reply } => {
                    self.in_dialog.set(true);
                    let msg = messages::delete_warning_question(&name, &attr);
                    let r = dialog::message_box(
                        &self.wnd,
                        "削除",
                        &msg,
                        dialog::MessageStyle::YesNoCancelAll,
                    );
                    self.in_dialog.set(false);
                    let _ = reply.send(r);
                }
                WorkerEvent::Done { id, kind, src_dir, dst_dir } => {
                    self.on_op_done(kind, &src_dir, &dst_dir)?;
                    self.tasks.borrow_mut().retain(|e| e.id != id);
                    self.maybe_kill_task_timer();
                }
                WorkerEvent::ArchiveProgress { is_left, done, total } => {
                    self.view(is_left).set_loading_progress(done, total);
                }
                WorkerEvent::ArchiveDone { id, archive, temp_root, outcome } => {
                    self.tasks.borrow_mut().retain(|e| e.id != id);
                    self.archive_extracting.borrow_mut().remove(&archive);
                    // この書庫を指して読込中のペイン（両側あり得る）をまとめて反映する。
                    let sides: Vec<bool> = [true, false]
                        .into_iter()
                        .filter(|&s| {
                            self.view(s).is_loading() && self.pane_in_archive(s, &archive)
                        })
                        .collect();
                    match outcome {
                        ArchiveOutcome::Ok => {
                            self.archive_extracted
                                .borrow_mut()
                                .insert(archive, temp_root);
                            for s in sides {
                                self.view(s).clear_loading();
                                self.reload_side(s)?;
                            }
                        }
                        ArchiveOutcome::Cancelled => {
                            self.log.warn("書庫の読込を中止しました");
                            for s in sides {
                                self.view(s).clear_loading();
                                self.exit_archive_to_parent(s)?;
                            }
                        }
                        ArchiveOutcome::Failed(e) => {
                            self.log.error(&format!("書庫を展開できません: {}", e));
                            for s in sides {
                                self.view(s).clear_loading();
                                self.exit_archive_to_parent(s)?;
                            }
                        }
                    }
                    self.maybe_kill_task_timer();
                }
                WorkerEvent::ArchiveWriteDone { id, src_is_left } => {
                    self.tasks.borrow_mut().retain(|e| e.id != id);
                    self.reload_side(src_is_left)?;
                    self.reload_side(!src_is_left)?;
                    self.maybe_kill_task_timer();
                }
                WorkerEvent::DirInfoDone { id, label, bytes, files, dirs } => {
                    self.tasks.borrow_mut().retain(|e| e.id != id);
                    self.maybe_kill_task_timer();
                    let msg = messages::directory_information(&label, bytes, files, dirs);
                    self.log.normal(&msg);
                    self.in_dialog.set(true);
                    dialog::message_box(&self.wnd, "情報", &msg, dialog::MessageStyle::OkOnly);
                    self.in_dialog.set(false);
                }
            }
        }
        // 読込中ペインのスピナーを進める（タイマ間隔ごとに1コマ）。
        for is_left in [true, false] {
            self.view(is_left).tick_loading();
        }
        Ok(())
    }

    /// 操作完了に応じて関与した側のペインを再読込・選択解除する。
    pub(crate) fn on_op_done(&self, kind: OpKind, src_dir: &Path, dst_dir: &Path) -> w::AnyResult<()> {
        for is_left in [true, false] {
            let path = self.pane(is_left).borrow().path().to_path_buf();
            let is_src = path == src_dir;
            let is_dst = path == dst_dir;
            match kind {
                OpKind::Copy => {
                    if is_dst {
                        self.reload_side(is_left)?;
                    } else if is_src {
                        self.view(is_left).state().borrow_mut().clear_all();
                        self.view(is_left).refresh()?;
                    }
                }
                OpKind::Move => {
                    if is_src || is_dst {
                        self.reload_side(is_left)?;
                    }
                }
                OpKind::Delete => {
                    if is_src {
                        self.reload_side(is_left)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// タスクが空で、かつどちらのペインも読込中でなければ取り込みタイマを止める。
    pub(crate) fn maybe_kill_task_timer(&self) {
        let loading = self.view(true).is_loading() || self.view(false).is_loading();
        if self.tasks.borrow().is_empty() && !loading {
            let _ = self.wnd.hwnd().KillTimer(task::TASK_TIMER_ID);
        }
    }
}
