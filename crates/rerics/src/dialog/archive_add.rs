use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{co, gui, prelude::*};
use super::*;

/// 書庫への追加先に同名エントリがあるとき、追加方式を尋ねる。OK で選択を、
/// キャンセル/Esc で `None` を返す。`summary` は衝突件数などの説明文。
pub fn archive_add_box(parent: &impl GuiParent, summary: &str) -> Option<ArchiveAddMode> {
    let (wnd, _arm) = modal_window("書庫への追加", 400, 180);

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
            text: "キャンセル",
            ctrl_id: 2,
            position: gui::dpi(298, 126),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<ArchiveAddMode>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    _arm.plain(
        "archive_add",
        "書庫への追加",
        summary,
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
    );

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

    super::show_modal_guarded(&wnd, parent);
    let _ = (ok, cancel);
    
    *result.borrow()
}
