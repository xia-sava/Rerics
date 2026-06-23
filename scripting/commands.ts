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

// オブジェクトモデルの例：表示中の項目・選択・カーソルを listDir 無しで読む。
// activePane() は現在ペインのスナップショット（dir / items / selectedItems / cursorItem）。
rerics.registerCommand("paneInfo", () => {
  const p = rerics.activePane();
  rerics.log(`現在地: ${p.dir}`);
  rerics.log(`項目数: ${p.items.length}（選択 ${p.selectedItems.length}）`);
  rerics.log(`カーソル: ${p.cursorItem ? p.cursorItem.name : "なし"}`);
});

// 選択中ファイルの合計サイズを出す（書込み系を介さない読み取りの実用例）。
rerics.registerCommand("selectedSize", () => {
  const files = rerics.activePane().selectedItems.filter((it) => !it.isDir);
  if (files.length === 0) {
    rerics.log("ファイルが選択されていません");
    return;
  }
  const total = files.reduce((sum, it) => sum + it.size, 0);
  rerics.log(`選択 ${files.length} ファイル — 合計 ${fmtSize(total)}`);
});

// 書き戻しの例：拡張子を入力させ、その拡張子のファイルをまとめて選択する。
// apply() の中で代入した selected は、コールバック終了時に 1 往復でまとめて反映される。
rerics.registerCommand("selectByExt", () => {
  const ext = rerics.prompt("選択する拡張子（例: txt）", "txt");
  if (ext === null) return;
  let n = 0;
  rerics.activePane().apply((d) => {
    for (const it of d.items) {
      if (!it.isDir && it.ext === ext) {
        it.selected = true;
        n++;
      }
    }
  });
  rerics.log(`${ext} を ${n} 件選択しました`);
});

// 即時書き戻しの例：カーソル直下の選択をその場でトグルする。
rerics.registerCommand("toggleCursor", () => {
  const it = rerics.activePane().cursorItem;
  if (it && !it.isParent) it.selected = !it.selected;
});

// select → 実行 の例：一時ファイル（.tmp/.bak）をまとめて選択して削除コマンドを呼ぶ。
// 内蔵コマンドは rerics.command(name, ...args) で叩ける（不明名・失敗は例外）。
rerics.registerCommand("cleanTemp", () => {
  let n = 0;
  rerics.activePane().apply((d) => {
    for (const it of d.items) {
      if (!it.isDir && (it.ext === "tmp" || it.ext === "bak")) {
        it.selected = true;
        n++;
      }
    }
  });
  if (n === 0) {
    rerics.log("一時ファイルはありません");
    return;
  }
  rerics.command("Delete"); // 確認ダイアログは本体側の設定に従う
});

// イベントの例：ディレクトリ移動を購読し、ログへ出す（changeDirectory）。
rerics.on("changeDirectory", (dir) => {
  rerics.log(`移動: ${dir}`);
});

// イベントの例：実行されたコマンドを記録する（executeCommand）。
// rerics.command() 発のコマンドは自己再帰回避のため発火しないので、ここでは無限ループしない。
rerics.on("executeCommand", (name) => {
  rerics.log(`コマンド: ${name}`);
});

// 非同期操作の例：選択した .txt を反対ペインへコピーし、完了を待ってからログを出す。
rerics.registerCommand("copyTxt", async () => {
  rerics.activePane().apply((d) => {
    for (const it of d.items) if (it.ext === "txt") it.selected = true;
  });
  await rerics.copy();
  rerics.log("コピー完了");
});

// 非同期削除の例：一時ファイルを選択して削除し、完了を待つ（await で順番を保てる）。
rerics.registerCommand("purgeTemp", async () => {
  let n = 0;
  rerics.activePane().apply((d) => {
    for (const it of d.items) {
      if (!it.isDir && (it.ext === "tmp" || it.ext === "bak")) {
        it.selected = true;
        n++;
      }
    }
  });
  if (n === 0) {
    rerics.log("一時ファイルはありません");
    return;
  }
  await rerics.delete();
  rerics.log(`${n} 件を削除しました`);
});

// モーダルの例：確認・入力・一覧選択。
rerics.registerCommand("askThings", async () => {
  if (!rerics.confirm("続けますか？")) {
    rerics.log("キャンセルしました");
    return;
  }
  const name = rerics.prompt("名前を入力", "");
  if (name === null) return;
  const dirs = (await rerics.listDir(rerics.currentDir())).filter((e) => e.isDir);
  if (dirs.length > 0) {
    const idx = rerics.select("移動先を選択", dirs.map((d) => d.name));
    if (idx !== null) rerics.navigate(rerics.currentDir() + "/" + dirs[idx].name);
  }
  rerics.log(`こんにちは、${name}`);
});
