//! Shared utilities for acc-tools binaries.

use acc_client::llm_config::LlmConfig;
use serde_json::Value;
use std::path::PathBuf;

/// Resolve Qdrant API key: QDRANT_API_KEY env → docker inspect fallback.
pub fn resolve_qdrant_api_key() -> Option<String> {
    if let Ok(k) = std::env::var("QDRANT_API_KEY") {
        if !k.is_empty() {
            return Some(k);
        }
    }
    // docker inspect fallback
    if let Ok(out) = std::process::Command::new("docker")
        .args(["inspect", "qdrant"])
        .output()
    {
        if let Ok(text) = std::str::from_utf8(&out.stdout) {
            if let Ok(val) = serde_json::from_str::<Value>(text) {
                if let Some(envs) = val[0]["Config"]["Env"].as_array() {
                    for env in envs {
                        if let Some(s) = env.as_str() {
                            if s.contains("API_KEY") {
                                let parts: Vec<&str> = s.splitn(2, '=').collect();
                                if parts.len() == 2 {
                                    return Some(parts[1].to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Build an EmbedClient from LlmConfig, using NVIDIA NIM defaults.
///
/// Uses `embed_url` → falls back to `base_url` → then NVIDIA default.
pub fn make_embed_client() -> Result<acc_qdrant::EmbedClient, acc_qdrant::QdrantError> {
    let cfg = LlmConfig::load();
    let base_url = if !cfg.embed_url.is_empty() {
        cfg.embed_url.clone()
    } else if !cfg.base_url.is_empty() {
        // Strip /v1 suffix if present, then re-add for embed endpoint
        cfg.base_url.trim_end_matches("/v1").to_string() + "/v1"
    } else {
        "https://integrate.api.nvidia.com/v1".to_string()
    };
    let api_key = if !cfg.embed_key.is_empty() {
        cfg.embed_key.clone()
    } else if !cfg.api_key.is_empty() {
        cfg.api_key.clone()
    } else {
        return Err(acc_qdrant::QdrantError::Config(
            "No embedding API key found (set NVIDIA_API_KEY or OPENAI_API_KEY)".to_string(),
        ));
    };
    let model = if cfg.embed_model.is_empty() {
        "text-embedding-3-large".to_string()
    } else {
        cfg.embed_model.clone()
    };
    acc_qdrant::EmbedClient::new(&base_url, &api_key, &model)
}

/// Load ~/.acc/.env or ~/.ccc/.env into the process environment (env var takes precedence).
pub fn load_acc_env() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    for dir in [".acc", ".ccc"] {
        let path = PathBuf::from(&home).join(dir).join(".env");
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let k = k.trim();
                    let v = v.trim().trim_matches('"').trim_matches('\'');
                    if std::env::var(k).is_err() {
                        // SAFETY: single-threaded startup
                        unsafe { std::env::set_var(k, v) };
                    }
                }
            }
            return;
        }
    }
}

/// Resolve ACC API base URL: ACC_URL → CCC_URL → http://localhost:8789
pub fn acc_url() -> String {
    std::env::var("ACC_URL")
        .or_else(|_| std::env::var("CCC_URL"))
        .unwrap_or_else(|_| "http://localhost:8789".to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Resolve ACC agent token: ACC_AGENT_TOKEN → CCC_AGENT_TOKEN → ACC_TOKEN → ""
pub fn acc_token() -> String {
    std::env::var("ACC_AGENT_TOKEN")
        .or_else(|_| std::env::var("CCC_AGENT_TOKEN"))
        .or_else(|_| std::env::var("ACC_TOKEN"))
        .unwrap_or_default()
}
