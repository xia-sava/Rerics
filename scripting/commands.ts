// Rerics ユーザスクリプトの例。
//
// このフォルダ（%APPDATA%\Rerics\scripts）に置いた *.ts / *.js は、起動時に
// ファイル名順で読み込まれる。WebStorm では rerics.* が型付きで補完される。
//
// 各スクリプトは registerCommand で「名前付きコマンド」を登録する。コマンドは
// あとから名前で呼び出される（将来はキーバインドにも結べる予定）。

// 共通ユーティリティは register せず普通の関数として定義し、各コマンドから呼ぶ。
// 別ファイルに分けても、名前順で先に読まれていれば後のファイルから参照できる
// （例：00-lib.ts に置いて 10-commands.ts から使う）。
function fmtSize(bytes: number): string {
  return `${(bytes / 1024).toFixed(1)} KiB`;
}
async function filesHere(): Promise<RericsDirEntry[]> {
  return (await rerics.listDir(rerics.currentDir())).filter((e) => !e.isDir);
}

// 同期コマンド：アクティブペインを親ディレクトリへ。
rerics.registerCommand("up", () => {
  rerics.navigate(rerics.currentDir() + "/..");
});

// 非同期コマンド：現在地の件数を数えてログへ（重い走査は await で裏スレッド実行）。
rerics.registerCommand("countHere", async () => {
  const entries = await rerics.listDir(rerics.currentDir());
  const dirs = entries.filter((e) => e.isDir).length;
  rerics.log(`${entries.length} 件（うちディレクトリ ${dirs}）`);
});

// mtime を使う例：現在地でいちばん新しいファイルを探してログへ。
rerics.registerCommand("newestHere", async () => {
  const files = await filesHere();
  if (files.length === 0) {
    rerics.log("ファイルなし");
    return;
  }
  const newest = files.reduce((a, b) => (b.mtime > a.mtime ? b : a));
  rerics.log(`最新: ${newest.name}（${new Date(newest.mtime).toLocaleString()}）`);
});

// size を使う例：現在地のファイルを大きい順に上位 5 件ログへ。
rerics.registerCommand("biggestHere", async () => {
  const files = await filesHere();
  files
    .sort((a, b) => b.size - a.size)
    .slice(0, 5)
    .forEach((f, i) => rerics.log(`${i + 1}. ${f.name} — ${fmtSize(f.size)}`));
});

// 直近 1 日以内に更新されたファイルだけ数える（mtime での絞り込み）。
rerics.registerCommand("recentHere", async () => {
  const dayAgo = Date.now() - 24 * 60 * 60 * 1000;
  const recent = (await filesHere()).filter((e) => e.mtime >= dayAgo);
  rerics.log(`24時間以内に更新: ${recent.length} 件`);
});
