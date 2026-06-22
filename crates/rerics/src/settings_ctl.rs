use winsafe::{self as w, prelude::*};
use rerics_core::Config;
use crate::{MainWindow, dialog, settings_dialog, system_is_light};

impl MainWindow {
    /// 設定ダイアログを開く。開いた時点で OS テーマを再判定し、OK なら設定をライブ反映して
    /// 差分を `config.toml` へ保存する。
    pub(crate) fn open_settings(&self) -> w::AnyResult<()> {
        if self.in_dialog.get() {
            return Ok(());
        }
        let mut current = self.config.borrow().clone();
        current.resolve_theme(system_is_light());
        self.in_dialog.set(true);
        let me = self.clone();
        settings_dialog::show(&self.wnd, &current, move |new| {
            let mut new = new.clone();
            new.resolve_theme(system_is_light());
            if let Err(e) = me.apply_config(new) {
                me.log.error(&format!("設定の適用に失敗: {}", e));
            } else if let Err(e) = me.config.borrow().save() {
                me.log.error(&format!("設定の保存に失敗: {}", e));
            }
        });
        self.in_dialog.set(false);
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
        }
        self.layout()?;
        self.refresh_tab_bar()?;
        Ok(())
    }
}
