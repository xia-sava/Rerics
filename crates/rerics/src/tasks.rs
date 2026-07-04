use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use winsafe::{self as w, prelude::*};
use rerics_core::{LogLevel, messages};
use crate::task::{self, ArchiveOutcome, OpKind, TaskControl, TaskEntry, TaskKind, WorkerEvent};
use crate::{MainWindow, dialog, task_manager};

impl MainWindow {
    /// 新しいタスク ID を払い出す。
    pub(crate) fn next_id(&self) -> u64 {
        let n = self.next_task_id.get();
        self.next_task_id.set(n + 1);
        n
    }

    /// 通常タスクをレジストリに登録し、最初の1件なら取り込みタイマを起動する。
    pub(crate) fn register_task(
        &self,
        id: u64,
        text: &str,
        description: String,
        control: Arc<TaskControl>,
    ) -> w::AnyResult<()> {
        self.register_task_kind(id, text, description, control, TaskKind::Normal)
    }

    /// 種別を指定してタスクを登録する。最初の1件なら取り込みタイマを起動する。
    pub(crate) fn register_task_kind(
        &self,
        id: u64,
        text: &str,
        description: String,
        control: Arc<TaskControl>,
        kind: TaskKind,
    ) -> w::AnyResult<()> {
        let was_empty = self.tasks.borrow().is_empty();
        self.tasks.borrow_mut().push(TaskEntry {
            id,
            text: text.to_owned(),
            description,
            control,
            start: Instant::now(),
            kind,
        });
        if was_empty {
            self.wnd
                .hwnd()
                .SetTimer(task::TASK_TIMER_ID, task::TASK_TIMER_MS, None)?;
        }
        Ok(())
    }

    /// 任意の仕事をワーカースレッドで回し、結果を UI スレッドの継続 `done` に渡す汎用ジョブ。
    /// `work` はワーカースレッドで走る（`Send`）、結果 `T` も `Send`。`done` は UI スレッドで
    /// 走るので `Send` 不要＝開いているダイアログのコントロール等を自由にキャプチャできる。
    /// 継続は `in_dialog` 中も配達されるので、モーダルを開いたまま後追いで内容を埋められる
    /// （Android の Main looper 相当を winsafe 向けに手で用意したもの）。
    pub(crate) fn spawn_job<T, W, D>(&self, work: W, done: D)
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
        D: FnOnce(&MainWindow, T) -> w::AnyResult<()> + 'static,
    {
        let id = self.next_id();
        self.ui_jobs.borrow_mut().insert(
            id,
            Box::new(move |mw, any| match any.downcast::<T>() {
                Ok(t) => done(mw, *t),
                Err(_) => Ok(()),
            }),
        );
        self.ensure_task_timer();
        let tx = self.ui_job_tx.clone();
        std::thread::spawn(move || {
            let result = work();
            let _ = tx.send((id, Box::new(result)));
        });
    }

    /// 取り込みタイマを起動する（汎用ジョブ用。`SetTimer` は同一 id 再呼び出しでリセット
    /// されるだけなので冪等に呼べる）。
    fn ensure_task_timer(&self) {
        let _ = self
            .wnd
            .hwnd()
            .SetTimer(task::TASK_TIMER_ID, task::TASK_TIMER_MS, None);
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
        // 汎用ジョブの継続は自己完結で安全なので、モーダル表示中（in_dialog）でも配達する。
        // 取り込み中に継続が別のジョブを積むこともあるので、対応表から取り出してから呼ぶ。
        while let Ok((id, result)) = self.ui_job_rx.try_recv() {
            let done = self.ui_jobs.borrow_mut().remove(&id);
            if let Some(done) = done {
                done(self, result)?;
            }
        }
        if self.in_dialog.get() {
            return Ok(());
        }
        // スクリプトのログ出力（追記・更新）はこの取り込みでまとめて反映する。
        self.drain_log_events();
        // 検索・比較のライブ追加は1取り込みぶんをまとめて1回だけ再描画する（項目ごとの
        // 再描画を避ける）。この取り込みで項目が増えた側を覚えておく。
        let mut find_dirty = [false, false];
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
                    self.log.start_progress(id);
                }
                WorkerEvent::LogUpdate { id, level, text } => {
                    self.log.update(id, level, &text);
                }
                WorkerEvent::LogEnd { id, level, text } => {
                    self.log.update(id, level, &text);
                    self.log.stop_progress(id);
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
                WorkerEvent::Done { id, kind, src_dir, dst_dir, cancelled, failed, single_name } => {
                    // 中止・失敗時は届いたか不確かなので、カーソル寄せはしない。
                    let focus = if cancelled || failed { None } else { single_name };
                    self.on_op_done(kind, &src_dir, &dst_dir, focus.as_deref())?;
                    self.tasks.borrow_mut().retain(|e| e.id != id);
                    self.notify_script_op_done(id, cancelled, failed);
                    self.maybe_kill_task_timer();
                }
                WorkerEvent::Progress { task_id, text } => {
                    self.notify_script_op_progress(task_id, &text);
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
                WorkerEvent::DirInfoDone { id, is_left, label, bytes, files, dirs, entries } => {
                    self.tasks.borrow_mut().retain(|e| e.id != id);
                    self.maybe_kill_task_timer();
                    let msg = messages::directory_information(&label, bytes, files, dirs);
                    self.log.normal(&msg);
                    self.apply_dir_sizes(is_left, &entries)?;
                }
                WorkerEvent::FindBegin { id, is_left } => {
                    let idx = if is_left { 0 } else { 1 };
                    // 現役タスクの開始だけがペインを結果モードへ切り替える（追い越された
                    // 旧タスクの開始通知は無視する）。
                    if self.find_task.borrow()[idx] == Some(id) {
                        self.view(is_left).state().borrow_mut().begin_find_result();
                        find_dirty[idx] = true;
                    }
                }
                WorkerEvent::FindItem { id, is_left, item } => {
                    let idx = if is_left { 0 } else { 1 };
                    if self.find_task.borrow()[idx] == Some(id) {
                        self.view(is_left).state().borrow_mut().push_find_result(item);
                        find_dirty[idx] = true;
                    }
                }
                WorkerEvent::FindDone { id, is_left } => {
                    // タスク登録解除は id 一致で必ず行う（追い越された旧タスクの後始末も）。
                    self.tasks.borrow_mut().retain(|e| e.id != id);
                    let idx = if is_left { 0 } else { 1 };
                    if self.find_task.borrow()[idx] == Some(id) {
                        self.find_task.borrow_mut()[idx] = None;
                        // 件数サマリはワーカーが進捗行を確定させて表示済み。
                        // 確定後の列幅を内容に合わせる。
                        self.view(is_left).autofit_columns()?;
                        // 操作後リフレッシュ・リネーム後の再検索では、完了時にカーソルを戻す。
                        // 名前（出自込み）で追い、結果から消えていれば元の行位置へ。
                        if let Some(refocus) = self.find_refocus.borrow_mut()[idx].take() {
                            let pr = self.view(is_left).page_rows();
                            let state = self.view(is_left).state();
                            let mut s = state.borrow_mut();
                            let found = refocus
                                .name
                                .as_deref()
                                .map(|n| {
                                    s.set_cursor_position_sourced(n, refocus.source.as_ref(), pr)
                                })
                                .unwrap_or(false);
                            if !found {
                                s.set_cursor(refocus.index as isize, pr);
                            }
                            s.select_start = s.cursor;
                        }
                        find_dirty[idx] = true;
                    }
                    self.maybe_kill_task_timer();
                }
                WorkerEvent::ScriptEngineReady { handle } => {
                    *self.script_isolate.borrow_mut() = Some(handle);
                }
                WorkerEvent::ScriptBegin { text, description } => {
                    // スクリプトは直列実行＝同時に走るのは1つ。新しい id で登録し直す。
                    let id = self.next_id();
                    let control = Arc::new(TaskControl::new());
                    let _ = self.register_task_kind(id, &text, description, control, TaskKind::Script);
                    *self.script_task.borrow_mut() = Some(id);
                    self.script_terminated.set(false);
                }
                WorkerEvent::ScriptEnd => {
                    if let Some(id) = self.script_task.borrow_mut().take() {
                        self.tasks.borrow_mut().retain(|e| e.id != id);
                    }
                    self.script_terminated.set(false);
                    // 回しっぱなしのログ進行表示を回収する（stopProgress 忘れの保険）。
                    self.log.stop_all_progress();
                    self.maybe_kill_task_timer();
                }
            }
        }
        // 中止されたスクリプトタスクは、V8 isolate を強制終了して止める（暴走 JS も止められる）。
        // terminate は一度だけ。エンジンが巻き戻ると ScriptEnd が届いて登録解除される。
        self.terminate_script_if_stopped();
        for is_left in [true, false] {
            let idx = if is_left { 0 } else { 1 };
            if find_dirty[idx] {
                // 列幅は流れてくる項目に合わせて毎回詰める（完了時にまとめて詰めない）。
                self.view(is_left).autofit_columns()?;
                self.view(is_left).refresh()?;
            }
        }
        // 読込中ペインのスピナーを進める（タイマ間隔ごとに1コマ）。
        for is_left in [true, false] {
            self.view(is_left).tick_loading();
        }
        // 進行表示中のログ行のぐるぐるも進める。
        self.log.tick_progress();
        self.maybe_kill_task_timer();
        Ok(())
    }

    /// 操作完了に応じて関与した側のペインを再読込・選択解除する。結果一覧は再検索（非同期・
    /// WAKE 取り込みで即時反映）、通常一覧は同期再読込で最新化する。`single_focus` はコピー/
    /// 移動の対象が単独だったときのその名前で、移動先ペインのカーソルを届いたファイルへ寄せる
    /// （複数対象はカーソル下ファイルの名前追従に任せる）。
    pub(crate) fn on_op_done(
        &self,
        kind: OpKind,
        src_dir: &Path,
        dst_dir: &Path,
        single_focus: Option<&str>,
    ) -> w::AnyResult<()> {
        for is_left in [true, false] {
            let path = self.pane(is_left).borrow().path().to_path_buf();
            let is_src = path == src_dir;
            let is_dst = path == dst_dir;
            match kind {
                OpKind::Copy => {
                    if is_dst {
                        self.refresh_side(is_left, single_focus)?;
                    } else if is_src {
                        self.view(is_left).state().borrow_mut().clear_all();
                        self.view(is_left).refresh()?;
                    }
                }
                OpKind::Move => {
                    if is_dst {
                        self.refresh_side(is_left, single_focus)?;
                    } else if is_src {
                        self.refresh_side(is_left, None)?;
                    }
                }
                OpKind::Delete => {
                    if is_src {
                        self.refresh_side(is_left, None)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// 現在のスクリプトタスクが中止されていれば、V8 isolate を強制終了して実行中の
    /// スクリプトを止める。terminate は現在のタスクにつき一度だけ発行する（多重発行防止）。
    /// 終了後はエンジンが巻き戻って `ScriptEnd` を送り、タスクが登録解除される。
    fn terminate_script_if_stopped(&self) {
        if self.script_terminated.get() {
            return;
        }
        let Some(id) = *self.script_task.borrow() else {
            return;
        };
        let stopped = self.tasks.borrow().iter().any(|e| e.id == id && e.control.is_stopped());
        if !stopped {
            return;
        }
        if let Some(handle) = self.script_isolate.borrow().as_ref() {
            let _ = handle.terminate_execution();
            // 走行中の並列ワーカーも止める（メインを止めるだけだとワーカースレッドは回り続ける）。
            for (_, worker) in self.script_worker_isolates.lock().unwrap().iter() {
                let _ = worker.terminate_execution();
            }
            // terminate 済みアイソレートは再利用しない。次の parallel() でプールを畳んで作り直させる。
            self.script_pool_stopped.store(true, std::sync::atomic::Ordering::Release);
            self.script_terminated.set(true);
            self.log.warn("スクリプトを停止しました");
        }
    }

    /// タスク・汎用ジョブとも空で、かつどちらのペインも読込中でなければ取り込みタイマを止める。
    pub(crate) fn maybe_kill_task_timer(&self) {
        let loading = self.view(true).is_loading() || self.view(false).is_loading();
        if self.tasks.borrow().is_empty() && !loading && self.ui_jobs.borrow().is_empty() {
            let _ = self.wnd.hwnd().KillTimer(task::TASK_TIMER_ID);
        }
    }
}
