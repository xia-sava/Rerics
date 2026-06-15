//! 共通モーダルダイアログ基盤。原作 `RecordsLib.PluginMessage` / `MessageForm` 相当。
//!
//! [`message_box`]（`PluginMessage.Show`）と [`input_box`]（`PluginMessage.Input`）を提供する。
//! スタイル（[`MessageStyle`]）でアイコン・ボタン構成を切り替える。`MessageStyle` の整数値は
//! 原作の enum に一致させ、将来スクリプトからの `int` → enum 変換をそのまま通せるようにする。

use std::cell::RefCell;
use std::rc::Rc;

use rerics_core::ConflictResolution;
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
    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title: "同名ファイルの処理",
        size: gui::dpi(380, 250),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
        process_dlg_msgs: true,
        ..Default::default()
    });

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
            text: "すべてに適用(&A)",
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

    #[cfg(feature = "debug-server")]
    let (reg_prompt, reg_wnd) = (name.to_string(), wnd.clone());
    {
        let ok = ok.clone();
        wnd.on().wm_create(move |_| {
            ok.hwnd().SetFocus();
            #[cfg(feature = "debug-server")]
            crate::debug_server::modal_registry::push(
                "conflict",
                "同名ファイルの処理",
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
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::pop();
    let _ = (ok, cancel);
    let r = result.borrow().clone();
    r
}
