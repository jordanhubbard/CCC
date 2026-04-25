/// acc-server/src/blob_expiry.rs — Periodic blob expiry sweeper.
///
/// Spawns a background task that wakes every [`SWEEP_INTERVAL_SECS`] seconds,
/// finds every entry in [`AppState::blob_store`] whose `expires_at` timestamp
/// has passed, evicts those entries from the in-memory store, and removes their
/// on-disk directory trees under [`AppState::blobs_path`].
///
/// Design notes
/// ─────────────
/// * The write-lock on `blob_store` is held **only** for the in-memory eviction
///   step.  All filesystem work happens after the lock is released so that
///   upload / download handlers are never blocked by slow I/O.
/// * A single `chrono::Utc::now()` snapshot is taken once per sweep and reused
///   for every entry, giving a consistent expiry boundary for the whole pass.
/// * Filesystem errors during removal are logged as warnings and do not abort
///   the sweep or panic the task — the metadata entry has already been evicted,
///   so the blob is unreachable through the API regardless.
use std::sync::Arc;

use crate::AppState;

/// How often the sweeper wakes to check for expired blobs (seconds).
const SWEEP_INTERVAL_SECS: u64 = 60;

/// Spawn the blob-expiry sweeper as a detached Tokio task.
///
/// Call this once from `main` after `app_state` has been fully constructed,
/// alongside the other background-task spawns.
pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(run(state));
}

/// Core sweep loop — runs forever, sleeping [`SWEEP_INTERVAL_SECS`] between
/// passes.
async fn run(state: Arc<AppState>) {
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(SWEEP_INTERVAL_SECS));

    loop {
        interval.tick().await;
        sweep(&state).await;
    }
}

/// Single expiry pass: evict expired entries from memory then remove their
/// files from disk.
async fn sweep(state: &Arc<AppState>) {
    let now = chrono::Utc::now();

    // ── Phase 1: collect expired IDs and evict from the in-memory store ──────
    //
    // The write-lock scope ends before any async I/O so that concurrent
    // handlers are not stalled during filesystem operations.
    let expired_ids: Vec<String> = {
        let mut store = state.blob_store.write().await;
        let expired: Vec<String> = store
            .values()
            .filter(|meta| {
                meta.expires_at
                    .as_deref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|exp| exp <= now)
                    .unwrap_or(false)
            })
            .map(|meta| meta.id.clone())
            .collect();

        for id in &expired {
            store.remove(id);
        }
        expired
    };

    if expired_ids.is_empty() {
        return;
    }

    tracing::info!(
        "blob_expiry: evicting {} expired blob(s)",
        expired_ids.len()
    );

    // ── Phase 2: remove on-disk data (lock already released) ─────────────────
    for id in &expired_ids {
        let blob_dir = std::path::Path::new(&state.blobs_path).join(id);
        if let Err(e) = tokio::fs::remove_dir_all(&blob_dir).await {
            // NotFound is benign — the directory may never have been written
            // (e.g. an upload that only registered metadata but wrote no data).
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    "blob_expiry: failed to remove data for blob {}: {}",
                    id,
                    e
                );
            }
        } else {
            tracing::debug!("blob_expiry: removed data directory for blob {}", id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use chrono::Duration;
    use tokio::sync::RwLock;

    use crate::bus_types::{BlobMeta, MediaType};

    // Build a minimal AppState populated with the provided blobs.
    async fn make_state(blobs: Vec<BlobMeta>, blobs_path: &str) -> Arc<crate::AppState> {
        use crate::{brain, db};

        let auth_conn = db::open_auth(":memory:").expect("auth db");
        let initial_hashes: HashSet<String> =
            db::auth_all_token_hashes(&auth_conn).into_iter().collect();
        let auth_db = Arc::new(tokio::sync::Mutex::new(auth_conn));
        let fleet_db = db::open_fleet(":memory:").expect("fleet db");
        let fleet_db = Arc::new(tokio::sync::Mutex::new(fleet_db));

        let store: HashMap<String, BlobMeta> =
            blobs.into_iter().map(|m| (m.id.clone(), m)).collect();

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        Arc::new(crate::AppState {
            auth_tokens: HashSet::new(),
            user_token_hashes: std::sync::RwLock::new(initial_hashes),
            auth_db,
            fleet_db,
            queue_path: dir.join("queue.json").to_string_lossy().into_owned(),
            agents_path: dir.join("agents.json").to_string_lossy().into_owned(),
            secrets_path: dir.join("secrets.json").to_string_lossy().into_owned(),
            bus_log_path: dir.join("bus.jsonl").to_string_lossy().into_owned(),
            projects_path: dir.join("projects.json").to_string_lossy().into_owned(),
            queue: RwLock::new(crate::state::QueueData::default()),
            agents: RwLock::new(serde_json::Value::Object(serde_json::Map::new())),
            secrets: RwLock::new(serde_json::Map::new()),
            projects: RwLock::new(Vec::new()),
            brain: Arc::new(brain::BrainQueue::new()),
            bus_tx: tokio::sync::broadcast::channel(256).0,
            bus_seq: std::sync::atomic::AtomicU64::new(0),
            start_time: std::time::SystemTime::now(),
            fs_root: dir.join("fs").to_string_lossy().into_owned(),
            supervisor: None,
            soul_store: RwLock::new(HashMap::new()),
            blob_store: RwLock::new(store),
            blobs_path: blobs_path.to_string(),
            dlq_path: dir.join("bus-dlq.jsonl").to_string_lossy().into_owned(),
            max_blob_bytes: 100 * 1024 * 1024, // 100 MiB default for unit tests
        })
    }

    fn make_blob(id: &str, expires_at: Option<chrono::DateTime<chrono::Utc>>) -> BlobMeta {
        BlobMeta {
            id: id.to_string(),
            mime_type: MediaType::TextPlain,
            size_bytes: 4,
            uploaded_by: "test".to_string(),
            uploaded_at: chrono::Utc::now().to_rfc3339(),
            expires_at: expires_at.map(|t| t.to_rfc3339()),
            allowed_agents: vec![],
            total_chunks: 1,
            chunks_received: 1,
            complete: true,
            chunks_seen: HashSet::from([0]),
        }
    }

    // ── sweep evicts expired entries and leaves live ones intact ─────────────

    #[tokio::test]
    async fn test_sweep_removes_expired_keeps_live() {
        let tmp = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();

        let expired = make_blob("exp-1", Some(now - Duration::seconds(10)));
        let live = make_blob("live-1", Some(now + Duration::seconds(3600)));
        let no_ttl = make_blob("no-ttl", None);

        let state = make_state(
            vec![expired, live, no_ttl],
            tmp.path().to_str().unwrap(),
        )
        .await;

        super::sweep(&state).await;

        let store = state.blob_store.read().await;
        assert!(
            !store.contains_key("exp-1"),
            "expired blob must be evicted from store"
        );
        assert!(
            store.contains_key("live-1"),
            "live blob must remain in store"
        );
        assert!(
            store.contains_key("no-ttl"),
            "blob with no TTL must remain in store"
        );
    }

    // ── sweep removes the on-disk directory for an expired blob ──────────────

    #[tokio::test]
    async fn test_sweep_removes_blob_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let blobs_path = tmp.path().to_str().unwrap();
        let now = chrono::Utc::now();

        let expired = make_blob("exp-dir", Some(now - Duration::seconds(1)));

        // Create the on-disk directory that the upload handler would create.
        let blob_dir = tmp.path().join("exp-dir");
        tokio::fs::create_dir_all(&blob_dir).await.unwrap();
        tokio::fs::write(blob_dir.join("data"), b"test").await.unwrap();

        let state = make_state(vec![expired], blobs_path).await;

        super::sweep(&state).await;

        assert!(
            !blob_dir.exists(),
            "blob data directory must be deleted from disk"
        );
    }

    // ── sweep is a no-op when the store is empty ──────────────────────────────

    #[tokio::test]
    async fn test_sweep_empty_store_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let state = make_state(vec![], tmp.path().to_str().unwrap()).await;
        // Must not panic.
        super::sweep(&state).await;
        assert!(state.blob_store.read().await.is_empty());
    }

    // ── sweep tolerates a missing on-disk directory without panicking ─────────

    #[tokio::test]
    async fn test_sweep_missing_directory_does_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();

        // Expired blob whose data directory was never created.
        let expired = make_blob("no-dir", Some(now - Duration::seconds(5)));
        let state = make_state(vec![expired], tmp.path().to_str().unwrap()).await;

        // Must complete without panicking even though the directory is absent.
        super::sweep(&state).await;

        assert!(!state.blob_store.read().await.contains_key("no-dir"));
    }

    // ── blob exactly at the expiry boundary is evicted ────────────────────────
    //
    // The check is `exp <= now`, so a blob whose `expires_at` equals the sweep
    // instant is considered expired.

    #[tokio::test]
    async fn test_sweep_boundary_expired_is_evicted() {
        let tmp = tempfile::tempdir().unwrap();
        // Subtract 1 ns to guarantee the timestamp is strictly in the past by
        // the time sweep() calls Utc::now() internally.
        let just_expired = chrono::Utc::now() - Duration::nanoseconds(1);
        let blob = make_blob("boundary", Some(just_expired));
        let state = make_state(vec![blob], tmp.path().to_str().unwrap()).await;

        super::sweep(&state).await;

        assert!(!state.blob_store.read().await.contains_key("boundary"));
    }
}
