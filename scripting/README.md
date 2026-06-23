# Rerics スクリプティング

Rerics は TS/JS で動作をカスタマイズできる。スクリプトは V8（deno_core 埋め込み）で
実行され、`rerics.*` ホスト API からファイラ本体を操作する。

## 置き場所

`%APPDATA%\Rerics\scripts`（ポータブル版は実行ファイルと同じフォルダの `scripts`）に
`*.ts` / `*.js` を置くと、起動時にファイル名順で読み込まれる。番号プレフィクス
（`10-foo.ts` など）で読み込み順を制御できる。

## 型付き開発（WebStorm / VS Code）

このフォルダの `rerics.d.ts` と `tsconfig.json` を scripts フォルダにコピーすると、
`rerics.*` が型付きで補完される。型は実行時には消去される（型検査はエディタ側の責務）。

```ts
rerics.registerCommand("up", () => {
  rerics.navigate(rerics.currentDir() + "/..");
});
```

## API

| 呼び出し | 説明 |
| --- | --- |
| `rerics.log(message)` | ログ欄へ出力 |
| `rerics.currentDir()` | アクティブペインの現在ディレクトリ |
| `rerics.navigate(path)` | アクティブペインを移動 |
| `rerics.confirm(message)` | 確認ダイアログ（はい/いいえ）→ `boolean` |
| `rerics.prompt(message, default?)` | 入力ダイアログ → `string \| null`（キャンセルで null） |
| `rerics.select(title, items)` | 一覧から選択 → `number \| null`（選んだ index・キャンセルで null） |
| `rerics.activePane()` | アクティブペインの状態スナップショット → `RericsPane` |
| `rerics.oppositePane()` | 反対側ペインの状態スナップショット → `RericsPane` |
| `rerics.command(name, ...args)` | 内蔵コマンドを実行（同期・不明名/失敗は例外） |
| `await rerics.listDir(path)` | ディレクトリ走査（裏スレッド・`Promise<RericsDirEntry[]>`） |
| `rerics.registerCommand(name, handler)` | 名前付きコマンドを登録（handler は同期/async どちらでも） |
| `rerics.on(event, handler)` | 本体のイベントを購読（`changeDirectory` / `executeCommand`） |
| `await rerics.copy()` / `await rerics.move()` / `await rerics.delete()` | 選択をコピー/移動/削除（ワーカー実行・完了を待てる・`cancel()` 可・`onProgress` で進捗） |

詳細な型は `rerics.d.ts` を参照。

### ペインのオブジェクトモデル

`activePane()` / `oppositePane()` は、現在表示中のペイン状態を **取得時点のスナップショット**
として返す（項目アクセスごとのスレッド往復を避けるため一括取得）。`listDir` と違い、実際に
表示・選択されている項目をそのまま読める。

```ts
const p = rerics.activePane();
p.dir;             // 現在地（書庫内なら "C:\foo.zip\inner" 形式）
p.items;           // 表示順の項目一覧（".." を含む）
p.selectedItems;   // 選択（マーク）中の項目だけ
p.cursorItem;      // カーソル行の項目（範囲外なら null）
for (const it of p.items) {
  it.name; it.baseName; it.ext; it.isDir; it.size; it.mtime; it.selected;
}
```

#### 選択の書き戻し

項目の `selected` は **代入できる**。`activePane()`/`oppositePane()` から得た項目への代入は
即時にペインへ反映される。多数をループで選ぶときは `pane.apply()` を使うと、コールバック内の
変更を **1 往復でまとめて反映**する（項目ごとのスレッド往復を避けられて軽い）。

```ts
// 即時：カーソル直下をその場でトグル
const it = rerics.activePane().cursorItem;
if (it) it.selected = !it.selected;

// まとめて：.txt を全部選択（apply の draft は即時反映しないペイン）
rerics.activePane().apply((d) => {
  for (const it of d.items) if (it.ext === "txt") it.selected = true;
});
```

書き戻しは取得時の行 index を指すため、スナップショット取得後に一覧がリロードされると
ずれる。読み取り→書き戻しは同じコマンド内で完結させること。

### 内蔵コマンドの実行

`rerics.command(name, ...args)` でファイラー本体のコマンドを名前で実行できる（カーソル移動・
ソート・コピー・削除など）。アクティブペイン文脈・同期で、不明な名前や実行失敗は例外を投げる。

```ts
rerics.activePane().apply((d) => {
  for (const it of d.items) if (it.ext === "tmp") it.selected = true;
});
rerics.command("Delete");  // 選んだ .tmp を削除（確認は本体設定に従う）
```

`rerics.command()` でワーカーを起動する操作（コピー/移動/削除など）は「開始」まで戻り、
**完了は待たない**。完了を待ちたいときは下記の `await` 版を使う。

### 非同期ファイル操作（完了を待つ）

`await rerics.copy()` / `await rerics.move()` / `await rerics.delete()` は、アクティブペインの選択
（無ければカーソル）項目を処理する（copy/move は反対ペインへ・delete はその場で）。ワーカー
スレッドで実行し、**完了するまで待てる** job を返す。失敗・中止は例外になる（`try/catch`）。

```ts
rerics.activePane().apply((d) => {
  for (const it of d.items) if (it.ext === "txt") it.selected = true;
});
await rerics.copy();        // コピー完了まで待つ
rerics.log("コピー完了");
```

同名衝突は本体の確認ダイアログで解決する（`await` 中もダイアログは反応する）。

返り値は job（`Promise` ＋ `cancel()`）。`job.cancel()` で進行中の操作を中止できる（中止されると
`await` は例外になる）。

```ts
const job = rerics.copy();
// 何らかの条件で job.cancel();
await job;
```

対象と行き先を明示することもできる（`items` はフルパス配列・`item.fullName` を使う）。`items` が
複数ディレクトリにまたがっても 1 つの job として扱える。

```ts
const p = rerics.activePane();
const items = p.items.filter((it) => !it.isDir).map((it) => it.fullName);
await rerics.copy(items, rerics.oppositePane().dir);   // 明示ベース
```

進捗を受け取りたいときは末尾に `{ onProgress }` を渡す。選択ベースなら `copy(options)`、明示ベース
なら `copy(items, dest, options)`。`onProgress` は処理が進むたびに `{ text }` で呼ばれる。

```ts
await rerics.copy({
  onProgress: (p) => rerics.log(p.text),   // 処理中のファイル・割合など
});
```

### イベントの購読

`rerics.on(event, handler)` で本体のイベントに反応できる。同じイベントに複数登録でき、登録順に
呼ばれる。ハンドラは同期でも `async` でもよい。

- `changeDirectory(dir)`：いずれかのペインの現在地が**実際に変わった**とき（在席再読込・F5・
  操作後の再読込では発火しない）。引数は新しい現在地パス。
- `executeCommand(name)`：内蔵コマンドが実行されたとき。引数はコマンド名。スクリプト発の
  `rerics.command()` 実行中は**発火しない**（自己再帰を避けるため）。

```ts
rerics.on("changeDirectory", (dir) => {
  if (dir.endsWith("photos")) rerics.command("SortByDate");
});
```

`changeDirectory` ハンドラの中で**無条件に移動すると無限ループ**になりうる。条件を付けること。

## 共通処理・複数ファイル

共通の処理は `registerCommand` せず、普通の関数として定義して各コマンドから呼べばよい。

```ts
async function filesHere() {
  return (await rerics.listDir(rerics.currentDir())).filter((e) => !e.isDir);
}

rerics.registerCommand("countFiles", async () => {
  rerics.log(`${(await filesHere()).length} 個のファイル`);
});
```

すべてのスクリプトは同じグローバル環境でファイル名順に読み込まれるため、先に読まれた
ファイルのトップレベル関数・定数は、後のファイルからも参照できる。共通処理を別ファイルに
切り出す場合は、番号プレフィクスで先に読ませる（例：`00-lib.ts` → `10-commands.ts`）。

## 制限（現状）

- スクリプトは classic script として実行される。**トップレベルの `await` / `import` は不可**。
  非同期処理は `async` 関数（`registerCommand` の handler や IIFE）の中で行う。
