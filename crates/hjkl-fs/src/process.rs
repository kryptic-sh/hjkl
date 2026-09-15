//! Is the process that owns this on-disk record still running?
//!
//! Every file hjkl leaves behind with an owner in it — a swap file's
//! `writer_pid`, a config write-lock's body — has the same question attached:
//! the owner may have crashed, and a record whose owner is gone must be
//! reclaimable rather than binding forever. Answering it needs a per-platform
//! probe, which is why it lives beside the rest of hjkl's platform-gated I/O
//! instead of being re-derived per crate: it was, and the two copies did not
//! agree — `hjkl-config` probed `/proc` and so answered "unknown" on Windows and
//! macOS, where a crashed writer's lock then held for a full stale timeout.
//!
//! # Three states, not two
//!
//! [`pid_liveness`] returns `Option<bool>`, and the distinction is the whole
//! point:
//!
//! - `Some(true)` — the process exists.
//! - `Some(false)` — it provably does not.
//! - `None` — this platform has no probe, so nothing was learned.
//!
//! `None` must never be read as "dead". A caller that collapses it that way
//! treats every live owner as reclaimable and loses mutual exclusion entirely.
//! Collapsing it to "alive" is the opposite failure and is sometimes the right
//! one — `hjkl-app`'s swap recovery does exactly that, because a record it
//! cannot clear locks the user out of their own file. Which way to fold is the
//! caller's policy; this module only reports what it knows.
//!
//! # What it does not tell you
//!
//! Pids are reused. A `Some(true)` says *a* process holds that pid now, not that
//! it is the one that wrote the record — so liveness is a fast path for
//! reclaiming an obviously-dead owner, not a lock. Callers pair it with a
//! timeout or a real lock for the rest.

/// Report whether a process with the given `pid` is running.
///
/// - Unix: `kill(pid, 0)` — success means alive and ours, `EPERM` means alive
///   and someone else's, anything else (`ESRCH`) means gone.
/// - Windows: `OpenProcess` + a zero-timeout `WaitForSingleObject`. A process
///   object is signaled only once the process has exited, so `WAIT_OBJECT_0`
///   means dead and a timeout means running; a null handle with
///   `ERROR_ACCESS_DENIED` means the process exists but is owned by another
///   user.
/// - Anywhere else: `None`. There is no portable probe, and inventing an answer
///   would make a caller's staleness check silently wrong in one direction or
///   the other.
///
/// pid 0 is reported as `Some(false)` on every platform: no OS probe answers
/// "is pid 0 running?" the way a caller means it. POSIX defines pid 0 for `kill`
/// as *every process in the caller's process group*, so `kill(0, 0)` succeeds
/// and would report "alive"; Windows resolves pid 0 to the System Idle Process,
/// which either opens or fails access-denied — both of which read as alive. A
/// pid of 0 in a record only ever comes from a truncated or corrupted write, and
/// classifying it as live pins that record to an owner that can never exit.
pub fn pid_liveness(pid: u32) -> Option<bool> {
    if pid == 0 {
        return Some(false);
    }
    #[cfg(unix)]
    {
        // SAFETY: `kill` with signal 0 performs the permission and existence
        // checks and delivers nothing, so it has no effect on the target
        // process; it takes two integers by value and touches no memory of ours.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if r == 0 {
            return Some(true);
        }
        // EPERM: the process exists, we just may not signal it.
        Some(std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        };
        const ERROR_ACCESS_DENIED: u32 = 5;

        // SAFETY: plain Win32 FFI. `OpenProcess` takes integers only; the handle
        // it returns is checked for null before use and closed on every path
        // that obtained one, and `WaitForSingleObject` reads nothing of ours.
        unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0, // bInheritHandle = FALSE
                pid,
            );
            if handle.is_null() {
                // No such process => dead; access-denied => it exists and is
                // owned by another user.
                return Some(GetLastError() == ERROR_ACCESS_DENIED);
            }
            let wait = WaitForSingleObject(handle, 0);
            CloseHandle(handle);
            Some(wait != WAIT_OBJECT_0)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        // No cheap probe exists here, and a guess would be worse than the gap:
        // see the module docs on why `None` is not "dead".
        let _ = pid;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one pid whose liveness is not in doubt.
    #[cfg(any(unix, windows))]
    #[test]
    fn self_is_alive() {
        assert_eq!(
            pid_liveness(std::process::id()),
            Some(true),
            "the running process must probe as alive"
        );
    }

    /// A pid far above any system's maximum is not running. (On Windows pids are
    /// multiples of 4, so this one cannot be valid there either.)
    #[cfg(any(unix, windows))]
    #[test]
    fn unused_pid_is_provably_dead() {
        assert_eq!(
            pid_liveness(999_999_999),
            Some(false),
            "an unassigned pid must probe as dead, not as unknown"
        );
    }

    /// pid 0 is guarded before any probe runs: `kill(0, 0)` targets the caller's
    /// whole process group and succeeds, and Windows resolves 0 to the System
    /// Idle Process — so without the guard a corrupt record's pid of 0 would
    /// report as a live owner forever.
    #[test]
    fn pid_zero_is_never_alive() {
        assert_eq!(
            pid_liveness(0),
            Some(false),
            "pid 0 must never report as a live owner"
        );
    }

    /// Off unix and Windows the answer is "cannot tell" — never `Some(false)`,
    /// which callers are entitled to act on as "provably dead".
    #[cfg(not(any(unix, windows)))]
    #[test]
    fn unprobeable_platform_reports_unknown() {
        assert_eq!(pid_liveness(std::process::id()), None);
        assert_eq!(pid_liveness(999_999_999), None);
    }
}
