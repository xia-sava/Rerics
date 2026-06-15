//! デバッグ制御サーバの統合テスト（`--features debug-server` のときだけ走る）。
//!
//! 実 `rerics.exe` を `--debug-server --headless` で起動し（`RERICS_DATA_DIR` で
//! ユーザ環境から隔離）、HTTP で `/state` 観測とコマンド注入を検証する。
//! core の単体テストでは届かない「実 DPI・実入力経路・実レイアウト」を踏む唯一の自動テスト。

#![cfg(feature = "debug-server")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;

const PORT: u16 = 8799;

/// 最小 HTTP/1.0 クライアント（依存を増やさない）。`(status, body)` を返す。
fn req(method: &str, path: &str, body: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", PORT)).ok()?;
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

#[test]
fn debug_server_smoke() {
    // 隔離した data-dir とサンドボックスを用意する。
    let base = std::env::temp_dir().join(format!("rerics_it_{}", std::process::id()));
    let data = base.join("data");
    let sbx = base.join("sbx");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&sbx).unwrap();
    for n in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(sbx.join(n), b"x").unwrap();
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

    let mut child = Command::new(env!("CARGO_BIN_EXE_rerics"))
        .arg(format!("--debug-server={PORT}"))
        .arg("--headless")
        .env("RERICS_DATA_DIR", &data)
        .spawn()
        .expect("spawn rerics");

    // 起動待ち（最大 ~10 秒）。
    let mut up = false;
    for _ in 0..50 {
        if req("GET", "/state", "").is_some() {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // 観測・駆動を一通り集める（panic で子プロセスを取り残さないよう、kill 前に値を確保）。
    let state = req("GET", "/state", "");
    let loc = req("GET", "/state/panes/left/location", "");
    let cur0 = req("GET", "/state/panes/left/cursor", "");
    let _ = req("POST", "/command/CursorDown", "");
    let cur1 = req("GET", "/state/panes/left/cursor", "");
    let badcmd = req("POST", "/command/Nope", "");
    let modal_cmd = req("POST", "/command/MakeDirectory", "");

    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&base);

    // 検証。
    assert!(up, "debug server did not come up");
    let (st, body) = state.expect("GET /state");
    assert_eq!(st, 200, "GET /state status");
    assert!(body.contains("\"panes\""), "state json missing panes: {body}");
    assert!(body.contains("\"active_view\""), "state json missing active_view");

    let (_, locb) = loc.expect("GET /state/panes/left/location");
    assert!(locb.contains("sbx"), "location should be the sandbox: {locb}");

    let (_, c0) = cur0.expect("cursor before");
    let (_, c1) = cur1.expect("cursor after");
    assert_eq!(c0.trim(), "0", "initial cursor");
    assert_eq!(c1.trim(), "1", "cursor should move to 1 after CursorDown");

    let (bst, _) = badcmd.expect("bad command");
    assert_eq!(bst, 400, "unknown command should be 400");

    // 書込み許可なしなのでモーダル系コマンドは 400（破壊防止のゲート）。
    let (mst, _) = modal_cmd.expect("modal command");
    assert_eq!(mst, 400, "MakeDirectory without --debug-allow-write should be 400");
}
