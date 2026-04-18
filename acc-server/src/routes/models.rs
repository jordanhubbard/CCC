/// /routes/models.rs — Model deployment orchestration API
///
/// POST /api/models/deploy
///   { "model_id": "google/gemma-4-31B-it", "agents": ["boris", "peabody"] }
///   Validates model, dispatches model-deploy.mjs orchestration script.
///   Returns a deploy_id for status polling.
///
/// GET /api/models/deploy/:id
///   Returns deploy status.
///
/// GET /api/models/current
///   Returns current model running on each Sweden node (via their tunnels).

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use crate::AppState;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployRequest {
    pub model_id: String,
    pub agents: Option<Vec<String>>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeployStatus {
    Queued,
    Validating,
    Downloading,
    Deploying,
    Verifying,
    Succeeded,
    PartialSuccess,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployRecord {
    pub id: String,
    pub model_id: String,
    pub agents: Vec<String>,
    pub dry_run: bool,
    pub status: DeployStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub log: Vec<String>,
    pub result: Option<Value>,
    pub pid: Option<u32>,
}

pub type DeployStore = Arc<RwLock<HashMap<String, DeployRecord>>>;

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/models/deploy", post(trigger_deploy))
        .route("/api/models/deploy/:id", get(get_deploy_status))
        .route("/api/models/current", get(current_models))
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /api/models/deploy
async fn trigger_deploy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<DeployRequest>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }

    // Validate model_id is non-empty and looks like a HF model path
    let model_id = body.model_id.trim().to_string();
    if model_id.is_empty() || (!model_id.contains('/') && !model_id.starts_with("meta-llama")) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "model_id must be a valid HuggingFace model ID (e.g. google/gemma-4-31B-it)"}))
        ).into_response();
    }

    let all_agents = vec![
        "boris".to_string(),
        "peabody".to_string(),
        "sherman".to_string(),
        "snidely".to_string(),
        "dudley".to_string(),
    ];
    let agents = body.agents.unwrap_or(all_agents);
    let dry_run = body.dry_run.unwrap_or(false);

    // Generate deploy ID
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let deploy_id = format!("deploy-{}", ts);

    // Find the model-deploy.mjs script
    let script_path = std::env::var("DEPLOY_SCRIPT")
        .unwrap_or_else(|_| {
            // Try relative paths from common locations
            let candidates = [
                "../scripts/model-deploy.mjs",
                "./scripts/model-deploy.mjs",
            ];
            candidates.iter()
                .find(|p| std::path::Path::new(p).exists())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "..ccc/scripts/model-deploy.mjs".to_string())
        });

    // Build args
    let mut args = vec![
        script_path.clone(),
        format!("--model={}", model_id),
        format!("--agents={}", agents.join(",")),
    ];
    if dry_run {
        args.push("--validate".to_string());
    } else {
        args.push("--deploy".to_string());
    }

    // Spawn the orchestration script in background
    let env_token = state.secrets.read().await
        .get("ACC_AGENT_TOKEN").cloned()
        .map(|v| v.as_str().unwrap_or("").to_string()).unwrap_or_else(|| std::env::var("ACC_AGENT_TOKEN").unwrap_or_default());
    let hf_token = state.secrets.read().await
        .get("HF_TOKEN").cloned()
        .map(|v| v.as_str().unwrap_or("").to_string()).unwrap_or_else(|| std::env::var("HF_TOKEN").unwrap_or_default());

    let log_path = format!("/tmp/model-deploy-{}.log", deploy_id);
    let log_path_clone = log_path.clone();
    let deploy_id_clone = deploy_id.clone();
    let model_id_clone = model_id.clone();
    let agents_clone = agents.clone();

    tokio::spawn(async move {
        let log_file = std::fs::File::create(&log_path_clone).ok();
        let stdio = if let Some(f) = log_file {
            std::process::Stdio::from(f)
        } else {
            std::process::Stdio::null()
        };

        let mut cmd = tokio::process::Command::new("node");
        cmd.args(&args)
            .env("ACC_AGENT_TOKEN", &env_token)
            .env("HF_TOKEN", &hf_token)
            .env("DEPLOY_ITEM_ID", "")
            .env("ACC_URL", std::env::var("ACC_URL").unwrap_or_else(|_| "http://localhost:8789".to_string()))
            .stdout(stdio)
            .stderr(std::process::Stdio::null());

        match cmd.spawn() {
            Ok(mut child) => {
                tracing::info!("model-deploy: spawned pid={:?} for deploy_id={}", child.id(), deploy_id_clone);
                let _ = child.wait().await;
            }
            Err(e) => {
                tracing::error!("model-deploy: failed to spawn script: {}", e);
            }
        }
    });

    // Return immediately with deploy_id for status polling
    Json(json!({
        "ok": true,
        "deploy_id": deploy_id,
        "model_id": model_id,
        "agents": agents,
        "dry_run": dry_run,
        "status": "queued",
        "log_path": log_path,
        "message": format!(
            "Deploy started. Follow with: GET /api/models/deploy/{}. Log: {}",
            deploy_id, log_path
        )
    })).into_response()
}

/// GET /api/models/deploy/:id
/// Returns deploy log and status by reading the log file.
async fn get_deploy_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(deploy_id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }

    let log_path = format!("/tmp/model-deploy-{}.log", deploy_id);
    let log_content = std::fs::read_to_string(&log_path).unwrap_or_default();
    let lines: Vec<&str> = log_content.lines().collect();

    // Infer status from log content
    let status = if log_content.contains("Deploy complete.") {
        if log_content.contains("Failed:  none") {
            "succeeded"
        } else if log_content.contains("Failed:") {
            "partial_success"
        } else {
            "succeeded"
        }
    } else if log_content.contains("ERROR:") {
        "failed"
    } else if log_content.contains("DRY RUN complete") {
        "validated"
    } else if !log_content.is_empty() {
        "running"
    } else {
        "queued"
    };

    Json(json!({
        "deploy_id": deploy_id,
        "status": status,
        "log_lines": lines.len(),
        "log_tail": lines.iter().rev().take(20).rev().collect::<Vec<_>>(),
        "log_path": log_path,
    })).into_response()
}

/// GET /api/models/current
/// Queries each Sweden node tunnel for currently loaded models.
async fn current_models(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }

    let agents = [
        ("boris",   18080u16),
        ("peabody", 18081),
        ("sherman", 18082),
        ("snidely", 18083),
        ("dudley",  18084),
    ];

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let mut results = Vec::new();
    for (name, port) in &agents {
        let url = format!("http://127.0.0.1:{}/v1/models", port);
        let entry = match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<Value>().await {
                    Ok(data) => {
                        let models: Vec<String> = data["data"].as_array()
                            .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect())
                            .unwrap_or_default();
                        json!({ "agent": name, "port": port, "status": "ok", "models": models })
                    }
                    Err(_) => json!({ "agent": name, "port": port, "status": "error", "models": [] }),
                }
            }
            _ => json!({ "agent": name, "port": port, "status": "unreachable", "models": [] }),
        };
        results.push(entry);
    }

    Json(json!({ "nodes": results })).into_response()
}
