//! Async wrapper around `GrammarLoader` for off-thread clone+compile.
//!
//! Consumers that can't block (TUI/GUI main loops) wrap a `GrammarLoader`
//! in `AsyncGrammarLoader` and call `load_async` to get a `LoadHandle` they
//! poll each tick / await.
//!
//! ⚠️ **Security:** this is only a threading wrapper — it performs the exact
//! same **download + compile + `dlopen`** of remote grammar code as the
//! synchronous [`GrammarLoader::load`], just on a worker thread. All the trust
//! caveats in the crate-root and `loader` docs apply unchanged.
//!
//! ## When to use vs sync
//!
//! Use `AsyncGrammarLoader` when:
//! - You're on an event loop (ratatui tick, wgpu frame, etc.) that must not
//!   block for 1–3 s on a grammar clone+compile.
//! - You want dedup: multiple callers requesting the same grammar name share
//!   one in-flight job automatically.
//!
//! Use `GrammarLoader::load` directly when:
//! - You're in a blocking context (xtask, test, CLI one-shot) and a sync
//!   call is fine.
//! - You've already checked `lookup_fresh` and know the grammar is cached.
//!
//! ## Threading model
//!
//! A fixed pool of 2 worker threads is spawned at construction time and lives
//! until `AsyncGrammarLoader` is dropped. 2 threads is intentional: clone +
//! compile is heavy CPU + I/O, and more threads mostly hurt via I/O contention
//! on the grammar source dirs. Each worker shares the `Arc<GrammarLoader>` and
//! the in-flight dedup map.
//!
//! Job dispatch uses the classic mpsc-as-pool pattern: a single
//! `Sender<Job>` is cloned into every worker; each worker races on the shared
//! `Arc<Mutex<Receiver<Job>>>`.
//!
//! ## Dedup semantics
//!
//! First `load_async("rust", …)` → inserts `("rust", vec![tx1])` + enqueues job.
//! Second `load_async("rust", …)` while first is in-flight → pushes `tx2` into
//! the existing vec, does **not** re-enqueue. Worker finishes → drains the vec,
//! broadcasts result to all subscribers, removes the entry.
//!
//! ## Error semantics
//!
//! `anyhow::Error` is not `Clone`. Worker converts errors to `LoadError::Failed(String)`
//! at the channel boundary using `format!("{e:#}")` to preserve the cause chain.
//! Both success and failure are broadcast to all in-flight subscribers.
//!
//! ## Cancellation
//!
//! Dropping a `LoadHandle` before it resolves is safe. The worker still
//! completes the job (clone+compile artifacts are cached for future callers).
//! The send into a dropped channel fails silently. If all handles for a name
//! are dropped, the worker logs a debug message and continues.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use tracing::debug;

use super::loader::GrammarLoader;
use super::manifest::{LangSpec, ManifestMeta};

/// Error type for async grammar loads.
///
/// Unlike `anyhow::Error`, this is `Clone` so it can be broadcast to multiple
/// waiting `LoadHandle`s. The original error chain is captured as a `String`
/// via `format!("{:#}")` at the worker→channel boundary.
#[derive(Clone, Debug, thiserror::Error)]
pub enum LoadError {
    /// The underlying `GrammarLoader::load` returned an error.
    #[error("grammar load failed: {0}")]
    Failed(String),
    /// The worker pool was unavailable before the job could be dispatched.
    #[error("grammar load dispatch failed: worker pool is unavailable")]
    DispatchFailed,
    /// The `LoadHandle` channel was dropped before the worker completed.
    /// This variant is produced internally if the send itself fails; callers
    /// generally won't see it (drop = no receiver = no one to observe it).
    #[error("load handle dropped before completion")]
    Cancelled,
}

// ── internal job type ────────────────────────────────────────────────────────

struct Job {
    name: String,
    spec: LangSpec,
    meta: ManifestMeta,
}

// ── in-flight dedup map ───────────────────────────────────────────────────────

type InFlight = Arc<Mutex<HashMap<String, Vec<Sender<Result<PathBuf, LoadError>>>>>>;

// ── public API ────────────────────────────────────────────────────────────────

/// Off-thread grammar loader with automatic dedup of concurrent requests.
///
/// Construct once (e.g. alongside your `LanguageDirectory`) and share via
/// `Arc<AsyncGrammarLoader>`. Cheap to clone — inner state is `Arc`-wrapped.
pub struct AsyncGrammarLoader {
    inner: Arc<GrammarLoader>,
    in_flight: InFlight,
    job_tx: Sender<Job>,
    // Kept alive so workers don't exit until this is dropped.
    _worker_handles: Arc<[thread::JoinHandle<()>]>,
}

impl AsyncGrammarLoader {
    /// Wrap an existing `GrammarLoader`. Spawns 2 worker threads immediately.
    pub fn new(loader: GrammarLoader) -> Self {
        let inner = Arc::new(loader);
        let in_flight: InFlight = Arc::new(Mutex::new(HashMap::new()));

        // Classic mpsc-as-pool: one Sender, shared Receiver behind a Mutex.
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let shared_rx = Arc::new(Mutex::new(job_rx));

        let mut handles = Vec::with_capacity(2);
        for _ in 0..2 {
            let loader_clone = Arc::clone(&inner);
            let in_flight_clone = Arc::clone(&in_flight);
            let rx_clone = Arc::clone(&shared_rx);

            let handle = thread::Builder::new()
                .name("hjkl-bonsai-grammar-loader".into())
                .spawn(move || worker_loop(loader_clone, in_flight_clone, rx_clone))
                .expect("spawn grammar loader worker");
            handles.push(handle);
        }

        Self {
            inner,
            in_flight,
            job_tx,
            _worker_handles: handles.into(),
        }
    }

    /// Kick off (or subscribe to) a background load for `name`.
    ///
    /// ⚠️ **Security:** on a cache miss the worker **downloads, compiles, and
    /// `dlopen`s remote grammar code** (same as [`GrammarLoader::load`]). Only
    /// request names whose manifest entry you trust.
    ///
    /// If another caller already requested the same `name` and the job hasn't
    /// completed yet, the returned `LoadHandle` subscribes to the same in-flight
    /// job — no duplicate clone+compile. If `lookup_fresh` would succeed (grammar
    /// already cached), the job is still enqueued but will complete almost
    /// immediately since `GrammarLoader::load` short-circuits.
    pub fn load_async(&self, name: String, spec: LangSpec, meta: ManifestMeta) -> LoadHandle {
        let (tx, rx) = mpsc::channel();

        let mut map = self.in_flight.lock().expect("in_flight mutex poisoned");
        if let Some(senders) = map.get_mut(&name) {
            // Already in-flight — subscribe.
            senders.push(tx);
        } else {
            // First caller — insert and enqueue.
            map.insert(name.clone(), vec![tx]);
            drop(map); // Release lock before sending to avoid potential deadlock.
            if self
                .job_tx
                .send(Job {
                    name: name.clone(),
                    spec,
                    meta,
                })
                .is_err()
            {
                let senders = self
                    .in_flight
                    .lock()
                    .expect("in_flight mutex poisoned")
                    .remove(&name)
                    .unwrap_or_default();
                for sender in senders {
                    let _ = sender.send(Err(LoadError::DispatchFailed));
                }
            }
            return LoadHandle { rx };
        }
        // Drop map lock before returning (borrow ends here for the else branch above,
        // but for the if branch we need an explicit drop — already done via scope).
        LoadHandle { rx }
    }

    /// Access the underlying sync loader (e.g. for `lookup_fresh` cache checks).
    pub fn inner(&self) -> &GrammarLoader {
        &self.inner
    }

    /// Snapshot of grammar names with at least one in-flight load. Order
    /// is unspecified. Used by the renderer to surface a global
    /// "loading grammar(s)" indicator independent of which buffer is
    /// active.
    pub fn in_flight_names(&self) -> Vec<String> {
        let map = self.in_flight.lock().expect("in_flight mutex poisoned");
        map.keys().cloned().collect()
    }
}

// ── worker ────────────────────────────────────────────────────────────────────

fn worker_loop(loader: Arc<GrammarLoader>, in_flight: InFlight, rx: Arc<Mutex<Receiver<Job>>>) {
    loop {
        // Acquire the next job. We hold the lock only long enough to recv.
        let job = {
            let guard = rx.lock().expect("job receiver mutex poisoned");
            match guard.recv() {
                Ok(j) => j,
                Err(_) => break, // Sender dropped → all AsyncGrammarLoaders gone.
            }
        };

        let name = job.name.clone();
        // Guard the load with `catch_unwind`: a panic inside `loader.load`
        // (e.g. a grammar with a pathological query) must NOT kill the worker
        // thread or leave the in-flight entry stuck in the map forever —
        // subscribers would then wait on a job no live worker will ever
        // complete. Convert a panic into a `Failed` result and carry on.
        let result: Result<PathBuf, LoadError> =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loader.load(&job.name, &job.spec, &job.meta)
            })) {
                Ok(r) => r.map_err(|e| LoadError::Failed(format!("{e:#}"))),
                Err(_) => Err(LoadError::Failed(format!(
                    "grammar loader panicked while loading `{name}`"
                ))),
            };

        // Drain the subscriber list and broadcast (always runs, even after a
        // panic above, so the in-flight entry is removed).
        let senders: Vec<Sender<Result<PathBuf, LoadError>>> = {
            let mut map = in_flight.lock().expect("in_flight mutex poisoned");
            map.remove(&name).unwrap_or_default()
        };

        if senders.is_empty() {
            debug!("load {name}: all subscribers dropped, completing anyway");
        }

        for tx in senders {
            // Ignore send errors — receiver dropped (handle was discarded).
            let _ = tx.send(result.clone());
        }
    }
}

// ── LoadHandle ────────────────────────────────────────────────────────────────

/// A handle to an in-flight grammar load. Cheap to hold; drop at any time.
///
/// Poll via `try_recv` each event-loop tick, or block via `recv_blocking`.
pub struct LoadHandle {
    rx: Receiver<Result<PathBuf, LoadError>>,
}

impl LoadHandle {
    /// Non-blocking poll.
    ///
    /// Returns `None` while the load is still in-flight, `Some(Ok(path))` on
    /// success, `Some(Err(_))` on failure. Once `Some` is returned, subsequent
    /// calls return `None` (channel is consumed).
    pub fn try_recv(&self) -> Option<Result<PathBuf, LoadError>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(LoadError::Cancelled)),
        }
    }

    /// Block the current thread until the load completes.
    ///
    /// Use sparingly — this defeats the purpose of the async API. Useful in
    /// tests or in blocking CLI tools that want the convenient dedup but can
    /// afford to wait.
    pub fn recv_blocking(self) -> Result<PathBuf, LoadError> {
        self.rx.recv().unwrap_or(Err(LoadError::Cancelled))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::runtime::manifest::{LangSpec, ManifestMeta, QuerySource};

    // ── mock infrastructure ────────────────────────────────────────────────

    /// Internal backend trait used by the test harness only.
    /// `AsyncGrammarLoader::new` still takes the concrete `GrammarLoader` for
    /// the public API; the mock shim is kept entirely within `#[cfg(test)]`.
    trait LoaderBackend: Send + Sync + 'static {
        fn load(&self, name: &str, spec: &LangSpec, meta: &ManifestMeta)
        -> anyhow::Result<PathBuf>;
    }

    /// A `GrammarLoader`-shaped struct that records how many times `load` was
    /// called and delegates to an inner `LoaderBackend`.
    struct MockLoader {
        backend: Box<dyn LoaderBackend>,
        call_count: Arc<AtomicUsize>,
    }

    impl MockLoader {
        fn new(backend: impl LoaderBackend) -> Self {
            Self {
                backend: Box::new(backend),
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    /// Wrap a `MockLoader` in the smallest possible `GrammarLoader`-compatible
    /// shell. Since `GrammarLoader` is a concrete struct with real fields, the
    /// cleanest approach is a separate async loader parametric on the backend.
    /// Rather than fork `AsyncGrammarLoader`, we build a test-only version that
    /// uses `MockLoader` directly.
    struct TestAsyncLoader {
        inner: Arc<MockLoader>,
        in_flight: InFlight,
        job_tx: Sender<TestJob>,
        _handles: Vec<thread::JoinHandle<()>>,
    }

    struct TestJob {
        name: String,
        spec: LangSpec,
        meta: ManifestMeta,
    }

    impl TestAsyncLoader {
        fn new(mock: MockLoader) -> Self {
            let inner = Arc::new(mock);
            let in_flight: InFlight = Arc::new(Mutex::new(HashMap::new()));
            let (job_tx, job_rx) = mpsc::channel::<TestJob>();
            let shared_rx = Arc::new(Mutex::new(job_rx));

            let mut handles = Vec::with_capacity(2);
            for _ in 0..2 {
                let loader_clone = Arc::clone(&inner);
                let in_flight_clone = Arc::clone(&in_flight);
                let rx_clone = Arc::clone(&shared_rx);

                let handle = thread::spawn(move || {
                    loop {
                        let job = {
                            let guard = rx_clone.lock().unwrap();
                            match guard.recv() {
                                Ok(j) => j,
                                Err(_) => break,
                            }
                        };
                        loader_clone.call_count.fetch_add(1, Ordering::SeqCst);
                        let name = job.name.clone();
                        // Mirror the production worker: a panic in the backend
                        // must not kill the worker or leak the in-flight entry.
                        let result =
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                loader_clone.backend.load(&job.name, &job.spec, &job.meta)
                            })) {
                                Ok(r) => r.map_err(|e| LoadError::Failed(format!("{e:#}"))),
                                Err(_) => {
                                    Err(LoadError::Failed(format!("panicked loading `{name}`")))
                                }
                            };

                        let senders: Vec<_> = {
                            let mut map = in_flight_clone.lock().unwrap();
                            map.remove(&name).unwrap_or_default()
                        };
                        if senders.is_empty() {
                            debug!("load {name}: all subscribers dropped, completing anyway");
                        }
                        for tx in senders {
                            let _ = tx.send(result.clone());
                        }
                    }
                });
                handles.push(handle);
            }

            Self {
                inner,
                in_flight,
                job_tx,
                _handles: handles,
            }
        }

        fn load_async(&self, name: String, spec: LangSpec, meta: ManifestMeta) -> LoadHandle {
            let (tx, rx) = mpsc::channel();
            let mut map = self.in_flight.lock().unwrap();
            if let Some(senders) = map.get_mut(&name) {
                senders.push(tx);
            } else {
                map.insert(name.clone(), vec![tx]);
                drop(map);
                let _ = self.job_tx.send(TestJob { name, spec, meta });
                return LoadHandle { rx };
            }
            LoadHandle { rx }
        }

        fn call_count(&self) -> usize {
            self.inner.call_count.load(Ordering::SeqCst)
        }
    }

    // ── backend implementations ────────────────────────────────────────────

    fn dummy_meta() -> ManifestMeta {
        ManifestMeta {
            helix_repo: "https://github.com/helix-editor/helix".into(),
            helix_rev: "aaaa0000bbbb1111cccc2222dddd3333eeee4444".into(),
            nvim_treesitter_repo: "https://github.com/nvim-treesitter/nvim-treesitter".into(),
            nvim_treesitter_rev: "ffff5555aaaa0000bbbb1111cccc2222dddd3333".into(),
        }
    }

    fn dummy_spec() -> LangSpec {
        LangSpec {
            git_url: "https://example.invalid/repo".into(),
            git_rev: "0000000000000000".into(),
            subpath: None,
            extensions: vec!["x".into()],
            c_files: vec!["src/parser.c".into()],
            query_source: QuerySource::Helix,
            query_subdir: None,
            source: None,
        }
    }

    /// Happy-path backend: returns a fixed path immediately.
    struct OkBackend {
        path: PathBuf,
    }

    impl LoaderBackend for OkBackend {
        fn load(
            &self,
            _name: &str,
            _spec: &LangSpec,
            _meta: &ManifestMeta,
        ) -> anyhow::Result<PathBuf> {
            Ok(self.path.clone())
        }
    }

    /// Failing backend: always returns an error.
    struct ErrBackend;

    impl LoaderBackend for ErrBackend {
        fn load(
            &self,
            _name: &str,
            _spec: &LangSpec,
            _meta: &ManifestMeta,
        ) -> anyhow::Result<PathBuf> {
            anyhow::bail!("mock compile error: cc not found")
        }
    }

    /// Panicking backend: models a load that blows up mid-flight.
    struct PanicBackend;

    impl LoaderBackend for PanicBackend {
        fn load(
            &self,
            _name: &str,
            _spec: &LangSpec,
            _meta: &ManifestMeta,
        ) -> anyhow::Result<PathBuf> {
            panic!("boom in grammar loader");
        }
    }

    /// Slow backend: sleeps before returning to let tests observe the
    /// in-flight window.
    struct SlowBackend {
        delay: Duration,
        path: PathBuf,
    }

    impl LoaderBackend for SlowBackend {
        fn load(
            &self,
            _name: &str,
            _spec: &LangSpec,
            _meta: &ManifestMeta,
        ) -> anyhow::Result<PathBuf> {
            thread::sleep(self.delay);
            Ok(self.path.clone())
        }
    }

    // ── tests ──────────────────────────────────────────────────────────────

    /// A panic inside the loader must resolve the handle to `Failed` (not a
    /// disconnected `Cancelled`) and must remove the in-flight entry rather
    /// than leaking it forever.
    #[test]
    fn worker_backend_panic_reports_failed_and_clears_in_flight() {
        // Silence the default panic hook so the deliberate panic doesn't spew
        // a backtrace to the test log. nextest isolates tests per process.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let loader = TestAsyncLoader::new(MockLoader::new(PanicBackend));
        let handle = loader.load_async("boom".into(), dummy_spec(), dummy_meta());
        let res = handle.recv_blocking();

        std::panic::set_hook(prev);

        assert!(
            matches!(res, Err(LoadError::Failed(_))),
            "panic must surface as Failed, got {res:?}"
        );
        // The worker removes the in-flight entry before broadcasting, so it
        // should already be gone; poll briefly to avoid a race.
        let mut cleared = false;
        for _ in 0..100 {
            if loader.in_flight.lock().unwrap().is_empty() {
                cleared = true;
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(cleared, "in-flight entry leaked after a loader panic");
    }

    /// Five concurrent requests for the same grammar name must trigger exactly
    /// one underlying `load()` call.
    #[test]
    fn load_async_dedups_concurrent_requests() {
        let path = PathBuf::from("/fake/rust.so");
        // Use a small sleep so all 5 requests arrive before the worker finishes.
        let mock = MockLoader::new(SlowBackend {
            delay: Duration::from_millis(80),
            path: path.clone(),
        });
        let loader = TestAsyncLoader::new(mock);

        let mut handles = Vec::new();
        for _ in 0..5 {
            handles.push(loader.load_async("test_grammar".into(), dummy_spec(), dummy_meta()));
        }

        // All 5 handles must resolve to the same Ok path.
        for h in handles {
            assert_eq!(h.recv_blocking().unwrap(), path);
        }

        // Only ONE actual load() call must have been made.
        assert_eq!(
            loader.call_count(),
            1,
            "expected 1 load() call, got {}",
            loader.call_count()
        );
    }

    /// A failing backend must surface `LoadError::Failed` to ALL subscribers.
    #[test]
    fn load_async_propagates_failure_to_all_subscribers() {
        let mock = MockLoader::new(ErrBackend);
        let loader = TestAsyncLoader::new(mock);

        // Use a tiny sleep-backed slow path so all 3 subscriptions arrive
        // before the worker finishes; ErrBackend is instant but the dedup
        // window still holds while the job is in the queue.
        let mut handles = Vec::new();
        for _ in 0..3 {
            handles.push(loader.load_async("fail_grammar".into(), dummy_spec(), dummy_meta()));
        }

        for h in handles {
            match h.recv_blocking() {
                Err(LoadError::Failed(msg)) => {
                    assert!(
                        msg.contains("mock compile error"),
                        "unexpected error: {msg}"
                    )
                }
                other => panic!("expected LoadError::Failed, got {other:?}"),
            }
        }
    }

    /// `try_recv` returns `None` while in-flight, then `Some` on completion.
    #[test]
    fn try_recv_returns_none_while_in_flight_then_some_on_completion() {
        let path = PathBuf::from("/fake/slow.so");
        let mock = MockLoader::new(SlowBackend {
            delay: Duration::from_millis(100),
            path: path.clone(),
        });
        let loader = TestAsyncLoader::new(mock);

        let handle = loader.load_async("slow_grammar".into(), dummy_spec(), dummy_meta());

        // Should be None immediately (worker is sleeping 100 ms).
        assert!(handle.try_recv().is_none(), "expected None while in-flight");

        // Wait for completion (generous timeout).
        thread::sleep(Duration::from_millis(300));

        assert_eq!(
            handle.try_recv().unwrap().unwrap(),
            path,
            "expected Ok(path) after completion"
        );
    }

    /// Basic happy-path: `recv_blocking` returns the resolved path.
    #[test]
    fn recv_blocking_returns_result() {
        let path = PathBuf::from("/fake/rust.so");
        let mock = MockLoader::new(OkBackend { path: path.clone() });
        let loader = TestAsyncLoader::new(mock);

        let handle = loader.load_async("rust_grammar".into(), dummy_spec(), dummy_meta());
        assert_eq!(handle.recv_blocking().unwrap(), path);
    }
}
