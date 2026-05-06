//! JSON-RPC framing: `Content-Length: N\r\n\r\n<payload>` over byte streams.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum message size accepted (16 MiB). Guards against runaway servers.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Write a single framed JSON-RPC message to `w`.
pub async fn write_message<W: AsyncWrite + Unpin>(
    w: &mut W,
    payload: &[u8],
) -> std::io::Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", payload.len());
    w.write_all(header.as_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await
}

/// Read a single framed JSON-RPC message from `r`.
///
/// Returns `Ok(None)` on a clean EOF before the first header byte.
/// Returns an error on malformed headers, oversized payloads, or mid-message EOF.
pub async fn read_message<R: AsyncBufRead + Unpin>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).await?;
        if n == 0 {
            // EOF — only clean if we haven't started reading headers yet.
            if content_length.is_none() && line.is_empty() {
                return Ok(None);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "EOF while reading LSP message headers",
            ));
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);

        if trimmed.is_empty() {
            // Blank line = end of headers.
            break;
        }

        if let Some(rest) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            let len: usize = rest.trim().parse().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("malformed Content-Length: {rest:?}"),
                )
            })?;
            if len > MAX_MESSAGE_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("LSP message too large: {len} > {MAX_MESSAGE_BYTES}"),
                ));
            }
            content_length = Some(len);
        }
        // Other headers (Content-Type, etc.) are silently skipped.
    }

    let len = content_length.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "LSP message missing Content-Length header",
        )
    })?;

    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{BufReader, duplex};

    async fn roundtrip(payload: &[u8]) -> Vec<u8> {
        // Use a large duplex capacity so small payloads don't need a concurrent reader.
        let capacity = payload.len() + 64;
        let (mut client, server) = duplex(capacity);
        write_message(&mut client, payload).await.unwrap();
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        read_message(&mut reader).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn roundtrip_simple() {
        let msg = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}";
        assert_eq!(roundtrip(msg).await, msg);
    }

    #[tokio::test]
    async fn roundtrip_empty_body() {
        let msg = b"{}";
        assert_eq!(roundtrip(msg).await, msg);
    }

    #[tokio::test]
    async fn roundtrip_large_body() {
        // Large payload — must concurrently write and read to avoid deadlock
        // (duplex is bounded; a sequential write would block until the reader drains).
        let payload: Vec<u8> = b"x".repeat(100_000);
        let (mut client, server) = duplex(65536);
        let write_task = tokio::spawn(async move {
            write_message(&mut client, &payload).await.unwrap();
            payload // return so we can compare
        });
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let result = read_message(&mut reader).await.unwrap().unwrap();
        let original = write_task.await.unwrap();
        assert_eq!(result, original);
    }

    #[tokio::test]
    async fn partial_buffer_split_read() {
        // duplex with a 16-byte buffer forces multiple read syscalls.
        let (mut client, server) = duplex(16);
        let payload = b"{\"id\":2}";
        let write_task = tokio::spawn(async move {
            write_message(&mut client, payload).await.unwrap();
        });
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let result = read_message(&mut reader).await.unwrap().unwrap();
        write_task.await.unwrap();
        assert_eq!(result, payload);
    }

    #[tokio::test]
    async fn malformed_content_length_rejected() {
        let (mut client, server) = duplex(65536);
        let garbage = b"Content-Length: not-a-number\r\n\r\n";
        client.write_all(garbage).await.unwrap();
        drop(client);
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let err = read_message(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("malformed Content-Length"));
    }

    #[tokio::test]
    async fn oversize_message_rejected() {
        let (mut client, server) = duplex(65536);
        let too_big = MAX_MESSAGE_BYTES + 1;
        let header = format!("Content-Length: {too_big}\r\n\r\n");
        client.write_all(header.as_bytes()).await.unwrap();
        drop(client);
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let err = read_message(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("too large"));
    }

    #[tokio::test]
    async fn clean_eof_returns_none() {
        let (client, server) = duplex(64);
        drop(client);
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let result = read_message(&mut reader).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn missing_content_length_header_rejected() {
        let (mut client, server) = duplex(65536);
        // Valid-looking header line, but it's not Content-Length.
        let msg = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\ndata";
        client.write_all(msg).await.unwrap();
        drop(client);
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let err = read_message(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("missing Content-Length"));
    }
}
