use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{self as w, co, gui, prelude::*};

/// 一覧から1つ選ぶモーダル。OK・ダブルクリック・Enter で選択 index を、キャンセル/Esc で
/// `None` を返す。`items` は表示行、`initial` は初期選択行。ドライブ選択・履歴・
/// 登録ディレクトリのジャンプで共用する。
pub fn list_box(
    parent: &impl GuiParent,
    title: &str,
    items: &[String],
    initial: usize,
) -> Option<usize> {
    let wnd = super::modal_window(title, 420, 320);

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
            text: "キャンセル",
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
                vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
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
