---
description: rerics の GUI 変更は debug-server から観測・駆動でき headless 検証できる形にする
paths:
  - crates/rerics/src/**
---

# GUI は観測可能・headless 検証可能に

`rerics`（GUI）の変更は、**窓を出さず** debug-server（`--features debug-server`）から観測・駆動できる形で実装する。

- 状態は `/state`（`debug_ctl.rs` の `debug_state_value`/`debug_pane_json`）に出す。新しい観測点はここへ。
- 操作は `/command/<token>`、モーダルは `/modal/*` で駆動できるようにする。**モーダルはレジストリに登録**（`modal_window`/`arm_modal`）＝ headless から読める・操作できる。
- **モーダルを開く新コマンドは `main.rs` の `debug_command_class` に種別（`MaybeModal`/`ModalWrite`）を追加**（忘れると headless テストがモーダルでブロックする）。
- 検証は e2e（`crates/rerics/tests/debug_server.rs`）で。手順は skill `rerics-e2e-verify` を参照。
- ⚠ **feature 有無は同じ exe を奪い合う**。debug-server で検証したら plain `./tools/dev.sh build` に戻す。
- 実処理は「表層 UI コマンド（値集め）」と「引数駆動の実処理」に二層分離し、実処理を debug-server/引数キーバインド/スクリプトから叩けるようにする。
