//! Worker trait — self-describing supervised agent workers.
//!
//! Each worker declares its name, capabilities, and whether it should run
//! based on environment configuration. The supervisor uses this metadata
//! for capability advertisement; actual process lifecycle stays in supervise.rs.
//!
//! # Adding a new worker
//! 1. Add a struct implementing `Worker`.
//! 2. Add it to `ALL_WORKERS`.
//! 3. Add a matching `ChildSpec` entry in `supervise.rs`.

/// A self-describing supervised agent worker (metadata only — not the process itself).
#[allow(dead_code)]
pub trait Worker: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Vec<String> { vec![] }
    fn enabled(&self) -> bool { true }
}

// ── Concrete worker types ─────────────────────────────────────────────────────

pub struct BusWorker;
pub struct QueueWorker;
pub struct TasksWorker;
pub struct HermesWorker;
pub struct GatewayWorker;
pub struct GatewayOffteraWorker;
pub struct ProxyWorker;

impl Worker for BusWorker {
    fn name(&self) -> &'static str { "bus" }
    fn capabilities(&self) -> Vec<String> {
        vec!["bus".into(), "event-relay".into()]
    }
}

impl Worker for QueueWorker {
    fn name(&self) -> &'static str { "queue" }
    fn capabilities(&self) -> Vec<String> {
        vec!["queue".into(), "task-dispatch".into()]
    }
}

impl Worker for TasksWorker {
    fn name(&self) -> &'static str { "tasks" }
    fn capabilities(&self) -> Vec<String> {
        vec!["tasks".into(), "bash".into(), "git".into()]
    }
}

impl Worker for HermesWorker {
    fn name(&self) -> &'static str { "hermes" }
    fn capabilities(&self) -> Vec<String> {
        vec!["hermes".into(), "llm".into()]
    }
}

impl Worker for GatewayWorker {
    fn name(&self) -> &'static str { "gateway" }
    fn capabilities(&self) -> Vec<String> {
        vec!["slack".into(), "telegram".into(), "chat".into()]
    }
    fn enabled(&self) -> bool {
        std::env::var("SLACK_APP_TOKEN").map(|t| t.starts_with("xapp-")).unwrap_or(false)
            || std::env::var("TELEGRAM_BOT_TOKEN").map(|t| !t.is_empty()).unwrap_or(false)
    }
}

impl Worker for GatewayOffteraWorker {
    fn name(&self) -> &'static str { "gateway-offtera" }
    fn capabilities(&self) -> Vec<String> {
        vec!["slack-offtera".into(), "chat-offtera".into()]
    }
    fn enabled(&self) -> bool {
        // Accept both env-var spellings during the typo-fix migration.
        std::env::var("SLACK_APP_TOKEN_OFFTERA")
            .or_else(|_| std::env::var("SLACK_APP_TOKEN_OFTERRA"))
            .map(|t| t.starts_with("xapp-"))
            .unwrap_or(false)
    }
}

impl Worker for ProxyWorker {
    fn name(&self) -> &'static str { "proxy" }
    fn capabilities(&self) -> Vec<String> {
        vec!["nvidia-proxy".into(), "llm-proxy".into()]
    }
    fn enabled(&self) -> bool {
        std::env::var("NVIDIA_API_BASE").is_ok()
    }
}

/// All known workers in declaration order.
pub fn all_workers() -> Vec<Box<dyn Worker>> {
    vec![
        Box::new(BusWorker),
        Box::new(QueueWorker),
        Box::new(TasksWorker),
        Box::new(HermesWorker),
        Box::new(GatewayWorker),
        Box::new(GatewayOffteraWorker),
        Box::new(ProxyWorker),
    ]
}

/// Capabilities contributed by all currently-enabled workers (de-duplicated).
pub fn enabled_capabilities() -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    all_workers()
        .iter()
        .filter(|w| w.enabled())
        .flat_map(|w| w.capabilities())
        .filter(|c| seen.insert(c.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_workers_have_unique_names() {
        let workers = all_workers();
        let mut names = std::collections::HashSet::new();
        for w in &workers {
            assert!(names.insert(w.name()), "duplicate worker name: {}", w.name());
        }
    }

    #[test]
    fn enabled_capabilities_deduplicates() {
        let caps = enabled_capabilities();
        let unique: std::collections::HashSet<_> = caps.iter().collect();
        assert_eq!(caps.len(), unique.len(), "capabilities must be deduplicated");
    }
}
