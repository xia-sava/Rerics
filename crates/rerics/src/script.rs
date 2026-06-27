//! TS/JS スクリプト基盤（V8 埋め込み）。
//!
//! スクリプトは別スレッドの V8 アイソレートで動き、`globalThis.rerics` 経由でホスト API を
//! 呼ぶ。GUI に触る操作は [`HostApi`] を介してUIスレッドへマーシャルする（実装は GUI 側）。
//! テストはモックの [`HostApi`] で同期的に検証する。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use deno_core::{JsRuntime, OpState, RuntimeOptions, extension, op2};

/// 非同期操作の進捗本文（`onProgress` へ渡す）。今は本文のみ・将来 件数等を足せる器。
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProgressInfo {
    pub text: String,
}

/// 進行中の非同期操作から JS へ 1 件ずつ流すイベント。進捗（`progress`）は 0 回以上続き、
/// 最後に完了（`done=true`・失敗/中止なら `error` 付き）が 1 度来てストリームが閉じる。
#[derive(serde::Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct JobEvent {
    /// 完了イベントなら true（このあと受信側は閉じる）。
    pub done: bool,
    /// 失敗・中止の理由（`done=true` 時のみ・成功は None）。
    pub error: Option<String>,
    /// 進捗イベントの本文（`done=false` 時のみ）。
    pub progress: Option<ProgressInfo>,
}

impl JobEvent {
    /// 進捗イベント。
    pub fn progress(text: String) -> Self {
        Self { done: false, error: None, progress: Some(ProgressInfo { text }) }
    }
    /// 完了イベント（`err`＝失敗/中止の理由・成功は None）。
    pub fn done(err: Option<String>) -> Self {
        Self { done: true, error: err, progress: None }
    }
}

/// 操作トークンへ流すイベントの送り口（ホスト＝UI 側が持つ）。進捗を複数回送れる。
pub type JobSender = tokio::sync::mpsc::UnboundedSender<JobEvent>;

/// 進行中の非同期操作のイベント受け口（トークン→`mpsc` 受信）。`op_op_start` が登録し、
/// `op_op_next` が 1 件ずつ取り出す。OpState に常駐させ、ops 間で共有する。
type JobReceivers = Rc<RefCell<HashMap<u64, tokio::sync::mpsc::UnboundedReceiver<JobEvent>>>>;

/// いま物理的に押されている修飾キーの状態（原作 `Filer.Shift`/`Control`/`Alt` 相当）。
/// JS では camelCase の真偽プロパティで見える。
#[derive(serde::Serialize, Clone, Copy, Default)]
#[serde(rename_all = "camelCase")]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// スクリプトからのホスト操作を受ける窓口。実 GUI 実装は UI スレッドへマーシャルし、
/// テストはモックで記録する。`&self` で受けるのは V8 アイソレートと同一スレッドから
/// 同期的に呼ばれるため。
pub trait HostApi {
    /// アプリのログ欄（実装依存）に指定レベルでメッセージを出す。
    fn log(&self, level: rerics_core::LogLevel, msg: &str);
    /// ログ欄の全文を返す（行は `\r\n` 区切り・末尾にも改行）。
    fn log_text(&self) -> String;
    /// 設定値をドット区切りキーで読む（未知キーは `None`）。
    fn config_get(&self, key: &str) -> Option<serde_json::Value>;
    /// 反対ペインを移動する。`kind`＝`"parent"`（親へ）/`"root"`（ルートへ）/その他（`path` へ）。
    fn change_opposite(&self, kind: &str, path: &str);
    /// アクティブペインの現在ディレクトリ（絶対パス）を返す。
    fn current_dir(&self) -> String;
    /// アクティブペインを `path` へ移動する。
    fn navigate(&self, path: &str);
    /// 確認ダイアログ（Yes/No）を出し、Yes なら true。
    fn confirm(&self, message: &str) -> bool;
    /// 入力ダイアログを出し、OK なら入力文字列・キャンセルなら None。
    fn prompt(&self, message: &str, default: &str) -> Option<String>;
    /// 一覧から 1 つ選ばせ、選んだ行の index・キャンセルなら None。
    fn select(&self, title: &str, items: &[String]) -> Option<usize>;
    /// ペイン（`opposite=false` でアクティブ・`true` で反対側）の現在状態を一括取得する。
    /// 別スレッド往復を 1 回で済ませるため、項目一覧ごとスナップショットで返す。
    fn pane_snapshot(&self, opposite: bool) -> PaneSnapshot;
    /// `is_left` 側ペインの `index` 行の選択状態を `selected` にする（即時・1 行）。
    fn set_selected(&self, is_left: bool, index: usize, selected: bool);
    /// `is_left` 側ペインの複数行の選択状態をまとめて適用する（再描画は 1 回）。
    fn apply_selection(&self, is_left: bool, changes: &[(usize, bool)]);
    /// 内蔵コマンドを名前で実行する（同期）。値返しクエリは値を、アクション系は `null` を返す。
    /// 不明な名前・実行失敗はエラー文字列を返す。ワーカーを起動する操作は「開始」までで戻り、完了は待たない。
    fn command(&self, name: &str, args: &[String]) -> Result<serde_json::Value, String>;
    /// 非同期ファイル操作を起動する。起動できたら**トークン**を返し、進行中は `events` へ進捗を
    /// 流し、完了時に完了イベント（成功 or 失敗/中止）を 1 度送る。`items` が空なら対象＝アクティブ
    /// ペインの選択（行き先＝反対ペイン）、非空なら対象＝そのパス群・行き先＝`dest`（delete では
    /// `dest` は無視）。起動できなければ（対象なし等）`Err`（その場合 `events` は使われない）。
    fn begin_operation(
        &self,
        op: ScriptOp,
        items: Vec<String>,
        dest: String,
        events: JobSender,
    ) -> Result<u64, String>;
    /// トークンで進行中の操作を中止する（未知のトークンは無視）。
    fn cancel_operation(&self, token: u64);
    /// パスを関連付けで開く（フォルダなら潜る／ファイルなら既定アプリ）。開きっぱなしで待たない。
    fn open(&self, path: &str);
    /// フォルダ選択ダイアログを開く（`title` 空なら既定見出し）。キャンセルは `None`。
    fn folder_dialog(&self, title: &str) -> Option<String>;
    /// ファイルを開くダイアログを開く（`title` 空なら既定見出し）。キャンセルは `None`。
    fn open_dialog(&self, title: &str) -> Option<String>;
    /// ファイル保存ダイアログを開く（`title` 空なら既定見出し）。キャンセルは `None`。
    fn save_dialog(&self, title: &str) -> Option<String>;
    /// いま押されている修飾キー（Shift/Ctrl/Alt）の状態を返す。物理キー状態なので UI スレッド
    /// 往復は不要（実装は直接読む）。
    fn modifiers(&self) -> Modifiers;
}

/// 外部プロセスを終了まで待った結果（`rerics.run` の戻り）。JS では camelCase で見える。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResult {
    /// 終了コード。シグナル等でコード無しに終わった場合は `null`。
    pub code: Option<i32>,
    /// 標準出力（UTF-8 として lossy 変換）。
    pub stdout: String,
    /// 標準エラー出力（UTF-8 として lossy 変換）。
    pub stderr: String,
}

/// 登録済みスクリプトコマンドのメタ情報（`registerCommand` の第3引数）。`label` は設定 UI の
/// 機能名カラムに出す日本語名、`genre` は機能順での見出しグループ、`summary` は補完やヘルプに
/// 出す 1 行説明（組込コマンドの [`rerics_core::CommandMeta::summary`] と同じ役割）。いずれも省略可。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ScriptCommand {
    /// 登録名（`invoke`／`script("name")` で指す識別子）。
    pub name: String,
    /// 設定 UI に出す表示名（無ければ既定の「スクリプト実行」を使う）。
    #[serde(default)]
    pub label: Option<String>,
    /// 機能順での所属ジャンル（既知ジャンル名なら組込群に混ぜ、未知なら「スクリプト」群へ）。
    #[serde(default)]
    pub genre: Option<String>,
    /// 補完・ヘルプに出す 1 行説明（組込の summary と同じインタフェース）。
    #[serde(default)]
    pub summary: Option<String>,
}

/// スクリプトが起動する非同期ファイル操作の種別。
pub enum ScriptOp {
    Copy,
    Move,
    Delete,
}

/// ペイン 1 つぶんの状態スナップショット（スクリプトへ渡す）。JS では camelCase で見える。
/// 項目アクセスのたびにスレッド往復しないよう、一覧を丸ごと 1 回で持って行く。
#[derive(serde::Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaneSnapshot {
    /// 現在地の表示パス（書庫内なら "C:\foo.zip\inner" 形式）。
    pub dir: String,
    /// 書庫内にいるか。
    pub is_archive: bool,
    /// 左ペインか（書き戻しを active/opposite ではなく具体側で指すための内部用）。
    pub is_left: bool,
    /// カーソル行の index（`items` の添字）。
    pub cursor: usize,
    /// 表示順の項目一覧（".." を含む）。
    pub items: Vec<PaneItem>,
    /// ソート種別のトークン（`getSortType` 用・`SortType::as_token`）。
    pub sort_type: String,
    /// ソートが逆順か（`getSortReverse` 用）。
    pub sort_reverse: bool,
    /// 現在のパスマスク（無ければ空文字・`getPathMask` 用）。
    pub path_mask: String,
}

/// ペイン内の 1 項目（スクリプトへ渡す）。コア `FileItem` を素直に写したもの。
#[derive(serde::Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaneItem {
    /// `items` 内での添字（将来の書き戻しで行を指す）。
    pub index: usize,
    /// フルパス（現在地と名前を結合したもの。明示的な操作対象指定に使える）。
    pub full_name: String,
    /// 表示名（拡張子込み）。
    pub name: String,
    /// 拡張子を除いた名前。
    pub base_name: String,
    /// 拡張子（ドット無し・無ければ空）。
    pub ext: String,
    pub is_dir: bool,
    /// 親（".."）エントリか。
    pub is_parent: bool,
    /// バイトサイズ（ディレクトリ・取得不可は 0）。
    pub size: u64,
    /// 最終更新時刻（Unix epoch ミリ秒・取得不可は 0）。
    pub mtime: u64,
    /// 選択（マーク）されているか。
    pub selected: bool,
    pub readonly: bool,
    pub hidden: bool,
}

type Host = Rc<dyn HostApi>;

/// ログレベル名（`info`/`warning`/`error`）を [`rerics_core::LogLevel`] へ。未知は `Normal`。
fn parse_log_level(name: &str) -> rerics_core::LogLevel {
    match name {
        "info" => rerics_core::LogLevel::Info,
        "warning" => rerics_core::LogLevel::Warning,
        "error" => rerics_core::LogLevel::Error,
        _ => rerics_core::LogLevel::Normal,
    }
}

#[op2(fast)]
fn op_log(state: &mut OpState, #[string] level: &str, #[string] msg: &str) {
    state.borrow::<Host>().log(parse_log_level(level), msg);
}

#[op2]
#[string]
fn op_log_text(state: &mut OpState) -> String {
    state.borrow::<Host>().log_text()
}

/// アプリのバージョン文字列（`Cargo.toml` の version）。
#[op2]
#[string]
fn op_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// ドット区切りキー（例 `"layout.border_unit"`）で JSON 値をたどる。各段はオブジェクトの
/// キー。途中で見つからなければ `None`。空キーは全体を返す。
pub fn config_lookup(root: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
    if key.is_empty() {
        return Some(root.clone());
    }
    let mut cur = root;
    for part in key.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur.clone())
}

/// 設定値をドット区切りキーで読む。`[]`＝未知キー・`[value]`＝値（JS 側で `null` へ畳む）。
#[op2]
#[serde]
fn op_config(state: &mut OpState, #[string] key: &str) -> Vec<serde_json::Value> {
    state.borrow::<Host>().config_get(key).into_iter().collect()
}

/// 反対ペインを移動する（投げっぱなし）。`kind`＝`"parent"`/`"root"`/その他（パス指定）。
#[op2(fast)]
fn op_change_opposite(state: &mut OpState, #[string] kind: &str, #[string] path: &str) {
    state.borrow::<Host>().change_opposite(kind, path);
}

#[op2]
#[string]
fn op_current_dir(state: &mut OpState) -> String {
    state.borrow::<Host>().current_dir()
}

#[op2(fast)]
fn op_navigate(state: &mut OpState, #[string] path: &str) {
    state.borrow::<Host>().navigate(path);
}

#[op2(fast)]
fn op_confirm(state: &mut OpState, #[string] message: &str) -> bool {
    state.borrow::<Host>().confirm(message)
}

/// 入力結果を `Vec` で包んで返す（`[]`＝キャンセル・`[s]`＝入力文字列）。op2 の同期 op は
/// `Option` を直に返せないため、JS 側で長さ 0/1 を `null`/値へ畳む。
#[op2]
#[serde]
fn op_prompt(state: &mut OpState, #[string] message: &str, #[string] default: &str) -> Vec<String> {
    state
        .borrow::<Host>()
        .prompt(message, default)
        .into_iter()
        .collect()
}

/// 選択結果を `Vec` で包んで返す（`[]`＝キャンセル・`[i]`＝選択 index）。理由は [`op_prompt`] と同じ。
#[op2]
#[serde]
fn op_select(
    state: &mut OpState,
    #[string] title: &str,
    #[serde] items: Vec<String>,
) -> Vec<u32> {
    state
        .borrow::<Host>()
        .select(title, &items)
        .map(|i| vec![i as u32])
        .unwrap_or_default()
}

/// ペイン状態のスナップショットを取得する同期 op（`opposite` で反対ペイン）。
#[op2]
#[serde]
fn op_pane_snapshot(state: &mut OpState, opposite: bool) -> PaneSnapshot {
    state.borrow::<Host>().pane_snapshot(opposite)
}

/// 1 行の選択状態を即時に書き戻す同期 op。
#[op2(fast)]
fn op_set_selected(state: &mut OpState, is_left: bool, index: u32, selected: bool) {
    state
        .borrow::<Host>()
        .set_selected(is_left, index as usize, selected);
}

/// 複数行の選択状態をまとめて書き戻す同期 op（`changes` は `[index, selected]` の配列）。
#[op2]
fn op_apply_selection(
    state: &mut OpState,
    is_left: bool,
    #[serde] changes: Vec<(u32, bool)>,
) {
    let changes: Vec<(usize, bool)> = changes.into_iter().map(|(i, v)| (i as usize, v)).collect();
    state.borrow::<Host>().apply_selection(is_left, &changes);
}

/// 内蔵コマンドを名前で実行する同期 op。値返しクエリは値を、アクション系は `null` を返す。
/// 不明な名前・実行失敗は JS の例外になる。
#[op2]
#[serde]
fn op_command(
    state: &mut OpState,
    #[string] name: &str,
    #[serde] args: Vec<String>,
) -> Result<serde_json::Value, deno_error::JsErrorBox> {
    state
        .borrow::<Host>()
        .command(name, &args)
        .map_err(deno_error::JsErrorBox::generic)
}

/// 組込コマンドのトークン名一覧。bootstrap がこれを回して `r.<token>()` の名前付き関数を
/// 動的生成する（Rust 側に個別 op を 127 本書かずに済ませる）。
#[op2]
#[serde]
fn op_builtin_commands() -> Vec<String> {
    rerics_core::Command::all().map(|c| c.as_token().to_owned()).collect()
}

/// ディレクトリ一覧の1エントリ（スクリプトへ渡す情報）。JS 側では camelCase で見える。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DirEntry {
    name: String,
    is_dir: bool,
    size: u64,
    /// 最終更新時刻（Unix epoch ミリ秒）。JS では `new Date(mtime)` で扱える。取得不可なら 0。
    mtime: u64,
}

/// `SystemTime` を Unix epoch ミリ秒へ。取得不可・1970 より前は 0。
fn epoch_millis(t: std::io::Result<std::time::SystemTime>) -> u64 {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `path` 直下を読み、各エントリの名前・ディレクトリ種別・サイズ・更新時刻を集める（同期・純 FS）。
fn read_dir_entries(path: &str) -> std::io::Result<Vec<DirEntry>> {
    let mut out = Vec::new();
    for ent in std::fs::read_dir(path)? {
        let ent = ent?;
        let md = ent.metadata()?;
        out.push(DirEntry {
            name: ent.file_name().to_string_lossy().into_owned(),
            is_dir: md.is_dir(),
            size: md.len(),
            mtime: epoch_millis(md.modified()),
        });
    }
    Ok(out)
}

/// 非同期ファイル操作を起動する同期 op。ホストへ起動を依頼し、得たトークン（タスク id）を
/// 返す。イベント受信用の `mpsc` を作り、受信側を `JobReceivers` に登録する（`op_op_next` が
/// 1 件ずつ取り出す）。起動できなければ例外。
#[op2]
fn op_op_start(
    state: &mut OpState,
    kind: u32,
    #[serde] items: Vec<String>,
    #[string] dest: &str,
) -> Result<u32, deno_error::JsErrorBox> {
    let op = match kind {
        0 => ScriptOp::Copy,
        1 => ScriptOp::Move,
        2 => ScriptOp::Delete,
        _ => return Err(deno_error::JsErrorBox::generic("unknown operation kind")),
    };
    let host = state.borrow::<Host>().clone();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<JobEvent>();
    let token = host
        .begin_operation(op, items, dest.to_string(), tx)
        .map_err(deno_error::JsErrorBox::generic)?;
    state.borrow::<JobReceivers>().borrow_mut().insert(token, rx);
    Ok(token as u32)
}

/// トークンの操作の次のイベントを 1 件待つ非同期 op。進捗なら `{progress}`、完了なら
/// `{done:true}`（失敗/中止は `{done:true, error}`）を返す。`JobReceivers` から受信側を
/// 借り出して待ち、完了でないなら受信側を戻す（完了なら破棄してストリームを閉じる）。
/// borrow を await 跨ぎにしないため take→await→reinsert する。
#[op2(async(lazy), nofast)]
#[serde]
async fn op_op_next(
    state: Rc<RefCell<OpState>>,
    token: u32,
) -> Result<JobEvent, deno_error::JsErrorBox> {
    let token = token as u64;
    let mut rx = match state.borrow().borrow::<JobReceivers>().borrow_mut().remove(&token) {
        Some(rx) => rx,
        None => return Err(deno_error::JsErrorBox::generic("unknown job token")),
    };
    let ev = rx.recv().await;
    match ev {
        Some(ev) => {
            // 完了でなければ次の受信に備えて受信側を戻す。完了ならそのまま閉じる。
            if !ev.done {
                state.borrow().borrow::<JobReceivers>().borrow_mut().insert(token, rx);
            }
            Ok(ev)
        }
        // 送り手が完了を送らずに落ちた（job が捨てられた）。
        None => Ok(JobEvent::done(Some("operation was dropped".to_string()))),
    }
}

/// トークンの操作を中止する同期 op。
#[op2(fast)]
fn op_op_cancel(state: &mut OpState, token: u32) {
    state.borrow::<Host>().cancel_operation(token as u64);
}

/// 重い走査を裏のブロッキングプールに逃がす非同期 op。GUI には触れない純粋処理なので
/// ホストを介さずそのまま `spawn_blocking` でき、スクリプトからは `await` するだけ。
#[op2(async(lazy), nofast)]
#[serde]
async fn op_list_dir(#[string] path: String) -> Result<Vec<DirEntry>, deno_error::JsErrorBox> {
    tokio::task::spawn_blocking(move || read_dir_entries(&path))
        .await
        .map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))?
        .map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))
}

/// パスを関連付けで開く（GUI 経由・開きっぱなし）。
#[op2(fast)]
fn op_open(state: &mut OpState, #[string] path: &str) {
    state.borrow::<Host>().open(path);
}

/// フォルダ選択結果を `Vec` で包んで返す（`[]`＝キャンセル・`[s]`＝選んだパス）。理由は [`op_prompt`] と同じ。
#[op2]
#[serde]
fn op_folder_dialog(state: &mut OpState, #[string] title: &str) -> Vec<String> {
    state.borrow::<Host>().folder_dialog(title).into_iter().collect()
}

/// ファイルを開くダイアログの結果を `Vec` で包んで返す（`[]`＝キャンセル・`[s]`＝選んだパス）。
#[op2]
#[serde]
fn op_open_dialog(state: &mut OpState, #[string] title: &str) -> Vec<String> {
    state.borrow::<Host>().open_dialog(title).into_iter().collect()
}

/// ファイル保存ダイアログの結果を `Vec` で包んで返す（`[]`＝キャンセル・`[s]`＝選んだパス）。
#[op2]
#[serde]
fn op_save_dialog(state: &mut OpState, #[string] title: &str) -> Vec<String> {
    state.borrow::<Host>().save_dialog(title).into_iter().collect()
}

/// いま押されている修飾キー（Shift/Ctrl/Alt）の状態を返す同期 op。
#[op2]
#[serde]
fn op_modifiers(state: &mut OpState) -> Modifiers {
    state.borrow::<Host>().modifiers()
}

/// 指定プログラムを起動して即リターンする（投げっぱなし）。`cwd` が非空ならそこを作業
/// ディレクトリにする。起動失敗は例外。GUI に触れないのでエンジンスレッドから直接起動する。
#[op2]
fn op_spawn(
    #[string] cmd: String,
    #[serde] args: Vec<String>,
    #[string] cwd: &str,
) -> Result<(), deno_error::JsErrorBox> {
    let mut command = std::process::Command::new(&cmd);
    command.args(&args);
    if !cwd.is_empty() {
        command.current_dir(cwd);
    }
    command
        .spawn()
        .map(|_child| ())
        .map_err(|e| deno_error::JsErrorBox::generic(format!("起動に失敗しました [{cmd}]: {e}")))
}

/// 指定プログラムを起動して終了まで待ち、結果（終了コード・標準出力・標準エラー）を返す
/// 非同期 op。`cwd` が非空ならそこを作業ディレクトリにする。重い待ちはブロッキングプールへ
/// 逃がす（`op_list_dir` と同じ形）。
#[op2(async(lazy))]
#[serde]
async fn op_run(
    #[string] cmd: String,
    #[serde] args: Vec<String>,
    #[string] cwd: String,
) -> Result<ProcessResult, deno_error::JsErrorBox> {
    tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(&cmd);
        command.args(&args);
        if !cwd.is_empty() {
            command.current_dir(&cwd);
        }
        command
            .output()
            .map(|o| ProcessResult {
                code: o.status.code(),
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            })
            .map_err(|e| format!("実行に失敗しました [{cmd}]: {e}"))
    })
    .await
    .map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))?
    .map_err(deno_error::JsErrorBox::generic)
}

/// 書庫 `src` の全エントリを `dst` 配下へ展開し、展開したファイル数を返す非同期 op。UI も
/// 確認も伴わず、`dst` は無ければ作る（zip-slip 防御は extract_all 側）。重い展開はブロッキング
/// プールへ逃がす（`op_list_dir` と同じ形）。GUI に触れないのでホストを介さない。
#[op2(async(lazy), nofast)]
async fn op_unpack(
    #[string] src: String,
    #[string] dst: String,
) -> Result<u32, deno_error::JsErrorBox> {
    tokio::task::spawn_blocking(move || {
        let backend = rerics_core::open_archive(std::path::Path::new(&src))
            .map_err(|e| format!("書庫を開けません [{src}]: {e}"))?;
        rerics_core::extract_all_to(&*backend, std::path::Path::new(&dst))
            .map(|n| n as u32)
            .map_err(|e| format!("展開に失敗しました [{src}]: {e}"))
    })
    .await
    .map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))?
    .map_err(deno_error::JsErrorBox::generic)
}

/// `rerics.fs.stat()` が返すメタデータ。存在しなければ JS 側で null に畳む。JS では camelCase で見える。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FsStat {
    is_dir: bool,
    is_file: bool,
    /// バイトサイズ（ディレクトリは 0）。
    size: u64,
    /// 最終更新時刻（Unix epoch ミリ秒・取得不可は 0）。
    mtime: u64,
    readonly: bool,
    hidden: bool,
}

/// メタデータの隠し属性を取る（非 Windows では常に false）。読取専用は `permissions().readonly()`。
#[cfg(windows)]
fn meta_hidden(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes() & 0x2 != 0
}
#[cfg(not(windows))]
fn meta_hidden(_md: &std::fs::Metadata) -> bool {
    false
}

/// fs プリミティブ：テキスト読み（UTF-8・不正なバイト列は例外）。バイナリ用は将来 `readBytes` を足す。
#[op2]
#[string]
fn op_fs_read_text(#[string] path: &str) -> Result<String, deno_error::JsErrorBox> {
    let bytes = std::fs::read(path)
        .map_err(|e| deno_error::JsErrorBox::generic(format!("読み込みに失敗しました [{path}]: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|_| deno_error::JsErrorBox::generic(format!("UTF-8 として読めません [{path}]")))
}

/// fs プリミティブ：テキスト書き（UTF-8・新規/上書き）。バイナリ用は将来 `writeBytes` を足す。
#[op2(fast)]
fn op_fs_write_text(
    #[string] path: &str,
    #[string] content: &str,
) -> Result<(), deno_error::JsErrorBox> {
    std::fs::write(path, content)
        .map_err(|e| deno_error::JsErrorBox::generic(format!("書き込みに失敗しました [{path}]: {e}")))
}

/// fs プリミティブ：ファイルコピー（中身を `dst` へ・上書き）。
#[op2(fast)]
fn op_fs_copy_file(
    #[string] src: &str,
    #[string] dst: &str,
) -> Result<(), deno_error::JsErrorBox> {
    std::fs::copy(src, dst)
        .map(|_| ())
        .map_err(|e| deno_error::JsErrorBox::generic(format!("コピーに失敗しました [{src}] → [{dst}]: {e}")))
}

/// fs プリミティブ：名前変更/移動（programmatic rename）。
#[op2(fast)]
fn op_fs_rename(
    #[string] src: &str,
    #[string] dst: &str,
) -> Result<(), deno_error::JsErrorBox> {
    std::fs::rename(src, dst)
        .map_err(|e| deno_error::JsErrorBox::generic(format!("名前変更に失敗しました [{src}] → [{dst}]: {e}")))
}

/// fs プリミティブ：ディレクトリ作成（途中も含め再帰作成・既存はそのまま成功）。
#[op2(fast)]
fn op_fs_mkdir(#[string] path: &str) -> Result<(), deno_error::JsErrorBox> {
    std::fs::create_dir_all(path)
        .map_err(|e| deno_error::JsErrorBox::generic(format!("ディレクトリ作成に失敗しました [{path}]: {e}")))
}

/// fs プリミティブ：存在判定（エラーは投げず false 寄せ）。
#[op2(fast)]
fn op_fs_exists(#[string] path: &str) -> bool {
    std::path::Path::new(path).exists()
}

/// fs プリミティブ：削除（ファイル or 空ディレクトリの非再帰削除）。中身ありディレクトリは例外。
#[op2(fast)]
fn op_fs_remove(#[string] path: &str) -> Result<(), deno_error::JsErrorBox> {
    let md = std::fs::metadata(path)
        .map_err(|e| deno_error::JsErrorBox::generic(format!("削除対象が見つかりません [{path}]: {e}")))?;
    let result = if md.is_dir() {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| deno_error::JsErrorBox::generic(format!("削除に失敗しました [{path}]: {e}")))
}

/// fs プリミティブ：stat。存在しなければ `[]`（JS で null へ畳む）・他の I/O エラーは例外。
/// `Option` を直に返せないため `op_prompt` と同じく長さ 0/1 の `Vec` で包む。
#[op2]
#[serde]
fn op_fs_stat(#[string] path: &str) -> Result<Vec<FsStat>, deno_error::JsErrorBox> {
    match std::fs::metadata(path) {
        Ok(md) => Ok(vec![FsStat {
            is_dir: md.is_dir(),
            is_file: md.is_file(),
            size: md.len(),
            mtime: epoch_millis(md.modified()),
            readonly: md.permissions().readonly(),
            hidden: meta_hidden(&md),
        }]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(deno_error::JsErrorBox::generic(format!("stat に失敗しました [{path}]: {e}"))),
    }
}

extension!(
    rerics_ext,
    ops = [
        op_log,
        op_log_text,
        op_version,
        op_config,
        op_change_opposite,
        op_current_dir,
        op_navigate,
        op_confirm,
        op_prompt,
        op_select,
        op_pane_snapshot,
        op_set_selected,
        op_apply_selection,
        op_command,
        op_builtin_commands,
        op_op_start,
        op_op_next,
        op_op_cancel,
        op_list_dir,
        op_open,
        op_folder_dialog,
        op_open_dialog,
        op_save_dialog,
        op_modifiers,
        op_spawn,
        op_run,
        op_unpack,
        op_fs_read_text,
        op_fs_write_text,
        op_fs_copy_file,
        op_fs_rename,
        op_fs_mkdir,
        op_fs_exists,
        op_fs_remove,
        op_fs_stat
    ]
);

/// `globalThis.rerics` を ops から組み立てるブートストラップ。スクリプト本体の前に1回走らせる。
/// 登録コマンドのコールバックは JS 側の Map に保持し、Rust からは名前で `__invokeCommand` を呼ぶ
/// （Rust は名前を持たず、一覧は `__commandNames` で JS Map から都度取る＝Map が唯一の真実）。
/// 末尾で `Deno` を遮蔽し、スクリプトから内部 op を直叩きできないようにする。
const BOOTSTRAP: &str = r#"
(() => {
  const ops = Deno.core.ops;
  const commands = new Map();
  const menus = new Map();
  const eventHandlers = new Map();
  // スナップショットから 1 ペインを組む。`sink(index, selected)` は item.selected を
  // 書いたときの送り先で、即時版は op を直に撃ち、apply() の draft 版は配列へ溜める。
  const buildPane = (snap, sink) => {
    const items = snap.items.map((raw) => {
      let sel = raw.selected;
      const it = {
        index: raw.index,
        fullName: raw.fullName,
        name: raw.name,
        baseName: raw.baseName,
        ext: raw.ext,
        isDir: raw.isDir,
        isParent: raw.isParent,
        size: raw.size,
        mtime: raw.mtime,
        readonly: raw.readonly,
        hidden: raw.hidden,
      };
      Object.defineProperty(it, "selected", {
        enumerable: true,
        get: () => sel,
        set: (v) => {
          sel = !!v;
          sink(raw.index, sel);
        },
      });
      return it;
    });
    const pane = {
      dir: snap.dir,
      isArchive: snap.isArchive,
      cursor: snap.cursor,
      items,
      get selectedItems() {
        return items.filter((it) => it.selected);
      },
      get cursorItem() {
        return items[snap.cursor] ?? null;
      },
      [Symbol.iterator]() {
        return items[Symbol.iterator]();
      },
      // 即時反映しない draft を渡し、コールバック内の選択変更を 1 往復でまとめて適用する。
      apply(fn) {
        const changes = [];
        const draft = buildPane(snap, (idx, v) => {
          changes.push([idx, v]);
        });
        fn(draft);
        if (changes.length) ops.op_apply_selection(snap.isLeft, changes);
        return pane;
      },
    };
    return pane;
  };
  // 即時版：item.selected への代入がその場で UI に反映される。
  const makePane = (snap) =>
    buildPane(snap, (idx, v) => ops.op_set_selected(snap.isLeft, idx, v));
  // 非同期操作を起動し、await できて .cancel() も持つ job を返す。op_op_start は起動失敗を
  // 例外にし、op_op_next を完了まで回す（進捗は onProgress へ・失敗/中止は reject）。
  // items が配列なら明示ベース（items＝パス配列・dest＝行き先）、null なら選択ベース。
  // opts.onProgress があれば進捗ごとに呼ぶ。
  const startOp = (kind, items, dest, opts) => {
    const onProgress = opts && typeof opts.onProgress === "function" ? opts.onProgress : null;
    const token = ops.op_op_start(kind, (items || []).map(String), dest == null ? "" : String(dest));
    const job = (async () => {
      for (;;) {
        const ev = await ops.op_op_next(token);
        if (!ev.done) {
          if (onProgress && ev.progress) {
            try {
              onProgress(ev.progress);
            } catch (e) {
              rerics.log("onProgress error: " + ((e && e.stack) || e));
            }
          }
          continue;
        }
        if (ev.error != null) throw new Error(ev.error);
        return;
      }
    })();
    job.cancel = () => ops.op_op_cancel(token);
    return job;
  };
  // copy/move：第1引数が配列なら明示(items, dest, opts)、オブジェクトなら選択ベース(opts)。
  const copyLike = (kind, a, b, c) =>
    Array.isArray(a) ? startOp(kind, a, b, c) : startOp(kind, null, null, a);
  // プロセス起動引数の末尾に { cwd } オプションが乗っていれば取り出す。残りは文字列引数。
  const splitProcArgs = (rest) => {
    let cwd = "";
    if (rest.length) {
      const last = rest[rest.length - 1];
      if (last !== null && typeof last === "object" && !Array.isArray(last)) {
        if (last.cwd != null) cwd = String(last.cwd);
        rest = rest.slice(0, -1);
      }
    }
    return { args: rest.map(String), cwd };
  };
  // VB の Like パターン（`*`=0文字以上・`?`=任意1文字・`#`=数字1文字・`[...]`/`[!...]`=文字クラス）
  // を正規表現へ。selectMask のマスク照合に使う（呼び手が大小を揃えて渡す）。
  const vbLikeToRegExp = (pattern) => {
    let re = "^";
    let i = 0;
    while (i < pattern.length) {
      const c = pattern[i];
      if (c === "*") { re += ".*"; i++; }
      else if (c === "?") { re += "."; i++; }
      else if (c === '#') { re += "[0-9]"; i++; }
      else if (c === "[") {
        let j = i + 1;
        let cls = "";
        if (pattern[j] === "!") { cls += "^"; j++; }
        while (j < pattern.length && pattern[j] !== "]") {
          const ch = pattern[j];
          cls += (ch === "\\" || ch === "]" || ch === "^") ? "\\" + ch : ch;
          j++;
        }
        if (j >= pattern.length) { re += "\\["; i++; }       // 閉じない [ はリテラル。
        else { re += "[" + cls + "]"; i = j + 1; }
      } else {
        re += c.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");       // リテラルはメタをエスケープ。
        i++;
      }
    }
    return new RegExp(re + "$");
  };
  globalThis.rerics = {
    log: (m) => ops.op_log("normal", String(m)),
    info: (m) => ops.op_log("info", String(m)),
    warning: (m) => ops.op_log("warning", String(m)),
    error: (m) => ops.op_log("error", String(m)),
    getLog: () => ops.op_log_text(),
    version: () => ops.op_version(),
    config: (key) => {
      const r = ops.op_config(String(key));
      return r.length ? r[0] : null;
    },
    currentDir: () => ops.op_current_dir(),
    isLeft: () => ops.op_pane_snapshot(false).isLeft,
    isRight: () => !ops.op_pane_snapshot(false).isLeft,
    currentDrive: () => {
      const m = /^([A-Za-z]:)/.exec(ops.op_current_dir());
      return m ? m[1] : "";
    },
    getSortType: () => ops.op_pane_snapshot(false).sortType,
    getSortReverse: () => ops.op_pane_snapshot(false).sortReverse,
    getPathMask: () => ops.op_pane_snapshot(false).pathMask,
    // カーソルの次の行から巡回して name に一致する項目へ移動し、見つかれば中央寄せして true。
    // 現在行は対象外。startwith=true（既定）で前方一致・false で部分一致。大小無視。
    incrementalSearch: (name, startwith) => {
      const sw = startwith === undefined ? true : !!startwith;
      const needle = String(name).toUpperCase();
      const snap = ops.op_pane_snapshot(false);
      const items = snap.items;
      const count = items.length;
      if (count === 0) return false;
      const start = snap.cursor;
      let num = start + 1;
      for (;;) {
        if (num >= count) num = 0;
        if (num === start) break;
        const text = String(items[num].name).toUpperCase();
        const hit = sw ? text.startsWith(needle) : text.indexOf(needle) >= 0;
        if (hit) {
          rerics.setCursorIndex(num);
          rerics.centerCursor();
          return true;
        }
        num++;
      }
      return false;
    },
    changeOppositeDirectory: (path) => ops.op_change_opposite("path", String(path)),
    changeOppositeDirectoryToParent: () => ops.op_change_opposite("parent", ""),
    changeOppositeDirectoryToRoot: () => ops.op_change_opposite("root", ""),
    // カンマ区切りの各マスク（VB Like）に一致する項目だけを選択し直す（既存選択はクリア）。
    // 1 件でも一致すれば true。大小無視。".." は対象外。
    selectMask: (mask) => {
      const patterns = String(mask).split(",").map((p) => p.trim()).filter((p) => p.length);
      const res = patterns.map((p) => vbLikeToRegExp(p.toUpperCase()));
      const snap = ops.op_pane_snapshot(false);
      const changes = [];
      let any = false;
      for (const it of snap.items) {
        if (it.isParent) continue;
        const name = it.name.toUpperCase();
        const want = res.some((r) => r.test(name));
        if (want) any = true;
        if (!!it.selected !== want) changes.push([it.index, want]);
      }
      if (changes.length) ops.op_apply_selection(snap.isLeft, changes);
      return any;
    },
    navigate: (p) => ops.op_navigate(String(p)),
    confirm: (m) => ops.op_confirm(String(m)),
    prompt: (m, d) => {
      const r = ops.op_prompt(String(m), d == null ? "" : String(d));
      return r.length ? r[0] : null;
    },
    select: (t, items) => {
      const r = ops.op_select(String(t), (items || []).map(String));
      return r.length ? r[0] : null;
    },
    listDir: (p) => ops.op_list_dir(String(p)),
    activePane: () => makePane(ops.op_pane_snapshot(false)),
    oppositePane: () => makePane(ops.op_pane_snapshot(true)),
    command: (name, ...args) => ops.op_command(String(name), args.map(String)),
    copy: (a, b, c) => copyLike(0, a, b, c),
    move: (a, b, c) => copyLike(1, a, b, c),
    delete: (a, b) =>
      Array.isArray(a) ? startOp(2, a, "", b) : startOp(2, null, "", a),
    open: (p) => ops.op_open(String(p)),
    folderDialog: (t) => {
      const r = ops.op_folder_dialog(t == null ? "" : String(t));
      return r.length ? r[0] : null;
    },
    openDialog: (t) => {
      const r = ops.op_open_dialog(t == null ? "" : String(t));
      return r.length ? r[0] : null;
    },
    saveDialog: (t) => {
      const r = ops.op_save_dialog(t == null ? "" : String(t));
      return r.length ? r[0] : null;
    },
    modifiers: () => ops.op_modifiers(),
    spawn: (cmd, ...rest) => {
      const { args, cwd } = splitProcArgs(rest);
      return ops.op_spawn(String(cmd), args, cwd);
    },
    run: (cmd, ...rest) => {
      const { args, cwd } = splitProcArgs(rest);
      return ops.op_run(String(cmd), args, cwd);
    },
    unpack: (src, dst) => ops.op_unpack(String(src), String(dst)),
    // 裏で動く低レベルファイル操作。画面にもログにも触れない（更新は呼び手が navigate 等で明示）。
    // 絶対パス前提。I/O エラーは例外（exists は false 寄せ・stat は不在で null）。
    fs: {
      readText: (p) => ops.op_fs_read_text(String(p)),
      writeText: (p, c) => ops.op_fs_write_text(String(p), c == null ? "" : String(c)),
      copyFile: (s, d) => ops.op_fs_copy_file(String(s), String(d)),
      rename: (s, d) => ops.op_fs_rename(String(s), String(d)),
      mkdir: (p) => ops.op_fs_mkdir(String(p)),
      exists: (p) => ops.op_fs_exists(String(p)),
      remove: (p) => ops.op_fs_remove(String(p)),
      stat: (p) => {
        const r = ops.op_fs_stat(String(p));
        return r.length ? r[0] : null;
      },
    },
    registerCommand: (name, fn, opts) => {
      if (typeof fn !== "function") throw new TypeError("registerCommand: fn must be a function");
      const o = opts || {};
      const key = String(name);
      commands.set(key, {
        fn,
        label: o.label == null ? null : String(o.label),
        genre: o.genre == null ? null : String(o.genre),
        summary: o.summary == null ? null : String(o.summary),
      });
      // 登録コマンドを r.<name>() でも呼べるようにする（式/コードから対象操作を書ける）。
      // 組込メンバーと衝突する名前は組込を優先し、r へは生やさない（マップには残る）。
      if (!builtinMembers.has(key)) rerics[key] = (...args) => fn(...args);
    },
    registerMenu: (name, items) => {
      if (!Array.isArray(items)) throw new TypeError("registerMenu: items must be an array");
      const norm = items.map((it) =>
        it && it.separator
          ? { label: "", command: "", separator: true }
          : {
              label: it && it.label != null ? String(it.label) : "",
              command: it && it.command != null ? String(it.command) : "",
              separator: false,
            }
      );
      menus.set(String(name), norm);
    },
    on: (event, fn) => {
      if (typeof fn !== "function") throw new TypeError("on: fn must be a function");
      const key = String(event);
      const list = eventHandlers.get(key);
      if (list) list.push(fn);
      else eventHandlers.set(key, [fn]);
    },
  };
  // 短縮別名。コマンド設定欄で `rerics.` が長いので `r.` で同じものを指せる。グローバルに
  // 1 度だけ置くことで、繰り返し eval しても再宣言エラーにならず、登録コマンド内でも使える。
  globalThis.r = globalThis.rerics;
  // 組込メンバー名の集合（この時点の rerics のキー）。登録コマンドの公開時に衝突判定へ使う。
  const builtinMembers = new Set(Object.keys(rerics));
  // 組込コマンドを r.<token>() の名前付き関数として生やす（今は戻り値を捨てる）。host API と
  // 同名（copy/move/delete 等）は host API を優先して上書きしない。生やした名前も組込メンバー
  // 扱いにして、同名の登録スクリプト関数が後から上書きするのを防ぐ。
  for (const token of ops.op_builtin_commands()) {
    if (!builtinMembers.has(token)) {
      rerics[token] = (...args) => rerics.command(token, ...args);
      builtinMembers.add(token);
    }
  }
  // ファイラー本体の出来事を登録ハンドラへ配る。1 つが投げても残りは続行する。
  globalThis.__fireEvent = (event, arg) => {
    const list = eventHandlers.get(String(event));
    if (!list) return;
    const report = (e) =>
      rerics.log("event error [" + event + "]: " + ((e && e.stack) || e));
    for (const fn of list) {
      try {
        const r = fn(arg);
        if (r && typeof r.then === "function") r.then(undefined, report);
      } catch (e) {
        report(e);
      }
    }
  };
  globalThis.__commandNames = () => [...commands.keys()];
  // 補完候補＝`r.` で呼べるもの（組込メンバー＋公開済み登録コマンド）の名前を昇順で返す。
  globalThis.__memberNames = () => Object.keys(globalThis.rerics).sort();
  globalThis.__commandMetas = () =>
    [...commands.entries()].map(([name, e]) => ({
      name,
      label: e.label,
      genre: e.genre,
      summary: e.summary,
    }));
  globalThis.__menuDefs = () =>
    [...menus.entries()].map(([name, items]) => ({ name, items }));
  globalThis.__invokeCommand = (name, ...args) => {
    const entry = commands.get(String(name));
    if (!entry) throw new Error("unknown command: " + name);
    const fn = entry.fn;
    const report = (e) => rerics.log("command error [" + name + "]: " + ((e && e.stack) || e));
    try {
      const r = fn(...args);
      if (r && typeof r.then === "function") return r.then(undefined, report);
      return r;
    } catch (e) {
      report(e);
    }
  };
})();
delete globalThis.Deno;
"#;

/// TS/JS ソースを型消去して実行可能な JS にする。`.ts` の型注釈・interface 等を落とす
/// （swc ベース・型検査はしない＝エディタ側の責務）。`specifier` はスタック表示用の URL。
pub fn transpile(
    specifier: &str,
    media_type: deno_ast::MediaType,
    source: String,
) -> Result<String, String> {
    let parsed = deno_ast::parse_module(deno_ast::ParseParams {
        specifier: deno_ast::ModuleSpecifier::parse(specifier).map_err(|e| e.to_string())?,
        text: source.into(),
        media_type,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|e| e.to_string())?;
    let emitted = parsed
        .transpile(
            &deno_ast::TranspileOptions::default(),
            &deno_ast::TranspileModuleOptions::default(),
            &deno_ast::EmitOptions::default(),
        )
        .map_err(|e| e.to_string())?;
    Ok(emitted.into_source().text)
}

/// 拡張子から MediaType を決める（`.ts`→TypeScript・それ以外→JavaScript）。
pub fn media_type_for(path: &std::path::Path) -> deno_ast::MediaType {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts") => deno_ast::MediaType::TypeScript,
        _ => deno_ast::MediaType::JavaScript,
    }
}

/// `dir` 直下の `.ts`/`.js` をファイル名順に読み、それぞれ型消去して実行する。スクリプトの
/// 標準的な読込口。1本が失敗しても残りは続行し、失敗した `(パス, メッセージ)` を集めて返す。
/// `dir` が存在しない場合は「スクリプト無し」として静かに空を返す。
pub fn load_dir(engine: &mut Engine, dir: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
    let mut errors = Vec::new();
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("ts") | Some("js")
                )
            })
            .collect(),
        Err(_) => return errors,
    };
    files.sort();
    for path in files {
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                let spec = format!("file:///{}", path.display().to_string().replace('\\', "/"));
                if let Err(e) = engine.run_ts("rerics:script", &spec, media_type_for(&path), src) {
                    errors.push((path, e));
                }
            }
            Err(e) => errors.push((path, e.to_string())),
        }
    }
    errors
}

/// 1スレッドぶんのスクリプト実行環境。V8 アイソレートと、非同期 op（Promise）を
/// 駆動するための current-thread tokio ランタイムを抱える。
pub struct Engine {
    runtime: JsRuntime,
    tokio_rt: tokio::runtime::Runtime,
}

impl Engine {
    /// ホスト API を結びつけた実行環境を作る。`globalThis.rerics` を即座に用意する。
    pub fn new(host: Host) -> Self {
        let tokio_rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread tokio runtime");
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![rerics_ext::init()],
            ..Default::default()
        });
        {
            let state = runtime.op_state();
            let mut state = state.borrow_mut();
            state.put::<Host>(host);
            state.put::<JobReceivers>(Rc::new(RefCell::new(HashMap::new())));
        }
        runtime
            .execute_script("rerics:bootstrap", BOOTSTRAP)
            .expect("bootstrap script must not fail");
        Self { runtime, tokio_rt }
    }

    /// 現在登録されているコマンド名（JS 側 Map のキー＝登録順・同名は後勝ちで一意）。
    /// 本体は名前＋メタの [`Engine::registered_command_metas`] を使うので、これは検証用。
    #[cfg(test)]
    pub fn registered_commands(&mut self) -> Vec<String> {
        let global = self
            .runtime
            .execute_script("rerics:list-commands", "globalThis.__commandNames()")
            .expect("__commandNames must not fail");
        deno_core::scope!(scope, &mut self.runtime);
        let local = deno_core::v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8::<Vec<String>>(scope, local).unwrap_or_default()
    }

    /// 登録済みコマンドのメタ情報（名前・表示名・ジャンル）を登録順で返す。設定 UI が
    /// スクリプト行のラベル／ジャンルを描くのに使う。`label`/`genre` 未指定は `None`。
    pub fn registered_command_metas(&mut self) -> Vec<ScriptCommand> {
        let global = self
            .runtime
            .execute_script("rerics:list-metas", "globalThis.__commandMetas()")
            .expect("__commandMetas must not fail");
        deno_core::scope!(scope, &mut self.runtime);
        let local = deno_core::v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8::<Vec<ScriptCommand>>(scope, local).unwrap_or_default()
    }

    /// `registerMenu` で登録された名前付きメニュー定義を登録順で返す。`menu("名前")` の解決時に
    /// config 定義とマージする。
    pub fn registered_menus(&mut self) -> Vec<rerics_core::MenuDef> {
        let global = self
            .runtime
            .execute_script("rerics:list-menus", "globalThis.__menuDefs()")
            .expect("__menuDefs must not fail");
        deno_core::scope!(scope, &mut self.runtime);
        let local = deno_core::v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8::<Vec<rerics_core::MenuDef>>(scope, local).unwrap_or_default()
    }

    /// `r.` で呼べるメンバー名を昇順で返す（組込ホスト API＋公開済み登録コマンド）。設定 UI の
    /// 引数/コード欄の補完候補に使う。
    pub fn registered_member_names(&mut self) -> Vec<String> {
        let global = self
            .runtime
            .execute_script("rerics:list-members", "globalThis.__memberNames()")
            .expect("__memberNames must not fail");
        deno_core::scope!(scope, &mut self.runtime);
        let local = deno_core::v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8::<Vec<String>>(scope, local).unwrap_or_default()
    }

    /// 登録済みイベントハンドラを発火する（`rerics.on` で登録したもの）。`arg` は単一の
    /// 文字列ペイロード。未登録イベントは無音。ハンドラが非同期でも Promise を完了させる。
    pub fn fire_event(&mut self, event: &str, arg: &str) -> Result<(), String> {
        let e = serde_json::to_string(event).map_err(|e| e.to_string())?;
        let a = serde_json::to_string(arg).map_err(|e| e.to_string())?;
        let code = format!("globalThis.__fireEvent({e}, {a});");
        self.run_to_completion("rerics:event", code)
            .map_err(|e| e.to_string())
    }

    /// 登録済みコマンドを名前で実行する。`args` はコールバックへ転送する。コールバックが
    /// 非同期でも Promise を完了させる。
    pub fn invoke_command(&mut self, name: &str, args: &[String]) -> Result<(), String> {
        let mut call_args = Vec::with_capacity(args.len() + 1);
        call_args.push(name.to_owned());
        call_args.extend_from_slice(args);
        let json = serde_json::to_string(&call_args).map_err(|e| e.to_string())?;
        let code = format!("globalThis.__invokeCommand(...{json});");
        self.run_to_completion("rerics:invoke", code)
            .map_err(|e| e.to_string())
    }

    /// JS ソースを実行し、イベントループが空になるまで回す（async op / Promise を完了させる）。
    pub fn run_to_completion(
        &mut self,
        name: &'static str,
        code: String,
    ) -> Result<(), deno_core::error::CoreError> {
        let Self {
            runtime, tokio_rt, ..
        } = self;
        tokio_rt.block_on(async {
            runtime.execute_script(name, code)?;
            runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default())
                .await?;
            Ok(())
        })
    }

    /// TS/JS ソースを型消去してから実行し、Promise を完了させる。スクリプト読込の標準経路。
    pub fn run_ts(
        &mut self,
        name: &'static str,
        specifier: &str,
        media_type: deno_ast::MediaType,
        source: String,
    ) -> Result<(), String> {
        let js = transpile(specifier, media_type, source)?;
        self.run_to_completion(name, js).map_err(|e| e.to_string())
    }

    /// TS/JS コードを評価し、最後の式の値を文字列で返す。値が Promise なら解決を待つ。
    /// `undefined`/`null` は空文字にする。第3弾の「式をコマンド引数にする」評価の土台。
    pub fn eval_to_string(
        &mut self,
        name: &'static str,
        specifier: &str,
        media_type: deno_ast::MediaType,
        source: String,
    ) -> Result<String, String> {
        let js = transpile(specifier, media_type, source)?;
        let Self { runtime, tokio_rt } = self;
        tokio_rt.block_on(async {
            let value = runtime.execute_script(name, js).map_err(|e| e.to_string())?;
            let resolved = runtime.resolve(value);
            let value = runtime
                .with_event_loop_promise(resolved, deno_core::PollEventLoopOptions::default())
                .await
                .map_err(|e| e.to_string())?;
            deno_core::scope!(scope, runtime);
            let local = deno_core::v8::Local::new(scope, value);
            if local.is_null_or_undefined() {
                Ok(String::new())
            } else {
                Ok(local.to_rust_string_lossy(scope))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `apply_selection` 1 回ぶんの記録＝(is_left, 変更 (index, selected) の列)。
    type AppliedSelection = (bool, Vec<(usize, bool)>);

    #[derive(Default)]
    struct MockHost {
        logs: RefCell<Vec<String>>,
        dir: String,
        navigated: RefCell<Vec<String>>,
        confirm_reply: bool,
        prompt_reply: Option<String>,
        select_reply: Option<usize>,
        active_pane: PaneSnapshot,
        opposite_pane: PaneSnapshot,
        set_selected: RefCell<Vec<(bool, usize, bool)>>,
        applied: RefCell<Vec<AppliedSelection>>,
        commands: RefCell<Vec<(String, Vec<String>)>>,
        /// この名前のコマンドは失敗させる（エラー経路の検証用）。
        failing_command: Option<String>,
        operations: RefCell<Vec<&'static str>>,
        cancelled: RefCell<Vec<u64>>,
        /// begin_operation が完了前に流す進捗本文（onProgress 検証用）。
        op_progress: Vec<String>,
        /// Some なら操作を失敗（中止）として完了させる（reject 検証用）。
        op_error: Option<String>,
        /// `open` で開こうとしたパス（関連付け起動の検証用）。
        opened: RefCell<Vec<String>>,
        /// フォルダ/開く/保存ダイアログの戻り（None でキャンセル）。
        folder_reply: Option<String>,
        open_reply: Option<String>,
        save_reply: Option<String>,
        /// `modifiers()` が返す修飾キー状態。
        modifiers: Modifiers,
        /// `config_get()` が引くルート JSON（ドット区切りキーでたどる）。
        config: serde_json::Value,
        /// `change_opposite()` が受けた `(kind, path)` の記録。
        opposite_nav: RefCell<Vec<(String, String)>>,
    }

    impl HostApi for MockHost {
        fn log(&self, _level: rerics_core::LogLevel, m: &str) {
            self.logs.borrow_mut().push(m.to_string());
        }
        fn log_text(&self) -> String {
            self.logs.borrow().iter().map(|l| format!("{l}\r\n")).collect()
        }
        fn config_get(&self, key: &str) -> Option<serde_json::Value> {
            config_lookup(&self.config, key)
        }
        fn change_opposite(&self, kind: &str, path: &str) {
            self.opposite_nav.borrow_mut().push((kind.to_string(), path.to_string()));
        }
        fn current_dir(&self) -> String {
            self.dir.clone()
        }
        fn navigate(&self, p: &str) {
            self.navigated.borrow_mut().push(p.to_string());
        }
        fn confirm(&self, _message: &str) -> bool {
            self.confirm_reply
        }
        fn prompt(&self, _message: &str, _default: &str) -> Option<String> {
            self.prompt_reply.clone()
        }
        fn select(&self, _title: &str, _items: &[String]) -> Option<usize> {
            self.select_reply
        }
        fn pane_snapshot(&self, opposite: bool) -> PaneSnapshot {
            if opposite {
                self.opposite_pane.clone()
            } else {
                self.active_pane.clone()
            }
        }
        fn set_selected(&self, is_left: bool, index: usize, selected: bool) {
            self.set_selected.borrow_mut().push((is_left, index, selected));
        }
        fn apply_selection(&self, is_left: bool, changes: &[(usize, bool)]) {
            self.applied.borrow_mut().push((is_left, changes.to_vec()));
        }
        fn command(&self, name: &str, args: &[String]) -> Result<serde_json::Value, String> {
            if self.failing_command.as_deref() == Some(name) {
                return Err(format!("boom: {name}"));
            }
            self.commands.borrow_mut().push((name.to_string(), args.to_vec()));
            Ok(serde_json::Value::Null)
        }
        fn begin_operation(
            &self,
            op: ScriptOp,
            _items: Vec<String>,
            _dest: String,
            events: JobSender,
        ) -> Result<u64, String> {
            self.operations.borrow_mut().push(match op {
                ScriptOp::Copy => "copy",
                ScriptOp::Move => "move",
                ScriptOp::Delete => "delete",
            });
            // 進捗→完了を即座に流す（実 GUI ではワーカーから順次送られる）。トークンは件数で代用。
            for text in &self.op_progress {
                let _ = events.send(JobEvent::progress(text.clone()));
            }
            let _ = events.send(JobEvent::done(self.op_error.clone()));
            Ok(self.operations.borrow().len() as u64)
        }
        fn cancel_operation(&self, token: u64) {
            self.cancelled.borrow_mut().push(token);
        }
        fn open(&self, path: &str) {
            self.opened.borrow_mut().push(path.to_string());
        }
        fn folder_dialog(&self, _title: &str) -> Option<String> {
            self.folder_reply.clone()
        }
        fn open_dialog(&self, _title: &str) -> Option<String> {
            self.open_reply.clone()
        }
        fn save_dialog(&self, _title: &str) -> Option<String> {
            self.save_reply.clone()
        }
        fn modifiers(&self) -> Modifiers {
            self.modifiers
        }
    }

    /// テスト用に名前・ディレクトリ種別・選択状態だけ与えた項目を作る。
    /// `base_name`/`ext` は名前の末尾ドットから導く（ファイルのみ）。
    fn item(index: usize, name: &str, is_dir: bool, selected: bool) -> PaneItem {
        let (base_name, ext) = match name.rfind('.') {
            Some(p) if !is_dir && p > 0 => (name[..p].to_string(), name[p + 1..].to_string()),
            _ => (name.to_string(), String::new()),
        };
        PaneItem {
            index,
            full_name: name.to_string(),
            name: name.to_string(),
            base_name,
            ext,
            is_dir,
            is_parent: name == "..",
            size: 0,
            mtime: 0,
            selected,
            readonly: false,
            hidden: false,
        }
    }

    #[test]
    fn script_logs_and_navigates_through_host() {
        let host = Rc::new(MockHost {
            dir: "C:\\tmp".into(),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:basic",
            r#"rerics.log("hi"); rerics.navigate(rerics.currentDir() + "\\sub");"#.to_string(),
        )
        .unwrap();
        assert_eq!(*host.logs.borrow(), vec!["hi".to_string()]);
        assert_eq!(*host.navigated.borrow(), vec!["C:\\tmp\\sub".to_string()]);
    }

    #[test]
    fn leveled_log_getlog_and_version() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:log",
            r#"
              rerics.log("plain");
              rerics.info("ok");
              rerics.warning("hmm");
              rerics.error("boom");
              rerics.log("len=" + rerics.getLog().split("\r\n").filter(s => s).length);
              rerics.log("ver=" + (rerics.version().length > 0));
            "#
            .to_string(),
        )
        .unwrap();
        // MockHost はレベルを無視して本文だけ溜める。getLog はそれを \r\n 連結で返す。
        assert_eq!(
            *host.logs.borrow(),
            vec![
                "plain".to_string(),
                "ok".to_string(),
                "hmm".to_string(),
                "boom".to_string(),
                // getLog 呼び出し時点で 4 行溜まっている。
                "len=4".to_string(),
                "ver=true".to_string(),
            ]
        );
    }

    #[test]
    fn config_reads_by_dotted_key() {
        let host = Rc::new(MockHost {
            config: serde_json::json!({
                "editor": "notepad.exe",
                "layout": { "border_unit": 50 },
                "cursor": { "to_parent": false },
            }),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:config",
            r#"
              rerics.log("editor=" + rerics.config("editor"));
              rerics.log("unit=" + rerics.config("layout.border_unit"));
              rerics.log("toParent=" + rerics.config("cursor.to_parent"));
              rerics.log("missing=" + (rerics.config("no.such.key") === null));
              rerics.log("obj=" + JSON.stringify(rerics.config("layout")));
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec![
                "editor=notepad.exe".to_string(),
                "unit=50".to_string(),
                "toParent=false".to_string(),
                "missing=true".to_string(),
                "obj={\"border_unit\":50}".to_string(),
            ]
        );
    }

    #[test]
    fn async_list_dir_runs_off_thread_and_resolves() {
        // 一時ディレクトリに 2 ファイル + 1 サブディレクトリを作る。
        let dir = std::env::temp_dir().join(format!("rerics-script-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("b.txt"), b"x").unwrap();

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        // バックスラッシュ回避のためスラッシュ表記で渡す（Windows でも read_dir は受ける）。
        let p = dir.display().to_string().replace('\\', "/");
        let code = format!(
            r#"(async () => {{
                 const entries = await rerics.listDir("{p}");
                 rerics.log("count=" + entries.length);
                 rerics.log("dirs=" + entries.filter(e => e.isDir).length);
                 rerics.log("dated=" + entries.filter(e => e.mtime > 0).length);
               }})();"#
        );
        eng.run_to_completion("test:async", code).unwrap();

        assert_eq!(
            *host.logs.borrow(),
            vec!["count=3".to_string(), "dirs=1".to_string(), "dated=3".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_passes_path_to_host() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion("test:open", r#"rerics.open("C:\\tmp\\file.txt");"#.to_string())
            .unwrap();
        assert_eq!(*host.opened.borrow(), vec!["C:\\tmp\\file.txt".to_string()]);
    }

    #[test]
    fn spawn_launches_process_fire_and_forget() {
        let dir = std::env::temp_dir().join(format!("rerics-spawn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("spawned.txt");

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        // copy /y nul <path> で空ファイルを作る。投げっぱなしなので後でファイル出現を待つ。
        let code = format!(
            r#"rerics.spawn("cmd", "/c", "copy", "/y", "nul", {:?});"#,
            marker.display().to_string()
        );
        eng.run_to_completion("test:spawn", code).unwrap();

        let appeared = (0..50).any(|_| {
            if marker.exists() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            false
        });
        assert!(appeared, "spawn したプロセスが空ファイルを作るはず: {}", marker.display());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_waits_for_process_and_returns_output() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        let code = r#"(async () => {
            const r = await rerics.run("cmd", "/c", "echo", "hi-from-run");
            rerics.log("code=" + r.code);
            rerics.log("out=" + r.stdout.trim());
        })();"#
            .to_string();
        eng.run_to_completion("test:run", code).unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec!["code=0".to_string(), "out=hi-from-run".to_string()]
        );
    }

    #[test]
    fn run_honors_cwd_option() {
        let dir = std::env::temp_dir().join(format!("rerics-cwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.display().to_string().replace('\\', "/");

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        // 末尾の { cwd } で作業ディレクトリを指定。`cmd /c cd` が現在地を出すので、その文字列に
        // 一意な作業dir名が含まれるかで cwd 反映を確認する。
        let code = format!(
            r#"(async () => {{
                 const r = await rerics.run("cmd", "/c", "cd", {{ cwd: "{p}" }});
                 rerics.log(r.stdout.toLowerCase().includes("rerics-cwd") ? "in-cwd" : "elsewhere:" + r.stdout.trim());
               }})();"#
        );
        eng.run_to_completion("test:cwd", code).unwrap();
        assert_eq!(*host.logs.borrow(), vec!["in-cwd".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_dialogs_round_trip_through_host() {
        let host = Rc::new(MockHost {
            folder_reply: Some("E:\\picked".into()),
            open_reply: Some("C:\\in.txt".into()),
            save_reply: None,
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:file-dialogs",
            r#"
              rerics.log("f=" + rerics.folderDialog("フォルダ"));
              rerics.log("o=" + rerics.openDialog());
              rerics.log("s=" + rerics.saveDialog("保存先"));
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec!["f=E:\\picked".to_string(), "o=C:\\in.txt".to_string(), "s=null".to_string()]
        );
    }

    #[test]
    fn typescript_types_are_erased_and_runs() {
        let host = Rc::new(MockHost {
            dir: "C:\\t".into(),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        let ts = r#"
            interface Person { name: string }
            function greet(p: Person): string { return "hi " + p.name; }
            const who: string = rerics.currentDir();
            rerics.log(greet({ name: who }));
        "#;
        eng.run_ts(
            "test:ts",
            "file:///rerics/test.ts",
            deno_ast::MediaType::TypeScript,
            ts.to_string(),
        )
        .unwrap();
        assert_eq!(*host.logs.borrow(), vec!["hi C:\\t".to_string()]);
    }

    #[test]
    fn register_then_invoke_command_calls_back_into_host() {
        let host = Rc::new(MockHost {
            dir: "C:\\base".into(),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:register",
            r#"rerics.registerCommand("up", () => rerics.navigate(rerics.currentDir() + "/.."));"#
                .to_string(),
        )
        .unwrap();
        assert_eq!(eng.registered_commands(), vec!["up".to_string()]);
        assert!(host.navigated.borrow().is_empty());

        eng.invoke_command("up", &[]).unwrap();
        assert_eq!(*host.navigated.borrow(), vec!["C:\\base/..".to_string()]);

        assert!(eng.invoke_command("missing", &[]).is_err());
    }

    #[test]
    fn invoke_command_forwards_args_to_callback() {
        let host = Rc::new(MockHost {
            dir: "C:\\base".into(),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:args",
            r#"rerics.registerCommand("go", (p) => rerics.navigate(String(p)));"#.to_string(),
        )
        .unwrap();
        // script("go", "C:\\target") 相当＝引数がコールバックへ転送される（Func_ シムの実体）。
        eng.invoke_command("go", &["C:\\target".to_string()]).unwrap();
        assert_eq!(*host.navigated.borrow(), vec!["C:\\target".to_string()]);
    }

    #[test]
    fn register_command_metadata_is_exposed() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:meta",
            r#"
              rerics.registerCommand("organize", () => {}, { label: "整理する", genre: "片付け", summary: "散らかりを整える" });
              rerics.registerCommand("onlyLabel", () => {}, { label: "ラベルだけ" });
              rerics.registerCommand("plain", () => {});
            "#
            .to_string(),
        )
        .unwrap();
        let metas = eng.registered_command_metas();
        assert_eq!(
            metas,
            vec![
                ScriptCommand {
                    name: "organize".into(),
                    label: Some("整理する".into()),
                    genre: Some("片付け".into()),
                    summary: Some("散らかりを整える".into()),
                },
                ScriptCommand {
                    name: "onlyLabel".into(),
                    label: Some("ラベルだけ".into()),
                    genre: None,
                    summary: None,
                },
                ScriptCommand { name: "plain".into(), label: None, genre: None, summary: None },
            ]
        );
        // 名前一覧は従来どおり（メタ化で壊れない）。
        assert_eq!(eng.registered_commands(), vec!["organize", "onlyLabel", "plain"]);
    }

    #[test]
    fn register_menu_is_exposed_as_menu_defs() {
        use rerics_core::{MenuDef, MenuItem};
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:menu",
            r#"
              rerics.registerMenu("編集", [
                { label: "コピー", command: "copy" },
                { separator: true },
                { label: "サブ", command: 'menu("他")' },
              ]);
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            eng.registered_menus(),
            vec![MenuDef {
                name: "編集".into(),
                items: vec![
                    MenuItem::entry("コピー", "copy"),
                    MenuItem::separator(),
                    MenuItem::entry("サブ", "menu(\"他\")"),
                ],
            }]
        );
    }

    #[test]
    fn registered_command_is_callable_via_r_namespace() {
        let host = Rc::new(MockHost {
            dir: "C:\\base".into(),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:register",
            r#"rerics.registerCommand("up", () => rerics.navigate(rerics.currentDir() + "/.."));"#
                .to_string(),
        )
        .unwrap();
        // 登録コマンドは r.<name>() でも呼べる（式/コードから対象操作を書ける）。
        eng.run_to_completion("test:call", "r.up();".to_string()).unwrap();
        assert_eq!(*host.navigated.borrow(), vec!["C:\\base/..".to_string()]);
    }

    #[test]
    fn member_names_merge_builtins_and_commands_builtin_wins_on_clash() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:register",
            r#"
              rerics.registerCommand("organize", () => {});
              rerics.registerCommand("prompt", () => 999);
            "#
            .to_string(),
        )
        .unwrap();
        let members = eng.registered_member_names();
        // 組込メンバーと公開済み登録コマンドが混ざって昇順で並ぶ。
        assert!(members.contains(&"currentDir".to_string()), "組込: {members:?}");
        assert!(members.contains(&"organize".to_string()), "登録コマンド: {members:?}");
        let mut sorted = members.clone();
        sorted.sort();
        assert_eq!(members, sorted, "昇順: {members:?}");
        // 衝突した "prompt" は組込が優先＝r.prompt は HostApi のまま（コマンドの 999 では上書きされない）。
        eng.run_to_completion("test:clash", r#"rerics.log(String(r.prompt("m")));"#.to_string())
            .unwrap();
        assert_eq!(*host.logs.borrow(), vec!["null".to_string()], "組込 prompt が勝つ");
    }

    #[test]
    fn load_dir_runs_ts_and_js_in_name_order() {
        let dir = std::env::temp_dir().join(format!("rerics-load-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 名前順で 10→20 の順に走るはず。.ts と .js を混在させる。
        std::fs::write(
            dir.join("20-second.js"),
            r#"rerics.registerCommand("second", () => {});"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("10-first.ts"),
            r#"const n: number = 1; rerics.registerCommand("first", (): void => {});"#,
        )
        .unwrap();

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host);
        let errors = load_dir(&mut eng, &dir);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(
            eng.registered_commands(),
            vec!["first".to_string(), "second".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_dir_missing_directory_is_silent() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host);
        let errors = load_dir(&mut eng, std::path::Path::new("C:\\no\\such\\rerics-dir-xyz"));
        assert!(errors.is_empty());
        assert!(eng.registered_commands().is_empty());
    }

    #[test]
    fn modal_apis_round_trip_through_host() {
        let host = Rc::new(MockHost {
            confirm_reply: true,
            prompt_reply: Some("typed".into()),
            select_reply: Some(2),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:modal",
            r#"
              rerics.log("c=" + rerics.confirm("ok?"));
              rerics.log("p=" + rerics.prompt("name?", "def"));
              rerics.log("s=" + rerics.select("pick", ["a", "b", "c"]));
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec!["c=true".to_string(), "p=typed".to_string(), "s=2".to_string()]
        );
    }

    #[test]
    fn object_model_exposes_pane_items_and_derived_views() {
        let host = Rc::new(MockHost {
            active_pane: PaneSnapshot {
                dir: "C:\\work".into(),
                is_archive: false,
                is_left: true,
                cursor: 2,
                items: vec![
                    item(0, "..", true, false),
                    item(1, "sub", true, false),
                    item(2, "a.txt", false, true),
                    item(3, "b.txt", false, true),
                ],
                ..Default::default()
            },
            opposite_pane: PaneSnapshot {
                dir: "C:\\other".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:object-model",
            r#"
              const p = rerics.activePane();
              rerics.log("dir=" + p.dir);
              rerics.log("count=" + p.items.length);
              rerics.log("sel=" + p.selectedItems.map(i => i.name).join(","));
              rerics.log("cursor=" + p.cursorItem.name);
              rerics.log("opp=" + rerics.oppositePane().dir);
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec![
                "dir=C:\\work".to_string(),
                "count=4".to_string(),
                "sel=a.txt,b.txt".to_string(),
                "cursor=a.txt".to_string(),
                "opp=C:\\other".to_string(),
            ]
        );
    }

    #[test]
    fn select_mask_replaces_selection_by_vb_like() {
        let host = Rc::new(MockHost {
            active_pane: PaneSnapshot {
                dir: "C:\\work".into(),
                is_left: true,
                items: vec![
                    item(0, "..", true, false),
                    item(1, "a.txt", false, true),   // 既存選択（マスク外＝解除されるはず）
                    item(2, "b.txt", false, false),
                    item(3, "c.dat", false, false),
                    item(4, "log9.txt", false, false),
                ],
                ..Default::default()
            },
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:select-mask",
            r#"
              // *.txt と log# の2マスク。a.txt は既選択だが *.txt に一致＝維持。
              rerics.log("hit=" + rerics.selectMask("*.txt, log#*"));
              rerics.log("none=" + rerics.selectMask("*.zip"));
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec!["hit=true".to_string(), "none=false".to_string()]
        );
        // 1回目：望む選択＝b.txt(2), log9.txt(4) を立て、a.txt(1) は既に true＝差分なし。c.dat(3) は対象外。
        // → 変更は (2,true),(4,true)。2回目（*.zip）は一致ゼロ＝全解除だが、MockHost のスナップショットは
        // 固定（前回の変更を反映しない）ため、元状態で選択済みの a.txt(1) のみが false への差分になる。
        assert_eq!(
            *host.applied.borrow(),
            vec![
                (true, vec![(2, true), (4, true)]),
                (true, vec![(1, false)]),
            ]
        );
    }

    #[test]
    fn change_opposite_directory_routes_kind_and_path() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:opposite-nav",
            r#"
              rerics.changeOppositeDirectory("D:\\dst");
              rerics.changeOppositeDirectoryToParent();
              rerics.changeOppositeDirectoryToRoot();
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.opposite_nav.borrow(),
            vec![
                ("path".to_string(), "D:\\dst".to_string()),
                ("parent".to_string(), "".to_string()),
                ("root".to_string(), "".to_string()),
            ]
        );
    }

    #[test]
    fn incremental_search_moves_cursor_and_centers() {
        let host = Rc::new(MockHost {
            active_pane: PaneSnapshot {
                dir: "C:\\work".into(),
                is_left: true,
                cursor: 0,
                items: vec![
                    item(0, "..", true, false),
                    item(1, "apple.txt", false, false),
                    item(2, "banana.txt", false, false),
                    item(3, "cherry.txt", false, false),
                ],
                ..Default::default()
            },
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:isearch",
            r#"
              rerics.log("hit=" + rerics.incrementalSearch("ban"));
              rerics.log("part=" + rerics.incrementalSearch("rry", false));
              rerics.log("miss=" + rerics.incrementalSearch("zzz"));
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec!["hit=true".to_string(), "part=true".to_string(), "miss=false".to_string()]
        );
        // ヒット 2 回ぶん、カーソル移動＋中央寄せが順に発火（cursor は常に 0＝モックは固定）。
        assert_eq!(
            *host.commands.borrow(),
            vec![
                ("setCursorIndex".to_string(), vec!["2".to_string()]),
                ("centerCursor".to_string(), vec![]),
                ("setCursorIndex".to_string(), vec!["3".to_string()]),
                ("centerCursor".to_string(), vec![]),
            ]
        );
    }

    #[test]
    fn state_getters_read_pane_and_dir() {
        let host = Rc::new(MockHost {
            dir: "D:\\proj\\src".into(),
            active_pane: PaneSnapshot {
                dir: "D:\\proj\\src".into(),
                is_left: false,
                sort_type: "lastWriteTime".into(),
                sort_reverse: true,
                path_mask: "*.rs".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:getters",
            r#"
              rerics.log("isLeft=" + rerics.isLeft());
              rerics.log("isRight=" + rerics.isRight());
              rerics.log("drive=" + rerics.currentDrive());
              rerics.log("sort=" + rerics.getSortType());
              rerics.log("rev=" + rerics.getSortReverse());
              rerics.log("mask=" + rerics.getPathMask());
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec![
                "isLeft=false".to_string(),
                "isRight=true".to_string(),
                "drive=D:".to_string(),
                "sort=lastWriteTime".to_string(),
                "rev=true".to_string(),
                "mask=*.rs".to_string(),
            ]
        );
    }

    #[test]
    fn selection_write_back_immediate_and_batched() {
        let host = Rc::new(MockHost {
            active_pane: PaneSnapshot {
                dir: "C:\\work".into(),
                is_left: true,
                cursor: 1,
                items: vec![
                    item(0, "..", true, false),
                    item(1, "a.txt", false, false),
                    item(2, "b.dat", false, false),
                    item(3, "c.txt", false, false),
                ],
                ..Default::default()
            },
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:write-back",
            r#"
              // 即時：カーソル行を選択 → その場で set_selected(index=1)。
              rerics.activePane().cursorItem.selected = true;
              // バッチ：.txt を全部選択 → apply で 1 回 apply_selection。
              rerics.activePane().apply((d) => {
                for (const it of d.items) if (it.ext === "txt") it.selected = true;
              });
            "#
            .to_string(),
        )
        .unwrap();
        // 即時は (左=true, index=1, true) が 1 件。
        assert_eq!(*host.set_selected.borrow(), vec![(true, 1, true)]);
        // バッチは index 1 と 3（a.txt/c.txt）がまとまって 1 回。
        assert_eq!(
            *host.applied.borrow(),
            vec![(true, vec![(1, true), (3, true)])]
        );
    }

    #[test]
    fn command_invokes_host_and_throws_on_error() {
        let host = Rc::new(MockHost {
            failing_command: Some("Boom".into()),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:command",
            r#"
              rerics.command("cursorDown");
              rerics.command("SortBy", "name", "asc");
              try { rerics.command("Boom"); rerics.log("no-throw"); }
              catch (e) { rerics.log("caught:" + e.message); }
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.commands.borrow(),
            vec![
                ("cursorDown".to_string(), vec![]),
                ("SortBy".to_string(), vec!["name".to_string(), "asc".to_string()]),
            ]
        );
        // 失敗コマンドは JS の例外になり、catch でメッセージを拾える。
        assert_eq!(*host.logs.borrow(), vec!["caught:boom: Boom".to_string()]);
    }

    #[test]
    fn builtin_commands_exposed_as_r_methods() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:r-builtin",
            r#"r.cursorDown(); r.setCursorPosition("a.txt");"#.to_string(),
        )
        .unwrap();
        // 組込コマンドが r.<token>() で呼べ、引数も command ブリッジへ届く。
        assert_eq!(
            *host.commands.borrow(),
            vec![
                ("cursorDown".to_string(), vec![]),
                ("setCursorPosition".to_string(), vec!["a.txt".to_string()]),
            ]
        );
    }

    #[test]
    fn event_handlers_fire_with_payload_and_can_call_host() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:on",
            r#"
              rerics.on("changeDirectory", (dir) => rerics.log("cd1:" + dir));
              rerics.on("changeDirectory", (dir) => rerics.navigate(dir + "/x"));
              rerics.on("executeCommand", (name) => rerics.log("cmd:" + name));
            "#
            .to_string(),
        )
        .unwrap();
        // 未登録イベントは無音。
        eng.fire_event("noSuchEvent", "").unwrap();
        // changeDirectory は両ハンドラが順に走る（ログ＋ホスト呼び出し）。
        eng.fire_event("changeDirectory", "C:\\d").unwrap();
        eng.fire_event("executeCommand", "cursorDown").unwrap();

        assert_eq!(
            *host.logs.borrow(),
            vec!["cd1:C:\\d".to_string(), "cmd:cursorDown".to_string()]
        );
        assert_eq!(*host.navigated.borrow(), vec!["C:\\d/x".to_string()]);
    }

    #[test]
    fn async_copy_and_move_await_host_completion() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:async-op",
            r#"(async () => {
                 await rerics.copy();
                 await rerics.move();
                 await rerics.delete();
                 rerics.log("ops done");
               })();"#
                .to_string(),
        )
        .unwrap();
        assert_eq!(*host.operations.borrow(), vec!["copy", "move", "delete"]);
        assert_eq!(*host.logs.borrow(), vec!["ops done".to_string()]);
    }

    #[test]
    fn async_op_job_cancel_routes_token_to_host() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:cancel",
            r#"(async () => {
                 const job = rerics.copy();
                 job.cancel();
                 await job;
                 rerics.log("after");
               })();"#
                .to_string(),
        )
        .unwrap();
        assert_eq!(*host.operations.borrow(), vec!["copy"]);
        // copy のトークンは 1（operations 件数で代用）。cancel がそのトークンで届く。
        assert_eq!(*host.cancelled.borrow(), vec![1u64]);
        assert_eq!(*host.logs.borrow(), vec!["after".to_string()]);
    }

    #[test]
    fn async_op_streams_progress_then_completes() {
        // 完了前に流れた進捗が onProgress に順番どおり届き、最後に await が解ける。
        let host = Rc::new(MockHost {
            op_progress: vec!["1/2".into(), "2/2".into()],
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:progress",
            r#"(async () => {
                 const seen = [];
                 await rerics.copy({ onProgress: (p) => seen.push(p.text) });
                 rerics.log("progress:" + seen.join(","));
               })();"#
                .to_string(),
        )
        .unwrap();
        assert_eq!(*host.operations.borrow(), vec!["copy"]);
        assert_eq!(*host.logs.borrow(), vec!["progress:1/2,2/2".to_string()]);
    }

    #[test]
    fn async_op_failure_rejects_the_await() {
        // 失敗（中止）で完了したら await は例外になり、try/catch で捕まえられる。
        let host = Rc::new(MockHost {
            op_error: Some("中止しました".into()),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:fail",
            r#"(async () => {
                 try {
                   await rerics.delete();
                   rerics.log("no-throw");
                 } catch (e) {
                   rerics.log("caught:" + e.message);
                 }
               })();"#
                .to_string(),
        )
        .unwrap();
        assert_eq!(*host.logs.borrow(), vec!["caught:中止しました".to_string()]);
    }

    #[test]
    fn helpers_are_shared_across_files_by_load_order() {
        // 先に読むファイルでトップレベル定義した普通の関数・定数を、後のファイルの
        // registerCommand コールバックから（invoke 時に）参照できるか＝ファイル間共有の実証。
        let dir = std::env::temp_dir().join(format!("rerics-share-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("00-lib.ts"),
            r#"function libDouble(n: number): number { return n * 2; }
               const LIB_TAG: string = "lib";"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("10-use.ts"),
            r#"rerics.registerCommand("useShared", () => {
                 rerics.log(LIB_TAG + ":" + libDouble(21));
               });"#,
        )
        .unwrap();

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        let errors = load_dir(&mut eng, &dir);
        assert!(errors.is_empty(), "errors: {errors:?}");
        eng.invoke_command("useShared", &[]).unwrap();
        assert_eq!(*host.logs.borrow(), vec!["lib:42".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fs_layer_round_trips_through_std_fs() {
        let dir = std::env::temp_dir().join(format!("rerics-fs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // バックスラッシュ回避のためスラッシュ表記で渡す（Windows でも std::fs は受ける）。
        let base = dir.display().to_string().replace('\\', "/");

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        let code = format!(
            r#"
              const b = "{base}";
              // mkdir（再帰）→ writeText → readText 往復。
              r.fs.mkdir(b + "/nested/deep");
              r.fs.writeText(b + "/nested/deep/a.txt", "こんにちは");
              rerics.log("read=" + r.fs.readText(b + "/nested/deep/a.txt"));
              // exists / stat。
              rerics.log("exists=" + r.fs.exists(b + "/nested/deep/a.txt"));
              rerics.log("missing=" + r.fs.exists(b + "/nope.txt"));
              const st = r.fs.stat(b + "/nested/deep/a.txt");
              rerics.log("stat=" + st.isFile + "," + (st.size > 0) + "," + (st.mtime > 0));
              rerics.log("statNull=" + (r.fs.stat(b + "/nope.txt") === null));
              // copyFile → rename → remove。
              r.fs.copyFile(b + "/nested/deep/a.txt", b + "/copy.txt");
              r.fs.rename(b + "/copy.txt", b + "/renamed.txt");
              rerics.log("renamed=" + (!r.fs.exists(b + "/copy.txt") && r.fs.exists(b + "/renamed.txt")));
              r.fs.remove(b + "/renamed.txt");
              rerics.log("removed=" + !r.fs.exists(b + "/renamed.txt"));
            "#
        );
        eng.run_to_completion("test:fs", code).unwrap();

        assert_eq!(
            *host.logs.borrow(),
            vec![
                "read=こんにちは".to_string(),
                "exists=true".to_string(),
                "missing=false".to_string(),
                "stat=true,true,true".to_string(),
                "statNull=true".to_string(),
                "renamed=true".to_string(),
                "removed=true".to_string(),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// テスト用の無圧縮(stored) zip を書き出す（外部書庫を用意せず実 backend を通すため）。
    fn write_stored_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = 0xFFFF_FFFFu32;
            for &b in data {
                crc ^= b as u32;
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
                }
            }
            !crc
        }
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let nb = name.as_bytes();
            let crc = crc32(data);
            let off = out.len() as u32;
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version/flags/method/time/date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(nb);
            out.extend_from_slice(data);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0u8; 12]); // extra/comment len, disk, attrs
            central.extend_from_slice(&off.to_le_bytes());
            central.extend_from_slice(nb);
        }
        let cd_off = out.len() as u32;
        let cd_len = central.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]); // disk numbers
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_len.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        std::fs::write(path, &out).unwrap();
    }

    #[test]
    fn unpack_extracts_archive_to_destination() {
        let dir = std::env::temp_dir().join(format!("rerics-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("arc.zip");
        write_stored_zip(&zip, &[("a.txt", b"AAA"), ("sub/c.txt", b"CCC")]);
        let zip_p = zip.display().to_string().replace('\\', "/");
        let dst_p = dir.join("out").display().to_string().replace('\\', "/");

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        let code = format!(
            r#"(async () => {{
                 const n = await rerics.unpack("{zip_p}", "{dst_p}");
                 rerics.log("n=" + n);
                 rerics.log("a=" + rerics.fs.readText("{dst_p}/a.txt"));
                 rerics.log("c=" + rerics.fs.readText("{dst_p}/sub/c.txt"));
               }})();"#
        );
        eng.run_to_completion("test:unpack", code).unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec!["n=2".to_string(), "a=AAA".to_string(), "c=CCC".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unpack_missing_archive_throws() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:unpack-throw",
            r#"(async () => {
                 try { await rerics.unpack("C:\\no\\such-xyz.zip", "C:\\tmp\\out"); rerics.log("no-throw"); }
                 catch (e) { rerics.log("caught"); }
               })();"#
            .to_string(),
        )
        .unwrap();
        assert_eq!(*host.logs.borrow(), vec!["caught".to_string()]);
    }

    #[test]
    fn modifiers_expose_pressed_keys() {
        let host = Rc::new(MockHost {
            modifiers: Modifiers { shift: true, ctrl: false, alt: true },
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:modifiers",
            r#"
              const m = rerics.modifiers();
              rerics.log("s=" + m.shift + ",c=" + m.ctrl + ",a=" + m.alt);
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(*host.logs.borrow(), vec!["s=true,c=false,a=true".to_string()]);
    }

    #[test]
    fn fs_read_text_missing_file_throws() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:fs-throw",
            r#"
              try { r.fs.readText("C:\\no\\such\\rerics-fs-xyz.txt"); rerics.log("no-throw"); }
              catch (e) { rerics.log("caught"); }
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(*host.logs.borrow(), vec!["caught".to_string()]);
    }
}
