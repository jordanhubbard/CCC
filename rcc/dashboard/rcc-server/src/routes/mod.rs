pub mod health;
pub mod queue;
pub mod agents;
pub mod secrets;
pub mod bus;
pub mod projects;
pub mod brain;
pub mod services;
pub mod lessons;
pub mod exec;
pub mod geek;
pub mod ui;
pub mod setup;
pub mod providers;
pub mod acp;
pub mod agentos;
pub mod memory;
pub mod issues;
pub mod fs;
pub mod supervisor;
pub mod conversations;
pub mod metrics;

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
    (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"})))
}
