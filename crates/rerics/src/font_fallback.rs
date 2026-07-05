//! 設定フォントに無いグリフの代替描画（フォントフォールバック）。
//!
//! 主フォントの cmap に無い文字は、Windows 既定のフォールバックだと SimSun 等の
//! セリフ体へ落ちることがある。ここでは設定 `font.fallback` のフォントを優先順に試し、
//! 文字列を「どのフォントで描くか」の run に分割して run ごとに HFONT を切り替えて
//! 描画・測定する。
//!
//! - フォールバック未設定、または全文字が主フォントで描ける場合は、従来どおり
//!   `DrawText`/`TextOut`/`GetTextExtentPoint32` へそのまま委譲する（fast path）。
//! - どのフォールバックにも無い文字は主フォントのまま描く＝従来のシステム任せ。
//! - グリフ有無は `GetGlyphIndicesW`（cmap 判定）で調べ、ファミリ名単位でキャッシュする。
//!   UTF-16 の 1 単位に収まらない文字（BMP 外）は判定できないので常に主フォント扱い。

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;

use winsafe::{self as w, co};

#[link(name = "gdi32")]
unsafe extern "system" {
    fn GetGlyphIndicesW(hdc: *mut c_void, lpstr: *const u16, c: i32, pgi: *mut u16, fl: u32)
        -> u32;
}

/// `GetGlyphIndicesW` に cmap へ無い文字を [`MISSING_GLYPH`] でマークさせるフラグ。
const GGI_MARK_NONEXISTING_GLYPHS: u32 = 1;
/// グリフ欠落を表す添字値。
const MISSING_GLYPH: u16 = 0xFFFF;
const GDI_ERROR: u32 = 0xFFFF_FFFF;

/// 一覧セルなどの省略記号。GDI の `DT_END_ELLIPSIS` は "..." 固定なので自前で詰める。
pub const ELLIPSIS: char = '…';

thread_local! {
    /// ファミリ名 → 文字 → グリフ有無。フォントの cmap は実行中不変とみなして
    /// プロセス生存中キャッシュする。
    static COVERAGE: RefCell<HashMap<String, HashMap<char, bool>>> =
        RefCell::new(HashMap::new());
}

/// DC に選択中のフォントが `ch`（BMP 内）のグリフを持つか。判定不能なら `None`。
fn glyph_exists(dc: &w::HDC, ch: char) -> Option<bool> {
    let unit = [ch as u16];
    let mut idx = [0u16];
    let r = unsafe {
        GetGlyphIndicesW(dc.ptr(), unit.as_ptr(), 1, idx.as_mut_ptr(), GGI_MARK_NONEXISTING_GLYPHS)
    };
    if r == GDI_ERROR {
        return None;
    }
    Some(idx[0] != MISSING_GLYPH)
}

/// `family`（`hfont` の生成元ファミリ）が `ch` のグリフを持つか。結果はファミリ名単位で
/// キャッシュする。判定不能なら `assume`（呼び出し側の安全側の値）を返し、キャッシュしない。
fn covers(dc: &w::HDC, family: &str, hfont: &w::HFONT, ch: char, assume: bool) -> bool {
    let cached = COVERAGE.with(|c| c.borrow().get(family).and_then(|m| m.get(&ch).copied()));
    if let Some(v) = cached {
        return v;
    }
    let Ok(_sel) = dc.SelectObject(hfont) else { return assume };
    let Some(v) = glyph_exists(dc, ch) else { return assume };
    COVERAGE.with(|c| c.borrow_mut().entry(family.to_owned()).or_default().insert(ch, v));
    v
}

/// `ch` を描くフォントの添字を選ぶ。ASCII・BMP 外は常に主フォント(0)。主フォントに
/// グリフが無ければフォールバックを順に試し、どれにも無ければ主フォントへ戻す。
fn pick_font(n_fonts: usize, ch: char, mut covers: impl FnMut(usize, char) -> bool) -> usize {
    if (ch as u32) < 0x80 || (ch as u32) > 0xFFFF {
        return 0;
    }
    if covers(0, ch) {
        return 0;
    }
    (1..n_fonts).find(|&i| covers(i, ch)).unwrap_or(0)
}

/// フォールバックのサイズ指定（主フォントサイズ基準の絶対値）を、コンポーネントの描画
/// サイズへ換算する。バー類は本文より小さいサイズで描くので、その差分を指定値にも同じ
/// だけ適用する。指定なしは描画サイズのまま。
pub fn effective_size(spec: Option<i32>, render_size: i32, main_size: i32) -> i32 {
    spec.map(|s| (s + render_size - main_size).max(6)).unwrap_or(render_size)
}

/// 文字ごとのフォント添字を、同添字の連なり（run）へまとめる。
fn group_runs(text: &str, idx: &[usize]) -> Vec<(usize, String)> {
    let mut runs: Vec<(usize, String)> = Vec::new();
    for (ch, &i) in text.chars().zip(idx) {
        match runs.last_mut() {
            Some((last, s)) if *last == i => s.push(ch),
            _ => runs.push((i, ch.to_string())),
        }
    }
    runs
}

/// 主フォント＋フォールバックの HFONT 一式。1 回の描画（`WM_PAINT`）の間だけ生きる。
pub struct FontSet {
    /// `[0]`=主フォント、`[1..]`=フォールバック（優先順）。ファミリ名と HFONT の対。
    fonts: Vec<(String, w::guard::DeleteObjectGuard<w::HFONT>)>,
    /// 主フォントの (tmHeight, tmAscent)。ベースライン合わせ用（初回測定でキャッシュ）。
    metrics: Cell<Option<(i32, i32)>>,
}

impl FontSet {
    /// `create`（ファミリ名＋サイズ指定 → HFONT。太さなどは呼び出し側の条件で固定）で
    /// 一式を組む。主フォントはサイズ指定なし（呼び出し側の描画サイズ）で作る。
    /// フォールバックは設定の `"ファミリ名[:サイズ]"` を分解して渡し、生成に失敗した
    /// ファミリは黙って除外する。
    pub fn new(
        primary_family: &str,
        fallback: &[String],
        create: impl Fn(&str, Option<i32>) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>>,
    ) -> w::SysResult<Self> {
        let mut fonts = vec![(primary_family.to_owned(), create(primary_family, None)?)];
        for entry in fallback {
            let (family, size) = rerics_core::FontSpec::parse_fallback_entry(entry);
            if family.is_empty() {
                continue;
            }
            if let Ok(f) = create(family, size) {
                fonts.push((family.to_owned(), f));
            }
        }
        Ok(Self { fonts, metrics: Cell::new(None) })
    }

    /// 主フォント。呼び出し側が DC へ選択して、描画・測定の既定フォントにする。
    pub fn primary(&self) -> &w::HFONT {
        &self.fonts[0].1
    }

    /// 文字ごとのフォント添字（`[0]`=主フォント）。フォールバック未設定または全文字が
    /// 主フォントなら `None`。セルグリッド描画（viewer）が run をさらに割るのに使う。
    pub fn char_fonts(&self, dc: &w::HDC, text: &str) -> Option<Vec<usize>> {
        if self.fonts.len() <= 1 {
            return None;
        }
        let pick = |ch| {
            pick_font(self.fonts.len(), ch, |i, c| {
                covers(dc, &self.fonts[i].0, &self.fonts[i].1, c, i == 0)
            })
        };
        let idx: Vec<usize> = text.chars().map(pick).collect();
        if idx.iter().all(|&i| i == 0) { None } else { Some(idx) }
    }

    /// 文字列を（フォント添字, 部分文字列）の run へ分割する。fast path なら `None`。
    fn runs(&self, dc: &w::HDC, text: &str) -> Option<Vec<(usize, String)>> {
        let idx = self.char_fonts(dc, text)?;
        Some(group_runs(text, &idx))
    }

    /// フォールバック込みの文字列幅（物理 px）。DC には主フォントを選択しておくこと
    /// （文字間隔などの DC 状態はそのまま測定へ効く）。
    pub fn width(&self, dc: &w::HDC, text: &str) -> i32 {
        match self.runs(dc, text) {
            None => dc.GetTextExtentPoint32(text).map(|z| z.cx).unwrap_or(0),
            Some(runs) => runs.iter().map(|(fi, s)| self.run_width(dc, *fi, s)).sum(),
        }
    }

    fn run_width(&self, dc: &w::HDC, fi: usize, s: &str) -> i32 {
        let Ok(_sel) = dc.SelectObject(&*self.fonts[fi].1) else { return 0 };
        dc.GetTextExtentPoint32(s).map(|z| z.cx).unwrap_or(0)
    }

    /// 主フォントの (行高, アセント)。フォント混在時のベースライン合わせに使う。
    fn primary_metrics(&self, dc: &w::HDC) -> (i32, i32) {
        if let Some(m) = self.metrics.get() {
            return m;
        }
        let m = (|| {
            let _sel = dc.SelectObject(self.primary()).ok()?;
            let tm = dc.GetTextMetrics().ok()?;
            Some((tm.tmHeight, tm.tmAscent))
        })()
        .unwrap_or((0, 0));
        self.metrics.set(Some(m));
        m
    }

    /// テキストを幅 `avail`（物理 px）に収める。収まればそのまま、超えるなら文字境界で
    /// 末尾を詰めて [`ELLIPSIS`] を付す（フォールバック込みの実測）。
    pub fn elide<'a>(&self, dc: &w::HDC, text: &'a str, avail: i32) -> Cow<'a, str> {
        if avail <= 0 {
            return Cow::Borrowed("");
        }
        if self.width(dc, text) <= avail {
            return Cow::Borrowed(text);
        }
        let chars: Vec<char> = text.chars().collect();
        let mut cut = chars.len();
        while cut > 0 {
            cut -= 1;
            let mut s: String = chars[..cut].iter().collect();
            s.push(ELLIPSIS);
            if self.width(dc, &s) <= avail {
                return Cow::Owned(s);
            }
        }
        Cow::Owned(ELLIPSIS.to_string())
    }

    /// パス風テキストを幅 `avail`（物理 px）に収める（`DT_PATH_ELLIPSIS` 相当）。最後の
    /// 区切り以降（ファイル名）を残し、前半を末尾側から詰めて [`ELLIPSIS`] を挟む。
    /// それでも収まらなければ末尾詰め（[`Self::elide`]）へ切り替える。
    pub fn elide_path<'a>(&self, dc: &w::HDC, text: &'a str, avail: i32) -> Cow<'a, str> {
        if avail <= 0 {
            return Cow::Borrowed("");
        }
        if self.width(dc, text) <= avail {
            return Cow::Borrowed(text);
        }
        let Some(sep) = text.rfind(['\\', '/']) else {
            return self.elide(dc, text, avail);
        };
        let tail = &text[sep..]; // 区切り込み
        let head: Vec<char> = text[..sep].chars().collect();
        let mut cut = head.len();
        loop {
            let mut s: String = head[..cut].iter().collect();
            s.push(ELLIPSIS);
            s.push_str(tail);
            if self.width(dc, &s) <= avail {
                return Cow::Owned(s);
            }
            if cut == 0 {
                break;
            }
            cut -= 1;
        }
        self.elide(dc, text, avail)
    }

    /// 一行テキストを `DrawText` 互換で描く。対応フラグは `VCENTER`/`RIGHT`/`CENTER`/
    /// `END_ELLIPSIS`/`PATH_ELLIPSIS`（プレフィクス解釈は常に無し＝`NOPREFIX` 相当）。
    /// fast path では `DrawText` へ委譲する。run 分割経路はクリップしないので、収まらない
    /// テキストは省略系フラグを併用するか呼び出し側で詰めること。
    pub fn draw_text(
        &self,
        dc: &w::HDC,
        text: &str,
        rect: w::RECT,
        flags: co::DT,
    ) -> w::AnyResult<()> {
        let text = if flags.has(co::DT::END_ELLIPSIS) {
            self.elide(dc, text, rect.right - rect.left)
        } else if flags.has(co::DT::PATH_ELLIPSIS) {
            self.elide_path(dc, text, rect.right - rect.left)
        } else {
            Cow::Borrowed(text)
        };
        let Some(runs) = self.runs(dc, &text) else {
            dc.DrawText(&text, rect, flags)?;
            return Ok(());
        };
        let total_w: i32 = runs.iter().map(|(fi, s)| self.run_width(dc, *fi, s)).sum();
        let (line_h, ascent) = self.primary_metrics(dc);
        let x = if flags.has(co::DT::RIGHT) {
            rect.right - total_w
        } else if flags.has(co::DT::CENTER) {
            rect.left + (rect.right - rect.left - total_w) / 2
        } else {
            rect.left
        };
        let top = if flags.has(co::DT::VCENTER) {
            rect.top + (rect.bottom - rect.top - line_h) / 2
        } else {
            rect.top
        };
        self.draw_runs(dc, &runs, x, top + ascent)
    }

    /// `TextOut` 互換（(x, y)=左上）。fast path ではそのまま `TextOut`。
    pub fn text_out(&self, dc: &w::HDC, x: i32, y: i32, text: &str) -> w::AnyResult<()> {
        let Some(runs) = self.runs(dc, text) else {
            dc.TextOut(x, y, text)?;
            return Ok(());
        };
        let (_, ascent) = self.primary_metrics(dc);
        self.draw_runs(dc, &runs, x, y + ascent)
    }

    /// フォント `fi` で 1 run を描く。(x, y) は主フォントの左上基準で、ベースラインを
    /// 主フォントへ合わせる。セルグリッド描画（viewer）用。
    pub fn text_out_at(&self, dc: &w::HDC, fi: usize, x: i32, y: i32, s: &str) -> w::AnyResult<()> {
        if fi == 0 {
            dc.TextOut(x, y, s)?;
            return Ok(());
        }
        let (_, ascent) = self.primary_metrics(dc);
        let prev = dc.SetTextAlign(co::TA::LEFT | co::TA::BASELINE)?;
        let sel = dc.SelectObject(&*self.fonts[fi].1);
        let r = if sel.is_ok() { dc.TextOut(x, y + ascent, s) } else { Ok(()) };
        drop(sel);
        let _ = dc.SetTextAlign(prev);
        r?;
        Ok(())
    }

    /// run 列をベースライン `baseline` に沿って `x` から順に描く。
    fn draw_runs(
        &self,
        dc: &w::HDC,
        runs: &[(usize, String)],
        mut x: i32,
        baseline: i32,
    ) -> w::AnyResult<()> {
        let prev = dc.SetTextAlign(co::TA::LEFT | co::TA::BASELINE)?;
        let mut result = Ok(());
        for (fi, s) in runs {
            let Ok(_sel) = dc.SelectObject(&*self.fonts[*fi].1) else { continue };
            if let Err(e) = dc.TextOut(x, baseline, s) {
                result = Err(e.into());
                break;
            }
            x += dc.GetTextExtentPoint32(s).map(|z| z.cx).unwrap_or(0);
        }
        let _ = dc.SetTextAlign(prev);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// フォント2つ（主+フォールバック1）で、フォールバック側だけが CJK 統合漢字を持つ想定。
    fn fake_covers(i: usize, ch: char) -> bool {
        match i {
            0 => !('\u{4E00}'..='\u{9FFF}').contains(&ch) || matches!(ch, '漢' | '字'),
            _ => ('\u{4E00}'..='\u{9FFF}').contains(&ch),
        }
    }

    #[test]
    fn pick_font_ascii_stays_primary() {
        // ASCII は coverage を見ずに主フォント。
        assert_eq!(pick_font(2, 'a', |_, _| false), 0);
    }

    #[test]
    fn pick_font_non_bmp_stays_primary() {
        assert_eq!(pick_font(2, '😀', |_, _| false), 0);
    }

    #[test]
    fn pick_font_prefers_primary_then_fallback() {
        assert_eq!(pick_font(2, '漢', fake_covers), 0);
        assert_eq!(pick_font(2, '简', fake_covers), 1);
    }

    #[test]
    fn pick_font_unknown_char_falls_back_to_primary() {
        // どのフォントにも無い文字は主フォント＝従来のシステム任せ。
        assert_eq!(pick_font(3, 'あ', |_, _| false), 0);
    }

    #[test]
    fn group_runs_merges_consecutive_same_font() {
        let text = "ab简体c";
        let idx = [0, 0, 1, 1, 0];
        assert_eq!(
            group_runs(text, &idx),
            vec![(0, "ab".to_owned()), (1, "简体".to_owned()), (0, "c".to_owned())]
        );
    }

    #[test]
    fn group_runs_empty() {
        assert!(group_runs("", &[]).is_empty());
    }

    #[test]
    fn effective_size_applies_render_delta() {
        // 指定なし＝描画サイズのまま。
        assert_eq!(effective_size(None, 12, 14), 12);
        // 本文（描画サイズ＝メイン）は指定値そのまま。
        assert_eq!(effective_size(Some(12), 14, 14), 12);
        // バー類（メイン-2）は指定値にも同じ差分を適用する。
        assert_eq!(effective_size(Some(12), 12, 14), 10);
        // 極端に小さくなっても下限で止める。
        assert_eq!(effective_size(Some(4), 6, 14), 6);
    }
}
