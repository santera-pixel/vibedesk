//! Members die with the engine, enforced by the kernel.
//!
//! `kill_on_drop` covers the ordinary path: the engine drops a member's child and
//! tokio terminates it. A killed engine drops nothing — no destructor runs, no
//! runtime shutdown — so every member it started keeps running and keeps its
//! worktree directory open. That is the mechanism behind `git worktree remove`
//! answering "Permission denied" for four directories after a cancelled run, and
//! behind a stopped worker still wedging the next run's checkout.
//!
//! A Windows job object with `KILL_ON_JOB_CLOSE` is the operating system's own
//! answer: the kernel closes the job when the last handle to it does, which
//! includes this process dying in any way, and terminates every member inside it.
//! One job serves every member; its handle is deliberately never closed, because
//! closing it is what kills them.
//!
//! Off Windows this is a no-op: the child-exit path is already covered by
//! `kill_on_drop` and by a closed stdin, and there is no portable equivalent.

#[cfg(windows)]
pub fn adopt(pid: u32) -> bool {
    imp::adopt(pid)
}

/// No-op: see the module comment. Present so callers need no `cfg`.
#[cfg(not(windows))]
pub fn adopt(_pid: u32) -> bool {
    false
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::sync::OnceLock;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

    /// Stored as an integer because a raw handle is not `Send`/`Sync`, and the job
    /// outlives every thread that adopts into it.
    static JOB: OnceLock<usize> = OnceLock::new();

    fn job() -> Option<HANDLE> {
        let stored = JOB.get_or_init(|| unsafe {
            let Ok(job) = CreateJobObjectW(None, None) else {
                return 0;
            };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set.is_err() {
                let _ = CloseHandle(job);
                return 0;
            }
            job.0 as usize
        });
        (*stored != 0).then_some(HANDLE(*stored as *mut c_void))
    }

    pub fn adopt(pid: u32) -> bool {
        let Some(job) = job() else {
            return false;
        };
        // `PROCESS_SET_QUOTA` is what assigning to a job requires; `TERMINATE` is
        // what lets the job kill the process when the engine goes.
        let Ok(process) =
            (unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) })
        else {
            return false;
        };
        let adopted = unsafe { AssignProcessToJobObject(job, process) }.is_ok();
        let _ = unsafe { CloseHandle(process) };
        adopted
    }
}
