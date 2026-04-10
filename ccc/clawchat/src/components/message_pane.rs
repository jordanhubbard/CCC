use leptos::*;
use leptos::html::Div;
use crate::types::{BusMessage, ChatContext};

/// Format an ISO8601 timestamp to HH:MM.
fn fmt_time(ts: &str) -> String {
    ts.split('T')
        .nth(1)
        .and_then(|t| t.split('.').next())
        .and_then(|t| t.get(..5))
        .unwrap_or(ts)
        .to_string()
}

/// Format ISO8601 to a date header (YYYY-MM-DD).
fn fmt_date(ts: &str) -> String {
    ts.split('T').next().unwrap_or(ts).to_string()
}

#[component]
pub fn MessagePane() -> impl IntoView {
    let ctx = use_context::<ChatContext>().expect("ChatContext missing");
    let list_ref = create_node_ref::<Div>();

    // Auto-scroll to bottom whenever messages change.
    create_effect(move |_| {
        let _ = ctx.messages.get(); // reactive dependency
        let _ = ctx.active_channel.get();
        if let Some(el) = list_ref.get() {
            el.set_scroll_top(el.scroll_height());
        }
    });

    // Messages filtered to active channel
    let channel_msgs = move || -> Vec<BusMessage> {
        let ch = ctx.active_channel.get();
        ctx.messages
            .get()
            .into_iter()
            .filter(|m| m.subject.as_deref() == Some(ch.as_str()))
            .collect()
    };

    view! {
        <div class="message-pane">
            <div class="channel-header">
                <span class="channel-header-hash">"#"</span>
                <span class="channel-header-name">
                    {move || ctx.active_channel.get().trim_start_matches('#').to_string()}
                </span>
                <span class="channel-header-count">
                    {move || {
                        let n = channel_msgs().len();
                        format!("{n} message{}", if n == 1 { "" } else { "s" })
                    }}
                </span>
            </div>

            <div class="message-list" node_ref=list_ref>
                {move || {
                    let msgs = channel_msgs();
                    if msgs.is_empty() {
                        return view! {
                            <div class="messages-empty">
                                <span class="messages-empty-icon">"💬"</span>
                                <p>"No messages yet. Be the first to say something."</p>
                            </div>
                        }.into_view();
                    }

                    let mut rendered: Vec<leptos::View> = Vec::new();
                    let mut last_date = String::new();
                    let mut last_author = String::new();

                    for msg in msgs {
                        let ts = msg.ts.clone().unwrap_or_default();
                        let date = fmt_date(&ts);
                        let author = msg.from.clone().unwrap_or_else(|| "?".to_string());
                        let body = msg.body.clone().unwrap_or_default();
                        let time = fmt_time(&ts);

                        // Date divider
                        if date != last_date {
                            let d = date.clone();
                            rendered.push(view! {
                                <div class="date-divider">
                                    <span class="date-divider-line"></span>
                                    <span class="date-divider-label">{d}</span>
                                    <span class="date-divider-line"></span>
                                </div>
                            }.into_view());
                            last_date = date;
                            last_author = String::new(); // force author header after date break
                        }

                        // Group consecutive messages from same author
                        let show_header = author != last_author;
                        last_author = author.clone();

                        if show_header {
                            let a = author.clone();
                            let t = time.clone();
                            rendered.push(view! {
                                <div class="msg-group">
                                    <div class="msg-header">
                                        <span class="msg-avatar">{a.chars().next().unwrap_or('?').to_uppercase().to_string()}</span>
                                        <span class="msg-author">{a}</span>
                                        <span class="msg-time">{t}</span>
                                    </div>
                                    <div class="msg-body">{body}</div>
                                </div>
                            }.into_view());
                        } else {
                            rendered.push(view! {
                                <div class="msg-continuation">
                                    <span class="msg-continuation-time">{time}</span>
                                    <div class="msg-body">{body}</div>
                                </div>
                            }.into_view());
                        }
                    }

                    rendered.into_view()
                }}
            </div>
        </div>
    }
}
