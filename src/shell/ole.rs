//! Clipboard and drag-and-drop plumbing for the shelf.

use crate::island::{Event, Post};
use crate::modules::shelf::Item;
use std::path::PathBuf;
use windows::core::{implement, w, Ref, Result, HRESULT, HSTRING};
use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HANDLE, HGLOBAL, HWND, POINTL, S_OK,
};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GHND};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, OleGetClipboard, RegisterDragDrop,
    ReleaseStgMedium, DROPEFFECT, DROPEFFECT_COPY,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{
    BHID_DataObject, DragQueryFileW, IShellItem, SHCreateItemFromParsingName, DROPFILES, HDROP,
};

const CF_UNICODETEXT: u16 = 13;
const CF_HDROP: u16 = 15;
/// Longest text the shelf keeps from one copy.
const TEXT_LIMIT: usize = 4000;

/// Read UTF-16 text out of a global memory block.
unsafe fn text_from(global: HGLOBAL) -> Option<String> {
    let pointer = GlobalLock(global).cast::<u16>();
    if pointer.is_null() {
        return None;
    }
    let units = (GlobalSize(global) / 2).min(TEXT_LIMIT);
    let slice = std::slice::from_raw_parts(pointer, units);
    let len = slice.iter().position(|c| *c == 0).unwrap_or(units);
    let text = String::from_utf16_lossy(&slice[..len]);
    let _ = GlobalUnlock(global);
    (!text.trim().is_empty()).then_some(text)
}

unsafe fn files_from(drop: HDROP) -> Vec<Item> {
    let count = DragQueryFileW(drop, u32::MAX, None);
    (0..count.min(8))
        .filter_map(|index| {
            let mut name = [0u16; 1024];
            let len = DragQueryFileW(drop, index, Some(&mut name)) as usize;
            (len > 0).then(|| Item::File(PathBuf::from(String::from_utf16_lossy(&name[..len]))))
        })
        .collect()
}

/// What is on the clipboard now, unless its owner asked not to be recorded
/// (password managers set that marker).
pub fn read_clipboard(hwnd: HWND) -> Vec<Item> {
    // SAFETY: the clipboard is opened and closed around the reads; handles belong to it.
    unsafe {
        let private = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        if private != 0 && IsClipboardFormatAvailable(private).is_ok() {
            return Vec::new();
        }
        if OpenClipboard(Some(hwnd)).is_err() {
            return Vec::new();
        }
        let items = if let Ok(handle) = GetClipboardData(u32::from(CF_HDROP)) {
            files_from(HDROP(handle.0))
        } else if let Ok(handle) = GetClipboardData(u32::from(CF_UNICODETEXT)) {
            text_from(HGLOBAL(handle.0)).map(Item::Text).into_iter().collect()
        } else {
            Vec::new()
        };
        let _ = CloseClipboard();
        items
    }
}

/// Put an item back on the clipboard.
pub fn write_clipboard(hwnd: HWND, item: &Item) -> bool {
    let (format, bytes): (u16, Vec<u8>) = match item {
        Item::Text(text) => {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            (CF_UNICODETEXT, wide.iter().flat_map(|u| u.to_le_bytes()).collect())
        }
        Item::File(path) => {
            // The layout Explorer uses: a header, then a double-NUL-terminated list.
            let header = DROPFILES { pFiles: std::mem::size_of::<DROPFILES>() as u32, fWide: true.into(), ..Default::default() };
            // SAFETY: DROPFILES is plain data.
            let mut bytes =
                unsafe { std::slice::from_raw_parts((&header as *const DROPFILES).cast::<u8>(), std::mem::size_of::<DROPFILES>()) }
                    .to_vec();
            let list: Vec<u16> = path.to_string_lossy().encode_utf16().chain([0, 0]).collect();
            bytes.extend(list.iter().flat_map(|u| u.to_le_bytes()));
            (CF_HDROP, bytes)
        }
    };
    // SAFETY: the block is sized for `bytes`, filled while locked, and owned by the clipboard on success.
    unsafe {
        let Ok(block) = GlobalAlloc(GHND, bytes.len()) else { return false };
        let pointer = GlobalLock(block).cast::<u8>();
        if pointer.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        let _ = GlobalUnlock(block);
        if OpenClipboard(Some(hwnd)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let stored = SetClipboardData(u32::from(format), Some(HANDLE(block.0))).is_ok();
        let _ = CloseClipboard();
        stored
    }
}

fn items_in(data: &IDataObject) -> Vec<Item> {
    let format = |cf: u16| FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: each medium is released after its global block has been read.
    unsafe {
        if let Ok(mut medium) = data.GetData(&format(CF_HDROP)) {
            let items = files_from(HDROP(medium.u.hGlobal.0));
            ReleaseStgMedium(&mut medium);
            return items;
        }
        if let Ok(mut medium) = data.GetData(&format(CF_UNICODETEXT)) {
            let text = text_from(medium.u.hGlobal);
            ReleaseStgMedium(&mut medium);
            return text.map(Item::Text).into_iter().collect();
        }
    }
    Vec::new()
}

#[implement(IDropTarget)]
struct DropTarget {
    post: Post,
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(&self, _data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _at: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        self.post.send(Event::DragOver(true));
        // SAFETY: OLE passes a valid out-pointer.
        unsafe { *effect = DROPEFFECT_COPY };
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _at: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        // SAFETY: as above.
        unsafe { *effect = DROPEFFECT_COPY };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        self.post.send(Event::DragOver(false));
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _at: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        if let Some(data) = data.as_ref() {
            let items = items_in(data);
            if !items.is_empty() {
                self.post.send(Event::Dropped(items));
            }
        }
        self.post.send(Event::DragOver(false));
        // SAFETY: as above.
        unsafe { *effect = DROPEFFECT_COPY };
        Ok(())
    }
}

/// Let things be dropped on the island. OLE must already be initialised on this thread.
pub fn accept_drops(hwnd: HWND, post: Post) {
    let target: IDropTarget = DropTarget { post }.into();
    // SAFETY: the window keeps the target alive until it is destroyed.
    unsafe {
        let _ = RegisterDragDrop(hwnd, &target);
    }
}

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: windows::core::BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        const LEFT_BUTTON: u32 = 1;
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & LEFT_BUTTON == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Drag `item` out to another app. Blocks until it is dropped or cancelled, so
/// it must be called from the message loop, not from inside a frame.
pub fn drag_out(hwnd: HWND, item: &Item) {
    // SAFETY: COM objects created and used on the UI thread.
    unsafe {
        let data: Option<IDataObject> = match item {
            // Files go out as real shell items, with every format Explorer offers.
            Item::File(path) => SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path.as_os_str()), None)
                .and_then(|shell| shell.BindToHandler::<_, IDataObject>(None, &BHID_DataObject))
                .ok(),
            // Text goes out as the clipboard's own data object.
            Item::Text(_) => write_clipboard(hwnd, item).then(|| OleGetClipboard().ok()).flatten(),
        };
        if let Some(data) = data {
            let source: IDropSource = DropSource.into();
            let mut effect = DROPEFFECT::default();
            let _ = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
        }
    }
}
