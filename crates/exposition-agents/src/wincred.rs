//! Windows Credential Manager details the keyring crate does not expose:
//! how long and how widely the stored key persists.
//!
//! The keyring crate (3.x) writes generic credentials with
//! `CRED_PERSIST_ENTERPRISE`, which roams with a domain user's profile to
//! other machines. Exposition keeps the provider key on this machine only
//! (`CRED_PERSIST_LOCAL_MACHINE`). The credential keeps keyring's layout
//! (target `user.service`, UTF-16LE password blob), so the keyring crate (and
//! earlier Exposition versions) still read it.
#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME, GetLastError};
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_ENTERPRISE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    CredWriteW,
};

/// Persistence values (`CREDENTIALW::Persist`).
pub const PERSIST_LOCAL_MACHINE: u32 = CRED_PERSIST_LOCAL_MACHINE;
pub const PERSIST_ENTERPRISE: u32 = CRED_PERSIST_ENTERPRISE;

/// A stored generic credential. The secret is overwritten when dropped.
pub struct Stored {
    pub blob: Vec<u8>,
    pub persist: u32,
    pub user_name: String,
    pub comment: String,
}

impl Drop for Stored {
    fn drop(&mut self) {
        for byte in self.blob.iter_mut() {
            // SAFETY: a valid, aligned, exclusively borrowed byte.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// # Safety
/// `ptr` is null or a NUL-terminated UTF-16 string.
unsafe fn from_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller guarantees NUL termination.
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// The generic credential `target`, if it exists. Errors are Win32 codes.
pub fn read(target: &str) -> Result<Option<Stored>, u32> {
    let name = wide(target);
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated; on success `credential` points to a
    // CREDENTIALW allocated by the system, freed below with CredFree.
    let ok = unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if ok == 0 {
        // SAFETY: no preconditions.
        let error = unsafe { GetLastError() };
        return if error == ERROR_NOT_FOUND { Ok(None) } else { Err(error) };
    }
    // SAFETY: CredReadW succeeded, so `credential` is valid until CredFree.
    let stored = unsafe {
        let c = &*credential;
        let blob = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
        };
        Stored { blob, persist: c.Persist, user_name: from_wide(c.UserName), comment: from_wide(c.Comment) }
    };
    // SAFETY: allocated by CredReadW.
    unsafe { CredFree(credential.cast()) };
    Ok(Some(stored))
}

/// Write (create or replace) the generic credential `target`.
pub fn write(target: &str, blob: &[u8], user_name: &str, comment: &str, persist: u32) -> Result<(), u32> {
    let mut name = wide(target);
    let mut user = wide(user_name);
    let mut comment = wide(comment);
    let mut secret = blob.to_vec();
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: name.as_mut_ptr(),
        Comment: comment.as_mut_ptr(),
        LastWritten: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
        CredentialBlobSize: secret.len() as u32,
        CredentialBlob: secret.as_mut_ptr(),
        Persist: persist,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: std::ptr::null_mut(),
        UserName: user.as_mut_ptr(),
    };
    // SAFETY: every pointer in `credential` is valid for the call.
    let ok = unsafe { CredWriteW(&credential, 0) };
    // SAFETY: no preconditions.
    let error = if ok == 0 { Some(unsafe { GetLastError() }) } else { None };
    for byte in secret.iter_mut() {
        // SAFETY: a valid, aligned, exclusively borrowed byte.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    error.map_or(Ok(()), Err)
}

/// Attempts to write a credential and see it read back as written. Credential
/// Manager can lose a change of persistence when another credential write of
/// the same user (any process) races it, so every write is verified.
const ATTEMPTS: usize = 4;

/// Write `blob` with `persist` and read it back until it is stored exactly so.
fn write_verified(target: &str, blob: &[u8], user_name: &str, comment: &str, persist: u32) -> Result<bool, u32> {
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(15 * attempt as u64));
        }
        write(target, blob, user_name, comment, persist)?;
        if let Ok(Some(copy)) = read(target)
            && copy.blob == blob
            && copy.persist == persist
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Write a password as keyring stores it (UTF-16LE, no terminator), verified.
/// Returns whether it reads back exactly as written.
pub fn write_password(target: &str, password: &str, user_name: &str, comment: &str, persist: u32) -> Result<bool, u32> {
    let blob = Stored {
        blob: password.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        persist,
        user_name: String::new(),
        comment: String::new(),
    };
    write_verified(target, &blob.blob, user_name, comment, persist)
}

/// Move an existing credential to local-machine persistence, keeping its
/// bytes exactly. The credential is only ever replaced by an identical copy,
/// and never deleted; if the copy does not stay, the original is written
/// back as it was. Returns whether the credential moved.
pub fn keep_on_this_machine(target: &str) -> Result<bool, u32> {
    let Some(original) = read(target)? else { return Ok(false) };
    if original.persist == PERSIST_LOCAL_MACHINE {
        return Ok(false);
    }
    if write_verified(target, &original.blob, &original.user_name, &original.comment, PERSIST_LOCAL_MACHINE)? {
        return Ok(true);
    }
    // Never lose the key: whatever is stored now, put the original back.
    write_verified(target, &original.blob, &original.user_name, &original.comment, original.persist)?;
    Ok(false)
}
