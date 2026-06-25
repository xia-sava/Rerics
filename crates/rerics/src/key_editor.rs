//! 設定ダイアログの「キー」ページ＝対話的キーマップエディタ。自前描画リストで機能順／キー順に
//! 並べ、検索・実打鍵キャプチャ割当・重複検出・個別削除/リマップ・ジャンル分けを行なう。
//! `config.keybinds` を下書き編集し、設定ダイアログの OK／適用で確定する（`settings_dialog::show` が生成）。

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use rerics_core::{Call, Command, CommandContext, KeyChord, KeyMap};
use winsafe::{self as w, co, gui, prelude::*};

use crate::script::ScriptCommand;
use crate::settings_dialog::{Shared, WM_PRINTCLIENT, label};
/// 「キー」ページが編集する対象のキーマップ。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyCategory {
    Filer,
    TextViewer,
    ImageViewer,
}

impl KeyCategory {
    /// このカテゴリで有効なコマンド文脈。
    fn context(self) -> CommandContext {
        match self {
            KeyCategory::Filer => CommandContext::Filer,
            KeyCategory::TextViewer => CommandContext::TextViewer,
            KeyCategory::ImageViewer => CommandContext::ImageViewer,
        }
    }

    /// このカテゴリの既定キーマップ（文字列マップ）。
    fn default_map(self) -> BTreeMap<String, String> {
        match self {
            KeyCategory::Filer => KeyMap::default().to_string_map(),
            KeyCategory::TextViewer => KeyMap::default_textviewer().to_string_map(),
            KeyCategory::ImageViewer => KeyMap::default_imageviewer().to_string_map(),
        }
    }

    /// debug-server で指すカテゴリ名。
    #[cfg(feature = "debug-server")]
    fn debug_str(self) -> &'static str {
        match self {
            KeyCategory::Filer => "filer",
            KeyCategory::TextViewer => "text",
            KeyCategory::ImageViewer => "image",
        }
    }

    /// 画面表示用のページ名。
    fn display(self) -> &'static str {
        match self {
            KeyCategory::Filer => "ファイラー",
            KeyCategory::TextViewer => "テキストビューア",
            KeyCategory::ImageViewer => "画像ビューア",
        }
    }
}

/// 機能順の 1 行＝1 つの機能欄の式と、その割り当てキー。割り当ての無い機能は
/// `chord: None` の 1 行で出す（そこへキャプチャして割当）。同じ機能の複数キー・引数違いは別行に割れる。
struct BindRow {
    /// 機能欄の式（`cursorDown()`・`copy("a")`・`organize()`・`{ コード }`）。未割当行も式を持つ。
    expr: String,
    /// 割り当てキー（未割当なら `None`）。
    chord: Option<String>,
    /// この chord に複数機能がある（衝突）。
    conflicted: bool,
}

/// キー順の 1 行＝1 chord と、それを定義している全機能（2 つ以上＝衝突）。`labels` は機能トークン
/// （命令の同一判定・debug 観測用）、`values` は機能欄の式（機能名／実呼び出しカラムの描画用）。
/// 両者は同じ並びで対応する。
struct KeyChordRow {
    chord: String,
    labels: Vec<String>,
    values: Vec<String>,
}

/// 機能順リストの表示行。ジャンル見出しを行間に挟むので、データ行（`view` 上の位置）と
/// 見出し行が混在する。`top`（スクロール）はこの表示行を単位に進む。
#[derive(Clone)]
enum DisplayLine {
    /// 全幅のジャンル見出し（選択不可）。独自ジャンル名も載るので所有 String。
    Header(String),
    /// データ行＝`view` 上の位置（`sel` はこの値で指す）。
    Row(usize),
}

/// 行を識別する機能トークン（命令の同一判定・debug 観測用）。組込呼び出しはコマンドトークン
/// （`copy`・`jumpDialog`）、`name()` 形のスクリプト呼び出しはその名前、それ以外（コード・複文）は
/// 式ソースそのまま。
fn row_token(expr: &str) -> String {
    match Call::parse(expr) {
        Call::Builtin { command, .. } => command.as_token().to_string(),
        Call::Script { source } => script_call_name(&source).unwrap_or(source),
    }
}

/// 実呼び出しの表示文字列（機能順の中央カラム）。組込はトークン＋引数（`changeDirectory("D:")`）、
/// スクリプト・コードは式ソースそのまま。
fn call_display(expr: &str) -> String {
    Call::parse(expr).to_expr()
}

/// 式が `name()`（引数なしの裸呼び出し）ならその名前を返す。登録スクリプトの引き当てに使う。
/// 引数つき・コード・複文は `None`。
fn script_call_name(expr: &str) -> Option<String> {
    let body = expr.trim().strip_prefix("rerics.").or_else(|| expr.trim().strip_prefix("r.")).unwrap_or(expr.trim());
    let name = body.strip_suffix("()")?.trim();
    let mut chars = name.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$');
    (first_ok && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')).then(|| name.to_string())
}

/// ジャンル名 → 並び順。組込ジャンル名に一致すればその順、未知（独自）なら「スクリプト」と同じ
/// 末尾（14）。スクリプトが `genre: "ファイル操作"` などで組込群に混ざれるようにするための引き当て。
fn genre_order(name: &str) -> u8 {
    Command::all()
        .map(command_genre)
        .find(|(_, g)| *g == name)
        .map(|(o, _)| o)
        .unwrap_or(14)
}

/// キー順の機能ラベル（トークン）を画面表示用の日本語名へ変換する。未知トークンはそのまま。
fn label_display(label: &str) -> String {
    Command::from_token(label)
        .map(|c| c.display_name().to_string())
        .unwrap_or_else(|| label.to_string())
}

/// 機能ピッカーの並び・見出し用ジャンル。`(並び順, 見出し)` を返す。並びは機能ピッカーを
/// ジャンルごとに固まらせるためのもので、設定 UI 専用の括り（コアの文脈分けとは別軸）。
/// メニュー編集のコマンドピッカー（[`crate::settings_dialog`]）も同じ括りで流用する。
pub(crate) fn command_genre(cmd: Command) -> (u8, &'static str) {
    use Command::*;
    match cmd {
        CursorUp | CursorDown | CursorTop | CursorEnd | CursorPageUp | CursorPageDown
        | SetCursorPosition | CursorOpposite => (0, "カーソル移動"),
        EnterDir | ToParent | ToRoot | HistoryBack | HistoryForward | PathHistoryDialog
        | ChangeDirectory | ChangeDirectoryDialog | ChangeDrive | ChangeDriveDialog | JumpDialog
        | PathRegisterDialog | IncrementalSearchDialog | NextDrive | PreviousDrive => (1, "移動・ナビゲーション"),
        MarkToggle | SelectAll | ClearAll | ReverseAll | SelectAllFile | ReverseAllFile
        | SelectFile | SelectMask | PathMask => (2, "選択"),
        Reload | Refresh | View | ViewFile | DirectoryInformation | SortByName | SortByExtension
        | SortBySize | SortByDate | Sort | SortDialog | SortReverseToggle => (3, "表示・並べ替え"),
        PageNext | PagePrevious | NewFiler | Exit => (4, "タブ"),
        FocusLeft | FocusRight | SwapPath | OppositeToCurrent | CurrentToOpposite | MaximizeLeft
        | MaximizeRight | MaximizeLeftForce | MaximizeRightForce | MaximizeCurrent | BorderLeft
        | BorderRight | BorderReset => (5, "ペイン"),
        MakeDirectory | Copy | Move | Rename | RenameSequenceDialog | Delete | SendToRecycled
        | CreateShortcut | ClipCopy | ClipCut | ClipPaste | CreateFileDialog | Edit | PropertyDialog
        | Compress | Extract => (6, "ファイル操作"),
        OpenTaskManager | OpenSettings | KeyBindsDialog | CommandDirect | Menu | CopyLog | ClearLog
        | Nop | End | Restart | Quit | MaximizeWindow | MinimizeWindow => (7, "アプリ・その他"),
        ViewerScrollUp | ViewerScrollDown | ViewerPageUp | ViewerPageDown | ViewerScrollTop
        | ViewerScrollBottom => (8, "スクロール"),
        ViewerSearchDialog | ViewerFindNext | ViewerFindPrevious => (9, "検索"),
        ViewerClose | ViewerSelectAll | ViewerToggleMode | ViewerChangeEncoding | ViewerCopy
        | ViewerContextMenu => (10, "ビューア操作"),
        ImageNext | ImagePrevious | ImageZoomIn | ImageZoomOut | ImageFitWindow | ImageActualSize
        | ImageFitWidth | ImageFitHeight | ImageFitLarge => (11, "表示・ズーム"),
        ImageRotateRight | ImageRotateLeft | ImageFlipHorizontal | ImageFlipVertical => {
            (12, "回転・反転")
        }
        ImageCopy | MediaTogglePlay => (13, "画像操作"),
    }
}

/// キー編集ページの並べ方。
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyView {
    /// 機能順（コマンド → 割り当てキー群）。
    ByCommand,
    /// キー順（キー → 起動する機能）。
    ByKey,
}

/// 機能ピッカー（インライン）の対象。キー順で機能ラベルをダブルクリックすると、その chord の
/// `old_value`（差し替え対象の生 invocation）を別機能へ置き換えるためにこの状態へ入る。
struct PickState {
    chord: String,
    old_value: String,
}

/// 「キー」ページの編集状態。
struct KeyEditorInner {
    shared: Rc<Shared>,
    category: KeyCategory,
    /// 登録済みスクリプトコマンドのメタ（名前→`{label, genre}`）。未割当でも一覧に出して割り当て
    /// 可能にし、表示名カラム／ジャンル見出しに使う。
    script_meta: HashMap<String, ScriptCommand>,
    /// `r.` で呼べるメンバー名（補完候補）。引数/コード欄の補完に渡す。
    members: Vec<String>,
    /// 編集中の下書き＝chord → 割り当て値（生の invocation 文字列）のリスト。空 Vec＝明示 unbind。
    /// **重複（1 つの chord に複数機能）を許す**＝これが衝突状態。未知バインド（`Func_*` 等）も
    /// 生値のまま保持し、反映時に消さない。OK/適用の検証を通った時だけ `config.keybinds` へ書き戻す。
    draft: RefCell<BTreeMap<String, Vec<String>>>,
    rows: RefCell<Vec<BindRow>>,
    /// キー順ビューの行（chord でソート）。`draft` から組む。
    key_rows: RefCell<Vec<KeyChordRow>>,
    /// ピックモード中ならその対象。`Some` の間はリストが機能ピッカー（全機能一覧）へ切り替わる。
    picking: RefCell<Option<PickState>>,
    /// ピックモードで選べる機能一覧（文脈で絞り済み）。`view` はこれを検索で絞った index。
    pick_rows: RefCell<Vec<Command>>,
    /// キー順で「キー定義を追加」したが、まだ機能未割当のキー（－表示の空キー定義）。
    pending: RefCell<Vec<String>>,
    /// 並べ方（機能順／キー順）。
    view_mode: Cell<KeyView>,
    /// 検索で絞り込んだ表示対象＝現モードの行配列へのインデックス（表示順）。`sel`/`top` はこの上の位置。
    view: RefCell<Vec<usize>>,
    /// 検索クエリ（機能名・キーへの部分一致・大小無視）。空なら全件表示。
    query: RefCell<String>,
    sel: Cell<usize>,
    /// 表示先頭行（スクロール）＝表示行（見出し込み）単位の先頭位置。
    top: Cell<usize>,
    row_h: Cell<i32>,
    /// 次の打鍵を選択行のコマンドへ割り当てる待ち状態。
    capturing: Cell<bool>,
    /// キャプチャがサブ選択キーの「変更（リマップ）」か（`true`）、新規追加か（`false`）。
    capturing_remap: Cell<bool>,
    /// キャプチャがキー順の「空キー定義の新規作成」か（`true`）。打鍵を pending へ入れる。
    capturing_newdef: Cell<bool>,
    /// 「式を編集」で書かれたが、まだキーへ結んでいない式（`copy("=式")`・`{ コード }`・登録外の呼び出し）。
    /// 未割当（－）行として並べ、通常のキャプチャで割り当てる。
    pending_exprs: RefCell<Vec<String>>,
    /// 直近の操作結果（観測・状態表示用）。
    status: RefCell<String>,
}

/// 「キー」ページ（割り当ての対話編集・自前描画）。機能順にコマンドを並べ、行を選んで
/// 「キー定義を追加」→実際のキー打鍵で割り当てる。`config.keybinds` を直接編集し OK/適用で確定する。
#[derive(Clone)]
pub(crate) struct KeyEditor {
    list: gui::WindowControl,
    search: gui::Edit,
    /// 機能順／キー順の並べ替え切替。
    toggle: gui::RadioGroup,
    /// 選択対象に効く左グループの 3 ボタン（モードでラベル/動作を切替）。
    btn_a: gui::Button,
    btn_b: gui::Button,
    btn_c: gui::Button,
    /// 「式を編集」ボタン。機能順で行を選んでいるときだけ有効（キー順では無効）。
    btn_edit: gui::Button,
    /// 上部ヒント（モードで文面を差し替える）。
    hint: gui::Label,
    inner: Rc<KeyEditorInner>,
}

impl KeyEditor {
    pub(crate) fn new(
        parent: &gui::WindowControl,
        shared: &Rc<Shared>,
        category: KeyCategory,
        scripts: Vec<ScriptCommand>,
        members: Vec<String>,
    ) -> Self {
        // 上部ヒント：モード（機能順／キー順／機能ピッカー）に応じて文面を差し替える。
        // ピッカー中は中止方法をここに大きく出して、背景色と合わせて別モードを明示する。
        let hint = gui::Label::new(
            parent,
            gui::LabelOpts {
                text: "機能を選び「キー定義を追加」で割り当て（実際にキーを押す・右クリックで中止）",
                position: gui::dpi(16, 12),
                size: gui::dpi(744, 18),
                ..Default::default()
            },
        );
        label(parent, "検索:", 16, 44, 40);
        let search = gui::Edit::new(
            parent,
            gui::EditOpts {
                control_style: co::ES::AUTOHSCROLL,
                position: gui::dpi(60, 40),
                width: gui::dpi_x(500),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );
        let toggle = gui::RadioGroup::new(
            parent,
            &[
                gui::RadioButtonOpts {
                    text: "機能順(&F)",
                    position: gui::dpi(572, 42),
                    size: gui::dpi(92, 20),
                    selected: true,
                    ..Default::default()
                },
                gui::RadioButtonOpts {
                    text: "キー順(&Y)",
                    position: gui::dpi(668, 42),
                    size: gui::dpi(92, 20),
                    ..Default::default()
                },
            ],
        );
        let list = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                position: gui::dpi(16, 72),
                size: gui::dpi(744, 372),
                class_bg_brush: gui::Brush::Color(co::COLOR::WINDOW),
                style: co::WS::CHILD
                    | co::WS::VISIBLE
                    | co::WS::CLIPSIBLINGS
                    | co::WS::TABSTOP
                    | co::WS::BORDER
                    | co::WS::VSCROLL,
                ..Default::default()
            },
        );
        // 段1＝選択した行に効く操作。左から「キー定義の追加/変更/削除」、間隔をあけて「引数を編集」。
        // 3 ボタンのラベルはモードで切り替わる（`relabel_buttons`）。
        let btn_a = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーを追加(&K)",
                position: gui::dpi(16, 454),
                width: gui::dpi_x(140),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        let btn_b = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーを変更(&C)",
                position: gui::dpi(162, 454),
                width: gui::dpi_x(140),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        let btn_c = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーを削除(&D)",
                position: gui::dpi(308, 454),
                width: gui::dpi_x(140),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        // 選択中の行の機能欄の式を編集する（モーダル→打鍵で割り当て）。組込呼び出しの引数付け・
        // 登録スクリプトの呼び替え・コード（複文）の記述まで、機能欄はすべてこの 1 つで編集する。
        let btn_edit = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "式を編集(&E)",
                position: gui::dpi(470, 454),
                width: gui::dpi_x(130),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        // 段2＝選択に依らない操作。右端にキーマップ全体のリセット（破壊力が強いのでラベルを明示する）。
        let reset = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーマップ全てを既定に戻す(&R)",
                position: gui::dpi(530, 490),
                width: gui::dpi_x(230),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        let me = Self {
            list,
            search,
            toggle,
            btn_a: btn_a.clone(),
            btn_b: btn_b.clone(),
            btn_c: btn_c.clone(),
            btn_edit: btn_edit.clone(),
            hint,
            inner: Rc::new(KeyEditorInner {
                shared: shared.clone(),
                category,
                script_meta: scripts.into_iter().map(|c| (c.name.clone(), c)).collect(),
                members,
                draft: RefCell::new(BTreeMap::new()),
                rows: RefCell::new(Vec::new()),
                key_rows: RefCell::new(Vec::new()),
                picking: RefCell::new(None),
                pick_rows: RefCell::new(Vec::new()),
                pending: RefCell::new(Vec::new()),
                view_mode: Cell::new(KeyView::ByCommand),
                view: RefCell::new(Vec::new()),
                query: RefCell::new(String::new()),
                sel: Cell::new(0),
                top: Cell::new(0),
                row_h: Cell::new(gui::dpi_y(22)),
                capturing: Cell::new(false),
                capturing_remap: Cell::new(false),
                capturing_newdef: Cell::new(false),
                pending_exprs: RefCell::new(Vec::new()),
                status: RefCell::new(String::new()),
            }),
        };
        me.load_draft();
        me.rebuild_rows();
        me.setup_events();
        {
            let this = me.clone();
            me.search.on().en_change(move || {
                let q = this.search.text().unwrap_or_default();
                this.apply_query(&q);
                Ok(())
            });
        }
        {
            let this = me.clone();
            me.toggle.on().bn_clicked(move || {
                this.set_view(this.toggle.selected_index() == Some(1));
                Ok(())
            });
        }
        // ボタン1：機能順＝キーを追加／キー順＝機能を変更（ピッカー）。
        {
            let this = me.clone();
            btn_a.on().bn_clicked(move || {
                if this.inner.view_mode.get() == KeyView::ByCommand {
                    this.begin_capture();
                } else {
                    this.enter_pick(0);
                }
                Ok(())
            });
        }
        // ボタン2：機能順＝キーを変更（リマップ）／キー順＝キーを削除。
        {
            let this = me.clone();
            btn_b.on().bn_clicked(move || {
                if this.inner.view_mode.get() == KeyView::ByCommand {
                    this.begin_remap();
                } else {
                    this.unbind_selected();
                }
                Ok(())
            });
        }
        // ボタン3：機能順＝キー定義を削除／キー順＝キー定義を追加（空キー定義を作る）。
        {
            let this = me.clone();
            btn_c.on().bn_clicked(move || {
                if this.inner.view_mode.get() == KeyView::ByCommand {
                    this.unbind_selected();
                } else {
                    this.add_key_def();
                }
                Ok(())
            });
        }
        {
            let this = me.clone();
            btn_edit.on().bn_clicked(move || {
                this.edit_expr_row();
                Ok(())
            });
        }
        {
            let this = me.clone();
            reset.on().bn_clicked(move || {
                this.reset();
                Ok(())
            });
        }
        me.register_debug();
        me
    }

    /// 現在のモードに合わせて左グループ 3 ボタンのラベルを更新する。
    fn relabel_buttons(&self) {
        let (a, b, c) = match self.inner.view_mode.get() {
            KeyView::ByCommand => ("キー定義を追加(&K)", "キー定義を変更(&C)", "キー定義を削除(&D)"),
            KeyView::ByKey => ("機能定義を変更(&C)", "キー定義を削除(&D)", "キー定義を追加(&K)"),
        };
        let _ = self.btn_a.hwnd().SetWindowText(a);
        let _ = self.btn_b.hwnd().SetWindowText(b);
        let _ = self.btn_c.hwnd().SetWindowText(c);
    }

    /// 「式を編集」ボタンの表示と有効/無効を状態に合わせる。式編集は機能順専用（キー順の 1 行は
    /// 複数機能を持ちうるので「どの機能の式か」が一意に定まらない）。そのためキー順ではボタン自体を
    /// 隠し、機能順では行を選んでいるときだけ有効にする（ピック中・見出し/未選択ではグレーアウト）。
    fn update_edit_button(&self) {
        // 窓未作成（構築途中）の間は触らない＝既定の有効のまま。
        if self.btn_edit.hwnd().ptr().is_null() {
            return;
        }
        if self.inner.view_mode.get() == KeyView::ByKey {
            self.btn_edit.hwnd().ShowWindow(co::SW::HIDE);
            return;
        }
        self.btn_edit.hwnd().ShowWindow(co::SW::SHOW);
        let enabled = self.inner.picking.borrow().is_none() && self.selected_bind().is_some();
        self.btn_edit.hwnd().EnableWindow(enabled);
    }

    /// 上部ヒントの文面を現モードに合わせて更新する。ピッカー中は中止方法をここに明示する。
    fn update_hint(&self) {
        let text = if self.inner.picking.borrow().is_some() {
            "◆ 機能ピッカー：割り当てる機能を選んで Enter／ダブルクリック（右クリックか Esc で中止）"
        } else {
            match self.inner.view_mode.get() {
                KeyView::ByCommand => {
                    "機能を選び「キー定義を追加」で割り当て（実際にキーを押す・右クリックで中止）"
                }
                KeyView::ByKey => {
                    "キーを選び「機能定義を変更」で機能を割り当て（－はダブルクリックでも割り当て可）"
                }
            }
        };
        let _ = self.hint.hwnd().SetWindowText(text);
    }

    fn hwnd(&self) -> &w::HWND {
        self.list.hwnd()
    }

    /// debug-server からこのページを観測・駆動できるようフックを登録する。実打鍵キャプチャと
    /// 同じ `assign`/`unbind_selected`/`reset` を叩く＝挙動が一本化される。
    #[cfg(feature = "debug-server")]
    fn register_debug(&self) {
        use crate::debug_server::modal_registry::{KeyEditorHooks, KeyEditorState};
        let read = {
            let this = self.clone();
            Box::new(move || {
                let picking = this.inner.picking.borrow().is_some();
                let by_key = this.inner.view_mode.get() == KeyView::ByKey;
                let view = this.inner.view.borrow();
                let rows = if picking {
                    let pr = this.inner.pick_rows.borrow();
                    view.iter()
                        .filter_map(|&i| pr.get(i))
                        .map(|c| (c.as_token().to_string(), Vec::new()))
                        .collect()
                } else if by_key {
                    let all = this.inner.key_rows.borrow();
                    view.iter()
                        .map(|&ri| {
                            let r = &all[ri];
                            (r.chord.clone(), r.labels.clone())
                        })
                        .collect()
                } else {
                    let all = this.inner.rows.borrow();
                    view.iter()
                        .map(|&ri| {
                            let r = &all[ri];
                            (row_token(&r.expr), r.chord.iter().cloned().collect())
                        })
                        .collect()
                };
                KeyEditorState {
                    rows,
                    selected: this.inner.sel.get(),
                    top: this.inner.top.get(),
                    capturing: this.inner.capturing.get(),
                    picking,
                    status: this.inner.status.borrow().clone(),
                    query: this.inner.query.borrow().clone(),
                    mode: if by_key { "key" } else { "command" }.to_string(),
                    conflicts: this.conflicts(),
                }
            }) as Box<dyn Fn() -> KeyEditorState>
        };
        let select = {
            let this = self.clone();
            Box::new(move |index: usize| {
                if index < this.inner.view.borrow().len() {
                    this.clear_status();
                    this.inner.sel.set(index);
                    this.ensure_visible();
                    let _ = this.hwnd().InvalidateRect(None, false);
                }
            }) as Box<dyn Fn(usize)>
        };
        let bind = {
            let this = self.clone();
            Box::new(move |command: &str, chord: &str| {
                let cmd = Command::from_token(command)
                    .ok_or_else(|| format!("unknown command: {command}"))?;
                let ch = KeyChord::parse(chord)
                    .ok_or_else(|| format!("unknown chord: {chord}"))?;
                let pos = {
                    let view = this.inner.view.borrow();
                    if this.inner.view_mode.get() == KeyView::ByKey {
                        let krows = this.inner.key_rows.borrow();
                        view.iter()
                            .position(|&ri| krows[ri].labels.iter().any(|l| Command::from_token(l) == Some(cmd)))
                    } else {
                        let rows = this.inner.rows.borrow();
                        view.iter().position(|&ri| row_token(&rows[ri].expr) == command)
                    }
                };
                if let Some(di) = pos {
                    this.inner.sel.set(di);
                }
                this.assign(format!("{}()", cmd.as_token()), ch);
                Ok(())
            }) as Box<dyn Fn(&str, &str) -> Result<(), String>>
        };
        let unbind = {
            let this = self.clone();
            Box::new(move || this.unbind_selected()) as Box<dyn Fn()>
        };
        let reset = {
            let this = self.clone();
            Box::new(move || this.reset()) as Box<dyn Fn()>
        };
        let search = {
            let this = self.clone();
            Box::new(move |q: &str| {
                let _ = this.search.set_text(q);
                this.apply_query(q);
            }) as Box<dyn Fn(&str)>
        };
        let set_view = {
            let this = self.clone();
            Box::new(move |by_key: bool| this.set_view(by_key)) as Box<dyn Fn(bool)>
        };
        let rebind = {
            let this = self.clone();
            Box::new(move |chord: &str| {
                let ch = KeyChord::parse(chord)
                    .ok_or_else(|| format!("unknown chord: {chord}"))?;
                this.remap(ch);
                Ok(())
            }) as Box<dyn Fn(&str) -> Result<(), String>>
        };
        // 選択行へ打鍵を割り当てる（begin_capture→キー押下と同じ＝行の式を束ねる）。
        // 引数つき呼び出し・スクリプト・コード行を割り当てるのに使う（`bind` は bare 機能のみ）。
        let capture = {
            let this = self.clone();
            Box::new(move |chord: &str| {
                let ch = KeyChord::parse(chord)
                    .ok_or_else(|| format!("unknown chord: {chord}"))?;
                let Some((expr, _)) = this.selected_bind() else {
                    return Err("no row selected".to_string());
                };
                this.assign(expr, ch);
                Ok(())
            }) as crate::debug_server::modal_registry::ChordFn
        };
        // 選択中の行の機能欄の式を差し替える（「式を編集」モーダル OK と同じ＝モーダル抜き）。
        // 割り当ては通常どおり行を選んで capture する。
        let set_expr = {
            let this = self.clone();
            Box::new(move |expr: &str| this.apply_expr(expr)) as Box<dyn Fn(&str)>
        };
        // 実際の「式を編集」モーダル（補完つき）を開く＝補完 UI を headless で観測・駆動するため。
        let open_expr = {
            let this = self.clone();
            Box::new(move || this.edit_expr_row()) as Box<dyn Fn()>
        };
        let pick = {
            let this = self.clone();
            Box::new(move |li: usize| this.enter_pick(li)) as Box<dyn Fn(usize)>
        };
        let pick_commit = {
            let this = self.clone();
            Box::new(move || this.commit_pick()) as Box<dyn Fn()>
        };
        let pick_cancel = {
            let this = self.clone();
            Box::new(move || this.cancel_pick()) as Box<dyn Fn()>
        };
        let scroll = {
            let this = self.clone();
            Box::new(move |top: i32| this.scroll_to(top as isize)) as Box<dyn Fn(i32)>
        };
        let add_keydef = {
            let this = self.clone();
            Box::new(move |chord: &str| {
                let ch = KeyChord::parse(chord)
                    .ok_or_else(|| format!("unknown chord: {chord}"))?;
                this.finish_newdef(ch);
                Ok(())
            }) as crate::debug_server::modal_registry::ChordFn
        };
        crate::debug_server::modal_registry::register_key_editor(
            self.inner.category.debug_str(),
            KeyEditorHooks {
                read,
                select,
                bind,
                unbind,
                reset,
                search,
                set_view,
                rebind,
                capture,
                set_expr,
                open_expr,
                pick,
                pick_commit,
                pick_cancel,
                scroll,
                add_keydef,
            },
        );
    }

    /// debug-server 無効ビルドでは何もしない。
    #[cfg(not(feature = "debug-server"))]
    fn register_debug(&self) {}

    /// `config` の当該カテゴリのキーマップへの参照。下書きの読み込み／書き戻しにのみ使う。
    fn with_cfg_map<R>(&self, f: impl FnOnce(&mut BTreeMap<String, String>) -> R) -> R {
        let mut cfg = self.inner.shared.cfg.borrow_mut();
        let map = match self.inner.category {
            KeyCategory::Filer => &mut cfg.keybinds,
            KeyCategory::TextViewer => &mut cfg.keybinds_textviewer,
            KeyCategory::ImageViewer => &mut cfg.keybinds_imageviewer,
        };
        f(map)
    }

    /// `config.keybinds` から下書きを作る。空値＝明示 unbind は空 Vec、非空は 1 要素のリスト。
    /// 未知バインドも生値のまま保持する（反映時に消さないため）。
    fn load_draft(&self) {
        let map = self.with_cfg_map(|m| m.clone());
        let mut draft: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (chord, val) in map {
            if val.trim().is_empty() {
                draft.insert(chord, Vec::new());
            } else {
                draft.insert(chord, vec![val]);
            }
        }
        *self.inner.draft.borrow_mut() = draft;
    }

    /// 下書きを `config.keybinds` へ書き戻す（衝突が無いと検証済みの時だけ呼ぶ）。
    /// 空 Vec＝unbind は空文字で残す（既定キーの打ち消し）、1 要素はその値。
    pub(crate) fn flush_draft(&self) {
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        for (chord, vals) in self.inner.draft.borrow().iter() {
            let first = vals.iter().find(|v| !v.trim().is_empty());
            out.insert(chord.clone(), first.cloned().unwrap_or_default());
        }
        self.with_cfg_map(|m| *m = out);
    }

    /// 衝突（1 つの chord に 2 機能以上）を chord 昇順で列挙する＝`(chord, ラベル群)`。
    fn conflicts(&self) -> Vec<(String, Vec<String>)> {
        self.inner
            .draft
            .borrow()
            .iter()
            .filter_map(|(chord, vals)| {
                let labels: Vec<String> = vals
                    .iter()
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| row_token(v))
                    .collect();
                (labels.len() > 1).then(|| (chord.clone(), labels))
            })
            .collect()
    }

    /// 衝突の短い要約（ダイアログ全域の警告用）。衝突が無ければ `None`。
    pub(crate) fn conflict_brief(&self) -> Option<String> {
        let c = self.conflicts();
        if c.is_empty() {
            return None;
        }
        let parts: Vec<String> = c
            .iter()
            .take(3)
            .map(|(chord, labels)| format!("{}={}", chord, labels.join("/")))
            .collect();
        let more = if c.len() > 3 {
            format!(" ほか{}件", c.len() - 3)
        } else {
            String::new()
        };
        Some(format!("{}: {}{}", self.inner.category.display(), parts.join(", "), more))
    }

    /// 衝突をステータスへ書き出す（OK/適用が弾いた理由を見せる）。
    pub(crate) fn note_conflicts(&self) {
        let desc: Vec<String> = self
            .conflicts()
            .into_iter()
            .map(|(chord, labels)| format!("{}（{}）", chord, labels.join(", ")))
            .collect();
        *self.inner.status.borrow_mut() =
            format!("キーの重複を解決してください: {}", desc.join(" / "));
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 機能欄の式の機能名（機能順の左カラム・キー順の機能カラム）。`name()` 形の登録スクリプトは
    /// メタの `label` があればそれ、無ければ名前。組込はコマンドの表示名、コード・複文は式ソース。
    fn value_label(&self, expr: &str) -> String {
        if let Some(name) = script_call_name(expr) {
            if let Some(label) = self.inner.script_meta.get(&name).and_then(|m| m.label.as_ref()) {
                return label.clone();
            }
            if matches!(Call::parse(expr), Call::Script { .. }) {
                return name;
            }
        }
        match Call::parse(expr) {
            Call::Builtin { command, .. } => command.display_name().to_string(),
            Call::Script { source } => source,
        }
    }

    /// 機能順の行が属するジャンル（並び順キー・見出し名）。登録スクリプト行でメタに `genre` が
    /// あればそれを使い（独自グループに分けられる）、組込はコマンド由来、その他コードは「スクリプト」。
    fn row_genre(&self, row: &BindRow) -> (u8, String) {
        self.expr_genre(&row.expr)
    }

    /// 機能欄の式が属するジャンル（並び順キー・見出し名）。
    fn expr_genre(&self, expr: &str) -> (u8, String) {
        match Call::parse(expr) {
            Call::Builtin { command, .. } => {
                let (o, g) = command_genre(command);
                (o, g.to_string())
            }
            Call::Script { source } => {
                if let Some(name) = script_call_name(&source)
                    && let Some(genre) = self.inner.script_meta.get(&name).and_then(|m| m.genre.as_ref())
                {
                    return (genre_order(genre), genre.clone());
                }
                (14, "スクリプト".to_string())
            }
        }
    }

    /// 下書きから機能順 `rows`（1 バインド＝1 行）・キー順 `key_rows` を組み直す。
    /// 未割当の機能は `chord: None` の 1 行で出す。式は組込呼び出し・登録スクリプト・コードの別なく扱う。
    fn rebuild_rows(&self) {
        // 機能欄の式 → そのバインド群（(chord, 衝突)）。
        let mut binds_by_expr: HashMap<String, Vec<(String, bool)>> = HashMap::new();
        let mut key_rows: Vec<KeyChordRow> = Vec::new();
        {
            let draft = self.inner.draft.borrow();
            for (chord, vals) in draft.iter() {
                let nonempty: Vec<&String> =
                    vals.iter().filter(|v| !v.trim().is_empty()).collect();
                if nonempty.is_empty() {
                    continue;
                }
                let conflicted = nonempty.len() > 1;
                let labels: Vec<String> = nonempty.iter().map(|v| row_token(v)).collect();
                let values: Vec<String> = nonempty.iter().map(|v| (*v).clone()).collect();
                key_rows.push(KeyChordRow { chord: chord.clone(), labels, values });
                for v in &nonempty {
                    binds_by_expr.entry((*v).clone()).or_default().push((chord.clone(), conflicted));
                }
            }
        }
        // 機能未割当の空キー定義（pending）をキー順に並べる（－表示）。既にバインドされたものは除く。
        {
            let bound: std::collections::HashSet<String> =
                key_rows.iter().map(|r| r.chord.clone()).collect();
            for chord in self.inner.pending.borrow().iter() {
                if !bound.contains(chord) {
                    key_rows.push(KeyChordRow {
                        chord: chord.clone(),
                        labels: Vec::new(),
                        values: Vec::new(),
                    });
                }
            }
        }
        // 式を「組込コマンド別」と「スクリプト・コード」へ仕分ける。組込はコマンドごとに固めて並べ、
        // スクリプト・コードは名前/ソート順で固定して並べる（割り当てても位置が動かない）。
        let mut bound_by_cmd: HashMap<Command, Vec<(String, String, bool)>> = HashMap::new();
        let mut pend_by_cmd: HashMap<Command, Vec<String>> = HashMap::new();
        let mut script_set: BTreeSet<String> = BTreeSet::new();
        for (expr, binds) in &binds_by_expr {
            match Call::parse(expr) {
                Call::Builtin { command, .. } => {
                    let slot = bound_by_cmd.entry(command).or_default();
                    for (chord, conflicted) in binds {
                        slot.push((chord.clone(), expr.clone(), *conflicted));
                    }
                }
                Call::Script { .. } => {
                    script_set.insert(expr.clone());
                }
            }
        }
        for v in self.inner.pending_exprs.borrow().iter() {
            match Call::parse(v) {
                Call::Builtin { command, .. } => pend_by_cmd.entry(command).or_default().push(v.clone()),
                Call::Script { .. } => {
                    script_set.insert(v.clone());
                }
            }
        }
        // 登録スクリプトは `name()` 形で（バインドが無くても）常に出す。
        for name in self.inner.script_meta.keys() {
            script_set.insert(format!("{name}()"));
        }
        let ctx = self.inner.category.context();
        // 表示する機能＝文脈内の全組込 ＋ 文脈外でもバインドのある組込。
        let mut cmds: Vec<Command> = Command::all().filter(|c| c.available_in(ctx)).collect();
        let mut extra: Vec<Command> =
            bound_by_cmd.keys().copied().filter(|c| !c.available_in(ctx)).collect();
        extra.sort_by_key(|c| c.as_token());
        cmds.extend(extra);
        let mut rows: Vec<BindRow> = Vec::new();
        for command in cmds {
            let pend = pend_by_cmd.remove(&command).unwrap_or_default();
            match bound_by_cmd.remove(&command) {
                Some(mut binds) => {
                    binds.sort_by(|a, b| a.0.cmp(&b.0));
                    let bound: std::collections::HashSet<String> =
                        binds.iter().map(|(_, e, _)| e.clone()).collect();
                    for (chord, expr, conflicted) in binds {
                        rows.push(BindRow { expr, chord: Some(chord), conflicted });
                    }
                    // バインド済みと重複しない引数つき式は未割当行として残す。
                    for v in pend {
                        if !bound.contains(&v) {
                            rows.push(BindRow { expr: v, chord: None, conflicted: false });
                        }
                    }
                }
                // 未割当＝bare 呼び出しの空行（そこへキャプチャして割り当てる）＋引数つき未割当行。
                None => {
                    rows.push(BindRow {
                        expr: format!("{}()", command.as_token()),
                        chord: None,
                        conflicted: false,
                    });
                    for v in pend {
                        rows.push(BindRow { expr: v, chord: None, conflicted: false });
                    }
                }
            }
        }
        // スクリプト・コード行：登録スクリプト＋バインド/pending のコードを、名前/ソート順に固定で並べる。
        for expr in script_set {
            match binds_by_expr.get(&expr) {
                Some(binds) => {
                    let mut binds = binds.clone();
                    binds.sort_by(|a, b| a.0.cmp(&b.0));
                    for (chord, conflicted) in binds {
                        rows.push(BindRow { expr: expr.clone(), chord: Some(chord), conflicted });
                    }
                }
                None => rows.push(BindRow { expr, chord: None, conflicted: false }),
            }
        }
        key_rows.sort_by(|a, b| a.chord.cmp(&b.chord));
        *self.inner.rows.borrow_mut() = rows;
        *self.inner.key_rows.borrow_mut() = key_rows;
        self.rebuild_view();
    }

    /// 検索クエリで現モードの行を絞り込み、表示対象 `view` を組み直す。選択を範囲内へ収める。
    /// ピックモード中は機能一覧（`pick_rows`）を絞り込む。
    fn rebuild_view(&self) {
        let q = self.inner.query.borrow().to_lowercase();
        if self.inner.picking.borrow().is_some() {
            let view: Vec<usize> = self
                .inner
                .pick_rows
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    q.is_empty()
                        || c.as_token().to_lowercase().contains(&q)
                        || c.display_name().to_lowercase().contains(&q)
                })
                .map(|(i, _)| i)
                .collect();
            let n = view.len();
            *self.inner.view.borrow_mut() = view;
            if n == 0 {
                self.inner.sel.set(0);
                self.inner.top.set(0);
            } else if self.inner.sel.get() >= n {
                self.inner.sel.set(n - 1);
            }
            self.update_scrollbar();
            self.update_edit_button();
            return;
        }
        let mut view: Vec<usize> = match self.inner.view_mode.get() {
            KeyView::ByCommand => self
                .inner
                .rows
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    q.is_empty()
                        || row_token(&r.expr).to_lowercase().contains(&q)
                        || self.value_label(&r.expr).to_lowercase().contains(&q)
                        || r.chord.as_ref().is_some_and(|c| c.to_lowercase().contains(&q))
                        || call_display(&r.expr).to_lowercase().contains(&q)
                })
                .map(|(i, _)| i)
                .collect(),
            KeyView::ByKey => self
                .inner
                .key_rows
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    q.is_empty()
                        || r.chord.to_lowercase().contains(&q)
                        || r.labels.iter().any(|l| l.to_lowercase().contains(&q))
                        || r.values.iter().any(|v| self.value_label(v).to_lowercase().contains(&q))
                        || r.values.iter().any(|v| call_display(v).to_lowercase().contains(&q))
                })
                .map(|(i, _)| i)
                .collect(),
        };
        // 機能順はジャンルごとに固める（同順内は元の並び＝enum/名前順）。独自ジャンルは名前で
        // さらに揃え、同ジャンルが散らばらないようにする。見出しは描画時に境界で出す。
        if self.inner.view_mode.get() == KeyView::ByCommand {
            let rows = self.inner.rows.borrow();
            view.sort_by_key(|&i| self.row_genre(&rows[i]));
        }
        let n = view.len();
        *self.inner.view.borrow_mut() = view;
        if n == 0 {
            self.inner.sel.set(0);
            self.inner.top.set(0);
        } else if self.inner.sel.get() >= n {
            self.inner.sel.set(n - 1);
        }
        self.update_scrollbar();
        self.update_edit_button();
    }

    /// 検索クエリを適用して表示を絞り込む（`config` は変更しない）。同じ値なら何もしない。
    fn apply_query(&self, q: &str) {
        {
            let mut cur = self.inner.query.borrow_mut();
            if *cur == q {
                return;
            }
            *cur = q.to_string();
        }
        self.clear_status();
        self.inner.sel.set(0);
        self.inner.top.set(0);
        self.rebuild_view();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 並べ方を切り替える（同じなら何もしない）。キャプチャ中なら中断する。
    fn set_view(&self, by_key: bool) {
        let mode = if by_key { KeyView::ByKey } else { KeyView::ByCommand };
        if self.inner.view_mode.get() == mode {
            return;
        }
        self.inner.view_mode.set(mode);
        self.inner.capturing.set(false);
        self.inner.sel.set(0);
        self.inner.top.set(0);
        self.toggle[0].select(!by_key);
        self.toggle[1].select(by_key);
        self.relabel_buttons();
        self.update_hint();
        self.rebuild_view();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 機能順で選択行の式（キー順・範囲外は `None`）。
    fn selected_expr(&self) -> Option<String> {
        if self.inner.view_mode.get() != KeyView::ByCommand {
            return None;
        }
        let ri = *self.inner.view.borrow().get(self.inner.sel.get())?;
        self.inner.rows.borrow().get(ri).map(|r| r.expr.clone())
    }

    /// キー順で選択中の chord（機能順では `None`）。
    fn selected_chord(&self) -> Option<String> {
        if self.inner.view_mode.get() != KeyView::ByKey {
            return None;
        }
        let ri = *self.inner.view.borrow().get(self.inner.sel.get())?;
        self.inner.key_rows.borrow().get(ri).map(|r| r.chord.clone())
    }

    /// 機能順で選択行のバインド（式・キー）。キー順・範囲外は `None`。
    fn selected_bind(&self) -> Option<(String, Option<String>)> {
        if self.inner.view_mode.get() != KeyView::ByCommand {
            return None;
        }
        let ri = *self.inner.view.borrow().get(self.inner.sel.get())?;
        let rows = self.inner.rows.borrow();
        let r = rows.get(ri)?;
        Some((r.expr.clone(), r.chord.clone()))
    }

    /// 機能順リストの表示行（ジャンル見出しをデータ行の間に挟む）。`top` はこの単位で進む。
    /// キー順・ピックモードは見出しを挟まず、データ行だけを順に並べる。
    fn display_lines(&self) -> Vec<DisplayLine> {
        let view = self.inner.view.borrow();
        if self.inner.picking.borrow().is_some()
            || self.inner.view_mode.get() != KeyView::ByCommand
        {
            return (0..view.len()).map(DisplayLine::Row).collect();
        }
        let rows = self.inner.rows.borrow();
        let mut out = Vec::with_capacity(view.len() + 8);
        let mut prev: Option<String> = None;
        for (vp, &ri) in view.iter().enumerate() {
            let g = self.row_genre(&rows[ri]).1;
            if prev.as_deref() != Some(g.as_str()) {
                out.push(DisplayLine::Header(g.clone()));
                prev = Some(g);
            }
            out.push(DisplayLine::Row(vp));
        }
        out
    }

    /// 選択中のデータ行が、表示行（見出し込み）の何番目に来るか。
    fn sel_display_index(&self) -> usize {
        let sel = self.inner.sel.get();
        self.display_lines()
            .iter()
            .position(|d| matches!(d, DisplayLine::Row(vp) if *vp == sel))
            .unwrap_or(0)
    }

    /// クリック x がキー順・指定表示行のどの機能に当たるか（機能名カラムを実測）。描画と同じ
    /// 表示名（Script ラベル反映）で測る。
    fn label_hit(&self, row_di: usize, x: i32) -> Option<usize> {
        if self.inner.view_mode.get() != KeyView::ByKey {
            return None;
        }
        let ri = *self.inner.view.borrow().get(row_di)?;
        let values: Vec<String> = self.inner.key_rows.borrow().get(ri)?.values.clone();
        if values.is_empty() {
            return None;
        }
        let dc = self.hwnd().GetDC().ok()?;
        let font = w::HFONT::GetStockObject(co::STOCK_FONT::DEFAULT_GUI).ok()?;
        let _sel = dc.SelectObject(&font).ok()?;
        let mut cx = gui::dpi_x(200);
        let sep = dc.GetTextExtentPoint32(", ").map(|z| z.cx).unwrap_or(0);
        for (li, v) in values.iter().enumerate() {
            let name = self.value_label(v);
            let tw = dc.GetTextExtentPoint32(&name).map(|z| z.cx).unwrap_or(0);
            if x >= cx && x < cx + tw {
                return Some(li);
            }
            cx += tw + sep;
        }
        None
    }

    /// キー順で選択行の li 番目の機能を「別機能へ差し替える」ピックモードへ入る（インライン）。
    fn enter_pick(&self, li: usize) {
        if self.inner.view_mode.get() != KeyView::ByKey
            || self.inner.capturing.get()
            || self.inner.picking.borrow().is_some()
        {
            return;
        }
        let Some(&ri) = self.inner.view.borrow().get(self.inner.sel.get()) else {
            return;
        };
        let chord = match self.inner.key_rows.borrow().get(ri) {
            Some(r) => r.chord.clone(),
            None => return,
        };
        // 空キー定義（pending）なら old_value は空＝置換でなく新規割り当て。
        let old_value = {
            let draft = self.inner.draft.borrow();
            let nonempty: Vec<String> = draft
                .get(&chord)
                .map(|vs| vs.iter().filter(|v| !v.trim().is_empty()).cloned().collect())
                .unwrap_or_default();
            nonempty.get(li).cloned().unwrap_or_default()
        };
        let old_label = if old_value.is_empty() {
            "－".to_string()
        } else {
            label_display(&row_token(&old_value))
        };
        let ctx = self.inner.category.context();
        // ジャンル順に固める（同ジャンル内は元の列挙順）。見出しは描画時に境界で出す。
        let mut pick: Vec<Command> = Command::all().filter(|c| c.available_in(ctx)).collect();
        pick.sort_by_key(|c| command_genre(*c).0);
        *self.inner.pick_rows.borrow_mut() = pick;
        *self.inner.picking.borrow_mut() = Some(PickState {
            chord: chord.clone(),
            old_value,
        });
        self.inner.sel.set(0);
        self.inner.top.set(0);
        *self.inner.query.borrow_mut() = String::new();
        let _ = self.search.set_text("");
        *self.inner.status.borrow_mut() = format!(
            "{} に割り当てる機能を選択（{} を置換・右クリック/Escで中止）",
            chord, old_label
        );
        self.update_hint();
        self.rebuild_view();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// ピックモードで選択中の機能を確定し、対象キーの定義を差し替える。
    fn commit_pick(&self) {
        let Some(pick) = self.inner.picking.borrow_mut().take() else {
            return;
        };
        let new_cmd = self
            .inner
            .view
            .borrow()
            .get(self.inner.sel.get())
            .and_then(|&i| self.inner.pick_rows.borrow().get(i).copied());
        if let Some(cmd) = new_cmd {
            let new_val = format!("{}()", cmd.as_token());
            {
                let mut draft = self.inner.draft.borrow_mut();
                let vals = draft.entry(pick.chord.clone()).or_default();
                // 置換対象（old_value）があれば外す。空キー定義なら新規割り当て。
                if !pick.old_value.is_empty() {
                    vals.retain(|v| *v != pick.old_value);
                }
                if !vals.iter().any(|v| matches!(Call::parse(v), Call::Builtin { command, .. } if command == cmd)) {
                    vals.push(new_val);
                }
            }
            // 機能が付いたので pending（空キー定義）からは外す。
            self.inner.pending.borrow_mut().retain(|c| *c != pick.chord);
            *self.inner.status.borrow_mut() =
                format!("{} を {} に割り当てました", pick.chord, cmd.display_name());
        }
        self.exit_pick_common();
    }

    /// ピックモードを中止して元のキー一覧へ戻る。
    fn cancel_pick(&self) {
        if self.inner.picking.borrow_mut().take().is_none() {
            return;
        }
        *self.inner.status.borrow_mut() = "中止しました".to_string();
        self.exit_pick_common();
    }

    /// ピック解除の後始末（検索クリア・選択リセット・再構築・再描画）。
    fn exit_pick_common(&self) {
        self.inner.pick_rows.borrow_mut().clear();
        *self.inner.query.borrow_mut() = String::new();
        let _ = self.search.set_text("");
        self.inner.sel.set(0);
        self.inner.top.set(0);
        self.update_hint();
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 選択行のキーを別キーへ移し替えるキャプチャを始める（次の打鍵で旧キー→新キー）。未割当行は不可。
    fn begin_remap(&self) {
        if self.inner.picking.borrow().is_some() {
            return;
        }
        if !matches!(self.selected_bind(), Some((_, Some(_)))) {
            return;
        }
        self.inner.capturing.set(true);
        self.inner.capturing_remap.set(true);
        *self.inner.status.borrow_mut() = "新しいキーを押してください（右クリックで中止）".to_string();
        self.hwnd().SetFocus();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 選択行のキーを、その式のまま新しいキーへ移し替える（旧キーから当該式を外す）。
    fn remap(&self, new: KeyChord) {
        let Some((expr, Some(old))) = self.selected_bind() else {
            return;
        };
        let Some(new_tok) = new.to_token() else {
            *self.inner.status.borrow_mut() = "未対応のキーです".to_string();
            let _ = self.hwnd().InvalidateRect(None, false);
            return;
        };
        if new_tok == old {
            return;
        }
        {
            let mut draft = self.inner.draft.borrow_mut();
            if let Some(vals) = draft.get_mut(&old) {
                vals.retain(|v| *v != expr);
            }
            let e = draft.entry(new_tok.clone()).or_default();
            e.retain(|v| !v.trim().is_empty());
            if !e.contains(&expr) {
                e.push(expr.clone());
            }
        }
        *self.inner.status.borrow_mut() =
            format!("{} を {} から {} に変更しました", self.value_label(&expr), old, new_tok);
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// `expr`（機能欄の式）を chord へ割り当てる。**既存の割り当ては消さず追記する**＝同じ
    /// chord に別の式があれば衝突になる（マークして OK/適用で解決を促す）。
    fn assign(&self, expr: String, chord: KeyChord) {
        let Some(tok) = chord.to_token() else {
            *self.inner.status.borrow_mut() = "未対応のキーです".to_string();
            return;
        };
        let label = self.value_label(&expr);
        let conflict = {
            let mut draft = self.inner.draft.borrow_mut();
            let e = draft.entry(tok.clone()).or_default();
            // 空 unbind マーカーは取り除く（今バインドし直すので）。
            e.retain(|v| !v.trim().is_empty());
            // 同じ式が既にこの chord にあるなら追記しない。
            if !e.contains(&expr) {
                e.push(expr);
            }
            e.len() > 1
        };
        *self.inner.status.borrow_mut() = if conflict {
            format!("{} に {} を割り当て（このキーは衝突しています）", label, tok)
        } else {
            format!("{} に {} を割り当てました", label, tok)
        };
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 選択行の割り当てを解除する（既定キーは空 Vec で打ち消す＝差分保存で永続）。
    /// 機能順＝サブ選択中の1キーからその機能を外す／キー順＝選択中のその1キー（定義を丸ごと）。
    fn unbind_selected(&self) {
        if self.inner.picking.borrow().is_some() {
            return;
        }
        let status = match self.inner.view_mode.get() {
            KeyView::ByCommand => {
                let Some((expr, chord)) = self.selected_bind() else {
                    return;
                };
                match chord {
                    // 選択行のキーからその式だけ取り除く（同キーの他機能＝衝突分は残す）。
                    Some(chord) => {
                        if let Some(vals) = self.inner.draft.borrow_mut().get_mut(&chord) {
                            vals.retain(|v| *v != expr);
                        }
                        format!("{} から {} を解除しました", chord, self.value_label(&expr))
                    }
                    // 未割当行：「式を編集」で作った式なら消す。素の bare 行は消せない。
                    None => {
                        if self.remove_pending(&expr) {
                            format!("{} の未割当の定義を削除しました", self.value_label(&expr))
                        } else {
                            *self.inner.status.borrow_mut() = "割り当てがありません".to_string();
                            return;
                        }
                    }
                }
            }
            KeyView::ByKey => {
                let Some(chord) = self.selected_chord() else {
                    return;
                };
                // そのキー定義を丸ごと外す＝空 Vec（unbind マーカー）にする。pending なら消すだけ。
                let was_pending = self.inner.pending.borrow().contains(&chord);
                self.inner.pending.borrow_mut().retain(|c| *c != chord);
                if !was_pending {
                    self.inner.draft.borrow_mut().insert(chord.clone(), Vec::new());
                }
                format!("{} の割り当てを解除しました", chord)
            }
        };
        *self.inner.status.borrow_mut() = status;
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// このページを既定キーマップへ戻す。
    fn reset(&self) {
        if self.inner.picking.borrow().is_some() {
            return;
        }
        let def = self.inner.category.default_map();
        let draft: BTreeMap<String, Vec<String>> = def
            .into_iter()
            .map(|(chord, val)| {
                if val.trim().is_empty() {
                    (chord, Vec::new())
                } else {
                    (chord, vec![val])
                }
            })
            .collect();
        *self.inner.draft.borrow_mut() = draft;
        self.inner.pending.borrow_mut().clear();
        *self.inner.status.borrow_mut() = "既定に戻しました".to_string();
        self.inner.capturing.set(false);
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// キャプチャ開始（次の打鍵を選択行へ割り当てる）。リストへフォーカスを移す。
    fn begin_capture(&self) {
        if self.inner.picking.borrow().is_some() || self.selected_expr().is_none() {
            return;
        }
        self.inner.capturing.set(true);
        *self.inner.status.borrow_mut() = "キーを押してください（右クリックで中止）".to_string();
        self.hwnd().SetFocus();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 「式を編集」：選択中の行の機能欄の式を、現在の式を prefill したモーダルで編集させ、`apply_expr`
    /// で行へ反映する。組込呼び出しの引数付け・スクリプトの呼び替え・コード（複文）まで全部ここで扱う。
    fn edit_expr_row(&self) {
        if self.inner.picking.borrow().is_some() || self.inner.capturing.get() {
            return;
        }
        let Some((expr, _)) = self.selected_bind() else {
            return;
        };
        // 補完メンバに、登録スクリプト関数の 1 行説明を添える（組込はメタデータから引かれる）。
        let members = crate::dialog::completion_members(&self.inner.members, |name| {
            self.inner.script_meta.get(name).and_then(|sc| sc.summary.clone())
        });
        let result = crate::dialog::code_box(
            &self.list,
            "機能欄の式を編集（組込はそのまま呼べる・r. でホスト API・複文可）",
            &expr,
            &members,
        );
        // 子コントロールを親にしたモーダルの後始末で list の無効化やフォーカス喪失が残ることが
        // あるので、OK/キャンセルに依らず有効化＋フォーカスを戻す（戻さないとホイールが効かない）。
        self.hwnd().EnableWindow(true);
        self.hwnd().SetFocus();
        let Some(new_expr) = result else {
            return;
        };
        self.apply_expr(&new_expr);
    }

    /// 選択中の行の式を `new_expr` へ反映する。バインド済み行はその場でそのキーの式を差し替え、
    /// 未割当行は新しい式を未割当（－）行として出し、通常のキャプチャで割り当てる。「式を編集」
    /// モーダル OK と debug の両方から使う。空・無変更は何もしない。
    fn apply_expr(&self, new_expr: &str) {
        let Some((old_expr, chord)) = self.selected_bind() else {
            return;
        };
        let new_expr = new_expr.trim().to_string();
        if new_expr.is_empty() || new_expr == old_expr {
            return;
        }
        match chord {
            // バインド済み＝そのキーの式を新しい式へ差し替える。
            Some(ch) => {
                let mut draft = self.inner.draft.borrow_mut();
                if let Some(vals) = draft.get_mut(&ch) {
                    for v in vals.iter_mut() {
                        if *v == old_expr {
                            *v = new_expr.clone();
                        }
                    }
                }
                drop(draft);
                *self.inner.status.borrow_mut() =
                    format!("{} の式を変更しました（{}）", self.value_label(&new_expr), ch);
            }
            // 未割当＝新しい式を未割当行として用意し、あとはキャプチャで割り当てる。素の bare 呼び出し
            // （組込/登録スクリプトの `name()`）は常に一覧へ出るので pending には積まない。
            None => {
                if !self.is_seeded_bare(&new_expr) {
                    let mut pe = self.inner.pending_exprs.borrow_mut();
                    if !pe.contains(&new_expr) {
                        pe.push(new_expr.clone());
                    }
                }
                *self.inner.status.borrow_mut() =
                    "式を追加しました。「キー定義を追加」で割り当ててください".to_string();
            }
        }
        self.inner.view_mode.set(KeyView::ByCommand);
        self.toggle[0].select(true);
        self.toggle[1].select(false);
        *self.inner.query.borrow_mut() = String::new();
        let _ = self.search.set_text("");
        self.relabel_buttons();
        self.update_hint();
        self.rebuild_rows();
        self.select_value_row(&new_expr);
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 式が、一覧へ常に出る素の bare 呼び出し（組込コマンド・登録スクリプトの `name()`）か。
    /// これに当たる式は pending へ積まずとも行が出るので、未割当の重複行を作らないための判定。
    fn is_seeded_bare(&self, expr: &str) -> bool {
        match script_call_name(expr) {
            Some(name) => {
                Command::from_token(&name).is_some() || self.inner.script_meta.contains_key(&name)
            }
            None => false,
        }
    }

    /// 未割当行の式を pending リストから取り除く。「式を編集」で作った未割当の式由来なら `true`。
    /// 素の bare 呼び出し（組込/登録スクリプト）は pending に無いので `false`（常に一覧へ出る）。
    fn remove_pending(&self, expr: &str) -> bool {
        let mut pe = self.inner.pending_exprs.borrow_mut();
        let n = pe.len();
        pe.retain(|v| v != expr);
        pe.len() != n
    }

    /// 機能順で `expr` の行を選択し、見える位置までスクロールする。
    fn select_value_row(&self, expr: &str) {
        let pos = {
            let view = self.inner.view.borrow();
            let rows = self.inner.rows.borrow();
            view.iter().position(|&ri| rows.get(ri).is_some_and(|r| r.expr == expr))
        };
        if let Some(p) = pos {
            self.inner.sel.set(p);
            self.ensure_visible();
        }
    }

    fn cancel_capture(&self) {
        self.inner.capturing.set(false);
        self.inner.capturing_remap.set(false);
        self.inner.capturing_newdef.set(false);
        *self.inner.status.borrow_mut() = "中止しました".to_string();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// キー順で「キー定義を追加」：次の打鍵で機能未割当の空キー定義（－）を作る。
    fn add_key_def(&self) {
        if self.inner.view_mode.get() != KeyView::ByKey
            || self.inner.picking.borrow().is_some()
            || self.inner.capturing.get()
        {
            return;
        }
        self.inner.capturing.set(true);
        self.inner.capturing_newdef.set(true);
        *self.inner.status.borrow_mut() =
            "追加するキーを押してください（右クリックで中止）".to_string();
        self.hwnd().SetFocus();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 空キー定義を作る（機能は後で割り当て）。既にバインド済みなら何もせずその行を選ぶ。
    fn finish_newdef(&self, chord: KeyChord) {
        let Some(tok) = chord.to_token() else {
            *self.inner.status.borrow_mut() = "未対応のキーです".to_string();
            let _ = self.hwnd().InvalidateRect(None, false);
            return;
        };
        let bound = self
            .inner
            .draft
            .borrow()
            .get(&tok)
            .is_some_and(|vs| vs.iter().any(|v| !v.trim().is_empty()));
        if bound {
            *self.inner.status.borrow_mut() = format!("{} は既に使われています", tok);
        } else {
            let mut pending = self.inner.pending.borrow_mut();
            if !pending.contains(&tok) {
                pending.push(tok.clone());
            }
            drop(pending);
            *self.inner.status.borrow_mut() =
                format!("空のキー定義 {} を追加（機能を割り当ててください）", tok);
        }
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 1 画面に収まる行数（最下部のステータス行 1 行を除いた本文領域）。
    fn visible_rows(&self) -> usize {
        let ch = self.hwnd().GetClientRect().map(|r| r.bottom - r.top).unwrap_or(0);
        let rh = self.inner.row_h.get().max(1);
        ((ch - rh) / rh).max(1) as usize
    }

    /// 縦スクロールバーを現在の行数・表示位置に合わせる。
    fn update_scrollbar(&self) {
        if self.hwnd().GetClientRect().is_err() {
            return;
        }
        let n = self.display_lines().len();
        let vis = self.visible_rows();
        let mut si = w::SCROLLINFO::default();
        si.fMask = co::SIF::RANGE | co::SIF::PAGE | co::SIF::POS;
        si.nMin = 0;
        si.nMax = (n as i32 - 1).max(0);
        si.nPage = vis as u32;
        si.nPos = self.inner.top.get() as i32;
        self.hwnd().SetScrollInfo(co::SBB::VERT, &si, true);
    }

    /// 表示先頭行を動かす（範囲内へクランプ・スクロールバーと再描画も更新）。選択は動かさない。
    fn scroll_to(&self, new_top: isize) {
        let n = self.display_lines().len();
        let vis = self.visible_rows();
        let max_top = n.saturating_sub(vis) as isize;
        let top = new_top.clamp(0, max_top) as usize;
        if top != self.inner.top.get() {
            self.inner.top.set(top);
            self.update_scrollbar();
            let _ = self.hwnd().InvalidateRect(None, false);
        }
    }

    /// 選択が見える位置までスクロールを調整する（表示行＝見出し込みで計算する）。
    fn ensure_visible(&self) {
        let di = self.sel_display_index();
        let vis = self.visible_rows();
        let mut top = self.inner.top.get();
        if di < top {
            top = di;
        } else if di >= top + vis {
            top = di + 1 - vis;
        }
        self.inner.top.set(top);
        self.update_scrollbar();
        self.update_edit_button();
    }

    /// 直近の操作結果メッセージ（「中止しました」等）を消す。次の操作で残骸を残さないため、
    /// 移動・選択・検索のたびに呼ぶ。キャプチャ中・ピック中の案内はライブなので消さない。
    fn clear_status(&self) {
        if self.inner.capturing.get() || self.inner.picking.borrow().is_some() {
            return;
        }
        if !self.inner.status.borrow().is_empty() {
            self.inner.status.borrow_mut().clear();
        }
    }

    fn move_sel(&self, dir: isize) {
        let n = self.inner.view.borrow().len() as isize;
        if n == 0 {
            return;
        }
        self.clear_status();
        let i = (self.inner.sel.get() as isize + dir).clamp(0, n - 1);
        self.inner.sel.set(i as usize);
        self.ensure_visible();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    fn setup_events(&self) {
        let this = self.clone();
        self.list.on().wm_get_dlg_code(move |_| {
            let flags = if this.inner.capturing.get() || this.inner.picking.borrow().is_some() {
                co::DLGC::WANTALLKEYS.raw()
            } else {
                co::DLGC::WANTARROWS.raw()
            };
            Ok(unsafe { co::DLGC::from_raw(flags) })
        });

        let this = self.clone();
        self.list.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.list.on().wm(unsafe { co::WM::from_raw(WM_PRINTCLIENT) }, move |p| {
            this.on_print(p.wparam);
            Ok(0)
        });

        let this = self.clone();
        self.list.on().wm_set_focus(move |_| {
            let _ = this.hwnd().InvalidateRect(None, false);
            Ok(())
        });
        let this = self.clone();
        self.list.on().wm_kill_focus(move |_| {
            let _ = this.hwnd().InvalidateRect(None, false);
            Ok(())
        });

        let this = self.clone();
        self.list.on().wm_l_button_down(move |p| {
            this.hwnd().SetFocus();
            // キャプチャ中の左クリックは無視（中止は右クリック・左は「決定」感を避ける）。
            if this.inner.capturing.get() {
                return Ok(());
            }
            let rh = this.inner.row_h.get().max(1);
            let di = this.inner.top.get() + (p.coords.y / rh) as usize;
            // 見出し行のクリックは無視（データ行だけ選べる）。
            if let Some(DisplayLine::Row(vp)) = this.display_lines().get(di).cloned() {
                this.clear_status();
                this.inner.sel.set(vp);
                this.ensure_visible();
                let _ = this.hwnd().InvalidateRect(None, false);
            }
            Ok(())
        });

        // ダブルクリック：機能順=キー有りは「変更」キャプチャ・キー無しは新規キャプチャ／
        // キー順=機能を機能ピッカーへ／ピック中=その機能で確定。
        let this = self.clone();
        self.list.on().wm_l_button_dbl_clk(move |p| {
            if this.inner.capturing.get() {
                return Ok(());
            }
            let rh = this.inner.row_h.get().max(1);
            let di = this.inner.top.get() + (p.coords.y / rh) as usize;
            let Some(DisplayLine::Row(vp)) = this.display_lines().get(di).cloned() else {
                return Ok(());
            };
            this.inner.sel.set(vp);
            if this.inner.picking.borrow().is_some() {
                this.commit_pick();
                return Ok(());
            }
            match this.inner.view_mode.get() {
                KeyView::ByCommand => match this.selected_bind() {
                    // キー有り＝そのキーを「変更」キャプチャへ。キー無し＝新規キャプチャへ
                    //（キー順で空キー定義「－」をダブルクリックするのと対称）。
                    Some((_, Some(_))) => {
                        this.ensure_visible();
                        this.begin_remap();
                        return Ok(());
                    }
                    Some((_, None)) => {
                        this.ensure_visible();
                        this.begin_capture();
                        return Ok(());
                    }
                    None => {}
                },
                KeyView::ByKey => {
                    // 機能ラベル上なら そのラベルを差替へ。空キー定義（－）行なら新規割り当てへ。
                    let li = this.label_hit(vp, p.coords.x).or_else(|| {
                        let empty = this
                            .inner
                            .view
                            .borrow()
                            .get(vp)
                            .and_then(|&ri| this.inner.key_rows.borrow().get(ri).map(|r| r.labels.is_empty()))
                            .unwrap_or(false);
                        empty.then_some(0)
                    });
                    if let Some(li) = li {
                        this.ensure_visible();
                        this.enter_pick(li);
                        return Ok(());
                    }
                }
            }
            this.ensure_visible();
            let _ = this.hwnd().InvalidateRect(None, false);
            Ok(())
        });

        // 右クリック：キャプチャ中／ピック中なら中止（左クリックは「決定」感があるので使わない）。
        let this = self.clone();
        self.list.on().wm_r_button_down(move |_| {
            if this.inner.capturing.get() {
                this.cancel_capture();
            } else if this.inner.picking.borrow().is_some() {
                this.cancel_pick();
            }
            Ok(())
        });

        // 縦スクロールバー：行単位で表示位置を動かす（選択は動かさない）。
        let this = self.clone();
        self.list.on().wm_v_scroll(move |p| {
            let cur = this.inner.top.get() as isize;
            let vis = this.visible_rows() as isize;
            let n = this.display_lines().len() as isize;
            let new = match p.request {
                co::SB_REQ::LINEUP => cur - 1,
                co::SB_REQ::LINEDOWN => cur + 1,
                co::SB_REQ::PAGEUP => cur - vis,
                co::SB_REQ::PAGEDOWN => cur + vis,
                co::SB_REQ::THUMBPOSITION | co::SB_REQ::THUMBTRACK => p.scroll_box_pos as isize,
                co::SB_REQ::TOP => 0,
                co::SB_REQ::BOTTOM => n,
                _ => cur,
            };
            this.scroll_to(new);
            Ok(())
        });

        // マウスホイール：3 行ずつスクロール（winsafe 0.0.27 は回転量が keys に入る）。
        let this = self.clone();
        self.list.on().wm_mouse_wheel(move |p| {
            let dist = p.keys.raw() as i16 as i32;
            let lines = (dist / 120) * 3;
            this.scroll_to(this.inner.top.get() as isize - lines as isize);
            Ok(())
        });

        let this = self.clone();
        self.list.on().wm_key_down(move |p| {
            this.on_key(p.vkey_code.raw());
            Ok(())
        });
    }

    /// キー入力処理。キャプチャ中は打鍵を chord 化して割り当て（中止はクリック＝ESC も
    /// 普通にキャプチャできる）、通常時は ↑↓ で行移動・←→ でキーのサブ選択。
    fn on_key(&self, vk: u16) {
        use rerics_core::vk as k;
        if self.inner.capturing.get() {
            // 修飾キー単体（Shift/Ctrl/Alt の VK）は確定打鍵としない（実キーが来るまで待つ）。
            if matches!(vk, 0x10..=0x12) {
                return;
            }
            let ctrl = w::GetAsyncKeyState(co::VK::CONTROL);
            let shift = w::GetAsyncKeyState(co::VK::SHIFT);
            let alt = w::GetAsyncKeyState(co::VK::MENU);
            let chord = KeyChord::new(vk, ctrl, shift, alt);
            self.inner.capturing.set(false);
            if self.inner.capturing_newdef.replace(false) {
                self.finish_newdef(chord);
            } else if self.inner.capturing_remap.replace(false) {
                self.remap(chord);
            } else if let Some((expr, _)) = self.selected_bind() {
                self.assign(expr, chord);
            }
            return;
        }
        let vis = self.visible_rows() as isize;
        // ピックモード：↑↓/PageUp/Down/Home/End で機能を選び、Enter で確定・Esc で中止。
        if self.inner.picking.borrow().is_some() {
            match vk {
                k::UP => self.move_sel(-1),
                k::DOWN => self.move_sel(1),
                k::PRIOR => self.move_sel(-vis),
                k::NEXT => self.move_sel(vis),
                k::HOME => self.move_sel(isize::MIN / 2),
                k::END => self.move_sel(isize::MAX / 2),
                k::RETURN => self.commit_pick(),
                k::ESCAPE => self.cancel_pick(),
                _ => {}
            }
            return;
        }
        if vk == k::UP {
            self.move_sel(-1);
        } else if vk == k::DOWN {
            self.move_sel(1);
        } else if vk == k::PRIOR {
            self.move_sel(-vis);
        } else if vk == k::NEXT {
            self.move_sel(vis);
        } else if vk == k::HOME {
            self.move_sel(isize::MIN / 2);
        } else if vk == k::END {
            self.move_sel(isize::MAX / 2);
        }
    }

    fn on_paint(&self) -> w::AnyResult<()> {
        let hdc = self.hwnd().BeginPaint()?;
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        let mem = hdc.CreateCompatibleDC()?;
        let bmp = hdc.CreateCompatibleBitmap(cw, ch)?;
        let _sel = mem.SelectObject(&*bmp)?;
        self.render(&mem, cw, ch)?;
        hdc.BitBlt(
            w::POINT { x: 0, y: 0 },
            w::SIZE { cx: cw, cy: ch },
            &mem,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    /// `WM_PRINTCLIENT`：与えられた DC へ直接描く（デバッグ制御サーバのスナップショット用）。
    fn on_print(&self, hdc_ptr: usize) {
        let hdc = unsafe { w::HDC::from_ptr(hdc_ptr as *mut std::ffi::c_void) };
        if let Ok(rc) = self.hwnd().GetClientRect() {
            let _ = self.render(&hdc, rc.right - rc.left, rc.bottom - rc.top);
        }
    }

    fn render(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let font = w::HFONT::GetStockObject(co::STOCK_FONT::DEFAULT_GUI)?;
        let _fsel = dc.SelectObject(&font)?;
        let fh = dc.GetTextMetrics().map(|tm| tm.tmHeight).unwrap_or(16);
        let row_h = (fh + gui::dpi_y(10)).max(gui::dpi_y(20));
        self.inner.row_h.set(row_h);
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;

        // ピックモード（機能ピッカー表示）中は背景色を変えて「別モード」を一目で分かるようにする。
        let picking = self.inner.picking.borrow().is_some();
        let fill = if picking {
            w::HBRUSH::GetSysColorBrush(co::COLOR::INFOBK)?
        } else {
            w::HBRUSH::GetSysColorBrush(co::COLOR::WINDOW)?
        };
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &fill)?;

        let text_col = w::GetSysColor(co::COLOR::WINDOWTEXT);
        let gray_col = w::GetSysColor(co::COLOR::GRAYTEXT);
        let hl_text = w::GetSysColor(co::COLOR::HIGHLIGHTTEXT);
        let hl_bg = w::HBRUSH::GetSysColorBrush(co::COLOR::HIGHLIGHT)?;
        // キャプチャ中の当該行・メッセージ行・ピッカー地に使うアイボリー。
        let ivory = w::HBRUSH::GetSysColorBrush(co::COLOR::INFOBK)?;

        let view = self.inner.view.borrow();
        let top = self.inner.top.get();
        let sel = self.inner.sel.get();
        let capturing = self.inner.capturing.get();
        let key_x = gui::dpi_x(8);
        // 最下部 1 行はステータス（操作結果・衝突メッセージ）に充てる。
        let body_h = (ch - row_h).max(row_h);
        let vis = (body_h / row_h).max(1) as usize;
        // 機能ピッカーは専用描画：左ガターにジャンル見出し（境界の行に淡色）、本体に機能名を並べる。
        if picking {
            let pr = self.inner.pick_rows.borrow();
            let name_x = gui::dpi_x(168);
            for vi in 0..vis {
                let di = top + vi;
                let Some(cmd) = view.get(di).and_then(|&i| pr.get(i)).copied() else {
                    break;
                };
                let y = vi as i32 * row_h;
                let ty = y + (row_h - fh) / 2;
                if di == sel {
                    dc.FillRect(w::RECT { left: 0, top: y, right: cw, bottom: y + row_h }, &hl_bg)?;
                }
                // ジャンルの先頭行（または可視範囲の最上行）にだけ見出しを出す。
                let genre = command_genre(cmd).1;
                let prev_genre = if vi > 0 {
                    view.get(di - 1).and_then(|&i| pr.get(i)).map(|c| command_genre(*c).1)
                } else {
                    None
                };
                if prev_genre != Some(genre) {
                    dc.SetTextColor(if di == sel { hl_text } else { gray_col })?;
                    dc.TextOut(key_x, ty, genre)?;
                }
                dc.SetTextColor(if di == sel { hl_text } else { text_col })?;
                dc.TextOut(name_x, ty, cmd.display_name())?;
            }
            let sep_y = ch - row_h;
            let sep_brush = w::HBRUSH::GetSysColorBrush(co::COLOR::BTNSHADOW)?;
            dc.FillRect(w::RECT { left: 0, top: sep_y, right: cw, bottom: sep_y + 1 }, &sep_brush)?;
            let status = self.inner.status.borrow();
            if !status.is_empty() {
                dc.SetTextColor(text_col)?;
                dc.TextOut(key_x, sep_y + (row_h - fh) / 2, &status)?;
            }
            return Ok(());
        }
        // 機能順は「見出し行＋3カラム（機能名｜実呼び出し｜キー）」、キー順は「キー｜機能群」を描く。
        // どちらも表示行（見出し込み）を `top` から `vis` 行ぶん辿る。キー列は狭いので、入力待ちの
        // 詳しい案内（右クリックで中止）は下部ステータス行に任せ、行内は短い目印だけ出す。
        let prompt = "← キー入力待ち";
        let display = self.display_lines();
        match self.inner.view_mode.get() {
            KeyView::ByCommand => {
                let name_x = gui::dpi_x(24);
                let call_x = gui::dpi_x(200);
                let keycol_x = gui::dpi_x(600);
                let band = w::HBRUSH::GetSysColorBrush(co::COLOR::BTNFACE)?;
                let rows = self.inner.rows.borrow();
                for vi in 0..vis {
                    let Some(line) = display.get(top + vi).cloned() else {
                        break;
                    };
                    let y = vi as i32 * row_h;
                    let ty = y + (row_h - fh) / 2;
                    match line {
                        DisplayLine::Header(g) => {
                            dc.FillRect(
                                w::RECT { left: 0, top: y, right: cw, bottom: y + row_h },
                                &band,
                            )?;
                            dc.SetTextColor(text_col)?;
                            dc.TextOut(key_x, ty, &g)?;
                        }
                        DisplayLine::Row(vp) => {
                            let Some(&ri) = view.get(vp) else { continue };
                            let r = &rows[ri];
                            let selected = vp == sel;
                            if selected && capturing {
                                dc.FillRect(
                                    w::RECT { left: 0, top: y, right: cw, bottom: y + row_h },
                                    &ivory,
                                )?;
                                dc.SetTextColor(text_col)?;
                            } else if selected {
                                dc.FillRect(
                                    w::RECT { left: 0, top: y, right: cw, bottom: y + row_h },
                                    &hl_bg,
                                )?;
                                dc.SetTextColor(hl_text)?;
                            } else {
                                dc.SetTextColor(text_col)?;
                            }
                            dc.TextOut(name_x, ty, &self.value_label(&r.expr))?;
                            dc.TextOut(call_x, ty, &call_display(&r.expr))?;
                            if selected && capturing {
                                dc.TextOut(keycol_x, ty, prompt)?;
                            } else if let Some(c) = &r.chord {
                                let mut k = c.clone();
                                if r.conflicted {
                                    k.push_str(" ⚠");
                                }
                                dc.TextOut(keycol_x, ty, &k)?;
                            } else {
                                if !selected {
                                    dc.SetTextColor(gray_col)?;
                                }
                                dc.TextOut(keycol_x, ty, "—")?;
                            }
                        }
                    }
                }
            }
            KeyView::ByKey => {
                // 機能順と対称に「キー｜機能名｜実呼び出し」の 3 カラム。機能名は Script ラベルを
                // 反映し、実呼び出しはラッパを剥がした中身（スクリプト名・コード・引数つきトークン）。
                let chord_x = gui::dpi_x(8);
                let name_x = gui::dpi_x(200);
                let call_x = gui::dpi_x(420);
                let krows = self.inner.key_rows.borrow();
                for vi in 0..vis {
                    let Some(DisplayLine::Row(vp)) = display.get(top + vi).cloned() else {
                        break;
                    };
                    let Some(&ri) = view.get(vp) else { continue };
                    let r = &krows[ri];
                    let y = vi as i32 * row_h;
                    let ty = y + (row_h - fh) / 2;
                    let selected = vp == sel;
                    if selected && capturing {
                        dc.FillRect(w::RECT { left: 0, top: y, right: cw, bottom: y + row_h }, &ivory)?;
                        dc.SetTextColor(text_col)?;
                    } else if selected {
                        dc.FillRect(w::RECT { left: 0, top: y, right: cw, bottom: y + row_h }, &hl_bg)?;
                        dc.SetTextColor(hl_text)?;
                    } else {
                        dc.SetTextColor(text_col)?;
                    }
                    dc.TextOut(chord_x, ty, &r.chord)?;
                    if selected && capturing {
                        dc.TextOut(name_x, ty, prompt)?;
                    } else if r.values.is_empty() {
                        if !selected {
                            dc.SetTextColor(gray_col)?;
                        }
                        dc.TextOut(name_x, ty, "－")?;
                    } else {
                        let mut names =
                            r.values.iter().map(|v| self.value_label(v)).collect::<Vec<_>>().join(", ");
                        if r.values.len() > 1 {
                            names.push_str(" ⚠");
                        }
                        let calls =
                            r.values.iter().map(|v| call_display(v)).collect::<Vec<_>>().join(", ");
                        dc.TextOut(name_x, ty, &names)?;
                        dc.TextOut(call_x, ty, &calls)?;
                    }
                }
            }
        }
        // 最下部のステータス行（薄い区切り線＋直近メッセージ）。メッセージがある時だけ
        // 行をアイボリーで塗って目立たせる（空のときは地のまま）。
        let sep_y = ch - row_h;
        let status = self.inner.status.borrow();
        if !status.is_empty() {
            dc.FillRect(w::RECT { left: 0, top: sep_y, right: cw, bottom: ch }, &ivory)?;
        }
        let sep_brush = w::HBRUSH::GetSysColorBrush(co::COLOR::BTNSHADOW)?;
        dc.FillRect(w::RECT { left: 0, top: sep_y, right: cw, bottom: sep_y + 1 }, &sep_brush)?;
        if !status.is_empty() {
            dc.SetTextColor(text_col)?;
            dc.TextOut(key_x, sep_y + (row_h - fh) / 2, &status)?;
        }
        Ok(())
    }

    /// window 生成後の初期化（ボタン整形・スクロール調整・再描画）。
    pub(crate) fn populate(&self) {
        self.relabel_buttons();
        self.update_hint();
        self.ensure_visible();
        let _ = self.hwnd().InvalidateRect(None, false);
    }
}
