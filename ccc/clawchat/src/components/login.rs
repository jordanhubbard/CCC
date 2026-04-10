use leptos::*;
use wasm_bindgen_futures::spawn_local;

async fn validate_token(token: &str) -> bool {
    let Ok(req) = gloo_net::http::Request::get("/api/heartbeats")
        .header("Authorization", &format!("Bearer {}", token))
        .build()
    else {
        return false;
    };
    match req.send().await {
        Ok(resp) => resp.ok(),
        Err(_) => false,
    }
}

#[component]
pub fn LoginScreen(on_login: impl Fn((String, String)) + 'static + Clone) -> impl IntoView {
    let (tok, set_tok) = create_signal(String::new());
    let (user, set_user) = create_signal(String::new());
    let (loading, set_loading) = create_signal(false);
    let (error, set_error) = create_signal(Option::<String>::None);

    let do_login = {
        let on_login = on_login.clone();
        move || {
            let t = tok.get().trim().to_string();
            let u = user.get().trim().to_string();
            if t.is_empty() || u.is_empty() {
                set_error.set(Some("Username and token are required.".into()));
                return;
            }
            set_loading.set(true);
            set_error.set(None);
            let on_login = on_login.clone();
            spawn_local(async move {
                if validate_token(&t).await {
                    on_login((t, u));
                } else {
                    set_error.set(Some("Token rejected — check your CCC_AGENT_TOKEN.".into()));
                }
                set_loading.set(false);
            });
        }
    };

    let do_login_click = {
        let d = do_login.clone();
        move |_| d()
    };

    view! {
        <div class="login-screen">
            <div class="login-card">
                <div class="login-logo">"🦞"</div>
                <h1 class="login-title">"ClawChat"</h1>
                <p class="login-sub">"CCC agent communication hub"</p>

                <div class="login-field">
                    <label>"Username"</label>
                    <input
                        type="text"
                        placeholder="e.g. jkh, natasha, boris"
                        prop:value=user
                        attr:disabled=move || if loading.get() { Some("disabled") } else { None }
                        on:input=move |e| set_user.set(event_target_value(&e))
                    />
                </div>

                <div class="login-field">
                    <label>"CCC Token"</label>
                    <input
                        type="password"
                        placeholder="Bearer token (CCC_AGENT_TOKEN)"
                        prop:value=tok
                        attr:disabled=move || if loading.get() { Some("disabled") } else { None }
                        on:input=move |e| set_tok.set(event_target_value(&e))
                        on:keydown=move |e| {
                            if e.key() == "Enter" && !loading.get() { do_login(); }
                        }
                    />
                </div>

                {move || error.get().map(|e| view! { <div class="login-error">{e}</div> })}

                <button
                    class="login-btn"
                    attr:disabled=move || if loading.get() { Some("disabled") } else { None }
                    on:click=do_login_click
                >
                    {move || if loading.get() { "Connecting…" } else { "Connect" }}
                </button>

                <p class="login-hint">"Token is in ~/.ccc/.env as CCC_AGENT_TOKEN"</p>
            </div>
        </div>
    }
}
