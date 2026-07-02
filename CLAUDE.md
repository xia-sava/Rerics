# CLAUDE.md

Rerics — Windows 向け2画面ファイラー（gary 氏の Records を Rust で現代化した非公式クローン）。
概要・由来は README.md、設計の詳細は ARCHITECTURE.md を参照。

## クレート構成
- `rerics-core` … UI/OS 非依存のロジック（一覧・ソート・比較・検索・`Command` 定義・機能欄の式パーサ `Call`）。単体テスト可。
- `rerics` … winsafe GUI・OS 連携・組込スクリプトエンジン（deno_core/埋め込み V8）。core を土台に画面・入力・外部プロセス・スクリプト API を実装。
- `vendor/winsafe` … UAF 修正版へ `[patch.crates-io]` で差し替え。

## ビルド・テスト
- ビルド `./tools/dev.sh build`（git-bash ラッパ→MSVC 環境で cargo）。`run`/`test` も同様。**Windows + MSVC 専用**。
- core `cargo test -p rerics-core --lib` ／ bin `cargo test -p rerics --bin rerics`
- e2e（headless）`cargo test -p rerics --features debug-server --test debug_server`
- ⚠ **feature 有無は同じ exe を奪い合う**。debug-server で検証したら plain `./tools/dev.sh build` に戻す。
- clippy は自分の変更で新規警告ゼロを保つ。

## 設計の要点（詳細は ARCHITECTURE.md）
- **コマンドの二層分離**：UI を伴うコマンドは表層（`xxxDialog`＝値集めのみ）と実処理（引数駆動・UI非依存）に分ける。実処理は script/引数キーバインド/debug-server から呼べてテスト可能に。
- **実処理の正本**：純ロジックは script API `r.xxx`（`script.rs` bootstrap）が唯一の正本。GUI/OS 依存は `rerics` の引数付き関数。
- **機能欄は式**：キー定義・メニューは式。`call.rs` の `Call` が単一組込呼び出しの同期 fast-path（`Builtin`）とエンジン丸投げ（`Script`）に振り分ける。
- **スクリプトは専用スレッド**（deno_core/V8）。GUI 状態へは `HostCall`/`HostResp` で marshaling。
- **結果一覧**（検索・比較）は `FileListState.find_result` フラグ＋`FileItem.source/info` で表現。
- **観測可能性＝テスト前提**：GUI は debug-server（`--features debug-server`）から `/state`・`/command/*`・`/modal/*` で観測・駆動できる形で実装。e2e は `crates/rerics/tests/debug_server.rs`。**窓を出さず headless で検証**。

## コマンド/スクリプト API を足すとき（詳細は ARCHITECTURE.md）
- 組込コマンド：`rerics-core/src/input.rs`（`Command`＋`ALL`）／`main.rs` の `exec_resolved`＋`debug_command_class`／`key_editor.rs` の `command_genre`。補完/ヘルプ/型定義は `ALL` から自動生成。
- script API `r.xxx`：op 実装＋bootstrap＋`dts.rs` の `HOST_API_MEMBERS` と `scripting/rerics.d.ts`。

## 規約
- 周囲の既存コードのスタイル・命名・イディオムに合わせる。early return・単一責任・意図を表す命名。
- ファイル末尾は改行で終える。コミット件名は命令形・1コミット1論理変更。

## スクリプティング
TS/JS で動作をカスタマイズ（`%APPDATA%\Rerics\scripts`）。API は `rerics.*`、型は `scripting/rerics.d.ts`、詳細は `scripting/README.md`。
