use winsafe::{self as w, prelude::*};
use rerics_core::{Config, FileListState, Pane, SortType};
use crate::window_state;
use crate::{MainWindow, TabSnapshot};

/// 保存状態から実タブ群を組む（ワーカースレッドで実行・GUI 非依存）。パス探索と一覧読込を
/// 含み、オフラインパスではブロックし得るので UI スレッドでは呼ばない。
fn build_restored_tabs(
    state: &rerics_core::State,
    cfg: &Config,
    home: &str,
) -> (Vec<TabSnapshot>, usize) {
    let tabs: Vec<TabSnapshot> = state
        .tabs
        .iter()
        .map(|t| {
            let left_path = crate::normalize_path(&t.left, ".");
            let right_path = crate::normalize_path(&t.right, home);
            TabSnapshot {
                left_state: MainWindow::build_state_for(
                    &left_path,
                    cfg,
                    t.sort_left,
                    t.sort_left_reverse,
                ),
                right_state: MainWindow::build_state_for(
                    &right_path,
                    cfg,
                    t.sort_right,
                    t.sort_right_reverse,
                ),
                left_path,
                right_path,
                active_right: t.active_right,
            }
        })
        .collect();
    let active = state.active_tab.min(tabs.len().saturating_sub(1));
    (tabs, active)
}

impl MainWindow {
    /// 現在のタブ群・ウィンドウ位置・分割比を state.toml へ保存する。
    /// 終了時（wm_destroy）と再起動時（Restart）の両方から呼ぶ。
    pub(crate) fn save_session_state(&self) {
        self.save_active();
        let window = window_state::capture(self.wnd.hwnd());
        let tabs: Vec<rerics_core::TabState> = self
            .tabs
            .borrow()
            .iter()
            .map(|t| rerics_core::TabState {
                left: t.left_path.clone(),
                right: t.right_path.clone(),
                active_right: t.active_right,
                sort_left: t.left_state.sort_type,
                sort_left_reverse: t.left_state.sort_reverse,
                sort_right: t.right_state.sort_type,
                sort_right_reverse: t.right_state.sort_reverse,
            })
            .collect();
        let state = rerics_core::State {
            window,
            tabs,
            active_tab: self.active.get(),
            split_ratio: self.split_ratio.get(),
        };
        if let Err(e) = state.save() {
            eprintln!("状態の保存に失敗: {}", e);
        }
    }

    /// 指定パスの一覧を読み、指定ソートでカーソル先頭の `FileListState` を組む。
    /// 列構成・日付ソート反転など config 由来のフィールドは、呼び出し元が個別に
    /// 覚えておかなくて済むよう、ここで `cfg` から一括して引き継ぐ。
    pub(crate) fn build_state_for(
        path: &str,
        cfg: &Config,
        sort_type: SortType,
        sort_reverse: bool,
    ) -> FileListState {
        let items = Pane::restore(path).read();
        let mut s = FileListState::new();
        s.columns = cfg.columns.clone();
        s.reverse_sort_date = cfg.reverse_sort_date;
        s.sort_type = sort_type;
        s.sort_reverse = sort_reverse;
        s.items = items;
        s.sort(sort_type, sort_reverse);
        s.cursor = 0;
        s.scroll_top = 0;
        s
    }

    /// 保存タブの復元をワーカーで行う。パス探索（`normalize_path`）と一覧読込
    /// （`build_state_for`）はオフライン UNC/取り外し済みドライブでは接続タイムアウトまで
    /// ブロックするため、UI スレッドから外す。完了したらタブ群を差し替えてアクティブタブを描く。
    pub(crate) fn start_restore_tabs(&self) {
        let Some(state) = self.pending_restore.borrow_mut().take() else {
            return;
        };
        let cfg = self.config.borrow().clone();
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "..".to_owned());
        self.spawn_job(
            move || build_restored_tabs(&state, &cfg, &home),
            move |mw, (tabs, active)| mw.apply_restored_tabs(tabs, active),
        );
    }

    /// 復元したタブ群をライブへ反映する。アクティブタブを描き直し、タブ帯とタイトルを更新する。
    pub(crate) fn apply_restored_tabs(
        &self,
        tabs: Vec<TabSnapshot>,
        active: usize,
    ) -> w::AnyResult<()> {
        if tabs.is_empty() {
            return Ok(());
        }
        let active = active.min(tabs.len() - 1);
        *self.tabs.borrow_mut() = tabs;
        self.active.set(active);
        let snap = self.tabs.borrow()[active].clone();
        self.load_snapshot(&snap)?;
        Ok(())
    }

    /// 現在のライブ状態を退避用スナップショットに固める。
    pub(crate) fn snapshot_live(&self) -> TabSnapshot {
        TabSnapshot {
            left_path: self.left_pane.borrow().loc_display(),
            right_path: self.right_pane.borrow().loc_display(),
            left_state: self.view(true).state().borrow().clone(),
            right_state: self.view(false).state().borrow().clone(),
            active_right: self.active_right.get(),
        }
    }

    /// スナップショットをライブ側へ反映し、再描画とフォーカス設定を行う。
    pub(crate) fn load_snapshot(&self, snap: &TabSnapshot) -> w::AnyResult<()> {
        *self.left_pane.borrow_mut() = Pane::restore(&snap.left_path);
        *self.right_pane.borrow_mut() = Pane::restore(&snap.right_path);
        *self.view(true).state().borrow_mut() = snap.left_state.clone();
        *self.view(false).state().borrow_mut() = snap.right_state.clone();
        self.set_active_pane(snap.active_right)?;
        // 旧タブで走行中の非同期読込に追い越されないよう世代を進め、残りスピナーも消す
        // （新タブの一覧はスナップショットから即復元済み）。
        for is_left in [true, false] {
            self.view(is_left).bump_load_gen();
            self.view(is_left).clear_loading();
        }
        // 起動・タブ切替の直後からアクティブ側ペインにカーソル下線を出す（反対側は消す）。
        // キー入力はキーシンクに集約するため、ペインに Win32 フォーカスを与えず可視状態だけ揃える。
        let active_is_left = !snap.active_right;
        self.view(active_is_left).set_cursor_visible(true);
        self.view(!active_is_left).set_cursor_visible(false);
        self.bar(true).set_path(&snap.left_path);
        self.bar(false).set_path(&snap.right_path);
        // per-file アイコンの基準ディレクトリを両ペインに設定（実FSのみ）。
        for is_left in [true, false] {
            let real_dir =
                self.pane(is_left).borrow().loc().as_real_path().map(|p| p.to_path_buf());
            self.view(is_left).set_dir(real_dir);
            self.arm_watch(is_left);
        }
        self.view(true).autofit_columns()?;
        self.view(false).autofit_columns()?;
        self.view(true).refresh()?;
        self.view(false).refresh()?;
        self.update_drive_info(true);
        self.update_drive_info(false);
        // 復元/タブ切替先がソリッド書庫等（非RA）なら一括展開＋スピナーを起こす。これらの
        // 経路は reload_side を通らないので、ここで明示的にトリガする（startup/タブ切替の edge）。
        for is_left in [true, false] {
            let _ = self.maybe_start_archive_extract(is_left);
        }
        self.cleanup_unreferenced_temps();
        // 切替先タブのアクティブペインの検索状態を共有検索バーへ反映する。
        let _ = self.sync_search_bar();
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// アクティブタブのスナップショットをライブから更新する。
    pub(crate) fn save_active(&self) {
        let snap = self.snapshot_live();
        let i = self.active.get();
        self.tabs.borrow_mut()[i] = snap;
    }

    /// アクティブ側ペインを切り替え、ウィンドウタイトルとタブ帯を追従させる。
    /// `active_right` を書き換える経路はここへ一本化し、呼び出し側が追従更新を
    /// 呼び忘れないようにする。
    pub(crate) fn set_active_pane(&self, active_right: bool) -> w::AnyResult<()> {
        self.active_right.set(active_right);
        self.update_title()?;
        self.refresh_tab_bar()
    }

    /// ウィンドウタイトルにアクティブタブ・アクティブペインの現在パスを反映する。
    pub(crate) fn update_title(&self) -> w::AnyResult<()> {
        let path = self.pane(!self.active_right.get()).borrow().loc_display();
        let app = concat!(
            "Rerics ",
            env!("CARGO_PKG_VERSION_MAJOR"),
            ".",
            env!("CARGO_PKG_VERSION_MINOR")
        );
        let title = if path.is_empty() { app.to_owned() } else { format!("{app} - {path}") };
        self.wnd.hwnd().SetWindowText(&title)?;
        Ok(())
    }

    /// タブ帯のラベルとアクティブ位置を更新し、再描画する。
    pub(crate) fn refresh_tab_bar(&self) -> w::AnyResult<()> {
        let active = self.active.get();
        // アクティブタブのラベルはライブのペインパスから（スナップショットは切替時のみ更新の
        // ため、同一タブ内で移動するとラベルが古くなる）。非アクティブタブはスナップショット。
        let live = {
            let p = self.pane(!self.active_right.get()).borrow();
            p.loc_display()
        };
        let labels: Vec<String> = self
            .tabs
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == active {
                    live.clone()
                } else if t.active_right {
                    t.right_path.clone()
                } else {
                    t.left_path.clone()
                }
            })
            .collect();
        self.tab_bar.set_tabs(labels, active);
        self.tab_bar.refresh()?;
        Ok(())
    }
}
