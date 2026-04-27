//! hermes-rust — native Rust agent session driver.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

mod agent;
mod conversation;
mod provider;
mod tool;

use agent::HermesAgent;
use provider::make_provider;
use tool::ToolRegistry;
use acc_client::Client;
use crate::config::Config;

pub async fn run(args: &[String]) {
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

    eprintln!("[hermes-rust v{}] agent={} hub={}", VERSION, cfg.agent_name, cfg.acc_url);

    let mut item_id: Option<String> = None;
    let mut query: Option<String> = None;
    let mut poll = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--item" => { i += 1; item_id = args.get(i).cloned(); }
            "--query" => { i += 1; query = args.get(i).cloned(); }
            "--poll" => poll = true,
            _ => {}
        }
        i += 1;
    }

    let client = build_client(&cfg);
    let tools = ToolRegistry::default_tools();

    let model = std::env::var("HERMES_MODEL")
        .unwrap_or_else(|_| "claude-opus-4-7".to_string());
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .unwrap_or_default();
    let provider = make_provider(api_key, model);

    let hermes = HermesAgent::new(cfg, client, provider, tools);

    if poll {
        hermes.poll_queue().await;
    } else if let Some(id) = item_id {
        let q = query.unwrap_or_default();
        hermes.run_item(id, q).await;
    } else if let Some(q) = query {
        hermes.run_query(q).await;
    } else {
        eprintln!("[hermes-rust] one of --item, --query, --poll required");
        std::process::exit(1);
    }
}

fn build_client(cfg: &Config) -> Client {
    Client::new(&cfg.acc_url, &cfg.acc_token).expect("failed to build HTTP client")
}
