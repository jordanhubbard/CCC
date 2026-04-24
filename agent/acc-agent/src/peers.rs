//! Peer discovery — query the hub for other agents in the cluster.
//!
//! Calls GET /api/agents/names?online=true and returns the list, excluding
//! this agent's own name. Results are best-effort: the hub's view lags by
//! at most one heartbeat interval (~30s).

use crate::config::Config;
use acc_client::Client;

/// Return the names of all currently-online peers (excluding self).
pub async fn list_peers(cfg: &Config, client: &Client) -> Vec<String> {
    match client.agents().names(true).await {
        Ok(names) => names
            .into_iter()
            .filter(|n| n != cfg.agent_name.as_str())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_mock::{HubMock, HubState};

    fn test_cfg(url: &str, name: &str) -> Config {
        Config {
            acc_dir: std::path::PathBuf::from("/tmp"),
            acc_url: url.to_string(),
            acc_token: "test-tok".to_string(),
            agent_name: name.to_string(),
            agentbus_token: String::new(),
            pair_programming: false,
            host: String::new(),
            ssh_user: "testuser".into(),
            ssh_host: "127.0.0.1".into(),
            ssh_port: 22,
        }
    }

    fn client_for(url: &str) -> Client {
        Client::new(url, "test-tok").expect("build client")
    }

    #[tokio::test]
    async fn test_list_peers_returns_others() {
        let mock = HubMock::with_state(HubState {
            agent_names: vec!["boris".into(), "natasha".into(), "bullwinkle".into()],
            ..Default::default()
        })
        .await;
        let client = client_for(&mock.url);
        let peers = list_peers(&test_cfg(&mock.url, "boris"), &client).await;
        assert!(!peers.contains(&"boris".to_string()), "must exclude self");
        assert!(peers.contains(&"natasha".to_string()));
        assert!(peers.contains(&"bullwinkle".to_string()));
    }

    #[tokio::test]
    async fn test_list_peers_empty_cluster() {
        let mock = HubMock::with_state(HubState {
            agent_names: vec!["boris".into()],
            ..Default::default()
        })
        .await;
        let client = client_for(&mock.url);
        let peers = list_peers(&test_cfg(&mock.url, "boris"), &client).await;
        assert!(peers.is_empty());
    }

    #[tokio::test]
    async fn test_list_peers_hub_unreachable_returns_empty() {
        let cfg = test_cfg("http://127.0.0.1:1", "boris");
        let client = client_for(&cfg.acc_url);
        let peers = list_peers(&cfg, &client).await;
        assert!(peers.is_empty());
    }

    #[tokio::test]
    async fn test_list_peers_no_agents_returns_empty() {
        let mock = HubMock::new().await;
        let client = client_for(&mock.url);
        let peers = list_peers(&test_cfg(&mock.url, "boris"), &client).await;
        assert!(peers.is_empty());
    }
}
