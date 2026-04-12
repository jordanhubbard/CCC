use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    status: String,
    #[serde(rename = "appliedAt")]
    applied_at: String,
}

type State = HashMap<String, Record>;

// ── State file path ────────────────────────────────────────────────────────

fn state_path() -> PathBuf {
    let ccc_dir = std::env::var("CCC_DIR").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        format!("{}/.ccc", home)
    });
    PathBuf::from(ccc_dir).join("migrations.json")
}

fn load(path: &PathBuf) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(path: &PathBuf, state: &State) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut content = serde_json::to_string_pretty(state)
        .unwrap_or_else(|_| "{}".to_string());
    content.push('\n');
    if let Err(e) = std::fs::write(path, &content) {
        eprintln!("migrations.json write failed: {e}");
        std::process::exit(1);
    }
}

fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ── Subcommand dispatch ────────────────────────────────────────────────────

pub fn run(args: &[String]) {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "is-applied" => cmd_is_applied(&args[1..]),
        "record"     => cmd_record(&args[1..]),
        "list"       => cmd_list(&args[1..]),
        _ => {
            eprintln!("Usage: ccc-agent migrate <is-applied|record|list>");
            std::process::exit(1);
        }
    }
}

// ── is-applied ────────────────────────────────────────────────────────────

fn cmd_is_applied(args: &[String]) {
    let name = args.first().unwrap_or_else(|| {
        eprintln!("Usage: ccc-agent migrate is-applied <name>");
        std::process::exit(2);
    });
    let path = state_path();
    let state = load(&path);
    match state.get(name) {
        Some(r) if r.status == "ok" => std::process::exit(0),
        _ => std::process::exit(1),
    }
}

// ── record ────────────────────────────────────────────────────────────────

fn cmd_record(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Usage: ccc-agent migrate record <name> <ok|failed>");
        std::process::exit(2);
    }
    let name = &args[0];
    let status = &args[1];
    let path = state_path();
    let mut state = load(&path);
    state.insert(name.clone(), Record {
        status: status.clone(),
        applied_at: now_utc(),
    });
    save(&path, &state);
}

// ── list ──────────────────────────────────────────────────────────────────

const GREEN:  &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED:    &str = "\x1b[31m";
const RESET:  &str = "\x1b[0m";

fn cmd_list(args: &[String]) {
    let dir = args.first().unwrap_or_else(|| {
        eprintln!("Usage: ccc-agent migrate list <migrations-dir>");
        std::process::exit(2);
    });

    let dir = PathBuf::from(dir);
    let state = load(&state_path());

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| {
            eprintln!("Cannot read migrations directory {}: {e}", dir.display());
            std::process::exit(1);
        })
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().map(|e| e == "sh").unwrap_or(false)
                && p.file_stem()
                    .and_then(|n| n.to_str())
                    .map(|n| n.len() >= 4 && n[..4].chars().all(|c| c.is_ascii_digit()))
                    .unwrap_or(false)
        })
        .collect();
    files.sort();

    for path in &files {
        let stem = path.file_stem().unwrap().to_str().unwrap();
        let num = &stem[..4];
        let desc = description_from_file(path);

        match state.get(stem) {
            Some(r) if r.status == "ok" =>
                println!("  {GREEN}✓{RESET} [{num}] {desc}  (applied {})", r.applied_at),
            Some(r) if r.status == "failed" =>
                println!("  {RED}✗{RESET} [{num}] {desc}  (failed {})", r.applied_at),
            _ =>
                println!("  {YELLOW}○{RESET} [{num}] {desc}  (pending)"),
        }
    }
}

fn description_from_file(path: &PathBuf) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("# Description:"))
        .map(|l| l.trim_start_matches("# Description:").trim().to_string())
        .unwrap_or_else(|| path.file_stem().unwrap().to_str().unwrap().to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn with_state_file(content: &str) -> (NamedTempFile, PathBuf) {
        let f = NamedTempFile::new().unwrap();
        f.as_file().write_all(content.as_bytes()).unwrap();
        let path = f.path().to_path_buf();
        (f, path)
    }

    #[test]
    fn test_load_empty() {
        let path = PathBuf::from("/nonexistent/path/migrations.json");
        let state = load(&path);
        assert!(state.is_empty());
    }

    #[test]
    fn test_load_and_check() {
        let json = r#"{"0001_test": {"status": "ok", "appliedAt": "2026-01-01T00:00:00Z"}}"#;
        let (_f, path) = with_state_file(json);
        let state = load(&path);
        assert_eq!(state["0001_test"].status, "ok");
    }

    #[test]
    fn test_record_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("migrations.json");
        let mut state = State::new();
        state.insert("0001_test".to_string(), Record {
            status: "ok".to_string(),
            applied_at: "2026-01-01T00:00:00Z".to_string(),
        });
        save(&path, &state);
        let loaded = load(&path);
        assert_eq!(loaded["0001_test"].status, "ok");
    }
}
