use leptos::*;
use wasm_bindgen::prelude::*;

use crate::types::BusMessage;

#[component]
pub fn SquirrelBus() -> impl IntoView {
    let (messages, set_messages) = create_signal(Vec::<BusMessage>::new());
    let (connected, set_connected) = create_signal(false);

    // Set up SSE connection for live updates
    {
        let set_msgs = set_messages;
        let set_conn = set_connected;

        if let Ok(es) = web_sys::EventSource::new("/bus/stream") {
            let es_clone = es.clone();

            // onopen
            let open_cb = Closure::<dyn FnMut()>::new(move || {
                set_conn.set(true);
            });
            es.set_onopen(Some(open_cb.as_ref().unchecked_ref()));
            open_cb.forget();

            // onmessage
            let msg_cb = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
                let data = e.data().as_string().unwrap_or_default();
                // Skip SSE comment lines (start with ':')
                if data.starts_with(':') || data.is_empty() {
                    return;
                }
                if let Ok(msg) = serde_json::from_str::<BusMessage>(&data) {
                    set_msgs.update(|msgs| {
                        msgs.insert(0, msg);
                        msgs.truncate(100); // keep last 100 messages
                    });
                }
            });
            es.set_onmessage(Some(msg_cb.as_ref().unchecked_ref()));
            msg_cb.forget();

            // onerror
            let err_cb = Closure::<dyn FnMut(_)>::new(move |_: web_sys::ErrorEvent| {
                set_conn.set(false);
            });
            es.set_onerror(Some(err_cb.as_ref().unchecked_ref()));
            err_cb.forget();

            // Close on component cleanup
            on_cleanup(move || {
                es_clone.close();
            });
        }
    }

    view! {
        <section class="section section-bus">
            <div class="section-header">
                <h2 class="section-title">
                    <span class="section-icon">"⇄"</span>
                    "SquirrelBus"
                </h2>
                <div class="bus-status">
                    {move || {
                        if connected.get() {
                            view! { <span class="conn-badge conn-live">"● live"</span> }
                                .into_view()
                        } else {
                            view! { <span class="conn-badge conn-waiting">"○ waiting"</span> }
                                .into_view()
                        }
                    }}
                    {move || {
                        let n = messages.get().len();
                        view! {
                            <span class="badge">
                                {n}
                                " msgs"
                            </span>
                        }
                    }}
                </div>
            </div>

            <div class="bus-stream">
                {move || {
                    let msgs = messages.get();
                    if msgs.is_empty() {
                        return view! {
                            <div class="bus-empty">"No messages yet — waiting for stream..."</div>
                        }
                            .into_view();
                    }
                    msgs.into_iter()
                        .map(|msg| {
                            let from = msg.from.clone().unwrap_or_else(|| "?".to_string());
                            let to = msg
                                .to
                                .clone()
                                .map(|t| format!(" → {t}"))
                                .unwrap_or_default();
                            let text = msg.text.clone().unwrap_or_default();
                            let ts = msg
                                .ts
                                .as_deref()
                                .and_then(|t| t.split('T').nth(1))
                                .and_then(|t| t.split('.').next())
                                .unwrap_or("")
                                .to_string();
                            let mtype = msg.msg_type.clone().unwrap_or_default();
                            view! {
                                <div class=format!("bus-msg msg-type-{mtype}")>
                                    <span class="msg-ts">{ts}</span>
                                    <span class="msg-from">
                                        {from}
                                        {to}
                                    </span>
                                    <span class="msg-text">{text}</span>
                                </div>
                            }
                        })
                        .collect::<Vec<_>>()
                        .into_view()
                }}
            </div>
        </section>
    }
}
