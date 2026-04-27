//! Submit handler types.

/// Form submit closure. Consumed via `Option::take` on first call.
pub type SubmitFn = Box<dyn FnOnce() -> SubmitOutcome + Send>;

/// Outcome returned by [`SubmitFn`]. Surfaced through `FormEvent::Submitted`.
#[derive(Debug, Clone)]
pub enum SubmitOutcome {
    Ok,
    Err(String),
}
