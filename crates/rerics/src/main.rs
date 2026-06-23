mod chrome;
mod script;
// 常時ビルド（純粋関数＋ユニットテスト）。呼び出し元は debug-server feature 下なので OFF 時は未使用。
#[allow(dead_code)]
mod debug_json;
#[cfg(feature = "debug-server")]
mod debug_server;
mod ui_marshal;
mod script_host;
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
mod winutil;
mod archive_ctl;
mod fileops;
mod layout;
mod search;
mod session;
mod settings_ctl;
mod sort;
mod tabs_nav;
mod tasks;
mod viewer_ctl;
#[cfg(feature = "debug-server")]
mod debug_ctl;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use log_view::LogView;
use media_view::MediaView;
use pane_view::PaneView;
use path_bar::PathBarView;
use status_bar::StatusBarView;
use tab_bar::TabBar;
use task::{TaskEntry, WorkerEvent};
use viewer::ViewerView;
use rerics_core::{
    Command, Config, FileListState, Invocation, KeyChord, KeyMap, Location, MacroAbort, MacroHost,
    Pane, SortType, WindowState,
};
use winsafe::{self as w, co, gui, prelude::*};

/// 現在メイン領域に重ねているビューア。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveView {
    None,
    Text,
    Media,
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
}

/// コマンドの種別を分類する。ファイル操作系のモーダルは ModalWrite、その他のモーダルを開く
/// ものは MaybeModal（いずれも modal_registry 登録済みで `/modal/*` から駆動できる）。
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
        // ビューアの検索はインライン検索バー（モーダルを開かない＝即時に応答が返る）。
        // ソート設定はモーダルを開く（並べ替えのみ＝書込みではない）。modal_registry に登録
        // 済みなので開いて OK/Cancel で閉じられる（ラジオ値そのものの選択は未対応＝種別変更の
        // ロジックは引数コマンド Sort(type) でも駆動・検証できる）。
        SortDialog => DebugCmdClass::MaybeModal,
        // 設定は modal_registry に登録済み＝開いて /snapshot/modal で撮れ、/modal/* で
        // ナビ移動・OK/Cancel まで駆動できる（配色変更そのものは Config 経由で別途検証）。
        OpenSettings => DebugCmdClass::MaybeModal,
        // タスクマネージャは多列 ListView モーダル（走行中タスクの一覧・中止/中断/再開）。
        // modal_registry に登録済み＝開いて /snapshot/modal で撮れ、/modal/select・
        // /modal/command で行選択やボタン操作まで駆動できる（タスク自体は別スレッド継続）。
        OpenTaskManager => DebugCmdClass::MaybeModal,
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

/// 汎用バックグラウンドジョブの結果（型消去してワーカー → UI スレッドへ運ぶ）。
type UiJobResult = Box<dyn std::any::Any + Send>;
/// その結果を UI スレッドで受け取って後処理する継続（UI 側に置くので `Send` 不要＝
/// 開いているダイアログのコントロール等を自由にキャプチャできる）。
type UiJobDone = Box<dyn FnOnce(&MainWindow, UiJobResult) -> w::AnyResult<()>>;

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
    /// 起動時の config.toml 読込エラー（あれば）。窓表示後にアラート＋ログで知らせる。
    config_error: Option<String>,
    left_pane: Rc<RefCell<Pane>>,
    right_pane: Rc<RefCell<Pane>>,
    keymap: Rc<RefCell<KeyMap>>,
    viewer_keymap: Rc<RefCell<KeyMap>>,
    media_keymap: Rc<RefCell<KeyMap>>,
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
    /// 汎用ジョブのワーカー → UI レーン。レガシーの `task_*` と別建てで、`in_dialog` 中も
    /// 配達する（モーダルを後追いで埋めるため）。`ui_jobs` は id → 継続の対応表。
    ui_job_tx: Sender<(u64, UiJobResult)>,
    ui_job_rx: Rc<Receiver<(u64, UiJobResult)>>,
    ui_jobs: Rc<RefCell<std::collections::HashMap<u64, UiJobDone>>>,
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
    script: script_host::ScriptBridge,
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

/// ペイン再読込時にカーソルをどこへ置くか。
enum ReloadCursor {
    /// 常に先頭へ（在席再読込：マスク変更・ファイル操作後など）。
    Reset,
    /// 再読込前のカーソル下ファイル名へ戻し、スクロール位置も維持する（F5）。
    Keep,
    /// 移動先パスで以前覚えたカーソル位置へ戻す。無ければ先頭（ディレクトリ移動）。
    /// `cursor.history` がオンのときだけ復元する（オフは先頭）。
    Recall,
    /// 戻る/進む用：`cursor.history` 設定に関係なく、移動先で覚えたカーソル位置へ常に
    /// 戻す。無ければ先頭（原作 HistoryBack/Forward 準拠＝常時復元）。
    RecallAlways,
    /// 読込完了後に指定名へカーソルを置く（無ければ先頭）。`center` で中央寄せ。
    /// ディレクトリ作成・連番リネーム・親移動など「直後に特定行へ寄せたい」操作で使う。
    Focus { name: String, center: bool },
}

/// 非同期読込の継続（[`MainWindow::apply_loaded_items`]）へ渡す、読込後処理の計画。
/// 読込開始時に UI スレッドで確定し、ワーカーの結果と共に取り込み時へ運ぶ。
struct LoadPlan {
    mode: ReloadCursor,
    /// Keep 用：再読込前のカーソル下名・スクロール・index。
    keep_name: Option<String>,
    keep_scroll: usize,
    keep_idx: usize,
    /// Recall/RecallAlways 用：このパスで覚えたカーソル名。
    recalled: Option<String>,
    /// 表示中のフィルタ（グロブ）。
    mask: Option<String>,
    /// この読込の世代（取り込み時に現世代と一致しなければ破棄）。
    generation: u64,
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
            size: gui::dpi(960, 640),
            style,
            process_dlg_msgs: false,
            ..Default::default()
        });

        let (mut config, config_error) = Config::load_reporting();
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
        // 高さは初回 layout() でフォント実測の行高×行数に置き換わる。生成時は概算 px。
        let log_h0 = config.layout.log_height * (config.font.size + 4);
        let log = LogView::new(&wnd, gui::dpi(m, m), gui::dpi(800, log_h0), &config);
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
                    config.default_sort,
                    false,
                ),
                right_state: Self::build_state_for(
                    &right_path,
                    &config.columns,
                    config.default_sort,
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
        let viewer_keymap = config.keymap_textviewer();
        let media_keymap = config.keymap_imageviewer();

        let (menu_bar, menu_cmds) = menu::build().expect("メニューバーの構築");

        let (task_tx, task_rx) = std::sync::mpsc::channel();
        let (ui_job_tx, ui_job_rx) = std::sync::mpsc::channel();

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
            config_error,
            left_pane,
            right_pane,
            keymap: Rc::new(RefCell::new(keymap)),
            viewer_keymap: Rc::new(RefCell::new(viewer_keymap)),
            media_keymap: Rc::new(RefCell::new(media_keymap)),
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
            ui_job_tx,
            ui_job_rx: Rc::new(ui_job_rx),
            ui_jobs: Rc::new(RefCell::new(std::collections::HashMap::new())),
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
            script: script_host::ScriptBridge::new(),
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

        // ビューアの右クリック＝コンテキストメニュー（表示・実行は MainWindow が担う）。
        {
            let this = self.clone();
            self.media.on_menu(move |pt| {
                let _ = this.show_media_menu(pt);
            });
            let this = self.clone();
            self.viewer.on_menu(move |pt| {
                let _ = this.show_text_menu(pt);
            });
            // 検索バーを閉じたら、キー入力先を本体（key_sink）へ戻す。
            let this = self.clone();
            self.viewer.on_search_close(move || {
                this.key_sink.hwnd().SetFocus();
            });
            // 検索バー内のニーモニック（Alt+C 等）と同じキーがユーザーのビューアキーバインドに
            // 割り当て済みなら、そちらを優先して実行する。
            let this = self.clone();
            self.viewer.on_chord(move |chord| {
                let resolved = this.viewer_keymap.borrow().resolve_inv(&chord).cloned();
                match resolved {
                    Some(inv) => {
                        let _ = this.exec_viewer(&inv);
                        true
                    }
                    None => false,
                }
            });
        }

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
            let wake = winutil::msg::DEBUG_WAKE;
            self.wnd.on().wm(wake, move |_| {
                this.drain_debug_requests();
                Ok(0)
            });
        }

        // スクリプトエンジンスレッドからの HostApi 要求を UI スレッドで捌く。
        {
            let this = self.clone();
            let wake = winutil::msg::SCRIPT_WAKE;
            self.wnd.on().wm(wake, move |_| {
                this.drain_script_requests();
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
        let icons_ready = winutil::msg::ICONS_READY;
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
            // 非同期アイコンローダを起動（完了は msg::ICONS_READY で受ける）。
            this.icon_cache.start(this.wnd.hwnd().ptr() as isize);
            // デバッグ制御サーバ起動時は本体を最小化で立ち上げ、作業の邪魔をしない。
            #[cfg(feature = "debug-server")]
            let debug_minimized = this.debug.port.is_some();
            #[cfg(not(feature = "debug-server"))]
            let debug_minimized = false;
            let effective_window = {
                let win = this.config.borrow().window;
                if win.fixed_size {
                    // 毎回既定サイズで起動する。位置は前回値があれば踏襲し、無ければ既定。
                    let (x, y) = this
                        .initial_window
                        .as_ref()
                        .map(|w| (w.x, w.y))
                        .unwrap_or((0, 0));
                    Some(WindowState {
                        x,
                        y,
                        width: win.width.max(1),
                        height: win.height.max(1),
                        maximized: false,
                    })
                } else {
                    this.initial_window.clone()
                }
            };
            if let Some(ws) = &effective_window {
                let applied = window_state::apply(this.wnd.hwnd(), ws);
                // 最小化起動時は最大化復元を抑止する（最小化が打ち消されないように）。
                if applied && ws.maximized && !debug_minimized {
                    unsafe {
                        let _ = this.wnd.hwnd().PostMessage(w::msg::WndMsg {
                            msg_id: winutil::msg::RESTORE_MAXIMIZE,
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
            // スクリプトエンジンを別スレッドに建て、起動スクリプトを読み込む。
            this.start_script_engine();
            // 設定読込エラーは、詳細をログへ出し、窓表示後にアラートを出す（遅延）。
            if let Some(detail) = &this.config_error {
                this.log.error("設定ファイル config.toml を読み込めませんでした。既定の設定で起動しています。");
                this.log.warn(&format!("config.toml の詳細: {detail}"));
                unsafe {
                    let _ = this.wnd.hwnd().PostMessage(w::msg::WndMsg {
                        msg_id: winutil::msg::CONFIG_WARN,
                        wparam: 0,
                        lparam: 0,
                    });
                }
            }
            Ok(0)
        });

        let this = self.clone();
        self.wnd.on().wm(winutil::msg::RESTORE_MAXIMIZE, move |_| {
            window_state::maximize(this.wnd.hwnd());
            Ok(0)
        });

        // 設定読込エラーのアラート（窓表示後に遅延表示）。詳細はログに出してある。
        // 登録モーダルなので debug-server から観測・クローズできる（実機/headless 同一挙動）。
        let this = self.clone();
        self.wnd.on().wm(winutil::msg::CONFIG_WARN, move |_| {
            dialog::message_box(
                &this.wnd,
                "設定の読み込み",
                "config.toml を読み込めませんでした。\n既定の設定で起動しました。\n詳細はログを確認してください。",
                dialog::MessageStyle::Error,
            );
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
        self.fire_script_event("executeCommand", cmd.as_token());
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
                s.move_cursor(c - 1, pr, arg_is_select(&args));
            }
            Command::CursorDown => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.move_cursor(c + 1, pr, arg_is_select(&args));
            }
            Command::CursorTop => {
                state.borrow_mut().move_cursor(0, pr, arg_is_select(&args));
            }
            Command::CursorEnd => {
                let mut s = state.borrow_mut();
                let last = s.count() as isize - 1;
                s.move_cursor(last, pr, arg_is_select(&args));
            }
            Command::CursorPageUp => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.move_cursor(c - pr as isize, pr, arg_is_select(&args));
            }
            Command::CursorPageDown => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.move_cursor(c + pr as isize, pr, arg_is_select(&args));
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
                // 既に左ペインがアクティブで CursorToParent が有効なら親へ移動、
                // そうでなければ左ペインへフォーカス移動（原作 CursorLeft 準拠）。
                if is_left && self.config.borrow().cursor.to_parent {
                    self.to_parent(true)?;
                } else {
                    self.view(true).hwnd().SetFocus();
                }
                return Ok(());
            }
            Command::FocusRight => {
                if !is_left && self.config.borrow().cursor.to_parent {
                    self.to_parent(false)?;
                } else {
                    self.view(false).hwnd().SetFocus();
                }
                return Ok(());
            }
            Command::MarkToggle => {
                let delta = mark_move_delta(&args, self.config.borrow().cursor.down_after_select);
                let mut s = state.borrow_mut();
                let c = s.cursor;
                s.reverse_file(c, pr);
                let c = s.cursor as isize;
                s.set_cursor(c + delta, pr);
            }
            Command::SelectFile => {
                let delta = mark_move_delta(&args, self.config.borrow().cursor.down_after_select);
                let mut s = state.borrow_mut();
                let c = s.cursor;
                s.select_file(c, pr);
                let c = s.cursor as isize;
                s.set_cursor(c + delta, pr);
            }
            Command::SelectAll => {
                let mut s = state.borrow_mut();
                s.select_all(false);
                s.select_start = s.cursor;
            }
            Command::SelectAllFile => {
                let mut s = state.borrow_mut();
                s.select_all(true);
                s.select_start = s.cursor;
            }
            Command::ReverseAll => {
                let mut s = state.borrow_mut();
                s.reverse_all(false);
                s.select_start = s.cursor;
            }
            Command::ReverseAllFile => {
                let mut s = state.borrow_mut();
                s.reverse_all(true);
                s.select_start = s.cursor;
            }
            Command::ClearAll => {
                let mut s = state.borrow_mut();
                s.clear_all();
                s.select_start = s.cursor;
            }
            Command::Reload => {
                self.reload_side_impl(true, ReloadCursor::Keep)?;
                self.reload_side_impl(false, ReloadCursor::Keep)?;
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
                self.remember_cursor_for_nav(!is_left);
                self.pane(!is_left).borrow_mut().set_loc(loc);
                self.reload_side_navigated_nolog(!is_left)?;
                return Ok(());
            }
            Command::CurrentToOpposite => {
                let loc = self.pane(!is_left).borrow().loc().clone();
                self.remember_cursor_for_nav(is_left);
                self.pane(is_left).borrow_mut().set_loc(loc);
                self.reload_side_navigated_nolog(is_left)?;
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
            Command::CopyLog => {
                self.log.copy_all()?;
                return Ok(());
            }
            Command::ClearLog => {
                self.log.clear();
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
            // ビューア専用コマンドはファイラー文脈では何もしない。
            _ => {}
        }
        view.refresh()?;
        self.update_selected_info(is_left);
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
                    let ctrl = w::GetAsyncKeyState(co::VK::CONTROL);
                    let shift = w::GetAsyncKeyState(co::VK::SHIFT);
                    let _ = this.media_key(p.vkey_code.raw(), ctrl, shift);
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

    /// ディレクトリ移動の直前に呼ぶ。現在カーソル下のファイル名を Pane に覚えさせ、
    /// 移動の直前に、今いる場所のカーソル位置を覚えておく（同じパスへ戻った時に復元できる）。
    /// 戻る/進む（#67）は設定に関係なく常時復元するため、記録自体は常に行う。`cursor.history`
    /// は「通常の再訪・ドライブ移動での復元」を出すかどうかの**読み出し側**ゲートに使う。
    fn remember_cursor_for_nav(&self, is_left: bool) {
        let limit = self.config.borrow().cursor.history_count;
        let name = {
            let st = self.view(is_left).state();
            let s = st.borrow();
            s.items.get(s.cursor).map(|it| it.name.clone())
        };
        if let Some(name) = name {
            self.pane(is_left).borrow_mut().remember_cursor(&name, limit);
        }
    }

    /// ペインの現在パスを読み直して State へ反映し、パスバーを更新する。
    ///
    /// 対象が「未展開の非ランダムアクセス書庫」なら、ここで一括展開を非同期に開始し
    /// （スピナー表示）、一覧反映は展開完了イベントに委ねて早期 return する。
    fn reload_side(&self, is_left: bool) -> w::AnyResult<()> {
        self.reload_side_impl(is_left, ReloadCursor::Reset)
    }

    /// ディレクトリ移動後の再読込。移動先パスで以前覚えたカーソル位置を復元し、
    /// 移動先をパス移動履歴（訪問ログ・グローバル・永続）へ記録する。ユーザが行き先を
    /// 指定した移動（侵入・パス入力・ジャンプ・ドライブ変更・親/ルート移動）で使う。
    fn reload_side_navigated(&self, is_left: bool) -> w::AnyResult<()> {
        self.record_visit(is_left);
        self.reload_side_impl(is_left, ReloadCursor::Recall)
    }

    /// 移動後の再読込だが、パス移動履歴へは記録しない。ペイン入替・左右同期のように
    /// 「新しい行き先の指定」ではない移動で使う。カーソル復元は通常の移動と同じ
    /// （`cursor.history` 連動）。
    fn reload_side_navigated_nolog(&self, is_left: bool) -> w::AnyResult<()> {
        self.reload_side_impl(is_left, ReloadCursor::Recall)
    }

    /// 戻る/進む用の再読込。パス移動履歴へは記録せず、`cursor.history` 設定に関係なく
    /// 移動先で覚えたカーソル位置を常に復元する（原作 HistoryBack/Forward 準拠）。
    fn reload_side_history(&self, is_left: bool) -> w::AnyResult<()> {
        self.reload_side_impl(is_left, ReloadCursor::RecallAlways)
    }

    /// 再読込し、完了後に `name` の行へカーソルを置く（無ければ先頭・`center` で中央寄せ）。
    /// ディレクトリ作成・連番リネーム・親移動など、読込直後に特定行へ寄せたい操作で使う。
    fn reload_side_focus(&self, is_left: bool, name: &str, center: bool) -> w::AnyResult<()> {
        self.reload_side_impl(is_left, ReloadCursor::Focus { name: name.to_owned(), center })
    }

    /// 移動先（そのペインの現在地）をパス移動履歴へ記録する。同一パスは最新へ集約、
    /// 上限超過は古い方から落とす。入力履歴と同じ `history.toml` に永続する。
    fn record_visit(&self, is_left: bool) {
        let disp = self.pane(is_left).borrow().loc_display();
        let mut hist = rerics_core::InputHistory::load();
        hist.add_capped(rerics_core::PATH_HISTORY_KEY, &disp, rerics_core::PATH_HISTORY_CAP);
        let _ = hist.save();
    }

    /// ペインを再読込する。`mode` でカーソルの行き先を決める：`Keep`＝再読込前のカーソル下
    /// ファイル名へ戻す（無ければ元 index 付近・F5 用）、`Recall`＝移動先パスで以前覚えた
    /// カーソル位置へ戻す（無ければ先頭・ディレクトリ移動用）、`Reset`＝常に先頭（在席再読込）。
    fn reload_side_impl(&self, is_left: bool, mode: ReloadCursor) -> w::AnyResult<()> {
        if self.maybe_start_archive_extract(is_left)? {
            return Ok(());
        }
        let view = self.view(is_left);
        // F5（Keep）のときだけ、再読込前のカーソル下ファイル名とスクロール位置を退避する。
        let (keep_name, keep_scroll, keep_idx) = if matches!(mode, ReloadCursor::Keep) {
            let st = view.state();
            let s = st.borrow();
            (s.items.get(s.cursor).map(|it| it.name.clone()), s.scroll_top, s.cursor)
        } else {
            (None, 0, 0)
        };
        let path = self.pane(is_left).borrow().loc_display();
        // 戻る/進む（RecallAlways）は常に、通常移動（Recall）は記憶オンのときだけ、
        // このパスで覚えたカーソル位置を引く。
        let recalled = if matches!(mode, ReloadCursor::RecallAlways)
            || (matches!(mode, ReloadCursor::Recall) && self.config.borrow().cursor.history)
        {
            self.pane(is_left)
                .borrow()
                .recalled_cursor(&path)
                .map(str::to_owned)
        } else {
            None
        };
        let mask = self.mask(is_left).borrow().clone();
        let read_loc = self.resolve_read_location(is_left);

        // 移動先は確定済みなので、パスバー・基準dir・ドライブ情報・タイトル・タブは即時更新する。
        // （一覧の中身だけがワーカーの読込待ち。）
        self.bar(is_left).set_path(&path);
        // per-file アイコン取得の基準ディレクトリ（実FSのみ。書庫内は None＝汎用アイコン）。
        let real_dir = self.pane(is_left).borrow().loc().as_real_path().map(|p| p.to_path_buf());
        view.set_dir(real_dir);
        self.update_drive_info(is_left);
        self.update_title()?;
        self.refresh_tab_bar()?;

        // 世代を進めてこの読込を識別し、設定の遅延つきの待機スピナーを仕込む。
        let generation = view.bump_load_gen();
        view.set_loading();

        let plan = LoadPlan { mode, keep_name, keep_scroll, keep_idx, recalled, mask, generation };
        self.spawn_job(
            move || read_loc.read().unwrap_or_default(),
            move |mw, items| mw.apply_loaded_items(is_left, items, plan),
        );
        Ok(())
    }

    /// 読み出す対象を Send な [`Location`] として確定する。一括展開済み書庫は **temp の実FS** を
    /// 指す（tar.gz 等を毎回再解凍しない）。それ以外（実FS・RA書庫・未展開）は現在地そのまま。
    fn resolve_read_location(&self, is_left: bool) -> Location {
        let loc = self.pane(is_left).borrow().loc().clone();
        if let Location::Archive { archive, inner } = &loc
            && let Some(root) = self.archive_extracted.borrow().get(archive).cloned() {
                return Location::Real(root.join(Self::inner_to_pathbuf(inner)));
            }
        loc
    }

    /// ワーカーが読み終えた一覧を取り込み、フィルタ・ソート・カーソルを適用して描画する。
    /// 読込開始後に新しいナビ／タブ切替へ追い越されていたら（世代不一致）破棄する。
    fn apply_loaded_items(
        &self,
        is_left: bool,
        items: Vec<rerics_core::FileItem>,
        plan: LoadPlan,
    ) -> w::AnyResult<()> {
        let view = self.view(is_left);
        if view.load_gen() != plan.generation {
            return Ok(());
        }
        view.clear_loading();
        let items = match plan.mask.as_ref() {
            Some(m) => items
                .into_iter()
                .filter(|it| it.is_parent || it.is_dir || rerics_core::glob_match(&it.name, m))
                .collect(),
            None => items,
        };
        let pr = view.page_rows();
        {
            let state = view.state();
            let mut s = state.borrow_mut();
            s.items = items;
            let sort = s.sort_type;
            let reverse = s.sort_reverse;
            s.sort(sort, reverse);
            match &plan.mode {
                ReloadCursor::Keep => {
                    let found = plan
                        .keep_name
                        .as_deref()
                        .map(|n| s.set_cursor_position(n, pr))
                        .unwrap_or(false);
                    if !found {
                        s.set_cursor(plan.keep_idx as isize, pr);
                    }
                    // スクロール位置を復元（カーソルが画面内に収まる限り見た目を維持）。
                    s.set_scroll_top(plan.keep_scroll as isize, pr);
                }
                ReloadCursor::Recall | ReloadCursor::RecallAlways => {
                    let found = plan
                        .recalled
                        .as_deref()
                        .map(|n| s.set_cursor_position(n, pr))
                        .unwrap_or(false);
                    if !found {
                        s.set_cursor(0, pr);
                    }
                }
                ReloadCursor::Reset => {
                    s.set_cursor(0, pr);
                }
                ReloadCursor::Focus { name, center } => {
                    if !s.set_cursor_position(name, pr) {
                        s.set_cursor(0, pr);
                    }
                    if *center {
                        s.center_cursor(pr);
                    }
                }
            }
        }
        view.autofit_columns()?;
        view.refresh()?;
        self.update_selected_info(is_left);
        self.cleanup_unreferenced_temps();
        // 一覧確定後に changeDirectory を配る（実際に現在地が変わったときだけ・notify 側で判定）。
        // ここなら activePane() や並べ替えコマンドがハンドラから効く。
        let dir = self.pane(is_left).borrow().loc_display();
        self.notify_dir_loaded(is_left, &dir);
        Ok(())
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

/// カーソル移動コマンドの引数が「選択しながら移動」を表すか（select/true/1・大小無視）。
fn arg_is_select(args: &[String]) -> bool {
    args.first().is_some_and(|a| {
        matches!(a.trim().to_ascii_lowercase().as_str(), "select" | "true" | "1")
    })
}

/// マーク操作（反転・選択）後のカーソル移動量。引数があればその整数（原作 `ReverseFile(n)` 相当）、
/// 無ければ `down_after_select` に従い 1（下）か 0（移動なし）。
fn mark_move_delta(args: &[String], down_after_select: bool) -> isize {
    if let Some(v) = args.first().and_then(|a| a.trim().parse::<isize>().ok()) {
        return v;
    }
    if down_after_select { 1 } else { 0 }
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
