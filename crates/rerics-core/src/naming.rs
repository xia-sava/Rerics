//! 名前の一意化。一覧に同名があるとき末尾へ ` (2)`, ` (3)` … を付けて衝突を避ける汎用
//! ユーティリティ。メニュー名のほか、将来のファイル衝突回避でも共用できる。

/// `base` が `existing` に無ければそのまま返し、あれば `base (2)`, `base (3)` … と空いている
/// 番号を付けて一意な名前にする。番号付き候補がさらに衝突する場合も空くまで進める。
pub fn unique_name(base: &str, existing: &[String]) -> String {
    if !existing.iter().any(|e| e == base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} ({n})"))
        .find(|cand| !existing.iter().any(|e| e == cand))
        .expect("無限列なので必ず空きが見つかる")
}

#[cfg(test)]
mod tests {
    use super::unique_name;

    #[test]
    fn keeps_name_when_no_collision() {
        let existing = vec!["a".to_string(), "b".to_string()];
        assert_eq!(unique_name("c", &existing), "c");
    }

    #[test]
    fn appends_number_on_collision() {
        let existing = vec!["a".to_string(), "b".to_string()];
        assert_eq!(unique_name("a", &existing), "a (2)");
    }

    #[test]
    fn skips_taken_numbers() {
        let existing =
            vec!["m".to_string(), "m (2)".to_string(), "m (3)".to_string()];
        assert_eq!(unique_name("m", &existing), "m (4)");
    }
}
