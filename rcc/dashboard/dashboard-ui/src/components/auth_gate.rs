/// auth_gate.rs — Front-door authentication gate for the CCC Dashboard.
///
/// Wraps the entire app: unauthenticated users see a login form,
/// authenticated users see the dashboard. Token validated against
/// the RCC API (`/api/health` with Bearer auth).
///
/// Credentials stored in localStorage for persistence across reloads.
/// Password-manager friendly (standard username/password fields).

use leptos::*;
use web_sys::wasm_bindgen::JsCast;

// ── localStorage helpers ───────────────────────────────────────────────

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn get_stored(key: &str) -> Option<String> {
    storage()?.get_item(key).ok()?
}

fn set_stored(key: &str, val: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(key, val);
    }
}

fn remove_stored(key: &str) {
    if let Some(s) = storage() {
        let _ = s.remove_item(key);
    }
}

const KEY_USER: &str = "rcc_username";
const KEY_TOKEN: &str = "rcc_token";

// ── Token validation ───────────────────────────────────────────────────

/// Validate a token by hitting `/api/health` with Bearer auth.
/// Returns true if the server responds 200.
async fn validate_token(token: &str) -> bool {
    let opts = web_sys::RequestInit::new();
    opts.set_method("GET");

    let headers = web_sys::Headers::new().unwrap();
    let _ = headers.set("Authorization", &format!("Bearer {}", token));
    opts.set_headers(&headers);

    let window = web_sys::window().unwrap();
    let url = "/api/health";

    match wasm_bindgen_futures::JsFuture::from(window.fetch_with_str_and_init(url, &opts)).await {
        Ok(resp) => {
            let resp: web_sys::Response = resp.unchecked_into();
            resp.status() == 200
        }
        Err(_) => false,
    }
}

// ── Shared logout signal ───────────────────────────────────────────────

/// WriteSignal that triggers logout when set to true.
/// Provided via context so LogoutButton can access it.
#[derive(Clone, Copy)]
pub struct LogoutTrigger(pub WriteSignal<bool>);

// ── AuthGate wrapper ───────────────────────────────────────────────────

/// Top-level gate. Renders children only when authenticated.
#[component]
pub fn AuthGate(children: ChildrenFn) -> impl IntoView {
    let (checking, set_checking) = create_signal(true);
    let (authenticated, set_authenticated) = create_signal(false);
    let (error_msg, set_error_msg) = create_signal(String::new());
    let (logout_trigger, set_logout_trigger) = create_signal(false);

    // Provide logout trigger for LogoutButton
    provide_context(LogoutTrigger(set_logout_trigger));

    // Watch for logout trigger
    create_effect(move |_| {
        if logout_trigger.get() {
            remove_stored(KEY_USER);
            remove_stored(KEY_TOKEN);
            set_authenticated.set(false);
            set_logout_trigger.set(false);
        }
    });

    // Check for stored credentials on mount
    create_effect(move |_| {
        spawn_local(async move {
            if let Some(token) = get_stored(KEY_TOKEN) {
                if !token.is_empty() && validate_token(&token).await {
                    set_authenticated.set(true);
                }
            }
            set_checking.set(false);
        });
    });

    let on_login = move |username: String, token: String| {
        set_error_msg.set(String::new());
        spawn_local(async move {
            if validate_token(&token).await {
                set_stored(KEY_USER, &username);
                set_stored(KEY_TOKEN, &token);
                set_authenticated.set(true);
            } else {
                set_error_msg.set("Invalid token — check your credentials".to_string());
            }
        });
    };

    view! {
        {move || {
            if checking.get() {
                view! {
                    <div class="auth-loading">
                        <div class="auth-spinner"></div>
                        <p>"Checking credentials..."</p>
                    </div>
                }.into_view()
            } else if authenticated.get() {
                children().into_view()
            } else {
                view! {
                    <LoginForm
                        on_login=on_login.clone()
                        error_msg=error_msg
                    />
                }.into_view()
            }
        }}
    }
}

// ── LoginForm ──────────────────────────────────────────────────────────

#[component]
fn LoginForm(
    on_login: impl Fn(String, String) + 'static + Clone,
    error_msg: ReadSignal<String>,
) -> impl IntoView {
    let (username, set_username) = create_signal(String::new());
    let (token, set_token) = create_signal(String::new());
    let (submitting, set_submitting) = create_signal(false);

    let on_login = on_login.clone();
    let handle_submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        set_submitting.set(true);
        let u = username.get_untracked();
        let t = token.get_untracked();
        on_login(u, t);
        set_submitting.set(false);
    };

    view! {
        <div class="auth-backdrop">
            <div class="auth-card">
                <div class="auth-logo">
                    <span class="auth-logo-icon">"🐿️"</span>
                    <h1>"Claw Command Center"</h1>
                </div>
                <form class="auth-form" on:submit=handle_submit autocomplete="on">
                    <div class="auth-field">
                        <label for="rcc-user">"Username"</label>
                        <input
                            id="rcc-user"
                            type="text"
                            name="username"
                            autocomplete="username"
                            placeholder="agent name or admin"
                            prop:value=move || username.get()
                            on:input=move |ev| set_username.set(event_target_value(&ev))
                        />
                    </div>
                    <div class="auth-field">
                        <label for="rcc-token">"Token"</label>
                        <input
                            id="rcc-token"
                            type="password"
                            name="password"
                            autocomplete="current-password"
                            placeholder="Bearer token"
                            prop:value=move || token.get()
                            on:input=move |ev| set_token.set(event_target_value(&ev))
                        />
                    </div>
                    {move || {
                        let msg = error_msg.get();
                        if msg.is_empty() {
                            view! { <span></span> }.into_view()
                        } else {
                            view! { <p class="auth-error">{msg}</p> }.into_view()
                        }
                    }}
                    <button
                        type="submit"
                        class="auth-submit"
                        prop:disabled=move || submitting.get()
                    >
                        {move || if submitting.get() { "Validating..." } else { "Sign In" }}
                    </button>
                </form>
                <p class="auth-hint">"Use your RCC agent token or admin token."</p>
            </div>
        </div>
    }
}

// ── LogoutButton (for use in the dashboard header) ─────────────────────

#[component]
pub fn LogoutButton() -> impl IntoView {
    let trigger = use_context::<LogoutTrigger>();

    view! {
        <button
            class="logout-btn"
            on:click=move |_| {
                if let Some(LogoutTrigger(set)) = trigger {
                    set.set(true);
                }
            }
        >
            "🚪 Logout"
        </button>
    }
}
