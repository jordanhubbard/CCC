//! Bus endpoints: send, recent messages, and a live SSE stream.
//!
//! The SSE stream is returned as `impl Stream<Item = Result<BusMsg>>`.
//! Callers drive it with `StreamExt::next().await` and decide their own
//! reconnection policy — the stream ends cleanly when the server closes
//! the connection, or yields `Err` for transport/JSON errors so callers
//! can decide whether to continue.

use crate::{Client, Error, Result};
use acc_model::{BusMsg, BusSendRequest};
use async_stream::try_stream;
use bytes::Bytes;
use futures_util::stream::{Stream, StreamExt};
use serde::Deserialize;

#[derive(Debug, Clone, Copy)]
pub struct BusApi<'a> {
    pub(crate) client: &'a Client,
}

impl<'a> BusApi<'a> {
    /// POST /api/bus/send
    pub async fn send(self, req: &BusSendRequest) -> Result<()> {
        let resp = self
            .client
            .http()
            .post(self.client.url("/api/bus/send"))
            .json(req)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let bytes = resp.bytes().await?;
        Err(Error::from_response(status, &bytes))
    }

    /// GET /api/bus/messages — list recent messages.
    pub async fn messages(self, limit: Option<u32>, kind: Option<&str>) -> Result<Vec<BusMsg>> {
        let mut q: Vec<(&'static str, String)> = Vec::new();
        if let Some(n) = limit {
            q.push(("limit", n.to_string()));
        }
        if let Some(k) = kind {
            q.push(("type", k.to_string()));
        }
        let resp = self
            .client
            .http()
            .get(self.client.url("/api/bus/messages"))
            .query(&q)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await?;
        if !(200..300).contains(&status) {
            return Err(Error::from_response(status, &bytes));
        }
        let env: ListEnvelope = serde_json::from_slice(&bytes)?;
        Ok(match env {
            ListEnvelope::Wrapped { messages } => messages,
            ListEnvelope::Bare(v) => v,
        })
    }

    /// GET /api/bus/stream — Server-Sent Events stream of bus messages.
    ///
    /// The stream yields messages as the server emits them. Keep-alive
    /// comment frames are silently skipped. The stream terminates when
    /// the server closes the connection; implement reconnect logic at
    /// the call site if needed.
    pub fn stream(self) -> impl Stream<Item = Result<BusMsg>> + 'a {
        let client = self.client;
        try_stream! {
            let mut body = Box::pin(open_sse(client).await?);
            let mut buf: Vec<u8> = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(Error::Http)?;
                buf.extend_from_slice(&chunk);
                while let Some(end) = find_frame_boundary(&buf) {
                    // `end` is the index of the last byte of the `\n\n`.
                    // Split off the frame including the terminator.
                    let frame: Vec<u8> = buf.drain(..=end).collect();
                    if let Some(data) = extract_sse_data(&frame) {
                        // Malformed JSON in a single frame should not kill
                        // the whole stream — servers occasionally emit
                        // garbage (comments, test events, etc.). Skip
                        // parse failures silently and keep streaming.
                        if let Ok(msg) = serde_json::from_str::<BusMsg>(&data) {
                            yield msg;
                        }
                    }
                    // No `data:` lines in frame = keep-alive or metadata-only; skip.
                }
            }
        }
    }
}

/// Open the SSE stream, resolving the status-code check before we start
/// consuming frames. Factored out so the happy-path `resp.bytes_stream()`
/// and the error-path `resp.bytes()` don't need to share `resp`.
async fn open_sse(client: &Client) -> Result<impl Stream<Item = reqwest::Result<Bytes>> + 'static> {
    let resp = client
        .http()
        .get(client.url("/api/bus/stream"))
        .header("accept", "text/event-stream")
        .send()
        .await?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let bytes = resp.bytes().await?;
        return Err(Error::from_response(status, &bytes));
    }
    Ok(resp.bytes_stream())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ListEnvelope {
    Wrapped { messages: Vec<BusMsg> },
    Bare(Vec<BusMsg>),
}

// ── SSE frame parsing ──────────────────────────────────────────────────────
//
// Server-Sent Events separate frames with a blank line. A blank line is
// encoded as either "\n\n" or "\r\n\r\n" depending on the transport. We
// handle both.

/// Returns the byte index of the last byte of a frame terminator (either
/// `\n\n` or `\r\n\r\n`), if one exists in `buf`.
fn find_frame_boundary(buf: &[u8]) -> Option<usize> {
    let mut lf = None;
    let mut crlf = None;
    for i in 1..buf.len() {
        if buf[i] == b'\n' && buf[i - 1] == b'\n' {
            lf = Some(i);
            break;
        }
        if i >= 3 && &buf[i - 3..=i] == b"\r\n\r\n" {
            crlf = Some(i);
            break;
        }
    }
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Extracts and concatenates `data:` fields from an SSE frame.
///
/// Per the SSE spec: multiple `data:` lines in one event are joined with
/// `\n`. Lines beginning with `:` are comments and are ignored. Returns
/// `None` if the frame has no `data:` lines (typical for keep-alives).
fn extract_sse_data(frame: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(frame).ok()?;
    let mut out = String::new();
    let mut saw_data = false;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            if saw_data {
                out.push('\n');
            }
            // One optional leading space per the spec.
            out.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            saw_data = true;
        }
        // other fields (event:, id:, retry:) — ignored for now
    }
    if saw_data {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_boundary_finds_lf_pair() {
        assert_eq!(find_frame_boundary(b"data: x\n\nmore"), Some(8));
    }

    #[test]
    fn sse_boundary_finds_crlf_pair() {
        assert_eq!(find_frame_boundary(b"data: x\r\n\r\nmore"), Some(10));
    }

    #[test]
    fn sse_boundary_none_for_partial() {
        assert!(find_frame_boundary(b"data: x\n").is_none());
    }

    #[test]
    fn extract_data_joins_multiple_data_lines() {
        let frame = b"data: {\"a\":1,\ndata: \"b\":2}\n\n";
        assert_eq!(
            extract_sse_data(frame).as_deref(),
            Some("{\"a\":1,\n\"b\":2}")
        );
    }

    #[test]
    fn extract_data_ignores_comments_and_meta() {
        let frame = b": keepalive\nevent: msg\ndata: hi\n\n";
        assert_eq!(extract_sse_data(frame).as_deref(), Some("hi"));
    }

    #[test]
    fn extract_data_none_for_keepalive_only() {
        let frame = b": keepalive\n\n";
        assert!(extract_sse_data(frame).is_none());
    }
}
