//! `LspManager` — the sync-side handle to the async LSP runtime thread.

use std::path::Path;

use crossbeam_channel::Receiver;
use tokio::sync::mpsc::UnboundedSender;

use crate::BufferId;
use crate::config::LspConfig;
use crate::event::{LspCommand, LspEvent};
use crate::runtime;

/// Owned handle to the background LSP thread and its channels.
///
/// Drop calls `ShutdownAll` but does **not** join the thread. Call
/// [`LspManager::shutdown`] for a clean, blocking teardown.
pub struct LspManager {
    cmd_tx: UnboundedSender<LspCommand>,
    evt_rx: Receiver<LspEvent>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LspManager {
    /// Spawn the background "hjkl-lsp" thread with its own `current_thread`
    /// tokio runtime. Returns immediately.
    pub fn spawn(config: LspConfig) -> Self {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<LspCommand>();
        let (evt_tx, evt_rx) = crossbeam_channel::unbounded::<LspEvent>();

        let thread = std::thread::Builder::new()
            .name("hjkl-lsp".to_string())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build LSP tokio runtime");
                rt.block_on(runtime::dispatch(cmd_rx, evt_tx, config));
            })
            .expect("failed to spawn hjkl-lsp thread");

        Self {
            cmd_tx,
            evt_rx,
            thread: Some(thread),
        }
    }

    /// Gracefully shut down: sends `ShutdownAll`, joins the thread (≤ 2 s).
    pub fn shutdown(mut self) {
        let _ = self.cmd_tx.send(LspCommand::ShutdownAll);
        if let Some(handle) = self.thread.take() {
            // Park the current thread with a 2-second timeout.
            // `std::thread::JoinHandle` doesn't have a timed join in stable Rust,
            // so we use a helper thread to enforce the deadline.
            let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
            std::thread::spawn(move || {
                let _ = handle.join();
                let _ = done_tx.send(());
            });
            let _ = done_rx.recv_timeout(std::time::Duration::from_secs(2));
        }
    }

    /// Attach a buffer to the appropriate language server.
    pub fn attach_buffer(&self, id: BufferId, path: &Path, language_id: &str, text: &str) {
        let _ = self.cmd_tx.send(LspCommand::AttachBuffer {
            id,
            path: path.to_path_buf(),
            language_id: language_id.to_string(),
            text: text.to_string(),
        });
    }

    /// Detach a buffer (closes the document on the server side).
    pub fn detach_buffer(&self, id: BufferId) {
        let _ = self.cmd_tx.send(LspCommand::DetachBuffer { id });
    }

    /// Notify the server that a buffer's full text changed.
    pub fn notify_change(&self, id: BufferId, full_text: &str) {
        let _ = self.cmd_tx.send(LspCommand::NotifyChange {
            id,
            full_text: full_text.to_string(),
        });
    }

    /// Non-blocking poll: returns the next pending event, or `None` if empty.
    pub fn try_recv_event(&self) -> Option<LspEvent> {
        self.evt_rx.try_recv().ok()
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(LspCommand::ShutdownAll);
        // Don't join in Drop — caller should call shutdown() explicitly.
        // If the thread is still around, just detach (JoinHandle drop detaches).
    }
}
