use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{self as w, co, gui, prelude::*};
use crate::MainWindow;

impl MainWindow {
    pub(crate) fn mask(&self, is_left: bool) -> &Rc<RefCell<Option<String>>> {
        if is_left { &self.left_mask } else { &self.right_mask }
    }

    /// 入力ダイアログでパスマスクを尋ね、表示フィルタを設定/解除して一覧を更新する。
    pub(crate) fn path_mask(&self, is_left: bool) -> w::AnyResult<()> {
        let cur = self.mask(is_left).borrow().clone().unwrap_or_default();
        let input = self.input_with_history(
            "パスマスク",
            "表示するマスク（* で解除・カンマ区切り）:",
            &cur,
            "pathmask",
        );
        let Some(input) = input else {
            return Ok(());
        };
        let input = input.trim();
        if input.is_empty() || input == "*" {
            *self.mask(is_left).borrow_mut() = None;
        } else {
            *self.mask(is_left).borrow_mut() = Some(input.to_owned());
        }
        self.reload_side(is_left)?;
        Ok(())
    }

    /// 入力ダイアログでマスクを尋ね、一致するファイルの選択状態を立てる。
    pub(crate) fn select_mask(&self, is_left: bool) -> w::AnyResult<()> {
        let input = self.input_with_history(
            "マスクで選択",
            "選択するマスク（カンマ区切り）:",
            "",
            "selectmask",
        );
        let Some(input) = input else {
            return Ok(());
        };
        let input = input.trim();
        if input.is_empty() {
            return Ok(());
        }
        {
            let state = self.view(is_left).state();
            let mut s = state.borrow_mut();
            for it in &mut s.items {
                if !it.is_parent && rerics_core::glob_match(&it.name, input) {
                    it.selected = true;
                }
            }
        }
        self.view(is_left).refresh()?;
        self.update_selected_info(is_left);
        Ok(())
    }

    /// インクリメンタルサーチ。小さな入力モーダルを出し、打鍵ごとに先頭から一致を
    /// 探してアクティブペインのカーソルを動かす（追従）。OK で確定、中止/Esc で元へ戻す。
    pub(crate) fn incremental_search(&self, is_left: bool) -> w::AnyResult<()> {
        let origin = self.view(is_left).state().borrow().cursor;

        let (wnd, arm) = crate::dialog::modal_window("インクリメンタルサーチ", 320, 96);

        let _label = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: "検索文字（打鍵でカーソルが追従）:",
                position: gui::dpi(12, 10),
                size: gui::dpi(296, 16),
                ..Default::default()
            },
        );

        let edit = gui::Edit::new(
            &wnd,
            gui::EditOpts {
                control_style: co::ES::AUTOHSCROLL,
                position: gui::dpi(12, 30),
                width: gui::dpi_x(296),
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
                position: gui::dpi(150, 60),
                width: gui::dpi_x(76),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );

        let cancel = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "中止(&S)",
                ctrl_id: 2,
                position: gui::dpi(232, 60),
                width: gui::dpi_x(76),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );

        // 打鍵追従：テキスト変化のたびに先頭から検索してカーソルを移す。
        {
            let this = self.clone();
            let edit2 = edit.clone();
            edit.on().en_change(move || {
                let q = edit2.text().unwrap_or_default();
                this.incremental_apply(is_left, &q);
                Ok(())
            });
        }

        #[cfg(feature = "debug-server")]
        arm.plain(
            "incremental",
            "インクリメンタルサーチ",
            "",
            true,
            vec![("OK".to_string(), 1u16), ("中止(&S)".to_string(), 2u16)],
        );
        {
            let edit2 = edit.clone();
            arm.on_create(move |_| {
                edit2.hwnd().SetFocus();
                Ok(())
            });
        }

        {
            let wnd2 = wnd.clone();
            ok.on().bn_clicked(move || {
                wnd2.close();
                Ok(())
            });
        }
        {
            let this = self.clone();
            let wnd2 = wnd.clone();
            cancel.on().bn_clicked(move || {
                // 中止＝開始時のカーソルへ戻す。
                this.move_cursor_to(is_left, origin);
                wnd2.close();
                Ok(())
            });
        }

        let _ = wnd.show_modal(&self.wnd);
        let _ = (edit, ok, cancel);
        Ok(())
    }

    /// インクリメンタルサーチの1打鍵分：先頭から `query` の一致を探してカーソル移動。
    pub(crate) fn incremental_apply(&self, is_left: bool, query: &str) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        let found = {
            let state = view.state();
            let s = state.borrow();
            rerics_core::find_match(&s.items, 0, query, true, false)
        };
        if let Some(i) = found {
            {
                let state = view.state();
                let mut s = state.borrow_mut();
                s.set_cursor(i as isize, pr);
                s.center_cursor(pr);
            }
            let _ = view.refresh();
        }
    }

    /// 指定ペインのカーソルを `idx` に移して再描画する。
    pub(crate) fn move_cursor_to(&self, is_left: bool, idx: usize) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        {
            let state = view.state();
            let mut s = state.borrow_mut();
            s.set_cursor(idx as isize, pr);
        }
        let _ = view.refresh();
    }
}
