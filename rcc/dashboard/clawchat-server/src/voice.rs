//! Voice channel in-memory state + Axum route handlers.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use std::convert::Infallible;

use axum::{
    extract::{Extension, Path},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

use crate::SharedState;

// ── In-memory voice channel state ─────────────────────────────────────────────

#[derive(Clone)]
pub struct VoiceParticipantEntry {
    pub agent_id: String,
    pub session_id: String,
    pub joined_at: i64,
}

#[derive(Clone)]
pub struct VoiceChannelEntry {
    pub id: String,
    pub name: String,
    pub participants: Vec<VoiceParticipantEntry>,
}

pub struct VoiceSession {
    pub channel_id: String,
    pub agent_id: String,
    pub tx: mpsc::Sender<String>,
}

pub struct VoiceState {
    pub channels: Mutex<Vec<VoiceChannelEntry>>,
    pub sessions: Mutex<HashMap<String, VoiceSession>>,
    pub agent_to_session: Mutex<HashMap<String, String>>,
}

impl VoiceState {
    pub fn new() -> Self {
        VoiceState {
            channels: Mutex::new(vec![
                VoiceChannelEntry { id: "voice-general".into(), name: "#voice-general".into(), participants: vec![] },
                VoiceChannelEntry { id: "voice-standup".into(), name: "#voice-standup".into(), participants: vec![] },
                VoiceChannelEntry { id: "voice-gpu-ops".into(), name: "#voice-gpu-ops".into(), participants: vec![] },
            ]),
            sessions: Mutex::new(HashMap::new()),
            agent_to_session: Mutex::new(HashMap::new()),
        }
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

// ── Error helper ──────────────────────────────────────────────────────────────

pub(crate) struct VoiceError(String);

impl IntoResponse for VoiceError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": self.0}))).into_response()
    }
}

type VR<T> = Result<T, VoiceError>;

// ── GET /api/voice/channels ───────────────────────────────────────────────────

pub async fn list_voice_channels(
    Extension(state): Extension<SharedState>,
) -> Json<serde_json::Value> {
    let channels = state.voice.channels.lock().unwrap();
    let out: Vec<serde_json::Value> = channels.iter().map(|ch| {
        json!({
            "id": ch.id,
            "name": ch.name,
            "participants": ch.participants.iter().map(|p| json!({
                "agent_id": p.agent_id,
                "joined_at": p.joined_at,
            })).collect::<Vec<_>>()
        })
    }).collect();
    Json(json!(out))
}

// ── POST /api/voice/join ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct JoinBody {
    pub channel_id: String,
    pub agent_id: String,
}

pub async fn voice_join(
    Extension(state): Extension<SharedState>,
    Json(body): Json<JoinBody>,
) -> VR<Json<serde_json::Value>> {
    {
        let channels = state.voice.channels.lock().unwrap();
        if !channels.iter().any(|ch| ch.id == body.channel_id) {
            return Err(VoiceError(format!("Channel '{}' not found", body.channel_id)));
        }
    }

    // Remove any existing session for this agent
    let old_session_id = state.voice.agent_to_session.lock().unwrap().get(&body.agent_id).cloned();
    if let Some(old_sid) = old_session_id {
        do_leave(&state, &old_sid);
    }

    let session_id = format!("vs-{}-{}", body.agent_id, now_secs());
    let (tx, _rx) = mpsc::channel::<String>(64); // rx replaced when SSE stream connects

    state.voice.sessions.lock().unwrap().insert(session_id.clone(), VoiceSession {
        channel_id: body.channel_id.clone(),
        agent_id: body.agent_id.clone(),
        tx,
    });
    state.voice.agent_to_session.lock().unwrap().insert(body.agent_id.clone(), session_id.clone());

    state.voice.channels.lock().unwrap()
        .iter_mut()
        .find(|c| c.id == body.channel_id)
        .map(|ch| {
            ch.participants.retain(|p| p.agent_id != body.agent_id);
            ch.participants.push(VoiceParticipantEntry {
                agent_id: body.agent_id.clone(),
                session_id: session_id.clone(),
                joined_at: now_secs(),
            });
        });

    Ok(Json(json!({
        "session_id": session_id,
        "ice_servers": [{"urls": ["stun:stun.l.google.com:19302"]}]
    })))
}

// ── POST /api/voice/leave ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LeaveBody {
    pub session_id: String,
}

pub async fn voice_leave(
    Extension(state): Extension<SharedState>,
    Json(body): Json<LeaveBody>,
) -> Json<serde_json::Value> {
    do_leave(&state, &body.session_id);
    Json(json!({"ok": true}))
}

pub fn do_leave(state: &SharedState, session_id: &str) {
    let session = state.voice.sessions.lock().unwrap().remove(session_id);
    if let Some(sess) = session {
        state.voice.agent_to_session.lock().unwrap().remove(&sess.agent_id);
        state.voice.channels.lock().unwrap()
            .iter_mut()
            .find(|c| c.id == sess.channel_id)
            .map(|ch| ch.participants.retain(|p| p.session_id != session_id));
    }
}

// ── POST /api/voice/signal ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SignalBody {
    pub session_id: String,
    pub to_agent: String,
    pub signal_type: String,
    pub payload: serde_json::Value,
}

pub async fn voice_signal_post(
    Extension(state): Extension<SharedState>,
    Json(body): Json<SignalBody>,
) -> Json<serde_json::Value> {
    let target_session_id = state.voice.agent_to_session.lock().unwrap()
        .get(&body.to_agent).cloned();

    if let Some(target_sid) = target_session_id {
        let tx = state.voice.sessions.lock().unwrap()
            .get(&target_sid)
            .map(|s| s.tx.clone());
        if let Some(tx) = tx {
            let msg = serde_json::to_string(&json!({
                "from_session": body.session_id,
                "signal_type": body.signal_type,
                "payload": body.payload,
            })).unwrap_or_default();
            let _ = tx.try_send(msg);
        }
    }

    Json(json!({"ok": true}))
}

// ── GET /api/voice/signal/{session_id} (SSE) ──────────────────────────────────

pub async fn voice_signal_sse(
    Extension(state): Extension<SharedState>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let (tx, rx) = mpsc::channel::<String>(64);

    // Replace the session's tx with the live one
    if let Some(sess) = state.voice.sessions.lock().unwrap().get_mut(&session_id) {
        sess.tx = tx;
    }

    let stream = ReceiverStream::new(rx).map(|msg| {
        Ok::<Event, Infallible>(Event::default().data(msg))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
