//! デバッグ制御サーバの統合テスト（`--features debug-server` のときだけ走る）。
//!
//! 実 `rerics.exe` を `--debug-server=0 --headless` で起動し（`RERICS_DATA_DIR` で
//! ユーザ環境から隔離）、HTTP で `/state` 観測とコマンド注入を検証する。
//! core の単体テストでは届かない「実 DPI・実入力経路・実レイアウト」を踏む唯一の自動テスト。
//!
//! feature を有効にしてビルドした時点で「debug 検証をしたい」意思表示なので、`#[ignore]`
//! は付けない（feature 一本がゲート）。`Server` が**自動ポート＋ユニーク作業dir＋Drop 掃除**を
//! 引き受けるので、テストを増やしても**並列実行で衝突しない**し後始末も不要。
//! 実行：`./tools/dev.sh test --features debug-server`

#![cfg(feature = "debug-server")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// テストごとにユニークな作業dirを作るための連番（プロセスIDと併用）。
static SEQ: AtomicU32 = AtomicU32::new(0);

/// e2e 用の隔離済みデバッグサーバ。`--debug-server=0`（OS 割当ポート）＋ユニークな
/// `RERICS_DATA_DIR` で起動し、**Drop で子プロセスの kill と作業dirの削除まで自動**で行う。
/// テストは `Server::start(...)` して `req()` を叩くだけ＝ポート・後始末・並列衝突を
/// 一切気にしなくてよい。
struct Server {
    child: Child,
    port: u16,
    base: PathBuf,
}

impl Server {
    /// サンドボックス（`sandbox_files` を置く）＋差分 config を用意して起動する。
    /// 作業dirはプロセスID＋連番でユニーク化するので、並列テストでも衝突しない。
    /// `config_toml` が空なら config.toml は書かない（既定設定で起動）。
    fn start(sandbox_files: &[&str], config_toml: &str) -> Server {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("rerics_it_{}_{}", std::process::id(), n));
        let data = base.join("data");
        let sbx = base.join("sbx");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&sbx).unwrap();
        for f in sandbox_files {
            std::fs::write(sbx.join(f), b"x").unwrap();
        }
        // state.toml はシングルクォート（リテラル）なのでバックスラッシュをそのまま書ける。
        std::fs::write(
            data.join("state.toml"),
            format!(
                "active_tab = 0\nsplit_ratio = 0.5\n[[tabs]]\nleft = '{p}'\nright = '{p}'\nactive_right = false\n",
                p = sbx.display()
            ),
        )
        .unwrap();
        if !config_toml.is_empty() {
            std::fs::write(data.join("config.toml"), config_toml).unwrap();
        }

        let (child, port) = spawn_and_wait(&data, false);
        Server { child, port, base }
    }

    /// 左=実FSサンドボックス（`real_files`）、右=書庫(zip・`zip_entries`)で起動する。
    /// 書込み許可つき（`--debug-allow-write`）＝書庫への追加/移動/mkdir を駆動できる。
    /// 右ペインは zip の中へ入った状態で始まる。
    fn start_archive(real_files: &[(&str, &[u8])], zip_entries: &[(&str, &[u8])]) -> Server {
        Self::start_archive_cfg(real_files, zip_entries, "")
    }

    /// [`start_archive`] に差分 config.toml を併せて書く版（config 駆動の書庫挙動の検証用）。
    fn start_archive_cfg(
        real_files: &[(&str, &[u8])],
        zip_entries: &[(&str, &[u8])],
        config_toml: &str,
    ) -> Server {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("rerics_it_{}_{}", std::process::id(), n));
        let data = base.join("data");
        let sbx = base.join("sbx");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&sbx).unwrap();
        for (name, body) in real_files {
            std::fs::write(sbx.join(name), body).unwrap();
        }
        let zip = base.join("arc.zip");
        build_stored_zip(&zip, zip_entries);
        std::fs::write(
            data.join("state.toml"),
            format!(
                "active_tab = 0\nsplit_ratio = 0.5\n[[tabs]]\nleft = '{l}'\nright = '{r}'\nactive_right = false\n",
                l = sbx.display(),
                r = zip.display(),
            ),
        )
        .unwrap();
        if !config_toml.is_empty() {
            std::fs::write(data.join("config.toml"), config_toml).unwrap();
        }
        let (child, port) = spawn_and_wait(&data, true);
        Server { child, port, base }
    }

    /// `start` と同じだが書込み許可つき（`--debug-allow-write`）で起動する。
    /// 実FS を破壊的に変更するコマンド（連番リネーム等）の e2e 用。
    fn start_writable(sandbox_files: &[&str]) -> Server {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("rerics_it_{}_{}", std::process::id(), n));
        let data = base.join("data");
        let sbx = base.join("sbx");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&sbx).unwrap();
        for f in sandbox_files {
            std::fs::write(sbx.join(f), b"x").unwrap();
        }
        std::fs::write(
            data.join("state.toml"),
            format!(
                "active_tab = 0\nsplit_ratio = 0.5\n[[tabs]]\nleft = '{p}'\nright = '{p}'\nactive_right = false\n",
                p = sbx.display()
            ),
        )
        .unwrap();
        let (child, port) = spawn_and_wait(&data, true);
        Server { child, port, base }
    }

    /// `start_writable` の左右別ディレクトリ版。左に `left_files` を置き、右は空。左をアクティブ
    /// にして起動する。反対ペイン作成など L/R 方向が問われる挙動の検証用。
    fn start_writable_split(left_files: &[&str]) -> Server {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("rerics_it_{}_{}", std::process::id(), n));
        let data = base.join("data");
        let l = base.join("sbxL");
        let r = base.join("sbxR");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&l).unwrap();
        std::fs::create_dir_all(&r).unwrap();
        for f in left_files {
            std::fs::write(l.join(f), b"x").unwrap();
        }
        std::fs::write(
            data.join("state.toml"),
            format!(
                "active_tab = 0\nsplit_ratio = 0.5\n[[tabs]]\nleft = '{l}'\nright = '{r}'\nactive_right = false\n",
                l = l.display(),
                r = r.display()
            ),
        )
        .unwrap();
        let (child, port) = spawn_and_wait(&data, true);
        Server { child, port, base }
    }

    /// `start_writable` と同じだが、差分 config.toml を併せて書いて起動する。
    /// （ファイル操作の確認ダイアログ設定など、config 駆動の挙動を書込み許可下で検証する用。）
    fn start_writable_cfg(sandbox_files: &[&str], config_toml: &str) -> Server {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("rerics_it_{}_{}", std::process::id(), n));
        let data = base.join("data");
        let sbx = base.join("sbx");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&sbx).unwrap();
        for f in sandbox_files {
            std::fs::write(sbx.join(f), b"x").unwrap();
        }
        std::fs::write(
            data.join("state.toml"),
            format!(
                "active_tab = 0\nsplit_ratio = 0.5\n[[tabs]]\nleft = '{p}'\nright = '{p}'\nactive_right = false\n",
                p = sbx.display()
            ),
        )
        .unwrap();
        if !config_toml.is_empty() {
            std::fs::write(data.join("config.toml"), config_toml).unwrap();
        }
        let (child, port) = spawn_and_wait(&data, true);
        Server { child, port, base }
    }

    /// `start` と同じ隔離起動だが、`data/scripts/` にユーザスクリプト（名前→中身）を
    /// 置いてから起動する。起動時に名前順で読み込まれる。
    fn start_with_scripts(sandbox_files: &[&str], scripts: &[(&str, &str)]) -> Server {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("rerics_it_{}_{}", std::process::id(), n));
        let data = base.join("data");
        let sbx = base.join("sbx");
        std::fs::create_dir_all(data.join("scripts")).unwrap();
        std::fs::create_dir_all(&sbx).unwrap();
        for f in sandbox_files {
            std::fs::write(sbx.join(f), b"x").unwrap();
        }
        for (name, body) in scripts {
            std::fs::write(data.join("scripts").join(name), body.as_bytes()).unwrap();
        }
        std::fs::write(
            data.join("state.toml"),
            format!(
                "active_tab = 0\nsplit_ratio = 0.5\n[[tabs]]\nleft = '{p}'\nright = '{p}'\nactive_right = false\n",
                p = sbx.display()
            ),
        )
        .unwrap();
        let (child, port) = spawn_and_wait(&data, false);
        Server { child, port, base }
    }

    /// 左右ペインを **別々の実ディレクトリ**（`base/left`・`base/right`）に向けて起動する。
    /// それぞれに `(名前, 中身)` のファイルを置く。ディレクトリ比較のように左右で内容が違う
    /// 状況を、cd/フォーカスの順序に依存せず最初から用意するための起動口。
    fn start_dirs(left_files: &[(&str, &[u8])], right_files: &[(&str, &[u8])]) -> Server {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("rerics_it_{}_{}", std::process::id(), n));
        let data = base.join("data");
        let left = base.join("left");
        let right = base.join("right");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        for (name, body) in left_files {
            std::fs::write(left.join(name), body).unwrap();
        }
        for (name, body) in right_files {
            std::fs::write(right.join(name), body).unwrap();
        }
        std::fs::write(
            data.join("state.toml"),
            format!(
                "active_tab = 0\nsplit_ratio = 0.5\n[[tabs]]\nleft = '{l}'\nright = '{r}'\nactive_right = false\n",
                l = left.display(),
                r = right.display(),
            ),
        )
        .unwrap();
        let (child, port) = spawn_and_wait(&data, false);
        Server { child, port, base }
    }

    /// `start_dirs` と同じ左右2ディレクトリ構成だが、書込み許可つきで起動する
    /// （結果一覧からのコピー/移動など、実FS を変更する e2e 用）。
    fn start_dirs_writable(left_files: &[(&str, &[u8])], right_files: &[(&str, &[u8])]) -> Server {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("rerics_it_{}_{}", std::process::id(), n));
        let data = base.join("data");
        let left = base.join("left");
        let right = base.join("right");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        for (name, body) in left_files {
            std::fs::write(left.join(name), body).unwrap();
        }
        for (name, body) in right_files {
            std::fs::write(right.join(name), body).unwrap();
        }
        std::fs::write(
            data.join("state.toml"),
            format!(
                "active_tab = 0\nsplit_ratio = 0.5\n[[tabs]]\nleft = '{l}'\nright = '{r}'\nactive_right = false\n",
                l = left.display(),
                r = right.display(),
            ),
        )
        .unwrap();
        let (child, port) = spawn_and_wait(&data, true);
        Server { child, port, base }
    }

    /// HTTP リクエストを投げる。`(status, body)` を返す。
    fn req(&self, method: &str, path: &str, body: &str) -> Option<(u16, String)> {
        req(self.port, method, path, body)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // assert で panic しても、ここで子プロセスと作業dirを確実に片付ける。
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// `--debug-server=0 --headless`（必要なら `--debug-allow-write`）で起動し、`/state` が
/// 返るまで待って `(子, ポート)` を返す。
fn spawn_and_wait(data: &Path, allow_write: bool) -> (Child, u16) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rerics"));
    cmd.arg("--debug-server=0").arg("--headless");
    if allow_write {
        cmd.arg("--debug-allow-write");
    }
    let mut child = cmd
        .env("RERICS_DATA_DIR", data)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn rerics");
    let port = read_port(&mut child);
    // 起動待ち（最大 ~10 秒）。HTTP は listening 直後でも、UI スレッドとの往復が
    // 回り始めるまで /state が返らないことがあるのでポーリングする。
    let mut up = false;
    for _ in 0..50 {
        if req(port, "GET", "/state", "").is_some() {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(up, "debug server did not come up");
    (child, port)
}

/// 無圧縮(stored)・UTF-8 フラグ無しの zip を手組みする（テスト用 fixture・依存を増やさない）。
fn build_stored_zip(path: &Path, entries: &[(&str, &[u8])]) {
    fn u16le(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn u32le(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let name = name.as_bytes();
        let crc = crc32(data);
        let off = out.len() as u32;
        u32le(&mut out, 0x0403_4b50);
        u16le(&mut out, 20);
        u16le(&mut out, 0);
        u16le(&mut out, 0);
        u16le(&mut out, 0);
        u16le(&mut out, 0);
        u32le(&mut out, crc);
        u32le(&mut out, data.len() as u32);
        u32le(&mut out, data.len() as u32);
        u16le(&mut out, name.len() as u16);
        u16le(&mut out, 0);
        out.extend_from_slice(name);
        out.extend_from_slice(data);
        u32le(&mut central, 0x0201_4b50);
        u16le(&mut central, 20);
        u16le(&mut central, 20);
        u16le(&mut central, 0);
        u16le(&mut central, 0);
        u16le(&mut central, 0);
        u16le(&mut central, 0);
        u32le(&mut central, crc);
        u32le(&mut central, data.len() as u32);
        u32le(&mut central, data.len() as u32);
        u16le(&mut central, name.len() as u16);
        u16le(&mut central, 0);
        u16le(&mut central, 0);
        u16le(&mut central, 0);
        u16le(&mut central, 0);
        u32le(&mut central, 0);
        u32le(&mut central, off);
        central.extend_from_slice(name);
    }
    let cd_off = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    u32le(&mut out, 0x0605_4b50);
    u16le(&mut out, 0);
    u16le(&mut out, 0);
    u16le(&mut out, entries.len() as u16);
    u16le(&mut out, entries.len() as u16);
    u32le(&mut out, cd_size);
    u32le(&mut out, cd_off);
    u16le(&mut out, 0);
    std::fs::write(path, &out).unwrap();
}

/// `path` を GET し続け、`pred(body)` が真になるまで待つ（最大 ~15 秒）。ワーカ完了や
/// モーダル出現など非同期な状態変化を待つのに使う。最後に観測した body を返す。
/// 条件成立で即返すので緑のテストの速度には影響せず、上限は過飽和・コールドスタート時の
/// 誤タイムアウトを防ぐための余裕。
fn poll<F: Fn(&str) -> bool>(server: &Server, path: &str, pred: F) -> String {
    let mut last = String::new();
    for _ in 0..150 {
        if let Some((_, body)) = server.req("GET", path, "") {
            if pred(&body) {
                return body;
            }
            last = body;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

/// モーダルが開く（`/state/modal` が非 null になる）まで待ち、その body を返す。
fn wait_modal(server: &Server) -> String {
    poll(server, "/state/modal", |b| b.trim() != "null")
}

/// `haystack` 中の `needle` の出現回数。
fn count_substr(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// 子の stdout が報告する**実バインドポート**を読む（最大 20 秒）。別スレッドで読むので、
/// パイプ読みがブロックしてもタイムアウトで諦められる。
fn read_port(child: &mut Child) -> u16 {
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(p) = parse_listen_port(&line) {
                let _ = tx.send(p);
                break;
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(20))
        .expect("debug server did not report its port")
}

/// "[debug-server] listening on http://127.0.0.1:PORT" 行から末尾のポート番号を取る。
fn parse_listen_port(line: &str) -> Option<u16> {
    line.rsplit(':').next()?.trim().parse().ok()
}

/// debug-server へ接続する。起動直後や並列実行の高負荷で一時的に失敗し得るので数回リトライする。
fn connect_retry(port: u16) -> Option<TcpStream> {
    for attempt in 0..20 {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(c) => return Some(c),
            Err(e) => {
                if attempt == 19 {
                    eprintln!("debug-server connect failed on port {port}: {e}");
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    None
}

/// 最小 HTTP/1.0 クライアント（依存を増やさない）。`(status, body)` を返す。
fn req(port: u16, method: &str, path: &str, body: &str) -> Option<(u16, String)> {
    let mut s = connect_retry(port)?;
    // 並列実行で CPU が過飽和すると UI スレッドへ marshal する応答が遅れるため長めに待つ。
    s.set_read_timeout(Some(Duration::from_secs(20))).ok();
    let head = format!(
        "{method} {path} HTTP/1.0\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).ok()?;
    s.write_all(body.as_bytes()).ok()?;
    let mut resp = String::new();
    if let Err(e) = s.read_to_string(&mut resp) {
        eprintln!("{method} {path} response read failed: {e}");
        return None;
    }
    let status: u16 = resp.split_whitespace().nth(1)?.parse().ok()?;
    let body = resp.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("").to_string();
    Some((status, body))
}

/// 本体起動・HTTP・exec 往復・外見反映の煙テスト。`Server` が後始末を持つので、
/// 値を溜めずに直接 assert してよい（panic しても Drop が子を片付ける）。
#[test]
fn debug_server_smoke() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "[font]\nsize = 18\n");

    let (st, body) = server.req("GET", "/state", "").expect("GET /state");
    assert_eq!(st, 200, "GET /state status");
    assert!(body.contains("\"panes\""), "state json missing panes: {body}");
    assert!(body.contains("\"active_view\""), "state json missing active_view");

    let loc = server
        .req("GET", "/state/panes/left/location", "")
        .expect("GET location")
        .1;
    assert!(loc.contains("sbx"), "location should be the sandbox: {loc}");

    let c0 = server
        .req("GET", "/state/panes/left/cursor", "")
        .expect("cursor before")
        .1;
    server
        .req("POST", "/command/cursorDown", "")
        .expect("cursorDown");
    let c1 = server
        .req("GET", "/state/panes/left/cursor", "")
        .expect("cursor after")
        .1;
    assert_eq!(c0.trim(), "0", "initial cursor");
    assert_eq!(c1.trim(), "1", "cursor should move to 1 after cursorDown");

    let bst = server.req("POST", "/command/nope", "").expect("bad command").0;
    assert_eq!(bst, 400, "unknown command should be 400");

    // 書込み許可なしなのでモーダル系コマンドは 400（破壊防止のゲート）。
    let mst = server
        .req("POST", "/command/makeDirectoryDialog", "")
        .expect("modal command")
        .0;
    assert_eq!(mst, 400, "makeDirectoryDialog without --debug-allow-write should be 400");

    // 外見の設定反映：config.toml の font size=18 が解決値・ペイン保持値の双方に出る。
    let pf = server
        .req("GET", "/presentation/font/size", "")
        .expect("presentation font size")
        .1;
    assert_eq!(pf.trim(), "18", "config font size should reflect in /presentation");
    let pnf = server
        .req("GET", "/presentation/panes/left/font/size", "")
        .expect("pane font size")
        .1;
    assert_eq!(pnf.trim(), "18", "config font size should reach the pane view");
    let pc = server
        .req("GET", "/presentation/resolved_colors", "")
        .expect("resolved colors")
        .1;
    assert!(pc.contains("\"cursor\""), "resolved_colors should list palette: {pc}");
}

/// 壊れた config.toml はアプリを止めず、既定で起動した上でアラート（メッセージモーダル）と
/// ログの詳細で知らせる（実機/headless 同一挙動＝モーダルは debug-server で観測・クローズ可能）。
#[test]
fn broken_config_warns_and_starts_with_defaults() {
    // `"C:\Users"` は TOML の無効エスケープ（\U）でパース失敗する（実際に起きた失敗例）。
    let server = Server::start(&["a.txt"], "editor = \"C:\\Users\"\n");

    // 既定で起動して応答する（黙って全無視で固まらない）。
    let av = server.req("GET", "/state/active_view", "").expect("active_view").1;
    assert_eq!(av.trim(), "\"none\"", "壊れた config でも既定で起動して応答する");

    // 起動時アラート（メッセージモーダル）が出ており、観測できる。
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"message\""), "alert modal が出る: {modal}");
    assert!(modal.contains("設定の読み込み"), "alert タイトル: {modal}");

    // ログに読込失敗の旨とパースエラーの詳細が出ている。
    let log = server.req("GET", "/state/log", "").expect("log").1;
    assert!(log.contains("config.toml を読み込めませんでした"), "ログにエラー: {log}");
    assert!(log.contains("TOML parse error"), "ログに詳細: {log}");

    // モーダルは debug-server から閉じられる（実機と同じ操作で進行できる）。
    server.req("POST", "/modal/key/enter", "").expect("close alert");
    let after = poll(&server, "/state/modal", |b| b.trim() == "null");
    assert_eq!(after.trim(), "null", "Enter でアラートを閉じられる");
}

/// ログのコピー／クリア（copyLog/clearLog）が配線され、clearLog でログ行が消えることを検証する。
#[test]
fn copy_and_clear_log() {
    // 壊れた config で起動すると読込失敗の旨が必ずログに出る（クリア対象を確実に用意する）。
    let server = Server::start(&["a.txt"], "editor = \"C:\\Users\"\n");
    // 起動時アラートを閉じてから操作する。
    wait_modal(&server);
    server.req("POST", "/modal/key/enter", "").expect("close alert");
    poll(&server, "/state/modal", |b| b.trim() == "null");

    let log0 = server.req("GET", "/state/log", "").expect("log").1;
    assert!(log0.contains("config.toml"), "前提：ログに行がある: {log0}");

    // copyLog は非モーダル・非破壊で実行できる（クリップボード内容は headless では読まない）。
    let (cst, _) = server.req("POST", "/command/copyLog", "").expect("copyLog");
    assert_eq!(cst, 200, "copyLog は実行できる");

    // clearLog でログ行が空になる。
    server.req("POST", "/command/clearLog", "").expect("clearLog");
    let log1 = poll(&server, "/state/log", |b| b.contains("\"lines\":[]"));
    assert!(log1.contains("\"lines\":[]"), "clearLog でログが空になる: {log1}");
}

/// `/command` の body 引数（JSON 文字列配列）が受理され、引数を見ないコマンドでは
/// 無害に無視されること、不正な body は 400 になることを確認する（引数基盤の配線検証）。
#[test]
fn command_accepts_json_array_args() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "");

    // 引数を取らない cursorDown に引数を付けても従来どおり動く（無視される）。
    let (st, _) = server
        .req("POST", "/command/cursorDown", r#"["ignored"]"#)
        .expect("cursorDown with args");
    assert_eq!(st, 200, "command with JSON array body should be accepted");
    let c = server
        .req("GET", "/state/panes/left/cursor", "")
        .expect("cursor")
        .1;
    assert_eq!(c.trim(), "1", "cursor should still advance with args present");

    // 配列でない body は 400。
    let bad = server
        .req("POST", "/command/cursorDown", "\"notarray\"")
        .expect("bad body")
        .0;
    assert_eq!(bad, 400, "non-array JSON body should be 400");

    // 文字列でない要素を含む配列も 400。
    let bad2 = server
        .req("POST", "/command/cursorDown", "[1, 2]")
        .expect("bad elem")
        .0;
    assert_eq!(bad2, 400, "non-string args should be 400");
}

/// 書庫への追加（非衝突＝無言 append）と、同名衝突→「再構築して置換」の配線を検証する。
/// 観測はすべて `/state` 経由（右ペインは zip の中＝反映が見える）。
#[test]
fn archive_add_and_replace() {
    let server = Server::start_archive(
        &[("a.txt", b"AAA"), ("b.txt", b"BBB")],
        &[("a.txt", b"OLD-IN-ZIP"), ("existing.txt", b"keep-me")],
    );
    // 右ペインは zip の中（既存の a.txt / existing.txt が見える）。
    let r0 = server.req("GET", "/state/panes/right/items", "").unwrap().1;
    assert!(
        r0.contains("\"name\":\"a.txt\"") && r0.contains("\"name\":\"existing.txt\""),
        "right pane should be inside the zip: {r0}"
    );

    // --- 非衝突 add：b.txt（実FS）→ zip。モーダル無しの無言 append。 ---
    // 左 items は [.., a.txt, b.txt]。cursorDown×2 で b.txt。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/copy", "").unwrap();
    let m = server.req("GET", "/state/modal", "").unwrap().1;
    assert_eq!(m.trim(), "null", "non-colliding add must not prompt: {m}");
    let r1 = poll(&server, "/state/panes/right/items", |b| {
        b.contains("\"name\":\"b.txt\"")
    });
    assert!(r1.contains("\"name\":\"b.txt\""), "b.txt should be added to the archive: {r1}");

    // --- 衝突 replace：a.txt（実FS, AAA）は zip の a.txt と同名 → モーダル → 既定=置換。 ---
    // reload でカーソルは .. に戻る。cursorDown×1 で a.txt。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/copy", "").unwrap();
    let modal = wait_modal(&server);
    assert!(
        modal.contains("\"kind\":\"archive_add\""),
        "same-name collision should prompt archive_add: {modal}"
    );
    // 既定ラジオ＝「再構築して置換」。OK で置換。
    server.req("POST", "/modal/command/ok", "").unwrap();
    // 置換は rebuild 経路を通る。
    let lg = poll(&server, "/state/log", |b| b.contains("Rebuild"));
    assert!(lg.contains("Rebuild"), "replace should run the rebuild path: {lg}");
    assert!(
        !lg.contains("失敗しました"),
        "replace must not fail (the old append-on-collision bug): {lg}"
    );
    // a.txt は重複しない（壊れた append なら Duplicate filename で失敗していた）。
    let r2 = poll(&server, "/state/panes/right/items", |b| {
        count_substr(b, "\"name\":\"a.txt\"") == 1 && b.contains("\"name\":\"b.txt\"")
    });
    assert_eq!(
        count_substr(&r2, "\"name\":\"a.txt\""),
        1,
        "a.txt must not be duplicated after replace: {r2}"
    );
    assert!(r2.contains("\"name\":\"existing.txt\""), "existing entry must be preserved: {r2}");
}

/// 書庫内 mkdir（＋同名はエラー）と、実FS→書庫への move（元削除）の配線を検証する。
#[test]
fn archive_mkdir_and_move() {
    let server = Server::start_archive(&[("m.txt", b"MMM")], &[("existing.txt", b"keep")]);

    // 右ペイン（書庫）をアクティブにして mkdir。
    server.req("POST", "/command/focusRight", "").unwrap();
    server.req("POST", "/command/makeDirectoryDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "newdir").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    let r = poll(&server, "/state/panes/right/items", |b| {
        b.contains("\"name\":\"newdir\"")
    });
    assert!(r.contains("\"name\":\"newdir\""), "newdir should be created in the archive: {r}");

    // 同名 mkdir はエラー（実FS のディレクトリ作成と同じ挙動）。
    server.req("POST", "/command/makeDirectoryDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "newdir").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    let lg = poll(&server, "/state/log", |b| b.contains("すでに存在します"));
    assert!(lg.contains("すでに存在します"), "duplicate mkdir should error: {lg}");
    // エラーの message box を閉じ、モーダルが消えるまで待つ（残っていると後続コマンドを
    // 横取りして active ペインが切り替わらず、move が誤判定される）。
    wait_modal(&server);
    server.req("POST", "/modal/key/enter", "");
    poll(&server, "/state/modal", |b| b.trim() == "null");
    // 重複ディレクトリは増えない。
    let r2 = server.req("GET", "/state/panes/right/items", "").unwrap().1;
    assert_eq!(
        count_substr(&r2, "\"name\":\"newdir\""),
        1,
        "no duplicate directory entry: {r2}"
    );

    // --- move：m.txt（実FS, 非衝突）→ 書庫。追加成功で元を削除する。 ---
    server.req("POST", "/command/focusLeft", "").unwrap();
    // 左 items は [.., m.txt]。cursorDown×1 で m.txt。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/move", "").unwrap();
    let r3 = poll(&server, "/state/panes/right/items", |b| {
        b.contains("\"name\":\"m.txt\"")
    });
    assert!(r3.contains("\"name\":\"m.txt\""), "moved file should appear in the archive: {r3}");
    let l = poll(&server, "/state/panes/left/items", |b| {
        !b.contains("\"name\":\"m.txt\"")
    });
    assert!(!l.contains("\"name\":\"m.txt\""), "source must be deleted after move: {l}");
}

/// 書庫内エントリの削除（確認ダイアログ→リビルド）を検証する。
#[test]
fn archive_delete() {
    let server = Server::start_archive(
        &[("dummy.txt", b"x")],
        &[("a.txt", b"AAA"), ("b.txt", b"BBB"), ("keep.txt", b"K")],
    );
    server.req("POST", "/command/focusRight", "").unwrap();
    // 右 items は [.., a.txt, b.txt, keep.txt]。cursorDown×1 で a.txt。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/delete", "").unwrap();
    // YesNo 確認モーダル → はい（既定ボタン＝enter）。
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"message\""), "delete should confirm first: {modal}");
    server.req("POST", "/modal/key/enter", "").unwrap();
    // a.txt が消え、他は残る。
    let r = poll(&server, "/state/panes/right/items", |b| {
        !b.contains("\"name\":\"a.txt\"")
    });
    assert!(!r.contains("\"name\":\"a.txt\""), "a.txt should be deleted: {r}");
    assert!(
        r.contains("\"name\":\"b.txt\"") && r.contains("\"name\":\"keep.txt\""),
        "other entries must remain: {r}"
    );
}

/// ask_before_copy=true のとき、copy の前に確認モーダルが出てキャンセルで中止できる。
#[test]
fn ask_before_copy_confirms() {
    let server = Server::start_writable_cfg(&["a.txt"], "[file_ops]\nask_before_copy = true\n");
    // 左 items は [.., a.txt]。cursorDown×1 で a.txt にカーソル。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/copy", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"message\""), "copy should confirm first: {modal}");
    assert!(modal.contains("コピー"), "confirm dialog titled コピー: {modal}");
    // キャンセルで中止＝ファイルはそのまま残る。
    server.req("POST", "/modal/command/cancel", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let items = server.req("GET", "/state/panes/left/items", "").expect("items").1;
    assert!(items.contains("\"name\":\"a.txt\""), "cancel leaves the file: {items}");
}

/// カーソル直下ファイル自体を移動したときは、カーソルは元の行位置（index）を保つ
/// （先頭へリセットしない）。shellMove と同じ `refresh_side` 経路を通る内蔵 move で
/// 検証する（シェル操作自体はモーダルがシェル所有で観測不可）。
#[test]
fn move_keeps_cursor_position() {
    let server = Server::start_dirs_writable(
        &[("a.txt", b"a"), ("b.txt", b"b"), ("c.txt", b"c")],
        &[],
    );
    // 左 items = [.., a.txt, b.txt, c.txt]。cursorDown×2 で b.txt（index 2）へ。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    let before = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(before.trim(), "2", "移動前カーソルは b.txt（index 2）");
    // b.txt を右へ移動。完了で左から b.txt が消える。
    server.req("POST", "/command/move", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| !b.contains("\"name\":\"b.txt\""));
    // 左 items = [.., a.txt, c.txt]。カーソルは元の index 2 付近＝c.txt を保つ（0 に戻らない）。
    let after = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(after.trim(), "2", "移動後もカーソル位置を保つ（先頭リセットしない）: {after}");
}

/// マークしたファイルを移動した後、カーソル直下だったファイルが残っていれば名前で追従する
/// （マーク数ぶん行位置がズレない）。
#[test]
fn move_marked_follows_cursor_file_by_name() {
    let server = Server::start_dirs_writable(
        &[("a.txt", b"a"), ("b.txt", b"b"), ("c.txt", b"c"), ("d.txt", b"d"), ("e.txt", b"e")],
        &[],
    );
    // 左 items = [.., a.txt, b.txt, c.txt, d.txt, e.txt]。a.txt へ降りて a.txt・b.txt をマーク
    // （markToggle は既定でカーソルを1つ進める）。マーク後カーソルは c.txt（index 3）。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();
    let before = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(before.trim(), "3", "マーク後カーソルは c.txt（index 3）");
    // マークした a.txt・b.txt を右へ移動。完了で左から2つ消える。
    server.req("POST", "/command/move", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| !b.contains("\"name\":\"a.txt\""));
    // 左 items = [.., c.txt, d.txt, e.txt]。カーソルは c.txt を名前で追従（index 1）＝
    // 元の行位置 3（e.txt）へズレない。
    let after = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(after.trim(), "1", "カーソルは c.txt を追従する（位置ズレしない）: {after}");
}

/// 単独ファイルの移動では、移動先ペインのカーソルが届いたそのファイルへ寄る
/// （リネームが新名へ寄るのと同じ focus 動作）。
#[test]
fn move_single_focuses_arrived_file_on_dest() {
    let server = Server::start_dirs_writable(
        &[("a.txt", b"a"), ("b.txt", b"b"), ("c.txt", b"c")],
        &[],
    );
    // 左 items = [.., a.txt, b.txt, c.txt]。cursorDown×2 で b.txt（index 2）。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    // b.txt を右へ移動。左は [.., a.txt, c.txt]、カーソルは index 2（c.txt）。
    server.req("POST", "/command/move", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| !b.contains("\"name\":\"b.txt\""));
    // 右へ移り、移ってきた b.txt にカーソルを合わせて左へ戻す。
    server.req("POST", "/command/focusRight", "").unwrap();
    poll(&server, "/state/panes/right/items", |b| b.contains("\"name\":\"b.txt\""));
    server.req("POST", "/command/cursorDown", "").unwrap(); // 右 [.., b.txt] の b.txt へ
    server.req("POST", "/command/move", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"b.txt\""));
    // 左 items = [.., a.txt, b.txt, c.txt]。カーソルは戻ってきた b.txt（index 2）へ寄る
    // （カーソル下だった c.txt の名前追従より、単独対象への focus が優先される）。
    let cur = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(cur.trim(), "2", "移動先は届いた単独ファイルへカーソルを寄せる: {cur}");
}

/// 複数ファイルの移動では、移動先ペインのカーソルはカーソル下のファイルを名前で追従する。
/// 手前へ項目が挿入されると行位置は動くが、同じファイルの上に乗り続ける。
#[test]
fn move_multiple_follows_cursor_file_on_dest() {
    let server = Server::start_dirs_writable(
        &[("a.txt", b"a"), ("b.txt", b"b"), ("c.txt", b"c")],
        &[("x.txt", b"x"), ("y.txt", b"y")],
    );
    // 右 items = [.., x.txt, y.txt]。y.txt（index 2）にカーソルを置いて左へ戻る。
    server.req("POST", "/command/focusRight", "").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/focusLeft", "").unwrap();
    // 左で a.txt・b.txt をマークして右へ移動。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();
    server.req("POST", "/command/move", "").unwrap();
    poll(&server, "/state/panes/right/items", |b| b.contains("\"name\":\"b.txt\""));
    // 右 items = [.., a.txt, b.txt, x.txt, y.txt]。カーソルは y.txt を名前で追従して
    // index 4 へ（2件の挿入で行位置は動くが、同じファイルの上に乗り続ける）。
    let cur = server.req("GET", "/state/panes/right/cursor", "").unwrap().1;
    assert_eq!(cur.trim(), "4", "移動先はカーソル下ファイルを名前で追従する: {cur}");
}

/// ask_before_delete=false のとき、delete は確認モーダルを出さず即削除する。
#[test]
fn ask_before_delete_off_skips_confirm() {
    let server = Server::start_writable_cfg(&["a.txt", "b.txt"], "[file_ops]\nask_before_delete = false\n");
    // 左 items は [.., a.txt, b.txt]。cursorDown×1 で a.txt にカーソル。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/delete", "").unwrap();
    // 確認なしで a.txt が消える。
    let items = poll(&server, "/state/panes/left/items", |b| !b.contains("\"name\":\"a.txt\""));
    assert!(!items.contains("\"name\":\"a.txt\""), "a.txt should be deleted directly: {items}");
    assert!(items.contains("\"name\":\"b.txt\""), "b.txt must remain: {items}");
    // モーダルは出ていない。
    let modal = server.req("GET", "/state/modal", "").expect("modal").1;
    assert!(modal.trim() == "null", "no confirm modal when ask_before_delete is off: {modal}");
    // 開始/終了の枠ログが実ログに出る。
    let log = poll(&server, "/state/log", |b| b.contains("削除終了"));
    assert!(log.contains("削除開始"), "start frame should be logged: {log}");
    assert!(log.contains("削除終了"), "end frame should be logged: {log}");
}

/// 圧縮ダイアログで名前を入れて OK すると、その名前の zip が作られる（まとめて1つ）。
#[test]
fn compress_creates_named_zip() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    server.req("POST", "/command/cursorDown", "").unwrap(); // .. -> a.txt
    server.req("POST", "/command/compressDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"compress\""), "compress dialog should open: {modal}");
    server.req("POST", "/modal/text", "out.zip").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"out.zip\""));
    assert!(items.contains("\"name\":\"out.zip\""), "out.zip should be created: {items}");
}

/// 個別圧縮（OneByOne）にチェックすると、マークした各項目が `<名前>.zip` になる。
#[test]
fn compress_one_by_one_makes_per_item_zips() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    // a.txt と b.txt を両方マーク（Space=markToggle はマーク後カーソルを下へ）。
    server.req("POST", "/command/cursorDown", "").unwrap(); // .. -> a.txt
    server.req("POST", "/command/markToggle", "").unwrap(); // mark a.txt -> b.txt
    server.req("POST", "/command/markToggle", "").unwrap(); // mark b.txt
    server.req("POST", "/command/compressDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"compress\""), "compress dialog should open: {modal}");
    // 個別圧縮にチェックして OK。
    server.req("POST", "/modal/check", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| {
        b.contains("\"name\":\"a.txt.zip\"") && b.contains("\"name\":\"b.txt.zip\"")
    });
    assert!(
        items.contains("\"name\":\"a.txt.zip\"") && items.contains("\"name\":\"b.txt.zip\""),
        "each item should become its own zip: {items}"
    );
}

/// 名前に .7z を入れて OK すると 7z が作られる（形式は名前の拡張子で決まる）。
#[test]
fn compress_creates_named_7z() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    server.req("POST", "/command/cursorDown", "").unwrap(); // .. -> a.txt
    server.req("POST", "/command/compressDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"compress\""), "compress dialog should open: {modal}");
    server.req("POST", "/modal/text", "out.7z").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"out.7z\""));
    assert!(items.contains("\"name\":\"out.7z\""), "out.7z should be created: {items}");
}

/// 複数対象で .tar.xz を入れて OK すると tar.xz が作られる。
#[test]
fn compress_creates_tar_xz() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    server.req("POST", "/command/cursorDown", "").unwrap(); // .. -> a.txt
    server.req("POST", "/command/markToggle", "").unwrap(); // mark a.txt -> b.txt
    server.req("POST", "/command/markToggle", "").unwrap(); // mark b.txt
    server.req("POST", "/command/compressDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "out.tar.xz").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"out.tar.xz\""));
    assert!(items.contains("\"name\":\"out.tar.xz\""), "out.tar.xz should be created: {items}");
}

/// 単一ファイルに .xz を入れて OK すると tar なしの単体 xz が作られる。
#[test]
fn compress_single_file_xz() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    server.req("POST", "/command/cursorDown", "").unwrap(); // .. -> a.txt
    server.req("POST", "/command/compressDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "a.txt.xz").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"a.txt.xz\""));
    assert!(items.contains("\"name\":\"a.txt.xz\""), "a.txt.xz should be created: {items}");
}

/// 形式ラジオで 7z を選び個別圧縮にチェックすると、各項目が `<名前>.7z` になる。
#[test]
fn compress_radio_7z_one_by_one() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    server.req("POST", "/command/cursorDown", "").unwrap(); // .. -> a.txt
    server.req("POST", "/command/markToggle", "").unwrap(); // mark a.txt -> b.txt
    server.req("POST", "/command/markToggle", "").unwrap(); // mark b.txt
    server.req("POST", "/command/compressDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/radio/1", "").unwrap(); // 0=zip 1=7z 2=xz
    server.req("POST", "/modal/check", "").unwrap(); // 個別圧縮
    server.req("POST", "/modal/command/ok", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| {
        b.contains("\"name\":\"a.txt.7z\"") && b.contains("\"name\":\"b.txt.7z\"")
    });
    assert!(
        items.contains("\"name\":\"a.txt.7z\"") && items.contains("\"name\":\"b.txt.7z\""),
        "each item should become its own 7z: {items}"
    );
}

/// 圧縮ファイルは反対ペイン（右）に作られる（原作準拠）。アクティブ（左）には残らない。
#[test]
fn compress_creates_in_opposite_pane() {
    let server = Server::start_writable_split(&["a.txt"]); // 左に a.txt・右は空・左アクティブ。
    server.req("POST", "/command/cursorDown", "").unwrap(); // .. -> a.txt
    server.req("POST", "/command/compressDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "out.zip").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    // 右（反対）ペインに現れる。
    let right = poll(&server, "/state/panes/right/items", |b| b.contains("\"name\":\"out.zip\""));
    assert!(right.contains("\"name\":\"out.zip\""), "archive should appear in the opposite (right) pane: {right}");
    // 左（アクティブ）ペインには作られない。
    let left = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert!(!left.contains("\"name\":\"out.zip\""), "archive must not be in the active (left) pane: {left}");
}

/// extract_create_directory=true のとき、書庫の展開先に書庫名のフォルダ（arc）が作られる。
#[test]
fn extract_create_directory_wraps_in_archive_named_dir() {
    let server = Server::start_archive_cfg(
        &[],
        &[("a.txt", b"AAA"), ("b.txt", b"BBB")],
        "[file_ops]\nextract_create_directory = true\n",
    );
    server.req("POST", "/command/focusRight", "").unwrap(); // 書庫ペインをアクティブに
    // 右 items は [.., a.txt, b.txt]。両方マークして展開。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();
    server.req("POST", "/command/extract", "").unwrap();
    // 左（実）ペインに書庫名の arc フォルダができ、その中へ取り出される。
    let left = poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"arc\""));
    assert!(left.contains("\"name\":\"arc\""), "extract should create an 'arc' directory: {left}");
    assert!(
        !left.contains("\"name\":\"a.txt\""),
        "entries go inside arc/, not the top level: {left}"
    );
}

/// 書庫内エントリの改名（リビルド）と、衝突時のエラーを検証する。
#[test]
fn archive_rename() {
    let server = Server::start_archive(
        &[("dummy.txt", b"x")],
        &[("a.txt", b"AAA"), ("b.txt", b"BBB")],
    );
    server.req("POST", "/command/focusRight", "").unwrap();
    // 右 items は [.., a.txt, b.txt]。cursorDown×1 で a.txt。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/renameDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "z.txt").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    let r = poll(&server, "/state/panes/right/items", |b| {
        b.contains("\"name\":\"z.txt\"")
    });
    assert!(
        r.contains("\"name\":\"z.txt\"") && !r.contains("\"name\":\"a.txt\""),
        "a.txt should be renamed to z.txt: {r}"
    );

    // 衝突：b.txt -> z.txt（z.txt は既存）はエラー。reload でカーソルは .. に戻る。
    // items は [.., b.txt, z.txt]。cursorDown×1 で b.txt。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/renameDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "z.txt").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    let lg = poll(&server, "/state/log", |b| b.contains("同名が存在します"));
    assert!(lg.contains("同名が存在します"), "rename collision should error: {lg}");
    // エラーの message box を閉じる。
    wait_modal(&server);
    server.req("POST", "/modal/key/enter", "");
    poll(&server, "/state/modal", |b| b.trim() == "null");
    // z.txt は1つのまま（衝突は置換せずエラー）。
    let r2 = server.req("GET", "/state/panes/right/items", "").unwrap().1;
    assert_eq!(count_substr(&r2, "\"name\":\"z.txt\""), 1, "no duplicate z.txt: {r2}");
    assert!(r2.contains("\"name\":\"b.txt\""), "b.txt must remain after failed rename: {r2}");
}

/// 非書庫の rename は名前/属性/更新日時の専用モーダルを開く。debug-server からは
/// チェック値を操作できないので、開いて OK で閉じても対象が壊れない（デッドロックしない）
/// ことだけを担保する。属性/日時の適用ロジック自体は core 側でテスト済み。
#[test]
fn rename_meta_dialog_opens_and_closes() {
    let server = Server::start_writable(&["a.txt"]);
    // items は [.., a.txt]。cursorDown×1 で a.txt。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/renameDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"rename\""), "should open rename meta modal: {modal}");
    // 既定値のまま OK（名前据え置き＝改名なし）。
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let items = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert!(items.contains("\"name\":\"a.txt\""), "a.txt should still exist: {items}");
}

/// createFileDialog＝入力したファイル名で空ファイルを作成する。
#[test]
fn create_file_makes_empty_file() {
    let server = Server::start_writable(&["a.txt"]);

    server.req("POST", "/command/createFileDialog", "").unwrap();
    // ファイル名入力ダイアログが開く。
    let modal = wait_modal(&server);
    assert!(modal.contains("\"has_input\":true"), "should ask for a name: {modal}");
    server.req("POST", "/modal/text", "made.txt").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();

    let items = poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"made.txt\""));
    assert!(items.contains("\"name\":\"made.txt\""), "new file should appear: {items}");
    // 空ファイルが作られている。
    let body = std::fs::read(server.base.join("sbx").join("made.txt")).unwrap();
    assert!(body.is_empty(), "new file should be empty: {body:?}");
}

/// toRoot＝カレントのドライブルートへ移動する。
#[test]
fn nav_to_root() {
    let server = Server::start(&["a.txt"], "");
    // 開始は sbx。location は JSON 文字列 "X:\\...\\sbx"（バックスラッシュはエスケープ）。
    let before = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert!(before.contains("sbx"), "should start in the sandbox: {before}");
    let drive = before.chars().nth(1).expect("drive letter"); // 先頭の引用符の次＝ドライブ文字
    // ルートは JSON では "X:\\"（X, :, \\）。
    let expected = format!("\"{drive}:\\\\\"");

    server.req("POST", "/command/toRoot", "").unwrap();
    let after = poll(&server, "/state/panes/left/location", |b| b.trim() == expected);
    assert_eq!(after.trim(), expected, "toRoot should jump to the drive root");
}

/// historyBack/historyForward＝パス移動履歴を前後する。
#[test]
fn nav_history_back_forward() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    let sbx = sbx.trim().to_string();

    // 親へ移動（sbx → その親）。
    server.req("POST", "/command/toParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx);
    let parent = parent.trim().to_string();
    assert_ne!(parent, sbx, "toParent should leave the sandbox");

    // 戻る＝sbx へ。
    server.req("POST", "/command/historyBack", "").unwrap();
    let back = poll(&server, "/state/panes/left/location", |b| b.trim() == sbx);
    assert_eq!(back.trim(), sbx, "historyBack should return to the sandbox");

    // 進む＝親へ。
    server.req("POST", "/command/historyForward", "").unwrap();
    let fwd = poll(&server, "/state/panes/left/location", |b| b.trim() == parent);
    assert_eq!(fwd.trim(), parent, "historyForward should go back to the parent");
}

/// #67: cursor.history=false でも、戻る/進むはカーソルを元の項目へ復元する（原作準拠＝常時復元）。
#[test]
fn history_back_restores_cursor_even_with_history_off() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "[cursor]\nhistory = false\n");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();

    // items: [.., a.txt, b.txt, c.txt]。cursorDown×3 で c.txt（index 3）へ。
    for _ in 0..3 {
        server.req("POST", "/command/cursorDown", "").unwrap();
    }
    let pre = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(pre.trim(), "3", "precondition: cursor should be on c.txt (index 3): {pre}");

    // 親へ移動 → 戻る。
    server.req("POST", "/command/toParent", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() != sbx);
    server.req("POST", "/command/historyBack", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() == sbx);

    // cursor.history=false でもカーソルは c.txt（index 3）へ復元される。
    let restored = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "3");
    assert_eq!(restored.trim(), "3", "historyBack should restore the cursor even with cursor.history off: {restored}");
}

/// pathHistoryDialog＝訪問ログ（list_box モーダル・新しい順）から選んでジャンプする。
#[test]
fn nav_path_history_dialog() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    let sbx_raw = sbx.trim_matches('"').replace("\\\\", "\\");

    // 親へ移動（履歴に親）→ パス入力で sbx へ戻る（履歴に sbx）。訪問ログは [親, sbx]。
    server.req("POST", "/command/toParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx).trim().to_string();
    assert_ne!(parent, sbx, "toParent should leave the sandbox");
    server.req("POST", "/command/changeDirectoryDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", &sbx_raw).unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() == sbx);

    // 履歴ダイアログを開く（訪問ログは新しい順＝[sbx, 親]だが、先頭の sbx は現在地なので
    // 表示からは除かれ、親だけが一覧に出る）。
    server.req("POST", "/command/pathHistoryDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"list\""), "should open a list modal: {modal}");
    assert!(!modal.contains("sbx"), "current location should be hidden from the list: {modal}");

    // 親（index 0）を選んで OK＝そこへジャンプ。
    server.req("POST", "/modal/select/0", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let now = poll(&server, "/state/panes/left/location", |b| b.trim() == parent);
    assert_eq!(now.trim(), parent, "selecting the parent entry should navigate there");
}

/// selectMaskDialog＝入力モーダルで取ったマスクを no-UI 版 `r.selectMask` へ委譲する。委譲版は
/// Like パターン＋既存選択をクリアして選び直すので、事前に選んだ非一致項目（b.dat）は外れ、
/// `*.txt` の 2 件だけが marked になる（＝マッチとクリアの両方を確認）。
#[test]
fn select_mask_dialog_delegates_with_clear() {
    let server = Server::start_with_scripts(&["a.txt", "b.dat", "c.txt"], &[]);

    // 事前にマスク非一致の b.dat を選んでおく（クリアされることの確認用）。
    server
        .req(
            "POST",
            "/script/eval",
            r#"rerics.activePane().items.find((it) => it.name === "b.dat").selected = true;"#,
        )
        .expect("preselect eval");
    poll(&server, "/state/panes/left/items", |b| {
        count_substr(b, "\"marked\":true") == 1
    });

    // selectMaskDialog で "*.txt" を入力＝委譲先が a.txt/c.txt を選び直し、b.dat はクリアされる。
    server.req("POST", "/command/selectMaskDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"has_input\":true"), "should open a text-input modal: {modal}");
    server.req("POST", "/modal/text", "*.txt").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();

    let items = poll(&server, "/state/panes/left/items", |b| {
        count_substr(b, "\"marked\":true") == 2
    });
    assert_eq!(
        count_substr(&items, "\"marked\":true"),
        2,
        "only the two .txt files should remain marked (b.dat cleared): {items}"
    );

    // マスク選択でもステータスバー左に選択件数が反映される（選択を変える経路は
    // refresh を通じて件数表示を更新する）。
    let status = poll(&server, "/state/panes/left/status_bar/left", |b| b.contains("選択 2"));
    assert!(
        status.contains("選択 2"),
        "status bar should show the selected count after mask select: {status}"
    );
}

/// 組込の全選択コマンドでも、ステータスバー左に選択件数が反映される（選択変更は refresh を
/// choke point に件数表示へ伝わる）。全選択→全解除で表示が出て消えることまで確認する。
#[test]
fn select_all_updates_status_bar_count() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "");

    server.req("POST", "/command/selectAll", "").unwrap();
    let status = poll(&server, "/state/panes/left/status_bar/left", |b| b.contains("選択 3"));
    assert!(
        status.contains("選択 3"),
        "status bar should show 3 selected after selectAll: {status}"
    );

    server.req("POST", "/command/clearAll", "").unwrap();
    let cleared = poll(&server, "/state/panes/left/status_bar/left", |b| !b.contains("選択"));
    assert!(
        !cleared.contains("選択"),
        "status bar should be cleared after clearAll: {cleared}"
    );
}

/// pathMaskDialog＝入力モーダルで取ったマスクを no-UI 版 `r.pathMask` へ委譲する。`*.txt` で
/// 表示が絞られ（b.dat が消える）、`*` で解除されて戻る（＝委譲と「空/`*` は解除」の正規化を確認）。
#[test]
fn path_mask_dialog_delegates_filter_and_clear() {
    let server = Server::start(&["a.txt", "b.dat"], "");
    let items0 = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert!(items0.contains("\"name\":\"b.dat\""), "b.dat should be visible initially: {items0}");

    // "*.txt" で絞る＝b.dat が消える。
    server.req("POST", "/command/pathMaskDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"has_input\":true"), "should open a text-input modal: {modal}");
    server.req("POST", "/modal/text", "*.txt").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    let filtered = poll(&server, "/state/panes/left/items", |b| !b.contains("\"name\":\"b.dat\""));
    assert!(filtered.contains("\"name\":\"a.txt\""), "a.txt should remain under *.txt: {filtered}");

    // "*" で解除＝b.dat が戻る。
    server.req("POST", "/command/pathMaskDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "*").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"b.dat\""));
}

/// パスマスクの絞り込み→解除ではファイルセットが変わらないので、カーソルは位置でなく
/// ファイル名で保つ＝解除後も同じファイルに残る（先頭へリセットしない）。
#[test]
fn path_mask_keeps_cursor_on_file() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt", "d.txt"], "");
    // items = [.., a.txt, b.txt, c.txt, d.txt]。cursorDown×3 で c.txt（index 3）。
    for _ in 0..3 {
        server.req("POST", "/command/cursorDown", "").unwrap();
    }
    assert_eq!(
        server.req("GET", "/state/panes/left/cursor", "").unwrap().1.trim(),
        "3",
        "マスク前カーソルは c.txt（index 3）"
    );
    // "c*" で c.txt だけに絞ってから解除する。
    server.req("POST", "/script/eval", r#"rerics.pathMask("c*")"#).unwrap();
    poll(&server, "/state/panes/left/items", |b| !b.contains("\"name\":\"b.txt\""));
    server.req("POST", "/script/eval", r#"rerics.pathMask("*")"#).unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"b.txt\""));
    // 解除後、カーソルは同じ c.txt（index 3）に残る（0 へ戻らない・名前で保つ）。
    let cur = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(cur.trim(), "3", "マスク解除でカーソルは同じ c.txt に残る: {cur}");
}

/// compareDialog＝比較方法の list_box モーダルで選んだ条件を no-UI 版 `r.compare` へ委譲して
/// 同名ファイルを選択する。左右とも同じディレクトリで開くので、`名前一致のみ` は各ファイルが
/// 反対側の同名（自分自身）と一致＝全ファイルが marked になる。
#[test]
fn compare_dialog_marks_same_name_files() {
    let server = Server::start(&["a.txt", "b.txt"], "");
    let items0 = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert_eq!(count_substr(&items0, "\"marked\":true"), 0, "no selection yet: {items0}");

    server.req("POST", "/command/compareDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"list\""), "should open a list modal: {modal}");
    assert!(modal.contains("名前一致のみ"), "should list compare options: {modal}");

    // 先頭（名前一致のみ）を選んで OK＝両ペイン同一 dir なので全ファイルがマークされる。
    server.req("POST", "/modal/select/0", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let items1 = poll(&server, "/state/panes/left/items", |b| {
        count_substr(b, "\"marked\":true") == 2
    });
    assert_eq!(
        count_substr(&items1, "\"marked\":true"),
        2,
        "compareDialog(name) should mark both same-name files: {items1}"
    );
}

/// changeDirectoryDialog＝パスを入力してそこへ移動する（input_box モーダル）。
#[test]
fn nav_change_directory_dialog() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server
        .req("GET", "/state/panes/left/location", "")
        .unwrap()
        .1
        .trim()
        .to_string();
    // JSON 文字列（"...\\sbx"）を実パス（...\sbx）へ戻して入力に使う。
    let sbx_raw = sbx_json.trim_matches('"').replace("\\\\", "\\");

    // いったん親へ移動してから、ダイアログで sbx を打って戻る。
    server.req("POST", "/command/toParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx_json);
    assert_ne!(parent.trim(), sbx_json, "toParent should leave the sandbox");

    server.req("POST", "/command/changeDirectoryDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"has_input\":true"), "CD should open a text-input modal: {modal}");

    server.req("POST", "/modal/text", &sbx_raw).unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    let back = poll(&server, "/state/panes/left/location", |b| b.trim() == sbx_json);
    assert_eq!(back.trim(), sbx_json, "typing a path should navigate there");
}

/// #70: 存在しないパスを入力すると、ログだけでなくエラーダイアログ（kind=message）が出る。
#[test]
fn nav_change_directory_missing_path_shows_error_dialog() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    let sbx_raw = sbx_json.trim_matches('"').replace("\\\\", "\\");
    let missing = format!("{sbx_raw}\\__no_such_dir__");

    server.req("POST", "/command/changeDirectoryDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", &missing).unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();

    // 入力モーダルが閉じた後、存在しないパスのエラーダイアログが開く。
    let err = poll(&server, "/state/modal", |b| b.contains("\"kind\":\"message\""));
    assert!(err.contains("ディレクトリが存在しません"), "missing path should raise NotExists dialog: {err}");

    // ダイアログを閉じても現在地は動かない（移動は失敗のまま）。
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let loc = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert_eq!(loc.trim(), sbx_json, "failed navigation should keep the original location");
}

/// #66: ユーザが行き先を指定した移動（親移動・パス入力）が訪問ログに記録され、
/// pathHistoryDialog に新しい順で出る。さらに history.toml の pathhistory バケツへ永続する。
#[test]
fn path_history_records_and_persists() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    let sbx_raw = sbx_json.trim_matches('"').replace("\\\\", "\\");

    // 親へ移動（履歴に親が入る）→ パス入力で sbx へ戻る（履歴に sbx が入る）。
    server.req("POST", "/command/toParent", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() != sbx_json);
    server.req("POST", "/command/changeDirectoryDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", &sbx_raw).unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() == sbx_json);

    // pathHistoryDialog：訪問ログの先頭は sbx（現在地）だが表示からは除かれ、親だけが出る。
    server.req("POST", "/command/pathHistoryDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"list\""), "path history should open a list modal: {modal}");
    assert!(!modal.contains("sbx"), "current location should be hidden from the list: {modal}");
    server.req("POST", "/modal/command/cancel", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");

    // history.toml に pathhistory バケツが永続する（再起動間も残る土台）。
    let hist_path = server.base.join("data").join("history.toml");
    let mut hist = String::new();
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&hist_path)
            && s.contains("pathhistory") {
                hist = s;
                break;
            }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    assert!(hist.contains("pathhistory"), "history.toml should persist the pathhistory bucket: {hist}");
    assert!(hist.contains("sbx"), "the visited path should be saved: {hist}");
}

/// #66 回帰: `view()`（Enter に割り当てられることが多い）でディレクトリへ侵入したときも
/// 訪問ログへ記録される。かつて `reload_side`（記録なし）を使っていて、`enterDir` 経由の
/// 侵入は記録されるのに `view()` 経由だけ記録が漏れる不整合があった。
#[test]
fn view_command_entering_directory_records_path_history() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.base.join("sbx");
    std::fs::create_dir_all(sbx.join("sub")).unwrap();
    server.req("POST", "/command/reload", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"sub\""));

    server.req("POST", "/command/setCursorPosition", r#"["sub"]"#).unwrap();
    server.req("POST", "/command/view", "").unwrap();
    let loc = poll(&server, "/state/panes/left/location", |b| b.contains("sub"));
    assert!(loc.contains("sub"), "view() should enter the directory: {loc}");

    let hist_path = server.base.join("data").join("history.toml");
    let mut hist = String::new();
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&hist_path)
            && s.contains("pathhistory") {
                hist = s;
                break;
            }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    assert!(hist.contains("sub"), "entering a directory via view() should record to path history: {hist}");
}

/// 回帰: `view()` でディレクトリへ侵入するときも直前のカーソル位置が記憶され、
/// 戻る（historyBack）で復元される。かつて `view()` 経由の侵入だけカーソル記憶が漏れ、
/// 戻ったときカーソルが先頭へ戻る不整合があった（`enterDir` 経由は復元される）。
#[test]
fn view_command_entering_directory_remembers_cursor() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.base.join("sbx");
    std::fs::create_dir_all(sbx.join("sub1")).unwrap();
    std::fs::create_dir_all(sbx.join("sub2")).unwrap();
    server.req("POST", "/command/reload", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"sub2\""));
    let sbx_loc = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();

    // items: [.., sub1, sub2, a.txt]。sub2（index 2）にカーソルを置いて view() で侵入。
    server.req("POST", "/command/setCursorPosition", r#"["sub2"]"#).unwrap();
    server.req("POST", "/command/view", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.contains("sub2"));

    // 戻ると sbx に復帰し、カーソルは侵入元の sub2（index 2）へ復元される。
    server.req("POST", "/command/historyBack", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() == sbx_loc);
    let restored = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    assert_eq!(restored.trim(), "2", "returning after view() entry should restore the cursor: {restored}");
}

/// #66: 戻る/進む（履歴の再生）は訪問ログに新たな記録を増やさない（往復で増殖しない）。
#[test]
fn path_history_back_forward_does_not_grow_log() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();

    // 親へ一度移動して back/forward の素地を作る（ここで親と…は記録される）。
    server.req("POST", "/command/toParent", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() != sbx_json);

    let hist_path = server.base.join("data").join("history.toml");
    let read_hist = || std::fs::read_to_string(&hist_path).unwrap_or_default();
    // pathhistory が書かれるまで待ち、その時点の pathhistory 行数を数える。
    for _ in 0..50 {
        if read_hist().contains("pathhistory") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    // 各訪問パスにユニークな作業dir名 "rerics_it" がちょうど1回出る＝記録件数の代理。
    let count_paths = |s: &str| s.matches("rerics_it").count();
    let before = count_paths(&read_hist());

    // 戻る→進む を数回。履歴の再生なので pathhistory は増えないはず。
    for _ in 0..3 {
        server.req("POST", "/command/historyBack", "").unwrap();
        server.req("POST", "/command/historyForward", "").unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    let after = count_paths(&read_hist());
    assert_eq!(after, before, "back/forward should not add path-history entries: before={before} after={after}");
}

/// 入力履歴（D2-1）：changeDirectory で打った値が history.toml の "changedir" バケツに永続する。
/// 入力欄が履歴コンボへ変わっても `/modal/text`（コンボ内 Edit）で打てることも兼ねて確認する。
#[test]
fn input_history_changedir_persists() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    let sbx_raw = sbx_json.trim_matches('"').replace("\\\\", "\\");

    server.req("POST", "/command/changeDirectoryDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", &sbx_raw).unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");

    // OK（Enter）後、履歴ファイルに "changedir" バケツと入力値が書かれる（保存はモーダル閉鎖直後）。
    let hist_path = server.base.join("data").join("history.toml");
    let mut hist = String::new();
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&hist_path)
            && s.contains("changedir") {
                hist = s;
                break;
            }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    assert!(hist.contains("changedir"), "history.toml should have the changedir bucket: {hist}");
    assert!(hist.contains("sbx"), "entered path should be recorded: {hist}");
}

/// リテラル引数版 `sort("size")` がソート種別を切り替える（段階3＝リテラル引数コマンド）。
#[test]
fn sort_command_changes_sort_type() {
    let server = Server::start(&["a.txt", "b.txt"], "");
    // 既定は名前順。
    let before = server.req("GET", "/state/panes/left/sort/type", "").unwrap().1;
    assert_eq!(before.trim(), "\"FileName\"", "default sort should be FileName");
    let flag = server.req("GET", "/state/panes/left/sort/default", "").unwrap().1;
    assert_eq!(flag.trim(), "true", "initial sort should match the configured default");

    server.req("POST", "/command/sort", r#"["size"]"#).unwrap();
    let after = server.req("GET", "/state/panes/left/sort/type", "").unwrap().1;
    assert_eq!(after.trim(), "\"Length\"", "sort(\"size\") should switch to Length");
    let flag = server.req("GET", "/state/panes/left/sort/default", "").unwrap().1;
    assert_eq!(flag.trim(), "false", "non-default sort should clear the default flag");
}

/// 引数無しの `sort()` は config の既定ソート（既定は FileName）に従う（原作準拠）。
#[test]
fn sort_command_without_arg_applies_default() {
    let server = Server::start(&["a.txt", "b.txt"], "");
    // いったん既定と違う種別（サイズ順）にする。
    server.req("POST", "/command/sort", r#"["size"]"#).unwrap();
    assert_eq!(
        server.req("GET", "/state/panes/left/sort/type", "").unwrap().1.trim(),
        "\"Length\"",
    );
    // 引数無しなら config の既定（FileName）へ戻る。
    server.req("POST", "/command/sort", "").unwrap();
    assert_eq!(
        server.req("GET", "/state/panes/left/sort/type", "").unwrap().1.trim(),
        "\"FileName\"",
        "argless sort() should fall back to the configured default (FileName)",
    );
    assert_eq!(
        server.req("GET", "/state/panes/left/sort/default", "").unwrap().1.trim(),
        "true",
        "returning to the configured default should set the default flag again",
    );
}

/// `setCursorPosition("c.txt")` がカーソルを指定名のファイルへ移す。
#[test]
fn set_cursor_position_jumps_to_named_file() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "");
    server
        .req("POST", "/command/setCursorPosition", r#"["c.txt"]"#)
        .unwrap();
    let cur = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    let cur = cur.trim();
    let name = server
        .req("GET", &format!("/state/panes/left/items/{cur}/name"), "")
        .unwrap()
        .1;
    assert_eq!(name.trim(), "\"c.txt\"", "cursor should land on c.txt");
}

/// `setCursorIndex(n)` がカーソルを 0 始まりの絶対番号へ移し、範囲外は端へ丸める。
#[test]
fn set_cursor_index_moves_to_absolute_position() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "");
    // 絶対番号 2 へ（debug-server は引数を文字列で受ける＝スクリプト経路と同じ）。
    server
        .req("POST", "/command/setCursorIndex", r#"["2"]"#)
        .unwrap();
    let cur = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(cur.trim(), "2", "cursor should land on index 2");
    // 範囲超過は末尾へ丸める（".." の有無に依らず最終項目は c.txt）。
    server
        .req("POST", "/command/setCursorIndex", r#"["999"]"#)
        .unwrap();
    let cur = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    let cur = cur.trim();
    let name = server
        .req("GET", &format!("/state/panes/left/items/{cur}/name"), "")
        .unwrap()
        .1;
    assert_eq!(name.trim(), "\"c.txt\"", "out-of-range index clamps to last item");
    // 負数は先頭へ丸める。
    server
        .req("POST", "/command/setCursorIndex", r#"["-5"]"#)
        .unwrap();
    let cur = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(cur.trim(), "0", "negative index clamps to 0");
}

/// `centerCursor()` がカーソル行を画面中央へスクロールする（scroll_top = cursor - page/2）。
#[test]
fn center_cursor_scrolls_cursor_to_middle() {
    // ページ行数より十分多い項目を置き、カーソルが端でクランプされない中ほどへ。
    let names: Vec<String> = (0..300).map(|i| format!("f{i:03}.txt")).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let server = Server::start(&refs, "");
    server
        .req("POST", "/command/setCursorIndex", r#"["150"]"#)
        .unwrap();
    server.req("POST", "/command/centerCursor", "").unwrap();
    let n = |path: &str| -> i64 {
        server.req("GET", path, "").unwrap().1.trim().parse().unwrap()
    };
    let cursor = n("/state/panes/left/cursor");
    let top = n("/state/panes/left/scroll_top");
    let page = n("/state/panes/left/page_rows");
    assert_eq!(cursor, 150, "cursor should be at 150");
    assert!(page > 2 && page < 150, "sane headless page_rows: {page}");
    // カーソルが画面中央＝先頭からの距離がページの半分。
    assert_eq!(cursor - top, page / 2, "cursor not centered: top={top} page={page}");
}

/// `changeDrive("X:")` がアクティブペインを指定ドライブのルートへ移す。
/// サンドボックスがどのドライブにあっても動くよう、現在地のドライブ文字を使う。
#[test]
fn change_drive_navigates_to_root() {
    let server = Server::start(&["a.txt"], "");
    let loc = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    let loc_raw = loc.trim().trim_matches('"').replace("\\\\", "\\");
    let drive = &loc_raw[..1];

    server
        .req("POST", "/command/changeDrive", &format!(r#"["{drive}:"]"#))
        .unwrap();
    let after = poll(&server, "/state/panes/left/location", |b| {
        b.trim().trim_matches('"').replace("\\\\", "\\") != loc_raw
    });
    let after_raw = after.trim().trim_matches('"').replace("\\\\", "\\");
    assert!(
        after_raw.starts_with(&format!("{drive}:")) && after_raw.len() <= 3,
        "changeDrive should land on the drive root, got {after_raw}"
    );
}

/// `view`（引数なし）はファイルを内蔵ビューアで開く（enterDir の外部起動と違う手触り）。
#[test]
fn view_command_opens_internal_viewer_for_file() {
    let server = Server::start(&["note.txt"], "");
    server
        .req("POST", "/command/setCursorPosition", r#"["note.txt"]"#)
        .unwrap();
    // view（type なし）＝内蔵テキストビューアで開く。MaybeModal 扱いなので exec は応答後に走る。
    server.req("POST", "/command/view", "").unwrap();
    let av = poll(&server, "/state/active_view", |b| b.trim() == "\"text\"");
    assert_eq!(av.trim(), "\"text\"", "view on a text file should open the internal text viewer");
    // 閉じると元へ戻る。
    server.req("POST", "/view/key/close", "").unwrap();
    let av2 = poll(&server, "/state/active_view", |b| b.trim() == "\"none\"");
    assert_eq!(av2.trim(), "\"none\"", "closing the viewer returns to the list");
}

/// 内容なし PDF（`pages` ページ・各 200x200）をバイト列で組み立てる。xref のオフセットを
/// 実バイト位置から計算するので厳密なパーサ（PDFium）でも開ける。
fn make_pdf(pages: usize) -> Vec<u8> {
    let mut objs = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string()];
    let kids = (0..pages)
        .map(|i| format!("{} 0 R", 3 + i))
        .collect::<Vec<_>>()
        .join(" ");
    objs.push(format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>"));
    for _ in 0..pages {
        objs.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_string());
    }
    let mut buf = Vec::new();
    buf.extend_from_slice(b"%PDF-1.4\n");
    let mut offs = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offs.push(buf.len());
        buf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = buf.len();
    let size = objs.len() + 1;
    buf.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \n");
    for o in &offs {
        buf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    buf.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    buf
}

/// PDF を view で開くと、テキストではなく画像ビューアにページ画像として載る（各ページを
/// PNG 化して前後送りする）。状態行の見出しは PDF 名・位置は総ページ数を映す。
#[test]
fn view_pdf_shows_pages_in_image_viewer() {
    let pdf = make_pdf(3);
    let server = Server::start_dirs(&[("doc.pdf", &pdf)], &[]);
    server
        .req("POST", "/command/setCursorPosition", r#"["doc.pdf"]"#)
        .unwrap();
    server.req("POST", "/command/view", "").unwrap();
    let av = poll(&server, "/state/active_view", |b| b.trim() == "\"media\"");
    assert_eq!(av.trim(), "\"media\"", "PDF は画像ビューアで開く（テキストではない）");
    let media = server.req("GET", "/state/media", "").unwrap().1;
    assert!(media.contains("\"total\":3"), "全 3 ページを巡回対象にする: {media}");
    assert!(media.contains("doc.pdf"), "状態行の見出しは PDF 名: {media}");
    // 次ページへ送れる（1 始まりの位置が 2 へ進む）。
    server.req("POST", "/view/key/next", "").unwrap();
    let media2 = poll(&server, "/state/media", |b| b.contains("\"index\":2"));
    assert!(media2.contains("\"index\":2"), "送りで 2 ページ目へ進む: {media2}");
    // 閉じると一覧へ戻る。
    server.req("POST", "/view/key/close", "").unwrap();
    let av2 = poll(&server, "/state/active_view", |b| b.trim() == "\"none\"");
    assert_eq!(av2.trim(), "\"none\"");
}

/// 拡大画像は Ctrl＋矢印で表示位置をパンできる（設定 `pan_step_px` の画素数だけ動く）。
/// 右を見る＝画像を左へ寄せる＝`pan_x` が負へ動き、左パンで中央へ戻る。
#[test]
fn image_viewer_pans_with_ctrl_arrows() {
    let pdf = make_pdf(1);
    let server = Server::start_dirs(&[("p.pdf", &pdf)], &[]);
    server
        .req("POST", "/command/setCursorPosition", r#"["p.pdf"]"#)
        .unwrap();
    server.req("POST", "/command/view", "").unwrap();
    poll(&server, "/state/active_view", |b| b.trim() == "\"media\"");
    // 拡大してパンできる状態にする（初期パンは中央）。
    server.req("POST", "/view/key/z", "").unwrap();
    let before = server.req("GET", "/state/media", "").unwrap().1;
    assert!(before.contains("\"pan_x\":0"), "初期パンは中央: {before}");
    // Ctrl+Right＝右を見る＝pan_x が負へ。
    server.req("POST", "/view/key/Ctrl+Right", "").unwrap();
    let after = poll(&server, "/state/media", |b| b.contains("\"pan_x\":-"));
    assert!(after.contains("\"pan_x\":-"), "Ctrl+Right で右へパンする: {after}");
    // Ctrl+Left で中央へ戻る。
    server.req("POST", "/view/key/Ctrl+Left", "").unwrap();
    let back = poll(&server, "/state/media", |b| b.contains("\"pan_x\":0"));
    assert!(back.contains("\"pan_x\":0"), "Ctrl+Left で戻る: {back}");
}

/// Alt 併用キーが WM_SYSKEYDOWN 経由でキーバインドへ回る（メニューに食われない）。
/// `/filer/syskey/G` は key_sink へ実 SYSKEYDOWN を送り、Alt+G に割り当てた式を発火させる。
#[test]
fn filer_alt_keybind_runs_via_syskeydown() {
    let server = Server::start(&["a.txt"], "[keybinds]\n\"Alt+G\" = 'r.log(\"alt-fired\")'\n");
    server.req("POST", "/filer/syskey/G", "").expect("syskey");
    let log = poll(&server, "/state/log", |b| b.contains("alt-fired"));
    assert!(log.contains("alt-fired"), "Alt+G バインドが SYSKEYDOWN 経由で実行される: {log}");
}

/// reload は side 引数で対象ペインを選べる（無指定＝両方・left/right/opposite/active＝1ペイン）。
/// 更新監視を切って起動するので、ディスクへ足したファイルは明示 reload まで一覧に出ない＝決定的。
#[test]
fn reload_targets_pane_by_side_arg() {
    let server = Server::start(&["a.txt"], "[reload_watch]\nenabled = false\n");
    std::fs::write(server.base.join("sbx").join("b.txt"), b"x").unwrap();
    let has_b = |side: &str, s: &Server| {
        s.req("GET", &format!("/state/panes/{side}/items"), "")
            .unwrap()
            .1
            .contains("\"name\":\"b.txt\"")
    };

    // reload("left") は左だけ更新＝左に b.txt が出て、右はまだ出ない。
    server.req("POST", "/command/reload", r#"["left"]"#).expect("reload left");
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"b.txt\""));
    assert!(has_b("left", &server), "reload(\"left\") で左に b.txt");
    assert!(!has_b("right", &server), "右は未 reload なので b.txt は出ない");

    // 無指定 reload() は両方更新＝右にも b.txt。
    server.req("POST", "/command/reload", "").expect("reload both");
    poll(&server, "/state/panes/right/items", |b| b.contains("\"name\":\"b.txt\""));
    assert!(has_b("right", &server), "無指定 reload で右にも b.txt");
}

/// 更新監視が有効なら、外部でディスクに足したファイルが明示 reload なしで一覧へ出る。
/// 監視スレッドが表示中の実ディレクトリを見張り、静穏（`wait_ms`）後に自動再読込する。
#[test]
fn watch_reflects_external_change_when_enabled() {
    let server = Server::start(&["a.txt"], "[reload_watch]\nenabled = true\nwait_ms = 100\n");
    // rerics のコマンドを介さず、外部からディスクへ直接 b.txt を足す。
    std::fs::write(server.base.join("sbx").join("b.txt"), b"x").unwrap();
    // 明示 reload せずとも監視→静穏待ち→自動再読込で b.txt が現れる。
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"b.txt\""));
    let left = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert!(left.contains("\"name\":\"b.txt\""), "監視で b.txt が自動反映される: {left}");
}

/// 更新監視を切ると、外部変更は明示 reload まで反映されない（原作 AutoReload=off 相当）。
#[test]
fn watch_disabled_needs_explicit_reload() {
    let server = Server::start(&["a.txt"], "[reload_watch]\nenabled = false\nwait_ms = 100\n");
    std::fs::write(server.base.join("sbx").join("b.txt"), b"x").unwrap();
    // 監視オフなので、待っても自動では出ない。
    std::thread::sleep(Duration::from_millis(600));
    let before = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert!(!before.contains("\"name\":\"b.txt\""), "監視オフでは自動反映されない: {before}");
    // 明示 reload で初めて出る。
    server.req("POST", "/command/reload", "").expect("reload");
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"b.txt\""));
    let after = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert!(after.contains("\"name\":\"b.txt\""), "明示 reload 後は出る: {after}");
}

/// テキストビューア表示中はビューア用コマンドがビューア文脈で実行される。
#[test]
fn viewer_commands_dispatch_in_text_context() {
    let server = Server::start(&["note.txt"], "");
    server
        .req("POST", "/command/setCursorPosition", r#"["note.txt"]"#)
        .unwrap();
    server.req("POST", "/command/view", "").unwrap();
    poll(&server, "/state/active_view", |b| b.trim() == "\"text\"");
    // バイナリ/テキスト切替は表示モードだけ変え、ビューアは開いたまま。
    let (st, _) = server
        .req("POST", "/command/viewerToggleMode", "")
        .expect("viewerToggleMode");
    assert_eq!(st, 200, "viewerToggleMode はテキストビューア文脈で実行される");
    let av = server.req("GET", "/state/active_view", "").unwrap().1;
    assert_eq!(av.trim(), "\"text\"", "モード切替後もテキストビューアは開いたまま");
    // 実キー経路（キーマップ解決→コマンド実行）で Esc を送ると閉じる。
    server.req("POST", "/view/key/Esc", "").unwrap();
    let av2 = poll(&server, "/state/active_view", |b| b.trim() == "\"none\"");
    assert_eq!(av2.trim(), "\"none\"", "Esc の実キー経路で一覧へ戻る");
}

/// pathRegisterDialog で現在地を登録し、jumpDialog でそこへ戻る。
#[test]
fn nav_register_and_jump() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server
        .req("GET", "/state/panes/left/location", "")
        .unwrap()
        .1
        .trim()
        .to_string();

    // 現在地（sbx）を "home" として登録する。
    server.req("POST", "/command/pathRegisterDialog", "").unwrap();
    let m = wait_modal(&server);
    assert!(m.contains("\"has_input\":true"), "register should ask for a label: {m}");
    server.req("POST", "/modal/text", "home").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");

    // 親へ移動してから、ジャンプで登録先（sbx）へ戻る。
    server.req("POST", "/command/toParent", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() != sbx_json);

    server.req("POST", "/command/jumpDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"jump\""), "jump should open the registered-dir list: {modal}");
    assert!(modal.contains("\"rows\":[["), "jump should be a multi-column list: {modal}");
    assert!(modal.contains("home"), "jump should list the bookmark: {modal}");
    server.req("POST", "/modal/select/0", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let back = poll(&server, "/state/panes/left/location", |b| b.trim() == sbx_json);
    assert_eq!(back.trim(), sbx_json, "jump should navigate to the bookmark");
}

/// #71: config に書いた登録ディレクトリのショートカットが、ジャンプダイアログの
/// 先頭（ショートカット）列に表示される（多列 ListView＋serde フィールドの確認）。
#[test]
fn jump_dialog_shows_configured_shortcut() {
    let cfg = "[[bookmarks]]\nlabel = \"ルート\"\npath = \"C:\\\\\"\nshortcut = \"C\"\n";
    let server = Server::start(&["a.txt"], cfg);
    server.req("POST", "/command/jumpDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"jump\""), "should open the jump dialog: {modal}");
    assert!(modal.contains("ルート"), "configured label should be listed: {modal}");
    // 行の先頭列がショートカット "C"。
    assert!(modal.contains("[\"C\",\"ルート\""), "shortcut should fill the first column: {modal}");
    server.req("POST", "/modal/command/cancel", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// changeDriveDialog＝ドライブ一覧から選んでそのルートへ移動する。
/// 既定選択は現在ドライブなので、OK で現在ドライブのルートへ移る。
#[test]
fn nav_change_drive_dialog() {
    let server = Server::start(&["a.txt"], "");
    let before = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    let drive = before.chars().nth(1).expect("drive letter");
    let expected = format!("\"{drive}:\\\\\"");

    server.req("POST", "/command/changeDriveDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"drive\""), "drive dialog should open the drive selector: {modal}");
    assert!(modal.contains("\"rows\":[["), "should list at least one drive row: {modal}");
    assert!(modal.contains(&format!("\"{drive}:\"")), "should list the current drive: {modal}");

    // 既定選択（現在ドライブ）のまま OK。
    server.req("POST", "/modal/command/ok", "").unwrap();
    let after = poll(&server, "/state/panes/left/location", |b| b.trim() == expected);
    assert_eq!(after.trim(), expected, "selecting the current drive should go to its root");
}

/// keyBindsDialog＝現在のキー割り当てをリストモーダルで読み取り専用表示する。
#[test]
fn keybinds_dialog_lists_current_bindings() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/keyBindsDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"list\""), "should open a list modal: {modal}");
    // 既定キーの一つ（Enter→enterDir）が一覧に出る。
    assert!(modal.contains("enterDir"), "binding list should include enterDir: {modal}");
    // 閉じる（選択結果は使わない）。
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// sortDialog＝ソート設定モーダルを開いて閉じる（並べ替えのみ＝allow-write 不要）。
/// ラジオ値の選択は未対応だが、開閉でデッドロックしないこと＋種別/昇降の現在値表示を担保。
#[test]
fn sort_dialog_opens_and_closes() {
    let server = Server::start(&["a.txt", "b.txt"], "");
    server.req("POST", "/command/sortDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"sort\""), "should open sort modal: {modal}");
    // 既定選択のまま OK（現在のソートで再適用＝無害）。
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    // 一覧は健在。
    let items = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert!(items.contains("\"name\":\"a.txt\""), "list should remain: {items}");
}

/// ソート設定モーダルで「種別ラジオ＋エクスプローラ互換チェック＋降順チェック」を
/// ニーモニックで操作し、OK 後の種別/昇降が組み合わせどおりになることを担保する。
/// （互換チェックは名前/拡張子に直交＝拡張子＋互換で ExtensionExpLike になる。）
#[test]
fn sort_dialog_explike_and_reverse() {
    let server = Server::start(&["a.txt", "b.txt"], "");
    let before = server.req("GET", "/state/panes/left/sort/type", "").unwrap().1;
    assert_eq!(before.trim(), "\"FileName\"", "default sort should be FileName");

    server.req("POST", "/command/sortDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"sort\""), "should open sort modal: {modal}");

    // 初期フォーカス＝選択中ラジオ（名前順）。↓で拡張子ラジオへ。
    // Tab で降順チェック→互換チェックの順にフォーカスし、Space でそれぞれトグルする。
    server.req("POST", "/modal/key/down", "").unwrap();
    server.req("POST", "/modal/key/tab", "").unwrap();
    server.req("POST", "/modal/key/space", "").unwrap();
    server.req("POST", "/modal/key/tab", "").unwrap();
    server.req("POST", "/modal/key/space", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");

    let ty = server.req("GET", "/state/panes/left/sort/type", "").unwrap().1;
    assert_eq!(ty.trim(), "\"ExtensionExpLike\"", "拡張子＋互換 → ExtensionExpLike: {ty}");
    let rev = server.req("GET", "/state/panes/left/sort/reverse", "").unwrap().1;
    assert_eq!(rev.trim(), "true", "降順チェックで reverse=true: {rev}");
}

/// incrementalSearchDialog＝打鍵ごとにカーソルが一致項目へ追従し、OK で確定する。
#[test]
fn find_incremental_search_follows_typing() {
    let server = Server::start(&["alpha.txt", "banana.txt", "cherry.txt"], "");
    // items は [.., alpha(1), banana(2), cherry(3)]。
    server.req("POST", "/command/incrementalSearchDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"incremental\""), "should open incremental modal: {modal}");
    assert!(modal.contains("\"has_input\":true"), "should have a text field: {modal}");

    // "ban" と打つとカーソルが banana.txt（index 2）へ追従する。
    server.req("POST", "/modal/text", "ban").unwrap();
    let c = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    assert_eq!(c.trim(), "2", "cursor should follow typing to banana.txt: {c}");

    // OK で確定。モーダルが閉じてもカーソルは 2 のまま。
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let c2 = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(c2.trim(), "2", "cursor should stay put after confirm: {c2}");
}

/// 中止すると開始時のカーソルへ戻す。
#[test]
fn find_incremental_search_cancel_restores() {
    let server = Server::start(&["alpha.txt", "banana.txt", "cherry.txt"], "");
    // 開始カーソルを 1（alpha）にしておく。
    server.req("POST", "/command/cursorDown", "").unwrap();
    let origin = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    assert_eq!(origin.trim(), "1");

    server.req("POST", "/command/incrementalSearchDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "cher").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "3");

    // 中止で origin(1) に戻る。
    server.req("POST", "/modal/command/cancel", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let c = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(c.trim(), "1", "cancel should restore the original cursor: {c}");
}

/// incrementalSearchDialog の「次(&N)」＝次の一致・「前(&P)」＝前の一致（原作 Next/Previous）。
/// 打鍵は先頭から追従、次は現在行の次から前方・折り返さない。ボタン id は 次=3・前=4。
#[test]
fn find_incremental_search_steps_matches() {
    let server = Server::start(&["a1.txt", "a2.txt", "b.txt"], "");
    // items は [.., a1(1), a2(2), b(3)]。
    server.req("POST", "/command/incrementalSearchDialog", "").unwrap();
    wait_modal(&server);

    // "a" で先頭一致 a1(1) へ追従。
    server.req("POST", "/modal/text", "a").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    // 「次」＝次の一致 a2(2) へ。
    server.req("POST", "/modal/command/3", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");

    // さらに「次」：次の "a" 一致は無く、折り返さないので 2 のまま。
    server.req("POST", "/modal/command/3", "").unwrap();
    let c = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(c.trim(), "2", "no wrap: cursor stays at the last match: {c}");

    // 「前」＝前の一致 a1(1) へ。
    server.req("POST", "/modal/command/4", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    let c2 = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(c2.trim(), "1", "prev steps to the previous match: {c2}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 検索結果一覧のビューア：基準直下ではなくサブフォルダ（項目の出自）にある実ファイルを開く。
/// 出自を見ずに基準直下を引くと「開けません」になるので、ここで出自解決の回帰を防ぐ。
#[test]
fn find_result_viewer_opens_item_from_source() {
    let server = Server::start(&["note.dat"], "");
    let sub = server.base.join("sbx").join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("target.txt"), b"hello from sub").unwrap();

    // 条件ダイアログは複数 Edit で駆動不可なので、マスク検索だけを直接起こす。
    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| {
        b.contains("\"name\":\"target.txt\"")
    });
    assert!(items.contains("\"info\":\"sub\""), "found item carries its source subpath: {items}");
    let fr = server.req("GET", "/state/panes/left/find_result", "").unwrap().1;
    assert_eq!(fr.trim(), "true", "result mode active: {fr}");

    // 結果は [.., target.txt]。cursorDown で target.txt（index 1）へ。
    server.req("POST", "/command/cursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    // 出自から実ファイルを読めればテキストビューアが前面に出る（active_view=text）。
    server.req("POST", "/command/view", "").unwrap();
    let av = poll(&server, "/state/active_view", |b| b.trim() == "\"text\"");
    assert_eq!(av.trim(), "\"text\"", "text viewer should open from item source: {av}");
    let log = server.req("GET", "/state/log/lines", "").unwrap().1;
    assert!(!log.contains("\u{958b}\u{3051}\u{307e}\u{305b}\u{3093}"), "no open error: {log}");
}

/// 検索結果一覧での親移動は、実際の親ではなく検索を開始したディレクトリ（基準）へ戻り、
/// 結果モードを抜ける。
#[test]
fn find_result_parent_returns_to_base() {
    let server = Server::start(&["note.dat"], "");
    let sub = server.base.join("sbx").join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("target.txt"), b"x").unwrap();

    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"target.txt\""));

    // 親移動＝結果一覧を抜けて基準ディレクトリを再表示する（親へは行かない）。
    server.req("POST", "/command/toParent", "").unwrap();
    let fr = poll(&server, "/state/panes/left/find_result", |b| b.trim() == "false");
    assert_eq!(fr.trim(), "false", "should leave result mode: {fr}");
    let items = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert!(items.contains("\"name\":\"note.dat\""), "back to base listing: {items}");
    assert!(items.contains("\"name\":\"sub\""), "base shows the subdir: {items}");
    assert!(!items.contains("\"name\":\"target.txt\""), "no longer in result mode: {items}");
}

/// 検索結果一覧から反対側へのコピーは、項目を出自ディレクトリ別にまとめて行う。
/// 異なるサブフォルダで見つけた項目もまとめて反対側へ届く。
#[test]
fn find_result_copy_uses_item_source() {
    let server = Server::start_dirs_writable(&[("note.dat", b"z")], &[]);
    let left = server.base.join("left");
    std::fs::create_dir_all(left.join("sub1")).unwrap();
    std::fs::create_dir_all(left.join("sub2")).unwrap();
    std::fs::write(left.join("sub1").join("x.txt"), b"X").unwrap();
    std::fs::write(left.join("sub2").join("y.txt"), b"Y").unwrap();

    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    poll(&server, "/state/panes/left/items", |b| {
        b.contains("\"name\":\"x.txt\"") && b.contains("\"name\":\"y.txt\"")
    });

    // 両方をマークして反対側へコピーする（選択の反映を待ってから copy する）。
    server
        .req(
            "POST",
            "/script/eval",
            r#"rerics.activePane().items.forEach((it) => { if (it.name === "x.txt" || it.name === "y.txt") it.selected = true; });"#,
        )
        .unwrap();
    poll(&server, "/state/panes/left/items", |b| {
        count_substr(b, "\"marked\":true") == 2
    });
    server.req("POST", "/command/copy", "").unwrap();

    let right = poll(&server, "/state/panes/right/items", |b| {
        b.contains("\"name\":\"x.txt\"") && b.contains("\"name\":\"y.txt\"")
    });
    assert!(right.contains("\"name\":\"x.txt\""), "x.txt copied to other pane: {right}");
    assert!(right.contains("\"name\":\"y.txt\""), "y.txt copied to other pane: {right}");
    // 出自の違う2ファイルが実体としても届いていること。
    assert_eq!(std::fs::read(server.base.join("right").join("x.txt")).unwrap(), b"X");
    assert_eq!(std::fs::read(server.base.join("right").join("y.txt")).unwrap(), b"Y");
}

/// 結果一覧のパス系操作（ショートカット作成）も項目の出自から解決する。.lnk は基準直下では
/// なく出自サブフォルダの実ファイルの隣に作られる。
#[test]
fn find_result_shortcut_uses_item_source() {
    let server = Server::start_dirs_writable(&[("note.dat", b"z")], &[]);
    let left = server.base.join("left");
    std::fs::create_dir_all(left.join("sub")).unwrap();
    std::fs::write(left.join("sub").join("t.txt"), b"x").unwrap();

    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"t.txt\""));
    // 結果は [.., t.txt]。cursorDown で t.txt（index 1）へ。
    server.req("POST", "/command/cursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    server.req("POST", "/command/createShortcut", "").unwrap();
    let made = left.join("sub").join("t.txt.lnk");
    let mut ok = false;
    for _ in 0..40 {
        if made.exists() {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(ok, "shortcut should be created next to the source file (in sub)");
    assert!(!left.join("t.txt.lnk").exists(), "not created in the search base");
}

/// 結果一覧の reload（End キー等）は、同名ファイルが別フォルダに複数あっても、元のカーソル
/// 位置（行）を保つ。名前一致で復元すると先頭側の同名へ飛ぶため、位置（index）で復元する。
#[test]
fn find_result_reload_keeps_cursor_position() {
    let server = Server::start(&["note.dat"], "");
    let s1 = server.base.join("sbx").join("s1");
    let s2 = server.base.join("sbx").join("s2");
    std::fs::create_dir_all(&s1).unwrap();
    std::fs::create_dir_all(&s2).unwrap();
    std::fs::write(s1.join("x.txt"), b"x").unwrap();
    std::fs::write(s2.join("x.txt"), b"x").unwrap();
    std::fs::write(s2.join("y.txt"), b"x").unwrap();

    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    poll(&server, "/state/panes/left/items", |b| count_substr(b, "\"name\":\"x.txt\"") == 2);
    // 末尾の項目（先頭でない行）へカーソルを置く。
    server.req("POST", "/command/setCursorIndex", "[\"3\"]").unwrap();
    let before = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(before.trim(), "3", "cursor moved off the top before reload");

    server.req("POST", "/command/reload", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| count_substr(b, "\"name\":\"x.txt\"") == 2);
    let after = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(after.trim(), "3", "reload keeps the cursor at the same row (not back to top)");
}

/// 検索の完了サマリは走査件数つきでログに出る。進捗行（「検索中…」）がインプレースで
/// 「検索結果 N件（走査 M件）」へ確定する＝完了後に「検索中」行が残らない。
#[test]
fn find_reports_scan_progress_summary() {
    let server = Server::start(&["note.dat"], "");
    let sub = server.base.join("sbx").join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("a.txt"), b"1").unwrap();
    std::fs::write(sub.join("b.txt"), b"2").unwrap();

    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    let log = poll(&server, "/state/log", |b| b.contains("検索結果"));
    assert!(log.contains("走査"), "完了サマリに走査件数が出る: {log}");
    assert!(!log.contains("検索中"), "進捗行が最終サマリへ確定し「検索中」行は残らない: {log}");
}

/// 結果一覧での再読込（reload・End キー等）は、ディレクトリへ戻らず再検索する。後から増えた
/// 一致ファイルも拾い、結果モードのまま最新化される。
#[test]
fn find_result_reload_researches() {
    let server = Server::start(&["note.dat"], "");
    let sub = server.base.join("sbx").join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("a.txt"), b"x").unwrap();

    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"a.txt\""));

    // 後から一致ファイルを足し、reload で再検索が拾うことを見る。
    std::fs::write(sub.join("c.txt"), b"y").unwrap();
    server.req("POST", "/command/reload", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| {
        b.contains("\"name\":\"a.txt\"") && b.contains("\"name\":\"c.txt\"")
    });
    assert!(items.contains("\"name\":\"c.txt\""), "reload re-searches and picks up c.txt: {items}");
    let fr = server.req("GET", "/state/panes/left/find_result", "").unwrap().1;
    assert_eq!(fr.trim(), "true", "stays in result mode after reload: {fr}");
}

/// 結果一覧で削除すると、ディレクトリへ戻らず**再検索して一覧を最新化**する（結果モード維持）。
#[test]
fn find_result_delete_refreshes_in_place() {
    let server = Server::start_dirs_writable(&[("note.dat", b"z")], &[]);
    let left = server.base.join("left");
    std::fs::create_dir_all(left.join("sub")).unwrap();
    std::fs::write(left.join("sub").join("a.txt"), b"A").unwrap();
    std::fs::write(left.join("sub").join("b.txt"), b"B").unwrap();

    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    poll(&server, "/state/panes/left/items", |b| {
        b.contains("\"name\":\"a.txt\"") && b.contains("\"name\":\"b.txt\"")
    });
    server
        .req("POST", "/script/eval", r#"rerics.activePane().items.forEach((it)=>{ if(it.name==="a.txt") it.selected=true; });"#)
        .unwrap();
    poll(&server, "/state/panes/left/items", |b| count_substr(b, "\"marked\":true") == 1);

    server.req("POST", "/command/delete", "").unwrap();
    // 削除確認に「はい」と答える。
    let modal = wait_modal(&server);
    assert!(modal.contains("\u{524a}\u{9664}"), "delete confirm: {modal}");
    server.req("POST", "/modal/command/1", "").unwrap();
    // 再検索で a.txt が消え b.txt が残る。基準ディレクトリへは戻らない（結果モードのまま）。
    let items = poll(&server, "/state/panes/left/items", |b| {
        b.contains("\"name\":\"b.txt\"") && !b.contains("\"name\":\"a.txt\"")
    });
    assert!(items.contains("\"name\":\"b.txt\""), "b.txt remains: {items}");
    assert!(!items.contains("\"name\":\"a.txt\""), "a.txt gone from list: {items}");
    let fr = server.req("GET", "/state/panes/left/find_result", "").unwrap().1;
    assert_eq!(fr.trim(), "true", "stays in result mode after delete: {fr}");
    assert!(!left.join("sub").join("a.txt").exists(), "a.txt deleted on disk");
    assert!(left.join("sub").join("b.txt").exists(), "b.txt kept on disk");
}

/// 結果一覧の情報表示（使用量計算）は、出自の異なる項目をまとめて合算する。
#[test]
fn find_result_directory_information_sums_sources() {
    let server = Server::start(&["note.dat"], "");
    let sbx = server.base.join("sbx");
    std::fs::create_dir_all(sbx.join("a")).unwrap();
    std::fs::create_dir_all(sbx.join("b")).unwrap();
    std::fs::write(sbx.join("a").join("p.txt"), b"12345").unwrap(); // 5 バイト
    std::fs::write(sbx.join("b").join("q.txt"), b"678").unwrap(); // 3 バイト

    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    poll(&server, "/state/panes/left/items", |b| {
        b.contains("\"name\":\"p.txt\"") && b.contains("\"name\":\"q.txt\"")
    });
    server
        .req("POST", "/script/eval", r#"rerics.activePane().items.forEach((it)=>{ if(it.name==="p.txt"||it.name==="q.txt") it.selected=true; });"#)
        .unwrap();
    poll(&server, "/state/panes/left/items", |b| count_substr(b, "\"marked\":true") == 2);

    server.req("POST", "/command/directoryInformation", "").unwrap();
    // 別々のサブフォルダの 5+3 バイトが合算され、結果はログに出る（ダイアログは出さない）。
    let log = poll(&server, "/state/log", |b| b.contains("8 バイト"));
    assert!(log.contains("8 バイト"), "should sum to 8 bytes: {log}");
    let modal = server.req("GET", "/state/modal", "").unwrap().1;
    assert_eq!(modal.trim(), "null", "no result dialog: {modal}");
}

/// directoryInformation＝カーソル位置の使用量を計算し、結果をログに出す（ダイアログは出さない）。
#[test]
fn info_directory_information() {
    let server = Server::start(&["a.txt"], "");
    // ".." から a.txt（1バイト・b"x"）へカーソルを移す。
    server.req("POST", "/command/cursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    // 計算はワーカで走り、完了後に結果がログへ出る。
    server.req("POST", "/command/directoryInformation", "").unwrap();
    let log = poll(&server, "/state/log", |b| b.contains("1 バイト"));
    assert!(log.contains("ファイル"), "result goes to the log: {log}");
    let modal = server.req("GET", "/state/modal", "").unwrap().1;
    assert_eq!(modal.trim(), "null", "no result dialog: {modal}");
}

/// directoryInformation＝計算したディレクトリの一覧行は "<DIR>" の代わりにサイズを表示する
/// （再読込までの一時表示・原作準拠）。ログの数値は桁区切りカンマ表記。
#[test]
fn directory_information_shows_dir_size_in_list() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.base.join("sbx");
    std::fs::create_dir_all(sbx.join("sub")).unwrap();
    std::fs::write(sbx.join("sub").join("f.bin"), vec![0u8; 1500]).unwrap();
    server.req("POST", "/command/reload", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"sub\""));

    // sub を選択して使用量計算。完了で sub の行へサイズが入る。
    server
        .req("POST", "/script/eval", r#"rerics.activePane().items.forEach((it)=>{ if(it.name==="sub") it.selected=true; });"#)
        .unwrap();
    poll(&server, "/state/panes/left/items", |b| count_substr(b, "\"marked\":true") == 1);
    server.req("POST", "/command/directoryInformation", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| b.contains("\"size\":1500"));
    assert!(items.contains("\"size\":1500"), "sub row gets its size: {items}");
    let log = server.req("GET", "/state/log", "").unwrap().1;
    assert!(log.contains("1,500 バイト"), "log numbers are digit-grouped: {log}");
    // 進捗行の進行表示（ぐるぐる）は完了で確定・停止している。
    let log = poll(&server, "/state/log", |b| b.contains("\"progress\":[]"));
    assert!(log.contains("\"progress\":[]"), "spinner stops on completion: {log}");

    // 再読込で <DIR> 表示（size なし）へ戻る。
    server.req("POST", "/command/reload", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| !b.contains("\"size\":1500"));
}

/// renameSequenceDialog＝既定テンプレート（File<No:0000>.ext）で選択を連番リネームする。
#[test]
fn rename_sequence_template_default() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    // a.txt(1) と b.txt(2) をマークする（Space＝markToggle はマーク後に下へ）。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();

    server.req("POST", "/command/renameSequenceDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"rename_seq\""), "should open rename_seq modal: {modal}");

    // 既定テンプレ＝File<No:0000>.ext・開始1・刻み1・変換なし。そのまま OK。
    server.req("POST", "/modal/command/ok", "").unwrap();

    let items = poll(&server, "/state/panes/left/items", |b| b.contains("File0001.ext"));
    assert!(items.contains("\"name\":\"File0001.ext\""), "a.txt -> File0001.ext: {items}");
    assert!(items.contains("\"name\":\"File0002.ext\""), "b.txt -> File0002.ext: {items}");
    assert!(!items.contains("\"name\":\"a.txt\""), "old a.txt should be gone: {items}");
    assert!(!items.contains("\"name\":\"b.txt\""), "old b.txt should be gone: {items}");
}

/// 主部の大小変換ラジオ（小文字）が適用される＝File0001.ext → file0001.ext。
/// 初期フォーカス＝テンプレコンボ。Tab で主部ラジオ群へ移り、↓↓で「小文字」を選ぶ。
#[test]
fn rename_sequence_base_lowercase() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();

    server.req("POST", "/command/renameSequenceDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/key/tab", "").unwrap();
    server.req("POST", "/modal/key/down", "").unwrap();
    server.req("POST", "/modal/key/down", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();

    let items = poll(&server, "/state/panes/left/items", |b| b.contains("file0001.ext"));
    assert!(items.contains("\"name\":\"file0001.ext\""), "主部小文字 a.txt -> file0001.ext: {items}");
    assert!(items.contains("\"name\":\"file0002.ext\""), "主部小文字 b.txt -> file0002.ext: {items}");
}

/// sendToRecycled＝確認の上ゴミ箱へ送る（ファイルが一覧から消える）。
/// ※検証で実ゴミ箱に 1 バイトの一時ファイルが入る（無害）。
#[test]
fn shell_send_to_recycled() {
    // ソート昇順で a_del.txt が先頭ファイル（index 1）、z_keep.txt が後。
    let server = Server::start_writable(&["a_del.txt", "z_keep.txt"]);
    server.req("POST", "/command/cursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    server.req("POST", "/command/sendToRecycled", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\u{30b4}\u{30df}\u{7bb1}"), "should confirm before recycling: {modal}");
    // 「はい」で実行。
    server.req("POST", "/modal/command/yes", "").unwrap();

    let items = poll(&server, "/state/panes/left/items", |b| !b.contains("a_del.txt"));
    assert!(!items.contains("\"name\":\"a_del.txt\""), "recycled file should leave the pane: {items}");
    assert!(items.contains("\"name\":\"z_keep.txt\""), "other files remain: {items}");

    // 項目別の逐次ログが出る。
    let log = poll(&server, "/state/log", |b| b.contains("SendToRecycled"));
    assert!(log.contains("SendToRecycled a_del.txt"), "per-item recycle log should appear: {log}");
}

/// createShortcut＝カーソル項目を指す .lnk を同じ場所に作る。
#[test]
fn shell_create_shortcut() {
    let server = Server::start_writable(&["doc.txt"]);
    // ".." から doc.txt（唯一のファイル・index 1）へ。
    server.req("POST", "/command/cursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    server.req("POST", "/command/createShortcut", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| b.contains("doc.txt.lnk"));
    assert!(
        items.contains("\"name\":\"doc.txt.lnk\""),
        "a .lnk shortcut should be created next to the target: {items}"
    );
}

/// createLinkDialog＝種類選択モーダルで選んだリンクを反対ペインに作る。ディレクトリのみの
/// 選択では既定がジャンクションに自動選択され、そのまま OK で junction が作られる
/// （特権不要。シンボリックリンクは要特権のため e2e にしない）。
#[test]
fn create_link_dialog_makes_junction() {
    let server = Server::start_writable_split(&[]);
    std::fs::create_dir_all(server.base.join("sbxL").join("realdir")).unwrap();
    std::fs::write(server.base.join("sbxL").join("realdir").join("inner.txt"), "x").unwrap();
    server.req("POST", "/command/reload", "").unwrap();
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"realdir\""));
    server.req("POST", "/command/cursorDown", "").unwrap(); // .. -> realdir
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    server.req("POST", "/command/createLinkDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"link_kind\""), "link kind dialog should open: {modal}");
    // ディレクトリのみの選択＝ジャンクションが既定で選択されている。
    assert!(
        modal.contains(r#"{"label":"ジャンクション（ディレクトリのみ）(&J)","enabled":true,"checked":true}"#),
        "junction should be the enabled default for directory targets: {modal}"
    );
    server.req("POST", "/modal/command/ok", "").unwrap();

    // 反対（右）ペインに同名で現れ、junction 越しに対象の中身へ届く。
    let items = poll(&server, "/state/panes/right/items", |b| b.contains("\"name\":\"realdir\""));
    assert!(
        items.contains("\"name\":\"realdir\""),
        "a junction should be created in the opposite pane: {items}"
    );
    let linked = server.base.join("sbxR").join("realdir").join("inner.txt");
    assert!(linked.exists(), "the junction should resolve to the target contents");
}

/// ファイルを含む選択ではジャンクションが選べない（グレーアウト）。
#[test]
fn create_link_dialog_disables_junction_for_files() {
    let server = Server::start_writable_split(&["doc.txt"]);
    server.req("POST", "/command/cursorDown", "").unwrap(); // .. -> doc.txt
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    server.req("POST", "/command/createLinkDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"link_kind\""), "link kind dialog should open: {modal}");
    assert!(
        modal.contains(r#"{"label":"ジャンクション（ディレクトリのみ）(&J)","enabled":false"#),
        "junction should be disabled for file targets: {modal}"
    );
    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// clipCopy→（サブフォルダへ移動して）clipPaste で実コピーされる。
/// ※検証で OS のクリップボードを上書きする（汚染許容・テスト実行時のみ）。
#[test]
fn shell_clipboard_copy_paste() {
    let server = Server::start_writable(&["file.txt"]);
    // 貼付先サブフォルダをディスクに作って一覧へ反映。
    std::fs::create_dir_all(server.base.join("sbx").join("dest")).unwrap();
    server.req("POST", "/command/reload", "").unwrap();
    // items=[.., dest(1), file.txt(2)]。file.txt へカーソルを合わせてコピー。
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"dest\""));
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    server.req("POST", "/command/clipCopy", "").unwrap();

    // dest へ入って貼り付け。
    server.req("POST", "/command/cursorUp", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    server.req("POST", "/command/enterDir", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.contains("dest"));
    server.req("POST", "/command/clipPaste", "").unwrap();

    let items = poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"file.txt\""));
    assert!(
        items.contains("\"name\":\"file.txt\""),
        "pasted file should appear in the destination folder: {items}"
    );
}

/// #10: F5 リロードでカーソルが「同名ファイル」に留まる（先頭 .. へ戻らない）。
#[test]
fn reload_keeps_cursor_on_same_file() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "");

    // 左 items は [.., a.txt, b.txt, c.txt]。cursorDown×2 で b.txt(index 2)。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    let before = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(before.trim(), "2", "カーソルは b.txt(index 2) のはず");

    // F5 相当。カーソル保持なら 2 のまま、旧挙動なら 0(..) へ戻る。
    server.req("POST", "/command/reload", "").unwrap();
    let after = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    assert_eq!(after.trim(), "2", "リロード後もカーソルは b.txt に留まるべき（先頭へ戻らない）");
}

/// cursorOpposite はアクティブペインを反対側へトグルする。
#[test]
fn cursor_opposite_toggles_active_pane() {
    let server = Server::start(&["a.txt"], "");
    let a0 = server.req("GET", "/state/active_pane", "").unwrap().1;
    assert!(a0.contains("left"), "初期は左アクティブ: {a0}");
    server.req("POST", "/command/cursorOpposite", "").unwrap();
    let a1 = poll(&server, "/state/active_pane", |b| b.contains("right"));
    assert!(a1.contains("right"), "cursorOpposite で右へ: {a1}");
    server.req("POST", "/command/cursorOpposite", "").unwrap();
    let a2 = poll(&server, "/state/active_pane", |b| b.contains("left"));
    assert!(a2.contains("left"), "もう一度で左へ戻る: {a2}");
}

/// #74: CursorToParent=on のとき、アクティブ側ペインで外向きカーソルキー（左ペインで
/// focusLeft）が親移動になる。
#[test]
fn cursor_to_parent_navigates_on_outward_key() {
    let server = Server::start(&["a.txt"], "[cursor]\nto_parent = true\n");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    assert!(server.req("GET", "/state/active_pane", "").unwrap().1.contains("left"), "初期は左アクティブ");

    server.req("POST", "/command/focusLeft", "").unwrap();
    let up = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx);
    assert_ne!(up.trim(), sbx, "left+focusLeft with CursorToParent should go to parent");
    // 移動先は sbx の祖先（親）のはず。
    let parent = up.trim().trim_matches('"');
    let sbx_raw = sbx.trim_matches('"');
    assert!(sbx_raw.starts_with(parent), "moved location should be an ancestor of sbx: {up} / {sbx}");
}

/// #74: CursorToParent=off（既定）では focusLeft は親移動せず、フォーカス移動のみ。
#[test]
fn cursor_to_parent_off_keeps_focus_only() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    server.req("POST", "/command/focusLeft", "").unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let loc = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert_eq!(loc.trim(), sbx, "off: focusLeft must not navigate to parent");
}

/// selectFile はカーソル位置を（トグルでなく）マークし、カーソルを1つ下げる。
#[test]
fn select_file_marks_current_and_advances() {
    let server = Server::start(&["alpha.txt", "beta.txt"], "");
    // .. → alpha(index 1) へ。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/selectFile", "").unwrap();
    // カーソルは beta(index 2) へ進む。
    let cur = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    assert_eq!(cur.trim(), "2", "selectFile 後はカーソルが1つ下へ");
    // alpha(index 1) がマークされている（JSON Pointer の配列添字で直接取る）。
    let m = server.req("GET", "/state/panes/left/items/1/marked", "").unwrap().1;
    assert_eq!(m.trim(), "true", "alpha.txt(index 1) がマークされているはず");
}

/// down_after_select=false のとき、markToggle はマークするがカーソルを動かさない（#58/#63）。
#[test]
fn mark_toggle_respects_down_after_select_off() {
    let server = Server::start(&["alpha.txt", "beta.txt"], "[cursor]\ndown_after_select = false\n");
    // .. → alpha(index 1) へ。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/markToggle", "").unwrap();
    // カーソルは alpha(index 1) のまま。
    let cur = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(cur.trim(), "1", "down_after_select=false ではカーソルは動かない");
    let m = server.req("GET", "/state/panes/left/items/1/marked", "").unwrap().1;
    assert_eq!(m.trim(), "true", "alpha.txt はマークされる");
}

/// Shift+Space=markToggle({cursorMove:-1}) はマーク反転後にカーソルを1つ上へ動かす（#59）。
#[test]
fn shift_space_toggles_and_moves_up() {
    let server = Server::start(&["alpha.txt", "beta.txt", "gamma.txt"], "");
    // .. → alpha(1) → beta(2)。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/exec", "markToggle({cursorMove:-1})").unwrap();
    // beta(2) がマークされ、カーソルは alpha(1) へ上がる。
    let cur = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    assert_eq!(cur.trim(), "1", "Shift+Space 後はカーソルが1つ上へ");
    let m = server.req("GET", "/state/panes/left/items/2/marked", "").unwrap().1;
    assert_eq!(m.trim(), "true", "beta.txt(index 2) がマークされる");
}

/// Shift+矢印=cursorXxx({select:true}) はアンカーから現在位置までを範囲マークしながら移動する（#60/#208）。
#[test]
fn shift_arrow_range_selects() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt", "d.txt"], "");
    // .. → a(1)。ここがアンカー。
    server.req("POST", "/command/cursorDown", "").unwrap();
    // Shift+Down ×2＝a→b→c を範囲マーク。
    server.req("POST", "/exec", "cursorDown({select:true})").unwrap();
    server.req("POST", "/exec", "cursorDown({select:true})").unwrap();
    let cur = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "3");
    assert_eq!(cur.trim(), "3", "Shift+Down ×2 でカーソルは c(index 3)");
    for (idx, want) in [(1, "true"), (2, "true"), (3, "true"), (4, "false")] {
        let m = server
            .req("GET", &format!("/state/panes/left/items/{idx}/marked"), "")
            .unwrap()
            .1;
        assert_eq!(m.trim(), want, "index {idx} の marked");
    }
    // Shift+Up で範囲を縮めると c(3) のマークは落ちる（アンカー a は固定）。
    server.req("POST", "/exec", "cursorUp({select:true})").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    let m3 = server.req("GET", "/state/panes/left/items/3/marked", "").unwrap().1;
    assert_eq!(m3.trim(), "false", "範囲外になった c はマーク解除");
    let m1 = server.req("GET", "/state/panes/left/items/1/marked", "").unwrap().1;
    assert_eq!(m1.trim(), "true", "アンカー a は依然マーク");
}

/// refresh / nop は副作用なし（200 を返し状態を変えない）。
#[test]
fn refresh_and_nop_are_noops() {
    let server = Server::start(&["a.txt", "b.txt"], "");
    server.req("POST", "/command/cursorDown", "").unwrap();
    let before = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    let r = server.req("POST", "/command/refresh", "").expect("refresh").0;
    assert_eq!(r, 200, "refresh は 200");
    let n = server.req("POST", "/command/nop", "").expect("nop").0;
    assert_eq!(n, 200, "nop は 200");
    let after = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(before.trim(), after.trim(), "refresh/nop でカーソルは不変");
}

/// 生バイトで HTTP 応答を読む（PNG 等のバイナリ用。`req` は UTF-8 前提でバイナリを落とす）。
fn req_bytes(port: u16, method: &str, path: &str) -> Option<(u16, Vec<u8>)> {
    let mut s = connect_retry(port)?;
    // スナップショット（PrintWindow）は高負荷時に遅れるため read は長めに待つ。
    s.set_read_timeout(Some(Duration::from_secs(20))).ok();
    let head = format!(
        "{method} {path} HTTP/1.0\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    s.write_all(head.as_bytes()).ok()?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).ok()?;
    let sep = resp.windows(4).position(|w| w == b"\r\n\r\n")?;
    let line_end = resp.windows(2).position(|w| w == b"\r\n")?;
    let status: u16 = std::str::from_utf8(&resp[..line_end])
        .ok()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((status, resp[sep + 4..].to_vec()))
}

/// 設定ダイアログ＝独自モーダルだが modal_registry に登録済み。openSettings で開き、
/// スクリプト/コードの行は、キー割り当ての有無で位置が動かない（名前/コード順に固定）。
#[test]
fn settings_key_editor_script_rows_keep_position_when_bound() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00.ts",
            "rerics.registerCommand(\"aaaScript\", () => {});\nrerics.registerCommand(\"zzzScript\", () => {});",
        )],
    );
    poll(&server, "/script/commands", |b| b.contains("zzzScript"));
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // script 系に絞ると aaaScript・zzzScript が名前順（aaa が先・zzz が後）に並ぶ。
    server.req("POST", "/keys/filer/search", "script").unwrap();
    let before = keys();
    assert!(
        before.contains(r#""rows":[["aaaScript",[]],["zzzScript",[]]]"#),
        "未割当 2 行: {before}"
    );

    // 2 番目（zzzScript）へキーを割り当てても、行は 2 番目のまま動かない。
    server.req("POST", "/keys/filer/select/1", "").unwrap();
    server.req("POST", "/keys/filer/capture", "Ctrl+Alt+Z").unwrap();
    server.req("POST", "/keys/filer/search", "script").unwrap();
    let after = keys();
    assert!(
        after.contains(r#""rows":[["aaaScript",[]],["zzzScript",["Ctrl+Alt+Z"]]]"#),
        "zzz は割り当て後も 2 番目に居座る（aaa が先・zzz が後）: {after}"
    );

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 設定ナビを pane 番号で切り替える debug エンドポイント（`/settings/nav/<pane>`）＝
/// キー編集ページを前面に出して /snapshot/modal で撮れる（headless 観測）。未オープンは 400。
#[test]
fn settings_nav_switches_page_for_observation() {
    let server = Server::start(&["a.txt"], "");
    // 設定が開く前は切り替え先が無いので 400。
    assert_eq!(
        server.req("POST", "/settings/nav/5", "").expect("nav").0,
        400,
        "未オープンは 400"
    );
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    // キー（ファイラー）ページ＝pane 5 へ切替。
    assert_eq!(server.req("POST", "/settings/nav/5", "").expect("nav").0, 200, "切替 ok");
    // 前面に出たキーリストごと /snapshot/modal が PNG として撮れる。
    let (st, png) = req_bytes(server.port, "GET", "/snapshot/modal").expect("snap");
    assert_eq!(st, 200, "snapshot 200");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 署名で始まる");
    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 自前描画（プレビュー/スウォッチ）を含む窓を /snapshot/modal が PrintWindow で撮れ、
/// ナビをキーで動かしても /modal/command/cancel で閉じられる（デッドロックしない）。
#[test]
fn settings_dialog_opens_snapshots_and_closes() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"settings\""), "設定モーダルが開くはず: {modal}");

    // 自前描画コントロールを含むモーダルが PNG として撮れる（WM_PRINTCLIENT 応答の担保）。
    let (st, png) = req_bytes(server.port, "GET", "/snapshot/modal").expect("snapshot/modal");
    assert_eq!(st, 200, "/snapshot/modal は 200");
    assert!(
        png.starts_with(&[0x89, b'P', b'N', b'G']),
        "PNG 署名で始まるはず ({} bytes)",
        png.len()
    );
    assert!(png.len() > 1000, "PNG が空でないはず: {} bytes", png.len());

    // ナビ ListBox（開いた直後にフォーカス）を下キーで配色ペインへ。状態 JSON が返り壊れない。
    let down = server.req("POST", "/modal/key/down", "").expect("modal key down").0;
    assert_eq!(down, 200, "/modal/key/down は状態を返す");

    // キャンセルで閉じる（ブロックしない）。
    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 新設の設定ページ（レイアウト＝既定ウィンドウサイズ／全般＝既定エディタ／ビューア＝
/// ズーム増減）がナビのキー操作で到達でき、各ページが PrintWindow で撮れる（headless で
/// 観測可能であることの担保）。先頭は「テーマ・フォント」で、下キーでページを順に辿る。
#[test]
fn settings_new_pages_reachable_and_snapshot() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);

    let snap_ok = |label: &str| {
        let (st, png) = req_bytes(server.port, "GET", "/snapshot/modal").expect("modal snap");
        assert_eq!(st, 200, "{label}: /snapshot/modal は 200");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "{label}: PNG 署名で始まる");
        assert!(png.len() > 1000, "{label}: PNG が非空 ({} bytes)", png.len());
    };
    let down = |n: usize| {
        for _ in 0..n {
            server.req("POST", "/modal/key/down", "").expect("modal key down");
        }
    };
    down(3); // テーマ・フォント → 配色 → テキストビューア → レイアウト
    snap_ok("レイアウト");
    down(2); // → 一覧 → 全般
    snap_ok("全般");
    down(3); // → ファイル操作 → カーソル → ビューア
    snap_ok("ビューア");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 設定ダイアログのキー編集ページを headless で駆動する：割り当て（追記）・解除・既定戻し。
/// 開いていなければ 404・未知コマンドは 400。
#[test]
fn settings_key_editor_binds_unbinds_resets() {
    let server = Server::start(&["a.txt"], "");
    // 設定を開く前は 404。
    assert_eq!(
        server.req("GET", "/keys/filer", "").expect("keys closed").0,
        404,
        "設定が開いていなければ 404"
    );
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 既定：makeDirectoryDialog=K、selectMaskDialog=未割当、衝突なし。
    let s = keys();
    assert!(s.contains(r#"["makeDirectoryDialog",["K"]]"#), "既定 makeDirectoryDialog=K: {s}");
    assert!(s.contains(r#"["selectMaskDialog",[]]"#), "既定 selectMaskDialog 未割当: {s}");
    assert!(s.contains(r#""conflicts":[]"#), "既定は衝突なし: {s}");

    // 未使用キーを割り当て（実打鍵キャプチャと同じ assign 経路・衝突なし）。
    assert_eq!(
        server.req("POST", "/keys/filer/bind", r#"["selectMaskDialog","Ctrl+Shift+M"]"#).unwrap().0,
        200,
        "bind は ok"
    );
    let s = keys();
    assert!(s.contains(r#"["selectMaskDialog",["Ctrl+Shift+M"]]"#), "割り当てが反映: {s}");
    assert!(s.contains(r#""conflicts":[]"#), "未使用キーなので衝突なし: {s}");
    assert!(s.contains("を割り当てました"), "割り当て直後はメッセージが出る: {s}");

    // unbind：直前の bind で選択は selectMaskDialog。その割り当てを解除。
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    assert!(keys().contains(r#"["selectMaskDialog",[]]"#), "selectMaskDialog の割り当てが消える");

    // reset：既定へ戻る（makeDirectoryDialog=K が復活）。直後はステータスにメッセージが残る。
    server.req("POST", "/keys/filer/reset", "").unwrap();
    let s = keys();
    assert!(s.contains(r#"["makeDirectoryDialog",["K"]]"#), "reset で既定へ");
    assert!(!s.contains(r#""status":"""#), "reset 直後はメッセージが残る: {s}");
    // 次の操作（選択）でメッセージが消える＝残骸が居座らない。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    assert!(keys().contains(r#""status":"""#), "選択など次の操作でメッセージが消える: {}", keys());

    // 未知コマンド/キーは 400。
    assert_eq!(
        server.req("POST", "/keys/filer/bind", r#"["Nonexistent","K"]"#).unwrap().0,
        400,
        "未知コマンドは 400"
    );

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 既存キーへの割り当ては機能を消さず**追記**＝衝突マークが立つ。衝突があると OK は反映せず
/// **閉じない**（ステータスに重複メッセージ）。衝突を解消すると OK で閉じられる。
#[test]
fn settings_key_editor_conflicts_block_ok_until_resolved() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // K は既定で makeDirectoryDialog。これを selectMaskDialog にも割り当てる＝消えずに衝突。
    server.req("POST", "/keys/filer/bind", r#"["selectMaskDialog","K"]"#).unwrap();
    let s = keys();
    assert!(s.contains(r#"["makeDirectoryDialog",["K"]]"#), "makeDirectoryDialog は K を保持: {s}");
    assert!(s.contains(r#"["selectMaskDialog",["K"]]"#), "selectMaskDialog も K を得る: {s}");
    assert!(
        s.contains(r#""conflicts":[["K",["makeDirectoryDialog","selectMaskDialog"]]]"#),
        "K の衝突が立つ: {s}"
    );

    // OK を押しても衝突で閉じない（モーダルは残る・ステータスに重複メッセージ）。
    server.req("POST", "/modal/command/ok", "").expect("ok");
    poll(&server, "/keys/filer", |b| b.contains("重複"));
    assert_ne!(
        server.req("GET", "/state/modal", "").unwrap().1.trim(),
        "null",
        "衝突中は OK で閉じない"
    );

    // 衝突を解消：選択中（selectMaskDialog）の割り当てを外す＝K は makeDirectoryDialog だけに戻る。
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    let s = keys();
    assert!(s.contains(r#""conflicts":[]"#), "衝突が解消: {s}");
    assert!(s.contains(r#"["makeDirectoryDialog",["K"]]"#), "makeDirectoryDialog=K に戻る: {s}");

    // 解消後は OK で閉じられる。
    server.req("POST", "/modal/command/ok", "").expect("ok");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 機能順での個別削除：1 機能に複数キーがある時は 1 キー=1 行に割れる。削除したい行を選んで外す。
#[test]
fn settings_key_editor_per_chord_delete_in_command_view() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // makeDirectoryDialog に未使用キーを足す＝1 キー=1 行なので 2 行に割れる（既定 K ＋ Ctrl+Shift+M）。
    server.req("POST", "/keys/filer/bind", r#"["makeDirectoryDialog","Ctrl+Shift+M"]"#).unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    let s = keys();
    assert!(
        s.contains(r#"["makeDirectoryDialog",["Ctrl+Shift+M"]]"#)
            && s.contains(r#"["makeDirectoryDialog",["K"]]"#),
        "makeDirectoryDialog が 2 行に割れる: {s}"
    );

    // K の行（chord 昇順で index 0）を選んで削除＝K 行だけ消え、Ctrl+Shift+M 行が残る。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    let s = keys();
    assert!(s.contains(r#"["makeDirectoryDialog",["Ctrl+Shift+M"]]"#), "Ctrl+Shift+M 行が残る: {s}");
    assert!(!s.contains(r#"["makeDirectoryDialog",["K"]]"#), "K 行は消える: {s}");

    // 残った行も削除＝未割当の空行に戻る。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    assert!(keys().contains(r#"["makeDirectoryDialog",[]]"#), "未割当の空行に戻る: {}", keys());

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 機能順でキーを「変更（リマップ）」：選択した行のキーを新しいキーへ移し替える（旧キーは外れる・
/// 呼び出しは同じ）。実機ではキー行のダブルクリック→打鍵に対応する経路。
#[test]
fn settings_key_editor_rebinds_selected_chord() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // makeDirectoryDialog に 2 つ目のキーを足す＝2 行に割れる（chord 昇順 [K, Ctrl+Shift+M]）。
    server.req("POST", "/keys/filer/bind", r#"["makeDirectoryDialog","Ctrl+Shift+M"]"#).unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    let s = keys();
    assert!(s.contains(r#"["makeDirectoryDialog",["K"]]"#), "K 行がある: {s}");

    // K の行（index 0）を選んで Ctrl+Alt+K へ変更＝K は外れ Ctrl+Alt+K になる（Ctrl+Shift+M は残る）。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/rebind", "Ctrl+Alt+K").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    let s = keys();
    assert!(s.contains(r#"["makeDirectoryDialog",["Ctrl+Alt+K"]]"#), "K が Ctrl+Alt+K に移る: {s}");
    assert!(s.contains(r#"["makeDirectoryDialog",["Ctrl+Shift+M"]]"#), "Ctrl+Shift+M は残る: {s}");
    assert!(!s.contains(r#"["makeDirectoryDialog",["K"]]"#), "K 行は無い: {s}");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 登録済みスクリプトが機能順の「スクリプト」ジャンルに未割当行で出て、選んでキャプチャすると
/// `script("name")` がキーへ割り当たる（debug の capture＝begin_capture→打鍵の経路）。
#[test]
fn settings_key_editor_binds_registered_script() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[("00-cmds.ts", r#"rerics.registerCommand("myScript", () => {});"#)],
    );
    // エンジンが登録を終えてから設定を開く（open_settings がその一覧を編集器へ渡す）。
    poll(&server, "/script/commands", |b| b.contains("myScript"));
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 登録スクリプトが未割当行（myScript・キー無し）として出る。名前で絞れる。
    server.req("POST", "/keys/filer/search", "myScript").unwrap();
    assert!(keys().contains(r#"["myScript",[]]"#), "未割当のスクリプト行が出る: {}", keys());

    // 行を選んでキャプチャ＝myScript() が Ctrl+Alt+S に割り当たる。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/capture", "Ctrl+Alt+S").unwrap();
    server.req("POST", "/keys/filer/search", "myScript").unwrap();
    assert!(
        keys().contains(r#"["myScript",["Ctrl+Alt+S"]]"#),
        "myScript が Ctrl+Alt+S に割り当たる: {}",
        keys()
    );

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 「式を編集」で行の式をコード（複文・ホスト API 呼び）へ書き替えると、コードの未割当（－）行が
/// 生え、通常どおりその行を選んでキャプチャするとキーへ結ばれる。式そのものが機能名・実呼び出しになる。
#[test]
fn settings_key_editor_binds_code() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 未割当の行（selectMaskDialog）を選んで式をコードへ書き替える＝コードの未割当行が生える
    //（前後スペースは trim される）。bare の selectMaskDialog 行は残る。
    server.req("POST", "/keys/filer/search", "selectMaskDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/expr", "  r.log(42)  ").unwrap();
    server.req("POST", "/keys/filer/search", "r.log").unwrap();
    assert!(keys().contains(r#"["r.log(42)",[]]"#), "未割当のコード行が生える: {}", keys());

    // その行を選んでキャプチャ＝r.log(42) が Ctrl+Alt+G に割り当たる。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/capture", "Ctrl+Alt+G").unwrap();
    server.req("POST", "/keys/filer/search", "r.log").unwrap();
    assert!(
        keys().contains(r#"["r.log(42)",["Ctrl+Alt+G"]]"#),
        "コードが Ctrl+Alt+G に割り当たる: {}",
        keys()
    );

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 「式を編集」＝バインド済みの組込コマンド行で式を書き替えると、そのキーの呼び出しがその場で
/// 差し替わる。既定 J の `jumpDialog()` に式引数を付け、OK で config.toml に残る。
#[test]
fn settings_key_editor_edits_bound_command_arg() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 既定で J = jumpDialog()。jumpDialog で絞って J の行を選ぶ。
    server.req("POST", "/keys/filer/search", "jumpDialog").unwrap();
    assert!(keys().contains(r#"["jumpDialog",["J"]]"#), "J の jumpDialog 行: {}", keys());
    server.req("POST", "/keys/filer/select/0", "").unwrap();

    // 式をリテラル引数つきへ差し替える（バインド済み＝そのキーの呼び出しをその場で置換）。
    server.req("POST", "/keys/filer/expr", r#"jumpDialog("D:")"#).unwrap();

    // OK で確定＝config.toml の J が新しい式へ更新される。
    server.req("POST", "/modal/command/ok", "").expect("ok");
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let cfg = std::fs::read_to_string(server.base.join("data").join("config.toml")).unwrap();
    assert!(
        cfg.contains(r#"jumpDialog("D:")"#),
        "J の引数が式へ差し替わって保存される: {cfg}"
    );
}

/// 「式を編集」＝未割当の組込コマンド行へ引数つきの式を付けると、引数つきの未割当（－）行が生え、
/// その行をキャプチャしてキーへ結べる。OK で `selectMaskDialog("=式")` が当該キーに残る。
#[test]
fn settings_key_editor_attaches_arg_to_unbound_command() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 既定で未バインドの selectMaskDialog（bare 行）を選ぶ。
    server.req("POST", "/keys/filer/search", "selectMaskDialog").unwrap();
    assert!(keys().contains(r#"["selectMaskDialog",[]]"#), "未割当の selectMask 行: {}", keys());
    server.req("POST", "/keys/filer/select/0", "").unwrap();

    // 引数つきの式へ書き替える＝引数つきの未割当行が生え、その行が選択される（apply_expr が選択する）。
    server.req("POST", "/keys/filer/expr", r#"selectMaskDialog("*.txt")"#).unwrap();
    // 選択中のその行をキャプチャ＝selectMaskDialog("*.txt") が Ctrl+Alt+J に割り当たる。
    server.req("POST", "/keys/filer/capture", "Ctrl+Alt+J").unwrap();

    // OK で確定＝config.toml の Ctrl+Alt+J に引数つき呼び出しが残る。
    server.req("POST", "/modal/command/ok", "").expect("ok");
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let cfg = std::fs::read_to_string(server.base.join("data").join("config.toml")).unwrap();
    assert!(
        cfg.contains(r#"selectMaskDialog("*.txt")"#),
        "引数つき呼び出しがキーに割り当たって保存される: {cfg}"
    );
}

/// 「式を編集」で作った未割当の引数つき行は、「キー定義を削除」でその定義ごと消せる（bare 行は残る）。
#[test]
fn settings_key_editor_deletes_unbound_arg_definition() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;
    let count = || keys().matches(r#"["selectMaskDialog",[]]"#).count();

    // 未バインドの selectMaskDialog（bare 行）を選んで引数つきの式へ書き替える＝引数つきの未割当行が増える。
    server.req("POST", "/keys/filer/search", "selectMaskDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/expr", r#"selectMaskDialog("*.txt")"#).unwrap();
    server.req("POST", "/keys/filer/search", "selectMaskDialog").unwrap();
    assert_eq!(count(), 2, "bare と引数つきで selectMaskDialog 行が2つ: {}", keys());

    // 引数つき行（bare の次＝index 1）を選んで「キー定義を削除」＝その定義が消えて bare だけ残る。
    server.req("POST", "/keys/filer/select/1", "").unwrap();
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    server.req("POST", "/keys/filer/search", "selectMaskDialog").unwrap();
    assert_eq!(count(), 1, "引数つきの定義が消えて bare だけ残る: {}", keys());
}

/// キー順でも「式を編集」（set_expr）で選択キーの機能欄の式を差し替えられる。キー順のまま反映され、
/// そのキーの機能が変わる（機能順専用だった式編集をキー順へ拡張）。
#[test]
fn settings_key_editor_by_key_edits_expression() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 未使用キー Ctrl+Shift+Q を selectMaskDialog に割り当て、キー順でその行を選ぶ。
    server.req("POST", "/keys/filer/bind", r#"["selectMaskDialog","Ctrl+Shift+Q"]"#).unwrap();
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();
    assert!(keys().contains(r#"["Ctrl+Shift+Q",["selectMaskDialog"]]"#), "対象キー行: {}", keys());
    server.req("POST", "/keys/filer/select/0", "").unwrap();

    // キー順のまま式を編集＝そのキーの機能が makeDirectoryDialog へ差し替わる。
    server.req("POST", "/keys/filer/expr", "makeDirectoryDialog()").unwrap();
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();
    let s = keys();
    assert!(s.contains(r#"["Ctrl+Shift+Q",["makeDirectoryDialog"]]"#), "キー順で式を編集して機能が変わる: {s}");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// キー順で列幅に収まらず切り詰められたセルは、hover ツールチップで全文を返す（debug は
/// `/keys/<cat>/tooltip/<row>/<col>` で観測）。短いセル・機能順ビュー・設定が閉じている時は出さない。
#[test]
fn settings_key_editor_truncated_cell_shows_tooltip() {
    let server = Server::start(&["a.txt"], "");
    // 設定が開いていなければ 404。
    assert_eq!(
        server.req("GET", "/keys/filer/tooltip/0/2", "").expect("closed").0,
        404,
        "設定が開いていなければ 404"
    );
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);

    // 未使用キーへ割り当て、キー順でその行を選び、列に収まらない長い式を入れる。
    server.req("POST", "/keys/filer/bind", r#"["selectMaskDialog","Ctrl+Shift+Q"]"#).unwrap();
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    let long = "{ aLongFunctionNumberOne(); aLongFunctionNumberTwo(); aLongFunctionNumberThree() }";
    server.req("POST", "/keys/filer/expr", long).unwrap();
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();

    // 実呼び出し列（col 2）は切り詰められる＝全文が返る。
    let (code, body) = server.req("GET", "/keys/filer/tooltip/0/2", "").expect("tooltip call");
    assert_eq!(code, 200, "切り詰めセルは 200: {body}");
    assert!(body.contains("aLongFunctionNumberThree"), "全文が返る: {body}");

    // キー列（col 0）は短いので切り詰め無し＝空。
    let body = server.req("GET", "/keys/filer/tooltip/0/0", "").expect("tooltip chord").1;
    assert!(body.contains(r#""text":"""#), "短いセルは空: {body}");

    // 機能順ビューはキー順専用の hover の対象外＝空。
    server.req("POST", "/keys/filer/view", "command").unwrap();
    let body = server.req("GET", "/keys/filer/tooltip/0/2", "").expect("tooltip cmdview").1;
    assert!(body.contains(r#""text":"""#), "機能順では空: {body}");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 切り詰めセルへ実際に hover した時の表示経路（resolver→ツールチップ生成→表示）が動く＝生成成功・
/// `WS_VISIBLE`・全文が返る（`/keys/<cat>/hover/<row>/<col>`）。切り詰め無しのセルは表示しない。
#[test]
fn settings_key_editor_hover_shows_tooltip_window() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);

    server.req("POST", "/keys/filer/bind", r#"["selectMaskDialog","Ctrl+Shift+Q"]"#).unwrap();
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    let long = "{ aLongFunctionNumberOne(); aLongFunctionNumberTwo(); aLongFunctionNumberThree() }";
    server.req("POST", "/keys/filer/expr", long).unwrap();
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();

    // 実呼び出し列（col 2）へ hover＝ツールチップ窓が作られ、表示状態になり、全文が出る。
    let (code, body) = server.req("GET", "/keys/filer/hover/0/2", "").expect("hover call");
    assert_eq!(code, 200, "hover は 200: {body}");
    assert!(body.contains(r#""created":true"#), "ツールチップ窓が作られる: {body}");
    assert!(body.contains(r#""visible":true"#), "表示状態になる: {body}");
    assert!(body.contains("aLongFunctionNumberThree"), "全文が出る: {body}");

    // キー列（col 0）は短い＝切り詰め無しなので表示しない。
    let body = server.req("GET", "/keys/filer/hover/0/0", "").expect("hover chord").1;
    assert!(body.contains(r#""visible":false"#), "短いセルは表示しない: {body}");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 機能順ビューでも同じ部品で切り詰めセルの全文を hover 表示する（col は左から機能名/実呼び出し/
/// キー）。全文取得（tooltip）と実表示経路（hover）の両方を検証する。
#[test]
fn settings_key_editor_command_view_shows_tooltip() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);

    // 既定の機能順のまま、未使用キーを割り当てて選択行へ列に収まらない長い式を入れる。
    server.req("POST", "/keys/filer/bind", r#"["selectMaskDialog","Ctrl+Shift+Q"]"#).unwrap();
    let long = "{ aLongFunctionNumberOne(); aLongFunctionNumberTwo(); aLongFunctionNumberThree() }";
    server.req("POST", "/keys/filer/expr", long).unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();

    // 実呼び出し列（機能順では col 1）は切り詰め＝全文が返る。
    let (code, body) = server.req("GET", "/keys/filer/tooltip/0/1", "").expect("tooltip");
    assert_eq!(code, 200, "tooltip 200: {body}");
    assert!(body.contains("aLongFunctionNumberThree"), "全文が返る: {body}");

    // 実 hover でツールチップ窓が作られて表示状態になる。
    let body = server.req("GET", "/keys/filer/hover/0/1", "").expect("hover").1;
    assert!(body.contains(r#""created":true"#), "ツールチップ窓が作られる: {body}");
    assert!(body.contains(r#""visible":true"#), "表示状態になる: {body}");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// ファイル一覧でも同じ部品で、列幅に収まらない長いセルの全文を hover 表示する（全文取得と
/// 実表示経路の両方）。列の自動調整を切って名前列を固定幅にし、長い名前を確実に切り詰めさせる。
#[test]
fn file_list_truncated_cell_shows_tooltip() {
    let long = "this_is_an_extremely_long_file_name_that_will_not_fit_in_the_name_column_aaaa.txt";
    let server = Server::start(&[long], "auto_adjust_columns = false\n");

    // 左ペイン row 1（row 0 は「..」）の名前列（col 0）が切り詰め＝全文が返る。
    let (code, body) = server.req("GET", "/list/left/tooltip/1/0", "").expect("tooltip");
    assert_eq!(code, 200, "tooltip 200: {body}");
    assert!(body.contains("extremely_long_file_name"), "全文が返る: {body}");

    // 実 hover でツールチップ窓が作られて表示状態になる。
    let body = server.req("GET", "/list/left/hover/1/0", "").expect("hover").1;
    assert!(body.contains(r#""created":true"#), "ツールチップ窓が作られる: {body}");
    assert!(body.contains(r#""visible":true"#), "表示状態になる: {body}");

    // サイズ列など短いセルは切り詰め無し＝出さない。
    let body = server.req("GET", "/list/left/hover/1/2", "").expect("hover size").1;
    assert!(body.contains(r#""visible":false"#), "短いセルは表示しない: {body}");
}

/// ログウィンドウでも同じ部品で、幅に収まらない行の全文を hover 表示する。窓を細くして
/// ログ幅を狭め、作成ログ行が確実に切り詰められるようにする。
#[test]
fn log_view_truncated_line_shows_tooltip() {
    let cfg = "[window]\nfixed_size = true\nwidth = 360\nheight = 400\n";
    let server = Server::start_writable_cfg(&["a.txt"], cfg);
    // ログを空にしてから 1 行だけ作る＝作成ログが row 0 に来る。
    server.req("POST", "/command/clearLog", "").unwrap();
    server.req("POST", "/command/makeDirectoryDialog", "").unwrap();
    wait_modal(&server);
    let long = "a_directory_with_a_fairly_long_name_for_logging_xx";
    server.req("POST", "/modal/text", long).unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/log", |b| b.contains("CreateDirectory"));

    // 細い窓ではログ行が切り詰め＝全文（作成名込み）が返る。
    let (code, body) = server.req("GET", "/log/tooltip/0", "").expect("tooltip");
    assert_eq!(code, 200, "tooltip 200: {body}");
    assert!(body.contains("a_directory_with_a_fairly_long_name"), "全文が返る: {body}");

    // 実 hover でツールチップ窓が作られて表示状態になる。
    let body = server.req("GET", "/log/hover/0", "").expect("hover").1;
    assert!(body.contains(r#""created":true"#), "ツールチップ窓が作られる: {body}");
    assert!(body.contains(r#""visible":true"#), "表示状態になる: {body}");
}

/// 長い一覧をスクロールできる（先頭行が動く・選択は不変・範囲外はクランプ）。ホイール／
/// スクロールバーと同じ scroll 経路を headless から叩く。
#[test]
fn settings_key_editor_scrolls_long_list() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 既定は先頭・選択 0。Filer は全コマンドが並ぶので 1 画面に収まらない。
    let s = keys();
    assert!(s.contains(r#""top":0"#), "初期は先頭: {s}");

    // 下へスクロール＝先頭行が動く・選択は不変。
    server.req("POST", "/keys/filer/scroll/5", "").unwrap();
    let s = keys();
    assert!(s.contains(r#""top":5"#), "先頭行が 5 へ: {s}");
    assert!(s.contains(r#""selected":0"#), "スクロールで選択は動かない: {s}");

    // 範囲外は末尾へクランプ（巨大値でも top は範囲内＝0 でも 100000 でもない）。
    server.req("POST", "/keys/filer/scroll/100000", "").unwrap();
    let s = keys();
    assert!(!s.contains(r#""top":100000"#), "範囲外はクランプ: {s}");
    assert!(!s.contains(r#""top":0"#), "末尾近くまでは進む: {s}");

    // 先頭へ戻す。
    server.req("POST", "/keys/filer/scroll/0", "").unwrap();
    assert!(keys().contains(r#""top":0"#), "先頭へ戻る");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// キー順で「キー定義を追加」＝空キー定義（機能未割当・－）を作り、後から「式を編集」（set_expr）で
/// 機能を割り当てられる（空式は新規割り当てとして draft へ足される）。
#[test]
fn settings_key_editor_add_empty_key_def_then_assign() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;
    server.req("POST", "/keys/filer/view", "key").unwrap();

    // 未使用キーの空キー定義を作る＝labels が空の行（機能未割当）。
    server.req("POST", "/keys/filer/addkeydef", "Ctrl+Shift+Z").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Z").unwrap();
    assert!(keys().contains(r#"["Ctrl+Shift+Z",[]]"#), "空キー定義の行: {}", keys());

    // その行を選び、「式を編集」で makeDirectoryDialog を割り当てる（空キー定義への新規割り当て）。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/expr", "makeDirectoryDialog()").unwrap();

    // 割り当て後：Ctrl+Shift+Z → makeDirectoryDialog（空キー定義が解消）。
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Z").unwrap();
    assert!(
        keys().contains(r#"["Ctrl+Shift+Z",["makeDirectoryDialog"]]"#),
        "空キー定義に機能が付く: {}",
        keys()
    );

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// キー編集ページの検索（機能名・キーへの部分一致・大小無視）。クエリで一覧が絞り込まれ、
/// 空クエリで全件へ戻る。`config` は変わらない（割り当ては不変）。
#[test]
fn settings_key_editor_search_filters_by_name_and_key() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;
    // 行数＝JSON 配列 `[...]` の個数（rows の各行が `["Cmd",[...]]`）。`],[` の数＋1。
    let count = |s: &str| s.matches("],[").count() + if s.contains("\"rows\":[]") { 0 } else { 1 };

    let full = keys();
    let full_n = count(&full);
    assert!(full_n > 40, "既定は Filer 全コマンドが並ぶ: {full_n}");
    assert!(full.contains(r#""query":"""#), "初期クエリは空: {full}");

    // 機能名で絞り込む："copy" は copy/clipCopy/viewerCopy… を含み、makeDirectoryDialog は除外。
    assert_eq!(
        server.req("POST", "/keys/filer/search", "copy").unwrap().0,
        200,
        "search は ok"
    );
    let s = keys();
    assert!(s.contains(r#""query":"copy""#), "クエリが反映される: {s}");
    assert!(s.contains(r#"["copy",["C"]]"#), "copy が残る: {s}");
    assert!(s.contains("clipCopy"), "clipCopy が残る: {s}");
    assert!(!s.contains("makeDirectoryDialog"), "無関係な機能は消える: {s}");
    let copy_n = count(&s);
    assert!(copy_n > 0 && copy_n < full_n, "件数が減る: {copy_n} < {full_n}");

    // 日本語の表示名でも絞り込める："コピー" は copy（表示名「コピー」）/clipCopy
    //（「クリップボードにコピー」）に一致し、makeDirectoryDialog（「フォルダ作成」）は除外。
    server.req("POST", "/keys/filer/search", "コピー").unwrap();
    let s = keys();
    assert!(s.contains(r#"["copy",["C"]]"#), "表示名検索で copy が残る: {s}");
    assert!(s.contains("clipCopy"), "表示名検索で clipCopy が残る: {s}");
    assert!(!s.contains("makeDirectoryDialog"), "表示名検索で無関係な機能は消える: {s}");

    // キーで絞り込む：既定 K は makeDirectoryDialog のみ（大小無視なので chord "K" に一致）。
    server.req("POST", "/keys/filer/search", "K").unwrap();
    let s = keys();
    assert!(s.contains("makeDirectoryDialog"), "K を持つ makeDirectory が出る: {s}");

    // 空クエリで全件へ戻る。
    server.req("POST", "/keys/filer/search", "").unwrap();
    let s = keys();
    assert_eq!(count(&s), full_n, "空クエリで全件に戻る: {s}");
    assert!(s.contains(r#""query":"""#), "クエリが空に戻る");

    // 絞り込みは config を変えない（割り当ては不変）＝copy=C のまま。
    assert!(keys().contains(r#"["copy",["C"]]"#), "割り当ては検索で変わらない");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// キー編集ページの機能順／キー順ビュー切替。キー順では行が「キー→機能」になり、検索も効く。
/// キー順の削除は選択中の 1 キーだけを外す（同じ機能の別キーは残る）。
#[test]
fn settings_key_editor_toggles_command_and_key_views() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 既定は機能順：行は (機能, [キー…])。makeDirectoryDialog=K。
    let s = keys();
    assert!(s.contains(r#""mode":"command""#), "初期は機能順: {s}");
    assert!(s.contains(r#"["makeDirectoryDialog",["K"]]"#), "機能順 makeDirectory=K: {s}");

    // キー順へ切替：行は (キー, [機能])。K→makeDirectoryDialog。
    assert_eq!(server.req("POST", "/keys/filer/view", "key").unwrap().0, 200, "view 切替 ok");
    let s = keys();
    assert!(s.contains(r#""mode":"key""#), "キー順になる: {s}");
    assert!(s.contains(r#"["K",["makeDirectoryDialog"]]"#), "キー順 K→makeDirectory: {s}");

    // キー順でも検索が効く（キー・機能名どちらにも一致）。
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    assert!(keys().contains(r#"["K",["makeDirectoryDialog"]]"#), "キー順で機能名検索が効く");
    server.req("POST", "/keys/filer/search", "").unwrap();

    // 機能順へ戻して、同じ機能に 2 キーを割り当てる（1 キー=1 行なので 2 行に割れる）。
    server.req("POST", "/keys/filer/view", "command").unwrap();
    server.req("POST", "/keys/filer/bind", r#"["selectMaskDialog","Ctrl+Shift+M"]"#).unwrap();
    server.req("POST", "/keys/filer/bind", r#"["selectMaskDialog","Ctrl+Shift+N"]"#).unwrap();
    let s = keys();
    assert!(
        s.contains(r#"["selectMaskDialog",["Ctrl+Shift+M"]]"#)
            && s.contains(r#"["selectMaskDialog",["Ctrl+Shift+N"]]"#),
        "selectMaskDialog が 2 行に割れる: {s}"
    );

    // キー順で Ctrl+Shift+M の行だけを選び、削除＝その 1 キーだけ外れる。
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+M").unwrap();
    let s = keys();
    // 絞り込みで M の行だけ（N の行は出ない）。rows 配列を厳密に見る（status 文言の巻き込みを避ける）。
    assert!(
        s.contains(r#""rows":[["Ctrl+Shift+M",["selectMaskDialog"]]]"#),
        "M の行だけが出る: {s}"
    );
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/unbind", "").unwrap();

    // 機能順へ戻すと、selectMaskDialog は N だけ残る（M だけが外れた）。
    server.req("POST", "/keys/filer/search", "").unwrap();
    server.req("POST", "/keys/filer/view", "command").unwrap();
    assert!(
        keys().contains(r#"["selectMaskDialog",["Ctrl+Shift+N"]]"#),
        "M だけ外れ N が残る: {}",
        keys()
    );

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// `/modal/resize/<w>x<h>` がモーダルへ WM_SIZE を飛ばし、クライアント寸法が要求サイズへ
/// 変わる（手動ドラッグの代替＝リサイズ追従ダイアログを headless で検証する基盤）。
#[test]
fn modal_resize_endpoint_changes_client_size() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);

    // /snapshot/modal は PrintWindow でクライアント領域を撮るので、PNG の IHDR 幅高が
    // そのままモーダルのクライアント寸法になる。
    let client_dims = |port| -> (u32, u32) {
        let (st, png) = req_bytes(port, "GET", "/snapshot/modal").expect("snap");
        assert_eq!(st, 200, "/snapshot/modal は 200");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 署名");
        let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        (w, h)
    };

    let (w0, h0) = client_dims(server.port);
    let resp = server.req("POST", "/modal/resize/700x520", "").expect("resize");
    assert_eq!(resp.0, 200, "/modal/resize は 200");

    let (w1, h1) = client_dims(server.port);
    // 縮んだうえで、要求した窓サイズ(700x520)から枠/タイトルバーを引いた近傍に収まる。
    assert!(w1 < w0 && h1 < h0, "resize 後はクライアントが縮むはず: {w0}x{h0} -> {w1}x{h1}");
    assert!((620..=700).contains(&w1), "幅は要求(700)近傍のはず: {w1}");
    assert!((430..=520).contains(&h1), "高さは要求(520)近傍のはず: {h1}");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// list_box セレクタ（キー割り当て一覧）はサイズ変更枠付きで、リサイズすると wm_size が
/// 一覧とボタンを再配置する。大きくしても壊れず（パニックせず）、撮影・選択・クローズできる。
#[test]
fn keybind_selector_is_resizable() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/keyBindsDialog", "").expect("keyBindsDialog");
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"list\""), "リスト選択モーダルが開くはず: {modal}");

    let (st, _) = server.req("POST", "/modal/resize/960x740", "").expect("resize");
    assert_eq!(st, 200, "/modal/resize は 200");

    // リサイズ後も撮影でき（再レイアウトが走ってもクラッシュしない）、選択行を動かせる。
    let (sst, png) = req_bytes(server.port, "GET", "/snapshot/modal").expect("snap");
    assert_eq!(sst, 200, "リサイズ後も /snapshot/modal は 200");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 署名");
    server.req("POST", "/modal/select/3", "").expect("select");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// リサイズ可能ダイアログは前回サイズを無言で記憶する（dialog-sizes.toml）。リサイズして
/// 閉じ、再オープンすると保存サイズで開く。
#[test]
fn dialog_remembers_last_size() {
    let server = Server::start(&["a.txt"], "");
    let client_dims = |port| -> (u32, u32) {
        let (st, png) = req_bytes(port, "GET", "/snapshot/modal").expect("snap");
        assert_eq!(st, 200);
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        (
            u32::from_be_bytes([png[16], png[17], png[18], png[19]]),
            u32::from_be_bytes([png[20], png[21], png[22], png[23]]),
        )
    };

    server.req("POST", "/command/keyBindsDialog", "").expect("open1");
    wait_modal(&server);
    let (w0, h0) = client_dims(server.port);

    // 小さくリサイズして閉じる。
    server.req("POST", "/modal/resize/700x520", "").expect("resize");
    server.req("POST", "/modal/command/cancel", "").expect("cancel1");
    poll(&server, "/state/modal", |b| b.trim() == "null");

    // 再オープンは記憶した小さいサイズで開く（既定より小さく・要求700近傍）。
    server.req("POST", "/command/keyBindsDialog", "").expect("open2");
    wait_modal(&server);
    let (w1, h1) = client_dims(server.port);
    assert!(w1 < w0 && h1 < h0, "記憶サイズで開くはず: {w0}x{h0} -> {w1}x{h1}");
    assert!((620..=700).contains(&w1), "幅は記憶した要求(700)近傍: {w1}");

    server.req("POST", "/modal/command/cancel", "").expect("cancel2");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// ドライブ選択（ListView）もサイズ変更枠付き。リサイズで一覧/ボタンが再配置され、
/// 撮影・クローズできる（共有ヘルパ relayout_list_dialog の ListView 経路）。
#[test]
fn drive_dialog_is_resizable() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/changeDriveDialog", "").expect("changeDriveDialog");
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"drive\""), "ドライブ選択が開くはず: {modal}");

    let (st, _) = server.req("POST", "/modal/resize/980x760", "").expect("resize");
    assert_eq!(st, 200, "/modal/resize は 200");
    let (sst, png) = req_bytes(server.port, "GET", "/snapshot/modal").expect("snap");
    assert_eq!(sst, 200, "リサイズ後も /snapshot/modal は 200");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 署名");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// タスクマネージャ（左寄せアクション＋右「閉じる」）はサイズ変更枠付き。リサイズで一覧が
/// 広がり、左ボタンは下端へ・閉じるは右下へ寄り直す（個別 relayout 経路）。壊れず撮影・クローズ可。
#[test]
fn task_manager_is_resizable() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openTaskManager", "").expect("openTaskManager");
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"tasks\""), "タスクマネージャが開くはず: {modal}");

    let (st, _) = server.req("POST", "/modal/resize/980x760", "").expect("resize");
    assert_eq!(st, 200, "/modal/resize は 200");
    let (sst, png) = req_bytes(server.port, "GET", "/snapshot/modal").expect("snap");
    assert_eq!(sst, 200, "リサイズ後も /snapshot/modal は 200");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 署名");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// タスクマネージャは多列 ListView モーダル（走行中タスクの一覧）。タスクが無くても開き、
/// modal_registry 登録済みなので観測・撮影・クローズできる（デッドロックしない）。
#[test]
fn task_manager_dialog_opens_observes_and_closes() {
    let server = Server::start(&["a.txt"], "");

    server.req("POST", "/command/openTaskManager", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"tasks\""), "タスクマネージャが開くはず: {modal}");
    assert!(modal.contains("\"headers\":[\"タスク\""), "列ヘッダが見えるはず: {modal}");
    assert!(modal.contains("\"label\":\"中止(&A)\""), "中止ボタンが登録されているはず: {modal}");

    // 登録モーダルなので PNG として撮れる。
    let (st, png) = req_bytes(server.port, "GET", "/snapshot/modal").expect("snapshot/modal");
    assert_eq!(st, 200, "/snapshot/modal は 200");
    assert!(
        png.starts_with(&[0x89, b'P', b'N', b'G']),
        "PNG 署名で始まるはず ({} bytes)",
        png.len()
    );

    // 閉じる（ブロックしない）。
    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// タスク制御：中止まで回り続けるダミータスク（`/debug/spawn-task`）を起こし、タスク
/// マネージャ越しに中断→再開→中止を確定的に検証する。検索・比較も同じ `TaskControl` /
/// `search_cancelled` に乗るので、ここでタスク制御機構そのものを担保する。
#[test]
fn task_control_suspend_resume_stop_via_task_manager() {
    let server = Server::start(&["a.txt"], "");
    // 中止されるまで終わらないタスクを起こす（待ち窓が無く確定的）。
    server.req("POST", "/debug/spawn-task", "").expect("spawn-task");

    server.req("POST", "/command/openTaskManager", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"tasks\""), "タスクマネージャが開く: {modal}");

    // 行が出るまで待つ（登録は別スレッド→タイマ取り込みのため）。最新ボタンで取り込む。
    let row_state = |s: &Server| -> Option<String> {
        let b = s.req("GET", "/state/modal", "")?.1;
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        let rows = v["rows"].as_array()?;
        rows.first()?.as_array()?.get(2)?.as_str().map(str::to_string)
    };
    poll(&server, "/state/modal", |b| {
        serde_json::from_str::<serde_json::Value>(b)
            .ok()
            .and_then(|v| v["rows"].as_array().map(|r| !r.is_empty()))
            .unwrap_or(false)
    });
    assert_eq!(row_state(&server).as_deref(), Some("実行中"), "初期は実行中");

    // 行を選んで中断（ctrl_id 101）→「中断」。状態は populate が control から読むので即時。
    server.req("POST", "/modal/select/0", "").unwrap();
    server.req("POST", "/modal/command/101", "").unwrap();
    assert_eq!(row_state(&server).as_deref(), Some("中断"), "中断へ");

    // 再開（102）→「実行中」。
    server.req("POST", "/modal/command/102", "").unwrap();
    assert_eq!(row_state(&server).as_deref(), Some("実行中"), "実行中へ戻る");

    // 中止（100）→ ワーカーが次の境界で気付いて完了し、タスク登録が消える。
    // 「最新」（103）で取り込みつつ行が消えるまで待つ。
    server.req("POST", "/modal/command/100", "").unwrap();
    let mut emptied = false;
    for _ in 0..50 {
        server.req("POST", "/modal/command/103", "").unwrap();
        let b = server.req("GET", "/state/modal", "").unwrap().1;
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        if v["rows"].as_array().map(|r| r.is_empty()).unwrap_or(false) {
            emptied = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(emptied, "中止でワーカーが終了し、タスク行が消える");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// ワーカー操作の進行表示（ぐるぐる）は、走行中にスクリプトが実行・終了しても
/// 巻き添えで止まらない（終了時の stopProgress 保険はスクリプトが始めた進行表示のみ回収）。
#[test]
fn worker_progress_survives_script_end() {
    let server = Server::start(&["a.txt"], "");
    // 中止まで回り続けるタスクが、実操作と同じ流儀の進捗行（進行表示つき）を開く。
    server.req("POST", "/debug/spawn-task", "").expect("spawn-task");
    poll(&server, "/state/log", |b| {
        b.contains("テスト用タスク実行中") && !b.contains("\"progress\":[]")
    });

    // スクリプトを1本実行して終了させる（ログ行の出現で終了近傍まで待つ）。
    server.req("POST", "/script/eval", r#"rerics.log("script-ran");"#).unwrap();
    poll(&server, "/state/log", |b| b.contains("script-ran"));
    // ScriptEnd の保険が走ったあとも、ワーカーの進行表示は回ったまま。
    std::thread::sleep(Duration::from_millis(300));
    let log = server.req("GET", "/state/log", "").unwrap().1;
    assert!(!log.contains("\"progress\":[]"), "worker spinner survives script end: {log}");
}

/// スクリプトのタスク化：暴走スクリプト（無限ループ）がスクリプトタスクとして出て、中断は
/// 無反応（V8 制約・実行中のまま）、中止で isolate が強制終了され消える。停止後もエンジンは
/// 復帰して次の評価に応える。
#[test]
fn script_task_stop_terminates_runaway_via_task_manager() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    // 暴走スクリプトを投げる（投げっぱなしで即返り、エンジンスレッドは無限ループに入る）。
    server.req("POST", "/script/eval", "while(true){}").unwrap();

    server.req("POST", "/command/openTaskManager", "").unwrap();
    wait_modal(&server);

    // スクリプトタスク行の状態（列0="スクリプト"・列2=状態）を取る。
    let script_state = |s: &Server| -> Option<String> {
        let b = s.req("GET", "/state/modal", "")?.1;
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        let rows = v["rows"].as_array()?;
        let row = rows.iter().find(|r| {
            r.as_array()
                .and_then(|c| c.first())
                .and_then(|x| x.as_str())
                == Some("スクリプト")
        })?;
        row.as_array()?.get(2)?.as_str().map(str::to_string)
    };

    // 「最新」で取り込みつつ、スクリプトタスクが「実行中」で出るまで待つ。
    let mut appeared = false;
    for _ in 0..50 {
        server.req("POST", "/modal/command/103", "").unwrap();
        if script_state(&server).as_deref() == Some("実行中") {
            appeared = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(appeared, "暴走スクリプトがスクリプトタスクとして出る");

    // 中断（101）は無反応＝状態は「実行中」のまま（種別で無効化）。
    server.req("POST", "/modal/select/0", "").unwrap();
    server.req("POST", "/modal/command/101", "").unwrap();
    assert_eq!(script_state(&server).as_deref(), Some("実行中"), "スクリプトは中断できない");

    // 中止（100）→ pump が isolate を terminate → エンジンが巻き戻り行が消える。
    server.req("POST", "/modal/command/100", "").unwrap();
    let mut gone = false;
    for _ in 0..60 {
        server.req("POST", "/modal/command/103", "").unwrap();
        if script_state(&server).is_none() {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(gone, "中止でスクリプトが強制終了され、タスク行が消える");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");

    // 停止後もエンジンは復帰している＝同期評価に応える。
    let (_, body) = server.req("POST", "/script/eval-value", "1+1").expect("eval-value");
    assert!(body.contains('2'), "停止後もエンジンが評価できる: {body}");
}

/// 並列ワーカーの停止：暴走ワーカー（無限ループ）はメインを止めるだけでは回り続けるので、
/// 停止時にワーカーのアイソレートも terminate する。生存ワーカー数（`/state/script/workers`）が
/// 起動で 1 になり、中止で 0 に戻ることで、ワーカーが実際に止まって登録解除されたことを見る。
#[test]
fn script_stop_terminates_runaway_parallel_worker() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    let workers = |s: &Server| -> u64 {
        s.req("GET", "/state/script/workers", "")
            .and_then(|(_, b)| b.trim().parse().ok())
            .unwrap_or(0)
    };

    // メインが await でブロックする形で、無限ループのワーカーを 1 つ起動する。
    server
        .req("POST", "/script/eval", "(async () => { await rerics.parallel(() => { while (true) {} }); })();")
        .unwrap();

    // ワーカーが登録されるまで待つ（生存ワーカー数 = 1）。
    let mut started = false;
    for _ in 0..60 {
        if workers(&server) == 1 {
            started = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(started, "並列ワーカーが起動して登録される");

    // タスクマネージャからスクリプトを中止する。
    server.req("POST", "/command/openTaskManager", "").unwrap();
    wait_modal(&server);
    let mut appeared = false;
    for _ in 0..50 {
        server.req("POST", "/modal/command/103", "").unwrap();
        let b = server.req("GET", "/state/modal", "").unwrap().1;
        if b.contains("スクリプト") {
            appeared = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(appeared, "スクリプトタスクが出る");
    server.req("POST", "/modal/select/0", "").unwrap();
    server.req("POST", "/modal/command/100", "").unwrap();

    // 中止でワーカーの isolate が terminate され、登録解除されて 0 へ戻る。
    let mut stopped = false;
    for _ in 0..80 {
        server.req("POST", "/modal/command/103", "").unwrap();
        if workers(&server) == 0 {
            stopped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(stopped, "中止で暴走ワーカーが止まり、生存ワーカー数が 0 へ戻る");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 無圧縮 24bpp の 1x1 BMP（白画素）。デコード可能な実画像が要るテスト用の最小 fixture。
const TINY_BMP: &[u8] = &[
    0x42, 0x4D, 0x3A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, // file header
    0x28, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // info header
    0xFF, 0xFF, 0xFF, 0x00, // 1px white + padding
];

/// 画像ビューアの表示モードキー（1=原寸/2=全体/3=幅/4=高/5=大）が、それぞれの
/// モードへ切り替わるのを debug-server で観測する。0 は原作に無い＝未バインドで不変。
#[test]
fn image_viewer_display_modes_switch_by_keys() {
    let server = Server::start(&["pic.bmp"], "");
    // 既定 placeholder("x") はデコードできずテキストへ退避するので、実デコード可能な画像へ差し替える。
    std::fs::write(server.base.join("sbx").join("pic.bmp"), TINY_BMP).unwrap();

    // 左 items は [.., pic.bmp]。cursorDown×1 で pic.bmp にカーソルを置いて開く。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/viewFile", "").expect("viewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "media");

    let mode = |s: &Server| {
        s.req("GET", "/state/media/mode", "")
            .expect("media mode")
            .1
            .trim()
            .trim_matches('"')
            .to_string()
    };

    // 既定は全体表示（Stretch）。
    assert_eq!(mode(&server), "fit", "初期モードは全体表示");

    // 各キーで対応モードへ切り替わる。
    for (key, want) in
        [("1", "actual"), ("2", "fit"), ("3", "fit_width"), ("4", "fit_height"), ("5", "fit_large")]
    {
        server.req("POST", &format!("/view/key/{key}"), "").expect("view key");
        assert_eq!(mode(&server), want, "キー {key} で {want} になるはず");
    }

    // 0 は画像ビューアでは未バインド＝直前の fit_large のまま変わらない。
    server.req("POST", "/view/key/0", "").expect("view key 0");
    assert_eq!(mode(&server), "fit_large", "0 は未バインドでモード不変");
}

/// メディア拡張子でも中身がデコードできなければテキスト/バイナリビューアへ退避する。
/// TypeScript の `.ts` は動画拡張子（MPEG-TS）と衝突するが、テキストとして開ける。
#[test]
fn view_falls_back_to_text_for_undecodable_media() {
    let server = Server::start(&["app.ts"], "");
    // `.ts` は MediaKind::Video に分類されるが、中身は TypeScript テキスト＝動画デコードは失敗する。
    std::fs::write(
        server.base.join("sbx").join("app.ts"),
        "export const greet = (name: string): string => `hi ${name}`;\n",
    )
    .unwrap();

    // app.ts にカーソルを置いて開く＝メディアにならずテキストビューアが前面に出る。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/viewFile", "").expect("viewFile");
    let av = poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    assert_eq!(av.trim().trim_matches('"'), "text", "デコード不可の .ts はテキストへ退避: {av}");
}

/// テキストビューアの検索が、可視範囲の全一致を桁単位で捉え、N で一致箇所単位に
/// 移動するのを debug-server で観測する（大小無視・同一行内の複数一致も辿る）。
#[test]
fn text_viewer_search_finds_all_occurrences_and_navigates() {
    let server = Server::start(&["doc.txt"], "");
    // 既定の placeholder を、複数一致を含む内容で上書きする（viewFile は表示時に読み直す）。
    std::fs::write(server.base.join("sbx").join("doc.txt"), "foo bar foo\nbaz\nFOO end\n").unwrap();

    // doc.txt にカーソルを置いてテキストビューアで開く。
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/viewFile", "").expect("viewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    // 一度撮影してレイアウト＋描画を走らせ、表示行を実幅で確定させる（検索は表示行を走査する）。
    req_bytes(server.port, "GET", "/snapshot").expect("warmup snapshot");

    // インライン検索バーを開いて "foo" を打ち込む（インクリメンタル検索が即時に走る）。
    server.req("POST", "/command/viewerSearchDialog", "").expect("open search");
    poll(&server, "/state/viewer/search_open", |b| b.trim() == "true");
    server.req("POST", "/view/search", "foo").expect("type foo");

    let match_json = |s: &Server| s.req("GET", "/state/viewer/match", "").expect("match").1;

    // 大小無視で全一致を数える（foo, foo, FOO ＝ 3）。
    let count = server.req("GET", "/state/viewer/match_count", "").expect("count").1;
    assert_eq!(count.trim(), "3", "大小無視で全一致を数える");
    // 初期一致は 0 行 0 桁・長さ 3。
    let m1 = match_json(&server);
    assert!(
        m1.contains("\"line\":0") && m1.contains("\"col\":0") && m1.contains("\"len\":3"),
        "初期一致は 0 行 0 桁: {m1}"
    );

    // ↓ で同一行内の次の一致（8 桁目）へ。
    server.req("POST", "/view/search/key/down", "").expect("down");
    let m2 = match_json(&server);
    assert!(m2.contains("\"col\":8"), "次の一致は同一行 8 桁: {m2}");

    // さらに ↓ で 3 行目の FOO（別行・0 桁）へ。
    server.req("POST", "/view/search/key/down", "").expect("down 2");
    let m3 = match_json(&server);
    assert!(m3.contains("\"line\":2") && m3.contains("\"col\":0"), "3 番目は 2 行目 0 桁: {m3}");

    // ↑ で 8 桁目の一致へ戻る。
    server.req("POST", "/view/search/key/up", "").expect("up");
    let m4 = match_json(&server);
    assert!(m4.contains("\"col\":8"), "↑ で前の一致 8 桁へ戻る: {m4}");

    // ハイライト描画を撮れる（観測可能）。
    let (st, png) = req_bytes(server.port, "GET", "/snapshot").expect("snapshot");
    assert_eq!(st, 200, "/snapshot は 200");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 署名で始まる");

    // Enter で確定するとバーは閉じるが検索語・ハイライトは残る。
    server.req("POST", "/view/search/key/enter", "").expect("enter");
    poll(&server, "/state/viewer/search_open", |b| b.trim() == "false");
    let count2 = server.req("GET", "/state/viewer/match_count", "").expect("count2").1;
    assert_eq!(count2.trim(), "3", "Enter 確定後も検索語は残る");

    // 再度開いて Esc で閉じても、検索語とハイライトは残る（開始位置へ戻るだけ）。
    server.req("POST", "/command/viewerSearchDialog", "").expect("reopen search");
    poll(&server, "/state/viewer/search_open", |b| b.trim() == "true");
    server.req("POST", "/view/search/key/esc", "").expect("esc");
    poll(&server, "/state/viewer/search_open", |b| b.trim() == "false");
    let count3 = server.req("GET", "/state/viewer/match_count", "").expect("count3").1;
    assert_eq!(count3.trim(), "3", "Esc で閉じても検索語・ハイライトは残る");
    let search = server.req("GET", "/state/viewer/search", "").expect("search").1;
    assert_eq!(search.trim(), "\"foo\"", "Esc 後も検索語は残る: {search}");
    // 現在一致（青）は手放す＝開き直すと戻った表示位置から検索し直す。
    let m_esc = match_json(&server);
    assert_eq!(m_esc.trim(), "null", "Esc 後は現在一致をリセット: {m_esc}");
}

/// 検索バー右側のトグル（大小区別・単語一致・正規表現）が一致集合を変えるのを観測する。
#[test]
fn text_viewer_search_options_toggle_matches() {
    let server = Server::start(&["doc.txt"], "");
    std::fs::write(
        server.base.join("sbx").join("doc.txt"),
        "foo Foo FOO foobar\nfo fooo\n",
    )
    .unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/viewFile", "").expect("viewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    req_bytes(server.port, "GET", "/snapshot").expect("warmup snapshot");

    let count = |s: &Server| s.req("GET", "/state/viewer/match_count", "").expect("count").1.trim().to_string();

    server.req("POST", "/command/viewerSearchDialog", "").expect("open");
    poll(&server, "/state/viewer/search_open", |b| b.trim() == "true");

    // 既定（大小無視・部分一致）：foo, Foo, FOO, foobar の foo, fooo の foo ＝ 5。
    server.req("POST", "/view/search", "foo").expect("type foo");
    assert_eq!(count(&server), "5", "既定は大小無視の部分一致で 5 件");

    // 大小区別 ON：小文字 foo のみ（foo・foobar の foo・fooo の foo）＝ 3。
    server.req("POST", "/view/search/option/case_sensitive/on", "").expect("case on");
    assert_eq!(count(&server), "3", "大小区別で 3 件");
    let cs = server.req("GET", "/state/viewer/case_sensitive", "").expect("cs").1;
    assert_eq!(cs.trim(), "true", "case_sensitive が立つ");
    server.req("POST", "/view/search/option/case_sensitive/off", "").expect("case off");

    // 単語一致 ON：語境界の foo/Foo/FOO ＝ 3（foobar は外す）。
    server.req("POST", "/view/search/option/whole_word/on", "").expect("word on");
    assert_eq!(count(&server), "3", "単語一致で 3 件");

    // 単語一致 ON の状態で正規表現 ON にすると、単語一致は排他で OFF になる。
    server.req("POST", "/view/search/option/regex/on", "").expect("regex on (excl)");
    let ww = server.req("GET", "/state/viewer/whole_word", "").expect("ww").1;
    assert_eq!(ww.trim(), "false", "正規表現 ON で単語一致が排他 OFF");
    let rx0 = server.req("GET", "/state/viewer/regex", "").expect("rx0").1;
    assert_eq!(rx0.trim(), "true", "正規表現は ON");
    server.req("POST", "/view/search/option/regex/off", "").expect("regex off");

    // 正規表現 ON：fo+ は可変長で複数一致する（大小無視は既定で効く）。
    server.req("POST", "/view/search/option/regex/on", "").expect("regex on");
    server.req("POST", "/view/search", "fo+").expect("type fo+");
    let rc: i32 = count(&server).parse().unwrap();
    assert!(rc >= 5, "正規表現 fo+ で複数一致（可変長）: {rc}");
    let rx = server.req("GET", "/state/viewer/regex", "").expect("rx").1;
    assert_eq!(rx.trim(), "true", "regex が立つ");

    // 不正な正規表現は 0 件扱い（落ちない）。
    server.req("POST", "/view/search", "fo(").expect("type bad");
    assert_eq!(count(&server), "0", "不正な正規表現は 0 件");
}

/// 検索履歴：Enter 確定時のみ記録・重複は最新へ集約・履歴選択で入力欄へ入るのを観測する。
#[test]
fn text_viewer_search_history_records_on_enter() {
    let server = Server::start(&["doc.txt"], "");
    std::fs::write(server.base.join("sbx").join("doc.txt"), "foo bar baz qux\n").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/viewFile", "").expect("viewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    req_bytes(server.port, "GET", "/snapshot").expect("warmup snapshot");

    let history = |s: &Server| s.req("GET", "/state/viewer/history", "").expect("history").1.trim().to_string();

    // 入力しただけでは記録されない（Enter 確定時のみ）。
    server.req("POST", "/view/search", "foo").expect("type foo");
    assert_eq!(history(&server), "[]", "入力だけでは履歴に入らない");

    // Enter で確定すると記録される。
    server.req("POST", "/view/search/key/enter", "").expect("enter foo");
    assert_eq!(history(&server), "[\"foo\"]", "Enter 確定で foo が記録される");

    // 別語を確定すると新しい順で先頭へ。
    server.req("POST", "/view/search", "bar").expect("type bar");
    server.req("POST", "/view/search/key/enter", "").expect("enter bar");
    assert_eq!(history(&server), "[\"bar\",\"foo\"]", "新しい順に積まれる");

    // 既出の語を再確定すると重複削除＝最新（先頭）へ集約（件数は増えない）。
    server.req("POST", "/view/search", "foo").expect("type foo 2");
    server.req("POST", "/view/search/key/enter", "").expect("enter foo 2");
    assert_eq!(history(&server), "[\"foo\",\"bar\"]", "重複は最新へ集約");

    // 履歴の index 番目（新しい順）を選ぶと入力欄へ入って検索される。
    server.req("POST", "/view/search/history/1", "").expect("pick history 1");
    let search = server.req("GET", "/state/viewer/search", "").expect("search").1;
    assert_eq!(search.trim(), "\"bar\"", "履歴1番目（bar）が入力欄へ: {search}");
    poll(&server, "/state/viewer/search_open", |b| b.trim() == "true");

    // 履歴ドロップダウンの開閉が観測できる。
    server.req("POST", "/view/search/dropdown/open", "").expect("open dropdown");
    let lo = server.req("GET", "/state/viewer/list_open", "").expect("list_open").1;
    assert_eq!(lo.trim(), "true", "ドロップダウンが開く");
    server.req("POST", "/view/search/dropdown/close", "").expect("close dropdown");
    let lc = server.req("GET", "/state/viewer/list_open", "").expect("list_open2").1;
    assert_eq!(lc.trim(), "false", "ドロップダウンが閉じる");
}

/// トグルのニーモニック（Alt+C/W/R 相当）が、未割り当てなら該当トグルを反転する。
#[test]
fn text_viewer_search_mnemonic_toggles_options() {
    let server = Server::start(&["doc.txt"], "");
    std::fs::write(server.base.join("sbx").join("doc.txt"), "foo bar\n").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/viewFile", "").expect("viewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    req_bytes(server.port, "GET", "/snapshot").expect("warmup snapshot");
    server.req("POST", "/command/viewerSearchDialog", "").expect("open");
    poll(&server, "/state/viewer/search_open", |b| b.trim() == "true");

    // 既定はケース無視 ON（case_sensitive=false）。ニーモニック c で反転＝大小区別 ON。
    server.req("POST", "/view/search/mnemonic/c", "").expect("mnemonic c");
    let cs = server.req("GET", "/state/viewer/case_sensitive", "").expect("cs").1;
    assert_eq!(cs.trim(), "true", "c で大小区別が立つ");
    // w で単語境界 ON。
    server.req("POST", "/view/search/mnemonic/w", "").expect("mnemonic w");
    let ww = server.req("GET", "/state/viewer/whole_word", "").expect("ww").1;
    assert_eq!(ww.trim(), "true", "w で単語境界が立つ");
}

/// ニーモニックと同じ Alt+キーがユーザーのビューアキーバインドに割り当て済みなら、ユーザー側を
/// 優先する（トグルせずそのコマンドを実行）。ここでは Alt+C を viewerClose に割り当てて確認。
#[test]
fn text_viewer_search_mnemonic_yields_to_user_keybind() {
    let server = Server::start(&["doc.txt"], "[keybinds_textviewer]\n\"Alt+C\" = \"viewerClose\"\n");
    std::fs::write(server.base.join("sbx").join("doc.txt"), "foo bar\n").unwrap();
    server.req("POST", "/command/cursorDown", "").unwrap();
    server.req("POST", "/command/viewFile", "").expect("viewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    req_bytes(server.port, "GET", "/snapshot").expect("warmup snapshot");
    server.req("POST", "/command/viewerSearchDialog", "").expect("open");
    poll(&server, "/state/viewer/search_open", |b| b.trim() == "true");

    // Alt+C は被っているのでユーザーバインド（viewerClose）が走る＝ビューアが閉じる。
    server.req("POST", "/view/search/mnemonic/c", "").expect("mnemonic c");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "none");
}

/// scripting：起動時に scripts を読み込み、登録されたコマンドが `/script/commands` に並ぶ。
#[test]
fn script_commands_register_on_startup() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00-cmds.ts",
            r#"rerics.registerCommand("logHi", () => { rerics.log("hi from script"); });
               rerics.registerCommand("goUp", () => { rerics.navigate(rerics.currentDir() + "/.."); });"#,
        )],
    );
    let list = poll(&server, "/script/commands", |b| b.contains("logHi"));
    assert!(
        list.contains("logHi") && list.contains("goUp"),
        "registered commands should be listed: {list}"
    );
}

/// scripting：`/script/members` に補完候補（組込メンバー＋登録コマンド名）が並び、登録コマンドは
/// `r.<name>()` でも呼べる（式/コードから対象操作を書ける）。
#[test]
fn script_members_list_and_commands_callable_via_r() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00-cmds.ts",
            r#"rerics.registerCommand("goUp", () => { rerics.navigate(rerics.currentDir() + "/.."); });"#,
        )],
    );
    // 組込メンバーと登録コマンド名が補完候補として並ぶ。
    let members = poll(&server, "/script/members", |b| b.contains("goUp"));
    assert!(
        members.contains("currentDir") && members.contains("prompt"),
        "組込メンバーが並ぶ: {members}"
    );
    assert!(members.contains("goUp"), "登録コマンド名が並ぶ: {members}");

    // 登録コマンドは r.<name>() でも呼べる＝r.goUp() で親フォルダへ移動する。
    let before = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    server.req("POST", "/script/eval", "r.goUp()").unwrap();
    let after = poll(&server, "/state/panes/left/location", |b| b.trim() != before);
    assert_ne!(after.trim(), before, "r.goUp() で親へ移動する: {after}");
}

/// 値返しクエリ組込（cursorName/markedCount/hasMarks）はスクリプトの `r.token()` で実値（文字列／
/// 数値／真偽）を返し、状態変化に追従する。アクション系（cursorDown 等）は `null` を返す。
#[test]
fn script_query_builtins_return_scalar_values() {
    let server = Server::start_with_scripts(
        &["a.txt", "b.txt", "c.txt"],
        &[(
            "00.ts",
            r#"
            rerics.registerCommand("probe", () => {
              const before = r.markedCount();
              const hasBefore = r.hasMarks();
              rerics.activePane().apply((d) => {
                for (const it of d.items) if (it.name === "a.txt") it.selected = true;
              });
              const after = r.markedCount();
              const hasAfter = r.hasMarks();
              rerics.log("Q nameType=" + (typeof r.cursorName())
                + " before=" + before + " hasBefore=" + hasBefore
                + " after=" + after + " hasAfter=" + hasAfter
                + " act=" + r.cursorDown());
            });
            "#,
        )],
    );
    poll(&server, "/script/commands", |b| b.contains("probe"));
    server.req("POST", "/script/eval", "r.probe()").unwrap();
    let log = poll(&server, "/state/log/lines", |b| b.contains("Q nameType="));
    assert!(log.contains("nameType=string"), "cursorName は文字列を返す: {log}");
    assert!(log.contains("before=0") && log.contains("hasBefore=false"), "初期はマーク無し: {log}");
    assert!(log.contains("after=1") && log.contains("hasAfter=true"), "マーク後は数と真偽が追従: {log}");
    assert!(log.contains("act=null"), "アクション系コマンドは null を返す: {log}");
}

/// 設定エディタ：補完つき「引数」モーダルで `r.<prefix>` を打つと候補（組込メンバー＋登録コマンド）が
/// 出て、候補の確定でカレット直前のプレフィックスがメンバ名へ置換される（headless 観測）。
#[test]
fn completion_popup_lists_members_and_inserts_on_accept() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[("00.ts", r#"rerics.registerCommand("myCmd", () => {});"#)],
    );
    poll(&server, "/script/members", |b| b.contains("myCmd"));
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").expect("nav");
    // 組込コマンド行（makeDirectoryDialog）を選んで、補完つき「式を編集」モーダルを開く（応答先返し）。
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    // prefill（makeDirectoryDialog()）を一旦空にしてから打鍵する。
    server.req("POST", "/modal/text", "").unwrap();

    // `=r.my` と実キー入力（WM_CHAR＝EN_CHANGE 経路）すると、登録コマンド myCmd が候補に出る。
    // 先頭（index 0）はジャンル見出しのラベル行で、最初の候補は index 1。
    server.req("POST", "/completion/keystrokes", "=r.my").unwrap();
    let comp = poll(&server, "/completion", |b| b.contains("myCmd"));
    assert!(comp.contains("myCmd"), "登録コマンドが補完候補に出る: {comp}");
    assert!(comp.contains("── スクリプト・API ──"), "ジャンル見出しが挟まる: {comp}");

    // ラベル行（index 0）は確定の対象外＝本文は変わらない。
    server.req("POST", "/completion/accept/0", "").unwrap();
    let unchanged = server.req("GET", "/completion", "").unwrap().1;
    assert!(unchanged.contains(r#""text":"=r.my""#), "ラベル行は確定できない: {unchanged}");

    // 候補（index 1＝myCmd）を確定＝プレフィックス `my` が `myCmd()` へ置換される（引数なしは () 付き）。
    server.req("POST", "/completion/accept/1", "").unwrap();
    let comp2 = poll(&server, "/completion", |b| b.contains(r#""text":"=r.myCmd()""#));
    assert!(comp2.contains(r#""text":"=r.myCmd()""#), "確定でメンバ名＋() が挿入される: {comp2}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 式エディタの補完は、式の先頭の裸の識別子（機能欄の fast-path 表記 `cursorUp()` の編集中）でも
/// 効く。候補は組込コマンドだけ（host API・スクリプト関数は裸では呼べないので出さない）で、確定も
/// `r.` なしの裸形で挿入される。空欄では勝手に開かず、Ctrl+Space で全組込ブラウズが出る。
#[test]
fn completion_recognizes_bare_builtin_context() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    server.req("POST", "/modal/text", "").unwrap();

    // 裸の `cur` ＝組込（cursorDown 等）が出て、host API（currentDir）は出ない。
    server.req("POST", "/completion/keystrokes", "cur").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("cursorDown"));
    assert!(c.contains("cursorDown"), "裸の識別子で組込候補が出る: {c}");
    assert!(!c.contains("currentDir"), "host API は裸文脈に出ない: {c}");

    // cursorDown の行を確定＝裸形 `cursorDown()` が入る（`r.` は付かない）。
    let v: serde_json::Value = serde_json::from_str(&c).unwrap();
    let idx = v["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .position(|x| x.as_str().unwrap_or("").starts_with("cursorDown"))
        .expect("cursorDown の行がある");
    server.req("POST", &format!("/completion/accept/{idx}"), "").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains(r#""text":"cursorDown()""#));
    assert!(c2.contains(r#""text":"cursorDown()""#), "裸形で確定される: {c2}");

    // 空欄は勝手に開かないが、Ctrl+Space で全組込がジャンル見出し付きで出る。
    server.req("POST", "/modal/text", "").unwrap();
    let hidden = server.req("GET", "/completion", "").unwrap().1;
    assert!(hidden.contains(r#""visible":false"#), "空欄では開かない: {hidden}");
    server.req("POST", "/completion/key/ctrlspace", "").unwrap();
    let c3 = poll(&server, "/completion", |b| b.contains(r#""visible":true"#));
    assert!(c3.contains("── カーソル移動 ──"), "Ctrl+Space で全組込ブラウズ: {c3}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 式エディタの signature help：カレットを囲う呼び出しのシグネチャ＋説明がヒント行（`/completion`
/// の `hint`）に常設され、いま書いている引数が ‹› で強調される。host API は d.ts 由来・組込は
/// メタデータ由来（引数個別の doc）・裸の組込呼び出しでも効く。呼び出しの外では消える。
#[test]
fn signature_help_tracks_enclosing_call_and_argument() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    server.req("POST", "/modal/text", "").unwrap();

    // host API：`r.spawn(` で第 0 引数 cmd が強調され、d.ts の説明が添う。
    server.req("POST", "/completion/keystrokes", "r.spawn(").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("‹cmd›"));
    assert!(c.contains("spawn(‹cmd›, ...args)"), "spawn の第0引数強調: {c}");
    assert!(c.contains("外部プログラムを起動"), "d.ts の説明が添う: {c}");

    // 第 2 引数へ進むと rest（...args）の強調に移る。文字列中のカンマは数えない。
    server.req("POST", "/completion/keystrokes", "\"x, y\", ").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains("‹...args›"));
    assert!(c2.contains("‹...args›"), "rest 引数の強調: {c2}");

    // 裸の組込：`setCursorIndex(` は引数 index の doc が説明に出る。
    server.req("POST", "/modal/text", "").unwrap();
    server.req("POST", "/completion/keystrokes", "setCursorIndex(").unwrap();
    let c3 = poll(&server, "/completion", |b| b.contains("‹index›"));
    assert!(c3.contains("移動先の位置"), "組込引数の doc: {c3}");

    // 呼び出しの外（閉じた後）ではヒントが消える。
    server.req("POST", "/completion/keystrokes", "0)").unwrap();
    let c4 = poll(&server, "/completion", |b| b.contains(r#""hint":"""#));
    assert!(c4.contains(r#""hint":"""#), "呼び出しの外では消える: {c4}");

    // ライブ検査：構文が通る式の未解決識別子は、OK を押さなくてもヒントに出る。
    server.req("POST", "/completion/type", "r.spawn(zzz)").unwrap();
    let c5 = poll(&server, "/completion", |b| b.contains("zzz は定義されていない"));
    assert!(c5.contains("定義されていない"), "ライブで未解決識別子が出る: {c5}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 引数の値補完：Enum 型引数の中では値がクォート付きで、オプション Object の中ではキーが
/// `名前: ` 形で補完される。OK は構文エラーの式を保存せず、ヒント行へエラーを出してダイアログに
/// 留まる（正しい式に直せば閉じる）。
#[test]
fn argument_value_completion_and_syntax_check_on_ok() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    server.req("POST", "/modal/text", "").unwrap();

    // 組込 Enum：`r.sort(` の直後で値の一覧が出て、確定でクォート付きで入る。
    server.req("POST", "/completion/keystrokes", "r.sort(").unwrap();
    let c = poll(&server, "/completion", |b| b.contains(r#"\"name\""#));
    let v: serde_json::Value = serde_json::from_str(&c).unwrap();
    let idx = v["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .position(|x| x.as_str().unwrap_or("") == "\"name\"")
        .expect("name の値がある");
    server.req("POST", &format!("/completion/accept/{idx}"), "").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains(r#"r.sort(\"name\""#));
    assert!(c2.contains(r#"r.sort(\"name\""#), "Enum 値がクォート付きで入る: {c2}");

    // host API のオプション Object：spawn の `{` の中で cwd キーが出て、確定で `cwd: ` が入る。
    server.req("POST", "/modal/text", "").unwrap();
    server.req("POST", "/completion/keystrokes", "r.spawn(\"x\", {").unwrap();
    let c3 = poll(&server, "/completion", |b| b.contains("cwd"));
    let v: serde_json::Value = serde_json::from_str(&c3).unwrap();
    let idx = v["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .position(|x| x.as_str().unwrap_or("").starts_with("cwd"))
        .expect("cwd キーがある");
    server.req("POST", &format!("/completion/accept/{idx}"), "").unwrap();
    let c4 = poll(&server, "/completion", |b| b.contains("{cwd: "));
    assert!(c4.contains("{cwd: "), "オプションキーが入る: {c4}");

    // 構文チェック：壊れた式で OK ＝閉じずにヒント行へ「構文エラー」が出る。
    server.req("POST", "/completion/type", "r.spawn(\"x\", ").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let c5 = poll(&server, "/completion", |b| b.contains("構文エラー"));
    assert!(c5.contains("構文エラー"), "OK で構文エラーが示される: {c5}");

    // 意味検査：未定義の識別子は閉じずに名前を示す。裸の組込＋非リテラル引数は r. 経由へ誘導。
    server.req("POST", "/completion/type", "r.spawn(aaa)").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let c6 = poll(&server, "/completion", |b| b.contains("定義されていない"));
    assert!(c6.contains("aaa"), "未定義の識別子を示す: {c6}");
    server.req("POST", "/completion/type", "cursorUp(aaa)").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let c7 = poll(&server, "/completion", |b| b.contains("r.cursorUp"));
    assert!(c7.contains("r.cursorUp"), "r. 経由へ誘導する: {c7}");

    // 意味検査：組込 fast-path の Enum に無い値も閉じずに示す（候補の列挙付き）。
    server.req("POST", "/completion/type", "sort(\"nmae\")").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let c8 = poll(&server, "/completion", |b| b.contains("は無い"));
    assert!(c8.contains("nmae") && c8.contains("extension"), "Enum に無い値を示す: {c8}");

    // 複文＝エラーになった文（行番号＋その行の中身）がヒントに添えられる。
    server.req("POST", "/completion/type", "const a = 1;\nr.spawn(bbb);").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let c9 = poll(&server, "/completion", |b| b.contains("2 行目"));
    assert!(
        c9.contains("bbb は定義されていない") && c9.contains("r.spawn(bbb);"),
        "エラー行の中身を添える: {c9}"
    );

    // 正しい式に直して OK ＝式エディタが閉じる（補完プローブが外れて null になる）。
    server.req("POST", "/completion/type", "cursorUp()").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let closed = poll(&server, "/completion", |b| b == "null");
    assert_eq!(closed, "null", "正しい式なら閉じる");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 式エディタの補完は名前空間の中身まで降りる（2 階層）。`r.fs.` で `fs.readText` 等が候補に出て、
/// 確定すると名前空間込みのメンバ名が挿入される。
#[test]
fn completion_descends_into_namespace_members() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    server.req("POST", "/modal/text", "").unwrap();

    // `=r.fs.read` で名前空間 fs 配下の readText が候補に出る（トップレベルの混入はしない）。
    server.req("POST", "/completion/keystrokes", "=r.fs.read").unwrap();
    let comp = poll(&server, "/completion", |b| b.contains("fs.readText"));
    assert!(comp.contains("fs.readText"), "名前空間メンバが補完候補に出る: {comp}");

    // 確定＝prefix `fs.read` が名前空間込みのメンバ名 `fs.readText` へ置換される。
    server.req("POST", "/completion/accept/0", "").unwrap();
    let comp2 = poll(&server, "/completion", |b| b.contains(r#""text":"=r.fs.readText"#));
    assert!(comp2.contains(r#""text":"=r.fs.readText"#), "確定で名前空間込み挿入: {comp2}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 式エディタの `r.` 補完は、組込コマンドのメンバに引数シグネチャ＋説明（メタデータ由来）を
/// 添えて見せる。非組込（host API・スクリプト関数）には付かない。
#[test]
fn completion_annotates_builtin_members_with_meta() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    server.req("POST", "/modal/text", "").unwrap();

    // 引数を取る組込（cursorDown）＝シグネチャ {select?} と説明文が添う。
    server.req("POST", "/completion/keystrokes", "=r.cursorDow").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("cursorDown"));
    assert!(
        c.contains("{select?}") && c.contains("カーソルを 1 つ下の項目へ移動する"),
        "組込メンバに引数ヒントと説明が添う: {c}"
    );

    // 引数なしの組込（reload）＝シグネチャは無く説明文だけが添う。
    server.req("POST", "/modal/text", "").unwrap();
    server.req("POST", "/completion/keystrokes", "=r.reloa").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains("reload"));
    assert!(c2.contains("最新にする"), "引数なし組込は説明だけが添う: {c2}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 式エディタは式の複雑さで 1 行／複数行モードを畳む。単一行の式はコンパクト（`multiline:false`）で
/// 開き、「複数行」トグル（モーダル唯一のチェックボックス＝`/modal/check`）で展開・再畳みできる。
/// モードは `/completion` の `multiline` で観測する。
#[test]
fn code_editor_folds_between_single_and_multi_line() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();

    let mode = |s: &str| {
        serde_json::from_str::<serde_json::Value>(s).unwrap()["multiline"].as_bool().unwrap()
    };

    // 単一行の式（makeDirectoryDialog()）はコンパクト（1 行）で開く。
    let c = poll(&server, "/completion", |b| b.contains("multiline"));
    assert!(!mode(&c), "単一行式はコンパクトで開く: {c}");

    // 「複数行」トグルで展開＝multiline:true。
    server.req("POST", "/modal/check", "").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains("\"multiline\":true"));
    assert!(mode(&c2), "トグルで複数行へ展開: {c2}");

    // もう一度トグルでコンパクトへ戻る。
    server.req("POST", "/modal/check", "").unwrap();
    let c3 = poll(&server, "/completion", |b| b.contains("\"multiline\":false"));
    assert!(!mode(&c3), "再トグルで 1 行へ畳む: {c3}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 式エディタ（code_box）の `r.` 補完は機能ブラウザを兼ねる：`r.` 直後（空クエリ）で全機能が
/// ジャンル見出し付きで並び、見出し行は選択・確定の対象外。候補の確定で `名前()` が挿入される
/// （名前うろ覚えのブラウズ入力）。
#[test]
fn code_editor_browses_functions_by_genre_in_completion() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    poll(&server, "/state", |b| b.contains("コードを割り当て"));
    server.req("POST", "/modal/text", "").unwrap();

    // `r.` 直後＝空クエリで全機能がジャンル見出し付きで出る。
    server.req("POST", "/completion/keystrokes", "r.").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("── カーソル移動 ──"));
    assert!(
        c.contains("── ファイル操作 ──") && c.contains("── スクリプト・API ──"),
        "ジャンル見出しが並ぶ: {c}"
    );

    // 見出し行（index 0）は確定できない＝本文は変わらない。
    server.req("POST", "/completion/accept/0", "").unwrap();
    let unchanged = server.req("GET", "/completion", "").unwrap().1;
    assert!(unchanged.contains(r#""text":"r.""#), "見出し行は確定できない: {unchanged}");

    // 一覧から delete の行を確定＝code_box の本文へ r.delete() が入る。
    let v: serde_json::Value = serde_json::from_str(&c).unwrap();
    let idx = v["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .position(|x| x.as_str().unwrap_or("").starts_with("delete"))
        .expect("delete の行がある");
    server.req("POST", &format!("/completion/accept/{idx}"), "").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains("r.delete()"));
    assert!(c2.contains("r.delete()"), "確定で r.delete() が本文へ入る: {c2}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 式エディタ（code_box）はサイズ可変。リサイズしても再レイアウトがクラッシュせず、撮影でき、
/// 補完も動く（リサイズ後の候補欄で候補が出る）。
#[test]
fn code_editor_is_resizable() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    poll(&server, "/state", |b| b.contains("コードを割り当て"));

    // 大きくリサイズしても再レイアウトが走り、撮影できる（クラッシュしない）。
    let (st, _) = server.req("POST", "/modal/resize/720x600", "").expect("resize");
    assert_eq!(st, 200, "/modal/resize は 200");
    let (sst, png) = req_bytes(server.port, "GET", "/snapshot/modal").expect("snap");
    assert_eq!(sst, 200, "リサイズ後も /snapshot/modal は 200");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 署名");

    // リサイズ後も補完が動く（再配置された候補欄に候補が出る）。
    server.req("POST", "/modal/text", "").unwrap();
    server.req("POST", "/completion/keystrokes", "=r.cursorDow").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("cursorDown"));
    assert!(c.contains("cursorDown"), "リサイズ後も補完が出る: {c}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 式エディタの `r.` 補完は、登録スクリプト関数にも `registerCommand` の summary を添える
/// （組込メタと同じインタフェース＝組込／スクリプトを区別せず説明が出る）。
#[test]
fn completion_annotates_script_members_with_summary() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00.ts",
            r#"rerics.registerCommand("organize", () => {}, { summary: "散らかりを整える" });"#,
        )],
    );
    poll(&server, "/script/members", |b| b.contains("organize"));
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    server.req("POST", "/modal/text", "").unwrap();

    server.req("POST", "/completion/keystrokes", "=r.organi").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("organize"));
    assert!(c.contains("散らかりを整える"), "スクリプト関数に summary が添う: {c}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 補完つき入力欄のキーボード操作：↑↓で候補移動（クランプ）・Enter で確定・Ctrl+Space で強制表示。
/// 実キー経路（WM_KEYDOWN/WM_CHAR を keyhook サブクラスが横取り）を headless で検証する。
#[test]
fn completion_keyboard_navigation_and_ctrl_space() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    server.req("POST", "/modal/text", "").unwrap();
    let comp = || server.req("GET", "/completion", "").unwrap().1;

    // `=r.o` で候補が出る。並びはジャンル見出し付き：
    //   0=── ペイン ──, 1=oppositeToCurrent, 2=── アプリ・その他 ──, 3=openHelp, 4=openSettings, …
    // 初期選択は先頭の候補行（index 1。index 0 は見出しラベル）。
    server.req("POST", "/completion/keystrokes", "=r.o").unwrap();
    let c = poll(&server, "/completion", |b| b.contains(r#""visible":true"#));
    assert!(c.contains("oppositeToCurrent"), "候補が出る: {c}");
    assert!(c.contains(r#""selected":1"#), "先頭の候補行が選択される: {c}");

    // ↓↓↑：見出し行（index 2）をスキップして 1→3→4→3 と動く。
    server.req("POST", "/completion/key/down", "").unwrap();
    server.req("POST", "/completion/key/down", "").unwrap();
    server.req("POST", "/completion/key/up", "").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains(r#""selected":3"#));
    assert!(c2.contains(r#""selected":3"#), "↓↓↑ は見出しをスキップして index 3: {c2}");

    // PageDown＝1 画面ぶん先の候補行へ・PageUp を余分に打っても先頭候補行（index 1）でクランプ。
    server.req("POST", "/completion/key/pagedown", "").unwrap();
    let cp = poll(&server, "/completion", |b| !b.contains(r#""selected":3"#));
    assert!(!cp.contains(r#""selected":3"#), "PageDown でまとめて移動: {cp}");
    server.req("POST", "/completion/key/pageup", "").unwrap();
    server.req("POST", "/completion/key/pageup", "").unwrap();
    let cq = poll(&server, "/completion", |b| b.contains(r#""selected":1"#));
    assert!(cq.contains(r#""selected":1"#), "PageUp は先頭の候補行でクランプ: {cq}");

    // ↓ で見出し（index 2）を飛ばして index 3（openHelp）へ戻し、Enter で確定＝
    // プレフィックス o が openHelp() に置換される。
    server.req("POST", "/completion/key/down", "").unwrap();
    server.req("POST", "/completion/key/enter", "").unwrap();
    let c3 = poll(&server, "/completion", |b| b.contains(r#""text":"=r.openHelp()"#));
    assert!(c3.contains(r#""text":"=r.openHelp()"#), "Enter で openHelp 確定: {c3}");

    // 唯一一致 `=r.currentDir` は自動では隠れる。Ctrl+Space で強制表示できる。
    server.req("POST", "/completion/keystrokes", "=r.currentDir").unwrap();
    assert!(comp().contains(r#""visible":false"#), "唯一一致は自動で隠れる: {}", comp());
    server.req("POST", "/completion/key/ctrlspace", "").unwrap();
    let c5 = poll(&server, "/completion", |b| b.contains(r#""visible":true"#));
    assert!(c5.contains("currentDir"), "Ctrl+Space で強制表示: {c5}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 補完はカレット直前の文字列で判定する：`=r.co` で c と o の間へカレットを戻すと、`co` ではなく
/// `c` の候補（currentDir 等）に変わる。確定は末尾の o を残すので利用者が自分で消す前提。
#[test]
fn completion_uses_text_up_to_caret() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "makeDirectoryDialog").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openexpr", "").unwrap();
    server.req("POST", "/modal/text", "").unwrap();

    // `=r.co` の末尾＝候補は co 前方一致（command/confirm/copy）。currentDir は含まれない。
    server.req("POST", "/completion/keystrokes", "=r.co").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("confirm"));
    assert!(c.contains("confirm"), "co の候補: {c}");
    assert!(!c.contains("currentDir"), "co では currentDir は出ない: {c}");

    // ← でカレットを c と o の間へ＝候補が c 前方一致へ変わり currentDir が入る。
    server.req("POST", "/completion/key/left", "").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains("currentDir"));
    assert!(c2.contains("currentDir"), "カレットを戻すと c の候補（currentDir）に変わる: {c2}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// scripting：`/script/eval` で評価したコードのログがアプリのログ欄へ出る（エンジン→UI 配線）。
#[test]
fn script_eval_runs_and_logs_to_app() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    let (st, _) = server
        .req("POST", "/script/eval", r#"rerics.log("eval-marker-42");"#)
        .expect("eval");
    assert_eq!(st, 200, "eval accepted");
    let log = poll(&server, "/state/log", |b| b.contains("eval-marker-42"));
    assert!(log.contains("eval-marker-42"), "eval log should reach app: {log}");
}

/// scripting：`/script/invoke` で登録コマンドを呼ぶと、ペイン操作（navigate）が UI に反映される。
#[test]
fn script_invoke_navigates_active_pane() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00-cmds.ts",
            r#"rerics.registerCommand("goUp", () => { rerics.navigate(rerics.currentDir() + "/.."); });"#,
        )],
    );
    let loc0 = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert!(loc0.contains("sbx"), "should start in the sandbox: {loc0}");

    server.req("POST", "/script/invoke/goUp", "").expect("invoke");
    let loc1 = poll(&server, "/state/panes/left/location", |b| !b.contains("sbx"));
    assert!(
        !loc1.contains("sbx"),
        "active pane should leave the sandbox after goUp: {loc1}"
    );
}

/// scripting：`rerics.confirm` がモーダルを出し、Yes 応答が boolean で返る（同期往復）。
#[test]
fn script_confirm_opens_modal_and_returns_choice() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req("POST", "/script/eval", r#"rerics.log("confirm=" + rerics.confirm("ok?"));"#)
        .expect("eval");
    // confirm 中もモーダルのメッセージループが回るので /state・/modal が応答する（デッドロックしない）。
    wait_modal(&server);
    server.req("POST", "/modal/key/y", "").expect("yes");
    let log = poll(&server, "/state/log", |b| b.contains("confirm=true"));
    assert!(log.contains("confirm=true"), "yes should yield true: {log}");
}

/// scripting：`rerics.prompt` が入力モーダルを出し、入力文字列が返る。
#[test]
fn script_prompt_opens_modal_and_returns_text() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req("POST", "/script/eval", r#"rerics.log("name=" + rerics.prompt("name?", "def"));"#)
        .expect("eval");
    wait_modal(&server);
    server.req("POST", "/modal/text", "hello").expect("text");
    server.req("POST", "/modal/key/enter", "").expect("enter");
    let log = poll(&server, "/state/log", |b| b.contains("name=hello"));
    assert!(log.contains("name=hello"), "prompt should return typed text: {log}");
}

/// scripting：`rerics.select` が一覧モーダルを出し、選んだ行の index が返る。
#[test]
fn script_select_opens_list_and_returns_index() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"rerics.log("idx=" + rerics.select("pick", ["x", "y", "z"]));"#,
        )
        .expect("eval");
    wait_modal(&server);
    server.req("POST", "/modal/select/1", "").expect("select");
    server.req("POST", "/modal/command/ok", "").expect("ok");
    let log = poll(&server, "/state/log", |b| b.contains("idx=1"));
    assert!(log.contains("idx=1"), "select should return chosen index: {log}");
}

/// scripting：`rerics.clipboard` の setText→getText がラウンドトリップする（CF_UNICODETEXT
/// の実クリップボードへ書いて読み戻す host 往復）。
#[test]
fn script_clipboard_round_trips_text() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"rerics.clipboard.setText("clip-rt-7"); rerics.log("clip=" + rerics.clipboard.getText());"#,
        )
        .expect("eval");
    let log = poll(&server, "/state/log", |b| b.contains("clip=clip-rt-7"));
    assert!(log.contains("clip=clip-rt-7"), "clipboard text should round-trip: {log}");
}

/// scripting：`rerics.clipboard` の setImage→getImage が画像をラウンドトリップする（CF_DIB の
/// 実クリップボードへ書いて読み戻し、寸法が保たれる host 往復）。
#[test]
fn script_clipboard_round_trips_image() {
    let dir = std::env::temp_dir().join(format!("rerics_clipimg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.png");
    let dst = dir.join("dst.png");
    let img = image::RgbImage::from_fn(3, 2, |x, y| image::Rgb([(x * 80) as u8, (y * 120) as u8, 200]));
    img.save(&src).unwrap();

    let server = Server::start_with_scripts(&["a.txt"], &[]);
    let js = format!(
        r#"rerics.clipboard.setImage("{}"); rerics.log("imgrt=" + rerics.clipboard.getImage("{}"));"#,
        src.display().to_string().replace('\\', "/"),
        dst.display().to_string().replace('\\', "/"),
    );
    server.req("POST", "/script/eval", &js).expect("eval");
    let log = poll(&server, "/state/log", |b| b.contains("imgrt="));
    assert!(log.contains("imgrt=true"), "clipboard image should round-trip: {log}");

    let out = image::open(&dst).expect("saved image decodes");
    assert_eq!((out.width(), out.height()), (3, 2), "dimensions preserved");
    let _ = std::fs::remove_dir_all(&dir);
}

/// scripting：`rerics.activePane()` が実ペインの項目・選択・カーソルを読み取れる
/// （オブジェクトモデルの実 GUI 経路＝スナップショットが UI スレッドから組み上がる）。
#[test]
fn script_active_pane_reads_items_selection_and_cursor() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt", "c.txt"], &[]);
    // 左 items は [.., a.txt, b.txt, c.txt]。cursorDown×1 で a.txt → markToggle で
    // a.txt を選択しカーソルは b.txt（index 2）へ。
    server.req("POST", "/command/cursorDown", "").expect("down");
    server.req("POST", "/command/markToggle", "").expect("mark");

    server
        .req(
            "POST",
            "/script/eval",
            r#"const p = rerics.activePane();
               rerics.log("om count=" + p.items.length
                 + " sel=" + p.selectedItems.map(i => i.name).join(",")
                 + " cur=" + (p.cursorItem ? p.cursorItem.name : "none")
                 + " inSbx=" + (p.dir.indexOf("sbx") >= 0));"#,
        )
        .expect("eval");

    let log = poll(&server, "/state/log", |b| b.contains("om count="));
    assert!(
        log.contains("om count=4 sel=a.txt cur=b.txt inSbx=true"),
        "activePane should reflect real items/selection/cursor: {log}"
    );

    // 反対ペインも読める（同じサンドボックスを開いている＝項目数は一致、選択は無し）。
    server
        .req(
            "POST",
            "/script/eval",
            r#"const o = rerics.oppositePane();
               rerics.log("opp count=" + o.items.length + " sel=" + o.selectedItems.length);"#,
        )
        .expect("eval opp");
    let log2 = poll(&server, "/state/log", |b| b.contains("opp count="));
    assert!(
        log2.contains("opp count=4 sel=0"),
        "oppositePane should read the other side: {log2}"
    );
}

/// scripting：選択の書き戻し。`apply()` のバッチ反映と即時 `selected=` が、実ペインの
/// 選択状態（`/state` の `marked`）へ届くことを検証する（オブジェクトモデル書き戻しの実経路）。
#[test]
fn script_selection_write_back_reaches_pane() {
    let server = Server::start_with_scripts(&["a.txt", "b.dat", "c.txt"], &[]);
    // 前提：まだ何も選択されていない。
    let items0 = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert_eq!(count_substr(&items0, "\"marked\":true"), 0, "no selection yet: {items0}");

    // バッチ：apply() の中で .txt を全選択 → 1 往復で a.txt と c.txt が marked になる。
    server
        .req(
            "POST",
            "/script/eval",
            r#"rerics.activePane().apply((d) => {
                 for (const it of d.items) if (it.ext === "txt") it.selected = true;
               });"#,
        )
        .expect("apply eval");
    let items1 = poll(&server, "/state/panes/left/items", |b| {
        count_substr(b, "\"marked\":true") == 2
    });
    assert_eq!(
        count_substr(&items1, "\"marked\":true"),
        2,
        "apply should mark both .txt files: {items1}"
    );

    // 即時：先頭の .txt（a.txt）を 1 つだけ即時に外す → marked は 1 件へ。
    server
        .req(
            "POST",
            "/script/eval",
            r#"rerics.activePane().items.find((it) => it.ext === "txt").selected = false;"#,
        )
        .expect("immediate eval");
    let items2 = poll(&server, "/state/panes/left/items", |b| {
        count_substr(b, "\"marked\":true") == 1
    });
    assert_eq!(
        count_substr(&items2, "\"marked\":true"),
        1,
        "immediate write should deselect one: {items2}"
    );
}

/// scripting：`rerics.command()` が内蔵コマンドを実行し（カーソル移動が UI に反映）、
/// 不明な名前は JS の例外になる（throw を catch できる）。
#[test]
fn script_command_invokes_builtin_and_throws_on_unknown() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt"], &[]);
    let c0 = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(c0.trim(), "0", "initial cursor");

    // 内蔵コマンドを実行＝カーソルが 1 へ進む。
    server
        .req("POST", "/script/eval", r#"rerics.command("cursorDown");"#)
        .expect("command eval");
    let c1 = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    assert_eq!(c1.trim(), "1", "rerics.command should run the builtin: {c1}");

    // 不明コマンドは例外になり、catch でメッセージを拾える。
    server
        .req(
            "POST",
            "/script/eval",
            r#"try { rerics.command("NoSuchCmd"); }
               catch (e) { rerics.log("cmd-error:" + e.message); }"#,
        )
        .expect("unknown eval");
    let log = poll(&server, "/state/log", |b| b.contains("cmd-error:"));
    assert!(
        log.contains("cmd-error:") && log.contains("NoSuchCmd"),
        "unknown command should throw with its name: {log}"
    );
}

/// scripting：`rerics.on` のイベントが実 GUI で配られる。executeCommand は全コマンドで、
/// changeDirectory は実移動のときだけ発火する（在席コマンドでは出ない）。
#[test]
fn script_events_fire_on_command_and_navigation() {
    let server = Server::start_with_scripts(
        &["a.txt", "b.txt"],
        &[(
            "00-ev.ts",
            r#"rerics.registerCommand("ready", () => {});
               rerics.on("executeCommand", (name) => rerics.log("EV cmd:" + name));
               rerics.on("changeDirectory", (dir) => rerics.log("EV cd:" + dir));"#,
        )],
    );
    // ハンドラ登録の完了を待つ（コマンドをビーコンに使う）。
    poll(&server, "/script/commands", |b| b.contains("ready"));

    // 在席コマンド：executeCommand は出るが changeDirectory は出ない（移動でないため）。
    server.req("POST", "/command/cursorDown", "").unwrap();
    let log = poll(&server, "/state/log", |b| b.contains("EV cmd:cursorDown"));
    assert!(log.contains("EV cmd:cursorDown"), "executeCommand should fire: {log}");
    assert_eq!(
        count_substr(&log, "EV cd:"),
        0,
        "in-place command must not fire changeDirectory: {log}"
    );

    // 親へ移動：移動なので changeDirectory も発火する。
    server.req("POST", "/command/toParent", "").unwrap();
    let log2 = poll(&server, "/state/log", |b| b.contains("EV cd:"));
    assert!(log2.contains("EV cd:"), "navigation should fire changeDirectory: {log2}");
    assert!(
        log2.contains("EV cmd:toParent"),
        "toParent should also fire executeCommand: {log2}"
    );
}

/// scripting：`await rerics.copy()` がワーカー完了で resolve し、コピーが実ペインに反映される
/// （非同期操作ブリッジの実経路＝UI スレッドのワーカー完了がエンジンの await を解く）。
#[test]
fn script_async_copy_awaits_worker_completion() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt"], &[]);
    // 右ペインを親へ移し、左=sbx／右=親 にする（src≠dst で同名衝突を避ける）。
    server.req("POST", "/command/focusRight", "").unwrap();
    server.req("POST", "/command/toParent", "").unwrap();
    let parent = server.req("GET", "/state/panes/right/location", "").unwrap().1;
    assert!(!parent.contains("sbx"), "right pane should be the parent: {parent}");
    server.req("POST", "/command/focusLeft", "").unwrap();

    // 左で a.txt を選択して await copy → 親へコピーされ、完了後にログが出る。
    server
        .req(
            "POST",
            "/script/eval",
            r#"(async () => {
                 rerics.activePane().apply((d) => {
                   for (const it of d.items) if (it.name === "a.txt") it.selected = true;
                 });
                 await rerics.copy();
                 rerics.log("ASYNC COPY DONE");
               })();"#,
        )
        .expect("eval");

    // await が解けて完了ログが出る（＝ワーカー完了がエンジンへ橋渡しされた）。
    let log = poll(&server, "/state/log", |b| b.contains("ASYNC COPY DONE"));
    assert!(log.contains("ASYNC COPY DONE"), "await copy should resolve after the worker: {log}");
    // 右ペイン（親）に a.txt がコピーされている。
    let right = poll(&server, "/state/panes/right/items", |b| b.contains("\"name\":\"a.txt\""));
    assert!(right.contains("\"name\":\"a.txt\""), "copied file should appear in the dest pane: {right}");
}

/// scripting：非同期操作の job が awaitable かつ `.cancel()` を持つ（キャンセル経路が実
/// GuiHost を通って壊れない）。実際に中止されるかはタイミング依存なのでフロー完走だけ見る。
#[test]
fn script_async_op_job_is_cancelable() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt"], &[]);
    server.req("POST", "/command/focusRight", "").unwrap();
    server.req("POST", "/command/toParent", "").unwrap();
    server.req("POST", "/command/focusLeft", "").unwrap();

    server
        .req(
            "POST",
            "/script/eval",
            r#"(async () => {
                 rerics.activePane().apply((d) => {
                   for (const it of d.items) if (it.name === "a.txt") it.selected = true;
                 });
                 const job = rerics.copy();
                 job.cancel();
                 try { await job; } catch (e) { /* 中止なら例外 */ }
                 rerics.log("CANCEL FLOW DONE");
               })();"#,
        )
        .expect("eval");

    let log = poll(&server, "/state/log", |b| b.contains("CANCEL FLOW DONE"));
    assert!(log.contains("CANCEL FLOW DONE"), "cancel flow should complete without error: {log}");
}

/// scripting：`await rerics.delete()` がアクティブペインの選択を削除し、完了で resolve する。
#[test]
fn script_async_delete_awaits_completion() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt", "c.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"(async () => {
                 rerics.activePane().apply((d) => {
                   for (const it of d.items) if (it.name === "a.txt") it.selected = true;
                 });
                 await rerics.delete();
                 rerics.log("DELETE DONE");
               })();"#,
        )
        .expect("eval");

    let log = poll(&server, "/state/log", |b| b.contains("DELETE DONE"));
    assert!(log.contains("DELETE DONE"), "await delete should resolve: {log}");
    // a.txt が消え、他は残る。
    let items = poll(&server, "/state/panes/left/items", |b| !b.contains("\"name\":\"a.txt\""));
    assert!(!items.contains("\"name\":\"a.txt\""), "a.txt should be deleted: {items}");
    assert!(items.contains("\"name\":\"b.txt\""), "b.txt must remain: {items}");
}

/// scripting：明示引数版 `rerics.copy(items, dest)`＝項目のフルパスと行き先を渡してコピーする。
#[test]
fn script_async_copy_explicit_items_and_dest() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt"], &[]);
    // 右ペインを親へ（行き先）。左＝sbx のファイルをフルパスで渡す。
    server.req("POST", "/command/focusRight", "").unwrap();
    server.req("POST", "/command/toParent", "").unwrap();
    server.req("POST", "/command/focusLeft", "").unwrap();

    server
        .req(
            "POST",
            "/script/eval",
            r#"(async () => {
                 const p = rerics.activePane();
                 const items = p.items.filter((it) => !it.isDir).map((it) => it.fullName);
                 await rerics.copy(items, rerics.oppositePane().dir);
                 rerics.log("EXPLICIT COPY DONE");
               })();"#,
        )
        .expect("eval");

    let log = poll(&server, "/state/log", |b| b.contains("EXPLICIT COPY DONE"));
    assert!(log.contains("EXPLICIT COPY DONE"), "explicit copy should resolve: {log}");
    // 行き先＝sbx の親（＝サンドボックスのベース）。両ファイルがそこへ書かれている。
    assert!(
        server.base.join("a.txt").exists() && server.base.join("b.txt").exists(),
        "explicit copy should write both files to the dest dir"
    );
}

/// scripting：`copy({ onProgress })` の進捗コールバックがコピー中に発火する（ワーカーの
/// 進捗が token 経由でストリームされ、完了前に onProgress が呼ばれる実経路）。
#[test]
fn script_async_copy_reports_progress() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt"], &[]);
    // 右ペインを親へ（src≠dst）。
    server.req("POST", "/command/focusRight", "").unwrap();
    server.req("POST", "/command/toParent", "").unwrap();
    server.req("POST", "/command/focusLeft", "").unwrap();

    server
        .req(
            "POST",
            "/script/eval",
            r#"(async () => {
                 let count = 0;
                 rerics.activePane().apply((d) => {
                   for (const it of d.items) if (it.name === "a.txt") it.selected = true;
                 });
                 await rerics.copy({ onProgress: (p) => { if (p && p.text) count++; } });
                 rerics.log("PROGRESS COUNT " + count);
               })();"#,
        )
        .expect("eval");

    let log = poll(&server, "/state/log", |b| b.contains("PROGRESS COUNT "));
    // 最低 1 回は届く（begin_progress＋完了更新で 2 回以上のはず）。0 でないことを確かめる。
    assert!(
        log.contains("PROGRESS COUNT ") && !log.contains("PROGRESS COUNT 0"),
        "onProgress should fire at least once during copy: {log}"
    );
}

/// quit は「賢いクローズ」：タブが複数あれば現タブを閉じ、最後の 1 枚ならアプリを終了する。
/// ここでは複数タブ時に現タブが閉じてアプリが生き続けること（＝強制終了でない）を検証する。
#[test]
fn quit_closes_tab_when_multiple_keeps_app_alive() {
    let server = Server::start(&["a.txt"], "");
    let count = || server.req("GET", "/state/tabs/count", "").expect("count").1;
    assert_eq!(count().trim(), "1", "初期は 1 タブ");
    server.req("POST", "/command/newFiler", "").expect("newFiler");
    assert_eq!(count().trim(), "2", "newFiler で 2 タブ");
    // タブが複数あるので quit は現タブを閉じるだけ（アプリは終了しない）。
    server.req("POST", "/command/quit", "").expect("quit");
    assert_eq!(count().trim(), "1", "quit で 1 タブに減る");
    assert!(server.req("GET", "/state", "").is_some(), "アプリは終了していない");
}

/// 組込コマンドが `r.<token>()` の名前付き関数として呼べる（bootstrap が動的生成）。
/// スクリプトから `r.cursorDown()` でカーソルが動く＝命令ブリッジ経由で内蔵コマンドへ届く。
#[test]
fn script_builtin_command_callable_as_r_method() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt", "c.txt"], &[]);
    assert_eq!(
        server.req("GET", "/state/panes/left/cursor", "").unwrap().1.trim(),
        "0",
        "初期カーソルは 0"
    );
    server.req("POST", "/script/eval", "r.cursorDown();").expect("eval");
    let c = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    assert_eq!(c.trim(), "1", "r.cursorDown() で内蔵コマンドが走りカーソルが 1 へ");
}

/// キーバインド経路：機能欄のスクリプト式（コード）が `exec` からエンジンへ流れ、評価される。
/// `/exec` は実際のキー押下と同じ `exec` を通るので、これでキー→コード評価の配線を検証する。
#[test]
fn exec_dispatches_script_code_to_engine() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server.req("POST", "/exec", r#"rerics.log("cmd-eval-marker-7");"#).expect("exec");
    let log = poll(&server, "/state/log", |b| b.contains("cmd-eval-marker-7"));
    assert!(log.contains("cmd-eval-marker-7"), "コード式が評価されて記録するはず: {log}");
}

/// 値返し eval：最後の式の値が文字列で返る。undefined/null は空、Promise は解決を待つ。
/// （HostApi を呼ぶ式は同期評価ではデッドロックするので 第1弾 では純粋な式のみ＝後段で非同期化）。
#[test]
fn eval_value_returns_last_expression() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    let body = |code: &str| server.req("POST", "/script/eval-value", code).expect("eval-value").1;
    assert_eq!(body("1 + 2").trim(), "\"3\"", "数式の結果を文字列で返す");
    assert_eq!(body(r#""ab" + "cd""#).trim(), "\"abcd\"", "文字列連結");
    assert_eq!(body("undefined").trim(), "\"\"", "undefined は空文字");
    assert_eq!(body("null").trim(), "\"\"", "null は空文字");
    assert_eq!(body(r#"Promise.resolve("async-7")"#).trim(), "\"async-7\"", "Promise は解決を待つ");
}

/// `r` 別名：`r` は `rerics` と同一オブジェクトで、ホスト API メソッドが見える。
#[test]
fn r_alias_points_to_rerics() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    let body = |code: &str| server.req("POST", "/script/eval-value", code).expect("eval-value").1;
    assert_eq!(body("r === rerics").trim(), "\"true\"", "r は rerics と同一参照");
    assert_eq!(body("typeof r.currentDir").trim(), "\"function\"", "r 経由でホスト API が見える");
}

/// プロセス op：`await rerics.run` が外部プロセスの終了を待ち、終了コードと stdout を返す。
#[test]
fn run_executes_process_and_returns_result() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"(async () => {
                 const r = await rerics.run("cmd", "/c", "echo", "rerics-run-9");
                 rerics.log("RUN code=" + r.code + " out=[" + r.stdout.trim() + "]");
               })();"#,
        )
        .expect("eval");
    let log = poll(&server, "/state/log", |b| b.contains("RUN code="));
    assert!(log.contains("RUN code=0"), "run は終了コード0を返すはず: {log}");
    assert!(log.contains("rerics-run-9"), "run は stdout を返すはず: {log}");
}

/// 並列 op：`rerics.parallel` が別スレッド＋別アイソレートで関数を走らせ、結果を await で返す。
/// `Promise.all` で複数を同時に投げても全件正しく集まる（実バイナリのワーカー factory 経由）。
#[test]
fn parallel_runs_functions_and_collects_results() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    let body = |code: &str| server.req("POST", "/script/eval-value", code).expect("eval-value").1;
    assert_eq!(
        body("rerics.parallel((x) => x * 2, 21)").trim(),
        "\"42\"",
        "1 件の並列実行が引数付きで結果を返す"
    );
    assert_eq!(
        body(r#"Promise.all([1,2,3,4].map((n) => rerics.parallel((x) => x * x, n))).then((a) => a.join(","))"#)
            .trim(),
        "\"1,4,9,16\"",
        "Promise.all で複数ワーカーの結果が全件集まる"
    );
}

/// 並列 op：ワーカースレッドからのホスト呼び出し（`rerics.log`）が UI スレッドへマーシャルされ、
/// 同時に戻り値もメインへ返る（別スレッド→UI スレッドの往復が成立する）。
#[test]
fn parallel_worker_can_call_host_and_return() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"(async () => {
                 const r = await rerics.parallel((n) => { rerics.log("WORKER-LOG-" + n); return n + 1; }, 41);
                 rerics.log("MAIN-GOT-" + r);
               })();"#,
        )
        .expect("eval");
    let log = poll(&server, "/state/log", |b| b.contains("MAIN-GOT-"));
    assert!(log.contains("WORKER-LOG-41"), "ワーカーからの log が UI へ届くはず: {log}");
    assert!(log.contains("MAIN-GOT-42"), "ワーカーの戻り値がメインへ返るはず: {log}");
}

/// ログ行ハンドル：`r.log` が返すハンドルの `update` が、その行をインプレースで書き換える。
/// 追記ではなく書き換えなので、更新後は元の本文が消えて新しい本文だけが残る。
#[test]
fn log_line_handle_update_rewrites_in_place() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"rerics.log("LOGLINE-before").update("LOGLINE-after");"#,
        )
        .expect("eval");
    let log = poll(&server, "/state/log", |b| b.contains("LOGLINE-after"));
    assert!(!log.contains("LOGLINE-before"), "同じ行が書き換わり元の本文は残らない: {log}");
}

/// ログは投げっぱなしで流すが、`getLog` は読む前にログレーンを汲み切るので、直前の `log` を
/// 確実に読み戻せる（read-your-writes）。汲み切らないと `RYW-MISS` になる。
#[test]
fn get_log_reads_back_just_written_lines() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"
                rerics.log("RYW-a");
                rerics.log("RYW-b");
                const seen = rerics.getLog();
                const ok = seen.includes("RYW-a") && seen.includes("RYW-b");
                rerics.log(ok ? "RYW-OK" : "RYW-MISS");
            "#,
        )
        .expect("eval");
    let log = poll(&server, "/state/log", |b| {
        b.contains("RYW-OK") || b.contains("RYW-MISS")
    });
    assert!(log.contains("RYW-OK"), "getLog が直前の追記を読み戻すはず: {log}");
}

/// 追記を投げっぱなし化しても連射で行が落ちない。`/state/log` は可視ウィンドウしか返さないので、
/// 全文を `getLog` で取り、先頭行と末尾行が両方そろっていることを確認する。
#[test]
fn rapid_log_appends_keep_all_lines() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"
                for (let i = 0; i < 100; i++) rerics.log("burst-" + i);
                const all = rerics.getLog();
                const ok = all.includes("burst-0\r\n") && all.includes("burst-99\r\n");
                rerics.log(ok ? "BURST-OK" : "BURST-MISS");
            "#,
        )
        .expect("eval");
    let log = poll(&server, "/state/log", |b| {
        b.contains("BURST-OK") || b.contains("BURST-MISS")
    });
    assert!(log.contains("BURST-OK"), "連射した先頭・末尾の行が全文に残るはず: {log}");
}

/// ログ行の進行表示：`startProgress`/`setProgress` が `/state/log` の progress に出て、`stopProgress`
/// で消える。走行中ずっと観測できるよう、重い `parallel` の await で持続させる。
#[test]
fn log_line_progress_shows_percent_then_clears() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"(async () => {
                const line = rerics.log("PROG-LINE");
                line.startProgress();
                line.setProgress(3, 4);
                await rerics.parallel(() => {
                    let x = 0;
                    for (let i = 0; i < 600000000; i++) x += i;
                    return x;
                });
                line.stopProgress();
                rerics.log("PROG-DONE");
            })();"#,
        )
        .expect("eval");
    // 走行中：進捗比 75% が観測できる。
    let during = poll(&server, "/state/log", |b| b.contains("\"percent\":75"));
    assert!(during.contains("\"percent\":75"), "進行中は 75% が出るはず: {during}");
    // 完了後：stopProgress で progress が空になる。
    let after = poll(&server, "/state/log", |b| {
        b.contains("PROG-DONE") && b.contains("\"progress\":[]")
    });
    assert!(after.contains("\"progress\":[]"), "stopProgress で進行表示が消えるはず: {after}");
}

/// 進行表示の保険：`stopProgress` を呼ばずスクリプトが終わっても、`ScriptEnd` で自動的に消える。
#[test]
fn log_line_progress_auto_clears_on_script_end() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req(
            "POST",
            "/script/eval",
            r#"(async () => {
                const line = rerics.log("AUTO-LINE");
                line.startProgress();
                line.setProgress(1, 2);
                await rerics.parallel(() => {
                    let x = 0;
                    for (let i = 0; i < 600000000; i++) x += i;
                    return x;
                });
                rerics.log("AUTO-DONE");
            })();"#,
        )
        .expect("eval");
    // 走行中：50% が観測できる（startProgress は届いている）。
    let during = poll(&server, "/state/log", |b| b.contains("\"percent\":50"));
    assert!(during.contains("\"percent\":50"), "進行中は 50% が出るはず: {during}");
    // 完了後：stopProgress なしでも自動で空になる。
    let after = poll(&server, "/state/log", |b| {
        b.contains("AUTO-DONE") && b.contains("\"progress\":[]")
    });
    assert!(after.contains("\"progress\":[]"), "ScriptEnd で進行表示が自動で消えるはず: {after}");
}

/// キーバインド経路：登録コマンドの呼び出し式（`r.goUp()`）が `exec` からエンジンへ流れ、実行される。
/// 登録コマンドがアクティブペインを移動させ、UI に反映されることで配線を検証する。
#[test]
fn exec_invokes_registered_command() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00-cmds.ts",
            r#"rerics.registerCommand("goUp", () => { rerics.navigate(rerics.currentDir() + "/.."); });"#,
        )],
    );
    let loc0 = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert!(loc0.contains("sbx"), "サンドボックスから開始するはず: {loc0}");

    server.req("POST", "/exec", "r.goUp()").expect("exec");
    let loc1 = poll(&server, "/state/panes/left/location", |b| !b.contains("sbx"));
    assert!(
        !loc1.contains("sbx"),
        "登録コマンドの呼び出し式が実行されてペインが移動するはず: {loc1}"
    );
}

/// 計算引数：機能欄のスクリプト式が組込を `r.` のネスト呼びで包み、引数を式の値で渡せる。
/// `r.changeDirectory(r.currentDir() + "/sub")` を評価して実フォルダへ移動できる（エンジン経路）。
#[test]
fn script_expr_computes_arg_and_runs_command() {
    let server = Server::start(&["a.txt"], "");
    std::fs::create_dir_all(server.base.join("sbx").join("sub")).unwrap();
    // 式は現在地を読んで "/sub" を足す＝HostApi（currentDir）を式中から呼んで組込へ渡す。
    server
        .req("POST", "/exec", r#"r.changeDirectory(r.currentDir() + "/sub")"#)
        .expect("exec");
    let loc = poll(&server, "/state/panes/left/location", |b| b.contains("sub"));
    assert!(loc.contains("sub"), "式の値でサブフォルダへ移動するはず: {loc}");
}

/// 計算引数の核心：式が `r.prompt()` 等のモーダルを呼んでも、UI スレッドはブロックしない
/// （エンジンは別スレッド）のでデッドロックしない。プロンプトへ入れたパスへ移動できる。
#[test]
fn script_expr_with_modal_does_not_deadlock() {
    let server = Server::start(&["a.txt"], "");
    let target = server.base.join("sbx").join("target");
    std::fs::create_dir_all(&target).unwrap();
    // 式が prompt を開く。exec は即返り、モーダルを debug 駆動してパスを返す。
    server.req("POST", "/exec", r#"r.changeDirectory(r.prompt("dir?"))"#).expect("exec");
    wait_modal(&server);
    server.req("POST", "/modal/text", &target.display().to_string()).expect("text");
    server.req("POST", "/modal/key/enter", "").expect("enter");
    let loc = poll(&server, "/state/panes/left/location", |b| b.contains("target"));
    assert!(loc.contains("target"), "プロンプトのパスへ移動するはず（デッドロックしない）: {loc}");
}

/// 計算引数：式が開いたモーダルをキャンセルすると prompt は null を返し、`r.changeDirectory(null)`
/// は移動しない（無音中止）。
#[test]
fn script_expr_modal_cancel_aborts_silently() {
    let server = Server::start(&["a.txt"], "");
    // 基準点を sbx から動かしておく（移動しないことを確かめるため）。
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    server.req("POST", "/command/toParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx);
    // 式が prompt を開く→Esc でキャンセル→prompt は null→移動しない。
    server.req("POST", "/exec", r#"r.changeDirectory(r.prompt("dir?"))"#).unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/key/esc", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let after = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert_eq!(after.trim(), parent.trim(), "式のキャンセルは移動しない（空＝無音中止）");
}

/// scripting：`registerCommand` の第3引数メタ（label/genre）が `/script/commands` に乗る。
/// 設定エディタはこの一覧でスクリプト行の表示名／ジャンルを描く（presentation は snapshot で確認）。
#[test]
fn script_command_metadata_surfaces_in_listing() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00.ts",
            r#"
            rerics.registerCommand("tidyUp", () => {}, { label: "デスクトップ整理", genre: "ファイル操作" });
            rerics.registerCommand("plainOne", () => {});
            "#,
        )],
    );
    let list = poll(&server, "/script/commands", |b| b.contains("plainOne"));
    // ラベル・ジャンル付きは値が乗り、無指定は null。
    assert!(
        list.contains(r#""name":"tidyUp""#)
            && list.contains(r#""label":"デスクトップ整理""#)
            && list.contains(r#""genre":"ファイル操作""#),
        "メタ付きコマンドは label/genre が乗る: {list}"
    );
    assert!(
        list.contains(r#""name":"plainOne","label":null,"genre":null"#),
        "メタ無しは label/genre が null: {list}"
    );
}

/// コマンドパレットは登録済みスクリプトコマンドも候補に出し、確定で `script("name")` トークンを
/// 挿入する（表示はラベル＋「（スクリプト）」）。
#[test]
fn command_direct_lists_registered_script_commands() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00.ts",
            r#"rerics.registerCommand("organize", () => {}, { label: "整理する" });"#,
        )],
    );
    poll(&server, "/script/members", |b| b.contains("organize"));
    server.req("POST", "/command/commandDirect", "").expect("commandDirect");
    wait_modal(&server);

    // ラベル「整理する」で引け、候補に「整理する（スクリプト）」が出る。
    server.req("POST", "/completion/keystrokes", "整理").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("整理する（スクリプト）"));
    assert!(c.contains("整理する（スクリプト）"), "スクリプトコマンドが候補に出る: {c}");

    // 先頭候補を確定＝入力欄に r.organize() 式が入る。
    server.req("POST", "/completion/accept/0", "").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains(r#"r.organize()"#));
    assert!(c2.contains(r#"r.organize()"#), "確定で r.organize() 式が挿入される: {c2}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// コマンドパレット（commandDirect）：補完は和名でも内部名でも引け、確定した文字列を
/// `Invocation` として解釈し、キー押下と同じ経路で実行する。解釈できない文字列はログに出す。
#[test]
fn command_direct_runs_typed_command() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "");
    assert_eq!(
        server.req("GET", "/state/panes/left/cursor", "").unwrap().1.trim(),
        "0",
        "初期カーソルは 0"
    );

    // パレットを開く（MaybeModal＝応答先返しで開く）。和名「下へ」で候補が引ける。
    server.req("POST", "/command/commandDirect", "").expect("commandDirect");
    wait_modal(&server);
    server.req("POST", "/completion/keystrokes", "下へ").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("cursorDown"));
    assert!(
        c.contains("カーソルを下へ (cursorDown)"),
        "和名でコマンド名補完が引ける: {c}"
    );
    // 候補行にはメタデータ由来の引数シグネチャと説明文が添えられる。
    assert!(
        c.contains("{select?}") && c.contains("カーソルを 1 つ下の項目へ移動する"),
        "補完候補に引数ヒントと説明が出る: {c}"
    );

    // 本文を内部名 cursorDown() にして OK＝実行され、カーソルが 1 へ動く。
    server.req("POST", "/completion/type", "cursorDown()").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    assert_eq!(
        server.req("GET", "/state/panes/left/cursor", "").unwrap().1.trim(),
        "1",
        "パレットで cursorDown() を実行するとカーソルが 1 へ動く"
    );

    // 組込に簡約できない式はエンジンへ送られ、評価に失敗してログへ出る（カーソルは動かない）。
    server.req("POST", "/command/commandDirect", "").expect("CommandDirect2");
    wait_modal(&server);
    server.req("POST", "/completion/type", "ぜんぜん違う文字列").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let log = poll(&server, "/state/log/lines", |b| b.contains("エラー"));
    assert!(
        log.contains("エラー"),
        "不正な式はエンジンの評価エラーになる: {log}"
    );
    assert_eq!(
        server.req("GET", "/state/panes/left/cursor", "").unwrap().1.trim(),
        "1",
        "不正コマンドではカーソルは動かない"
    );
}

/// config の `[[menus]]` で定義した名前付きメニューを解決し（参照式サブメニュー込み）、項目を
/// 選ぶとキー押下と同じ経路で実行する。ネイティブポップアップは headless で駆動できないので、
/// `/menu/<name>` で解決済みモデルを観測し、`/menu/<name>/select/<idx>` で実行する。
#[test]
fn named_menu_resolves_and_dispatches() {
    let config = r#"
[[menus]]
name = "test"
items = [
  { label = "下へ(&D)", command = "cursorDown()" },
  { separator = true },
  { label = "サブ(&S)", command = 'menu("sub")' },
]

[[menus]]
name = "sub"
items = [
  { label = "先頭へ", command = "cursorTop()" },
]
"#;
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], config);

    // 解決済みの項目木：コマンド・セパレータ・参照式サブメニューが出る。
    let tree = server.req("GET", "/menu/test", "").unwrap().1;
    assert!(tree.contains("\"command\":\"cursorDown()\""), "コマンド項目: {tree}");
    assert!(tree.contains("\"sep\":true"), "セパレータ: {tree}");
    assert!(tree.contains("\"command\":\"cursorTop()\""), "サブメニューが展開される: {tree}");
    // サブメニュー内の項目にも深さ優先で葉インデックスが振られる。
    assert!(tree.contains("\"leaf\":1"), "サブメニューの葉も採番される: {tree}");

    // 葉 0（cursorDown）を選ぶとカーソルが 1 へ動く。
    server.req("POST", "/menu/test/select/0", "").unwrap();
    let c = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    assert_eq!(c.trim(), "1", "葉0=cursorDown でカーソルが 1 へ");

    // 葉 1（サブメニュー内の cursorTop）を選ぶとカーソルが 0 へ戻る＝サブメニュー項目も実行できる。
    server.req("POST", "/menu/test/select/1", "").unwrap();
    let c = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "0");
    assert_eq!(c.trim(), "0", "葉1=サブメニューの cursorTop でカーソルが 0 へ");

    // 未定義メニューは null。
    let unknown = server.req("GET", "/menu/nope", "").unwrap().1;
    assert_eq!(unknown.trim(), "null", "未定義メニューは null: {unknown}");
}

/// スクリプトが `registerMenu` で登録した名前付きメニューも `menu("名前")` の解決対象になる
/// （config 定義と同じレジストリへマージされる）。`/menu/<name>` で出て `select` で実行できる。
#[test]
fn named_menu_includes_script_registered() {
    let server = Server::start_with_scripts(
        &["a.txt", "b.txt", "c.txt"],
        &[(
            "00.ts",
            r#"rerics.registerMenu("scripted", [
                { label: "末尾へ", command: "cursorEnd()" },
                { label: "先頭へ", command: "cursorTop()" },
            ]);"#,
        )],
    );

    let tree = server.req("GET", "/menu/scripted", "").unwrap().1;
    assert!(tree.contains("\"command\":\"cursorEnd()\""), "登録メニューが解決される: {tree}");
    assert!(tree.contains("\"command\":\"cursorTop()\""), "2 項目目も出る: {tree}");

    // 葉 0（cursorEnd）でカーソルが末尾へ動く。
    server.req("POST", "/menu/scripted/select/0", "").unwrap();
    let moved = poll(&server, "/state/panes/left/cursor", |b| b.trim() != "0");
    assert_ne!(moved.trim(), "0", "cursorEnd で末尾へ動く");

    // 葉 1（cursorTop）でカーソルが先頭へ戻る。
    server.req("POST", "/menu/scripted/select/1", "").unwrap();
    let top = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "0");
    assert_eq!(top.trim(), "0", "cursorTop で先頭へ戻る");
}

/// 設定の「メニュー」ページでメニューの追加/選択/改名/並べ替え/削除を駆動できる。標準
/// コントロールは generic な `/modal/*` で叩けないので、専用フック `/menu-editor/*` で観測・駆動する。
/// 編集は作業コピー（Shared.cfg）に対してで、OK を押すまで実 config には触れない。
#[test]
fn menu_editor_drives_menu_crud() {
    let config = r#"
[[menus]]
name = "alpha"
items = [ { label = "コピー", command = "copy" } ]

[[menus]]
name = "beta"
items = []
"#;
    let server = Server::start(&["a.txt"], config);
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    // 設定が開いてメニュー編集フックが登録されるまで待つ。
    let s0 = poll(&server, "/menu-editor", |b| b.contains("\"name\":\"alpha\""));
    assert!(s0.contains("\"name\":\"beta\""), "初期状態に config の2メニュー: {s0}");
    assert!(s0.contains("\"selected_menu\":null"), "初期は未選択: {s0}");

    // 追加：末尾に新メニューが付き、それが選択される。
    let s = server.req("POST", "/menu-editor/add", "gamma").unwrap().1;
    assert!(s.contains("\"name\":\"gamma\""), "追加された: {s}");
    assert!(s.contains("\"selected_menu\":2"), "追加分が選択される: {s}");

    // 改名：選択中（gamma）を改名する。
    let s = server.req("POST", "/menu-editor/rename", "gamma2").unwrap().1;
    assert!(s.contains("\"name\":\"gamma2\"") && !s.contains("\"name\":\"gamma\""), "改名: {s}");

    // 並べ替え：上へ動かすと index 1 へ。
    let s = server.req("POST", "/menu-editor/move/-1", "").unwrap().1;
    assert!(s.contains("\"selected_menu\":1"), "上へ移動で index 1: {s}");

    // 削除：選択中（gamma2）が消える。
    let s = server.req("POST", "/menu-editor/delete", "").unwrap().1;
    assert!(!s.contains("gamma2"), "削除された: {s}");
    assert!(s.contains("\"name\":\"alpha\"") && s.contains("\"name\":\"beta\""), "他は残る: {s}");
}

/// メニュー編集を OK で確定すると、`config.toml` へ保存される（ライブ反映＋ディスク永続化）。
/// 設定ダイアログの OK は arm 登録済みなので `/modal/command/ok` で押せる。
#[test]
fn menu_editor_persists_to_config_on_ok() {
    let config = r#"
[[menus]]
name = "alpha"
items = [ { label = "コピー", command = "copy" } ]
"#;
    let server = Server::start(&["a.txt"], config);
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    poll(&server, "/menu-editor", |b| b.contains("\"name\":\"alpha\""));

    // メニューを足して OK で確定（ライブ反映＋config.toml へ保存）。
    server.req("POST", "/menu-editor/add", "gamma").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    // 設定モーダルが閉じるまで待つ（OK 処理＝検証→反映→保存→close が走り切る）。
    poll(&server, "/state/modal", |b| b.trim() == "null");

    // config.toml に追加メニューが書き出され、既存も残っている。
    let cfg_path = server.base.join("data").join("config.toml");
    let mut saved = String::new();
    for _ in 0..50 {
        saved = std::fs::read_to_string(&cfg_path).unwrap_or_default();
        if saved.contains("gamma") {
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(saved.contains("name = \"gamma\""), "追加メニューが保存される: {saved}");
    assert!(saved.contains("name = \"alpha\""), "既存メニューも残る: {saved}");
}

/// メニュー名は重複させず、同名を足すと末尾へ ` (2)`, ` (3)` … が自動で付く。改名で他メニュー
/// 名にぶつけても同様に一意化される（`MenuRegistry` は同名後勝ちなので埋もれを防ぐ）。
#[test]
fn menu_editor_dedupes_menu_names() {
    let config = r#"
[[menus]]
name = "alpha"
items = []

[[menus]]
name = "beta"
items = []
"#;
    let server = Server::start(&["a.txt"], config);
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    poll(&server, "/menu-editor", |b| b.contains("\"name\":\"alpha\""));

    // 同名 alpha を足すと alpha (2)。
    let s = server.req("POST", "/menu-editor/add", "alpha").unwrap().1;
    assert!(s.contains("\"name\":\"alpha (2)\""), "同名は (2): {s}");

    // 改名で beta を alpha へぶつけると、alpha・alpha (2) を避けて alpha (3)。
    server.req("POST", "/menu-editor/select/1", "").unwrap();
    let s = server.req("POST", "/menu-editor/rename", "alpha").unwrap().1;
    assert!(s.contains("\"name\":\"alpha (3)\""), "改名も一意化: {s}");
    assert!(!s.contains("\"name\":\"beta\""), "beta は消える: {s}");
}

/// 設定の「メニュー」ページで、選択中メニューの項目（ラベル/コマンド/セパレータ）を
/// 追加/選択/更新/並べ替え/削除できる。項目操作 body は `{label,command,separator}` JSON。
#[test]
fn menu_editor_drives_item_crud() {
    let config = r#"
[[menus]]
name = "alpha"
items = [ { label = "コピー", command = "copy" } ]
"#;
    let server = Server::start(&["a.txt"], config);
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    poll(&server, "/menu-editor", |b| b.contains("\"name\":\"alpha\""));

    // 左メニューを選ぶ（項目操作は選択中メニューに対して行う）。
    let s = server.req("POST", "/menu-editor/select/0", "").unwrap().1;
    assert!(s.contains("\"selected_menu\":0"), "メニュー選択: {s}");
    assert!(s.contains("\"selected_item\":null"), "項目は未選択: {s}");

    // 項目追加：末尾に付き、それが選択される。
    let s = server
        .req("POST", "/menu-editor/item-add", r#"{"label":"切り取り","command":"Cut"}"#)
        .unwrap()
        .1;
    assert!(s.contains("\"label\":\"切り取り\"") && s.contains("\"command\":\"Cut\""), "追加: {s}");
    assert!(s.contains("\"selected_item\":1"), "追加分が選択される: {s}");

    // セパレータ追加：区切り線が末尾に付く。
    let s = server.req("POST", "/menu-editor/item-add", r#"{"separator":true}"#).unwrap().1;
    assert!(s.contains("\"separator\":true"), "セパレータ追加: {s}");
    assert!(s.contains("\"selected_item\":2"), "セパレータが選択される: {s}");

    // 項目選択：先頭（コピー）を選び直す。
    let s = server.req("POST", "/menu-editor/item-select/0", "").unwrap().1;
    assert!(s.contains("\"selected_item\":0"), "先頭を選択: {s}");

    // 項目更新：選択中（コピー）を別コマンドへ。
    let s = server
        .req("POST", "/menu-editor/item-update", r#"{"label":"複製","command":"Duplicate"}"#)
        .unwrap()
        .1;
    assert!(s.contains("\"label\":\"複製\"") && s.contains("\"command\":\"Duplicate\""), "更新: {s}");
    assert!(!s.contains("\"label\":\"コピー\""), "旧ラベルは消える: {s}");

    // 並べ替え：下へ動かすと index 1 へ。
    let s = server.req("POST", "/menu-editor/item-move/1", "").unwrap().1;
    assert!(s.contains("\"selected_item\":1"), "下へ移動で index 1: {s}");

    // 項目削除：選択中（複製）が消える。
    let s = server.req("POST", "/menu-editor/item-delete", "").unwrap().1;
    assert!(!s.contains("\"label\":\"複製\""), "削除された: {s}");
    assert!(s.contains("\"label\":\"切り取り\"") && s.contains("\"separator\":true"), "他は残る: {s}");
}

/// 設定の「メニュー」ページのコマンド欄を、補完つきコードエディタ（code_box）で編集する。
/// 編集モーダルは閉じるまでブロックするので item-pick は respond-first。組込メンバの補完
/// （引数ヒント＋説明）が出る＝キー編集と同じ統一形。式を打って OK するとコマンド下書き欄へ
/// 書き戻る。ネストモーダルは既存の `/modal/*`・`/completion/*` で観測・駆動する。
#[test]
fn menu_editor_command_editor_writes_expr() {
    let config = r#"
[[menus]]
name = "alpha"
items = [ { label = "コピー", command = "copy" } ]
"#;
    let server = Server::start(&["a.txt"], config);
    server.req("POST", "/command/openSettings", "").expect("openSettings");
    poll(&server, "/menu-editor", |b| b.contains("\"name\":\"alpha\""));

    // メニュー・項目を選ぶと、その項目のコマンドが下書き欄へ載る。
    server.req("POST", "/menu-editor/select/0", "").unwrap();
    let s = server.req("POST", "/menu-editor/item-select/0", "").unwrap().1;
    assert!(s.contains("\"command\":\"copy\""), "選択項目が下書き欄へ: {s}");

    // コードエディタを開く（respond-first）。最前面モーダルが補完つきコード入力になる。
    let r = server.req("POST", "/menu-editor/item-pick", "").unwrap().1;
    assert!(r.contains("modal_opening"), "respond-first: {r}");
    poll(&server, "/state", |b| b.contains("コードを割り当て"));

    // 組込メンバがメニュー欄の補完にも引数ヒント＋説明つきで出る（機能欄＝コードの統一）。
    server.req("POST", "/modal/text", "").unwrap();
    server.req("POST", "/completion/keystrokes", "=r.cursorDow").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("cursorDown"));
    assert!(c.contains("{select?}"), "メニュー欄でも組込の引数ヒントが出る: {c}");

    // 式を打って OK＝コマンド下書き欄へ書き戻る。メニュー項目自体は OK 前なのでまだ copy のまま。
    server.req("POST", "/modal/text", "delete()").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let s = poll(&server, "/menu-editor", |b| b.contains("\"command\":\"delete()\""));
    assert!(s.contains("\"command\":\"copy\""), "項目はまだ copy のまま: {s}");
}

/// 多数行のリスト選択モーダルに `/modal/wheel/<delta>` でホイールを送ると、先頭表示行
/// （`/state` の `modal.top`）が動く。`WS_VSCROLL` 付きリストのネイティブスクロールを headless で
/// 駆動・観測する経路。題材は「キー割り当て」一覧（keyBindsDialog＝既定で多数行の list_box）。
#[test]
fn modal_wheel_scrolls_list() {
    let server = Server::start(&["a.txt"], "");
    // 多数行の list_box（現在のキー割り当て一覧）を開く（respond-first）。
    server.req("POST", "/command/keyBindsDialog", "").unwrap();
    poll(&server, "/state", |b| b.contains("キー割り当て"));

    let top = |s: &Server| -> u64 {
        let st = s.req("GET", "/state", "").unwrap().1;
        let v: serde_json::Value = serde_json::from_str(&st).unwrap();
        v["modal"]["top"].as_u64().expect("modal.top")
    };
    assert_eq!(top(&server), 0, "初期は先頭");

    // 下へ回すと先頭行が進む。
    server.req("POST", "/modal/wheel/-240", "").unwrap();
    let down = top(&server);
    assert!(down > 0, "下スクロールで先頭行が進む: {down}");

    // 上へ大きく回すと先頭へ戻る（クランプ）。
    server.req("POST", "/modal/wheel/2400", "").unwrap();
    assert_eq!(top(&server), 0, "上スクロールで先頭へ戻る");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// メニュー項目に登録スクリプトの呼び出し式（`r.名前(引数)`）を書くと、選んだとき登録スクリプトが
/// 引数ごと実行される（スクリプト連携メニュー・引数転送つき）。
#[test]
fn menu_script_token_runs_registered_script_with_args() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00.ts",
            r#"
            rerics.registerCommand("ping", (msg) => rerics.log("PONG:" + msg));
            rerics.registerMenu("fns", [{ label: "ピング", command: 'r.ping("hi")' }]);
            "#,
        )],
    );

    // 項目を選ぶと登録スクリプトが引数つきで走る（ログに出る）。
    server.req("POST", "/menu/fns/select/0", "").unwrap();
    let log = poll(&server, "/state/log/lines", |b| b.contains("PONG:hi"));
    assert!(log.contains("PONG:hi"), "script 経由でスクリプトが引数つきで実行される: {log}");
}

/// GET /meta/<token> は組込コマンドのメタデータ（説明・引数仕様・例・有効文脈）を返す。未知は 404。
#[test]
fn command_meta_endpoint_reports_args_examples_and_contexts() {
    let server = Server::start(&["a.txt"], "");

    // 引数を取るコマンド：cursorDown は select オプションと例を持つ。
    let (st, body) = server.req("GET", "/meta/cursorDown", "").expect("meta");
    assert_eq!(st, 200, "既知トークンは 200");
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["token"], "cursorDown");
    assert_eq!(v["contexts"][0], "filer");
    assert_eq!(v["args"][0]["type"]["kind"], "options");
    assert_eq!(v["args"][0]["type"]["options"][0]["name"], "select");
    assert_eq!(v["args"][0]["type"]["options"][0]["type"]["kind"], "bool");
    assert!(
        v["examples"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "cursorDown({select:true})"),
        "select の例が出る: {body}"
    );

    // enum 引数：sort。
    let sort: serde_json::Value =
        serde_json::from_str(&server.req("GET", "/meta/sort", "").unwrap().1).unwrap();
    assert_eq!(sort["args"][0]["type"]["kind"], "enum");
    assert!(
        sort["args"][0]["type"]["values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "name"),
        "sort の種別に name が出る"
    );

    // 引数なしコマンドは args 空・display は短い機能名・summary は詳しい説明文。
    let copy: serde_json::Value =
        serde_json::from_str(&server.req("GET", "/meta/copy", "").unwrap().1).unwrap();
    assert_eq!(copy["args"].as_array().unwrap().len(), 0);
    assert_eq!(copy["display"], "コピー");
    let summary = copy["summary"].as_str().unwrap();
    assert!(summary.contains("コピー") && summary.len() > "コピー".len(), "詳しい説明: {summary}");

    // 別名トークンも解決する（CD → changeDirectory）。
    let cd: serde_json::Value =
        serde_json::from_str(&server.req("GET", "/meta/CD", "").unwrap().1).unwrap();
    assert_eq!(cd["token"], "changeDirectory");

    // 未知トークンは 404。
    assert_eq!(
        server.req("GET", "/meta/noSuchCommand", "").unwrap().0,
        404,
        "未知トークンは 404"
    );
}

/// GET /help はコマンドリファレンス HTML を返す。組込（説明・例つき）と登録スクリプトが同じ表形式で
/// 並び、各コマンドに標準キーと現在キーを併記する（`openHelp` がブラウザで開くのと同じ生成物）。
#[test]
fn help_endpoint_returns_command_reference() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00.ts",
            r#"rerics.registerCommand("organize", () => {}, { summary: "散らかりを整える" });"#,
        )],
    );
    poll(&server, "/script/commands", |b| b.contains("organize"));

    let (st, html) = server.req("GET", "/help", "").expect("help");
    assert_eq!(st, 200, "/help は 200");
    assert!(html.contains("<title>Rerics コマンドリファレンス</title>"), "HTML ヘルプである");
    // 組込：トークンと使用例。
    assert!(html.contains("cursorDown") && html.contains("cursorDown({select:true})"), "組込＋例");
    // 値返しクエリも並ぶ。
    assert!(html.contains("markedCount"), "クエリ組込");
    // 標準キー／現在キーの両列。
    assert!(html.contains("標準キー") && html.contains("現在キー"), "両キー列");
    // スクリプトも同じ表形式で並ぶ＝組込/スクリプトの統一。
    assert!(html.contains("organize") && html.contains("散らかりを整える"), "スクリプトも同表に");
}

/// ディレクトリ比較：左右に異なる内容のディレクトリを置き、`directoryCompare` で差分が
/// 結果一覧（find_result＝true・情報列つき）に出る。さらに結果項目を開くと出自へ移動して
/// 結果モードを抜ける。
#[test]
fn directory_compare_shows_diff_result_pane() {
    // 左＝base/left、右＝base/right。共通名サイズ違い・左のみ・右のみを仕込む。
    let server = Server::start_dirs(
        &[("common.txt", b"aaaa"), ("only_left.txt", b"x")],
        &[("common.txt", b"b"), ("only_right.txt", b"y")],
    );

    // 比較実行（ワーカー→結果ペインへライブ追加）。結果モードに入り、期待項目が
    // 出揃うまで待つ（項目は1件ずつ流れてくるため、件数の確定を待つ）。
    server.req("POST", "/command/directoryCompare", "").unwrap();
    let body = poll(&server, "/state", |b| {
        serde_json::from_str::<serde_json::Value>(b)
            .ok()
            .map(|v| {
                let p = &v["panes"]["left"];
                p["find_result"].as_bool().unwrap_or(false)
                    && p["items"]
                        .as_array()
                        .map(|items| {
                            ["common.txt", "only_left.txt", "only_right.txt"]
                                .iter()
                                .all(|n| items.iter().any(|it| it["name"] == *n))
                        })
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let left_pane = &v["panes"]["left"];
    assert_eq!(left_pane["find_result"], true, "結果モードに入る: {body}");
    // 情報列が出る。
    let cols = left_pane["columns"].as_array().unwrap();
    assert!(
        cols.iter().any(|c| c["kind"] == "Information"),
        "情報列が並ぶ: {cols:?}"
    );

    let items = left_pane["items"].as_array().unwrap();
    let by_name = |n: &str| items.iter().find(|it| it["name"] == n);
    // 共通サイズ違い・追加・削除がそれぞれ出る。
    assert!(by_name("common.txt").is_some(), "サイズ違いの common.txt: {items:?}");
    let add = by_name("only_left.txt").expect("追加項目");
    assert_eq!(add["info"], "追加", "追加の情報列");
    assert!(add["source"].as_str().unwrap().replace('\\', "/").ends_with("/left"), "追加の出自は left: {add}");
    let del = by_name("only_right.txt").expect("削除項目");
    assert!(del["info"].as_str().unwrap().starts_with("削除:"), "削除の情報列: {del}");
    // 先頭は結果モードを抜ける ".."。
    assert_eq!(items[0]["name"], "..", "先頭は親項目");

    // 追加項目（only_left.txt）にカーソルを合わせて開く→出自 L へ移動して結果モードを抜ける。
    server
        .req("POST", "/command/setCursorPosition", "[\"only_left.txt\"]")
        .unwrap();
    server.req("POST", "/command/enterDir", "").unwrap();
    let after = poll(&server, "/state", |b| {
        serde_json::from_str::<serde_json::Value>(b)
            .ok()
            .and_then(|v| v["panes"]["left"]["find_result"].as_bool())
            .map(|fr| !fr)
            .unwrap_or(false)
    });
    let v2: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert_eq!(v2["panes"]["left"]["find_result"], false, "開くと結果モードを抜ける");
    let loc = v2["panes"]["left"]["location"].as_str().unwrap().replace('\\', "/");
    assert!(loc.ends_with("/left"), "出自 left へ移動している: {loc}");
    // カーソルは開いた名前に乗る。
    let items2 = v2["panes"]["left"]["items"].as_array().unwrap();
    let cur = items2.iter().find(|it| it["cursor"] == true).unwrap();
    assert_eq!(cur["name"], "only_left.txt", "カーソルは開いた項目に乗る");
}

/// ディレクトリ比較（条件指定）：条件ダイアログが開き、OK で既定条件の比較が走って結果ペインに
/// 差分が出る（ラジオ値そのものの選択は debug-server 未対応＝条件別ロジックは core テストで担保。
/// ここは「開く→OK→比較が走る」までを検証する）。
#[test]
fn directory_compare_dialog_opens_and_runs() {
    let server = Server::start_dirs(
        &[("only_left.txt", b"x")],
        &[("only_right.txt", b"y")],
    );

    // 条件ダイアログを開く。
    server.req("POST", "/command/directoryCompareDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("ディレクトリ比較"), "比較条件モーダルが開く: {modal}");

    // 既定条件（日付:不一致・サイズ:無視・再帰/追加/削除 on）で OK。
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");

    let body = poll(&server, "/state", |b| {
        serde_json::from_str::<serde_json::Value>(b)
            .ok()
            .map(|v| {
                let p = &v["panes"]["left"];
                p["find_result"].as_bool().unwrap_or(false)
                    && p["items"]
                        .as_array()
                        .map(|items| {
                            ["only_left.txt", "only_right.txt"]
                                .iter()
                                .all(|n| items.iter().any(|it| it["name"] == *n))
                        })
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["panes"]["left"]["find_result"], true, "OK で比較が走り結果モードへ: {body}");
    let items = v["panes"]["left"]["items"].as_array().unwrap();
    // 追加（左のみ）・削除（右のみ）が既定チェックで出る。
    assert!(items.iter().any(|it| it["name"] == "only_left.txt"), "追加項目: {items:?}");
    assert!(items.iter().any(|it| it["name"] == "only_right.txt"), "削除項目: {items:?}");
}

/// ファイル検索（引数版）：マスクで現在地以下を再帰検索し、一致ファイルが結果ペインに
/// 相対サブパス付きで出る。非一致は出ない。
#[test]
fn find_file_command_lists_matches_recursively() {
    let server = Server::start(&["a.txt", "b.log"], "");
    let sub = server.base.join("sbx").join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("c.txt"), b"x").unwrap();

    server.req("POST", "/command/findFile", "[\"*.txt\"]").unwrap();
    let body = poll(&server, "/state", |b| {
        serde_json::from_str::<serde_json::Value>(b)
            .ok()
            .map(|v| {
                let p = &v["panes"]["left"];
                p["find_result"].as_bool().unwrap_or(false)
                    && p["items"]
                        .as_array()
                        .map(|items| {
                            ["a.txt", "c.txt"]
                                .iter()
                                .all(|n| items.iter().any(|it| it["name"] == *n))
                        })
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let items = v["panes"]["left"]["items"].as_array().unwrap();
    let by_name = |n: &str| items.iter().find(|it| it["name"] == n);
    assert!(by_name("a.txt").is_some(), "直下の a.txt: {items:?}");
    let c = by_name("c.txt").expect("サブdir の c.txt");
    assert_eq!(c["info"], "sub", "相対サブパスが info に出る");
    assert!(by_name("b.log").is_none(), "非一致は出ない: {items:?}");
    // 情報列が並ぶ。
    let cols = v["panes"]["left"]["columns"].as_array().unwrap();
    assert!(cols.iter().any(|c| c["kind"] == "Information"), "情報列: {cols:?}");
}

/// ファイル検索（条件指定）：条件ダイアログが開いて閉じられる（名前/日付/サイズ入力欄の
/// 個別駆動は debug-server 未対応＝検索ロジックは引数版コマンドと core テストで担保）。
#[test]
fn find_file_dialog_opens_and_cancels() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/findFileDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("ファイル検索"), "検索条件モーダルが開く: {modal}");
    // ファイル名マスクは既定 `*` で開く（前回値が無い初回）。
    let input = poll(&server, "/state/modal/input", |b| b.trim() == "\"*\"");
    assert_eq!(input.trim(), "\"*\"", "name mask defaults to *: {input}");
    server.req("POST", "/modal/command/cancel", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// マスクで選択（selectMaskDialog）は既定値 `*` で開く（全選択され上書きしやすい）。
#[test]
fn select_mask_dialog_defaults_to_star() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/selectMaskDialog", "").unwrap();
    wait_modal(&server);
    let input = poll(&server, "/state/modal/input", |b| b.trim() == "\"*\"");
    assert_eq!(input.trim(), "\"*\"", "select mask defaults to *: {input}");
    server.req("POST", "/modal/command/cancel", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// サムネイル表示切替（thumbnailMode）はアクティブペインだけの行高をサムネイルサイズへ
/// 広げ、再度の切替で元へ戻す。反対ペインは独立して通常表示のまま。
#[test]
fn thumbnail_mode_toggles_active_pane_row_height() {
    let server = Server::start(&["a.txt", "pic.png"], "");

    let item_height = |side: &str| -> i32 {
        server
            .req("GET", &format!("/presentation/panes/{side}/item_height"), "")
            .expect("item_height")
            .1
            .trim()
            .parse()
            .expect("item_height int")
    };
    let thumbnail = |side: &str| -> String {
        server
            .req("GET", &format!("/presentation/panes/{side}/thumbnail"), "")
            .expect("thumbnail")
            .1
            .trim()
            .to_string()
    };

    let h0 = item_height("left");
    assert_eq!(thumbnail("left"), "false", "初期はサムネイル表示オフ");

    // アクティブ（左）ペインをサムネイル表示へ。
    server.req("POST", "/command/thumbnailMode", "").expect("thumbnailMode on");
    assert_eq!(thumbnail("left"), "true", "切替でサムネイル表示オン");
    let h1 = item_height("left");
    assert!(h1 > h0, "サムネイル表示で行高が広がる: {h0} -> {h1}");

    // 反対（右）ペインは独立＝影響を受けない。
    assert_eq!(thumbnail("right"), "false", "反対ペインは通常表示のまま");

    // もう一度切り替えると元の行高へ戻る。
    server.req("POST", "/command/thumbnailMode", "").expect("thumbnailMode off");
    assert_eq!(thumbnail("left"), "false", "再切替でサムネイル表示オフ");
    assert_eq!(item_height("left"), h0, "行高が元へ戻る");
}

/// バージョン情報（about）モーダルが、アプリ名＋バージョン、UnRAR の利用条件、そして
/// ビルド時に集めたサードパーティライセンス一覧を載せて開き、debug-server から観測・クローズ
/// できることを検証する。
#[test]
fn about_dialog_shows_version_and_licenses() {
    let server = Server::start(&["a.txt"], "");

    let (st, _) = server.req("POST", "/command/about", "").expect("about command");
    assert_eq!(st, 200, "about は読取専用モーダルなので実行できる");

    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"about\""), "about モーダルが出る: {}", &modal[..modal.len().min(200)]);
    assert!(modal.contains("Rerics について"), "ダイアログのタイトル");
    assert!(
        modal.contains(&format!("Rerics {}", env!("CARGO_PKG_VERSION"))),
        "アプリ名とバージョンが載る"
    );
    // UnRAR ライセンス第2項（配布条件として掲示が要る一文）。
    assert!(modal.contains("UnRAR source code"), "UnRAR の利用条件が載る");
    // cargo-about が集めた一覧の代表（依存に必ず居る Apache-2.0 / MIT の見出し）。
    assert!(modal.contains("Apache License"), "サードパーティライセンス一覧が載る");

    // モーダルは debug-server から閉じられる（「閉じる」ボタン＝ctrl_id 1）。
    server.req("POST", "/modal/command/ok", "").expect("close about");
    let after = poll(&server, "/state/modal", |b| b.trim() == "null");
    assert_eq!(after.trim(), "null", "閉じるボタンで閉じられる");
}

/// インクリメンタルサーチ入力欄で ↑↓ が前／次の一致へカーソルを動かし（端では折り返さない）、
/// Enter で一致位置を残して確定する。原作の手触り（Up=前・Down=次）の回帰防止。
#[test]
fn incremental_search_arrows_step_between_matches() {
    let server =
        Server::start(&["match_a.txt", "match_b.txt", "match_c.txt", "other.txt"], "");

    // items は [.., match_a, match_b, match_c, other]＝一致は index 1..=3。
    let wait_cursor = |val: &str| {
        let got = poll(&server, "/state/panes/left/cursor", |b| b.trim() == val);
        assert_eq!(got.trim(), val, "カーソルが index {val} に来る");
    };

    server
        .req("POST", "/command/incrementalSearchDialog", "")
        .expect("open incremental search");
    wait_modal(&server);

    // "match" 入力で先頭の一致（match_a＝index 1）へ追従する。
    server.req("POST", "/modal/text", "match").expect("set query");
    wait_cursor("1");

    // Down＝次の一致（b→c）、最後の一致では動かない。
    server.req("POST", "/modal/key/down", "").expect("down");
    wait_cursor("2");
    server.req("POST", "/modal/key/down", "").expect("down");
    wait_cursor("3");
    server.req("POST", "/modal/key/down", "").expect("down");
    wait_cursor("3");

    // Up＝前の一致（b→a）、先頭の一致では動かない。
    server.req("POST", "/modal/key/up", "").expect("up");
    wait_cursor("2");
    server.req("POST", "/modal/key/up", "").expect("up");
    wait_cursor("1");
    server.req("POST", "/modal/key/up", "").expect("up");
    wait_cursor("1");

    // Enter＝確定：一致位置（match_a＝1）を残してモーダルを閉じる。
    server.req("POST", "/modal/key/enter", "").expect("enter");
    let after = poll(&server, "/state/modal", |b| b.trim() == "null");
    assert_eq!(after.trim(), "null", "Enter で確定して閉じる");
    wait_cursor("1");
}

/// pageNext/pagePrevious＝タブを次/前へ巡回移動する（末尾で先頭へ・先頭で末尾へ巻き戻る＝
/// 原作 frmMain.PageNext/PagePrevious 準拠）。
#[test]
fn tab_page_next_previous_cycle() {
    let server = Server::start_writable(&["a.txt"]);
    // 開始は 1 タブ・active=0。newFiler ×2 で 3 タブにする。
    server.req("POST", "/command/newFiler", "").unwrap();
    server.req("POST", "/command/newFiler", "").unwrap();
    let count = server.req("GET", "/state/tabs/count", "").unwrap().1;
    assert_eq!(count.trim(), "3", "newFiler×2 で 3 タブ: {count}");

    let active_is = |v: &str| {
        let got = poll(&server, "/state/tabs/active", |b| b.trim() == v);
        assert_eq!(got.trim(), v, "active タブが {v} である");
    };
    active_is("2"); // 直近の新タブがアクティブ。

    // 末尾(2)から pageNext で先頭(0)へ巻き戻り、以降は前進。
    server.req("POST", "/command/pageNext", "").unwrap();
    active_is("0");
    server.req("POST", "/command/pageNext", "").unwrap();
    active_is("1");
    // 先頭(0)から pagePrevious で末尾(2)へ巻き戻る。
    server.req("POST", "/command/pagePrevious", "").unwrap();
    active_is("0");
    server.req("POST", "/command/pagePrevious", "").unwrap();
    active_is("2");
}

/// newFiler＝現在のパスを複製した新タブを「アクティブ直後」へ挿入して切り替える
/// （原作は末尾追加だが、rerics はタブモデルを刷新し複製元の隣へ挿入する意図的な差）。
#[test]
fn new_filer_inserts_tab_after_active() {
    let server = Server::start_writable(&["a.txt"]);
    let start = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();

    // 1 枚目の newFiler：2 タブ・active=1・新タブは複製元と同じパス。
    server.req("POST", "/command/newFiler", "").unwrap();
    assert_eq!(server.req("GET", "/state/tabs/count", "").unwrap().1.trim(), "2");
    assert_eq!(server.req("GET", "/state/tabs/active", "").unwrap().1.trim(), "1");
    let loc = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert_eq!(loc.trim(), start, "新タブは複製元のパスを引き継ぐ: {loc}");

    // tab0 へ戻ってから newFiler：アクティブ直後(index 1)へ挿入され active=1 になる
    // （末尾追加なら index 2 になるはず＝挿入位置の差を固定する）。
    server.req("POST", "/command/pagePrevious", "").unwrap();
    let got = poll(&server, "/state/tabs/active", |b| b.trim() == "0");
    assert_eq!(got.trim(), "0", "tab0 へ戻る");
    server.req("POST", "/command/newFiler", "").unwrap();
    assert_eq!(server.req("GET", "/state/tabs/count", "").unwrap().1.trim(), "3");
    assert_eq!(
        server.req("GET", "/state/tabs/active", "").unwrap().1.trim(),
        "1",
        "アクティブ直後へ挿入＝active は 1（末尾追加なら 2 になる）"
    );
}
