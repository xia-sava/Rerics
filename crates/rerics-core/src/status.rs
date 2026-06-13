//! ステータスバー表示用の現代化フォーマッタ（サイズ・選択情報・ドライブ容量）。UI 非依存。

/// バイト数を B/KB/MB/GB で表す。GB を最大単位とし、KB 以上は小数1桁＋整数部3桁区切り、
/// B は整数で表示する（"512 B" / "130.4 MB" / "1,880.2 GB"）。
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes < KB {
        return format!("{bytes} B");
    }
    let (value, unit) = if bytes < MB {
        (bytes as f64 / KB as f64, "KB")
    } else if bytes < GB {
        (bytes as f64 / MB as f64, "MB")
    } else {
        (bytes as f64 / GB as f64, "GB")
    };
    format!("{} {}", decimal1_grouped(value), unit)
}

/// 非負の値を小数1桁・整数部3桁区切りの文字列にする（1880.25 → "1,880.2"）。
fn decimal1_grouped(v: f64) -> String {
    let s = format!("{v:.1}");
    let (int_part, frac_part) = s.split_once('.').unwrap_or((s.as_str(), "0"));
    format!("{}.{}", group_thousands(int_part), frac_part)
}

/// 整数の桁文字列に3桁区切りのカンマを挿入する（"1880" → "1,880"）。
fn group_thousands(digits: &str) -> String {
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 選択情報（"選択 N 個 {size}"）。0件なら `None`。
pub fn format_selected(count: u64, size: u64) -> Option<String> {
    if count == 0 {
        return None;
    }
    Some(format!("選択 {} 個 {}", count, format_size(size)))
}

/// ドライブ容量（"{letter} 空き {free} / 全 {total}"）。`letter` はレター表記（"C:" 等）。
pub fn format_drive(letter: &str, free: u64, total: u64) -> String {
    format!("{} 空き {} / 全 {}", letter, format_size(free), format_size(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    #[test]
    fn size_bytes_are_integer() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn size_kb_mb_gb_have_one_decimal() {
        assert_eq!(format_size(KB), "1.0 KB");
        assert_eq!(format_size(KB + KB / 2), "1.5 KB");
        assert_eq!(format_size(MB), "1.0 MB");
        assert_eq!(format_size(GB), "1.0 GB");
    }

    #[test]
    fn size_caps_at_gb() {
        assert_eq!(format_size(2 * TB), "2,048.0 GB");
    }

    #[test]
    fn size_groups_thousands() {
        assert_eq!(format_size(1880 * GB + GB / 5), "1,880.2 GB");
        assert_eq!(format_size(1_000_000 * GB), "1,000,000.0 GB");
    }

    #[test]
    fn selected_zero_is_none() {
        assert_eq!(format_selected(0, 1234), None);
    }

    #[test]
    fn selected_formats_count_and_size() {
        assert_eq!(format_selected(3, KB + KB / 2).as_deref(), Some("選択 3 個 1.5 KB"));
        assert_eq!(format_selected(1, 0).as_deref(), Some("選択 1 個 0 B"));
    }

    #[test]
    fn drive_formats_free_and_total() {
        assert_eq!(format_drive("C:", KB, 2 * KB), "C: 空き 1.0 KB / 全 2.0 KB");
    }

    #[test]
    fn group_thousands_boundaries() {
        assert_eq!(group_thousands("0"), "0");
        assert_eq!(group_thousands("999"), "999");
        assert_eq!(group_thousands("1000"), "1,000");
        assert_eq!(group_thousands("1234567"), "1,234,567");
    }
}
