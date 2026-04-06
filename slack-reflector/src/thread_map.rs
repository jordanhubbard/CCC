use dashmap::DashMap;
use std::sync::Arc;

/// Maps thread timestamps between workspaces so threaded replies stay threaded.
///
/// Key: (source_workspace_idx, source_thread_ts)
/// Value: mirrored_thread_ts on the peer workspace
#[derive(Clone)]
pub struct ThreadMap {
    inner: Arc<DashMap<(usize, String), String>>,
}

impl ThreadMap {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    /// Record a thread mapping: when we mirror a message that starts or is part of
    /// a thread, we store the source_ts -> mirrored_ts relationship.
    pub fn insert(&self, source_ws: usize, source_ts: &str, mirrored_ts: &str) {
        self.inner.insert(
            (source_ws, source_ts.to_string()),
            mirrored_ts.to_string(),
        );
    }

    /// Look up the mirrored thread_ts for a source thread_ts
    pub fn get(&self, source_ws: usize, source_ts: &str) -> Option<String> {
        self.inner
            .get(&(source_ws, source_ts.to_string()))
            .map(|v| v.clone())
    }

    /// Prune old entries (call periodically). Keeps map from growing unbounded.
    /// Simple strategy: if map > max_size, clear it entirely (threads older than
    /// the cache window won't get matched, which is acceptable).
    pub fn prune_if_needed(&self, max_size: usize) {
        if self.inner.len() > max_size {
            self.inner.clear();
            tracing::info!("thread map pruned (exceeded {} entries)", max_size);
        }
    }
}
