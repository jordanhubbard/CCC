/// rcc-server configuration — loads from rcc.json then falls back to env vars.
///
/// Priority (highest first):
///   1. Environment variables (allow CI/systemd overrides without touching the file)
///   2. ~/.rcc/rcc.json  (or path in RCC_CONFIG env var)
///   3. Hard-coded defaults
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── JSON schema ───────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct RccConfig {
    pub port: Option<u16>,
    pub data_dir: Option<String>,
    pub queue_path: Option<String>,
    pub agents_path: Option<String>,
    pub secrets_path: Option<String>,
    pub bus_log_path: Option<String>,
    pub projects_path: Option<String>,
    pub auth_tokens: Vec<String>,
    pub minio: MinioConfig,
    pub supervisor: SupervisorConfig,
    pub qdrant: QdrantConfig,
    pub tokenhub: TokenhubConfig,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct MinioConfig {
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
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
    pub minio_endpoint: String,
    pub minio_bucket: String,
    pub minio_access_key: Option<String>,
    pub minio_secret_key: Option<String>,
    pub supervisor_enabled: bool,
    pub tokenhub_bin: String,
    pub qdrant_url: String,
    pub qdrant_api_key: Option<String>,
    pub tokenhub_url: String,
}

// ── Loader ────────────────────────────────────────────────────────────────────

fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("RCC_CONFIG") {
        return PathBuf::from(p);
    }
    // ~/.rcc/rcc.json
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".rcc").join("rcc.json")
}

fn load_json_config() -> RccConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<RccConfig>(&contents) {
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
                RccConfig::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("No config file at {} — using env/defaults", path.display());
            RccConfig::default()
        }
        Err(e) => {
            tracing::warn!(
                "Failed to read {}: {} — using env/defaults",
                path.display(),
                e
            );
            RccConfig::default()
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

    let data_dir = resolve_str("CCC_DATA_DIR", j.data_dir.clone(), "./data");

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

    let port: u16 = evar("RCC_PORT")
        .and_then(|s| s.parse().ok())
        .or(j.port)
        .unwrap_or(8789);

    // Auth tokens: env wins, then JSON array, then empty (open dev mode)
    let auth_tokens: std::collections::HashSet<String> = if let Some(raw) = evar("CCC_AUTH_TOKENS")
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

    let minio_endpoint = resolve_str("MINIO_ENDPOINT", j.minio.endpoint, "http://localhost:9000");
    let minio_bucket = resolve_str("MINIO_BUCKET", j.minio.bucket, "agents");
    let minio_access_key = evar("MINIO_ACCESS_KEY").or(j.minio.access_key);
    let minio_secret_key = evar("MINIO_SECRET_KEY").or(j.minio.secret_key);

    let supervisor_enabled = evar("SUPERVISOR_ENABLED")
        .map(|s| s == "true")
        .unwrap_or(j.supervisor.enabled);
    let tokenhub_bin = resolve_str(
        "TOKENHUB_BIN",
        j.supervisor.tokenhub_bin,
        "/home/jkh/tokenhub/bin/tokenhub",
    );

    let qdrant_url = resolve_str(
        "QDRANT_FLEET_URL",
        j.qdrant.url,
        "http://100.89.199.14:6333",
    );
    let qdrant_api_key = evar("QDRANT_FLEET_KEY").or(j.qdrant.api_key);
    let tokenhub_url = resolve_str("TOKENHUB_URL", j.tokenhub.url, "http://127.0.0.1:8090");

    ResolvedConfig {
        port,
        data_dir,
        queue_path,
        agents_path,
        secrets_path,
        bus_log_path,
        projects_path,
        auth_tokens,
        minio_endpoint,
        minio_bucket,
        minio_access_key,
        minio_secret_key,
        supervisor_enabled,
        tokenhub_bin,
        qdrant_url,
        qdrant_api_key,
        tokenhub_url,
    }
}
