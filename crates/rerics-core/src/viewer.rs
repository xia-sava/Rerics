//! テキスト/バイナリビューアの表示モデル（UI 非依存）。
//!
//! バイト列を現在のエンコーディングでデコードして折返し済みのテキスト行に、または
//! バイナリダンプ行に整形する。デコードは lossy＝壊れたバイトは U+FFFD で表示し続ける
//! （文字化けしながらでも落ちずに見られる）。GUI は得られた [`DisplayLine`] 列を描くだけ。

use encoding_rs::Encoding as RsEncoding;
use unicode_width::UnicodeWidthChar;

/// ビューアが扱うエンコーディング（原作の 6 種を踏襲。自動判定はしない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    ShiftJis,
    EucJp,
    Utf16Le,
    Utf16Be,
    Iso2022Jp,
}

impl Encoding {
    /// 切替の循環順。
    pub const CYCLE: [Encoding; 6] = [
        Encoding::Utf8,
        Encoding::ShiftJis,
        Encoding::EucJp,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Iso2022Jp,
    ];

    /// 表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::ShiftJis => "Shift_JIS",
            Encoding::EucJp => "EUC-JP",
            Encoding::Utf16Le => "UTF-16LE",
            Encoding::Utf16Be => "UTF-16BE",
            Encoding::Iso2022Jp => "ISO-2022-JP",
        }
    }

    /// 循環順で次（`forward=false` なら前）へ。
    pub fn cycle(self, forward: bool) -> Encoding {
        let i = Encoding::CYCLE.iter().position(|&e| e == self).unwrap_or(0);
        let n = Encoding::CYCLE.len();
        let j = if forward { (i + 1) % n } else { (i + n - 1) % n };
        Encoding::CYCLE[j]
    }

    fn rs(self) -> &'static RsEncoding {
        match self {
            Encoding::Utf8 => encoding_rs::UTF_8,
            Encoding::ShiftJis => encoding_rs::SHIFT_JIS,
            Encoding::EucJp => encoding_rs::EUC_JP,
            Encoding::Utf16Le => encoding_rs::UTF_16LE,
            Encoding::Utf16Be => encoding_rs::UTF_16BE,
            Encoding::Iso2022Jp => encoding_rs::ISO_2022_JP,
        }
    }

    /// バイト列を lossy デコードする（不正バイトは U+FFFD）。
    pub fn decode(self, bytes: &[u8]) -> String {
        let (cow, _, _) = self.rs().decode(bytes);
        cow.into_owned()
    }
}

/// 先頭の一定範囲を見て、テキストとして表示できそうになければ true（バイナリ）。
/// NUL を含むか、どのエンコーディングで解釈しても表示不可文字が多いならバイナリとみなす（軽量判定）。
pub fn looks_binary(bytes: &[u8]) -> bool {
    const SNIFF: usize = 8192;
    let chunk = &bytes[..bytes.len().min(SNIFF)];
    if chunk.is_empty() {
        return false;
    }
    // NUL はテキストにはまず無い強い信号。
    if chunk.contains(&0) {
        return true;
    }
    // バイト指向のエンコーディングで解釈し、最も「テキストらしい」解釈でも表示不可文字が
    // 多いならバイナリ。UTF-16 はどんなバイト対もコードポイントに化けて判定が緩むため除外する
    // （UTF-16 テキストは ASCII 部が NUL を含むので上の NUL 信号で拾える＝git 流）。
    let min_bad = Encoding::CYCLE
        .iter()
        .filter(|e| !matches!(e, Encoding::Utf16Le | Encoding::Utf16Be))
        .map(|e| undisplayable_ratio(&e.decode(chunk)))
        .fold(1.0f32, f32::min);
    min_bad > 0.30
}

/// 文字列中の「表示できない文字」（制御文字・U+FFFD・DEL）の割合。タブ/改行/復帰は除く。
fn undisplayable_ratio(s: &str) -> f32 {
    let mut total = 0usize;
    let mut bad = 0usize;
    for ch in s.chars() {
        total += 1;
        let undisplayable = match ch {
            '\t' | '\n' | '\r' => false,
            '\u{FFFD}' | '\u{7F}' => true,
            _ => (ch as u32) < 0x20,
        };
        if undisplayable {
            bad += 1;
        }
    }
    if total == 0 { 0.0 } else { bad as f32 / total as f32 }
}

/// 表示モード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Text,
    Binary,
}

/// 1 表示行。`gutter`＝左端の行番号 or オフセット（折返し継続行は空）、`body`＝本文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLine {
    pub gutter: String,
    pub body: String,
}

/// ビューアの表示モデル。バイト列＋現在のエンコーディング＋モードを保持する。
pub struct ViewerModel {
    pub bytes: Vec<u8>,
    pub encoding: Encoding,
    pub mode: ViewMode,
}

impl ViewerModel {
    /// 既定（UTF-8・テキストモード）で作る。
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes, encoding: Encoding::Utf8, mode: ViewMode::Text }
    }

    /// ファイルを開く。バイナリらしければバイナリモードで開始する（原作准拠）。
    pub fn open(bytes: Vec<u8>) -> Self {
        let mode = if looks_binary(&bytes) { ViewMode::Binary } else { ViewMode::Text };
        Self { bytes, encoding: Encoding::Utf8, mode }
    }

    /// エンコーディングを循環切替する。
    pub fn cycle_encoding(&mut self, forward: bool) {
        self.encoding = self.encoding.cycle(forward);
    }

    /// テキスト/バイナリを切替する。
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            ViewMode::Text => ViewMode::Binary,
            ViewMode::Binary => ViewMode::Text,
        };
    }

    /// 表示行を生成する。`wrap_cols`＝テキストの折返し表示幅（全角=2）、`tab_width`＝タブ幅。
    /// バイナリモードでは両引数は無視する。
    pub fn lines(&self, wrap_cols: usize, tab_width: usize) -> Vec<DisplayLine> {
        match self.mode {
            ViewMode::Text => self.text_lines(wrap_cols, tab_width),
            ViewMode::Binary => self.binary_lines(),
        }
    }

    fn text_lines(&self, wrap_cols: usize, tab_width: usize) -> Vec<DisplayLine> {
        let text = self.encoding.decode(&self.bytes);
        let tab_width = tab_width.max(1);
        let mut out = Vec::new();
        let mut lineno = 0usize;
        for logical in split_lines(&text) {
            lineno += 1;
            let segments = wrap_line(logical, wrap_cols, tab_width);
            for (i, seg) in segments.into_iter().enumerate() {
                let gutter = if i == 0 { lineno.to_string() } else { String::new() };
                out.push(DisplayLine { gutter, body: seg });
            }
        }
        if out.is_empty() {
            out.push(DisplayLine { gutter: "1".to_owned(), body: String::new() });
        }
        out
    }

    fn binary_lines(&self) -> Vec<DisplayLine> {
        let mut out = Vec::new();
        for (i, chunk) in self.bytes.chunks(16).enumerate() {
            let offset = i * 16;
            let mut hex = String::with_capacity(52);
            for j in 0..16 {
                if j == 8 {
                    hex.push_str("- ");
                }
                if j < chunk.len() {
                    hex.push_str(&format!("{:02X} ", chunk[j]));
                } else {
                    hex.push_str("   ");
                }
            }
            // 文字表現列：チャンクを現在のエンコーディングでデコードし、制御文字は空白へ。
            let decoded = self.encoding.decode(chunk);
            let chars: String = decoded
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            out.push(DisplayLine {
                gutter: format!("{offset:06X}"),
                body: format!("{hex}| {chars}"),
            });
        }
        if out.is_empty() {
            out.push(DisplayLine { gutter: "000000".to_owned(), body: String::new() });
        }
        out
    }
}

/// テキストを論理行へ分割する（改行は `\n`・`\r\n`・`\r` を許容、行末改行は含めない）。
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&text[start..i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                lines.push(&text[start..i]);
                i += 1;
                if i < bytes.len() && bytes[i] == b'\n' {
                    i += 1;
                }
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        lines.push(&text[start..]);
    } else if bytes.is_empty() {
        lines.push("");
    }
    lines
}

/// 1 論理行をタブ展開しつつ表示幅 `wrap_cols`（全角=2）で折返す。各セグメント文字列を返す。
fn wrap_line(line: &str, wrap_cols: usize, tab_width: usize) -> Vec<String> {
    let wrap = wrap_cols.max(1);
    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut col = 0usize;
    let push_char = |cur: &mut String, col: &mut usize, segments: &mut Vec<String>, ch: char, w: usize| {
        if *col + w > wrap && !cur.is_empty() {
            segments.push(std::mem::take(cur));
            *col = 0;
        }
        cur.push(ch);
        *col += w;
    };
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = tab_width - (col % tab_width);
            for _ in 0..spaces {
                push_char(&mut cur, &mut col, &mut segments, ' ', 1);
            }
        } else {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if w == 0 {
                if ch.is_control() {
                    // 制御文字は脱落させず置換文字で可視化する（1桁消費する）。
                    push_char(&mut cur, &mut col, &mut segments, '\u{FFFD}', 1);
                }
                // 結合文字などのゼロ幅文字は表示行に出さない（基底文字に影響させない）。
                continue;
            }
            push_char(&mut cur, &mut col, &mut segments, ch, w);
        }
    }
    segments.push(cur);
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_cycle_wraps() {
        assert_eq!(Encoding::Utf8.cycle(true), Encoding::ShiftJis);
        assert_eq!(Encoding::Iso2022Jp.cycle(true), Encoding::Utf8);
        assert_eq!(Encoding::Utf8.cycle(false), Encoding::Iso2022Jp);
        assert_eq!(Encoding::ShiftJis.cycle(false), Encoding::Utf8);
    }

    #[test]
    fn decode_utf8_and_shiftjis() {
        // "あ" = UTF-8 E3 81 82 / Shift_JIS 82 A0 / EUC-JP A4 A2。
        assert_eq!(Encoding::Utf8.decode(&[0xE3, 0x81, 0x82]), "あ");
        assert_eq!(Encoding::ShiftJis.decode(&[0x82, 0xA0]), "あ");
        assert_eq!(Encoding::EucJp.decode(&[0xA4, 0xA2]), "あ");
    }

    #[test]
    fn decode_lossy_does_not_panic() {
        // 不正な UTF-8 でも U+FFFD を含む文字列が返り、落ちない。
        let s = Encoding::Utf8.decode(&[0x41, 0xFF, 0xFE, 0x42]);
        assert!(s.starts_with('A'));
        assert!(s.ends_with('B'));
        assert!(s.contains('\u{FFFD}'));
    }

    #[test]
    fn text_lines_wrap_by_display_width() {
        let model = ViewerModel::new(b"abcdef".to_vec());
        let lines = model.lines(3, 4);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], DisplayLine { gutter: "1".into(), body: "abc".into() });
        // 折返し継続行は gutter 空。
        assert_eq!(lines[1], DisplayLine { gutter: "".into(), body: "def".into() });
    }

    #[test]
    fn text_lines_fullwidth_counts_two() {
        // 全角3文字＝表示幅6。wrap=4 だと 2文字(幅4)で折返す。
        let model = ViewerModel::new("ああα".as_bytes().to_vec());
        let lines = model.lines(4, 4);
        // "ああ"(幅4) / "α"(幅1)。α は半角扱い。
        assert_eq!(lines[0].body, "ああ");
        assert_eq!(lines[1].body, "α");
    }

    #[test]
    fn text_lines_split_crlf_and_number() {
        let model = ViewerModel::new(b"a\r\nb\nc".to_vec());
        let lines = model.lines(80, 4);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], DisplayLine { gutter: "1".into(), body: "a".into() });
        assert_eq!(lines[1], DisplayLine { gutter: "2".into(), body: "b".into() });
        assert_eq!(lines[2], DisplayLine { gutter: "3".into(), body: "c".into() });
    }

    #[test]
    fn control_char_becomes_replacement() {
        // 制御文字（BEL=0x07）は脱落させず U+FFFD で可視化する。
        let model = ViewerModel::new(b"a\x07b".to_vec());
        let lines = model.lines(80, 4);
        assert_eq!(lines[0].body, "a\u{FFFD}b");
    }

    #[test]
    fn combining_mark_is_not_replaced() {
        // 結合文字（U+0301 アクセント）はゼロ幅のまま落とし、置換文字にはしない。
        let model = ViewerModel::new("e\u{0301}x".as_bytes().to_vec());
        let lines = model.lines(80, 4);
        assert!(!lines[0].body.contains('\u{FFFD}'), "結合文字を置換してはいけない: {:?}", lines[0].body);
    }

    #[test]
    fn tab_expands_to_stops() {
        let model = ViewerModel::new(b"a\tb".to_vec());
        let lines = model.lines(80, 4);
        // a + (タブで次の4の倍数まで=3スペース) + b
        assert_eq!(lines[0].body, "a   b");
    }

    #[test]
    fn binary_dump_format() {
        let bytes: Vec<u8> = (0u8..18).collect();
        let model = {
            let mut m = ViewerModel::new(bytes);
            m.toggle_mode();
            m
        };
        let lines = model.lines(80, 4);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].gutter, "000000");
        // 0..7 + "- " + 8..15、8 バイト目の前に区切り。
        assert!(lines[0].body.starts_with("00 01 02 03 04 05 06 07 - 08 09 0A 0B 0C 0D 0E 0F | "));
        assert_eq!(lines[1].gutter, "000010");
        assert!(lines[1].body.starts_with("10 11 "));
    }

    #[test]
    fn binary_char_column_uses_encoding() {
        // Shift_JIS "あ" = 82 A0。文字列列に "あ" が出る。
        let mut model = ViewerModel::new(vec![0x82, 0xA0]);
        model.encoding = Encoding::ShiftJis;
        model.toggle_mode();
        let lines = model.lines(80, 4);
        assert!(lines[0].body.ends_with("| あ"));
    }

    #[test]
    fn looks_binary_detects_binary_and_text() {
        assert!(!looks_binary(b"hello world\n"));
        assert!(!looks_binary("日本語のテキスト\n".as_bytes()));
        assert!(!looks_binary(&[0x82, 0xA0, 0x82, 0xA2])); // Shift_JIS "あい"
        assert!(!looks_binary(b"")); // 空はテキスト扱い
        assert!(looks_binary(b"PK\x03\x04\x00\x00binary")); // NUL を含む
        assert!(looks_binary(&[0xFFu8; 64])); // どのエンコでも不正
    }

    #[test]
    fn open_starts_in_binary_for_binary_bytes() {
        assert_eq!(ViewerModel::open(b"plain text\n".to_vec()).mode, ViewMode::Text);
        assert_eq!(ViewerModel::open(vec![0x00, 0x01, 0x02, 0x03]).mode, ViewMode::Binary);
    }

    #[test]
    fn empty_file_text_and_binary() {
        let model = ViewerModel::new(Vec::new());
        assert_eq!(model.lines(80, 4).len(), 1);
        let mut m = ViewerModel::new(Vec::new());
        m.toggle_mode();
        assert_eq!(m.lines(80, 4).len(), 1);
    }
}
