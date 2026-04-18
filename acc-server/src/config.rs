/// acc-server configuration — loads from acc.json then falls back to env vars.
///
/// Priority (highest first):
///   1. Environment variables (allow CI/systemd overrides without touching the file)
///   2. ~/.acc/acc.json  (or path in ACC_CONFIG env var)
///   3. Hard-coded defaults
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── JSON schema ───────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct CccConfig {
    pub port: Option<u16>,
    pub data_dir: Option<String>,
    pub queue_path: Option<String>,
    pub agents_path: Option<String>,
    pub secrets_path: Option<String>,
    pub bus_log_path: Option<String>,
    pub projects_path: Option<String>,
    pub auth_tokens: Vec<String>,
    pub fs_root: Option<String>,
    pub supervisor: SupervisorConfig,
    pub qdrant: QdrantConfig,
    pub tokenhub: TokenhubConfig,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct SupervisorConfig {
    pub enabled: bool,
    pub tokenhub_bin: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct QdrantConfig {
    pub url: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct TokenhubConfig {
    pub url: Option<String>,
}

// ── Resolved (all fields concrete after merging JSON + env) ──────────────────

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub port: u16,
    pub data_dir: String,
    pub queue_path: String,
    pub agents_path: String,
    pub secrets_path: String,
    pub bus_log_path: String,
    pub projects_path: String,
    pub auth_tokens: std::collections::HashSet<String>,
    /// When set, acc-server uses SQLite for persistent state instead of JSON files.
    /// On first start, existing JSON data is migrated automatically.
    pub db_path: Option<String>,
    /// Path to the auth SQLite database (always-on, regardless of db_path).
    /// Default: ~/.ccc/auth.db
    pub auth_db_path: String,
    pub fs_root: String,
    pub supervisor_enabled: bool,
    pub tokenhub_bin: String,
    pub qdrant_url: String,
    pub qdrant_api_key: Option<String>,
    pub tokenhub_url: String,
}

// ── Loader ────────────────────────────────────────────────────────────────────

fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("ACC_CONFIG") {
        return PathBuf::from(p);
    }
    // ~/.acc/acc.json
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".acc").join("acc.json")
}

fn load_json_config() -> CccConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<CccConfig>(&contents) {
            Ok(cfg) => {
                tracing::info!("Loaded config from {}", path.display());
                cfg
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to parse {}: {} — using env/defaults",
                    path.display(),
                    e
                );
                CccConfig::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("No config file at {} — using env/defaults", path.display());
            CccConfig::default()
        }
        Err(e) => {
            tracing::warn!(
                "Failed to read {}: {} — using env/defaults",
                path.display(),
                e
            );
            CccConfig::default()
        }
    }
}

/// Merge: env var wins over json field wins over default.
fn evar(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

fn resolve_str(env_key: &str, json_val: Option<String>, default: &str) -> String {
    evar(env_key)
        .or(json_val)
        .unwrap_or_else(|| default.to_string())
}

pub fn load() -> ResolvedConfig {
    let j = load_json_config();

    let data_dir = resolve_str("ACC_DATA_DIR", j.data_dir.clone(), "./data");

    let queue_path = resolve_str(
        "QUEUE_PATH",
        j.queue_path,
        &format!("{}/queue.json", data_dir),
    );
    let agents_path = resolve_str(
        "AGENTS_PATH",
        j.agents_path,
        &format!("{}/agents.json", data_dir),
    );
    let secrets_path = resolve_str(
        "SECRETS_PATH",
        j.secrets_path,
        &format!("{}/secrets.json", data_dir),
    );
    let bus_log_path = resolve_str(
        "BUS_LOG_PATH",
        j.bus_log_path,
        &format!("{}/bus.jsonl", data_dir),
    );
    let projects_path = resolve_str(
        "PROJECTS_PATH",
        j.projects_path,
        &format!("{}/projects.json", data_dir),
    );

    let port: u16 = evar("ACC_PORT")
        .and_then(|s| s.parse().ok())
        .or(j.port)
        .unwrap_or(8789);

    // Auth tokens: env wins, then JSON array, then empty (open dev mode)
    let auth_tokens: std::collections::HashSet<String> = if let Some(raw) = evar("ACC_AUTH_TOKENS")
    {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        j.auth_tokens
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect()
    };

    let fs_root = resolve_str("ACC_FS_ROOT", j.fs_root, "/srv/accfs");

    let supervisor_enabled = evar("SUPERVISOR_ENABLED")
        .map(|s| s == "true")
        .unwrap_or(j.supervisor.enabled);
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let tokenhub_bin = resolve_str(
        "TOKENHUB_BIN",
        j.supervisor.tokenhub_bin,
        &format!("{}/tokenhub/bin/tokenhub", home),
    );

    let qdrant_url = resolve_str(
        "QDRANT_FLEET_URL",
        j.qdrant.url,
        "http://qdrant.service.consul:6333",
    );
    let qdrant_api_key = evar("QDRANT_FLEET_KEY").or(j.qdrant.api_key);
    let tokenhub_url = resolve_str("TOKENHUB_URL", j.tokenhub.url, "http://tokenhub.service.consul:8090");

    let db_path = evar("ACC_DB_PATH");

    let auth_db_path = evar("AUTH_DB_PATH")
        .unwrap_or_else(|| format!("{}/.acc/auth.db", home));

    ResolvedConfig {
        port,
        data_dir,
        queue_path,
        agents_path,
        secrets_path,
        bus_log_path,
        projects_path,
        auth_tokens,
        db_path,
        auth_db_path,
        fs_root,
        supervisor_enabled,
        tokenhub_bin,
        qdrant_url,
        qdrant_api_key,
        tokenhub_url,
    }
}
