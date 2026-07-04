---
description: rerics-core を UI/OS 非依存・単体テスト可能に保つ
paths:
  - crates/rerics-core/**
---

# rerics-core は UI/OS 非依存に保つ

`rerics-core` は GUI(winsafe)・Win32・OS 依存・画面状態を **持ち込まない**。純粋なロジック
（一覧・ソート・比較・検索・パス/VFS・`Command` 定義・機能欄の式 `Call`・アーカイブ）だけを置く。

- 依存を足すときは UI/OS 非依存か確認する。winsafe や Win32 API、`rerics` クレートへの依存は不可。
  - 例外: std で取れないファイルメタデータの取得に限り、`cfg(windows)` の極小 FFI
    （kernel32 直宣言・依存クレート追加なし）は置いてよい（例: `file_list.rs` の reparse tag）。
    UI・ウィンドウ・プロセス制御の Win32 は引き続き不可。
- 純ロジックの正本はここ。GUI コマンドの「実処理」も、UI/OS に依存しないものは core（または script `r.xxx`）へ寄せ、`rerics` 側は薄く呼ぶだけにする。
- 逐次処理の中断・進捗は `Sink`（`emit`/`cancelled`/`tick`）等で GUI 非依存に表す（検索・比較・サイズ計算に倣う）。
- 変更は `cargo test -p rerics-core --lib` で単体テストする。新ロジックにはテストを添える。
