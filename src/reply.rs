//! [`Reply<T>`] — unified sync/async reply target for background thread ops.

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
