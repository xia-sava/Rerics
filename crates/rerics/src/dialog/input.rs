use std::cell::{Cell, RefCell};
use std::rc::Rc;
use winsafe::msg::{em, lb};
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

    result.borrow().clone()
}

/// 複数行のコード入力モーダル。OK ならコード文字列、キャンセル/Esc なら None。
/// キー編集の「コードを割り当て」（`Eval` 割り当て）で、束ねる JS/TS コードを書くのに使う。
/// カレットまでのテキスト `before` の末尾が `r.` / `rerics.` のメンバアクセスなら、その直後の
/// 識別子プレフィックスを返す。返り＝(プレフィックスの UTF-16 長, prefix 文字列)。`=r.cur` なら
/// `(3, "cur")`、`=r.` なら `(0, "")`、`bar.cur` のように手前が別トークンなら `None`。
/// 補完候補の絞り込み（`prefix` で前方一致）と、確定時の置換範囲（UTF-16 長）に使う。
fn completion_prefix(before: &str) -> Option<(usize, String)> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let trailing_ident = |s: &str| -> String {
        let rev: String = s.chars().rev().take_while(|c| is_ident(*c)).collect();
        rev.chars().rev().collect()
    };
    let prefix = trailing_ident(before);
    // prefix は ASCII 識別子なのでバイト長＝文字数＝末尾位置の境界は妥当。
    let head = before[..before.len() - prefix.len()].strip_suffix('.')?;
    // ドットの手前が `r` か `rerics` ちょうど＝別名そのものを指すときだけ補完する。
    if matches!(trailing_ident(head).as_str(), "r" | "rerics") {
        Some((prefix.encode_utf16().count(), prefix))
    } else {
        None
    }
}

/// `members` から `prefix` に前方一致する候補を返す（大小無視・元の順序を保つ）。空 prefix は全件。
fn completion_candidates(members: &[String], prefix: &str) -> Vec<String> {
    let p = prefix.to_lowercase();
    members.iter().filter(|m| m.to_lowercase().starts_with(&p)).cloned().collect()
}

pub fn code_box(
    parent: &impl GuiParent,
    message: &str,
    value: &str,
    members: &[String],
) -> Option<String> {
    let (wnd, arm) = modal_window("コードを割り当て", 480, 400);

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: message,
            position: gui::dpi(16, 14),
            size: gui::dpi(448, 18),
            ..Default::default()
        },
    );

    let edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: value,
            control_style: co::ES::MULTILINE
                | co::ES::WANTRETURN
                | co::ES::AUTOVSCROLL
                | co::ES::NOHIDESEL,
            window_style: co::WS::CHILD
                | co::WS::GROUP
                | co::WS::TABSTOP
                | co::WS::VISIBLE
                | co::WS::BORDER
                | co::WS::VSCROLL,
            position: gui::dpi(16, 38),
            width: gui::dpi_x(448),
            height: gui::dpi_y(150),
            ..Default::default()
        },
    );

    // `r.` 補完の候補リスト。既定は隠しておき、`r.<prefix>` を打つと候補を入れて表示する。
    // 項目のシングルクリックで、カレット直前のプレフィックスをその候補名に置換する。
    let cand = gui::ListBox::new(
        &wnd,
        gui::ListBoxOpts {
            position: gui::dpi(16, 194),
            size: gui::dpi(448, 150),
            window_style: co::WS::CHILD | co::WS::BORDER | co::WS::VSCROLL,
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(290, 356),
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
            position: gui::dpi(378, 356),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    arm.plain(
        "input",
        "コードを割り当て",
        message,
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
    );
    {
        let e = edit.clone();
        arm.on_create(move |_| {
            e.hwnd().SetFocus();
            Ok(())
        });
    }
    {
        let result = result.clone();
        let edit = edit.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            *result.borrow_mut() = Some(edit.text().unwrap_or_default());
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

    let members_rc: Rc<Vec<String>> = Rc::new(members.to_vec());
    // 置換範囲（カレット直前の `r.<prefix>` の prefix 部分・UTF-16 オフセット）。候補表示中だけ Some。
    let range: Rc<Cell<Option<(u32, u32)>>> = Rc::new(Cell::new(None));

    // 候補 idx を確定＝カレット直前のプレフィックスをその名前へ置換し、リストを隠して入力へ戻す。
    let do_insert: Rc<dyn Fn(u32)> = {
        let edit = edit.clone();
        let cand = cand.clone();
        let range = range.clone();
        Rc::new(move |idx: u32| {
            if let (Some((start, end)), Ok(name)) = (range.get(), cand.items().text(idx)) {
                edit.set_selection(start as i32, end as i32);
                edit.replace_selection(&name);
                let _ = cand.hwnd().ShowWindow(co::SW::HIDE);
                range.set(None);
                edit.hwnd().SetFocus();
            }
        })
    };

    // カレットまでの文字列 `before` を見て、`r.<prefix>` なら候補を入れて表示・なければ隠す補完更新。
    // 実入力（EN_CHANGE）と headless 観測（type_text）の両方から、各自の `before` を渡して呼ぶ。
    let update: Rc<dyn Fn(&str)> = {
        let cand = cand.clone();
        let members = members_rc.clone();
        let range = range.clone();
        Rc::new(move |before: &str| {
            let caret = before.encode_utf16().count() as u32;
            // 候補があり、かつ「唯一かつ入力済みと同一」でなければ出す（確定直後の再表示を防ぐ）。
            let shown = completion_prefix(before).and_then(|(plen, prefix)| {
                let list = completion_candidates(&members, &prefix);
                let only_exact = list.len() == 1 && list[0].eq_ignore_ascii_case(&prefix);
                (!(list.is_empty() || only_exact)).then_some((plen as u32, list))
            });
            match shown {
                Some((plen, list)) => {
                    cand.items().delete_all();
                    let _ = cand.items().add(&list);
                    let _ = unsafe { cand.hwnd().SendMessage(lb::SetCurSel { index: Some(0) }) };
                    let _ = cand.hwnd().ShowWindow(co::SW::SHOW);
                    range.set(Some((caret - plen, caret)));
                }
                None => {
                    let _ = cand.hwnd().ShowWindow(co::SW::HIDE);
                    range.set(None);
                }
            }
        })
    };
    {
        let edit2 = edit.clone();
        let update = update.clone();
        edit.on().en_change(move || {
            // 実入力：本文とカレット（EM_GETSEL）からカレット直前の文字列を作る。
            let text = edit2.text().unwrap_or_default();
            let (mut start, mut end) = (0u32, 0u32);
            unsafe {
                edit2.hwnd().SendMessage(em::GetSel {
                    first_index: Some(&mut start),
                    past_last_index: Some(&mut end),
                });
            }
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let caret = (end as usize).min(utf16.len());
            update(&String::from_utf16_lossy(&utf16[..caret]));
            Ok(())
        });
    }

    // 候補のシングルクリック＝その項目を確定する。
    {
        let cand2 = cand.clone();
        let do_insert = do_insert.clone();
        cand.on().lbn_sel_change(move || {
            if let Some(idx) = unsafe { cand2.hwnd().SendMessage(lb::GetCurSel {}) } {
                do_insert(idx);
            }
            Ok(())
        });
    }

    // headless 観測：開いている補完つき入力欄の入力模擬・候補読み取り・確定・本文取得を公開する。
    #[cfg(feature = "debug-server")]
    {
        let cand_p = cand.clone();
        let edit_p = edit.clone();
        let edit_t = edit.clone();
        let do_insert = do_insert.clone();
        let update = update.clone();
        completion_probe::set(completion_probe::Probe {
            type_text: Box::new(move |s| {
                // 入力模擬：本文を s にし、カレットは末尾＝before は全文として補完を更新する。
                let _ = edit_t.set_text(s);
                update(s);
            }),
            candidates: Box::new(move || {
                let n = cand_p.items().count().unwrap_or(0);
                (0..n).filter_map(|i| cand_p.items().text(i).ok()).collect()
            }),
            accept: Box::new(move |idx| do_insert(idx)),
            text: Box::new(move || edit_p.text().unwrap_or_default()),
        });
    }

    let _ = wnd.show_modal(parent);
    #[cfg(feature = "debug-server")]
    completion_probe::clear();
    let _ = (cancel, cand, &do_insert, &update);
    result.borrow().clone()
}

/// 開いている補完つき入力欄（`code_box`）を headless から観測・駆動する窓口。モーダルは UI
/// スレッドの入れ子ループで開くので、その間 debug リクエストはこのプローブ越しに候補を読み・確定できる。
#[cfg(feature = "debug-server")]
pub mod completion_probe {
    use std::cell::RefCell;

    pub struct Probe {
        /// 入力を模擬する：本文を `s` にしてカレットを末尾に置き、補完を更新する。
        pub type_text: Box<dyn Fn(&str)>,
        /// 現在の候補リスト（上から順）。
        pub candidates: Box<dyn Fn() -> Vec<String>>,
        /// idx 番目の候補を確定（カレット直前のプレフィックスを置換）。
        pub accept: Box<dyn Fn(u32)>,
        /// 入力欄の現在の本文。
        pub text: Box<dyn Fn() -> String>,
    }

    thread_local! {
        static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    }

    pub fn set(p: Probe) {
        PROBE.with(|s| *s.borrow_mut() = Some(p));
    }
    pub fn clear() {
        PROBE.with(|s| *s.borrow_mut() = None);
    }
    /// 本文を `s` にして補完を更新する（入力の模擬）。開いていれば `true`。
    pub fn type_text(s: &str) -> bool {
        PROBE.with(|p| p.borrow().as_ref().map(|p| (p.type_text)(s)).is_some())
    }
    /// 候補一覧（入力欄が開いていなければ None）。
    pub fn candidates() -> Option<Vec<String>> {
        PROBE.with(|s| s.borrow().as_ref().map(|p| (p.candidates)()))
    }
    /// idx 番目の候補を確定する。開いていれば `true`。
    pub fn accept(idx: u32) -> bool {
        PROBE.with(|s| {
            s.borrow().as_ref().map(|p| (p.accept)(idx)).is_some()
        })
    }
    /// 入力欄の現在の本文（開いていなければ None）。
    pub fn text() -> Option<String> {
        PROBE.with(|s| s.borrow().as_ref().map(|p| (p.text)()))
    }
}

#[cfg(test)]
mod tests {
    use super::{completion_candidates, completion_prefix};

    #[test]
    fn prefix_detects_r_member_access() {
        assert_eq!(completion_prefix("=r.cur"), Some((3, "cur".into())));
        assert_eq!(completion_prefix("=r."), Some((0, "".into())));
        assert_eq!(completion_prefix("=rerics.fold"), Some((4, "fold".into())));
        // 文の途中でも直前が r. ならよい。
        assert_eq!(completion_prefix("=r.cursorName() + r.cu"), Some((2, "cu".into())));
    }

    #[test]
    fn prefix_rejects_non_r_contexts() {
        // 手前が別トークン（bar）なので補完しない。
        assert_eq!(completion_prefix("bar.cur"), None);
        // ただの識別子・ドット無し。
        assert_eq!(completion_prefix("=current"), None);
        // r だけ（ドット未入力）はまだメンバアクセスでない。
        assert_eq!(completion_prefix("=r"), None);
    }

    #[test]
    fn candidates_filter_prefix_case_insensitive_keep_order() {
        let members =
            vec!["currentDir".to_string(), "cursorItem".to_string(), "prompt".to_string()];
        assert_eq!(completion_candidates(&members, "cur"), vec!["currentDir", "cursorItem"]);
        // 大小無視。
        assert_eq!(completion_candidates(&members, "CUR"), vec!["currentDir", "cursorItem"]);
        // 空 prefix は全件・順序保持。
        assert_eq!(completion_candidates(&members, "").len(), 3);
        assert!(completion_candidates(&members, "xyz").is_empty());
    }
}
