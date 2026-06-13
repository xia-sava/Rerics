//! 左右ペインのコンテナ。パスバー（上）とファイルリスト（中）を内包する WindowControl。
//!
//! 各ペインを1つの子ウィンドウに束ねることで、レイアウト追従・分割線・ステータスバー
//! 追加といった後続の作り込みをペイン単位で扱えるようにする。将来ステータスバー（下）も
//! ここに加える。

use std::rc::Rc;

use rerics_core::{Config, Rgb};
use winsafe::{self as w, co, gui, prelude::*};

use crate::file_list::FileListView;

/// 左右いずれかのペイン全体（パスバー＋ファイルリスト）。
#[derive(Clone)]
pub struct PaneView {
    container: gui::WindowControl,
    bar: gui::Label,
    list: FileListView,
    bar_height: i32,
    bar_gap: i32,
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
        let bg = w::HBRUSH::CreateSolidBrush(rgb(cfg.colors.background)).expect("背景ブラシの生成");
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
        let bar = gui::Label::new(
            &container,
            gui::LabelOpts {
                text: "",
                position: gui::dpi(0, 0),
                size: gui::dpi(100, cfg.layout.bar_height),
                ..Default::default()
            },
        );
        let list = FileListView::new(&container, gui::dpi(0, 0), gui::dpi(100, 100), cfg);
        Self {
            container,
            bar,
            list,
            bar_height: gui::dpi_y(cfg.layout.bar_height),
            bar_gap: gui::dpi_y(cfg.layout.bar_gap),
            _bg: Rc::new(bg),
        }
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.container.hwnd()
    }

    pub fn list(&self) -> &FileListView {
        &self.list
    }

    pub fn bar(&self) -> &gui::Label {
        &self.bar
    }

    /// コンテナ内でパスバー（上）とファイルリスト（残り）を再配置する。
    pub fn relayout(&self) -> w::AnyResult<()> {
        let rc = self.container.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        let list_y = self.bar_height + self.bar_gap;
        place(self.bar.hwnd(), 0, 0, cw, self.bar_height)?;
        place(self.list.hwnd(), 0, list_y, cw, (ch - list_y).max(0))?;
        self.list.refresh()?;
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
