//! Consolidated log viewer (CCC-zkc).
//!
//! Spawns `journalctl -f` server-side filtered to ACC components and streams
//! the output as Server-Sent Events to the dashboard. Linux-only by design;
//! on macOS dev hub, the SSE stream returns an "unsupported" record and
//! closes — operator gets a clear signal instead of dead silence.
//!
//! GET /api/logs/stream
//!     ?identifier=<name>  optional comma-separated journalctl `-t` filter.
//!                         Defaults to all known acc-agent-* + acc-server +
//!                         hermes identifiers.
//!     ?lines=<n>          backfill last N matching journal lines before
//!                         tailing live (default 50, max 500).
//!
//! Each SSE event is a JSON object: { ts, identifier, host, level, msg }
//! Fields come from journalctl's --output=json (selected and renamed).

use crate::AppState;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/logs/stream", get(logs_stream))
}

#[derive(Deserialize)]
struct LogsQuery {
    identifier: Option<String>,
    lines: Option<usize>,
}

const DEFAULT_IDENTIFIERS: &[&str] = &[
    "acc-server",
    "acc-agent-tasks",
    "acc-agent-queue",
    "acc-agent-bus",
    "acc-agent-hermes",
    "acc-agent-supervise",
    "acc-agent-proxy",
    "hermes",
];

async fn logs_stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<LogsQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    let lines = q.lines.unwrap_or(50).min(500);

    let identifiers: Vec<String> = q
        .identifier
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_IDENTIFIERS.iter().map(|s| s.to_string()).collect());

    // Linux-only: spawn journalctl. On macOS or systems without journald
    // we return a single SSE record explaining and close cleanly.
    let stream = build_stream(identifiers, lines);
    Sse::new(stream)
        .keep_alive(KeepAlive::new().text("ping"))
        .into_response()
}

fn build_stream(
    identifiers: Vec<String>,
    backfill_lines: usize,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        // Probe: is journalctl available?
        let journalctl_ok = Command::new("journalctl")
            .arg("--version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !journalctl_ok {
            let payload = json!({
                "level": "warn",
                "ts": chrono::Utc::now().to_rfc3339(),
                "identifier": "acc-server",
                "msg": "journalctl unavailable on this host (likely macOS or non-systemd Linux); log aggregation requires Linux+systemd.",
            });
            yield Ok(Event::default().data(payload.to_string()));
            return;
        }

        // Build the journalctl args: -t <id> for each identifier, plus
        // -f to follow, --output=json so we can extract structured fields,
        // -n <lines> for backfill.
        let mut args: Vec<String> = vec![
            "-f".into(),
            "--output=json".into(),
            "-n".into(), backfill_lines.to_string(),
        ];
        for id in &identifiers {
            args.push("-t".into());
            args.push(id.clone());
        }

        let mut child = match Command::new("journalctl")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let payload = json!({
                    "level": "error",
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "identifier": "acc-server",
                    "msg": format!("failed to spawn journalctl: {e}"),
                });
                yield Ok(Event::default().data(payload.to_string()));
                return;
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => return,
        };
        let mut reader = BufReader::new(stdout).lines();

        // Send a connected control event so the client knows the stream
        // is live.
        yield Ok(Event::default().data(
            json!({"type":"connected","identifiers":identifiers}).to_string()
        ));

        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    // journalctl JSON format has many fields; pick the ones
                    // the dashboard cares about and emit a compact record.
                    let raw: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let msg = raw.get("MESSAGE").and_then(|v| v.as_str()).unwrap_or("");
                    let ident = raw.get("SYSLOG_IDENTIFIER")
                        .and_then(|v| v.as_str())
                        .or_else(|| raw.get("_SYSTEMD_UNIT").and_then(|v| v.as_str()))
                        .unwrap_or("");
                    let host = raw.get("_HOSTNAME").and_then(|v| v.as_str()).unwrap_or("");
                    let pri = raw.get("PRIORITY").and_then(|v| v.as_str()).unwrap_or("6");
                    // journald PRIORITY: 0=emerg .. 7=debug; map to text the
                    // dashboard can color-code.
                    let level = match pri {
                        "0" | "1" | "2" | "3" => "error",
                        "4" => "warn",
                        "5" => "notice",
                        "6" => "info",
                        _ => "debug",
                    };
                    // _SOURCE_REALTIME_TIMESTAMP is microseconds since epoch
                    let ts_us = raw.get("_SOURCE_REALTIME_TIMESTAMP")
                        .or_else(|| raw.get("__REALTIME_TIMESTAMP"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0);
                    let ts = if ts_us > 0 {
                        chrono::DateTime::from_timestamp(ts_us / 1_000_000, 0)
                            .map(|dt| dt.to_rfc3339())
                            .unwrap_or_default()
                    } else {
                        chrono::Utc::now().to_rfc3339()
                    };
                    let payload = json!({
                        "ts": ts,
                        "level": level,
                        "identifier": ident,
                        "host": host,
                        "msg": msg,
                    });
                    yield Ok(Event::default().data(payload.to_string()));
                }
                Ok(None) => break, // journalctl exited
                Err(_) => break,
            }
        }
        // Reap child so we don't leave zombies
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
