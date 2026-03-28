use leptos::*;
use wasm_bindgen::prelude::*;
use std::collections::HashMap;

use crate::types::{BusMessage, HeartbeatMap};

const BUS_URL: &str = "http://100.89.199.14:8788/bus/send";
const BUS_TOKEN: &str = "wq-5dcad756f6d3e345c00b5cb3dfcbdedb";

#[derive(Clone, Debug, PartialEq)]
struct ChatMessage {
    id: String,
    from: String,
    to: String,
    text: String,
    ts_raw: String,
    reactions: HashMap<String, u32>,
}

fn agent_icon(name: &str) -> &'static str {
    match name.to_lowercase().as_str() {
        "natasha" => "🦊",
        "rocky"   => "🐿️",
        "bullwinkle" => "🫎",
        "boris"   => "🕵️",
        _ => "🤖",
    }
}

fn relative_time(ts: &str) -> String {
    // ts is like "2024-01-01T12:34:56.789Z"
    // We do a best-effort parse to extract seconds ago via JS Date
    let js_now = js_sys::Date::now(); // ms since epoch
    let js_ts = js_sys::Date::parse(ts); // ms since epoch
    if js_ts.is_nan() || js_ts <= 0.0 {
        // fallback: show time portion
        return ts.split('T').nth(1)
            .and_then(|t| t.split('.').next())
            .unwrap_or(ts)
            .to_string();
    }
    let diff_secs = ((js_now - js_ts) / 1000.0) as i64;
    if diff_secs < 60 {
        format!("{}s ago", diff_secs.max(0))
    } else if diff_secs < 3600 {
        format!("{}m ago", diff_secs / 60)
    } else if diff_secs < 86400 {
        format!("{}h ago", diff_secs / 3600)
    } else {
        format!("{}d ago", diff_secs / 86400)
    }
}

fn highlight_mentions(text: &str) -> Vec<(String, bool)> {
    // Split text into (segment, is_mention) pairs
    let mut parts = Vec::new();
    let mut remaining = text;
    while let Some(at) = remaining.find('@') {
        if at > 0 {
            parts.push((remaining[..at].to_string(), false));
        }
        let rest = &remaining[at..];
        let end = rest[1..].find(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        parts.push((rest[..end].to_string(), true));
        remaining = &rest[end..];
    }
    if !remaining.is_empty() {
        parts.push((remaining.to_string(), false));
    }
    parts
}

async fn fetch_agents() -> HeartbeatMap {
    let Ok(resp) = gloo_net::http::Request::get("/api/agents").send().await else {
        return HeartbeatMap::default();
    };
    resp.json::<HeartbeatMap>().await.unwrap_or_default()
}

async fn send_message(from: String, to: String, text: String) -> bool {
    let body = serde_json::json!({
        "from": from,
        "to": to,
        "text": text,
        "type": "chat"
    });
    let Ok(resp) = gloo_net::http::Request::post(BUS_URL)
        .header("Authorization", &format!("Bearer {BUS_TOKEN}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .unwrap()
        .send()
        .await
    else {
        return false;
    };
    resp.ok()
}

const CHANNELS: &[&str] = &["#general", "#agents", "#ops"];
const DMS: &[&str] = &["natasha", "rocky", "bullwinkle", "boris"];
const REACTION_EMOJIS: &[&str] = &["👍", "❤️", "🔥", "🎉", "😂"];

#[component]
pub fn SquirrelChat() -> impl IntoView {
    let (messages, set_messages) = create_signal(Vec::<ChatMessage>::new());
    let (connected, set_connected) = create_signal(false);
    let (active_channel, set_active_channel) = create_signal("#general".to_string());
    let (input_text, set_input_text) = create_signal(String::new());
    let (picker_open_for, set_picker_open_for) = create_signal(Option::<String>::None);
    let (tick, set_tick) = create_signal(0u32);

    // Poll agents every 30s for presence dots
    {
        let set_t = set_tick;
        leptos::spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(30_000).await;
                set_t.update(|t| *t = t.wrapping_add(1));
            }
        });
    }
    let agents = create_resource(move || tick.get(), |_| fetch_agents());

    // SSE for live bus messages
    {
        let set_msgs = set_messages;
        let set_conn = set_connected;
        if let Ok(es) = web_sys::EventSource::new("/bus/stream") {
            let es_clone = es.clone();

            let open_cb = Closure::<dyn FnMut()>::new(move || set_conn.set(true));
            es.set_onopen(Some(open_cb.as_ref().unchecked_ref()));
            open_cb.forget();

            let msg_cb = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
                let data = e.data().as_string().unwrap_or_default();
                if data.starts_with(':') || data.is_empty() { return; }
                if let Ok(msg) = serde_json::from_str::<BusMessage>(&data) {
                    // Only show chat-type messages
                    let mtype = msg.msg_type.as_deref().unwrap_or("");
                    if mtype != "chat" && !mtype.is_empty() { return; }
                    let cm = ChatMessage {
                        id: msg.id.clone().unwrap_or_else(|| {
                            format!("{}", js_sys::Date::now() as u64)
                        }),
                        from: msg.from.clone().unwrap_or_else(|| "?".to_string()),
                        to: msg.to.clone().unwrap_or_else(|| "#general".to_string()),
                        text: msg.text.clone().unwrap_or_default(),
                        ts_raw: msg.ts.clone().unwrap_or_default(),
                        reactions: HashMap::new(),
                    };
                    set_msgs.update(|msgs| {
                        msgs.push(cm);
                        if msgs.len() > 200 { msgs.drain(0..100); }
                    });
                }
            });
            es.set_onmessage(Some(msg_cb.as_ref().unchecked_ref()));
            msg_cb.forget();

            let err_cb = Closure::<dyn FnMut(_)>::new(move |_: web_sys::ErrorEvent| {
                set_conn.set(false);
            });
            es.set_onerror(Some(err_cb.as_ref().unchecked_ref()));
            err_cb.forget();

            on_cleanup(move || es_clone.close());
        }
    }

    let on_send = move || {
        let text = input_text.get();
        let text = text.trim().to_string();
        if text.is_empty() { return; }
        let channel = active_channel.get();
        set_input_text.set(String::new());
        leptos::spawn_local(async move {
            send_message("natasha".to_string(), channel, text).await;
        });
    };

    let on_send_clone = on_send.clone();

    view! {
        <div class="chat-layout">
            // ── Sidebar ──────────────────────────────────────────────────────
            <aside class="chat-sidebar">
                <div class="chat-sidebar-section">
                    <div class="chat-sidebar-label">"Channels"</div>
                    {CHANNELS.iter().map(|ch| {
                        let ch = *ch;
                        let ch_str = ch.to_string();
                        view! {
                            <div
                                class="chat-channel-item"
                                class:chat-channel-active=move || active_channel.get() == ch_str
                                on:click=move |_| set_active_channel.set(ch.to_string())
                            >
                                <span class="chat-ch-name">{ch}</span>
                            </div>
                        }
                    }).collect::<Vec<_>>()}
                </div>
                <div class="chat-sidebar-section">
                    <div class="chat-sidebar-label">"Direct Messages"</div>
                    {DMS.iter().map(|dm| {
                        let dm = *dm;
                        let dm_str = dm.to_string();
                        let icon = agent_icon(dm);
                        view! {
                            <div
                                class="chat-channel-item"
                                class:chat-channel-active=move || active_channel.get() == dm_str
                                on:click=move |_| set_active_channel.set(dm.to_string())
                            >
                                <span class="chat-presence-dot"
                                    class:dot-online=move || {
                                        agents.get()
                                            .and_then(|a| a.get(dm).map(|h| h.online.unwrap_or(false)))
                                            .unwrap_or(false)
                                    }
                                    class:dot-offline=move || {
                                        !agents.get()
                                            .and_then(|a| a.get(dm).map(|h| h.online.unwrap_or(false)))
                                            .unwrap_or(false)
                                    }
                                ></span>
                                <span class="chat-dm-icon">{icon}</span>
                                <span class="chat-ch-name">{dm}</span>
                            </div>
                        }
                    }).collect::<Vec<_>>()}
                </div>
            </aside>

            // ── Main ─────────────────────────────────────────────────────────
            <div class="chat-main">
                <div class="chat-header">
                    <span class="chat-header-channel">{move || active_channel.get()}</span>
                    <span class="chat-conn-status">
                        {move || if connected.get() {
                            view! { <span class="conn-badge conn-live">"● live"</span> }.into_view()
                        } else {
                            view! { <span class="conn-badge conn-waiting">"○ connecting"</span> }.into_view()
                        }}
                    </span>
                </div>

                <div class="chat-messages" id="chat-messages-scroll">
                    {move || {
                        let ch = active_channel.get();
                        let msgs = messages.get();
                        let filtered: Vec<_> = msgs.iter()
                            .filter(|m| m.to == ch || m.from == ch)
                            .cloned()
                            .collect();

                        if filtered.is_empty() {
                            return view! {
                                <div class="chat-empty">"No messages yet in " {ch}</div>
                            }.into_view();
                        }

                        filtered.into_iter().map(|msg| {
                            let msg_id = msg.id.clone();
                            let msg_id2 = msg.id.clone();
                            let icon = agent_icon(&msg.from);
                            let rel_ts = relative_time(&msg.ts_raw);
                            let parts = highlight_mentions(&msg.text);
                            let reactions = msg.reactions.clone();

                            view! {
                                <div
                                    class="chat-msg"
                                    on:click=move |_| {
                                        let id = msg_id.clone();
                                        set_picker_open_for.update(|cur| {
                                            if cur.as_deref() == Some(&id) {
                                                *cur = None;
                                            } else {
                                                *cur = Some(id);
                                            }
                                        });
                                    }
                                >
                                    <span class="chat-msg-icon">{icon}</span>
                                    <div class="chat-msg-body">
                                        <div class="chat-msg-meta">
                                            <span class="chat-msg-sender">{msg.from.clone()}</span>
                                            <span class="chat-msg-ts">{rel_ts}</span>
                                        </div>
                                        <div class="chat-msg-text">
                                            {parts.into_iter().map(|(seg, is_mention)| {
                                                if is_mention {
                                                    view! { <strong class="chat-mention">{seg}</strong> }.into_view()
                                                } else {
                                                    view! { <span>{seg}</span> }.into_view()
                                                }
                                            }).collect::<Vec<_>>()}
                                        </div>
                                        {if !reactions.is_empty() {
                                            view! {
                                                <div class="chat-reactions">
                                                    {reactions.into_iter().map(|(emoji, count)| {
                                                        view! {
                                                            <span class="chat-reaction-pill">
                                                                {emoji} " " {count}
                                                            </span>
                                                        }
                                                    }).collect::<Vec<_>>()}
                                                </div>
                                            }.into_view()
                                        } else {
                                            view! { <span></span> }.into_view()
                                        }}

                                        // Emoji picker
                                        {move || {
                                            let open = picker_open_for.get()
                                                .map(|id| id == msg_id2)
                                                .unwrap_or(false);
                                            if !open { return view! { <span></span> }.into_view(); }
                                            let msg_id3 = msg_id2.clone();
                                            view! {
                                                <div class="chat-emoji-picker">
                                                    {REACTION_EMOJIS.iter().map(|em| {
                                                        let em = *em;
                                                        let mid = msg_id3.clone();
                                                        view! {
                                                            <button
                                                                class="chat-emoji-btn"
                                                                on:click=move |e| {
                                                                    e.stop_propagation();
                                                                    let emoji = em.to_string();
                                                                    let id = mid.clone();
                                                                    set_messages.update(|msgs| {
                                                                        if let Some(m) = msgs.iter_mut().find(|m| m.id == id) {
                                                                            *m.reactions.entry(emoji).or_insert(0) += 1;
                                                                        }
                                                                    });
                                                                    set_picker_open_for.set(None);
                                                                }
                                                            >{em}</button>
                                                        }
                                                    }).collect::<Vec<_>>()}
                                                </div>
                                            }.into_view()
                                        }}
                                    </div>
                                </div>
                            }
                        }).collect::<Vec<_>>().into_view()
                    }}
                </div>

                <div class="chat-input-area">
                    <textarea
                        class="chat-input"
                        placeholder=move || format!("Message {}", active_channel.get())
                        prop:value=move || input_text.get()
                        on:input=move |e| {
                            use wasm_bindgen::JsCast;
                            let el = e.target()
                                .and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok());
                            if let Some(el) = el {
                                set_input_text.set(el.value());
                            }
                        }
                        on:keydown=move |e| {
                            let key = e.key();
                            let shift = e.shift_key();
                            let ctrl = e.ctrl_key();
                            if key == "Enter" && !shift {
                                e.prevent_default();
                                on_send();
                            } else if key == "Enter" && ctrl {
                                e.prevent_default();
                                on_send_clone();
                            }
                        }
                    ></textarea>
                    <button
                        class="btn chat-send-btn"
                        on:click=move |_| {
                            let text = input_text.get();
                            let text = text.trim().to_string();
                            if text.is_empty() { return; }
                            let channel = active_channel.get();
                            set_input_text.set(String::new());
                            leptos::spawn_local(async move {
                                send_message("natasha".to_string(), channel, text).await;
                            });
                        }
                    >"Send"</button>
                </div>
            </div>
        </div>
    }
}
