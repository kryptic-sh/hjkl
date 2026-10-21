//! A pipe that wakes a background thread's poll(2) when a request is queued
//! for it, so the thread can wait with no timeout instead of polling its
//! inbox: an idle process holding the clipboard does not wake.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use crate::ClipboardError;

/// The write end, kept by the thread's handle.
pub struct Waker(OwnedFd);

/// The read end, polled by the thread beside its connection.
pub struct WakeReader(OwnedFd);

/// A non-blocking wake pipe.
pub fn pipe() -> Result<(Waker, WakeReader), ClipboardError> {
    let mut fds = [0i32; 2];
    // SAFETY: pipe2 is safe to call with a valid [i32;2] and valid flags.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    if rc != 0 {
        return Err(ClipboardError::io(std::io::Error::last_os_error()));
    }
    // SAFETY: pipe2 just opened both fds, and nothing else owns them.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Ok((Waker(write), WakeReader(read)))
}

impl Waker {
    /// Wake the thread for a request just queued. A full pipe already holds
    /// a wakeup the thread has yet to read, so EAGAIN needs nothing more.
    pub fn wake(&self, backend: &str) {
        // SAFETY: writes one byte from a live buffer to an fd this owns.
        let rc = unsafe { libc::write(self.0.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::WouldBlock {
                eprintln!("hjkl-clipboard {backend}: wake failed: {e}");
            }
        }
    }
}

impl WakeReader {
    pub fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    /// Empty the pipe once poll(2) said it was readable; the inbox is drained
    /// after.
    pub fn drain(&self) {
        let mut buf = [0u8; 64];
        // SAFETY: reads into a live buffer of its own length from an fd this
        // owns; non-blocking, so it stops at EAGAIN once empty.
        while unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
    }
}
