// sc_thread.rs — Thread panel component for ClawChat
// Bullwinkle (Track A UI) — shows thread replies in a side panel

use leptos::*;
use crate::components::sc_types::ScMessage;

// ─── Thread Panel ────────────────────────────────────────────────────────────

/// Side panel showing thread replies for a selected message.
/// `parent` is the root message. `replies` are fetched from `/api/messages/:id/thread`.
/// `on_close` hides the panel. `on_reply` sends a new reply.
#[component]
pub fn ThreadPanel(
    parent: ScMessage,
    replies: ReadSignal<Vec<ScMessage>>,
    on_close: Callback<()>,
    on_reply: Callback<(i64, String)>,
) -> impl IntoView {
    let (reply_text, set_reply_text) = create_signal(String::new());
    let parent_id = parent.id.unwrap_or(0);
    let parent_from = parent.from_agent.clone().unwrap_or_else(|| "?".to_string());
    let parent_text = parent.text.clone().unwrap_or_default();
    let parent_ts = if parent.ts > 0 { parent.format_ts() } else { String::new() };

    view! {
        <div class="sc-thread-panel">
            <div class="sc-thread-header">
                <span class="sc-thread-title">"Thread"</span>
                <button class="sc-close-btn" on:click=move |_| on_close.call(())>"✕"</button>
            </div>

            // Parent message
            <div class="sc-thread-parent">
                <div class="sc-msg-header">
                    <span class="sc-msg-from">{parent_from}</span>
                    <span class="sc-msg-ts">{parent_ts}</span>
                </div>
                <div class="sc-msg-content">{parent_text}</div>
                <div class="sc-thread-reply-count">
                    {move || {
                        let count = replies.get().len();
                        if count == 0 {
                            "No replies yet".to_string()
                        } else {
                            format!("{} {}", count, if count == 1 { "reply" } else { "replies" })
                        }
                    }}
                </div>
            </div>

            <div class="sc-thread-divider" />

            // Replies
            <div class="sc-thread-replies">
                {move || {
                    replies.get().into_iter().map(|msg| {
                        let from = msg.from_agent.clone().unwrap_or_else(|| "?".to_string());
                        let text = msg.text.clone().unwrap_or_default();
                        let ts = if msg.ts > 0 { msg.format_ts() } else { String::new() };
                        view! {
                            <div class="sc-msg sc-thread-msg">
                                <div class="sc-msg-header">
                                    <span class="sc-msg-from">{from}</span>
                                    <span class="sc-msg-ts">{ts}</span>
                                </div>
                                <div class="sc-msg-content">{text}</div>
                            </div>
                        }
                    }).collect::<Vec<_>>()
                }}
            </div>

            // Reply input
            <div class="sc-thread-input">
                <textarea
                    class="sc-textarea sc-thread-textarea"
                    placeholder="Reply in thread..."
                    prop:value=move || reply_text.get()
                    on:input=move |ev| set_reply_text.set(event_target_value(&ev))
                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                        if ev.key() == "Enter" && ev.ctrl_key() {
                            ev.prevent_default();
                            let text = reply_text.get_untracked().trim().to_string();
                            if !text.is_empty() {
                                on_reply.call((parent_id, text));
                                set_reply_text.set(String::new());
                            }
                        }
                    }
                />
                <button
                    class="sc-send-btn sc-thread-send"
                    on:click=move |_| {
                        let text = reply_text.get_untracked().trim().to_string();
                        if !text.is_empty() {
                            on_reply.call((parent_id, text));
                            set_reply_text.set(String::new());
                        }
                    }
                >"Reply"</button>
            </div>
        </div>
    }
}
