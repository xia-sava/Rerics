//! 左右ペインのコンテナ。パスバー（上）とファイルリスト（中）を内包する WindowControl。
//!
//! 各ペインを1つの子ウィンドウに束ねることで、レイアウト追従・分割線・ステータスバー
//! 追加といった後続の作り込みをペイン単位で扱えるようにする。将来ステータスバー（下）も
//! ここに加える。

use std::cell::Cell;
use std::rc::Rc;

use rerics_core::{Config, Rgb};
use winsafe::{self as w, co, gui, prelude::*};

use crate::file_list::FileListView;
use crate::path_bar::PathBarView;
use crate::status_bar::StatusBarView;

/// 左右いずれかのペイン全体（パスバー＋ファイルリスト＋ステータスバー）。
#[derive(Clone)]
pub struct PaneView {
    container: gui::WindowControl,
    bar: PathBarView,
    list: FileListView,
    status: StatusBarView,
    bar_height: Rc<Cell<i32>>,
    bar_gap: Rc<Cell<i32>>,
    status_bar_height: Rc<Cell<i32>>,
    /// コンテナ背景ブラシの寿命を保持する（`class_bg_brush` へ raw コピーを渡すため）。
    _bg: Rc<w::guard::DeleteObjectGuard<w::HBRUSH>>,
}

impl PaneView {
    /// 親に子コンテナとして生成する。中に空のパスバーとファイルリストを作る。
    pub fn new(
        parent: &(impl GuiParent + 'static),
        position: (i32, i32),
        size: (i32, i32),
        cfg: &Config,
    ) -> Self {
        let bg = w::HBRUSH::CreateSolidBrush(rgb(cfg.active_colors().background)).expect("背景ブラシの生成");
        let bg_raw = unsafe { bg.raw_copy() };
        let container = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                class_bg_brush: gui::Brush::Handle(bg_raw),
                position,
                size,
                style: co::WS::CHILD
                    | co::WS::VISIBLE
                    | co::WS::CLIPCHILDREN
                    | co::WS::CLIPSIBLINGS,
                ..Default::default()
            },
        );
        crate::winutil::passive_focus(&container);
        let bar = PathBarView::new(&container, gui::dpi(0, 0), gui::dpi(100, cfg.layout.bar_height));
        let list = FileListView::new(&container, gui::dpi(0, 0), gui::dpi(100, 100), cfg);
        let status =
            StatusBarView::new(&container, gui::dpi(0, 0), gui::dpi(100, cfg.layout.status_bar_height));
        Self {
            container,
            bar,
            list,
            status,
            bar_height: Rc::new(Cell::new(gui::dpi_y(cfg.layout.bar_height))),
            bar_gap: Rc::new(Cell::new(gui::dpi_y(cfg.layout.bar_gap))),
            status_bar_height: Rc::new(Cell::new(gui::dpi_y(cfg.layout.status_bar_height))),
            _bg: Rc::new(bg),
        }
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.container.hwnd()
    }

    pub fn list(&self) -> &FileListView {
        &self.list
    }

    pub fn bar(&self) -> &PathBarView {
        &self.bar
    }

    pub fn status(&self) -> &StatusBarView {
        &self.status
    }

    /// 設定をペイン配下（パスバー・リスト・ステータス）へ反映し、寸法を更新する。
    /// パスバー・ステータスバーは chrome 共通のシステムUIフォント固定のため対象外。
    pub fn apply_config(&self, cfg: &Config) {
        self.bar_height.set(gui::dpi_y(cfg.layout.bar_height));
        self.bar_gap.set(gui::dpi_y(cfg.layout.bar_gap));
        self.status_bar_height.set(gui::dpi_y(cfg.layout.status_bar_height));
        self.list.apply_config(cfg);
    }

    /// コンテナ内でパスバー（上）・ファイルリスト（中）・ステータスバー（下）を再配置する。
    pub fn relayout(&self) -> w::AnyResult<()> {
        let rc = self.container.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        let bar_height = self.bar_height.get();
        let status_bar_height = self.status_bar_height.get();
        let list_y = bar_height + self.bar_gap.get();
        let status_y = ch - status_bar_height;
        place(self.bar.hwnd(), 0, 0, cw, bar_height)?;
        place(self.list.hwnd(), 0, list_y, cw, (status_y - list_y).max(0))?;
        place(self.status.hwnd(), 0, status_y, cw, status_bar_height)?;
        self.list.refresh()?;
        self.status.refresh()?;
        Ok(())
    }
}

fn place(hwnd: &w::HWND, x: i32, y: i32, cx: i32, cy: i32) -> w::AnyResult<()> {
    hwnd.MoveWindow(w::POINT { x, y }, w::SIZE { cx, cy }, true)?;
    Ok(())
}

fn rgb(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}
