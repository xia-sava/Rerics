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

/** ペイン内の 1 項目（`rerics.activePane()` などが返す）。 */
interface RericsItem {
  /** `items` 内での添字。 */
  readonly index: number;
  /** 表示名（拡張子込み）。 */
  readonly name: string;
  /** 拡張子を除いた名前。 */
  readonly baseName: string;
  /** 拡張子（ドット無し・無ければ空文字）。 */
  readonly ext: string;
  /** ディレクトリなら true。 */
  readonly isDir: boolean;
  /** 親（".."）エントリなら true。 */
  readonly isParent: boolean;
  /** バイト単位のサイズ（ディレクトリ・取得不可は 0）。 */
  readonly size: number;
  /** 最終更新時刻（Unix epoch ミリ秒）。`new Date(mtime)` で扱える。取得不可なら 0。 */
  readonly mtime: number;
  /**
   * 選択（マーク）されているか。
   *
   * 現状は読み取り専用（書き戻し配線は次の段階で入る）。値を代入しても今は反映されない。
   */
  readonly selected: boolean;
  readonly readonly: boolean;
  readonly hidden: boolean;
}

/**
 * 片側ペインの状態スナップショット（`rerics.activePane()` / `rerics.oppositePane()`）。
 *
 * 取得時点のコピーで、以後ペインが変化しても更新されない。項目アクセスごとのスレッド
 * 往復を避けるため、一覧を丸ごと 1 回で取得する。
 */
interface RericsPane {
  /** 現在地の表示パス（書庫内なら "C:\\foo.zip\\inner" 形式）。 */
  readonly dir: string;
  /** 書庫の中にいるか。 */
  readonly isArchive: boolean;
  /** カーソル行の index（`items` の添字）。 */
  readonly cursor: number;
  /** 表示順の項目一覧（".." を含む）。 */
  readonly items: RericsItem[];
  /** 選択（マーク）されている項目だけを抜き出した一覧。 */
  readonly selectedItems: RericsItem[];
  /** カーソル行の項目（範囲外なら null）。 */
  readonly cursorItem: RericsItem | null;
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
   * アクティブペインの現在状態（現在地・項目一覧・選択・カーソル）を取得する。
   * 返るのは取得時点のスナップショットで、以後の変化は反映されない。
   */
  function activePane(): RericsPane;

  /** 反対側ペインの現在状態を取得する。詳細は {@link activePane}。 */
  function oppositePane(): RericsPane;

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
