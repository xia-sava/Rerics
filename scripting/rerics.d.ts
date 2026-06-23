// Rerics スクリプト API の型定義。
//
// WebStorm / VS Code はこの d.ts を読むと `rerics.*` を型付きで補完する。
// 実行時は deno_ast による型消去（型検査なし）で走るため、ここでの型は
// エディタ補助のためだけのもの。型の不一致はエディタ側でのみ検出される。
//
// 使い方：このファイルと tsconfig.json を、ユーザスクリプト（*.ts）と同じ
// フォルダ（%APPDATA%\Rerics\scripts）に置くと補完が効く。

/** `rerics.listDir()` が返す 1 エントリ。 */
interface RericsDirEntry {
  /** 名前のみ（パスではない）。 */
  name: string;
  /** ディレクトリなら true。 */
  isDir: boolean;
  /** バイト単位のサイズ（ディレクトリは 0）。 */
  size: number;
  /** 最終更新時刻（Unix epoch ミリ秒）。`new Date(mtime)` で扱える。取得不可なら 0。 */
  mtime: number;
}

/** Rerics 本体が提供するホスト API。グローバル `rerics` から呼ぶ。 */
declare namespace rerics {
  /** アプリのログ欄へメッセージを出す。 */
  function log(message: string): void;

  /** アクティブペインの現在ディレクトリ（絶対パス）を返す。 */
  function currentDir(): string;

  /** アクティブペインを `path` へ移動する。 */
  function navigate(path: string): void;

  /** 確認ダイアログ（はい/いいえ）を出す。「はい」なら true。 */
  function confirm(message: string): boolean;

  /** 入力ダイアログを出す。OK なら入力文字列、キャンセルなら null。 */
  function prompt(message: string, defaultValue?: string): string | null;

  /** 一覧から 1 つ選ばせる。選んだ行の index、キャンセルなら null。 */
  function select(title: string, items: string[]): number | null;

  /**
   * `path` 直下を裏スレッドで走査して返す。重いディレクトリでも UI を止めない。
   * `await` して使う。
   */
  function listDir(path: string): Promise<RericsDirEntry[]>;

  /**
   * 名前付きコマンドを登録する。`handler` は同期でも `async`（Promise を返す）でもよい。
   * 同名で再登録すると後勝ちで上書きする。
   */
  function registerCommand(name: string, handler: () => void | Promise<void>): void;
}
