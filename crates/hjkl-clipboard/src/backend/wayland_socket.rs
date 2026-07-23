//! Unix-socket wrapper for the Wayland wire protocol.
//!
//! Handles `sendmsg`/`recvmsg` for passing file descriptors via SCM_RIGHTS
//! ancillary data and buffering incoming message bytes + fds.
//!
//! No libwayland-client — only libc + std.

use std::collections::VecDeque;
use std::ffi::c_int;

use crate::ClipboardError;

use super::wayland_wire::{MessageHeader, parse_message_header};

// ---------------------------------------------------------------------------
// Socket wrapper
// ---------------------------------------------------------------------------

/// A connected Wayland Unix socket with SCM_RIGHTS fd-passing support.
pub(crate) struct WaylandSocket {
    fd: c_int,
    /// Accumulated received bytes (may contain many partial or complete messages).
    rx_buf: VecDeque<u8>,
    /// File descriptors received via SCM_RIGHTS.
    rx_fds: VecDeque<c_int>,
}

impl WaylandSocket {
    /// Connect to the Wayland compositor socket.
    ///
    /// Resolution order:
    /// 1. `$WAYLAND_DISPLAY` — if it starts with `/`, use as-is (absolute path).
    ///    Otherwise, prepend `$XDG_RUNTIME_DIR` (default `/run/user/<uid>`).
    /// 2. Fallback: `$XDG_RUNTIME_DIR/wayland-0`.
    pub(crate) fn connect() -> Result<Self, ClipboardError> {
        let socket_path = wayland_socket_path()?;
        connect_to_path(&socket_path)
    }

    /// Send `bytes` on the socket, attaching `fds` via SCM_RIGHTS.
    ///
    /// If `fds` is empty, no ancillary data is sent.
    pub(crate) fn send(&self, bytes: &[u8], fds: &[c_int]) -> Result<(), ClipboardError> {
        if bytes.is_empty() {
            return Ok(());
        }

        if fds.is_empty() {
            send_plain(self.fd, bytes)
        } else {
            send_with_fds(self.fd, bytes, fds)
        }
    }

    /// Read available data from the socket into the internal buffer.
    ///
    /// `blocking = true` blocks until at least one byte arrives.
    /// `blocking = false` sets `O_NONBLOCK` behaviour: returns `Ok(())` even
    /// if no data is available yet (the caller should check `next_message`).
    pub(crate) fn recv(&mut self, blocking: bool) -> Result<(), ClipboardError> {
        recv_into(self.fd, &mut self.rx_buf, &mut self.rx_fds, blocking)
    }

    /// Try to extract the next complete message from the receive buffer.
    ///
    /// Returns `None` if fewer than 8 header bytes, or if the advertised
    /// message size has not yet been fully received.
    pub(crate) fn next_message(&mut self) -> Option<(MessageHeader, Vec<u8>)> {
        // Peek at a contiguous slice — VecDeque may be non-contiguous so we
        // need to make_contiguous first.
        let contiguous = self.rx_buf.make_contiguous();

        let (hdr, _) = parse_message_header(contiguous)?;
        let total = hdr.size as usize;
        // A well-formed Wayland message is at least its 8-byte header. A smaller
        // advertised size is malformed (buggy or hostile compositor / corrupted
        // stream); slicing `msg_bytes[8..]` would then panic. Drop the 8 header
        // bytes we peeked to make forward progress instead of trusting `total`.
        if total < 8 {
            let drop = 8.min(self.rx_buf.len());
            self.rx_buf.drain(..drop);
            return None;
        }
        if self.rx_buf.len() < total {
            return None;
        }

        // Drain `total` bytes from the front of the deque.
        let msg_bytes: Vec<u8> = self.rx_buf.drain(..total).collect();
        // Args start after the 8-byte header.
        let args = msg_bytes[8..].to_vec();
        Some((hdr, args))
    }

    /// Take the oldest fd received via SCM_RIGHTS.
    pub(crate) fn next_fd(&mut self) -> Option<c_int> {
        self.rx_fds.pop_front()
    }

    /// The raw socket file descriptor (for use with `poll`/`epoll` in 6b).
    pub(crate) fn raw_fd(&self) -> c_int {
        self.fd
    }

    /// Construct a `WaylandSocket` from a raw file descriptor.
    ///
    /// # Safety
    ///
    /// The caller must transfer ownership of `fd` — it must be a valid open
    /// socket fd that will not be closed by any other code path. The
    /// `WaylandSocket` `Drop` impl will close it.
    #[cfg(test)]
    pub(crate) unsafe fn from_raw_fd(fd: c_int) -> Self {
        WaylandSocket {
            fd,
            rx_buf: std::collections::VecDeque::new(),
            rx_fds: std::collections::VecDeque::new(),
        }
    }
}

impl Drop for WaylandSocket {
    fn drop(&mut self) {
        // Close any fds received via SCM_RIGHTS that were never consumed —
        // they are owned by us once recvmsg delivers them and would otherwise
        // leak.
        for fd in self.rx_fds.drain(..) {
            // SAFETY: fd was received via SCM_RIGHTS and is owned by us.
            unsafe { libc::close(fd) };
        }
        // SAFETY: fd was opened by us via libc::socket; close it exactly once.
        unsafe { libc::close(self.fd) };
    }
}

// ---------------------------------------------------------------------------
// Socket path resolution
// ---------------------------------------------------------------------------

fn wayland_socket_path() -> Result<String, ClipboardError> {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_owned());

    if display.starts_with('/') {
        return Ok(display);
    }

    // Relative name — prepend XDG_RUNTIME_DIR.
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        // SAFETY: getuid() is always safe to call; no threads are reading this
        // value concurrently in the normal early-init path.
        let uid = unsafe { libc::getuid() };
        format!("/run/user/{uid}")
    });

    Ok(format!("{runtime_dir}/{display}"))
}

// ---------------------------------------------------------------------------
// connect_to_path
// ---------------------------------------------------------------------------

fn connect_to_path(path: &str) -> Result<WaylandSocket, ClipboardError> {
    if path.len() >= 108 {
        // UNIX_PATH_MAX is 108 bytes including NUL on Linux.
        return Err(ClipboardError::io_other("Wayland socket path too long"));
    }

    // SAFETY: socket(2) is always safe with valid constant arguments.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(ClipboardError::io(std::io::Error::last_os_error()));
    }

    // Build sockaddr_un.
    // SAFETY: zeroed bytes are a valid initial state for sockaddr_un.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;

    // Copy path into sun_path; we already checked len < 108.
    let path_bytes = path.as_bytes();
    // SAFETY: addr.sun_path is [i8; 108]; we copy at most 107 bytes and the
    // rest remains zeroed (NUL-terminated).
    unsafe {
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr() as *const libc::c_char,
            addr.sun_path.as_mut_ptr(),
            path_bytes.len(),
        );
    }

    let addr_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1)
        as libc::socklen_t;

    // SAFETY: fd is valid; addr is fully initialised above.
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        )
    };
    if rc != 0 {
        // SAFETY: fd is valid; we own it.
        unsafe { libc::close(fd) };
        return Err(ClipboardError::NoDisplay);
    }

    Ok(WaylandSocket {
        fd,
        rx_buf: VecDeque::new(),
        rx_fds: VecDeque::new(),
    })
}

// ---------------------------------------------------------------------------
// Plain send (no fds)
// ---------------------------------------------------------------------------

fn send_plain(fd: c_int, bytes: &[u8]) -> Result<(), ClipboardError> {
    let mut sent = 0;
    while sent < bytes.len() {
        // SAFETY: fd is valid; slice pointer and length are from a Rust slice.
        let n = unsafe {
            libc::send(
                fd,
                bytes[sent..].as_ptr() as *const libc::c_void,
                bytes.len() - sent,
                libc::MSG_NOSIGNAL,
            )
        };
        if n < 0 {
            return Err(ClipboardError::io(std::io::Error::last_os_error()));
        }
        sent += n as usize;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// sendmsg with SCM_RIGHTS
// ---------------------------------------------------------------------------

fn send_with_fds(fd: c_int, bytes: &[u8], fds: &[c_int]) -> Result<(), ClipboardError> {
    // Compute control-message buffer size.
    // SAFETY: CMSG_SPACE is a pure arithmetic macro; always safe.
    let cmsg_space =
        unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as libc::c_uint) } as usize;

    let mut cmsg_buf = vec![0u8; cmsg_space];

    // Defensive: verify fds truly fits in the allocated CMSG buffer.
    // SAFETY: CMSG_LEN is pure arithmetic.
    let hdr_size = unsafe { libc::CMSG_LEN(0) } as usize;
    let fds_bytes = std::mem::size_of_val(fds);
    if fds_bytes + hdr_size > cmsg_space {
        return Err(ClipboardError::io_other("too many fds for CMSG buffer"));
    }

    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut libc::c_void,
        iov_len: bytes.len(),
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    // msg_controllen is size_t (usize) on glibc but socklen_t (u32) on musl.
    // try_into() infers the correct type per target; suppress the useless-
    // conversion lint that fires on glibc where both sides are usize.
    #[allow(clippy::useless_conversion)]
    {
        msg.msg_controllen = cmsg_space
            .try_into()
            .expect("cmsg_space fits in msg_controllen");
    }

    // Fill in the cmsghdr.
    // SAFETY: msg_control points to a buffer of size msg_controllen;
    // CMSG_FIRSTHDR returns a pointer into that buffer.
    let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    if cmsg.is_null() {
        return Err(ClipboardError::io_other("CMSG_FIRSTHDR returned null"));
    }

    // SAFETY: cmsg is non-null and points inside cmsg_buf.
    unsafe {
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as libc::c_uint) as _;

        // Copy fd integers into the CMSG data region.
        // SAFETY: CMSG_DATA returns a pointer into cmsg's data region which is
        // large enough for `fds.len()` c_int values per CMSG_SPACE above and
        // the defensive fds_bytes + hdr_size check verified earlier.
        let data_ptr = libc::CMSG_DATA(cmsg) as *mut c_int;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), data_ptr, fds.len());
    }

    // SAFETY: fd valid; msg is fully initialised.
    let n = unsafe { libc::sendmsg(fd, &msg, libc::MSG_NOSIGNAL) };
    if n < 0 {
        return Err(ClipboardError::io(std::io::Error::last_os_error()));
    }

    // The SCM_RIGHTS control message (the fds) is carried only by this first
    // `sendmsg`. If the data was written partially, send the remaining bytes
    // as plain data (no cmsg — the fds are already transferred). Skipping this
    // would truncate the wire message and desync the wayland protocol. Wayland
    // messages are small so a partial write is rare, but not impossible under
    // socket-buffer pressure.
    let sent = n as usize;
    if sent < bytes.len() {
        send_plain(fd, &bytes[sent..])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// recvmsg — fills rx_buf and rx_fds
// ---------------------------------------------------------------------------

// Maximum fds we expect to receive in one recvmsg call.
const MAX_FDS_PER_RECV: usize = 8;
// Read buffer size per recvmsg call.
const RECV_BUF_SIZE: usize = 4096;

fn recv_into(
    fd: c_int,
    rx_buf: &mut VecDeque<u8>,
    rx_fds: &mut VecDeque<c_int>,
    blocking: bool,
) -> Result<(), ClipboardError> {
    let mut data_buf = [0u8; RECV_BUF_SIZE];

    // SAFETY: CMSG_SPACE is pure arithmetic.
    let cmsg_space = unsafe {
        libc::CMSG_SPACE((MAX_FDS_PER_RECV * std::mem::size_of::<c_int>()) as libc::c_uint)
    } as usize;
    let mut cmsg_buf = vec![0u8; cmsg_space];

    let mut iov = libc::iovec {
        iov_base: data_buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: data_buf.len(),
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    // msg_controllen is size_t (usize) on glibc but socklen_t (u32) on musl.
    // try_into() infers the correct type per target; suppress the useless-
    // conversion lint that fires on glibc where both sides are usize.
    #[allow(clippy::useless_conversion)]
    {
        msg.msg_controllen = cmsg_space
            .try_into()
            .expect("cmsg_space fits in msg_controllen");
    }

    let flags = if blocking { 0 } else { libc::MSG_DONTWAIT };

    // SAFETY: fd is valid; msg is fully initialised above.
    let n = unsafe { libc::recvmsg(fd, &mut msg, flags) };

    if n < 0 {
        let err = std::io::Error::last_os_error();
        if !blocking
            && (err.raw_os_error() == Some(libc::EAGAIN)
                || err.raw_os_error() == Some(libc::EWOULDBLOCK))
        {
            // No data yet; non-blocking, not an error.
            return Ok(());
        }
        return Err(ClipboardError::io(err));
    }

    if n == 0 {
        return Err(ClipboardError::io_other("Wayland socket closed"));
    }

    // Append received bytes.
    rx_buf.extend(&data_buf[..n as usize]);

    // Extract any received fds from ancillary data.
    // Kernel updates msg_controllen to actual control-data bytes received.
    // Use this as the authoritative bound for the cmsg walk + fd extraction.
    let control_len = msg.msg_controllen as usize;
    // SAFETY: msg is valid after recvmsg; CMSG_FIRSTHDR reads msg_control.
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg.is_null() {
        // SAFETY: cmsg is non-null and within cmsg_buf.
        let level = unsafe { (*cmsg).cmsg_level };
        let typ = unsafe { (*cmsg).cmsg_type };
        if level == libc::SOL_SOCKET && typ == libc::SCM_RIGHTS {
            // SAFETY: cmsg is valid; CMSG_DATA points into cmsg's data region.
            let data = unsafe { libc::CMSG_DATA(cmsg) };
            let cmsg_len = unsafe { (*cmsg).cmsg_len } as usize;
            // SAFETY: CMSG_LEN(0) gives the header size.
            let hdr_size = unsafe { libc::CMSG_LEN(0) } as usize;
            // Reject a malformed cmsg that claims to be smaller than its own header.
            if cmsg_len < hdr_size {
                // Skip this cmsg — malicious or corrupt data.
                cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
                continue;
            }
            let data_len = cmsg_len - hdr_size;
            let n_fds = data_len / std::mem::size_of::<c_int>();
            if n_fds == 0 {
                cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
                continue;
            }
            // Bounds-check: the fd data must fit within the control-message
            // buffer the kernel filled. A malicious compositor or kernel bug
            // could set cmsg_len larger than the actual buffer.
            let data_end = (data as *const c_int).wrapping_add(n_fds);
            let buf_end = unsafe { cmsg_buf.as_ptr().add(control_len) } as *const c_int;
            if data_end > buf_end {
                // Corrupt cmsg — skip.
                cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
                continue;
            }
            for i in 0..n_fds {
                // SAFETY: data_end <= buf_end verified above; each add(i)
                // for i < n_fds stays within [data, data_end).
                let received_fd = unsafe { *(data as *const c_int).add(i) };
                rx_fds.push_back(received_fd);
            }
        }
        // SAFETY: msg and cmsg are both valid; CMSG_NXTHDR walks the chain.
        cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
    }

    Ok(())
}
