//! Hand-rolled single-value async primitive — zero new deps.
//!
//! `Oneshot<T>` allows a background thread to resolve a `Future` that lives on
//! the async executor side. No tokio / async-std required.

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
            SlotState::Taken => panic!("oneshot polled after completion"),
            _ => {
                *guard = SlotState::Waiting(cx.waker().clone());
                std::task::Poll::Pending
            }
        }
    }
}
