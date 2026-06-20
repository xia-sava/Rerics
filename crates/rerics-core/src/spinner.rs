//! レイテンシある処理の「ぐるぐる／進捗」表示の状態。
//!
//! タイミング（出現閾値）・コマ送り・百分率の計算だけを共通化し、実際の描画
//! （ペイン中央／一覧セル内／…）は各サイトに委ねる。UI 非依存なのでここで完結してテストできる。

use std::time::{Duration, Instant};

/// ぐるぐるのコマ（ブライユ点字）。
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// 進行中処理の待機表示の状態。
pub struct Spinner {
    /// 開始時刻（出現閾値の判定起点）。
    started: Instant,
    /// この時間を過ぎてから表示する（短時間で終わる処理をちらつかせない）。
    visible_after: Duration,
    /// コマ送りカウンタ。
    frame: usize,
    /// 進捗（done, total）。total=0／未設定は不定（充填率・百分率を出さない）。
    progress: Option<(u64, u64)>,
}

impl Spinner {
    /// 即座に表示するスピナー（閾値0）。
    pub fn immediate() -> Self {
        Self::with_delay(Duration::ZERO)
    }

    /// 閾値 `visible_after` を過ぎてから表示するスピナー。
    pub fn with_delay(visible_after: Duration) -> Self {
        Self { started: Instant::now(), visible_after, frame: 0, progress: None }
    }

    /// コマを1つ進める（タイマから毎回呼ぶ）。
    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// 出現閾値を過ぎたか（＝今この瞬間に描画すべきか）。
    pub fn visible(&self) -> bool {
        self.started.elapsed() >= self.visible_after
    }

    /// 現在のコマ文字。
    pub fn glyph(&self) -> &'static str {
        SPINNER_FRAMES[self.frame % SPINNER_FRAMES.len()]
    }

    /// 進捗を設定する。
    pub fn set_progress(&mut self, done: u64, total: u64) {
        self.progress = Some((done, total));
    }

    /// 現在の進捗（done, total）。
    pub fn progress(&self) -> Option<(u64, u64)> {
        self.progress
    }

    /// 0..=100 の百分率（total>0 のときのみ）。
    pub fn percent(&self) -> Option<u64> {
        self.progress
            .and_then(|(done, total)| (total > 0).then(|| done.min(total) * 100 / total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_is_visible_at_once() {
        assert!(Spinner::immediate().visible());
    }

    #[test]
    fn delayed_is_hidden_until_threshold() {
        assert!(!Spinner::with_delay(Duration::from_secs(3600)).visible());
    }

    #[test]
    fn glyph_cycles_through_all_frames() {
        let mut s = Spinner::immediate();
        assert_eq!(s.glyph(), SPINNER_FRAMES[0]);
        for expected in &SPINNER_FRAMES[1..] {
            s.tick();
            assert_eq!(s.glyph(), *expected);
        }
        // 一周して先頭へ戻る。
        s.tick();
        assert_eq!(s.glyph(), SPINNER_FRAMES[0]);
    }

    #[test]
    fn percent_needs_positive_total() {
        let mut s = Spinner::immediate();
        assert_eq!(s.percent(), None);
        s.set_progress(0, 0);
        assert_eq!(s.percent(), None);
        s.set_progress(1, 4);
        assert_eq!(s.percent(), Some(25));
    }

    #[test]
    fn percent_clamps_overshoot() {
        let mut s = Spinner::immediate();
        s.set_progress(9, 4);
        assert_eq!(s.percent(), Some(100));
    }
}
