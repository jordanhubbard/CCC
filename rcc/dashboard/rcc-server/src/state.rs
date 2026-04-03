use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::{RwLock, broadcast};
use crate::brain::BrainQueue;
use crate::routes::metrics::MetricPoint;
use crate::supervisor::SupervisorHandle;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct QueueData {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
    #[serde(default)]
    pub completed: Vec<serde_json::Value>,
}

pub struct AppState {
    pub auth_tokens: HashSet<String>,
    pub queue_path: String,
    pub agents_path: String,
    pub secrets_path: String,
    pub bus_log_path: String,
    pub projects_path: String,
    pub metrics_path: String,
    pub queue: RwLock<QueueData>,
    pub agents: RwLock<serde_json::Value>,
    pub secrets: RwLock<serde_json::Map<String, serde_json::Value>>,
    pub projects: RwLock<Vec<serde_json::Value>>,
    pub metrics: RwLock<HashMap<String, Vec<MetricPoint>>>,
    pub brain: Arc<BrainQueue>,
    pub bus_tx: broadcast::Sender<String>,
    pub bus_seq: AtomicU64,
    pub start_time: std::time::SystemTime,
    pub s3_client: Option<Arc<aws_sdk_s3::Client>>,
    pub s3_bucket: String,
    pub supervisor: Option<Arc<SupervisorHandle>>,
}

impl AppState {
    pub fn is_authed(&self, headers: &axum::http::HeaderMap) -> bool {
        if self.auth_tokens.is_empty() {
            return true; // open / dev mode
        }
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = auth.trim_start_matches("Bearer ").trim();
        // Timing-safe compare using subtle
        use subtle::ConstantTimeEq;
        for valid in &self.auth_tokens {
            let a: &[u8] = token.as_bytes();
            let b: &[u8] = valid.as_bytes();
            if a.len() == b.len() && bool::from(a.ct_eq(b)) {
                return true;
            }
        }
        false
    }
}

pub async fn load_all(state: &Arc<AppState>) {
    load_queue(state).await;
    load_agents(state).await;
    load_secrets(state).await;
    load_projects(state).await;
    load_metrics(state).await;
}

pub async fn load_metrics(state: &Arc<AppState>) {
    match tokio::fs::read_to_string(&state.metrics_path).await {
        Ok(content) => {
            if let Ok(data) = serde_json::from_str::<HashMap<String, Vec<MetricPoint>>>(&content) {
                *state.metrics.write().await = data;
                tracing::info!("Loaded metrics from {}", state.metrics_path);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("Metrics file not found, starting empty");
        }
        Err(e) => tracing::warn!("Failed to load metrics: {}", e),
    }
}

pub async fn flush_metrics(state: &Arc<AppState>) {
    let data = state.metrics.read().await;
    let content = match serde_json::to_string_pretty(&*data) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to serialize metrics: {}", e);
            return;
        }
    };
    drop(data);
    if let Err(e) = write_atomic(&state.metrics_path, &content).await {
        tracing::warn!("Failed to flush metrics: {}", e);
    }
}

pub async fn load_projects(state: &Arc<AppState>) {
    match tokio::fs::read_to_string(&state.projects_path).await {
        Ok(content) => {
            if let Ok(data) = serde_json::from_str::<Vec<serde_json::Value>>(&content) {
                *state.projects.write().await = data;
                tracing::info!("Loaded projects from {}", state.projects_path);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("Projects file not found, starting empty");
        }
        Err(e) => tracing::warn!("Failed to load projects: {}", e),
    }
}

pub async fn load_queue(state: &Arc<AppState>) {
    match tokio::fs::read_to_string(&state.queue_path).await {
        Ok(content) => {
            if let Ok(data) = serde_json::from_str::<QueueData>(&content) {
                *state.queue.write().await = data;
                tracing::info!("Loaded queue from {}", state.queue_path);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("Queue file not found, starting empty");
        }
        Err(e) => {
            tracing::warn!("Failed to load queue: {}", e);
        }
    }
}

pub async fn flush_queue(state: &Arc<AppState>) {
    let data = state.queue.read().await;
    let content = match serde_json::to_string_pretty(&*data) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to serialize queue: {}", e);
            return;
        }
    };
    drop(data);
    if let Err(e) = write_atomic(&state.queue_path, &content).await {
        tracing::warn!("Failed to flush queue: {}", e);
    }
}

pub async fn load_agents(state: &Arc<AppState>) {
    match tokio::fs::read_to_string(&state.agents_path).await {
        Ok(content) => {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                let obj = if data.is_array() {
                    // legacy array format -> convert to map
                    let mut map = serde_json::Map::new();
                    for agent in data.as_array().unwrap() {
                        if let Some(name) = agent.get("name").and_then(|n| n.as_str()) {
                            map.insert(name.to_string(), agent.clone());
                        }
                    }
                    serde_json::Value::Object(map)
                } else {
                    data
                };
                *state.agents.write().await = obj;
                tracing::info!("Loaded agents from {}", state.agents_path);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("Agents file not found, starting empty");
        }
        Err(e) => tracing::warn!("Failed to load agents: {}", e),
    }
}

pub async fn flush_agents(state: &Arc<AppState>) {
    let data = state.agents.read().await;
    let content = match serde_json::to_string_pretty(&*data) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to serialize agents: {}", e);
            return;
        }
    };
    drop(data);
    if let Some(parent) = std::path::Path::new(&state.agents_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = write_atomic(&state.agents_path, &content).await {
        tracing::warn!("Failed to flush agents: {}", e);
    }
}

pub async fn load_secrets(state: &Arc<AppState>) {
    match tokio::fs::read_to_string(&state.secrets_path).await {
        Ok(content) => {
            if let Ok(data) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&content) {
                *state.secrets.write().await = data;
                tracing::info!("Loaded secrets from {}", state.secrets_path);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("Failed to load secrets: {}", e),
    }
}

pub async fn flush_secrets(state: &Arc<AppState>) {
    let data = state.secrets.read().await;
    let content = match serde_json::to_string_pretty(&*data) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to serialize secrets: {}", e);
            return;
        }
    };
    drop(data);
    if let Some(parent) = std::path::Path::new(&state.secrets_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = write_atomic(&state.secrets_path, &content).await {
        tracing::warn!("Failed to flush secrets: {}", e);
    }
    // chmod 600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            &state.secrets_path,
            std::fs::Permissions::from_mode(0o600),
        );
    }
}

async fn write_atomic(path: &str, content: &str) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = format!("{}.tmp", path);
    tokio::fs::write(&tmp, content).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}
