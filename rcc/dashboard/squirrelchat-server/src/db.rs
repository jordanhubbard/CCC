use rusqlite::{Connection, Result, params};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use crate::models::{Message, Channel, User, Project, FileInfo, Attachment};

#[derive(Clone)]
pub struct Db(pub Arc<Mutex<Connection>>);

impl Db {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Ok(Db(Arc::new(Mutex::new(conn))))
    }

    pub fn migrate(&self) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS users (
                id        TEXT PRIMARY KEY,
                name      TEXT NOT NULL,
                type      TEXT DEFAULT 'agent',
                status    TEXT DEFAULT 'offline',
                last_seen INTEGER,
                token     TEXT
            );

            CREATE TABLE IF NOT EXISTS channels (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                type        TEXT DEFAULT 'public',
                created_by  TEXT,
                created_at  INTEGER DEFAULT (unixepoch() * 1000),
                description TEXT
            );

            CREATE TABLE IF NOT EXISTS messages (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                ts           INTEGER NOT NULL,
                from_agent   TEXT NOT NULL,
                text         TEXT NOT NULL,
                channel      TEXT NOT NULL DEFAULT 'general',
                thread_id    INTEGER,
                mentions     TEXT,
                slash_result TEXT,
                edited_at    INTEGER,
                created_at   INTEGER DEFAULT (unixepoch() * 1000)
            );

            CREATE TABLE IF NOT EXISTS reactions (
                message_id  INTEGER NOT NULL,
                emoji       TEXT NOT NULL,
                from_agent  TEXT NOT NULL,
                created_at  INTEGER DEFAULT (unixepoch() * 1000),
                PRIMARY KEY (message_id, emoji, from_agent)
            );

            CREATE TABLE IF NOT EXISTS projects (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                description TEXT,
                tags        TEXT,
                assignee    TEXT,
                status      TEXT DEFAULT 'active',
                created_at  INTEGER DEFAULT (unixepoch() * 1000),
                updated_at  INTEGER DEFAULT (unixepoch() * 1000)
            );

            CREATE TABLE IF NOT EXISTS project_files (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id  TEXT NOT NULL,
                filename    TEXT NOT NULL,
                content     BLOB,
                encoding    TEXT DEFAULT 'utf8',
                size        INTEGER,
                created_at  INTEGER DEFAULT (unixepoch() * 1000),
                UNIQUE(project_id, filename)
            );

            -- Seed default channels if empty
            INSERT OR IGNORE INTO channels (id, name, type, created_by, description)
            VALUES
                ('general',  'general',  'public', 'rocky', 'General chat'),
                ('ops',      'ops',      'public', 'rocky', 'Ops and infra'),
                ('agents',   'agents',   'public', 'rocky', 'Agent coordination');

            -- Pinned messages per channel
            CREATE TABLE IF NOT EXISTS pins (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id  TEXT NOT NULL,
                message_id  INTEGER NOT NULL,
                pinned_by   TEXT NOT NULL,
                pinned_at   INTEGER DEFAULT (unixepoch() * 1000),
                UNIQUE(channel_id, message_id)
            );

            -- Message attachments (file sharing)
            CREATE TABLE IF NOT EXISTS attachments (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id  INTEGER NOT NULL,
                filename    TEXT NOT NULL,
                mime_type   TEXT DEFAULT 'application/octet-stream',
                size        INTEGER,
                content     BLOB,
                created_at  INTEGER DEFAULT (unixepoch() * 1000)
            );

            -- FTS5 full-text search index over messages
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                text,
                from_agent,
                channel,
                content='messages',
                content_rowid='id'
            );

            -- Keep FTS in sync via triggers
            CREATE TRIGGER IF NOT EXISTS messages_fts_insert
            AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, text, from_agent, channel)
                VALUES (new.id, new.text, new.from_agent, new.channel);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_fts_delete
            AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, text, from_agent, channel)
                VALUES ('delete', old.id, old.text, old.from_agent, old.channel);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_fts_update
            AFTER UPDATE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, text, from_agent, channel)
                VALUES ('delete', old.id, old.text, old.from_agent, old.channel);
                INSERT INTO messages_fts(rowid, text, from_agent, channel)
                VALUES (new.id, new.text, new.from_agent, new.channel);
            END;

            -- Per-user read cursors: last message ts seen in each channel
            CREATE TABLE IF NOT EXISTS read_cursors (
                user_id    TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                last_read  INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER DEFAULT (unixepoch() * 1000),
                PRIMARY KEY (user_id, channel_id)
            );

            -- Scheduled messages for future delivery
            CREATE TABLE IF NOT EXISTS scheduled_messages (
                id         TEXT PRIMARY KEY,
                channel_id TEXT NOT NULL,
                sender     TEXT NOT NULL,
                text       TEXT NOT NULL,
                deliver_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                delivered  INTEGER NOT NULL DEFAULT 0
            );
        "#)?;
        Ok(())
    }

    // ── Reactions helper ──────────────────────────────────────────────────────

    fn load_reactions(&self, conn: &Connection, msg_id: i64) -> HashMap<String, Vec<String>> {
        let mut stmt = conn.prepare(
            "SELECT emoji, from_agent FROM reactions WHERE message_id = ? ORDER BY emoji, created_at"
        ).unwrap();
        let rows = stmt.query_map([msg_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).unwrap();

        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows.flatten() {
            map.entry(row.0).or_default().push(row.1);
        }
        map
    }

    fn load_reply_count(&self, conn: &Connection, msg_id: i64) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE thread_id = ?",
            [msg_id],
            |r| r.get(0),
        ).unwrap_or(0)
    }

    fn row_to_message(&self, conn: &Connection, row: &rusqlite::Row) -> rusqlite::Result<Message> {
        let id: i64 = row.get(0)?;
        let ts: i64 = row.get(1)?;
        let from_agent: String = row.get(2)?;
        let text: String = row.get(3)?;
        let channel: String = row.get(4)?;
        let thread_id: Option<i64> = row.get(5)?;
        let mentions_raw: Option<String> = row.get(6)?;
        let slash_result: Option<String> = row.get(7)?;

        let mentions: Vec<String> = mentions_raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let reactions = self.load_reactions(conn, id);
        let reply_count = self.load_reply_count(conn, id);

        Ok(Message { id, ts, from_agent, text, channel, mentions, thread_id, reply_count, reactions_map: reactions, slash_result })
    }

    // ── Messages ──────────────────────────────────────────────────────────────

    pub fn get_messages(&self, channel: &str, limit: i64, since: Option<i64>) -> Result<Vec<Message>> {
        let conn = self.0.lock().unwrap();
        let since = since.unwrap_or(0);
        let mut stmt = conn.prepare(
            "SELECT id, ts, from_agent, text, channel, thread_id, mentions, slash_result
             FROM messages
             WHERE channel = ? AND thread_id IS NULL AND ts > ?
             ORDER BY ts ASC
             LIMIT ?"
        )?;
        let msgs: Vec<Message> = stmt.query_map(params![channel, since, limit], |row| {
            self.row_to_message(&conn, row)
        })?.filter_map(|r| r.ok()).collect();
        Ok(msgs)
    }

    pub fn get_message(&self, id: i64) -> Result<Option<Message>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, ts, from_agent, text, channel, thread_id, mentions, slash_result
             FROM messages WHERE id = ?"
        )?;
        let msg = stmt.query_row([id], |row| self.row_to_message(&conn, row)).ok();
        Ok(msg)
    }

    pub fn insert_message(
        &self,
        from: &str,
        text: &str,
        channel: &str,
        mentions: &[String],
        thread_id: Option<i64>,
    ) -> Result<i64> {
        let conn = self.0.lock().unwrap();
        let ts = now_ms();
        let mentions_json = serde_json::to_string(mentions).unwrap();
        conn.execute(
            "INSERT INTO messages (ts, from_agent, text, channel, thread_id, mentions)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![ts, from, text, channel, thread_id, mentions_json],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_message(&self, id: i64, text: &str) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let n = conn.execute(
            "UPDATE messages SET text = ?, edited_at = ? WHERE id = ?",
            params![text, now_ms(), id],
        )?;
        Ok(n > 0)
    }

    pub fn delete_message(&self, id: i64) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let n = conn.execute("DELETE FROM messages WHERE id = ?", [id])?;
        Ok(n > 0)
    }

    pub fn get_thread(&self, parent_id: i64, limit: i64) -> Result<Vec<Message>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, ts, from_agent, text, channel, thread_id, mentions, slash_result
             FROM messages WHERE thread_id = ?
             ORDER BY ts ASC LIMIT ?"
        )?;
        let msgs: Vec<Message> = stmt.query_map(params![parent_id, limit], |row| {
            self.row_to_message(&conn, row)
        })?.filter_map(|r| r.ok()).collect();
        Ok(msgs)
    }

    // ── Reactions ─────────────────────────────────────────────────────────────

    pub fn add_reaction(&self, msg_id: i64, from: &str, emoji: &str) -> Result<HashMap<String, Vec<String>>> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO reactions (message_id, emoji, from_agent) VALUES (?, ?, ?)",
            params![msg_id, emoji, from],
        )?;
        Ok(self.load_reactions(&conn, msg_id))
    }

    pub fn remove_reaction(&self, msg_id: i64, from: &str, emoji: &str) -> Result<HashMap<String, Vec<String>>> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "DELETE FROM reactions WHERE message_id = ? AND emoji = ? AND from_agent = ?",
            params![msg_id, emoji, from],
        )?;
        Ok(self.load_reactions(&conn, msg_id))
    }

    // ── Channels ──────────────────────────────────────────────────────────────

    pub fn get_channels(&self) -> Result<Vec<Channel>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, type, created_by, created_at, description FROM channels ORDER BY created_at ASC"
        )?;
        let channels: Vec<Channel> = stmt.query_map([], |row| {
            Ok(Channel {
                id: row.get(0)?,
                name: row.get(1)?,
                channel_type: row.get(2)?,
                created_by: row.get(3)?,
                created_at: row.get(4)?,
                description: row.get(5)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(channels)
    }

    pub fn insert_channel(&self, id: &str, name: &str, ch_type: &str, created_by: &str, description: Option<&str>) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO channels (id, name, type, created_by, description) VALUES (?, ?, ?, ?, ?)",
            params![id, name, ch_type, created_by, description],
        )?;
        Ok(())
    }

    pub fn get_channel(&self, id: &str) -> Result<Option<Channel>> {
        let conn = self.0.lock().unwrap();
        let ch = conn.query_row(
            "SELECT id, name, type, created_by, created_at, description FROM channels WHERE id = ?",
            [id],
            |row| Ok(Channel {
                id: row.get(0)?,
                name: row.get(1)?,
                channel_type: row.get(2)?,
                created_by: row.get(3)?,
                created_at: row.get(4)?,
                description: row.get(5)?,
            }),
        ).ok();
        Ok(ch)
    }

    pub fn delete_channel(&self, id: &str) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let n = conn.execute("DELETE FROM channels WHERE id = ?", [id])?;
        Ok(n > 0)
    }

    // ── Users ─────────────────────────────────────────────────────────────────

    pub fn get_users(&self) -> Result<Vec<User>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, type, status, last_seen FROM users ORDER BY name ASC"
        )?;
        let users: Vec<User> = stmt.query_map([], |row| {
            let status: String = row.get(3).unwrap_or_else(|_| "offline".into());
            let last_seen: Option<i64> = row.get(4)?;
            let online = is_online(last_seen);
            Ok(User {
                id: row.get(0)?,
                name: row.get(1)?,
                user_type: row.get(2).unwrap_or_else(|_| "agent".into()),
                online,
                status,
                last_seen,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(users)
    }

    pub fn upsert_heartbeat(&self, agent_id: &str, status: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        let ts = now_ms();
        conn.execute(
            "INSERT INTO users (id, name, status, last_seen) VALUES (?1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET status = ?2, last_seen = ?3",
            params![agent_id, status, ts],
        )?;
        Ok(())
    }

    // ── Projects ──────────────────────────────────────────────────────────────

    pub fn get_projects(&self) -> Result<Vec<Project>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, description, tags, assignee, status, created_at, updated_at FROM projects ORDER BY created_at DESC"
        )?;
        let projects: Vec<Project> = stmt.query_map([], |row| {
            let tags_raw: Option<String> = row.get(3)?;
            let tags: Vec<String> = tags_raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                tags,
                assignee: row.get(4)?,
                status: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(projects)
    }

    pub fn get_project(&self, id: &str) -> Result<Option<Project>> {
        let conn = self.0.lock().unwrap();
        let p = conn.query_row(
            "SELECT id, name, description, tags, assignee, status, created_at, updated_at FROM projects WHERE id = ?",
            [id],
            |row| {
                let tags_raw: Option<String> = row.get(3)?;
                let tags: Vec<String> = tags_raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    tags,
                    assignee: row.get(4)?,
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            },
        ).ok();
        Ok(p)
    }

    pub fn insert_project(&self, id: &str, name: &str, description: Option<&str>, tags: &[String], assignee: Option<&str>, status: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        let tags_json = serde_json::to_string(tags).unwrap();
        let ts = now_ms();
        conn.execute(
            "INSERT INTO projects (id, name, description, tags, assignee, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![id, name, description, tags_json, assignee, status, ts, ts],
        )?;
        Ok(())
    }

    pub fn update_project(&self, id: &str, name: Option<&str>, description: Option<&str>, status: Option<&str>, assignee: Option<&str>) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let ts = now_ms();
        // Build dynamic update — only update fields that are Some
        let mut sets = vec!["updated_at = ?1".to_string()];
        let mut idx = 2usize;
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(ts)];

        if let Some(v) = name {
            sets.push(format!("name = ?{idx}"));
            values.push(Box::new(v.to_string()));
            idx += 1;
        }
        if let Some(v) = description {
            sets.push(format!("description = ?{idx}"));
            values.push(Box::new(v.to_string()));
            idx += 1;
        }
        if let Some(v) = status {
            sets.push(format!("status = ?{idx}"));
            values.push(Box::new(v.to_string()));
            idx += 1;
        }
        if let Some(v) = assignee {
            sets.push(format!("assignee = ?{idx}"));
            values.push(Box::new(v.to_string()));
            idx += 1;
        }

        values.push(Box::new(id.to_string()));
        let sql = format!("UPDATE projects SET {} WHERE id = ?{idx}", sets.join(", "));
        let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        let n = conn.execute(&sql, refs.as_slice())?;
        Ok(n > 0)
    }

    pub fn delete_project(&self, id: &str) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let n = conn.execute("DELETE FROM projects WHERE id = ?", [id])?;
        Ok(n > 0)
    }

    // ── Full-Text Search ──────────────────────────────────────────────────────

    /// Search messages using FTS5. Returns up to `limit` results ordered by rank.
    /// Optionally filter by channel.
    pub fn search_messages(&self, query: &str, channel: Option<&str>, limit: i64) -> Result<Vec<Message>> {
        let conn = self.0.lock().unwrap();
        let msgs: Vec<Message> = if let Some(ch) = channel {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.ts, m.from_agent, m.text, m.channel, m.thread_id, m.mentions, m.slash_result
                 FROM messages m
                 JOIN messages_fts fts ON fts.rowid = m.id
                 WHERE messages_fts MATCH ?1 AND m.channel = ?2
                 ORDER BY fts.rank LIMIT ?3"
            )?;
            let x: Vec<Message> = stmt.query_map(params![query, ch, limit], |row| self.row_to_message(&conn, row))?
                .filter_map(|r| r.ok()).collect();
            x
        } else {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.ts, m.from_agent, m.text, m.channel, m.thread_id, m.mentions, m.slash_result
                 FROM messages m
                 JOIN messages_fts fts ON fts.rowid = m.id
                 WHERE messages_fts MATCH ?1
                 ORDER BY fts.rank LIMIT ?2"
            )?;
            let x: Vec<Message> = stmt.query_map(params![query, limit], |row| self.row_to_message(&conn, row))?
                .filter_map(|r| r.ok()).collect();
            x
        };
        Ok(msgs)
    }

    // ── Attachments ───────────────────────────────────────────────────────────

    pub fn insert_attachment(&self, message_id: i64, filename: &str, mime_type: &str, content: &[u8]) -> Result<i64> {
        let conn = self.0.lock().unwrap();
        let size = content.len() as i64;
        conn.execute(
            "INSERT INTO attachments (message_id, filename, mime_type, size, content) VALUES (?, ?, ?, ?, ?)",
            params![message_id, filename, mime_type, size, content],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_attachments(&self, message_id: i64) -> Result<Vec<Attachment>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, message_id, filename, mime_type, size, created_at FROM attachments WHERE message_id = ? ORDER BY created_at ASC"
        )?;
        let attachments: Vec<Attachment> = stmt.query_map([message_id], |row| {
            Ok(Attachment {
                id: row.get(0)?,
                message_id: row.get(1)?,
                filename: row.get(2)?,
                mime_type: row.get(3)?,
                size: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(attachments)
    }

    pub fn get_attachment_content(&self, attachment_id: i64) -> Result<Option<(String, String, Vec<u8>)>> {
        let conn = self.0.lock().unwrap();
        let result = conn.query_row(
            "SELECT filename, mime_type, content FROM attachments WHERE id = ?",
            [attachment_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Vec<u8>>(2)?)),
        ).ok();
        Ok(result)
    }

    // ── Pins ──────────────────────────────────────────────────────────────────

    pub fn pin_message(&self, channel_id: &str, message_id: i64, pinned_by: &str) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let n = conn.execute(
            "INSERT OR IGNORE INTO pins (channel_id, message_id, pinned_by) VALUES (?, ?, ?)",
            params![channel_id, message_id, pinned_by],
        )?;
        Ok(n > 0)
    }

    pub fn unpin_message(&self, channel_id: &str, message_id: i64) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM pins WHERE channel_id = ? AND message_id = ?",
            params![channel_id, message_id],
        )?;
        Ok(n > 0)
    }

    pub fn get_pins(&self, channel_id: &str) -> Result<Vec<Message>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.ts, m.from_agent, m.text, m.channel, m.thread_id, m.mentions, m.slash_result
             FROM messages m
             JOIN pins p ON p.message_id = m.id
             WHERE p.channel_id = ?
             ORDER BY p.pinned_at DESC"
        )?;
        let msgs: Vec<Message> = stmt.query_map([channel_id], |row| self.row_to_message(&conn, row))?
            .filter_map(|r| r.ok()).collect();
        Ok(msgs)
    }

    // ── DMs ───────────────────────────────────────────────────────────────────

    /// Get or create a DM channel between two agents. ID is canonical: dm-{a}-{b} (sorted).
    pub fn get_or_create_dm(&self, agent_a: &str, agent_b: &str) -> Result<Channel> {
        let conn = self.0.lock().unwrap();
        // Canonical ID: sort the two agents alphabetically
        let (first, second) = if agent_a <= agent_b { (agent_a, agent_b) } else { (agent_b, agent_a) };
        let dm_id = format!("dm-{}-{}", first, second);
        let dm_name = format!("{} & {}", first, second);

        // Upsert the DM channel
        conn.execute(
            "INSERT OR IGNORE INTO channels (id, name, type, created_by, description) VALUES (?, ?, 'dm', ?, 'Direct message')",
            params![dm_id, dm_name, agent_a],
        )?;

        let ch = conn.query_row(
            "SELECT id, name, type, created_by, created_at, description FROM channels WHERE id = ?",
            [&dm_id],
            |row| Ok(Channel {
                id: row.get(0)?,
                name: row.get(1)?,
                channel_type: row.get(2)?,
                created_by: row.get(3)?,
                created_at: row.get(4)?,
                description: row.get(5)?,
            }),
        )?;
        Ok(ch)
    }

    /// List all DM channels that involve a given agent (either side).
    pub fn get_dms_for_agent(&self, agent: &str) -> Result<Vec<Channel>> {
        let conn = self.0.lock().unwrap();
        let pattern = format!("%{}%", agent);
        let mut stmt = conn.prepare(
            "SELECT id, name, type, created_by, created_at, description FROM channels
             WHERE type = 'dm' AND id LIKE ?
             ORDER BY created_at DESC"
        )?;
        let channels: Vec<Channel> = stmt.query_map([pattern], |row| {
            Ok(Channel {
                id: row.get(0)?,
                name: row.get(1)?,
                channel_type: row.get(2)?,
                created_by: row.get(3)?,
                created_at: row.get(4)?,
                description: row.get(5)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(channels)
    }

    // ── Project Files ─────────────────────────────────────────────────────────

    pub fn get_project_files(&self, project_id: &str) -> Result<Vec<FileInfo>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, filename, size, encoding, created_at FROM project_files WHERE project_id = ? ORDER BY filename ASC"
        )?;
        let files: Vec<FileInfo> = stmt.query_map([project_id], |row| {
            Ok(FileInfo {
                id: row.get(0)?,
                filename: row.get(1)?,
                size: row.get(2)?,
                encoding: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(files)
    }

    pub fn get_project_file_content(&self, project_id: &str, filename: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.0.lock().unwrap();
        let content = conn.query_row(
            "SELECT content FROM project_files WHERE project_id = ? AND filename = ?",
            params![project_id, filename],
            |row| row.get::<_, Vec<u8>>(0),
        ).ok();
        Ok(content)
    }

    pub fn upsert_project_file(&self, project_id: &str, filename: &str, content: &[u8], encoding: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        let size = content.len() as i64;
        let ts = now_ms();
        conn.execute(
            "INSERT INTO project_files (project_id, filename, content, encoding, size, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id, filename) DO UPDATE SET content = ?3, encoding = ?4, size = ?5",
            params![project_id, filename, content, encoding, size, ts],
        )?;
        Ok(())
    }

    // ── Read cursors / unread counts ─────────────────────────────────────────

    /// Mark a channel as read up to `ts` (milliseconds epoch) for a given user.
    pub fn upsert_read_cursor(&self, user_id: &str, channel_id: &str, ts: i64) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO read_cursors (user_id, channel_id, last_read, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, channel_id) DO UPDATE SET
               last_read  = MAX(excluded.last_read, last_read),
               updated_at = excluded.updated_at",
            params![user_id, channel_id, ts, now_ms()],
        )?;
        Ok(())
    }

    /// Returns HashMap<channel_id, unread_count> for the given user.
    pub fn get_unread_counts(&self, user_id: &str) -> Result<HashMap<String, i64>> {
        let conn = self.0.lock().unwrap();
        // For channels where no cursor exists, count is 0 (user hasn't visited).
        // For channels with a cursor, count messages newer than last_read.
        let mut stmt = conn.prepare(
            "SELECT c.id,
                    COALESCE((
                        SELECT COUNT(*) FROM messages m
                        WHERE m.channel = c.id
                          AND m.ts > COALESCE(
                              (SELECT last_read FROM read_cursors
                               WHERE user_id = ?1 AND channel_id = c.id), 0)
                    ), 0) AS cnt
             FROM channels c"
        )?;
        let mut counts = HashMap::new();
        let rows = stmt.query_map(params![user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (ch, cnt) = row?;
            counts.insert(ch, cnt);
        }
        Ok(counts)
    }
    // ── Scheduled messages ────────────────────────────────────────────────────

    pub fn insert_scheduled_message(
        &self,
        id: &str,
        channel_id: &str,
        sender: &str,
        text: &str,
        deliver_at: i64,
    ) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO scheduled_messages (id, channel_id, sender, text, deliver_at, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![id, channel_id, sender, text, deliver_at, now_ms()],
        )?;
        Ok(())
    }

    pub fn get_pending_scheduled_messages(&self, now: i64) -> Result<Vec<(String, String, String, String, i64)>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, channel_id, sender, text, deliver_at
             FROM scheduled_messages
             WHERE delivered = 0 AND deliver_at <= ?"
        )?;
        let rows = stmt.query_map([now], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    pub fn mark_scheduled_delivered(&self, id: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE scheduled_messages SET delivered = 1 WHERE id = ?",
            [id],
        )?;
        Ok(())
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn is_online(last_seen: Option<i64>) -> bool {
    match last_seen {
        Some(ts) => (now_ms() - ts) < 120_000, // online if seen within 2 min
        None => false,
    }
}
