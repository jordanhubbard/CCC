//! Shared ACC domain types used by both `acc-server` and `acc-client`.
//!
//! Types are organized by resource. Each module mirrors the corresponding
//! `/api/<resource>` route group on the server. Keep definitions minimal
//! and wire-focused — server-internal representations belong in `acc-server`.

pub mod agent;
pub mod bus;
pub mod error;
pub mod memory;
pub mod project;
pub mod queue;
pub mod task;

pub use agent::{Agent, AgentOnlineStatus};
pub use bus::{BusMsg, BusSendRequest};
pub use error::ApiError;
pub use memory::{MemoryHit, MemorySearchRequest, MemoryStoreRequest};
pub use project::{CreateProjectRequest, Project, ProjectStatus};
pub use queue::{
    ClaimItemRequest, CommentItemRequest, CompleteItemRequest, FailItemRequest, HeartbeatRequest,
    KeepaliveRequest, QueueItem,
};
pub use task::{
    ClaimRequest, CompleteRequest, CreateTaskRequest, ReviewResult, ReviewResultRequest, Task,
    TaskStatus, TaskType, UnclaimRequest,
};
