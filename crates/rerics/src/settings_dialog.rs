//! 設定ダイアログ。左ナビ（ツリー：外観＝テーマ・フォント／配色／レイアウト、動作＝カーソル、
//! キー）と中央の詳細 pane、外観カテゴリ選択中だけ右へ出す「ミニ全体窓」プレビューの構成。
//!
//! 編集中の値はすべて [`Shared`] へ即時反映し、配色・フォント・レイアウト寸法・テーマの
//! 変更はその場でプレビューへ反映する（ライブプレビュー）。`OK`／`適用` で現在の [`Config`]
//! を `on_apply` コールバックへ渡し（呼び出し側がライブ反映＋差分保存する）、`適用` は閉じずに
//! 継続、`OK` は閉じる。`キャンセル` は最後の `適用` 以降の編集を破棄して閉じる。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{
    Bookmark, Colors, Column, ColumnKind, Config, IconSize, Layout, Rgb, ResolvedTheme,
    SizeFormat, SortType, Theme, WheelAction,
};
use winsafe::{self as w, co, gui, msg::lb, msg::tvm, prelude::*};

/// 自前描画コントロールをオフスクリーン DC へ描かせるメッセージ。`PrintWindow`
/// （デバッグ制御サーバの `/snapshot/modal`）が子ごとに送るので、これに応答しないと黒く写る。
const WM_PRINTCLIENT: u32 = 0x0318;

/// 配色テーブルの行ラベルと、`Colors` の各色への get/set（表示順）。
#[allow(clippy::type_complexity)]
const COLOR_FIELDS: &[(&str, fn(&Colors) -> Rgb, fn(&mut Colors, Rgb))] = &[
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

/// レイアウト寸法フィールドのラベルと get/set（すべて論理 px）。
#[allow(clippy::type_complexity)]
const LAYOUT_FIELDS: &[(&str, fn(&Layout) -> i32, fn(&mut Layout, i32))] = &[
    ("余白", |l| l.margin, |l, v| l.margin = v),
    ("ペイン間隔", |l| l.gap, |l, v| l.gap = v),
    ("パスバー高", |l| l.bar_height, |l, v| l.bar_height = v),
    ("パスバー間隔", |l| l.bar_gap, |l, v| l.bar_gap = v),
    ("ステータス高", |l| l.status_bar_height, |l, v| l.status_bar_height = v),
    ("タブ高", |l| l.tab_height, |l, v| l.tab_height = v),
    ("ログ高", |l| l.log_height, |l, v| l.log_height = v),
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
fn list_font(family: &str, size: i32) -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
    w::HFONT::CreateFont(
        w::SIZE { cx: 0, cy: -gui::dpi_y(size) },
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
        fill(dc, tr, list_y, x + w, (list_y + th).min(list_b), c.cursor)?;
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
        ("archive.zip", c.selected_file, Deco::Selected),
    ];
    let mut ry = list_y;
    for (name, color, deco) in rows {
        if ry + row_h > list_b {
            break;
        }
        match deco {
            Deco::Selected => {
                fill(dc, x, ry, list_r, ry + row_h, c.selected_file_bg)?;
                dc.SetTextColor(to_colorref(c.selected_file))?;
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
        if matches!(deco, Deco::Cursor) {
            frame(dc, x, ry, list_r, ry + row_h, c.cursor)?;
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
fn draw_log(dc: &w::HDC, x: i32, y: i32, w: i32, h: i32, fh: i32, c: &Colors) -> w::AnyResult<()> {
    if w <= 0 || h <= 0 {
        return Ok(());
    }
    fill(dc, x, y, x + w, y + h, c.log_background)?;
    let pad = gui::dpi_y(2);
    let left = x + gui::dpi_x(4);
    let row_h = fh + pad;
    let logs: [(&str, Rgb); 4] = [
        ("コピーを開始します", c.log_normal),
        ("3 個のファイルを選択しました", c.log_info),
        ("空き容量が少なくなっています", c.log_warning),
        ("アクセスが拒否されました", c.log_error),
    ];
    let mut ly = y + pad;
    for (text, color) in logs {
        if ly + row_h > y + h {
            break;
        }
        dc.SetTextColor(to_colorref(color))?;
        dc.TextOut(left, ly, text)?;
        ly += row_h;
    }
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

        let font = list_font(&family, fsize)?;
        let _fsel = dc.SelectObject(&*font)?;
        let fh = dc.GetTextMetrics().map(|tm| tm.tmHeight).unwrap_or(16);
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
        let log_h = gui::dpi_y(lay.log_height);
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
        draw_pane(dc, left_x, pane_top, left_w, pane_h, bar_h, bar_gap, status_h, sb_w, fh, icons.show, icon_px, &colors)?;
        draw_pane(dc, right_x, pane_top, right_w, pane_h, bar_h, bar_gap, status_h, sb_w, fh, icons.show, icon_px, &colors)?;
        draw_log(dc, left_x, log_y, log_w, log_h, fh, &colors)?;
        Ok(())
    }
}

/// 配色を実色のスウォッチ付きで一覧し、ダブルクリック（または「変更」ボタン）で編集する自前リスト。
#[derive(Clone)]
struct SwatchList {
    wnd: gui::WindowControl,
    shared: Rc<Shared>,
    preview: Preview,
    sel: Rc<Cell<usize>>,
    row_h: Rc<Cell<i32>>,
}

impl SwatchList {
    fn new(
        parent: &(impl GuiParent + 'static),
        pos: (i32, i32),
        size: (i32, i32),
        shared: Rc<Shared>,
        preview: Preview,
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
            preview,
            sel: Rc::new(Cell::new(0)),
            row_h: Rc::new(Cell::new(gui::dpi_y(24))),
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
        if idx < COLOR_FIELDS.len() {
            self.sel.set(idx);
            self.refresh();
        }
    }

    /// 選択中の色を色選択ダイアログで編集して反映する。
    fn edit_selected(&self) {
        let idx = self.sel.get();
        let Some((_, get, set)) = COLOR_FIELDS.get(idx) else {
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
            self.preview.refresh();
        }
    }

    /// 編集対象テーマの配色を既定へ戻す。
    fn reset(&self) {
        let dark = self.shared.target_dark.get();
        {
            let mut cfg = self.shared.cfg.borrow_mut();
            if dark {
                cfg.colors.dark = Colors::dark();
            } else {
                cfg.colors.light = Colors::light();
            }
        }
        self.refresh();
        self.preview.refresh();
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

        // 15 項目を 1 列に縦並び（フル高で全色を収める）。
        for (i, (label, get, _)) in COLOR_FIELDS.iter().enumerate() {
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
                if let Some(i) = unsafe { sl.hwnd().SendMessage(lb::GetCurSel {}) } {
                    if let Some((st, _)) = SORT_TYPES.get(i as usize) {
                        shared.cfg.borrow_mut().default_sort = *st;
                    }
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
                if let Some(i) = sel {
                    if let Some(it) = shown.items().iter().nth(i) {
                        let _ = it.select(true);
                        let _ = it.focus();
                    }
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
                if let Some(ki) = unsafe { available.hwnd().SendMessage(lb::GetCurSel {}) } {
                    if let Some((kind, _)) = COLUMN_KINDS.get(ki as usize) {
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
                if let Some(i) = sel {
                    if let Some(it) = list.items().iter().nth(i) {
                        let _ = it.select(true);
                        let _ = it.focus();
                    }
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

/// 「キー」ページ（割り当ての一覧表示）。
#[derive(Clone)]
struct KeysPane {
    list: gui::ListBox,
    rows: Rc<Vec<String>>,
}

impl KeysPane {
    fn new(parent: &gui::WindowControl, shared: &Rc<Shared>) -> Self {
        label(parent, "現在のキー割り当て（変更は config.toml で行います）", 16, 12, 400);
        let list = gui::ListBox::new(
            parent,
            gui::ListBoxOpts {
                position: gui::dpi(16, 36),
                size: gui::dpi(760, 500),
                ..Default::default()
            },
        );
        let rows: Vec<String> = shared
            .cfg
            .borrow()
            .keybinds
            .iter()
            .map(|(k, v)| format!("{k:<16} {v}"))
            .collect();
        Self { list, rows: Rc::new(rows) }
    }

    /// window 生成後に一覧を流し込む（生成前の add は無効化されるため）。
    fn populate(&self) {
        let _ = self.list.items().add(&self.rows);
    }
}

/// 設定ダイアログを表示する。`OK`／`適用` で確定した [`Config`] を `on_apply` へ渡す
/// （`適用` は閉じずに継続、`OK` は閉じる。`キャンセル` は破棄して閉じる）。
pub fn show(parent: &impl GuiParent, current: &Config, on_apply: impl Fn(&Config) + 'static) {
    let wnd = crate::dialog::modal_window_sysmenu("設定", 960, 620);

    let shared = Rc::new(Shared {
        cfg: RefCell::new(current.clone()),
        target_dark: Cell::new(current.resolved == ResolvedTheme::Dark),
    });

    // 左カラム：ナビ（ツリー）。各ノードの data は表示する pane 番号。
    let nav = gui::TreeView::<usize>::new(
        &wnd,
        gui::TreeViewOpts {
            position: gui::dpi(12, 12),
            size: gui::dpi(152, 544),
            ..Default::default()
        },
    );

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
    let panes = vec![
        pane_appearance.clone(),
        pane_colors.clone(),
        pane_layout.clone(),
        pane_cursor.clone(),
        pane_registered.clone(),
        pane_keys.clone(),
        pane_image.clone(),
        pane_list.clone(),
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

    // 各 pane の中身。
    build_appearance(&pane_appearance, &shared, &preview);
    build_layout(&pane_layout, &shared, &preview);
    build_cursor(&pane_cursor, &shared);
    build_viewer(&pane_image, &shared);
    build_list(&pane_list, &shared);
    let columns_editor = ColumnsEditor::new(&pane_list, &shared);
    let registered = RegisteredPane::new(&pane_registered, &shared);
    let keys = KeysPane::new(&pane_keys, &shared);

    // 配色 pane：操作ボタン（上段）＋スウォッチ一覧（フル高・1 列）。
    label(&pane_colors, "色をダブルクリックで変更", 8, 14, 200);
    let change = gui::Button::new(
        &pane_colors,
        gui::ButtonOpts {
            text: "変更(&C)...",
            position: gui::dpi(8, 36),
            width: gui::dpi_x(110),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );
    let reset = gui::Button::new(
        &pane_colors,
        gui::ButtonOpts {
            text: "既定に戻す(&R)",
            position: gui::dpi(126, 36),
            width: gui::dpi_x(130),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );
    let swatch = SwatchList::new(&pane_colors, gui::dpi(8, 72), gui::dpi(344, 466), shared.clone(), preview.clone());

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

    // window 生成後：ナビ流し込み・初期表示 pane・各リスト初期化。
    {
        let nav = nav.clone();
        let panes = panes.clone();
        let keys = keys.clone();
        let registered = registered.clone();
        let columns_editor = columns_editor.clone();
        #[cfg(feature = "debug-server")]
        let reg_wnd = wnd.clone();
        wnd.on().wm_create(move |_| {
            // ツリー構築：外観(テーマ・フォント/配色/レイアウト)・動作(カーソル/ビューア)・
            // 登録(登録ディレクトリ)・キー。data＝pane 番号。外観のみ右にプレビューを出す。
            if let Ok(appearance) = nav.items().add_root("外観", None, 0) {
                let _ = appearance.add_child("テーマ・フォント", None, 0);
                let _ = appearance.add_child("配色", None, 1);
                let _ = appearance.add_child("レイアウト", None, 2);
                let _ = appearance.expand(true);
            }
            let _ = nav.items().add_root("一覧", None, 7);
            if let Ok(behavior) = nav.items().add_root("動作", None, 3) {
                let _ = behavior.add_child("カーソル", None, 3);
                let _ = behavior.add_child("ビューア", None, 6);
                let _ = behavior.expand(true);
            }
            if let Ok(register) = nav.items().add_root("登録", None, 4) {
                let _ = register.add_child("登録ディレクトリ", None, 4);
                let _ = register.expand(true);
            }
            let _ = nav.items().add_root("キー", None, 5);
            // 先頭ルート（外観＝pane 0）を初期選択（tvn_sel_changed が pane／プレビューを整える）。
            unsafe {
                if let Some(first) =
                    nav.hwnd().SendMessage(tvm::GetNextItem { relationship: co::TVGN::ROOT, hitem: None })
                {
                    let _ = nav
                        .hwnd()
                        .SendMessage(tvm::SelectItem { action: co::TVGN::CARET, hitem: &first });
                }
            }
            for (i, p) in panes.iter().enumerate() {
                p.hwnd().ShowWindow(if i == 0 { co::SW::SHOW } else { co::SW::HIDE });
            }
            registered.populate();
            keys.populate();
            columns_editor.populate();
            #[cfg(feature = "debug-server")]
            crate::debug_server::modal_registry::push(
                "settings",
                "設定",
                "",
                reg_wnd.hwnd().ptr() as isize,
                false,
                vec![
                    ("OK".to_string(), 1u16),
                    ("キャンセル".to_string(), 2u16),
                    ("適用".to_string(), 3u16),
                ],
            );
            Ok(0)
        });
    }

    // 左ナビ（ツリー）選択で pane を切り替え、外観ブランチ（pane 0..=2）のときだけプレビューを出す。
    {
        let nav2 = nav.clone();
        let panes = panes.clone();
        let preview = preview.clone();
        let preview_label = preview_label.clone();
        nav.on().tvn_sel_changed(move |_| {
            if let Some(item) = nav2.items().iter_selected().next() {
                let idx = *item.data().borrow();
                for (i, p) in panes.iter().enumerate() {
                    p.hwnd().ShowWindow(if i == idx { co::SW::SHOW } else { co::SW::HIDE });
                }
                // 外観ブランチ（pane 0..=2）のときだけプレビューを出す。
                let sw = if idx <= 2 { co::SW::SHOW } else { co::SW::HIDE };
                preview_label.hwnd().ShowWindow(sw);
                preview.hwnd().ShowWindow(sw);
            }
            Ok(())
        });
    }

    // 配色操作ボタン。
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

    // 適用：閉じずに現在の設定を反映する。
    {
        let on_apply = on_apply.clone();
        let shared = shared.clone();
        apply.on().bn_clicked(move || {
            on_apply(&shared.cfg.borrow());
            Ok(())
        });
    }
    // OK：反映して閉じる。
    {
        let on_apply = on_apply.clone();
        let shared = shared.clone();
        let wnd2 = wnd.clone();
        ok.on().bn_clicked(move || {
            on_apply(&shared.cfg.borrow());
            wnd2.close();
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
    #[cfg(feature = "debug-server")]
    crate::debug_server::modal_registry::pop();
    let _ = (nav, panes, swatch, change, reset, keys, registered, ok, cancel, apply, preview_label);
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
