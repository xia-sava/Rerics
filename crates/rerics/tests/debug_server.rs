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

/// CreateFile＝入力したファイル名で空ファイルを作成する。
#[test]
fn create_file_makes_empty_file() {
    let server = Server::start_writable(&["a.txt"]);

    server.req("POST", "/command/CreateFile", "").unwrap();
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
        if let Ok(s) = std::fs::read_to_string(&hist_path) {
            if s.contains("pathhistory") {
                hist = s;
                break;
            }
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
        if let Ok(s) = std::fs::read_to_string(&hist_path) {
            if s.contains("changedir") {
                hist = s;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    assert!(hist.contains("changedir"), "history.toml should have the changedir bucket: {hist}");
    assert!(hist.contains("sbx"), "entered path should be recorded: {hist}");
}

/// 引数マクロ版 `ChangeDirectory("<I:…>")`：`<I:>` が入力モーダルを開き、打った値で移動する
/// ことを確認する（引数基盤の段階2＝マクロ展開の実証）。
#[test]
fn nav_change_directory_macro_input() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server
        .req("GET", "/state/panes/left/location", "")
        .unwrap()
        .1
        .trim()
        .to_string();
    let sbx_raw = sbx_json.trim_matches('"').replace("\\\\", "\\");

    server.req("POST", "/command/ToParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx_json);
    assert_ne!(parent.trim(), sbx_json, "ToParent should leave the sandbox");

    // body の引数に入力マクロを渡す＝実行直前に入力モーダルが開く。
    server
        .req("POST", "/command/ChangeDirectory", r#"["<I:移動先>"]"#)
        .unwrap();
    let modal = wait_modal(&server);
    assert!(
        modal.contains("\"has_input\":true"),
        "<I:> macro should open a text-input modal: {modal}"
    );

    server.req("POST", "/modal/text", &sbx_raw).unwrap();
    server.req("POST", "/modal/key/enter", "").unwrap();
    let back = poll(&server, "/state/panes/left/location", |b| b.trim() == sbx_json);
    assert_eq!(back.trim(), sbx_json, "input from <I:> macro should navigate there");
}

/// 引数マクロ版 `ChangeDirectory` で入力をキャンセルすると、原作準拠で無音中止（移動しない）。
#[test]
fn nav_change_directory_macro_cancel_is_silent() {
    let server = Server::start(&["a.txt"], "");
    let sbx_json = server
        .req("GET", "/state/panes/left/location", "")
        .unwrap()
        .1
        .trim()
        .to_string();

    server.req("POST", "/command/ToParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx_json);

    server
        .req("POST", "/command/ChangeDirectory", r#"["<I:移動先>"]"#)
        .unwrap();
    wait_modal(&server);
    // Esc でキャンセル＝モーダルが閉じて、場所は変わらない。
    server.req("POST", "/modal/key/esc", "").unwrap();
    poll(&server, "/state/modal", |b| b.trim() == "null");
    let after = server
        .req("GET", "/state/panes/left/location", "")
        .unwrap()
        .1;
    assert_eq!(after.trim(), parent.trim(), "cancel should not navigate (silent abort)");
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

/// RegisterPath で現在地を登録し、JumpDialog でそこへ戻る。
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
    server.req("POST", "/command/RegisterPath", "").unwrap();
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
