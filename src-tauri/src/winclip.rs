//! Windows clipboard interop for files.
//!
//! Explorer and the desktop exchange files through the `CF_HDROP` clipboard
//! format plus a "Preferred DropEffect" companion format that says whether the
//! source was copied or cut. Speaking that dialect means Ctrl+C here pastes in
//! Explorer, and Ctrl+C in Explorer pastes here.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{HANDLE, HGLOBAL};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::Shell::{DragQueryFileW, DROPFILES, HDROP};

const CF_HDROP: u32 = 15;
const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_MOVE: u32 = 2;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Id of the "Preferred DropEffect" format, registered on first use.
fn drop_effect_format() -> u32 {
    let name = wide("Preferred DropEffect");
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// Allocate a moveable HGLOBAL and fill it from `fill`.
unsafe fn alloc_global(size: usize, fill: impl FnOnce(*mut u8)) -> Option<HGLOBAL> {
    let h = GlobalAlloc(GMEM_MOVEABLE, size);
    if h.is_null() {
        return None;
    }
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        return None;
    }
    std::ptr::write_bytes(p, 0, size);
    fill(p);
    GlobalUnlock(h);
    Some(h)
}

/// Publish `paths` on the Windows clipboard as a file drop.
pub fn set_files(paths: &[String], cut: bool) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    // The path list is a double-null-terminated block of UTF-16 strings.
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(OsStr::new(p).encode_wide());
        list.push(0);
    }
    list.push(0);

    let header = std::mem::size_of::<DROPFILES>();
    let bytes = header + list.len() * 2;

    unsafe {
        let hdrop = alloc_global(bytes, |p| {
            let df = p as *mut DROPFILES;
            (*df).pFiles = header as u32;
            (*df).fWide = 1; // UTF-16 paths
            std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, p.add(header), list.len() * 2);
        })
        .ok_or("clipboard allocation failed")?;

        let effect = alloc_global(4, |p| {
            *(p as *mut u32) = if cut { DROPEFFECT_MOVE } else { DROPEFFECT_COPY };
        });

        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("clipboard is busy".into());
        }
        EmptyClipboard();
        // Ownership of both blocks passes to the clipboard on success.
        if SetClipboardData(CF_HDROP, hdrop as HANDLE).is_null() {
            CloseClipboard();
            return Err("could not write the clipboard".into());
        }
        let fmt = drop_effect_format();
        if let (Some(h), true) = (effect, fmt != 0) {
            SetClipboardData(fmt, h as HANDLE);
        }
        CloseClipboard();
    }
    Ok(())
}

/// Read a file drop off the Windows clipboard: `(paths, cut)`.
pub fn get_files() -> Option<(Vec<String>, bool)> {
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let h = GetClipboardData(CF_HDROP);
        if h.is_null() {
            CloseClipboard();
            return None;
        }
        let drop = h as HDROP;
        let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(drop, i, buf.as_mut_ptr(), buf.len() as u32);
            if got > 0 {
                out.push(String::from_utf16_lossy(&buf[..got as usize]));
            }
        }

        // Copy or cut? Absent format means copy, which is Explorer's default.
        let mut cut = false;
        let fmt = drop_effect_format();
        if fmt != 0 {
            let he = GetClipboardData(fmt);
            if !he.is_null() {
                let p = GlobalLock(he as HGLOBAL) as *const u32;
                if !p.is_null() {
                    cut = (*p & DROPEFFECT_MOVE) != 0;
                }
                GlobalUnlock(he as HGLOBAL);
            }
        }
        CloseClipboard();
        if out.is_empty() {
            None
        } else {
            Some((out, cut))
        }
    }
}
