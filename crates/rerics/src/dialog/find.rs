use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use rerics_core::{FindOptions, parse_local};
use winsafe::{co, gui, prelude::*};
use super::*;

/// ファイル検索の条件ダイアログ（原作 frmFindFile の名前・日付・サイズ部のみ。内容検索＝
/// キーワード/検索方法/抽出条件は対象外）。OK なら [`FindOptions`] を返す。中止/Esc は `None`。
pub fn find_file_box(parent: &impl GuiParent) -> Option<FindOptions> {
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
            radio("過去1日", 16, 140, false),
            radio("過去1週間", 100, 140, false),
            radio("過去1ヶ月", 196, 140, false),
        ],
    );
    let from_date = edit(&wnd, 170, 92, 76);
    let _ = label(&wnd, "～", 250, 95, 16);
    let to_date = edit(&wnd, 266, 92, 76);
    let _ = label(&wnd, "(yyyy/mm/dd)", 170, 116, 180);

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

    let result: Rc<RefCell<Option<FindOptions>>> = Rc::new(RefCell::new(None));

    arm_modal(
        &arm,
        "find_file",
        "ファイル検索",
        "ファイル名・日付・サイズの条件",
        true,
        vec![("検索開始".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
        |_| Ok(()),
    );
    {
        let result = result.clone();
        let name = name.clone();
        let (date_mode, from_date, to_date) = (date_mode.clone(), from_date.clone(), to_date.clone());
        let (from_size, to_size) = (from_size.clone(), to_size.clone());
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            let mut opts = FindOptions::default();
            opts.set_masks(&name.text().unwrap_or_default());
            let (f, t) = date_range(
                date_mode.selected_index(),
                &from_date.text().unwrap_or_default(),
                &to_date.text().unwrap_or_default(),
            );
            opts.from_date = f;
            opts.to_date = t;
            opts.min_size = parse_size(&from_size.text().unwrap_or_default());
            opts.max_size = parse_size(&to_size.text().unwrap_or_default());
            *result.borrow_mut() = Some(opts);
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

/// 日付ラジオの選択と入力欄から、更新日時の下限・上限を求める。
/// 0＝すべて（絞らない）、1＝日付指定（from/to をパース）、2/3/4＝過去1日/週/月（下限のみ）。
fn date_range(mode: Option<usize>, from: &str, to: &str) -> (Option<SystemTime>, Option<SystemTime>) {
    match mode {
        Some(1) => (parse_date(from), parse_date(to)),
        Some(2) => (Some(ago(Duration::from_secs(86_400))), None),
        Some(3) => (Some(ago(Duration::from_secs(7 * 86_400))), None),
        Some(4) => (Some(ago(Duration::from_secs(30 * 86_400))), None),
        _ => (None, None),
    }
}

/// 現在時刻から `d` だけ過去の時刻。
fn ago(d: Duration) -> SystemTime {
    SystemTime::now().checked_sub(d).unwrap_or(SystemTime::UNIX_EPOCH)
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
