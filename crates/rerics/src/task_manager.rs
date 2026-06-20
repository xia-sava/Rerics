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
            ctrl_id: 100,
            position: gui::dpi(12, 294),
            width: gui::dpi_x(90),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let suspend = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "中断(&S)",
            ctrl_id: 101,
            position: gui::dpi(110, 294),
            width: gui::dpi_x(90),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let resume = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "再開(&R)",
            ctrl_id: 102,
            position: gui::dpi(208, 294),
            width: gui::dpi_x(90),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let refresh = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "最新",
            ctrl_id: 103,
            position: gui::dpi(306, 294),
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
            #[cfg(feature = "debug-server")]
            {
                let modal_ptr = list.hwnd().GetParent().map(|h| h.ptr() as isize).unwrap_or(0);
                let list_r = list.clone();
                let list_s = list.clone();
                crate::debug_server::modal_registry::push_list_view(
                    "tasks",
                    "タスクマネージャ",
                    modal_ptr,
                    vec![
                        ("中止(&A)".to_owned(), 100u16),
                        ("中断(&S)".to_owned(), 101u16),
                        ("再開(&R)".to_owned(), 102u16),
                        ("最新".to_owned(), 103u16),
                        ("閉じる".to_owned(), 2u16),
                    ],
                    crate::debug_server::modal_registry::ListViewHooks {
                        headers: ["タスク", "詳細", "状態", "経過時間"]
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        read: Box::new(move || {
                            let rows = list_r
                                .items()
                                .iter()
                                .map(|it| (0..4u32).map(|c| it.text(c)).collect())
                                .collect();
                            let sel =
                                list_r.items().iter().position(|it| it.is_selected()).unwrap_or(0);
                            (rows, sel)
                        }),
                        select: Box::new(move |idx| {
                            if let Some(it) = list_s.items().iter().nth(idx) {
                                let _ = it.select(true);
                                let _ = it.focus();
                            }
                        }),
                    },
                );
            }
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
            act_on_selected(&list, &tasks, |c| c.stop());
            Ok(())
        });
    }

    {
        let list = list.clone();
        let tasks = tasks.clone();
        suspend.on().bn_clicked(move || {
            act_on_selected(&list, &tasks, |c| c.suspend());
            Ok(())
        });
    }

    {
        let list = list.clone();
        let tasks = tasks.clone();
        resume.on().bn_clicked(move || {
            act_on_selected(&list, &tasks, |c| c.resume());
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
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::pop();
    let _ = (stop, suspend, resume, refresh, close);
}

/// 選択行のタスクに `action`（中止/中断/再開）を適用し、一覧を再描画する。
fn act_on_selected(
    list: &gui::ListView<u64>,
    tasks: &Registry,
    action: impl Fn(&crate::task::TaskControl),
) {
    if let Some(item) = list.items().iter_selected().next() {
        let id = *item.data().borrow();
        if let Some(entry) = tasks.borrow().iter().find(|e| e.id == id) {
            action(&entry.control);
        }
    }
    populate(list, tasks);
}

/// 一覧をレジストリの現在内容で再描画する。選択行（task id）は復元する。
fn populate(list: &gui::ListView<u64>, tasks: &Registry) {
    let prev = list.items().iter_selected().next().map(|it| *it.data().borrow());
    let _ = list.items().delete_all();
    for entry in tasks.borrow().iter() {
        let elapsed = fmt_elapsed(Instant::now().saturating_duration_since(entry.start));
        let row = [
            entry.text.clone(),
            entry.description.clone(),
            entry.control.state_label().to_owned(),
            elapsed,
        ];
        if let Ok(item) = list.items().add(&row, None, entry.id) {
            if Some(entry.id) == prev {
                let _ = item.select(true);
                let _ = item.focus();
            }
        }
    }
}

/// 経過時間を `HH:MM:SS` で整形する。
fn fmt_elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}
