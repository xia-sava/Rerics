use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{co, gui, prelude::*};
use super::*;

/// 圧縮（zip 作成）の入力ダイアログ。書庫名（履歴コンボ）と「個別に圧縮する」
/// チェックを尋ね、OK なら [`CompressChoice`]、キャンセル/Esc なら `None` を返す。
/// 個別圧縮を選ぶと書庫名は使わない（各項目が `<項目名>.zip` になる）ので名前欄を無効化する。
/// `history` は書庫名の候補（新しい順）。履歴への追加・保存は呼び出し側が担う。
pub fn compress_box(
    parent: &impl GuiParent,
    default_name: &str,
    history: &[&str],
) -> Option<CompressChoice> {
    let (wnd, arm) = modal_window("圧縮", 360, 168);

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "圧縮ファイル名を入力して下さい。",
            position: gui::dpi(16, 14),
            size: gui::dpi(328, 18),
            ..Default::default()
        },
    );

    let combo = gui::ComboBox::new(
        &wnd,
        gui::ComboBoxOpts {
            control_style: co::CBS::DROPDOWN,
            position: gui::dpi(16, 38),
            width: gui::dpi_x(328),
            items: history,
            ..Default::default()
        },
    );

    let one_by_one = gui::CheckBox::new(
        &wnd,
        gui::CheckBoxOpts {
            text: "選択項目を個別に圧縮する(&O)",
            position: gui::dpi(16, 72),
            size: gui::dpi(328, 22),
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(170, 102),
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
            position: gui::dpi(258, 102),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<CompressChoice>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    arm.plain(
        "compress",
        "圧縮",
        "圧縮ファイル名を入力して下さい。",
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
    );
    {
        let combo_c = combo.clone();
        let default = default_name.to_string();
        arm.on_create(move |_| {
            let _ = combo_c.hwnd().SetWindowText(&default);
            let _ = combo_c.hwnd().SetFocus();
            Ok(())
        });
    }

    // 個別圧縮を選ぶと書庫名は不要なので名前欄をグレーアウトする。
    {
        let combo_c = combo.clone();
        let obo = one_by_one.clone();
        one_by_one.on().bn_clicked(move || {
            let _ = combo_c.hwnd().EnableWindow(!obo.is_checked());
            Ok(())
        });
    }

    {
        let result = result.clone();
        let combo_c = combo.clone();
        let obo = one_by_one.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            *result.borrow_mut() = Some(CompressChoice {
                name: combo_c.hwnd().GetWindowText().unwrap_or_default(),
                one_by_one: obo.is_checked(),
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
    let _ = (ok, cancel);
    let r = result.borrow().clone();
    r
}
