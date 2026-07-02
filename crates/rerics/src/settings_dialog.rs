//! 設定ダイアログ。左ナビ（フラットな自前描画リスト：ジャンル見出しは選択不可・ページだけが
//! ↑↓／クリックで選べる）と中央の詳細 pane、外観カテゴリ選択中だけ右へ出す
//! 「ミニ全体窓」プレビューの構成。
//!
//! 編集中の値はすべて [`Shared`] へ即時反映し、配色・フォント・レイアウト寸法・テーマの
//! 変更はその場でプレビューへ反映する（ライブプレビュー）。`OK`／`適用` で現在の [`Config`]
//! を `on_apply` コールバックへ渡し（呼び出し側がライブ反映＋差分保存する）、`適用` は閉じずに
//! 継続、`OK` は閉じる。`キャンセル` は最後の `適用` 以降の編集を破棄して閉じる。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{
    Bookmark, Colors, Column, ColumnKind, Config, FileOpSettings, IconSize, Layout,
    MenuDef, MenuItem, Rgb, ResolvedTheme, SizeFormat, SortType, Theme, WheelAction,
};
use winsafe::{self as w, co, gui, msg::lb, prelude::*};

use crate::key_editor::{KeyCategory, KeyEditor};

/// 自前描画コントロールをオフスクリーン DC へ描かせるメッセージ。`PrintWindow`
/// （デバッグ制御サーバの `/snapshot/modal`）が子ごとに送るので、これに応答しないと黒く写る。
pub(crate) const WM_PRINTCLIENT: u32 = 0x0318;

/// 配色テーブル：行ラベルと `Colors` の各色への get/set（表示順）。
#[allow(clippy::type_complexity)]
type ColorFields = &'static [(&'static str, fn(&Colors) -> Rgb, fn(&mut Colors, Rgb))];

/// ファイル一覧・ログの配色（「配色」ページ）。
const COLOR_FIELDS: ColorFields = &[
    ("通常ファイル", |c| c.file_normal, |c, v| c.file_normal = v),
    ("ディレクトリ", |c| c.directory, |c, v| c.directory = v),
    ("読取専用", |c| c.readonly, |c, v| c.readonly = v),
    ("システム", |c| c.system, |c, v| c.system = v),
    ("隠し", |c| c.hidden, |c, v| c.hidden = v),
    ("背景", |c| c.background, |c, v| c.background = v),
    ("背景2(溝)", |c| c.background2, |c, v| c.background2 = v),
    ("選択文字", |c| c.selected_file, |c, v| c.selected_file = v),
    ("選択背景", |c| c.selected_file_bg, |c, v| c.selected_file_bg = v),
    ("カーソル", |c| c.cursor, |c, v| c.cursor = v),
    ("ログ背景", |c| c.log_background, |c, v| c.log_background = v),
    ("ログ通常", |c| c.log_normal, |c, v| c.log_normal = v),
    ("ログ情報", |c| c.log_info, |c, v| c.log_info = v),
    ("ログ警告", |c| c.log_warning, |c, v| c.log_warning = v),
    ("ログエラー", |c| c.log_error, |c, v| c.log_error = v),
];

/// テキストビューアの配色（「テキストビューア」ページ）。
const VIEWER_COLOR_FIELDS: ColorFields = &[
    ("背景", |c| c.viewer_background, |c, v| c.viewer_background = v),
    ("本文", |c| c.viewer_text, |c, v| c.viewer_text = v),
    ("行番号", |c| c.viewer_line, |c, v| c.viewer_line = v),
    ("区切り線", |c| c.viewer_separator, |c, v| c.viewer_separator = v),
    ("記号(改行/EOF)", |c| c.viewer_symbol, |c, v| c.viewer_symbol = v),
    ("検索文字", |c| c.viewer_find_text, |c, v| c.viewer_find_text = v),
    ("検索背景", |c| c.viewer_find_bg, |c, v| c.viewer_find_bg = v),
    ("検索カーソル行の下線", |c| c.viewer_cursor, |c, v| c.viewer_cursor = v),
];

/// レイアウト寸法フィールドのラベルと get/set（すべて論理 px）。
#[allow(clippy::type_complexity)]
const LAYOUT_FIELDS: &[(&str, fn(&Layout) -> i32, fn(&mut Layout, i32))] = &[
    ("余白", |l| l.margin, |l, v| l.margin = v),
    ("ペイン間隔", |l| l.gap, |l, v| l.gap = v),
    ("パスバー高", |l| l.bar_height, |l, v| l.bar_height = v),
    ("パスバー間隔", |l| l.bar_gap, |l, v| l.bar_gap = v),
    ("ステータス高", |l| l.status_bar_height, |l, v| l.status_bar_height = v),
    ("タブ高", |l| l.tab_height, |l, v| l.tab_height = v),
    ("ログ行数", |l| l.log_height, |l, v| l.log_height = v),
    ("ログ間隔", |l| l.log_gap, |l, v| l.log_gap = v),
    ("スクロールバー幅", |l| l.scrollbar_width, |l, v| l.scrollbar_width = v),
    ("スプリッタ幅", |l| l.splitter_width, |l, v| l.splitter_width = v),
    ("最大化余白", |l| l.maximize_margin, |l, v| l.maximize_margin = v),
    ("境界移動量", |l| l.border_unit, |l, v| l.border_unit = v),
];

/// 左ナビの項目名（順序はセクション pane と一致させる）。
fn to_colorref(c: Rgb) -> w::COLORREF {
    w::COLORREF::from_rgb(c.r, c.g, c.b)
}

fn from_colorref(c: w::COLORREF) -> Rgb {
    Rgb::new(c.GetRValue(), c.GetGValue(), c.GetBValue())
}

/// 色選択コモンダイアログを開く。OK なら選んだ色、キャンセルなら元の色を返す。
fn choose_color(owner: &w::HWND, initial: Rgb) -> Rgb {
    let mut custom = [w::COLORREF::from_rgb(255, 255, 255); 16];
    let mut cc = w::CHOOSECOLOR::default();
    cc.hwndOwner = unsafe { owner.raw_copy() };
    cc.Flags = co::CC::ANYCOLOR | co::CC::FULLOPEN | co::CC::RGBINIT;
    cc.rgbResult = to_colorref(initial);
    cc.set_lpCustColors(Some(&mut custom));
    match w::ChooseColor(&mut cc) {
        Ok(true) => from_colorref(cc.rgbResult),
        _ => initial,
    }
}

// winsafe 0.0.27 にフォント選択コモンダイアログのラッパが無いため、comdlg32 を直接呼ぶ。
// （comdlg32 は winsafe の `ChooseColor` で既にリンクされている。）
const CF_SCREENFONTS: u32 = 0x0000_0001;
const CF_INITTOLOGFONTSTRUCT: u32 = 0x0000_0040;
const CF_FORCEFONTEXIST: u32 = 0x0001_0000;

/// [`CHOOSEFONTW`](https://learn.microsoft.com/en-us/windows/win32/api/commdlg/ns-commdlg-choosefontw)。
#[repr(C)]
struct ChooseFontStruct {
    l_struct_size: u32,
    hwnd_owner: *mut std::ffi::c_void,
    hdc: *mut std::ffi::c_void,
    lp_log_font: *mut w::LOGFONT,
    i_point_size: i32,
    flags: u32,
    rgb_colors: u32,
    l_cust_data: isize,
    lpfn_hook: *mut std::ffi::c_void,
    lp_template_name: *const u16,
    h_instance: *mut std::ffi::c_void,
    lpsz_style: *mut u16,
    n_font_type: u16,
    ___missing_alignment: u16,
    n_size_min: i32,
    n_size_max: i32,
}

#[link(name = "comdlg32")]
unsafe extern "system" {
    fn ChooseFontW(lpcf: *mut ChooseFontStruct) -> i32;
}

/// フォント選択コモンダイアログを開く。OK なら選んだ（フォント名, 論理 px サイズ）、
/// キャンセルなら `None`。サイズは `list_font` と同じ `dpi_y` スケールの逆算で論理 px へ戻す。
fn choose_font(owner: &w::HWND, family: &str, size: i32) -> Option<(String, i32)> {
    let mut lf = w::LOGFONT::new_face(-gui::dpi_y(size), family);
    lf.lfCharSet = co::CHARSET::DEFAULT;
    let mut cf = ChooseFontStruct {
        l_struct_size: std::mem::size_of::<ChooseFontStruct>() as u32,
        hwnd_owner: owner.ptr(),
        hdc: std::ptr::null_mut(),
        lp_log_font: &mut lf,
        i_point_size: 0,
        flags: CF_SCREENFONTS | CF_INITTOLOGFONTSTRUCT | CF_FORCEFONTEXIST,
        rgb_colors: 0,
        l_cust_data: 0,
        lpfn_hook: std::ptr::null_mut(),
        lp_template_name: std::ptr::null(),
        h_instance: std::ptr::null_mut(),
        lpsz_style: std::ptr::null_mut(),
        n_font_type: 0,
        ___missing_alignment: 0,
        n_size_min: 0,
        n_size_max: 0,
    };
    if unsafe { ChooseFontW(&mut cf) } == 0 {
        return None;
    }
    let scale = gui::dpi_y(96).max(1);
    let new_size = ((lf.lfHeight.unsigned_abs() as i64 * 96) / scale as i64) as i32;
    Some((lf.lfFaceName(), new_size.clamp(6, 72)))
}

/// Edit の整数値を取り出す。空・解釈不能なら `cur` を返す。
fn parse_or(edit: &gui::Edit, cur: i32) -> i32 {
    edit.text()
        .ok()
        .and_then(|t| t.trim().parse::<i32>().ok())
        .unwrap_or(cur)
}

/// プレビュー一覧のフォント（設定のファミリ・サイズ）。実ファイル一覧と同じ生成条件。
fn list_font(
    family: &str,
    size: i32,
    bold: bool,
) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
    w::HFONT::CreateFont(
        w::SIZE { cx: 0, cy: -gui::dpi_y(size) },
        0,
        0,
        if bold { co::FW::BOLD } else { co::FW::NORMAL },
        false,
        false,
        false,
        co::CHARSET::DEFAULT,
        co::OUT_PRECIS::DEFAULT,
        co::CLIP::DEFAULT_PRECIS,
        co::QUALITY::CLEARTYPE,
        co::PITCH::FIXED,
        family,
    )
}

/// 矩形を単色で塗る。
fn fill(dc: &w::HDC, l: i32, t: i32, r: i32, b: i32, c: Rgb) -> w::AnyResult<()> {
    let br = w::HBRUSH::CreateSolidBrush(to_colorref(c))?;
    dc.FillRect(w::RECT { left: l, top: t, right: r, bottom: b }, &br)?;
    Ok(())
}

/// 矩形の周囲に 1px の枠を描く。
fn frame(dc: &w::HDC, l: i32, t: i32, r: i32, b: i32, c: Rgb) -> w::AnyResult<()> {
    let br = w::HBRUSH::CreateSolidBrush(to_colorref(c))?;
    dc.FillRect(w::RECT { left: l, top: t, right: r, bottom: t + 1 }, &br)?;
    dc.FillRect(w::RECT { left: l, top: b - 1, right: r, bottom: b }, &br)?;
    dc.FillRect(w::RECT { left: l, top: t, right: l + 1, bottom: b }, &br)?;
    dc.FillRect(w::RECT { left: r - 1, top: t, right: r, bottom: b }, &br)?;
    Ok(())
}

/// プレビュー行の装飾。
#[derive(Clone, Copy)]
enum Deco {
    Plain,
    Cursor,
    Selected,
}

/// ダイアログ全体で共有する編集中の設定と、プレビュー／配色編集の対象テーマ。
pub(crate) struct Shared {
    pub(crate) cfg: RefCell<Config>,
    /// 編集・プレビュー対象（true=ダーク, false=ライト）。
    target_dark: Cell<bool>,
}

impl Shared {
    /// 現在の編集対象テーマの配色を取り出す。
    fn target_colors(&self) -> Colors {
        let c = self.cfg.borrow().colors;
        if self.target_dark.get() { c.dark } else { c.light }
    }
}

/// タブバー（上端・全幅）を縮小描画する。
fn draw_tabs(dc: &w::HDC, x: i32, y: i32, w: i32, h: i32, fh: i32, c: &Colors) -> w::AnyResult<()> {
    if w <= 0 || h <= 0 {
        return Ok(());
    }
    fill(dc, x, y, x + w, y + h, c.background2)?;
    let tw = gui::dpi_x(96);
    let pad = gui::dpi_x(4);
    let top = y + gui::dpi_y(2);
    let tabs: [(&str, bool); 2] = [("1 C:\\src", true), ("2 D:\\backup", false)];
    let mut tx = x + pad;
    for (text, active) in tabs {
        if tx + tw > x + w {
            break;
        }
        fill(dc, tx, top, tx + tw, y + h, if active { c.background } else { c.background2 })?;
        frame(dc, tx, top, tx + tw, y + h, c.file_normal)?;
        dc.SetTextColor(to_colorref(if active { c.file_normal } else { c.directory }))?;
        if h - gui::dpi_y(2) >= fh {
            dc.TextOut(tx + pad, top + ((y + h - top) - fh) / 2, text)?;
        }
        tx += tw + pad;
    }
    Ok(())
}

/// 1 ペイン（パスバー・ファイルリスト・スクロールバー・ステータスバー）を縮小描画する。
#[allow(clippy::too_many_arguments)]
fn draw_pane(
    dc: &w::HDC,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    bar_h: i32,
    bar_gap: i32,
    status_h: i32,
    sb_w: i32,
    fh: i32,
    icons_show: bool,
    icon_px: i32,
    c: &Colors,
    active: bool,
) -> w::AnyResult<()> {
    if w <= 0 || h <= 0 {
        return Ok(());
    }
    let pad = gui::dpi_y(2);
    let left = x + gui::dpi_x(4);

    // パスバー。
    if bar_h > 0 {
        fill(dc, x, y, x + w, y + bar_h, c.background)?;
        dc.SetTextColor(to_colorref(c.file_normal))?;
        if bar_h >= fh {
            dc.TextOut(left, y + (bar_h - fh) / 2, "C:\\Users\\xia\\src")?;
        }
    }

    // ファイルリスト（パスバーの下・ステータスバーの上）。
    let list_y = y + bar_h + bar_gap;
    let status_y = (y + h - status_h).max(list_y);
    let list_b = status_y;
    fill(dc, x, list_y, x + w, list_b, c.background)?;

    // スクロールバー（右端）。
    let list_r = if sb_w > 0 && w > sb_w {
        let tr = x + w - sb_w;
        fill(dc, tr, list_y, x + w, list_b, c.background2)?;
        let th = ((list_b - list_y) / 3).max(1);
        fill(dc, tr, list_y, x + w, (list_y + th).min(list_b), c.scrollbar_thumb())?;
        tr
    } else {
        x + w
    };

    // 行（色割り当てとカーソル／選択の見え方を一通り示す）。アイコン表示時は左に代用の
    // 四角を置き、その分だけ行高も伸ばして（中・大で行が高くなる挙動を反映）名前を右へ寄せる。
    let row_h = if icons_show { (fh + pad * 2).max(icon_px) } else { fh + pad * 2 };
    let rows: [(&str, Rgb, Deco); 6] = [
        ("src", c.directory, Deco::Plain),
        ("readme.md", c.file_normal, Deco::Cursor),
        ("LICENSE", c.readonly, Deco::Plain),
        ("pagefile.sys", c.system, Deco::Plain),
        (".gitignore", c.hidden, Deco::Plain),
        // archive.zip は通常ファイル＝自然色は file_normal（マーク時のみ選択色になる）。
        ("archive.zip", c.file_normal, Deco::Selected),
    ];
    let mut ry = list_y;
    for (name, color, deco) in rows {
        if ry + row_h > list_b {
            break;
        }
        match deco {
            Deco::Selected => {
                // 実リストと同じく、アクティブ側は選択色＋選択文字色、非アクティブ側は
                // 選択背景を地色へ寄せて淡くし文字は自然色へ戻す（どちらが現側か一目で分かる）。
                let (bg, text) = if active {
                    (c.selected_file_bg, c.selected_file)
                } else {
                    (c.selected_file_bg.blend(c.background, 3, 5), color)
                };
                fill(dc, x, ry, list_r, ry + row_h, bg)?;
                dc.SetTextColor(to_colorref(text))?;
            }
            _ => {
                dc.SetTextColor(to_colorref(color))?;
            }
        }
        let mut text_left = left;
        if icons_show {
            let iy = ry + (row_h - icon_px) / 2;
            fill(dc, left, iy, left + icon_px, iy + icon_px, color)?;
            text_left = left + icon_px + gui::dpi_x(2);
        }
        dc.TextOut(text_left, ry + (row_h - fh) / 2, name)?;
        // カーソルはアクティブ側だけに出す（実リストも非アクティブ側にカーソルを描かない）。
        // 実リストのカーソルは行全体の枠ではなく文字直下の下線（`colors.cursor`）。
        if active && matches!(deco, Deco::Cursor) {
            let uy = ry + (row_h - fh) / 2 + fh - 1;
            fill(dc, x, uy, list_r, uy + 1, c.cursor)?;
        }
        ry += row_h;
    }

    // ステータスバー。
    if status_h > 0 {
        fill(dc, x, status_y, x + w, y + h, c.background2)?;
        dc.SetTextColor(to_colorref(c.file_normal))?;
        if status_h >= fh {
            dc.TextOut(left, status_y + (status_h - fh) / 2, "6 個  1 選択")?;
        }
    }
    Ok(())
}

/// ミニ・ログ（下端・全幅）を縮小描画する。
#[allow(clippy::too_many_arguments)]
fn draw_log(
    dc: &w::HDC,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    fh: i32,
    c: &Colors,
    font: &w::HFONT,
    font_bold: &w::HFONT,
) -> w::AnyResult<()> {
    if w <= 0 || h <= 0 {
        return Ok(());
    }
    fill(dc, x, y, x + w, y + h, c.log_background)?;
    let pad = gui::dpi_y(2);
    let left = x + gui::dpi_x(4);
    let row_h = fh + pad;
    // 実ログと同じく Info/Error は太字（log_view.rs paint_to と対応）。
    let logs: [(&str, Rgb, bool); 4] = [
        ("コピーを開始します", c.log_normal, false),
        ("3 個のファイルを選択しました", c.log_info, true),
        ("空き容量が少なくなっています", c.log_warning, false),
        ("アクセスが拒否されました", c.log_error, true),
    ];
    let mut ly = y + pad;
    for (text, color, bold) in logs {
        if ly + row_h > y + h {
            break;
        }
        dc.SelectObject(if bold { font_bold } else { font })?;
        dc.SetTextColor(to_colorref(color))?;
        dc.TextOut(left, ly, text)?;
        ly += row_h;
    }
    dc.SelectObject(font)?;
    Ok(())
}

/// 設定の効きをその場で見せるライブプレビュー（実レイアウトを縮小した「ミニ全体窓」）。
#[derive(Clone)]
struct Preview {
    wnd: gui::WindowControl,
    shared: Rc<Shared>,
}

impl Preview {
    fn new(parent: &(impl GuiParent + 'static), pos: (i32, i32), size: (i32, i32), shared: Rc<Shared>) -> Self {
        let wnd = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                position: pos,
                size,
                style: co::WS::CHILD | co::WS::VISIBLE | co::WS::CLIPSIBLINGS | co::WS::BORDER,
                ..Default::default()
            },
        );
        let me = Self { wnd, shared };
        let this = me.clone();
        me.wnd.on().wm_paint(move || this.on_paint());
        let this = me.clone();
        me.wnd.on().wm(unsafe { co::WM::from_raw(WM_PRINTCLIENT) }, move |p| {
            this.on_print(p.wparam);
            Ok(0)
        });
        me
    }

    fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    fn refresh(&self) {
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    fn on_paint(&self) -> w::AnyResult<()> {
        let hdc = self.hwnd().BeginPaint()?;
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        let mem = hdc.CreateCompatibleDC()?;
        let bmp = hdc.CreateCompatibleBitmap(cw, ch)?;
        let _sel = mem.SelectObject(&*bmp)?;
        self.render(&mem, cw, ch)?;
        hdc.BitBlt(
            w::POINT { x: 0, y: 0 },
            w::SIZE { cx: cw, cy: ch },
            &mem,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    /// `WM_PRINTCLIENT`：与えられた DC へ直接描く（オフスクリーン捕捉用）。
    fn on_print(&self, hdc_ptr: usize) {
        let hdc = unsafe { w::HDC::from_ptr(hdc_ptr as *mut std::ffi::c_void) };
        if let Ok(rc) = self.hwnd().GetClientRect() {
            let _ = self.render(&hdc, rc.right - rc.left, rc.bottom - rc.top);
        }
    }

    /// 実レイアウト（`layout.rs` / `pane_view.rs`）と同じ式で寸法を割り付けた縮小窓を描く。
    fn render(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        let (family, fsize, lay, icons) = {
            let cfg = self.shared.cfg.borrow();
            (cfg.font.family.clone(), cfg.font.size, cfg.layout.clone(), cfg.icons.clone())
        };
        let colors = self.shared.target_colors();

        let font = list_font(&family, fsize, false)?;
        let font_bold = list_font(&family, fsize, true)?;
        let _fsel = dc.SelectObject(&*font)?;
        let tm = dc.GetTextMetrics().ok();
        let fh = tm.as_ref().map(|t| t.tmHeight).unwrap_or(16);
        // ログ窓はフォントの行高（tmHeight + 外部レディング）× 行数で高さを決める。
        let log_line_h = tm.as_ref().map(|t| t.tmHeight + t.tmExternalLeading).unwrap_or(17);
        // アイコンの代用枠サイズ（file_list と同じ式：自動は行=フォント高に収める）。
        let icon_px = match icons.size.logical_px() {
            0 => gui::dpi_x(16).min(fh),
            logical => gui::dpi_x(logical),
        };
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;

        let m = gui::dpi_x(lay.margin);
        let my = gui::dpi_y(lay.margin);
        let splitter_w = gui::dpi_x(lay.splitter_width);
        let tab_h = gui::dpi_y(lay.tab_height);
        let log_h = lay.log_height.max(1) * log_line_h;
        let log_gap = gui::dpi_y(lay.log_gap);
        let bar_h = gui::dpi_y(lay.bar_height);
        let bar_gap = gui::dpi_y(lay.bar_gap);
        let status_h = gui::dpi_y(lay.status_bar_height);
        let sb_w = gui::dpi_x(lay.scrollbar_width);

        let bars_y = tab_h;
        let log_y = (ch - my - log_h).max(bars_y);
        let pane_top = bars_y;
        let pane_h = (log_y - log_gap - pane_top).max(0);

        let panes_total = (cw - m * 2 - splitter_w).max(0);
        let left_w = panes_total / 2;
        let right_w = panes_total - left_w;
        let left_x = m;
        let right_x = m + left_w + splitter_w;
        let log_w = cw - m * 2;

        // 余白・溝・スプリッタの地色。残りの矩形を上から punch していく。
        fill(dc, 0, 0, cw, ch, colors.background2)?;
        draw_tabs(dc, 0, 0, cw, tab_h, fh, &colors)?;
        draw_pane(dc, left_x, pane_top, left_w, pane_h, bar_h, bar_gap, status_h, sb_w, fh, icons.show, icon_px, &colors, true)?;
        draw_pane(dc, right_x, pane_top, right_w, pane_h, bar_h, bar_gap, status_h, sb_w, fh, icons.show, icon_px, &colors, false)?;
        draw_log(dc, left_x, log_y, log_w, log_h, fh, &colors, &font, &font_bold)?;
        Ok(())
    }
}

/// テキストビューアの配色プレビュー（サンプルコードをビューア配色で描く小窓）。
#[derive(Clone)]
struct ViewerPreview {
    wnd: gui::WindowControl,
    shared: Rc<Shared>,
}

/// プレビューに出すサンプル行（行番号・改行マーク・[EOF]・検索ヒットの見え方を示す）。
const VIEWER_SAMPLE: &[&str] = &[
    "fn main() {",
    "    let n = 42;",
    "    println!(\"{}\", n);",
    "}",
];

impl ViewerPreview {
    fn new(parent: &(impl GuiParent + 'static), pos: (i32, i32), size: (i32, i32), shared: Rc<Shared>) -> Self {
        let wnd = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                position: pos,
                size,
                style: co::WS::CHILD | co::WS::VISIBLE | co::WS::CLIPSIBLINGS | co::WS::BORDER,
                ..Default::default()
            },
        );
        let me = Self { wnd, shared };
        let this = me.clone();
        me.wnd.on().wm_paint(move || this.on_paint());
        let this = me.clone();
        me.wnd.on().wm(unsafe { co::WM::from_raw(WM_PRINTCLIENT) }, move |p| {
            this.on_print(p.wparam);
            Ok(0)
        });
        me
    }

    fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    fn refresh(&self) {
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    fn on_paint(&self) -> w::AnyResult<()> {
        let hdc = self.hwnd().BeginPaint()?;
        let rc = self.hwnd().GetClientRect()?;
        let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        let mem = hdc.CreateCompatibleDC()?;
        let bmp = hdc.CreateCompatibleBitmap(cw, ch)?;
        let _sel = mem.SelectObject(&*bmp)?;
        self.render(&mem, cw, ch)?;
        hdc.BitBlt(w::POINT { x: 0, y: 0 }, w::SIZE { cx: cw, cy: ch }, &mem, w::POINT { x: 0, y: 0 }, co::ROP::SRCCOPY)?;
        Ok(())
    }

    fn on_print(&self, hdc_ptr: usize) {
        let hdc = unsafe { w::HDC::from_ptr(hdc_ptr as *mut std::ffi::c_void) };
        if let Ok(rc) = self.hwnd().GetClientRect() {
            let _ = self.render(&hdc, rc.right - rc.left, rc.bottom - rc.top);
        }
    }

    fn render(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        let (family, fsize) = {
            let cfg = self.shared.cfg.borrow();
            (cfg.font.family.clone(), cfg.font.size)
        };
        let colors = self.shared.target_colors();
        let font = w::HFONT::CreateFont(
            w::SIZE { cx: 0, cy: -gui::dpi_y(fsize) },
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
            co::PITCH::FIXED,
            &family,
        )?;
        let _fsel = dc.SelectObject(&*font)?;
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;
        let tm = dc.GetTextMetrics().ok();
        let fh = tm.as_ref().map(|t| t.tmHeight).unwrap_or(16);
        let cwd = tm.as_ref().map(|t| t.tmAveCharWidth).unwrap_or(8).max(1);
        let lh = fh + gui::dpi_y(1);

        fill(dc, 0, 0, cw, ch, colors.viewer_background)?;

        let pad = gui::dpi_x(6);
        let sep_x = pad + cwd * 3;
        let body_x = sep_x + gui::dpi_x(6);
        let top = gui::dpi_y(6);
        let bottom = top + lh * VIEWER_SAMPLE.len() as i32;
        // 行番号と本文を仕切る縦線。
        fill(dc, sep_x, top, sep_x + gui::dpi_x(1), bottom, colors.viewer_separator)?;

        let find_line = 1usize; // 2行目を検索ヒット例にする。
        for (i, text) in VIEWER_SAMPLE.iter().enumerate() {
            let y = top + i as i32 * lh;
            if i == find_line {
                fill(dc, body_x, y, cw, y + lh, colors.viewer_find_bg)?;
            }
            dc.SetTextColor(to_colorref(colors.viewer_line))?;
            dc.TextOut(pad, y, &(i + 1).to_string())?;
            let text_col = if i == find_line { colors.viewer_find_text } else { colors.viewer_text };
            dc.SetTextColor(to_colorref(text_col))?;
            dc.TextOut(body_x, y, text)?;
            let end_x = body_x + text.chars().count() as i32 * cwd;
            dc.SetTextColor(to_colorref(colors.viewer_symbol))?;
            if i + 1 < VIEWER_SAMPLE.len() {
                dc.TextOut(end_x, y, "↓")?;
            } else {
                dc.TextOut(end_x + cwd, y, "[EOF]")?;
            }
        }
        Ok(())
    }
}

/// 配色を実色のスウォッチ付きで一覧し、ダブルクリック（または「変更」ボタン）で編集する自前リスト。
#[derive(Clone)]
struct SwatchList {
    wnd: gui::WindowControl,
    shared: Rc<Shared>,
    /// 色変更・リセット後に呼ぶ（対応するプレビューを再描画する）。
    on_change: Rc<dyn Fn()>,
    sel: Rc<Cell<usize>>,
    row_h: Rc<Cell<i32>>,
    /// このリストが編集する色フィールド（配色 or テキストビューア）。
    fields: ColorFields,
}

impl SwatchList {
    fn new(
        parent: &(impl GuiParent + 'static),
        pos: (i32, i32),
        size: (i32, i32),
        shared: Rc<Shared>,
        on_change: Rc<dyn Fn()>,
        fields: ColorFields,
    ) -> Self {
        let wnd = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                position: pos,
                size,
                style: co::WS::CHILD | co::WS::VISIBLE | co::WS::CLIPSIBLINGS | co::WS::BORDER,
                ..Default::default()
            },
        );
        let me = Self {
            wnd,
            shared,
            on_change,
            sel: Rc::new(Cell::new(0)),
            row_h: Rc::new(Cell::new(gui::dpi_y(24))),
            fields,
        };
        let this = me.clone();
        me.wnd.on().wm_paint(move || this.on_paint());
        let this = me.clone();
        me.wnd.on().wm_l_button_down(move |p| {
            this.on_click(p.coords);
            Ok(())
        });
        let this = me.clone();
        me.wnd.on().wm_l_button_dbl_clk(move |_| {
            this.edit_selected();
            Ok(())
        });
        let this = me.clone();
        me.wnd.on().wm(unsafe { co::WM::from_raw(WM_PRINTCLIENT) }, move |p| {
            this.on_print(p.wparam);
            Ok(0)
        });
        me
    }

    fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    fn refresh(&self) {
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// `WM_PRINTCLIENT`：与えられた DC へ直接描く（オフスクリーン捕捉用）。
    fn on_print(&self, hdc_ptr: usize) {
        let hdc = unsafe { w::HDC::from_ptr(hdc_ptr as *mut std::ffi::c_void) };
        if let Ok(rc) = self.hwnd().GetClientRect() {
            let _ = self.render(&hdc, rc.right - rc.left, rc.bottom - rc.top);
        }
    }

    fn on_click(&self, pt: w::POINT) {
        let rh = self.row_h.get().max(1);
        let idx = (pt.y / rh) as usize;
        if idx < self.fields.len() {
            self.sel.set(idx);
            self.refresh();
        }
    }

    /// 選択中の色を色選択ダイアログで編集して反映する。
    fn edit_selected(&self) {
        let idx = self.sel.get();
        let Some((_, get, set)) = self.fields.get(idx) else {
            return;
        };
        let dark = self.shared.target_dark.get();
        let cur = get(&self.shared.target_colors());
        let picked = choose_color(self.hwnd(), cur);
        if picked != cur {
            {
                let mut cfg = self.shared.cfg.borrow_mut();
                let target = if dark { &mut cfg.colors.dark } else { &mut cfg.colors.light };
                set(target, picked);
            }
            self.refresh();
            (self.on_change)();
        }
    }

    /// このリストが扱う色だけを編集対象テーマの既定へ戻す。
    fn reset(&self) {
        let dark = self.shared.target_dark.get();
        let defaults = if dark { Colors::dark() } else { Colors::light() };
        {
            let mut cfg = self.shared.cfg.borrow_mut();
            let target = if dark { &mut cfg.colors.dark } else { &mut cfg.colors.light };
            for (_, get, set) in self.fields {
                set(target, get(&defaults));
            }
        }
        self.refresh();
        (self.on_change)();
    }

    fn on_paint(&self) -> w::AnyResult<()> {
        let hdc = self.hwnd().BeginPaint()?;
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        let mem = hdc.CreateCompatibleDC()?;
        let bmp = hdc.CreateCompatibleBitmap(cw, ch)?;
        let _sel = mem.SelectObject(&*bmp)?;
        self.render(&mem, cw, ch)?;
        hdc.BitBlt(
            w::POINT { x: 0, y: 0 },
            w::SIZE { cx: cw, cy: ch },
            &mem,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    fn render(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let font = w::HFONT::GetStockObject(co::STOCK_FONT::DEFAULT_GUI)?;
        let _fsel = dc.SelectObject(&font)?;
        let fh = dc.GetTextMetrics().map(|tm| tm.tmHeight).unwrap_or(16);
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;

        let pad = gui::dpi_y(5);
        let row_h = fh + pad * 2;
        self.row_h.set(row_h);
        let left = gui::dpi_x(8);

        let bg = w::GetSysColor(co::COLOR::WINDOW);
        let bg_brush = w::HBRUSH::CreateSolidBrush(bg)?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg_brush)?;

        let hl = w::GetSysColor(co::COLOR::HIGHLIGHT);
        let hl_text = w::GetSysColor(co::COLOR::HIGHLIGHTTEXT);
        let normal_text = w::GetSysColor(co::COLOR::WINDOWTEXT);
        let frame_col = from_colorref(w::GetSysColor(co::COLOR::GRAYTEXT));

        let colors = self.shared.target_colors();
        let sel = self.sel.get();
        let sw_w = gui::dpi_x(34);
        let sw_pad = gui::dpi_y(3);

        // 各色を 1 列に縦並び（フル高で全色を収める）。
        for (i, (label, get, _)) in self.fields.iter().enumerate() {
            let y = i as i32 * row_h;
            let selected = i == sel;
            if selected {
                let hl_brush = w::HBRUSH::CreateSolidBrush(hl)?;
                dc.FillRect(w::RECT { left: 0, top: y, right: cw, bottom: y + row_h }, &hl_brush)?;
            }

            // 実色のスウォッチ（枠付き）。
            let c = get(&colors);
            let sx = left;
            let sy = y + sw_pad;
            let sb = y + row_h - sw_pad;
            frame(dc, sx, sy, sx + sw_w, sb, frame_col)?;
            fill(dc, sx + 1, sy + 1, sx + sw_w - 1, sb - 1, c)?;

            // ラベルと 16 進値。16 進は固定列に描いて行をまたいで縦に揃える。
            dc.SetTextColor(if selected { hl_text } else { normal_text })?;
            let label_x = sx + sw_w + gui::dpi_x(8);
            dc.TextOut(label_x, y + pad, label)?;
            let hex = format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
            dc.TextOut(label_x + gui::dpi_x(132), y + pad, &hex)?;
        }
        Ok(())
    }
}

/// 色ページ（「配色」「テキストビューア」）を組む：説明ラベル＋変更/既定ボタン＋スウォッチ一覧。
/// 変更・リセットのボタンハンドラまで配線する（コントロールは pane が所有するので返さない）。
fn build_color_page(
    pane: &gui::WindowControl,
    shared: &Rc<Shared>,
    on_change: Rc<dyn Fn()>,
    fields: ColorFields,
) {
    label(pane, "色をダブルクリックで変更", 8, 14, 200);
    let change = gui::Button::new(
        pane,
        gui::ButtonOpts {
            text: "変更(&C)...",
            position: gui::dpi(8, 36),
            width: gui::dpi_x(110),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );
    let reset = gui::Button::new(
        pane,
        gui::ButtonOpts {
            text: "既定に戻す(&R)",
            position: gui::dpi(126, 36),
            width: gui::dpi_x(130),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );
    let swatch = SwatchList::new(pane, gui::dpi(8, 72), gui::dpi(344, 466), shared.clone(), on_change, fields);
    {
        let swatch = swatch.clone();
        change.on().bn_clicked(move || {
            swatch.edit_selected();
            Ok(())
        });
    }
    {
        let swatch = swatch.clone();
        reset.on().bn_clicked(move || {
            swatch.reset();
            Ok(())
        });
    }
}

/// セクション pane（左ナビ選択で表示を切り替える子ウィンドウ）。
fn make_pane(parent: &(impl GuiParent + 'static), pos: (i32, i32), size: (i32, i32)) -> gui::WindowControl {
    gui::WindowControl::new(
        parent,
        gui::WindowControlOpts {
            position: pos,
            size,
            class_bg_brush: gui::Brush::Color(co::COLOR::BTNFACE),
            style: co::WS::CHILD | co::WS::VISIBLE | co::WS::CLIPCHILDREN | co::WS::CLIPSIBLINGS,
            // Tab キーが pane 内の子コントロール（Edit 等）へ降りられるようにする。
            ex_style: co::WS_EX::CONTROLPARENT,
            ..Default::default()
        },
    )
}

pub(crate) fn label(parent: &(impl GuiParent + 'static), text: &str, x: i32, y: i32, cx: i32) {
    let _ = gui::Label::new(
        parent,
        gui::LabelOpts {
            text,
            position: gui::dpi(x, y),
            size: gui::dpi(cx, 18),
            ..Default::default()
        },
    );
}

/// 設定ナビの1行：ジャンル見出し（フォーカスを取らない）か、ページ（↑↓・クリックで選択可）。
enum NavRow {
    Header(&'static str),
    Page { label: &'static str, pane: usize },
}

/// 設定の左ナビ（フラット）。`pane` は中央で出すペイン番号。見出しは選択対象にならず、
/// ↑↓・クリックともページ行だけが選ばれる（見出しは飛ばす）。
const NAV_ROWS: &[NavRow] = &[
    NavRow::Header("外観"),
    NavRow::Page { label: "テーマ・フォント", pane: 0 },
    NavRow::Page { label: "配色", pane: 1 },
    NavRow::Page { label: "テキストビューア", pane: 9 },
    NavRow::Page { label: "レイアウト", pane: 2 },
    NavRow::Page { label: "一覧", pane: 7 },
    NavRow::Header("動作"),
    NavRow::Page { label: "全般", pane: 8 },
    NavRow::Page { label: "ファイル操作", pane: 12 },
    NavRow::Page { label: "カーソル", pane: 3 },
    NavRow::Page { label: "ビューア", pane: 6 },
    NavRow::Header("登録"),
    NavRow::Page { label: "ディレクトリ", pane: 4 },
    NavRow::Page { label: "メニュー", pane: 13 },
    NavRow::Header("キー"),
    NavRow::Page { label: "ファイラー", pane: 5 },
    NavRow::Page { label: "テキストビューア", pane: 10 },
    NavRow::Page { label: "画像ビューア", pane: 11 },
];

/// 選択変更時に pane 番号を渡すコールバック。
type SelectHandler = Box<dyn Fn(usize)>;

struct NavInner {
    /// 選択中ページの `NAV_ROWS` インデックス。
    sel: Cell<usize>,
    /// 1行の高さ（px・描画時にフォント高から決める）。
    row_h: Cell<i32>,
    /// 選択変更時に pane 番号を渡すコールバック。
    on_select: RefCell<Option<SelectHandler>>,
}

/// 設定の左ナビ。ジャンル見出し＋ページの平坦リストを自前描画し、見出しは選択させない。
/// これで「親ジャンルを選んだら配下の代表でない先頭ページが出る」違和感を無くす。
#[derive(Clone)]
struct SettingsNav {
    wnd: gui::WindowControl,
    inner: Rc<NavInner>,
}

impl SettingsNav {
    fn new(parent: &(impl GuiParent + 'static), pos: (i32, i32), size: (i32, i32)) -> Self {
        let wnd = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                position: pos,
                size,
                class_bg_brush: gui::Brush::Color(co::COLOR::WINDOW),
                style: co::WS::CHILD
                    | co::WS::VISIBLE
                    | co::WS::CLIPSIBLINGS
                    | co::WS::TABSTOP
                    | co::WS::BORDER,
                ..Default::default()
            },
        );
        let first = NAV_ROWS
            .iter()
            .position(|r| matches!(r, NavRow::Page { .. }))
            .unwrap_or(0);
        let me = Self {
            wnd,
            inner: Rc::new(NavInner {
                sel: Cell::new(first),
                row_h: Cell::new(gui::dpi_y(22)),
                on_select: RefCell::new(None),
            }),
        };
        me.setup_events();
        me
    }

    fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    fn on_select(&self, cb: impl Fn(usize) + 'static) {
        *self.inner.on_select.borrow_mut() = Some(Box::new(cb));
    }

    /// 現在選択中ページの pane 番号。
    fn selected_pane(&self) -> usize {
        match NAV_ROWS[self.inner.sel.get()] {
            NavRow::Page { pane, .. } => pane,
            NavRow::Header(_) => 0,
        }
    }

    /// `idx` のページを選択し（見出しなら無視）、再描画してコールバックを呼ぶ。
    fn select(&self, idx: usize) {
        let NavRow::Page { pane, .. } = NAV_ROWS[idx] else {
            return;
        };
        self.inner.sel.set(idx);
        let _ = self.hwnd().InvalidateRect(None, false);
        if let Some(cb) = self.inner.on_select.borrow().as_ref() {
            cb(pane);
        }
    }

    /// 現在選択から `dir`（+1/-1）方向の次のページへ移る（見出しは飛ばす）。
    fn move_sel(&self, dir: isize) {
        let n = NAV_ROWS.len() as isize;
        let mut i = self.inner.sel.get() as isize + dir;
        while (0..n).contains(&i) {
            if matches!(NAV_ROWS[i as usize], NavRow::Page { .. }) {
                self.select(i as usize);
                return;
            }
            i += dir;
        }
    }

    fn setup_events(&self) {
        let this = self.clone();
        self.wnd.on().wm_get_dlg_code(move |_| {
            let _ = &this;
            Ok(unsafe { co::DLGC::from_raw(co::DLGC::WANTARROWS.raw()) })
        });

        let this = self.clone();
        self.wnd.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.wnd.on().wm(unsafe { co::WM::from_raw(WM_PRINTCLIENT) }, move |p| {
            this.on_print(p.wparam);
            Ok(0)
        });

        let this = self.clone();
        self.wnd.on().wm_set_focus(move |_| {
            let _ = this.hwnd().InvalidateRect(None, false);
            Ok(())
        });
        let this = self.clone();
        self.wnd.on().wm_kill_focus(move |_| {
            let _ = this.hwnd().InvalidateRect(None, false);
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_l_button_down(move |p| {
            this.hwnd().SetFocus();
            let rh = this.inner.row_h.get().max(1);
            let row = (p.coords.y / rh) as usize;
            if row < NAV_ROWS.len() {
                this.select(row);
            }
            Ok(())
        });

        let this = self.clone();
        self.wnd.on().wm_key_down(move |p| {
            let vk = p.vkey_code.raw();
            if vk == co::VK::UP.raw() {
                this.move_sel(-1);
            } else if vk == co::VK::DOWN.raw() {
                this.move_sel(1);
            }
            Ok(())
        });
    }

    fn on_paint(&self) -> w::AnyResult<()> {
        let hdc = self.hwnd().BeginPaint()?;
        let rc = self.hwnd().GetClientRect()?;
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        if cw <= 0 || ch <= 0 {
            return Ok(());
        }
        let mem = hdc.CreateCompatibleDC()?;
        let bmp = hdc.CreateCompatibleBitmap(cw, ch)?;
        let _sel = mem.SelectObject(&*bmp)?;
        self.render(&mem, cw, ch)?;
        hdc.BitBlt(
            w::POINT { x: 0, y: 0 },
            w::SIZE { cx: cw, cy: ch },
            &mem,
            w::POINT { x: 0, y: 0 },
            co::ROP::SRCCOPY,
        )?;
        Ok(())
    }

    /// `WM_PRINTCLIENT`：与えられた DC へ直接描く（デバッグ制御サーバのスナップショット用）。
    fn on_print(&self, hdc_ptr: usize) {
        let hdc = unsafe { w::HDC::from_ptr(hdc_ptr as *mut std::ffi::c_void) };
        if let Ok(rc) = self.hwnd().GetClientRect() {
            let _ = self.render(&hdc, rc.right - rc.left, rc.bottom - rc.top);
        }
    }

    fn render(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let font = w::HFONT::GetStockObject(co::STOCK_FONT::DEFAULT_GUI)?;
        let _fsel = dc.SelectObject(&font)?;
        let fh = dc.GetTextMetrics().map(|tm| tm.tmHeight).unwrap_or(16);
        let row_h = (fh + gui::dpi_y(10)).max(gui::dpi_y(20));
        self.inner.row_h.set(row_h);
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;

        let bg = w::HBRUSH::GetSysColorBrush(co::COLOR::WINDOW)?;
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &bg)?;

        let text_col = w::GetSysColor(co::COLOR::WINDOWTEXT);
        let gray_col = w::GetSysColor(co::COLOR::GRAYTEXT);
        let hl_text = w::GetSysColor(co::COLOR::HIGHLIGHTTEXT);
        let hl_bg = w::HBRUSH::GetSysColorBrush(co::COLOR::HIGHLIGHT)?;

        let sel = self.inner.sel.get();
        let header_x = gui::dpi_x(6);
        let page_x = gui::dpi_x(22);
        for (i, row) in NAV_ROWS.iter().enumerate() {
            let y = i as i32 * row_h;
            let ty = y + (row_h - fh) / 2;
            match row {
                NavRow::Header(name) => {
                    dc.SetTextColor(gray_col)?;
                    dc.TextOut(header_x, ty, name)?;
                }
                NavRow::Page { label, .. } => {
                    if i == sel {
                        dc.FillRect(
                            w::RECT { left: 0, top: y, right: cw, bottom: y + row_h },
                            &hl_bg,
                        )?;
                        dc.SetTextColor(hl_text)?;
                    } else {
                        dc.SetTextColor(text_col)?;
                    }
                    dc.TextOut(page_x, ty, label)?;
                }
            }
        }
        Ok(())
    }
}

/// 「外観」ページ（テーマ・フォント）を構築する。すべての編集を即 `Shared` へ反映する。
/// イベントは親 window 側に保持されるため、生成したコントロールは保持しなくてよい。
fn build_appearance(parent: &gui::WindowControl, shared: &Rc<Shared>, preview: &Preview) {
    let cfg = shared.cfg.borrow();
    group_box(parent, "配色テーマ", 12, 8, 344, 96);
    let theme = gui::RadioGroup::new(
        parent,
        &[
            gui::RadioButtonOpts {
                text: "ダーク(&D)",
                position: gui::dpi(28, 32),
                size: gui::dpi(300, 20),
                selected: cfg.theme == Theme::Dark,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "ライト(&L)",
                position: gui::dpi(28, 56),
                size: gui::dpi(300, 20),
                selected: cfg.theme == Theme::Light,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "システムに従う(&S)",
                position: gui::dpi(28, 80),
                size: gui::dpi(300, 20),
                selected: cfg.theme == Theme::System,
                ..Default::default()
            },
        ],
    );

    group_box(parent, "フォント", 12, 112, 344, 116);
    label(parent, "フォント名", 24, 138, 90);
    let font_family = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &cfg.font.family,
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(108, 136),
            width: gui::dpi_x(232),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    label(parent, "サイズ", 24, 170, 90);
    let font_size = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &cfg.font.size.to_string(),
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(108, 168),
            width: gui::dpi_x(60),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let font_btn = gui::Button::new(
        parent,
        gui::ButtonOpts {
            text: "フォント選択(&F)...",
            position: gui::dpi(108, 196),
            width: gui::dpi_x(150),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    );

    // アイコン（表示の有無とサイズ）。フォントの下に縦に積む（右側はプレビューが占有）。
    group_box(parent, "アイコン", 12, 236, 344, 246);
    let icon_show = gui::CheckBox::new(
        parent,
        gui::CheckBoxOpts {
            text: "ファイル一覧にアイコンを表示する(&I)",
            position: gui::dpi(24, 264),
            size: gui::dpi(320, 22),
            check_state: if cfg.icons.show { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        },
    );
    label(parent, "サイズ", 24, 302, 100);
    let icon_size = gui::RadioGroup::new(
        parent,
        &[
            gui::RadioButtonOpts {
                text: "自動（行に合わせる）(&U)",
                position: gui::dpi(44, 326),
                size: gui::dpi(290, 20),
                selected: cfg.icons.size == IconSize::Auto,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "小 (16)(&A)",
                position: gui::dpi(44, 350),
                size: gui::dpi(290, 20),
                selected: cfg.icons.size == IconSize::Small,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "中 (24)(&M)",
                position: gui::dpi(44, 374),
                size: gui::dpi(290, 20),
                selected: cfg.icons.size == IconSize::Medium,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "大 (32)(&G)",
                position: gui::dpi(44, 398),
                size: gui::dpi(290, 20),
                selected: cfg.icons.size == IconSize::Large,
                ..Default::default()
            },
        ],
    );
    label(parent, "（中・大を選ぶと行の高さが広がります）", 24, 424, 320);
    label(parent, "サムネイルモード時のサイズ", 24, 452, 180);
    let thumb_size = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &cfg.icons.thumbnail_size.to_string(),
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(210, 450),
            width: gui::dpi_x(44),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let _thumb_spin = gui::UpDown::new(
        parent,
        gui::UpDownOpts {
            position: gui::dpi(254, 450),
            height: gui::dpi_y(22),
            range: (16, 256),
            value: cfg.icons.thumbnail_size,
            control_style: co::UDS::AUTOBUDDY
                | co::UDS::SETBUDDYINT
                | co::UDS::ALIGNRIGHT
                | co::UDS::ARROWKEYS,
            ..Default::default()
        },
    );
    label(parent, "px", 280, 452, 24);
    drop(cfg);

    // テーマ選択を即反映し、プレビュー／配色編集の対象サイドもこれに追従させる。
    {
        let shared = shared.clone();
        let theme2 = theme.clone();
        let preview = preview.clone();
        theme.on().bn_clicked(move || {
            let dark = {
                let mut cfg = shared.cfg.borrow_mut();
                cfg.theme = match theme2.selected_index() {
                    Some(0) => Theme::Dark,
                    Some(1) => Theme::Light,
                    _ => Theme::System,
                };
                match cfg.theme {
                    Theme::Dark => true,
                    Theme::Light => false,
                    Theme::System => cfg.resolved == ResolvedTheme::Dark,
                }
            };
            shared.target_dark.set(dark);
            preview.refresh();
            Ok(())
        });
    }

    // フォント編集はその場でプレビューへ反映する。
    {
        let shared = shared.clone();
        let preview = preview.clone();
        let edit = font_family.clone();
        font_family.on().en_change(move || {
            if let Ok(f) = edit.text() {
                let f = f.trim();
                if !f.is_empty() {
                    shared.cfg.borrow_mut().font.family = f.to_owned();
                    preview.refresh();
                }
            }
            Ok(())
        });
    }
    {
        let shared = shared.clone();
        let preview = preview.clone();
        let edit = font_size.clone();
        font_size.on().en_change(move || {
            let cur = shared.cfg.borrow().font.size;
            let size = parse_or(&edit, cur).clamp(6, 72);
            shared.cfg.borrow_mut().font.size = size;
            preview.refresh();
            Ok(())
        });
    }

    // フォント選択ダイアログ。選んだ値を Edit へ書き戻すと en_change が cfg・プレビューへ反映する。
    {
        let shared = shared.clone();
        let ff = font_family.clone();
        let fs = font_size.clone();
        let btn = font_btn.clone();
        font_btn.on().bn_clicked(move || {
            let (family, size) = {
                let cfg = shared.cfg.borrow();
                (cfg.font.family.clone(), cfg.font.size)
            };
            if let Some((new_family, new_size)) = choose_font(btn.hwnd(), &family, size) {
                let _ = ff.hwnd().SetWindowText(&new_family);
                let _ = fs.hwnd().SetWindowText(&new_size.to_string());
            }
            Ok(())
        });
    }

    // アイコン表示の有無・サイズを cfg へ反映し、プレビューへも反映する。
    {
        let shared = shared.clone();
        let check = icon_show.clone();
        let preview = preview.clone();
        icon_show.on().bn_clicked(move || {
            shared.cfg.borrow_mut().icons.show = check.is_checked();
            preview.refresh();
            Ok(())
        });
    }
    {
        let shared = shared.clone();
        let group = icon_size.clone();
        let preview = preview.clone();
        icon_size.on().bn_clicked(move || {
            shared.cfg.borrow_mut().icons.size = match group.selected_index() {
                Some(1) => IconSize::Small,
                Some(2) => IconSize::Medium,
                Some(3) => IconSize::Large,
                _ => IconSize::Auto,
            };
            preview.refresh();
            Ok(())
        });
    }
    {
        let shared = shared.clone();
        let edit = thumb_size.clone();
        thumb_size.on().en_change(move || {
            let cur = shared.cfg.borrow().icons.thumbnail_size;
            let v = parse_or(&edit, cur).clamp(16, 256);
            shared.cfg.borrow_mut().icons.thumbnail_size = v;
            Ok(())
        });
    }
}

/// 「レイアウト」ページ（寸法を数値で編集）を構築する。各 Edit を即 `Shared` へ反映する。
fn build_layout(parent: &gui::WindowControl, shared: &Rc<Shared>, preview: &Preview) {
    let cfg = shared.cfg.borrow();
    let mut edits = Vec::with_capacity(LAYOUT_FIELDS.len());
    for (i, (lbl, get, _)) in LAYOUT_FIELDS.iter().enumerate() {
        let y = 12 + i as i32 * 34;
        label(parent, lbl, 16, y + 2, 150);
        let edit = gui::Edit::new(
            parent,
            gui::EditOpts {
                text: &get(&cfg.layout).to_string(),
                control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
                position: gui::dpi(170, y),
                width: gui::dpi_x(64),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        // 直前の Edit にバディ付けした上下スピン（クリック・矢印キーで増減）。
        let _spin = gui::UpDown::new(
            parent,
            gui::UpDownOpts {
                position: gui::dpi(234, y),
                height: gui::dpi_y(22),
                range: (0, 4000),
                value: get(&cfg.layout),
                control_style: co::UDS::AUTOBUDDY
                    | co::UDS::SETBUDDYINT
                    | co::UDS::ALIGNRIGHT
                    | co::UDS::ARROWKEYS,
                ..Default::default()
            },
        );
        edits.push(edit);
    }
    drop(cfg);

    // 各寸法の編集を即プレビューへ反映する。
    for (edit, (_, get, set)) in edits.iter().zip(LAYOUT_FIELDS) {
        let shared = shared.clone();
        let preview = preview.clone();
        let edit2 = edit.clone();
        let get = *get;
        let set = *set;
        edit.on().en_change(move || {
            let cur = get(&shared.cfg.borrow().layout);
            let v = parse_or(&edit2, cur).max(0);
            set(&mut shared.cfg.borrow_mut().layout, v);
            preview.refresh();
            Ok(())
        });
    }

    // 既定ウィンドウサイズ（毎回このサイズで起動するか）。
    let (fixed_size, win_w, win_h) = {
        let c = shared.cfg.borrow();
        (c.window.fixed_size, c.window.width, c.window.height)
    };
    let fixed_check = gui::CheckBox::new(
        parent,
        gui::CheckBoxOpts {
            text: "毎回既定サイズで起動する(&S)",
            position: gui::dpi(16, 426),
            size: gui::dpi(300, 22),
            check_state: if fixed_size { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        },
    );
    label(parent, "幅", 36, 460, 32);
    let w_edit = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &win_w.to_string(),
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(72, 458),
            width: gui::dpi_x(60),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    label(parent, "高さ", 148, 460, 36);
    let h_edit = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &win_h.to_string(),
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(192, 458),
            width: gui::dpi_x(60),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let _ = w_edit.hwnd().EnableWindow(fixed_size);
    let _ = h_edit.hwnd().EnableWindow(fixed_size);
    {
        let shared = shared.clone();
        let fc = fixed_check.clone();
        let we = w_edit.clone();
        let he = h_edit.clone();
        fixed_check.on().bn_clicked(move || {
            let on = fc.is_checked();
            shared.cfg.borrow_mut().window.fixed_size = on;
            let _ = we.hwnd().EnableWindow(on);
            let _ = he.hwnd().EnableWindow(on);
            Ok(())
        });
    }
    {
        let shared = shared.clone();
        let we = w_edit.clone();
        w_edit.on().en_change(move || {
            let cur = shared.cfg.borrow().window.width;
            let v = parse_or(&we, cur).max(1);
            shared.cfg.borrow_mut().window.width = v;
            Ok(())
        });
    }
    {
        let shared = shared.clone();
        let he = h_edit.clone();
        h_edit.on().en_change(move || {
            let cur = shared.cfg.borrow().window.height;
            let v = parse_or(&he, cur).max(1);
            shared.cfg.borrow_mut().window.height = v;
            Ok(())
        });
    }
}

/// 「動作」ページ（全般の動作設定）。今は待機表示の遅延のみ。操作を即 `Shared` へ反映する。
fn build_behavior(parent: &gui::WindowControl, shared: &Rc<Shared>) {
    let delay_ms = shared.cfg.borrow().progress_delay_ms;
    label(parent, "「読込中」表示までの時間(ms)", 16, 18, 240);
    let delay_edit = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &delay_ms.to_string(),
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(262, 16),
            width: gui::dpi_x(64),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let _delay_spin = gui::UpDown::new(
        parent,
        gui::UpDownOpts {
            position: gui::dpi(326, 16),
            height: gui::dpi_y(22),
            range: (0, 10000),
            value: delay_ms.min(10000) as i32,
            control_style: co::UDS::AUTOBUDDY
                | co::UDS::SETBUDDYINT
                | co::UDS::ALIGNRIGHT
                | co::UDS::ARROWKEYS,
            ..Default::default()
        },
    );
    label(
        parent,
        "（読込・展開がこれより長くかかるとき「読込中」を出します。0 で即時）",
        16,
        46,
        560,
    );
    {
        let shared = shared.clone();
        let delay_edit2 = delay_edit.clone();
        delay_edit.on().en_change(move || {
            let cur = shared.cfg.borrow().progress_delay_ms as i32;
            let v = parse_or(&delay_edit2, cur).clamp(0, 10000);
            shared.cfg.borrow_mut().progress_delay_ms = v as u64;
            Ok(())
        });
    }

    label(parent, "既定エディタ", 16, 86, 110);
    let editor_init = shared.cfg.borrow().editor.clone();
    let editor_edit = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &editor_init,
            control_style: co::ES::AUTOHSCROLL,
            position: gui::dpi(130, 84),
            width: gui::dpi_x(360),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let editor_browse = gui::Button::new(
        parent,
        gui::ButtonOpts {
            text: "参照...",
            position: gui::dpi(498, 83),
            width: gui::dpi_x(64),
            height: gui::dpi_y(24),
            ..Default::default()
        },
    );
    label(
        parent,
        "（ファイルを外部エディタで開く操作で使うプログラム。空にすると無効）",
        16,
        114,
        560,
    );
    {
        let shared = shared.clone();
        let editor_edit2 = editor_edit.clone();
        editor_edit.on().en_change(move || {
            shared.cfg.borrow_mut().editor = editor_edit2.text().unwrap_or_default();
            Ok(())
        });
    }
    {
        let parent_hwnd = parent.hwnd().ptr();
        let ee = editor_edit.clone();
        editor_browse.on().bn_clicked(move || {
            if let Some(p) = crate::shell::choose_file(parent_hwnd, "既定エディタの選択", false) {
                let _ = ee.set_text(&p.to_string_lossy());
            }
            Ok(())
        });
    }
}

/// 「カーソル」ページ（位置記憶のオン/オフと履歴件数の上限）を構築する。各操作を即 `Shared` へ反映する。
fn build_cursor(parent: &gui::WindowControl, shared: &Rc<Shared>) {
    let (history, count) = {
        let cfg = shared.cfg.borrow();
        (cfg.cursor.history, cfg.cursor.history_count)
    };
    let check = gui::CheckBox::new(
        parent,
        gui::CheckBoxOpts {
            text: "カーソル位置を記憶する(&M)",
            position: gui::dpi(16, 16),
            size: gui::dpi(280, 22),
            check_state: if history { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        },
    );
    label(parent, "履歴件数の上限", 36, 52, 110);
    let count_edit = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &count.to_string(),
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(150, 50),
            width: gui::dpi_x(64),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    label(parent, "（ディレクトリを再訪したとき、前回のカーソル位置へ戻します）", 16, 92, 340);
    let _ = count_edit.hwnd().EnableWindow(history);

    // オン/オフ：cfg へ反映し、件数 Edit の有効/無効を連動させる。
    {
        let shared = shared.clone();
        let check2 = check.clone();
        let count_edit = count_edit.clone();
        check.on().bn_clicked(move || {
            let on = check2.is_checked();
            shared.cfg.borrow_mut().cursor.history = on;
            let _ = count_edit.hwnd().EnableWindow(on);
            Ok(())
        });
    }
    // 件数：1 以上にクランプして反映。
    {
        let shared = shared.clone();
        let edit = count_edit.clone();
        count_edit.on().en_change(move || {
            let cur = shared.cfg.borrow().cursor.history_count as i32;
            let v = parse_or(&edit, cur).max(1);
            shared.cfg.borrow_mut().cursor.history_count = v as usize;
            Ok(())
        });
    }

    // 左右カーソルキーで親移動（原作 CursorToParent・既定オフ）。
    let to_parent = shared.cfg.borrow().cursor.to_parent;
    let parent_check = gui::CheckBox::new(
        parent,
        gui::CheckBoxOpts {
            text: "左右カーソルキーで親ディレクトリへ移動する(&P)",
            position: gui::dpi(16, 124),
            size: gui::dpi(320, 22),
            check_state: if to_parent { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        },
    );
    label(parent, "（アクティブ側で外向きのカーソルキー：左ペインで←／右ペインで→ が親移動になります）", 16, 152, 360);
    {
        let shared = shared.clone();
        let pc = parent_check.clone();
        parent_check.on().bn_clicked(move || {
            shared.cfg.borrow_mut().cursor.to_parent = pc.is_checked();
            Ok(())
        });
    }

    // マーク反転後のカーソル下移動（原作 DownAfterSelect・既定オン）。
    let down_after = shared.cfg.borrow().cursor.down_after_select;
    let down_check = gui::CheckBox::new(
        parent,
        gui::CheckBoxOpts {
            text: "マーク反転後にカーソルを下へ移動する(&D)",
            position: gui::dpi(16, 184),
            size: gui::dpi(320, 22),
            check_state: if down_after { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        },
    );
    label(parent, "（Space でマークを反転したあと、自動でカーソルが1つ下へ進みます）", 16, 212, 360);
    {
        let shared = shared.clone();
        let dc = down_check.clone();
        down_check.on().bn_clicked(move || {
            shared.cfg.borrow_mut().cursor.down_after_select = dc.is_checked();
            Ok(())
        });
    }
}

/// 「ファイル操作」ページの1行（ラベル・Y 位置・初期値・反映先セッター）。
type FileOpRow = (&'static str, i32, bool, fn(&mut FileOpSettings, bool));

/// 「ファイル操作」ページ（コピー/移動/削除前の確認ダイアログ）。各操作を即 `Shared` へ反映する。
fn build_fileops(parent: &gui::WindowControl, shared: &Rc<Shared>) {
    let f = shared.cfg.borrow().file_ops;
    let rows: [FileOpRow; 6] = [
        ("コピーの前に確認する(&C)", 16, f.ask_before_copy, |s, v| s.ask_before_copy = v),
        ("移動の前に確認する(&M)", 50, f.ask_before_move, |s, v| s.ask_before_move = v),
        ("削除・ゴミ箱送りの前に確認する(&D)", 84, f.ask_before_delete, |s, v| s.ask_before_delete = v),
        ("書庫の展開時に書庫名のフォルダを作る(&E)", 118, f.extract_create_directory, |s, v| s.extract_create_directory = v),
        ("ディレクトリのコピー時に属性も複製する(&R)", 162, f.copy_attribute, |s, v| s.copy_attribute = v),
        ("ディレクトリのコピー時に作成・更新日時も複製する(&I)", 196, f.copy_date, |s, v| s.copy_date = v),
    ];
    for (text, y, init, set) in rows {
        let check = gui::CheckBox::new(
            parent,
            gui::CheckBoxOpts {
                text,
                position: gui::dpi(16, y),
                size: gui::dpi(360, 22),
                check_state: if init { co::BST::CHECKED } else { co::BST::UNCHECKED },
                ..Default::default()
            },
        );
        let shared = shared.clone();
        let c = check.clone();
        check.on().bn_clicked(move || {
            set(&mut shared.cfg.borrow_mut().file_ops, c.is_checked());
            Ok(())
        });
    }
    label(parent, "（確認をオフにした操作は確認なしで即実行します）", 36, 144, 400);
}

/// ラベル付きのグループ枠を置く（Win32 の BS_GROUPBOX ボタン）。
fn group_box(parent: &(impl GuiParent + 'static), text: &str, x: i32, y: i32, cx: i32, cy: i32) {
    let _ = gui::Button::new(
        parent,
        gui::ButtonOpts {
            text,
            control_style: co::BS::GROUPBOX,
            position: gui::dpi(x, y),
            width: gui::dpi_x(cx),
            height: gui::dpi_y(cy),
            ..Default::default()
        },
    );
}

/// 「ビューア」ページ。画像／テキストでセクション分けし、各設定を即 `Shared` へ反映する。
/// テキストセクションは枠のみ（設定項目は今後ここへ追加していく）。
fn build_viewer(parent: &gui::WindowControl, shared: &Rc<Shared>) {
    let wheel = shared.cfg.borrow().image.wheel;

    // 画像セクション。
    group_box(parent, "画像", 12, 8, 752, 100);
    label(parent, "マウスホイール", 28, 38, 110);
    let group = gui::RadioGroup::new(
        parent,
        &[
            gui::RadioButtonOpts {
                text: "前後送り(&N)",
                position: gui::dpi(142, 36),
                size: gui::dpi(104, 20),
                selected: wheel == WheelAction::Navigate,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "拡大／縮小(&Z)",
                position: gui::dpi(248, 36),
                size: gui::dpi(118, 20),
                selected: wheel == WheelAction::Zoom,
                ..Default::default()
            },
        ],
    );
    {
        let shared = shared.clone();
        let group2 = group.clone();
        group.on().bn_clicked(move || {
            shared.cfg.borrow_mut().image.wheel = match group2.selected_index() {
                Some(1) => WheelAction::Zoom,
                _ => WheelAction::Navigate,
            };
            Ok(())
        });
    }

    label(parent, "ズーム増減（%）", 28, 62, 110);
    let zoom_step = shared.cfg.borrow().image.zoom_step_percent;
    let zoom_edit = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &zoom_step.to_string(),
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(142, 60),
            width: gui::dpi_x(60),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let _zoom_spin = gui::UpDown::new(
        parent,
        gui::UpDownOpts {
            position: gui::dpi(202, 60),
            height: gui::dpi_y(22),
            range: (1, 400),
            value: zoom_step.clamp(1, 400) as i32,
            control_style: co::UDS::AUTOBUDDY
                | co::UDS::SETBUDDYINT
                | co::UDS::ALIGNRIGHT
                | co::UDS::ARROWKEYS,
            ..Default::default()
        },
    );
    label(parent, "（1 段あたりの拡大率。例 25 で 1.25 倍ずつ）", 268, 62, 280);
    {
        let shared = shared.clone();
        let ze = zoom_edit.clone();
        zoom_edit.on().en_change(move || {
            let cur = shared.cfg.borrow().image.zoom_step_percent as i32;
            let v = parse_or(&ze, cur).clamp(1, 400);
            shared.cfg.borrow_mut().image.zoom_step_percent = v as u32;
            Ok(())
        });
    }

    label(parent, "パン移動量（px）", 28, 86, 110);
    let pan_step = shared.cfg.borrow().image.pan_step_px;
    let pan_edit = gui::Edit::new(
        parent,
        gui::EditOpts {
            text: &pan_step.to_string(),
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(142, 84),
            width: gui::dpi_x(60),
            height: gui::dpi_y(22),
            ..Default::default()
        },
    );
    let _pan_spin = gui::UpDown::new(
        parent,
        gui::UpDownOpts {
            position: gui::dpi(202, 84),
            height: gui::dpi_y(22),
            range: (1, 2000),
            value: pan_step.clamp(1, 2000) as i32,
            control_style: co::UDS::AUTOBUDDY
                | co::UDS::SETBUDDYINT
                | co::UDS::ALIGNRIGHT
                | co::UDS::ARROWKEYS,
            ..Default::default()
        },
    );
    label(parent, "（Ctrl＋矢印での1回あたりの移動画素数）", 268, 86, 280);
    {
        let shared = shared.clone();
        let pe = pan_edit.clone();
        pan_edit.on().en_change(move || {
            let cur = shared.cfg.borrow().image.pan_step_px as i32;
            let v = parse_or(&pe, cur).clamp(1, 2000);
            shared.cfg.borrow_mut().image.pan_step_px = v as u32;
            Ok(())
        });
    }

    // テキストセクション（設定項目は今後追加）。
    group_box(parent, "テキスト", 12, 120, 752, 76);
}

/// 「一覧」ページ。ファイルサイズ列の表記スタイルを選ぶ（列構成・既定ソートは今後ここへ追加）。
fn build_list(parent: &gui::WindowControl, shared: &Rc<Shared>) {
    let fmt = shared.cfg.borrow().size_format;

    group_box(parent, "ファイルサイズの表記", 270, 8, 494, 158);
    label(parent, "サイズ列の表示形式", 286, 34, 200);
    let group = gui::RadioGroup::new(
        parent,
        &[
            gui::RadioButtonOpts {
                text: "詳細：全バイトをカンマ区切り（例 1,234,567）(&D)",
                position: gui::dpi(286, 58),
                size: gui::dpi(440, 20),
                selected: fmt == SizeFormat::Detail,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "省略：小はバイト・大は単位（例 1.2 MB）(&M)",
                position: gui::dpi(286, 82),
                size: gui::dpi(440, 20),
                selected: fmt == SizeFormat::Simple2,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "省略：常に単位＋小数1桁（例 500.0 KB）(&U)",
                position: gui::dpi(286, 106),
                size: gui::dpi(440, 20),
                selected: fmt == SizeFormat::Simple1,
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "KB 固定：エクスプローラ風（例 1,229 KB）(&K)",
                position: gui::dpi(286, 130),
                size: gui::dpi(440, 20),
                selected: fmt == SizeFormat::Explorer,
                ..Default::default()
            },
        ],
    );
    {
        let shared = shared.clone();
        let group2 = group.clone();
        group.on().bn_clicked(move || {
            shared.cfg.borrow_mut().size_format = match group2.selected_index() {
                Some(1) => SizeFormat::Simple2,
                Some(2) => SizeFormat::Simple1,
                Some(3) => SizeFormat::Explorer,
                _ => SizeFormat::Detail,
            };
            Ok(())
        });
    }
}

/// 列エディタで選べる種類と説明ラベル（リスト表示順）。Icon/Information は名前列内包のため無し。
const COLUMN_KINDS: &[(ColumnKind, &str)] = &[
    (ColumnKind::FileName, "ファイル名（拡張子込み）"),
    (ColumnKind::FileBaseName, "ファイル名（拡張子なし）"),
    (ColumnKind::FileExtension, "種類（拡張子）"),
    (ColumnKind::Length, "サイズ"),
    (ColumnKind::LastWriteTime, "更新日時（4桁年）"),
    (ColumnKind::LastWriteTimeS, "更新日時（2桁年）"),
    (ColumnKind::CreateTime, "作成日時（4桁年）"),
    (ColumnKind::CreateTimeS, "作成日時（2桁年）"),
    (ColumnKind::Attribute, "属性"),
];

/// 種類の説明ラベル。
fn kind_label(kind: ColumnKind) -> &'static str {
    COLUMN_KINDS.iter().find(|(k, _)| *k == kind).map(|(_, l)| *l).unwrap_or("?")
}

/// 既定ソートの種別ラジオ／互換チェックの状態から `default_sort` を組み直す。
/// 種別・表記は S キーの「ソート」ダイアログ（[`crate::dialog::SORT_KINDS`]）と共有する。
fn apply_default_sort(shared: &Rc<Shared>, kinds: &gui::RadioGroup, explike: &gui::CheckBox) {
    let base = kinds
        .selected_index()
        .and_then(|i| crate::dialog::SORT_KINDS.get(i))
        .map(|(_, t)| *t)
        .unwrap_or(SortType::FileName);
    shared.cfg.borrow_mut().default_sort = SortType::with_explike(base, explike.is_checked());
}

/// ラベル付きボタンを置く。
fn button(parent: &(impl GuiParent + 'static), text: &str, x: i32, y: i32, w: i32) -> gui::Button {
    gui::Button::new(
        parent,
        gui::ButtonOpts {
            text,
            position: gui::dpi(x, y),
            width: gui::dpi_x(w),
            height: gui::dpi_y(26),
            ..Default::default()
        },
    )
}

/// 「一覧」ページの編集部（既定ソート・列の種類/幅/順序・自動調整トグル）。
#[derive(Clone)]
struct ColumnsEditor {
    /// 表示中の列（種類・幅）。
    shown: gui::ListView<()>,
    /// 使用可能な列（全種類・重複可）。
    available: gui::ListBox,
    /// 文字間隔スピナーと初期値。winsafe は value==0 だと生成時に位置を設定しないため、
    /// 生成後の populate で明示的に合わせる（既定 0 が範囲下限で表示されるのを防ぐ）。
    spacing_spin: gui::UpDown,
    spacing_init: i32,
    rebuild: Rc<dyn Fn(Option<usize>)>,
}

impl ColumnsEditor {
    fn new(parent: &gui::WindowControl, shared: &Rc<Shared>) -> Self {
        // 既定の並び順（state が無い初回起動時に使う）。
        group_box(parent, "既定の並び順", 12, 8, 250, 158);
        // 種別（2列）＋自然順＋降順。S キーの「ソート」ダイアログと項目・表記を揃える。
        let (init_kind, init_exp) = shared.cfg.borrow().default_sort.split_explike();
        let sort_kinds = gui::RadioGroup::new(
            parent,
            &crate::dialog::SORT_KINDS
                .iter()
                .enumerate()
                .map(|(i, (label, ty))| gui::RadioButtonOpts {
                    text: label,
                    position: gui::dpi(24 + (i as i32 % 2) * 114, 30 + (i as i32 / 2) * 24),
                    size: gui::dpi(110, 20),
                    selected: *ty == init_kind,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        );
        let sort_explike = gui::CheckBox::new(
            parent,
            gui::CheckBoxOpts {
                text: "自然順(&X)",
                position: gui::dpi(24, 108),
                size: gui::dpi(220, 18),
                check_state: if init_exp { co::BST::CHECKED } else { co::BST::UNCHECKED },
                ..Default::default()
            },
        );
        let sort_reverse = gui::CheckBox::new(
            parent,
            gui::CheckBoxOpts {
                text: "降順(&R)",
                position: gui::dpi(24, 130),
                size: gui::dpi(220, 18),
                check_state: if shared.cfg.borrow().default_sort_reverse {
                    co::BST::CHECKED
                } else {
                    co::BST::UNCHECKED
                },
                ..Default::default()
            },
        );
        {
            let shared = shared.clone();
            let kinds = sort_kinds.clone();
            let explike = sort_explike.clone();
            sort_kinds.on().bn_clicked(move || {
                apply_default_sort(&shared, &kinds, &explike);
                Ok(())
            });
        }
        {
            let shared = shared.clone();
            let kinds = sort_kinds.clone();
            let explike = sort_explike.clone();
            sort_explike.on().bn_clicked(move || {
                apply_default_sort(&shared, &kinds, &explike);
                Ok(())
            });
        }
        {
            let shared = shared.clone();
            let rev = sort_reverse.clone();
            sort_reverse.on().bn_clicked(move || {
                shared.cfg.borrow_mut().default_sort_reverse = rev.is_checked();
                Ok(())
            });
        }

        group_box(parent, "ファイル一覧の列構成", 12, 174, 752, 360);

        let auto = shared.cfg.borrow().auto_adjust_columns;
        let auto_check = gui::CheckBox::new(
            parent,
            gui::CheckBoxOpts {
                text: "列幅を自動で内容に合わせる（オフで指定した幅をそのまま使う）(&W)",
                position: gui::dpi(24, 198),
                size: gui::dpi(470, 22),
                check_state: if auto { co::BST::CHECKED } else { co::BST::UNCHECKED },
                ..Default::default()
            },
        );

        // 文字間隔（自動調整トグルと同じ行の右側）。負で詰める＝マイナス入力を許すため
        // ES::NUMBER は付けない。
        label(parent, "文字間隔（px・負で詰める）", 512, 200, 170);
        let spacing = shared.cfg.borrow().char_spacing_px;
        let spacing_edit = gui::Edit::new(
            parent,
            gui::EditOpts {
                text: &spacing.to_string(),
                control_style: co::ES::AUTOHSCROLL,
                position: gui::dpi(686, 198),
                width: gui::dpi_x(44),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        let spacing_spin = gui::UpDown::new(
            parent,
            gui::UpDownOpts {
                position: gui::dpi(730, 198),
                height: gui::dpi_y(22),
                range: (-20, 20),
                value: spacing.clamp(-20, 20),
                control_style: co::UDS::AUTOBUDDY
                    | co::UDS::SETBUDDYINT
                    | co::UDS::ALIGNRIGHT
                    | co::UDS::ARROWKEYS,
                ..Default::default()
            },
        );
        {
            let shared = shared.clone();
            let se = spacing_edit.clone();
            spacing_edit.on().en_change(move || {
                let cur = shared.cfg.borrow().char_spacing_px;
                let v = parse_or(&se, cur).clamp(-20, 20);
                shared.cfg.borrow_mut().char_spacing_px = v;
                Ok(())
            });
        }

        // 左：使用可能な列（全種類・重複可）。
        label(parent, "使用可能な列", 24, 228, 200);
        let available = gui::ListBox::new(
            parent,
            gui::ListBoxOpts {
                position: gui::dpi(24, 250),
                size: gui::dpi(230, 248),
                ..Default::default()
            },
        );

        // 中央：←→ で出し入れ。
        let to_shown = button(parent, "追加 →", 268, 300, 96);
        let to_avail = button(parent, "← 削除", 268, 340, 96);

        // 右：表示中の列（順番どおりに表示される）。
        label(parent, "表示中の列", 376, 228, 200);
        let shown = gui::ListView::<()>::new(
            parent,
            gui::ListViewOpts {
                position: gui::dpi(376, 250),
                size: gui::dpi(376, 218),
                control_style: co::LVS::REPORT
                    | co::LVS::NOSORTHEADER
                    | co::LVS::SHOWSELALWAYS
                    | co::LVS::SINGLESEL,
                control_ex_style: co::LVS_EX::FULLROWSELECT,
                ..Default::default()
            },
        );
        label(parent, "幅", 376, 478, 24);
        let width_edit = gui::Edit::new(
            parent,
            gui::EditOpts {
                control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
                position: gui::dpi(400, 476),
                width: gui::dpi_x(60),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        label(parent, "（行をダブルクリックで幅編集）", 470, 478, 220);
        let fwd = button(parent, "列を手前へ", 376, 506, 116);
        let back = button(parent, "列を後ろへ", 498, 506, 116);

        let selected: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        // プログラム的な幅入力更新中フラグ（en_change の再入で cfg を二重借用しないよう抑制）。
        let editing: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        // 表示中リストを cfg.columns から組み直す。
        let rebuild: Rc<dyn Fn(Option<usize>)> = Rc::new({
            let shown = shown.clone();
            let shared = shared.clone();
            let selected = selected.clone();
            move |sel| {
                let _ = shown.items().delete_all();
                for c in shared.cfg.borrow().columns.iter() {
                    let _ = shown.items().add(
                        &[kind_label(c.kind).to_owned(), c.width.to_string()],
                        None,
                        (),
                    );
                }
                if let Some(i) = sel
                    && let Some(it) = shown.items().iter().nth(i) {
                        let _ = it.select(true);
                        let _ = it.focus();
                    }
                selected.set(sel);
            }
        });

        // 表示中の選択 → 幅入力欄へ反映。
        {
            let shown2 = shown.clone();
            let shared = shared.clone();
            let we = width_edit.clone();
            let selected = selected.clone();
            let editing = editing.clone();
            shown.on().lvn_item_changed(move |_| {
                if let Some(i) = shown2.items().iter().position(|it| it.is_selected()) {
                    // 借用を手放してから set_text（en_change の再入で borrow_mut しても安全に）。
                    let w = shared.cfg.borrow().columns.get(i).map(|c| c.width);
                    selected.set(Some(i));
                    if let Some(w) = w {
                        editing.set(true);
                        let _ = we.set_text(&w.to_string());
                        editing.set(false);
                    }
                }
                Ok(())
            });
        }

        // ダブルクリック → 幅入力へフォーカス（その場で幅編集）。
        {
            let we = width_edit.clone();
            shown.on().nm_dbl_clk(move |_| {
                let _ = we.hwnd().SetFocus();
                we.set_selection(0, -1); // すぐ上書きできるよう全選択。
                Ok(())
            });
        }

        // 幅入力の変更を選択行へ即反映（16〜1000 にクランプ）。
        {
            let shared = shared.clone();
            let we = width_edit.clone();
            let shown2 = shown.clone();
            let selected = selected.clone();
            let editing = editing.clone();
            width_edit.on().en_change(move || {
                if editing.get() {
                    return Ok(());
                }
                let Some(i) = selected.get() else { return Ok(()) };
                let w = we.text().unwrap_or_default().trim().parse::<i32>().unwrap_or(0);
                if (16..=1000).contains(&w) {
                    if let Some(c) = shared.cfg.borrow_mut().columns.get_mut(i) {
                        c.width = w;
                    }
                    if let Some(it) = shown2.items().iter().nth(i) {
                        let _ = it.set_text(1, &w.to_string());
                    }
                }
                Ok(())
            });
        }

        // → 使用可能で選択中の種類を表示中の末尾へ追加。
        {
            let shared = shared.clone();
            let rebuild = rebuild.clone();
            let available = available.clone();
            to_shown.on().bn_clicked(move || {
                if let Some(ki) = unsafe { available.hwnd().SendMessage(lb::GetCurSel {}) }
                    && let Some((kind, _)) = COLUMN_KINDS.get(ki as usize) {
                        let col = Column {
                            kind: *kind,
                            text: kind.header_label().to_owned(),
                            width: 100,
                            align: kind.default_align(),
                        };
                        let idx = {
                            let mut cfg = shared.cfg.borrow_mut();
                            cfg.columns.push(col);
                            cfg.columns.len() - 1
                        };
                        rebuild(Some(idx));
                    }
                Ok(())
            });
        }

        // ← 表示中で選択中の列を外す（最低1列は残す）。
        {
            let shared = shared.clone();
            let rebuild = rebuild.clone();
            let selected = selected.clone();
            to_avail.on().bn_clicked(move || {
                let Some(i) = selected.get() else { return Ok(()) };
                let next = {
                    let mut cfg = shared.cfg.borrow_mut();
                    if cfg.columns.len() <= 1 {
                        return Ok(());
                    }
                    cfg.columns.remove(i);
                    Some(i.saturating_sub(1).min(cfg.columns.len() - 1))
                };
                rebuild(next);
                Ok(())
            });
        }

        // 手前へ / 後ろへ：順序入れ替え。
        {
            let shared = shared.clone();
            let rebuild = rebuild.clone();
            let selected = selected.clone();
            fwd.on().bn_clicked(move || {
                let Some(i) = selected.get() else { return Ok(()) };
                if i == 0 {
                    return Ok(());
                }
                shared.cfg.borrow_mut().columns.swap(i, i - 1);
                rebuild(Some(i - 1));
                Ok(())
            });
        }
        {
            let shared = shared.clone();
            let rebuild = rebuild.clone();
            let selected = selected.clone();
            back.on().bn_clicked(move || {
                let Some(i) = selected.get() else { return Ok(()) };
                let len = shared.cfg.borrow().columns.len();
                if i + 1 >= len {
                    return Ok(());
                }
                shared.cfg.borrow_mut().columns.swap(i, i + 1);
                rebuild(Some(i + 1));
                Ok(())
            });
        }

        // 自動調整トグル。
        {
            let shared = shared.clone();
            let ac = auto_check.clone();
            auto_check.on().bn_clicked(move || {
                shared.cfg.borrow_mut().auto_adjust_columns = ac.is_checked();
                Ok(())
            });
        }

        Self { shown, available, spacing_spin, spacing_init: spacing.clamp(-20, 20), rebuild }
    }

    /// 窓生成後に表示中の列・使用可能な列・既定ソートを流し込む（生成前の add は無効化されるため）。
    fn populate(&self) {
        for (head, width) in [("種類", 286), ("幅", 80)] {
            let _ = self.shown.cols().add(head, gui::dpi_x(width));
        }
        let labels: Vec<&str> = COLUMN_KINDS.iter().map(|(_, l)| *l).collect();
        let _ = self.available.items().add(&labels);

        // 生成時に value==0 だと省かれる位置設定を、既定 0 でも正しく表示されるよう明示する。
        self.spacing_spin.set_pos(self.spacing_init);

        (self.rebuild)(Some(0));
    }
}

/// ショートカット入力を先頭1文字へ丸める（空白のみ/空は空文字）。
fn normalize_shortcut(raw: &str) -> String {
    raw.trim().chars().next().map(|c| c.to_string()).unwrap_or_default()
}

/// 入力フィールドの現在値から `Bookmark` を組む。ショートカットは先頭1文字に丸める。
fn fields_to_bookmark(name: &gui::Edit, path: &gui::Edit, sc: &gui::Edit) -> Bookmark {
    let label = name.text().unwrap_or_default().trim().to_owned();
    let path = path.text().unwrap_or_default().trim().to_owned();
    let shortcut = normalize_shortcut(&sc.text().unwrap_or_default());
    Bookmark { label, path, shortcut }
}

/// パスの末尾要素（登録名の既定値）。取れなければパスそのもの。
fn leaf_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

/// 項目1つを一覧の2列（ラベル／コマンド）へ整形する。セパレータは区切り線として見せる。
fn menu_item_columns(it: &MenuItem) -> [String; 2] {
    if it.separator {
        ["──────────".to_string(), String::new()]
    } else {
        [it.label.clone(), it.command.clone()]
    }
}

/// メニュー項目を編集欄の内容（ラベル／コマンド／セパレータ）で操作するクロージャ。
type MenuItemOp = Rc<dyn Fn(&str, &str, bool)>;

/// 編集欄の内容から項目を1つ作る。セパレータ時はラベル/コマンドを無視し区切り線にする。
/// それ以外でラベルが空なら既定名で埋める。
fn build_menu_item(label: &str, command: &str, sep: bool) -> MenuItem {
    if sep {
        MenuItem::separator()
    } else {
        let label = label.trim();
        let label = if label.is_empty() { "項目" } else { label };
        MenuItem::entry(label, command.trim())
    }
}

/// 「メニュー」ページ。左にメニュー名一覧、右に選択メニューの項目（ラベル／コマンド）を出す
/// マスターディテール。`menu("名前")` で開く名前付きメニュー（`shared.cfg.menus`）を編集する。
/// 左の名前欄＋ボタンでメニューの追加/改名/削除/並べ替え。項目の編集は後続増分。
#[derive(Clone)]
struct MenusPane {
    menu_list: gui::ListView<()>,
    item_list: gui::ListView<()>,
    /// 左のメニュー名一覧を `cfg.menus` から組み直し、指定 index を選び直す。
    rebuild_menus: Rc<dyn Fn(Option<usize>)>,
}

impl MenusPane {
    fn new(
        parent: &gui::WindowControl,
        shared: &Rc<Shared>,
        wnd: &gui::WindowModal,
        scripts: Vec<crate::script::ScriptCommand>,
        members: Vec<String>,
    ) -> Self {
        label(parent, "メニュー（Menu(\"名前\") で開く）。選ぶと右に項目が出る。", 8, 8, 520);
        let menu_list = gui::ListView::<()>::new(
            parent,
            gui::ListViewOpts {
                position: gui::dpi(8, 30),
                size: gui::dpi(240, 400),
                control_style: co::LVS::REPORT
                    | co::LVS::NOSORTHEADER
                    | co::LVS::SHOWSELALWAYS
                    | co::LVS::SINGLESEL,
                control_ex_style: co::LVS_EX::FULLROWSELECT,
                ..Default::default()
            },
        );
        let item_list = gui::ListView::<()>::new(
            parent,
            gui::ListViewOpts {
                position: gui::dpi(258, 30),
                size: gui::dpi(510, 400),
                control_style: co::LVS::REPORT
                    | co::LVS::NOSORTHEADER
                    | co::LVS::SHOWSELALWAYS
                    | co::LVS::SINGLESEL,
                control_ex_style: co::LVS_EX::FULLROWSELECT,
                ..Default::default()
            },
        );

        label(parent, "メニュー名", 8, 440, 240);
        let name_edit = gui::Edit::new(
            parent,
            gui::EditOpts {
                position: gui::dpi(8, 460),
                width: gui::dpi_x(240),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        let add = button(parent, "追加(&N)", 8, 488, 74);
        let rename = button(parent, "改名(&R)", 86, 488, 74);
        let del = button(parent, "削除(&D)", 164, 488, 74);
        let up = button(parent, "↑", 8, 518, 74);
        let down = button(parent, "↓", 86, 518, 74);

        // 右ペイン下部：選択中メニューの項目を編集する欄とボタン。
        label(parent, "ラベル", 258, 440, 250);
        let label_edit = gui::Edit::new(
            parent,
            gui::EditOpts {
                position: gui::dpi(258, 460),
                width: gui::dpi_x(250),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        label(parent, "コマンド", 514, 440, 254);
        let command_edit = gui::Edit::new(
            parent,
            gui::EditOpts {
                position: gui::dpi(514, 460),
                width: gui::dpi_x(178),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        let pick_btn = button(parent, "コードを編集(&P)", 696, 459, 72);
        let sep_check = gui::CheckBox::new(
            parent,
            gui::CheckBoxOpts {
                text: "セパレータ(&S)",
                position: gui::dpi(258, 490),
                size: gui::dpi(150, 22),
                ..Default::default()
            },
        );
        let item_add = button(parent, "項目追加(&A)", 258, 518, 90);
        let item_update = button(parent, "更新(&U)", 352, 518, 74);
        let item_del = button(parent, "項目削除(&X)", 430, 518, 90);
        let item_up = button(parent, "↑", 524, 518, 40);
        let item_down = button(parent, "↓", 568, 518, 40);

        let selected_menu: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let selected_item: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));

        // 選択中の項目を覚え、ラベル/コマンド/セパレータの編集欄へ反映する。None で欄を空に戻す。
        let show_item: Rc<dyn Fn(Option<usize>)> = Rc::new({
            let shared = shared.clone();
            let selected_menu = selected_menu.clone();
            let selected_item = selected_item.clone();
            let label_edit = label_edit.clone();
            let command_edit = command_edit.clone();
            let sep_check = sep_check.clone();
            move |item_sel| {
                selected_item.set(item_sel);
                let (label, command, sep) = item_sel
                    .and_then(|ii| {
                        let cfg = shared.cfg.borrow();
                        cfg.menus
                            .get(selected_menu.get()?)?
                            .items
                            .get(ii)
                            .map(|it| (it.label.clone(), it.command.clone(), it.separator))
                    })
                    .unwrap_or_default();
                let _ = label_edit.set_text(&label);
                let _ = command_edit.set_text(&command);
                sep_check.set_check(sep);
            }
        });

        // 右の項目一覧を、選択中メニューの items から組み直す。引数は選び直す項目 index（None で
        // 何も選ばず欄を空に）。
        let rebuild_items: Rc<dyn Fn(Option<usize>)> = Rc::new({
            let item_list = item_list.clone();
            let shared = shared.clone();
            let selected_menu = selected_menu.clone();
            let show_item = show_item.clone();
            move |item_sel| {
                let _ = item_list.items().delete_all();
                if let Some(mi) = selected_menu.get() {
                    let cfg = shared.cfg.borrow();
                    if let Some(menu) = cfg.menus.get(mi) {
                        for it in &menu.items {
                            let _ = item_list.items().add(&menu_item_columns(it), None, ());
                        }
                    }
                }
                if let Some(ii) = item_sel
                    && let Some(it) = item_list.items().iter().nth(ii)
                {
                    let _ = it.select(true);
                    let _ = it.focus();
                }
                show_item(item_sel);
            }
        });

        // 左を cfg.menus から組み直し、sel を選び直して右へも反映する。
        let rebuild_menus: Rc<dyn Fn(Option<usize>)> = Rc::new({
            let menu_list = menu_list.clone();
            let shared = shared.clone();
            let selected_menu = selected_menu.clone();
            let rebuild_items = rebuild_items.clone();
            move |sel| {
                let _ = menu_list.items().delete_all();
                for m in shared.cfg.borrow().menus.iter() {
                    let _ = menu_list.items().add(std::slice::from_ref(&m.name), None, ());
                }
                if let Some(i) = sel
                    && let Some(it) = menu_list.items().iter().nth(i)
                {
                    let _ = it.select(true);
                    let _ = it.focus();
                }
                selected_menu.set(sel);
                rebuild_items(None);
            }
        });

        // 左の選択が変わったら、その index を覚えて名前欄と右を更新する。
        {
            let menu_list2 = menu_list.clone();
            let shared = shared.clone();
            let selected_menu = selected_menu.clone();
            let rebuild_items = rebuild_items.clone();
            let ne = name_edit.clone();
            menu_list.on().lvn_item_changed(move |_| {
                let sel = menu_list2.items().iter().position(|it| it.is_selected());
                if let Some(i) = sel
                    && let Some(m) = shared.cfg.borrow().menus.get(i)
                {
                    let _ = ne.set_text(&m.name);
                }
                selected_menu.set(sel);
                rebuild_items(None);
                Ok(())
            });
        }

        // 右の項目選択が変わったら、その index を覚えて編集欄へ反映する。
        {
            let item_list2 = item_list.clone();
            let show_item = show_item.clone();
            item_list.on().lvn_item_changed(move |_| {
                let sel = item_list2.items().iter().position(|it| it.is_selected());
                show_item(sel);
                Ok(())
            });
        }

        // メニュー操作はクロージャに抽出し、ボタンと debug フックの両方から呼ぶ。
        let do_add: Rc<dyn Fn(&str)> = Rc::new({
            let shared = shared.clone();
            let rebuild_menus = rebuild_menus.clone();
            move |name: &str| {
                let name = name.trim();
                let base = if name.is_empty() { "新しいメニュー" } else { name };
                let idx = {
                    let mut cfg = shared.cfg.borrow_mut();
                    let existing: Vec<String> = cfg.menus.iter().map(|m| m.name.clone()).collect();
                    let unique = rerics_core::unique_name(base, &existing);
                    cfg.menus.push(MenuDef { name: unique, items: Vec::new() });
                    cfg.menus.len() - 1
                };
                rebuild_menus(Some(idx));
            }
        });
        let do_rename: Rc<dyn Fn(&str)> = Rc::new({
            let shared = shared.clone();
            let rebuild_menus = rebuild_menus.clone();
            let selected_menu = selected_menu.clone();
            move |name: &str| {
                let Some(i) = selected_menu.get() else { return };
                let name = name.trim();
                if name.is_empty() {
                    return;
                }
                {
                    let mut cfg = shared.cfg.borrow_mut();
                    // 自分以外の名前と衝突しないよう一意化する（同名へ戻すだけなら自分は除くので不変）。
                    let existing: Vec<String> = cfg
                        .menus
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != i)
                        .map(|(_, m)| m.name.clone())
                        .collect();
                    let unique = rerics_core::unique_name(name, &existing);
                    if let Some(m) = cfg.menus.get_mut(i) {
                        m.name = unique;
                    }
                }
                rebuild_menus(Some(i));
            }
        });
        let do_delete: Rc<dyn Fn()> = Rc::new({
            let shared = shared.clone();
            let rebuild_menus = rebuild_menus.clone();
            let selected_menu = selected_menu.clone();
            move || {
                let Some(i) = selected_menu.get() else { return };
                let next = {
                    let mut cfg = shared.cfg.borrow_mut();
                    if i < cfg.menus.len() {
                        cfg.menus.remove(i);
                    }
                    if cfg.menus.is_empty() {
                        None
                    } else {
                        Some(i.saturating_sub(1).min(cfg.menus.len() - 1))
                    }
                };
                rebuild_menus(next);
            }
        });
        let do_move: Rc<dyn Fn(i32)> = Rc::new({
            let shared = shared.clone();
            let rebuild_menus = rebuild_menus.clone();
            let selected_menu = selected_menu.clone();
            move |delta: i32| {
                let Some(i) = selected_menu.get() else { return };
                let len = shared.cfg.borrow().menus.len();
                let j = i as i32 + delta;
                if j < 0 || j as usize >= len {
                    return;
                }
                let j = j as usize;
                shared.cfg.borrow_mut().menus.swap(i, j);
                rebuild_menus(Some(j));
            }
        });

        // 項目操作も同様にクロージャへ抽出し、ボタンと debug フックの両方から呼ぶ。
        let do_item_add: MenuItemOp = Rc::new({
            let shared = shared.clone();
            let selected_menu = selected_menu.clone();
            let rebuild_items = rebuild_items.clone();
            move |label: &str, command: &str, sep: bool| {
                let Some(mi) = selected_menu.get() else { return };
                let new_idx = {
                    let mut cfg = shared.cfg.borrow_mut();
                    let Some(menu) = cfg.menus.get_mut(mi) else { return };
                    menu.items.push(build_menu_item(label, command, sep));
                    menu.items.len() - 1
                };
                rebuild_items(Some(new_idx));
            }
        });
        let do_item_update: MenuItemOp = Rc::new({
            let shared = shared.clone();
            let selected_menu = selected_menu.clone();
            let selected_item = selected_item.clone();
            let rebuild_items = rebuild_items.clone();
            move |label: &str, command: &str, sep: bool| {
                let Some(mi) = selected_menu.get() else { return };
                let Some(ii) = selected_item.get() else { return };
                {
                    let mut cfg = shared.cfg.borrow_mut();
                    let Some(item) = cfg.menus.get_mut(mi).and_then(|m| m.items.get_mut(ii)) else {
                        return;
                    };
                    *item = build_menu_item(label, command, sep);
                }
                rebuild_items(Some(ii));
            }
        });
        let do_item_delete: Rc<dyn Fn()> = Rc::new({
            let shared = shared.clone();
            let selected_menu = selected_menu.clone();
            let selected_item = selected_item.clone();
            let rebuild_items = rebuild_items.clone();
            move || {
                let Some(mi) = selected_menu.get() else { return };
                let Some(ii) = selected_item.get() else { return };
                let next = {
                    let mut cfg = shared.cfg.borrow_mut();
                    let Some(menu) = cfg.menus.get_mut(mi) else { return };
                    if ii < menu.items.len() {
                        menu.items.remove(ii);
                    }
                    if menu.items.is_empty() {
                        None
                    } else {
                        Some(ii.saturating_sub(1).min(menu.items.len() - 1))
                    }
                };
                rebuild_items(next);
            }
        });
        let do_item_move: Rc<dyn Fn(i32)> = Rc::new({
            let shared = shared.clone();
            let selected_menu = selected_menu.clone();
            let selected_item = selected_item.clone();
            let rebuild_items = rebuild_items.clone();
            move |delta: i32| {
                let Some(mi) = selected_menu.get() else { return };
                let Some(ii) = selected_item.get() else { return };
                let j = {
                    let mut cfg = shared.cfg.borrow_mut();
                    let Some(menu) = cfg.menus.get_mut(mi) else { return };
                    let j = ii as i32 + delta;
                    if j < 0 || j as usize >= menu.items.len() {
                        return;
                    }
                    let j = j as usize;
                    menu.items.swap(ii, j);
                    j
                };
                rebuild_items(Some(j));
            }
        });

        // コマンド欄の式をコードエディタ（補完つき）で編集する。現在の欄内容を初期値に開き、OK の
        // 文字列を欄へ書き戻す。キー編集の「式を編集」と同じ `code_box` を流用＝組込もスクリプトも
        // 同じ補完（引数ヒント＋説明）で編集できる。ボタンと debug フックの両方から呼ぶ。
        let script_summaries: std::collections::HashMap<String, String> = scripts
            .iter()
            .filter_map(|c| c.summary.clone().map(|s| (c.name.clone(), s)))
            .collect();
        let pick_command: Rc<dyn Fn()> = Rc::new({
            let wnd = wnd.clone();
            let command_edit = command_edit.clone();
            let members = members.clone();
            let script_summaries = script_summaries.clone();
            move || {
                let current = command_edit.text().unwrap_or_default();
                let comp = crate::dialog::completion_members(&members, |name| {
                    script_summaries.get(name).cloned()
                });
                if let Some(expr) = crate::dialog::code_box(
                    &wnd,
                    "メニュー項目の式を編集（組込はそのまま呼べる・r. でホスト API・複文可）",
                    current.trim(),
                    &comp,
                ) {
                    let _ = command_edit.set_text(expr.trim());
                }
            }
        });

        // ボタンは名前欄の内容を使って操作を呼ぶ。
        {
            let f = do_add.clone();
            let ne = name_edit.clone();
            add.on().bn_clicked(move || {
                f(&ne.text().unwrap_or_default());
                Ok(())
            });
        }
        {
            let f = do_rename.clone();
            let ne = name_edit.clone();
            rename.on().bn_clicked(move || {
                f(&ne.text().unwrap_or_default());
                Ok(())
            });
        }
        {
            let f = do_delete.clone();
            del.on().bn_clicked(move || {
                f();
                Ok(())
            });
        }
        {
            let f = do_move.clone();
            up.on().bn_clicked(move || {
                f(-1);
                Ok(())
            });
        }
        {
            let f = do_move.clone();
            down.on().bn_clicked(move || {
                f(1);
                Ok(())
            });
        }

        // 項目ボタンはラベル/コマンド欄とセパレータチェックの内容で操作を呼ぶ。
        {
            let f = do_item_add.clone();
            let le = label_edit.clone();
            let ce = command_edit.clone();
            let sc = sep_check.clone();
            item_add.on().bn_clicked(move || {
                f(&le.text().unwrap_or_default(), &ce.text().unwrap_or_default(), sc.is_checked());
                Ok(())
            });
        }
        {
            let f = do_item_update.clone();
            let le = label_edit.clone();
            let ce = command_edit.clone();
            let sc = sep_check.clone();
            item_update.on().bn_clicked(move || {
                f(&le.text().unwrap_or_default(), &ce.text().unwrap_or_default(), sc.is_checked());
                Ok(())
            });
        }
        {
            let f = do_item_delete.clone();
            item_del.on().bn_clicked(move || {
                f();
                Ok(())
            });
        }
        {
            let f = do_item_move.clone();
            item_up.on().bn_clicked(move || {
                f(-1);
                Ok(())
            });
        }
        {
            let f = do_item_move.clone();
            item_down.on().bn_clicked(move || {
                f(1);
                Ok(())
            });
        }
        {
            let f = pick_command.clone();
            pick_btn.on().bn_clicked(move || {
                f();
                Ok(())
            });
        }

        // debug-server：標準コントロールは generic な `/modal/*` で叩けないので、操作フックを
        // 登録して `/menu-editor/*` から駆動・観測できるようにする。
        #[cfg(feature = "debug-server")]
        {
            use crate::debug_server::modal_registry::{MenuEditorHooks, register_menu_editor};
            let do_select: Rc<dyn Fn(usize)> = Rc::new({
                let menu_list = menu_list.clone();
                let shared = shared.clone();
                let selected_menu = selected_menu.clone();
                let rebuild_items = rebuild_items.clone();
                let ne = name_edit.clone();
                move |idx: usize| {
                    if idx >= shared.cfg.borrow().menus.len() {
                        return;
                    }
                    if let Some(it) = menu_list.items().iter().nth(idx) {
                        let _ = it.select(true);
                        let _ = it.focus();
                    }
                    if let Some(m) = shared.cfg.borrow().menus.get(idx) {
                        let _ = ne.set_text(&m.name);
                    }
                    selected_menu.set(Some(idx));
                    rebuild_items(None);
                }
            });
            // 範囲内の項目を選び直す（左メニュー未選択や範囲外は何もしない）。
            let do_item_select: Rc<dyn Fn(usize)> = Rc::new({
                let shared = shared.clone();
                let selected_menu = selected_menu.clone();
                let rebuild_items = rebuild_items.clone();
                move |idx: usize| {
                    let in_range = selected_menu
                        .get()
                        .and_then(|mi| shared.cfg.borrow().menus.get(mi).map(|m| idx < m.items.len()))
                        .unwrap_or(false);
                    if in_range {
                        rebuild_items(Some(idx));
                    }
                }
            });
            let read: Box<dyn Fn() -> String> = Box::new({
                let shared = shared.clone();
                let selected_menu = selected_menu.clone();
                let selected_item = selected_item.clone();
                let label_edit = label_edit.clone();
                let command_edit = command_edit.clone();
                let sep_check = sep_check.clone();
                move || {
                    let cfg = shared.cfg.borrow();
                    let menus: Vec<_> = cfg
                        .menus
                        .iter()
                        .map(|m| {
                            let items: Vec<_> = m
                                .items
                                .iter()
                                .map(|it| {
                                    serde_json::json!({
                                        "label": it.label,
                                        "command": it.command,
                                        "separator": it.separator,
                                    })
                                })
                                .collect();
                            serde_json::json!({ "name": m.name, "items": items })
                        })
                        .collect();
                    serde_json::json!({
                        "menus": menus,
                        "selected_menu": selected_menu.get(),
                        "selected_item": selected_item.get(),
                        "draft": {
                            "label": label_edit.text().unwrap_or_default(),
                            "command": command_edit.text().unwrap_or_default(),
                            "separator": sep_check.is_checked(),
                        },
                    })
                    .to_string()
                }
            });
            register_menu_editor(MenuEditorHooks {
                read,
                select_menu: Box::new({
                    let f = do_select;
                    move |i| f(i)
                }),
                add_menu: Box::new({
                    let f = do_add.clone();
                    move |n| f(n)
                }),
                rename_menu: Box::new({
                    let f = do_rename.clone();
                    move |n| f(n)
                }),
                delete_menu: Box::new({
                    let f = do_delete.clone();
                    move || f()
                }),
                move_menu: Box::new({
                    let f = do_move.clone();
                    move |d| f(d)
                }),
                select_item: Box::new({
                    let f = do_item_select;
                    move |i| f(i)
                }),
                add_item: Box::new({
                    let f = do_item_add.clone();
                    move |l, c, s| f(l, c, s)
                }),
                update_item: Box::new({
                    let f = do_item_update.clone();
                    move |l, c, s| f(l, c, s)
                }),
                delete_item: Box::new({
                    let f = do_item_delete.clone();
                    move || f()
                }),
                move_item: Box::new({
                    let f = do_item_move.clone();
                    move |d| f(d)
                }),
                pick_command: Box::new({
                    let f = pick_command.clone();
                    move || f()
                }),
            });
        }

        Self { menu_list, item_list, rebuild_menus }
    }

    /// 窓生成後に列を作り（生成前の add は無効化されるため）、左のメニュー名一覧を `cfg.menus`
    /// から組み直す。右の項目一覧は空に戻す。ページ表示時（`on_create`）に呼ぶ。
    fn populate(&self) {
        let _ = self.menu_list.cols().add("メニュー", gui::dpi_x(224));
        let _ = self.item_list.cols().add("ラベル", gui::dpi_x(220));
        let _ = self.item_list.cols().add("コマンド", gui::dpi_x(270));
        (self.rebuild_menus)(None);
    }
}

/// 「登録ディレクトリ」ページ。一覧（ショートカット/名前/場所）＋下部の入力欄でインライン編集
/// （追加/更新/削除/並べ替え/フォルダ参照）。編集は即 `shared.cfg.bookmarks` へ反映する。
#[derive(Clone)]
struct RegisteredPane {
    list: gui::ListView<()>,
    rebuild: Rc<dyn Fn(Option<usize>)>,
}

impl RegisteredPane {
    fn new(parent: &gui::WindowControl, shared: &Rc<Shared>) -> Self {
        label(parent, "ジャンプ一覧に出す場所。ショートカットは1文字。", 8, 8, 344);
        let list = gui::ListView::<()>::new(
            parent,
            gui::ListViewOpts {
                position: gui::dpi(8, 30),
                size: gui::dpi(760, 248),
                control_style: co::LVS::REPORT
                    | co::LVS::NOSORTHEADER
                    | co::LVS::SHOWSELALWAYS
                    | co::LVS::SINGLESEL,
                control_ex_style: co::LVS_EX::FULLROWSELECT,
                ..Default::default()
            },
        );

        label(parent, "名前", 8, 292, 56);
        let name_edit = gui::Edit::new(
            parent,
            gui::EditOpts {
                position: gui::dpi(72, 290),
                width: gui::dpi_x(660),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        label(parent, "場所", 8, 320, 56);
        let path_edit = gui::Edit::new(
            parent,
            gui::EditOpts {
                position: gui::dpi(72, 318),
                width: gui::dpi_x(588),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        let browse = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "参照...",
                position: gui::dpi(668, 317),
                width: gui::dpi_x(64),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );
        label(parent, "ショートカット", 8, 348, 78);
        let sc_edit = gui::Edit::new(
            parent,
            gui::EditOpts {
                position: gui::dpi(90, 346),
                width: gui::dpi_x(40),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );

        let add = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "追加(&D)",
                position: gui::dpi(8, 382),
                width: gui::dpi_x(64),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );
        let update = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "更新(&U)",
                position: gui::dpi(78, 382),
                width: gui::dpi_x(64),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );
        let del = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "削除(&L)",
                position: gui::dpi(148, 382),
                width: gui::dpi_x(64),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );
        let up = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "↑",
                position: gui::dpi(240, 382),
                width: gui::dpi_x(48),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );
        let down = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "↓",
                position: gui::dpi(296, 382),
                width: gui::dpi_x(48),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );

        // ListView 選択 index（= bookmarks の index）。
        let selected: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));

        // bookmarks から一覧を組み直し、指定行を選択し直す。
        let rebuild: Rc<dyn Fn(Option<usize>)> = Rc::new({
            let list = list.clone();
            let shared = shared.clone();
            let selected = selected.clone();
            move |sel| {
                let _ = list.items().delete_all();
                for b in shared.cfg.borrow().bookmarks.iter() {
                    let _ = list.items().add(
                        &[b.shortcut.clone(), b.label.clone(), b.path.clone()],
                        None,
                        (),
                    );
                }
                if let Some(i) = sel
                    && let Some(it) = list.items().iter().nth(i) {
                        let _ = it.select(true);
                        let _ = it.focus();
                    }
                selected.set(sel);
            }
        });

        // 行を選んだら、その内容を下の入力欄へ展開する。
        {
            let list2 = list.clone();
            let shared = shared.clone();
            let ne = name_edit.clone();
            let pe = path_edit.clone();
            let se = sc_edit.clone();
            let selected = selected.clone();
            list.on().lvn_item_changed(move |_| {
                if let Some(i) = list2.items().iter().position(|it| it.is_selected()) {
                    if let Some(b) = shared.cfg.borrow().bookmarks.get(i) {
                        let _ = ne.set_text(&b.label);
                        let _ = pe.set_text(&b.path);
                        let _ = se.set_text(&b.shortcut);
                    }
                    selected.set(Some(i));
                }
                Ok(())
            });
        }

        // 追加：入力欄の内容を新規行として末尾に足す（場所は必須・名前は空ならパス末尾）。
        {
            let shared = shared.clone();
            let rebuild = rebuild.clone();
            let ne = name_edit.clone();
            let pe = path_edit.clone();
            let se = sc_edit.clone();
            add.on().bn_clicked(move || {
                let mut b = fields_to_bookmark(&ne, &pe, &se);
                if b.path.is_empty() {
                    return Ok(());
                }
                if b.label.is_empty() {
                    b.label = leaf_label(&b.path);
                }
                let idx = {
                    let mut cfg = shared.cfg.borrow_mut();
                    cfg.bookmarks.push(b);
                    cfg.bookmarks.len() - 1
                };
                rebuild(Some(idx));
                Ok(())
            });
        }

        // 更新：選択行を入力欄の内容で上書きする（場所は必須）。
        {
            let shared = shared.clone();
            let rebuild = rebuild.clone();
            let selected = selected.clone();
            let ne = name_edit.clone();
            let pe = path_edit.clone();
            let se = sc_edit.clone();
            update.on().bn_clicked(move || {
                let Some(i) = selected.get() else { return Ok(()) };
                let mut b = fields_to_bookmark(&ne, &pe, &se);
                if b.path.is_empty() {
                    return Ok(());
                }
                if b.label.is_empty() {
                    b.label = leaf_label(&b.path);
                }
                {
                    let mut cfg = shared.cfg.borrow_mut();
                    if let Some(slot) = cfg.bookmarks.get_mut(i) {
                        *slot = b;
                    }
                }
                rebuild(Some(i));
                Ok(())
            });
        }

        // 削除：選択行を消す。直前の行を選び直す。
        {
            let shared = shared.clone();
            let rebuild = rebuild.clone();
            let selected = selected.clone();
            del.on().bn_clicked(move || {
                let Some(i) = selected.get() else { return Ok(()) };
                let next = {
                    let mut cfg = shared.cfg.borrow_mut();
                    if i < cfg.bookmarks.len() {
                        cfg.bookmarks.remove(i);
                    }
                    if cfg.bookmarks.is_empty() {
                        None
                    } else {
                        Some(i.saturating_sub(1).min(cfg.bookmarks.len() - 1))
                    }
                };
                rebuild(next);
                Ok(())
            });
        }

        // ↑/↓：選択行を入れ替えて並べ替える。
        {
            let shared = shared.clone();
            let rebuild = rebuild.clone();
            let selected = selected.clone();
            up.on().bn_clicked(move || {
                let Some(i) = selected.get() else { return Ok(()) };
                if i == 0 {
                    return Ok(());
                }
                shared.cfg.borrow_mut().bookmarks.swap(i, i - 1);
                rebuild(Some(i - 1));
                Ok(())
            });
        }
        {
            let shared = shared.clone();
            let rebuild = rebuild.clone();
            let selected = selected.clone();
            down.on().bn_clicked(move || {
                let Some(i) = selected.get() else { return Ok(()) };
                let len = shared.cfg.borrow().bookmarks.len();
                if i + 1 >= len {
                    return Ok(());
                }
                shared.cfg.borrow_mut().bookmarks.swap(i, i + 1);
                rebuild(Some(i + 1));
                Ok(())
            });
        }

        // 参照：フォルダ選択ダイアログで場所を埋める（名前が空なら末尾名を補う）。
        {
            let parent_hwnd = parent.hwnd().ptr();
            let pe = path_edit.clone();
            let ne = name_edit.clone();
            browse.on().bn_clicked(move || {
                if let Some(dir) = crate::shell::choose_folder(parent_hwnd, "登録するフォルダを選択") {
                    let s = dir.to_string_lossy().into_owned();
                    let _ = pe.set_text(&s);
                    if ne.text().unwrap_or_default().trim().is_empty() {
                        let _ = ne.set_text(&leaf_label(&s));
                    }
                }
                Ok(())
            });
        }

        Self { list, rebuild }
    }

    /// 窓生成後に列を作り、一覧を流し込む（生成前の add は無効化されるため）。
    /// 先頭行を初期選択する（空なら select は無効化されるだけ）。
    fn populate(&self) {
        for (head, width) in [("", 44), ("名前", 200), ("場所", 500)] {
            let _ = self.list.cols().add(head, gui::dpi_x(width));
        }
        (self.rebuild)(Some(0));
    }
}

/// 設定ダイアログを表示する。`OK`／`適用` で確定した [`Config`] を `on_apply` へ渡す
/// （`適用` は閉じずに継続、`OK` は閉じる。`キャンセル` は破棄して閉じる）。
pub fn show(
    parent: &impl GuiParent,
    current: &Config,
    scripts: Vec<crate::script::ScriptCommand>,
    members: Vec<String>,
    on_apply: impl Fn(&Config) + 'static,
) {
    // 前回開いた設定ダイアログのキー編集フックを捨てる（このダイアログ生成で登録し直す）。
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::clear_key_editors();
    let (wnd, arm) = crate::dialog::modal_window_sysmenu("設定", 960, 620);

    let shared = Rc::new(Shared {
        cfg: RefCell::new(current.clone()),
        target_dark: Cell::new(current.resolved == ResolvedTheme::Dark),
    });

    // 左カラム：ナビ（フラット・自前描画）。ジャンル見出しは選択不可、ページだけが選べる。
    let nav = SettingsNav::new(&wnd, gui::dpi(12, 12), gui::dpi(152, 544));

    // 中央カラム：セクション pane（同じ矩形に重ねて show/hide で切替）。番号は nav の data と対応。
    let pane_pos = gui::dpi(172, 12);
    let pane_size = gui::dpi(360, 544);
    // 外観ページ（0..=2）はプレビューと並ぶので狭く、それ以外はプレビューを出さないので
    // その領域までフル幅にする（プレビューは「外観のときだけ出す」専用機能）。
    let pane_wide = gui::dpi(776, 544);
    let pane_appearance = make_pane(&wnd, pane_pos, pane_size); // 0
    let pane_colors = make_pane(&wnd, pane_pos, pane_size); // 1
    let pane_layout = make_pane(&wnd, pane_pos, pane_size); // 2
    let pane_cursor = make_pane(&wnd, pane_pos, pane_wide); // 3
    let pane_registered = make_pane(&wnd, pane_pos, pane_wide); // 4
    let pane_keys = make_pane(&wnd, pane_pos, pane_wide); // 5
    let pane_image = make_pane(&wnd, pane_pos, pane_wide); // 6
    let pane_list = make_pane(&wnd, pane_pos, pane_wide); // 7
    let pane_behavior = make_pane(&wnd, pane_pos, pane_wide); // 8
    let pane_viewer_colors = make_pane(&wnd, pane_pos, pane_size); // 9
    let pane_keys_text = make_pane(&wnd, pane_pos, pane_wide); // 10
    let pane_keys_image = make_pane(&wnd, pane_pos, pane_wide); // 11
    let pane_fileops = make_pane(&wnd, pane_pos, pane_wide); // 12
    let pane_menus = make_pane(&wnd, pane_pos, pane_wide); // 13
    let panes = vec![
        pane_appearance.clone(),
        pane_colors.clone(),
        pane_layout.clone(),
        pane_cursor.clone(),
        pane_registered.clone(),
        pane_keys.clone(),
        pane_image.clone(),
        pane_list.clone(),
        pane_behavior.clone(),
        pane_viewer_colors.clone(),
        pane_keys_text.clone(),
        pane_keys_image.clone(),
        pane_fileops.clone(),
        pane_menus.clone(),
    ];

    // 右カラム：プレビュー（外観カテゴリ選択中だけ表示）。表示テーマは「配色テーマ」に追従する
    // （専用のダーク/ライト切替は廃止）。pane 切替に応じてラベルごと show/hide する。
    let preview_label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "プレビュー",
            position: gui::dpi(546, 14),
            size: gui::dpi(80, 18),
            ..Default::default()
        },
    );
    let preview = Preview::new(&wnd, gui::dpi(546, 40), gui::dpi(402, 516), shared.clone());
    // テキストビューア配色用のプレビュー（同じ場所に重ね、ページに応じて出し分ける）。
    let viewer_preview = ViewerPreview::new(&wnd, gui::dpi(546, 40), gui::dpi(402, 516), shared.clone());
    viewer_preview.hwnd().ShowWindow(co::SW::HIDE);

    // 各 pane の中身。
    build_appearance(&pane_appearance, &shared, &preview);
    build_layout(&pane_layout, &shared, &preview);
    build_cursor(&pane_cursor, &shared);
    build_viewer(&pane_image, &shared);
    build_behavior(&pane_behavior, &shared);
    build_fileops(&pane_fileops, &shared);
    build_list(&pane_list, &shared);
    let columns_editor = ColumnsEditor::new(&pane_list, &shared);
    let registered = RegisteredPane::new(&pane_registered, &shared);
    let menus_pane = MenusPane::new(&pane_menus, &shared, &wnd, scripts.clone(), members.clone());
    let keys = KeyEditor::new(&pane_keys, &shared, KeyCategory::Filer, scripts, members.clone());
    let keys_text =
        KeyEditor::new(&pane_keys_text, &shared, KeyCategory::TextViewer, Vec::new(), members.clone());
    let keys_image =
        KeyEditor::new(&pane_keys_image, &shared, KeyCategory::ImageViewer, Vec::new(), members);

    // 配色 pane（ファイル一覧・ログ）とテキストビューア pane（ビューア専用色）。
    // 色変更後はそれぞれ対応するプレビューだけを再描画する。
    build_color_page(&pane_colors, &shared, {
        let preview = preview.clone();
        Rc::new(move || preview.refresh())
    }, COLOR_FIELDS);
    build_color_page(&pane_viewer_colors, &shared, {
        let viewer_preview = viewer_preview.clone();
        Rc::new(move || viewer_preview.refresh())
    }, VIEWER_COLOR_FIELDS);

    // 下段左：検証メッセージ（キー重複で OK/適用が弾かれた理由を、どのページを見ていても出す）。
    let validation_label = gui::Label::new(
        &wnd,
        gui::LabelOpts {
            text: "",
            position: gui::dpi(12, 584),
            size: gui::dpi(632, 18),
            ..Default::default()
        },
    );

    // 下段：OK / キャンセル / 適用。
    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(658, 578),
            width: gui::dpi_x(90),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );
    let cancel = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "キャンセル",
            ctrl_id: 2,
            position: gui::dpi(756, 578),
            width: gui::dpi_x(94),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );
    let apply = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "適用(&A)",
            ctrl_id: 3,
            position: gui::dpi(858, 578),
            width: gui::dpi_x(90),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );

    let on_apply = Rc::new(on_apply);

    #[cfg(feature = "debug-server")]
    arm.plain(
        "settings",
        "設定",
        "",
        false,
        vec![
            ("OK".to_string(), 1u16),
            ("キャンセル".to_string(), 2u16),
            ("適用".to_string(), 3u16),
        ],
    );

    // window 生成後：ナビ流し込み・初期表示 pane・各リスト初期化。
    {
        let nav = nav.clone();
        let panes = panes.clone();
        let keys = keys.clone();
        let keys_text = keys_text.clone();
        let keys_image = keys_image.clone();
        let registered = registered.clone();
        let menus_pane = menus_pane.clone();
        let columns_editor = columns_editor.clone();
        arm.on_create(move |_| {
            // 初期表示：先頭ページ（pane 0）を出し、ナビへフォーカスを与える。
            let init = nav.selected_pane();
            for (i, p) in panes.iter().enumerate() {
                p.hwnd().ShowWindow(if i == init { co::SW::SHOW } else { co::SW::HIDE });
            }
            nav.hwnd().SetFocus();
            registered.populate();
            menus_pane.populate();
            keys.populate();
            keys_text.populate();
            keys_image.populate();
            columns_editor.populate();
            Ok(())
        });
    }

    // ナビのページ選択で中央 pane を切り替え、プレビューを出し分ける：
    // ファイル一覧プレビュー（pane 0..=2）／テキストビューアプレビュー（pane 9）。
    {
        let panes = panes.clone();
        let preview = preview.clone();
        let viewer_preview = viewer_preview.clone();
        let preview_label = preview_label.clone();
        nav.on_select(move |pane| {
            for (i, p) in panes.iter().enumerate() {
                p.hwnd().ShowWindow(if i == pane { co::SW::SHOW } else { co::SW::HIDE });
            }
            let list_pv = pane <= 2;
            let viewer_pv = pane == 9;
            preview_label.hwnd().ShowWindow(if list_pv || viewer_pv { co::SW::SHOW } else { co::SW::HIDE });
            preview.hwnd().ShowWindow(if list_pv { co::SW::SHOW } else { co::SW::HIDE });
            viewer_preview.hwnd().ShowWindow(if viewer_pv { co::SW::SHOW } else { co::SW::HIDE });
        });
        // debug-server から pane 番号でページを切り替えられるようにする（スナップショット観測用）。
        #[cfg(feature = "debug-server")]
        {
            let nav = nav.clone();
            crate::debug_server::modal_registry::register_settings_nav(Box::new(move |pane| {
                if let Some(i) = NAV_ROWS
                    .iter()
                    .position(|r| matches!(r, NavRow::Page { pane: p, .. } if *p == pane))
                {
                    nav.select(i);
                }
            }));
        }
    }

    // キー編集の検証＋反映：3 ページのどれかにキー重複（衝突）があれば反映せず false を返し
    // （理由は各ページのステータスへ）、無ければ下書きを config へ書き戻して true。
    let validate_and_flush: Rc<dyn Fn() -> bool> = {
        let editors = vec![keys.clone(), keys_text.clone(), keys_image.clone()];
        let validation_label = validation_label.clone();
        Rc::new(move || {
            let mut briefs = Vec::new();
            for e in &editors {
                if let Some(b) = e.conflict_brief() {
                    e.note_conflicts();
                    briefs.push(b);
                }
            }
            if briefs.is_empty() {
                for e in &editors {
                    e.flush_draft();
                }
                let _ = validation_label.hwnd().SetWindowText("");
                true
            } else {
                let _ = validation_label.hwnd().SetWindowText(&format!(
                    "キーが重複しています（解決するまで反映できません）— {}",
                    briefs.join(" / ")
                ));
                false
            }
        })
    };

    // 適用：検証を通れば閉じずに現在の設定を反映する。重複があれば反映しない（閉じない）。
    {
        let on_apply = on_apply.clone();
        let shared = shared.clone();
        let validate_and_flush = validate_and_flush.clone();
        apply.on().bn_clicked(move || {
            if validate_and_flush() {
                on_apply(&shared.cfg.borrow());
            }
            Ok(())
        });
    }
    // OK：検証を通れば反映して閉じる。重複があれば反映せず**閉じない**（継続編集）。
    {
        let on_apply = on_apply.clone();
        let shared = shared.clone();
        let wnd2 = wnd.clone();
        let validate_and_flush = validate_and_flush.clone();
        ok.on().bn_clicked(move || {
            if validate_and_flush() {
                on_apply(&shared.cfg.borrow());
                wnd2.close();
            }
            Ok(())
        });
    }
    // キャンセル：破棄して閉じる（実反映は適用／OK 時のみなので revert 不要）。
    {
        let wnd2 = wnd.clone();
        cancel.on().bn_clicked(move || {
            wnd2.close();
            Ok(())
        });
    }

    let _ = wnd.show_modal(parent);
    // 閉じたらキー編集フックを捨てる（破棄済みウィンドウを debug 経路が触らないように）。
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::clear_key_editors();
    let _ = (nav, panes, keys, keys_text, keys_image, registered, menus_pane, ok, cancel, apply, preview_label, preview, viewer_preview);
}

#[cfg(test)]
mod tests {
    use super::{leaf_label, normalize_shortcut};

    #[test]
    fn normalize_shortcut_keeps_first_char_only() {
        assert_eq!(normalize_shortcut("G"), "G");
        assert_eq!(normalize_shortcut("  d  "), "d"); // 前後空白は除去
        assert_eq!(normalize_shortcut("GG"), "G"); // 先頭1文字だけ
        assert_eq!(normalize_shortcut(""), "");
        assert_eq!(normalize_shortcut("   "), "");
    }

    #[test]
    fn leaf_label_uses_path_tail() {
        assert_eq!(leaf_label("C:\\Users\\me\\Documents"), "Documents");
        assert_eq!(leaf_label("D:\\work"), "work");
    }
}
