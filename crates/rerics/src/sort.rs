use rerics_core::SortType;
use crate::{MainWindow, dialog};

impl MainWindow {
    /// 指定ペインを並べ替える。カーソル下のファイルを保持する。`toggle` 時は
    /// 現在の昇降を反転、そうでなければ昇順にする。
    /// ソート設定ダイアログを開き、選ばれた種別・昇降をアクティブペインに適用する。
    /// カーソルは現在のファイル名へ追従させる。
    pub(crate) fn sort_dialog(&self, is_left: bool) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        let state = view.state();
        let (cur, reverse) = {
            let s = state.borrow();
            (s.sort_type, s.sort_reverse)
        };
        let Some((sort, reverse)) = dialog::sort_box(&self.wnd, cur, reverse) else {
            return;
        };
        let mut s = state.borrow_mut();
        let name = s.items.get(s.cursor).map(|i| i.name.clone());
        s.sort(sort, reverse);
        if let Some(n) = name {
            s.set_cursor_position(&n, pr);
        }
        drop(s);
        let _ = view.refresh();
    }

    pub(crate) fn sort_active(&self, is_left: bool, sort: SortType, toggle: bool) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        let state = view.state();
        let mut s = state.borrow_mut();
        let name = s.items.get(s.cursor).map(|i| i.name.clone());
        let reverse = if toggle { !s.sort_reverse } else { false };
        s.sort(sort, reverse);
        if let Some(n) = name {
            s.set_cursor_position(&n, pr);
        }
        drop(s);
        let _ = view.refresh();
    }
}
