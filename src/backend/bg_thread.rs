//! Singleton background thread for Linux backends (X11 + Wayland).
//!
//! Spawned lazily on first clipboard operation, lives until process exit.
//! Accepts `Request` messages and dispatches to the active backend.
//! The bg thread keeps selections alive independently of `Clipboard` handle
//! lifetimes — drop of last handle does NOT kill the thread.

use std::sync::OnceLock;
use std::sync::mpsc;

use crate::error::ClipboardError;
use crate::oneshot::Oneshot;
use crate::reply::Reply;

// ---------------------------------------------------------------------------
// Op — operations the bg thread can execute
// ---------------------------------------------------------------------------

/// Operations that can be dispatched to the background thread.
///
/// Phase 1: only `Echo` for end-to-end roundtrip testing.
/// Phase 5/6 will add `Set`, `Get`, `Clear`, `Available`.
pub(crate) enum Op {
    /// No-op test operation: the bg thread echoes the string back.
    Echo(String),
}

// ---------------------------------------------------------------------------
// Request / inbox types
// ---------------------------------------------------------------------------

/// A message sent to the bg thread inbox.
pub(crate) struct Request {
    pub(crate) op: Op,
    pub(crate) reply: Reply<Result<String, ClipboardError>>,
}

// ---------------------------------------------------------------------------
// BgThread
// ---------------------------------------------------------------------------

/// Handle to the singleton background thread.
pub(crate) struct BgThread {
    tx: mpsc::Sender<Request>,
}

impl BgThread {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Request>();

        std::thread::Builder::new()
            .name("hjkl-clipboard-bg".into())
            .spawn(move || {
                for req in rx {
                    let result = dispatch(req.op);
                    req.reply.resolve(result);
                }
            })
            .expect("failed to spawn clipboard bg thread");

        Self { tx }
    }

    /// Enqueue an op and block on the condvar until the reply arrives.
    pub(crate) fn send_sync(&self, op: Op) -> Result<String, ClipboardError> {
        use std::sync::{Arc, Condvar, Mutex};

        let pair = Arc::new((
            Mutex::new(None::<Result<String, ClipboardError>>),
            Condvar::new(),
        ));
        let reply = Reply::Sync(Arc::clone(&pair));

        self.tx
            .send(Request { op, reply })
            .expect("bg thread inbox closed");

        let (lock, cvar) = &*pair;
        let mut guard = lock.lock().unwrap();
        while guard.is_none() {
            guard = cvar.wait(guard).unwrap();
        }
        guard.take().unwrap()
    }

    /// Enqueue an op and return a `Future` that resolves when the bg thread replies.
    pub(crate) fn send_async(&self, op: Op) -> OneshotFuture {
        let oneshot = Oneshot::new();
        let reply = Reply::Async(std::sync::Arc::clone(&oneshot));

        self.tx
            .send(Request { op, reply })
            .expect("bg thread inbox closed");

        OneshotFuture { oneshot }
    }
}

// ---------------------------------------------------------------------------
// OneshotFuture — wraps Oneshot<Result<String, ClipboardError>> as a Future
// ---------------------------------------------------------------------------

/// Future returned by [`BgThread::send_async`].
pub(crate) struct OneshotFuture {
    oneshot: std::sync::Arc<Oneshot<Result<String, ClipboardError>>>,
}

impl std::future::Future for OneshotFuture {
    type Output = Result<String, ClipboardError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.oneshot.poll(cx)
    }
}

// ---------------------------------------------------------------------------
// Singleton accessor
// ---------------------------------------------------------------------------

static BG_THREAD: OnceLock<BgThread> = OnceLock::new();

/// Returns the process-global singleton background thread, spawning it lazily
/// on first call.
pub(crate) fn bg_thread() -> &'static BgThread {
    BG_THREAD.get_or_init(BgThread::new)
}

// ---------------------------------------------------------------------------
// Internal dispatch
// ---------------------------------------------------------------------------

fn dispatch(op: Op) -> Result<String, ClipboardError> {
    match op {
        Op::Echo(s) => Ok(s),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake};

    // -----------------------------------------------------------------------
    // Tiny no-op executor for async tests (no `futures` dep)
    // -----------------------------------------------------------------------

    struct NoopWaker;
    impl Wake for NoopWaker {
        fn wake(self: Arc<Self>) {}
    }

    /// Park-loop executor. Works because the bg thread's `reply.resolve()` call
    /// invokes the real waker stored in the Oneshot, which unparks this thread.
    fn block_on<F: std::future::Future>(mut fut: F) -> F::Output {
        // Use an unpark waker so the bg thread actually wakes us.
        let current = std::thread::current();
        let waker = std::task::Waker::from(Arc::new(UnparkWaker(current)));
        let mut cx = Context::from_waker(&waker);
        // SAFETY: we never move `fut` after pinning.
        let mut fut = unsafe { std::pin::Pin::new_unchecked(&mut fut) };
        loop {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(1));
        }
    }

    struct UnparkWaker(std::thread::Thread);
    impl Wake for UnparkWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    // -----------------------------------------------------------------------

    /// Sync roundtrip: `send_sync(Echo("hello"))` returns `Ok("hello")`.
    #[test]
    fn echo_sync() {
        let result = bg_thread().send_sync(Op::Echo("hello".into()));
        assert_eq!(result.unwrap(), "hello");
    }

    /// Async roundtrip: `send_async(Echo("world")).await` returns `Ok("world")`.
    #[test]
    fn echo_async() {
        let fut = bg_thread().send_async(Op::Echo("world".into()));
        let result = block_on(fut);
        assert_eq!(result.unwrap(), "world");
    }

    /// Multiple sequential requests work — singleton thread stays alive.
    #[test]
    fn sequential_requests() {
        let bt = bg_thread();
        for i in 0..5u32 {
            let s = i.to_string();
            let result = bt.send_sync(Op::Echo(s.clone()));
            assert_eq!(result.unwrap(), s);
        }
    }

    /// Concurrent burst: 10 sync requests from different threads, all correct.
    #[test]
    fn concurrent_sync_burst() {
        let handles: Vec<_> = (0..10u32)
            .map(|i| {
                std::thread::spawn(move || {
                    let s = format!("thread-{i}");
                    let result = bg_thread().send_sync(Op::Echo(s.clone()));
                    assert_eq!(result.unwrap(), s);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }
}
