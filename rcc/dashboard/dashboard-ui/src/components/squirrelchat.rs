use leptos::*;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

// ─── Types (imported from shared module) ─────────────────────────────────────
// All SC data structures live in sc_types.rs — import from there, not here.

use crate::components::sc_types::{
    ScChannel, ScFile, ScIdentity, ScMessage, ScProject, ScUser, ScWsFrame,
    DEFAULT_CHANNELS, FALLBACK_AGENT_NAMES,
};
use crate::components::sc_reactions::{EmojiPicker, ReactionsBar};
use crate::components::sc_thread::ThreadPanel;
use crate::components::sc_channel_modal::CreateChannelModal;

// ─── Helpers ──────────────────────────────────────────────────────────────────

// format_msg_ts is now ScMessage::format_ts() — no standalone helper needed

fn render_text_with_mentions(text: &str) -> impl IntoView {
    let parts: Vec<leptos::View> = text
        .split(' ')
        .enumerate()
        .map(|(i, word)| {
            let spacer = if i > 0 { " " } else { "" };
            if word.starts_with('@') {
                view! { <span class="sc-mention">{format!("{}{}", spacer, word)}</span> }
                    .into_view()
            } else {
                view! { <span>{format!("{}{}", spacer, word)}</span> }.into_view()
            }
        })
        .collect();
    view! { <span class="sc-msg-body">{parts}</span> }
}

// ─── Auth helpers ────────────────────────────────────────────────────────────

/// Get the localStorage Storage object
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok())
        .flatten()
}

/// Read the stored auth token from localStorage (key: "sc_token")
fn sc_token() -> Option<String> {
    local_storage()
        .and_then(|s| s.get_item("sc_token").ok())
        .flatten()
        .filter(|t: &String| !t.is_empty())
}

/// Read the stored identity from localStorage (key: "sc_identity" as JSON)
fn sc_stored_identity() -> Option<ScIdentity> {
    let raw: String = local_storage()
        .and_then(|s| s.get_item("sc_identity").ok())
        .flatten()?;
    serde_json::from_str(&raw).ok()
}

/// Attach the auth token to a request builder if available
fn with_auth(req: gloo_net::http::RequestBuilder) -> gloo_net::http::RequestBuilder {
    if let Some(token) = sc_token() {
        req.header("Authorization", &format!("Bearer {}", token))
    } else {
        req
    }
}

// ─── Async fetchers ───────────────────────────────────────────────────────────

async fn fetch_sc_messages(channel: String) -> Vec<ScMessage> {
    // Primary: Axum squirrelchat-server on 8793
    let url = format!("http://localhost:8793/api/messages?channel={}&limit=50", channel);
    let Ok(req) = gloo_net::http::Request::get(&url).build() else { return vec![] };
    let Ok(resp) = req.send().await else { return vec![] };
    if resp.ok() {
        return resp.json::<Vec<ScMessage>>().await.unwrap_or_default();
    }
    // Fallback: Node.js server.mjs via dashboard proxy
    let url2 = format!("/sc/api/messages?channel={}&limit=50", channel);
    let Ok(req2) = with_auth(gloo_net::http::Request::get(&url2)).build() else { return vec![] };
    let Ok(resp2) = req2.send().await else { return vec![] };
    resp2.json::<Vec<ScMessage>>().await.unwrap_or_default()
}

/// Fetch channel list from Axum server; falls back to Node proxy, then hardcoded defaults.
async fn fetch_sc_channels() -> Vec<ScChannel> {
    // Primary: Axum squirrelchat-server on 8793
    if let Ok(resp) = gloo_net::http::Request::get("http://localhost:8793/api/channels").send().await {
        if resp.ok() {
            if let Ok(chs) = resp.json::<Vec<ScChannel>>().await {
                if !chs.is_empty() { return chs; }
            }
        }
    }
    // Fallback: Node proxy
    let Ok(req) = with_auth(gloo_net::http::Request::get("/sc/api/channels")).build() else {
        return default_channels();
    };
    match req.send().await {
        Ok(resp) if resp.ok() => resp.json::<Vec<ScChannel>>().await.unwrap_or_else(|_| default_channels()),
        _ => default_channels(),
    }
}

fn default_channels() -> Vec<ScChannel> {
    DEFAULT_CHANNELS
        .iter()
        .map(|(id, name)| ScChannel {
            id: id.to_string(),
            name: name.to_string(),
            channel_type: Some("public".to_string()),
            ..Default::default()
        })
        .collect()
}

/// Fetch current user identity from /sc/api/me; falls back to localStorage,
/// then to a generic "anonymous" identity.
async fn fetch_sc_identity() -> ScIdentity {
    // Try server first
    if let Ok(req) = with_auth(gloo_net::http::Request::get("/sc/api/me")).build() {
        if let Ok(resp) = req.send().await {
            if resp.ok() {
                if let Ok(id) = resp.json::<ScIdentity>().await {
                    return id;
                }
            }
        }
    }
    // Fall back to localStorage
    if let Some(id) = sc_stored_identity() {
        return id;
    }
    // Final fallback
    ScIdentity {
        id: "anonymous".to_string(),
        name: "anonymous".to_string(),
        needs_name: true,
        ..Default::default()
    }
}

async fn fetch_sc_agents() -> Vec<ScUser> {
    // Primary: Axum squirrelchat-server on 8793
    if let Ok(resp) = gloo_net::http::Request::get("http://localhost:8793/api/agents").send().await {
        if resp.ok() {
            if let Ok(users) = resp.json::<Vec<ScUser>>().await {
                if !users.is_empty() { return users; }
            }
        }
    }
    // Fallback: Node proxy
    let Ok(req) = with_auth(gloo_net::http::Request::get("/sc/api/agents")).build() else {
        return vec![];
    };
    let Ok(resp) = req.send().await else { return vec![] };
    resp.json::<Vec<ScUser>>().await.unwrap_or_else(|_| {
        FALLBACK_AGENT_NAMES
            .iter()
            .map(|n| ScUser {
                id: n.to_string(),
                name: n.to_string(),
                ..Default::default()
            })
            .collect()
    })
}

async fn fetch_sc_projects() -> Vec<ScProject> {
    let Ok(resp) = gloo_net::http::Request::get("/sc/api/projects").send().await else {
        return vec![];
    };
    resp.json::<Vec<ScProject>>().await.unwrap_or_default()
}

async fn fetch_sc_files(project_id: String) -> Vec<ScFile> {
    let url = format!("/sc/api/projects/{}/files", project_id);
    let Ok(resp) = gloo_net::http::Request::get(&url).send().await else {
        return vec![];
    };
    resp.json::<Vec<ScFile>>().await.unwrap_or_default()
}

// ─── Send helper (free function — signals are Copy) ───────────────────────────

fn trigger_send(
    input_text: ReadSignal<String>,
    set_input_text: WriteSignal<String>,
    selected_channel: ReadSignal<String>,
    set_messages: WriteSignal<Vec<ScMessage>>,
    sending: ReadSignal<bool>,
    set_sending: WriteSignal<bool>,
    set_mention_query: WriteSignal<Option<String>>,
    identity: ReadSignal<ScIdentity>,
) {
    let text = input_text.get_untracked().trim().to_string();
    if text.is_empty() || sending.get_untracked() {
        return;
    }
    let channel = selected_channel.get_untracked();
    let sender_name = identity.get_untracked().name;
    let token = identity.get_untracked().token.clone();
    set_sending.set(true);
    set_input_text.set(String::new());
    set_mention_query.set(None);

    let text_clone = text.clone();
    let channel_clone = channel.clone();
    let sender_name_clone = sender_name.clone();

    spawn_local(async move {
        let payload = serde_json::json!({
            "from": sender_name_clone,
            "text": text_clone,
            "channel": channel_clone,
        });
        // Post to Axum server; fallback to Node proxy
        let posted = {
            let req_builder = gloo_net::http::Request::post("http://localhost:8793/api/messages");
            if let Ok(req) = req_builder.json(&payload) {
                req.send().await.map(|r| r.ok()).unwrap_or(false)
            } else { false }
        };
        if !posted {
            let req_builder = gloo_net::http::Request::post("/sc/api/messages");
            let req_builder = if let Some(tok) = &token {
                req_builder.header("Authorization", &format!("Bearer {}", tok))
            } else {
                req_builder
            };
            if let Ok(req) = req_builder.json(&payload) {
                let _ = req.send().await;
            }
        }
        set_messages.update(|msgs| {
            msgs.push(ScMessage {
                id: None,
                from_agent: Some(sender_name.clone()),
                text: Some(text_clone),
                channel: Some(channel_clone),
                ..Default::default()
            });
        });
        set_sending.set(false);
    });
}

// ─── Component ────────────────────────────────────────────────────────────────

#[component]
pub fn SquirrelChat() -> impl IntoView {
    let (selected_channel, set_selected_channel) = create_signal("general".to_string());
    let (messages, set_messages) = create_signal(Vec::<ScMessage>::new());
    let (agents, set_agents) = create_signal(Vec::<ScUser>::new());
    let (channels, set_channels) = create_signal(
        DEFAULT_CHANNELS
            .iter()
            .map(|(id, name)| ScChannel {
                id: id.to_string(),
                name: name.to_string(),
                channel_type: Some("public".to_string()),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    );
    let (identity, set_identity) = create_signal(ScIdentity {
        id: "anonymous".to_string(),
        name: "anonymous".to_string(),
        needs_name: true,
        ..Default::default()
    });
    let (projects, set_projects) = create_signal(Vec::<ScProject>::new());
    let (project_files, set_project_files) = create_signal(Vec::<ScFile>::new());
    let (input_text, set_input_text) = create_signal(String::new());
    let (sending, set_sending) = create_signal(false);
    let (selected_project, set_selected_project) = create_signal(Option::<ScProject>::None);
    let (mention_query, set_mention_query) = create_signal(Option::<String>::None);
    let (unread, set_unread) =
        create_signal(std::collections::HashMap::<String, u32>::new());
    let (sc_connected, set_sc_connected) = create_signal(false);
    let (show_new_project, set_show_new_project) = create_signal(false);
    let (new_proj_name, set_new_proj_name) = create_signal(String::new());
    let (new_proj_desc, set_new_proj_desc) = create_signal(String::new());
    // Thread panel state
    let (thread_parent, set_thread_parent) = create_signal(Option::<ScMessage>::None);
    let (thread_replies, set_thread_replies) = create_signal(Vec::<ScMessage>::new());
    // Emoji picker state (per-message: store the message id that has picker open)
    let (picker_msg_id, set_picker_msg_id) = create_signal(Option::<i64>::None);
    // Channel create modal
    let (show_new_channel, set_show_new_channel) = create_signal(false);

    let chat_ref = create_node_ref::<leptos::html::Div>();

    // ── Load messages when channel changes ────────────────────────────────────
    let messages_res = create_resource(move || selected_channel.get(), fetch_sc_messages);

    create_effect(move |_| {
        if let Some(msgs) = messages_res.get() {
            set_messages.set(msgs);
        }
    });

    // ── Bootstrap identity + channels on load ─────────────────────────────────
    spawn_local(async move {
        let id = fetch_sc_identity().await;
        set_identity.set(id);
    });

    spawn_local(async move {
        let ch = fetch_sc_channels().await;
        if !ch.is_empty() {
            set_channels.set(ch);
        }
    });

    // ── Poll agents every 30 s ────────────────────────────────────────────────
    spawn_local(async move {
        loop {
            let ag = fetch_sc_agents().await;
            if !ag.is_empty() {
                set_agents.set(ag);
            }
            gloo_timers::future::TimeoutFuture::new(30_000).await;
        }
    });

    // ── Load projects once ────────────────────────────────────────────────────
    spawn_local(async move {
        let proj = fetch_sc_projects().await;
        set_projects.set(proj);
    });

    // ── WebSocket connection to squirrelchat-server ───────────────────────────
    // Constructs ws://<host>:8793/api/ws from window.location.
    // TODO: when dashboard-server proxies /sc/ws → 8793, switch to relative path.
    {
        let ws_url = web_sys::window()
            .and_then(|w| w.location().hostname().ok())
            .map(|host| format!("ws://{}:8793/api/ws", host))
            .unwrap_or_else(|| "ws://localhost:8793/api/ws".to_string());

        if let Ok(ws) = web_sys::WebSocket::new(&ws_url) {
            let ws_cleanup = ws.clone();

            // onopen — mark connected
            let open_cb = Closure::<dyn FnMut()>::new(move || {
                set_sc_connected.set(true);
            });
            ws.set_onopen(Some(open_cb.as_ref().unchecked_ref()));
            open_cb.forget();

            // onclose / onerror — mark disconnected
            let close_cb = Closure::<dyn FnMut(_)>::new(move |_: web_sys::CloseEvent| {
                set_sc_connected.set(false);
            });
            ws.set_onclose(Some(close_cb.as_ref().unchecked_ref()));
            close_cb.forget();

            let err_cb = Closure::<dyn FnMut(_)>::new(move |_: web_sys::ErrorEvent| {
                set_sc_connected.set(false);
            });
            ws.set_onerror(Some(err_cb.as_ref().unchecked_ref()));
            err_cb.forget();

            // onmessage — dispatch ServerFrame variants
            let msg_cb = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
                let data = e.data().as_string().unwrap_or_default();
                if data.is_empty() {
                    return;
                }
                if let Ok(frame) = serde_json::from_str::<ScWsFrame>(&data) {
                    match frame {
                        ScWsFrame::Connected { session_id: _ } => {
                            // Connected frame fires immediately on handshake
                            set_sc_connected.set(true);
                        }
                        ScWsFrame::Message { message } => {
                            let ch = message.channel.clone().unwrap_or_default();
                            let cur = selected_channel.get_untracked();
                            if ch == cur || ch.is_empty() {
                                set_messages.update(|msgs| msgs.push(message));
                            } else {
                                set_unread.update(|u| {
                                    *u.entry(ch).or_insert(0) += 1;
                                });
                            }
                        }
                        ScWsFrame::Reaction { message_id, reactions } => {
                            set_messages.update(|msgs| {
                                if let Some(msg) = msgs.iter_mut().find(|m| m.id == Some(message_id)) {
                                    msg.reactions.clear();
                                    for r in &reactions {
                                        msg.reactions.insert(r.emoji.clone(), r.agents.clone());
                                    }
                                }
                            });
                        }
                        ScWsFrame::Presence { agent, online } => {
                            set_agents.update(|agents| {
                                if let Some(a) = agents.iter_mut().find(|a| a.id == agent) {
                                    a.online = online;
                                    a.status = if online { "online".to_string() } else { "offline".to_string() };
                                }
                            });
                        }
                        ScWsFrame::Channel { action: _, channel } => {
                            set_channels.update(|chs| {
                                if !chs.iter().any(|c| c.id == channel.id) {
                                    chs.push(channel);
                                }
                            });
                        }
                        _ => {}
                    }
                }
            });
            ws.set_onmessage(Some(msg_cb.as_ref().unchecked_ref()));
            msg_cb.forget();

            on_cleanup(move || {
                let _ = ws_cleanup.close();
            });
        }
    }

    // ── Auto-scroll to bottom on new messages ─────────────────────────────────
    create_effect(move |_| {
        let _ = messages.get();
        if let Some(el) = chat_ref.get() {
            el.set_scroll_top(el.scroll_height());
        }
    });

    // ── @mention autocomplete ─────────────────────────────────────────────────
    let mention_suggestions = create_memo(move |_| {
        if let Some(q) = mention_query.get() {
            let q_lower = q.to_lowercase();
            let from_agents: Vec<String> = agents
                .get()
                .into_iter()
                .map(|a| a.name)
                .filter(|n| n.to_lowercase().starts_with(&q_lower))
                .collect();
            if from_agents.is_empty() {
                FALLBACK_AGENT_NAMES
                    .iter()
                    .filter(|n| n.to_lowercase().starts_with(&q_lower))
                    .map(|s| s.to_string())
                    .collect()
            } else {
                from_agents
            }
        } else {
            vec![]
        }
    });

    // ── View ──────────────────────────────────────────────────────────────────
    view! {
        <div class="sc-layout">
            // ── Sidebar ────────────────────────────────────────────────────────
            <aside class="sc-sidebar">
                // Channels
                <div class="sc-sidebar-section">
                    <div class="sc-section-header">
                        "Channels"
                        <button
                            class="sc-mini-btn"
                            on:click=move |_| set_show_new_channel.set(true)
                        >"+"</button>
                    </div>
                    {move || channels.get().into_iter().map(|ch| {
                        let ch_id = ch.id.clone();
                        let ch_id2 = ch.id.clone();
                        let ch_id3 = ch.id.clone();
                        let ch_name = ch.name.clone();
                        view! {
                            <div
                                class="sc-channel-item"
                                class:sc-channel-active=move || selected_channel.get() == ch_id
                                on:click=move |_| {
                                    set_selected_channel.set(ch_id2.clone());
                                    set_unread.update(|u| { u.remove(&ch_id2.clone()); });
                                    set_selected_project.set(None);
                                }
                            >
                                <span>"#" {ch_name}</span>
                                {move || {
                                    let n = unread.get().get(&ch_id3).copied().unwrap_or(0);
                                    if n > 0 {
                                        view! { <span class="sc-unread">{n}</span> }.into_view()
                                    } else {
                                        ().into_view()
                                    }
                                }}
                            </div>
                        }
                    }).collect::<Vec<_>>()}
                </div>

                // Agents
                <div class="sc-sidebar-section">
                    <div class="sc-section-header">"Agents"</div>
                    {move || {
                        let ag_list = agents.get();
                        if ag_list.is_empty() {
                            FALLBACK_AGENT_NAMES.iter().map(|&name| {
                                view! {
                                    <div class="sc-agent-item">
                                        <span class="sc-presence">"🔴"</span>
                                        <span class="sc-agent-name">{name}</span>
                                    </div>
                                }
                            }).collect::<Vec<_>>().into_view()
                        } else {
                            ag_list.into_iter().map(|a| {
                                let icon = a.presence_icon();
                                view! {
                                    <div class="sc-agent-item">
                                        <span class="sc-presence">{icon}</span>
                                        <span class="sc-agent-name">{a.name.clone()}</span>
                                    </div>
                                }
                            }).collect::<Vec<_>>().into_view()
                        }
                    }}
                </div>

                // Projects
                <div class="sc-sidebar-section">
                    <div class="sc-section-header">
                        "📁 Projects"
                        <button
                            class="sc-mini-btn"
                            on:click=move |_| set_show_new_project.set(true)
                        >"+"</button>
                    </div>
                    {move || {
                        projects.get().into_iter().map(|p| {
                            let p_click = p.clone();
                            view! {
                                <div
                                    class="sc-project-item"
                                    on:click=move |_| {
                                        let pid = p_click.id.clone();
                                        set_selected_project.set(Some(p_click.clone()));
                                        spawn_local(async move {
                                            let files = fetch_sc_files(pid).await;
                                            set_project_files.set(files);
                                        });
                                    }
                                >
                                    {p.name.clone()}
                                </div>
                            }
                        }).collect::<Vec<_>>()
                    }}
                </div>
            </aside>

            // ── Main area ─────────────────────────────────────────────────────
            <div class="sc-main">
                {move || {
                    if let Some(proj) = selected_project.get() {
                        // ── Project panel ──────────────────────────────────────
                        let proj_id_dl = proj.id.clone();
                        let proj_id_del = proj.id.clone();
                        let proj_name_del = proj.name.clone();
                        view! {
                            <div class="sc-project-panel">
                                <div class="sc-panel-header">
                                    <h3 class="sc-panel-title">{proj.name.clone()}</h3>
                                    <button
                                        class="sc-close-btn"
                                        on:click=move |_| set_selected_project.set(None)
                                    >"✕ Close"</button>
                                </div>
                                <div class="sc-project-details">
                                    {proj.description.as_deref().map(|d| view! {
                                        <p class="sc-proj-desc">{d.to_string()}</p>
                                    })}
                                    <div class="sc-proj-meta">
                                        {proj.status.as_deref().map(|s| view! {
                                            <span class="sc-meta-item">
                                                <span class="sc-meta-label">"status: "</span>
                                                {s.to_string()}
                                            </span>
                                        })}
                                        {proj.assignee.as_deref().map(|a| view! {
                                            <span class="sc-meta-item">
                                                <span class="sc-meta-label">"assignee: "</span>
                                                {a.to_string()}
                                            </span>
                                        })}
                                    </div>
                                    {if !proj.tags.is_empty() {
                                        let tag_views: Vec<_> = proj.tags.iter().map(|t| view! {
                                            <span class="sc-tag">{t.clone()}</span>
                                        }).collect();
                                        Some(view! { <div class="sc-tags">{tag_views}</div> })
                                    } else {
                                        None
                                    }}
                                </div>

                                // Files
                                <div class="sc-files-section">
                                    <div class="sc-files-header">
                                        <span>"Files"</span>
                                        <a
                                            class="sc-dl-btn"
                                            href=format!("/sc/api/projects/{}/download", proj_id_dl)
                                            target="_blank"
                                        >"⬇ Download all"</a>
                                    </div>
                                    {move || {
                                        let files = project_files.get();
                                        if files.is_empty() {
                                            view! { <div class="sc-empty-files">"No files"</div> }.into_view()
                                        } else {
                                            files.into_iter().map(|f| view! {
                                                <div class="sc-file-item">
                                                    <span class="sc-file-name">{f.filename.clone()}</span>
                                                    {f.size.map(|s| view! {
                                                        <span class="sc-file-size">{format!("{} B", s)}</span>
                                                    })}
                                                </div>
                                            }).collect::<Vec<_>>().into_view()
                                        }
                                    }}
                                    // Upload
                                    <label class="sc-upload-btn">
                                        "📎 Upload file"
                                        <input
                                            type="file"
                                            style="display:none"
                                            on:change=move |ev| {
                                                use wasm_bindgen::JsCast;
                                                let input = ev
                                                    .target()
                                                    .unwrap()
                                                    .unchecked_into::<web_sys::HtmlInputElement>();
                                                let file_list = input.files().unwrap();
                                                if let Some(file) = file_list.get(0) {
                                                    let proj_id = proj.id.clone();
                                                    let file_name = file.name();
                                                    let reader = web_sys::FileReader::new().unwrap();
                                                    let reader2 = reader.clone();
                                                    let onload = Closure::<dyn FnMut(_)>::new(
                                                        move |_: web_sys::Event| {
                                                            if let Ok(result) = reader2.result() {
                                                                if let Some(data_url) = result.as_string() {
                                                                    let b64 = data_url
                                                                        .split(',')
                                                                        .nth(1)
                                                                        .unwrap_or("")
                                                                        .to_string();
                                                                    let pid = proj_id.clone();
                                                                    let fname = file_name.clone();
                                                                    spawn_local(async move {
                                                                        let url = format!(
                                                                            "/sc/api/projects/{}/files",
                                                                            pid
                                                                        );
                                                                        let payload = serde_json::json!({
                                                                            "filename": fname,
                                                                            "content": b64,
                                                                            "encoding": "base64",
                                                                        });
                                                                        if let Ok(req) =
                                                                            gloo_net::http::Request::post(&url)
                                                                                .json(&payload)
                                                                        {
                                                                            let _ = req.send().await;
                                                                        }
                                                                        let files =
                                                                            fetch_sc_files(pid).await;
                                                                        set_project_files.set(files);
                                                                    });
                                                                }
                                                            }
                                                        },
                                                    );
                                                    reader.set_onload(Some(
                                                        onload.as_ref().unchecked_ref(),
                                                    ));
                                                    onload.forget();
                                                    let _ = reader.read_as_data_url(&file);
                                                }
                                            }
                                        />
                                    </label>
                                </div>

                                // Delete project
                                <button
                                    class="sc-delete-btn"
                                    on:click=move |_| {
                                        let pid = proj_id_del.clone();
                                        let pname = proj_name_del.clone();
                                        spawn_local(async move {
                                            let url = format!("/sc/api/projects/{}", pid);
                                            if let Ok(req) =
                                                gloo_net::http::Request::delete(&url).build()
                                            {
                                                let _ = req.send().await;
                                            }
                                            let projs = fetch_sc_projects().await;
                                            set_projects.set(projs);
                                            set_selected_project.set(None);
                                            let _ = pname;
                                        });
                                    }
                                >"🗑 Delete Project"</button>
                            </div>
                        }.into_view()
                    } else {
                        // ── Chat view ──────────────────────────────────────────
                        view! {
                            <div class="sc-chat">
                                <div class="sc-chat-header">
                                    <span class="sc-channel-title">"#" {move || selected_channel.get()}</span>
                                    <span class="sc-conn-badge">
                                        {move || if sc_connected.get() {
                                            view! { <span class="conn-badge conn-live">"● live"</span> }.into_view()
                                        } else {
                                            view! { <span class="conn-badge conn-waiting">"○ offline"</span> }.into_view()
                                        }}
                                    </span>
                                </div>

                                <div class="sc-messages" node_ref=chat_ref>
                                    {move || {
                                        let msgs = messages.get();
                                        if msgs.is_empty() {
                                            return view! {
                                                <div class="sc-no-messages">
                                                    "No messages yet — waiting..."
                                                </div>
                                            }.into_view();
                                        }
                                        msgs.into_iter().enumerate().map(|(i, msg)| {
                                            let ts = if msg.ts > 0 { msg.format_ts() } else { String::new() };
                                            let from = msg.from_agent.clone()
                                                .unwrap_or_else(|| "?".to_string());
                                            let text = msg.text.clone().unwrap_or_default();
                                            let msg_id = msg.id.unwrap_or(0);
                                            let reactions = msg.reactions.clone();
                                            let reply_count = msg.reply_count;
                                            let current_user = identity.get_untracked().id.clone();
                                            let msg_for_thread = msg.clone();

                                            // Per-message picker visibility
                                            let picker_visible = create_memo(move |_| {
                                                picker_msg_id.get() == Some(msg_id)
                                            });
                                            let (picker_vis_read, _) = create_signal(false);

                                            view! {
                                                <div class="sc-msg">
                                                    <div class="sc-msg-header">
                                                        <span class="sc-msg-from">{from}</span>
                                                        <span class="sc-msg-ts">{ts}</span>
                                                        <div class="sc-msg-actions">
                                                            // React button — toggles emoji picker
                                                            <button class="sc-react-btn" on:click=move |_| {
                                                                set_picker_msg_id.update(|current| {
                                                                    if *current == Some(msg_id) {
                                                                        *current = None;
                                                                    } else {
                                                                        *current = Some(msg_id);
                                                                    }
                                                                });
                                                            }>"😀"</button>
                                                            // Thread button (only if top-level)
                                                            <button class="sc-thread-btn" on:click={
                                                                let msg_clone = msg_for_thread.clone();
                                                                move |_| {
                                                                    let m = msg_clone.clone();
                                                                    let mid = m.id.unwrap_or(0);
                                                                    set_thread_parent.set(Some(m));
                                                                    // Fetch thread replies
                                                                    spawn_local(async move {
                                                                        let url = format!("/sc/api/messages/{}/thread?limit=100", mid);
                                                                        let resp = gloo_net::http::Request::get(&url)
                                                                            .send().await;
                                                                        if let Ok(resp) = resp {
                                                                            let replies = resp.json::<Vec<ScMessage>>().await
                                                                                .unwrap_or_default();
                                                                            set_thread_replies.set(replies);
                                                                        }
                                                                    });
                                                                }
                                                            }>{
                                                                if reply_count > 0 {
                                                                    format!("💬 {}", reply_count)
                                                                } else {
                                                                    "💬".to_string()
                                                                }
                                                            }</button>
                                                            // Delete button
                                                            <button class="sc-del-btn" on:click=move |_| {
                                                                let mid = msg_id;
                                                                spawn_local(async move {
                                                                    let url = format!("/sc/api/messages/{}", mid);
                                                                    if let Ok(req) = gloo_net::http::Request::delete(&url).build() {
                                                                        let _ = req.send().await;
                                                                    }
                                                                });
                                                                set_messages.update(|msgs| {
                                                                    if i < msgs.len() { msgs.remove(i); }
                                                                });
                                                            }>"🗑"</button>
                                                        </div>
                                                    </div>
                                                    <div class="sc-msg-content">
                                                        {render_text_with_mentions(&text)}
                                                    </div>
                                                    // Emoji picker (shown when this msg's react btn is clicked)
                                                    {move || {
                                                        if picker_msg_id.get() == Some(msg_id) {
                                                            let (vis, _) = create_signal(true);
                                                            view! {
                                                                <EmojiPicker
                                                                    visible=vis
                                                                    on_pick=Callback::new(move |emoji: String| {
                                                                        set_picker_msg_id.set(None);
                                                                        let mid = msg_id;
                                                                        let user = identity.get_untracked().id.clone();
                                                                        // POST reaction to server
                                                                        spawn_local(async move {
                                                                            let url = format!("/sc/api/messages/{}/react", mid);
                                                                            let payload = serde_json::json!({
                                                                                "from": user,
                                                                                "emoji": emoji,
                                                                            });
                                                                            if let Ok(req) = gloo_net::http::Request::post(&url).json(&payload) {
                                                                                let _ = req.send().await;
                                                                            }
                                                                        });
                                                                        // Optimistic update
                                                                        set_messages.update(|msgs| {
                                                                            if let Some(m) = msgs.iter_mut().find(|m| m.id == Some(mid)) {
                                                                                let user_id = identity.get_untracked().id;
                                                                                let users = m.reactions.entry(emoji.clone()).or_default();
                                                                                if let Some(pos) = users.iter().position(|u| u == &user_id) {
                                                                                    users.remove(pos);
                                                                                    if users.is_empty() {
                                                                                        m.reactions.remove(&emoji);
                                                                                    }
                                                                                } else {
                                                                                    users.push(user_id);
                                                                                }
                                                                            }
                                                                        });
                                                                    })
                                                                    on_close=Callback::new(move |_| {
                                                                        set_picker_msg_id.set(None);
                                                                    })
                                                                />
                                                            }.into_view()
                                                        } else {
                                                            ().into_view()
                                                        }
                                                    }}
                                                    // Reactions bar
                                                    <ReactionsBar
                                                        reactions=reactions.clone()
                                                        current_user=current_user.clone()
                                                        message_id=msg_id
                                                        on_toggle=Callback::new(move |(mid, emoji): (i64, String)| {
                                                            let user = identity.get_untracked().id.clone();
                                                            // POST toggle reaction
                                                            spawn_local(async move {
                                                                let url = format!("/sc/api/messages/{}/react", mid);
                                                                let payload = serde_json::json!({
                                                                    "from": user,
                                                                    "emoji": emoji,
                                                                });
                                                                if let Ok(req) = gloo_net::http::Request::post(&url).json(&payload) {
                                                                    let _ = req.send().await;
                                                                }
                                                            });
                                                            // Optimistic update
                                                            set_messages.update(|msgs| {
                                                                if let Some(m) = msgs.iter_mut().find(|m| m.id == Some(mid)) {
                                                                    let user_id = identity.get_untracked().id;
                                                                    let users = m.reactions.entry(emoji.clone()).or_default();
                                                                    if let Some(pos) = users.iter().position(|u| u == &user_id) {
                                                                        users.remove(pos);
                                                                        if users.is_empty() {
                                                                            m.reactions.remove(&emoji);
                                                                        }
                                                                    } else {
                                                                        users.push(user_id);
                                                                    }
                                                                }
                                                            });
                                                        })
                                                    />
                                                </div>
                                            }
                                        }).collect::<Vec<_>>().into_view()
                                    }}
                                </div>

                                <div class="sc-input-area">
                                    {move || {
                                        let suggestions = mention_suggestions.get();
                                        if suggestions.is_empty() {
                                            ().into_view()
                                        } else {
                                            view! {
                                                <div class="sc-mention-dropdown">
                                                    {suggestions.into_iter().map(|name| {
                                                        let n = name.clone();
                                                        view! {
                                                            <div
                                                                class="sc-mention-item"
                                                                on:click=move |_| {
                                                                    set_input_text.update(|t| {
                                                                        if let Some(pos) = t.rfind('@') {
                                                                            t.truncate(pos);
                                                                            t.push('@');
                                                                            t.push_str(&n);
                                                                            t.push(' ');
                                                                        }
                                                                    });
                                                                    set_mention_query.set(None);
                                                                }
                                                            >{name.clone()}</div>
                                                        }
                                                    }).collect::<Vec<_>>()}
                                                </div>
                                            }.into_view()
                                        }
                                    }}
                                    <div class="sc-input-row">
                                        <textarea
                                            class="sc-textarea"
                                            placeholder="Type a message... @mention supported  (Ctrl+Enter to send)"
                                            prop:value=move || input_text.get()
                                            on:input=move |ev| {
                                                let val = event_target_value(&ev);
                                                // Detect @mention
                                                if let Some(at_pos) = val.rfind('@') {
                                                    let after = &val[at_pos + 1..];
                                                    if !after.contains(' ') && !after.is_empty() {
                                                        set_mention_query.set(Some(after.to_string()));
                                                    } else {
                                                        set_mention_query.set(None);
                                                    }
                                                } else {
                                                    set_mention_query.set(None);
                                                }
                                                set_input_text.set(val);
                                            }
                                            on:keydown=move |ev: web_sys::KeyboardEvent| {
                                                if ev.key() == "Enter" && ev.ctrl_key() {
                                                    ev.prevent_default();
                                                    trigger_send(
                                                        input_text, set_input_text,
                                                        selected_channel, set_messages,
                                                        sending, set_sending, set_mention_query,
                                                        identity,
                                                    );
                                                }
                                            }
                                        />
                                        <button
                                            class="sc-send-btn"
                                            class:sc-sending=move || sending.get()
                                            on:click=move |_| {
                                                trigger_send(
                                                    input_text, set_input_text,
                                                    selected_channel, set_messages,
                                                    sending, set_sending, set_mention_query,
                                                    identity,
                                                );
                                            }
                                        >
                                            {move || if sending.get() { "Sending..." } else { "Send" }}
                                        </button>
                                    </div>
                                </div>
                            </div>
                        }.into_view()
                    }
                }}
            </div>

            // ── New project modal ─────────────────────────────────────────────
            {move || {
                if !show_new_project.get() {
                    return ().into_view();
                }
                view! {
                    <div class="sc-modal-overlay" on:click=move |_| set_show_new_project.set(false)>
                        <div class="sc-modal" on:click=|ev| ev.stop_propagation()>
                            <h3 class="sc-modal-title">"New Project"</h3>
                            <input
                                type="text"
                                class="sc-modal-input"
                                placeholder="Project name"
                                prop:value=move || new_proj_name.get()
                                on:input=move |ev| set_new_proj_name.set(event_target_value(&ev))
                            />
                            <textarea
                                class="sc-modal-textarea"
                                placeholder="Description (optional)"
                                prop:value=move || new_proj_desc.get()
                                on:input=move |ev| set_new_proj_desc.set(event_target_value(&ev))
                            />
                            <div class="sc-modal-actions">
                                <button class="sc-btn-primary" on:click=move |_| {
                                    let name = new_proj_name.get_untracked();
                                    if name.is_empty() { return; }
                                    let desc = new_proj_desc.get_untracked();
                                    let id = name.to_lowercase().replace(' ', "-");
                                    let assignee = identity.get_untracked().name;
                                    spawn_local(async move {
                                        let payload = serde_json::json!({
                                            "id": id,
                                            "name": name,
                                            "description": desc,
                                            "tags": [],
                                            "assignee": assignee,
                                        });
                                        if let Ok(req) = gloo_net::http::Request::post("/sc/api/projects")
                                            .json(&payload) {
                                            let _ = req.send().await;
                                        }
                                        let projs = fetch_sc_projects().await;
                                        set_projects.set(projs);
                                    });
                                    set_show_new_project.set(false);
                                    set_new_proj_name.set(String::new());
                                    set_new_proj_desc.set(String::new());
                                }>"Create"</button>
                                <button class="sc-btn-cancel" on:click=move |_| {
                                    set_show_new_project.set(false);
                                }>"Cancel"</button>
                            </div>
                        </div>
                    </div>
                }.into_view()
            }}

            // ── Thread panel (right side) ─────────────────────────────────────
            {move || {
                if let Some(parent) = thread_parent.get() {
                    view! {
                        <ThreadPanel
                            parent=parent
                            replies=thread_replies
                            on_close=Callback::new(move |_| set_thread_parent.set(None))
                            on_reply=Callback::new(move |(parent_id, text): (i64, String)| {
                                let sender = identity.get_untracked().id.clone();
                                let token = identity.get_untracked().token.clone();
                                spawn_local(async move {
                                    let url = format!("/sc/api/messages/{}/reply", parent_id);
                                    let payload = serde_json::json!({
                                        "from": sender,
                                        "text": text,
                                    });
                                    let req_builder = gloo_net::http::Request::post(&url);
                                    let req_builder = if let Some(tok) = &token {
                                        req_builder.header("Authorization", &format!("Bearer {}", tok))
                                    } else {
                                        req_builder
                                    };
                                    if let Ok(req) = req_builder.json(&payload) {
                                        let _ = req.send().await;
                                    }
                                    // Re-fetch thread
                                    let thread_url = format!("/sc/api/messages/{}/thread?limit=100", parent_id);
                                    if let Ok(resp) = gloo_net::http::Request::get(&thread_url).send().await {
                                        let replies = resp.json::<Vec<ScMessage>>().await.unwrap_or_default();
                                        set_thread_replies.set(replies);
                                    }
                                });
                            })
                        />
                    }.into_view()
                } else {
                    ().into_view()
                }
            }}

            // ── Channel create modal ──────────────────────────────────────────
            {move || {
                if !show_new_channel.get() {
                    return ().into_view();
                }
                view! {
                    <CreateChannelModal
                        on_create=Callback::new(move |(slug, name, desc): (String, String, String)| {
                            set_show_new_channel.set(false);
                            let token = identity.get_untracked().token.clone();
                            spawn_local(async move {
                                let payload = serde_json::json!({
                                    "id": slug,
                                    "name": name,
                                    "description": desc,
                                    "type": "public",
                                });
                                let req_builder = gloo_net::http::Request::post("/sc/api/channels");
                                let req_builder = if let Some(tok) = &token {
                                    req_builder.header("Authorization", &format!("Bearer {}", tok))
                                } else {
                                    req_builder
                                };
                                if let Ok(req) = req_builder.json(&payload) {
                                    let _ = req.send().await;
                                }
                                // Re-fetch channels
                                let ch = fetch_sc_channels().await;
                                if !ch.is_empty() {
                                    set_channels.set(ch);
                                }
                            });
                        })
                        on_close=Callback::new(move |_| set_show_new_channel.set(false))
                    />
                }.into_view()
            }}
        </div>
    }
}
