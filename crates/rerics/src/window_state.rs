//! ウィンドウの位置・サイズ・最大化状態を取得/適用する。
//! 多モニタの画面外を考慮し、保存値を最寄りモニタの作業領域内に収めて配置する。
//!
//! 位置の取得/適用には Win32 の `WINDOWPLACEMENT` を使う。位置・サイズ・最大化と
//! その復元位置を一括で扱えるためだが、winsafe 同梱の `WINDOWPLACEMENT` は
//! `length` が実サイズと食い違い `SetWindowPlacement` に弾かれるので、構造体と
//! 関数を生 FFI で持つ。

use std::ffi::c_void;

use rerics_core::WindowState;
use winsafe::{self as w, co, prelude::*};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FfiPoint {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FfiRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Placement {
    length: u32,
    flags: u32,
    show_cmd: u32,
    min_pos: FfiPoint,
    max_pos: FfiPoint,
    normal_pos: FfiRect,
}

const SW_SHOWNORMAL: u32 = 1;
const SW_SHOWMAXIMIZED: u32 = 3;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetWindowPlacement(hwnd: *mut c_void, placement: *mut Placement) -> i32;
    fn SetWindowPlacement(hwnd: *mut c_void, placement: *const Placement) -> i32;
}

impl Placement {
    fn new() -> Self {
        Self {
            length: std::mem::size_of::<Placement>() as u32,
            flags: 0,
            show_cmd: SW_SHOWNORMAL,
            min_pos: FfiPoint::default(),
            max_pos: FfiPoint::default(),
            normal_pos: FfiRect::default(),
        }
    }
}

/// HWND の通常時の位置/サイズと最大化状態を取り出す。取得失敗時は `None`。
pub fn capture(hwnd: &w::HWND) -> Option<WindowState> {
    let mut p = Placement::new();
    if unsafe { GetWindowPlacement(hwnd.ptr(), &mut p) } == 0 {
        return None;
    }
    let rc = p.normal_pos;
    Some(WindowState {
        x: rc.left,
        y: rc.top,
        width: rc.right - rc.left,
        height: rc.bottom - rc.top,
        maximized: p.show_cmd == SW_SHOWMAXIMIZED,
    })
}

/// 保存済みの通常位置/サイズを HWND に適用する。どのモニタにも乗らない場合は
/// 何もせず `false` を返す。乗る場合は最寄りモニタの作業領域内にクランプして
/// 配置し `true` を返す。最大化はここでは行わない（生成直後に最大化すると復元
/// 位置が失われるため、表示後に [`maximize`] を呼ぶ）。
pub fn apply(hwnd: &w::HWND, st: &WindowState) -> bool {
    let rc = w::RECT {
        left: st.x,
        top: st.y,
        right: st.x + st.width,
        bottom: st.y + st.height,
    };
    if w::HMONITOR::MonitorFromRect(rc, co::MONITOR::DEFAULTTONULL)
        .as_opt()
        .is_none()
    {
        return false;
    }
    let mon = w::HMONITOR::MonitorFromRect(rc, co::MONITOR::DEFAULTTONEAREST);
    let Ok(info) = mon.GetMonitorInfo() else {
        return false;
    };
    let work = info.rcWork;
    let (nx, ny) = rerics_core::clamp_to_work(
        st.x,
        st.y,
        st.width,
        st.height,
        (work.left, work.top, work.right, work.bottom),
    );
    let p = Placement {
        show_cmd: SW_SHOWNORMAL,
        normal_pos: FfiRect {
            left: nx,
            top: ny,
            right: nx + st.width,
            bottom: ny + st.height,
        },
        ..Placement::new()
    };
    unsafe {
        SetWindowPlacement(hwnd.ptr(), &p);
    }
    true
}

/// ウィンドウを最大化表示する。直前に [`apply`] で設定した通常位置が復元位置
/// として保持される。ウィンドウが完全に表示された後に呼ぶこと。
pub fn maximize(hwnd: &w::HWND) {
    hwnd.ShowWindow(co::SW::SHOWMAXIMIZED);
}
