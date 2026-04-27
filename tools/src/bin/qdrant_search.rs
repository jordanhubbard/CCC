//! Qdrant semantic search CLI.
//! Port of scripts/qdrant-python/qdrant_search.py

use acc_qdrant::QdrantClient;
use clap::Parser;
use serde_json::{json, Value};

const COLLECTION_SESSIONS: &str = "hermes_sessions";
const COLLECTION_MEMORIES: &str = "agent_memories";
const COLLECTIONS: &[&str] = &[COLLECTION_SESSIONS, COLLECTION_MEMORIES, "slack_history"];

#[derive(Parser)]
#[command(about = "Search Qdrant vector collections for Hermes agent data")]
struct Args {
    /// Search query text
    query: Option<String>,
    /// Collection to search (default: search all)
    #[arg(short, long)]
    collection: Option<String>,
    /// Max results per collection
    #[arg(short = 'n', long, default_value = "5")]
    limit: u64,
    /// Show collection stats
    #[arg(long)]
    stats: bool,
    /// Filter by agent name
    #[arg(long)]
    agent: Option<String>,
    /// Filter by source
    #[arg(long)]
    source: Option<String>,
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() {
    acc_tools::load_acc_env();
    let args = Args::parse();

    let qdrant_url =
        std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_string());
    let api_key = acc_tools::resolve_qdrant_api_key();
    let qdrant =
        QdrantClient::new(&qdrant_url, api_key.as_deref()).expect("qdrant client");

    if args.stats {
        show_stats(&qdrant).await;
        return;
    }

    let query = match args.query {
        Some(q) => q,
        None => {
            eprintln!("Usage: qdrant-search <query> [options]\n       qdrant-search --stats");
            std::process::exit(1);
        }
    };

    let embed = match acc_tools::make_embed_client() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Embed client error: {e}");
            std::process::exit(1);
        }
    };

    let vector = match embed.embed(&[query.as_str()]).await {
        Ok(mut v) => v.remove(0),
        Err(e) => {
            eprintln!("Embedding error: {e}");
            std::process::exit(1);
        }
    };

    // Build filter
    let filter = build_filter(args.agent.as_deref(), args.source.as_deref());

    // Determine collections to search
    let collections: Vec<String> = if let Some(c) = args.collection {
        vec![c]
    } else {
        let mut found = Vec::new();
        for name in COLLECTIONS {
            if qdrant.collection_point_count(name).await > 0 {
                found.push(name.to_string());
            }
        }
        found
    };

    if !args.json {
        println!("=== Searching: \"{}\" ===", query);
        println!("  Collections: {}", collections.join(", "));
    }

    let mut all_results: Vec<(String, Vec<Value>)> = Vec::new();

    for coll in &collections {
        match qdrant
            .search_points(coll, &vector, args.limit, filter.clone())
            .await
        {
            Ok(hits) => {
                if !args.json {
                    for (i, hit) in hits.iter().enumerate() {
                        println!(
                            "\n--- Result {} (score: {:.4}) [{}] ---",
                            i + 1,
                            hit.score,
                            coll
                        );
                        display_result(&hit.payload, coll);
                    }
                }
                let results: Vec<Value> = hits
                    .iter()
                    .map(|h| json!({"score": h.score, "payload": h.payload}))
                    .collect();
                all_results.push((coll.clone(), results));
            }
            Err(e) => {
                if !args.json {
                    eprintln!("  Error searching {coll}: {e}");
                }
            }
        }
    }

    if args.json {
        let map: serde_json::Map<String, Value> = all_results
            .into_iter()
            .map(|(k, v)| (k, json!(v)))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Object(map)).unwrap()
        );
    } else if all_results.iter().all(|(_, r)| r.is_empty()) {
        println!("\n  No results found in any collection.");
    }
}

async fn show_stats(qdrant: &QdrantClient) {
    println!("=== Qdrant Collection Stats ===\n");
    for name in COLLECTIONS {
        let count = qdrant.collection_point_count(name).await;
        if count > 0 {
            println!("  {name}: {count} points");
        } else {
            println!("  {name}: (empty or not found)");
        }
    }
    println!();
}

fn build_filter(agent: Option<&str>, source: Option<&str>) -> Option<Value> {
    let mut must = Vec::new();
    if let Some(a) = agent {
        must.push(json!({"key": "agent", "match": {"value": a}}));
    }
    if let Some(s) = source {
        must.push(json!({"key": "source", "match": {"value": s}}));
    }
    if must.is_empty() {
        None
    } else {
        Some(json!({"must": must}))
    }
}

fn display_result(payload: &Value, collection: &str) {
    let text = payload["text"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(600)
        .collect::<String>();
    match collection {
        "slack_history" => {
            println!(
                "  Channel: #{}  User: {}  TS: {}",
                payload["channel_name"].as_str().unwrap_or("?"),
                payload["user"].as_str().unwrap_or("?"),
                payload["ts"].as_str().unwrap_or("?"),
            );
            println!("  {}", text.chars().take(500).collect::<String>());
        }
        c if c == COLLECTION_SESSIONS => {
            println!(
                "  Session: {}  Source: {}  Started: {}  Chunk: {}/{}",
                payload["session_id"].as_str().unwrap_or("?"),
                payload["source"].as_str().unwrap_or("?"),
                payload["started_at"].as_str().unwrap_or("?"),
                payload["chunk_index"].as_u64().unwrap_or(0),
                payload["total_chunks"].as_u64().unwrap_or(0),
            );
            println!("  {}", text);
        }
        _ => {
            println!(
                "  Agent: {}  Type: {}",
                payload["agent"].as_str().unwrap_or("?"),
                payload["chunk_type"]
                    .as_str()
                    .or_else(|| payload["source_type"].as_str())
                    .unwrap_or("?"),
            );
            println!("  {}", text.chars().take(500).collect::<String>());
        }
    }
}
