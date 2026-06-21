use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use winsafe as w;
use rerics_core::{Location, LogLevel, data_dir, messages, open_archive};
use crate::task::{ArchiveOutcome, ChannelHost, TaskControl, WorkerEvent};
use crate::{ArchiveOp, MainWindow, dialog, hash64, join_inner_path, read_capped, short_desc};

impl MainWindow {
    /// 書庫内では未対応の書込み操作をガードする。対象ペインが書庫内なら警告ログを
    /// 出して `true` を返す（呼び側は早期 return する）。展開コピー等は後段で対応。
    pub(crate) fn block_if_archive(&self, is_left: bool, op: &str) -> bool {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn(&format!("書庫内では{}は未対応です", op));
            true
        } else {
            false
        }
    }

    /// 現在ペインのカーソル下ファイル `name` の bytes を取得する（実FS/書庫内 両対応）。
    /// `cap` を超える分は切り詰め、超過していたら `truncated=true` を返す。
    pub(crate) fn read_pane_file(&self, is_left: bool, name: &str, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
        let loc = self.pane(is_left).borrow().loc().clone();
        match loc {
            Location::Real(dir) => read_capped(&dir.join(name), cap),
            Location::Archive { archive, inner } => {
                let mut bytes = self.read_archive_entry(&archive, &join_inner_path(&inner, name))?;
                let truncated = bytes.len() > cap;
                bytes.truncate(cap);
                Ok((bytes, truncated))
            }
        }
    }

    /// 書庫内エントリを読む（暗号化エントリはキャッシュ済み or 入力プロンプトのパスワードで
    /// 復号する）。パスワードが合えば書庫単位でキャッシュし、同一書庫の他エントリで再入力
    /// させない。誤入力は数回まで再入力を促す。
    pub(crate) fn read_archive_entry(&self, archive: &Path, inner: &str) -> std::io::Result<Vec<u8>> {
        // 既に temp に在れば実FS から直接読む（一括展開済み or per-file 展開済み・再展開しない）。
        let root = self.register_archive_temp(archive);
        if let Some(rel) = Self::safe_inner_path(inner) {
            let p = root.join(&rel);
            if p.is_file() {
                return std::fs::read(&p);
            }
        }
        let backend = open_archive(archive)?;
        let encrypted = backend
            .list()
            .ok()
            .and_then(|es| es.into_iter().find(|e| e.path == inner))
            .map(|e| e.is_encrypted)
            .unwrap_or(false);
        if !encrypted {
            return backend.read(inner);
        }
        // キャッシュ済みパスワードを先に試す。
        if let Some(pw) = self.archive_passwords.borrow().get(archive).cloned() {
            if let Ok(b) = backend.read_with_password(inner, Some(&pw)) {
                return Ok(b);
            }
        }
        for _ in 0..3 {
            let Some(pw) = self.prompt_password(archive) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "パスワードが必要です",
                ));
            };
            match backend.read_with_password(inner, Some(pw.as_bytes())) {
                Ok(b) => {
                    self.archive_passwords
                        .borrow_mut()
                        .insert(archive.to_path_buf(), pw.into_bytes());
                    return Ok(b);
                }
                Err(_) => self.log.warn("パスワードが違うようです"),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "パスワードが一致しません",
        ))
    }

    /// 書庫から取り出したファイルの一時展開先。**プロセスごとに分離**して、別インスタンス
    /// の掃除が稼働中インスタンスの展開物を壊さないようにする（共有 dir を全削除しない）。
    pub(crate) fn archive_temp_dir() -> PathBuf {
        data_dir()
            .join("cache")
            .join("archive")
            .join(std::process::id().to_string())
    }

    /// 自プロセスの一時展開先のみを削除する（起動時の残骸掃除＋終了時の後始末）。
    /// 他プロセスの dir には触れない。クラッシュで残った他 pid の残骸は手動掃除（cache 配下）。
    pub(crate) fn clear_archive_temp() {
        let _ = std::fs::remove_dir_all(Self::archive_temp_dir());
    }

    /// 書庫1つ分の temp をまとめる dir 名＝書庫パス＋mtime のハッシュ。一括展開も per-file 展開も
    /// この配下に置くので、回収はこの dir を1発削除すれば済む（mtime 込み＝外部更新で別 key）。
    pub(crate) fn archive_key(archive: &Path) -> String {
        let stamp = std::fs::metadata(archive)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!(
            "{:016x}",
            hash64(&format!("{}\u{0}{}", archive.display(), stamp))
        )
    }

    /// 書庫1つ分の temp ルート（`cache/archive/<pid>/<key>/`）。
    pub(crate) fn archive_temp_root(archive: &Path) -> PathBuf {
        Self::archive_temp_dir().join(Self::archive_key(archive))
    }

    /// 一括展開の完了マーカ（`<key>.done`・ルートの兄弟）。content と混ざらないよう外に置く。
    /// 中断/クラッシュで残った不完全なルートを「完了済み」と誤認しないため成功後にだけ作る。
    pub(crate) fn archive_extract_marker(archive: &Path) -> PathBuf {
        Self::archive_temp_dir().join(format!("{}.done", Self::archive_key(archive)))
    }

    /// この書庫の temp ルートをレジストリに登録して返す（セッション中の参照カウント掃除の元）。
    pub(crate) fn register_archive_temp(&self, archive: &Path) -> PathBuf {
        let root = Self::archive_temp_root(archive);
        self.archive_temp_dirs
            .borrow_mut()
            .entry(archive.to_path_buf())
            .or_insert_with(|| root.clone());
        root
    }

    /// 書庫内パス（'/' 区切り）を temp ルート配下の安全な相対パスへ。空/"."は捨て、".."や '\\'
    /// 混入は弾く（zip-slip 対策）。有効セグメントが無ければ None。
    pub(crate) fn safe_inner_path(inner: &str) -> Option<PathBuf> {
        let mut p = PathBuf::new();
        let mut any = false;
        for seg in inner.split('/') {
            if seg.is_empty() || seg == "." {
                continue;
            }
            if seg == ".." || seg.contains('\\') {
                return None;
            }
            p.push(seg);
            any = true;
        }
        any.then_some(p)
    }

    /// 書庫内パスを temp ルート配下の相対パスへ（読み取り用途・不正は空）。
    pub(crate) fn inner_to_pathbuf(inner: &str) -> PathBuf {
        Self::safe_inner_path(inner).unwrap_or_default()
    }

    /// 書庫内ファイルの実パスを得る。temp に既に在ればそれを、無ければ個別展開する。
    pub(crate) fn resolve_archive_file(
        &self,
        archive: &Path,
        inner_file: &str,
        _name: &str,
        password: Option<&[u8]>,
    ) -> std::io::Result<PathBuf> {
        let root = self.register_archive_temp(archive);
        if let Some(rel) = Self::safe_inner_path(inner_file) {
            let p = root.join(&rel);
            if p.is_file() {
                return Ok(p);
            }
        }
        Self::extract_entry_to_temp(archive, inner_file, password)
    }

    /// 書庫内エントリを temp（`<root>/<inner>`）へ展開し実パスを返す。既に在れば再利用する
    /// （mtime 込みキーなので外部更新時は別ルート＝古い展開物を二度と参照しない）。書込みは
    /// 「一時名→rename」のアトミック方式（BG 並行展開でも書きかけを読まない・計画 §7.6）。
    ///
    /// static のまま（メディア resolver クロージャ・BG プリフェッチからも呼ぶ）。レジストリ
    /// 登録は主スレッド側 `register_archive_temp` に委ねる（呼ぶ書庫は既に登録済み）。
    pub(crate) fn extract_entry_to_temp(
        archive: &Path,
        inner_file: &str,
        password: Option<&[u8]>,
    ) -> std::io::Result<PathBuf> {
        let rel = Self::safe_inner_path(inner_file).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "不正な書庫内パス")
        })?;
        let path = Self::archive_temp_root(archive).join(&rel);
        if path.is_file() {
            return Ok(path);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let backend = open_archive(archive)?;
        let bytes = backend.read_with_password(inner_file, password)?;
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("entry");
        let tmp = path.with_file_name(format!("{}.tmp.{}", fname, std::process::id()));
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// 対象ペインが「未展開の非ランダムアクセス書庫」なら一括展開を非同期で開始する。
    /// 開始したら `true`（呼び側は一覧反映を完了イベントに委ねる）。実FS/RA 書庫/展開済みは
    /// `false`（呼び側は従来どおり同期 populate する）。`caps` は安価にプローブする。
    pub(crate) fn maybe_start_archive_extract(&self, is_left: bool) -> w::AnyResult<bool> {
        let loc = self.pane(is_left).borrow().loc().clone();
        let Location::Archive { archive, .. } = loc else {
            return Ok(false);
        };
        if self.archive_extracted.borrow().contains_key(&archive) {
            return Ok(false);
        }
        // 同じ書庫を別ペインが既に展開中なら、このペインもスピナーを出して完了を待つ
        // （二重ワーカ＝同一 temp への並行展開を避ける。完了時に両ペインまとめて反映する）。
        if self.archive_extracting.borrow().contains(&archive) {
            self.view(is_left).set_loading();
            return Ok(true);
        }
        let random_access = match open_archive(&archive) {
            Ok(be) => be.caps().random_access,
            Err(_) => return Ok(false),
        };
        if random_access {
            return Ok(false);
        }
        let root = self.register_archive_temp(&archive);
        if Self::archive_extract_marker(&archive).is_file() && root.is_dir() {
            self.archive_extracted.borrow_mut().insert(archive, root);
            return Ok(false);
        }
        self.start_archive_extract(is_left, archive, root)?;
        Ok(true)
    }

    /// 非RA 書庫の一括展開をワーカースレッドで起動する。ペインには不定の待機スピナーを出し、
    /// 進捗の % はログのインプレース行へ流す（`LogLine`/`LogUpdate`）。完了は `ArchiveDone` で
    /// `wm_timer` 経由に取り込む。中断は `TaskControl`（Esc／タスクマネージャ）で伝える。
    pub(crate) fn start_archive_extract(
        &self,
        is_left: bool,
        archive: PathBuf,
        root: PathBuf,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let id = self.next_id();
        let pid = self.progress_seq.fetch_add(1, Ordering::Relaxed);
        let name = archive
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.register_task(id, "展開", format!("{} を展開中", name), control.clone())?;
        self.archive_extracting.borrow_mut().insert(archive.clone());
        self.view(is_left).set_loading();
        let tx = self.task_tx.clone();
        let shutdown = self.shutdown.clone();
        let marker = Self::archive_extract_marker(&archive);
        std::thread::spawn(move || {
            let _ = tx.send(WorkerEvent::LogLine {
                id: pid,
                level: LogLevel::Normal,
                text: messages::archive_extract(&name),
            });
            let result: Result<ArchiveOutcome, String> = (|| {
                let backend = open_archive(&archive).map_err(|e| e.to_string())?;
                // 前回の中断残骸を捨ててクリーンに展開する。
                let _ = std::fs::remove_dir_all(&root);
                std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
                let mut cancelled = false;
                let mut last_pct = u32::MAX;
                backend
                    .extract_all(&root, &mut |_inner, done, total| {
                        if total > 0 {
                            let pct = (done.min(total) * 100 / total) as u32;
                            if pct != last_pct {
                                last_pct = pct;
                                let _ = tx.send(WorkerEvent::LogUpdate {
                                    id: pid,
                                    text: messages::archive_extract_progress(&name, pct),
                                });
                            }
                        }
                        if control.is_stopped() || shutdown.load(Ordering::Relaxed) {
                            cancelled = true;
                            return false;
                        }
                        true
                    })
                    .map_err(|e| e.to_string())?;
                if cancelled {
                    let _ = std::fs::remove_dir_all(&root);
                    return Ok(ArchiveOutcome::Cancelled);
                }
                let _ = std::fs::write(&marker, b"");
                Ok(ArchiveOutcome::Ok)
            })();
            let outcome = match result {
                Ok(o) => o,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&root);
                    ArchiveOutcome::Failed(e)
                }
            };
            // 進捗行から % を落として確定する（成否に依らず）。
            let _ = tx.send(WorkerEvent::LogUpdate { id: pid, text: messages::archive_extract(&name) });
            let _ = tx.send(WorkerEvent::ArchiveDone {
                id,
                archive,
                temp_root: root,
                outcome,
            });
        });
        Ok(())
    }

    /// 走行中の書庫一括展開を中止要求する（Esc／読込中ペイン）。ワーカは次のコールバックで
    /// 気付き、`ArchiveDone{Cancelled}` を返す。
    pub(crate) fn cancel_archive_load(&self) {
        for t in self.tasks.borrow().iter() {
            if t.text == "展開" {
                t.control.stop();
            }
        }
    }

    /// 展開できなかった書庫から実親へ抜ける（中断/失敗時の復帰）。
    pub(crate) fn exit_archive_to_parent(&self, is_left: bool) -> w::AnyResult<()> {
        loop {
            if !self.pane(is_left).borrow().is_archive() {
                break;
            }
            if self.pane(is_left).borrow_mut().to_parent().is_none() {
                break;
            }
        }
        self.reload_side(is_left)
    }

    /// 指定ペインの現在地が書庫 `archive` の中か（一括展開完了時の対象ペイン判定に使う）。
    pub(crate) fn pane_in_archive(&self, is_left: bool, archive: &Path) -> bool {
        matches!(
            self.pane(is_left).borrow().loc(),
            Location::Archive { archive: a, .. } if a == archive
        )
    }

    /// 全タブのペイン位置から「temp を保持すべき書庫」を割り出し、それ以外の登録済み temp を
    /// 裏で削除する。保持＝どれかのペインが（中に居る）or（それが見える親dirに居る）。ナビ/
    /// タブ操作の後に呼ぶ。削除は best-effort（外部が掴むファイルは残し、起動時掃除で回収）。
    pub(crate) fn cleanup_unreferenced_temps(&self) {
        use std::collections::HashSet;
        // 参照中の場所を全部集める（アクティブタブはライブ、他タブはスナップショット文字列）。
        let mut locs: Vec<Location> = vec![
            self.left_pane.borrow().loc().clone(),
            self.right_pane.borrow().loc().clone(),
        ];
        let active = self.active.get();
        for (i, t) in self.tabs.borrow().iter().enumerate() {
            if i == active {
                continue;
            }
            locs.push(Location::parse(&t.left_path));
            locs.push(Location::parse(&t.right_path));
        }
        let mut inside: HashSet<PathBuf> = HashSet::new();
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for loc in &locs {
            match loc {
                Location::Archive { archive, .. } => {
                    inside.insert(archive.clone());
                }
                Location::Real(d) => {
                    dirs.insert(d.clone());
                }
            }
        }
        let mut to_delete: Vec<PathBuf> = Vec::new();
        {
            let mut reg = self.archive_temp_dirs.borrow_mut();
            let mut extracted = self.archive_extracted.borrow_mut();
            reg.retain(|archive, root| {
                let referenced = inside.contains(archive)
                    || archive.parent().is_some_and(|p| dirs.contains(p));
                if !referenced {
                    to_delete.push(root.clone());
                    to_delete.push(Self::archive_extract_marker(archive));
                    extracted.remove(archive);
                }
                referenced
            });
        }
        if to_delete.is_empty() {
            return;
        }
        std::thread::spawn(move || {
            for p in to_delete {
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
        });
    }

    /// pid が生存しているか（`OpenProcess` 成功で生存とみなす）。死にpid temp の起動時掃除に使う。
    pub(crate) fn pid_alive(pid: u32) -> bool {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
            fn CloseHandle(h: *mut c_void) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            CloseHandle(h);
            true
        }
    }

    /// 起動時に `cache/archive/<pid>/` を走査し、生きていない pid の dir を裏で削除する
    /// （クラッシュ/前回終了の残骸回収）。生存インスタンスの temp は触らない。
    pub(crate) fn sweep_dead_pid_temps() {
        let base = data_dir().join("cache").join("archive");
        let self_pid = std::process::id();
        std::thread::spawn(move || {
            let Ok(rd) = std::fs::read_dir(&base) else {
                return;
            };
            for ent in rd.flatten() {
                if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let Some(pid) = ent
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    continue;
                };
                if pid != self_pid && !Self::pid_alive(pid) {
                    let _ = std::fs::remove_dir_all(ent.path());
                }
            }
        });
    }

    /// 書庫内にディレクトリを作る（`caps.can_mkdir` のとき。append で既存を壊さない）。
    pub(crate) fn make_directory_in_archive(&self, is_left: bool) -> w::AnyResult<()> {
        let (archive, inner) = {
            let p = self.pane(is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        let backend = match rerics_core::open_archive(&archive) {
            Ok(b) if b.caps().can_mkdir => b,
            Ok(_) => {
                self.log.warn("この書庫形式はディレクトリの作成に未対応です");
                return Ok(());
            }
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        };
        let name = self.input_with_history(
            "ディレクトリの作成",
            &messages::directory_name_question(),
            "新しいディレクトリ",
            "mkdir",
        );
        let Some(name) = name else {
            return Ok(());
        };
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let target = if inner.is_empty() {
            name.to_string()
        } else {
            format!("{inner}/{name}")
        };
        // 同名（ファイル/ディレクトリ）が既存なら、実FS のディレクトリ作成と同じくエラーにする。
        let existing: Vec<String> = backend
            .list()
            .map(|es| es.into_iter().map(|e| e.path).collect())
            .unwrap_or_default();
        let prefix = format!("{target}/");
        if existing.iter().any(|p| *p == target || p.starts_with(&prefix)) {
            let line = messages::create_directory_failure(name, "すでに存在します");
            self.log.error(&line);
            dialog::message_box(&self.wnd, "ディレクトリの作成", &line, dialog::MessageStyle::Error);
            return Ok(());
        }
        let mut writer = match rerics_core::open_archive_writer(&archive) {
            Ok(w) => w,
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        };
        if let Err(e) = writer.mkdir(&target) {
            let line = messages::create_directory_failure(name, &e.to_string());
            self.log.error(&line);
            dialog::message_box(&self.wnd, "ディレクトリの作成", &line, dialog::MessageStyle::Error);
            return Ok(());
        }
        self.log.normal(&messages::create_directory(name));
        self.reload_side_focus(is_left, name, false)?;
        Ok(())
    }

    /// 書庫内の選択項目を反対側ペイン（実FS）へ取り出す（展開コピー）。
    pub(crate) fn extract_from_archive(&self, is_left: bool) -> w::AnyResult<()> {
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let (archive, inner) = {
            let p = self.pane(is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        let dst_dir = match self.pane(!is_left).borrow().as_real_path() {
            Some(p) => p.to_path_buf(),
            None => {
                self.log.warn("取り出し先が実フォルダではありません");
                return Ok(());
            }
        };
        self.start_extract(archive, inner, names, dst_dir)
    }

    /// 実FS の選択項目を反対側ペイン（書庫）へ追加する。move なら追加成功後に実FS の元を消す。
    /// 同名エントリがあれば追加方式（append／再構築して置換）を尋ねる。
    pub(crate) fn add_to_archive(&self, is_left: bool, move_it: bool) -> w::AnyResult<()> {
        fn inner_join(prefix: &str, name: &str) -> String {
            if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            }
        }
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let src_dir = match self.pane(is_left).borrow().as_real_path() {
            Some(p) => p.to_path_buf(),
            None => {
                self.log.warn("追加元が実フォルダではありません");
                return Ok(());
            }
        };
        let (archive, inner) = {
            let p = self.pane(!is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        let backend = match rerics_core::open_archive(&archive) {
            Ok(b) => b,
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        };
        if !backend.caps().can_add {
            self.log.warn("この書庫形式はファイルの追加に未対応です");
            return Ok(());
        }
        // 同名衝突をスキャンして方式を決める（衝突ゼロなら無言で append）。
        let existing: Vec<String> = backend
            .list()
            .map(|es| es.into_iter().map(|e| e.path).collect())
            .unwrap_or_default();
        let colliding: Vec<String> = names
            .iter()
            .filter(|n| {
                let t = inner_join(&inner, n);
                let pfx = format!("{t}/");
                existing.iter().any(|p| *p == t || p.starts_with(&pfx))
            })
            .cloned()
            .collect();
        let mode = if !colliding.is_empty() {
            let summary = format!(
                "{} 個が書庫内の既存と同名です。\n追加方式を選んでください。",
                colliding.len()
            );
            match dialog::archive_add_box(&self.wnd, &summary) {
                Some(m) => m,
                None => return Ok(()),
            }
        } else {
            dialog::ArchiveAddMode::Append
        };
        // zip は同名エントリの追記ができない。スキップは衝突分を除いて append、
        // 置換は全件を再構築（rebuild）で足す。move のとき元を消すのは実際に足した分だけ。
        let targets: Vec<String> = match mode {
            dialog::ArchiveAddMode::Append => {
                names.into_iter().filter(|n| !colliding.contains(n)).collect()
            }
            dialog::ArchiveAddMode::Rebuild => names,
        };
        if targets.is_empty() {
            self.log.warn(&format!("{} 件すべて同名のためスキップしました", colliding.len()));
            return Ok(());
        }
        self.start_archive_add(archive, inner, src_dir, targets, move_it, mode, is_left)
    }

    /// 書庫への追加をワーカースレッドで起動する。`mode` に応じて append／再構築を選び、
    /// move なら全件成功後に実FS の元を削除する。完了で関与した両ペインを再読込させる。
    pub(crate) fn start_archive_add(
        &self,
        archive: PathBuf,
        inner: String,
        src_dir: PathBuf,
        names: Vec<String>,
        move_it: bool,
        mode: dialog::ArchiveAddMode,
        is_left: bool,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let label = if move_it { "書庫へ移動" } else { "書庫へ追加" };
        let desc = format!("{} -> {}", short_desc(&names), archive.display());
        self.register_task(id, label, desc, control)?;
        std::thread::spawn(move || {
            let summary = match mode {
                dialog::ArchiveAddMode::Append => {
                    rerics_core::run_archive_add(&host, &src_dir, &names, &archive, &inner)
                }
                dialog::ArchiveAddMode::Rebuild => {
                    rerics_core::run_archive_rebuild(&host, &src_dir, &names, &archive, &inner)
                }
            };
            // move は全件成功（エラー無し・未中断）のときだけ実FS の元を削除する。
            if move_it && summary.err == 0 && !summary.cancelled {
                for name in &names {
                    let p = src_dir.join(name);
                    let r = if p.is_dir() {
                        std::fs::remove_dir_all(&p)
                    } else {
                        std::fs::remove_file(&p)
                    };
                    if let Err(e) = r {
                        let _ = host.tx.send(WorkerEvent::Log {
                            level: LogLevel::Error,
                            text: messages::delete_failure(name, &e.to_string()),
                        });
                    }
                }
            }
            let _ = host.tx.send(WorkerEvent::ArchiveWriteDone { id, src_is_left: is_left });
        });
        Ok(())
    }

    /// 書庫内の削除/改名をワーカースレッドで起動する（どちらも全体リビルド）。完了で
    /// 関与ペインを再読込する。
    pub(crate) fn start_archive_op(
        &self,
        archive: PathBuf,
        inner: String,
        op: ArchiveOp,
        is_left: bool,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let (label, desc): (&str, String) = match &op {
            ArchiveOp::Delete(names) => {
                ("書庫から削除", format!("{} ({})", short_desc(names), archive.display()))
            }
            ArchiveOp::Rename { old, new } => ("書庫内で改名", format!("{old} -> {new}")),
        };
        self.register_task(id, label, desc, control)?;
        std::thread::spawn(move || {
            match op {
                ArchiveOp::Delete(names) => {
                    rerics_core::run_archive_delete(&host, &archive, &inner, &names);
                }
                ArchiveOp::Rename { old, new } => {
                    rerics_core::run_archive_rename(&host, &archive, &inner, &old, &new);
                }
            }
            let _ = host.tx.send(WorkerEvent::ArchiveWriteDone { id, src_is_left: is_left });
        });
        Ok(())
    }

    /// 書庫内のカーソル項目を改名する（`caps.can_rename` のとき・全体リビルド）。
    /// 新しい名前が既存と衝突する場合は安全側でエラーにする（実FS と違い上書きしない）。
    pub(crate) fn rename_in_archive(&self, is_left: bool) -> w::AnyResult<()> {
        let (archive, inner) = {
            let p = self.pane(is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        let backend = match rerics_core::open_archive(&archive) {
            Ok(b) if b.caps().can_rename => b,
            Ok(_) => {
                self.log.warn("この書庫形式は名前の変更に未対応です");
                return Ok(());
            }
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        };
        let (old, old_is_dir) = {
            let view = self.view(is_left);
            let state = view.state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent => (it.name.clone(), it.is_dir),
                _ => return Ok(()),
            }
        };
        let new = dialog::input_box_select(
            &self.wnd,
            "名前の変更",
            "新しい名前を入力して下さい。",
            &old,
            dialog::InputMode::Plain,
            dialog::InputSelect::BeforeExt { is_dir: old_is_dir },
        );
        let Some(new) = new else {
            return Ok(());
        };
        let new = new.trim();
        if new.is_empty() || new == old.as_str() {
            return Ok(());
        }
        let target = if inner.is_empty() {
            new.to_string()
        } else {
            format!("{inner}/{new}")
        };
        let existing: Vec<String> = backend
            .list()
            .map(|es| es.into_iter().map(|e| e.path).collect())
            .unwrap_or_default();
        let pfx = format!("{target}/");
        if existing.iter().any(|p| *p == target || p.starts_with(&pfx)) {
            let line = messages::rename_failure(&old, "同名が存在します");
            self.log.error(&line);
            dialog::message_box(&self.wnd, "名前の変更", &line, dialog::MessageStyle::Error);
            return Ok(());
        }
        self.start_archive_op(archive, inner, ArchiveOp::Rename { old, new: new.to_string() }, is_left)
    }

    /// 書庫内の選択（無ければカーソル）を確認付きで削除する（`caps.can_remove`・全体リビルド）。
    pub(crate) fn delete_in_archive(&self, is_left: bool) -> w::AnyResult<()> {
        let (archive, inner) = {
            let p = self.pane(is_left).borrow();
            match p.loc() {
                Location::Archive { archive, inner } => (archive.clone(), inner.clone()),
                _ => return Ok(()),
            }
        };
        match rerics_core::open_archive(&archive) {
            Ok(b) if b.caps().can_remove => {}
            Ok(_) => {
                self.log.warn("この書庫形式は削除に未対応です");
                return Ok(());
            }
            Err(e) => {
                self.log.error(&format!("書庫を開けません: {}", e));
                return Ok(());
            }
        }
        let names: Vec<String> = {
            let view = self.view(is_left);
            let state = view.state();
            let s = state.borrow();
            let selected: Vec<String> = s
                .items
                .iter()
                .filter(|it| it.selected && !it.is_parent)
                .map(|it| it.name.clone())
                .collect();
            if selected.is_empty() {
                match s.items.get(s.cursor) {
                    Some(it) if !it.is_parent => vec![it.name.clone()],
                    _ => Vec::new(),
                }
            } else {
                selected
            }
        };
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        if self.config.borrow().file_ops.ask_before_delete {
            let short = crate::short_desc(&names);
            let ans = dialog::message_box(
                &self.wnd,
                "削除",
                &messages::delete_question(&short),
                dialog::MessageStyle::YesNo,
            );
            if ans != dialog::MessageResult::Yes {
                return Ok(());
            }
        }
        self.start_archive_op(archive, inner, ArchiveOp::Delete(names), is_left)
    }
}
