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

/// カレットまでのテキスト `before` から補完文脈を返す。返り＝(置換長 UTF-16, クエリ, 裸文脈か)。
/// `r.`/`rerics.` のメンバアクセス（[`completion_prefix`]・裸=false・全メンバが対象）に加え、
/// **式の先頭の裸の識別子**（手前が空白のみ）も文脈とする（裸=true）。機能欄の正史は裸の
/// `cursorUp()` 形（`Call` の fast-path）なので、その表記のまま補完が効くようにする。
/// 裸で呼べるのは組込コマンドだけ（host API・スクリプト関数は裸だとエンジンで未定義）なので、
/// 候補の絞り込みは呼び手が裸フラグで行う。
fn completion_context(before: &str) -> Option<(usize, String, bool)> {
    if let Some((plen, prefix)) = completion_prefix(before) {
        return Some((plen, prefix, false));
    }
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let rev: String = before.chars().rev().take_while(|c| is_ident(*c)).collect();
    let ident: String = rev.chars().rev().collect();
    let head = &before[..before.len() - ident.len()];
    if head.trim().is_empty() {
        return Some((ident.encode_utf16().count(), ident, true));
    }
    None
}

/// 編集中の呼び出し＝カレットを囲う最も内側の関数呼び出しの文脈。
#[derive(Debug, PartialEq)]
struct CallCtx {
    /// 呼び出し名（`r.spawn`・裸の `cursorUp` 等・ドット込み）。
    name: String,
    /// カレットが何番目の引数にいるか（0 始まり）。
    arg: usize,
    /// カレットが引数の Object リテラル（`{}`）の中にいるか。
    in_object: bool,
    /// Object の中で現在のエントリの `:` より後（＝値の位置）にいるか。
    after_colon: bool,
    /// カレットが閉じていない文字列リテラルの中にいるか（値の入力途中）。
    in_string: bool,
}

/// カレットまでのテキスト `before` から編集中の呼び出し（[`CallCtx`]）を返す。前から走査して
/// 開き括弧のスタックを保ち、文字列・コメントの中は読み飛ばす。`{}`/`[]` の中のカンマは引数の
/// 区切りに数えない（オブジェクトリテラル引数）。名前の取れる `(` が無ければ（グルーピングの
/// 括弧だけ等）None。
fn enclosing_call(before: &str) -> Option<CallCtx> {
    struct Open {
        kind: char,
        /// `(` のとき、その直前の呼び出し名。
        name: Option<String>,
        commas: usize,
        /// `{` のとき、現在のエントリで `:` を通過したか。
        after_colon: bool,
    }
    let chars: Vec<char> = before.chars().collect();
    let mut stack: Vec<Open> = Vec::new();
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            q @ ('"' | '\'' | '`') => {
                i += 1;
                while i < chars.len() && chars[i] != q {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                // 閉じクォート無しで末尾に達した＝カレットは文字列の中。
                in_string = i >= chars.len();
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 1;
            }
            c @ ('(' | '[' | '{') => {
                let name = (c == '(').then(|| call_name_before(&chars[..i])).flatten();
                stack.push(Open { kind: c, name, commas: 0, after_colon: false });
            }
            ')' | ']' | '}' => {
                stack.pop();
            }
            ',' => {
                if let Some(top) = stack.last_mut() {
                    top.commas += 1;
                    top.after_colon = false;
                }
            }
            ':' => {
                if let Some(top) = stack.last_mut()
                    && top.kind == '{'
                {
                    top.after_colon = true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let (in_object, after_colon) = stack
        .last()
        .map(|o| (o.kind == '{', o.kind == '{' && o.after_colon))
        .unwrap_or((false, false));
    stack.iter().rev().find_map(|o| {
        (o.kind == '(')
            .then(|| {
                o.name.clone().map(|name| CallCtx {
                    name,
                    arg: o.commas,
                    in_object,
                    after_colon,
                    in_string,
                })
            })
            .flatten()
    })
}

/// 引数値の補完で置換するプレフィックス。カレット直前の識別子と、文字列の入力途中
/// （`in_string`）ならその開きクォートも置換範囲に含める。
/// 返り＝(置換長 UTF-16, クォートを除いた入力途中の文字列)。
fn value_prefix(before: &str, in_string: bool) -> (usize, String) {
    let chars: Vec<char> = before.chars().collect();
    let mut start = chars.len();
    while start > 0 && (chars[start - 1].is_ascii_alphanumeric() || chars[start - 1] == '_') {
        start -= 1;
    }
    let query: String = chars[start..].iter().collect();
    let mut plen = query.encode_utf16().count();
    if in_string && start > 0 && (chars[start - 1] == '"' || chars[start - 1] == '\'') {
        plen += 1;
    }
    (plen, query)
}

/// カレットが引数リストの中にいるときの値補完候補。組込は `ArgType::Enum` の値と
/// `ArgType::Options` のキー、host API は型中の文字列リテラル union の値とオプション Object の
/// キー（d.ts 由来）を出す。Object 内の値の位置（`:` の後）では出さない。該当なしは空。
fn value_items(call: &CallCtx, query: &str) -> Vec<CompletionItem> {
    use rerics_core::{ArgType, Command};
    if call.in_object && call.after_colon {
        return Vec::new();
    }
    let (token, bare) =
        match call.name.strip_prefix("r.").or_else(|| call.name.strip_prefix("rerics.")) {
            Some(t) => (t, false),
            None if !call.name.contains('.') => (call.name.as_str(), true),
            _ => return Vec::new(),
        };
    let q = query.to_lowercase();
    let mut out = Vec::new();
    // `r.` 文脈は host API が実体（組込と同名でも host が勝つ）なので host を先に引く。
    if !bare && let Some(sig) = crate::hostsig::host_sig(token) {
        let Some(idx) = host_param_index(sig, call.arg) else {
            return out;
        };
        let ty = &sig.params[idx].ty;
        if call.in_object {
            for (name, doc) in crate::hostsig::option_keys(ty) {
                if name.to_lowercase().starts_with(&q) {
                    let detail = (!doc.is_empty()).then_some(doc);
                    out.push(CompletionItem::plain(name.clone(), format!("{name}: "), detail));
                }
            }
        } else {
            for v in crate::hostsig::enum_values(ty) {
                if v.to_lowercase().starts_with(&q) {
                    out.push(CompletionItem::plain(format!("\"{v}\""), format!("\"{v}\""), None));
                }
            }
        }
        return out;
    }
    let Some(cmd) = Command::from_token(token) else {
        return out;
    };
    let Some(spec) = cmd.meta().args.get(call.arg) else {
        return out;
    };
    match spec.ty {
        ArgType::Enum(vals) if !call.in_object => {
            for v in vals {
                if v.to_lowercase().starts_with(&q) {
                    out.push(CompletionItem::plain(format!("\"{v}\""), format!("\"{v}\""), None));
                }
            }
        }
        ArgType::Options(opts) if call.in_object => {
            for o in opts {
                if o.name.to_lowercase().starts_with(&q) {
                    let detail = (!o.doc.is_empty()).then(|| o.doc.to_string());
                    out.push(CompletionItem::plain(
                        o.name.to_string(),
                        format!("{}: ", o.name),
                        detail,
                    ));
                }
            }
        }
        _ => {}
    }
    out
}

/// `(` の直前の呼び出し名（識別子＋ドットの連なり・手前の空白は許す）。無ければ None。
fn call_name_before(head: &[char]) -> Option<String> {
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    let mut end = head.len();
    while end > 0 && head[end - 1].is_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_name(head[start - 1]) {
        start -= 1;
    }
    (start < end).then(|| head[start..end].iter().collect())
}

/// 編集中の呼び出し `name`（`r.`/`rerics.` 込みか裸・第 `arg` 引数）へのシグネチャヘルプ 1 行を
/// 組む。`r.` 文脈は host API（d.ts 由来・実体が host）→組込→スクリプトの順、裸は fast-path＝
/// 組込だけを引く。いま書いている引数は `‹›` で強調し、組込で引数個別の説明（doc）があれば
/// 説明文をそれに差し替える。解決できない名前（`foo.bar` 等）は None。
fn signature_help(members: &[CompletionMember], name: &str, arg: usize) -> Option<String> {
    use rerics_core::Command;
    let (token, bare) =
        match name.strip_prefix("r.").or_else(|| name.strip_prefix("rerics.")) {
            Some(t) => (t, false),
            None if !name.contains('.') => (name, true),
            _ => return None,
        };
    if bare {
        return Command::from_token(token).map(|cmd| builtin_help(cmd, arg));
    }
    if let Some(sig) = crate::hostsig::host_sig(token) {
        return Some(host_help(token, sig, arg));
    }
    if let Some(cmd) = Command::from_token(token) {
        return Some(builtin_help(cmd, arg));
    }
    let m = members.iter().find(|m| m.name == token)?;
    let summary = m.script.as_ref().and_then(|s| s.summary.clone()).unwrap_or_default();
    Some(format!("{token}(…)  {summary}").trim_end().to_string())
}

/// 組込コマンドのシグネチャヘルプ。第 `arg` 引数を `‹›` で強調し、その引数の doc（無ければ
/// コマンドの summary）を添える。
fn builtin_help(cmd: rerics_core::Command, arg: usize) -> String {
    let meta = cmd.meta();
    let parts: Vec<String> = meta
        .args
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let name = arg_display(spec);
            if i == arg { format!("‹{name}›") } else { name }
        })
        .collect();
    let doc = meta.args.get(arg).map(|s| s.doc).filter(|d| !d.is_empty()).unwrap_or(meta.summary);
    format!("{}({})  {}", cmd.as_token(), parts.join(", "), doc)
}

/// host API の第 `arg` 引数に対応するパラメータ位置。rest 引数（`...`）は溢れた分もそこへ
/// 入るので、`arg` が末尾を超えていたら rest の位置を返す。
fn host_param_index(sig: &crate::hostsig::HostSig, arg: usize) -> Option<usize> {
    if arg < sig.params.len() {
        return Some(arg);
    }
    sig.params.len().checked_sub(1).filter(|&last| sig.params[last].name.starts_with("..."))
}

/// host API のシグネチャヘルプ。第 `arg` 引数を `‹›` で強調する。
fn host_help(token: &str, sig: &crate::hostsig::HostSig, arg: usize) -> String {
    let highlight = host_param_index(sig, arg);
    let parts: Vec<String> = sig
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| if Some(i) == highlight { format!("‹{}›", p.name) } else { p.name.clone() })
        .collect();
    format!("{}({})  {}", token, parts.join(", "), sig.summary)
}

/// 式ソースの構文エラーを返す（無ければ None）。エンジンの実行系と同じ deno_ast（TypeScript
/// 扱い）でパースする＝ここを通れば型消去・実行へ進める。空文はチェックしない。
/// 返り＝(メッセージ, 1 始まり行, 1 始まり桁)。
fn syntax_error(code: &str) -> Option<(String, usize, usize)> {
    use deno_ast::diagnostics::Diagnostic as _;
    if code.trim().is_empty() {
        return None;
    }
    let parsed = deno_ast::parse_module(deno_ast::ParseParams {
        specifier: deno_ast::ModuleSpecifier::parse("file:///expr.ts").expect("static url"),
        text: code.to_string().into(),
        media_type: deno_ast::MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    });
    // 致命的エラー（Err）に加え、リカバリして続行したエラー（diagnostics）も構文エラー扱いにする
    // （swc が recover しても V8 では落ちる）。
    let diag = match &parsed {
        Ok(p) => p.diagnostics().first().cloned(),
        Err(e) => Some(e.clone()),
    }?;
    let pos = diag.display_position();
    Some((diag.message().to_string(), pos.line_number, pos.column_number))
}

/// Script 式の未解決識別子＝どのスコープにも束縛されず、エンジンの `globalThis` にも実在しない
/// 名前を探す（最初の 1 件）。V8 は動的なので実行するまで気づけない typo をここで捕まえる。
/// 型注釈の中の名前は実行時の解決と無関係なので見ない。構文エラーの式は対象外（先に構文検査を通す）。
/// 返り＝(名前, 1 始まり行, 1 始まり桁)。
fn unresolved_ident(code: &str, globals: &[String]) -> Option<(String, usize, usize)> {
    use deno_ast::SourceRangedForSpanned as _;
    use deno_ast::swc::ecma_visit::{Visit, VisitWith};
    struct Finder<'a> {
        unresolved: deno_ast::swc::common::SyntaxContext,
        globals: &'a [String],
        found: Option<(String, deno_ast::SourcePos)>,
    }
    impl Visit for Finder<'_> {
        fn visit_ident(&mut self, n: &deno_ast::swc::ast::Ident) {
            let sym = n.sym.as_str();
            // `arguments` は関数の中でだけ暗黙に束縛される特殊名なので許す。
            if self.found.is_none()
                && n.ctxt == self.unresolved
                && sym != "arguments"
                && !self.globals.iter().any(|g| g == sym)
            {
                self.found = Some((sym.to_string(), n.start()));
            }
        }
        // 型注釈（TS）は型消去で消える＝実行時の名前解決に関与しない。
        fn visit_ts_type(&mut self, _n: &deno_ast::swc::ast::TsType) {}
    }
    let parsed = deno_ast::parse_module(deno_ast::ParseParams {
        specifier: deno_ast::ModuleSpecifier::parse("file:///expr.ts").expect("static url"),
        text: code.to_string().into(),
        media_type: deno_ast::MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: true,
        maybe_syntax: None,
    })
    .ok()?;
    let mut finder =
        Finder { unresolved: parsed.unresolved_context(), globals, found: None };
    parsed.program().visit_with(&mut finder);
    let (name, pos) = finder.found?;
    let d = parsed.text_info_lazy().line_and_column_display(pos);
    Some((name, d.line_number, d.column_number))
}

/// OK 時の意味検査。組込 fast-path 呼び出しは引数をメタデータと突き合わせ、エンジン行きの
/// Script 式は未解決識別子を探す。問題があれば (メッセージ, 1 始まり行, 1 始まり桁)。
/// 組込コマンド名そのものが未解決（裸のままリテラル以外の引数で呼んだ等）のときは、
/// `r.` 経由の書き方へ誘導する。
fn semantic_error(code: &str, globals: &[String]) -> Option<(String, usize, usize)> {
    use rerics_core::{Call, Command, validate_builtin_args};
    match Call::parse(code) {
        Call::Builtin { command, args } => {
            validate_builtin_args(command, &args).map(|msg| (msg, 1, 1))
        }
        Call::Script { .. } => {
            let (name, line, col) = unresolved_ident(code, globals)?;
            let msg = if Command::from_token(&name).is_some() {
                format!(
                    "組込 {name} はこの形ではエンジンから見えない（{name}(リテラル引数) の単独呼び出しか r.{name}(...) と書く）"
                )
            } else {
                format!("{name} は定義されていない")
            };
            Some((msg, line, col))
        }
    }
}

/// テキスト内の `line`（1 始まり）・`column`（1 始まり）を UTF-16 オフセットへ変換する
/// （Edit の `set_selection` 用）。範囲を超えたら末尾。
fn utf16_offset_at(text: &str, line: usize, column: usize) -> usize {
    let mut off = 0;
    let mut l = 1;
    let mut c = 1;
    for ch in text.chars() {
        if l == line && c == column {
            return off;
        }
        if ch == '\n' {
            l += 1;
            c = 1;
        } else if l == line && ch != '\r' {
            c += 1;
        }
        off += ch.len_utf16();
    }
    off
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
/// `bare`（式の先頭の裸の識別子＝fast-path 文脈）では組込コマンドだけに絞る。
/// 並びはジャンルごとに固め、ジャンル見出しのラベル行を挟む（該当が 1 ジャンルだけなら挟まない）。
/// query が非空のときはジャンル自体も「群内の最強マッチ」順に並べ、目当ての機能が上に来るようにする。
/// 確定挿入は callable なら `名前()`（引数ありはカレットを括弧内へ）、オブジェクトは名前のみ。
fn completion_items(members: &[CompletionMember], query: &str, bare: bool) -> Vec<CompletionItem> {
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
        if bare && builtin.is_none() {
            continue;
        }
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
        // 引数有無：host API は宣言引数（`r.` 文脈では組込と同名でも host が実体なのでこちらを優先）、
        // 組込コマンドはメタデータの必須引数から（省略可のみなら引数なし扱い＝そのまま呼べる形で
        // 確定してカレットを括弧の後ろへ置く）。裸文脈は fast-path＝常に組込が実体なのでメタだけを
        // 見る。callable でなければ（オブジェクト）括弧を付けない。
        let builtin_needs_args =
            builtin.map(|c| c.meta().args.iter().any(|a| a.required)).unwrap_or(false);
        let has_args = builtin_needs_args || (!bare && m.arity > 0);
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
    if args.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = args.iter().map(arg_display).collect();
    format!("({})", parts.join(", "))
}

/// 引数仕様 1 件の表示名（`path`・`by?`・`{select?}` 等）。
fn arg_display(a: &rerics_core::ArgSpec) -> String {
    use rerics_core::ArgType;
    match a.ty {
        ArgType::Options(opts) => {
            let keys: Vec<String> = opts.iter().map(|o| format!("{}?", o.name)).collect();
            format!("{{{}}}", keys.join(", "))
        }
        _ if a.required => a.name.to_string(),
        _ => format!("{}?", a.name),
    }
}

/// 補完モデル。カレットまでの文字列 `before`・カレットの UTF-16 位置・`force`（Ctrl+Space の
/// 明示トリガ）から、置換開始位置（UTF-16・終端はカレット）と候補列を返す。None なら出さない。
type CompleteFn = Rc<dyn Fn(&str, u32, bool) -> Option<(u32, Vec<CompletionItem>)>>;

/// signature help の更新先。カレットまでの文字列を受けてヒント行を書き換える。
type ContextHintFn = Rc<dyn Fn(&str)>;

/// 補完つき入力欄の配線（`code_box`／`command_box` 共通）。`edit` の下に隠した `cand` を、
/// `complete` モデルの返す候補で出し入れする。候補は表示文字列で見せ、確定時は挿入文字列を
/// カレット直前の置換範囲へ入れる。キー操作（↑↓移動クランプ・Enter 確定・Ctrl+Space 表示）と
/// headless 観測（`completion_probe`）もここでまとめて仕込む。`show_modal` は呼び出し側で。
/// `context_hint` を渡すと、本文・カレットが動くたびカレットまでのテキストで呼ぶ
/// （signature help の更新用）。
fn install_completion(
    arm: &ModalArm,
    edit: &gui::Edit,
    cand: &gui::ListBox,
    complete: CompleteFn,
    context_hint: Option<ContextHintFn>,
) {
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
        let context_hint = context_hint.clone();
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
                    // カレットを括弧内へ戻した位置で signature help を出し直す
                    // （EN_CHANGE 時点のカレットは挿入末尾なので、ここで再評価する）。
                    if let Some(hint) = &context_hint {
                        let utf16: Vec<u16> =
                            edit.text().unwrap_or_default().encode_utf16().collect();
                        let end = (pos as usize).min(utf16.len());
                        hint(&String::from_utf16_lossy(&utf16[..end]));
                    }
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
        let context_hint = context_hint.clone();
        Rc::new(move |force: bool| {
            let utf16: Vec<u16> = edit.text().unwrap_or_default().encode_utf16().collect();
            let caret = (caret_offset(&edit) as usize).min(utf16.len()) as u32;
            let before = String::from_utf16_lossy(&utf16[..caret as usize]);
            if let Some(hint) = &context_hint {
                hint(&before);
            }
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

/// 式エディタの可変寸法（DPI 換算前）を返す。
/// `(入力欄の高さ, ヒント行の y, 候補欄の上端 y, 候補欄の高さ)`。
/// compact＝1 行入力＋広い候補欄（コマンドを探しやすい）／expanded＝複数行入力＋下に候補欄
/// （凝った処理を書きやすい）。入力欄の左上・幅は両モード共通なので持たない。生成時の初期配置に使う
/// （以後のリサイズ・モード切替は [`relayout_code_box`] が動的に計算する）。
fn code_box_geometry(expanded: bool) -> (i32, i32, i32, i32) {
    if expanded { (150, 192, 222, 172) } else { (24, 66, 96, 306) }
}

/// 式エディタの子コントロールを、クライアント `cw`×`ch`（物理px）に合わせて配置し直す。リサイズ
/// （`wm_size`）とモード切替（複数行トグル）の両方から呼ぶ。compact＝入力 1 行＋候補欄が残りを占める／
/// expanded＝入力欄が伸び縮みして候補欄は下端固定。論理寸法は `gui::dpi_*` で物理へ換算する。
#[allow(clippy::too_many_arguments)]
fn relayout_code_box(
    label: &winsafe::HWND,
    mode: &winsafe::HWND,
    edit: &winsafe::HWND,
    hint: &winsafe::HWND,
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
    let hint_h = gui::dpi_y(18);
    let hint_gap = gui::dpi_y(4);

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

    // 中央：入力欄・シグネチャヒント行・候補欄。下端のボタン行より上の領域を分け合う。
    // ヒント行は常に入力欄の直下（編集中の呼び出しの引数ヘルプを出す場所）。
    let content_w = (cw - m * 2).max(1);
    let content_bottom = (btn_y - gap).max(edit_top);
    let min_pane = gui::dpi_y(40);
    let (edit_h, cand_y, cand_h) = if expanded {
        let cand_h = gui::dpi_y(150)
            .min((content_bottom - edit_top - hint_gap - hint_h - gap - min_pane).max(min_pane));
        let cand_y = content_bottom - cand_h;
        let edit_h = (cand_y - gap - hint_h - hint_gap - edit_top).max(min_pane);
        (edit_h, cand_y, cand_h)
    } else {
        let edit_h = gui::dpi_y(24);
        let cand_y = edit_top + edit_h + hint_gap + hint_h + gap;
        let cand_h = (content_bottom - cand_y).max(min_pane);
        (edit_h, cand_y, cand_h)
    };
    let hint_y = edit_top + edit_h + hint_gap;
    let _ = edit.MoveWindow(P { x: m, y: edit_top }, S { cx: content_w, cy: edit_h }, true);
    let _ = hint.MoveWindow(P { x: m, y: hint_y }, S { cx: content_w, cy: hint_h }, true);
    let _ = cand.MoveWindow(P { x: m, y: cand_y }, S { cx: content_w, cy: cand_h }, true);
}

pub fn code_box(
    parent: &impl GuiParent,
    message: &str,
    value: &str,
    members: &[CompletionMember],
    globals: &[String],
) -> Option<String> {
    // サイズ可変＋サイズ記憶（キー "code_box"・キー編集/メニュー編集で共有）。
    let (wnd, arm) = modal_window_resizable_keyed("コードを割り当て", "code_box", 560, 480, 360, 300);

    // 種の式が複数行を含めば展開モード、単一行ならコンパクトモードで開く（凝った式だけ広く使う）。
    let multiline_init = value.contains('\n');
    let (edit_h0, hint_y0, cand_y0, cand_h0) = code_box_geometry(multiline_init);

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
            width: gui::dpi_x(528),
            height: gui::dpi_y(edit_h0),
            ..Default::default()
        },
    );

    // 編集中の呼び出しのシグネチャヘルプ（常設・入力欄の直下）。カレットを囲う呼び出しが
    // 解決できたときだけ `spawn(‹cmd›, ...args)  説明` の形で入る。
    let hint = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            position: gui::dpi(16, hint_y0),
            size: gui::dpi(528, 18),
            ..Default::default()
        },
    );

    // `r.` 補完の候補リスト。既定は隠しておき、`r.<prefix>` を打つと候補を入れて表示する。
    // 項目のシングルクリックで、カレット直前のプレフィックスをその候補名に置換する。
    let cand = gui::ListBox::new(
        &wnd,
        gui::ListBoxOpts {
            position: gui::dpi(16, cand_y0),
            size: gui::dpi(528, cand_h0),
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
        let (label, mode, edit, hint, cand, ok, cancel) = (
            label.clone(),
            mode.clone(),
            edit.clone(),
            hint.clone(),
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
                    hint.hwnd(),
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
        let hint = hint.clone();
        let globals = globals.to_vec();
        ok.on().bn_clicked(move || {
            let code = edit.text().unwrap_or_default();
            // 保存しても実行時に落ちるだけの式（構文エラー・組込引数の不整合・未定義の名前）は
            // 閉じずにヒント行へエラーを出してカレットをその位置へ飛ばす（破棄して閉じるのは
            // キャンセル）。構文 → 意味の順で検査する。
            let error = syntax_error(&code)
                .map(|(msg, line, col)| (format!("構文エラー: {msg}"), line, col))
                .or_else(|| semantic_error(&code, &globals));
            if let Some((msg, line, col)) = error {
                let _ = hint.hwnd().SetWindowText(&msg);
                let pos = utf16_offset_at(&code, line, col) as i32;
                edit.set_selection(pos, pos);
                edit.hwnd().SetFocus();
                return Ok(());
            }
            *result.borrow_mut() = Some(code);
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
    // 補完モデル＝カレット直前が `r.`／`rerics.` のメンバアクセス、または式の先頭の裸の識別子
    // （fast-path の組込呼び出し＝組込だけに絞る）のとき、識別子に曖昧一致する候補をジャンル見出し
    // 付きで出す（`r.` 直後の空クエリは全件＝機能ブラウザを兼ねる）。`force` は Ctrl+Space＝
    // 「唯一かつ入力済みと同一」でも出すのに加え、空欄でも裸文脈の全組込を出す（空欄は勝手に
    // 開くとうるさいので force 限定）。
    let members_shared: Rc<Vec<CompletionMember>> = Rc::new(members.to_vec());
    let complete: CompleteFn = {
        let members = members_shared.clone();
        Rc::new(move |before, caret, force| {
            // メンバー補完（`r.`／`rerics.` のメンバアクセス・式の先頭の裸の識別子）。
            if let Some((plen, prefix, bare)) = completion_context(before) {
                if bare && prefix.is_empty() && !force {
                    return None;
                }
                let items = completion_items(&members, &prefix, bare);
                let names: Vec<&str> =
                    items.iter().filter(|i| i.selectable).map(|i| i.display.as_str()).collect();
                let only_exact = names.len() == 1 && names[0].eq_ignore_ascii_case(&prefix);
                if names.is_empty() || (!force && only_exact) {
                    return None;
                }
                return Some((caret - plen as u32, items));
            }
            // 引数の値補完（Enum の値・オプション Object のキー）。値をこれから書く位置
            // （手前が `(`／`,`／`{`＝引数・エントリの先頭）だけで出す＝書き終えた値の直後には
            // 出さない。
            let call = enclosing_call(before)?;
            let (plen, query) = value_prefix(before, call.in_string);
            let head: String = {
                let chars: Vec<char> = before.chars().collect();
                chars[..chars.len().saturating_sub(plen)].iter().collect()
            };
            if !matches!(head.trim_end().chars().last(), Some('(' | ',' | '{')) {
                return None;
            }
            let items = value_items(&call, &query);
            let only_exact = items.len() == 1
                && items[0].display.trim_matches('"').eq_ignore_ascii_case(&query);
            if items.is_empty() || (!force && only_exact) {
                return None;
            }
            Some((caret - plen as u32, items))
        })
    };
    // signature help＝カレットを囲う呼び出しのシグネチャをヒント行に出す（解決不能なら消す）。
    // 本文・カレットが動くたびに install_completion 側から呼ばれる。
    let context_hint: ContextHintFn = {
        let hint = hint.clone();
        let members = members_shared.clone();
        Rc::new(move |before: &str| {
            let text = enclosing_call(before)
                .and_then(|call| signature_help(&members, &call.name, call.arg))
                .unwrap_or_default();
            // 変化が無ければ触らない（キーストロークごとの再描画ちらつきを避ける）。
            if hint.hwnd().GetWindowText().unwrap_or_default() != text {
                let _ = hint.hwnd().SetWindowText(&text);
            }
        })
    };
    install_completion(&arm, &edit, &cand, complete, Some(context_hint));

    // headless 観測：現在のモード（1 行／複数行）と signature help のヒント行を読めるようにする。
    #[cfg(feature = "debug-server")]
    {
        let expanded = expanded.clone();
        completion_probe::set_multiline(Box::new(move || expanded.get()));
        let hint = hint.clone();
        completion_probe::set_hint(Box::new(move || hint.hwnd().GetWindowText().unwrap_or_default()));
    }

    let _ = wnd.show_modal(parent);
    keyhook::pop();
    #[cfg(feature = "debug-server")]
    completion_probe::clear();
    let _ = (cancel, cand, mode, expanded, hint);
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
    install_completion(&arm, &edit, &cand, complete, None);

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
        /// 式エディタの signature help（ヒント行）の現在文字列の読み取り。
        static HINT: RefCell<Option<Box<dyn Fn() -> String>>> = const { RefCell::new(None) };
    }

    pub fn set(p: Probe) {
        PROBE.with(|s| *s.borrow_mut() = Some(p));
    }
    pub fn clear() {
        PROBE.with(|s| *s.borrow_mut() = None);
        MULTILINE.with(|s| *s.borrow_mut() = None);
        HINT.with(|s| *s.borrow_mut() = None);
    }
    /// `code_box` が現在モード（複数行なら true）を読む関数を登録する。
    pub fn set_multiline(f: Box<dyn Fn() -> bool>) {
        MULTILINE.with(|s| *s.borrow_mut() = Some(f));
    }
    /// 式エディタが複数行モードか（式エディタが開いていなければ None）。
    pub fn multiline() -> Option<bool> {
        MULTILINE.with(|s| s.borrow().as_ref().map(|f| f()))
    }
    /// `code_box` が signature help のヒント行文字列を読む関数を登録する。
    pub fn set_hint(f: Box<dyn Fn() -> String>) {
        HINT.with(|s| *s.borrow_mut() = Some(f));
    }
    /// signature help のヒント行の現在文字列（式エディタが開いていなければ None）。
    pub fn hint() -> Option<String> {
        HINT.with(|s| s.borrow().as_ref().map(|f| f()))
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
    use super::{
        CallCtx, CompletionMember, completion_context, completion_items, completion_prefix,
        enclosing_call, match_rank, semantic_error, signature_help, syntax_error, utf16_offset_at,
        value_items,
    };
    use crate::script::ScriptCommand;

    #[test]
    fn semantic_check_catches_unresolved_and_builtin_misuse() {
        let globals: Vec<String> =
            ["r", "rerics", "JSON", "globalThis", "myHelper"].iter().map(|s| s.to_string()).collect();
        // 正しい形は素通し：組込 fast-path・r. 経由・グローバル関数・空文。
        assert_eq!(semantic_error("cursorUp()", &globals), None);
        assert_eq!(semantic_error(r#"r.spawn("x", { cwd: r.currentDir() })"#, &globals), None);
        assert_eq!(semantic_error("myHelper(1)", &globals), None);
        assert_eq!(semantic_error("", &globals), None);
        // 変数・関数の typo＝未解決識別子。
        let (msg, _, _) = semantic_error("r.spawn(aaa)", &globals).expect("aaa");
        assert!(msg.contains("aaa") && msg.contains("定義されていない"), "{msg}");
        // 組込をリテラル以外の引数で裸呼び＝エンジンでは未定義なので r. 経由へ誘導。
        let (msg, _, col) = semantic_error("cursorUp(aaa)", &globals).expect("bare builtin");
        assert!(msg.contains("r.cursorUp"), "誘導メッセージ: {msg}");
        assert_eq!(col, 1, "エラー位置は先頭の cursorUp");
        // 組込 fast-path の引数不整合（core の検証が効く）。
        let (msg, _, _) = semantic_error(r#"sort("nmae")"#, &globals).expect("enum");
        assert!(msg.contains("nmae"), "{msg}");
        // 宣言済みのローカルは未解決にならない。型注釈の型名も見ない。
        assert_eq!(semantic_error("const a = 1; r.log(a);", &globals), None);
        assert_eq!(semantic_error("const n: SomeType = null; r.log(n);", &globals), None);
    }

    #[test]
    fn syntax_check_reports_error_position_and_passes_valid_code() {
        // 正しい式・空文は素通し。
        assert!(syntax_error("cursorUp()").is_none());
        assert!(syntax_error(r#"r.spawn("x", { cwd: r.currentDir() })"#).is_none());
        assert!(syntax_error("").is_none());
        // 閉じ忘れ＝エラー（1 始まりの位置付き）。
        let (msg, line, col) = syntax_error(r#"r.spawn("x", "#).expect("unclosed");
        assert!(line >= 1 && col >= 1, "{msg} at {line}:{col}");
        // 複数行コードは 2 行目のエラー位置を指す。
        let (_, line, _) = syntax_error("const a = 1;\nconst = 2;").expect("bad decl");
        assert_eq!(line, 2);
        // 行・桁 → UTF-16 オフセット（CRLF・非 ASCII 混じり）。
        let text = "abc\r\nあいう";
        assert_eq!(utf16_offset_at(text, 1, 1), 0);
        assert_eq!(utf16_offset_at(text, 2, 2), 6);
        // 範囲外は末尾へクランプ。
        assert_eq!(utf16_offset_at(text, 9, 9), text.encode_utf16().count());
    }

    fn member(name: &str, callable: bool, arity: u32) -> CompletionMember {
        CompletionMember { name: name.to_string(), callable, arity, script: None }
    }

    #[test]
    fn enclosing_call_finds_innermost_call_and_arg_index() {
        let call = |s: &str| enclosing_call(s).map(|c| (c.name, c.arg));
        assert_eq!(call("r.spawn("), Some(("r.spawn".into(), 0)));
        assert_eq!(call(r#"r.spawn("x", "#), Some(("r.spawn".into(), 1)));
        // 文字列の中のカンマ・括弧は数えない（閉じていない文字列を入力中でも囲う呼び出しが取れる）。
        assert_eq!(call(r#"r.spawn("a, (b"#), Some(("r.spawn".into(), 0)));
        // オブジェクトリテラルの中のカンマは引数の区切りに数えない。
        assert_eq!(call(r#"r.spawn("x", {a: 1, b: 2"#), Some(("r.spawn".into(), 1)));
        // ネスト＝内側の呼び出しが勝ち、閉じたら外側へ戻る。
        assert_eq!(call("r.spawn(r.cursorName("), Some(("r.cursorName".into(), 0)));
        assert_eq!(call("r.spawn(r.cursorName(), "), Some(("r.spawn".into(), 1)));
        // 裸の組込呼び出し。
        assert_eq!(call("setCursorIndex("), Some(("setCursorIndex".into(), 0)));
        // 名前のない括弧（グルーピング）は呼び出しでない＝外側の呼び出しを返す。
        assert_eq!(call("r.spawn((1 + 2"), Some(("r.spawn".into(), 0)));
        // 呼び出しの外・呼び出しなし。
        assert_eq!(call("r.spawn()"), None);
        assert_eq!(call("abc"), None);
    }

    #[test]
    fn enclosing_call_tracks_object_and_string_state() {
        // Object リテラルの中＝キーの位置。
        let c = enclosing_call(r#"r.spawn("x", {"#).expect("object");
        assert!(c.in_object && !c.after_colon && !c.in_string);
        // `:` の後＝値の位置。`,` で次のエントリのキー位置へ戻る。
        let c = enclosing_call(r#"r.spawn("x", {cwd: "#).expect("value pos");
        assert!(c.in_object && c.after_colon);
        let c = enclosing_call(r#"r.spawn("x", {cwd: p, "#).expect("next key");
        assert!(c.in_object && !c.after_colon);
        // 文字列の入力途中。
        let c = enclosing_call(r#"r.sort(""#).expect("in string");
        assert!(c.in_string && !c.in_object);
        let c = enclosing_call(r#"r.sort("name""#).expect("closed string");
        assert!(!c.in_string);
    }

    #[test]
    fn value_items_offer_enum_values_and_option_keys() {
        let ctx = |name: &str, arg, in_object, after_colon| CallCtx {
            name: name.to_string(),
            arg,
            in_object,
            after_colon,
            in_string: false,
        };
        // 組込 Enum＝値をクォート付きで出し、前方一致で絞る。
        let vals: Vec<String> = value_items(&ctx("r.sort", 0, false, false), "")
            .into_iter()
            .map(|i| i.insert)
            .collect();
        assert!(vals.contains(&"\"name\"".to_string()), "sort の Enum 値: {vals:?}");
        let vals = value_items(&ctx("sort", 0, false, false), "na");
        assert_eq!(vals.len(), 1, "前方一致で絞る: {}", vals.len());
        assert_eq!(vals[0].insert, "\"name\"");
        // 組込 Options＝Object 内でキーを出す（doc 付き・挿入は `名前: `）。
        let keys = value_items(&ctx("cursorDown", 0, true, false), "");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].insert, "select: ");
        assert!(keys[0].detail.is_some(), "OptSpec.doc が説明に付く");
        // host API のオプション Object（spawn の RericsProcOptions）＝cwd キーが出る。
        let keys: Vec<String> = value_items(&ctx("r.spawn", 1, true, false), "")
            .into_iter()
            .map(|i| i.insert)
            .collect();
        assert!(keys.contains(&"cwd: ".to_string()), "spawn の cwd キー: {keys:?}");
        // host API の文字列リテラル union（compare の type）＝値が出る。
        let vals: Vec<String> = value_items(&ctx("r.compare", 0, false, false), "s")
            .into_iter()
            .map(|i| i.insert)
            .collect();
        assert!(vals.contains(&"\"sameDate\"".to_string()), "compare の union 値: {vals:?}");
        // `:` の後（値の位置）ではキーを出さない。Enum でない引数も出さない。
        assert!(value_items(&ctx("r.spawn", 1, true, true), "").is_empty());
        assert!(value_items(&ctx("setCursorIndex", 0, false, false), "").is_empty());
    }

    #[test]
    fn signature_help_resolves_host_builtin_and_script() {
        // host API（d.ts 由来）。rest 引数（...args）は溢れた位置でも強調される。
        let h = signature_help(&[], "r.spawn", 0).expect("spawn");
        assert!(h.starts_with("spawn(‹cmd›, ...args)"), "第0引数の強調: {h}");
        let h = signature_help(&[], "r.spawn", 5).expect("spawn rest");
        assert!(h.contains("‹...args›"), "rest の強調: {h}");
        // rerics. 表記・名前空間メンバも同じに引ける。
        assert!(signature_help(&[], "rerics.fs.readText", 0).is_some());
        // 組込＝強調中の引数の doc を説明に出す。
        let h = signature_help(&[], "r.setCursorIndex", 0).expect("builtin");
        assert!(h.contains("‹index›") && h.contains("移動先の位置"), "引数 doc: {h}");
        // 裸は fast-path＝組込だけを引き、host API は解決しない。
        assert!(signature_help(&[], "cursorUp", 0).is_some());
        assert!(signature_help(&[], "spawn", 0).is_none(), "裸の host API は解決しない");
        // スクリプトコマンド＝summary を出す。
        let mut sc = member("organize", true, 0);
        sc.script = Some(ScriptCommand {
            name: "organize".to_string(),
            label: None,
            genre: None,
            summary: Some("散らかりを整える".to_string()),
        });
        let h = signature_help(&[sc], "r.organize", 0).expect("script");
        assert!(h.contains("散らかりを整える"), "script summary: {h}");
        // 未知の名前・別オブジェクトのメソッドは None。
        assert!(signature_help(&[], "r.noSuch", 0).is_none());
        assert!(signature_help(&[], "foo.bar", 0).is_none());
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
        let items = completion_items(&members, "", false);
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
        let items = completion_items(&members, "cursor", false);
        assert!(items.iter().all(|i| i.selectable), "単一ジャンルは見出しなし");
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn fuzzy_match_pulls_strong_group_first() {
        // "cp" は cursorPath（頭文字一致）が copy（飛び石一致）より強い＝所属ジャンルごと上に来る。
        let members = vec![member("copy", true, 3), member("cursorPath", true, 0)];
        let items = completion_items(&members, "cp", false);
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
        let items = completion_items(&members, "", false);
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
        let top: Vec<String> = completion_items(&members, "cl", false)
            .into_iter()
            .filter(|i| i.selectable)
            .map(|i| i.display)
            .collect();
        assert_eq!(top, vec!["clearAll", "clipboard"]);
        // 名前空間配下（query にドット）はその階層だけ。
        let sub: Vec<String> = completion_items(&members, "clipboard.", false)
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
        let items = completion_items(&members, "", false);
        let labels: Vec<&str> =
            items.iter().filter(|i| !i.selectable).map(|i| i.display.as_str()).collect();
        assert_eq!(
            labels,
            vec!["── カーソル移動 ──", "── ファイル操作 ──"],
            "群がマージされる: {labels:?}"
        );
        // 和名でも引ける。
        let hit: Vec<String> = completion_items(&members, "整理", false)
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
    fn context_detects_bare_leading_identifier() {
        // 式の先頭の裸の識別子＝裸文脈（機能欄の fast-path 表記 `cursorUp()` を編集中）。
        assert_eq!(completion_context("cursorU"), Some((7, "cursorU".into(), true)));
        assert_eq!(completion_context("  curs"), Some((4, "curs".into(), true)));
        // 空文字も裸文脈（Ctrl+Space の全組込ブラウズ用。出すかは呼び手が force で判断）。
        assert_eq!(completion_context(""), Some((0, "".into(), true)));
        // `r.` メンバアクセスは従来どおり（裸=false）。
        assert_eq!(completion_context("=r.cur"), Some((3, "cur".into(), false)));
        // 先頭以外（手前に別のトークンや行がある）は文脈にしない。
        assert_eq!(completion_context("=cursorU"), None);
        assert_eq!(completion_context("const x = 1;\ncons"), None);
        assert_eq!(completion_context("bar.cur"), None);
    }

    #[test]
    fn bare_context_completes_builtins_only_with_builtin_meta() {
        let members = vec![
            member("cursorDown", true, 0),
            member("setCursorIndex", true, 0),
            member("copy", true, 3),  // host API と同名の組込（裸では組込が実体）
            member("spawn", true, 1), // host API のみ＝裸では呼べない
            member("fs", false, 0),   // オブジェクト＝裸では呼べない
        ];
        let items = completion_items(&members, "", true);
        let names: Vec<&str> =
            items.iter().filter(|i| i.selectable).map(|i| i.display.as_str()).collect();
        assert!(names.contains(&"cursorDown") && names.contains(&"copy"), "組込は出る: {names:?}");
        assert!(!names.contains(&"spawn") && !names.contains(&"fs"), "非組込は出ない: {names:?}");
        // 裸文脈の copy は fast-path＝組込が実体なので、host の宣言引数でなく組込メタ
        // （必須引数なし）でカレット位置を決める。
        let copy = items.iter().find(|i| i.display == "copy").expect("copy");
        assert_eq!(copy.insert, "copy()");
        assert_eq!(copy.caret_back, 0, "裸の copy は組込メタで引数なし扱い");
        let set = items.iter().find(|i| i.display == "setCursorIndex").expect("setCursorIndex");
        assert_eq!(set.caret_back, 1, "必須引数ありはカレットを括弧内へ");
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
