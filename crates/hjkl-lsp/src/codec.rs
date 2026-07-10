//! JSON-RPC framing: `Content-Length: N\r\n\r\n<payload>` over byte streams.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum message size accepted (16 MiB). Guards against runaway servers.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum total header-section size accepted (64 KiB). Real LSP headers are
/// tens of bytes; this guards against a misbehaving server streaming an
/// endless header line (unbounded allocation) or endless header lines.
const MAX_HEADER_BYTES: usize = 64 * 1024;

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
    // Remaining header bytes we are willing to read for this message.
    let mut header_budget = MAX_HEADER_BYTES;

    loop {
        let mut line = String::new();
        // Bound each read so a header line with no terminating newline cannot
        // grow `line` without limit. Reading one byte past the budget lets us
        // distinguish "exactly at the limit" from "over it".
        let n = {
            let mut limited = (&mut *r).take(header_budget as u64 + 1);
            limited.read_line(&mut line).await?
        };
        if n == 0 {
            // EOF — only clean if we haven't started reading headers yet.
            if header_budget == MAX_HEADER_BYTES {
                return Ok(None);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "EOF while reading LSP message headers",
            ));
        }
        if n > header_budget {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("LSP message headers exceed {MAX_HEADER_BYTES} bytes"),
            ));
        }
        header_budget -= n;

        let trimmed = line.trim_end_matches(['\r', '\n']);

        if trimmed.is_empty() {
            // Blank line = end of headers.
            break;
        }

        // Header names are case-insensitive (HTTP-style framing).
        if let Some((name, rest)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
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
    async fn unterminated_header_line_rejected() {
        // A header line longer than the total header budget, never terminated
        // by a newline, must be rejected instead of buffered without bound.
        let (mut client, server) = duplex(MAX_HEADER_BYTES + 4096);
        let junk = vec![b'A'; MAX_HEADER_BYTES + 1024];
        client.write_all(&junk).await.unwrap();
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let err = read_message(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("headers exceed"));
    }

    #[tokio::test]
    async fn endless_header_lines_rejected() {
        // Many small header lines that never end with a blank line must hit
        // the header budget instead of looping forever.
        let (mut client, server) = duplex(MAX_HEADER_BYTES + 4096);
        let line = b"X-Filler: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n".repeat(MAX_HEADER_BYTES / 32);
        let write_task = tokio::spawn(async move {
            let _ = client.write_all(&line).await;
        });
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let err = read_message(&mut reader).await.unwrap_err();
        write_task.await.unwrap();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("headers exceed"));
    }

    #[tokio::test]
    async fn eof_mid_headers_is_error() {
        // Partial header data followed by EOF is not a clean EOF.
        let (mut client, server) = duplex(64);
        client.write_all(b"Content-Length: 5\r\n").await.unwrap();
        drop(client);
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let err = read_message(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn mixed_case_content_length_accepted() {
        let (mut client, server) = duplex(65536);
        client
            .write_all(b"CONTENT-LENGTH: 2\r\n\r\n{}")
            .await
            .unwrap();
        drop(client);
        let mut reader = BufReader::with_capacity(256 * 1024, server);
        let msg = read_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(msg, b"{}");
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
