use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{co, gui, prelude::*};
use super::*;

/// ラジオの並び順（上から）。
const KINDS: [LinkKind; 3] = [LinkKind::Symlink, LinkKind::Junction, LinkKind::Shortcut];

/// 作成するリンクの種類を尋ねる。OK で選択を、キャンセル/Esc で `None` を返す。
/// `symlink_enabled`/`junction_enabled` が偽の種類はグレーアウトして選べなくする。
/// `default` を初期選択にする（呼び手が有効な種類を渡す）。
pub fn link_kind_box(
    parent: &impl GuiParent,
    symlink_enabled: bool,
    junction_enabled: bool,
    default: LinkKind,
) -> Option<LinkKind> {
    let (wnd, _arm) = modal_window("リンクの作成", 400, 176);

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "作成するリンクの種類を選んで下さい。",
            position: gui::dpi(16, 12),
            size: gui::dpi(368, 18),
            ..Default::default()
        },
    );

    let radios = gui::RadioGroup::new(
        &wnd,
        &[
            gui::RadioButtonOpts {
                text: "シンボリックリンク(&Y)",
                position: gui::dpi(16, 42),
                size: gui::dpi(360, 20),
                selected: default == LinkKind::Symlink,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "ジャンクション（ディレクトリのみ）(&J)",
                position: gui::dpi(16, 68),
                size: gui::dpi(360, 20),
                selected: default == LinkKind::Junction,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "ショートカット（.lnk）(&S)",
                position: gui::dpi(16, 94),
                size: gui::dpi(360, 20),
                selected: default == LinkKind::Shortcut,
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
            position: gui::dpi(210, 130),
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
            position: gui::dpi(298, 130),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<LinkKind>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    _arm.plain(
        "link_kind",
        "リンクの作成",
        "作成するリンクの種類を選んで下さい。",
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
    );

    {
        let radios = radios.clone();
        _arm.on_create(move |_| {
            if !symlink_enabled {
                radios[0].hwnd().EnableWindow(false);
            }
            if !junction_enabled {
                radios[1].hwnd().EnableWindow(false);
            }
            Ok(())
        });
    }

    {
        let result = result.clone();
        let radios = radios.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            let kind = radios.selected_index().and_then(|i| KINDS.get(i)).copied().unwrap_or(default);
            *result.borrow_mut() = Some(kind);
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
