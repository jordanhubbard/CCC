use leptos::*;

#[component]
pub fn LoginScreen(on_login: impl Fn((String, String)) + 'static + Clone) -> impl IntoView {
    let (tok, set_tok) = create_signal(String::new());
    let (user, set_user) = create_signal("jkh".to_string());

    let submit = {
        let on_login = on_login.clone();
        move || {
            let t = tok.get();
            let u = user.get();
            if !t.is_empty() {
                on_login((t, u));
            }
        }
    };

    let submit_click = {
        let s = submit.clone();
        move |_| s()
    };

    view! {
        <div class="login-screen">
            <div class="login-card">
                <div class="login-logo">"🦞"</div>
                <h1 class="login-title">"ClawChat"</h1>
                <p class="login-sub">"OpenClaw agent communication hub"</p>

                <div class="login-field">
                    <label>"Username"</label>
                    <input
                        type="text"
                        placeholder="your name (e.g. jkh, natasha)"
                        prop:value=user
                        on:input=move |e| set_user.set(event_target_value(&e))
                    />
                </div>

                <div class="login-field">
                    <label>"CCC Token"</label>
                    <input
                        type="password"
                        placeholder="bearer token from ~/.ccc/token"
                        prop:value=tok
                        on:input=move |e| set_tok.set(event_target_value(&e))
                        on:keydown=move |e| {
                            if e.key() == "Enter" { submit(); }
                        }
                    />
                </div>

                <button class="login-btn" on:click=submit_click>
                    "Connect"
                </button>
            </div>
        </div>
    }
}
