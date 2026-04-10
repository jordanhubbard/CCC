use leptos::*;
use wasm_bindgen_futures::spawn_local;
use crate::types::{BusMessage, ChatContext};

#[component]
pub fn InputBar() -> impl IntoView {
    let ctx = use_context::<ChatContext>().expect("ChatContext missing");
    let (text, set_text) = create_signal(String::new());
    let (sending, set_sending) = create_signal(false);

    let do_send = move || {
        let body = text.get();
        if body.trim().is_empty() || sending.get() {
            return;
        }
        let channel = ctx.active_channel.get();
        let from = ctx.username.get();
        let tok = ctx.token.get().unwrap_or_default();
        let set_msgs = ctx.set_messages;

        set_text.set(String::new());
        set_sending.set(true);

        spawn_local(async move {
            let payload = serde_json::json!({
                "from": from,
                "to": "all",
                "type": "text",
                "subject": channel,
                "body": body,
                "mime": "text/plain",
            });

            let result = gloo_net::http::Request::post("/bus/send")
                .header("Authorization", &format!("Bearer {tok}"))
                .header("Content-Type", "application/json")
                .body(payload.to_string())
                .unwrap()
                .send()
                .await;

            // Optimistically append if the SSE echo hasn't arrived yet
            if let Ok(resp) = result {
                if resp.ok() {
                    if let Ok(msg) = resp.json::<BusMessage>().await {
                        set_msgs.update(|v| v.push(msg));
                    }
                }
            }

            set_sending.set(false);
        });
    };

    view! {
        <div class="input-bar">
            <div class="input-channel-label">
                "#"
                {move || ctx.active_channel.get().trim_start_matches('#').to_string()}
            </div>
            <div class="input-row">
                <textarea
                    class="input-text"
                    placeholder=move || format!("Message #{}", ctx.active_channel.get().trim_start_matches('#'))
                    prop:value=text
                    attr:disabled=move || if sending.get() { Some("disabled") } else { None }
                    on:input=move |e| set_text.set(event_target_value(&e))
                    on:keydown=move |e| {
                        if e.key() == "Enter" && !e.shift_key() {
                            e.prevent_default();
                            do_send();
                        }
                    }
                />
                <button
                    class="send-btn"
                    attr:disabled=move || if sending.get() { Some("disabled") } else { None }
                    on:click=move |_| do_send()
                >
                    {move || if sending.get() { "…" } else { "Send" }}
                </button>
            </div>
        </div>
    }
}
