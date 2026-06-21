//! 共通モーダルダイアログ基盤。原作 `RecordsLib.PluginMessage` / `MessageForm` 相当。
//!
//! [`message_box`]（`PluginMessage.Show`）と [`input_box`]（`PluginMessage.Input`）を提供する。
//! スタイル（[`MessageStyle`]）でアイコン・ボタン構成を切り替える。`MessageStyle` の整数値は
//! 原作の enum に一致させ、将来スクリプトからの `int` → enum 変換をそのまま通せるようにする。

use std::time::SystemTime;

use rerics_core::{NameCase, SortType, floor_to_local_midnight, format_local};
use winsafe::{self as w, co, gui, prelude::*};

mod message;
mod input;
mod conflict;
mod archive_add;
mod compress;
mod sort;
mod rename;
mod list;
pub use message::message_box;
pub use input::{input_box, input_box_select, input_box_full};
pub use conflict::conflict_box;
pub use archive_add::archive_add_box;
pub use compress::compress_box;
pub use sort::sort_box;
pub use rename::rename_box;
pub use list::list_box;

#[allow(non_snake_case)]
mod ffi {
    use core::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn DrawIconEx(
            hdc: *mut c_void,
            x: i32,
            y: i32,
            hicon: *mut c_void,
            cx: i32,
            cy: i32,
            step: u32,
            brush: *mut c_void,
            flags: u32,
        ) -> i32;
    }
}

const DI_NORMAL: u32 = 0x0003;

/// モーダル中のキー観測（原作 `Form.KeyPreview=true` 相当）。
///
/// winsafe のモーダル窓は子コントロールにフォーカスがあると生キーを受け取れない（Phase 0 実測）。
/// `WH_KEYBOARD` フックは設置できるが `PostMessage` 合成キーを拾わず headless 検証不可だった。
/// そこで**モーダルの全子コントロールを `SetWindowSubclass`（comctl32）でサブクラス化**して、
/// 子へ dispatch される `WM_KEYDOWN`/`WM_KEYUP` を横取りする。これは**実キーでも PostMessage
/// 合成キーでも発火する**ので headless で検証できる。Shift 連動グレーアウト等のカスタム挙動の土台。
/// 観測はスタックで持ち（モーダルはネストし得る）、最前面の観測のみを呼ぶ。
///
/// conflict_box（同名衝突）の Shift 連動で使用。実キー・PostMessage 合成キー双方で発火する。
pub mod keyhook {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::rc::Rc;
    use winsafe::{self as w, co};

    #[allow(non_snake_case)]
    mod ffi {
        use core::ffi::c_void;
        pub type SubclassProc =
            unsafe extern "system" fn(*mut c_void, u32, usize, isize, usize, usize) -> isize;
        #[link(name = "comctl32")]
        unsafe extern "system" {
            pub fn SetWindowSubclass(
                hwnd: *mut c_void,
                proc: SubclassProc,
                id: usize,
                refdata: usize,
            ) -> i32;
            pub fn DefSubclassProc(
                hwnd: *mut c_void,
                msg: u32,
                wparam: usize,
                lparam: isize,
            ) -> isize;
        }
    }

    const SUBCLASS_ID: usize = 0x5245_4b59; // "REKY"
    const WM_KEYDOWN: u32 = 0x0100;
    const WM_KEYUP: u32 = 0x0101;

    /// `(vk, is_down)` を受け取る観測。`is_down` は押下 true・解放 false。
    type Observer = Rc<dyn Fn(u16, bool)>;

    thread_local! {
        static OBSERVERS: RefCell<Vec<Observer>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "system" fn sub_proc(
        hwnd: *mut c_void,
        msg: u32,
        wparam: usize,
        lparam: isize,
        _id: usize,
        _ref: usize,
    ) -> isize {
        if msg == WM_KEYDOWN || msg == WM_KEYUP {
            // 借用を保持したまま呼ぶと観測内のメッセージ処理で再入し得るので Rc を取り出してから呼ぶ。
            let top = OBSERVERS.with(|o| o.borrow().last().cloned());
            if let Some(f) = top {
                f(wparam as u16, msg == WM_KEYDOWN);
            }
        }
        unsafe { ffi::DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// 観測を積み、`parent` の全子コントロールをサブクラス化してキーを横取りする。
    /// 子コントロールは作成済みである必要があるので `wm_create` の末尾で呼ぶ。
    pub fn push(parent: &w::HWND, cb: impl Fn(u16, bool) + 'static) {
        OBSERVERS.with(|o| o.borrow_mut().push(Rc::new(cb)));
        if let Ok(mut cur) = parent.GetWindow(co::GW::CHILD) {
            loop {
                unsafe {
                    ffi::SetWindowSubclass(cur.ptr() as *mut c_void, sub_proc, SUBCLASS_ID, 0);
                }
                match cur.GetWindow(co::GW::HWNDNEXT) {
                    Ok(n) => cur = n,
                    Err(_) => break,
                }
            }
        }
    }

    /// 最前面の観測を外す（サブクラスは窓破棄で自然消滅するので明示解除は不要）。
    pub fn pop() {
        OBSERVERS.with(|o| {
            o.borrow_mut().pop();
        });
    }
}

/// 標準モーダル窓を作る（タイトル＋クライアント幅高）。原作 `PluginForm` 相当の佇まい
/// （× 無し・最大化/最小化無し・親中央・`IsDialogMessage` 処理あり）を一元化する。
///
/// アプリのモーダルは生の `WindowModal::new` を各所で書かず、必ずこの関数（または
/// [`modal_window_sysmenu`]）を通す。配線（登録・フォーカス）は [`arm_modal`] を併用する。
pub fn modal_window(title: &str, w: i32, h: i32) -> gui::WindowModal {
    modal_window_styled(title, w, h, co::WS::default())
}

/// [`modal_window`] にタイトルバーの × （システムメニュー）を足したもの。設定のように
/// 大きく、× で閉じられると自然なモーダルで使う。
pub fn modal_window_sysmenu(title: &str, w: i32, h: i32) -> gui::WindowModal {
    modal_window_styled(title, w, h, co::WS::SYSMENU)
}

fn modal_window_styled(title: &str, w: i32, h: i32, extra: co::WS) -> gui::WindowModal {
    gui::WindowModal::new(gui::WindowModalOpts {
        title,
        size: gui::dpi(w, h),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE | extra,
        process_dlg_msgs: true,
        ..Default::default()
    })
}

/// モーダルの標準 `wm_create` 配線を仕込む：初期フォーカス（[`focus_initial`]）＋
/// （debug-server 時）`modal_registry` 登録＋ダイアログ固有の作成時処理 `on_create`。
/// `on_create` には親 `HWND` が渡る（子コントロール作成済み＝[`keyhook::push`] や
/// 初期の有効/無効設定をここで行う）。固有処理が要らなければ `|_| {}` を渡す。
/// `buttons` は (ラベル, ctrl_id) の列（OK=1・Cancel=2 等）。
pub fn arm_modal(
    wnd: &gui::WindowModal,
    kind: &'static str,
    reg_title: &str,
    reg_prompt: &str,
    has_input: bool,
    buttons: Vec<(String, u16)>,
    on_create: impl Fn(&w::HWND) + 'static,
) {
    let wf = wnd.clone();
    #[cfg(feature = "debug-server")]
    let reg = (kind, reg_title.to_string(), reg_prompt.to_string(), has_input, buttons);
    #[cfg(not(feature = "debug-server"))]
    let _ = (kind, reg_title, reg_prompt, has_input, buttons);
    wnd.on().wm_create(move |_| {
        focus_initial(wf.hwnd());
        #[cfg(feature = "debug-server")]
        crate::debug_server::modal_registry::push(
            reg.0,
            &reg.1,
            &reg.2,
            wf.hwnd().ptr() as isize,
            reg.3,
            reg.4.clone(),
        );
        on_create(wf.hwnd());
        Ok(0)
    });
}

/// モーダルを閉じた後始末（`modal_registry` から取り除く）。`show_modal` 直後に呼ぶ。
pub fn disarm_modal() {
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::pop();
}

/// 原作 WinForms の「ロード時にタブ順先頭のコントロールへフォーカス」を再現する基盤処理。
///
/// winsafe の既定（`delegate_focus_to_first_child`）は先頭の子＝ラベル等にもフォーカスを
/// 投げるだけで、矢印操作の起点にならない。ここでは**先頭の「可視・有効・WS_TABSTOP」
/// コントロール**を z 順で探し、それがラジオなら**同グループの選択中ラジオ**へ寄せる
/// （開いてすぐ矢印で選べ、最初の矢印で選択を失わない＝原作の手触り）。各ダイアログは
/// `wm_create` でこれを呼ぶだけ＝初期フォーカスのロジックを基盤に集約する。
pub fn focus_initial(parent: &w::HWND) {
    let style = |h: &w::HWND| h.GetWindowLongPtr(co::GWLP::STYLE) as u32;
    let is_radio = |h: &w::HWND| {
        h.GetClassName().map(|c| c.eq_ignore_ascii_case("button")).unwrap_or(false)
            && matches!(style(h) & 0x0F, 0x04 | 0x09)
    };
    let checked = |h: &w::HWND| unsafe { h.SendMessage(w::msg::bm::GetCheck {}) } == co::BST::CHECKED;

    // タブ順の先頭コントロール（ダイアログマネージャの規則で解決＝z 順依存しない）。
    let Ok(first) = parent.GetNextDlgTabItem(&w::HWND::NULL, false) else {
        return;
    };
    if first.ptr().is_null() {
        return;
    }

    // 先頭がラジオで未選択なら、同グループの選択中ラジオへ寄せる（GetNextDlgGroupItem は
    // グループ内を巡回する）。選択を失わずに矢印で操作できる＝原作の手触り。
    let focus = if is_radio(&first) && !checked(&first) {
        let mut found: Option<w::HWND> = None;
        let mut cur = unsafe { first.raw_copy() };
        for _ in 0..64 {
            let Ok(next) = parent.GetNextDlgGroupItem(&cur, false) else {
                break;
            };
            if next.ptr() == first.ptr() {
                break;
            }
            if is_radio(&next) && checked(&next) {
                found = Some(unsafe { next.raw_copy() });
                break;
            }
            cur = next;
        }
        found.unwrap_or(first)
    } else {
        first
    };
    focus.SetFocus();
}

/// メッセージボックスのスタイル。整数値は原作 `RecordsLib.MessageStyle` に一致させ、
/// 将来スクリプトからの `int` → enum 変換をそのまま通せるようにする。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum MessageStyle {
    OkOnly = 1,
    OkCancel = 2,
    AbortRetryIgnore = 3,
    YesNo = 4,
    YesNoCancel = 5,
    YesNoAll = 6,
    YesNoCancelAll = 7,
    RetryCancel = 8,
    Warning = 9,
    Error = 10,
}

/// メッセージボックスの結果。原作 `RecordsLib.MessageResult` 準拠。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageResult {
    Ok,
    Cancel,
    Abort,
    Retry,
    Ignore,
    Yes,
    No,
    YesAll,
    NoAll,
}

impl MessageStyle {
    /// 左に表示する system アイコン（無ければ None）。
    fn icon(self) -> Option<co::IDI> {
        use MessageStyle::*;
        match self {
            OkOnly => Some(co::IDI::INFORMATION),
            Warning => Some(co::IDI::WARNING),
            Error => Some(co::IDI::ERROR),
            OkCancel | AbortRetryIgnore | YesNo | YesNoCancel | YesNoAll | YesNoCancelAll
            | RetryCancel => Some(co::IDI::QUESTION),
        }
    }

    /// ボタンを押さず閉じた場合の既定結果。
    fn default_result(self) -> MessageResult {
        use MessageResult as R;
        use MessageStyle::*;
        match self {
            OkOnly | Warning | Error => R::Ok,
            OkCancel | RetryCancel => R::Cancel,
            YesNo | YesNoCancel | YesNoAll | YesNoCancelAll => R::No,
            AbortRetryIgnore => R::Abort,
        }
    }

    fn has_all_checkbox(self) -> bool {
        matches!(self, MessageStyle::YesNoAll | MessageStyle::YesNoCancelAll)
    }

    /// (ラベル, 基本結果) の並び。先頭が既定ボタン(Enter)、`cancel_index` が Esc。
    fn buttons(self) -> Vec<(&'static str, MessageResult)> {
        use MessageResult as R;
        use MessageStyle::*;
        match self {
            OkOnly | Warning | Error => vec![("OK", R::Ok)],
            OkCancel => vec![("OK", R::Ok), ("キャンセル", R::Cancel)],
            YesNo | YesNoAll => vec![("はい(&Y)", R::Yes), ("いいえ(&N)", R::No)],
            YesNoCancel | YesNoCancelAll => {
                vec![("はい(&Y)", R::Yes), ("いいえ(&N)", R::No), ("キャンセル", R::Cancel)]
            }
            AbortRetryIgnore => {
                vec![("中止(&A)", R::Abort), ("再試行(&R)", R::Retry), ("無視(&I)", R::Ignore)]
            }
            RetryCancel => vec![("再試行(&R)", R::Retry), ("キャンセル", R::Cancel)],
        }
    }

    /// Esc が対応するボタン index（原作 CancelButton。単一ボタンは 0）。
    fn cancel_index(self) -> usize {
        use MessageStyle::*;
        match self {
            OkOnly | Warning | Error => 0,
            _ => 1,
        }
    }
}

/// 基本結果を「すべてに適用」チェック状態で最終結果に変換する。
fn finalize(base: MessageResult, all_checked: bool) -> MessageResult {
    match base {
        MessageResult::Yes if all_checked => MessageResult::YesAll,
        MessageResult::No if all_checked => MessageResult::NoAll,
        other => other,
    }
}

/// メッセージを UI フォント（ラベルと同じ `lfMenuFont`）で測り、論理単位の
/// (ラベル幅, ラベル高) と**折返し済みテキスト**を返す。幅は最長行に合わせ `[MIN,MAX]` に
/// クランプし、その幅で**手動折返し**する（空白優先・無ければ文字境界で割る）。`DrawText` の
/// `DT_WORDBREAK` は空白でしか折らずパス等の連続文字列が切れるため、自前で折って切れを防ぐ。
/// 測定できなければ元テキストをそのまま返す。
fn measure_message(message: &str) -> (i32, i32, String) {
    const MIN_LW: i32 = 300;
    const MAX_LW: i32 = 560;
    let measured = (|| -> w::SysResult<(i32, i32, String)> {
        let mut ncm = w::NONCLIENTMETRICS::default();
        unsafe {
            w::SystemParametersInfo(
                co::SPI::GETNONCLIENTMETRICS,
                std::mem::size_of::<w::NONCLIENTMETRICS>() as u32,
                &mut ncm,
                co::SPIF::NoValue,
            )?;
        }
        let font = w::HFONT::CreateFontIndirect(&ncm.lfMenuFont)?;
        let dc = w::HWND::NULL.GetDC()?;
        let _sel = dc.SelectObject(&*font)?;
        let width = |s: &str| dc.GetTextExtentPoint32(s).map(|z| z.cx).unwrap_or(0);
        let line_h = dc.GetTextExtentPoint32("Ag").map(|z| z.cy).unwrap_or(16);

        let fx = gui::dpi_x(1000).max(1) as i64;
        let fy = gui::dpi_y(1000).max(1) as i64;
        let to_lx = |p: i32| (p as i64 * 1000 / fx) as i32;
        let to_ly = |p: i32| ((p as i64 * 1000 + fy - 1) / fy) as i32;

        // 折返し幅（物理）＝最長行の自然幅を [MIN,MAX] にクランプ。
        let natural = message.split('\n').map(&width).max().unwrap_or(0);
        let lw = (to_lx(natural) + 8).clamp(MIN_LW, MAX_LW);
        let max_w = gui::dpi_x(lw);

        // 手動折返し：空白で詰めていき、語単体が幅を超えるなら文字境界で割る。
        let mut out: Vec<String> = Vec::new();
        for line in message.split('\n') {
            if width(line) <= max_w {
                out.push(line.to_string());
                continue;
            }
            let mut cur = String::new();
            for word in line.split(' ') {
                let cand =
                    if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
                if width(&cand) <= max_w {
                    cur = cand;
                    continue;
                }
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut rest = word;
                while width(rest) > max_w {
                    let mut cut = 0;
                    for (i, _) in rest.char_indices().skip(1) {
                        if width(&rest[..i]) > max_w {
                            break;
                        }
                        cut = i;
                    }
                    if cut == 0 {
                        // 1文字でも幅を超える（極端に狭い）→ 1文字ずつ進めて無限ループを防ぐ。
                        cut = rest.char_indices().nth(1).map(|(i, _)| i).unwrap_or(rest.len());
                    }
                    out.push(rest[..cut].to_string());
                    rest = &rest[cut..];
                }
                cur = rest.to_string();
            }
            out.push(cur);
        }
        if out.is_empty() {
            out.push(String::new());
        }
        let h_phys = out.len() as i32 * line_h;
        Ok((lw, to_ly(h_phys).max(18) + 6, out.join("\n")))
    })();
    measured.unwrap_or_else(|_| (MIN_LW, 48, message.to_string()))
}


/// 入力ボックスの入力種別。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// 通常テキスト。
    Plain,
    /// 伏字（パスワード）。
    Password,
}

/// 拡張子の前（最後の `.` の手前）の UTF-16 位置を返す。ディレクトリ・拡張子なし・
/// 先頭ドット（`.gitignore` 等）は末尾位置。`EM_SETSEL` のキャレット位置に使う。
fn before_ext_pos(name: &str, is_dir: bool) -> i32 {
    let end = name.encode_utf16().count() as i32;
    if is_dir {
        return end;
    }
    match name.rfind('.') {
        Some(0) | None => end,
        Some(idx) => name[..idx].encode_utf16().count() as i32,
    }
}

/// 入力欄の初期選択。`AsIs`＝明示設定なし（従来）、`BeforeExt`＝拡張子の前にキャレット
/// （原作 RenameStyle "BeforeExtension"・選択なし）。改名系入力で使う。
#[derive(Clone, Copy)]
pub enum InputSelect {
    AsIs,
    BeforeExt { is_dir: bool },
}

impl InputSelect {
    /// テキスト `text` を持つ `edit` に初期選択を適用する（フォーカス後に呼ぶ）。
    fn apply(self, edit: &gui::Edit, text: &str) {
        if let InputSelect::BeforeExt { is_dir } = self {
            let pos = before_ext_pos(text, is_dir);
            edit.set_selection(pos, pos);
        }
    }
}

/// 書庫への追加方式。同名エントリがあるときの分岐に使う。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArchiveAddMode {
    /// 衝突分はスキップし、残りを append で足す（既存を壊さない・高速）。
    Append,
    /// 全体を再構築して同名を置換する（CP932 名は UTF-8 へ近代化される）。
    Rebuild,
}

/// 圧縮ダイアログの結果（書庫名と圧縮方式）。
#[derive(Clone, PartialEq, Eq)]
pub struct CompressChoice {
    /// 出力する zip 名（まとめて1つに圧縮する場合に使う）。
    pub name: String,
    /// 選択項目を個別に `<項目名>.zip` へ圧縮する（true）か、まとめて1つにする（false）か。
    pub one_by_one: bool,
}


/// ソート設定ダイアログの種別ラジオ（表示ラベル → ソート種別・表示順）。
/// エクスプローラ互換は独立チェックで名前/拡張子に直交させるため、種別はこの6つのみ。
const SORT_KINDS: &[(&str, SortType)] = &[
    ("名前順(&F)", SortType::FileName),
    ("拡張子(&E)", SortType::Extension),
    ("更新日付(&D)", SortType::LastWriteTime),
    ("サイズ(&S)", SortType::Length),
    ("属性(&A)", SortType::Attribute),
    ("作成日付(&C)", SortType::CreateTime),
];


/// 名前変更ダイアログの結果。`name` は単一時の変更後名（複数一括は `None`）、
/// `attrs` は RO/隠し/システム/アーカイブの各設定（`Some` で設定・`None` で据え置き）、
/// `modified`/`created` は更新日時・作成日時（`Some` で設定・`None` で据え置き）。
pub struct RenameResult {
    pub name: Option<String>,
    pub attrs: [Option<bool>; 4],
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    /// 複数一括時の名前変換（単一は即時に `name` へ反映済みなので `None`）。
    pub name_case: NameCase,
    /// 単一ディレクトリ時、属性を配下へ再帰適用するか。
    pub sub_attr: bool,
    /// 単一ディレクトリ時、日時を配下へ再帰適用するか。
    pub sub_time: bool,
}

/// 日時欄の横の「...」ボタンに、原作の日時クイック設定メニュー（現在時刻／00:00:00）を
/// 配線する。選んだ値を `edit` に書き込む。`TrackPopupMenu` 自身がモーダルループを回す
/// ので、`TPM::RETURNCMD` で選択コマンドを同期取得する。
fn quick_time_menu(btn: &gui::Button, edit: &gui::Edit) {
    let edit = edit.clone();
    let btnf = btn.clone();
    btn.on().bn_clicked(move || {
        let mut menu = w::HMENU::CreatePopupMenu()?;
        menu.AppendMenu(co::MF::STRING, w::IdMenu::Id(1), w::BmpPtrStr::from_str("現在時刻"))?;
        menu.AppendMenu(co::MF::STRING, w::IdMenu::Id(2), w::BmpPtrStr::from_str("00:00:00"))?;
        let rc = btnf.hwnd().GetWindowRect()?;
        let chosen = menu.TrackPopupMenu(
            co::TPM::RETURNCMD | co::TPM::LEFTALIGN | co::TPM::TOPALIGN,
            w::POINT::with(rc.left, rc.bottom),
            btnf.hwnd(),
        )?;
        let now = SystemTime::now();
        match chosen {
            Some(1) => edit.set_text(&format_local(now))?,
            Some(2) => edit.set_text(&format_local(floor_to_local_midnight(now)))?,
            _ => {}
        }
        menu.DestroyMenu()?;
        Ok(())
    });
}

/// チェックボックスの状態を「設定する/しない/据え置き」に読み替える。
fn cb_tristate(cb: &gui::CheckBox) -> Option<bool> {
    match cb.state() {
        co::BST::CHECKED => Some(true),
        co::BST::UNCHECKED => Some(false),
        _ => None,
    }
}


