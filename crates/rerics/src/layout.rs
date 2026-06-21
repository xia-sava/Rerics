use winsafe::{self as w, gui, prelude::*};
use crate::{ActiveView, MainWindow, place};

impl MainWindow {
    /// 左右2ペイン（パスバー＋リスト）と境界線を `split_ratio` に従って割り付ける。
    pub(crate) fn layout(&self) -> w::AnyResult<()> {
        let rc = self.wnd.hwnd().GetClientRect()?;
        let total_w = rc.right - rc.left;
        let total_h = rc.bottom - rc.top;
        let cfg = self.config.borrow();
        let lay = &cfg.layout;
        let m = gui::dpi_x(lay.margin);
        let my = gui::dpi_y(lay.margin);
        let splitter_w = gui::dpi_x(lay.splitter_width);

        let tab_h = gui::dpi_y(lay.tab_height);
        let log_h = self.log.height_for_rows(lay.log_height);
        let log_gap = gui::dpi_y(lay.log_gap);
        let bars_y = tab_h;
        let log_y = total_h - my - log_h;
        let pane_h = (log_y - log_gap - bars_y).max(0);

        let panes_total = (total_w - m * 2 - splitter_w).max(0);
        let min_pane = gui::dpi_x(24).min(panes_total / 2);
        let left_w = ((panes_total as f64 * self.split_ratio.get()).round() as i32)
            .clamp(min_pane, (panes_total - min_pane).max(min_pane));
        let right_w = panes_total - left_w;
        let left_x = m;
        let splitter_x = m + left_w;
        let right_x = splitter_x + splitter_w;
        let log_w = total_w - m * 2;

        place(self.tab_bar.hwnd(), 0, 0, total_w, tab_h)?;
        place(self.left.hwnd(), left_x, bars_y, left_w, pane_h)?;
        self.left.relayout()?;
        place(self.splitter.hwnd(), splitter_x, bars_y, splitter_w, pane_h)?;
        place(self.right.hwnd(), right_x, bars_y, right_w, pane_h)?;
        self.right.relayout()?;
        // 利用可能幅が変わったので content-fit を再計算する（フレックス列が残り幅に追従）。
        self.view(true).autofit_columns()?;
        self.view(false).autofit_columns()?;
        place(self.log.hwnd(), left_x, log_y, log_w, log_h)?;
        // ビューアはタブバー下のメイン領域（ペイン＋ログ）全体を覆う。表示状態は維持。
        let view_h = (total_h - bars_y).max(0);
        place(self.viewer.hwnd(), 0, bars_y, total_w, view_h)?;
        place(self.media.hwnd(), 0, bars_y, total_w, view_h)?;
        match self.active_view.get() {
            ActiveView::Text => self.viewer.hwnd().BringWindowToTop()?,
            ActiveView::Media => self.media.hwnd().BringWindowToTop()?,
            ActiveView::None => {}
        }
        self.tab_bar.refresh()?;
        self.log.refresh()?;
        self.viewer.refresh()?;
        self.media.refresh()?;
        Ok(())
    }

    /// 現在のクライアント幅でのペイン合計幅（左右の幅の和・物理px）。
    pub(crate) fn panes_total(&self) -> w::AnyResult<i32> {
        let rc = self.wnd.hwnd().GetClientRect()?;
        let total_w = rc.right - rc.left;
        let cfg = self.config.borrow();
        let lay = &cfg.layout;
        let m = gui::dpi_x(lay.margin);
        let splitter_w = gui::dpi_x(lay.splitter_width);
        Ok((total_w - m * 2 - splitter_w).max(1))
    }

    /// 左ペイン幅（物理px）から分割比を更新して再レイアウトする。
    pub(crate) fn set_left_width(&self, left_w: i32) -> w::AnyResult<()> {
        let pt = self.panes_total()?;
        let ratio = (left_w as f64 / pt as f64).clamp(0.05, 0.95);
        self.split_ratio.set(ratio);
        self.layout()
    }

    /// スプリッタのドラッグ（親座標の希望左端）を分割比へ反映する。
    pub(crate) fn drag_splitter(&self, splitter_left: i32) -> w::AnyResult<()> {
        let m = gui::dpi_x(self.config.borrow().layout.margin);
        self.maximized.set(false);
        self.set_left_width(splitter_left - m)
    }

    /// 左ペイン最大化（トグル。最大化中はトグルで中央へ）。`force` は常に最大化。
    pub(crate) fn maximize_left(&self, force: bool) -> w::AnyResult<()> {
        if !force && self.maximized.get() {
            return self.border_reset();
        }
        let pt = self.panes_total()?;
        let margin = gui::dpi_x(self.config.borrow().layout.maximize_margin);
        self.maximized.set(true);
        self.set_left_width(pt - margin)
    }

    /// 右ペイン最大化（トグル）。`force` は常に最大化。
    pub(crate) fn maximize_right(&self, force: bool) -> w::AnyResult<()> {
        if !force && self.maximized.get() {
            return self.border_reset();
        }
        let margin = gui::dpi_x(self.config.borrow().layout.maximize_margin);
        self.maximized.set(true);
        self.set_left_width(margin)
    }

    /// 境界を中央50%へ戻す（最大化状態を解除）。
    pub(crate) fn border_reset(&self) -> w::AnyResult<()> {
        self.maximized.set(false);
        self.split_ratio.set(0.5);
        self.layout()
    }

    /// 境界を `delta`（物理px・左ペインが正で広がる）だけ動かす。中央（50%）を
    /// またぐ移動は中央へ吸着させる。
    pub(crate) fn border_move(&self, delta: i32) -> w::AnyResult<()> {
        let pt = self.panes_total()?;
        let cur_left = (pt as f64 * self.split_ratio.get()).round() as i32;
        let next = snap_to_center(cur_left, cur_left + delta, pt / 2);
        self.set_left_width(next)
    }
}

/// 左ペイン幅 `cur` から `next` への移動が中央 `center` をまたぐとき、中央へ吸着させる。
/// またがない移動はそのまま `next` を返す（中央ちょうどからは外へ出られる）。
fn snap_to_center(cur: i32, next: i32, center: i32) -> i32 {
    if (cur < center && next > center) || (cur > center && next < center) {
        center
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::snap_to_center;

    #[test]
    fn center_snap_catches_only_crossing() {
        assert_eq!(snap_to_center(40, 60, 50), 50); // 左→右でまたぐ
        assert_eq!(snap_to_center(60, 40, 50), 50); // 右→左でまたぐ
        assert_eq!(snap_to_center(40, 48, 50), 48); // またがない
        assert_eq!(snap_to_center(50, 70, 50), 70); // 中央からは外へ出られる
        assert_eq!(snap_to_center(80, 60, 50), 60); // 同じ側の移動はそのまま
    }
}
