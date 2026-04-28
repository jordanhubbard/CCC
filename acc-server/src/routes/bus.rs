use crate::AppState;

/// Default TTL (seconds) for blob messages stored on the bus log.
/// Blobs older than this are eligible for eviction by the retention sweeper.
pub const BLOB_DEFAULT_TTL_SECS: u64 = 86_400; // 24 hours

/// Maximum TTL (seconds) a caller may request for a blob message.
/// Requests exceeding this are clamped to this value.
pub const BLOB_MAX_TTL_SECS: u64 = 604_800; // 7 days

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Json,
    },
    routing::{get, post},
    Router,
};
use futures_util::stream::{self, Stream, StreamExt};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio_stream::wrappers::BroadcastStream;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // /api/bus/* — used by dashboard UI and API clients
        .route("/api/bus/stream", get(bus_stream))
        .route("/api/bus/send", post(bus_send))
        .route("/api/bus/messages", get(bus_messages))
        .route("/api/bus/presence", get(bus_presence))
        // /bus/* — used by ClawChat (nginx proxies /bus/ → 8789/bus/)
        .route("/bus/stream", get(bus_stream))
        .route("/bus/send", post(bus_send))
        .route("/bus/messages", get(bus_messages))
        .route("/bus/presence", get(bus_presence))
        // Bus viewer — self-contained HTML dashboard for blob rendering
        .route("/bus/viewer", get(bus_viewer))
        .route("/api/bus/viewer", get(bus_viewer))
}

// ── Query params for /bus/messages ────────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct BusQuery {
    /// Max number of messages to return (default 500, max 2000).
    limit: Option<usize>,
    /// Filter by subject (channel). Matches exact string.
    subject: Option<String>,
    /// Filter by message type ("text", "reaction", etc.).
    #[serde(rename = "type")]
    msg_type: Option<String>,
    /// Filter replies: return only messages with this thread_id.
    thread_id: Option<String>,
    /// DM filter: combined with `from`, returns messages between two users.
    to: Option<String>,
    /// DM filter peer (used with `to`).
    from: Option<String>,
    /// Return only messages with ts > since (ISO-8601).
    since: Option<String>,
    /// Return only blob messages belonging to a specific chunked upload.
    /// Matches the `upload_id` field set by the sender on each chunk.
    upload_id: Option<String>,
}

// ── SSE stream ────────────────────────────────────────────────────────────────

async fn bus_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let replay = load_bus_messages(&state.bus_log_path, 50, &BusQuery::default()).await;

    let rx = state.bus_tx.subscribe();
    let live = BroadcastStream::new(rx).filter_map(|msg| async move {
        match msg {
            Ok(data) => {
                // Enrich blob messages on the live SSE path so subscribers
                // receive blob_meta without needing to re-query /bus/messages.
                let enriched = serde_json::from_str::<Value>(&data)
                    .map(|mut v| {
                        enrich_blob_message(&mut v);
                        v.to_string()
                    })
                    .unwrap_or(data);
                Some(Ok(Event::default().data(enriched)))
            }
            Err(_) => None,
        }
    });

    let connected = stream::once(async { Ok(Event::default().data(r#"{"type":"connected"}"#)) });
    let replayed = stream::iter(replay.into_iter().map(|raw| {
        let enriched = serde_json::from_str::<Value>(&raw)
            .map(|mut v| {
                enrich_blob_message(&mut v);
                v.to_string()
            })
            .unwrap_or(raw);
        Ok(Event::default().data(enriched))
    }));

    let combined = connected.chain(replayed).chain(live);
    Sse::new(combined).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(30))
            .text("ping"),
    )
}

// ── POST /bus/send ────────────────────────────────────────────────────────────

async fn bus_send(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    // Validate mime/enc if sender includes a mime field
    if let Some(mime_str) = body.get("mime").and_then(|v| v.as_str()) {
        let mime: crate::bus_types::MediaType = mime_str.parse().unwrap_or_else(|_| unreachable!());
        if !mime.is_known() {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"unknown_media_type","mime":mime_str,
                            "known_types":crate::bus_types::MediaType::all_known()})),
            ).into_response();
        }
        if mime.is_binary() {
            let enc = body.get("enc").and_then(|v| v.as_str()).unwrap_or("");
            if enc != "base64" {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"error":"binary_type_requires_base64_enc","mime":mime_str})),
                ).into_response();
            }
        }
    }

    let seq = state.bus_seq.fetch_add(1, Ordering::SeqCst);
    let now = chrono::Utc::now().to_rfc3339();

    let mut msg = body;
    if let Some(obj) = msg.as_object_mut() {
        // Assign stable id if not provided by the sender
        obj.entry("id").or_insert_with(|| json!(format!("msg-{}", seq)));
        obj.insert("seq".into(), json!(seq));
        obj.insert("ts".into(), json!(now));

        // Clamp ttl_secs on blob messages to BLOB_MAX_TTL_SECS.
        // Per SPEC.md: "Requests above BLOB_MAX_TTL_SECS (604 800 s / 7 days)
        // are clamped."  We only apply this to type=blob so that non-blob
        // messages are not affected.
        let is_blob = obj.get("type").and_then(|v| v.as_str()) == Some("blob");
        if is_blob {
            if let Some(ttl) = obj.get("ttl_secs").and_then(|v| v.as_u64()) {
                if ttl > BLOB_MAX_TTL_SECS {
                    obj.insert("ttl_secs".into(), json!(BLOB_MAX_TTL_SECS));
                }
            }
        }
    }

    let msg_str = serde_json::to_string(&msg).unwrap_or_default();
    let log_line = format!("{}\n", msg_str);
    let _ = append_line(&state.bus_log_path, &log_line).await;
    let _ = state.bus_tx.send(msg_str);

    Json(json!({"ok": true, "message": msg})).into_response()
}

// ── GET /bus/messages ─────────────────────────────────────────────────────────

async fn bus_messages(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<BusQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let limit = q.limit.unwrap_or(500).min(2000);
    let msgs = load_bus_messages(&state.bus_log_path, limit, &q).await;
    let parsed: Vec<Value> = msgs
        .iter()
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .map(|mut v| {
            enrich_blob_message(&mut v);
            v
        })
        .collect();
    Json(json!(parsed)).into_response()
}

// ── Blob metadata enrichment ──────────────────────────────────────────────────

/// For `type=blob` messages, attach a `blob_meta` object that the viewer uses
/// to pick the right HTML element.  The `blob_uri` is the authoritative source
/// for the media bytes: it is either a pre-stored URI carried on the message
/// itself (written by the storage layer that produced the blob) or a synthetic
/// data-URI built from the base64-encoded body so that the viewer works even
/// for messages that were posted directly without going through the storage
/// layer.
///
/// Rendering hints, per SPEC.md §MIME Type Conventions:
///   image/*  → <img src=blob_uri>
///   audio/*  → <audio controls src=blob_uri>
///   video/*  → <video controls src=blob_uri>
///   other    → <a href=blob_uri download> fallback link
///
/// Fields injected into `blob_meta`:
///   mime        – MIME type (copied from message `mime`, defaulting to
///                 "application/octet-stream")
///   enc         – encoding from message (`none` | `base64`)
///   size_bytes  – byte-length of the raw body string (proxy for payload size)
///   render_as   – one of "image" | "audio" | "video" | "download"
///   blob_uri    – URI to use as `src` / `href` in the rendered element;
///                 pre-existing URIs on the message take precedence
fn enrich_blob_message(msg: &mut Value) {
    let is_blob = msg
        .get("type")
        .and_then(|v| v.as_str())
        .map(|t| t == "blob")
        .unwrap_or(false);

    if !is_blob {
        return;
    }

    let obj = match msg.as_object_mut() {
        Some(o) => o,
        None => return,
    };

    let mime = obj
        .get("mime")
        .and_then(|v| v.as_str())
        .unwrap_or("application/octet-stream")
        .to_string();

    let enc = obj
        .get("enc")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string();

    let body_str = obj
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let size_bytes = body_str.len();

    let render_as = if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("video/") {
        "video"
    } else {
        "download"
    };

    // If the storage layer already wrote a blob_uri onto the message, honour
    // it; otherwise synthesise a data-URI from the encoded body so that the
    // viewer is self-contained even for inline blobs.
    let blob_uri: String = obj
        .get("blob_uri")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if enc == "base64" && !body_str.is_empty() {
                // body is already base64; wrap into a data URI
                format!("data:{};base64,{}", mime, body_str)
            } else if !body_str.is_empty() {
                // plain text body — percent-encode it into a data URI
                let encoded = body_str
                    .bytes()
                    .fold(String::new(), |mut acc, b| {
                        if b.is_ascii_alphanumeric()
                            || b == b'-'
                            || b == b'_'
                            || b == b'.'
                            || b == b'~'
                        {
                            acc.push(b as char);
                        } else {
                            acc.push_str(&format!("%{:02X}", b));
                        }
                        acc
                    });
                format!("data:{},{}", mime, encoded)
            } else {
                String::new()
            }
        });

    obj.insert(
        "blob_meta".to_string(),
        json!({
            "mime":       mime,
            "enc":        enc,
            "size_bytes": size_bytes,
            "render_as":  render_as,
            "blob_uri":   blob_uri,
        }),
    );
}

// ── GET /bus/viewer — self-contained HTML bus dashboard ──────────────────────
//
// No auth required: the dashboard is read-only and relies on the same bearer
// token that is already required by /bus/messages and /bus/stream.  The JS
// inside the page prompts for a token on first load and stores it in
// sessionStorage so that page refreshes don't require re-entry.
//
// Rendering follows SPEC.md §MIME Type Conventions:
//   image/*  → <img>
//   audio/*  → <audio controls>
//   video/*  → <video controls>
//   other    → <a download> fallback link

async fn bus_viewer() -> impl IntoResponse {
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>AgentBus Viewer</title>
<style>
  :root {
    --bg: #0d1117; --surface: #161b22; --border: #30363d;
    --text: #c9d1d9; --muted: #8b949e; --accent: #58a6ff;
    --green: #3fb950; --yellow: #d29922; --red: #f85149;
    --online: #3fb950; --offline: #6e7681;
    --image-bg: #0d1117; --media-radius: 8px;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { background: var(--bg); color: var(--text); font-family: 'Segoe UI', system-ui, sans-serif; font-size: 14px; height: 100vh; display: flex; flex-direction: column; }

  /* ── toolbar ── */
  #toolbar { background: var(--surface); border-bottom: 1px solid var(--border); padding: 10px 16px; display: flex; align-items: center; gap: 12px; flex-shrink: 0; }
  #toolbar h1 { font-size: 16px; font-weight: 600; white-space: nowrap; }
  #toolbar h1 span { color: var(--accent); }
  #filter-type { background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 6px; padding: 4px 8px; font-size: 13px; }
  #btn-clear  { margin-left: auto; background: none; border: 1px solid var(--border); border-radius: 6px; color: var(--muted); padding: 4px 10px; cursor: pointer; font-size: 12px; }
  #btn-clear:hover { border-color: var(--accent); color: var(--accent); }
  #status-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--offline); flex-shrink: 0; }
  #status-dot.connected { background: var(--online); }
  #status-label { font-size: 12px; color: var(--muted); }

  /* ── presence strip ── */
  #presence { background: var(--surface); border-bottom: 1px solid var(--border); padding: 6px 16px; display: flex; gap: 16px; flex-wrap: wrap; flex-shrink: 0; }
  .agent-pill { display: flex; align-items: center; gap: 5px; font-size: 12px; color: var(--muted); }
  .agent-pill .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--offline); }
  .agent-pill .dot.online { background: var(--online); }

  /* ── message list ── */
  #messages { flex: 1; overflow-y: auto; padding: 12px 16px; display: flex; flex-direction: column; gap: 8px; }

  /* ── individual message card ── */
  .msg { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 10px 14px; display: flex; flex-direction: column; gap: 6px; }
  .msg.type-blob { border-left: 3px solid var(--accent); }
  .msg-header { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
  .msg-from   { font-weight: 600; color: var(--accent); font-size: 13px; }
  .msg-to     { color: var(--muted); font-size: 12px; }
  .msg-type   { background: var(--bg); border: 1px solid var(--border); border-radius: 4px; padding: 1px 6px; font-size: 11px; color: var(--muted); }
  .msg-type.blob { color: var(--yellow); border-color: var(--yellow); }
  .msg-ts     { font-size: 11px; color: var(--muted); margin-left: auto; }
  .msg-subject { font-size: 12px; color: var(--muted); font-style: italic; }
  .msg-seq    { font-size: 11px; color: #484f58; }

  /* ── body rendering ── */
  .msg-body   { font-size: 13px; line-height: 1.5; word-break: break-word; }
  .msg-body pre { background: var(--bg); border: 1px solid var(--border); border-radius: 6px; padding: 8px 10px; overflow-x: auto; font-size: 12px; white-space: pre-wrap; }

  /* ── blob media rendering ── */
  .blob-media { margin-top: 4px; }
  .blob-media img { max-width: 100%; max-height: 480px; border-radius: var(--media-radius); background: var(--image-bg); display: block; object-fit: contain; }
  .blob-media audio { width: 100%; border-radius: var(--media-radius); }
  .blob-media video { max-width: 100%; max-height: 480px; border-radius: var(--media-radius); background: #000; display: block; }
  .blob-download { display: inline-flex; align-items: center; gap: 6px; padding: 6px 12px; background: var(--bg); border: 1px solid var(--border); border-radius: 6px; color: var(--accent); text-decoration: none; font-size: 12px; }
  .blob-download:hover { border-color: var(--accent); }
  .blob-meta  { margin-top: 4px; font-size: 11px; color: var(--muted); display: flex; gap: 10px; flex-wrap: wrap; }
  .blob-meta span { background: var(--bg); border: 1px solid var(--border); border-radius: 4px; padding: 1px 6px; }

  /* ── token prompt overlay ── */
  #token-overlay { position: fixed; inset: 0; background: rgba(0,0,0,.7); display: flex; align-items: center; justify-content: center; z-index: 100; }
  #token-box { background: var(--surface); border: 1px solid var(--border); border-radius: 10px; padding: 28px 32px; width: 380px; display: flex; flex-direction: column; gap: 14px; }
  #token-box h2 { font-size: 16px; }
  #token-box p  { font-size: 13px; color: var(--muted); line-height: 1.5; }
  #token-input  { background: var(--bg); color: var(--text); border: 1px solid var(--border); border-radius: 6px; padding: 8px 10px; font-size: 13px; outline: none; }
  #token-input:focus { border-color: var(--accent); }
  #token-submit { background: var(--accent); color: #0d1117; border: none; border-radius: 6px; padding: 8px 14px; font-weight: 600; cursor: pointer; font-size: 13px; }
  #token-submit:hover { opacity: .88; }
  #token-error  { font-size: 12px; color: var(--red); display: none; }
</style>
</head>
<body>

<!-- Token prompt -->
<div id="token-overlay">
  <div id="token-box">
    <h2>AgentBus Viewer</h2>
    <p>Enter your Bearer token to connect to the bus.  The token is kept only in <code>sessionStorage</code> and never sent anywhere except this server.</p>
    <input id="token-input" type="password" placeholder="Bearer token…" autocomplete="off">
    <div id="token-error">Invalid token — check and try again.</div>
    <button id="token-submit">Connect</button>
  </div>
</div>

<!-- Main UI -->
<div id="toolbar">
  <div id="status-dot"></div>
  <span id="status-label">Connecting…</span>
  <h1>Agent<span>Bus</span> Viewer</h1>
  <select id="filter-type">
    <option value="">All types</option>
    <option value="text">text</option>
    <option value="blob">blob</option>
    <option value="heartbeat">heartbeat</option>
    <option value="rcc.update">rcc.update</option>
    <option value="rcc.exec">rcc.exec</option>
    <option value="ping">ping</option>
    <option value="pong">pong</option>
    <option value="event">event</option>
    <option value="memo">memo</option>
    <option value="handoff">handoff</option>
  </select>
  <button id="btn-clear">Clear</button>
</div>

<div id="presence"></div>

<div id="messages"><p style="color:var(--muted);font-size:13px;">Waiting for messages…</p></div>

<script>
'use strict';

// ── state ──────────────────────────────────────────────────────────────────
let token = sessionStorage.getItem('bus_token') || '';
let evtSource = null;
let messages  = [];   // ordered newest-last
let filterType = '';

// ── DOM refs ───────────────────────────────────────────────────────────────
const overlay     = document.getElementById('token-overlay');
const tokenInput  = document.getElementById('token-input');
const tokenSubmit = document.getElementById('token-submit');
const tokenError  = document.getElementById('token-error');
const statusDot   = document.getElementById('status-dot');
const statusLabel = document.getElementById('status-label');
const presenceEl  = document.getElementById('presence');
const msgList     = document.getElementById('messages');
const filterSel   = document.getElementById('filter-type');
const btnClear    = document.getElementById('btn-clear');

// ── helpers ────────────────────────────────────────────────────────────────
function esc(s) {
  if (s === null || s === undefined) return '';
  return String(s)
    .replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;')
    .replace(/"/g,'&quot;');
}

function fmtTs(ts) {
  if (!ts) return '';
  try {
    const d = new Date(ts);
    return d.toLocaleTimeString([], {hour:'2-digit', minute:'2-digit', second:'2-digit'});
  } catch { return ts; }
}

function fmtBytes(n) {
  if (!n) return '0 B';
  if (n < 1024) return n + ' B';
  if (n < 1024*1024) return (n/1024).toFixed(1) + ' KB';
  return (n/1024/1024).toFixed(2) + ' MB';
}

// ── blob media renderer ───────────────────────────────────────────────────
//
// Follows SPEC.md §MIME Type Conventions exactly:
//   image/*  → <img src=blob_uri>
//   audio/*  → <audio controls src=blob_uri>
//   video/*  → <video controls src=blob_uri>
//   other    → <a href=blob_uri download> fallback link
//
// The blob_uri is already present on the message via blob_meta (injected by
// the server's enrich_blob_message()). We fall back to synthesising a
// data-URI from the raw body if blob_meta is missing for some reason.

function renderBlob(msg) {
  const meta = msg.blob_meta || {};
  const mime = meta.mime || msg.mime || 'application/octet-stream';
  const enc  = meta.enc  || msg.enc  || 'none';
  const renderAs = meta.render_as || inferRenderAs(mime);
  const sizeBytes = meta.size_bytes;

  // Resolve the blob URI: prefer blob_meta.blob_uri (set by storage layer),
  // then msg.blob_uri (set by sender), then synthesise from body.
  let blobUri = meta.blob_uri || msg.blob_uri || '';
  if (!blobUri) {
    const body = typeof msg.body === 'string' ? msg.body : JSON.stringify(msg.body);
    if (enc === 'base64' && body) {
      blobUri = `data:${mime};base64,${body}`;
    } else if (body) {
      blobUri = `data:${mime},${encodeURIComponent(body)}`;
    }
  }

  if (!blobUri) {
    return '<div class="blob-media"><em style="color:var(--muted)">No blob data</em></div>';
  }

  // Filename hint for download links
  const fname = msg.subject || msg.id || 'blob';

  let mediaHtml = '';
  if (renderAs === 'image') {
    mediaHtml = `<img src="${esc(blobUri)}" alt="${esc(fname)}" loading="lazy">`;
  } else if (renderAs === 'audio') {
    mediaHtml = `<audio controls preload="metadata"><source src="${esc(blobUri)}" type="${esc(mime)}">Your browser does not support the audio element.</audio>`;
  } else if (renderAs === 'video') {
    mediaHtml = `<video controls preload="metadata"><source src="${esc(blobUri)}" type="${esc(mime)}">Your browser does not support the video element.</video>`;
  } else {
    // Fallback download link for any other MIME type
    mediaHtml = `<a class="blob-download" href="${esc(blobUri)}" download="${esc(fname)}">⬇ Download ${esc(mime)}</a>`;
  }

  const metaBadges = [
    `<span>${esc(mime)}</span>`,
    sizeBytes ? `<span>${fmtBytes(sizeBytes)}</span>` : '',
    enc !== 'none' ? `<span>enc:${esc(enc)}</span>` : '',
  ].filter(Boolean).join('');

  return `<div class="blob-media">${mediaHtml}</div><div class="blob-meta">${metaBadges}</div>`;
}

function inferRenderAs(mime) {
  if (mime.startsWith('image/')) return 'image';
  if (mime.startsWith('audio/')) return 'audio';
  if (mime.startsWith('video/')) return 'video';
  return 'download';
}

// ── message card builder ──────────────────────────────────────────────────

function buildCard(msg) {
  const isBlob = msg.type === 'blob';
  const typeClass = isBlob ? 'blob' : '';

  // Body: for non-blob messages, render body as preformatted text
  let bodyHtml = '';
  if (isBlob) {
    bodyHtml = renderBlob(msg);
  } else {
    const raw = msg.body !== undefined
      ? (typeof msg.body === 'string' ? msg.body : JSON.stringify(msg.body, null, 2))
      : '';
    if (raw) {
      bodyHtml = `<div class="msg-body"><pre>${esc(raw)}</pre></div>`;
    }
  }

  const subject = msg.subject
    ? `<div class="msg-subject">${esc(msg.subject)}</div>` : '';

  return `
<div class="msg${isBlob ? ' type-blob' : ''}">
  <div class="msg-header">
    <span class="msg-from">${esc(msg.from || '?')}</span>
    <span class="msg-to">→ ${esc(msg.to || 'all')}</span>
    <span class="msg-type ${typeClass}">${esc(msg.type || 'text')}</span>
    <span class="msg-ts">${fmtTs(msg.ts)}</span>
    ${msg.seq !== undefined ? `<span class="msg-seq">#${msg.seq}</span>` : ''}
  </div>
  ${subject}
  ${bodyHtml}
</div>`.trim();
}

// ── rendering ─────────────────────────────────────────────────────────────

function renderMessages() {
  const filtered = filterType
    ? messages.filter(m => m.type === filterType)
    : messages;

  if (filtered.length === 0) {
    msgList.innerHTML = '<p style="color:var(--muted);font-size:13px;">No messages' +
      (filterType ? ` of type <strong>${esc(filterType)}</strong>` : '') + '.</p>';
    return;
  }

  // Render newest at bottom; use DocumentFragment for one DOM update
  const frag = document.createDocumentFragment();
  for (const msg of filtered) {
    const div = document.createElement('div');
    div.innerHTML = buildCard(msg);
    frag.appendChild(div.firstChild);
  }
  msgList.innerHTML = '';
  msgList.appendChild(frag);
  // Scroll to bottom
  msgList.scrollTop = msgList.scrollHeight;
}

function addMessage(msg) {
  if (!msg || msg.type === 'connected') return;
  messages.push(msg);
  // Cap at 500 in memory
  if (messages.length > 500) messages.shift();
  renderMessages();
}

// ── presence ──────────────────────────────────────────────────────────────

async function refreshPresence() {
  if (!token) return;
  try {
    const r = await fetch('/bus/presence', { headers: { Authorization: `Bearer ${token}` } });
    if (!r.ok) return;
    const data = await r.json();
    const pills = Object.entries(data).map(([name, info]) => {
      const online = info.status === 'online';
      return `<div class="agent-pill"><div class="dot${online ? ' online' : ''}"></div>${esc(name)}</div>`;
    }).join('');
    presenceEl.innerHTML = pills || '<span style="color:var(--muted);font-size:12px;">No agents registered</span>';
  } catch { /* ignore */ }
}

// ── SSE connection ────────────────────────────────────────────────────────

function connect() {
  if (evtSource) { evtSource.close(); evtSource = null; }

  statusDot.className   = '';
  statusLabel.textContent = 'Connecting…';

  // Load historical messages first, then open SSE stream
  fetch(`/bus/messages?limit=200`, { headers: { Authorization: `Bearer ${token}` } })
    .then(r => {
      if (r.status === 401) { showTokenPrompt('Invalid token.'); return []; }
      return r.ok ? r.json() : [];
    })
    .then(data => {
      if (!Array.isArray(data)) return;
      messages = data;
      renderMessages();
    })
    .catch(() => {});

  evtSource = new EventSource(`/bus/stream?token=${encodeURIComponent(token)}`);
  // Note: EventSource doesn't support custom headers; for the SSE endpoint we
  // pass the token via query-string.  The server accepts Bearer from
  // Authorization header on all other endpoints; SSE callers use ?token= as
  // a workaround consistent with browser EventSource limitations.

  evtSource.onopen = () => {
    statusDot.className   = 'connected';
    statusLabel.textContent = 'Connected';
    refreshPresence();
  };

  evtSource.onmessage = (e) => {
    try {
      const msg = JSON.parse(e.data);
      addMessage(msg);
    } catch { /* bad frame */ }
  };

  evtSource.onerror = () => {
    statusDot.className   = '';
    statusLabel.textContent = 'Reconnecting…';
  };
}

// ── token prompt ──────────────────────────────────────────────────────────

function showTokenPrompt(errMsg) {
  overlay.style.display = 'flex';
  if (errMsg) { tokenError.textContent = errMsg; tokenError.style.display = 'block'; }
}

function hideTokenPrompt() { overlay.style.display = 'none'; }

tokenSubmit.addEventListener('click', () => {
  const t = tokenInput.value.trim();
  if (!t) { tokenError.textContent = 'Token cannot be empty.'; tokenError.style.display = 'block'; return; }
  token = t;
  sessionStorage.setItem('bus_token', token);
  tokenError.style.display = 'none';
  hideTokenPrompt();
  connect();
});

tokenInput.addEventListener('keydown', e => { if (e.key === 'Enter') tokenSubmit.click(); });

// ── filter + clear ────────────────────────────────────────────────────────

filterSel.addEventListener('change', () => { filterType = filterSel.value; renderMessages(); });
btnClear.addEventListener('click', () => { messages = []; renderMessages(); });

// ── presence refresh ──────────────────────────────────────────────────────
setInterval(refreshPresence, 30_000);

// ── boot ──────────────────────────────────────────────────────────────────
if (token) { hideTokenPrompt(); connect(); }
</script>
</body>
</html>"#;

    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
}

async fn bus_presence(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let agents = state.agents.read().await;
    let now = chrono::Utc::now();

    let mut presence = serde_json::Map::new();
    if let Some(obj) = agents.as_object() {
        for (name, agent) in obj {
            let last_seen_str = agent.get("last_seen")
                .or_else(|| agent.get("lastSeen"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let online = chrono::DateTime::parse_from_rfc3339(last_seen_str)
                .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_seconds() < 600)
                .unwrap_or(false);
            presence.insert(name.clone(), json!({
                "status": if online { "online" } else { "offline" },
                "last_seen": last_seen_str,
            }));
        }
    }
    Json(Value::Object(presence)).into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Initialize the bus sequence counter from the log on startup.
///
/// Reads the last-written message's `seq` field and returns `seq + 1` so
/// the next assigned sequence continues monotonically across server
/// restarts. Previously `bus_seq` was hard-initialized to 0, which caused
/// msg-id collisions with existing log entries every restart and made
/// "newest seq" a meaningless ordering signal.
pub fn initial_bus_seq(path: &str) -> u64 {
    let Ok(content) = std::fs::read_to_string(path) else { return 0 };
    for line in content.lines().rev() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(seq) = v.get("seq").and_then(|s| s.as_u64()) {
                return seq + 1;
            }
        }
    }
    0
}

async fn load_bus_messages(path: &str, limit: usize, q: &BusQuery) -> Vec<String> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // Walk the log newest-first, apply filters, take up to `limit` matching
    // messages, then reverse to chronological (oldest-first) for the caller.
    //
    // The previous implementation did `.rev().take(limit*4).rev().take(limit)`,
    // which returns the *oldest* `limit` messages in the most recent
    // `limit*4` line window — i.e. a mid-log slice, not the tail. On a log
    // with N > limit*4 lines, SSE replay and `/bus/messages` both silently
    // served 20-day-old data while live writes continued at the tail.
    let mut matched: Vec<String> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .filter(|line| {
            let Ok(v) = serde_json::from_str::<Value>(line) else { return false };

            if let Some(subj) = &q.subject {
                if v.get("subject").and_then(|s| s.as_str()) != Some(subj.as_str()) {
                    return false;
                }
            }
            if let Some(t) = &q.msg_type {
                if v.get("type").and_then(|s| s.as_str()) != Some(t.as_str()) {
                    return false;
                }
            }
            if let Some(tid) = &q.thread_id {
                if v.get("thread_id").and_then(|s| s.as_str()) != Some(tid.as_str()) {
                    return false;
                }
            }
            if let Some(to_user) = &q.to {
                let msg_to = v.get("to").and_then(|s| s.as_str()).unwrap_or("");
                let msg_from = v.get("from").and_then(|s| s.as_str()).unwrap_or("");
                let from_user = q.from.as_deref().unwrap_or("");
                if !((msg_to == to_user && msg_from == from_user)
                    || (msg_to == from_user && msg_from == to_user))
                {
                    return false;
                }
            }
            if let Some(since) = &q.since {
                let msg_ts = v.get("ts").and_then(|s| s.as_str()).unwrap_or("");
                if msg_ts <= since.as_str() {
                    return false;
                }
            }
            if let Some(uid) = &q.upload_id {
                if v.get("upload_id").and_then(|s| s.as_str()) != Some(uid.as_str()) {
                    return false;
                }
            }
            true
        })
        .take(limit)
        .map(|s| s.to_string())
        .collect();

    // Reverse back so callers get chronological order (oldest → newest).
    matched.reverse();
    matched
}

async fn append_line(path: &str, line: &str) -> std::io::Result<()> {
    use tokio::fs::OpenOptions;
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(line.as_bytes()).await?;
    Ok(())
}
