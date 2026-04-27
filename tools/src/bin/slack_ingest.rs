//! Slack channel ingestion to Qdrant.
//! Port of scripts/slack-channel-ingest.py

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::path::PathBuf;

const COLLECTION: &str = "holographic_memory";
const EMBED_DIM: u64 = 3072;
const MAX_MSGS: u64 = 50;

struct Channel {
    name: &'static str,
    id: &'static str,
}

const CHANNELS: &[Channel] = &[
    Channel { name: "rockyandfriends", id: "C0AMNRSN9EZ" },
    Channel { name: "project-ccc", id: "C0ANY3AGW4Q" },
];

#[tokio::main]
async fn main() {
    acc_tools::load_acc_env();

    let slack_token = std::env::var("SLACK_OMGJKH_TOKEN").unwrap_or_default();
    if slack_token.is_empty() {
        println!("{}", json!({"error": "No SLACK_OMGJKH_TOKEN found"}));
        std::process::exit(1);
    }

    let qdrant_url = std::env::var("QDRANT_URL")
        .unwrap_or_else(|_| "http://localhost:6333".to_string());
    let qdrant_key = acc_tools::resolve_qdrant_api_key();
    let qdrant = acc_qdrant::QdrantClient::new(&qdrant_url, qdrant_key.as_deref())
        .expect("qdrant client");

    // Ensure collection exists
    if let Err(e) = qdrant
        .ensure_collection(COLLECTION, EMBED_DIM, &["source", "channel", "user"])
        .await
    {
        eprintln!("Collection setup error: {e}");
    }

    let embed = match acc_tools::make_embed_client() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Embed error: {e}");
            std::process::exit(1);
        }
    };

    let http = reqwest::Client::new();
    let watermark_dir = watermark_dir();
    std::fs::create_dir_all(&watermark_dir).ok();

    let mut report = json!({"channels": {}, "total_ingested": 0, "errors": []});
    let mut total: u64 = 0;

    for ch in CHANNELS {
        let watermark = get_watermark(&watermark_dir, ch.name);
        let messages = fetch_messages(&http, ch.id, &watermark, &slack_token).await;
        if messages.is_empty() {
            report["channels"][ch.name] = json!({"new": 0, "ingested": 0});
            continue;
        }

        let chunks: Vec<(u64, String, Value)> = messages
            .iter()
            .filter_map(|msg| format_message(msg, ch.name))
            .collect();

        if chunks.is_empty() {
            report["channels"][ch.name] =
                json!({"new": messages.len(), "ingested": 0, "note": "all filtered"});
            continue;
        }

        let texts: Vec<&str> = chunks.iter().map(|(_, t, _)| t.as_str()).collect();
        let vectors = match embed.embed(&texts).await {
            Ok(v) => v,
            Err(e) => {
                report["errors"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(format!("{}: embed error: {e}", ch.name)));
                continue;
            }
        };

        let points: Vec<Value> = chunks
            .iter()
            .zip(vectors.iter())
            .map(|((id, _text, payload), vec)| {
                json!({"id": id, "vector": vec, "payload": payload})
            })
            .collect();

        if let Err(e) = qdrant.upsert_points_raw(COLLECTION, points).await {
            report["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(format!("{}: upsert error: {e}", ch.name)));
            continue;
        }

        let upserted = chunks.len() as u64;
        let max_ts = messages
            .iter()
            .filter_map(|m| m["ts"].as_str())
            .max()
            .unwrap_or("0")
            .to_string();
        set_watermark(&watermark_dir, ch.name, &max_ts);

        report["channels"][ch.name] = json!({"new": messages.len(), "ingested": upserted});
        total += upserted;
    }

    report["total_ingested"] = json!(total);
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

fn watermark_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    for dir in [".acc", ".ccc"] {
        let base = PathBuf::from(&home).join(dir);
        if base.exists() {
            return base.join("watermarks");
        }
    }
    PathBuf::from(&home).join(".acc").join("watermarks")
}

fn get_watermark(dir: &PathBuf, channel: &str) -> String {
    let path = dir.join(format!("{channel}.ts"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| "0".to_string())
        .trim()
        .to_string()
}

fn set_watermark(dir: &PathBuf, channel: &str, ts: &str) {
    let path = dir.join(format!("{channel}.ts"));
    let _ = std::fs::write(&path, ts);
}

async fn fetch_messages(
    http: &reqwest::Client,
    channel_id: &str,
    oldest: &str,
    token: &str,
) -> Vec<Value> {
    let url = format!(
        "https://slack.com/api/conversations.history?channel={channel_id}&oldest={oldest}&limit={MAX_MSGS}"
    );
    match http.get(&url).bearer_auth(token).send().await {
        Ok(r) => {
            let data: Value = r.json().await.unwrap_or(json!({}));
            if data["ok"].as_bool().unwrap_or(false) {
                data["messages"].as_array().cloned().unwrap_or_default()
            } else {
                vec![]
            }
        }
        Err(_) => vec![],
    }
}

fn format_message(msg: &Value, channel_name: &str) -> Option<(u64, String, Value)> {
    let user = msg["user"]
        .as_str()
        .or_else(|| msg["username"].as_str())
        .unwrap_or("unknown");
    let ts = msg["ts"].as_str().unwrap_or("0");
    let text = msg["text"].as_str().unwrap_or("");
    if text.trim().len() < 10 {
        return None;
    }

    let ts_f: f64 = ts.parse().unwrap_or(0.0);
    let dt = DateTime::from_timestamp(ts_f as i64, 0).unwrap_or_else(Utc::now);
    let date_str = dt.format("%Y-%m-%d %H:%M UTC").to_string();
    let formatted = format!("[#{channel_name} {date_str}] {user}: {text}");

    let key = format!("{channel_name}:{ts}");
    let hash = md5::compute(key.as_bytes());
    let hex = format!("{:x}", hash);
    let id = u64::from_str_radix(&hex[..16], 16).unwrap_or(0) & 0x7FFFFFFFFFFFFFFF;

    let payload = json!({
        "source": "slack",
        "channel": channel_name,
        "channel_name": channel_name,
        "user": user,
        "ts": ts,
        "date": date_str,
        "type": "chat_message",
        "text": &formatted,
    });

    Some((id, formatted, payload))
}
