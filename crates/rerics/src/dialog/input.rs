use std::cell::{Cell, RefCell};
use std::rc::Rc;
use winsafe::msg::{cb, lb};
use winsafe::{co, gui, prelude::*};
use super::*;

/// 原作 `PluginMessage.Input` 相当。メッセージ＋1行入力のモーダルを表示し、
/// OK なら入力文字列、キャンセル/Esc なら None を返す。初期選択は従来どおり（`AsIs`）。
pub fn input_box(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    value: &str,
    mode: InputMode,
) -> Option<String> {
    input_box_full(parent, title, message, value, mode, InputSelect::AsIs, None)
}

/// [`input_box`] に初期選択（[`InputSelect`]）指定を加えた版。改名系入力で拡張子前に
/// キャレットを置く（原作 RenameStyle）。
pub fn input_box_select(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    value: &str,
    mode: InputMode,
    select: InputSelect,
) -> Option<String> {
    input_box_full(parent, title, message, value, mode, select, None)
}

/// [`input_box`] に履歴（用途キー別の過去入力）を加えた版。入力欄を編集可能コンボにし、
/// `history`（新しい順）を候補に出す。`history` が `Some` のときのみコンボ・`None` は素の Edit。
/// 原作 MessageForm の cboInput（Key 指定時の履歴コンボ）相当。`select` はコンボでは無視する。
/// 履歴への追加・保存は呼び出し側（[`crate::MainWindow`]）が担う。
pub fn input_box_full(
    parent: &impl GuiParent,
    title: &str,
    message: &str,
    value: &str,
    mode: InputMode,
    select: InputSelect,
    history: Option<&[&str]>,
) -> Option<String> {
    let (wnd, arm) = modal_window(title, 360, 150);

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: message,
            position: gui::dpi(16, 14),
            size: gui::dpi(328, 18),
            ..Default::default()
        },
    );

    // 履歴ありかつ Plain のときだけ編集可能コンボ（候補＝履歴・新しい順）。
    // それ以外（履歴なし・パスワード）は従来どおり素の Edit。
    let combo_history = match mode {
        InputMode::Plain => history,
        InputMode::Password => None,
    };
    // 入力欄の現在値の読み取り・初期化（フォーカス＋初期値/選択）を、コンボ/Edit 共通の
    // クロージャに包んで後段から扱う。コントロール実体は `_keep` で生存させる。
    let read: Rc<dyn Fn() -> String>;
    let on_create: Box<dyn Fn()>;
    let _keep: Box<dyn std::any::Any>;
    if let Some(items) = combo_history {
        let combo = gui::ComboBox::new(
            &wnd,
            gui::ComboBoxOpts {
                control_style: co::CBS::DROPDOWN,
                position: gui::dpi(16, 38),
                width: gui::dpi_x(328),
                items,
                ..Default::default()
            },
        );
        let r = combo.clone();
        read = Rc::new(move || r.hwnd().GetWindowText().unwrap_or_default());
        let f = combo.clone();
        let v = value.to_string();
        on_create = Box::new(move || {
            let _ = f.hwnd().SetWindowText(&v);
            let _ = f.hwnd().SetFocus();
            if matches!(select, InputSelect::All) {
                let _ = unsafe {
                    f.hwnd().SendMessage(cb::SetEditSel { start_pos: Some(0), end_pos: None })
                };
            }
        });
        _keep = Box::new(combo);
    } else {
        let edit_style = match mode {
            InputMode::Plain => co::ES::AUTOHSCROLL,
            InputMode::Password => co::ES::AUTOHSCROLL | co::ES::PASSWORD,
        };
        let edit = gui::Edit::new(
            &wnd,
            gui::EditOpts {
                text: value,
                control_style: edit_style,
                position: gui::dpi(16, 38),
                width: gui::dpi_x(328),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );
        let r = edit.clone();
        read = Rc::new(move || r.text().unwrap_or_default());
        let f = edit.clone();
        let v = value.to_string();
        on_create = Box::new(move || {
            f.hwnd().SetFocus();
            select.apply(&f, &v);
        });
        _keep = Box::new(edit);
    }

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(170, 80),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let cancel = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "キャンセル",
            ctrl_id: 2,
            position: gui::dpi(258, 80),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    arm.plain(
        "input",
        title,
        message,
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
    );
    arm.on_create(move |_| {
        on_create();
        Ok(())
    });

    {
        let result = result.clone();
        let read = read.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            *result.borrow_mut() = Some(read());
            wnd2.close();
            Ok(())
        });
    }

    {
        let wnd2 = wnd.clone();
        cancel.on().bn_clicked(move || {
            wnd2.close();
            Ok(())
        });
    }

    let _ = wnd.show_modal(parent);
    let _ = cancel;

    result.borrow().clone()
}

/// 複数行のコード入力モーダル。OK ならコード文字列、キャンセル/Esc なら None。
/// キー編集の「式を編集」で、機能欄の式（組込呼び出し・スクリプト・複文コード）を書くのに使う。
/// カレットまでのテキスト `before` の末尾が `r.` / `rerics.` のメンバアクセスなら、その直後の
/// 識別子プレフィックスを返す。返り＝(プレフィックスの UTF-16 長, prefix 文字列)。`=r.cur` なら
/// `(3, "cur")`、`=r.` なら `(0, "")`、`bar.cur` のように手前が別トークンなら `None`。
/// 補完候補の絞り込み（`prefix` で前方一致）と、確定時の置換範囲（UTF-16 長）に使う。
fn completion_prefix(before: &str) -> Option<(usize, String)> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let trailing_ident = |s: &str| -> String {
        let rev: String = s.chars().rev().take_while(|c| is_ident(*c)).collect();
        rev.chars().rev().collect()
    };
    let root_is = |s: &str| matches!(s, "r" | "rerics");
    // 末尾の識別子。ASCII 識別子なのでバイト長＝文字数＝末尾位置の境界は妥当。
    let ident = trailing_ident(before);
    let head = before[..before.len() - ident.len()].strip_suffix('.')?;
    // `r.<ident>`：ドットの手前が `r`/`rerics` そのもの＝1 階層のメンバアクセス。
    let ns = trailing_ident(head);
    if root_is(&ns) {
        return Some((ident.encode_utf16().count(), ident));
    }
    // `r.<ns>.<ident>`：名前空間 ns の手前が `r`/`rerics` そのもの＝2 階層。`fs.read` のように
    // 名前空間込みの prefix を返し、候補（`fs.readText` 等）と前方一致させる。
    let head2 = head[..head.len() - ns.len()].strip_suffix('.')?;
    if ns.is_empty() || !root_is(&trailing_ident(head2)) {
        return None;
    }
    let prefix = format!("{ns}.{ident}");
    Some((prefix.encode_utf16().count(), prefix))
}

/// 補完クエリと候補の照合。マッチの強さを返す（小さいほど強い）：0=名前の前方一致、
/// 1=camelCase 頭文字一致（`cp` → `cursorPath`）、2=表示名（和名）の部分一致、
/// 3=名前の飛び石一致（クエリの文字が順に現れる）。合わなければ None。空クエリは全件 0。
/// 名前は ASCII 前提で大小無視、和名はそのまま部分一致する。
fn match_rank(query: &str, name: &str, label: Option<&str>) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let q = query.to_lowercase();
    let n = name.to_lowercase();
    if n.starts_with(&q) {
        return Some(0);
    }
    // 頭文字列＝各セグメント（ドット区切り）の先頭文字＋大文字。cursorPath → "cp"。
    let initials: String = name
        .split('.')
        .flat_map(|seg| {
            seg.chars()
                .enumerate()
                .filter(|(i, c)| *i == 0 || c.is_ascii_uppercase())
                .map(|(_, c)| c)
        })
        .collect::<String>()
        .to_lowercase();
    if initials.starts_with(&q) {
        return Some(1);
    }
    if let Some(l) = label
        && l.to_lowercase().contains(&q)
    {
        return Some(2);
    }
    // 飛び石一致は 1 文字だとほぼ全件に合ってノイズになるので、2 文字以上のクエリだけ。
    if q.chars().count() >= 2 {
        let mut rest = n.chars();
        if q.chars().all(|qc| rest.by_ref().any(|nc| nc == qc)) {
            return Some(3);
        }
    }
    None
}

/// Edit のカレット位置（UTF-16 オフセット）を返す。winsafe の `em::GetSel` はポインタ出力の
/// マーシャリングが効かない（常に 0）ので、EM_GETSEL の戻り値（上位 16bit＝選択終端＝カレット）を読む。
fn caret_offset(edit: &gui::Edit) -> u32 {
    struct GetSelRaw;
    impl winsafe::prelude::MsgSend for GetSelRaw {
        type RetType = u32;
        unsafe fn isize_to_ret(&self, v: isize) -> u32 {
            (v as u32) >> 16
        }
        fn as_generic_wm(&mut self) -> winsafe::msg::WndMsg {
            winsafe::msg::WndMsg { msg_id: co::EM::GETSEL.into(), wparam: 0, lparam: 0 }
        }
    }
    unsafe { edit.hwnd().SendMessage(GetSelRaw) }
}

/// 補完候補1件＝リストに見せる表示文字列と、確定時に入力欄へ挿入する文字列。多くは同一だが、
/// コマンドパレットのように「和名 (Token) を見せて Token を挿入」する用途で別々にできる。
/// `detail` は組込コマンドの引数シグネチャ＋説明（メタデータ由来）。あれば候補行の右に添える。
/// `selectable=false` はジャンル見出しのラベル行＝選択・確定の対象外。`caret_back` は確定挿入後に
/// カレットを何文字（UTF-16）戻すか（引数ありの `名前()` 挿入で 1＝括弧内へ置く）。
struct CompletionItem {
    display: String,
    insert: String,
    detail: Option<String>,
    selectable: bool,
    caret_back: u32,
}

impl CompletionItem {
    /// 通常の候補行（選択可・カレットは挿入末尾のまま）。
    fn plain(display: String, insert: String, detail: Option<String>) -> Self {
        Self { display, insert, detail, selectable: true, caret_back: 0 }
    }

    /// ジャンル見出しのラベル行。
    fn label(genre: &str) -> Self {
        Self {
            display: format!("── {genre} ──"),
            insert: String::new(),
            detail: None,
            selectable: false,
            caret_back: 0,
        }
    }
}

/// `code_box` の `r.` 補完に渡すメンバ 1 件。組込／host API／スクリプト関数を区別せず名前で持つ。
/// `callable`/`arity` はエンジン報告の関数判定・宣言引数数（組込コマンドはメタデータが正）。
/// `script` は登録スクリプトコマンドのメタ（表示名・ジャンル・説明。組込・host API は `None`）。
#[derive(Clone)]
pub struct CompletionMember {
    pub name: String,
    pub callable: bool,
    pub arity: u32,
    pub script: Option<crate::script::ScriptCommand>,
}

/// エンジンから取ったメンバ一覧と、名前→登録スクリプトコマンドのメタを引く関数から、
/// `code_box` に渡す補完メンバ列を組む。キー編集とメニュー編集で共用する（組込はメタデータから
/// 説明が引かれるので `script` は登録スクリプトコマンドだけに付く）。
pub fn completion_members(
    members: &[crate::script::MemberInfo],
    script: impl Fn(&str) -> Option<crate::script::ScriptCommand>,
) -> Vec<CompletionMember> {
    members
        .iter()
        .map(|m| CompletionMember {
            name: m.name.clone(),
            callable: m.callable,
            arity: m.arity,
            script: script(&m.name),
        })
        .collect()
}

/// `members` から `query` に合う補完候補列を組む。照合は [`match_rank`]（名前・和名の曖昧一致）、
/// 階層は query のドット数と同じものだけ（`r.` はトップレベルのみ・`r.fs.` は `fs.` 配下のみ）。
/// 並びはジャンルごとに固め、ジャンル見出しのラベル行を挟む（該当が 1 ジャンルだけなら挟まない）。
/// query が非空のときはジャンル自体も「群内の最強マッチ」順に並べ、目当ての機能が上に来るようにする。
/// 確定挿入は callable なら `名前()`（引数ありはカレットを括弧内へ）、オブジェクトは名前のみ。
fn completion_items(members: &[CompletionMember], query: &str) -> Vec<CompletionItem> {
    use rerics_core::Command;
    let depth = query.matches('.').count();
    struct Hit {
        genre: (u8, String),
        rank: u8,
        item: CompletionItem,
    }
    let mut hits: Vec<Hit> = Vec::new();
    for m in members {
        if m.name.matches('.').count() != depth {
            continue;
        }
        let builtin = Command::from_token(&m.name);
        let label = builtin
            .map(|c| c.display_name().to_string())
            .or_else(|| m.script.as_ref().and_then(|s| s.label.clone()));
        let Some(rank) = match_rank(query, &m.name, label.as_deref()) else {
            continue;
        };
        let genre = match (builtin, m.script.as_ref().and_then(|s| s.genre.clone())) {
            (Some(cmd), _) => {
                let (o, g) = crate::key_editor::command_genre(cmd);
                (o, g.to_string())
            }
            (None, Some(g)) => (crate::key_editor::genre_order(&g), g),
            (None, None) => (u8::MAX, "スクリプト・API".to_string()),
        };
        let detail = builtin
            .map(meta_hint)
            .or_else(|| m.script.as_ref().and_then(|s| s.summary.clone()));
        // 引数有無：host API は宣言引数（組込と同名なら host が実体なのでこちらを優先）、
        // 組込コマンドはメタデータの必須引数から（省略可のみなら引数なし扱い＝そのまま呼べる形で
        // 確定してカレットを括弧の後ろへ置く）。callable でなければ（オブジェクト）括弧を付けない。
        let has_args = m.arity > 0
            || builtin.map(|c| c.meta().args.iter().any(|a| a.required)).unwrap_or(false);
        let callable = m.callable || builtin.is_some();
        let (insert, caret_back) = if callable {
            (format!("{}()", m.name), if has_args { 1 } else { 0 })
        } else {
            (m.name.clone(), 0)
        };
        hits.push(Hit {
            genre,
            rank,
            item: CompletionItem {
                display: m.name.clone(),
                insert,
                detail,
                selectable: true,
                caret_back,
            },
        });
    }
    // 群ごとの最強マッチ（query 非空時に群の並びを決める）。空 query は全員 0＝ジャンル順のまま。
    let mut best: std::collections::HashMap<(u8, String), u8> = std::collections::HashMap::new();
    for h in &hits {
        let e = best.entry(h.genre.clone()).or_insert(u8::MAX);
        *e = (*e).min(h.rank);
    }
    // 安定ソート＝同キー内は members の名前昇順が保たれる。
    hits.sort_by_key(|h| (best[&h.genre], h.genre.0, h.genre.1.clone(), h.rank));
    let multi = best.len() > 1;
    let mut out: Vec<CompletionItem> = Vec::with_capacity(hits.len());
    let mut cur: Option<(u8, String)> = None;
    for h in hits {
        if multi && cur.as_ref() != Some(&h.genre) {
            out.push(CompletionItem::label(&h.genre.1));
            cur = Some(h.genre);
        }
        out.push(h.item);
    }
    out
}

/// 組込コマンドのメタデータから、補完候補に添える 1 行ヒント（引数シグネチャ＋説明）を作る。
fn meta_hint(cmd: rerics_core::Command) -> String {
    let m = cmd.meta();
    let sig = arg_signature(m.args);
    if sig.is_empty() { m.summary.to_string() } else { format!("{sig}  {}", m.summary) }
}

/// 引数仕様を `(path)` `(by)` `({select?})` のようなシグネチャ文字列にする。引数なしは空。
fn arg_signature(args: &[rerics_core::ArgSpec]) -> String {
    use rerics_core::ArgType;
    if args.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = args
        .iter()
        .map(|a| match a.ty {
            ArgType::Options(opts) => {
                let keys: Vec<String> = opts.iter().map(|o| format!("{}?", o.name)).collect();
                format!("{{{}}}", keys.join(", "))
            }
            _ if a.required => a.name.to_string(),
            _ => format!("{}?", a.name),
        })
        .collect();
    format!("({})", parts.join(", "))
}

/// 補完モデル。カレットまでの文字列 `before`・カレットの UTF-16 位置・`force`（Ctrl+Space の
/// 明示トリガ）から、置換開始位置（UTF-16・終端はカレット）と候補列を返す。None なら出さない。
type CompleteFn = Rc<dyn Fn(&str, u32, bool) -> Option<(u32, Vec<CompletionItem>)>>;

/// 補完つき入力欄の配線（`code_box`／`command_box` 共通）。`edit` の下に隠した `cand` を、
/// `complete` モデルの返す候補で出し入れする。候補は表示文字列で見せ、確定時は挿入文字列を
/// カレット直前の置換範囲へ入れる。キー操作（↑↓移動クランプ・Enter 確定・Ctrl+Space 表示）と
/// headless 観測（`completion_probe`）もここでまとめて仕込む。`show_modal` は呼び出し側で。
fn install_completion(arm: &ModalArm, edit: &gui::Edit, cand: &gui::ListBox, complete: CompleteFn) {
    // 置換範囲（カレット直前のプレフィックス・UTF-16）と、リストと並ぶ候補列。候補表示中だけ有効。
    let range: Rc<Cell<Option<(u32, u32)>>> = Rc::new(Cell::new(None));
    let rows: Rc<RefCell<Vec<CompletionItem>>> = Rc::new(RefCell::new(Vec::new()));

    // 候補 idx を確定＝カレット直前のプレフィックスをその挿入文字列へ置換し、リストを隠して入力へ戻す。
    // ラベル行（selectable=false）は何もしない。`caret_back` があれば挿入末尾からその分カレットを戻す
    // （`名前()` の括弧内へ置く）。
    let do_insert: Rc<dyn Fn(u32)> = {
        let edit = edit.clone();
        let cand = cand.clone();
        let range = range.clone();
        let rows = rows.clone();
        Rc::new(move |idx: u32| {
            // replace_selection の EN_CHANGE が update を呼び rows を書き換えるので、借用は先に手放す。
            let picked = {
                let rows = rows.borrow();
                match rows.get(idx as usize) {
                    Some(item) if item.selectable => Some((item.insert.clone(), item.caret_back)),
                    _ => None,
                }
            };
            if let (Some((start, end)), Some((text, caret_back))) = (range.get(), picked) {
                edit.set_selection(start as i32, end as i32);
                edit.replace_selection(&text);
                if caret_back > 0 {
                    let pos = start + text.encode_utf16().count() as u32 - caret_back;
                    edit.set_selection(pos as i32, pos as i32);
                }
                let _ = cand.hwnd().ShowWindow(co::SW::HIDE);
                range.set(None);
                edit.hwnd().SetFocus();
            }
        })
    };

    // 本文とカレット位置から `complete` を引いて候補を出し入れする。文字入力・カレット移動の両方から呼ぶ。
    type Update = Rc<dyn Fn(bool)>;
    let update: Update = {
        let edit = edit.clone();
        let cand = cand.clone();
        let range = range.clone();
        let rows = rows.clone();
        let complete = complete.clone();
        Rc::new(move |force: bool| {
            let utf16: Vec<u16> = edit.text().unwrap_or_default().encode_utf16().collect();
            let caret = (caret_offset(&edit) as usize).min(utf16.len()) as u32;
            let before = String::from_utf16_lossy(&utf16[..caret as usize]);
            match complete(&before, caret, force) {
                Some((start, items)) if !items.is_empty() => {
                    cand.items().delete_all();
                    let displays: Vec<String> = items
                        .iter()
                        .map(|i| match &i.detail {
                            // タブ区切り＝listbox のタブストップで説明の頭を桁揃えする。
                            Some(d) => format!("{}\t{}", i.display, d),
                            None => i.display.clone(),
                        })
                        .collect();
                    let _ = cand.items().add(&displays);
                    // 初期選択は先頭の選択可能行（先頭がジャンルラベルのことがある）。
                    let first = items.iter().position(|i| i.selectable).unwrap_or(0) as u32;
                    *rows.borrow_mut() = items;
                    let _ = unsafe { cand.hwnd().SendMessage(lb::SetCurSel { index: Some(first) }) };
                    let _ = cand.hwnd().ShowWindow(co::SW::SHOW);
                    range.set(Some((start, caret)));
                }
                _ => {
                    let _ = cand.hwnd().ShowWindow(co::SW::HIDE);
                    range.set(None);
                    rows.borrow_mut().clear();
                }
            }
        })
    };
    {
        let update = update.clone();
        edit.on().en_change(move || {
            update(false);
            Ok(())
        });
    }

    // 候補のシングルクリック＝その項目を確定する。
    {
        let cand2 = cand.clone();
        let do_insert = do_insert.clone();
        cand.on().lbn_sel_change(move || {
            if let Some(idx) = unsafe { cand2.hwnd().SendMessage(lb::GetCurSel {}) } {
                do_insert(idx);
            }
            Ok(())
        });
    }

    // 生成時：入力欄へフォーカスし、子コントロールのキーを横取りして補完候補を操作する。
    // ↑↓＝候補移動（ラベル行はスキップ・端でクランプ）・Enter＝確定・Ctrl+Space＝補完を開く。
    // いずれも Edit へは渡さない。
    {
        let edit_focus = edit.clone();
        let edit_h = edit.clone();
        let cand_h = cand.clone();
        let range = range.clone();
        let rows_h = rows.clone();
        let update = update.clone();
        let do_insert = do_insert.clone();
        let ctrl = Rc::new(Cell::new(false));
        let suppress_space = Rc::new(Cell::new(false));
        arm.on_create(move |hwnd| {
            edit_focus.hwnd().SetFocus();
            // 説明列の頭を固定するタブストップ（ダイアログ単位＝平均文字幅の 1/4）。名前が
            // これを超える長い候補は次の桁へ送られる（桁グリッド揃え）。
            let _ = unsafe { cand_h.hwnd().SendMessage(lb::SetTabStops { tab_stops: &[92] }) };
            let edit_h = edit_h.clone();
            let cand_h = cand_h.clone();
            let range = range.clone();
            let rows_h = rows_h.clone();
            let update = update.clone();
            let do_insert = do_insert.clone();
            let ctrl = ctrl.clone();
            let suppress_space = suppress_space.clone();
            keyhook::push(hwnd, move |msg, wparam| {
                let vk = wparam as u16;
                // Ctrl 状態の追跡（VK_CONTROL=0x11）。消費しない。
                if vk == 0x11 {
                    if msg == keyhook::WM_KEYDOWN {
                        ctrl.set(true);
                    } else if msg == keyhook::WM_KEYUP {
                        ctrl.set(false);
                    }
                    return false;
                }
                // 入力欄にフォーカスがある時だけ補完操作を扱う。
                if winsafe::HWND::GetFocus().map(|f| f.ptr()) != Some(edit_h.hwnd().ptr()) {
                    return false;
                }
                // Ctrl+Space（VK_SPACE=0x20）：補完を開く（消費）。直後の空白 WM_CHAR も抑制する。
                if msg == keyhook::WM_KEYDOWN && vk == 0x20 && ctrl.get() {
                    update(true);
                    suppress_space.set(true);
                    return true;
                }
                if msg == keyhook::WM_CHAR && wparam == 0x20 && suppress_space.get() {
                    suppress_space.set(false);
                    return true;
                }
                // カレット移動キーの KEYUP：カレットが動いたので補完を作り直す（カレット直前で判定するため）。
                // ←→ Home End は常に、↑↓ は候補リスト非表示時のみ（表示中の↑↓は候補移動なので除外）。
                if msg == keyhook::WM_KEYUP {
                    let horiz = matches!(vk, 0x25 | 0x27 | 0x24 | 0x23);
                    let vert = matches!(vk, 0x26 | 0x28);
                    if horiz || (vert && range.get().is_none()) {
                        update(false);
                    }
                    return false;
                }
                // 以降は候補リスト表示中だけ（KEYDOWN／WM_CHAR）。
                if range.get().is_none() {
                    return false;
                }
                // ↑（0x26）↓（0x28）＝1 行、PageUp（0x21）PageDown（0x22）＝1 画面ぶん、候補を
                // 移動する（ラベル行はスキップ・端でクランプ・消費）。
                if msg == keyhook::WM_KEYDOWN && matches!(vk, 0x21 | 0x22 | 0x26 | 0x28) {
                    let count = cand_h.items().count().unwrap_or(0);
                    if count == 0 {
                        return false;
                    }
                    let rows = rows_h.borrow();
                    let sel_rows: Vec<u32> = (0..count)
                        .filter(|&i| rows.get(i as usize).is_some_and(|r| r.selectable))
                        .collect();
                    if sel_rows.is_empty() {
                        return true;
                    }
                    // 1 画面の行数（PageUp/Down の移動量）＝候補欄の高さ ÷ 行高。
                    let page = {
                        let item_h =
                            unsafe { cand_h.hwnd().SendMessage(lb::GetItemHeight { index: None }) }
                                .map(|h| h.max(1))
                                .unwrap_or(1) as i32;
                        let client_h =
                            cand_h.hwnd().GetClientRect().map(|r| r.bottom - r.top).unwrap_or(0);
                        (client_h / item_h).max(1) as usize
                    };
                    let cur = unsafe { cand_h.hwnd().SendMessage(lb::GetCurSel {}) }.unwrap_or(0);
                    let pos = sel_rows.iter().position(|&i| i >= cur).unwrap_or(sel_rows.len() - 1);
                    let next = match vk {
                        0x26 => pos.saturating_sub(1),
                        0x28 => (pos + 1).min(sel_rows.len() - 1),
                        0x21 => pos.saturating_sub(page),
                        _ => (pos + page).min(sel_rows.len() - 1),
                    };
                    let _ = unsafe {
                        cand_h.hwnd().SendMessage(lb::SetCurSel { index: Some(sel_rows[next]) })
                    };
                    return true;
                }
                // Enter（WM_CHAR 0x0D）：選択中の候補を確定（消費して改行を防ぐ）。
                if msg == keyhook::WM_CHAR && wparam == 0x0D {
                    if let Some(idx) = unsafe { cand_h.hwnd().SendMessage(lb::GetCurSel {}) } {
                        do_insert(idx);
                    }
                    return true;
                }
                false
            });
            Ok(())
        });
    }

    // headless 観測：開いている補完つき入力欄の入力模擬・候補読み取り・確定・本文取得を公開する。
    #[cfg(feature = "debug-server")]
    {
        let cand_p = cand.clone();
        let cand_v = cand.clone();
        let edit_p = edit.clone();
        let edit_t = edit.clone();
        let range_v = range.clone();
        let do_insert = do_insert.clone();
        let update = update.clone();
        completion_probe::set(completion_probe::Probe {
            type_text: Box::new(move |s| {
                // 入力模擬：本文を s にしてカレットを末尾へ置き、補完を更新する。
                let _ = edit_t.set_text(s);
                let n = s.encode_utf16().count() as i32;
                edit_t.set_selection(n, n);
                update(false);
            }),
            candidates: Box::new(move || {
                let n = cand_p.items().count().unwrap_or(0);
                (0..n).filter_map(|i| cand_p.items().text(i).ok()).collect()
            }),
            visible: Box::new(move || range_v.get().is_some()),
            selected: Box::new(move || {
                unsafe { cand_v.hwnd().SendMessage(lb::GetCurSel {}) }.map_or(-1, |i| i as i32)
            }),
            accept: Box::new(move |idx| do_insert(idx)),
            text: Box::new(move || edit_p.text().unwrap_or_default()),
        });
    }
    // 配線済みの Rc は登録ハンドラ／プローブが保持するのでここで保持し続ける必要はない。
    let _ = do_insert;
}

/// 式エディタの可変寸法（DPI 換算前）を返す。`(入力欄の高さ, 候補欄の上端 y, 候補欄の高さ)`。
/// compact＝1 行入力＋広い候補欄（コマンドを探しやすい）／expanded＝複数行入力＋下に候補欄
/// （凝った処理を書きやすい）。入力欄の左上・幅は両モード共通なので持たない。生成時の初期配置に使う
/// （以後のリサイズ・モード切替は [`relayout_code_box`] が動的に計算する）。
fn code_box_geometry(expanded: bool) -> (i32, i32, i32) {
    if expanded { (150, 194, 150) } else { (24, 70, 274) }
}

/// 式エディタの子コントロールを、クライアント `cw`×`ch`（物理px）に合わせて配置し直す。リサイズ
/// （`wm_size`）とモード切替（複数行トグル）の両方から呼ぶ。compact＝入力 1 行＋候補欄が残りを占める／
/// expanded＝入力欄が伸び縮みして候補欄は下端固定。論理寸法は `gui::dpi_*` で物理へ換算する。
#[allow(clippy::too_many_arguments)]
fn relayout_code_box(
    label: &winsafe::HWND,
    mode: &winsafe::HWND,
    edit: &winsafe::HWND,
    cand: &winsafe::HWND,
    ok: &winsafe::HWND,
    cancel: &winsafe::HWND,
    cw: i32,
    ch: i32,
    expanded: bool,
) {
    use winsafe::{POINT as P, SIZE as S};
    let m = gui::dpi_x(16);
    let cbw = gui::dpi_x(92);
    let btn_gap = gui::dpi_x(8);
    let btn_h = gui::dpi_y(26);
    let gap = gui::dpi_y(12);
    let edit_top = gui::dpi_y(38);

    // 上端：左にラベル、右上にモードトグル。
    let label_w = (cw - m * 2 - cbw - btn_gap).max(1);
    let _ = label.MoveWindow(P { x: m, y: gui::dpi_y(14) }, S { cx: label_w, cy: gui::dpi_y(18) }, true);
    let _ = mode.MoveWindow(P { x: cw - m - cbw, y: gui::dpi_y(12) }, S { cx: cbw, cy: gui::dpi_y(20) }, true);

    // 下端：右にキャンセル＝最右・OK＝その左。
    let btn_y = (ch - gui::dpi_y(16) - btn_h).max(edit_top);
    let (ok_w, cancel_w) = (gui::dpi_x(80), gui::dpi_x(86));
    let cancel_x = (cw - m - cancel_w).max(0);
    let _ = cancel.MoveWindow(P { x: cancel_x, y: btn_y }, S { cx: cancel_w, cy: btn_h }, true);
    let ok_x = (cancel_x - btn_gap - ok_w).max(0);
    let _ = ok.MoveWindow(P { x: ok_x, y: btn_y }, S { cx: ok_w, cy: btn_h }, true);

    // 中央：入力欄と候補欄。下端のボタン行より上の領域を分け合う。
    let content_w = (cw - m * 2).max(1);
    let content_bottom = (btn_y - gap).max(edit_top);
    let min_pane = gui::dpi_y(40);
    let (edit_h, cand_y, cand_h) = if expanded {
        let cand_h = gui::dpi_y(150).min((content_bottom - edit_top - gap - min_pane).max(min_pane));
        let cand_y = content_bottom - cand_h;
        let edit_h = (cand_y - gap - edit_top).max(min_pane);
        (edit_h, cand_y, cand_h)
    } else {
        let edit_h = gui::dpi_y(24);
        let cand_y = edit_top + edit_h + gap;
        let cand_h = (content_bottom - cand_y).max(min_pane);
        (edit_h, cand_y, cand_h)
    };
    let _ = edit.MoveWindow(P { x: m, y: edit_top }, S { cx: content_w, cy: edit_h }, true);
    let _ = cand.MoveWindow(P { x: m, y: cand_y }, S { cx: content_w, cy: cand_h }, true);
}

pub fn code_box(
    parent: &impl GuiParent,
    message: &str,
    value: &str,
    members: &[CompletionMember],
) -> Option<String> {
    // サイズ可変＋サイズ記憶（キー "code_box"・キー編集/メニュー編集で共有）。
    let (wnd, arm) = modal_window_resizable_keyed("コードを割り当て", "code_box", 560, 480, 360, 300);

    // 種の式が複数行を含めば展開モード、単一行ならコンパクトモードで開く（凝った式だけ広く使う）。
    let multiline_init = value.contains('\n');
    let (edit_h0, cand_y0, cand_h0) = code_box_geometry(multiline_init);

    let label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: message,
            position: gui::dpi(16, 14),
            size: gui::dpi(348, 18),
            ..Default::default()
        },
    );

    // 1 行／複数行モードの手動トグル。種の式に応じた初期状態で開き、押すと入力欄と候補欄を畳む。
    let mode = gui::CheckBox::new(
        &wnd,
        gui::CheckBoxOpts {
            text: "複数行(&M)",
            position: gui::dpi(372, 12),
            size: gui::dpi(92, 20),
            check_state: if multiline_init { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        },
    );

    let edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            text: value,
            control_style: co::ES::MULTILINE
                | co::ES::WANTRETURN
                | co::ES::AUTOVSCROLL
                | co::ES::NOHIDESEL,
            window_style: co::WS::CHILD
                | co::WS::GROUP
                | co::WS::TABSTOP
                | co::WS::VISIBLE
                | co::WS::BORDER
                | co::WS::VSCROLL,
            position: gui::dpi(16, 38),
            width: gui::dpi_x(448),
            height: gui::dpi_y(edit_h0),
            ..Default::default()
        },
    );

    // `r.` 補完の候補リスト。既定は隠しておき、`r.<prefix>` を打つと候補を入れて表示する。
    // 項目のシングルクリックで、カレット直前のプレフィックスをその候補名に置換する。
    let cand = gui::ListBox::new(
        &wnd,
        gui::ListBoxOpts {
            position: gui::dpi(16, cand_y0),
            size: gui::dpi(448, cand_h0),
            // タブストップで「名前／説明」を桁揃えする（説明の頭位置を固定する）。
            control_style: co::LBS::NOTIFY | co::LBS::USETABSTOPS,
            window_style: co::WS::CHILD | co::WS::BORDER | co::WS::VSCROLL,
            ..Default::default()
        },
    );

    // 現在のモード（複数行なら true）。トグルとリサイズ追従が共有する。
    let expanded = Rc::new(Cell::new(multiline_init));

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(290, 356),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );
    let cancel = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "キャンセル",
            ctrl_id: 2,
            position: gui::dpi(378, 356),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );
    // 子コントロールを現在のクライアントサイズとモードに合わせて配置し直す共有処理。リサイズと
    // 「複数行」トグルの両方から呼ぶ。
    let relayout: Rc<dyn Fn(bool)> = {
        let wnd = wnd.clone();
        let (label, mode, edit, cand, ok, cancel) = (
            label.clone(),
            mode.clone(),
            edit.clone(),
            cand.clone(),
            ok.clone(),
            cancel.clone(),
        );
        Rc::new(move |expanded: bool| {
            if let Ok(rc) = wnd.hwnd().GetClientRect() {
                relayout_code_box(
                    label.hwnd(),
                    mode.hwnd(),
                    edit.hwnd(),
                    cand.hwnd(),
                    ok.hwnd(),
                    cancel.hwnd(),
                    rc.right,
                    rc.bottom,
                    expanded,
                );
            }
        })
    };
    // 「複数行」トグル＝モードを反転して配置し直す（コントロール再生成なし）。
    {
        let mode_h = mode.clone();
        let expanded = expanded.clone();
        let relayout = relayout.clone();
        mode.on().bn_clicked(move || {
            let exp = mode_h.is_checked();
            expanded.set(exp);
            relayout(exp);
            Ok(())
        });
    }
    // リサイズ追従＝現在のモードのまま配置し直す。
    {
        let expanded = expanded.clone();
        let relayout = relayout.clone();
        wnd.on().wm_size(move |_| {
            relayout(expanded.get());
            Ok(())
        });
    }

    let result: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    arm.plain(
        "input",
        "コードを割り当て",
        message,
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
    );
    {
        let result = result.clone();
        let edit = edit.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            *result.borrow_mut() = Some(edit.text().unwrap_or_default());
            wnd2.close();
            Ok(())
        });
    }
    {
        let wnd2 = wnd.clone();
        cancel.on().bn_clicked(move || {
            wnd2.close();
            Ok(())
        });
    }
    // 補完モデル＝カレット直前が `r.`／`rerics.` のときだけ、その後の識別子に曖昧一致するメンバを
    // ジャンル見出し付きで出す（`r.` 直後の空クエリは全件＝機能ブラウザを兼ねる）。`force` は
    // Ctrl+Space＝「唯一かつ入力済みと同一」でも出す。
    let members_owned: Vec<CompletionMember> = members.to_vec();
    let complete: CompleteFn = Rc::new(move |before, caret, force| {
        let (plen, prefix) = completion_prefix(before)?;
        let items = completion_items(&members_owned, &prefix);
        let names: Vec<&str> =
            items.iter().filter(|i| i.selectable).map(|i| i.display.as_str()).collect();
        let only_exact = names.len() == 1 && names[0].eq_ignore_ascii_case(&prefix);
        if names.is_empty() || (!force && only_exact) {
            return None;
        }
        Some((caret - plen as u32, items))
    });
    install_completion(&arm, &edit, &cand, complete);

    // headless 観測：現在のモード（1 行／複数行）を読めるようにする。
    #[cfg(feature = "debug-server")]
    {
        let expanded = expanded.clone();
        completion_probe::set_multiline(Box::new(move || expanded.get()));
    }

    let _ = wnd.show_modal(parent);
    keyhook::pop();
    #[cfg(feature = "debug-server")]
    completion_probe::clear();
    let _ = (cancel, cand, mode, expanded);
    result.borrow().clone()
}

/// コマンドパレット（原作 `CommandDirect`）。1 行入力にコマンド名補完を付け、OK／Enter で打った
/// 文字列を返す（キャンセル/Esc は None）。`commands`＝(表示, 挿入トークン) の候補。補完は和名・
/// 内部トークンどちらの部分一致でも引け、確定で挿入トークンを入力欄へ入れる（実行は呼び出し側）。
pub fn command_box(
    parent: &impl GuiParent,
    message: &str,
    commands: &[(String, String)],
) -> Option<String> {
    let (wnd, arm) = modal_window("コマンドを実行", 460, 320);

    let _label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: message,
            position: gui::dpi(16, 14),
            size: gui::dpi(428, 18),
            ..Default::default()
        },
    );

    let edit = gui::Edit::new(
        &wnd,
        gui::EditOpts {
            control_style: co::ES::AUTOHSCROLL | co::ES::NOHIDESEL,
            window_style: co::WS::CHILD
                | co::WS::GROUP
                | co::WS::TABSTOP
                | co::WS::VISIBLE
                | co::WS::BORDER,
            position: gui::dpi(16, 38),
            width: gui::dpi_x(428),
            height: gui::dpi_y(24),
            ..Default::default()
        },
    );

    // コマンド名補完の候補リスト（既定は隠す）。`和名 (Token)` を見せて Token を挿入する。
    let cand = gui::ListBox::new(
        &wnd,
        gui::ListBoxOpts {
            position: gui::dpi(16, 70),
            size: gui::dpi(428, 180),
            // タブストップで「名前／説明」を桁揃えする（説明の頭位置を固定する）。
            control_style: co::LBS::NOTIFY | co::LBS::USETABSTOPS,
            window_style: co::WS::CHILD | co::WS::BORDER | co::WS::VSCROLL,
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(270, 262),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );
    let cancel = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "キャンセル",
            ctrl_id: 2,
            position: gui::dpi(358, 262),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    #[cfg(feature = "debug-server")]
    arm.plain(
        "input",
        "コマンドを実行",
        message,
        true,
        vec![("OK".to_string(), 1u16), ("キャンセル".to_string(), 2u16)],
    );
    {
        let result = result.clone();
        let edit = edit.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            *result.borrow_mut() = Some(edit.text().unwrap_or_default());
            wnd2.close();
            Ok(())
        });
    }
    {
        let wnd2 = wnd.clone();
        cancel.on().bn_clicked(move || {
            wnd2.close();
            Ok(())
        });
    }

    // 補完モデル＝入力全体（カレットまで）をクエリにし、和名 or トークンの部分一致で候補を出す。
    // 入力がちょうど 1 件のトークンと一致したら（非 force）出さない＝その状態の Enter は実行に回す。
    let commands_for = commands.to_vec();
    let complete: CompleteFn = Rc::new(move |before, _caret, force| {
        let q = before.trim_start();
        if q.is_empty() && !force {
            return None;
        }
        let ql = q.to_lowercase();
        let items: Vec<CompletionItem> = commands_for
            .iter()
            .filter(|(disp, tok)| {
                q.is_empty() || tok.to_lowercase().contains(&ql) || disp.contains(q)
            })
            .map(|(disp, tok)| {
                // パレットのトークンは `cursorDown()` 形なので Call::parse で組込を引く。
                let detail = match rerics_core::Call::parse(tok) {
                    rerics_core::Call::Builtin { command, .. } => Some(meta_hint(command)),
                    rerics_core::Call::Script { .. } => None,
                };
                CompletionItem::plain(disp.clone(), tok.clone(), detail)
            })
            .collect();
        let only_exact = items.len() == 1 && items[0].insert.eq_ignore_ascii_case(q);
        if items.is_empty() || (!force && only_exact) {
            return None;
        }
        Some((0, items))
    });
    install_completion(&arm, &edit, &cand, complete);

    let _ = wnd.show_modal(parent);
    keyhook::pop();
    #[cfg(feature = "debug-server")]
    completion_probe::clear();
    let _ = (cancel, cand);
    result.borrow().clone()
}

/// 開いている補完つき入力欄（`code_box`）を headless から観測・駆動する窓口。モーダルは UI
/// スレッドの入れ子ループで開くので、その間 debug リクエストはこのプローブ越しに候補を読み・確定できる。
#[cfg(feature = "debug-server")]
pub mod completion_probe {
    use std::cell::RefCell;

    pub struct Probe {
        /// 入力を模擬する：本文を `s` にしてカレットを末尾に置き、補完を更新する。
        pub type_text: Box<dyn Fn(&str)>,
        /// 現在の候補リスト（上から順）。
        pub candidates: Box<dyn Fn() -> Vec<String>>,
        /// 候補リストが表示中か。
        pub visible: Box<dyn Fn() -> bool>,
        /// 選択中の候補 index（無選択は -1）。
        pub selected: Box<dyn Fn() -> i32>,
        /// idx 番目の候補を確定（カレット直前のプレフィックスを置換）。
        pub accept: Box<dyn Fn(u32)>,
        /// 入力欄の現在の本文。
        pub text: Box<dyn Fn() -> String>,
    }

    thread_local! {
        static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
        /// 式エディタ（`code_box`）の現在モード読み取り。入力欄系（`input_box` 等）は付けない。
        static MULTILINE: RefCell<Option<Box<dyn Fn() -> bool>>> = const { RefCell::new(None) };
    }

    pub fn set(p: Probe) {
        PROBE.with(|s| *s.borrow_mut() = Some(p));
    }
    pub fn clear() {
        PROBE.with(|s| *s.borrow_mut() = None);
        MULTILINE.with(|s| *s.borrow_mut() = None);
    }
    /// `code_box` が現在モード（複数行なら true）を読む関数を登録する。
    pub fn set_multiline(f: Box<dyn Fn() -> bool>) {
        MULTILINE.with(|s| *s.borrow_mut() = Some(f));
    }
    /// 式エディタが複数行モードか（式エディタが開いていなければ None）。
    pub fn multiline() -> Option<bool> {
        MULTILINE.with(|s| s.borrow().as_ref().map(|f| f()))
    }
    /// 本文を `s` にして補完を更新する（入力の模擬）。開いていれば `true`。
    pub fn type_text(s: &str) -> bool {
        PROBE.with(|p| p.borrow().as_ref().map(|p| (p.type_text)(s)).is_some())
    }
    /// 候補一覧（入力欄が開いていなければ None）。
    pub fn candidates() -> Option<Vec<String>> {
        PROBE.with(|s| s.borrow().as_ref().map(|p| (p.candidates)()))
    }
    /// 候補リストが表示中か（入力欄が開いていなければ None）。
    pub fn visible() -> Option<bool> {
        PROBE.with(|s| s.borrow().as_ref().map(|p| (p.visible)()))
    }
    /// 選択中の候補 index（無選択は -1・入力欄が開いていなければ None）。
    pub fn selected() -> Option<i32> {
        PROBE.with(|s| s.borrow().as_ref().map(|p| (p.selected)()))
    }
    /// idx 番目の候補を確定する。開いていれば `true`。
    pub fn accept(idx: u32) -> bool {
        PROBE.with(|s| {
            s.borrow().as_ref().map(|p| (p.accept)(idx)).is_some()
        })
    }
    /// 入力欄の現在の本文（開いていなければ None）。
    pub fn text() -> Option<String> {
        PROBE.with(|s| s.borrow().as_ref().map(|p| (p.text)()))
    }
}

#[cfg(test)]
mod tests {
    use super::{CompletionMember, completion_items, completion_prefix, match_rank};
    use crate::script::ScriptCommand;

    fn member(name: &str, callable: bool, arity: u32) -> CompletionMember {
        CompletionMember { name: name.to_string(), callable, arity, script: None }
    }

    #[test]
    fn rank_orders_prefix_initials_label_subsequence() {
        // 前方一致が最強。
        assert_eq!(match_rank("cur", "cursorPath", None), Some(0));
        // camelCase 頭文字一致。
        assert_eq!(match_rank("cp", "cursorPath", None), Some(1));
        // 和名の部分一致。
        assert_eq!(match_rank("削除", "delete", Some("削除")), Some(2));
        // 飛び石一致。
        assert_eq!(match_rank("cpath", "cursorPath", None), Some(3));
        // 大小無視。
        assert_eq!(match_rank("CUR", "cursorPath", None), Some(0));
        // 合わなければ None・空クエリは全件。
        assert_eq!(match_rank("xyz", "cursorPath", None), None);
        assert_eq!(match_rank("", "cursorPath", None), Some(0));
    }

    #[test]
    fn items_group_by_genre_with_label_rows() {
        let members = vec![
            member("copy", true, 3),
            member("cursorDown", true, 0),
            member("spawn", true, 1),
        ];
        let items = completion_items(&members, "");
        let displays: Vec<&str> = items.iter().map(|i| i.display.as_str()).collect();
        // 複数ジャンル＝見出しラベル行が挟まり、ラベルは選択不可。
        assert_eq!(
            displays,
            vec![
                "── カーソル移動 ──",
                "cursorDown",
                "── ファイル操作 ──",
                "copy",
                "── スクリプト・API ──",
                "spawn"
            ],
            "ジャンル順＋見出し: {displays:?}"
        );
        assert!(items.iter().filter(|i| i.display.starts_with("──")).all(|i| !i.selectable));
    }

    #[test]
    fn items_single_genre_omits_label_row() {
        let members = vec![member("cursorDown", true, 0), member("cursorUp", true, 0)];
        let items = completion_items(&members, "cursor");
        assert!(items.iter().all(|i| i.selectable), "単一ジャンルは見出しなし");
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn fuzzy_match_pulls_strong_group_first() {
        // "cp" は cursorPath（頭文字一致）が copy（飛び石一致）より強い＝所属ジャンルごと上に来る。
        let members = vec![member("copy", true, 3), member("cursorPath", true, 0)];
        let items = completion_items(&members, "cp");
        let displays: Vec<&str> = items.iter().map(|i| i.display.as_str()).collect();
        let pos = |n: &str| displays.iter().position(|x| *x == n).expect(n);
        assert!(pos("cursorPath") < pos("copy"), "強いマッチの群が先: {displays:?}");
    }

    #[test]
    fn items_append_parens_by_arity_and_skip_objects() {
        let members = vec![
            member("cursorDown", true, 0),     // 組込・引数は省略可の {select} のみ
            member("setCursorIndex", true, 0), // 組込・必須引数あり（メタが正・ラッパ arity は 0）
            member("spawn", true, 1),          // host API・引数あり
            member("fs", false, 0),            // オブジェクト
        ];
        let items = completion_items(&members, "");
        let find = |n: &str| items.iter().find(|i| i.display == n).expect(n);
        assert_eq!(find("cursorDown").insert, "cursorDown()");
        assert_eq!(find("cursorDown").caret_back, 0, "省略可のみはカレットを閉じ括弧の後ろへ");
        assert_eq!(find("setCursorIndex").insert, "setCursorIndex()");
        assert_eq!(find("setCursorIndex").caret_back, 1, "組込の必須引数はメタデータから引く");
        assert_eq!(find("spawn").caret_back, 1, "host API は宣言引数から引く");
        assert_eq!(find("fs").insert, "fs", "オブジェクトには括弧を付けない");
    }

    #[test]
    fn items_keep_same_hierarchy_only() {
        let members = vec![
            member("clipboard", false, 0),
            member("clipboard.getText", true, 0),
            member("clipboard.setText", true, 1),
            member("clearAll", true, 0),
        ];
        // トップレベル（query にドット無し）はドット付きメンバを含めない。
        let top: Vec<String> = completion_items(&members, "cl")
            .into_iter()
            .filter(|i| i.selectable)
            .map(|i| i.display)
            .collect();
        assert_eq!(top, vec!["clearAll", "clipboard"]);
        // 名前空間配下（query にドット）はその階層だけ。
        let sub: Vec<String> = completion_items(&members, "clipboard.")
            .into_iter()
            .map(|i| i.display)
            .collect();
        assert_eq!(sub, vec!["clipboard.getText", "clipboard.setText"]);
    }

    #[test]
    fn script_genre_merges_into_builtin_group_and_label_matches() {
        let mut sc = member("organize", true, 0);
        sc.script = Some(ScriptCommand {
            name: "organize".to_string(),
            label: Some("整理する".to_string()),
            genre: Some("ファイル操作".to_string()),
            summary: Some("散らかりを整理".to_string()),
        });
        let members = vec![member("copy", true, 3), member("cursorDown", true, 0), sc];
        // genre 指定のスクリプトコマンドは組込ジャンルの群に混ざる＝「ファイル操作」見出しは 1 つ。
        let items = completion_items(&members, "");
        let labels: Vec<&str> =
            items.iter().filter(|i| !i.selectable).map(|i| i.display.as_str()).collect();
        assert_eq!(
            labels,
            vec!["── カーソル移動 ──", "── ファイル操作 ──"],
            "群がマージされる: {labels:?}"
        );
        // 和名でも引ける。
        let hit: Vec<String> = completion_items(&members, "整理")
            .into_iter()
            .filter(|i| i.selectable)
            .map(|i| i.display)
            .collect();
        assert_eq!(hit, vec!["organize"]);
    }

    #[test]
    fn prefix_detects_r_member_access() {
        assert_eq!(completion_prefix("=r.cur"), Some((3, "cur".into())));
        assert_eq!(completion_prefix("=r."), Some((0, "".into())));
        assert_eq!(completion_prefix("=rerics.fold"), Some((4, "fold".into())));
        // 文の途中でも直前が r. ならよい。
        assert_eq!(completion_prefix("=r.cursorName() + r.cu"), Some((2, "cu".into())));
    }

    #[test]
    fn prefix_rejects_non_r_contexts() {
        // 手前が別トークン（bar）なので補完しない。
        assert_eq!(completion_prefix("bar.cur"), None);
        // ただの識別子・ドット無し。
        assert_eq!(completion_prefix("=current"), None);
        // r だけ（ドット未入力）はまだメンバアクセスでない。
        assert_eq!(completion_prefix("=r"), None);
    }

    #[test]
    fn prefix_detects_namespace_member_access() {
        // `r.<名前空間>.<識別子>` は名前空間込みの prefix を返す。
        assert_eq!(completion_prefix("=r.fs.read"), Some((7, "fs.read".into())));
        assert_eq!(completion_prefix("=r.fs."), Some((3, "fs.".into())));
        assert_eq!(completion_prefix("=rerics.env.doc"), Some((7, "env.doc".into())));
        // 名前空間の手前が r/rerics でなければ補完しない。
        assert_eq!(completion_prefix("=foo.bar.baz"), None);
    }

}
