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
    let (wnd, arm) = super::modal_window_resizable(title, 420, 320);

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
    #[cfg(feature = "debug-server")]
    arm.list(
        "list",
        title,
        rows.clone(),
        initial,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
    );
    {
        let list = list.clone();
        let rows = rows.clone();
        arm.on_create(move |_| {
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
            Ok(())
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

    // リサイズ追従：ListBox は余白いっぱいに広げ、OK/キャンセルは右下へ寄せ直す。
    {
        let wndc = wnd.clone();
        let (lst, okc, cancelc) = (list.clone(), ok.clone(), cancel.clone());
        wnd.on().wm_size(move |_| {
            if let Ok(rc) = wndc.hwnd().GetClientRect() {
                relayout(&lst, &okc, &cancelc, rc.right, rc.bottom);
            }
            Ok(())
        });
    }
    // 小さくし過ぎてボタンや一覧が潰れないよう最小サイズを抑える。
    wnd.on().wm_get_min_max_info(move |p| {
        p.info.ptMinTrackSize = w::POINT { x: gui::dpi_x(280), y: gui::dpi_y(200) };
        Ok(())
    });

    let _ = wnd.show_modal(parent);
    let _ = (ok, cancel, list);
    let r = *result.borrow();
    r
}

/// クライアント寸法 `cw`×`ch`（物理px）に合わせて一覧とボタンを再配置する。一覧は左右上を
/// 16/14px の余白で広げ、OK/キャンセルは下端右寄せ（OK が左・キャンセルが右）。
fn relayout(list: &gui::ListBox, ok: &gui::Button, cancel: &gui::Button, cw: i32, ch: i32) {
    let mx = gui::dpi_x(16);
    let top = gui::dpi_y(14);
    let bh = gui::dpi_y(26);
    let bottom_margin = gui::dpi_y(16);
    let gap = gui::dpi_y(12);
    let ok_w = gui::dpi_x(80);
    let cancel_w = gui::dpi_x(86);
    let btn_gap = gui::dpi_x(8);

    let btn_y = (ch - bottom_margin - bh).max(top);
    let cancel_x = (cw - mx - cancel_w).max(0);
    let ok_x = (cancel_x - btn_gap - ok_w).max(0);
    let list_w = (cw - mx * 2).max(1);
    let list_h = (btn_y - gap - top).max(1);

    let _ = list.hwnd().MoveWindow(w::POINT { x: mx, y: top }, w::SIZE { cx: list_w, cy: list_h }, true);
    let _ = ok.hwnd().MoveWindow(w::POINT { x: ok_x, y: btn_y }, w::SIZE { cx: ok_w, cy: bh }, true);
    let _ = cancel.hwnd().MoveWindow(w::POINT { x: cancel_x, y: btn_y }, w::SIZE { cx: cancel_w, cy: bh }, true);
}
