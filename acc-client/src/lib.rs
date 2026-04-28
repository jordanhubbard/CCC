//! HTTP client for the ACC fleet API.
//!
//! Use [`Client::new`] with an explicit base URL and token, or
//! [`Client::from_env`] to pick up credentials the same way `acc-cli` does
//! (flag → env → `~/.acc/.env`).
//!
//! ```no_run
//! # async fn demo() -> Result<(), acc_client::Error> {
//! use acc_client::model::TaskStatus;
//! let client = acc_client::Client::from_env()?;
//! let tasks = client.tasks().list().status(TaskStatus::Open).send().await?;
//! # let _ = tasks;
//! # Ok(()) }
//! ```

pub mod agents;
pub mod auth;
pub mod bus;
pub mod error;
pub mod items;
pub mod llm_config;
pub mod memory;
pub mod projects;
pub mod queue;
pub mod sessions;
pub mod tasks;

pub use acc_model as model;
pub use error::{Error, Result};

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

const DEFAULT_BASE_URL: &str = "http://localhost:8789";

/// An authenticated client for the ACC API.
///
/// The client is cheap to clone — the underlying `reqwest::Client` is
/// reference-counted and safe to share across tasks.
#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    /// Construct a client with an explicit base URL and bearer token.
    pub fn new(base_url: impl Into<String>, token: &str) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let auth =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::InvalidToken)?;
        headers.insert(AUTHORIZATION, auth);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(Error::Http)?;
        Ok(Self {
            base: base_url.into().trim_end_matches('/').to_string(),
            http,
        })
    }

    /// Resolve credentials from the environment the same way `acc-cli` does:
    ///   1. `ACC_TOKEN` env var
    ///   2. `~/.acc/.env` (keys `ACC_TOKEN` then `ACC_AGENT_TOKEN`)
    ///
    /// Base URL comes from `ACC_HUB_URL` or the default `http://localhost:8789`.
    pub fn from_env() -> Result<Self> {
        let base = std::env::var("ACC_HUB_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let token = auth::resolve_token(None)?;
        Self::new(base, &token)
    }

    /// Base URL this client points at (no trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub(crate) fn url(&self, path: &str) -> String {
        debug_assert!(path.starts_with('/'), "paths must start with /");
        format!("{}{}", self.base, path)
    }

    /// Entry point for task operations.
    pub fn tasks(&self) -> tasks::TasksApi<'_> {
        tasks::TasksApi { client: self }
    }

    /// Low-level request helper for endpoints this crate hasn't typed yet.
    ///
    /// Handles bearer auth, JSON request/response, and the typed error
    /// mapping (409 → Conflict, 423 → Locked, etc.). Callers pass the HTTP
    /// method as a string (`"GET" | "POST" | "PUT" | "DELETE" | ...`) and
    /// parse the returned `serde_json::Value` as needed.
    ///
    /// Prefer a typed method where one exists — this is an escape hatch
    /// for bespoke endpoints (custom dispatch, soul data, etc.) not worth
    /// modeling upstream.
    pub async fn request_json(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        use reqwest::Method;
        let m = Method::from_bytes(method.as_bytes()).map_err(|_| Error::Api {
            status: 0,
            body: acc_model::ApiError {
                error: "invalid_method".into(),
                message: Some(format!("unknown HTTP method: {method}")),
                extra: Default::default(),
            },
        })?;
        let mut req = self.http.request(m, self.url(path));
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await?;
        if !(200..300).contains(&status) {
            return Err(Error::from_response(status, &bytes));
        }
        if bytes.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Entry point for queue list/get.
    pub fn queue(&self) -> queue::QueueApi<'_> {
        queue::QueueApi { client: self }
    }

    /// Entry point for per-item mutations and heartbeat.
    pub fn items(&self) -> items::ItemsApi<'_> {
        items::ItemsApi { client: self }
    }

    /// Entry point for project operations.
    pub fn projects(&self) -> projects::ProjectsApi<'_> {
        projects::ProjectsApi { client: self }
    }

    /// Entry point for agent registry reads.
    pub fn agents(&self) -> agents::AgentsApi<'_> {
        agents::AgentsApi { client: self }
    }

    /// Entry point for bus send/messages/SSE stream.
    pub fn bus(&self) -> bus::BusApi<'_> {
        bus::BusApi { client: self }
    }

    /// Entry point for memory search/store.
    pub fn memory(&self) -> memory::MemoryApi<'_> {
        memory::MemoryApi { client: self }
    }

    /// Entry point for hub-backed gateway conversation sessions.
    pub fn sessions(&self) -> sessions::SessionsApi<'_> {
        sessions::SessionsApi { client: self }
    }
}
