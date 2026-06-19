use std::path::{Path, PathBuf};
use winsafe::{self as w, co, prelude::*};
use rerics_core::{Location, MacroAbort, MacroCtx, expand_macros};
use crate::{ActiveView, DialogMacroHost, MainWindow, TabSnapshot, dialog, drive_info_text, join_inner_path};

impl MainWindow {
    /// 指定 index のタブへ切替える（範囲外・現在と同じなら何もしない）。
    pub(crate) fn switch_tab(&self, index: usize) -> w::AnyResult<()> {
        if index >= self.tabs.borrow().len() || index == self.active.get() {
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
        self.reload_side_navigated(true)?;
        self.reload_side_navigated(false)?;
        Ok(())
    }

    /// カーソル行を侵入する（dir/親なら移動、file は無視）。
    pub(crate) fn activate(&self, is_left: bool, index: usize) -> w::AnyResult<()> {
        let view = self.view(is_left);
        let (is_parent, is_dir, name) = {
            let state = view.state();
            let s = state.borrow();
            let Some(it) = s.items.get(index) else {
                return Ok(());
            };
            (it.is_parent, it.is_dir, it.name.clone())
        };
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
    pub(crate) fn to_parent(&self, is_left: bool) -> w::AnyResult<()> {
        self.remember_cursor_for_nav(is_left);
        let prev = self.pane(is_left).borrow_mut().to_parent();
        let Some(prev_name) = prev else {
            return Ok(());
        };
        self.reload_side_navigated(is_left)?;
        let view = self.view(is_left);
        let pr = view.page_rows();
        {
            let state = view.state();
            let mut s = state.borrow_mut();
            s.set_cursor_position(&prev_name, pr);
            s.center_cursor(pr);
        }
        view.refresh()?;
        Ok(())
    }

    /// カレントのドライブルート（`C:\`）へ移動する。書庫内では効かない（警告のみ）。
    pub(crate) fn to_root(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではルートへ移動できません。");
            return Ok(());
        }
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
            self.reload_side_navigated(is_left)?;
        }
        Ok(())
    }

    /// 移動履歴の一覧から選んでそこへジャンプする。履歴が空なら情報ログのみ。
    pub(crate) fn path_history_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        let history = self.pane(is_left).borrow().history();
        if history.is_empty() {
            self.log.info("移動履歴がありません。");
            return Ok(());
        }
        let Some(idx) = dialog::list_box(&self.wnd, "移動履歴", &history, 0) else {
            return Ok(());
        };
        let Some(disp) = history.get(idx).cloned() else {
            return Ok(());
        };
        let loc = Location::parse(&disp);
        self.remember_cursor_for_nav(is_left);
        if self.pane(is_left).borrow_mut().navigate(loc) {
            self.reload_side_navigated(is_left)?;
        }
        Ok(())
    }

    /// パスを入力してそこへ移動する。移動できなければエラーログ。
    /// 指定パスへ移動する（引数版 `ChangeDirectory("path")`）。空や移動失敗はログのみ。
    /// パスはマクロ展開済み（`<I:>`/`<FOLDERDIALOG>` 等は呼び出し側で解決される）。
    pub(crate) fn change_directory(&self, is_left: bool, target: Option<&str>) -> w::AnyResult<()> {
        let Some(input) = target.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let loc = Location::parse(input);
        self.remember_cursor_for_nav(is_left);
        if self.pane(is_left).borrow_mut().navigate(loc) {
            self.reload_side_navigated(is_left)?;
        } else {
            self.log.error(&format!("移動できません: {input}"));
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
        if self.pane(is_left).borrow_mut().navigate(loc) {
            self.reload_side_navigated(is_left)?;
        } else {
            let line = format!("移動できません: {input}");
            self.log.error(&line);
        }
        Ok(())
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
            .map(|p| p.to_string_lossy().to_uppercase());
        let idx = roots
            .iter()
            .position(|r| Some(r.to_uppercase()) == cur)
            .unwrap_or(0) as isize;
        let n = roots.len() as isize;
        let next = roots[((idx + delta).rem_euclid(n)) as usize].clone();
        self.go_to_drive(is_left, &next)
    }

    /// アクティブペインを指定ドライブへ移す（引数版 `ChangeDrive("C:")`）。
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

    /// ドライブ一覧（容量つき）から選んでそのルートへ移動する。
    pub(crate) fn change_drive_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        let roots = w::GetLogicalDriveStrings().unwrap_or_default();
        if roots.is_empty() {
            return Ok(());
        }
        let labels: Vec<String> = roots
            .iter()
            .map(|r| {
                let info = drive_info_text(Path::new(r));
                if info.is_empty() { r.clone() } else { info }
            })
            .collect();
        let cur = self
            .pane(is_left)
            .borrow()
            .path()
            .ancestors()
            .last()
            .map(|p| p.to_string_lossy().to_uppercase());
        let initial = roots
            .iter()
            .position(|r| Some(r.to_uppercase()) == cur)
            .unwrap_or(0);
        let Some(idx) = dialog::list_box(&self.wnd, "ドライブの選択", &labels, initial) else {
            return Ok(());
        };
        let Some(root) = roots.get(idx) else {
            return Ok(());
        };
        self.go_to_drive(is_left, root)
    }

    /// 登録ディレクトリの一覧から選んでそこへジャンプする。空なら情報ログのみ。
    pub(crate) fn jump_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        let bookmarks: Vec<(String, String)> = self
            .config
            .borrow()
            .bookmarks
            .iter()
            .map(|b| (b.label.clone(), b.path.clone()))
            .collect();
        if bookmarks.is_empty() {
            self.log.info("登録ディレクトリがありません。");
            return Ok(());
        }
        let labels: Vec<String> = bookmarks
            .iter()
            .map(|(l, p)| format!("{l}  ({p})"))
            .collect();
        let Some(idx) = dialog::list_box(&self.wnd, "ジャンプ", &labels, 0) else {
            return Ok(());
        };
        let Some((_, path)) = bookmarks.get(idx) else {
            return Ok(());
        };
        let loc = Location::parse(path);
        self.remember_cursor_for_nav(is_left);
        if self.pane(is_left).borrow_mut().navigate(loc) {
            self.reload_side_navigated(is_left)?;
        } else {
            self.log.error(&format!("移動できません: {path}"));
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

    /// 引数列のマクロを展開する。文字列置換（`<C>`/`<O>`/`<P>`）に加え、ダイアログ系
    /// （`<I:>`/`<FOLDERDIALOG>`）は GUI ホスト越しにモーダルを開く。キャンセルは [`MacroAbort`]。
    pub(crate) fn expand_args(&self, is_left: bool, args: &[String]) -> Result<Vec<String>, MacroAbort> {
        let current = self.pane(is_left).borrow().loc_display();
        let opposite = self.pane(!is_left).borrow().loc_display();
        let cursor_path = {
            let st = self.view(is_left).state();
            let s = st.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent => format!("{}/{}", current, it.name),
                _ => String::new(),
            }
        };
        let selected: Vec<String> = {
            let st = self.view(is_left).state();
            let s = st.borrow();
            s.items
                .iter()
                .filter(|it| it.selected && !it.is_parent)
                .map(|it| format!("{}/{}", current, it.name))
                .collect()
        };
        let host = DialogMacroHost { app: self };
        let ctx = MacroCtx { current, opposite, cursor_path, selected, host: &host };
        expand_macros(args, &ctx)
    }

    /// 画面座標 `coords` の下にあるペインをホイール回転分だけスクロールする。
    pub(crate) fn scroll_under_cursor(&self, distance: i16, coords: w::POINT) -> w::AnyResult<()> {
        match self.active_view.get() {
            ActiveView::Text => return self.viewer.scroll_by_wheel(distance),
            ActiveView::Media => return self.media.on_wheel(distance),
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
