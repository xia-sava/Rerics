    use super::zip_be::ZipBackend;
    use super::sevenz::SevenZBackend;
    use super::rar::RarBackend;
    use super::*;
    use crate::Location;

    /// テスト用の一意な temp パス（同プロセス内の並行テストは tag で区別）。
    fn temp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rerics_arc_{}_{}.zip", std::process::id(), tag));
        p
    }

    /// deflate 圧縮で zip を生成する（ASCII 名・UTF-8 フラグ付き）。
    fn build_zip(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        let f = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        for (name, data) in entries {
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
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
            u16le(&mut out, 20); // version needed
            u16le(&mut out, 0); // flags（UTF-8 ビット無し）
            u16le(&mut out, 0); // method stored
            u16le(&mut out, 0); // time
            u16le(&mut out, 0); // date
            u32le(&mut out, crc);
            u32le(&mut out, data.len() as u32);
            u32le(&mut out, data.len() as u32);
            u16le(&mut out, name.len() as u16);
            u16le(&mut out, 0); // extra len
            out.extend_from_slice(name);
            out.extend_from_slice(data);
            // central directory header
            u32le(&mut central, 0x0201_4b50);
            u16le(&mut central, 20); // version made by
            u16le(&mut central, 20); // version needed
            u16le(&mut central, 0); // flags
            u16le(&mut central, 0); // method
            u16le(&mut central, 0); // time
            u16le(&mut central, 0); // date
            u32le(&mut central, crc);
            u32le(&mut central, data.len() as u32);
            u32le(&mut central, data.len() as u32);
            u16le(&mut central, name.len() as u16);
            u16le(&mut central, 0); // extra
            u16le(&mut central, 0); // comment
            u16le(&mut central, 0); // disk start
            u16le(&mut central, 0); // internal attrs
            u32le(&mut central, 0); // external attrs
            u32le(&mut central, off);
            central.extend_from_slice(name);
        }
        let cd_off = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        // end of central directory
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

    #[test]
    fn decode_name_utf8_and_sjis() {
        assert_eq!(decode_name("日本語".as_bytes()), "日本語");
        // CP932 の "日本語"（フラグ無しの旧 zip 相当）
        assert_eq!(decode_name(&[0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]), "日本語");
        assert_eq!(decode_name(b"ascii.txt"), "ascii.txt");
    }

    #[test]
    fn normalize_inner_strips() {
        assert_eq!(normalize_inner("a/b/"), "a/b");
        assert_eq!(normalize_inner("a\\b"), "a/b");
        assert_eq!(normalize_inner("/"), "");
    }

    #[test]
    fn list_and_read_deflate() {
        let path = temp_path("deflate");
        build_zip(
            &path,
            &[("a.txt", b"AAA"), ("b/c.txt", b"CCC"), ("b/d.txt", b"DDD")],
        );
        let be = ZipBackend::open(&path).unwrap();
        let list = be.list().unwrap();
        assert!(list
            .iter()
            .any(|e| e.path == "a.txt" && !e.is_dir && e.size == Some(3)));
        assert!(list.iter().any(|e| e.path == "b/c.txt"));
        assert_eq!(be.read("a.txt").unwrap(), b"AAA");
        assert_eq!(be.read("b/c.txt").unwrap(), b"CCC");
        assert!(be.read("missing").is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn encrypted_entry_reads_with_password() {
        use std::io::Write;
        let path = temp_path("aes");
        {
            let f = std::fs::File::create(&path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .with_aes_encryption(zip::AesMode::Aes256, "secret");
            zw.start_file("secret.txt", opts).unwrap();
            zw.write_all(b"classified").unwrap();
            zw.finish().unwrap();
        }
        let be = ZipBackend::open(&path).unwrap();
        // 暗号化フラグが立つ。
        assert!(be.list().unwrap().iter().any(|e| e.path == "secret.txt" && e.is_encrypted));
        // パスワード無しでは読めない。
        assert!(be.read("secret.txt").is_err());
        // 正しいパスワードで復号できる。
        assert_eq!(
            be.read_with_password("secret.txt", Some(b"secret")).unwrap(),
            b"classified"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_capped_truncates_and_passes_through() {
        let path = temp_path("capped");
        build_zip(&path, &[("big.txt", b"0123456789ABCDEF")]);
        let be = ZipBackend::open(&path).unwrap();
        let (head, truncated) = be.read_capped("big.txt", 4).unwrap();
        assert_eq!(head, b"0123");
        assert!(truncated);
        let (full, trunc2) = be.read_capped("big.txt", 100).unwrap();
        assert_eq!(full, b"0123456789ABCDEF");
        assert!(!trunc2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn entries_at_root_and_sub() {
        let path = temp_path("tree");
        build_zip(
            &path,
            &[("a.txt", b"A"), ("b/c.txt", b"C"), ("b/d.txt", b"D")],
        );
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        let root = entries_at(&all, "");
        // 暗黙ディレクトリ b を拾い、dir 優先で並ぶ
        assert!(root.iter().any(|i| i.name == "b" && i.is_dir));
        assert!(root.iter().any(|i| i.name == "a.txt" && !i.is_dir));
        let sub = entries_at(&all, "b");
        let names: Vec<_> = sub.iter().map(|i| i.name.clone()).collect();
        assert!(names.contains(&"c.txt".to_string()));
        assert!(names.contains(&"d.txt".to_string()));
        assert_eq!(sub.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cp932_name_end_to_end() {
        let mut name = vec![0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]; // 日本語
        name.extend_from_slice(b".txt");
        let path = temp_path("cp932");
        build_stored_zip_raw(&path, &[(&name, b"hello")]);
        let be = ZipBackend::open(&path).unwrap();
        let list = be.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, "日本語.txt");
        assert!(!list[0].is_dir);
        assert_eq!(be.read("日本語.txt").unwrap(), b"hello");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_zip_lists_nothing() {
        let path = temp_path("empty");
        build_zip(&path, &[]);
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        assert!(all.is_empty());
        assert!(entries_at(&all, "").is_empty());
        assert!(be.read("anything").is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deep_nest_folds_one_level_per_step() {
        let path = temp_path("deep");
        build_zip(&path, &[("a/b/c/d.txt", b"D")]);
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        assert!(entries_at(&all, "").iter().any(|i| i.name == "a" && i.is_dir));
        assert!(entries_at(&all, "a").iter().any(|i| i.name == "b" && i.is_dir));
        assert!(entries_at(&all, "a/b").iter().any(|i| i.name == "c" && i.is_dir));
        let leaf = entries_at(&all, "a/b/c");
        assert_eq!(leaf.len(), 1);
        assert!(leaf.iter().any(|i| i.name == "d.txt" && !i.is_dir));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn explicit_dir_entry_and_read_dir_errors() {
        use std::io::Write;
        let path = temp_path("dironly");
        {
            let f = std::fs::File::create(&path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            zw.add_directory("emptydir", opts).unwrap();
            zw.start_file("emptydir/inner.txt", opts).unwrap();
            zw.write_all(b"I").unwrap();
            zw.finish().unwrap();
        }
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        assert!(all.iter().any(|e| e.path == "emptydir" && e.is_dir));
        assert!(entries_at(&all, "").iter().any(|i| i.name == "emptydir" && i.is_dir));
        // ディレクトリを read するとエラー（ファイルではない）。
        assert!(be.read("emptydir").is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn leading_and_dot_segments_make_no_phantom_dir() {
        // 先頭スラッシュ/"." セグメント付きの生バイト名（一部ツールが生成する）。
        let path = temp_path("phantom");
        build_stored_zip_raw(&path, &[(b"/abs/file.txt", b"X"), (b"./root.txt", b"Y")]);
        let be = ZipBackend::open(&path).unwrap();
        let all = be.list().unwrap();
        assert!(all.iter().any(|e| e.path == "abs/file.txt"));
        assert!(all.iter().any(|e| e.path == "root.txt"));
        let root = entries_at(&all, "");
        // 空名の幽霊ディレクトリが現れない。
        assert!(root.iter().all(|i| !i.name.is_empty()));
        assert!(root.iter().any(|i| i.name == "abs" && i.is_dir));
        assert!(root.iter().any(|i| i.name == "root.txt" && !i.is_dir));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn location_parse_detects_archive_boundary() {
        let dir = std::env::temp_dir().join(format!("rerics_parse_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("p.zip");
        build_zip(&zip, &[("a.txt", b"A"), ("b/c.txt", b"C")]);

        // 実在ディレクトリ → Real
        assert!(!Location::parse(&dir.to_string_lossy()).is_archive());
        // 書庫ルート → Archive{inner=""}
        let a = Location::parse(&zip.to_string_lossy());
        assert!(matches!(&a, Location::Archive { inner, .. } if inner.is_empty()));
        // 書庫内 inner（OS セパレータ）→ Archive{inner="b"}
        let sub = zip.join("b");
        let s = Location::parse(&sub.to_string_lossy());
        assert!(matches!(&s, Location::Archive { inner, .. } if inner == "b"));
        // 存在しないパス → Real フォールバック
        assert!(!Location::parse("C:\\no\\such\\dir_xyz_zzz").is_archive());

        let _ = std::fs::remove_file(&zip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn location_enter_and_parent() {
        let dir = std::env::temp_dir().join(format!("rerics_nav_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("test.zip");
        build_zip(&zip, &[("a.txt", b"A"), ("b/c.txt", b"C")]);

        let root = Location::Real(dir.clone());
        // 書庫ファイルへ潜る（is_dir=false）
        let inzip = root.enter("test.zip", false).unwrap();
        assert!(inzip.is_archive());
        let items = inzip.read().unwrap();
        assert!(items.iter().any(|i| i.is_parent));
        assert!(items.iter().any(|i| i.name == "b" && i.is_dir));
        assert!(items.iter().any(|i| i.name == "a.txt"));

        // 書庫内 dir へ潜る
        let inb = inzip.enter("b", true).unwrap();
        assert!(inb.read().unwrap().iter().any(|i| i.name == "c.txt"));

        // b の親＝書庫ルート、出てきた名前は "b"
        let (par, prev) = inb.to_parent().unwrap();
        assert_eq!(prev, "b");
        assert!(matches!(&par, Location::Archive { inner, .. } if inner.is_empty()));

        // 書庫ルートの親＝実 dir、出てきた名前は書庫ファイル名
        let (par2, prev2) = par.to_parent().unwrap();
        assert_eq!(prev2, "test.zip");
        assert_eq!(par2.as_real_path(), Some(dir.as_path()));

        let _ = std::fs::remove_file(&zip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// rar 読取。同梱フィクスチャ version.rar を一覧・読取・一括展開する。
    #[test]
    fn rar_list_and_read() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/version.rar");
        let be = RarBackend::open(&p).unwrap();
        assert!(!be.caps().random_access);
        let list = be.list().unwrap();
        assert!(list.iter().any(|e| e.path == "VERSION" && !e.is_dir));
        assert_eq!(be.read("VERSION").unwrap(), b"unrar-0.4.0");
        assert!(be.read("nope").is_err());

        // 単一パス展開（extract_all override）でファイルが書き出される。
        let dir = temp_path("rar_extract");
        let _ = std::fs::remove_dir_all(&dir);
        let n = extract_all_to(&be, &dir).unwrap();
        assert_eq!(n, 1, "1 ファイル展開");
        assert_eq!(std::fs::read(dir.join("VERSION")).unwrap(), b"unrar-0.4.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// ソリッド／非ソリッドで `caps().random_access` が反転すること。
    #[test]
    fn sevenz_solid_flips_random_access() {
        assert!(!SevenZBackend::open(&fixture("solid.7z"))
            .unwrap()
            .caps()
            .random_access);
        assert!(SevenZBackend::open(&fixture("nonsolid.7z"))
            .unwrap()
            .caps()
            .random_access);
    }

    /// 7z の一覧・個別読取（'\\' 区切りの格納名を '/' へ正規化して扱う）。
    #[test]
    fn sevenz_list_and_read() {
        for name in ["solid.7z", "nonsolid.7z"] {
            let be = SevenZBackend::open(&fixture(name)).unwrap();
            let list = be.list().unwrap();
            assert!(
                list.iter().any(|e| e.path == "a.txt" && !e.is_dir && e.size == Some(3)),
                "{name}: a.txt"
            );
            assert!(list.iter().any(|e| e.path == "sub" && e.is_dir), "{name}: sub dir");
            assert!(list.iter().any(|e| e.path == "sub/c.txt"), "{name}: sub/c.txt");
            assert_eq!(be.read("a.txt").unwrap(), b"AAA", "{name}");
            assert_eq!(be.read("sub/c.txt").unwrap(), b"CCC", "{name}");
            assert_eq!(be.read("sub/d.txt").unwrap(), b"DDD", "{name}");
            assert!(be.read("missing").is_err(), "{name}");
        }
    }

    /// `extract_all` がツリーを実FSへ展開し、各ファイルでコールバックが進捗を刻むこと。
    #[test]
    fn sevenz_extract_all_writes_tree() {
        let be = SevenZBackend::open(&fixture("solid.7z")).unwrap();
        let dest = std::env::temp_dir()
            .join(format!("rerics_7z_extract_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        let mut seen = 0u64;
        be.extract_all(&dest, &mut |_name, done, total| {
            assert_eq!(total, 3);
            seen = done + 1;
            true
        })
        .unwrap();
        assert_eq!(seen, 3);
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"AAA");
        assert_eq!(std::fs::read(dest.join("sub").join("c.txt")).unwrap(), b"CCC");
        assert_eq!(std::fs::read(dest.join("sub").join("d.txt")).unwrap(), b"DDD");
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// `extract_all` のコールバックが `false` を返すと途中で止まること。
    #[test]
    fn sevenz_extract_all_cancels() {
        let be = SevenZBackend::open(&fixture("nonsolid.7z")).unwrap();
        let dest = std::env::temp_dir()
            .join(format!("rerics_7z_cancel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        be.extract_all(&dest, &mut |_name, _done, _total| false).unwrap();
        // 1件も展開していない（最初の each で false）。
        assert!(!dest.join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// tar 本体＋各圧縮ラップ（gz/bz2/xz/zstd）の一覧・読取・一括展開。すべて非RA。
    #[test]
    fn tar_family_list_read_extract() {
        for name in ["tree.tar", "tree.tar.gz", "tree.tar.bz2", "tree.tar.xz", "tree.tar.zst"] {
            let be = open_archive(&fixture(name)).unwrap();
            assert!(!be.caps().random_access, "{name}: tar は非RA");
            let list = be.list().unwrap();
            assert!(
                list.iter().any(|e| e.path == "a.txt" && !e.is_dir && e.size == Some(3)),
                "{name}: a.txt"
            );
            assert!(list.iter().any(|e| e.path == "sub/c.txt"), "{name}: sub/c.txt");
            assert_eq!(be.read("a.txt").unwrap(), b"AAA", "{name}");
            assert_eq!(be.read("sub/d.txt").unwrap(), b"DDD", "{name}");
            let dest = std::env::temp_dir().join(format!(
                "rerics_tar_{}_{}",
                std::process::id(),
                name.replace('.', "_")
            ));
            let _ = std::fs::remove_dir_all(&dest);
            std::fs::create_dir_all(&dest).unwrap();
            let mut called = false;
            be.extract_all(&dest, &mut |_p, _done, total| {
                // tar の進捗は「消費バイト数／圧縮ファイルサイズ」（total>0）。
                assert!(total > 0, "{name}: total はファイルサイズ");
                called = true;
                true
            })
            .unwrap();
            assert!(called, "{name}: コールバックが呼ばれる");
            assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"AAA", "{name}");
            assert_eq!(std::fs::read(dest.join("sub").join("c.txt")).unwrap(), b"CCC", "{name}");
            let _ = std::fs::remove_dir_all(&dest);
        }
    }

    /// 単体圧縮（gz/xz）＝1エントリ（圧縮拡張子を除いた名前）・ランダムアクセス可・読む時に解凍。
    #[test]
    fn single_file_compressed_one_entry() {
        for name in ["note.txt.xz", "note.txt.gz", "note.txt.zst"] {
            let be = open_archive(&fixture(name)).unwrap();
            assert!(be.caps().random_access, "{name}: 単体は RA");
            let list = be.list().unwrap();
            assert_eq!(list.len(), 1, "{name}");
            assert_eq!(list[0].path, "note.txt", "{name}");
            assert!(!list[0].is_dir);
            // gz/xz/zstd は展開後サイズ（"hello world"=11）をメタ/ヘッダから取れる。
            assert_eq!(list[0].size, Some(11), "{name}: 展開後サイズ");
            assert_eq!(be.read("note.txt").unwrap(), b"hello world", "{name}");
            assert!(be.read("nope").is_err(), "{name}");
            let (head, trunc) = be.read_capped("note.txt", 5).unwrap();
            assert_eq!(head, b"hello", "{name}");
            assert!(trunc, "{name}");
        }
    }

    /// zip への append：**既存の CP932 名エントリを壊さず**新規ファイル/ディレクトリを足せる。
    #[test]
    fn zip_append_preserves_cp932_names() {
        let mut cp932 = vec![0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]; // 日本語
        cp932.extend_from_slice(b".txt");
        let path = temp_path("append_cp932");
        build_stored_zip_raw(&path, &[(&cp932, b"orig")]);

        // 追加（add）と mkdir を append で実行。
        let mut w = open_archive_writer(&path).unwrap();
        w.add("added.txt", b"NEW").unwrap();
        w.mkdir("newdir").unwrap();
        // 未対応操作はエラー。
        assert!(w.remove("added.txt").is_err());
        assert!(w.rename("added.txt", "x").is_err());

        let be = ZipBackend::open(&path).unwrap();
        let list = be.list().unwrap();
        // 既存の CP932 名が壊れていない（正しくデコードできる）。
        assert!(
            list.iter().any(|e| e.path == "日本語.txt"),
            "CP932 名が保持される: {:?}",
            list.iter().map(|e| &e.path).collect::<Vec<_>>()
        );
        // 既存データも無傷、新規も読める。
        assert_eq!(be.read("日本語.txt").unwrap(), b"orig");
        assert_eq!(be.read("added.txt").unwrap(), b"NEW");
        assert!(list.iter().any(|e| e.path == "newdir" && e.is_dir));
        let _ = std::fs::remove_file(&path);
    }

    /// 書込み未対応形式（7z 等）は `open_archive_writer` がエラー。
    #[test]
    fn writer_unsupported_for_non_zip() {
        assert!(open_archive_writer(&fixture("solid.7z")).is_err());
        assert!(open_archive_writer(&fixture("tree.tar")).is_err());
    }

    /// 拡張子分類（二重拡張子・短縮形・単体・非書庫）。
    #[test]
    fn classify_known_extensions() {
        for p in ["x.tar", "x.tar.gz", "x.tgz", "x.tar.zstd", "x.json.xz", "a.zip", "a.7z"] {
            assert!(is_known_archive(Path::new(p)), "{p} は書庫のはず");
        }
        for p in ["a.txt", "a.png", "noext"] {
            assert!(!is_known_archive(Path::new(p)), "{p} は非書庫のはず");
        }
    }
