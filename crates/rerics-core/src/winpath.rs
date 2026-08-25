//! Win32 の生 API へ渡すパス文字列。
//!
//! 前置なしのパスは終端 NUL 込み `MAX_PATH`（260 文字）までしか受け付けられず、超えると
//! `ERROR_INVALID_NAME` で弾かれる。`\\?\` を前置した verbatim 形式なら上限が外れるので、
//! 超える長さのときだけその形へ均す。収まるパスはそのまま渡す（verbatim は `.`／`..`・
//! スラッシュの正規化を経ないので、届く範囲まで広げると解釈が変わる）。
//!
//! Shell API（`SHFileOperation`・`ShellExecute`・`SHParseDisplayName` 等）は verbatim を
//! 受け付けないため、ここは通さず実行ファイルのマニフェスト（`longPathAware`）に委ねる。

use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, Prefix};

/// 前置なしのパスに Win32 が課す長さの上限（終端 NUL を含む文字数）。
const MAX_PATH: usize = 260;

/// `path` を Win32 へ渡す null 終端 UTF-16 列にする。上限を超える長さなら `\\?\` 前置へ均す。
pub fn wide_path(path: &Path) -> Vec<u16> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    if wide.len() < MAX_PATH {
        return wide;
    }
    verbatim(path).unwrap_or(wide)
}

/// `path` を `\\?\` 前置の verbatim 形式（null 終端 UTF-16）へ均す。verbatim は正規化を経ない
/// ので、相対パスの絶対化と `.`／`..`・スラッシュの畳み込みを先に済ませる。UNC は
/// `\\?\UNC\server\share` 形式にする。均せない形（デバイス名前空間）は `None`。
fn verbatim(path: &Path) -> Option<Vec<u16>> {
    let abs = std::path::absolute(path).ok()?;
    let Some(Component::Prefix(prefix)) = abs.components().next() else {
        return None;
    };
    let build = |head: &str, skip: usize| -> Vec<u16> {
        head.encode_utf16()
            .chain(abs.as_os_str().encode_wide().skip(skip))
            .chain(std::iter::once(0))
            .collect()
    };
    match prefix.kind() {
        Prefix::Verbatim(_) | Prefix::VerbatimUNC(..) | Prefix::VerbatimDisk(_) => {
            Some(build("", 0))
        }
        Prefix::Disk(_) => Some(build(r"\\?\", 0)),
        // `\\server\share` の先頭 1 文字を落として `\\?\UNC` へ継ぐ。
        Prefix::UNC(..) => Some(build(r"\\?\UNC", 1)),
        Prefix::DeviceNS(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 終端 NUL を落として文字列へ戻す。
    fn text(wide: &[u16]) -> String {
        String::from_utf16_lossy(&wide[..wide.len() - 1])
    }

    /// `head` に続けて 38 文字 × 8 段の深いパスを作る（`MAX_PATH` 超え）。
    fn deep(head: &str) -> String {
        let segments = ["a", "b", "c", "d", "e", "f", "g", "h"].map(|c| c.repeat(38)).join("\\");
        format!("{head}\\{segments}")
    }

    #[test]
    fn short_path_is_passed_through() {
        assert_eq!(text(&wide_path(Path::new(r"C:\tmp\a.txt"))), r"C:\tmp\a.txt");
    }

    #[test]
    fn long_path_gets_verbatim_prefix() {
        let long = deep(r"C:\tmp");
        assert!(long.len() >= MAX_PATH);
        assert_eq!(text(&wide_path(Path::new(&long))), format!(r"\\?\{long}"));
    }

    #[test]
    fn long_unc_path_gets_unc_verbatim_prefix() {
        let long = deep(r"\\server\share");
        // `\\server\share\…` の先頭 1 文字を落として `\\?\UNC` に継いだ形。
        assert_eq!(text(&wide_path(Path::new(&long))), format!(r"\\?\UNC{}", &long[1..]));
    }

    #[test]
    fn long_verbatim_path_is_kept_as_is() {
        let long = format!(r"\\?\{}", deep(r"C:\tmp"));
        assert_eq!(text(&wide_path(Path::new(&long))), long);
    }
}
