mod chrome;
// 常時ビルド（純粋関数＋ユニットテスト）。呼び出し元は debug-server feature 下なので OFF 時は未使用。
#[allow(dead_code)]
mod debug_json;
#[cfg(feature = "debug-server")]
mod debug_server;
mod dialog;
mod file_list;
mod icons;
mod log_view;
mod menu;
mod pane_view;
mod path_bar;
mod settings_dialog;
mod shell;
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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use file_list::FileListView;
use log_view::LogView;
use media_view::{MediaView, NavResolver};
use pane_view::PaneView;
use path_bar::PathBarView;
use status_bar::StatusBarView;
use tab_bar::TabBar;
use task::{ArchiveOutcome, ChannelHost, OpKind, TaskControl, TaskEntry, WorkerEvent};
use viewer::ViewerView;
use rerics_core::{
    Column, Command, Config, FileListState, Invocation, KeyChord, KeyMap, Location, LogLevel,
    MacroAbort, MacroCtx, MacroHost, MediaKind, Pane, SortType, WindowState, data_dir, expand_macros,
    messages, open_archive,
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
    /// 即実行して状態を返せる（ナビ/マーク/ソート/タブ等）。
    NonModal,
    /// モーダルを開く可能性がある読取系（ビューア等。書込みではないので allow_write 不要）。
    /// 単一スレッドの HTTP がモーダル待ちで詰まらないよう、exec の前に応答を返す。
    MaybeModal,
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
        MakeDirectory | CreateFile | Rename | Delete | Copy | Move | Compress | Extract
        | RenameSequenceDialog | SendToRecycled | CreateShortcut | ClipPaste => {
            DebugCmdClass::ModalWrite
        }
        // View/ViewFile は暗号化書庫でパスワード入力モーダルを開き得る（書込みではない）。
        // View はディレクトリ/書庫へ潜る場合もあるが、いずれも非破壊。
        View | ViewFile => DebugCmdClass::MaybeModal,
        // 履歴ダイアログは読取モーダル（リスト選択）を開く（書込みではない）。
        PathHistoryDialog => DebugCmdClass::MaybeModal,
        // ディレクトリ移動は入力/フォルダ選択マクロでモーダルを開き得る（移動は書込みではない）。
        ChangeDirectory | ChangeDirectoryDialog => DebugCmdClass::MaybeModal,
        // ドライブ選択はリスト選択モーダルを開く（移動は書込みではない）。
        ChangeDriveDialog => DebugCmdClass::MaybeModal,
        // ジャンプ（リスト選択）・登録（ラベル入力）はモーダルを開く。登録は config.toml を
        // 書くがユーザファイル操作ではないので allow_write は要さない。
        JumpDialog | RegisterPath => DebugCmdClass::MaybeModal,
        // キー割り当て一覧はリスト選択モーダル（読取専用・選択結果は使わない）。
        KeyBindsDialog => DebugCmdClass::MaybeModal,
        // インクリメンタルサーチは入力モーダル（打鍵追従でカーソル移動・読取のみ）。
        IncrementalSearchDialog => DebugCmdClass::MaybeModal,
        // ソート設定はモーダルを開く（並べ替えのみ＝書込みではない）。modal_registry に登録
        // 済みなので開いて OK/Cancel で閉じられる（ラジオ値そのものの選択は未対応＝種別変更の
        // ロジックは引数コマンド Sort(type) でも駆動・検証できる）。
        SortDialog => DebugCmdClass::MaybeModal,
        OpenSettings | OpenTaskManager => DebugCmdClass::Unsupported,
        _ => DebugCmdClass::NonModal,
    }
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
    /// ファイル一覧のシェルアイコンキャッシュ（左右ペイン共有・非同期ローダ保持）。
    icon_cache: Rc<icons::IconCache>,
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
    /// 書庫内メディア閲覧中の先読み（BG プリフェッチ）スレッドへの停止フラグ。
    /// 別の書庫を開く/ビューアを閉じる際に立てて旧スレッドを止める（§7.6）。
    media_prefetch: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    /// 書庫ごとに一度入力したパスワードを保持する（同一書庫の他エントリで再入力させない）。
    archive_passwords: Rc<RefCell<std::collections::HashMap<PathBuf, Vec<u8>>>>,
    /// 一括展開済みの非ランダムアクセス書庫（ソリッド7z 等）→ 展開先 temp_root。
    /// ここに在る書庫は一覧/閲覧/取り出しを temp_root 配下の実FSから提供する（§7.4・O(n²)回避）。
    archive_extracted: Rc<RefCell<std::collections::HashMap<PathBuf, PathBuf>>>,
    /// 現在ワーカが一括展開中の書庫（二重起動＝同一 temp への並行展開を防ぐ）。
    archive_extracting: Rc<RefCell<std::collections::HashSet<PathBuf>>>,
    /// temp を作った書庫 → その temp ルート dir。セッション中掃除（参照ゼロで回収）の元。
    archive_temp_dirs: Rc<RefCell<std::collections::HashMap<PathBuf, PathBuf>>>,
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

/// マクロのダイアログ系（`<I:>`/`<FOLDERDIALOG>`）を GUI で供給するホスト。
struct DialogMacroHost<'a> {
    app: &'a MainWindow,
}

impl MacroHost for DialogMacroHost<'_> {
    fn prompt(&self, title: &str) -> Option<String> {
        let message = if title.is_empty() { "値を入力して下さい。" } else { title };
        dialog::input_box(&self.app.wnd, "入力", message, "", dialog::InputMode::Plain)
    }

    fn choose_folder(&self, title: &str) -> Option<String> {
        shell::choose_folder(self.app.wnd.hwnd().ptr(), title)
            .map(|p| p.to_string_lossy().into_owned())
    }

    fn choose_open_file(&self, title: &str) -> Option<String> {
        shell::choose_file(self.app.wnd.hwnd().ptr(), title, false)
            .map(|p| p.to_string_lossy().into_owned())
    }

    fn choose_save_file(&self, title: &str) -> Option<String> {
        shell::choose_file(self.app.wnd.hwnd().ptr(), title, true)
            .map(|p| p.to_string_lossy().into_owned())
    }
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
        // シェルアイコンのキャッシュを左右ペインで共有する。
        let icon_cache = Rc::new(icons::IconCache::new());
        left.list().set_icon_cache(icon_cache.clone());
        right.list().set_icon_cache(icon_cache.clone());
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

        // 前回の異常終了で残った一時展開を起動時に掃除する。自pid分を消し、さらに死んでる
        // 他pid（クラッシュ残骸）の dir も裏で回収する（生存インスタンスの temp は触らない）。
        Self::clear_archive_temp();
        Self::sweep_dead_pid_temps();

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
                    left_state: Self::build_state_for(
                        &left_path,
                        &config.columns,
                        t.sort_left,
                        t.sort_left_reverse,
                    ),
                    right_state: Self::build_state_for(
                        &right_path,
                        &config.columns,
                        t.sort_right,
                        t.sort_right_reverse,
                    ),
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
                left_state: Self::build_state_for(
                    &left_path,
                    &config.columns,
                    SortType::default(),
                    false,
                ),
                right_state: Self::build_state_for(
                    &right_path,
                    &config.columns,
                    SortType::default(),
                    false,
                ),
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
            icon_cache,
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
            media_prefetch: Rc::new(RefCell::new(None)),
            archive_passwords: Rc::new(RefCell::new(std::collections::HashMap::new())),
            archive_extracted: Rc::new(RefCell::new(std::collections::HashMap::new())),
            archive_extracting: Rc::new(RefCell::new(std::collections::HashSet::new())),
            archive_temp_dirs: Rc::new(RefCell::new(std::collections::HashMap::new())),
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
                this.exec(is_left, &Invocation::bare(cmd))?;
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

        // 非同期アイコンローダの完了通知。結果を取り込み、両ペインを再描画する。
        let this = self.clone();
        let icons_ready = unsafe { co::WM::from_raw(icons::WM_ICONS_READY) };
        self.wnd.on().wm(icons_ready, move |_| {
            if this.icon_cache.drain_results() {
                let _ = this.view(true).refresh();
                let _ = this.view(false).refresh();
            }
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
            // 非同期アイコンローダを起動（完了は WM_ICONS_READY で受ける）。
            this.icon_cache.start(this.wnd.hwnd().ptr() as isize);
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
            // headless 時は本体を画面外へ送る。モーダルは親ウィンドウの中央に作られるので、
            // これでモーダルも画面外に出て、headless 検証中の一瞬のフラッシュが見えなくなる
            // （バックグラウンドプロセスゆえフォーカスは元々奪わない）。モーダルは VISIBLE の
            // ままなので /snapshot/modal（PrintWindow）は画面外でもそのまま撮れる。
            #[cfg(feature = "debug-server")]
            if this.debug.headless {
                let _ = this.wnd.hwnd().SetWindowPos(
                    w::HwndPlace::None,
                    w::POINT::with(-32000, -32000),
                    w::SIZE::default(),
                    co::SWP::NOSIZE | co::SWP::NOZORDER | co::SWP::NOACTIVATE,
                );
            }
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
            this.save_session_state();
            Ok(())
        });
    }

    /// 現在のタブ群・ウィンドウ位置・分割比を state.toml へ保存する。
    /// 終了時（wm_destroy）と再起動時（Restart）の両方から呼ぶ。
    fn save_session_state(&self) {
        self.save_active();
        let window = window_state::capture(&self.wnd.hwnd());
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
            let _ = this.update_title();
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

    fn exec(&self, is_left: bool, inv: &Invocation) -> w::AnyResult<()> {
        let cmd = inv.command;
        let view = self.view(is_left);
        // 書庫の読込中はキー入力を抑止し、Esc（ClearAll）と「親へ戻る」（ToParent・既定 BS）を
        // 展開中止に割り当てる。デカい書庫にうっかり潜った時、咄嗟の「出る」操作で抜けられる。
        if view.is_loading() {
            if matches!(cmd, Command::ClearAll | Command::ToParent) {
                self.cancel_archive_load();
            }
            return Ok(());
        }
        // 引数があれば実行直前にマクロを展開する。入力/選択のキャンセルは無音で実行中止。
        let args = if inv.args.is_empty() {
            Vec::new()
        } else {
            match self.expand_args(is_left, &inv.args) {
                Ok(a) => a,
                Err(MacroAbort) => return Ok(()),
            }
        };
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
            Command::SetCursorPosition => {
                if let Some(name) = args.first() {
                    let mut s = state.borrow_mut();
                    if let Some(idx) = s.items.iter().position(|it| it.name == *name) {
                        s.set_cursor(idx as isize, pr);
                    }
                }
            }
            Command::EnterDir => {
                let cursor = state.borrow().cursor;
                self.activate(is_left, cursor)?;
                return Ok(());
            }
            Command::View => {
                self.view_command(is_left, args.first().map(String::as_str))?;
                return Ok(());
            }
            Command::ToParent => {
                self.to_parent(is_left)?;
                return Ok(());
            }
            Command::ToRoot => {
                self.to_root(is_left)?;
                return Ok(());
            }
            Command::HistoryBack => {
                self.history_move(is_left, false)?;
                return Ok(());
            }
            Command::HistoryForward => {
                self.history_move(is_left, true)?;
                return Ok(());
            }
            Command::PathHistoryDialog => {
                self.path_history_dialog(is_left)?;
                return Ok(());
            }
            Command::ChangeDirectory => {
                self.change_directory(is_left, args.first().map(String::as_str))?;
                return Ok(());
            }
            Command::ChangeDrive => {
                self.change_drive_to(is_left, args.first().map(String::as_str))?;
                return Ok(());
            }
            Command::ChangeDirectoryDialog => {
                self.change_directory_dialog(is_left)?;
                return Ok(());
            }
            Command::ChangeDriveDialog => {
                self.change_drive_dialog(is_left)?;
                return Ok(());
            }
            Command::JumpDialog => {
                self.jump_dialog(is_left)?;
                return Ok(());
            }
            Command::RegisterPath => {
                self.register_path(is_left)?;
                return Ok(());
            }
            Command::IncrementalSearchDialog => {
                self.incremental_search(is_left)?;
                return Ok(());
            }
            Command::DirectoryInformation => {
                self.directory_information(is_left)?;
                return Ok(());
            }
            Command::RenameSequenceDialog => {
                self.rename_sequence_dialog(is_left)?;
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
            Command::SelectFile => {
                let mut s = state.borrow_mut();
                let c = s.cursor;
                s.select_file(c, pr);
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
                self.reload_side_impl(true, true)?;
                self.reload_side_impl(false, true)?;
                return Ok(());
            }
            Command::SortByName => self.sort_active(is_left, SortType::FileName, false),
            Command::SortByExtension => self.sort_active(is_left, SortType::Extension, false),
            Command::SortBySize => self.sort_active(is_left, SortType::Length, false),
            Command::SortByDate => self.sort_active(is_left, SortType::LastWriteTime, false),
            Command::Sort => {
                if let Some(t) = args.first().and_then(|s| SortType::from_token(s)) {
                    self.sort_active(is_left, t, false);
                }
            }
            Command::SortReverseToggle => {
                let t = state.borrow().sort_type;
                self.sort_active(is_left, t, true);
            }
            Command::SortDialog => {
                self.sort_dialog(is_left);
                return Ok(());
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
            Command::Compress => {
                self.compress(is_left)?;
                return Ok(());
            }
            Command::Extract => {
                self.extract_menu(is_left)?;
                return Ok(());
            }
            Command::ViewFile => {
                self.view_file(is_left)?;
                return Ok(());
            }
            Command::Edit => {
                self.edit(is_left)?;
                return Ok(());
            }
            Command::PropertyDialog => {
                self.property_dialog(is_left)?;
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
            Command::SendToRecycled => {
                self.send_to_recycled(is_left)?;
                return Ok(());
            }
            Command::CreateShortcut => {
                self.create_shortcut(is_left)?;
                return Ok(());
            }
            Command::ClipCopy => {
                self.clip_copy(is_left, false)?;
                return Ok(());
            }
            Command::ClipCut => {
                self.clip_copy(is_left, true)?;
                return Ok(());
            }
            Command::ClipPaste => {
                self.clip_paste(is_left)?;
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
            Command::KeyBindsDialog => {
                self.keybinds_dialog();
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
            Command::CursorOpposite => {
                // 反対ペイン（現アクティブでない側）へフォーカスを移す。
                self.view(self.active_right.get()).hwnd().SetFocus();
                return Ok(());
            }
            Command::Refresh => {
                self.view(true).refresh()?;
                self.view(false).refresh()?;
                return Ok(());
            }
            Command::Nop => {
                return Ok(());
            }
            Command::MaximizeCurrent => {
                if self.active_right.get() {
                    self.maximize_right(false)?;
                } else {
                    self.maximize_left(false)?;
                }
                return Ok(());
            }
            Command::MaximizeWindow => {
                self.wnd.hwnd().ShowWindow(co::SW::SHOWMAXIMIZED);
                return Ok(());
            }
            Command::MinimizeWindow => {
                self.wnd.hwnd().ShowWindow(co::SW::MINIMIZE);
                return Ok(());
            }
            Command::Restart => {
                // 現セッションを保存してから同じ exe を起動し直し、自分は終了する。
                self.save_session_state();
                if let Ok(exe) = std::env::current_exe() {
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    let _ = std::process::Command::new(exe).args(&args).spawn();
                }
                self.wnd.hwnd().DestroyWindow()?;
                return Ok(());
            }
            Command::ApplicationExit | Command::End | Command::Quit => {
                self.wnd.hwnd().DestroyWindow()?;
                return Ok(());
            }
        }
        view.refresh()?;
        self.update_selected_info(is_left);
        Ok(())
    }

    /// アクティブペインを指定ドライブのルートへ移す（引数版 `ChangeDrive("C:")`）。
    /// 引数は `C` / `C:` / `C:\` のいずれでも可。空や不正は何もしない。
    fn change_drive_to(&self, is_left: bool, drive: Option<&str>) -> w::AnyResult<()> {
        let Some(d) = drive.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let Some(letter) = d.chars().next().filter(|c| c.is_ascii_alphabetic()) else {
            return Ok(());
        };
        let root = format!("{}:\\", letter.to_ascii_uppercase());
        *self.pane(is_left).borrow_mut() = Pane::open(&root);
        self.reload_side(is_left)?;
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
    /// ソート設定ダイアログを開き、選ばれた種別・昇降をアクティブペインに適用する。
    /// カーソルは現在のファイル名へ追従させる。
    fn sort_dialog(&self, is_left: bool) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        let state = view.state();
        let (cur, reverse) = {
            let s = state.borrow();
            (s.sort_type, s.sort_reverse)
        };
        let Some((sort, reverse)) = dialog::sort_box(&self.wnd, cur, reverse) else {
            return;
        };
        let mut s = state.borrow_mut();
        let name = s.items.get(s.cursor).map(|i| i.name.clone());
        s.sort(sort, reverse);
        if let Some(n) = name {
            s.set_cursor_position(&n, pr);
        }
    }

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
    fn build_state_for(
        path: &str,
        columns: &[Column],
        sort_type: SortType,
        sort_reverse: bool,
    ) -> FileListState {
        let items = Pane::restore(path).read();
        let mut s = FileListState::new();
        s.columns = columns.to_vec();
        s.sort_type = sort_type;
        s.sort_reverse = sort_reverse;
        s.items = items;
        s.sort(sort_type, sort_reverse);
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
        }
        self.view(true).autofit_columns()?;
        self.view(false).autofit_columns()?;
        self.view(true).refresh()?;
        self.view(false).refresh()?;
        self.update_selected_info(true);
        self.update_selected_info(false);
        self.update_drive_info(true);
        self.update_drive_info(false);
        // 復元/タブ切替先がソリッド書庫等（非RA）なら一括展開＋スピナーを起こす。これらの
        // 経路は reload_side を通らないので、ここで明示的にトリガする（startup/タブ切替の edge）。
        for is_left in [true, false] {
            let _ = self.maybe_start_archive_extract(is_left);
        }
        self.cleanup_unreferenced_temps();
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// アクティブタブのスナップショットをライブから更新する。
    fn save_active(&self) {
        let snap = self.snapshot_live();
        let i = self.active.get();
        self.tabs.borrow_mut()[i] = snap;
    }

    /// ウィンドウタイトルにアクティブタブ・アクティブペインの現在パスを反映する。
    fn update_title(&self) -> w::AnyResult<()> {
        let path = self.pane(!self.active_right.get()).borrow().loc_display();
        let title = if path.is_empty() { "Rerics".to_owned() } else { path };
        self.wnd.hwnd().SetWindowText(&title)?;
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
            let resolved = this.keymap.borrow().resolve_inv(&chord).cloned();
            if let Some(inv) = resolved {
                let is_left = !this.active_right.get();
                let _ = this.exec(is_left, &inv);
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
    /// 原作 `View` 相当。引数なし＝親へ戻る/ディレクトリ・書庫へ潜る/それ以外は内蔵ビューア
    /// （拡張子で text/media 振り分け）。`type` 指定時はディレクトリでは何もせず、ファイルを
    /// そのビューアで開く（`"text"`/`"bin"` は強制テキスト・他は拡張子振り分け）。
    /// EnterDir（関連付けで外部起動）と違い、ファイルは常に内蔵ビューアで開くのが手触りの差。
    fn view_command(&self, is_left: bool, vtype: Option<&str>) -> w::AnyResult<()> {
        let (is_parent, is_dir, name) = {
            let state = self.view(is_left).state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) => (it.is_parent, it.is_dir, it.name.clone()),
                None => return Ok(()),
            }
        };
        let typed = vtype.map(|t| !t.is_empty()).unwrap_or(false);
        if !typed {
            // 親・ディレクトリ・書庫は潜る（原作 View の type=="" 経路）。
            if is_parent {
                return self.to_parent(is_left);
            }
            if is_dir {
                if self.pane(is_left).borrow_mut().enter(&name, true) {
                    self.reload_side(is_left)?;
                }
                return Ok(());
            }
            if self.pane(is_left).borrow_mut().enter(&name, false) {
                self.reload_side(is_left)?;
                return Ok(());
            }
            return self.view_file(is_left);
        }
        // type 指定時：ディレクトリ/親は何もしない（原作 View）。
        if is_parent || is_dir {
            return Ok(());
        }
        match vtype {
            Some(t) if t.eq_ignore_ascii_case("text") || t.eq_ignore_ascii_case("bin") => {
                self.view_text(is_left, &name)
            }
            _ => self.view_file(is_left),
        }
    }

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
        // 別メディアを開くので、前回の書庫プリフェッチがあれば止める。
        self.cancel_media_prefetch();
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
                // resolver/プリフェッチが作る temp が登録済みルート配下に来るよう、ここで登録する
                // （セッション中掃除の参照元）。
                self.register_archive_temp(&archive);
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

                // 一括展開済みの書庫（ソリッド 7z 等）は temp の実FS を指すだけ＝再展開も
                // プリフェッチも不要。resolver は temp_root 配下の実パスを返す。
                if let Some(root) = self.archive_extracted.borrow().get(&archive).cloned() {
                    let entries2 = entries.clone();
                    let resolver: NavResolver = Rc::new(move |i: usize| {
                        let (inner_file, _nm) = entries2.get(i)?;
                        let p = root.join(Self::inner_to_pathbuf(inner_file));
                        p.is_file().then_some(p)
                    });
                    self.media.open_nav(n, index, resolver);
                    return self.show_viewer(ActiveView::Media);
                }

                // 暗号化メディアなら開く前にパスワードを確保して resolver で使い回す（平文は None）。
                let password = self.ensure_media_password(&archive, entries.get(index).map(|(i, _)| i.as_str()));

                // BG プリフェッチ（§7.6）：現在位置を共有 atomic で伝え、別スレッドが近傍を
                // 先読み展開して共有 mtime キャッシュを温める。FG（resolver）は同期展開で
                // 割り込み、BG は存在チェックでスキップ＝共有キャッシュ越しに協調する。
                let cur = Arc::new(AtomicUsize::new(index));
                let cancel = Arc::new(AtomicBool::new(false));
                *self.media_prefetch.borrow_mut() = Some(cancel.clone());
                {
                    let bg_entries: Vec<(String, String)> = (*entries).clone();
                    let bg_archive = archive.clone();
                    let bg_cur = cur.clone();
                    let bg_cancel = cancel.clone();
                    let bg_shutdown = self.shutdown.clone();
                    std::thread::spawn(move || {
                        Self::media_prefetch_loop(
                            &bg_archive,
                            &bg_entries,
                            &bg_cur,
                            &bg_cancel,
                            &bg_shutdown,
                        );
                    });
                }

                let archive = archive.clone();
                let log = self.log.clone();
                let resolver: NavResolver = Rc::new(move |i: usize| {
                    // BG にカレント位置を伝える（先読みの中心を移動先へ寄せる）。
                    cur.store(i, Ordering::Relaxed);
                    let (inner_file, nm) = entries.get(i)?;
                    match Self::extract_entry_to_temp(&archive, inner_file, password.as_deref()) {
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
        self.cancel_media_prefetch();
        self.media.stop_playback();
        self.active_view.set(ActiveView::None);
        self.viewer.hwnd().ShowWindow(co::SW::HIDE);
        self.media.hwnd().ShowWindow(co::SW::HIDE);
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// 走行中の書庫プリフェッチスレッドがあれば停止フラグを立てる（次の窓パスで終了する）。
    fn cancel_media_prefetch(&self) {
        if let Some(c) = self.media_prefetch.borrow_mut().take() {
            c.store(true, Ordering::Relaxed);
        }
    }

    /// 書庫内メディアの先読み展開ループ（BG スレッド・§7.6）。`cur` の前後の近傍を
    /// 共有キャッシュへ温める。`extract_entry_to_temp` は既展開なら存在チェックで安価に
    /// 返る（再展開しない）ので、毎パスで近傍を舐め直しても重くならない。`cancel`/
    /// `shutdown` が立つか、カレントが動いたら速やかに切り上げて再センタリングする。
    fn media_prefetch_loop(
        archive: &Path,
        entries: &[(String, String)],
        cur: &AtomicUsize,
        cancel: &AtomicBool,
        shutdown: &AtomicBool,
    ) {
        // 前方優先で温める窓（漫画の順送りを想定）。総量はこの窓に限られる＝暴走しない。
        const AHEAD: usize = 6;
        const BEHIND: usize = 2;
        let n = entries.len();
        loop {
            if cancel.load(Ordering::Relaxed) || shutdown.load(Ordering::Relaxed) {
                return;
            }
            let center = cur.load(Ordering::Relaxed);
            let mut targets: Vec<usize> = (1..=AHEAD).map(|k| center + k).collect();
            for k in 1..=BEHIND {
                if center >= k {
                    targets.push(center - k);
                }
            }
            for idx in targets {
                if cancel.load(Ordering::Relaxed) || shutdown.load(Ordering::Relaxed) {
                    return;
                }
                // カレントが動いたら今の窓は捨てて再センタリングする。
                if cur.load(Ordering::Relaxed) != center {
                    break;
                }
                if idx >= n {
                    continue;
                }
                let (inner, _name) = &entries[idx];
                // BG は静的呼び＝プロンプト不可。暗号化エントリは展開せず（FG が同期展開で扱う）。
                let _ = Self::extract_entry_to_temp(archive, inner, None);
            }
            // カレント変化を待つ短い休止（FG は同期展開で割り込むので latency は問題にならない）。
            std::thread::sleep(Duration::from_millis(120));
        }
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
                let mut bytes = self.read_archive_entry(&archive, &join_inner_path(&inner, name))?;
                let truncated = bytes.len() > cap;
                bytes.truncate(cap);
                Ok((bytes, truncated))
            }
        }
    }

    /// 書庫内エントリを読む（暗号化エントリはキャッシュ済み or 入力プロンプトのパスワードで
    /// 復号する）。パスワードが合えば書庫単位でキャッシュし、同一書庫の他エントリで再入力
    /// させない。誤入力は数回まで再入力を促す。
    fn read_archive_entry(&self, archive: &Path, inner: &str) -> std::io::Result<Vec<u8>> {
        // 既に temp に在れば実FS から直接読む（一括展開済み or per-file 展開済み・再展開しない）。
        let root = self.register_archive_temp(archive);
        if let Some(rel) = Self::safe_inner_path(inner) {
            let p = root.join(&rel);
            if p.is_file() {
                return std::fs::read(&p);
            }
        }
        let backend = open_archive(archive)?;
        let encrypted = backend
            .list()
            .ok()
            .and_then(|es| es.into_iter().find(|e| e.path == inner))
            .map(|e| e.is_encrypted)
            .unwrap_or(false);
        if !encrypted {
            return backend.read(inner);
        }
        // キャッシュ済みパスワードを先に試す。
        if let Some(pw) = self.archive_passwords.borrow().get(archive).cloned() {
            if let Ok(b) = backend.read_with_password(inner, Some(&pw)) {
                return Ok(b);
            }
        }
        for _ in 0..3 {
            let Some(pw) = self.prompt_password(archive) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "パスワードが必要です",
                ));
            };
            match backend.read_with_password(inner, Some(pw.as_bytes())) {
                Ok(b) => {
                    self.archive_passwords
                        .borrow_mut()
                        .insert(archive.to_path_buf(), pw.into_bytes());
                    return Ok(b);
                }
                Err(_) => self.log.warn("パスワードが違うようです"),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "パスワードが一致しません",
        ))
    }

    /// 書庫のパスワードを入力ダイアログで尋ねる（伏せ字）。
    fn prompt_password(&self, archive: &Path) -> Option<String> {
        let name = archive
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        dialog::input_box(
            &self.wnd,
            "パスワード",
            &format!("{} のパスワードを入力して下さい。", name),
            "",
            dialog::InputMode::Password,
        )
    }

    /// 書庫にキャッシュ済みのパスワードがあれば返す（メディア展開で再利用する）。
    fn cached_password(&self, archive: &Path) -> Option<Vec<u8>> {
        self.archive_passwords.borrow().get(archive).cloned()
    }

    /// 書庫内エントリ `inner` が暗号化されているか（list の is_encrypted を見る）。
    fn entry_is_encrypted(&self, archive: &Path, inner: &str) -> bool {
        open_archive(archive)
            .ok()
            .and_then(|b| b.list().ok())
            .and_then(|es| es.into_iter().find(|e| e.path == inner))
            .map(|e| e.is_encrypted)
            .unwrap_or(false)
    }

    /// 暗号化メディアを開く前にパスワードを確保する（キャッシュ→無ければプロンプト→保存）。
    /// 平文なら `None`。書庫メディアの resolver が展開時に用いる。
    fn ensure_media_password(&self, archive: &Path, target_inner: Option<&str>) -> Option<Vec<u8>> {
        let enc = target_inner
            .map(|t| self.entry_is_encrypted(archive, t))
            .unwrap_or(false);
        if !enc {
            return None;
        }
        if let Some(pw) = self.cached_password(archive) {
            return Some(pw);
        }
        let pw = self.prompt_password(archive)?.into_bytes();
        self.archive_passwords
            .borrow_mut()
            .insert(archive.to_path_buf(), pw.clone());
        Some(pw)
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

    /// 書庫1つ分の temp をまとめる dir 名＝書庫パス＋mtime のハッシュ。一括展開も per-file 展開も
    /// この配下に置くので、回収はこの dir を1発削除すれば済む（mtime 込み＝外部更新で別 key）。
    fn archive_key(archive: &Path) -> String {
        let stamp = std::fs::metadata(archive)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!(
            "{:016x}",
            hash64(&format!("{}\u{0}{}", archive.display(), stamp))
        )
    }

    /// 書庫1つ分の temp ルート（`cache/archive/<pid>/<key>/`）。
    fn archive_temp_root(archive: &Path) -> PathBuf {
        Self::archive_temp_dir().join(Self::archive_key(archive))
    }

    /// 一括展開の完了マーカ（`<key>.done`・ルートの兄弟）。content と混ざらないよう外に置く。
    /// 中断/クラッシュで残った不完全なルートを「完了済み」と誤認しないため成功後にだけ作る。
    fn archive_extract_marker(archive: &Path) -> PathBuf {
        Self::archive_temp_dir().join(format!("{}.done", Self::archive_key(archive)))
    }

    /// この書庫の temp ルートをレジストリに登録して返す（セッション中の参照カウント掃除の元）。
    fn register_archive_temp(&self, archive: &Path) -> PathBuf {
        let root = Self::archive_temp_root(archive);
        self.archive_temp_dirs
            .borrow_mut()
            .entry(archive.to_path_buf())
            .or_insert_with(|| root.clone());
        root
    }

    /// 書庫内パス（'/' 区切り）を temp ルート配下の安全な相対パスへ。空/"."は捨て、".."や '\\'
    /// 混入は弾く（zip-slip 対策）。有効セグメントが無ければ None。
    fn safe_inner_path(inner: &str) -> Option<PathBuf> {
        let mut p = PathBuf::new();
        let mut any = false;
        for seg in inner.split('/') {
            if seg.is_empty() || seg == "." {
                continue;
            }
            if seg == ".." || seg.contains('\\') {
                return None;
            }
            p.push(seg);
            any = true;
        }
        any.then_some(p)
    }

    /// 書庫内パスを temp ルート配下の相対パスへ（読み取り用途・不正は空）。
    fn inner_to_pathbuf(inner: &str) -> PathBuf {
        Self::safe_inner_path(inner).unwrap_or_default()
    }

    /// 書庫内ファイルの実パスを得る。temp に既に在ればそれを、無ければ個別展開する。
    fn resolve_archive_file(
        &self,
        archive: &Path,
        inner_file: &str,
        _name: &str,
        password: Option<&[u8]>,
    ) -> std::io::Result<PathBuf> {
        let root = self.register_archive_temp(archive);
        if let Some(rel) = Self::safe_inner_path(inner_file) {
            let p = root.join(&rel);
            if p.is_file() {
                return Ok(p);
            }
        }
        Self::extract_entry_to_temp(archive, inner_file, password)
    }

    /// 書庫内エントリを temp（`<root>/<inner>`）へ展開し実パスを返す。既に在れば再利用する
    /// （mtime 込みキーなので外部更新時は別ルート＝古い展開物を二度と参照しない）。書込みは
    /// 「一時名→rename」のアトミック方式（BG 並行展開でも書きかけを読まない・計画 §7.6）。
    ///
    /// static のまま（メディア resolver クロージャ・BG プリフェッチからも呼ぶ）。レジストリ
    /// 登録は主スレッド側 `register_archive_temp` に委ねる（呼ぶ書庫は既に登録済み）。
    fn extract_entry_to_temp(
        archive: &Path,
        inner_file: &str,
        password: Option<&[u8]>,
    ) -> std::io::Result<PathBuf> {
        let rel = Self::safe_inner_path(inner_file).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "不正な書庫内パス")
        })?;
        let path = Self::archive_temp_root(archive).join(&rel);
        if path.is_file() {
            return Ok(path);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let backend = open_archive(archive)?;
        let bytes = backend.read_with_password(inner_file, password)?;
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("entry");
        let tmp = path.with_file_name(format!("{}.tmp.{}", fname, std::process::id()));
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
                debug_server::Request::Command { name, args } => {
                    self.debug_dispatch_command(&name, args, tx)
                }
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
                debug_server::Request::ModalSelect { index } => {
                    let _ = tx.send(self.debug_modal_select(index));
                }
            }
        }
    }

    /// `POST /command/<Name>` の振り分け。非モーダルは実行後 state を返す。モーダルを開くコマンドは
    /// 先に応答を返してから exec（ネストループでブロック）。未対応コマンドは弾く。
    #[cfg(feature = "debug-server")]
    fn debug_dispatch_command(
        &self,
        name: &str,
        args: Vec<String>,
        tx: Sender<debug_server::Response>,
    ) {
        let Some(cmd) = Command::from_token(name) else {
            let _ = tx.send(debug_server::Response::BadRequest(format!(
                "unknown command: {name}"
            )));
            return;
        };
        let inv = Invocation::new(cmd, args);
        let is_left = !self.active_right.get();
        match debug_command_class(cmd) {
            DebugCmdClass::NonModal => {
                let r = match self.exec(is_left, &inv) {
                    Ok(()) => debug_server::Response::Json(self.debug_state_value().to_string()),
                    Err(e) => debug_server::Response::Error(format!("exec error: {e}")),
                };
                let _ = tx.send(r);
            }
            DebugCmdClass::MaybeModal => {
                // 読取系だが暗号化書庫等でモーダルを開き得る。単一スレッドの HTTP が
                // モーダル待ちで詰まり `/modal/*` を捌けなくなる（デッドロック）のを避け、
                // exec の前に応答を返す。モーダルが出なければそのまま実行が終わる。
                let _ = tx.send(debug_server::Response::Json(
                    "{\"maybe_modal\":true}".to_string(),
                ));
                let _ = self.exec(is_left, &inv);
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
                let _ = self.exec(is_left, &inv);
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

    /// モーダル内の最初の ListBox 子コントロールを探す。
    #[cfg(feature = "debug-server")]
    fn debug_modal_listbox(modal: &w::HWND) -> Option<w::HWND> {
        let mut found: Option<w::HWND> = None;
        modal.EnumChildWindows(|c| {
            if c.GetClassName().map(|s| s.eq_ignore_ascii_case("ListBox")).unwrap_or(false) {
                found = Some(c);
                false
            } else {
                true
            }
        });
        found
    }

    /// `POST /modal/select/<index>`：リスト選択モーダルの選択行を設定する。
    #[cfg(feature = "debug-server")]
    fn debug_modal_select(&self, index: usize) -> debug_server::Response {
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        match Self::debug_modal_listbox(&modal) {
            Some(list) => {
                unsafe {
                    let _ = list.SendMessage(w::msg::lb::SetCurSel { index: Some(index as u32) });
                }
                debug_server::Response::Json(self.debug_state_value().to_string())
            }
            None => debug_server::Response::BadRequest("modal has no list".into()),
        }
    }

    /// `POST /modal/key/<key>`：開いているモーダルへキー送出。`<key>` は `enter`/`esc`/`tab`/
    /// `shift`/矢印/英数字など。`<key>/down`・`<key>/up` で押下のみ・解放のみを送れる
    /// （「Shift を押している間だけ」のような down/up を分離して検証するため）。
    #[cfg(feature = "debug-server")]
    fn debug_modal_key(&self, key: &str) -> debug_server::Response {
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        // "shift/down" のように phase を付けられる（無ければ down→up の完全押下）。
        let (name, phase) = key.split_once('/').map_or((key, None), |(n, p)| (n, Some(p)));
        let lk = name.to_ascii_lowercase();
        let vk: u16 = match lk.as_str() {
            "enter" | "return" => 0x0D,
            "esc" | "escape" => 0x1B,
            "tab" => 0x09,
            "shift" => 0x10,
            "space" => 0x20,
            "left" => 0x25,
            "up" => 0x26,
            "right" => 0x27,
            "down" => 0x28,
            "home" => 0x24,
            "end" => 0x23,
            s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() => {
                s.as_bytes()[0].to_ascii_uppercase() as u16
            }
            s if s.len() == 1 && s.as_bytes()[0].is_ascii_digit() => s.as_bytes()[0] as u16,
            _ => return debug_server::Response::BadRequest(format!("unknown modal key: {key}")),
        };
        // 実キー入力はフォーカス中の子へ届く。IsDialogMessage の矢印グループ移動は
        // 子宛メッセージでないと発動しないため、フォーカス中の窓へ送る（無ければモーダルへ）。
        let send_down = phase != Some("up");
        let send_up = phase != Some("down");
        let focus = w::HWND::GetFocus();
        let target = focus.as_ref().unwrap_or(&modal);
        unsafe {
            if send_down {
                let _ = target.PostMessage(w::msg::WndMsg {
                    msg_id: co::WM::KEYDOWN,
                    wparam: vk as usize,
                    lparam: 0,
                });
            }
            if send_up {
                let _ = target.PostMessage(w::msg::WndMsg {
                    msg_id: co::WM::from_raw(0x0101), // WM_KEYUP
                    wparam: vk as usize,
                    lparam: 0,
                });
            }
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
                // リスト選択モーダルなら現在の選択を実コントロールから読む。
                let selected = if e.items.is_empty() {
                    e.selected
                } else {
                    let m = unsafe { w::HWND::from_ptr(e.modal_ptr as *mut std::ffi::c_void) };
                    Self::debug_modal_listbox(&m)
                        .and_then(|l| unsafe { l.SendMessage(w::msg::lb::GetCurSel {}) })
                        .map(|n| n as usize)
                        .unwrap_or(e.selected)
                };
                json!({
                    "kind": e.kind,
                    "title": e.title,
                    "prompt": e.prompt,
                    "has_input": e.has_input,
                    "input": input,
                    "items": e.items,
                    "selected": selected,
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
        let cfg = self.config.borrow();
        let mut v =
            debug_json::presentation_top_json(&cfg.theme, &cfg.active_colors(), &cfg.font, &cfg.layout);
        v["panes"] = serde_json::json!({
            "left": self.view(true).presentation(),
            "right": self.view(false).presentation(),
        });
        v
    }

    /// 片側ペインの状態を JSON 値で組む。GUI から値を集め、純粋関数 `debug_json::pane_state_json`
    /// に渡すだけの薄い層（シリアライズ本体はそちらでユニットテスト済み）。
    #[cfg(feature = "debug-server")]
    fn debug_pane_json(&self, is_left: bool) -> serde_json::Value {
        let (location, is_archive) = {
            let pane = self.pane(is_left).borrow();
            (pane.loc_display(), pane.is_archive())
        };
        let view = self.view(is_left);
        let page_rows = view.page_rows();
        let mask = self.mask(is_left).borrow().clone();
        let path_bar = self.bar(is_left).text();
        let status_left = self.status(is_left).left_text();
        let status_right = self.status(is_left).right_text();
        let chrome = debug_json::PaneChrome {
            location: &location,
            is_archive,
            page_rows,
            mask: mask.as_deref(),
            path_bar: &path_bar,
            status_left: &status_left,
            status_right: &status_right,
        };
        let st = view.state();
        let s = st.borrow();
        debug_json::pane_state_json(&s, &chrome)
    }

    fn mask(&self, is_left: bool) -> &Rc<RefCell<Option<String>>> {
        if is_left { &self.left_mask } else { &self.right_mask }
    }

    /// ペインの現在パスを読み直して State へ反映し、パスバーを更新する。
    ///
    /// 対象が「未展開の非ランダムアクセス書庫」なら、ここで一括展開を非同期に開始し
    /// （スピナー表示）、一覧反映は展開完了イベントに委ねて早期 return する。
    fn reload_side(&self, is_left: bool) -> w::AnyResult<()> {
        self.reload_side_impl(is_left, false)
    }

    /// ペインを再読込する。`keep_cursor` が真なら再読込前のカーソル下ファイル名とスクロール位置を
    /// 退避し、同名ファイルがあればそこへカーソルを戻す（無ければ元の index 付近へ）。F5 リロード用。
    /// ディレクトリ移動など他経路は false で常に先頭へ。
    fn reload_side_impl(&self, is_left: bool, keep_cursor: bool) -> w::AnyResult<()> {
        if self.maybe_start_archive_extract(is_left)? {
            return Ok(());
        }
        let view = self.view(is_left);
        view.clear_loading();
        // 再読込前のカーソル位置（同名復元用）とスクロール位置を退避する。
        let (keep_name, keep_scroll, keep_idx) = if keep_cursor {
            let st = view.state();
            let s = st.borrow();
            (s.items.get(s.cursor).map(|it| it.name.clone()), s.scroll_top, s.cursor)
        } else {
            (None, 0, 0)
        };
        let items = self.read_side_items(is_left);
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
            if keep_cursor {
                let found = keep_name
                    .as_deref()
                    .map(|n| s.set_cursor_position(n, pr))
                    .unwrap_or(false);
                if !found {
                    s.set_cursor(keep_idx as isize, pr);
                }
                // スクロール位置を復元（カーソルが画面内に収まる限り見た目を維持）。
                s.set_scroll_top(keep_scroll as isize, pr);
            } else {
                s.set_cursor(0, pr);
            }
        }
        self.bar(is_left).set_path(&path);
        // per-file アイコン取得の基準ディレクトリ（実FSのみ。書庫内は None＝汎用アイコン）。
        let real_dir = self.pane(is_left).borrow().loc().as_real_path().map(|p| p.to_path_buf());
        view.set_dir(real_dir);
        view.autofit_columns()?;
        view.refresh()?;
        self.update_selected_info(is_left);
        self.update_drive_info(is_left);
        self.update_title()?;
        self.refresh_tab_bar()?;
        self.cleanup_unreferenced_temps();
        Ok(())
    }

    /// ペインの一覧を読む。一括展開済み書庫は **temp の実FS から**列挙する（tar.gz 等を毎回
    /// 再解凍しないため）。それ以外（実FS・RA書庫・未展開）は従来どおり `Pane::read`。
    fn read_side_items(&self, is_left: bool) -> Vec<rerics_core::FileItem> {
        let loc = self.pane(is_left).borrow().loc().clone();
        if let Location::Archive { archive, inner } = &loc {
            if let Some(root) = self.archive_extracted.borrow().get(archive).cloned() {
                let dir = root.join(Self::inner_to_pathbuf(inner));
                if let Ok(items) = rerics_core::read_items(&dir) {
                    return items;
                }
            }
        }
        self.pane(is_left).borrow().read()
    }

    /// 全タブのペイン位置から「temp を保持すべき書庫」を割り出し、それ以外の登録済み temp を
    /// 裏で削除する。保持＝どれかのペインが（中に居る）or（それが見える親dirに居る）。ナビ/
    /// タブ操作の後に呼ぶ。削除は best-effort（外部が掴むファイルは残し、起動時掃除で回収）。
    fn cleanup_unreferenced_temps(&self) {
        use std::collections::HashSet;
        // 参照中の場所を全部集める（アクティブタブはライブ、他タブはスナップショット文字列）。
        let mut locs: Vec<Location> = vec![
            self.left_pane.borrow().loc().clone(),
            self.right_pane.borrow().loc().clone(),
        ];
        let active = self.active.get();
        for (i, t) in self.tabs.borrow().iter().enumerate() {
            if i == active {
                continue;
            }
            locs.push(Location::parse(&t.left_path));
            locs.push(Location::parse(&t.right_path));
        }
        let mut inside: HashSet<PathBuf> = HashSet::new();
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for loc in &locs {
            match loc {
                Location::Archive { archive, .. } => {
                    inside.insert(archive.clone());
                }
                Location::Real(d) => {
                    dirs.insert(d.clone());
                }
            }
        }
        let mut to_delete: Vec<PathBuf> = Vec::new();
        {
            let mut reg = self.archive_temp_dirs.borrow_mut();
            let mut extracted = self.archive_extracted.borrow_mut();
            reg.retain(|archive, root| {
                let referenced = inside.contains(archive)
                    || archive.parent().is_some_and(|p| dirs.contains(p));
                if !referenced {
                    to_delete.push(root.clone());
                    to_delete.push(Self::archive_extract_marker(archive));
                    extracted.remove(archive);
                }
                referenced
            });
        }
        if to_delete.is_empty() {
            return;
        }
        std::thread::spawn(move || {
            for p in to_delete {
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
        });
    }

    /// pid が生存しているか（`OpenProcess` 成功で生存とみなす）。死にpid temp の起動時掃除に使う。
    fn pid_alive(pid: u32) -> bool {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
            fn CloseHandle(h: *mut c_void) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            CloseHandle(h);
            true
        }
    }

    /// 起動時に `cache/archive/<pid>/` を走査し、生きていない pid の dir を裏で削除する
    /// （クラッシュ/前回終了の残骸回収）。生存インスタンスの temp は触らない。
    fn sweep_dead_pid_temps() {
        let base = data_dir().join("cache").join("archive");
        let self_pid = std::process::id();
        std::thread::spawn(move || {
            let Ok(rd) = std::fs::read_dir(&base) else {
                return;
            };
            for ent in rd.flatten() {
                if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let Some(pid) = ent
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    continue;
                };
                if pid != self_pid && !Self::pid_alive(pid) {
                    let _ = std::fs::remove_dir_all(ent.path());
                }
            }
        });
    }

    /// 対象ペインが「未展開の非ランダムアクセス書庫」なら一括展開を非同期で開始する。
    /// 開始したら `true`（呼び側は一覧反映を完了イベントに委ねる）。実FS/RA 書庫/展開済みは
    /// `false`（呼び側は従来どおり同期 populate する）。`caps` は安価にプローブする。
    fn maybe_start_archive_extract(&self, is_left: bool) -> w::AnyResult<bool> {
        let loc = self.pane(is_left).borrow().loc().clone();
        let Location::Archive { archive, .. } = loc else {
            return Ok(false);
        };
        if self.archive_extracted.borrow().contains_key(&archive) {
            return Ok(false);
        }
        // 同じ書庫を別ペインが既に展開中なら、このペインもスピナーを出して完了を待つ
        // （二重ワーカ＝同一 temp への並行展開を避ける。完了時に両ペインまとめて反映する）。
        if self.archive_extracting.borrow().contains(&archive) {
            self.view(is_left).set_loading();
            return Ok(true);
        }
        let random_access = match open_archive(&archive) {
            Ok(be) => be.caps().random_access,
            Err(_) => return Ok(false),
        };
        if random_access {
            return Ok(false);
        }
        let root = self.register_archive_temp(&archive);
        if Self::archive_extract_marker(&archive).is_file() && root.is_dir() {
            self.archive_extracted.borrow_mut().insert(archive, root);
            return Ok(false);
        }
        self.start_archive_extract(is_left, archive, root)?;
        Ok(true)
    }

    /// 非RA 書庫の一括展開をワーカースレッドで起動する（読込ぐるぐる）。進捗は
    /// `ArchiveProgress`、完了は `ArchiveDone` で `wm_timer` 経由に取り込む。中断は
    /// `TaskControl`（Esc／タスクマネージャ）で伝える。
    fn start_archive_extract(
        &self,
        is_left: bool,
        archive: PathBuf,
        root: PathBuf,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let id = self.next_id();
        let name = archive
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.register_task(id, "展開", format!("{} を展開中", name), control.clone())?;
        self.archive_extracting.borrow_mut().insert(archive.clone());
        self.view(is_left).set_loading();
        let tx = self.task_tx.clone();
        let shutdown = self.shutdown.clone();
        let marker = Self::archive_extract_marker(&archive);
        std::thread::spawn(move || {
            let result: Result<ArchiveOutcome, String> = (|| {
                let backend = open_archive(&archive).map_err(|e| e.to_string())?;
                // 前回の中断残骸を捨ててクリーンに展開する。
                let _ = std::fs::remove_dir_all(&root);
                std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
                let mut cancelled = false;
                backend
                    .extract_all(&root, &mut |_inner, done, total| {
                        let _ = tx.send(WorkerEvent::ArchiveProgress { is_left, done, total });
                        if control.is_stopped() || shutdown.load(Ordering::Relaxed) {
                            cancelled = true;
                            return false;
                        }
                        true
                    })
                    .map_err(|e| e.to_string())?;
                if cancelled {
                    let _ = std::fs::remove_dir_all(&root);
                    return Ok(ArchiveOutcome::Cancelled);
                }
                let _ = std::fs::write(&marker, b"");
                Ok(ArchiveOutcome::Ok)
            })();
            let outcome = match result {
                Ok(o) => o,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&root);
                    ArchiveOutcome::Failed(e)
                }
            };
            let _ = tx.send(WorkerEvent::ArchiveDone {
                id,
                archive,
                temp_root: root,
                outcome,
            });
        });
        Ok(())
    }

    /// 走行中の書庫一括展開を中止要求する（Esc／読込中ペイン）。ワーカは次のコールバックで
    /// 気付き、`ArchiveDone{Cancelled}` を返す。
    fn cancel_archive_load(&self) {
        for t in self.tasks.borrow().iter() {
            if t.text == "展開" {
                t.control.stop();
            }
        }
    }

    /// 展開できなかった書庫から実親へ抜ける（中断/失敗時の復帰）。
    fn exit_archive_to_parent(&self, is_left: bool) -> w::AnyResult<()> {
        loop {
            if !self.pane(is_left).borrow().is_archive() {
                break;
            }
            if self.pane(is_left).borrow_mut().to_parent().is_none() {
                break;
            }
        }
        self.reload_side(is_left)
    }

    /// 指定ペインの現在地が書庫 `archive` の中か（一括展開完了時の対象ペイン判定に使う）。
    fn pane_in_archive(&self, is_left: bool, archive: &Path) -> bool {
        matches!(
            self.pane(is_left).borrow().loc(),
            Location::Archive { archive: a, .. } if a == archive
        )
    }

    /// タスクが空で、かつどちらのペインも読込中でなければ取り込みタイマを止める。
    fn maybe_kill_task_timer(&self) {
        let loading = self.view(true).is_loading() || self.view(false).is_loading();
        if self.tasks.borrow().is_empty() && !loading {
            let _ = self.wnd.hwnd().KillTimer(task::TASK_TIMER_ID);
        }
    }

    /// 入力ダイアログで名前を尋ね、アクティブペインの現在パス直下にディレクトリを作る。
    /// 作成後は一覧を更新し、新ディレクトリへカーソルを移す。
    fn make_directory(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            return self.make_directory_in_archive(is_left);
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

    /// 書庫内にディレクトリを作る（`caps.can_mkdir` のとき。append で既存を壊さない）。
    fn make_directory_in_archive(&self, is_left: bool) -> w::AnyResult<()> {
        let (archive, inner) = {
            let p = self.pane(is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        let backend = match rerics_core::open_archive(&archive) {
            Ok(b) if b.caps().can_mkdir => b,
            Ok(_) => {
                self.log.warn("この書庫形式はディレクトリの作成に未対応です");
                return Ok(());
            }
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        };
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
        let target = if inner.is_empty() {
            name.to_string()
        } else {
            format!("{inner}/{name}")
        };
        // 同名（ファイル/ディレクトリ）が既存なら、実FS のディレクトリ作成と同じくエラーにする。
        let existing: Vec<String> = backend
            .list()
            .map(|es| es.into_iter().map(|e| e.path).collect())
            .unwrap_or_default();
        let prefix = format!("{target}/");
        if existing.iter().any(|p| *p == target || p.starts_with(&prefix)) {
            let line = messages::create_directory_failure(name, "すでに存在します");
            self.log.error(&line);
            dialog::message_box(&self.wnd, "ディレクトリの作成", &line, dialog::MessageStyle::Error);
            return Ok(());
        }
        let mut writer = match rerics_core::open_archive_writer(&archive) {
            Ok(w) => w,
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        };
        if let Err(e) = writer.mkdir(&target) {
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

    /// 新規ファイルを作る。`data_dir/templates` にテンプレートがあれば選択させ、
    /// 選んだテンプレートを複製する（既定名＝テンプレ名）。無ければ従来どおり空ファイル。
    /// 既存ファイルは上書きしない。
    fn create_file(&self, is_left: bool) -> w::AnyResult<()> {
        if self.block_if_archive(is_left, "ファイルの作成") {
            return Ok(());
        }
        let tdir = rerics_core::data_dir().join("templates");
        let mut templates: Vec<String> = std::fs::read_dir(&tdir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        templates.sort();

        // テンプレート選択（先頭＝空ファイル）。無ければスキップ＝空ファイル。
        let template: Option<std::path::PathBuf> = if templates.is_empty() {
            None
        } else {
            let mut items = Vec::with_capacity(templates.len() + 1);
            items.push("（空ファイル）".to_string());
            items.extend(templates.iter().cloned());
            let Some(idx) = dialog::list_box(&self.wnd, "テンプレートの選択", &items, 0) else {
                return Ok(());
            };
            if idx == 0 {
                None
            } else {
                Some(tdir.join(&templates[idx - 1]))
            }
        };

        let default_name = template
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = dialog::input_box(
            &self.wnd,
            "新規ファイルの作成",
            "ファイル名を入力して下さい。",
            &default_name,
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
        if path.exists() {
            let msg = messages::all_ready_exists(name);
            dialog::message_box(&self.wnd, "新規ファイルの作成", &msg, dialog::MessageStyle::Error);
            return Ok(());
        }
        let made = match &template {
            None => std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map(|_| ()),
            Some(tpl) => std::fs::copy(tpl, &path).map(|_| ()),
        };
        if let Err(e) = made {
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

    /// アクティブペインの選択項目を新しい zip に圧縮する（実FS のみ）。出力名を尋ね、
    /// アクティブペインの直下に作る。既存名は上書き確認する。
    fn compress(&self, is_left: bool) -> w::AnyResult<()> {
        if self.block_if_archive(is_left, "圧縮") {
            return Ok(());
        }
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        // 既定名：単一選択ならその名 + .zip、複数なら親ディレクトリ名 + .zip。
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        let default_name = if names.len() == 1 {
            format!("{}.zip", names[0])
        } else {
            let base = dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "archive".to_owned());
            format!("{base}.zip")
        };
        let name = dialog::input_box(
            &self.wnd,
            "圧縮",
            "圧縮ファイル名を入力して下さい。",
            &default_name,
            dialog::InputMode::Plain,
        );
        let Some(name) = name else {
            return Ok(());
        };
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let dst_zip = dir.join(name);
        if dst_zip.exists() {
            let r = dialog::message_box(
                &self.wnd,
                "圧縮",
                &messages::all_ready_exists(name),
                dialog::MessageStyle::YesNo,
            );
            if r != dialog::MessageResult::Yes {
                return Ok(());
            }
        }
        self.start_compress(dir, names, dst_zip)
    }

    /// メニュー「解凍」からの取り出し。アクティブが書庫なら反対の実ペインへ展開する。
    fn extract_menu(&self, is_left: bool) -> w::AnyResult<()> {
        if !self.pane(is_left).borrow().is_archive() {
            self.log.warn("カレントが書庫ではありません");
            return Ok(());
        }
        self.extract_from_archive(is_left)
    }

    /// 圧縮作成をワーカースレッドで起動する。完了で出力先（＝src と同じ dir）を再読込する。
    fn start_compress(
        &self,
        dir: PathBuf,
        names: Vec<String>,
        dst_zip: PathBuf,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let desc = format!("{} -> {}", short_desc(&names), dst_zip.display());
        self.register_task(id, "圧縮", desc, control)?;
        let src_dir = dir.clone();
        std::thread::spawn(move || {
            rerics_core::run_compress(&host, &src_dir, &names, &dst_zip);
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind: OpKind::Copy,
                src_dir: src_dir.clone(),
                dst_dir: src_dir,
            });
        });
        Ok(())
    }

    /// アクティブペインの選択（無ければカーソル）を反対側ペインへコピー/移動する。
    fn copy_or_move(&self, is_left: bool, move_it: bool) -> w::AnyResult<()> {
        let src_is_archive = self.pane(is_left).borrow().is_archive();
        let dst_is_archive = self.pane(!is_left).borrow().is_archive();

        // src が書庫＝取り出し（展開コピー）。移動は元（書庫）を消せないので未対応でスルー。
        if src_is_archive {
            if move_it {
                self.log.warn("書庫からの移動は未対応です");
                return Ok(());
            }
            if dst_is_archive {
                self.log.warn("書庫から書庫への取り出しは未対応です");
                return Ok(());
            }
            return self.extract_from_archive(is_left);
        }
        // dst が書庫＝実FS から書庫への追加（コピー）／移動（追加後に元を削除）。
        if dst_is_archive {
            return self.add_to_archive(is_left, move_it);
        }

        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let src_dir = self.pane(is_left).borrow().path().to_path_buf();
        let dst_dir = self.pane(!is_left).borrow().path().to_path_buf();
        self.start_copy(src_dir, dst_dir, names, move_it)
    }

    /// 選択中（無ければカーソル位置）の項目名を集める。`..` は除外する。
    fn selected_or_cursor_names(&self, is_left: bool) -> Vec<String> {
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
    }

    /// 書庫内の選択項目を反対側ペイン（実FS）へ取り出す（展開コピー）。
    fn extract_from_archive(&self, is_left: bool) -> w::AnyResult<()> {
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let (archive, inner) = {
            let p = self.pane(is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        let dst_dir = match self.pane(!is_left).borrow().as_real_path() {
            Some(p) => p.to_path_buf(),
            None => {
                self.log.warn("取り出し先が実フォルダではありません");
                return Ok(());
            }
        };
        self.start_extract(archive, inner, names, dst_dir)
    }

    /// 実FS の選択項目を反対側ペイン（書庫）へ追加する。move なら追加成功後に実FS の元を消す。
    /// 同名エントリがあれば追加方式（append／再構築して置換）を尋ねる。
    fn add_to_archive(&self, is_left: bool, move_it: bool) -> w::AnyResult<()> {
        fn inner_join(prefix: &str, name: &str) -> String {
            if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            }
        }
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let src_dir = match self.pane(is_left).borrow().as_real_path() {
            Some(p) => p.to_path_buf(),
            None => {
                self.log.warn("追加元が実フォルダではありません");
                return Ok(());
            }
        };
        let (archive, inner) = {
            let p = self.pane(!is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        let backend = match rerics_core::open_archive(&archive) {
            Ok(b) => b,
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        };
        if !backend.caps().can_add {
            self.log.warn("この書庫形式はファイルの追加に未対応です");
            return Ok(());
        }
        // 同名衝突をスキャンして方式を決める（衝突ゼロなら無言で append）。
        let existing: Vec<String> = backend
            .list()
            .map(|es| es.into_iter().map(|e| e.path).collect())
            .unwrap_or_default();
        let colliding: Vec<String> = names
            .iter()
            .filter(|n| {
                let t = inner_join(&inner, n);
                let pfx = format!("{t}/");
                existing.iter().any(|p| *p == t || p.starts_with(&pfx))
            })
            .cloned()
            .collect();
        let mode = if !colliding.is_empty() {
            let summary = format!(
                "{} 個が書庫内の既存と同名です。\n追加方式を選んでください。",
                colliding.len()
            );
            match dialog::archive_add_box(&self.wnd, &summary) {
                Some(m) => m,
                None => return Ok(()),
            }
        } else {
            dialog::ArchiveAddMode::Append
        };
        // zip は同名エントリの追記ができない。スキップは衝突分を除いて append、
        // 置換は全件を再構築（rebuild）で足す。move のとき元を消すのは実際に足した分だけ。
        let targets: Vec<String> = match mode {
            dialog::ArchiveAddMode::Append => {
                names.into_iter().filter(|n| !colliding.contains(n)).collect()
            }
            dialog::ArchiveAddMode::Rebuild => names,
        };
        if targets.is_empty() {
            self.log.warn(&format!("{} 件すべて同名のためスキップしました", colliding.len()));
            return Ok(());
        }
        self.start_archive_add(archive, inner, src_dir, targets, move_it, mode, is_left)
    }

    /// 書庫への追加をワーカースレッドで起動する。`mode` に応じて append／再構築を選び、
    /// move なら全件成功後に実FS の元を削除する。完了で関与した両ペインを再読込させる。
    fn start_archive_add(
        &self,
        archive: PathBuf,
        inner: String,
        src_dir: PathBuf,
        names: Vec<String>,
        move_it: bool,
        mode: dialog::ArchiveAddMode,
        is_left: bool,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let label = if move_it { "書庫へ移動" } else { "書庫へ追加" };
        let desc = format!("{} -> {}", short_desc(&names), archive.display());
        self.register_task(id, label, desc, control)?;
        std::thread::spawn(move || {
            let summary = match mode {
                dialog::ArchiveAddMode::Append => {
                    rerics_core::run_archive_add(&host, &src_dir, &names, &archive, &inner)
                }
                dialog::ArchiveAddMode::Rebuild => {
                    rerics_core::run_archive_rebuild(&host, &src_dir, &names, &archive, &inner)
                }
            };
            // move は全件成功（エラー無し・未中断）のときだけ実FS の元を削除する。
            if move_it && summary.err == 0 && !summary.cancelled {
                for name in &names {
                    let p = src_dir.join(name);
                    let r = if p.is_dir() {
                        std::fs::remove_dir_all(&p)
                    } else {
                        std::fs::remove_file(&p)
                    };
                    if let Err(e) = r {
                        let _ = host.tx.send(WorkerEvent::Log {
                            level: LogLevel::Error,
                            text: messages::delete_failure(name, &e.to_string()),
                        });
                    }
                }
            }
            let _ = host.tx.send(WorkerEvent::ArchiveWriteDone { id, src_is_left: is_left });
        });
        Ok(())
    }

    /// 書庫内の削除/改名をワーカースレッドで起動する（どちらも全体リビルド）。完了で
    /// 関与ペインを再読込する。
    fn start_archive_op(
        &self,
        archive: PathBuf,
        inner: String,
        op: ArchiveOp,
        is_left: bool,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let (label, desc): (&str, String) = match &op {
            ArchiveOp::Delete(names) => {
                ("書庫から削除", format!("{} ({})", short_desc(names), archive.display()))
            }
            ArchiveOp::Rename { old, new } => ("書庫内で改名", format!("{old} -> {new}")),
        };
        self.register_task(id, label, desc, control)?;
        std::thread::spawn(move || {
            match op {
                ArchiveOp::Delete(names) => {
                    rerics_core::run_archive_delete(&host, &archive, &inner, &names);
                }
                ArchiveOp::Rename { old, new } => {
                    rerics_core::run_archive_rename(&host, &archive, &inner, &old, &new);
                }
            }
            let _ = host.tx.send(WorkerEvent::ArchiveWriteDone { id, src_is_left: is_left });
        });
        Ok(())
    }

    /// 書庫内のカーソル項目を改名する（`caps.can_rename` のとき・全体リビルド）。
    /// 新しい名前が既存と衝突する場合は安全側でエラーにする（実FS と違い上書きしない）。
    fn rename_in_archive(&self, is_left: bool) -> w::AnyResult<()> {
        let (archive, inner) = {
            let p = self.pane(is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        let backend = match rerics_core::open_archive(&archive) {
            Ok(b) if b.caps().can_rename => b,
            Ok(_) => {
                self.log.warn("この書庫形式は名前の変更に未対応です");
                return Ok(());
            }
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        };
        let old = {
            let view = self.view(is_left);
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
        if new.is_empty() || new == old.as_str() {
            return Ok(());
        }
        let target = if inner.is_empty() {
            new.to_string()
        } else {
            format!("{inner}/{new}")
        };
        let existing: Vec<String> = backend
            .list()
            .map(|es| es.into_iter().map(|e| e.path).collect())
            .unwrap_or_default();
        let pfx = format!("{target}/");
        if existing.iter().any(|p| *p == target || p.starts_with(&pfx)) {
            let line = messages::rename_failure(&old, "同名が存在します");
            self.log.error(&line);
            dialog::message_box(&self.wnd, "名前の変更", &line, dialog::MessageStyle::Error);
            return Ok(());
        }
        self.start_archive_op(archive, inner, ArchiveOp::Rename { old, new: new.to_string() }, is_left)
    }

    /// 書庫内の選択（無ければカーソル）を確認付きで削除する（`caps.can_remove`・全体リビルド）。
    fn delete_in_archive(&self, is_left: bool) -> w::AnyResult<()> {
        let (archive, inner) = {
            let p = self.pane(is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        match rerics_core::open_archive(&archive) {
            Ok(b) if b.caps().can_remove => {}
            Ok(_) => {
                self.log.warn("この書庫形式は削除に未対応です");
                return Ok(());
            }
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        }
        let names: Vec<String> = {
            let view = self.view(is_left);
            let state = view.state();
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
        self.start_archive_op(archive, inner, ArchiveOp::Delete(names), is_left)
    }

    /// 書庫からの取り出しをワーカースレッドで起動する。ワーカ内で書庫を開いて
    /// `run_extract` を回し、完了で dst ペインを再読込させる。
    fn start_extract(
        &self,
        archive: PathBuf,
        inner: String,
        names: Vec<String>,
        dst_dir: PathBuf,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let desc = format!("{} -> {}", short_desc(&names), dst_dir.display());
        self.register_task(id, "取り出し", desc, control)?;
        let dst_done = dst_dir.clone();
        std::thread::spawn(move || {
            match rerics_core::open_archive(&archive) {
                Ok(backend) => match backend.list() {
                    Ok(entries) => {
                        rerics_core::run_extract(&host, backend.as_ref(), &entries, &inner, &names, &dst_dir);
                    }
                    Err(e) => {
                        let _ = host.tx.send(WorkerEvent::Log {
                            level: LogLevel::Error,
                            text: format!("書庫の読取に失敗しました: {}", e),
                        });
                    }
                },
                Err(e) => {
                    let _ = host.tx.send(WorkerEvent::Log {
                        level: LogLevel::Error,
                        text: format!("書庫を開けません: {}", e),
                    });
                }
            }
            // src は書庫（実パス無し＝空）として渡す。dst（実FS）が再読込される。
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind: OpKind::Copy,
                src_dir: PathBuf::new(),
                dst_dir: dst_done,
            });
        });
        Ok(())
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
    /// カーソル/選択のディスク使用量を再帰計算する（実FSのみ・別スレッド）。完了は
    /// `DirInfoDone` で受け、結果をダイアログ＋ログに出す。
    fn directory_information(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内では使用量計算は未対応です。");
            return Ok(());
        }
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        self.start_dir_info(dir, names)
    }

    fn start_dir_info(&self, dir: PathBuf, names: Vec<String>) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let label = short_desc(&names);
        self.register_task(id, "情報", label.clone(), control)?;
        std::thread::spawn(move || {
            let info = rerics_core::run_calc_size(&host, &dir, &names);
            let _ = host.tx.send(WorkerEvent::DirInfoDone {
                id,
                label,
                bytes: info.bytes,
                files: info.files,
                dirs: info.dirs,
            });
        });
        Ok(())
    }

    /// 連番リネームダイアログ。プレフィックス・開始番号・桁数・拡張子保持を入力し、
    /// プレビューしながら OK で一括リネームする（実FSのみ・選択/カーソル対象）。
    fn rename_sequence_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内では連番リネームは未対応です。");
            return Ok(());
        }
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();

        let wnd = gui::WindowModal::new(gui::WindowModalOpts {
            title: "連番リネーム",
            size: gui::dpi(420, 220),
            style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
            process_dlg_msgs: true,
            ..Default::default()
        });
        let _lp = gui::Label::new(&wnd, gui::LabelOpts {
            text: "プレフィックス:",
            position: gui::dpi(12, 14),
            size: gui::dpi(110, 16),
            ..Default::default()
        });
        // prefix は最初に作る（debug-server の入力欄ターゲットが先頭の Edit のため）。
        let prefix = gui::Edit::new(&wnd, gui::EditOpts {
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(126, 12),
            width: gui::dpi_x(280),
            height: gui::dpi_y(22),
            ..Default::default()
        });
        let _ls = gui::Label::new(&wnd, gui::LabelOpts {
            text: "開始番号:",
            position: gui::dpi(12, 44),
            size: gui::dpi(110, 16),
            ..Default::default()
        });
        let start = gui::Edit::new(&wnd, gui::EditOpts {
            text: "1",
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(126, 42),
            width: gui::dpi_x(80),
            height: gui::dpi_y(22),
            ..Default::default()
        });
        let _ld = gui::Label::new(&wnd, gui::LabelOpts {
            text: "桁数:",
            position: gui::dpi(220, 44),
            size: gui::dpi(50, 16),
            ..Default::default()
        });
        let digits = gui::Edit::new(&wnd, gui::EditOpts {
            text: "3",
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(272, 42),
            width: gui::dpi_x(60),
            height: gui::dpi_y(22),
            ..Default::default()
        });
        let keep = gui::CheckBox::new(&wnd, gui::CheckBoxOpts {
            text: "元の拡張子を残す(&E)",
            position: gui::dpi(126, 74),
            size: gui::dpi(220, 18),
            ..Default::default()
        });
        let preview = gui::Label::new(&wnd, gui::LabelOpts {
            text: "",
            position: gui::dpi(12, 104),
            size: gui::dpi(394, 40),
            ..Default::default()
        });
        let ok = gui::Button::new(&wnd, gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(232, 156),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        });
        let cancel = gui::Button::new(&wnd, gui::ButtonOpts {
            text: "中止(&S)",
            ctrl_id: 2,
            position: gui::dpi(320, 156),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        });

        // プレビュー更新（フィールド変化のたびに先頭・末尾の変換例を出す）。
        let update: std::rc::Rc<dyn Fn()> = {
            let prefix = prefix.clone();
            let start = start.clone();
            let digits = digits.clone();
            let keep = keep.clone();
            let preview = preview.clone();
            let names = names.clone();
            std::rc::Rc::new(move || {
                let p = prefix.text().unwrap_or_default();
                let s = start.text().unwrap_or_default().trim().parse::<u64>().unwrap_or(1);
                let d = digits.text().unwrap_or_default().trim().parse::<usize>().unwrap_or(3);
                let news = rerics_core::sequence_names(&names, &p, s, d, keep.is_checked());
                let text = match (names.first(), news.first()) {
                    (Some(o1), Some(n1)) if names.len() > 1 => format!(
                        "例: {o1} → {n1}  …  {} → {}",
                        names.last().unwrap(),
                        news.last().unwrap()
                    ),
                    (Some(o1), Some(n1)) => format!("例: {o1} → {n1}"),
                    _ => String::new(),
                };
                let _ = preview.hwnd().SetWindowText(&text);
            })
        };
        for ed in [&prefix, &start, &digits] {
            let u = update.clone();
            ed.on().en_change(move || {
                u();
                Ok(())
            });
        }
        {
            let u = update.clone();
            keep.on().bn_clicked(move || {
                u();
                Ok(())
            });
        }

        #[cfg(feature = "debug-server")]
        let reg_wnd = wnd.clone();
        {
            let prefix = prefix.clone();
            let keep = keep.clone();
            let update = update.clone();
            wnd.on().wm_create(move |_| {
                keep.set_check(true);
                update();
                prefix.hwnd().SetFocus();
                #[cfg(feature = "debug-server")]
                crate::debug_server::modal_registry::push(
                    "rename_seq",
                    "連番リネーム",
                    "",
                    reg_wnd.hwnd().ptr() as isize,
                    true,
                    vec![("OK".to_string(), 1u16), ("中止(&S)".to_string(), 2u16)],
                );
                Ok(0)
            });
        }

        {
            let this = self.clone();
            let wnd2 = wnd.clone();
            let prefix = prefix.clone();
            let start = start.clone();
            let digits = digits.clone();
            let keep = keep.clone();
            let names = names.clone();
            let dir = dir.clone();
            ok.on().bn_clicked(move || {
                let p = prefix.text().unwrap_or_default();
                let s = start.text().unwrap_or_default().trim().parse::<u64>().unwrap_or(1);
                let d = digits.text().unwrap_or_default().trim().parse::<usize>().unwrap_or(3);
                let news = rerics_core::sequence_names(&names, &p, s, d, keep.is_checked());
                this.apply_sequence_rename(is_left, &dir, &names, &news);
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

        let _ = wnd.show_modal(&self.wnd);
        #[cfg(feature = "debug-server")]
        crate::debug_server::modal_registry::pop();
        let _ = (prefix, start, digits, keep, preview, ok, cancel);
        Ok(())
    }

    /// 連番リネームの実行：集合内の入れ替えでも壊れないよう一時名を経由する二段階改名。
    /// 新名の重複・集合外の既存ファイルとの衝突は中止する。
    fn apply_sequence_rename(&self, is_left: bool, dir: &Path, olds: &[String], news: &[String]) {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for n in news {
            if !seen.insert(n.as_str()) {
                self.log.error(&format!("連番リネーム中止：新しい名前が重複します（{n}）"));
                return;
            }
        }
        let old_set: HashSet<&str> = olds.iter().map(String::as_str).collect();
        for n in news {
            if !old_set.contains(n.as_str()) && dir.join(n).exists() {
                self.log.error(&format!("連番リネーム中止：既存ファイルと衝突します（{n}）"));
                return;
            }
        }
        let mut tmps = Vec::new();
        for (i, old) in olds.iter().enumerate() {
            let tmp = format!("{old}.rerics-seq-{i}");
            if let Err(e) = std::fs::rename(dir.join(old), dir.join(&tmp)) {
                self.log.error(&format!("リネーム失敗（{old}）：{e}"));
                let _ = self.reload_side(is_left);
                return;
            }
            tmps.push(tmp);
        }
        for (tmp, new) in tmps.iter().zip(news.iter()) {
            if let Err(e) = std::fs::rename(dir.join(tmp), dir.join(new)) {
                self.log.error(&format!("リネーム失敗（→{new}）：{e}"));
            }
        }
        self.log.normal(&format!("連番リネーム: {} 件", news.len()));
        let _ = self.reload_side(is_left);
    }

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

    /// 現在の実効キー割り当ての一覧を読み取り専用で表示する。
    fn keybinds_dialog(&self) {
        let rows: Vec<String> = self
            .keymap
            .borrow()
            .to_string_map()
            .iter()
            .map(|(k, v)| format!("{k:<18} {v}"))
            .collect();
        let _ = dialog::list_box(&self.wnd, "キー割り当て", &rows, 0);
        self.key_sink.hwnd().SetFocus();
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
        if self.pane(is_left).borrow().is_archive() {
            return self.rename_in_archive(is_left);
        }
        // 対象＝選択（無ければカーソル）。1件なら名前編集つき単一、複数なら属性/日時の一括。
        let targets: Vec<String> = {
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
        if targets.is_empty() {
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();

        let (single, attrs, modified, created) = if targets.len() == 1 {
            let p = dir.join(&targets[0]);
            (
                Some(targets[0].clone()),
                rerics_core::read_attrs(&p).unwrap_or_default(),
                rerics_core::modified_time(&p),
                rerics_core::created_time(&p),
            )
        } else {
            (None, rerics_core::FileAttrs::default(), None, None)
        };

        let Some(res) = dialog::rename_box(
            &self.wnd,
            single.as_deref(),
            targets.len(),
            attrs,
            modified,
            created,
        ) else {
            return Ok(());
        };

        // 単一は名前変更を先に処理し、以降の属性/日時は新パスへ適用する。
        let mut paths: Vec<std::path::PathBuf> = targets.iter().map(|n| dir.join(n)).collect();
        let mut cursor_name = single.clone();
        if let (Some(old), Some(new)) = (single.as_ref(), res.name.as_ref()) {
            let new = new.trim();
            if !new.is_empty() && new != old.as_str() {
                if let Err(e) = std::fs::rename(dir.join(old), dir.join(new)) {
                    let line = messages::rename_failure(old, &e.to_string());
                    self.log.error(&line);
                    dialog::message_box(
                        &self.wnd,
                        "名前の変更",
                        &line,
                        dialog::MessageStyle::Error,
                    );
                    return Ok(());
                }
                self.log.normal(&messages::rename(old, new));
                paths = vec![dir.join(new)];
                cursor_name = Some(new.to_owned());
            }
        }

        // 属性・更新日時の適用（複数なら据え置き＝None のフィールドは触らない）。
        let mut errors = 0usize;
        let mut changed = 0usize;
        let touch_attrs = res.attrs.iter().any(|a| a.is_some());
        if touch_attrs || res.modified.is_some() || res.created.is_some() {
            for p in &paths {
                match self.apply_meta(p, &res.attrs, res.modified, res.created) {
                    Ok(true) => changed += 1,
                    Ok(false) => {}
                    Err(e) => {
                        errors += 1;
                        self.log.error(&format!(
                            "属性/日時の変更に失敗: {} ({})",
                            p.display(),
                            e
                        ));
                    }
                }
            }
            if changed > 0 {
                self.log.normal(&format!("{changed} 件の属性／更新日時を変更しました。"));
            }
            if errors > 0 {
                dialog::message_box(
                    &self.wnd,
                    "名前と属性の変更",
                    &format!("{errors} 件の属性／更新日時の変更に失敗しました（ログ参照）。"),
                    dialog::MessageStyle::Warning,
                );
            }
        }

        self.reload_side(is_left)?;
        if let Some(n) = cursor_name {
            let pr = self.view(is_left).page_rows();
            self.view(is_left).state().borrow_mut().set_cursor_position(&n, pr);
        }
        self.view(is_left).refresh()?;
        Ok(())
    }

    /// 1ファイルへ属性（据え置き＝None は触らない）と更新日時を適用する。
    /// 何か変更したら `Ok(true)`、変更対象が無ければ `Ok(false)`。
    fn apply_meta(
        &self,
        path: &std::path::Path,
        attrs: &[Option<bool>; 4],
        modified: Option<std::time::SystemTime>,
        created: Option<std::time::SystemTime>,
    ) -> std::io::Result<bool> {
        let mut did = false;
        if let Some(t) = modified {
            rerics_core::set_modified_time(path, t)?;
            did = true;
        }
        if let Some(t) = created {
            rerics_core::set_created_time(path, t)?;
            did = true;
        }
        if attrs.iter().any(|a| a.is_some()) {
            let mut cur = rerics_core::read_attrs(path).unwrap_or_default();
            if let Some(v) = attrs[0] {
                cur.readonly = v;
            }
            if let Some(v) = attrs[1] {
                cur.hidden = v;
            }
            if let Some(v) = attrs[2] {
                cur.system = v;
            }
            if let Some(v) = attrs[3] {
                cur.archive = v;
            }
            rerics_core::write_attrs(path, cur)?;
            did = true;
        }
        Ok(did)
    }

    /// アクティブペインの選択（無ければカーソル）を確認ダイアログ付きで削除する。
    fn delete(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            return self.delete_in_archive(is_left);
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

    /// 選択（無ければカーソル）をゴミ箱へ送る（確認ダイアログ付き・実FSのみ・同期）。
    fn send_to_recycled(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではゴミ箱送りは未対応です。");
            return Ok(());
        }
        let names = self.selected_or_cursor_names(is_left);
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
            "ゴミ箱へ送る",
            &format!("{short}をゴミ箱へ送りますか？"),
            dialog::MessageStyle::YesNo,
        );
        if ans != dialog::MessageResult::Yes {
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        let paths: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).collect();
        match shell::send_to_recycle(&paths) {
            Ok(()) => self.log.normal(&format!("ゴミ箱へ送りました: {} 件", names.len())),
            Err(e) => self.log.error(&format!("ゴミ箱送りに失敗しました: {e}")),
        }
        self.reload_side(is_left)?;
        Ok(())
    }

    /// 選択（無ければカーソル）の各項目を指すショートカット（.lnk）を同じ場所に作る。
    fn create_shortcut(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではショートカット作成は未対応です。");
            return Ok(());
        }
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        let mut ok = 0usize;
        for name in &names {
            let target = dir.join(name);
            let lnk = dir.join(format!("{name}.lnk"));
            match shell::create_shortcut(&target, &lnk) {
                Ok(()) => ok += 1,
                Err(e) => self.log.error(&format!("ショートカット作成に失敗しました（{name}）：{e}")),
            }
        }
        if ok > 0 {
            self.log.normal(&format!("ショートカットを作成しました: {ok} 件"));
        }
        self.reload_side(is_left)?;
        Ok(())
    }

    /// 選択（無ければカーソル）のパスをクリップボードへ載せる（`move_it`＝切り取り）。
    fn clip_copy(&self, is_left: bool, move_it: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではクリップボード操作は未対応です。");
            return Ok(());
        }
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        let paths: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).collect();
        match shell::clip_copy_files(self.wnd.hwnd(), &paths, move_it) {
            Ok(()) => {
                let verb = if move_it { "切り取り" } else { "コピー" };
                self.log.normal(&format!("クリップボードへ{verb}: {} 件", names.len()));
            }
            Err(e) => self.log.error(&format!("クリップボード操作に失敗しました: {e}")),
        }
        Ok(())
    }

    /// クリップボードのファイルを現在地へ貼り付ける（コピー/移動はクリップボードの指定に従う）。
    fn clip_paste(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内へは貼り付けできません。");
            return Ok(());
        }
        let (paths, move_it) = match shell::clip_paste_files(self.wnd.hwnd()) {
            Ok(v) => v,
            Err(e) => {
                self.log.info(&e);
                return Ok(());
            }
        };
        // 親ディレクトリごとにまとめて run_copy する（複数フォルダ由来でも壊れない）。
        let mut groups: std::collections::BTreeMap<PathBuf, Vec<String>> = Default::default();
        for p in &paths {
            if let (Some(par), Some(nm)) = (p.parent(), p.file_name()) {
                groups
                    .entry(par.to_path_buf())
                    .or_default()
                    .push(nm.to_string_lossy().into_owned());
            }
        }
        if groups.is_empty() {
            return Ok(());
        }
        let dst = self.pane(is_left).borrow().path().to_path_buf();
        self.start_clip_paste(dst, groups.into_iter().collect(), move_it)
    }

    fn start_clip_paste(
        &self,
        dst: PathBuf,
        groups: Vec<(PathBuf, Vec<String>)>,
        move_it: bool,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let total: usize = groups.iter().map(|(_, n)| n.len()).sum();
        let text = if move_it { "貼り付け(移動)" } else { "貼り付け(コピー)" };
        self.register_task(id, text, format!("{total} 件"), control)?;
        let dst2 = dst.clone();
        std::thread::spawn(move || {
            for (src, names) in groups {
                rerics_core::run_copy(&host, &src, &dst2, &names, move_it);
            }
            let kind = if move_it { OpKind::Move } else { OpKind::Copy };
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind,
                src_dir: dst2.clone(),
                dst_dir: dst2,
            });
        });
        Ok(())
    }

    /// カーソル上のファイルを設定エディタ（config の editor）で開く（外部プロセス・実FSのみ）。
    fn edit(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内のファイルは編集起動に未対応です。");
            return Ok(());
        }
        let name = {
            let view = self.view(is_left);
            let state = view.state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent && !it.is_dir => it.name.clone(),
                _ => return Ok(()),
            }
        };
        let path = self.pane(is_left).borrow().path().join(&name);
        let editor = self.config.borrow().editor.clone();
        if editor.trim().is_empty() {
            self.log.warn("エディタが設定されていません（config の editor）。");
            return Ok(());
        }
        match shell::launch_editor(&editor, &path) {
            Ok(()) => self.log.normal(&format!("編集: {name}")),
            Err(e) => self.log.error(&e),
        }
        Ok(())
    }

    /// カーソル項目の Windows シェルのプロパティシートを開く（実FSのみ・モードレス）。
    fn property_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではプロパティ表示に未対応です。");
            return Ok(());
        }
        let name = {
            let view = self.view(is_left);
            let state = view.state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent => it.name.clone(),
                _ => return Ok(()),
            }
        };
        let path = self.pane(is_left).borrow().path().join(&name);
        if let Err(e) = shell::show_properties(self.wnd.hwnd(), &path) {
            self.log.error(&e);
        }
        Ok(())
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

    /// カレントのドライブルート（`C:\`）へ移動する。書庫内では効かない（警告のみ）。
    fn to_root(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではルートへ移動できません。");
            return Ok(());
        }
        let root = self.pane(is_left).borrow().loc().to_root();
        let Some(root) = root else {
            return Ok(());
        };
        if self.pane(is_left).borrow_mut().navigate(root) {
            self.reload_side(is_left)?;
        }
        Ok(())
    }

    /// パス移動履歴を前後する（`forward`=進む / それ以外=戻る）。移動できたら再読込。
    fn history_move(&self, is_left: bool, forward: bool) -> w::AnyResult<()> {
        let moved = {
            let mut p = self.pane(is_left).borrow_mut();
            if forward { p.go_forward() } else { p.go_back() }
        };
        if moved {
            self.reload_side(is_left)?;
        }
        Ok(())
    }

    /// 移動履歴の一覧から選んでそこへジャンプする。履歴が空なら情報ログのみ。
    fn path_history_dialog(&self, is_left: bool) -> w::AnyResult<()> {
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
        if self.pane(is_left).borrow_mut().navigate(loc) {
            self.reload_side(is_left)?;
        }
        Ok(())
    }

    /// パスを入力してそこへ移動する。移動できなければエラーログ。
    /// 指定パスへ移動する（引数版 `ChangeDirectory("path")`）。空や移動失敗はログのみ。
    /// パスはマクロ展開済み（`<I:>`/`<FOLDERDIALOG>` 等は呼び出し側で解決される）。
    fn change_directory(&self, is_left: bool, target: Option<&str>) -> w::AnyResult<()> {
        let Some(input) = target.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let loc = Location::parse(input);
        if self.pane(is_left).borrow_mut().navigate(loc) {
            self.reload_side(is_left)?;
        } else {
            self.log.error(&format!("移動できません: {input}"));
        }
        Ok(())
    }

    /// 引数列のマクロを展開する。文字列置換（`<C>`/`<O>`/`<P>`）に加え、ダイアログ系
    /// （`<I:>`/`<FOLDERDIALOG>`）は GUI ホスト越しにモーダルを開く。キャンセルは [`MacroAbort`]。
    fn expand_args(&self, is_left: bool, args: &[String]) -> Result<Vec<String>, MacroAbort> {
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

    fn change_directory_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        let current = self.pane(is_left).borrow().loc_display();
        let Some(input) = dialog::input_box(
            &self.wnd,
            "ディレクトリ移動",
            "移動先のパスを入力して下さい。",
            &current,
            dialog::InputMode::Plain,
        ) else {
            return Ok(());
        };
        let input = input.trim();
        if input.is_empty() {
            return Ok(());
        }
        let loc = Location::parse(input);
        if self.pane(is_left).borrow_mut().navigate(loc) {
            self.reload_side(is_left)?;
        } else {
            let line = format!("移動できません: {input}");
            self.log.error(&line);
        }
        Ok(())
    }

    /// ドライブ一覧（容量つき）から選んでそのルートへ移動する。
    fn change_drive_dialog(&self, is_left: bool) -> w::AnyResult<()> {
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
        if self
            .pane(is_left)
            .borrow_mut()
            .navigate(Location::Real(PathBuf::from(root)))
        {
            self.reload_side(is_left)?;
        }
        Ok(())
    }

    /// 現在地を登録ディレクトリ（ブックマーク）に追加する。登録名を尋ね、config に保存。
    fn register_path(&self, is_left: bool) -> w::AnyResult<()> {
        let path = self.pane(is_left).borrow().loc_display();
        let default_label = Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let Some(label) = dialog::input_box(
            &self.wnd,
            "ディレクトリの登録",
            "登録名を入力して下さい。",
            &default_label,
            dialog::InputMode::Plain,
        ) else {
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

    /// インクリメンタルサーチ。小さな入力モーダルを出し、打鍵ごとに先頭から一致を
    /// 探してアクティブペインのカーソルを動かす（追従）。OK で確定、中止/Esc で元へ戻す。
    fn incremental_search(&self, is_left: bool) -> w::AnyResult<()> {
        let origin = self.view(is_left).state().borrow().cursor;

        let wnd = gui::WindowModal::new(gui::WindowModalOpts {
            title: "インクリメンタルサーチ",
            size: gui::dpi(320, 96),
            style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
            process_dlg_msgs: true,
            ..Default::default()
        });

        let _label = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: "検索文字（打鍵でカーソルが追従）:",
                position: gui::dpi(12, 10),
                size: gui::dpi(296, 16),
                ..Default::default()
            },
        );

        let edit = gui::Edit::new(
            &wnd,
            gui::EditOpts {
                control_style: co::ES::AUTOHSCROLL,
                position: gui::dpi(12, 30),
                width: gui::dpi_x(296),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );

        let ok = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "OK",
                control_style: co::BS::DEFPUSHBUTTON,
                ctrl_id: 1,
                position: gui::dpi(150, 60),
                width: gui::dpi_x(76),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );

        let cancel = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "中止(&S)",
                ctrl_id: 2,
                position: gui::dpi(232, 60),
                width: gui::dpi_x(76),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );

        // 打鍵追従：テキスト変化のたびに先頭から検索してカーソルを移す。
        {
            let this = self.clone();
            let edit2 = edit.clone();
            edit.on().en_change(move || {
                let q = edit2.text().unwrap_or_default();
                this.incremental_apply(is_left, &q);
                Ok(())
            });
        }

        #[cfg(feature = "debug-server")]
        let reg_wnd = wnd.clone();
        {
            let edit2 = edit.clone();
            wnd.on().wm_create(move |_| {
                edit2.hwnd().SetFocus();
                #[cfg(feature = "debug-server")]
                crate::debug_server::modal_registry::push(
                    "incremental",
                    "インクリメンタルサーチ",
                    "",
                    reg_wnd.hwnd().ptr() as isize,
                    true,
                    vec![("OK".to_string(), 1u16), ("中止(&S)".to_string(), 2u16)],
                );
                Ok(0)
            });
        }

        {
            let wnd2 = wnd.clone();
            ok.on().bn_clicked(move || {
                wnd2.close();
                Ok(())
            });
        }
        {
            let this = self.clone();
            let wnd2 = wnd.clone();
            cancel.on().bn_clicked(move || {
                // 中止＝開始時のカーソルへ戻す。
                this.move_cursor_to(is_left, origin);
                wnd2.close();
                Ok(())
            });
        }

        let _ = wnd.show_modal(&self.wnd);
        #[cfg(feature = "debug-server")]
        crate::debug_server::modal_registry::pop();
        let _ = (edit, ok, cancel);
        Ok(())
    }

    /// インクリメンタルサーチの1打鍵分：先頭から `query` の一致を探してカーソル移動。
    fn incremental_apply(&self, is_left: bool, query: &str) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        let found = {
            let state = view.state();
            let s = state.borrow();
            rerics_core::find_match(&s.items, 0, query, true, false)
        };
        if let Some(i) = found {
            {
                let state = view.state();
                let mut s = state.borrow_mut();
                s.set_cursor(i as isize, pr);
                s.center_cursor(pr);
            }
            let _ = view.refresh();
        }
    }

    /// 指定ペインのカーソルを `idx` に移して再描画する。
    fn move_cursor_to(&self, is_left: bool, idx: usize) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        {
            let state = view.state();
            let mut s = state.borrow_mut();
            s.set_cursor(idx as isize, pr);
        }
        let _ = view.refresh();
    }

    /// 登録ディレクトリの一覧から選んでそこへジャンプする。空なら情報ログのみ。
    fn jump_dialog(&self, is_left: bool) -> w::AnyResult<()> {
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
        if self.pane(is_left).borrow_mut().navigate(loc) {
            self.reload_side(is_left)?;
        } else {
            self.log.error(&format!("移動できません: {path}"));
        }
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

/// 書庫内で全体リビルドを伴う編集操作（ワーカースレッドへ渡す）。
enum ArchiveOp {
    /// 指定エントリ（ディレクトリは配下も）を削除する。
    Delete(Vec<String>),
    /// `old` を `new` に改名する。
    Rename { old: String, new: String },
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
