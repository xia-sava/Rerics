//! 共通モーダルダイアログ基盤。原作 `RecordsLib.PluginMessage` / `MessageForm` 相当。
//!
//! [`message_box`]（`PluginMessage.Show`）と [`input_box`]（`PluginMessage.Input`）を提供する。
//! スタイル（[`MessageStyle`]）でアイコン・ボタン構成を切り替える。`MessageStyle` の整数値は
//! 原作の enum に一致させ、将来スクリプトからの `int` → enum 変換をそのまま通せるようにする。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;

use rerics_core::{NameCase, SortType, floor_to_local_midnight, format_local};
use winsafe::{self as w, co, gui, prelude::*};

mod message;
mod input;
mod conflict;
mod archive_add;
mod link;
mod compare;
mod compress;
mod find;
mod sort;
mod rename;
mod list;
mod about;
pub use message::message_box;
pub use input::{
    code_box, command_box, completion_members, input_box, input_box_full, input_box_select,
};
#[cfg(feature = "debug-server")]
pub use input::completion_probe;
pub use conflict::conflict_box;
pub use archive_add::archive_add_box;
pub use link::link_kind_box;
pub use compare::compare_options_box;
pub use compress::compress_box;
pub use find::find_file_box;
pub use sort::sort_box;
pub use rename::rename_box;
pub use list::list_box;
pub use about::about_box;

#[allow(non_snake_case)]
mod ffi {
    use core::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn DrawIconEx(
            hdc: *mut c_void,
            x: i32,
            y: i32,
            hicon: *mut c_void,
            cx: i32,
            cy: i32,
            step: u32,
            brush: *mut c_void,
            flags: u32,
        ) -> i32;
    }
}

const DI_NORMAL: u32 = 0x0003;

/// モーダル中のキー観測（原作 `Form.KeyPreview=true` 相当）。
///
/// winsafe のモーダル窓は子コントロールにフォーカスがあると生キーを受け取れない（Phase 0 実測）。
/// `WH_KEYBOARD` フックは設置できるが `PostMessage` 合成キーを拾わず headless 検証不可だった。
/// そこで**モーダルの全子コントロールを `SetWindowSubclass`（comctl32）でサブクラス化**して、
/// 子へ dispatch される `WM_KEYDOWN`/`WM_KEYUP` を横取りする。これは**実キーでも PostMessage
/// 合成キーでも発火する**ので headless で検証できる。Shift 連動グレーアウト等のカスタム挙動の土台。
/// 観測はスタックで持ち（モーダルはネストし得る）、最前面の観測のみを呼ぶ。
///
/// conflict_box（同名衝突）の Shift 連動で使用。実キー・PostMessage 合成キー双方で発火する。
pub mod keyhook {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::rc::Rc;
    use winsafe::{self as w, co};

    #[allow(non_snake_case)]
    mod ffi {
        use core::ffi::c_void;
        pub type SubclassProc =
            unsafe extern "system" fn(*mut c_void, u32, usize, isize, usize, usize) -> isize;
        #[link(name = "comctl32")]
        unsafe extern "system" {
            pub fn SetWindowSubclass(
                hwnd: *mut c_void,
                proc: SubclassProc,
                id: usize,
                refdata: usize,
            ) -> i32;
            pub fn DefSubclassProc(
                hwnd: *mut c_void,
                msg: u32,
                wparam: usize,
                lparam: isize,
            ) -> isize;
        }
    }

    const SUBCLASS_ID: usize = 0x5245_4b59; // "REKY"
    pub const WM_KEYDOWN: u32 = 0x0100;
    pub const WM_KEYUP: u32 = 0x0101;
    pub const WM_CHAR: u32 = 0x0102;

    /// `(msg, wparam)` を受け取る観測。`true` を返すとそのキーを消費して既定処理へ渡さない
    /// （補完候補のキー操作などで Edit へキーを渡したくないとき）。`WM_KEYDOWN`/`WM_KEYUP`/`WM_CHAR` を受ける。
    type Observer = Rc<dyn Fn(u32, usize) -> bool>;

    thread_local! {
        static OBSERVERS: RefCell<Vec<Observer>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "system" fn sub_proc(
        hwnd: *mut c_void,
        msg: u32,
        wparam: usize,
        lparam: isize,
        _id: usize,
        _ref: usize,
    ) -> isize {
        if msg == WM_KEYDOWN || msg == WM_KEYUP || msg == WM_CHAR {
            // 借用を保持したまま呼ぶと観測内のメッセージ処理で再入し得るので Rc を取り出してから呼ぶ。
            let top = OBSERVERS.with(|o| o.borrow().last().cloned());
            if let Some(f) = top
                && f(msg, wparam)
            {
                return 0; // 消費＝既定処理（DefSubclassProc）へ渡さない。
            }
        }
        unsafe { ffi::DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// 観測を積み、`parent` の全子コントロールをサブクラス化してキーを横取りする。
    /// 子コントロールは作成済みである必要があるので `wm_create` の末尾で呼ぶ。
    pub fn push(parent: &w::HWND, cb: impl Fn(u32, usize) -> bool + 'static) {
        OBSERVERS.with(|o| o.borrow_mut().push(Rc::new(cb)));
        if let Ok(mut cur) = parent.GetWindow(co::GW::CHILD) {
            loop {
                unsafe {
                    ffi::SetWindowSubclass(cur.ptr(), sub_proc, SUBCLASS_ID, 0);
                }
                match cur.GetWindow(co::GW::HWNDNEXT) {
                    Ok(n) => cur = n,
                    Err(_) => break,
                }
            }
        }
    }

    /// 最前面の観測を外す（サブクラスは窓破棄で自然消滅するので明示解除は不要）。
    pub fn pop() {
        OBSERVERS.with(|o| {
            o.borrow_mut().pop();
        });
    }
}

/// モーダル登録の遅延データ。`modal_window` の `wm_create` で `modal_registry` へ push される。
#[cfg(feature = "debug-server")]
enum Reg {
    Plain {
        kind: &'static str,
        title: String,
        prompt: String,
        has_input: bool,
        buttons: Vec<(String, u16)>,
    },
    List {
        kind: &'static str,
        title: String,
        items: Vec<String>,
        selected: usize,
        buttons: Vec<(String, u16)>,
    },
    ListView {
        kind: &'static str,
        title: String,
        buttons: Vec<(String, u16)>,
        hooks: crate::debug_server::modal_registry::ListViewHooks,
    },
}

/// モーダルの配線ハンドル。[`modal_window`] が返す。ダイアログは show 前に
/// [`ModalArm::plain`]/[`ModalArm::list`]/[`ModalArm::list_view`] で debug-server への登録
/// 情報を、[`ModalArm::on_create`] で生成時の固有処理（初期フォーカスの上書き・keyhook 等）を
/// 仕込む。いずれも省略可。登録を省いても `modal_window` が最小エントリを自動 push するので、
/// **どのモーダルも必ず debug-server から観測（撮影）できる**。
#[derive(Clone)]
pub struct ModalArm {
    #[cfg(feature = "debug-server")]
    reg: Rc<RefCell<Option<Reg>>>,
    #[allow(clippy::type_complexity)]
    on_create: Rc<RefCell<Option<Box<dyn Fn(&w::HWND) -> w::AnyResult<()>>>>>,
    #[allow(clippy::type_complexity)]
    on_destroy: Rc<RefCell<Vec<Box<dyn Fn()>>>>,
}

impl ModalArm {
    fn new() -> Self {
        Self {
            #[cfg(feature = "debug-server")]
            reg: Rc::new(RefCell::new(None)),
            on_create: Rc::new(RefCell::new(None)),
            on_destroy: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// 生成時の固有処理（初期フォーカスの上書き・keyhook など）を仕込む。親 `HWND` が渡る
    /// （子コントロール作成済み）。
    pub fn on_create(&self, f: impl Fn(&w::HWND) -> w::AnyResult<()> + 'static) {
        *self.on_create.borrow_mut() = Some(Box::new(f));
    }

    /// 閉鎖時の固有処理（サイズの記録など）を仕込む。登録順に全部走る。
    ///
    /// `WM_DESTROY` を直接ハンドルしてはいけない：winsafe は通常メッセージを
    /// 「最後に登録された 1 つだけ実行して打ち切る」ので、後から登録すると基盤の後始末
    /// （debug-server のモーダル登録解除）を消してしまう。追加処理は必ずここへ足す。
    pub fn on_destroy(&self, f: impl Fn() + 'static) {
        self.on_destroy.borrow_mut().push(Box::new(f));
    }

    /// 通常モーダルの登録情報を仕込む（`buttons` は (ラベル, ctrl_id)・OK=1/Cancel=2）。
    #[cfg(feature = "debug-server")]
    pub fn plain(
        &self,
        kind: &'static str,
        title: &str,
        prompt: &str,
        has_input: bool,
        buttons: Vec<(String, u16)>,
    ) {
        *self.reg.borrow_mut() = Some(Reg::Plain {
            kind,
            title: title.to_owned(),
            prompt: prompt.to_owned(),
            has_input,
            buttons,
        });
    }

    /// 単列リスト選択モーダルの登録情報を仕込む。
    #[cfg(feature = "debug-server")]
    pub fn list(
        &self,
        kind: &'static str,
        title: &str,
        items: Vec<String>,
        selected: usize,
        buttons: Vec<(String, u16)>,
    ) {
        *self.reg.borrow_mut() = Some(Reg::List {
            kind,
            title: title.to_owned(),
            items,
            selected,
            buttons,
        });
    }

    /// 多列 ListView モーダルの登録情報を仕込む。
    #[cfg(feature = "debug-server")]
    pub fn list_view(
        &self,
        kind: &'static str,
        title: &str,
        buttons: Vec<(String, u16)>,
        hooks: crate::debug_server::modal_registry::ListViewHooks,
    ) {
        *self.reg.borrow_mut() = Some(Reg::ListView {
            kind,
            title: title.to_owned(),
            buttons,
            hooks,
        });
    }
}

/// 標準モーダル窓を作る（タイトル＋クライアント幅高）。原作 `PluginForm` 相当の佇まい
/// （× 無し・最大化/最小化無し・親中央・`IsDialogMessage` 処理あり）を一元化する。
///
/// アプリのモーダルは生の `WindowModal::new` を各所で書かず、必ずこの関数（または
/// [`modal_window_sysmenu`]）を通す。返り値の [`ModalArm`] へ登録情報・生成時処理を仕込む
/// （省略可）。フォーカス・debug-server 登録・閉鎖時の後始末（pop）はこの関数が一手に
/// 引き受けるので、**登録忘れでモーダルが撮れなくなることがない**。
pub fn modal_window(title: &str, w: i32, h: i32) -> (gui::WindowModal, ModalArm) {
    modal_window_styled(title, w, h, co::WS::default())
}

thread_local! {
    /// モーダル表示中の入れ子深さ（UI スレッド専有）。表示行為そのもの
    /// （[`show_modal_guarded`]）が管理し、[`modal_active`] で読む。
    static MODAL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// モーダルダイアログが1つでも表示中か。モーダルの内部メッセージループは `WM_TIMER` を
/// 汲むため、タイマ駆動の取り込み（`pump_tasks`）はこれを見て、多重取り込みと
/// 「モーダルの下でペイン再読込などの完了処理が走る」再入を抑止する。
pub fn modal_active() -> bool {
    MODAL_DEPTH.with(|d| d.get()) > 0
}

/// モーダル表示の唯一の関門。`show_modal` を直接呼ばず必ずこれを通す（表示中フラグの
/// 管理を、呼び出し元の規約ではなく表示行為そのものに内包させる）。深さカウンタなので
/// 設定→キーエディタのような入れ子でも、内側の復帰が外側のガードを外さない。
/// 直呼びの混入は `show_modal_is_called_only_through_the_guarded_wrapper` が検出する。
pub fn show_modal_guarded(wnd: &gui::WindowModal, parent: &impl GuiParent) {
    struct DepthGuard;
    impl DepthGuard {
        fn arm() -> Self {
            MODAL_DEPTH.with(|d| d.set(d.get() + 1));
            DepthGuard
        }
    }
    impl Drop for DepthGuard {
        fn drop(&mut self) {
            MODAL_DEPTH.with(|d| d.set(d.get() - 1));
        }
    }
    let _guard = DepthGuard::arm();
    let _ = wnd.show_modal(parent);
}

/// [`modal_window`] にタイトルバーの × （システムメニュー）を足したもの。設定のように
/// 大きく、× で閉じられると自然なモーダルで使う。
pub fn modal_window_sysmenu(title: &str, w: i32, h: i32) -> (gui::WindowModal, ModalArm) {
    modal_window_styled(title, w, h, co::WS::SYSMENU)
}

/// サイズ変更枠（× も）付きで、**前回サイズを無言で記憶する**モーダル。一覧から選ぶ
/// セレクタのように中身を広げて見たいモーダルで使う。`key` 別に前回のクライアントサイズ
/// （論理px）を `dialog-sizes.toml` へ保存し、次回はそのサイズで開く。リサイズ時の再配置は
/// 各ダイアログが `wm_size` で行う。最小サイズは `min_w`/`min_h`（論理px）で抑え、保存値が
/// それ未満なら最小へ、画面サイズを超えていたら既定（`default_w`/`default_h`）へ戻す。
pub fn modal_window_resizable_keyed(
    title: &str,
    key: &'static str,
    default_w: i32,
    default_h: i32,
    min_w: i32,
    min_h: i32,
) -> (gui::WindowModal, ModalArm) {
    let (w0, h0) = resolve_dialog_size(key, (default_w, default_h), (min_w, min_h));
    let (wnd, arm) = modal_window_styled(title, w0, h0, co::WS::SYSMENU | co::WS::SIZEBOX);

    // ドラッグでの縮小下限。
    wnd.on().wm_get_min_max_info(move |p| {
        p.info.ptMinTrackSize = w::POINT { x: gui::dpi_x(min_w), y: gui::dpi_y(min_h) };
        Ok(())
    });
    // 閉じる時に現在のクライアントサイズ（論理px）を無言で記録する。winsafe は同じ
    // メッセージへ複数ハンドラを登録でき全部走るので、基盤の DESTROY（pop）と併存できる。
    let wsave = wnd.clone();
    arm.on_destroy(move || {
        if let Ok(rc) = wsave.hwnd().GetClientRect() {
            let mut store = rerics_core::DialogSizes::load();
            store.set(key, (to_logical(rc.right, true), to_logical(rc.bottom, false)));
            let _ = store.save();
        }
    });
    (wnd, arm)
}

/// 物理px を論理px へ戻す（`gui::dpi_*(1000)` が 1000 論理の物理px＝スケール）。
fn to_logical(phys: i32, horizontal: bool) -> i32 {
    let scale = (if horizontal { gui::dpi_x(1000) } else { gui::dpi_y(1000) }).max(1);
    (phys as i64 * 1000 / scale as i64) as i32
}

/// プライマリ画面サイズ（論理px）。保存値が画面を超えていないかの判定に使う。
fn screen_logical() -> (i32, i32) {
    let sw = w::GetSystemMetrics(co::SM::CXSCREEN);
    let sh = w::GetSystemMetrics(co::SM::CYSCREEN);
    (to_logical(sw, true), to_logical(sh, false))
}

/// `key` の前回サイズを検証して返す。未保存は既定。最小未満は最小へ引き上げ、画面を
/// 超えるサイズは既定へ戻す。
fn resolve_dialog_size(key: &str, def: (i32, i32), min: (i32, i32)) -> (i32, i32) {
    let Some((w, h)) = rerics_core::DialogSizes::load().get(key) else {
        return def;
    };
    let (w, h) = (w.max(min.0), h.max(min.1));
    let (sw, sh) = screen_logical();
    if w > sw || h > sh {
        return def;
    }
    (w, h)
}

/// 「一覧＋下端右寄せボタン」型モーダルのリサイズ追従。クライアント `cw`×`ch`（物理px）に対し
/// `list` を四周 `margin`（論理px）で広げ（下端はボタン行ぶん空ける）、`buttons` を下端へ
/// 右寄せに並べる（`buttons[0]` が最も右）。各要素は `(hwnd, 幅〔論理px〕)`・高さは `btn_h`。
/// list_box・ドライブ選択・登録ディレクトリで共用する。
pub fn relayout_list_dialog(
    list: &w::HWND,
    margin: i32,
    btn_h: i32,
    buttons: &[(&w::HWND, i32)],
    cw: i32,
    ch: i32,
) {
    let m = gui::dpi_x(margin);
    let mt = gui::dpi_y(margin);
    let bh = gui::dpi_y(btn_h);
    let gap = gui::dpi_y(12);
    let btn_gap = gui::dpi_x(8);
    let btn_y = (ch - mt - bh).max(mt);
    let mut right = cw - m;
    for (h, w_logical) in buttons {
        let bw = gui::dpi_x(*w_logical);
        let bx = (right - bw).max(0);
        let _ = h.MoveWindow(w::POINT { x: bx, y: btn_y }, w::SIZE { cx: bw, cy: bh }, true);
        right = bx - btn_gap;
    }
    let list_w = (cw - m * 2).max(1);
    let list_h = (btn_y - gap - mt).max(1);
    let _ = list.MoveWindow(w::POINT { x: m, y: mt }, w::SIZE { cx: list_w, cy: list_h }, true);
}

fn modal_window_styled(title: &str, w: i32, h: i32, extra: co::WS) -> (gui::WindowModal, ModalArm) {
    // headless（debug-server 撮影）時は生成時にアクティブ化させない。VISIBLE 付きで top-level
    // 窓を作ると初回 show が SW_SHOW 相当でフォアグラウンド化し、画面に出ない（親を画面外退避
    // 済み）状態でも手前で作業中のアプリが一瞬フォーカスを失う。そこで VISIBLE を外して生成し
    // （活性化しない）、wm_create 末尾で SW_SHOWNOACTIVATE により活性化せず可視化する。可視には
    // なるので /snapshot/modal の PrintWindow は中身を撮れる（不可視のままだと真っ黒になる）。
    let mut style = co::WS::CAPTION | co::WS::BORDER | extra;
    let mut ex_style = co::WS_EX::LEFT | co::WS_EX::DLGMODALFRAME;
    #[cfg(feature = "debug-server")]
    let headless = crate::debug_server::parse_headless();
    #[cfg(not(feature = "debug-server"))]
    let headless = false;
    if headless {
        ex_style |= co::WS_EX::NOACTIVATE;
    } else {
        style |= co::WS::VISIBLE;
    }
    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title,
        size: gui::dpi(w, h),
        style,
        ex_style,
        process_dlg_msgs: true,
        ..Default::default()
    });
    let arm = ModalArm::new();

    // 生成時：初期フォーカス → debug-server 登録（未指定なら最小エントリを自動 push）→
    // ダイアログ固有の on_create。これで登録忘れでも必ず観測可能になる。
    {
        let wf = wnd.clone();
        let oc = arm.on_create.clone();
        #[cfg(feature = "debug-server")]
        let reg = arm.reg.clone();
        #[cfg(feature = "debug-server")]
        let fallback = title.to_owned();
        wnd.on().wm_create(move |_| {
            focus_initial(wf.hwnd());
            #[cfg(feature = "debug-server")]
            {
                use crate::debug_server::modal_registry as reg_mod;
                let hp = wf.hwnd().ptr() as isize;
                match reg.borrow_mut().take() {
                    Some(Reg::Plain { kind, title, prompt, has_input, buttons }) => {
                        reg_mod::push(kind, &title, &prompt, hp, has_input, buttons)
                    }
                    Some(Reg::List { kind, title, items, selected, buttons }) => {
                        reg_mod::push_list(kind, &title, items, selected, hp, buttons)
                    }
                    Some(Reg::ListView { kind, title, buttons, hooks }) => {
                        reg_mod::push_list_view(kind, &title, hp, buttons, hooks)
                    }
                    None => reg_mod::push("modal", &fallback, "", hp, false, Vec::new()),
                }
            }
            if let Some(f) = oc.borrow_mut().take() {
                f(wf.hwnd())?;
            }
            // headless 時は VISIBLE 無しで生成しているので、初期フォーカス等を済ませた後に
            // 活性化せず可視化する。これで撮影（PrintWindow）は撮れるが前景は奪わない。
            // 非アクティブ表示は初回 WM_PAINT が遅延するため、子まで同期再描画して、開いた直後の
            // /snapshot/modal でも自前描画ペインの中身が揃うようにする。
            #[cfg(feature = "debug-server")]
            if crate::debug_server::parse_headless() {
                // 非表示のうちに確立したフォーカス（focus_initial または on_create の指定）は
                // 可視化で外れることがある。再計算はせず、確立済みのフォーカスを表示後に再適用して
                // キー操作（IsDialogMessage の矢印/Tab 翻訳）が確実に効くようにする。
                let focused = w::HWND::GetFocus();
                wf.hwnd().ShowWindow(co::SW::SHOWNOACTIVATE);
                if let Some(f) = focused {
                    f.SetFocus();
                }
                if let Ok(rc) = wf.hwnd().GetClientRect() {
                    let _ = wf.hwnd().RedrawWindow(
                        rc,
                        &w::HRGN::NULL,
                        co::RDW::INVALIDATE | co::RDW::ERASE | co::RDW::ALLCHILDREN | co::RDW::UPDATENOW,
                    );
                }
            }
            Ok(0)
        });
    }
    // 閉鎖時：固有処理を済ませてから登録を取り除く。NCDESTROY は winsafe の内部後始末と被るので
    // DESTROY で行う。winsafe は通常メッセージを「最後に登録された 1 つだけ」実行して打ち切るため、
    // WM_DESTROY のハンドラはここ 1 つに集約する（固有処理は `ModalArm::on_destroy` から呼ぶ）。
    {
        let od = arm.on_destroy.clone();
        wnd.on().wm(co::WM::DESTROY, move |_| {
            for f in od.borrow().iter() {
                f();
            }
            #[cfg(feature = "debug-server")]
            crate::debug_server::modal_registry::pop();
            Ok(0)
        });
    }

    (wnd, arm)
}

/// 通常モーダルの登録情報＋生成時処理を従来の引数並びでまとめて仕込む簡易版。
pub fn arm_modal(
    arm: &ModalArm,
    kind: &'static str,
    reg_title: &str,
    reg_prompt: &str,
    has_input: bool,
    buttons: Vec<(String, u16)>,
    on_create: impl Fn(&w::HWND) -> w::AnyResult<()> + 'static,
) {
    #[cfg(feature = "debug-server")]
    arm.plain(kind, reg_title, reg_prompt, has_input, buttons);
    #[cfg(not(feature = "debug-server"))]
    let _ = (kind, reg_title, reg_prompt, has_input, buttons);
    arm.on_create(on_create);
}

/// 原作 WinForms の「ロード時にタブ順先頭のコントロールへフォーカス」を再現する基盤処理。
///
/// winsafe の既定（`delegate_focus_to_first_child`）は先頭の子＝ラベル等にもフォーカスを
/// 投げるだけで、矢印操作の起点にならない。ここでは**先頭の「可視・有効・WS_TABSTOP」
/// コントロール**を z 順で探し、それがラジオなら**同グループの選択中ラジオ**へ寄せる
/// （開いてすぐ矢印で選べ、最初の矢印で選択を失わない＝原作の手触り）。各ダイアログは
/// `wm_create` でこれを呼ぶだけ＝初期フォーカスのロジックを基盤に集約する。
pub fn focus_initial(parent: &w::HWND) {
    let style = |h: &w::HWND| h.GetWindowLongPtr(co::GWLP::STYLE) as u32;
    let is_radio = |h: &w::HWND| {
        h.GetClassName().map(|c| c.eq_ignore_ascii_case("button")).unwrap_or(false)
            && matches!(style(h) & 0x0F, 0x04 | 0x09)
    };
    let checked = |h: &w::HWND| unsafe { h.SendMessage(w::msg::bm::GetCheck {}) } == co::BST::CHECKED;

    // タブ順の先頭コントロール（ダイアログマネージャの規則で解決＝z 順依存しない）。
    let Ok(first) = parent.GetNextDlgTabItem(&w::HWND::NULL, false) else {
        return;
    };
    if first.ptr().is_null() {
        return;
    }

    // 先頭がラジオで未選択なら、同グループの選択中ラジオへ寄せる（GetNextDlgGroupItem は
    // グループ内を巡回する）。選択を失わずに矢印で操作できる＝原作の手触り。
    let focus = if is_radio(&first) && !checked(&first) {
        let mut found: Option<w::HWND> = None;
        let mut cur = unsafe { first.raw_copy() };
        for _ in 0..64 {
            let Ok(next) = parent.GetNextDlgGroupItem(&cur, false) else {
                break;
            };
            if next.ptr() == first.ptr() {
                break;
            }
            if is_radio(&next) && checked(&next) {
                found = Some(unsafe { next.raw_copy() });
                break;
            }
            cur = next;
        }
        found.unwrap_or(first)
    } else {
        first
    };
    focus.SetFocus();
}

/// メッセージボックスのスタイル。整数値は原作 `RecordsLib.MessageStyle` に一致させ、
/// 将来スクリプトからの `int` → enum 変換をそのまま通せるようにする。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum MessageStyle {
    OkOnly = 1,
    OkCancel = 2,
    AbortRetryIgnore = 3,
    YesNo = 4,
    YesNoCancel = 5,
    YesNoAll = 6,
    YesNoCancelAll = 7,
    RetryCancel = 8,
    Warning = 9,
    Error = 10,
}

/// メッセージボックスの結果。原作 `RecordsLib.MessageResult` 準拠。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageResult {
    Ok,
    Cancel,
    Abort,
    Retry,
    Ignore,
    Yes,
    No,
    YesAll,
    NoAll,
}

impl MessageStyle {
    /// 左に表示する system アイコン（無ければ None）。
    fn icon(self) -> Option<co::IDI> {
        use MessageStyle::*;
        match self {
            OkOnly => Some(co::IDI::INFORMATION),
            Warning => Some(co::IDI::WARNING),
            Error => Some(co::IDI::ERROR),
            OkCancel | AbortRetryIgnore | YesNo | YesNoCancel | YesNoAll | YesNoCancelAll
            | RetryCancel => Some(co::IDI::QUESTION),
        }
    }

    /// ボタンを押さず閉じた場合の既定結果。
    fn default_result(self) -> MessageResult {
        use MessageResult as R;
        use MessageStyle::*;
        match self {
            OkOnly | Warning | Error => R::Ok,
            OkCancel | RetryCancel => R::Cancel,
            YesNo | YesNoCancel | YesNoAll | YesNoCancelAll => R::No,
            AbortRetryIgnore => R::Abort,
        }
    }

    fn has_all_checkbox(self) -> bool {
        matches!(self, MessageStyle::YesNoAll | MessageStyle::YesNoCancelAll)
    }

    /// (ラベル, 基本結果) の並び。先頭が既定ボタン(Enter)、`cancel_index` が Esc。
    fn buttons(self) -> Vec<(&'static str, MessageResult)> {
        use MessageResult as R;
        use MessageStyle::*;
        match self {
            OkOnly | Warning | Error => vec![("OK", R::Ok)],
            OkCancel => vec![("OK", R::Ok), ("キャンセル", R::Cancel)],
            YesNo | YesNoAll => vec![("はい(&Y)", R::Yes), ("いいえ(&N)", R::No)],
            YesNoCancel | YesNoCancelAll => {
                vec![("はい(&Y)", R::Yes), ("いいえ(&N)", R::No), ("キャンセル", R::Cancel)]
            }
            AbortRetryIgnore => {
                vec![("中止(&A)", R::Abort), ("再試行(&R)", R::Retry), ("無視(&I)", R::Ignore)]
            }
            RetryCancel => vec![("再試行(&R)", R::Retry), ("キャンセル", R::Cancel)],
        }
    }

    /// Esc が対応するボタン index（原作 CancelButton。単一ボタンは 0）。
    fn cancel_index(self) -> usize {
        use MessageStyle::*;
        match self {
            OkOnly | Warning | Error => 0,
            _ => 1,
        }
    }
}

/// 基本結果を「すべてに適用」チェック状態で最終結果に変換する。
fn finalize(base: MessageResult, all_checked: bool) -> MessageResult {
    match base {
        MessageResult::Yes if all_checked => MessageResult::YesAll,
        MessageResult::No if all_checked => MessageResult::NoAll,
        other => other,
    }
}

/// メッセージを UI フォント（ラベルと同じ `lfMenuFont`）で測り、論理単位の
/// (ラベル幅, ラベル高) と**折返し済みテキスト**を返す。幅は最長行に合わせ `[MIN,MAX]` に
/// クランプし、その幅で**手動折返し**する（空白優先・無ければ文字境界で割る）。`DrawText` の
/// `DT_WORDBREAK` は空白でしか折らずパス等の連続文字列が切れるため、自前で折って切れを防ぐ。
/// 測定できなければ元テキストをそのまま返す。
fn measure_message(message: &str) -> (i32, i32, String) {
    const MIN_LW: i32 = 300;
    const MAX_LW: i32 = 560;
    let measured = (|| -> w::SysResult<(i32, i32, String)> {
        let mut ncm = w::NONCLIENTMETRICS::default();
        unsafe {
            w::SystemParametersInfo(
                co::SPI::GETNONCLIENTMETRICS,
                std::mem::size_of::<w::NONCLIENTMETRICS>() as u32,
                &mut ncm,
                co::SPIF::NoValue,
            )?;
        }
        let font = w::HFONT::CreateFontIndirect(&ncm.lfMenuFont)?;
        let dc = w::HWND::NULL.GetDC()?;
        let _sel = dc.SelectObject(&*font)?;
        let width = |s: &str| dc.GetTextExtentPoint32(s).map(|z| z.cx).unwrap_or(0);
        let line_h = dc.GetTextExtentPoint32("Ag").map(|z| z.cy).unwrap_or(16);

        let fx = gui::dpi_x(1000).max(1) as i64;
        let fy = gui::dpi_y(1000).max(1) as i64;
        let to_lx = |p: i32| (p as i64 * 1000 / fx) as i32;
        let to_ly = |p: i32| ((p as i64 * 1000 + fy - 1) / fy) as i32;

        // 折返し幅（物理）＝最長行の自然幅を [MIN,MAX] にクランプ。
        let natural = message.split('\n').map(&width).max().unwrap_or(0);
        let lw = (to_lx(natural) + 8).clamp(MIN_LW, MAX_LW);
        let max_w = gui::dpi_x(lw);

        // 手動折返し：空白で詰めていき、語単体が幅を超えるなら文字境界で割る。
        let mut out: Vec<String> = Vec::new();
        for line in message.split('\n') {
            if width(line) <= max_w {
                out.push(line.to_string());
                continue;
            }
            let mut cur = String::new();
            for word in line.split(' ') {
                let cand =
                    if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
                if width(&cand) <= max_w {
                    cur = cand;
                    continue;
                }
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut rest = word;
                while width(rest) > max_w {
                    let mut cut = 0;
                    for (i, _) in rest.char_indices().skip(1) {
                        if width(&rest[..i]) > max_w {
                            break;
                        }
                        cut = i;
                    }
                    if cut == 0 {
                        // 1文字でも幅を超える（極端に狭い）→ 1文字ずつ進めて無限ループを防ぐ。
                        cut = rest.char_indices().nth(1).map(|(i, _)| i).unwrap_or(rest.len());
                    }
                    out.push(rest[..cut].to_string());
                    rest = &rest[cut..];
                }
                cur = rest.to_string();
            }
            out.push(cur);
        }
        if out.is_empty() {
            out.push(String::new());
        }
        let h_phys = out.len() as i32 * line_h;
        Ok((lw, to_ly(h_phys).max(18) + 6, out.join("\n")))
    })();
    measured.unwrap_or_else(|_| (MIN_LW, 48, message.to_string()))
}


/// 入力ボックスの入力種別。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// 通常テキスト。
    Plain,
    /// 伏字（パスワード）。
    Password,
}

/// 拡張子の前（最後の `.` の手前）の UTF-16 位置を返す。ディレクトリ・拡張子なし・
/// 先頭ドット（`.gitignore` 等）は末尾位置。`EM_SETSEL` のキャレット位置に使う。
fn before_ext_pos(name: &str, is_dir: bool) -> i32 {
    let end = name.encode_utf16().count() as i32;
    if is_dir {
        return end;
    }
    match name.rfind('.') {
        Some(0) | None => end,
        Some(idx) => name[..idx].encode_utf16().count() as i32,
    }
}

/// 入力欄の初期選択。`AsIs`＝明示設定なし（従来）、`All`＝全選択（既定値を上書きしやすく
/// する・マスク入力で使う）、`BeforeExt`＝拡張子の前にキャレット（原作 RenameStyle
/// "BeforeExtension"・選択なし）。改名系入力で使う。
#[derive(Clone, Copy)]
pub enum InputSelect {
    AsIs,
    All,
    BeforeExt { is_dir: bool },
}

impl InputSelect {
    /// テキスト `text` を持つ `edit` に初期選択を適用する（フォーカス後に呼ぶ）。
    fn apply(self, edit: &gui::Edit, text: &str) {
        match self {
            InputSelect::All => {
                let n = text.encode_utf16().count() as i32;
                edit.set_selection(0, n);
            }
            InputSelect::BeforeExt { is_dir } => {
                let pos = before_ext_pos(text, is_dir);
                edit.set_selection(pos, pos);
            }
            InputSelect::AsIs => {}
        }
    }
}

/// 作成するリンクの種類。[`link_kind_box`] の選択結果で、リンク作成の実処理の分岐に使う。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// Windows ショートカット（`.lnk`）。
    Shortcut,
    /// NTFS シンボリックリンク（ファイル/ディレクトリ両用・要特権か開発者モード）。
    Symlink,
    /// NTFS ジャンクション（ディレクトリ専用・特権不要）。
    Junction,
}

/// 書庫への追加方式。同名エントリがあるときの分岐に使う。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArchiveAddMode {
    /// 衝突分はスキップし、残りを append で足す（既存を壊さない・高速）。
    Append,
    /// 全体を再構築して同名を置換する（CP932 名は UTF-8 へ近代化される）。
    Rebuild,
}

/// 圧縮ダイアログで選べる形式。実際に作る形式は最終的な出力名の拡張子で決まるが、
/// 個別圧縮や、名前に既知の拡張子が無いときの既定を決めるのにこの選択を使う。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CompressFormat {
    Zip,
    SevenZ,
    Xz,
}

/// 各形式を選んだときの既定の出力名（フル名）。名前欄が未編集のあいだ、形式ラジオの切替で
/// 名前欄をこの値へ差し替える（xz は対象の束ね要否で `.xz`／`.tar.xz` が入れ替わる）。
#[derive(Clone)]
pub struct CompressDefaults {
    pub zip: String,
    pub sevenz: String,
    pub xz: String,
}

/// 圧縮ダイアログの結果（書庫名・個別圧縮・選択形式）。
#[derive(Clone, PartialEq, Eq)]
pub struct CompressChoice {
    /// 出力する書庫名（まとめて1つに圧縮する場合に使う）。実際の形式はこの拡張子で決まる。
    pub name: String,
    /// 選択項目を個別に圧縮する（true）か、まとめて1つにする（false）か。
    pub one_by_one: bool,
    /// 選択した形式（個別圧縮・拡張子省略時の既定に使う）。
    pub format: CompressFormat,
}


/// ソート設定ダイアログの種別ラジオ（表示ラベル → ソート種別・表示順）。
/// 自然順（エクスプローラ互換）を既定の名前順/拡張子順とし、コードポイント順は
/// 名前/拡張子それぞれ「（コード）」付きの独立した種別として並べる。
pub(crate) const SORT_KINDS: &[(&str, SortType)] = &[
    ("名前順(&F)", SortType::FileNameExpLike),
    ("拡張子(&E)", SortType::ExtensionExpLike),
    ("更新日付(&D)", SortType::LastWriteTime),
    ("サイズ(&S)", SortType::Length),
    ("属性(&A)", SortType::Attribute),
    ("作成日付(&C)", SortType::CreateTime),
    ("名前順（コード）(&N)", SortType::FileName),
    ("拡張子（コード）(&X)", SortType::Extension),
];


/// 名前変更ダイアログの結果。`name` は単一時の変更後名（複数一括は `None`）、
/// `attrs` は RO/隠し/システム/アーカイブの各設定（`Some` で設定・`None` で据え置き）、
/// `modified`/`created` は更新日時・作成日時（`Some` で設定・`None` で据え置き）。
pub struct RenameResult {
    pub name: Option<String>,
    pub attrs: [Option<bool>; 4],
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    /// 複数一括時の名前変換（単一は即時に `name` へ反映済みなので `None`）。
    pub name_case: NameCase,
    /// 単一ディレクトリ時、属性を配下へ再帰適用するか。
    pub sub_attr: bool,
    /// 単一ディレクトリ時、日時を配下へ再帰適用するか。
    pub sub_time: bool,
}

/// 日時欄の横の「...」ボタンに、原作の日時クイック設定メニュー（現在時刻／00:00:00）を
/// 配線する。選んだ値を `edit` に書き込む。`TrackPopupMenu` 自身がモーダルループを回す
/// ので、`TPM::RETURNCMD` で選択コマンドを同期取得する。
fn quick_time_menu(btn: &gui::Button, edit: &gui::Edit) {
    let edit = edit.clone();
    let btnf = btn.clone();
    btn.on().bn_clicked(move || {
        let mut menu = w::HMENU::CreatePopupMenu()?;
        menu.AppendMenu(co::MF::STRING, w::IdMenu::Id(1), w::BmpPtrStr::from_str("現在時刻"))?;
        menu.AppendMenu(co::MF::STRING, w::IdMenu::Id(2), w::BmpPtrStr::from_str("00:00:00"))?;
        let rc = btnf.hwnd().GetWindowRect()?;
        let chosen = menu.TrackPopupMenu(
            co::TPM::RETURNCMD | co::TPM::LEFTALIGN | co::TPM::TOPALIGN,
            w::POINT::with(rc.left, rc.bottom),
            btnf.hwnd(),
        )?;
        let now = SystemTime::now();
        match chosen {
            Some(1) => edit.set_text(&format_local(now))?,
            Some(2) => edit.set_text(&format_local(floor_to_local_midnight(now)))?,
            _ => {}
        }
        menu.DestroyMenu()?;
        Ok(())
    });
}

/// チェックボックスの状態を「設定する/しない/据え置き」に読み替える。
fn cb_tristate(cb: &gui::CheckBox) -> Option<bool> {
    match cb.state() {
        co::BST::CHECKED => Some(true),
        co::BST::UNCHECKED => Some(false),
        _ => None,
    }
}



#[cfg(test)]
mod guard_tests {
    /// 契約: `show_modal` の直呼びは [`super::show_modal_guarded`]（このモジュール）だけが
    /// 行う。直呼びのモーダルが増えると、表示中フラグが立たず `pump_tasks` がモーダルの
    /// 内部ループから再入して、モーダルの下でペイン再読込などの完了処理が走ってしまう。
    #[test]
    fn show_modal_is_called_only_through_the_guarded_wrapper() {
        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        scan(&src_root, &mut offenders);
        assert!(
            offenders.is_empty(),
            "show_modal は dialog::show_modal_guarded 経由で呼ぶこと: {offenders:?}"
        );
    }

    fn scan(dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan(&path, out);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            // 関門自身（このファイル）は除外する。
            if path.ends_with("dialog/mod.rs") || path.ends_with(r"dialog\mod.rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (i, line) in text.lines().enumerate() {
                if line.contains(".show_modal(") {
                    out.push(format!("{}:{}", path.display(), i + 1));
                }
            }
        }
    }
}
