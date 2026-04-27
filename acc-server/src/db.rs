/// SQLite database layer for acc-server.
///
/// Enabled by setting `ACC_DB_PATH` to a `.db` file path.
/// On first start, data is migrated from existing JSON files automatically.
/// Once migrated, JSON files become stale and are no longer written.
///
/// Schema version is tracked in the `schema_version` table.
/// Migrations are additive — run in order, each runs exactly once.
use rusqlite::{Connection, Result, params};
use serde_json::Value;
use std::path::Path;

const CURRENT_VERSION: i64 = 5;

/// Open a database connection, create schema if needed, run any pending migrations.
pub fn open(path: &str) -> Result<Connection> {
    // Create parent directory if needed
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let conn = Connection::open(path)?;

    // WAL mode for concurrent readers
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    init_schema(&conn)?;
    run_migrations(&conn)?;

    tracing::info!("SQLite database opened: {}", path);
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS queue_items (
            id TEXT PRIMARY KEY,
            status TEXT NOT NULL DEFAULT 'pending',
            priority INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            data TEXT NOT NULL  -- full JSON blob
        );

        CREATE INDEX IF NOT EXISTS idx_queue_status ON queue_items(status);
        CREATE INDEX IF NOT EXISTS idx_queue_priority ON queue_items(priority DESC, created_at ASC);

        CREATE TABLE IF NOT EXISTS queue_completed (
            id TEXT PRIMARY KEY,
            completed_at TEXT NOT NULL,
            data TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS agents (
            name TEXT PRIMARY KEY,
            host TEXT,
            status TEXT NOT NULL DEFAULT 'offline',
            last_heartbeat TEXT,
            data TEXT NOT NULL  -- full JSON blob with capabilities etc.
        );

        CREATE TABLE IF NOT EXISTS heartbeats (
            agent TEXT NOT NULL,
            ts TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'online',
            host TEXT,
            PRIMARY KEY (agent, ts)
        );

        CREATE INDEX IF NOT EXISTS idx_heartbeats_agent ON heartbeats(agent, ts DESC);

        CREATE TABLE IF NOT EXISTS secrets (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        );

        CREATE TABLE IF NOT EXISTS fleet_tasks (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'open',
            priority INTEGER NOT NULL DEFAULT 2,
            claimed_by TEXT,
            claimed_at TEXT,
            claim_expires_at TEXT,
            completed_at TEXT,
            completed_by TEXT,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            metadata      TEXT NOT NULL DEFAULT '{}',
            task_type     TEXT NOT NULL DEFAULT 'work',
            review_of     TEXT,
            phase         TEXT,
            blocked_by    TEXT NOT NULL DEFAULT '[]',
            review_result TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_fleet_tasks_status   ON fleet_tasks(status, priority, created_at);
        CREATE INDEX IF NOT EXISTS idx_fleet_tasks_project  ON fleet_tasks(project_id);
        CREATE INDEX IF NOT EXISTS idx_fleet_tasks_agent    ON fleet_tasks(claimed_by, status);
        CREATE INDEX IF NOT EXISTS idx_fleet_tasks_expires  ON fleet_tasks(claim_expires_at);

        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            name TEXT,
            full_name TEXT,
            data TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            email TEXT,
            token_hash TEXT,
            confirmed_at TEXT,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            data TEXT NOT NULL DEFAULT '{}'
        );

        CREATE TABLE IF NOT EXISTS lessons (
            id TEXT PRIMARY KEY,
            domain TEXT NOT NULL,
            symptom TEXT NOT NULL,
            fix TEXT NOT NULL,
            agent TEXT,
            tags TEXT NOT NULL DEFAULT '[]',  -- JSON array
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            data TEXT NOT NULL DEFAULT '{}'
        );

        CREATE INDEX IF NOT EXISTS idx_lessons_domain ON lessons(domain);

        CREATE TABLE IF NOT EXISTS bus_messages (
            id TEXT PRIMARY KEY,
            seq INTEGER NOT NULL,
            ts TEXT NOT NULL,
            msg_type TEXT NOT NULL DEFAULT 'text',
            from_agent TEXT,
            to_agent TEXT,
            subject TEXT,
            topic TEXT,
            body TEXT,
            thread_id TEXT,
            target TEXT,
            emoji TEXT,
            action TEXT,
            data TEXT NOT NULL DEFAULT '{}'  -- full JSON blob for forward-compat
        );

        CREATE INDEX IF NOT EXISTS idx_bus_subject ON bus_messages(subject, ts);
        CREATE INDEX IF NOT EXISTS idx_bus_to ON bus_messages(to_agent, ts);
        CREATE INDEX IF NOT EXISTS idx_bus_seq ON bus_messages(seq DESC);
        CREATE INDEX IF NOT EXISTS idx_bus_thread ON bus_messages(thread_id);

        CREATE TABLE IF NOT EXISTS requests (
            id TEXT PRIMARY KEY,
            body TEXT NOT NULL DEFAULT '',
            channel TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'pending',
            claimed_by TEXT,
            claimed_at TEXT,
            completed_at TEXT,
            completed_by TEXT,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            metadata TEXT NOT NULL DEFAULT '{}'
        );
        CREATE INDEX IF NOT EXISTS idx_requests_status  ON requests(status, created_at);
        CREATE INDEX IF NOT EXISTS idx_requests_claimed ON requests(claimed_by, status);
    ")?;
    Ok(())
}

fn get_schema_version(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT MAX(version) FROM schema_version",
        [],
        |row| row.get::<_, Option<i64>>(0),
    )
    .unwrap_or(None)
    .unwrap_or(0)
}

fn set_schema_version(conn: &Connection, version: i64) -> Result<()> {
    conn.execute("DELETE FROM schema_version", [])?;
    conn.execute("INSERT INTO schema_version (version) VALUES (?1)", params![version])?;
    Ok(())
}

fn run_migrations(conn: &Connection) -> Result<()> {
    let version = get_schema_version(conn);
    if version >= CURRENT_VERSION {
        return Ok(());
    }

    // Migration v1: initial schema is already created by init_schema.
    tracing::info!("Database schema at version {} (current: {})", version, CURRENT_VERSION);

    if version < 2 {
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS fleet_tasks (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'open',
                priority INTEGER NOT NULL DEFAULT 2,
                claimed_by TEXT,
                claimed_at TEXT,
                claim_expires_at TEXT,
                completed_at TEXT,
                completed_by TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                metadata TEXT NOT NULL DEFAULT '{}'
            );
            CREATE INDEX IF NOT EXISTS idx_fleet_tasks_status  ON fleet_tasks(status, priority, created_at);
            CREATE INDEX IF NOT EXISTS idx_fleet_tasks_project ON fleet_tasks(project_id);
            CREATE INDEX IF NOT EXISTS idx_fleet_tasks_agent   ON fleet_tasks(claimed_by, status);
            CREATE INDEX IF NOT EXISTS idx_fleet_tasks_expires ON fleet_tasks(claim_expires_at);
        ")?;
        set_schema_version(conn, 2)?;
    }

    if version < 3 {
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS requests (
                id TEXT PRIMARY KEY,
                body TEXT NOT NULL DEFAULT '',
                channel TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'pending',
                claimed_by TEXT,
                claimed_at TEXT,
                completed_at TEXT,
                completed_by TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                metadata TEXT NOT NULL DEFAULT '{}'
            );
            CREATE INDEX IF NOT EXISTS idx_requests_status  ON requests(status, created_at);
            CREATE INDEX IF NOT EXISTS idx_requests_claimed ON requests(claimed_by, status);
        ")?;
        set_schema_version(conn, 3)?;
    }

    if version < 4 {
        let col_exists: bool = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('fleet_tasks') WHERE name='task_type'",
            [], |r| r.get::<_, i64>(0),
        ).unwrap_or(0) > 0;
        if !col_exists {
            conn.execute_batch("
                ALTER TABLE fleet_tasks ADD COLUMN task_type TEXT NOT NULL DEFAULT 'work';
                ALTER TABLE fleet_tasks ADD COLUMN review_of TEXT;
                ALTER TABLE fleet_tasks ADD COLUMN phase TEXT;
                ALTER TABLE fleet_tasks ADD COLUMN blocked_by TEXT NOT NULL DEFAULT '[]';
                ALTER TABLE fleet_tasks ADD COLUMN review_result TEXT;
            ")?;
        }
        // Always ensure the phase index exists (idempotent)
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_fleet_tasks_phase ON fleet_tasks(project_id, phase, status);"
        )?;
        set_schema_version(conn, 4)?;
    }

    if version < 5 {
        // v4 migration had a bug: if task_type already existed it skipped blocked_by/review_result.
        // v5 adds them individually with existence checks so it's always safe to run.
        for (col, def) in &[
            ("blocked_by",    "TEXT NOT NULL DEFAULT '[]'"),
            ("review_result", "TEXT"),
        ] {
            let exists: bool = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('fleet_tasks') WHERE name=?1",
                rusqlite::params![col],
                |r| r.get::<_, i64>(0),
            ).unwrap_or(0) > 0;
            if !exists {
                conn.execute_batch(&format!(
                    "ALTER TABLE fleet_tasks ADD COLUMN {} {};", col, def
                ))?;
            }
        }
        set_schema_version(conn, 5)?;
    }

    set_schema_version(conn, CURRENT_VERSION)?;
    tracing::info!("Database schema migrated to version {}", CURRENT_VERSION);
    Ok(())
}

/// Migrate existing JSON data files into the SQLite database.
/// Safe to call multiple times — uses INSERT OR IGNORE for idempotency.
/// Returns (items_migrated, agents_migrated, secrets_migrated, projects_migrated).
pub fn migrate_from_json(
    conn: &Connection,
    queue_path: &str,
    agents_path: &str,
    secrets_path: &str,
    projects_path: &str,
) -> (usize, usize, usize, usize) {
    let q = migrate_queue(conn, queue_path);
    let a = migrate_agents(conn, agents_path);
    let s = migrate_secrets(conn, secrets_path);
    let p = migrate_projects(conn, projects_path);
    tracing::info!(
        "JSON→SQLite migration: {} queue items, {} agents, {} secrets, {} projects",
        q, a, s, p
    );
    (q, a, s, p)
}

fn migrate_queue(conn: &Connection, path: &str) -> usize {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let data: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return 0,
    };

    let mut count = 0;
    let now = chrono::Utc::now().to_rfc3339();

    if let Some(items) = data.get("items").and_then(|v| v.as_array()) {
        for item in items {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
            let priority = item.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
            let created_at = item.get("created_at").and_then(|v| v.as_str()).unwrap_or(&now);
            let updated_at = item.get("updated_at").and_then(|v| v.as_str()).unwrap_or(&now);
            let blob = item.to_string();

            if id.is_empty() { continue; }
            let r = conn.execute(
                "INSERT OR IGNORE INTO queue_items (id, status, priority, created_at, updated_at, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, status, priority, created_at, updated_at, blob],
            );
            if r.is_ok() { count += 1; }
        }
    }

    if let Some(completed) = data.get("completed").and_then(|v| v.as_array()) {
        for item in completed {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let completed_at = item.get("completed_at").and_then(|v| v.as_str()).unwrap_or(&now);
            let blob = item.to_string();
            if id.is_empty() { continue; }
            let _ = conn.execute(
                "INSERT OR IGNORE INTO queue_completed (id, completed_at, data) VALUES (?1, ?2, ?3)",
                params![id, completed_at, blob],
            );
        }
    }

    count
}

fn migrate_agents(conn: &Connection, path: &str) -> usize {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let data: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return 0,
    };

    let mut count = 0;

    // Agents stored as either array or object keyed by name
    let entries: Vec<&Value> = if let Some(arr) = data.as_array() {
        arr.iter().collect()
    } else if let Some(obj) = data.as_object() {
        obj.values().collect()
    } else {
        return 0;
    };

    for agent in entries {
        let name = agent.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() { continue; }
        let host = agent.get("host").and_then(|v| v.as_str()).unwrap_or("");
        let status = agent.get("status").and_then(|v| v.as_str()).unwrap_or("offline");
        let blob = agent.to_string();

        let r = conn.execute(
            "INSERT OR IGNORE INTO agents (name, host, status, data) VALUES (?1, ?2, ?3, ?4)",
            params![name, host, status, blob],
        );
        if r.is_ok() { count += 1; }
    }

    count
}

fn migrate_secrets(conn: &Connection, path: &str) -> usize {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let data: serde_json::Map<String, Value> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return 0,
    };

    let mut count = 0;
    let now = chrono::Utc::now().to_rfc3339();

    for (key, value) in &data {
        let val_str = value.as_str().map(|s| s.to_string()).unwrap_or_else(|| value.to_string());
        let r = conn.execute(
            "INSERT OR IGNORE INTO secrets (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![key, val_str, now],
        );
        if r.is_ok() { count += 1; }
    }

    count
}

fn migrate_projects(conn: &Connection, path: &str) -> usize {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let data: Vec<Value> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return 0,
    };

    let mut count = 0;

    for project in &data {
        let id = project.get("id").and_then(|v| v.as_str())
            .or_else(|| project.get("full_name").and_then(|v| v.as_str()))
            .unwrap_or("");
        if id.is_empty() { continue; }
        let name = project.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let full_name = project.get("full_name").and_then(|v| v.as_str()).unwrap_or("");
        let blob = project.to_string();

        let r = conn.execute(
            "INSERT OR IGNORE INTO projects (id, name, full_name, data) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, full_name, blob],
        );
        if r.is_ok() { count += 1; }
    }

    count
}

// ── Load helpers (fleet_db → in-memory) ──────────────────────────────────────

pub fn db_load_agents(conn: &Connection) -> Value {
    let mut stmt = match conn.prepare("SELECT data FROM agents") {
        Ok(s) => s,
        Err(_) => return Value::Object(serde_json::Map::new()),
    };
    let mut map = serde_json::Map::new();
    let rows = stmt.query_map([], |row| row.get::<_, String>(0));
    if let Ok(rows) = rows {
        for row in rows.flatten() {
            if let Ok(v) = serde_json::from_str::<Value>(&row) {
                if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                    map.insert(name.to_string(), v);
                }
            }
        }
    }
    Value::Object(map)
}

pub fn db_load_queue_items(conn: &Connection) -> Vec<Value> {
    let mut items = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT data FROM queue_items ORDER BY created_at ASC") {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
            for row in rows.flatten() {
                if let Ok(v) = serde_json::from_str::<Value>(&row) {
                    items.push(v);
                }
            }
        }
    }
    items
}

pub fn db_load_queue_completed(conn: &Connection) -> Vec<Value> {
    let mut completed = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT data FROM queue_completed ORDER BY completed_at DESC LIMIT 500",
    ) {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
            for row in rows.flatten() {
                if let Ok(v) = serde_json::from_str::<Value>(&row) {
                    completed.push(v);
                }
            }
        }
    }
    completed
}

pub fn db_load_secrets(conn: &Connection) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM secrets") {
        if let Ok(rows) =
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        {
            for row in rows.flatten() {
                let (key, val) = row;
                let json_val = serde_json::from_str::<Value>(&val)
                    .unwrap_or_else(|_| Value::String(val));
                map.insert(key, json_val);
            }
        }
    }
    map
}

pub fn db_load_projects(conn: &Connection) -> Vec<Value> {
    let mut projects = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT data FROM projects") {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
            for row in rows.flatten() {
                if let Ok(v) = serde_json::from_str::<Value>(&row) {
                    projects.push(v);
                }
            }
        }
    }
    projects
}

// ── Persist helpers (in-memory → fleet_db) ───────────────────────────────────

pub fn db_upsert_agent(conn: &Connection, data: &Value) -> Result<()> {
    let name = data.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let host = data.get("host").and_then(|h| h.as_str()).unwrap_or("");
    let status = if data
        .get("decommissioned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        "decommissioned"
    } else {
        "online"
    };
    let last_heartbeat = data.get("lastSeen").and_then(|v| v.as_str()).map(|s| s.to_string());
    let blob = data.to_string();
    conn.execute(
        "INSERT OR REPLACE INTO agents (name, host, status, last_heartbeat, data) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![name, host, status, last_heartbeat, blob],
    )?;
    Ok(())
}

pub fn db_delete_agent(conn: &Connection, name: &str) -> Result<()> {
    conn.execute("DELETE FROM agents WHERE name = ?1", params![name])?;
    Ok(())
}

pub fn db_upsert_queue_item(conn: &Connection, item: &Value) -> Result<()> {
    let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
    let priority = item.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
    let now = chrono::Utc::now().to_rfc3339();
    let created_at = item
        .get("created")
        .or_else(|| item.get("created_at"))
        .and_then(|v| v.as_str())
        .unwrap_or(&now);
    let blob = item.to_string();
    conn.execute(
        "INSERT OR REPLACE INTO queue_items \
         (id, status, priority, created_at, updated_at, data) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, status, priority, created_at, &now, blob],
    )?;
    Ok(())
}

pub fn db_upsert_queue_completed(conn: &Connection, item: &Value) -> Result<()> {
    let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let now = chrono::Utc::now().to_rfc3339();
    let completed_at = item
        .get("completedAt")
        .or_else(|| item.get("completed_at"))
        .and_then(|v| v.as_str())
        .unwrap_or(&now);
    let blob = item.to_string();
    conn.execute(
        "INSERT OR REPLACE INTO queue_completed (id, completed_at, data) VALUES (?1, ?2, ?3)",
        params![id, completed_at, blob],
    )?;
    Ok(())
}

pub fn db_upsert_secret(conn: &Connection, key: &str, value: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO secrets (key, value, updated_at) VALUES (?1, ?2, ?3)",
        params![key, value, now],
    )?;
    Ok(())
}

pub fn db_delete_secret(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM secrets WHERE key = ?1", params![key])?;
    Ok(())
}

pub fn db_upsert_project(conn: &Connection, project: &Value) -> Result<()> {
    let id = project
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| project.get("full_name").and_then(|v| v.as_str()))
        .unwrap_or("");
    let name = project.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let full_name = project.get("full_name").and_then(|v| v.as_str()).unwrap_or("");
    let blob = project.to_string();
    conn.execute(
        "INSERT OR REPLACE INTO projects (id, name, full_name, data) VALUES (?1, ?2, ?3, ?4)",
        params![id, name, full_name, blob],
    )?;
    Ok(())
}

pub fn db_delete_project(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
    Ok(())
}

/// Insert a bus message into SQLite. Used when SQLite mode is active.
pub fn insert_bus_message(conn: &Connection, msg: &Value) -> Result<()> {
    let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let seq = msg.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
    let ts = msg.get("ts").and_then(|v| v.as_str()).unwrap_or("");
    let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("text");
    let from = msg.get("from").and_then(|v| v.as_str());
    let to = msg.get("to").and_then(|v| v.as_str());
    let subject = msg.get("subject").and_then(|v| v.as_str());
    let topic = msg.get("topic").and_then(|v| v.as_str());
    let body = msg.get("body").and_then(|v| v.as_str());
    let thread_id = msg.get("thread_id").and_then(|v| v.as_str());
    let target = msg.get("target").and_then(|v| v.as_str());
    let emoji = msg.get("emoji").and_then(|v| v.as_str());
    let action = msg.get("action").and_then(|v| v.as_str());
    let blob = msg.to_string();

    conn.execute(
        "INSERT OR IGNORE INTO bus_messages
         (id, seq, ts, msg_type, from_agent, to_agent, subject, topic, body,
          thread_id, target, emoji, action, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![id, seq, ts, msg_type, from, to, subject, topic, body,
                thread_id, target, emoji, action, blob],
    )?;
    Ok(())
}

// ── Auth DB (always-on, separate from the optional ACC_DB_PATH database) ─────

/// Open (or create) the auth database at `path`.
/// Schema: users(id, username, token_hash, created_at, last_seen).
pub fn open_auth(path: &str) -> Result<Connection> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS users (
            id          TEXT PRIMARY KEY,
            username    TEXT NOT NULL UNIQUE,
            token_hash  TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            last_seen   TEXT
        );
    ")?;
    tracing::info!("Auth database opened: {}", path);
    Ok(conn)
}

/// Always-on fleet database: opens/creates at the given path with fleet_tasks schema.
pub fn open_fleet(path: &str) -> Result<Connection> {
    open(path)
}

/// Load all token hashes from the auth DB (used to seed the in-memory cache).
pub fn auth_all_token_hashes(conn: &Connection) -> Vec<String> {
    let mut stmt = match conn.prepare("SELECT token_hash FROM users") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let result: Vec<String> = match stmt.query_map([], |row| row.get::<_, String>(0)) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_init() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        init_schema(&conn).unwrap();
        run_migrations(&conn).unwrap();
        assert_eq!(get_schema_version(&conn), CURRENT_VERSION);
    }

    #[test]
    fn test_migrate_empty() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // Should not panic on missing files
        let (q, a, s, p) = migrate_from_json(&conn, "/nonexistent/q.json", "/nonexistent/a.json",
                                              "/nonexistent/s.json", "/nonexistent/p.json");
        assert_eq!((q, a, s, p), (0, 0, 0, 0));
    }
}
