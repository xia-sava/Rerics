    use super::*;
    use crate::LogLevel;
    use crate::archive::ArchiveEntry;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// テスト専用の一時ディレクトリ。Drop で再帰削除する。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("rerics_optest_{}_{n}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        fn write_file(&self, name: &str, body: &str) {
            std::fs::write(self.join(name), body).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// `dir` 直下に書庫再構築の一時ファイル（`*.rerics-tmp-*`）が残っていないか。
    fn has_rewrite_tmp(dir: &std::path::Path) -> bool {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".rerics-tmp"))
    }

    /// ログを記録し、指定件数のコピー後に中止を返すフェイクホスト。
    struct FakeHost {
        logs: RefCell<Vec<(LogLevel, String)>>,
        cancel_after: isize,
        conflict: ConflictResolution,
        delete_warn: DeleteWarnChoice,
        copy_opts: CopyOptions,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                logs: RefCell::new(Vec::new()),
                cancel_after: -1,
                conflict: ConflictResolution::Overwrite,
                delete_warn: DeleteWarnChoice::Yes,
                copy_opts: CopyOptions::default(),
            }
        }

        fn cancelling(after: isize) -> Self {
            Self { cancel_after: after, ..Self::new() }
        }

        fn with_conflict(res: ConflictResolution) -> Self {
            Self { conflict: res, ..Self::new() }
        }

        fn with_delete_warn(choice: DeleteWarnChoice) -> Self {
            Self { delete_warn: choice, ..Self::new() }
        }

        fn lines(&self) -> Vec<String> {
            self.logs.borrow().iter().map(|(_, t)| t.clone()).collect()
        }
    }

    impl OperationHost for FakeHost {
        fn log(&self, level: LogLevel, text: &str) {
            self.logs.borrow_mut().push((level, text.to_owned()));
        }

        fn cancelled(&self) -> bool {
            if self.cancel_after < 0 {
                return false;
            }
            let copies = self
                .logs
                .borrow()
                .iter()
                .filter(|(lvl, t)| {
                    *lvl == LogLevel::Normal
                        && (t.starts_with("Copy ")
                            || t.starts_with("Move ")
                            || t.starts_with("Compress "))
                })
                .count() as isize;
            copies >= self.cancel_after
        }

        fn wait_while_suspended(&self) {}

        fn resolve_conflict(&self, _name: &str) -> ConflictResolution {
            self.conflict.clone()
        }

        fn confirm_delete_attr(&self, _name: &str, _attr: &str) -> DeleteWarnChoice {
            self.delete_warn
        }

        fn copy_options(&self) -> CopyOptions {
            self.copy_opts
        }
    }

    #[test]
    fn copy_file_succeeds() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "hello");
        let host = FakeHost::new();
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(sum, OpSummary { ok: 1, skip: 0, err: 0, cancelled: false });
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "hello");
        assert!(src.join("a.txt").exists());
    }

    #[test]
    fn move_file_removes_source() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "hello");
        let host = FakeHost::new();
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], true);
        assert_eq!(sum.ok, 1);
        assert!(!src.join("a.txt").exists());
        assert!(dst.join("a.txt").exists());
    }

    #[test]
    fn copy_logs_start_and_end_frames() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "hello");
        let host = FakeHost::new();
        run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        let lines = host.lines();
        assert_eq!(lines.first().map(String::as_str), Some("コピー開始"));
        assert_eq!(lines.last().map(String::as_str), Some("コピー終了"));

        // 移動は「移動開始」/「移動終了」。
        src.write_file("b.txt", "x");
        let host2 = FakeHost::new();
        run_copy(&host2, &src.path, &dst.path, &["b.txt".to_owned()], true);
        let l2 = host2.lines();
        assert_eq!(l2.first().map(String::as_str), Some("移動開始"));
        assert_eq!(l2.last().map(String::as_str), Some("移動終了"));
    }

    #[test]
    fn copy_error_ends_with_warning_frame() {
        let src = TempDir::new();
        let dst = TempDir::new();
        let host = FakeHost::new();
        run_copy(&host, &src.path, &dst.path, &["nope.txt".to_owned()], false);
        assert_eq!(host.lines().last().map(String::as_str), Some("コピー警告終了"));
    }

    #[test]
    fn copy_cancel_ends_with_abort_frame() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "x");
        let host = FakeHost::cancelling(0);
        run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(host.lines().last().map(String::as_str), Some("コピー中止"));
    }

    #[test]
    fn delete_logs_start_and_end_frames() {
        let dir = TempDir::new();
        dir.write_file("a.txt", "x");
        let host = FakeHost::new();
        run_delete(&host, &dir.path, &["a.txt".to_owned()]);
        let lines = host.lines();
        assert_eq!(lines.first().map(String::as_str), Some("削除開始"));
        assert_eq!(lines.last().map(String::as_str), Some("削除終了"));
    }

    #[test]
    fn calc_size_counts_files_dirs_and_bytes() {
        let base = TempDir::new();
        base.write_file("a.txt", "12345"); // 5 bytes
        std::fs::create_dir_all(base.join("sub")).unwrap();
        base.write_file("sub/b.txt", "xyz"); // 3 bytes
        let host = FakeHost::new();
        let info = run_calc_size(&host, &base.path, &["a.txt".to_owned(), "sub".to_owned()]);
        assert_eq!(info.files, 2, "a.txt と sub/b.txt の2ファイル");
        assert_eq!(info.dirs, 1, "選んだ sub 自身を数える");
        assert_eq!(info.bytes, 8, "5 + 3 バイト");
    }

    #[test]
    fn calc_size_groups_accumulates_across_dirs() {
        let d1 = TempDir::new();
        d1.write_file("a.txt", "12345"); // 5 bytes
        let d2 = TempDir::new();
        d2.write_file("b.txt", "xyz"); // 3 bytes
        let host = FakeHost::new();
        let groups = vec![
            (d1.path.clone(), vec!["a.txt".to_owned()]),
            (d2.path.clone(), vec!["b.txt".to_owned()]),
        ];
        let info = run_calc_size_groups(&host, &groups);
        assert_eq!(info.files, 2, "両ディレクトリのファイルを合算");
        assert_eq!(info.dirs, 0, "ファイルのみ＝フォルダは数えない");
        assert_eq!(info.bytes, 8, "5 + 3 バイトを合算");
    }

    #[test]
    fn copy_missing_source_reports_error() {
        let src = TempDir::new();
        let dst = TempDir::new();
        let host = FakeHost::new();
        let sum = run_copy(&host, &src.path, &dst.path, &["nope.txt".to_owned()], false);
        assert_eq!(sum.err, 1);
        assert_eq!(sum.ok, 0);
        assert!(host.lines().iter().any(|l| l.contains("コピーに失敗しました")));
        assert!(host.lines().iter().any(|l| l == "0 Success, 0 Skip, 1 Error"));
    }

    #[test]
    fn copy_file_onto_existing_dir_skips() {
        // src に file a、dst に同名 dir a が既存＝種別不一致 → 上書きせずスキップ。
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a", "hello");
        std::fs::create_dir_all(dst.join("a")).unwrap();
        let host = FakeHost::new();
        let sum = run_copy(&host, &src.path, &dst.path, &["a".to_owned()], false);
        assert_eq!(sum, OpSummary { ok: 0, skip: 1, err: 0, cancelled: false });
        assert!(host.lines().iter().any(|l| l.contains("ディレクトリ属性が異なる")));
        assert!(dst.join("a").is_dir(), "既存ディレクトリは上書きされない");
    }

    #[test]
    fn copy_dir_onto_existing_file_skips() {
        // src に dir a、dst に同名 file a が既存＝種別不一致 → スキップ。
        let src = TempDir::new();
        let dst = TempDir::new();
        std::fs::create_dir_all(src.join("a")).unwrap();
        dst.write_file("a", "x");
        let host = FakeHost::new();
        let sum = run_copy(&host, &src.path, &dst.path, &["a".to_owned()], false);
        assert_eq!(sum.skip, 1);
        assert_eq!(sum.ok, 0);
        assert!(host.lines().iter().any(|l| l.contains("ディレクトリ属性が異なる")));
        assert!(dst.join("a").is_file(), "既存ファイルは残る");
    }

    #[test]
    #[cfg(windows)]
    fn copy_dir_replicates_modified_date() {
        // copy_date を有効にすると、コピー先ディレクトリの更新日時が元と一致する。
        let src = TempDir::new();
        let dst = TempDir::new();
        std::fs::create_dir_all(src.join("d")).unwrap();
        src.write_file("d/inner.txt", "x");
        let src_mtime = std::fs::metadata(src.join("d")).unwrap().modified().unwrap();
        let host = FakeHost {
            copy_opts: CopyOptions { copy_attribute: true, copy_date: true },
            ..FakeHost::new()
        };
        run_copy(&host, &src.path, &dst.path, &["d".to_owned()], false);
        let dst_mtime = std::fs::metadata(dst.join("d")).unwrap().modified().unwrap();
        assert_eq!(dst_mtime, src_mtime, "コピー先ディレクトリの更新日時が元と一致するはず");
    }

    #[test]
    #[cfg(windows)]
    fn copy_dir_without_copy_date_uses_fresh_time() {
        // copy_date を無効（既定）にすると、コピー先の更新日時は元と一致しない（新規作成時刻）。
        let src = TempDir::new();
        let dst = TempDir::new();
        std::fs::create_dir_all(src.join("d")).unwrap();
        src.write_file("d/inner.txt", "x");
        let src_mtime = std::fs::metadata(src.join("d")).unwrap().modified().unwrap();
        let host = FakeHost::new(); // copy_opts 既定＝複製しない
        run_copy(&host, &src.path, &dst.path, &["d".to_owned()], false);
        let dst_mtime = std::fs::metadata(dst.join("d")).unwrap().modified().unwrap();
        assert_ne!(dst_mtime, src_mtime, "複製しない設定では元の日時を引き継がない");
    }

    #[test]
    fn copy_same_path_guarded() {
        let dir = TempDir::new();
        dir.write_file("a.txt", "x");
        let host = FakeHost::new();
        let sum = run_copy(&host, &dir.path, &dir.path, &["a.txt".to_owned()], false);
        assert_eq!(sum.err, 1);
        assert!(host.lines().iter().any(|l| l.starts_with("コピー先が同じです")));
    }

    #[test]
    fn delete_file_succeeds() {
        let dir = TempDir::new();
        dir.write_file("a.txt", "x");
        let host = FakeHost::new();
        let sum = run_delete(&host, &dir.path, &["a.txt".to_owned()]);
        assert_eq!(sum.ok, 1);
        assert!(!dir.join("a.txt").exists());
    }

    #[test]
    fn delete_readonly_warns_and_no_keeps_file() {
        let dir = TempDir::new();
        dir.write_file("a.txt", "x");
        let ro = dir.join("a.txt");
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&ro, perms).unwrap();
        let host = FakeHost::with_delete_warn(DeleteWarnChoice::No);
        let sum = run_delete(&host, &dir.path, &["a.txt".to_owned()]);
        assert_eq!(sum.ok, 0);
        assert!(ro.exists());
        // 後始末のため属性を戻す（Windows の読み取り専用ビットを落とす意図）。
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&ro, perms).unwrap();
    }

    #[test]
    fn delete_readonly_yes_clears_and_deletes() {
        let dir = TempDir::new();
        dir.write_file("a.txt", "x");
        let ro = dir.join("a.txt");
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&ro, perms).unwrap();
        let host = FakeHost::with_delete_warn(DeleteWarnChoice::Yes);
        let sum = run_delete(&host, &dir.path, &["a.txt".to_owned()]);
        assert_eq!(sum.ok, 1);
        assert!(!ro.exists());
    }

    #[test]
    fn delete_directory_recurses_and_counts_each_item() {
        let root = TempDir::new();
        let d = root.join("d");
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(d.join("c.txt"), "c").unwrap();
        std::fs::write(sub.join("a.txt"), "a").unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();
        let host = FakeHost::new();
        let sum = run_delete(&host, &root.path, &["d".to_owned()]);
        // ファイル3＋ディレクトリ2（sub・d）の計5件。
        assert_eq!(sum.ok, 5);
        assert_eq!(sum.err, 0);
        assert!(!d.exists());
    }

    #[test]
    fn delete_keeps_directory_when_child_kept() {
        let root = TempDir::new();
        let d = root.join("d");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("plain.txt"), "p").unwrap();
        let ro = d.join("ro.txt");
        std::fs::write(&ro, "r").unwrap();
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&ro, perms).unwrap();
        // 属性ファイルは「いいえ」で残す → 親ディレクトリも空にならず残る。
        let host = FakeHost::with_delete_warn(DeleteWarnChoice::No);
        let sum = run_delete(&host, &root.path, &["d".to_owned()]);
        assert_eq!(sum.ok, 1); // plain.txt のみ削除。
        assert!(ro.exists());
        assert!(d.exists());
        // 後始末（Windows の読み取り専用ビットを落とす意図）。
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&ro, perms).unwrap();
    }

    #[test]
    fn delete_clears_nested_readonly_child_on_yes() {
        let root = TempDir::new();
        let d = root.join("d");
        std::fs::create_dir_all(&d).unwrap();
        let ro = d.join("ro.txt");
        std::fs::write(&ro, "r").unwrap();
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&ro, perms).unwrap();
        let host = FakeHost::with_delete_warn(DeleteWarnChoice::Yes);
        let sum = run_delete(&host, &root.path, &["d".to_owned()]);
        assert_eq!(sum.ok, 2); // ro.txt ＋ d。
        assert_eq!(sum.err, 0);
        assert!(!d.exists());
    }

    #[test]
    fn cancel_stops_early() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "1");
        src.write_file("b.txt", "2");
        let host = FakeHost::cancelling(2);
        let names = vec!["a.txt".to_owned(), "b.txt".to_owned()];
        let sum = run_copy(&host, &src.path, &dst.path, &names, false);
        assert!(sum.cancelled);
        assert_eq!(sum.ok, 1);
        assert!(dst.join("a.txt").exists());
        assert!(!dst.join("b.txt").exists());
    }

    #[test]
    fn cancel_during_file_copy_leaves_no_partial() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "hello");
        // 1件目のコピー中（begin_progress で Copy 行が出た直後）に中止が立つ。
        let host = FakeHost::cancelling(1);
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert!(sum.cancelled);
        assert_eq!(sum.ok, 0);
        assert!(!dst.join("a.txt").exists());
    }

    #[test]
    fn conflict_skip_keeps_destination() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "new");
        dst.write_file("a.txt", "old");
        let host = FakeHost::with_conflict(ConflictResolution::Skip);
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(sum.skip, 1);
        assert_eq!(sum.ok, 0);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "old");
    }

    #[test]
    fn conflict_overwrite_replaces_destination() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "new");
        dst.write_file("a.txt", "old");
        let host = FakeHost::with_conflict(ConflictResolution::Overwrite);
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(sum.ok, 1);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "new");
    }

    #[test]
    fn conflict_rename_copies_to_new_name() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "new");
        dst.write_file("a.txt", "old");
        let host = FakeHost::with_conflict(ConflictResolution::Rename("a2.txt".to_owned()));
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(sum.ok, 1);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(dst.join("a2.txt")).unwrap(), "new");
    }

    #[test]
    fn conflict_force_overwrites_readonly() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "new");
        dst.write_file("a.txt", "old");
        let ro = dst.join("a.txt");
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&ro, perms).unwrap();
        let host = FakeHost::with_conflict(ConflictResolution::OverwriteForce);
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(sum.ok, 1);
        assert_eq!(std::fs::read_to_string(&ro).unwrap(), "new");
    }

    #[test]
    fn progress_tracker_silent_before_three_seconds() {
        let mut t = ProgressTracker::new();
        assert_eq!(t.tick_with_elapsed(0, 50, 100), None);
        assert_eq!(t.tick_with_elapsed(2, 50, 100), None);
    }

    #[test]
    fn progress_tracker_reports_changed_percent_after_three_seconds() {
        let mut t = ProgressTracker::new();
        assert_eq!(t.tick_with_elapsed(3, 50, 100), Some(50));
        assert_eq!(t.tick_with_elapsed(4, 50, 100), None);
        assert_eq!(t.tick_with_elapsed(5, 73, 100), Some(73));
    }

    #[test]
    fn progress_tracker_guards_zero_total() {
        let mut t = ProgressTracker::new();
        assert_eq!(t.tick_with_elapsed(9, 0, 0), None);
    }

    #[test]
    fn conflict_cancel_aborts() {
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "new");
        src.write_file("b.txt", "new");
        dst.write_file("a.txt", "old");
        let host = FakeHost::with_conflict(ConflictResolution::Cancel);
        let names = vec!["a.txt".to_owned(), "b.txt".to_owned()];
        let sum = run_copy(&host, &src.path, &dst.path, &names, false);
        assert!(sum.cancelled);
        assert!(!dst.join("b.txt").exists());
    }

    /// 取り出しテスト用の擬似書庫（メモリ上のエントリ列＋bytes）。
    struct MockArchive {
        entries: Vec<ArchiveEntry>,
        data: std::collections::HashMap<String, Vec<u8>>,
    }

    impl MockArchive {
        fn new(files: &[(&str, &[u8])]) -> Self {
            let mut entries = Vec::new();
            let mut data = std::collections::HashMap::new();
            for (path, body) in files {
                entries.push(ArchiveEntry {
                    path: (*path).to_string(),
                    is_dir: false,
                    size: Some(body.len() as u64),
                    packed_size: None,
                    mtime: None,
                    is_encrypted: false,
                });
                data.insert((*path).to_string(), body.to_vec());
            }
            Self { entries, data }
        }
    }

    impl crate::ArchiveBackend for MockArchive {
        fn caps(&self) -> crate::Caps {
            crate::Caps { random_access: true, ..Default::default() }
        }
        fn list(&self) -> std::io::Result<Vec<ArchiveEntry>> {
            Ok(self.entries.clone())
        }
        fn read(&self, inner: &str) -> std::io::Result<Vec<u8>> {
            self.data
                .get(inner)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))
        }
    }

    #[test]
    fn extract_file_and_dir_recurses() {
        let dst = TempDir::new();
        let arc = MockArchive::new(&[("a.txt", b"AAA"), ("sub/c.txt", b"CCC"), ("sub/d.txt", b"DDD")]);
        let entries = arc.entries.clone();
        let host = FakeHost::new();
        let names = vec!["a.txt".to_owned(), "sub".to_owned()];
        let sum = run_extract(&host, &arc, &entries, "", &names, &dst.path);
        // a.txt（ファイル）＋ sub（dir）＋ sub 配下2ファイル＝ ok 4。
        assert_eq!(sum.err, 0);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "AAA");
        assert_eq!(std::fs::read_to_string(dst.path.join("sub").join("c.txt")).unwrap(), "CCC");
        assert_eq!(std::fs::read_to_string(dst.path.join("sub").join("d.txt")).unwrap(), "DDD");
    }

    #[test]
    fn extract_subdir_only() {
        let dst = TempDir::new();
        let arc = MockArchive::new(&[("docs/readme.txt", b"R")]);
        let entries = arc.entries.clone();
        let host = FakeHost::new();
        // docs 直下から readme.txt だけを取り出す（src_inner="docs"）。
        let sum = run_extract(&host, &arc, &entries, "docs", &["readme.txt".to_owned()], &dst.path);
        assert_eq!(sum.ok, 1);
        assert_eq!(std::fs::read_to_string(dst.join("readme.txt")).unwrap(), "R");
    }

    #[test]
    fn extract_rejects_zip_slip_name() {
        let dst = TempDir::new();
        let arc = MockArchive::new(&[("ok.txt", b"X")]);
        let entries = arc.entries.clone();
        let host = FakeHost::new();
        // ".." を含む名前は弾く（dst 外への書き出し防止）。
        let sum = run_extract(&host, &arc, &entries, "", &["..".to_owned()], &dst.path);
        assert_eq!(sum.err, 1);
        assert_eq!(sum.ok, 0);
        // dst の親に何も書かれていない。
        assert!(!dst.path.parent().unwrap().join("evil").exists());
    }

    #[test]
    fn extract_all_to_writes_every_entry() {
        let dst = TempDir::new();
        let arc = MockArchive::new(&[("a.txt", b"AAA"), ("sub/c.txt", b"CCC")]);
        let n = crate::extract_all_to(&arc, &dst.path).unwrap();
        assert_eq!(n, 2, "should report extracted file count");
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "AAA");
        assert_eq!(
            std::fs::read_to_string(dst.path.join("sub").join("c.txt")).unwrap(),
            "CCC"
        );
    }

    #[test]
    fn extract_conflict_skip_keeps_destination() {
        let dst = TempDir::new();
        dst.write_file("a.txt", "old");
        let arc = MockArchive::new(&[("a.txt", b"new")]);
        let entries = arc.entries.clone();
        let host = FakeHost::with_conflict(ConflictResolution::Skip);
        let sum = run_extract(&host, &arc, &entries, "", &["a.txt".to_owned()], &dst.path);
        assert_eq!(sum.skip, 1);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "old");
    }

    #[test]
    fn extract_conflict_overwrite_replaces() {
        let dst = TempDir::new();
        dst.write_file("a.txt", "old");
        let arc = MockArchive::new(&[("a.txt", b"new")]);
        let entries = arc.entries.clone();
        let host = FakeHost::with_conflict(ConflictResolution::Overwrite);
        let sum = run_extract(&host, &arc, &entries, "", &["a.txt".to_owned()], &dst.path);
        assert_eq!(sum.ok, 1);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "new");
    }

    #[test]
    fn compress_roundtrips_files_and_dirs() {
        let src = TempDir::new();
        src.write_file("a.txt", "alpha");
        std::fs::create_dir(src.join("sub")).unwrap();
        std::fs::write(src.join("sub").join("c.txt"), "charlie").unwrap();
        let zip = src.join("out.zip");
        let host = FakeHost::new();
        let sum = run_compress(
            &host,
            &src.path,
            &["a.txt".to_owned(), "sub".to_owned()],
            &zip,
        );
        // a.txt（ファイル）＋ sub（dir）＋ sub/c.txt（ファイル）。
        assert_eq!(sum.err, 0);
        assert!(zip.is_file());
        // ZipBackend で読み戻して内容を確認する。
        let be = crate::open_archive(&zip).unwrap();
        assert_eq!(be.read("a.txt").unwrap(), b"alpha");
        assert_eq!(be.read("sub/c.txt").unwrap(), b"charlie");
    }

    #[test]
    fn compress_cancel_removes_partial() {
        let src = TempDir::new();
        src.write_file("a.txt", "x");
        src.write_file("b.txt", "y");
        let zip = src.join("out.zip");
        // 1件圧縮した時点で中止が立つ（"Compress " 行を数える）。
        let host = FakeHost {
            cancel_after: 1,
            ..FakeHost::new()
        };
        let sum = run_compress(
            &host,
            &src.path,
            &["a.txt".to_owned(), "b.txt".to_owned()],
            &zip,
        );
        assert!(sum.cancelled);
        // 中止時は作りかけ zip を残さない。
        assert!(!zip.exists());
    }

    #[test]
    fn compress_7z_roundtrips_files_and_dirs() {
        let src = TempDir::new();
        src.write_file("a.txt", "alpha");
        std::fs::create_dir(src.join("sub")).unwrap();
        std::fs::write(src.join("sub").join("c.txt"), "charlie").unwrap();
        let dst = src.join("out.7z");
        let host = FakeHost::new();
        let sum = run_compress_7z(
            &host,
            &src.path,
            &["a.txt".to_owned(), "sub".to_owned()],
            &dst,
        );
        assert_eq!(sum.err, 0);
        assert!(dst.is_file());
        let be = crate::open_archive(&dst).unwrap();
        assert_eq!(be.read("a.txt").unwrap(), b"alpha");
        assert_eq!(be.read("sub/c.txt").unwrap(), b"charlie");
    }

    #[test]
    fn compress_xz_single_roundtrips() {
        let src = TempDir::new();
        src.write_file("a.txt", "alpha");
        let dst = src.join("a.txt.xz");
        let host = FakeHost::new();
        let sum = run_compress_xz_single(&host, &src.path, "a.txt", &dst);
        assert_eq!(sum.err, 0);
        assert!(dst.is_file());
        // 単体圧縮は 1 エントリ（内側名は圧縮拡張子を除いた元名）として読み戻せる。
        let be = crate::open_archive(&dst).unwrap();
        assert_eq!(be.read("a.txt").unwrap(), b"alpha");
    }

    #[test]
    fn compress_tar_xz_roundtrips_files_and_dirs() {
        let src = TempDir::new();
        src.write_file("a.txt", "alpha");
        std::fs::create_dir(src.join("sub")).unwrap();
        std::fs::write(src.join("sub").join("c.txt"), "charlie").unwrap();
        let dst = src.join("out.tar.xz");
        let host = FakeHost::new();
        let sum = run_compress_tar_xz(
            &host,
            &src.path,
            &["a.txt".to_owned(), "sub".to_owned()],
            &dst,
        );
        assert_eq!(sum.err, 0);
        assert!(dst.is_file());
        let be = crate::open_archive(&dst).unwrap();
        assert_eq!(be.read("a.txt").unwrap(), b"alpha");
        assert_eq!(be.read("sub/c.txt").unwrap(), b"charlie");
    }

    #[test]
    fn compress_7z_cancel_removes_partial() {
        let src = TempDir::new();
        src.write_file("a.txt", "x");
        src.write_file("b.txt", "y");
        let dst = src.join("out.7z");
        let host = FakeHost { cancel_after: 1, ..FakeHost::new() };
        let sum = run_compress_7z(
            &host,
            &src.path,
            &["a.txt".to_owned(), "b.txt".to_owned()],
            &dst,
        );
        assert!(sum.cancelled);
        assert!(!dst.exists());
    }

    /// 標準 CRC-32（IEEE・反転多項式）。手組み stored zip の検証値用。
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

    /// 無圧縮(stored)・UTF-8 フラグ無しで任意の生バイト名 zip を手組みする
    /// （CP932 名の検証用。高レベル writer は UTF-8 フラグを立ててしまうため）。
    fn build_stored_zip_raw(path: &Path, entries: &[(&[u8], &[u8])]) {
        fn u16le(v: &mut Vec<u8>, x: u16) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        fn u32le(v: &mut Vec<u8>, x: u32) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data) in entries {
            let crc = crc32(data);
            let off = out.len() as u32;
            // local file header
            u32le(&mut out, 0x0403_4b50);
            u16le(&mut out, 20);
            u16le(&mut out, 0); // flags（UTF-8 ビット無し）
            u16le(&mut out, 0); // method stored
            u16le(&mut out, 0);
            u16le(&mut out, 0);
            u32le(&mut out, crc);
            u32le(&mut out, data.len() as u32);
            u32le(&mut out, data.len() as u32);
            u16le(&mut out, name.len() as u16);
            u16le(&mut out, 0);
            out.extend_from_slice(name);
            out.extend_from_slice(data);
            // central directory header
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

    /// CP932（UTF-8 フラグ無し）の "日本語.txt" の生バイト列。
    fn cp932_nihongo_txt() -> Vec<u8> {
        let mut n = vec![0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea];
        n.extend_from_slice(b".txt");
        n
    }

    #[test]
    fn archive_add_appends_and_preserves_existing_cp932() {
        let dir = TempDir::new();
        let zip = dir.join("a.zip");
        let cp = cp932_nihongo_txt();
        build_stored_zip_raw(&zip, &[(&cp, b"orig")]);
        let src = TempDir::new();
        src.write_file("new.txt", "hello");
        let host = FakeHost::new();
        let sum = run_archive_add(&host, &src.path, &["new.txt".to_owned()], &zip, "");
        assert_eq!(sum.err, 0);
        assert_eq!(sum.ok, 1);
        let be = crate::open_archive(&zip).unwrap();
        let paths: Vec<String> = be.list().unwrap().into_iter().map(|e| e.path).collect();
        // append は既存の生バイト名を触らない＝CP932 名が壊れない。
        assert!(paths.iter().any(|p| p == "日本語.txt"), "CP932 名保持: {paths:?}");
        assert_eq!(be.read("日本語.txt").unwrap(), b"orig");
        assert_eq!(be.read("new.txt").unwrap(), b"hello");
    }

    #[test]
    fn archive_rebuild_replaces_same_name_without_duplicate() {
        let dir = TempDir::new();
        let zip = dir.join("a.zip");
        build_stored_zip_raw(&zip, &[(b"a.txt", b"old"), (b"b.txt", b"keep")]);
        let src = TempDir::new();
        src.write_file("a.txt", "new");
        let host = FakeHost::new();
        let sum = run_archive_rebuild(&host, &src.path, &["a.txt".to_owned()], &zip, "");
        assert_eq!(sum.err, 0);
        let be = crate::open_archive(&zip).unwrap();
        let files: Vec<String> = be
            .list()
            .unwrap()
            .into_iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.path)
            .collect();
        // 同名は置換され重複しない。
        assert_eq!(files.iter().filter(|p| *p == "a.txt").count(), 1, "重複なし: {files:?}");
        assert_eq!(be.read("a.txt").unwrap(), b"new");
        assert_eq!(be.read("b.txt").unwrap(), b"keep");
    }

    #[test]
    fn archive_rebuild_modernizes_cp932_name() {
        let dir = TempDir::new();
        let zip = dir.join("a.zip");
        let cp = cp932_nihongo_txt();
        build_stored_zip_raw(&zip, &[(&cp, b"orig"), (b"a.txt", b"old")]);
        let src = TempDir::new();
        src.write_file("a.txt", "new");
        let host = FakeHost::new();
        let sum = run_archive_rebuild(&host, &src.path, &["a.txt".to_owned()], &zip, "");
        assert_eq!(sum.err, 0);
        let be = crate::open_archive(&zip).unwrap();
        // 触っていない CP932 名は近代化（UTF-8 化）後も同じ表示名で読める。
        assert_eq!(be.read("日本語.txt").unwrap(), b"orig");
        assert_eq!(be.read("a.txt").unwrap(), b"new");
    }

    #[test]
    fn archive_rebuild_cancel_keeps_original() {
        let dir = TempDir::new();
        let zip = dir.join("a.zip");
        let cp = cp932_nihongo_txt();
        build_stored_zip_raw(&zip, &[(&cp, b"orig"), (b"a.txt", b"old")]);
        let src = TempDir::new();
        src.write_file("a.txt", "new");
        // 即中止：書き戻しループの最初で止まる。
        let host = FakeHost::cancelling(0);
        let sum = run_archive_rebuild(&host, &src.path, &["a.txt".to_owned()], &zip, "");
        assert!(sum.cancelled);
        // 元書庫は無傷（中身も CP932 名も元のまま）。
        let be = crate::open_archive(&zip).unwrap();
        assert_eq!(be.read("a.txt").unwrap(), b"old");
        assert_eq!(be.read("日本語.txt").unwrap(), b"orig");
        // 一時ファイルは残らない。
        assert!(!has_rewrite_tmp(&dir.path));
    }

    #[test]
    fn archive_delete_removes_entry_and_preserves_cp932() {
        let dir = TempDir::new();
        let zip = dir.join("a.zip");
        let cp = cp932_nihongo_txt();
        build_stored_zip_raw(&zip, &[(&cp, b"orig"), (b"a.txt", b"AAA"), (b"b.txt", b"BBB")]);
        let host = FakeHost::new();
        let sum = run_archive_delete(&host, &zip, "", &["a.txt".to_owned()]);
        assert_eq!(sum.err, 0);
        assert_eq!(sum.ok, 1);
        let be = crate::open_archive(&zip).unwrap();
        let paths: Vec<String> = be.list().unwrap().into_iter().map(|e| e.path).collect();
        assert!(!paths.iter().any(|p| p == "a.txt"), "a.txt must be removed: {paths:?}");
        // 触っていないエントリは残り、CP932 名も保持される。
        assert_eq!(be.read("b.txt").unwrap(), b"BBB");
        assert_eq!(be.read("日本語.txt").unwrap(), b"orig");
    }

    #[test]
    fn archive_delete_dir_removes_subtree() {
        let dir = TempDir::new();
        let zip = dir.join("a.zip");
        build_stored_zip_raw(
            &zip,
            &[(b"sub/c.txt", b"C"), (b"sub/d.txt", b"D"), (b"e.txt", b"E")],
        );
        let host = FakeHost::new();
        let sum = run_archive_delete(&host, &zip, "", &["sub".to_owned()]);
        assert_eq!(sum.err, 0);
        let be = crate::open_archive(&zip).unwrap();
        let paths: Vec<String> = be.list().unwrap().into_iter().map(|e| e.path).collect();
        assert!(!paths.iter().any(|p| p.starts_with("sub/")), "subtree removed: {paths:?}");
        assert_eq!(be.read("e.txt").unwrap(), b"E");
    }

    #[test]
    fn archive_rename_file_and_preserves_cp932() {
        let dir = TempDir::new();
        let zip = dir.join("a.zip");
        let cp = cp932_nihongo_txt();
        build_stored_zip_raw(&zip, &[(&cp, b"orig"), (b"a.txt", b"AAA")]);
        let host = FakeHost::new();
        let sum = run_archive_rename(&host, &zip, "", "a.txt", "z.txt");
        assert_eq!(sum.err, 0);
        let be = crate::open_archive(&zip).unwrap();
        let paths: Vec<String> = be.list().unwrap().into_iter().map(|e| e.path).collect();
        assert!(!paths.iter().any(|p| p == "a.txt"), "old name gone: {paths:?}");
        assert_eq!(be.read("z.txt").unwrap(), b"AAA");
        // 触っていない CP932 名は近代化後も同じ表示名で読める。
        assert_eq!(be.read("日本語.txt").unwrap(), b"orig");
    }

    #[test]
    fn archive_rename_dir_renames_subtree() {
        let dir = TempDir::new();
        let zip = dir.join("a.zip");
        build_stored_zip_raw(&zip, &[(b"sub/c.txt", b"C"), (b"e.txt", b"E")]);
        let host = FakeHost::new();
        let sum = run_archive_rename(&host, &zip, "", "sub", "box");
        assert_eq!(sum.err, 0);
        let be = crate::open_archive(&zip).unwrap();
        let paths: Vec<String> = be.list().unwrap().into_iter().map(|e| e.path).collect();
        assert!(!paths.iter().any(|p| p.starts_with("sub/")), "old subtree gone: {paths:?}");
        assert_eq!(be.read("box/c.txt").unwrap(), b"C");
        assert_eq!(be.read("e.txt").unwrap(), b"E");
    }

    #[test]
    fn archive_delete_cancel_keeps_original() {
        let dir = TempDir::new();
        let zip = dir.join("a.zip");
        build_stored_zip_raw(&zip, &[(b"a.txt", b"AAA"), (b"b.txt", b"BBB")]);
        let host = FakeHost::cancelling(0);
        let sum = run_archive_delete(&host, &zip, "", &["a.txt".to_owned()]);
        assert!(sum.cancelled);
        // 元書庫は無傷。
        let be = crate::open_archive(&zip).unwrap();
        assert_eq!(be.read("a.txt").unwrap(), b"AAA");
        assert_eq!(be.read("b.txt").unwrap(), b"BBB");
        assert!(!has_rewrite_tmp(&dir.path));
    }

    #[test]
    fn copy_dir_into_own_subtree_refused() {
        // dst が src の配下（自身の中）なら無限再帰になるので拒否する。
        let root = TempDir::new();
        std::fs::create_dir_all(root.join("d").join("inner")).unwrap();
        std::fs::write(root.join("d").join("inner").join("x.txt"), "x").unwrap();
        let dst_dir = root.join("d"); // d を d の中へコピー。
        let host = FakeHost::new();
        let sum = run_copy(&host, &root.path, &dst_dir, &["d".to_owned()], false);
        assert_eq!(sum.err, 1);
        assert_eq!(sum.ok, 0);
        assert!(host.lines().iter().any(|l| l.contains("自身の下へは")));
    }

    #[test]
    fn conflict_rename_to_existing_does_not_overwrite() {
        // 別名(Rename)先も既存なら、黙って上書きせず再確認（固定ホストでは最終的にスキップ）。
        let src = TempDir::new();
        let dst = TempDir::new();
        src.write_file("a.txt", "new");
        dst.write_file("a.txt", "old"); // 最初の衝突。
        dst.write_file("b.txt", "keep"); // Rename 先も既存。
        let host = FakeHost::with_conflict(ConflictResolution::Rename("b.txt".to_owned()));
        let sum = run_copy(&host, &src.path, &dst.path, &["a.txt".to_owned()], false);
        assert_eq!(sum.ok, 0);
        assert_eq!(sum.skip, 1);
        assert_eq!(std::fs::read_to_string(dst.join("b.txt")).unwrap(), "keep", "既存を潰さない");
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "old");
    }

    #[test]
    fn extract_rejects_drive_prefixed_name() {
        // "C:evil" のようなドライブ相対プレフィクスは弾く（push で base を捨てて外へ逃げる）。
        let dst = TempDir::new();
        let arc = MockArchive::new(&[("ok.txt", b"X")]);
        let entries = arc.entries.clone();
        let host = FakeHost::new();
        let sum = run_extract(&host, &arc, &entries, "", &["C:evil.txt".to_owned()], &dst.path);
        assert_eq!(sum.err, 1);
        assert_eq!(sum.ok, 0);
    }
