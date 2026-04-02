//! Geek View — SVG topology map of the distributed agent brain.
//!
//! Nodes are driven by live /api/agents/status data — no hardcoded list.
//! Traffic particles flow along edges when ClawBus messages arrive.

use leptos::*;
use wasm_bindgen::prelude::*;
use serde::{Deserialize, Serialize};

// ── Layout constants ─────────────────────────────────────────────────────────

const SVG_W: f32 = 860.0;
const SVG_H: f32 = 520.0;

const HUB_X: f32 = SVG_W / 2.0;
const HUB_Y: f32 = SVG_H / 2.0;

const NW2: f32 = 72.0;
const NH2: f32 = 36.0;

const TICK_MS: u32 = 40;
const PARTICLE_TICKS: u32 = 28;

// ── Agent record (from /api/agents/status) ───────────────────────────────────

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct AgentRecord {
    name:          String,
    #[serde(rename = "onlineStatus")]
    online_status: Option<String>,
    host:          Option<String>,
    #[serde(rename = "gap_minutes")]
    gap_minutes:   Option<f64>,
}

// ── Particle ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Particle {
    x0: f32, y0: f32,
    xm: f32, ym: f32,
    x1: f32, y1: f32,
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

#[inline] fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }

// ── Soul commit ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SoulCommit {
    pub agent:   Option<String>,
    pub hash:    Option<String>,
    pub message: Option<String>,
    pub ts:      Option<String>,
}

// ── Bus message ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize)]
struct BusMessage {
    from:     Option<String>,
    to:       Option<String>,
    text:     Option<String>,
    #[serde(rename = "type")]
    msg_type: Option<String>,
}

// ── Layout: distribute nodes in a circle around the hub ──────────────────────

fn layout_positions(count: usize) -> Vec<(f32, f32)> {
    if count == 0 { return vec![]; }
    let radius = (SVG_W.min(SVG_H) / 2.0 - 80.0).max(120.0);
    (0..count).map(|i| {
        let angle = std::f32::consts::TAU * (i as f32) / (count as f32)
            - std::f32::consts::FRAC_PI_2; // start at top
        (HUB_X + radius * angle.cos(), HUB_Y + radius * angle.sin())
    }).collect()
}

// ── Data fetchers ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Deserialize)]
struct AgentsStatusResp {
    agents: Vec<AgentRecord>,
}

async fn fetch_agents() -> Vec<AgentRecord> {
    let Ok(resp) = gloo_net::http::Request::get(
        "http://146.190.134.110:8789/api/agents/status"
    ).send().await else { return vec![]; };
    if !resp.ok() { return vec![]; }
    // Try wrapper object first, fall back to bare array
    if let Ok(wrapped) = resp.json::<AgentsStatusResp>().await {
        return wrapped.agents;
    }
    let Ok(resp2) = gloo_net::http::Request::get(
        "http://146.190.134.110:8789/api/agents/status"
    ).send().await else { return vec![]; };
    resp2.json::<Vec<AgentRecord>>().await.unwrap_or_default()
}

async fn fetch_soul_commits() -> Vec<SoulCommit> {
    let Ok(resp) = gloo_net::http::Request::get(
        "http://146.190.134.110:8789/api/commits"
    ).send().await else { return vec![]; };
    if !resp.ok() { return vec![]; }
    resp.json::<Vec<SoulCommit>>().await.unwrap_or_default()
}

// ── Status color ─────────────────────────────────────────────────────────────

fn status_color(status: Option<&str>) -> &'static str {
    match status {
        Some("online")          => "#00b894",
        Some("degraded")        => "#fdcb6e",
        Some("decommissioned")  => "#636e72",
        _                       => "#e17055",
    }
}

// ── Component ─────────────────────────────────────────────────────────────────

#[component]
pub fn GeekView() -> impl IntoView {
    let (poll_tick, set_poll_tick)   = create_signal(0u32);
    let (particles, set_particles)   = create_signal(Vec::<Particle>::new());
    let (sse_live, set_sse_live)     = create_signal(false);
    let (traffic_log, set_traffic_log) = create_signal(Vec::<String>::new());

    // 30-second polling tick
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

    // Particle animation ticker (40 ms)
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

    // SSE — /bus/stream
    {
        let sp   = set_particles;
        let sl   = set_sse_live;
        let slog = set_traffic_log;

        if let Ok(es) = web_sys::EventSource::new(
            "http://146.190.134.110:8789/bus/stream"
        ) {
            let es_cleanup = es.clone();

            let open_cb = Closure::<dyn FnMut()>::new(move || { sl.set(true); });
            es.set_onopen(Some(open_cb.as_ref().unchecked_ref()));
            open_cb.forget();

            let msg_cb = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
                let data = e.data().as_string().unwrap_or_default();
                if data.starts_with(':') || data.is_empty() { return; }
                let Ok(msg) = serde_json::from_str::<BusMessage>(&data) else { return };

                let from_s = msg.from.as_deref().unwrap_or("?").to_string();
                let to_s   = msg.to.as_deref().unwrap_or("*").to_string();
                let text_s = msg.text.as_deref().unwrap_or("")
                    .chars().take(60).collect::<String>();
                slog.update(|log| {
                    log.insert(0, format!("{from_s} → {to_s}: {text_s}"));
                    log.truncate(20);
                });

                // Particles: only if we can match sender/receiver to a node index.
                // (We can't easily map names to layout positions here without
                //  the current agents list, so just log. The traffic log is the
                //  main live indicator.)
                let color: &'static str = match msg.msg_type.as_deref() {
                    Some("heartbeat") => "#74b9ff",
                    Some("brain")     => "#a29bfe",
                    _                 => "#55efc4",
                };
                // Emit a quick pulse from a random edge when we can't route precisely
                sp.update(|ps| {
                    ps.push(Particle {
                        color,
                        x0: HUB_X - 50.0, y0: HUB_Y - 50.0,
                        xm: HUB_X, ym: HUB_Y,
                        x1: HUB_X + 50.0, y1: HUB_Y + 50.0,
                        ticks: 0,
                    });
                    ps.truncate(30);
                });
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
                    view! { <span class="conn-badge conn-waiting">"○ polling"</span> }.into_view()
                }}
            </div>

            // SVG topology map — fully dynamic
            <div class="geek-svg-wrap">
                <svg
                    viewBox={viewbox}
                    class="geek-svg"
                    xmlns="http://www.w3.org/2000/svg"
                >
                    {move || {
                        let agent_list = agents.get().unwrap_or_default();
                        let positions  = layout_positions(agent_list.len());

                        // Edges
                        let edges = positions.iter().map(|(cx, cy)| {
                            view! {
                                <line
                                    x1={cx.to_string()} y1={cy.to_string()}
                                    x2={HUB_X.to_string()} y2={HUB_Y.to_string()}
                                    stroke="#2d3436" stroke-width="1.5"
                                />
                            }
                        }).collect::<Vec<_>>();

                        // Agent nodes
                        let nodes = agent_list.iter().zip(positions.iter()).map(|(agent, (cx, cy))| {
                            let color   = status_color(agent.online_status.as_deref());
                            let nx      = cx - NW2;
                            let ny      = cy - NH2;
                            let label   = agent.name.clone();
                            let sublbl  = agent.host.clone().unwrap_or_default();
                            let status  = agent.online_status.clone().unwrap_or_else(|| "unknown".into());
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
                                        x={cx.to_string()}
                                        y={(cy - 8.0).to_string()}
                                        text-anchor="middle" font-size="11"
                                        fill="#dfe6e9" font-weight="bold"
                                    >{label}</text>
                                    <text
                                        x={cx.to_string()}
                                        y={(cy + 6.0).to_string()}
                                        text-anchor="middle" font-size="8"
                                        fill="#636e72"
                                    >{sublbl}</text>
                                    <text
                                        x={cx.to_string()}
                                        y={(cy + 18.0).to_string()}
                                        text-anchor="middle" font-size="7"
                                        fill="#74b9ff"
                                    >{status}</text>
                                </g>
                            }
                        }).collect::<Vec<_>>();

                        view! {
                            <>
                                {edges}
                                // Hub
                                <circle
                                    cx={HUB_X.to_string()} cy={HUB_Y.to_string()}
                                    r="24"
                                    fill="#1e272e" stroke="#636e72" stroke-width="1.5"
                                />
                                <text
                                    x={HUB_X.to_string()} y={(HUB_Y - 5.0).to_string()}
                                    text-anchor="middle" font-size="8" fill="#b2bec3"
                                >"ClawBus"</text>
                                <text
                                    x={HUB_X.to_string()} y={(HUB_Y + 7.0).to_string()}
                                    text-anchor="middle" font-size="7" fill="#636e72"
                                >"hub"</text>
                                {nodes}
                            </>
                        }
                    }}

                    // Particles
                    {move || {
                        particles.get().into_iter().map(|p| {
                            let (px, py) = p.pos();
                            view! {
                                <circle
                                    cx={px.to_string()} cy={py.to_string()}
                                    r="4" fill={p.color} opacity="0.9"
                                />
                            }
                        }).collect::<Vec<_>>().into_view()
                    }}
                </svg>
            </div>

            // Traffic log
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

            // Soul commit timeline
            {move || {
                let commits = soul_commits.get().unwrap_or_default();
                if commits.is_empty() { return view! { <></> }.into_view(); }
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
