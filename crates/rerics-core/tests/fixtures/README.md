# テスト用フィクスチャ

- `version.rar` … rar 読取（`rar` feature）の自走テスト用の最小サンプル。
  単一エントリ `VERSION`（内容 `unrar-0.4.0`・11バイト）。
  出所：MIT ライセンスの `unrar` crate（0.5.8）の同梱テストデータ `data/version.rar`。
  rar は当環境で作成手段が無い（7z も rar 作成不可）ため既存サンプルを流用。
- `solid.7z` / `nonsolid.7z` … 7z 読取（`SevenZBackend`）の自走テスト用。
  3ファイル（`a.txt`=AAA・`sub/c.txt`=CCC・`sub/d.txt`=DDD）。
  `solid.7z` は `7z a -ms=on`（Blocks=1＝ソリッド）、`nonsolid.7z` は `-ms=off`（Blocks=3）。
  ソリッド判定（`caps().random_access` の反転）と一括展開のテストに使う。
- `tree.tar` / `tree.tar.gz` / `tree.tar.bz2` / `tree.tar.xz` / `tree.tar.zst` … `TarBackend` 用。
  中身は 7z fixture と同じ3ファイル（a.txt=AAA・sub/c.txt=CCC・sub/d.txt=DDD）。
  GNU tar で生成（`tar -C src -c{,z,j,J}f` ／zstd は `tar -cf - | zstd`）。各圧縮ラップの
  list/read/一括展開を網羅。
- `note.txt.xz` / `note.txt.gz` … `SingleFileBackend` 用（単体圧縮＝1エントリ）。
  中身は `hello world`、内側エントリ名は圧縮拡張子を除いた `note.txt`。
