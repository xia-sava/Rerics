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
   * 選択（マーク）されているか。**代入できる**。
   *
   * `activePane()`/`oppositePane()` から得た項目への代入は即時にペインへ反映される。
   * `pane.apply()` の draft から得た項目への代入はコールバック終了時にまとめて反映される。
   */
  selected: boolean;
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

  /** `for (const it of pane)` で項目を走査できる（`pane.items` と同じ並び）。 */
  [Symbol.iterator](): Iterator<RericsItem>;

  /**
   * 選択変更をまとめて 1 回で反映する。`draft` は「即時反映しないペイン」で、その項目へ
   * 代入した `selected` はコールバック終了時に一括適用される（項目ごとのスレッド往復を
   * 避けるので、多数選択のループはこちらが軽い）。自分自身を返す。
   *
   * ```ts
   * rerics.activePane().apply((d) => {
   *   for (const it of d.items) if (it.ext === "txt") it.selected = true;
   * });
   * ```
   */
  apply(fn: (draft: RericsPane) => void): RericsPane;
}

/**
 * `rerics.on` で購読できるイベント名。引数はイベントごとに異なる。
 * - `changeDirectory`：いずれかのペインの現在地が実際に変わったとき。引数＝新しい現在地パス。
 * - `executeCommand`：内蔵コマンドが実行されたとき。引数＝コマンド名。
 *   （スクリプト発の `rerics.command()` 実行中は自己再帰を避けるため発火しない。）
 */
type RericsEvent = "changeDirectory" | "executeCommand";

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
   * 内蔵コマンドを名前で実行する（アクティブペイン文脈・同期）。引数は文字列で渡す。
   * 不明なコマンド名・実行失敗は例外を投げる（`try/catch` で拾える）。
   *
   * ワーカーを起動する操作（コピー/移動/削除など）は「開始」まで戻り、**完了は待たない**。
   * 完了を待ちたい操作向けの `await` 版は今後追加する。
   *
   * ```ts
   * rerics.activePane().apply((d) => {
   *   for (const it of d.items) if (it.ext === "tmp") it.selected = true;
   * });
   * rerics.command("Delete");   // 選んだ .tmp を削除
   * ```
   */
  function command(name: string, ...args: string[]): void;

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

  /**
   * ファイラー本体のイベントにハンドラを登録する。同じイベントに複数登録でき、登録順に
   * 呼ばれる。ハンドラは同期でも `async` でもよい。引数はイベントごとに異なる（{@link RericsEvent}）。
   *
   * 注意：`changeDirectory` ハンドラの中で無条件に移動すると無限ループになりうる。条件を付けること。
   *
   * ```ts
   * rerics.on("changeDirectory", (dir) => {
   *   if (dir.endsWith("photos")) rerics.command("SortByDate");
   * });
   * ```
   */
  function on(event: RericsEvent, handler: (arg: string) => void | Promise<void>): void;
}
