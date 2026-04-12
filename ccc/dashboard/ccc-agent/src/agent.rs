use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
struct AgentJson {
    schema_version: u32,
    agent_name: String,
    host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    onboarded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    onboarded_by: Option<String>,
    ccc_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_upgraded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_upgraded_version: Option<String>,
    // Preserve any extra fields the file may have had
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn parse_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let prefix = format!("--{}=", flag);
    args.iter()
        .find(|a| a.starts_with(&prefix))
        .map(|a| a[prefix.len()..].as_ref())
}

fn require_flag<'a>(args: &'a [String], flag: &str) -> &'a str {
    parse_flag(args, flag).unwrap_or_else(|| {
        eprintln!("Missing required flag: --{flag}=<value>");
        std::process::exit(2);
    })
}

pub fn run(args: &[String]) {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { &[] } else { &args[1..] };
    match sub {
        "init"    => cmd_init(rest),
        "upgrade" => cmd_upgrade(rest),
        _ => {
            eprintln!("Usage: ccc-agent agent <init|upgrade>");
            std::process::exit(1);
        }
    }
}

// ── init — write agent.json on first onboarding ───────────────────────────
//
// ccc-agent agent init <path> --name=X --host=X --version=X [--by=X]
//
// Idempotent: if the file already exists it is NOT overwritten (use upgrade
// to update version fields on subsequent runs).

fn cmd_init(args: &[String]) {
    let path = args.first().unwrap_or_else(|| {
        eprintln!("Usage: ccc-agent agent init <path> --name=X --host=X --version=X [--by=X]");
        std::process::exit(2);
    });
    let path = PathBuf::from(path);

    if path.exists() {
        // Already exists — do nothing (idempotent)
        return;
    }

    let name    = require_flag(args, "name");
    let host    = require_flag(args, "host");
    let version = require_flag(args, "version");
    let by      = parse_flag(args, "by").unwrap_or("ccc-agent");
    let now     = now_utc();

    let record = AgentJson {
        schema_version: 1,
        agent_name: name.to_string(),
        host: host.to_string(),
        onboarded_at: Some(now.clone()),
        onboarded_by: Some(by.to_string()),
        ccc_version: version.to_string(),
        last_upgraded_at: Some(now.clone()),
        last_upgraded_version: Some(version.to_string()),
        extra: serde_json::Map::new(),
    };

    write_json(&path, &record);
}

// ── upgrade — update version fields in an existing agent.json ────────────
//
// ccc-agent agent upgrade <path> --version=X
//
// Creates the file if it doesn't exist (same as init with sensible defaults).

fn cmd_upgrade(args: &[String]) {
    let path = args.first().unwrap_or_else(|| {
        eprintln!("Usage: ccc-agent agent upgrade <path> --version=X");
        std::process::exit(2);
    });
    let path    = PathBuf::from(path);
    let version = require_flag(args, "version");
    let now     = now_utc();

    let mut record: AgentJson = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        // First-time creation during --force upgrade
        let host = hostname();
        let agent_name = std::env::var("AGENT_NAME").unwrap_or_else(|_| "unknown".to_string());
        AgentJson {
            schema_version: 1,
            agent_name,
            host,
            onboarded_at: Some(now.clone()),
            onboarded_by: Some("upgrade-node.sh (--force)".to_string()),
            ..Default::default()
        }
    };

    record.ccc_version = version.to_string();
    record.last_upgraded_at = Some(now);
    record.last_upgraded_version = Some(version.to_string());

    write_json(&path, &record);
}

// ── helpers ───────────────────────────────────────────────────────────────

fn write_json(path: &PathBuf, record: &AgentJson) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut content = serde_json::to_string_pretty(record)
        .unwrap_or_else(|_| "{}".to_string());
    content.push('\n');
    std::fs::write(path, &content).unwrap_or_else(|e| {
        eprintln!("Failed to write {}: {e}", path.display());
        std::process::exit(1);
    });
    // chmod 600 equivalent
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms).ok();
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .or_else(|_| {
            std::process::Command::new("hostname")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|_| "unknown".to_string())
}
