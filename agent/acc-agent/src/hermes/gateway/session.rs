use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use acc_client::Client;

use super::super::conversation::ConversationHistory;

const MAX_HISTORY_MESSAGES: usize = 60;

pub struct SessionStore {
    base_dir: PathBuf,
    cache: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    hub: Option<Client>,
    agent_name: String,
    workspace: String,
}

impl SessionStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        let base_dir = base_dir.into();
        std::fs::create_dir_all(&base_dir).ok();
        Self {
            base_dir,
            cache: Arc::new(Mutex::new(HashMap::new())),
            hub: None,
            agent_name: String::new(),
            workspace: "default".to_string(),
        }
    }

    pub fn with_hub(mut self, client: Client, agent_name: String, workspace: String) -> Self {
        self.hub = Some(client);
        self.agent_name = agent_name;
        self.workspace = workspace;
        self
    }

    fn key_to_path(&self, key: &str) -> PathBuf {
        let safe: String = key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.base_dir.join(format!("{safe}.json"))
    }

    pub async fn load_history(&self, key: &str) -> ConversationHistory {
        // Try hub first (authoritative, cross-agent consistent).
        if let Some(client) = &self.hub {
            match client.sessions().get(key).await {
                Ok(messages) => {
                    let mut cache = self.cache.lock().await;
                    cache.insert(key.to_string(), messages.clone());
                    return ConversationHistory::from_turns(&messages);
                }
                Err(e) => {
                    tracing::warn!(
                        "[session] hub load failed for {key}: {e} — falling back to file"
                    );
                }
            }
        }
        // File fallback (local cache / hub unavailable).
        let mut cache = self.cache.lock().await;
        if let Some(msgs) = cache.get(key) {
            return ConversationHistory::from_turns(msgs);
        }
        let path = self.key_to_path(key);
        let messages: Vec<Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        cache.insert(key.to_string(), messages.clone());
        ConversationHistory::from_turns(&messages)
    }

    pub async fn save_history(&self, key: &str, history: &ConversationHistory) {
        let mut messages = history.messages.clone();
        if messages.len() > MAX_HISTORY_MESSAGES {
            messages = messages.split_off(messages.len() - MAX_HISTORY_MESSAGES);
        }
        // Write to hub (authoritative).
        if let Some(client) = &self.hub {
            if let Err(e) = client
                .sessions()
                .put(key, &self.agent_name, &self.workspace, &messages)
                .await
            {
                tracing::warn!("[session] hub save failed for {key}: {e}");
            }
        }
        // Write to local file (cache / fallback).
        let path = self.key_to_path(key);
        {
            let mut cache = self.cache.lock().await;
            cache.insert(key.to_string(), messages.clone());
        }
        if let Ok(json) = serde_json::to_string_pretty(&messages) {
            let tmp = path.with_extension("tmp");
            let _ = std::fs::write(&tmp, &json);
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub async fn clear(&self, key: &str) {
        if let Some(client) = &self.hub {
            let _ = client.sessions().delete(key).await;
        }
        let path = self.key_to_path(key);
        self.cache.lock().await.remove(key);
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn round_trips_history() {
        let dir = tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let key = "test:session:1";
        let mut h = store.load_history(key).await;
        assert!(h.messages.is_empty());
        h.push_user_text("hello");
        store.save_history(key, &h).await;
        let h2 = store.load_history(key).await;
        assert_eq!(h2.messages.len(), 1);
        assert_eq!(h2.messages[0]["role"], "user");
    }

    #[tokio::test]
    async fn clear_removes_session() {
        let dir = tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        let key = "test:clear:1";
        let mut h = store.load_history(key).await;
        h.push_user_text("hi");
        store.save_history(key, &h).await;
        store.clear(key).await;
        let h2 = store.load_history(key).await;
        assert!(h2.messages.is_empty());
    }
}
