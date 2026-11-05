//! Markdown / HTML のレンダリング表示パネル（WebView2）。
//!
//! 他のビューアと同じく、別窓を作らずメイン領域へ重ねる 1 枚の `WindowControl` に WebView2 の
//! コントローラを載せる。下端には状態行（ファイル名、リンクにマウスを乗せている間はその行き先）を
//! 自前で描く。WebView2 の環境は初めて表示するときに非同期で作る。資源は
//! `WebResourceRequested` で一手に配り（表示中の文書・同じディレクトリのファイル・同梱の
//! CSS とスクリプト）、それ以外のホストへの要求は拒否する。ページ遷移と新しい窓はすべて止め、
//! リンク先は呼び出し側へ渡す。キーボードのフォーカスは呼び出し側へ戻し、WebView2 には
//! マウス操作（選択・リンク・ホイール）だけを渡す。右クリックのメニューも呼び出し側が出す。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use rerics_core::{ASSET_HOST, Config, DOC_HOST, KeyChord, Rgb, html_csp, markdown_csp};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    AcceleratorKeyPressedEventHandler, ContextMenuRequestedEventHandler,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    ExecuteScriptCompletedHandler, FocusChangedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, StatusBarTextChangedEventHandler,
    WebResourceRequestedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR};
use winsafe::{self as w, co, gui, prelude::*};

use crate::chrome;

/// WebView2 がフォーカスを得たあと、マウスのボタンが離れるのを見張るタイマー。
const REFOCUS_TIMER_ID: usize = 0x5246;
/// 見張りの間隔（ミリ秒）。
const REFOCUS_POLL_MS: u32 = 50;

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
    /// 文書のある場所の表示名（ディレクトリのパス・書庫内の場所）。リンクの行き先を示すのに使う。
    pub(crate) location: String,
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
type MenuHandler = Box<dyn Fn(w::POINT)>;
type FocusHandler = Box<dyn Fn()>;

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
    font_family: String,
    font_size: i32,
    /// マウスを乗せているリンクの URL（乗せていなければ空）。
    hover: RefCell<String>,
    on_link: RefCell<Option<LinkHandler>>,
    on_key: RefCell<Option<KeyHandler>>,
    on_failed: RefCell<Option<FailHandler>>,
    on_menu: RefCell<Option<MenuHandler>>,
    on_focus: RefCell<Option<FocusHandler>>,
    /// 表示中の文書の読み込みが済んだか（debug 観測用）。
    loaded: Cell<bool>,
    /// WebView2 がキーボードのフォーカスを得た回数（debug 観測用）。
    focus_events: Cell<u32>,
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
        cfg: &Config,
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
            background: cfg.active_colors().viewer_background,
            font_family: cfg.font.family.clone(),
            font_size: cfg.font.size,
            hover: RefCell::new(String::new()),
            on_link: RefCell::new(None),
            on_key: RefCell::new(None),
            on_failed: RefCell::new(None),
            on_menu: RefCell::new(None),
            on_focus: RefCell::new(None),
            loaded: Cell::new(false),
            focus_events: Cell::new(0),
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

    /// 文書を右クリックしたときのコールバック（画面座標）を登録する。WebView2 の既定のメニューの
    /// 代わりに呼ぶ。
    pub fn on_menu(&self, cb: impl Fn(w::POINT) + 'static) {
        *self.inner.on_menu.borrow_mut() = Some(Box::new(cb));
    }

    /// WebView2 がキーボードのフォーカスを得たときのコールバックを登録する。キー入力は他の
    /// ビューアと同じ経路で受けるので、呼び出し側はフォーカスを取り戻す。
    pub fn on_focus(&self, cb: impl Fn() + 'static) {
        *self.inner.on_focus.borrow_mut() = Some(Box::new(cb));
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
        self.set_hover(String::new());
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
        self.inner.hover.borrow_mut().clear();
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

    /// WebView2 がキーボードのフォーカスを得た回数（debug 観測用）。
    #[cfg(feature = "debug-server")]
    pub fn focus_events(&self) -> u32 {
        self.inner.focus_events.get()
    }

    /// WebView2 へキーボードのフォーカスを移す（クリックでフォーカスが移るのと同じ状況を
    /// debug-server から作る）。WebView2 が使えなければ偽。
    #[cfg(feature = "debug-server")]
    pub fn focus_webview(&self) -> bool {
        let Engine::Ready(ready) = &*self.inner.engine.borrow() else {
            return false;
        };
        unsafe {
            ready
                .controller
                .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                .is_ok()
        }
    }

    /// マウスを乗せているリンクの URL を設定して状態行を描き直す（空ならファイル名に戻す）。
    /// WebView2 の状態表示の変化を受けて呼ぶ。debug-server からも駆動できる。
    pub fn set_hover(&self, uri: String) {
        *self.inner.hover.borrow_mut() = uri;
        let _ = self.hwnd().InvalidateRect(None, true);
    }

    /// 状態行の文言。リンクにマウスを乗せている間はその行き先（文書のディレクトリ内なら実際の
    /// パス）、それ以外はファイル名と操作の案内。
    pub fn status_text(&self) -> String {
        let page = self.inner.page.borrow();
        let hover = self.inner.hover.borrow();
        if !hover.is_empty() {
            let location = page.as_ref().map(|p| p.location.as_str()).unwrap_or("");
            return format!("リンク先: {}", link_display(&hover, location));
        }
        match page.as_ref() {
            Some(p) => format!("{}    (B/Tab:表示形式 Esc:閉じる)", p.name),
            None => String::new(),
        }
    }

    fn status_height(&self) -> i32 {
        gui::dpi_y(self.inner.font_size + 8)
    }

    fn on_paint(&self) -> w::AnyResult<()> {
        let hdc = self.hwnd().BeginPaint()?;
        let rc = self.hwnd().GetClientRect()?;
        self.paint_to(&hdc, rc.right - rc.left, rc.bottom - rc.top)
    }

    /// ターゲットビットマップ選択済みの任意 DC へ、パネル自身が描く部分（地と状態行）を描く
    /// （debug-server のスナップショット用。本文は WebView2 が描くので写らない）。
    #[cfg(feature = "debug-server")]
    pub(crate) fn render_to(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        self.paint_to(dc, cw, ch)
    }

    fn paint_to(&self, hdc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        let sh = self.status_height();
        let sy = (ch - sh).max(0);
        // 本文は WebView2 が覆う。作成前・読み込み前に見える地だけ塗る。
        let bg = self.inner.background;
        let body = w::HBRUSH::CreateSolidBrush(w::COLORREF::from_rgb(bg.r, bg.g, bg.b))?;
        hdc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: sy }, &body)?;
        let face = w::HBRUSH::CreateSolidBrush(chrome::face())?;
        hdc.FillRect(w::RECT { left: 0, top: sy, right: cw, bottom: ch }, &face)?;
        chrome::hline(hdc, 0, cw, sy, chrome::highlight())?;
        let font = w::HFONT::CreateFont(
            w::SIZE { cx: 0, cy: -gui::dpi_y((self.inner.font_size - 2).max(6)) },
            0,
            0,
            co::FW::NORMAL,
            false,
            false,
            false,
            co::CHARSET::DEFAULT,
            co::OUT_PRECIS::DEFAULT,
            co::CLIP::DEFAULT_PRECIS,
            co::QUALITY::CLEARTYPE,
            co::PITCH::DEFAULT,
            &self.inner.font_family,
        )?;
        let _sel = hdc.SelectObject(&*font)?;
        hdc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        hdc.SetTextColor(chrome::text())?;
        let pad = gui::dpi_x(self.inner.font_size).max(1);
        let rect = w::RECT { left: pad, top: sy, right: cw - pad, bottom: ch };
        hdc.DrawText(
            &self.status_text(),
            rect,
            co::DT::SINGLELINE | co::DT::VCENTER | co::DT::NOPREFIX | co::DT::PATH_ELLIPSIS,
        )?;
        Ok(())
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
        self.wnd.on().wm_paint(move || me.on_paint());
        let me = self.clone();
        self.wnd.on().wm_size(move |_| {
            me.fit_bounds();
            let _ = me.hwnd().InvalidateRect(None, true);
            Ok(())
        });
        let me = self.clone();
        self.wnd.on().wm_timer(REFOCUS_TIMER_ID, move || {
            me.refocus_when_released();
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

    /// WebView2 をパネルのクライアント領域（下端の状態行を除く）に合わせる。
    fn fit_bounds(&self) {
        let Engine::Ready(ready) = &*self.inner.engine.borrow() else {
            return;
        };
        let Ok(rc) = self.hwnd().GetClientRect() else {
            return;
        };
        let bottom = (rc.bottom - rc.top - self.status_height()).max(0);
        let bounds = RECT { left: 0, top: 0, right: rc.right - rc.left, bottom };
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
            // 既定のメニューは有効のままにしないと右クリックの通知が来ない。通知を受けて
            // 既定のメニューは出さず、呼び出し側のメニューに差し替える。
            settings.SetAreDefaultContextMenusEnabled(true)?;
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
            // リンクにマウスを乗せたときの行き先（WebView2 自身の状態表示は無効にして自前で出す）。
            if let Ok(webview12) = webview.cast::<ICoreWebView2_12>() {
                let me = self.clone();
                let source = webview12.clone();
                webview12.add_StatusBarTextChanged(
                    &StatusBarTextChangedEventHandler::create(Box::new(move |_, _| {
                        let mut text = PWSTR::null();
                        source.StatusBarText(&mut text)?;
                        me.set_hover(take_pwstr(text));
                        Ok(())
                    })),
                    &mut token,
                )?;
            }
            if let Ok(webview11) = webview.cast::<ICoreWebView2_11>() {
                let me = self.clone();
                webview11.add_ContextMenuRequested(
                    &ContextMenuRequestedEventHandler::create(Box::new(move |_, args| {
                        if let Some(args) = args {
                            me.on_context_menu(&args)?;
                        }
                        Ok(())
                    })),
                    &mut token,
                )?;
            }
            // キーボードのフォーカスは呼び出し側へ戻す（WebView2 にはマウス操作だけを渡す）。
            // クリックやドラッグの途中で取り戻すと WebView2 が取り直すので、ボタンが離れるのを
            // 待ってから戻す。
            let me = self.clone();
            controller.add_GotFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    me.inner.focus_events.set(me.inner.focus_events.get().wrapping_add(1));
                    let _ = me.hwnd().SetTimer(REFOCUS_TIMER_ID, REFOCUS_POLL_MS, None);
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

    /// マウスのボタンがすべて離れていれば、見張りをやめてフォーカスを呼び出し側へ戻す。
    fn refocus_when_released(&self) {
        let pressed = [co::VK::LBUTTON, co::VK::RBUTTON, co::VK::MBUTTON]
            .into_iter()
            .any(w::GetAsyncKeyState);
        if pressed {
            return;
        }
        let _ = self.hwnd().KillTimer(REFOCUS_TIMER_ID);
        if let Some(cb) = &*self.inner.on_focus.borrow() {
            cb();
        }
    }

    /// 右クリックで既定のメニューを出さず、呼び出し側のメニューを右クリックした位置に出す。
    fn on_context_menu(
        &self,
        args: &ICoreWebView2ContextMenuRequestedEventArgs,
    ) -> windows::core::Result<()> {
        let mut at = POINT::default();
        unsafe {
            args.SetHandled(true)?;
            args.Location(&mut at)?;
        }
        // メニューを出している間に見張りがフォーカスを動かさないよう止める（閉じたあとは
        // 呼び出し側がフォーカスを戻す）。
        let _ = self.hwnd().KillTimer(REFOCUS_TIMER_ID);
        // 位置は表示倍率を掛ける前の座標（DIP）で届くので、WebView2 の描画倍率を掛けて画素へ直す。
        // WebView2 はパネルの左上に置いているので、文書内の位置がそのままパネル内の位置になる。
        let scale = self.rasterization_scale();
        let client = w::POINT {
            x: (at.x as f64 * scale).round() as i32,
            y: (at.y as f64 * scale).round() as i32,
        };
        let pt = self.hwnd().ClientToScreen(client).unwrap_or_default();
        if let Some(cb) = &*self.inner.on_menu.borrow() {
            cb(pt);
        }
        Ok(())
    }

    /// WebView2 の描画倍率（DIP から画素への倍率）。取れなければ窓の DPI から求める。
    fn rasterization_scale(&self) -> f64 {
        if let Engine::Ready(ready) = &*self.inner.engine.borrow()
            && let Ok(c3) = ready.controller.cast::<ICoreWebView2Controller3>()
        {
            let mut scale = 0.0f64;
            if unsafe { c3.RasterizationScale(&mut scale) }.is_ok() && scale > 0.0 {
                return scale;
            }
        }
        gui::dpi_x(100) as f64 / 100.0
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

/// リンクの行き先の表示名。文書のディレクトリ基準のリンクは、文書のある場所 `location` を前に
/// 付けた実際のパスに直す。外部の URL はそのまま。
fn link_display(uri: &str, location: &str) -> String {
    match route(uri) {
        Route::Doc(path) if !location.is_empty() => {
            format!("{}\\{}", location.trim_end_matches('\\'), path.replace('/', "\\"))
        }
        Route::Doc(path) => path.replace('/', "\\"),
        _ => uri.to_string(),
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
    fn link_display_shows_real_location() {
        assert_eq!(
            link_display(&format!("https://{DOC_HOST}/sub/other%20doc.md#x"), "C:\\work\\"),
            "C:\\work\\sub\\other doc.md"
        );
        assert_eq!(link_display("https://example.com/a?b=1", "C:\\work"), "https://example.com/a?b=1");
    }

    #[test]
    fn assets_are_bundled() {
        assert!(asset("mermaid.min.js").is_some_and(|(b, _)| b.len() > 100_000));
        assert!(asset("markdown.css").is_some());
        assert!(asset("markdown.js").is_some());
        assert!(asset("other.js").is_none());
    }
}
