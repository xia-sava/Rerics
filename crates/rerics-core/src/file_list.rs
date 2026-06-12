//! ファイル一覧のモデル層（UI 非依存）。
//!
//! `FileItem`（1エントリ）・`SortType`（ソート種別）・比較器・`FileListState`
//! （カーソル/スクロール/選択/列を持つコントロール状態）・配色モデルを提供する。

use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Local, TimeZone};

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
    fn bare(name: String, is_dir: bool) -> Self {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortType {
    FileName,
    Extension,
    Length,
    CreateTime,
    LastWriteTime,
    Attribute,
    FileNameExpLike,
    ExtensionExpLike,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// 1列の表示定義。`width` は論理 px（DPI スケールは GUI 層）。
#[derive(Debug, Clone)]
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

/// RGB 三つ組の配色（GUI 層で COLORREF へ変換）。
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
}

/// 配色モデル（既定は黒背景のダークテーマ）。
#[derive(Debug, Clone, Copy)]
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
    pub log_info: Rgb,
    pub log_warning: Rgb,
    pub log_error: Rgb,
}

impl Default for Colors {
    fn default() -> Self {
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
            log_background: Rgb::new(0x10, 0x10, 0x10),
            log_info: Rgb::new(0xc8, 0xc8, 0xc8),
            log_warning: Rgb::new(0xff, 0xd0, 0x40),
            log_error: Rgb::new(0xff, 0x70, 0x70),
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
            ColumnKind::Length => {
                if item.is_dir {
                    "<DIR>".to_owned()
                } else {
                    format_size(item.size.unwrap_or(0))
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn file(name: &str) -> FileItem {
        FileItem::bare(name.to_owned(), false)
    }

    fn dir(name: &str) -> FileItem {
        FileItem::bare(name.to_owned(), true)
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
    fn item_color_chain() {
        let colors = Colors::default();
        let mut it = dir("d");
        assert_eq!(colors.item_color(&it), colors.directory);
        it.system = true;
        // system が最後勝ち。
        assert_eq!(colors.item_color(&it), colors.system);
    }
}
