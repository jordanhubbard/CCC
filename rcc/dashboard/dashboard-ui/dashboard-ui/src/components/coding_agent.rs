/// CodingAgent — web UI front-end for charmbracelet/crush sessions
///
/// Connects to crush-server (sparky:8793 or do-host1:8794) which wraps the crush CLI.
/// The server URL is derived at runtime from window.location.hostname — no hardcoded IPs.
/// Features:
///   - Session list with refresh
///   - Session detail / message history
///   - Prompt input → SSE streaming output
///   - Start new session / continue existing
///   - Delete session

use leptos::*;
use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use super::diff_view::{DiffView, looks_like_diff, extract_diff};

// ── Config ─────────────────────────────────────────────────────────────────

/// Derive crush-server base URL at runtime from window.location.hostname.
/// - If served from sparky (100.87.229.125 or sparky.*), use port 8793.
/// - If served from do-host1 (146.190.134.110), use port 8794 (fallback).
/// - Falls back to hardcoded sparky IP if window is unavailable.
fn crush_base_url() -> String {
    let hostname = web_sys::window()
        .and_then(|w| w.location().hostname().ok())
        .unwrap_or_default();
    let port = if hostname.contains("146.190.134.110") || hostname == "do-host1" {
        8794 // do-host1 fallback crush-server port
    } else {
        8793 // sparky (default)
    };
    format!("http://{}:{}", hostname, port)
}

fn crush_url(path: &str) -> String {
    format!("{}{}", crush_base_url(), path)
}

// ── Types ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CrushSession {
    pub id: String,
    pub title: Option<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    pub message_count: Option<u32>,
    pub model: Option<String>,
    pub cwd: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct CrushMessage {
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub created: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct CrushSessionDetail {
    pub id: String,
    pub title: Option<String>,
    pub messages: Option<Vec<CrushMessage>>,
    pub model: Option<String>,
    pub cwd: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct CrushProject {
    pub path: String,
    pub name: Option<String>,
    pub session_count: Option<u32>,
}

// ── Component ──────────────────────────────────────────────────────────────

#[component]
pub fn CodingAgent() -> impl IntoView {
    let sessions = create_rw_signal::<Vec<CrushSession>>(vec![]);
    let selected_session = create_rw_signal::<Option<String>>(None);
    let session_detail = create_rw_signal::<Option<CrushSessionDetail>>(None);
    let prompt = create_rw_signal::<String>(String::new());
    let output = create_rw_signal::<String>(String::new());
    let running = create_rw_signal::<bool>(false);
    let error = create_rw_signal::<Option<String>>(None);
    let _refresh_tick = create_rw_signal::<u32>(0);
    let cwd = create_rw_signal::<String>(String::new());
    let model = create_rw_signal::<String>(String::new());
    let show_diff = create_rw_signal::<bool>(false);
    // Provider tracking: "crush" | "claude-code" | "" (unknown/idle)
    let active_provider = create_rw_signal::<String>(String::new());

    // Load sessions on mount + refresh
    let fetch_sessions = {
        let sessions = sessions.clone();
        let error = error.clone();
        move || {
            let sessions = sessions.clone();
            let error = error.clone();
            spawn_local(async move {
                match gloo_net::http::Request::get(&crush_url("/sessions"))
                    .send()
                    .await
                {
                    Ok(resp) => {
                        if let Ok(list) = resp.json::<Vec<CrushSession>>().await {
                            sessions.set(list);
                            error.set(None);
                        }
                    }
                    Err(e) => {
                        error.set(Some(format!("Failed to load sessions: {e}")));
                    }
                }
            });
        }
    };

    // Initial load
    fetch_sessions();

    // Load session detail when selected changes
    create_effect({
        let selected_session = selected_session.clone();
        let session_detail = session_detail.clone();
        let error = error.clone();
        move |_| {
            let id = selected_session.get();
            let session_detail = session_detail.clone();
            let error = error.clone();
            if let Some(id) = id {
                spawn_local(async move {
                    match gloo_net::http::Request::get(&crush_url(&format!("/sessions/{id}")))
                        .send()
                        .await
                    {
                        Ok(resp) => {
                            if let Ok(detail) = resp.json::<CrushSessionDetail>().await {
                                session_detail.set(Some(detail));
                            }
                        }
                        Err(e) => {
                            error.set(Some(format!("Failed to load session: {e}")));
                        }
                    }
                });
            } else {
                session_detail.set(None);
            }
        }
    });

    // Run prompt via SSE
    let run_prompt = {
        let prompt = prompt.clone();
        let selected_session = selected_session.clone();
        let cwd = cwd.clone();
        let model = model.clone();
        let output = output.clone();
        let running = running.clone();
        let error = error.clone();
        let active_provider = active_provider.clone();
        let fetch_sessions_after = {
            let fetch_sessions = fetch_sessions.clone();
            fetch_sessions
        };
        move || {
            let p = prompt.get();
            if p.trim().is_empty() || running.get() {
                return;
            }

            output.set(String::new());
            running.set(true);
            error.set(None);

            let sid = selected_session.get();
            let cwd_val = cwd.get();
            let model_val = model.get();

            let body = serde_json::json!({
                "prompt": p,
                "sessionId": sid,
                "cwd": if cwd_val.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(cwd_val) },
                "model": if model_val.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(model_val) },
            });

            let output2 = output.clone();
            let running2 = running.clone();
            let error2 = error.clone();
            let selected2 = selected_session.clone();
            let active_provider2 = active_provider.clone();
            let fetch_after = fetch_sessions_after.clone();

            spawn_local(async move {
                // crush-server /run is SSE — use fetch with ReadableStream
                let opts = web_sys::RequestInit::new();
                opts.set_method("POST");
                let headers = web_sys::Headers::new().unwrap();
                let _ = headers.set("Content-Type", "application/json");
                opts.set_headers(&headers);
                let body_str = body.to_string();
                opts.set_body(&JsValue::from_str(&body_str));

                let window = web_sys::window().unwrap();
                let resp_promise = window
                    .fetch_with_str_and_init(&crush_url("/run"), &opts);

                match wasm_bindgen_futures::JsFuture::from(resp_promise).await {
                    Err(e) => {
                        error2.set(Some(format!("Fetch error: {:?}", e)));
                        running2.set(false);
                        return;
                    }
                    Ok(resp_val) => {
                        let resp: web_sys::Response = resp_val.unchecked_into();
                        let body_stream = resp.body().unwrap();
                        let reader = body_stream.get_reader();
                        let reader: web_sys::ReadableStreamDefaultReader =
                            reader.unchecked_into();

                        let decoder = js_sys::Reflect::get(
                            &web_sys::window().unwrap(),
                            &JsValue::from_str("TextDecoder"),
                        ).unwrap();
                        let decoder = js_sys::Reflect::construct(
                            &decoder.unchecked_into::<js_sys::Function>(),
                            &js_sys::Array::new(),
                        ).unwrap();

                        let mut buf = String::new();

                        loop {
                            let read_promise = reader.read();
                            match wasm_bindgen_futures::JsFuture::from(read_promise).await {
                                Err(e) => {
                                    error2.set(Some(format!("Stream error: {:?}", e)));
                                    break;
                                }
                                Ok(result) => {
                                    let done = js_sys::Reflect::get(&result, &JsValue::from_str("done"))
                                        .unwrap()
                                        .as_bool()
                                        .unwrap_or(false);
                                    if done {
                                        break;
                                    }

                                    let value = js_sys::Reflect::get(&result, &JsValue::from_str("value")).unwrap();
                                    let decode_fn = js_sys::Reflect::get(&decoder, &JsValue::from_str("decode")).unwrap();
                                    let text = js_sys::Reflect::apply(
                                        &decode_fn.unchecked_into::<js_sys::Function>(),
                                        &decoder,
                                        &js_sys::Array::of1(&value),
                                    ).unwrap().as_string().unwrap_or_default();

                                    buf.push_str(&text);

                                    // Parse SSE lines from buffer
                                    while let Some(pos) = buf.find("\n\n") {
                                        let block = buf[..pos].to_string();
                                        buf = buf[pos + 2..].to_string();

                                        let mut event_type = String::from("message");
                                        let mut data = String::new();

                                        for line in block.lines() {
                                            if let Some(e) = line.strip_prefix("event: ") {
                                                event_type = e.to_string();
                                            } else if let Some(d) = line.strip_prefix("data: ") {
                                                data = d.to_string();
                                            }
                                        }

                                        match event_type.as_str() {
                                            "chunk" => {
                                                let decoded = data.replace("\\n", "\n");
                                                output2.update(|o| o.push_str(&decoded));
                                            }
                                            "provider" => {
                                                // Failover notification from crush-server
                                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                                                    if let Some(p) = v.get("provider").and_then(|s| s.as_str()) {
                                                        active_provider2.set(p.to_string());
                                                    }
                                                    // Surface failover reason as a subtle status note
                                                    if let Some(reason) = v.get("reason").and_then(|s| s.as_str()) {
                                                        output2.update(|o| {
                                                            o.push_str(&format!("\n⚡ Failover to crush: {}\n", reason));
                                                        });
                                                    }
                                                }
                                            }
                                            "done" => {
                                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                                                    if let Some(sid) = v.get("sessionId").and_then(|s| s.as_str()) {
                                                        selected2.set(Some(sid.to_string()));
                                                    }
                                                    // Update provider badge from done payload
                                                    if let Some(p) = v.get("provider").and_then(|s| s.as_str()) {
                                                        active_provider2.set(p.to_string());
                                                    }
                                                }
                                                running2.set(false);
                                                fetch_after();
                                            }
                                            "error" => {
                                                error2.set(Some(data.trim_matches('"').to_string()));
                                                running2.set(false);
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }

                        if running2.get() {
                            running2.set(false);
                        }
                    }
                }
            });
        }
    };

    // Delete session
    let delete_session = {
        let sessions = sessions.clone();
        let selected_session = selected_session.clone();
        let session_detail = session_detail.clone();
        let fetch_sessions = fetch_sessions.clone();
        move |id: String| {
            let _sessions = sessions.clone();
            let selected = selected_session.clone();
            let detail = session_detail.clone();
            let fetch = fetch_sessions.clone();
            spawn_local(async move {
                let _ = gloo_net::http::Request::delete(&crush_url(&format!("/sessions/{id}")))
                    .send()
                    .await;
                selected.set(None);
                detail.set(None);
                fetch();
            });
        }
    };

    view! {
        <div class="coding-agent">
            <div class="ca-header">
                <h2>"⚡ Coding Agent"</h2>
                <span class="ca-subtitle">"crush — agentic coding sessions"</span>
                <button
                    class="ca-refresh-btn"
                    on:click={
                        let f = fetch_sessions.clone();
                        move |_| f()
                    }
                >"↻ Refresh"</button>
            </div>

            {move || error.get().map(|e| view! {
                <div class="ca-error">{e}</div>
            })}

            <div class="ca-layout">
                // ── Session sidebar ─────────────────────────────────────────
                <div class="ca-sidebar">
                    <div class="ca-sidebar-header">
                        <span>"Sessions"</span>
                        <button
                            class="ca-new-btn"
                            title="New session"
                            on:click={
                                let sel = selected_session.clone();
                                let out = output.clone();
                                let det = session_detail.clone();
                                move |_| {
                                    sel.set(None);
                                    out.set(String::new());
                                    det.set(None);
                                }
                            }
                        >"+ New"</button>
                    </div>
                    <div class="ca-session-list">
                        {move || {
                            let list = sessions.get();
                            if list.is_empty() {
                                view! { <div class="ca-empty">"No sessions yet"</div> }.into_view()
                            } else {
                                list.into_iter().map(|s| {
                                    let id = s.id.clone();
                                    let id2 = id.clone();
                                    let title = s.title.clone().unwrap_or_else(|| id[..8.min(id.len())].to_string());
                                    let model_label = s.model.clone().unwrap_or_default();
                                    let count = s.message_count.unwrap_or(0);
                                    let selected = selected_session.clone();
                                    let del = delete_session.clone();
                                    let _sel_id = selected_session.get();
                                    view! {
                                        <div
                                            class="ca-session-item"
                                            class:ca-session-active=move || selected_session.get().as_deref() == Some(&id)
                                            on:click={
                                                let id = id.clone();
                                                let selected = selected.clone();
                                                move |_| selected.set(Some(id.clone()))
                                            }
                                        >
                                            <div class="ca-session-title">{title}</div>
                                            <div class="ca-session-meta">
                                                {if !model_label.is_empty() {
                                                    view! { <span class="ca-badge">{model_label}</span> }.into_view()
                                                } else {
                                                    view! {}.into_view()
                                                }}
                                                <span class="ca-msg-count">{count}" msgs"</span>
                                            </div>
                                            <button
                                                class="ca-del-btn"
                                                title="Delete session"
                                                on:click={
                                                    let id2 = id2.clone();
                                                    let del = del.clone();
                                                    move |e: web_sys::MouseEvent| {
                                                        e.stop_propagation();
                                                        del(id2.clone());
                                                    }
                                                }
                                            >"🗑"</button>
                                        </div>
                                    }
                                }).collect_view()
                            }
                        }}
                    </div>
                </div>

                // ── Main panel ──────────────────────────────────────────────
                <div class="ca-main">
                    // Session detail / history
                    {move || {
                        session_detail.get().map(|detail| {
                            let msgs = detail.messages.unwrap_or_default();
                            view! {
                                <div class="ca-history">
                                    <div class="ca-history-header">
                                        <span class="ca-session-name">
                                            {detail.title.unwrap_or_else(|| "Untitled".to_string())}
                                        </span>
                                        {detail.cwd.map(|c| view! {
                                            <span class="ca-cwd" title="Working directory">{"📁 "}{c}</span>
                                        })}
                                    </div>
                                    <div class="ca-messages">
                                        {msgs.into_iter().map(|m| {
                                            let role_class = if m.role == "assistant" { "ca-msg-assistant" } else { "ca-msg-user" };
                                            let role_label = if m.role == "assistant" { "🤖" } else { "👤" };
                                            view! {
                                                <div class={format!("ca-msg {role_class}")}>
                                                    <span class="ca-msg-role">{role_label}</span>
                                                    <pre class="ca-msg-content">{m.content}</pre>
                                                </div>
                                            }
                                        }).collect_view()}
                                    </div>
                                </div>
                            }.into_view()
                        })
                    }}

                    // Streaming output + diff view
                    {move || {
                        let out = output.get();
                        if !out.is_empty() {
                            let has_diff = looks_like_diff(&out);
                            let diff_text = if has_diff { extract_diff(&out) } else { String::new() };
                            let showing_diff = show_diff.get() && has_diff;

                            view! {
                                <div class="ca-output-panel">
                                    <div class="ca-output-label">
                                        {move || if running.get() {
                                            view! { <span class="ca-running">"⏳ Running..."</span> }.into_view()
                                        } else {
                                            view! { <span class="ca-done">"✅ Done"</span> }.into_view()
                                        }}
                                        // Provider badge: shows which backend handled the request
                                        {move || {
                                            let p = active_provider.get();
                                            if p.is_empty() {
                                                view! {}.into_view()
                                            } else {
                                                let (badge_class, label): (&'static str, String) = match p.as_str() {
                                                    "claude-code" => ("ca-provider-badge ca-provider-claude", "⚙ claude-code".to_string()),
                                                    "crush"       => ("ca-provider-badge ca-provider-crush", "🫎 crush".to_string()),
                                                    _             => ("ca-provider-badge", p.clone()),
                                                };
                                                view! {
                                                    <span class={badge_class}>{label}</span>
                                                }.into_view()
                                            }
                                        }}
                                        {if has_diff {
                                            view! {
                                                <button
                                                    class="ca-diff-toggle"
                                                    on:click={
                                                        let show_diff = show_diff.clone();
                                                        move |_| show_diff.update(|v| *v = !*v)
                                                    }
                                                >
                                                    {move || if show_diff.get() { "📝 Raw Output" } else { "📊 View Diff" }}
                                                </button>
                                            }.into_view()
                                        } else {
                                            view! {}.into_view()
                                        }}
                                    </div>
                                    {if showing_diff {
                                        view! {
                                            <div class="ca-diff-container">
                                                <DiffView diff_text=diff_text />
                                            </div>
                                        }.into_view()
                                    } else {
                                        view! {
                                            <pre class="ca-output">{out}</pre>
                                        }.into_view()
                                    }}
                                </div>
                            }.into_view()
                        } else {
                            view! {}.into_view()
                        }
                    }}

                    // Prompt area
                    <div class="ca-prompt-area">
                        <div class="ca-prompt-options">
                            <input
                                type="text"
                                class="ca-input-small"
                                placeholder="Working dir (optional)"
                                prop:value=move || cwd.get()
                                on:input=move |e| {
                                    cwd.set(event_target_value(&e));
                                }
                            />
                            <input
                                type="text"
                                class="ca-input-small"
                                placeholder="Model (optional, e.g. claude-3-5-sonnet)"
                                prop:value=move || model.get()
                                on:input=move |e| {
                                    model.set(event_target_value(&e));
                                }
                            />
                            {move || selected_session.get().map(|sid| {
                                let short = sid[..8.min(sid.len())].to_string();
                                view! {
                                    <span class="ca-continuing">{"Continuing: "}{short}</span>
                                }
                            })}
                        </div>
                        <div class="ca-prompt-row">
                            <textarea
                                class="ca-prompt-input"
                                placeholder="Enter a prompt for crush..."
                                rows="4"
                                prop:value=move || prompt.get()
                                on:input=move |e| {
                                    prompt.set(event_target_value(&e));
                                }
                                on:keydown={
                                    let run = run_prompt.clone();
                                    move |e: web_sys::KeyboardEvent| {
                                        if e.key() == "Enter" && (e.ctrl_key() || e.meta_key()) {
                                            run();
                                        }
                                    }
                                }
                            />
                            <button
                                class="ca-run-btn"
                                class:ca-run-busy=move || running.get()
                                disabled=move || running.get()
                                on:click={
                                    let run = run_prompt.clone();
                                    move |_| run()
                                }
                            >
                                {move || if running.get() { "Running..." } else { "▶ Run" }}
                            </button>
                        </div>
                        <div class="ca-prompt-hint">"Ctrl+Enter to run"</div>
                    </div>
                </div>
            </div>
        </div>
    }
}
