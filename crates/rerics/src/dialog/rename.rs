use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::SystemTime;
use rerics_core::{FileAttrs, NameCase, format_local, parse_local};
use winsafe::{self as w, co, gui, prelude::*};
use super::*;

/// 名前・属性・作成/更新日時の変更ダイアログ。`single` が `Some` なら単一対象（名前編集可・
/// 属性とチェックは現在値で初期化・`single_is_dir` がディレクトリ判定）、`None` なら `count`
/// 件への一括（名前は変換メニューで一括・属性は据え置き＝中間状態で初期化）。OK なら
/// [`RenameResult`]、中止/Esc なら `None`。
pub fn rename_box(
    parent: &impl GuiParent,
    single: Option<&str>,
    single_is_dir: bool,
    count: usize,
    attrs: FileAttrs,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
) -> Option<RenameResult> {
    let is_single = single.is_some();
    // 単一ディレクトリ時のみ「サブディレクトリにも適用」チェックを出す（その分縦に広げる）。
    let show_sub = is_single && single_is_dir;
    let win_h = if show_sub { 388 } else { 340 };
    let btn_y = if show_sub { 348 } else { 290 };
    let wnd = modal_window("名前の変更", 360, win_h);

    // 名前行（常設）。単一＝編集可・名前プリフィル、複数＝無効で変換結果ラベルを表示。
    // 右の「...」は名前変換メニュー（原作 btnFileName）。
    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "名前(&N)",
            position: gui::dpi(16, 16),
            size: gui::dpi(76, 18),
            ..Default::default()
        },
    );
    let name_edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: single.unwrap_or(""),
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(96, 14),
            width: gui::dpi_x(212),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let name_btn = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "...",
            ctrl_id: 12,
            position: gui::dpi(312, 14),
            width: gui::dpi_x(26),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    if !is_single {
        let _ = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: &format!("{count} 個の項目に属性／日時／名前を適用します。"),
                position: gui::dpi(16, 40),
                size: gui::dpi(328, 18),
                ..Default::default()
            },
        );
    }

    // 属性チェック群。単一は2状態（現在値で初期化）、一括は3状態（中間＝据え置き）。
    let style = if is_single { co::BS::AUTOCHECKBOX } else { co::BS::AUTO3STATE };
    let init = |on: bool| {
        if is_single {
            if on { co::BST::CHECKED } else { co::BST::UNCHECKED }
        } else {
            co::BST::INDETERMINATE
        }
    };
    let labels = [
        ("読み取り専用(&R)", attrs.readonly),
        ("隠し(&H)", attrs.hidden),
        ("システム(&S)", attrs.system),
        ("アーカイブ(&A)", attrs.archive),
    ];
    let checks: Vec<gui::CheckBox> = labels
        .iter()
        .enumerate()
        .map(|(i, (label, on))| {
            gui::CheckBox::new(
                &wnd,
                gui::CheckBoxOpts {
                    text: label,
                    control_style: style,
                    check_state: init(*on),
                    position: gui::dpi(24, 56 + i as i32 * 26),
                    size: gui::dpi(300, 22),
                    ..Default::default()
                },
            )
        })
        .collect();

    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "日付（YYYY/MM/DD HH:MM:SS・空欄=変更しない）",
            position: gui::dpi(16, 168),
            size: gui::dpi(328, 18),
            ..Default::default()
        },
    );
    // 単一時は現在値でプリフィル、複数一括は空欄（＝据え置き）。更新日付が上・作成日時が下。
    let pre = |t: Option<SystemTime>| match (is_single, t) {
        (true, Some(t)) => format_local(t),
        _ => String::new(),
    };
    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "更新日付(&U)",
            position: gui::dpi(16, 193),
            size: gui::dpi(76, 18),
            ..Default::default()
        },
    );
    let mtime_edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: &pre(modified),
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(96, 190),
            width: gui::dpi_x(212),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let mtime_btn = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "...",
            ctrl_id: 10,
            position: gui::dpi(312, 190),
            width: gui::dpi_x(26),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let _ = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "作成日時(&C)",
            position: gui::dpi(16, 221),
            size: gui::dpi(76, 18),
            ..Default::default()
        },
    );
    let ctime_edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: &pre(created),
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(96, 218),
            width: gui::dpi_x(212),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let ctime_btn = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "...",
            ctrl_id: 11,
            position: gui::dpi(312, 218),
            width: gui::dpi_x(26),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );

    // サブディレクトリ再帰適用（単一ディレクトリ時のみ）。属性用・日時用を独立に持つ。
    let sub_checks: Option<(gui::CheckBox, gui::CheckBox)> = if show_sub {
        let sub_attr = gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "サブディレクトリにも属性を適用(&B)",
                control_style: co::BS::AUTOCHECKBOX,
                position: gui::dpi(24, 254),
                size: gui::dpi(320, 22),
                ..Default::default()
            },
        );
        let sub_time = gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "サブディレクトリにも日時を適用(&G)",
                control_style: co::BS::AUTOCHECKBOX,
                position: gui::dpi(24, 280),
                size: gui::dpi(320, 22),
                ..Default::default()
            },
        );
        Some((sub_attr, sub_time))
    } else {
        None
    };

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(172, btn_y),
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
            position: gui::dpi(260, btn_y),
            width: gui::dpi_x(84),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    quick_time_menu(&mtime_btn, &mtime_edit);
    quick_time_menu(&ctime_btn, &ctime_edit);

    // サブ適用チェック中は名前編集を無効化（原作 CheckSubState）。属性/日時の再帰だけ行う。
    if let Some((sub_attr, sub_time)) = &sub_checks {
        let refresh = {
            let name_edit = name_edit.clone();
            let name_btn = name_btn.clone();
            let sub_attr = sub_attr.clone();
            let sub_time = sub_time.clone();
            move || {
                let on = sub_attr.is_checked() || sub_time.is_checked();
                name_edit.hwnd().EnableWindow(!on);
                name_btn.hwnd().EnableWindow(!on);
            }
        };
        {
            let refresh = refresh.clone();
            sub_attr.on().bn_clicked(move || {
                refresh();
                Ok(())
            });
        }
        {
            let refresh = refresh.clone();
            sub_time.on().bn_clicked(move || {
                refresh();
                Ok(())
            });
        }
    }

    // 名前変換の選択（複数一括時のみ保持。単一は即時に名前欄へ反映）。
    let name_case: Rc<Cell<NameCase>> = Rc::new(Cell::new(NameCase::None));
    // (種別, メニューラベル)。単一は先頭「何もしない」を出さない（原作準拠）。
    let case_entries = [
        (NameCase::None, "何もしない"),
        (NameCase::Upper, "すべて大文字にする"),
        (NameCase::Lower, "すべて小文字にする"),
        (NameCase::ExtUpper, "拡張子を大文字にする"),
        (NameCase::ExtLower, "拡張子を小文字にする"),
    ];
    {
        let name_edit = name_edit.clone();
        let name_btnf = name_btn.clone();
        let name_case = name_case.clone();
        name_btn.on().bn_clicked(move || {
            let mut menu = w::HMENU::CreatePopupMenu()?;
            let cur = name_case.get();
            for (i, (kind, label)) in case_entries.iter().enumerate() {
                if is_single && *kind == NameCase::None {
                    continue;
                }
                let mut flags = co::MF::STRING;
                if !is_single && *kind == cur {
                    flags |= co::MF::CHECKED;
                }
                menu.AppendMenu(
                    flags,
                    w::IdMenu::Id((i + 1) as u16),
                    w::BmpPtrStr::from_str(label),
                )?;
            }
            let rc = name_btnf.hwnd().GetWindowRect()?;
            let chosen = menu.TrackPopupMenu(
                co::TPM::RETURNCMD | co::TPM::LEFTALIGN | co::TPM::TOPALIGN,
                w::POINT::with(rc.left, rc.bottom),
                name_btnf.hwnd(),
            )?;
            menu.DestroyMenu()?;
            let Some(id) = chosen else {
                return Ok(());
            };
            let kind =
                case_entries.get((id - 1) as usize).map(|(k, _)| *k).unwrap_or(NameCase::None);
            if is_single {
                // 即時変換（「何もしない」は単一では出さない）。
                let next = kind.apply(&name_edit.text()?, single_is_dir);
                name_edit.set_text(&next)?;
                let end = next.encode_utf16().count() as i32;
                name_edit.set_selection(end, end);
            } else {
                name_case.set(kind);
                // 無効な名前欄に選んだ変換のラベルを表示（何もしない＝空）。
                let label = if kind == NameCase::None { "" } else { case_entries[(id - 1) as usize].1 };
                name_edit.set_text(label)?;
            }
            Ok(())
        });
    }

    let result: Rc<RefCell<Option<RenameResult>>> = Rc::new(RefCell::new(None));

    // 単一＝名前 Edit の初期キャレットを末尾（選択なし）に＝従来どおりの手触り。
    // 複数＝名前 Edit を無効化（名前は変換メニュー専用）し、フォーカスを属性へ移す。
    // arm_modal は focus_initial の後に on_create を呼ぶので、ここで設定すれば残る。
    let name_init = name_edit.clone();
    let first_check = checks[0].clone();
    arm_modal(
        &wnd,
        "rename",
        "名前の変更",
        "名前/属性/日時の変更",
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
        move |_| {
            if is_single {
                // 拡張子の前にキャレット（原作 RenameStyle・ディレクトリは末尾）。
                if let Ok(t) = name_init.text() {
                    let pos = before_ext_pos(&t, single_is_dir);
                    name_init.set_selection(pos, pos);
                }
            } else {
                name_init.hwnd().EnableWindow(false);
                first_check.hwnd().SetFocus();
            }
        },
    );
    {
        let result = result.clone();
        let wnd2 = wnd.clone();
        let checks = checks.clone();
        let name_edit = name_edit.clone();
        let mtime_edit = mtime_edit.clone();
        let ctime_edit = ctime_edit.clone();
        let name_case = name_case.clone();
        let sub_checks = sub_checks.clone();
        ok.on().bn_clicked(move || {
            let name = if is_single {
                name_edit.text().ok().map(|s| s.trim().to_owned())
            } else {
                None
            };
            let attrs = [
                cb_tristate(&checks[0]),
                cb_tristate(&checks[1]),
                cb_tristate(&checks[2]),
                cb_tristate(&checks[3]),
            ];
            let parse_time = |e: &gui::Edit| {
                e.text().ok().and_then(|s| {
                    let s = s.trim();
                    if s.is_empty() { None } else { parse_local(s) }
                })
            };
            let modified = parse_time(&mtime_edit);
            let created = parse_time(&ctime_edit);
            let (sub_attr, sub_time) = match &sub_checks {
                Some((a, t)) => (a.is_checked(), t.is_checked()),
                None => (false, false),
            };
            *result.borrow_mut() = Some(RenameResult {
                name,
                attrs,
                modified,
                created,
                name_case: name_case.get(),
                sub_attr,
                sub_time,
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
    disarm_modal();
    let _ = (
        ok, cancel, checks, name_edit, mtime_edit, ctime_edit, mtime_btn, ctime_btn, name_btn,
        sub_checks,
    );
    result.borrow_mut().take()
}
