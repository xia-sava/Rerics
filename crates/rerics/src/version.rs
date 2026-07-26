//! アプリのバージョン表記。`major.minor` は `Cargo.toml`、patch は CI が
//! `RERICS_BUILD_NUMBER`（GitHub Actions の run number）として埋め込む単調増加のビルド番号。
//! 自動更新（[`crate::update`]）の版比較もこのビルド番号で行う。

/// 埋め込み済みの自ビルド番号。CI ビルド（`RERICS_BUILD_NUMBER` 環境変数つき）以外は 0
/// （自動更新では常に「更新あり」と判定される＝ローカル開発ビルド向けの妥当な既定）。
pub(crate) fn build_number() -> u64 {
    option_env!("RERICS_BUILD_NUMBER").and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// 自ビルドのバージョン文字列（`1.0.123` 形式）。
pub(crate) fn full() -> String {
    format!(
        "{}.{}.{}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        build_number()
    )
}
