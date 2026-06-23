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
| `await rerics.listDir(path)` | ディレクトリ走査（裏スレッド・`Promise<RericsDirEntry[]>`） |
| `rerics.registerCommand(name, handler)` | 名前付きコマンドを登録（handler は同期/async どちらでも） |

詳細な型は `rerics.d.ts` を参照。

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
