use leptos::*;
use crate::types::{ChatContext, DEFAULT_CHANNELS};

fn ls_remove(key: &str) {
    if let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = s.remove_item(key);
    }
}

#[component]
pub fn Sidebar() -> impl IntoView {
    let ctx = use_context::<ChatContext>().expect("ChatContext missing");

    // Discover extra channels from message subjects not in DEFAULT_CHANNELS
    let extra_channels = move || -> Vec<String> {
        let msgs = ctx.messages.get();
        let defaults: Vec<&str> = DEFAULT_CHANNELS.iter().map(|(id, _)| *id).collect();
        let mut seen = std::collections::HashSet::new();
        let mut extras: Vec<String> = Vec::new();
        for msg in &msgs {
            if let Some(subj) = &msg.subject {
                let s = subj.as_str();
                if !defaults.contains(&s) && seen.insert(s.to_string()) {
                    extras.push(s.to_string());
                }
            }
        }
        extras.sort();
        extras
    };

    view! {
        <div class="chat-sidebar">
            <div class="sidebar-header">
                <span class="sidebar-logo">"🦞"</span>
                <span class="sidebar-title">"ClawChat"</span>
                <span class=move || {
                    if ctx.connected.get() { "conn-dot conn-live" } else { "conn-dot conn-dead" }
                }></span>
            </div>

            <div class="sidebar-section">
                <div class="sidebar-section-label">"CHANNELS"</div>

                // Default channels
                {DEFAULT_CHANNELS.iter().map(|(ch_id, ch_label)| {
                    let id = ch_id.to_string();
                    let lbl = ch_label.to_string();
                    let id_active = id.clone();
                    let id_click = id.clone();
                    let id_unread = id.clone();

                    view! {
                        <button
                            class="channel-item"
                            class:channel-active=move || ctx.active_channel.get() == id_active
                            on:click=move |_| {
                                let ch = id_click.clone();
                                // Mark channel as read
                                let count = ctx.messages.get().iter()
                                    .filter(|m| m.subject.as_deref() == Some(ch.as_str()))
                                    .count();
                                ctx.read_counts.update(|m| { m.insert(ch.clone(), count); });
                                ctx.set_active_channel.set(ch);
                            }
                        >
                            <span class="channel-hash">"#"</span>
                            {lbl}
                            {move || {
                                let count = channel_unread(
                                    ctx.messages.get(),
                                    &id_unread,
                                    ctx.active_channel.get(),
                                    ctx.read_counts.get(),
                                );
                                if count > 0 {
                                    view! { <span class="unread-badge">{count}</span> }.into_view()
                                } else {
                                    view! {}.into_view()
                                }
                            }}
                        </button>
                    }
                }).collect::<Vec<_>>().into_view()}

                // Discovered extra channels (from message history)
                {move || {
                    extra_channels().into_iter().map(|id| {
                        let id_active = id.clone();
                        let id_click = id.clone();
                        let lbl = id.trim_start_matches('#').to_string();
                        view! {
                            <button
                                class="channel-item channel-discovered"
                                class:channel-active=move || ctx.active_channel.get() == id_active
                                on:click=move |_| {
                                    let ch = id_click.clone();
                                    let count = ctx.messages.get().iter()
                                        .filter(|m| m.subject.as_deref() == Some(ch.as_str()))
                                        .count();
                                    ctx.read_counts.update(|m| { m.insert(ch.clone(), count); });
                                    ctx.set_active_channel.set(ch);
                                }
                            >
                                <span class="channel-hash">"#"</span>
                                {lbl}
                            </button>
                        }
                    }).collect::<Vec<_>>().into_view()
                }}
            </div>

            <div class="sidebar-section sidebar-presence">
                <div class="sidebar-section-label">"AGENTS"</div>
                {move || {
                    let p = ctx.presence.get();
                    if p.is_empty() {
                        return view! {
                            <div class="presence-empty">"no heartbeats yet"</div>
                        }.into_view();
                    }
                    p.into_iter().map(|e| {
                        let dot = if e.online { "presence-dot online" } else { "presence-dot offline" };
                        view! {
                            <div class="presence-item">
                                <span class=dot></span>
                                <span class="presence-name">{e.agent}</span>
                            </div>
                        }
                    }).collect::<Vec<_>>().into_view()
                }}
            </div>

            <div class="sidebar-footer">
                <span class="sidebar-footer-user">{move || ctx.username.get()}</span>
                <button class="sidebar-signout-btn" on:click=move |_| {
                    ls_remove("ccc_token");
                    ls_remove("ccc_username");
                    ctx.set_token.set(None);
                }>"Sign out"</button>
            </div>
        </div>
    }
}

/// Count unread messages in a channel.
/// Unread = current_count - read_watermark. Defaults to 0 if channel was never visited.
fn channel_unread(
    msgs: Vec<crate::types::BusMessage>,
    channel: &str,
    active: String,
    read_counts: std::collections::HashMap<String, usize>,
) -> usize {
    if channel == active.as_str() {
        return 0;
    }
    let current = msgs.iter().filter(|m| m.subject.as_deref() == Some(channel)).count();
    let watermark = read_counts.get(channel).copied().unwrap_or(current);
    current.saturating_sub(watermark).min(99)
}
