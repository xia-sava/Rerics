use winsafe::{self as w, prelude::*};
use rerics_core::Config;
use crate::{MainWindow, dialog, settings_dialog, system_is_light};

impl MainWindow {
    /// 設定ダイアログを開く。開いた時点で OS テーマを再判定し、OK なら設定をライブ反映して
    /// 差分を `config.toml` へ保存する。
    pub(crate) fn open_settings(&self) -> w::AnyResult<()> {
        if dialog::modal_active() {
            return Ok(());
        }
        let mut current = self.config.borrow().clone();
        current.resolve_theme(system_is_light());
        let scripts = self.script_list_commands();
        let members = self.script_list_members();
        let globals = self.script_list_globals();
        let me = self.clone();
        settings_dialog::show(&self.wnd, &current, scripts, members, globals, move |new| {
            let mut new = new.clone();
            new.resolve_theme(system_is_light());
            if let Err(e) = me.apply_config(new) {
                me.log.error(&format!("設定の適用に失敗: {}", e));
            } else if let Err(e) = me.config.borrow().save() {
                me.log.error(&format!("設定の保存に失敗: {}", e));
            }
        });
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// 現在の実効キー割り当ての一覧を読み取り専用で表示する。
    pub(crate) fn keybinds_dialog(&self) {
        let rows: Vec<String> = self
            .keymap
            .borrow()
            .to_string_map()
            .iter()
            .map(|(k, v)| format!("{k:<18} {v}"))
            .collect();
        let _ = dialog::list_box(&self.wnd, "キー割り当て", "keybinds", &rows, 0);
        self.key_sink.hwnd().SetFocus();
    }

    /// 新しい設定をライブ反映する（配色・フォント・レイアウト寸法・キーバインド）。
    /// 列構成の変更は再起動後に反映される。
    pub(crate) fn apply_config(&self, new: Config) -> w::AnyResult<()> {
        let old_default = {
            let cfg = self.config.borrow();
            (cfg.default_sort, cfg.default_sort_reverse)
        };
        *self.config.borrow_mut() = new;
        let km = self.config.borrow().keymap();
        *self.keymap.borrow_mut() = km;
        let vkm = self.config.borrow().keymap_textviewer();
        *self.viewer_keymap.borrow_mut() = vkm;
        let mkm = self.config.borrow().keymap_imageviewer();
        *self.media_keymap.borrow_mut() = mkm;
        {
            let cfg = self.config.borrow();
            self.left.apply_config(&cfg);
            self.right.apply_config(&cfg);
            self.tab_bar.apply_config(&cfg);
            self.log.apply_config(&cfg);
            // 既定ソート・日付ソート反転の変更は非アクティブタブのスナップショットにも
            // 追従させる（表示中ペインは FileListView::apply_config が行う）。
            let (pl, pr) = (self.view(true).page_rows(), self.view(false).page_rows());
            for t in self.tabs.borrow_mut().iter_mut() {
                t.left_state.apply_sort_config_change(old_default, &cfg, pl);
                t.right_state.apply_sort_config_change(old_default, &cfg, pr);
            }
        }
        // 更新監視の設定変更を反映して張り替える（対象外になった監視は止め、有効化は張る）。
        self.arm_watch(true);
        self.arm_watch(false);
        self.layout()?;
        self.refresh_tab_bar()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rerics_core::Config;

    /// 設定キーの反映経路。
    enum Apply {
        /// 設定ダイアログ OK 時に `apply_config` の連鎖で配り直す（キャッシュを再構築する）。
        Live,
        /// 使用時に毎回 `self.config` から読む（キャッシュしないので配布不要＝自動でライブ）。
        ReadAtUse,
        /// 起動時にしか読まない（再起動後に反映）。
        RestartOnly,
    }

    /// config 全キー（ドット区切り・プレフィックスで下位キーをまとめて分類できる）の
    /// 反映経路の台帳。**設定項目を追加するとこのテストが落ちる。** 追加した項目を
    /// - `apply_config` の連鎖で配るなら `Live` に足し、`apply_config` への配線も行う
    /// - 使用時に config から都度読むだけなら `ReadAtUse` に足す
    /// - 起動時のみ反映なら `RestartOnly` に足す（設定画面の説明とも整合させる）
    /// 分類だけ足して配線を忘れると「再起動するまで反映されない」事故になるので、
    /// `Live` に足すときは反映経路を必ず確認する。
    const LEDGER: &[(&str, Apply)] = &[
        ("theme", Apply::Live), // active_colors 経由で各ビューへ
        ("font", Apply::Live),
        ("layout", Apply::Live), // layout() 再実行
        ("colors", Apply::Live),
        ("columns", Apply::Live), // FileListView::apply_config が表示中ペインの state へ
        ("size_format", Apply::Live),
        ("auto_adjust_columns", Apply::Live),
        ("char_spacing_px", Apply::Live),
        ("default_sort", Apply::Live), // apply_sort_config_change
        ("default_sort_reverse", Apply::Live),
        ("reverse_sort_date", Apply::Live),
        ("progress_delay_ms", Apply::Live),
        ("keybinds", Apply::Live), // keymap 再構築
        ("keybinds_textviewer", Apply::Live),
        ("keybinds_imageviewer", Apply::Live),
        ("bookmarks", Apply::ReadAtUse),
        ("menus", Apply::ReadAtUse),
        ("editor", Apply::ReadAtUse),
        ("cursor", Apply::ReadAtUse),
        ("image", Apply::ReadAtUse), // ビューアを開くたびに読む
        ("icons", Apply::Live),
        ("file_ops", Apply::ReadAtUse),
        ("window", Apply::RestartOnly), // 起動時のウィンドウ生成のみ
        ("reload_watch", Apply::Live),  // arm_watch 張り替え
    ];

    /// serde 表現の葉キーをドット区切りで列挙する（空でないオブジェクトのみ潜る）。
    fn leaf_keys(prefix: &str, v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) if !map.is_empty() => {
                for (k, v) in map {
                    let key =
                        if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                    leaf_keys(&key, v, out);
                }
            }
            _ => out.push(prefix.to_string()),
        }
    }

    fn covers(prefix: &str, key: &str) -> bool {
        key == prefix || key.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.'))
    }

    /// 契約: config の全キーが台帳（LEDGER）で反映経路を宣言されていること。
    /// 新しい設定項目のライブ反映（apply_config への配線）を無自覚に忘れるのを防ぐ。
    #[test]
    fn every_config_key_declares_its_apply_path() {
        let v = serde_json::to_value(Config::default()).unwrap();
        let mut keys = Vec::new();
        leaf_keys("", &v, &mut keys);
        assert!(!keys.is_empty(), "Config のキー列挙に失敗");
        let unclassified: Vec<&String> = keys
            .iter()
            .filter(|k| !LEDGER.iter().any(|(p, _)| covers(p, k)))
            .collect();
        assert!(
            unclassified.is_empty(),
            "反映経路が未分類の設定キー（LEDGER のコメントに従って分類する）: {unclassified:?}"
        );
        // 逆方向：設定から消えたキーが台帳に残っていないこと。
        let stale: Vec<&str> = LEDGER
            .iter()
            .map(|(p, _)| *p)
            .filter(|p| !keys.iter().any(|k| covers(p, k)))
            .collect();
        assert!(stale.is_empty(), "config に存在しないキーが台帳に残っている: {stale:?}");
    }
}
