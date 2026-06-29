//! ディレクトリ走査が結果を逐次返すための受け口。
//!
//! 検索・比較（[`crate::find`]・[`crate::compare`]）は、見つけた項目を貯め込んで一度に
//! 返すのではなく、この `Sink` 経由で1件ずつ呼び出し側へ渡す。これにより呼び出し側は
//! 結果一覧へライブ追加でき、`cancelled` で走査を途中から打ち切れる。

use crate::FileItem;

/// 走査結果の受け口。`emit` で1件ずつ項目を受け取り、`cancelled` で各境界の
/// 中止／中断を問い合わせる。
///
/// `cancelled` は「打ち切るべきか」を返す述語で、中断（一時停止）中はその中でブロック
/// して待ってから返す実装もあり得る（GUI のタスク制御がこれを担う）。純ロジックの
/// テストや UI を持たない呼び出しでは「常に続行」（`&|| false`）を渡す。
pub struct Sink<'a> {
    /// 見つかった項目を1件渡す。
    pub emit: &'a mut dyn FnMut(FileItem),
    /// 続行可否の問い合わせ。`true` なら走査を打ち切る。
    pub cancelled: &'a dyn Fn() -> bool,
    /// 項目を1つ走査するたびに呼ぶ進捗報告（該当の有無を問わず）。呼び側が件数を数えて
    /// 間引いて表示する。途中経過の要らない呼び出しでは何もしない `&mut || {}` を渡す。
    pub progress: &'a mut dyn FnMut(),
}

impl Sink<'_> {
    /// 項目を1件、呼び出し側へ渡す。
    pub(crate) fn push(&mut self, item: FileItem) {
        (self.emit)(item);
    }

    /// 走査を打ち切るべきか（中断中はこの中でブロックし得る）。
    pub(crate) fn is_cancelled(&self) -> bool {
        (self.cancelled)()
    }

    /// 項目を1つ走査したことを報告する（該当・非該当を問わず1件ぶん）。
    pub(crate) fn tick(&mut self) {
        (self.progress)();
    }
}
