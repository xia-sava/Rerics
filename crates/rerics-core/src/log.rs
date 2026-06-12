//! ログウィンドウのモデル層（UI 非依存）。
//!
//! `LogLevel`（重要度）・`LogLine`（1行）・`LogState`（行の保持・上限トリム・
//! スクロール位置）を提供する。描画は GUI 層の責務。

/// ログ行の重要度。文字色と太字の決定に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// 通常（操作の逐次ログ）。白・非太字。
    Normal,
    /// 情報（結果サマリ等）。太字。
    Info,
    Warning,
    Error,
}

/// ログ1行。
#[derive(Debug, Clone)]
pub struct LogLine {
    pub text: String,
    pub level: LogLevel,
}

/// ログウィンドウの状態モデル（描画と完全分離）。
#[derive(Clone)]
pub struct LogState {
    pub lines: Vec<LogLine>,
    pub scroll_top: usize,
    /// 保持する最大行数。超過分は先頭から捨てる。
    pub max_lines: usize,
}

impl Default for LogState {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            scroll_top: 0,
            max_lines: 1000,
        }
    }
}

impl LogState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn count(&self) -> usize {
        self.lines.len()
    }

    /// レベル付きでメッセージを追加する。`text` 内の改行は行ごとに分割して
    /// 個別の行にする（末尾空白は落とす）。上限超過分は先頭から捨てる。
    pub fn push(&mut self, level: LogLevel, text: &str) {
        for raw in text.split('\n') {
            let line = raw.trim_end_matches('\r').trim_end();
            self.lines.push(LogLine {
                text: line.to_owned(),
                level,
            });
        }
        let max = self.max_lines.max(1);
        if self.lines.len() > max {
            let excess = self.lines.len() - max;
            self.lines.drain(0..excess);
        }
    }

    /// 表示できる最終行（0 件なら 0）。
    pub fn scroll_bottom(&self, page_rows: usize) -> usize {
        let count = self.count();
        if count == 0 {
            return 0;
        }
        (self.scroll_top + page_rows.max(1) - 1).min(count - 1)
    }

    /// scroll_top を 0..=max(0, count-page_rows) にクランプする。
    fn clamp_scroll(&mut self, page_rows: usize) {
        let max = self.count().saturating_sub(page_rows.max(1));
        if self.scroll_top > max {
            self.scroll_top = max;
        }
    }

    /// scroll_top を直接設定する。
    pub fn set_scroll_top(&mut self, top: isize, page_rows: usize) {
        let top = if top < 0 { 0 } else { top as usize };
        self.scroll_top = top;
        self.clamp_scroll(page_rows.max(1));
    }

    /// 末尾行が見えるところまでスクロールする（新着追従）。
    pub fn scroll_to_bottom(&mut self, page_rows: usize) {
        let page_rows = page_rows.max(1);
        let top = self.count().saturating_sub(page_rows);
        self.scroll_top = top;
    }

    /// 全消去する。
    pub fn clear(&mut self) {
        self.lines.clear();
        self.scroll_top = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_splits_multiline() {
        let mut s = LogState::new();
        s.push(LogLevel::Info, "a\nb\r\nc");
        assert_eq!(s.count(), 3);
        assert_eq!(s.lines[0].text, "a");
        assert_eq!(s.lines[1].text, "b");
        assert_eq!(s.lines[2].text, "c");
    }

    #[test]
    fn push_trims_trailing_ws() {
        let mut s = LogState::new();
        s.push(LogLevel::Error, "boom   ");
        assert_eq!(s.lines[0].text, "boom");
        assert_eq!(s.lines[0].level, LogLevel::Error);
    }

    #[test]
    fn max_lines_drops_front() {
        let mut s = LogState::new();
        s.max_lines = 3;
        for i in 0..5 {
            s.push(LogLevel::Info, &format!("line{i}"));
        }
        assert_eq!(s.count(), 3);
        assert_eq!(s.lines[0].text, "line2");
        assert_eq!(s.lines[2].text, "line4");
    }

    #[test]
    fn scroll_to_bottom_shows_last() {
        let mut s = LogState::new();
        for i in 0..20 {
            s.push(LogLevel::Info, &format!("l{i}"));
        }
        let pr = 5;
        s.scroll_to_bottom(pr);
        assert_eq!(s.scroll_top, 15);
        assert_eq!(s.scroll_bottom(pr), 19);
    }

    #[test]
    fn scroll_to_bottom_when_fits_is_zero() {
        let mut s = LogState::new();
        for i in 0..3 {
            s.push(LogLevel::Info, &format!("l{i}"));
        }
        s.scroll_to_bottom(10);
        assert_eq!(s.scroll_top, 0);
    }

    #[test]
    fn set_scroll_top_clamps() {
        let mut s = LogState::new();
        for i in 0..20 {
            s.push(LogLevel::Info, &format!("l{i}"));
        }
        let pr = 5;
        s.set_scroll_top(100, pr);
        assert_eq!(s.scroll_top, 15);
        s.set_scroll_top(-5, pr);
        assert_eq!(s.scroll_top, 0);
    }
}
