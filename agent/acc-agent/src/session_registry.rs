use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use acc_model::{AgentCapacity, AgentExecutor, AgentSession, HeartbeatRequest};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::session_discovery::{self, DiscoverySnapshot};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RegistryDisk {
    #[serde(default)]
    executors: Vec<AgentExecutor>,
    #[serde(default)]
    sessions: Vec<AgentSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capacity: Option<AgentCapacity>,
}

#[derive(Debug, Clone, Default)]
struct RegistryState {
    executors: Vec<AgentExecutor>,
    sessions: Vec<AgentSession>,
    capacity: AgentCapacity,
    last_refresh: Option<Instant>,
}

pub struct SessionRegistry {
    path: PathBuf,
    state: RwLock<RegistryState>,
}

impl SessionRegistry {
    fn load(cfg: &Config) -> Arc<Self> {
        let path = cfg.session_registry_file();
        let disk = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<RegistryDisk>(&s).ok())
            .unwrap_or_default();
        Arc::new(Self {
            path,
            state: RwLock::new(RegistryState {
                executors: disk.executors,
                sessions: disk.sessions,
                capacity: disk.capacity.unwrap_or_default(),
                last_refresh: None,
            }),
        })
    }

    async fn refresh_if_stale(&self, cfg: &Config) {
        let stale = {
            let state = self.state.read().await;
            state
                .last_refresh
                .map(|t| t.elapsed() > Duration::from_secs(30))
                .unwrap_or(true)
        };
        if stale {
            self.refresh(cfg).await;
        }
    }

    async fn refresh(&self, cfg: &Config) {
        let snapshot = session_discovery::discover(cfg).await;
        let capacity = build_capacity(cfg, &snapshot.sessions);
        {
            let mut state = self.state.write().await;
            state.executors = snapshot.executors.clone();
            state.sessions = snapshot.sessions.clone();
            state.capacity = capacity.clone();
            state.last_refresh = Some(Instant::now());
        }
        persist_registry(&self.path, &snapshot, &capacity);
    }

    async fn heartbeat_fragment(&self, cfg: &Config) -> HeartbeatFragment {
        self.refresh_if_stale(cfg).await;
        let state = self.state.read().await;
        HeartbeatFragment {
            executors: state.executors.clone(),
            sessions: state.sessions.clone(),
            free_session_slots: state.capacity.free_session_slots,
            max_sessions: state.capacity.max_sessions,
            session_spawn_denied_reason: state.capacity.session_spawn_denied_reason.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HeartbeatFragment {
    pub executors: Vec<AgentExecutor>,
    pub sessions: Vec<AgentSession>,
    pub free_session_slots: Option<u32>,
    pub max_sessions: Option<u32>,
    pub session_spawn_denied_reason: Option<String>,
}

pub async fn augment_heartbeat(cfg: &Config, req: &mut HeartbeatRequest) {
    let registry = shared(cfg);
    let fragment = registry.heartbeat_fragment(cfg).await;
    req.executors = fragment.executors;
    req.sessions = fragment.sessions;
    req.free_session_slots = fragment.free_session_slots;
    req.max_sessions = fragment.max_sessions;
    req.session_spawn_denied_reason = fragment.session_spawn_denied_reason;
}

fn shared(cfg: &Config) -> Arc<SessionRegistry> {
    static CELL: OnceLock<Arc<SessionRegistry>> = OnceLock::new();
    CELL.get_or_init(|| SessionRegistry::load(cfg)).clone()
}

fn build_capacity(cfg: &Config, sessions: &[AgentSession]) -> AgentCapacity {
    let max_sessions = cfg.max_cli_sessions();
    let used_sessions = sessions
        .iter()
        .filter(|s| s.state.as_deref() != Some("dead"))
        .count() as u32;
    let free_session_slots = max_sessions.saturating_sub(used_sessions);
    let available_memory_mb = available_memory_mb();
    let session_spawn_denied_reason = if free_session_slots == 0 {
        Some("session_limit_reached".to_string())
    } else if let Some(free_mb) = available_memory_mb {
        if free_mb < cfg.session_min_free_memory_mb() {
            Some(format!("memory_pressure:{free_mb}mb"))
        } else {
            None
        }
    } else {
        None
    };

    AgentCapacity {
        free_session_slots: Some(free_session_slots),
        max_sessions: Some(max_sessions),
        session_spawn_denied_reason,
        ..Default::default()
    }
}

fn persist_registry(path: &PathBuf, snapshot: &DiscoverySnapshot, capacity: &AgentCapacity) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let disk = RegistryDisk {
        executors: snapshot.executors.clone(),
        sessions: snapshot.sessions.clone(),
        capacity: Some(capacity.clone()),
    };
    if let Ok(data) = serde_json::to_vec_pretty(&disk) {
        let _ = std::fs::write(path, data);
    }
}

fn available_memory_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let kb = rest.split_whitespace().next()?.parse::<u64>().ok()?;
                return Some(kb / 1024);
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("vm_stat").output().ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let page_size = 4096u64;
        let mut free_pages = 0u64;
        for line in stdout.lines() {
            if line.starts_with("Pages free:") || line.starts_with("Pages speculative:") {
                let value = line
                    .split(':')
                    .nth(1)?
                    .trim()
                    .trim_end_matches('.')
                    .replace('.', "");
                free_pages += value.parse::<u64>().ok()?;
            }
        }
        Some((free_pages * page_size) / (1024 * 1024))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str, state: &str) -> AgentSession {
        AgentSession {
            name: name.to_string(),
            state: Some(state.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn build_capacity_counts_non_dead_sessions() {
        std::env::set_var("ACC_MAX_CLI_SESSIONS", "3");
        let cfg = Config {
            acc_dir: std::env::temp_dir(),
            acc_url: "http://example.test".into(),
            acc_token: "tok".into(),
            agent_name: "agent".into(),
            agentbus_token: "bus".into(),
            pair_programming: true,
            host: "host".into(),
            ssh_user: "user".into(),
            ssh_host: "host".into(),
            ssh_port: 22,
        };
        let capacity = build_capacity(
            &cfg,
            &[
                session("a", "busy"),
                session("b", "dead"),
                session("c", "idle"),
            ],
        );
        assert_eq!(capacity.max_sessions, Some(3));
        assert_eq!(capacity.free_session_slots, Some(1));
        std::env::remove_var("ACC_MAX_CLI_SESSIONS");
    }
}
