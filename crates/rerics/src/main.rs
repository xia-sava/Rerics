mod chrome;
mod dialog;
mod file_list;
mod log_view;
mod menu;
mod pane_view;
mod path_bar;
mod settings_dialog;
mod splitter;
mod status_bar;
mod tab_bar;
mod task;
mod task_manager;
mod window_state;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use file_list::FileListView;
use log_view::LogView;
use pane_view::PaneView;
use path_bar::PathBarView;
use status_bar::StatusBarView;
use tab_bar::TabBar;
use task::{ChannelHost, OpKind, TaskControl, TaskEntry, WorkerEvent};
use rerics_core::{
    Column, Command, Config, FileListState, KeyChord, KeyMap, LogLevel, Pane, SortType,
    WindowState, messages,
};
use winsafe::{self as w, co, gui, prelude::*};

/// 表示完了後に最大化を実行させるための自前メッセージ（`WM_APP`）。
fn wm_restore_maximize() -> co::WM {
    unsafe { co::WM::from_raw(0x8000) }
}

fn main() {
    if let Err(e) = MainWindow::new().run() {
        eprintln!("エラー: {}", e);
    }
}

/// OS のアプリ配色がライトかどうかを返す（レジストリ `AppsUseLightTheme`）。
/// 値が取れない場合は Windows 標準のライト扱いにフォールバックする。
fn system_is_light() -> bool {
    use std::ffi::c_void;
    // winsafe 0.0.27 にレジストリ読み出しが無いため advapi32 を直接叩く。
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            hkey: *mut c_void,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            pdw_type: *mut u32,
            pv_data: *mut c_void,
            pcb_data: *mut u32,
        ) -> i32;
    }
    // HKEY_CURRENT_USER は (LONG)0x80000001 を符号拡張したハンドル定数。
    let hkcu = 0x80000001u32 as i32 as isize as usize as *mut c_void;
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
    let sub_key: Vec<u16> =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
            .encode_utf16()
            .collect();
    let value: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
    let mut data: u32 = 1;
    let mut cb: u32 = 4;
    let rc = unsafe {
        RegGetValueW(
            hkcu,
            sub_key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut cb,
        )
    };
    if rc == 0 { data != 0 } else { true }
}

#[derive(Clone)]
struct MainWindow {
    wnd: gui::WindowMain,
    left: PaneView,
    right: PaneView,
    splitter: splitter::SplitterView,
    tab_bar: TabBar,
    log: LogView,
    key_sink: gui::WindowControl,
    menu_bar: Rc<w::HMENU>,
    menu_cmds: Rc<std::collections::HashMap<u16, Command>>,
    config: Rc<RefCell<Config>>,
    left_pane: Rc<RefCell<Pane>>,
    right_pane: Rc<RefCell<Pane>>,
    keymap: Rc<RefCell<KeyMap>>,
    initial_window: Option<WindowState>,
    active_right: Rc<Cell<bool>>,
    /// 左ペインの幅比（0.0〜1.0）。スプリッタのドラッグ／最大化／境界移動で変わる。
    split_ratio: Rc<Cell<f64>>,
    /// いずれかのペインが最大化中か（Maximize トグルの状態・原作 MaximizeFileList 相当）。
    maximized: Rc<Cell<bool>>,
    tabs: Rc<RefCell<Vec<TabSnapshot>>>,
    active: Rc<Cell<usize>>,
    left_mask: Rc<RefCell<Option<String>>>,
    right_mask: Rc<RefCell<Option<String>>>,
    task_tx: Sender<WorkerEvent>,
    task_rx: Rc<Receiver<WorkerEvent>>,
    tasks: Rc<RefCell<Vec<TaskEntry>>>,
    next_task_id: Rc<Cell<u64>>,
    progress_seq: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    in_dialog: Rc<Cell<bool>>,
}

/// 1タブの保存状態（非アクティブ時の退避先）。アクティブタブの実体はライブ側
/// （left_pane/right_pane/ビューの state/active_right）にあり、ここは切替時に出し入れする。
#[derive(Clone)]
struct TabSnapshot {
    left_path: String,
    right_path: String,
    left_state: FileListState,
    right_state: FileListState,
    active_right: bool,
}

impl MainWindow {
    fn new() -> Self {
        let wnd = gui::WindowMain::new(gui::WindowMainOpts {
            title: "Rerics",
            size: gui::dpi(960, 560),
            style: co::WS::CAPTION
                | co::WS::SYSMENU
                | co::WS::CLIPCHILDREN
                | co::WS::BORDER
                | co::WS::VISIBLE
                | co::WS::SIZEBOX
                | co::WS::MINIMIZEBOX
                | co::WS::MAXIMIZEBOX,
            process_dlg_msgs: false,
            ..Default::default()
        });

        let mut config = Config::load();
        config.resolve_theme(system_is_light());
        let m = config.layout.margin;

        let left = PaneView::new(&wnd, gui::dpi(m, m), gui::dpi(400, 400), &config);
        let right = PaneView::new(&wnd, gui::dpi(m, m), gui::dpi(400, 400), &config);
        let splitter = splitter::SplitterView::new(
            &wnd,
            gui::dpi(0, 0),
            gui::dpi(config.layout.splitter_width, 400),
        );

        let tab_bar = TabBar::new(&wnd, gui::dpi(0, 0), gui::dpi(800, config.layout.tab_height), &config);
        let log = LogView::new(&wnd, gui::dpi(m, m), gui::dpi(800, config.layout.log_height), &config);

        // 全キー入力を集約する 1x1 の不可視コントロール（Win32 フォーカスはここに固定し、
        // 左右ペインはフォーカスを持たない）。
        let key_sink = gui::WindowControl::new(
            &wnd,
            gui::WindowControlOpts {
                position: gui::dpi(0, 0),
                size: gui::dpi(1, 1),
                style: co::WS::CHILD | co::WS::VISIBLE | co::WS::TABSTOP,
                ..Default::default()
            },
        );

        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "..".to_owned());

        let state = rerics_core::State::load();
        let initial_window = state.window.clone();
        let initial_split = state.split_ratio.clamp(0.05, 0.95);

        // 保存タブから退避用スナップショット集合を組む。存在しないパスはフォールバックへ正規化。
        let mut tabs: Vec<TabSnapshot> = state
            .tabs
            .iter()
            .map(|t| {
                let left_path = normalize_path(&t.left, ".");
                let right_path = normalize_path(&t.right, &home);
                TabSnapshot {
                    left_state: Self::build_state_for(&left_path, &config.columns),
                    right_state: Self::build_state_for(&right_path, &config.columns),
                    left_path,
                    right_path,
                    active_right: t.active_right,
                }
            })
            .collect();
        if tabs.is_empty() {
            let left_path = ".".to_owned();
            let right_path = home.clone();
            tabs.push(TabSnapshot {
                left_state: Self::build_state_for(&left_path, &config.columns),
                right_state: Self::build_state_for(&right_path, &config.columns),
                left_path,
                right_path,
                active_right: false,
            });
        }
        let active = state.active_tab.min(tabs.len() - 1);

        let cur = &tabs[active];
        let left_pane = Rc::new(RefCell::new(Pane::open(&cur.left_path)));
        let right_pane = Rc::new(RefCell::new(Pane::open(&cur.right_path)));
        let active_right = cur.active_right;

        let keymap = config.keymap();

        let (menu_bar, menu_cmds) = menu::build().expect("メニューバーの構築");

        let (task_tx, task_rx) = std::sync::mpsc::channel();

        Self {
            wnd,
            left,
            right,
            splitter,
            tab_bar,
            log,
            key_sink,
            menu_bar: Rc::new(menu_bar),
            menu_cmds: Rc::new(menu_cmds),
            config: Rc::new(RefCell::new(config)),
            left_pane,
            right_pane,
            keymap: Rc::new(RefCell::new(keymap)),
            initial_window,
            active_right: Rc::new(Cell::new(active_right)),
            split_ratio: Rc::new(Cell::new(initial_split)),
            maximized: Rc::new(Cell::new(false)),
            tabs: Rc::new(RefCell::new(tabs)),
            active: Rc::new(Cell::new(active)),
            left_mask: Rc::new(RefCell::new(None)),
            right_mask: Rc::new(RefCell::new(None)),
            task_tx,
            task_rx: Rc::new(task_rx),
            tasks: Rc::new(RefCell::new(Vec::new())),
            next_task_id: Rc::new(Cell::new(0)),
            progress_seq: Arc::new(AtomicU64::new(0)),
            shutdown: Arc::new(AtomicBool::new(false)),
            in_dialog: Rc::new(Cell::new(false)),
        }
    }

    fn run(&self) -> w::AnyResult<i32> {
        self.setup_events();
        self.wnd.run_main(None)
    }

    fn setup_events(&self) {
        // 各ペインのキー入力とダブルクリックを配線（コントロール生成は済んでいるが、
        // FileListView のコールバック登録は実行時可で、内部イベントは生成前に配線済み）。
        self.wire_pane(true);
        self.wire_pane(false);
        self.wire_key_sink();

        // メニュー項目（有効なもの）をアクティブ側ペインへのコマンド実行に配線する。
        for (&id, &cmd) in self.menu_cmds.iter() {
            let this = self.clone();
            self.wnd.on().wm_command_acc_menu(id, move || {
                let is_left = !this.active_right.get();
                this.exec(is_left, cmd)?;
                Ok(())
            });
        }

        // アクティブ化のたびにフォーカスをキーシンクへ集約する。
        let this = self.clone();
        self.wnd.on().wm(co::WM::ACTIVATE, move |_| {
            this.key_sink.hwnd().SetFocus();
            Ok(0)
        });

        let this = self.clone();
        self.splitter.on_drag(move |splitter_left| {
            let _ = this.drag_splitter(splitter_left);
        });

        let this = self.clone();
        self.tab_bar.on_click(move |index| {
            let _ = this.switch_tab(index);
        });

        let this = self.clone();
        self.wnd.on().wm_create(move |_| {
            this.wnd.hwnd().SetMenu(&this.menu_bar)?;
            if let Some(ws) = &this.initial_window {
                let applied = window_state::apply(&this.wnd.hwnd(), ws);
                if applied && ws.maximized {
                    unsafe {
                        let _ = this.wnd.hwnd().PostMessage(w::msg::WndMsg {
                            msg_id: wm_restore_maximize(),
                            wparam: 0,
                            lparam: 0,
                        });
                    }
                }
            }
            this.layout()?;
            let snap = this.tabs.borrow()[this.active.get()].clone();
            this.load_snapshot(&snap)?;
            this.update_title()?;
            this.refresh_tab_bar()?;
            Ok(0)
        });

        let this = self.clone();
        self.wnd.on().wm(wm_restore_maximize(), move |_| {
            window_state::maximize(&this.wnd.hwnd());
            Ok(0)
        });

        let this = self.clone();
        self.wnd.on().wm_mouse_wheel(move |p| {
            let dist = p.keys.raw() as i16;
            this.scroll_under_cursor(dist, p.coords)?;
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_size(move |_| this.layout());

        let this = self.clone();
        self.wnd.on().wm_timer(task::TASK_TIMER_ID, move || this.pump_tasks());

        let this = self.clone();
        self.wnd.on().wm_destroy(move || {
            this.shutdown.store(true, Ordering::Relaxed);
            this.save_active();
            let window = window_state::capture(&this.wnd.hwnd());
            let tabs: Vec<rerics_core::TabState> = this
                .tabs
                .borrow()
                .iter()
                .map(|t| rerics_core::TabState {
                    left: t.left_path.clone(),
                    right: t.right_path.clone(),
                    active_right: t.active_right,
                })
                .collect();
            let state = rerics_core::State {
                window,
                tabs,
                active_tab: this.active.get(),
                split_ratio: this.split_ratio.get(),
            };
            if let Err(e) = state.save() {
                eprintln!("状態の保存に失敗: {}", e);
            }
            Ok(())
        });
    }

    fn wire_pane(&self, is_left: bool) {
        let this = self.clone();
        self.view(is_left).on_activate(move |idx| {
            let _ = this.activate(is_left, idx);
        });

        // クリックでアクティブ側を切り替える（カーソルを出し、反対側を消す）。キー入力は
        // キーシンクに集約するので、ここではフォーカスをキーシンクへ戻すだけにする。
        let this = self.clone();
        self.view(is_left).on_got_focus(move || {
            this.active_right.set(!is_left);
            this.view(is_left).set_cursor_visible(true);
            this.view(!is_left).set_cursor_visible(false);
            this.key_sink.hwnd().SetFocus();
        });

        // ホイールはカーソル下のペインをスクロールする。
        let this = self.clone();
        self.view(is_left).on_wheel(move |dist, coords| {
            let _ = this.scroll_under_cursor(dist, coords);
        });
    }

    /// 画面座標 `coords` の下にあるペインをホイール回転分だけスクロールする。
    fn scroll_under_cursor(&self, distance: i16, coords: w::POINT) -> w::AnyResult<()> {
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

    fn exec(&self, is_left: bool, cmd: Command) -> w::AnyResult<()> {
        let view = self.view(is_left);
        let state = view.state();
        let pr = view.page_rows();
        match cmd {
            Command::CursorUp => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.set_cursor(c - 1, pr);
            }
            Command::CursorDown => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.set_cursor(c + 1, pr);
            }
            Command::CursorTop => {
                state.borrow_mut().set_cursor(0, pr);
            }
            Command::CursorEnd => {
                let mut s = state.borrow_mut();
                let last = s.count() as isize - 1;
                s.set_cursor(last, pr);
            }
            Command::CursorPageUp => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.set_cursor(c - pr as isize, pr);
            }
            Command::CursorPageDown => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.set_cursor(c + pr as isize, pr);
            }
            Command::EnterDir => {
                let cursor = state.borrow().cursor;
                self.activate(is_left, cursor)?;
                return Ok(());
            }
            Command::ToParent => {
                self.to_parent(is_left)?;
                return Ok(());
            }
            Command::FocusLeft => {
                self.view(true).hwnd().SetFocus();
                return Ok(());
            }
            Command::FocusRight => {
                self.view(false).hwnd().SetFocus();
                return Ok(());
            }
            Command::MarkToggle => {
                let mut s = state.borrow_mut();
                let c = s.cursor;
                s.reverse_file(c, pr);
                let c = s.cursor as isize;
                s.set_cursor(c + 1, pr);
            }
            Command::SelectAll => {
                state.borrow_mut().select_all(false);
            }
            Command::SelectAllFile => {
                state.borrow_mut().select_all(true);
            }
            Command::ReverseAll => {
                state.borrow_mut().reverse_all(false);
            }
            Command::ReverseAllFile => {
                state.borrow_mut().reverse_all(true);
            }
            Command::ClearAll => {
                state.borrow_mut().clear_all();
            }
            Command::Reload => {
                self.reload_side(true)?;
                self.reload_side(false)?;
                return Ok(());
            }
            Command::SortByName => self.sort_active(is_left, SortType::FileName, false),
            Command::SortByExtension => self.sort_active(is_left, SortType::Extension, false),
            Command::SortBySize => self.sort_active(is_left, SortType::Length, false),
            Command::SortByDate => self.sort_active(is_left, SortType::LastWriteTime, false),
            Command::SortReverseToggle => {
                let t = state.borrow().sort_type;
                self.sort_active(is_left, t, true);
            }
            Command::PageNext => {
                self.page_next()?;
                return Ok(());
            }
            Command::PagePrevious => {
                self.page_previous()?;
                return Ok(());
            }
            Command::NewTab => {
                self.new_tab()?;
                return Ok(());
            }
            Command::CloseTab => {
                self.close_tab()?;
                return Ok(());
            }
            Command::MakeDirectory => {
                self.make_directory(is_left)?;
                return Ok(());
            }
            Command::CreateFile => {
                self.create_file(is_left)?;
                return Ok(());
            }
            Command::Copy => {
                self.copy_or_move(is_left, false)?;
                return Ok(());
            }
            Command::Move => {
                self.copy_or_move(is_left, true)?;
                return Ok(());
            }
            Command::SwapPath => {
                self.swap_paths()?;
                return Ok(());
            }
            Command::NextDrive => {
                self.change_drive(is_left, 1)?;
                return Ok(());
            }
            Command::PreviousDrive => {
                self.change_drive(is_left, -1)?;
                return Ok(());
            }
            Command::OppositeToCurrent => {
                let p = self.pane(is_left).borrow().path().to_path_buf();
                *self.pane(!is_left).borrow_mut() = Pane::open(&p);
                self.reload_side(!is_left)?;
                return Ok(());
            }
            Command::CurrentToOpposite => {
                let p = self.pane(!is_left).borrow().path().to_path_buf();
                *self.pane(is_left).borrow_mut() = Pane::open(&p);
                self.reload_side(is_left)?;
                return Ok(());
            }
            Command::Rename => {
                self.rename(is_left)?;
                return Ok(());
            }
            Command::Delete => {
                self.delete(is_left)?;
                return Ok(());
            }
            Command::PathMask => {
                self.path_mask(is_left)?;
                return Ok(());
            }
            Command::SelectMask => {
                self.select_mask(is_left)?;
                return Ok(());
            }
            Command::OpenTaskManager => {
                self.open_task_manager()?;
                return Ok(());
            }
            Command::OpenSettings => {
                self.open_settings()?;
                return Ok(());
            }
            Command::MaximizeLeft => {
                self.maximize_left(false)?;
                return Ok(());
            }
            Command::MaximizeRight => {
                self.maximize_right(false)?;
                return Ok(());
            }
            Command::MaximizeLeftForce => {
                self.maximize_left(true)?;
                return Ok(());
            }
            Command::MaximizeRightForce => {
                self.maximize_right(true)?;
                return Ok(());
            }
            Command::BorderRight => {
                let unit = self.config.borrow().layout.border_unit;
                self.border_move(gui::dpi_x(unit))?;
                return Ok(());
            }
            Command::BorderLeft => {
                let unit = self.config.borrow().layout.border_unit;
                self.border_move(-gui::dpi_x(unit))?;
                return Ok(());
            }
            Command::BorderReset => {
                self.border_reset()?;
                return Ok(());
            }
            Command::Quit => {
                self.wnd.hwnd().DestroyWindow()?;
                return Ok(());
            }
        }
        view.refresh()?;
        self.update_selected_info(is_left);
        Ok(())
    }

    /// アクティブペインを次/前のドライブのルートへ移す（`delta` は +1/-1、巡回）。
    fn change_drive(&self, is_left: bool, delta: isize) -> w::AnyResult<()> {
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
        let next = &roots[((idx + delta).rem_euclid(n)) as usize];
        *self.pane(is_left).borrow_mut() = Pane::open(next);
        self.reload_side(is_left)?;
        Ok(())
    }

    /// 左右ペインのパスを入れ替える。
    fn swap_paths(&self) -> w::AnyResult<()> {
        let lp = self.left_pane.borrow().path().to_path_buf();
        let rp = self.right_pane.borrow().path().to_path_buf();
        *self.left_pane.borrow_mut() = Pane::open(&rp);
        *self.right_pane.borrow_mut() = Pane::open(&lp);
        self.reload_side(true)?;
        self.reload_side(false)?;
        Ok(())
    }

    /// 指定ペインを並べ替える。カーソル下のファイルを保持する。`toggle` 時は
    /// 現在の昇降を反転、そうでなければ昇順にする。
    fn sort_active(&self, is_left: bool, sort: SortType, toggle: bool) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        let state = view.state();
        let mut s = state.borrow_mut();
        let name = s.items.get(s.cursor).map(|i| i.name.clone());
        let reverse = if toggle { !s.sort_reverse } else { false };
        s.sort(sort, reverse);
        if let Some(n) = name {
            s.set_cursor_position(&n, pr);
        }
    }

    /// 指定パスの一覧を読み、既定ソートでカーソル先頭の `FileListState` を組む。
    fn build_state_for(path: &str, columns: &[Column]) -> FileListState {
        let items = Pane::open(path).read();
        let mut s = FileListState::new();
        s.columns = columns.to_vec();
        let sort = s.sort_type;
        let reverse = s.sort_reverse;
        s.items = items;
        s.sort(sort, reverse);
        s.cursor = 0;
        s.scroll_top = 0;
        s
    }

    /// 現在のライブ状態を退避用スナップショットに固める。
    fn snapshot_live(&self) -> TabSnapshot {
        TabSnapshot {
            left_path: self.left_pane.borrow().path().display().to_string(),
            right_path: self.right_pane.borrow().path().display().to_string(),
            left_state: self.view(true).state().borrow().clone(),
            right_state: self.view(false).state().borrow().clone(),
            active_right: self.active_right.get(),
        }
    }

    /// スナップショットをライブ側へ反映し、再描画とフォーカス設定を行う。
    fn load_snapshot(&self, snap: &TabSnapshot) -> w::AnyResult<()> {
        *self.left_pane.borrow_mut() = Pane::open(&snap.left_path);
        *self.right_pane.borrow_mut() = Pane::open(&snap.right_path);
        *self.view(true).state().borrow_mut() = snap.left_state.clone();
        *self.view(false).state().borrow_mut() = snap.right_state.clone();
        self.active_right.set(snap.active_right);
        self.bar(true).set_path(&snap.left_path);
        self.bar(false).set_path(&snap.right_path);
        self.view(true).autofit_columns()?;
        self.view(false).autofit_columns()?;
        self.view(true).refresh()?;
        self.view(false).refresh()?;
        self.update_selected_info(true);
        self.update_selected_info(false);
        self.update_drive_info(true);
        self.update_drive_info(false);
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// アクティブタブのスナップショットをライブから更新する。
    fn save_active(&self) {
        let snap = self.snapshot_live();
        let i = self.active.get();
        self.tabs.borrow_mut()[i] = snap;
    }

    /// ウィンドウタイトルに `[現在/総数]` を反映する。
    fn update_title(&self) -> w::AnyResult<()> {
        let total = self.tabs.borrow().len();
        let n = self.active.get() + 1;
        self.wnd
            .hwnd()
            .SetWindowText(&format!("Rerics [{}/{}]", n, total))?;
        Ok(())
    }

    /// タブ帯のラベルとアクティブ位置を更新し、再描画する。
    fn refresh_tab_bar(&self) -> w::AnyResult<()> {
        let active = self.active.get();
        // アクティブタブのラベルはライブのペインパスから（スナップショットは切替時のみ更新の
        // ため、同一タブ内で移動するとラベルが古くなる）。非アクティブタブはスナップショット。
        let live = {
            let p = self.pane(!self.active_right.get()).borrow();
            p.path().to_path_buf()
        };
        let labels: Vec<String> = self
            .tabs
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == active {
                    live.display().to_string()
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

    /// 指定 index のタブへ切替える（範囲外・現在と同じなら何もしない）。
    fn switch_tab(&self, index: usize) -> w::AnyResult<()> {
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
    fn page_next(&self) -> w::AnyResult<()> {
        let total = self.tabs.borrow().len();
        if total <= 1 {
            return Ok(());
        }
        let cur = self.active.get();
        self.switch_tab((cur + 1) % total)
    }

    /// 前のタブへ循環移動する。
    fn page_previous(&self) -> w::AnyResult<()> {
        let total = self.tabs.borrow().len();
        if total <= 1 {
            return Ok(());
        }
        let cur = self.active.get();
        self.switch_tab((cur + total - 1) % total)
    }

    /// 現在のパスを複製した新タブをアクティブ直後に挿入して切替える。
    fn new_tab(&self) -> w::AnyResult<()> {
        self.save_active();
        let left_path = self.left_pane.borrow().path().display().to_string();
        let right_path = self.right_pane.borrow().path().display().to_string();
        let columns = self.config.borrow().columns.clone();
        let snap = TabSnapshot {
            left_state: Self::build_state_for(&left_path, &columns),
            right_state: Self::build_state_for(&right_path, &columns),
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
    fn close_tab(&self) -> w::AnyResult<()> {
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

    fn wire_key_sink(&self) {
        self.key_sink.on().wm_get_dlg_code(move |_| {
            let flags = co::DLGC::WANTARROWS.raw() | co::DLGC::WANTALLKEYS.raw();
            Ok(unsafe { co::DLGC::from_raw(flags) })
        });
        let this = self.clone();
        self.key_sink.on().wm_key_down(move |p| {
            let ctrl = w::GetAsyncKeyState(co::VK::CONTROL);
            let shift = w::GetAsyncKeyState(co::VK::SHIFT);
            let chord = KeyChord::new(p.vkey_code.raw(), ctrl, shift, p.has_alt_key);
            let resolved = this.keymap.borrow().resolve(&chord);
            if let Some(cmd) = resolved {
                let is_left = !this.active_right.get();
                let _ = this.exec(is_left, cmd);
            }
            Ok(())
        });
    }

    fn view(&self, is_left: bool) -> &FileListView {
        if is_left { self.left.list() } else { self.right.list() }
    }

    fn bar(&self, is_left: bool) -> &PathBarView {
        if is_left { self.left.bar() } else { self.right.bar() }
    }

    fn status(&self, is_left: bool) -> &StatusBarView {
        if is_left { self.left.status() } else { self.right.status() }
    }

    /// ペインの選択数/サイズをステータスバー左へ反映する（0件なら空）。
    fn update_selected_info(&self, is_left: bool) {
        let (count, size) = self.view(is_left).state().borrow().selected_count_size();
        let text = rerics_core::format_selected(count, size).unwrap_or_default();
        self.status(is_left).set_left(&text);
    }

    /// ペインのドライブ容量をステータスバー右へ反映する。
    fn update_drive_info(&self, is_left: bool) {
        let path = self.pane(is_left).borrow().path().to_path_buf();
        self.status(is_left).set_right(&drive_info_text(&path));
    }

    fn pane(&self, is_left: bool) -> &Rc<RefCell<Pane>> {
        if is_left { &self.left_pane } else { &self.right_pane }
    }

    fn mask(&self, is_left: bool) -> &Rc<RefCell<Option<String>>> {
        if is_left { &self.left_mask } else { &self.right_mask }
    }

    /// ペインの現在パスを読み直して State へ反映し、パスバーを更新する。
    fn reload_side(&self, is_left: bool) -> w::AnyResult<()> {
        let view = self.view(is_left);
        let items = self.pane(is_left).borrow().read();
        let items = match self.mask(is_left).borrow().as_ref() {
            Some(m) => items
                .into_iter()
                .filter(|it| it.is_parent || it.is_dir || rerics_core::glob_match(&it.name, m))
                .collect(),
            None => items,
        };
        let path = self.pane(is_left).borrow().path().display().to_string();
        let pr = view.page_rows();
        {
            let state = view.state();
            let mut s = state.borrow_mut();
            s.items = items;
            let sort = s.sort_type;
            let reverse = s.sort_reverse;
            s.sort(sort, reverse);
            s.set_cursor(0, pr);
        }
        self.bar(is_left).set_path(&path);
        view.autofit_columns()?;
        view.refresh()?;
        self.update_selected_info(is_left);
        self.update_drive_info(is_left);
        self.refresh_tab_bar()?;
        Ok(())
    }

    /// 入力ダイアログで名前を尋ね、アクティブペインの現在パス直下にディレクトリを作る。
    /// 作成後は一覧を更新し、新ディレクトリへカーソルを移す。
    fn make_directory(&self, is_left: bool) -> w::AnyResult<()> {
        let name = dialog::input_box(
            &self.wnd,
            "ディレクトリの作成",
            &messages::directory_name_question(),
            "新しいディレクトリ",
            dialog::InputMode::Plain,
        );
        let Some(name) = name else {
            return Ok(());
        };
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().join(name);
        if let Err(e) = std::fs::create_dir(&dir) {
            let line = messages::create_directory_failure(name, &e.to_string());
            self.log.error(&line);
            dialog::message_box(&self.wnd, "ディレクトリの作成", &line, dialog::MessageStyle::Error);
            return Ok(());
        }
        self.log.normal(&messages::create_directory(name));
        self.reload_side(is_left)?;
        let view = self.view(is_left);
        let pr = view.page_rows();
        view.state().borrow_mut().set_cursor_position(name, pr);
        view.refresh()?;
        Ok(())
    }

    /// 入力ダイアログで新規の空ファイルを作る。既存ファイルは上書きしない。
    fn create_file(&self, is_left: bool) -> w::AnyResult<()> {
        let name = dialog::input_box(
            &self.wnd,
            "新規ファイルの作成",
            "ファイル名を入力して下さい。",
            "",
            dialog::InputMode::Plain,
        );
        let Some(name) = name else {
            return Ok(());
        };
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let path = self.pane(is_left).borrow().path().join(name);
        if let Err(e) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            let msg = if e.kind() == std::io::ErrorKind::AlreadyExists {
                messages::all_ready_exists(name)
            } else {
                format!("{e}")
            };
            dialog::message_box(&self.wnd, "新規ファイルの作成", &msg, dialog::MessageStyle::Error);
            return Ok(());
        }
        self.reload_side(is_left)?;
        let view = self.view(is_left);
        let pr = view.page_rows();
        view.state().borrow_mut().set_cursor_position(name, pr);
        view.refresh()?;
        Ok(())
    }

    /// アクティブペインの選択（無ければカーソル）を反対側ペインへコピー/移動する。
    fn copy_or_move(&self, is_left: bool, move_it: bool) -> w::AnyResult<()> {
        let names: Vec<String> = {
            let state = self.view(is_left).state();
            let s = state.borrow();
            let selected: Vec<String> = s
                .items
                .iter()
                .filter(|it| it.selected && !it.is_parent)
                .map(|it| it.name.clone())
                .collect();
            if selected.is_empty() {
                match s.items.get(s.cursor) {
                    Some(it) if !it.is_parent => vec![it.name.clone()],
                    _ => Vec::new(),
                }
            } else {
                selected
            }
        };
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let src_dir = self.pane(is_left).borrow().path().to_path_buf();
        let dst_dir = self.pane(!is_left).borrow().path().to_path_buf();
        self.start_copy(src_dir, dst_dir, names, move_it)
    }

    /// コピー/移動をワーカースレッドで起動する。完了は `wm_timer` 経由で取り込む。
    fn start_copy(
        &self,
        src_dir: PathBuf,
        dst_dir: PathBuf,
        names: Vec<String>,
        move_it: bool,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let kind = if move_it { OpKind::Move } else { OpKind::Copy };
        let id = self.next_id();
        let text = if move_it { "移動" } else { "コピー" };
        let desc = format!("{} -> {}", short_desc(&names), dst_dir.display());
        self.register_task(id, text, desc, control)?;
        std::thread::spawn(move || {
            rerics_core::run_copy(&host, &src_dir, &dst_dir, &names, move_it);
            let _ = host.tx.send(WorkerEvent::Done { id, kind, src_dir, dst_dir });
        });
        Ok(())
    }

    /// 削除をワーカースレッドで起動する。
    fn start_delete(&self, dir: PathBuf, names: Vec<String>) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        self.register_task(id, "削除", short_desc(&names), control)?;
        std::thread::spawn(move || {
            rerics_core::run_delete(&host, &dir, &names);
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind: OpKind::Delete,
                src_dir: dir.clone(),
                dst_dir: dir,
            });
        });
        Ok(())
    }

    /// 新しいタスク ID を払い出す。
    fn next_id(&self) -> u64 {
        let n = self.next_task_id.get();
        self.next_task_id.set(n + 1);
        n
    }

    /// タスクをレジストリに登録し、最初の1件なら取り込みタイマを起動する。
    fn register_task(
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
    fn open_task_manager(&self) -> w::AnyResult<()> {
        task_manager::show(&self.wnd, &self.tasks);
        Ok(())
    }

    /// 設定ダイアログを開く。開いた時点で OS テーマを再判定し、OK なら設定をライブ反映して
    /// 差分を `config.toml` へ保存する。
    fn open_settings(&self) -> w::AnyResult<()> {
        if self.in_dialog.get() {
            return Ok(());
        }
        let mut current = self.config.borrow().clone();
        current.resolve_theme(system_is_light());
        self.in_dialog.set(true);
        let edited = settings_dialog::show(&self.wnd, &current);
        self.in_dialog.set(false);
        if let Some(mut new) = edited {
            new.resolve_theme(system_is_light());
            self.apply_config(new)?;
            if let Err(e) = self.config.borrow().save() {
                self.log.error(&format!("設定の保存に失敗: {}", e));
            }
        }
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// 新しい設定をライブ反映する（配色・フォント・レイアウト寸法・キーバインド）。
    /// 列構成の変更は再起動後に反映される。
    fn apply_config(&self, new: Config) -> w::AnyResult<()> {
        *self.config.borrow_mut() = new;
        let km = self.config.borrow().keymap();
        *self.keymap.borrow_mut() = km;
        {
            let cfg = self.config.borrow();
            self.left.apply_config(&cfg);
            self.right.apply_config(&cfg);
            self.tab_bar.apply_config(&cfg);
            self.log.apply_config(&cfg);
        }
        self.layout()?;
        self.refresh_tab_bar()?;
        Ok(())
    }

    /// ワーカーからのイベントを取り込み、ログ反映・完了処理を行う。
    ///
    /// 衝突モーダル表示中はモーダルの内部ループから `WM_TIMER` が再入するため、
    /// `in_dialog` ガードで多重取り込みを抑止する。
    fn pump_tasks(&self) -> w::AnyResult<()> {
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
                    if self.tasks.borrow().is_empty() {
                        let _ = self.wnd.hwnd().KillTimer(task::TASK_TIMER_ID);
                    }
                }
            }
        }
        Ok(())
    }

    /// 操作完了に応じて関与した側のペインを再読込・選択解除する。
    fn on_op_done(&self, kind: OpKind, src_dir: &Path, dst_dir: &Path) -> w::AnyResult<()> {
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

    /// カーソル位置の項目を入力ダイアログでリネームする。完了後は新名へカーソルを移す。
    fn rename(&self, is_left: bool) -> w::AnyResult<()> {
        let view = self.view(is_left);
        let old = {
            let state = view.state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent => it.name.clone(),
                _ => return Ok(()),
            }
        };
        let new = dialog::input_box(
            &self.wnd,
            "名前の変更",
            "新しい名前を入力して下さい。",
            &old,
            dialog::InputMode::Plain,
        );
        let Some(new) = new else {
            return Ok(());
        };
        let new = new.trim();
        if new.is_empty() || new == old {
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        if let Err(e) = std::fs::rename(dir.join(&old), dir.join(new)) {
            let line = messages::rename_failure(&old, &e.to_string());
            self.log.error(&line);
            dialog::message_box(&self.wnd, "名前の変更", &line, dialog::MessageStyle::Error);
            return Ok(());
        }
        self.log.normal(&messages::rename(&old, new));
        self.reload_side(is_left)?;
        let pr = self.view(is_left).page_rows();
        self.view(is_left)
            .state()
            .borrow_mut()
            .set_cursor_position(new, pr);
        self.view(is_left).refresh()?;
        Ok(())
    }

    /// アクティブペインの選択（無ければカーソル）を確認ダイアログ付きで削除する。
    fn delete(&self, is_left: bool) -> w::AnyResult<()> {
        let names: Vec<String> = {
            let state = self.view(is_left).state();
            let s = state.borrow();
            let selected: Vec<String> = s
                .items
                .iter()
                .filter(|it| it.selected && !it.is_parent)
                .map(|it| it.name.clone())
                .collect();
            if selected.is_empty() {
                match s.items.get(s.cursor) {
                    Some(it) if !it.is_parent => vec![it.name.clone()],
                    _ => Vec::new(),
                }
            } else {
                selected
            }
        };
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let short = if names.len() > 1 {
            format!("{}他", names[0])
        } else {
            names[0].clone()
        };
        let ans = dialog::message_box(
            &self.wnd,
            "削除",
            &messages::delete_question(&short),
            dialog::MessageStyle::YesNo,
        );
        if ans != dialog::MessageResult::Yes {
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        self.start_delete(dir, names)
    }

    /// 入力ダイアログでパスマスクを尋ね、表示フィルタを設定/解除して一覧を更新する。
    fn path_mask(&self, is_left: bool) -> w::AnyResult<()> {
        let cur = self.mask(is_left).borrow().clone().unwrap_or_default();
        let input = dialog::input_box(
            &self.wnd,
            "パスマスク",
            "表示するマスク（* で解除・カンマ区切り）:",
            &cur,
            dialog::InputMode::Plain,
        );
        let Some(input) = input else {
            return Ok(());
        };
        let input = input.trim();
        if input.is_empty() || input == "*" {
            *self.mask(is_left).borrow_mut() = None;
        } else {
            *self.mask(is_left).borrow_mut() = Some(input.to_owned());
        }
        self.reload_side(is_left)?;
        Ok(())
    }

    /// 入力ダイアログでマスクを尋ね、一致するファイルの選択状態を立てる。
    fn select_mask(&self, is_left: bool) -> w::AnyResult<()> {
        let input = dialog::input_box(
            &self.wnd,
            "マスクで選択",
            "選択するマスク（カンマ区切り）:",
            "",
            dialog::InputMode::Plain,
        );
        let Some(input) = input else {
            return Ok(());
        };
        let input = input.trim();
        if input.is_empty() {
            return Ok(());
        }
        {
            let state = self.view(is_left).state();
            let mut s = state.borrow_mut();
            for it in &mut s.items {
                if !it.is_parent && rerics_core::glob_match(&it.name, input) {
                    it.selected = true;
                }
            }
        }
        self.view(is_left).refresh()?;
        self.update_selected_info(is_left);
        Ok(())
    }

    /// カーソル行を侵入する（dir/親なら移動、file は無視）。
    fn activate(&self, is_left: bool, index: usize) -> w::AnyResult<()> {
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
        if is_dir {
            if self.pane(is_left).borrow_mut().enter(&name) {
                self.reload_side(is_left)?;
            }
        } else {
            let path = self.pane(is_left).borrow().path().join(&name);
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
    fn to_parent(&self, is_left: bool) -> w::AnyResult<()> {
        let prev = self.pane(is_left).borrow_mut().to_parent();
        let Some(prev_name) = prev else {
            return Ok(());
        };
        self.reload_side(is_left)?;
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

    /// 左右2ペイン（パスバー＋リスト）と境界線を `split_ratio` に従って割り付ける。
    fn layout(&self) -> w::AnyResult<()> {
        let rc = self.wnd.hwnd().GetClientRect()?;
        let total_w = rc.right - rc.left;
        let total_h = rc.bottom - rc.top;
        let cfg = self.config.borrow();
        let lay = &cfg.layout;
        let m = gui::dpi_x(lay.margin);
        let my = gui::dpi_y(lay.margin);
        let splitter_w = gui::dpi_x(lay.splitter_width);

        let tab_h = gui::dpi_y(lay.tab_height);
        let log_h = gui::dpi_y(lay.log_height);
        let log_gap = gui::dpi_y(lay.log_gap);
        let bars_y = tab_h;
        let log_y = total_h - my - log_h;
        let pane_h = (log_y - log_gap - bars_y).max(0);

        let panes_total = (total_w - m * 2 - splitter_w).max(0);
        let min_pane = gui::dpi_x(24).min(panes_total / 2);
        let left_w = ((panes_total as f64 * self.split_ratio.get()).round() as i32)
            .clamp(min_pane, (panes_total - min_pane).max(min_pane));
        let right_w = panes_total - left_w;
        let left_x = m;
        let splitter_x = m + left_w;
        let right_x = splitter_x + splitter_w;
        let log_w = total_w - m * 2;

        place(self.tab_bar.hwnd(), 0, 0, total_w, tab_h)?;
        place(self.left.hwnd(), left_x, bars_y, left_w, pane_h)?;
        self.left.relayout()?;
        place(self.splitter.hwnd(), splitter_x, bars_y, splitter_w, pane_h)?;
        place(self.right.hwnd(), right_x, bars_y, right_w, pane_h)?;
        self.right.relayout()?;
        // 利用可能幅が変わったので content-fit を再計算する（フレックス列が残り幅に追従）。
        self.view(true).autofit_columns()?;
        self.view(false).autofit_columns()?;
        place(self.log.hwnd(), left_x, log_y, log_w, log_h)?;
        self.tab_bar.refresh()?;
        self.log.refresh()?;
        Ok(())
    }

    /// 現在のクライアント幅でのペイン合計幅（左右の幅の和・物理px）。
    fn panes_total(&self) -> w::AnyResult<i32> {
        let rc = self.wnd.hwnd().GetClientRect()?;
        let total_w = rc.right - rc.left;
        let cfg = self.config.borrow();
        let lay = &cfg.layout;
        let m = gui::dpi_x(lay.margin);
        let splitter_w = gui::dpi_x(lay.splitter_width);
        Ok((total_w - m * 2 - splitter_w).max(1))
    }

    /// 左ペイン幅（物理px）から分割比を更新して再レイアウトする。
    fn set_left_width(&self, left_w: i32) -> w::AnyResult<()> {
        let pt = self.panes_total()?;
        let ratio = (left_w as f64 / pt as f64).clamp(0.05, 0.95);
        self.split_ratio.set(ratio);
        self.layout()
    }

    /// スプリッタのドラッグ（親座標の希望左端）を分割比へ反映する。
    fn drag_splitter(&self, splitter_left: i32) -> w::AnyResult<()> {
        let m = gui::dpi_x(self.config.borrow().layout.margin);
        self.maximized.set(false);
        self.set_left_width(splitter_left - m)
    }

    /// 左ペイン最大化（トグル。最大化中はトグルで中央へ）。`force` は常に最大化。
    fn maximize_left(&self, force: bool) -> w::AnyResult<()> {
        if !force && self.maximized.get() {
            return self.border_reset();
        }
        let pt = self.panes_total()?;
        let margin = gui::dpi_x(self.config.borrow().layout.maximize_margin);
        self.maximized.set(true);
        self.set_left_width(pt - margin)
    }

    /// 右ペイン最大化（トグル）。`force` は常に最大化。
    fn maximize_right(&self, force: bool) -> w::AnyResult<()> {
        if !force && self.maximized.get() {
            return self.border_reset();
        }
        let margin = gui::dpi_x(self.config.borrow().layout.maximize_margin);
        self.maximized.set(true);
        self.set_left_width(margin)
    }

    /// 境界を中央50%へ戻す（最大化状態を解除）。
    fn border_reset(&self) -> w::AnyResult<()> {
        self.maximized.set(false);
        self.split_ratio.set(0.5);
        self.layout()
    }

    /// 境界を `delta`（物理px・左ペインが正で広がる）だけ動かす。
    fn border_move(&self, delta: i32) -> w::AnyResult<()> {
        let pt = self.panes_total()?;
        let cur_left = (pt as f64 * self.split_ratio.get()).round() as i32;
        self.set_left_width(cur_left + delta)
    }
}

/// タスク詳細用の短い対象表記（複数なら先頭名＋「他」）。
fn short_desc(names: &[String]) -> String {
    match names.split_first() {
        Some((first, rest)) if !rest.is_empty() => format!("{first}他"),
        Some((first, _)) => first.clone(),
        None => String::new(),
    }
}

fn place(hwnd: &w::HWND, x: i32, y: i32, cx: i32, cy: i32) -> w::AnyResult<()> {
    hwnd.MoveWindow(w::POINT { x, y }, w::SIZE { cx, cy }, true)?;
    Ok(())
}

/// パスの属するドライブの容量を「C: 空き ◯ / 全 ◯」形式で返す。取得できなければ空。
fn drive_info_text(path: &Path) -> String {
    let dir = path.to_string_lossy();
    let mut free = 0u64;
    let mut total = 0u64;
    if w::GetDiskFreeSpaceEx(Some(dir.as_ref()), Some(&mut free), Some(&mut total), None).is_err() {
        return String::new();
    }
    rerics_core::format_drive(&drive_label(path), free, total)
}

/// パスのドライブレター表記（"C:"）。求められなければ空。
fn drive_label(path: &Path) -> String {
    path.ancestors()
        .last()
        .map(|r| r.to_string_lossy().trim_end_matches(['\\', '/']).to_uppercase())
        .unwrap_or_default()
}

/// パスを正規化する。存在しない（ディレクトリでない）場合は `fallback` を返す。
fn normalize_path(path: &str, fallback: &str) -> String {
    if !path.is_empty() && std::path::Path::new(path).is_dir() {
        path.to_owned()
    } else {
        fallback.to_owned()
    }
}
