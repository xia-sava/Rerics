//! `Command::ALL` から TypeScript 型定義（組込コマンドの宣言）を生成する。
//!
//! エディタ補完用の `rerics.commands.d.ts` を起動時に書き出すのに使う。手書きの
//! `rerics.d.ts`（`interface RericsCommands {}`）へ宣言マージで合体する。

use crate::input::{ArgType, Command};

/// host API（bootstrap が `r`/`rerics` に直に生やすメンバー）と同名の組込コマンドは、実行時に
/// 組込側を生やさず host を優先する。型定義でも二重宣言を避けるためここで除外する。
const HOST_API_MEMBERS: &[&str] = &[
    "log", "currentDir", "navigate", "confirm", "prompt", "select", "listDir", "activePane",
    "oppositePane", "command", "copy", "move", "delete", "open", "folderDialog", "openDialog",
    "saveDialog", "modifiers", "spawn", "run", "unpack", "fs", "registerCommand", "registerMenu",
    "on",
];

/// `r`/`rerics` で組込側が優先される名前か（host API メンバー or 組込コマンド token）。
/// 登録スクリプトコマンドの型生成で、組込と重複する宣言を避けるのに使う。
pub fn reserved_member(name: &str) -> bool {
    HOST_API_MEMBERS.contains(&name) || Command::all().any(|c| c.as_token() == name)
}

/// 引数型を TypeScript 表記へ。`Options`（`{ select: true }` 等）は `r.` 経由では渡せない
/// （引数は文字列化される）ため `None` を返し、シグネチャから省く。
fn ts_type(ty: &ArgType) -> Option<String> {
    Some(match ty {
        ArgType::Str | ArgType::Path => "string".to_string(),
        ArgType::Int => "number".to_string(),
        ArgType::Bool => "boolean".to_string(),
        ArgType::Enum(vals) => vals
            .iter()
            .map(|v| format!("\"{v}\""))
            .collect::<Vec<_>>()
            .join(" | "),
        ArgType::Options(_) => return None,
    })
}

/// 1 コマンドのメソッド宣言（JSDoc 付き）を作る。
fn method_decl(cmd: Command) -> String {
    let meta = cmd.meta();
    let params: Vec<String> = meta
        .args
        .iter()
        .filter_map(|spec| {
            let t = ts_type(&spec.ty)?;
            let opt = if spec.required { "" } else { "?" };
            Some(format!("{}{}: {}", spec.name, opt, t))
        })
        .collect();
    let mut out = format!("  /**\n   * {}\n", meta.summary);
    // 例は `r.` から使えるスカラ形のみ（`{ ... }` オプション記法は除く）。
    for ex in meta.examples.iter().filter(|e| !e.contains('{')) {
        out.push_str(&format!("   * @example {ex}\n"));
    }
    out.push_str("   */\n");
    out.push_str(&format!("  {}({}): CommandResult;\n", cmd.as_token(), params.join(", ")));
    out
}

/// 組込コマンドの TypeScript 宣言（`interface RericsCommands { ... }`）を生成する。
/// 出力はトークン昇順で安定（内容比較での無駄な書き換えを避けるため）。
pub fn commands_dts() -> String {
    let mut commands: Vec<Command> = Command::all()
        .filter(|c| !HOST_API_MEMBERS.contains(&c.as_token()))
        .collect();
    commands.sort_by_key(|c| c.as_token());

    let mut out = String::new();
    out.push_str("// このファイルは Rerics が起動時に Command 一覧から自動生成する。手で編集しない。\n");
    out.push_str("// 組込コマンドを r.<名前>() / rerics.<名前>() で補完するための宣言。\n\n");
    out.push_str("interface RericsCommands {\n");
    for cmd in commands {
        out.push_str(&method_decl(cmd));
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_builtin_interface_with_typed_args() {
        let s = commands_dts();
        assert!(s.contains("interface RericsCommands {"), "interface 宣言: {s}");
        // 引数無しコマンド。
        assert!(s.contains("cursorDown(): CommandResult;"), "cursorDown: {s}");
        assert!(s.contains("centerCursor(): CommandResult;"), "centerCursor: {s}");
        // 整数引数のコマンド。
        assert!(
            s.contains("setCursorIndex(index: number): CommandResult;"),
            "typed int arg: {s}"
        );
        // host API と同名の組込は出さない。
        assert!(!s.contains("  copy("), "copy は host API なので除外: {s}");
        assert!(!s.contains("  delete("), "delete は host API なので除外: {s}");
        // JSDoc の summary が入る。
        assert!(s.contains("カーソルを 1 つ下の項目へ移動する"), "summary: {s}");
    }

    #[test]
    fn output_is_stable_and_sorted() {
        // 2 回生成して一致（書換え判定が安定する）。
        assert_eq!(commands_dts(), commands_dts());
    }
}
