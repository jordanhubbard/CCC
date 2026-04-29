/// SQLite database layer for acc-server.
///
/// Enabled by setting `ACC_DB_PATH` to a `.db` file path.
/// On first start, data is migrated from existing JSON files automatically.
/// Once migrated, JSON files become stale and are no longer written.
///
/// Schema version is tracked in the `schema_version` table.
/// Migrations are additive — run in order, each runs exactly once.
use rusqlite::{params, Connection, Result};
use serde_json::Value;
use std::path::Path;

const CURRENT_VERSION: i64 = 9;

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

        CREATE TABLE IF NOT EXISTS gateway_sessions (
            session_key   TEXT PRIMARY KEY,
            agent_name    TEXT NOT NULL DEFAULT '',
            messages_json TEXT NOT NULL DEFAULT '[]',
            workspace     TEXT NOT NULL DEFAULT 'default',
            updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        );

        CREATE TABLE IF NOT EXISTS conversation_chains (
            id            TEXT PRIMARY KEY,
            source        TEXT NOT NULL,
            workspace     TEXT NOT NULL DEFAULT '',
            channel_id    TEXT NOT NULL DEFAULT '',
            thread_id     TEXT NOT NULL DEFAULT '',
            root_event_id TEXT,
            title         TEXT NOT NULL DEFAULT '',
            summary       TEXT NOT NULL DEFAULT '',
            status        TEXT NOT NULL DEFAULT 'active',
            outcome       TEXT,
            created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            closed_at     TEXT,
            metadata      TEXT NOT NULL DEFAULT '{}'
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_chains_source
            ON conversation_chains(source, workspace, channel_id, thread_id);
        CREATE INDEX IF NOT EXISTS idx_conversation_chains_status
            ON conversation_chains(status, updated_at DESC);

        CREATE TABLE IF NOT EXISTS conversation_chain_events (
            id              TEXT PRIMARY KEY,
            chain_id        TEXT NOT NULL,
            event_type      TEXT NOT NULL,
            source_event_id TEXT,
            actor_id        TEXT,
            actor_name      TEXT,
            actor_kind      TEXT,
            text            TEXT,
            occurred_at     TEXT NOT NULL,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            metadata        TEXT NOT NULL DEFAULT '{}'
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_chain_events_chain
            ON conversation_chain_events(chain_id, occurred_at ASC);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_conversation_chain_events_source
            ON conversation_chain_events(chain_id, source_event_id)
            WHERE source_event_id IS NOT NULL AND source_event_id != '';

        CREATE TABLE IF NOT EXISTS conversation_chain_participants (
            chain_id         TEXT NOT NULL,
            participant_id   TEXT NOT NULL,
            platform         TEXT NOT NULL DEFAULT '',
            display_name     TEXT,
            participant_kind TEXT NOT NULL DEFAULT 'human',
            first_seen_at    TEXT NOT NULL,
            last_seen_at     TEXT NOT NULL,
            metadata         TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (chain_id, participant_id)
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_chain_participants_id
            ON conversation_chain_participants(participant_id);

        CREATE TABLE IF NOT EXISTS conversation_chain_entities (
            chain_id      TEXT NOT NULL,
            entity_type   TEXT NOT NULL,
            entity_id     TEXT NOT NULL,
            label         TEXT,
            first_seen_at TEXT NOT NULL,
            last_seen_at  TEXT NOT NULL,
            metadata      TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (chain_id, entity_type, entity_id)
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_chain_entities_id
            ON conversation_chain_entities(entity_type, entity_id);

        CREATE TABLE IF NOT EXISTS conversation_chain_tasks (
            chain_id     TEXT NOT NULL,
            task_id      TEXT NOT NULL,
            relationship TEXT NOT NULL DEFAULT 'spawned',
            created_at   TEXT NOT NULL,
            resolved_at  TEXT,
            metadata     TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (chain_id, task_id)
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_chain_tasks_task
            ON conversation_chain_tasks(task_id);

        CREATE TABLE IF NOT EXISTS vault_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS vault_secrets (
            key        TEXT PRIMARY KEY,
            value_b64  TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        );
    ")?;
    Ok(())
}

fn get_schema_version(conn: &Connection) -> i64 {
    conn.query_row("SELECT MAX(version) FROM schema_version", [], |row| {
        row.get::<_, Option<i64>>(0)
    })
    .unwrap_or(None)
    .unwrap_or(0)
}

fn set_schema_version(conn: &Connection, version: i64) -> Result<()> {
    conn.execute("DELETE FROM schema_version", [])?;
    conn.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        params![version],
    )?;
    Ok(())
}

fn run_migrations(conn: &Connection) -> Result<()> {
    let version = get_schema_version(conn);
    if version >= CURRENT_VERSION {
        return Ok(());
    }

    // Migration v1: initial schema is already created by init_schema.
    tracing::info!(
        "Database schema at version {} (current: {})",
        version,
        CURRENT_VERSION
    );

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
        conn.execute_batch(
            "
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
        ",
        )?;
        set_schema_version(conn, 3)?;
    }

    if version < 4 {
        let col_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('fleet_tasks') WHERE name='task_type'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;
        if !col_exists {
            conn.execute_batch(
                "
                ALTER TABLE fleet_tasks ADD COLUMN task_type TEXT NOT NULL DEFAULT 'work';
                ALTER TABLE fleet_tasks ADD COLUMN review_of TEXT;
                ALTER TABLE fleet_tasks ADD COLUMN phase TEXT;
                ALTER TABLE fleet_tasks ADD COLUMN blocked_by TEXT NOT NULL DEFAULT '[]';
                ALTER TABLE fleet_tasks ADD COLUMN review_result TEXT;
            ",
            )?;
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
            ("blocked_by", "TEXT NOT NULL DEFAULT '[]'"),
            ("review_result", "TEXT"),
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('fleet_tasks') WHERE name=?1",
                    rusqlite::params![col],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0)
                > 0;
            if !exists {
                conn.execute_batch(&format!(
                    "ALTER TABLE fleet_tasks ADD COLUMN {} {};",
                    col, def
                ))?;
            }
        }
        set_schema_version(conn, 5)?;
    }

    if version < 6 {
        // Add output and inputs columns to fleet_tasks (idempotent)
        for (col, def) in &[("output", "TEXT"), ("inputs", "TEXT NOT NULL DEFAULT '{}'")] {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('fleet_tasks') WHERE name=?1",
                    rusqlite::params![col],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0)
                > 0;
            if !exists {
                conn.execute_batch(&format!(
                    "ALTER TABLE fleet_tasks ADD COLUMN {} {};",
                    col, def
                ))?;
            }
        }
        // Create conversations table and index
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS conversations (
                task_id    TEXT NOT NULL,
                turn_index INTEGER NOT NULL,
                role       TEXT NOT NULL,
                content    TEXT NOT NULL,
                input_tokens  INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                stop_reason   TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                PRIMARY KEY (task_id, turn_index)
            );
            CREATE INDEX IF NOT EXISTS idx_conversations_task ON conversations(task_id, turn_index);
        ",
        )?;
        set_schema_version(conn, 6)?;
    }

    if version < 7 {
        // Add source column to fleet_tasks for unified task model
        let has_source: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('fleet_tasks') WHERE name='source'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_source {
            conn.execute_batch(
                "ALTER TABLE fleet_tasks ADD COLUMN source TEXT NOT NULL DEFAULT 'fleet';",
            )?;
            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_fleet_tasks_source ON fleet_tasks(source, status);",
            )?;
        }
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            params![7],
        )?;
        tracing::info!("Migration v7 applied: source column on fleet_tasks");
    }

    if version < 8 {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS vault_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS vault_secrets (
                key        TEXT PRIMARY KEY,
                value_b64  TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );
        ",
        )?;
        tracing::info!("Migration v8 applied: vault_meta and vault_secrets tables");
    }

    if version < 9 {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS conversation_chains (
                id            TEXT PRIMARY KEY,
                source        TEXT NOT NULL,
                workspace     TEXT NOT NULL DEFAULT '',
                channel_id    TEXT NOT NULL DEFAULT '',
                thread_id     TEXT NOT NULL DEFAULT '',
                root_event_id TEXT,
                title         TEXT NOT NULL DEFAULT '',
                summary       TEXT NOT NULL DEFAULT '',
                status        TEXT NOT NULL DEFAULT 'active',
                outcome       TEXT,
                created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                closed_at     TEXT,
                metadata      TEXT NOT NULL DEFAULT '{}'
            );
            CREATE INDEX IF NOT EXISTS idx_conversation_chains_source
                ON conversation_chains(source, workspace, channel_id, thread_id);
            CREATE INDEX IF NOT EXISTS idx_conversation_chains_status
                ON conversation_chains(status, updated_at DESC);

            CREATE TABLE IF NOT EXISTS conversation_chain_events (
                id              TEXT PRIMARY KEY,
                chain_id        TEXT NOT NULL,
                event_type      TEXT NOT NULL,
                source_event_id TEXT,
                actor_id        TEXT,
                actor_name      TEXT,
                actor_kind      TEXT,
                text            TEXT,
                occurred_at     TEXT NOT NULL,
                created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                metadata        TEXT NOT NULL DEFAULT '{}'
            );
            CREATE INDEX IF NOT EXISTS idx_conversation_chain_events_chain
                ON conversation_chain_events(chain_id, occurred_at ASC);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_conversation_chain_events_source
                ON conversation_chain_events(chain_id, source_event_id)
                WHERE source_event_id IS NOT NULL AND source_event_id != '';

            CREATE TABLE IF NOT EXISTS conversation_chain_participants (
                chain_id         TEXT NOT NULL,
                participant_id   TEXT NOT NULL,
                platform         TEXT NOT NULL DEFAULT '',
                display_name     TEXT,
                participant_kind TEXT NOT NULL DEFAULT 'human',
                first_seen_at    TEXT NOT NULL,
                last_seen_at     TEXT NOT NULL,
                metadata         TEXT NOT NULL DEFAULT '{}',
                PRIMARY KEY (chain_id, participant_id)
            );
            CREATE INDEX IF NOT EXISTS idx_conversation_chain_participants_id
                ON conversation_chain_participants(participant_id);

            CREATE TABLE IF NOT EXISTS conversation_chain_entities (
                chain_id      TEXT NOT NULL,
                entity_type   TEXT NOT NULL,
                entity_id     TEXT NOT NULL,
                label         TEXT,
                first_seen_at TEXT NOT NULL,
                last_seen_at  TEXT NOT NULL,
                metadata      TEXT NOT NULL DEFAULT '{}',
                PRIMARY KEY (chain_id, entity_type, entity_id)
            );
            CREATE INDEX IF NOT EXISTS idx_conversation_chain_entities_id
                ON conversation_chain_entities(entity_type, entity_id);

            CREATE TABLE IF NOT EXISTS conversation_chain_tasks (
                chain_id     TEXT NOT NULL,
                task_id      TEXT NOT NULL,
                relationship TEXT NOT NULL DEFAULT 'spawned',
                created_at   TEXT NOT NULL,
                resolved_at  TEXT,
                metadata     TEXT NOT NULL DEFAULT '{}',
                PRIMARY KEY (chain_id, task_id)
            );
            CREATE INDEX IF NOT EXISTS idx_conversation_chain_tasks_task
                ON conversation_chain_tasks(task_id);
        ",
        )?;
        tracing::info!("Migration v9 applied: conversation chain tables");
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
        q,
        a,
        s,
        p
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
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");
            let priority = item.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
            let created_at = item
                .get("created_at")
                .and_then(|v| v.as_str())
                .unwrap_or(&now);
            let updated_at = item
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or(&now);
            let blob = item.to_string();

            if id.is_empty() {
                continue;
            }
            let r = conn.execute(
                "INSERT OR IGNORE INTO queue_items (id, status, priority, created_at, updated_at, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, status, priority, created_at, updated_at, blob],
            );
            if r.is_ok() {
                count += 1;
            }
        }
    }

    if let Some(completed) = data.get("completed").and_then(|v| v.as_array()) {
        for item in completed {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let completed_at = item
                .get("completed_at")
                .and_then(|v| v.as_str())
                .unwrap_or(&now);
            let blob = item.to_string();
            if id.is_empty() {
                continue;
            }
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
        if name.is_empty() {
            continue;
        }
        let host = agent.get("host").and_then(|v| v.as_str()).unwrap_or("");
        let status = agent
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("offline");
        let blob = agent.to_string();

        let r = conn.execute(
            "INSERT OR IGNORE INTO agents (name, host, status, data) VALUES (?1, ?2, ?3, ?4)",
            params![name, host, status, blob],
        );
        if r.is_ok() {
            count += 1;
        }
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
        let val_str = value
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| value.to_string());
        let r = conn.execute(
            "INSERT OR IGNORE INTO secrets (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![key, val_str, now],
        );
        if r.is_ok() {
            count += 1;
        }
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
        let id = project
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| project.get("full_name").and_then(|v| v.as_str()))
            .unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let name = project.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let full_name = project
            .get("full_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let blob = project.to_string();

        let r = conn.execute(
            "INSERT OR IGNORE INTO projects (id, name, full_name, data) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, full_name, blob],
        );
        if r.is_ok() {
            count += 1;
        }
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
    if let Ok(mut stmt) =
        conn.prepare("SELECT data FROM queue_completed ORDER BY completed_at DESC LIMIT 500")
    {
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
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                let (key, val) = row;
                let json_val =
                    serde_json::from_str::<Value>(&val).unwrap_or_else(|_| Value::String(val));
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
    let last_heartbeat = data
        .get("lastSeen")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
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
    let status = item
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending");
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
    let full_name = project
        .get("full_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
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
        params![
            id, seq, ts, msg_type, from, to, subject, topic, body, thread_id, target, emoji,
            action, blob
        ],
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
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS users (
            id          TEXT PRIMARY KEY,
            username    TEXT NOT NULL UNIQUE,
            token_hash  TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            last_seen   TEXT
        );
    ",
    )?;
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

// ── DAG helpers ───────────────────────────────────────────────────────────────

/// Returns all `blocked_by` mappings from fleet_tasks: task_id → list of blockers.
/// Used by cycle detection before inserting or updating a task's dependencies.
pub fn db_all_blocked_by(conn: &Connection) -> std::collections::HashMap<String, Vec<String>> {
    let mut stmt = match conn.prepare(
        "SELECT id, blocked_by FROM fleet_tasks \
         WHERE blocked_by IS NOT NULL AND blocked_by != '[]'",
    ) {
        Ok(s) => s,
        Err(_) => return std::collections::HashMap::new(),
    };
    let mut map = std::collections::HashMap::new();
    let _ = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map(|rows| {
            for (id, bj) in rows.flatten() {
                let blockers: Vec<String> = serde_json::from_str(&bj).unwrap_or_default();
                if !blockers.is_empty() {
                    map.insert(id, blockers);
                }
            }
        });
    map
}

/// Finds open tasks whose `blocked_by` list contains `completed_id` and are now
/// fully unblocked (all blockers completed and not review-rejected).
///
/// Called after a task completes or a review is approved to discover tasks
/// that should now be dispatched.
pub fn db_find_newly_unblocked(conn: &Connection, completed_id: &str) -> Vec<String> {
    // LIKE search: the JSON array encodes IDs as quoted strings, so we look
    // for `"task-abc"` within the stored JSON text. This is reliable because
    // task IDs never contain embedded quotes.
    let pattern = format!("%\"{completed_id}\"%");
    let mut stmt = match conn.prepare(
        "SELECT id, blocked_by FROM fleet_tasks WHERE status='open' AND blocked_by LIKE ?1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let candidates: Vec<(String, Vec<String>)> = stmt
        .query_map(params![pattern], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map(|rows| {
            rows.flatten()
                .filter_map(|(id, bj)| {
                    let blockers: Vec<String> = serde_json::from_str(&bj).unwrap_or_default();
                    // Confirm the LIKE match is exact (not a substring false-positive).
                    if blockers.contains(&completed_id.to_string()) {
                        Some((id, blockers))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // For each candidate, verify ALL its blockers are satisfied.
    candidates
        .into_iter()
        .filter(|(_, blockers)| {
            blockers.iter().all(|blocker_id| {
                conn.query_row(
                    "SELECT COUNT(*) FROM fleet_tasks WHERE id=?1 AND status='completed' \
                     AND (review_result IS NULL OR review_result != 'rejected')",
                    params![blocker_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0)
                    > 0
            })
        })
        .map(|(id, _)| id)
        .collect()
}

/// Persist one conversation turn for a task.
pub fn db_save_turn(
    conn: &Connection,
    task_id: &str,
    turn_index: i64,
    role: &str,
    content: &str,
    input_tokens: i64,
    output_tokens: i64,
    stop_reason: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO conversations \
         (task_id, turn_index, role, content, input_tokens, output_tokens, stop_reason) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![
            task_id,
            turn_index,
            role,
            content,
            input_tokens,
            output_tokens,
            stop_reason
        ],
    )?;
    Ok(())
}

/// Load all turns for a task, ordered by turn_index ascending.
pub fn db_load_turns(conn: &Connection, task_id: &str) -> Vec<serde_json::Value> {
    use serde_json::json;
    let mut stmt = match conn.prepare(
        "SELECT turn_index,role,content,input_tokens,output_tokens,stop_reason,created_at \
         FROM conversations WHERE task_id=?1 ORDER BY turn_index ASC",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(rusqlite::params![task_id], |row| {
        let content_str: String = row.get(2)?;
        let content_val: serde_json::Value =
            serde_json::from_str(&content_str).unwrap_or(serde_json::Value::String(content_str));
        Ok(json!({
            "turn_index":    row.get::<_, i64>(0)?,
            "role":          row.get::<_, String>(1)?,
            "content":       content_val,
            "input_tokens":  row.get::<_, i64>(3)?,
            "output_tokens": row.get::<_, i64>(4)?,
            "stop_reason":   row.get::<_, Option<String>>(5)?,
            "created_at":    row.get::<_, String>(6)?,
        }))
    })
    .ok()
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Mirror a queue item claim into the corresponding fleet_task (source='queue').
/// Best-effort — silently ignores errors so queue lifecycle is never blocked.
pub fn db_fleet_sync_claim(
    conn: &rusqlite::Connection,
    item_id: &str,
    claimed_by: &str,
    claimed_at: &str,
) {
    let expires = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::minutes(30))
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    let _ = conn.execute(
        "UPDATE fleet_tasks SET status='claimed', claimed_by=?1, claimed_at=?2,
         claim_expires_at=?3, updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now')
         WHERE id=?4 AND source='queue'",
        rusqlite::params![claimed_by, claimed_at, expires, item_id],
    );
}

/// Mirror a queue item completion into the corresponding fleet_task (source='queue').
pub fn db_fleet_sync_complete(
    conn: &rusqlite::Connection,
    item_id: &str,
    completed_by: &str,
    output: &str,
) {
    let _ = conn.execute(
        "UPDATE fleet_tasks SET status='completed', completed_by=?1, completed_at=strftime('%Y-%m-%dT%H:%M:%SZ','now'),
         output=?2, updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now')
         WHERE id=?3 AND source='queue'",
        rusqlite::params![completed_by, output, item_id],
    );
}

/// Mirror a queue item failure into the corresponding fleet_task (source='queue').
/// If attempts < max_attempts the queue item goes back to pending (unclaimed);
/// if blocked (max attempts exceeded) we set fleet_task to 'failed'.
pub fn db_fleet_sync_fail(conn: &rusqlite::Connection, item_id: &str, blocked: bool) {
    let status = if blocked { "failed" } else { "open" };
    let _ = conn.execute(
        "UPDATE fleet_tasks SET status=?1, claimed_by=NULL, claimed_at=NULL,
         claim_expires_at=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now')
         WHERE id=?2 AND source='queue'",
        rusqlite::params![status, item_id],
    );
}

/// Extend fleet_task claim expiry for a queue item keepalive.
pub fn db_fleet_sync_keepalive(conn: &rusqlite::Connection, item_id: &str) {
    let _ = conn.execute(
        "UPDATE fleet_tasks SET claim_expires_at=datetime('now','+30 minutes'),
         updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now')
         WHERE id=?1 AND source='queue'",
        rusqlite::params![item_id],
    );
}

/// Create a fleet_task record mirroring a queue item, with source='queue'.
/// Called when POST /api/queue creates a new item so it's visible to the fleet task system.
pub fn db_create_fleet_task_from_queue(
    conn: &Connection,
    item_id: &str,
    title: &str,
    description: &str,
    priority_str: &str,
    project_id: &str,
    metadata: &serde_json::Value,
) -> Result<()> {
    // Map queue priority string to integer
    let priority_int: i64 = match priority_str {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "normal" => 2,
        "low" => 3,
        "idea" => 4,
        _ => 2,
    };
    let meta_str = metadata.to_string();
    conn.execute(
        "INSERT OR IGNORE INTO fleet_tasks
         (id, project_id, title, description, status, priority, source, metadata, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'open', ?5, 'queue', ?6,
                 strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                 strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        params![item_id, project_id, title, description, priority_int, meta_str],
    )?;
    Ok(())
}

/// Collect outputs from all completed blockers and write them as the task's inputs map.
/// Call this after a task becomes newly unblocked.
pub fn db_populate_inputs(
    conn: &Connection,
    task_id: &str,
    blocked_by: &[String],
) -> rusqlite::Result<()> {
    let mut inputs = serde_json::Map::new();
    for blocker_id in blocked_by {
        let output: Option<String> = conn
            .query_row(
                "SELECT output FROM fleet_tasks WHERE id=?1 AND status='completed'",
                rusqlite::params![blocker_id],
                |r| r.get(0),
            )
            .unwrap_or(None);
        if let Some(s) = output {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                inputs.insert(blocker_id.clone(), v);
            }
        }
    }
    let inputs_str = serde_json::to_string(&inputs).unwrap_or_else(|_| "{}".to_string());
    conn.execute(
        "UPDATE fleet_tasks SET inputs=?1 WHERE id=?2",
        rusqlite::params![inputs_str, task_id],
    )?;
    Ok(())
}

// ── Vault persistence helpers ─────────────────────────────────────────────────

/// Load the vault salt from vault_meta. Returns None if not yet set.
pub fn db_load_vault_salt(conn: &Connection) -> Option<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    conn.query_row(
        "SELECT value FROM vault_meta WHERE key = 'salt'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|s| B64.decode(s).ok())
}

/// Persist the vault salt (base64-encoded) into vault_meta.
pub fn db_save_vault_salt(conn: &Connection, salt: &[u8]) {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let b64 = B64.encode(salt);
    let _ = conn.execute(
        "INSERT OR REPLACE INTO vault_meta (key, value) VALUES ('salt', ?1)",
        params![b64],
    );
}

/// Load all encrypted blobs from vault_secrets (key → base64 ciphertext).
pub fn db_load_vault_blobs(conn: &Connection) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT key, value_b64 FROM vault_secrets") {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
        }
    }
    map
}

/// Replace all encrypted blobs in vault_secrets with the given map.
pub fn db_flush_vault_blobs(conn: &Connection, blobs: &std::collections::HashMap<String, String>) {
    let _ = conn.execute("DELETE FROM vault_secrets", []);
    let now = chrono::Utc::now().to_rfc3339();
    for (key, b64) in blobs {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO vault_secrets (key, value_b64, updated_at) VALUES (?1, ?2, ?3)",
            params![key, b64, now],
        );
    }
}

// ── Gateway session helpers ───────────────────────────────────────────────────

pub fn get_session(conn: &Connection, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    let mut stmt =
        conn.prepare_cached("SELECT messages_json FROM gateway_sessions WHERE session_key = ?1")?;
    let mut rows = stmt.query(rusqlite::params![key])?;
    if let Some(row) = rows.next()? {
        let json: String = row.get(0)?;
        let msgs: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();
        Ok(Some(msgs))
    } else {
        Ok(None)
    }
}

pub fn put_session(
    conn: &Connection,
    key: &str,
    agent: &str,
    workspace: &str,
    messages: &[serde_json::Value],
) -> Result<()> {
    let json = serde_json::to_string(messages).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "INSERT INTO gateway_sessions (session_key, agent_name, workspace, messages_json, updated_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
         ON CONFLICT(session_key) DO UPDATE SET
             agent_name    = excluded.agent_name,
             workspace     = excluded.workspace,
             messages_json = excluded.messages_json,
             updated_at    = excluded.updated_at",
        rusqlite::params![key, agent, workspace, json],
    )?;
    Ok(())
}

pub fn delete_session(conn: &Connection, key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM gateway_sessions WHERE session_key = ?1",
        rusqlite::params![key],
    )?;
    Ok(())
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
        let (q, a, s, p) = migrate_from_json(
            &conn,
            "/nonexistent/q.json",
            "/nonexistent/a.json",
            "/nonexistent/s.json",
            "/nonexistent/p.json",
        );
        assert_eq!((q, a, s, p), (0, 0, 0, 0));
    }

    fn make_test_conn_with_queue_task(item_id: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        init_schema(&conn).unwrap();
        run_migrations(&conn).unwrap();
        // Insert a fleet_task with source='queue' to act as the mirror target.
        conn.execute(
            "INSERT INTO fleet_tasks (id, project_id, title, description, status, priority, source)
             VALUES (?1, 'queue', 'Test task', '', 'open', 2, 'queue')",
            params![item_id],
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_fleet_sync_claim_updates_status() {
        let conn = make_test_conn_with_queue_task("wq-test-001");
        db_fleet_sync_claim(&conn, "wq-test-001", "hermes", "2026-04-26T00:00:00Z");
        let (status, claimed_by): (String, String) = conn
            .query_row(
                "SELECT status, claimed_by FROM fleet_tasks WHERE id='wq-test-001'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "claimed");
        assert_eq!(claimed_by, "hermes");
    }

    #[test]
    fn test_fleet_sync_complete_updates_status() {
        let conn = make_test_conn_with_queue_task("wq-test-002");
        db_fleet_sync_complete(&conn, "wq-test-002", "hermes", "done");
        let (status, completed_by, output): (String, String, String) = conn
            .query_row(
                "SELECT status, completed_by, output FROM fleet_tasks WHERE id='wq-test-002'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    ))
                },
            )
            .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(completed_by, "hermes");
        assert_eq!(output, "done");
    }

    #[test]
    fn test_fleet_sync_fail_blocked() {
        let conn = make_test_conn_with_queue_task("wq-test-003");
        // First claim it
        db_fleet_sync_claim(&conn, "wq-test-003", "hermes", "2026-04-26T00:00:00Z");
        // Then fail with blocked=true
        db_fleet_sync_fail(&conn, "wq-test-003", true);
        let (status, claimed_by): (String, Option<String>) = conn
            .query_row(
                "SELECT status, claimed_by FROM fleet_tasks WHERE id='wq-test-003'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert!(claimed_by.is_none());
    }

    #[test]
    fn test_fleet_sync_fail_retry() {
        let conn = make_test_conn_with_queue_task("wq-test-004");
        db_fleet_sync_claim(&conn, "wq-test-004", "hermes", "2026-04-26T00:00:00Z");
        db_fleet_sync_fail(&conn, "wq-test-004", false);
        let status: String = conn
            .query_row(
                "SELECT status FROM fleet_tasks WHERE id='wq-test-004'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "open");
    }

    #[test]
    fn test_fleet_sync_keepalive_updates_expiry() {
        let conn = make_test_conn_with_queue_task("wq-test-005");
        db_fleet_sync_claim(&conn, "wq-test-005", "hermes", "2026-04-26T00:00:00Z");
        db_fleet_sync_keepalive(&conn, "wq-test-005");
        // Just verify it doesn't error and the row still exists
        let status: String = conn
            .query_row(
                "SELECT status FROM fleet_tasks WHERE id='wq-test-005'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "claimed");
    }

    #[test]
    fn test_fleet_sync_no_op_on_missing_id() {
        let conn = make_test_conn_with_queue_task("wq-test-006");
        // Calling sync on a non-existent item should silently do nothing
        db_fleet_sync_claim(&conn, "wq-NONEXISTENT", "hermes", "2026-04-26T00:00:00Z");
        db_fleet_sync_complete(&conn, "wq-NONEXISTENT", "hermes", "done");
        db_fleet_sync_fail(&conn, "wq-NONEXISTENT", false);
        db_fleet_sync_keepalive(&conn, "wq-NONEXISTENT");
        // The real row should be unchanged
        let status: String = conn
            .query_row(
                "SELECT status FROM fleet_tasks WHERE id='wq-test-006'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "open");
    }
}
