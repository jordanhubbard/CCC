//! Slack-to-Qdrant memory ingestion service.
//!
//! For every (workspace, bot) pair whose bot token lives in the secret
//! store under `slack/{ws}/{bot}/bot-token`, this service:
//!
//!   1. Validates the token via `auth.test`.
//!   2. Lists every conversation that bot is a member of via
//!      `users.conversations`.
//!   3. Fetches new messages via `conversations.history` using a
//!      per-(workspace, channel) timestamp watermark on disk.
//!   4. Embeds each message and upserts to a single Qdrant collection
//!      (`holographic_memory`) with a `workspace` field on the payload so
//!      callers can filter or search across workspaces.
//!
//! Runs in a steady-state loop with a configurable poll interval (default
//! 5 minutes). Pass `--once` to run a single cycle and exit; useful for
//! cron-style operation or for first-bring-up.
//!
//! Designed to run on the hub host only (where Qdrant is local and the
//! bot tokens already live in the vault). The supervisor's
//! `slack_ingest_enabled` predicate gates this on `IS_HUB=true`.

use crate::config::Config;
use acc_client::Client;
use acc_qdrant::{EmbedClient, QdrantClient};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(300); // 5 min
const HISTORY_LIMIT: u32 = 100;
const CHANNELS_PAGE_LIMIT: u32 = 200;
const MIN_TEXT_LEN: usize = 10;

// Light pacing — tightened back from the 5s the restrictive
// `text-embedding-3-large` tier required, since the default model now
// (`azure/openai/text-embedding-3-small`) handles back-to-back bursts
// cleanly. Retry-with-backoff stays as defense-in-depth so the service
// degrades gracefully if the operator points NVIDIA_EMBED_MODEL back at
// a tighter-quota model.
const INTER_CHANNEL_DELAY: Duration = Duration::from_millis(150);
const EMBED_RETRY_429_DELAYS: &[Duration] = &[
    Duration::from_secs(2),
    Duration::from_secs(8),
    Duration::from_secs(30),
];

/// Resolve the embedding dimension from env (`EMBED_DIM` or
/// `NVIDIA_EMBED_DIM`); defaults to 1536, the dimension of
/// `text-embedding-3-small`.
fn resolve_embed_dim() -> u64 {
    std::env::var("EMBED_DIM")
        .or_else(|_| std::env::var("NVIDIA_EMBED_DIM"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1536)
}

/// Resolve the Qdrant collection name. Defaults to
/// `holographic_memory_<dim>` so a model swap that changes vector
/// dimension lands in a fresh collection rather than colliding with
/// the old one. Override via `SLACK_INGEST_COLLECTION` for ad-hoc
/// experiments.
fn resolve_collection_name(embed_dim: u64) -> String {
    std::env::var("SLACK_INGEST_COLLECTION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("holographic_memory_{embed_dim}"))
}

/// Workspaces and bots checked in every cycle. The service silently skips
/// any (workspace, bot) pair that does not have a `bot-token` secret
/// stored, so adding or removing bots is a vault operation, not a code
/// change.
const WORKSPACES: &[&str] = &["omgjkh", "offtera"];
const BOTS: &[&str] = &[
    "rocky",
    "boris",
    "natasha",
    "bullwinkle",
    "peabody",
    "sherman",
];

pub async fn run(args: &[String]) {
    let once = args.iter().any(|a| a == "--once");

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[slack-ingest] config error: {e}");
            std::process::exit(1);
        }
    };

    let client = Client::new(&cfg.acc_url, &cfg.acc_token).expect("acc client");

    let qdrant_url = std::env::var("QDRANT_URL")
        .unwrap_or_else(|_| "http://localhost:6333".to_string());
    let qdrant_key = acc_tools::resolve_qdrant_api_key();
    let qdrant = match QdrantClient::new(&qdrant_url, qdrant_key.as_deref()) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("[slack-ingest] qdrant client: {e}");
            std::process::exit(1);
        }
    };

    let embed_dim = resolve_embed_dim();
    let collection = resolve_collection_name(embed_dim);
    if let Err(e) = qdrant
        .ensure_collection(
            &collection,
            embed_dim,
            &["source", "workspace", "channel", "user"],
        )
        .await
    {
        eprintln!("[slack-ingest] ensure_collection: {e}");
    }

    let embed = match acc_tools::make_embed_client() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[slack-ingest] embed client: {e}");
            std::process::exit(1);
        }
    };

    let watermark_dir = cfg.acc_dir.join("watermarks");
    std::fs::create_dir_all(&watermark_dir).ok();

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client");

    eprintln!(
        "[slack-ingest] starting (interval={:?}, qdrant={}, collection={}, dim={})",
        POLL_INTERVAL, qdrant_url, collection, embed_dim
    );

    loop {
        let started = std::time::Instant::now();
        let stats =
            run_cycle(&client, &qdrant, &collection, &embed, &http, &watermark_dir).await;
        eprintln!(
            "[slack-ingest] cycle done: pairs={} channels={} ingested={} elapsed={:?}",
            stats.pairs_attempted, stats.channels_visited, stats.messages_ingested, started.elapsed()
        );
        if once {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[derive(Default)]
struct CycleStats {
    pairs_attempted: u32,
    channels_visited: u32,
    messages_ingested: u32,
}

async fn run_cycle(
    client: &Client,
    qdrant: &QdrantClient,
    collection: &str,
    embed: &EmbedClient,
    http: &reqwest::Client,
    watermark_dir: &Path,
) -> CycleStats {
    let mut stats = CycleStats::default();

    for ws in WORKSPACES {
        for bot in BOTS {
            let token_key = format!("slack/{ws}/{bot}/bot-token");
            let bot_token = match client.secrets().get(&token_key).await {
                Ok(Some(t)) => t.trim().to_string(),
                Ok(None) => continue, // No token for this pair → skip silently.
                Err(e) => {
                    eprintln!("[slack-ingest] {ws}/{bot}: secret fetch: {e}");
                    continue;
                }
            };
            if bot_token.is_empty() {
                continue;
            }
            stats.pairs_attempted += 1;

            let bot_user_id = match auth_test(http, &bot_token).await {
                Some(uid) => uid,
                None => {
                    eprintln!("[slack-ingest] {ws}/{bot}: auth.test failed");
                    continue;
                }
            };

            let channels = match list_user_channels(http, &bot_token, &bot_user_id).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[slack-ingest] {ws}/{bot}: users.conversations: {e}");
                    continue;
                }
            };

            for ch in &channels {
                let ch_id = ch["id"].as_str().unwrap_or("");
                if ch_id.is_empty() {
                    continue;
                }
                // DMs / MPDMs come back without a `name` field; fall back to
                // the channel ID so log lines and payloads still identify
                // the conversation.
                let ch_name = ch["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(ch_id);
                stats.channels_visited += 1;

                let wm_key = format!("{ws}.{ch_id}");
                let watermark = read_watermark(watermark_dir, &wm_key);
                let messages = fetch_messages(http, ch_id, &watermark, &bot_token).await;
                if messages.is_empty() {
                    continue;
                }

                let chunks: Vec<(u64, String, Value)> = messages
                    .iter()
                    .filter_map(|m| format_message(m, ws, ch_name))
                    .collect();

                // Always advance the watermark even when every message in
                // the page filtered out, otherwise we re-fetch the same
                // empty page forever.
                let max_ts = messages
                    .iter()
                    .filter_map(|m| m["ts"].as_str())
                    .max()
                    .unwrap_or("0")
                    .to_string();

                if chunks.is_empty() {
                    write_watermark(watermark_dir, &wm_key, &max_ts);
                    continue;
                }

                let texts: Vec<&str> = chunks.iter().map(|(_, t, _)| t.as_str()).collect();
                let vectors = match embed_with_retry(embed, &texts).await {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[slack-ingest] {ws}/{bot}/{ch_name}: embed: {e}");
                        continue; // Don't advance watermark; retry next cycle.
                    }
                };

                let points: Vec<Value> = chunks
                    .iter()
                    .zip(vectors.iter())
                    .map(|((id, _t, payload), vec)| {
                        json!({"id": id, "vector": vec, "payload": payload})
                    })
                    .collect();

                if let Err(e) = qdrant.upsert_points_raw(collection, points).await {
                    eprintln!("[slack-ingest] {ws}/{bot}/{ch_name}: upsert: {e}");
                    continue; // Don't advance watermark.
                }

                write_watermark(watermark_dir, &wm_key, &max_ts);
                stats.messages_ingested += chunks.len() as u32;
                eprintln!(
                    "[slack-ingest] {ws}/{bot}/{ch_name}: +{} messages",
                    chunks.len()
                );
            }
            // Pace the embed endpoint between every channel that produced a
            // history page (not just the ones that ingested), since even a
            // page that filtered to zero rows still consumed a Slack call.
            tokio::time::sleep(INTER_CHANNEL_DELAY).await;
        }
    }

    stats
}

async fn embed_with_retry(
    embed: &EmbedClient,
    texts: &[&str],
) -> Result<Vec<Vec<f32>>, acc_qdrant::QdrantError> {
    let mut last_err: Option<acc_qdrant::QdrantError> = None;
    for (attempt, delay) in std::iter::once(&Duration::ZERO)
        .chain(EMBED_RETRY_429_DELAYS.iter())
        .enumerate()
    {
        if !delay.is_zero() {
            tokio::time::sleep(*delay).await;
        }
        match embed.embed(texts).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let s = e.to_string();
                if attempt < EMBED_RETRY_429_DELAYS.len() && s.contains("429") {
                    last_err = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        acc_qdrant::QdrantError::Config("embed_with_retry exhausted".to_string())
    }))
}

// ── Slack API helpers ─────────────────────────────────────────────────────────

async fn auth_test(http: &reqwest::Client, token: &str) -> Option<String> {
    let resp: Value = http
        .get("https://slack.com/api/auth.test")
        .bearer_auth(token)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if !resp["ok"].as_bool().unwrap_or(false) {
        return None;
    }
    Some(resp["user_id"].as_str()?.to_string())
}

async fn list_user_channels(
    http: &reqwest::Client,
    token: &str,
    user_id: &str,
) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut cursor = String::new();
    loop {
        let mut url = format!(
            "https://slack.com/api/users.conversations?user={user_id}\
             &types=public_channel,private_channel,mpim,im\
             &exclude_archived=true&limit={CHANNELS_PAGE_LIMIT}"
        );
        if !cursor.is_empty() {
            url.push_str("&cursor=");
            url.push_str(&cursor);
        }
        let resp: Value = http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| format!("http: {e}"))?
            .json()
            .await
            .map_err(|e| format!("parse: {e}"))?;
        if !resp["ok"].as_bool().unwrap_or(false) {
            return Err(format!(
                "slack: {}",
                resp["error"].as_str().unwrap_or("unknown")
            ));
        }
        if let Some(arr) = resp["channels"].as_array() {
            out.extend_from_slice(arr);
        }
        let next = resp["response_metadata"]["next_cursor"]
            .as_str()
            .unwrap_or("");
        if next.is_empty() {
            break;
        }
        cursor = next.to_string();
    }
    Ok(out)
}

async fn fetch_messages(
    http: &reqwest::Client,
    channel_id: &str,
    oldest: &str,
    token: &str,
) -> Vec<Value> {
    let url = format!(
        "https://slack.com/api/conversations.history?channel={channel_id}\
         &oldest={oldest}&limit={HISTORY_LIMIT}"
    );
    match http.get(&url).bearer_auth(token).send().await {
        Ok(r) => {
            let data: Value = r.json().await.unwrap_or(json!({}));
            if data["ok"].as_bool().unwrap_or(false) {
                data["messages"].as_array().cloned().unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        Err(_) => Vec::new(),
    }
}

// ── Message formatting ────────────────────────────────────────────────────────

fn format_message(msg: &Value, workspace: &str, channel_name: &str) -> Option<(u64, String, Value)> {
    let user = msg["user"]
        .as_str()
        .or_else(|| msg["username"].as_str())
        .unwrap_or("unknown");
    let ts = msg["ts"].as_str().unwrap_or("0");
    let text = msg["text"].as_str().unwrap_or("");
    if text.trim().len() < MIN_TEXT_LEN {
        return None;
    }

    let ts_f: f64 = ts.parse().unwrap_or(0.0);
    let dt = DateTime::from_timestamp(ts_f as i64, 0).unwrap_or_else(Utc::now);
    let date_str = dt.format("%Y-%m-%d %H:%M UTC").to_string();
    let formatted = format!(
        "[{workspace}/#{channel_name} {date_str}] {user}: {text}"
    );

    // Deterministic 63-bit ID from (workspace, channel, ts) so a re-fetch
    // of the same message overwrites in-place rather than duplicating.
    let key = format!("{workspace}:{channel_name}:{ts}");
    let hash = md5::compute(key.as_bytes());
    let hex = format!("{:x}", hash);
    let id = u64::from_str_radix(&hex[..16], 16).unwrap_or(0) & 0x7FFF_FFFF_FFFF_FFFF;

    let payload = json!({
        "source":       "slack",
        "workspace":    workspace,
        "channel":      channel_name,
        "channel_name": channel_name,
        "user":         user,
        "ts":           ts,
        "date":         date_str,
        "type":         "chat_message",
        "text":         &formatted,
    });

    Some((id, formatted, payload))
}

// ── Watermark ─────────────────────────────────────────────────────────────────

fn read_watermark(dir: &Path, key: &str) -> String {
    let path = dir.join(format!("{key}.ts"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| "0".to_string())
        .trim()
        .to_string()
}

fn write_watermark(dir: &Path, key: &str, ts: &str) {
    let path: PathBuf = dir.join(format!("{key}.ts"));
    let _ = std::fs::write(&path, ts);
}
