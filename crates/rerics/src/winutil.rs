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
    /// スクリプトエンジンスレッドが UI スレッドを起こす（scripting）。
    #[cfg_attr(not(feature = "scripting"), allow(dead_code))]
    pub const SCRIPT_WAKE: co::WM = unsafe { co::WM::from_raw(0x8004) };
}
