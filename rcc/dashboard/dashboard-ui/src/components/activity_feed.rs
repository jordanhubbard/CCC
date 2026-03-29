use leptos::*;

use crate::context::DashboardContext;
use crate::types::QueueItem;

fn relative_time(ts: &str) -> String {
    let now_ms = js_sys::Date::now();
    let parsed_ms = js_sys::Date::parse(ts);
    if parsed_ms.is_nan() {
        return ts.split('T').next().unwrap_or(ts).to_string();
    }
    let diff_secs = ((now_ms - parsed_ms) / 1000.0) as i64;
    if diff_secs < 0 {
        return "just now".to_string();
    }
    if diff_secs < 60 {
        format!("{}s ago", diff_secs)
    } else if diff_secs < 3600 {
        format!("{}m ago", diff_secs / 60)
    } else if diff_secs < 86400 {
        format!("{}h ago", diff_secs / 3600)
    } else {
        format!("{}d ago", diff_secs / 86400)
    }
}

fn status_class(status: Option<&str>) -> &'static str {
    match status {
        Some("completed") | Some("done") => "feed-event status-completed",
        Some("in_progress") | Some("in-progress") => "feed-event status-in-progress",
        Some("failed") | Some("error") => "feed-event status-failed",
        _ => "feed-event status-pending",
    }
}

fn status_label(status: Option<&str>) -> &'static str {
    match status {
        Some("completed") | Some("done") => "done",
        Some("in_progress") | Some("in-progress") => "active",
        Some("failed") | Some("error") => "failed",
        _ => "pending",
    }
}

#[component]
pub fn ActivityFeed() -> impl IntoView {
    let ctx = use_context::<DashboardContext>().expect("DashboardContext missing");
    let queue = ctx.queue;

    view! {
        <section class="section section-activity">
            <h2 class="section-title">
                <span class="section-icon">"◎"</span>
                "Activity"
            </h2>
            <div class="activity-feed">
                {move || {
                    let q = queue.get().unwrap_or_default();

                    // Combine active items + completed, sort newest first
                    let mut all: Vec<QueueItem> = q.items.clone();
                    if let Some(mut done) = q.completed {
                        all.append(&mut done);
                    }
                    all.sort_by(|a, b| {
                        b.created_at.as_deref().unwrap_or("").cmp(a.created_at.as_deref().unwrap_or(""))
                    });
                    let events: Vec<QueueItem> = all.into_iter().take(10).collect();

                    if events.is_empty() {
                        return view! {
                            <div class="feed-empty">"No activity yet."</div>
                        }.into_view();
                    }

                    events.into_iter().map(|item| {
                        let title = item.title.clone();
                        let _status = item.status.as_deref().unwrap_or("pending").to_string();
                        let assignee = item.assignee.clone().unwrap_or_default();
                        let ts = item.created_at.as_deref()
                            .map(relative_time)
                            .unwrap_or_default();
                        let cls = status_class(item.status.as_deref());
                        let label = status_label(item.status.as_deref());
                        view! {
                            <div class=cls>
                                <span class="feed-status-badge">{label}</span>
                                <span class="feed-title">{title}</span>
                                <span class="feed-meta">
                                    {if !assignee.is_empty() {
                                        format!("{assignee} · ")
                                    } else {
                                        String::new()
                                    }}
                                    {ts}
                                </span>
                            </div>
                        }
                    }).collect::<Vec<_>>().into_view()
                }}
            </div>
        </section>
    }
}
