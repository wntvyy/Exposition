//! Native file clipboard (P1): the OS clipboard's list of files, so files
//! copied in Explorer can be pasted into the project and project files copied
//! in Exposition can be pasted in Explorer.
//!
//! Windows only (CF_HDROP, plus "Preferred DropEffect" = copy so Explorer
//! copies rather than moves). The paths read here are INPUTS only: they grant
//! no access, and pasting them goes through the ordinary operation plan
//! (`OpTarget::External`), which refuses what may not be copied. Arbitrary
//! clipboard text is never interpreted. Other platforms report the clipboard
//! as unsupported instead of pretending it works.
#![allow(unsafe_code)]

use std::path::PathBuf;

/// The files on the OS clipboard (absolute paths), empty when it holds none.
pub fn read_files() -> Result<Vec<String>, String> {
    imp::read_files()
}

/// Put these absolute paths on the OS clipboard as a copied file list.
pub fn write_files(paths: &[PathBuf]) -> Result<(), String> {
    imp::write_files(paths)
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{GetLastError, GlobalFree, HANDLE, HGLOBAL, POINT};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows_sys::Win32::System::Ole::CF_HDROP;
    use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};

    /// Files read from one clipboard list (more are ignored).
    const MAX_FILES: u32 = 10_000;
    const DROPEFFECT_COPY: u32 = 1;

    /// The clipboard, open for this thread until dropped.
    struct Open;

    impl Open {
        fn new() -> Result<Self, String> {
            // Another program may hold the clipboard briefly.
            for _ in 0..40 {
                // SAFETY: no owner window; closed by `Drop`.
                if unsafe { OpenClipboard(null_mut()) } != 0 {
                    return Ok(Open);
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err("The clipboard is in use by another program; try again.".into())
        }
    }

    impl Drop for Open {
        fn drop(&mut self) {
            // SAFETY: opened by `Open::new` on this thread.
            unsafe { CloseClipboard() };
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn read_files() -> Result<Vec<String>, String> {
        let _open = Open::new()?;
        // SAFETY: the clipboard is open; CF_HDROP data is an HDROP owned by
        // the clipboard, read with DragQueryFileW while the clipboard is open.
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP as u32) == 0 {
                return Ok(Vec::new());
            }
            let handle = GetClipboardData(CF_HDROP as u32);
            if handle.is_null() {
                return Ok(Vec::new());
            }
            let drop = handle as HDROP;
            let count = DragQueryFileW(drop, u32::MAX, null_mut(), 0).min(MAX_FILES);
            let mut out = Vec::new();
            for index in 0..count {
                let len = DragQueryFileW(drop, index, null_mut(), 0);
                if len == 0 || len > 32_767 {
                    continue;
                }
                let mut buffer = vec![0u16; len as usize + 1];
                let copied = DragQueryFileW(drop, index, buffer.as_mut_ptr(), buffer.len() as u32);
                buffer.truncate(copied as usize);
                // Names that are not valid Unicode cannot be project paths.
                if let Ok(path) = OsString::from_wide(&buffer).into_string() {
                    out.push(path);
                }
            }
            Ok(out)
        }
    }

    /// Allocate movable global memory holding `bytes`.
    fn global(bytes: &[u8]) -> Result<HGLOBAL, String> {
        // SAFETY: plain allocation; the block is written while locked and
        // freed by the caller unless the clipboard takes ownership.
        unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if memory.is_null() {
                return Err("Out of memory for the clipboard.".into());
            }
            let target = GlobalLock(memory) as *mut u8;
            if target.is_null() {
                GlobalFree(memory);
                return Err("The clipboard memory could not be prepared.".into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            GlobalUnlock(memory);
            Ok(memory)
        }
    }

    pub fn write_files(paths: &[PathBuf]) -> Result<(), String> {
        // DROPFILES header, then NUL-separated wide paths ending in a double NUL.
        let header = std::mem::size_of::<DROPFILES>();
        let files = DROPFILES { pFiles: header as u32, pt: POINT { x: 0, y: 0 }, fNC: 0, fWide: 1 };
        let mut bytes: Vec<u8> = Vec::with_capacity(header + 64 * paths.len());
        // SAFETY: DROPFILES is plain old data (packed, no padding to read).
        bytes.extend_from_slice(unsafe { std::slice::from_raw_parts(&files as *const DROPFILES as *const u8, header) });
        for path in paths {
            for unit in path.as_os_str().encode_wide().chain(std::iter::once(0)) {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
        }
        bytes.extend_from_slice(&0u16.to_le_bytes());
        let list = global(&bytes)?;
        let effect = match global(&DROPEFFECT_COPY.to_le_bytes()) {
            Ok(effect) => effect,
            Err(e) => {
                // SAFETY: not handed to the clipboard.
                unsafe { GlobalFree(list) };
                return Err(e);
            }
        };
        let open = match Open::new() {
            Ok(open) => open,
            Err(e) => {
                // SAFETY: neither block was handed to the clipboard.
                unsafe {
                    GlobalFree(list);
                    GlobalFree(effect);
                }
                return Err(e);
            }
        };
        // SAFETY: the clipboard is open; on success it owns the memory.
        unsafe {
            if EmptyClipboard() == 0 {
                GlobalFree(list);
                GlobalFree(effect);
                return Err(format!("The clipboard could not be cleared (error {}).", GetLastError()));
            }
            if SetClipboardData(CF_HDROP as u32, list as HANDLE).is_null() {
                let error = GetLastError();
                GlobalFree(list);
                GlobalFree(effect);
                return Err(format!("The file list could not be put on the clipboard (error {error})."));
            }
            let format = RegisterClipboardFormatW(wide("Preferred DropEffect").as_ptr());
            if format == 0 || SetClipboardData(format, effect as HANDLE).is_null() {
                GlobalFree(effect);
            }
        }
        drop(open);
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    const UNSUPPORTED: &str =
        "Copying files to and from the system clipboard is supported on Windows only in this version.";

    pub fn read_files() -> Result<Vec<String>, String> {
        Err(UNSUPPORTED.into())
    }

    pub fn write_files(_paths: &[PathBuf]) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// REAL OS clipboard round trip. Ignored by default because it replaces
    /// whatever the developer has on the clipboard; run explicitly:
    /// `cargo test -p exposition --lib clipboard -- --ignored`.
    #[test]
    #[ignore = "replaces the user's clipboard contents"]
    fn file_lists_round_trip_through_the_real_windows_clipboard() {
        let dir = std::env::temp_dir().join(format!("exposition-clipboard-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a b.txt");
        let b = dir.join("ünïcode – 数据.csv");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let paths = [std::fs::canonicalize(&a).unwrap(), std::fs::canonicalize(&b).unwrap()];
        let shown: Vec<PathBuf> =
            paths.iter().map(|p| PathBuf::from(exposition_workspace::root::display_path(p))).collect();
        write_files(&shown).unwrap();
        let read = read_files().unwrap();
        let expected: Vec<String> = shown.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(read, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
