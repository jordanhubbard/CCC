//! Services Directory — live health-checked cards for every RCC service.

use leptos::*;
use wasm_bindgen_futures::spawn_local;

#[derive(Clone, Debug)]
struct ServiceDef {
    name:        &'static str,
    desc:        &'static str,
    icon:        &'static str,
    url:         &'static str,
    health_url:  &'static str,
    category:    &'static str,
}

const SERVICES: &[ServiceDef] = &[
    ServiceDef {
        name:       "RCC Dashboard",
        desc:       "Main portal — WASM SPA, 8 tabs",
        icon:       "🐿️",
        url:        "http://146.190.134.110:8788/",
        health_url: "http://146.190.134.110:8788/",
        category:   "UI",
    },
    ServiceDef {
        name:       "TokenHub Admin",
        desc:       "Inference proxy — providers, models, routing, topology",
        icon:       "🔑",
        url:        "http://146.190.134.110:8090/admin",
        health_url: "http://146.190.134.110:8090/health",
        category:   "UI",
    },
    ServiceDef {
        name:       "SquirrelChat",
        desc:       "Team chat — channels, DMs, threads, reactions",
        icon:       "💬",
        url:        "http://146.190.134.110:8790/",
        health_url: "http://146.190.134.110:8790/",
        category:   "UI",
    },
    ServiceDef {
        name:       "MinIO Console",
        desc:       "S3-compatible object storage web console",
        icon:       "🗄️",
        url:        "http://146.190.134.110:9001/",
        health_url: "http://146.190.134.110:9001/minio/health/live",
        category:   "UI",
    },
    ServiceDef {
        name:       "RCC API",
        desc:       "Backend API — queue, heartbeats, secrets, bus",
        icon:       "⚙️",
        url:        "http://146.190.134.110:8789/health",
        health_url: "http://146.190.134.110:8789/health",
        category:   "API",
    },
    ServiceDef {
        name:       "SquirrelChat API",
        desc:       "Rust/Axum backend — channels, messages, WebSocket",
        icon:       "🦀",
        url:        "http://146.190.134.110:8793/health",
        health_url: "http://146.190.134.110:8793/health",
        category:   "API",
    },
    ServiceDef {
        name:       "TokenHub API",
        desc:       "OpenAI-compatible inference aggregation proxy",
        icon:       "🤖",
        url:        "http://146.190.134.110:8090/health",
        health_url: "http://146.190.134.110:8090/health",
        category:   "API",
    },
    ServiceDef {
        name:       "MinIO S3 API",
        desc:       "S3-compatible storage endpoint",
        icon:       "📦",
        url:        "http://146.190.134.110:9000/",
        health_url: "http://146.190.134.110:9000/minio/health/live",
        category:   "API",
    },
    ServiceDef {
        name:       "Boris vLLM",
        desc:       "Nemotron-3 120B FP8 — 4x L40, 262k ctx (tunneled)",
        icon:       "🧠",
        url:        "http://146.190.134.110:18080/v1/models",
        health_url: "http://146.190.134.110:18080/health",
        category:   "GPU",
    },
    ServiceDef {
        name:       "SquirrelBus",
        desc:       "Message bus SSE stream (embedded in dashboard)",
        icon:       "🚌",
        url:        "http://146.190.134.110:8789/bus/stream",
        health_url: "http://146.190.134.110:8789/health",
        category:   "API",
    },
];

#[derive(Clone, PartialEq)]
enum HealthStatus {
    Unknown,
    Checking,
    Up,
    Down,
}

impl HealthStatus {
    fn dot_class(&self) -> &'static str {
        match self {
            HealthStatus::Unknown  => "svc-dot svc-dot-unknown",
            HealthStatus::Checking => "svc-dot svc-dot-checking",
            HealthStatus::Up       => "svc-dot svc-dot-up",
            HealthStatus::Down     => "svc-dot svc-dot-down",
        }
    }
    fn label(&self) -> &'static str {
        match self {
            HealthStatus::Unknown  => "—",
            HealthStatus::Checking => "…",
            HealthStatus::Up       => "UP",
            HealthStatus::Down     => "DOWN",
        }
    }
}

async fn fetch_ok(url: String) -> bool {
    use wasm_bindgen::JsCast;
    let window = web_sys::window().unwrap();
    let opts = web_sys::RequestInit::new();
    opts.set_method("GET");
    opts.set_mode(web_sys::RequestMode::NoCors);
    let request = match web_sys::Request::new_with_str_and_init(&url, &opts) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let promise = window.fetch_with_request(&request);
    match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(_) => true,
        Err(_) => false,
    }
}

fn run_health_checks(statuses: &[RwSignal<HealthStatus>]) {
    for (i, svc) in SERVICES.iter().enumerate() {
        let sig = statuses[i];
        sig.set(HealthStatus::Checking);
        let url = svc.health_url.to_string();
        spawn_local(async move {
            let ok = fetch_ok(url).await;
            sig.set(if ok { HealthStatus::Up } else { HealthStatus::Down });
        });
    }
}

#[component]
pub fn Services() -> impl IntoView {
    let statuses: Vec<RwSignal<HealthStatus>> = SERVICES
        .iter()
        .map(|_| create_rw_signal(HealthStatus::Unknown))
        .collect();

    let (cat_filter, set_cat_filter) = create_signal("All".to_string());

    // Initial health check on mount
    {
        let s = statuses.clone();
        create_effect(move |_| {
            run_health_checks(&s);
        });
    }

    let statuses_view    = statuses.clone();
    let statuses_recheck = statuses.clone();

    let recheck = move |_| {
        run_health_checks(&statuses_recheck);
    };

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
                        "↻ Recheck"
                    </button>
                </div>
            </div>
            <div class="services-grid">
                {SERVICES.iter().enumerate().map(|(i, svc)| {
                    let sig = statuses_view[i];
                    let cat = svc.category.to_string();
                    view! {
                        <div
                            class="svc-card"
                            class:svc-card-hidden=move || {
                                let f = cat_filter.get();
                                f != "All" && f != cat
                            }
                        >
                            <div class="svc-card-top">
                                <span class="svc-icon">{svc.icon}</span>
                                <div class="svc-info">
                                    <div class="svc-name">{svc.name}</div>
                                    <div class="svc-desc">{svc.desc}</div>
                                </div>
                                <div class="svc-status">
                                    <span class=move || sig.get().dot_class()></span>
                                    <span class="svc-status-label">{move || sig.get().label()}</span>
                                </div>
                            </div>
                            <div class="svc-card-footer">
                                <span class="svc-cat-badge">{svc.category}</span>
                                <a
                                    class="svc-open-btn"
                                    href={svc.url}
                                    target="_blank"
                                    rel="noopener noreferrer"
                                >"Open ↗"</a>
                            </div>
                        </div>
                    }
                }).collect_view()}
            </div>
        </div>
    }
}
