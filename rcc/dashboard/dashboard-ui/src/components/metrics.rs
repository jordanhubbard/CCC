use leptos::*;

use crate::context::DashboardContext;

#[component]
pub fn Metrics() -> impl IntoView {
    let ctx = use_context::<DashboardContext>().expect("DashboardContext missing");
    let queue = ctx.queue;
    let heartbeats = ctx.heartbeats;

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
                    let online_agents = hb
                        .values()
                        .filter(|h| h.online.unwrap_or(false))
                        .count();
                    let total_agents = hb.len();
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
