use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use winsafe::{self as w, prelude::*};
use rerics_core::LogLevel;
use crate::MainWindow;
use crate::search_bar::OptKind;
use crate::task::{TaskControl, WorkerEvent};

/// 進捗行を更新する走査件数の間隔。これだけ走査するごとに「走査N件…」を書き換える
/// （完了時は件数に関わらず最終サマリへ確定する）。項目ごとの再描画を避けつつ進捗を出す。
const SCAN_REPORT_EVERY: usize = 256;

/// ライブ追加の起床通知を間引く件数の間隔。これだけ項目が増えるごとに `TASK_WAKE` を撒く
/// （pump は1回で全 drain するので項目ごとに撒く必要はない）。中間項目は取り込みタイマ
/// （50ms）が backstop になり、最後の取りこぼしは完了時の無条件 WAKE が確実に拾う。
const WAKE_EVERY: usize = 64;

/// 検索・比較ワーカーが各境界で呼ぶ続行判定。中断中はブロックして待ち、中止または
/// アプリ終了が要求されていれば `true`（走査を打ち切る）を返す。
fn search_cancelled(control: &TaskControl, shutdown: &AtomicBool) -> bool {
    while control.is_suspended() && !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    control.is_stopped() || shutdown.load(Ordering::Relaxed)
}

impl MainWindow {
    pub(crate) fn mask(&self, is_left: bool) -> &Rc<RefCell<Option<String>>> {
        if is_left { &self.left_mask } else { &self.right_mask }
    }

    /// 入力ダイアログでパスマスクを尋ね、no-UI 版 `r.pathMask` へ委譲する。設定/解除の正規化
    /// （空・`*` は解除）と一覧更新は委譲先（`SetPathMask`）が行う。
    pub(crate) fn path_mask(&self) -> w::AnyResult<()> {
        let is_left = !self.active_right.get();
        let cur = self.mask(is_left).borrow().clone().unwrap_or_default();
        // 現在のマスクを初期値に投入する（未設定なら `*`＝解除）。開いた瞬間に全選択。
        let initial = if cur.trim().is_empty() { "*".to_owned() } else { cur };
        let input = self.input_mask(
            "パスマスク",
            "表示するマスク（* で解除・カンマ区切り）:",
            &initial,
            "pathmask",
        );
        let Some(input) = input else {
            return Ok(());
        };
        let arg = serde_json::to_string(input.trim()).unwrap_or_else(|_| "\"\"".to_string());
        self.script_send(crate::script_host::EngineCmdKind::Eval(format!("r.pathMask({arg})")));
        Ok(())
    }

    /// 入力ダイアログでマスクを尋ね、選んだマスクで選択し直す。選択ロジックは UI を持たない
    /// `r.selectMask`（カンマ区切りの Like パターン・既存選択をクリアして選び直す）へ委譲する。
    pub(crate) fn select_mask(&self) -> w::AnyResult<()> {
        // 初期値は前回の確定値（無ければ `*`）。開いた瞬間に全選択して上書きしやすくする。
        let input = self.input_mask(
            "マスクで選択",
            "選択するマスク（カンマ区切り）:",
            "",
            "selectmask",
        );
        let Some(input) = input else {
            return Ok(());
        };
        let input = input.trim();
        if input.is_empty() {
            return Ok(());
        }
        let arg = serde_json::to_string(input).unwrap_or_else(|_| "\"\"".to_string());
        self.script_send(crate::script_host::EngineCmdKind::Eval(format!("r.selectMask({arg})")));
        Ok(())
    }

    /// 比較方法を一覧から選ばせ、選んだ条件で同名ファイル比較選択を実行する。比較ロジック
    /// 自体は UI を持たない `r.compare` に委譲する（UI で選んだ条件を引数として渡すだけ）。
    pub(crate) fn compare_dialog(&self) -> w::AnyResult<()> {
        const OPTIONS: &[(&str, &str)] = &[
            ("名前一致のみ", "name"),
            ("日付が一致", "sameDate"),
            ("日付が不一致", "diffDate"),
            ("日付が新しい", "newer"),
            ("日付が古い", "older"),
            ("サイズが一致", "sameSize"),
            ("サイズが不一致", "diffSize"),
            ("サイズが小さい", "smaller"),
            ("サイズが大きい", "larger"),
            ("存在しないファイル", "notExists"),
        ];
        let labels: Vec<String> = OPTIONS.iter().map(|(label, _)| label.to_string()).collect();
        let Some(idx) =
            crate::dialog::list_box(&self.wnd, "同名ファイル選択", "compare_dialog", &labels, 0)
        else {
            return Ok(());
        };
        let token = OPTIONS[idx].1;
        self.script_send(crate::script_host::EngineCmdKind::Eval(format!("r.compare({token:?})")));
        Ok(())
    }

    /// 検索・比較タスクを起こす。同ペインで前の検索／比較がまだ走っていれば先に止めて
    /// （結果が混ざらないように）、新しいタスクを登録し、払い出した id と制御を返す。
    /// `find_task` のスロットは同期で立てるので、直後に別検索が来ても取り違えない。
    fn start_find_task(
        &self,
        is_left: bool,
        text: &str,
        desc: String,
    ) -> w::AnyResult<(u64, Arc<TaskControl>)> {
        let idx = if is_left { 0 } else { 1 };
        let prev = self.find_task.borrow()[idx];
        if let Some(prev) = prev {
            let tasks = self.tasks.borrow();
            if let Some(e) = tasks.iter().find(|e| e.id == prev) {
                e.control.stop();
            }
        }
        // 旧タスクの復元位置を引き継がない（追い越された再検索の refocus が新検索へ漏れて
        // カーソルを誤誘導するのを防ぐ）。再検索で残したい場合は呼び側が起動後に再設定する。
        self.find_refocus.borrow_mut()[idx] = None;
        let control = Arc::new(TaskControl::new());
        let id = self.next_id();
        self.register_task(id, text, desc, control.clone())?;
        self.find_task.borrow_mut()[idx] = Some(id);
        Ok((id, control))
    }

    /// 指定側で走行中の検索／比較タスクがあれば止め、結果一覧まわりのスロットを片付ける。
    /// 結果一覧から実ディレクトリへ離脱したときに呼ぶ。これを怠ると、遅れて届く
    /// `FindDone`/`FindBegin` が通常一覧へ干渉する（カーソル飛び・列詰め・結果モード復帰）。
    pub(crate) fn cancel_find_task(&self, is_left: bool) {
        let idx = if is_left { 0 } else { 1 };
        if let Some(id) = self.find_task.borrow_mut()[idx].take() {
            let tasks = self.tasks.borrow();
            if let Some(e) = tasks.iter().find(|e| e.id == id) {
                e.control.stop();
            }
        }
        self.find_refocus.borrow_mut()[idx] = None;
    }

    /// 条件ダイアログ（名前・日付・サイズ）を出し、OK ならその条件でファイル検索を実行する。
    /// ファイル名マスクの初期値は前回の確定値（無ければ `*`）。検索ロジックは `run_find_file`
    /// （Rust 正本）へ委譲する。
    pub(crate) fn find_file_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        let mut hist = rerics_core::InputHistory::load();
        let initial = hist.get("findfile").first().cloned().unwrap_or_else(|| "*".to_owned());
        if let Some((opts, raw_name)) = crate::dialog::find_file_box(&self.wnd, &initial) {
            let raw = raw_name.trim();
            if !raw.is_empty() {
                hist.add("findfile", raw);
                let _ = hist.save();
            }
            if opts.is_empty() {
                self.log.warn("検索条件がありません。");
            } else {
                self.run_find_file(is_left, opts);
            }
        }
        Ok(())
    }

    /// その場のリフレッシュ（ファイル操作後・リネーム後など）。結果一覧モードなら元の検索／
    /// 比較を再実行して一覧を作り直し（結果モードを保つ）、そうでなければ通常の同期再読込。
    /// カーソルは `focus` 指定があればその名へ寄せ、無ければカーソル下ファイルを名前で追い、
    /// 消えていれば元の行位置（index）を保つ。親移動・項目を開く等の「離脱」操作は従来どおり
    /// [`reload_side`](Self::reload_side) 系を使う。
    pub(crate) fn refresh_side(&self, is_left: bool, focus: Option<&str>) -> w::AnyResult<()> {
        if self.view(is_left).state().borrow().find_result && self.research_side(is_left, focus) {
            return Ok(());
        }
        let mode = match focus {
            Some(name) => crate::ReloadCursor::Focus { name: name.to_owned(), center: false },
            None => crate::ReloadCursor::Keep,
        };
        self.reload_side_now(is_left, mode)
    }

    /// 覚えている検索／比較条件を**非同期で**再実行し、結果一覧を作り直す。条件が無ければ
    /// `false`（呼び側は通常再読込へ）。走査はワーカースレッドで回し、結果は WAKE 取り込みで
    /// 即時にストリーム反映する（大きな木でも UI スレッドを止めない）。完了時にカーソルを
    /// `focus`（リネーム後の新名）または現在のカーソル下の名前で追い、結果から消えていれば
    /// 元の行位置へ戻す（`find_refocus`）。
    pub(crate) fn research_side(&self, is_left: bool, focus: Option<&str>) -> bool {
        let idx = if is_left { 0 } else { 1 };
        let query = self.find_query.borrow()[idx].clone();
        let Some(query) = query else {
            return false;
        };
        // 再検索後のカーソル：リネームは新名（呼び側指定）、操作後はカーソル下の名前で追い、
        // 結果から消えていれば元の行位置へ戻す。名前照合は出自（source）込み。
        let refocus = {
            let st = self.view(is_left).state();
            let s = st.borrow();
            let cur = s.items.get(s.cursor);
            crate::Refocus {
                name: focus.map(str::to_owned).or_else(|| cur.map(|it| it.name.clone())),
                source: cur.and_then(|it| it.source.clone()),
                index: s.cursor,
            }
        };
        // 退避はタスク起動後に行う（start_find_task が refocus をリセットするため、その後に置く）。
        match query {
            crate::FindQuery::Find(opts) => self.run_find_file(is_left, opts),
            crate::FindQuery::Compare(opts) => self.run_directory_compare(is_left, opts),
        }
        self.find_refocus.borrow_mut()[idx] = Some(refocus);
        true
    }

    /// 現在地以下を再帰検索し、条件に合うファイルを結果一覧へ出す。検索はタスクとして
    /// ワーカースレッドで回し、見つかった項目を1件ずつ結果ペインへライブ追加する。
    /// タスクマネージャから中止・中断・再開できる。
    pub(crate) fn run_find_file(&self, is_left: bool, opts: rerics_core::FindOptions) {
        let root = self.pane(is_left).borrow().loc().clone();
        // 操作後のリフレッシュで再検索できるよう条件を覚える（結果モードを保つ）。
        self.find_query.borrow_mut()[if is_left { 0 } else { 1 }] =
            Some(crate::FindQuery::Find(opts.clone()));
        self.log.info(&format!("ファイル検索: {}", root.loc_display()));
        let Ok((id, control)) = self.start_find_task(is_left, "ファイル検索", root.loc_display())
        else {
            return;
        };
        let tx = self.task_tx.clone();
        let shutdown = self.shutdown.clone();
        let wake = self.wnd.hwnd().ptr() as isize;
        let pid = self.progress_seq.fetch_add(1, Ordering::Relaxed);
        let origin_tab = self.active_tab_id();
        std::thread::spawn(move || {
            let _ = tx.send(WorkerEvent::FindBegin { id, is_left });
            let _ = tx.send(WorkerEvent::LogLine {
                id: pid,
                level: LogLevel::Info,
                text: "ファイル検索中… 走査 0件 該当 0件".to_owned(),
                origin_tab,
            });
            crate::winutil::post_app_message(wake, crate::winutil::msg::TASK_WAKE);
            let scanned = std::cell::Cell::new(0usize);
            let found = std::cell::Cell::new(0usize);
            let count = {
                let mut emit = |it| {
                    let n = found.get() + 1;
                    found.set(n);
                    let _ = tx.send(WorkerEvent::FindItem { id, is_left, item: it });
                    if n.is_multiple_of(WAKE_EVERY) {
                        crate::winutil::post_app_message(wake, crate::winutil::msg::TASK_WAKE);
                    }
                };
                let mut tick = || {
                    let n = scanned.get() + 1;
                    scanned.set(n);
                    if n.is_multiple_of(SCAN_REPORT_EVERY) {
                        let _ = tx.send(WorkerEvent::LogUpdate {
                            id: pid,
                            level: None,
                            text: format!("ファイル検索中… 走査 {n}件 該当 {}件", found.get()),
                            origin_tab,
                        });
                        crate::winutil::post_app_message(wake, crate::winutil::msg::TASK_WAKE);
                    }
                };
                let cancelled = || search_cancelled(&control, &shutdown);
                let mut sink = rerics_core::Sink {
                    emit: &mut emit,
                    cancelled: &cancelled,
                    progress: &mut tick,
                };
                rerics_core::find_file(&root, &opts, &mut sink)
            };
            let cancelled = control.is_stopped() || shutdown.load(Ordering::Relaxed);
            let head = if cancelled { "検索中止" } else { "検索結果" };
            let summary = format!("{head} {count}件（走査 {}件）", scanned.get());
            let level = if cancelled { Some(LogLevel::Warning) } else { None };
            let _ = tx.send(WorkerEvent::LogEnd { id: pid, level, text: summary, origin_tab });
            let _ = tx.send(WorkerEvent::FindDone { id, is_left });
            crate::winutil::post_app_message(wake, crate::winutil::msg::TASK_WAKE);
        });
    }

    /// 条件ダイアログ（日付・サイズの比較条件と抽出範囲）を出し、OK ならその条件で
    /// ディレクトリ比較を実行する。比較ロジックは `run_directory_compare`（Rust 正本）へ委譲。
    pub(crate) fn directory_compare_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        if let Some(opts) = crate::dialog::compare_options_box(&self.wnd) {
            self.run_directory_compare(is_left, opts);
        }
        Ok(())
    }

    /// アクティブペインと反対ペインのディレクトリを比較し、差分を結果一覧へ出す（原作
    /// ディレクトリ比較）。比較はタスクとしてワーカースレッドで回し、見つかった差分項目を
    /// 1件ずつ結果ペインへライブ追加する。タスクマネージャから中止・中断・再開できる。
    pub(crate) fn run_directory_compare(&self, is_left: bool, opts: rerics_core::CompareOptions) {
        let src = self.pane(is_left).borrow().loc().clone();
        let dst = self.pane(!is_left).borrow().loc().clone();
        // 操作後のリフレッシュで再比較できるよう条件を覚える（結果モードを保つ）。
        self.find_query.borrow_mut()[if is_left { 0 } else { 1 }] =
            Some(crate::FindQuery::Compare(opts));
        self.log.info(&format!("ディレクトリ比較: {}", src.loc_display()));
        let Ok((id, control)) = self.start_find_task(is_left, "ディレクトリ比較", src.loc_display())
        else {
            return;
        };
        let tx = self.task_tx.clone();
        let shutdown = self.shutdown.clone();
        let wake = self.wnd.hwnd().ptr() as isize;
        let pid = self.progress_seq.fetch_add(1, Ordering::Relaxed);
        let origin_tab = self.active_tab_id();
        std::thread::spawn(move || {
            let _ = tx.send(WorkerEvent::FindBegin { id, is_left });
            let _ = tx.send(WorkerEvent::LogLine {
                id: pid,
                level: LogLevel::Info,
                text: "ディレクトリ比較中… 走査 0件 差分 0件".to_owned(),
                origin_tab,
            });
            crate::winutil::post_app_message(wake, crate::winutil::msg::TASK_WAKE);
            let scanned = std::cell::Cell::new(0usize);
            let found = std::cell::Cell::new(0usize);
            let counts = {
                let mut emit = |it| {
                    let n = found.get() + 1;
                    found.set(n);
                    let _ = tx.send(WorkerEvent::FindItem { id, is_left, item: it });
                    if n.is_multiple_of(WAKE_EVERY) {
                        crate::winutil::post_app_message(wake, crate::winutil::msg::TASK_WAKE);
                    }
                };
                let mut tick = || {
                    let n = scanned.get() + 1;
                    scanned.set(n);
                    if n.is_multiple_of(SCAN_REPORT_EVERY) {
                        let _ = tx.send(WorkerEvent::LogUpdate {
                            id: pid,
                            level: None,
                            text: format!("ディレクトリ比較中… 走査 {n}件 差分 {}件", found.get()),
                            origin_tab,
                        });
                        crate::winutil::post_app_message(wake, crate::winutil::msg::TASK_WAKE);
                    }
                };
                let cancelled = || search_cancelled(&control, &shutdown);
                let mut sink = rerics_core::Sink {
                    emit: &mut emit,
                    cancelled: &cancelled,
                    progress: &mut tick,
                };
                rerics_core::directory_compare(&src, &dst, &opts, &mut sink)
            };
            let cancelled = control.is_stopped() || shutdown.load(Ordering::Relaxed);
            let head = if cancelled { "比較中止" } else { "比較結果" };
            let summary = format!(
                "{head} 一致:{} 不一致:{} 追加:{} 削除:{}（走査 {}件）",
                counts.equals, counts.not_equals, counts.adds, counts.deletes, scanned.get()
            );
            let level = if cancelled { Some(LogLevel::Warning) } else { None };
            let _ = tx.send(WorkerEvent::LogEnd { id: pid, level, text: summary, origin_tab });
            let _ = tx.send(WorkerEvent::FindDone { id, is_left });
            crate::winutil::post_app_message(wake, crate::winutil::msg::TASK_WAKE);
        });
    }

    /// テスト足場：中止まで回り続けるダミーのタスクを起こし、払い出した id を返す。
    /// タスク制御（中止・中断・再開）をタスクマネージャ越しに確定的に検証するために使う
    /// （検索・比較と同じ `search_cancelled` で止まる＝同じ制御機構を叩く）。進捗行も
    /// 実操作と同じ流儀（`LogLine` 開始 → `LogEnd` 確定）で持ち、進行表示（ぐるぐる）の
    /// 生存も検証できる。
    #[cfg(feature = "debug-server")]
    pub(crate) fn start_debug_task(&self) -> u64 {
        let control = Arc::new(TaskControl::new());
        let id = self.next_id();
        let _ = self.register_task(id, "デバッグ", "テスト用タスク".to_string(), control.clone());
        let tx = self.task_tx.clone();
        let shutdown = self.shutdown.clone();
        let pid = self.progress_seq.fetch_add(1, Ordering::Relaxed);
        let origin_tab = self.active_tab_id();
        std::thread::spawn(move || {
            let _ = tx.send(WorkerEvent::LogLine {
                id: pid,
                level: LogLevel::Normal,
                text: "テスト用タスク実行中".to_owned(),
                origin_tab,
            });
            // 中止（中断→中止／アプリ終了を含む）まで待つ。中断中は search_cancelled が
            // ブロックして待つ。
            while !search_cancelled(&control, &shutdown) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let _ = tx.send(WorkerEvent::LogEnd {
                id: pid,
                level: None,
                text: "テスト用タスク終了".to_owned(),
                origin_tab,
            });
            // 完了通知。id 一致でタスク登録を解除するだけ（find_task は立てていないので
            // 結果ペインには触れない）。
            let _ = tx.send(WorkerEvent::FindDone { id, is_left: true });
        });
        id
    }

    /// 共有 1 組の検索バー UI を、アクティブペインの状態と `MainWindow` の表層関数へ配線する。
    pub(crate) fn wire_search_bar(&self) {
        let this = self.clone();
        self.search_bar.on_change(move |query| {
            let _ = this.search_apply(!this.active_right.get(), query);
        });
        let this = self.clone();
        self.search_bar.on_step(move |forward| {
            this.search_step(!this.active_right.get(), forward);
        });
        let this = self.clone();
        self.search_bar.on_confirm(move || {
            let _ = this.search_confirm(!this.active_right.get());
        });
        let this = self.clone();
        self.search_bar.on_cancel(move || {
            let _ = this.search_close(!this.active_right.get());
        });
        let this = self.clone();
        self.search_bar.on_option(move |kind, on| {
            let _ = this.search_set_option(!this.active_right.get(), kind, on);
        });
    }

    /// Fキー。バーが閉じていれば開き、検索語が残っていればそこから再開する（先頭から追従）。
    /// 既にバーが開いている（再フォーカスのみ）ときは追従をやり直さず、検索ボックスへ
    /// フォーカス＋全選択するだけに留める。
    pub(crate) fn search_open(&self, is_left: bool) -> w::AnyResult<()> {
        let already_open = self.view(is_left).state().borrow().search.active;
        if already_open {
            self.search_bar.focus_edit(true);
            return Ok(());
        }
        {
            let state = self.view(is_left).state();
            state.borrow_mut().search.active = true;
        }
        let query = self.view(is_left).state().borrow().search.query.clone();
        self.search_apply(is_left, &query)?;
        self.sync_search_bar()?;
        self.search_bar.focus_edit(true);
        Ok(())
    }

    /// Esc。バーの表示を閉じ、絞り込み・ハイライトを解除する（一覧を全復元）。カーソルは
    /// バーを開いた後に検索で辿り着いた位置に留める。絞り込み解除で一覧の並び・件数が変わり
    /// index が同じ項目を指すとは限らないため、現在のカーソル項目を名前＋出自で控え、
    /// 復元後の一覧で同じ項目へ再対応付けする。検索語・オプション・絞り込み設定は保持する。
    /// フォーカスはキーシンクへ戻す。
    pub(crate) fn search_close(&self, is_left: bool) -> w::AnyResult<()> {
        let page_rows = self.view(is_left).page_rows();
        {
            let state = self.view(is_left).state();
            let mut s = state.borrow_mut();
            s.search.active = false;
            let cur = s.items.get(s.cursor);
            let name = cur.map(|it| it.name.clone());
            let source = cur.and_then(|it| it.source.clone());
            let prev_index = s.cursor;
            s.apply_search();
            s.restore_cursor_after_rebuild(name.as_deref(), source.as_ref(), prev_index, None, page_rows);
        }
        let _ = self.view(is_left).refresh();
        self.sync_search_bar()?;
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// Enter。検索語を履歴 `"filesearch"` へ記録する（空なら記録しない）。バーは開いたまま
    /// （絞り込み・ハイライトも維持）、フォーカスを一覧（キーシンク）へ戻す。
    pub(crate) fn search_confirm(&self, is_left: bool) -> w::AnyResult<()> {
        let query = self.view(is_left).state().borrow().search.query.clone();
        self.search_bar.record_history(&query);
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// 打鍵（en_change）。`query` を状態へ反映し、絞り込みを再適用したうえで先頭から追従する。
    fn search_apply(&self, is_left: bool, query: &str) -> w::AnyResult<()> {
        let page_rows = self.view(is_left).page_rows();
        {
            let state = self.view(is_left).state();
            let mut s = state.borrow_mut();
            s.search.query = query.to_owned();
            s.apply_search();
            // apply_search は scroll_top をクランプしないので、set_cursor で安全域へ戻す。
            let cursor = s.cursor as isize;
            s.set_cursor(cursor, page_rows);
        }
        self.incremental_apply(is_left);
        let _ = self.view(is_left).refresh();
        Ok(())
    }

    /// トグル変更（Case/Word/Regex/Filter）。対応するオプション/絞り込みを更新し、再適用して
    /// から現在の検索語で追従し直す。`on` は `SearchOptions` の生フィールド値。
    pub(crate) fn search_set_option(&self, is_left: bool, kind: OptKind, on: bool) -> w::AnyResult<()> {
        {
            let state = self.view(is_left).state();
            let mut s = state.borrow_mut();
            match kind {
                OptKind::Case => s.search.opts.case_sensitive = on,
                OptKind::Word => {
                    s.search.opts.whole_word = on;
                    if on {
                        s.search.opts.regex = false;
                    }
                }
                OptKind::Regex => {
                    s.search.opts.regex = on;
                    if on {
                        s.search.opts.whole_word = false;
                    }
                }
                OptKind::Filter => s.search.filter = on,
            }
        }
        let query = self.view(is_left).state().borrow().search.query.clone();
        self.search_apply(is_left, &query)?;
        self.sync_search_bar()?;
        Ok(())
    }

    /// ↑↓・前後ボタン。現在カーソル行の次（前）から Matcher で探す。
    pub(crate) fn search_step(&self, is_left: bool, forward: bool) {
        self.incremental_step(is_left, forward);
    }

    /// 共有 1 組のバー UI をアクティブペインの状態へ同期する唯一の口。アクティブペインが
    /// `ActiveView::None` かつ `search.active` なら表示＋状態流し込み、そうでなければ隠す。
    /// 表示/非表示が変化したらレイアウトし直す。フォーカスは一切動かさない。
    pub(crate) fn sync_search_bar(&self) -> w::AnyResult<()> {
        let is_left = !self.active_right.get();
        let visible = self.search_bar_visible();
        let was_visible = self.search_bar.hwnd().IsWindowVisible();
        if visible {
            let s = self.view(is_left).state().borrow().search.clone();
            self.search_bar.show();
            self.search_bar.set_state(&s);
        } else {
            self.search_bar.hide();
        }
        if visible != was_visible {
            self.layout()?;
        }
        Ok(())
    }

    /// 検索バーを表示すべき状態か（ファイラ操作中で、アクティブペインの検索がアクティブ）。
    pub(crate) fn search_bar_visible(&self) -> bool {
        matches!(self.active_view.get(), crate::ActiveView::None)
            && self.view(!self.active_right.get()).state().borrow().search.active
    }

    /// インクリメンタルサーチの1打鍵分：先頭から現在の検索オプション（Case/Word/Regex）に
    /// 従った一致を探してカーソル移動。
    pub(crate) fn incremental_apply(&self, is_left: bool) {
        let matcher = self.view(is_left).state().borrow().search.matcher();
        self.incremental_move(is_left, 0, &matcher, true);
    }

    /// `from` から `forward` 方向へ `matcher` の一致を探し、見つかればカーソルとアンカー
    /// (`select_start`)をそこへ移して中央寄せする。原作 IncrementalSearch の一致時挙動
    /// （`SelectedIndex`＋`SelectStart` 設定＋`CenterCursor`）に合わせる。折り返さない。
    pub(crate) fn incremental_move(
        &self,
        is_left: bool,
        from: usize,
        matcher: &rerics_core::Matcher,
        forward: bool,
    ) -> bool {
        let view = self.view(is_left);
        let pr = view.page_rows();
        let found = {
            let state = view.state();
            let s = state.borrow();
            rerics_core::find_match(&s.items, from, matcher, forward, false)
        };
        if let Some(i) = found {
            {
                let state = view.state();
                let mut s = state.borrow_mut();
                s.set_cursor(i as isize, pr);
                s.select_start = i;
                s.center_cursor(pr);
            }
            let _ = view.refresh();
        }
        found.is_some()
    }

    /// 次／前の一致へ移動する（↑↓・「次(&N)」/「前(&P)」ボタン）。現在行の次（前）から、現在の
    /// 検索オプションに従って探索し、折り返さない。
    fn incremental_step(&self, is_left: bool, forward: bool) {
        let (cursor, count, matcher) = {
            let s = self.view(is_left).state();
            let s = s.borrow();
            (s.cursor, s.count(), s.search.matcher())
        };
        if forward {
            if cursor + 1 < count {
                self.incremental_move(is_left, cursor + 1, &matcher, true);
            }
        } else if cursor > 0 {
            self.incremental_move(is_left, cursor - 1, &matcher, false);
        }
    }
}
