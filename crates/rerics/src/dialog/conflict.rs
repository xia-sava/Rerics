use std::cell::RefCell;
use std::rc::Rc;
use rerics_core::ConflictResolution;
use winsafe::{self as w, co, gui, prelude::*};
use super::*;

/// 原作 `frmCopyOption`（「同名ファイルの処理」）相当。コピー/移動先に同名ファイルが
/// 在るとき、解決方法（最新ならコピー/上書き/強制上書き/名前変更/スキップ）と
/// 「すべてに適用」を尋ねる。OK でラジオ選択＋チェック状態を、キャンセル/Esc で `Cancel` を返す。
pub fn conflict_box(parent: &impl GuiParent, name: &str) -> (ConflictResolution, bool) {
    let (wnd, arm) = modal_window("同名ファイルの処理", 380, 250);

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: name,
            position: gui::dpi(16, 12),
            size: gui::dpi(348, 18),
            ..Default::default()
        },
    );

    let radios = gui::RadioGroup::new(
        &wnd,
        &[
            gui::RadioButtonOpts {
                text: "最新ならコピー(&N)",
                position: gui::dpi(16, 40),
                size: gui::dpi(220, 20),
                selected: true,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "上書き(&O)",
                position: gui::dpi(16, 64),
                size: gui::dpi(220, 20),
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "強制上書き(&F)",
                position: gui::dpi(16, 88),
                size: gui::dpi(220, 20),
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "名前を変更してコピー(&R)",
                position: gui::dpi(16, 112),
                size: gui::dpi(180, 20),
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "スキップ(&K)",
                position: gui::dpi(16, 136),
                size: gui::dpi(220, 20),
                ..Default::default()
            },
        ],
    );

    let rename = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: name,
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(200, 112),
            width: gui::dpi_x(150),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );

    let all = gui::CheckBox::new(
        &wnd,
        gui::CheckBoxOpts {
            text: "すべてに適用(SHIFT)",
            position: gui::dpi(16, 166),
            size: gui::dpi(220, 18),
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(190, 196),
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
            position: gui::dpi(278, 196),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result = Rc::new(RefCell::new((ConflictResolution::Cancel, false)));
    // 改名 Edit の初期キャレットは拡張子の前（原作 RenameStyle・衝突はファイル名）。
    let rename_pos = before_ext_pos(name, false);

    // 「すべてに適用」中は改名ラジオを無効化（改名＋全適用は排他＝原作）。改名 Edit は
    // 「名前を変更してコピー」選択中かつ全適用未チェックのときだけ有効。
    let refresh: Rc<dyn Fn()> = {
        let radios = radios.clone();
        let rename = rename.clone();
        let all = all.clone();
        Rc::new(move || {
            let all_checked = all.is_checked();
            let rename_sel = radios.selected_index() == Some(3);
            radios[3].hwnd().EnableWindow(!all_checked);
            rename.hwnd().EnableWindow(rename_sel && !all_checked);
        })
    };
    {
        let refresh = refresh.clone();
        let radios_c = radios.clone();
        let rename = rename.clone();
        radios.on().bn_clicked(move || {
            refresh();
            // 「名前を変更してコピー」を選んだら改名 Edit へフォーカス＋拡張子前にキャレット（原作）。
            if radios_c.selected_index() == Some(3) {
                rename.hwnd().SetFocus();
                rename.set_selection(rename_pos, rename_pos);
            }
            Ok(())
        });
    }
    {
        let refresh = refresh.clone();
        all.on().bn_clicked(move || {
            refresh();
            Ok(())
        });
    }

    {
        // 作成時：初期の有効/無効を反映し、Shift 連動＋改名欄の上下キーの keyhook を張る。
        let all_k = all.clone();
        let rename_k = rename.clone();
        let radios_k = radios.clone();
        let refresh_c = refresh.clone();
        let rename_sel = rename.clone();
        arm_modal(
            &arm,
            "conflict",
            "同名ファイルの処理",
            name,
            true,
            vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
            move |hwnd| {
                refresh_c();
                rename_sel.set_selection(rename_pos, rename_pos);
                let all_k = all_k.clone();
                let rename_k = rename_k.clone();
                let radios_k = radios_k.clone();
                let refresh_k = refresh_c.clone();
                keyhook::push(hwnd, move |vk, down| {
                    let in_rename =
                        w::HWND::GetFocus().map(|f| f.ptr()) == Some(rename_k.hwnd().ptr());
                    // 改名 Edit 内の上下キー：ラジオ選択へ戻す（↑=強制上書き idx2・↓=スキップ
                    // idx4）。単一行 Edit の上下は元々無動作なので横取りして問題ない。BM_CLICK で
                    // 標準クリック相当（選択＋フォーカス＋BN_CLICKED→refresh）を起こす。
                    if down && in_rename && (vk == 0x26 || vk == 0x28) {
                        let target = if vk == 0x26 { 2 } else { 4 };
                        unsafe {
                            radios_k[target].hwnd().SendMessage(w::msg::bm::Click {});
                        }
                        return;
                    }
                    // 原作 frmCopyOption：Shift 押下中だけ「すべてに適用」を自動チェック。
                    // 改名 Edit 入力中は Shift を無視する。
                    if vk != 0x10 || in_rename {
                        return;
                    }
                    all_k.set_check(down);
                    refresh_k();
                });
                Ok(())
            },
        );
    }

    {
        let result = result.clone();
        let radios = radios.clone();
        let rename = rename.clone();
        let all = all.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            let choice = match radios.selected_index() {
                Some(1) => ConflictResolution::Overwrite,
                Some(2) => ConflictResolution::OverwriteForce,
                Some(3) => ConflictResolution::Rename(rename.text().unwrap_or_default()),
                Some(4) => ConflictResolution::Skip,
                _ => ConflictResolution::Newest,
            };
            *result.borrow_mut() = (choice, all.is_checked());
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
    keyhook::pop();
    let _ = (ok, cancel);
    let r = result.borrow().clone();
    r
}
