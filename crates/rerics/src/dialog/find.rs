use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use rerics_core::{FindOptions, format_local, parse_local};
use winsafe::{co, gui, prelude::*};
use super::*;

/// ファイル検索の条件ダイアログ（原作 frmFindFile の名前・日付・サイズ部のみ。内容検索＝
/// キーワード/検索方法/抽出条件は対象外）。`initial_name` をファイル名欄の初期値にし、開いた
/// 瞬間に全選択する。OK なら `(条件, 入力したファイル名マスク)` を返す（生のマスクは呼び側が
/// 次回の既定として覚えるのに使う）。中止/Esc は `None`。
pub fn find_file_box(parent: &impl GuiParent, initial_name: &str) -> Option<(FindOptions, String)> {
    let (wnd, arm) = modal_window("ファイル検索", 380, 264);

    let _ = label(&wnd, "ファイル名(&F)", 16, 12, 348);
    let name = edit(&wnd, 16, 30, 348);
    let _ = label(&wnd, "※ワイルドカード可、カンマまたはスペースで区切れます。", 16, 52, 348);

    let _ = label(&wnd, "日付", 16, 74, 320);
    let date_mode = gui::RadioGroup::new(
        &wnd,
        &[
            radio("すべて", 16, 94, true),
            radio("日付指定", 88, 94, false),
        ],
    );
    let from_date = edit(&wnd, 170, 92, 76);
    let _ = label(&wnd, "～", 250, 95, 16);
    let to_date = edit(&wnd, 266, 92, 76);
    let _ = label(&wnd, "(yyyy/mm/dd)", 170, 116, 180);
    // 日付クイック設定：押すと from 欄へ当該日付が入り「日付指定」モードへ切り替わる。
    let presets = [
        ("今日", 16, DateShortcut::Today, 11u16),
        ("1日前", 90, DateShortcut::Day, 12),
        ("1週間前", 164, DateShortcut::Week, 13),
        ("1ヶ月前", 238, DateShortcut::Month, 14),
    ];
    let date_btns: Vec<gui::Button> = presets
        .into_iter()
        .map(|(text, x, which, ctrl_id)| {
            let btn = date_button(&wnd, text, x, 136, 70, ctrl_id);
            let from = from_date.clone();
            let mode = date_mode.clone();
            btn.on().bn_clicked(move || {
                from.set_text(&shortcut_date(SystemTime::now(), which))?;
                mode[1].select(true);
                mode[0].select(false);
                Ok(())
            });
            btn
        })
        .collect();

    let _ = label(&wnd, "サイズ", 16, 166, 320);
    let from_size = edit(&wnd, 16, 184, 90);
    let _ = label(&wnd, "～", 112, 187, 16);
    let to_size = edit(&wnd, 130, 184, 90);
    let _ = label(&wnd, "※既定KB・末尾 MB/GB で単位指定", 16, 208, 320);

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "検索開始",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(196, 230),
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
            position: gui::dpi(284, 230),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<(FindOptions, String)>>> = Rc::new(RefCell::new(None));

    arm_modal(
        &arm,
        "find_file",
        "ファイル検索",
        "ファイル名・日付・サイズの条件",
        true,
        vec![("検索開始".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
        {
            let name = name.clone();
            let initial = initial_name.to_owned();
            move |_| {
                name.set_text(&initial)?;
                let n = initial.encode_utf16().count() as i32;
                name.set_selection(0, n);
                name.hwnd().SetFocus();
                Ok(())
            }
        },
    );
    {
        let result = result.clone();
        let name = name.clone();
        let (date_mode, from_date, to_date) = (date_mode.clone(), from_date.clone(), to_date.clone());
        let (from_size, to_size) = (from_size.clone(), to_size.clone());
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            let raw_name = name.text().unwrap_or_default();
            let mut opts = FindOptions::default();
            opts.set_masks(&raw_name);
            let (f, t) = date_range(
                date_mode.selected_index(),
                &from_date.text().unwrap_or_default(),
                &to_date.text().unwrap_or_default(),
            );
            opts.from_date = f;
            opts.to_date = t;
            opts.min_size = parse_size(&from_size.text().unwrap_or_default());
            opts.max_size = parse_size(&to_size.text().unwrap_or_default());
            *result.borrow_mut() = Some((opts, raw_name));
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
    let _ = (ok, cancel, date_btns);

    result.borrow_mut().take()
}

fn label(wnd: &gui::WindowModal, text: &str, x: i32, y: i32, w: i32) -> gui::Label {
    gui::Label::new(
        wnd,
        gui::LabelOpts { text, position: gui::dpi(x, y), size: gui::dpi(w, 16), ..Default::default() },
    )
}

fn edit(wnd: &gui::WindowModal, x: i32, y: i32, w: i32) -> gui::Edit {
    gui::Edit::new(
        wnd,
        gui::EditOpts {
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(x, y),
            width: gui::dpi_x(w),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    )
}

fn radio(text: &str, x: i32, y: i32, selected: bool) -> gui::RadioButtonOpts<'_> {
    gui::RadioButtonOpts { text, position: gui::dpi(x, y), size: gui::dpi(80, 18), selected, ..Default::default() }
}

fn date_button(wnd: &gui::WindowModal, text: &str, x: i32, y: i32, w: i32, ctrl_id: u16) -> gui::Button {
    gui::Button::new(
        wnd,
        gui::ButtonOpts {
            text,
            ctrl_id,
            position: gui::dpi(x, y),
            width: gui::dpi_x(w),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    )
}

/// 日付クイック設定の種別（from 欄へ流し込む基準日）。
#[derive(Clone, Copy)]
enum DateShortcut {
    Today,
    Day,
    Week,
    Month,
}

/// ショートカットが指す日付を `yyyy/mm/dd` で返す。今日＝当日、ほかは現在から 1 日/1 週間/
/// 1 ヶ月（30 日）前。範囲外は UNIX 元期に丸める。
fn shortcut_date(now: SystemTime, which: DateShortcut) -> String {
    let back = match which {
        DateShortcut::Today => Duration::ZERO,
        DateShortcut::Day => Duration::from_secs(86_400),
        DateShortcut::Week => Duration::from_secs(7 * 86_400),
        DateShortcut::Month => Duration::from_secs(30 * 86_400),
    };
    let t = now.checked_sub(back).unwrap_or(SystemTime::UNIX_EPOCH);
    format_local(t).split(' ').next().unwrap_or("").to_owned()
}

/// 日付モードの選択と入力欄から、更新日時の下限・上限を求める。
/// 0＝すべて（絞らない）、1＝日付指定（from/to をパース）。
fn date_range(mode: Option<usize>, from: &str, to: &str) -> (Option<SystemTime>, Option<SystemTime>) {
    match mode {
        Some(1) => (parse_date(from), parse_date(to)),
        _ => (None, None),
    }
}

/// `yyyy/mm/dd` を当日 00:00:00 の `SystemTime` へ。空・解釈不能は `None`。
fn parse_date(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    parse_local(&format!("{s} 00:00:00"))
}

/// サイズ入力をバイトへ。既定 KB、末尾 `MB`/`GB` で単位変更。空・解釈不能は `None`。
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let upper = s.to_uppercase();
    let (num, mult) = if let Some(n) = upper.strip_suffix("GB") {
        (n, 1024 * 1024 * 1024)
    } else if let Some(n) = upper.strip_suffix("MB") {
        (n, 1024 * 1024)
    } else if let Some(n) = upper.strip_suffix("KB") {
        (n, 1024)
    } else {
        (upper.as_str(), 1024)
    };
    num.trim().parse::<u64>().ok().map(|v| v * mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> SystemTime {
        // ローカル真夜中跨ぎを避けるため、ある日の正午あたりを基準にする。
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn shortcut_today_matches_format_local_date_part() {
        let now = fixed_now();
        let today = shortcut_date(now, DateShortcut::Today);
        assert_eq!(today, format_local(now).split(' ').next().unwrap());
        // `yyyy/mm/dd` 形（10 文字・区切り 2 個）で from 欄へそのまま入る。
        assert_eq!(today.len(), 10, "{today}");
        assert_eq!(today.matches('/').count(), 2, "{today}");
        // 入力欄の解釈（parse_date）に往復で乗る。
        assert!(parse_date(&today).is_some(), "{today}");
    }

    #[test]
    fn shortcut_past_presets_go_back_in_time() {
        let now = fixed_now();
        let today = parse_date(&shortcut_date(now, DateShortcut::Today)).unwrap();
        for which in [DateShortcut::Day, DateShortcut::Week, DateShortcut::Month] {
            let past = parse_date(&shortcut_date(now, which)).unwrap();
            assert!(past < today, "{:?} should be before today", which as u8);
        }
    }
}
