use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{co, gui, prelude::*};
use super::*;

/// 圧縮の入力ダイアログ。書庫名（履歴コンボ）・形式（zip / 7z / xz）・「個別に圧縮する」
/// を尋ね、OK なら [`CompressChoice`]、キャンセル/Esc なら `None` を返す。
///
/// 実際に作る形式は最終的な**名前の拡張子**で決まる。形式ラジオは名前欄が未編集のあいだだけ
/// 拡張子を差し替える“種”で、一度でも手入力すると以後は名前欄に触らない。`defaults` は各形式の
/// 既定フル名（xz は対象の束ね要否で `.xz`／`.tar.xz`）。個別圧縮を選ぶと名前欄は使わない
/// （各項目が形式ごとの拡張子になる）ので名前欄を無効化する。`history` は書庫名の候補（新しい順）。
pub fn compress_box(
    parent: &impl GuiParent,
    defaults: &CompressDefaults,
    history: &[&str],
) -> Option<CompressChoice> {
    let (wnd, arm) = modal_window("圧縮", 380, 208);

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "圧縮ファイル名を入力して下さい。",
            position: gui::dpi(16, 14),
            size: gui::dpi(348, 18),
            ..Default::default()
        },
    );

    let combo = gui::ComboBox::new(
        &wnd,
        gui::ComboBoxOpts {
            control_style: co::CBS::DROPDOWN,
            position: gui::dpi(16, 38),
            width: gui::dpi_x(348),
            items: history,
            ..Default::default()
        },
    );

    let _fmt_label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "形式",
            position: gui::dpi(16, 74),
            size: gui::dpi(36, 18),
            ..Default::default()
        },
    );
    let formats = gui::RadioGroup::new(
        &wnd,
        &[
            gui::RadioButtonOpts {
                text: "&zip",
                position: gui::dpi(56, 72),
                size: gui::dpi(64, 20),
                selected: true,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "&7z",
                position: gui::dpi(132, 72),
                size: gui::dpi(64, 20),
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "&xz",
                position: gui::dpi(208, 72),
                size: gui::dpi(64, 20),
                ..Default::default()
            },
        ],
    );

    let one_by_one = gui::CheckBox::new(
        &wnd,
        gui::CheckBoxOpts {
            text: "選択項目を個別に圧縮する(&O)",
            position: gui::dpi(16, 104),
            size: gui::dpi(348, 22),
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(190, 142),
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
            position: gui::dpi(278, 142),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<CompressChoice>>> = Rc::new(RefCell::new(None));
    // 名前欄が「自動生成の既定のまま」かを判定するための最後に入れた既定値。
    let last_auto = Rc::new(RefCell::new(defaults.zip.clone()));

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
        let default = defaults.zip.clone();
        arm.on_create(move |_| {
            let _ = combo_c.hwnd().SetWindowText(&default);
            let _ = combo_c.hwnd().SetFocus();
            Ok(())
        });
    }

    // 形式ラジオの切替：名前欄が未編集（＝最後に入れた既定のまま）ならその形式の既定名へ
    // 差し替える。手入力で変わっていたら触らない。
    {
        let combo_c = combo.clone();
        let formats_c = formats.clone();
        let defaults = defaults.clone();
        let last_auto = last_auto.clone();
        formats.on().bn_clicked(move || {
            let fmt = format_of(formats_c.selected_index().unwrap_or(0));
            let next = default_name(&defaults, fmt).to_string();
            let cur = combo_c.hwnd().GetWindowText().unwrap_or_default();
            if cur == *last_auto.borrow() {
                let _ = combo_c.hwnd().SetWindowText(&next);
                *last_auto.borrow_mut() = next;
            }
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
        let formats_c = formats.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            *result.borrow_mut() = Some(CompressChoice {
                name: combo_c.hwnd().GetWindowText().unwrap_or_default(),
                one_by_one: obo.is_checked(),
                format: format_of(formats_c.selected_index().unwrap_or(0)),
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

    result.borrow().clone()
}

/// ラジオの選択 index を [`CompressFormat`] へ（0=zip・1=7z・2=xz）。
fn format_of(index: usize) -> CompressFormat {
    match index {
        1 => CompressFormat::SevenZ,
        2 => CompressFormat::Xz,
        _ => CompressFormat::Zip,
    }
}

/// 形式に対応する既定のフル名。
fn default_name(defaults: &CompressDefaults, fmt: CompressFormat) -> &str {
    match fmt {
        CompressFormat::Zip => &defaults.zip,
        CompressFormat::SevenZ => &defaults.sevenz,
        CompressFormat::Xz => &defaults.xz,
    }
}
