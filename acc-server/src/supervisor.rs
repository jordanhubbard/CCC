use std::sync::Arc;
use tokio::sync::RwLock;

pub struct ManagedProcess {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub health_url: Option<String>,
    pub restart_delay_ms: u64,
}

#[derive(Clone, serde::Serialize)]
pub struct ProcessStatus {
    pub name: String,
    pub pid: Option<u32>,
    pub healthy: bool,
    pub restarts: u32,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct SupervisorHandle {
    pub statuses: Arc<RwLock<Vec<ProcessStatus>>>,
}

pub struct Supervisor {
    processes: Vec<ManagedProcess>,
    statuses: Arc<RwLock<Vec<ProcessStatus>>>,
}

impl Supervisor {
    pub fn new(processes: Vec<ManagedProcess>) -> (Self, Arc<SupervisorHandle>) {
        let initial: Vec<ProcessStatus> = processes
            .iter()
            .map(|p| ProcessStatus {
                name: p.name.clone(),
                pid: None,
                healthy: false,
                restarts: 0,
                started_at: None,
            })
            .collect();
        let statuses = Arc::new(RwLock::new(initial));
        let handle = Arc::new(SupervisorHandle {
            statuses: statuses.clone(),
        });
        (
            Supervisor {
                processes,
                statuses,
            },
            handle,
        )
    }

    pub async fn run(self) {
        let mut tasks = Vec::new();
        for (idx, process) in self.processes.into_iter().enumerate() {
            let statuses = self.statuses.clone();
            tasks.push(tokio::spawn(run_process(idx, process, statuses)));
        }
        for t in tasks {
            let _ = t.await;
        }
    }

    async fn is_healthy(url: &str) -> bool {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        match client.get(url).send().await {
            Ok(resp) => resp.status().as_u16() == 200,
            Err(_) => false,
        }
    }
}

async fn run_process(
    idx: usize,
    process: ManagedProcess,
    statuses: Arc<RwLock<Vec<ProcessStatus>>>,
) {
    let mut restart_count = 0u32;
    loop {
        tracing::info!(
            "[supervisor] starting '{}' ({})",
            process.name,
            process.command
        );
        let started_at = chrono::Utc::now();

        let mut cmd = tokio::process::Command::new(&process.command);
        cmd.args(&process.args);
        for (k, v) in &process.env {
            cmd.env(k, v);
        }

        match cmd.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                {
                    let mut s = statuses.write().await;
                    if let Some(entry) = s.get_mut(idx) {
                        entry.pid = pid;
                        entry.started_at = Some(started_at);
                        entry.restarts = restart_count;
                        entry.healthy = false;
                    }
                }
                tracing::info!("[supervisor] '{}' started (pid={:?})", process.name, pid);

                // Schedule health check after 10s
                if let Some(url) = process.health_url.clone() {
                    let name = process.name.clone();
                    let statuses2 = statuses.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                        let healthy = Supervisor::is_healthy(&url).await;
                        if healthy {
                            tracing::info!("[supervisor] '{}' health check OK at {}", name, url);
                        } else {
                            tracing::warn!(
                                "[supervisor] '{}' health check FAILED at {}",
                                name,
                                url
                            );
                        }
                        let mut s = statuses2.write().await;
                        if let Some(entry) = s.get_mut(idx) {
                            entry.healthy = healthy;
                        }
                    });
                }

                match child.wait().await {
                    Ok(status) => tracing::warn!(
                        "[supervisor] '{}' exited with status {}",
                        process.name,
                        status
                    ),
                    Err(e) => {
                        tracing::error!("[supervisor] '{}' wait() error: {}", process.name, e)
                    }
                }
            }
            Err(e) => {
                tracing::error!("[supervisor] failed to spawn '{}': {}", process.name, e);
            }
        }

        {
            let mut s = statuses.write().await;
            if let Some(entry) = s.get_mut(idx) {
                entry.pid = None;
                entry.healthy = false;
            }
        }

        restart_count += 1;
        tracing::info!(
            "[supervisor] '{}' will restart in {}ms (attempt {})",
            process.name,
            process.restart_delay_ms,
            restart_count
        );
        tokio::time::sleep(std::time::Duration::from_millis(process.restart_delay_ms)).await;
    }
}
