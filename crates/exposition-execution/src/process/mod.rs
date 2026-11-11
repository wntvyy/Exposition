//! Process-tree containment: spawn a child so that it and every descendant can
//! be killed together, and are killed if the application goes away.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{Containment, describe_abnormal_exit, spawn_contained};
#[cfg(windows)]
pub use windows::{Containment, describe_abnormal_exit, spawn_contained};

use exposition_core::job::JobOutcome;

/// Map a process exit status to a job outcome.
pub fn outcome_from_status(status: std::process::ExitStatus) -> JobOutcome {
    if let Some(code) = status.code() {
        if let Some(description) = describe_abnormal_exit(code) {
            return JobOutcome::Terminated { description };
        }
        return JobOutcome::Exited { code };
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return JobOutcome::Terminated { description: format!("terminated by signal {signal}") };
        }
    }
    JobOutcome::Terminated { description: "terminated without an exit code".into() }
}
