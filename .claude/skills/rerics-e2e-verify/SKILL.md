---
name: rerics-e2e-verify
description: Rerics の GUI を headless のデバッグ制御サーバ（--debug-server）で挙動・見た目を検証する手順。/state 観測・/command(POST)駆動・/snapshot 目視・/modal 自動操作、RERICS_DATA_DIR + state.toml での隔離起動、サーバ停止（netstat→taskkill）、feature ビルドの罠、e2e テストの書き方を含む。Rerics GUI の e2e・スナップショット・目視・実機検証をするときに使う。
---

# Rerics e2e / スナップショット検証

開発専用のローカル HTTP 制御・観測サーバ（`feature = "debug-server"`・既定 off・リリース非搭載）で、
**画面を見ずに HTTP だけで状態観測・コマンド駆動・スナップショット**ができる。窓を出さない
（`--headless`）から非侵襲で、夜間自走でも安全。視覚要素は `/snapshot/<要素>` の PNG を Read して目視する。

> このファイルだけ読めば一通り検証できる想定。詳細実装は `crates/rerics/src/debug_server.rs` /
> `debug_json.rs` / `tests/debug_server.rs`。

## 鉄則チェックリスト（毎回ここでつまずく）

1. **`/command/<X>` は POST 限定**。`curl` 既定は GET なので、素で叩くと 404 `not found` が返り
   **何も実行されない**（カーソルも動かずマークも付かない）。必ず `curl -X POST`。観測系
   `/state`・`/presentation`・`/snapshot` は GET でよい。
2. **feature 付き / 無し build は同じ `target/debug/rerics.exe` を奪い合う**。`./tools/dev.sh build`
   （feature 無し）や `dev.sh test` を挟むと debug-server が**消える**。しかもこの上書きは
   **検証フローの外**（並行する別作業の plain ビルド／テスト、ユーザの手元ビルド等）でもいつでも起き得る。
   **「session 中に一度ビルドしたから以後も feature 版のまま」と仮定してはいけない**——起動の直前に毎回
   ビルド状態を確認し、必要なら再ビルドする（迷ったら `./tools/dev.sh build --features debug-server` を
   無条件で打ち直してから起動するのが安全）。**feature 無しの exe を起動すると、feature ゲートのフラグ
   （`--headless` 等）が黙って無視され、非表示のはずが実 GUI 窓が画面に出る**（headless 検証のつもりが窓が飛ぶ）。
   **検証が終わったら `./tools/dev.sh build`（plain）で実行用 exe に戻す**（戻し忘れると本番起動でサーバが生きたまま）。
3. **停止は `netstat -ano`→`taskkill //PID`**。`/command/Quit` で正常終了すると **state.toml に現在地が
   保存**され次回起動の初期状態が変わる（＝テスト汚染）。`taskkill //F //IM rerics.exe` でも可だが、
   並列起動時はポートから PID を引いて個別に落とす（下記）。
4. **生きた exe は `target` をロックする**。サーバを止めずに plain build すると
   「アクセスが拒否されました (os error 5)」。**ビルド前に必ず停止**。
5. **headless でも `EnterDir` を“ファイル”に送ると `ShellExecute` が実デスクトップにダイアログを出す**
   （関連付け未設定だと「アプリを選ぶ」窓がユーザ画面に飛ぶ）。headless なのは Rerics 本体の窓だけ。
   → ファイルを開くのは内蔵ビューア **`ViewFile`（V）だけ**。`EnterDir`（Enter）は**ディレクトリ/書庫だけ**に送る。
   カーソル位置を `/state/panes/<side>/cursor` で確認してから叩く。
6. **ブロックし得る `/command` は `--max-time` 付き**か `run_in_background:true`。素の `curl ... &` は
   ツールがコマンド全体を背景化して扱いづらい。モーダルを開き得るコマンドは `MaybeModal` 化済で
   即応答が返るので素で叩いてよい（下記）。

## ビルドと起動

```sh
./tools/dev.sh build --features debug-server          # ① debug-server 込みでビルド（生 cargo は link 衝突で不可）
RERICS_DATA_DIR=<隔離dir> ./target/debug/rerics.exe --debug-server=<PORT> --headless &
```

起動フラグ：
- `--debug-server[=PORT]` … 既定 8731・`127.0.0.1` 限定。`=0` で OS 任せ（実ポートは起動ログ
  `[debug-server] listening on http://127.0.0.1:PORT` に出る＝並列起動で衝突しない）。
- `--headless` … 窓を一切出さず起動（スナップショットも撮れる・決定論）。
- 無印 `--debug-server` … 真の最小化起動（フォーカスを奪わない）。
- `--debug-allow-write` … 破壊的（ファイル操作）コマンドを許可（既定は 400 拒否）。
- `RERICS_DATA_DIR=<dir>` … state.toml/config.toml の置き場を上書き（テスト隔離用・**本番 state.toml を汚さない**）。

## エンドポイント cheatsheet

### 観測（GET・`/state` と `/presentation` は JSON Pointer でサブツリー取得可）
- `GET /state[/<ptr>]` … モデル状態。両ペインの items/cursor/scroll_top/visible/mask/sort/columns、
  `active_view`・`media`・`tab_bar`・`tabs`・`log`・`modal`・`window`・`active_pane`。
  例：`/state/panes/left/cursor`・`/state/panes/left/items`・`/state/modal`。
  - **item のマークは `marked` フィールド**（`selected` ではない）。`cursor` は bool（その行がカーソルか）。
- `GET /presentation[/<ptr>]` … 解決済み外見。`theme`・`resolved_colors`（hex）・`font`・`layout`・各ペインの
  色/フォント/header_height/item_height/scrollbar_width。例：`/presentation/font/size`。

### 駆動（POST・実行後 `/state` を返す）
- `POST /command/<Name>` … `Command` をアクティブ側ペインに実行。トークンは
  `crates/rerics-core/src/input.rs` の `from_token`（例 `CursorDown`/`CursorUp`/`MarkToggle`/`SelectAll`/
  `FocusLeft`/`FocusRight`/`EnterDir`/`ToParent`/`SortBySize`/`ViewFile`…）。未知/不可は 400。
  書込み系（`MakeDirectory`/`Rename`/`Delete`/`Copy`/`Move`）は要 `--debug-allow-write`。
- `POST /view/key/<next|prev|close>` … 重ね表示ビューア（画像/テキスト）の操作。
- `POST /modal/text`（body=値）／`POST /modal/key/<enter|esc|y|n|tab>`／
  `POST /modal/command/<ok|cancel|役割名|ctrl_id>`／`POST /modal/select/<n>` … 開いてるモーダルの操作。

### 撮影（GET・PNG・窓非依存合成でフラッシュ無し・headless でも撮れる）
- `GET /snapshot[.png]` … クライアント全体。
- `GET /snapshot/<name>` … 名前付き要素：`full`・`pane`・`list`・`path_bar`・`status_bar`・`tab_bar`・
  `log`・`cursor`。`_left`/`_right` を付けて左右指定（省略時アクティブ側）。例 `list_left`・`pane_right`。
- `GET /snapshot/<name>/<x,y-WxH>` … 要素相対のサブ範囲。`GET /snapshot/modal` … 開いてるモーダル。

## 標準フロー（手動・隔離起動 → 駆動 → 目視 → 停止 → 復帰）

```sh
# ① 隔離 dir と state.toml を用意（両ペインを検証用ディレクトリへ向ける）
D="$(mktemp -d)"; mkdir -p "$D/data" "$D/sbx"
for f in alpha.txt beta.txt gamma.txt; do echo x > "$D/sbx/$f"; done
SBX="$(cd "$D/sbx" && pwd -W)"          # Windows 形式の絶対パス
printf "active_tab = 0\nsplit_ratio = 0.5\n[[tabs]]\nleft = '%s'\nright = '%s'\nactive_right = false\n" \
  "$SBX" "$SBX" > "$D/data/state.toml"
#   state.toml の path はシングルクォート（リテラル）＝バックスラッシュも '/' もOK。active_right=false で左がアクティブ。

# ② ビルド & 起動
./tools/dev.sh build --features debug-server
RERICS_DATA_DIR="$(cd "$D/data" && pwd -W)" ./target/debug/rerics.exe --debug-server=8755 --headless &
sleep 2; curl -s --max-time 5 127.0.0.1:8755/state >/dev/null && echo up

# ③ 駆動（POST！）。カーソルを実ファイルへ動かしてからマーク等。
P=8755
curl -s -X POST --max-time 5 "127.0.0.1:$P/command/CursorDown" >/dev/null   # ..→alpha
curl -s -X POST --max-time 5 "127.0.0.1:$P/command/MarkToggle" >/dev/null
curl -s --max-time 5 "127.0.0.1:$P/state" | python3.12 -c \
  "import sys,json;p=json.load(sys.stdin)['panes']['left'];print(p['cursor'],[(i['name'],i.get('marked')) for i in p['items']])"

# ④ 目視（PNG を保存して Read ツールで開く）
curl -s --max-time 8 "127.0.0.1:$P/snapshot/list_left" -o /tmp/shot.png   # → Read /tmp/shot.png

# ⑤ 停止（ポートから listening PID を引いて taskkill。/command/Quit は使わない）
PID=$(netstat -ano | grep "127.0.0.1:$P" | grep LISTENING | awk '{print $5}' | head -1 | tr -d '\r')
taskkill //PID "$PID" //F
sleep 1; curl -s --max-time 3 127.0.0.1:$P/state >/dev/null 2>&1 && echo STILL_UP || echo down

# ⑥ 実行用 exe に戻す & 後始末
./tools/dev.sh build
rm -rf "$D"
```

## 視覚要素の目視検証

- **手順**：`/snapshot/<要素>` を `-o x.png` で保存 → **Read ツールで PNG を開いて目視**。要素名は上記。
- **左右で見比べる**：アクティブ/非アクティブの差（カーソル下線・マーク色など）は
  `list_left` と `list_right` を2枚撮って並べる。先に `FocusLeft`/`FocusRight` で意図した側をアクティブにする。
- **一瞬の状態（スピナー等）を撮る**：`/snapshot/pane_<side>` を ~150ms 間隔で 20〜30 連写し、PNG サイズ差
  （ほぼ黒の読込画面＝小・一覧＝大）で該当フレームを特定すると速い。
- 実画面の地の証拠が要るとき、または **debug-server で撮れないモーダル**を撮るときは `tools/ui.ps1`
  （フラッシュあり・`-Foreground` で別窓対応／詳細は下記レシピ）。ユーザ作業中なら一声かける。

## モーダルの自動操作（MaybeModal とデッドロック回避）

- **罠**：debug-server は単一スレッドで `/command` を逐次処理し、UI スレッドへ積んで `recv` でブロックする。
  モーダルを開くコマンドを「exec してから応答」にすると、exec がモーダル待ちで止まった瞬間に HTTP が固まり
  後続の `/modal/*` を捌けず**完全デッドロック**。
- **対処（実装済）**：モーダルを開き得るコマンドは `DebugCmdClass`（main.rs）で **exec 前に応答**を返す。
  書込み系＝`ModalWrite`（要 allow_write）、読取だがモーダルを開き得る `ViewFile` 等＝`MaybeModal`（allow_write 不要）。
- **撮れる／撮れないの境目**：`/state/modal`・`/modal/*`・`/snapshot/modal` で観測・操作・撮影できるのは
  **`modal_registry`（debug_server.rs）に push 登録され、かつ開くコマンドが `MaybeModal`／`ModalWrite`** の
  モーダルだけ（`dialog::` 系はこの両方を満たす）。winsafe の `show_modal` を registry 登録せず回すダイアログは
  `DebugCmdClass::Unsupported` で、debug-server からは**起動も観測もできない**＝**実窓キャプチャが唯一の手段**（下記レシピ）。
- **手順例（入力欄つきモーダルを値設定→確定まで自動化）**：
  `POST /command/<Name>`（即 `{"maybe_modal":true}` 応答）→ `GET /state/modal`（入力ボックスが見える）→
  `POST /modal/text`（body=値）→ `POST /modal/key/enter` → `GET /state` で結果を確認。
  リスト選択モーダルは `POST /modal/select/<n>`。

## 個別レシピ

### 特定ディレクトリを開いた状態で始める
上記フロー①の通り、`RERICS_DATA_DIR` 隔離の `state.toml` で両ペインを検証用 dir へ向ける（推奨）。
本番 state.toml を差し替える方式（`cp state.toml state.toml.bak` → 最小 state.toml → 検証 → `mv` 復元）も可だが、
隔離の方が**本番を一切触らず**安全。**罠**：Bash ツールは前コマンドの `cd` が残る。検証ファイル作成・exe 起動は
絶対パスで書くか先頭で repo root へ。SJIS テスト材料は `iconv -f UTF-8 -t CP932 utf8.txt > sjis.txt`。

### テーマ（dark/light）両方を撮る
`theme=System` は OS 依存で片方しか見えない。検証中だけ隔離 `config.toml` に `theme="dark"` / `theme="light"` を
書いて各起動でスクショ（本番 config.toml は通常空なので、隔離 dir 側に書けば復元不要）。F5=Reload は無害。

### debug-server で撮れないモーダルを実窓キャプチャする
registry 非登録の `Unsupported` モーダル（winsafe `show_modal` 系）は debug-server で観測・撮影できないので、
実ウィンドウを出して `tools/ui.ps1 -Foreground` で前面に出た別窓を直接撮る（ユーザ作業中なら一声）。手順は汎用:

1. メイン窓を `--headless` でなく**通常起動**（`Start-Process target/debug/rerics.exe`）。
2. `-Keys` でモーダルを開くトリガを送る（`%`＝Alt でメニューのニーモニックを順に押す）。
3. 開いた窓のフォーカス済みコントロールは `-PostKeys`（矢印・Tab 等）で動かす（リストや項目の移動）。
4. `-Foreground` で前面窓を撮る（`GetForegroundWindow` 基準＝別窓が前面なら `EnumWindows` 探索は不要）。
5. `-Close` で Esc 閉じ＋メイン窓最小化（作業画面に残さない）。

```pwsh
pwsh -File tools/ui.ps1 -Keys "<開くキー>" -PostKeys "<移動キー>" -Foreground -Close   # → target/shot.png を Read で目視
```

ラジオ等はニーモニック（`%`＋文字）、OK=Enter・Cancel=Esc。テーマ別に撮るなら隔離 config.toml に `theme` を
書いて起動を分ける（上記「テーマ両方を撮る」と同じ）。検証後は隔離 config.toml を空に戻す。

## e2e テストの書き方（3パターン・spawn 無しが基本）

1. **挙動** … core ユニットテスト（`crates/rerics-core/src/*.rs`）。状態を組む→メソッド→assert。`./tools/dev.sh test -p rerics-core`。
2. **外見/シリアライズ** … `crates/rerics/src/debug_json.rs` の純粋関数を直接コール（spawn 不要）。
   `pane_state_json`/`presentation_top_json` を手組みデータで呼び JSON を assert。
3. **e2e 煙テスト** … `crates/rerics/tests/debug_server.rs`。実 exe を headless spawn→HTTP で act/observe。
   ゲートは `feature = "debug-server"` 一本（`#[ignore]` は使わない）。実行は `./tools/dev.sh test --features debug-server`。
   - **`Server` ガードが面倒を全部見る**：`--debug-server=0` で起動し子の stdout から実ポートを読む／作業dirは
     `プロセスID＋連番` でユニーク化／**Drop で子 kill＋作業dir削除**。新テストは `Server::start(&[files], config_toml)`
     して `server.req("POST", "/command/...", "")` を叩くだけ。書込み系は `start_writable`、書庫は `start_archive`。

普段の `./tools/dev.sh test`（feature なし）は spawn ゼロで速い。**ただし feature 無し build は debug-server exe を
上書きする**（鉄則②）ので、手動検証と並行するときは順序に注意。

## 関連ファイル
- `crates/rerics/src/debug_server.rs` … HTTP・ルーティング・モーダルレジストリ（feature 下）。
- `crates/rerics/src/debug_json.rs` … 応答 JSON を組む純粋関数（常時ビルド・ユニットテスト）。
- `crates/rerics/src/main.rs` の `debug_*` メソッド群・`DebugCmdClass`（NonModal/MaybeModal/ModalWrite）。
- `crates/rerics/tests/debug_server.rs` … e2e 煙テストと `Server` ガード（state.toml 書式の実例）。
- 設計の経緯 `(debug-server 設計メモ)`。実装勘所の続きは `(実装メモ)`。
