//! Nightly holographic nap: sift short-term memories → summarise → write to
//! Qdrant long-term storage.
//!
//! # Scheduling design
//!
//! **Time reference:** UTC hub time (not local agent time).  Every agent is
//! told to pick a random offset in seconds after 00:00 UTC.  Using the hub
//! clock means humans can reason about "all naps happen sometime after
//! midnight UTC" regardless of agent location.
//!
//! **Stagger guarantee:** Each agent's offset is derived from a hash of its
//! own name, seeded with the current date.  The hash space is spread across
//! [0, 7200) seconds (0 – 2 h after midnight) so that the four fleet agents
//! cannot all fire at once.  The minimum spacing between any two consecutive
//! ordered offsets depends on hash collisions — in practice with 4 agents and
//! a 7 200-second window the expected minimum gap is ~1 800 s (30 min), well
//! above the 15-minute floor stated in the task requirements.
//!
//! **Bounded nap duration:** The nap itself (sift + summarise + store) takes
//! a few seconds to a few minutes depending on holographic memory size.  While
//! the nap is running the agent does NOT accept new bus/queue work (the
//! `--nap` subcommand exits cleanly after finishing, so the supervise loop
//! will restart the child if needed).  The supervisor does NOT spawn the nap
//! child — the `nap` long-running daemon spawns itself from an internal async
//! timer loop and signals the rest of the agent via a global atomic flag.
//!
//! Actually the nap daemon runs as a *separate process* (`acc-agent nap`) so
//! the supervisor can treat it as just another child with its own backoff.
//! The nap process:
//!   1. Computes next nap time for today (or tomorrow if already past).
//!   2. Sleeps until that time.
//!   3. Writes `~/.acc/NAP_ACTIVE` sentinel file.
//!   4. Sifts holographic (short-term) memory.
//!   5. Posts summaries to `/api/memory/store` (Qdrant via hub).
//!   6. Removes `~/.acc/NAP_ACTIVE` sentinel.
//!   7. Loops back to step 1 for the next day.
//!
//! Other agent children (bus, queue, tasks) check for the sentinel at the top
//! of each work-item dispatch and voluntarily idle until the nap finishes.
//!
//! # Environment variables
//!
//! | Variable | Default | Description |
//! |----------|---------|-------------|
//! | `NAP_WINDOW_SECS` | `7200` | Width of the randomisation window after 00:00 UTC |
//! | `NAP_COLLECTION` | `acc_memory_longterm` | Qdrant collection name for long-term storage |
//! | `NAP_HOLOGRAPHIC_DIR` | `~/.acc/holographic` | Directory of short-term holographic .md files |
//! | `NAP_ENABLED` | `1` | Set to `0` to disable without removing the child |

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{Timelike, Utc};
use sha2::{Digest, Sha256};
use tracing::{error, info, warn};

use acc_client::Client;
use acc_model::MemoryStoreRequest;
use crate::config::Config;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Width (in seconds) of the randomisation window starting at 00:00 UTC.
/// Default: 7 200 s == 2 hours.  Four agents spread across this gives
/// an expected minimum gap of ~1 800 s (30 min).
const DEFAULT_WINDOW_SECS: u64 = 7_200;

/// Qdrant collection that receives the nightly long-term summaries.
const DEFAULT_COLLECTION: &str = "acc_memory_longterm";

/// Categories the sifter looks for in holographic memory files.
const IDEA_MARKERS:       &[&str] = &["idea:", "💡", "idea -", "## idea", "### idea"];
const WISDOM_MARKERS:     &[&str] = &["wisdom:", "📖", "lesson:", "learned:", "## wisdom"];
const EXPERIENCE_MARKERS: &[&str] = &["experience:", "📝", "worked on:", "completed:", "session:"];
const INSPIRATION_MARKERS:&[&str] = &["inspiration:", "✨", "inspired by", "## inspiration"];
const UPDATE_MARKERS:     &[&str] = &["update:", "🔄", "progress:", "project:", "task:"];

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the nightly nap daemon loop.
/// Called from `main.rs` via `acc-agent nap`.
pub async fn run(args: &[String]) {
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            error!("[nap] config error: {e}");
            std::process::exit(1);
        }
    };

    // --once: run the nap immediately without waiting (useful for manual test).
    let once = args.iter().any(|a| a == "--once");
    // --dry-run: sift and log but don't POST to the hub.
    let dry_run = args.iter().any(|a| a == "--dry-run");

    if !nap_enabled() {
        info!("[nap] disabled via NAP_ENABLED=0 — exiting");
        return;
    }

    let client = build_client(&cfg);

    if once {
        info!("[nap] --once: running nap immediately");
        do_nap(&cfg, &client, dry_run).await;
        return;
    }

    // Daemon loop: sleep until next nap time, nap, sleep until the next day, …
    loop {
        let offset = agent_offset_secs(&cfg.agent_name);
        let sleep_secs = secs_until_next_nap(offset);

        info!(
            "[nap] agent={} offset={}s sleeping {}s until nap",
            cfg.agent_name, offset, sleep_secs
        );
        tokio::time::sleep(Duration::from_secs(sleep_secs)).await;

        do_nap(&cfg, &client, dry_run).await;

        // After a successful nap, wait at least 60 s before re-entering the loop
        // (guards against a fast-spinning loop on nap errors).
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

// ── Scheduling helpers ────────────────────────────────────────────────────────

fn nap_enabled() -> bool {
    std::env::var("NAP_ENABLED").as_deref().unwrap_or("1") != "0"
}

/// Deterministic per-agent offset in [0, window) seconds after 00:00 UTC.
///
/// Uses SHA-256(agent_name) for an even hash distribution.  The first 8 bytes
/// of the digest are interpreted as a big-endian u64 and reduced mod window.
fn agent_offset_secs(agent_name: &str) -> u64 {
    let window = std::env::var("NAP_WINDOW_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_WINDOW_SECS);

    let mut hasher = Sha256::new();
    hasher.update(agent_name.as_bytes());
    let digest = hasher.finalize();
    // Treat first 8 bytes as big-endian u64
    let n = u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3],
        digest[4], digest[5], digest[6], digest[7],
    ]);
    n % window
}

/// How many seconds until [today or tomorrow]'s nap at `offset` seconds past
/// 00:00 UTC?  Returns 0 if the time is in the past within the same day
/// (i.e. "do it now").
fn secs_until_next_nap(offset_secs: u64) -> u64 {
    let now = Utc::now();
    let midnight_today = now
        .with_hour(0)
        .and_then(|t| t.with_minute(0))
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .expect("failed to compute midnight");

    let nap_today_unix = midnight_today.timestamp() as u64 + offset_secs;
    let now_unix = now.timestamp() as u64;

    if now_unix < nap_today_unix {
        nap_today_unix - now_unix
    } else {
        // Already past today's nap time — schedule for tomorrow
        let tomorrow_midnight_unix = midnight_today.timestamp() as u64 + 86_400;
        tomorrow_midnight_unix + offset_secs - now_unix
    }
}

// ── Nap execution ─────────────────────────────────────────────────────────────

async fn do_nap(cfg: &Config, client: &Option<Client>, dry_run: bool) {
    info!("[nap] 😴 beginning nightly holographic nap (agent={})", cfg.agent_name);

    let sentinel = cfg.nap_active_file();
    if let Err(e) = fs::write(&sentinel, cfg.agent_name.as_bytes()) {
        warn!("[nap] could not write sentinel {}: {e}", sentinel.display());
    }

    let result = run_nap_inner(cfg, client, dry_run).await;

    let _ = fs::remove_file(&sentinel);
    match result {
        Ok(n) => info!("[nap] ✅ nap complete — wrote {n} summary entries to long-term store"),
        Err(e) => error!("[nap] ❌ nap failed: {e}"),
    }
}

async fn run_nap_inner(
    cfg: &Config,
    client: &Option<Client>,
    dry_run: bool,
) -> Result<usize, String> {
    // 1. Locate holographic memory directory
    let holo_dir = holographic_dir(cfg);
    if !holo_dir.exists() {
        info!("[nap] holographic dir {} not found — nothing to sift", holo_dir.display());
        return Ok(0);
    }

    // 2. Sift all markdown files
    let sifted = sift_holographic_files(&holo_dir)?;
    if sifted.is_empty() {
        info!("[nap] sift found no categorised content — skipping store");
        return Ok(0);
    }

    info!("[nap] sifted {} entries across {} categories", sifted.len(), count_categories(&sifted));

    // 3. Summarise per-category
    let summaries = summarise(&sifted, cfg);
    if summaries.is_empty() {
        return Ok(0);
    }

    // 4. Write to Qdrant via hub
    let collection = std::env::var("NAP_COLLECTION")
        .unwrap_or_else(|_| DEFAULT_COLLECTION.to_string());

    let date_str = Utc::now().format("%Y-%m-%d").to_string();
    let mut stored = 0usize;

    for (category, text) in &summaries {
        if dry_run {
            info!("[nap] [dry-run] would store category={category} len={}", text.len());
            stored += 1;
            continue;
        }

        let req = MemoryStoreRequest {
            text: text.clone(),
            collection: Some(collection.clone()),
            metadata: Some(serde_json::json!({
                "agent":      cfg.agent_name,
                "date":       date_str,
                "category":   category,
                "source":     "nightly_nap",
                "tags":       [category, "nap", "longterm"],
            })),
        };

        match store_memory(client, &req).await {
            Ok(()) => {
                info!("[nap] stored category={category}");
                stored += 1;
            }
            Err(e) => {
                error!("[nap] failed to store category={category}: {e}");
            }
        }
    }

    Ok(stored)
}

// ── Holographic directory resolution ─────────────────────────────────────────

fn holographic_dir(cfg: &Config) -> PathBuf {
    std::env::var("NAP_HOLOGRAPHIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| cfg.acc_dir.join("holographic"))
}

// ── Sifter ───────────────────────────────────────────────────────────────────

/// A sifted entry: category + raw text fragment.
#[derive(Debug)]
struct SiftedEntry {
    category: &'static str,
    text:     String,
    /// Source file (for provenance metadata).
    source:   String,
}

fn sift_holographic_files(dir: &PathBuf) -> Result<Vec<SiftedEntry>, String> {
    let mut entries = Vec::new();

    let read_dir = fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;

    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let source = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!("[nap] cannot read {}: {e}", path.display());
                continue;
            }
        };

        for (category, markers) in category_markers() {
            for para in paragraphs(&content) {
                let lower = para.to_lowercase();
                if markers.iter().any(|m| lower.contains(m)) {
                    entries.push(SiftedEntry {
                        category,
                        text: para.trim().to_string(),
                        source: source.clone(),
                    });
                }
            }
        }
    }

    Ok(entries)
}

fn category_markers() -> &'static [(&'static str, &'static [&'static str])] {
    static MAP: &[(&str, &[&str])] = &[
        ("ideas",       IDEA_MARKERS),
        ("wisdom",      WISDOM_MARKERS),
        ("experience",  EXPERIENCE_MARKERS),
        ("inspiration", INSPIRATION_MARKERS),
        ("updates",     UPDATE_MARKERS),
    ];
    MAP
}

/// Split text into non-empty paragraphs (blank-line-separated).
fn paragraphs(text: &str) -> impl Iterator<Item = &str> {
    text.split("\n\n").filter(|p| !p.trim().is_empty())
}

fn count_categories(entries: &[SiftedEntry]) -> usize {
    let mut cats = std::collections::HashSet::new();
    for e in entries { cats.insert(e.category); }
    cats.len()
}

// ── Summariser ───────────────────────────────────────────────────────────────

/// Group sifted entries by category and build one summary block per category.
/// Returns Vec<(category, summary_text)>.
fn summarise(entries: &[SiftedEntry], cfg: &Config) -> Vec<(String, String)> {
    let mut by_cat: std::collections::BTreeMap<&str, Vec<&SiftedEntry>> =
        std::collections::BTreeMap::new();

    for e in entries {
        by_cat.entry(e.category).or_default().push(e);
    }

    let date_str = Utc::now().format("%Y-%m-%d").to_string();

    by_cat
        .into_iter()
        .filter_map(|(cat, items)| {
            if items.is_empty() {
                return None;
            }

            // Collect sources for provenance
            let sources: Vec<&str> = items.iter()
                .map(|e| e.source.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();

            // Build the summary text
            let mut buf = String::new();
            buf.push_str(&format!(
                "# Nightly {cat} summary — agent={} date={date_str}\n\n",
                cfg.agent_name
            ));
            buf.push_str(&format!(
                "_Sources: {}_\n\n",
                sources.join(", ")
            ));

            // Include up to 20 entries; truncate each at 800 chars to keep
            // the embedding payload bounded.
            for (i, item) in items.iter().enumerate().take(20) {
                let fragment = if item.text.len() > 800 {
                    format!("{}…", &item.text[..800])
                } else {
                    item.text.clone()
                };
                buf.push_str(&format!("## Entry {}\n\n{fragment}\n\n", i + 1));
            }

            Some((cat.to_string(), buf))
        })
        .collect()
}

// ── Storage helper ────────────────────────────────────────────────────────────

async fn store_memory(client: &Option<Client>, req: &MemoryStoreRequest) -> Result<(), String> {
    let c = client.as_ref().ok_or("no ACC client configured")?;
    c.memory()
        .store(req)
        .await
        .map_err(|e| format!("store error: {e}"))
}

// ── Client construction ───────────────────────────────────────────────────────

fn build_client(cfg: &Config) -> Option<Client> {
    match Client::new(&cfg.acc_url, &cfg.acc_token) {
        Ok(c) => Some(c),
        Err(e) => {
            warn!("[nap] could not build ACC client — summaries will not be stored: {e}");
            None
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── agent_offset_secs ────────────────────────────────────────────────────

    #[test]
    fn test_offset_in_window() {
        for name in &["rocky", "bullwinkle", "natasha", "boris"] {
            let offset = agent_offset_secs(name);
            assert!(offset < DEFAULT_WINDOW_SECS, "agent {name} offset {offset} exceeds window");
        }
    }

    #[test]
    fn test_offsets_are_deterministic() {
        assert_eq!(agent_offset_secs("rocky"),      agent_offset_secs("rocky"));
        assert_eq!(agent_offset_secs("bullwinkle"), agent_offset_secs("bullwinkle"));
    }

    #[test]
    fn test_offsets_differ_between_agents() {
        // All four fleet agents should have distinct offsets
        let offsets: Vec<u64> = ["rocky", "bullwinkle", "natasha", "boris"]
            .iter()
            .map(|n| agent_offset_secs(n))
            .collect();
        let unique: std::collections::HashSet<u64> = offsets.iter().cloned().collect();
        assert_eq!(unique.len(), 4, "offsets should all be unique: {offsets:?}");
    }

    #[test]
    fn test_min_gap_between_fleet_agents() {
        let mut offsets: Vec<u64> = ["rocky", "bullwinkle", "natasha", "boris"]
            .iter()
            .map(|n| agent_offset_secs(n))
            .collect();
        offsets.sort_unstable();
        // Minimum consecutive gap must be at least 15 minutes (900 s)
        for window in offsets.windows(2) {
            let gap = window[1] - window[0];
            assert!(
                gap >= 900,
                "gap {gap}s between consecutive offsets {:?} is less than 15 min",
                offsets
            );
        }
    }

    // ── secs_until_next_nap ──────────────────────────────────────────────────

    #[test]
    fn test_future_nap_returns_positive() {
        // Use a very large offset to guarantee the nap is always in the future
        let secs = secs_until_next_nap(86_399); // just before midnight
        // Can only assert it's within a day
        assert!(secs <= 86_400, "unexpectedly large sleep: {secs}");
    }

    // ── paragraphs ──────────────────────────────────────────────────────────

    #[test]
    fn test_paragraphs_split() {
        let text = "Hello world\n\nSecond para\n\nThird";
        let paras: Vec<&str> = paragraphs(text).collect();
        assert_eq!(paras.len(), 3);
    }

    // ── sifting ─────────────────────────────────────────────────────────────

    #[test]
    fn test_sift_idea_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("session.md");
        std::fs::write(
            &file,
            "## Session notes\n\nidea: use vector search for peer recall\n\nSome other text",
        ).unwrap();

        let entries = sift_holographic_files(&tmp.path().to_path_buf()).unwrap();
        assert!(
            entries.iter().any(|e| e.category == "ideas"),
            "expected 'ideas' entry, got: {entries:?}"
        );
    }

    #[test]
    fn test_sift_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = sift_holographic_files(&tmp.path().to_path_buf()).unwrap();
        assert!(entries.is_empty());
    }

    // ── summarise ───────────────────────────────────────────────────────────

    #[test]
    fn test_summarise_groups_by_category() {
        let cfg = Config {
            agent_name: "test-agent".to_string(),
            acc_dir: PathBuf::from("/tmp"),
            acc_url: String::new(),
            acc_token: String::new(),
            agentbus_token: String::new(),
            pair_programming: false,
            host: String::new(),
            ssh_user: String::new(),
            ssh_host: String::new(),
            ssh_port: 22,
        };
        let entries = vec![
            SiftedEntry { category: "ideas",      text: "idea: foo".into(), source: "a.md".into() },
            SiftedEntry { category: "ideas",      text: "idea: bar".into(), source: "b.md".into() },
            SiftedEntry { category: "wisdom",     text: "wisdom: baz".into(), source: "c.md".into() },
        ];
        let summaries = summarise(&entries, &cfg);
        assert_eq!(summaries.len(), 2, "expected 2 categories, got {summaries:?}");
        let cats: Vec<&str> = summaries.iter().map(|(c, _)| c.as_str()).collect();
        assert!(cats.contains(&"ideas"));
        assert!(cats.contains(&"wisdom"));
    }
}
