//! Services Directory — clickable cards for every service in the fleet,
//! with live health dots polled every 30 s.

use leptos::*;
use wasm_bindgen::prelude::*;

// ── Service registry ─────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct ServiceEntry {
    icon:        &'static str,
    name:        &'static str,
    host:        &'static str,
    url:         &'static str,
    health_url:  Option<&'static str>, // None = derived from url + "/health"
    description: &'static str,
    auth:        Option<&'static str>, // None = open, Some(hint) = auth required
}

static SERVICES: &[ServiceEntry] = &[
    ServiceEntry {
        icon:        "🐿️",
        name:        "RCC Dashboard",
        host:        "do-host1 :8789",
        url:         "http://146.190.134.110:8789/",
        health_url:  Some("http://146.190.134.110:8789/health"),
        description: "Main portal — Dashboard, Geek View, Kanban, SquirrelChat, Agents, Issues, Providers, Coding",
        auth:        None,
    },
    ServiceEntry {
        icon:        "💬",
        name:        "SquirrelChat",
        host:        "do-host1 :8790",
        url:         "http://146.190.134.110:8790/",
        health_url:  Some("http://146.190.134.110:8790/api/channels"),
        description: "Real-time multi-channel chat — agents + humans, DMs, threads, reactions, file sharing (Node.js v2)",
        auth:        None,
    },
    ServiceEntry {
        icon:        "🪙",
        name:        "TokenHub Admin",
        host:        "do-host1 :8090",
        url:         "http://146.190.134.110:8090/admin",
        health_url:  Some("http://146.190.134.110:8090/health"),
        description: "LLM proxy — provider management, routing, Cytoscape topology, D3 charts, what-if simulator",
        auth:        None,
    },
    ServiceEntry {
        icon:        "🪣",
        name:        "MinIO Console",
        host:        "do-host1 :9001",
        url:         "http://146.190.134.110:9001/",
        health_url:  Some("http://146.190.134.110:9000/minio/health/live"),
        description: "S3-compatible object storage — agent files, manifests, squirrelbus logs",
        auth:        Some("Login required (MinIO credentials)"),
    },
    ServiceEntry {
        icon:        "🔌",
        name:        "RCC API",
        host:        "do-host1 :8789",
        url:         "http://146.190.134.110:8789/health",
        health_url:  Some("http://146.190.134.110:8789/health"),
        description: "Workqueue + heartbeat + project API (integrated into RCC Dashboard; direct API access needs auth)",
        auth:        Some("Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb"),
    },
    ServiceEntry {
        icon:        "🚌",
        name:        "SquirrelBus API",
        host:        "do-host1 :8789",
        url:         "http://146.190.134.110:8789/api/bus/messages",
        health_url:  Some("http://146.190.134.110:8789/health"),
        description: "Typed agent-to-agent message bus (viewer integrated in Dashboard tab)",
        auth:        Some("Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb"),
    },
    ServiceEntry {
        icon:        "🎙️",
        name:        "Whisper STT",
        host:        "sparky :8792",
        url:         "http://146.190.134.110:8792/",
        health_url:  Some("http://146.190.134.110:8792/health"),
        description: "Speech-to-text API (whisper.cpp on sparky GB10) — proxied via do-host1",
        auth:        None,
    },
];

// ── Health poll ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum HealthStatus {
    Unknown,
    Up,
    Down,
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__rcc"])]
    fn rcc_base() -> String;
}

async fn probe_url(url: &str) -> HealthStatus {
    let opts = web_sys::RequestInit::new();
    opts.set_method("GET");
    opts.set_mode(web_sys::RequestMode::NoCors); // avoids CORS errors for health checks
    let request = match web_sys::Request::new_with_str_and_init(url, &opts) {
        Ok(r) => r,
        Err(_) => return HealthStatus::Down,
    };
    let window = match web_sys::window() {
        Some(w) => w,
        None => return HealthStatus::Unknown,
    };
    let promise = window.fetch_with_request(&request);
    let result = wasm_bindgen_futures::JsFuture::from(promise).await;
    match result {
        Ok(_) => HealthStatus::Up,
        Err(_) => HealthStatus::Down,
    }
}

// ── Component ─────────────────────────────────────────────────────────────────

#[component]
pub fn Services() -> impl IntoView {
    // Vec of (status_signal, set_status) per service, index-aligned with SERVICES
    let statuses: Vec<(ReadSignal<HealthStatus>, WriteSignal<HealthStatus>)> = SERVICES
        .iter()
        .map(|_| create_signal(HealthStatus::Unknown))
        .collect();

    let statuses_for_poll = statuses.clone();

    // Poll all health endpoints on mount and every 30 s
    create_effect(move |_| {
        let setters: Vec<WriteSignal<HealthStatus>> =
            statuses_for_poll.iter().map(|(_, s)| *s).collect();

        spawn_local(async move {
            poll_all(&setters).await;

            // repeat every 30 s
            loop {
                gloo_timers::future::TimeoutFuture::new(30_000).await;
                poll_all(&setters).await;
            }
        });
    });

    let statuses_for_view = statuses.clone();

    view! {
        <div class="services-panel">
            <div class="services-header">
                <h2>"🗺️ Services Directory"</h2>
                <p class="services-subtitle">
                    "All running services in the fleet — click any card to open. "
                    <span class="dot-legend">
                        <span class="dot dot-up">"●"</span>" up  "
                        <span class="dot dot-down">"●"</span>" down  "
                        <span class="dot dot-unknown">"●"</span>" checking"
                    </span>
                </p>
            </div>
            <div class="services-grid">
                {SERVICES.iter().enumerate().map(|(i, svc)| {
                    let (status, _) = statuses_for_view[i];
                    let url = svc.url;
                    view! {
                        <a
                            class="service-card"
                            href={url}
                            target="_blank"
                            rel="noopener noreferrer"
                        >
                            <div class="service-card-header">
                                <span class="service-icon">{svc.icon}</span>
                                <span class="service-name">{svc.name}</span>
                                <span
                                    class="service-dot"
                                    class:dot-up=move || status.get() == HealthStatus::Up
                                    class:dot-down=move || status.get() == HealthStatus::Down
                                    class:dot-unknown=move || status.get() == HealthStatus::Unknown
                                >"●"</span>
                            </div>
                            <div class="service-host">"📍 "{svc.host}</div>
                            <div class="service-desc">{svc.description}</div>
                            {svc.auth.map(|hint| view! {
                                <div class="service-auth">"🔒 "{hint}</div>
                            })}
                            <div class="service-url">{url}</div>
                        </a>
                    }
                }).collect_view()}
            </div>
        </div>
    }
}

async fn poll_all(setters: &[WriteSignal<HealthStatus>]) {
    for (i, svc) in SERVICES.iter().enumerate() {
        let health_url = svc.health_url.unwrap_or(svc.url);
        let status = probe_url(health_url).await;
        setters[i].set(status);
    }
}
