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

/// 引数部（括弧の中身）を JSON 互換リテラルの並びとして読む。空なら空配列、読めなければ `None`。
/// 単引用符やキー無しオブジェクトなど JSON でないリテラルは今は読めず `None`＝エンジンへ送る。
fn parse_args(inside: &str) -> Option<Vec<Value>> {
    let trimmed = inside.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    match serde_json::from_str(&format!("[{trimmed}]")).ok()? {
        Value::Array(items) => Some(items),
        _ => None,
    }
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
        // 単引用符やキー無しオブジェクトは今は JSON として読めない＝エンジンへ（後で対応）。
        assert_eq!(script("copy('single')"), "copy('single')");
        assert_eq!(script("cursorDown({ select: true })"), "cursorDown({ select: true })");
    }
}
