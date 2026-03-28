//! agentOS SDK — Rust bindings
//! 
//! This is the primary interface for writing agentOS-native agents.
//! An agent is anything that implements the `Agent` trait.

pub mod identity;
pub mod objects;
pub mod messages;
pub mod tasks;
pub mod inference;
pub mod capabilities;
pub mod introspect;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ─── Agent Identity ────────────────────────────────────────────────────────────

/// Cryptographic agent identity — the public key IS the ID.
/// All messages from this agent are signed with the corresponding private key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub [u8; 32]); // ED25519 public key bytes

impl AgentId {
    pub fn display(&self) -> String {
        hex::encode(&self.0[..8]) // first 8 bytes for human display
    }
}

// ─── Object Identity ───────────────────────────────────────────────────────────

/// UUID-based object identifier for ObjectVault items.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectId(pub Uuid);

impl ObjectId {
    pub fn new() -> Self {
        ObjectId(Uuid::new_v4())
    }
}

// ─── Task Identity ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub Uuid);

impl TaskId {
    pub fn new() -> Self {
        TaskId(Uuid::new_v4())
    }
}

// ─── Core Message Type ─────────────────────────────────────────────────────────

/// A typed, signed message between agents.
/// All agent-to-agent communication flows through this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Unique message ID
    pub id: Uuid,
    /// Sender's agent ID
    pub from: AgentId,
    /// Recipient (None = broadcast on topic)
    pub to: Option<AgentId>,
    /// Topic for pub/sub routing (None = direct)
    pub topic: Option<String>,
    /// Schema type of the payload (e.g. "agentOS::tasks::TaskAssignment")
    pub schema: String,
    /// JSON-encoded payload
    pub payload: serde_json::Value,
    /// Unix timestamp (microseconds)
    pub timestamp_us: u64,
    /// TTL in microseconds (0 = no expiry)
    pub ttl_us: u64,
    /// ED25519 signature over (id + from + schema + payload + timestamp_us)
    pub signature: Vec<u8>,
}

// ─── Task Types ────────────────────────────────────────────────────────────────

/// A unit of work assigned by TaskForest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    /// Human-readable task type (matched against agent's declared capabilities)
    pub task_type: String,
    /// JSON input payload (schema defined by task_type)
    pub input: serde_json::Value,
    /// Priority (0 = lowest, 255 = highest)
    pub priority: u8,
    /// Optional deadline (Unix timestamp, microseconds)
    pub deadline_us: Option<u64>,
    /// Parent task (if this is a subtask)
    pub parent: Option<TaskId>,
}

/// Result emitted by an agent completing a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: TaskId,
    pub success: bool,
    /// JSON output payload
    pub output: serde_json::Value,
    /// Error message if !success
    pub error: Option<String>,
    /// IDs of any objects created/modified
    pub objects_affected: Vec<ObjectId>,
}

// ─── The Agent Trait ───────────────────────────────────────────────────────────

/// Everything that runs in agentOS implements this trait.
/// 
/// The runtime calls these methods on the agent. The agent
/// must NOT block — use async or delegate to subtasks.
pub trait Agent: Send + Sync {
    /// Called once at agent startup.
    /// Register capabilities, initialize state, connect to services.
    fn init(&mut self, ctx: &mut AgentContext) -> Result<(), AgentError>;

    /// Called for each incoming message.
    /// Route to appropriate handler based on message.schema.
    fn handle_message(&mut self, ctx: &mut AgentContext, msg: Message) -> Result<(), AgentError>;

    /// Called when TaskForest assigns a task.
    /// Return the result (or Err to trigger retry logic).
    fn handle_task(&mut self, ctx: &mut AgentContext, task: Task) -> Result<TaskResult, AgentError>;

    /// Called before graceful shutdown.
    /// Flush state, release capabilities, say goodbye.
    fn shutdown(&mut self, ctx: &mut AgentContext) -> Result<(), AgentError>;

    /// Declare what task types this agent can handle.
    /// Used by TaskForest for routing.
    fn capabilities(&self) -> Vec<String>;
}

// ─── Agent Context ─────────────────────────────────────────────────────────────

/// Provided by the runtime to every agent method call.
/// The agent's window into agentOS services.
pub struct AgentContext {
    /// This agent's identity
    pub id: AgentId,
    /// Capability handle to NameServer
    pub nameserver: Box<dyn nameserver::NameServerClient>,
    /// Capability handle to ObjectVault
    pub vault: Box<dyn objects::ObjectVaultClient>,
    /// Capability handle to TaskForest
    pub taskforest: Box<dyn tasks::TaskForestClient>,
    /// Capability handle to ModelBus
    pub modelbus: Box<dyn inference::ModelBusClient>,
    /// Send a message to another agent
    pub send: Box<dyn Fn(Message) -> Result<(), AgentError>>,
}

// ─── Error Type ────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("capability denied: {0}")]
    CapabilityDenied(String),

    #[error("object not found: {0}")]
    ObjectNotFound(String),

    #[error("task failed: {0}")]
    TaskFailed(String),

    #[error("inference error: {0}")]
    InferenceError(String),

    #[error("message decode error: {0}")]
    MessageDecodeError(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("internal: {0}")]
    Internal(String),
}

// Stub module references so the crate compiles
mod nameserver {
    pub trait NameServerClient: Send + Sync {
        fn lookup(&self, name: &str) -> Option<crate::AgentId>;
        fn register(&mut self, name: &str, id: &crate::AgentId) -> Result<(), crate::AgentError>;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_id_is_unique() {
        let a = ObjectId::new();
        let b = ObjectId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn task_id_is_unique() {
        let a = TaskId::new();
        let b = TaskId::new();
        assert_ne!(a, b);
    }
}
