use leptos::*;

use crate::context::DashboardContext;

/// Parse an ISO-8601 timestamp string to seconds since Unix epoch.
fn parse_iso_to_epoch_m(ts: &str) -> Option<u64> {
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
    // Gregorian days since 1970-01-01
    let y2 = if m <= 2 { y - 1 } else { y };
    let m2 = if m <= 2 { m + 12 } else { m };
    let a = y2 / 100;
    let b = 2 - a + a / 4;
    let jd = ((365.25 * (y2 + 4716) as f64) as u64)
        + ((30.6001 * (m2 + 1) as f64) as u64)
        + d + b as u64;
    let days = jd.saturating_sub(2440588);
    Some(days * 86400 + h * 3600 + mi * 60 + s as u64)
}

#[component]
pub fn Metrics() -> impl IntoView {
    let ctx = use_context::<DashboardContext>().expect("DashboardContext missing");
    let queue = ctx.queue;
    let heartbeats = ctx.heartbeats;
    let agents_res = ctx.agents;

    view! {
        <section class="section section-metrics">
            <h2 class="section-title">
                <span class="section-icon">"▲"</span>
                "Metrics"
            </h2>
            <div class="metrics-grid">
                {move || {
                    let q = queue.get().unwrap_or_default();
                    let hb = heartbeats.get().unwrap_or_default();
                    let agent_list = agents_res.get().unwrap_or_default();

                    let now_secs = (js_sys::Date::now() as u64) / 1000;
                    let cutoff_24h = now_secs.saturating_sub(86400);

                    // Exclude ideas and completed/closed from "active" queue depth
                    let active_items: Vec<_> = q.items.iter()
                        .filter(|i| !matches!(
                            i.status.as_deref(),
                            Some("completed") | Some("done") | Some("closed") | Some("cancelled")
                        ))
                        .filter(|i| i.priority.as_deref() != Some("idea"))
                        .collect();
                    let total = active_items.len();
                    let pending = active_items.iter()
                        .filter(|i| matches!(
                            i.status.as_deref(),
                            Some("pending") | Some("incubating")
                        ))
                        .count();
                    let in_progress = active_items.iter()
                        .filter(|i| matches!(
                            i.status.as_deref(),
                            Some("in_progress") | Some("in-progress") | Some("claimed")
                        ))
                        .count();
                    let done_count = q.items.iter()
                        .filter(|i| matches!(
                            i.status.as_deref(),
                            Some("completed") | Some("done") | Some("closed")
                        ))
                        .count()
                        + q.completed.as_ref().map(|c| c.len()).unwrap_or(0);

                    // Filter agents to 24h activity window (matching agent cards)
                    let is_recent = |name: &str| -> bool {
                        let agent_last = agent_list.iter()
                            .find(|a| a.name.as_deref() == Some(name))
                            .and_then(|a| a.last_seen.as_deref())
                            .and_then(parse_iso_to_epoch_m);
                        let hb_last = hb.get(name)
                            .and_then(|h| h.ts.as_deref())
                            .and_then(parse_iso_to_epoch_m);
                        agent_last.or(hb_last).unwrap_or(0) >= cutoff_24h
                    };

                    let online_agents = hb
                        .iter()
                        .filter(|(name, h)| {
                            h.online.unwrap_or(false)
                                && !h.decommissioned.unwrap_or(false)
                                && is_recent(name)
                        })
                        .count();
                    let total_agents = hb
                        .iter()
                        .filter(|(name, h)| {
                            !h.decommissioned.unwrap_or(false) && is_recent(name)
                        })
                        .count();
                    let completion_rate = if total + done_count > 0 {
                        (done_count as f64 / (total + done_count) as f64 * 100.0) as u32
                    } else {
                        0
                    };
                    view! {
                        <div class="metric-card">
                            <div class="metric-value">{total}</div>
                            <div class="metric-label">"Queue Depth"</div>
                        </div>
                        <div class="metric-card">
                            <div class="metric-value metric-pending">{pending}</div>
                            <div class="metric-label">"Pending"</div>
                        </div>
                        <div class="metric-card">
                            <div class="metric-value metric-active">{in_progress}</div>
                            <div class="metric-label">"In Progress"</div>
                        </div>
                        <div class="metric-card">
                            <div class="metric-value metric-done">{done_count}</div>
                            <div class="metric-label">"Completed"</div>
                        </div>
                        <div class="metric-card">
                            <div class="metric-value">
                                {online_agents}
                                "/"
                                {total_agents}
                            </div>
                            <div class="metric-label">"Agents Online"</div>
                        </div>
                        <div class="metric-card">
                            <div class="metric-value metric-rate">
                                {completion_rate}
                                "%"
                            </div>
                            <div class="metric-label">"Completion Rate"</div>
                        </div>
                    }
                        .into_view()
                }}
            </div>
        </section>
    }
}
