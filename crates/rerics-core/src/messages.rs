//! メッセージカタログ。原作 `Filer.message`（XML）の文字列を型付き関数で持つ。
//!
//! ログ・ダイアログの文言を一箇所に集約し、原作準拠の文面を再利用する。
//! 各関数のドキュメントに原作の `id` とテンプレートを併記する。
//!
//! 参照テーブルとして未使用の項目も持つ（順次載せ替えていく）。
#![allow(dead_code)]

/// `Copy {0}`（コピー操作の逐次ログ）。
pub fn copy(name: &str) -> String {
    format!("Copy {name}")
}

/// `Move {0}`（移動操作の逐次ログ）。
pub fn move_(name: &str) -> String {
    format!("Move {name}")
}

/// `Delete {0}`（ファイル削除の逐次ログ）。
pub fn delete(name: &str) -> String {
    format!("Delete {name}")
}

/// `DeleteDirectory {0}`（ディレクトリ削除の逐次ログ）。
pub fn delete_directory(name: &str) -> String {
    format!("DeleteDirectory {name}")
}

/// `Skip {0}`（スキップの逐次ログ）。
pub fn skip(name: &str) -> String {
    format!("Skip {name}")
}

/// `CreateDirectory {0}`（ディレクトリ作成成功ログ）。
pub fn create_directory(name: &str) -> String {
    format!("CreateDirectory {name}")
}

/// `Rename {0} to {1}`（改名成功ログ）。
pub fn rename(old: &str, new: &str) -> String {
    format!("Rename {old} to {new}")
}

/// `{0} Success, {1} Skip, {2} Error`（コピー/移動の結果サマリ）。
pub fn copy_result(ok: usize, skip: usize, err: usize) -> String {
    format!("{ok} Success, {skip} Skip, {err} Error")
}

/// `{0} Success, {1} Error`（削除の結果サマリ）。
pub fn delete_result(ok: usize, err: usize) -> String {
    format!("{ok} Success, {err} Error")
}

/// `コピーに失敗しました。- {0}`（原作 {0}=ファイル名。理由も併記する）。
pub fn copy_failure(name: &str, reason: &str) -> String {
    format!("コピーに失敗しました。- {name} ({reason})")
}

/// `移動に失敗しました。- {0}`（原作 {0}=ファイル名。理由も併記する）。
pub fn move_failure(name: &str, reason: &str) -> String {
    format!("移動に失敗しました。- {name} ({reason})")
}

/// `削除に失敗しました。- {0}`（原作 {0}=ファイル名。理由も併記する）。
pub fn delete_failure(name: &str, reason: &str) -> String {
    format!("削除に失敗しました。- {name} ({reason})")
}

/// `名前を変更出来ません。- {0}`（原作 {0}=ファイル名。理由も併記する）。
pub fn rename_failure(name: &str, reason: &str) -> String {
    format!("名前を変更出来ません。- {name} ({reason})")
}

/// `ディレクトリが作成出来ません。- {0}`（原作 {0}=ディレクトリ名。理由も併記する）。
pub fn create_directory_failure(name: &str, reason: &str) -> String {
    format!("ディレクトリが作成出来ません。- {name} ({reason})")
}

/// `コピー先が同じです。- {0}`。
pub fn same_copy_path(name: &str) -> String {
    format!("コピー先が同じです。- {name}")
}

/// `移動先が同じです。- {0}`。
pub fn same_move_path(name: &str) -> String {
    format!("移動先が同じです。- {name}")
}

/// `すでに存在します。- {0}`。
pub fn all_ready_exists(name: &str) -> String {
    format!("すでに存在します。- {name}")
}

/// `ファイルが選択されていません。`。
pub fn not_selected_error() -> String {
    "ファイルが選択されていません。".to_owned()
}

/// `カレントが仮想ディレクトリです。`。
pub fn current_is_virtual_directory() -> String {
    "カレントが仮想ディレクトリです。".to_owned()
}

/// `{0}を削除してもよろしいですか？`（削除確認）。
pub fn delete_question(name: &str) -> String {
    format!("{name}を削除してもよろしいですか？")
}

/// `{0}は{1}ファイルです。削除してよろしいですか？`（属性付きファイルの削除確認）。
pub fn delete_warning_question(name: &str, attr: &str) -> String {
    format!("{name}は{attr}ファイルです。削除してよろしいですか？")
}

/// `{0}をコピーしてもよろしいですか？`（コピー確認）。
pub fn copy_question(name: &str) -> String {
    format!("{name}をコピーしてもよろしいですか？")
}

/// `{0}を移動してもよろしいですか？`（移動確認）。
pub fn move_question(name: &str) -> String {
    format!("{name}を移動してもよろしいですか？")
}

/// `ディレクトリ名を入力して下さい。`（ディレクトリ作成の入力プロンプト）。
pub fn directory_name_question() -> String {
    "ディレクトリ名を入力して下さい。".to_owned()
}
