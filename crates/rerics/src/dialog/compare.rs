use std::cell::RefCell;
use std::rc::Rc;
use rerics_core::{CompareCondition, CompareOptions};
use winsafe::{co, gui, prelude::*};
use super::*;

/// 日付の比較条件ラジオ（原作 frmDirectoryCompare の並び）。既定は「不一致」。
const DATE_KINDS: &[(&str, CompareCondition)] = &[
    ("無視", CompareCondition::None),
    ("不一致", CompareCondition::NotEquals),
    ("一致", CompareCondition::Equals),
    ("新しい", CompareCondition::Less),
    ("古い", CompareCondition::Greater),
];

/// サイズの比較条件ラジオ。既定は「無視」。
const SIZE_KINDS: &[(&str, CompareCondition)] = &[
    ("無視", CompareCondition::None),
    ("不一致", CompareCondition::NotEquals),
    ("一致", CompareCondition::Equals),
    ("小さい", CompareCondition::Less),
    ("大きい", CompareCondition::Greater),
];

/// ディレクトリ比較の条件ダイアログ（原作 frmDirectoryCompare 相当）。日付・サイズの比較条件と
/// ディレクトリ再帰・追加/削除の抽出を選ばせ、OK なら [`CompareOptions`] を返す。中止/Esc は `None`。
pub fn compare_options_box(parent: &impl GuiParent) -> Option<CompareOptions> {
    let (wnd, arm) = modal_window("ディレクトリ比較", 360, 240);

    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "日付",
            position: gui::dpi(16, 10),
            size: gui::dpi(320, 16),
            ..Default::default()
        },
    );
    let date = radio_row(&wnd, DATE_KINDS, 30, 1);

    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "ファイルサイズ",
            position: gui::dpi(16, 56),
            size: gui::dpi(320, 16),
            ..Default::default()
        },
    );
    let size = radio_row(&wnd, SIZE_KINDS, 76, 0);

    let dir = check(&wnd, "ディレクトリ内も検索(&D)", 110, true);
    let exist = check(&wnd, "追加ファイルの抽出(&1)", 134, true);
    let not_exist = check(&wnd, "削除ファイルの抽出(&2)", 158, true);

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(180, 200),
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
            position: gui::dpi(268, 200),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<CompareOptions>>> = Rc::new(RefCell::new(None));

    arm_modal(
        &arm,
        "directory_compare",
        "ディレクトリ比較",
        "日付・サイズの比較条件と抽出範囲",
        false,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
        |_| Ok(()),
    );
    {
        let result = result.clone();
        let (date, size) = (date.clone(), size.clone());
        let (dir, exist, not_exist) = (dir.clone(), exist.clone(), not_exist.clone());
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            *result.borrow_mut() = Some(CompareOptions {
                date: selected(&date, DATE_KINDS),
                size: selected(&size, SIZE_KINDS),
                recurse: dir.is_checked(),
                show_added: exist.is_checked(),
                show_deleted: not_exist.is_checked(),
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

    super::show_modal_guarded(&wnd, parent);
    let _ = (ok, cancel);

    *result.borrow()
}

/// ラベル配列から横並びのラジオグループを作る（`selected` 番目を初期選択）。
fn radio_row(wnd: &gui::WindowModal, kinds: &[(&str, CompareCondition)], y: i32, selected: usize) -> gui::RadioGroup {
    gui::RadioGroup::new(
        wnd,
        &kinds
            .iter()
            .enumerate()
            .map(|(i, (label, _))| gui::RadioButtonOpts {
                text: label,
                position: gui::dpi(16 + i as i32 * 66, y),
                size: gui::dpi(64, 20),
                selected: i == selected,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )
}

/// チェックボックスを1つ作る。
fn check(wnd: &gui::WindowModal, text: &str, y: i32, checked: bool) -> gui::CheckBox {
    gui::CheckBox::new(
        wnd,
        gui::CheckBoxOpts {
            text,
            position: gui::dpi(16, y),
            size: gui::dpi(320, 18),
            check_state: if checked { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        },
    )
}

/// ラジオグループの選択を対応する [`CompareCondition`] へ（未選択なら先頭＝無視）。
fn selected(group: &gui::RadioGroup, kinds: &[(&str, CompareCondition)]) -> CompareCondition {
    group
        .selected_index()
        .and_then(|i| kinds.get(i))
        .map(|(_, c)| *c)
        .unwrap_or(CompareCondition::None)
}
