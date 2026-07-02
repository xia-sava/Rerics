# Rerics

Windows 向けの2画面ファイラー．gary 氏のファイラー **Records** を Rust で現代化した，非公式のクローンです．

## 由来

X68000 の2画面ファイラー **TF / STF** の精神的後継として作られた **mint**，その mint の精神的後継として
gary 氏が作られた **Records** ── その Records が好きすぎて，現代の環境で動く形にしたくてクローンを作りました．
それが Rerics です．

手触り（2画面・キー操作中心・機能欄でのカスタマイズ）を受け継ぎつつ，実装は Rust で新規に書き起こしています．
Records 本体のソースやリソースは含みません．挙動を観察して再実装した独立プロジェクトで，原作者・原作とは
無関係の非公式なものです．

## 構成

Rust の workspace（edition 2024）で，2つのクレートに分かれています．

- **`rerics-core`** … UI 非依存のロジック．ファイル一覧・ソート・コマンド定義・機能欄の式パーサなど．
  GUI にも OS にも依存せず単体でテストできる．
- **`rerics`** … GUI（[winsafe](https://github.com/rodrigocfd/winsafe)）と OS 連携，組込スクリプトエンジン
  （deno_core・埋め込み V8）．`rerics-core` を土台に画面・入力・外部プロセス・スクリプト API を実装する．

詳しくは [`ARCHITECTURE.md`](ARCHITECTURE.md) を参照してください．

## ビルド

Windows + Rust（edition 2024）+ MSVC ツールチェインが必要です（winsafe が Win32 API に直接触れるため
Windows 専用）．

```sh
./tools/dev.sh build   # git-bash 用ラッパ（MSVC 環境込みで cargo を呼ぶ）
./tools/dev.sh run
./tools/dev.sh test
```

MSVC 環境を整えた PowerShell / コマンドプロンプトからは，通常どおり `cargo build` でもビルドできます．

なお winsafe は，モーダルを閉じる際の use-after-free を修正したローカル版（`vendor/winsafe`）へ
`[patch.crates-io]` で差し替えています（上流で修正されたら外す予定）．

## スクリプティング

TS/JS で動作をカスタマイズできます．`%APPDATA%\Rerics\scripts` に置いたスクリプトが起動時に読み込まれ，
`rerics.*` ホスト API からファイラ本体を操作します．詳細は [`scripting/README.md`](scripting/README.md)．

## 状態

個人的に作っている，開発途中のプロジェクトです．全てのコードを Claude Code で書いており，
リポジトリ主はコードを1ミリも見ていません．

## クレジット

- 着想と手触りの源である **Records**（gary 氏）と，その源流である **mint** / **TF・STF** に敬意を表します．

同梱・利用しているサードパーティ:

- **[winsafe](https://github.com/rodrigocfd/winsafe)**（MIT）… Win32 GUI バインディング．モーダルを閉じる際の
  use-after-free を修正したローカル版を `vendor/winsafe` に同梱（[`LICENSE.md`](vendor/winsafe/LICENSE.md)）．
- **[PDFium](https://pdfium.googlesource.com/pdfium/)**（BSD-3-Clause）… PDF ラスタライズ．
  [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) の事前ビルド `pdfium.dll` を
  `vendor/pdfium` に同梱（同梱ラッパの [`LICENSE`](vendor/pdfium/LICENSE) は MIT，PDFium 本体と依存ライブラリの
  ライセンスは [`vendor/pdfium/licenses/`](vendor/pdfium/licenses)）．
- **[UnRAR](https://www.rarlab.com/)**（`unrar` / `unrar_sys` クレート経由）… RAR 書庫の展開に使用．
  UnRAR ライセンスにより，RAR 書庫の取り扱いは自由ですが，RAR 圧縮アルゴリズムの再現には使用できません．

Rerics 本体のライセンスは現時点で未設定です（全権利留保．とはいえクローンアプリで権利を主張するとかおこがましいですが）．
