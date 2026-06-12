//! 設定/状態ファイルの保存先解決・TOML 汎用読み書き・状態データモデル・
//! 作業領域クランプの純関数。UI には依存しない。

use std::path::{Path, PathBuf};

/// 設定/状態ファイルの保存先ディレクトリを返す。
///
/// 実行ファイルと同じディレクトリに `Rerics.portable` があればそのディレクトリを
/// 使う（ポータブルモード）。無ければ `%APPDATA%\Rerics`。いずれも解決できない
/// 場合はカレントディレクトリ。この関数はディレクトリを作らない。
pub fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("Rerics.portable").exists() {
                return dir.to_path_buf();
            }
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            return PathBuf::from(appdata).join("Rerics");
        }
    }
    PathBuf::from(".")
}

/// ユーザ設定ファイル（手編集想定）のパス。
pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

/// 自動更新される状態ファイルのパス。
pub fn state_path() -> PathBuf {
    data_dir().join("state.toml")
}

/// TOML ファイルを読んでデシリアライズする。ファイルが無い・読めない・
/// パースに失敗したいずれの場合も `T::default()` を返す。
pub fn load_toml<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// 値を TOML としてファイルへ書き出す。親ディレクトリは自動で用意する。
/// 直列化エラーは `InvalidData` の `io::Error` に変換する。
pub fn save_toml<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, text)
}

/// ウィンドウの位置・サイズ・最大化状態。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

/// 自動保存される全体状態。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowState>,
}

impl State {
    /// 状態ファイルから読み込む（無ければ既定）。
    pub fn load() -> Self {
        load_toml(&state_path())
    }

    /// 状態ファイルへ保存する。
    pub fn save(&self) -> std::io::Result<()> {
        save_toml(&state_path(), self)
    }
}

/// 矩形 (x, y, w, h) を作業領域 work=(left, top, right, bottom) 内に収めるよう
/// 補正した左上座標 (x, y) を返す。サイズは変えない。
/// 右/下がはみ出す場合は左/上へ寄せ、それでも左/上が作業領域より手前なら
/// left/top に合わせる。
pub fn clamp_to_work(x: i32, y: i32, w: i32, h: i32, work: (i32, i32, i32, i32)) -> (i32, i32) {
    let (left, top, right, bottom) = work;
    let nx = if right - w < left {
        left
    } else {
        (x.min(right - w)).max(left)
    };
    let ny = if bottom - h < top {
        top
    } else {
        (y.min(bottom - h)).max(top)
    };
    (nx, ny)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_state_roundtrip() {
        let st = WindowState { x: 10, y: 20, width: 800, height: 600, maximized: true };
        let s = toml::to_string(&st).unwrap();
        let back: WindowState = toml::from_str(&s).unwrap();
        assert_eq!(st, back);
    }

    #[test]
    fn state_roundtrip() {
        let st = State {
            window: Some(WindowState { x: 1, y: 2, width: 3, height: 4, maximized: false }),
        };
        let s = toml::to_string(&st).unwrap();
        let back: State = toml::from_str(&s).unwrap();
        assert_eq!(st.window, back.window);
    }

    #[test]
    fn default_state_serializes_without_window() {
        let st = State::default();
        assert!(st.window.is_none());
        let s = toml::to_string(&st).unwrap();
        assert!(!s.contains("[window]"));
    }

    #[test]
    fn load_missing_returns_default() {
        let path = std::env::temp_dir().join("rerics_test_missing_does_not_exist.toml");
        let _ = std::fs::remove_file(&path);
        let st: State = load_toml(&path);
        assert!(st.window.is_none());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let path = std::env::temp_dir().join("rerics_test_state_roundtrip.toml");
        let _ = std::fs::remove_file(&path);
        let st = State {
            window: Some(WindowState { x: 100, y: 200, width: 960, height: 560, maximized: true }),
        };
        save_toml(&path, &st).unwrap();
        let back: State = load_toml(&path);
        assert_eq!(st.window, back.window);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clamp_inside_is_unchanged() {
        let work = (0, 0, 1920, 1080);
        assert_eq!(clamp_to_work(100, 100, 400, 300, work), (100, 100));
    }

    #[test]
    fn clamp_bottom_right_overflow_moves_to_top_left() {
        let work = (0, 0, 1000, 800);
        assert_eq!(clamp_to_work(900, 700, 400, 300, work), (600, 500));
    }

    #[test]
    fn clamp_negative_origin_moves_to_work_origin() {
        let work = (0, 0, 1920, 1080);
        assert_eq!(clamp_to_work(-50, -30, 400, 300, work), (0, 0));
    }

    #[test]
    fn clamp_window_larger_than_work_pins_top_left() {
        let work = (10, 20, 410, 220);
        assert_eq!(clamp_to_work(100, 100, 800, 600, work), (10, 20));
    }
}
