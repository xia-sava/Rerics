mod file_list;
mod window_state;

use std::cell::RefCell;
use std::rc::Rc;

use file_list::FileListView;
use rerics_core::{Command, KeyChord, KeyMap, Pane, WindowState};
use winsafe::{self as w, co, gui, prelude::*};

const MARGIN: i32 = 8;
const GAP: i32 = 8;
const BAR_H: i32 = 22;
const BAR_GAP: i32 = 4;

/// 表示完了後に最大化を実行させるための自前メッセージ（`WM_APP`）。
fn wm_restore_maximize() -> co::WM {
    unsafe { co::WM::from_raw(0x8000) }
}

fn main() {
    if let Err(e) = MainWindow::new().run() {
        eprintln!("エラー: {}", e);
    }
}

#[derive(Clone)]
struct MainWindow {
    wnd: gui::WindowMain,
    left: FileListView,
    right: FileListView,
    left_bar: gui::Label,
    right_bar: gui::Label,
    left_pane: Rc<RefCell<Pane>>,
    right_pane: Rc<RefCell<Pane>>,
    keymap: Rc<KeyMap>,
    initial_window: Option<WindowState>,
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

        let left = FileListView::new(&wnd, gui::dpi(MARGIN, MARGIN), gui::dpi(400, 400));
        let right = FileListView::new(&wnd, gui::dpi(MARGIN, MARGIN), gui::dpi(400, 400));

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
        let left_bar = make_bar(&wnd);
        let right_bar = make_bar(&wnd);

        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "..".to_owned());
        let left_pane = Rc::new(RefCell::new(Pane::open(".")));
        let right_pane = Rc::new(RefCell::new(Pane::open(&home)));

        let keymap = KeyMap::default();

        let initial_window = rerics_core::State::load().window;

        Self {
            wnd,
            left,
            right,
            left_bar,
            right_bar,
            left_pane,
            right_pane,
            keymap: Rc::new(keymap),
            initial_window,
        }
    }

    fn run(&self) -> w::AnyResult<i32> {
        self.setup_events();
        self.wnd.run_main(None)
    }

    fn setup_events(&self) {
        // 各ペインのキー入力とダブルクリックを配線（コントロール生成は済んでいるが、
        // FileListView のコールバック登録は実行時可で、内部イベントは生成前に配線済み）。
        self.wire_pane(true);
        self.wire_pane(false);

        let this = self.clone();
        self.wnd.on().wm_create(move |_| {
            if let Some(ws) = &this.initial_window {
                let applied = window_state::apply(&this.wnd.hwnd(), ws);
                if applied && ws.maximized {
                    unsafe {
                        let _ = this.wnd.hwnd().PostMessage(w::msg::WndMsg {
                            msg_id: wm_restore_maximize(),
                            wparam: 0,
                            lparam: 0,
                        });
                    }
                }
            }
            this.reload_side(true)?;
            this.reload_side(false)?;
            this.layout()?;
            this.left.hwnd().SetFocus();
            Ok(0)
        });

        let this = self.clone();
        self.wnd.on().wm(wm_restore_maximize(), move |_| {
            window_state::maximize(&this.wnd.hwnd());
            Ok(0)
        });

        let this = self.clone();
        self.wnd.on().wm_size(move |_| this.layout());

        let this = self.clone();
        self.wnd.on().wm_destroy(move || {
            if let Some(ws) = window_state::capture(&this.wnd.hwnd()) {
                let state = rerics_core::State { window: Some(ws) };
                if let Err(e) = state.save() {
                    eprintln!("状態の保存に失敗: {}", e);
                }
            }
            Ok(())
        });
    }

    fn wire_pane(&self, is_left: bool) {
        let this = self.clone();
        self.view(is_left).on_key_down(move |vk, alt, _shift| {
            let mut chord = KeyChord::key(vk);
            chord.alt = alt;
            if let Some(cmd) = this.keymap.resolve(&chord) {
                let _ = this.exec(is_left, cmd);
            }
        });

        let this = self.clone();
        self.view(is_left).on_activate(move |idx| {
            let _ = this.activate(is_left, idx);
        });

        // アクティブ側のカーソルを出し、反対側を消す。
        let this = self.clone();
        self.view(is_left).on_got_focus(move || {
            this.view(!is_left).set_cursor_visible(false);
        });
    }

    fn exec(&self, is_left: bool, cmd: Command) -> w::AnyResult<()> {
        let view = self.view(is_left);
        let state = view.state();
        let pr = view.page_rows();
        match cmd {
            Command::CursorUp => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.set_cursor(c - 1, pr);
            }
            Command::CursorDown => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.set_cursor(c + 1, pr);
            }
            Command::CursorTop => {
                state.borrow_mut().set_cursor(0, pr);
            }
            Command::CursorEnd => {
                let mut s = state.borrow_mut();
                let last = s.count() as isize - 1;
                s.set_cursor(last, pr);
            }
            Command::CursorPageUp => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.set_cursor(c - pr as isize, pr);
            }
            Command::CursorPageDown => {
                let mut s = state.borrow_mut();
                let c = s.cursor as isize;
                s.set_cursor(c + pr as isize, pr);
            }
            Command::EnterDir => {
                let cursor = state.borrow().cursor;
                self.activate(is_left, cursor)?;
                return Ok(());
            }
            Command::ToParent => {
                self.to_parent(is_left)?;
                return Ok(());
            }
            Command::FocusLeft => {
                self.left.hwnd().SetFocus();
                return Ok(());
            }
            Command::FocusRight => {
                self.right.hwnd().SetFocus();
                return Ok(());
            }
            Command::MarkToggle => {
                let mut s = state.borrow_mut();
                let c = s.cursor;
                s.reverse_file(c, pr);
                let c = s.cursor as isize;
                s.set_cursor(c + 1, pr);
            }
        }
        view.refresh()?;
        Ok(())
    }

    fn view(&self, is_left: bool) -> &FileListView {
        if is_left { &self.left } else { &self.right }
    }

    fn bar(&self, is_left: bool) -> &gui::Label {
        if is_left { &self.left_bar } else { &self.right_bar }
    }

    fn pane(&self, is_left: bool) -> &Rc<RefCell<Pane>> {
        if is_left { &self.left_pane } else { &self.right_pane }
    }

    /// ペインの現在パスを読み直して State へ反映し、パスバーを更新する。
    fn reload_side(&self, is_left: bool) -> w::AnyResult<()> {
        let view = self.view(is_left);
        let items = self.pane(is_left).borrow().read();
        let path = self.pane(is_left).borrow().path().display().to_string();
        let pr = view.page_rows();
        {
            let state = view.state();
            let mut s = state.borrow_mut();
            s.items = items;
            let sort = s.sort_type;
            let reverse = s.sort_reverse;
            s.sort(sort, reverse);
            s.set_cursor(0, pr);
        }
        self.bar(is_left).hwnd().SetWindowText(&path)?;
        view.refresh()?;
        Ok(())
    }

    /// カーソル行を侵入する（dir/親なら移動、file は無視）。
    fn activate(&self, is_left: bool, index: usize) -> w::AnyResult<()> {
        let view = self.view(is_left);
        let (is_parent, is_dir, name) = {
            let state = view.state();
            let s = state.borrow();
            let Some(it) = s.items.get(index) else {
                return Ok(());
            };
            (it.is_parent, it.is_dir, it.name.clone())
        };
        if is_parent {
            return self.to_parent(is_left);
        }
        if is_dir {
            if self.pane(is_left).borrow_mut().enter(&name) {
                self.reload_side(is_left)?;
            }
        }
        Ok(())
    }

    /// 親ディレクトリへ移動し、元ディレクトリ名にカーソルを置きセンタリングする。
    fn to_parent(&self, is_left: bool) -> w::AnyResult<()> {
        let prev = self.pane(is_left).borrow_mut().to_parent();
        let Some(prev_name) = prev else {
            return Ok(());
        };
        self.reload_side(is_left)?;
        let view = self.view(is_left);
        let pr = view.page_rows();
        {
            let state = view.state();
            let mut s = state.borrow_mut();
            s.set_cursor_position(&prev_name, pr);
            s.center_cursor(pr);
        }
        view.refresh()?;
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
        self.left.refresh()?;
        self.right.refresh()?;
        Ok(())
    }
}

fn place(hwnd: &w::HWND, x: i32, y: i32, cx: i32, cy: i32) -> w::AnyResult<()> {
    hwnd.MoveWindow(w::POINT { x, y }, w::SIZE { cx, cy }, true)?;
    Ok(())
}
