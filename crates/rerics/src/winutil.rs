//! Win32／winsafe を生で叩く定型を、アプリの意図の言葉でまとめる薄い層。

use winsafe::{co, gui, prelude::*};

// WM_MOUSEACTIVATE の戻り値（winsafe に co::MA 相当が無いため定数で持つ）。
const MA_ACTIVATE: isize = 1;

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
