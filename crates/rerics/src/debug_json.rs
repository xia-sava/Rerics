//! デバッグ制御サーバの応答 JSON を core 状態から組む純粋関数（feature gate 無し・常時ビルド）。
//!
//! ここへ集約することで、**本体を起動せず関数コールだけでシリアライズをユニットテスト**できる。
//! デバッグ制御サーバ（`debug-server` feature 下）は、GUI から値を集めてこれらを呼ぶ薄いラッパに徹する。
//! 引数の `*Chrome` は GUI 由来スカラー（窓サイズ・フォント実測・各バー文字列）で、テストは合成値、
//! 実機は実値を渡す。これらが `paint_to`/各 view が持つのと同じ出どころを使う限り値ズレは起きない。

use rerics_core::{Colors, FileItem, FileListState, FontSpec, Layout, Theme};
use serde_json::{Value, json};

/// ペインの GUI 由来スカラー（core 状態に無い、窓・実測・バー文字列）。
pub struct PaneChrome<'a> {
    pub location: &'a str,
    pub is_archive: bool,
    pub page_rows: usize,
    pub mask: Option<&'a str>,
    pub path_bar: &'a str,
    pub status_left: &'a str,
    pub status_right: &'a str,
}

/// 属性フラグを D/R/H/S/A の文字列へ（表示の属性列と同趣旨）。
pub fn attrs_string(it: &FileItem) -> String {
    let mut s = String::new();
    if it.is_dir {
        s.push('D');
    }
    if it.readonly {
        s.push('R');
    }
    if it.hidden {
        s.push('H');
    }
    if it.system {
        s.push('S');
    }
    if it.archive {
        s.push('A');
    }
    s
}

/// `FileItem` 1 件を `/state` 用 JSON へ。`..` は簡易形。
pub fn item_json(it: &FileItem, is_cursor: bool) -> Value {
    if it.is_parent {
        return json!({ "name": it.name, "is_parent": true, "is_dir": true, "cursor": is_cursor });
    }
    let modified = it
        .modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    json!({
        "name": it.name,
        "is_dir": it.is_dir,
        "ext": it.extension,
        "size": it.size,
        "marked": it.selected,
        "attrs": attrs_string(it),
        "cursor": is_cursor,
        "modified": modified,
        // リンク項目のみ非 null（種別トークンとリンク先）。
        "link": it.link_kind().as_token(),
        "link_target": it.link_target.as_deref(),
        // 検索・比較の結果項目のみ非 null（出自の場所と補助情報）。通常一覧では null。
        "source": it.source.as_ref().map(|l| l.loc_display()),
        "info": it.info,
    })
}

/// 片側ペインの `/state` JSON（core 状態＋GUI スカラー）。
pub fn pane_state_json(state: &FileListState, chrome: &PaneChrome) -> Value {
    let (sel_count, sel_size) = state.selected_count_size();
    let visible_end = (state.scroll_top + chrome.page_rows).min(state.items.len());
    let items: Vec<Value> = state
        .items
        .iter()
        .enumerate()
        .map(|(i, it)| item_json(it, i == state.cursor))
        .collect();
    let columns: Vec<Value> = state
        .columns
        .iter()
        .map(|c| {
            json!({
                "kind": format!("{:?}", c.kind),
                "text": c.text,
                "width": c.width,
                "align": format!("{:?}", c.align),
            })
        })
        .collect();
    json!({
        "location": chrome.location,
        "is_archive": chrome.is_archive,
        "path_bar": chrome.path_bar,
        "status_bar": { "left": chrome.status_left, "right": chrome.status_right },
        "cursor": state.cursor,
        "scroll_top": state.scroll_top,
        "page_rows": chrome.page_rows,
        "visible": [state.scroll_top, visible_end],
        "mask": chrome.mask,
        "sort": { "type": format!("{:?}", state.sort_type), "reverse": state.sort_reverse },
        "find_result": state.find_result,
        "selected_count": sel_count,
        "selected_size": sel_size,
        "columns": columns,
        "items": items,
    })
}

/// `/presentation` の上位（解決済みの設定）JSON。`Rgb` は hex 文字列へシリアライズされる。
pub fn presentation_top_json(theme: &Theme, colors: &Colors, font: &FontSpec, layout: &Layout) -> Value {
    json!({
        "theme": serde_json::to_value(theme).unwrap_or_default(),
        "resolved_colors": serde_json::to_value(colors).unwrap_or_default(),
        "font": serde_json::to_value(font).unwrap_or_default(),
        "layout": serde_json::to_value(layout).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rerics_core::{Config, FileListState};

    fn file_item(name: &str, ext: &str, is_dir: bool, selected: bool, hidden: bool) -> FileItem {
        FileItem {
            name: name.to_string(),
            base_name: name.to_string(),
            extension: ext.to_string(),
            is_dir,
            is_parent: false,
            size: Some(100),
            created: None,
            modified: None,
            accessed: None,
            readonly: false,
            hidden,
            system: false,
            archive: false,
            reparse: false,
            link: rerics_core::LinkKind::None,
            link_target: None,
            selected,
            source: None,
            info: None,
        }
    }

    #[test]
    fn pane_state_json_shapes_items_columns_cursor() {
        let mut state = FileListState::new();
        state.items = vec![
            FileItem::parent(),
            file_item("dir1", "", true, false, false),
            file_item("a.txt", "txt", false, true, false), // marked
            file_item("secret", "", false, false, true),    // hidden
        ];
        state.cursor = 2; // a.txt
        let chrome = PaneChrome {
            location: "C:\\work",
            is_archive: false,
            page_rows: 30,
            mask: Some("*.txt"),
            path_bar: "C:\\work",
            status_left: "1 selected",
            status_right: "C: free",
        };
        let v = pane_state_json(&state, &chrome);

        assert_eq!(v["location"], "C:\\work");
        assert_eq!(v["mask"], "*.txt");
        assert_eq!(v["cursor"], 2);
        assert_eq!(v["status_bar"]["right"], "C: free");

        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0]["is_parent"], true);
        assert_eq!(items[1]["attrs"], "D"); // dir
        assert_eq!(items[2]["name"], "a.txt");
        assert_eq!(items[2]["marked"], true);
        assert_eq!(items[2]["cursor"], true); // cursor on index 2
        assert_eq!(items[2]["ext"], "txt");
        assert_eq!(items[3]["attrs"], "H"); // hidden
        assert_eq!(items[1]["cursor"], false);
    }

    #[test]
    fn presentation_reflects_config_font_and_colors() {
        let mut cfg = Config::default();
        cfg.font.size = 18;
        cfg.resolve_theme(false); // dark 解決
        let v = presentation_top_json(&cfg.theme, &cfg.active_colors(), &cfg.font, &cfg.layout);

        assert_eq!(v["font"]["size"], 18);
        // 配色は hex 文字列で出る（Rgb の Serialize）。
        let cursor = v["resolved_colors"]["cursor"].as_str().unwrap();
        assert!(cursor.starts_with('#') && cursor.len() == 7, "hex color: {cursor}");
        // レイアウト寸法も出る。
        assert!(v["layout"]["tab_height"].is_number());
    }
}
