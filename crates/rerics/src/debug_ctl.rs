#![cfg(feature = "debug-server")]

use std::sync::mpsc::Sender;
use winsafe::{self as w, co, prelude::*};
use rerics_core::{Command, Invocation};
use crate::{ActiveView, DebugCmdClass, MainWindow, debug_command_class, debug_json, debug_server, parse_region};

impl MainWindow {
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
                    let v = self.debug_state_value();
                    let r = match v.pointer(&pointer) {
                        Some(sub) => debug_server::Response::Json(sub.to_string()),
                        None => debug_server::Response::NotFound,
                    };
                    let _ = tx.send(r);
                }
                debug_server::Request::Presentation { pointer } => {
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
                debug_server::Request::Snapshot { spec } => {
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
        match debug_command_class(cmd) {
            DebugCmdClass::NonModal => {
                let r = match self.exec(is_left, &inv) {
                    Ok(()) => debug_server::Response::Json(self.debug_state_value().to_string()),
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

    /// `POST /view/key/<action>`：重ね表示中ビューアの操作（next/prev/close）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn debug_view_key(&self, action: &str) -> debug_server::Response {
        let r = match action {
            "next" => self.media.navigate(1),
            "prev" => self.media.navigate(-1),
            "close" => self.close_viewer(),
            _ => {
                return debug_server::Response::BadRequest(format!("unknown view action: {action}"));
            }
        };
        match r {
            Ok(()) => debug_server::Response::Json(self.debug_state_value().to_string()),
            Err(e) => debug_server::Response::Error(format!("view key error: {e}")),
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
            memdc.GetDIBits(&*bmp, 0, ch as u32, Some(&mut buf), &mut bmi, co::DIB::RGB_COLORS)?;
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
            target.GetDIBits(&*bmp, 0, ch as u32, Some(&mut buf), &mut bmi, co::DIB::RGB_COLORS)?;
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
            json!({ "index": index, "total": total, "title": self.media.title() })
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
}
