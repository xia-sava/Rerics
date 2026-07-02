---
name: add-command
description: Rerics に新しい組込コマンド（Command）を追加する全手順。input.rs の Command/ALL 追加、main.rs の exec_resolved 分岐と debug_command_class、key_editor.rs の command_genre、表層UIと実処理の二層分離、e2e 検証まで。新しい組込コマンドを足すときに使う。
---

# 組込コマンドを追加する

新しい `Command` を1つ足すときの手順。設計背景は `ARCHITECTURE.md`。

## 触る場所（すべて必須）
1. `crates/rerics-core/src/input.rs`
   - `Command` enum にバリアントを追加。
   - `ALL` テーブルに `(Variant, "token", "表示名", "説明")` を追加。
   - 既定キーを付けるなら `default_keymap` 相当に `m.bind(KeyChord::new(...), Variant)`。
2. `crates/rerics/src/main.rs`
   - `exec_resolved` に分岐を追加し、実処理を呼ぶ。
   - **モーダルを開くコマンドなら `debug_command_class` に種別（`MaybeModal`/`ModalWrite`）を追加**（忘れると headless がブロック）。
3. `crates/rerics/src/key_editor.rs`
   - `command_genre`（網羅 match）にジャンルを追加（漏れるとビルドエラー）。

補完・ヘルプ・メニュー・型定義（`.d.ts`）は `ALL` から自動生成なので追従不要。

## 実装方針（二層分離）
- UI（ダイアログ/入力）を伴うなら、表層コマンド `xxxDialog`（値を集めるだけ）と、引数を受ける実処理に分ける。
- 実処理の正本は性質で決める：
  - 純ロジック（UI/OS 非依存）→ script `r.xxx`（`script.rs` bootstrap）を正本にし、表層は `EngineCmd::Eval` で呼ぶ。
  - GUI・OS 依存 → `rerics` の引数付き関数として実装し、表層から直接呼ぶ（同期・失敗時ダイアログ可）。

## 検証
- ビルド `./tools/dev.sh build`、clippy 新規警告ゼロ。
- headless で `/command/<token>` を叩いて e2e（`crates/rerics/tests/debug_server.rs`）。手順は skill `rerics-e2e-verify`。
