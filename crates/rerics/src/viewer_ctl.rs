use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use winsafe::{self as w, co, prelude::*};
use rerics_core::{
    Call, Command, HtmlTheme, KeyChord, Location, MediaKind, RenderKind, ViewForm, ViewForms,
    ViewLayout, ViewMode, data_dir, decode_text, markdown_document, open_archive,
};
use crate::media_view::{NavResolver, PageRerender};
use crate::rendered_view::{FileReader, LinkTarget, Page, PageBody};
use crate::file_list::FileListView;
use crate::{ActiveView, MainWindow, dialog, hash64, join_inner_path, viewer};

/// ビューアで表示中のファイルと、その表示形式の組。
#[derive(Clone)]
pub(crate) struct ViewSession {
    /// 表示中のファイルが並ぶペイン。
    pub(crate) is_left: bool,
    /// 表示中のファイル名（一覧の項目名）。
    pub(crate) name: String,
    /// そのファイルが持つ表示形式。
    pub(crate) forms: ViewForms,
}

impl MainWindow {
    pub(crate) fn view(&self, is_left: bool) -> &FileListView {
        if is_left { self.left.list() } else { self.right.list() }
    }

    /// カーソル下の項目が実在するディレクトリ。検索・比較の結果一覧では項目の出自
    /// （`source`）が実際の場所なのでそれを、通常はペインの現在地を返す。ビューアや
    /// ファイル操作が「名前」を実パスへ解決する起点に使う。
    pub(crate) fn cursor_dir(&self, is_left: bool) -> Location {
        let state = self.view(is_left).state();
        let s = state.borrow();
        s.items
            .get(s.cursor)
            .and_then(|it| it.source.clone())
            .unwrap_or_else(|| self.pane(is_left).borrow().loc().clone())
    }

    /// カーソル下のファイルを内蔵ビューアで開く。原作 `View` 相当。引数なし＝親へ戻る/
    /// ディレクトリ・書庫へ潜る/それ以外はファイルが持つ一番上の表示形式で開く。`type` 指定時は
    /// ディレクトリでは何もせず、ファイルをその表示形式（`"text"`＝テキスト・`"bin"`＝バイナリ・
    /// 他は一番上の形式）で開く。持っていない形式を指定されたら一番上の形式で開く。
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
                return self.enter_dir(is_left, &name);
            }
            if self.enter_archive(is_left, &name)? {
                return Ok(());
            }
            return self.view_file(is_left);
        }
        // type 指定時：ディレクトリ/親は何もしない（原作 View）。
        if is_parent || is_dir {
            return Ok(());
        }
        let preferred = match vtype {
            Some(t) if t.eq_ignore_ascii_case("text") => Some(ViewForm::Text),
            Some(t) if t.eq_ignore_ascii_case("bin") => Some(ViewForm::Binary),
            Some(t) if t.eq_ignore_ascii_case("rendered") => Some(ViewForm::Rendered),
            _ => None,
        };
        self.open_view(is_left, preferred, false)
    }

    pub(crate) fn view_file(&self, is_left: bool) -> w::AnyResult<()> {
        self.open_view(is_left, None, false)
    }

    /// カーソル下のファイルを `preferred`（持っていなければ一番上の形式）で開く。`at_end` なら
    /// 末尾（スクロール表示は最下部・PDF は最終ページ）から表示する。
    pub(crate) fn open_view(
        &self,
        is_left: bool,
        preferred: Option<ViewForm>,
        at_end: bool,
    ) -> w::AnyResult<()> {
        let Some(name) = self.cursor_file_name(is_left) else {
            return Ok(());
        };
        let content = if ViewForms::needs_head(&name) {
            match self.read_for_view(is_left, &name) {
                Some(v) => Some(v),
                None => return Ok(()),
            }
        } else {
            None
        };
        let forms = ViewForms::detect(&name, content.as_ref().map(|(b, _)| b.as_slice()));
        let form = forms.resolve(preferred);
        *self.view_session.borrow_mut() =
            Some(ViewSession { is_left, name: name.clone(), forms });
        match (form, forms.render) {
            (ViewForm::Rendered, Some(RenderKind::Media(kind))) => {
                self.view_media(is_left, kind, &name)
            }
            (ViewForm::Rendered, Some(RenderKind::Pdf)) => self.view_pdf(is_left, &name, at_end),
            (ViewForm::Rendered, Some(kind @ (RenderKind::Markdown | RenderKind::Html))) => {
                self.view_rendered(is_left, &name, kind, at_end)
            }
            (ViewForm::Binary, _) => {
                self.show_text(is_left, &name, content, Some(ViewMode::Binary), at_end)
            }
            _ => self.show_text(is_left, &name, content, Some(ViewMode::Text), at_end),
        }
    }

    /// Markdown / HTML をレンダリング表示で開く。相対パスの資源は、開いたファイルと同じ
    /// 場所（実FS のディレクトリ・書庫内のディレクトリ）を基準に読む。WebView2 が使えなければ
    /// テキストで開く。
    fn view_rendered(
        &self,
        is_left: bool,
        name: &str,
        kind: RenderKind,
        at_end: bool,
    ) -> w::AnyResult<()> {
        if !self.rendered.available() {
            return self.show_text(is_left, name, None, Some(ViewMode::Text), at_end);
        }
        let body = match kind {
            RenderKind::Markdown => {
                let Some((bytes, _)) = self.read_for_view(is_left, name) else {
                    return Ok(());
                };
                let colors = self.config.borrow().active_colors();
                let bg = colors.viewer_background;
                let theme = HtmlTheme {
                    background: bg,
                    text: colors.viewer_text,
                    dark: (bg.r as u32 + bg.g as u32 + bg.b as u32) < 384,
                };
                PageBody::Document(markdown_document(&decode_text(&bytes), name, &theme))
            }
            _ => PageBody::File,
        };
        let files = self.rendered_file_reader(is_left);
        self.rendered.open(Page { name: name.to_string(), body, files });
        if at_end {
            self.rendered.scroll_to_end_when_loaded();
        }
        self.show_viewer(ActiveView::Rendered)
    }

    /// レンダリング表示の文書から相対パスで資源を読む関数。基準は開いた時点のカーソル下の
    /// 場所（検索・比較の結果一覧では項目の出自）。
    fn rendered_file_reader(&self, is_left: bool) -> FileReader {
        const MAX_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
        match self.cursor_dir(is_left) {
            Location::Real(dir) => Rc::new(move |rel: &str| {
                let path = dir.join(rel.replace('/', "\\"));
                path.is_file().then(|| std::fs::read(&path).ok()).flatten()
            }),
            Location::Archive { archive, inner } => {
                let this = self.clone();
                Rc::new(move |rel: &str| {
                    let entry = join_inner_path(&inner, rel);
                    this.read_archive_entry(&archive, &entry, MAX_RESOURCE_BYTES)
                        .ok()
                        .map(|(bytes, _)| bytes)
                })
            }
        }
    }

    /// レンダリング表示の文書内リンクをたどる。外部の URL とローカルのファイルは既定の
    /// 関連付けで開く（書庫内のファイルは開けない旨をログへ出す）。
    pub(crate) fn follow_rendered_link(&self, target: LinkTarget) {
        let target = match target {
            LinkTarget::External(url) => url,
            LinkTarget::Local(rel) => {
                let Some(session) = self.view_session.borrow().clone() else {
                    return;
                };
                match self.cursor_dir(session.is_left) {
                    Location::Real(dir) => dir.join(rel.replace('/', "\\")).display().to_string(),
                    Location::Archive { .. } => {
                        self.log.error(&format!("書庫内のリンク先は開けません: {rel}"));
                        return;
                    }
                }
            }
        };
        if crate::shell::launch_suppressed() {
            self.log.info(&crate::shell::launch_suppressed_line(&target));
            return;
        }
        if let Err(e) = crate::shell::open_associated(self.wnd.hwnd(), &target, None) {
            self.log.error(&format!("リンク先を開けません: {target}: {e}"));
        }
    }

    /// WebView2 を作れなかった。理由をログへ出し、表示中の文書をテキストで開き直す。
    pub(crate) fn rendered_failed(&self, reason: &str) {
        self.log.error(reason);
        let Some(session) = self.view_session.borrow().clone() else {
            return;
        };
        if matches!(self.active_view.get(), ActiveView::Rendered) {
            let _ = self.open_view(session.is_left, Some(ViewForm::Text), false);
        }
    }

    /// レンダリング表示が受け持つコマンドを実行する。
    fn exec_rendered(&self, cmd: Command) {
        match cmd {
            Command::ViewerScrollUp => self.rendered.scroll_lines(-1),
            Command::ViewerScrollDown => self.rendered.scroll_lines(1),
            Command::ViewerScrollTop => self.rendered.scroll_to_edge(false),
            Command::ViewerScrollBottom => self.rendered.scroll_to_edge(true),
            Command::ViewerSelectAll => self.rendered.select_all(),
            Command::ViewerCopy => self.rendered.copy_selection(),
            _ => {}
        }
    }

    /// カーソル下がファイルならその名前（ディレクトリ・親は `None`）。
    fn cursor_file_name(&self, is_left: bool) -> Option<String> {
        let state = self.view(is_left).state();
        let s = state.borrow();
        s.items
            .get(s.cursor)
            .filter(|it| !it.is_parent && !it.is_dir)
            .map(|it| it.name.clone())
    }

    /// ビューアに載せる分だけファイルを読む。読めなければログへ出して `None`。
    fn read_for_view(&self, is_left: bool, name: &str) -> Option<(Vec<u8>, bool)> {
        match self.read_pane_file(is_left, name, viewer::MAX_VIEW_BYTES) {
            Ok(v) => Some(v),
            Err(e) => {
                self.log.error(&format!("ビューアで開けません: {}: {}", name, e));
                None
            }
        }
    }

    /// テキスト/バイナリビューアで開く（モードは内容から自動判定）。レンダリング表示に
    /// 失敗したファイルの退避先。
    pub(crate) fn view_text(&self, is_left: bool, name: &str) -> w::AnyResult<()> {
        self.show_text(is_left, name, None, None, false)
    }

    /// テキスト/バイナリビューアで開く（実FS/書庫内とも bytes 直送）。`content` が `None` なら
    /// ここで読む。`mode` が `None` なら内容から自動判定する。
    fn show_text(
        &self,
        is_left: bool,
        name: &str,
        content: Option<(Vec<u8>, bool)>,
        mode: Option<ViewMode>,
        at_end: bool,
    ) -> w::AnyResult<()> {
        let Some((bytes, truncated)) = content.or_else(|| self.read_for_view(is_left, name)) else {
            return Ok(());
        };
        self.viewer.open(name, bytes, truncated);
        if let Some(mode) = mode {
            self.viewer.set_mode(mode)?;
        }
        if at_end {
            self.viewer.scroll_end()?;
        }
        self.show_viewer(ActiveView::Text)
    }

    /// 画像/動画ビューアで開く。実FS は同ディレクトリの閲覧可能メディアを前後送りに、
    /// 書庫内はカーソル下の1ファイルを一時展開して開く（書庫内の前後送りは後段で対応）。
    pub(crate) fn view_media(&self, is_left: bool, _kind: MediaKind, name: &str) -> w::AnyResult<()> {
        // 別メディアを開くので、前回の書庫プリフェッチがあれば止める。
        self.cancel_media_prefetch();
        // 検索・比較の結果一覧は項目ごとに出自ディレクトリが異なるため、前後送りはせず
        // カーソル下の1ファイルだけを開く（出自から実パスを解決し、書庫内なら一時展開）。
        if self.view(is_left).state().borrow().find_result {
            return self.view_media_single(is_left, name);
        }
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
                    return self.show_media_or_text(is_left, name);
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
        self.show_media_or_text(is_left, name)
    }

    /// メディアが表示フレームを得られていれば前面へ、デコードできなければ（コーデック非対応・
    /// 壊れ・テキストの拡張子衝突など）テキスト/バイナリビューアへ退避する。
    fn show_media_or_text(&self, is_left: bool, name: &str) -> w::AnyResult<()> {
        if self.media.current_loaded() {
            return self.show_viewer(ActiveView::Media);
        }
        self.cancel_media_prefetch();
        self.view_text(is_left, name)
    }

    /// 結果一覧のカーソル下メディアを単一表示で開く（前後送りなし）。実FS は出自ディレクトリ
    /// 直下の実パスを、書庫内は一時展開した実パスを、単一要素としてメディアビューアへ渡す。
    fn view_media_single(&self, is_left: bool, name: &str) -> w::AnyResult<()> {
        match self.cursor_dir(is_left) {
            Location::Real(dir) => {
                self.media.open(vec![dir.join(name)], 0);
            }
            Location::Archive { archive, inner } => {
                self.register_archive_temp(&archive);
                let inner_file = join_inner_path(&inner, name);
                let password = self.ensure_media_password(&archive, Some(&inner_file));
                match Self::extract_entry_to_temp(&archive, &inner_file, password.as_deref()) {
                    Ok(p) => self.media.open(vec![p], 0),
                    Err(e) => {
                        self.log
                            .error(&format!("書庫内メディアを展開できません: {}: {}", name, e));
                        return Ok(());
                    }
                }
            }
        }
        self.show_media_or_text(is_left, name)
    }

    /// PDF をインライン表示する。各ページを PDFium で PNG へラスタライズし、画像ビューアの
    /// 前後送りにページ単位で載せる（同階層のファイルではなく PDF のページを巡回対象にする）。
    /// resolver がページ index から PNG を遅延生成・キャッシュする。PDFium 未ロードや PDF を
    /// 開けないときはテキスト/バイナリビューアへ退避する。`at_end` なら最終ページから表示する。
    pub(crate) fn view_pdf(&self, is_left: bool, name: &str, at_end: bool) -> w::AnyResult<()> {
        self.cancel_media_prefetch();
        let Some(pdf_path) = self.resolve_pdf_path(is_left, name) else {
            return self.view_text(is_left, name);
        };
        let pages = match crate::pdf::page_count(&pdf_path) {
            Ok(n) if n > 0 => n,
            Ok(_) => return self.view_text(is_left, name),
            Err(e) => {
                self.log.error(&format!("PDF を開けません: {}: {}", name, e));
                return self.view_text(is_left, name);
            }
        };
        let root = Self::pdf_temp_root(&pdf_path);
        let _ = std::fs::create_dir_all(&root);
        let start = if at_end { pages - 1 } else { 0 };
        // 先読みスレッドへ現在ページを伝える共有カレントと、停止フラグ。
        let cur = Arc::new(AtomicUsize::new(start));
        let cancel = Arc::new(AtomicBool::new(false));
        *self.media_prefetch.borrow_mut() = Some(cancel.clone());
        // 既定幅のページ PNG（前後送りの本体）。確定済みファイルを介して先読みスレッドと協調する
        // ので、隣接ページが先読み済みなら焼き直さず即座に返る。
        let resolver: NavResolver = {
            let pdf = pdf_path.clone();
            let root = root.clone();
            let log = self.log.clone();
            let cur = cur.clone();
            Rc::new(move |i: usize| {
                cur.store(i, Ordering::Relaxed);
                let dest = root.join(format!("page-{i}.png"));
                match crate::pdf::render_page_atomic(&pdf, i, crate::pdf::DEFAULT_RENDER_WIDTH, &dest) {
                    Ok(()) => Some(dest),
                    Err(e) => {
                        log.error(&format!("PDF のページを描画できません: {} ページ目: {}", i + 1, e));
                        None
                    }
                }
            })
        };
        // ズーム追従：表示画素が既定幅を超えたら、その幅で焼き直した高解像 PNG を返す。幅ごとに
        // 別ファイルへ焼き、既にあれば再利用する（ズームの往復で焼き直しを繰り返さない）。
        let rerender: PageRerender = {
            let pdf = pdf_path.clone();
            let root = root.clone();
            let log = self.log.clone();
            Rc::new(move |i: usize, width: u32| {
                let dest = root.join(format!("page-{i}@{width}.png"));
                if dest.is_file() {
                    return Some(dest);
                }
                match crate::pdf::render_page(&pdf, i, width as i32, &dest) {
                    Ok(()) => Some(dest),
                    Err(e) => {
                        log.error(&format!(
                            "PDF のページを高解像で描画できません: {} ページ目: {}",
                            i + 1,
                            e
                        ));
                        None
                    }
                }
            })
        };
        self.media
            .open_nav_captioned(pages, start, resolver, Some(name.to_string()));
        self.media.set_rerender(rerender);
        // 近傍ページを別スレッドで先読み PNG 化し、前後送りを即応にする（書庫メディアと同方式）。
        {
            let pdf = pdf_path.clone();
            let root = root.clone();
            let shutdown = self.shutdown.clone();
            std::thread::spawn(move || {
                Self::pdf_prefetch_loop(&pdf, &root, pages, &cur, &cancel, &shutdown);
            });
        }
        self.show_media_or_text(is_left, name)
    }

    /// PDF ページの先読みループ（BG スレッド）。`cur` の前後の近傍ページを既定幅で焼いて確定
    /// ファイルを温める。`render_page_atomic` は既にあれば安価に返るので毎パス舐め直しても
    /// 重くならない。`cancel`/`shutdown` が立つかカレントが動いたら速やかに切り上げて再センタ
    /// リングする（PDFium 呼び出しは内部ロックで直列化＝1ページ焼くごとに FG へ譲れる）。
    pub(crate) fn pdf_prefetch_loop(
        pdf: &Path,
        root: &Path,
        pages: usize,
        cur: &AtomicUsize,
        cancel: &AtomicBool,
        shutdown: &AtomicBool,
    ) {
        // 前方優先で温める窓（順送りを想定）。総量はこの窓に限られる＝暴走しない。
        const AHEAD: usize = 4;
        const BEHIND: usize = 1;
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
                if idx >= pages {
                    continue;
                }
                let dest = root.join(format!("page-{idx}.png"));
                let _ =
                    crate::pdf::render_page_atomic(pdf, idx, crate::pdf::DEFAULT_RENDER_WIDTH, &dest);
            }
            std::thread::sleep(Duration::from_millis(120));
        }
    }

    /// カーソル下 PDF の表示に使えるローカルパスを返す。実FS はそのパス、書庫内は一時展開した
    /// パス。結果一覧は項目の出自から解決する。開けない/展開失敗は `None`。
    fn resolve_pdf_path(&self, is_left: bool, name: &str) -> Option<PathBuf> {
        let loc = if self.view(is_left).state().borrow().find_result {
            self.cursor_dir(is_left)
        } else {
            self.pane(is_left).borrow().loc().clone()
        };
        match loc {
            Location::Real(dir) => Some(dir.join(name)),
            Location::Archive { archive, inner } => {
                self.register_archive_temp(&archive);
                let inner_file = join_inner_path(&inner, name);
                let password = self.ensure_media_password(&archive, Some(&inner_file));
                match Self::extract_entry_to_temp(&archive, &inner_file, password.as_deref()) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        self.log
                            .error(&format!("書庫内 PDF を展開できません: {}: {}", name, e));
                        None
                    }
                }
            }
        }
    }

    /// PDF から焼いたページ PNG の一時置き場。**プロセスごとに分離**して、別インスタンスの
    /// 掃除が稼働中インスタンスの生成物を壊さないようにする（書庫 temp と同じ方針）。
    fn pdf_temp_dir() -> PathBuf {
        data_dir()
            .join("cache")
            .join("pdf")
            .join(std::process::id().to_string())
    }

    /// 自プロセスの PDF ページ temp のみを削除する（起動時の残骸掃除＋終了時の後始末）。
    pub(crate) fn clear_pdf_temp() {
        let _ = std::fs::remove_dir_all(Self::pdf_temp_dir());
    }

    /// PDF 1つ分のページ temp ルート（`cache/pdf/<pid>/<key>/`）。key はパス＋mtime のハッシュ
    /// ＝外部更新で別 key になり、焼き直しが古い PNG に当たらない。
    fn pdf_temp_root(pdf: &Path) -> PathBuf {
        let stamp = std::fs::metadata(pdf)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let key = format!("{:016x}", hash64(&format!("{}\u{0}{}", pdf.display(), stamp)));
        Self::pdf_temp_dir().join(key)
    }

    /// 指定ビューアを最前面に出し、他を隠してキー入力を奪う。
    pub(crate) fn show_viewer(&self, which: ActiveView) -> w::AnyResult<()> {
        self.active_view.set(which);
        // 切り替えた先で使わない部品は、再生・先読み・文書を残さない。
        if which != ActiveView::Media {
            self.cancel_media_prefetch();
            self.media.release_media();
            self.media.hwnd().ShowWindow(co::SW::HIDE);
        }
        if which != ActiveView::Rendered {
            self.rendered.release();
            self.rendered.hwnd().ShowWindow(co::SW::HIDE);
        }
        if which != ActiveView::Text {
            self.viewer.hwnd().ShowWindow(co::SW::HIDE);
        }
        match which {
            ActiveView::Text => {
                self.viewer.hwnd().ShowWindow(co::SW::SHOW);
                self.viewer.hwnd().BringWindowToTop()?;
                self.viewer.refresh()?;
            }
            ActiveView::Media => {
                self.media.hwnd().ShowWindow(co::SW::SHOW);
                self.media.hwnd().BringWindowToTop()?;
                self.media.refresh()?;
            }
            ActiveView::Rendered => {
                self.rendered.hwnd().ShowWindow(co::SW::SHOW);
                self.rendered.hwnd().BringWindowToTop()?;
            }
            ActiveView::None => {}
        }
        // ビューアが前面に出たので共有検索バーを畳む（検索状態自体は保持される）。
        let _ = self.sync_search_bar();
        self.key_sink.hwnd().SetFocus();
        Ok(())
    }

    /// ビューアを閉じてファイラ表示へ戻す。
    pub(crate) fn close_viewer(&self) -> w::AnyResult<()> {
        self.cancel_media_prefetch();
        self.media.release_media();
        self.rendered.release();
        self.active_view.set(ActiveView::None);
        *self.view_session.borrow_mut() = None;
        self.viewer.hwnd().ShowWindow(co::SW::HIDE);
        self.media.hwnd().ShowWindow(co::SW::HIDE);
        self.rendered.hwnd().ShowWindow(co::SW::HIDE);
        // ファイラへ戻ったので、そのペインの検索がアクティブなら共有検索バーを復活させる。
        let _ = self.sync_search_bar();
        self.key_sink.hwnd().SetFocus();
        // ビューアを隠した跡地を、一覧やパスバー等の子ウィンドウまで含めて即時に描き直す
        // （非表示にしただけでは子へ再描画が伝わらず表示が乱れることがある）。
        if let Ok(rc) = self.wnd.hwnd().GetClientRect() {
            let _ = self.wnd.hwnd().RedrawWindow(
                rc,
                &w::HRGN::NULL,
                co::RDW::INVALIDATE | co::RDW::ERASE | co::RDW::ALLCHILDREN | co::RDW::UPDATENOW,
            );
        }
        Ok(())
    }

    /// 表示中のファイルの表示形式（ビューアを閉じていれば `None`）。
    pub(crate) fn current_form(&self) -> Option<ViewForm> {
        match self.active_view.get() {
            ActiveView::None => None,
            ActiveView::Media | ActiveView::Rendered => Some(ViewForm::Rendered),
            ActiveView::Text => Some(match self.viewer.mode() {
                ViewMode::Text => ViewForm::Text,
                ViewMode::Binary => ViewForm::Binary,
            }),
        }
    }

    /// 表示中の並べ方（キー定義の表を選ぶ）。
    fn current_layout(&self) -> ViewLayout {
        let forms = self.view_session.borrow().as_ref().map(|s| s.forms);
        match (self.current_form(), forms) {
            (Some(form), Some(forms)) => forms.layout(form),
            (Some(ViewForm::Rendered), None) => ViewLayout::Paged,
            _ => ViewLayout::Scroll,
        }
    }

    /// ビューアのキーを解決する。表示中の並べ方の表を先に引き、無ければ共通の表を引く。
    pub(crate) fn resolve_viewer_chord(&self, chord: &KeyChord) -> Option<Call> {
        let table = match self.current_layout() {
            ViewLayout::Scroll => &self.viewer_scroll_keymap,
            ViewLayout::Paged => &self.viewer_paged_keymap,
        };
        let resolved = table.borrow().resolve_call(chord);
        resolved.or_else(|| self.viewer_keymap.borrow().resolve_call(chord))
    }

    /// ビューア表示中のキー操作。
    pub(crate) fn viewer_key(&self, vk: u16, ctrl: bool, shift: bool) -> w::AnyResult<()> {
        let chord = KeyChord::new(vk, ctrl, shift, false);
        if let Some(call) = self.resolve_viewer_chord(&chord) {
            self.exec_viewer(&call)?;
        }
        Ok(())
    }

    /// ビューアのコマンドを実行する（キーバインド・メニューの共通入口）。表示中の部品が
    /// 受け持たないコマンドは何もしない。
    pub(crate) fn exec_viewer(&self, call: &Call) -> w::AnyResult<()> {
        let cmd = match call {
            Call::Builtin { command, .. } => *command,
            Call::Script { source } => {
                self.script_send(crate::script_host::EngineCmdKind::Eval(source.clone()));
                return Ok(());
            }
        };
        match cmd {
            Command::ViewerClose => self.close_viewer()?,
            Command::ViewerCycleForm => self.cycle_form()?,
            Command::ViewerNext => {
                self.step_file(1, false)?;
            }
            Command::ViewerPrevious => {
                self.step_file(-1, false)?;
            }
            Command::ViewerPageDown => self.viewer_page(true, false)?,
            Command::ViewerPageUp => self.viewer_page(false, false)?,
            Command::ViewerPageDownOrNext => self.viewer_page(true, true)?,
            Command::ViewerPageUpOrPrevious => self.viewer_page(false, true)?,
            Command::Edit => self.edit(!self.active_right.get())?,
            Command::OpenSettings => self.open_settings()?,
            _ => match self.active_view.get() {
                ActiveView::Text => self.exec_text_viewer(cmd)?,
                ActiveView::Media => self.exec_media(cmd)?,
                ActiveView::Rendered => self.exec_rendered(cmd),
                ActiveView::None => {}
            },
        }
        Ok(())
    }

    /// テキスト/バイナリビューアが受け持つコマンドを実行する。
    fn exec_text_viewer(&self, cmd: Command) -> w::AnyResult<()> {
        match cmd {
            Command::ViewerScrollUp => self.viewer.scroll_by(-1)?,
            Command::ViewerScrollDown => self.viewer.scroll_by(1)?,
            Command::ViewerScrollTop => self.viewer.scroll_home()?,
            Command::ViewerScrollBottom => self.viewer.scroll_end()?,
            Command::ViewerChangeEncoding => self.viewer.cycle_encoding(true)?,
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
            _ => {}
        }
        Ok(())
    }

    /// 画像・動画・PDF ビューアが受け持つコマンドを実行する。
    fn exec_media(&self, cmd: Command) -> w::AnyResult<()> {
        match cmd {
            Command::MediaTogglePlay => self.media.toggle_play()?,
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
            Command::ImagePanUp => self.media.pan_step(0, -1)?,
            Command::ImagePanDown => self.media.pan_step(0, 1)?,
            Command::ImagePanLeft => self.media.pan_step(-1, 0)?,
            Command::ImagePanRight => self.media.pan_step(1, 0)?,
            Command::ImageCopy => self.media.copy_to_clipboard()?,
            _ => {}
        }
        Ok(())
    }

    /// 表示中のファイルを、持っている表示形式の次の形式で表示し直す。テキストとバイナリの
    /// 間はビューアのモード切替で済ませる。
    fn cycle_form(&self) -> w::AnyResult<()> {
        let session = self.view_session.borrow().clone();
        let (Some(session), Some(cur)) = (session, self.current_form()) else {
            return Ok(());
        };
        let next = session.forms.next(cur);
        match (cur, next) {
            _ if next == cur => Ok(()),
            (ViewForm::Text, ViewForm::Binary) => self.viewer.set_mode(ViewMode::Binary),
            (ViewForm::Binary, ViewForm::Text) => self.viewer.set_mode(ViewMode::Text),
            _ => self.open_view(session.is_left, Some(next), false),
        }
    }

    /// ページを送る。`down` が真なら先へ。スクロール表示は 1 ページぶんスクロール、PDF は
    /// 1 ページ、画像・動画は 1 ファイル送る。`overflow` が真なら、端のページでさらに送ると
    /// 隣のファイルへ移る（前のファイルへは末尾から入る）。
    pub(crate) fn viewer_page(&self, down: bool, overflow: bool) -> w::AnyResult<()> {
        let delta = if down { 1 } else { -1 };
        let at_edge = match self.active_view.get() {
            ActiveView::None => return Ok(()),
            ActiveView::Text => {
                let at_edge = if down { self.viewer.at_end() } else { self.viewer.at_top() };
                if !(overflow && at_edge) {
                    return self.viewer.scroll_page(down);
                }
                true
            }
            ActiveView::Media if self.showing_pdf() => {
                if self.media.step(delta)? || !overflow {
                    return Ok(());
                }
                true
            }
            ActiveView::Media => false,
            // 端かどうかは文書側でしか分からないので、判定とスクロールを任せて端なら送る。
            ActiveView::Rendered => {
                let this = self.clone();
                self.rendered.page(down, overflow, move || {
                    let _ = this.step_file(delta, !down);
                });
                return Ok(());
            }
        };
        self.step_file(delta, at_edge && !down)?;
        Ok(())
    }

    /// PDF のページを表示しているか。
    fn showing_pdf(&self) -> bool {
        matches!(self.active_view.get(), ActiveView::Media)
            && self
                .view_session
                .borrow()
                .as_ref()
                .is_some_and(|s| s.forms.render == Some(RenderKind::Pdf))
    }

    /// 一覧で `delta` 先のファイルを表示する（ディレクトリ・親は飛ばす）。一覧の端では止まり、
    /// 動けたかを返す。ペインのカーソルも移動先へ動かす。`at_end` なら移動先を末尾から表示する。
    /// 検索・比較の結果一覧では項目ごとに出自が異なるため送らない。
    pub(crate) fn step_file(&self, delta: isize, at_end: bool) -> w::AnyResult<bool> {
        let Some(session) = self.view_session.borrow().clone() else {
            return Ok(false);
        };
        let is_left = session.is_left;
        if self.view(is_left).state().borrow().find_result {
            return Ok(false);
        }
        let files = self.pane_files(is_left);
        let Some(index) = files.iter().position(|(name, _)| *name == session.name) else {
            return Ok(false);
        };
        let target = index as isize + delta;
        if target < 0 || target as usize >= files.len() {
            return Ok(false);
        }
        let target = target as usize;
        let (name, ext) = files[target].clone();
        self.mutate_state(is_left, |s, pr| {
            s.set_cursor_position(&name, pr);
        })?;
        // 画像・動画どうしの移動は、開いた時点の巡回セット（一覧の画像・動画を同じ順に並べた
        // もの）の中で送る。書庫内の先読みをそのまま活かせる。
        let in_media_set = matches!(self.active_view.get(), ActiveView::Media)
            && matches!(session.forms.render, Some(RenderKind::Media(_)))
            && MediaKind::from_extension(&ext).is_some();
        if !in_media_set {
            self.open_view(is_left, None, at_end)?;
            return Ok(true);
        }
        let media_index = files[..target]
            .iter()
            .filter(|(_, e)| MediaKind::from_extension(e).is_some())
            .count();
        let forms = ViewForms::detect(&name, None);
        *self.view_session.borrow_mut() = Some(ViewSession { is_left, name: name.clone(), forms });
        self.media.go_to(media_index)?;
        if !self.media.current_loaded() {
            self.cancel_media_prefetch();
            self.view_text(is_left, &name)?;
        }
        Ok(true)
    }

    /// ペインの一覧に並ぶファイル（ディレクトリ・親を除く）の名前と拡張子を、一覧の順に返す。
    fn pane_files(&self, is_left: bool) -> Vec<(String, String)> {
        let state = self.view(is_left).state();
        let s = state.borrow();
        s.items
            .iter()
            .filter(|it| !it.is_dir && !it.is_parent)
            .map(|it| (it.name.clone(), it.extension.clone()))
            .collect()
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
            PREV => self.viewer_page(false, false)?,
            NEXT => self.viewer_page(true, false)?,
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
            (MODE, "表示形式を切替(&T)"),
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
            MODE => self.cycle_form()?,
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

    /// 暗号化メディアを開く前にパスワードを確保する。平文なら `None`。書庫メディアの
    /// resolver が展開時に用いる。確保は [`ensure_archive_password`]
    /// (crate::MainWindow::ensure_archive_password) に委譲し、検証はそのエントリの先頭を
    /// 試し読みして行う（誤入力を無検証でキャッシュしない）。
    pub(crate) fn ensure_media_password(&self, archive: &Path, target_inner: Option<&str>) -> Option<Vec<u8>> {
        let inner = target_inner?;
        if !self.entry_is_encrypted(archive, inner) {
            return None;
        }
        let backend = open_archive(archive).ok()?;
        self.ensure_archive_password(archive, |pw| {
            backend.read_capped_with_password(inner, 1, Some(pw)).is_ok()
        })
        .ok()
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
