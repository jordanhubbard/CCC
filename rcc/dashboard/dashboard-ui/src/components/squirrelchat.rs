use leptos::*;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

// ─── Types (imported from shared module) ─────────────────────────────────────
// All SC data structures live in sc_types.rs — import from there, not here.

use crate::components::sc_types::{
    ScAttachment, ScChannel, ScFile, ScIdentity, ScMessage, ScPresenceMap, ScProject, ScUser,
    ScWsFrame, DEFAULT_CHANNELS, FALLBACK_AGENT_NAMES,
};

// ─── Browser notification helper ─────────────────────────────────────────────

fn request_notification_permission() {
    use web_sys::Notification;
    if Notification::permission() == web_sys::NotificationPermission::Default {
        let _ = Notification::request_permission();
    }
}

fn maybe_notify(title: &str, body: &str) {
    use web_sys::Notification;
    // Only fire if tab is not focused
    let focused = web_sys::window()
        .and_then(|w| w.document())
        .map(|d| d.has_focus().unwrap_or(false))
        .unwrap_or(true);
    if focused {
        return;
    }
    if Notification::permission() != web_sys::NotificationPermission::Granted {
        return;
    }
    let opts = web_sys::NotificationOptions::new();
    opts.set_body(body);
    let _ = Notification::new_with_options(title, &opts);
}
use crate::components::sc_reactions::{EmojiPicker, ReactionsBar};
use crate::components::sc_thread::ThreadPanel;
use crate::components::sc_channel_modal::CreateChannelModal;

// ─── Helpers ──────────────────────────────────────────────────────────────────

// format_msg_ts is now ScMessage::format_ts() — no standalone helper needed

use crate::components::sc_types::ScReaction;

/// Optimistic toggle of a reaction on a message's Vec<ScReaction>.
/// Adds user to the reaction if not present, removes if present.
fn toggle_reaction(reactions: &mut Vec<ScReaction>, emoji: &str, user_id: &str) {
    if let Some(r) = reactions.iter_mut().find(|r| r.emoji == emoji) {
        if let Some(pos) = r.agents.iter().position(|u| u == user_id) {
            r.agents.remove(pos);
            r.count = r.agents.len();
            if r.count == 0 {
                reactions.retain(|r| r.emoji != emoji);
            }
        } else {
            r.agents.push(user_id.to_string());
            r.count = r.agents.len();
        }
    } else {
        reactions.push(ScReaction {
            emoji: emoji.to_string(),
            count: 1,
            agents: vec![user_id.to_string()],
        });
    }
}

/// Render text with basic markdown: **bold**, *italic*, `code`, ```blocks```, [links](url), @mentions.
fn render_markdown(text: &str) -> impl IntoView {
    // Handle fenced code blocks first (``` ... ```)
    let parts: Vec<View> = if text.contains("```") {
        let mut views: Vec<View> = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("```") {
            // Push text before the code block
            let before = &rest[..start];
            if !before.is_empty() {
                views.push(render_inline_markdown(before).into_view());
            }
            let after_open = &rest[start + 3..];
            // Find language hint (first line) and content
            let (lang, content_start) = if let Some(nl) = after_open.find('\n') {
                (&after_open[..nl], &after_open[nl + 1..])
            } else {
                ("", after_open)
            };
            if let Some(end) = content_start.find("```") {
                let code = &content_start[..end];
                let lang_str = lang.trim().to_string();
                let code_str = code.to_string();
                views.push(view! {
                    <pre class="sc-code-block">
                        {if !lang_str.is_empty() { view! { <span class="sc-code-lang">{lang_str}</span> }.into_view() } else { ().into_view() }}
                        <code>{code_str}</code>
                    </pre>
                }.into_view());
                rest = &content_start[end + 3..];
            } else {
                // Unclosed code block — treat rest as code
                views.push(view! { <pre class="sc-code-block"><code>{content_start.to_string()}</code></pre> }.into_view());
                rest = "";
                break;
            }
        }
        if !rest.is_empty() {
            views.push(render_inline_markdown(rest).into_view());
        }
        views
    } else {
        vec![render_inline_markdown(text).into_view()]
    };
    view! { <span class="sc-md">{parts}</span> }
}

/// Render inline markdown (bold, italic, code, links, @mentions). No block elements.
fn render_inline_markdown(text: &str) -> impl IntoView {
    // Split on newlines to handle line breaks
    let lines: Vec<&str> = text.split('\n').collect();
    let line_count = lines.len();
    let rendered: Vec<View> = lines.into_iter().enumerate().map(|(i, line)| {
        let mut parts: Vec<View> = Vec::new();
        let mut chars = line.char_indices().peekable();
        let bytes = line.as_bytes();
        let len = line.len();
        let mut pos = 0usize;

        while pos < len {
            // Check for **bold**
            if pos + 1 < len && bytes[pos] == b'*' && bytes[pos + 1] == b'*' {
                if let Some(end) = line[pos + 2..].find("**") {
                    let inner = &line[pos + 2..pos + 2 + end];
                    parts.push(view! { <strong>{inner.to_string()}</strong> }.into_view());
                    pos += 2 + end + 2;
                    continue;
                }
            }
            // Check for *italic* (single asterisk, not followed by another)
            if bytes[pos] == b'*' && (pos + 1 >= len || bytes[pos + 1] != b'*') {
                if let Some(end) = line[pos + 1..].find('*') {
                    let inner = &line[pos + 1..pos + 1 + end];
                    if !inner.is_empty() && !inner.contains(' ') {
                        parts.push(view! { <em>{inner.to_string()}</em> }.into_view());
                        pos += 1 + end + 1;
                        continue;
                    }
                }
            }
            // Check for `inline code`
            if bytes[pos] == b'`' {
                if let Some(end) = line[pos + 1..].find('`') {
                    let inner = &line[pos + 1..pos + 1 + end];
                    parts.push(view! { <code class="sc-inline-code">{inner.to_string()}</code> }.into_view());
                    pos += 1 + end + 1;
                    continue;
                }
            }
            // Check for [link](url)
            if bytes[pos] == b'[' {
                if let Some(bracket_end) = line[pos..].find("](") {
                    let label = &line[pos + 1..pos + bracket_end];
                    let after = &line[pos + bracket_end + 2..];
                    if let Some(paren_end) = after.find(')') {
                        let url = &after[..paren_end];
                        let label_str = label.to_string();
                        let url_str = url.to_string();
                        parts.push(view! {
                            <a href=url_str target="_blank" rel="noopener" class="sc-md-link">{label_str}</a>
                        }.into_view());
                        pos += bracket_end + 2 + paren_end + 1;
                        continue;
                    }
                }
            }
            // Check for @mention
            if bytes[pos] == b'@' {
                let word_end = line[pos..].find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-').unwrap_or(line.len() - pos);
                if word_end > 1 {
                    let word = &line[pos..pos + word_end];
                    let word_str = word.to_string();
                    parts.push(view! { <span class="sc-mention">{word_str}</span> }.into_view());
                    pos += word_end;
                    continue;
                }
            }
            // Plain character — accumulate
            let char_end = line[pos..].char_indices().nth(1).map(|(i, _)| pos + i).unwrap_or(len);
            // Peek ahead for a run of plain chars
            let mut plain_end = char_end;
            while plain_end < len {
                let b = bytes[plain_end];
                if b == b'*' || b == b'`' || b == b'@' || b == b'[' { break; }
                plain_end += line[plain_end..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            let plain = &line[pos..plain_end];
            parts.push(view! { <span>{plain.to_string()}</span> }.into_view());
            pos = plain_end;
        }

        let mut line_view: Vec<View> = parts;
        if i + 1 < line_count {
            line_view.push(view! { <br/> }.into_view());
        }
        view! { <span>{line_view}</span> }.into_view()
    }).collect();
    view! { <span>{rendered}</span> }
}

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
    let url = format!("/sc/api/messages?channel={}&limit=50", channel);
    let Ok(req) = with_auth(gloo_net::http::Request::get(&url)).build() else { return vec![] };
    let Ok(resp) = req.send().await else { return vec![] };
    resp.json::<Vec<ScMessage>>().await.unwrap_or_default()
}

/// Fetch channel list from squirrelchat-server via /sc proxy.
async fn fetch_sc_channels() -> Vec<ScChannel> {
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
        // Post via /sc proxy
        {
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
    // Tracks channels where the current user has been @mentioned
    let (mentions_unread, set_mentions_unread) =
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
    // Search
    let (search_query, set_search_query) = create_signal(String::new());
    let (search_results, set_search_results) = create_signal(Vec::<ScMessage>::new());
    let (search_active, set_search_active) = create_signal(false);
    // DMs
    let (dm_channels, set_dm_channels) = create_signal(Vec::<ScChannel>::new());
    // File attach
    let (attach_data, set_attach_data) = create_signal(Option::<(String, String, String)>::None); // (filename, mime, b64)
    // Edit/delete/pin
    let (edit_msg_id, set_edit_msg_id) = create_signal(Option::<i64>::None);
    let (edit_text, set_edit_text) = create_signal(String::new());
    let (pinned_messages, set_pinned_messages) = create_signal(Vec::<ScMessage>::new());
    let (show_pins_panel, set_show_pins_panel) = create_signal(false);
    // Mobile sidebar toggle
    let (sidebar_open, set_sidebar_open) = create_signal(false);
    // Presence map — polled every 30s
    let (presence, set_presence) = create_signal(ScPresenceMap::default());
    // Typing indicators: channel_id → set of agent names currently typing (excluding self)
    let (typing_users, set_typing_users) = create_signal(std::collections::HashMap::<String, std::collections::HashSet<String>>::new());
    // WebSocket handle for sending client frames
    let ws_handle = store_value(Option::<web_sys::WebSocket>::None);
    // Debounce timer for stop-typing events (StoredValue is Copy — safe in reactive closures)
    let typing_timeout = store_value(Option::<gloo_timers::callback::Timeout>::None);

    let chat_ref = create_node_ref::<leptos::html::Div>();

    // ── Load messages when channel changes ────────────────────────────────────
    let messages_res = create_resource(move || selected_channel.get(), fetch_sc_messages);

    create_effect(move |_| {
        if let Some(msgs) = messages_res.get() {
            set_messages.set(msgs);
        }
    });

    // ── Load pins when channel changes ────────────────────────────────────────
    create_effect(move |_| {
        let ch = selected_channel.get();
        spawn_local(async move {
            let url = format!("/sc/api/channels/{}/pins", ch);
            if let Ok(resp) = gloo_net::http::Request::get(&url).send().await {
                if let Ok(data) = resp.json::<serde_json::Value>().await {
                    if let Some(pins) = data["pins"].as_array() {
                        let msgs: Vec<ScMessage> = pins.iter()
                            .filter_map(|v| serde_json::from_value(v.clone()).ok())
                            .collect();
                        set_pinned_messages.set(msgs);
                    }
                }
            }
        });
    });

    // Request notification permission on mount
    request_notification_permission();

    // ── Keyboard shortcuts ────────────────────────────────────────────────────
    let (show_channel_switcher, set_show_channel_switcher) = create_signal(false);
    let (show_help, set_show_help) = create_signal(false);
    let (switcher_query, set_switcher_query) = create_signal(String::new());

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
    // WebSocket connects directly to squirrelchat-server (8793); dashboard-server
    // does not proxy WS upgrades. Uses wss:// when page is served over HTTPS.
    {
        let ws_url = web_sys::window()
            .and_then(|w| {
                let loc = w.location();
                let proto = loc.protocol().ok()?;
                let host = loc.hostname().ok()?;
                let ws_proto = if proto == "https:" { "wss" } else { "ws" };
                Some(format!("{}://{}:8793/api/ws", ws_proto, host))
            })
            .unwrap_or_else(|| "ws://localhost:8793/api/ws".to_string());

        if let Ok(ws) = web_sys::WebSocket::new(&ws_url) {
            ws_handle.set_value(Some(ws.clone()));
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
                            // Check if current user is @mentioned in this message
                            let my_id = identity.get_untracked().id.clone();
                            let my_name = identity.get_untracked().name.clone();
                            let msg_text = message.text.as_deref().unwrap_or("");
                            let is_mention = message.mentions.iter().any(|m| {
                                m == &my_id || m == &my_name
                                    || m == &format!("@{}", my_id)
                                    || m == &format!("@{}", my_name)
                            }) || msg_text.contains(&format!("@{}", my_id))
                              || msg_text.contains(&format!("@{}", my_name));
                            // DM notification regardless of mention
                            let is_dm = ch.starts_with("dm-");
                            if ch == cur || ch.is_empty() {
                                set_messages.update(|msgs| msgs.push(message));
                            } else {
                                set_unread.update(|u| {
                                    *u.entry(ch.clone()).or_insert(0) += 1;
                                });
                                if is_dm && !is_mention {
                                    let from = message.from_agent.as_deref().unwrap_or("someone");
                                    let dm_text = msg_text.to_string();
                                    maybe_notify(&format!("DM from {}", from), &dm_text);
                                }
                                if is_mention {
                                    set_mentions_unread.update(|u| {
                                        *u.entry(ch.clone()).or_insert(0) += 1;
                                    });
                                    // Browser notification when tab not focused
                                    let notif_title = format!("@mention in #{}", ch);
                                    let notif_body = msg_text.to_string();
                                    maybe_notify(&notif_title, &notif_body);
                                }
                            }
                        }
                        ScWsFrame::Reaction { message_id, reactions } => {
                            set_messages.update(|msgs| {
                                if let Some(msg) = msgs.iter_mut().find(|m| m.id == Some(message_id)) {
                                    msg.reactions = reactions.clone();
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
                        ScWsFrame::Channel { action, channel } => {
                            if action == "dm_opened" || channel.channel_type.as_deref() == Some("dm") {
                                set_dm_channels.update(|dms| {
                                    if !dms.iter().any(|c| c.id == channel.id) {
                                        dms.push(channel);
                                    }
                                });
                            } else {
                                set_channels.update(|chs| {
                                    if !chs.iter().any(|c| c.id == channel.id) {
                                        chs.push(channel);
                                    }
                                });
                            }
                        }
                        ScWsFrame::Typing { channel, agent, is_typing } => {
                            let my_name = identity.get_untracked().name.clone();
                            if agent != my_name {
                                set_typing_users.update(|map| {
                                    let set = map.entry(channel).or_default();
                                    if is_typing {
                                        set.insert(agent);
                                    } else {
                                        set.remove(&agent);
                                    }
                                });
                            }
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

    // ── Presence polling (every 30s) ──────────────────────────────────────────
    {
        // Initial fetch
        spawn_local(async move {
            if let Ok(resp) = gloo_net::http::Request::get("/sc/api/presence").send().await {
                if let Ok(data) = resp.json::<ScPresenceMap>().await {
                    set_presence.set(data);
                }
            }
        });
        // Interval: poll every 30s
        use gloo_timers::callback::Interval;
        let _presence_interval = Interval::new(30_000, move || {
            spawn_local(async move {
                if let Ok(resp) = gloo_net::http::Request::get("/sc/api/presence").send().await {
                    if let Ok(data) = resp.json::<ScPresenceMap>().await {
                        set_presence.set(data);
                    }
                }
            });
        });
        // Keep interval alive
        _presence_interval.forget();
    }

    // ── Global keyboard shortcuts ─────────────────────────────────────────────
    // Cmd+K / Ctrl+K → channel switcher, Cmd+/ / Ctrl+/ → help
    {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;
        let handler = Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(move |ev: web_sys::KeyboardEvent| {
            let meta = ev.meta_key() || ev.ctrl_key();
            if meta && ev.key() == "k" {
                ev.prevent_default();
                set_show_channel_switcher.update(|v| *v = !*v);
                set_switcher_query.set(String::new());
            } else if meta && ev.key() == "/" {
                ev.prevent_default();
                set_show_help.update(|v| *v = !*v);
            } else if ev.key() == "Escape" {
                set_show_channel_switcher.set(false);
                set_show_help.set(false);
            }
        });
        if let Some(win) = web_sys::window() {
            let _ = win.add_event_listener_with_callback(
                "keydown",
                handler.as_ref().unchecked_ref(),
            );
        }
        handler.forget(); // leak intentionally — lives for app lifetime
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
            <aside class="sc-sidebar" class:sc-sidebar-open=move || sidebar_open.get()>
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
                                    set_mentions_unread.update(|u| { u.remove(&ch_id2.clone()); });
                                    set_selected_project.set(None);
                                }
                            >
                                <span>"#" {ch_name}</span>
                                {move || {
                                    let n = unread.get().get(&ch_id3).copied().unwrap_or(0);
                                    let m = mentions_unread.get().get(&ch_id3).copied().unwrap_or(0);
                                    if m > 0 {
                                        view! {
                                            <span class="sc-unread sc-unread-mention" title="You were mentioned">
                                                "@" {n}
                                            </span>
                                        }.into_view()
                                    } else if n > 0 {
                                        view! { <span class="sc-unread">{n}</span> }.into_view()
                                    } else {
                                        ().into_view()
                                    }
                                }}
                            </div>
                        }
                    }).collect::<Vec<_>>()}
                </div>

                // Agents — with live presence from /api/presence
                <div class="sc-sidebar-section">
                    <div class="sc-section-header">
                        "Agents"
                        // Online count badge
                        {move || {
                            let p = presence.get();
                            let online = p.agents.values().filter(|e| e.status == "online").count();
                            if online > 0 {
                                view! { <span class="sc-online-count" title="agents online">{online}" online"</span> }.into_view()
                            } else { ().into_view() }
                        }}
                    </div>
                    {move || {
                        let ag_list = agents.get();
                        let pmap = presence.get();
                        // Merge: use live agent list if available, else fallbacks
                        let names: Vec<String> = if ag_list.is_empty() {
                            FALLBACK_AGENT_NAMES.iter().map(|s| s.to_string()).collect()
                        } else {
                            ag_list.iter().map(|a| a.name.clone()).collect()
                        };
                        names.into_iter().map(|name| {
                            let p_entry = pmap.agents.get(&name).cloned();
                            let (status_class, icon, tip) = if let Some(ref p) = p_entry {
                                (p.dot_class(), p.icon(), p.status_text.clone())
                            } else {
                                // Unknown while presence hasn't loaded yet
                                ("sc-presence-dot sc-presence-unknown", "⚫", "unknown".to_string())
                            };
                            let name_clone = name.clone();
                            view! {
                                <div class="sc-agent-item" title=tip>
                                    <span class=status_class title=icon></span>
                                    <span class="sc-agent-name">{name_clone}</span>
                                </div>
                            }
                        }).collect::<Vec<_>>()
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

                // Direct Messages
                <div class="sc-sidebar-section">
                    <div class="sc-section-header">
                        "💬 DMs"
                        <button class="sc-new-btn" title="Start a DM" on:click=move |_| {
                            // Simple prompt-based DM open — production would use a modal
                            let my_id = identity.get_untracked().id.clone();
                            spawn_local(async move {
                                // Open DM with current identity — noop placeholder that wires the endpoint
                                // Full modal handled by future PR; endpoint is live
                                let _ = gloo_net::http::Request::post("/sc/api/dms")
                                    .json(&serde_json::json!({ "from": my_id, "to": "rocky" }))
                                    .ok()
                                    .unwrap()
                                    .send()
                                    .await;
                            });
                        }>"+"</button>
                    </div>
                    {move || {
                        let dms = dm_channels.get();
                        dms.iter().map(|dm| {
                            let dm_id = dm.id.clone();
                            let dm_id2 = dm.id.clone();
                            let dm_id3 = dm.id.clone();
                            let dm_name = dm.name.clone();
                            view! {
                                <div
                                    class="sc-channel-item sc-dm-item"
                                    class:sc-channel-active=move || selected_channel.get() == dm_id
                                    on:click=move |_| {
                                        set_selected_channel.set(dm_id2.clone());
                                        set_unread.update(|u| { u.remove(&dm_id2.clone()); });
                                        set_mentions_unread.update(|u| { u.remove(&dm_id2.clone()); });
                                        set_selected_project.set(None);
                                    }
                                >
                                    <span>"💬 " {dm_name}</span>
                                    {move || {
                                        let n = unread.get().get(&dm_id3).copied().unwrap_or(0);
                                        if n > 0 {
                                            view! { <span class="sc-unread">{n}</span> }.into_view()
                                        } else {
                                            ().into_view()
                                        }
                                    }}
                                </div>
                            }
                        }).collect::<Vec<_>>()
                    }}
                </div>
            </aside>

            // ── Main area ─────────────────────────────────────────────────────
            <div class="sc-main">
                // ── Search bar ────────────────────────────────────────────────
                <div class="sc-search-bar">
                    <input
                        type="text"
                        class="sc-search-input"
                        placeholder="🔍 Search messages..."
                        prop:value=move || search_query.get()
                        on:input=move |ev| {
                            use leptos::ev::Event;
                            let val = event_target_value(&ev);
                            set_search_query.set(val.clone());
                            if val.trim().is_empty() {
                                set_search_active.set(false);
                                set_search_results.set(vec![]);
                            } else {
                                set_search_active.set(true);
                                // Fetch search results
                                let q = val.clone();
                                spawn_local(async move {
                                    let url = format!("/sc/api/search?q={}&limit=20", js_sys::encode_uri_component(&q));
                                    if let Ok(resp) = gloo_net::http::Request::get(&url).send().await {
                                        if let Ok(data) = resp.json::<serde_json::Value>().await {
                                            if let Some(results) = data["results"].as_array() {
                                                let msgs: Vec<ScMessage> = results.iter()
                                                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                                                    .collect();
                                                set_search_results.set(msgs);
                                            }
                                        }
                                    }
                                });
                            }
                        }
                    />
                    {move || if search_active.get() {
                        view! {
                            <button class="sc-search-clear" on:click=move |_| {
                                set_search_query.set(String::new());
                                set_search_active.set(false);
                                set_search_results.set(vec![]);
                            }>"✕"</button>
                        }.into_view()
                    } else { ().into_view() }}
                </div>
                // ── Search results overlay ────────────────────────────────────
                {move || if search_active.get() {
                    let results = search_results.get();
                    view! {
                        <div class="sc-search-results">
                            <div class="sc-search-results-header">
                                {format!("{} result(s) for '{}'", results.len(), search_query.get())}
                            </div>
                            {if results.is_empty() {
                                view! { <div class="sc-search-empty">"No messages found."</div> }.into_view()
                            } else {
                                results.iter().map(|msg| {
                                    let ch = msg.channel.clone().unwrap_or_default();
                                    let from = msg.from_agent.clone().unwrap_or_default();
                                    let text = msg.text.clone().unwrap_or_default();
                                    let ts = msg.format_ts();
                                    let ch_clone = ch.clone();
                                    view! {
                                        <div class="sc-search-result-item" on:click=move |_| {
                                            set_selected_channel.set(ch_clone.clone());
                                            set_search_active.set(false);
                                            set_search_query.set(String::new());
                                            set_search_results.set(vec![]);
                                        }>
                                            <span class="sc-search-ch">"#" {ch}</span>
                                            <span class="sc-search-from">{from}</span>
                                            <span class="sc-search-ts">{ts}</span>
                                            <div class="sc-search-text">{text}</div>
                                        </div>
                                    }
                                }).collect::<Vec<_>>().into_view()
                            }}
                        </div>
                    }.into_view()
                } else { ().into_view() }}

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
                                    // Mobile hamburger
                                    <button class="sc-hamburger" aria-label="Toggle sidebar"
                                        on:click=move |_| set_sidebar_open.update(|v| *v = !*v)>
                                        "☰"
                                    </button>
                                    <span class="sc-channel-title">"#" {move || selected_channel.get()}</span>
                                    <span class="sc-conn-badge">
                                        {move || if sc_connected.get() {
                                            view! { <span class="conn-badge conn-live">"● live"</span> }.into_view()
                                        } else {
                                            view! { <span class="conn-badge conn-waiting">"○ offline"</span> }.into_view()
                                        }}
                                    </span>
                                    // Pins toggle button
                                    <button
                                        class="sc-pins-btn"
                                        class:sc-pins-active=move || show_pins_panel.get()
                                        title="Pinned messages"
                                        on:click=move |_| set_show_pins_panel.update(|v| *v = !*v)
                                    >
                                        {move || {
                                            let n = pinned_messages.get().len();
                                            if n > 0 { format!("📌 {}", n) } else { "📌".to_string() }
                                        }}
                                    </button>
                                </div>

                                // Pinned messages panel
                                {move || if show_pins_panel.get() {
                                    let pins = pinned_messages.get();
                                    let ch = selected_channel.get_untracked();
                                    view! {
                                        <div class="sc-pins-panel">
                                            <div class="sc-pins-header">
                                                "📌 Pinned messages in #" {ch.clone()}
                                            </div>
                                            {if pins.is_empty() {
                                                view! { <div class="sc-pins-empty">"No pinned messages yet."</div> }.into_view()
                                            } else {
                                                pins.iter().map(|msg| {
                                                    let mid = msg.id.unwrap_or(0);
                                                    let from = msg.from_agent.clone().unwrap_or_default();
                                                    let text = msg.text.clone().unwrap_or_default();
                                                    let ch2 = ch.clone();
                                                    view! {
                                                        <div class="sc-pin-item">
                                                            <span class="sc-pin-from">{from}</span>
                                                            <span class="sc-pin-text">{text}</span>
                                                            <button class="sc-pin-remove" title="Unpin"
                                                                on:click=move |_| {
                                                                    let ch3 = ch2.clone();
                                                                    spawn_local(async move {
                                                                        let url = format!("/sc/api/channels/{}/pins/{}", ch3, mid);
                                                                        if let Ok(req) = gloo_net::http::Request::delete(&url).build() {
                                                                            let _ = req.send().await;
                                                                        }
                                                                        // Refresh pins
                                                                        let url2 = format!("/sc/api/channels/{}/pins", ch3);
                                                                        if let Ok(resp) = gloo_net::http::Request::get(&url2).send().await {
                                                                            if let Ok(data) = resp.json::<serde_json::Value>().await {
                                                                                if let Some(arr) = data["pins"].as_array() {
                                                                                    let msgs: Vec<ScMessage> = arr.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect();
                                                                                    set_pinned_messages.set(msgs);
                                                                                }
                                                                            }
                                                                        }
                                                                    });
                                                                }
                                                            >"✕"</button>
                                                        </div>
                                                    }
                                                }).collect::<Vec<_>>().into_view()
                                            }}
                                        </div>
                                    }.into_view()
                                } else { ().into_view() }}

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
                                            let attachments = msg.attachments.clone();
                                            let reply_count = msg.reply_count;
                                            let current_user = identity.get_untracked().id.clone();
                                            let current_user_name = identity.get_untracked().name.clone();
                                            let msg_for_thread = msg.clone();

                                            // Detect if current user is mentioned in this message
                                            let text_str = msg.text.as_deref().unwrap_or("");
                                            let is_mentioned = msg.mentions.iter().any(|m| {
                                                m == &current_user || m == &current_user_name
                                                    || m == &format!("@{}", current_user)
                                                    || m == &format!("@{}", current_user_name)
                                            }) || text_str.contains(&format!("@{}", current_user))
                                              || text_str.contains(&format!("@{}", current_user_name));

                                            // Per-message picker visibility
                                            let picker_visible = create_memo(move |_| {
                                                picker_msg_id.get() == Some(msg_id)
                                            });
                                            let (picker_vis_read, _) = create_signal(false);

                                            view! {
                                                <div
                                                    class="sc-msg"
                                                    class:sc-msg-mention=is_mentioned
                                                >
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
                                                            // Edit button
                                                            {
                                                                let text_for_edit = text.clone();
                                                                view! {
                                                                    <button class="sc-edit-btn" title="Edit" on:click=move |_| {
                                                                        set_edit_msg_id.set(Some(msg_id));
                                                                        set_edit_text.set(text_for_edit.clone());
                                                                    }>"✏️"</button>
                                                                }
                                                            }
                                                            // Pin button
                                                            <button class="sc-pin-btn" title="Pin to channel" on:click=move |_| {
                                                                let mid = msg_id;
                                                                let ch = selected_channel.get_untracked();
                                                                let my_id = identity.get_untracked().id.clone();
                                                                spawn_local(async move {
                                                                    let url = format!("/sc/api/channels/{}/pins/{}", ch, mid);
                                                                    if let Ok(req) = gloo_net::http::Request::post(&url)
                                                                        .json(&serde_json::json!({"pinned_by": my_id})) {
                                                                        let _ = req.send().await;
                                                                    }
                                                                    // Refresh pins
                                                                    let url2 = format!("/sc/api/channels/{}/pins", ch);
                                                                    if let Ok(resp) = gloo_net::http::Request::get(&url2).send().await {
                                                                        if let Ok(data) = resp.json::<serde_json::Value>().await {
                                                                            if let Some(arr) = data["pins"].as_array() {
                                                                                let msgs: Vec<ScMessage> = arr.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect();
                                                                                set_pinned_messages.set(msgs);
                                                                            }
                                                                        }
                                                                    }
                                                                });
                                                            }>"📌"</button>
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
                                                    // Inline edit form
                                                    {move || if edit_msg_id.get() == Some(msg_id) {
                                                        view! {
                                                            <div class="sc-edit-form">
                                                                <textarea
                                                                    class="sc-edit-textarea"
                                                                    prop:value=move || edit_text.get()
                                                                    on:input=move |ev| set_edit_text.set(event_target_value(&ev))
                                                                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                                                                        if ev.key() == "Escape" {
                                                                            set_edit_msg_id.set(None);
                                                                        } else if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) {
                                                                            let mid = msg_id;
                                                                            let new_text = edit_text.get_untracked();
                                                                            spawn_local(async move {
                                                                                let url = format!("/sc/api/messages/{}", mid);
                                                                                if let Ok(req) = gloo_net::http::Request::patch(&url)
                                                                                    .json(&serde_json::json!({"text": new_text})) {
                                                                                    let _ = req.send().await;
                                                                                }
                                                                            });
                                                                            set_messages.update(|msgs| {
                                                                                if let Some(m) = msgs.get_mut(i) {
                                                                                    m.text = Some(edit_text.get_untracked());
                                                                                }
                                                                            });
                                                                            set_edit_msg_id.set(None);
                                                                        }
                                                                    }
                                                                />
                                                                <div class="sc-edit-actions">
                                                                    <span class="sc-edit-hint">"Ctrl+Enter to save · Esc to cancel"</span>
                                                                    <button class="sc-edit-save" on:click=move |_| {
                                                                        let mid = msg_id;
                                                                        let new_text = edit_text.get_untracked();
                                                                        spawn_local(async move {
                                                                            let url = format!("/sc/api/messages/{}", mid);
                                                                            if let Ok(req) = gloo_net::http::Request::patch(&url)
                                                                                .json(&serde_json::json!({"text": new_text})) {
                                                                                let _ = req.send().await;
                                                                            }
                                                                        });
                                                                        set_messages.update(|msgs| {
                                                                            if let Some(m) = msgs.get_mut(i) {
                                                                                m.text = Some(edit_text.get_untracked());
                                                                            }
                                                                        });
                                                                        set_edit_msg_id.set(None);
                                                                    }>"Save"</button>
                                                                </div>
                                                            </div>
                                                        }.into_view()
                                                    } else {
                                                        view! {
                                                            <div class="sc-msg-content">
                                                                {render_markdown(&text)}
                                                            </div>
                                                        }.into_view()
                                                    }}
                                                    // Inline attachments
                                                    {if !attachments.is_empty() {
                                                        let atts = attachments.clone();
                                                        view! {
                                                            <div class="sc-attachments">
                                                                {atts.iter().map(|att| {
                                                                    let url = format!("/sc/api/attachments/{}", att.id);
                                                                    let fname = att.filename.clone();
                                                                    let mime = att.mime_type.clone();
                                                                    let size_kb = att.size.map(|s| format!("{:.1}KB", s as f64 / 1024.0)).unwrap_or_default();
                                                                    if mime.starts_with("image/") {
                                                                        view! {
                                                                            <div class="sc-attachment sc-attachment-image">
                                                                                <img src=url.clone() alt=fname.clone() class="sc-attachment-img" />
                                                                                <div class="sc-attachment-name"><a href=url target="_blank">{fname}</a> {size_kb}</div>
                                                                            </div>
                                                                        }.into_view()
                                                                    } else {
                                                                        view! {
                                                                            <div class="sc-attachment sc-attachment-file">
                                                                                <span class="sc-attachment-icon">"📎"</span>
                                                                                <a href=url target="_blank" class="sc-attachment-name">{fname}</a>
                                                                                <span class="sc-attachment-size">{size_kb}</span>
                                                                            </div>
                                                                        }.into_view()
                                                                    }
                                                                }).collect::<Vec<_>>()}
                                                            </div>
                                                        }.into_view()
                                                    } else {
                                                        ().into_view()
                                                    }}
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
                                                                        let emoji_for_req = emoji.clone();
                                                                        // POST reaction to server
                                                                        spawn_local(async move {
                                                                            let url = format!("/sc/api/messages/{}/react", mid);
                                                                            let payload = serde_json::json!({
                                                                                "from": user,
                                                                                "emoji": emoji_for_req,
                                                                            });
                                                                            if let Ok(req) = gloo_net::http::Request::post(&url).json(&payload) {
                                                                                let _ = req.send().await;
                                                                            }
                                                                        });
                                                                        // Optimistic update
                                                                        set_messages.update(|msgs| {
                                                                            if let Some(m) = msgs.iter_mut().find(|m| m.id == Some(mid)) {
                                                                                let user_id = identity.get_untracked().id;
                                                                                toggle_reaction(&mut m.reactions, &emoji, &user_id);
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
                                                            let emoji_for_req = emoji.clone();
                                                            // POST toggle reaction
                                                            spawn_local(async move {
                                                                let url = format!("/sc/api/messages/{}/react", mid);
                                                                let payload = serde_json::json!({
                                                                    "from": user,
                                                                    "emoji": emoji_for_req,
                                                                });
                                                                if let Ok(req) = gloo_net::http::Request::post(&url).json(&payload) {
                                                                    let _ = req.send().await;
                                                                }
                                                            });
                                                            // Optimistic update
                                                            set_messages.update(|msgs| {
                                                                if let Some(m) = msgs.iter_mut().find(|m| m.id == Some(mid)) {
                                                                    let user_id = identity.get_untracked().id;
                                                                    toggle_reaction(&mut m.reactions, &emoji, &user_id);
                                                                }
                                                            });
                                                        })
                                                    />
                                                </div>
                                            }
                                        }).collect::<Vec<_>>().into_view()
                                    }}
                                </div>

                                // Typing indicator
                                {move || {
                                    let ch = selected_channel.get();
                                    let map = typing_users.get();
                                    let mut typers: Vec<String> = map.get(&ch)
                                        .map(|s| s.iter().cloned().collect())
                                        .unwrap_or_default();
                                    typers.sort();
                                    if typers.is_empty() {
                                        ().into_view()
                                    } else {
                                        let text = match typers.len() {
                                            1 => format!("{} is typing…", typers[0]),
                                            2 => format!("{} and {} are typing…", typers[0], typers[1]),
                                            _ => "Several people are typing…".to_string(),
                                        };
                                        view! { <div class="sc-typing-indicator">{text}</div> }.into_view()
                                    }
                                }}

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
                                                // Typing indicator: send is_typing=true, debounce false
                                                let my_name = identity.get_untracked().name.clone();
                                                let ch = selected_channel.get_untracked();
                                                let val_nonempty = !val.trim().is_empty();
                                                ws_handle.with_value(|ws_opt| {
                                                    if let Some(ws) = ws_opt {
                                                        typing_timeout.update_value(|t| { *t = None; });
                                                        if val_nonempty {
                                                            let start_msg = serde_json::json!({"type":"typing","channel":ch,"agent":my_name,"is_typing":true}).to_string();
                                                            let _ = ws.send_with_str(&start_msg);
                                                            let ws_c = ws.clone();
                                                            let ch_c = ch.clone();
                                                            let name_c = my_name.clone();
                                                            let timeout = gloo_timers::callback::Timeout::new(2000, move || {
                                                                let stop = serde_json::json!({"type":"typing","channel":ch_c,"agent":name_c,"is_typing":false}).to_string();
                                                                let _ = ws_c.send_with_str(&stop);
                                                                typing_timeout.set_value(None);
                                                            });
                                                            typing_timeout.set_value(Some(timeout));
                                                        }
                                                    }
                                                });
                                                set_input_text.set(val);
                                            }
                                            on:keydown=move |ev: web_sys::KeyboardEvent| {
                                                if ev.key() == "Enter" && ev.ctrl_key() {
                                                    ev.prevent_default();
                                                    // Cancel typing and send stop before sending
                                                    typing_timeout.update_value(|t| { *t = None; });
                                                    let my_name = identity.get_untracked().name.clone();
                                                    let ch = selected_channel.get_untracked();
                                                    ws_handle.with_value(|ws_opt| {
                                                        if let Some(ws) = ws_opt {
                                                            let stop = serde_json::json!({"type":"typing","channel":ch,"agent":my_name,"is_typing":false}).to_string();
                                                            let _ = ws.send_with_str(&stop);
                                                        }
                                                    });
                                                    trigger_send(
                                                        input_text, set_input_text,
                                                        selected_channel, set_messages,
                                                        sending, set_sending, set_mention_query,
                                                        identity,
                                                    );
                                                }
                                            }
                                        />
                                        // File attach button
                                        <label class="sc-attach-btn" title="Attach file">
                                            "📎"
                                            <input
                                                type="file"
                                                style="display:none"
                                                on:change=move |ev| {
                                                    use web_sys::HtmlInputElement;
                                                    let input: HtmlInputElement = ev.target().unwrap().unchecked_into();
                                                    if let Some(file) = input.files().and_then(|fl| fl.item(0)) {
                                                        let fname = file.name();
                                                        let ftype = file.type_();
                                                        let reader = web_sys::FileReader::new().unwrap();
                                                        let reader_clone = reader.clone();
                                                        let set_attach = set_attach_data.clone();
                                                        let fname_c = fname.clone();
                                                        let ftype_c = ftype.clone();
                                                        let closure = wasm_bindgen::closure::Closure::once(move || {
                                                            if let Ok(result) = reader_clone.result() {
                                                                let ab: js_sys::ArrayBuffer = result.dyn_into().unwrap();
                                                                let ua = js_sys::Uint8Array::new(&ab);
                                                                let bytes = ua.to_vec();
                                                                use base64::Engine;
                                                                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                                                                set_attach.set(Some((fname_c, ftype_c, b64)));
                                                            }
                                                        });
                                                        reader.set_onloadend(Some(closure.as_ref().unchecked_ref()));
                                                        reader.read_as_array_buffer(&file).unwrap();
                                                        closure.forget();
                                                    }
                                                }
                                            />
                                        </label>
                                        {move || if let Some((fname, _, _)) = attach_data.get() {
                                            view! {
                                                <span class="sc-attach-preview">
                                                    {format!("📎 {}", fname)}
                                                    <button class="sc-attach-clear" on:click=move |_| {
                                                        set_attach_data.set(None);
                                                    }>"✕"</button>
                                                </span>
                                            }.into_view()
                                        } else { ().into_view() }}
                                        <button
                                            class="sc-send-btn"
                                            class:sc-sending=move || sending.get()
                                            on:click=move |_| {
                                                // Cancel typing indicator on send
                                                typing_timeout.update_value(|t| { *t = None; });
                                                let my_name = identity.get_untracked().name.clone();
                                                let ch = selected_channel.get_untracked();
                                                ws_handle.with_value(|ws_opt| {
                                                    if let Some(ws) = ws_opt {
                                                        let stop = serde_json::json!({"type":"typing","channel":ch,"agent":my_name,"is_typing":false}).to_string();
                                                        let _ = ws.send_with_str(&stop);
                                                    }
                                                });
                                                // If there's an attachment, send message first then upload
                                                if let Some((fname, mime, b64)) = attach_data.get_untracked() {
                                                    // Send the message text (or empty placeholder)
                                                    let text = input_text.get_untracked();
                                                    let ch = selected_channel.get_untracked();
                                                    let user = identity.get_untracked().id.clone();
                                                    let set_att = set_attach_data;
                                                    let set_msgs = set_messages;
                                                    let set_inp = set_input_text;
                                                    spawn_local(async move {
                                                        let msg_text = if text.trim().is_empty() {
                                                            format!("📎 {}", fname)
                                                        } else {
                                                            text
                                                        };
                                                        let payload = serde_json::json!({
                                                            "from": user,
                                                            "text": msg_text,
                                                            "channel": ch,
                                                        });
                                                        if let Ok(req) = gloo_net::http::Request::post("/sc/api/messages").json(&payload) {
                                                            if let Ok(resp) = req.send().await {
                                                                if let Ok(data) = resp.json::<serde_json::Value>().await {
                                                                    let msg_id = data["message"]["id"].as_i64();
                                                                    if let Some(mid) = msg_id {
                                                                        // Upload attachment to the message
                                                                        let att_payload = serde_json::json!({
                                                                            "filename": fname,
                                                                            "mime_type": mime,
                                                                            "content_b64": b64,
                                                                        });
                                                                        let url = format!("/sc/api/messages/{}/attachments", mid);
                                                                        if let Ok(req2) = gloo_net::http::Request::post(&url).json(&att_payload) {
                                                                            let _ = req2.send().await;
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        set_inp.set(String::new());
                                                        set_att.set(None);
                                                    });
                                                } else {
                                                    trigger_send(
                                                        input_text, set_input_text,
                                                        selected_channel, set_messages,
                                                        sending, set_sending, set_mention_query,
                                                        identity,
                                                    );
                                                }
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

            // ── Cmd+K Channel switcher ────────────────────────────────────────
            {move || if show_channel_switcher.get() {
                let all_channels = channels.get();
                let all_dms = dm_channels.get();
                let q = switcher_query.get().to_lowercase();
                let filtered: Vec<_> = all_channels.iter().chain(all_dms.iter())
                    .filter(|c| q.is_empty() || c.name.to_lowercase().contains(&q) || c.id.to_lowercase().contains(&q))
                    .cloned()
                    .collect();
                view! {
                    <div class="sc-switcher-overlay" on:click=move |_| set_show_channel_switcher.set(false)>
                        <div class="sc-switcher" on:click=|ev| ev.stop_propagation()>
                            <div class="sc-switcher-header">"⌘K — Switch Channel"</div>
                            <input
                                type="text"
                                class="sc-switcher-input"
                                placeholder="Type to filter channels..."
                                autofocus=true
                                prop:value=move || switcher_query.get()
                                on:input=move |ev| set_switcher_query.set(event_target_value(&ev))
                            />
                            <div class="sc-switcher-list">
                                {filtered.iter().map(|ch| {
                                    let ch_id = ch.id.clone();
                                    let ch_id2 = ch.id.clone();
                                    let is_dm = ch.channel_type.as_deref() == Some("dm");
                                    let prefix = if is_dm { "💬 " } else { "# " };
                                    let name = ch.name.clone();
                                    view! {
                                        <div class="sc-switcher-item"
                                            class:sc-switcher-active=move || selected_channel.get() == ch_id
                                            on:click=move |_| {
                                                set_selected_channel.set(ch_id2.clone());
                                                set_unread.update(|u| { u.remove(&ch_id2.clone()); });
                                                set_mentions_unread.update(|u| { u.remove(&ch_id2.clone()); });
                                                set_selected_project.set(None);
                                                set_show_channel_switcher.set(false);
                                            }
                                        >
                                            {prefix} {name}
                                        </div>
                                    }
                                }).collect::<Vec<_>>()}
                                {if filtered.is_empty() {
                                    view! { <div class="sc-switcher-empty">"No channels match"</div> }.into_view()
                                } else { ().into_view() }}
                            </div>
                            <div class="sc-switcher-footer">"↑↓ navigate  ↵ select  Esc close"</div>
                        </div>
                    </div>
                }.into_view()
            } else { ().into_view() }}

            // ── Cmd+/ Help modal ──────────────────────────────────────────────
            {move || if show_help.get() {
                view! {
                    <div class="sc-switcher-overlay" on:click=move |_| set_show_help.set(false)>
                        <div class="sc-help-modal" on:click=|ev| ev.stop_propagation()>
                            <div class="sc-switcher-header">"⌘/ — Keyboard Shortcuts"</div>
                            <div class="sc-help-grid">
                                <div class="sc-help-row"><kbd>"⌘K"</kbd><span>"Switch channel"</span></div>
                                <div class="sc-help-row"><kbd>"⌘/"</kbd><span>"Show this help"</span></div>
                                <div class="sc-help-row"><kbd>"Ctrl+Enter"</kbd><span>"Send message"</span></div>
                                <div class="sc-help-row"><kbd>"Esc"</kbd><span>"Close modal / clear search"</span></div>
                            </div>
                            <div class="sc-help-grid sc-help-md">
                                <div class="sc-switcher-header">"Markdown Formatting"</div>
                                <div class="sc-help-row"><kbd>"**text**"</kbd><span><strong>"bold"</strong></span></div>
                                <div class="sc-help-row"><kbd>"*text*"</kbd><span><em>"italic"</em></span></div>
                                <div class="sc-help-row"><kbd>"`code`"</kbd><span><code class="sc-inline-code">"inline code"</code></span></div>
                                <div class="sc-help-row"><kbd>"```lang\\ncode\\n```"</kbd><span>"code block"</span></div>
                                <div class="sc-help-row"><kbd>"[label](url)"</kbd><span>"link"</span></div>
                                <div class="sc-help-row"><kbd>"@name"</kbd><span>"mention"</span></div>
                            </div>
                            <button class="sc-help-close" on:click=move |_| set_show_help.set(false)>"Close"</button>
                        </div>
                    </div>
                }.into_view()
            } else { ().into_view() }}
        </div>
    }
}
