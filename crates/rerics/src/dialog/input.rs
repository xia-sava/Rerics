use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{co, gui, prelude::*};
use super::*;

/// 原作 `PluginMessage.Input` 相当。メッセージ＋1行入力のモーダルを表示し、
/// OK なら入力文字列、キャンセル/Esc なら None を返す。初期選択は従来どおり（`AsIs`）。
pub fn input_box(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    value: &str,
    mode: InputMode,
) -> Option<String> {
    input_box_full(parent, title, message, value, mode, InputSelect::AsIs, None)
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
    input_box_full(parent, title, message, value, mode, select, None)
}

/// [`input_box`] に履歴（用途キー別の過去入力）を加えた版。入力欄を編集可能コンボにし、
/// `history`（新しい順）を候補に出す。`history` が `Some` のときのみコンボ・`None` は素の Edit。
/// 原作 MessageForm の cboInput（Key 指定時の履歴コンボ）相当。`select` はコンボでは無視する。
/// 履歴への追加・保存は呼び出し側（[`crate::MainWindow`]）が担う。
pub fn input_box_full(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    value: &str,
    mode: InputMode,
    select: InputSelect,
    history: Option<&[&str]>,
) -> Option<String> {
    let (wnd, arm) = modal_window(title, 360, 150);

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: message,
            position: gui::dpi(16, 14),
            size: gui::dpi(328, 18),
            ..Default::default()
        },
    );

    // 履歴ありかつ Plain のときだけ編集可能コンボ（候補＝履歴・新しい順）。
    // それ以外（履歴なし・パスワード）は従来どおり素の Edit。
    let combo_history = match mode {
        InputMode::Plain => history,
        InputMode::Password => None,
    };
    // 入力欄の現在値の読み取り・初期化（フォーカス＋初期値/選択）を、コンボ/Edit 共通の
    // クロージャに包んで後段から扱う。コントロール実体は `_keep` で生存させる。
    let read: Rc<dyn Fn() -> String>;
    let on_create: Box<dyn Fn()>;
    let _keep: Box<dyn std::any::Any>;
    if let Some(items) = combo_history {
        let combo = gui::ComboBox::new(
            &wnd,
            gui::ComboBoxOpts {
                control_style: co::CBS::DROPDOWN,
                position: gui::dpi(16, 38),
                width: gui::dpi_x(328),
                items,
                ..Default::default()
            },
        );
        let r = combo.clone();
        read = Rc::new(move || r.hwnd().GetWindowText().unwrap_or_default());
        let f = combo.clone();
        let v = value.to_string();
        on_create = Box::new(move || {
            let _ = f.hwnd().SetWindowText(&v);
            let _ = f.hwnd().SetFocus();
        });
        _keep = Box::new(combo);
    } else {
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
        let r = edit.clone();
        read = Rc::new(move || r.text().unwrap_or_default());
        let f = edit.clone();
        let v = value.to_string();
        on_create = Box::new(move || {
            f.hwnd().SetFocus();
            select.apply(&f, &v);
        });
        _keep = Box::new(edit);
    }

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
    arm.plain(
        "input",
        title,
        message,
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
    );
    arm.on_create(move |_| {
        on_create();
        Ok(())
    });

    {
        let result = result.clone();
        let read = read.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            *result.borrow_mut() = Some(read());
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
    let _ = cancel;
    let r = result.borrow().clone();
    r
}
