//! Geek View — SVG topology map of the distributed agent brain.
//!
//! Machines are primary nodes; service chips sit on each host.
//! Live traffic particles flow along edges when SquirrelBus messages arrive.
//! Falls back gracefully to a static (polled) map if SSE is unavailable.

use leptos::*;
use wasm_bindgen::prelude::*;
use serde::{Deserialize, Serialize};

use crate::types::{BusMessage, HeartbeatMap};

// ── Layout constants ─────────────────────────────────────────────────────────

const SVG_W: f32 = 800.0;
const SVG_H: f32 = 490.0;

/// Central SquirrelBus hub
const HUB_X: f32 = 400.0;
const HUB_Y: f32 = 248.0;

/// Machine node half-dimensions
const NW2: f32 = 68.0;
const NH2: f32 = 34.0;

/// Particle animation: 40ms tick ≈ 25fps, travel ≈ 1.1 s
const TICK_MS: u32 = 40;
const PARTICLE_TICKS: u32 = 28;

// ── Static topology ──────────────────────────────────────────────────────────

// (id, primary_label, sub_label, center_x, center_y, services)
static NODES: &[(&str, &str, &str, f32, f32, &[&str])] = &[
    ("sparky",      "sparky",      "natasha · GB10",  400.0,  68.0, &["ollama", "gpu"]),
    ("do-host1",    "do-host1",    "rocky · Rocky 9", 660.0, 248.0, &["rcc-api", "gateway", "wq"]),
    ("puck",        "puck",        "bullwinkle",       140.0, 248.0, &["milvus", "minio"]),
    ("jordanh-rtx", "jordanh-rtx", "agent-rtx",        400.0, 418.0, &["render"]),
];

// ── Particle ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Particle {
    x0:    f32, y0: f32,  // start (sender node center)
    xm:    f32, ym: f32,  // mid   (SquirrelBus hub)
    x1:    f32, y1: f32,  // end   (receiver node center)
    ticks: u32,
    color: &'static str,
}

impl Particle {
    fn pos(&self) -> (f32, f32) {
        let t = (self.ticks as f32 / PARTICLE_TICKS as f32).min(1.0);
        if t < 0.5 {
            let u = t * 2.0;
            (lerp(self.x0, self.xm, u), lerp(self.y0, self.ym, u))
        } else {
            let u = (t - 0.5) * 2.0;
            (lerp(self.xm, self.x1, u), lerp(self.ym, self.y1, u))
        }
    }
    fn done(&self) -> bool { self.ticks >= PARTICLE_TICKS }
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }

// ── Soul commit ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SoulCommit {
    pub agent:   Option<String>,
    pub hash:    Option<String>,
    pub message: Option<String>,
    pub ts:      Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Map an agent/host name string to its NODES index.
fn name_to_node(name: &str) -> Option<usize> {
    let n = name.to_lowercase();
    if n.contains("natasha") || n.contains("sparky") || n.contains("gb10") {
        Some(0)
    } else if n.contains("rocky") || n.contains("do-host") {
        Some(1)
    } else if n.contains("bullwinkle") || n.contains("puck") {
        Some(2)
    } else if n.contains("boris") || n.contains("rtx") || n.contains("jordanh") {
        Some(3)
    } else {
        None
    }
}

/// Return a CSS hex color for a node based on heartbeat data.
fn node_color(hb: &HeartbeatMap, node_idx: usize) -> &'static str {
    for (agent_name, data) in hb {
        let by_agent = name_to_node(agent_name.as_str()) == Some(node_idx);
        let by_host  = data.host.as_deref()
            .and_then(|h| name_to_node(h))
            == Some(node_idx);
        if by_agent || by_host {
            if data.decommissioned.unwrap_or(false) { return "#636e72"; }
            return if data.online.unwrap_or(false) { "#00b894" } else { "#fdcb6e" };
        }
    }
    "#e17055"  // no data → assume offline
}

// ── Data fetchers ─────────────────────────────────────────────────────────────

async fn fetch_agents() -> HeartbeatMap {
    // Primary: /api/agents.  Fallback: /api/heartbeats.
    if let Ok(r) = gloo_net::http::Request::get("/api/agents").send().await {
        if let Ok(m) = r.json::<HeartbeatMap>().await { return m; }
    }
    let Ok(r2) = gloo_net::http::Request::get("/api/heartbeats").send().await else {
        return HeartbeatMap::default();
    };
    r2.json::<HeartbeatMap>().await.unwrap_or_default()
}

async fn fetch_soul_commits() -> Vec<SoulCommit> {
    let Ok(resp) = gloo_net::http::Request::get("/api/commits").send().await else {
        return vec![];
    };
    if !resp.ok() { return vec![]; }
    resp.json::<Vec<SoulCommit>>().await.unwrap_or_default()
}

// ── Component ─────────────────────────────────────────────────────────────────

#[component]
pub fn GeekView() -> impl IntoView {
    // Polling tick drives heartbeat + commit re-fetches (every 30 s)
    let (poll_tick, set_poll_tick) = create_signal(0u32);

    // Active traffic particles
    let (particles, set_particles) = create_signal(Vec::<Particle>::new());

    // SSE connection indicator
    let (sse_live, set_sse_live) = create_signal(false);

    // Rolling traffic log (last 20 events)
    let (traffic_log, set_traffic_log) = create_signal(Vec::<String>::new());

    // ── 30-second polling tick ───────────────────────────────────────────────
    {
        let st = set_poll_tick;
        leptos::spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(30_000).await;
                st.update(|t| *t = t.wrapping_add(1));
            }
        });
    }

    let agents       = create_resource(move || poll_tick.get(), |_| fetch_agents());
    let soul_commits = create_resource(move || poll_tick.get(), |_| fetch_soul_commits());

    // ── Particle animation ticker (40 ms) ────────────────────────────────────
    {
        // Use an Rc<Cell<bool>> abort flag so the loop stops on cleanup.
        let running       = std::rc::Rc::new(std::cell::Cell::new(true));
        let running_guard = running.clone();
        let sp            = set_particles;

        leptos::spawn_local(async move {
            while running.get() {
                gloo_timers::future::TimeoutFuture::new(TICK_MS).await;
                sp.update(|ps| {
                    for p in ps.iter_mut() { p.ticks += 1; }
                    ps.retain(|p| !p.done());
                });
            }
        });

        on_cleanup(move || { running_guard.set(false); });
    }

    // ── SSE — /bus/stream ─────────────────────────────────────────────────────
    // WriteSignal / ReadSignal are Copy, so we can move them into multiple closures.
    {
        let sp   = set_particles;
        let sl   = set_sse_live;
        let slog = set_traffic_log;

        if let Ok(es) = web_sys::EventSource::new("/bus/stream") {
            let es_cleanup = es.clone();

            let open_cb = Closure::<dyn FnMut()>::new(move || { sl.set(true); });
            es.set_onopen(Some(open_cb.as_ref().unchecked_ref()));
            open_cb.forget();

            let msg_cb = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
                let data = e.data().as_string().unwrap_or_default();
                if data.starts_with(':') || data.is_empty() { return; }
                let Ok(msg) = serde_json::from_str::<BusMessage>(&data) else { return };

                // Append to traffic log
                let from_s = msg.from.as_deref().unwrap_or("?").to_string();
                let to_s   = msg.to.as_deref().unwrap_or("*").to_string();
                let text_s = msg.text.as_deref().unwrap_or("")
                    .chars().take(60).collect::<String>();
                slog.update(|log| {
                    log.insert(0, format!("{from_s} → {to_s}: {text_s}"));
                    log.truncate(20);
                });

                // Spawn a particle for directed messages
                let fi = msg.from.as_deref().and_then(name_to_node);
                let ti = msg.to.as_deref().and_then(name_to_node);
                if let (Some(fi), Some(ti)) = (fi, ti) {
                    let (_, _, _, fx, fy, _) = NODES[fi];
                    let (_, _, _, tx, ty, _) = NODES[ti];
                    let color: &'static str = match msg.msg_type.as_deref() {
                        Some("heartbeat") => "#74b9ff",
                        Some("brain")     => "#a29bfe",
                        _                 => "#55efc4",
                    };
                    sp.update(|ps| {
                        ps.push(Particle {
                            color,
                            x0: fx, y0: fy,
                            xm: HUB_X, ym: HUB_Y,
                            x1: tx, y1: ty,
                            ticks: 0,
                        });
                        ps.truncate(30);  // cap simultaneous particles
                    });
                }
            });
            es.set_onmessage(Some(msg_cb.as_ref().unchecked_ref()));
            msg_cb.forget();

            let err_cb = Closure::<dyn FnMut(_)>::new(move |_: web_sys::ErrorEvent| {
                sl.set(false);
            });
            es.set_onerror(Some(err_cb.as_ref().unchecked_ref()));
            err_cb.forget();

            on_cleanup(move || { es_cleanup.close(); });
        }
        // If EventSource construction fails, sse_live stays false → static mode.
    }

    // ── Pre-compute static SVG strings ───────────────────────────────────────

    let viewbox = format!("0 0 {} {}", SVG_W as u32, SVG_H as u32);

    // Edge lines from each machine node to the hub
    let edges: Vec<_> = NODES.iter().map(|(_, _, _, cx, cy, _)| {
        view! {
            <line
                x1={cx.to_string()} y1={cy.to_string()}
                x2={HUB_X.to_string()} y2={HUB_Y.to_string()}
                stroke="#2d3436"
                stroke-width="1.5"
            />
        }
    }).collect();

    // ── Render ────────────────────────────────────────────────────────────────
    view! {
        <section class="section section-geek">

            <div class="section-header">
                <h2 class="section-title">
                    <span class="section-icon">"⬡"</span>
                    "Geek View"
                </h2>
                <div class="geek-legend">
                    <span class="legend-dot" style="background:#00b894">"  "</span>
                    " online "
                    <span class="legend-dot" style="background:#fdcb6e">"  "</span>
                    " degraded "
                    <span class="legend-dot" style="background:#e17055">"  "</span>
                    " offline"
                </div>
                {move || if sse_live.get() {
                    view! { <span class="conn-badge conn-live">"● live"</span> }.into_view()
                } else {
                    view! { <span class="conn-badge conn-waiting">"○ static"</span> }.into_view()
                }}
            </div>

            // ── SVG topology map ──────────────────────────────────────────────
            <div class="geek-svg-wrap">
                <svg
                    viewBox={viewbox}
                    class="geek-svg"
                    xmlns="http://www.w3.org/2000/svg"
                >
                    // Static edge lines
                    {edges}

                    // SquirrelBus hub (center)
                    <circle
                        cx={HUB_X.to_string()} cy={HUB_Y.to_string()}
                        r="22"
                        fill="#1e272e" stroke="#636e72" stroke-width="1.5"
                    />
                    <text
                        x={HUB_X.to_string()} y={(HUB_Y - 5.0).to_string()}
                        text-anchor="middle" font-size="8" fill="#b2bec3"
                    >"SquirrelBus"</text>
                    <text
                        x={HUB_X.to_string()} y={(HUB_Y + 7.0).to_string()}
                        text-anchor="middle" font-size="7" fill="#636e72"
                    >"hub"</text>

                    // Machine nodes — re-render when heartbeat data refreshes
                    {move || {
                        let hb = agents.get().unwrap_or_default();
                        NODES.iter().enumerate().map(|(i, node)| {
                            let (_, label, sublabel, cx, cy, services) = *node;
                            let color = node_color(&hb, i);
                            let nx = cx - NW2;
                            let ny = cy - NH2;
                            let service_str = services.join(" · ");
                            view! {
                                <g>
                                    // Background rect with status-colour border
                                    <rect
                                        x={nx.to_string()} y={ny.to_string()}
                                        width={(NW2 * 2.0).to_string()}
                                        height={(NH2 * 2.0).to_string()}
                                        rx="6" ry="6"
                                        fill="#1e272e"
                                        stroke={color}
                                        stroke-width="1.5"
                                    />
                                    // Status indicator dot
                                    <circle
                                        cx={(nx + 10.0).to_string()}
                                        cy={(ny + 10.0).to_string()}
                                        r="4" fill={color}
                                    />
                                    // Hostname label
                                    <text
                                        x={cx.to_string()}
                                        y={(cy - 8.0).to_string()}
                                        text-anchor="middle"
                                        font-size="11"
                                        fill="#dfe6e9"
                                        font-weight="bold"
                                    >{label}</text>
                                    // Agent / hardware sub-label
                                    <text
                                        x={cx.to_string()}
                                        y={(cy + 5.0).to_string()}
                                        text-anchor="middle"
                                        font-size="8"
                                        fill="#636e72"
                                    >{sublabel}</text>
                                    // Service chips
                                    <text
                                        x={cx.to_string()}
                                        y={(cy + 18.0).to_string()}
                                        text-anchor="middle"
                                        font-size="7"
                                        fill="#74b9ff"
                                    >{service_str}</text>
                                </g>
                            }
                        }).collect::<Vec<_>>().into_view()
                    }}

                    // Live traffic particles — re-render every tick
                    {move || {
                        particles.get().into_iter().map(|p| {
                            let (px, py) = p.pos();
                            view! {
                                <circle
                                    cx={px.to_string()}
                                    cy={py.to_string()}
                                    r="4"
                                    fill={p.color}
                                    opacity="0.9"
                                />
                            }
                        }).collect::<Vec<_>>().into_view()
                    }}
                </svg>
            </div>

            // ── Traffic event log ─────────────────────────────────────────────
            <div class="geek-traffic-log">
                <div class="geek-log-title">"Traffic"</div>
                {move || {
                    let log = traffic_log.get();
                    if log.is_empty() {
                        return view! {
                            <div class="geek-log-empty">"Waiting for bus events…"</div>
                        }.into_view();
                    }
                    log.into_iter().map(|entry| {
                        view! { <div class="geek-log-entry">{entry}</div> }
                    }).collect::<Vec<_>>().into_view()
                }}
            </div>

            // ── Soul commit timeline (gracefully absent if API missing) ────────
            {move || {
                let commits = soul_commits.get().unwrap_or_default();
                if commits.is_empty() {
                    return view! { <></> }.into_view();
                }
                view! {
                    <div class="geek-soul-timeline">
                        <div class="geek-soul-title">"Soul Commits"</div>
                        <div class="geek-soul-list">
                            {commits.into_iter().take(10).map(|c| {
                                let agent = c.agent.unwrap_or_default();
                                let hash  = c.hash.as_deref().unwrap_or("")
                                    .chars().take(7).collect::<String>();
                                let msg   = c.message.unwrap_or_default();
                                let ts    = c.ts.as_deref().unwrap_or("")
                                    .chars().take(16).collect::<String>();
                                view! {
                                    <div class="geek-soul-row">
                                        <span class="soul-agent">{agent}</span>
                                        <span class="soul-hash">{hash}</span>
                                        <span class="soul-msg">{msg}</span>
                                        <span class="soul-ts">{ts}</span>
                                    </div>
                                }
                            }).collect::<Vec<_>>().into_view()}
                        </div>
                    </div>
                }.into_view()
            }}

        </section>
    }
}
