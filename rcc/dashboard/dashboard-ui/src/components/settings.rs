use leptos::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetupConfig {
    agent_name: String,
    public_url: String,
    tokenhub_url: String,
    supervisor_enabled: bool,
    minio_endpoint: String,
    minio_bucket: String,
    rcc_port: u16,
    log_level: String,
    crush_server_url: String,
    sc_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SetupStatus {
    first_run: bool,
    has_tokenhub: bool,
    has_minio: bool,
    agent_name: String,
    version: String,
}

fn bool_badge(v: bool) -> &'static str {
    if v { "badge-yes" } else { "badge-no" }
}

fn bool_label(v: bool) -> &'static str {
    if v { "✓ yes" } else { "✗ no" }
}

#[component]
pub fn Settings() -> impl IntoView {
    let config = create_resource(
        || (),
        |_| async move {
            gloo_net::http::Request::get("/api/setup/config")
                .send()
                .await
                .ok()?
                .json::<SetupConfig>()
                .await
                .ok()
        },
    );

    let status = create_resource(
        || (),
        |_| async move {
            gloo_net::http::Request::get("/api/setup/status")
                .send()
                .await
                .ok()?
                .json::<SetupStatus>()
                .await
                .ok()
        },
    );

    view! {
        <div class="settings-panel">
            <div class="panel-header">
                <h2>"Settings"</h2>
            </div>

            // ── Setup Status Card ────────────────────────────────────────
            <div class="settings-section">
                <h3>"Setup Status"</h3>
                <Suspense fallback=move || view! { <p class="loading-text">"…"</p> }>
                    {move || status.get().map(|s| match s {
                        None => view! { <p class="error-msg">"Failed to load status"</p> }.into_view(),
                        Some(st) => view! {
                            <div class="status-grid">
                                <div class="status-row">
                                    <span class="status-key">"Agent"</span>
                                    <span class="status-val">{st.agent_name}</span>
                                </div>
                                <div class="status-row">
                                    <span class="status-key">"Version"</span>
                                    <span class="status-val">{st.version}</span>
                                </div>
                                <div class="status-row">
                                    <span class="status-key">"First run"</span>
                                    <span class={format!("badge {}", bool_badge(st.first_run))}>
                                        {bool_label(st.first_run)}
                                    </span>
                                </div>
                                <div class="status-row">
                                    <span class="status-key">"TokenHub"</span>
                                    <span class={format!("badge {}", bool_badge(st.has_tokenhub))}>
                                        {bool_label(st.has_tokenhub)}
                                    </span>
                                </div>
                                <div class="status-row">
                                    <span class="status-key">"MinIO / S3"</span>
                                    <span class={format!("badge {}", bool_badge(st.has_minio))}>
                                        {bool_label(st.has_minio)}
                                    </span>
                                </div>
                            </div>
                        }.into_view(),
                    })}
                </Suspense>
            </div>

            // ── Runtime Config ──────────────────────────────────────────
            <div class="settings-section">
                <h3>"Runtime Config"</h3>
                <Suspense fallback=move || view! { <p class="loading-text">"…"</p> }>
                    {move || config.get().map(|c| match c {
                        None => view! {
                            <p class="error-msg">"Config unavailable (auth required)"</p>
                        }.into_view(),
                        Some(cfg) => view! {
                            <div class="config-grid">
                                <div class="config-row">
                                    <span class="config-key">"Agent name"</span>
                                    <span class="config-val">{cfg.agent_name}</span>
                                </div>
                                <div class="config-row">
                                    <span class="config-key">"Public URL"</span>
                                    <span class="config-val mono">
                                        {if cfg.public_url.is_empty() { "(not set)".to_string() } else { cfg.public_url }}
                                    </span>
                                </div>
                                <div class="config-row">
                                    <span class="config-key">"TokenHub URL"</span>
                                    <span class="config-val mono">{cfg.tokenhub_url}</span>
                                </div>
                                <div class="config-row">
                                    <span class="config-key">"Crush Server URL"</span>
                                    <span class="config-val mono">{cfg.crush_server_url}</span>
                                </div>
                                <div class="config-row">
                                    <span class="config-key">"MinIO endpoint"</span>
                                    <span class="config-val mono">
                                        {if cfg.minio_endpoint.is_empty() { "(not set)".to_string() } else { cfg.minio_endpoint }}
                                    </span>
                                </div>
                                <div class="config-row">
                                    <span class="config-key">"MinIO bucket"</span>
                                    <span class="config-val">{cfg.minio_bucket}</span>
                                </div>
                                <div class="config-row">
                                    <span class="config-key">"CCC port"</span>
                                    <span class="config-val">{cfg.rcc_port.to_string()}</span>
                                </div>
                                <div class="config-row">
                                    <span class="config-key">"Supervisor"</span>
                                    <span class={format!("badge {}", bool_badge(cfg.supervisor_enabled))}>
                                        {bool_label(cfg.supervisor_enabled)}
                                    </span>
                                </div>
                                <div class="config-row">
                                    <span class="config-key">"Log level"</span>
                                    <span class="config-val">{cfg.log_level}</span>
                                </div>
                            </div>
                        }.into_view(),
                    })}
                </Suspense>
            </div>

            <div class="settings-footer">
                <p class="muted">"Config editing (Phase 3) coming soon. For now: edit .env and restart rcc-server."</p>
            </div>
        </div>
    }
}
