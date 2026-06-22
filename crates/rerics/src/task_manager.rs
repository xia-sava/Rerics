//! タスクマネージャ・モーダル。原作 `frmTaskManager` 相当。
//!
//! 走行中タスクを一覧表示し、選択タスクの中止・手動リフレッシュ・閉じるを行う。
//! ライブ更新はせず、原作同様「最新」ボタン（と開いた時）で再描画する。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use winsafe::{self as w, gui, prelude::*};

use crate::task::TaskEntry;

type Registry = Rc<RefCell<Vec<TaskEntry>>>;

/// タスクマネージャを表示する。タスクは別スレッドで動き続けるため、閉じても処理は継続する。
pub fn show(parent: &impl GuiParent, tasks: &Registry) {
    let (wnd, arm) =
        crate::dialog::modal_window_resizable_keyed("タスクマネージャ", "tasks", 588, 360, 480, 260);

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

    // リサイズ追従：一覧を広げ、左寄せアクションは x 固定で下端へ・「閉じる」は右下へ。
    {
        let wndc = wnd.clone();
        let lst = list.clone();
        let (b_stop, b_suspend, b_resume, b_refresh, b_close) =
            (stop.clone(), suspend.clone(), resume.clone(), refresh.clone(), close.clone());
        wnd.on().wm_size(move |_| {
            if let Ok(rc) = wndc.hwnd().GetClientRect() {
                relayout(&lst, [&b_stop, &b_suspend, &b_resume, &b_refresh], &b_close, rc.right, rc.bottom);
            }
            Ok(())
        });
    }

    {
        let list = list.clone();
        let tasks = tasks.clone();
        arm.on_create(move |_| {
            populate(&list, &tasks);
            Ok(())
        });
    }
    #[cfg(feature = "debug-server")]
    {
        let list_r = list.clone();
        let list_s = list.clone();
        arm.list_view(
            "tasks",
            "タスクマネージャ",
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
                    let sel = list_r.items().iter().position(|it| it.is_selected()).unwrap_or(0);
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
    let _ = (stop, suspend, resume, refresh, close);
}

/// クライアント `cw`×`ch`（物理px）に合わせて再配置する。一覧を四周 12px で広げ、左寄せの
/// アクション 4 ボタンは x（12/110/208/306・幅90・間隔98）を保ったまま下端へ、「閉じる」は
/// 下端右寄せへ寄せ直す。
fn relayout(
    list: &gui::ListView<u64>,
    left: [&gui::Button; 4],
    close: &gui::Button,
    cw: i32,
    ch: i32,
) {
    let m = gui::dpi_x(12);
    let mt = gui::dpi_y(12);
    let bh = gui::dpi_y(26);
    let bottom = gui::dpi_y(16);
    let gap = gui::dpi_y(12);
    let btn_y = (ch - bottom - bh).max(mt);
    for (i, b) in left.iter().enumerate() {
        let x = gui::dpi_x(12 + i as i32 * 98);
        let _ = b.hwnd().MoveWindow(w::POINT { x, y: btn_y }, w::SIZE { cx: gui::dpi_x(90), cy: bh }, true);
    }
    let close_w = gui::dpi_x(80);
    let _ = close.hwnd().MoveWindow(
        w::POINT { x: (cw - m - close_w).max(0), y: btn_y },
        w::SIZE { cx: close_w, cy: bh },
        true,
    );
    let list_w = (cw - m * 2).max(1);
    let list_h = (btn_y - gap - mt).max(1);
    let _ = list.hwnd().MoveWindow(w::POINT { x: m, y: mt }, w::SIZE { cx: list_w, cy: list_h }, true);
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
    let mut selected_any = false;
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
                selected_any = true;
            }
        }
    }
    // 前回選択が無い（初回表示など）ときは先頭行を選んでおく（そのまま操作できる）。
    if !selected_any {
        if let Some(item) = list.items().iter().next() {
            let _ = item.select(true);
            let _ = item.focus();
        }
    }
}

/// 経過時間を `HH:MM:SS` で整形する。
fn fmt_elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}
