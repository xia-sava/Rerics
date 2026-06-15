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
use std::path::PathBuf;
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

        let mut child = Command::new(env!("CARGO_BIN_EXE_rerics"))
            .arg("--debug-server=0")
            .arg("--headless")
            .env("RERICS_DATA_DIR", &data)
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

/// 最小 HTTP/1.0 クライアント（依存を増やさない）。`(status, body)` を返す。
fn req(port: u16, method: &str, path: &str, body: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let head = format!(
        "{method} {path} HTTP/1.0\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).ok()?;
    s.write_all(body.as_bytes()).ok()?;
    let mut resp = String::new();
    s.read_to_string(&mut resp).ok()?;
    let status: u16 = resp.split_whitespace().nth(1)?.parse().ok()?;
    let body = resp.splitn(2, "\r\n\r\n").nth(1).unwrap_or("").to_string();
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
        .req("POST", "/command/CursorDown", "")
        .expect("CursorDown");
    let c1 = server
        .req("GET", "/state/panes/left/cursor", "")
        .expect("cursor after")
        .1;
    assert_eq!(c0.trim(), "0", "initial cursor");
    assert_eq!(c1.trim(), "1", "cursor should move to 1 after CursorDown");

    let bst = server.req("POST", "/command/Nope", "").expect("bad command").0;
    assert_eq!(bst, 400, "unknown command should be 400");

    // 書込み許可なしなのでモーダル系コマンドは 400（破壊防止のゲート）。
    let mst = server
        .req("POST", "/command/MakeDirectory", "")
        .expect("modal command")
        .0;
    assert_eq!(mst, 400, "MakeDirectory without --debug-allow-write should be 400");

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
