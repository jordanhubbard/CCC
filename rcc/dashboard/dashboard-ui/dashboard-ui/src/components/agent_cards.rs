use leptos::*;

use crate::context::DashboardContext;
use crate::types::{AgentInfo, HeartbeatData};

/// Returns "X min ago", "Xh ago", "Xd ago", or "just now" from an ISO-8601 string.
/// Falls back to showing just the time if parsing fails.
fn relative_time(ts: &str) -> String {
    // Parse seconds since epoch using basic string math (no chrono in WASM)
    // Format: 2026-03-28T06:27:36.849Z
    let now_ms = js_sys::Date::now() as u64;
    let now_sec = now_ms / 1000;

    // Parse ISO timestamp manually
    let parse = || -> Option<u64> {
        let (date_part, time_part) = ts.split_once('T')?;
        let mut dp = date_part.split('-');
        let y: u64 = dp.next()?.parse().ok()?;
        let m: u64 = dp.next()?.parse().ok()?;
        let d: u64 = dp.next()?.parse().ok()?;
        let time_clean = time_part.trim_end_matches('Z');
        let mut tp = time_clean.split(':');
        let h: u64 = tp.next()?.parse().ok()?;
        let mi: u64 = tp.next()?.parse().ok()?;
        let s: f64 = tp.next().unwrap_or("0").parse().ok()?;
        // Days since epoch (approximate, ignores leap seconds)
        let days = days_since_epoch(y, m, d);
        Some(days * 86400 + h * 3600 + mi * 60 + s as u64)
    };

    if let Some(ts_sec) = parse() {
        let diff = now_sec.saturating_sub(ts_sec);
        if diff < 60 {
            return "just now".to_string();
        } else if diff < 3600 {
            return format!("{}m ago", diff / 60);
        } else if diff < 86400 {
            return format!("{}h ago", diff / 3600);
        } else {
            return format!("{}d ago", diff / 86400);
        }
    }

    // Fallback: show HH:MM
    if let Some(t) = ts.split('T').nth(1) {
        t[..t.len().min(5)].to_string()
    } else {
        ts.to_string()
    }
}

fn days_since_epoch(y: u64, m: u64, d: u64) -> u64 {
    // Gregorian days since 1970-01-01 (close enough for display purposes)
    let y = if m <= 2 { y - 1 } else { y };
    let m = if m <= 2 { m + 12 } else { m };
    let a = y / 100;
    let b = 2 - a + a / 4;
    let jd = ((365.25 * (y + 4716) as f64) as u64)
        + ((30.6001 * (m + 1) as f64) as u64)
        + d + b as u64;
    jd.saturating_sub(2440588) // 2440588 = JD of 1970-01-01
}

#[component]
pub fn AgentCards() -> impl IntoView {
    let ctx = use_context::<DashboardContext>().expect("DashboardContext missing");
    let heartbeats = ctx.heartbeats;
    let agents = ctx.agents;

    view! {
        <section class="section section-agents">
            <h2 class="section-title">
                <span class="section-icon">"●"</span>
                "Agents"
            </h2>
            <div class="agent-grid">
                {move || {
                    let hb_map = heartbeats.get().unwrap_or_default();
                    let agent_list = agents.get().unwrap_or_default();

                    if hb_map.is_empty() && agent_list.is_empty() {
                        return view! { <p class="loading">"Loading agents..."</p> }.into_view();
                    }

                    // Merge: start from heartbeat map (has online status), enrich with /api/agents data
                    let mut names: Vec<String> = hb_map.keys().cloned().collect();
                    // Add any agents from /api/agents not in heartbeat
                    for a in &agent_list {
                        if let Some(n) = &a.name {
                            if !names.contains(n) {
                                names.push(n.clone());
                            }
                        }
                    }
                    names.sort();

                    // Filter out decommissioned
                    let names: Vec<String> = names.into_iter().filter(|n| {
                        let hb = hb_map.get(n);
                        !hb.map(|h| h.decommissioned.unwrap_or(false)).unwrap_or(false)
                    }).collect();

                    if names.is_empty() {
                        return view! { <p class="empty">"No agents registered"</p> }.into_view();
                    }

                    names.into_iter().map(|name| {
                        let hb: HeartbeatData = hb_map.get(&name).cloned().unwrap_or_default();
                        let info: Option<AgentInfo> = agent_list.iter()
                            .find(|a| a.name.as_deref() == Some(&name))
                            .cloned();

                        // Staleness check: even if onlineStatus="online", flag if >2h since last heartbeat
                        let now_sec = (js_sys::Date::now() as u64) / 1000;
                        let hb_age_secs = hb.ts.as_deref()
                            .and_then(|ts| {
                                // Reuse parse logic inline
                                let (dp, tp) = ts.split_once('T')?;
                                let mut d = dp.split('-');
                                let y: u64 = d.next()?.parse().ok()?;
                                let m: u64 = d.next()?.parse().ok()?;
                                let day: u64 = d.next()?.parse().ok()?;
                                let tc = tp.trim_end_matches('Z');
                                let mut t = tc.split(':');
                                let h: u64 = t.next()?.parse().ok()?;
                                let mi: u64 = t.next()?.parse().ok()?;
                                let s: f64 = t.next().unwrap_or("0").parse().ok()?;
                                let days = days_since_epoch(y, m, day);
                                Some(now_sec.saturating_sub(days * 86400 + h * 3600 + mi * 60 + s as u64))
                            })
                            .unwrap_or(u64::MAX);

                        let online = hb.online.unwrap_or(false);
                        let status_class = if !online {
                            "offline"
                        } else if hb_age_secs > 7200 {
                            "stale"   // online but heartbeat is >2h old
                        } else {
                            "online"
                        };

                        let ts_display = hb.ts.as_deref()
                            .map(relative_time)
                            .unwrap_or_else(|| "never".to_string());

                        let host = hb.host.clone()
                            .or_else(|| info.as_ref().and_then(|i| i.host.clone()))
                            .unwrap_or_default();

                        let caps = info.as_ref().and_then(|i| i.capabilities.as_ref()).cloned();
                        let llm = info.as_ref().and_then(|i| i.llm.as_ref()).cloned();

                        // GPU badge
                        let gpu_badge = caps.as_ref().and_then(|c| {
                            if c.gpu.unwrap_or(false) {
                                let model = c.gpu_model.clone().unwrap_or_else(|| "GPU".into());
                                let count = c.gpu_count.unwrap_or(1);
                                let vram = c.gpu_vram_gb.map(|v| format!(" {}GB", v)).unwrap_or_default();
                                Some(format!("{}× {}{}", count, model, vram))
                            } else {
                                None
                            }
                        });

                        // Active model
                        let active_model = llm.as_ref()
                            .and_then(|l| l.models.as_ref())
                            .and_then(|m| m.first().cloned())
                            .or_else(|| caps.as_ref().and_then(|c| c.vllm_model.clone()))
                            .or_else(|| hb.model.clone());

                        // Capability pills
                        let mut pill_items: Vec<&'static str> = vec![];
                        if caps.as_ref().map(|c| c.claude_cli.unwrap_or(false)).unwrap_or(false) {
                            pill_items.push("claude");
                        }
                        if caps.as_ref().map(|c| c.vllm.unwrap_or(false)).unwrap_or(false) {
                            pill_items.push("vllm");
                        }
                        if caps.as_ref().map(|c| c.inference_key.unwrap_or(false)).unwrap_or(false) {
                            pill_items.push("inference");
                        }

                        view! {
                            <div class=format!("agent-card {status_class}")>
                                <div class="agent-header">
                                    <span class=format!("agent-dot dot-{status_class}")></span>
                                    <span class="agent-name">{name.clone()}</span>
                                    {if !pill_items.is_empty() {
                                        view! {
                                            <span style="display:flex;gap:3px;margin-left:auto;flex-wrap:wrap;">
                                                {pill_items.iter().map(|p| view! {
                                                    <span style="font-size:9px;background:var(--surface3,#2d333b);color:var(--text-dim);padding:1px 5px;border-radius:8px;letter-spacing:0.02em;">
                                                        {*p}
                                                    </span>
                                                }).collect::<Vec<_>>()}
                                            </span>
                                        }.into_view()
                                    } else {
                                        view! { <span></span> }.into_view()
                                    }}
                                </div>
                                <div class="agent-meta">
                                    {if !host.is_empty() {
                                        view! {
                                            <span class="meta-item">
                                                <span class="meta-label">"host:"</span>
                                                {host}
                                            </span>
                                        }.into_view()
                                    } else { view! { <span></span> }.into_view() }}
                                    {if let Some(gpu) = gpu_badge {
                                        view! {
                                            <span class="meta-item">
                                                <span class="meta-label">"gpu:"</span>
                                                <span style="color:var(--green,#3fb950);font-weight:500;">{gpu}</span>
                                            </span>
                                        }.into_view()
                                    } else { view! { <span></span> }.into_view() }}
                                    {if let Some(model) = active_model {
                                        view! {
                                            <span class="meta-item">
                                                <span class="meta-label">"model:"</span>
                                                <span style="font-family:monospace;font-size:10px;">{model}</span>
                                            </span>
                                        }.into_view()
                                    } else { view! { <span></span> }.into_view() }}
                                </div>
                                <div class="agent-ts">
                                    {if status_class == "stale" {
                                        view! {
                                            <span style="color:var(--orange);font-size:10px;">
                                                "⚠ stale · "
                                            </span>
                                        }.into_view()
                                    } else if online {
                                        view! {
                                            <span style="color:var(--green,#3fb950);font-size:10px;">
                                                "● online · "
                                            </span>
                                        }.into_view()
                                    } else {
                                        view! {
                                            <span style="color:var(--text-dimmer,#484f58);font-size:10px;">
                                                "○ last: "
                                            </span>
                                        }.into_view()
                                    }}
                                    {ts_display}
                                </div>
                            </div>
                        }
                    }).collect::<Vec<_>>().into_view()
                }}
            </div>
        </section>
    }
}
