use std::cell::RefCell;
use std::rc::Rc;

use winsafe::{co, gui, prelude::*};

/// 1行テキスト入力モーダルダイアログ。OK で入力文字列、キャンセル/Esc で None を返す。
pub struct InputDialog {
    wnd: gui::WindowModal,
    result: Rc<RefCell<Option<String>>>,
}

impl InputDialog {
    /// タイトル・プロンプト・初期文字列を指定してダイアログを構築する。
    pub fn new(title: &str, prompt: &str, initial: &str) -> Self {
        let wnd = gui::WindowModal::new(gui::WindowModalOpts {
            title,
            size: gui::dpi(340, 140),
            style: co::WS::CAPTION | co::WS::SYSMENU | co::WS::BORDER | co::WS::VISIBLE,
            process_dlg_msgs: true,
            ..Default::default()
        });

        let _label = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: prompt,
                position: gui::dpi(12, 12),
                size: gui::dpi(316, 18),
                ..Default::default()
            },
        );

        let edit = gui::Edit::new(
            &wnd,
            gui::EditOpts {
                text: initial,
                position: gui::dpi(12, 36),
                width: gui::dpi_x(316),
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
                position: gui::dpi(150, 76),
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
                position: gui::dpi(238, 76),
                width: gui::dpi_x(90),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );

        let result = Rc::new(RefCell::new(None));

        {
            let edit = edit.clone();
            wnd.on().wm_create(move |_| {
                edit.hwnd().SetFocus();
                Ok(0)
            });
        }

        {
            let result = result.clone();
            let edit = edit.clone();
            let wnd = wnd.clone();
            ok.on().bn_clicked(move || {
                *result.borrow_mut() = Some(edit.text().unwrap_or_default());
                wnd.close();
                Ok(())
            });
        }

        {
            let wnd = wnd.clone();
            cancel.on().bn_clicked(move || {
                wnd.close();
                Ok(())
            });
        }

        Self { wnd, result }
    }

    /// 親ウィンドウ中央にモーダル表示し、OK なら入力文字列、キャンセル/Esc なら None を返す。
    pub fn show(self, parent: &impl GuiParent) -> Option<String> {
        let _ = self.wnd.show_modal(parent);
        self.result.borrow().clone()
    }
}

/// 「はい / いいえ」確認モーダル。はい(Enter)で true、いいえ(Esc/閉じる)で false を返す。
/// 既定は「いいえ」（何も選ばず閉じた場合も false）。
pub struct ConfirmDialog {
    wnd: gui::WindowModal,
    result: Rc<RefCell<bool>>,
}

impl ConfirmDialog {
    /// タイトル・本文を指定して構築する。
    pub fn new(title: &str, message: &str) -> Self {
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
                position: gui::dpi(16, 20),
                size: gui::dpi(328, 56),
                ..Default::default()
            },
        );

        let yes = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "はい(&Y)",
                control_style: co::BS::DEFPUSHBUTTON,
                ctrl_id: 1,
                position: gui::dpi(92, 96),
                width: gui::dpi_x(80),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );

        let no = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "いいえ(&N)",
                ctrl_id: 2,
                position: gui::dpi(180, 96),
                width: gui::dpi_x(88),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );

        let result = Rc::new(RefCell::new(false));

        {
            let yes = yes.clone();
            wnd.on().wm_create(move |_| {
                yes.hwnd().SetFocus();
                Ok(0)
            });
        }

        {
            let result = result.clone();
            let wnd = wnd.clone();
            yes.on().bn_clicked(move || {
                *result.borrow_mut() = true;
                wnd.close();
                Ok(())
            });
        }

        {
            let wnd = wnd.clone();
            no.on().bn_clicked(move || {
                wnd.close();
                Ok(())
            });
        }

        Self { wnd, result }
    }

    /// 親ウィンドウ中央にモーダル表示し、はいなら true、いいえ/Esc なら false を返す。
    pub fn show(self, parent: &impl GuiParent) -> bool {
        let _ = self.wnd.show_modal(parent);
        *self.result.borrow()
    }
}
