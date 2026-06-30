use winsafe::{co, gui, prelude::*};

/// バージョン情報モーダル。`body` を読み取り専用の複数行テキストボックスに表示する
/// （アプリ情報＋サードパーティライセンス一覧）。閉じる／× で終わる。
pub fn about_box(parent: &impl GuiParent, title: &str, body: &str) {
    let (wnd, arm) = super::modal_window_sysmenu(title, 580, 460);

    let text = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            position: gui::dpi(12, 12),
            width: gui::dpi_x(556),
            height: gui::dpi_y(396),
            // 整形（ライセンスの体裁・ASCII 見出し）を崩さないよう折返しさせず、縦横スクロールを付ける。
            control_style: co::ES::MULTILINE
                | co::ES::READONLY
                | co::ES::AUTOHSCROLL
                | co::ES::AUTOVSCROLL
                | co::ES::NOHIDESEL,
            window_style: co::WS::CHILD
                | co::WS::GROUP
                | co::WS::TABSTOP
                | co::WS::VISIBLE
                | co::WS::BORDER
                | co::WS::VSCROLL
                | co::WS::HSCROLL,
            ..Default::default()
        },
    );

    let close = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "閉じる",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(488, 418),
            width: gui::dpi_x(80),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );

    #[cfg(feature = "debug-server")]
    arm.plain("about", title, body, false, vec![("閉じる".to_string(), 1u16)]);

    {
        let text = text.clone();
        let body = body.to_string();
        arm.on_create(move |_| {
            // 既定の文字数上限を外してから大きな本文を流し込む。
            text.limit_text(None);
            text.set_text(&body)?;
            text.set_selection(0, 0);
            text.hwnd().SetFocus();
            Ok(())
        });
    }

    {
        let wnd2 = wnd.clone();
        close.on().bn_clicked(move || {
            wnd2.close();
            Ok(())
        });
    }

    let _ = wnd.show_modal(parent);
    let _ = (text, close);
}
