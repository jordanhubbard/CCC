use leptos::*;

#[component]
pub fn BusSend() -> impl IntoView {
    let (to_agent, set_to_agent) = create_signal("natasha".to_string());
    let (msg_type, set_msg_type) = create_signal("chat".to_string());
    let (message, set_message) = create_signal(String::new());
    let (toast, set_toast) = create_signal(Option::<(bool, String)>::None);

    view! {
        <section class="section section-bus-send">
            <h2 class="section-title">
                <span class="section-icon">"✉"</span>
                "Bus Send"
            </h2>
            {move || toast.get().map(|(ok, msg)| {
                let cls = if ok { "toast toast-ok" } else { "toast toast-err" };
                view! { <div class=cls>{msg}</div> }
            })}
            <div class="bus-send-form">
                <div class="form-row">
                    <label class="form-label">"To"</label>
                    <select
                        class="form-select"
                        on:change=move |e| set_to_agent.set(event_target_value(&e))
                    >
                        <option value="natasha">"natasha"</option>
                        <option value="rocky">"rocky"</option>
                        <option value="bullwinkle">"bullwinkle"</option>
                        <option value="boris">"boris"</option>
                        <option value="all">"all"</option>
                    </select>
                </div>
                <div class="form-row">
                    <label class="form-label">"Type"</label>
                    <select
                        class="form-select"
                        on:change=move |e| set_msg_type.set(event_target_value(&e))
                    >
                        <option value="chat">"chat"</option>
                        <option value="task">"task"</option>
                        <option value="ping">"ping"</option>
                        <option value="status">"status"</option>
                    </select>
                </div>
                <div class="form-row">
                    <textarea
                        class="form-textarea"
                        placeholder="Message..."
                        prop:value=move || message.get()
                        on:input=move |e| set_message.set(event_target_value(&e))
                    />
                </div>
                <button
                    class="btn-send"
                    on:click=move |_| {
                        let to = to_agent.get();
                        let mtype = msg_type.get();
                        let text = message.get();
                        if text.trim().is_empty() {
                            return;
                        }
                        let body = serde_json::json!({
                            "to": to,
                            "type": mtype,
                            "text": text,
                        }).to_string();
                        spawn_local(async move {
                            let result = gloo_net::http::Request::post("/bus/send")
                                .header("Authorization", env!("RCC_AUTH_TOKEN", "<YOUR_RCC_TOKEN>"))
                                .header("Content-Type", "application/json")
                                .body(body)
                                .expect("body")
                                .send()
                                .await;
                            match result {
                                Ok(resp) if resp.ok() => {
                                    set_toast.set(Some((true, "Message sent!".to_string())));
                                    set_message.set(String::new());
                                }
                                Ok(resp) => {
                                    set_toast.set(Some((false, format!("Error: HTTP {}", resp.status()))));
                                }
                                Err(e) => {
                                    set_toast.set(Some((false, format!("Failed: {e}"))));
                                }
                            }
                            gloo_timers::future::TimeoutFuture::new(3_000).await;
                            set_toast.set(None);
                        });
                    }
                >"Send"</button>
            </div>
        </section>
    }
}
