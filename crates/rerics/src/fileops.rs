use std::path::{Path, PathBuf};
use std::sync::Arc;
use winsafe::{self as w, co, gui, prelude::*};
use rerics_core::{Location, LogLevel, messages};
use crate::task::{ChannelHost, OpKind, TaskControl, WorkerEvent};
use crate::{MainWindow, dialog, shell, short_desc};

/// パスの末尾要素（ファイル名）を `String` で返す（取れなければ空文字）。
fn file_name_of(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// 圧縮で作る書庫形式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CompressKind {
    Zip,
    SevenZ,
    /// xz 単体圧縮（対象1ファイル・tar なし）。
    Xz,
    TarXz,
}

impl CompressKind {
    /// 形式トークンを解釈する（大小無視）。未知は `None`。
    fn from_token(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "zip" => Some(CompressKind::Zip),
            "7z" => Some(CompressKind::SevenZ),
            "xz" => Some(CompressKind::Xz),
            "tar.xz" | "tarxz" | "txz" => Some(CompressKind::TarXz),
            _ => None,
        }
    }
}

/// 最終的な出力名 `name`・選択形式 `seed`・束ね要否 `bundling`（複数 or ディレクトリ）から、
/// 実際に作る形式と出力名を決める。名前に既知の書庫拡張子があればそれが優先（＝表示と実体が
/// 一致）。単体 xz を選んでも束ねが要るなら tar.xz へ格上げする。既知拡張子が無ければ `seed`
/// の拡張子を補う。
pub(crate) fn resolve_compress(
    name: &str,
    seed: dialog::CompressFormat,
    bundling: bool,
) -> (CompressKind, String) {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.xz") || lower.ends_with(".txz") {
        return (CompressKind::TarXz, name.to_string());
    }
    if lower.ends_with(".7z") {
        return (CompressKind::SevenZ, name.to_string());
    }
    if lower.ends_with(".zip") {
        return (CompressKind::Zip, name.to_string());
    }
    if lower.ends_with(".xz") {
        if bundling {
            let base = &name[..name.len() - ".xz".len()];
            return (CompressKind::TarXz, format!("{base}.tar.xz"));
        }
        return (CompressKind::Xz, name.to_string());
    }
    match seed {
        dialog::CompressFormat::Zip => (CompressKind::Zip, format!("{name}.zip")),
        dialog::CompressFormat::SevenZ => (CompressKind::SevenZ, format!("{name}.7z")),
        dialog::CompressFormat::Xz if bundling => (CompressKind::TarXz, format!("{name}.tar.xz")),
        dialog::CompressFormat::Xz => (CompressKind::Xz, format!("{name}.xz")),
    }
}

/// 個別圧縮での 1 項目の形式と出力名。xz はディレクトリ項目なら tar.xz、通常ファイルなら
/// 単体 xz。
fn per_item_compress(
    name: &str,
    format: dialog::CompressFormat,
    is_dir: bool,
) -> (CompressKind, String) {
    match format {
        dialog::CompressFormat::Zip => (CompressKind::Zip, format!("{name}.zip")),
        dialog::CompressFormat::SevenZ => (CompressKind::SevenZ, format!("{name}.7z")),
        dialog::CompressFormat::Xz if is_dir => (CompressKind::TarXz, format!("{name}.tar.xz")),
        dialog::CompressFormat::Xz => (CompressKind::Xz, format!("{name}.xz")),
    }
}

impl MainWindow {
    /// 入力ダイアログで名前を尋ね、アクティブペインの現在パス直下にディレクトリを作る。
    /// 作成後は一覧を更新し、新ディレクトリへカーソルを移す。
    pub(crate) fn make_directory(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            return self.make_directory_in_archive(is_left);
        }
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
        // 実処理は no-UI 版の正本 script_create_directory へ委譲する（作成・ログ・一覧更新は
        // そちらが行う）。失敗時はログに加えてダイアログでも報せる。
        match self.script_create_directory(name) {
            Ok(dir) => {
                // 設定が有効なら、作成した新ディレクトリへ入る（原作 CreateDirectoryAndMove 相当）。
                if self.config.borrow().cursor.create_directory_and_move {
                    self.remember_cursor_for_nav(is_left);
                    if self.pane(is_left).borrow_mut().navigate(Location::parse(&dir)) {
                        self.reload_side_navigated(is_left)?;
                    }
                }
            }
            Err(line) => {
                dialog::message_box(&self.wnd, "ディレクトリの作成", &line, dialog::MessageStyle::Error);
            }
        }
        Ok(())
    }

    /// スクリプト用：名前（相対はアクティブペインの現在地基準）でディレクトリを作り、作成した
    /// 絶対パスを返す。作成後は一覧を更新して新ディレクトリへカーソルを移す。失敗は `Err`。
    pub(crate) fn script_create_directory(&self, name: &str) -> Result<String, String> {
        let is_left = !self.active_right.get();
        if self.pane(is_left).borrow().is_archive() {
            return Err("書庫内ではディレクトリを作成できません".to_string());
        }
        let name = name.trim();
        if name.is_empty() {
            return Err("ディレクトリ名が空です".to_string());
        }
        let p = Path::new(name);
        let dir = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.pane(is_left).borrow().path().join(name)
        };
        if let Err(e) = std::fs::create_dir(&dir) {
            let line = messages::create_directory_failure(name, &e.to_string());
            self.log.error(&line);
            return Err(line);
        }
        self.log.normal(&messages::create_directory(name));
        let focus = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let _ = self.reload_side_focus(is_left, &focus, false);
        Ok(dir.display().to_string())
    }

    /// 新規ファイルを作る。入力されたファイル名で空ファイルを作成する。
    /// 既存ファイルは上書きしない。
    pub(crate) fn create_file(&self, is_left: bool) -> w::AnyResult<()> {
        if self.block_if_archive(is_left, "ファイルの作成") {
            return Ok(());
        }
        let name = self.input_with_history(
            "新規ファイルの作成",
            "ファイル名を入力して下さい。",
            "",
            "createfile",
        );
        let Some(name) = name else {
            return Ok(());
        };
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let path = self.pane(is_left).borrow().path().join(name);
        if path.exists() {
            let msg = messages::all_ready_exists(name);
            dialog::message_box(&self.wnd, "新規ファイルの作成", &msg, dialog::MessageStyle::Error);
            return Ok(());
        }
        let made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(|_| ());
        if let Err(e) = made {
            let msg = if e.kind() == std::io::ErrorKind::AlreadyExists {
                messages::all_ready_exists(name)
            } else {
                format!("{e}")
            };
            dialog::message_box(&self.wnd, "新規ファイルの作成", &msg, dialog::MessageStyle::Error);
            return Ok(());
        }
        self.reload_side_focus(is_left, name, false)?;
        Ok(())
    }

    /// アクティブペインの選択項目を新しい書庫（zip / 7z / xz / tar.xz）へ圧縮する（実FS のみ）。
    /// 出力名と形式を尋ね、反対ペイン（実FS）の直下に作る（原作準拠・反対が書庫/仮想なら
    /// アクティブ側へフォールバック）。既存名は上書き確認する。実際の形式は最終的な名前の
    /// 拡張子で決まる（[`resolve_compress`]）。
    pub(crate) fn compress(&self, is_left: bool) -> w::AnyResult<()> {
        if self.block_if_archive(is_left, "圧縮") {
            return Ok(());
        }
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        // 作成先は反対ペイン（実FS）＝原作準拠。反対が書庫/仮想ならアクティブ側にフォールバック。
        let dst_dir = self
            .pane(!is_left)
            .borrow()
            .as_real_path()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| dir.clone());
        // 単一の通常ファイルだけが tar なしの単体圧縮になり得る。複数・ディレクトリは束ねが要る。
        let bundling = names.len() > 1 || dir.join(&names[0]).is_dir();
        // 既定名：単一選択ならその名、複数なら親ディレクトリ名を base に。xz は束ね要否で拡張子が変わる。
        let base = if names.len() == 1 {
            names[0].clone()
        } else {
            dir.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "archive".to_owned())
        };
        let defaults = dialog::CompressDefaults {
            zip: format!("{base}.zip"),
            sevenz: format!("{base}.7z"),
            xz: if bundling { format!("{base}.tar.xz") } else { format!("{base}.xz") },
        };
        // 履歴付きの圧縮ダイアログ（書庫名＋形式＋個別圧縮の選択）。
        let mut hist = rerics_core::InputHistory::load();
        let items = hist.get("compress");
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let Some(choice) = dialog::compress_box(&self.wnd, &defaults, &refs) else {
            return Ok(());
        };

        if choice.one_by_one {
            // 各項目を形式ごとの拡張子へ個別圧縮する（書庫名欄は使わない）。
            return self.start_compress_each(dir, names, choice.format, dst_dir);
        }

        let name = choice.name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let (kind, out_name) = resolve_compress(name, choice.format, bundling);
        hist.add("compress", &out_name);
        let _ = hist.save();
        let dst = dst_dir.join(&out_name);
        if dst.exists() {
            let r = dialog::message_box(
                &self.wnd,
                "圧縮",
                &messages::all_ready_exists(&out_name),
                dialog::MessageStyle::YesNo,
            );
            if r != dialog::MessageResult::Yes {
                return Ok(());
            }
        }
        // ワーカー起動はスクリプト版と共有の start_compress。起動失敗時はログとダイアログで報せる
        // （圧縮そのものの失敗は非同期でワーカーがログする）。
        if let Err(e) = self.start_compress(dir, names, dst, kind) {
            let line = e.to_string();
            self.log.error(&line);
            dialog::message_box(&self.wnd, "圧縮", &line, dialog::MessageStyle::Error);
        }
        Ok(())
    }

    /// スクリプト用：対象名の列 `files`（相対は現在地基準）を `archive` へ圧縮するワーカーを
    /// 起動する（投げっぱなし）。`kind` は `zip`/`7z`/`xz`/`tar.xz`（`xz` は単体圧縮＝対象1
    /// ファイルのみ）。起動前の検証失敗は `Err`。表層 UI の `compress` ダイアログもこの実処理を
    /// 共有して呼ぶ。
    pub(crate) fn script_compress(
        &self,
        kind: &str,
        archive: &str,
        files: &[String],
    ) -> Result<(), String> {
        let is_left = !self.active_right.get();
        if self.pane(is_left).borrow().is_archive() {
            return Err("書庫内では圧縮できません".to_string());
        }
        let ck = CompressKind::from_token(kind)
            .ok_or_else(|| format!("未対応の圧縮形式です: {kind}（zip / 7z / xz / tar.xz）"))?;
        let names: Vec<String> =
            files.iter().map(|f| f.trim().to_owned()).filter(|f| !f.is_empty()).collect();
        if names.is_empty() {
            return Err("圧縮対象がありません".to_string());
        }
        if ck == CompressKind::Xz && names.len() != 1 {
            return Err("xz 単体圧縮は対象1ファイルのみです（複数は tar.xz）".to_string());
        }
        let archive = archive.trim();
        if archive.is_empty() {
            return Err("出力する書庫名が空です".to_string());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        let ap = Path::new(archive);
        let dst = if ap.is_absolute() { ap.to_path_buf() } else { dir.join(archive) };
        self.start_compress(dir, names, dst, ck).map_err(|e| e.to_string())
    }

    /// メニュー「解凍」からの取り出し。アクティブが書庫なら反対の実ペインへ展開する。
    pub(crate) fn extract_menu(&self, is_left: bool) -> w::AnyResult<()> {
        if !self.pane(is_left).borrow().is_archive() {
            self.log.warn("カレントが書庫ではありません");
            return Ok(());
        }
        self.extract_from_archive(is_left)
    }

    /// 圧縮作成をワーカースレッドで起動する。`dir` は対象を読む元（アクティブペイン）、`dst`
    /// は出力先のフルパス（反対ペイン等）。`kind` で形式を選び分ける。完了で出力先ディレクトリ
    /// （`dst` の親）を再読込し、元ペインの選択を解除する。
    pub(crate) fn start_compress(
        &self,
        dir: PathBuf,
        names: Vec<String>,
        dst: PathBuf,
        kind: CompressKind,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let desc = format!("{} -> {}", short_desc(&names), dst.display());
        self.register_task(id, "圧縮", desc, control)?;
        let src_dir = dir.clone();
        let reload_dir = dst.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| dir.clone());
        std::thread::spawn(move || {
            let sum = match kind {
                CompressKind::Zip => rerics_core::run_compress(&host, &src_dir, &names, &dst),
                CompressKind::SevenZ => rerics_core::run_compress_7z(&host, &src_dir, &names, &dst),
                CompressKind::TarXz => rerics_core::run_compress_tar_xz(&host, &src_dir, &names, &dst),
                CompressKind::Xz => {
                    rerics_core::run_compress_xz_single(&host, &src_dir, &names[0], &dst)
                }
            };
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind: OpKind::Copy,
                src_dir,
                dst_dir: reload_dir,
                cancelled: sum.cancelled,
                failed: sum.err > 0,
            });
        });
        Ok(())
    }

    /// 選択項目をそれぞれ形式ごとの拡張子へ個別圧縮する（OneByOne）。`dir` は対象を読む元、
    /// `dst_dir` は出力先ディレクトリ（反対ペイン等）。同名が既にあれば上書きせずスキップする。
    /// 完了で出力先を再読込し、元ペインの選択を解除する。
    pub(crate) fn start_compress_each(
        &self,
        dir: PathBuf,
        names: Vec<String>,
        format: dialog::CompressFormat,
        dst_dir: PathBuf,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let desc = format!("{} (個別)", short_desc(&names));
        self.register_task(id, "圧縮", desc, control)?;
        let src_dir = dir.clone();
        std::thread::spawn(move || {
            let mut cancelled = false;
            let mut failed = false;
            for name in &names {
                let is_dir = src_dir.join(name).is_dir();
                let (kind, out) = per_item_compress(name, format, is_dir);
                let dst = dst_dir.join(&out);
                if dst.exists() {
                    let _ = host.tx.send(WorkerEvent::Log {
                        level: LogLevel::Warning,
                        text: messages::all_ready_exists(&out),
                    });
                    continue;
                }
                let sum = match kind {
                    CompressKind::Zip => {
                        rerics_core::run_compress(&host, &src_dir, std::slice::from_ref(name), &dst)
                    }
                    CompressKind::SevenZ => {
                        rerics_core::run_compress_7z(&host, &src_dir, std::slice::from_ref(name), &dst)
                    }
                    CompressKind::TarXz => rerics_core::run_compress_tar_xz(
                        &host,
                        &src_dir,
                        std::slice::from_ref(name),
                        &dst,
                    ),
                    CompressKind::Xz => {
                        rerics_core::run_compress_xz_single(&host, &src_dir, name, &dst)
                    }
                };
                failed |= sum.err > 0;
                if sum.cancelled {
                    cancelled = true;
                    break;
                }
            }
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind: OpKind::Copy,
                src_dir,
                dst_dir,
                cancelled,
                failed,
            });
        });
        Ok(())
    }

    /// アクティブペインの選択（無ければカーソル）を反対側ペインへコピー/移動する。
    pub(crate) fn copy_or_move(&self, is_left: bool, move_it: bool) -> w::AnyResult<()> {
        let src_is_archive = self.pane(is_left).borrow().is_archive();
        let dst_is_archive = self.pane(!is_left).borrow().is_archive();

        // src が書庫＝取り出し（展開コピー）。移動は元（書庫）を消せないので未対応でスルー。
        if src_is_archive {
            if move_it {
                self.log.warn("書庫からの移動は未対応です");
                return Ok(());
            }
            if dst_is_archive {
                self.log.warn("書庫から書庫への取り出しは未対応です");
                return Ok(());
            }
            return self.extract_from_archive(is_left);
        }
        // dst が書庫＝実FS から書庫への追加（コピー）／移動（追加後に元を削除）。
        if dst_is_archive {
            return self.add_to_archive(is_left, move_it);
        }

        // 結果一覧では項目ごとに出自が異なるので、出自別にまとめて1タスクで処理する。
        let groups = if self.view(is_left).state().borrow().find_result {
            Some(self.selected_result_groups(is_left))
        } else {
            None
        };
        let names: Vec<String> = match &groups {
            Some(g) => g.iter().flat_map(|(_, n)| n.iter().cloned()).collect(),
            None => self.selected_or_cursor_names(is_left),
        };
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        // 設定で有効なら、コピー/移動の前に確認する（既定はどちらもオフ）。
        let ask = {
            let f = self.config.borrow().file_ops;
            if move_it { f.ask_before_move } else { f.ask_before_copy }
        };
        if ask {
            let short = short_desc(&names);
            let (title, question) = if move_it {
                ("移動", messages::move_question(&short))
            } else {
                ("コピー", messages::copy_question(&short))
            };
            let ans = dialog::message_box(&self.wnd, title, &question, dialog::MessageStyle::YesNo);
            if ans != dialog::MessageResult::Yes {
                return Ok(());
            }
        }
        let dst_dir = self.pane(!is_left).borrow().path().to_path_buf();
        match groups {
            Some(groups) => {
                let base_src = self.pane(is_left).borrow().path().to_path_buf();
                self.start_copy_grouped(base_src, groups, dst_dir, move_it)?;
            }
            None => {
                let src_dir = self.pane(is_left).borrow().path().to_path_buf();
                self.start_copy(src_dir, dst_dir, names, move_it)?;
            }
        }
        Ok(())
    }

    /// 選択中（無ければカーソル位置）の項目名を集める。`..` は除外する。
    pub(crate) fn selected_or_cursor_names(&self, is_left: bool) -> Vec<String> {
        let state = self.view(is_left).state();
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
    }

    /// 選択（無ければカーソル）項目を `(出自Location, 名前)` の組で返す。`..` は除外。通常モードは
    /// 全項目がペイン現在地、検索・比較の結果一覧は各項目の出自（`source`）。すべてのファイル操作が
    /// 「現在地基準の名前」ではなくこの単一の入口で対象を解決すれば、結果一覧でも自然に動く。
    pub(crate) fn selected_targets(&self, is_left: bool) -> Vec<(Location, String)> {
        let base = self.pane(is_left).borrow().loc().clone();
        let state = self.view(is_left).state();
        let s = state.borrow();
        let any_selected = s.items.iter().any(|it| it.selected && !it.is_parent);
        s.items
            .iter()
            .enumerate()
            .filter(|(i, it)| {
                !it.is_parent && if any_selected { it.selected } else { *i == s.cursor }
            })
            .map(|(_, it)| (it.source.clone().unwrap_or_else(|| base.clone()), it.name.clone()))
            .collect()
    }

    /// [`selected_targets`](Self::selected_targets) のうち実FS出自のものを `(実パス, 名前)` で返す。
    /// 書庫内出自は除外する。ゴミ箱送り・クリップボード・ショートカット等、実パスを要する操作で使う。
    pub(crate) fn selected_real_targets(&self, is_left: bool) -> Vec<(PathBuf, String)> {
        self.selected_targets(is_left)
            .into_iter()
            .filter_map(|(loc, name)| loc.as_real_path().map(|d| (d.join(&name), name)))
            .collect()
    }

    /// 結果一覧のコピー/移動/削除用に、選択（無ければカーソル）項目を出自ディレクトリ別にまとめる。
    /// `(出自ディレクトリ, 名前リスト)` の組を出現順で返す（実FS出自のみ）。
    pub(crate) fn selected_result_groups(&self, is_left: bool) -> Vec<(PathBuf, Vec<String>)> {
        let mut groups: Vec<(PathBuf, Vec<String>)> = Vec::new();
        for (loc, name) in self.selected_targets(is_left) {
            let Location::Real(dir) = loc else {
                continue;
            };
            match groups.iter_mut().find(|(d, _)| *d == dir) {
                Some((_, names)) => names.push(name),
                None => groups.push((dir, vec![name])),
            }
        }
        groups
    }

    /// 書庫からの取り出しをワーカースレッドで起動する。ワーカ内で書庫を開いて
    /// `run_extract` を回し、完了で dst ペインを再読込させる。
    pub(crate) fn start_extract(
        &self,
        archive: PathBuf,
        inner: String,
        names: Vec<String>,
        dst_dir: PathBuf,
        reload_dir: PathBuf,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let desc = format!("{} -> {}", short_desc(&names), dst_dir.display());
        self.register_task(id, "取り出し", desc, control)?;
        // 取り出しは dst_dir で行い、完了後は表示中のペイン（reload_dir）を再読込する
        // （書庫名フォルダを作る設定では dst_dir はその下層になり表示ペインと一致しないため）。
        let dst_done = reload_dir;
        std::thread::spawn(move || {
            let (cancelled, failed) = match rerics_core::open_archive(&archive) {
                Ok(backend) => match backend.list() {
                    Ok(entries) => {
                        let sum = rerics_core::run_extract(
                            &host,
                            backend.as_ref(),
                            &entries,
                            &inner,
                            &names,
                            &dst_dir,
                        );
                        (sum.cancelled, sum.err > 0)
                    }
                    Err(e) => {
                        let _ = host.tx.send(WorkerEvent::Log {
                            level: LogLevel::Error,
                            text: format!("書庫の読取に失敗しました: {}", e),
                        });
                        (false, true)
                    }
                },
                Err(e) => {
                    let _ = host.tx.send(WorkerEvent::Log {
                        level: LogLevel::Error,
                        text: format!("書庫を開けません: {}", e),
                    });
                    (false, true)
                }
            };
            // src は書庫（実パス無し＝空）として渡す。dst（実FS）が再読込される。
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind: OpKind::Copy,
                src_dir: PathBuf::new(),
                dst_dir: dst_done,
                cancelled,
                failed,
            });
        });
        Ok(())
    }

    /// コピー/移動をワーカースレッドで起動する。完了は `wm_timer` 経由で取り込む。
    /// コピー/移動をワーカースレッドで起動し、払い出したタスク `id` を返す。`id` は完了
    /// （`WorkerEvent::Done`）の突合に使える（スクリプトの async 操作が完了を待つのに利用）。
    pub(crate) fn start_copy(
        &self,
        src_dir: PathBuf,
        dst_dir: PathBuf,
        names: Vec<String>,
        move_it: bool,
    ) -> w::AnyResult<u64> {
        let control = Arc::new(TaskControl::new());
        let copy_opts = {
            let f = self.config.borrow().file_ops;
            rerics_core::CopyOptions {
                copy_attribute: f.copy_attribute,
                copy_date: f.copy_date,
            }
        };
        let id = self.next_id();
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        )
        .with_copy_options(copy_opts)
        .with_task_id(id);
        let kind = if move_it { OpKind::Move } else { OpKind::Copy };
        let text = if move_it { "移動" } else { "コピー" };
        let desc = format!("{} -> {}", short_desc(&names), dst_dir.display());
        self.register_task(id, text, desc, control)?;
        std::thread::spawn(move || {
            let sum = rerics_core::run_copy(&host, &src_dir, &dst_dir, &names, move_it);
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind,
                src_dir,
                dst_dir,
                cancelled: sum.cancelled,
                failed: sum.err > 0,
            });
        });
        Ok(id)
    }

    /// 検索・比較の結果一覧から、出自ディレクトリ別にまとめた項目を反対側へコピー/移動する。
    /// 1タスクで各グループを順に処理し、完了は基準ペイン（結果一覧）の場所を `src_dir` として
    /// 通知する＝[`on_op_done`](Self::on_op_done) がコピーなら選択解除のみ・移動なら基準へ復帰。
    pub(crate) fn start_copy_grouped(
        &self,
        base_src: PathBuf,
        groups: Vec<(PathBuf, Vec<String>)>,
        dst_dir: PathBuf,
        move_it: bool,
    ) -> w::AnyResult<u64> {
        let control = Arc::new(TaskControl::new());
        let copy_opts = {
            let f = self.config.borrow().file_ops;
            rerics_core::CopyOptions {
                copy_attribute: f.copy_attribute,
                copy_date: f.copy_date,
            }
        };
        let id = self.next_id();
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        )
        .with_copy_options(copy_opts)
        .with_task_id(id);
        let kind = if move_it { OpKind::Move } else { OpKind::Copy };
        let text = if move_it { "移動" } else { "コピー" };
        let all_names: Vec<String> = groups.iter().flat_map(|(_, n)| n.iter().cloned()).collect();
        let desc = format!("{} -> {}", short_desc(&all_names), dst_dir.display());
        self.register_task(id, text, desc, control)?;
        std::thread::spawn(move || {
            let mut cancelled = false;
            let mut failed = false;
            for (src_dir, names) in &groups {
                let sum = rerics_core::run_copy(&host, src_dir, &dst_dir, names, move_it);
                failed |= sum.err > 0;
                if sum.cancelled {
                    cancelled = true;
                    break;
                }
            }
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind,
                src_dir: base_src,
                dst_dir,
                cancelled,
                failed,
            });
        });
        Ok(id)
    }

    /// カーソル位置の項目を入力ダイアログでリネームする。完了後は新名へカーソルを移す。
    pub(crate) fn rename(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            return self.rename_in_archive(is_left);
        }
        // 対象＝選択（無ければカーソル）。1件なら名前編集つき単一、複数なら属性/日時/名前変換の一括。
        // 各対象は出自ディレクトリを併せ持つ（結果一覧では項目ごとに出自が異なる）＝`(出自dir, 名前, dirか)`。
        let targets: Vec<(PathBuf, String, bool)> = self
            .selected_targets(is_left)
            .into_iter()
            .filter_map(|(loc, name)| {
                let is_dir = self
                    .view(is_left)
                    .state()
                    .borrow()
                    .items
                    .iter()
                    .find(|it| it.name == name)
                    .map(|it| it.is_dir)
                    .unwrap_or(false);
                loc.as_real_path().map(|d| (d.to_path_buf(), name, is_dir))
            })
            .collect();
        if targets.is_empty() {
            return Ok(());
        }

        let (single, single_is_dir, attrs, modified, created) = if targets.len() == 1 {
            let (dir0, name, is_dir) = &targets[0];
            let p = dir0.join(name);
            (
                Some(name.clone()),
                *is_dir,
                rerics_core::read_attrs(&p).unwrap_or_default(),
                rerics_core::modified_time(&p),
                rerics_core::created_time(&p),
            )
        } else {
            (None, false, rerics_core::FileAttrs::default(), None, None)
        };

        let Some(res) = dialog::rename_box(
            &self.wnd,
            single.as_deref(),
            single_is_dir,
            targets.len(),
            attrs,
            modified,
            created,
        ) else {
            return Ok(());
        };

        // 名前変更を先に処理し、以降の属性/日時は新パスへ適用する。各対象は自分の出自ディレクトリで改名する。
        let mut paths: Vec<std::path::PathBuf> = targets.iter().map(|(d, n, _)| d.join(n)).collect();
        let mut cursor_name = single.clone();
        if let (Some(old), Some(new)) = (single.as_ref(), res.name.as_ref()) {
            let dir = &targets[0].0;
            let new = new.trim();
            if !new.is_empty() && new != old.as_str() {
                if let Err(e) = std::fs::rename(dir.join(old), dir.join(new)) {
                    let line = messages::rename_failure(old, &e.to_string());
                    self.log.error(&line);
                    dialog::message_box(
                        &self.wnd,
                        "名前の変更",
                        &line,
                        dialog::MessageStyle::Error,
                    );
                    return Ok(());
                }
                self.log.normal(&messages::rename(old, new));
                paths = vec![dir.join(new)];
                cursor_name = Some(new.to_owned());
            }
        }

        // 複数一括の名前変換（大文字/小文字・拡張子）。各ファイルを自分の出自で改名し新パスへ差し替える。
        if single.is_none() && res.name_case != rerics_core::NameCase::None {
            let mut new_paths = Vec::with_capacity(targets.len());
            let mut rename_errors = 0usize;
            for (dir, name, is_dir) in &targets {
                let new_name = res.name_case.apply(name, *is_dir);
                if new_name == *name {
                    new_paths.push(dir.join(name));
                    continue;
                }
                match std::fs::rename(dir.join(name), dir.join(&new_name)) {
                    Ok(()) => {
                        self.log.normal(&messages::rename(name, &new_name));
                        new_paths.push(dir.join(&new_name));
                    }
                    Err(e) => {
                        rename_errors += 1;
                        self.log.error(&messages::rename_failure(name, &e.to_string()));
                        new_paths.push(dir.join(name));
                    }
                }
            }
            paths = new_paths;
            if rename_errors > 0 {
                dialog::message_box(
                    &self.wnd,
                    "名前の変更",
                    &format!("{rename_errors} 件の名前変更に失敗しました（ログ参照）。"),
                    dialog::MessageStyle::Warning,
                );
            }
        }

        // 属性・更新日時の適用（複数なら据え置き＝None のフィールドは触らない）。
        let mut errors = 0usize;
        let mut changed = 0usize;
        let touch_attrs = res.attrs.iter().any(|a| a.is_some());
        if touch_attrs || res.modified.is_some() || res.created.is_some() {
            for p in &paths {
                match self.apply_meta(p, &res.attrs, res.modified, res.created) {
                    Ok(true) => changed += 1,
                    Ok(false) => {}
                    Err(e) => {
                        errors += 1;
                        self.log.error(&format!(
                            "属性/日時の変更に失敗: {} ({})",
                            p.display(),
                            e
                        ));
                    }
                }
            }
            if changed > 0 {
                self.log.normal(&format!("{changed} 件の属性／更新日時を変更しました。"));
            }
            if errors > 0 {
                dialog::message_box(
                    &self.wnd,
                    "名前の変更",
                    &format!("{errors} 件の属性／更新日時の変更に失敗しました（ログ参照）。"),
                    dialog::MessageStyle::Warning,
                );
            }
        }

        // サブディレクトリ再帰適用（単一ディレクトリ時のみ）。属性・日時を独立に配下へ。
        if single_is_dir && (res.sub_attr || res.sub_time) {
            let sub_attrs = if res.sub_attr { res.attrs } else { [None; 4] };
            let sub_modified = if res.sub_time { res.modified } else { None };
            let sub_created = if res.sub_time { res.created } else { None };
            let mut descendants = Vec::new();
            Self::collect_descendants(&paths[0], &mut descendants);
            let mut sub_changed = 0usize;
            let mut sub_errors = 0usize;
            for p in &descendants {
                match self.apply_meta(p, &sub_attrs, sub_modified, sub_created) {
                    Ok(true) => sub_changed += 1,
                    Ok(false) => {}
                    Err(e) => {
                        sub_errors += 1;
                        self.log.error(&format!("配下の属性/日時の変更に失敗: {} ({})", p.display(), e));
                    }
                }
            }
            if sub_changed > 0 {
                self.log.normal(&format!("配下 {sub_changed} 件の属性／日時を変更しました。"));
            }
            if sub_errors > 0 {
                dialog::message_box(
                    &self.wnd,
                    "名前の変更",
                    &format!("配下 {sub_errors} 件の変更に失敗しました（ログ参照）。"),
                    dialog::MessageStyle::Warning,
                );
            }
        }

        // 結果一覧では再検索して一覧を最新化する（結果モードを保ち、改名後の新名へカーソルを寄せる）。
        if self.view(is_left).state().borrow().find_result {
            self.refresh_side(is_left, cursor_name.as_deref())?;
        } else {
            match cursor_name {
                Some(n) => self.reload_side_focus(is_left, &n, false)?,
                None => self.reload_side(is_left)?,
            }
        }
        Ok(())
    }

    /// `root` 配下の全エントリ（ファイル・ディレクトリ）を再帰収集する。シンボリックリンク
    /// 等の reparse は辿らない（`file_type().is_dir()` は付け替え先を辿らないため自然に除外）。
    pub(crate) fn collect_descendants(root: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(root) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            out.push(path.clone());
            if is_dir {
                Self::collect_descendants(&path, out);
            }
        }
    }

    /// 1ファイルへ属性（据え置き＝None は触らない）と更新日時を適用する。
    /// 何か変更したら `Ok(true)`、変更対象が無ければ `Ok(false)`。
    pub(crate) fn apply_meta(
        &self,
        path: &std::path::Path,
        attrs: &[Option<bool>; 4],
        modified: Option<std::time::SystemTime>,
        created: Option<std::time::SystemTime>,
    ) -> std::io::Result<bool> {
        let mut did = false;
        if let Some(t) = modified {
            rerics_core::set_modified_time(path, t)?;
            did = true;
        }
        if let Some(t) = created {
            rerics_core::set_created_time(path, t)?;
            did = true;
        }
        if attrs.iter().any(|a| a.is_some()) {
            let mut cur = rerics_core::read_attrs(path).unwrap_or_default();
            if let Some(v) = attrs[0] {
                cur.readonly = v;
            }
            if let Some(v) = attrs[1] {
                cur.hidden = v;
            }
            if let Some(v) = attrs[2] {
                cur.system = v;
            }
            if let Some(v) = attrs[3] {
                cur.archive = v;
            }
            rerics_core::write_attrs(path, cur)?;
            did = true;
        }
        Ok(did)
    }

    /// アクティブペインの選択（無ければカーソル）を確認ダイアログ付きで削除する。
    pub(crate) fn delete(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            return self.delete_in_archive(is_left);
        }
        // 結果一覧では項目ごとに出自が異なるので、出自別にまとめて1タスクで処理する。
        let groups = if self.view(is_left).state().borrow().find_result {
            Some(self.selected_result_groups(is_left))
        } else {
            None
        };
        let names: Vec<String> = match &groups {
            Some(g) => g.iter().flat_map(|(_, n)| n.iter().cloned()).collect(),
            None => self.selected_or_cursor_names(is_left),
        };
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        if self.config.borrow().file_ops.ask_before_delete {
            let short = short_desc(&names);
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
        match groups {
            Some(groups) => {
                let base_src = self.pane(is_left).borrow().path().to_path_buf();
                self.start_delete_grouped(base_src, groups)?;
            }
            None => {
                let dir = self.pane(is_left).borrow().path().to_path_buf();
                self.start_delete(dir, names)?;
            }
        }
        Ok(())
    }

    /// 選択（無ければカーソル）をゴミ箱へ送る（確認ダイアログ付き・実FSのみ・同期）。
    pub(crate) fn send_to_recycled(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではゴミ箱送りは未対応です。");
            return Ok(());
        }
        let targets = self.selected_real_targets(is_left);
        if targets.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        if self.config.borrow().file_ops.ask_before_delete {
            let names: Vec<String> = targets.iter().map(|(_, n)| n.clone()).collect();
            let short = short_desc(&names);
            let ans = dialog::message_box(
                &self.wnd,
                "ゴミ箱へ送る",
                &format!("{short}をゴミ箱へ送りますか？"),
                dialog::MessageStyle::YesNo,
            );
            if ans != dialog::MessageResult::Yes {
                return Ok(());
            }
        }
        for (path, name) in &targets {
            self.log.normal(&messages::send_to_recycled(name));
            if let Err(e) = shell::send_to_recycle(std::slice::from_ref(path)) {
                self.log.error(&messages::send_to_recycled_failure(name, &e));
            }
        }
        // 削除系はファイルセットが変わるので、カーソルは位置（index）で保つ（Delete と揃える）。
        self.refresh_side(is_left, None)?;
        Ok(())
    }

    /// 選択（無ければカーソル）項目を反対側ペインのディレクトリへ、Explorer のダイアログ
    /// （`IFileOperation`）でコピー/移動する。完了後に両ペインを最新化する（実FSのみ）。
    pub(crate) fn shell_transfer(&self, is_left: bool, move_it: bool) -> w::AnyResult<()> {
        let label = if move_it { "シェル移動" } else { "シェルコピー" };
        if self.pane(is_left).borrow().is_archive() || self.pane(!is_left).borrow().is_archive() {
            self.log.warn(&format!("書庫では{label}は使えません。"));
            return Ok(());
        }
        let items: Vec<PathBuf> =
            self.selected_real_targets(is_left).into_iter().map(|(p, _)| p).collect();
        if items.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let Some(dst) = self.pane(!is_left).borrow().as_real_path().map(|p| p.to_path_buf()) else {
            self.log.warn(&format!("反対側が実フォルダではないため{label}できません。"));
            return Ok(());
        };
        let res = if move_it {
            shell::shell_move(self.wnd.hwnd(), &items, &dst)
        } else {
            shell::shell_copy(self.wnd.hwnd(), &items, &dst)
        };
        match res {
            Ok(true) => self.log.normal(&format!("{label}: {} 件", items.len())),
            Ok(false) => self.log.info(&format!("{label}を中止しました。")),
            Err(e) => self.log.error(&format!("{label}に失敗しました: {e}")),
        }
        self.refresh_side(is_left, None)?;
        self.refresh_side(!is_left, None)?;
        Ok(())
    }

    /// 選択（無ければカーソル）項目を Explorer のダイアログ（`IFileOperation`）で完全削除する。
    /// 確認・進捗はシェルが出す。完了後にアクティブ側を最新化する（実FSのみ）。
    pub(crate) fn shell_delete_op(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではシェル削除は使えません。");
            return Ok(());
        }
        let items: Vec<PathBuf> =
            self.selected_real_targets(is_left).into_iter().map(|(p, _)| p).collect();
        if items.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        match shell::shell_delete(self.wnd.hwnd(), &items) {
            Ok(true) => self.log.normal(&format!("シェル削除: {} 件", items.len())),
            Ok(false) => self.log.info("シェル削除を中止しました。"),
            Err(e) => self.log.error(&format!("シェル削除に失敗しました: {e}")),
        }
        self.refresh_side(is_left, None)?;
        Ok(())
    }

    /// カーソル項目を Explorer のダイアログ（`IFileOperation`）で名前変更する。新名を本体の入力
    /// ダイアログで尋ね、同名衝突はシェルが解決する。完了後に新名へカーソルを移す（実FSのみ）。
    pub(crate) fn shell_rename_op(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではシェル名前変更は使えません。");
            return Ok(());
        }
        let name = {
            let view = self.view(is_left);
            let state = view.state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent => it.name.clone(),
                _ => return Ok(()),
            }
        };
        let Some(dir) = self.pane(is_left).borrow().as_real_path().map(|p| p.to_path_buf()) else {
            return Ok(());
        };
        let Some(new_name) =
            self.input_with_history("名前の変更", "新しい名前を入力して下さい。", &name, "shellrename")
        else {
            return Ok(());
        };
        let new_name = new_name.trim();
        if new_name.is_empty() || new_name == name {
            return Ok(());
        }
        match shell::shell_rename(self.wnd.hwnd(), &dir.join(&name), new_name) {
            Ok(true) => {
                self.log.normal(&messages::rename(&name, new_name));
                self.reload_side_focus(is_left, new_name, false)?;
            }
            Ok(false) => self.log.info("名前変更を中止しました。"),
            Err(e) => self.log.error(&format!("名前変更に失敗しました: {e}")),
        }
        Ok(())
    }

    /// 選択（無ければカーソル）の各項目を指すショートカット（.lnk）を同じ場所に作る。
    pub(crate) fn create_shortcut(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではショートカット作成は未対応です。");
            return Ok(());
        }
        // 検索・比較の結果一覧は複数フォルダ由来の寄せ集めなので、各項目の出自ディレクトリの隣へ
        // 作る。通常モードは copy/move と同じく反対ペインを宛先にする（反対が書庫/結果一覧なら不可）。
        let find_result = self.view(is_left).state().borrow().find_result;
        let dst_dir = if find_result {
            None
        } else {
            if self.pane(!is_left).borrow().is_archive() {
                self.log.warn("反対側が書庫のためショートカットを作成できません。");
                return Ok(());
            }
            if self.view(!is_left).state().borrow().find_result {
                self.log.warn("反対側が検索結果のためショートカットを作成できません。");
                return Ok(());
            }
            Some(self.pane(!is_left).borrow().path().to_path_buf())
        };
        let targets = self.selected_real_targets(is_left);
        if targets.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let mut ok = 0usize;
        for (target, name) in &targets {
            let lnk = match &dst_dir {
                Some(dir) => dir.join(format!("{name}.lnk")),
                None => target.with_file_name(format!("{name}.lnk")),
            };
            match shell::create_shortcut(target, &lnk) {
                Ok(()) => ok += 1,
                Err(e) => self.log.error(&format!("ショートカット作成に失敗しました（{name}）：{e}")),
            }
        }
        if ok > 0 {
            self.log.normal(&format!("ショートカットを作成しました: {ok} 件"));
        }
        // 反対ペインに作ったら増えた項目を見せるためそちらを、結果一覧なら出自に散らばり一覧へは
        // 現れないのでアクティブ側を（結果モードを保ったまま）再読込する。
        self.refresh_side(if find_result { is_left } else { !is_left }, None)?;
        Ok(())
    }

    /// 選択（無ければカーソル）のパスをクリップボードへ載せる（`move_it`＝切り取り）。
    pub(crate) fn clip_copy(&self, is_left: bool, move_it: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではクリップボード操作は未対応です。");
            return Ok(());
        }
        let targets = self.selected_real_targets(is_left);
        if targets.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let paths: Vec<PathBuf> = targets.iter().map(|(p, _)| p.clone()).collect();
        match shell::clip_copy_files(self.wnd.hwnd(), &paths, move_it) {
            Ok(()) => {
                let verb = if move_it { "切り取り" } else { "コピー" };
                self.log.normal(&format!("クリップボードへ{verb}: {} 件", paths.len()));
            }
            Err(e) => self.log.error(&format!("クリップボード操作に失敗しました: {e}")),
        }
        Ok(())
    }

    /// クリップボードのファイルを現在地へ貼り付ける（コピー/移動はクリップボードの指定に従う）。
    pub(crate) fn clip_paste(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内へは貼り付けできません。");
            return Ok(());
        }
        let (paths, move_it) = match shell::clip_paste_files(self.wnd.hwnd()) {
            Ok(v) => v,
            // クリップボードを開けない等のシステム失敗は、黙らせずエラーとして明示する。
            Err(e) => {
                self.log.error(&format!("クリップボードからの貼り付けに失敗しました: {e}"));
                return Ok(());
            }
        };
        if paths.is_empty() {
            // ファイルが無ければ画像（CF_DIB）を探し、あればファイルとして保存する。
            return self.try_paste_clipboard_image(is_left);
        }
        // 親ディレクトリごとにまとめて run_copy する（複数フォルダ由来でも壊れない）。
        let mut groups: std::collections::BTreeMap<PathBuf, Vec<String>> = Default::default();
        for p in &paths {
            if let (Some(par), Some(nm)) = (p.parent(), p.file_name()) {
                groups
                    .entry(par.to_path_buf())
                    .or_default()
                    .push(nm.to_string_lossy().into_owned());
            }
        }
        if groups.is_empty() {
            return Ok(());
        }
        let dst = self.pane(is_left).borrow().path().to_path_buf();
        self.start_clip_paste(dst, groups.into_iter().collect(), move_it)
    }

    /// クリップボードにファイルが無いとき、画像（CF_DIB）があればカレントディレクトリへ
    /// `clipboard_YYYYMMDD_HHMMSS.png` として保存し、その新ファイルへカーソルを移す。
    /// 画像も無ければ貼り付け対象なしとして報せる。
    fn try_paste_clipboard_image(&self, is_left: bool) -> w::AnyResult<()> {
        let stamp = rerics_core::format_stamp_compact(std::time::SystemTime::now());
        let name = format!("clipboard_{stamp}.png");
        let dest = self.pane(is_left).borrow().path().join(&name);
        match shell::clip_get_image(self.wnd.hwnd(), &dest) {
            Ok(true) => {
                self.log
                    .info(&format!("クリップボードの画像を {name} として保存しました。"));
                self.reload_side_focus(is_left, &name, false)?;
            }
            Ok(false) => {
                self.log.info("クリップボードに貼り付けられるものがありません。");
            }
            Err(e) => {
                self.log
                    .error(&format!("クリップボードの画像を保存できませんでした: {e}"));
            }
        }
        Ok(())
    }

    pub(crate) fn start_clip_paste(
        &self,
        dst: PathBuf,
        groups: Vec<(PathBuf, Vec<String>)>,
        move_it: bool,
    ) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let total: usize = groups.iter().map(|(_, n)| n.len()).sum();
        let text = if move_it { "貼り付け(移動)" } else { "貼り付け(コピー)" };
        self.register_task(id, text, format!("{total} 件"), control)?;
        let dst2 = dst.clone();
        std::thread::spawn(move || {
            let mut cancelled = false;
            let mut failed = false;
            for (src, names) in groups {
                let sum = rerics_core::run_copy(&host, &src, &dst2, &names, move_it);
                failed |= sum.err > 0;
                if sum.cancelled {
                    cancelled = true;
                    break;
                }
            }
            let kind = if move_it { OpKind::Move } else { OpKind::Copy };
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind,
                src_dir: dst2.clone(),
                dst_dir: dst2,
                cancelled,
                failed,
            });
        });
        Ok(())
    }

    /// カーソル上のファイルを設定エディタ（config の editor）で開く（外部プロセス・実FSのみ）。
    pub(crate) fn edit(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内のファイルは編集起動に未対応です。");
            return Ok(());
        }
        let name = {
            let view = self.view(is_left);
            let state = view.state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent && !it.is_dir => it.name.clone(),
                _ => return Ok(()),
            }
        };
        let path = self.pane(is_left).borrow().path().join(&name);
        let editor = self.config.borrow().editor.clone();
        if editor.trim().is_empty() {
            self.log.warn("エディタが設定されていません（config の editor）。");
            return Ok(());
        }
        match shell::launch_editor(&editor, &path) {
            Ok(()) => self.log.normal(&format!("編集: {name}")),
            Err(e) => self.log.error(&e),
        }
        Ok(())
    }

    /// 連番リネームダイアログ。プレフィックス・開始番号・桁数・拡張子保持を入力し、
    /// プレビューしながら OK で一括リネームする（実FSのみ・選択/カーソル対象）。
    pub(crate) fn rename_sequence_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内では連番リネームは未対応です。");
            return Ok(());
        }
        // テンプレート展開は元名の主部/拡張子分割に dir 判定が要るので (名前, dir か) で集める。
        let items: Vec<(String, bool)> = {
            let state = self.view(is_left).state();
            let s = state.borrow();
            let sel: Vec<(String, bool)> = s
                .items
                .iter()
                .filter(|it| it.selected && !it.is_parent)
                .map(|it| (it.name.clone(), it.is_dir))
                .collect();
            if sel.is_empty() {
                match s.items.get(s.cursor) {
                    Some(it) if !it.is_parent => vec![(it.name.clone(), it.is_dir)],
                    _ => Vec::new(),
                }
            } else {
                sel
            }
        };
        if items.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let names: Vec<String> = items.iter().map(|(n, _)| n.clone()).collect();
        let dir = self.pane(is_left).borrow().path().to_path_buf();

        // 命名規則テンプレートのプリセット（先頭＝既定）。原作 frmRenameSeq の cboFileName と同じ。
        const PRESETS: &[&str] = &[
            "File<No:0000>.ext",
            "<F:r><F:e>",
            "<F:r>_<No><F:e>",
            "<F:r>_<No:0000><F:e>",
        ];

        let (wnd, arm) = crate::dialog::modal_window("連番リネーム", 444, 268);
        let _lf = gui::Label::new(&wnd, gui::LabelOpts {
            text: "命名規則(&F):",
            position: gui::dpi(12, 15),
            size: gui::dpi(80, 16),
            ..Default::default()
        });
        // テンプレートは編集可能コンボ（プリセット選択＋自由入力）。先頭プリセットを初期選択。
        let template = gui::ComboBox::new(&wnd, gui::ComboBoxOpts {
            control_style: co::CBS::DROPDOWN,
            position: gui::dpi(96, 12),
            width: gui::dpi_x(336),
            items: PRESETS,
            selected_item: Some(0),
            ..Default::default()
        });
        // マクロ凡例。
        for (x, y, text) in [
            (14, 40, "<F:r> … 元の主部"),
            (14, 58, "<F:e> … 元の拡張子"),
            (234, 40, "<No> … 連番"),
            (234, 58, "<No:0000> … 0 で桁数指定"),
        ] {
            let _ = gui::Label::new(&wnd, gui::LabelOpts {
                text,
                position: gui::dpi(x, y),
                size: gui::dpi(210, 16),
                ..Default::default()
            });
        }

        let _lb = gui::Label::new(&wnd, gui::LabelOpts {
            text: "ファイル名主部(&B)",
            position: gui::dpi(14, 84),
            size: gui::dpi(140, 16),
            ..Default::default()
        });
        let base_case = gui::RadioGroup::new(
            &wnd,
            &["命名規則通り", "大文字", "小文字", "先頭大文字"]
                .iter()
                .enumerate()
                .map(|(i, label)| gui::RadioButtonOpts {
                    text: label,
                    position: gui::dpi(20, 104 + i as i32 * 22),
                    size: gui::dpi(130, 20),
                    selected: i == 0,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        );
        let _le = gui::Label::new(&wnd, gui::LabelOpts {
            text: "拡張子(&E)",
            position: gui::dpi(170, 84),
            size: gui::dpi(140, 16),
            ..Default::default()
        });
        let ext_case = gui::RadioGroup::new(
            &wnd,
            &["命名規則通り", "大文字", "小文字", "先頭大文字"]
                .iter()
                .enumerate()
                .map(|(i, label)| gui::RadioButtonOpts {
                    text: label,
                    position: gui::dpi(176, 104 + i as i32 * 22),
                    size: gui::dpi(130, 20),
                    selected: i == 0,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        );

        let _ln = gui::Label::new(&wnd, gui::LabelOpts {
            text: "連番(<No>)",
            position: gui::dpi(322, 84),
            size: gui::dpi(110, 16),
            ..Default::default()
        });
        let _ls = gui::Label::new(&wnd, gui::LabelOpts {
            text: "開始番号",
            position: gui::dpi(322, 106),
            size: gui::dpi(110, 16),
            ..Default::default()
        });
        let start = gui::Edit::new(&wnd, gui::EditOpts {
            text: "1",
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(322, 124),
            width: gui::dpi_x(70),
            height: gui::dpi_y(22),
            ..Default::default()
        });
        let _li = gui::Label::new(&wnd, gui::LabelOpts {
            text: "増分",
            position: gui::dpi(322, 152),
            size: gui::dpi(110, 16),
            ..Default::default()
        });
        let step = gui::Edit::new(&wnd, gui::EditOpts {
            text: "1",
            control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
            position: gui::dpi(322, 170),
            width: gui::dpi_x(70),
            height: gui::dpi_y(22),
            ..Default::default()
        });

        let preview = gui::Label::new(&wnd, gui::LabelOpts {
            text: "",
            position: gui::dpi(14, 200),
            size: gui::dpi(418, 18),
            ..Default::default()
        });
        let ok = gui::Button::new(&wnd, gui::ButtonOpts {
            text: "OK",
            control_style: co::BS::DEFPUSHBUTTON,
            ctrl_id: 1,
            position: gui::dpi(256, 230),
            width: gui::dpi_x(80),
            height: gui::dpi_y(26),
            ..Default::default()
        });
        let cancel = gui::Button::new(&wnd, gui::ButtonOpts {
            text: "中止(&S)",
            ctrl_id: 2,
            position: gui::dpi(344, 230),
            width: gui::dpi_x(86),
            height: gui::dpi_y(26),
            ..Default::default()
        });

        // 入力を読み取り (テンプレ, 開始, 刻み, 主部変換, 拡張子変換) にまとめる。
        // 開始はパース不可で 0、刻みは 0/不可なら 1（1以上にクランプ）。
        let read_params = {
            let template = template.clone();
            let start = start.clone();
            let step = step.clone();
            let base_case = base_case.clone();
            let ext_case = ext_case.clone();
            move || {
                let t = template.hwnd().GetWindowText().unwrap_or_default();
                let s = start.text().unwrap_or_default().trim().parse::<u64>().unwrap_or(0);
                let st = step.text().unwrap_or_default().trim().parse::<u64>().unwrap_or(1).max(1);
                let bc = rerics_core::SeqCase::from_index(base_case.selected_index().unwrap_or(0));
                let ec = rerics_core::SeqCase::from_index(ext_case.selected_index().unwrap_or(0));
                (t, s, st, bc, ec)
            }
        };

        // プレビュー更新（入力変化のたびに先頭・末尾の変換例を出す）。
        let update: std::rc::Rc<dyn Fn()> = {
            let read_params = read_params.clone();
            let preview = preview.clone();
            let items = items.clone();
            let names = names.clone();
            std::rc::Rc::new(move || {
                let (t, s, st, bc, ec) = read_params();
                let news = rerics_core::sequence_rename(&items, &t, s, st, bc, ec);
                let text = match (names.first(), news.first()) {
                    (Some(o1), Some(n1)) if names.len() > 1 => format!(
                        "例: {o1} → {n1}  …  {} → {}",
                        names.last().unwrap(),
                        news.last().unwrap()
                    ),
                    (Some(o1), Some(n1)) => format!("例: {o1} → {n1}"),
                    _ => String::new(),
                };
                let _ = preview.hwnd().SetWindowText(&text);
            })
        };
        for ed in [&start, &step] {
            let u = update.clone();
            ed.on().en_change(move || {
                u();
                Ok(())
            });
        }
        {
            let u = update.clone();
            template.on().cbn_edit_change(move || {
                u();
                Ok(())
            });
        }
        {
            // プリセット選択時はコンボのテキストを選択値へ同期してから更新する。
            let u = update.clone();
            let tmpl = template.clone();
            template.on().cbn_sel_change(move || {
                if let Ok(Some(t)) = tmpl.items().selected_text() {
                    let _ = tmpl.hwnd().SetWindowText(&t);
                }
                u();
                Ok(())
            });
        }
        for grp in [&base_case, &ext_case] {
            let u = update.clone();
            grp.on().bn_clicked(move || {
                u();
                Ok(())
            });
        }

        #[cfg(feature = "debug-server")]
        arm.plain(
            "rename_seq",
            "連番リネーム",
            "",
            false,
            vec![("OK".to_string(), 1u16), ("中止(&S)".to_string(), 2u16)],
        );
        {
            let template = template.clone();
            let update = update.clone();
            arm.on_create(move |_| {
                update();
                template.hwnd().SetFocus();
                Ok(())
            });
        }

        {
            let this = self.clone();
            let wnd2 = wnd.clone();
            let read_params = read_params.clone();
            let items = items.clone();
            let names = names.clone();
            let dir = dir.clone();
            ok.on().bn_clicked(move || {
                let (t, s, st, bc, ec) = read_params();
                // 空テンプレは何もしない（ダイアログは閉じない）。
                if t.trim().is_empty() {
                    return Ok(());
                }
                let news = rerics_core::sequence_rename(&items, &t, s, st, bc, ec);
                this.apply_sequence_rename(is_left, &dir, &names, &news);
                wnd2.close();
                Ok(())
            });
        }
        {
            let wnd2 = wnd.clone();
            cancel.on().bn_clicked(move || {
                wnd2.close();
                Ok(())
            });
        }

        let _ = wnd.show_modal(&self.wnd);
        let _ = (template, start, step, base_case, ext_case, preview, ok, cancel);
        Ok(())
    }

    /// 連番リネームの実行：集合内の入れ替えでも壊れないよう一時名を経由する二段階改名。
    /// 新名の重複・集合外の既存ファイルとの衝突は中止する。
    pub(crate) fn apply_sequence_rename(&self, is_left: bool, dir: &Path, olds: &[String], news: &[String]) {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for n in news {
            if !seen.insert(n.as_str()) {
                self.log.error(&format!("連番リネーム中止：新しい名前が重複します（{n}）"));
                return;
            }
        }
        let old_set: HashSet<&str> = olds.iter().map(String::as_str).collect();
        for n in news {
            if !old_set.contains(n.as_str()) && dir.join(n).exists() {
                self.log.error(&format!("連番リネーム中止：既存ファイルと衝突します（{n}）"));
                return;
            }
        }
        let mut tmps = Vec::new();
        for (i, old) in olds.iter().enumerate() {
            let tmp = format!("{old}.rerics-seq-{i}");
            if let Err(e) = std::fs::rename(dir.join(old), dir.join(&tmp)) {
                self.log.error(&format!("リネーム失敗（{old}）：{e}"));
                let _ = self.reload_side(is_left);
                return;
            }
            tmps.push(tmp);
        }
        for (tmp, new) in tmps.iter().zip(news.iter()) {
            if let Err(e) = std::fs::rename(dir.join(tmp), dir.join(new)) {
                self.log.error(&format!("リネーム失敗（→{new}）：{e}"));
            }
        }
        self.log.normal(&format!("連番リネーム: {} 件", news.len()));
        let _ = self.reload_side(is_left);
    }

    /// スクリプト発の一括改名。`(旧フルパス, 新フルパス)` を順に処理し、同名衝突は `conflict_box`
    /// で解決する（「全部に適用」を覚える）。実FS のみ・件数サマリを返す（一覧の再読込は呼び出し側）。
    pub(crate) fn rename_files_with_conflict(
        &self,
        pairs: Vec<(String, String)>,
    ) -> crate::script::RenameSummary {
        use rerics_core::ConflictResolution;
        let mut summary = crate::script::RenameSummary::default();
        let mut apply_all: Option<ConflictResolution> = None;
        'outer: for (from, to) in pairs {
            let from = PathBuf::from(from);
            if !from.exists() {
                summary.err += 1;
                self.log.error(&format!("改名元がありません: {}", from.display()));
                continue;
            }
            let mut target = PathBuf::from(to);
            // 別名選択時は新しいターゲットで衝突を再判定するためループする。
            // 大文字小文字だけの変更（同一ファイル）は衝突ではないのでそのまま通す。
            while target.exists() && !Self::same_file(&from, &target) {
                let res = match &apply_all {
                    Some(r) => r.clone(),
                    None => {
                        let name = file_name_of(&target);
                        let (r, all) = dialog::conflict_box(&self.wnd, &name);
                        if all {
                            apply_all = Some(r.clone());
                        }
                        r
                    }
                };
                match res {
                    ConflictResolution::Skip => {
                        summary.skip += 1;
                        continue 'outer;
                    }
                    ConflictResolution::Cancel => {
                        summary.cancelled = true;
                        break 'outer;
                    }
                    ConflictResolution::Rename(new_name) => {
                        target = target.with_file_name(new_name);
                        continue;
                    }
                    ConflictResolution::Newest if !Self::is_newer(&from, &target) => {
                        summary.skip += 1;
                        continue 'outer;
                    }
                    ConflictResolution::Newest | ConflictResolution::Overwrite => {}
                    ConflictResolution::OverwriteForce => Self::clear_attrs(&target),
                }
                // 上書き確定：Windows の rename は既存先を置き換えられないので先に消す。
                if let Err(e) = Self::remove_path(&target) {
                    summary.err += 1;
                    self.log.error(&format!("上書き準備に失敗: {} ({e})", target.display()));
                    continue 'outer;
                }
                break;
            }
            match std::fs::rename(&from, &target) {
                Ok(()) => {
                    summary.ok += 1;
                    self.log.normal(&rerics_core::messages::rename(
                        &file_name_of(&from),
                        &file_name_of(&target),
                    ));
                }
                Err(e) => {
                    summary.err += 1;
                    self.log.error(&rerics_core::messages::rename_failure(
                        &file_name_of(&from),
                        &e.to_string(),
                    ));
                }
            }
        }
        summary
    }

    /// 2 つのパスが同一ファイルを指すか（大文字小文字だけ異なる改名を衝突と誤判定しないため）。
    fn same_file(a: &Path, b: &Path) -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => a == b,
        }
    }

    /// `src` の更新日時が `dst` より新しいか（[`ConflictResolution::Newest`] 判定用）。
    fn is_newer(src: &Path, dst: &Path) -> bool {
        match (rerics_core::modified_time(src), rerics_core::modified_time(dst)) {
            (Some(s), Some(d)) => s > d,
            _ => false,
        }
    }

    /// ファイル・ディレクトリのどちらでも消す（上書きの前処理）。
    fn remove_path(p: &Path) -> std::io::Result<()> {
        if p.is_dir() {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        }
    }

    /// 読み込み専用/隠し/システム属性を解除する（[`ConflictResolution::OverwriteForce`] 用）。
    fn clear_attrs(p: &Path) {
        let mut a = rerics_core::read_attrs(p).unwrap_or_default();
        a.readonly = false;
        a.hidden = false;
        a.system = false;
        let _ = rerics_core::write_attrs(p, a);
    }

    /// 削除をワーカースレッドで起動し、払い出したタスク `id` を返す（[`start_copy`] と同様、
    /// スクリプトの async 操作が完了を待つのに使える）。
    pub(crate) fn start_delete(&self, dir: PathBuf, names: Vec<String>) -> w::AnyResult<u64> {
        let control = Arc::new(TaskControl::new());
        let id = self.next_id();
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        )
        .with_task_id(id);
        self.register_task(id, "削除", short_desc(&names), control)?;
        std::thread::spawn(move || {
            let sum = rerics_core::run_delete(&host, &dir, &names);
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind: OpKind::Delete,
                src_dir: dir.clone(),
                dst_dir: dir,
                cancelled: sum.cancelled,
                failed: sum.err > 0,
            });
        });
        Ok(id)
    }

    /// 検索・比較の結果一覧から、出自ディレクトリ別にまとめた項目を削除する。1タスクで各
    /// グループを順に処理し、完了は基準ペイン（結果一覧）の場所を通知する＝
    /// [`on_op_done`](Self::on_op_done) が結果一覧を基準ディレクトリへ復帰させる。
    pub(crate) fn start_delete_grouped(
        &self,
        base_src: PathBuf,
        groups: Vec<(PathBuf, Vec<String>)>,
    ) -> w::AnyResult<u64> {
        let control = Arc::new(TaskControl::new());
        let id = self.next_id();
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        )
        .with_task_id(id);
        let all_names: Vec<String> = groups.iter().flat_map(|(_, n)| n.iter().cloned()).collect();
        self.register_task(id, "削除", short_desc(&all_names), control)?;
        std::thread::spawn(move || {
            let mut cancelled = false;
            let mut failed = false;
            for (dir, names) in &groups {
                let sum = rerics_core::run_delete(&host, dir, names);
                failed |= sum.err > 0;
                if sum.cancelled {
                    cancelled = true;
                    break;
                }
            }
            let _ = host.tx.send(WorkerEvent::Done {
                id,
                kind: OpKind::Delete,
                src_dir: base_src.clone(),
                dst_dir: base_src,
                cancelled,
                failed,
            });
        });
        Ok(id)
    }

    /// 削除をワーカースレッドで起動する。
    /// カーソル/選択のディスク使用量を再帰計算する（実FSのみ・別スレッド）。完了は
    /// `DirInfoDone` で受け、結果をダイアログ＋ログに出す。
    pub(crate) fn directory_information(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内では使用量計算は未対応です。");
            return Ok(());
        }
        // 結果一覧では項目ごとに出自が異なるので、出自別にまとめて1タスクで合算する。
        if self.view(is_left).state().borrow().find_result {
            let groups = self.selected_result_groups(is_left);
            if groups.is_empty() {
                self.log.error(&messages::not_selected_error());
                return Ok(());
            }
            return self.start_dir_info_grouped(groups);
        }
        let names = self.selected_or_cursor_names(is_left);
        if names.is_empty() {
            self.log.error(&messages::not_selected_error());
            return Ok(());
        }
        let dir = self.pane(is_left).borrow().path().to_path_buf();
        self.start_dir_info(dir, names)
    }

    pub(crate) fn start_dir_info(&self, dir: PathBuf, names: Vec<String>) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let label = short_desc(&names);
        self.register_task(id, "情報", label.clone(), control)?;
        std::thread::spawn(move || {
            let info = rerics_core::run_calc_size(&host, &dir, &names);
            let _ = host.tx.send(WorkerEvent::DirInfoDone {
                id,
                label,
                bytes: info.bytes,
                files: info.files,
                dirs: info.dirs,
            });
        });
        Ok(())
    }

    /// 結果一覧用に、出自ディレクトリ別にまとめた項目の使用量を1タスクで合算する。
    pub(crate) fn start_dir_info_grouped(&self, groups: Vec<(PathBuf, Vec<String>)>) -> w::AnyResult<()> {
        let control = Arc::new(TaskControl::new());
        let host = ChannelHost::new(
            self.task_tx.clone(),
            self.shutdown.clone(),
            control.clone(),
            self.progress_seq.clone(),
        );
        let id = self.next_id();
        let all_names: Vec<String> = groups.iter().flat_map(|(_, n)| n.iter().cloned()).collect();
        let label = short_desc(&all_names);
        self.register_task(id, "情報", label.clone(), control)?;
        std::thread::spawn(move || {
            let info = rerics_core::run_calc_size_groups(&host, &groups);
            let _ = host.tx.send(WorkerEvent::DirInfoDone {
                id,
                label,
                bytes: info.bytes,
                files: info.files,
                dirs: info.dirs,
            });
        });
        Ok(())
    }

    /// カーソル項目の Windows シェルのプロパティシートを開く（実FSのみ・モードレス）。
    pub(crate) fn property_dialog(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではプロパティ表示に未対応です。");
            return Ok(());
        }
        let name = {
            let view = self.view(is_left);
            let state = view.state();
            let s = state.borrow();
            match s.items.get(s.cursor) {
                Some(it) if !it.is_parent => it.name.clone(),
                _ => return Ok(()),
            }
        };
        let Some(dir) = self.cursor_dir(is_left).as_real_path().map(|p| p.to_path_buf()) else {
            return Ok(());
        };
        let path = dir.join(&name);
        if let Err(e) = shell::show_properties(self.wnd.hwnd(), &path) {
            self.log.error(&e);
        }
        Ok(())
    }

    /// 選択（無ければカーソル）した項目に対し、シェルのコンテキストメニューを表示する。
    pub(crate) fn context_menu(&self, is_left: bool) -> w::AnyResult<()> {
        if self.pane(is_left).borrow().is_archive() {
            self.log.warn("書庫内ではコンテキストメニューに未対応です。");
            return Ok(());
        }
        let paths: Vec<PathBuf> = self
            .selected_real_targets(is_left)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        if paths.is_empty() {
            return Ok(());
        }
        // キーで開いた直後はそのキーの WM_CHAR がキューに残り、TrackPopupMenu のモーダル
        // ループがそれをアクセスキー入力として食う→不一致でビープが鳴る。先に捨てる。
        crate::flush_pending_chars();
        if let Err(e) = shell::show_context_menu(self.wnd.hwnd(), &paths) {
            self.log.error(&e);
        }
        Ok(())
    }

    /// 履歴つき入力ダイアログ。用途キー `key` の履歴（新しい順）を候補に出し、
    /// 確定した値を履歴へ追記して保存する。`history.toml` に永続。
    pub(crate) fn input_with_history(
        &self,
        title: &str,
        message: &str,
        value: &str,
        key: &str,
    ) -> Option<String> {
        let mut hist = rerics_core::InputHistory::load();
        let items = hist.get(key);
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let result = dialog::input_box_full(
            &self.wnd,
            title,
            message,
            value,
            dialog::InputMode::Plain,
            dialog::InputSelect::AsIs,
            Some(&refs),
        );
        if let Some(v) = &result {
            hist.add(key, v);
            let _ = hist.save();
        }
        result
    }

    /// マスク（ワイルドカード）入力。初期値は「呼び側指定 > 直近の確定値 > `*`」で、開いた
    /// 瞬間に全選択するので上書きしやすい。確定値は用途キー `key` の履歴に積み、次回の既定に
    /// なる（＝2 回続けて操作すれば前回値を覚えている）。
    pub(crate) fn input_mask(&self, title: &str, message: &str, value: &str, key: &str) -> Option<String> {
        let mut hist = rerics_core::InputHistory::load();
        let items = hist.get(key);
        let initial = if value.trim().is_empty() {
            items.first().cloned().unwrap_or_else(|| "*".to_owned())
        } else {
            value.to_owned()
        };
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let result = dialog::input_box_full(
            &self.wnd,
            title,
            message,
            &initial,
            dialog::InputMode::Plain,
            dialog::InputSelect::All,
            Some(&refs),
        );
        if let Some(v) = &result {
            hist.add(key, v);
            let _ = hist.save();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::CompressFormat;

    #[test]
    fn resolve_prefers_known_extension_over_seed() {
        // 名前の拡張子が既知ならそれが優先（seed と食い違っても名前どおり）。
        assert_eq!(resolve_compress("x.7z", CompressFormat::Zip, false).0, CompressKind::SevenZ);
        assert_eq!(resolve_compress("x.zip", CompressFormat::Xz, false).0, CompressKind::Zip);
        assert_eq!(resolve_compress("x.tar.xz", CompressFormat::Zip, false).0, CompressKind::TarXz);
        assert_eq!(resolve_compress("x.txz", CompressFormat::Zip, false).0, CompressKind::TarXz);
    }

    #[test]
    fn resolve_xz_single_vs_tar_by_bundling() {
        // 単体ファイルの .xz は単体 xz、束ねが要るなら tar.xz へ格上げして名前も直す。
        assert_eq!(resolve_compress("a.txt.xz", CompressFormat::Xz, false), (CompressKind::Xz, "a.txt.xz".to_owned()));
        assert_eq!(resolve_compress("a.txt.xz", CompressFormat::Xz, true), (CompressKind::TarXz, "a.txt.tar.xz".to_owned()));
    }

    #[test]
    fn resolve_appends_seed_extension_when_unknown() {
        // 既知拡張子が無ければ seed 形式の拡張子を補う。
        assert_eq!(resolve_compress("foo", CompressFormat::Zip, false), (CompressKind::Zip, "foo.zip".to_owned()));
        assert_eq!(resolve_compress("foo", CompressFormat::SevenZ, true), (CompressKind::SevenZ, "foo.7z".to_owned()));
        assert_eq!(resolve_compress("foo", CompressFormat::Xz, false), (CompressKind::Xz, "foo.xz".to_owned()));
        assert_eq!(resolve_compress("foo", CompressFormat::Xz, true), (CompressKind::TarXz, "foo.tar.xz".to_owned()));
    }

    #[test]
    fn per_item_xz_dir_becomes_tar_xz() {
        assert_eq!(per_item_compress("d", CompressFormat::Xz, true), (CompressKind::TarXz, "d.tar.xz".to_owned()));
        assert_eq!(per_item_compress("a.txt", CompressFormat::Xz, false), (CompressKind::Xz, "a.txt.xz".to_owned()));
        assert_eq!(per_item_compress("a", CompressFormat::SevenZ, false), (CompressKind::SevenZ, "a.7z".to_owned()));
    }
}
