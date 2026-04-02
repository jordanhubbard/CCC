/// Auth gate — front-door authentication wrapper for the RCC Dashboard.
///
/// Wraps the entire app. If no valid token is stored in localStorage,
/// shows a login form (username + token). Validates against the RCC API.
/// On success, stores credentials and renders children.
///
/// Per wq-JKH-002: unauthenticated users see a login screen, not the app.
use leptos::*;
use wasm_bindgen_futures::spawn_local;

const LS_KEY_USERNAME: &str = "rcc_username";
const LS_KEY_TOKEN: &str = "rcc_token";

/// Read a value from localStorage
fn ls_get(key: &str) -> Option<String> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(key).ok().flatten())
        .filter(|v| !v.is_empty())
}

/// Write a value to localStorage
fn ls_set(key: &str, val: &str) {
    if let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
    {
        let _ = storage.set_item(key, val);
    }
}

/// Remove a value from localStorage
fn ls_remove(key: &str) {
    if let Some(storage) = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
    {
        let _ = storage.remove_item(key);
    }
}

/// Validate token against RCC API by hitting GET /api/heartbeats with Bearer auth.
/// Returns true if the server responds 200.
async fn validate_token(token: &str) -> bool {
    let Ok(req) = gloo_net::http::Request::get("/api/heartbeats")
        .header("Authorization", &format!("Bearer {}", token))
        .build()
    else {
        return false;
    };
    match gloo_net::http::RequestBuilder::from(req).send().await {
        Ok(resp) => resp.ok(),
        Err(_) => false,
    }
}

/// Shared auth context — available to all components via use_context::<AuthState>()
#[derive(Clone)]
pub struct AuthState {
    pub authenticated: ReadSignal<bool>,
    pub set_authenticated: WriteSignal<bool>,
    pub username: ReadSignal<String>,
    pub set_username: WriteSignal<String>,
}

impl AuthState {
    /// Get the stored token for use in API requests
    pub fn token() -> Option<String> {
        ls_get(LS_KEY_TOKEN)
    }

    /// Log out: clear storage and reset signals
    pub fn logout(&self) {
        ls_remove(LS_KEY_USERNAME);
        ls_remove(LS_KEY_TOKEN);
        self.set_authenticated.set(false);
        self.set_username.set(String::new());
    }
}

/// AuthGate wraps the entire app. Children only render when authenticated.
#[component]
pub fn AuthGate(children: Children) -> impl IntoView {
    let (authenticated, set_authenticated) = create_signal(false);
    let (username, set_username) = create_signal(String::new());
    let (checking, set_checking) = create_signal(true);

    let auth_state = AuthState {
        authenticated,
        set_authenticated,
        username,
        set_username,
    };
    provide_context(auth_state.clone());

    // On mount: check localStorage for existing token and validate it
    {
        let set_auth = set_authenticated;
        let set_user = set_username;
        let set_chk = set_checking;
        spawn_local(async move {
            if let (Some(token), Some(user)) = (ls_get(LS_KEY_TOKEN), ls_get(LS_KEY_USERNAME)) {
                if validate_token(&token).await {
                    set_user.set(user);
                    set_auth.set(true);
                } else {
                    // Stale or invalid token — clear it
                    ls_remove(LS_KEY_TOKEN);
                    ls_remove(LS_KEY_USERNAME);
                }
            }
            set_chk.set(false);
        });
    }

    let stored_children = store_value(children);

    view! {
        {move || {
            if checking.get() {
                view! {
                    <div class="auth-loading">
                        <div class="auth-spinner">"🐿️"</div>
                        <p>"Checking credentials..."</p>
                    </div>
                }.into_view()
            } else if authenticated.get() {
                stored_children.with_value(|children| children()).into_view()
            } else {
                view! { <LoginForm /> }.into_view()
            }
        }}
    }
}

/// Login form — shown when user is not authenticated.
#[component]
fn LoginForm() -> impl IntoView {
    let (login_username, set_login_username) = create_signal(String::new());
    let (login_token, set_login_token) = create_signal(String::new());
    let (error, set_error) = create_signal(Option::<String>::None);
    let (loading, set_loading) = create_signal(false);

    let auth_state = use_context::<AuthState>().expect("AuthState must be provided by AuthGate");

    let on_submit = move |ev: web_sys::Event| {
        ev.prevent_default();
        let user = login_username.get().trim().to_string();
        let token = login_token.get().trim().to_string();

        if user.is_empty() || token.is_empty() {
            set_error.set(Some("Username and token are required".into()));
            return;
        }

        set_loading.set(true);
        set_error.set(None);

        let auth = auth_state.clone();
        spawn_local(async move {
            if validate_token(&token).await {
                ls_set(LS_KEY_USERNAME, &user);
                ls_set(LS_KEY_TOKEN, &token);
                auth.set_username.set(user);
                auth.set_authenticated.set(true);
            } else {
                set_error.set(Some("Invalid token — check your credentials and try again".into()));
            }
            set_loading.set(false);
        });
    };

    view! {
        <div class="auth-backdrop">
            <div class="auth-card">
                <div class="auth-header">
                    <span class="auth-logo">"🐿️"</span>
                    <h1>"Rocky Command Center"</h1>
                    <p class="auth-subtitle">"Sign in to continue"</p>
                </div>
                <form class="auth-form" on:submit=on_submit>
                    <div class="auth-field">
                        <label for="rcc-username">"Username"</label>
                        <input
                            id="rcc-username"
                            type="text"
                            autocomplete="username"
                            placeholder="Your username"
                            prop:value=login_username
                            on:input=move |ev| set_login_username.set(event_target_value(&ev))
                            prop:disabled=loading
                        />
                    </div>
                    <div class="auth-field">
                        <label for="rcc-token">"Token"</label>
                        <input
                            id="rcc-token"
                            type="password"
                            autocomplete="current-password"
                            placeholder="Your RCC token"
                            prop:value=login_token
                            on:input=move |ev| set_login_token.set(event_target_value(&ev))
                            prop:disabled=loading
                        />
                    </div>
                    {move || error.get().map(|e| view! {
                        <div class="auth-error">{e}</div>
                    })}
                    <button type="submit" class="auth-submit" prop:disabled=loading>
                        {move || if loading.get() { "Signing in…" } else { "Sign In" }}
                    </button>
                </form>
                <p class="auth-hint">
                    "Your token is your RCC auth token. Save it in a password manager for easy access."
                </p>
            </div>
        </div>
    }
}

/// Logout button component — place in the dashboard header.
#[component]
pub fn LogoutButton() -> impl IntoView {
    let auth_state = use_context::<AuthState>();

    view! {
        {move || auth_state.clone().map(|auth| {
            let user = auth.username.get();
            if user.is_empty() { return view! { <></> }.into_view(); }
            let auth_for_click = auth.clone();
            view! {
                <div class="auth-user-info">
                    <span class="auth-user-name">{user}</span>
                    <button
                        class="auth-logout-btn"
                        on:click=move |_| auth_for_click.logout()
                    >"Logout"</button>
                </div>
            }.into_view()
        })}
    }
}
