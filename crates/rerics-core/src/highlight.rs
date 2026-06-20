//! テキストの構文ハイライト（syntect ラッパ）。
//!
//! 拡張子から言語を判定し、組み込みテーマ（ダーク/ライト）の前景色だけを取り出す。
//! 背景色はビューア側のユーザ設定を使うため、ここでは前景トークン色のみ返す。

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

use crate::Rgb;

/// 行末改行を含まない行向けの構文セット（`split_lines` の出力に合う）。
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_nonewlines)
}

fn themes() -> &'static ThemeSet {
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    THEMES.get_or_init(ThemeSet::load_defaults)
}

/// 拡張子から判定される言語名（ハイライト対象なら `Some`）。プレーンテキスト・未知は `None`。
pub fn syntax_name(ext: &str) -> Option<String> {
    let syntax = syntaxes().find_syntax_by_extension(ext)?;
    if syntax.name == "Plain Text" {
        return None;
    }
    Some(syntax.name.clone())
}

/// 1ファイル分の行ハイライタ。行を順に渡すと前景色付きの文字列を返す（状態を持つ）。
pub struct Highlighter {
    inner: HighlightLines<'static>,
}

impl Highlighter {
    /// 拡張子から言語を、テーマフラグ（dark/light）から配色を選んで作る。
    /// 言語を判定できなければ `None`（＝ハイライトしない）。
    pub fn for_extension(ext: &str, dark: bool) -> Option<Self> {
        let ss = syntaxes();
        let syntax = ss.find_syntax_by_extension(ext)?;
        // プレーンテキストはハイライトしない（ユーザ設定の本文色をそのまま使う）。
        if syntax.name == "Plain Text" {
            return None;
        }
        let theme_name = if dark { "base16-ocean.dark" } else { "InspiredGitHub" };
        let theme = themes().themes.get(theme_name)?;
        Some(Self { inner: HighlightLines::new(syntax, theme) })
    }

    /// 1行をハイライトし、各文字に前景色を付けて返す。失敗時は全文字を `None`（既定色）に。
    pub fn highlight_line(&mut self, line: &str) -> Vec<(char, Option<Rgb>)> {
        match self.inner.highlight_line(line, syntaxes()) {
            Ok(spans) => spans
                .into_iter()
                .flat_map(|(style, text)| {
                    let fg = style.foreground;
                    let rgb = Rgb::new(fg.r, fg.g, fg.b);
                    text.chars().map(move |ch| (ch, Some(rgb)))
                })
                .collect(),
            Err(_) => line.chars().map(|ch| (ch, None)).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extension_highlights_keywords() {
        let mut h = Highlighter::for_extension("rs", true).expect("rust syntax exists");
        let styled = h.highlight_line("fn main() {}");
        // 全文字に色が付く（行全体がカバーされる）。
        assert_eq!(styled.iter().filter(|(_, c)| c.is_some()).count(), styled.len());
        assert_eq!(styled.iter().map(|(c, _)| c).collect::<String>(), "fn main() {}");
        // キーワード fn と識別子 main などで色が分かれる（全部同じ色ではない）。
        let first = styled[0].1;
        assert!(styled.iter().any(|(_, c)| *c != first), "複数のトークン色が出るはず");
    }

    #[test]
    fn unknown_extension_is_none() {
        assert!(Highlighter::for_extension("no_such_ext_xyz", true).is_none());
    }
}
