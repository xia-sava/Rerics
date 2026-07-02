//! Win32／winsafe を生で叩く定型を、アプリの意図の言葉でまとめる薄い層。

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;

use winsafe::{self as w, co, gui, prelude::*};

// WM_MOUSEACTIVATE の戻り値（winsafe に co::MA 相当が無いため定数で持つ）。
const MA_ACTIVATE: isize = 1;

#[link(name = "gdi32")]
unsafe extern "system" {
    fn SetTextCharacterExtra(hdc: *mut c_void, extra: i32) -> i32;
}

/// DC の文字間隔（intercharacter spacing）を論理単位で設定する。負で詰める。
///
/// `DrawText`/`TextOut` の描画にも `GetTextExtentPoint32` の実測にも効くので、フォントを
/// 選択した直後に一度呼べば、その DC 上の以降の文字描画・幅測定が一律にこの間隔になる。
/// winsafe に相当メソッドが無いため gdi32 を直に叩く。
pub fn set_char_spacing(dc: &w::HDC, extra: i32) {
    unsafe { SetTextCharacterExtra(dc.ptr(), extra) };
}

/// クリックでキーフォーカスを奪わない「受け身の面」として子コントロールを設定する。
///
/// ペイン・ログ・各種バーは、それ自体はキーフォーカスを持たない（キー入力はキーシンクへ
/// 集約する）。一方で、窓が背面にいるときにクリックしたら標準どおり前面化させたい。
///
/// `WM_MOUSEACTIVATE` は**非アクティブな窓にしか送られない**ので、常に `MA_ACTIVATE` を
/// 返して前面化を許可するだけでよい。前面かどうかを判定してはいけない――ハンドラが走る
/// 時点で既に活性化処理が始まっており、`GetForegroundWindow` は自窓を返すレースになる。
/// それを見て `MA_NOACTIVATE` を返すと、起きかけた活性化を自分で打ち消してしまう。
/// 活性化に伴うキーフォーカスは `WM_ACTIVATE` 側でキーシンクへ戻す。
///
/// `WindowControl` の生成直後（メッセージ処理が始まる前）に呼ぶこと。
pub fn passive_focus(wnd: &gui::WindowControl) {
    wnd.on().wm(co::WM::MOUSEACTIVATE, |_| Ok(MA_ACTIVATE));
}

/// アプリ専用ウィンドウメッセージの採番表。
///
/// `WM_APP`(0x8000) 以上の私用域から、互いに衝突しないよう **ここで一括採番する**。
/// 新規はこの表に追記し、番号が重複しないことを目視で担保する（過去に別ファイルへ散在させて
/// 番号衝突を踏んだ）。FFI の生 `PostMessageW` へ渡すときは `.raw()` で `u32` を取り出す。
pub mod msg {
    use winsafe::co;

    /// 表示完了後に最大化を実行させる（起動時の最大化復元）。
    pub const RESTORE_MAXIMIZE: co::WM = unsafe { co::WM::from_raw(0x8000) };
    /// HTTP スレッドが UI スレッドを起こす（debug-server）。
    #[cfg_attr(not(feature = "debug-server"), allow(dead_code))]
    pub const DEBUG_WAKE: co::WM = unsafe { co::WM::from_raw(0x8001) };
    /// 非同期アイコン読込の完了通知。
    pub const ICONS_READY: co::WM = unsafe { co::WM::from_raw(0x8002) };
    /// 起動後に設定読み込み失敗のアラートを出す。
    pub const CONFIG_WARN: co::WM = unsafe { co::WM::from_raw(0x8003) };
    /// スクリプトエンジンスレッドが UI スレッドを起こす。
    pub const SCRIPT_WAKE: co::WM = unsafe { co::WM::from_raw(0x8004) };
    /// 検索・比較ワーカーが UI スレッドを起こす（イベント送信ごとに取り込ませる）。
    pub const TASK_WAKE: co::WM = unsafe { co::WM::from_raw(0x8005) };
    /// ディレクトリ更新監視スレッドが UI スレッドへ再読込を要求する（`wparam` に対象サイド）。
    pub const RELOAD_WATCH: co::WM = unsafe { co::WM::from_raw(0x8006) };
}

// 共通ツールチップ（標準コモンコントロール `tooltips_class32`）。winsafe は TTM_* メッセージを
// 公開していないので、ここで生 FFI を閉じ込める。自前描画リストの「切り詰めセルの全文を hover で
// 見せる」用途を想定し、`TTF_TRACK`（位置を手動指定）で使う。
unsafe extern "system" {
    fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: *mut c_void,
        menu: *mut c_void,
        instance: *mut c_void,
        param: *mut c_void,
    ) -> *mut c_void;
    fn SendMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn PostMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
    #[cfg(feature = "debug-server")]
    fn GetWindowLongW(hwnd: *mut c_void, index: i32) -> i32;
}

/// ワーカースレッドから UI スレッドへアプリ専用メッセージを投げて起こす。`HWND` は `Send` で
/// ないので、生ポインタ（`HWND::ptr()` の `isize`）を渡してワーカーから安全に呼べる経路にする。
pub(crate) fn post_app_message(hwnd: isize, msg: co::WM) {
    unsafe {
        PostMessageW(hwnd as *mut c_void, msg.raw(), 0, 0);
    }
}

/// [`post_app_message`] の `wparam` つき版。監視スレッドが対象サイドを乗せて起こすのに使う。
pub(crate) fn post_app_message_wparam(hwnd: isize, msg: co::WM, wparam: usize) {
    unsafe {
        PostMessageW(hwnd as *mut c_void, msg.raw(), wparam, 0);
    }
}

const WM_USER: u32 = 0x0400;
const TTM_ADDTOOLW: u32 = WM_USER + 50;
const TTM_TRACKACTIVATE: u32 = WM_USER + 17;
const TTM_TRACKPOSITION: u32 = WM_USER + 18;
const TTM_UPDATETIPTEXTW: u32 = WM_USER + 57;
const TTF_IDISHWND: u32 = 0x0001;
const TTF_TRACK: u32 = 0x0020;
const TTF_ABSOLUTE: u32 = 0x0080;
const TTS_ALWAYSTIP: u32 = 0x01;
const TTS_NOPREFIX: u32 = 0x02;
const TTS_BALLOON: u32 = 0x40;
const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const CW_USEDEFAULT: i32 = i32::MIN;
#[cfg(feature = "debug-server")]
const WS_VISIBLE: i32 = 0x1000_0000;
#[cfg(feature = "debug-server")]
const GWL_STYLE: i32 = -16;

/// [`TTTOOLINFOW`](https://learn.microsoft.com/en-us/windows/win32/api/commctrl/ns-commctrl-tttoolinfow)。
/// 末尾の `lpReserved`（comctl32 v6 で追加）は載せない＝マニフェスト無しで使う v5.82 の
/// `cbSize`（lParam まで）に合わせる。これより大きい `cbSize` だと v5 では `TTM_ADDTOOL` が弾く。
#[repr(C)]
struct ToolInfoW {
    cb_size: u32,
    u_flags: u32,
    hwnd: *mut c_void,
    u_id: usize,
    rect: [i32; 4],
    hinst: *mut c_void,
    lpsz_text: *mut u16,
    l_param: isize,
}

/// 親窓に張り付く 1 個の追跡ツールチップ。`show`/`hide` で位置と本文を差し替える。
/// 親が破棄されれば子として一緒に破棄されるので、明示破棄は持たない。
pub struct Tooltip {
    hwnd: *mut c_void,
    parent: *mut c_void,
}

impl Tooltip {
    /// `parent` のクライアント全体を覆う追跡ツールチップを作る。生成失敗時は `None`。
    pub fn new(parent: &w::HWND) -> Option<Self> {
        let class: Vec<u16> = "tooltips_class32\0".encode_utf16().collect();
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX | TTS_BALLOON,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                parent.ptr(),
                std::ptr::null_mut(),
                parent.hinstance().ptr(),
                std::ptr::null_mut(),
            )
        };
        if hwnd.is_null() {
            return None;
        }
        let tip = Tooltip { hwnd, parent: parent.ptr() };
        let mut ti = tip.tool_info(std::ptr::null_mut());
        let added = unsafe { SendMessageW(hwnd, TTM_ADDTOOLW, 0, &mut ti as *mut ToolInfoW as isize) };
        if added == 0 {
            return None;
        }
        Some(tip)
    }

    fn tool_info(&self, text: *mut u16) -> ToolInfoW {
        ToolInfoW {
            cb_size: std::mem::size_of::<ToolInfoW>() as u32,
            u_flags: TTF_IDISHWND | TTF_TRACK | TTF_ABSOLUTE,
            hwnd: self.parent,
            u_id: self.parent as usize,
            rect: [0; 4],
            hinst: std::ptr::null_mut(),
            lpsz_text: text,
            l_param: 0,
        }
    }

    /// ツールチップ窓が表示状態（`WS_VISIBLE`）か。表示経路の観測用。
    #[cfg(feature = "debug-server")]
    fn is_visible(&self) -> bool {
        unsafe { GetWindowLongW(self.hwnd, GWL_STYLE) & WS_VISIBLE != 0 }
    }

    /// `screen`（スクリーン座標）を指す位置に `text` を表示する。
    pub fn show(&self, text: &str, screen: w::POINT) {
        let mut buf: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut ti = self.tool_info(buf.as_mut_ptr());
        let pos = (screen.x as u16 as isize) | ((screen.y as u16 as isize) << 16);
        unsafe {
            SendMessageW(self.hwnd, TTM_UPDATETIPTEXTW, 0, &mut ti as *mut ToolInfoW as isize);
            SendMessageW(self.hwnd, TTM_TRACKPOSITION, 0, pos);
            SendMessageW(self.hwnd, TTM_TRACKACTIVATE, 1, &mut ti as *mut ToolInfoW as isize);
        }
    }

    /// 表示中のツールチップを消す。
    pub fn hide(&self) {
        let mut ti = self.tool_info(std::ptr::null_mut());
        unsafe {
            SendMessageW(self.hwnd, TTM_TRACKACTIVATE, 0, &mut ti as *mut ToolInfoW as isize);
        }
    }
}

const HOVER_TIMER_ID: usize = 0xA0F0;
const HOVER_DELAY_MS: u32 = 500;

/// hover ツールチップの追跡状態。
struct HoverState {
    /// ツールチップ本体（最初の hover で遅延生成する）。
    tip: Option<Tooltip>,
    /// いま表示中のセル矩形（同じセル上では出し直さない）。
    shown: Option<w::RECT>,
    /// 表示待ち＝(セル矩形, 全文, アンカーのスクリーン座標)。タイマー満了で表示する。
    pending: Option<(w::RECT, String, w::POINT)>,
    /// `WM_MOUSELEAVE` を受け取るための追跡を貼ってあるか。
    tracking: bool,
}

/// 自前描画リストへ「切り詰めセルの全文を hover で見せる」ツールチップを足す再利用部品。
///
/// 骨格（遅延タイマー／表示・消去・`WM_MOUSELEAVE` 追跡）はここに持ち、リスト固有なのは
/// `resolver` だけ＝マウス位置（クライアント座標）→ **切り詰めで全文が見えていないセルなら
/// (セル矩形, 全文)**、それ以外（切り詰め無し・セル外）なら `None`。矩形は同一セル判定に使う。
///
/// winsafe は同一メッセージにつき最後に登録したハンドラしか呼ばないので、骨格側で
/// `WM_MOUSEMOVE` 等を登録すると既存ハンドラを潰してしまう。そこで**メッセージ登録はせず**、
/// ホストが自分の `wm_mouse_move`/`wm_mouse_leave`/`wm_timer(`[`Self::TIMER_ID`]`)` から
/// [`Self::on_mouse_move`]／[`Self::on_mouse_leave`]／[`Self::on_timer`] を呼ぶ。`Clone` は
/// 内部状態（`Rc`）を共有する＝ハンドラとデバッグフックで同じ実体を使える。
#[derive(Clone)]
pub struct CellTooltip {
    state: Rc<RefCell<HoverState>>,
    resolver: Rc<dyn Fn(w::POINT) -> Option<(w::RECT, String)>>,
}

impl CellTooltip {
    /// ホストが `wm_timer` を登録するときに使うタイマー id。
    pub const TIMER_ID: usize = HOVER_TIMER_ID;

    pub fn new<F>(resolver: F) -> Self
    where
        F: Fn(w::POINT) -> Option<(w::RECT, String)> + 'static,
    {
        Self {
            state: Rc::new(RefCell::new(HoverState {
                tip: None,
                shown: None,
                pending: None,
                tracking: false,
            })),
            resolver: Rc::new(resolver),
        }
    }

    /// ホストの `WM_MOUSEMOVE` から呼ぶ。切り詰めセルへ入ったら遅延表示を仕込み、別セル／セル外へ
    /// 動いたら出し直し・消去する。`WM_MOUSELEAVE` 追跡もここで貼る。
    pub fn on_mouse_move(&self, hwnd: &w::HWND, pt: w::POINT) {
        let mut s = self.state.borrow_mut();
        if !s.tracking {
            let mut tme = w::TRACKMOUSEEVENT::default();
            tme.dwFlags = co::TME::LEAVE;
            tme.hwndTrack = unsafe { hwnd.raw_copy() };
            if w::TrackMouseEvent(&mut tme).is_ok() {
                s.tracking = true;
            }
        }
        match (self.resolver)(pt) {
            None => hover_clear(&mut s, hwnd),
            Some((rect, _)) if s.shown == Some(rect) => {}
            Some((rect, _)) if s.pending.as_ref().is_some_and(|(r, ..)| *r == rect) => {}
            Some((rect, text)) => {
                if s.shown.take().is_some()
                    && let Some(tip) = &s.tip
                {
                    tip.hide();
                }
                let anchor = hwnd.ClientToScreen(pt).unwrap_or(pt);
                s.pending = Some((rect, text, anchor));
                let _ = hwnd.SetTimer(HOVER_TIMER_ID, HOVER_DELAY_MS, None);
            }
        }
    }

    /// ホストの `wm_timer(`[`Self::TIMER_ID`]`)` から呼ぶ。仕込んだセルの全文を表示する。
    pub fn on_timer(&self, hwnd: &w::HWND) {
        let _ = hwnd.KillTimer(HOVER_TIMER_ID);
        let mut s = self.state.borrow_mut();
        if let Some((rect, text, anchor)) = s.pending.take() {
            if s.tip.is_none() {
                s.tip = Tooltip::new(hwnd);
            }
            if let Some(tip) = &s.tip {
                tip.show(&text, anchor);
                s.shown = Some(rect);
            }
        }
    }

    /// ホストの `WM_MOUSELEAVE` から呼ぶ。表示中／待機中のツールチップを消す。
    pub fn on_mouse_leave(&self, hwnd: &w::HWND) {
        let mut s = self.state.borrow_mut();
        s.tracking = false;
        hover_clear(&mut s, hwnd);
    }

    /// `pt` へ実際に hover した表示経路（resolver→生成→表示）を即時に駆動して観測する（500ms 待たず）。
    /// 戻り値＝(ツールチップ生成成功, 表示状態, 全文)。切り詰めセルでなければ消去して `(_, false, "")`。
    #[cfg(feature = "debug-server")]
    pub fn probe(&self, hwnd: &w::HWND, pt: w::POINT) -> (bool, bool, String) {
        let mut s = self.state.borrow_mut();
        match (self.resolver)(pt) {
            None => {
                hover_clear(&mut s, hwnd);
                (s.tip.is_some(), false, String::new())
            }
            Some((rect, text)) => {
                if s.tip.is_none() {
                    s.tip = Tooltip::new(hwnd);
                }
                let anchor = hwnd.ClientToScreen(pt).unwrap_or(pt);
                let visible = s.tip.as_ref().is_some_and(|tip| {
                    tip.show(&text, anchor);
                    tip.is_visible()
                });
                s.shown = Some(rect);
                (s.tip.is_some(), visible, text)
            }
        }
    }
}

/// 表示中／表示待ちのツールチップを消し、タイマーも止める。
fn hover_clear(s: &mut HoverState, hwnd: &w::HWND) {
    if s.pending.take().is_some() {
        let _ = hwnd.KillTimer(HOVER_TIMER_ID);
    }
    if s.shown.take().is_some()
        && let Some(tip) = &s.tip
    {
        tip.hide();
    }
}
