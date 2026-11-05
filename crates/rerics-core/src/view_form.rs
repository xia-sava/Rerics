//! ビューアの表示形式（レンダリング表示・テキスト・バイナリ）の判定と切り替え順。
//!
//! ファイルごとに「レンダリング表示できるか」「テキスト表示があるか」を持ち、バイナリ表示は
//! 全ファイルが持つ（他の形式が無いファイルもバイナリ表示で開ける）。最初に開く形式は、持っている
//! 形式のうち優先順（レンダリング表示 → テキスト → バイナリ）で先頭のもので、切り替えはこの順に巡回する。

use std::path::Path;

use crate::archive::is_known_archive;
use crate::media::MediaKind;
use crate::viewer::looks_text;

/// レンダリング表示の種類。どの描画部品で見せるかを決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderKind {
    /// 画像・アニメーション・動画。
    Media(MediaKind),
    /// PDF（ページを画像へ描いて見せる）。
    Pdf,
    /// Markdown（HTML へ変換して見せる）。
    Markdown,
    /// HTML。
    Html,
}

impl RenderKind {
    /// ソースをテキストとしても読めるか（Markdown・HTML）。
    fn has_source_text(self) -> bool {
        matches!(self, RenderKind::Markdown | RenderKind::Html)
    }
}

/// ビューアの表示形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewForm {
    /// 形式を解釈して描いた表示（種類は [`ViewForms::render`]）。
    Rendered,
    /// 文字コードを解釈したテキスト表示。
    Text,
    /// バイト列をそのまま見せるバイナリ表示。
    Binary,
}

impl ViewForm {
    /// 巡回順（上ほど優先して最初に開く）。
    const ORDER: [ViewForm; 3] = [ViewForm::Rendered, ViewForm::Text, ViewForm::Binary];

    fn rank(self) -> usize {
        Self::ORDER.iter().position(|f| *f == self).unwrap_or(0)
    }

    /// debug-server 等で使う識別名。
    pub fn token(self) -> &'static str {
        match self {
            ViewForm::Rendered => "rendered",
            ViewForm::Text => "text",
            ViewForm::Binary => "binary",
        }
    }
}

/// 表示の並べ方。キー定義の表をどれにするかを決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewLayout {
    /// 長い本文を上下にスクロールして読む（テキスト・バイナリ・Markdown・HTML）。
    Scroll,
    /// 1 枚ずつ送る（画像・動画・PDF）。
    Paged,
}

/// 1 ファイルが持つ表示形式の組。バイナリ表示は常に持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewForms {
    /// レンダリング表示の種類（できないなら `None`）。
    pub render: Option<RenderKind>,
    /// テキスト表示を持つか。
    pub text: bool,
}

impl ViewForms {
    /// ファイル名と先頭のバイト列から表示形式の組を決める。`head` は種類が名前だけで
    /// 決まらないファイルのテキスト判定に使い、`None` ならテキストを持つとみなす。
    pub fn detect(name: &str, head: Option<&[u8]>) -> ViewForms {
        if let Some(render) = render_kind(name) {
            return ViewForms { render: Some(render), text: render.has_source_text() };
        }
        if is_known_archive(Path::new(name)) {
            return ViewForms { render: None, text: false };
        }
        let text = head.is_none_or(looks_text);
        ViewForms { render: None, text }
    }

    /// 内容を読まずに表示形式が決まるか（レンダリング表示・書庫）。決まらなければ
    /// 先頭のバイト列を読んで [`ViewForms::detect`] へ渡す。
    pub fn needs_head(name: &str) -> bool {
        render_kind(name).is_none() && !is_known_archive(Path::new(name))
    }

    /// その形式を持つか。
    pub fn has(&self, form: ViewForm) -> bool {
        match form {
            ViewForm::Rendered => self.render.is_some(),
            ViewForm::Text => self.text,
            ViewForm::Binary => true,
        }
    }

    /// 最初に開く形式（持っている形式のうち優先順で先頭のもの）。
    pub fn initial(&self) -> ViewForm {
        ViewForm::ORDER.into_iter().find(|f| self.has(*f)).unwrap_or(ViewForm::Binary)
    }

    /// `cur` の次に巡回する形式。`cur` を持っていなくても巡回順の位置から次を選ぶ。
    pub fn next(&self, cur: ViewForm) -> ViewForm {
        let n = ViewForm::ORDER.len();
        let start = cur.rank();
        (1..=n)
            .map(|k| ViewForm::ORDER[(start + k) % n])
            .find(|f| self.has(*f))
            .unwrap_or(cur)
    }

    /// 指定の形式を持てばそれを、持たなければ最初に開く形式を返す。
    pub fn resolve(&self, preferred: Option<ViewForm>) -> ViewForm {
        match preferred {
            Some(f) if self.has(f) => f,
            _ => self.initial(),
        }
    }

    /// `form` で表示しているときの並べ方。
    pub fn layout(&self, form: ViewForm) -> ViewLayout {
        match (form, self.render) {
            (ViewForm::Rendered, Some(RenderKind::Media(_) | RenderKind::Pdf)) => ViewLayout::Paged,
            _ => ViewLayout::Scroll,
        }
    }
}

/// 名前（拡張子）からレンダリング表示の種類を引く。
fn render_kind(name: &str) -> Option<RenderKind> {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("");
    if let Some(kind) = MediaKind::from_extension(ext) {
        return Some(RenderKind::Media(kind));
    }
    match ext.to_ascii_lowercase().as_str() {
        "pdf" => Some(RenderKind::Pdf),
        "md" | "markdown" => Some(RenderKind::Markdown),
        "html" | "htm" => Some(RenderKind::Html),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_renders_then_binary() {
        let f = ViewForms::detect("a.PNG", None);
        assert_eq!(f.render, Some(RenderKind::Media(MediaKind::Image)));
        assert!(!f.text);
        assert_eq!(f.initial(), ViewForm::Rendered);
        assert_eq!(f.next(ViewForm::Rendered), ViewForm::Binary);
        assert_eq!(f.next(ViewForm::Binary), ViewForm::Rendered);
        assert_eq!(f.layout(ViewForm::Rendered), ViewLayout::Paged);
        assert_eq!(f.layout(ViewForm::Binary), ViewLayout::Scroll);
    }

    #[test]
    fn pdf_renders_then_binary() {
        let f = ViewForms::detect("doc.pdf", None);
        assert_eq!(f.render, Some(RenderKind::Pdf));
        assert_eq!(f.initial(), ViewForm::Rendered);
        assert_eq!(f.next(ViewForm::Rendered), ViewForm::Binary);
        assert!(!ViewForms::needs_head("doc.pdf"));
    }

    #[test]
    fn text_file_is_text_then_binary() {
        let f = ViewForms::detect("note.txt", Some(b"hello\n"));
        assert_eq!(f, ViewForms { render: None, text: true });
        assert_eq!(f.initial(), ViewForm::Text);
        assert_eq!(f.next(ViewForm::Text), ViewForm::Binary);
        assert_eq!(f.next(ViewForm::Binary), ViewForm::Text);
        assert!(ViewForms::needs_head("note.txt"));
    }

    #[test]
    fn binary_content_has_only_binary() {
        let f = ViewForms::detect("blob.dat", Some(&[0u8, 1, 2, 3]));
        assert!(!f.text);
        assert_eq!(f.initial(), ViewForm::Binary);
        // 巡回先が無ければその場に留まる。
        assert_eq!(f.next(ViewForm::Binary), ViewForm::Binary);
    }

    #[test]
    fn markdown_renders_then_text_then_binary() {
        let f = ViewForms::detect("README.md", None);
        assert_eq!(f, ViewForms { render: Some(RenderKind::Markdown), text: true });
        assert_eq!(f.initial(), ViewForm::Rendered);
        assert_eq!(f.next(ViewForm::Rendered), ViewForm::Text);
        assert_eq!(f.next(ViewForm::Text), ViewForm::Binary);
        assert_eq!(f.next(ViewForm::Binary), ViewForm::Rendered);
        // 文書のレンダリング表示はスクロールして読む。
        assert_eq!(f.layout(ViewForm::Rendered), ViewLayout::Scroll);
        assert!(!ViewForms::needs_head("README.md"));
    }

    #[test]
    fn html_renders_then_text() {
        let f = ViewForms::detect("index.HTM", None);
        assert_eq!(f.render, Some(RenderKind::Html));
        assert!(f.text);
        assert_eq!(f.initial(), ViewForm::Rendered);
    }

    #[test]
    fn utf16_with_bom_is_text() {
        // UTF-16 は NUL を含むが、BOM があればテキストとして扱う。
        let f = ViewForms::detect("wide.txt", Some(&[0xFF, 0xFE, b'a', 0, b'b', 0]));
        assert!(f.text);
    }

    #[test]
    fn archive_has_only_binary_without_reading() {
        assert!(!ViewForms::needs_head("pack.zip"));
        let f = ViewForms::detect("pack.zip", None);
        assert_eq!(f, ViewForms { render: None, text: false });
        assert_eq!(f.initial(), ViewForm::Binary);
    }

    #[test]
    fn binary_is_always_reachable() {
        for name in ["a.png", "b.mp4", "c.pdf", "d.txt", "e.zip"] {
            let f = ViewForms::detect(name, Some(b"x"));
            let mut cur = f.initial();
            let reached = (0..3).any(|_| {
                cur = f.next(cur);
                cur == ViewForm::Binary
            }) || f.initial() == ViewForm::Binary;
            assert!(reached, "{name} からバイナリ表示へ届く");
        }
    }

    #[test]
    fn next_from_missing_form_moves_on_in_order() {
        // 画像の表示に失敗してテキストで開いていても、巡回順の次（バイナリ）へ進む。
        let f = ViewForms::detect("a.png", None);
        assert_eq!(f.next(ViewForm::Text), ViewForm::Binary);
    }

    #[test]
    fn resolve_prefers_available_form() {
        let f = ViewForms::detect("note.txt", Some(b"hi"));
        assert_eq!(f.resolve(Some(ViewForm::Binary)), ViewForm::Binary);
        assert_eq!(f.resolve(Some(ViewForm::Rendered)), ViewForm::Text);
        assert_eq!(f.resolve(None), ViewForm::Text);
    }
}
