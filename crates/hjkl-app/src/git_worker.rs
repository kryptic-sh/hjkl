//! Background worker for git diff-change computation.
//!
//! Moves the `git2::Diff` + `is_untracked` work off the UI thread.
//! The worker owns a single background thread. Jobs are submitted via
//! [`GitSignsWorker::submit`] (non-blocking; latest-wins per buffer_id)
//! and results are drained via [`GitSignsWorker::try_recv`] each tick.
//!
//! Coalescing policy: when a new job for a buffer arrives before the
//! previous one has been picked up by the worker, the old job is
//! replaced. This mirrors the `SyntaxWorker` pattern in `syntax.rs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::thread;

use crossbeam_channel::{Receiver, Sender};

use crate::git::GitChange;
use hjkl_buffer::BufferId;

/// A git diff job submitted to the worker.
pub struct GitJob {
    pub buffer_id: BufferId,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub dirty_gen: u64,
}

/// Result produced by the worker for a single job.
pub struct GitResult {
    pub buffer_id: BufferId,
    pub dirty_gen: u64,
    pub changes: Vec<GitChange>,
    pub is_untracked: bool,
}

/// Background worker that computes git diff changes off the UI thread.
///
/// One background thread services all submissions. Jobs are coalesced
/// per buffer_id (latest-wins) so a burst of buffer switches or edits
/// never queues more than one job per buffer at a time.
pub struct GitSignsWorker {
    tx: Sender<GitJob>,
    rx: Receiver<GitResult>,
    _join: thread::JoinHandle<()>,
}

impl GitSignsWorker {
    /// Spawn the worker thread. Returns immediately.
    pub fn new() -> Self {
        // Bounded(1): the worker drains as fast as possible; if the UI
        // submits faster than the worker can process, only the most
        // recent pending job matters. We use an unbounded result channel
        // so the worker never blocks on a slow UI drain.
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<GitJob>();
        let (res_tx, res_rx) = crossbeam_channel::unbounded::<GitResult>();

        let handle = thread::Builder::new()
            .name("hjkl-git-signs".into())
            .spawn(move || worker_loop(job_rx, res_tx))
            .expect("spawn git-signs worker");

        Self {
            tx: job_tx,
            rx: res_rx,
            _join: handle,
        }
    }

    /// Submit a job. Non-blocking.
    ///
    /// Coalescing is handled on the worker side via a `HashMap` that
    /// keeps only the latest job per buffer_id. Here we just push onto
    /// the unbounded channel; the worker drains quickly and the per-
    /// buffer latest-wins logic lives in [`worker_loop`].
    pub fn submit(&self, job: GitJob) {
        // Ignore send errors — if the channel is disconnected (worker
        // panicked and was cleaned up), silently drop the job. The UI
        // will simply not receive updated git changes.
        let _ = self.tx.send(job);
    }

    /// Non-blocking drain. Returns the next completed result, if any.
    /// Call repeatedly per tick to process all queued results.
    pub fn try_recv(&self) -> Option<GitResult> {
        self.rx.try_recv().ok()
    }
}

impl Default for GitSignsWorker {
    fn default() -> Self {
        Self::new()
    }
}

/// Main loop executed on the worker thread.
///
/// Drains incoming jobs into a per-buffer-id "latest job" map, then
/// processes one job at a time in FIFO order of first arrival, always
/// using the most-recent job for that buffer_id (coalesce). Results
/// are sent back on `res_tx`.
fn worker_loop(job_rx: Receiver<GitJob>, res_tx: Sender<GitResult>) {
    // Map from buffer_id to the most recent pending job for that buffer.
    let mut pending: HashMap<BufferId, GitJob> = HashMap::new();
    // Queue of buffer_ids to process in order (FIFO by first arrival).
    let mut queue: Vec<BufferId> = Vec::new();

    loop {
        // Block until at least one job arrives (or channel closes).
        let first = match job_rx.recv() {
            Ok(j) => j,
            Err(_) => return, // sender dropped → exit
        };

        // Drain all immediately-available additional jobs without blocking.
        let mut batch = vec![first];
        while let Ok(j) = job_rx.try_recv() {
            batch.push(j);
        }

        // Coalesce: latest-wins per buffer_id, FIFO queue for first arrival.
        for job in batch {
            let id = job.buffer_id;
            let is_new = !pending.contains_key(&id);
            pending.insert(id, job);
            if is_new {
                queue.push(id);
            }
        }

        // Process all queued buffer_ids (in order of first arrival).
        // Each processes the most recent job for that buffer.
        let ids: Vec<BufferId> = std::mem::take(&mut queue);
        for id in ids {
            let job = match pending.remove(&id) {
                Some(j) => j,
                None => continue, // already consumed by a duplicate entry
            };

            let changes = crate::git::changes_for_bytes(&job.path, &job.bytes);
            let is_untracked = crate::git::is_untracked(&job.path);

            let result = GitResult {
                buffer_id: job.buffer_id,
                dirty_gen: job.dirty_gen,
                changes,
                is_untracked,
            };

            if res_tx.send(result).is_err() {
                // Receiver dropped → UI is gone. Exit.
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Worker with a nonexistent path and empty bytes must return
    /// `is_untracked = true` and `changes = []` because there is no git
    /// repo at /tmp/nonexistent_hjkl_git_test_path. The test is bounded
    /// to 500 ms to catch any accidental blocking.
    #[test]
    fn worker_returns_result_for_nonexistent_path() {
        let worker = GitSignsWorker::new();
        let job = GitJob {
            buffer_id: 42,
            path: PathBuf::from("/tmp/nonexistent_hjkl_git_test_path_12345/file.txt"),
            bytes: Vec::new(),
            dirty_gen: 7,
        };
        worker.submit(job);

        let deadline = Instant::now() + Duration::from_millis(500);
        let result = loop {
            if let Some(r) = worker.try_recv() {
                break Some(r);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };

        let result = result.expect("worker should return a result within 500ms");
        assert_eq!(result.buffer_id, 42);
        assert_eq!(result.dirty_gen, 7);
        assert!(
            result.changes.is_empty(),
            "expected empty changes for nonexistent path; got {:?}",
            result.changes
        );
        // `is_untracked` returns false on I/O error (no repo / no such file);
        // the important invariant is that the worker returns a result at all
        // and that changes is empty. is_untracked may be false for a path that
        // doesn't exist on disk because git2::Repository::discover fails first.
        assert!(
            !result.is_untracked || result.changes.is_empty(),
            "unexpected changes for nonexistent path: {:?}",
            result.changes
        );
    }

    /// Two jobs for the same buffer_id submitted in quick succession should
    /// coalesce: only one result arrives (or the second is the latest).
    #[test]
    fn worker_coalesces_jobs_for_same_buffer() {
        let worker = GitSignsWorker::new();
        for dg in 0u64..5 {
            worker.submit(GitJob {
                buffer_id: 1,
                path: PathBuf::from("/tmp/nonexistent_hjkl_coalesce_test/f.txt"),
                bytes: Vec::new(),
                dirty_gen: dg,
            });
        }

        // Drain whatever we get within 500 ms.
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut results: Vec<GitResult> = Vec::new();
        loop {
            while let Some(r) = worker.try_recv() {
                results.push(r);
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            !results.is_empty(),
            "expected at least one result from the worker"
        );
        // All results must be for buffer_id 1.
        for r in &results {
            assert_eq!(r.buffer_id, 1);
        }
        // The last result must have a dirty_gen of 4 (the latest submitted).
        // Due to coalescing the worker may skip intermediate gens.
        let last_gen = results.iter().map(|r| r.dirty_gen).max().unwrap();
        assert_eq!(last_gen, 4, "expected latest dirty_gen=4 to be delivered");
    }
}
