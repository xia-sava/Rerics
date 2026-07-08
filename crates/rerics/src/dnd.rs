//! OLE ドラッグ＆ドロップの低レベル配線。
//!
//! 送信（`DoDragDrop` の呼び出し）は winsafe に `IDropSource`/`DoDragDrop` が無いため
//! `windows` crate 側で実装する。受信（`IDropTarget` 登録）は winsafe の `IDropTarget`
//! ラッパーを使い、この module はそちらが CF_HDROP を読むためのヘルパーだけを提供する。

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use winsafe::{self as w, co, prelude::*};

use windows::core::implement;
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK};
use windows::Win32::System::Com::{CoTaskMemFree, IDataObject};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK,
    DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MODIFIERKEYS_FLAGS, MK_LBUTTON};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{BHID_DataObject, SHCreateShellItemArrayFromIDLists, SHParseDisplayName};

/// `IDropSource` の最小実装。カーソルは既定（OS 描画）に任せ、Esc でキャンセル・
/// 左ボタン解放で確定するだけの標準挙動にする。
#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: windows::core::BOOL,
        grfkeystate: MODIFIERKEYS_FLAGS,
    ) -> windows::core::HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if grfkeystate.0 & MK_LBUTTON.0 == 0 {
            return DRAGDROP_S_DROP;
        }
        S_OK
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows::core::HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// 絶対パス群から `ITEMIDLIST` を作る。失敗した分は無視し、1件も作れなければ空。
fn parse_pidls(paths: &[PathBuf]) -> Vec<*mut ITEMIDLIST> {
    let mut pidls = Vec::with_capacity(paths.len());
    for p in paths {
        let wide: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let ok = unsafe {
            SHParseDisplayName(windows::core::PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None).is_ok()
        };
        if ok && !pidl.is_null() {
            pidls.push(pidl);
        }
    }
    pidls
}

fn free_pidls(pidls: &[*mut ITEMIDLIST]) {
    for p in pidls {
        unsafe { CoTaskMemFree(Some(*p as *const _)) };
    }
}

/// 選択中の絶対パス群を渡し、OLE ドラッグを同期的に実行する（`DoDragDrop` はブロッキング
/// で戻り値はドロップ確定まで待つ）。実際の転送（コピー/移動）はドロップ先の
/// `IDropTarget::Drop` が担うので、ここでは成立した効果だけを返す。ドロップ不成立・
/// キャンセル・エラーはすべて `None`。
pub(crate) fn begin_drag(paths: &[PathBuf]) -> Option<co::DROPEFFECT> {
    if paths.is_empty() {
        return None;
    }
    let pidls = parse_pidls(paths);
    if pidls.is_empty() {
        return None;
    }
    let result = (|| -> windows::core::Result<DROPEFFECT> {
        let refs: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const ITEMIDLIST).collect();
        let array = unsafe { SHCreateShellItemArrayFromIDLists(&refs) }?;
        let data_object: IDataObject = unsafe { array.BindToHandler(None, &BHID_DataObject) }?;
        let drop_source: IDropSource = DropSource.into();
        let allowed = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0 | DROPEFFECT_LINK.0);
        let mut effect = DROPEFFECT_NONE;
        let hr = unsafe { DoDragDrop(&data_object, &drop_source, allowed, &mut effect) };
        Ok(if hr == DRAGDROP_S_DROP { effect } else { DROPEFFECT_NONE })
    })();
    free_pidls(&pidls);
    result.ok().filter(|e| *e != DROPEFFECT_NONE).map(|e| unsafe { co::DROPEFFECT::from_raw(e.0) })
}

/// winsafe の `IDataObject`（`IDropTarget` 系イベントで受け取るもの）から CF_HDROP の
/// パス一覧を取り出す。形式が無い・取得失敗はすべて空 Vec（呼び出し側はドロップ不可扱い）。
pub(crate) fn hdrop_paths(data: &w::IDataObject) -> Vec<PathBuf> {
    let mut fmt = w::FORMATETC::default();
    fmt.cfFormat = co::CF::HDROP;
    fmt.dwAspect = co::DVASPECT::CONTENT;
    fmt.tymed = co::TYMED::HGLOBAL;
    let Ok(medium) = (unsafe { data.GetData(&fmt) }) else {
        return Vec::new();
    };
    let Some(hglobal) = (unsafe { medium.ptr_hglobal() }) else {
        return Vec::new();
    };
    let Ok(ptr_lock) = hglobal.GlobalLock() else {
        return Vec::new();
    };
    let hdrop = unsafe { w::HDROP::from_ptr(ptr_lock.as_ptr() as _) };
    let Ok(it) = hdrop.DragQueryFile() else {
        return Vec::new();
    };
    it.filter_map(Result::ok).map(PathBuf::from).collect()
}
