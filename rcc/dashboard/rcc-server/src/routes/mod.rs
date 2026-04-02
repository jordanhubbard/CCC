pub mod health;
pub mod queue;
pub mod agents;
pub mod secrets;
pub mod bus;

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
