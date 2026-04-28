pub mod acp;
pub mod agents;
pub mod auth;
pub mod blobs;
pub mod brain;
pub mod bus;
pub mod chat_sessions;
pub mod conversations;
pub mod exec;
pub mod fs;
pub mod geek;
pub mod github;
pub mod health;
pub mod issues;
pub mod lessons;
pub mod logs;
pub mod memory;
pub mod metrics;
pub mod models;
pub mod panes;
pub mod projects;
pub mod providers;
pub mod queue;
pub mod requests;
pub mod secrets;
pub mod services;
pub mod setup;
pub mod soul;
pub mod supervisor;
pub mod tasks;
pub mod ui;
pub mod vault;
pub mod watchdog;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde_json::json;

#[allow(dead_code)]
pub fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({"error": "Not found"})))
}

#[allow(dead_code)]
pub fn unauthorized() -> impl IntoResponse {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "Unauthorized"})),
    )
}
