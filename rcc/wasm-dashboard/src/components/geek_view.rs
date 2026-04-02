/// GeekView — SVG infrastructure topology map with live SSE traffic particles.
///
/// Layout is hand-tuned (static positions), with live heartbeat dots and
/// animated particles triggered by ClawBus events from /api/geek/stream.
use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{EventSource, MessageEvent};
use crate::types::{HeartbeatMap, TrafficEvent};

// ── Component ─────────────────────────────────────────────────────────────────

#[component]
pub fn GeekView(heartbeats: RwSignal<HeartbeatMap>) -> impl IntoView {
    let traffic: RwSignal<Vec<TrafficEvent>> = RwSignal::new(vec![]);
    let sse_status: RwSignal<&'static str>   = RwSignal::new("connecting");

    // Connect SSE for live traffic events
    {
        let traffic = traffic;
        let status  = sse_status;
        if let Ok(es) = EventSource::new("/api/geek/stream") {
            {
                let es2 = es.clone();
                let s   = status;
                let on_open = Closure::wrap(Box::new(move |_: web_sys::Event| {
                    s.set("live");
                    // Remove unused warning on es2
                    let _ = &es2;
                }) as Box<dyn FnMut(_)>);
                es.set_onopen(Some(on_open.as_ref().unchecked_ref()));
                on_open.forget();
            }
            {
                let s = status;
                let on_err = Closure::wrap(Box::new(move |_: web_sys::Event| {
                    s.set("reconnecting");
                }) as Box<dyn FnMut(_)>);
                es.set_onerror(Some(on_err.as_ref().unchecked_ref()));
                on_err.forget();
            }
            {
                let t = traffic;
                let on_msg = Closure::wrap(Box::new(move |e: MessageEvent| {
                    if let Some(text) = e.data().as_string() {
                        if let Ok(evt) = serde_json::from_str::<TrafficEvent>(&text) {
                            t.update(|v| {
                                v.insert(0, evt);
                                if v.len() > 20 { v.truncate(20); }
                            });
                        }
                    }
                }) as Box<dyn FnMut(_)>);
                es.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
                on_msg.forget();
            }
            let _ = Box::new(es);
        }
    }

    view! {
        <div class="geek-layout">
            // SVG topology
            <div class="geek-svg-container">
                <TopologySvg heartbeats=heartbeats />
            </div>
            // Sidebar: traffic log
            <div class="geek-sidebar">
                <div class="card" style="flex-shrink:0">
                    <div class="card-title">"🖥️ Fleet Topology"</div>
                    <div style="font-size:11px;color:var(--text-dim)">
                        "SSE: "
                        <span style=move || {
                            let c = match sse_status.get() {
                                "live"         => "color:var(--green)",
                                "reconnecting" => "color:var(--yellow)",
                                _              => "color:var(--text-dim)",
                            };
                            c
                        }>
                            {move || sse_status.get()}
                        </span>
                    </div>
                </div>
                <div class="geek-traffic" style="flex:1">
                    <div class="panel-title" style="font-size:11px;font-weight:700;color:var(--text-muted);text-transform:uppercase;letter-spacing:.05em;margin-bottom:8px">
                        "📡 Live Traffic"
                    </div>
                    {move || {
                        let events = traffic.get();
                        if events.is_empty() {
                            view! {
                                <div style="color:var(--text-dim);font-size:11px">"Waiting for events…"</div>
                            }.into_any()
                        } else {
                            events.into_iter().map(|evt| {
                                view! { <TrafficRow evt=evt /> }
                            }).collect_view().into_any()
                        }
                    }}
                </div>
            </div>
        </div>
    }
}

// ── Traffic log row ───────────────────────────────────────────────────────────

#[component]
fn TrafficRow(evt: TrafficEvent) -> impl IntoView {
    let from = evt.from.as_deref().unwrap_or("?").to_string();
    let to   = evt.to.as_deref().unwrap_or("?").to_string();
    let typ  = evt.event_type.as_deref().unwrap_or("msg").to_string();
    let ts   = evt.ts.as_deref().unwrap_or("").to_string();
    let ts_short = if ts.len() >= 19 { ts[11..19].to_string() } else { ts };

    view! {
        <div class="traffic-entry">
            <span class="t-ts">{ts_short}</span>
            <span class="t-from">{from}</span>
            <span style="color:var(--text-dim)">"→"</span>
            <span class="t-to">{to}</span>
            <span class="t-type">{typ}</span>
        </div>
    }
}

// ── SVG topology (ported from geek-view-reference.mjs) ───────────────────────

#[component]
fn TopologySvg(heartbeats: RwSignal<HeartbeatMap>) -> impl IntoView {
    // Derive heartbeat dot color per agent
    let dot_color = move |agent: &str| -> &'static str {
        let hbs = heartbeats.get();
        match hbs.get(agent) {
            None     => "#484f58",
            Some(hb) => match hb.status_class() {
                "online"  => "#3fb950",
                "stale"   => "#d29922",
                _          => "#f85149",
            },
        }
    };

    view! {
        <svg
            id="topo-svg"
            viewBox="0 0 900 600"
            preserveAspectRatio="xMidYMid meet"
            xmlns="http://www.w3.org/2000/svg"
        >
            <defs>
                <marker id="arrow" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
                    <path d="M0,0 L0,6 L6,3 z" fill="#30363d" />
                </marker>
                <marker id="arrow-active" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
                    <path d="M0,0 L0,6 L6,3 z" fill="#58a6ff" />
                </marker>
            </defs>

            // ── Edges ──────────────────────────────────────────────────────────
            // Rocky ↔ Natasha
            <path d="M 300,160 C 450,90 540,90 610,160"
                fill="none" stroke="#30363d" stroke-width="1.5" opacity="0.6"
                marker-end="url(#arrow)" />
            // Rocky ↔ Bullwinkle
            <path d="M 220,280 C 200,370 200,420 220,460"
                fill="none" stroke="#30363d" stroke-width="1.5" opacity="0.6"
                marker-end="url(#arrow)" />
            // Natasha ↔ Bullwinkle
            <path d="M 660,280 C 650,370 480,440 360,460"
                fill="none" stroke="#30363d" stroke-width="1.5" opacity="0.4"
                marker-end="url(#arrow)" />
            // Boris → Rocky (dashed, on-demand)
            <path d="M 820,200 C 700,175 380,175 300,200"
                fill="none" stroke="#30363d" stroke-width="1" opacity="0.4"
                stroke-dasharray="5,3" marker-end="url(#arrow)" />
            // Rocky → Milvus
            <path d="M 300,210 L 440,310"
                fill="none" stroke="#21262d" stroke-width="1" stroke-dasharray="4,3" />
            // Rocky → MinIO
            <path d="M 270,215 L 400,360"
                fill="none" stroke="#21262d" stroke-width="1" stroke-dasharray="4,3" />
            // Natasha → Milvus
            <path d="M 610,210 L 480,310"
                fill="none" stroke="#21262d" stroke-width="1" stroke-dasharray="4,3" />
            // Natasha → NVIDIA
            <path d="M 730,145 L 845,75"
                fill="none" stroke="#1a3a2a" stroke-width="1" stroke-dasharray="4,3" />

            // ── Machine nodes ──────────────────────────────────────────────────
            // Rocky (do-host1)
            <g class="machine-node" id="node-rocky" transform="translate(140,130)">
                <rect width="185" height="140" rx="10" fill="#161b22" stroke="#f85149" stroke-width="1.5" />
                <circle cx="173" cy="12" r="5"
                    fill=move || dot_color("rocky") />
                <text x="10" y="20" font-size="12" font-weight="700" fill="#f85149">"🐿️ Rocky"</text>
                <text x="10" y="34" font-size="9" fill="#6e7681">"do-host1 · CPU VPS"</text>
                <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">"[CCC API :8789]"</text>
                <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">"[Dashboard :8788]"</text>
                <text x="10" y="76" font-size="9" fill="#8b949e" font-family="monospace">"[ClawBus hub]"</text>
                <text x="10" y="88" font-size="9" fill="#8b949e" font-family="monospace">"[CCC Brain]"</text>
                <text x="10" y="100" font-size="9" fill="#8b949e" font-family="monospace">"[SearXNG :8888]"</text>
                <text x="10" y="112" font-size="9" fill="#8b949e" font-family="monospace">"[MinIO :9000]"</text>
                <text x="10" y="124" font-size="9" fill="#8b949e" font-family="monospace">"[Milvus :19530]"</text>
            </g>

            // Natasha (sparky)
            <g class="machine-node" id="node-natasha" transform="translate(590,130)">
                <rect width="200" height="155" rx="10" fill="#161b22" stroke="#3fb950" stroke-width="1.5" />
                <circle cx="188" cy="12" r="5"
                    fill=move || dot_color("natasha") />
                <text x="10" y="20" font-size="12" font-weight="700" fill="#3fb950">"🕵️ Natasha"</text>
                <text x="10" y="34" font-size="9" fill="#6e7681">"sparky · GB10 · 128GB"</text>
                <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">"[OpenClaw :18789]"</text>
                <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">"[ClawBus /bus→18799]"</text>
                <text x="10" y="76" font-size="9" fill="#3fb950" font-family="monospace">"[CUDA/RTX ⚡]"</text>
                <text x="10" y="88" font-size="9" fill="#8b949e" font-family="monospace">"[Ollama :11434 ✓]"</text>
                <text x="10" y="100" font-size="9" fill="#484f58" font-family="monospace">"  qwen2.5-coder:32b"</text>
                <text x="10" y="112" font-size="9" fill="#484f58" font-family="monospace">"  qwen3-coder:latest"</text>
                <text x="10" y="124" font-size="9" fill="#8b949e" font-family="monospace">"[Milvus :19530]"</text>
            </g>

            // Bullwinkle (puck)
            <g class="machine-node" id="node-bullwinkle" transform="translate(140,440)">
                <rect width="185" height="120" rx="10" fill="#161b22" stroke="#a371f7" stroke-width="1.5" />
                <circle cx="173" cy="12" r="5"
                    fill=move || dot_color("bullwinkle") />
                <text x="10" y="20" font-size="12" font-weight="700" fill="#a371f7">"🫎 Bullwinkle"</text>
                <text x="10" y="34" font-size="9" fill="#6e7681">"puck · Mac mini · CPU"</text>
                <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">"[OpenClaw :18789]"</text>
                <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">"[Google Workspace]"</text>
                <text x="10" y="76" font-size="9" fill="#8b949e" font-family="monospace">"[iMessage / Calendar]"</text>
                <text x="10" y="88" font-size="9" fill="#8b949e" font-family="monospace">"[Sonos]"</text>
                <text x="10" y="100" font-size="9" fill="#8b949e" font-family="monospace">"[launchd crons]"</text>
            </g>

            // Boris (l40-sweden) — dashed border = sometimes offline
            <g class="machine-node" id="node-boris" transform="translate(800,130)">
                <rect width="130" height="115" rx="10" fill="#161b22" stroke="#d29922"
                    stroke-width="1.5" stroke-dasharray="5,3" />
                <circle cx="118" cy="12" r="5"
                    fill=move || dot_color("boris") />
                <text x="10" y="20" font-size="12" font-weight="700" fill="#d29922">"⚡ Boris"</text>
                <text x="10" y="34" font-size="9" fill="#6e7681">"l40-sweden"</text>
                <text x="10" y="52" font-size="9" fill="#8b949e" font-family="monospace">"[4x L40 GPUs]"</text>
                <text x="10" y="64" font-size="9" fill="#8b949e" font-family="monospace">"[128GB RAM]"</text>
                <text x="10" y="76" font-size="9" fill="#8b949e" font-family="monospace">"[Omniverse]"</text>
                <text x="10" y="88" font-size="9" fill="#d29922" font-family="monospace">"[Isaac Lab]"</text>
            </g>

            // ── Shared infra nodes ─────────────────────────────────────────────
            <g class="service-node" transform="translate(450,310)">
                <circle cx="0" cy="0" r="34" fill="#161b22" stroke="#1f6feb" stroke-width="1.5" />
                <text x="0" y="-9" font-size="10" font-weight="700" fill="#1f6feb" text-anchor="middle">"Milvus"</text>
                <text x="0" y="5"  font-size="8"  fill="#6e7681"  text-anchor="middle">":19530"</text>
                <text x="0" y="17" font-size="8"  fill="#484f58"  text-anchor="middle">"do-host1"</text>
            </g>

            <g class="service-node" transform="translate(420,395)">
                <circle cx="0" cy="0" r="30" fill="#161b22" stroke="#1f6feb" stroke-width="1.5" />
                <text x="0" y="-7" font-size="10" font-weight="700" fill="#1f6feb" text-anchor="middle">"MinIO"</text>
                <text x="0" y="6"  font-size="8"  fill="#6e7681"  text-anchor="middle">":9000"</text>
                <text x="0" y="17" font-size="8"  fill="#484f58"  text-anchor="middle">"do-host1"</text>
            </g>

            <g class="service-node" transform="translate(355,455)">
                <circle cx="0" cy="0" r="28" fill="#161b22" stroke="#1f6feb" stroke-width="1.5" />
                <text x="0" y="-6" font-size="10" font-weight="700" fill="#1f6feb" text-anchor="middle">"SearXNG"</text>
                <text x="0" y="6"  font-size="8"  fill="#6e7681"  text-anchor="middle">":8888"</text>
            </g>

            // ── External nodes ─────────────────────────────────────────────────
            <g transform="translate(848,42)">
                <rect width="75" height="40" rx="6" fill="#0d1117" stroke="#3fb95044"
                    stroke-width="1" stroke-dasharray="3,2" />
                <text x="37" y="14" font-size="9" fill="#3fb950" text-anchor="middle">"NVIDIA"</text>
                <text x="37" y="26" font-size="9" fill="#3fb950" text-anchor="middle">"Gateway"</text>
                <text x="37" y="36" font-size="8" fill="#484f58" text-anchor="middle">"cloud"</text>
            </g>

            <g transform="translate(10,520)">
                <rect width="90" height="36" rx="6" fill="#0d1117" stroke="#30363d44"
                    stroke-width="1" stroke-dasharray="3,2" />
                <text x="45" y="14" font-size="9" fill="#8b949e" text-anchor="middle">"GitHub"</text>
                <text x="45" y="26" font-size="8" fill="#484f58" text-anchor="middle">"api.github.com"</text>
            </g>

            <g transform="translate(115,520)">
                <rect width="90" height="36" rx="6" fill="#0d1117" stroke="#30363d44"
                    stroke-width="1" stroke-dasharray="3,2" />
                <text x="45" y="14" font-size="9" fill="#8b949e" text-anchor="middle">"Telegram"</text>
                <text x="45" y="26" font-size="8" fill="#484f58" text-anchor="middle">"jkh direct"</text>
            </g>

            <g transform="translate(220,520)">
                <rect width="105" height="36" rx="6" fill="#0d1117" stroke="#30363d44"
                    stroke-width="1" stroke-dasharray="3,2" />
                <text x="52" y="14" font-size="9" fill="#8b949e" text-anchor="middle">"Mattermost"</text>
                <text x="52" y="26" font-size="8" fill="#484f58" text-anchor="middle">"chat.yourmom.photos"</text>
            </g>

            <g transform="translate(340,520)">
                <rect width="90" height="36" rx="6" fill="#0d1117" stroke="#30363d44"
                    stroke-width="1" stroke-dasharray="3,2" />
                <text x="45" y="14" font-size="9" fill="#8b949e" text-anchor="middle">"Slack omgjkh"</text>
                <text x="45" y="26" font-size="8" fill="#484f58" text-anchor="middle">"omgjkh.slack.com"</text>
            </g>

            // ── Traffic particle layer (rendered in DOM via JS injection trick) ─
            // Particles are rendered as <circle> elements using CSS offset-path.
            // We use a <g id="particles"> and inject via Effect + web-sys.
            <g id="geek-particles" />

        </svg>
    }
}
