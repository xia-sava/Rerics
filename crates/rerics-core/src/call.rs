//! 機能欄に書かれた「式」を、その場で同期実行できる組込呼び出しか、エンジンへ丸投げする
//! スクリプトソースかへ振り分ける。キー定義・メニュー項目など「機能を指定する場所」は
//! どこも式（コード）で書け、ほとんどのバインドは単一の組込呼び出しなので、そこだけ
//! 構造化して fast-path 実行できるようにする。

use crate::input::Command;
use serde_json::Value;

/// 式の解釈結果。リテラル引数の単一組込呼び出しに簡約できれば [`Call::Builtin`]、
/// それ以外（ネスト呼び出し・未知の識別子・複雑な式）は [`Call::Script`] として
/// エンジンへ丸ごと渡す。
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// 組込コマンド＋リテラル引数。fast-path で同期実行できる。
    Builtin { command: Command, args: Vec<Value> },
    /// エンジンで評価する式ソース（ネスト・スクリプト関数・制御構文など）。
    Script { source: String },
}

impl Call {
    /// 機能欄の式文字列を解釈する。簡約できない式はソースのまま [`Call::Script`] になる。
    pub fn parse(expr: &str) -> Call {
        let src = expr.trim();
        parse_builtin(src).unwrap_or_else(|| Call::Script { source: src.to_owned() })
    }

    /// 式文字列へ戻す（表示・観測用）。`Builtin` は `name(args)`（引数は式表現）、
    /// `Script` はソースそのまま。
    pub fn to_expr(&self) -> String {
        match self {
            Call::Builtin { command, args } => {
                let parts: Vec<String> = args.iter().map(value_to_expr).collect();
                format!("{}({})", command.as_token(), parts.join(", "))
            }
            Call::Script { source } => source.clone(),
        }
    }
}

/// 組込 fast-path 呼び出しの引数をメタデータと突き合わせ、問題があれば人間向けの説明を返す。
/// 個数超過・必須引数の欠落・型（文字列/整数/真偽値）・Enum に無い値・Options に無いキーと
/// 値の型を検査する。メタが引数を宣言していないコマンド（`args` 空）は、実際は引数を受けても
/// 宣言が未整備のものと区別できないため検査しない。
pub fn validate_builtin_args(command: Command, args: &[Value]) -> Option<String> {
    use crate::input::ArgType;
    let meta = command.meta();
    if meta.args.is_empty() {
        return None;
    }
    let token = command.as_token();
    if args.len() > meta.args.len() {
        return Some(format!(
            "{token} の引数は最大 {} 個（{} 個渡されている）",
            meta.args.len(),
            args.len()
        ));
    }
    // スカラ型（文字列・整数・真偽値）と値の突き合わせ。`what` は「by」「オプション select」など。
    fn scalar_error(token: &str, what: &str, ty: &ArgType, v: &Value) -> Option<String> {
        let expected = match ty {
            ArgType::Str | ArgType::Path if !v.is_string() => "文字列",
            ArgType::Int if v.as_i64().is_none() => "整数",
            ArgType::Bool if !v.is_boolean() => "真偽値",
            _ => return None,
        };
        Some(format!("{token} の {what} は{expected}で指定する"))
    }
    for (i, spec) in meta.args.iter().enumerate() {
        let Some(v) = args.get(i) else {
            if spec.required {
                return Some(format!("{token} には引数 {} が必要", spec.name));
            }
            continue;
        };
        match &spec.ty {
            ArgType::Enum(vals) => match v.as_str() {
                Some(s) if vals.contains(&s) => {}
                Some(s) => {
                    return Some(format!(
                        "{token} の {} に \"{s}\" は無い（{} のいずれか）",
                        spec.name,
                        vals.join(" / ")
                    ));
                }
                None => {
                    return Some(format!(
                        "{token} の {} は文字列で指定する（{} のいずれか）",
                        spec.name,
                        vals.join(" / ")
                    ));
                }
            },
            ArgType::Options(opts) => {
                let keys = || opts.iter().map(|o| o.name).collect::<Vec<_>>().join(" / ");
                let Some(map) = v.as_object() else {
                    return Some(format!(
                        "{token} の {} は {{名前: 値}} 形のオプションで指定する（{}）",
                        spec.name,
                        keys()
                    ));
                };
                for (k, val) in map {
                    let Some(opt) = opts.iter().find(|o| o.name == k) else {
                        return Some(format!(
                            "{token} に {k} というオプションは無い（使えるのは {}）",
                            keys()
                        ));
                    };
                    if let Some(msg) =
                        scalar_error(token, &format!("オプション {k}"), &opt.ty, val)
                    {
                        return Some(msg);
                    }
                }
            }
            ty => {
                if let Some(msg) = scalar_error(token, spec.name, ty, v) {
                    return Some(msg);
                }
            }
        }
    }
    None
}

/// JSON 値を機能欄の式表記へ整形する。オブジェクトのキーは識別子ならクォートを外し
/// （`{select:true}`）、入力した名前付きオプションの見た目をそのまま保つ。
fn value_to_expr(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, val)| {
                    let key = if is_ident(k) { k.clone() } else { format!("{:?}", k) };
                    format!("{key}:{}", value_to_expr(val))
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(value_to_expr).collect();
            format!("[{}]", parts.join(", "))
        }
        other => other.to_string(),
    }
}

/// `name(args)` 形（先頭 `r.`/`rerics.` は省略記法として剥がす）を、リテラル引数の組込呼び出しへ
/// 簡約する。簡約できなければ `None`（呼び出し側で [`Call::Script`] 扱い）。
///
/// 判定は粗い段階：引数部に `(` を含めばネスト呼び出しとみなしてエンジンへ送る（文字列リテラル中の
/// `(` も拾ってしまうが、初期は割り切る。後で AST により厳密化する）。
fn parse_builtin(src: &str) -> Option<Call> {
    let body = src.strip_prefix("rerics.").or_else(|| src.strip_prefix("r.")).unwrap_or(src);
    let open = body.find('(')?;
    if !body.ends_with(')') {
        return None;
    }
    let name = body[..open].trim();
    if !is_ident(name) {
        return None;
    }
    let command = Command::from_token(name)?;
    let inside = &body[open + 1..body.len() - 1];
    if inside.contains('(') {
        return None;
    }
    let args = parse_args(inside)?;
    Some(Call::Builtin { command, args })
}

/// JS の識別子（先頭は英字/`_`/`$`・以降は英数/`_`/`$`）か。
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// 引数部（括弧の中身）をリテラルの並びとして読む。空なら空配列、読めなければ `None`。
/// 名前付きオプションの裸キー（`{select:true}`）は JSON へ寄せてから読む。単引用符など
/// 他の非 JSON 構文は今は読めず `None`＝エンジンへ送る。
fn parse_args(inside: &str) -> Option<Vec<Value>> {
    let trimmed = inside.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    let normalized = normalize_object_keys(trimmed);
    match serde_json::from_str(&format!("[{normalized}]")).ok()? {
        Value::Array(items) => Some(items),
        _ => None,
    }
}

/// JS オブジェクトリテラルの裸キー（`{ select: true }`）を JSON 互換（`{"select":true}`）へ
/// 寄せる。文字列リテラルの中身は触らない。`:` の直前にある識別子だけをキーとみなして
/// クォートする。単引用符など他の非 JSON 構文には手を出さない（読めなければ呼び出し側が
/// `Script` 扱いにする）。
fn normalize_object_keys(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            out.push(c);
            while let Some(d) = chars.next() {
                out.push(d);
                if d == '\\' {
                    if let Some(e) = chars.next() {
                        out.push(e);
                    }
                } else if d == '"' {
                    break;
                }
            }
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let mut ident = String::from(c);
            while let Some(&d) = chars.peek() {
                if d.is_ascii_alphanumeric() || d == '_' || d == '$' {
                    ident.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            let mut ws = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_whitespace() {
                    ws.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            if chars.peek() == Some(&':') {
                out.push('"');
                out.push_str(&ident);
                out.push('"');
            } else {
                out.push_str(&ident);
            }
            out.push_str(&ws);
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn builtin(s: &str) -> (Command, Vec<Value>) {
        match Call::parse(s) {
            Call::Builtin { command, args } => (command, args),
            Call::Script { source } => panic!("Builtin のはずが Script: {source}"),
        }
    }

    fn script(s: &str) -> String {
        match Call::parse(s) {
            Call::Script { source } => source,
            Call::Builtin { command, .. } => panic!("Script のはずが Builtin: {command:?}"),
        }
    }

    #[test]
    fn bare_call_is_builtin() {
        assert_eq!(builtin("cursorDown()"), (Command::CursorDown, vec![]));
        // 余分な空白も許容。
        assert_eq!(builtin("  reload()  "), (Command::Reload, vec![]));
    }

    #[test]
    fn r_prefix_is_stripped() {
        assert_eq!(builtin("r.cursorDown()"), (Command::CursorDown, vec![]));
        assert_eq!(builtin("rerics.cursorDown()"), (Command::CursorDown, vec![]));
    }

    #[test]
    fn literal_args_parse_as_json() {
        assert_eq!(builtin(r#"copy("a", "b")"#), (Command::Copy, vec![json!("a"), json!("b")]));
        assert_eq!(
            builtin("setCursorPosition(3, true)"),
            (Command::SetCursorPosition, vec![json!(3), json!(true)])
        );
    }

    #[test]
    fn nested_call_goes_to_script() {
        // 引数に呼び出しがある＝fast-path 対象外。式ソースのまま。
        assert_eq!(script("copy(curDir())"), "copy(curDir())");
    }

    #[test]
    fn unknown_identifier_goes_to_script() {
        // 組込に無い名前＝スクリプト関数（か未定義）。core からは未知なのでエンジンへ。
        assert_eq!(script("myFunc()"), "myFunc()");
    }

    #[test]
    fn non_call_goes_to_script() {
        // 括弧呼び出しでない式・`()` 無しの裸識別子（＝プロパティ参照）はエンジンへ。
        assert_eq!(script("1 + 2"), "1 + 2");
        assert_eq!(script("cursorDown"), "cursorDown");
    }

    #[test]
    fn non_json_literal_goes_to_script() {
        // 単引用符は今は JSON として読めない＝エンジンへ（後で対応）。
        assert_eq!(script("copy('single')"), "copy('single')");
    }

    #[test]
    fn named_option_object_parses_as_builtin() {
        // 裸キーのオブジェクトリテラル＝名前付きオプションは fast-path の組込呼び出しに残す。
        assert_eq!(
            builtin("cursorDown({ select: true })"),
            (Command::CursorDown, vec![json!({ "select": true })])
        );
        assert_eq!(
            builtin("markToggle({cursorMove:-1})"),
            (Command::MarkToggle, vec![json!({ "cursorMove": -1 })])
        );
    }

    #[test]
    fn named_option_roundtrips_through_to_expr() {
        // to_expr はキーのクォートを外し、再パースで同じ Call に戻る（裸キーの見た目を保つ）。
        let expr = Call::Builtin {
            command: Command::CursorDown,
            args: vec![json!({ "select": true })],
        }
        .to_expr();
        assert_eq!(expr, "cursorDown({select:true})");
        assert_eq!(builtin(&expr), (Command::CursorDown, vec![json!({ "select": true })]));
    }

    #[test]
    fn validate_checks_count_types_enum_and_option_keys() {
        let ok = |s: &str| {
            let (cmd, args) = builtin(s);
            validate_builtin_args(cmd, &args)
        };
        // 正しい呼び出しは素通し。
        assert_eq!(ok("cursorDown()"), None);
        assert_eq!(ok("cursorDown({select: true})"), None);
        assert_eq!(ok(r#"sort("name")"#), None);
        assert_eq!(ok("setCursorIndex(3)"), None);
        // 個数超過（cursorDown はオプション 1 つまで）。
        assert!(ok("cursorDown(1, 2)").unwrap().contains("最大"), "個数超過");
        // 必須引数の欠落。
        assert!(ok("setCursorIndex()").unwrap().contains("必要"), "必須欠落");
        // 型違い（Int にオブジェクト・Options に数値）。
        assert!(ok("setCursorIndex(true)").unwrap().contains("整数"), "Int 型違い");
        assert!(ok("cursorDown(3)").unwrap().contains("オプション"), "Options 型違い");
        // Enum に無い値。
        let msg = ok(r#"sort("nmae")"#).expect("enum error");
        assert!(msg.contains("nmae") && msg.contains("name"), "候補を示す: {msg}");
        // Options に無いキー・値の型違い。
        let msg = ok("cursorDown({selct: true})").expect("unknown key");
        assert!(msg.contains("selct") && msg.contains("select"), "使えるキーを示す: {msg}");
        assert!(
            ok("cursorDown({select: 1})").unwrap().contains("真偽値"),
            "オプション値の型違い"
        );
        // メタが引数を宣言しないコマンド（copy 等）は検査しない。
        assert_eq!(ok(r#"copy("a", "b")"#), None);
    }

    #[test]
    fn colon_inside_string_is_not_treated_as_key() {
        // 文字列リテラル中の識別子＋`:` をキーと誤認しない。
        assert_eq!(
            builtin(r#"changeDirectory("C:\\tmp")"#),
            (Command::ChangeDirectory, vec![json!("C:\\tmp")])
        );
    }
}
