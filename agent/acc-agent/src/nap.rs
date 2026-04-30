//! Nightly holographic memory nap service.
//!
//! Each agent runs a single "nap" once per day at a UTC-based time derived
//! deterministically from its agent name. The name hash spreads agents across
//! a 6-hour window (00:00–05:59 UTC) so they never all fire simultaneously and
//! maintain at least ~15 minutes of separation on average.
//!
//! During the nap the agent:
//!   1. Writes the quench file → stops claiming new fleet tasks (~bounded minutes)
//!   2. Collects short-term memory files from the workspace memory directories
//!   3. LLM-summarises the collected content (ideas, wisdom, experience, tasks)
//!   4. Embeds the summary and upserts to Qdrant long-term storage with rich
//!      metadata (agent, date, session source, project tags)
//!   5. Removes the quench file → agent resumes fleet availability
//!
//! # Scheduling clock: UTC
//! Hub time (UTC) is the reference so agents coordinate across time zones. Each
//! agent's nap offset is `md5_u64(agent_name) % 360` minutes after midnight UTC.
//! With 4 typical agents (boris, natasha, rocky, bullwinkle) the expected
//! per-agent offsets are spread quasi-randomly — empirically ≥ 60 min apart.
//!
//! # Memory sources scanned
//! Priority order (all relative to `acc_dir`):
//!   1. `workspace/memory/YYYY-MM-DD.md`        — daily workspace notes
//!   2. `memory/YYYY-MM-DD.md`                  — agent-local daily notes
//!   3. `shared/*/memory/YYYY-MM-DD.md`         — per-project daily notes
//!   4. `shared/*/MEMORY.md`                    — project long-term summaries
//! Files from the last 7 days are included.

use crate::config::Config;
use acc_qdrant::{EmbedClient, QdrantClient};
use chrono::{Duration as ChronoDuration, NaiveDate, Utc};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const NAP_WINDOW_MINUTES: u64 = 360; // 6-hour spread for 4+ agents
const MEMORY_LOOKBACK_DAYS: i64 = 7;
const MAX_MEMORY_BYTES: usize = 100_000; // soft cap for LLM context
const LLM_TIMEOUT_SECS: u64 = 120;
const NAP_COLLECTION_SOURCE: &str = "nap_summary";

// ── Scheduling ─────────────────────────────────────────────────────────────────

/// Deterministic nap offset in minutes past UTC midnight.
/// Uses the low 32 bits of the agent name's MD5 hash, modulo the window.
fn nap_offset_minutes(agent_name: &str) -> u64 {
    let digest = md5::compute(agent_name.as_bytes());
    let lo = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) as u64;
    lo % NAP_WINDOW_MINUTES
}

/// Seconds from now until the next occurrence of UTC midnight + `offset_mins`.
fn secs_until_next_nap(offset_mins: u64) -> u64 {
    let now = Utc::now();
    let today_midnight = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight must be valid")
        .and_utc();
    let today_nap = today_midnight + ChronoDuration::minutes(offset_mins as i64);
    let tomorrow_nap = today_nap + ChronoDuration::days(1);

    let target = if now < today_nap { today_nap } else { tomorrow_nap };
    let delta = (target - now).num_seconds().max(0);
    delta as u64
}

// ── Memory collection ─────────────────────────────────────────────────────────

struct MemoryFile {
    label: String,
    content: String,
}

/// Collect short-term memory files from known locations.
fn collect_memory_files(acc_dir: &Path) -> Vec<MemoryFile> {
    let mut files: Vec<MemoryFile> = Vec::new();
    let today = Utc::now().date_naive();

    // Daily note patterns: check today and previous MEMORY_LOOKBACK_DAYS days.
    let date_range: Vec<NaiveDate> = (0..=MEMORY_LOOKBACK_DAYS)
        .filter_map(|d| today.checked_sub_days(chrono::Days::new(d as u64)))
        .collect();

    // 1. workspace/memory/YYYY-MM-DD.md
    let ws_mem = acc_dir.join("workspace").join("memory");
    for date in &date_range {
        let path = ws_mem.join(format!("{date}.md"));
        if let Some(f) = read_file_if_exists(&path, &format!("workspace/{date}")) {
            files.push(f);
        }
    }

    // 2. memory/YYYY-MM-DD.md (agent-local)
    let local_mem = acc_dir.join("memory");
    for date in &date_range {
        let path = local_mem.join(format!("{date}.md"));
        if let Some(f) = read_file_if_exists(&path, &format!("local/{date}")) {
            files.push(f);
        }
    }

    // 3+4. shared/*/memory/YYYY-MM-DD.md  and  shared/*/MEMORY.md
    let shared = acc_dir.join("shared");
    if let Ok(entries) = std::fs::read_dir(&shared) {
        let mut project_dirs: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        project_dirs.sort();
        for proj in &project_dirs {
            let proj_name = proj
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            // 3. Daily notes
            let proj_mem = proj.join("memory");
            for date in &date_range {
                let path = proj_mem.join(format!("{date}.md"));
                if let Some(f) =
                    read_file_if_exists(&path, &format!("shared/{proj_name}/{date}"))
                {
                    files.push(f);
                }
            }

            // 4. Long-term project memory (included once, lower weight)
            let memory_md = proj.join("MEMORY.md");
            if let Some(f) =
                read_file_if_exists(&memory_md, &format!("shared/{proj_name}/MEMORY"))
            {
                // Trim very large MEMORY.md files; they're background context only.
                let trimmed = if f.content.len() > 8000 {
                    format!("{}\n\n[… truncated …]", &f.content[..8000])
                } else {
                    f.content
                };
                files.push(MemoryFile {
                    label: f.label,
                    content: trimmed,
                });
            }
        }
    }

    files
}

fn read_file_if_exists(path: &Path, label: &str) -> Option<MemoryFile> {
    let content = std::fs::read_to_string(path).ok()?.trim().to_string();
    if content.is_empty() {
        return None;
    }
    Some(MemoryFile {
        label: label.to_string(),
        content,
    })
}

/// Concatenate collected memory files into a single prompt block, capped at
/// MAX_MEMORY_BYTES to stay within LLM context limits.
fn build_memory_block(files: &[MemoryFile]) -> String {
    let mut out = String::new();
    let mut total = 0usize;
    for f in files {
        let section = format!("\n---\n# {}\n\n{}\n", f.label, f.content);
        if total + section.len() > MAX_MEMORY_BYTES {
            out.push_str("\n---\n[additional memory files truncated — context limit reached]\n");
            break;
        }
        out.push_str(&section);
        total += section.len();
    }
    out
}

// ── LLM summarization ─────────────────────────────────────────────────────────

async fn summarize_memories(
    http: &reqwest::Client,
    agent_name: &str,
    memory_block: &str,
) -> Option<String> {
    let llm_cfg = acc_client::llm_config::LlmConfig::load();

    let system = format!(
        "You are {agent_name}, an AI agent, distilling your short-term memory into a \
         structured long-term summary. Extract and organise:\n\
         - Ideas and proposals encountered\n\
         - Wisdom and lessons learned\n\
         - Notable experiences and events\n\
         - Inspiration and observations\n\
         - Project/task updates and progress\n\
         Be concise but comprehensive. Use markdown headings. Preserve specifics \
         (task IDs, project names, decisions). Omit secrets, credentials, and PII."
    );

    let messages = vec![json!({
        "role": "user",
        "content": format!(
            "Today's date: {}\n\nMy short-term memory files:\n{memory_block}\n\n\
             Please write a structured long-term memory summary of my day.",
            Utc::now().format("%Y-%m-%d")
        )
    })];

    // Prefer OpenAI-compat endpoint (OPENAI_BASE_URL set on this host).
    let model = std::env::var("HERMES_MODEL")
        .or_else(|_| std::env::var("CLAUDE_CODE_DEFAULT_MODEL"))
        .unwrap_or_else(|_| "claude-opus-4-7".to_string());

    if llm_cfg.is_openai_configured() {
        call_openai(http, &llm_cfg.base_url, &llm_cfg.api_key, &model, &system, &messages).await
    } else {
        let ant_url = llm_cfg.anthropic_base_url_or_default().to_string();
        let ant_key = if !llm_cfg.anthropic_key.is_empty() {
            llm_cfg.anthropic_key.clone()
        } else {
            llm_cfg.api_key.clone()
        };
        call_anthropic(http, &ant_url, &ant_key, &model, &system, &messages).await
    }
}

async fn call_openai(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    messages: &[Value],
) -> Option<String> {
    let base = base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1");
    let mut oai_msgs = vec![json!({"role": "system", "content": system})];
    oai_msgs.extend_from_slice(messages);

    let body = json!({
        "model": model,
        "max_tokens": 2048,
        "messages": oai_msgs,
    });

    let resp: Value = http
        .post(format!("{base}/v1/chat/completions"))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    resp["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
}

async fn call_anthropic(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    messages: &[Value],
) -> Option<String> {
    let base = base_url.trim_end_matches('/');
    let body = json!({
        "model": model,
        "max_tokens": 2048,
        "system": system,
        "messages": messages,
    });

    let resp: Value = http
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    resp["content"][0]["text"].as_str().map(str::to_string)
}

// ── Qdrant upsert ─────────────────────────────────────────────────────────────

async fn upsert_summary(
    qdrant: &QdrantClient,
    embed: &EmbedClient,
    collection: &str,
    agent: &str,
    summary: &str,
    date_str: &str,
) -> Result<(), String> {
    let texts = [summary];
    let vectors = embed
        .embed(&texts)
        .await
        .map_err(|e| format!("embed: {e}"))?;

    let key = format!("nap:{agent}:{date_str}");
    let hash = md5::compute(key.as_bytes());
    let hex = format!("{:x}", hash);
    let id = u64::from_str_radix(&hex[..16], 16).unwrap_or(0) & 0x7FFF_FFFF_FFFF_FFFF;

    let payload = json!({
        "source":   NAP_COLLECTION_SOURCE,
        "agent":    agent,
        "date":     date_str,
        "type":     "nap_summary",
        "text":     summary,
    });

    let point = json!({"id": id, "vector": vectors[0], "payload": payload});
    qdrant
        .upsert_points_raw(collection, vec![point])
        .await
        .map_err(|e| format!("upsert: {e}"))
}

// ── Nap logic ─────────────────────────────────────────────────────────────────

pub async fn run_nap(cfg: &Config) {
    let date_str = Utc::now().format("%Y-%m-%d").to_string();
    log(cfg, &format!("nap starting (date={date_str})"));

    // 1. Write quench file — pause fleet task claiming for duration of nap.
    let quench = cfg.quench_file();
    let _ = std::fs::write(&quench, &date_str);
    log(cfg, "quench written — pausing fleet task claiming");

    let result = run_nap_inner(cfg, &date_str).await;

    // 5. Remove quench file — resume fleet availability.
    let _ = std::fs::remove_file(&quench);
    match result {
        Ok(n) => log(cfg, &format!("nap complete — {n} point(s) upserted to Qdrant")),
        Err(e) => log(cfg, &format!("nap error: {e}")),
    }
}

async fn run_nap_inner(cfg: &Config, date_str: &str) -> Result<usize, String> {
    // 2. Collect memory files.
    let files = collect_memory_files(&cfg.acc_dir);
    if files.is_empty() {
        return Err("no memory files found — nothing to summarise".to_string());
    }
    log(cfg, &format!("collected {} memory file(s)", files.len()));

    let memory_block = build_memory_block(&files);

    // 3. LLM summarise.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(LLM_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let summary = summarize_memories(&http, &cfg.agent_name, &memory_block)
        .await
        .ok_or_else(|| "LLM summarisation failed".to_string())?;

    log(
        cfg,
        &format!("summary generated ({} chars)", summary.len()),
    );

    // 4. Embed + upsert to Qdrant.
    let qdrant_url =
        std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_string());
    let qdrant_key = acc_tools::resolve_qdrant_api_key();
    let qdrant = QdrantClient::new(&qdrant_url, qdrant_key.as_deref())
        .map_err(|e| format!("qdrant client: {e}"))?;

    let embed_dim = std::env::var("EMBED_DIM")
        .or_else(|_| std::env::var("NVIDIA_EMBED_DIM"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(1536);
    let collection = std::env::var("SLACK_INGEST_COLLECTION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("holographic_memory_{embed_dim}"));

    if let Err(e) = qdrant
        .ensure_collection(
            &collection,
            embed_dim,
            &["source", "agent", "date", "type"],
        )
        .await
    {
        return Err(format!("ensure_collection: {e}"));
    }

    let embed = acc_tools::make_embed_client().map_err(|e| format!("embed client: {e}"))?;

    upsert_summary(&qdrant, &embed, &collection, &cfg.agent_name, &summary, date_str).await?;

    log(
        cfg,
        &format!(
            "upserted nap summary to {collection} (agent={}, date={date_str})",
            cfg.agent_name
        ),
    );

    Ok(1)
}

// ── Scheduler ─────────────────────────────────────────────────────────────────

/// Long-running nap scheduler. Waits until each agent's nightly nap time
/// (UTC midnight + deterministic per-agent offset), then runs the nap.
pub async fn run_scheduler(cfg: &Config) {
    let offset = nap_offset_minutes(&cfg.agent_name);
    let nap_time_utc = offset_to_hhmm(offset);
    log(
        cfg,
        &format!(
            "nap scheduler started (agent={}, nap_time={}:{:02} UTC, offset={offset}min)",
            cfg.agent_name,
            nap_time_utc.0,
            nap_time_utc.1
        ),
    );

    loop {
        let secs = secs_until_next_nap(offset);
        log(
            cfg,
            &format!(
                "next nap in {:.1}h ({secs}s)",
                secs as f64 / 3600.0
            ),
        );
        tokio::time::sleep(Duration::from_secs(secs)).await;
        run_nap(cfg).await;
        // Brief pause so we don't re-fire immediately after a nap that ran very
        // close to the offset boundary.
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

fn offset_to_hhmm(offset_mins: u64) -> (u64, u64) {
    (offset_mins / 60, offset_mins % 60)
}

// ── CLI entry point ────────────────────────────────────────────────────────────

/// `acc-agent nap [--schedule]`
///
/// Without `--schedule`: run a single nap immediately (useful for testing and
/// first-time setup verification).
///
/// With `--schedule`: long-running daemon that wakes each night at this agent's
/// scheduled nap time (UTC midnight + deterministic per-agent offset).
pub async fn run(args: &[String]) {
    let schedule = args.iter().any(|a| a == "--schedule");

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[nap] config error: {e}");
            std::process::exit(1);
        }
    };

    if schedule {
        run_scheduler(&cfg).await;
    } else {
        run_nap(&cfg).await;
    }
}

// ── Logging ───────────────────────────────────────────────────────────────────

fn log(cfg: &Config, msg: &str) {
    let ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [{}] [nap] {msg}", cfg.agent_name);
    eprintln!("{line}");
    let path = cfg.acc_dir.join("logs").join("nap.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nap_offset_is_deterministic() {
        let a = nap_offset_minutes("natasha");
        let b = nap_offset_minutes("natasha");
        assert_eq!(a, b);
        assert!(a < NAP_WINDOW_MINUTES);
    }

    #[test]
    fn agents_have_distinct_offsets() {
        let agents = ["boris", "natasha", "rocky", "bullwinkle"];
        let offsets: Vec<u64> = agents.iter().map(|a| nap_offset_minutes(a)).collect();
        // All must be within window
        assert!(offsets.iter().all(|&o| o < NAP_WINDOW_MINUTES));
        // Must be distinct (hash collisions on 4 values in 360-bucket space are
        // essentially impossible with MD5)
        let unique: std::collections::HashSet<u64> = offsets.iter().cloned().collect();
        assert_eq!(unique.len(), agents.len(), "all agents need distinct offsets");
        // Log offsets for documentation
        for (agent, offset) in agents.iter().zip(offsets.iter()) {
            println!("{agent}: {offset}min past midnight UTC");
        }
    }

    #[test]
    fn secs_until_next_nap_is_positive_and_bounded() {
        let secs = secs_until_next_nap(120); // 2am UTC
        assert!(secs <= 86400, "must be at most one day away");
    }

    #[test]
    fn build_memory_block_truncates_at_limit() {
        let big = MemoryFile {
            label: "test".to_string(),
            content: "x".repeat(MAX_MEMORY_BYTES + 1000),
        };
        let block = build_memory_block(&[big]);
        assert!(block.len() <= MAX_MEMORY_BYTES + 200, "must be near cap");
    }

    #[test]
    fn offset_to_hhmm_converts_correctly() {
        assert_eq!(offset_to_hhmm(0), (0, 0));
        assert_eq!(offset_to_hhmm(90), (1, 30));
        assert_eq!(offset_to_hhmm(359), (5, 59));
    }
}
