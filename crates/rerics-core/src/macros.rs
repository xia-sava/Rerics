//! コマンド引数の実行時マクロ展開（`<...>`）。UI 非依存。
//!
//! 引数文字列に埋め込まれた `<C>`（現在パス）・`<O>`（反対パス）・`<P>`（カーソルのフルパス）等の
//! 文字列置換と、`<I:…>`（入力）・`<FOLDERDIALOG[:…]>`（フォルダ選択）・`<OPENDIALOG[:…]>`
//! （ファイルを開く）・`<SAVEDIALOG[:…]>`（保存先）のダイアログ系を展開する。
//! ダイアログ系は GUI 依存なので [`MacroHost`] 越しに GUI 層が供給する。

/// マクロ展開の中止（入力/選択のキャンセル）。呼び出し側は無音で実行を取りやめる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacroAbort;

/// ダイアログ/ピッカーなど GUI 依存のマクロを供給するホスト。
pub trait MacroHost {
    /// 入力ダイアログを開く。`title` は見出し（空なら既定見出し）。キャンセルは `None`。
    fn prompt(&self, title: &str) -> Option<String>;
    /// フォルダ選択ダイアログを開く。`title` は見出し（空なら既定見出し）。キャンセルは `None`。
    fn choose_folder(&self, title: &str) -> Option<String>;
    /// ファイルを開くダイアログ（`<OPENDIALOG>`）。キャンセルは `None`。
    fn choose_open_file(&self, title: &str) -> Option<String>;
    /// ファイル保存ダイアログ（`<SAVEDIALOG>`）。キャンセルは `None`。
    fn choose_save_file(&self, title: &str) -> Option<String>;
}

/// マクロ展開の文脈。文字列置換に使う値と、GUI ホストを保持する。
pub struct MacroCtx<'a> {
    /// 現在ペインのパス（`<C>`）。
    pub current: String,
    /// 反対ペインのパス（`<O>`）。
    pub opposite: String,
    /// カーソル位置のフルパス（`<P>`）。無ければ空。
    pub cursor_path: String,
    /// ダイアログ系マクロを開くホスト。
    pub host: &'a dyn MacroHost,
}

/// 各引数の `<...>` を展開した新しい引数列を返す。いずれかでキャンセルされたら [`MacroAbort`]。
pub fn expand_macros(args: &[String], ctx: &MacroCtx) -> Result<Vec<String>, MacroAbort> {
    args.iter().map(|a| expand_one(a, ctx)).collect()
}

/// 1引数中のマクロをすべて展開する。閉じない `<` はリテラルとして残す。
fn expand_one(arg: &str, ctx: &MacroCtx) -> Result<String, MacroAbort> {
    let mut out = String::new();
    let mut rest = arg;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            out.push_str(&rest[open..]);
            return Ok(out);
        };
        out.push_str(&expand_token(&after[..close], ctx)?);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `<...>` の中身（山括弧を除く）を1つ展開する。`name` と `name:arg` の両形に対応。
/// 未知マクロは `<...>` のままリテラルとして残す。
fn expand_token(token: &str, ctx: &MacroCtx) -> Result<String, MacroAbort> {
    let (name, arg) = match token.split_once(':') {
        Some((n, a)) => (n, a),
        None => (token, ""),
    };
    match name.to_ascii_uppercase().as_str() {
        "C" => Ok(ctx.current.clone()),
        "O" => Ok(ctx.opposite.clone()),
        "P" => Ok(ctx.cursor_path.clone()),
        "I" => ctx.host.prompt(arg).ok_or(MacroAbort),
        "FOLDERDIALOG" => ctx.host.choose_folder(arg).ok_or(MacroAbort),
        "OPENDIALOG" => ctx.host.choose_open_file(arg).ok_or(MacroAbort),
        "SAVEDIALOG" => ctx.host.choose_save_file(arg).ok_or(MacroAbort),
        _ => Ok(format!("<{token}>")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用ホスト：各ダイアログの戻り値を固定する（None でキャンセル）。
    #[derive(Default)]
    struct FakeHost {
        prompt: Option<String>,
        folder: Option<String>,
        open: Option<String>,
        save: Option<String>,
    }
    impl MacroHost for FakeHost {
        fn prompt(&self, _title: &str) -> Option<String> {
            self.prompt.clone()
        }
        fn choose_folder(&self, _title: &str) -> Option<String> {
            self.folder.clone()
        }
        fn choose_open_file(&self, _title: &str) -> Option<String> {
            self.open.clone()
        }
        fn choose_save_file(&self, _title: &str) -> Option<String> {
            self.save.clone()
        }
    }

    fn ctx<'a>(host: &'a FakeHost) -> MacroCtx<'a> {
        MacroCtx {
            current: "C:/cur".into(),
            opposite: "D:/opp".into(),
            cursor_path: "C:/cur/file.txt".into(),
            host,
        }
    }

    #[test]
    fn expands_path_substitutions() {
        let host = FakeHost::default();
        let c = ctx(&host);
        assert_eq!(expand_one("<C>", &c), Ok("C:/cur".into()));
        assert_eq!(expand_one("<O>", &c), Ok("D:/opp".into()));
        assert_eq!(expand_one("<P>", &c), Ok("C:/cur/file.txt".into()));
        // 前後のリテラル・複数マクロの混在。
        assert_eq!(expand_one("[<C>]=<O>", &c), Ok("[C:/cur]=D:/opp".into()));
        // 大小無視。
        assert_eq!(expand_one("<c>", &c), Ok("C:/cur".into()));
    }

    #[test]
    fn input_macro_uses_host_and_aborts_on_cancel() {
        let host = FakeHost { prompt: Some("typed".into()), ..Default::default() };
        assert_eq!(expand_one("<I:タイトル>", &ctx(&host)), Ok("typed".into()));

        let host = FakeHost::default();
        assert_eq!(expand_one("<I>", &ctx(&host)), Err(MacroAbort));
    }

    #[test]
    fn folder_macro_uses_host_and_aborts_on_cancel() {
        let host = FakeHost { folder: Some("E:/picked".into()), ..Default::default() };
        assert_eq!(expand_one("<FOLDERDIALOG>", &ctx(&host)), Ok("E:/picked".into()));

        let host = FakeHost::default();
        assert_eq!(expand_one("<FOLDERDIALOG:選択>", &ctx(&host)), Err(MacroAbort));
    }

    #[test]
    fn open_and_save_macros_use_host_and_abort_on_cancel() {
        let host = FakeHost { open: Some("C:/in.txt".into()), ..Default::default() };
        assert_eq!(expand_one("<OPENDIALOG>", &ctx(&host)), Ok("C:/in.txt".into()));
        let host = FakeHost { save: Some("C:/out.txt".into()), ..Default::default() };
        assert_eq!(expand_one("<SAVEDIALOG:保存先>", &ctx(&host)), Ok("C:/out.txt".into()));
        // キャンセルは中止。
        let host = FakeHost::default();
        assert_eq!(expand_one("<OPENDIALOG>", &ctx(&host)), Err(MacroAbort));
        assert_eq!(expand_one("<SAVEDIALOG>", &ctx(&host)), Err(MacroAbort));
    }

    #[test]
    fn unknown_macro_and_unclosed_bracket_left_literal() {
        let host = FakeHost::default();
        let c = ctx(&host);
        assert_eq!(expand_one("<BOGUS>", &c), Ok("<BOGUS>".into()));
        assert_eq!(expand_one("a < b", &c), Ok("a < b".into()));
        assert_eq!(expand_one("no macros", &c), Ok("no macros".into()));
    }

    #[test]
    fn expand_macros_over_list_aborts_whole() {
        let host = FakeHost::default();
        let c = ctx(&host);
        // 1つでもキャンセルされたら全体が中止。
        assert_eq!(expand_macros(&["<C>".into(), "<I>".into()], &c), Err(MacroAbort));
        // 全部成功なら展開済み列。
        let host = FakeHost { prompt: Some("x".into()), ..Default::default() };
        let c = ctx(&host);
        assert_eq!(
            expand_macros(&["<C>".into(), "<I>".into()], &c),
            Ok(vec!["C:/cur".into(), "x".into()])
        );
    }
}
