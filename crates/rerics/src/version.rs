//! アプリのバージョン表記。`Cargo.toml` の `[workspace.package]` の version（MAJOR.MINOR.PATCH）を
//! そのまま使う。リリースはこの版と同じ名前のタグ（`vMAJOR.MINOR.PATCH`）を打って作るので、
//! 表記と配布物の版は一致する。自動更新（[`crate::update`]）の版比較もこの版で行う。

/// 自ビルドのバージョン文字列（`1.1.0` 形式）。
pub(crate) fn full() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `MAJOR.MINOR.PATCH` を数の組にする（先頭の `v` は許す）。形が違えば `None`。
pub(crate) fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// `candidate` が自ビルドより新しい版か。どちらかが解釈できなければ新しくないとみなす。
pub(crate) fn is_newer_than_current(candidate: &str) -> bool {
    match (parse(candidate), parse(full())) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_semver_with_optional_v() {
        assert_eq!(parse("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse("v10.0.42"), Some((10, 0, 42)));
        assert_eq!(parse("1.2"), None);
        assert_eq!(parse("1.2.3.4"), None);
        assert_eq!(parse("1.2.x"), None);
    }

    #[test]
    fn compares_numerically_not_lexically() {
        assert!(parse("1.10.0") > parse("1.9.9"));
        assert!(parse("1.1.0") > parse("1.0.150"));
    }

    #[test]
    fn current_version_is_parseable() {
        assert!(parse(full()).is_some());
        assert!(!is_newer_than_current(full()));
        assert!(!is_newer_than_current("not-a-version"));
    }
}
