//! 設定ダイアログ。配色テーマ・フォント・レイアウト・キーバインドをタブで編集する。
//!
//! `show` がモーダルを表示し、OK なら編集後の [`Config`] を返す。返した設定は呼び出し側で
//! ライブ反映＋差分保存される。各ページは自分の widget 値を `collect` で `Config` へ書き戻す。

use std::cell::RefCell;
use std::rc::Rc;

use rerics_core::{Colors, Config, Layout, ResolvedTheme, Rgb, Theme, ThemeColors};
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

/// 「全般」ページ（テーマ・フォント）。
#[derive(Clone)]
struct GeneralPage {
    page: gui::TabPage,
    theme: gui::RadioGroup,
    font_family: gui::Edit,
    font_size: gui::Edit,
}

impl From<GeneralPage> for gui::TabPage {
    fn from(p: GeneralPage) -> Self {
        p.page
    }
}

impl GeneralPage {
    fn new(parent: &(impl GuiParent + 'static), cfg: &Config) -> Self {
        let page = gui::TabPage::new(parent, gui::TabPageOpts::default());

        let _ = gui::Label::new(
            &page,
            gui::LabelOpts {
                text: "配色テーマ",
                position: gui::dpi(16, 16),
                size: gui::dpi(120, 18),
                ..Default::default()
            },
        );
        let theme = gui::RadioGroup::new(
            &page,
            &[
                gui::RadioButtonOpts {
                    text: "ダーク(&D)",
                    position: gui::dpi(28, 40),
                    size: gui::dpi(160, 20),
                    selected: cfg.theme == Theme::Dark,
                    ..Default::default()
                },
                gui::RadioButtonOpts {
                    text: "ライト(&L)",
                    position: gui::dpi(28, 64),
                    size: gui::dpi(160, 20),
                    selected: cfg.theme == Theme::Light,
                    ..Default::default()
                },
                gui::RadioButtonOpts {
                    text: "システムに従う(&S)",
                    position: gui::dpi(28, 88),
                    size: gui::dpi(220, 20),
                    selected: cfg.theme == Theme::System,
                    ..Default::default()
                },
            ],
        );

        let _ = gui::Label::new(
            &page,
            gui::LabelOpts {
                text: "フォント名",
                position: gui::dpi(16, 128),
                size: gui::dpi(120, 18),
                ..Default::default()
            },
        );
        let font_family = gui::Edit::new(
            &page,
            gui::EditOpts {
                text: &cfg.font.family,
                control_style: co::ES::AUTOHSCROLL,
                position: gui::dpi(140, 126),
                width: gui::dpi_x(220),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );

        let _ = gui::Label::new(
            &page,
            gui::LabelOpts {
                text: "フォントサイズ",
                position: gui::dpi(16, 158),
                size: gui::dpi(120, 18),
                ..Default::default()
            },
        );
        let font_size = gui::Edit::new(
            &page,
            gui::EditOpts {
                text: &cfg.font.size.to_string(),
                control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
                position: gui::dpi(140, 156),
                width: gui::dpi_x(60),
                height: gui::dpi_y(22),
                ..Default::default()
            },
        );

        Self { page, theme, font_family, font_size }
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

/// 「配色」ページ（ダーク/ライトの各色を個別に編集）。
#[derive(Clone)]
struct ColorsPage {
    page: gui::TabPage,
    target: gui::RadioGroup,
    list: gui::ListBox,
    colors: Rc<RefCell<ThemeColors>>,
}

impl From<ColorsPage> for gui::TabPage {
    fn from(p: ColorsPage) -> Self {
        p.page
    }
}

impl ColorsPage {
    fn new(parent: &(impl GuiParent + 'static), cfg: &Config) -> Self {
        let page = gui::TabPage::new(parent, gui::TabPageOpts::default());
        let colors = Rc::new(RefCell::new(cfg.colors));

        let _ = gui::Label::new(
            &page,
            gui::LabelOpts {
                text: "編集対象",
                position: gui::dpi(16, 14),
                size: gui::dpi(80, 18),
                ..Default::default()
            },
        );
        let target = gui::RadioGroup::new(
            &page,
            &[
                gui::RadioButtonOpts {
                    text: "ダーク",
                    position: gui::dpi(96, 12),
                    size: gui::dpi(80, 20),
                    selected: cfg.resolved == ResolvedTheme::Dark,
                    ..Default::default()
                },
                gui::RadioButtonOpts {
                    text: "ライト",
                    position: gui::dpi(184, 12),
                    size: gui::dpi(80, 20),
                    selected: cfg.resolved == ResolvedTheme::Light,
                    ..Default::default()
                },
            ],
        );

        let list = gui::ListBox::new(
            &page,
            gui::ListBoxOpts {
                position: gui::dpi(16, 40),
                size: gui::dpi(300, 240),
                ..Default::default()
            },
        );

        let change = gui::Button::new(
            &page,
            gui::ButtonOpts {
                text: "色の変更(&C)...",
                position: gui::dpi(330, 40),
                width: gui::dpi_x(150),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );
        let reset = gui::Button::new(
            &page,
            gui::ButtonOpts {
                text: "このテーマを既定に戻す",
                position: gui::dpi(330, 74),
                width: gui::dpi_x(150),
                height: gui::dpi_y(26),
                ..Default::default()
            },
        );

        let me = Self { page, target, list, colors };

        // 編集対象の切替で一覧を再表示する。
        for i in 0..me.target.count() {
            let this = me.clone();
            me.target[i].on().bn_clicked(move || {
                this.repopulate();
                Ok(())
            });
        }

        // 選択中の色を色選択ダイアログで変更する。
        {
            let this = me.clone();
            change.on().bn_clicked(move || {
                this.edit_selected();
                Ok(())
            });
        }

        // 編集対象のテーマを既定色へ戻す。
        {
            let this = me.clone();
            reset.on().bn_clicked(move || {
                let dark = this.target_is_dark();
                {
                    let mut c = this.colors.borrow_mut();
                    if dark {
                        c.dark = Colors::dark();
                    } else {
                        c.light = Colors::light();
                    }
                }
                this.repopulate();
                Ok(())
            });
        }

        // 生成直後はまだ window が無いため、表示は最初の WM 後（repopulate は add 失敗を無視）。
        me.repopulate();
        me
    }

    fn target_is_dark(&self) -> bool {
        self.target.selected_index() != Some(1)
    }

    /// 編集対象テーマの `Colors` を取り出す。
    fn current_colors(&self) -> Colors {
        let c = self.colors.borrow();
        if self.target_is_dark() { c.dark } else { c.light }
    }

    /// 一覧を現在の編集対象テーマの色で埋め直す（選択位置は保つ）。
    fn repopulate(&self) {
        let sel = self.selected_index();
        self.list.items().delete_all();
        let colors = self.current_colors();
        let rows: Vec<String> = COLOR_FIELDS
            .iter()
            .map(|(label, get, _)| {
                let c = get(&colors);
                format!("{label:<12} #{:02X}{:02X}{:02X}", c.r, c.g, c.b)
            })
            .collect();
        let _ = self.list.items().add(&rows);
        let restore = sel.unwrap_or(0).min(COLOR_FIELDS.len() as u32 - 1);
        unsafe {
            let _ = self.list.hwnd().SendMessage(lb::SetCurSel { index: Some(restore) });
        }
    }

    fn selected_index(&self) -> Option<u32> {
        unsafe { self.list.hwnd().SendMessage(lb::GetCurSel {}) }
    }

    /// 選択中の色を色選択ダイアログで編集して反映する。
    fn edit_selected(&self) {
        let Some(idx) = self.selected_index() else {
            return;
        };
        let idx = idx as usize;
        let Some((_, get, set)) = COLOR_FIELDS.get(idx) else {
            return;
        };
        let dark = self.target_is_dark();
        let cur = get(&self.current_colors());
        let picked = choose_color(self.page.hwnd(), cur);
        if picked != cur {
            let mut c = self.colors.borrow_mut();
            let target = if dark { &mut c.dark } else { &mut c.light };
            set(target, picked);
            drop(c);
            self.repopulate();
        }
    }

    fn collect(&self, cfg: &mut Config) {
        cfg.colors = *self.colors.borrow();
    }
}

/// 「レイアウト」ページ（寸法を数値で編集）。
#[derive(Clone)]
struct LayoutPage {
    page: gui::TabPage,
    edits: Vec<gui::Edit>,
}

impl From<LayoutPage> for gui::TabPage {
    fn from(p: LayoutPage) -> Self {
        p.page
    }
}

impl LayoutPage {
    fn new(parent: &(impl GuiParent + 'static), cfg: &Config) -> Self {
        let page = gui::TabPage::new(parent, gui::TabPageOpts::default());
        let mut edits = Vec::with_capacity(LAYOUT_FIELDS.len());
        // 2 列に並べる。
        for (i, (label, get, _)) in LAYOUT_FIELDS.iter().enumerate() {
            let col = (i / 6) as i32;
            let row = (i % 6) as i32;
            let x = 16 + col * 240;
            let y = 16 + row * 34;
            let _ = gui::Label::new(
                &page,
                gui::LabelOpts {
                    text: label,
                    position: gui::dpi(x, y + 2),
                    size: gui::dpi(130, 18),
                    ..Default::default()
                },
            );
            let edit = gui::Edit::new(
                &page,
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
        Self { page, edits }
    }

    fn collect(&self, cfg: &mut Config) {
        for (edit, (_, get, set)) in self.edits.iter().zip(LAYOUT_FIELDS) {
            let cur = get(&cfg.layout);
            set(&mut cfg.layout, parse_or(edit, cur).max(0));
        }
    }
}

/// 「キー」ページ（現状は割り当ての一覧表示のみ）。
#[derive(Clone)]
struct KeysPage {
    page: gui::TabPage,
    list: gui::ListBox,
    rows: Rc<Vec<String>>,
}

impl From<KeysPage> for gui::TabPage {
    fn from(p: KeysPage) -> Self {
        p.page
    }
}

impl KeysPage {
    fn new(parent: &(impl GuiParent + 'static), cfg: &Config) -> Self {
        let page = gui::TabPage::new(parent, gui::TabPageOpts::default());
        let _ = gui::Label::new(
            &page,
            gui::LabelOpts {
                text: "現在のキー割り当て（編集は今後対応・config.toml で変更可）",
                position: gui::dpi(16, 12),
                size: gui::dpi(460, 18),
                ..Default::default()
            },
        );
        let list = gui::ListBox::new(
            &page,
            gui::ListBoxOpts {
                position: gui::dpi(16, 36),
                size: gui::dpi(464, 248),
                ..Default::default()
            },
        );
        let rows: Vec<String> = cfg
            .keybinds
            .iter()
            .map(|(k, v)| format!("{k:<16} {v}"))
            .collect();
        Self { page, list, rows: Rc::new(rows) }
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
        size: gui::dpi(540, 480),
        style: co::WS::CAPTION | co::WS::BORDER | co::WS::VISIBLE | co::WS::SYSMENU,
        process_dlg_msgs: true,
        ..Default::default()
    });

    let general = GeneralPage::new(&wnd, current);
    let colors = ColorsPage::new(&wnd, current);
    let layout = LayoutPage::new(&wnd, current);
    let keys = KeysPage::new(&wnd, current);

    let pages: Vec<(&str, gui::TabPage)> = vec![
        ("全般", general.clone().into()),
        ("配色", colors.clone().into()),
        ("レイアウト", layout.clone().into()),
        ("キー", keys.clone().into()),
    ];
    let tab = gui::Tab::new(
        &wnd,
        gui::TabOpts {
            position: gui::dpi(8, 8),
            size: gui::dpi(524, 420),
            pages: &pages,
            ..Default::default()
        },
    );

    let ok = gui::Button::new(
        &wnd,
        gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(340, 440),
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
            position: gui::dpi(438, 440),
            width: gui::dpi_x(94),
            height: gui::dpi_y(28),
            ..Default::default()
        },
    );

    let result: Rc<RefCell<Option<Config>>> = Rc::new(RefCell::new(None));

    // window 生成後に各リストを流し込む（生成前の add は無効化されるため）。
    {
        let colors2 = colors.clone();
        let keys2 = keys.clone();
        wnd.on().wm_create(move |_| {
            colors2.repopulate();
            keys2.populate();
            Ok(0)
        });
    }

    {
        let result = result.clone();
        let wnd2 = wnd.clone();
        let base = current.clone();
        let general = general.clone();
        let colors = colors.clone();
        let layout = layout.clone();
        ok.on().bn_clicked(move || {
            let mut cfg = base.clone();
            general.collect(&mut cfg);
            colors.collect(&mut cfg);
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
    let _ = (tab, ok, cancel, keys);
    let r = result.borrow().clone();
    r
}
