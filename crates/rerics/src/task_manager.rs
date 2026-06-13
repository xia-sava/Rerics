//! タスクマネージャ・モーダル。原作 `frmTaskManager` 相当。
//!
//! 走行中タスクを一覧表示し、選択タスクの中止・手動リフレッシュ・閉じるを行う。
//! ライブ更新はせず、原作同様「最新」ボタン（と開いた時）で再描画する。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use winsafe::{co, gui, prelude::*};

use crate::task::TaskEntry;

type Registry = Rc<RefCell<Vec<TaskEntry>>>;

/// タスクマネージャを表示する。タスクは別スレッドで動き続けるため、閉じても処理は継続する。
pub fn show(parent: &impl GuiParent, tasks: &Registry) {
    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title: "タスクマネージャ",
        size: gui::dpi(588, 360),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE,
        process_dlg_msgs: true,
        ..Default::default()
    });

    let list = gui::ListView::<u64>::new(
        &wnd,
        gui::ListViewOpts {
            position: gui::dpi(12, 12),
            size: gui::dpi(564, 270),
            columns: &[("タスク", 110), ("詳細", 254), ("状態", 80), ("経過時間", 96)],
            ..Default::default()
        },
    );

    let stop = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "中止(&A)",
            position: gui::dpi(12, 294),
            width: gui::dpi_x(90),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let refresh = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "最新(&R)",
            position: gui::dpi(110, 294),
            width: gui::dpi_x(90),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let close = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "閉じる",
            ctrl_id: 2,
            position: gui::dpi(496, 294),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    {
        let list = list.clone();
        let tasks = tasks.clone();
        wnd.on().wm_create(move |_| {
            populate(&list, &tasks);
            Ok(0)
        });
    }

    {
        let list = list.clone();
        let tasks = tasks.clone();
        refresh.on().bn_clicked(move || {
            populate(&list, &tasks);
            Ok(())
        });
    }

    {
        let list = list.clone();
        let tasks = tasks.clone();
        stop.on().bn_clicked(move || {
            if let Some(item) = list.items().iter_selected().next() {
                let id = *item.data().borrow();
                if let Some(entry) = tasks.borrow().iter().find(|e| e.id == id) {
                    entry.control.stop();
                }
            }
            populate(&list, &tasks);
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
    let _ = (stop, refresh, close);
}

/// 一覧をレジストリの現在内容で再描画する。
fn populate(list: &gui::ListView<u64>, tasks: &Registry) {
    let _ = list.items().delete_all();
    for entry in tasks.borrow().iter() {
        let elapsed = fmt_elapsed(Instant::now().saturating_duration_since(entry.start));
        let row = [
            entry.text.clone(),
            entry.description.clone(),
            entry.control.state_label().to_owned(),
            elapsed,
        ];
        let _ = list.items().add(&row, None, entry.id);
    }
}

/// 経過時間を `HH:MM:SS` で整形する。
fn fmt_elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}
