//! Geek View — SVG topology map of the distributed agent brain.
//!
//! Machines are primary nodes derived dynamically from /api/agents.
//! Live traffic particles flow along edges when SquirrelBus messages arrive.
//! Falls back gracefully to a static (polled) map if SSE is unavailable.

use leptos::*;
use wasm_bindgen::prelude::*;
use serde::{Deserialize, Serialize};

use crate::types::{AgentInfo, AgentList, BusMessage};

// ── Layout constants ─────────────────────────────────────────────────────────

const SVG_W: f32 = 800.0;
const SVG_H: f32 = 490.0;

/// Central SquirrelBus hub
const HUB_X: f32 = 400.0;
const HUB_Y: f32 = 248.0;

/// Orbit radius for node placement
const ORBIT_R: f32 = 185.0;

/// Machine node half-dimensions
const NW2: f32 = 68.0;
const NH2: f32 = 34.0;

/// Particle animation: 40ms tick ≈ 25fps, travel ≈ 1.1 s
const TICK_MS: u32 = 40;
const PARTICLE_TICKS: u32 = 28;

// ── Dynamic node layout ───────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct NodeLayout {
    /// stable routing key — agent name (lowercase)
    key:      String,
    label:    String,
    sublabel: String,
    cx:       f32,
    cy:       f32,
    services: Vec<String>,
    color:    &'static str,
}

/// Place N nodes evenly around the hub.  First node at top (−π/2).
fn radial_positions(n: usize) -> Vec<(f32, f32)> {
    if n == 0 { return vec![]; }
    (0..n).map(|i| {
        let angle = -std::f32::consts::FRAC_PI_2
            + (i as f32) * 2.0 * std::f32::consts::PI / (n as f32);
        let x = HUB_X + ORBIT_R * angle.cos();
        let y = HUB_Y + ORBIT_R * angle.sin();
        // Clamp so nodes don't clip the SVG border
        let x = x.max(NW2 + 4.0).min(SVG_W - NW2 - 4.0);
        let y = y.max(NH2 + 4.0).min(SVG_H - NH2 - 4.0);
        (x, y)
    }).collect()
}

fn agent_color(info: &AgentInfo) -> &'static str {
    let status = info.online_status.as_deref().unwrap_or("offline");
    match status {
        "online"        => "#00b894",
        "degraded"      => "#fdcb6e",
        "decommissioned"=> "#636e72",
        _               => "#e17055",
    }
}

fn agent_sublabel(info: &AgentInfo) -> String {
    let mut parts: Vec<&str> = vec![];
    if let Some(name) = &info.name { parts.push(name.as_str()); }
    if let Some(caps) = &info.capabilities {
        if let Some(model) = &caps.gpu_model {
            if !model.is_empty() { parts.push(model.as_str()); }
        }
    }
    parts.join(" · ")
}

fn agent_services(info: &AgentInfo) -> Vec<String> {
    let mut svc: Vec<String> = vec![];
    if let Some(caps) = &info.capabilities {
        if caps.gpu.unwrap_or(false) {
            if let Some(n) = caps.gpu_count {
                if n > 0 { svc.push(format!("{}× gpu", n)); }
            } else {
                svc.push("gpu".into());
            }
        }
        if caps.vllm.unwrap_or(false) { svc.push("vllm".into()); }
        if caps.claude_cli.unwrap_or(false) { svc.push("claude".into()); }
        if caps.inference_key.unwrap_or(false) {
            if let Some(p) = &caps.inference_provider {
                svc.push(p.clone());
            }
        }
    }
    svc
}

fn build_layout(agents: &AgentList) -> Vec<NodeLayout> {
    // Filter out agents we don't want to show
    let visible: Vec<&AgentInfo> = agents.iter()
        .filter(|a| {
            a.online_status.as_deref() != Some("decommissioned")
        })
        .collect();

    let positions = radial_positions(visible.len());

    visible.iter().enumerate().map(|(i, info)| {
        let (cx, cy) = positions[i];
        let key = info.name.clone().unwrap_or_default().to_lowercase();
        let label = info.host.clone().unwrap_or_else(|| key.clone());
        NodeLayout {
            key,
            label,
            sublabel: agent_sublabel(info),
            cx,
            cy,
            services: agent_services(info),
            color: agent_color(info),
        }
    }).collect()
}

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

// ── Data fetchers ─────────────────────────────────────────────────────────────

async fn fetch_agents() -> AgentList {
    let Ok(r) = gloo_net::http::Request::get("/api/agents").send().await else {
        return AgentList::default();
    };
    r.json::<AgentList>().await.unwrap_or_default()
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
    // Polling tick drives re-fetches every 30 s
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
    // We cache the latest layout so SSE particles can map agent names → coords.
    let (layout_cache, set_layout_cache) = create_signal(Vec::<NodeLayout>::new());

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

                // Spawn a particle using layout cache for coord lookup
                let layout = layout_cache.get_untracked();
                let find = |name: &str| -> Option<(f32, f32)> {
                    let n = name.to_lowercase();
                    layout.iter().find(|node| {
                        node.key.contains(&n) || n.contains(&node.key)
                    }).map(|node| (node.cx, node.cy))
                };

                if let (Some(from), Some(to)) = (
                    msg.from.as_deref().and_then(|f| find(f)),
                    msg.to.as_deref().and_then(|t| find(t)),
                ) {
                    let color: &'static str = match msg.msg_type.as_deref() {
                        Some("heartbeat") => "#74b9ff",
                        Some("brain")     => "#a29bfe",
                        _                 => "#55efc4",
                    };
                    sp.update(|ps| {
                        ps.push(Particle {
                            color,
                            x0: from.0, y0: from.1,
                            xm: HUB_X, ym: HUB_Y,
                            x1: to.0, y1: to.1,
                            ticks: 0,
                        });
                        ps.truncate(30);
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
    }

    // ── Render ────────────────────────────────────────────────────────────────
    let viewbox = format!("0 0 {} {}", SVG_W as u32, SVG_H as u32);

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
                    {move || {
                        let agent_list = agents.get().unwrap_or_default();
                        let layout = build_layout(&agent_list);

                        // Update layout cache for SSE particle routing
                        set_layout_cache.set(layout.clone());

                        let edges: Vec<_> = layout.iter().map(|node| {
                            view! {
                                <line
                                    x1={node.cx.to_string()} y1={node.cy.to_string()}
                                    x2={HUB_X.to_string()} y2={HUB_Y.to_string()}
                                    stroke="#2d3436"
                                    stroke-width="1.5"
                                />
                            }
                        }).collect();

                        let nodes_svg: Vec<_> = layout.iter().map(|node| {
                            let nx = node.cx - NW2;
                            let ny = node.cy - NH2;
                            let service_str = node.services.join(" · ");
                            let color = node.color;
                            view! {
                                <g>
                                    <rect
                                        x={nx.to_string()} y={ny.to_string()}
                                        width={(NW2 * 2.0).to_string()}
                                        height={(NH2 * 2.0).to_string()}
                                        rx="6" ry="6"
                                        fill="#1e272e"
                                        stroke={color}
                                        stroke-width="1.5"
                                    />
                                    <circle
                                        cx={(nx + 10.0).to_string()}
                                        cy={(ny + 10.0).to_string()}
                                        r="4" fill={color}
                                    />
                                    <text
                                        x={node.cx.to_string()}
                                        y={(node.cy - 8.0).to_string()}
                                        text-anchor="middle"
                                        font-size="11"
                                        fill="#dfe6e9"
                                        font-weight="bold"
                                    >{node.label.clone()}</text>
                                    <text
                                        x={node.cx.to_string()}
                                        y={(node.cy + 5.0).to_string()}
                                        text-anchor="middle"
                                        font-size="8"
                                        fill="#636e72"
                                    >{node.sublabel.clone()}</text>
                                    <text
                                        x={node.cx.to_string()}
                                        y={(node.cy + 18.0).to_string()}
                                        text-anchor="middle"
                                        font-size="7"
                                        fill="#74b9ff"
                                    >{service_str}</text>
                                </g>
                            }
                        }).collect();

                        view! {
                            <>
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
                                {nodes_svg}
                            </>
                        }.into_view()
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

            // ── Soul commit timeline ──────────────────────────────────────────
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
