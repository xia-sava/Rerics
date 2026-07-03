//! 埋め込みの `rerics.d.ts` から host API のシグネチャ（パラメータ名列と 1 行説明）を抽出する。
//! 式エディタの signature help（編集中の呼び出しの常設ヒント）が実行時に引く。手書きの型定義を
//! 唯一の正本としてパースするので、API を足すときに別表を保守しなくてよい。

use std::collections::HashMap;
use std::sync::OnceLock;

/// エディタ補完用型定義の原本（起動時に `scripts/rerics.d.ts` として配るのと同じ内容）。
pub(crate) const HOST_DTS: &str = include_str!("../../../scripting/rerics.d.ts");

/// host API メソッドのパラメータ 1 つ。`name` は表示名（`...args`・`options?` の修飾込み・
/// 型注釈は落とす）、`ty` は型注釈の生文字列（オプションキー・Enum 値の補完が引く）。
pub(crate) struct HostParam {
    pub name: String,
    pub ty: String,
}

/// host API メンバー 1 件のシグネチャ。`summary` は JSDoc の最初の一文。
pub(crate) struct HostSig {
    pub params: Vec<HostParam>,
    pub summary: String,
}

/// パース済み d.ts の表。メソッドシグネチャと、interface 名→プロパティ（キー名, 1 行説明）の
/// 一覧（オプション Object のキー補完用）。
struct Tables {
    sigs: HashMap<String, HostSig>,
    iface_props: HashMap<String, Vec<(String, String)>>,
}

fn tables() -> &'static Tables {
    static TABLE: OnceLock<Tables> = OnceLock::new();
    TABLE.get_or_init(|| parse_dts(HOST_DTS))
}

/// `name`（`spawn`・`fs.readText` 等）の host API シグネチャを引く。初回に d.ts をパースして保持。
pub(crate) fn host_sig(name: &str) -> Option<&'static HostSig> {
    tables().sigs.get(name)
}

/// パラメータ型の文字列から、オプション Object のキー候補（キー名, 説明）を引く。型に現れる
/// interface 名（`RericsProcOptions` 等）のプロパティと、インライン Object 型
/// （`{ selectAll?: boolean }`）のキーを合わせて返す。該当なしは空。
pub(crate) fn option_keys(ty: &str) -> Vec<(String, String)> {
    let t = tables();
    let mut out: Vec<(String, String)> = Vec::new();
    // 型中の識別子で interface 表に載っているもののプロパティを集める。
    for word in ty.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if let Some(props) = t.iface_props.get(word) {
            for (name, doc) in props {
                if !out.iter().any(|(n, _)| n == name) {
                    out.push((name.clone(), doc.clone()));
                }
            }
        }
    }
    // インライン Object 型（`{ key?: 型; ... }`）のキー。
    if let Some(open) = ty.find('{') {
        let inner = ty[open + 1..].split('}').next().unwrap_or("");
        for piece in inner.split([';', ',']) {
            let name = piece.split(':').next().unwrap_or("").trim().trim_end_matches('?').trim();
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !out.iter().any(|(n, _)| n == name)
            {
                out.push((name.to_string(), String::new()));
            }
        }
    }
    out
}

/// パラメータ型の文字列から、文字列リテラル union（`"name" | "sameDate" | …`）の値を引く。
/// 該当なしは空。
pub(crate) fn enum_values(ty: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = ty;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else { break };
        let v = &after[..close];
        if !v.is_empty() {
            out.push(v.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

/// interface のメンバー 1 件。メソッド（パラメータ列）かプロパティ（型の先頭識別子・無ければ空）。
struct Member {
    name: String,
    kind: MemberKind,
    summary: String,
}

enum MemberKind {
    Method(Vec<HostParam>),
    Prop(String),
}

/// d.ts 全体からパース済み表を組む。シグネチャは `RericsApi` のメソッドを最上位、プロパティ
/// （`fs: RericsFs` 等）で参照される interface のメソッドを `fs.xxx` 形で登録する。
/// 全 interface のプロパティ一覧はオプション Object のキー補完用に別表で持つ。
fn parse_dts(src: &str) -> Tables {
    let ifaces = parse_interfaces(src);
    let mut sigs = HashMap::new();
    if let Some(api) = ifaces.get("RericsApi") {
        for m in api {
            match &m.kind {
                MemberKind::Method(params) => {
                    insert_widest(&mut sigs, m.name.clone(), params, &m.summary)
                }
                MemberKind::Prop(ty) => {
                    if let Some(subs) = ifaces.get(ty.as_str()) {
                        for s in subs {
                            if let MemberKind::Method(params) = &s.kind {
                                insert_widest(
                                    &mut sigs,
                                    format!("{}.{}", m.name, s.name),
                                    params,
                                    &s.summary,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let iface_props = ifaces
        .iter()
        .map(|(name, members)| {
            let props: Vec<(String, String)> = members
                .iter()
                .filter(|m| matches!(m.kind, MemberKind::Prop(_)))
                .map(|m| (m.name.clone(), m.summary.clone()))
                .collect();
            (name.clone(), props)
        })
        .collect();
    Tables { sigs, iface_props }
}

/// 同名メソッド（オーバーロード）はパラメータの多い宣言を採る（`copy()` と
/// `copy(items, dest, options?)` なら後者＝引数を書くときのヒントとして情報が多い方）。
fn insert_widest(
    out: &mut HashMap<String, HostSig>,
    name: String,
    params: &[HostParam],
    summary: &str,
) {
    let better = out.get(&name).is_none_or(|cur| cur.params.len() < params.len());
    if better {
        let params = params
            .iter()
            .map(|p| HostParam { name: p.name.clone(), ty: p.ty.clone() })
            .collect();
        out.insert(name, HostSig { params, summary: summary.to_string() });
    }
}

/// d.ts を行走査して interface ごとのメンバー一覧を組む。JSDoc は直後のメンバーの summary に
/// なる（最初の内容行の一文）。宣言は `;` で終わるまで複数行を連結する（union やオブジェクト型が
/// 行をまたぐため）。interface の終わりは単独の `}` 行（この d.ts の整形前提・テストで守る）。
fn parse_interfaces(src: &str) -> HashMap<String, Vec<Member>> {
    let mut out: HashMap<String, Vec<Member>> = HashMap::new();
    let mut cur: Option<(String, Vec<Member>)> = None;
    let mut in_doc = false;
    let mut summary = String::new();
    let mut decl = String::new();
    for line in src.lines() {
        let t = line.trim();
        if in_doc {
            if summary.is_empty() {
                let content = t.trim_start_matches('*').trim();
                if !content.is_empty() {
                    summary = clean_doc(content);
                }
            }
            if t.ends_with("*/") {
                in_doc = false;
            }
            continue;
        }
        if let Some(doc) = t.strip_prefix("/**") {
            // 1 行完結（`/** 説明。 */`）と複数行の両対応。
            match doc.strip_suffix("*/") {
                Some(one) => summary = clean_doc(one.trim()),
                None => {
                    in_doc = true;
                    summary.clear();
                }
            }
            continue;
        }
        if t.starts_with("//") || t.is_empty() {
            continue;
        }
        match &mut cur {
            None => {
                if let Some(rest) = t.strip_prefix("interface ") {
                    let name = rest
                        .split(|c: char| c.is_whitespace() || c == '{')
                        .next()
                        .unwrap_or("")
                        .to_string();
                    cur = Some((name, Vec::new()));
                    summary.clear();
                }
            }
            Some((_, members)) => {
                if decl.is_empty() && t == "}" {
                    let (name, members) = cur.take().expect("interface open");
                    out.insert(name, members);
                    continue;
                }
                if !decl.is_empty() {
                    decl.push(' ');
                }
                decl.push_str(t);
                // 宣言の終わり＝括弧が閉じた上での `;`。
                if decl.ends_with(';') && paren_balanced(&decl) {
                    if let Some(m) = parse_member(&decl, &summary) {
                        members.push(m);
                    }
                    decl.clear();
                    summary.clear();
                }
            }
        }
    }
    out
}

/// `()`/`[]`/`{}`/`<>` の対応が取れているか（宣言の複数行連結の終了判定）。
/// アロー型（`=>`）の `>` は括弧でないので数えない。
fn paren_balanced(s: &str) -> bool {
    let mut d = 0i32;
    let mut prev = '\0';
    for c in s.chars() {
        match c {
            '(' | '[' | '{' | '<' => d += 1,
            '>' if prev == '=' => {}
            ')' | ']' | '}' | '>' => d -= 1,
            _ => {}
        }
        prev = c;
    }
    d == 0
}

/// 連結済みのメンバー宣言 1 件をパースする。`name(params): ret;`（ジェネリクス `<...>` は
/// 読み飛ばす）ならメソッド、`name: Type;` ならプロパティ。どちらでもなければ None。
fn parse_member(decl: &str, summary: &str) -> Option<Member> {
    let decl = decl.strip_prefix("readonly ").unwrap_or(decl);
    let name: String = decl.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    if name.is_empty() {
        return None;
    }
    let rest = decl[name.len()..].trim_start();
    // ジェネリクス（`parallel<R = unknown>(...)`）は `(` の手前まで読み飛ばす。
    let rest = if let Some(r) = rest.strip_prefix('<') {
        let mut d = 1i32;
        let mut idx = 0;
        for (i, c) in r.char_indices() {
            match c {
                '<' => d += 1,
                '>' => d -= 1,
                _ => {}
            }
            if d == 0 {
                idx = i + 1;
                break;
            }
        }
        r[idx..].trim_start()
    } else {
        rest
    };
    if let Some(params) = rest.strip_prefix('(') {
        // 対応する `)` までがパラメータ列。
        let mut d = 1i32;
        let mut end = params.len();
        for (i, c) in params.char_indices() {
            match c {
                '(' => d += 1,
                ')' => d -= 1,
                _ => {}
            }
            if d == 0 {
                end = i;
                break;
            }
        }
        return Some(Member {
            name,
            kind: MemberKind::Method(param_names(&params[..end])),
            summary: summary.to_string(),
        });
    }
    let rest = rest.strip_prefix('?').unwrap_or(rest).trim_start();
    let ty = rest.strip_prefix(':')?.trim().trim_end_matches(';').trim();
    // 型の先頭識別子（`fs: RericsFs` の名前空間解決用）。関数型など識別子で始まらなければ空の
    // まま持つ（プロパティ名はオプションキー補完が使うので落とさない）。
    let ty_name: String = ty.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    Some(Member { name, kind: MemberKind::Prop(ty_name), summary: summary.to_string() })
}

/// パラメータ列を最上位のカンマで割り、各パラメータの表示名（型注釈より前・`...`/`?` 込み）と
/// 型注釈の生文字列を返す。アロー型（`=>`）の `>` は括弧でないので数えない。
fn param_names(params: &str) -> Vec<HostParam> {
    let mut out = Vec::new();
    let mut d = 0i32;
    let mut prev = '\0';
    let mut piece = String::new();
    for c in params.chars() {
        match c {
            '(' | '[' | '{' | '<' => d += 1,
            '>' if prev == '=' => {}
            ')' | ']' | '}' | '>' => d -= 1,
            ',' if d == 0 => {
                push_param(&mut out, &piece);
                piece.clear();
                prev = c;
                continue;
            }
            _ => {}
        }
        piece.push(c);
        prev = c;
    }
    push_param(&mut out, &piece);
    out
}

/// パラメータ 1 片から表示名（`: 型` より前）と型文字列を取り出して積む（空は無視）。
fn push_param(out: &mut Vec<HostParam>, piece: &str) {
    let (name, ty) = match piece.split_once(':') {
        Some((n, t)) => (n.trim(), t.trim()),
        None => (piece.trim(), ""),
    };
    if !name.is_empty() {
        out.push(HostParam { name: name.to_string(), ty: ty.to_string() });
    }
}

/// JSDoc の 1 行から表示用の説明を作る。マークダウン修飾（`**`・バッククォート）と
/// `{@link X}` を落とし、最初の一文（`。` まで）に切り詰める。
fn clean_doc(line: &str) -> String {
    let mut s = line.replace("**", "").replace('`', "");
    while let Some(i) = s.find("{@link ") {
        let Some(j) = s[i..].find('}') else { break };
        let inner = s[i + "{@link ".len()..i + j].to_string();
        s.replace_range(i..=i + j, &inner);
    }
    match s.find('。') {
        Some(i) => s[..i + '。'.len_utf8()].to_string(),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(sig: &HostSig) -> Vec<&str> {
        sig.params.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn extracts_top_level_and_namespaced_signatures() {
        let sig = host_sig("spawn").expect("spawn");
        assert_eq!(names(sig), vec!["cmd", "...args"]);
        assert!(sig.summary.contains("外部プログラムを起動"), "summary: {}", sig.summary);
        let sig = host_sig("fs.readText").expect("fs.readText");
        assert_eq!(names(sig), vec!["path"]);
        let sig = host_sig("clipboard.setText").expect("clipboard.setText");
        assert_eq!(names(sig), vec!["text"]);
        // 引数なし。
        assert!(host_sig("modifiers").expect("modifiers").params.is_empty());
        // 存在しない名前は None。
        assert!(host_sig("noSuchApi").is_none());
    }

    #[test]
    fn multiline_and_optional_params_are_parsed() {
        // 複数行宣言（union が行をまたぐ）。
        let sig = host_sig("compare").expect("compare");
        assert_eq!(names(sig), vec!["type"]);
        // 省略可（`?`）とオブジェクト型パラメータ。
        let sig = host_sig("prompt").expect("prompt");
        assert_eq!(names(sig), vec!["message", "defaultValue?", "options?"]);
        // ジェネリクス付き宣言。
        let sig = host_sig("parallel").expect("parallel");
        assert_eq!(names(sig), vec!["fn", "arg?"]);
    }

    #[test]
    fn overloads_take_widest_declaration() {
        let sig = host_sig("copy").expect("copy");
        assert_eq!(names(sig), vec!["items", "dest", "options?"]);
        let sig = host_sig("delete").expect("delete");
        assert_eq!(names(sig), vec!["items", "options?"]);
    }

    #[test]
    fn option_keys_resolve_interfaces_and_inline_objects() {
        // interface 参照（spawn の rest 引数型に現れる RericsProcOptions）。
        let ty = &host_sig("spawn").expect("spawn").params[1].ty;
        let keys = option_keys(ty);
        assert!(keys.iter().any(|(n, _)| n == "cwd"), "cwd キー: {keys:?}");
        // RericsOpOptions＝関数型プロパティ（onProgress）も名前が拾える。
        let ty = &host_sig("copy").expect("copy").params[2].ty;
        let keys = option_keys(ty);
        assert!(keys.iter().any(|(n, _)| n == "onProgress"), "onProgress キー: {keys:?}");
        // インライン Object 型（prompt の options）。
        let ty = &host_sig("prompt").expect("prompt").params[2].ty;
        let keys = option_keys(ty);
        assert!(keys.iter().any(|(n, _)| n == "selectAll"), "selectAll キー: {keys:?}");
        // Object でない型は空。
        assert!(option_keys("string").is_empty());
    }

    #[test]
    fn enum_values_extract_string_literal_unions() {
        let ty = &host_sig("compare").expect("compare").params[0].ty;
        let vals = enum_values(ty);
        assert!(vals.contains(&"name".to_string()) && vals.contains(&"notExists".to_string()), "{vals:?}");
        assert!(enum_values("string").is_empty());
    }

    #[test]
    fn summaries_are_single_sentence_without_markup() {
        let s = &host_sig("confirm").expect("confirm").summary;
        assert_eq!(s, "確認ダイアログ（はい/いいえ）を出す。");
        // {@link} は名前だけ残す。
        let s = &host_sig("oppositePane").expect("oppositePane").summary;
        assert!(!s.contains("{@link"), "link 除去: {s}");
    }
}
