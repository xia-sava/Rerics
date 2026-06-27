use std::cell::RefCell;
use std::rc::Rc;
use winsafe::{self as w, co, gui, prelude::*};
use crate::MainWindow;

impl MainWindow {
    pub(crate) fn mask(&self, is_left: bool) -> &Rc<RefCell<Option<String>>> {
        if is_left { &self.left_mask } else { &self.right_mask }
    }

    /// 入力ダイアログでパスマスクを尋ね、no-UI 版 `r.pathMask` へ委譲する。設定/解除の正規化
    /// （空・`*` は解除）と一覧更新は委譲先（`SetPathMask`）が行う。
    pub(crate) fn path_mask(&self) -> w::AnyResult<()> {
        let is_left = !self.active_right.get();
        let cur = self.mask(is_left).borrow().clone().unwrap_or_default();
        let input = self.input_with_history(
            "パスマスク",
            "表示するマスク（* で解除・カンマ区切り）:",
            &cur,
            "pathmask",
        );
        let Some(input) = input else {
            return Ok(());
        };
        let arg = serde_json::to_string(input.trim()).unwrap_or_else(|_| "\"\"".to_string());
        self.script_send(crate::script_host::EngineCmd::Eval(format!("r.pathMask({arg})")));
        Ok(())
    }

    /// 入力ダイアログでマスクを尋ね、選んだマスクで選択し直す。選択ロジックは UI を持たない
    /// `r.selectMask`（カンマ区切りの Like パターン・既存選択をクリアして選び直す）へ委譲する。
    pub(crate) fn select_mask(&self) -> w::AnyResult<()> {
        let input = self.input_with_history(
            "マスクで選択",
            "選択するマスク（カンマ区切り）:",
            "",
            "selectmask",
        );
        let Some(input) = input else {
            return Ok(());
        };
        let input = input.trim();
        if input.is_empty() {
            return Ok(());
        }
        let arg = serde_json::to_string(input).unwrap_or_else(|_| "\"\"".to_string());
        self.script_send(crate::script_host::EngineCmd::Eval(format!("r.selectMask({arg})")));
        Ok(())
    }

    /// 比較方法を一覧から選ばせ、選んだ条件で同名ファイル比較選択を実行する。比較ロジック
    /// 自体は UI を持たない `r.compare` に委譲する（UI で選んだ条件を引数として渡すだけ）。
    pub(crate) fn compare_dialog(&self) -> w::AnyResult<()> {
        const OPTIONS: &[(&str, &str)] = &[
            ("名前一致のみ", "name"),
            ("日付が一致", "sameDate"),
            ("日付が不一致", "diffDate"),
            ("日付が新しい", "newer"),
            ("日付が古い", "older"),
            ("サイズが一致", "sameSize"),
            ("サイズが不一致", "diffSize"),
            ("サイズが小さい", "smaller"),
            ("サイズが大きい", "larger"),
            ("存在しないファイル", "notExists"),
        ];
        let labels: Vec<String> = OPTIONS.iter().map(|(label, _)| label.to_string()).collect();
        let Some(idx) =
            crate::dialog::list_box(&self.wnd, "同名ファイル選択", "compare_dialog", &labels, 0)
        else {
            return Ok(());
        };
        let token = OPTIONS[idx].1;
        self.script_send(crate::script_host::EngineCmd::Eval(format!("r.compare({token:?})")));
        Ok(())
    }

    /// 指定ペインを検索・比較の結果一覧へ切り替える。`items` は合成項目（各々 `source`/`info`
    /// を持つ。先頭の ".." は結果モードを抜ける親項目）。一覧だけ差し替え、現在地（パスバー・
    /// 基準ディレクトリ）は元のままにする。
    pub(crate) fn show_find_result(
        &self,
        is_left: bool,
        items: Vec<rerics_core::FileItem>,
    ) -> w::AnyResult<()> {
        let view = self.view(is_left);
        {
            let state = view.state();
            state.borrow_mut().set_find_result(items);
        }
        view.autofit_columns()?;
        view.refresh()?;
        self.update_selected_info(is_left);
        Ok(())
    }

    /// 条件ダイアログ（名前・日付・サイズ）を出し、OK ならその条件でファイル検索を実行する。
    /// 検索ロジックは `run_find_file`（Rust 正本）へ委譲する。
    pub(crate) fn find_file_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        if let Some(opts) = crate::dialog::find_file_box(&self.wnd) {
            if opts.is_empty() {
                self.log.warn("検索条件がありません。");
            } else {
                self.run_find_file(is_left, opts);
            }
        }
        Ok(())
    }

    /// 現在地以下を再帰検索し、条件に合うファイルを結果一覧に出す。検索はワーカースレッドで
    /// 回し、終わったら結果ペインへ流し込む。
    pub(crate) fn run_find_file(&self, is_left: bool, opts: rerics_core::FindOptions) {
        let root = self.pane(is_left).borrow().loc().clone();
        self.log.info(&format!("ファイル検索: {}", root.loc_display()));
        self.spawn_job(
            move || rerics_core::find_file(&root, &opts),
            move |mw, (items, count)| {
                let mut all = Vec::with_capacity(items.len() + 1);
                all.push(rerics_core::FileItem::parent());
                all.extend(items);
                mw.show_find_result(is_left, all)?;
                mw.log.info(&format!("検索結果 {count}件"));
                Ok(())
            },
        );
    }

    /// 条件ダイアログ（日付・サイズの比較条件と抽出範囲）を出し、OK ならその条件で
    /// ディレクトリ比較を実行する。比較ロジックは `run_directory_compare`（Rust 正本）へ委譲。
    pub(crate) fn directory_compare_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        if let Some(opts) = crate::dialog::compare_options_box(&self.wnd) {
            self.run_directory_compare(is_left, opts);
        }
        Ok(())
    }

    /// アクティブペインと反対ペインのディレクトリを比較し、差分を結果一覧に出す（原作
    /// ディレクトリ比較）。比較はワーカースレッドで回し、終わったら結果ペインへ流し込む。
    pub(crate) fn run_directory_compare(&self, is_left: bool, opts: rerics_core::CompareOptions) {
        let src = self.pane(is_left).borrow().loc().clone();
        let dst = self.pane(!is_left).borrow().loc().clone();
        self.log.info(&format!("ディレクトリ比較: {}", src.loc_display()));
        self.spawn_job(
            move || rerics_core::directory_compare(&src, &dst, &opts),
            move |mw, (items, counts)| {
                let mut all = Vec::with_capacity(items.len() + 1);
                all.push(rerics_core::FileItem::parent());
                all.extend(items);
                mw.show_find_result(is_left, all)?;
                mw.log.info(&format!(
                    "比較結果 一致:{} 不一致:{} 追加:{} 削除:{}",
                    counts.equals, counts.not_equals, counts.adds, counts.deletes
                ));
                Ok(())
            },
        );
    }

    /// インクリメンタルサーチ。小さな入力モーダルを出し、打鍵ごとに先頭から一致を
    /// 探してアクティブペインのカーソルを動かす（追従）。OK で確定、中止/Esc で元へ戻す。
    pub(crate) fn incremental_search(&self, is_left: bool) -> w::AnyResult<()> {
        let origin = self.view(is_left).state().borrow().cursor;

        let (wnd, arm) = crate::dialog::modal_window("インクリメンタルサーチ", 320, 96);

        let _label = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: "検索文字（打鍵でカーソルが追従）:",
                position: gui::dpi(12, 10),
                size: gui::dpi(296, 16),
                ..Default::default()
            },
        );

        let edit = gui::Edit::new(
            &wnd,
            gui::EditOpts {
                control_style: co::ES::AUTOHSCROLL,
                position: gui::dpi(12, 30),
                width: gui::dpi_x(296),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );

        let ok = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "OK",
                control_style: co::BS::DEFPUSHBUTTON,
                ctrl_id: 1,
                position: gui::dpi(150, 60),
                width: gui::dpi_x(76),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );

        let cancel = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "中止(&S)",
                ctrl_id: 2,
                position: gui::dpi(232, 60),
                width: gui::dpi_x(76),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );

        // 打鍵追従：テキスト変化のたびに先頭から検索してカーソルを移す。
        {
            let this = self.clone();
            let edit2 = edit.clone();
            edit.on().en_change(move || {
                let q = edit2.text().unwrap_or_default();
                this.incremental_apply(is_left, &q);
                Ok(())
            });
        }

        #[cfg(feature = "debug-server")]
        arm.plain(
            "incremental",
            "インクリメンタルサーチ",
            "",
            true,
            vec![("OK".to_string(), 1u16), ("中止(&S)".to_string(), 2u16)],
        );
        {
            let edit2 = edit.clone();
            arm.on_create(move |_| {
                edit2.hwnd().SetFocus();
                Ok(())
            });
        }

        {
            let wnd2 = wnd.clone();
            ok.on().bn_clicked(move || {
                wnd2.close();
                Ok(())
            });
        }
        {
            let this = self.clone();
            let wnd2 = wnd.clone();
            cancel.on().bn_clicked(move || {
                // 中止＝開始時のカーソルへ戻す。
                this.move_cursor_to(is_left, origin);
                wnd2.close();
                Ok(())
            });
        }

        let _ = wnd.show_modal(&self.wnd);
        let _ = (edit, ok, cancel);
        Ok(())
    }

    /// インクリメンタルサーチの1打鍵分：先頭から `query` の一致を探してカーソル移動。
    pub(crate) fn incremental_apply(&self, is_left: bool, query: &str) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        let found = {
            let state = view.state();
            let s = state.borrow();
            rerics_core::find_match(&s.items, 0, query, true, false)
        };
        if let Some(i) = found {
            {
                let state = view.state();
                let mut s = state.borrow_mut();
                s.set_cursor(i as isize, pr);
                s.center_cursor(pr);
            }
            let _ = view.refresh();
        }
    }

    /// 指定ペインのカーソルを `idx` に移して再描画する。
    pub(crate) fn move_cursor_to(&self, is_left: bool, idx: usize) {
        let view = self.view(is_left);
        let pr = view.page_rows();
        {
            let state = view.state();
            let mut s = state.borrow_mut();
            s.set_cursor(idx as isize, pr);
        }
        let _ = view.refresh();
    }
}
