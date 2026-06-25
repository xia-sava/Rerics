use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use winsafe::{self as w, co, prelude::*};
use rerics_core::{Call, Command, KeyChord, Location, MediaKind, open_archive};
use crate::media_view::NavResolver;
use crate::file_list::FileListView;
use crate::{ActiveView, MainWindow, dialog, join_inner_path, viewer};

impl MainWindow {
    pub(crate) fn view(&self, is_left: bool) -> &FileListView {
        if is_left { self.left.list() } else { self.right.list() }
    }

    /// カーソル下のファイルを種別に応じたビューアで開く（ディレクトリ/親は無視）。
    /// 原作 `View` 相当。引数なし＝親へ戻る/ディレクトリ・書庫へ潜る/それ以外は内蔵ビューア
    /// （拡張子で text/media 振り分け）。`type` 指定時はディレクトリでは何もせず、ファイルを
    /// そのビューアで開く（`"text"`/`"bin"` は強制テキスト・他は拡張子振り分け）。
    /// EnterDir（関連付けで外部起動）と違い、ファイルは常に内蔵ビューアで開くのが手触りの差。
    pub(crate) fn view_command(&self, is_left: bool, vtype: Option<&str>) -> w::AnyResult<()> {
        let (is_parent, is_dir, name) = {
            let state = self.view(is_left).state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) => (it.is_parent, it.is_dir, it.name.clone()),
                None => return Ok(()),
            }
        };
        let typed = vtype.map(|t| !t.is_empty()).unwrap_or(false);
        if !typed {
            // 親・ディレクトリ・書庫は潜る（原作 View の type=="" 経路）。
            if is_parent {
                return self.to_parent(is_left);
            }
            if is_dir {
                if self.pane(is_left).borrow_mut().enter(&name, true) {
                    self.reload_side(is_left)?;
                }
                return Ok(());
            }
            if self.pane(is_left).borrow_mut().enter(&name, false) {
                self.reload_side(is_left)?;
                return Ok(());
            }
            return self.view_file(is_left);
        }
        // type 指定時：ディレクトリ/親は何もしない（原作 View）。
        if is_parent || is_dir {
            return Ok(());
        }
        match vtype {
            Some(t) if t.eq_ignore_ascii_case("text") || t.eq_ignore_ascii_case("bin") => {
                self.view_text(is_left, &name)
            }
            _ => self.view_file(is_left),
        }
    }

    pub(crate) fn view_file(&self, is_left: bool) -> w::AnyResult<()> {
        let (name, ext) = {
            let state = self.view(is_left).state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent && !it.is_dir => (it.name.clone(), it.extension.clone()),
                _ => return Ok(()),
            }
        };
        match MediaKind::from_extension(&ext) {
            Some(kind) => self.view_media(is_left, kind, &name),
            None => self.view_text(is_left, &name),
        }
    }

    /// テキスト/バイナリビューアで開く（実FS/書庫内とも bytes 直送）。
    pub(crate) fn view_text(&self, is_left: bool, name: &str) -> w::AnyResult<()> {
        let (bytes, truncated) = match self.read_pane_file(is_left, name, viewer::MAX_VIEW_BYTES) {
            Ok(v) => v,
            Err(e) => {
                self.log.error(&format!("ビューアで開けません: {}: {}", name, e));
                return Ok(());
            }
        };
        self.viewer.open(name, bytes, truncated);
        self.show_viewer(ActiveView::Text)
    }

    /// 画像/動画ビューアで開く。実FS は同ディレクトリの閲覧可能メディアを前後送りに、
    /// 書庫内はカーソル下の1ファイルを一時展開して開く（書庫内の前後送りは後段で対応）。
    pub(crate) fn view_media(&self, is_left: bool, _kind: MediaKind, name: &str) -> w::AnyResult<()> {
        // 別メディアを開くので、前回の書庫プリフェッチがあれば止める。
        self.cancel_media_prefetch();
        let loc = self.pane(is_left).borrow().loc().clone();
        match loc {
            Location::Real(dir) => {
                let target = dir.join(name);
                let mut files: Vec<PathBuf> = Vec::new();
                let mut index = 0;
                {
                    let state = self.view(is_left).state();
                    let s = state.borrow();
                    for it in &s.items {
                        if it.is_dir || it.is_parent {
                            continue;
                        }
                        if MediaKind::from_extension(&it.extension).is_some() {
                            let p = dir.join(&it.name);
                            if p == target {
                                index = files.len();
                            }
                            files.push(p);
                        }
                    }
                }
                if files.is_empty() {
                    files.push(target);
                }
                self.media.open(files, index);
            }
            Location::Archive { archive, inner } => {
                // resolver/プリフェッチが作る temp が登録済みルート配下に来るよう、ここで登録する
                // （セッション中掃除の参照元）。
                self.register_archive_temp(&archive);
                // 同階層の閲覧可能メディアを巡回対象にし（実FS と同じ体験）、表示中の位置を求める。
                // 実パスへの展開は resolver が移動時に1枚ずつ遅延実行する（一括展開しない）。
                let mut entries: Vec<(String, String)> = Vec::new();
                let mut index = 0;
                {
                    let state = self.view(is_left).state();
                    let s = state.borrow();
                    for it in &s.items {
                        if it.is_dir || it.is_parent {
                            continue;
                        }
                        if MediaKind::from_extension(&it.extension).is_some() {
                            if it.name == name {
                                index = entries.len();
                            }
                            entries.push((join_inner_path(&inner, &it.name), it.name.clone()));
                        }
                    }
                }
                if entries.is_empty() {
                    entries.push((join_inner_path(&inner, name), name.to_string()));
                }
                let n = entries.len();
                let entries = Rc::new(entries);

                // 一括展開済みの書庫（ソリッド 7z 等）は temp の実FS を指すだけ＝再展開も
                // プリフェッチも不要。resolver は temp_root 配下の実パスを返す。
                if let Some(root) = self.archive_extracted.borrow().get(&archive).cloned() {
                    let entries2 = entries.clone();
                    let resolver: NavResolver = Rc::new(move |i: usize| {
                        let (inner_file, _nm) = entries2.get(i)?;
                        let p = root.join(Self::inner_to_pathbuf(inner_file));
                        p.is_file().then_some(p)
                    });
                    self.media.open_nav(n, index, resolver);
                    return self.show_viewer(ActiveView::Media);
                }

                // 暗号化メディアなら開く前にパスワードを確保して resolver で使い回す（平文は None）。
                let password = self.ensure_media_password(&archive, entries.get(index).map(|(i, _)| i.as_str()));

                // BG プリフェッチ（§7.6）：現在位置を共有 atomic で伝え、別スレッドが近傍を
                // 先読み展開して共有 mtime キャッシュを温める。FG（resolver）は同期展開で
                // 割り込み、BG は存在チェックでスキップ＝共有キャッシュ越しに協調する。
                let cur = Arc::new(AtomicUsize::new(index));
                let cancel = Arc::new(AtomicBool::new(false));
                *self.media_prefetch.borrow_mut() = Some(cancel.clone());
                {
                    let bg_entries: Vec<(String, String)> = (*entries).clone();
                    let bg_archive = archive.clone();
                    let bg_cur = cur.clone();
                    let bg_cancel = cancel.clone();
                    let bg_shutdown = self.shutdown.clone();
                    std::thread::spawn(move || {
                        Self::media_prefetch_loop(
                            &bg_archive,
                            &bg_entries,
                            &bg_cur,
                            &bg_cancel,
                            &bg_shutdown,
                        );
                    });
                }

                let archive = archive.clone();
                let log = self.log.clone();
                let resolver: NavResolver = Rc::new(move |i: usize| {
                    // BG にカレント位置を伝える（先読みの中心を移動先へ寄せる）。
                    cur.store(i, Ordering::Relaxed);
                    let (inner_file, nm) = entries.get(i)?;
                    match Self::extract_entry_to_temp(&archive, inner_file, password.as_deref()) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            log.error(&format!("書庫内メディアを展開できません: {}: {}", nm, e));
                            None
                        }
                    }
                });
                self.media.open_nav(n, index, resolver);
            }
        }
        self.show_viewer(ActiveView::Media)
    }

    /// 指定ビューアを最前面に出し、もう一方を隠してキー入力を奪う。
    pub(crate) fn show_viewer(&self, which: ActiveView) -> w::AnyResult<()> {
        self.active_view.set(which);
        match which {
            ActiveView::Text => {
                self.media.hwnd().ShowWindow(co::SW::HIDE);
                self.viewer.hwnd().ShowWindow(co::SW::SHOW);
                self.viewer.hwnd().BringWindowToTop()?;
                self.viewer.refresh()?;
            }
            ActiveView::Media => {
                self.viewer.hwnd().ShowWindow(co::SW::HIDE);
                self.media.hwnd().ShowWindow(co::SW::SHOW);
                self.media.hwnd().BringWindowToTop()?;
                self.media.refresh()?;
            }
            ActiveView::None => {}
        }
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// ビューアを閉じてファイラ表示へ戻す。
    pub(crate) fn close_viewer(&self) -> w::AnyResult<()> {
        self.cancel_media_prefetch();
        self.media.stop_playback();
        self.active_view.set(ActiveView::None);
        self.viewer.hwnd().ShowWindow(co::SW::HIDE);
        self.media.hwnd().ShowWindow(co::SW::HIDE);
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// ビューア表示中のキー操作。固定キー（設定対象外）。
    pub(crate) fn viewer_key(&self, vk: u16, ctrl: bool, shift: bool) -> w::AnyResult<()> {
        let chord = KeyChord::new(vk, ctrl, shift, false);
        let resolved = self.viewer_keymap.borrow().resolve_call(&chord);
        if let Some(call) = resolved {
            self.exec_viewer(&call)?;
        }
        Ok(())
    }

    /// テキストビューアのコマンドを実行する（キーバインド・メニューの共通入口）。
    pub(crate) fn exec_viewer(&self, call: &Call) -> w::AnyResult<()> {
        let cmd = match call {
            Call::Builtin { command, .. } => *command,
            Call::Script { source } => {
                self.script_send(crate::script_host::EngineCmd::Eval(source.clone()));
                return Ok(());
            }
        };
        match cmd {
            Command::ViewerClose => self.close_viewer()?,
            Command::ViewerScrollUp => self.viewer.scroll_by(-1)?,
            Command::ViewerScrollDown => self.viewer.scroll_by(1)?,
            Command::ViewerPageUp => self.viewer.scroll_page(false)?,
            Command::ViewerPageDown => self.viewer.scroll_page(true)?,
            Command::ViewerScrollTop => self.viewer.scroll_home()?,
            Command::ViewerScrollBottom => self.viewer.scroll_end()?,
            Command::ViewerChangeEncoding => self.viewer.cycle_encoding(true)?,
            Command::ViewerToggleMode => self.viewer.toggle_mode()?,
            Command::ViewerSearchDialog => self.viewer.open_search_bar()?,
            Command::ViewerFindNext => self.viewer.find_next(true)?,
            Command::ViewerFindPrevious => self.viewer.find_next(false)?,
            Command::ViewerCopy => self.viewer.copy_selection()?,
            Command::ViewerSelectAll => {
                self.viewer.select_all();
                self.viewer.refresh()?;
            }
            Command::ViewerContextMenu => {
                let pt = self
                    .viewer
                    .hwnd()
                    .ClientToScreen(w::POINT::default())
                    .unwrap_or_default();
                self.show_text_menu(pt)?;
            }
            Command::Edit => self.edit(!self.active_right.get())?,
            Command::OpenSettings => self.open_settings()?,
            _ => {}
        }
        Ok(())
    }

    /// ビューア表示中の画像/動画キー操作。
    pub(crate) fn media_key(&self, vk: u16, ctrl: bool, shift: bool) -> w::AnyResult<()> {
        let chord = KeyChord::new(vk, ctrl, shift, false);
        let resolved = self.media_keymap.borrow().resolve_call(&chord);
        if let Some(call) = resolved {
            self.exec_media(&call)?;
        }
        Ok(())
    }

    /// 画像・動画ビューアのコマンドを実行する（キーバインドの共通入口）。
    pub(crate) fn exec_media(&self, call: &Call) -> w::AnyResult<()> {
        let cmd = match call {
            Call::Builtin { command, .. } => *command,
            Call::Script { source } => {
                self.script_send(crate::script_host::EngineCmd::Eval(source.clone()));
                return Ok(());
            }
        };
        match cmd {
            Command::ViewerClose => self.close_viewer()?,
            Command::MediaTogglePlay => self.media.toggle_play()?,
            Command::ImagePrevious => self.media.navigate(-1)?,
            Command::ImageNext => self.media.navigate(1)?,
            Command::ImageZoomIn => self.media.zoom(true)?,
            Command::ImageZoomOut => self.media.zoom(false)?,
            Command::ImageFitWindow => self.media.fit_to_window()?,
            Command::ImageActualSize => self.media.actual_size()?,
            Command::ImageFitWidth => self.media.fit_width()?,
            Command::ImageFitHeight => self.media.fit_height()?,
            Command::ImageFitLarge => self.media.fit_look_large()?,
            Command::ImageRotateRight => self.media.rotate()?,
            Command::ImageRotateLeft => self.media.rotate_left()?,
            Command::ImageFlipHorizontal => self.media.flip_horizontal()?,
            Command::ImageFlipVertical => self.media.flip_vertical()?,
            Command::ImageCopy => self.media.copy_to_clipboard()?,
            _ => {}
        }
        Ok(())
    }

    /// 画像ビューアの右クリックメニューを表示し、選んだ操作を実行する（画面座標 `pt`）。
    /// メニュー構成は暫定（朝レビュー対象）。
    pub(crate) fn show_media_menu(&self, pt: w::POINT) -> w::AnyResult<()> {
        const COPY: u16 = 1;
        const ZOOM_IN: u16 = 2;
        const ZOOM_OUT: u16 = 3;
        const FIT: u16 = 4;
        const ACTUAL: u16 = 5;
        const ROT_R: u16 = 6;
        const ROT_L: u16 = 7;
        const FLIP_H: u16 = 8;
        const FLIP_V: u16 = 9;
        const PREV: u16 = 10;
        const NEXT: u16 = 11;
        const CLOSE: u16 = 12;
        let items: &[(u16, &str)] = &[
            (COPY, "コピー(&C)"),
            (0, ""),
            (ZOOM_IN, "ズームイン(&I)"),
            (ZOOM_OUT, "ズームアウト(&O)"),
            (FIT, "全体表示(&F)"),
            (ACTUAL, "原寸(&1)"),
            (0, ""),
            (ROT_R, "右回転(&R)"),
            (ROT_L, "左回転(&L)"),
            (FLIP_H, "左右反転(&V)"),
            (FLIP_V, "上下反転(&H)"),
            (0, ""),
            (PREV, "前へ(&P)"),
            (NEXT, "次へ(&N)"),
            (0, ""),
            (CLOSE, "閉じる(&X)"),
        ];
        let Some(id) = self.popup_menu(items, pt, self.media.hwnd())? else {
            return Ok(());
        };
        match id {
            COPY => self.media.copy_to_clipboard()?,
            ZOOM_IN => self.media.zoom(true)?,
            ZOOM_OUT => self.media.zoom(false)?,
            FIT => self.media.fit_to_window()?,
            ACTUAL => self.media.actual_size()?,
            ROT_R => self.media.rotate()?,
            ROT_L => self.media.rotate_left()?,
            FLIP_H => self.media.flip_horizontal()?,
            FLIP_V => self.media.flip_vertical()?,
            PREV => self.media.navigate(-1)?,
            NEXT => self.media.navigate(1)?,
            CLOSE => self.close_viewer()?,
            _ => {}
        }
        Ok(())
    }

    /// テキストビューアの右クリックメニューを表示し、選んだ操作を実行する（画面座標 `pt`）。
    /// メニュー構成は暫定（朝レビュー対象）。
    pub(crate) fn show_text_menu(&self, pt: w::POINT) -> w::AnyResult<()> {
        const COPY: u16 = 1;
        const SELECT_ALL: u16 = 2;
        const SEARCH: u16 = 3;
        const FIND_NEXT: u16 = 4;
        const ENCODING: u16 = 5;
        const MODE: u16 = 6;
        const CLOSE: u16 = 7;
        let items: &[(u16, &str)] = &[
            (COPY, "コピー(&C)"),
            (SELECT_ALL, "すべて選択(&A)"),
            (0, ""),
            (SEARCH, "検索(&F)..."),
            (FIND_NEXT, "次を検索(&N)"),
            (0, ""),
            (ENCODING, "文字コード切替(&E)"),
            (MODE, "テキスト／バイナリ切替(&B)"),
            (0, ""),
            (CLOSE, "閉じる(&X)"),
        ];
        let Some(id) = self.popup_menu(items, pt, self.viewer.hwnd())? else {
            return Ok(());
        };
        match id {
            COPY => self.viewer.copy_selection()?,
            SELECT_ALL => {
                self.viewer.select_all();
                self.viewer.refresh()?;
            }
            SEARCH => self.viewer.open_search_bar()?,
            FIND_NEXT => self.viewer.find_next(true)?,
            ENCODING => self.viewer.cycle_encoding(true)?,
            MODE => self.viewer.toggle_mode()?,
            CLOSE => self.close_viewer()?,
            _ => {}
        }
        Ok(())
    }

    /// ポップアップメニューを表示し、選択された項目 ID を返す（キャンセルは None）。
    /// `items` は `(id, ラベル)`。id=0 はセパレータ。
    pub(crate) fn popup_menu(
        &self,
        items: &[(u16, &str)],
        pt: w::POINT,
        owner: &w::HWND,
    ) -> w::AnyResult<Option<u16>> {
        let menu = w::HMENU::CreatePopupMenu()?;
        for (id, label) in items {
            if *id == 0 {
                menu.AppendMenu(co::MF::SEPARATOR, w::IdMenu::None, w::BmpPtrStr::None)?;
            } else {
                menu.AppendMenu(
                    co::MF::STRING,
                    w::IdMenu::Id(*id),
                    w::BmpPtrStr::from_str(label),
                )?;
            }
        }
        // フォアグラウンド化しておかないと、メニュー外クリックで閉じない不具合がある。
        let _ = owner.SetForegroundWindow();
        let chosen = menu.TrackPopupMenu(
            co::TPM::RETURNCMD | co::TPM::LEFTALIGN | co::TPM::TOPALIGN,
            pt,
            owner,
        )?;
        Ok(chosen.map(|id| id as u16))
    }

    /// 走行中の書庫プリフェッチスレッドがあれば停止フラグを立てる（次の窓パスで終了する）。
    pub(crate) fn cancel_media_prefetch(&self) {
        if let Some(c) = self.media_prefetch.borrow_mut().take() {
            c.store(true, Ordering::Relaxed);
        }
    }

    /// 書庫内メディアの先読み展開ループ（BG スレッド・§7.6）。`cur` の前後の近傍を
    /// 共有キャッシュへ温める。`extract_entry_to_temp` は既展開なら存在チェックで安価に
    /// 返る（再展開しない）ので、毎パスで近傍を舐め直しても重くならない。`cancel`/
    /// `shutdown` が立つか、カレントが動いたら速やかに切り上げて再センタリングする。
    pub(crate) fn media_prefetch_loop(
        archive: &Path,
        entries: &[(String, String)],
        cur: &AtomicUsize,
        cancel: &AtomicBool,
        shutdown: &AtomicBool,
    ) {
        // 前方優先で温める窓（漫画の順送りを想定）。総量はこの窓に限られる＝暴走しない。
        const AHEAD: usize = 6;
        const BEHIND: usize = 2;
        let n = entries.len();
        loop {
            if cancel.load(Ordering::Relaxed) || shutdown.load(Ordering::Relaxed) {
                return;
            }
            let center = cur.load(Ordering::Relaxed);
            let mut targets: Vec<usize> = (1..=AHEAD).map(|k| center + k).collect();
            for k in 1..=BEHIND {
                if center >= k {
                    targets.push(center - k);
                }
            }
            for idx in targets {
                if cancel.load(Ordering::Relaxed) || shutdown.load(Ordering::Relaxed) {
                    return;
                }
                // カレントが動いたら今の窓は捨てて再センタリングする。
                if cur.load(Ordering::Relaxed) != center {
                    break;
                }
                if idx >= n {
                    continue;
                }
                let (inner, _name) = &entries[idx];
                // BG は静的呼び＝プロンプト不可。暗号化エントリは展開せず（FG が同期展開で扱う）。
                let _ = Self::extract_entry_to_temp(archive, inner, None);
            }
            // カレント変化を待つ短い休止（FG は同期展開で割り込むので latency は問題にならない）。
            std::thread::sleep(Duration::from_millis(120));
        }
    }

    /// 暗号化メディアを開く前にパスワードを確保する（キャッシュ→無ければプロンプト→保存）。
    /// 平文なら `None`。書庫メディアの resolver が展開時に用いる。
    pub(crate) fn ensure_media_password(&self, archive: &Path, target_inner: Option<&str>) -> Option<Vec<u8>> {
        let enc = target_inner
            .map(|t| self.entry_is_encrypted(archive, t))
            .unwrap_or(false);
        if !enc {
            return None;
        }
        if let Some(pw) = self.cached_password(archive) {
            return Some(pw);
        }
        let pw = self.prompt_password(archive)?.into_bytes();
        self.archive_passwords
            .borrow_mut()
            .insert(archive.to_path_buf(), pw.clone());
        Some(pw)
    }

    /// 書庫のパスワードを入力ダイアログで尋ねる（伏せ字）。
    pub(crate) fn prompt_password(&self, archive: &Path) -> Option<String> {
        let name = archive
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        dialog::input_box(
            &self.wnd,
            "パスワード",
            &format!("{} のパスワードを入力して下さい。", name),
            "",
            dialog::InputMode::Password,
        )
    }

    /// 書庫にキャッシュ済みのパスワードがあれば返す（メディア展開で再利用する）。
    pub(crate) fn cached_password(&self, archive: &Path) -> Option<Vec<u8>> {
        self.archive_passwords.borrow().get(archive).cloned()
    }

    /// 書庫内エントリ `inner` が暗号化されているか（list の is_encrypted を見る）。
    pub(crate) fn entry_is_encrypted(&self, archive: &Path, inner: &str) -> bool {
        open_archive(archive)
            .ok()
            .and_then(|b| b.list().ok())
            .and_then(|es| es.into_iter().find(|e| e.path == inner))
            .map(|e| e.is_encrypted)
            .unwrap_or(false)
    }
}
