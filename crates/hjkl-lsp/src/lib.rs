//! LSP client for hjkl. See module docs.

pub mod codec;
pub mod config;
pub mod event;
mod manager;
mod runtime;
mod server;
pub mod uri;
pub mod workspace;

pub use config::{LspConfig, ServerConfig};
pub use event::{LspCommand, LspEvent, RpcError, ServerKey};
pub use manager::LspManager;
pub use server::Server;

pub type BufferId = u64;

/// Test helpers exposed for integration tests in `tests/`.
pub mod testing {
    use crossbeam_channel::Sender;
    use tokio::io::{AsyncRead, AsyncWrite};

    use crate::event::{LspEvent, ServerKey};
    use crate::server;

    /// Spawn a `Server` using arbitrary I/O instead of a real child process.
    /// Used by integration tests in `tests/mock_server.rs`.
    pub async fn spawn_server_from_io<R, W>(
        key: ServerKey,
        stdin_writer: W,
        stdout_reader: R,
        evt_tx: Sender<LspEvent>,
    ) -> anyhow::Result<server::Server>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        server::spawn_from_io(key, stdin_writer, stdout_reader, evt_tx).await
    }
}
