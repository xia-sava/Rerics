//! ファイル一覧のモデル層（UI 非依存）。
//!
//! `FileItem`（1エントリ）・`SortType`（ソート種別）・比較器・`FileListState`
//! （カーソル/スクロール/選択/列を持つコントロール状態）・配色モデルを提供する。

use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Local, TimeZone};
use serde::{Deserialize, Serialize};

/// ファイル一覧の1エントリ。
#[derive(Debug, Clone)]
pub struct FileItem {
    pub name: String,
    pub base_name: String,
    pub extension: String,
    pub is_dir: bool,
    pub is_parent: bool,
    pub size: Option<u64>,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub readonly: bool,
    pub hidden: bool,
    pub system: bool,
    pub archive: bool,
    pub reparse: bool,
    pub selected: bool,
}

impl FileItem {
    /// `name` と `is_dir` から base/ext を分解した item を作る（属性・日時は既定値）。
    pub(crate) fn bare(name: String, is_dir: bool) -> Self {
        let (base_name, extension) = split_base_ext(&name, is_dir);
        Self {
            name,
            base_name,
            extension,
            is_dir,
            is_parent: false,
            size: None,
            created: None,
            modified: None,
            readonly: false,
            hidden: false,
            system: false,
            archive: false,
            reparse: false,
            selected: false,
        }
    }

    /// 親（".."）エントリを作る。
    pub fn parent() -> Self {
        let mut it = Self::bare("..".to_owned(), true);
        it.is_parent = true;
        it
    }

    /// std のメタデータから item を構築する。
    #[cfg(windows)]
    pub fn from_metadata(name: String, meta: &std::fs::Metadata) -> Self {
        use std::os::windows::fs::MetadataExt;
        let is_dir = meta.is_dir();
        let mut it = Self::bare(name, is_dir);
        let attr = meta.file_attributes();
        it.readonly = attr & 0x1 != 0;
        it.hidden = attr & 0x2 != 0;
        it.system = attr & 0x4 != 0;
        it.archive = attr & 0x20 != 0;
        it.reparse = attr & 0x400 != 0;
        it.size = if is_dir { None } else { Some(meta.len()) };
        it.created = meta.created().ok();
        it.modified = meta.modified().ok();
        it
    }
}

/// 名前と種別から base/ext を分解する。
///
/// dir は base=name, ext=""。file は名前が '.' 始まりで他に '.' が無ければ ext="",
/// それ以外は最後の '.' で分割し ext は '.' を含む。
fn split_base_ext(name: &str, is_dir: bool) -> (String, String) {
    if is_dir {
        return (name.to_owned(), String::new());
    }
    match name.rfind('.') {
        Some(0) => (name.to_owned(), String::new()),
        Some(idx) => (name[..idx].to_owned(), name[idx..].to_owned()),
        None => (name.to_owned(), String::new()),
    }
}

/// ソート種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SortType {
    #[default]
    FileName,
    Extension,
    Length,
    CreateTime,
    LastWriteTime,
    Attribute,
    FileNameExpLike,
    ExtensionExpLike,
}

impl SortType {
    /// リテラル引数（`Sort("name")` 等）からソート種別を解釈する。大小無視。
    /// バリアント名のほか、よく使う別名（size/date/ext 等）も受理する。
    pub fn from_token(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "name" | "filename" => Some(Self::FileName),
            "ext" | "extension" => Some(Self::Extension),
            "size" | "length" => Some(Self::Length),
            "createtime" | "created" => Some(Self::CreateTime),
            "date" | "time" | "modified" | "lastwritetime" => Some(Self::LastWriteTime),
            "attr" | "attribute" => Some(Self::Attribute),
            "filenameexplike" => Some(Self::FileNameExpLike),
            "extensionexplike" => Some(Self::ExtensionExpLike),
            _ => None,
        }
    }
}

/// 2エントリをソート種別で比較する（reverse なし）。親優先・dir 優先は呼び出し側で先に判定済み。
fn compare_kind(a: &FileItem, b: &FileItem, sort: SortType) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let by_name = || a.name.to_uppercase().cmp(&b.name.to_uppercase());
    match sort {
        SortType::FileName => by_name(),
        SortType::Extension => {
            let o = a.extension.to_uppercase().cmp(&b.extension.to_uppercase());
            if o == Ordering::Equal { by_name() } else { o }
        }
        SortType::Length => {
            let o = a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0));
            if o == Ordering::Equal { by_name() } else { o }
        }
        SortType::CreateTime => {
            let o = cmp_time(b.created, a.created);
            if o == Ordering::Equal { by_name() } else { o }
        }
        SortType::LastWriteTime => {
            let o = cmp_time(b.modified, a.modified);
            if o == Ordering::Equal { by_name() } else { o }
        }
        SortType::Attribute => {
            // System → Hidden → Readonly → Archive の優先で持つ方が先。
            if a.system != b.system {
                if a.system { Ordering::Less } else { Ordering::Greater }
            } else if a.hidden != b.hidden {
                if a.hidden { Ordering::Less } else { Ordering::Greater }
            } else if a.readonly != b.readonly {
                if a.readonly { Ordering::Less } else { Ordering::Greater }
            } else if a.archive != b.archive {
                if a.archive { Ordering::Less } else { Ordering::Greater }
            } else {
                by_name()
            }
        }
        SortType::FileNameExpLike => {
            exp_like_compare(&a.name.to_uppercase(), &b.name.to_uppercase())
        }
        SortType::ExtensionExpLike => {
            let o = exp_like_compare(&a.extension.to_uppercase(), &b.extension.to_uppercase());
            if o == Ordering::Equal {
                exp_like_compare(&a.name.to_uppercase(), &b.name.to_uppercase())
            } else {
                o
            }
        }
    }
}

/// SystemTime を `DateTime::Compare(x, y)` 相当で比較する（None は MinValue 扱い）。
fn cmp_time(x: Option<SystemTime>, y: Option<SystemTime>) -> std::cmp::Ordering {
    match (x, y) {
        (Some(a), Some(b)) => a.cmp(&b),
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
    }
}

/// 比較器本体（親優先 → dir 優先 → 種別比較、reverse は種別比較のみ反転）。
fn compare_items(a: &FileItem, b: &FileItem, sort: SortType, reverse: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.is_parent, b.is_parent) {
        (true, false) => return Ordering::Less,
        (true, true) => return Ordering::Equal,
        (false, true) => return Ordering::Greater,
        (false, false) => {}
    }
    if a.is_dir != b.is_dir {
        return if a.is_dir { Ordering::Less } else { Ordering::Greater };
    }
    let o = compare_kind(a, b, sort);
    if reverse { o.reverse() } else { o }
}

/// 自然順比較（文字列中の数字列を数値として比較する）。
///
/// 完全一致は Equal。先頭文字が同じ or 両方数字なら「非数字プレフィクス＋数字列」を
/// 繰り返しマッチし、プレフィクス(trim) が等しい間は数字部を整数比較する（9桁以上は通常比較へ）。
fn exp_like_compare(input1: &str, input2: &str) -> std::cmp::Ordering {
    if input1 == input2 {
        return std::cmp::Ordering::Equal;
    }
    let c1 = input1.chars().next().unwrap_or('\0');
    let c2 = input2.chars().next().unwrap_or('\0');
    if c1 == c2 || (c1.is_ascii_digit() && c2.is_ascii_digit()) {
        let mut m1 = ExpMatcher::new(input1);
        let mut m2 = ExpMatcher::new(input2);
        if let (Some(mut a), Some(mut b)) = (m1.next(), m2.next()) {
            loop {
                if a.prefix.trim() != b.prefix.trim() {
                    break;
                }
                if a.digits.len() > 8 || b.digits.len() > 8 {
                    break;
                }
                let na: i64 = a.digits.parse().unwrap_or(0);
                let nb: i64 = b.digits.parse().unwrap_or(0);
                let num = na - nb;
                if num != 0 {
                    return num.cmp(&0);
                }
                let rest1 = &input1[a.end..];
                let rest2 = &input2[b.end..];
                match (m1.next(), m2.next()) {
                    (Some(na2), Some(nb2)) => {
                        a = na2;
                        b = nb2;
                        continue;
                    }
                    _ => return rest1.cmp(rest2),
                }
            }
        }
    }
    input1.cmp(input2)
}

/// 1回分の「非数字プレフィクス＋数字列」マッチ結果。
struct ExpMatch {
    prefix: String,
    digits: String,
    /// マッチ終端の input 内バイト位置（match.Index + match.Length 相当）。
    end: usize,
}

/// `([^\d]*)(\d+)` を順次マッチするイテレータ。
struct ExpMatcher<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> ExpMatcher<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, pos: 0 }
    }

    fn next(&mut self) -> Option<ExpMatch> {
        let bytes = self.s.as_bytes();
        let n = bytes.len();
        let mut i = self.pos;
        // 非数字プレフィクス。
        let prefix_start = i;
        while i < n && !bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i >= n {
            return None;
        }
        let prefix = self.s[prefix_start..i].to_owned();
        // 数字列（1文字以上）。
        let digit_start = i;
        while i < n && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let digits = self.s[digit_start..i].to_owned();
        self.pos = i;
        Some(ExpMatch { prefix, digits, end: i })
    }
}

/// 列の表示種別（Icon/Information は今回なし）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnKind {
    FileName,
    FileBaseName,
    FileExtension,
    Length,
    CreateTime,
    LastWriteTime,
    CreateTimeS,
    LastWriteTimeS,
    Attribute,
}

/// 列のテキスト揃え。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Align {
    Left,
    Right,
}

/// 1列の表示定義。`width` は論理 px（DPI スケールは GUI 層）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub kind: ColumnKind,
    pub text: String,
    pub width: i32,
    pub align: Align,
}

impl Column {
    fn new(kind: ColumnKind, text: &str, width: i32, align: Align) -> Self {
        Self { kind, text: text.to_owned(), width, align }
    }
}

/// 固定幅列の「最悪幅の代表文字列」。`cell_text` の書式と対になっており、
/// 内容に依らずジャンプしない列幅を測るために使う（GUI 層が文字数×平均文字幅で測定）。
/// 内容追従させたい列（フレックスの名前列・可変の拡張子列）は空文字列を返す。
pub fn column_sample(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::FileName | ColumnKind::FileBaseName | ColumnKind::FileExtension => "",
        // 生バイト＋3桁区切り。12桁（〜約931GiB）まで固定し、それ以上は手動拡張に委ねる。
        ColumnKind::Length => "999,999,999,999",
        ColumnKind::CreateTime | ColumnKind::LastWriteTime => "0000/00/00 00:00",
        ColumnKind::CreateTimeS | ColumnKind::LastWriteTimeS => "00/00/00 00:00",
        ColumnKind::Attribute => "DSHRA",
    }
}

/// デフォルトの列構成。
pub fn default_columns() -> Vec<Column> {
    vec![
        Column::new(ColumnKind::FileBaseName, "ファイル名", 230, Align::Left),
        Column::new(ColumnKind::FileExtension, "種類", 60, Align::Left),
        Column::new(ColumnKind::Length, "サイズ", 90, Align::Right),
        Column::new(ColumnKind::LastWriteTime, "更新日時", 120, Align::Left),
        Column::new(ColumnKind::Attribute, "属性", 50, Align::Left),
    ]
}

/// 列幅を内容に合わせて調整する（content-fit）。
///
/// `measured[i]` は列 i の内容実幅（ヘッダラベルと最長セルの大きい方・パディング無し）。
/// 最初の名前列（`FileName`/`FileBaseName`）をフレックス列とし、残り幅を埋める。
/// それ以外の列は `measured + pad` を `avail * max_ratio` で上限クランプする
/// （上限により、種類列など可変長のものが極端に広がるのを防ぐ）。
/// フレックス列は `min_flex` を下限とし、それより狭くはならない。pane がさらに狭いと
/// 列の合計が `avail` を超え、右端の固定列が描画時に画面外へはみ出る（呼び出し側でクリップ）。
/// 引数の単位は呼び出し側の任意（全て同一単位なら px でも論理 px でもよい）。
pub fn auto_adjust_columns(
    columns: &mut [Column],
    measured: &[i32],
    avail: i32,
    scrollbar_w: i32,
    pad: i32,
    max_ratio: f64,
    min_flex: i32,
) {
    if columns.is_empty() {
        return;
    }
    let cap = (avail as f64 * max_ratio).max(8.0) as i32;
    let flex = columns
        .iter()
        .position(|c| matches!(c.kind, ColumnKind::FileName | ColumnKind::FileBaseName));
    let mut fixed_total = 0;
    for (i, col) in columns.iter_mut().enumerate() {
        if Some(i) == flex {
            continue;
        }
        let w = (measured.get(i).copied().unwrap_or(0) + pad).clamp(8, cap);
        col.width = w;
        fixed_total += w;
    }
    if let Some(fi) = flex {
        let rest = avail - scrollbar_w - fixed_total;
        columns[fi].width = rest.max(min_flex.max(1));
    }
}

/// RGB 三つ組の配色（GUI 層で COLORREF へ変換）。
/// TOML へは `"#rrggbb"` の16進文字列として直列化する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `"#rrggbb"` 文字列を解釈する（`#` 省略可・大小無視）。
    pub fn parse_hex(s: &str) -> Option<Self> {
        let h = s.trim().strip_prefix('#').unwrap_or(s.trim());
        if h.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&h[0..2], 16).ok()?;
        let g = u8::from_str_radix(&h[2..4], 16).ok()?;
        let b = u8::from_str_radix(&h[4..6], 16).ok()?;
        Some(Self { r, g, b })
    }

    /// `"#rrggbb"` 文字列へ変換する。
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// 自身を `other` 方向へ `num/den` の比率で混ぜた色（整数演算・各チャンネル独立）。
    /// `num=0` で自身、`num=den` で `other`。
    pub fn blend(self, other: Rgb, num: u16, den: u16) -> Self {
        let mix = |a: u8, b: u8| -> u8 {
            ((a as u16 * (den - num) + b as u16 * num) / den) as u8
        };
        Self::new(mix(self.r, other.r), mix(self.g, other.g), mix(self.b, other.b))
    }
}

impl Serialize for Rgb {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Rgb::parse_hex(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color: {s}")))
    }
}

/// 配色モデル（既定は黒背景のダークテーマ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colors {
    pub file_normal: Rgb,
    pub directory: Rgb,
    pub readonly: Rgb,
    pub system: Rgb,
    pub hidden: Rgb,
    pub background: Rgb,
    pub background2: Rgb,
    pub selected_file: Rgb,
    pub selected_file_bg: Rgb,
    pub cursor: Rgb,
    pub log_background: Rgb,
    pub log_normal: Rgb,
    pub log_info: Rgb,
    pub log_warning: Rgb,
    pub log_error: Rgb,
}

impl Default for Colors {
    fn default() -> Self {
        Self::dark()
    }
}

impl Colors {
    /// ダークテーマの既定色（黒背景・明るい属性色）。
    pub fn dark() -> Self {
        Self {
            file_normal: Rgb::new(255, 255, 255),
            directory: Rgb::new(0xff, 0xff, 0xe8),
            readonly: Rgb::new(0x80, 0xff, 0x80),
            system: Rgb::new(0x80, 0xff, 0xff),
            hidden: Rgb::new(0x80, 0x80, 0xff),
            background: Rgb::new(0, 0, 0),
            background2: Rgb::new(0x20, 0x20, 0x20),
            selected_file: Rgb::new(255, 255, 255),
            selected_file_bg: Rgb::new(0x60, 0xa0, 0x80),
            cursor: Rgb::new(0x00, 0xff, 0x80),
            log_background: Rgb::new(0x00, 0x00, 0x00),
            log_normal: Rgb::new(0xff, 0xff, 0xff),
            log_info: Rgb::new(0xc8, 0xc8, 0xc8),
            log_warning: Rgb::new(0xff, 0xd0, 0x40),
            log_error: Rgb::new(0xff, 0x70, 0x70),
        }
    }

    /// ライトテーマの既定色（白背景・Windows 標準配色を再現。属性色はダークと
    /// 同系統の低明度に落としたもの）。
    pub fn light() -> Self {
        Self {
            file_normal: Rgb::new(0x00, 0x00, 0x00),
            directory: Rgb::new(0x70, 0x58, 0x00),
            readonly: Rgb::new(0x00, 0x80, 0x00),
            system: Rgb::new(0x00, 0x80, 0x80),
            hidden: Rgb::new(0x40, 0x40, 0xa0),
            background: Rgb::new(0xff, 0xff, 0xff),
            background2: Rgb::new(0xf0, 0xf0, 0xf0),
            selected_file: Rgb::new(0xff, 0xff, 0xff),
            selected_file_bg: Rgb::new(0x00, 0x78, 0xd7),
            cursor: Rgb::new(0x00, 0x60, 0xc0),
            log_background: Rgb::new(0xff, 0xff, 0xff),
            log_normal: Rgb::new(0x00, 0x00, 0x00),
            log_info: Rgb::new(0x40, 0x40, 0x40),
            log_warning: Rgb::new(0x9a, 0x5a, 0x00),
            log_error: Rgb::new(0xc0, 0x00, 0x00),
        }
    }
}

impl Colors {
    /// 非マーク時の文字色（通常→Dir→ReadOnly→Hidden→System の後勝ち上書き）。
    pub fn item_color(&self, item: &FileItem) -> Rgb {
        let mut c = self.file_normal;
        if item.is_dir {
            c = self.directory;
        }
        if item.readonly {
            c = self.readonly;
        }
        if item.hidden {
            c = self.hidden;
        }
        if item.system {
            c = self.system;
        }
        c
    }
}

/// ファイル一覧コントロールの状態モデル（描画と完全分離）。
#[derive(Clone)]
pub struct FileListState {
    pub items: Vec<FileItem>,
    pub cursor: usize,
    pub scroll_top: usize,
    pub select_start: usize,
    pub sort_type: SortType,
    pub sort_reverse: bool,
    pub columns: Vec<Column>,
}

impl Default for FileListState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            cursor: 0,
            scroll_top: 0,
            select_start: 0,
            sort_type: SortType::FileName,
            sort_reverse: false,
            columns: default_columns(),
        }
    }
}

impl FileListState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn count(&self) -> usize {
        self.items.len()
    }

    /// 表示できる最終行。
    pub fn scroll_bottom(&self, page_rows: usize) -> usize {
        let count = self.count();
        if count == 0 {
            return 0;
        }
        (self.scroll_top + page_rows.max(1) - 1).min(count - 1)
    }

    /// scroll_top を 0..=max(0, count-page_rows) にクランプする。
    fn clamp_scroll(&mut self, page_rows: usize) {
        let count = self.count();
        let max = count.saturating_sub(page_rows.max(1));
        if self.scroll_top > max {
            self.scroll_top = max;
        }
    }

    /// カーソルを `idx` へ移動し、必要に応じスクロール追従する。
    pub fn set_cursor(&mut self, idx: isize, page_rows: usize) {
        let count = self.count();
        if count == 0 {
            self.cursor = 0;
            self.scroll_top = 0;
            return;
        }
        let page_rows = page_rows.max(1);
        let margin: isize = 1;
        // クランプ 0..=count-1。
        let mut cur = idx;
        if cur < 0 {
            cur = 0;
        }
        let maxi = (count - 1) as isize;
        if cur > maxi {
            cur = maxi;
        }
        self.cursor = cur as usize;
        // NeedScrollBar: count-1 >= page_rows かつ count-1 > 0。
        let need = maxi >= page_rows as isize && maxi > 0;
        if need {
            let top = self.scroll_top as isize;
            if cur < top + margin {
                let mut v = cur - margin;
                if v < 0 {
                    v = 0;
                }
                self.scroll_top = v as usize;
            } else if cur >= top + page_rows as isize - margin {
                let mut v = cur - (page_rows as isize - margin - 1);
                if v < 0 {
                    v = 0;
                }
                self.scroll_top = v as usize;
            }
        } else {
            self.scroll_top = 0;
        }
        self.clamp_scroll(page_rows);
    }

    /// scroll_top を直接設定する（カーソルは動かさない）。
    pub fn set_scroll_top(&mut self, top: isize, page_rows: usize) {
        let top = if top < 0 { 0 } else { top as usize };
        self.scroll_top = top;
        self.clamp_scroll(page_rows.max(1));
    }

    /// カーソルが表示範囲外なら、表示範囲内の最も近い行へ寄せる（スクロール後に使う）。
    pub fn cursor_into_view(&mut self, page_rows: usize) {
        if self.count() == 0 {
            return;
        }
        let top = self.scroll_top;
        let bottom = self.scroll_bottom(page_rows);
        if self.cursor < top {
            self.cursor = top;
        } else if self.cursor > bottom {
            self.cursor = bottom;
        }
    }

    /// カーソル行が中央に来るようスクロールする。
    pub fn center_cursor(&mut self, page_rows: usize) {
        let page_rows = page_rows.max(1);
        let top = self.cursor as isize - (page_rows / 2) as isize;
        self.set_scroll_top(top, page_rows);
    }

    /// `filename` に一致する行へカーソルを移動する（大文字小文字は区別）。
    pub fn set_cursor_position(&mut self, filename: &str, page_rows: usize) -> bool {
        if let Some(i) = self.items.iter().position(|it| it.name == filename) {
            self.set_cursor(i as isize, page_rows);
            self.select_start = i;
            true
        } else {
            false
        }
    }

    /// index のマークを立てる（親はスキップ）。カーソルも idx へ・select_start=idx。
    pub fn select_file(&mut self, idx: usize, page_rows: usize) {
        if idx >= self.count() {
            return;
        }
        self.set_cursor(idx as isize, page_rows);
        self.select_start = idx;
        let it = &mut self.items[idx];
        if !it.is_parent {
            it.selected = true;
        }
    }

    /// index のマークを外す（親はスキップ）。
    pub fn clear_file(&mut self, idx: usize, page_rows: usize) {
        if idx >= self.count() {
            return;
        }
        self.set_cursor(idx as isize, page_rows);
        self.select_start = idx;
        let it = &mut self.items[idx];
        if !it.is_parent {
            it.selected = false;
        }
    }

    /// index のマークをトグルする（親はスキップ）。カーソルも idx へ・select_start=idx。
    pub fn reverse_file(&mut self, idx: usize, page_rows: usize) {
        if idx >= self.count() {
            return;
        }
        self.set_cursor(idx as isize, page_rows);
        self.select_start = idx;
        let it = &mut self.items[idx];
        if !it.is_parent {
            it.selected = !it.selected;
        }
    }

    /// start〜end を範囲マークする（双方向。親はスキップ）。
    pub fn select_files(&mut self, start: usize, end: usize) {
        let count = self.count();
        if count == 0 || start >= count || end >= count {
            return;
        }
        let mut i = start as isize;
        let e = end as isize;
        loop {
            let it = &mut self.items[i as usize];
            if !it.is_parent {
                it.selected = true;
            }
            if i == e {
                break;
            }
            i += if i < e { 1 } else { -1 };
        }
    }

    /// 全項目マーク（親はスキップ。fileonly で dir も除外）。
    pub fn select_all(&mut self, fileonly: bool) {
        for it in &mut self.items {
            if !it.is_parent && (!it.is_dir || !fileonly) {
                it.selected = true;
            }
        }
    }

    /// 全項目トグル（親はスキップ。fileonly で dir も除外）。
    pub fn reverse_all(&mut self, fileonly: bool) {
        for it in &mut self.items {
            if !it.is_parent && (!it.is_dir || !fileonly) {
                it.selected = !it.selected;
            }
        }
    }

    /// 全項目のマークを外す。
    pub fn clear_all(&mut self) {
        for it in &mut self.items {
            it.selected = false;
        }
    }

    /// マーク数と合計サイズ。
    pub fn selected_count_size(&self) -> (u64, u64) {
        let mut count = 0u64;
        let mut size = 0u64;
        for it in &self.items {
            if it.selected {
                count += 1;
                size += it.size.unwrap_or(0);
            }
        }
        (count, size)
    }

    /// items をソートする。
    pub fn sort(&mut self, sort: SortType, reverse: bool) {
        self.sort_type = sort;
        self.sort_reverse = reverse;
        self.items
            .sort_by(|a, b| compare_items(a, b, sort, reverse));
    }

    /// 列のセルテキストを生成する。
    pub fn cell_text(&self, item: &FileItem, kind: ColumnKind) -> String {
        match kind {
            ColumnKind::FileName => item.name.clone(),
            ColumnKind::FileBaseName => item.base_name.clone(),
            ColumnKind::FileExtension => item.extension.clone(),
            ColumnKind::Length => match item.size {
                // サイズが取れていればディレクトリでも数値表示（書庫内 dir 等）。
                Some(sz) => format_size(sz),
                // サイズ不明：ディレクトリは "<DIR>"、ファイルは 0 と詐称せず "--"。
                None => {
                    if item.is_dir {
                        "<DIR>".to_owned()
                    } else {
                        "--".to_owned()
                    }
                }
            },
            ColumnKind::CreateTime => format_time(item.created, "%Y/%m/%d %H:%M"),
            ColumnKind::LastWriteTime => format_time(item.modified, "%Y/%m/%d %H:%M"),
            ColumnKind::CreateTimeS => format_time(item.created, "%y/%m/%d %H:%M"),
            ColumnKind::LastWriteTimeS => format_time(item.modified, "%y/%m/%d %H:%M"),
            ColumnKind::Attribute => {
                let mut s = String::new();
                if item.is_dir {
                    s.push('D');
                }
                if item.system {
                    s.push('S');
                }
                if item.hidden {
                    s.push('H');
                }
                if item.readonly {
                    s.push('R');
                }
                if item.archive {
                    s.push('A');
                }
                if item.reparse {
                    s.push('J');
                }
                s
            }
        }
    }
}

/// 桁区切りカンマの10進バイト数表記。
fn format_size(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// SystemTime を Local 変換し指定書式で文字列化する（None は空）。
fn format_time(t: Option<SystemTime>, fmt: &str) -> String {
    let Some(t) = t else {
        return String::new();
    };
    let dt: DateTime<Local> = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => match Local.timestamp_opt(d.as_secs() as i64, 0).single() {
            Some(dt) => dt,
            None => return String::new(),
        },
        Err(_) => return String::new(),
    };
    dt.format(fmt).to_string()
}

/// 指定ディレクトリ直下の `FileItem` を読み出す（親があれば先頭に ".." を含める）。
pub fn read_items(path: impl AsRef<Path>) -> std::io::Result<Vec<FileItem>> {
    let path = path.as_ref();
    let mut items = Vec::new();
    if path.parent().is_some() {
        items.push(FileItem::parent());
    }
    for ent in std::fs::read_dir(path)? {
        let ent = ent?;
        let meta = ent.metadata()?;
        let name = ent.file_name().to_string_lossy().into_owned();
        items.push(FileItem::from_metadata(name, &meta));
    }
    Ok(items)
}

/// カンマ区切りのグロブパターン（`*`=任意長, `?`=任意1文字）のいずれかに
/// `name` が（大文字小文字を無視して）一致するか。空パターンや "*" は全一致。
pub fn glob_match(name: &str, patterns: &str) -> bool {
    let pats: Vec<&str> = patterns
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if pats.is_empty() {
        return true;
    }
    let name_lower: Vec<char> = name.to_lowercase().chars().collect();
    pats.iter().any(|p| {
        let pat_lower: Vec<char> = p.to_lowercase().chars().collect();
        glob_one(&name_lower, &pat_lower)
    })
}

/// 1パターンとの照合（`*`=0文字以上, `?`=任意1文字）。引数は小文字化済みの `char` 列。
fn glob_one(name: &[char], pat: &[char]) -> bool {
    let (mut ni, mut pi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_n = 0usize;
    while ni < name.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == name[ni]) {
            ni += 1;
            pi += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi);
            star_n = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            star_n += 1;
            ni = star_n;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

/// `query`（大小無視・部分一致）に一致する項目の添字を探す。`from` から `forward`
/// 方向に走査し、`wrap` なら端で折り返す。".."（親）は対象外。query 空・該当なし・
/// 空リストは `None`。インクリメンタルサーチの心臓部（打鍵ごとに呼ぶ）。
pub fn find_match(
    items: &[FileItem],
    from: usize,
    query: &str,
    forward: bool,
    wrap: bool,
) -> Option<usize> {
    if query.is_empty() || items.is_empty() {
        return None;
    }
    let q = query.to_lowercase();
    let n = items.len();
    let from = from.min(n - 1);
    let matches = |i: usize| !items[i].is_parent && items[i].name.to_lowercase().contains(&q);
    // 走査する件数：折り返し時は全件、片方向のみなら端まで。
    let count = if wrap {
        n
    } else if forward {
        n - from
    } else {
        from + 1
    };
    for k in 0..count {
        // forward なら from→末尾（必要なら先頭へ折り返し）、backward なら from→先頭。
        let i = if forward {
            (from + k) % n
        } else {
            (from + n - (k % n)) % n
        };
        if matches(i) {
            return Some(i);
        }
    }
    None
}

/// 連番リネームの新名を生成する。各 `names[i]` を `{prefix}{番号:0digits}{元拡張子?}`
/// に変換する。`start` から連番、`digits` 桁で0詰め、`keep_ext` なら元の拡張子
/// （先頭ドットつき）を残す。GUI から独立した純関数（プレビューと実行で共用）。
pub fn sequence_names(
    names: &[String],
    prefix: &str,
    start: u64,
    digits: usize,
    keep_ext: bool,
) -> Vec<String> {
    let width = digits.max(1);
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let num = start + i as u64;
            let ext = if keep_ext { ext_with_dot(name) } else { String::new() };
            format!("{prefix}{num:0width$}{ext}")
        })
        .collect()
}

/// ファイル名から拡張子を先頭ドットつきで取り出す（"a.txt"→".txt"・".bashrc"や
/// 拡張子なしは ""）。最後のドット以降を拡張子とみなす。
fn ext_with_dot(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((base, ext)) if !base.is_empty() => format!(".{ext}"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn sort_type_from_token() {
        assert_eq!(SortType::from_token("name"), Some(SortType::FileName));
        assert_eq!(SortType::from_token("FileName"), Some(SortType::FileName));
        assert_eq!(SortType::from_token("ext"), Some(SortType::Extension));
        assert_eq!(SortType::from_token("size"), Some(SortType::Length));
        assert_eq!(SortType::from_token(" Date "), Some(SortType::LastWriteTime));
        assert_eq!(SortType::from_token("bogus"), None);
    }

    fn file(name: &str) -> FileItem {
        FileItem::bare(name.to_owned(), false)
    }

    fn dir(name: &str) -> FileItem {
        FileItem::bare(name.to_owned(), true)
    }

    #[test]
    fn find_match_direction_wrap_and_parent() {
        let items = vec![
            FileItem::parent(),
            file("Apple.txt"),
            file("banana.txt"),
            file("Cherry.txt"),
            file("apricot.txt"),
        ];
        // 大小無視・部分一致・先頭から。
        assert_eq!(find_match(&items, 0, "ap", true, false), Some(1)); // Apple
        // index2 から前方＝apricot。
        assert_eq!(find_match(&items, 2, "ap", true, false), Some(4));
        // 折り返し無しで後ろに無ければ自身のみ評価。
        assert_eq!(find_match(&items, 4, "ap", true, false), Some(4));
        // 折り返しありで末尾から banana を拾う。
        assert_eq!(find_match(&items, 4, "ban", true, true), Some(2));
        // 後方検索。
        assert_eq!(find_match(&items, 3, "ap", false, false), Some(1));
        // 該当なし・空クエリ・".." は対象外。
        assert_eq!(find_match(&items, 0, "zzz", true, true), None);
        assert_eq!(find_match(&items, 0, "", true, true), None);
        assert_eq!(find_match(&items, 0, "..", true, false), None);
    }

    #[test]
    fn sequence_names_pads_keeps_or_drops_ext() {
        let names = vec!["a.txt".to_owned(), "b.jpg".to_owned(), "noext".to_owned()];
        // 拡張子を保持。
        let out = sequence_names(&names, "img", 1, 3, true);
        assert_eq!(out, vec!["img001.txt", "img002.jpg", "img003"]);
        // 拡張子を残さず・桁上がりも 0 詰め幅で。
        let out2 = sequence_names(&names, "p", 9, 2, false);
        assert_eq!(out2, vec!["p09", "p10", "p11"]);
    }

    fn state_with(n: usize) -> FileListState {
        let mut s = FileListState::new();
        s.items = (0..n).map(|i| file(&format!("f{i}"))).collect();
        s
    }

    #[test]
    fn base_ext_split() {
        assert_eq!(split_base_ext("foo.txt", false), ("foo".to_owned(), ".txt".to_owned()));
        assert_eq!(split_base_ext(".gitignore", false), (".gitignore".to_owned(), String::new()));
        assert_eq!(split_base_ext("a.tar.gz", false), ("a.tar".to_owned(), ".gz".to_owned()));
        assert_eq!(split_base_ext("noext", false), ("noext".to_owned(), String::new()));
        assert_eq!(split_base_ext("dir.name", true), ("dir.name".to_owned(), String::new()));
    }

    #[test]
    fn cursor_follow_margin() {
        let mut s = state_with(100);
        let pr = 10;
        s.set_cursor(0, pr);
        assert_eq!(s.scroll_top, 0);
        s.set_cursor(50, pr);
        // cursor=50, page=10, margin=1 → scroll_top = 50-(10-1-1)=42
        assert_eq!(s.cursor, 50);
        assert_eq!(s.scroll_top, 42);
        s.set_cursor(99, pr);
        assert_eq!(s.cursor, 99);
        // scroll_top クランプ後 max = 100-10 = 90
        assert_eq!(s.scroll_top, 90);
        s.set_cursor(0, pr);
        assert_eq!(s.scroll_top, 0);
    }

    #[test]
    fn cursor_into_view_clamps() {
        let mut s = state_with(100);
        let pr = 10;
        // カーソルが表示範囲より上 → 先頭可視行へ
        s.cursor = 5;
        s.set_scroll_top(40, pr); // 可視 [40,49]
        s.cursor_into_view(pr);
        assert_eq!(s.cursor, 40);
        // カーソルが表示範囲より下 → 末尾可視行へ
        s.cursor = 95;
        s.set_scroll_top(40, pr);
        s.cursor_into_view(pr);
        assert_eq!(s.cursor, 49);
        // 範囲内なら不変
        s.cursor = 45;
        s.set_scroll_top(40, pr);
        s.cursor_into_view(pr);
        assert_eq!(s.cursor, 45);
    }

    #[test]
    fn cursor_no_scroll_when_fits() {
        let mut s = state_with(5);
        let pr = 10;
        s.set_cursor(4, pr);
        assert_eq!(s.cursor, 4);
        assert_eq!(s.scroll_top, 0);
    }

    #[test]
    fn cursor_clamps_out_of_range() {
        let mut s = state_with(10);
        s.set_cursor(-5, 10);
        assert_eq!(s.cursor, 0);
        s.set_cursor(999, 10);
        assert_eq!(s.cursor, 9);
    }

    #[test]
    fn sort_parent_dir_file_order() {
        let mut s = FileListState::new();
        s.items = vec![file("b.txt"), dir("zdir"), FileItem::parent(), file("a.txt"), dir("adir")];
        s.sort(SortType::FileName, false);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["..", "adir", "zdir", "a.txt", "b.txt"]);
    }

    #[test]
    fn sort_filename_case_insensitive() {
        let mut s = FileListState::new();
        s.items = vec![file("Banana"), file("apple"), file("Cherry")];
        s.sort(SortType::FileName, false);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["apple", "Banana", "Cherry"]);
    }

    #[test]
    fn sort_lastwrite_newest_first_default() {
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + std::time::Duration::from_secs(100);
        let t2 = t0 + std::time::Duration::from_secs(200);
        let mut a = file("old");
        a.modified = Some(t0);
        let mut b = file("mid");
        b.modified = Some(t1);
        let mut c = file("new");
        c.modified = Some(t2);
        let mut s = FileListState::new();
        s.items = vec![a, b, c];
        s.sort(SortType::LastWriteTime, false);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["new", "mid", "old"]);
        s.sort(SortType::LastWriteTime, true);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["old", "mid", "new"]);
    }

    #[test]
    fn explike_natural_order() {
        assert_eq!(exp_like_compare("FILE2", "FILE10"), Ordering::Less);
        assert_eq!(exp_like_compare("FILE10", "FILE2"), Ordering::Greater);
        assert_eq!(exp_like_compare("FILE2", "FILE2"), Ordering::Equal);
        // 等値プレフィクスで複数数字列。
        assert_eq!(exp_like_compare("V1-2", "V1-10"), Ordering::Less);
        // 9桁以上は通常文字列比較へフォールバック。
        assert_eq!(
            exp_like_compare("X1000000000", "X2"),
            "X1000000000".cmp("X2")
        );
    }

    #[test]
    fn explike_sort_in_state() {
        let mut s = FileListState::new();
        s.items = vec![file("file10"), file("file2"), file("file1")];
        s.sort(SortType::FileNameExpLike, false);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["file1", "file2", "file10"]);
    }

    #[test]
    fn mark_skips_parent() {
        let mut s = FileListState::new();
        s.items = vec![FileItem::parent(), file("a"), file("b")];
        s.select_file(0, 10);
        assert!(!s.items[0].selected);
        s.select_file(1, 10);
        assert!(s.items[1].selected);
        assert_eq!(s.cursor, 1);
        assert_eq!(s.select_start, 1);
    }

    #[test]
    fn select_files_both_directions() {
        let mut s = FileListState::new();
        s.items = vec![file("a"), file("b"), file("c"), file("d")];
        s.select_files(3, 1);
        assert!(!s.items[0].selected);
        assert!(s.items[1].selected);
        assert!(s.items[2].selected);
        assert!(s.items[3].selected);
    }

    #[test]
    fn reverse_all_fileonly() {
        let mut s = FileListState::new();
        s.items = vec![FileItem::parent(), dir("d"), file("f")];
        s.reverse_all(true);
        assert!(!s.items[0].selected);
        assert!(!s.items[1].selected);
        assert!(s.items[2].selected);
    }

    #[test]
    fn cell_text_variants() {
        let mut d = dir("mydir");
        d.modified = None;
        assert_eq!(
            FileListState::new().cell_text(&d, ColumnKind::Length),
            "<DIR>"
        );
        let mut f = file("big.bin");
        f.size = Some(1234567);
        assert_eq!(
            FileListState::new().cell_text(&f, ColumnKind::Length),
            "1,234,567"
        );
        // #14: サイズが取れているディレクトリ（書庫内 dir 等）は数値表示。
        let mut sized_dir = dir("withsize");
        sized_dir.size = Some(4096);
        assert_eq!(
            FileListState::new().cell_text(&sized_dir, ColumnKind::Length),
            "4,096"
        );
        // サイズ不明のファイルは 0 と詐称せず "--"。
        let unknown = file("unknown.bin");
        assert_eq!(
            FileListState::new().cell_text(&unknown, ColumnKind::Length),
            "--"
        );
        let mut attr = file("x");
        attr.system = true;
        attr.hidden = true;
        attr.readonly = true;
        attr.archive = true;
        assert_eq!(
            FileListState::new().cell_text(&attr, ColumnKind::Attribute),
            "SHRA"
        );
        let dt = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(0);
        let mut tf = file("t");
        tf.modified = Some(dt);
        let txt = FileListState::new().cell_text(&tf, ColumnKind::LastWriteTime);
        assert_eq!(txt.len(), "yyyy/MM/dd HH:mm".len());
        assert_eq!(txt.matches('/').count(), 2);
    }

    #[test]
    fn glob_match_basics() {
        assert!(glob_match("a.txt", "*.txt"));
        assert!(!glob_match("a.png", "*.txt"));
        assert!(glob_match("README", "*"));
        assert!(glob_match("x", ""));
        assert!(glob_match("a.txt", "*.png, *.txt"));
        assert!(glob_match("Foo.TXT", "*.txt"));
        assert!(glob_match("ab", "a?"));
        assert!(!glob_match("abc", "a?"));
    }

    #[test]
    fn rgb_blend_endpoints_and_midpoint() {
        let a = Rgb::new(0x60, 0xa0, 0x80);
        let b = Rgb::new(0x00, 0x00, 0x00);
        // num=0 で自身、num=den で other。
        assert_eq!(a.blend(b, 0, 5), a);
        assert_eq!(a.blend(b, 5, 5), b);
        // 60% を黒へ寄せる＝各チャンネルが約 40% に減る。
        assert_eq!(a.blend(b, 3, 5), Rgb::new(0x26, 0x40, 0x33));
        // 白へ向けて 50% は中点。
        let w = Rgb::new(0xff, 0xff, 0xff);
        assert_eq!(b.blend(w, 1, 2), Rgb::new(0x7f, 0x7f, 0x7f));
    }

    #[test]
    fn item_color_chain() {
        let colors = Colors::default();
        let mut it = dir("d");
        assert_eq!(colors.item_color(&it), colors.directory);
        it.system = true;
        // system が最後勝ち。
        assert_eq!(colors.item_color(&it), colors.system);
    }

    #[test]
    fn auto_adjust_flex_and_clamp() {
        // [0]=FileBaseName(flex) [1]=種類 [2]=サイズ [3]=更新日時 [4]=属性。
        let mut cols = default_columns();
        // measured[0] はフレックスなので無視される。[3] は cap(=100) を超えるのでクランプ。
        let measured = [999, 30, 40, 200, 20];
        auto_adjust_columns(&mut cols, &measured, 400, 16, 10, 0.25, 8);
        // 非フレックスは measured+pad、上限 cap=400*0.25=100。
        assert_eq!(cols[1].width, 40); // 30+10
        assert_eq!(cols[2].width, 50); // 40+10
        assert_eq!(cols[3].width, 100); // 200+10=210 → 100 にクランプ
        assert_eq!(cols[4].width, 30); // 20+10
        // フレックスは残り幅 = avail - scrollbar - 固定合計(40+50+100+30=220)。
        assert_eq!(cols[0].width, 400 - 16 - 220);
    }

    #[test]
    fn auto_adjust_flex_floors_at_min() {
        // 固定列が広すぎて残りが負になっても、フレックスは min_flex(=50) で下げ止まる
        // （pane がさらに狭いと固定列が描画で画面外へはみ出る）。
        let mut cols = default_columns();
        let measured = [0, 80, 80, 80, 80];
        auto_adjust_columns(&mut cols, &measured, 120, 16, 4, 0.5, 50);
        assert_eq!(cols[0].width, 50);
    }

    #[test]
    fn column_sample_matches_cell_format() {
        // 内容追従させる列は空（フレックス／可変）。
        assert_eq!(column_sample(ColumnKind::FileBaseName), "");
        assert_eq!(column_sample(ColumnKind::FileExtension), "");
        // 日時の代表は実セルと同じ文字数（書式が一致している保証）。
        let dt = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(0);
        let mut tf = file("t");
        tf.modified = Some(dt);
        let cell = FileListState::new().cell_text(&tf, ColumnKind::LastWriteTime);
        assert_eq!(
            column_sample(ColumnKind::LastWriteTime).chars().count(),
            cell.chars().count()
        );
        // 属性の代表は全フラグ立ての実セル以上の長さ。
        let mut attr = file("x");
        attr.system = true;
        attr.hidden = true;
        attr.readonly = true;
        attr.archive = true;
        let acell = FileListState::new().cell_text(&attr, ColumnKind::Attribute);
        assert!(column_sample(ColumnKind::Attribute).chars().count() >= acell.chars().count());
        // サイズの代表は12桁＋3桁区切り。
        assert_eq!(column_sample(ColumnKind::Length), "999,999,999,999");
    }

    #[test]
    fn auto_adjust_measured_short_and_empty() {
        // measured が短いと不足分は 0 として扱う（min 8 でクランプ）。
        let mut cols = default_columns();
        auto_adjust_columns(&mut cols, &[0, 0], 400, 0, 0, 0.25, 8);
        assert_eq!(cols[1].width, 8);
        assert_eq!(cols[4].width, 8);
        // 空 columns は no-op（パニックしない）。
        let mut none: Vec<Column> = Vec::new();
        auto_adjust_columns(&mut none, &[], 400, 0, 4, 0.25, 8);
        assert!(none.is_empty());
    }
}
