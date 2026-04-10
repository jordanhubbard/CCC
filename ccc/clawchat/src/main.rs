mod types;
mod components;

use leptos::*;
use types::*;
use components::{login::LoginScreen, sidebar::Sidebar, message_pane::MessagePane, input_bar::InputBar};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

fn ls_get(key: &str) -> Option<String> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(key).ok().flatten())
        .filter(|v| !v.is_empty())
}

fn ls_set(key: &str, val: &str) {
    if let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = s.set_item(key, val);
    }
}

const LS_KEY_TOKEN: &str = "ccc_token";
const LS_KEY_USER: &str = "ccc_username";

#[component]
fn App() -> impl IntoView {
    let (token, set_token) = create_signal(ls_get(LS_KEY_TOKEN));
    let (username, set_username) = create_signal(
        ls_get(LS_KEY_USER).unwrap_or_else(|| "jkh".to_string()),
    );

    let on_login = move |(tok, user): (String, String)| {
        ls_set(LS_KEY_TOKEN, &tok);
        ls_set(LS_KEY_USER, &user);
        set_token.set(Some(tok));
        set_username.set(user);
    };

    view! {
        {move || {
            match token.get() {
                None => view! {
                    <LoginScreen on_login=on_login.clone() />
                }.into_view(),
                Some(_) => view! {
                    <ChatApp token=token username=username />
                }.into_view(),
            }
        }}
    }
}

#[component]
fn ChatApp(
    token: ReadSignal<Option<String>>,
    username: ReadSignal<String>,
) -> impl IntoView {
    let (active_channel, set_active_channel) = create_signal("#general".to_string());
    let (messages, set_messages) = create_signal(Vec::<BusMessage>::new());
    let (presence, set_presence) = create_signal(Vec::<PresenceEntry>::new());
    let (connected, set_connected) = create_signal(false);

    // Load message history on mount
    {
        let tok = token.get_untracked().unwrap_or_default();
        let set_msgs = set_messages;
        spawn_local(async move {
            let r = gloo_net::http::Request::get("/bus/messages?type=text&limit=500")
                .header("Authorization", &format!("Bearer {tok}"))
                .send()
                .await;
            if let Ok(resp) = r {
                if let Ok(msgs) = resp.json::<Vec<BusMessage>>().await {
                    set_msgs.set(msgs);
                }
            }
        });
    }

    // Load presence on mount
    {
        let tok = token.get_untracked().unwrap_or_default();
        let set_p = set_presence;
        spawn_local(async move {
            let r = gloo_net::http::Request::get("/bus/presence")
                .header("Authorization", &format!("Bearer {tok}"))
                .send()
                .await;
            if let Ok(resp) = r {
                if let Ok(val) = resp.json::<serde_json::Value>().await {
                    if let Some(obj) = val.as_object() {
                        let entries: Vec<PresenceEntry> = obj
                            .iter()
                            .map(|(name, info)| {
                                // online if status == "online" and last_seen < 10min
                                let status = info
                                    .get("status")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("offline");
                                PresenceEntry {
                                    agent: name.clone(),
                                    online: status == "online",
                                }
                            })
                            .collect();
                        set_p.set(entries);
                    }
                }
            }
        });
    }

    // SSE stream for real-time messages
    {
        let set_msgs = set_messages;
        let set_conn = set_connected;

        if let Ok(es) = web_sys::EventSource::new("/bus/stream") {
            let es_cleanup = es.clone();

            let open_cb = Closure::<dyn FnMut()>::new(move || {
                set_conn.set(true);
            });
            es.set_onopen(Some(open_cb.as_ref().unchecked_ref()));
            open_cb.forget();

            let msg_cb = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
                let data = e.data().as_string().unwrap_or_default();
                if data.starts_with(':') || data.is_empty() {
                    return;
                }
                if let Ok(msg) = serde_json::from_str::<BusMessage>(&data) {
                    if msg.msg_type.as_deref() == Some("text") {
                        set_msgs.update(|v| v.push(msg));
                    }
                }
            });
            es.set_onmessage(Some(msg_cb.as_ref().unchecked_ref()));
            msg_cb.forget();

            let err_cb = Closure::<dyn FnMut(_)>::new(move |_: web_sys::ErrorEvent| {
                set_conn.set(false);
            });
            es.set_onerror(Some(err_cb.as_ref().unchecked_ref()));
            err_cb.forget();

            on_cleanup(move || es_cleanup.close());
        }
    }

    let ctx = ChatContext {
        token,
        username,
        messages,
        set_messages,
        active_channel,
        set_active_channel,
        presence,
        connected,
    };
    provide_context(ctx);

    view! {
        <div class="clawchat-app">
            <Sidebar />
            <div class="chat-main">
                <MessagePane />
                <InputBar />
            </div>
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(|| view! { <App /> });
}
