use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{self as w, co, gui, prelude::*};
use super::*;

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

    let (wnd, arm) = modal_window(title, win_w, win_h);

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
    arm.plain("message", title, message, false, reg_buttons);
    let first_btn = buttons.first().cloned();
    let checkbox_k = checkbox.clone();
    arm.on_create(move |hwnd| {
        if let Some(first) = &first_btn {
            first.hwnd().SetFocus();
        }
        // Shift 押下中だけ「すべてに適用」を自動チェックする（Shift＋はい/いいえで全適用）。
        if let Some(cb) = &checkbox_k {
            let cb_k = cb.clone();
            keyhook::push(hwnd, move |msg, wparam| {
                if (msg == keyhook::WM_KEYDOWN || msg == keyhook::WM_KEYUP) && wparam as u16 == 0x10
                {
                    cb_k.set_check(msg == keyhook::WM_KEYDOWN);
                }
                false
            });
        }
        Ok(())
    });

    if let Some(idi) = style.icon()
        && let Ok(mut guard) = w::HINSTANCE::NULL.LoadIcon(w::IdIdiStr::Idi(idi)) {
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

    super::show_modal_guarded(&wnd, parent);
    if has_all {
        keyhook::pop();
    }
    let _ = buttons;
    
    *result.borrow()
}
