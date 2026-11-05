---
name: release
description: Rerics のバージョンを上げて配布する。Cargo のバージョンを上げてコミットし、バージョンタグを打って GitHub Actions に配布物を作らせる手順、打つ前の確認、打ち直しが効かないことなど。「バージョンを上げて」「リリースして」「配布して」と言われたときに使う。手元の release ビルドを実機へ差し替える rerics-deploy とは別物。
---

# バージョンを上げる

`vMAJOR.MINOR.PATCH` の形のタグを push すると、GitHub Actions（`.github/workflows/release.yml`）が
配布物を組んで公開する。main への push では何も実行されない（push ごとの検証のワークフローは無い）。

バージョンは **Cargo のバージョン**（リポジトリ直下の `Cargo.toml` の `[workspace.package]` の `version`）で、
各 crate はこれを継ぐ。**人が決める。** タグはこのバージョンの先頭に `v` を付けたもので、食い違えば
ワークフローが止まる。アプリの表記・exe のバージョン情報・自動更新の比較はすべてこのバージョンで、
新しいかどうかは `MAJOR.MINOR.PATCH` を数として比べる（`crates/rerics/src/version.rs`）。

## 打つ前の確認

1. **作業ツリーがクリーンで、検証が通っていること。** CI は core と bin の単体テストしか回さないので、
   e2e は手元で通しておく。

   ```
   ./tools/dev.sh test -p rerics-core --lib
   ./tools/dev.sh test -p rerics --bin rerics
   ./tools/dev.sh test -p rerics --features debug-server --test debug_server
   ./tools/dev.sh build
   ```

   e2e は feature 付きで exe を作り直すので、最後に plain のビルドへ戻す。clippy は自分の変更で
   新規警告ゼロを保つ（`./tools/dev.sh clippy -p rerics -p rerics-core`）。

2. **配布中のバージョンを確認して、次のバージョンを決める。**

   ```
   gh release view latest --json name,tagName
   gh release download latest -p manifest.json -O -
   ```

## バージョンを上げてコミットする

1. `Cargo.toml` の `[workspace.package]` の `version` を新しいバージョンにする（例 `1.1.1`）。
2. `./tools/dev.sh build` で `Cargo.lock` の workspace の crate のバージョンを書き換えさせる。ワークフローは
   `--locked` でビルドするので、`Cargo.lock` を上げ忘れると止まる。
3. `Cargo.toml` と `Cargo.lock` だけをコミットする（例「バージョンを 1.1.1 にする」）。

## push してタグを打つ

```
git push origin main
git tag v1.1.1
git push origin v1.1.1
```

**main の push とタグの push は、どちらも外部へ公開する操作。それぞれ実行の直前に必ずユーザーへ
確認を取る。** 依頼済みであっても、押す瞬間に改めて一声かける。main の push の確認はタグの push に
及ばない。

## 確認する

```
gh run list --workflow Release --limit 3
```

`Release` ワークフローが実行される。成功すると 2 つのリリースができている。

- **`v1.1.1`** … バージョンごとの記録。配布物の実物（`rerics-1.1.1-windows.zip`）が残るので、後から
  どのバージョンで何を配ったか辿れる
- **`latest`** … 自動更新の取得先（prerelease）。固定の名前の配布物（`rerics-windows.zip`）と
  `manifest.json` を置き換える

```
gh release view v1.1.1 --json name,assets
gh release download latest -p manifest.json -O -
```

`manifest.json` の `version` が新しいバージョンで、`sha256` が `latest` の `rerics-windows.zip` と合っていることを
確かめる。アプリの「その他」→「自動更新」は `manifest.json` を読み、`asset` の zip をダウンロードして
`sha256` で照合してから差し替える。

## 気をつけること

- **タグの打ち直しはしない。** 配布したものを直すときは patch を上げた新しいバージョンを出す。同じ
  バージョンの配布物を差し替えると、既に入れた人には新しいバージョンとして見えない。
- **バージョンを下げる手段は無い。** 新しいバージョンかどうかをバージョンの数で比べるので、下げた
  バージョンは更新として届かない。問題が出たら戻すのではなく、直したバージョンを出す。
- **接尾辞つきのタグは通らない**（`v1.2.0-rc1` 等）。ワークフローが `vMAJOR.MINOR.PATCH` 以外を弾く。
- **`latest` の置き場と `rerics-windows.zip` の名前は動かさない。** アプリは取得先を `latest` 固定で
  組み立てる。ここを動かすと配布済みのアプリが更新を見つけられなくなる。
- **`manifest.json` の `build`（CI の実行番号）は外さない。** 1.0.x のアプリは版ではなくこの値で比べ、
  自分の値より大きいときだけ更新する。外すと 1.0.x の実機が自力で新しいバージョンへ乗り換えられない。
  実行番号はワークフローのファイルごとに増えるので、`release.yml` の名前も変えない。

## 配布せずに試したいとき

`Release` ワークフローを **workflow_dispatch で手動実行**すると、ビルドとテストのあと zip が成果物として
残るだけで、リリースには一切触れない。**成果物は 3 日で消えるので、その間にダウンロードする。**
バージョンは Cargo のバージョンのままなので、中の Rerics は配布中の同じバージョンと区別が付かない。
自動更新の確認にも使わない。
