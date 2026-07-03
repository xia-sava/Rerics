---
name: rerics-deploy
description: Rerics の release ビルドをデプロイ先（実機の常用インストール先）へ差し替えて起動し直す手順。稼働中インスタンスを graceful 終了 → release ビルド → exe をコピー → 起動。問題発見→報告→改良→devビルド確認→commit&push の後、実機へ本番反映するときに使う。窓が開く起動を伴う。
---

# release をデプロイして実機を差し替える

開発中の改良を実機の常用 Rerics へ反映する最後の一手。`./tools/dev.sh deploy` 一発で完結する。

## 実行

```bash
./tools/dev.sh deploy
```

これで以下が順に走る（`tools/deploy.sh`）:
1. デプロイ先で稼働中の `rerics.exe` **だけ**を graceful close（WM_CLOSE）し、終了を待つ（state.toml を保存させるため。残ったら Kill にフォールバック）。dev/e2e の別インスタンスは実行パスが違うので巻き込まない。
2. `./tools/dev.sh build --release`（MSVC 環境で plain GUI ビルド＝debug-server feature なし・コンソール窓なし）。
3. `target/release/rerics.exe` をデプロイ先へコピー（**exe のみ**。pdfium.dll / README.md は据え置き）。
4. デプロイ先の `rerics.exe` を起動（**窓が開く**）。

## 前提・注意

- **デプロイ先パスはリポジトリに焼かない**。`RERICS_DEPLOY_DIR` 環境変数、無ければ gitignored な `.claude/deploy.local.sh`（`export RERICS_DEPLOY_DIR="..."`）から解決する。どちらも無ければエラーで停止する。
- **窓を出す起動を伴う**（手順4）。これは意図した本番起動だが、[[prefer-headless-launch]] の方針どおり、実行前にユーザへ一声かけてから走らせる。
- 想定される位置づけは「問題発見 → セッション冒頭で報告 → 相談しながら改良 → dev ビルドで実機確認 → commit & push」**の後**の本番反映。commit / push はこの手順に含めない（別途）。
- pdfium.dll / README.md を更新したいときはこの手順の対象外なので手動で反映する。
