//! 設定ダイアログ。左ナビ（フラットな自前描画リスト：ジャンル見出しは選択不可・ページだけが
//! ↑↓／クリックで選べる）と中央の詳細 pane、外観カテゴリ選択中だけ右へ出す
//! 「ミニ全体窓」プレビューの構成。
//!
//! 編集中の値はすべて [`Shared`] へ即時反映し、配色・フォント・レイアウト寸法・テーマの
//! 変更はその場でプレビューへ反映する（ライブプレビュー）。`OK`／`適用` で現在の [`Config`]
//! を `on_apply` コールバックへ渡し（呼び出し側がライブ反映＋差分保存する）、`適用` は閉じずに
//! 継続、`OK` は閉じる。`キャンセル` は最後の `適用` 以降の編集を破棄して閉じる。

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use rerics_core::{
    Bookmark, Colors, Column, ColumnKind, Command, CommandContext, Config, FileOpSettings, IconSize,
    Invocation, KeyChord, KeyMap, Layout, Rgb, ResolvedTheme, SizeFormat, SortType, Theme,
    WheelAction,
};
use winsafe::{self as w, co, gui, msg::lb, prelude::*};

/// 自前描画コントロールをオフスクリーン DC へ描かせるメッセージ。`PrintWindow`
/// （デバッグ制御サーバの `/snapshot/modal`）が子ごとに送るので、これに応答しないと黒く写る。
const WM_PRINTCLIENT: u32 = 0x0318;

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
struct Shared {
    cfg: RefCell<Config>,
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

fn label(parent: &(impl GuiParent + 'static), text: &str, x: i32, y: i32, cx: i32) {
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
    NavRow::Page { label: "登録ディレクトリ", pane: 4 },
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
    group_box(parent, "画像", 12, 8, 752, 76);
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

    // テキストセクション（設定項目は今後追加）。
    group_box(parent, "テキスト", 12, 96, 752, 76);
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

/// 既定ソート選択肢（リスト表示順）。
const SORT_TYPES: &[(SortType, &str)] = &[
    (SortType::FileName, "名前順"),
    (SortType::Extension, "拡張子順"),
    (SortType::Length, "サイズ順"),
    (SortType::LastWriteTime, "更新日時順"),
    (SortType::CreateTime, "作成日時順"),
    (SortType::Attribute, "属性順"),
    (SortType::FileNameExpLike, "名前順（自然順）"),
    (SortType::ExtensionExpLike, "拡張子順（自然順）"),
];

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
    sort_list: gui::ListBox,
    /// 既定ソートリストで初期選択する行（窓生成後の populate で選ぶ）。
    sort_sel: Option<usize>,
    rebuild: Rc<dyn Fn(Option<usize>)>,
}

impl ColumnsEditor {
    fn new(parent: &gui::WindowControl, shared: &Rc<Shared>) -> Self {
        // 既定の並び順（state が無い初回起動時に使う）。
        group_box(parent, "既定の並び順", 12, 8, 250, 158);
        let sort_list = gui::ListBox::new(
            parent,
            gui::ListBoxOpts {
                position: gui::dpi(24, 32),
                size: gui::dpi(226, 116),
                ..Default::default()
            },
        );
        {
            let shared = shared.clone();
            let sl = sort_list.clone();
            sort_list.on().lbn_sel_change(move || {
                if let Some(i) = unsafe { sl.hwnd().SendMessage(lb::GetCurSel {}) }
                    && let Some((st, _)) = SORT_TYPES.get(i as usize) {
                        shared.cfg.borrow_mut().default_sort = *st;
                    }
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

        let sort_sel = SORT_TYPES
            .iter()
            .position(|(s, _)| *s == shared.cfg.borrow().default_sort);

        Self { shown, available, sort_list, sort_sel, rebuild }
    }

    /// 窓生成後に表示中の列・使用可能な列・既定ソートを流し込む（生成前の add は無効化されるため）。
    fn populate(&self) {
        for (head, width) in [("種類", 286), ("幅", 80)] {
            let _ = self.shown.cols().add(head, gui::dpi_x(width));
        }
        let labels: Vec<&str> = COLUMN_KINDS.iter().map(|(_, l)| *l).collect();
        let _ = self.available.items().add(&labels);

        let sort_labels: Vec<&str> = SORT_TYPES.iter().map(|(_, l)| *l).collect();
        let _ = self.sort_list.items().add(&sort_labels);
        unsafe {
            let _ = self.sort_list.hwnd().SendMessage(lb::SetCurSel {
                index: self.sort_sel.map(|i| i as u32),
            });
        }

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

/// 「キー」ページが編集する対象のキーマップ。
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyCategory {
    Filer,
    TextViewer,
    ImageViewer,
}

impl KeyCategory {
    /// このカテゴリで有効なコマンド文脈。
    fn context(self) -> CommandContext {
        match self {
            KeyCategory::Filer => CommandContext::Filer,
            KeyCategory::TextViewer => CommandContext::TextViewer,
            KeyCategory::ImageViewer => CommandContext::ImageViewer,
        }
    }

    /// このカテゴリの既定キーマップ（文字列マップ）。
    fn default_map(self) -> BTreeMap<String, String> {
        match self {
            KeyCategory::Filer => KeyMap::default().to_string_map(),
            KeyCategory::TextViewer => KeyMap::default_textviewer().to_string_map(),
            KeyCategory::ImageViewer => KeyMap::default_imageviewer().to_string_map(),
        }
    }

    /// debug-server で指すカテゴリ名。
    #[cfg(feature = "debug-server")]
    fn debug_str(self) -> &'static str {
        match self {
            KeyCategory::Filer => "filer",
            KeyCategory::TextViewer => "text",
            KeyCategory::ImageViewer => "image",
        }
    }
}

/// 機能順の 1 行＝1 コマンドと、それに割り当たっている chord 群。
struct KeyRow {
    command: Command,
    chords: Vec<ChordRef>,
}

/// 機能順の行に並ぶ 1 つの chord 参照。`conflicted`＝同じ chord を他機能も定義している（衝突）。
struct ChordRef {
    token: String,
    conflicted: bool,
}

/// キー順の 1 行＝1 chord と、それを定義している全機能のラベル（2 つ以上＝衝突）。
struct KeyChordRow {
    chord: String,
    labels: Vec<String>,
}

/// 割り当て値の表示ラベル。既知コマンドは正規トークン名、未知（`Func_*` 等）は生値のまま。
fn binding_label(value: &str) -> String {
    Invocation::parse(value)
        .map(|i| i.command.as_token().to_string())
        .unwrap_or_else(|| value.trim().to_string())
}

/// キー編集ページの並べ方。
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyView {
    /// 機能順（コマンド → 割り当てキー群）。
    ByCommand,
    /// キー順（キー → 起動する機能）。
    ByKey,
}

/// 機能ピッカー（インライン）の対象。キー順で機能ラベルをダブルクリックすると、その chord の
/// `old_value`（差し替え対象の生 invocation）を別機能へ置き換えるためにこの状態へ入る。
struct PickState {
    chord: String,
    old_value: String,
}

/// 「キー」ページの編集状態。
struct KeyEditorInner {
    shared: Rc<Shared>,
    category: KeyCategory,
    /// 編集中の下書き＝chord → 割り当て値（生の invocation 文字列）のリスト。空 Vec＝明示 unbind。
    /// **重複（1 つの chord に複数機能）を許す**＝これが衝突状態。未知バインド（`Func_*` 等）も
    /// 生値のまま保持し、反映時に消さない。OK/適用の検証を通った時だけ `config.keybinds` へ書き戻す。
    draft: RefCell<BTreeMap<String, Vec<String>>>,
    rows: RefCell<Vec<KeyRow>>,
    /// キー順ビューの行（chord でソート）。`draft` から組む。
    key_rows: RefCell<Vec<KeyChordRow>>,
    /// ピックモード中ならその対象。`Some` の間はリストが機能ピッカー（全機能一覧）へ切り替わる。
    picking: RefCell<Option<PickState>>,
    /// ピックモードで選べる機能一覧（文脈で絞り済み）。`view` はこれを検索で絞った index。
    pick_rows: RefCell<Vec<Command>>,
    /// キー順で「キー定義を追加」したが、まだ機能未割当のキー（－表示の空キー定義）。
    pending: RefCell<Vec<String>>,
    /// 並べ方（機能順／キー順）。
    view_mode: Cell<KeyView>,
    /// 検索で絞り込んだ表示対象＝現モードの行配列へのインデックス（表示順）。`sel`/`top` はこの上の位置。
    view: RefCell<Vec<usize>>,
    /// 検索クエリ（機能名・キーへの部分一致・大小無視）。空なら全件表示。
    query: RefCell<String>,
    sel: Cell<usize>,
    /// 機能順で選択行のキー群のうち、サブ選択中のキー index（個別削除・個別変更の対象）。
    sub: Cell<usize>,
    /// 表示先頭行（スクロール）。
    top: Cell<usize>,
    row_h: Cell<i32>,
    /// 次の打鍵を選択行のコマンドへ割り当てる待ち状態。
    capturing: Cell<bool>,
    /// キャプチャがサブ選択キーの「変更（リマップ）」か（`true`）、新規追加か（`false`）。
    capturing_remap: Cell<bool>,
    /// キャプチャがキー順の「空キー定義の新規作成」か（`true`）。打鍵を pending へ入れる。
    capturing_newdef: Cell<bool>,
    /// 直近の操作結果（観測・状態表示用）。
    status: RefCell<String>,
}

/// 「キー」ページ（割り当ての対話編集・自前描画）。機能順にコマンドを並べ、行を選んで
/// 「キーを追加」→実際のキー打鍵で割り当てる。`config.keybinds` を直接編集し OK/適用で確定する。
#[derive(Clone)]
struct KeyEditor {
    list: gui::WindowControl,
    search: gui::Edit,
    /// 機能順／キー順の並べ替え切替。
    toggle: gui::RadioGroup,
    /// 選択対象に効く左グループの 3 ボタン（モードでラベル/動作を切替）。
    btn_a: gui::Button,
    btn_b: gui::Button,
    btn_c: gui::Button,
    inner: Rc<KeyEditorInner>,
}

impl KeyEditor {
    fn new(parent: &gui::WindowControl, shared: &Rc<Shared>, category: KeyCategory) -> Self {
        label(
            parent,
            "機能を選び「キーを追加」で割り当て（実際にキーを押す・Escで中止）",
            16,
            12,
            560,
        );
        label(parent, "検索:", 16, 44, 40);
        let search = gui::Edit::new(
            parent,
            gui::EditOpts {
                control_style: co::ES::AUTOHSCROLL,
                position: gui::dpi(60, 40),
                width: gui::dpi_x(500),
                height: gui::dpi_y(24),
                ..Default::default()
            },
        );
        let toggle = gui::RadioGroup::new(
            parent,
            &[
                gui::RadioButtonOpts {
                    text: "機能順(&F)",
                    position: gui::dpi(572, 42),
                    size: gui::dpi(92, 20),
                    selected: true,
                    ..Default::default()
                },
                gui::RadioButtonOpts {
                    text: "キー順(&Y)",
                    position: gui::dpi(668, 42),
                    size: gui::dpi(92, 20),
                    ..Default::default()
                },
            ],
        );
        let list = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                position: gui::dpi(16, 72),
                size: gui::dpi(744, 414),
                class_bg_brush: gui::Brush::Color(co::COLOR::WINDOW),
                style: co::WS::CHILD
                    | co::WS::VISIBLE
                    | co::WS::CLIPSIBLINGS
                    | co::WS::TABSTOP
                    | co::WS::BORDER
                    | co::WS::VSCROLL,
                ..Default::default()
            },
        );
        // 左グループ＝選択対象に効く 3 ボタン（初期は機能順ラベル）。
        let btn_a = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーを追加(&K)",
                position: gui::dpi(16, 496),
                width: gui::dpi_x(150),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        let btn_b = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーを変更(&C)",
                position: gui::dpi(174, 496),
                width: gui::dpi_x(150),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        let btn_c = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "キーを削除(&D)",
                position: gui::dpi(332, 496),
                width: gui::dpi_x(150),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        // 右に分離＝ページ全域に効く操作。
        let reset = gui::Button::new(
            parent,
            gui::ButtonOpts {
                text: "このページを既定に戻す(&R)",
                position: gui::dpi(580, 496),
                width: gui::dpi_x(180),
                height: gui::dpi_y(28),
                ..Default::default()
            },
        );
        let me = Self {
            list,
            search,
            toggle,
            btn_a: btn_a.clone(),
            btn_b: btn_b.clone(),
            btn_c: btn_c.clone(),
            inner: Rc::new(KeyEditorInner {
                shared: shared.clone(),
                category,
                draft: RefCell::new(BTreeMap::new()),
                rows: RefCell::new(Vec::new()),
                key_rows: RefCell::new(Vec::new()),
                picking: RefCell::new(None),
                pick_rows: RefCell::new(Vec::new()),
                pending: RefCell::new(Vec::new()),
                view_mode: Cell::new(KeyView::ByCommand),
                view: RefCell::new(Vec::new()),
                query: RefCell::new(String::new()),
                sel: Cell::new(0),
                sub: Cell::new(0),
                top: Cell::new(0),
                row_h: Cell::new(gui::dpi_y(22)),
                capturing: Cell::new(false),
                capturing_remap: Cell::new(false),
                capturing_newdef: Cell::new(false),
                status: RefCell::new(String::new()),
            }),
        };
        me.load_draft();
        me.rebuild_rows();
        me.setup_events();
        {
            let this = me.clone();
            me.search.on().en_change(move || {
                let q = this.search.text().unwrap_or_default();
                this.apply_query(&q);
                Ok(())
            });
        }
        {
            let this = me.clone();
            me.toggle.on().bn_clicked(move || {
                this.set_view(this.toggle.selected_index() == Some(1));
                Ok(())
            });
        }
        // ボタン1：機能順＝キーを追加／キー順＝機能を変更（ピッカー）。
        {
            let this = me.clone();
            btn_a.on().bn_clicked(move || {
                if this.inner.view_mode.get() == KeyView::ByCommand {
                    this.begin_capture();
                } else {
                    this.enter_pick(0);
                }
                Ok(())
            });
        }
        // ボタン2：機能順＝キーを変更（リマップ）／キー順＝キーを削除。
        {
            let this = me.clone();
            btn_b.on().bn_clicked(move || {
                if this.inner.view_mode.get() == KeyView::ByCommand {
                    this.begin_remap();
                } else {
                    this.unbind_selected();
                }
                Ok(())
            });
        }
        // ボタン3：機能順＝キー定義を削除／キー順＝キー定義を追加（空キー定義を作る）。
        {
            let this = me.clone();
            btn_c.on().bn_clicked(move || {
                if this.inner.view_mode.get() == KeyView::ByCommand {
                    this.unbind_selected();
                } else {
                    this.add_key_def();
                }
                Ok(())
            });
        }
        {
            let this = me.clone();
            reset.on().bn_clicked(move || {
                this.reset();
                Ok(())
            });
        }
        me.register_debug();
        me
    }

    /// 現在のモードに合わせて左グループ 3 ボタンのラベルを更新する。
    fn relabel_buttons(&self) {
        let (a, b, c) = match self.inner.view_mode.get() {
            KeyView::ByCommand => ("キー定義を追加(&K)", "キー定義を変更(&C)", "キー定義を削除(&D)"),
            KeyView::ByKey => ("機能定義を変更(&C)", "キー定義を削除(&D)", "キー定義を追加(&K)"),
        };
        let _ = self.btn_a.hwnd().SetWindowText(a);
        let _ = self.btn_b.hwnd().SetWindowText(b);
        let _ = self.btn_c.hwnd().SetWindowText(c);
    }

    fn hwnd(&self) -> &w::HWND {
        self.list.hwnd()
    }

    /// debug-server からこのページを観測・駆動できるようフックを登録する。実打鍵キャプチャと
    /// 同じ `assign`/`unbind_selected`/`reset` を叩く＝挙動が一本化される。
    #[cfg(feature = "debug-server")]
    fn register_debug(&self) {
        use crate::debug_server::modal_registry::{KeyEditorHooks, KeyEditorState};
        let read = {
            let this = self.clone();
            Box::new(move || {
                let picking = this.inner.picking.borrow().is_some();
                let by_key = this.inner.view_mode.get() == KeyView::ByKey;
                let view = this.inner.view.borrow();
                let rows = if picking {
                    let pr = this.inner.pick_rows.borrow();
                    view.iter()
                        .filter_map(|&i| pr.get(i))
                        .map(|c| (c.as_token().to_string(), Vec::new()))
                        .collect()
                } else if by_key {
                    let all = this.inner.key_rows.borrow();
                    view.iter()
                        .map(|&ri| {
                            let r = &all[ri];
                            (r.chord.clone(), r.labels.clone())
                        })
                        .collect()
                } else {
                    let all = this.inner.rows.borrow();
                    view.iter()
                        .map(|&ri| {
                            let r = &all[ri];
                            (
                                r.command.as_token().to_string(),
                                r.chords.iter().map(|c| c.token.clone()).collect(),
                            )
                        })
                        .collect()
                };
                KeyEditorState {
                    rows,
                    selected: this.inner.sel.get(),
                    sub: this.inner.sub.get(),
                    top: this.inner.top.get(),
                    capturing: this.inner.capturing.get(),
                    picking,
                    status: this.inner.status.borrow().clone(),
                    query: this.inner.query.borrow().clone(),
                    mode: if by_key { "key" } else { "command" }.to_string(),
                    conflicts: this.conflicts(),
                }
            }) as Box<dyn Fn() -> KeyEditorState>
        };
        let select = {
            let this = self.clone();
            Box::new(move |index: usize| {
                if index < this.inner.view.borrow().len() {
                    this.inner.sel.set(index);
                    this.inner.sub.set(0);
                    this.ensure_visible();
                    let _ = this.hwnd().InvalidateRect(None, false);
                }
            }) as Box<dyn Fn(usize)>
        };
        let bind = {
            let this = self.clone();
            Box::new(move |command: &str, chord: &str| {
                let cmd = Command::from_token(command)
                    .ok_or_else(|| format!("unknown command: {command}"))?;
                let ch = KeyChord::parse(chord)
                    .ok_or_else(|| format!("unknown chord: {chord}"))?;
                let pos = {
                    let view = this.inner.view.borrow();
                    if this.inner.view_mode.get() == KeyView::ByKey {
                        let krows = this.inner.key_rows.borrow();
                        view.iter()
                            .position(|&ri| krows[ri].labels.iter().any(|l| Command::from_token(l) == Some(cmd)))
                    } else {
                        let rows = this.inner.rows.borrow();
                        view.iter().position(|&ri| rows[ri].command == cmd)
                    }
                };
                if let Some(di) = pos {
                    this.inner.sel.set(di);
                }
                this.assign(cmd, ch);
                Ok(())
            }) as Box<dyn Fn(&str, &str) -> Result<(), String>>
        };
        let unbind = {
            let this = self.clone();
            Box::new(move || this.unbind_selected()) as Box<dyn Fn()>
        };
        let reset = {
            let this = self.clone();
            Box::new(move || this.reset()) as Box<dyn Fn()>
        };
        let search = {
            let this = self.clone();
            Box::new(move |q: &str| {
                let _ = this.search.set_text(q);
                this.apply_query(q);
            }) as Box<dyn Fn(&str)>
        };
        let set_view = {
            let this = self.clone();
            Box::new(move |by_key: bool| this.set_view(by_key)) as Box<dyn Fn(bool)>
        };
        let select_chord = {
            let this = self.clone();
            Box::new(move |index: usize| {
                if index < this.sel_chord_count() {
                    this.inner.sub.set(index);
                    let _ = this.hwnd().InvalidateRect(None, false);
                }
            }) as Box<dyn Fn(usize)>
        };
        let rebind = {
            let this = self.clone();
            Box::new(move |chord: &str| {
                let ch = KeyChord::parse(chord)
                    .ok_or_else(|| format!("unknown chord: {chord}"))?;
                this.remap_sub(ch);
                Ok(())
            }) as Box<dyn Fn(&str) -> Result<(), String>>
        };
        let pick = {
            let this = self.clone();
            Box::new(move |li: usize| this.enter_pick(li)) as Box<dyn Fn(usize)>
        };
        let pick_commit = {
            let this = self.clone();
            Box::new(move || this.commit_pick()) as Box<dyn Fn()>
        };
        let pick_cancel = {
            let this = self.clone();
            Box::new(move || this.cancel_pick()) as Box<dyn Fn()>
        };
        let scroll = {
            let this = self.clone();
            Box::new(move |top: i32| this.scroll_to(top as isize)) as Box<dyn Fn(i32)>
        };
        let add_keydef = {
            let this = self.clone();
            Box::new(move |chord: &str| {
                let ch = KeyChord::parse(chord)
                    .ok_or_else(|| format!("unknown chord: {chord}"))?;
                this.finish_newdef(ch);
                Ok(())
            }) as crate::debug_server::modal_registry::ChordFn
        };
        crate::debug_server::modal_registry::register_key_editor(
            self.inner.category.debug_str(),
            KeyEditorHooks {
                read,
                select,
                bind,
                unbind,
                reset,
                search,
                set_view,
                select_chord,
                rebind,
                pick,
                pick_commit,
                pick_cancel,
                scroll,
                add_keydef,
            },
        );
    }

    /// debug-server 無効ビルドでは何もしない。
    #[cfg(not(feature = "debug-server"))]
    fn register_debug(&self) {}

    /// `config` の当該カテゴリのキーマップへの参照。下書きの読み込み／書き戻しにのみ使う。
    fn with_cfg_map<R>(&self, f: impl FnOnce(&mut BTreeMap<String, String>) -> R) -> R {
        let mut cfg = self.inner.shared.cfg.borrow_mut();
        let map = match self.inner.category {
            KeyCategory::Filer => &mut cfg.keybinds,
            KeyCategory::TextViewer => &mut cfg.keybinds_textviewer,
            KeyCategory::ImageViewer => &mut cfg.keybinds_imageviewer,
        };
        f(map)
    }

    /// `config.keybinds` から下書きを作る。空値＝明示 unbind は空 Vec、非空は 1 要素のリスト。
    /// 未知バインドも生値のまま保持する（反映時に消さないため）。
    fn load_draft(&self) {
        let map = self.with_cfg_map(|m| m.clone());
        let mut draft: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (chord, val) in map {
            if val.trim().is_empty() {
                draft.insert(chord, Vec::new());
            } else {
                draft.insert(chord, vec![val]);
            }
        }
        *self.inner.draft.borrow_mut() = draft;
    }

    /// 下書きを `config.keybinds` へ書き戻す（衝突が無いと検証済みの時だけ呼ぶ）。
    /// 空 Vec＝unbind は空文字で残す（既定キーの打ち消し）、1 要素はその値。
    fn flush_draft(&self) {
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        for (chord, vals) in self.inner.draft.borrow().iter() {
            let first = vals.iter().find(|v| !v.trim().is_empty());
            out.insert(chord.clone(), first.cloned().unwrap_or_default());
        }
        self.with_cfg_map(|m| *m = out);
    }

    /// 衝突（1 つの chord に 2 機能以上）を chord 昇順で列挙する＝`(chord, ラベル群)`。
    fn conflicts(&self) -> Vec<(String, Vec<String>)> {
        self.inner
            .draft
            .borrow()
            .iter()
            .filter_map(|(chord, vals)| {
                let labels: Vec<String> = vals
                    .iter()
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| binding_label(v))
                    .collect();
                (labels.len() > 1).then(|| (chord.clone(), labels))
            })
            .collect()
    }

    /// 衝突が 1 つでもあるか。
    fn has_conflicts(&self) -> bool {
        self.inner.draft.borrow().values().any(|vals| {
            vals.iter().filter(|v| !v.trim().is_empty()).count() > 1
        })
    }

    /// 衝突をステータスへ書き出す（OK/適用が弾いた理由を見せる）。
    fn note_conflicts(&self) {
        let desc: Vec<String> = self
            .conflicts()
            .into_iter()
            .map(|(chord, labels)| format!("{}（{}）", chord, labels.join(", ")))
            .collect();
        *self.inner.status.borrow_mut() =
            format!("キーの重複を解決してください: {}", desc.join(" / "));
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 下書きから機能順 `rows`・キー順 `key_rows` を組み直す（空 Vec＝unbind は除く）。
    fn rebuild_rows(&self) {
        let mut by_cmd: HashMap<Command, Vec<ChordRef>> = HashMap::new();
        let mut key_rows: Vec<KeyChordRow> = Vec::new();
        {
            let draft = self.inner.draft.borrow();
            for (chord, vals) in draft.iter() {
                let nonempty: Vec<&String> =
                    vals.iter().filter(|v| !v.trim().is_empty()).collect();
                if nonempty.is_empty() {
                    continue;
                }
                let conflicted = nonempty.len() > 1;
                let labels: Vec<String> = nonempty.iter().map(|v| binding_label(v)).collect();
                key_rows.push(KeyChordRow { chord: chord.clone(), labels });
                for v in &nonempty {
                    if let Some(inv) = Invocation::parse(v) {
                        by_cmd
                            .entry(inv.command)
                            .or_default()
                            .push(ChordRef { token: chord.clone(), conflicted });
                    }
                }
            }
        }
        // 機能未割当の空キー定義（pending）をキー順に並べる（－表示）。既にバインドされたものは除く。
        {
            let bound: std::collections::HashSet<String> =
                key_rows.iter().map(|r| r.chord.clone()).collect();
            for chord in self.inner.pending.borrow().iter() {
                if !bound.contains(chord) {
                    key_rows.push(KeyChordRow { chord: chord.clone(), labels: Vec::new() });
                }
            }
        }
        let ctx = self.inner.category.context();
        let rows: Vec<KeyRow> = Command::all()
            .filter(|c| c.available_in(ctx))
            .map(|command| {
                let mut chords = by_cmd.remove(&command).unwrap_or_default();
                chords.sort_by(|a, b| a.token.cmp(&b.token));
                KeyRow { command, chords }
            })
            .collect();
        key_rows.sort_by(|a, b| a.chord.cmp(&b.chord));
        *self.inner.rows.borrow_mut() = rows;
        *self.inner.key_rows.borrow_mut() = key_rows;
        self.rebuild_view();
    }

    /// 検索クエリで現モードの行を絞り込み、表示対象 `view` を組み直す。選択を範囲内へ収める。
    /// ピックモード中は機能一覧（`pick_rows`）を絞り込む。
    fn rebuild_view(&self) {
        let q = self.inner.query.borrow().to_lowercase();
        if self.inner.picking.borrow().is_some() {
            let view: Vec<usize> = self
                .inner
                .pick_rows
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, c)| q.is_empty() || c.as_token().to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect();
            let n = view.len();
            *self.inner.view.borrow_mut() = view;
            if n == 0 {
                self.inner.sel.set(0);
                self.inner.top.set(0);
            } else if self.inner.sel.get() >= n {
                self.inner.sel.set(n - 1);
            }
            self.update_scrollbar();
            return;
        }
        let view: Vec<usize> = match self.inner.view_mode.get() {
            KeyView::ByCommand => self
                .inner
                .rows
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    q.is_empty()
                        || r.command.as_token().to_lowercase().contains(&q)
                        || r.chords.iter().any(|c| c.token.to_lowercase().contains(&q))
                })
                .map(|(i, _)| i)
                .collect(),
            KeyView::ByKey => self
                .inner
                .key_rows
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    q.is_empty()
                        || r.chord.to_lowercase().contains(&q)
                        || r.labels.iter().any(|l| l.to_lowercase().contains(&q))
                })
                .map(|(i, _)| i)
                .collect(),
        };
        let n = view.len();
        *self.inner.view.borrow_mut() = view;
        if n == 0 {
            self.inner.sel.set(0);
            self.inner.top.set(0);
        } else if self.inner.sel.get() >= n {
            self.inner.sel.set(n - 1);
        }
        let cc = self.sel_chord_count();
        if self.inner.sub.get() >= cc {
            self.inner.sub.set(cc.saturating_sub(1));
        }
        self.update_scrollbar();
    }

    /// 検索クエリを適用して表示を絞り込む（`config` は変更しない）。同じ値なら何もしない。
    fn apply_query(&self, q: &str) {
        {
            let mut cur = self.inner.query.borrow_mut();
            if *cur == q {
                return;
            }
            *cur = q.to_string();
        }
        self.inner.sel.set(0);
        self.inner.top.set(0);
        self.rebuild_view();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 並べ方を切り替える（同じなら何もしない）。キャプチャ中なら中断する。
    fn set_view(&self, by_key: bool) {
        let mode = if by_key { KeyView::ByKey } else { KeyView::ByCommand };
        if self.inner.view_mode.get() == mode {
            return;
        }
        self.inner.view_mode.set(mode);
        self.inner.capturing.set(false);
        self.inner.sel.set(0);
        self.inner.top.set(0);
        self.toggle[0].select(!by_key);
        self.toggle[1].select(by_key);
        self.relabel_buttons();
        self.rebuild_view();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 選択行のコマンド（機能順＝その行の機能／キー順＝その chord を定義する先頭の既知機能）。
    fn selected_command(&self) -> Option<Command> {
        let ri = *self.inner.view.borrow().get(self.inner.sel.get())?;
        match self.inner.view_mode.get() {
            KeyView::ByCommand => self.inner.rows.borrow().get(ri).map(|r| r.command),
            KeyView::ByKey => self
                .inner
                .key_rows
                .borrow()
                .get(ri)
                .and_then(|r| r.labels.iter().find_map(|l| Command::from_token(l))),
        }
    }

    /// キー順で選択中の chord（機能順では `None`）。
    fn selected_chord(&self) -> Option<String> {
        if self.inner.view_mode.get() != KeyView::ByKey {
            return None;
        }
        let ri = *self.inner.view.borrow().get(self.inner.sel.get())?;
        self.inner.key_rows.borrow().get(ri).map(|r| r.chord.clone())
    }

    /// 機能順で選択行のキー数。
    fn sel_chord_count(&self) -> usize {
        if self.inner.view_mode.get() != KeyView::ByCommand {
            return 0;
        }
        let Some(&ri) = self.inner.view.borrow().get(self.inner.sel.get()) else {
            return 0;
        };
        self.inner.rows.borrow().get(ri).map(|r| r.chords.len()).unwrap_or(0)
    }

    /// 機能順でサブ選択中のキー（範囲外・キー順では `None`）。
    fn sub_chord(&self) -> Option<String> {
        if self.inner.view_mode.get() != KeyView::ByCommand {
            return None;
        }
        let ri = *self.inner.view.borrow().get(self.inner.sel.get())?;
        let rows = self.inner.rows.borrow();
        rows.get(ri)?.chords.get(self.inner.sub.get()).map(|c| c.token.clone())
    }

    /// 機能順でサブ選択を左右に動かす。
    fn move_sub(&self, dir: isize) {
        let n = self.sel_chord_count() as isize;
        if n <= 1 {
            return;
        }
        let i = (self.inner.sub.get() as isize + dir).clamp(0, n - 1);
        self.inner.sub.set(i as usize);
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// クリック x が機能順・指定表示行のどのキーに当たるか（行内のキーを実測して判定）。
    fn chord_hit(&self, row_di: usize, x: i32) -> Option<usize> {
        if self.inner.view_mode.get() != KeyView::ByCommand {
            return None;
        }
        let ri = *self.inner.view.borrow().get(row_di)?;
        let chords: Vec<(String, bool)> = self
            .inner
            .rows
            .borrow()
            .get(ri)?
            .chords
            .iter()
            .map(|c| (c.token.clone(), c.conflicted))
            .collect();
        if chords.is_empty() {
            return None;
        }
        let dc = self.hwnd().GetDC().ok()?;
        let font = w::HFONT::GetStockObject(co::STOCK_FONT::DEFAULT_GUI).ok()?;
        let _sel = dc.SelectObject(&font).ok()?;
        let mut cx = gui::dpi_x(260);
        let sep = dc.GetTextExtentPoint32(", ").map(|z| z.cx).unwrap_or(0);
        for (ci, (tok, conflicted)) in chords.iter().enumerate() {
            let mut label = tok.clone();
            if *conflicted {
                label.push_str(" ⚠");
            }
            let tw = dc.GetTextExtentPoint32(&label).map(|z| z.cx).unwrap_or(0);
            if x >= cx && x < cx + tw {
                return Some(ci);
            }
            cx += tw + sep;
        }
        None
    }

    /// クリック x がキー順・指定表示行のどの機能ラベルに当たるか（右カラムを実測）。
    fn label_hit(&self, row_di: usize, x: i32) -> Option<usize> {
        if self.inner.view_mode.get() != KeyView::ByKey {
            return None;
        }
        let ri = *self.inner.view.borrow().get(row_di)?;
        let labels: Vec<String> = self.inner.key_rows.borrow().get(ri)?.labels.clone();
        if labels.is_empty() {
            return None;
        }
        let dc = self.hwnd().GetDC().ok()?;
        let font = w::HFONT::GetStockObject(co::STOCK_FONT::DEFAULT_GUI).ok()?;
        let _sel = dc.SelectObject(&font).ok()?;
        let mut cx = gui::dpi_x(260);
        let sep = dc.GetTextExtentPoint32(", ").map(|z| z.cx).unwrap_or(0);
        for (li, lab) in labels.iter().enumerate() {
            let tw = dc.GetTextExtentPoint32(lab).map(|z| z.cx).unwrap_or(0);
            if x >= cx && x < cx + tw {
                return Some(li);
            }
            cx += tw + sep;
        }
        None
    }

    /// キー順で選択行の li 番目の機能を「別機能へ差し替える」ピックモードへ入る（インライン）。
    fn enter_pick(&self, li: usize) {
        if self.inner.view_mode.get() != KeyView::ByKey
            || self.inner.capturing.get()
            || self.inner.picking.borrow().is_some()
        {
            return;
        }
        let Some(&ri) = self.inner.view.borrow().get(self.inner.sel.get()) else {
            return;
        };
        let chord = match self.inner.key_rows.borrow().get(ri) {
            Some(r) => r.chord.clone(),
            None => return,
        };
        // 空キー定義（pending）なら old_value は空＝置換でなく新規割り当て。
        let old_value = {
            let draft = self.inner.draft.borrow();
            let nonempty: Vec<String> = draft
                .get(&chord)
                .map(|vs| vs.iter().filter(|v| !v.trim().is_empty()).cloned().collect())
                .unwrap_or_default();
            nonempty.get(li).cloned().unwrap_or_default()
        };
        let old_label = if old_value.is_empty() {
            "－".to_string()
        } else {
            binding_label(&old_value)
        };
        let ctx = self.inner.category.context();
        *self.inner.pick_rows.borrow_mut() =
            Command::all().filter(|c| c.available_in(ctx)).collect();
        *self.inner.picking.borrow_mut() = Some(PickState {
            chord: chord.clone(),
            old_value,
        });
        self.inner.sel.set(0);
        self.inner.top.set(0);
        *self.inner.query.borrow_mut() = String::new();
        let _ = self.search.set_text("");
        *self.inner.status.borrow_mut() = format!(
            "{} に割り当てる機能を選択（{} を置換・右クリック/Escで中止）",
            chord, old_label
        );
        self.rebuild_view();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// ピックモードで選択中の機能を確定し、対象キーの定義を差し替える。
    fn commit_pick(&self) {
        let Some(pick) = self.inner.picking.borrow_mut().take() else {
            return;
        };
        let new_cmd = self
            .inner
            .view
            .borrow()
            .get(self.inner.sel.get())
            .and_then(|&i| self.inner.pick_rows.borrow().get(i).copied());
        if let Some(cmd) = new_cmd {
            let new_val = Invocation::bare(cmd).to_token_string();
            {
                let mut draft = self.inner.draft.borrow_mut();
                let vals = draft.entry(pick.chord.clone()).or_default();
                // 置換対象（old_value）があれば外す。空キー定義なら新規割り当て。
                if !pick.old_value.is_empty() {
                    vals.retain(|v| *v != pick.old_value);
                }
                if !vals.iter().any(|v| Invocation::parse(v).map(|i| i.command) == Some(cmd)) {
                    vals.push(new_val);
                }
            }
            // 機能が付いたので pending（空キー定義）からは外す。
            self.inner.pending.borrow_mut().retain(|c| *c != pick.chord);
            *self.inner.status.borrow_mut() =
                format!("{} を {} に割り当てました", pick.chord, cmd.as_token());
        }
        self.exit_pick_common();
    }

    /// ピックモードを中止して元のキー一覧へ戻る。
    fn cancel_pick(&self) {
        if self.inner.picking.borrow_mut().take().is_none() {
            return;
        }
        *self.inner.status.borrow_mut() = "中止しました".to_string();
        self.exit_pick_common();
    }

    /// ピック解除の後始末（検索クリア・選択リセット・再構築・再描画）。
    fn exit_pick_common(&self) {
        self.inner.pick_rows.borrow_mut().clear();
        *self.inner.query.borrow_mut() = String::new();
        let _ = self.search.set_text("");
        self.inner.sel.set(0);
        self.inner.top.set(0);
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// サブ選択キーの「変更（リマップ）」キャプチャを始める（次の打鍵で旧キー→新キーへ移す）。
    fn begin_remap(&self) {
        if self.inner.picking.borrow().is_some()
            || self.selected_command().is_none()
            || self.sub_chord().is_none()
        {
            return;
        }
        self.inner.capturing.set(true);
        self.inner.capturing_remap.set(true);
        *self.inner.status.borrow_mut() = "新しいキーを押してください（右クリックで中止）".to_string();
        self.hwnd().SetFocus();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// サブ選択キーをその機能のまま新しいキーへ移し替える（旧キーから当該機能を外す）。
    fn remap_sub(&self, new: KeyChord) {
        let Some(command) = self.selected_command() else {
            return;
        };
        let Some(old) = self.sub_chord() else {
            return;
        };
        let Some(new_tok) = new.to_token() else {
            *self.inner.status.borrow_mut() = "未対応のキーです".to_string();
            let _ = self.hwnd().InvalidateRect(None, false);
            return;
        };
        if new_tok == old {
            return;
        }
        let value = Invocation::bare(command).to_token_string();
        {
            let mut draft = self.inner.draft.borrow_mut();
            if let Some(vals) = draft.get_mut(&old) {
                vals.retain(|v| Invocation::parse(v).map(|i| i.command) != Some(command));
            }
            let e = draft.entry(new_tok.clone()).or_default();
            e.retain(|v| !v.trim().is_empty());
            if !e.iter().any(|v| Invocation::parse(v).map(|i| i.command) == Some(command)) {
                e.push(value);
            }
        }
        *self.inner.status.borrow_mut() =
            format!("{} を {} から {} に変更しました", command.as_token(), old, new_tok);
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// chord を選択行のコマンドへ割り当てる。**既存の機能は消さず追記する**＝同じ chord に
    /// 別機能があれば衝突になる（マークして OK/適用で解決を促す）。
    fn assign(&self, command: Command, chord: KeyChord) {
        let Some(tok) = chord.to_token() else {
            *self.inner.status.borrow_mut() = "未対応のキーです".to_string();
            return;
        };
        let value = Invocation::bare(command).to_token_string();
        let conflict = {
            let mut draft = self.inner.draft.borrow_mut();
            let e = draft.entry(tok.clone()).or_default();
            // 空 unbind マーカーは取り除く（今バインドし直すので）。
            e.retain(|v| !v.trim().is_empty());
            // 同じ機能が既にこの chord にあるなら追記しない。
            let already = e
                .iter()
                .any(|v| Invocation::parse(v).map(|i| i.command) == Some(command));
            if !already {
                e.push(value);
            }
            e.len() > 1
        };
        *self.inner.status.borrow_mut() = if conflict {
            format!("{} に {} を割り当て（このキーは衝突しています）", command.as_token(), tok)
        } else {
            format!("{} に {} を割り当てました", command.as_token(), tok)
        };
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 選択行の割り当てを解除する（既定キーは空 Vec で打ち消す＝差分保存で永続）。
    /// 機能順＝サブ選択中の1キーからその機能を外す／キー順＝選択中のその1キー（定義を丸ごと）。
    fn unbind_selected(&self) {
        if self.inner.picking.borrow().is_some() {
            return;
        }
        let status = match self.inner.view_mode.get() {
            KeyView::ByCommand => {
                let Some(command) = self.selected_command() else {
                    return;
                };
                let Some(chord) = self.sub_chord() else {
                    *self.inner.status.borrow_mut() = "割り当てがありません".to_string();
                    return;
                };
                // サブ選択キーからその機能だけ取り除く（同キーの他機能＝衝突分は残す）。
                if let Some(vals) = self.inner.draft.borrow_mut().get_mut(&chord) {
                    vals.retain(|v| Invocation::parse(v).map(|i| i.command) != Some(command));
                }
                format!("{} から {} を解除しました", chord, command.as_token())
            }
            KeyView::ByKey => {
                let Some(chord) = self.selected_chord() else {
                    return;
                };
                // そのキー定義を丸ごと外す＝空 Vec（unbind マーカー）にする。pending なら消すだけ。
                let was_pending = self.inner.pending.borrow().contains(&chord);
                self.inner.pending.borrow_mut().retain(|c| *c != chord);
                if !was_pending {
                    self.inner.draft.borrow_mut().insert(chord.clone(), Vec::new());
                }
                format!("{} の割り当てを解除しました", chord)
            }
        };
        *self.inner.status.borrow_mut() = status;
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// このページを既定キーマップへ戻す。
    fn reset(&self) {
        if self.inner.picking.borrow().is_some() {
            return;
        }
        let def = self.inner.category.default_map();
        let draft: BTreeMap<String, Vec<String>> = def
            .into_iter()
            .map(|(chord, val)| {
                if val.trim().is_empty() {
                    (chord, Vec::new())
                } else {
                    (chord, vec![val])
                }
            })
            .collect();
        *self.inner.draft.borrow_mut() = draft;
        self.inner.pending.borrow_mut().clear();
        *self.inner.status.borrow_mut() = "既定に戻しました".to_string();
        self.inner.capturing.set(false);
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// キャプチャ開始（次の打鍵を選択行へ割り当てる）。リストへフォーカスを移す。
    fn begin_capture(&self) {
        if self.inner.picking.borrow().is_some() || self.selected_command().is_none() {
            return;
        }
        self.inner.capturing.set(true);
        *self.inner.status.borrow_mut() = "キーを押してください（右クリックで中止）".to_string();
        self.hwnd().SetFocus();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    fn cancel_capture(&self) {
        self.inner.capturing.set(false);
        self.inner.capturing_remap.set(false);
        self.inner.capturing_newdef.set(false);
        *self.inner.status.borrow_mut() = "中止しました".to_string();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// キー順で「キー定義を追加」：次の打鍵で機能未割当の空キー定義（－）を作る。
    fn add_key_def(&self) {
        if self.inner.view_mode.get() != KeyView::ByKey
            || self.inner.picking.borrow().is_some()
            || self.inner.capturing.get()
        {
            return;
        }
        self.inner.capturing.set(true);
        self.inner.capturing_newdef.set(true);
        *self.inner.status.borrow_mut() =
            "追加するキーを押してください（右クリックで中止）".to_string();
        self.hwnd().SetFocus();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 空キー定義を作る（機能は後で割り当て）。既にバインド済みなら何もせずその行を選ぶ。
    fn finish_newdef(&self, chord: KeyChord) {
        let Some(tok) = chord.to_token() else {
            *self.inner.status.borrow_mut() = "未対応のキーです".to_string();
            let _ = self.hwnd().InvalidateRect(None, false);
            return;
        };
        let bound = self
            .inner
            .draft
            .borrow()
            .get(&tok)
            .is_some_and(|vs| vs.iter().any(|v| !v.trim().is_empty()));
        if bound {
            *self.inner.status.borrow_mut() = format!("{} は既に使われています", tok);
        } else {
            let mut pending = self.inner.pending.borrow_mut();
            if !pending.contains(&tok) {
                pending.push(tok.clone());
            }
            drop(pending);
            *self.inner.status.borrow_mut() =
                format!("空のキー定義 {} を追加（機能を割り当ててください）", tok);
        }
        self.rebuild_rows();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    /// 1 画面に収まる行数（最下部のステータス行 1 行を除いた本文領域）。
    fn visible_rows(&self) -> usize {
        let ch = self.hwnd().GetClientRect().map(|r| r.bottom - r.top).unwrap_or(0);
        let rh = self.inner.row_h.get().max(1);
        ((ch - rh) / rh).max(1) as usize
    }

    /// 縦スクロールバーを現在の行数・表示位置に合わせる。
    fn update_scrollbar(&self) {
        if self.hwnd().GetClientRect().is_err() {
            return;
        }
        let n = self.inner.view.borrow().len();
        let vis = self.visible_rows();
        let mut si = w::SCROLLINFO::default();
        si.fMask = co::SIF::RANGE | co::SIF::PAGE | co::SIF::POS;
        si.nMin = 0;
        si.nMax = (n as i32 - 1).max(0);
        si.nPage = vis as u32;
        si.nPos = self.inner.top.get() as i32;
        self.hwnd().SetScrollInfo(co::SBB::VERT, &si, true);
    }

    /// 表示先頭行を動かす（範囲内へクランプ・スクロールバーと再描画も更新）。選択は動かさない。
    fn scroll_to(&self, new_top: isize) {
        let n = self.inner.view.borrow().len();
        let vis = self.visible_rows();
        let max_top = n.saturating_sub(vis) as isize;
        let top = new_top.clamp(0, max_top) as usize;
        if top != self.inner.top.get() {
            self.inner.top.set(top);
            self.update_scrollbar();
            let _ = self.hwnd().InvalidateRect(None, false);
        }
    }

    /// 選択が見える位置までスクロールを調整する。
    fn ensure_visible(&self) {
        let sel = self.inner.sel.get();
        let vis = self.visible_rows();
        let mut top = self.inner.top.get();
        if sel < top {
            top = sel;
        } else if sel >= top + vis {
            top = sel + 1 - vis;
        }
        self.inner.top.set(top);
        self.update_scrollbar();
    }

    fn move_sel(&self, dir: isize) {
        let n = self.inner.view.borrow().len() as isize;
        if n == 0 {
            return;
        }
        let i = (self.inner.sel.get() as isize + dir).clamp(0, n - 1);
        self.inner.sel.set(i as usize);
        self.inner.sub.set(0);
        self.ensure_visible();
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    fn setup_events(&self) {
        let this = self.clone();
        self.list.on().wm_get_dlg_code(move |_| {
            let flags = if this.inner.capturing.get() || this.inner.picking.borrow().is_some() {
                co::DLGC::WANTALLKEYS.raw()
            } else {
                co::DLGC::WANTARROWS.raw()
            };
            Ok(unsafe { co::DLGC::from_raw(flags) })
        });

        let this = self.clone();
        self.list.on().wm_paint(move || this.on_paint());

        let this = self.clone();
        self.list.on().wm(unsafe { co::WM::from_raw(WM_PRINTCLIENT) }, move |p| {
            this.on_print(p.wparam);
            Ok(0)
        });

        let this = self.clone();
        self.list.on().wm_set_focus(move |_| {
            let _ = this.hwnd().InvalidateRect(None, false);
            Ok(())
        });
        let this = self.clone();
        self.list.on().wm_kill_focus(move |_| {
            let _ = this.hwnd().InvalidateRect(None, false);
            Ok(())
        });

        let this = self.clone();
        self.list.on().wm_l_button_down(move |p| {
            this.hwnd().SetFocus();
            // キャプチャ中の左クリックは無視（中止は右クリック・左は「決定」感を避ける）。
            if this.inner.capturing.get() {
                return Ok(());
            }
            let rh = this.inner.row_h.get().max(1);
            let row = this.inner.top.get() + (p.coords.y / rh) as usize;
            if row < this.inner.view.borrow().len() {
                this.inner.sel.set(row);
                // ピック中は行選択のみ。通常はクリックしたキーをサブ選択（行内 hit-test）。
                let sub = if this.inner.picking.borrow().is_some() {
                    0
                } else {
                    this.chord_hit(row, p.coords.x).unwrap_or(0)
                };
                this.inner.sub.set(sub);
                this.ensure_visible();
                let _ = this.hwnd().InvalidateRect(None, false);
            }
            Ok(())
        });

        // ダブルクリック：機能順=キーを「変更」キャプチャへ／キー順=機能を機能ピッカーへ／
        // ピック中=その機能で確定。
        let this = self.clone();
        self.list.on().wm_l_button_dbl_clk(move |p| {
            if this.inner.capturing.get() {
                return Ok(());
            }
            let rh = this.inner.row_h.get().max(1);
            let row = this.inner.top.get() + (p.coords.y / rh) as usize;
            if row >= this.inner.view.borrow().len() {
                return Ok(());
            }
            this.inner.sel.set(row);
            if this.inner.picking.borrow().is_some() {
                this.commit_pick();
                return Ok(());
            }
            match this.inner.view_mode.get() {
                KeyView::ByCommand => {
                    if let Some(ci) = this.chord_hit(row, p.coords.x) {
                        this.inner.sub.set(ci);
                        this.ensure_visible();
                        this.begin_remap();
                        return Ok(());
                    }
                }
                KeyView::ByKey => {
                    // 機能ラベル上なら そのラベルを差替へ。空キー定義（－）行なら新規割り当てへ。
                    let li = this.label_hit(row, p.coords.x).or_else(|| {
                        let empty = this
                            .inner
                            .view
                            .borrow()
                            .get(row)
                            .and_then(|&ri| this.inner.key_rows.borrow().get(ri).map(|r| r.labels.is_empty()))
                            .unwrap_or(false);
                        empty.then_some(0)
                    });
                    if let Some(li) = li {
                        this.ensure_visible();
                        this.enter_pick(li);
                        return Ok(());
                    }
                }
            }
            this.inner.sub.set(0);
            this.ensure_visible();
            let _ = this.hwnd().InvalidateRect(None, false);
            Ok(())
        });

        // 右クリック：キャプチャ中／ピック中なら中止（左クリックは「決定」感があるので使わない）。
        let this = self.clone();
        self.list.on().wm_r_button_down(move |_| {
            if this.inner.capturing.get() {
                this.cancel_capture();
            } else if this.inner.picking.borrow().is_some() {
                this.cancel_pick();
            }
            Ok(())
        });

        // 縦スクロールバー：行単位で表示位置を動かす（選択は動かさない）。
        let this = self.clone();
        self.list.on().wm_v_scroll(move |p| {
            let cur = this.inner.top.get() as isize;
            let vis = this.visible_rows() as isize;
            let n = this.inner.view.borrow().len() as isize;
            let new = match p.request {
                co::SB_REQ::LINEUP => cur - 1,
                co::SB_REQ::LINEDOWN => cur + 1,
                co::SB_REQ::PAGEUP => cur - vis,
                co::SB_REQ::PAGEDOWN => cur + vis,
                co::SB_REQ::THUMBPOSITION | co::SB_REQ::THUMBTRACK => p.scroll_box_pos as isize,
                co::SB_REQ::TOP => 0,
                co::SB_REQ::BOTTOM => n,
                _ => cur,
            };
            this.scroll_to(new);
            Ok(())
        });

        // マウスホイール：3 行ずつスクロール（winsafe 0.0.27 は回転量が keys に入る）。
        let this = self.clone();
        self.list.on().wm_mouse_wheel(move |p| {
            let dist = p.keys.raw() as i16 as i32;
            let lines = (dist / 120) * 3;
            this.scroll_to(this.inner.top.get() as isize - lines as isize);
            Ok(())
        });

        let this = self.clone();
        self.list.on().wm_key_down(move |p| {
            this.on_key(p.vkey_code.raw());
            Ok(())
        });
    }

    /// キー入力処理。キャプチャ中は打鍵を chord 化して割り当て（中止はクリック＝ESC も
    /// 普通にキャプチャできる）、通常時は ↑↓ で行移動・←→ でキーのサブ選択。
    fn on_key(&self, vk: u16) {
        use rerics_core::vk as k;
        if self.inner.capturing.get() {
            // 修飾キー単体（Shift/Ctrl/Alt の VK）は確定打鍵としない（実キーが来るまで待つ）。
            if matches!(vk, 0x10..=0x12) {
                return;
            }
            let ctrl = w::GetAsyncKeyState(co::VK::CONTROL);
            let shift = w::GetAsyncKeyState(co::VK::SHIFT);
            let alt = w::GetAsyncKeyState(co::VK::MENU);
            let chord = KeyChord::new(vk, ctrl, shift, alt);
            self.inner.capturing.set(false);
            if self.inner.capturing_newdef.replace(false) {
                self.finish_newdef(chord);
            } else if self.inner.capturing_remap.replace(false) {
                self.remap_sub(chord);
            } else if let Some(command) = self.selected_command() {
                self.assign(command, chord);
            }
            return;
        }
        let vis = self.visible_rows() as isize;
        // ピックモード：↑↓/PageUp/Down/Home/End で機能を選び、Enter で確定・Esc で中止。
        if self.inner.picking.borrow().is_some() {
            match vk {
                k::UP => self.move_sel(-1),
                k::DOWN => self.move_sel(1),
                k::PRIOR => self.move_sel(-vis),
                k::NEXT => self.move_sel(vis),
                k::HOME => self.move_sel(isize::MIN / 2),
                k::END => self.move_sel(isize::MAX / 2),
                k::RETURN => self.commit_pick(),
                k::ESCAPE => self.cancel_pick(),
                _ => {}
            }
            return;
        }
        if vk == k::UP {
            self.move_sel(-1);
        } else if vk == k::DOWN {
            self.move_sel(1);
        } else if vk == k::PRIOR {
            self.move_sel(-vis);
        } else if vk == k::NEXT {
            self.move_sel(vis);
        } else if vk == k::HOME {
            self.move_sel(isize::MIN / 2);
        } else if vk == k::END {
            self.move_sel(isize::MAX / 2);
        } else if vk == k::LEFT {
            self.move_sub(-1);
        } else if vk == k::RIGHT {
            self.move_sub(1);
        }
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

        // ピックモード（機能ピッカー表示）中は背景色を変えて「別モード」を一目で分かるようにする。
        let picking = self.inner.picking.borrow().is_some();
        let bg = w::HBRUSH::GetSysColorBrush(co::COLOR::WINDOW)?;
        let fill = if picking {
            w::HBRUSH::GetSysColorBrush(co::COLOR::INFOBK)?
        } else {
            w::HBRUSH::GetSysColorBrush(co::COLOR::WINDOW)?
        };
        dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &fill)?;

        let text_col = w::GetSysColor(co::COLOR::WINDOWTEXT);
        let gray_col = w::GetSysColor(co::COLOR::GRAYTEXT);
        let hl_text = w::GetSysColor(co::COLOR::HIGHLIGHTTEXT);
        let hl_bg = w::HBRUSH::GetSysColorBrush(co::COLOR::HIGHLIGHT)?;

        let view = self.inner.view.borrow();
        let top = self.inner.top.get();
        let sel = self.inner.sel.get();
        let capturing = self.inner.capturing.get();
        let key_x = gui::dpi_x(8);
        let chord_x = gui::dpi_x(260);
        // 最下部 1 行はステータス（操作結果・衝突メッセージ）に充てる。
        let body_h = (ch - row_h).max(row_h);
        let vis = (body_h / row_h).max(1) as usize;
        // 可視範囲の各行を現モードで (左, 右, 右を淡色表示するか) に文字列化する。衝突は ⚠ を付す。
        // 機能順＝(機能, キー群 or "—"), キー順＝(キー, 全機能ラベル or －)。
        let lines: Vec<(String, String, bool)> = if picking {
            // ピックモード：機能一覧（pick_rows を view で絞ったもの）を並べる。
            let pr = self.inner.pick_rows.borrow();
            (0..vis)
                .filter_map(|vi| {
                    view.get(top + vi)
                        .and_then(|&i| pr.get(i))
                        .map(|c| (c.as_token().to_string(), String::new(), false))
                })
                .collect()
        } else {
            match self.inner.view_mode.get() {
            KeyView::ByCommand => {
                let rows = self.inner.rows.borrow();
                (0..vis)
                    .filter_map(|vi| {
                        view.get(top + vi).map(|&ri| {
                            let r = &rows[ri];
                            if r.chords.is_empty() {
                                (r.command.as_token().to_string(), "—".to_string(), true)
                            } else {
                                let joined = r
                                    .chords
                                    .iter()
                                    .map(|c| {
                                        if c.conflicted {
                                            format!("{} ⚠", c.token)
                                        } else {
                                            c.token.clone()
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                (r.command.as_token().to_string(), joined, false)
                            }
                        })
                    })
                    .collect()
            }
            KeyView::ByKey => {
                let krows = self.inner.key_rows.borrow();
                (0..vis)
                    .filter_map(|vi| {
                        view.get(top + vi).map(|&ri| {
                            let r = &krows[ri];
                            if r.labels.is_empty() {
                                // 機能未割当の空キー定義は － を淡色で（機能を割り当ててください）。
                                (r.chord.clone(), "－".to_string(), true)
                            } else {
                                let mut right = r.labels.join(", ");
                                if r.labels.len() > 1 {
                                    right.push_str(" ⚠");
                                }
                                (r.chord.clone(), right, false)
                            }
                        })
                    })
                    .collect()
            }
            }
        };
        // 機能順・選択行のキー群（サブ選択のチップ描画・ヒットテスト用）。
        let sub = self.inner.sub.get();
        let sel_chords: Vec<(String, bool)> = if !picking
            && self.inner.view_mode.get() == KeyView::ByCommand
        {
            view.get(sel)
                .and_then(|&ri| {
                    self.inner
                        .rows
                        .borrow()
                        .get(ri)
                        .map(|r| r.chords.iter().map(|c| (c.token.clone(), c.conflicted)).collect())
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for (vi, (left, right, muted)) in lines.iter().enumerate() {
            let di = top + vi;
            let y = vi as i32 * row_h;
            let ty = y + (row_h - fh) / 2;
            if di == sel {
                dc.FillRect(w::RECT { left: 0, top: y, right: cw, bottom: y + row_h }, &hl_bg)?;
                dc.SetTextColor(hl_text)?;
            } else {
                dc.SetTextColor(text_col)?;
            }
            dc.TextOut(key_x, ty, left)?;
            if di == sel && capturing {
                dc.TextOut(chord_x, ty, "← キーを押してください（右クリックで中止）")?;
            } else if di == sel && !sel_chords.is_empty() {
                // 選択行のキーを個別に描く。サブ選択は WINDOW 地のチップで強調する。
                let mut x = chord_x;
                for (ci, (tok, conflicted)) in sel_chords.iter().enumerate() {
                    let mut label = tok.clone();
                    if *conflicted {
                        label.push_str(" ⚠");
                    }
                    let tw = dc.GetTextExtentPoint32(&label).map(|z| z.cx).unwrap_or(0);
                    if ci == sub {
                        dc.FillRect(
                            w::RECT {
                                left: x - gui::dpi_x(3),
                                top: y + gui::dpi_y(1),
                                right: x + tw + gui::dpi_x(3),
                                bottom: y + row_h - gui::dpi_y(1),
                            },
                            &bg,
                        )?;
                        dc.SetTextColor(text_col)?;
                        dc.TextOut(x, ty, &label)?;
                        dc.SetTextColor(hl_text)?;
                    } else {
                        dc.TextOut(x, ty, &label)?;
                    }
                    x += tw;
                    if ci + 1 < sel_chords.len() {
                        dc.TextOut(x, ty, ", ")?;
                        x += dc.GetTextExtentPoint32(", ").map(|z| z.cx).unwrap_or(0);
                    }
                }
            } else {
                if *muted && di != sel {
                    dc.SetTextColor(gray_col)?;
                }
                dc.TextOut(chord_x, ty, right)?;
            }
        }
        // 最下部のステータス行（薄い区切り線＋直近メッセージ）。
        let sep_y = ch - row_h;
        let sep_brush = w::HBRUSH::GetSysColorBrush(co::COLOR::BTNSHADOW)?;
        dc.FillRect(w::RECT { left: 0, top: sep_y, right: cw, bottom: sep_y + 1 }, &sep_brush)?;
        let status = self.inner.status.borrow();
        if !status.is_empty() {
            dc.SetTextColor(text_col)?;
            dc.TextOut(key_x, sep_y + (row_h - fh) / 2, &status)?;
        }
        Ok(())
    }

    /// window 生成後の初期化（ボタン整形・スクロール調整・再描画）。
    fn populate(&self) {
        self.relabel_buttons();
        self.ensure_visible();
        let _ = self.hwnd().InvalidateRect(None, false);
    }
}

/// 設定ダイアログを表示する。`OK`／`適用` で確定した [`Config`] を `on_apply` へ渡す
/// （`適用` は閉じずに継続、`OK` は閉じる。`キャンセル` は破棄して閉じる）。
pub fn show(parent: &impl GuiParent, current: &Config, on_apply: impl Fn(&Config) + 'static) {
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
    let keys = KeyEditor::new(&pane_keys, &shared, KeyCategory::Filer);
    let keys_text = KeyEditor::new(&pane_keys_text, &shared, KeyCategory::TextViewer);
    let keys_image = KeyEditor::new(&pane_keys_image, &shared, KeyCategory::ImageViewer);

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
        let columns_editor = columns_editor.clone();
        arm.on_create(move |_| {
            // 初期表示：先頭ページ（pane 0）を出し、ナビへフォーカスを与える。
            let init = nav.selected_pane();
            for (i, p) in panes.iter().enumerate() {
                p.hwnd().ShowWindow(if i == init { co::SW::SHOW } else { co::SW::HIDE });
            }
            nav.hwnd().SetFocus();
            registered.populate();
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
    }

    // キー編集の検証＋反映：3 ページのどれかにキー重複（衝突）があれば反映せず false を返し
    // （理由は各ページのステータスへ）、無ければ下書きを config へ書き戻して true。
    let validate_and_flush: Rc<dyn Fn() -> bool> = {
        let editors = vec![keys.clone(), keys_text.clone(), keys_image.clone()];
        Rc::new(move || {
            let mut ok = true;
            for e in &editors {
                if e.has_conflicts() {
                    e.note_conflicts();
                    ok = false;
                }
            }
            if ok {
                for e in &editors {
                    e.flush_draft();
                }
            }
            ok
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
    let _ = (nav, panes, keys, keys_text, keys_image, registered, ok, cancel, apply, preview_label, preview, viewer_preview);
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
