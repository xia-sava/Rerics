//! 設定ダイアログ。左ナビ（外観・配色・レイアウト・キー）と常時表示のライブプレビューを
//! 並べた master-detail 構成。
//!
//! `show` がモーダルを表示し、OK なら編集後の [`Config`] を返す。返した設定は呼び出し側で
//! ライブ反映＋差分保存される。編集中の値は [`Shared`] に集約し、配色・フォントの変更は
//! その場でプレビューへ反映する。レイアウト・テーマ・フォントは OK 時に widget から確定する。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rerics_core::{Colors, Config, Layout, Rgb, ResolvedTheme, Theme};
use winsafe::{self as w, co, gui, msg::lb, prelude::*};

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
const SECTIONS: &[&str] = &["外観", "配色", "レイアウト", "キー"];

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

/// 配色・フォントの効きをその場で見せるライブプレビュー（ミニ・ファイル一覧＋ミニ・ログ）。
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

    fn render(&self, dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
        let (family, size) = {
            let cfg = self.shared.cfg.borrow();
            (cfg.font.family.clone(), cfg.font.size)
        };
        let font = list_font(&family, size)?;
        let _fsel = dc.SelectObject(&*font)?;
        let fh = dc.GetTextMetrics().map(|tm| tm.tmHeight).unwrap_or(16);
        dc.SetBkMode(co::BKMODE::TRANSPARENT)?;

        let colors = self.shared.target_colors();
        let pad = gui::dpi_y(3);
        let left = gui::dpi_x(8);
        let row_h = fh + pad * 2;

        // 下部にミニ・ログ（4 行）を確保し、残りをファイル一覧にあてる。
        let log_h = row_h * 4 + pad * 2;
        let file_bottom = (ch - log_h).max(row_h);

        fill(dc, 0, 0, cw, file_bottom, colors.background)?;

        // ヘッダ行。
        fill(dc, 0, 0, cw, row_h, colors.background2)?;
        dc.SetTextColor(to_colorref(colors.file_normal))?;
        dc.TextOut(left, pad, "名前                                更新日時              サイズ")?;

        // データ行。色割り当てとカーソル／選択の見え方を一通り示す。
        let rows: [(&str, Rgb, Deco); 6] = [
            ("src", colors.directory, Deco::Plain),
            ("readme.md", colors.file_normal, Deco::Cursor),
            ("LICENSE", colors.readonly, Deco::Plain),
            ("pagefile.sys", colors.system, Deco::Plain),
            (".gitignore", colors.hidden, Deco::Plain),
            ("archive.zip", colors.selected_file, Deco::Selected),
        ];
        let mut y = row_h;
        for (name, color, deco) in rows {
            if y + row_h > file_bottom {
                break;
            }
            match deco {
                Deco::Selected => {
                    fill(dc, 0, y, cw, y + row_h, colors.selected_file_bg)?;
                    dc.SetTextColor(to_colorref(colors.selected_file))?;
                }
                _ => {
                    dc.SetTextColor(to_colorref(color))?;
                }
            }
            dc.TextOut(left, y + pad, name)?;
            if matches!(deco, Deco::Cursor) {
                frame(dc, 0, y, cw, y + row_h, colors.cursor)?;
            }
            y += row_h;
        }

        // ミニ・ログ。
        fill(dc, 0, file_bottom, cw, ch, colors.log_background)?;
        let logs: [(&str, Rgb); 4] = [
            ("コピーを開始します", colors.log_normal),
            ("3 個のファイルを選択しました", colors.log_info),
            ("空き容量が少なくなっています", colors.log_warning),
            ("アクセスが拒否されました", colors.log_error),
        ];
        let mut ly = file_bottom + pad;
        for (text, color) in logs {
            dc.SetTextColor(to_colorref(color))?;
            dc.TextOut(left, ly, text)?;
            ly += row_h;
        }
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
            row_h: Rc::new(Cell::new(gui::dpi_y(22))),
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
        me
    }

    fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    fn refresh(&self) {
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    fn on_click(&self, pt: w::POINT) {
        let rh = self.row_h.get().max(1);
        let Ok(rc) = self.hwnd().GetClientRect() else {
            return;
        };
        let col_w = ((rc.right - rc.left) / 2).max(1);
        let rows_per_col = (COLOR_FIELDS.len() as i32 + 1) / 2;
        let col = (pt.x / col_w).clamp(0, 1);
        let row = pt.y / rh;
        let idx = (col * rows_per_col + row) as usize;
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
        let sw_w = gui::dpi_x(30);
        let sw_pad = gui::dpi_y(3);

        // 15 項目を 2 列に振り分けて全色を収める。
        let col_w = cw / 2;
        let rows_per_col = (COLOR_FIELDS.len() as i32 + 1) / 2;
        for (i, (label, get, _)) in COLOR_FIELDS.iter().enumerate() {
            let col = i as i32 / rows_per_col;
            let row = i as i32 % rows_per_col;
            let x0 = col * col_w;
            let y = row * row_h;
            let selected = i == sel;
            if selected {
                let hl_brush = w::HBRUSH::CreateSolidBrush(hl)?;
                dc.FillRect(w::RECT { left: x0, top: y, right: x0 + col_w, bottom: y + row_h }, &hl_brush)?;
            }

            // 実色のスウォッチ（枠付き）。
            let c = get(&colors);
            let sx = x0 + left;
            let sy = y + sw_pad;
            let sb = y + row_h - sw_pad;
            frame(dc, sx, sy, sx + sw_w, sb, frame_col)?;
            fill(dc, sx + 1, sy + 1, sx + sw_w - 1, sb - 1, c)?;

            // ラベルと 16 進値。
            dc.SetTextColor(if selected { hl_text } else { normal_text })?;
            let text = format!("{label}  #{:02X}{:02X}{:02X}", c.r, c.g, c.b);
            dc.TextOut(sx + sw_w + gui::dpi_x(8), y + pad, &text)?;
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

/// 「外観」ページ（テーマ・フォント）。
#[derive(Clone)]
struct AppearancePane {
    theme: gui::RadioGroup,
    font_family: gui::Edit,
    font_size: gui::Edit,
}

impl AppearancePane {
    fn new(parent: &gui::WindowControl, shared: &Rc<Shared>, preview: &Preview) -> Self {
        let cfg = shared.cfg.borrow();
        label(parent, "配色テーマ", 16, 14, 120);
        let theme = gui::RadioGroup::new(
            parent,
            &[
                gui::RadioButtonOpts {
                    text: "ダーク(&D)",
                    position: gui::dpi(28, 38),
                    size: gui::dpi(180, 20),
                    selected: cfg.theme == Theme::Dark,
                    ..Default::default()
                },
                gui::RadioButtonOpts {
                    text: "ライト(&L)",
                    position: gui::dpi(28, 62),
                    size: gui::dpi(180, 20),
                    selected: cfg.theme == Theme::Light,
                    ..Default::default()
                },
                gui::RadioButtonOpts {
                    text: "システムに従う(&S)",
                    position: gui::dpi(28, 86),
                    size: gui::dpi(220, 20),
                    selected: cfg.theme == Theme::System,
                    ..Default::default()
                },
            ],
        );

        label(parent, "フォント名", 16, 134, 90);
        let font_family = gui::Edit::new(
            parent,
            gui::EditOpts {
                text: &cfg.font.family,
                control_style: co::ES::AUTOHSCROLL,
                position: gui::dpi(112, 132),
                width: gui::dpi_x(240),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        label(parent, "フォントサイズ", 16, 166, 90);
        let font_size = gui::Edit::new(
            parent,
            gui::EditOpts {
                text: &cfg.font.size.to_string(),
                control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
                position: gui::dpi(112, 164),
                width: gui::dpi_x(60),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );
        label(parent, "（フォントはプレビューに反映されます）", 16, 196, 320);
        drop(cfg);

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

        Self { theme, font_family, font_size }
    }

    fn collect(&self, cfg: &mut Config) {
        cfg.theme = match self.theme.selected_index() {
            Some(0) => Theme::Dark,
            Some(1) => Theme::Light,
            _ => Theme::System,
        };
        if let Ok(f) = self.font_family.text() {
            let f = f.trim();
            if !f.is_empty() {
                cfg.font.family = f.to_owned();
            }
        }
        cfg.font.size = parse_or(&self.font_size, cfg.font.size).clamp(6, 72);
    }
}

/// 「レイアウト」ページ（寸法を数値で編集）。
#[derive(Clone)]
struct LayoutPane {
    edits: Vec<gui::Edit>,
}

impl LayoutPane {
    fn new(parent: &gui::WindowControl, shared: &Rc<Shared>) -> Self {
        let cfg = shared.cfg.borrow();
        let mut edits = Vec::with_capacity(LAYOUT_FIELDS.len());
        for (i, (lbl, get, _)) in LAYOUT_FIELDS.iter().enumerate() {
            let col = (i / 6) as i32;
            let row = (i % 6) as i32;
            let x = 16 + col * 232;
            let y = 16 + row * 34;
            label(parent, lbl, x, y + 2, 130);
            let edit = gui::Edit::new(
                parent,
                gui::EditOpts {
                    text: &get(&cfg.layout).to_string(),
                    control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
                    position: gui::dpi(x + 134, y),
                    width: gui::dpi_x(60),
                    height: gui::dpi_y(22),
                    ..Default::default()
                },
            );
            edits.push(edit);
        }
        Self { edits }
    }

    fn collect(&self, cfg: &mut Config) {
        for (edit, (_, get, set)) in self.edits.iter().zip(LAYOUT_FIELDS) {
            let cur = get(&cfg.layout);
            set(&mut cfg.layout, parse_or(edit, cur).max(0));
        }
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
        label(parent, "現在のキー割り当て（変更は config.toml で行います）", 16, 12, 460);
        let list = gui::ListBox::new(
            parent,
            gui::ListBoxOpts {
                position: gui::dpi(16, 36),
                size: gui::dpi(452, 232),
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

/// 設定ダイアログを表示し、OK なら編集後の設定を返す。キャンセルなら `None`。
pub fn show(parent: &impl GuiParent, current: &Config) -> Option<Config> {
    let wnd = gui::WindowModal::new(gui::WindowModalOpts {
        title: "設定",
        size: gui::dpi(680, 600),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE | co::WS::SYSMENU,
        process_dlg_msgs: true,
        ..Default::default()
    });

    let shared = Rc::new(Shared {
        cfg: RefCell::new(current.clone()),
        target_dark: Cell::new(current.resolved == ResolvedTheme::Dark),
    });

    // 左ナビ。
    let nav = gui::ListBox::new(
        &wnd,
        gui::ListBoxOpts {
            position: gui::dpi(12, 12),
            size: gui::dpi(140, 548),
            ..Default::default()
        },
    );

    // 右側：セクション pane（同じ矩形に重ねて show/hide で切替）。
    let pane_pos = gui::dpi(164, 12);
    let pane_size = gui::dpi(504, 252);
    let pane_appearance = make_pane(&wnd, pane_pos, pane_size);
    let pane_colors = make_pane(&wnd, pane_pos, pane_size);
    let pane_layout = make_pane(&wnd, pane_pos, pane_size);
    let pane_keys = make_pane(&wnd, pane_pos, pane_size);
    let panes = vec![
        pane_appearance.clone(),
        pane_colors.clone(),
        pane_layout.clone(),
        pane_keys.clone(),
    ];

    // プレビュー（右下・常時表示）と、その対象テーマ切替。
    label(&wnd, "プレビュー", 164, 272, 80);
    let target = gui::RadioGroup::new(
        &wnd,
        &[
            gui::RadioButtonOpts {
                text: "ダーク",
                position: gui::dpi(248, 270),
                size: gui::dpi(72, 20),
                selected: shared.target_dark.get(),
                ..Default::default()
            },
            gui::RadioButtonOpts {
                text: "ライト",
                position: gui::dpi(324, 270),
                size: gui::dpi(72, 20),
                selected: !shared.target_dark.get(),
                ..Default::default()
            },
        ],
    );
    let preview = Preview::new(&wnd, gui::dpi(164, 296), gui::dpi(504, 264), shared.clone());

    // 各 pane の中身。
    let appearance = AppearancePane::new(&pane_appearance, &shared, &preview);
    let layout = LayoutPane::new(&pane_layout, &shared);
    let keys = KeysPane::new(&pane_keys, &shared);

    // 配色 pane：スウォッチ一覧（2 列・横いっぱい）＋上段の操作ボタン。
    label(&pane_colors, "色をダブルクリックで変更", 8, 12, 270);
    let change = gui::Button::new(
        &pane_colors,
        gui::ButtonOpts {
            text: "変更(&C)...",
            position: gui::dpi(290, 6),
            width: gui::dpi_x(96),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );
    let reset = gui::Button::new(
        &pane_colors,
        gui::ButtonOpts {
            text: "既定に戻す(&R)",
            position: gui::dpi(392, 6),
            width: gui::dpi_x(108),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );
    let swatch = SwatchList::new(&pane_colors, gui::dpi(8, 40), gui::dpi(488, 204), shared.clone(), preview.clone());

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(474, 564),
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
            position: gui::dpi(572, 564),
            width: gui::dpi_x(94),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<Config>>> = Rc::new(RefCell::new(None));

    // window 生成後：ナビ流し込み・初期表示 pane・各リスト初期化。
    {
        let nav = nav.clone();
        let panes = panes.clone();
        let keys = keys.clone();
        wnd.on().wm_create(move |_| {
            let _ = nav.items().add(SECTIONS);
            unsafe {
                let _ = nav.hwnd().SendMessage(lb::SetCurSel { index: Some(0) });
            }
            for (i, p) in panes.iter().enumerate() {
                p.hwnd().ShowWindow(if i == 0 { co::SW::SHOW } else { co::SW::HIDE });
            }
            keys.populate();
            Ok(0)
        });
    }

    // 左ナビ選択で pane を切り替える。
    {
        let nav2 = nav.clone();
        let panes = panes.clone();
        nav.on().lbn_sel_change(move || {
            let idx = unsafe { nav2.hwnd().SendMessage(lb::GetCurSel {}) };
            if let Some(idx) = idx {
                for (i, p) in panes.iter().enumerate() {
                    p.hwnd().ShowWindow(if i as u32 == idx { co::SW::SHOW } else { co::SW::HIDE });
                }
            }
            Ok(())
        });
    }

    // プレビュー対象テーマの切替（配色編集対象も連動）。
    for i in 0..target.count() {
        let shared = shared.clone();
        let preview = preview.clone();
        let swatch = swatch.clone();
        target[i].on().bn_clicked(move || {
            shared.target_dark.set(i == 0);
            swatch.refresh();
            preview.refresh();
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

    // OK：widget から確定値を集めて返す（配色は shared 上で編集済み）。
    {
        let result = result.clone();
        let wnd2 = wnd.clone();
        let shared = shared.clone();
        let appearance = appearance.clone();
        let layout = layout.clone();
        ok.on().bn_clicked(move || {
            let mut cfg = shared.cfg.borrow().clone();
            appearance.collect(&mut cfg);
            layout.collect(&mut cfg);
            *result.borrow_mut() = Some(cfg);
            wnd2.close();
            Ok(())
        });
    }
    {
        let wnd2 = wnd.clone();
        cancel.on().bn_clicked(move || {
            wnd2.close();
            Ok(())
        });
    }

    let _ = wnd.show_modal(parent);
    let _ = (nav, panes, swatch, change, reset, target, keys, ok, cancel);
    let r = result.borrow().clone();
    r
}
