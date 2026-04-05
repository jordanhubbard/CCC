//! Services Directory — live health data from CCC /api/services/status (server-side probed).
//! No client-side NoCors hacks — all probing is done server-side with real TCP checks.

use leptos::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;

const RCC_API: &str = "http://146.190.134.110:8789";

#[derive(Clone, Debug, Deserialize)]
struct ServiceStatus {
    id:         String,
    name:       String,
    url:        String,
    desc:       String,
    host:       String,
    online:     bool,
    latency_ms: Option<u32>,
}

impl ServiceStatus {
    fn category(&self) -> &'static str {
        match self.id.as_str() {
            "rcc-dashboard" | "services-map" | "tokenhub-admin" => "UI",
            "boris-vllm" | "peabody-vllm" | "sherman-vllm" | "snidely-vllm" | "dudley-vllm"
            | "whisper-api" | "clawfs" | "usdagent" | "ollama"                 => "GPU",
            _                                                                     => "API",
        }
    }

    fn icon(&self) -> &'static str {
        match self.id.as_str() {
            "rcc-dashboard"  => "🐿️",
            "services-map"   => "🗺️",
                        "tokenhub-admin" => "🔑",
            "clawbus"        => "🚌",
            "boris-vllm"     => "🧠",
            "peabody-vllm"   => "🧠",
            "sherman-vllm"   => "🧠",
            "snidely-vllm"   => "🧠",
            "dudley-vllm"    => "🧠",
            "whisper-api"    => "🎙️",
            "clawfs"        => "📁",
            "usdagent"       => "🎨",
            "milvus"         => "🔍",
            "ollama"         => "🦙",
            _                => "⚙️",
        }
    }
}

async fn fetch_services() -> Vec<ServiceStatus> {
    let url = format!("{}/api/services/status", RCC_API);
    let window = match web_sys::window() {
        Some(w) => w,
        None    => return vec![],
    };
    let resp_val = match wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(&url)).await {
        Ok(v)  => v,
        Err(_) => return vec![],
    };
    let resp: web_sys::Response = match resp_val.dyn_into::<web_sys::Response>() {
        Ok(r)  => r,
        Err(_) => return vec![],
    };
    let text_val = match wasm_bindgen_futures::JsFuture::from(match resp.text() {
        Ok(p)  => p,
        Err(_) => return vec![],
    }).await {
        Ok(v)  => v,
        Err(_) => return vec![],
    };
    let text = match text_val.as_string() {
        Some(t) => t,
        None    => return vec![],
    };
    serde_json::from_str(&text).unwrap_or_default()
}

#[component]
pub fn Services() -> impl IntoView {
    let (services, set_services) = create_signal(Vec::<ServiceStatus>::new());
    let (loading, set_loading)   = create_signal(true);
    let (cat_filter, set_cat_filter) = create_signal("All".to_string());

    let load = move || {
        set_loading.set(true);
        wasm_bindgen_futures::spawn_local(async move {
            let data = fetch_services().await;
            set_services.set(data);
            set_loading.set(false);
        });
    };

    // Load on mount
    create_effect(move |_| { load(); });

    let recheck = move |_| { load(); };
    let categories = ["All", "UI", "API", "GPU"];

    view! {
        <div class="services-container">
            <div class="services-header">
                <h2 class="services-title">"🗺️ Services Directory"</h2>
                <div class="services-actions">
                    <div class="svc-cat-filters">
                        {categories.iter().map(|c| {
                            let c_str = c.to_string();
                            view! {
                                <button
                                    class="svc-cat-btn"
                                    class:svc-cat-active=move || cat_filter.get() == c_str
                                    on:click={
                                        let c_str = c_str.clone();
                                        move |_| set_cat_filter.set(c_str.clone())
                                    }
                                >{*c}</button>
                            }
                        }).collect_view()}
                    </div>
                    <button class="svc-recheck-btn" on:click=recheck>
                        {move || if loading.get() { "⟳ Checking…" } else { "↻ Recheck" }}
                    </button>
                </div>
            </div>
            {move || if loading.get() && services.get().is_empty() {
                view! { <div class="svc-loading">"Probing services…"</div> }.into_view()
            } else {
                let svcs = services.get();
                let filter = cat_filter.get();
                let cards = svcs.iter().filter(|s| {
                    filter == "All" || s.category() == filter.as_str()
                }).map(|s| {
                    let online     = s.online;
                    let latency    = s.latency_ms;
                    let url        = s.url.clone();
                    let host       = s.host.clone();
                    let icon       = s.icon();
                    let name       = s.name.clone();
                    let desc       = s.desc.clone();
                    let cat        = s.category();

                    let dot_class  = if online { "svc-dot svc-dot-up" } else { "svc-dot svc-dot-down" };
                    let status_lbl = if online { "UP" } else { "DOWN" };
                    let latency_str = latency.map(|ms| format!(" {}ms", ms)).unwrap_or_default();

                    view! {
                        <div class="svc-card">
                            <div class="svc-card-top">
                                <span class="svc-icon">{icon}</span>
                                <div class="svc-info">
                                    <div class="svc-name">{name}</div>
                                    <div class="svc-desc">{desc}</div>
                                    <div class="svc-host">"host: "{host}</div>
                                </div>
                                <div class="svc-status">
                                    <span class={dot_class}></span>
                                    <span class="svc-status-label">
                                        {status_lbl}{latency_str}
                                    </span>
                                </div>
                            </div>
                            <div class="svc-card-footer">
                                <span class="svc-cat-badge">{cat}</span>
                                <a
                                    class="svc-open-btn"
                                    href={url}
                                    target="_blank"
                                    rel="noopener noreferrer"
                                >"Open ↗"</a>
                            </div>
                        </div>
                    }
                }).collect_view();

                view! { <div class="services-grid">{cards}</div> }.into_view()
            }}
        </div>
    }
}
