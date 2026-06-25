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

/// `path` を GET し続け、`pred(body)` が真になるまで待つ（最大 ~5 秒）。ワーカ完了や
/// モーダル出現など非同期な状態変化を待つのに使う。最後に観測した body を返す。
fn poll<F: Fn(&str) -> bool>(server: &Server, path: &str, pred: F) -> String {
    let mut last = String::new();
    for _ in 0..50 {
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

/// ログのコピー／クリア（CopyLog/ClearLog）が配線され、ClearLog でログ行が消えることを検証する。
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

    // CopyLog は非モーダル・非破壊で実行できる（クリップボード内容は headless では読まない）。
    let (cst, _) = server.req("POST", "/command/CopyLog", "").expect("CopyLog");
    assert_eq!(cst, 200, "CopyLog は実行できる");

    // ClearLog でログ行が空になる。
    server.req("POST", "/command/ClearLog", "").expect("ClearLog");
    let log1 = poll(&server, "/state/log", |b| b.contains("\"lines\":[]"));
    assert!(log1.contains("\"lines\":[]"), "ClearLog でログが空になる: {log1}");
}

/// `/command` の body 引数（JSON 文字列配列）が受理され、引数を見ないコマンドでは
/// 無害に無視されること、不正な body は 400 になることを確認する（引数基盤の配線検証）。
#[test]
fn command_accepts_json_array_args() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "");

    // 引数を取らない CursorDown に引数を付けても従来どおり動く（無視される）。
    let (st, _) = server
        .req("POST", "/command/CursorDown", r#"["ignored"]"#)
        .expect("CursorDown with args");
    assert_eq!(st, 200, "command with JSON array body should be accepted");
    let c = server
        .req("GET", "/state/panes/left/cursor", "")
        .expect("cursor")
        .1;
    assert_eq!(c.trim(), "1", "cursor should still advance with args present");

    // 配列でない body は 400。
    let bad = server
        .req("POST", "/command/CursorDown", "\"notarray\"")
        .expect("bad body")
        .0;
    assert_eq!(bad, 400, "non-array JSON body should be 400");

    // 文字列でない要素を含む配列も 400。
    let bad2 = server
        .req("POST", "/command/CursorDown", "[1, 2]")
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
    // 左 items は [.., a.txt, b.txt]。CursorDown×2 で b.txt。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/Copy", "").unwrap();
    let m = server.req("GET", "/state/modal", "").unwrap().1;
    assert_eq!(m.trim(), "null", "non-colliding add must not prompt: {m}");
    let r1 = poll(&server, "/state/panes/right/items", |b| {
        b.contains("\"name\":\"b.txt\"")
    });
    assert!(r1.contains("\"name\":\"b.txt\""), "b.txt should be added to the archive: {r1}");

    // --- 衝突 replace：a.txt（実FS, AAA）は zip の a.txt と同名 → モーダル → 既定=置換。 ---
    // reload でカーソルは .. に戻る。CursorDown×1 で a.txt。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/Copy", "").unwrap();
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
    server.req("POST", "/command/FocusRight", "").unwrap();
    server.req("POST", "/command/MakeDirectory", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "newdir").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    let r = poll(&server, "/state/panes/right/items", |b| {
        b.contains("\"name\":\"newdir\"")
    });
    assert!(r.contains("\"name\":\"newdir\""), "newdir should be created in the archive: {r}");

    // 同名 mkdir はエラー（実FS のディレクトリ作成と同じ挙動）。
    server.req("POST", "/command/MakeDirectory", "").unwrap();
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
    server.req("POST", "/command/FocusLeft", "").unwrap();
    // 左 items は [.., m.txt]。CursorDown×1 で m.txt。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/Move", "").unwrap();
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
    server.req("POST", "/command/FocusRight", "").unwrap();
    // 右 items は [.., a.txt, b.txt, keep.txt]。CursorDown×1 で a.txt。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/Delete", "").unwrap();
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

/// ask_before_copy=true のとき、Copy の前に確認モーダルが出てキャンセルで中止できる。
#[test]
fn ask_before_copy_confirms() {
    let server = Server::start_writable_cfg(&["a.txt"], "[file_ops]\nask_before_copy = true\n");
    // 左 items は [.., a.txt]。CursorDown×1 で a.txt にカーソル。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/Copy", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"message\""), "copy should confirm first: {modal}");
    assert!(modal.contains("コピー"), "confirm dialog titled コピー: {modal}");
    // キャンセルで中止＝ファイルはそのまま残る。
    server.req("POST", "/modal/command/cancel", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let items = server.req("GET", "/state/panes/left/items", "").expect("items").1;
    assert!(items.contains("\"name\":\"a.txt\""), "cancel leaves the file: {items}");
}

/// ask_before_delete=false のとき、Delete は確認モーダルを出さず即削除する。
#[test]
fn ask_before_delete_off_skips_confirm() {
    let server = Server::start_writable_cfg(&["a.txt", "b.txt"], "[file_ops]\nask_before_delete = false\n");
    // 左 items は [.., a.txt, b.txt]。CursorDown×1 で a.txt にカーソル。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/Delete", "").unwrap();
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
    server.req("POST", "/command/CursorDown", "").unwrap(); // .. -> a.txt
    server.req("POST", "/command/Compress", "").unwrap();
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
    // a.txt と b.txt を両方マーク（Space=MarkToggle はマーク後カーソルを下へ）。
    server.req("POST", "/command/CursorDown", "").unwrap(); // .. -> a.txt
    server.req("POST", "/command/MarkToggle", "").unwrap(); // mark a.txt -> b.txt
    server.req("POST", "/command/MarkToggle", "").unwrap(); // mark b.txt
    server.req("POST", "/command/Compress", "").unwrap();
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

/// extract_create_directory=true のとき、書庫の展開先に書庫名のフォルダ（arc）が作られる。
#[test]
fn extract_create_directory_wraps_in_archive_named_dir() {
    let server = Server::start_archive_cfg(
        &[],
        &[("a.txt", b"AAA"), ("b.txt", b"BBB")],
        "[file_ops]\nextract_create_directory = true\n",
    );
    server.req("POST", "/command/FocusRight", "").unwrap(); // 書庫ペインをアクティブに
    // 右 items は [.., a.txt, b.txt]。両方マークして展開。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/MarkToggle", "").unwrap();
    server.req("POST", "/command/MarkToggle", "").unwrap();
    server.req("POST", "/command/Extract", "").unwrap();
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
    server.req("POST", "/command/FocusRight", "").unwrap();
    // 右 items は [.., a.txt, b.txt]。CursorDown×1 で a.txt。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/Rename", "").unwrap();
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
    // items は [.., b.txt, z.txt]。CursorDown×1 で b.txt。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/Rename", "").unwrap();
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

/// 非書庫の Rename は名前/属性/更新日時の専用モーダルを開く。debug-server からは
/// チェック値を操作できないので、開いて OK で閉じても対象が壊れない（デッドロックしない）
/// ことだけを担保する。属性/日時の適用ロジック自体は core 側でテスト済み。
#[test]
fn rename_meta_dialog_opens_and_closes() {
    let server = Server::start_writable(&["a.txt"]);
    // items は [.., a.txt]。CursorDown×1 で a.txt。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/Rename", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"rename\""), "should open rename meta modal: {modal}");
    // 既定値のまま OK（名前据え置き＝改名なし）。
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let items = server.req("GET", "/state/panes/left/items", "").unwrap().1;
    assert!(items.contains("\"name\":\"a.txt\""), "a.txt should still exist: {items}");
}

/// CreateFileDialog＝入力したファイル名で空ファイルを作成する。
#[test]
fn create_file_makes_empty_file() {
    let server = Server::start_writable(&["a.txt"]);

    server.req("POST", "/command/CreateFileDialog", "").unwrap();
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

/// ToRoot＝カレントのドライブルートへ移動する。
#[test]
fn nav_to_root() {
    let server = Server::start(&["a.txt"], "");
    // 開始は sbx。location は JSON 文字列 "X:\\...\\sbx"（バックスラッシュはエスケープ）。
    let before = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert!(before.contains("sbx"), "should start in the sandbox: {before}");
    let drive = before.chars().nth(1).expect("drive letter"); // 先頭の引用符の次＝ドライブ文字
    // ルートは JSON では "X:\\"（X, :, \\）。
    let expected = format!("\"{drive}:\\\\\"");

    server.req("POST", "/command/ToRoot", "").unwrap();
    let after = poll(&server, "/state/panes/left/location", |b| b.trim() == expected);
    assert_eq!(after.trim(), expected, "ToRoot should jump to the drive root");
}

/// HistoryBack/HistoryForward＝パス移動履歴を前後する。
#[test]
fn nav_history_back_forward() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    let sbx = sbx.trim().to_string();

    // 親へ移動（sbx → その親）。
    server.req("POST", "/command/ToParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx);
    let parent = parent.trim().to_string();
    assert_ne!(parent, sbx, "ToParent should leave the sandbox");

    // 戻る＝sbx へ。
    server.req("POST", "/command/HistoryBack", "").unwrap();
    let back = poll(&server, "/state/panes/left/location", |b| b.trim() == sbx);
    assert_eq!(back.trim(), sbx, "HistoryBack should return to the sandbox");

    // 進む＝親へ。
    server.req("POST", "/command/HistoryForward", "").unwrap();
    let fwd = poll(&server, "/state/panes/left/location", |b| b.trim() == parent);
    assert_eq!(fwd.trim(), parent, "HistoryForward should go back to the parent");
}

/// #67: cursor.history=false でも、戻る/進むはカーソルを元の項目へ復元する（原作準拠＝常時復元）。
#[test]
fn history_back_restores_cursor_even_with_history_off() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "[cursor]\nhistory = false\n");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();

    // items: [.., a.txt, b.txt, c.txt]。CursorDown×3 で c.txt（index 3）へ。
    for _ in 0..3 {
        server.req("POST", "/command/CursorDown", "").unwrap();
    }
    let pre = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(pre.trim(), "3", "precondition: cursor should be on c.txt (index 3): {pre}");

    // 親へ移動 → 戻る。
    server.req("POST", "/command/ToParent", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() != sbx);
    server.req("POST", "/command/HistoryBack", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() == sbx);

    // cursor.history=false でもカーソルは c.txt（index 3）へ復元される。
    let restored = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "3");
    assert_eq!(restored.trim(), "3", "HistoryBack should restore the cursor even with cursor.history off: {restored}");
}

/// PathHistoryDialog＝訪問ログ（list_box モーダル・新しい順）から選んでジャンプする。
#[test]
fn nav_path_history_dialog() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    let sbx_raw = sbx.trim_matches('"').replace("\\\\", "\\");

    // 親へ移動（履歴に親）→ パス入力で sbx へ戻る（履歴に sbx）。訪問ログは [親, sbx]。
    server.req("POST", "/command/ToParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx).trim().to_string();
    assert_ne!(parent, sbx, "ToParent should leave the sandbox");
    server.req("POST", "/command/ChangeDirectoryDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", &sbx_raw).unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() == sbx);

    // 履歴ダイアログを開く（新しい順＝[sbx, 親]）。訪問した sbx が一覧に出る。
    server.req("POST", "/command/PathHistoryDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"list\""), "should open a list modal: {modal}");
    assert!(modal.contains("sbx"), "visited sbx should be listed: {modal}");

    // 現在地でない親（index 1）を選んで OK＝そこへジャンプ。
    server.req("POST", "/modal/select/1", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let now = poll(&server, "/state/panes/left/location", |b| b.trim() == parent);
    assert_eq!(now.trim(), parent, "selecting the parent entry should navigate there");
}

/// ChangeDirectoryDialog＝パスを入力してそこへ移動する（input_box モーダル）。
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
    server.req("POST", "/command/ToParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx_json);
    assert_ne!(parent.trim(), sbx_json, "ToParent should leave the sandbox");

    server.req("POST", "/command/ChangeDirectoryDialog", "").unwrap();
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

    server.req("POST", "/command/ChangeDirectoryDialog", "").unwrap();
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
/// PathHistoryDialog に新しい順で出る。さらに history.toml の pathhistory バケツへ永続する。
#[test]
fn path_history_records_and_persists() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    let sbx_raw = sbx_json.trim_matches('"').replace("\\\\", "\\");

    // 親へ移動（履歴に親が入る）→ パス入力で sbx へ戻る（履歴に sbx が入る）。
    server.req("POST", "/command/ToParent", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() != sbx_json);
    server.req("POST", "/command/ChangeDirectoryDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", &sbx_raw).unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() == sbx_json);

    // PathHistoryDialog：訪問した sbx が一覧に出る（新しい順の先頭）。
    server.req("POST", "/command/PathHistoryDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"list\""), "path history should open a list modal: {modal}");
    assert!(modal.contains("sbx"), "visited sbx should be listed: {modal}");
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

/// #66: 戻る/進む（履歴の再生）は訪問ログに新たな記録を増やさない（往復で増殖しない）。
#[test]
fn path_history_back_forward_does_not_grow_log() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();

    // 親へ一度移動して back/forward の素地を作る（ここで親と…は記録される）。
    server.req("POST", "/command/ToParent", "").unwrap();
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
        server.req("POST", "/command/HistoryBack", "").unwrap();
        server.req("POST", "/command/HistoryForward", "").unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    let after = count_paths(&read_hist());
    assert_eq!(after, before, "back/forward should not add path-history entries: before={before} after={after}");
}

/// 入力履歴（D2-1）：ChangeDirectory で打った値が history.toml の "changedir" バケツに永続する。
/// 入力欄が履歴コンボへ変わっても `/modal/text`（コンボ内 Edit）で打てることも兼ねて確認する。
#[test]
fn input_history_changedir_persists() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    let sbx_raw = sbx_json.trim_matches('"').replace("\\\\", "\\");

    server.req("POST", "/command/ChangeDirectoryDialog", "").unwrap();
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

/// リテラル引数版 `Sort("size")` がソート種別を切り替える（段階3＝リテラル引数コマンド）。
#[test]
fn sort_command_changes_sort_type() {
    let server = Server::start(&["a.txt", "b.txt"], "");
    // 既定は名前順。
    let before = server.req("GET", "/state/panes/left/sort/type", "").unwrap().1;
    assert_eq!(before.trim(), "\"FileName\"", "default sort should be FileName");

    server.req("POST", "/command/Sort", r#"["size"]"#).unwrap();
    let after = server.req("GET", "/state/panes/left/sort/type", "").unwrap().1;
    assert_eq!(after.trim(), "\"Length\"", "Sort(\"size\") should switch to Length");
}

/// `SetCursorPosition("c.txt")` がカーソルを指定名のファイルへ移す。
#[test]
fn set_cursor_position_jumps_to_named_file() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], "");
    server
        .req("POST", "/command/SetCursorPosition", r#"["c.txt"]"#)
        .unwrap();
    let cur = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    let cur = cur.trim();
    let name = server
        .req("GET", &format!("/state/panes/left/items/{cur}/name"), "")
        .unwrap()
        .1;
    assert_eq!(name.trim(), "\"c.txt\"", "cursor should land on c.txt");
}

/// `ChangeDrive("X:")` がアクティブペインを指定ドライブのルートへ移す。
/// サンドボックスがどのドライブにあっても動くよう、現在地のドライブ文字を使う。
#[test]
fn change_drive_navigates_to_root() {
    let server = Server::start(&["a.txt"], "");
    let loc = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    let loc_raw = loc.trim().trim_matches('"').replace("\\\\", "\\");
    let drive = &loc_raw[..1];

    server
        .req("POST", "/command/ChangeDrive", &format!(r#"["{drive}:"]"#))
        .unwrap();
    let after = poll(&server, "/state/panes/left/location", |b| {
        b.trim().trim_matches('"').replace("\\\\", "\\") != loc_raw
    });
    let after_raw = after.trim().trim_matches('"').replace("\\\\", "\\");
    assert!(
        after_raw.starts_with(&format!("{drive}:")) && after_raw.len() <= 3,
        "ChangeDrive should land on the drive root, got {after_raw}"
    );
}

/// `View`（引数なし）はファイルを内蔵ビューアで開く（EnterDir の外部起動と違う手触り）。
#[test]
fn view_command_opens_internal_viewer_for_file() {
    let server = Server::start(&["note.txt"], "");
    server
        .req("POST", "/command/SetCursorPosition", r#"["note.txt"]"#)
        .unwrap();
    // View（type なし）＝内蔵テキストビューアで開く。MaybeModal 扱いなので exec は応答後に走る。
    server.req("POST", "/command/View", "").unwrap();
    let av = poll(&server, "/state/active_view", |b| b.trim() == "\"text\"");
    assert_eq!(av.trim(), "\"text\"", "View on a text file should open the internal text viewer");
    // 閉じると元へ戻る。
    server.req("POST", "/view/key/close", "").unwrap();
    let av2 = poll(&server, "/state/active_view", |b| b.trim() == "\"none\"");
    assert_eq!(av2.trim(), "\"none\"", "closing the viewer returns to the list");
}

/// テキストビューア表示中はビューア用コマンドがビューア文脈で実行される。
#[test]
fn viewer_commands_dispatch_in_text_context() {
    let server = Server::start(&["note.txt"], "");
    server
        .req("POST", "/command/SetCursorPosition", r#"["note.txt"]"#)
        .unwrap();
    server.req("POST", "/command/View", "").unwrap();
    poll(&server, "/state/active_view", |b| b.trim() == "\"text\"");
    // バイナリ/テキスト切替は表示モードだけ変え、ビューアは開いたまま。
    let (st, _) = server
        .req("POST", "/command/ViewerToggleMode", "")
        .expect("ViewerToggleMode");
    assert_eq!(st, 200, "ViewerToggleMode はテキストビューア文脈で実行される");
    let av = server.req("GET", "/state/active_view", "").unwrap().1;
    assert_eq!(av.trim(), "\"text\"", "モード切替後もテキストビューアは開いたまま");
    // 実キー経路（キーマップ解決→コマンド実行）で Esc を送ると閉じる。
    server.req("POST", "/view/key/Esc", "").unwrap();
    let av2 = poll(&server, "/state/active_view", |b| b.trim() == "\"none\"");
    assert_eq!(av2.trim(), "\"none\"", "Esc の実キー経路で一覧へ戻る");
}

/// PathRegisterDialog で現在地を登録し、JumpDialog でそこへ戻る。
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
    server.req("POST", "/command/PathRegisterDialog", "").unwrap();
    let m = wait_modal(&server);
    assert!(m.contains("\"has_input\":true"), "register should ask for a label: {m}");
    server.req("POST", "/modal/text", "home").unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");

    // 親へ移動してから、ジャンプで登録先（sbx）へ戻る。
    server.req("POST", "/command/ToParent", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.trim() != sbx_json);

    server.req("POST", "/command/JumpDialog", "").unwrap();
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
    server.req("POST", "/command/JumpDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"jump\""), "should open the jump dialog: {modal}");
    assert!(modal.contains("ルート"), "configured label should be listed: {modal}");
    // 行の先頭列がショートカット "C"。
    assert!(modal.contains("[\"C\",\"ルート\""), "shortcut should fill the first column: {modal}");
    server.req("POST", "/modal/command/cancel", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// ChangeDriveDialog＝ドライブ一覧から選んでそのルートへ移動する。
/// 既定選択は現在ドライブなので、OK で現在ドライブのルートへ移る。
#[test]
fn nav_change_drive_dialog() {
    let server = Server::start(&["a.txt"], "");
    let before = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    let drive = before.chars().nth(1).expect("drive letter");
    let expected = format!("\"{drive}:\\\\\"");

    server.req("POST", "/command/ChangeDriveDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"drive\""), "drive dialog should open the drive selector: {modal}");
    assert!(modal.contains("\"rows\":[["), "should list at least one drive row: {modal}");
    assert!(modal.contains(&format!("\"{drive}:\"")), "should list the current drive: {modal}");

    // 既定選択（現在ドライブ）のまま OK。
    server.req("POST", "/modal/command/ok", "").unwrap();
    let after = poll(&server, "/state/panes/left/location", |b| b.trim() == expected);
    assert_eq!(after.trim(), expected, "selecting the current drive should go to its root");
}

/// KeyBindsDialog＝現在のキー割り当てをリストモーダルで読み取り専用表示する。
#[test]
fn keybinds_dialog_lists_current_bindings() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/KeyBindsDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"list\""), "should open a list modal: {modal}");
    // 既定キーの一つ（Enter→EnterDir）が一覧に出る。
    assert!(modal.contains("EnterDir"), "binding list should include EnterDir: {modal}");
    // 閉じる（選択結果は使わない）。
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// SortDialog＝ソート設定モーダルを開いて閉じる（並べ替えのみ＝allow-write 不要）。
/// ラジオ値の選択は未対応だが、開閉でデッドロックしないこと＋種別/昇降の現在値表示を担保。
#[test]
fn sort_dialog_opens_and_closes() {
    let server = Server::start(&["a.txt", "b.txt"], "");
    server.req("POST", "/command/SortDialog", "").unwrap();
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

    server.req("POST", "/command/SortDialog", "").unwrap();
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

/// IncrementalSearchDialog＝打鍵ごとにカーソルが一致項目へ追従し、OK で確定する。
#[test]
fn find_incremental_search_follows_typing() {
    let server = Server::start(&["alpha.txt", "banana.txt", "cherry.txt"], "");
    // items は [.., alpha(1), banana(2), cherry(3)]。
    server.req("POST", "/command/IncrementalSearchDialog", "").unwrap();
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
    server.req("POST", "/command/CursorDown", "").unwrap();
    let origin = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    assert_eq!(origin.trim(), "1");

    server.req("POST", "/command/IncrementalSearchDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/text", "cher").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "3");

    // 中止で origin(1) に戻る。
    server.req("POST", "/modal/command/cancel", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let c = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(c.trim(), "1", "cancel should restore the original cursor: {c}");
}

/// DirectoryInformation＝カーソル位置の使用量を計算し結果ダイアログを出す。
#[test]
fn info_directory_information() {
    let server = Server::start(&["a.txt"], "");
    // ".." から a.txt（1バイト・b"x"）へカーソルを移す。
    server.req("POST", "/command/CursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    // 計算はワーカで走り、完了後に結果モーダルが出る。
    server.req("POST", "/command/DirectoryInformation", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("ファイル"), "should show a result dialog: {modal}");
    assert!(modal.contains("1 \u{30d0}\u{30a4}\u{30c8}"), "should count 1 byte: {modal}");

    // 結果ダイアログを閉じる。
    server.req("POST", "/modal/key/enter", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// RenameSequenceDialog＝既定テンプレート（File<No:0000>.ext）で選択を連番リネームする。
#[test]
fn rename_sequence_template_default() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    // a.txt(1) と b.txt(2) をマークする（Space＝MarkToggle はマーク後に下へ）。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/MarkToggle", "").unwrap();
    server.req("POST", "/command/MarkToggle", "").unwrap();

    server.req("POST", "/command/RenameSequenceDialog", "").unwrap();
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
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/MarkToggle", "").unwrap();
    server.req("POST", "/command/MarkToggle", "").unwrap();

    server.req("POST", "/command/RenameSequenceDialog", "").unwrap();
    wait_modal(&server);
    server.req("POST", "/modal/key/tab", "").unwrap();
    server.req("POST", "/modal/key/down", "").unwrap();
    server.req("POST", "/modal/key/down", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();

    let items = poll(&server, "/state/panes/left/items", |b| b.contains("file0001.ext"));
    assert!(items.contains("\"name\":\"file0001.ext\""), "主部小文字 a.txt -> file0001.ext: {items}");
    assert!(items.contains("\"name\":\"file0002.ext\""), "主部小文字 b.txt -> file0002.ext: {items}");
}

/// SendToRecycled＝確認の上ゴミ箱へ送る（ファイルが一覧から消える）。
/// ※検証で実ゴミ箱に 1 バイトの一時ファイルが入る（無害）。
#[test]
fn shell_send_to_recycled() {
    // ソート昇順で a_del.txt が先頭ファイル（index 1）、z_keep.txt が後。
    let server = Server::start_writable(&["a_del.txt", "z_keep.txt"]);
    server.req("POST", "/command/CursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    server.req("POST", "/command/SendToRecycled", "").unwrap();
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

/// CreateShortcut＝カーソル項目を指す .lnk を同じ場所に作る。
#[test]
fn shell_create_shortcut() {
    let server = Server::start_writable(&["doc.txt"]);
    // ".." から doc.txt（唯一のファイル・index 1）へ。
    server.req("POST", "/command/CursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");

    server.req("POST", "/command/CreateShortcut", "").unwrap();
    let items = poll(&server, "/state/panes/left/items", |b| b.contains("doc.txt.lnk"));
    assert!(
        items.contains("\"name\":\"doc.txt.lnk\""),
        "a .lnk shortcut should be created next to the target: {items}"
    );
}

/// ClipCopy→（サブフォルダへ移動して）ClipPaste で実コピーされる。
/// ※検証で OS のクリップボードを上書きする（汚染許容・テスト実行時のみ）。
#[test]
fn shell_clipboard_copy_paste() {
    let server = Server::start_writable(&["file.txt"]);
    // 貼付先サブフォルダをディスクに作って一覧へ反映。
    std::fs::create_dir_all(server.base.join("sbx").join("dest")).unwrap();
    server.req("POST", "/command/Reload", "").unwrap();
    // items=[.., dest(1), file.txt(2)]。file.txt へカーソルを合わせてコピー。
    poll(&server, "/state/panes/left/items", |b| b.contains("\"name\":\"dest\""));
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/CursorDown", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    server.req("POST", "/command/ClipCopy", "").unwrap();

    // dest へ入って貼り付け。
    server.req("POST", "/command/CursorUp", "").unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    server.req("POST", "/command/EnterDir", "").unwrap();
    poll(&server, "/state/panes/left/location", |b| b.contains("dest"));
    server.req("POST", "/command/ClipPaste", "").unwrap();

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

    // 左 items は [.., a.txt, b.txt, c.txt]。CursorDown×2 で b.txt(index 2)。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/CursorDown", "").unwrap();
    let before = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(before.trim(), "2", "カーソルは b.txt(index 2) のはず");

    // F5 相当。カーソル保持なら 2 のまま、旧挙動なら 0(..) へ戻る。
    server.req("POST", "/command/Reload", "").unwrap();
    let after = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    assert_eq!(after.trim(), "2", "リロード後もカーソルは b.txt に留まるべき（先頭へ戻らない）");
}

/// CursorOpposite はアクティブペインを反対側へトグルする。
#[test]
fn cursor_opposite_toggles_active_pane() {
    let server = Server::start(&["a.txt"], "");
    let a0 = server.req("GET", "/state/active_pane", "").unwrap().1;
    assert!(a0.contains("left"), "初期は左アクティブ: {a0}");
    server.req("POST", "/command/CursorOpposite", "").unwrap();
    let a1 = poll(&server, "/state/active_pane", |b| b.contains("right"));
    assert!(a1.contains("right"), "CursorOpposite で右へ: {a1}");
    server.req("POST", "/command/CursorOpposite", "").unwrap();
    let a2 = poll(&server, "/state/active_pane", |b| b.contains("left"));
    assert!(a2.contains("left"), "もう一度で左へ戻る: {a2}");
}

/// #74: CursorToParent=on のとき、アクティブ側ペインで外向きカーソルキー（左ペインで
/// FocusLeft）が親移動になる。
#[test]
fn cursor_to_parent_navigates_on_outward_key() {
    let server = Server::start(&["a.txt"], "[cursor]\nto_parent = true\n");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    assert!(server.req("GET", "/state/active_pane", "").unwrap().1.contains("left"), "初期は左アクティブ");

    server.req("POST", "/command/FocusLeft", "").unwrap();
    let up = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx);
    assert_ne!(up.trim(), sbx, "left+FocusLeft with CursorToParent should go to parent");
    // 移動先は sbx の祖先（親）のはず。
    let parent = up.trim().trim_matches('"');
    let sbx_raw = sbx.trim_matches('"');
    assert!(sbx_raw.starts_with(parent), "moved location should be an ancestor of sbx: {up} / {sbx}");
}

/// #74: CursorToParent=off（既定）では FocusLeft は親移動せず、フォーカス移動のみ。
#[test]
fn cursor_to_parent_off_keeps_focus_only() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    server.req("POST", "/command/FocusLeft", "").unwrap();
    std::thread::sleep(Duration::from_millis(250));
    let loc = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert_eq!(loc.trim(), sbx, "off: FocusLeft must not navigate to parent");
}

/// SelectFile はカーソル位置を（トグルでなく）マークし、カーソルを1つ下げる。
#[test]
fn select_file_marks_current_and_advances() {
    let server = Server::start(&["alpha.txt", "beta.txt"], "");
    // .. → alpha(index 1) へ。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/SelectFile", "").unwrap();
    // カーソルは beta(index 2) へ進む。
    let cur = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    assert_eq!(cur.trim(), "2", "SelectFile 後はカーソルが1つ下へ");
    // alpha(index 1) がマークされている（JSON Pointer の配列添字で直接取る）。
    let m = server.req("GET", "/state/panes/left/items/1/marked", "").unwrap().1;
    assert_eq!(m.trim(), "true", "alpha.txt(index 1) がマークされているはず");
}

/// down_after_select=false のとき、MarkToggle はマークするがカーソルを動かさない（#58/#63）。
#[test]
fn mark_toggle_respects_down_after_select_off() {
    let server = Server::start(&["alpha.txt", "beta.txt"], "[cursor]\ndown_after_select = false\n");
    // .. → alpha(index 1) へ。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/MarkToggle", "").unwrap();
    // カーソルは alpha(index 1) のまま。
    let cur = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(cur.trim(), "1", "down_after_select=false ではカーソルは動かない");
    let m = server.req("GET", "/state/panes/left/items/1/marked", "").unwrap().1;
    assert_eq!(m.trim(), "true", "alpha.txt はマークされる");
}

/// Shift+Space=MarkToggle("-1") はマーク反転後にカーソルを1つ上へ動かす（#59）。
#[test]
fn shift_space_toggles_and_moves_up() {
    let server = Server::start(&["alpha.txt", "beta.txt", "gamma.txt"], "");
    // .. → alpha(1) → beta(2)。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/MarkToggle", r#"["-1"]"#).unwrap();
    // beta(2) がマークされ、カーソルは alpha(1) へ上がる。
    let cur = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    assert_eq!(cur.trim(), "1", "Shift+Space 後はカーソルが1つ上へ");
    let m = server.req("GET", "/state/panes/left/items/2/marked", "").unwrap().1;
    assert_eq!(m.trim(), "true", "beta.txt(index 2) がマークされる");
}

/// Shift+矢印=CursorXxx("select") はアンカーから現在位置までを範囲マークしながら移動する（#60/#208）。
#[test]
fn shift_arrow_range_selects() {
    let server = Server::start(&["a.txt", "b.txt", "c.txt", "d.txt"], "");
    // .. → a(1)。ここがアンカー。
    server.req("POST", "/command/CursorDown", "").unwrap();
    // Shift+Down ×2＝a→b→c を範囲マーク。
    server.req("POST", "/command/CursorDown", r#"["select"]"#).unwrap();
    server.req("POST", "/command/CursorDown", r#"["select"]"#).unwrap();
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
    server.req("POST", "/command/CursorUp", r#"["select"]"#).unwrap();
    poll(&server, "/state/panes/left/cursor", |b| b.trim() == "2");
    let m3 = server.req("GET", "/state/panes/left/items/3/marked", "").unwrap().1;
    assert_eq!(m3.trim(), "false", "範囲外になった c はマーク解除");
    let m1 = server.req("GET", "/state/panes/left/items/1/marked", "").unwrap().1;
    assert_eq!(m1.trim(), "true", "アンカー a は依然マーク");
}

/// Refresh / Nop は副作用なし（200 を返し状態を変えない）。
#[test]
fn refresh_and_nop_are_noops() {
    let server = Server::start(&["a.txt", "b.txt"], "");
    server.req("POST", "/command/CursorDown", "").unwrap();
    let before = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    let r = server.req("POST", "/command/Refresh", "").expect("Refresh").0;
    assert_eq!(r, 200, "Refresh は 200");
    let n = server.req("POST", "/command/Nop", "").expect("Nop").0;
    assert_eq!(n, 200, "Nop は 200");
    let after = server.req("GET", "/state/panes/left/cursor", "").unwrap().1;
    assert_eq!(before.trim(), after.trim(), "Refresh/Nop でカーソルは不変");
}

/// 生バイトで HTTP 応答を読む（PNG 等のバイナリ用。`req` は UTF-8 前提でバイナリを落とす）。
fn req_bytes(port: u16, method: &str, path: &str) -> Option<(u16, Vec<u8>)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(8))).ok();
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

/// 設定ダイアログ＝独自モーダルだが modal_registry に登録済み。OpenSettings で開き、
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
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // Script 系に絞ると aaaScript・zzzScript が名前順（aaa が先・zzz が後）に並ぶ。
    server.req("POST", "/keys/filer/search", "Script").unwrap();
    let before = keys();
    assert!(before.contains(r#""rows":[["Script",[]],["Script",[]]]"#), "未割当 2 行: {before}");

    // 2 番目（zzzScript）へキーを割り当てても、行は 2 番目のまま動かない。
    server.req("POST", "/keys/filer/select/1", "").unwrap();
    server.req("POST", "/keys/filer/capture", "Ctrl+Alt+Z").unwrap();
    server.req("POST", "/keys/filer/search", "Script").unwrap();
    let after = keys();
    assert!(
        after.contains(r#""rows":[["Script",[]],["Script",["Ctrl+Alt+Z"]]]"#),
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
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
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
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
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
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
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
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 既定：MakeDirectory=K、SelectMask=未割当、衝突なし。
    let s = keys();
    assert!(s.contains(r#"["MakeDirectory",["K"]]"#), "既定 MakeDirectory=K: {s}");
    assert!(s.contains(r#"["SelectMask",[]]"#), "既定 SelectMask 未割当: {s}");
    assert!(s.contains(r#""conflicts":[]"#), "既定は衝突なし: {s}");

    // 未使用キーを割り当て（実打鍵キャプチャと同じ assign 経路・衝突なし）。
    assert_eq!(
        server.req("POST", "/keys/filer/bind", r#"["SelectMask","Ctrl+Shift+M"]"#).unwrap().0,
        200,
        "bind は ok"
    );
    let s = keys();
    assert!(s.contains(r#"["SelectMask",["Ctrl+Shift+M"]]"#), "割り当てが反映: {s}");
    assert!(s.contains(r#""conflicts":[]"#), "未使用キーなので衝突なし: {s}");
    assert!(s.contains("を割り当てました"), "割り当て直後はメッセージが出る: {s}");

    // unbind：直前の bind で選択は SelectMask。その割り当てを解除。
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    assert!(keys().contains(r#"["SelectMask",[]]"#), "SelectMask の割り当てが消える");

    // reset：既定へ戻る（MakeDirectory=K が復活）。直後はステータスにメッセージが残る。
    server.req("POST", "/keys/filer/reset", "").unwrap();
    let s = keys();
    assert!(s.contains(r#"["MakeDirectory",["K"]]"#), "reset で既定へ");
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
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // K は既定で MakeDirectory。これを SelectMask にも割り当てる＝消えずに衝突。
    server.req("POST", "/keys/filer/bind", r#"["SelectMask","K"]"#).unwrap();
    let s = keys();
    assert!(s.contains(r#"["MakeDirectory",["K"]]"#), "MakeDirectory は K を保持: {s}");
    assert!(s.contains(r#"["SelectMask",["K"]]"#), "SelectMask も K を得る: {s}");
    assert!(
        s.contains(r#""conflicts":[["K",["MakeDirectory","SelectMask"]]]"#),
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

    // 衝突を解消：選択中（SelectMask）の割り当てを外す＝K は MakeDirectory だけに戻る。
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    let s = keys();
    assert!(s.contains(r#""conflicts":[]"#), "衝突が解消: {s}");
    assert!(s.contains(r#"["MakeDirectory",["K"]]"#), "MakeDirectory=K に戻る: {s}");

    // 解消後は OK で閉じられる。
    server.req("POST", "/modal/command/ok", "").expect("ok");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 機能順での個別削除：1 機能に複数キーがある時は 1 キー=1 行に割れる。削除したい行を選んで外す。
#[test]
fn settings_key_editor_per_chord_delete_in_command_view() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // MakeDirectory に未使用キーを足す＝1 キー=1 行なので 2 行に割れる（既定 K ＋ Ctrl+Shift+M）。
    server.req("POST", "/keys/filer/bind", r#"["MakeDirectory","Ctrl+Shift+M"]"#).unwrap();
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    let s = keys();
    assert!(
        s.contains(r#"["MakeDirectory",["Ctrl+Shift+M"]]"#)
            && s.contains(r#"["MakeDirectory",["K"]]"#),
        "MakeDirectory が 2 行に割れる: {s}"
    );

    // K の行（chord 昇順で index 1）を選んで削除＝K 行だけ消え、Ctrl+Shift+M 行が残る。
    server.req("POST", "/keys/filer/select/1", "").unwrap();
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    let s = keys();
    assert!(s.contains(r#"["MakeDirectory",["Ctrl+Shift+M"]]"#), "Ctrl+Shift+M 行が残る: {s}");
    assert!(!s.contains(r#"["MakeDirectory",["K"]]"#), "K 行は消える: {s}");

    // 残った行も削除＝未割当の空行に戻る。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    assert!(keys().contains(r#"["MakeDirectory",[]]"#), "未割当の空行に戻る: {}", keys());

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 機能順でキーを「変更（リマップ）」：選択した行のキーを新しいキーへ移し替える（旧キーは外れる・
/// 呼び出しは同じ）。実機ではキー行のダブルクリック→打鍵に対応する経路。
#[test]
fn settings_key_editor_rebinds_selected_chord() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // MakeDirectory に 2 つ目のキーを足す＝2 行に割れる（chord 昇順 [Ctrl+Shift+M, K]）。
    server.req("POST", "/keys/filer/bind", r#"["MakeDirectory","Ctrl+Shift+M"]"#).unwrap();
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    let s = keys();
    assert!(s.contains(r#"["MakeDirectory",["K"]]"#), "K 行がある: {s}");

    // K の行（index 1）を選んで Ctrl+Alt+K へ変更＝K は外れ Ctrl+Alt+K になる（Ctrl+Shift+M は残る）。
    server.req("POST", "/keys/filer/select/1", "").unwrap();
    server.req("POST", "/keys/filer/rebind", "Ctrl+Alt+K").unwrap();
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    let s = keys();
    assert!(s.contains(r#"["MakeDirectory",["Ctrl+Alt+K"]]"#), "K が Ctrl+Alt+K に移る: {s}");
    assert!(s.contains(r#"["MakeDirectory",["Ctrl+Shift+M"]]"#), "Ctrl+Shift+M は残る: {s}");
    assert!(!s.contains(r#"["MakeDirectory",["K"]]"#), "K 行は無い: {s}");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 登録済みスクリプトが機能順の「スクリプト」ジャンルに未割当行で出て、選んでキャプチャすると
/// `Script("name")` がキーへ割り当たる（debug の capture＝begin_capture→打鍵の経路）。
#[test]
fn settings_key_editor_binds_registered_script() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[("00-cmds.ts", r#"rerics.registerCommand("myScript", () => {});"#)],
    );
    // エンジンが登録を終えてから設定を開く（open_settings がその一覧を編集器へ渡す）。
    poll(&server, "/script/commands", |b| b.contains("myScript"));
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 登録スクリプトが未割当行（Script・キー無し）として出る。実呼び出し=名前で絞れる。
    server.req("POST", "/keys/filer/search", "myScript").unwrap();
    assert!(keys().contains(r#"["Script",[]]"#), "未割当の Script 行が出る: {}", keys());

    // 行を選んでキャプチャ＝Script("myScript") が Ctrl+Alt+S に割り当たる。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/capture", "Ctrl+Alt+S").unwrap();
    server.req("POST", "/keys/filer/search", "myScript").unwrap();
    assert!(
        keys().contains(r#"["Script",["Ctrl+Alt+S"]]"#),
        "Script が Ctrl+Alt+S に割り当たる: {}",
        keys()
    );

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 「コードを割り当て」＝コードを追加すると未割当（－）の `Eval` 行がスクリプトジャンルに生え、
/// 通常どおりその行を選んでキャプチャするとキーへ結ばれる。実呼び出しカラムはラッパを剥がしたコード。
#[test]
fn settings_key_editor_binds_eval_code() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // コードを追加＝未割当の Eval 行が生える（前後スペースは trim される）。
    server.req("POST", "/keys/filer/code", "  r.log(42)  ").unwrap();
    server.req("POST", "/keys/filer/search", "r.log").unwrap();
    assert!(keys().contains(r#"["Eval",[]]"#), "未割当の Eval 行が生える: {}", keys());

    // その行を選んでキャプチャ＝Eval("r.log(42)") が Ctrl+Alt+G に割り当たる。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/capture", "Ctrl+Alt+G").unwrap();
    server.req("POST", "/keys/filer/search", "r.log").unwrap();
    assert!(
        keys().contains(r#"["Eval",["Ctrl+Alt+G"]]"#),
        "Eval が Ctrl+Alt+G に割り当たる: {}",
        keys()
    );

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 「引数」＝バインド済みの組込コマンド行で引数の式を編集すると、そのキーの呼び出しがその場で
/// 差し替わる。既定 F4 の `=r.prompt(...)` を `=r.currentDir()` へ変え、OK で config.toml に残る。
#[test]
fn settings_key_editor_edits_bound_command_arg() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 既定で F4 = ChangeDirectory("=r.prompt(...)")。r.prompt で F4 の行だけに絞って選ぶ。
    server.req("POST", "/keys/filer/search", "r.prompt").unwrap();
    assert!(keys().contains(r#"["ChangeDirectory",["F4"]]"#), "F4 の ChangeDirectory 行: {}", keys());
    server.req("POST", "/keys/filer/select/0", "").unwrap();

    // 引数を式へ差し替える（バインド済み＝そのキーの呼び出しをその場で置換）。
    server.req("POST", "/keys/filer/arg", "=r.currentDir()").unwrap();

    // OK で確定＝config.toml の F4 が新しい式へ更新される。
    server.req("POST", "/modal/command/ok", "").expect("ok");
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let cfg = std::fs::read_to_string(server.base.join("data").join("config.toml")).unwrap();
    assert!(
        cfg.contains(r#"ChangeDirectory("=r.currentDir()")"#),
        "F4 の引数が式へ差し替わって保存される: {cfg}"
    );
}

/// 「引数」＝未割当の組込コマンド行へ引数の式を付けると、引数つきの未割当（－）行が生え、その行を
/// キャプチャしてキーへ結べる。OK で `SelectMask("=式")` が当該キーに残る。
#[test]
fn settings_key_editor_attaches_arg_to_unbound_command() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 既定で未バインドの SelectMask（bare 行）を選ぶ。
    server.req("POST", "/keys/filer/search", "SelectMask").unwrap();
    assert!(keys().contains(r#"["SelectMask",[]]"#), "未割当の SelectMask 行: {}", keys());
    server.req("POST", "/keys/filer/select/0", "").unwrap();

    // 引数を付ける＝引数つきの未割当行が生え、その行が選択される（apply_arg が選択する）。
    server.req("POST", "/keys/filer/arg", "=r.cursorName()").unwrap();
    // 選択中のその行をキャプチャ＝SelectMask("=r.cursorName()") が Ctrl+Alt+J に割り当たる。
    server.req("POST", "/keys/filer/capture", "Ctrl+Alt+J").unwrap();

    // OK で確定＝config.toml の Ctrl+Alt+J に引数つき呼び出しが残る。
    server.req("POST", "/modal/command/ok", "").expect("ok");
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let cfg = std::fs::read_to_string(server.base.join("data").join("config.toml")).unwrap();
    assert!(
        cfg.contains(r#"SelectMask("=r.cursorName()")"#),
        "引数つき呼び出しがキーに割り当たって保存される: {cfg}"
    );
}

/// 「引数」で作った未割当の引数つき行は、「キー定義を削除」でその定義ごと消せる（bare 行は残る）。
#[test]
fn settings_key_editor_deletes_unbound_arg_definition() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;
    let count = || keys().matches(r#"["SelectMask",[]]"#).count();

    // 未バインドの SelectMask（bare 行）を選んで引数を付ける＝引数つきの未割当行が増える。
    server.req("POST", "/keys/filer/search", "SelectMask").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/arg", "=r.cursorName()").unwrap();
    server.req("POST", "/keys/filer/search", "SelectMask").unwrap();
    assert_eq!(count(), 2, "bare と引数つきで SelectMask 行が2つ: {}", keys());

    // 引数つき行（bare の次＝index 1）を選んで「キー定義を削除」＝その定義が消えて bare だけ残る。
    server.req("POST", "/keys/filer/select/1", "").unwrap();
    server.req("POST", "/keys/filer/unbind", "").unwrap();
    server.req("POST", "/keys/filer/search", "SelectMask").unwrap();
    assert_eq!(count(), 1, "引数つきの定義が消えて bare だけ残る: {}", keys());
}

/// キー順で機能名をダブルクリック相当＝インライン機能ピッカーで別機能へ差し替える。
/// 機能一覧は検索ボックスで絞り込め、確定でそのキーの定義が変わる（中止なら不変）。
#[test]
fn settings_key_editor_inline_function_picker_changes_binding() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 未使用キー Ctrl+Shift+Q を SelectMask に割り当て、キー順でその行を出す。
    server.req("POST", "/keys/filer/bind", r#"["SelectMask","Ctrl+Shift+Q"]"#).unwrap();
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();
    assert!(keys().contains(r#"["Ctrl+Shift+Q",["SelectMask"]]"#), "対象キー行: {}", keys());
    server.req("POST", "/keys/filer/select/0", "").unwrap();

    // その機能（label 0＝SelectMask）のピッカーへ。中止すると不変。
    server.req("POST", "/keys/filer/pick/0", "").unwrap();
    assert!(keys().contains(r#""picking":true"#), "ピックモードに入る: {}", keys());
    // ピッカーはジャンル順に並ぶ（カーソル移動ジャンルが先頭＝CursorUp が最初）。
    assert!(
        keys().contains(r#""rows":[["CursorUp",[]]"#),
        "機能ピッカーはジャンル順（先頭 CursorUp）: {}",
        keys()
    );
    server.req("POST", "/keys/filer/pickcancel", "").unwrap();
    assert!(keys().contains(r#""picking":false"#), "中止でピック解除");
    // 中止後（検索クリア・キー順へ復帰）も割り当ては不変。
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();
    assert!(keys().contains(r#"["Ctrl+Shift+Q",["SelectMask"]]"#), "中止で不変: {}", keys());

    // 再びピッカーへ入り、検索で MakeDirectory に絞って確定＝定義が差し替わる。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/pick/0", "").unwrap();
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    assert!(keys().contains(r#"["MakeDirectory",[]]"#), "ピッカーに機能が並ぶ: {}", keys());
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/pickcommit", "").unwrap();

    // 確定後（キー順・検索クリア）：Ctrl+Shift+Q は MakeDirectory に、SelectMask からは外れる。
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Q").unwrap();
    let s = keys();
    assert!(s.contains(r#"["Ctrl+Shift+Q",["MakeDirectory"]]"#), "機能が差し替わる: {s}");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// 長い一覧をスクロールできる（先頭行が動く・選択は不変・範囲外はクランプ）。ホイール／
/// スクロールバーと同じ scroll 経路を headless から叩く。
#[test]
fn settings_key_editor_scrolls_long_list() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
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

/// キー順で「キー定義を追加」＝空キー定義（機能未割当・－）を作り、後から機能ピッカーで
/// 機能を割り当てられる。
#[test]
fn settings_key_editor_add_empty_key_def_then_assign() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;
    server.req("POST", "/keys/filer/view", "key").unwrap();

    // 未使用キーの空キー定義を作る＝labels が空の行（機能未割当）。
    server.req("POST", "/keys/filer/addkeydef", "Ctrl+Shift+Z").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Z").unwrap();
    assert!(keys().contains(r#"["Ctrl+Shift+Z",[]]"#), "空キー定義の行: {}", keys());

    // その行を選び、機能ピッカーで MakeDirectory を割り当てる。
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/pick/0", "").unwrap();
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/pickcommit", "").unwrap();

    // 割り当て後：Ctrl+Shift+Z → MakeDirectory（空キー定義が解消）。
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+Z").unwrap();
    assert!(
        keys().contains(r#"["Ctrl+Shift+Z",["MakeDirectory"]]"#),
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
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;
    // 行数＝JSON 配列 `[...]` の個数（rows の各行が `["Cmd",[...]]`）。`],[` の数＋1。
    let count = |s: &str| s.matches("],[").count() + if s.contains("\"rows\":[]") { 0 } else { 1 };

    let full = keys();
    let full_n = count(&full);
    assert!(full_n > 40, "既定は Filer 全コマンドが並ぶ: {full_n}");
    assert!(full.contains(r#""query":"""#), "初期クエリは空: {full}");

    // 機能名で絞り込む："copy" は Copy/ClipCopy/ViewerCopy… を含み、MakeDirectory は除外。
    assert_eq!(
        server.req("POST", "/keys/filer/search", "copy").unwrap().0,
        200,
        "search は ok"
    );
    let s = keys();
    assert!(s.contains(r#""query":"copy""#), "クエリが反映される: {s}");
    assert!(s.contains(r#"["Copy",["C"]]"#), "Copy が残る: {s}");
    assert!(s.contains("ClipCopy"), "ClipCopy が残る: {s}");
    assert!(!s.contains("MakeDirectory"), "無関係な機能は消える: {s}");
    let copy_n = count(&s);
    assert!(copy_n > 0 && copy_n < full_n, "件数が減る: {copy_n} < {full_n}");

    // 日本語の表示名でも絞り込める："コピー" は Copy（表示名「コピー」）/ClipCopy
    //（「クリップボードにコピー」）に一致し、MakeDirectory（「フォルダ作成」）は除外。
    server.req("POST", "/keys/filer/search", "コピー").unwrap();
    let s = keys();
    assert!(s.contains(r#"["Copy",["C"]]"#), "表示名検索で Copy が残る: {s}");
    assert!(s.contains("ClipCopy"), "表示名検索で ClipCopy が残る: {s}");
    assert!(!s.contains("MakeDirectory"), "表示名検索で無関係な機能は消える: {s}");

    // キーで絞り込む：既定 K は MakeDirectory のみ（大小無視なので chord "K" に一致）。
    server.req("POST", "/keys/filer/search", "K").unwrap();
    let s = keys();
    assert!(s.contains("MakeDirectory"), "K を持つ MakeDirectory が出る: {s}");

    // 空クエリで全件へ戻る。
    server.req("POST", "/keys/filer/search", "").unwrap();
    let s = keys();
    assert_eq!(count(&s), full_n, "空クエリで全件に戻る: {s}");
    assert!(s.contains(r#""query":"""#), "クエリが空に戻る");

    // 絞り込みは config を変えない（割り当ては不変）＝Copy=C のまま。
    assert!(keys().contains(r#"["Copy",["C"]]"#), "割り当ては検索で変わらない");

    server.req("POST", "/modal/command/cancel", "").expect("cancel");
    poll(&server, "/state/modal", |b| b.trim() == "null");
}

/// キー編集ページの機能順／キー順ビュー切替。キー順では行が「キー→機能」になり、検索も効く。
/// キー順の削除は選択中の 1 キーだけを外す（同じ機能の別キーは残る）。
#[test]
fn settings_key_editor_toggles_command_and_key_views() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    let keys = || server.req("GET", "/keys/filer", "").expect("keys").1;

    // 既定は機能順：行は (機能, [キー…])。MakeDirectory=K。
    let s = keys();
    assert!(s.contains(r#""mode":"command""#), "初期は機能順: {s}");
    assert!(s.contains(r#"["MakeDirectory",["K"]]"#), "機能順 MakeDirectory=K: {s}");

    // キー順へ切替：行は (キー, [機能])。K→MakeDirectory。
    assert_eq!(server.req("POST", "/keys/filer/view", "key").unwrap().0, 200, "view 切替 ok");
    let s = keys();
    assert!(s.contains(r#""mode":"key""#), "キー順になる: {s}");
    assert!(s.contains(r#"["K",["MakeDirectory"]]"#), "キー順 K→MakeDirectory: {s}");

    // キー順でも検索が効く（キー・機能名どちらにも一致）。
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    assert!(keys().contains(r#"["K",["MakeDirectory"]]"#), "キー順で機能名検索が効く");
    server.req("POST", "/keys/filer/search", "").unwrap();

    // 機能順へ戻して、同じ機能に 2 キーを割り当てる（1 キー=1 行なので 2 行に割れる）。
    server.req("POST", "/keys/filer/view", "command").unwrap();
    server.req("POST", "/keys/filer/bind", r#"["SelectMask","Ctrl+Shift+M"]"#).unwrap();
    server.req("POST", "/keys/filer/bind", r#"["SelectMask","Ctrl+Shift+N"]"#).unwrap();
    let s = keys();
    assert!(
        s.contains(r#"["SelectMask",["Ctrl+Shift+M"]]"#)
            && s.contains(r#"["SelectMask",["Ctrl+Shift+N"]]"#),
        "SelectMask が 2 行に割れる: {s}"
    );

    // キー順で Ctrl+Shift+M の行だけを選び、削除＝その 1 キーだけ外れる。
    server.req("POST", "/keys/filer/view", "key").unwrap();
    server.req("POST", "/keys/filer/search", "Ctrl+Shift+M").unwrap();
    let s = keys();
    // 絞り込みで M の行だけ（N の行は出ない）。rows 配列を厳密に見る（status 文言の巻き込みを避ける）。
    assert!(
        s.contains(r#""rows":[["Ctrl+Shift+M",["SelectMask"]]]"#),
        "M の行だけが出る: {s}"
    );
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/unbind", "").unwrap();

    // 機能順へ戻すと、SelectMask は N だけ残る（M だけが外れた）。
    server.req("POST", "/keys/filer/search", "").unwrap();
    server.req("POST", "/keys/filer/view", "command").unwrap();
    assert!(
        keys().contains(r#"["SelectMask",["Ctrl+Shift+N"]]"#),
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
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
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
    server.req("POST", "/command/KeyBindsDialog", "").expect("KeyBindsDialog");
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

    server.req("POST", "/command/KeyBindsDialog", "").expect("open1");
    wait_modal(&server);
    let (w0, h0) = client_dims(server.port);

    // 小さくリサイズして閉じる。
    server.req("POST", "/modal/resize/700x520", "").expect("resize");
    server.req("POST", "/modal/command/cancel", "").expect("cancel1");
    poll(&server, "/state/modal", |b| b.trim() == "null");

    // 再オープンは記憶した小さいサイズで開く（既定より小さく・要求700近傍）。
    server.req("POST", "/command/KeyBindsDialog", "").expect("open2");
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
    server.req("POST", "/command/ChangeDriveDialog", "").expect("ChangeDriveDialog");
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
    server.req("POST", "/command/OpenTaskManager", "").expect("OpenTaskManager");
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

    server.req("POST", "/command/OpenTaskManager", "").unwrap();
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

/// 画像ビューアの表示モードキー（1=原寸/2=全体/3=幅/4=高/5=大）が、それぞれの
/// モードへ切り替わるのを debug-server で観測する。0 は原作に無い＝未バインドで不変。
#[test]
fn image_viewer_display_modes_switch_by_keys() {
    let server = Server::start(&["pic.png"], "");

    // 左 items は [.., pic.png]。CursorDown×1 で pic.png にカーソルを置いて開く。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/ViewFile", "").expect("ViewFile");
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

/// テキストビューアの検索が、可視範囲の全一致を桁単位で捉え、N で一致箇所単位に
/// 移動するのを debug-server で観測する（大小無視・同一行内の複数一致も辿る）。
#[test]
fn text_viewer_search_finds_all_occurrences_and_navigates() {
    let server = Server::start(&["doc.txt"], "");
    // 既定の placeholder を、複数一致を含む内容で上書きする（ViewFile は表示時に読み直す）。
    std::fs::write(server.base.join("sbx").join("doc.txt"), "foo bar foo\nbaz\nFOO end\n").unwrap();

    // doc.txt にカーソルを置いてテキストビューアで開く。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/ViewFile", "").expect("ViewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    // 一度撮影してレイアウト＋描画を走らせ、表示行を実幅で確定させる（検索は表示行を走査する）。
    req_bytes(server.port, "GET", "/snapshot").expect("warmup snapshot");

    // インライン検索バーを開いて "foo" を打ち込む（インクリメンタル検索が即時に走る）。
    server.req("POST", "/command/ViewerSearchDialog", "").expect("open search");
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
    server.req("POST", "/command/ViewerSearchDialog", "").expect("reopen search");
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
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/ViewFile", "").expect("ViewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    req_bytes(server.port, "GET", "/snapshot").expect("warmup snapshot");

    let count = |s: &Server| s.req("GET", "/state/viewer/match_count", "").expect("count").1.trim().to_string();

    server.req("POST", "/command/ViewerSearchDialog", "").expect("open");
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
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/ViewFile", "").expect("ViewFile");
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
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/ViewFile", "").expect("ViewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    req_bytes(server.port, "GET", "/snapshot").expect("warmup snapshot");
    server.req("POST", "/command/ViewerSearchDialog", "").expect("open");
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
/// 優先する（トグルせずそのコマンドを実行）。ここでは Alt+C を ViewerClose に割り当てて確認。
#[test]
fn text_viewer_search_mnemonic_yields_to_user_keybind() {
    let server = Server::start(&["doc.txt"], "[keybinds_textviewer]\n\"Alt+C\" = \"ViewerClose\"\n");
    std::fs::write(server.base.join("sbx").join("doc.txt"), "foo bar\n").unwrap();
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/ViewFile", "").expect("ViewFile");
    poll(&server, "/state/active_view", |b| b.trim().trim_matches('"') == "text");
    req_bytes(server.port, "GET", "/snapshot").expect("warmup snapshot");
    server.req("POST", "/command/ViewerSearchDialog", "").expect("open");
    poll(&server, "/state/viewer/search_open", |b| b.trim() == "true");

    // Alt+C は被っているのでユーザーバインド（ViewerClose）が走る＝ビューアが閉じる。
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

/// 設定エディタ：補完つき「引数」モーダルで `r.<prefix>` を打つと候補（組込メンバー＋登録コマンド）が
/// 出て、候補の確定でカレット直前のプレフィックスがメンバ名へ置換される（headless 観測）。
#[test]
fn completion_popup_lists_members_and_inserts_on_accept() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[("00.ts", r#"rerics.registerCommand("myCmd", () => {});"#)],
    );
    poll(&server, "/script/members", |b| b.contains("myCmd"));
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").expect("nav");
    // 組込コマンド行（MakeDirectory）を選んで、補完つき「引数」モーダルを開く（応答先返し）。
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openarg", "").unwrap();

    // `=r.my` と実キー入力（WM_CHAR＝EN_CHANGE 経路）すると、登録コマンド myCmd が候補に出る。
    server.req("POST", "/completion/keystrokes", "=r.my").unwrap();
    let comp = poll(&server, "/completion", |b| b.contains("myCmd"));
    assert!(comp.contains("myCmd"), "登録コマンドが補完候補に出る: {comp}");

    // 先頭候補を確定＝プレフィックス `my` がメンバ名 `myCmd` へ置換される。
    server.req("POST", "/completion/accept/0", "").unwrap();
    let comp2 = poll(&server, "/completion", |b| b.contains(r#""text":"=r.myCmd"#));
    assert!(comp2.contains(r#""text":"=r.myCmd"#), "確定でメンバ名が挿入される: {comp2}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// 補完つき入力欄のキーボード操作：↑↓で候補移動（クランプ）・Enter で確定・Ctrl+Space で強制表示。
/// 実キー経路（WM_KEYDOWN/WM_CHAR を keyhook サブクラスが横取り）を headless で検証する。
#[test]
fn completion_keyboard_navigation_and_ctrl_space() {
    let server = Server::start(&["a.txt"], "");
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openarg", "").unwrap();
    let comp = || server.req("GET", "/completion", "").unwrap().1;

    // `=r.o` で候補（on/open/openDialog/oppositePane）が出て、先頭が選択されている。
    server.req("POST", "/completion/keystrokes", "=r.o").unwrap();
    let c = poll(&server, "/completion", |b| b.contains(r#""visible":true"#));
    assert!(c.contains("oppositePane"), "候補が出る: {c}");
    assert!(c.contains(r#""selected":0"#), "先頭が選択される: {c}");

    // ↓↓↑ で選択が index 1（open）に動く。
    server.req("POST", "/completion/key/down", "").unwrap();
    server.req("POST", "/completion/key/down", "").unwrap();
    server.req("POST", "/completion/key/up", "").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains(r#""selected":1"#));
    assert!(c2.contains(r#""selected":1"#), "↓↓↑ で index 1: {c2}");

    // Enter で選択中（open）を確定＝プレフィックス o が open に置換される。
    server.req("POST", "/completion/key/enter", "").unwrap();
    let c3 = poll(&server, "/completion", |b| b.contains(r#""text":"=r.open"#));
    assert!(c3.contains(r#""text":"=r.open"#), "Enter で open 確定: {c3}");

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
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
    wait_modal(&server);
    server.req("POST", "/settings/nav/5", "").unwrap();
    server.req("POST", "/keys/filer/search", "MakeDirectory").unwrap();
    server.req("POST", "/keys/filer/select/0", "").unwrap();
    server.req("POST", "/keys/filer/openarg", "").unwrap();

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

/// scripting：`rerics.activePane()` が実ペインの項目・選択・カーソルを読み取れる
/// （オブジェクトモデルの実 GUI 経路＝スナップショットが UI スレッドから組み上がる）。
#[test]
fn script_active_pane_reads_items_selection_and_cursor() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt", "c.txt"], &[]);
    // 左 items は [.., a.txt, b.txt, c.txt]。CursorDown×1 で a.txt → MarkToggle で
    // a.txt を選択しカーソルは b.txt（index 2）へ。
    server.req("POST", "/command/CursorDown", "").expect("down");
    server.req("POST", "/command/MarkToggle", "").expect("mark");

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
        .req("POST", "/script/eval", r#"rerics.command("CursorDown");"#)
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
    server.req("POST", "/command/CursorDown", "").unwrap();
    let log = poll(&server, "/state/log", |b| b.contains("EV cmd:CursorDown"));
    assert!(log.contains("EV cmd:CursorDown"), "executeCommand should fire: {log}");
    assert_eq!(
        count_substr(&log, "EV cd:"),
        0,
        "in-place command must not fire changeDirectory: {log}"
    );

    // 親へ移動：移動なので changeDirectory も発火する。
    server.req("POST", "/command/ToParent", "").unwrap();
    let log2 = poll(&server, "/state/log", |b| b.contains("EV cd:"));
    assert!(log2.contains("EV cd:"), "navigation should fire changeDirectory: {log2}");
    assert!(
        log2.contains("EV cmd:ToParent"),
        "ToParent should also fire executeCommand: {log2}"
    );
}

/// scripting：`await rerics.copy()` がワーカー完了で resolve し、コピーが実ペインに反映される
/// （非同期操作ブリッジの実経路＝UI スレッドのワーカー完了がエンジンの await を解く）。
#[test]
fn script_async_copy_awaits_worker_completion() {
    let server = Server::start_with_scripts(&["a.txt", "b.txt"], &[]);
    // 右ペインを親へ移し、左=sbx／右=親 にする（src≠dst で同名衝突を避ける）。
    server.req("POST", "/command/FocusRight", "").unwrap();
    server.req("POST", "/command/ToParent", "").unwrap();
    let parent = server.req("GET", "/state/panes/right/location", "").unwrap().1;
    assert!(!parent.contains("sbx"), "right pane should be the parent: {parent}");
    server.req("POST", "/command/FocusLeft", "").unwrap();

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
    server.req("POST", "/command/FocusRight", "").unwrap();
    server.req("POST", "/command/ToParent", "").unwrap();
    server.req("POST", "/command/FocusLeft", "").unwrap();

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
    server.req("POST", "/command/FocusRight", "").unwrap();
    server.req("POST", "/command/ToParent", "").unwrap();
    server.req("POST", "/command/FocusLeft", "").unwrap();

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
    server.req("POST", "/command/FocusRight", "").unwrap();
    server.req("POST", "/command/ToParent", "").unwrap();
    server.req("POST", "/command/FocusLeft", "").unwrap();

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

/// Quit は「賢いクローズ」：タブが複数あれば現タブを閉じ、最後の 1 枚ならアプリを終了する。
/// ここでは複数タブ時に現タブが閉じてアプリが生き続けること（＝強制終了でない）を検証する。
#[test]
fn quit_closes_tab_when_multiple_keeps_app_alive() {
    let server = Server::start(&["a.txt"], "");
    let count = || server.req("GET", "/state/tabs/count", "").expect("count").1;
    assert_eq!(count().trim(), "1", "初期は 1 タブ");
    server.req("POST", "/command/NewFiler", "").expect("NewFiler");
    assert_eq!(count().trim(), "2", "NewFiler で 2 タブ");
    // タブが複数あるので Quit は現タブを閉じるだけ（アプリは終了しない）。
    server.req("POST", "/command/Quit", "").expect("Quit");
    assert_eq!(count().trim(), "1", "Quit で 1 タブに減る");
    assert!(server.req("GET", "/state", "").is_some(), "アプリは終了していない");
}

/// キーバインド経路：`Eval("code")` コマンドが `exec` からエンジンへ流れ、コードが評価される。
/// `/command/Eval` は実際のキー押下と同じ `exec` を通るので、これでキー→コード評価の配線を検証する。
#[test]
fn eval_command_dispatches_code_to_engine() {
    let server = Server::start_with_scripts(&["a.txt"], &[]);
    server
        .req("POST", "/command/Eval", r#"["rerics.log(\"cmd-eval-marker-7\");"]"#)
        .expect("Eval");
    let log = poll(&server, "/state/log", |b| b.contains("cmd-eval-marker-7"));
    assert!(log.contains("cmd-eval-marker-7"), "Eval コマンドがコードを評価して記録するはず: {log}");
}

/// 値返し Eval：最後の式の値が文字列で返る。undefined/null は空、Promise は解決を待つ。
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

/// キーバインド経路：`Script("name")` コマンドが `exec` からエンジンへ流れ、登録コマンドを実行する。
/// 登録コマンドがアクティブペインを移動させ、UI に反映されることで配線を検証する。
#[test]
fn script_command_invokes_registered_command() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00-cmds.ts",
            r#"rerics.registerCommand("goUp", () => { rerics.navigate(rerics.currentDir() + "/.."); });"#,
        )],
    );
    let loc0 = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    assert!(loc0.contains("sbx"), "サンドボックスから開始するはず: {loc0}");

    server.req("POST", "/command/Script", r#"["goUp"]"#).expect("Script");
    let loc1 = poll(&server, "/state/panes/left/location", |b| !b.contains("sbx"));
    assert!(
        !loc1.contains("sbx"),
        "Script コマンドが登録コマンドを実行してペインが移動するはず: {loc1}"
    );
}

/// 第3弾：引数の式（`=...`）が別スレッドで非同期評価され、その値で本体コマンドが走る。
/// `=r.currentDir() + "/sub"` を評価して実フォルダへ移動できる（マクロ展開ではなく式評価の経路）。
#[test]
fn expr_arg_evaluates_async_and_runs_command() {
    let server = Server::start(&["a.txt"], "");
    std::fs::create_dir_all(server.base.join("sbx").join("sub")).unwrap();
    // 引数の式は現在地を読んで "/sub" を足す＝HostApi（currentDir）を式中から呼ぶ非同期評価。
    server
        .req("POST", "/command/ChangeDirectory", r#"["=r.currentDir() + \"/sub\""]"#)
        .expect("cd");
    let loc = poll(&server, "/state/panes/left/location", |b| b.contains("sub"));
    assert!(loc.contains("sub"), "式の値でサブフォルダへ移動するはず: {loc}");
}

/// 第3弾の核心：引数の式が `r.prompt()` 等のモーダルを呼んでも、UI は recv でブロックしない
/// （結果は別チャネル＋wake で届く）のでデッドロックしない。プロンプトへ入れたパスへ移動できる。
#[test]
fn expr_arg_with_modal_does_not_deadlock() {
    let server = Server::start(&["a.txt"], "");
    let target = server.base.join("sbx").join("target");
    std::fs::create_dir_all(&target).unwrap();
    // 式が prompt を開く。コマンドは即返り、モーダルを debug 駆動してパスを返す。
    server
        .req("POST", "/command/ChangeDirectory", r#"["=r.prompt(\"dir?\")"]"#)
        .expect("cd");
    wait_modal(&server);
    server.req("POST", "/modal/text", &target.display().to_string()).expect("text");
    server.req("POST", "/modal/key/enter", "").expect("enter");
    let loc = poll(&server, "/state/panes/left/location", |b| b.contains("target"));
    assert!(loc.contains("target"), "プロンプトのパスへ移動するはず（デッドロックしない）: {loc}");
}

/// 第3弾：引数の式が開いたモーダルをキャンセルすると、式は空（null）になり実行中止＝移動しない
/// （マクロのキャンセルと同じ無音中止）。
#[test]
fn expr_arg_modal_cancel_aborts_silently() {
    let server = Server::start(&["a.txt"], "");
    // 基準点を sbx から動かしておく（移動しないことを確かめるため）。
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1.trim().to_string();
    server.req("POST", "/command/ToParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx);
    // 式が prompt を開く→Esc でキャンセル→式は空→中止＝場所は変わらない。
    server
        .req("POST", "/command/ChangeDirectory", r#"["=r.prompt(\"dir?\")"]"#)
        .unwrap();
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

/// コマンドパレットは登録済みスクリプトコマンドも候補に出し、確定で `Script("name")` トークンを
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
    server.req("POST", "/command/CommandDirect", "").expect("CommandDirect");
    wait_modal(&server);

    // ラベル「整理する」で引け、候補に「整理する（スクリプト）」が出る。
    server.req("POST", "/completion/keystrokes", "整理").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("整理する（スクリプト）"));
    assert!(c.contains("整理する（スクリプト）"), "スクリプトコマンドが候補に出る: {c}");

    // 先頭候補を確定＝入力欄に Script("organize") トークンが入る。
    server.req("POST", "/completion/accept/0", "").unwrap();
    let c2 = poll(&server, "/completion", |b| b.contains(r#"Script(\"organize\")"#));
    assert!(c2.contains(r#"Script(\"organize\")"#), "確定で Script トークンが挿入される: {c2}");

    server.req("POST", "/modal/command/cancel", "").unwrap();
}

/// コマンドパレット（CommandDirect）：補完は和名でも内部名でも引け、確定した文字列を
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
    server.req("POST", "/command/CommandDirect", "").expect("CommandDirect");
    wait_modal(&server);
    server.req("POST", "/completion/keystrokes", "下へ").unwrap();
    let c = poll(&server, "/completion", |b| b.contains("CursorDown"));
    assert!(
        c.contains("カーソルを下へ (CursorDown)"),
        "和名でコマンド名補完が引ける: {c}"
    );

    // 本文を内部名 CursorDown にして OK＝実行され、カーソルが 1 へ動く。
    server.req("POST", "/completion/type", "CursorDown").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    assert_eq!(
        server.req("GET", "/state/panes/left/cursor", "").unwrap().1.trim(),
        "1",
        "パレットで CursorDown を実行するとカーソルが 1 へ動く"
    );

    // 解釈できない文字列は実行されずログに警告が出る（カーソルは動かない）。
    server.req("POST", "/command/CommandDirect", "").expect("CommandDirect2");
    wait_modal(&server);
    server.req("POST", "/completion/type", "ぜんぜん違う文字列").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let log = server.req("GET", "/state/log/lines", "").unwrap().1;
    assert!(
        log.contains("コマンドとして解釈できません"),
        "不正コマンドは警告ログに出る: {log}"
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
  { label = "下へ(&D)", command = "CursorDown" },
  { separator = true },
  { label = "サブ(&S)", command = 'Menu("sub")' },
]

[[menus]]
name = "sub"
items = [
  { label = "先頭へ", command = "CursorTop" },
]
"#;
    let server = Server::start(&["a.txt", "b.txt", "c.txt"], config);

    // 解決済みの項目木：コマンド・セパレータ・参照式サブメニューが出る。
    let tree = server.req("GET", "/menu/test", "").unwrap().1;
    assert!(tree.contains("\"command\":\"CursorDown\""), "コマンド項目: {tree}");
    assert!(tree.contains("\"sep\":true"), "セパレータ: {tree}");
    assert!(tree.contains("\"command\":\"CursorTop\""), "サブメニューが展開される: {tree}");
    // サブメニュー内の項目にも深さ優先で葉インデックスが振られる。
    assert!(tree.contains("\"leaf\":1"), "サブメニューの葉も採番される: {tree}");

    // 葉 0（CursorDown）を選ぶとカーソルが 1 へ動く。
    server.req("POST", "/menu/test/select/0", "").unwrap();
    let c = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "1");
    assert_eq!(c.trim(), "1", "葉0=CursorDown でカーソルが 1 へ");

    // 葉 1（サブメニュー内の CursorTop）を選ぶとカーソルが 0 へ戻る＝サブメニュー項目も実行できる。
    server.req("POST", "/menu/test/select/1", "").unwrap();
    let c = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "0");
    assert_eq!(c.trim(), "0", "葉1=サブメニューの CursorTop でカーソルが 0 へ");

    // 未定義メニューは null。
    let unknown = server.req("GET", "/menu/nope", "").unwrap().1;
    assert_eq!(unknown.trim(), "null", "未定義メニューは null: {unknown}");
}

/// スクリプトが `registerMenu` で登録した名前付きメニューも `Menu("名前")` の解決対象になる
/// （config 定義と同じレジストリへマージされる）。`/menu/<name>` で出て `select` で実行できる。
#[test]
fn named_menu_includes_script_registered() {
    let server = Server::start_with_scripts(
        &["a.txt", "b.txt", "c.txt"],
        &[(
            "00.ts",
            r#"rerics.registerMenu("scripted", [
                { label: "末尾へ", command: "CursorEnd" },
                { label: "先頭へ", command: "CursorTop" },
            ]);"#,
        )],
    );

    let tree = server.req("GET", "/menu/scripted", "").unwrap().1;
    assert!(tree.contains("\"command\":\"CursorEnd\""), "登録メニューが解決される: {tree}");
    assert!(tree.contains("\"command\":\"CursorTop\""), "2 項目目も出る: {tree}");

    // 葉 0（CursorEnd）でカーソルが末尾へ動く。
    server.req("POST", "/menu/scripted/select/0", "").unwrap();
    let moved = poll(&server, "/state/panes/left/cursor", |b| b.trim() != "0");
    assert_ne!(moved.trim(), "0", "CursorEnd で末尾へ動く");

    // 葉 1（CursorTop）でカーソルが先頭へ戻る。
    server.req("POST", "/menu/scripted/select/1", "").unwrap();
    let top = poll(&server, "/state/panes/left/cursor", |b| b.trim() == "0");
    assert_eq!(top.trim(), "0", "CursorTop で先頭へ戻る");
}

/// 設定の「メニュー」ページでメニューの追加/選択/改名/並べ替え/削除を駆動できる。標準
/// コントロールは generic な `/modal/*` で叩けないので、専用フック `/menu-editor/*` で観測・駆動する。
/// 編集は作業コピー（Shared.cfg）に対してで、OK を押すまで実 config には触れない。
#[test]
fn menu_editor_drives_menu_crud() {
    let config = r#"
[[menus]]
name = "alpha"
items = [ { label = "コピー", command = "Copy" } ]

[[menus]]
name = "beta"
items = []
"#;
    let server = Server::start(&["a.txt"], config);
    server.req("POST", "/command/OpenSettings", "").expect("OpenSettings");
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

/// メニュー項目に `Script("名前", 引数...)` を書くと、選んだとき登録スクリプトが引数ごと
/// 実行される（原作のスクリプト連携メニューを移植する経路・引数転送つき）。
#[test]
fn menu_script_token_runs_registered_script_with_args() {
    let server = Server::start_with_scripts(
        &["a.txt"],
        &[(
            "00.ts",
            r#"
            rerics.registerCommand("ping", (msg) => rerics.log("PONG:" + msg));
            rerics.registerMenu("fns", [{ label: "ピング", command: 'Script("ping", "hi")' }]);
            "#,
        )],
    );

    // 項目を選ぶと登録スクリプトが引数つきで走る（ログに出る）。
    server.req("POST", "/menu/fns/select/0", "").unwrap();
    let log = poll(&server, "/state/log/lines", |b| b.contains("PONG:hi"));
    assert!(log.contains("PONG:hi"), "Script 経由でスクリプトが引数つきで実行される: {log}");
}
