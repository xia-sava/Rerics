//! コマンドリファレンス（HTML ヘルプ）の生成。組込コマンドのメタデータ（`Command::meta()`＝core の
//! 単一ソース）と登録スクリプトの `summary` を、同じ表形式で 1 枚の HTML に束ねる。標準キーマップと
//! 現在のキーマップから各コマンドの割り当てキーを逆引きして、両方を併記する。

use std::collections::{BTreeMap, HashMap};

use rerics_core::{ArgSpec, ArgType, Call, Command, KeyMap};

use crate::key_editor::command_genre;
use crate::script::ScriptCommand;

/// 標準キーマップ群・現在キーマップ群・登録スクリプトから、コマンドリファレンスの HTML を作る。
/// `*_maps` は文脈（ファイラー／テキスト／画像）ごとのキーマップ。組込はジャンル順、スクリプトは
/// 末尾の専用節に並べる。各行に標準キーと現在キーを併記する。
pub fn help_html(default_maps: &[&KeyMap], current_maps: &[&KeyMap], scripts: &[ScriptCommand]) -> String {
    let def = sorted_index(keybind_index(default_maps));
    let cur = sorted_index(keybind_index(current_maps));

    // 組込コマンドをジャンル（index 順）にまとめる。
    let mut genres: BTreeMap<u8, (&'static str, Vec<Command>)> = BTreeMap::new();
    for cmd in Command::all() {
        let (gi, gn) = command_genre(cmd);
        genres.entry(gi).or_insert_with(|| (gn, Vec::new())).1.push(cmd);
    }

    let mut html = String::new();
    html.push_str(HEAD);
    html.push_str("<h1>Rerics コマンドリファレンス</h1>\n");
    html.push_str(
        "<p>機能を指定する場所（キー定義・メニュー）は式（コード）で、組込もスクリプトも \
         <code>r.コマンド名()</code> の形で呼べます。各行に標準キーと現在のキーを併記します。</p>\n",
    );

    for (gname, cmds) in genres.values() {
        html.push_str(&format!("<h2>{}</h2>\n", esc(gname)));
        html.push_str(TABLE_HEAD);
        for &cmd in cmds {
            let tok = cmd.as_token();
            let meta = cmd.meta();
            html.push_str(&row(
                &keys_cell(def.get(tok)),
                &keys_cell(cur.get(tok)),
                tok,
                cmd.display_name(),
                &desc_cell(meta.summary, meta.args, meta.examples),
            ));
        }
        html.push_str("</table>\n");
    }

    if !scripts.is_empty() {
        html.push_str("<h2>スクリプト</h2>\n");
        html.push_str(TABLE_HEAD);
        for sc in scripts {
            let label = sc.label.as_deref().unwrap_or("スクリプト実行");
            let summary = sc.summary.as_deref().unwrap_or("");
            html.push_str(&row(
                &keys_cell(def.get(&sc.name)),
                &keys_cell(cur.get(&sc.name)),
                &sc.name,
                label,
                &desc_cell(summary, &[], &[]),
            ));
        }
        html.push_str("</table>\n");
    }

    html.push_str(FOOT);
    html
}

/// キーマップ群から「コマンドトークン → 割り当てキー一覧」を逆引きする。複数文脈のキーマップを
/// 渡せば統合する。素呼び（組込＝引数の有無を問わずそのコマンド・スクリプト＝`name()` の bare）だけ拾う。
fn keybind_index(maps: &[&KeyMap]) -> HashMap<String, Vec<String>> {
    let mut idx: HashMap<String, Vec<String>> = HashMap::new();
    for km in maps {
        for (chord, expr) in km.to_string_map() {
            if let Some(tok) = bare_call_token(&expr) {
                idx.entry(tok).or_default().push(chord);
            }
        }
    }
    idx
}

/// 各トークンのキー一覧をソート・重複除去する（表示を安定させる）。
fn sorted_index(mut idx: HashMap<String, Vec<String>>) -> HashMap<String, Vec<String>> {
    for v in idx.values_mut() {
        v.sort();
        v.dedup();
    }
    idx
}

/// 式が単一コマンドの呼び出しなら、そのトークンを返す。組込はそのトークン（引数の有無は問わない）、
/// スクリプトは `r.name()` / `name()` の素呼びだけ拾う（引数つき・複文は対象外）。
fn bare_call_token(expr: &str) -> Option<String> {
    match Call::parse(expr) {
        Call::Builtin { command, .. } => Some(command.as_token().to_string()),
        Call::Script { source } => {
            let s = source.trim();
            let s = s.strip_prefix("r.").or_else(|| s.strip_prefix("rerics.")).unwrap_or(s);
            let s = s.strip_suffix("()")?;
            (!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                .then(|| s.to_string())
        }
    }
}

/// キー一覧のセル。空なら「—」。
fn keys_cell(chords: Option<&Vec<String>>) -> String {
    match chords {
        Some(cs) if !cs.is_empty() => {
            cs.iter().map(|c| format!("<kbd>{}</kbd>", esc(c))).collect::<Vec<_>>().join(" ")
        }
        _ => "<span class=\"none\">—</span>".to_string(),
    }
}

/// 説明セル＝1 行説明＋引数仕様＋使用例。
fn desc_cell(summary: &str, args: &[ArgSpec], examples: &[&str]) -> String {
    let mut s = esc(summary);
    if let Some(a) = arg_lines(args) {
        s.push_str(&a);
    }
    if !examples.is_empty() {
        let ex = examples.iter().map(|e| format!("<code>{}</code>", esc(e))).collect::<Vec<_>>();
        s.push_str(&format!("<div class=\"ex\">例: {}</div>", ex.join("　")));
    }
    s
}

/// 引数仕様の説明行。引数なしは `None`。名前付きオプションは各オプションを `名前?` で展開する。
fn arg_lines(args: &[ArgSpec]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for a in args {
        match a.ty {
            ArgType::Options(opts) => {
                for o in opts {
                    parts.push(format!("{}? — {}", esc(o.name), esc(o.doc)));
                }
            }
            _ => {
                let opt = if a.required { "" } else { "?" };
                parts.push(format!("{}{} — {}", esc(a.name), opt, esc(a.doc)));
            }
        }
    }
    Some(format!("<div class=\"args\">引数: {}</div>", parts.join("、")))
}

/// 1 行（標準キー／現在キー／コマンド〔トークン＋機能名〕／説明）。
fn row(def_keys: &str, cur_keys: &str, token: &str, name: &str, desc: &str) -> String {
    format!(
        "<tr><td class=\"k\">{def_keys}</td><td class=\"k\">{cur_keys}</td>\
         <td><code>{}</code><br><span class=\"name\">{}</span></td><td>{desc}</td></tr>\n",
        esc(token),
        esc(name),
    )
}

/// HTML テキストの最小エスケープ。
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

const TABLE_HEAD: &str = "<table>\n<tr><th>標準キー</th><th>現在キー</th><th>コマンド</th><th>説明</th></tr>\n";

const HEAD: &str = r#"<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Rerics コマンドリファレンス</title>
<style>
  body { font-family: "Yu Gothic UI", "Meiryo", system-ui, sans-serif; margin: 24px auto; max-width: 1000px; color: #222; line-height: 1.6; }
  h1 { font-size: 1.6rem; border-bottom: 2px solid #557; padding-bottom: 4px; }
  h2 { font-size: 1.15rem; margin-top: 1.8em; color: #335; border-left: 4px solid #88a; padding-left: 8px; }
  table { border-collapse: collapse; width: 100%; margin: 8px 0 16px; }
  th, td { border: 1px solid #ccc; padding: 5px 8px; text-align: left; vertical-align: top; }
  th { background: #eef; font-weight: 600; }
  td.k { white-space: nowrap; width: 12%; }
  kbd { background: #f4f4f8; border: 1px solid #bbb; border-bottom-width: 2px; border-radius: 4px; padding: 1px 6px; font-size: 0.85em; }
  code { background: #f4f4f8; border-radius: 3px; padding: 1px 4px; font-size: 0.9em; }
  .name { color: #555; font-size: 0.9em; }
  .args, .ex { color: #555; font-size: 0.86em; margin-top: 3px; }
  .none { color: #aaa; }
</style>
</head>
<body>
"#;

const FOOT: &str = "</body>\n</html>\n";

#[cfg(test)]
mod tests {
    use super::*;
    use rerics_core::KeyChord;

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).expect("chord")
    }

    #[test]
    fn help_lists_builtins_scripts_and_both_keybinds() {
        let def = KeyMap::default();
        // 現在＝cursorDown を Ctrl+J へ付け替えた版（標準と現在で割り当てが異なる状況を作る）。
        let mut cur = KeyMap::new();
        cur.bind_expr(chord("Ctrl+J"), "cursorDown()");
        let empty = KeyMap::new();
        let scripts = vec![ScriptCommand {
            name: "organize".to_string(),
            label: Some("整理".to_string()),
            genre: None,
            summary: Some("散らかりを整える".to_string()),
        }];

        let html = help_html(&[&def, &empty, &empty], &[&cur, &empty, &empty], &scripts);

        // 組込：トークン・説明・使用例が出る。
        assert!(html.contains("cursorDown"), "組込トークン");
        assert!(html.contains("カーソルを 1 つ下"), "組込の説明");
        assert!(html.contains("cursorDown({select:true})"), "使用例");
        // 値返しクエリのジャンルとトークンも並ぶ。
        assert!(html.contains("情報取得") && html.contains("markedCount"), "クエリ組込");
        // openHelp 自身もリファレンスに載る。
        assert!(html.contains("openHelp"), "openHelp コマンド");
        // 標準キーと現在キーの両列。
        assert!(html.contains("標準キー") && html.contains("現在キー"), "両キー列の見出し");
        // 現在キーに付け替え先（Ctrl+J）が出る。
        assert!(html.contains("Ctrl+J"), "現在キーに付け替え先が併記される");
        // スクリプトも同じ表形式で末尾の節に並ぶ。
        assert!(html.contains("<h2>スクリプト</h2>"), "スクリプト節");
        assert!(html.contains("organize") && html.contains("散らかりを整える"), "スクリプトの名前と説明");
    }

    #[test]
    fn help_escapes_html_metacharacters() {
        let empty = KeyMap::new();
        let scripts = vec![ScriptCommand {
            name: "danger".to_string(),
            label: None,
            genre: None,
            summary: Some("a < b & c > d".to_string()),
        }];
        let html = help_html(&[&empty, &empty, &empty], &[&empty, &empty, &empty], &scripts);
        assert!(html.contains("a &lt; b &amp; c &gt; d"), "説明の <&> がエスケープされる");
    }
}
