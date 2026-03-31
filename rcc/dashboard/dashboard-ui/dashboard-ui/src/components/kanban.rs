use leptos::*;

use crate::types::{QueueItem, QueueResponse};

async fn fetch_queue_kanban() -> QueueResponse {
    let Ok(resp) = gloo_net::http::Request::get("/api/queue").send().await else {
        return QueueResponse::default();
    };
    resp.json::<QueueResponse>().await.unwrap_or_default()
}

async fn patch_item_status(id: String, status: String) {
    let payload = serde_json::json!({"status": status});
    let Ok(req) = gloo_net::http::Request::patch(&format!("/api/item/{id}"))
        .json(&payload)
    else {
        return;
    };
    let _ = req.send().await;
}

async fn patch_item_assignee(id: String, assignee: String) {
    let payload = serde_json::json!({"assignee": assignee});
    let Ok(req) = gloo_net::http::Request::patch(&format!("/api/item/{id}"))
        .json(&payload)
    else {
        return;
    };
    let _ = req.send().await;
}

fn assignee_emoji(a: &str) -> &'static str {
    match a {
        "natasha"    => "🕵️",
        "rocky"      => "🐿️",
        "bullwinkle" => "🫎",
        "boris"      => "🎨",
        "all"        => "👥",
        _            => "❓",
    }
}

fn item_col_key(item: &QueueItem) -> &'static str {
    match item.status.as_deref().unwrap_or("pending") {
        "in-progress" | "in_progress" => "in_progress",
        "awaiting-jkh" | "awaiting_jkh" => "awaiting_jkh",
        "done" | "completed" | "cancelled" => "done",
        _ => "pending",
    }
}

fn col_to_api_status(col: &str) -> &'static str {
    match col {
        "in_progress"  => "in-progress",
        "awaiting_jkh" => "awaiting-jkh",
        "done"         => "completed",
        _              => "pending",
    }
}

fn prio_extra_class(p: &str) -> &'static str {
    match p {
        "high" | "critical" => " kanban-card--high",
        "medium"            => " kanban-card--medium",
        _                   => "",
    }
}

#[component]
pub fn Kanban() -> impl IntoView {
    let (tick, set_tick) = create_signal(0u32);
    let (dragging_id, set_dragging_id) = create_signal(Option::<String>::None);
    let (drag_over_col, set_drag_over_col) = create_signal(Option::<String>::None);
    let (expanded_id, set_expanded_id) = create_signal(Option::<String>::None);

    // Poll every 20 seconds
    spawn_local(async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(20_000).await;
            set_tick.update(|t| *t = t.wrapping_add(1));
        }
    });

    let queue = create_resource(move || tick.get(), |_| fetch_queue_kanban());

    // Merge active and completed (cap completed at 15 for Done column)
    let all_items = move || {
        let q = queue.get().unwrap_or_default();
        let mut v = q.items;
        if let Some(c) = q.completed {
            v.extend(c.into_iter().take(15));
        }
        v
    };

    // render_col is a Fn closure — all captures (Leptos signals, Resource) are Copy
    let render_col = |col_id: &'static str, col_label: &'static str| {
        view! {
            <div
                class=move || {
                    if drag_over_col.get().as_deref() == Some(col_id) {
                        "kanban-col kanban-col--over"
                    } else {
                        "kanban-col"
                    }
                }
                on:dragenter=move |e| {
                    e.prevent_default();
                    set_drag_over_col.set(Some(col_id.to_string()));
                }
                on:dragover=move |e| {
                    e.prevent_default();
                }
                on:drop=move |e| {
                    e.prevent_default();
                    set_drag_over_col.set(None);
                    if let Some(id) = dragging_id.get_untracked() {
                        let st = col_to_api_status(col_id).to_string();
                        spawn_local(async move { patch_item_status(id, st).await; });
                        set_tick.update(|t| *t = t.wrapping_add(1));
                    }
                    set_dragging_id.set(None);
                }
            >
                <div class="kanban-col-header">
                    <span class="kanban-col-title">{col_label}</span>
                    <span class="badge">
                        {move || {
                            all_items()
                                .into_iter()
                                .filter(|i| item_col_key(i) == col_id)
                                .count()
                        }}
                    </span>
                </div>
                <div class="kanban-cards">
                    {move || {
                        all_items()
                            .into_iter()
                            .filter(|i| item_col_key(i) == col_id)
                            .map(|item| {
                                let id        = item.id.clone();
                                let id_drag   = id.clone();
                                let id_exp    = id.clone();
                                let id_chk    = id.clone();
                                let id_patch  = id.clone();
                                let prio      = item.priority.as_deref().unwrap_or("low").to_string();
                                let extra_cls = prio_extra_class(&prio).to_string();
                                let assignee  = item.assignee.as_deref().unwrap_or("").to_string();
                                let emoji     = assignee_emoji(&assignee).to_string();
                                let title     = item.title.clone();
                                let body      = item.body.clone().unwrap_or_default();
                                let asgn_init = assignee.clone();
                                let prio_disp = prio.clone();
                                let prio_cls  = format!("prio-badge prio-{prio}");

                                view! {
                                    <div
                                        class=format!("kanban-card{extra_cls}")
                                        draggable="true"
                                        on:dragstart=move |_| {
                                            set_dragging_id.set(Some(id_drag.clone()));
                                        }
                                        on:dragend=move |_| {
                                            set_dragging_id.set(None);
                                            set_drag_over_col.set(None);
                                        }
                                        on:click=move |_| {
                                            let cur = expanded_id.get_untracked();
                                            if cur.as_deref() == Some(id_exp.as_str()) {
                                                set_expanded_id.set(None);
                                            } else {
                                                set_expanded_id.set(Some(id_exp.clone()));
                                            }
                                        }
                                    >
                                        <div class="kanban-card-top">
                                            <span class="kanban-card-title">{title}</span>
                                            <span class=prio_cls>{prio_disp}</span>
                                        </div>
                                        <div class="kanban-card-foot">
                                            <span class="kanban-avatar" title=assignee.clone()>
                                                {emoji}
                                            </span>
                                            <span class="kanban-assignee">{assignee}</span>
                                        </div>
                                        {move || {
                                            if expanded_id.get().as_deref() == Some(id_chk.as_str()) {
                                                let body_c = body.clone();
                                                let asgn_c = asgn_init.clone();
                                                let id_c   = id_patch.clone();
                                                view! {
                                                    <div
                                                        class="kanban-detail"
                                                        on:click=|e| e.stop_propagation()
                                                    >
                                                        {(!body_c.is_empty()).then(|| view! {
                                                            <pre class="kanban-body">{body_c.clone()}</pre>
                                                        })}
                                                        <div class="kanban-field">
                                                            <span class="kanban-field-label">"Assignee"</span>
                                                            <select
                                                                class="kanban-select"
                                                                on:change=move |e| {
                                                                    let new_a = event_target_value(&e);
                                                                    let idc = id_c.clone();
                                                                    spawn_local(async move {
                                                                        patch_item_assignee(idc, new_a).await;
                                                                    });
                                                                    set_tick.update(|t| *t = t.wrapping_add(1));
                                                                }
                                                            >
                                                                <option value="natasha" selected=asgn_c == "natasha">"🕵️ natasha"</option>
                                                                <option value="rocky" selected=asgn_c == "rocky">"🐿️ rocky"</option>
                                                                <option value="bullwinkle" selected=asgn_c == "bullwinkle">"🫎 bullwinkle"</option>
                                                                <option value="boris" selected=asgn_c == "boris">"🎨 boris"</option>
                                                                <option value="all" selected=asgn_c == "all">"👥 all"</option>
                                                            </select>
                                                        </div>
                                                    </div>
                                                }
                                                .into_view()
                                            } else {
                                                view! { <></> }.into_view()
                                            }
                                        }}
                                    </div>
                                }
                            })
                            .collect::<Vec<_>>()
                            .into_view()
                    }}
                </div>
            </div>
        }
    };

    view! {
        <section class="section section-kanban">
            <div class="section-header">
                <h2 class="section-title">
                    <span class="section-icon">"📋"</span>
                    "Kanban Board"
                </h2>
            </div>
            <div class="kanban-board">
                {render_col("pending", "Pending")}
                {render_col("in_progress", "In Progress")}
                {render_col("awaiting_jkh", "Awaiting jkh")}
                {render_col("done", "Done")}
            </div>
        </section>
    }
}
