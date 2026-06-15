mod chrome;
#[cfg(feature = "debug-server")]
mod debug_server;
mod dialog;
mod file_list;
mod log_view;
mod menu;
mod pane_view;
mod path_bar;
mod settings_dialog;
mod splitter;
mod media_view;
mod status_bar;
mod tab_bar;
mod task;
mod task_manager;
mod video;
mod viewer;
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
use media_view::{MediaView, NavResolver};
use pane_view::PaneView;
use path_bar::PathBarView;
use status_bar::StatusBarView;
use tab_bar::TabBar;
use task::{ChannelHost, OpKind, TaskControl, TaskEntry, WorkerEvent};
use viewer::ViewerView;
use rerics_core::{
    Column, Command, Config, FileListState, KeyChord, KeyMap, Location, LogLevel, MediaKind, Pane,
    SortType, WindowState, data_dir, messages, open_archive,
};
use winsafe::{self as w, co, gui, prelude::*};

/// 現在メイン領域に重ねているビューア。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveView {
    None,
    Text,
    Media,
}

/// 表示完了後に最大化を実行させるための自前メッセージ（`WM_APP`）。
fn wm_restore_maximize() -> co::WM {
    unsafe { co::WM::from_raw(0x8000) }
}

/// FileItem 1 件をデバッグ `/state` 用の JSON 値へ。
#[cfg(feature = "debug-server")]
fn debug_item_json(it: &rerics_core::FileItem, is_cursor: bool) -> serde_json::Value {
    use serde_json::json;
    if it.is_parent {
        return json!({ "name": it.name, "is_parent": true, "is_dir": true, "cursor": is_cursor });
    }
    let modified = it
        .modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    json!({
        "name": it.name,
        "is_dir": it.is_dir,
        "ext": it.extension,
        "size": it.size,
        "marked": it.selected,
        "attrs": debug_attrs(it),
        "cursor": is_cursor,
        "modified": modified,
    })
}

/// スナップショットの範囲指定 `"x,y-WxH"` を解析する。
#[cfg(feature = "debug-server")]
fn parse_region(s: &str) -> Option<(i32, i32, i32, i32)> {
    let (xy, wh) = s.split_once('-')?;
    let (x, y) = xy.split_once(',')?;
    let (w, h) = wh.split_once('x')?;
    Some((
        x.parse().ok()?,
        y.parse().ok()?,
        w.parse().ok()?,
        h.parse().ok()?,
    ))
}

/// デバッグ制御サーバから見たコマンドの種別。
#[cfg(feature = "debug-server")]
enum DebugCmdClass {
    /// 即実行して状態を返せる（ナビ/マーク/ソート/タブ/ビューア等）。
    NonModal,
    /// モーダルを開く＋ファイルを操作し得る（要 `--debug-allow-write`・`/modal/*` で操作）。
    ModalWrite,
    /// デバッグ制御サーバでは未対応（複雑モーダル等）。
    Unsupported,
}

/// コマンドの種別を分類する。ファイル操作系のモーダルは ModalWrite、設定/タスク管理は Unsupported。
#[cfg(feature = "debug-server")]
fn debug_command_class(cmd: Command) -> DebugCmdClass {
    use Command::*;
    match cmd {
        MakeDirectory | CreateFile | Rename | Delete | Copy | Move => DebugCmdClass::ModalWrite,
        OpenSettings | OpenTaskManager => DebugCmdClass::Unsupported,
        _ => DebugCmdClass::NonModal,
    }
}

/// 属性フラグを R/H/S/A/D の文字列へ（表示の属性列と同趣旨）。
#[cfg(feature = "debug-server")]
fn debug_attrs(it: &rerics_core::FileItem) -> String {
    let mut s = String::new();
    if it.is_dir {
        s.push('D');
    }
    if it.readonly {
        s.push('R');
    }
    if it.hidden {
        s.push('H');
    }
    if it.system {
        s.push('S');
    }
    if it.archive {
        s.push('A');
    }
    s
}

fn main() {
    #[cfg(feature = "debug-server")]
    let (debug_port, debug_allow_write, debug_headless) = (
        debug_server::parse_port(),
        debug_server::parse_allow_write(),
        debug_server::parse_headless(),
    );
    #[cfg(not(feature = "debug-server"))]
    let (debug_port, debug_allow_write, debug_headless): (Option<u16>, bool, bool) =
        (None, false, false);
    if let Err(e) = MainWindow::new(debug_port, debug_allow_write, debug_headless).run() {
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
    viewer: ViewerView,
    media: MediaView,
    /// 現在重ねているビューア。None 以外の間はキー入力をビューア操作へ振り向ける。
    active_view: Rc<Cell<ActiveView>>,
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
    #[cfg(feature = "debug-server")]
    debug: debug_server::Bridge,
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
    #[cfg_attr(not(feature = "debug-server"), allow(unused_variables))]
    fn new(debug_port: Option<u16>, debug_allow_write: bool, debug_headless: bool) -> Self {
        // デバッグ制御サーバ起動時は最初から最小化で出すため、生成時に VISIBLE を付けない
        // （付けると CreateWindowEx が一瞬フル表示してしまう）。表示は run_main の cmd_show に任せる。
        let mut style = co::WS::CAPTION
            | co::WS::SYSMENU
            | co::WS::CLIPCHILDREN
            | co::WS::BORDER
            | co::WS::SIZEBOX
            | co::WS::MINIMIZEBOX
            | co::WS::MAXIMIZEBOX;
        if debug_port.is_none() {
            style |= co::WS::VISIBLE;
        }
        let wnd = gui::WindowMain::new(gui::WindowMainOpts {
            title: "Rerics",
            size: gui::dpi(960, 560),
            style,
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
        // メイン領域に重ねるビューア（初期は非表示。layout で位置決め）。
        let viewer = ViewerView::new(&wnd, gui::dpi(0, 0), gui::dpi(400, 400), &config);
        let media = MediaView::new(&wnd, gui::dpi(0, 0), gui::dpi(400, 400), &config);

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

        // 前回の異常終了で残った一時展開を起動時に掃除する。
        Self::clear_archive_temp();

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
        let left_pane = Rc::new(RefCell::new(Pane::restore(&cur.left_path)));
        let right_pane = Rc::new(RefCell::new(Pane::restore(&cur.right_path)));
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
            viewer,
            media,
            active_view: Rc::new(Cell::new(ActiveView::None)),
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
            #[cfg(feature = "debug-server")]
            debug: debug_server::Bridge::new(debug_port, debug_allow_write, debug_headless),
        }
    }

    fn run(&self) -> w::AnyResult<i32> {
        self.setup_events();
        // デバッグ制御サーバ起動時は最初の表示自体を「非アクティブ最小化」にして、
        // フル表示のフラッシュを避ける（VISIBLE も外してあるので真の最小化起動になる）。
        #[cfg(feature = "debug-server")]
        let cmd_show = if self.debug.port.is_some() {
            // headless は完全非表示、通常 debug は非アクティブ最小化。
            if self.debug.headless {
                Some(co::SW::HIDE)
            } else {
                Some(co::SW::SHOWMINNOACTIVE)
            }
        } else {
            None
        };
        #[cfg(not(feature = "debug-server"))]
        let cmd_show: Option<co::SW> = None;
        self.wnd.run_main(cmd_show)
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

        // デバッグ制御サーバからの要求を UI スレッドで捌く（feature 有効時のみ）。
        #[cfg(feature = "debug-server")]
        {
            let this = self.clone();
            let wake = unsafe { co::WM::from_raw(debug_server::WM_DEBUG_WAKE) };
            self.wnd.on().wm(wake, move |_| {
                this.drain_debug_requests();
                Ok(0)
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
            // デバッグ制御サーバ起動時は本体を最小化で立ち上げ、作業の邪魔をしない。
            #[cfg(feature = "debug-server")]
            let debug_minimized = this.debug.port.is_some();
            #[cfg(not(feature = "debug-server"))]
            let debug_minimized = false;
            if let Some(ws) = &this.initial_window {
                let applied = window_state::apply(&this.wnd.hwnd(), ws);
                // 最小化起動時は最大化復元を抑止する（最小化が打ち消されないように）。
                if applied && ws.maximized && !debug_minimized {
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
            // hwnd が有効になったここでデバッグ制御サーバを起動する（指定時のみ）。
            #[cfg(feature = "debug-server")]
            if let Some(port) = this.debug.port {
                let hwnd_ptr = this.wnd.hwnd().ptr() as isize;
                debug_server::start(port, this.debug.queue.clone(), hwnd_ptr);
            }
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
            Self::clear_archive_temp();
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
            Command::ViewFile => {
                self.view_file(is_left)?;
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
                let loc = self.pane(is_left).borrow().loc().clone();
                self.pane(!is_left).borrow_mut().set_loc(loc);
                self.reload_side(!is_left)?;
                return Ok(());
            }
            Command::CurrentToOpposite => {
                let loc = self.pane(!is_left).borrow().loc().clone();
                self.pane(is_left).borrow_mut().set_loc(loc);
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

    /// 左右ペインの現在地を入れ替える（書庫内同士でも成立する）。
    fn swap_paths(&self) -> w::AnyResult<()> {
        let l = self.left_pane.borrow().loc().clone();
        let r = self.right_pane.borrow().loc().clone();
        self.left_pane.borrow_mut().set_loc(r);
        self.right_pane.borrow_mut().set_loc(l);
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
        let items = Pane::restore(path).read();
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
            left_path: self.left_pane.borrow().loc_display(),
            right_path: self.right_pane.borrow().loc_display(),
            left_state: self.view(true).state().borrow().clone(),
            right_state: self.view(false).state().borrow().clone(),
            active_right: self.active_right.get(),
        }
    }

    /// スナップショットをライブ側へ反映し、再描画とフォーカス設定を行う。
    fn load_snapshot(&self, snap: &TabSnapshot) -> w::AnyResult<()> {
        *self.left_pane.borrow_mut() = Pane::restore(&snap.left_path);
        *self.right_pane.borrow_mut() = Pane::restore(&snap.right_path);
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
        let left_path = self.left_pane.borrow().loc_display();
        let right_path = self.right_pane.borrow().loc_display();
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
            // ビューア表示中はキーをビューア操作へ振り向ける（ファイラのキーマップは無効）。
            match this.active_view.get() {
                ActiveView::Text => {
                    let ctrl = w::GetAsyncKeyState(co::VK::CONTROL);
                    let shift = w::GetAsyncKeyState(co::VK::SHIFT);
                    let _ = this.viewer_key(p.vkey_code.raw(), ctrl, shift);
                    return Ok(());
                }
                ActiveView::Media => {
                    let shift = w::GetAsyncKeyState(co::VK::SHIFT);
                    let _ = this.media_key(p.vkey_code.raw(), shift);
                    return Ok(());
                }
                ActiveView::None => {}
            }
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

    /// ビューア表示中のキー操作。固定キー（設定対象外）。
    fn viewer_key(&self, vk: u16, ctrl: bool, shift: bool) -> w::AnyResult<()> {
        use rerics_core::vk;
        const VK_F: u16 = 0x46;
        const VK_F3: u16 = 0x72;
        const VK_Q: u16 = 0x51;
        const VK_B: u16 = 0x42;
        // Ctrl+C は選択コピー（C 単独はエンコーディング切替）。
        if ctrl && vk == vk::C {
            self.viewer.copy_selection()?;
            return Ok(());
        }
        match vk {
            vk::ESCAPE | VK_Q | vk::RETURN => self.close_viewer()?,
            vk::UP => self.viewer.scroll_by(-1)?,
            vk::DOWN => self.viewer.scroll_by(1)?,
            vk::PRIOR => self.viewer.scroll_page(false)?,
            vk::NEXT => self.viewer.scroll_page(true)?,
            vk::HOME => self.viewer.scroll_home()?,
            vk::END => self.viewer.scroll_end()?,
            vk::C => self.viewer.cycle_encoding(true)?,
            VK_B => self.viewer.toggle_mode()?,
            VK_F => self.viewer_search()?,
            // F3=次, Shift+F3=前。
            VK_F3 => self.viewer.find_next(!shift)?,
            _ => {}
        }
        Ok(())
    }

    /// 検索語を入力ダイアログで尋ね、ビューア内を検索する。
    fn viewer_search(&self) -> w::AnyResult<()> {
        let cur = self.viewer.search_term();
        let input = dialog::input_box(
            &self.wnd,
            "検索",
            "検索する文字列（空で解除・F3で次・Shift+F3で前）:",
            &cur,
            dialog::InputMode::Plain,
        );
        if let Some(term) = input {
            self.viewer.set_search(term.trim())?;
        }
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// カーソル下のファイルを種別に応じたビューアで開く（ディレクトリ/親は無視）。
    fn view_file(&self, is_left: bool) -> w::AnyResult<()> {
        let (name, ext) = {
            let state = self.view(is_left).state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent && !it.is_dir => (it.name.clone(), it.extension.clone()),
                _ => return Ok(()),
            }
        };
        match MediaKind::from_extension(&ext) {
            Some(kind) => self.view_media(is_left, kind, &name),
            None => self.view_text(is_left, &name),
        }
    }

    /// テキスト/バイナリビューアで開く（実FS/書庫内とも bytes 直送）。
    fn view_text(&self, is_left: bool, name: &str) -> w::AnyResult<()> {
        let (bytes, truncated) = match self.read_pane_file(is_left, name, viewer::MAX_VIEW_BYTES) {
            Ok(v) => v,
            Err(e) => {
                self.log.error(&format!("ビューアで開けません: {}: {}", name, e));
                return Ok(());
            }
        };
        self.viewer.open(name, bytes, truncated);
        self.show_viewer(ActiveView::Text)
    }

    /// 画像/動画ビューアで開く。実FS は同ディレクトリの閲覧可能メディアを前後送りに、
    /// 書庫内はカーソル下の1ファイルを一時展開して開く（書庫内の前後送りは後段で対応）。
    fn view_media(&self, is_left: bool, _kind: MediaKind, name: &str) -> w::AnyResult<()> {
        let loc = self.pane(is_left).borrow().loc().clone();
        match loc {
            Location::Real(dir) => {
                let target = dir.join(name);
                let mut files: Vec<PathBuf> = Vec::new();
                let mut index = 0;
                {
                    let state = self.view(is_left).state();
                    let s = state.borrow();
                    for it in &s.items {
                        if it.is_dir || it.is_parent {
                            continue;
                        }
                        if MediaKind::from_extension(&it.extension).is_some() {
                            let p = dir.join(&it.name);
                            if p == target {
                                index = files.len();
                            }
                            files.push(p);
                        }
                    }
                }
                if files.is_empty() {
                    files.push(target);
                }
                self.media.open(files, index);
            }
            Location::Archive { archive, inner } => {
                // 同階層の閲覧可能メディアを巡回対象にし（実FS と同じ体験）、表示中の位置を求める。
                // 実パスへの展開は resolver が移動時に1枚ずつ遅延実行する（一括展開しない）。
                let mut entries: Vec<(String, String)> = Vec::new();
                let mut index = 0;
                {
                    let state = self.view(is_left).state();
                    let s = state.borrow();
                    for it in &s.items {
                        if it.is_dir || it.is_parent {
                            continue;
                        }
                        if MediaKind::from_extension(&it.extension).is_some() {
                            if it.name == name {
                                index = entries.len();
                            }
                            entries.push((join_inner_path(&inner, &it.name), it.name.clone()));
                        }
                    }
                }
                if entries.is_empty() {
                    entries.push((join_inner_path(&inner, name), name.to_string()));
                }
                let n = entries.len();
                let entries = Rc::new(entries);
                let archive = archive.clone();
                let log = self.log.clone();
                let resolver: NavResolver = Rc::new(move |i: usize| {
                    let (inner_file, nm) = entries.get(i)?;
                    match Self::extract_entry_to_temp(&archive, inner_file, nm) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            log.error(&format!("書庫内メディアを展開できません: {}: {}", nm, e));
                            None
                        }
                    }
                });
                self.media.open_nav(n, index, resolver);
            }
        }
        self.show_viewer(ActiveView::Media)
    }

    /// 指定ビューアを最前面に出し、もう一方を隠してキー入力を奪う。
    fn show_viewer(&self, which: ActiveView) -> w::AnyResult<()> {
        self.active_view.set(which);
        match which {
            ActiveView::Text => {
                self.media.hwnd().ShowWindow(co::SW::HIDE);
                self.viewer.hwnd().ShowWindow(co::SW::SHOW);
                self.viewer.hwnd().BringWindowToTop()?;
                self.viewer.refresh()?;
            }
            ActiveView::Media => {
                self.viewer.hwnd().ShowWindow(co::SW::HIDE);
                self.media.hwnd().ShowWindow(co::SW::SHOW);
                self.media.hwnd().BringWindowToTop()?;
                self.media.refresh()?;
            }
            ActiveView::None => {}
        }
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// ビューア表示中の画像/動画キー操作（固定キー・設定対象外）。
    fn media_key(&self, vk: u16, _shift: bool) -> w::AnyResult<()> {
        use rerics_core::vk;
        const VK_Q: u16 = 0x51;
        const VK_R: u16 = 0x52;
        const VK_0: u16 = 0x30;
        const VK_1: u16 = 0x31;
        const VK_OEM_PLUS: u16 = 0xBB;
        const VK_OEM_MINUS: u16 = 0xBD;
        const VK_ADD: u16 = 0x6B;
        const VK_SUBTRACT: u16 = 0x6D;
        match vk {
            vk::ESCAPE | VK_Q | vk::RETURN => self.close_viewer()?,
            vk::SPACE => self.media.toggle_play()?,
            vk::LEFT | vk::UP | vk::PRIOR => self.media.navigate(-1)?,
            vk::RIGHT | vk::DOWN | vk::NEXT => self.media.navigate(1)?,
            VK_OEM_PLUS | VK_ADD => self.media.zoom_by(1.25)?,
            VK_OEM_MINUS | VK_SUBTRACT => self.media.zoom_by(0.8)?,
            VK_0 => self.media.fit_to_window()?,
            VK_1 => self.media.actual_size()?,
            VK_R => self.media.rotate()?,
            _ => {}
        }
        Ok(())
    }

    /// ビューアを閉じてファイラ表示へ戻す。
    fn close_viewer(&self) -> w::AnyResult<()> {
        self.media.stop_playback();
        self.active_view.set(ActiveView::None);
        self.viewer.hwnd().ShowWindow(co::SW::HIDE);
        self.media.hwnd().ShowWindow(co::SW::HIDE);
        self.key_sink.hwnd().SetFocus();
        Ok(())
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
        // 書庫内はドライブ容量の対象外（実パスが無い）＝空表示にする。
        let real = self.pane(is_left).borrow().as_real_path().map(Path::to_path_buf);
        match real {
            Some(path) => self.status(is_left).set_right(&drive_info_text(&path)),
            None => self.status(is_left).set_right(""),
        }
    }

    fn pane(&self, is_left: bool) -> &Rc<RefCell<Pane>> {
        if is_left { &self.left_pane } else { &self.right_pane }
    }

    /// 書庫内では未対応の書込み操作をガードする。対象ペインが書庫内なら警告ログを
    /// 出して `true` を返す（呼び側は早期 return する）。展開コピー等は後段で対応。
    fn block_if_archive(&self, is_left: bool, op: &str) -> bool {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn(&format!("書庫内では{}は未対応です", op));
            true
        } else {
            false
        }
    }

    /// 現在ペインのカーソル下ファイル `name` の bytes を取得する（実FS/書庫内 両対応）。
    /// `cap` を超える分は切り詰め、超過していたら `truncated=true` を返す。
    fn read_pane_file(&self, is_left: bool, name: &str, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
        let loc = self.pane(is_left).borrow().loc().clone();
        match loc {
            Location::Real(dir) => read_capped(&dir.join(name), cap),
            Location::Archive { archive, inner } => {
                let backend = open_archive(&archive)?;
                let mut bytes = backend.read(&join_inner_path(&inner, name))?;
                let truncated = bytes.len() > cap;
                bytes.truncate(cap);
                Ok((bytes, truncated))
            }
        }
    }

    /// 書庫から取り出したファイルの一時展開先。**プロセスごとに分離**して、別インスタンス
    /// の掃除が稼働中インスタンスの展開物を壊さないようにする（共有 dir を全削除しない）。
    fn archive_temp_dir() -> PathBuf {
        data_dir()
            .join("cache")
            .join("archive")
            .join(std::process::id().to_string())
    }

    /// 自プロセスの一時展開先のみを削除する（起動時の残骸掃除＋終了時の後始末）。
    /// 他プロセスの dir には触れない。クラッシュで残った他 pid の残骸は手動掃除（cache 配下）。
    fn clear_archive_temp() {
        let _ = std::fs::remove_dir_all(Self::archive_temp_dir());
    }

    /// 書庫内エントリを一時ディレクトリへ展開し実パスを返す。元の名前を保つ。
    /// キーに**書庫の mtime を含め**、同一キーの temp が既に在れば**再展開せず再利用**する
    /// （外部から書庫が更新されれば mtime が変わり別 temp に展開＝古い展開物を二度と参照しない）。
    /// 書込みは「一時名→rename」のアトミック方式で、将来 BG 並行展開を足しても書きかけを
    /// 読む競合が起きないようにしておく（計画 §7.6）。
    fn extract_entry_to_temp(archive: &Path, inner_file: &str, name: &str) -> std::io::Result<PathBuf> {
        // 末尾コンポーネントだけを採り、区切りや ".." での書き出し先逸脱を防ぐ（拡張子は保つ）。
        let safe = Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("entry");
        let stamp = std::fs::metadata(archive)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let key = format!("{}\u{0}{}\u{0}{}", archive.display(), stamp, inner_file);
        let sub = Self::archive_temp_dir().join(format!("{:016x}", hash64(&key)));
        let path = sub.join(safe);
        if path.is_file() {
            return Ok(path);
        }
        std::fs::create_dir_all(&sub)?;
        let backend = open_archive(archive)?;
        let bytes = backend.read(inner_file)?;
        let tmp = sub.join(format!("{}.tmp.{}", safe, std::process::id()));
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// デバッグ制御サーバの要求キューを UI スレッドで処理する（feature 有効時のみ）。
    /// モーダルを開くコマンドは exec がネストループでブロックするため、応答を先に返してから実行する。
    /// その間に届く `/modal/*`・`/state` 等はネストループ経由で本関数が再入して捌く。
    #[cfg(feature = "debug-server")]
    fn drain_debug_requests(&self) {
        loop {
            let item = self.debug.queue.lock().unwrap().pop_front();
            let Some((req, tx)) = item else { break };
            match req {
                debug_server::Request::State { pointer } => {
                    let v = self.debug_state_value();
                    let r = match v.pointer(&pointer) {
                        Some(sub) => debug_server::Response::Json(sub.to_string()),
                        None => debug_server::Response::NotFound,
                    };
                    let _ = tx.send(r);
                }
                debug_server::Request::Presentation { pointer } => {
                    let v = self.debug_presentation_value();
                    let r = match v.pointer(&pointer) {
                        Some(sub) => debug_server::Response::Json(sub.to_string()),
                        None => debug_server::Response::NotFound,
                    };
                    let _ = tx.send(r);
                }
                debug_server::Request::Command { name } => self.debug_dispatch_command(&name, tx),
                debug_server::Request::ViewKey { action } => {
                    let _ = tx.send(self.debug_view_key(&action));
                }
                debug_server::Request::Snapshot { spec } => {
                    let _ = tx.send(self.debug_snapshot(&spec));
                }
                debug_server::Request::ModalKey { key } => {
                    let _ = tx.send(self.debug_modal_key(&key));
                }
                debug_server::Request::ModalText { value } => {
                    let _ = tx.send(self.debug_modal_text(&value));
                }
                debug_server::Request::ModalCommand { role } => {
                    let _ = tx.send(self.debug_modal_command(&role));
                }
            }
        }
    }

    /// `POST /command/<Name>` の振り分け。非モーダルは実行後 state を返す。モーダルを開くコマンドは
    /// 先に応答を返してから exec（ネストループでブロック）。未対応コマンドは弾く。
    #[cfg(feature = "debug-server")]
    fn debug_dispatch_command(&self, name: &str, tx: Sender<debug_server::Response>) {
        let Some(cmd) = Command::from_token(name) else {
            let _ = tx.send(debug_server::Response::BadRequest(format!(
                "unknown command: {name}"
            )));
            return;
        };
        let is_left = !self.active_right.get();
        match debug_command_class(cmd) {
            DebugCmdClass::NonModal => {
                let r = match self.exec(is_left, cmd) {
                    Ok(()) => debug_server::Response::Json(self.debug_state_value().to_string()),
                    Err(e) => debug_server::Response::Error(format!("exec error: {e}")),
                };
                let _ = tx.send(r);
            }
            DebugCmdClass::ModalWrite => {
                if !self.debug.allow_write {
                    let _ = tx.send(debug_server::Response::BadRequest(format!(
                        "write disabled; restart with --debug-allow-write to run: {name}"
                    )));
                    return;
                }
                // モーダルを開く前に応答（exec はモーダルが閉じるまでブロックするため）。
                let _ = tx.send(debug_server::Response::Json(
                    "{\"modal_opening\":true}".to_string(),
                ));
                let _ = self.exec(is_left, cmd);
            }
            DebugCmdClass::Unsupported => {
                let _ = tx.send(debug_server::Response::BadRequest(format!(
                    "command not supported over debug server: {name}"
                )));
            }
        }
    }

    /// 最前面モーダルの HWND を得る（無ければ None）。
    #[cfg(feature = "debug-server")]
    fn debug_modal_hwnd(&self) -> Option<w::HWND> {
        let ptr = debug_server::modal_registry::with_top(|t| t.map(|e| e.modal_ptr))?;
        Some(unsafe { w::HWND::from_ptr(ptr as *mut std::ffi::c_void) })
    }

    /// モーダル内の最初の Edit 子コントロールを探す。
    #[cfg(feature = "debug-server")]
    fn debug_modal_edit(modal: &w::HWND) -> Option<w::HWND> {
        let mut found: Option<w::HWND> = None;
        modal.EnumChildWindows(|c| {
            if c.GetClassName().map(|s| s.eq_ignore_ascii_case("Edit")).unwrap_or(false) {
                found = Some(c);
                false
            } else {
                true
            }
        });
        found
    }

    /// `POST /modal/key/<key>`：開いているモーダルへキー送出（enter/esc/y/n/tab）。
    #[cfg(feature = "debug-server")]
    fn debug_modal_key(&self, key: &str) -> debug_server::Response {
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        let vk: u16 = match key.to_ascii_lowercase().as_str() {
            "enter" | "return" => 0x0D,
            "esc" | "escape" => 0x1B,
            "tab" => 0x09,
            "y" => 0x59,
            "n" => 0x4E,
            _ => return debug_server::Response::BadRequest(format!("unknown modal key: {key}")),
        };
        unsafe {
            let _ = modal.PostMessage(w::msg::WndMsg {
                msg_id: co::WM::KEYDOWN,
                wparam: vk as usize,
                lparam: 0,
            });
            let _ = modal.PostMessage(w::msg::WndMsg {
                msg_id: co::WM::from_raw(0x0101), // WM_KEYUP
                wparam: vk as usize,
                lparam: 0,
            });
        }
        debug_server::Response::Json(self.debug_state_value().to_string())
    }

    /// `POST /modal/text`：開いているモーダルの入力欄へ文字列を設定する。
    #[cfg(feature = "debug-server")]
    fn debug_modal_text(&self, value: &str) -> debug_server::Response {
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        match Self::debug_modal_edit(&modal) {
            Some(edit) => {
                let _ = edit.SetWindowText(value);
                debug_server::Response::Json(self.debug_state_value().to_string())
            }
            None => debug_server::Response::BadRequest("modal has no text field".into()),
        }
    }

    /// `POST /modal/command/<role>`：開いているモーダルのボタンを役割名/ラベル/ctrl_id で押す。
    #[cfg(feature = "debug-server")]
    fn debug_modal_command(&self, role: &str) -> debug_server::Response {
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        // 役割名・数値 ctrl_id・ラベル部分一致から ctrl_id を解決する。
        let id = debug_server::modal_registry::with_top(|t| {
            let e = t?;
            let r = role.to_ascii_lowercase();
            if r == "ok" || r == "yes" {
                return Some(1u16);
            }
            if r == "cancel" {
                return Some(2u16);
            }
            if let Ok(n) = role.parse::<u16>() {
                return Some(n);
            }
            e.buttons
                .iter()
                .find(|(label, _)| label.replace('&', "").to_lowercase().contains(&r))
                .map(|(_, id)| *id)
        });
        let Some(id) = id else {
            return debug_server::Response::BadRequest(format!("unknown modal button: {role}"));
        };
        let Ok(btn) = modal.GetDlgItem(id) else {
            return debug_server::Response::BadRequest(format!("button id {id} not found"));
        };
        // 親へ WM_COMMAND(BN_CLICKED) を送る（winsafe の bn_clicked が ctrl_id で振り分ける）。
        unsafe {
            let _ = modal.PostMessage(w::msg::WndMsg {
                msg_id: co::WM::COMMAND,
                wparam: id as usize,
                lparam: btn.ptr() as isize,
            });
        }
        debug_server::Response::Json(self.debug_state_value().to_string())
    }

    /// `POST /view/key/<action>`：重ね表示中ビューアの操作（next/prev/close）。
    #[cfg(feature = "debug-server")]
    fn debug_view_key(&self, action: &str) -> debug_server::Response {
        let r = match action {
            "next" => self.media.navigate(1),
            "prev" => self.media.navigate(-1),
            "close" => self.close_viewer(),
            _ => {
                return debug_server::Response::BadRequest(format!("unknown view action: {action}"));
            }
        };
        match r {
            Ok(()) => debug_server::Response::Json(self.debug_state_value().to_string()),
            Err(e) => debug_server::Response::Error(format!("view key error: {e}")),
        }
    }

    /// `GET /snapshot[/<spec>]`：画面 PNG を返す。spec は全体／名前付き要素／数値範囲／要素相対範囲。
    /// 名前付き要素の矩形は復帰後レイアウトで確定するため、撮影準備（復帰＋再レイアウト）を先に行う。
    #[cfg(feature = "debug-server")]
    fn debug_snapshot(&self, spec: &str) -> debug_server::Response {
        let was_min = self.debug_prepare_capture();
        let result = self.debug_snapshot_inner(spec);
        if was_min {
            self.wnd.hwnd().ShowWindow(co::SW::SHOWMINNOACTIVE);
        }
        result
    }

    /// 撮影準備：最小化中なら復帰し、ループ停止中でも子が正しい位置・内容になるよう手動再レイアウト＋同期再描画。
    /// 撮影は窓自身の DC から行う（オクルージョン非依存）ので最前面化は不要。戻り値は「元が最小化だったか」。
    #[cfg(feature = "debug-server")]
    fn debug_prepare_capture(&self) -> bool {
        let hwnd = self.wnd.hwnd();
        let was_min = hwnd.IsIconic();
        if was_min {
            hwnd.ShowWindow(co::SW::SHOWNOACTIVATE);
            // WM_SIZE はループ停止中で届かないので、復帰後サイズで明示的に再レイアウトする。
            let _ = self.layout();
        }
        was_min
    }

    #[cfg(feature = "debug-server")]
    fn debug_snapshot_inner(&self, spec: &str) -> debug_server::Response {
        let segs: Vec<&str> = spec.split('/').filter(|s| !s.is_empty()).collect();
        let (base_name, region): (Option<&str>, Option<&str>) = match segs.as_slice() {
            [] => (None, None),
            [a] if parse_region(a).is_some() => (None, Some(*a)),
            [a] => (Some(*a), None),
            [a, b] => (Some(*a), Some(*b)),
            _ => return debug_server::Response::BadRequest(format!("bad snapshot spec: {spec}")),
        };
        // 対象窓と基準矩形を決める：`modal`/`modal_*` はモーダル窓、それ以外は main クライアント。
        let is_modal = base_name
            .map(|n| n == "modal" || n.starts_with("modal_"))
            .unwrap_or(false);
        let (buf, cw, ch, base) = if is_modal {
            let Some(modal) = self.debug_modal_hwnd() else {
                return debug_server::Response::BadRequest("no modal open".into());
            };
            // モーダルは標準コントロール製なので PrintWindow（WM_PRINTCLIENT 応答）で撮る。
            match self.capture_modal_print(&modal) {
                Ok((b, w, h)) => (b, w, h, (0, 0, w, h)),
                Err(e) => return debug_server::Response::Error(format!("snapshot error: {e}")),
            }
        } else {
            let base = match base_name {
                Some(name) => match self.debug_rect(name) {
                    Some(r) => r,
                    None => {
                        return debug_server::Response::BadRequest(format!(
                            "unknown snapshot target: {name}"
                        ));
                    }
                },
                None => match self.debug_rect("client") {
                    Some(r) => r,
                    None => return debug_server::Response::Error("no client rect".into()),
                },
            };
            match self.capture_render_bgra() {
                Ok((b, w, h)) => (b, w, h, base),
                Err(e) => return debug_server::Response::Error(format!("snapshot error: {e}")),
            }
        };
        let rect = match region {
            Some(rs) => match parse_region(rs) {
                Some((rx, ry, rw, rh)) => (base.0 + rx, base.1 + ry, rw, rh),
                None => return debug_server::Response::BadRequest(format!("bad region: {rs}")),
            },
            None => base,
        };
        match Self::crop_bgra_to_png(&buf, cw, ch, rect) {
            Ok(png) => debug_server::Response::Png(png),
            Err(e) => debug_server::Response::Error(format!("snapshot error: {e}")),
        }
    }

    /// 名前付き要素のクライアント座標矩形 `(x,y,w,h)`。`_left`/`_right` 省略時はアクティブ側。
    #[cfg(feature = "debug-server")]
    fn debug_rect(&self, name: &str) -> Option<(i32, i32, i32, i32)> {
        let a = !self.active_right.get();
        match name {
            "full" | "client" | "window" => {
                let rc = self.wnd.hwnd().GetClientRect().ok()?;
                Some((0, 0, rc.right - rc.left, rc.bottom - rc.top))
            }
            "tab_bar" => self.rect_in_client(self.tab_bar.hwnd()),
            "log" => self.rect_in_client(self.log.hwnd()),
            "path_bar" => self.rect_in_client(self.bar(a).hwnd()),
            "path_bar_left" => self.rect_in_client(self.bar(true).hwnd()),
            "path_bar_right" => self.rect_in_client(self.bar(false).hwnd()),
            "list" => self.rect_in_client(self.view(a).hwnd()),
            "list_left" => self.rect_in_client(self.view(true).hwnd()),
            "list_right" => self.rect_in_client(self.view(false).hwnd()),
            "status_bar" => self.rect_in_client(self.status(a).hwnd()),
            "status_bar_left" => self.rect_in_client(self.status(true).hwnd()),
            "status_bar_right" => self.rect_in_client(self.status(false).hwnd()),
            "pane" => self.pane_bbox(a),
            "pane_left" => self.pane_bbox(true),
            "pane_right" => self.pane_bbox(false),
            "cursor" => self.cursor_rect(a),
            "cursor_left" => self.cursor_rect(true),
            "cursor_right" => self.cursor_rect(false),
            // モーダルはクライアント座標に収まらない別窓なので、ここでは扱わず capture 側で別処理。
            _ => None,
        }
    }

    /// 子コントロールの矩形を main 窓のクライアント座標へ変換する。
    #[cfg(feature = "debug-server")]
    fn rect_in_client(&self, child: &w::HWND) -> Option<(i32, i32, i32, i32)> {
        let wr = child.GetWindowRect().ok()?;
        let origin = self.wnd.hwnd().ClientToScreen(w::POINT { x: 0, y: 0 }).ok()?;
        Some((
            wr.left - origin.x,
            wr.top - origin.y,
            wr.right - wr.left,
            wr.bottom - wr.top,
        ))
    }

    /// ペイン全体（パスバー＋一覧＋ステータス）の外接矩形。
    #[cfg(feature = "debug-server")]
    fn pane_bbox(&self, is_left: bool) -> Option<(i32, i32, i32, i32)> {
        let a = self.rect_in_client(self.bar(is_left).hwnd())?;
        let b = self.rect_in_client(self.view(is_left).hwnd())?;
        let c = self.rect_in_client(self.status(is_left).hwnd())?;
        let x0 = a.0.min(b.0).min(c.0);
        let y0 = a.1.min(b.1).min(c.1);
        let x1 = (a.0 + a.2).max(b.0 + b.2).max(c.0 + c.2);
        let y1 = (a.1 + a.3).max(b.1 + b.3).max(c.1 + c.3);
        Some((x0, y0, x1 - x0, y1 - y0))
    }

    /// アクティブ/指定ペインのカーソル行の矩形（一覧内の行位置を main クライアント座標へ）。
    #[cfg(feature = "debug-server")]
    fn cursor_rect(&self, is_left: bool) -> Option<(i32, i32, i32, i32)> {
        let lr = self.rect_in_client(self.view(is_left).hwnd())?;
        let (cx, cy, cw, ch) = self.view(is_left).cursor_row_rect()?;
        Some((lr.0 + cx, lr.1 + cy, cw, ch))
    }


    /// 子コントロールを自前 bitmap へ `render_to` し、合成 DC の位置へ BitBlt する。
    #[cfg(feature = "debug-server")]
    fn render_view_into(
        &self,
        target: &w::HDC,
        win_dc: &w::HDC,
        child: &w::HWND,
        draw: impl FnOnce(&w::HDC, i32, i32) -> w::AnyResult<()>,
    ) -> w::AnyResult<()> {
        let Some((x, y, w, h)) = self.rect_in_client(child) else {
            return Ok(());
        };
        if w <= 0 || h <= 0 {
            return Ok(());
        }
        let vdc = win_dc.CreateCompatibleDC()?;
        let vbmp = win_dc.CreateCompatibleBitmap(w, h)?;
        let _s = vdc.SelectObject(&*vbmp)?;
        draw(&vdc, w, h)?;
        target.BitBlt(
            w::POINT { x, y },
            w::SIZE { cx: w, cy: h },
            &vdc,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    /// モーダル（標準コントロール製）を `PrintWindow` でクライアント領域を BGRA(top-down) に撮る。
    /// 標準コントロールは `WM_PRINTCLIENT` に応答するので、自前描画と違い PrintWindow が機能する。
    #[cfg(feature = "debug-server")]
    fn capture_modal_print(&self, modal: &w::HWND) -> w::AnyResult<(Vec<u8>, i32, i32)> {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn PrintWindow(
                hwnd: *mut std::ffi::c_void,
                hdc: *mut std::ffi::c_void,
                flags: u32,
            ) -> i32;
        }
        const PW_CLIENTONLY: u32 = 1;
        const PW_RENDERFULLCONTENT: u32 = 2;
        let crc = modal.GetClientRect()?;
        let (cw, ch) = (crc.right - crc.left, crc.bottom - crc.top);
        if cw <= 0 || ch <= 0 {
            return Err("empty modal client".into());
        }
        let win_dc = modal.GetDC()?;
        let memdc = win_dc.CreateCompatibleDC()?;
        let bmp = win_dc.CreateCompatibleBitmap(cw, ch)?;
        {
            let _sel = memdc.SelectObject(&*bmp)?;
            unsafe {
                PrintWindow(modal.ptr(), memdc.ptr(), PW_CLIENTONLY | PW_RENDERFULLCONTENT);
            }
        }
        let mut bmi = w::BITMAPINFO::default();
        bmi.bmiHeader.biWidth = cw;
        bmi.bmiHeader.biHeight = -ch;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = co::BI::RGB;
        let mut buf = vec![0u8; (cw as usize) * (ch as usize) * 4];
        unsafe {
            memdc.GetDIBits(&*bmp, 0, ch as u32, Some(&mut buf), &mut bmi, co::DIB::RGB_COLORS)?;
        }
        Ok((buf, cw, ch))
    }

    /// メインクライアント全体を各 view の `render_to` から合成し BGRA(top-down) で得る。
    /// 窓のピクセルを読まず状態から描き起こすため、**非表示（headless）でも決定論的に撮れる**。
    #[cfg(feature = "debug-server")]
    fn capture_render_bgra(&self) -> w::AnyResult<(Vec<u8>, i32, i32)> {
        let hwnd = self.wnd.hwnd();
        let crc = hwnd.GetClientRect()?;
        let (cw, ch) = (crc.right - crc.left, crc.bottom - crc.top);
        if cw <= 0 || ch <= 0 {
            return Err("empty client".into());
        }
        let win_dc = hwnd.GetDC()?;
        let target = win_dc.CreateCompatibleDC()?;
        let bmp = win_dc.CreateCompatibleBitmap(cw, ch)?;
        let _sel = target.SelectObject(&*bmp)?;
        // 隙間（chrome）の下地をシステム 3D グレーで塗る。
        let base = w::HBRUSH::CreateSolidBrush(w::GetSysColor(co::COLOR::BTNFACE))?;
        target.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &base)?;

        self.render_view_into(&target, &win_dc, self.tab_bar.hwnd(), |d, w, h| {
            self.tab_bar.render_to(d, w, h)
        })?;
        for is_left in [true, false] {
            self.render_view_into(&target, &win_dc, self.bar(is_left).hwnd(), |d, w, h| {
                self.bar(is_left).render_to(d, w, h)
            })?;
            self.render_view_into(&target, &win_dc, self.view(is_left).hwnd(), |d, w, h| {
                self.view(is_left).render_to(d, w, h)
            })?;
            self.render_view_into(&target, &win_dc, self.status(is_left).hwnd(), |d, w, h| {
                self.status(is_left).render_to(d, w, h)
            })?;
        }
        self.render_view_into(&target, &win_dc, self.splitter.hwnd(), |d, w, h| {
            self.splitter.render_to(d, w, h)
        })?;
        self.render_view_into(&target, &win_dc, self.log.hwnd(), |d, w, h| {
            self.log.render_to(d, w, h)
        })?;
        // 重ね表示中のビューア/メディアを最前面として上書きする。
        match self.active_view.get() {
            ActiveView::Text => {
                self.render_view_into(&target, &win_dc, self.viewer.hwnd(), |d, w, h| {
                    self.viewer.render_to(d, w, h)
                })?;
            }
            ActiveView::Media => {
                self.render_view_into(&target, &win_dc, self.media.hwnd(), |d, w, h| {
                    self.media.render_to(d, w, h)
                })?;
            }
            ActiveView::None => {}
        }

        let mut bmi = w::BITMAPINFO::default();
        bmi.bmiHeader.biWidth = cw;
        bmi.bmiHeader.biHeight = -ch;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = co::BI::RGB;
        let mut buf = vec![0u8; (cw as usize) * (ch as usize) * 4];
        unsafe {
            target.GetDIBits(&*bmp, 0, ch as u32, Some(&mut buf), &mut bmi, co::DIB::RGB_COLORS)?;
        }
        Ok((buf, cw, ch))
    }

    /// 合成済み BGRA(top-down) バッファから矩形を切り出して PNG にする。
    #[cfg(feature = "debug-server")]
    fn crop_bgra_to_png(
        full: &[u8],
        cw: i32,
        ch: i32,
        rect: (i32, i32, i32, i32),
    ) -> w::AnyResult<Vec<u8>> {
        let (rx, ry, rw, rh) = rect;
        if rw <= 0 || rh <= 0 {
            return Err("empty snapshot region".into());
        }
        let mut out = vec![0u8; (rw as usize) * (rh as usize) * 4];
        for row in 0..rh {
            let sy = ry + row;
            if sy < 0 || sy >= ch {
                continue;
            }
            for col in 0..rw {
                let sx = rx + col;
                if sx < 0 || sx >= cw {
                    continue;
                }
                let si = ((sy * cw + sx) * 4) as usize;
                let di = ((row * rw + col) * 4) as usize;
                // BGRA → RGBA、アルファは不透明に。
                out[di] = full[si + 2];
                out[di + 1] = full[si + 1];
                out[di + 2] = full[si];
                out[di + 3] = 255;
            }
        }
        let img = image::RgbaImage::from_raw(rw as u32, rh as u32, out)
            .ok_or("rgba buffer size mismatch")?;
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)?;
        Ok(buf.into_inner())
    }

    /// 現在の UI 状態を JSON 値で組む（画面構成要素ほぼ全部・サブツリーは呼び側が JSON Pointer で抽出）。
    #[cfg(feature = "debug-server")]
    fn debug_state_value(&self) -> serde_json::Value {
        use serde_json::json;
        let active_view = match self.active_view.get() {
            ActiveView::None => "none",
            ActiveView::Text => "text",
            ActiveView::Media => "media",
        };
        let media = if matches!(self.active_view.get(), ActiveView::Media) {
            let (index, total) = self.media.nav_position();
            json!({ "index": index, "total": total, "title": self.media.title() })
        } else {
            serde_json::Value::Null
        };
        let tabs: Vec<serde_json::Value> = self
            .tabs
            .borrow()
            .iter()
            .map(|t| {
                json!({ "left": t.left_path, "right": t.right_path, "active_right": t.active_right })
            })
            .collect();
        let log_lines: Vec<serde_json::Value> = self
            .log
            .tail(50)
            .into_iter()
            .map(|(level, text)| json!({ "level": level, "text": text }))
            .collect();
        let modal = debug_server::modal_registry::with_top(|t| match t {
            None => serde_json::Value::Null,
            Some(e) => {
                let input = if e.has_input {
                    let m = unsafe { w::HWND::from_ptr(e.modal_ptr as *mut std::ffi::c_void) };
                    Self::debug_modal_edit(&m)
                        .and_then(|ed| ed.GetWindowText().ok())
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null)
                } else {
                    serde_json::Value::Null
                };
                json!({
                    "kind": e.kind,
                    "title": e.title,
                    "prompt": e.prompt,
                    "has_input": e.has_input,
                    "input": input,
                    "buttons": e.buttons.iter().map(|(l, id)| json!({ "label": l, "id": id })).collect::<Vec<_>>(),
                })
            }
        });
        json!({
            "window": {
                "title": self.wnd.hwnd().GetWindowText().unwrap_or_default(),
                "maximized": self.maximized.get(),
                "split_ratio": self.split_ratio.get(),
            },
            "active_pane": if self.active_right.get() { "right" } else { "left" },
            "active_view": active_view,
            "panes": {
                "left": self.debug_pane_json(true),
                "right": self.debug_pane_json(false),
            },
            "modal": modal,
            "media": media,
            "tab_bar": { "active": self.active.get(), "labels": self.tab_bar.labels() },
            "tabs": { "active": self.active.get(), "count": tabs.len(), "items": tabs },
            "log": { "lines": log_lines },
        })
    }

    /// 解決済みの外見情報を JSON 値で組む（設定が描画に反映されているかのテスト用）。
    /// 上位＝解決後の設定値（テーマ/配色/フォント/レイアウト寸法）、panes＝各ペインが
    /// 実際に保持している値（apply_config の配線確認）。いずれも `paint_to` が読むのと同じ出どころ。
    #[cfg(feature = "debug-server")]
    fn debug_presentation_value(&self) -> serde_json::Value {
        use serde_json::json;
        let cfg = self.config.borrow();
        json!({
            "theme": serde_json::to_value(&cfg.theme).unwrap_or_default(),
            "resolved_colors": serde_json::to_value(cfg.active_colors()).unwrap_or_default(),
            "font": serde_json::to_value(&cfg.font).unwrap_or_default(),
            "layout": serde_json::to_value(&cfg.layout).unwrap_or_default(),
            "panes": {
                "left": self.view(true).presentation(),
                "right": self.view(false).presentation(),
            },
        })
    }

    /// 片側ペインの状態を JSON 値で組む（表示対象 items・カーソル・マーク・属性・ソート等）。
    #[cfg(feature = "debug-server")]
    fn debug_pane_json(&self, is_left: bool) -> serde_json::Value {
        use serde_json::json;
        let (location, is_archive) = {
            let pane = self.pane(is_left).borrow();
            (pane.loc_display(), pane.is_archive())
        };
        let view = self.view(is_left);
        let page_rows = view.page_rows();
        let st = view.state();
        let s = st.borrow();
        let (sel_count, sel_size) = s.selected_count_size();
        let visible_end = (s.scroll_top + page_rows).min(s.items.len());
        let items: Vec<serde_json::Value> = s
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| debug_item_json(it, i == s.cursor))
            .collect();
        let columns: Vec<serde_json::Value> = s
            .columns
            .iter()
            .map(|c| {
                json!({
                    "kind": format!("{:?}", c.kind),
                    "text": c.text,
                    "width": c.width,
                    "align": format!("{:?}", c.align),
                })
            })
            .collect();
        let mask = self.mask(is_left).borrow().clone();
        json!({
            "location": location,
            "is_archive": is_archive,
            "path_bar": self.bar(is_left).text(),
            "status_bar": {
                "left": self.status(is_left).left_text(),
                "right": self.status(is_left).right_text(),
            },
            "cursor": s.cursor,
            "scroll_top": s.scroll_top,
            "page_rows": page_rows,
            "visible": [s.scroll_top, visible_end],
            "mask": mask,
            "sort": { "type": format!("{:?}", s.sort_type), "reverse": s.sort_reverse },
            "selected_count": sel_count,
            "selected_size": sel_size,
            "columns": columns,
            "items": items,
        })
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
        let path = self.pane(is_left).borrow().loc_display();
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
        if self.block_if_archive(is_left, "ディレクトリの作成") {
            return Ok(());
        }
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
        if self.block_if_archive(is_left, "ファイルの作成") {
            return Ok(());
        }
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
        let verb = if move_it { "移動" } else { "コピー" };
        // 書庫→実FS の取り出し（展開コピー）は段階5で対応・書庫への書込みは未対応。
        if self.pane(is_left).borrow().is_archive() {
            self.log
                .warn(&format!("書庫からの{}（取り出し）は未対応です", verb));
            return Ok(());
        }
        if self.pane(!is_left).borrow().is_archive() {
            self.log.warn(&format!("書庫への{}は未対応です", verb));
            return Ok(());
        }
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
        if self.block_if_archive(is_left, "名前の変更") {
            return Ok(());
        }
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
        if self.block_if_archive(is_left, "削除") {
            return Ok(());
        }
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
            if self.pane(is_left).borrow_mut().enter(&name, is_dir) {
                self.reload_side(is_left)?;
            }
        } else {
            // 書庫ファイルなら潜る（zip 等）。
            if self.pane(is_left).borrow_mut().enter(&name, is_dir) {
                self.reload_side(is_left)?;
                return Ok(());
            }
            // 開く対象の実パスを得る（書庫内は一時展開してから関連付け起動）。
            let loc = self.pane(is_left).borrow().loc().clone();
            let path = match loc {
                Location::Real(dir) => dir.join(&name),
                Location::Archive { archive, inner } => {
                    let inner_file = join_inner_path(&inner, &name);
                    match Self::extract_entry_to_temp(&archive, &inner_file, &name) {
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
        // ビューアはタブバー下のメイン領域（ペイン＋ログ）全体を覆う。表示状態は維持。
        let view_h = (total_h - bars_y).max(0);
        place(self.viewer.hwnd(), 0, bars_y, total_w, view_h)?;
        place(self.media.hwnd(), 0, bars_y, total_w, view_h)?;
        match self.active_view.get() {
            ActiveView::Text => self.viewer.hwnd().BringWindowToTop()?,
            ActiveView::Media => self.media.hwnd().BringWindowToTop()?,
            ActiveView::None => {}
        }
        self.tab_bar.refresh()?;
        self.log.refresh()?;
        self.viewer.refresh()?;
        self.media.refresh()?;
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

/// ファイルを最大 `cap` バイトまで読む。`cap` を超える場合は先頭 `cap` バイトと
/// 切り詰めフラグ `true` を返す。
fn read_capped(path: &Path, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(cap as u64 + 1).read_to_end(&mut buf)?;
    let truncated = buf.len() > cap;
    buf.truncate(cap);
    Ok((buf, truncated))
}

/// 書庫内 dir パス `inner` と子名 `name` を '/' で連結する（inner 空ならそのまま name）。
fn join_inner_path(inner: &str, name: &str) -> String {
    if inner.is_empty() {
        name.to_string()
    } else {
        format!("{inner}/{name}")
    }
}

/// 文字列の 64bit ハッシュ（一時展開のサブディレクトリ名用・1 起動内で一意なら十分）。
fn hash64(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// パスを正規化する。実在ディレクトリ、または途中に実在する書庫ファイルを含む
/// 書庫内パスならそのまま。どちらでもなければ `fallback` を返す。
fn normalize_path(path: &str, fallback: &str) -> String {
    if path.is_empty() {
        return fallback.to_owned();
    }
    if std::path::Path::new(path).is_dir() {
        return path.to_owned();
    }
    if rerics_core::Location::parse(path).is_archive() {
        return path.to_owned();
    }
    fallback.to_owned()
}
