use std::cell::{Cell, RefCell};
use std::rc::Rc;
use winsafe::msg::lb;
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

/// Edit のカレット位置（UTF-16 オフセット）を返す。winsafe の `em::GetSel` はポインタ出力の
/// マーシャリングが効かない（常に 0）ので、EM_GETSEL の戻り値（上位 16bit＝選択終端＝カレット）を読む。
fn caret_offset(edit: &gui::Edit) -> u32 {
    struct GetSelRaw;
    impl winsafe::prelude::MsgSend for GetSelRaw {
        type RetType = u32;
        unsafe fn isize_to_ret(&self, v: isize) -> u32 {
            (v as u32) >> 16
        }
        fn as_generic_wm(&mut self) -> winsafe::msg::WndMsg {
            winsafe::msg::WndMsg { msg_id: co::EM::GETSEL.into(), wparam: 0, lparam: 0 }
        }
    }
    unsafe { edit.hwnd().SendMessage(GetSelRaw) }
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
    // 本文とカレット位置から「カレット直前の文字列」を見て候補を出し入れする。`force` は Ctrl+Space
    // の明示トリガ用＝「唯一かつ入力済みと同一」でも表示する。文字入力・カレット移動の両方から呼ぶ。
    type Update = Rc<dyn Fn(bool)>;
    let update: Update = {
        let edit = edit.clone();
        let cand = cand.clone();
        let members = members_rc.clone();
        let range = range.clone();
        Rc::new(move |force: bool| {
            let utf16: Vec<u16> = edit.text().unwrap_or_default().encode_utf16().collect();
            let caret = (caret_offset(&edit) as usize).min(utf16.len()) as u32;
            let before = String::from_utf16_lossy(&utf16[..caret as usize]);
            // 候補があり、かつ（強制でなければ）「唯一かつ入力済みと同一」でなければ出す。
            let shown = completion_prefix(&before).and_then(|(plen, prefix)| {
                let list = completion_candidates(&members, &prefix);
                let only_exact = list.len() == 1 && list[0].eq_ignore_ascii_case(&prefix);
                (!(list.is_empty() || (!force && only_exact))).then_some((plen as u32, list))
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
        let update = update.clone();
        edit.on().en_change(move || {
            update(false);
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

    // 生成時：入力欄へフォーカスし、子コントロールのキーを横取りして補完候補を操作する。
    // ↑↓＝候補移動（クランプ）・Enter＝確定・Ctrl+Space＝補完を開く。いずれも Edit へは渡さない。
    {
        let edit_focus = edit.clone();
        let edit_h = edit.clone();
        let cand_h = cand.clone();
        let range = range.clone();
        let update = update.clone();
        let do_insert = do_insert.clone();
        let ctrl = Rc::new(Cell::new(false));
        let suppress_space = Rc::new(Cell::new(false));
        arm.on_create(move |hwnd| {
            edit_focus.hwnd().SetFocus();
            let edit_h = edit_h.clone();
            let cand_h = cand_h.clone();
            let range = range.clone();
            let update = update.clone();
            let do_insert = do_insert.clone();
            let ctrl = ctrl.clone();
            let suppress_space = suppress_space.clone();
            keyhook::push(hwnd, move |msg, wparam| {
                let vk = wparam as u16;
                // Ctrl 状態の追跡（VK_CONTROL=0x11）。消費しない。
                if vk == 0x11 {
                    if msg == keyhook::WM_KEYDOWN {
                        ctrl.set(true);
                    } else if msg == keyhook::WM_KEYUP {
                        ctrl.set(false);
                    }
                    return false;
                }
                // 入力欄にフォーカスがある時だけ補完操作を扱う。
                if winsafe::HWND::GetFocus().map(|f| f.ptr()) != Some(edit_h.hwnd().ptr()) {
                    return false;
                }
                // Ctrl+Space（VK_SPACE=0x20）：補完を開く（消費）。直後の空白 WM_CHAR も抑制する。
                if msg == keyhook::WM_KEYDOWN && vk == 0x20 && ctrl.get() {
                    update(true);
                    suppress_space.set(true);
                    return true;
                }
                if msg == keyhook::WM_CHAR && wparam == 0x20 && suppress_space.get() {
                    suppress_space.set(false);
                    return true;
                }
                // カレット移動キーの KEYUP：カレットが動いたので補完を作り直す（カレット直前で判定するため）。
                // ←→ Home End は常に、↑↓ は候補リスト非表示時のみ（表示中の↑↓は候補移動なので除外）。
                if msg == keyhook::WM_KEYUP {
                    let horiz = matches!(vk, 0x25 | 0x27 | 0x24 | 0x23);
                    let vert = matches!(vk, 0x26 | 0x28);
                    if horiz || (vert && range.get().is_none()) {
                        update(false);
                    }
                    return false;
                }
                // 以降は候補リスト表示中だけ（KEYDOWN／WM_CHAR）。
                if range.get().is_none() {
                    return false;
                }
                // ↑（0x26）↓（0x28）：候補を上下に移動（端でクランプ・消費）。
                if msg == keyhook::WM_KEYDOWN && (vk == 0x26 || vk == 0x28) {
                    let count = cand_h.items().count().unwrap_or(0);
                    if count == 0 {
                        return false;
                    }
                    let cur = unsafe { cand_h.hwnd().SendMessage(lb::GetCurSel {}) }.unwrap_or(0);
                    let next =
                        if vk == 0x26 { cur.saturating_sub(1) } else { (cur + 1).min(count - 1) };
                    let _ = unsafe { cand_h.hwnd().SendMessage(lb::SetCurSel { index: Some(next) }) };
                    return true;
                }
                // Enter（WM_CHAR 0x0D）：選択中の候補を確定（消費して改行を防ぐ）。
                if msg == keyhook::WM_CHAR && wparam == 0x0D {
                    if let Some(idx) = unsafe { cand_h.hwnd().SendMessage(lb::GetCurSel {}) } {
                        do_insert(idx);
                    }
                    return true;
                }
                false
            });
            Ok(())
        });
    }

    // headless 観測：開いている補完つき入力欄の入力模擬・候補読み取り・確定・本文取得を公開する。
    #[cfg(feature = "debug-server")]
    {
        let cand_p = cand.clone();
        let cand_v = cand.clone();
        let edit_p = edit.clone();
        let edit_t = edit.clone();
        let range_v = range.clone();
        let do_insert = do_insert.clone();
        let update = update.clone();
        completion_probe::set(completion_probe::Probe {
            type_text: Box::new(move |s| {
                // 入力模擬：本文を s にしてカレットを末尾へ置き、補完を更新する。
                let _ = edit_t.set_text(s);
                let n = s.encode_utf16().count() as i32;
                edit_t.set_selection(n, n);
                update(false);
            }),
            candidates: Box::new(move || {
                let n = cand_p.items().count().unwrap_or(0);
                (0..n).filter_map(|i| cand_p.items().text(i).ok()).collect()
            }),
            visible: Box::new(move || range_v.get().is_some()),
            selected: Box::new(move || {
                unsafe { cand_v.hwnd().SendMessage(lb::GetCurSel {}) }.map_or(-1, |i| i as i32)
            }),
            accept: Box::new(move |idx| do_insert(idx)),
            text: Box::new(move || edit_p.text().unwrap_or_default()),
        });
    }

    let _ = wnd.show_modal(parent);
    keyhook::pop();
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
        /// 候補リストが表示中か。
        pub visible: Box<dyn Fn() -> bool>,
        /// 選択中の候補 index（無選択は -1）。
        pub selected: Box<dyn Fn() -> i32>,
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
    /// 候補リストが表示中か（入力欄が開いていなければ None）。
    pub fn visible() -> Option<bool> {
        PROBE.with(|s| s.borrow().as_ref().map(|p| (p.visible)()))
    }
    /// 選択中の候補 index（無選択は -1・入力欄が開いていなければ None）。
    pub fn selected() -> Option<i32> {
        PROBE.with(|s| s.borrow().as_ref().map(|p| (p.selected)()))
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
