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
        // 検索バー表示中はタブ帯の直下にバーを差し込み、その分ペインを下げる。
        let search_h = if self.search_bar_visible() { self.search_bar.height() } else { 0 };
        let bars_y = tab_h + search_h;
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
        if search_h > 0 {
            place(self.search_bar.hwnd(), 0, tab_h, total_w, search_h)?;
            self.search_bar.layout_children();
        }
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

    /// 境界を `delta`（物理px・左ペインが正で広がる）だけ動かす。中央（50%）をまたぐ移動は
    /// 中央へ吸着し、またがない移動は各ペインが最大化余白（`maximize_margin`）を保つ位置で
    /// 止める。中央へ戻る移動は最大化状態を解除し、中央からズレる移動は最大化状態にする。
    /// 端で動けないときは何もしない（状態も変えない）。
    pub(crate) fn border_move(&self, delta: i32) -> w::AnyResult<()> {
        let pt = self.panes_total()?;
        let edge = gui::dpi_x(self.config.borrow().layout.maximize_margin).min(pt / 2);
        let center = pt / 2;
        let cur = (pt as f64 * self.split_ratio.get()).round() as i32;
        let (next, crossed) = border_target(cur, cur + delta, center, pt, edge);
        if next == cur {
            return Ok(());
        }
        self.maximized.set(!crossed);
        self.set_left_width(next)
    }
}

/// 境界移動の着地点と、中央 `center` をまたいだか（`crossed`）を返す。またぐ移動は中央へ
/// 吸着させ、またがない移動は各ペインが `edge`（最大化余白）を残す `[edge, total-edge]` へ
/// クランプする。`crossed`＝中央へ戻る＝最大化解除、非またぎ＝中央からズレる＝最大化、の判定に使う。
fn border_target(cur: i32, raw: i32, center: i32, total: i32, edge: i32) -> (i32, bool) {
    let crossed = (cur < center && raw > center) || (cur > center && raw < center);
    let next = if crossed { center } else { raw.clamp(edge, total - edge) };
    (next, crossed)
}

#[cfg(test)]
mod tests {
    use super::border_target;

    #[test]
    fn border_target_snaps_center_and_reports_crossing() {
        // 中央(50)をまたぐ移動は中央へ吸着し crossed=true（＝最大化解除側）。
        assert_eq!(border_target(40, 60, 50, 100, 20), (50, true)); // 左→右でまたぐ
        assert_eq!(border_target(60, 40, 50, 100, 20), (50, true)); // 右→左でまたぐ
        // またがない移動はそのまま・crossed=false（＝最大化側）。
        assert_eq!(border_target(40, 45, 50, 100, 20), (45, false));
        assert_eq!(border_target(50, 70, 50, 100, 20), (70, false)); // 中央からは外へ出られる
        // 端（edge=20 → [20,80]）でクランプする。
        assert_eq!(border_target(30, 10, 50, 100, 20), (20, false)); // 左端で止まる
        assert_eq!(border_target(70, 95, 50, 100, 20), (80, false)); // 右端で止まる
    }
}
