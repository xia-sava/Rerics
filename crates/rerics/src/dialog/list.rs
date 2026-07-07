use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{self as w, co, gui, prelude::*};

/// 一覧から1つ選ぶモーダル。OK・ダブルクリック・Enter で選択 index を、キャンセル/Esc で
/// `None` を返す。`items` は表示行、`initial` は初期選択行。ドライブ選択・履歴・
/// 登録ディレクトリのジャンプで共用する。
pub fn list_box(
    parent: &impl GuiParent,
    title: &str,
    key: &'static str,
    items: &[String],
    initial: usize,
) -> Option<usize> {
    let (wnd, arm) = super::modal_window_resizable_keyed(title, key, 420, 320, 280, 200);

    let list = gui::ListBox::new(
        &wnd,
        gui::ListBoxOpts {
            position: gui::dpi(16, 14),
            size: gui::dpi(388, 250),
            // 縦スクロールを付ける。これが無いと項目が窓に収まらなくてもスクロール範囲が生まれず、
            // ネイティブのホイールスクロールも効かない（機能ピッカーのような長い一覧で必須）。
            window_style: co::WS::CHILD
                | co::WS::GROUP
                | co::WS::TABSTOP
                | co::WS::VISIBLE
                | co::WS::VSCROLL,
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
                super::relayout_list_dialog(
                    lst.hwnd(),
                    16,
                    26,
                    &[(cancelc.hwnd(), 86), (okc.hwnd(), 80)],
                    rc.right,
                    rc.bottom,
                );
            }
            Ok(())
        });
    }

    // ホイールスクロールは ListBox の `WS_VSCROLL` でネイティブに効くが、フォーカスがボタン側に
    // あるなどでホイールが一覧でなくモーダル窓へ伝播してきた場合の保険として、ここでも回転量ぶん
    // 先頭行をずらす。
    {
        let list = list.clone();
        wnd.on().wm_mouse_wheel(move |p| {
            // winsafe 0.0.27 は回転量を keys 側（HIWORD）へ入れて渡す。
            let notches = (p.keys.raw() as i16 as i32) / 120;
            if notches != 0 {
                let lines = notches * crate::file_list::os_wheel_scroll_lines() as i32;
                unsafe {
                    let top = list.hwnd().SendMessage(w::msg::lb::GetTopIndex {}).unwrap_or(0) as i32;
                    let count = list.hwnd().SendMessage(w::msg::lb::GetCount {}).unwrap_or(0) as i32;
                    let _ = list.hwnd().SendMessage(w::msg::lb::SetTopIndex {
                        index: scrolled_top(top, count, lines) as u32,
                    });
                }
            }
            Ok(())
        });
    }

    super::show_modal_guarded(&wnd, parent);
    let _ = (ok, cancel, list);

    *result.borrow()
}

/// ホイール回転で動かした先頭行を求める。`lines` 正＝上へ（先頭を小さく）、負＝下へ。
/// 末尾を越えない・先頭を割らないようクランプする（空リストは 0）。
fn scrolled_top(top: i32, count: i32, lines: i32) -> i32 {
    (top - lines).clamp(0, (count - 1).max(0))
}

#[cfg(test)]
mod tests {
    use super::scrolled_top;

    #[test]
    fn wheel_scroll_clamps_within_list() {
        // 上回転（lines 正）で先頭が前へ、下回転で後ろへ。
        assert_eq!(scrolled_top(10, 100, 3), 7, "上へ3行");
        assert_eq!(scrolled_top(10, 100, -3), 13, "下へ3行");
        // 先頭・末尾でクランプ。
        assert_eq!(scrolled_top(1, 100, 5), 0, "先頭を割らない");
        assert_eq!(scrolled_top(98, 100, -5), 99, "末尾を越えない");
        // 空リストは 0。
        assert_eq!(scrolled_top(0, 0, -5), 0, "空は 0");
    }
}
