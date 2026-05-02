//! Hand-rolled single-value async primitive — zero new deps.
//!
//! `Oneshot<T>` allows a background thread to resolve a `Future` that lives on
//! the async executor side. No tokio / async-std required.

// Used by X11/Wayland backends on Linux; macOS/Windows backend dispatch is
// direct so Oneshot is dead on those targets at the module level.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::task::Waker;

enum SlotState<T> {
    Empty,
    Waiting(Waker),
    Ready(T),
    Taken,
}

/// A single-producer single-consumer async channel that holds one value.
pub(crate) struct Oneshot<T> {
    state: Mutex<SlotState<T>>,
}

impl<T> Oneshot<T> {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SlotState::Empty),
        })
    }

    /// Called by the background thread to resolve the future.
    pub(crate) fn resolve(self: &Arc<Self>, value: T) {
        let mut guard = self.state.lock().expect("oneshot mutex poisoned");
        let old = std::mem::replace(&mut *guard, SlotState::Ready(value));
        if let SlotState::Waiting(waker) = old {
            drop(guard);
            waker.wake();
        }
    }

    /// Poll the oneshot from within a `Future::poll` impl.
    pub(crate) fn poll(self: &Arc<Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<T> {
        let mut guard = self.state.lock().expect("oneshot mutex poisoned");
        match &*guard {
            SlotState::Ready(_) => {
                let SlotState::Ready(value) = std::mem::replace(&mut *guard, SlotState::Taken)
                else {
                    unreachable!()
                };
                std::task::Poll::Ready(value)
            }
            SlotState::Taken => {
                if cfg!(debug_assertions) {
                    panic!("oneshot polled after completion");
                }
                std::task::Poll::Pending
            }
            _ => {
                *guard = SlotState::Waiting(cx.waker().clone());
                std::task::Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake};

    // ---------------------------------------------------------------------------
    // Minimal waker helpers
    // ---------------------------------------------------------------------------

    struct NoopWaker;
    impl Wake for NoopWaker {
        fn wake(self: Arc<Self>) {}
    }

    fn noop_cx() -> (Arc<NoopWaker>, std::task::Waker) {
        let w = Arc::new(NoopWaker);
        let waker = std::task::Waker::from(w.clone());
        (w, waker)
    }

    /// A waker that unparks a specific thread when woken.
    struct UnparkWaker(std::thread::Thread);
    impl Wake for UnparkWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    /// Resolve before poll: value lands, first poll returns `Ready`.
    #[test]
    fn resolve_before_poll() {
        let os = Oneshot::new();
        os.resolve(42u32);

        let (_, waker) = noop_cx();
        let mut cx = Context::from_waker(&waker);
        assert_eq!(os.poll(&mut cx), Poll::Ready(42u32));
    }

    /// Poll before resolve: first poll returns `Pending`, resolve wakes waker,
    /// second poll returns `Ready`.
    #[test]
    fn poll_before_resolve() {
        let os = Oneshot::new();

        let (_, waker) = noop_cx();
        let mut cx = Context::from_waker(&waker);

        // First poll — nothing ready yet.
        assert_eq!(os.poll(&mut cx), Poll::Pending);

        // Resolve from "background".
        os.resolve(99u32);

        // Second poll — now ready.
        assert_eq!(os.poll(&mut cx), Poll::Ready(99u32));
    }

    /// Multiple polls before resolve: only latest waker stored, no double-wake.
    #[test]
    fn multiple_polls_before_resolve() {
        let os = Oneshot::new();

        let (_, w1) = noop_cx();
        let mut cx1 = Context::from_waker(&w1);
        assert_eq!(os.poll(&mut cx1), Poll::Pending);

        // Second poll overwrites waker.
        let (_, w2) = noop_cx();
        let mut cx2 = Context::from_waker(&w2);
        assert_eq!(os.poll(&mut cx2), Poll::Pending);

        // Resolve; only the latest waker should fire (both are no-ops here,
        // so we just verify no panic and that the value is returned correctly).
        os.resolve(7u32);

        let (_, w3) = noop_cx();
        let mut cx3 = Context::from_waker(&w3);
        assert_eq!(os.poll(&mut cx3), Poll::Ready(7u32));
    }

    /// Panic on poll-after-completion.
    #[test]
    #[should_panic(expected = "oneshot polled after completion")]
    fn panic_on_poll_after_taken() {
        let os = Oneshot::new();
        os.resolve(1u32);

        let (_, waker) = noop_cx();
        let mut cx = Context::from_waker(&waker);

        // Consume the value.
        assert_eq!(os.poll(&mut cx), Poll::Ready(1u32));

        // Poll again — must panic.
        let _ = os.poll(&mut cx);
    }

    /// Concurrent resolve from another thread + poll on main thread.
    #[test]
    fn concurrent_resolve_and_poll() {
        let os = Oneshot::new();
        let os2 = Arc::clone(&os);

        let main_thread = std::thread::current();
        let unpark_waker = Arc::new(UnparkWaker(main_thread));
        let waker = std::task::Waker::from(unpark_waker);
        let mut cx = Context::from_waker(&waker);

        // First poll — registers waker.
        assert_eq!(os.poll(&mut cx), Poll::Pending);

        // Spawn a thread that resolves after a short wait.
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            os2.resolve(123u32);
        });

        // Park until woken by the unpark waker.
        loop {
            match os.poll(&mut cx) {
                Poll::Ready(v) => {
                    assert_eq!(v, 123u32);
                    break;
                }
                Poll::Pending => std::thread::park(),
            }
        }

        handle.join().unwrap();
    }

    /// Drop without resolving: no panic, no leak.
    #[test]
    fn drop_without_resolve() {
        let os = Oneshot::<u32>::new();
        drop(os);
        // Test passes if we reach here.
    }
}
