//! clawfs-sync — watches local workspace dirs and syncs to/from MinIO S3
//! wq-AGENTFS-002

use aws_credential_types::Credentials;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::config::Region;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

// ── Backend abstraction ───────────────────────────────────────────────────────

#[derive(Clone)]
enum Backend {
    S3 {
        client: Arc<aws_sdk_s3::Client>,
        bucket: String,
    },
    Rcc {
        http: Arc<reqwest::Client>,
        endpoint: String,
    },
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Map a watch directory to its S3 key prefix.
/// ~/.../shared   → "shared/"
/// ~/.../memory   → "{agent}/memory/"
/// ~/.../other    → "{agent}/other/"
fn s3_prefix_for_dir(dir: &Path, agent_name: &str) -> String {
    let basename = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("files");
    if basename == "shared" {
        "shared/".to_string()
    } else {
        format!("{}/{}/", agent_name, basename)
    }
}

/// Given a local file path and the (dir → prefix) mapping, return the S3 key.
/// Returns None for files in subdirectories.
fn s3_key_for_file(file: &Path, dir_prefixes: &[(PathBuf, String)]) -> Option<String> {
    for (dir, prefix) in dir_prefixes {
        if let Ok(rel) = file.strip_prefix(dir) {
            let rel_str = rel.to_str()?;
            if rel_str.contains('/') {
                return None; // skip nested paths
            }
            return Some(format!("{}{}", prefix, rel_str));
        }
    }
    None
}

// ── Upload ────────────────────────────────────────────────────────────────────

async fn upload_file(path: &Path, key: &str, backend: &Backend, agent_name: &str) {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            error!("read {} failed: {}", path.display(), e);
            return;
        }
    };
    let len = bytes.len();
    match backend {
        Backend::S3 { client, bucket } => {
            let stream = ByteStream::from(bytes);
            match client
                .put_object()
                .bucket(bucket)
                .key(key)
                .body(stream)
                .send()
                .await
            {
                Ok(_) => info!("uploaded {} → s3://{}/{} ({} B)", path.display(), bucket, key, len),
                Err(e) => error!("upload {} failed: {}", key, e),
            }
        }
        Backend::Rcc { http, endpoint } => match String::from_utf8(bytes) {
            Ok(content) => {
                let body = serde_json::json!({
                    "path": key,
                    "content": content,
                    "agent": agent_name,
                });
                match http
                    .post(format!("{}/write", endpoint))
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => {
                        info!("uploaded {} → rcc:{} ({} B)", path.display(), key, len)
                    }
                    Ok(r) => error!("rcc upload {} HTTP {}", key, r.status()),
                    Err(e) => error!("rcc upload {} failed: {}", key, e),
                }
            }
            Err(_) => warn!("skipping binary file {} (CCC API is text-only)", path.display()),
        },
    }
}

// ── Sync pull ─────────────────────────────────────────────────────────────────

async fn sync_pull(backend: &Backend, prefix: &str, local_dir: &Path, agent_name: &str) {
    match backend {
        Backend::S3 { client, bucket } => {
            sync_pull_s3(client, bucket, prefix, local_dir).await
        }
        Backend::Rcc { http, endpoint } => {
            sync_pull_rcc(http, endpoint, prefix, local_dir, agent_name).await
        }
    }
}

async fn sync_pull_s3(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
    local_dir: &Path,
) {
    let output = match client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(prefix)
        .send()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            error!("list_objects prefix={} failed: {}", prefix, e);
            return;
        }
    };

    for obj in output.contents() {
        let key = match obj.key() {
            Some(k) => k,
            None => continue,
        };
        let fname = key.trim_start_matches(prefix);
        if fname.is_empty() || fname.contains('/') {
            continue;
        }
        let local_path = local_dir.join(fname);

        let s3_ms = obj
            .last_modified()
            .and_then(|dt| dt.to_millis().ok())
            .unwrap_or(0);

        let should_dl = if local_path.exists() {
            match local_path.metadata().and_then(|m| m.modified()) {
                Ok(mtime) => {
                    let local_ms = mtime
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    s3_ms > local_ms
                }
                Err(_) => true,
            }
        } else {
            true
        };

        if !should_dl {
            continue;
        }

        match client.get_object().bucket(bucket).key(key).send().await {
            Ok(resp) => match resp.body.collect().await {
                Ok(data) => {
                    let bytes = data.into_bytes();
                    match tokio::fs::write(&local_path, &bytes).await {
                        Ok(_) => info!(
                            "downloaded s3://{}/{} → {} ({} B)",
                            bucket,
                            key,
                            local_path.display(),
                            bytes.len()
                        ),
                        Err(e) => error!("write {} failed: {}", local_path.display(), e),
                    }
                }
                Err(e) => error!("get_object body {} failed: {}", key, e),
            },
            Err(e) => error!("get_object {} failed: {}", key, e),
        }
    }
}

async fn sync_pull_rcc(
    http: &reqwest::Client,
    endpoint: &str,
    prefix: &str,
    local_dir: &Path,
    agent_name: &str,
) {
    let list: serde_json::Value = match http
        .get(format!("{}/list", endpoint))
        .query(&[("prefix", prefix), ("agent", agent_name)])
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(v) => v,
            Err(e) => {
                error!("rcc list parse prefix={} failed: {}", prefix, e);
                return;
            }
        },
        Err(e) => {
            error!("rcc list prefix={} failed: {}", prefix, e);
            return;
        }
    };

    let objects = match list["objects"].as_array() {
        Some(arr) => arr.clone(),
        None => return,
    };

    for obj in objects {
        let key = match obj["key"].as_str() {
            Some(k) => k,
            None => continue,
        };
        let fname = key.trim_start_matches(prefix);
        if fname.is_empty() || fname.contains('/') {
            continue;
        }
        let local_path = local_dir.join(fname);

        match http
            .get(format!("{}/read", endpoint))
            .query(&[("path", key), ("agent", agent_name)])
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => match r.bytes().await {
                Ok(bytes) => match tokio::fs::write(&local_path, &bytes).await {
                    Ok(_) => info!("rcc downloaded {} → {} ({} B)", key, local_path.display(), bytes.len()),
                    Err(e) => error!("write {} failed: {}", local_path.display(), e),
                },
                Err(e) => error!("rcc read body {} failed: {}", key, e),
            },
            Ok(r) => error!("rcc read {} HTTP {}", key, r.status()),
            Err(e) => error!("rcc read {} failed: {}", key, e),
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("clawfs_sync=info")),
        )
        .init();

    // ── Env config ────────────────────────────────────────────────────────────
    let minio_endpoint = std::env::var("MINIO_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".to_string());
    let bucket = std::env::var("MINIO_BUCKET")
        .unwrap_or_else(|_| "agents".to_string());
    let agent_name = std::env::var("AGENT_NAME")
        .unwrap_or_else(|_| "rocky".to_string());
    let rcc_endpoint = std::env::var("RCC_AGENTFS_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty());

    let watch_dirs_raw = std::env::var("WATCH_DIRS").unwrap_or_else(|_| {
        "~/.openclaw/workspace/memory,~/.openclaw/workspace/shared".to_string()
    });
    let watch_dirs: Vec<PathBuf> = watch_dirs_raw
        .split(',')
        .map(|s| expand_tilde(s.trim()))
        .collect();

    // ── Backend ───────────────────────────────────────────────────────────────
    let backend: Arc<Backend> = if let Some(ref url) = rcc_endpoint {
        info!("backend: CCC API at {}", url);
        Arc::new(Backend::Rcc {
            http: Arc::new(reqwest::Client::new()),
            endpoint: url.clone(),
        })
    } else {
        let access_key = std::env::var("MINIO_ACCESS_KEY").unwrap_or_default();
        let secret_key = std::env::var("MINIO_SECRET_KEY").unwrap_or_default();
        let creds = Credentials::new(&access_key, &secret_key, None, None, "env");
        let s3_cfg = aws_sdk_s3::Config::builder()
            .credentials_provider(creds)
            .region(Region::new("us-east-1"))
            .endpoint_url(&minio_endpoint)
            .force_path_style(true)
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .build();
        info!("backend: S3/MinIO at {} bucket={}", minio_endpoint, bucket);
        Arc::new(Backend::S3 {
            client: Arc::new(aws_sdk_s3::Client::from_conf(s3_cfg)),
            bucket: bucket.clone(),
        })
    };

    // ── Ensure watch dirs exist ───────────────────────────────────────────────
    for dir in &watch_dirs {
        tokio::fs::create_dir_all(dir).await?;
        info!("watch dir: {}", dir.display());
    }

    // Compute (dir, s3_prefix) pairs
    let dir_prefixes: Arc<Vec<(PathBuf, String)>> = Arc::new(
        watch_dirs
            .iter()
            .map(|d| (d.clone(), s3_prefix_for_dir(d, &agent_name)))
            .collect(),
    );

    // ── Step 1: initial pull of agent memory ──────────────────────────────────
    let memory_dir = expand_tilde("~/.openclaw/workspace/memory");
    let memory_prefix = format!("{}/memory/", agent_name);
    info!("initial sync pull: {} → {}", memory_prefix, memory_dir.display());
    sync_pull(&backend, &memory_prefix, &memory_dir, &agent_name).await;

    // ── Notify watcher ────────────────────────────────────────────────────────
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Result<Event>>();
    let tx_watcher = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            let _ = tx_watcher.send(res);
        },
        notify::Config::default(),
    )?;
    for dir in &watch_dirs {
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
    }
    info!("watching {} dir(s) for changes", watch_dirs.len());

    // pending debounce map: path → time of last write event
    let pending: Arc<Mutex<HashMap<PathBuf, Instant>>> = Arc::new(Mutex::new(HashMap::new()));

    // ── Task 1: accumulate file-change events into pending map ────────────────
    let pending_rx = pending.clone();
    let watcher_task = tokio::spawn(async move {
        while let Some(res) = rx.recv().await {
            match res {
                Ok(event) => {
                    let is_write = matches!(
                        event.kind,
                        EventKind::Create(_) | EventKind::Modify(_)
                    );
                    if is_write {
                        let mut map = pending_rx.lock().await;
                        for path in event.paths {
                            if path.is_file() {
                                map.insert(path, Instant::now());
                            }
                        }
                    }
                }
                Err(e) => warn!("watch error: {}", e),
            }
        }
    });

    // ── Task 2: debounce uploader (5s idle → upload) ──────────────────────────
    let pending_up = pending.clone();
    let backend_up = backend.clone();
    let dir_prefixes_up = dir_prefixes.clone();
    let agent_name_up = agent_name.clone();
    let upload_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let ready: Vec<PathBuf> = {
                let mut map = pending_up.lock().await;
                let now = Instant::now();
                let mut ready = Vec::new();
                map.retain(|path, last| {
                    if now.duration_since(*last) >= Duration::from_secs(5) {
                        ready.push(path.clone());
                        false
                    } else {
                        true
                    }
                });
                ready
            };
            for path in ready {
                if let Some(key) = s3_key_for_file(&path, &dir_prefixes_up) {
                    upload_file(&path, &key, &backend_up, &agent_name_up).await;
                }
            }
        }
    });

    // ── Task 3: full sync pull every 60s ──────────────────────────────────────
    let backend_sync = backend.clone();
    let dir_prefixes_sync = dir_prefixes.clone();
    let agent_name_sync = agent_name.clone();
    let sync_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.tick().await; // consume first tick (initial pull already done)
        loop {
            tick.tick().await;
            info!("periodic full sync pull");
            for (dir, prefix) in dir_prefixes_sync.iter() {
                sync_pull(&backend_sync, prefix, dir, &agent_name_sync).await;
            }
        }
    });

    info!(
        "clawfs-sync running (agent={}, dirs={})",
        agent_name,
        watch_dirs.len()
    );

    // Keep watcher alive and wait for shutdown
    let _watcher = watcher;
    tokio::select! {
        _ = watcher_task => warn!("watcher task exited"),
        _ = upload_task  => warn!("upload task exited"),
        _ = sync_task    => warn!("sync task exited"),
        _ = tokio::signal::ctrl_c() => info!("ctrl-c, shutting down"),
    }

    Ok(())
}
