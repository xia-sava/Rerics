//! バー類（タブ・パスバー・ステータスバー）共通の GDI 装飾ヘルパ。
//!
//! chrome（タブ／パスバー／ステータスバー／列ヘッダ）は配色テーマに依らず Windows の
//! システム 3D グレー（BTNFACE 系）で固定し、明るい枠でリスト本体（テーマ追従）を囲う。
//! こうすると原作（ライトグレーの枠＋黒いリスト）と同じ佇まいになる。

use winsafe::{self as w, co};

/// バー面の色（システム 3D グレー）。
pub fn face() -> w::COLORREF {
    w::GetSysColor(co::COLOR::BTNFACE)
}

/// 文書面の白（選択タブの強調背景など）。
pub fn window() -> w::COLORREF {
    w::GetSysColor(co::COLOR::WINDOW)
}

/// 隆起ベベルの明るい側（左上）。
pub fn highlight() -> w::COLORREF {
    w::GetSysColor(co::COLOR::BTNHIGHLIGHT)
}

/// 隆起ベベルの暗い側（右下）。
pub fn shadow() -> w::COLORREF {
    w::GetSysColor(co::COLOR::BTNSHADOW)
}

/// バー上の主テキスト色。
pub fn text() -> w::COLORREF {
    w::GetSysColor(co::COLOR::BTNTEXT)
}

/// バー上の控えめなテキスト色（非アクティブタブ等）。
pub fn gray_text() -> w::COLORREF {
    w::GetSysColor(co::COLOR::GRAYTEXT)
}

/// システムのメニューフォント（`NONCLIENTMETRICS.lfMenuFont`）から HFONT を作る。
/// chrome 系バー（タブ／ロケーション／ステータス）はこれで統一し、ファイル一覧の
/// 設定フォント（`cfg.font`）とは独立させる。メニュー自体も OS 標準のこのフォントで
/// 描かれるため、chrome とメニューの見た目が揃う。
pub fn ui_font() -> w::SysResult<w::guard::DeleteObjectGuard<w::HFONT>> {
    let mut ncm = w::NONCLIENTMETRICS::default();
    unsafe {
        w::SystemParametersInfo(
            co::SPI::GETNONCLIENTMETRICS,
            std::mem::size_of::<w::NONCLIENTMETRICS>() as u32,
            &mut ncm,
            co::SPIF::NoValue,
        )?;
    }
    w::HFONT::CreateFontIndirect(&ncm.lfMenuFont)
}

/// 水平線を1本引く。
pub fn hline(dc: &w::HDC, x0: i32, x1: i32, y: i32, color: w::COLORREF) -> w::AnyResult<()> {
    let pen = w::HPEN::CreatePen(co::PS::SOLID, 1, color)?;
    let _sel = dc.SelectObject(&*pen)?;
    dc.MoveToEx(x0, y, None)?;
    dc.LineTo(x1, y)?;
    Ok(())
}

/// 垂直線を1本引く。
pub fn vline(dc: &w::HDC, x: i32, y0: i32, y1: i32, color: w::COLORREF) -> w::AnyResult<()> {
    let pen = w::HPEN::CreatePen(co::PS::SOLID, 1, color)?;
    let _sel = dc.SelectObject(&*pen)?;
    dc.MoveToEx(x, y0, None)?;
    dc.LineTo(x, y1)?;
    Ok(())
}

/// 全幅の帯を BTNFACE で塗り、上辺ハイライト・下辺シャドウの隆起ベベルを引く。
pub fn fill_bar(dc: &w::HDC, cw: i32, ch: i32) -> w::AnyResult<()> {
    let brush = w::HBRUSH::CreateSolidBrush(face())?;
    dc.FillRect(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, &brush)?;
    hline(dc, 0, cw, 0, highlight())?;
    hline(dc, 0, cw, ch - 1, shadow())?;
    Ok(())
}
