//! The wheel as an OLE drop target: Explorer (and any app) hands us the files
//! being dragged. We only ever accept a copy, so the source never deletes its
//! file, even though Shift means "move" in Explorer.
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::Foundation::{POINTL, S_OK};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows_core::{Ref, Result, implement};

#[implement(IDropTarget)]
pub struct DropTarget;

/// The file paths in a drag, or none when it carries something else (text, a browser's virtual file).
fn files(data: &IDataObject) -> Vec<PathBuf> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    unsafe {
        if data.QueryGetData(&format) != S_OK {
            return vec![];
        }
        let Ok(mut medium) = data.GetData(&format) else { return vec![] };
        let drop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(drop, u32::MAX, None);
        let mut found = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(drop, i, Some(&mut buf));
            found.push(PathBuf::from(OsString::from_wide(&buf[..len])));
        }
        ReleaseStgMedium(&mut medium);
        found
    }
}

/// COPY when the source allows it, else nothing.
fn copy_if_allowed(effect: *mut DROPEFFECT) -> bool {
    let allowed = unsafe { (*effect).0 & DROPEFFECT_COPY.0 != 0 };
    unsafe { *effect = if allowed { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
    allowed
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL,
                 effect: *mut DROPEFFECT) -> Result<()> {
        let files = data.ok().map(files).unwrap_or_default();
        let carries_files = !files.is_empty();
        super::drag_enter(files, (pt.x, pt.y));
        if !carries_files || !copy_if_allowed(effect) {
            unsafe { *effect = DROPEFFECT_NONE };
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        if super::drag_over((pt.x, pt.y)) {
            copy_if_allowed(effect);
        } else {
            unsafe { *effect = DROPEFFECT_NONE };
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        super::drag_leave();
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT)
            -> Result<()> {
        let files = data.ok().map(files).unwrap_or_default();
        let copy = copy_if_allowed(effect);
        if !super::dropped(files, (pt.x, pt.y), copy) {
            unsafe { *effect = DROPEFFECT_NONE };
        }
        Ok(())
    }
}
