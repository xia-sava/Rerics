//! 共通モーダルダイアログ基盤。原作 `RecordsLib.PluginMessage` / `MessageForm` 相当。
//!
//! [`message_box`]（`PluginMessage.Show`）と [`input_box`]（`PluginMessage.Input`）を提供する。
//! スタイル（[`MessageStyle`]）でアイコン・ボタン構成を切り替える。`MessageStyle` の整数値は
//! 原作の enum に一致させ、将来スクリプトからの `int` → enum 変換をそのまま通せるようにする。

use std::cell::RefCell;
use std::rc::Rc;

use std::time::SystemTime;

use rerics_core::{ConflictResolution, FileAttrs, SortType, format_local, parse_local};
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

/// 原作 `PluginMessage.Show` 相当。スタイルに応じてアイコン・ボタンを構成した
/// モーダルを表示し、結果を返す。Enter=既定ボタン、Esc=CancelButton。
pub fn message_box(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    style: MessageStyle,
) -> MessageResult {
    let style_has_all = style.has_all_checkbox();
    // 「すべてに適用」付きのスタイルはチェックを独立行にするぶん縦に広げる。
    let win_h = if style_has_all { 185 } else { 150 };
    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title,
        size: gui::dpi(400, win_h),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
        process_dlg_msgs: true,
        ..Default::default()
    });

    let has_icon = style.icon().is_some();
    let label_x = if has_icon { 56 } else { 16 };
    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: message,
            position: gui::dpi(label_x, 18),
            size: gui::dpi(400 - label_x - 16, 60),
            ..Default::default()
        },
    );

    let specs = style.buttons();
    let cancel_index = style.cancel_index();
    let has_all = style_has_all;
    let result = Rc::new(RefCell::new(style.default_result()));

    let btn_w = 96;
    let gap = 8;
    let n = specs.len() as i32;
    // ボタンは中央寄せの独立行。チェックは（あれば）その上の独立行に左寄せで置く。
    let total = n * btn_w + (n - 1) * gap;
    let mut x = (400 - total) / 2;
    let btn_y = if has_all { 122 } else { 96 };

    let checkbox = if has_all {
        Some(gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "すべてに適用(&A)",
                position: gui::dpi(16, 92),
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

/// 原作 `PluginMessage.Input` 相当。メッセージ＋1行入力のモーダルを表示し、
/// OK なら入力文字列、キャンセル/Esc なら None を返す。
pub fn input_box(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    value: &str,
    mode: InputMode,
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
        wnd.on().wm_create(move |_| {
            edit.hwnd().SetFocus();
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
        radios.on().bn_clicked(move || {
            refresh();
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
        // 作成時：初期の有効/無効を反映し、Shift 連動の keyhook を張る。
        let all_k = all.clone();
        let rename_k = rename.clone();
        let refresh_c = refresh.clone();
        arm_modal(
            &wnd,
            "conflict",
            "同名ファイルの処理",
            name,
            true,
            vec![("OK".to_string(), 1u16), ("中止(&S)".to_string(), 2u16)],
            move |hwnd| {
                refresh_c();
                let all_k = all_k.clone();
                let rename_k = rename_k.clone();
                let refresh_k = refresh_c.clone();
                // 原作 frmCopyOption：Shift 押下中だけ「すべてに適用」を自動チェック。
                // 改名 Edit 入力中は Shift を無視する。
                keyhook::push(hwnd, move |vk, down| {
                    if vk != 0x10 {
                        return;
                    }
                    if w::HWND::GetFocus().map(|f| f.ptr()) == Some(rename_k.hwnd().ptr()) {
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

/// ソート設定ダイアログの種別リスト（表示ラベル → ソート種別・表示順）。
const SORT_KINDS: &[(&str, SortType)] = &[
    ("名前(&N)", SortType::FileName),
    ("拡張子(&E)", SortType::Extension),
    ("サイズ(&S)", SortType::Length),
    ("更新日時(&M)", SortType::LastWriteTime),
    ("作成日時(&C)", SortType::CreateTime),
    ("属性(&A)", SortType::Attribute),
    ("名前(Explorer風)", SortType::FileNameExpLike),
    ("拡張子(Explorer風)", SortType::ExtensionExpLike),
];

/// ソート設定ダイアログ。種別と昇順/降順を選ばせ、OK なら `(種別, 降順か)` を返す。
/// 中止/Esc は `None`。`cur`/`reverse` を初期選択にする。
pub fn sort_box(parent: &impl GuiParent, cur: SortType, reverse: bool) -> Option<(SortType, bool)> {
    let wnd = modal_window("ソート設定", 320, 320);

    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "並べ替えの基準",
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
                size: gui::dpi(260, 20),
                selected: *ty == cur,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    );

    let dir = gui::RadioGroup::new(
        &wnd,
        &[
            gui::RadioButtonOpts {
                text: "昇順(&U)",
                position: gui::dpi(24, 236),
                size: gui::dpi(120, 20),
                selected: !reverse,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "降順(&D)",
                position: gui::dpi(150, 236),
                size: gui::dpi(120, 20),
                selected: reverse,
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
            position: gui::dpi(132, 274),
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
            position: gui::dpi(220, 274),
            width: gui::dpi_x(84),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<(SortType, bool)>>> = Rc::new(RefCell::new(None));

    arm_modal(
        &wnd,
        "sort",
        "ソート設定",
        "ソートの種別と昇降",
        false,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
        |_| {},
    );
    {
        let result = result.clone();
        let kinds = kinds.clone();
        let dir = dir.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            let ty = kinds
                .selected_index()
                .and_then(|i| SORT_KINDS.get(i as usize))
                .map(|(_, t)| *t)
                .unwrap_or(SortType::FileName);
            let rev = dir.selected_index() == Some(1);
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
/// `modified` は更新日時（`Some` で設定・`None` で据え置き）。
pub struct RenameResult {
    pub name: Option<String>,
    pub attrs: [Option<bool>; 4],
    pub modified: Option<SystemTime>,
}

/// チェックボックスの状態を「設定する/しない/据え置き」に読み替える。
fn cb_tristate(cb: &gui::CheckBox) -> Option<bool> {
    match cb.state() {
        co::BST::CHECKED => Some(true),
        co::BST::UNCHECKED => Some(false),
        _ => None,
    }
}

/// 名前・属性・更新日時の変更ダイアログ。`single` が `Some` なら単一対象（名前編集可・
/// 属性とチェックは現在値で初期化）、`None` なら `count` 件への一括（名前なし・属性は
/// 据え置き＝中間状態で初期化）。OK なら [`RenameResult`]、中止/Esc なら `None`。
pub fn rename_box(
    parent: &impl GuiParent,
    single: Option<&str>,
    count: usize,
    attrs: FileAttrs,
    modified: Option<SystemTime>,
) -> Option<RenameResult> {
    let is_single = single.is_some();
    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title: "名前と属性の変更",
        size: gui::dpi(360, 340),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
        process_dlg_msgs: true,
        ..Default::default()
    });

    let name_edit = if let Some(name) = single {
        let _ = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: "名前(&N)",
                position: gui::dpi(16, 16),
                size: gui::dpi(80, 18),
                ..Default::default()
            },
        );
        Some(gui::Edit::new(
            &wnd,
            gui::EditOpts {
                text: name,
                control_style: co::ES::AUTOHSCROLL,
                position: gui::dpi(96, 14),
                width: gui::dpi_x(248),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        ))
    } else {
        let _ = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: &format!("{count} 個の項目に属性／更新日時を適用します。", count = count),
                position: gui::dpi(16, 16),
                size: gui::dpi(328, 18),
                ..Default::default()
            },
        );
        None
    };

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
            text: "更新日時 (YYYY-MM-DD HH:MM:SS・空欄=変更しない)",
            position: gui::dpi(16, 172),
            size: gui::dpi(328, 18),
            ..Default::default()
        },
    );
    let time_text = match (is_single, modified) {
        (true, Some(t)) => format_local(t),
        _ => String::new(),
    };
    let time_edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: &time_text,
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(24, 194),
            width: gui::dpi_x(240),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(172, 290),
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
            position: gui::dpi(260, 290),
            width: gui::dpi_x(84),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<RenameResult>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    let reg_wnd = wnd.clone();
    {
        let wf = wnd.clone();
        wnd.on().wm_create(move |_| {
            focus_initial(wf.hwnd());
            #[cfg(feature = "debug-server")]
            crate::debug_server::modal_registry::push(
                "rename",
                "名前と属性の変更",
                "名前/属性/更新日時の変更",
                reg_wnd.hwnd().ptr() as isize,
                true,
                vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
            );
            Ok(0)
        });
    }
    {
        let result = result.clone();
        let wnd2 = wnd.clone();
        let checks = checks.clone();
        let name_edit = name_edit.clone();
        let time_edit = time_edit.clone();
        ok.on().bn_clicked(move || {
            let name = name_edit.as_ref().and_then(|e| e.text().ok()).map(|s| s.trim().to_owned());
            let attrs = [
                cb_tristate(&checks[0]),
                cb_tristate(&checks[1]),
                cb_tristate(&checks[2]),
                cb_tristate(&checks[3]),
            ];
            let modified = time_edit.text().ok().and_then(|s| {
                let s = s.trim();
                if s.is_empty() { None } else { parse_local(s) }
            });
            *result.borrow_mut() = Some(RenameResult { name, attrs, modified });
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
    let _ = (ok, cancel, checks, name_edit, time_edit);
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
