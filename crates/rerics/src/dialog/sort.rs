use std::cell::RefCell;
use std::rc::Rc;
use rerics_core::SortType;
use winsafe::{co, gui, prelude::*};
use super::*;

/// ソート設定ダイアログ。種別と昇順/降順を選ばせ、OK なら `(種別, 降順か)` を返す。
/// 中止/Esc は `None`。`cur`/`reverse` を初期選択にする。
pub fn sort_box(parent: &impl GuiParent, cur: SortType, reverse: bool) -> Option<(SortType, bool)> {
    let (wnd, arm) = modal_window("ソート", 280, 300);

    // 自然順は名前/拡張子に直交するチェック。種別ラジオは自然順なしの素の種別を選び、
    // 自然順種別が現在値なら対応する素の種別ラジオを選びチェックを立てる。
    let (init_kind, init_exp) = cur.split_explike();

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
            text: "自然順(&X)",
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
        &arm,
        "sort",
        "ソート",
        "ソートの種別と昇降",
        false,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
        |_| Ok(()),
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
                .and_then(|i| SORT_KINDS.get(i))
                .map(|(_, t)| *t)
                .unwrap_or(SortType::FileName);
            let ty = SortType::with_explike(base, explike.is_checked());
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

    super::show_modal_guarded(&wnd, parent);
    let _ = (ok, cancel);
    
    *result.borrow()
}
