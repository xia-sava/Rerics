# アーキテクチャ

## クレート構成

- **`rerics-core`** … UI 非依存のロジック。ファイル一覧・ソート・コマンド定義（`Command`）・
  機能欄の式パーサ（`Call`）など。GUI にも OS にも依存せず、単体でテストできる。
- **`rerics`** … GUI（winsafe）と OS 連携、組込スクリプトエンジン（deno_core）。`rerics-core`
  を土台に、画面・入力・外部プロセス・スクリプト API を実装する。

## スクリプトエンジンとスレッドモデル

Rerics は組込スクリプトエンジン（deno_core・埋め込み V8）を専用スレッドで動かす。スクリプト
からホスト（ファイルシステム・画面状態・外部プロセス）へ触れるのは、`crates/rerics/src/script.rs`
で定義する op（`#[op2]`）経由のみ。bootstrap スクリプトが起動時に `globalThis.rerics`
（別名 `r`）を組み立て、各 op をメソッドとして公開する。

UI はメインスレッド（winsafe のメッセージループ）で動く。エンジンスレッドが画面状態を要する
操作（カーソル移動・選択・ペイン情報の取得など）を行うときは、`HostCall` / `HostResp` で
メインスレッドへ要求を marshaling し、メインスレッド側の dispatch が処理して返す。逆に UI 側
からスクリプトを起動するときは `EngineCmd::Eval` でソースをエンジンスレッドへ送る
（投げっぱなし・非同期）。

この境界があるため、表層 UI コマンドが純ロジックの `r.xxx` を呼ぶときは `EngineCmd::Eval`
経由になり、選択などの結果は marshaling を一往復してから画面へ反映される（即時ではない）。

### スクリプト実行のタスク化と停止

ユーザースクリプトの実行（コマンド呼び出し・eval・イベントハンドラ）は、停止できる**タスク**
として扱う。エンジンスレッドは各実行の前後に `WorkerEvent::ScriptBegin`／`ScriptEnd` を
`task_tx` で送り（送った直後に `SCRIPT_WAKE` を post して取り込みタイマ未起動でも拾わせる）、
UI 側はタスクマネージャに種別 `TaskKind::Script` の行として並べる。スクリプトは直列実行
（同時に走るのは1つ）なので、UI は「現在のスクリプトタスク」を1つだけ覚える。

中止は V8 の `IsolateHandle::terminate_execution`（起動時に `ScriptEngineReady` で UI へ渡す）で
**強制終了**する＝協調キャンセルでは止められない暴走 JS も止められる。`pump_tasks` が中止された
スクリプトタスクを見つけたら isolate を terminate し（1回だけ）、エンジンが巻き戻って `ScriptEnd`
を送ると登録解除される。次の実行に terminate フラグが残らないよう、各実行の前に
`Engine::clear_terminate` を呼ぶ。**中断／再開は V8 の制約でできない**ため、スクリプトタスクでは
タスクマネージャの中断／再開を無反応にする（状態は「実行中」のまま・中止のみ効く）。

## コマンドの構造：表層 UI と実処理の分離

UI（ダイアログ・入力ボックス・リスト選択）を伴うコマンドは、次の二層に必ず分ける。

1. **表層 UI コマンド**（`xxxDialog`）… ユーザーから値を集めるだけ。実処理ロジックを持たない。
2. **実処理**（引数を受ける関数）… 集めた値を引数に取り、実際の処理を行う。UI に依存しない。

表層 UI コマンドは、集めた値を引数として実処理を呼ぶだけにする。これにより実処理は常に
引数で駆動でき、スクリプト・引数つきキーバインド・debug-server から呼べてテストできる。
UI を差し替えても実処理は変わらず、同じ処理が UI 側とロジック側で二重に実装されて挙動が
食い違うことも防げる。

### 実処理の置き場

実処理の置き場は、その性質で決める。

- **純ロジック**（ファイル操作・選択・比較・並べ替えなど、UI/OS に依存しないもの）…
  スクリプト API `r.xxx` として実装する（`crates/rerics/src/script.rs` の bootstrap）。
  これが唯一の正本で、スクリプトからも内蔵コマンドからも同じ実装を使う。
- **GUI/OS 依存**（シェルのプロパティシート・外部エディタ起動・ビューアなど、スクリプトで
  書けないもの）… `rerics` クレート内の引数つき関数として実装する。表層 UI コマンドは
  この関数を直接呼ぶ。

表層 UI コマンドから実処理を呼ぶ手段は、正本がどこにあるかで決まる。

- 正本が bootstrap の JS（例：`r.compare`）なら `EngineCmd::Eval` で呼ぶ。エンジンスレッドへ
  渡るので非同期で、結果は marshaling を一往復してから反映される。
- 正本が Rust 側の関数（例：`script_create_directory`。これは `r.makeDirectory` の実体でもある）
  なら、表層 UI からその関数を直接呼ぶ。同期で結果を扱え、失敗時にダイアログも出せる。

### 命名規約

- **表層 UI あり** … `xxxDialog`（`Command` の token。キーに裸の token でバインドできる）。
- **UI なしの実処理** … `xxx`（スクリプト API メンバー。programmatic 用に予約する）。

### 例：`CompareDialog`

`CompareDialog` は `dialog::list_box` で比較条件を 1 つ選ばせ、選んだ条件 token を引数に
`r.compare(token)` を `EngineCmd::Eval` で呼ぶ。比較ロジックそのものは `r.compare`
（純 JS・両ペインの一覧を突き合わせて選択する）が唯一の正本で、UI コマンドは値を集めて
渡すだけ。実装は `crates/rerics/src/search.rs` の `compare_dialog`。

## コマンドの実行経路（機能欄は式）

キー定義・メニュー項目など「機能を指定する場所」は式（コード）で書ける。
`crates/rerics-core/src/call.rs` の `Call` が式を振り分ける。単一の組込コマンド呼び出しに
簡約できる式は同期実行の fast-path（`Call::Builtin`）に、それ以外（ネスト呼び出し・
スクリプト関数・制御構文など）はエンジンへ丸投げ（`Call::Script`）になる。

- 組込コマンドは token で直接書ける（例：`cursorDown`、`compareDialog`）。
- スクリプト API メンバーや登録コマンドは `r.xxx()` の式で書く（裸の token では引けない）。

## 組込コマンドを追加するとき触る箇所

`Command` を 1 つ増やすときのチェックリスト。

- `crates/rerics-core/src/input.rs` … `Command` enum と `ALL` テーブル（token・表示名・説明）。
- `crates/rerics/src/main.rs` … `exec_resolved` の分岐。モーダルを開くコマンドは
  `debug_command_class` に種別（`MaybeModal` / `ModalWrite`）を追加する（忘れると
  debug-server から叩いたときモーダルでブロックする）。
- `crates/rerics/src/key_editor.rs` … `command_genre`（網羅 match なので追加必須）。
- 補完・ヘルプ・メニュー・型定義（`.d.ts`）は `ALL` テーブルから自動生成されるため、
  個別の追従は不要。

## スクリプト API メンバーを追加するとき触る箇所

`r.xxx` を 1 つ増やすときのチェックリスト。

- 値返し・失敗時の例外は `op_command` と同じ `Result<serde_json::Value, JsErrorBox>` 機構を
  使う。GUI の状態に触るなら `HostApi` trait メソッド＋`HostCall`/`HostResp` バリアント＋
  `GuiHost` の marshal＋`drain_script_requests` の dispatch＋`MockHost` の実装を揃える。
  純粋な計算やシステムクエリは Host を介さない op にできる。
- `extension!` の ops 一覧に登録し、bootstrap の `globalThis.rerics` に生やす。
- `crates/rerics-core/src/dts.rs` の `HOST_API_MEMBERS` と `scripting/rerics.d.ts` の両方に
  追加する。

## 結果一覧（検索・比較のペイン）

ペインは通常「1つの実ディレクトリの鏡」だが、ディレクトリ比較・ファイル検索は**複数ディレクトリ
出身の項目をフラットに並べる結果一覧**を表示する。これは新しい場所種別（`Location`）ではなく、
`FileListState` の**モードフラグ**で表す：

- `FileListState.find_result: bool` … 結果一覧モードか。`true` の間は情報列（`ColumnKind::Information`）
  を出し、各項目は出自情報（`FileItem.source: Option<Location>`＝出自ディレクトリ／`info`＝相対サブパス
  や "追加"/"削除" などの説明）を持つ。項目は通常一覧と同じ `items` に入るので、描画・選択・ソート・
  `/state` 観測がそのまま効く。
- 流し込み（ライブ追加）… 純ロジック（`rerics_core::directory_compare` / `find_file`）は、走査結果を
  ため込まず `rerics_core::Sink`（`emit`＝項目を1件ずつ渡す／`cancelled`＝各境界で打ち切り判定。
  中断中はこの中でブロックして待つ）経由で逐次返す。GUI 側はこれを**タスク**として回し（コピー等と同じ
  `TaskControl`＋`register_task`）、ワーカーが `WorkerEvent::FindBegin`／`FindItem`／`FindDone` を送る。
  取り込みは `pump_tasks` が担い、`FindBegin` で結果モードへ切替（`begin_find_result`）・`FindItem` で
  追記（`push_find_result`）・`FindDone` で件数ログと列幅調整を行う。**項目ごとの再描画は避け、1取り込み
  ぶんをまとめて1回だけ再描画する**。ペインの現在地（`Pane.loc`）は基準ディレクトリのまま変えない。
- タスク制御 … 検索・比較はタスクマネージャに並び、中止／中断／再開できる（`TaskControl`）。中止はそれまでに
  出た結果を残したまま打ち切る。同じペインで再検索したときは、`MainWindow.find_task`（`[左, 右]` の現役
  タスク id）で取り違えを防ぎ、旧タスクを止めてから新タスクの項目だけを追記する。
- 抜ける … 結果項目を開く（Enter）と出自（`source` → 無ければ現在地）へ navigate してその名前へカーソルを
  合わせ、通常のディレクトリへ戻る。先頭の ".." も基準ディレクトリの再読込で抜ける。**いずれの通常移動も
  `apply_loaded_items` に合流し、そこで `find_result` を解除して設定列（`config.columns`）へ戻す**ので、
  解除処理は一箇所に集約される。

比較・検索の**条件ごとのロジックは core のユニットテスト**（`compare.rs` / `find.rs`）で、**結果ペインの
挙動**（情報列・出自ジャンプ・モード解除）は e2e（`directory_compare_*` / `find_file_*`）で担保する。
ダイアログのラジオ/チェック/個別入力欄は debug-server から駆動できないため、条件別の検証は引数版コマンド
（`directoryCompare` / `findFile("mask")`）と core テストに寄せる。

## 観測可能性とテスト

GUI は debug-server（`--debug-server` / `--headless`）から状態を観測し、操作を駆動できる形で
実装する。モーダルはレジストリに登録し、window を出さずに `/state`・`/command/*`・`/modal/*`
などで検証できるようにする。前述の「実処理を引数で駆動できる形に分ける」設計と合わせて、機能は
ヘッドレスで end-to-end にテストする（`crates/rerics/tests/debug_server.rs`）。
