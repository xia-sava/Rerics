//! TS/JS スクリプト基盤（V8 埋め込み）。
//!
//! スクリプトは別スレッドの V8 アイソレートで動き、`globalThis.rerics` 経由でホスト API を
//! 呼ぶ。GUI に触る操作は [`HostApi`] を介してUIスレッドへマーシャルする（実装は GUI 側）。
//! テストはモックの [`HostApi`] で同期的に検証する。

use std::rc::Rc;

use deno_core::{JsRuntime, OpState, RuntimeOptions, extension, op2};

/// スクリプトからのホスト操作を受ける窓口。実 GUI 実装は UI スレッドへマーシャルし、
/// テストはモックで記録する。`&self` で受けるのは V8 アイソレートと同一スレッドから
/// 同期的に呼ばれるため。
pub trait HostApi {
    /// アプリのログ欄（実装依存）にメッセージを出す。
    fn log(&self, msg: &str);
    /// アクティブペインの現在ディレクトリ（絶対パス）を返す。
    fn current_dir(&self) -> String;
    /// アクティブペインを `path` へ移動する。
    fn navigate(&self, path: &str);
    /// 確認ダイアログ（Yes/No）を出し、Yes なら true。
    fn confirm(&self, message: &str) -> bool;
    /// 入力ダイアログを出し、OK なら入力文字列・キャンセルなら None。
    fn prompt(&self, message: &str, default: &str) -> Option<String>;
    /// 一覧から 1 つ選ばせ、選んだ行の index・キャンセルなら None。
    fn select(&self, title: &str, items: &[String]) -> Option<usize>;
    /// ペイン（`opposite=false` でアクティブ・`true` で反対側）の現在状態を一括取得する。
    /// 別スレッド往復を 1 回で済ませるため、項目一覧ごとスナップショットで返す。
    fn pane_snapshot(&self, opposite: bool) -> PaneSnapshot;
    /// `is_left` 側ペインの `index` 行の選択状態を `selected` にする（即時・1 行）。
    fn set_selected(&self, is_left: bool, index: usize, selected: bool);
    /// `is_left` 側ペインの複数行の選択状態をまとめて適用する（再描画は 1 回）。
    fn apply_selection(&self, is_left: bool, changes: &[(usize, bool)]);
    /// 内蔵コマンドを名前で実行する（同期）。不明な名前・実行失敗はエラー文字列を返す。
    /// ワーカーを起動する操作は「開始」までで戻り、完了は待たない。
    fn command(&self, name: &str, args: &[String]) -> Result<(), String>;
}

/// ペイン 1 つぶんの状態スナップショット（スクリプトへ渡す）。JS では camelCase で見える。
/// 項目アクセスのたびにスレッド往復しないよう、一覧を丸ごと 1 回で持って行く。
#[derive(serde::Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaneSnapshot {
    /// 現在地の表示パス（書庫内なら "C:\foo.zip\inner" 形式）。
    pub dir: String,
    /// 書庫内にいるか。
    pub is_archive: bool,
    /// 左ペインか（書き戻しを active/opposite ではなく具体側で指すための内部用）。
    pub is_left: bool,
    /// カーソル行の index（`items` の添字）。
    pub cursor: usize,
    /// 表示順の項目一覧（".." を含む）。
    pub items: Vec<PaneItem>,
}

/// ペイン内の 1 項目（スクリプトへ渡す）。コア `FileItem` を素直に写したもの。
#[derive(serde::Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaneItem {
    /// `items` 内での添字（将来の書き戻しで行を指す）。
    pub index: usize,
    /// 表示名（拡張子込み）。
    pub name: String,
    /// 拡張子を除いた名前。
    pub base_name: String,
    /// 拡張子（ドット無し・無ければ空）。
    pub ext: String,
    pub is_dir: bool,
    /// 親（".."）エントリか。
    pub is_parent: bool,
    /// バイトサイズ（ディレクトリ・取得不可は 0）。
    pub size: u64,
    /// 最終更新時刻（Unix epoch ミリ秒・取得不可は 0）。
    pub mtime: u64,
    /// 選択（マーク）されているか。
    pub selected: bool,
    pub readonly: bool,
    pub hidden: bool,
}

type Host = Rc<dyn HostApi>;

#[op2(fast)]
fn op_log(state: &mut OpState, #[string] msg: &str) {
    state.borrow::<Host>().log(msg);
}

#[op2]
#[string]
fn op_current_dir(state: &mut OpState) -> String {
    state.borrow::<Host>().current_dir()
}

#[op2(fast)]
fn op_navigate(state: &mut OpState, #[string] path: &str) {
    state.borrow::<Host>().navigate(path);
}

#[op2(fast)]
fn op_confirm(state: &mut OpState, #[string] message: &str) -> bool {
    state.borrow::<Host>().confirm(message)
}

/// 入力結果を `Vec` で包んで返す（`[]`＝キャンセル・`[s]`＝入力文字列）。op2 の同期 op は
/// `Option` を直に返せないため、JS 側で長さ 0/1 を `null`/値へ畳む。
#[op2]
#[serde]
fn op_prompt(state: &mut OpState, #[string] message: &str, #[string] default: &str) -> Vec<String> {
    state
        .borrow::<Host>()
        .prompt(message, default)
        .into_iter()
        .collect()
}

/// 選択結果を `Vec` で包んで返す（`[]`＝キャンセル・`[i]`＝選択 index）。理由は [`op_prompt`] と同じ。
#[op2]
#[serde]
fn op_select(
    state: &mut OpState,
    #[string] title: &str,
    #[serde] items: Vec<String>,
) -> Vec<u32> {
    state
        .borrow::<Host>()
        .select(title, &items)
        .map(|i| vec![i as u32])
        .unwrap_or_default()
}

/// ペイン状態のスナップショットを取得する同期 op（`opposite` で反対ペイン）。
#[op2]
#[serde]
fn op_pane_snapshot(state: &mut OpState, opposite: bool) -> PaneSnapshot {
    state.borrow::<Host>().pane_snapshot(opposite)
}

/// 1 行の選択状態を即時に書き戻す同期 op。
#[op2(fast)]
fn op_set_selected(state: &mut OpState, is_left: bool, index: u32, selected: bool) {
    state
        .borrow::<Host>()
        .set_selected(is_left, index as usize, selected);
}

/// 複数行の選択状態をまとめて書き戻す同期 op（`changes` は `[index, selected]` の配列）。
#[op2]
fn op_apply_selection(
    state: &mut OpState,
    is_left: bool,
    #[serde] changes: Vec<(u32, bool)>,
) {
    let changes: Vec<(usize, bool)> = changes.into_iter().map(|(i, v)| (i as usize, v)).collect();
    state.borrow::<Host>().apply_selection(is_left, &changes);
}

/// 内蔵コマンドを名前で実行する同期 op。不明な名前・実行失敗は JS の例外になる。
#[op2]
fn op_command(
    state: &mut OpState,
    #[string] name: &str,
    #[serde] args: Vec<String>,
) -> Result<(), deno_error::JsErrorBox> {
    state
        .borrow::<Host>()
        .command(name, &args)
        .map_err(deno_error::JsErrorBox::generic)
}

/// ディレクトリ一覧の1エントリ（スクリプトへ渡す情報）。JS 側では camelCase で見える。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DirEntry {
    name: String,
    is_dir: bool,
    size: u64,
    /// 最終更新時刻（Unix epoch ミリ秒）。JS では `new Date(mtime)` で扱える。取得不可なら 0。
    mtime: u64,
}

/// `SystemTime` を Unix epoch ミリ秒へ。取得不可・1970 より前は 0。
fn epoch_millis(t: std::io::Result<std::time::SystemTime>) -> u64 {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `path` 直下を読み、各エントリの名前・ディレクトリ種別・サイズ・更新時刻を集める（同期・純 FS）。
fn read_dir_entries(path: &str) -> std::io::Result<Vec<DirEntry>> {
    let mut out = Vec::new();
    for ent in std::fs::read_dir(path)? {
        let ent = ent?;
        let md = ent.metadata()?;
        out.push(DirEntry {
            name: ent.file_name().to_string_lossy().into_owned(),
            is_dir: md.is_dir(),
            size: md.len(),
            mtime: epoch_millis(md.modified()),
        });
    }
    Ok(out)
}

/// 重い走査を裏のブロッキングプールに逃がす非同期 op。GUI には触れない純粋処理なので
/// ホストを介さずそのまま `spawn_blocking` でき、スクリプトからは `await` するだけ。
#[op2(async(lazy), nofast)]
#[serde]
async fn op_list_dir(#[string] path: String) -> Result<Vec<DirEntry>, deno_error::JsErrorBox> {
    tokio::task::spawn_blocking(move || read_dir_entries(&path))
        .await
        .map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))?
        .map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))
}

extension!(
    rerics_ext,
    ops = [
        op_log,
        op_current_dir,
        op_navigate,
        op_confirm,
        op_prompt,
        op_select,
        op_pane_snapshot,
        op_set_selected,
        op_apply_selection,
        op_command,
        op_list_dir
    ]
);

/// `globalThis.rerics` を ops から組み立てるブートストラップ。スクリプト本体の前に1回走らせる。
/// 登録コマンドのコールバックは JS 側の Map に保持し、Rust からは名前で `__invokeCommand` を呼ぶ
/// （Rust は名前を持たず、一覧は `__commandNames` で JS Map から都度取る＝Map が唯一の真実）。
/// 末尾で `Deno` を遮蔽し、スクリプトから内部 op を直叩きできないようにする。
const BOOTSTRAP: &str = r#"
(() => {
  const ops = Deno.core.ops;
  const commands = new Map();
  const eventHandlers = new Map();
  // スナップショットから 1 ペインを組む。`sink(index, selected)` は item.selected を
  // 書いたときの送り先で、即時版は op を直に撃ち、apply() の draft 版は配列へ溜める。
  const buildPane = (snap, sink) => {
    const items = snap.items.map((raw) => {
      let sel = raw.selected;
      const it = {
        index: raw.index,
        name: raw.name,
        baseName: raw.baseName,
        ext: raw.ext,
        isDir: raw.isDir,
        isParent: raw.isParent,
        size: raw.size,
        mtime: raw.mtime,
        readonly: raw.readonly,
        hidden: raw.hidden,
      };
      Object.defineProperty(it, "selected", {
        enumerable: true,
        get: () => sel,
        set: (v) => {
          sel = !!v;
          sink(raw.index, sel);
        },
      });
      return it;
    });
    const pane = {
      dir: snap.dir,
      isArchive: snap.isArchive,
      cursor: snap.cursor,
      items,
      get selectedItems() {
        return items.filter((it) => it.selected);
      },
      get cursorItem() {
        return items[snap.cursor] ?? null;
      },
      [Symbol.iterator]() {
        return items[Symbol.iterator]();
      },
      // 即時反映しない draft を渡し、コールバック内の選択変更を 1 往復でまとめて適用する。
      apply(fn) {
        const changes = [];
        const draft = buildPane(snap, (idx, v) => {
          changes.push([idx, v]);
        });
        fn(draft);
        if (changes.length) ops.op_apply_selection(snap.isLeft, changes);
        return pane;
      },
    };
    return pane;
  };
  // 即時版：item.selected への代入がその場で UI に反映される。
  const makePane = (snap) =>
    buildPane(snap, (idx, v) => ops.op_set_selected(snap.isLeft, idx, v));
  globalThis.rerics = {
    log: (m) => ops.op_log(String(m)),
    currentDir: () => ops.op_current_dir(),
    navigate: (p) => ops.op_navigate(String(p)),
    confirm: (m) => ops.op_confirm(String(m)),
    prompt: (m, d) => {
      const r = ops.op_prompt(String(m), d == null ? "" : String(d));
      return r.length ? r[0] : null;
    },
    select: (t, items) => {
      const r = ops.op_select(String(t), (items || []).map(String));
      return r.length ? r[0] : null;
    },
    listDir: (p) => ops.op_list_dir(String(p)),
    activePane: () => makePane(ops.op_pane_snapshot(false)),
    oppositePane: () => makePane(ops.op_pane_snapshot(true)),
    command: (name, ...args) => ops.op_command(String(name), args.map(String)),
    registerCommand: (name, fn) => {
      if (typeof fn !== "function") throw new TypeError("registerCommand: fn must be a function");
      commands.set(String(name), fn);
    },
    on: (event, fn) => {
      if (typeof fn !== "function") throw new TypeError("on: fn must be a function");
      const key = String(event);
      const list = eventHandlers.get(key);
      if (list) list.push(fn);
      else eventHandlers.set(key, [fn]);
    },
  };
  // ファイラー本体の出来事を登録ハンドラへ配る。1 つが投げても残りは続行する。
  globalThis.__fireEvent = (event, arg) => {
    const list = eventHandlers.get(String(event));
    if (!list) return;
    const report = (e) =>
      rerics.log("event error [" + event + "]: " + ((e && e.stack) || e));
    for (const fn of list) {
      try {
        const r = fn(arg);
        if (r && typeof r.then === "function") r.then(undefined, report);
      } catch (e) {
        report(e);
      }
    }
  };
  globalThis.__commandNames = () => [...commands.keys()];
  globalThis.__invokeCommand = (name) => {
    const fn = commands.get(String(name));
    if (!fn) throw new Error("unknown command: " + name);
    const report = (e) => rerics.log("command error [" + name + "]: " + ((e && e.stack) || e));
    try {
      const r = fn();
      if (r && typeof r.then === "function") return r.then(undefined, report);
      return r;
    } catch (e) {
      report(e);
    }
  };
})();
delete globalThis.Deno;
"#;

/// TS/JS ソースを型消去して実行可能な JS にする。`.ts` の型注釈・interface 等を落とす
/// （swc ベース・型検査はしない＝エディタ側の責務）。`specifier` はスタック表示用の URL。
pub fn transpile(
    specifier: &str,
    media_type: deno_ast::MediaType,
    source: String,
) -> Result<String, String> {
    let parsed = deno_ast::parse_module(deno_ast::ParseParams {
        specifier: deno_ast::ModuleSpecifier::parse(specifier).map_err(|e| e.to_string())?,
        text: source.into(),
        media_type,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|e| e.to_string())?;
    let emitted = parsed
        .transpile(
            &deno_ast::TranspileOptions::default(),
            &deno_ast::TranspileModuleOptions::default(),
            &deno_ast::EmitOptions::default(),
        )
        .map_err(|e| e.to_string())?;
    Ok(emitted.into_source().text)
}

/// 拡張子から MediaType を決める（`.ts`→TypeScript・それ以外→JavaScript）。
pub fn media_type_for(path: &std::path::Path) -> deno_ast::MediaType {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts") => deno_ast::MediaType::TypeScript,
        _ => deno_ast::MediaType::JavaScript,
    }
}

/// `dir` 直下の `.ts`/`.js` をファイル名順に読み、それぞれ型消去して実行する。スクリプトの
/// 標準的な読込口。1本が失敗しても残りは続行し、失敗した `(パス, メッセージ)` を集めて返す。
/// `dir` が存在しない場合は「スクリプト無し」として静かに空を返す。
pub fn load_dir(engine: &mut Engine, dir: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
    let mut errors = Vec::new();
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("ts") | Some("js")
                )
            })
            .collect(),
        Err(_) => return errors,
    };
    files.sort();
    for path in files {
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                let spec = format!("file:///{}", path.display().to_string().replace('\\', "/"));
                if let Err(e) = engine.run_ts("rerics:script", &spec, media_type_for(&path), src) {
                    errors.push((path, e));
                }
            }
            Err(e) => errors.push((path, e.to_string())),
        }
    }
    errors
}

/// 1スレッドぶんのスクリプト実行環境。V8 アイソレートと、非同期 op（Promise）を
/// 駆動するための current-thread tokio ランタイムを抱える。
pub struct Engine {
    runtime: JsRuntime,
    tokio_rt: tokio::runtime::Runtime,
}

impl Engine {
    /// ホスト API を結びつけた実行環境を作る。`globalThis.rerics` を即座に用意する。
    pub fn new(host: Host) -> Self {
        let tokio_rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread tokio runtime");
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![rerics_ext::init()],
            ..Default::default()
        });
        {
            let state = runtime.op_state();
            let mut state = state.borrow_mut();
            state.put::<Host>(host);
        }
        runtime
            .execute_script("rerics:bootstrap", BOOTSTRAP)
            .expect("bootstrap script must not fail");
        Self { runtime, tokio_rt }
    }

    /// 現在登録されているコマンド名（JS 側 Map のキー＝登録順・同名は後勝ちで一意）。
    pub fn registered_commands(&mut self) -> Vec<String> {
        let global = self
            .runtime
            .execute_script("rerics:list-commands", "globalThis.__commandNames()")
            .expect("__commandNames must not fail");
        deno_core::scope!(scope, &mut self.runtime);
        let local = deno_core::v8::Local::new(scope, global);
        deno_core::serde_v8::from_v8::<Vec<String>>(scope, local).unwrap_or_default()
    }

    /// 登録済みイベントハンドラを発火する（`rerics.on` で登録したもの）。`arg` は単一の
    /// 文字列ペイロード。未登録イベントは無音。ハンドラが非同期でも Promise を完了させる。
    pub fn fire_event(&mut self, event: &str, arg: &str) -> Result<(), String> {
        let e = serde_json::to_string(event).map_err(|e| e.to_string())?;
        let a = serde_json::to_string(arg).map_err(|e| e.to_string())?;
        let code = format!("globalThis.__fireEvent({e}, {a});");
        self.run_to_completion("rerics:event", code)
            .map_err(|e| e.to_string())
    }

    /// 登録済みコマンドを名前で実行する。コールバックが非同期でも Promise を完了させる。
    pub fn invoke_command(&mut self, name: &str) -> Result<(), String> {
        let literal = serde_json::to_string(name).map_err(|e| e.to_string())?;
        let code = format!("globalThis.__invokeCommand({literal});");
        self.run_to_completion("rerics:invoke", code)
            .map_err(|e| e.to_string())
    }

    /// JS ソースを実行し、イベントループが空になるまで回す（async op / Promise を完了させる）。
    pub fn run_to_completion(
        &mut self,
        name: &'static str,
        code: String,
    ) -> Result<(), deno_core::error::CoreError> {
        let Self {
            runtime, tokio_rt, ..
        } = self;
        tokio_rt.block_on(async {
            runtime.execute_script(name, code)?;
            runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default())
                .await?;
            Ok(())
        })
    }

    /// TS/JS ソースを型消去してから実行し、Promise を完了させる。スクリプト読込の標準経路。
    pub fn run_ts(
        &mut self,
        name: &'static str,
        specifier: &str,
        media_type: deno_ast::MediaType,
        source: String,
    ) -> Result<(), String> {
        let js = transpile(specifier, media_type, source)?;
        self.run_to_completion(name, js).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct MockHost {
        logs: RefCell<Vec<String>>,
        dir: String,
        navigated: RefCell<Vec<String>>,
        confirm_reply: bool,
        prompt_reply: Option<String>,
        select_reply: Option<usize>,
        active_pane: PaneSnapshot,
        opposite_pane: PaneSnapshot,
        set_selected: RefCell<Vec<(bool, usize, bool)>>,
        applied: RefCell<Vec<(bool, Vec<(usize, bool)>)>>,
        commands: RefCell<Vec<(String, Vec<String>)>>,
        /// この名前のコマンドは失敗させる（エラー経路の検証用）。
        failing_command: Option<String>,
    }

    impl HostApi for MockHost {
        fn log(&self, m: &str) {
            self.logs.borrow_mut().push(m.to_string());
        }
        fn current_dir(&self) -> String {
            self.dir.clone()
        }
        fn navigate(&self, p: &str) {
            self.navigated.borrow_mut().push(p.to_string());
        }
        fn confirm(&self, _message: &str) -> bool {
            self.confirm_reply
        }
        fn prompt(&self, _message: &str, _default: &str) -> Option<String> {
            self.prompt_reply.clone()
        }
        fn select(&self, _title: &str, _items: &[String]) -> Option<usize> {
            self.select_reply
        }
        fn pane_snapshot(&self, opposite: bool) -> PaneSnapshot {
            if opposite {
                self.opposite_pane.clone()
            } else {
                self.active_pane.clone()
            }
        }
        fn set_selected(&self, is_left: bool, index: usize, selected: bool) {
            self.set_selected.borrow_mut().push((is_left, index, selected));
        }
        fn apply_selection(&self, is_left: bool, changes: &[(usize, bool)]) {
            self.applied.borrow_mut().push((is_left, changes.to_vec()));
        }
        fn command(&self, name: &str, args: &[String]) -> Result<(), String> {
            if self.failing_command.as_deref() == Some(name) {
                return Err(format!("boom: {name}"));
            }
            self.commands.borrow_mut().push((name.to_string(), args.to_vec()));
            Ok(())
        }
    }

    /// テスト用に名前・ディレクトリ種別・選択状態だけ与えた項目を作る。
    /// `base_name`/`ext` は名前の末尾ドットから導く（ファイルのみ）。
    fn item(index: usize, name: &str, is_dir: bool, selected: bool) -> PaneItem {
        let (base_name, ext) = match name.rfind('.') {
            Some(p) if !is_dir && p > 0 => (name[..p].to_string(), name[p + 1..].to_string()),
            _ => (name.to_string(), String::new()),
        };
        PaneItem {
            index,
            name: name.to_string(),
            base_name,
            ext,
            is_dir,
            is_parent: name == "..",
            size: 0,
            mtime: 0,
            selected,
            readonly: false,
            hidden: false,
        }
    }

    #[test]
    fn script_logs_and_navigates_through_host() {
        let host = Rc::new(MockHost {
            dir: "C:\\tmp".into(),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:basic",
            r#"rerics.log("hi"); rerics.navigate(rerics.currentDir() + "\\sub");"#.to_string(),
        )
        .unwrap();
        assert_eq!(*host.logs.borrow(), vec!["hi".to_string()]);
        assert_eq!(*host.navigated.borrow(), vec!["C:\\tmp\\sub".to_string()]);
    }

    #[test]
    fn async_list_dir_runs_off_thread_and_resolves() {
        // 一時ディレクトリに 2 ファイル + 1 サブディレクトリを作る。
        let dir = std::env::temp_dir().join(format!("rerics-script-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("b.txt"), b"x").unwrap();

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        // バックスラッシュ回避のためスラッシュ表記で渡す（Windows でも read_dir は受ける）。
        let p = dir.display().to_string().replace('\\', "/");
        let code = format!(
            r#"(async () => {{
                 const entries = await rerics.listDir("{p}");
                 rerics.log("count=" + entries.length);
                 rerics.log("dirs=" + entries.filter(e => e.isDir).length);
                 rerics.log("dated=" + entries.filter(e => e.mtime > 0).length);
               }})();"#
        );
        eng.run_to_completion("test:async", code).unwrap();

        assert_eq!(
            *host.logs.borrow(),
            vec!["count=3".to_string(), "dirs=1".to_string(), "dated=3".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typescript_types_are_erased_and_runs() {
        let host = Rc::new(MockHost {
            dir: "C:\\t".into(),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        let ts = r#"
            interface Person { name: string }
            function greet(p: Person): string { return "hi " + p.name; }
            const who: string = rerics.currentDir();
            rerics.log(greet({ name: who }));
        "#;
        eng.run_ts(
            "test:ts",
            "file:///rerics/test.ts",
            deno_ast::MediaType::TypeScript,
            ts.to_string(),
        )
        .unwrap();
        assert_eq!(*host.logs.borrow(), vec!["hi C:\\t".to_string()]);
    }

    #[test]
    fn register_then_invoke_command_calls_back_into_host() {
        let host = Rc::new(MockHost {
            dir: "C:\\base".into(),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:register",
            r#"rerics.registerCommand("up", () => rerics.navigate(rerics.currentDir() + "/.."));"#
                .to_string(),
        )
        .unwrap();
        assert_eq!(eng.registered_commands(), vec!["up".to_string()]);
        assert!(host.navigated.borrow().is_empty());

        eng.invoke_command("up").unwrap();
        assert_eq!(*host.navigated.borrow(), vec!["C:\\base/..".to_string()]);

        assert!(eng.invoke_command("missing").is_err());
    }

    #[test]
    fn load_dir_runs_ts_and_js_in_name_order() {
        let dir = std::env::temp_dir().join(format!("rerics-load-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 名前順で 10→20 の順に走るはず。.ts と .js を混在させる。
        std::fs::write(
            dir.join("20-second.js"),
            r#"rerics.registerCommand("second", () => {});"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("10-first.ts"),
            r#"const n: number = 1; rerics.registerCommand("first", (): void => {});"#,
        )
        .unwrap();

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host);
        let errors = load_dir(&mut eng, &dir);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(
            eng.registered_commands(),
            vec!["first".to_string(), "second".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_dir_missing_directory_is_silent() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host);
        let errors = load_dir(&mut eng, std::path::Path::new("C:\\no\\such\\rerics-dir-xyz"));
        assert!(errors.is_empty());
        assert!(eng.registered_commands().is_empty());
    }

    #[test]
    fn modal_apis_round_trip_through_host() {
        let host = Rc::new(MockHost {
            confirm_reply: true,
            prompt_reply: Some("typed".into()),
            select_reply: Some(2),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:modal",
            r#"
              rerics.log("c=" + rerics.confirm("ok?"));
              rerics.log("p=" + rerics.prompt("name?", "def"));
              rerics.log("s=" + rerics.select("pick", ["a", "b", "c"]));
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec!["c=true".to_string(), "p=typed".to_string(), "s=2".to_string()]
        );
    }

    #[test]
    fn object_model_exposes_pane_items_and_derived_views() {
        let host = Rc::new(MockHost {
            active_pane: PaneSnapshot {
                dir: "C:\\work".into(),
                is_archive: false,
                is_left: true,
                cursor: 2,
                items: vec![
                    item(0, "..", true, false),
                    item(1, "sub", true, false),
                    item(2, "a.txt", false, true),
                    item(3, "b.txt", false, true),
                ],
            },
            opposite_pane: PaneSnapshot {
                dir: "C:\\other".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:object-model",
            r#"
              const p = rerics.activePane();
              rerics.log("dir=" + p.dir);
              rerics.log("count=" + p.items.length);
              rerics.log("sel=" + p.selectedItems.map(i => i.name).join(","));
              rerics.log("cursor=" + p.cursorItem.name);
              rerics.log("opp=" + rerics.oppositePane().dir);
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.logs.borrow(),
            vec![
                "dir=C:\\work".to_string(),
                "count=4".to_string(),
                "sel=a.txt,b.txt".to_string(),
                "cursor=a.txt".to_string(),
                "opp=C:\\other".to_string(),
            ]
        );
    }

    #[test]
    fn selection_write_back_immediate_and_batched() {
        let host = Rc::new(MockHost {
            active_pane: PaneSnapshot {
                dir: "C:\\work".into(),
                is_left: true,
                cursor: 1,
                items: vec![
                    item(0, "..", true, false),
                    item(1, "a.txt", false, false),
                    item(2, "b.dat", false, false),
                    item(3, "c.txt", false, false),
                ],
                ..Default::default()
            },
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:write-back",
            r#"
              // 即時：カーソル行を選択 → その場で set_selected(index=1)。
              rerics.activePane().cursorItem.selected = true;
              // バッチ：.txt を全部選択 → apply で 1 回 apply_selection。
              rerics.activePane().apply((d) => {
                for (const it of d.items) if (it.ext === "txt") it.selected = true;
              });
            "#
            .to_string(),
        )
        .unwrap();
        // 即時は (左=true, index=1, true) が 1 件。
        assert_eq!(*host.set_selected.borrow(), vec![(true, 1, true)]);
        // バッチは index 1 と 3（a.txt/c.txt）がまとまって 1 回。
        assert_eq!(
            *host.applied.borrow(),
            vec![(true, vec![(1, true), (3, true)])]
        );
    }

    #[test]
    fn command_invokes_host_and_throws_on_error() {
        let host = Rc::new(MockHost {
            failing_command: Some("Boom".into()),
            ..Default::default()
        });
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:command",
            r#"
              rerics.command("CursorDown");
              rerics.command("SortBy", "name", "asc");
              try { rerics.command("Boom"); rerics.log("no-throw"); }
              catch (e) { rerics.log("caught:" + e.message); }
            "#
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            *host.commands.borrow(),
            vec![
                ("CursorDown".to_string(), vec![]),
                ("SortBy".to_string(), vec!["name".to_string(), "asc".to_string()]),
            ]
        );
        // 失敗コマンドは JS の例外になり、catch でメッセージを拾える。
        assert_eq!(*host.logs.borrow(), vec!["caught:boom: Boom".to_string()]);
    }

    #[test]
    fn event_handlers_fire_with_payload_and_can_call_host() {
        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        eng.run_to_completion(
            "test:on",
            r#"
              rerics.on("changeDirectory", (dir) => rerics.log("cd1:" + dir));
              rerics.on("changeDirectory", (dir) => rerics.navigate(dir + "/x"));
              rerics.on("executeCommand", (name) => rerics.log("cmd:" + name));
            "#
            .to_string(),
        )
        .unwrap();
        // 未登録イベントは無音。
        eng.fire_event("noSuchEvent", "").unwrap();
        // changeDirectory は両ハンドラが順に走る（ログ＋ホスト呼び出し）。
        eng.fire_event("changeDirectory", "C:\\d").unwrap();
        eng.fire_event("executeCommand", "CursorDown").unwrap();

        assert_eq!(
            *host.logs.borrow(),
            vec!["cd1:C:\\d".to_string(), "cmd:CursorDown".to_string()]
        );
        assert_eq!(*host.navigated.borrow(), vec!["C:\\d/x".to_string()]);
    }

    #[test]
    fn helpers_are_shared_across_files_by_load_order() {
        // 先に読むファイルでトップレベル定義した普通の関数・定数を、後のファイルの
        // registerCommand コールバックから（invoke 時に）参照できるか＝ファイル間共有の実証。
        let dir = std::env::temp_dir().join(format!("rerics-share-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("00-lib.ts"),
            r#"function libDouble(n: number): number { return n * 2; }
               const LIB_TAG: string = "lib";"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("10-use.ts"),
            r#"rerics.registerCommand("useShared", () => {
                 rerics.log(LIB_TAG + ":" + libDouble(21));
               });"#,
        )
        .unwrap();

        let host = Rc::new(MockHost::default());
        let mut eng = Engine::new(host.clone());
        let errors = load_dir(&mut eng, &dir);
        assert!(errors.is_empty(), "errors: {errors:?}");
        eng.invoke_command("useShared").unwrap();
        assert_eq!(*host.logs.borrow(), vec!["lib:42".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
