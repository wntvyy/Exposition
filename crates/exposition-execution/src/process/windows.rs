//! Windows process-tree containment with Job Objects.
//!
//! This is the only module in the crate that uses `unsafe`: thin, checked
//! wrappers around kernel32 job-object, toolhelp, and thread calls.
//!
//! Launch sequence (no window in which the child can escape):
//! 1. Create an unnamed, non-inheritable job object with
//!    `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and no breakaway permission.
//! 2. Spawn the child with `CREATE_SUSPENDED | CREATE_NO_WINDOW`; its primary
//!    thread has not executed a single instruction, so it cannot have created
//!    descendants yet.
//! 3. `AssignProcessToJobObject`. Every process the child creates afterwards
//!    is placed in the same job automatically and cannot break away.
//! 4. Resume the primary thread (found via a toolhelp thread snapshot, because
//!    `std` does not expose the thread handle on stable Rust).
//!
//! `TerminateJobObject` kills the whole tree for cancel/timeout/finish.
//! Dropping [`Containment`] closes the only job handle; with
//! `KILL_ON_JOB_CLOSE` the kernel then kills every remaining process — this is
//! what guarantees cleanup if the application exits or crashes.
#![allow(unsafe_code)]

use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

/// Exit code given to processes killed through the job.
const KILLED_EXIT_CODE: u32 = 0xE000_0001;

/// An owned kernel handle closed on drop.
struct OwnedHandle(HANDLE);

// SAFETY: a job object handle is a process-wide kernel handle; the job APIs
// used here are thread-safe.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid handle we own and close exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Containment for one process tree.
pub struct Containment {
    job: OwnedHandle,
}

fn create_job() -> io::Result<OwnedHandle> {
    // SAFETY: null security attributes (non-inheritable handle) and null name.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = OwnedHandle(job);
    // SAFETY: zero-initialization is a valid value for this plain C struct.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    // SAFETY: pointer and size describe `limits`, which outlives the call.
    let ok = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// Resume every thread of a process created suspended (it has exactly one).
fn resume_process(pid: u32) -> io::Result<()> {
    // SAFETY: plain snapshot request; the returned handle is checked and owned.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return Err(io::Error::last_os_error());
    }
    let snapshot = OwnedHandle(snapshot);
    // SAFETY: zeroed THREADENTRY32 with dwSize set is the documented input.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let mut resumed = 0usize;
    // SAFETY: `entry` is a valid, correctly sized out-parameter.
    let mut more = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: opening a thread by id; the handle is checked and owned.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = OwnedHandle(thread);
            // SAFETY: valid thread handle with THREAD_SUSPEND_RESUME access.
            if unsafe { ResumeThread(thread.0) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            resumed += 1;
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
    }
    if resumed == 0 {
        return Err(io::Error::other("suspended child process has no threads to resume"));
    }
    Ok(())
}

/// Spawn `command` inside a fresh kill-on-close job object.
pub fn spawn_contained(command: &mut Command) -> io::Result<(Child, Containment)> {
    let job = create_job()?;
    command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let mut child = command.spawn()?;
    let process = child.as_raw_handle() as HANDLE;
    // SAFETY: both handles are valid; the child is suspended.
    if unsafe { AssignProcessToJobObject(job.0, process) } == 0 {
        let error = io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    if let Err(error) = resume_process(child.id()) {
        // SAFETY: valid job handle.
        unsafe {
            TerminateJobObject(job.0, KILLED_EXIT_CODE);
        }
        let _ = child.wait();
        return Err(error);
    }
    Ok((child, Containment { job }))
}

impl Containment {
    /// Kill every process in the tree. Idempotent.
    pub fn kill_tree(&self) {
        // SAFETY: valid job handle owned by `self`.
        unsafe {
            TerminateJobObject(self.job.0, KILLED_EXIT_CODE);
        }
    }

    /// Number of live processes in the tree, if the OS reports it.
    pub fn active_processes(&self) -> Option<u32> {
        // SAFETY: zeroed plain C struct as out-parameter of the right size.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: pointer/size describe `info`.
        let ok = unsafe {
            QueryInformationJobObject(
                self.job.0,
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(info.ActiveProcesses)
    }
}

/// Exit codes at or above this value are NTSTATUS failure codes (crashes,
/// unhandled exceptions), reported as abnormal termination.
pub fn describe_abnormal_exit(code: i32) -> Option<String> {
    let unsigned = code as u32;
    if unsigned == KILLED_EXIT_CODE {
        return Some("killed through its process-tree container".into());
    }
    (unsigned >= 0xC000_0000).then(|| format!("terminated abnormally (status 0x{unsigned:08X})"))
}
