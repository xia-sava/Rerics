//! 共通モーダルダイアログ基盤。原作 `RecordsLib.PluginMessage` / `MessageForm` 相当。
//!
//! [`message_box`]（`PluginMessage.Show`）と [`input_box`]（`PluginMessage.Input`）を提供する。
//! スタイル（[`MessageStyle`]）でアイコン・ボタン構成を切り替える。`MessageStyle` の整数値は
//! 原作の enum に一致させ、将来スクリプトからの `int` → enum 変換をそのまま通せるようにする。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use std::time::SystemTime;

use rerics_core::{
    ConflictResolution, FileAttrs, NameCase, SortType, floor_to_local_midnight, format_local,
    parse_local,
};
use winsafe::{self as w, co, gui, prelude::*};

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
pub fn modal_window(title: &str, w: i32, h: i32) -> gui::WindowModal {
    gui::WindowModal::new(gui::WindowModalOpts {
        title,
        size: gui::dpi(w, h),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
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

/// 原作 `PluginMessage.Show` 相当。スタイルに応じてアイコン・ボタンを構成した
/// モーダルを表示し、結果を返す。Enter=既定ボタン、Esc=CancelButton。
pub fn message_box(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    style: MessageStyle,
) -> MessageResult {
    let has_all = style.has_all_checkbox();
    let has_icon = style.icon().is_some();
    let label_x = if has_icon { 56 } else { 16 };

    // メッセージを測ってラベル幅/高を決め、それに合わせて窓・ボタン位置を組む（切れ防止）。
    let (label_w, label_h, wrapped) = measure_message(message);
    let text_top = 18;
    let content_bottom = text_top + label_h;
    // 「すべてに適用」付きはチェックを独立行にするぶん縦に広げる。
    let (checkbox_y, btn_y) = if has_all {
        (content_bottom + 12, content_bottom + 42)
    } else {
        (0, content_bottom + 16)
    };
    let win_h = btn_y + 26 + 22;

    let specs = style.buttons();
    let cancel_index = style.cancel_index();
    let result = Rc::new(RefCell::new(style.default_result()));

    let btn_w = 96;
    let gap = 8;
    let n = specs.len() as i32;
    let total = n * btn_w + (n - 1) * gap;
    // 窓幅はラベルが収まりつつボタン行も収まる幅に。
    let win_w = (label_x + label_w + 16).max(total + 32);

    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title,
        size: gui::dpi(win_w, win_h),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
        process_dlg_msgs: true,
        ..Default::default()
    });

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: &wrapped,
            position: gui::dpi(label_x, text_top),
            size: gui::dpi(label_w, label_h),
            ..Default::default()
        },
    );

    // ボタンは中央寄せの独立行。チェックは（あれば）その上の独立行に左寄せで置く。
    let mut x = (win_w - total) / 2;

    let checkbox = if has_all {
        Some(gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "すべてに適用(&A)",
                position: gui::dpi(16, checkbox_y),
                size: gui::dpi(160, 18),
                ..Default::default()
            },
        ))
    } else {
        None
    };

    #[cfg(feature = "debug-server")]
    let mut reg_buttons: Vec<(String, u16)> = Vec::new();
    let mut buttons = Vec::new();
    for (i, (label, base)) in specs.iter().enumerate() {
        let is_default = i == 0;
        let ctrl_id: u16 = if specs.len() == 1 {
            2
        } else if i == 0 {
            1
        } else if i == cancel_index {
            2
        } else {
            (100 + i) as u16
        };
        #[cfg(feature = "debug-server")]
        reg_buttons.push((label.to_string(), ctrl_id));
        let mut bs = co::BS::PUSHBUTTON;
        if is_default {
            bs = co::BS::DEFPUSHBUTTON;
        }
        let btn = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: label,
                control_style: bs,
                ctrl_id,
                position: gui::dpi(x, btn_y),
                width: gui::dpi_x(btn_w),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );
        x += btn_w + gap;

        let base = *base;
        let result = result.clone();
        let wnd2 = wnd.clone();
        let checkbox = checkbox.clone();
        btn.on().bn_clicked(move || {
            let all = checkbox.as_ref().map(|c| c.is_checked()).unwrap_or(false);
            *result.borrow_mut() = finalize(base, all);
            wnd2.close();
            Ok(())
        });
        buttons.push(btn);
    }

    #[cfg(feature = "debug-server")]
    let (reg_title, reg_prompt, reg_wnd) = (title.to_string(), message.to_string(), wnd.clone());
    if let Some(first) = buttons.first().cloned() {
        wnd.on().wm_create(move |_| {
            first.hwnd().SetFocus();
            #[cfg(feature = "debug-server")]
            crate::debug_server::modal_registry::push(
                "message",
                &reg_title,
                &reg_prompt,
                reg_wnd.hwnd().ptr() as isize,
                false,
                reg_buttons.clone(),
            );
            Ok(0)
        });
    }

    if let Some(idi) = style.icon() {
        if let Ok(mut guard) = w::HINSTANCE::NULL.LoadIcon(w::IdIdiStr::Idi(idi)) {
            let hicon_ptr = guard.leak().ptr() as usize;
            let wnd2 = wnd.clone();
            wnd.on().wm_paint(move || {
                let hdc = wnd2.hwnd().BeginPaint()?;
                unsafe {
                    ffi::DrawIconEx(
                        hdc.ptr(),
                        gui::dpi_x(16),
                        gui::dpi_y(16),
                        hicon_ptr as *mut _,
                        gui::dpi_x(32),
                        gui::dpi_y(32),
                        0,
                        core::ptr::null_mut(),
                        DI_NORMAL,
                    );
                }
                Ok(())
            });
        }
    }

    let _ = wnd.show_modal(parent);
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::pop();
    let _ = buttons;
    let r = *result.borrow();
    r
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

/// 原作 `PluginMessage.Input` 相当。メッセージ＋1行入力のモーダルを表示し、
/// OK なら入力文字列、キャンセル/Esc なら None を返す。初期選択は従来どおり（`AsIs`）。
pub fn input_box(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    value: &str,
    mode: InputMode,
) -> Option<String> {
    input_box_select(parent, title, message, value, mode, InputSelect::AsIs)
}

/// [`input_box`] に初期選択（[`InputSelect`]）指定を加えた版。改名系入力で拡張子前に
/// キャレットを置く（原作 RenameStyle）。
pub fn input_box_select(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    value: &str,
    mode: InputMode,
    select: InputSelect,
) -> Option<String> {
    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title,
        size: gui::dpi(360, 150),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
        process_dlg_msgs: true,
        ..Default::default()
    });

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: message,
            position: gui::dpi(16, 14),
            size: gui::dpi(328, 18),
            ..Default::default()
        },
    );

    let edit_style = match mode {
        InputMode::Plain => co::ES::AUTOHSCROLL,
        InputMode::Password => co::ES::AUTOHSCROLL | co::ES::PASSWORD,
    };
    let edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: value,
            control_style: edit_style,
            position: gui::dpi(16, 38),
            width: gui::dpi_x(328),
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
            position: gui::dpi(170, 80),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let cancel = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "キャンセル",
            ctrl_id: 2,
            position: gui::dpi(258, 80),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    let (reg_title, reg_prompt, reg_wnd) = (title.to_string(), message.to_string(), wnd.clone());
    {
        let edit = edit.clone();
        let value = value.to_string();
        wnd.on().wm_create(move |_| {
            edit.hwnd().SetFocus();
            select.apply(&edit, &value);
            #[cfg(feature = "debug-server")]
            crate::debug_server::modal_registry::push(
                "input",
                &reg_title,
                &reg_prompt,
                reg_wnd.hwnd().ptr() as isize,
                true,
                vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
            );
            Ok(0)
        });
    }

    {
        let result = result.clone();
        let edit = edit.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            *result.borrow_mut() = Some(edit.text().unwrap_or_default());
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

    let _ = wnd.show_modal(parent);
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::pop();
    let _ = cancel;
    let r = result.borrow().clone();
    r
}

/// 原作 `frmCopyOption`（「同名ファイルの処理」）相当。コピー/移動先に同名ファイルが
/// 在るとき、解決方法（最新ならコピー/上書き/強制上書き/名前変更/スキップ）と
/// 「すべてに適用」を尋ねる。OK でラジオ選択＋チェック状態を、中止/Esc で `Cancel` を返す。
pub fn conflict_box(parent: &impl GuiParent, name: &str) -> (ConflictResolution, bool) {
    let wnd = modal_window("同名ファイルの処理", 380, 250);

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: name,
            position: gui::dpi(16, 12),
            size: gui::dpi(348, 18),
            ..Default::default()
        },
    );

    let radios = gui::RadioGroup::new(
        &wnd,
        &[
            gui::RadioButtonOpts {
                text: "最新ならコピー(&N)",
                position: gui::dpi(16, 40),
                size: gui::dpi(220, 20),
                selected: true,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "上書き(&O)",
                position: gui::dpi(16, 64),
                size: gui::dpi(220, 20),
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "強制上書き(&F)",
                position: gui::dpi(16, 88),
                size: gui::dpi(220, 20),
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "名前を変更してコピー(&R)",
                position: gui::dpi(16, 112),
                size: gui::dpi(180, 20),
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "スキップ(&K)",
                position: gui::dpi(16, 136),
                size: gui::dpi(220, 20),
                ..Default::default()
            },
        ],
    );

    let rename = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: name,
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(200, 112),
            width: gui::dpi_x(150),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );

    let all = gui::CheckBox::new(
        &wnd,
        gui::CheckBoxOpts {
            text: "すべてに適用(SHIFT)",
            position: gui::dpi(16, 166),
            size: gui::dpi(220, 18),
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(190, 196),
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
            position: gui::dpi(278, 196),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result = Rc::new(RefCell::new((ConflictResolution::Cancel, false)));
    // 改名 Edit の初期キャレットは拡張子の前（原作 RenameStyle・衝突はファイル名）。
    let rename_pos = before_ext_pos(name, false);

    // 「すべてに適用」中は改名ラジオを無効化（改名＋全適用は排他＝原作）。改名 Edit は
    // 「名前を変更してコピー」選択中かつ全適用未チェックのときだけ有効。
    let refresh: Rc<dyn Fn()> = {
        let radios = radios.clone();
        let rename = rename.clone();
        let all = all.clone();
        Rc::new(move || {
            let all_checked = all.is_checked();
            let rename_sel = radios.selected_index() == Some(3);
            radios[3].hwnd().EnableWindow(!all_checked);
            rename.hwnd().EnableWindow(rename_sel && !all_checked);
        })
    };
    {
        let refresh = refresh.clone();
        let radios_c = radios.clone();
        let rename = rename.clone();
        radios.on().bn_clicked(move || {
            refresh();
            // 「名前を変更してコピー」を選んだら改名 Edit へフォーカス＋拡張子前にキャレット（原作）。
            if radios_c.selected_index() == Some(3) {
                rename.hwnd().SetFocus();
                rename.set_selection(rename_pos, rename_pos);
            }
            Ok(())
        });
    }
    {
        let refresh = refresh.clone();
        all.on().bn_clicked(move || {
            refresh();
            Ok(())
        });
    }

    {
        // 作成時：初期の有効/無効を反映し、Shift 連動＋改名欄の上下キーの keyhook を張る。
        let all_k = all.clone();
        let rename_k = rename.clone();
        let radios_k = radios.clone();
        let refresh_c = refresh.clone();
        let rename_sel = rename.clone();
        arm_modal(
            &wnd,
            "conflict",
            "同名ファイルの処理",
            name,
            true,
            vec![("OK".to_string(), 1u16), ("中止(&S)".to_string(), 2u16)],
            move |hwnd| {
                refresh_c();
                rename_sel.set_selection(rename_pos, rename_pos);
                let all_k = all_k.clone();
                let rename_k = rename_k.clone();
                let radios_k = radios_k.clone();
                let refresh_k = refresh_c.clone();
                keyhook::push(hwnd, move |vk, down| {
                    let in_rename =
                        w::HWND::GetFocus().map(|f| f.ptr()) == Some(rename_k.hwnd().ptr());
                    // 改名 Edit 内の上下キー：ラジオ選択へ戻す（↑=強制上書き idx2・↓=スキップ
                    // idx4）。単一行 Edit の上下は元々無動作なので横取りして問題ない。BM_CLICK で
                    // 標準クリック相当（選択＋フォーカス＋BN_CLICKED→refresh）を起こす。
                    if down && in_rename && (vk == 0x26 || vk == 0x28) {
                        let target = if vk == 0x26 { 2 } else { 4 };
                        unsafe {
                            radios_k[target].hwnd().SendMessage(w::msg::bm::Click {});
                        }
                        return;
                    }
                    // 原作 frmCopyOption：Shift 押下中だけ「すべてに適用」を自動チェック。
                    // 改名 Edit 入力中は Shift を無視する。
                    if vk != 0x10 || in_rename {
                        return;
                    }
                    all_k.set_check(down);
                    refresh_k();
                });
            },
        );
    }

    {
        let result = result.clone();
        let radios = radios.clone();
        let rename = rename.clone();
        let all = all.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            let choice = match radios.selected_index() {
                Some(1) => ConflictResolution::Overwrite,
                Some(2) => ConflictResolution::OverwriteForce,
                Some(3) => ConflictResolution::Rename(rename.text().unwrap_or_default()),
                Some(4) => ConflictResolution::Skip,
                _ => ConflictResolution::Newest,
            };
            *result.borrow_mut() = (choice, all.is_checked());
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

    let _ = wnd.show_modal(parent);
    keyhook::pop();
    disarm_modal();
    let _ = (ok, cancel);
    let r = result.borrow().clone();
    r
}

/// 書庫への追加方式。同名エントリがあるときの分岐に使う。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArchiveAddMode {
    /// 衝突分はスキップし、残りを append で足す（既存を壊さない・高速）。
    Append,
    /// 全体を再構築して同名を置換する（CP932 名は UTF-8 へ近代化される）。
    Rebuild,
}

/// 書庫への追加先に同名エントリがあるとき、追加方式を尋ねる。OK で選択を、
/// 中止/Esc で `None` を返す。`summary` は衝突件数などの説明文。
pub fn archive_add_box(parent: &impl GuiParent, summary: &str) -> Option<ArchiveAddMode> {
    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title: "書庫への追加",
        size: gui::dpi(400, 180),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
        process_dlg_msgs: true,
        ..Default::default()
    });

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: summary,
            position: gui::dpi(16, 12),
            size: gui::dpi(368, 36),
            ..Default::default()
        },
    );

    let radios = gui::RadioGroup::new(
        &wnd,
        &[
            gui::RadioButtonOpts {
                text: "重複はスキップ（衝突分を追加しない）(&K)",
                position: gui::dpi(16, 56),
                size: gui::dpi(360, 20),
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "再構築して置換（既存を上書き）(&R)",
                position: gui::dpi(16, 84),
                size: gui::dpi(360, 20),
                selected: true,
                ..Default::default()
            },
        ],
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(210, 126),
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
            position: gui::dpi(298, 126),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<ArchiveAddMode>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    let (reg_prompt, reg_wnd) = (summary.to_string(), wnd.clone());
    {
        let wf = wnd.clone();
        wnd.on().wm_create(move |_| {
            focus_initial(wf.hwnd());
            #[cfg(feature = "debug-server")]
            crate::debug_server::modal_registry::push(
                "archive_add",
                "書庫への追加",
                &reg_prompt,
                reg_wnd.hwnd().ptr() as isize,
                true,
                vec![("OK".to_string(), 1u16), ("中止(&S)".to_string(), 2u16)],
            );
            Ok(0)
        });
    }

    {
        let result = result.clone();
        let radios = radios.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            let mode = match radios.selected_index() {
                Some(1) => ArchiveAddMode::Rebuild,
                _ => ArchiveAddMode::Append,
            };
            *result.borrow_mut() = Some(mode);
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

    let _ = wnd.show_modal(parent);
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::pop();
    let _ = (ok, cancel);
    let r = *result.borrow();
    r
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

/// ソート設定ダイアログ。種別と昇順/降順を選ばせ、OK なら `(種別, 降順か)` を返す。
/// 中止/Esc は `None`。`cur`/`reverse` を初期選択にする。
pub fn sort_box(parent: &impl GuiParent, cur: SortType, reverse: bool) -> Option<(SortType, bool)> {
    let wnd = modal_window("ソート", 280, 300);

    // エクスプローラ互換は名前/拡張子に直交するチェック。種別ラジオは互換なしの素の種別を選び、
    // 互換種別が現在値なら対応する素の種別ラジオを選びチェックを立てる。
    let (init_kind, init_exp) = match cur {
        SortType::FileNameExpLike => (SortType::FileName, true),
        SortType::ExtensionExpLike => (SortType::Extension, true),
        other => (other, false),
    };

    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "ソート方法",
            position: gui::dpi(16, 12),
            size: gui::dpi(200, 18),
            ..Default::default()
        },
    );
    let kinds = gui::RadioGroup::new(
        &wnd,
        &SORT_KINDS
            .iter()
            .enumerate()
            .map(|(i, (label, ty))| gui::RadioButtonOpts {
                text: label,
                position: gui::dpi(24, 36 + i as i32 * 24),
                size: gui::dpi(240, 20),
                selected: *ty == init_kind,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    );

    let reverse_cb = gui::CheckBox::new(
        &wnd,
        gui::CheckBoxOpts {
            text: "降順(&R)",
            position: gui::dpi(24, 184),
            size: gui::dpi(240, 18),
            check_state: if reverse { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        },
    );
    let explike = gui::CheckBox::new(
        &wnd,
        gui::CheckBoxOpts {
            text: "エクスプローラ互換(&X)",
            position: gui::dpi(24, 208),
            size: gui::dpi(240, 18),
            check_state: if init_exp { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(96, 244),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );
    let cancel = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "キャンセル",
            ctrl_id: 2,
            position: gui::dpi(184, 244),
            width: gui::dpi_x(84),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<(SortType, bool)>>> = Rc::new(RefCell::new(None));

    arm_modal(
        &wnd,
        "sort",
        "ソート",
        "ソートの種別と昇降",
        false,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
        |_| {},
    );
    {
        let result = result.clone();
        let kinds = kinds.clone();
        let reverse_cb = reverse_cb.clone();
        let explike = explike.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            let base = kinds
                .selected_index()
                .and_then(|i| SORT_KINDS.get(i as usize))
                .map(|(_, t)| *t)
                .unwrap_or(SortType::FileName);
            let ty = match (base, explike.is_checked()) {
                (SortType::FileName, true) => SortType::FileNameExpLike,
                (SortType::Extension, true) => SortType::ExtensionExpLike,
                (t, _) => t,
            };
            let rev = reverse_cb.is_checked();
            *result.borrow_mut() = Some((ty, rev));
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

    let _ = wnd.show_modal(parent);
    disarm_modal();
    let _ = (ok, cancel);
    let r = *result.borrow();
    r
}

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

/// 名前・属性・作成/更新日時の変更ダイアログ。`single` が `Some` なら単一対象（名前編集可・
/// 属性とチェックは現在値で初期化・`single_is_dir` がディレクトリ判定）、`None` なら `count`
/// 件への一括（名前は変換メニューで一括・属性は据え置き＝中間状態で初期化）。OK なら
/// [`RenameResult`]、中止/Esc なら `None`。
pub fn rename_box(
    parent: &impl GuiParent,
    single: Option<&str>,
    single_is_dir: bool,
    count: usize,
    attrs: FileAttrs,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
) -> Option<RenameResult> {
    let is_single = single.is_some();
    // 単一ディレクトリ時のみ「サブディレクトリにも適用」チェックを出す（その分縦に広げる）。
    let show_sub = is_single && single_is_dir;
    let win_h = if show_sub { 388 } else { 340 };
    let btn_y = if show_sub { 348 } else { 290 };
    let wnd = modal_window("名前の変更", 360, win_h);

    // 名前行（常設）。単一＝編集可・名前プリフィル、複数＝無効で変換結果ラベルを表示。
    // 右の「...」は名前変換メニュー（原作 btnFileName）。
    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "名前(&N)",
            position: gui::dpi(16, 16),
            size: gui::dpi(76, 18),
            ..Default::default()
        },
    );
    let name_edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: single.unwrap_or(""),
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(96, 14),
            width: gui::dpi_x(212),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let name_btn = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "...",
            ctrl_id: 12,
            position: gui::dpi(312, 14),
            width: gui::dpi_x(26),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    if !is_single {
        let _ = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: &format!("{count} 個の項目に属性／日時／名前を適用します。"),
                position: gui::dpi(16, 40),
                size: gui::dpi(328, 18),
                ..Default::default()
            },
        );
    }

    // 属性チェック群。単一は2状態（現在値で初期化）、一括は3状態（中間＝据え置き）。
    let style = if is_single { co::BS::AUTOCHECKBOX } else { co::BS::AUTO3STATE };
    let init = |on: bool| {
        if is_single {
            if on { co::BST::CHECKED } else { co::BST::UNCHECKED }
        } else {
            co::BST::INDETERMINATE
        }
    };
    let labels = [
        ("読み取り専用(&R)", attrs.readonly),
        ("隠し(&H)", attrs.hidden),
        ("システム(&S)", attrs.system),
        ("アーカイブ(&A)", attrs.archive),
    ];
    let checks: Vec<gui::CheckBox> = labels
        .iter()
        .enumerate()
        .map(|(i, (label, on))| {
            gui::CheckBox::new(
                &wnd,
                gui::CheckBoxOpts {
                    text: label,
                    control_style: style,
                    check_state: init(*on),
                    position: gui::dpi(24, 56 + i as i32 * 26),
                    size: gui::dpi(300, 22),
                    ..Default::default()
                },
            )
        })
        .collect();

    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "日付（YYYY/MM/DD HH:MM:SS・空欄=変更しない）",
            position: gui::dpi(16, 168),
            size: gui::dpi(328, 18),
            ..Default::default()
        },
    );
    // 単一時は現在値でプリフィル、複数一括は空欄（＝据え置き）。更新日付が上・作成日時が下。
    let pre = |t: Option<SystemTime>| match (is_single, t) {
        (true, Some(t)) => format_local(t),
        _ => String::new(),
    };
    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "更新日付(&U)",
            position: gui::dpi(16, 193),
            size: gui::dpi(76, 18),
            ..Default::default()
        },
    );
    let mtime_edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: &pre(modified),
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(96, 190),
            width: gui::dpi_x(212),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let mtime_btn = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "...",
            ctrl_id: 10,
            position: gui::dpi(312, 190),
            width: gui::dpi_x(26),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "作成日時(&C)",
            position: gui::dpi(16, 221),
            size: gui::dpi(76, 18),
            ..Default::default()
        },
    );
    let ctime_edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: &pre(created),
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(96, 218),
            width: gui::dpi_x(212),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let ctime_btn = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "...",
            ctrl_id: 11,
            position: gui::dpi(312, 218),
            width: gui::dpi_x(26),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );

    // サブディレクトリ再帰適用（単一ディレクトリ時のみ）。属性用・日時用を独立に持つ。
    let sub_checks: Option<(gui::CheckBox, gui::CheckBox)> = if show_sub {
        let sub_attr = gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "サブディレクトリにも属性を適用(&B)",
                control_style: co::BS::AUTOCHECKBOX,
                position: gui::dpi(24, 254),
                size: gui::dpi(320, 22),
                ..Default::default()
            },
        );
        let sub_time = gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "サブディレクトリにも日時を適用(&G)",
                control_style: co::BS::AUTOCHECKBOX,
                position: gui::dpi(24, 280),
                size: gui::dpi(320, 22),
                ..Default::default()
            },
        );
        Some((sub_attr, sub_time))
    } else {
        None
    };

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(172, btn_y),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );
    let cancel = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "キャンセル",
            ctrl_id: 2,
            position: gui::dpi(260, btn_y),
            width: gui::dpi_x(84),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    quick_time_menu(&mtime_btn, &mtime_edit);
    quick_time_menu(&ctime_btn, &ctime_edit);

    // サブ適用チェック中は名前編集を無効化（原作 CheckSubState）。属性/日時の再帰だけ行う。
    if let Some((sub_attr, sub_time)) = &sub_checks {
        let refresh = {
            let name_edit = name_edit.clone();
            let name_btn = name_btn.clone();
            let sub_attr = sub_attr.clone();
            let sub_time = sub_time.clone();
            move || {
                let on = sub_attr.is_checked() || sub_time.is_checked();
                name_edit.hwnd().EnableWindow(!on);
                name_btn.hwnd().EnableWindow(!on);
            }
        };
        {
            let refresh = refresh.clone();
            sub_attr.on().bn_clicked(move || {
                refresh();
                Ok(())
            });
        }
        {
            let refresh = refresh.clone();
            sub_time.on().bn_clicked(move || {
                refresh();
                Ok(())
            });
        }
    }

    // 名前変換の選択（複数一括時のみ保持。単一は即時に名前欄へ反映）。
    let name_case: Rc<Cell<NameCase>> = Rc::new(Cell::new(NameCase::None));
    // (種別, メニューラベル)。単一は先頭「何もしない」を出さない（原作準拠）。
    let case_entries = [
        (NameCase::None, "何もしない"),
        (NameCase::Upper, "すべて大文字にする"),
        (NameCase::Lower, "すべて小文字にする"),
        (NameCase::ExtUpper, "拡張子を大文字にする"),
        (NameCase::ExtLower, "拡張子を小文字にする"),
    ];
    {
        let name_edit = name_edit.clone();
        let name_btnf = name_btn.clone();
        let name_case = name_case.clone();
        name_btn.on().bn_clicked(move || {
            let mut menu = w::HMENU::CreatePopupMenu()?;
            let cur = name_case.get();
            for (i, (kind, label)) in case_entries.iter().enumerate() {
                if is_single && *kind == NameCase::None {
                    continue;
                }
                let mut flags = co::MF::STRING;
                if !is_single && *kind == cur {
                    flags |= co::MF::CHECKED;
                }
                menu.AppendMenu(
                    flags,
                    w::IdMenu::Id((i + 1) as u16),
                    w::BmpPtrStr::from_str(label),
                )?;
            }
            let rc = name_btnf.hwnd().GetWindowRect()?;
            let chosen = menu.TrackPopupMenu(
                co::TPM::RETURNCMD | co::TPM::LEFTALIGN | co::TPM::TOPALIGN,
                w::POINT::with(rc.left, rc.bottom),
                name_btnf.hwnd(),
            )?;
            menu.DestroyMenu()?;
            let Some(id) = chosen else {
                return Ok(());
            };
            let kind =
                case_entries.get((id - 1) as usize).map(|(k, _)| *k).unwrap_or(NameCase::None);
            if is_single {
                // 即時変換（「何もしない」は単一では出さない）。
                let next = kind.apply(&name_edit.text()?, single_is_dir);
                name_edit.set_text(&next)?;
                let end = next.encode_utf16().count() as i32;
                name_edit.set_selection(end, end);
            } else {
                name_case.set(kind);
                // 無効な名前欄に選んだ変換のラベルを表示（何もしない＝空）。
                let label = if kind == NameCase::None { "" } else { case_entries[(id - 1) as usize].1 };
                name_edit.set_text(label)?;
            }
            Ok(())
        });
    }

    let result: Rc<RefCell<Option<RenameResult>>> = Rc::new(RefCell::new(None));

    // 単一＝名前 Edit の初期キャレットを末尾（選択なし）に＝従来どおりの手触り。
    // 複数＝名前 Edit を無効化（名前は変換メニュー専用）し、フォーカスを属性へ移す。
    // arm_modal は focus_initial の後に on_create を呼ぶので、ここで設定すれば残る。
    let name_init = name_edit.clone();
    let first_check = checks[0].clone();
    arm_modal(
        &wnd,
        "rename",
        "名前の変更",
        "名前/属性/日時の変更",
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
        move |_| {
            if is_single {
                // 拡張子の前にキャレット（原作 RenameStyle・ディレクトリは末尾）。
                if let Ok(t) = name_init.text() {
                    let pos = before_ext_pos(&t, single_is_dir);
                    name_init.set_selection(pos, pos);
                }
            } else {
                name_init.hwnd().EnableWindow(false);
                first_check.hwnd().SetFocus();
            }
        },
    );
    {
        let result = result.clone();
        let wnd2 = wnd.clone();
        let checks = checks.clone();
        let name_edit = name_edit.clone();
        let mtime_edit = mtime_edit.clone();
        let ctime_edit = ctime_edit.clone();
        let name_case = name_case.clone();
        let sub_checks = sub_checks.clone();
        ok.on().bn_clicked(move || {
            let name = if is_single {
                name_edit.text().ok().map(|s| s.trim().to_owned())
            } else {
                None
            };
            let attrs = [
                cb_tristate(&checks[0]),
                cb_tristate(&checks[1]),
                cb_tristate(&checks[2]),
                cb_tristate(&checks[3]),
            ];
            let parse_time = |e: &gui::Edit| {
                e.text().ok().and_then(|s| {
                    let s = s.trim();
                    if s.is_empty() { None } else { parse_local(s) }
                })
            };
            let modified = parse_time(&mtime_edit);
            let created = parse_time(&ctime_edit);
            let (sub_attr, sub_time) = match &sub_checks {
                Some((a, t)) => (a.is_checked(), t.is_checked()),
                None => (false, false),
            };
            *result.borrow_mut() = Some(RenameResult {
                name,
                attrs,
                modified,
                created,
                name_case: name_case.get(),
                sub_attr,
                sub_time,
            });
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

    let _ = wnd.show_modal(parent);
    disarm_modal();
    let _ = (
        ok, cancel, checks, name_edit, mtime_edit, ctime_edit, mtime_btn, ctime_btn, name_btn,
        sub_checks,
    );
    result.borrow_mut().take()
}

/// 一覧から1つ選ぶモーダル。OK・ダブルクリック・Enter で選択 index を、中止/Esc で
/// `None` を返す。`items` は表示行、`initial` は初期選択行。ドライブ選択・履歴・
/// 登録ディレクトリのジャンプで共用する。
pub fn list_box(
    parent: &impl GuiParent,
    title: &str,
    items: &[String],
    initial: usize,
) -> Option<usize> {
    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title,
        size: gui::dpi(420, 320),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
        process_dlg_msgs: true,
        ..Default::default()
    });

    let list = gui::ListBox::new(
        &wnd,
        gui::ListBoxOpts {
            position: gui::dpi(16, 14),
            size: gui::dpi(388, 250),
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(232, 276),
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
            position: gui::dpi(320, 276),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
    let rows: Vec<String> = items.to_vec();
    let initial = if items.is_empty() { 0 } else { initial.min(items.len() - 1) };

    #[cfg(feature = "debug-server")]
    let (reg_title, reg_wnd, reg_items) = (title.to_string(), wnd.clone(), rows.clone());
    {
        let list = list.clone();
        let rows = rows.clone();
        wnd.on().wm_create(move |_| {
            // ListBox は HWND 生成後でないと add が効かない（生成前 add は無効）。
            if !rows.is_empty() {
                let _ = list.items().add(&rows);
                unsafe {
                    let _ = list
                        .hwnd()
                        .SendMessage(w::msg::lb::SetCurSel { index: Some(initial as u32) });
                }
            }
            list.hwnd().SetFocus();
            #[cfg(feature = "debug-server")]
            crate::debug_server::modal_registry::push_list(
                "list",
                &reg_title,
                reg_items.clone(),
                initial,
                reg_wnd.hwnd().ptr() as isize,
                vec![("OK".to_string(), 1u16), ("中止(&S)".to_string(), 2u16)],
            );
            Ok(0)
        });
    }

    {
        let result = result.clone();
        let list_cap = list.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            let sel = unsafe { list_cap.hwnd().SendMessage(w::msg::lb::GetCurSel {}) };
            if let Some(i) = sel {
                *result.borrow_mut() = Some(i as usize);
            }
            wnd2.close();
            Ok(())
        });
    }

    {
        let result = result.clone();
        let list_cap = list.clone();
        let wnd2 = wnd.clone();
        list.on().lbn_dbl_clk(move || {
            let sel = unsafe { list_cap.hwnd().SendMessage(w::msg::lb::GetCurSel {}) };
            if let Some(i) = sel {
                *result.borrow_mut() = Some(i as usize);
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

    let _ = wnd.show_modal(parent);
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::pop();
    let _ = (ok, cancel, list);
    let r = *result.borrow();
    r
}
