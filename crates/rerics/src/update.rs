//! GitHub Releases から最新ビルドを確認し、ダウンロードして自身を差し替える自動更新。
//!
//! リリースは push のたびに CI（`.github/workflows/release.yml`）がタグ `latest` を上書きして
//! 公開する（[`REPO`]・[`TAG`]）。バージョン比較は semver ではなく、CI が
//! `RERICS_BUILD_NUMBER`（GitHub Actions の run number）として埋め込む単調増加のビルド番号で行う。
//! 実行ファイルは実行中でも「リネーム」はできる（メモリマップは既存ハンドル経由で生き続ける）ため、
//! 差し替えは「退避（rename）→ 新ファイルを配置（copy）→ 新 exe を起動 → 自分は終了」の順で行う。
//! 退避した旧ファイル（`*.old`）は次回起動時（[`CLEANUP_ARG`] 付き起動）に削除する。

use std::io::Read;
use std::path::Path;
use std::time::Duration;

const REPO: &str = "xia-sava/Rerics";
const TAG: &str = "latest";

/// 更新適用後の再起動に付ける引数。旧ファイル（`*.old`）の掃除を要求する。
pub const CLEANUP_ARG: &str = "--cleanup-old-update";

#[derive(serde::Deserialize)]
struct Manifest {
    build: u64,
    commit: String,
    asset: String,
    sha256: String,
}

/// 確認できた新しいビルドの情報。
pub struct UpdateInfo {
    pub build: u64,
    pub commit: String,
    manifest: Manifest,
}

/// 埋め込み済みの自ビルド番号。CI ビルド（`RERICS_BUILD_NUMBER` 環境変数つき）以外は 0
/// （常に「更新あり」と判定される＝ローカル開発ビルド向けの妥当な既定）。
fn current_build() -> u64 {
    option_env!("RERICS_BUILD_NUMBER").and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn release_asset_url(name: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{TAG}/{name}")
}

/// native-tls（Windows は Schannel）を明示的に配線した Agent を作る。`ureq::get` 等の
/// ショートカット関数は既定の rustls 実装専用で、native-tls feature を有効にしただけでは
/// 使われない（`AgentBuilder::tls_connector` で渡して初めて有効になる）ため。
fn http_agent() -> Result<ureq::Agent, String> {
    let connector = native_tls::TlsConnector::new().map_err(|e| format!("TLS の初期化に失敗しました: {e}"))?;
    Ok(ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(connector)).build())
}

/// 最新ビルドを確認する。現在の自ビルドより新しければ `Some` を返す。
pub fn check_for_update() -> Result<Option<UpdateInfo>, String> {
    let manifest: Manifest = http_agent()?
        .get(&release_asset_url("manifest.json"))
        .call()
        .map_err(|e| format!("更新情報の取得に失敗しました: {e}"))?
        .into_json()
        .map_err(|e| format!("更新情報の解析に失敗しました: {e}"))?;
    if manifest.build <= current_build() {
        return Ok(None);
    }
    Ok(Some(UpdateInfo { build: manifest.build, commit: manifest.commit.clone(), manifest }))
}

/// 新しいビルドをダウンロードし、sha256 を検証してから自分の実行ファイル・付随 DLL を差し替える。
/// 成功時、置き換え済みの exe はまだ起動していない（呼び出し元が起動と自プロセス終了を行う）。
pub fn download_and_apply(info: &UpdateInfo) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("実行ファイルの場所が不明です: {e}"))?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| "実行ファイルのディレクトリが特定できません".to_string())?;

    let zip_bytes = download_bytes(&release_asset_url(&info.manifest.asset))?;
    verify_sha256(&zip_bytes, &info.manifest.sha256)?;

    let extract_dir = std::env::temp_dir().join(format!("rerics-update-{}", info.build));
    let result = (|| {
        extract_zip(&zip_bytes, &extract_dir)?;
        for name in ["rerics.exe", "pdfium.dll"] {
            let src = extract_dir.join(name);
            if src.exists() {
                replace_file(exe_dir, name, &src)?;
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&extract_dir);
    result
}

fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    http_agent()?
        .get(url)
        .call()
        .map_err(|e| format!("ダウンロードに失敗しました: {e}"))?
        .into_reader()
        .read_to_end(&mut buf)
        .map_err(|e| format!("ダウンロードの読み込みに失敗しました: {e}"))?;
    Ok(buf)
}

fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let actual: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    if !actual.eq_ignore_ascii_case(expected_hex) {
        return Err("ダウンロードしたファイルの検証に失敗しました（sha256 不一致）".to_string());
    }
    Ok(())
}

/// zip のバイト列を `dest` へ展開する。書庫内に `..` を含むエントリはスキップする
/// （`enclosed_name` がパストラバーサルを弾いた結果 `None` を返す）。
fn extract_zip(bytes: &[u8], dest: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("更新パッケージを開けません: {e}"))?;
    std::fs::create_dir_all(dest).map_err(|e| format!("展開先を作成できません: {e}"))?;
    for i in 0..archive.len() {
        let mut entry =
            archive.by_index(i).map_err(|e| format!("更新パッケージの読取に失敗しました: {e}"))?;
        let Some(name) = entry.enclosed_name() else { continue };
        let out_path = dest.join(name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `dir/name` を `dir/name.old` へ退避してから `src` を `dir/name` へ配置する。実行中の exe・
/// ロード済み DLL でもディレクトリエントリのリネームは可能（メモリマップは既存ハンドルで
/// 生き続ける）。退避に使う `.old` は前回分の残骸があれば先に消す。
fn replace_file(dir: &Path, name: &str, src: &Path) -> Result<(), String> {
    let target = dir.join(name);
    let old = dir.join(format!("{name}.old"));
    let _ = std::fs::remove_file(&old);
    if target.exists() {
        std::fs::rename(&target, &old).map_err(|e| format!("{name} を退避できません: {e}"))?;
    }
    std::fs::copy(src, &target).map_err(|e| format!("{name} を配置できません: {e}"))?;
    Ok(())
}

/// [`CLEANUP_ARG`] 付きで起動された（＝直前に自分自身を更新した）場合、旧プロセスが退避した
/// `*.old` を削除する。直前のプロセスがハンドルを手放すまで一瞬掛かることがあるため、短い
/// リトライを挟む。消せなくても致命的ではない（次回の更新時に上書きで片付く）ので無視する。
pub fn cleanup_old_files_if_requested() {
    if !std::env::args().any(|a| a == CLEANUP_ARG) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    for name in ["rerics.exe.old", "pdfium.dll.old"] {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        for _ in 0..20 {
            if std::fs::remove_file(&path).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
