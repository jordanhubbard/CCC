/// /api/metrics/:name — lightweight time-series store for sparklines

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/metrics/{name}", post(post_metric).get(get_metric))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricPoint {
    pub ts: String,
    pub value: f64,
}

#[derive(Deserialize)]
struct PostBody {
    value: f64,
}

#[derive(Deserialize)]
struct GetQuery {
    n: Option<usize>,
}

const MAX_POINTS: usize = 1000;

async fn post_metric(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<PostBody>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }

    let ts = chrono::Utc::now().to_rfc3339();
    let point = MetricPoint { ts, value: body.value };

    {
        let mut metrics = state.metrics.write().await;
        let series = metrics.entry(name.clone()).or_insert_with(Vec::new);
        series.push(point);
        if series.len() > MAX_POINTS {
            let drain_to = series.len() - MAX_POINTS;
            series.drain(0..drain_to);
        }
    }

    crate::state::flush_metrics(&state).await;

    (StatusCode::OK, Json(json!({"ok": true, "metric": name}))).into_response()
}

async fn get_metric(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<GetQuery>,
) -> impl IntoResponse {
    let n = params.n.unwrap_or(50).min(MAX_POINTS);
    let metrics = state.metrics.read().await;
    let points: Vec<&MetricPoint> = metrics
        .get(&name)
        .map(|series| {
            let skip = series.len().saturating_sub(n);
            series[skip..].iter().collect()
        })
        .unwrap_or_default();

    Json(json!({
        "metric": name,
        "n": points.len(),
        "points": points,
    }))
}
