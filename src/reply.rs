//! [`Reply<T>`] — unified sync/async reply target for background thread ops.

// Used by X11/Wayland backends on Linux; macOS/Windows dispatch is direct.
#![allow(dead_code)]

use std::sync::{Arc, Condvar, Mutex};

use crate::oneshot::Oneshot;

/// Reply target passed alongside a request to the background thread.
///
/// The bg thread calls `resolve` regardless of whether the caller is sync or
/// async. The front-door wrapper (`block_on` for sync, `Future` for async)
/// extracts the result via the appropriate mechanism.
pub(crate) enum Reply<T> {
    /// Sync caller: wakes via condvar.
    Sync(Arc<(Mutex<Option<T>>, Condvar)>),
    /// Async caller: wakes the executor via `Waker`.
    Async(Arc<Oneshot<T>>),
}

impl<T> Reply<T> {
    pub(crate) fn resolve(self, value: T) {
        match self {
            Self::Sync(pair) => {
                let (lock, cvar) = &*pair;
                *lock.lock().expect("reply mutex poisoned") = Some(value);
                cvar.notify_one();
            }
            Self::Async(oneshot) => {
                oneshot.resolve(value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake};

    struct NoopWaker;
    impl Wake for NoopWaker {
        fn wake(self: Arc<Self>) {}
    }

    /// `Sync` variant: condvar wakeup delivers value to a waiting thread.
    #[test]
    fn sync_variant_delivers_value() {
        let pair = Arc::new((Mutex::new(None::<u32>), Condvar::new()));
        let reply = Reply::Sync(Arc::clone(&pair));

        let pair2 = Arc::clone(&pair);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            reply.resolve(55u32);
        });

        let (lock, cvar) = &*pair2;
        let mut guard = lock.lock().unwrap();
        while guard.is_none() {
            guard = cvar.wait(guard).unwrap();
        }
        assert_eq!(*guard, Some(55u32));
        handle.join().unwrap();
    }

    /// `Async` variant: forwards value to the Oneshot correctly.
    #[test]
    fn async_variant_forwards_to_oneshot() {
        let os = Oneshot::new();
        let reply = Reply::Async(Arc::clone(&os));

        reply.resolve(77u32);

        let waker = std::task::Waker::from(Arc::new(NoopWaker));
        let mut cx = Context::from_waker(&waker);
        assert_eq!(os.poll(&mut cx), Poll::Ready(77u32));
    }

    /// Both variants are `Send`-safe: send the Reply to a worker thread.
    #[test]
    fn both_variants_are_send() {
        // Sync variant.
        let pair = Arc::new((Mutex::new(None::<u32>), Condvar::new()));
        let reply_s: Reply<u32> = Reply::Sync(Arc::clone(&pair));
        let h1 = std::thread::spawn(move || {
            reply_s.resolve(1);
        });
        h1.join().unwrap();

        // Async variant.
        let os = Oneshot::<u32>::new();
        let reply_a: Reply<u32> = Reply::Async(Arc::clone(&os));
        let h2 = std::thread::spawn(move || {
            reply_a.resolve(2);
        });
        h2.join().unwrap();
    }
}
