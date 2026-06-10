//! Background worker for the explorer's full directory walk.
//!
//! Opening the explorer in a huge directory (a home dir) must not block the
//! first paint. The app shows a shallow (top-level-only) tree immediately, then
//! submits a job here; this worker runs the full recursive walk + git status on
//! a background thread and hands back an [`ExplorerScan`] the app swaps in once
//! it arrives. One thread services all jobs; only the latest job matters
//! (latest-wins), mirroring the git-signs worker.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use super::explorer::{ExplorerScan, scan_explorer_tree};

/// A request to walk `root`'s full tree with the given filter options.
pub(crate) struct ExplorerScanJob {
    pub root: PathBuf,
    pub show_hidden: bool,
    pub respect_gitignore: bool,
}

/// Background walker. Submit a job on open / refresh; drain the result each tick.
pub(crate) struct ExplorerScanWorker {
    tx: Option<Sender<ExplorerScanJob>>,
    rx: Receiver<ExplorerScan>,
    join: Option<thread::JoinHandle<()>>,
}

impl ExplorerScanWorker {
    /// Spawn the worker thread. Returns immediately.
    pub(crate) fn new() -> Self {
        let (job_tx, job_rx) = std::sync::mpsc::channel::<ExplorerScanJob>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<ExplorerScan>();
        let handle = thread::Builder::new()
            .name("hjkl-explorer-scan".into())
            .spawn(move || worker_loop(job_rx, res_tx))
            .expect("spawn explorer-scan worker");
        Self {
            tx: Some(job_tx),
            rx: res_rx,
            join: Some(handle),
        }
    }

    /// Submit a walk job. Non-blocking; silently dropped if the worker is gone.
    pub(crate) fn submit(&self, job: ExplorerScanJob) {
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(job);
        }
    }

    /// Non-blocking drain of the latest completed scan, if any. Drains the whole
    /// queue and returns only the most recent (older results are stale).
    pub(crate) fn try_recv(&self) -> Option<ExplorerScan> {
        let mut latest = None;
        while let Ok(scan) = self.rx.try_recv() {
            latest = Some(scan);
        }
        latest
    }
}

impl Drop for ExplorerScanWorker {
    /// Close the job channel and join the worker before returning. The walk
    /// touches `libgit2`; joining (rather than detaching) avoids the same
    /// OpenSSL-cleanup-vs-worker race the git-signs worker documents.
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl Default for ExplorerScanWorker {
    fn default() -> Self {
        Self::new()
    }
}

/// Worker loop: for each job, run the full walk and forward the result. When a
/// burst of jobs is queued, only the last is processed (the earlier roots are
/// already superseded).
fn worker_loop(job_rx: Receiver<ExplorerScanJob>, res_tx: Sender<ExplorerScan>) {
    while let Ok(first) = job_rx.recv() {
        // Coalesce: take the most recent queued job (latest-wins).
        let mut job = first;
        while let Ok(newer) = job_rx.try_recv() {
            job = newer;
        }
        let scan = scan_explorer_tree(job.root, job.show_hidden, job.respect_gitignore);
        if res_tx.send(scan).is_err() {
            break; // app dropped the worker
        }
    }
}
