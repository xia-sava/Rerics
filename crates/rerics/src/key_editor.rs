//! 設定ダイアログの「キー」ページ＝対話的キーマップエディタ。自前描画リストで機能順／キー順に
//! 並べ、検索・実打鍵キャプチャ割当・重複検出・個別削除/リマップ・ジャンル分けを行なう。
//! `config.keybinds` を下書き編集し、設定ダイアログの OK／適用で確定する（`settings_dialog::show` が生成）。

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use rerics_core::{Command, CommandContext, Invocation, KeyChord, KeyMap};
use winsafe::{self as w, co, gui, prelude::*};

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

/// 機能順の 1 行＝1 コマンドと、それに割り当たっている chord 群。
struct KeyRow {
    command: Command,
    chords: Vec<ChordRef>,
}

/// 機能順の行に並ぶ 1 つの chord 参照。`conflicted`＝同じ chord を他機能も定義している（衝突）。
struct ChordRef {
    token: String,
    conflicted: bool,
}

/// キー順の 1 行＝1 chord と、それを定義している全機能のラベル（2 つ以上＝衝突）。
struct KeyChordRow {
    chord: String,
    labels: Vec<String>,
}

/// 割り当て値の表示ラベル。既知コマンドは正規トークン名、未知（`Func_*` 等）は生値のまま。
fn binding_label(value: &str) -> String {
    Invocation::parse(value)
        .map(|i| i.command.as_token().to_string())
        .unwrap_or_else(|| value.trim().to_string())
}

/// キー順の機能ラベル（トークン）を画面表示用の日本語名へ変換する。未知トークンはそのまま。
fn label_display(label: &str) -> String {
    Command::from_token(label)
        .map(|c| c.display_name().to_string())
        .unwrap_or_else(|| label.to_string())
}

/// 機能ピッカーの並び・見出し用ジャンル。`(並び順, 見出し)` を返す。並びは機能ピッカーを
/// ジャンルごとに固まらせるためのもので、設定 UI 専用の括り（コアの文脈分けとは別軸）。
fn command_genre(cmd: Command) -> (u8, &'static str) {
    use Command::*;
    match cmd {
        CursorUp | CursorDown | CursorTop | CursorEnd | CursorPageUp | CursorPageDown
        | SetCursorPosition | CursorOpposite => (0, "カーソル移動"),
        EnterDir | ToParent | ToRoot | HistoryBack | HistoryForward | PathHistoryDialog
        | ChangeDirectory | ChangeDirectoryDialog | ChangeDrive | ChangeDriveDialog | JumpDialog
        | RegisterPath | IncrementalSearchDialog | NextDrive | PreviousDrive => (1, "移動・ナビゲーション"),
        MarkToggle | SelectAll | ClearAll | ReverseAll | SelectAllFile | ReverseAllFile
        | SelectFile | SelectMask | PathMask => (2, "選択"),
        Reload | Refresh | View | ViewFile | DirectoryInformation | SortByName | SortByExtension
        | SortBySize | SortByDate | Sort | SortDialog | SortReverseToggle => (3, "表示・並べ替え"),
        PageNext | PagePrevious | NewTab | CloseTab => (4, "タブ"),
        FocusLeft | FocusRight | SwapPath | OppositeToCurrent | CurrentToOpposite | MaximizeLeft
        | MaximizeRight | MaximizeLeftForce | MaximizeRightForce | MaximizeCurrent | BorderLeft
        | BorderRight | BorderReset => (5, "ペイン"),
        MakeDirectory | Copy | Move | Rename | RenameSequenceDialog | Delete | SendToRecycled
        | CreateShortcut | ClipCopy | ClipCut | ClipPaste | CreateFile | Edit | PropertyDialog
        | Compress | Extract => (6, "ファイル操作"),
        OpenTaskManager | OpenSettings | KeyBindsDialog | CopyLog | ClearLog | Nop
        | End | Restart | Quit | MaximizeWindow | MinimizeWindow => (7, "アプリ・その他"),
        Script | Eval => (14, "スクリプト"),
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
    /// 編集中の下書き＝chord → 割り当て値（生の invocation 文字列）のリスト。空 Vec＝明示 unbind。
    /// **重複（1 つの chord に複数機能）を許す**＝これが衝突状態。未知バインド（`Func_*` 等）も
    /// 生値のまま保持し、反映時に消さない。OK/適用の検証を通った時だけ `config.keybinds` へ書き戻す。
    draft: RefCell<BTreeMap<String, Vec<String>>>,
    rows: RefCell<Vec<KeyRow>>,
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
    /// 機能順で選択行のキー群のうち、サブ選択中のキー index（個別削除・個別変更の対象）。
    sub: Cell<usize>,
    /// 表示先頭行（スクロール）。
    top: Cell<usize>,
    row_h: Cell<i32>,
    /// 次の打鍵を選択行のコマンドへ割り当てる待ち状態。
    capturing: Cell<bool>,
    /// キャプチャがサブ選択キーの「変更（リマップ）」か（`true`）、新規追加か（`false`）。
    capturing_remap: Cell<bool>,
    /// キャプチャがキー順の「空キー定義の新規作成」か（`true`）。打鍵を pending へ入れる。
    capturing_newdef: Cell<bool>,
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
    /// 上部ヒント（モードで文面を差し替える）。
    hint: gui::Label,
    inner: Rc<KeyEditorInner>,
}

impl KeyEditor {
    pub(crate) fn new(parent: &gui::WindowControl, shared: &Rc<Shared>, category: KeyCategory) -> Self {
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
                size: gui::dpi(744, 414),
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
        // 左グループ＝選択対象に効く 3 ボタン（初期は機能順ラベル）。
        let btn_a = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーを追加(&K)",
                position: gui::dpi(16, 496),
                width: gui::dpi_x(150),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        let btn_b = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーを変更(&C)",
                position: gui::dpi(174, 496),
                width: gui::dpi_x(150),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        let btn_c = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーを削除(&D)",
                position: gui::dpi(332, 496),
                width: gui::dpi_x(150),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        // 右に分離＝ページ全域に効く操作。
        let reset = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "このページを既定に戻す(&R)",
                position: gui::dpi(580, 496),
                width: gui::dpi_x(180),
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
            hint,
            inner: Rc::new(KeyEditorInner {
                shared: shared.clone(),
                category,
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
                sub: Cell::new(0),
                top: Cell::new(0),
                row_h: Cell::new(gui::dpi_y(22)),
                capturing: Cell::new(false),
                capturing_remap: Cell::new(false),
                capturing_newdef: Cell::new(false),
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
                            (
                                r.command.as_token().to_string(),
                                r.chords.iter().map(|c| c.token.clone()).collect(),
                            )
                        })
                        .collect()
                };
                KeyEditorState {
                    rows,
                    selected: this.inner.sel.get(),
                    sub: this.inner.sub.get(),
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
                    this.inner.sub.set(0);
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
                        view.iter().position(|&ri| rows[ri].command == cmd)
                    }
                };
                if let Some(di) = pos {
                    this.inner.sel.set(di);
                }
                this.assign(cmd, ch);
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
        let select_chord = {
            let this = self.clone();
            Box::new(move |index: usize| {
                if index < this.sel_chord_count() {
                    this.inner.sub.set(index);
                    let _ = this.hwnd().InvalidateRect(None, false);
                }
            }) as Box<dyn Fn(usize)>
        };
        let rebind = {
            let this = self.clone();
            Box::new(move |chord: &str| {
                let ch = KeyChord::parse(chord)
                    .ok_or_else(|| format!("unknown chord: {chord}"))?;
                this.remap_sub(ch);
                Ok(())
            }) as Box<dyn Fn(&str) -> Result<(), String>>
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
                select_chord,
                rebind,
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
                    .map(|v| binding_label(v))
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

    /// 下書きから機能順 `rows`・キー順 `key_rows` を組み直す（空 Vec＝unbind は除く）。
    fn rebuild_rows(&self) {
        let mut by_cmd: HashMap<Command, Vec<ChordRef>> = HashMap::new();
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
                let labels: Vec<String> = nonempty.iter().map(|v| binding_label(v)).collect();
                key_rows.push(KeyChordRow { chord: chord.clone(), labels });
                for v in &nonempty {
                    if let Some(inv) = Invocation::parse(v) {
                        by_cmd
                            .entry(inv.command)
                            .or_default()
                            .push(ChordRef { token: chord.clone(), conflicted });
                    }
                }
            }
        }
        // 機能未割当の空キー定義（pending）をキー順に並べる（－表示）。既にバインドされたものは除く。
        {
            let bound: std::collections::HashSet<String> =
                key_rows.iter().map(|r| r.chord.clone()).collect();
            for chord in self.inner.pending.borrow().iter() {
                if !bound.contains(chord) {
                    key_rows.push(KeyChordRow { chord: chord.clone(), labels: Vec::new() });
                }
            }
        }
        let ctx = self.inner.category.context();
        let rows: Vec<KeyRow> = Command::all()
            .filter(|c| c.available_in(ctx))
            .map(|command| {
                let mut chords = by_cmd.remove(&command).unwrap_or_default();
                chords.sort_by(|a, b| a.token.cmp(&b.token));
                KeyRow { command, chords }
            })
            .collect();
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
                        || r.command.as_token().to_lowercase().contains(&q)
                        || r.command.display_name().to_lowercase().contains(&q)
                        || r.chords.iter().any(|c| c.token.to_lowercase().contains(&q))
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
                        || r.labels.iter().any(|l| label_display(l).to_lowercase().contains(&q))
                })
                .map(|(i, _)| i)
                .collect(),
        };
        // 機能順はジャンルごとに固める（同ジャンル内は元の並び＝enum 順）。見出しは描画時に境界で出す。
        if self.inner.view_mode.get() == KeyView::ByCommand {
            let rows = self.inner.rows.borrow();
            view.sort_by_key(|&i| command_genre(rows[i].command).0);
        }
        let n = view.len();
        *self.inner.view.borrow_mut() = view;
        if n == 0 {
            self.inner.sel.set(0);
            self.inner.top.set(0);
        } else if self.inner.sel.get() >= n {
            self.inner.sel.set(n - 1);
        }
        let cc = self.sel_chord_count();
        if self.inner.sub.get() >= cc {
            self.inner.sub.set(cc.saturating_sub(1));
        }
        self.update_scrollbar();
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

    /// 選択行のコマンド（機能順＝その行の機能／キー順＝その chord を定義する先頭の既知機能）。
    fn selected_command(&self) -> Option<Command> {
        let ri = *self.inner.view.borrow().get(self.inner.sel.get())?;
        match self.inner.view_mode.get() {
            KeyView::ByCommand => self.inner.rows.borrow().get(ri).map(|r| r.command),
            KeyView::ByKey => self
                .inner
                .key_rows
                .borrow()
                .get(ri)
                .and_then(|r| r.labels.iter().find_map(|l| Command::from_token(l))),
        }
    }

    /// キー順で選択中の chord（機能順では `None`）。
    fn selected_chord(&self) -> Option<String> {
        if self.inner.view_mode.get() != KeyView::ByKey {
            return None;
        }
        let ri = *self.inner.view.borrow().get(self.inner.sel.get())?;
        self.inner.key_rows.borrow().get(ri).map(|r| r.chord.clone())
    }

    /// 機能順で選択行のキー数。
    fn sel_chord_count(&self) -> usize {
        if self.inner.view_mode.get() != KeyView::ByCommand {
            return 0;
        }
        let Some(&ri) = self.inner.view.borrow().get(self.inner.sel.get()) else {
            return 0;
        };
        self.inner.rows.borrow().get(ri).map(|r| r.chords.len()).unwrap_or(0)
    }

    /// 機能順でサブ選択中のキー（範囲外・キー順では `None`）。
    fn sub_chord(&self) -> Option<String> {
        if self.inner.view_mode.get() != KeyView::ByCommand {
            return None;
        }
        let ri = *self.inner.view.borrow().get(self.inner.sel.get())?;
        let rows = self.inner.rows.borrow();
        rows.get(ri)?.chords.get(self.inner.sub.get()).map(|c| c.token.clone())
    }

    /// 機能順でサブ選択を左右に動かす。
    fn move_sub(&self, dir: isize) {
        let n = self.sel_chord_count() as isize;
        if n <= 1 {
            return;
        }
        self.clear_status();
        let i = (self.inner.sub.get() as isize + dir).clamp(0, n - 1);
        self.inner.sub.set(i as usize);
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// クリック x が機能順・指定表示行のどのキーに当たるか（行内のキーを実測して判定）。
    fn chord_hit(&self, row_di: usize, x: i32) -> Option<usize> {
        if self.inner.view_mode.get() != KeyView::ByCommand {
            return None;
        }
        let ri = *self.inner.view.borrow().get(row_di)?;
        let chords: Vec<(String, bool)> = self
            .inner
            .rows
            .borrow()
            .get(ri)?
            .chords
            .iter()
            .map(|c| (c.token.clone(), c.conflicted))
            .collect();
        if chords.is_empty() {
            return None;
        }
        let dc = self.hwnd().GetDC().ok()?;
        let font = w::HFONT::GetStockObject(co::STOCK_FONT::DEFAULT_GUI).ok()?;
        let _sel = dc.SelectObject(&font).ok()?;
        // 機能順のキー列開始 x（render の chord_x と一致させる＝見出し列ぶん右）。
        let mut cx = gui::dpi_x(388);
        let sep = dc.GetTextExtentPoint32(", ").map(|z| z.cx).unwrap_or(0);
        for (ci, (tok, conflicted)) in chords.iter().enumerate() {
            let mut label = tok.clone();
            if *conflicted {
                label.push_str(" ⚠");
            }
            let tw = dc.GetTextExtentPoint32(&label).map(|z| z.cx).unwrap_or(0);
            if x >= cx && x < cx + tw {
                return Some(ci);
            }
            cx += tw + sep;
        }
        None
    }

    /// クリック x がキー順・指定表示行のどの機能ラベルに当たるか（右カラムを実測）。
    fn label_hit(&self, row_di: usize, x: i32) -> Option<usize> {
        if self.inner.view_mode.get() != KeyView::ByKey {
            return None;
        }
        let ri = *self.inner.view.borrow().get(row_di)?;
        let labels: Vec<String> = self.inner.key_rows.borrow().get(ri)?.labels.clone();
        if labels.is_empty() {
            return None;
        }
        let dc = self.hwnd().GetDC().ok()?;
        let font = w::HFONT::GetStockObject(co::STOCK_FONT::DEFAULT_GUI).ok()?;
        let _sel = dc.SelectObject(&font).ok()?;
        let mut cx = gui::dpi_x(260);
        let sep = dc.GetTextExtentPoint32(", ").map(|z| z.cx).unwrap_or(0);
        for (li, lab) in labels.iter().enumerate() {
            let tw = dc.GetTextExtentPoint32(lab).map(|z| z.cx).unwrap_or(0);
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
            label_display(&binding_label(&old_value))
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
            let new_val = Invocation::bare(cmd).to_token_string();
            {
                let mut draft = self.inner.draft.borrow_mut();
                let vals = draft.entry(pick.chord.clone()).or_default();
                // 置換対象（old_value）があれば外す。空キー定義なら新規割り当て。
                if !pick.old_value.is_empty() {
                    vals.retain(|v| *v != pick.old_value);
                }
                if !vals.iter().any(|v| Invocation::parse(v).map(|i| i.command) == Some(cmd)) {
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

    /// サブ選択キーの「変更（リマップ）」キャプチャを始める（次の打鍵で旧キー→新キーへ移す）。
    fn begin_remap(&self) {
        if self.inner.picking.borrow().is_some()
            || self.selected_command().is_none()
            || self.sub_chord().is_none()
        {
            return;
        }
        self.inner.capturing.set(true);
        self.inner.capturing_remap.set(true);
        *self.inner.status.borrow_mut() = "新しいキーを押してください（右クリックで中止）".to_string();
        self.hwnd().SetFocus();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// サブ選択キーをその機能のまま新しいキーへ移し替える（旧キーから当該機能を外す）。
    fn remap_sub(&self, new: KeyChord) {
        let Some(command) = self.selected_command() else {
            return;
        };
        let Some(old) = self.sub_chord() else {
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
        let value = Invocation::bare(command).to_token_string();
        {
            let mut draft = self.inner.draft.borrow_mut();
            if let Some(vals) = draft.get_mut(&old) {
                vals.retain(|v| Invocation::parse(v).map(|i| i.command) != Some(command));
            }
            let e = draft.entry(new_tok.clone()).or_default();
            e.retain(|v| !v.trim().is_empty());
            if !e.iter().any(|v| Invocation::parse(v).map(|i| i.command) == Some(command)) {
                e.push(value);
            }
        }
        *self.inner.status.borrow_mut() =
            format!("{} を {} から {} に変更しました", command.display_name(), old, new_tok);
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// chord を選択行のコマンドへ割り当てる。**既存の機能は消さず追記する**＝同じ chord に
    /// 別機能があれば衝突になる（マークして OK/適用で解決を促す）。
    fn assign(&self, command: Command, chord: KeyChord) {
        let Some(tok) = chord.to_token() else {
            *self.inner.status.borrow_mut() = "未対応のキーです".to_string();
            return;
        };
        let value = Invocation::bare(command).to_token_string();
        let conflict = {
            let mut draft = self.inner.draft.borrow_mut();
            let e = draft.entry(tok.clone()).or_default();
            // 空 unbind マーカーは取り除く（今バインドし直すので）。
            e.retain(|v| !v.trim().is_empty());
            // 同じ機能が既にこの chord にあるなら追記しない。
            let already = e
                .iter()
                .any(|v| Invocation::parse(v).map(|i| i.command) == Some(command));
            if !already {
                e.push(value);
            }
            e.len() > 1
        };
        *self.inner.status.borrow_mut() = if conflict {
            format!("{} に {} を割り当て（このキーは衝突しています）", command.display_name(), tok)
        } else {
            format!("{} に {} を割り当てました", command.display_name(), tok)
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
                let Some(command) = self.selected_command() else {
                    return;
                };
                let Some(chord) = self.sub_chord() else {
                    *self.inner.status.borrow_mut() = "割り当てがありません".to_string();
                    return;
                };
                // サブ選択キーからその機能だけ取り除く（同キーの他機能＝衝突分は残す）。
                if let Some(vals) = self.inner.draft.borrow_mut().get_mut(&chord) {
                    vals.retain(|v| Invocation::parse(v).map(|i| i.command) != Some(command));
                }
                format!("{} から {} を解除しました", chord, command.display_name())
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
        if self.inner.picking.borrow().is_some() || self.selected_command().is_none() {
            return;
        }
        self.inner.capturing.set(true);
        *self.inner.status.borrow_mut() = "キーを押してください（右クリックで中止）".to_string();
        self.hwnd().SetFocus();
        let _ = self.hwnd().InvalidateRect(None, false);
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
        let n = self.inner.view.borrow().len();
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
        let n = self.inner.view.borrow().len();
        let vis = self.visible_rows();
        let max_top = n.saturating_sub(vis) as isize;
        let top = new_top.clamp(0, max_top) as usize;
        if top != self.inner.top.get() {
            self.inner.top.set(top);
            self.update_scrollbar();
            let _ = self.hwnd().InvalidateRect(None, false);
        }
    }

    /// 選択が見える位置までスクロールを調整する。
    fn ensure_visible(&self) {
        let sel = self.inner.sel.get();
        let vis = self.visible_rows();
        let mut top = self.inner.top.get();
        if sel < top {
            top = sel;
        } else if sel >= top + vis {
            top = sel + 1 - vis;
        }
        self.inner.top.set(top);
        self.update_scrollbar();
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
        self.inner.sub.set(0);
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
            let row = this.inner.top.get() + (p.coords.y / rh) as usize;
            if row < this.inner.view.borrow().len() {
                this.clear_status();
                this.inner.sel.set(row);
                // ピック中は行選択のみ。通常はクリックしたキーをサブ選択（行内 hit-test）。
                let sub = if this.inner.picking.borrow().is_some() {
                    0
                } else {
                    this.chord_hit(row, p.coords.x).unwrap_or(0)
                };
                this.inner.sub.set(sub);
                this.ensure_visible();
                let _ = this.hwnd().InvalidateRect(None, false);
            }
            Ok(())
        });

        // ダブルクリック：機能順=キーを「変更」キャプチャへ／キー順=機能を機能ピッカーへ／
        // ピック中=その機能で確定。
        let this = self.clone();
        self.list.on().wm_l_button_dbl_clk(move |p| {
            if this.inner.capturing.get() {
                return Ok(());
            }
            let rh = this.inner.row_h.get().max(1);
            let row = this.inner.top.get() + (p.coords.y / rh) as usize;
            if row >= this.inner.view.borrow().len() {
                return Ok(());
            }
            this.inner.sel.set(row);
            if this.inner.picking.borrow().is_some() {
                this.commit_pick();
                return Ok(());
            }
            match this.inner.view_mode.get() {
                KeyView::ByCommand => {
                    if let Some(ci) = this.chord_hit(row, p.coords.x) {
                        this.inner.sub.set(ci);
                        this.ensure_visible();
                        this.begin_remap();
                        return Ok(());
                    }
                    // キー未割当（— 表示）の機能をダブルクリック＝新しいキーのキャプチャを始める
                    //（キー順で空キー定義「－」をダブルクリックするのと対称）。
                    let no_keys = this
                        .inner
                        .view
                        .borrow()
                        .get(row)
                        .and_then(|&ri| this.inner.rows.borrow().get(ri).map(|r| r.chords.is_empty()))
                        .unwrap_or(false);
                    if no_keys {
                        this.ensure_visible();
                        this.begin_capture();
                        return Ok(());
                    }
                }
                KeyView::ByKey => {
                    // 機能ラベル上なら そのラベルを差替へ。空キー定義（－）行なら新規割り当てへ。
                    let li = this.label_hit(row, p.coords.x).or_else(|| {
                        let empty = this
                            .inner
                            .view
                            .borrow()
                            .get(row)
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
            this.inner.sub.set(0);
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
            let n = this.inner.view.borrow().len() as isize;
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
                self.remap_sub(chord);
            } else if let Some(command) = self.selected_command() {
                self.assign(command, chord);
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
        } else if vk == k::LEFT {
            self.move_sub(-1);
        } else if vk == k::RIGHT {
            self.move_sub(1);
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
        let bg = w::HBRUSH::GetSysColorBrush(co::COLOR::WINDOW)?;
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
        // 機能順は左にジャンル見出し列を設けるので、機能名は右へずらしキー列も広げる。
        let by_command = !picking && self.inner.view_mode.get() == KeyView::ByCommand;
        let left_x = if by_command { gui::dpi_x(150) } else { key_x };
        let chord_x = if by_command { gui::dpi_x(388) } else { gui::dpi_x(260) };
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
        // 可視範囲の各行を現モードで (左, 右, 右を淡色表示するか) に文字列化する。衝突は ⚠ を付す。
        // 機能順＝(機能, キー群 or "—"), キー順＝(キー, 全機能ラベル or －)。
        let lines: Vec<(String, String, bool)> = {
            match self.inner.view_mode.get() {
            KeyView::ByCommand => {
                let rows = self.inner.rows.borrow();
                (0..vis)
                    .filter_map(|vi| {
                        view.get(top + vi).map(|&ri| {
                            let r = &rows[ri];
                            if r.chords.is_empty() {
                                (r.command.display_name().to_string(), "—".to_string(), true)
                            } else {
                                let joined = r
                                    .chords
                                    .iter()
                                    .map(|c| {
                                        if c.conflicted {
                                            format!("{} ⚠", c.token)
                                        } else {
                                            c.token.clone()
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                (r.command.display_name().to_string(), joined, false)
                            }
                        })
                    })
                    .collect()
            }
            KeyView::ByKey => {
                let krows = self.inner.key_rows.borrow();
                (0..vis)
                    .filter_map(|vi| {
                        view.get(top + vi).map(|&ri| {
                            let r = &krows[ri];
                            if r.labels.is_empty() {
                                // 機能未割当の空キー定義は － を淡色で（機能を割り当ててください）。
                                (r.chord.clone(), "－".to_string(), true)
                            } else {
                                let mut right = r
                                    .labels
                                    .iter()
                                    .map(|l| label_display(l))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                if r.labels.len() > 1 {
                                    right.push_str(" ⚠");
                                }
                                (r.chord.clone(), right, false)
                            }
                        })
                    })
                    .collect()
            }
            }
        };
        // 機能順・選択行のキー群（サブ選択のチップ描画・ヒットテスト用）。
        let sub = self.inner.sub.get();
        let sel_chords: Vec<(String, bool)> = if !picking
            && self.inner.view_mode.get() == KeyView::ByCommand
        {
            view.get(sel)
                .and_then(|&ri| {
                    self.inner
                        .rows
                        .borrow()
                        .get(ri)
                        .map(|r| r.chords.iter().map(|c| (c.token.clone(), c.conflicted)).collect())
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        // 機能順は左の見出し列に、各ジャンルの先頭行（と可視範囲の最上行）でジャンル名を出す。
        let genre_labels: Vec<Option<&'static str>> = if by_command {
            let rows = self.inner.rows.borrow();
            let genre_of = |di: usize| -> Option<&'static str> {
                view.get(di).and_then(|&ri| rows.get(ri)).map(|r| command_genre(r.command).1)
            };
            (0..lines.len())
                .map(|vi| {
                    let di = top + vi;
                    let g = genre_of(di)?;
                    let prev = if vi == 0 { None } else { genre_of(di - 1) };
                    (prev != Some(g)).then_some(g)
                })
                .collect()
        } else {
            Vec::new()
        };
        for (vi, (left, right, muted)) in lines.iter().enumerate() {
            let di = top + vi;
            let y = vi as i32 * row_h;
            let ty = y + (row_h - fh) / 2;
            if di == sel && capturing {
                // キャプチャ中の当該行はアイボリーで「今ここに割り当てている」を示す（青反転にしない）。
                dc.FillRect(w::RECT { left: 0, top: y, right: cw, bottom: y + row_h }, &ivory)?;
                dc.SetTextColor(text_col)?;
            } else if di == sel {
                dc.FillRect(w::RECT { left: 0, top: y, right: cw, bottom: y + row_h }, &hl_bg)?;
                dc.SetTextColor(hl_text)?;
            } else {
                dc.SetTextColor(text_col)?;
            }
            // 機能順：左の見出し列にジャンル名（境界行のみ・淡色）。
            if let Some(g) = genre_labels.get(vi).copied().flatten() {
                let gc = if di == sel && !capturing { hl_text } else { gray_col };
                dc.SetTextColor(gc)?;
                dc.TextOut(key_x, ty, g)?;
                let restore = if di == sel && !capturing { hl_text } else { text_col };
                dc.SetTextColor(restore)?;
            }
            dc.TextOut(left_x, ty, left)?;
            if di == sel && capturing {
                dc.TextOut(chord_x, ty, "← キーを押してください（右クリックで中止）")?;
            } else if di == sel && !sel_chords.is_empty() {
                // 選択行のキーを個別に描く。サブ選択は WINDOW 地のチップで強調する。
                let mut x = chord_x;
                for (ci, (tok, conflicted)) in sel_chords.iter().enumerate() {
                    let mut label = tok.clone();
                    if *conflicted {
                        label.push_str(" ⚠");
                    }
                    let tw = dc.GetTextExtentPoint32(&label).map(|z| z.cx).unwrap_or(0);
                    if ci == sub {
                        dc.FillRect(
                            w::RECT {
                                left: x - gui::dpi_x(3),
                                top: y + gui::dpi_y(1),
                                right: x + tw + gui::dpi_x(3),
                                bottom: y + row_h - gui::dpi_y(1),
                            },
                            &bg,
                        )?;
                        dc.SetTextColor(text_col)?;
                        dc.TextOut(x, ty, &label)?;
                        dc.SetTextColor(hl_text)?;
                    } else {
                        dc.TextOut(x, ty, &label)?;
                    }
                    x += tw;
                    if ci + 1 < sel_chords.len() {
                        dc.TextOut(x, ty, ", ")?;
                        x += dc.GetTextExtentPoint32(", ").map(|z| z.cx).unwrap_or(0);
                    }
                }
            } else {
                if *muted && di != sel {
                    dc.SetTextColor(gray_col)?;
                }
                dc.TextOut(chord_x, ty, right)?;
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
