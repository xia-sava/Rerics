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
    pub accessed: Option<SystemTime>,
    pub readonly: bool,
    pub hidden: bool,
    pub system: bool,
    pub archive: bool,
    pub reparse: bool,
    pub selected: bool,
    /// この項目が属する場所（VFS）。通常一覧では `None`（ペインの現在地が場所）。検索・比較などの
    /// 結果一覧では、項目が出自のディレクトリをまたぐので、その出自の場所をここに持つ。
    pub source: Option<crate::vfs::Location>,
    /// 結果一覧で表示する補助情報（相対サブパスや "追加"/"削除"/"新しい" などの説明）。
    pub info: Option<String>,
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
            accessed: None,
            readonly: false,
            hidden: false,
            system: false,
            archive: false,
            reparse: false,
            selected: false,
            source: None,
            info: None,
        }
    }

    /// この項目が属する場所（VFS）を返す。`source` があればそれを、無ければ与えられたペインの
    /// 現在地 `pane` を使う（通常一覧の項目はペイン現在地が場所）。
    pub fn source_or(&self, pane: &crate::vfs::Location) -> crate::vfs::Location {
        self.source.clone().unwrap_or_else(|| pane.clone())
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
        it.accessed = meta.accessed().ok();
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

/// 名前変換メニュー（原作 frmRename）の種別。`apply` で名前へ変換を適用する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NameCase {
    /// 変換しない。
    #[default]
    None,
    /// 名前全体を大文字に。
    Upper,
    /// 名前全体を小文字に。
    Lower,
    /// 拡張子だけ大文字に（主部は保つ）。
    ExtUpper,
    /// 拡張子だけ小文字に（主部は保つ）。
    ExtLower,
}

impl NameCase {
    /// `name` に変換を適用した新しい名前を返す。`is_dir` のときは拡張子なし扱い。
    pub fn apply(self, name: &str, is_dir: bool) -> String {
        match self {
            NameCase::None => name.to_owned(),
            NameCase::Upper => name.to_uppercase(),
            NameCase::Lower => name.to_lowercase(),
            NameCase::ExtUpper => {
                let (base, ext) = split_base_ext(name, is_dir);
                format!("{base}{}", ext.to_uppercase())
            }
            NameCase::ExtLower => {
                let (base, ext) = split_base_ext(name, is_dir);
                format!("{base}{}", ext.to_lowercase())
            }
        }
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

/// ファイルサイズ列の表記スタイル（原作 `CustomSizeStyle` 由来・単位は Unit1 固定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SizeFormat {
    /// 全バイトをカンマ区切り（例 `1,234,567`）。
    #[default]
    Detail,
    /// 常に単位＋小数1桁（例 `1.2 MB` / `500.0 KB`）。原作 Simple1。
    Simple1,
    /// 1MB 未満はバイト、以上は単位＋小数1桁（例 `512,000` / `1.2 MB`）。原作 Simple2。
    Simple2,
    /// KB 単位の整数固定（エクスプローラ風・例 `1,229 KB`）。原作 Explorer。
    Explorer,
}

/// バイト数を `SizeFormat` に従って整形する。単位ラベルは原作 Unit1（なし/ KB/ MB/ GB/ TB）。
pub fn format_size_styled(n: u64, fmt: SizeFormat) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    match fmt {
        SizeFormat::Detail => format_size(n),
        SizeFormat::Simple1 => {
            let (v, u) = if n >= TB {
                (n as f64 / TB as f64, " TB")
            } else if n >= GB {
                (n as f64 / GB as f64, " GB")
            } else if n >= MB {
                (n as f64 / MB as f64, " MB")
            } else {
                (n as f64 / KB as f64, " KB")
            };
            format!("{}{}", decimal1_grouped(v), u)
        }
        SizeFormat::Simple2 => {
            if n >= TB {
                format!("{} TB", decimal1_grouped(n as f64 / TB as f64))
            } else if n >= GB {
                format!("{} GB", decimal1_grouped(n as f64 / GB as f64))
            } else if n >= MB {
                format!("{} MB", decimal1_grouped(n as f64 / MB as f64))
            } else {
                format_size(n)
            }
        }
        SizeFormat::Explorer => {
            let kb = ((n as f64) / KB as f64).round() as u64;
            format!("{} KB", format_size(kb))
        }
    }
}

/// 非負の値を小数1桁・整数部3桁区切りにする（1880.25 → "1,880.2"）。
fn decimal1_grouped(v: f64) -> String {
    let s = format!("{v:.1}");
    let (int_part, frac) = s.split_once('.').unwrap_or((s.as_str(), "0"));
    let int_grouped = int_part.parse::<u64>().map(format_size).unwrap_or_else(|_| int_part.to_owned());
    format!("{int_grouped}.{frac}")
}

impl SortType {
    /// リテラル引数（`sort("name")` 等）からソート種別を解釈する。大小無視。
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

    /// 種別を正規トークン文字列にする（`from_token` で読み戻せる）。スクリプトの
    /// `getSortType` が返す値。
    pub fn as_token(self) -> &'static str {
        match self {
            Self::FileName => "fileName",
            Self::Extension => "extension",
            Self::Length => "length",
            Self::CreateTime => "createTime",
            Self::LastWriteTime => "lastWriteTime",
            Self::Attribute => "attribute",
            Self::FileNameExpLike => "fileNameExpLike",
            Self::ExtensionExpLike => "extensionExpLike",
        }
    }
}

/// ファイル名同士を比較する。Windows ではユーザの既定ロケールの言語的照合
/// （エクスプローラと同様に記号が英数字より前に並ぶ）を用い、それ以外の
/// プラットフォームではコードポイント順にフォールバックする。
/// 引数は大文字小文字を畳んだうえで渡される前提で、フラグは無指定にする。
#[cfg(windows)]
fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CompareStringEx(
            locale: *const u16,
            flags: u32,
            str1: *const u16,
            len1: i32,
            str2: *const u16,
            len2: i32,
            version: *const core::ffi::c_void,
            reserved: *const core::ffi::c_void,
            param: isize,
        ) -> i32;
    }
    let w1: Vec<u16> = a.encode_utf16().collect();
    let w2: Vec<u16> = b.encode_utf16().collect();
    // locale=NULL はユーザ既定ロケール。戻り値 1/2/3 が Less/Equal/Greater、
    // 0 は失敗なのでコードポイント順へフォールバックする。
    let r = unsafe {
        CompareStringEx(
            core::ptr::null(),
            0,
            w1.as_ptr(),
            w1.len() as i32,
            w2.as_ptr(),
            w2.len() as i32,
            core::ptr::null(),
            core::ptr::null(),
            0,
        )
    };
    match r {
        1 => Ordering::Less,
        3 => Ordering::Greater,
        2 => Ordering::Equal,
        _ => a.cmp(b),
    }
}

#[cfg(not(windows))]
fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    a.cmp(b)
}

/// 2エントリをソート種別で比較する（reverse なし）。親優先・dir 優先は呼び出し側で先に判定済み。
fn compare_kind(a: &FileItem, b: &FileItem, sort: SortType) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let by_name = || locale_compare(&a.name.to_uppercase(), &b.name.to_uppercase());
    match sort {
        SortType::FileName => by_name(),
        SortType::Extension => {
            let o = locale_compare(&a.extension.to_uppercase(), &b.extension.to_uppercase());
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
    if c1 == c2 || (digit_value(c1).is_some() && digit_value(c2).is_some()) {
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
                    _ => return locale_compare(rest1, rest2),
                }
            }
        }
    }
    locale_compare(input1, input2)
}

/// 1回分の「非数字プレフィクス＋数字列」マッチ結果。
struct ExpMatch {
    prefix: String,
    digits: String,
    /// マッチ終端の input 内バイト位置（match.Index + match.Length 相当）。
    end: usize,
}

/// ASCII 数字と全角数字（U+FF10..=U+FF19）を 0..=9 の値へ。
/// 全角数字を半角に揃えたうえで自然順比較するための変換を兼ねる。
fn digit_value(c: char) -> Option<u32> {
    match c {
        '0'..='9' => Some(c as u32 - '0' as u32),
        '０'..='９' => Some(c as u32 - '０' as u32),
        _ => None,
    }
}

/// `([^\d]*)(\d+)` を順次マッチするイテレータ。数字列は半角化して保持する。
struct ExpMatcher<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> ExpMatcher<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, pos: 0 }
    }

    fn next(&mut self) -> Option<ExpMatch> {
        // 非数字プレフィクス。
        let prefix_start = self.pos;
        let Some((doff, _)) = self.s[self.pos..]
            .char_indices()
            .find(|(_, c)| digit_value(*c).is_some())
        else {
            self.pos = self.s.len();
            return None;
        };
        let digit_start = self.pos + doff;
        let prefix = self.s[prefix_start..digit_start].to_owned();
        // 数字列（1文字以上・全角は半角化して格納）。
        let mut digits = String::new();
        let mut end = digit_start;
        for (off, c) in self.s[digit_start..].char_indices() {
            match digit_value(c) {
                Some(v) => {
                    digits.push((b'0' + v as u8) as char);
                    end = digit_start + off + c.len_utf8();
                }
                None => break,
            }
        }
        self.pos = end;
        Some(ExpMatch { prefix, digits, end })
    }
}

/// 列の表示種別（Icon は今回なし）。`Information` は検索・比較の結果一覧でだけ使う
/// 補助情報列（相対サブパスや "追加"/"削除" などの説明＝`FileItem.info`）。
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
    Information,
}

impl ColumnKind {
    /// この列のヘッダクリックで適用するソート種別。ファイル名・拡張子は、
    /// 現在のソートが自然順（ExpLike）であればその自然順版を維持する。
    pub fn sort_target(self, current: SortType) -> SortType {
        let exp_like = matches!(
            current,
            SortType::FileNameExpLike | SortType::ExtensionExpLike
        );
        match self {
            ColumnKind::FileName | ColumnKind::FileBaseName => {
                if exp_like { SortType::FileNameExpLike } else { SortType::FileName }
            }
            ColumnKind::FileExtension => {
                if exp_like { SortType::ExtensionExpLike } else { SortType::Extension }
            }
            ColumnKind::Length => SortType::Length,
            ColumnKind::CreateTime | ColumnKind::CreateTimeS => SortType::CreateTime,
            ColumnKind::LastWriteTime | ColumnKind::LastWriteTimeS => SortType::LastWriteTime,
            ColumnKind::Attribute => SortType::Attribute,
            // 補助情報列は対応するソート種別を持たない。見出しクリックは現在のソートを保つ。
            ColumnKind::Information => current,
        }
    }

    /// 列見出しの既定ラベル（日付の桁数違いは同じ見出し）。
    pub fn header_label(self) -> &'static str {
        match self {
            ColumnKind::FileName | ColumnKind::FileBaseName => "ファイル名",
            ColumnKind::FileExtension => "種類",
            ColumnKind::Length => "サイズ",
            ColumnKind::CreateTime | ColumnKind::CreateTimeS => "作成日時",
            ColumnKind::LastWriteTime | ColumnKind::LastWriteTimeS => "更新日時",
            ColumnKind::Attribute => "属性",
            ColumnKind::Information => "情報",
        }
    }

    /// この種別の既定の揃え（サイズのみ右寄せ）。
    pub fn default_align(self) -> Align {
        match self {
            ColumnKind::Length => Align::Right,
            _ => Align::Left,
        }
    }
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
        // 情報列は内容が可変長（相対パス等）なので内容追従させる。
        ColumnKind::FileName | ColumnKind::FileBaseName | ColumnKind::FileExtension | ColumnKind::Information => "",
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

/// 検索・比較の結果一覧の列構成。通常の列に末尾の情報列（出自の相対パスや説明）を足す。
pub fn result_columns() -> Vec<Column> {
    let mut cols = default_columns();
    cols.push(Column::new(ColumnKind::Information, "情報", 160, Align::Left));
    cols
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
    /// テキストビューアの背景。
    pub viewer_background: Rgb,
    /// テキストビューアの本文。
    pub viewer_text: Rgb,
    /// テキストビューアの行番号（gutter）。
    pub viewer_line: Rgb,
    /// テキストビューアの行番号と本文を仕切る縦線。
    pub viewer_separator: Rgb,
    /// テキストビューアの記号（改行マーク・[EOF]）。
    pub viewer_symbol: Rgb,
    /// テキストビューアの検索ヒット文字。
    pub viewer_find_text: Rgb,
    /// テキストビューアの検索ヒット背景。
    pub viewer_find_bg: Rgb,
    /// テキストビューアの検索カーソル行の下線。
    pub viewer_cursor: Rgb,
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
            directory: Rgb::new(0xff, 0xff, 0xff),
            readonly: Rgb::new(0x90, 0xee, 0x90),
            system: Rgb::new(0xf0, 0x80, 0x80),
            hidden: Rgb::new(0xad, 0xd8, 0xe6),
            background: Rgb::new(0, 0, 0),
            background2: Rgb::new(0x20, 0x20, 0x20),
            selected_file: Rgb::new(255, 255, 255),
            selected_file_bg: Rgb::new(0x2f, 0x60, 0x98),
            cursor: Rgb::new(0xff, 0xc0, 0x4d),
            log_background: Rgb::new(0x00, 0x00, 0x00),
            log_normal: Rgb::new(0xff, 0xff, 0xff),
            log_info: Rgb::new(0xad, 0xd8, 0xe6),
            log_warning: Rgb::new(0x90, 0xee, 0x90),
            log_error: Rgb::new(0xff, 0x00, 0x00),
            // テキストビューアの既定色（ダークは背景＝黒・本文＝白）。
            viewer_background: Rgb::new(0x00, 0x00, 0x00),
            viewer_text: Rgb::new(0xff, 0xff, 0xff),
            viewer_line: Rgb::new(0x00, 0x80, 0x00),
            viewer_separator: Rgb::new(0xa0, 0xa0, 0xa0),
            viewer_symbol: Rgb::new(0x80, 0x00, 0x80),
            viewer_find_text: Rgb::new(0x00, 0x00, 0x00),
            viewer_find_bg: Rgb::new(0xff, 0xff, 0x00),
            // 原作の Cursor=Blue。黒背景で沈まないよう明るい青に寄せる（意図準拠）。
            viewer_cursor: Rgb::new(0x4a, 0x90, 0xff),
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
            // テキストビューアの既定色（ライトは背景＝白・本文＝黒）。
            viewer_background: Rgb::new(0xff, 0xff, 0xff),
            viewer_text: Rgb::new(0x00, 0x00, 0x00),
            viewer_line: Rgb::new(0x00, 0x80, 0x00),
            viewer_separator: Rgb::new(0xa0, 0xa0, 0xa0),
            viewer_symbol: Rgb::new(0x80, 0x00, 0x80),
            viewer_find_text: Rgb::new(0x00, 0x00, 0x00),
            viewer_find_bg: Rgb::new(0xff, 0xff, 0x00),
            // 原作の Cursor=Blue（白背景なのでそのまま）。
            viewer_cursor: Rgb::new(0x00, 0x00, 0xff),
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

    /// スクロールバーつまみの色。トラック（`background2`）を本文色へ寄せた中間グレーで、
    /// dark/light どちらのテーマでも視認できる。リスト本体と設定プレビューで共通に使い、
    /// 両者の見た目がずれないようにする（専用の設定色は持たない）。
    pub fn scrollbar_thumb(&self) -> Rgb {
        self.background2.blend(self.file_normal, 2, 5)
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
    /// 日付ソートのときだけ昇降を追加で反転する（古い日付を先頭にできる）。既定 false。
    pub reverse_sort_date: bool,
    pub columns: Vec<Column>,
    /// 検索・比較の結果一覧を表示中か。`true` のとき項目は複数ディレクトリ出身の合成項目
    /// （各々 `FileItem.source`/`info` を持つ）で、情報列を出す。通常の再読込で `false` に戻る。
    pub find_result: bool,
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
            reverse_sort_date: false,
            columns: default_columns(),
            find_result: false,
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

    /// 検索・比較の結果一覧へ切り替える。与えられた合成項目（各々 `source`/`info` を持つ）を
    /// そのまま並べ、情報列を出し、カーソルを先頭へ戻す。項目の並びは呼び出し側（ワーカー）が
    /// 確定済みとして保持する（ここでは並べ替えない）。通常の再読込で `find_result` は解除される。
    pub fn set_find_result(&mut self, items: Vec<FileItem>) {
        self.items = items;
        self.columns = result_columns();
        self.find_result = true;
        self.cursor = 0;
        self.scroll_top = 0;
        self.select_start = 0;
    }

    /// 検索・比較の結果一覧を空（先頭の ".." のみ）で開始する。以降は [`push_find_result`]
    /// で見つかった項目を1件ずつ追記していく（ライブ追加）。
    ///
    /// [`push_find_result`]: Self::push_find_result
    pub fn begin_find_result(&mut self) {
        self.set_find_result(vec![FileItem::parent()]);
    }

    /// ライブ追加中の結果一覧へ項目を1件足す（末尾へ追記・並べ替えはしない）。
    /// 結果モードでないときは何もしない。
    pub fn push_find_result(&mut self, item: FileItem) {
        if self.find_result {
            self.items.push(item);
        }
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

    /// カーソルを `target` へ移動する。`select` のとき、アンカー(`select_start`)から
    /// 移動後の位置までを範囲マークする（それ以外のマークは落とす）。`select` でないときは
    /// アンカーを移動後の位置へ追従させる（原作 `CursorXxx(bool select)` 準拠）。
    pub fn move_cursor(&mut self, target: isize, page_rows: usize, select: bool) {
        self.set_cursor(target, page_rows);
        if select {
            self.clear_all();
            self.select_files(self.select_start, self.cursor);
        } else {
            self.select_start = self.cursor;
        }
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

    /// items をソートする。`reverse_sort_date` が有効なら日付ソートのみ昇降を追加反転する。
    pub fn sort(&mut self, sort: SortType, reverse: bool) {
        self.sort_type = sort;
        self.sort_reverse = reverse;
        let effective = reverse ^ (self.reverse_sort_date && sort == SortType::LastWriteTime);
        self.items
            .sort_by(|a, b| compare_items(a, b, sort, effective));
    }

    /// 列のセルテキストを生成する。
    pub fn cell_text(&self, item: &FileItem, kind: ColumnKind, size_format: SizeFormat) -> String {
        match kind {
            ColumnKind::FileName => item.name.clone(),
            ColumnKind::FileBaseName => item.base_name.clone(),
            ColumnKind::FileExtension => item.extension.clone(),
            ColumnKind::Length => match item.size {
                // サイズが取れていればディレクトリでも数値表示（書庫内 dir 等）。
                Some(sz) => format_size_styled(sz, size_format),
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
            ColumnKind::Information => item.info.clone().unwrap_or_default(),
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
        if i > 0 && (len - i).is_multiple_of(3) {
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

/// 連番リネームの主部・拡張子それぞれに掛ける大文字小文字変換（原作 frmRenameSeq の4択）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeqCase {
    /// 命名規則通り（変換しない）。
    #[default]
    None,
    /// 大文字。
    Upper,
    /// 小文字。
    Lower,
    /// 先頭大文字（残りは小文字）。
    Capitalize,
}

impl SeqCase {
    /// ラジオ選択 index（0=命名規則通り/1=大文字/2=小文字/3=先頭大文字）から作る。
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => SeqCase::Upper,
            2 => SeqCase::Lower,
            3 => SeqCase::Capitalize,
            _ => SeqCase::None,
        }
    }

    /// 主部（拡張子なし）へ適用する。
    fn apply_base(self, s: &str) -> String {
        match self {
            SeqCase::None => s.to_owned(),
            SeqCase::Upper => s.to_uppercase(),
            SeqCase::Lower => s.to_lowercase(),
            SeqCase::Capitalize => capitalize(s),
        }
    }

    /// 拡張子（先頭ドットつき・空もあり）へ適用する。ドットは保ったまま中身を変換する。
    fn apply_ext(self, s: &str) -> String {
        match self {
            SeqCase::None => s.to_owned(),
            SeqCase::Upper => s.to_uppercase(),
            SeqCase::Lower => s.to_lowercase(),
            SeqCase::Capitalize => match s.strip_prefix('.') {
                Some(rest) => format!(".{}", capitalize(rest)),
                None => capitalize(s),
            },
        }
    }
}

/// 先頭1文字を大文字、残りを小文字にする。
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// 連番リネームの命名規則テンプレートを解釈し、新しい名前を生成する純関数
/// （プレビューと実行で共用）。トークンは `<F:r>`=元主部・`<F:e>`=元拡張子（ドット込み）・
/// `<No>`=連番・`<No:0000>`=`0` の個数で桁数を指定したゼロ埋め連番。連番は `start` から
/// `step` 刻み。展開後の名前を主部/拡張子へ再分割し、それぞれに変換を適用して再結合する。
/// 各 `items[i]` は `(名前, ディレクトリか)`。
pub fn sequence_rename(
    items: &[(String, bool)],
    template: &str,
    start: u64,
    step: u64,
    base_case: SeqCase,
    ext_case: SeqCase,
) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .map(|(i, (name, is_dir))| {
            let no = start + i as u64 * step;
            let (obase, oext) = split_base_ext(name, *is_dir);
            let expanded = expand_seq_template(template, &obase, &oext, no);
            let (base, ext) = split_base_ext(&expanded, *is_dir);
            format!("{}{}", base_case.apply_base(&base), ext_case.apply_ext(&ext))
        })
        .collect()
}

/// テンプレート1件分を展開する。未知の `<...>` はそのまま残し、閉じない `<` は文字として扱う。
fn expand_seq_template(template: &str, base: &str, ext: &str, no: u64) -> String {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<'
            && let Some(rel) = chars[i + 1..].iter().position(|&c| c == '>') {
                let token: String = chars[i + 1..i + 1 + rel].iter().collect();
                out.push_str(&expand_seq_token(&token, base, ext, no));
                i += rel + 2;
                continue;
            }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `<...>` 内の1トークンを値へ。`F:r`→主部・`F:e`→拡張子・`F`→元の名前・`No`/`No:fmt`→連番。
fn expand_seq_token(token: &str, base: &str, ext: &str, no: u64) -> String {
    let (name, arg) = match token.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (token, None),
    };
    match name.to_ascii_uppercase().as_str() {
        "F" => match arg.map(str::to_ascii_uppercase) {
            Some(a) if a.contains('E') => ext.to_owned(),
            Some(a) if a.contains('R') => base.to_owned(),
            _ => format!("{base}{ext}"),
        },
        "NO" => {
            let width = arg.map_or(0, |a| a.chars().filter(|&c| c == '0').count());
            format!("{no:0width$}")
        }
        _ => format!("<{token}>"),
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

    #[test]
    fn sort_type_token_round_trips() {
        for t in [
            SortType::FileName,
            SortType::Extension,
            SortType::Length,
            SortType::CreateTime,
            SortType::LastWriteTime,
            SortType::Attribute,
            SortType::FileNameExpLike,
            SortType::ExtensionExpLike,
        ] {
            assert_eq!(SortType::from_token(t.as_token()), Some(t), "token: {}", t.as_token());
        }
    }

    fn file(name: &str) -> FileItem {
        FileItem::bare(name.to_owned(), false)
    }

    fn dir(name: &str) -> FileItem {
        FileItem::bare(name.to_owned(), true)
    }

    #[test]
    fn source_or_falls_back_to_pane_location() {
        use crate::vfs::Location;
        let pane = Location::Real(std::path::PathBuf::from("C:\\pane"));

        // 通常項目（source 無し）はペイン現在地を場所とする。
        let normal = file("a.txt");
        assert!(normal.source.is_none());
        assert!(matches!(normal.source_or(&pane), Location::Real(p) if p == std::path::Path::new("C:\\pane")));

        // 結果項目（source 有り）は自身の出自を場所とする。
        let mut result = file("b.txt");
        result.source = Some(Location::Real(std::path::PathBuf::from("C:\\other")));
        result.info = Some("追加".to_owned());
        assert!(matches!(result.source_or(&pane), Location::Real(p) if p == std::path::Path::new("C:\\other")));
    }

    #[test]
    fn information_column_renders_info_field() {
        let mut it = file("b.txt");
        it.info = Some("sub\\dir".to_owned());
        let s = FileListState::new();
        assert_eq!(s.cell_text(&it, ColumnKind::Information, SizeFormat::Detail), "sub\\dir");
        // info 無しは空文字。
        let plain = file("c.txt");
        assert_eq!(s.cell_text(&plain, ColumnKind::Information, SizeFormat::Detail), "");
    }

    #[test]
    fn result_columns_appends_information() {
        let cols = result_columns();
        assert_eq!(cols.last().map(|c| c.kind), Some(ColumnKind::Information));
        // 通常列はそのまま含む。
        assert!(cols.iter().any(|c| c.kind == ColumnKind::FileBaseName));
    }

    #[test]
    fn set_find_result_switches_mode_columns_and_cursor() {
        use crate::vfs::Location;
        let mut s = FileListState::new();
        s.cursor = 3;
        assert!(!s.find_result);
        assert!(!s.columns.iter().any(|c| c.kind == ColumnKind::Information));

        let mut a = file("a.txt");
        a.source = Some(Location::Real(std::path::PathBuf::from("C:\\x")));
        a.info = Some("追加".to_owned());
        s.set_find_result(vec![FileItem::parent(), a]);

        assert!(s.find_result);
        assert_eq!(s.cursor, 0);
        assert_eq!(s.count(), 2);
        assert!(s.columns.iter().any(|c| c.kind == ColumnKind::Information));
    }

    #[test]
    fn begin_then_push_appends_live() {
        let mut s = FileListState::new();
        s.cursor = 3;
        s.begin_find_result();
        // 開始直後は ".." のみ・結果モード・情報列あり・カーソル先頭。
        assert!(s.find_result);
        assert_eq!(s.cursor, 0);
        assert_eq!(s.count(), 1);
        assert!(s.items[0].is_parent);
        assert!(s.columns.iter().any(|c| c.kind == ColumnKind::Information));
        // 1件ずつ追記すると末尾に積まれる。
        s.push_find_result(file("a.txt"));
        s.push_find_result(file("b.txt"));
        assert_eq!(s.count(), 3);
        assert_eq!(s.items[1].name, "a.txt");
        assert_eq!(s.items[2].name, "b.txt");
    }

    #[test]
    fn push_find_result_ignored_outside_result_mode() {
        let mut s = FileListState::new();
        let before = s.count();
        s.push_find_result(file("x.txt"));
        assert!(!s.find_result);
        assert_eq!(s.count(), before, "結果モードでなければ追記しない");
    }

    #[test]
    fn information_header_and_no_sort_change() {
        assert_eq!(ColumnKind::Information.header_label(), "情報");
        // 情報列の見出しクリックは現在のソートを保つ（専用ソート種別を持たない）。
        assert_eq!(ColumnKind::Information.sort_target(SortType::Length), SortType::Length);
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
    fn sequence_rename_expands_template() {
        let items = vec![
            ("Photo.JPG".to_owned(), false),
            ("note.txt".to_owned(), false),
            ("noext".to_owned(), false),
        ];
        // <F:r>_<No:0000><F:e>＝主部＋_＋4桁連番＋元拡張子（変換なし）。
        let out =
            sequence_rename(&items, "<F:r>_<No:0000><F:e>", 1, 1, SeqCase::None, SeqCase::None);
        assert_eq!(out, vec!["Photo_0001.JPG", "note_0002.txt", "noext_0003"]);
        // 固定主部＋3桁連番＋リテラル拡張子。開始0・刻み5。
        let out2 = sequence_rename(&items, "File<No:000>.dat", 0, 5, SeqCase::None, SeqCase::None);
        assert_eq!(out2, vec!["File000.dat", "File005.dat", "File010.dat"]);
        // <No>（桁指定なし）はゼロ埋めしない。<F:e> が無ければ拡張子は付かない。
        let out3 = sequence_rename(&items[..1], "<F:r>-<No>", 8, 1, SeqCase::None, SeqCase::None);
        assert_eq!(out3, vec!["Photo-8"]);
    }

    #[test]
    fn sequence_rename_applies_case() {
        let jpg = vec![("Photo.JPG".to_owned(), false)];
        assert_eq!(
            sequence_rename(&jpg, "<F:r><F:e>", 1, 1, SeqCase::Lower, SeqCase::Lower),
            vec!["photo.jpg"]
        );
        assert_eq!(
            sequence_rename(&jpg, "<F:r><F:e>", 1, 1, SeqCase::Upper, SeqCase::Upper),
            vec!["PHOTO.JPG"]
        );
        // 先頭大文字＝主部は先頭1字 upper＋残り lower、拡張子はドット維持＋先頭 upper＋残り lower。
        let cap = sequence_rename(
            &[("hELLo.HTML".to_owned(), false)],
            "<F:r><F:e>",
            1,
            1,
            SeqCase::Capitalize,
            SeqCase::Capitalize,
        );
        assert_eq!(cap, vec!["Hello.Html"]);
    }

    #[test]
    fn sequence_rename_directory_has_no_ext() {
        // ディレクトリは拡張子なし扱い＝全体が主部、<F:e> は空。
        let dir = vec![("my.folder".to_owned(), true)];
        assert_eq!(
            sequence_rename(&dir, "<F:r>_<No>", 1, 1, SeqCase::Upper, SeqCase::None),
            vec!["MY.FOLDER_1"]
        );
        assert_eq!(
            sequence_rename(&dir, "<F:r><F:e>", 1, 1, SeqCase::None, SeqCase::None),
            vec!["my.folder"]
        );
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
    fn name_case_apply() {
        assert_eq!(NameCase::None.apply("Foo.Txt", false), "Foo.Txt");
        assert_eq!(NameCase::Upper.apply("Foo.Txt", false), "FOO.TXT");
        assert_eq!(NameCase::Lower.apply("Foo.Txt", false), "foo.txt");
        assert_eq!(NameCase::ExtUpper.apply("Foo.Txt", false), "Foo.TXT");
        assert_eq!(NameCase::ExtLower.apply("Foo.Txt", false), "Foo.txt");
        // 拡張子なし・dir は Ext 変換で主部不変。
        assert_eq!(NameCase::ExtUpper.apply("noext", false), "noext");
        assert_eq!(NameCase::ExtLower.apply("My.Dir", true), "My.Dir");
        // 全体変換は dir でも効く。
        assert_eq!(NameCase::Upper.apply("My.Dir", true), "MY.DIR");
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
    fn reverse_sort_date_flips_only_date() {
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + std::time::Duration::from_secs(100);
        let mut a = file("old");
        a.modified = Some(t0);
        let mut b = file("new");
        b.modified = Some(t1);
        let mut s = FileListState::new();
        s.reverse_sort_date = true;
        s.items = vec![a, b];
        // 既定 reverse=false でも日付ソートは反転して古い順が先頭になる。
        s.sort(SortType::LastWriteTime, false);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["old", "new"]);
        // 名前ソートには影響しない。
        s.sort(SortType::FileName, false);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["new", "old"]);
    }

    #[test]
    fn explike_natural_order() {
        assert_eq!(exp_like_compare("FILE2", "FILE10"), Ordering::Less);
        assert_eq!(exp_like_compare("FILE10", "FILE2"), Ordering::Greater);
        assert_eq!(exp_like_compare("FILE2", "FILE2"), Ordering::Equal);
        // 等値プレフィクスで複数数字列。
        assert_eq!(exp_like_compare("V1-2", "V1-10"), Ordering::Less);
        // 9桁以上は数値比較せず通常の文字列比較へフォールバックする。
        assert_eq!(exp_like_compare("X1000000000", "X2"), Ordering::Less);
    }

    #[test]
    fn explike_fullwidth_digits() {
        // 全角数字は半角化して数値比較する（原作 StrConv.Narrow 相当）。
        assert_eq!(exp_like_compare("ファイル２", "ファイル１０"), Ordering::Less);
        assert_eq!(exp_like_compare("ファイル１０", "ファイル２"), Ordering::Greater);
        // 先頭が全角数字でも数字列として扱う。
        assert_eq!(exp_like_compare("２", "10"), Ordering::Less);
        // 半角と全角の混在も同値（半角化後に一致）。
        assert_eq!(exp_like_compare("A１", "A1"), Ordering::Equal);
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
    fn header_sort_target_follows_explike_mode() {
        // 通常ソート中は名前・拡張子とも通常版。
        assert_eq!(ColumnKind::FileName.sort_target(SortType::FileName), SortType::FileName);
        assert_eq!(
            ColumnKind::FileExtension.sort_target(SortType::FileName),
            SortType::Extension
        );
        // 自然順ソート中は名前・拡張子とも自然順版を維持する。
        assert_eq!(
            ColumnKind::FileName.sort_target(SortType::FileNameExpLike),
            SortType::FileNameExpLike
        );
        assert_eq!(
            ColumnKind::FileExtension.sort_target(SortType::FileNameExpLike),
            SortType::ExtensionExpLike
        );
        assert_eq!(
            ColumnKind::FileBaseName.sort_target(SortType::ExtensionExpLike),
            SortType::FileNameExpLike
        );
        // 名前以外の列は ExpLike モードに左右されない。
        assert_eq!(
            ColumnKind::Length.sort_target(SortType::FileNameExpLike),
            SortType::Length
        );
        assert_eq!(
            ColumnKind::CreateTimeS.sort_target(SortType::FileNameExpLike),
            SortType::CreateTime
        );
    }

    #[test]
    fn explike_sort_fullwidth_in_state() {
        let mut s = FileListState::new();
        s.items = vec![file("画像１０"), file("画像２"), file("画像１")];
        s.sort(SortType::FileNameExpLike, false);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["画像１", "画像２", "画像１０"]);
    }

    #[test]
    #[cfg(windows)]
    fn locale_compare_orders_symbols_before_letters() {
        use std::cmp::Ordering;
        // ユーザロケールの言語的照合では記号がアルファベットより前に並ぶ。
        assert_eq!(locale_compare("_NEW", "APPLE"), Ordering::Less);
        assert_eq!(locale_compare("APPLE", "BANANA"), Ordering::Less);
    }

    #[test]
    #[cfg(windows)]
    fn sort_symbol_prefix_before_letters() {
        // 通常ソートでも ExpLike でも記号始まりが先頭へ来る（エクスプローラ準拠）。
        for sort in [SortType::FileName, SortType::FileNameExpLike] {
            let mut s = FileListState::new();
            s.items = vec![file("apple"), file("_new"), file("Banana")];
            s.sort(sort, false);
            let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
            assert_eq!(names, vec!["_new", "apple", "Banana"], "sort={sort:?}");
        }
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
    fn move_cursor_plain_follows_anchor() {
        // 非 select の移動は select_start を移動後のカーソルへ追従させる（#61）。
        let mut s = FileListState::new();
        s.items = vec![FileItem::parent(), file("a"), file("b"), file("c")];
        s.select_start = 0;
        s.move_cursor(2, 10, false);
        assert_eq!(s.cursor, 2);
        assert_eq!(s.select_start, 2);
        // マークは付かない。
        assert!(s.items.iter().all(|it| !it.selected));
    }

    #[test]
    fn move_cursor_select_marks_anchor_range() {
        // select の移動はアンカー〜現在位置を範囲マークし、アンカーは固定（#60/#208）。
        let mut s = FileListState::new();
        s.items = vec![FileItem::parent(), file("a"), file("b"), file("c"), file("d")];
        s.select_start = 1;
        s.set_cursor(1, 10);
        // 1→3 を選択しながら移動。
        s.move_cursor(3, 10, true);
        assert_eq!(s.cursor, 3);
        assert_eq!(s.select_start, 1); // アンカーは動かない
        assert!(!s.items[0].selected);
        assert!(s.items[1].selected);
        assert!(s.items[2].selected);
        assert!(s.items[3].selected);
        assert!(!s.items[4].selected);
        // さらに 3→1 へ縮めると範囲外（旧マーク）は落ちる。
        s.move_cursor(1, 10, true);
        assert!(s.items[1].selected);
        assert!(!s.items[2].selected);
        assert!(!s.items[3].selected);
    }

    #[test]
    fn move_cursor_select_skips_parent() {
        // 範囲が親(..)を含んでも親はマークしない。
        let mut s = FileListState::new();
        s.items = vec![FileItem::parent(), file("a"), file("b")];
        s.select_start = 2;
        s.set_cursor(2, 10);
        s.move_cursor(0, 10, true);
        assert!(!s.items[0].selected);
        assert!(s.items[1].selected);
        assert!(s.items[2].selected);
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
            FileListState::new().cell_text(&d, ColumnKind::Length, SizeFormat::Detail),
            "<DIR>"
        );
        let mut f = file("big.bin");
        f.size = Some(1234567);
        assert_eq!(
            FileListState::new().cell_text(&f, ColumnKind::Length, SizeFormat::Detail),
            "1,234,567"
        );
        // #14: サイズが取れているディレクトリ（書庫内 dir 等）は数値表示。
        let mut sized_dir = dir("withsize");
        sized_dir.size = Some(4096);
        assert_eq!(
            FileListState::new().cell_text(&sized_dir, ColumnKind::Length, SizeFormat::Detail),
            "4,096"
        );
        // サイズ不明のファイルは 0 と詐称せず "--"。
        let unknown = file("unknown.bin");
        assert_eq!(
            FileListState::new().cell_text(&unknown, ColumnKind::Length, SizeFormat::Detail),
            "--"
        );
        let mut attr = file("x");
        attr.system = true;
        attr.hidden = true;
        attr.readonly = true;
        attr.archive = true;
        assert_eq!(
            FileListState::new().cell_text(&attr, ColumnKind::Attribute, SizeFormat::Detail),
            "SHRA"
        );
        let dt = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(0);
        let mut tf = file("t");
        tf.modified = Some(dt);
        let txt = FileListState::new().cell_text(&tf, ColumnKind::LastWriteTime, SizeFormat::Detail);
        assert_eq!(txt.len(), "yyyy/MM/dd HH:mm".len());
        assert_eq!(txt.matches('/').count(), 2);
    }

    #[test]
    fn size_format_styles() {
        let kb = 1024u64;
        let mb = kb * 1024;
        let gb = mb * 1024;
        // 詳細：全バイト・カンマ区切り。
        assert_eq!(format_size_styled(1_234_567, SizeFormat::Detail), "1,234,567");
        // Simple1：常に単位＋小数1桁。
        assert_eq!(format_size_styled(512, SizeFormat::Simple1), "0.5 KB");
        assert_eq!(format_size_styled(5 * mb, SizeFormat::Simple1), "5.0 MB");
        assert_eq!(format_size_styled(3 * gb + gb / 2, SizeFormat::Simple1), "3.5 GB");
        // Simple2：1MB 未満はバイト、以上は単位。
        assert_eq!(format_size_styled(500 * kb, SizeFormat::Simple2), "512,000");
        assert_eq!(format_size_styled(5 * mb, SizeFormat::Simple2), "5.0 MB");
        // Explorer：KB 整数固定。
        assert_eq!(format_size_styled(5 * mb, SizeFormat::Explorer), "5,120 KB");
        assert_eq!(format_size_styled(1536, SizeFormat::Explorer), "2 KB");
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
        let cell = FileListState::new().cell_text(&tf, ColumnKind::LastWriteTime, SizeFormat::Detail);
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
        let acell = FileListState::new().cell_text(&attr, ColumnKind::Attribute, SizeFormat::Detail);
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
