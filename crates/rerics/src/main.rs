use std::cell::RefCell;
use std::rc::Rc;

use rerics_core::{Command, KeyChord, KeyMap, Pane};
use winsafe::{self as w, co, gui, msg, prelude::*};

const MARGIN: i32 = 8;
const GAP: i32 = 8;
const BAR_H: i32 = 22;
const BAR_GAP: i32 = 4;

fn main() {
    if let Err(e) = MainWindow::new().run() {
        eprintln!("エラー: {}", e);
    }
}

#[derive(Clone)]
struct MainWindow {
    wnd: gui::WindowMain,
    left: gui::ListView,
    right: gui::ListView,
    left_bar: gui::Label,
    right_bar: gui::Label,
    left_pane: Rc<RefCell<Pane>>,
    right_pane: Rc<RefCell<Pane>>,
    keymap: Rc<KeyMap>,
}

impl MainWindow {
    fn new() -> Self {
        let wnd = gui::WindowMain::new(gui::WindowMainOpts {
            title: "Rerics",
            size: gui::dpi(960, 560),
            style: co::WS::CAPTION
                | co::WS::SYSMENU
                | co::WS::CLIPCHILDREN
                | co::WS::BORDER
                | co::WS::VISIBLE
                | co::WS::SIZEBOX
                | co::WS::MINIMIZEBOX
                | co::WS::MAXIMIZEBOX,
            process_dlg_msgs: false,
            ..Default::default()
        });

        let make_list = |parent: &gui::WindowMain| {
            gui::ListView::new(
                parent,
                gui::ListViewOpts {
                    position: gui::dpi(MARGIN, MARGIN),
                    size: gui::dpi(400, 400),
                    control_style: co::LVS::REPORT | co::LVS::SHOWSELALWAYS,
                    control_ex_style: co::LVS_EX::FULLROWSELECT | co::LVS_EX::GRIDLINES,
                    columns: &[("名前", 300), ("サイズ", 100)],
                    ..Default::default()
                },
            )
        };
        let make_bar = |parent: &gui::WindowMain| {
            gui::Label::new(
                parent,
                gui::LabelOpts {
                    text: "",
                    position: gui::dpi(MARGIN, MARGIN),
                    size: gui::dpi(400, BAR_H),
                    ..Default::default()
                },
            )
        };

        let left = make_list(&wnd);
        let right = make_list(&wnd);
        let left_bar = make_bar(&wnd);
        let right_bar = make_bar(&wnd);

        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "..".to_owned());
        let left_pane = Rc::new(RefCell::new(Pane::open(".")));
        let right_pane = Rc::new(RefCell::new(Pane::open(&home)));

        // キーバインドは KeyMap で自由に差し替え可能（将来は TOML から読む）。
        // 例: 上下逆 → keymap.bind(KeyChord::key(vk::UP), Command::CursorDown) ...
        let keymap = KeyMap::records_default();

        Self {
            wnd,
            left,
            right,
            left_bar,
            right_bar,
            left_pane,
            right_pane,
            keymap: Rc::new(keymap),
        }
    }

    fn run(&self) -> w::AnyResult<i32> {
        self.setup_events();
        self.wnd.run_main(None)
    }

    fn setup_events(&self) {
        let this = self.clone();
        self.wnd.on().wm_create(move |_| {
            refresh(&this.left, &this.left_bar, &this.left_pane)?;
            refresh(&this.right, &this.right_bar, &this.right_pane)?;
            this.layout()?;
            this.left.hwnd().SetFocus();
            Ok(0)
        });

        let this = self.clone();
        self.wnd.on().wm_size(move |_| this.layout());

        self.wire_pane(true);
        self.wire_pane(false);
    }

    /// 片側ペインのキー（サブクラスで横取り）とマウスを配線する。
    fn wire_pane(&self, is_left: bool) {
        let list = self.list(is_left);

        // ListView を subclass して WM_KEYDOWN を横取り。バインド済みキーは
        // コマンド実行＋値返却で既定処理を抑止、未バインドは DefSubclassProc へ委譲。
        let this = self.clone();
        list.on_subclass().wm(co::WM::KEYDOWN, move |p| {
            let chord = KeyChord::key(p.wparam as u16);
            match this.keymap.resolve(&chord) {
                Some(cmd) => {
                    this.exec(is_left, cmd)?;
                    Ok(0)
                }
                None => Ok(unsafe { this.list(is_left).hwnd().DefSubclassProc(p) }),
            }
        });

        let this = self.clone();
        list.on().nm_dbl_clk(move |p| {
            if p.iItem >= 0 {
                this.activate(is_left, p.iItem as usize)?;
            }
            Ok(())
        });
    }

    /// コマンドを実行する。
    fn exec(&self, is_left: bool, cmd: Command) -> w::AnyResult<()> {
        let list = self.list(is_left);
        match cmd {
            Command::CursorUp => set_cursor(list, focused_index(list) - 1)?,
            Command::CursorDown => set_cursor(list, focused_index(list) + 1)?,
            Command::CursorTop => set_cursor(list, 0)?,
            Command::CursorEnd => set_cursor(list, list.items().count() as i32 - 1)?,
            Command::CursorPageUp => set_cursor(list, focused_index(list) - page_size(list))?,
            Command::CursorPageDown => set_cursor(list, focused_index(list) + page_size(list))?,
            Command::EnterDir => {
                if let Some(idx) = list.items().focused().map(|i| i.index() as usize) {
                    self.activate(is_left, idx)?;
                }
            }
            Command::ToParent => {
                if self.pane(is_left).borrow_mut().to_parent() {
                    self.refresh_side(is_left)?;
                }
            }
            Command::FocusLeft => {
                self.left.hwnd().SetFocus();
            }
            Command::FocusRight => {
                self.right.hwnd().SetFocus();
            }
        }
        Ok(())
    }

    fn list(&self, is_left: bool) -> &gui::ListView {
        if is_left { &self.left } else { &self.right }
    }

    fn bar(&self, is_left: bool) -> &gui::Label {
        if is_left { &self.left_bar } else { &self.right_bar }
    }

    fn pane(&self, is_left: bool) -> &Rc<RefCell<Pane>> {
        if is_left { &self.left_pane } else { &self.right_pane }
    }

    fn refresh_side(&self, is_left: bool) -> w::AnyResult<()> {
        refresh(self.list(is_left), self.bar(is_left), self.pane(is_left))
    }

    /// `index` のエントリへ侵入できたら再描画する。
    fn activate(&self, is_left: bool, index: usize) -> w::AnyResult<()> {
        if self.pane(is_left).borrow_mut().enter(index) {
            self.refresh_side(is_left)?;
        }
        Ok(())
    }

    /// 左右2ペイン（パスバー＋リスト）をクライアント領域に均等割り付けする。
    fn layout(&self) -> w::AnyResult<()> {
        let rc = self.wnd.hwnd().GetClientRect()?;
        let total_w = rc.right - rc.left;
        let total_h = rc.bottom - rc.top;
        let m = gui::dpi_x(MARGIN);
        let gap = gui::dpi_x(GAP);
        let my = gui::dpi_y(MARGIN);
        let bar_h = gui::dpi_y(BAR_H);
        let bar_gap = gui::dpi_y(BAR_GAP);

        let pane_w = (total_w - m * 2 - gap) / 2;
        let list_y = my + bar_h + bar_gap;
        let list_h = total_h - list_y - my;
        let left_x = m;
        let right_x = m + pane_w + gap;

        place(self.left_bar.hwnd(), left_x, my, pane_w, bar_h)?;
        place(self.left.hwnd(), left_x, list_y, pane_w, list_h)?;
        place(self.right_bar.hwnd(), right_x, my, pane_w, bar_h)?;
        place(self.right.hwnd(), right_x, list_y, pane_w, list_h)?;
        Ok(())
    }
}

fn place(hwnd: &w::HWND, x: i32, y: i32, cx: i32, cy: i32) -> w::AnyResult<()> {
    hwnd.MoveWindow(w::POINT { x, y }, w::SIZE { cx, cy }, true)?;
    Ok(())
}

/// 現在のカーソル（フォーカス）行の index。無ければ 0。
fn focused_index(list: &gui::ListView) -> i32 {
    list.items().focused().map(|i| i.index() as i32).unwrap_or(0)
}

/// ListView の1ページの行数。
fn page_size(list: &gui::ListView) -> i32 {
    let n = unsafe { list.hwnd().SendMessage(msg::lvm::GetCountPerPage {}) } as i32;
    n.max(1)
}

/// カーソルを `target` 行へ移動する（範囲はクランプ）。
fn set_cursor(list: &gui::ListView, target: i32) -> w::AnyResult<()> {
    let items = list.items();
    let count = items.count() as i32;
    if count == 0 {
        return Ok(());
    }
    let t = target.clamp(0, count - 1);
    if let Some(cur) = items.focused() {
        cur.select(false)?;
    }
    if let Some(it) = items.iter().nth(t as usize) {
        it.focus()?;
        it.select(true)?;
        it.ensure_visible()?;
    }
    Ok(())
}

/// ペインの内容を ListView に反映し、パスバーを更新し、先頭にカーソルを置く。
fn refresh(list: &gui::ListView, bar: &gui::Label, pane: &Rc<RefCell<Pane>>) -> w::AnyResult<()> {
    let pane = pane.borrow();
    let items = list.items();
    items.delete_all()?;
    for e in pane.entries() {
        let size = if e.is_dir {
            "<DIR>".to_owned()
        } else {
            format!("{} B", e.size)
        };
        items.add(&[e.name.as_str(), size.as_str()], None, ())?;
    }
    bar.hwnd().SetWindowText(&pane.path().display().to_string())?;
    if let Some(first) = items.iter().next() {
        first.focus()?;
        first.select(true)?;
    }
    Ok(())
}
