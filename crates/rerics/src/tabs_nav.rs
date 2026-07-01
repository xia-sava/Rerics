use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use winsafe::{self as w, co, gui, prelude::*};
use rerics_core::{Location, Spinner, format_size};
use crate::{ActiveView, MainWindow, TabSnapshot, dialog, join_inner_path};

impl MainWindow {
    /// 指定 index のタブへ切替える（範囲外・現在と同じなら何もしない）。
    pub(crate) fn switch_tab(&self, index: usize) -> w::AnyResult<()> {
        if index >= self.tabs.borrow().len() || index == self.active.get() {
            return Ok(());
        }
        // どちらかのペインが非同期読込中はタブ切替を抑止する（読込前の古い一覧をスナップ
        // ショットへ固めると、戻ったとき誤った内容が残り続けるため）。キー経路は exec が
        // 読込中を抑止するが、タブ帯のマウスクリックはそこを通らないのでここで揃える。
        if self.view(true).is_loading() || self.view(false).is_loading() {
            return Ok(());
        }
        self.save_active();
        self.active.set(index);
        let snap = self.tabs.borrow()[index].clone();
        self.load_snapshot(&snap)?;
        self.update_title()?;
        self.refresh_tab_bar()?;
        Ok(())
    }

    /// 次のタブへ循環移動する。
    pub(crate) fn page_next(&self) -> w::AnyResult<()> {
        let total = self.tabs.borrow().len();
        if total <= 1 {
            return Ok(());
        }
        let cur = self.active.get();
        self.switch_tab((cur + 1) % total)
    }

    /// 前のタブへ循環移動する。
    pub(crate) fn page_previous(&self) -> w::AnyResult<()> {
        let total = self.tabs.borrow().len();
        if total <= 1 {
            return Ok(());
        }
        let cur = self.active.get();
        self.switch_tab((cur + total - 1) % total)
    }

    /// 現在のパスを複製した新タブをアクティブ直後に挿入して切替える。
    pub(crate) fn new_tab(&self) -> w::AnyResult<()> {
        self.save_active();
        let left_path = self.left_pane.borrow().loc_display();
        let right_path = self.right_pane.borrow().loc_display();
        let columns = self.config.borrow().columns.clone();
        // 複製元の現在ソートを引き継ぐ（見えているままの新タブにする）。
        let (sl, slr) = {
            let st = self.view(true).state();
            let s = st.borrow();
            (s.sort_type, s.sort_reverse)
        };
        let (sr, srr) = {
            let st = self.view(false).state();
            let s = st.borrow();
            (s.sort_type, s.sort_reverse)
        };
        let snap = TabSnapshot {
            left_state: Self::build_state_for(&left_path, &columns, sl, slr),
            right_state: Self::build_state_for(&right_path, &columns, sr, srr),
            left_path,
            right_path,
            active_right: self.active_right.get(),
        };
        let index = self.active.get() + 1;
        self.tabs.borrow_mut().insert(index, snap);
        self.active.set(index);
        let snap = self.tabs.borrow()[index].clone();
        self.load_snapshot(&snap)?;
        self.update_title()?;
        self.refresh_tab_bar()?;
        Ok(())
    }

    /// 現在のタブを閉じる（最後の1枚は閉じない）。
    pub(crate) fn close_tab(&self) -> w::AnyResult<()> {
        let total = self.tabs.borrow().len();
        if total <= 1 {
            return Ok(());
        }
        let cur = self.active.get();
        self.tabs.borrow_mut().remove(cur);
        let len = self.tabs.borrow().len();
        let active = cur.min(len - 1);
        self.active.set(active);
        let snap = self.tabs.borrow()[active].clone();
        self.load_snapshot(&snap)?;
        self.update_title()?;
        self.refresh_tab_bar()?;
        Ok(())
    }

    /// 左右ペインの現在地を入れ替える（書庫内同士でも成立する）。
    pub(crate) fn swap_paths(&self) -> w::AnyResult<()> {
        self.remember_cursor_for_nav(true);
        self.remember_cursor_for_nav(false);
        let l = self.left_pane.borrow().loc().clone();
        let r = self.right_pane.borrow().loc().clone();
        self.left_pane.borrow_mut().set_loc(r);
        self.right_pane.borrow_mut().set_loc(l);
        self.reload_side_navigated_nolog(true)?;
        self.reload_side_navigated_nolog(false)?;
        Ok(())
    }

    /// カーソル行を侵入する（dir/親なら移動、file は無視）。
    pub(crate) fn activate(&self, is_left: bool, index: usize) -> w::AnyResult<()> {
        let view = self.view(is_left);
        let (is_parent, is_dir, name, find_result, source) = {
            let state = view.state();
            let s = state.borrow();
            let Some(it) = s.items.get(index) else {
                return Ok(());
            };
            (it.is_parent, it.is_dir, it.name.clone(), s.find_result, it.source.clone())
        };
        // 結果一覧（検索・比較）の項目を開く：その項目の出自ディレクトリへ移動してカーソルを
        // 名前に合わせ、結果モードを抜ける。先頭の ".." は基準ディレクトリへ戻る（再読込で解除）。
        if find_result {
            if is_parent {
                return self.reload_side(is_left);
            }
            let target = source.unwrap_or_else(|| self.pane(is_left).borrow().loc().clone());
            self.remember_cursor_for_nav(is_left);
            let moved = self.pane(is_left).borrow_mut().navigate(target);
            if moved {
                self.record_visit(is_left);
                self.reload_side_focus(is_left, &name, true)?;
            } else {
                self.reload_side(is_left)?;
            }
            return Ok(());
        }
        if is_parent {
            return self.to_parent(is_left);
        }
        // ディレクトリ/書庫へ潜る前に、今のカーソル位置を覚えておく（再訪時に復元）。
        self.remember_cursor_for_nav(is_left);
        if is_dir {
            if self.pane(is_left).borrow_mut().enter(&name, is_dir) {
                self.reload_side_navigated(is_left)?;
            }
        } else {
            // 書庫ファイルなら潜る（zip 等）。
            if self.pane(is_left).borrow_mut().enter(&name, is_dir) {
                self.reload_side_navigated(is_left)?;
                return Ok(());
            }
            // 開く対象の実パスを得る（書庫内は一時展開してから関連付け起動）。
            let loc = self.pane(is_left).borrow().loc().clone();
            let path = match loc {
                Location::Real(dir) => dir.join(&name),
                Location::Archive { archive, inner } => {
                    let inner_file = join_inner_path(&inner, &name);
                    let pw = self.ensure_media_password(&archive, Some(&inner_file));
                    match self.resolve_archive_file(&archive, &inner_file, &name, pw.as_deref()) {
                        Ok(p) => p,
                        Err(e) => {
                            self.log
                                .error(&format!("書庫内ファイルを展開できません: {}: {}", name, e));
                            return Ok(());
                        }
                    }
                }
            };
            if let Err(e) =
                self.wnd
                    .hwnd()
                    .ShellExecute("open", &path.to_string_lossy(), None, None, co::SW::SHOWNORMAL)
            {
                self.log
                    .error(&format!("ファイルを開けません: {}: {}", name, e));
            }
        }
        Ok(())
    }

    /// 親ディレクトリへ移動し、元ディレクトリ名にカーソルを置きセンタリングする。
    /// 検索・比較の結果一覧では、検索を開始したディレクトリへ戻る（結果モードを抜けて
    /// 基準ディレクトリを再表示する＝実際の親へは移動しない）。
    pub(crate) fn to_parent(&self, is_left: bool) -> w::AnyResult<()> {
        if self.view(is_left).state().borrow().find_result {
            return self.reload_side(is_left);
        }
        self.remember_cursor_for_nav(is_left);
        let prev = self.pane(is_left).borrow_mut().to_parent();
        let Some(prev_name) = prev else {
            return Ok(());
        };
        self.record_visit(is_left);
        self.reload_side_focus(is_left, &prev_name, true)?;
        Ok(())
    }

    /// カレントのドライブルート（`C:\`）へ移動する。書庫内なら書庫のあるドライブのルートへ抜ける。
    pub(crate) fn to_root(&self, is_left: bool) -> w::AnyResult<()> {
        let root = self.pane(is_left).borrow().loc().to_root();
        let Some(root) = root else {
            return Ok(());
        };
        self.remember_cursor_for_nav(is_left);
        if self.pane(is_left).borrow_mut().navigate(root) {
            self.reload_side_navigated(is_left)?;
        }
        Ok(())
    }

    /// パス移動履歴を前後する（`forward`=進む / それ以外=戻る）。移動できたら再読込。
    pub(crate) fn history_move(&self, is_left: bool, forward: bool) -> w::AnyResult<()> {
        self.remember_cursor_for_nav(is_left);
        let moved = {
            let mut p = self.pane(is_left).borrow_mut();
            if forward { p.go_forward() } else { p.go_back() }
        };
        if moved {
            self.reload_side_history(is_left)?;
        }
        Ok(())
    }

    /// パス移動履歴（訪問ログ＝グローバル・永続・新しい順）から選んでそこへジャンプする。
    /// 履歴が空なら情報ログのみ。原作 PathHistoryDialog 相当。
    pub(crate) fn path_history_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        let history = rerics_core::InputHistory::load().get(rerics_core::PATH_HISTORY_KEY);
        if history.is_empty() {
            self.log.info("移動履歴がありません。");
            return Ok(());
        }
        let Some(idx) = dialog::list_box(&self.wnd, "移動履歴", "history", &history, 0) else {
            return Ok(());
        };
        let Some(disp) = history.get(idx).cloned() else {
            return Ok(());
        };
        let loc = Location::parse(&disp);
        self.remember_cursor_for_nav(is_left);
        let outcome = self.pane(is_left).borrow_mut().navigate_reported(loc);
        match outcome {
            Ok(()) => self.reload_side_navigated(is_left)?,
            Err(e) => self.report_change_directory_error(&e),
        }
        Ok(())
    }

    /// 指定パスへ移動する（引数版 `changeDirectory("path")`）。空や移動失敗はログのみ。
    /// `target` は解決済みのパス（式 `=r.folderDialog()` 等は呼び出し側で評価される）。
    pub(crate) fn change_directory(&self, is_left: bool, target: Option<&str>) -> w::AnyResult<()> {
        let Some(input) = target.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let loc = Location::parse(input);
        self.remember_cursor_for_nav(is_left);
        // navigate_reported の RefMut はこの行で解放してから（reload が同じ pane を再借用するため）
        // 結果を判定する。match の scrutinee に直接書くと借用が match 末尾まで延命して panic する。
        let outcome = self.pane(is_left).borrow_mut().navigate_reported(loc);
        match outcome {
            Ok(()) => self.reload_side_navigated(is_left)?,
            Err(e) => self.report_change_directory_error(&e),
        }
        Ok(())
    }

    pub(crate) fn change_directory_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        let current = self.pane(is_left).borrow().loc_display();
        let Some(input) =
            self.input_with_history("ディレクトリ移動", "移動先のパスを入力して下さい。", &current, "changedir")
        else {
            return Ok(());
        };
        let input = input.trim();
        if input.is_empty() {
            return Ok(());
        }
        let loc = Location::parse(input);
        self.remember_cursor_for_nav(is_left);
        let outcome = self.pane(is_left).borrow_mut().navigate_reported(loc);
        match outcome {
            Ok(()) => self.reload_side_navigated(is_left)?,
            Err(e) => self.report_change_directory_error(&e),
        }
        Ok(())
    }

    /// `changeDirectory` の失敗をログ＋エラーダイアログで報せる（原作 `NotExistsDirectory` /
    /// `ChangeDirectoryError` 相当）。存在しない場合と、それ以外（権限不足等）で文言を分ける。
    fn report_change_directory_error(&self, err: &std::io::Error) {
        let msg = change_directory_error_message(err);
        self.log.error(&msg);
        dialog::message_box(&self.wnd, "ディレクトリ移動", &msg, dialog::MessageStyle::Error);
    }

    /// 指定ドライブのルート文字列（`C:\` 形式）へ移す共通口。カーソル履歴が有効なら、
    /// そのドライブで前回居たディレクトリ＋カーソル位置を復元し、無ければ（またはその
    /// 場所が読めなくなっていれば）ルートへ移る。移動は履歴（戻る/進む）に積む。
    fn go_to_drive(&self, is_left: bool, root: &str) -> w::AnyResult<()> {
        self.remember_cursor_for_nav(is_left);
        let recalled: Option<String> = if self.config.borrow().cursor.history {
            self.pane(is_left)
                .borrow()
                .recalled_drive_dir(root)
                .map(str::to_owned)
        } else {
            None
        };
        {
            let mut pane = self.pane(is_left).borrow_mut();
            let moved = recalled
                .map(|p| pane.navigate(Location::parse(&p)))
                .unwrap_or(false);
            if !moved {
                pane.navigate(Location::Real(PathBuf::from(root)));
            }
        }
        self.reload_side_navigated(is_left)?;
        Ok(())
    }

    /// アクティブペインを次/前のドライブへ移す（`delta` は +1/-1、巡回）。
    pub(crate) fn change_drive(&self, is_left: bool, delta: isize) -> w::AnyResult<()> {
        let roots = w::GetLogicalDriveStrings().unwrap_or_default();
        if roots.is_empty() {
            return Ok(());
        }
        let cur = self
            .pane(is_left)
            .borrow()
            .path()
            .ancestors()
            .last()
            .map(|p| p.to_string_lossy().to_ascii_uppercase());
        let idx = roots
            .iter()
            .position(|r| Some(r.to_ascii_uppercase()) == cur)
            .unwrap_or(0);
        // 準備未了（空の光学/リムーバブル・切断ネットワーク等）は巡回対象から外す。
        let Some(next) = next_ready_index(roots.len(), idx, delta, |i| drive_ready(&roots[i]))
        else {
            return Ok(());
        };
        let next = roots[next].clone();
        self.go_to_drive(is_left, &next)
    }

    /// アクティブペインを指定ドライブへ移す（引数版 `changeDrive("C:")`）。
    /// 引数は `C` / `C:` / `C:\` のいずれでも可。空や不正は何もしない。
    pub(crate) fn change_drive_to(&self, is_left: bool, drive: Option<&str>) -> w::AnyResult<()> {
        let Some(d) = drive.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let Some(letter) = d.chars().next().filter(|c| c.is_ascii_alphabetic()) else {
            return Ok(());
        };
        let root = format!("{}:\\", letter.to_ascii_uppercase());
        self.go_to_drive(is_left, &root)
    }

    /// ドライブ一覧（多列）から選んでそのルートへ移動する。一覧はドライブ名・種類だけ
    /// 即座に出し、ボリューム名・容量（取得が未了ドライブで遅い）は各ドライブを別スレッドで
    /// probe して帰った順に各セルへ後追いで埋める（押した瞬間に出て、UI は固まらない）。
    pub(crate) fn change_drive_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        let roots = w::GetLogicalDriveStrings().unwrap_or_default();
        if roots.is_empty() {
            return Ok(());
        }
        let cur = self
            .pane(is_left)
            .borrow()
            .path()
            .ancestors()
            .last()
            .map(|p| p.to_string_lossy().to_ascii_uppercase());
        let initial = roots
            .iter()
            .position(|r| Some(r.to_ascii_uppercase()) == cur)
            .unwrap_or(0);

        let (wnd, arm) =
            crate::dialog::modal_window_resizable_keyed("ドライブの選択", "drive", 472, 320, 360, 240);
        let list = gui::ListView::<()>::new(
            &wnd,
            gui::ListViewOpts {
                position: gui::dpi(12, 12),
                size: gui::dpi(448, 252),
                // 1 行だけ選べる単一選択。既定（REPORT/NOSORTHEADER/SHOWSELALWAYS）に追加する。
                control_style: co::LVS::REPORT
                    | co::LVS::NOSORTHEADER
                    | co::LVS::SHOWSELALWAYS
                    | co::LVS::SINGLESEL,
                control_ex_style: co::LVS_EX::FULLROWSELECT,
                ..Default::default()
            },
        );
        let ok = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "OK",
                control_style: co::BS::DEFPUSHBUTTON,
                ctrl_id: 1,
                position: gui::dpi(284, 278),
                width: gui::dpi_x(80),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );
        let cancel = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "中止(&S)",
                ctrl_id: 2,
                position: gui::dpi(372, 278),
                width: gui::dpi_x(86),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );

        // リサイズ追従：一覧を広げ OK/中止を右下へ。最小サイズも抑える。
        {
            let wndc = wnd.clone();
            let (lst, okc, cancelc) = (list.clone(), ok.clone(), cancel.clone());
            wnd.on().wm_size(move |_| {
                if let Ok(rc) = wndc.hwnd().GetClientRect() {
                    crate::dialog::relayout_list_dialog(
                        lst.hwnd(),
                        12,
                        26,
                        &[(cancelc.hwnd(), 86), (okc.hwnd(), 80)],
                        rc.right,
                        rc.bottom,
                    );
                }
                Ok(())
            });
        }

        let result: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
        // モーダルが閉じた後に届く遅延 fill を捨てるための生存フラグ。
        let alive = Rc::new(Cell::new(true));
        // probe 中の行（true）。ボリューム列のスピナー表示に使う（帰ったら false）。
        let pending = Rc::new(RefCell::new(vec![false; roots.len()]));
        let spinner = Rc::new(RefCell::new(Spinner::immediate()));

        {
            let list = list.clone();
            let roots_c = roots.clone();
            let me = self.clone();
            let alive_c = alive.clone();
            let pending_c = pending.clone();
            arm.on_create(move |_| {
                for (head, width) in [
                    ("ドライブ", 64),
                    ("ボリューム", 150),
                    ("種類", 104),
                    ("空き容量", 112),
                    ("合計容量", 112),
                ] {
                    list.cols().add(head, gui::dpi_x(width))?;
                }
                // A/B のフロッピーは無駄に回さないため名前のみ・probe しない（原作準拠）。
                for r in &roots_c {
                    let ty = if is_ab_floppy(r) { "" } else { drive_type_label(r) };
                    list.items()
                        .add(&[drive_letter(r), String::new(), ty.to_owned(), String::new(), String::new()], None, ())?;
                }
                if let Some(it) = list.items().iter().nth(initial) {
                    it.select(true)?;
                    it.focus()?;
                }
                list.hwnd().SetFocus();
                for (i, r) in roots_c.iter().enumerate() {
                    if is_ab_floppy(r) {
                        continue;
                    }
                    pending_c.borrow_mut()[i] = true;
                    let root = r.clone();
                    let list2 = list.clone();
                    let alive2 = alive_c.clone();
                    let pending2 = pending_c.clone();
                    me.spawn_job(
                        move || probe_drive_info(&root),
                        move |_mw, (vol, free, total)| {
                            pending2.borrow_mut()[i] = false;
                            if alive2.get()
                                && let Some(it) = list2.items().iter().nth(i) {
                                    let _ = it.set_text(1, &vol);
                                    let _ = it.set_text(3, &free);
                                    let _ = it.set_text(4, &total);
                                }
                            Ok(())
                        },
                    );
                }
                // ボリューム取得中の行に回すスピナー（モーダル窓ローカルのタイマ）。
                if let Ok(modal) = list.hwnd().GetParent() {
                    let _ = modal.SetTimer(SPIN_TIMER_ID, 110, None);
                }
                Ok(())
            });
        }
        #[cfg(feature = "debug-server")]
        {
            let list_r = list.clone();
            let list_s = list.clone();
            arm.list_view(
                "drive",
                "ドライブの選択",
                vec![("OK".to_owned(), 1u16), ("中止(&S)".to_owned(), 2u16)],
                crate::debug_server::modal_registry::ListViewHooks {
                    headers: ["ドライブ", "ボリューム", "種類", "空き容量", "合計容量"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    read: Box::new(move || {
                        let rows = list_r
                            .items()
                            .iter()
                            .map(|it| (0..5u32).map(|c| it.text(c)).collect())
                            .collect();
                        let sel = list_r.items().iter().position(|it| it.is_selected()).unwrap_or(0);
                        (rows, sel)
                    }),
                    select: Box::new(move |idx| {
                        if let Some(it) = list_s.items().iter().nth(idx) {
                            let _ = it.select(true);
                            let _ = it.focus();
                        }
                    }),
                },
            );
        }
        {
            // probe 中の行のボリューム列でスピナーを回す。
            let list2 = list.clone();
            let pending2 = pending.clone();
            let spinner2 = spinner.clone();
            wnd.on().wm_timer(SPIN_TIMER_ID, move || {
                let glyph = {
                    let mut s = spinner2.borrow_mut();
                    s.tick();
                    s.glyph()
                };
                for (i, busy) in pending2.borrow().iter().enumerate() {
                    if *busy
                        && let Some(it) = list2.items().iter().nth(i) {
                            let _ = it.set_text(1, glyph);
                        }
                }
                Ok(())
            });
        }
        {
            // ドライブ文字キーで、その行が一意に決まればそのまま選択＋移動（原作準拠）。
            let result = result.clone();
            let list2 = list.clone();
            let wnd2 = wnd.clone();
            list.on().lvn_key_down(move |p| {
                let raw = p.wVKey.raw();
                if (0x41..=0x5A).contains(&raw) {
                    let ch = raw as u8 as char;
                    let mut hit = None;
                    let mut multi = false;
                    for (i, it) in list2.items().iter().enumerate() {
                        if it.text(0).to_ascii_uppercase().starts_with(ch) {
                            if hit.is_some() {
                                multi = true;
                                break;
                            }
                            hit = Some(i);
                        }
                    }
                    if let (Some(idx), false) = (hit, multi) {
                        *result.borrow_mut() = Some(idx);
                        wnd2.close();
                    }
                }
                Ok(())
            });
        }
        {
            let result = result.clone();
            let wnd2 = wnd.clone();
            list.on().lvn_item_activate(move |p| {
                if p.iItem >= 0 {
                    *result.borrow_mut() = Some(p.iItem as usize);
                }
                wnd2.close();
                Ok(())
            });
        }
        {
            let result = result.clone();
            let list2 = list.clone();
            let wnd2 = wnd.clone();
            ok.on().bn_clicked(move || {
                if let Some(idx) = list2.items().iter().position(|it| it.is_selected()) {
                    *result.borrow_mut() = Some(idx);
                }
                wnd2.close();
                Ok(())
            });
        }
        {
            let wnd2 = wnd.clone();
            cancel.on().bn_clicked(move || {
                wnd2.close();
                Ok(())
            });
        }

        self.in_dialog.set(true);
        let _ = wnd.show_modal(&self.wnd);
        self.in_dialog.set(false);
        alive.set(false);
        let _ = (ok, cancel, list);

        let sel = *result.borrow();
        if let Some(root) = sel.and_then(|idx| roots.get(idx).cloned()) {
            self.go_to_drive(is_left, &root)?;
        }
        Ok(())
    }

    /// 登録ディレクトリの一覧から選んでそこへジャンプする（原作 RegisteredPathDialog／
    /// JumpDialog）。ショートカット/名前/場所の 3 列で表示し、行のショートカットキーを押すと
    /// そのまま移動する。空なら情報ログのみ。
    pub(crate) fn jump_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        // (shortcut, label, path) を表示順に取り出す。
        let entries: Vec<(String, String, String)> = self
            .config
            .borrow()
            .bookmarks
            .iter()
            .map(|b| (b.shortcut.clone(), b.label.clone(), b.path.clone()))
            .collect();
        if entries.is_empty() {
            self.log.info("登録ディレクトリがありません。");
            return Ok(());
        }

        let (wnd, arm) =
            crate::dialog::modal_window_resizable_keyed("登録ディレクトリ", "jump", 600, 360, 400, 260);
        let list = gui::ListView::<()>::new(
            &wnd,
            gui::ListViewOpts {
                position: gui::dpi(12, 12),
                size: gui::dpi(576, 292),
                control_style: co::LVS::REPORT
                    | co::LVS::NOSORTHEADER
                    | co::LVS::SHOWSELALWAYS
                    | co::LVS::SINGLESEL,
                control_ex_style: co::LVS_EX::FULLROWSELECT,
                ..Default::default()
            },
        );
        let ok = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "OK",
                control_style: co::BS::DEFPUSHBUTTON,
                ctrl_id: 1,
                position: gui::dpi(412, 318),
                width: gui::dpi_x(80),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );
        let cancel = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "中止(&S)",
                ctrl_id: 2,
                position: gui::dpi(500, 318),
                width: gui::dpi_x(86),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );

        // リサイズ追従：一覧を広げ OK/中止を右下へ。最小サイズも抑える。
        {
            let wndc = wnd.clone();
            let (lst, okc, cancelc) = (list.clone(), ok.clone(), cancel.clone());
            wnd.on().wm_size(move |_| {
                if let Ok(rc) = wndc.hwnd().GetClientRect() {
                    crate::dialog::relayout_list_dialog(
                        lst.hwnd(),
                        12,
                        26,
                        &[(cancelc.hwnd(), 86), (okc.hwnd(), 80)],
                        rc.right,
                        rc.bottom,
                    );
                }
                Ok(())
            });
        }

        let result: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));

        {
            let list = list.clone();
            let entries_c = entries.clone();
            arm.on_create(move |_| {
                for (head, width) in [("", 44), ("名前", 180), ("場所", 330)] {
                    list.cols().add(head, gui::dpi_x(width))?;
                }
                for (sc, name, path) in &entries_c {
                    list.items().add(&[sc.clone(), name.clone(), path.clone()], None, ())?;
                }
                if let Some(it) = list.items().iter().next() {
                    it.select(true)?;
                    it.focus()?;
                }
                list.hwnd().SetFocus();
                Ok(())
            });
        }
        #[cfg(feature = "debug-server")]
        {
            let list_r = list.clone();
            let list_s = list.clone();
            arm.list_view(
                "jump",
                "登録ディレクトリ",
                vec![("OK".to_owned(), 1u16), ("中止(&S)".to_owned(), 2u16)],
                crate::debug_server::modal_registry::ListViewHooks {
                    headers: ["ショートカット", "名前", "場所"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    read: Box::new(move || {
                        let rows = list_r
                            .items()
                            .iter()
                            .map(|it| (0..3u32).map(|c| it.text(c)).collect())
                            .collect();
                        let sel = list_r.items().iter().position(|it| it.is_selected()).unwrap_or(0);
                        (rows, sel)
                    }),
                    select: Box::new(move |idx| {
                        if let Some(it) = list_s.items().iter().nth(idx) {
                            let _ = it.select(true);
                            let _ = it.focus();
                        }
                    }),
                },
            );
        }
        {
            // ショートカットキーを押したら、その行が一意に決まればそのまま選択＋移動。
            let result = result.clone();
            let entries_c = entries.clone();
            let wnd2 = wnd.clone();
            list.on().lvn_key_down(move |p| {
                let raw = p.wVKey.raw();
                if (0x41..=0x5A).contains(&raw) {
                    let ch = raw as u8 as char; // 'A'..'Z'
                    let idx = unique_shortcut_index(entries_c.iter().map(|(s, _, _)| s.as_str()), ch);
                    if let Some(idx) = idx {
                        *result.borrow_mut() = Some(idx);
                        wnd2.close();
                    }
                }
                Ok(())
            });
        }
        {
            let result = result.clone();
            let wnd2 = wnd.clone();
            list.on().lvn_item_activate(move |p| {
                if p.iItem >= 0 {
                    *result.borrow_mut() = Some(p.iItem as usize);
                }
                wnd2.close();
                Ok(())
            });
        }
        {
            let result = result.clone();
            let list2 = list.clone();
            let wnd2 = wnd.clone();
            ok.on().bn_clicked(move || {
                if let Some(idx) = list2.items().iter().position(|it| it.is_selected()) {
                    *result.borrow_mut() = Some(idx);
                }
                wnd2.close();
                Ok(())
            });
        }
        {
            let wnd2 = wnd.clone();
            cancel.on().bn_clicked(move || {
                wnd2.close();
                Ok(())
            });
        }

        self.in_dialog.set(true);
        let _ = wnd.show_modal(&self.wnd);
        self.in_dialog.set(false);
        let _ = (ok, cancel, list);

        let sel = *result.borrow();
        if let Some((_, _, path)) = sel.and_then(|idx| entries.get(idx)) {
            let loc = Location::parse(path);
            self.remember_cursor_for_nav(is_left);
            let outcome = self.pane(is_left).borrow_mut().navigate_reported(loc);
            match outcome {
                Ok(()) => self.reload_side_navigated(is_left)?,
                Err(e) => self.report_change_directory_error(&e),
            }
        }
        Ok(())
    }

    /// 現在地を登録ディレクトリ（ブックマーク）に追加する。登録名を尋ね、config に保存。
    pub(crate) fn register_path(&self, is_left: bool) -> w::AnyResult<()> {
        let path = self.pane(is_left).borrow().loc_display();
        let default_label = Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let Some(label) =
            self.input_with_history("ディレクトリの登録", "登録名を入力して下さい。", &default_label, "register")
        else {
            return Ok(());
        };
        let label = label.trim();
        if label.is_empty() {
            return Ok(());
        }
        {
            let mut cfg = self.config.borrow_mut();
            cfg.add_bookmark(label, &path);
            let _ = cfg.save();
        }
        self.log.normal(&format!("登録しました: {label}"));
        Ok(())
    }


    /// 画面座標 `coords` の下にあるペインをホイール回転分だけスクロールする。
    pub(crate) fn scroll_under_cursor(&self, distance: i16, coords: w::POINT) -> w::AnyResult<()> {
        match self.active_view.get() {
            ActiveView::Text => return self.viewer.scroll_by_wheel(distance),
            ActiveView::Media => {
                // ホイール動作は設定で切替（既定＝送り・原作准拠／上=前・下=次）。
                return match self.config.borrow().image.wheel {
                    rerics_core::WheelAction::Zoom => self.media.on_wheel(distance),
                    rerics_core::WheelAction::Navigate => {
                        self.media.navigate(if distance > 0 { -1 } else { 1 })
                    }
                };
            }
            ActiveView::None => {}
        }
        if let Some(hw) = w::HWND::WindowFromPoint(coords) {
            let p = hw.ptr();
            if p == self.view(true).hwnd().ptr() {
                self.view(true).scroll_by_wheel(distance)?;
            } else if p == self.view(false).hwnd().ptr() {
                self.view(false).scroll_by_wheel(distance)?;
            } else if p == self.log.hwnd().ptr() {
                self.log.scroll_by_wheel(distance)?;
            }
        }
        Ok(())
    }
}

/// `roots` を `delta`（+1/-1）方向に巡回し、`cur` 以外で最初に `ready` を満たす index を返す。
/// 他に対象が無ければ（全て未了・ドライブが1つだけ等）None。
/// `changeDirectory` 失敗時のダイアログ文言（原作 `NotExistsDirectory` / `ChangeDirectoryError`）。
/// 移動先が存在しないなら専用文、それ以外（権限不足・読込エラー等）は原因付きで報せる。
fn change_directory_error_message(err: &std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::NotFound {
        "ディレクトリが存在しません。".to_owned()
    } else {
        format!("ディレクトリが変更出来ません。\n原因：{err}")
    }
}

/// 登録ディレクトリのショートカット集合から、押されたキー `ch`（大文字）に一致する行が
/// ただ一つのときだけその index を返す（未一致・複数一致は None＝確定させない）。
/// 一致判定はショートカット先頭1文字を大文字化して比較する。
fn unique_shortcut_index<'a>(shortcuts: impl Iterator<Item = &'a str>, ch: char) -> Option<usize> {
    let mut hit = None;
    for (i, sc) in shortcuts.enumerate() {
        if sc.chars().next().map(|c| c.to_ascii_uppercase()) == Some(ch) {
            if hit.is_some() {
                return None;
            }
            hit = Some(i);
        }
    }
    hit
}

fn next_ready_index(n: usize, cur: usize, delta: isize, ready: impl Fn(usize) -> bool) -> Option<usize> {
    (1..n)
        .map(|step| (cur as isize + delta * step as isize).rem_euclid(n as isize) as usize)
        .find(|&i| ready(i))
}

/// ドライブのルートが今アクセス可能か（容量取得の成否で判定）。空の光学/リムーバブルや
/// 切断ネットワークは失敗＝準備未了とみなす。
fn drive_ready(root: &str) -> bool {
    let mut free = 0u64;
    let mut total = 0u64;
    w::GetDiskFreeSpaceEx(Some(root), Some(&mut free), Some(&mut total), None).is_ok()
}

/// ドライブ選択ダイアログのスピナー用タイマ ID（モーダル窓ローカルなので衝突しない）。
const SPIN_TIMER_ID: usize = 0xD2;

/// ルート文字列（`C:\`）のドライブ表記（`C:`）。
fn drive_letter(root: &str) -> String {
    root.trim_end_matches(['\\', '/']).to_uppercase()
}

/// ドライブ種別の表示ラベル（メディアに触れない `GetDriveType` なので即時に得られる）。
fn drive_type_label(root: &str) -> &'static str {
    match w::GetDriveType(Some(root)) {
        co::DRIVE::FIXED => "固定",
        co::DRIVE::REMOVABLE => "リムーバブル",
        co::DRIVE::CDROM => "CD-ROM",
        co::DRIVE::REMOTE => "ネットワーク",
        co::DRIVE::RAMDISK => "RAM ディスク",
        _ => "",
    }
}

/// A: / B: のリムーバブル（フロッピー）か。無駄にドライブを回さないよう probe 対象から外す。
fn is_ab_floppy(root: &str) -> bool {
    matches!(root.chars().next().map(|c| c.to_ascii_uppercase()), Some('A') | Some('B'))
        && w::GetDriveType(Some(root)) == co::DRIVE::REMOVABLE
}

/// ドライブの遅い情報（ボリューム名・空き/合計容量）を probe する（ワーカースレッドで実行）。
/// 準備未了（空の光学/リムーバブル等）では各取得が失敗するので、「probe は完了したが
/// 情報なし」と分かるよう各セルを `--` で返す（空欄＝未取得 と区別する）。
fn probe_drive_info(root: &str) -> (String, String, String) {
    let mut free = 0u64;
    let mut total = 0u64;
    if w::GetDiskFreeSpaceEx(Some(root), Some(&mut free), Some(&mut total), None).is_err() {
        let dash = || "--".to_owned();
        return (dash(), dash(), dash());
    }
    let mut volume = String::new();
    let _ = w::GetVolumeInformation(Some(root), Some(&mut volume), None, None, None, None);
    (volume, format_size(free), format_size(total))
}

#[cfg(test)]
mod tests {
    use super::{change_directory_error_message, next_ready_index, unique_shortcut_index};

    #[test]
    fn shortcut_index_matches_unique_case_insensitively() {
        let scs = ["G", "d", "", "M"];
        let pick = |ch| unique_shortcut_index(scs.iter().copied(), ch);
        assert_eq!(pick('G'), Some(0)); // 大文字一致
        assert_eq!(pick('D'), Some(1)); // 小文字割当でも大文字キーで一致
        assert_eq!(pick('M'), Some(3));
        assert_eq!(pick('X'), None); // 未割当
    }

    #[test]
    fn shortcut_index_rejects_duplicates_and_empty() {
        // 同じショートカットが複数あれば確定させない。
        assert_eq!(unique_shortcut_index(["A", "a"].iter().copied(), 'A'), None);
        // 空ショートカットはどのキーにも一致しない。
        assert_eq!(unique_shortcut_index(["", ""].iter().copied(), 'A'), None);
    }

    #[test]
    fn change_directory_error_distinguishes_not_found() {
        use std::io::{Error, ErrorKind};
        let not_found = change_directory_error_message(&Error::from(ErrorKind::NotFound));
        assert_eq!(not_found, "ディレクトリが存在しません。");

        let denied = change_directory_error_message(&Error::new(ErrorKind::PermissionDenied, "アクセスが拒否されました"));
        assert!(denied.starts_with("ディレクトリが変更出来ません。"));
        assert!(denied.contains("原因："));
    }

    #[test]
    fn skips_not_ready_in_both_directions() {
        // 4 ドライブ、index 1 と 3 が未了（0 と 2 のみ ready）。
        let ready = |i: usize| i == 0 || i == 2;
        assert_eq!(next_ready_index(4, 0, 1, ready), Some(2)); // 0→(1飛ばし)→2
        assert_eq!(next_ready_index(4, 0, -1, ready), Some(2)); // 0→(3飛ばし・巡回)→2
        assert_eq!(next_ready_index(4, 2, 1, ready), Some(0)); // 2→(3飛ばし)→0
    }

    #[test]
    fn none_when_no_other_ready() {
        assert_eq!(next_ready_index(3, 0, 1, |i| i == 0), None); // 自分だけ ready
        assert_eq!(next_ready_index(1, 0, 1, |_| true), None); // ドライブ1つだけ
    }
}
