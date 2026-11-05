//! Markdown / HTML のレンダリング表示パネル（WebView2）。
//!
//! 他のビューアと同じく、別窓を作らずメイン領域へ重ねる 1 枚の `WindowControl` に WebView2 の
//! コントローラを載せる。WebView2 の環境は初めて表示するときに非同期で作る。資源は
//! `WebResourceRequested` で一手に配り（表示中の文書・同じディレクトリのファイル・同梱の
//! CSS とスクリプト）、それ以外のホストへの要求は拒否する。ページ遷移と新しい窓はすべて止め、
//! リンク先は呼び出し側へ渡す。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use rerics_core::{ASSET_HOST, DOC_HOST, KeyChord, Rgb, html_csp, markdown_csp};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, ExecuteScriptCompletedHandler,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler,
    WebResourceRequestedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR};
use winsafe::{self as w, co, gui, prelude::*};

const MERMAID_JS: &[u8] = include_bytes!("../assets/rendered/mermaid.min.js");
const MARKDOWN_CSS: &[u8] = include_bytes!("../assets/rendered/markdown.css");
const MARKDOWN_JS: &[u8] = include_bytes!("../assets/rendered/markdown.js");

/// 表示中の文書から同じディレクトリ基準の相対パスでファイルを読む関数（読めなければ `None`）。
pub(crate) type FileReader = Rc<dyn Fn(&str) -> Option<Vec<u8>>>;

/// 文書の中身。
pub(crate) enum PageBody {
    /// こちらで組んだ HTML 文書（Markdown を変換したもの）。
    Document(String),
    /// ファイルそのもの（HTML）。中身は [`Page::files`] で読む。
    File,
}

/// レンダリング表示する 1 文書。
pub(crate) struct Page {
    /// ファイル名（文書の URL の末尾。相対パスの基準になる）。
    pub(crate) name: String,
    pub(crate) body: PageBody,
    /// 同じディレクトリ基準でファイルを読む関数。
    pub(crate) files: FileReader,
}

/// 文書内のリンクの行き先。
pub(crate) enum LinkTarget {
    /// 外部の URL（http・https・mailto）。
    External(String),
    /// 文書のディレクトリ基準の相対パス。
    Local(String),
}

type LinkHandler = Box<dyn Fn(LinkTarget)>;
type KeyHandler = Box<dyn Fn(KeyChord) -> bool>;
type FailHandler = Box<dyn Fn(String)>;

/// WebView2 の準備状況。
#[derive(Clone)]
enum Engine {
    /// 作らない（画面を持たない起動）。
    Disabled,
    /// まだ作っていない。
    Idle,
    /// 作成中（完了したら保留中の文書を表示する）。
    Creating,
    Ready(Ready),
    /// 作れなかった。
    Failed(String),
}

#[derive(Clone)]
struct Ready {
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
}

struct Inner {
    engine: RefCell<Engine>,
    page: RefCell<Option<Rc<Page>>>,
    user_data_dir: PathBuf,
    background: Rgb,
    on_link: RefCell<Option<LinkHandler>>,
    on_key: RefCell<Option<KeyHandler>>,
    on_failed: RefCell<Option<FailHandler>>,
    /// 表示中の文書の読み込みが済んだか（debug 観測用）。
    loaded: Cell<bool>,
}

/// レンダリング表示パネル。
#[derive(Clone)]
pub struct RenderedView {
    wnd: gui::WindowControl,
    inner: Rc<Inner>,
}

impl RenderedView {
    /// `enabled` が偽なら WebView2 を作らない（文書の受け付けと状態だけを持つ）。
    pub fn new(
        parent: &(impl GuiParent + 'static),
        position: (i32, i32),
        size: (i32, i32),
        background: Rgb,
        enabled: bool,
    ) -> Self {
        let wnd = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                class_bg_brush: gui::Brush::None,
                position,
                size,
                style: co::WS::CHILD | co::WS::CLIPSIBLINGS | co::WS::CLIPCHILDREN,
                ..Default::default()
            },
        );
        let inner = Rc::new(Inner {
            engine: RefCell::new(if enabled { Engine::Idle } else { Engine::Disabled }),
            page: RefCell::new(None),
            user_data_dir: user_data_dir(),
            background,
            on_link: RefCell::new(None),
            on_key: RefCell::new(None),
            on_failed: RefCell::new(None),
            loaded: Cell::new(false),
        });
        let me = Self { wnd, inner };
        me.setup_events();
        me
    }

    pub fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    /// 文書内のリンクが選ばれたときのコールバックを登録する。
    pub fn on_link(&self, cb: impl Fn(LinkTarget) + 'static) {
        *self.inner.on_link.borrow_mut() = Some(Box::new(cb));
    }

    /// WebView2 にフォーカスがある間のキー押下を渡すコールバックを登録する。処理したら真を返す。
    pub fn on_key(&self, cb: impl Fn(KeyChord) -> bool + 'static) {
        *self.inner.on_key.borrow_mut() = Some(Box::new(cb));
    }

    /// WebView2 を作れなかったときのコールバックを登録する（理由を渡す）。
    pub fn on_failed(&self, cb: impl Fn(String) + 'static) {
        *self.inner.on_failed.borrow_mut() = Some(Box::new(cb));
    }

    /// 文書を受け付けられるか（WebView2 を作れなかったら偽）。画面を持たない起動では描画しない
    /// まま文書と状態だけを持つので真。
    pub fn available(&self) -> bool {
        !matches!(*self.inner.engine.borrow(), Engine::Failed(_))
    }

    /// 文書を表示する。WebView2 がまだ無ければ作り、できてから表示する。
    pub fn open(&self, page: Page) {
        *self.inner.page.borrow_mut() = Some(Rc::new(page));
        self.inner.loaded.set(false);
        let engine = self.inner.engine.borrow().clone();
        match engine {
            Engine::Idle => self.start_engine(),
            Engine::Ready(ready) => self.navigate(&ready),
            Engine::Failed(reason) => self.notify_failed(reason),
            Engine::Creating | Engine::Disabled => {}
        }
    }

    /// 表示中の文書を手放す（WebView2 は空のページへ移して残す）。
    pub fn release(&self) {
        *self.inner.page.borrow_mut() = None;
        self.inner.loaded.set(false);
        if let Engine::Ready(ready) = &*self.inner.engine.borrow() {
            unsafe {
                let _ = ready.webview.Navigate(&HSTRING::from("about:blank"));
            }
        }
    }

    /// 表示中の文書のファイル名（debug 観測用）。
    #[cfg(feature = "debug-server")]
    pub fn page_name(&self) -> Option<String> {
        self.inner.page.borrow().as_ref().map(|p| p.name.clone())
    }

    /// WebView2 の状態を表す語（debug 観測用）。
    #[cfg(feature = "debug-server")]
    pub fn engine_state(&self) -> &'static str {
        match &*self.inner.engine.borrow() {
            Engine::Disabled => "disabled",
            Engine::Idle => "idle",
            Engine::Creating => "creating",
            Engine::Ready(_) => "ready",
            Engine::Failed(_) => "failed",
        }
    }

    /// 表示中の文書の読み込みが済んだか（debug 観測用）。
    #[cfg(feature = "debug-server")]
    pub fn loaded(&self) -> bool {
        self.inner.loaded.get()
    }

    /// 文書でスクリプトを実行し、結果（JSON 文字列。失敗したら `null`）をコールバックへ渡す。
    /// WebView2 が使えなければコールバックは呼ばず偽を返す。
    pub fn eval(&self, script: &str, done: impl FnOnce(String) + 'static) -> bool {
        let Engine::Ready(ready) = self.inner.engine.borrow().clone() else {
            return false;
        };
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            done(if result.is_ok() { json } else { "null".to_string() });
            Ok(())
        }));
        unsafe { ready.webview.ExecuteScript(&HSTRING::from(script), &handler).is_ok() }
    }

    /// 表示中の見た目を PNG で撮ってコールバックへ渡す（失敗したら `None`）。WebView2 が
    /// 使えなければコールバックは呼ばず偽を返す。
    #[cfg(feature = "debug-server")]
    pub fn capture_png(&self, done: impl FnOnce(Option<Vec<u8>>) + 'static) -> bool {
        use windows::Win32::System::Com::{STATSTG, STREAM_SEEK_SET, STATFLAG_NONAME};
        let Engine::Ready(ready) = self.inner.engine.borrow().clone() else {
            return false;
        };
        let Some(stream) = (unsafe { SHCreateMemStream(None) }) else {
            return false;
        };
        let target = stream.clone();
        let handler = webview2_com::CapturePreviewCompletedHandler::create(Box::new(move |result| {
            let png = result.ok().and_then(|()| unsafe {
                let mut stat = STATSTG::default();
                target.Stat(&mut stat, STATFLAG_NONAME).ok()?;
                target.Seek(0, STREAM_SEEK_SET, None).ok()?;
                let mut buf = vec![0u8; stat.cbSize as usize];
                let mut read = 0u32;
                target
                    .Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut read))
                    .ok()
                    .ok()?;
                buf.truncate(read as usize);
                Some(buf)
            });
            done(png);
            Ok(())
        }));
        unsafe {
            ready
                .webview
                .CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &handler)
                .is_ok()
        }
    }

    /// 文書を `delta` 行ぶんスクロールする（正＝下）。
    pub fn scroll_lines(&self, delta: i32) {
        self.eval(&format!("window.scrollBy(0, {} * 40)", delta), |_| {});
    }

    /// 文書を先頭・末尾へスクロールする。
    pub fn scroll_to_edge(&self, end: bool) {
        let y = if end { "document.documentElement.scrollHeight" } else { "0" };
        self.eval(&format!("window.scrollTo(0, {y})"), |_| {});
    }

    /// 1 ページぶんスクロールする。`overflow` が真で既に端なら、スクロールせず `at_edge` を呼ぶ。
    /// WebView2 が使えない（画面を持たない起動など）ときは文書を端とみなす。
    pub fn page(&self, down: bool, overflow: bool, at_edge: impl FnOnce() + 'static) {
        if !matches!(*self.inner.engine.borrow(), Engine::Ready(_)) {
            if overflow {
                at_edge();
            }
            return;
        }
        let script = format!(
            "(() => {{ const e = document.scrollingElement || document.documentElement; \
             const edge = {down} ? e.scrollTop + window.innerHeight >= e.scrollHeight - 2 : e.scrollTop <= 0; \
             if (!({overflow} && edge)) window.scrollBy(0, ({down} ? 1 : -1) * window.innerHeight * 0.9); \
             return edge; }})()"
        );
        self.eval(&script, move |json| {
            if overflow && json.trim() == "true" {
                at_edge();
            }
        });
    }

    /// 末尾から表示し始める（前のファイルへ戻ったとき）。読み込み完了前なら完了後に効かせる。
    pub fn scroll_to_end_when_loaded(&self) {
        self.eval(
            "(() => { const go = () => window.scrollTo(0, document.documentElement.scrollHeight); \
             if (document.readyState === 'complete') go(); else window.addEventListener('load', go); })()",
            |_| {},
        );
    }

    /// 文書全体を選択する。
    pub fn select_all(&self) {
        self.eval("document.getSelection().selectAllChildren(document.body)", |_| {});
    }

    /// 選択範囲のテキストをクリップボードへ写す。
    pub fn copy_selection(&self) {
        let me = self.clone();
        self.eval("document.getSelection().toString()", move |json| {
            let text: String = serde_json::from_str(&json).unwrap_or_default();
            if !text.is_empty() {
                let _ = me.set_clipboard_text(&text);
            }
        });
    }

    fn set_clipboard_text(&self, text: &str) -> w::AnyResult<()> {
        let mut u16s: Vec<u16> = text.encode_utf16().collect();
        u16s.push(0);
        let bytes: Vec<u8> = u16s.iter().flat_map(|u| u.to_le_bytes()).collect();
        let clip = self.hwnd().OpenClipboard()?;
        clip.EmptyClipboard()?;
        clip.SetClipboardData(co::CF::UNICODETEXT, &bytes)?;
        Ok(())
    }

    fn setup_events(&self) {
        let me = self.clone();
        self.wnd.on().wm_size(move |_| {
            me.fit_bounds();
            Ok(())
        });
        let me = self.clone();
        self.wnd.on().wm_show_window(move |p| {
            if let Engine::Ready(ready) = &*me.inner.engine.borrow() {
                unsafe {
                    let _ = ready.controller.SetIsVisible(p.being_shown);
                }
            }
            Ok(())
        });
    }

    /// WebView2 をパネルのクライアント領域いっぱいに合わせる。
    fn fit_bounds(&self) {
        let Engine::Ready(ready) = &*self.inner.engine.borrow() else {
            return;
        };
        let Ok(rc) = self.hwnd().GetClientRect() else {
            return;
        };
        let bounds = RECT { left: 0, top: 0, right: rc.right - rc.left, bottom: rc.bottom - rc.top };
        unsafe {
            let _ = ready.controller.SetBounds(bounds);
        }
    }

    fn start_engine(&self) {
        *self.inner.engine.borrow_mut() = Engine::Creating;
        let _ = std::fs::create_dir_all(&self.inner.user_data_dir);
        let me = self.clone();
        let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
            move |result, env| {
                match (result, env) {
                    (Ok(()), Some(env)) => me.create_controller(env),
                    (result, _) => me.fail(format!("WebView2 の環境を作れません: {result:?}")),
                }
                Ok(())
            },
        ));
        let user_data = HSTRING::from(self.inner.user_data_dir.as_os_str());
        let started = unsafe {
            CreateCoreWebView2EnvironmentWithOptions(
                PCWSTR::null(),
                &user_data,
                None::<&ICoreWebView2EnvironmentOptions>,
                &handler,
            )
        };
        if let Err(e) = started {
            self.fail(format!("WebView2 を起動できません（ランタイムが無い可能性）: {e}"));
        }
    }

    fn create_controller(&self, env: ICoreWebView2Environment) {
        let me = self.clone();
        let env2 = env.clone();
        let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
            move |result, controller| {
                match (result, controller) {
                    (Ok(()), Some(controller)) => {
                        if let Err(e) = me.setup_webview(env2, controller) {
                            me.fail(format!("WebView2 を設定できません: {e}"));
                        }
                    }
                    (result, _) => me.fail(format!("WebView2 を作れません: {result:?}")),
                }
                Ok(())
            },
        ));
        let parent = HWND(self.hwnd().ptr());
        if let Err(e) = unsafe { env.CreateCoreWebView2Controller(parent, &handler) } {
            self.fail(format!("WebView2 を作れません: {e}"));
        }
    }

    fn setup_webview(
        &self,
        env: ICoreWebView2Environment,
        controller: ICoreWebView2Controller,
    ) -> windows::core::Result<()> {
        let webview = unsafe { controller.CoreWebView2()? };
        unsafe {
            let settings = webview.Settings()?;
            settings.SetAreDevToolsEnabled(false)?;
            settings.SetAreDefaultContextMenusEnabled(false)?;
            settings.SetAreDefaultScriptDialogsEnabled(false)?;
            settings.SetIsWebMessageEnabled(false)?;
            settings.SetAreHostObjectsAllowed(false)?;
            settings.SetIsStatusBarEnabled(false)?;
            if let Ok(s4) = settings.cast::<ICoreWebView2Settings4>() {
                s4.SetIsPasswordAutosaveEnabled(false)?;
                s4.SetIsGeneralAutofillEnabled(false)?;
            }
            if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() {
                let bg = self.inner.background;
                c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: bg.r, G: bg.g, B: bg.b })?;
            }
        }
        self.add_handlers(&env, &controller, &webview)?;
        let ready = Ready { controller, webview };
        *self.inner.engine.borrow_mut() = Engine::Ready(ready.clone());
        self.fit_bounds();
        unsafe {
            ready.controller.SetIsVisible(self.hwnd().IsWindowVisible())?;
        }
        if self.inner.page.borrow().is_some() {
            self.navigate(&ready);
        }
        Ok(())
    }

    fn add_handlers(
        &self,
        env: &ICoreWebView2Environment,
        controller: &ICoreWebView2Controller,
        webview: &ICoreWebView2,
    ) -> windows::core::Result<()> {
        let mut token = 0i64;
        unsafe {
            webview.AddWebResourceRequestedFilter(
                &HSTRING::from("*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            )?;
            let me = self.clone();
            let env = env.clone();
            webview.add_WebResourceRequested(
                &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        me.respond(&env, &args)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let me = self.clone();
            webview.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        me.on_navigation(&args)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let me = self.clone();
            webview.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        args.SetHandled(true)?;
                        let mut uri = PWSTR::null();
                        args.Uri(&mut uri)?;
                        me.follow_link(&take_pwstr(uri));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let me = self.clone();
            webview.add_NavigationCompleted(
                &webview2_com::NavigationCompletedEventHandler::create(Box::new(move |_, _| {
                    me.inner.loaded.set(me.inner.page.borrow().is_some());
                    Ok(())
                })),
                &mut token,
            )?;
            let me = self.clone();
            controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        me.on_accelerator(&args)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    /// 表示中の文書の URL。
    fn doc_url(&self) -> Option<String> {
        self.inner.page.borrow().as_ref().map(|p| doc_url(&p.name))
    }

    fn navigate(&self, ready: &Ready) {
        let Some(url) = self.doc_url() else {
            return;
        };
        unsafe {
            let _ = ready.webview.Navigate(&HSTRING::from(url));
        }
    }

    /// 要求された資源を返す。自前のホスト以外は拒否する。
    fn respond(
        &self,
        env: &ICoreWebView2Environment,
        args: &ICoreWebView2WebResourceRequestedEventArgs,
    ) -> windows::core::Result<()> {
        let uri = unsafe {
            let request = args.Request()?;
            let mut uri = PWSTR::null();
            request.Uri(&mut uri)?;
            take_pwstr(uri)
        };
        let reply = match route(&uri) {
            Route::Asset(name) => match asset(&name) {
                Some((bytes, ty)) => Reply::ok(bytes.to_vec(), format!("Content-Type: {ty}")),
                None => Reply::status(404),
            },
            Route::Doc(path) => self.doc_reply(&path),
            Route::Other => Reply::status(403),
        };
        unsafe {
            let stream = SHCreateMemStream(Some(&reply.body));
            let reason = if reply.status == 200 { "OK" } else { "Unavailable" };
            let response = env.CreateWebResourceResponse(
                stream.as_ref(),
                reply.status,
                &HSTRING::from(reason),
                &HSTRING::from(reply.headers),
            )?;
            args.SetResponse(&response)
        }
    }

    /// 文書ホストの資源。表示中の文書そのものなら文書を、それ以外は同じディレクトリ基準で読む。
    /// どの応答にも表示中の文書の種類に応じた CSP を付ける。
    fn doc_reply(&self, path: &str) -> Reply {
        let Some(page) = self.inner.page.borrow().clone() else {
            return Reply::status(404);
        };
        let csp = match page.body {
            PageBody::Document(_) => markdown_csp(),
            PageBody::File => html_csp(),
        };
        let (body, ty) = match &page.body {
            PageBody::Document(html) if path == page.name => {
                (Some(html.as_bytes().to_vec()), "text/html; charset=utf-8")
            }
            PageBody::File if path == page.name => ((page.files)(path), "text/html"),
            _ => ((page.files)(path), content_type(path)),
        };
        match body {
            Some(body) => Reply::ok(
                body,
                format!("Content-Type: {ty}\r\nContent-Security-Policy: {csp}"),
            ),
            None => Reply::status(404),
        }
    }

    /// 表示中の文書（とそのページ内の移動）以外への遷移は止め、行き先を呼び出し側へ渡す。
    fn on_navigation(
        &self,
        args: &ICoreWebView2NavigationStartingEventArgs,
    ) -> windows::core::Result<()> {
        let uri = unsafe {
            let mut uri = PWSTR::null();
            args.Uri(&mut uri)?;
            take_pwstr(uri)
        };
        let doc = self.doc_url();
        if doc.as_deref() == Some(strip_fragment(&uri)) || uri == "about:blank" {
            return Ok(());
        }
        unsafe {
            args.SetCancel(true)?;
        }
        self.follow_link(&uri);
        Ok(())
    }

    fn follow_link(&self, uri: &str) {
        let Some(target) = link_target(uri) else {
            return;
        };
        if let Some(cb) = &*self.inner.on_link.borrow() {
            cb(target);
        }
    }

    /// WebView2 にフォーカスがある間のキーを Rerics のキー定義へ回す。
    fn on_accelerator(
        &self,
        args: &ICoreWebView2AcceleratorKeyPressedEventArgs,
    ) -> windows::core::Result<()> {
        let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
        let mut vk = 0u32;
        unsafe {
            args.KeyEventKind(&mut kind)?;
            args.VirtualKey(&mut vk)?;
        }
        if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
            && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
        {
            return Ok(());
        }
        let chord = KeyChord::new(
            vk as u16,
            w::GetAsyncKeyState(co::VK::CONTROL),
            w::GetAsyncKeyState(co::VK::SHIFT),
            false,
        );
        let handled = self.inner.on_key.borrow().as_ref().is_some_and(|cb| cb(chord));
        if handled {
            unsafe {
                args.SetHandled(true)?;
            }
        }
        Ok(())
    }

    fn fail(&self, reason: String) {
        *self.inner.engine.borrow_mut() = Engine::Failed(reason.clone());
        if self.inner.page.borrow().is_some() {
            self.notify_failed(reason);
        }
    }

    fn notify_failed(&self, reason: String) {
        if let Some(cb) = &*self.inner.on_failed.borrow() {
            cb(reason);
        }
    }
}

/// 応答の中身。
struct Reply {
    status: i32,
    body: Vec<u8>,
    headers: String,
}

impl Reply {
    fn ok(body: Vec<u8>, headers: String) -> Self {
        Self { status: 200, body, headers }
    }

    fn status(status: i32) -> Self {
        Self { status, body: Vec::new(), headers: String::new() }
    }
}

/// 要求の振り分け先。
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// 同梱の資源（名前）。
    Asset(String),
    /// 文書のディレクトリ基準の相対パス。
    Doc(String),
    /// 自前のホスト以外。
    Other,
}

fn route(uri: &str) -> Route {
    let Some(rest) = uri.strip_prefix("https://") else {
        return Route::Other;
    };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = percent_decode(strip_query(path));
    if host.eq_ignore_ascii_case(ASSET_HOST) {
        Route::Asset(path)
    } else if host.eq_ignore_ascii_case(DOC_HOST) {
        Route::Doc(path)
    } else {
        Route::Other
    }
}

/// リンクの行き先。自前の文書ホストなら相対パス、外部の URL はそのまま、他は扱わない。
fn link_target(uri: &str) -> Option<LinkTarget> {
    match route(uri) {
        Route::Doc(path) if !path.is_empty() => Some(LinkTarget::Local(path)),
        Route::Doc(_) | Route::Asset(_) => None,
        Route::Other => {
            let lower = uri.to_ascii_lowercase();
            ["http://", "https://", "mailto:"]
                .iter()
                .any(|p| lower.starts_with(p))
                .then(|| LinkTarget::External(uri.to_string()))
        }
    }
}

fn doc_url(name: &str) -> String {
    format!("https://{DOC_HOST}/{}", percent_encode(name))
}

fn strip_fragment(uri: &str) -> &str {
    uri.split_once('#').map_or(uri, |(u, _)| u)
}

fn strip_query(path: &str) -> &str {
    let path = strip_fragment(path);
    path.split_once('?').map_or(path, |(p, _)| p)
}

/// パスの 1 区切りを URL 用に符号化する（英数字と `-._~` 以外を `%XX` に）。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn asset(name: &str) -> Option<(&'static [u8], &'static str)> {
    match name {
        "mermaid.min.js" => Some((MERMAID_JS, "text/javascript")),
        "markdown.js" => Some((MARKDOWN_JS, "text/javascript")),
        "markdown.css" => Some((MARKDOWN_CSS, "text/css")),
        _ => None,
    }
}

fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// WebView2 のユーザーデータの置き場所。データディレクトリを差し替えて起動しているときはその下、
/// 通常はローカルのアプリデータの下（持ち運び用の置き場所やネットワーク上を汚さない）。
fn user_data_dir() -> PathBuf {
    if std::env::var_os("RERICS_DATA_DIR").is_some() {
        return rerics_core::data_dir().join("WebView2");
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Rerics")
        .join("WebView2")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_own_hosts_and_blocks_others() {
        assert_eq!(route(&format!("https://{ASSET_HOST}/markdown.css")), Route::Asset("markdown.css".into()));
        assert_eq!(
            route(&format!("https://{DOC_HOST}/img/a%20b.png?x=1#frag")),
            Route::Doc("img/a b.png".into())
        );
        assert_eq!(route("https://example.com/x.png"), Route::Other);
        assert_eq!(route("http://doc.rerics.invalid/x"), Route::Other);
        assert_eq!(route("file:///C:/x"), Route::Other);
    }

    #[test]
    fn doc_url_roundtrips_names() {
        for name in ["README.md", "設計 メモ.md", "a#b?.md", "100%.md"] {
            assert_eq!(route(&doc_url(name)), Route::Doc(name.to_string()));
        }
    }

    #[test]
    fn link_targets() {
        assert!(matches!(link_target("https://example.com/"), Some(LinkTarget::External(_))));
        assert!(matches!(link_target("mailto:a@b"), Some(LinkTarget::External(_))));
        assert!(matches!(
            link_target(&format!("https://{DOC_HOST}/other.md")),
            Some(LinkTarget::Local(p)) if p == "other.md"
        ));
        assert!(link_target("javascript:alert(1)").is_none());
        assert!(link_target("file:///C:/Windows").is_none());
    }

    #[test]
    fn assets_are_bundled() {
        assert!(asset("mermaid.min.js").is_some_and(|(b, _)| b.len() > 100_000));
        assert!(asset("markdown.css").is_some());
        assert!(asset("markdown.js").is_some());
        assert!(asset("other.js").is_none());
    }
}
