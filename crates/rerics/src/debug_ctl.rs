#![cfg(feature = "debug-server")]

use std::sync::mpsc::Sender;
use winsafe::{self as w, co, prelude::*};
use rerics_core::{Command, CommandContext, Invocation};
use crate::{ActiveView, DebugCmdClass, MainWindow, debug_command_class, debug_json, debug_server, parse_region};

impl MainWindow {
    /// 保留中の汎用ジョブ（非同期ディレクトリ読込の継続など）を、無くなるまで同期実行する。
    /// アプリの挙動（読込は非同期）は変えず、debug server がコマンド後の確定状態を観測できる
    /// ようにするためのハーネス用。ワーカーが結果を返さない異常時は短いタイムアウトで諦める。
    fn settle_pending_jobs(&self) {
        while !self.ui_jobs.borrow().is_empty() {
            match self.ui_job_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok((id, result)) => {
                    let done = self.ui_jobs.borrow_mut().remove(&id);
                    if let Some(done) = done {
                        let _ = done(self, result);
                    }
                }
                Err(_) => break,
            }
        }
    }

    /// デバッグ制御サーバの要求キューを UI スレッドで処理する（feature 有効時のみ）。
    /// モーダルを開くコマンドは exec がネストループでブロックするため、応答を先に返してから実行する。
    /// その間に届く `/modal/*`・`/state` 等はネストループ経由で本関数が再入して捌く。
    #[cfg(feature = "debug-server")]
    pub(crate) fn drain_debug_requests(&self) {
        loop {
            let item = self.debug.queue.lock().unwrap().pop_front();
            let Some((req, tx)) = item else { break };
            match req {
                debug_server::Request::State { pointer } => {
                    self.settle_pending_jobs();
                    let v = self.debug_state_value();
                    let r = match v.pointer(&pointer) {
                        Some(sub) => debug_server::Response::Json(sub.to_string()),
                        None => debug_server::Response::NotFound,
                    };
                    let _ = tx.send(r);
                }
                debug_server::Request::Presentation { pointer } => {
                    self.settle_pending_jobs();
                    let v = self.debug_presentation_value();
                    let r = match v.pointer(&pointer) {
                        Some(sub) => debug_server::Response::Json(sub.to_string()),
                        None => debug_server::Response::NotFound,
                    };
                    let _ = tx.send(r);
                }
                debug_server::Request::Command { name, args } => {
                    self.debug_dispatch_command(&name, args, tx)
                }
                debug_server::Request::ViewKey { action } => {
                    let _ = tx.send(self.debug_view_key(&action));
                }
                debug_server::Request::ViewSearch { value } => {
                    let _ = tx.send(self.debug_view_search(&value));
                }
                debug_server::Request::ViewSearchKey { key } => {
                    let _ = tx.send(self.debug_view_search_key(&key));
                }
                debug_server::Request::ViewSearchOption { name, on } => {
                    let _ = tx.send(self.debug_view_search_option(&name, on));
                }
                debug_server::Request::ViewSearchHistory { index } => {
                    let _ = tx.send(self.debug_view_search_history(index));
                }
                debug_server::Request::ViewSearchDropdown { open } => {
                    let _ = tx.send(self.debug_view_search_dropdown(open));
                }
                debug_server::Request::ViewSearchMnemonic { key } => {
                    let _ = tx.send(self.debug_view_search_mnemonic(key));
                }
                debug_server::Request::Snapshot { spec } => {
                    self.settle_pending_jobs();
                    let _ = tx.send(self.debug_snapshot(&spec));
                }
                debug_server::Request::ModalKey { key } => {
                    let _ = tx.send(self.debug_modal_key(&key));
                }
                debug_server::Request::ModalText { value } => {
                    let _ = tx.send(self.debug_modal_text(&value));
                }
                debug_server::Request::ModalCommand { role } => {
                    let _ = tx.send(self.debug_modal_command(&role));
                }
                debug_server::Request::ModalSelect { index } => {
                    let _ = tx.send(self.debug_modal_select(index));
                }
                debug_server::Request::ModalCheck => {
                    let _ = tx.send(self.debug_modal_check());
                }
                debug_server::Request::ModalResize { width, height } => {
                    let _ = tx.send(self.debug_modal_resize(width, height));
                }
                debug_server::Request::ScriptCommands => {
                    let names = self.script_list_commands();
                    let json = serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string());
                    let _ = tx.send(debug_server::Response::Json(json));
                }
                debug_server::Request::ScriptInvoke { name } => {
                    self.script_send(crate::script_host::EngineCmd::Invoke(name));
                    let _ = tx.send(debug_server::Response::Json("\"ok\"".to_string()));
                }
                debug_server::Request::ScriptEval { code } => {
                    self.script_send(crate::script_host::EngineCmd::Eval(code));
                    let _ = tx.send(debug_server::Response::Json("\"ok\"".to_string()));
                }
                debug_server::Request::KeysState { category } => {
                    let _ = tx.send(self.debug_keys_state(&category));
                }
                debug_server::Request::KeysSelect { category, index } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.select)(index);
                        Ok(())
                    }));
                }
                debug_server::Request::KeysBind { category, command, chord } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| (h.bind)(&command, &chord)));
                }
                debug_server::Request::KeysUnbind { category } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.unbind)();
                        Ok(())
                    }));
                }
                debug_server::Request::KeysReset { category } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.reset)();
                        Ok(())
                    }));
                }
                debug_server::Request::KeysSearch { category, query } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.search)(&query);
                        Ok(())
                    }));
                }
                debug_server::Request::KeysSetView { category, by_key } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.set_view)(by_key);
                        Ok(())
                    }));
                }
                debug_server::Request::KeysSelectChord { category, index } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.select_chord)(index);
                        Ok(())
                    }));
                }
                debug_server::Request::KeysRebind { category, chord } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| (h.rebind)(&chord)));
                }
                debug_server::Request::KeysPick { category, label } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.pick)(label);
                        Ok(())
                    }));
                }
                debug_server::Request::KeysPickCommit { category } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.pick_commit)();
                        Ok(())
                    }));
                }
                debug_server::Request::KeysPickCancel { category } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.pick_cancel)();
                        Ok(())
                    }));
                }
                debug_server::Request::KeysScroll { category, top } => {
                    let _ = tx.send(self.debug_keys_op(&category, |h| {
                        (h.scroll)(top);
                        Ok(())
                    }));
                }
            }
        }
    }

    /// `POST /command/<Name>` の振り分け。非モーダルは実行後 state を返す。モーダルを開くコマンドは
    /// 先に応答を返してから exec（ネストループでブロック）。未対応コマンドは弾く。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_dispatch_command(
        &self,
        name: &str,
        args: Vec<String>,
        tx: Sender<debug_server::Response>,
    ) {
        let Some(cmd) = Command::from_token(name) else {
            let _ = tx.send(debug_server::Response::BadRequest(format!(
                "unknown command: {name}"
            )));
            return;
        };
        let inv = Invocation::new(cmd, args);
        let is_left = !self.active_right.get();
        // 表示中ビューアのコマンドはそのビューア文脈で実行する。
        let viewer_is_text = match self.active_view.get() {
            ActiveView::Text if cmd.available_in(CommandContext::TextViewer) => Some(true),
            ActiveView::Media if cmd.available_in(CommandContext::ImageViewer) => Some(false),
            _ => None,
        };
        if let Some(is_text) = viewer_is_text {
            // 検索など、モーダルを開き得るビューアコマンドは exec がモーダルを閉じるまで
            // ブロックする。単一スレッドの HTTP が `/modal/*` を捌けなくなるのを避け、先に応答する。
            if !matches!(debug_command_class(cmd), DebugCmdClass::NonModal) {
                let _ = tx.send(debug_server::Response::Json("{\"maybe_modal\":true}".to_string()));
                let _ = if is_text { self.exec_viewer(&inv) } else { self.exec_media(&inv) };
                return;
            }
            let result = if is_text { self.exec_viewer(&inv) } else { self.exec_media(&inv) };
            let r = match result {
                Ok(()) => {
                    self.settle_pending_jobs();
                    debug_server::Response::Json(self.debug_state_value().to_string())
                }
                Err(e) => debug_server::Response::Error(format!("exec error: {e}")),
            };
            let _ = tx.send(r);
            return;
        }
        match debug_command_class(cmd) {
            DebugCmdClass::NonModal => {
                let r = match self.exec(is_left, &inv) {
                    Ok(()) => {
                        self.settle_pending_jobs();
                        debug_server::Response::Json(self.debug_state_value().to_string())
                    }
                    Err(e) => debug_server::Response::Error(format!("exec error: {e}")),
                };
                let _ = tx.send(r);
            }
            DebugCmdClass::MaybeModal => {
                // 読取系だが暗号化書庫等でモーダルを開き得る。単一スレッドの HTTP が
                // モーダル待ちで詰まり `/modal/*` を捌けなくなる（デッドロック）のを避け、
                // exec の前に応答を返す。モーダルが出なければそのまま実行が終わる。
                let _ = tx.send(debug_server::Response::Json(
                    "{\"maybe_modal\":true}".to_string(),
                ));
                let _ = self.exec(is_left, &inv);
            }
            DebugCmdClass::ModalWrite => {
                if !self.debug.allow_write {
                    let _ = tx.send(debug_server::Response::BadRequest(format!(
                        "write disabled; restart with --debug-allow-write to run: {name}"
                    )));
                    return;
                }
                // モーダルを開く前に応答（exec はモーダルが閉じるまでブロックするため）。
                let _ = tx.send(debug_server::Response::Json(
                    "{\"modal_opening\":true}".to_string(),
                ));
                let _ = self.exec(is_left, &inv);
            }
        }
    }

    /// 最前面モーダルの HWND を得る（無ければ None）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_modal_hwnd(&self) -> Option<w::HWND> {
        let ptr = debug_server::modal_registry::with_top(|t| t.map(|e| e.modal_ptr))?;
        Some(unsafe { w::HWND::from_ptr(ptr as *mut std::ffi::c_void) })
    }

    /// モーダル内の最初の Edit 子コントロールを探す。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_modal_edit(modal: &w::HWND) -> Option<w::HWND> {
        let mut found: Option<w::HWND> = None;
        modal.EnumChildWindows(|c| {
            if c.GetClassName().map(|s| s.eq_ignore_ascii_case("Edit")).unwrap_or(false) {
                found = Some(c);
                false
            } else {
                true
            }
        });
        found
    }

    /// モーダル内の最初のチェックボックスへクリックを送り、チェック状態をトグルする
    /// （`POST /modal/check`）。BM_CLICK なので状態反転＋親への BN_CLICKED 通知まで起きる。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_modal_check(&self) -> debug_server::Response {
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        let mut found: Option<w::HWND> = None;
        modal.EnumChildWindows(|c| {
            let is_btn =
                c.GetClassName().map(|s| s.eq_ignore_ascii_case("Button")).unwrap_or(false);
            let style = c.GetWindowLongPtr(co::GWLP::STYLE) as u32;
            // BS_CHECKBOX(2)/BS_AUTOCHECKBOX(3) の下位ビット一致でチェックボックスを拾う
            // （押しボタン=0/1・ラジオ=9 は除外）。
            if is_btn && matches!(style & 0xF, 2 | 3) {
                found = Some(c);
                false
            } else {
                true
            }
        });
        let Some(cb) = found else {
            return debug_server::Response::BadRequest("modal has no checkbox".into());
        };
        const BM_CLICK: u32 = 0x00F5;
        unsafe {
            let _ = cb.PostMessage(w::msg::WndMsg {
                msg_id: co::WM::from_raw(BM_CLICK),
                wparam: 0,
                lparam: 0,
            });
        }
        debug_server::Response::Json(self.debug_state_value().to_string())
    }

    /// モーダル内の最初の ListBox 子コントロールを探す。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_modal_listbox(modal: &w::HWND) -> Option<w::HWND> {
        let mut found: Option<w::HWND> = None;
        modal.EnumChildWindows(|c| {
            if c.GetClassName().map(|s| s.eq_ignore_ascii_case("ListBox")).unwrap_or(false) {
                found = Some(c);
                false
            } else {
                true
            }
        });
        found
    }

    /// `POST /modal/select/<index>`：リスト選択モーダルの選択行を設定する。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_modal_select(&self, index: usize) -> debug_server::Response {
        // 多列 ListView モーダルはフックで選択する。
        let used = debug_server::modal_registry::with_top(|t| match t.and_then(|e| e.list_view.as_ref()) {
            Some(h) => {
                (h.select)(index);
                true
            }
            None => false,
        });
        if used {
            return debug_server::Response::Json(self.debug_state_value().to_string());
        }
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        match Self::debug_modal_listbox(&modal) {
            Some(list) => {
                unsafe {
                    let _ = list.SendMessage(w::msg::lb::SetCurSel { index: Some(index as u32) });
                }
                debug_server::Response::Json(self.debug_state_value().to_string())
            }
            None => debug_server::Response::BadRequest("modal has no list".into()),
        }
    }

    /// `POST /modal/resize/<w>x<h>`：開いているモーダルの窓サイズを w×h（物理px）へ変える。
    /// `SetWindowPos` で WM_SIZE を飛ばし、リサイズ追従するダイアログの再レイアウトを headless で
    /// 撮って検証できるようにする（手動ドラッグの代替手段）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_modal_resize(&self, width: i32, height: i32) -> debug_server::Response {
        if width <= 0 || height <= 0 {
            return debug_server::Response::BadRequest("resize needs positive w/h".into());
        }
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        if let Err(e) = modal.SetWindowPos(
            w::HwndPlace::None,
            w::POINT::with(0, 0),
            w::SIZE::with(width, height),
            co::SWP::NOMOVE | co::SWP::NOZORDER | co::SWP::NOACTIVATE,
        ) {
            return debug_server::Response::Error(format!("resize failed: {e}"));
        }
        debug_server::Response::Json(self.debug_state_value().to_string())
    }

    /// `POST /modal/key/<key>`：開いているモーダルへキー送出。`<key>` は `enter`/`esc`/`tab`/
    /// `shift`/矢印/英数字など。`<key>/down`・`<key>/up` で押下のみ・解放のみを送れる
    /// （「Shift を押している間だけ」のような down/up を分離して検証するため）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_modal_key(&self, key: &str) -> debug_server::Response {
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        // "shift/down" のように phase を付けられる（無ければ down→up の完全押下）。
        let (name, phase) = key.split_once('/').map_or((key, None), |(n, p)| (n, Some(p)));
        let lk = name.to_ascii_lowercase();
        let vk: u16 = match lk.as_str() {
            "enter" | "return" => 0x0D,
            "esc" | "escape" => 0x1B,
            "tab" => 0x09,
            "shift" => 0x10,
            "space" => 0x20,
            "left" => 0x25,
            "up" => 0x26,
            "right" => 0x27,
            "down" => 0x28,
            "home" => 0x24,
            "end" => 0x23,
            s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() => {
                s.as_bytes()[0].to_ascii_uppercase() as u16
            }
            s if s.len() == 1 && s.as_bytes()[0].is_ascii_digit() => s.as_bytes()[0] as u16,
            _ => return debug_server::Response::BadRequest(format!("unknown modal key: {key}")),
        };
        // 実キー入力はフォーカス中の子へ届く。IsDialogMessage の矢印グループ移動は
        // 子宛メッセージでないと発動しないため、フォーカス中の窓へ送る（無ければモーダルへ）。
        let send_down = phase != Some("up");
        let send_up = phase != Some("down");
        // フォーカスがモーダル内に無いと IsDialogMessage が enter/esc/矢印を翻訳できず、
        // 一発勝負の合成キーが取りこぼされる（非アクティブ・オフスクリーン時に稀に起きる）。
        // UI スレッドのこのタイミングで先頭タブストップへフォーカスを寄せ、確実に届かせる。
        let focus_in_modal = w::HWND::GetFocus()
            .map(|f| f.ptr() == modal.ptr() || modal.IsChild(&f))
            .unwrap_or(false);
        if !focus_in_modal
            && let Ok(first) = modal.GetNextDlgTabItem(&w::HWND::NULL, false)
                && !first.ptr().is_null() {
                    first.SetFocus();
                }
        let focus = w::HWND::GetFocus();
        let target = focus.as_ref().unwrap_or(&modal);
        unsafe {
            if send_down {
                let _ = target.PostMessage(w::msg::WndMsg {
                    msg_id: co::WM::KEYDOWN,
                    wparam: vk as usize,
                    lparam: 0,
                });
            }
            if send_up {
                let _ = target.PostMessage(w::msg::WndMsg {
                    msg_id: co::WM::from_raw(0x0101), // WM_KEYUP
                    wparam: vk as usize,
                    lparam: 0,
                });
            }
        }
        debug_server::Response::Json(self.debug_state_value().to_string())
    }

    /// `POST /modal/text`：開いているモーダルの入力欄へ文字列を設定する。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_modal_text(&self, value: &str) -> debug_server::Response {
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        match Self::debug_modal_edit(&modal) {
            Some(edit) => {
                let _ = edit.SetWindowText(value);
                debug_server::Response::Json(self.debug_state_value().to_string())
            }
            None => debug_server::Response::BadRequest("modal has no text field".into()),
        }
    }

    /// `POST /modal/command/<role>`：開いているモーダルのボタンを役割名/ラベル/ctrl_id で押す。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_modal_command(&self, role: &str) -> debug_server::Response {
        let Some(modal) = self.debug_modal_hwnd() else {
            return debug_server::Response::BadRequest("no modal open".into());
        };
        // 役割名・数値 ctrl_id・ラベル部分一致から ctrl_id を解決する。
        let id = debug_server::modal_registry::with_top(|t| {
            let e = t?;
            let r = role.to_ascii_lowercase();
            if r == "ok" || r == "yes" {
                return Some(1u16);
            }
            if r == "cancel" {
                return Some(2u16);
            }
            if let Ok(n) = role.parse::<u16>() {
                return Some(n);
            }
            e.buttons
                .iter()
                .find(|(label, _)| label.replace('&', "").to_lowercase().contains(&r))
                .map(|(_, id)| *id)
        });
        let Some(id) = id else {
            return debug_server::Response::BadRequest(format!("unknown modal button: {role}"));
        };
        let Ok(btn) = modal.GetDlgItem(id) else {
            return debug_server::Response::BadRequest(format!("button id {id} not found"));
        };
        // 親へ WM_COMMAND(BN_CLICKED) を送る（winsafe の bn_clicked が ctrl_id で振り分ける）。
        unsafe {
            let _ = modal.PostMessage(w::msg::WndMsg {
                msg_id: co::WM::COMMAND,
                wparam: id as usize,
                lparam: btn.ptr() as isize,
            });
        }
        debug_server::Response::Json(self.debug_state_value().to_string())
    }

    /// `POST /view/key/<action>`：重ね表示中ビューアの操作。`next`/`prev`/`close` の特殊操作のほか、
    /// キーチョード名（`Esc`・`B`・`Ctrl+F` 等）を渡すと、表示中のビューアの実キー経路
    /// （キーマップ解決→コマンド実行）へそのまま流す。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_view_key(&self, action: &str) -> debug_server::Response {
        let r = match action {
            "next" => self.media.navigate(1),
            "prev" => self.media.navigate(-1),
            "close" => self.close_viewer(),
            _ => {
                let Some(chord) = rerics_core::KeyChord::parse(action) else {
                    return debug_server::Response::BadRequest(format!(
                        "unknown view action: {action}"
                    ));
                };
                match self.active_view.get() {
                    ActiveView::Text => self.viewer_key(chord.vk, chord.ctrl, chord.shift),
                    ActiveView::Media => self.media_key(chord.vk, chord.ctrl, chord.shift),
                    ActiveView::None => {
                        return debug_server::Response::BadRequest("no viewer active".to_string());
                    }
                }
            }
        };
        match r {
            Ok(()) => debug_server::Response::Json(self.debug_state_value().to_string()),
            Err(e) => debug_server::Response::Error(format!("view key error: {e}")),
        }
    }

    /// `POST /view/search`：テキストビューアの検索バーへ文字列を入れて即時検索する。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_view_search(&self, value: &str) -> debug_server::Response {
        if !matches!(self.active_view.get(), ActiveView::Text) {
            return debug_server::Response::BadRequest("text viewer not active".into());
        }
        match self.viewer.debug_set_bar_text(value) {
            Ok(()) => debug_server::Response::Json(self.debug_state_value().to_string()),
            Err(e) => debug_server::Response::Error(format!("view search error: {e}")),
        }
    }

    /// `POST /view/search/key/<key>`：検索バーのキー操作（down/up/enter/esc）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_view_search_key(&self, key: &str) -> debug_server::Response {
        if !matches!(self.active_view.get(), ActiveView::Text) {
            return debug_server::Response::BadRequest("text viewer not active".into());
        }
        let r = match key.to_ascii_lowercase().as_str() {
            "down" | "next" => self.viewer.find_next(true),
            "up" | "prev" => self.viewer.find_next(false),
            "enter" | "return" => self.viewer.confirm_search_bar(),
            "esc" | "escape" => self.viewer.cancel_search_bar(),
            _ => return debug_server::Response::BadRequest(format!("unknown search key: {key}")),
        };
        match r {
            Ok(()) => debug_server::Response::Json(self.debug_state_value().to_string()),
            Err(e) => debug_server::Response::Error(format!("view search key error: {e}")),
        }
    }

    /// `POST /view/search/option/<name>/<on|off>`：検索オプション（case/word/regex）を切り替える。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_view_search_option(&self, name: &str, on: bool) -> debug_server::Response {
        if !matches!(self.active_view.get(), ActiveView::Text) {
            return debug_server::Response::BadRequest("text viewer not active".into());
        }
        match self.viewer.debug_set_option(name, on) {
            Ok(true) => debug_server::Response::Json(self.debug_state_value().to_string()),
            Ok(false) => {
                debug_server::Response::BadRequest(format!("unknown search option: {name}"))
            }
            Err(e) => debug_server::Response::Error(format!("view search option error: {e}")),
        }
    }

    /// `POST /view/search/history/<index>`：検索履歴の index 番目（新しい順）を選んで検索する。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_view_search_history(&self, index: usize) -> debug_server::Response {
        if !matches!(self.active_view.get(), ActiveView::Text) {
            return debug_server::Response::BadRequest("text viewer not active".into());
        }
        match self.viewer.debug_select_history(index) {
            Ok(true) => debug_server::Response::Json(self.debug_state_value().to_string()),
            Ok(false) => debug_server::Response::BadRequest(format!("no history at {index}")),
            Err(e) => debug_server::Response::Error(format!("view search history error: {e}")),
        }
    }

    /// `POST /view/search/dropdown/<open|close>`：履歴ドロップダウンを開く/閉じる。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_view_search_dropdown(&self, open: bool) -> debug_server::Response {
        if !matches!(self.active_view.get(), ActiveView::Text) {
            return debug_server::Response::BadRequest("text viewer not active".into());
        }
        match self.viewer.debug_dropdown(open) {
            Ok(()) => debug_server::Response::Json(self.debug_state_value().to_string()),
            Err(e) => debug_server::Response::Error(format!("view search dropdown error: {e}")),
        }
    }

    /// `POST /view/search/mnemonic/<c|w|r>`：トグルのニーモニック（Alt+C/W/R 相当）を駆動する。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_view_search_mnemonic(&self, key: char) -> debug_server::Response {
        if !matches!(self.active_view.get(), ActiveView::Text) {
            return debug_server::Response::BadRequest("text viewer not active".into());
        }
        match self.viewer.debug_mnemonic(key) {
            Ok(_) => debug_server::Response::Json(self.debug_state_value().to_string()),
            Err(e) => debug_server::Response::Error(format!("view search mnemonic error: {e}")),
        }
    }

    /// `GET /snapshot[/<spec>]`：画面 PNG を返す。spec は全体／名前付き要素／数値範囲／要素相対範囲。
    /// 名前付き要素の矩形は復帰後レイアウトで確定するため、撮影準備（復帰＋再レイアウト）を先に行う。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_snapshot(&self, spec: &str) -> debug_server::Response {
        let was_min = self.debug_prepare_capture();
        let result = self.debug_snapshot_inner(spec);
        if was_min {
            self.wnd.hwnd().ShowWindow(co::SW::SHOWMINNOACTIVE);
        }
        result
    }

    /// 撮影準備：最小化中なら復帰し、ループ停止中でも子が正しい位置・内容になるよう手動再レイアウト＋同期再描画。
    /// 撮影は窓自身の DC から行う（オクルージョン非依存）ので最前面化は不要。戻り値は「元が最小化だったか」。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_prepare_capture(&self) -> bool {
        let hwnd = self.wnd.hwnd();
        let was_min = hwnd.IsIconic();
        if was_min {
            hwnd.ShowWindow(co::SW::SHOWNOACTIVATE);
            // WM_SIZE はループ停止中で届かないので、復帰後サイズで明示的に再レイアウトする。
            let _ = self.layout();
        }
        was_min
    }

    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_snapshot_inner(&self, spec: &str) -> debug_server::Response {
        let segs: Vec<&str> = spec.split('/').filter(|s| !s.is_empty()).collect();
        let (base_name, region): (Option<&str>, Option<&str>) = match segs.as_slice() {
            [] => (None, None),
            [a] if parse_region(a).is_some() => (None, Some(*a)),
            [a] => (Some(*a), None),
            [a, b] => (Some(*a), Some(*b)),
            _ => return debug_server::Response::BadRequest(format!("bad snapshot spec: {spec}")),
        };
        // 対象窓と基準矩形を決める：`modal`/`modal_*` はモーダル窓、それ以外は main クライアント。
        let is_modal = base_name
            .map(|n| n == "modal" || n.starts_with("modal_"))
            .unwrap_or(false);
        let (buf, cw, ch, base) = if is_modal {
            let Some(modal) = self.debug_modal_hwnd() else {
                return debug_server::Response::BadRequest("no modal open".into());
            };
            // モーダルは標準コントロール製なので PrintWindow（WM_PRINTCLIENT 応答）で撮る。
            match self.capture_modal_print(&modal) {
                Ok((b, w, h)) => (b, w, h, (0, 0, w, h)),
                Err(e) => return debug_server::Response::Error(format!("snapshot error: {e}")),
            }
        } else {
            let base = match base_name {
                Some(name) => match self.debug_rect(name) {
                    Some(r) => r,
                    None => {
                        return debug_server::Response::BadRequest(format!(
                            "unknown snapshot target: {name}"
                        ));
                    }
                },
                None => match self.debug_rect("client") {
                    Some(r) => r,
                    None => return debug_server::Response::Error("no client rect".into()),
                },
            };
            match self.capture_render_bgra() {
                Ok((b, w, h)) => (b, w, h, base),
                Err(e) => return debug_server::Response::Error(format!("snapshot error: {e}")),
            }
        };
        let rect = match region {
            Some(rs) => match parse_region(rs) {
                Some((rx, ry, rw, rh)) => (base.0 + rx, base.1 + ry, rw, rh),
                None => return debug_server::Response::BadRequest(format!("bad region: {rs}")),
            },
            None => base,
        };
        match Self::crop_bgra_to_png(&buf, cw, ch, rect) {
            Ok(png) => debug_server::Response::Png(png),
            Err(e) => debug_server::Response::Error(format!("snapshot error: {e}")),
        }
    }

    /// 名前付き要素のクライアント座標矩形 `(x,y,w,h)`。`_left`/`_right` 省略時はアクティブ側。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_rect(&self, name: &str) -> Option<(i32, i32, i32, i32)> {
        let a = !self.active_right.get();
        match name {
            "full" | "client" | "window" => {
                let rc = self.wnd.hwnd().GetClientRect().ok()?;
                Some((0, 0, rc.right - rc.left, rc.bottom - rc.top))
            }
            "tab_bar" => self.rect_in_client(self.tab_bar.hwnd()),
            "log" => self.rect_in_client(self.log.hwnd()),
            "path_bar" => self.rect_in_client(self.bar(a).hwnd()),
            "path_bar_left" => self.rect_in_client(self.bar(true).hwnd()),
            "path_bar_right" => self.rect_in_client(self.bar(false).hwnd()),
            "list" => self.rect_in_client(self.view(a).hwnd()),
            "list_left" => self.rect_in_client(self.view(true).hwnd()),
            "list_right" => self.rect_in_client(self.view(false).hwnd()),
            "status_bar" => self.rect_in_client(self.status(a).hwnd()),
            "status_bar_left" => self.rect_in_client(self.status(true).hwnd()),
            "status_bar_right" => self.rect_in_client(self.status(false).hwnd()),
            "pane" => self.pane_bbox(a),
            "pane_left" => self.pane_bbox(true),
            "pane_right" => self.pane_bbox(false),
            "cursor" => self.cursor_rect(a),
            "cursor_left" => self.cursor_rect(true),
            "cursor_right" => self.cursor_rect(false),
            // モーダルはクライアント座標に収まらない別窓なので、ここでは扱わず capture 側で別処理。
            _ => None,
        }
    }

    /// 子コントロールの矩形を main 窓のクライアント座標へ変換する。
    #[cfg(feature = "debug-server")]
    pub(crate) fn rect_in_client(&self, child: &w::HWND) -> Option<(i32, i32, i32, i32)> {
        let wr = child.GetWindowRect().ok()?;
        let origin = self.wnd.hwnd().ClientToScreen(w::POINT { x: 0, y: 0 }).ok()?;
        Some((
            wr.left - origin.x,
            wr.top - origin.y,
            wr.right - wr.left,
            wr.bottom - wr.top,
        ))
    }

    /// ペイン全体（パスバー＋一覧＋ステータス）の外接矩形。
    #[cfg(feature = "debug-server")]
    pub(crate) fn pane_bbox(&self, is_left: bool) -> Option<(i32, i32, i32, i32)> {
        let a = self.rect_in_client(self.bar(is_left).hwnd())?;
        let b = self.rect_in_client(self.view(is_left).hwnd())?;
        let c = self.rect_in_client(self.status(is_left).hwnd())?;
        let x0 = a.0.min(b.0).min(c.0);
        let y0 = a.1.min(b.1).min(c.1);
        let x1 = (a.0 + a.2).max(b.0 + b.2).max(c.0 + c.2);
        let y1 = (a.1 + a.3).max(b.1 + b.3).max(c.1 + c.3);
        Some((x0, y0, x1 - x0, y1 - y0))
    }

    /// アクティブ/指定ペインのカーソル行の矩形（一覧内の行位置を main クライアント座標へ）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn cursor_rect(&self, is_left: bool) -> Option<(i32, i32, i32, i32)> {
        let lr = self.rect_in_client(self.view(is_left).hwnd())?;
        let (cx, cy, cw, ch) = self.view(is_left).cursor_row_rect()?;
        Some((lr.0 + cx, lr.1 + cy, cw, ch))
    }


    /// 子コントロールを自前 bitmap へ `render_to` し、合成 DC の位置へ BitBlt する。
    #[cfg(feature = "debug-server")]
    pub(crate) fn render_view_into(
        &self,
        target: &w::HDC,
        win_dc: &w::HDC,
        child: &w::HWND,
        draw: impl FnOnce(&w::HDC, i32, i32) -> w::AnyResult<()>,
    ) -> w::AnyResult<()> {
        let Some((x, y, w, h)) = self.rect_in_client(child) else {
            return Ok(());
        };
        if w <= 0 || h <= 0 {
            return Ok(());
        }
        let vdc = win_dc.CreateCompatibleDC()?;
        let vbmp = win_dc.CreateCompatibleBitmap(w, h)?;
        let _s = vdc.SelectObject(&*vbmp)?;
        draw(&vdc, w, h)?;
        target.BitBlt(
            w::POINT { x, y },
            w::SIZE { cx: w, cy: h },
            &vdc,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    /// モーダル（標準コントロール製）を `PrintWindow` でクライアント領域を BGRA(top-down) に撮る。
    /// 標準コントロールは `WM_PRINTCLIENT` に応答するので、自前描画と違い PrintWindow が機能する。
    #[cfg(feature = "debug-server")]
    pub(crate) fn capture_modal_print(&self, modal: &w::HWND) -> w::AnyResult<(Vec<u8>, i32, i32)> {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn PrintWindow(
                hwnd: *mut std::ffi::c_void,
                hdc: *mut std::ffi::c_void,
                flags: u32,
            ) -> i32;
        }
        const PW_CLIENTONLY: u32 = 1;
        const PW_RENDERFULLCONTENT: u32 = 2;
        let crc = modal.GetClientRect()?;
        let (cw, ch) = (crc.right - crc.left, crc.bottom - crc.top);
        if cw <= 0 || ch <= 0 {
            return Err("empty modal client".into());
        }
        let win_dc = modal.GetDC()?;
        let memdc = win_dc.CreateCompatibleDC()?;
        let bmp = win_dc.CreateCompatibleBitmap(cw, ch)?;
        {
            let _sel = memdc.SelectObject(&*bmp)?;
            unsafe {
                PrintWindow(modal.ptr(), memdc.ptr(), PW_CLIENTONLY | PW_RENDERFULLCONTENT);
            }
        }
        let mut bmi = w::BITMAPINFO::default();
        bmi.bmiHeader.biWidth = cw;
        bmi.bmiHeader.biHeight = -ch;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = co::BI::RGB;
        let mut buf = vec![0u8; (cw as usize) * (ch as usize) * 4];
        unsafe {
            memdc.GetDIBits(&bmp, 0, ch as u32, Some(&mut buf), &mut bmi, co::DIB::RGB_COLORS)?;
        }
        Ok((buf, cw, ch))
    }

    /// メインクライアント全体を各 view の `render_to` から合成し BGRA(top-down) で得る。
    /// 窓のピクセルを読まず状態から描き起こすため、**非表示（headless）でも決定論的に撮れる**。
    #[cfg(feature = "debug-server")]
    pub(crate) fn capture_render_bgra(&self) -> w::AnyResult<(Vec<u8>, i32, i32)> {
        let hwnd = self.wnd.hwnd();
        let crc = hwnd.GetClientRect()?;
        let (cw, ch) = (crc.right - crc.left, crc.bottom - crc.top);
        if cw <= 0 || ch <= 0 {
            return Err("empty client".into());
        }
        let win_dc = hwnd.GetDC()?;
        let target = win_dc.CreateCompatibleDC()?;
        let bmp = win_dc.CreateCompatibleBitmap(cw, ch)?;
        let _sel = target.SelectObject(&*bmp)?;
        // 隙間（chrome）の下地をシステム 3D グレーで塗る。
        let base = w::HBRUSH::CreateSolidBrush(w::GetSysColor(co::COLOR::BTNFACE))?;
        target.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &base)?;

        self.render_view_into(&target, &win_dc, self.tab_bar.hwnd(), |d, w, h| {
            self.tab_bar.render_to(d, w, h)
        })?;
        for is_left in [true, false] {
            self.render_view_into(&target, &win_dc, self.bar(is_left).hwnd(), |d, w, h| {
                self.bar(is_left).render_to(d, w, h)
            })?;
            self.render_view_into(&target, &win_dc, self.view(is_left).hwnd(), |d, w, h| {
                self.view(is_left).render_to(d, w, h)
            })?;
            self.render_view_into(&target, &win_dc, self.status(is_left).hwnd(), |d, w, h| {
                self.status(is_left).render_to(d, w, h)
            })?;
        }
        self.render_view_into(&target, &win_dc, self.splitter.hwnd(), |d, w, h| {
            self.splitter.render_to(d, w, h)
        })?;
        self.render_view_into(&target, &win_dc, self.log.hwnd(), |d, w, h| {
            self.log.render_to(d, w, h)
        })?;
        // 重ね表示中のビューア/メディアを最前面として上書きする。
        match self.active_view.get() {
            ActiveView::Text => {
                self.render_view_into(&target, &win_dc, self.viewer.hwnd(), |d, w, h| {
                    self.viewer.render_to(d, w, h)
                })?;
            }
            ActiveView::Media => {
                self.render_view_into(&target, &win_dc, self.media.hwnd(), |d, w, h| {
                    self.media.render_to(d, w, h)
                })?;
            }
            ActiveView::None => {}
        }

        let mut bmi = w::BITMAPINFO::default();
        bmi.bmiHeader.biWidth = cw;
        bmi.bmiHeader.biHeight = -ch;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = co::BI::RGB;
        let mut buf = vec![0u8; (cw as usize) * (ch as usize) * 4];
        unsafe {
            target.GetDIBits(&bmp, 0, ch as u32, Some(&mut buf), &mut bmi, co::DIB::RGB_COLORS)?;
        }
        Ok((buf, cw, ch))
    }

    /// 合成済み BGRA(top-down) バッファから矩形を切り出して PNG にする。
    #[cfg(feature = "debug-server")]
    pub(crate) fn crop_bgra_to_png(
        full: &[u8],
        cw: i32,
        ch: i32,
        rect: (i32, i32, i32, i32),
    ) -> w::AnyResult<Vec<u8>> {
        let (rx, ry, rw, rh) = rect;
        if rw <= 0 || rh <= 0 {
            return Err("empty snapshot region".into());
        }
        let mut out = vec![0u8; (rw as usize) * (rh as usize) * 4];
        for row in 0..rh {
            let sy = ry + row;
            if sy < 0 || sy >= ch {
                continue;
            }
            for col in 0..rw {
                let sx = rx + col;
                if sx < 0 || sx >= cw {
                    continue;
                }
                let si = ((sy * cw + sx) * 4) as usize;
                let di = ((row * rw + col) * 4) as usize;
                // BGRA → RGBA、アルファは不透明に。
                out[di] = full[si + 2];
                out[di + 1] = full[si + 1];
                out[di + 2] = full[si];
                out[di + 3] = 255;
            }
        }
        let img = image::RgbaImage::from_raw(rw as u32, rh as u32, out)
            .ok_or("rgba buffer size mismatch")?;
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)?;
        Ok(buf.into_inner())
    }

    /// 現在の UI 状態を JSON 値で組む（画面構成要素ほぼ全部・サブツリーは呼び側が JSON Pointer で抽出）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_state_value(&self) -> serde_json::Value {
        use serde_json::json;
        let active_view = match self.active_view.get() {
            ActiveView::None => "none",
            ActiveView::Text => "text",
            ActiveView::Media => "media",
        };
        let media = if matches!(self.active_view.get(), ActiveView::Media) {
            let (index, total) = self.media.nav_position();
            json!({
                "index": index,
                "total": total,
                "title": self.media.title(),
                "mode": self.media.display_mode(),
                "scale_percent": self.media.scale_percent(),
            })
        } else {
            serde_json::Value::Null
        };
        let viewer = if matches!(self.active_view.get(), ActiveView::Text) {
            let (search, pos, count, opts) = self.viewer.debug_search_state();
            json!({
                "search": search,
                "search_open": self.viewer.is_search_bar_open(),
                "match": pos.map(|(l, c, len)| json!({ "line": l, "col": c, "len": len })),
                "match_count": count,
                "case_sensitive": opts.case_sensitive,
                "whole_word": opts.whole_word,
                "regex": opts.regex,
                "history": self.viewer.debug_history(),
                "list_open": self.viewer.debug_is_dropdown_open(),
            })
        } else {
            serde_json::Value::Null
        };
        let tabs: Vec<serde_json::Value> = self
            .tabs
            .borrow()
            .iter()
            .map(|t| {
                json!({ "left": t.left_path, "right": t.right_path, "active_right": t.active_right })
            })
            .collect();
        let log_lines: Vec<serde_json::Value> = self
            .log
            .tail(50)
            .into_iter()
            .map(|(level, text)| json!({ "level": level, "text": text }))
            .collect();
        let modal = debug_server::modal_registry::with_top(|t| match t {
            None => serde_json::Value::Null,
            Some(e) => {
                let input = if e.has_input {
                    let m = unsafe { w::HWND::from_ptr(e.modal_ptr as *mut std::ffi::c_void) };
                    Self::debug_modal_edit(&m)
                        .and_then(|ed| ed.GetWindowText().ok())
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null)
                } else {
                    serde_json::Value::Null
                };
                // 多列 ListView はフックから行・選択をライブで読む（プログレッシブ更新も反映）。
                // 単列 ListBox は実コントロールから選択を読む。それ以外は静的値。
                let (rows, headers, selected) = match &e.list_view {
                    Some(h) => {
                        let (rows, sel) = (h.read)();
                        (rows, h.headers.clone(), sel)
                    }
                    None => {
                        let selected = if e.items.is_empty() {
                            e.selected
                        } else {
                            let m = unsafe { w::HWND::from_ptr(e.modal_ptr as *mut std::ffi::c_void) };
                            Self::debug_modal_listbox(&m)
                                .and_then(|l| unsafe { l.SendMessage(w::msg::lb::GetCurSel {}) })
                                .map(|n| n as usize)
                                .unwrap_or(e.selected)
                        };
                        (Vec::new(), Vec::new(), selected)
                    }
                };
                json!({
                    "kind": e.kind,
                    "title": e.title,
                    "prompt": e.prompt,
                    "has_input": e.has_input,
                    "input": input,
                    "items": e.items,
                    "rows": rows,
                    "headers": headers,
                    "selected": selected,
                    "buttons": e.buttons.iter().map(|(l, id)| json!({ "label": l, "id": id })).collect::<Vec<_>>(),
                })
            }
        });
        json!({
            "window": {
                "title": self.wnd.hwnd().GetWindowText().unwrap_or_default(),
                "maximized": self.maximized.get(),
                "split_ratio": self.split_ratio.get(),
            },
            "active_pane": if self.active_right.get() { "right" } else { "left" },
            "active_view": active_view,
            "panes": {
                "left": self.debug_pane_json(true),
                "right": self.debug_pane_json(false),
            },
            "modal": modal,
            "media": media,
            "viewer": viewer,
            "tab_bar": { "active": self.active.get(), "labels": self.tab_bar.labels() },
            "tabs": { "active": self.active.get(), "count": tabs.len(), "items": tabs },
            "log": { "lines": log_lines },
        })
    }

    /// 解決済みの外見情報を JSON 値で組む（設定が描画に反映されているかのテスト用）。
    /// 上位＝解決後の設定値（テーマ/配色/フォント/レイアウト寸法）、panes＝各ペインが
    /// 実際に保持している値（apply_config の配線確認）。いずれも `paint_to` が読むのと同じ出どころ。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_presentation_value(&self) -> serde_json::Value {
        let cfg = self.config.borrow();
        let mut v =
            debug_json::presentation_top_json(&cfg.theme, &cfg.active_colors(), &cfg.font, &cfg.layout);
        v["panes"] = serde_json::json!({
            "left": self.view(true).presentation(),
            "right": self.view(false).presentation(),
        });
        v
    }

    /// 片側ペインの状態を JSON 値で組む。GUI から値を集め、純粋関数 `debug_json::pane_state_json`
    /// に渡すだけの薄い層（シリアライズ本体はそちらでユニットテスト済み）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_pane_json(&self, is_left: bool) -> serde_json::Value {
        let (location, is_archive) = {
            let pane = self.pane(is_left).borrow();
            (pane.loc_display(), pane.is_archive())
        };
        let view = self.view(is_left);
        let page_rows = view.page_rows();
        let mask = self.mask(is_left).borrow().clone();
        let path_bar = self.bar(is_left).text();
        let status_left = self.status(is_left).left_text();
        let status_right = self.status(is_left).right_text();
        let chrome = debug_json::PaneChrome {
            location: &location,
            is_archive,
            page_rows,
            mask: mask.as_deref(),
            path_bar: &path_bar,
            status_left: &status_left,
            status_right: &status_right,
        };
        let st = view.state();
        let s = st.borrow();
        debug_json::pane_state_json(&s, &chrome)
    }

    /// 設定ダイアログのキー編集ページの状態を JSON で返す（開いていなければ 404）。
    pub(crate) fn debug_keys_state(&self, category: &str) -> debug_server::Response {
        match debug_server::modal_registry::with_key_editor(category, |h| (h.read)()) {
            Some(state) => match serde_json::to_string(&state) {
                Ok(json) => debug_server::Response::Json(json),
                Err(e) => debug_server::Response::Error(e.to_string()),
            },
            None => debug_server::Response::NotFound,
        }
    }

    /// 設定ダイアログのキー編集ページを操作する（開いていなければ 404・操作失敗は 400）。
    pub(crate) fn debug_keys_op(
        &self,
        category: &str,
        f: impl FnOnce(&debug_server::modal_registry::KeyEditorHooks) -> Result<(), String>,
    ) -> debug_server::Response {
        match debug_server::modal_registry::with_key_editor(category, f) {
            Some(Ok(())) => debug_server::Response::Json("\"ok\"".to_string()),
            Some(Err(e)) => debug_server::Response::BadRequest(e),
            None => debug_server::Response::NotFound,
        }
    }
}
