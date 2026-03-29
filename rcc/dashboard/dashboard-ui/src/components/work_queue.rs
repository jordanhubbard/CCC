use leptos::*;

use crate::context::DashboardContext;
use crate::types::QueueItem;

fn priority_class(p: &str) -> &'static str {
    match p {
        "critical" => "prio-critical",
        "high" => "prio-high",
        "medium" => "prio-medium",
        "low" => "prio-low",
        "idea" => "prio-idea",
        _ => "prio-default",
    }
}

fn status_label(s: &str) -> &'static str {
    match s {
        "pending" => "pending",
        "claimed" => "claimed",
        "in_progress" | "in-progress" => "in-progress",
        "done" | "completed" => "done",
        "closed" => "closed",
        "failed" => "failed",
        "blocked" => "blocked",
        "deferred" => "deferred",
        "incubating" => "incubating",
        _ => "unknown",
    }
}

fn is_active_status(s: &str) -> bool {
    matches!(s, "pending" | "claimed" | "in-progress" | "in_progress")
}

fn days_since_epoch(y: u64, m: u64, d: u64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let m = if m <= 2 { m + 12 } else { m };
    let a = y / 100;
    let b = 2u64.saturating_add(a / 4).saturating_sub(a);
    let jd = ((365.25 * (y + 4716) as f64) as u64)
        + ((30.6001 * (m + 1) as f64) as u64)
        + d + b;
    jd.saturating_sub(2440588)
}

fn age_display(created_at: &str) -> String {
    let now_sec = (js_sys::Date::now() as u64) / 1000;
    let parse = || -> Option<u64> {
        let (date_part, time_part) = created_at.split_once('T')?;
        let mut dp = date_part.split('-');
        let y: u64 = dp.next()?.parse().ok()?;
        let m: u64 = dp.next()?.parse().ok()?;
        let d: u64 = dp.next()?.parse().ok()?;
        let time_clean = time_part.trim_end_matches('Z');
        let mut tp = time_clean.split(':');
        let h: u64 = tp.next()?.parse().ok()?;
        let mi: u64 = tp.next()?.parse().ok()?;
        let s: f64 = tp.next().unwrap_or("0").parse().ok()?;
        Some(days_since_epoch(y, m, d) * 86400 + h * 3600 + mi * 60 + s as u64)
    };
    if let Some(ts_sec) = parse() {
        let diff = now_sec.saturating_sub(ts_sec);
        if diff < 60 { return "just now".to_string(); }
        if diff < 3600 { return format!("{}m ago", diff / 60); }
        if diff < 86400 { return format!("{}h ago", diff / 3600); }
        return format!("{}d ago", diff / 86400);
    }
    created_at.split('T').next().unwrap_or(created_at).to_string()
}

#[component]
pub fn WorkQueue() -> impl IntoView {
    let ctx = use_context::<DashboardContext>().expect("DashboardContext missing");
    let queue = ctx.queue;
    let (filter, set_filter) = create_signal(String::new());
    let (expanded_id, set_expanded_id) = create_signal(Option::<String>::None);
    let (show_completed, set_show_completed) = create_signal(false);

    fn priority_rank(p: &str) -> u8 {
        match p {
            "critical" => 0,
            "high"     => 1,
            "medium"   => 2,
            "low"      => 3,
            "idea"     => 9,
            _          => 5,
        }
    }

    let filtered_items = move || {
        let q = queue.get().unwrap_or_default();
        let f = filter.get().to_lowercase();
        let mut items: Vec<QueueItem> = q
            .items
            .into_iter()
            .filter(|item| {
                if item.priority.as_deref() == Some("idea") {
                    return false; // ideas shown in Idea Incubator
                }
                // Only show active items in the main table (unless filter is set)
                let status = item.status.as_deref().unwrap_or("pending");
                if f.is_empty() && !is_active_status(status) {
                    return false;
                }
                if f.is_empty() {
                    return true;
                }
                item.title.to_lowercase().contains(&f)
                    || item.id.to_lowercase().contains(&f)
                    || item
                        .assignee
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&f)
                    || status.to_lowercase().contains(&f)
            })
            .collect();

        // Sort: jkh decision items first, then by priority rank, then by created_at
        items.sort_by(|a, b| {
            let a_jkh = a.assignee.as_deref().map(|x| x.to_lowercase() == "jkh").unwrap_or(false);
            let b_jkh = b.assignee.as_deref().map(|x| x.to_lowercase() == "jkh").unwrap_or(false);
            if a_jkh != b_jkh { return b_jkh.cmp(&a_jkh); } // jkh first
            let a_rank = priority_rank(a.priority.as_deref().unwrap_or("medium"));
            let b_rank = priority_rank(b.priority.as_deref().unwrap_or("medium"));
            a_rank.cmp(&b_rank)
        });

        items
    };

    let completed_items = move || {
        let q = queue.get().unwrap_or_default();
        let f = filter.get().to_lowercase();
        q.completed
            .unwrap_or_default()
            .into_iter()
            .filter(|item| {
                if f.is_empty() {
                    return true;
                }
                item.title.to_lowercase().contains(&f)
                    || item.id.to_lowercase().contains(&f)
            })
            .take(20) // show last 20 completed
            .collect::<Vec<_>>()
    };

    view! {
        <section class="section section-queue">
            <div class="section-header">
                <h2 class="section-title">
                    <span class="section-icon">"▤"</span>
                    "Work Queue"
                    {move || {
                        let count = filtered_items().len();
                        view! { <span class="badge">{count}</span> }
                    }}
                </h2>
                <div class="queue-controls">
                    <input
                        type="text"
                        class="filter-input"
                        placeholder="filter by title, assignee, status..."
                        on:input=move |e| {
                            set_filter.set(event_target_value(&e));
                        }
                    />
                    <label class="toggle-label">
                        <input
                            type="checkbox"
                            on:change=move |e| {
                                set_show_completed
                                    .set(
                                        event_target_checked(&e),
                                    );
                            }
                        />
                        " show completed"
                    </label>
                </div>
            </div>

            <div class="queue-table-wrap">
                <table class="queue-table">
                    <thead>
                        <tr>
                            <th>"ID"</th>
                            <th>"Title"</th>
                            <th>"Priority"</th>
                            <th>"Assignee"</th>
                            <th>"Status"</th>
                            <th>"Age"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            let items = filtered_items();
                            if items.is_empty() {
                                return view! {
                                    <tr>
                                        <td colspan="6" class="empty-row">"No items"</td>
                                    </tr>
                                }
                                    .into_view();
                            }
                            items
                                .into_iter()
                                .map(|item| {
                                    let id = item.id.clone();
                                    // Each reactive closure needs its own clone to avoid
                                    // the borrow-after-move compile error.
                                    let id_click   = id.clone();
                                    let id_class   = id.clone();
                                    let id_detail  = id.clone();
                                    let id_display = id.clone();
                                    let prio = item
                                        .priority
                                        .as_deref()
                                        .unwrap_or("medium")
                                        .to_string();
                                    let pclass = priority_class(&prio);
                                    let status = item
                                        .status
                                        .as_deref()
                                        .unwrap_or("pending")
                                        .to_string();
                                    let slabel = status_label(&status).to_string();
                                    let age = item
                                        .created_at
                                        .as_deref()
                                        .map(age_display)
                                        .unwrap_or_default();
                                    let assignee = item
                                        .assignee
                                        .clone()
                                        .unwrap_or_default();
                                    let is_jkh = assignee.to_lowercase() == "jkh";
                                    let title = item.title.clone();
                                    let body = item.body.clone().unwrap_or_default();
                                    view! {
                                        <tr
                                            class=move || {
                                                let base = if expanded_id.get().as_deref()
                                                    == Some(id_class.as_str())
                                                {
                                                    "queue-row expanded"
                                                } else {
                                                    "queue-row"
                                                };
                                                if is_jkh { format!("{} row-decision", base) } else { base.to_string() }
                                            }
                                            on:click=move |_| {
                                                let cur = expanded_id.get();
                                                if cur.as_deref() == Some(id_click.as_str()) {
                                                    set_expanded_id.set(None);
                                                } else {
                                                    set_expanded_id.set(Some(id_click.clone()));
                                                }
                                            }
                                        >
                                            <td class="col-id">
                                                <span class="item-id">{id_display}</span>
                                            </td>
                                            <td class="col-title">{title}</td>
                                            <td class="col-prio">
                                                <span class=format!("prio-badge {pclass}")>
                                                    {prio}
                                                </span>
                                            </td>
                                            <td class="col-assignee">{assignee}</td>
                                            <td class="col-status">
                                                <span class=format!("status-badge status-{slabel}")>
                                                    {slabel}
                                                </span>
                                            </td>
                                            <td class="col-age">{age}</td>
                                        </tr>
                                        {move || {
                                            if expanded_id.get().as_deref()
                                                == Some(id_detail.as_str())
                                            {
                                                view! {
                                                    <tr class="detail-row">
                                                        <td colspan="6">
                                                            <div class="item-detail">
                                                                <pre class="item-body">
                                                                    {body.clone()}
                                                                </pre>
                                                            </div>
                                                        </td>
                                                    </tr>
                                                }
                                                .into_view()
                                            } else {
                                                view! { <></> }.into_view()
                                            }
                                        }}
                                    }
                                })
                                .collect::<Vec<_>>()
                                .into_view()
                        }}
                    </tbody>
                </table>
            </div>

            {move || {
                if show_completed.get() {
                    let items = completed_items();
                    view! {
                        <div class="completed-section">
                            <h3 class="subsection-title">"Recently Completed"</h3>
                            <table class="queue-table completed-table">
                                <thead>
                                    <tr>
                                        <th>"ID"</th>
                                        <th>"Title"</th>
                                        <th>"Assignee"</th>
                                        <th>"Resolution"</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {items
                                        .into_iter()
                                        .map(|item| {
                                            view! {
                                                <tr class="queue-row done">
                                                    <td class="col-id">
                                                        {item.id.clone()}
                                                    </td>
                                                    <td>{item.title.clone()}</td>
                                                    <td>
                                                        {item.assignee.clone().unwrap_or_default()}
                                                    </td>
                                                    <td class="resolution">
                                                        {item
                                                            .resolution
                                                            .as_deref()
                                                            .unwrap_or("")
                                                            .chars()
                                                            .take(80)
                                                            .collect::<String>()}
                                                    </td>
                                                </tr>
                                            }
                                        })
                                        .collect::<Vec<_>>()}
                                </tbody>
                            </table>
                        </div>
                    }
                        .into_view()
                } else {
                    view! { <></> }.into_view()
                }
            }}
        </section>
    }
}
