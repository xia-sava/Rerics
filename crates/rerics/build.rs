//! サードパーティライセンス一覧を cargo-about で集めて `OUT_DIR/licenses.txt` へ書き出す。
//! バージョン情報ダイアログがこれを `include_str!` で取り込む。cargo-about はデータを
//! HTML エスケープして埋めるので、プレーンテキスト向けに復元する。依存（Cargo.lock）か
//! 設定・テンプレートが変わったときだけ再生成する。cargo-about が無い/失敗してもビルドは
//! 止めず、プレースホルダを書いて警告する。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=about.toml");
    println!("cargo:rerun-if-changed=about.hbs");
    println!("cargo:rerun-if-changed=../../Cargo.lock");

    embed_icon();
    copy_pdfium_dll();

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

/// 同梱の PDFium DLL を実行ファイルと同じディレクトリへ配置する。実行時は exe 隣の
/// `pdfium.dll` を `Pdfium::bind_to_library` で動的ロードする（`src/pdf.rs`）。
/// コピーに失敗してもビルドは止めず警告に留める（PDF 表示だけが無効になる）。
#[cfg(windows)]
fn copy_pdfium_dll() {
    const SRC: &str = "../../vendor/pdfium/pdfium.dll";
    println!("cargo:rerun-if-changed={SRC}");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    // OUT_DIR = target/<profile>/build/<pkg>/out → 3 つ上が target/<profile>/。
    let Some(profile_dir) = Path::new(&out_dir).ancestors().nth(3) else {
        println!("cargo:warning=出力プロファイルのディレクトリを特定できませんでした");
        return;
    };
    // 実行ファイル隣（配布・本番起動）とテストバイナリ隣（target/<profile>/deps）の両方へ置く。
    // どちらも実行時に exe 同ディレクトリの pdfium.dll を探すため。
    for dir in [profile_dir.to_path_buf(), profile_dir.join("deps")] {
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join("pdfium.dll");
        if let Err(e) = std::fs::copy(SRC, &dest) {
            println!(
                "cargo:warning=pdfium.dll のコピーに失敗しました（{}）: {e}",
                dir.display()
            );
        }
    }
}

#[cfg(not(windows))]
fn copy_pdfium_dll() {}

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
