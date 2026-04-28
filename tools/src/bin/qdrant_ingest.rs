//! Hermes session ingestion to Qdrant.
//! Port of scripts/qdrant-python/qdrant_ingest.py

use acc_qdrant::{chunk_text, deterministic_id, EmbedClient, QdrantClient};
use chrono::Utc;
use clap::Parser;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::PathBuf;

const COLLECTION_SESSIONS: &str = "hermes_sessions";
const COLLECTION_MEMORIES: &str = "agent_memories";
const EMBED_DIM: u64 = 3072;
const IDX_FIELDS: &[&str] = &["session_id", "agent", "source", "role", "chunk_type"];

#[derive(Parser)]
#[command(about = "Ingest Hermes sessions into Qdrant")]
struct Args {
    /// Re-ingest all sessions
    #[arg(long)]
    all: bool,
    /// Also ingest MEMORY.md/USER.md/SOUL.md
    #[arg(long)]
    memory: bool,
    /// Ingest specific session ID
    #[arg(long)]
    session: Option<String>,
}

#[tokio::main]
async fn main() {
    acc_tools::load_acc_env();
    let args = Args::parse();

    let agent_name = std::env::var("AGENT_NAME").unwrap_or_else(|_| "Rocky".to_string());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let hermes_dir = PathBuf::from(&home).join(".hermes");
    let state_db = hermes_dir.join("state.db");

    println!("=== Qdrant Ingestion for {agent_name} ===");
    println!("  DB: {}", state_db.display());

    let qdrant_url =
        std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_string());
    let qdrant_key = acc_tools::resolve_qdrant_api_key();
    let qdrant = QdrantClient::new(&qdrant_url, qdrant_key.as_deref()).expect("qdrant client");

    let embed = match acc_tools::make_embed_client() {
        Ok(e) => {
            println!("  Credentials loaded ✓");
            e
        }
        Err(e) => {
            eprintln!("Embed error: {e}");
            std::process::exit(1);
        }
    };

    let sess_count = qdrant
        .ensure_collection(COLLECTION_SESSIONS, EMBED_DIM, IDX_FIELDS)
        .await
        .unwrap_or(0);
    let mem_count = qdrant
        .ensure_collection(COLLECTION_MEMORIES, EMBED_DIM, IDX_FIELDS)
        .await
        .unwrap_or(0);
    println!("  {COLLECTION_SESSIONS}: {sess_count} existing points");
    println!("  {COLLECTION_MEMORIES}: {mem_count} existing points");

    let ingest_state_path = hermes_dir.join("scripts").join(".qdrant_ingest_state.json");
    let mut state = load_state(&ingest_state_path);

    // Determine sessions to ingest
    let conn = match Connection::open(&state_db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Cannot open {}: {e}", state_db.display());
            std::process::exit(1);
        }
    };

    let sessions = if let Some(ref sid) = args.session {
        get_sessions(&conn, Some(sid))
    } else if args.all {
        get_sessions(&conn, None)
    } else {
        let already: std::collections::HashSet<String> = state["ingested_sessions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        get_sessions(&conn, None)
            .into_iter()
            .filter(|s| !already.contains(s["id"].as_str().unwrap_or("")))
            .collect()
    };

    let session_count = sessions.len();
    if args.session.is_some() {
        println!("\n  Ingesting specific session");
    } else if args.all {
        println!("\n  Re-ingesting ALL {} sessions", session_count);
    } else {
        println!("\n  {} new sessions to ingest", session_count);
    }

    if !sessions.is_empty() {
        let t0 = std::time::Instant::now();
        let count = ingest_sessions(&conn, &sessions, &qdrant, &embed, &agent_name).await;
        println!(
            "  ✓ Ingested {} session points in {:.1}s",
            count,
            t0.elapsed().as_secs_f64()
        );

        // Update state — rebuild the set to avoid borrow conflicts
        let mut ingested_set: Vec<String> = state["ingested_sessions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        for s in &sessions {
            if let Some(id) = s["id"].as_str() {
                if !ingested_set.contains(&id.to_string()) {
                    ingested_set.push(id.to_string());
                }
            }
        }
        state["ingested_sessions"] = json!(ingested_set);
        state["last_run"] = json!(Utc::now().to_rfc3339());
        save_state(&ingest_state_path, &state);
    } else {
        println!("  No sessions to ingest.");
    }

    if args.memory {
        println!("\n  Ingesting memory files...");
        let t0 = std::time::Instant::now();
        let count = ingest_memory_files(&hermes_dir, &qdrant, &embed, &agent_name).await;
        println!(
            "  ✓ Ingested {} memory points in {:.1}s",
            count,
            t0.elapsed().as_secs_f64()
        );
    }

    let final_sess = qdrant.collection_point_count(COLLECTION_SESSIONS).await;
    let final_mem = qdrant.collection_point_count(COLLECTION_MEMORIES).await;
    println!("\n=== Final State ===");
    println!("  {COLLECTION_SESSIONS}: {final_sess} points");
    println!("  {COLLECTION_MEMORIES}: {final_mem} points");
    println!("  Done!");
}

fn get_sessions(conn: &Connection, session_id: Option<&str>) -> Vec<Value> {
    let query = if session_id.is_some() {
        "SELECT id, source, model, started_at, title FROM sessions WHERE id = ?1"
    } else {
        "SELECT id, source, model, started_at, title FROM sessions ORDER BY started_at"
    };
    let mut stmt = conn.prepare(query).unwrap();
    if let Some(sid) = session_id {
        stmt.query_map(params![sid], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "source": row.get::<_, Option<String>>(1)?,
                "model": row.get::<_, Option<String>>(2)?,
                "started_at": row.get::<_, Option<i64>>(3)?,
                "title": row.get::<_, Option<String>>(4)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    } else {
        stmt.query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "source": row.get::<_, Option<String>>(1)?,
                "model": row.get::<_, Option<String>>(2)?,
                "started_at": row.get::<_, Option<i64>>(3)?,
                "title": row.get::<_, Option<String>>(4)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }
}

fn get_messages(conn: &Connection, session_id: &str) -> Vec<Value> {
    let mut stmt = conn
        .prepare(
            "SELECT role, content, tool_name FROM messages WHERE session_id = ?1 ORDER BY timestamp",
        )
        .unwrap();
    stmt.query_map(params![session_id], |row| {
        Ok(json!({
            "role": row.get::<_, String>(0)?,
            "content": row.get::<_, Option<String>>(1)?,
            "tool_name": row.get::<_, Option<String>>(2)?,
        }))
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

async fn ingest_sessions(
    conn: &Connection,
    sessions: &[Value],
    qdrant: &QdrantClient,
    embed: &EmbedClient,
    agent_name: &str,
) -> u64 {
    let mut all_chunks: Vec<(u64, String, Value)> = Vec::new();

    for session in sessions {
        let sid = session["id"].as_str().unwrap_or("");
        let messages = get_messages(conn, sid);
        if messages.is_empty() {
            continue;
        }

        let source = session["source"].as_str().unwrap_or("unknown");
        let model = session["model"].as_str().unwrap_or("unknown");
        let started_at = session["started_at"].as_i64();
        let started_str = started_at
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
            .map(|dt: chrono::DateTime<Utc>| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let title = session["title"].as_str().unwrap_or("");

        let mut parts: Vec<String> = Vec::new();
        for msg in &messages {
            let role = msg["role"].as_str().unwrap_or("user");
            let content = msg["content"].as_str().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let tool_name = msg["tool_name"].as_str().unwrap_or("");
            let prefix = match role {
                "user" => "User".to_string(),
                "assistant" => format!("Assistant ({agent_name})"),
                "system" => "System".to_string(),
                "tool" => {
                    if !tool_name.is_empty() {
                        format!("Tool ({tool_name})")
                    } else {
                        "Tool".to_string()
                    }
                }
                r => r.to_string(),
            };
            parts.push(format!("{prefix}: {content}"));
        }
        if parts.is_empty() {
            continue;
        }

        let full_text = parts.join("\n\n");
        let mut header = format!(
            "Session: {sid}\nAgent: {agent_name}\nSource: {source}\nModel: {model}\nStarted: {started_str}"
        );
        if !title.is_empty() {
            header.push_str(&format!("\nTitle: {title}"));
        }

        let chunks = chunk_text(&full_text, 1500, 200);
        let total = chunks.len();
        for (i, chunk) in chunks.into_iter().enumerate() {
            let id = deterministic_id("hermes_session", &[sid, &i.to_string()]);
            let text = format!("{header}\n\n{chunk}");
            let payload = json!({
                "session_id": sid,
                "agent": agent_name,
                "source": source,
                "model": model,
                "started_at": started_str,
                "title": title,
                "chunk_index": i,
                "total_chunks": total,
                "chunk_type": "session",
                "text": &text,
                "ingested_at": Utc::now().to_rfc3339(),
            });
            all_chunks.push((id, text, payload));
        }
    }

    if all_chunks.is_empty() {
        return 0;
    }
    println!(
        "  {} chunks from {} sessions",
        all_chunks.len(),
        sessions.len()
    );

    let texts: Vec<&str> = all_chunks.iter().map(|(_, t, _)| t.as_str()).collect();
    let embeddings = match embed.embed(&texts).await {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Embedding error: {err}");
            return 0;
        }
    };

    let points: Vec<Value> = all_chunks
        .iter()
        .zip(embeddings.iter())
        .map(|((id, _, payload), vec)| json!({"id": id, "vector": vec, "payload": payload}))
        .collect();

    let count = points.len() as u64;
    if let Err(e) = qdrant.upsert_points_raw(COLLECTION_SESSIONS, points).await {
        eprintln!("Upsert error: {e}");
        return 0;
    }
    count
}

async fn ingest_memory_files(
    hermes_dir: &PathBuf,
    qdrant: &QdrantClient,
    embed: &EmbedClient,
    agent_name: &str,
) -> u64 {
    let files = [
        (hermes_dir.join("MEMORY.md"), "memory"),
        (hermes_dir.join("USER.md"), "user_profile"),
        (hermes_dir.join("SOUL.md"), "soul"),
    ];
    let mut all_chunks: Vec<(u64, String, Value)> = Vec::new();

    for (path, mtype) in &files {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let chunks = chunk_text(&text, 1000, 100);
                let total = chunks.len();
                println!("  {}: {} chunks", path.display(), total);
                for (i, chunk) in chunks.into_iter().enumerate() {
                    let id =
                        deterministic_id(&format!("hermes_{mtype}"), &[agent_name, &i.to_string()]);
                    let header = format!("Agent: {agent_name}\nType: {mtype}\n\n");
                    let text = format!("{header}{chunk}");
                    let payload = json!({
                        "agent": agent_name,
                        "chunk_type": mtype,
                        "chunk_index": i,
                        "total_chunks": total,
                        "text": &text,
                        "source": format!("{mtype}.md"),
                        "ingested_at": Utc::now().to_rfc3339(),
                    });
                    all_chunks.push((id, text, payload));
                }
            }
            Err(_) => println!("  {}: not found, skipping", path.display()),
        }
    }

    if all_chunks.is_empty() {
        return 0;
    }
    let texts: Vec<&str> = all_chunks.iter().map(|(_, t, _)| t.as_str()).collect();
    let embeddings = match embed.embed(&texts).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Memory embed error: {e}");
            return 0;
        }
    };

    let points: Vec<Value> = all_chunks
        .iter()
        .zip(embeddings.iter())
        .map(|((id, _, payload), vec)| json!({"id": id, "vector": vec, "payload": payload}))
        .collect();

    let count = points.len() as u64;
    if let Err(e) = qdrant.upsert_points_raw(COLLECTION_MEMORIES, points).await {
        eprintln!("Memory upsert error: {e}");
        return 0;
    }
    count
}

fn load_state(path: &PathBuf) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({"ingested_sessions": [], "last_run": null}))
}

fn save_state(path: &PathBuf, state: &Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(state).unwrap_or_default(),
    );
}
