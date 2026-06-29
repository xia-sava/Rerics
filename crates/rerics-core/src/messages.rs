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

/// `Copy {0} {1}%`（コピー進捗の逐次ログ更新）。
pub fn copy_progress(name: &str, percent: u32) -> String {
    format!("Copy {name} {percent}%")
}

/// `Move {0}`（移動操作の逐次ログ）。
pub fn move_(name: &str) -> String {
    format!("Move {name}")
}

/// `Move {0} {1}%`（移動進捗の逐次ログ更新）。
pub fn move_progress(name: &str, percent: u32) -> String {
    format!("Move {name} {percent}%")
}

/// `Delete {0}`（ファイル削除の逐次ログ）。
pub fn delete(name: &str) -> String {
    format!("Delete {name}")
}

/// `DeleteDirectory {0}`（ディレクトリ削除の逐次ログ）。
pub fn delete_directory(name: &str) -> String {
    format!("DeleteDirectory {name}")
}

/// `SendToRecycled {0}`（ゴミ箱送りの逐次ログ）。
pub fn send_to_recycled(name: &str) -> String {
    format!("SendToRecycled {name}")
}

/// `ゴミ箱送りに失敗しました。- {0}`（{0}=ファイル名。理由も併記する）。
pub fn send_to_recycled_failure(name: &str, reason: &str) -> String {
    format!("ゴミ箱送りに失敗しました。- {name} ({reason})")
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

/// 操作開始の枠ログ（例: `コピー開始`）。`verb` は `コピー`/`移動`/`削除`。
pub fn op_started(verb: &str) -> String {
    format!("{verb}開始")
}

/// 操作の正常終了の枠ログ（例: `コピー終了`）。
pub fn op_finished(verb: &str) -> String {
    format!("{verb}終了")
}

/// エラーを含む終了の枠ログ（例: `コピー警告終了`）。
pub fn op_finished_with_errors(verb: &str) -> String {
    format!("{verb}警告終了")
}

/// ユーザ中止の枠ログ（例: `コピー中止`）。
pub fn op_aborted(verb: &str) -> String {
    format!("{verb}中止")
}

/// `Compress {0}`（圧縮への追加の逐次ログ）。
pub fn compress(name: &str) -> String {
    format!("Compress {name}")
}

/// `圧縮に失敗しました。- {0}`（理由も併記する）。
pub fn compress_failure(name: &str, reason: &str) -> String {
    format!("圧縮に失敗しました。- {name} ({reason})")
}

/// `Add {0}`（書庫への追加の逐次ログ）。
pub fn archive_add(name: &str) -> String {
    format!("Add {name}")
}

/// `Add {0} {1}%`（書庫への追加のバイト進捗。インプレース更新行）。
pub fn archive_add_progress(name: &str, percent: u32) -> String {
    format!("Add {name} {percent}%")
}

/// `書庫への追加に失敗しました。- {0}`（理由も併記する）。
pub fn archive_add_failure(name: &str, reason: &str) -> String {
    format!("書庫への追加に失敗しました。- {name} ({reason})")
}

/// `Extract {0}`（非ランダムアクセス書庫の一括展開の開始行）。
pub fn archive_extract(name: &str) -> String {
    format!("Extract {name}")
}

/// `Extract {0} {1}%`（一括展開の進捗。インプレース更新行）。
pub fn archive_extract_progress(name: &str, percent: u32) -> String {
    format!("Extract {name} {percent}%")
}

/// `書庫の更新に失敗しました。- {0}`（再構築＝削除/改名/置換の共通失敗・理由併記）。
pub fn archive_update_failure(name: &str, reason: &str) -> String {
    format!("書庫の更新に失敗しました。- {name} ({reason})")
}

/// `Rebuild`（書庫を再構築して書き戻す処理の開始行）。
pub fn archive_rebuild() -> String {
    "Rebuild".to_string()
}

/// `Rebuild {0}%`（再構築の進捗。インプレース更新行）。
pub fn archive_rebuild_progress(percent: u32) -> String {
    format!("Rebuild {percent}%")
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

/// `ディレクトリ属性が異なるため{0}出来ません。- {1}`（ファイルとディレクトリの種別不一致で
/// スキップ）。`verb` は `コピー`/`移動`。
pub fn unmatch_attribute(verb: &str, name: &str) -> String {
    format!("ディレクトリ属性が異なるため{verb}出来ません。- {name}")
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

/// ディレクトリ使用量の結果（{0}=対象・{1}=バイト・{2}=ファイル数・{3}=フォルダ数）。
pub fn directory_information(target: &str, bytes: u64, files: u64, dirs: u64) -> String {
    format!("{target} : {bytes} バイト（{files} ファイル / {dirs} フォルダ）")
}

/// 使用量計算の進捗（走査済み件数。インプレース更新行）。
pub fn calc_size_progress(scanned: u64) -> String {
    format!("使用量計算中… {scanned} 件")
}

/// 使用量計算の完了（インプレース更新行を確定させる）。
pub fn calc_size_done(scanned: u64) -> String {
    format!("使用量計算 完了（{scanned} 件）")
}
