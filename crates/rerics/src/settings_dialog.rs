//! 設定ダイアログ（仮）。配色テーマ・フォント・レイアウト・キーバインドを編集する。

use rerics_core::Config;
use winsafe::{self as w, prelude::*};

/// 設定ダイアログを表示し、OK なら編集後の設定を返す。キャンセルなら `None`。
pub fn show(_parent: &impl GuiParent, _current: &Config) -> Option<Config> {
    None
}
