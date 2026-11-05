//! Markdown / HTML をビューアのレンダリング表示に載せるための文書を組む。
//!
//! Markdown は GFM 拡張込みで HTML へ変換し、生の HTML は通さない。コードブロックは構文
//! ハイライトの色を付け、` ```mermaid ` は Mermaid が描く要素に置き換える。表示側は
//! [`DOC_HOST`]（開いたファイルのディレクトリ）と [`ASSET_HOST`]（同梱の CSS・スクリプト）の
//! 2 つの仮想ホストから資源を配り、それ以外への通信は CSP と表示側の両方で止める。

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

use comrak::adapters::{CodefenceRendererAdapter, SyntaxHighlighterAdapter};
use comrak::nodes::Sourcepos;
use comrak::options::Plugins;
use comrak::{Options, markdown_to_html_with_plugins};

use crate::Rgb;
use crate::highlight::Highlighter;

/// 開いたファイルのディレクトリを配る仮想ホスト。
pub const DOC_HOST: &str = "doc.rerics.invalid";
/// 同梱の CSS・スクリプトを配る仮想ホスト。
pub const ASSET_HOST: &str = "app.rerics.invalid";

/// レンダリング表示の配色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlTheme {
    pub background: Rgb,
    pub text: Rgb,
    /// 暗い背景か（構文ハイライトと Mermaid の配色を選ぶ）。
    pub dark: bool,
}

/// Markdown 文書に付ける CSP。同梱のスクリプト（Mermaid）だけを動かし、外部へは出さない。
pub fn markdown_csp() -> String {
    format!(
        "default-src 'none'; script-src https://{ASSET_HOST}; \
         style-src https://{ASSET_HOST} 'unsafe-inline'; \
         img-src https://{DOC_HOST} data:; font-src https://{ASSET_HOST} data:"
    )
}

/// HTML 文書に付ける CSP。ページのスクリプトは動かさず、開いたファイルのディレクトリ以外へは出さない。
pub fn html_csp() -> String {
    format!(
        "default-src 'none'; style-src https://{DOC_HOST} 'unsafe-inline'; \
         img-src https://{DOC_HOST} data:; font-src https://{DOC_HOST} data:; \
         media-src https://{DOC_HOST}"
    )
}

/// Markdown のソースを、レンダリング表示に載せる HTML 文書へ変換する。
pub fn markdown_document(source: &str, title: &str, theme: &HtmlTheme) -> String {
    let body = markdown_body(source, theme.dark);
    let scheme = if theme.dark { "dark" } else { "light" };
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\n\
         <title>{title}</title>\n\
         <link rel=\"stylesheet\" href=\"https://{ASSET_HOST}/markdown.css\">\n\
         <style>:root {{ --bg: {bg}; --fg: {fg}; color-scheme: {scheme}; }}</style>\n\
         </head><body class=\"{scheme}\"><article class=\"markdown-body\">\n{body}</article>\n\
         <script src=\"https://{ASSET_HOST}/mermaid.min.js\"></script>\n\
         <script src=\"https://{ASSET_HOST}/markdown.js\"></script>\n\
         </body></html>\n",
        csp = markdown_csp(),
        title = escape_html(title),
        bg = hex(theme.background),
        fg = hex(theme.text),
    )
}

/// Markdown のソースを HTML の断片へ変換する（GFM 拡張・生 HTML は通さない）。
fn markdown_body(source: &str, dark: bool) -> String {
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.alerts = true;
    options.render.r#unsafe = false;
    let highlighter = CodeHighlighter { dark };
    let mermaid = MermaidRenderer;
    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&highlighter);
    plugins.render.codefence_renderers.insert("mermaid".to_string(), &mermaid);
    markdown_to_html_with_plugins(source, &options, &plugins)
}

/// コードブロックへ構文ハイライトの色を付ける。
struct CodeHighlighter {
    dark: bool,
}

impl SyntaxHighlighterAdapter for CodeHighlighter {
    fn write_highlighted(
        &self,
        output: &mut dyn fmt::Write,
        lang: Option<&str>,
        code: &str,
    ) -> fmt::Result {
        let Some(mut h) = lang.and_then(|l| Highlighter::for_token(l, self.dark)) else {
            return output.write_str(&escape_html(code));
        };
        for line in code.split_inclusive('\n') {
            let (body, newline) = match line.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (line, ""),
            };
            write_colored_runs(output, &h.highlight_line(body))?;
            output.write_str(newline)?;
        }
        Ok(())
    }

    fn write_pre_tag(
        &self,
        output: &mut dyn fmt::Write,
        attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> fmt::Result {
        write_tag(output, "pre", attributes)
    }

    fn write_code_tag(
        &self,
        output: &mut dyn fmt::Write,
        attributes: HashMap<&'static str, Cow<'_, str>>,
    ) -> fmt::Result {
        write_tag(output, "code", attributes)
    }
}

/// 同じ色が続く文字をひとまとまりの `<span>` にして書く。
fn write_colored_runs(output: &mut dyn fmt::Write, chars: &[(char, Option<Rgb>)]) -> fmt::Result {
    let mut i = 0;
    while i < chars.len() {
        let color = chars[i].1;
        let end = chars[i..].iter().position(|(_, c)| *c != color).map_or(chars.len(), |n| i + n);
        let text: String = chars[i..end].iter().map(|(ch, _)| *ch).collect();
        match color {
            Some(c) => write!(output, "<span style=\"color:{}\">{}</span>", hex(c), escape_html(&text))?,
            None => output.write_str(&escape_html(&text))?,
        }
        i = end;
    }
    Ok(())
}

/// 開始タグを属性付きで書く（属性は名前順で決定的に並べる）。
fn write_tag(
    output: &mut dyn fmt::Write,
    name: &str,
    attributes: HashMap<&'static str, Cow<'_, str>>,
) -> fmt::Result {
    let mut attrs: Vec<_> = attributes.into_iter().collect();
    attrs.sort_by_key(|(k, _)| *k);
    write!(output, "<{name}")?;
    for (k, v) in attrs {
        write!(output, " {k}=\"{}\"", escape_html(&v))?;
    }
    output.write_str(">")
}

/// ` ```mermaid ` ブロックを Mermaid が描く要素に置き換える。
struct MermaidRenderer;

impl CodefenceRendererAdapter for MermaidRenderer {
    fn write(
        &self,
        output: &mut dyn fmt::Write,
        _lang: &str,
        _meta: &str,
        code: &str,
        _sourcepos: Option<Sourcepos>,
    ) -> fmt::Result {
        write!(output, "<pre class=\"mermaid\">{}</pre>\n", escape_html(code))
    }
}

/// HTML の特殊文字を実体参照にする。
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> HtmlTheme {
        HtmlTheme { background: Rgb::new(0, 0, 0), text: Rgb::new(255, 255, 255), dark: true }
    }

    #[test]
    fn gfm_table_tasklist_strikethrough_and_autolink() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n\n~~old~~ https://example.com\n";
        let html = markdown_body(src, false);
        assert!(html.contains("<table>"), "{html}");
        assert!(html.contains("type=\"checkbox\""), "{html}");
        assert!(html.contains("<del>old</del>"), "{html}");
        assert!(html.contains("<a href=\"https://example.com\">"), "{html}");
    }

    #[test]
    fn footnotes_render() {
        let html = markdown_body("text[^1]\n\n[^1]: note\n", false);
        assert!(html.contains("footnote"), "{html}");
    }

    #[test]
    fn raw_html_is_not_passed_through() {
        let html = markdown_body("<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n", false);
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("onerror"), "{html}");
    }

    #[test]
    fn mermaid_block_becomes_mermaid_element() {
        let html = markdown_body("```mermaid\ngraph TD; A-->B\n```\n", false);
        assert!(html.contains("<pre class=\"mermaid\">graph TD; A--&gt;B\n</pre>"), "{html}");
        assert!(!html.contains("<code"), "{html}");
    }

    #[test]
    fn code_block_is_highlighted_and_escaped() {
        let html = markdown_body("```rust\nfn main() { let a = 1 < 2; }\n```\n", true);
        assert!(html.contains("<span style=\"color:#"), "{html}");
        assert!(html.contains("&lt;"), "{html}");
        // 言語が分からないブロックは色を付けず、エスケープだけする。
        let plain = markdown_body("```nosuchlang\n<b>\n```\n", true);
        assert!(plain.contains("&lt;b&gt;"), "{plain}");
        assert!(!plain.contains("<span"), "{plain}");
    }

    #[test]
    fn document_carries_csp_theme_and_assets() {
        let doc = markdown_document("# T\n", "a<b>.md", &theme());
        assert!(doc.contains("Content-Security-Policy"));
        assert!(doc.contains(&markdown_csp()));
        assert!(doc.contains("<title>a&lt;b&gt;.md</title>"));
        assert!(doc.contains("--bg: #000000"));
        assert!(doc.contains("<body class=\"dark\">"));
        assert!(doc.contains(&format!("https://{ASSET_HOST}/mermaid.min.js")));
        assert!(doc.contains("<h1>T</h1>"));
    }

    #[test]
    fn csp_keeps_requests_on_own_hosts() {
        for csp in [markdown_csp(), html_csp()] {
            assert!(csp.starts_with("default-src 'none'"), "{csp}");
            assert!(!csp.contains("http://"), "{csp}");
            assert!(!csp.contains('*'), "{csp}");
        }
        // HTML ではページのスクリプトを動かさない。
        assert!(!html_csp().contains("script-src"));
    }
}
