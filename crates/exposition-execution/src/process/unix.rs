//! Unix process-tree containment with process groups.
//!
//! The child is placed in a new process group (`setpgid(0, 0)` via
//! `CommandExt::process_group(0)`) before it executes; descendants inherit the
//! group. `killpg(SIGKILL)` terminates the tree. A descendant that calls
//! `setsid()`/`setpgid()` can leave the group — process groups are weaker than
//! Windows job objects (cgroups would be needed for a hard guarantee). This
//! path is structurally supported but unverified: Windows is the primary
//! verified platform.
#![allow(unsafe_code)]

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

pub struct Containment {
    pgid: libc::pid_t,
}

pub fn spawn_contained(command: &mut Command) -> io::Result<(Child, Containment)> {
    command.process_group(0);
    let child = command.spawn()?;
    let pgid = child.id() as libc::pid_t;
    Ok((child, Containment { pgid }))
}

impl Containment {
    pub fn kill_tree(&self) {
        // SAFETY: killpg has no memory-safety preconditions; an already-empty
        // group yields ESRCH, which is ignored.
        unsafe {
            libc::killpg(self.pgid, libc::SIGKILL);
        }
    }

    pub fn active_processes(&self) -> Option<u32> {
        None
    }
}

impl Drop for Containment {
    fn drop(&mut self) {
        self.kill_tree();
    }
}

pub fn describe_abnormal_exit(_code: i32) -> Option<String> {
    None
}
