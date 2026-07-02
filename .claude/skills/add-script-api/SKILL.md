---
name: add-script-api
description: Rerics に新しいスクリプト API メンバー（r.xxx）を追加する全手順。op 実装、HostApi/HostCall/HostResp の marshaling、bootstrap への公開、dts.rs の HOST_API_MEMBERS と rerics.d.ts の型追加まで。新しい r.xxx を足すときに使う。
---

# スクリプト API メンバー（r.xxx）を追加する

`r.xxx` を1つ足すときの手順。設計背景は `ARCHITECTURE.md`。

## 触る場所
1. **op を実装**（`crates/rerics/src/script.rs`）
   - 値返し・失敗時例外は `op_command` と同じ `Result<serde_json::Value, JsErrorBox>` 機構を使う。
   - **GUI 状態に触るなら**：`HostApi` trait メソッド＋`HostCall`/`HostResp` バリアント＋`GuiHost` の marshal＋`drain_script_requests` の dispatch＋`MockHost` の実装を揃える。
   - 純粋な計算・システムクエリなら Host を介さない op でよい。
2. **公開**：`extension!` の ops 一覧に登録し、bootstrap の `globalThis.rerics`（別名 `r`）にメソッドとして生やす。
3. **型定義**：`crates/rerics-core/src/dts.rs` の `HOST_API_MEMBERS` と `scripting/rerics.d.ts` の両方に追加する。

## 方針
- 純ロジックは core（`rerics-core`）へ寄せ、op は薄く呼ぶだけにできると良い（core が唯一の正本）。
- GUI コマンドの実処理を `r.xxx` として実装した場合、表層 UI コマンドは `EngineCmd::Eval` でそれを呼ぶ（marshaling を一往復するので結果反映は非同期）。

## 検証
- core 側ロジックは `cargo test -p rerics-core --lib`。
- API 経由は headless の script eval／e2e（`crates/rerics/tests/debug_server.rs`）。手順は skill `rerics-e2e-verify`。
