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

## 観測可能性とテスト

GUI は debug-server（`--debug-server` / `--headless`）から状態を観測し、操作を駆動できる形で
実装する。モーダルはレジストリに登録し、window を出さずに `/state`・`/command/*`・`/modal/*`
などで検証できるようにする。前述の「実処理を引数で駆動できる形に分ける」設計と合わせて、機能は
ヘッドレスで end-to-end にテストする（`crates/rerics/tests/debug_server.rs`）。
