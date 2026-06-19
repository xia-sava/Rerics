//! Win32／winsafe を生で叩く定型を、アプリの意図の言葉でまとめる薄い層。

use winsafe::{self as w, co, gui, prelude::*};

// WM_MOUSEACTIVATE の戻り値（winsafe に co::MA 相当が無いため定数で持つ）。
const MA_ACTIVATE: isize = 1;
const MA_NOACTIVATE: isize = 3;

/// クリックでキーフォーカスを奪わない「受け身の面」として子コントロールを設定する。
///
/// ペイン・ログ・各種バーのように、それ自体はキーフォーカスを持たず（キー入力は
/// キーシンクへ集約する）クリックされても前面のフォーカスを動かさない面に使う。
/// ただし窓全体が背面にいるときは、標準どおりクリックで前面化させる。
///
/// `WindowControl` の生成直後（メッセージ処理が始まる前）に呼ぶこと。
pub fn passive_focus(wnd: &gui::WindowControl) {
    wnd.on()
        .wm(co::WM::MOUSEACTIVATE, |m| Ok(mouse_activate_result_for(m.wparam)));
}

/// `WM_MOUSEACTIVATE` の戻り値を、活性化対象のトップレベル窓ハンドル（`wparam`）から決める。
fn mouse_activate_result_for(top_level_wparam: usize) -> isize {
    let top = unsafe { w::HWND::from_ptr(top_level_wparam as *mut std::ffi::c_void) };
    let is_foreground = w::HWND::GetForegroundWindow().as_ref() == Some(&top);
    mouse_activate_result(is_foreground)
}

/// `WM_MOUSEACTIVATE` の戻り値を決める純粋部分。
///
/// 既に前面（アクティブ）ならフォーカスを奪わない（`MA_NOACTIVATE`）。背面なら
/// 標準どおり前面化を許可し、そのクリックも通す（`MA_ACTIVATE`）。
fn mouse_activate_result(is_foreground: bool) -> isize {
    if is_foreground {
        MA_NOACTIVATE
    } else {
        MA_ACTIVATE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_keeps_focus() {
        assert_eq!(mouse_activate_result(true), MA_NOACTIVATE);
    }

    #[test]
    fn background_click_brings_to_front() {
        assert_eq!(mouse_activate_result(false), MA_ACTIVATE);
    }
}
