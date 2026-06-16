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

/// PathHistoryDialog＝移動履歴の一覧（list_box モーダル）から選んでジャンプする。
#[test]
fn nav_path_history_dialog() {
    let server = Server::start(&["a.txt"], "");
    let sbx = server.req("GET", "/state/panes/left/location", "").unwrap().1;
    let sbx = sbx.trim().to_string();

    // 親へ移動して履歴を1件作る。
    server.req("POST", "/command/ToParent", "").unwrap();
    let parent = poll(&server, "/state/panes/left/location", |b| b.trim() != sbx);
    assert_ne!(parent.trim(), sbx, "ToParent should leave the sandbox");

    // 履歴ダイアログを開く（リスト選択モーダル）。
    server.req("POST", "/command/PathHistoryDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"list\""), "should open a list modal: {modal}");
    assert!(modal.contains("sbx"), "history should list the sandbox: {modal}");

    // 先頭（直前の現在地＝sbx）を選んで OK＝そこへジャンプ。
    server.req("POST", "/modal/select/0", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let back = poll(&server, "/state/panes/left/location", |b| b.trim() == sbx);
    assert_eq!(back.trim(), sbx, "selecting a history entry should navigate there");
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
    assert!(modal.contains("\"kind\":\"list\""), "jump should open a list: {modal}");
    assert!(modal.contains("home"), "jump should list the bookmark: {modal}");
    server.req("POST", "/modal/select/0", "").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();
    let back = poll(&server, "/state/panes/left/location", |b| b.trim() == sbx_json);
    assert_eq!(back.trim(), sbx_json, "jump should navigate to the bookmark");
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
    assert!(modal.contains("\"kind\":\"list\""), "drive dialog should open a list: {modal}");
    assert!(modal.contains("\"items\":[\""), "should list at least one drive: {modal}");

    // 既定選択（現在ドライブ）のまま OK。
    server.req("POST", "/modal/command/ok", "").unwrap();
    let after = poll(&server, "/state/panes/left/location", |b| b.trim() == expected);
    assert_eq!(after.trim(), expected, "selecting the current drive should go to its root");
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

/// RenameSequenceDialog＝選択を連番にリネームする（プレフィックス＋0詰め＋拡張子保持）。
#[test]
fn rename_sequence_with_prefix() {
    let server = Server::start_writable(&["a.txt", "b.txt"]);
    // a.txt(1) と b.txt(2) をマークする（Space＝MarkToggle はマーク後に下へ）。
    server.req("POST", "/command/CursorDown", "").unwrap();
    server.req("POST", "/command/MarkToggle", "").unwrap();
    server.req("POST", "/command/MarkToggle", "").unwrap();

    server.req("POST", "/command/RenameSequenceDialog", "").unwrap();
    let modal = wait_modal(&server);
    assert!(modal.contains("\"kind\":\"rename_seq\""), "should open rename_seq modal: {modal}");

    // プレフィックスを "img" に（先頭の Edit）。開始番号 1・桁 3・拡張子保持は既定。
    server.req("POST", "/modal/text", "img").unwrap();
    server.req("POST", "/modal/command/ok", "").unwrap();

    let items = poll(&server, "/state/panes/left/items", |b| b.contains("img001.txt"));
    assert!(items.contains("\"name\":\"img001.txt\""), "a.txt -> img001.txt: {items}");
    assert!(items.contains("\"name\":\"img002.txt\""), "b.txt -> img002.txt: {items}");
    assert!(!items.contains("\"name\":\"a.txt\""), "old a.txt should be gone: {items}");
    assert!(!items.contains("\"name\":\"b.txt\""), "old b.txt should be gone: {items}");
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
