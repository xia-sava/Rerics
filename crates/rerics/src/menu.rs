//! メインメニューバー。原作のメニューツリーを再現し、対応コマンドのある項目だけ有効化する。
//!
//! 未対応の項目はグレーアウトで掲示だけする（整理は後続）。各有効項目には一意の ID を振り、
//! `build` が返す `ID → Command` の対応を使って `wm_command_acc_menu` から実行へつなぐ。

use std::collections::HashMap;

use rerics_core::Command;
use winsafe::{self as w, co};

/// メニュー項目。`label == "-"` はセパレータ、`cmd == None` は未対応（グレーアウト）。
struct Item {
    label: &'static str,
    cmd: Option<Command>,
}

/// 1つのトップレベルメニューとその項目列。
struct MenuDef {
    label: &'static str,
    items: &'static [Item],
}

const SEP: Item = Item { label: "-", cmd: None };

const fn on(label: &'static str, cmd: Command) -> Item {
    Item { label, cmd: Some(cmd) }
}

const fn off(label: &'static str) -> Item {
    Item { label, cmd: None }
}

const MENUS: &[MenuDef] = &[
    MenuDef {
        label: "Records(&X)",
        items: &[
            on("設定(&S)", Command::OpenSettings),
            off("プラグインの設定"),
            SEP,
            on("タブを閉じる(&C)", Command::CloseTab),
            on("再起動(&R)", Command::Restart),
            on("終了(&X)", Command::Quit),
            off("Debug"),
        ],
    },
    MenuDef {
        label: "編集(&E)",
        items: &[
            on("コピー(&C)", Command::Copy),
            on("移動(&M)", Command::Move),
            on("削除(&D)", Command::Delete),
            off("ごみ箱"),
            on("ディレクトリの作成", Command::MakeDirectory),
            SEP,
            off("シェル項目"),
            SEP,
            off("検索(&F)"),
            off("ディレクトリ比較"),
            off("ディレクトリの容量計算"),
            SEP,
            on("すべて選択(&A)", Command::SelectAll),
        ],
    },
    MenuDef {
        label: "表示(&V)",
        items: &[
            off("ドライブリスト(&D)"),
            off("ソート(&S)"),
            on("パスマスク(&P)", Command::PathMask),
            off("登録ディレクトリ(&R)"),
            off("ディレクトリ履歴(&H)"),
            off("キーバインドリスト"),
            off("ログ表示切替"),
            off("サムネイル表示切替"),
            SEP,
            on("最新の情報に更新", Command::Reload),
        ],
    },
    MenuDef {
        label: "ツール(&T)",
        items: &[
            off("マイコンピュータで開く"),
            off("親ディレクトリを新しいタブで開く(&N)"),
            SEP,
            off("シェル項目"),
            SEP,
            on("圧縮(&P)", Command::Compress),
            on("解凍(&U)", Command::Extract),
            SEP,
            off("コマンドプロンプト"),
            off("実行コマンドの入力"),
            off("ファイル名付き実行コマンドの入力"),
            on("新規ファイルの作成", Command::CreateFile),
            off("テキストエディタ"),
        ],
    },
    MenuDef {
        label: "登録(&R)",
        items: &[
            off("ファイルの関連付けに追加(&F)"),
            off("登録ディレクトリに追加(&D)"),
        ],
    },
    MenuDef {
        label: "その他(&O)",
        items: &[
            off("自動更新"),
            off("ログをコピー"),
            off("ログクリア"),
            off("一時ファイルをクリア"),
            off("列幅の保存"),
            off("列幅の復元"),
            off("ネットワークドライブの割り当て"),
            off("ネットワークドライブの切断"),
            off("仮想ドライブの切断"),
        ],
    },
    MenuDef {
        label: "ヘルプ(&H)",
        items: &[off("ヘルプ")],
    },
];

/// メニュー項目 ID の起点（ダイアログの制御 ID と衝突しない高い値から採番）。
const MENU_ID_BASE: u16 = 0xE000;

/// メニューバーを構築し、`(HMENU, 項目ID → Command)` を返す。
pub fn build() -> w::SysResult<(w::HMENU, HashMap<u16, Command>)> {
    let bar = w::HMENU::CreateMenu()?;
    let mut map = HashMap::new();
    let mut next = MENU_ID_BASE;
    for md in MENUS {
        let popup = w::HMENU::CreatePopupMenu()?;
        for item in md.items {
            if item.label == "-" {
                popup.AppendMenu(co::MF::SEPARATOR, w::IdMenu::None, w::BmpPtrStr::None)?;
            } else if let Some(cmd) = item.cmd {
                let id = next;
                next += 1;
                map.insert(id, cmd);
                popup.AppendMenu(co::MF::STRING, w::IdMenu::Id(id), w::BmpPtrStr::from_str(item.label))?;
            } else {
                popup.AppendMenu(
                    co::MF::STRING | co::MF::GRAYED,
                    w::IdMenu::None,
                    w::BmpPtrStr::from_str(item.label),
                )?;
            }
        }
        bar.AppendMenu(co::MF::POPUP, w::IdMenu::Menu(&popup), w::BmpPtrStr::from_str(md.label))?;
    }
    Ok((bar, map))
}
