# テスト用フィクスチャ

- `version.rar` … rar 読取（`rar` feature）の自走テスト用の最小サンプル。
  単一エントリ `VERSION`（内容 `unrar-0.4.0`・11バイト）。
  出所：MIT ライセンスの `unrar` crate（0.5.8）の同梱テストデータ `data/version.rar`。
  rar は当環境で作成手段が無い（7z も rar 作成不可）ため既存サンプルを流用。
- `solid.7z` / `nonsolid.7z` … 7z 読取（`SevenZBackend`）の自走テスト用。
  3ファイル（`a.txt`=AAA・`sub/c.txt`=CCC・`sub/d.txt`=DDD）。
  `solid.7z` は `7z a -ms=on`（Blocks=1＝ソリッド）、`nonsolid.7z` は `-ms=off`（Blocks=3）。
  ソリッド判定（`caps().random_access` の反転）と一括展開のテストに使う。
