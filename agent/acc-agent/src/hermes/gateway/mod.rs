mod session;
mod slack;
mod telegram;

use super::agent::HermesAgent;
use super::provider::make_provider;
use super::slack_api::SlackApiClient;
use super::slack_tools::{all_slack_tools, SlackMemorySearchTool};
use super::tool::{Tool, ToolRegistry};
use crate::config::Config;
use acc_client::Client;
use acc_qdrant::QdrantClient;
use session::SessionStore;
use std::sync::Arc;

/// Run the gateway for a specific workspace.
///
/// Token resolution: secret store first (preferred), env-var fallback for
/// dev. See `resolve_slack_tokens` for the exact key format and fallback
/// chain. The legacy `SLACK_APP_TOKEN{_WORKSPACE}` env-var path remains
/// supported so existing dev setups keep working.
pub async fn run(workspace: Option<&str>) {
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[hermes-gateway] config error: {e}");
            std::process::exit(1);
        }
    };

    let ws_label = workspace_slug(workspace);
    eprintln!(
        "[hermes-gateway] starting agent={} hub={} workspace={ws_label}",
        cfg.agent_name, cfg.acc_url
    );

    let llm_cfg = acc_client::llm_config::LlmConfig::load();
    let model = if llm_cfg.model.is_empty() {
        "claude-opus-4-7".to_string()
    } else {
        llm_cfg.model.clone()
    };
    let api_key = if !llm_cfg.anthropic_key.is_empty() {
        llm_cfg.anthropic_key
    } else {
        llm_cfg.api_key
    };
    let provider = make_provider(api_key, model);

    let client = Client::new(&cfg.acc_url, &cfg.acc_token).expect("acc client");

    // Resolve Slack tokens early so we can register Slack-aware LLM tools
    // on the same registry the agent uses.
    let slack_tokens = resolve_slack_tokens(&client, workspace, &cfg.agent_name).await;

    let mut tool_list = ToolRegistry::default_tools_vec();
    let slack_api = if let Some((bot_token, _)) = slack_tokens.as_ref() {
        let api = Arc::new(SlackApiClient::new(bot_token.clone()));
        tool_list.extend(all_slack_tools(api.clone()));
        Some(api)
    } else {
        None
    };

    // slack_memory_search is registered when both Qdrant and an embed
    // client are configured. The hub host always has Qdrant local; other
    // bot hosts query it remotely if QDRANT_URL is set on their host.
    let memory_search_count = match build_memory_search_tool() {
        Ok(Some(tool)) => {
            tool_list.push(tool);
            1
        }
        Ok(None) => 0,
        Err(e) => {
            eprintln!("[hermes-gateway/{ws_label}] slack_memory_search disabled: {e}");
            0
        }
    };

    let tools = ToolRegistry::new(tool_list);

    let agent = Arc::new(HermesAgent::new(cfg.clone(), client.clone(), provider, tools));

    // Sessions are namespaced by workspace so conversations don't bleed across.
    let sessions_dir = cfg
        .acc_dir
        .join("data")
        .join("sessions")
        .join(&ws_label);
    let sessions = Arc::new(SessionStore::new(sessions_dir).with_hub(
        Client::new(&cfg.acc_url, &cfg.acc_token).expect("sessions client"),
        cfg.agent_name.clone(),
        ws_label.clone(),
    ));

    let mut handles = Vec::new();

    // Start Slack if tokens were resolved.
    match slack_tokens {
        Some((bot_token, app_token)) => {
            match slack::SlackAdapter::new(
                sessions.clone(),
                agent.clone(),
                client.clone(),
                ws_label.clone(),
                bot_token,
                app_token,
            )
            .await
            {
                Some(adapter) => {
                    let slack_tool_count = if slack_api.is_some() { 5 } else { 0 };
                    eprintln!(
                        "[hermes-gateway/{ws_label}] Slack adapter started \
                         ({} Slack tools, {} memory-search tools)",
                        slack_tool_count, memory_search_count
                    );
                    let adapter = Arc::new(adapter);
                    handles.push(tokio::spawn(async move { adapter.run().await }));
                }
                None => eprintln!(
                    "[hermes-gateway/{ws_label}] Slack tokens present but auth.test failed"
                ),
            }
        }
        None => eprintln!(
            "[hermes-gateway/{ws_label}] Slack not configured (no token in secret store or env)"
        ),
    }

    // Start Telegram if configured (only for default workspace — no per-workspace Telegram).
    if workspace.is_none() {
        match telegram::TelegramAdapter::new(sessions.clone(), agent.clone(), client.clone()).await {
            Some(adapter) => {
                eprintln!("[hermes-gateway/default] Telegram adapter started");
                let adapter = Arc::new(adapter);
                handles.push(tokio::spawn(async move { adapter.run().await }));
            }
            None => eprintln!(
                "[hermes-gateway/default] Telegram not configured (TELEGRAM_BOT_TOKEN missing)"
            ),
        }
    }

    if handles.is_empty() {
        eprintln!("[hermes-gateway/{ws_label}] no platforms configured — exiting");
        std::process::exit(1);
    }

    for h in handles {
        let _ = h.await;
    }
}

/// Normalize a workspace argument to a lowercase slug used for secret-store
/// keys, session-store directories, and log labels. `None` maps to `omgjkh`,
/// the historical default workspace.
fn workspace_slug(workspace: Option<&str>) -> String {
    workspace.unwrap_or("omgjkh").to_lowercase()
}

/// Build the slack_memory_search tool when Qdrant + embed config is
/// available. Returns `Ok(Some(tool))` when fully configured, `Ok(None)`
/// when Qdrant is intentionally not configured (no `QDRANT_URL`), and
/// `Err(reason)` when partially configured so the operator sees why.
fn build_memory_search_tool() -> Result<Option<Box<dyn Tool>>, String> {
    let qdrant_url = match std::env::var("QDRANT_URL").ok().filter(|s| !s.is_empty()) {
        Some(u) => u,
        None => return Ok(None),
    };
    let qdrant_key = std::env::var("QDRANT_API_KEY").ok().filter(|s| !s.is_empty());
    let qdrant = QdrantClient::new(&qdrant_url, qdrant_key.as_deref())
        .map_err(|e| format!("qdrant client: {e}"))?;
    let embed = acc_tools::make_embed_client().map_err(|e| format!("embed client: {e}"))?;

    let embed_dim = std::env::var("EMBED_DIM")
        .or_else(|_| std::env::var("NVIDIA_EMBED_DIM"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(1536);
    let collection = std::env::var("SLACK_INGEST_COLLECTION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("holographic_memory_{embed_dim}"));

    Ok(Some(Box::new(SlackMemorySearchTool::new(
        Arc::new(qdrant),
        Arc::new(embed),
        collection,
    ))))
}

/// Try the secret store first (key shape `slack/{workspace}/{bot}/{type}`),
/// fall back to env vars (`SLACK_APP_TOKEN{_WORKSPACE}` and
/// `SLACK_BOT_TOKEN{_WORKSPACE}`, plus the legacy `SLACK_OMGJKH_TOKEN`).
/// Returns `Some((bot_token, app_token))` only when both are present.
async fn resolve_slack_tokens(
    client: &Client,
    workspace: Option<&str>,
    bot_name: &str,
) -> Option<(String, String)> {
    let ws = workspace_slug(workspace);
    let bot = bot_name.to_lowercase();

    let bot_key = format!("slack/{ws}/{bot}/bot-token");
    let app_key = format!("slack/{ws}/{bot}/app-token");

    let mut bot_token = match client.secrets().get(&bot_key).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[hermes-gateway/{ws}] secret-store lookup {bot_key} failed: {e}");
            None
        }
    };
    let mut app_token = match client.secrets().get(&app_key).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[hermes-gateway/{ws}] secret-store lookup {app_key} failed: {e}");
            None
        }
    };

    let suffix = workspace
        .map(|w| format!("_{}", w.to_uppercase()))
        .unwrap_or_default();

    if bot_token.is_none() {
        bot_token = std::env::var(format!("SLACK_BOT_TOKEN{suffix}"))
            .ok()
            .or_else(|| {
                if suffix.is_empty() {
                    std::env::var("SLACK_OMGJKH_TOKEN").ok()
                } else {
                    None
                }
            })
            .filter(|t| !t.is_empty());
    }
    if app_token.is_none() {
        app_token = std::env::var(format!("SLACK_APP_TOKEN{suffix}"))
            .ok()
            .filter(|t| t.starts_with("xapp-"));
    }

    match (bot_token, app_token) {
        (Some(b), Some(a)) if !b.is_empty() && a.starts_with("xapp-") => Some((b, a)),
        _ => None,
    }
}
