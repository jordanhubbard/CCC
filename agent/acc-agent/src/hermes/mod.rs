//! hermes-rust — native Rust agent session driver.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

mod agent;
mod conversation;
mod gateway;
mod provider;
mod tool;

use crate::config::Config;
use acc_client::Client;
use agent::HermesAgent;
use provider::make_provider;
use tool::ToolRegistry;

pub async fn run(args: &[String]) {
    // Gateway mode: long-running Slack/Telegram bot.
    if args.iter().any(|a| a == "--gateway") {
        // Optional --workspace <name> selects which set of env vars to use.
        // e.g. --workspace ofterra → reads SLACK_APP_TOKEN_OFTERRA, SLACK_BOT_TOKEN_OFTERRA
        let workspace = args
            .windows(2)
            .find(|w| w[0] == "--workspace")
            .map(|w| w[1].to_uppercase());
        gateway::run(workspace.as_deref()).await;
        return;
    }
    native_run(args).await;
}

async fn native_run(args: &[String]) {
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[hermes-rust] config error: {e}");
            std::process::exit(1);
        }
    };

    eprintln!(
        "[hermes-rust v{}] agent={} hub={}",
        VERSION, cfg.agent_name, cfg.acc_url
    );

    let mut item_id: Option<String> = None;
    let mut task_id: Option<String> = None;
    let mut query: Option<String> = None;
    let mut poll = false;
    let mut poll_queue_legacy = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--item" => {
                i += 1;
                item_id = args.get(i).cloned();
            }
            "--task" => {
                i += 1;
                task_id = args.get(i).cloned();
            }
            "--query" => {
                i += 1;
                query = args.get(i).cloned();
            }
            "--poll" => poll = true,
            "--poll-queue" => poll_queue_legacy = true,
            _ => {}
        }
        i += 1;
    }

    let client = build_client(&cfg);
    let tools = ToolRegistry::default_tools();

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

    let hermes = HermesAgent::new(cfg, client, provider, tools);

    if poll {
        hermes.poll_tasks().await;
    } else if poll_queue_legacy {
        hermes.poll_queue_legacy().await;
    } else if let Some(id) = task_id {
        let q = query.unwrap_or_default();
        hermes.run_task(id, q).await;
    } else if let Some(id) = item_id {
        let q = query.unwrap_or_default();
        hermes.run_queue_item(id, q).await;
    } else if let Some(q) = query {
        hermes.run_query(q).await;
    } else {
        eprintln!("[hermes-rust] one of --poll, --poll-queue, --task, --item, or --query required");
        std::process::exit(1);
    }
}

fn build_client(cfg: &Config) -> Client {
    Client::new(&cfg.acc_url, &cfg.acc_token).expect("failed to build HTTP client")
}
