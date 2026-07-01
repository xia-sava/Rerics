//! PDF ページのラスタライズ。同梱の PDFium を実行時に動的ロードし、ページを PNG 画像へ
//! 変換する。ビューアは変換後の PNG を通常の画像として扱うので、PDF を「複数ページ＝複数
//! 画像の入れ物」として画像ビューアの前後送りに載せられる（`viewer_ctl` が結線する）。

use std::path::Path;
use pdfium_render::prelude::*;

/// 既定のラスタライズ幅（px）。ズーム追従は未対応のため、表示に十分な固定解像度で焼く。
pub const DEFAULT_RENDER_WIDTH: i32 = 1600;

thread_local! {
    /// GUI スレッドで唯一の PDFium。DLL バインドはプロセスで一度きり（再バインドは失敗する）。
    /// ロードできなければ `None`＝PDF 表示のみ無効（他機能には影響しない）。
    static PDFIUM: Option<Pdfium> = init_pdfium();
}

/// 実行ファイルと同じディレクトリの `pdfium.dll` を動的ロードする（`build.rs` が配置する）。
fn init_pdfium() -> Option<Pdfium> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let dll = Pdfium::pdfium_platform_library_name_at_path(&dir);
    Pdfium::bind_to_library(&dll).ok().map(Pdfium::new)
}

/// PDF のページ数を返す。開けない・PDFium 未ロードなら `Err`。
pub fn page_count(pdf: &Path) -> Result<usize, String> {
    PDFIUM.with(|p| {
        let pdfium = p.as_ref().ok_or_else(|| "PDFium を初期化できません".to_string())?;
        let doc = pdfium
            .load_pdf_from_file(pdf, None)
            .map_err(|e| format!("PDF を開けません: {e}"))?;
        Ok(doc.pages().len() as usize)
    })
}

/// `index`（0 始まり）ページを幅 `target_width` px でラスタライズし、`dest` へ PNG 保存する。
/// 縦長ページで高さが暴走しないよう幅の 2 倍を上限にする。
pub fn render_page(pdf: &Path, index: usize, target_width: i32, dest: &Path) -> Result<(), String> {
    PDFIUM.with(|p| {
        let pdfium = p.as_ref().ok_or_else(|| "PDFium を初期化できません".to_string())?;
        let doc = pdfium
            .load_pdf_from_file(pdf, None)
            .map_err(|e| format!("PDF を開けません: {e}"))?;
        let page = doc
            .pages()
            .get(index as PdfPageIndex)
            .map_err(|e| format!("{} ページ目を取得できません: {e}", index + 1))?;
        let config = PdfRenderConfig::new()
            .set_target_width(target_width)
            .set_maximum_height(target_width * 2);
        let image = page
            .render_with_config(&config)
            .map_err(|e| format!("ページを描画できません: {e}"))?
            .as_image()
            .map_err(|e| format!("画像に変換できません: {e}"))?;
        image
            .save(dest)
            .map_err(|e| format!("PNG を保存できません: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 ページ・内容なしの最小 PDF をバイト列で組み立てる。xref のオフセットを実バイト
    /// 位置から計算するので、厳密なパーサでも開ける。
    fn minimal_pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>",
        ];
        let mut buf = Vec::new();
        buf.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (i, body) in objects.iter().enumerate() {
            offsets.push(buf.len());
            buf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref_pos = buf.len();
        let size = objects.len() + 1;
        buf.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        buf.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        buf.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n")
                .as_bytes(),
        );
        buf
    }

    /// このテスト専用の一時ディレクトリ（プロセス ID で分離・使い捨て）。
    fn scratch_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rerics-pdf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn counts_pages_and_renders_minimal_pdf() {
        let dir = scratch_dir();
        let pdf = dir.join("min.pdf");
        std::fs::write(&pdf, minimal_pdf()).unwrap();

        assert_eq!(page_count(&pdf).unwrap(), 1);

        let png = dir.join("page0.png");
        render_page(&pdf, 0, DEFAULT_RENDER_WIDTH, &png).unwrap();

        let rendered = image::open(&png).expect("PNG として読み戻せる");
        assert_eq!(rendered.width() as i32, DEFAULT_RENDER_WIDTH);
        assert!(rendered.height() > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
