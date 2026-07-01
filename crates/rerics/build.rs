//! サードパーティライセンス一覧を cargo-about で集めて `OUT_DIR/licenses.txt` へ書き出す。
//! バージョン情報ダイアログがこれを `include_str!` で取り込む。cargo-about はデータを
//! HTML エスケープして埋めるので、プレーンテキスト向けに復元する。依存（Cargo.lock）か
//! 設定・テンプレートが変わったときだけ再生成する。cargo-about が無い/失敗してもビルドは
//! 止めず、プレースホルダを書いて警告する。

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=about.toml");
    println!("cargo:rerun-if-changed=about.hbs");
    println!("cargo:rerun-if-changed=../../Cargo.lock");

    embed_icon();

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let dest = PathBuf::from(&out_dir).join("licenses.txt");
    let raw = PathBuf::from(&out_dir).join("licenses.raw.txt");

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let result = Command::new(&cargo)
        .args(["about", "generate", "about.hbs", "-o"])
        .arg(&raw)
        .status();

    let text = match result {
        Ok(s) if s.success() => match std::fs::read_to_string(&raw) {
            Ok(t) => Some(to_crlf(&html_unescape(&t))),
            Err(e) => {
                println!("cargo:warning=ライセンス一覧の読込に失敗: {e}");
                None
            }
        },
        Ok(s) => {
            println!(
                "cargo:warning=cargo-about が失敗しました（exit {:?}）。`cargo install cargo-about --features cli` で導入できます。",
                s.code()
            );
            None
        }
        Err(e) => {
            println!(
                "cargo:warning=cargo-about を実行できません（{e}）。`cargo install cargo-about --features cli` で導入できます。"
            );
            None
        }
    };

    let text = text.unwrap_or_else(|| {
        "（サードパーティライセンス一覧の生成に失敗しました。cargo-about を導入して再ビルドしてください。）\n"
            .to_string()
    });
    std::fs::write(&dest, text).expect("write licenses.txt");
}

/// アプリアイコンを実行ファイルへ埋め込む（Explorer/タスクバー用）。リソース ID 1 で入れ、
/// ウィンドウ側は `WindowMainOpts.class_icon = Icon::Id(1)` で同じアイコンを参照する。
/// リソースコンパイラが無い等で失敗してもビルドは止めず警告に留める。
#[cfg(windows)]
fn embed_icon() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id("assets/icon.ico", "1");
    if let Err(e) = res.compile() {
        println!("cargo:warning=アプリアイコンの埋め込みに失敗しました: {e}");
    }
}

#[cfg(not(windows))]
fn embed_icon() {}

/// プレーンテキスト出力向けに最小限の HTML 実体参照を復元する。`&amp;` は二重復元を避けるため最後に。
fn html_unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// 改行を CRLF に揃える（Win32 マルチライン Edit は LF 単体では改行表示しない）。
fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}
