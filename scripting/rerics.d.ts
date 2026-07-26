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
  /** フルパス（明示的な操作対象指定に使える。例：`rerics.copy(items.map(i => i.fullName), dest)`）。 */
  readonly fullName: string;
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
  /** 作成時刻（Unix epoch ミリ秒）。`new Date(ctime)` で扱える。取得不可なら 0。 */
  readonly ctime: number;
  /** 最終アクセス時刻（Unix epoch ミリ秒）。`new Date(atime)` で扱える。取得不可なら 0。 */
  readonly atime: number;
  /**
   * 選択（マーク）されているか。**代入できる**。
   *
   * `activePane()`/`oppositePane()` から得た項目への代入は即時にペインへ反映される。
   * `pane.apply()` の draft から得た項目への代入はコールバック終了時にまとめて反映される。
   */
  selected: boolean;
  readonly readonly: boolean;
  readonly hidden: boolean;
  /** システム属性。 */
  readonly system: boolean;
  /** アーカイブ属性（書庫内かどうかではなく属性ビット）。 */
  readonly archive: boolean;
  /** 再解析ポイント（シンボリックリンク・ジャンクション等）。 */
  readonly reparse: boolean;
  /**
   * リンク種別。junction=ディレクトリジャンクション、symlink=NTFS シンボリックリンク、
   * wsl=WSL 形式（Cygwin 3.4+ の既定もこれ）、cygwin=Cygwin 旧来型（cookie ファイル）、
   * reparse=その他の再解析ポイント。リンクでなければ null。
   */
  readonly link: "junction" | "symlink" | "wsl" | "cygwin" | "reparse" | null;
  /** リンク先の表示文字列（取れなければ null。WSL/Cygwin 形式は POSIX パス）。 */
  readonly linkTarget: string | null;
  /** 書庫など仮想ディレクトリ内の項目なら true。 */
  readonly virtual: boolean;
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

/** 非同期ファイル操作の進捗（`onProgress` に渡る）。 */
interface RericsProgress {
  /** 進捗の本文（処理中のファイル名など）。 */
  readonly text: string;
  /** 済んだ件数（数えられる操作のみ。`unpack` など）。 */
  readonly done?: number;
  /** 全件数（数えられる操作のみ）。`RericsLogLine.setProgress` にそのまま渡せる。 */
  readonly total?: number;
}

/** 非同期ファイル操作のオプション。 */
interface RericsOpOptions {
  /** 進捗があるたびに呼ばれる（完了前に 0 回以上）。 */
  onProgress?: (progress: RericsProgress) => void;
}

/**
 * 非同期ファイル操作のハンドル。`await` で完了を待て、`cancel()` で中止できる。
 * 失敗・中止すると `await` は例外になる。
 */
interface RericsJob extends Promise<void> {
  /** 進行中の操作を中止する。 */
  cancel(): void;
}

/** `rerics.fs.stat()` が返すメタデータ。 */
interface RericsFsStat {
  /** ディレクトリなら true。 */
  readonly isDir: boolean;
  /** 通常ファイルなら true。 */
  readonly isFile: boolean;
  /** バイト単位のサイズ（ディレクトリは 0）。 */
  readonly size: number;
  /** 最終更新時刻（Unix epoch ミリ秒）。`new Date(mtime)` で扱える。取得不可なら 0。 */
  readonly mtime: number;
  /** 読み取り専用属性。 */
  readonly readonly: boolean;
  /** 隠し属性。 */
  readonly hidden: boolean;
}

/**
 * 裏で動く低レベルファイル操作（`rerics.fs`）。`rerics.copy()/move()/delete()` と違い、
 * 画面にもログにも触れず、確認も進捗も出さずに同期で実行する。表示を更新したいときは
 * 呼び手が `rerics.navigate()` などで明示する。
 *
 * - パスは**絶対パス**で渡す（相対パスは解決しない）。
 * - I/O 失敗は例外（`try/catch` で拾える）。`exists` は投げず真偽を返し、`stat` は不在で null。
 * - テキストは `readText`/`writeText`（UTF-8）。バイナリ対応は将来 `readBytes`/`writeBytes` を足す。
 */
interface RericsFs {
  /** テキストファイルを UTF-8 で読む。不正なバイト列・読込失敗は例外。 */
  readText(path: string): string;
  /** テキストを UTF-8 で書く（新規作成／既存は上書き）。 */
  writeText(path: string, content: string): void;
  /** `src` の中身を `dst` へコピーする（上書き）。 */
  copyFile(src: string, dst: string): void;
  /** `src` を `dst` へ名前変更／移動する。 */
  rename(src: string, dst: string): void;
  /** ディレクトリを作る（途中の階層も再帰作成・既存はそのまま成功）。 */
  mkdir(path: string): void;
  /** `target` を指す NTFS シンボリックリンクを `link` に作る（ファイル/ディレクトリは自動判別・要特権か開発者モード）。 */
  symlink(target: string, link: string): void;
  /** `target` ディレクトリを指す NTFS ジャンクションを `link` に作る（ディレクトリ専用・特権不要）。 */
  junction(target: string, link: string): void;
  /** 存在すれば true（エラーでも false 寄せ）。 */
  exists(path: string): boolean;
  /** ファイル／空ディレクトリを削除する（非再帰・中身ありディレクトリは例外）。 */
  remove(path: string): void;
  /** メタデータを返す。存在しなければ null・他の I/O エラーは例外。 */
  stat(path: string): RericsFsStat | null;
}

/**
 * 文字列ユーティリティ（`rerics.str`）。JS 標準で足りる操作（大小変換・trim・正規表現など）は
 * 持たず、JS 標準では難しい全角半角・かなの相互変換だけを提供する。VB の `StrConv` 相当
 * （内部は Win32 `LCMapStringEx`）。
 */
interface RericsStr {
  /** 全角を半角へ（ASCII・カタカナ）。 */
  toNarrow(text: string): string;
  /** 半角を全角へ（ASCII・カタカナ）。 */
  toWide(text: string): string;
  /** ひらがなをカタカナへ。 */
  toKatakana(text: string): string;
  /** カタカナをひらがなへ。 */
  toHiragana(text: string): string;
}

/**
 * 環境情報（`rerics.env`）。特殊フォルダ・システム情報・環境変数を読む。取得できない値は空文字。
 */
interface RericsEnv {
  /** ドキュメントフォルダ。 */
  documents(): string;
  /** デスクトップフォルダ。 */
  desktop(): string;
  /** Program Files フォルダ。 */
  programFiles(): string;
  /** スタートメニューフォルダ。 */
  startMenu(): string;
  /** スタートメニューのプログラムフォルダ。 */
  programs(): string;
  /** システムフォルダ（System32）。 */
  system(): string;
  /** 一時ディレクトリ。 */
  tempPath(): string;
  /** 実行ファイルのあるディレクトリ。 */
  applicationPath(): string;
  /** 起動コマンドライン（文字列）。 */
  commandLine(): string;
  /** 起動引数の配列（先頭は実行ファイル）。 */
  commandLineArgs(): string[];
  /** ログオンユーザ名。 */
  userName(): string;
  /** ドメイン名。 */
  domainName(): string;
  /** コンピュータ名。 */
  machineName(): string;
  /** 環境変数の値（未設定は空文字）。 */
  get(name: string): string;
}

/**
 * クリップボードのテキスト・画像の読み書き（`rerics.clipboard`）。テキストは Windows 標準の
 * CF_UNICODETEXT、画像は CF_DIB でやり取りする。
 */
interface RericsClipboard {
  /** クリップボードへテキストを設定する。 */
  setText(text: string): void;
  /** クリップボードのテキストを返す（テキストが無ければ空文字）。 */
  getText(): string;
  /** 画像ファイルを読み込み、クリップボードへ画像（CF_DIB）として設定する。成功で true。透過は失われる。 */
  setImage(path: string): boolean;
  /** クリップボードの画像を dest（拡張子で形式を決める）へ保存する。画像があり保存できたら true。 */
  getImage(dest: string): boolean;
}

/** `rerics.spawn()` / `rerics.run()` の末尾に渡せる起動オプション。 */
interface RericsProcOptions {
  /** 作業ディレクトリ（省略時はプロセス既定）。 */
  cwd?: string;
}

/** いま押されている修飾キーの状態（`rerics.modifiers()`）。原作 `Filer.Shift`/`Control`/`Alt` 相当。 */
interface RericsModifiers {
  /** Shift が押されているか。 */
  readonly shift: boolean;
  /** Ctrl が押されているか。 */
  readonly ctrl: boolean;
  /** Alt が押されているか。 */
  readonly alt: boolean;
}

/** `rerics.run()` が返す外部プロセスの結果。 */
interface RericsProcessResult {
  /** 終了コード（シグナル等でコード無しに終わった場合は null）。 */
  readonly code: number | null;
  /** 標準出力（UTF-8 として読んだ文字列）。 */
  readonly stdout: string;
  /** 標準エラー出力（UTF-8 として読んだ文字列）。 */
  readonly stderr: string;
}

/** 組込コマンドを `r.<名前>()` で呼んだときの戻り値（値返しクエリは値・アクションは null）。 */
type CommandResult = string | number | boolean | null;

/**
 * ログ行のレベル（表示色）。`RericsLogLine.update` の第2引数で使う。
 */
type RericsLogLevel = "normal" | "info" | "warning" | "error";

/**
 * `log` / `info` / `warning` / `error` が返すログ行のハンドル。普段は受け取らずに捨ててよい。
 * 受け取って `update` を呼ぶと、その行を**インプレースで書き換える**（追記ではない）。進捗の
 * 1 行更新などに使う。反映はタイマ駆動で、連続更新でも描画が詰まらない。
 *
 * `startProgress` でこの行に「ぐるぐる（＋任意で百分率）」の生存表示を付けられる。データ更新が
 * 止まっている間も回り続けるので、`unpack` の大きいファイル復号など無音の待ちが違和感なく見える。
 *
 * ```ts
 * const line = r.log("展開中…");
 * line.startProgress();
 * await r.unpack(src, dst, {
 *   onProgress: (p) => {
 *     if (p.total) line.setProgress(p.done, p.total);
 *     line.update("展開中: " + p.text);
 *   },
 * });
 * line.stopProgress();
 * r.info("展開終了"); // 終了は別の行として出す
 * ```
 */
interface RericsLogLine {
  /** この行の本文を書き換える。`level` を渡すと表示色（レベル）も差し替える。 */
  update(text: string, level?: RericsLogLevel): void;

  /** この行で進行表示（ぐるぐる）を始める。`stopProgress` まで回り続ける。 */
  startProgress(): void;

  /** 進行表示中の行へ進捗比を与える（`total>0` のとき百分率を出す）。 */
  setProgress(done: number, total: number): void;

  /** 進行表示を止める（ぐるぐる・百分率を消し、本文だけ残す）。 */
  stopProgress(): void;
}

/**
 * Rerics 本体が提供するホスト API（グローバル `rerics`／短縮 `r`）。
 */
interface RericsApi {
  /** アプリのログ欄へ通常レベルで出し、その行のハンドルを返す（`RericsLogLine`）。 */
  log(message: string): RericsLogLine;

  /** ログ欄へ情報レベルで出す（太字）。行のハンドルを返す。 */
  info(message: string): RericsLogLine;

  /** ログ欄へ警告レベルで出す。行のハンドルを返す。 */
  warning(message: string): RericsLogLine;

  /** ログ欄へエラーレベルで出す（太字）。行のハンドルを返す。 */
  error(message: string): RericsLogLine;

  /** ログ欄の全文を返す（行は `\r\n` 区切り・末尾にも改行）。 */
  getLog(): string;

  /** アプリのバージョン文字列（`1.0.123` 形式。patch はビルド番号）を返す。 */
  version(): string;

  /**
   * 設定値をドット区切りキーで読む（読取専用）。キーは `config.toml` の構造に対応する
   * （例：`r.config("editor")`・`r.config("layout.border_unit")`・`r.config("cursor.to_parent")`）。
   * 値はそのまま（文字列・数値・真偽・配列・オブジェクト）返り、未知キーは null。
   */
  config(key: string): unknown;

  /** アクティブペインの現在ディレクトリ（絶対パス）を返す。 */
  currentDir(): string;

  /** アクティブペインのドライブ（例 `"C:"`）。判別できなければ空文字。 */
  currentDrive(): string;

  /** アクティブペインが左ペインなら true。 */
  isLeft(): boolean;

  /** アクティブペインが右ペインなら true。 */
  isRight(): boolean;

  /** 現在のソート種別トークンを返す（例 `"fileName"`・`"lastWriteTime"`）。 */
  getSortType(): string;

  /** ソートが逆順なら true。 */
  getSortReverse(): boolean;

  /** 現在のパスマスクを返す（マスク無しは空文字）。 */
  getPathMask(): string;

  /**
   * カーソルの次の行から巡回して `name` に一致する項目へカーソルを移動し、見つかれば中央へ
   * 寄せて true を返す（現在行は対象外・大小無視）。`startwith` 既定 true は前方一致、false は
   * 部分一致。見つからなければカーソルは動かさず false。
   */
  incrementalSearch(name: string, startwith?: boolean): boolean;

  /** アクティブペインを `path` へ移動する。 */
  navigate(path: string): void;

  /** 反対ペインを `path` へ移動する（失敗時はログのみ）。 */
  changeOppositeDirectory(path: string): void;

  /** 反対ペインを親ディレクトリへ移動する。 */
  changeOppositeDirectoryToParent(): void;

  /** 反対ペインをドライブのルートへ移動する（書庫内では効かない）。 */
  changeOppositeDirectoryToRoot(): void;

  /**
   * カンマ区切りの各マスク（VB Like：`*` `?` `#` `[...]` `[!...]`）に一致する項目だけを
   * 選択し直す（既存選択はクリアしてから付け直す）。大小無視・`".."` は対象外。1 件でも
   * 一致すれば true。UI ありの版は `selectMaskDialog`。
   */
  selectMask(mask: string): boolean;

  /**
   * アクティブペインの表示マスク（パスマスク）を設定して一覧を更新する。空文字または `"*"`
   * で解除。UI ありの版は `pathMaskDialog`。
   */
  pathMask(mask: string): void;

  /**
   * ディレクトリを作る（相対名はアクティブペインの現在地基準）。作成した絶対パスを返す。
   * 失敗すると例外（`try/catch` で拾える）。UI ありの版は `makeDirectoryDialog`。
   */
  makeDirectory(name: string): string;

  /**
   * 対象名の配列 `files`（相対はアクティブペインの現在地基準）を `archive` へ圧縮するワーカーを
   * 起動して**待たずに**戻る（進捗はログに出る）。対応形式は `type` = `"zip"` のみ。起動前の
   * 検証失敗（未対応形式・対象なし・書庫内）は例外。UI ありの版は `compressDialog`。
   */
  compress(type: string, archive: string, files: string[]): void;

  /**
   * アクティブペインの各項目を、反対ペインで**同名（大小無視）かつ同じディレクトリ種別**の
   * 項目と突き合わせ、比較種別 `type` に合う項目だけを選択し直す（既存の選択はクリア）。選択
   * した件数を返す。`..` は対象外。種別はアクティブ側から見た関係（`"newer"` ＝アクティブ側が
   * 新しい等）。日付系（`sameDate`/`diffDate`/`newer`/`older`）はディレクトリを対象外とする。
   * UI ありの版は `compareDialog`。
   *
   * - `"name"`：同名（種別だけ一致）
   * - `"sameDate"` / `"diffDate"`：更新日時が一致 / 不一致
   * - `"newer"` / `"older"`：更新日時が新しい / 古い
   * - `"sameSize"` / `"diffSize"`：サイズが一致 / 不一致
   * - `"smaller"` / `"larger"`：サイズが小さい / 大きい
   * - `"notExists"`：反対ペインに同名（同種別）が無い
   */
  compare(
    type:
      | "name"
      | "sameDate"
      | "diffDate"
      | "newer"
      | "older"
      | "sameSize"
      | "diffSize"
      | "smaller"
      | "larger"
      | "notExists",
  ): number;

  /**
   * いま押されている修飾キー（Shift/Ctrl/Alt）の状態を返す。キーに割り当てたスクリプト
   * コマンドの中で、押下中の修飾で動作を分けたいときに使う（呼んだ時点の物理キー状態）。
   */
  modifiers(): RericsModifiers;

  /** 確認ダイアログ（はい/いいえ）を出す。「はい」なら true。 */
  confirm(message: string): boolean;

  /**
   * 入力ダイアログを出す。OK なら入力文字列、キャンセルなら null。
   * `options.selectAll` が真なら初期テキストを全選択して開く（すぐ上書き入力できる）。
   */
  prompt(
    message: string,
    defaultValue?: string,
    options?: { selectAll?: boolean },
  ): string | null;

  /** 一覧から 1 つ選ばせる。選んだ行の index、キャンセルなら null。 */
  select(title: string, items: string[]): number | null;

  /** パスを OS の関連付けで開く（ファイル・フォルダ・URL）。起動を待たない。 */
  open(path: string): void;

  /** フォルダ選択ダイアログを出す。選んだパス、キャンセルなら null。 */
  folderDialog(title?: string): string | null;

  /** ファイルを開くダイアログを出す。選んだパス、キャンセルなら null。 */
  openDialog(title?: string): string | null;

  /** ファイル保存ダイアログを出す。選んだパス、キャンセルなら null。 */
  saveDialog(title?: string): string | null;

  /**
   * アクティブペインの現在状態（現在地・項目一覧・選択・カーソル）を取得する。
   * 返るのは取得時点のスナップショットで、以後の変化は反映されない。
   */
  activePane(): RericsPane;

  /** 反対側ペインの現在状態を取得する。詳細は {@link activePane}。 */
  oppositePane(): RericsPane;

  /**
   * 内蔵コマンドを名前で実行する（アクティブペイン文脈・同期）。引数は文字列で渡す。
   * 不明なコマンド名・実行失敗は例外を投げる（`try/catch` で拾える）。
   *
   * **値返し**：状態を読むクエリ系コマンド（`cursorName` / `cursorPath` / `markedCount` /
   * `hasMarks` など）は値（文字列・数値・真偽）を返す。副作用だけのアクション系（カーソル移動・
   * コピーなど）は `null` を返す。`r.<コマンド名>()` の名前付き呼び出しでも同じ値が返る。
   *
   * ワーカーを起動する操作（コピー/移動/削除など）は「開始」まで戻り、**完了は待たない**。
   * 完了を待ちたいときは `await rerics.copy()` などの非同期版を使う。
   *
   * ```ts
   * rerics.activePane().apply((d) => {
   *   for (const it of d.items) if (it.ext === "tmp") it.selected = true;
   * });
   * rerics.command("delete");   // 選んだ .tmp を削除（開始まで・完了は待たない）
   *
   * if (r.hasMarks()) rerics.log(`${r.markedCount()} 件マーク中：${r.cursorName()}`);
   * ```
   */
  command(name: string, ...args: string[]): string | number | boolean | null;

  /**
   * `path` 直下を裏スレッドで走査して返す。重いディレクトリでも UI を止めない。
   * `await` して使う。
   */
  listDir(path: string): Promise<RericsDirEntry[]>;

  /**
   * 裏で動く低レベルファイル操作（読み書き・コピー・名前変更・mkdir・存在判定・stat・削除）。
   * 画面に触れない同期 API。詳細は {@link RericsFs}。
   *
   * ```ts
   * const cfg = rerics.activePane().dir + "\\config.json";
   * if (rerics.fs.exists(cfg)) {
   *   const data = JSON.parse(rerics.fs.readText(cfg));
   *   rerics.fs.writeText(cfg, JSON.stringify({ ...data, opened: true }));
   * }
   * ```
   */
  fs: RericsFs;

  /**
   * 文字列ユーティリティ（全角半角・かなの相互変換）。詳細は {@link RericsStr}。
   *
   * ```ts
   * rerics.str.toNarrow("ＡＢＣ１２３"); // "ABC123"
   * rerics.str.toKatakana("あいう");    // "アイウ"
   * ```
   */
  str: RericsStr;

  /**
   * 環境情報（特殊フォルダ・システム情報・環境変数）。詳細は {@link RericsEnv}。
   *
   * ```ts
   * rerics.navigate(rerics.env.desktop());
   * const home = rerics.env.get("USERPROFILE");
   * ```
   */
  env: RericsEnv;

  /**
   * クリップボードのテキスト読み書き。詳細は {@link RericsClipboard}。
   *
   * ```ts
   * rerics.clipboard.setText(rerics.activePane().dir);
   * const text = rerics.clipboard.getText();
   * ```
   */
  clipboard: RericsClipboard;

  /**
   * 外部プログラムを起動して**待たずに**戻る（投げっぱなし）。引数は文字列で渡し、末尾に
   * `{ cwd }` を付けると作業ディレクトリを指定できる。起動失敗は例外。
   *
   * ```ts
   * // 現在地で WSL ターミナルを開く
   * rerics.spawn("wt.exe", "wsl", { cwd: rerics.activePane().dir });
   * ```
   */
  spawn(cmd: string, ...args: (string | RericsProcOptions)[]): void;

  /**
   * 実行ファイル `path` を、生の引数文字列 `params` 付きで**現在のディレクトリ**で起動して
   * **待たずに**戻る（投げっぱなし）。`params` はコマンドラインの末尾へそのまま付くので
   * `/flag "値"` のような書式を保てる（個別に渡してクォートされる `spawn` とは別物）。
   * 起動失敗は例外。
   *
   * ```ts
   * rerics.execute("notepad.exe", "C:\\memo.txt");
   * ```
   */
  execute(path: string, params?: string): void;

  /**
   * 外部プログラムを起動して**終了まで待ち**、結果（終了コード・標準出力・標準エラー）を返す。
   * 引数は文字列で渡し、末尾に `{ cwd }` を付けると作業ディレクトリを指定できる。`await` して使う。
   *
   * ```ts
   * const r = await rerics.run("git", "status", "--porcelain", { cwd: rerics.activePane().dir });
   * if (r.code === 0 && r.stdout.trim()) rerics.log("変更あり");
   * ```
   */
  run(cmd: string, ...args: (string | RericsProcOptions)[]): Promise<RericsProcessResult>;

  /**
   * 書庫ファイル `src` の中身を丸ごとディレクトリ `dst` 配下へ展開し、展開したファイル数を
   * 返す（UI も確認も出さない裏処理）。`dst` は無ければ作る。`..` を含む細工エントリは弾く
   * （zip-slip 対策）。対応形式は本体と同じ（zip / 7z / tar 系 / 単体圧縮 など）。`await` して使う。
   *
   * 既定では何もログを出さない。`options.onProgress` を渡すと、エントリを 1 つ取り出すごとに
   * 進捗（`text`＝書庫内のエントリ名・`done`/`total`＝件数）が来るので、出したいログは自分で出せる
   * （copy/move と同じ形）。1 行を `update` で書き換えつつ `startProgress` で待ちを見せるのが定石。
   *
   * 選択した項目を画面付きで取り出したいときは内蔵コマンド `extract` を使う。
   *
   * ```ts
   * const line = rerics.log("展開中…");
   * line.startProgress();
   * const n = await rerics.unpack("C:\\dl\\pkg.zip", rerics.activePane().dir + "\\pkg", {
   *   onProgress: (p) => {
   *     if (p.total) line.setProgress(p.done, p.total);
   *     line.update("展開中: " + p.text);
   *   },
   * });
   * line.stopProgress();
   * rerics.info(`${n} 件展開した`);
   * rerics.navigate(rerics.activePane().dir); // 表示を更新したいなら明示的に
   * ```
   */
  unpack(src: string, dst: string, options?: RericsOpOptions): Promise<number>;

  /**
   * 関数 `fn` を別スレッド＋別 V8 アイソレートで本当に並列に実行し、戻り値を `await` で受け取る。
   * CPU を使う重い処理（ハッシュ計算・大量の文字列処理など）を UI もメインのスクリプト実行も
   * 止めずに走らせられる。`Promise.all` で複数を同時に投げれば、CPU のコア数まで本当に並列に動く
   * （それを超える分は枠が空くまで待つ）。
   *
   * `fn` は別アイソレートへソースとして渡るため、**外側で捕捉した変数・関数は見えない**。必要な値は
   * `arg` で渡すこと（`worker_threads` と同じ制約）。`arg` と戻り値は JSON で受け渡すので、関数・
   * `undefined`・循環参照などシリアライズできない値は失われる。`fn` の中では `rerics.log` などの
   * ホスト API は使えるが、`rerics.parallel` の入れ子（ワーカーからさらにワーカー）はできない。
   *
   * ```ts
   * // 1 件を重い処理に回す（関数の中だけで完結させる）。
   * const total = await rerics.parallel((n) => {
   *   let acc = 0;
   *   for (let i = 0; i < n; i++) acc += i;
   *   return acc;
   * }, 1_000_000);
   * // 複数をコア数まで本当に並列に。
   * const squares = await Promise.all(
   *   [1, 2, 3, 4].map((n) => rerics.parallel((x) => x * x, n)),
   * );
   * ```
   */
  parallel<R = unknown, A = unknown>(
    fn: ((arg: A) => R | Promise<R>) | string,
    arg?: A,
  ): Promise<R>;

  /**
   * 名前付きコマンドを登録する。`handler` は同期でも `async`（Promise を返す）でもよい。
   * 同名で再登録すると後勝ちで上書きする。`options` で設定 UI・補完に出すメタ情報を添えられる
   * （`label`＝機能名・`genre`＝機能順の見出しグループ・`summary`＝補完やヘルプに出す 1 行説明）。
   */
  registerCommand(
    name: string,
    handler: (...args: unknown[]) => unknown,
    options?: { label?: string; genre?: string; summary?: string },
  ): void;

  /**
   * 名前付きメニューを登録する。機能欄の式 `menu("名前")` やキー割り当てから開ける。
   * 各項目は `label`（表示名）と `command`（機能欄と同じ式）。`separator: true` の項目は
   * 区切り線になる。同名で再登録すると後勝ちで上書きする。
   */
  registerMenu(
    name: string,
    items: { label?: string; command?: string; separator?: boolean }[],
  ): void;

  /**
   * ファイラー本体のイベントにハンドラを登録する。同じイベントに複数登録でき、登録順に
   * 呼ばれる。ハンドラは同期でも `async` でもよい。引数はイベントごとに異なる（{@link RericsEvent}）。
   *
   * 注意：`changeDirectory` ハンドラの中で無条件に移動すると無限ループになりうる。条件を付けること。
   *
   * ```ts
   * rerics.on("changeDirectory", (dir) => {
   *   if (dir.endsWith("photos")) rerics.command("sortByDate");
   * });
   * ```
   */
  on(event: RericsEvent, handler: (arg: string) => void | Promise<void>): void;

  /**
   * 項目を反対ペイン（または `dest`）へコピーする。ワーカーで実行し、**完了まで待てる**
   * job（`Promise` ＋ `cancel()`）を返す。失敗・中止は例外。
   *
   * - 引数なし＝アクティブペインの選択（無ければカーソル）→ 反対ペイン。
   * - `copy(items, dest)`＝`items`（フルパス配列。`item.fullName` を使う）→ `dest` ディレクトリ。
   *   `items` が複数ディレクトリにまたがってもよい（job は全完了で resolve）。
   * - 末尾に `{ onProgress }` を渡すと進捗を受け取れる（`copy(options)` / `copy(items, dest, options)`）。
   *
   * ```ts
   * // 選択ベース（進捗つき）
   * rerics.activePane().apply((d) => {
   *   for (const it of d.items) if (it.ext === "txt") it.selected = true;
   * });
   * await rerics.copy({ onProgress: (p) => rerics.log(p.text) });
   *
   * // 明示ベース
   * const p = rerics.activePane();
   * const items = p.items.filter((it) => !it.isDir).map((it) => it.fullName);
   * await rerics.copy(items, rerics.oppositePane().dir);
   * ```
   */
  copy(options?: RericsOpOptions): RericsJob;
  copy(items: string[], dest: string, options?: RericsOpOptions): RericsJob;

  /** コピーと同じだが移動（成功後に元を削除）。詳細は {@link copy}。 */
  move(options?: RericsOpOptions): RericsJob;
  move(items: string[], dest: string, options?: RericsOpOptions): RericsJob;

  /**
   * 項目を削除する。引数なし＝アクティブペインの選択（無ければカーソル）、`delete(items)`＝
   * `items`（フルパス配列）。末尾に `{ onProgress }` を渡せる。詳細は {@link copy}。
   */
  delete(options?: RericsOpOptions): RericsJob;
  delete(items: string[], options?: RericsOpOptions): RericsJob;

  /**
   * `(旧フルパス, 新フルパス)` の組を順に改名する（同期・実 FS のみ）。同名衝突は
   * 衝突ダイアログ（上書き/強制上書き/別名/スキップ/新しい方・「全部に適用」付き）で
   * 解決する。結果の件数サマリを返す。
   */
  renameFiles(pairs: { from: string; to: string }[]): {
    ok: number;
    skip: number;
    err: number;
    cancelled: boolean;
  };
}

/**
 * 組込コマンド（`cursorDown` など）を `r.<名前>()` で呼ぶための宣言。中身は起動時に
 * `rerics.commands.d.ts`（`Command::ALL` から自動生成）が宣言マージで埋める。手書きしない。
 */
interface RericsCommands {}

/** ホスト API ＋ 組込コマンド。グローバル `rerics` から呼ぶ。 */
declare const rerics: RericsApi & RericsCommands;

/**
 * `rerics` の短縮別名。`rerics.foo()` と `r.foo()` は同じものを指す（実行時に
 * `globalThis.r = globalThis.rerics` で結ばれる）。設定欄やスクリプトでは `r.` が短くて使いやすい。
 * 登録したスクリプトコマンドも `r.<名前>()` で呼べる（型としては index シグネチャで許容）。
 */
declare const r: typeof rerics & { [command: string]: (...args: unknown[]) => unknown };
