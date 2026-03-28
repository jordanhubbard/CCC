use leptos::prelude::*;
use leptos::task::spawn_local;
use crate::types::{QueueItem, HeartbeatMap, agent_color};
use crate::api;

const COLUMNS: &[(&str, &str)] = &[
    ("rocky",      "🐿️ Rocky"),
    ("bullwinkle", "🫎 Bullwinkle"),
    ("natasha",    "🕵️ Natasha"),
    ("boris",      "⚡ Boris"),
    ("unassigned", "📥 Unassigned"),
    ("appeal",     "🎯 Appeal"),
];

// ── Filter state ──────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Eq)]
pub struct KanbanFilters {
    pub type_filter:     String,  // "all" | "bug" | "idea" | "feature" | "proposal"
    pub priority_filter: String,  // "all" | "urgent" | "high" | "medium" | "low"
    pub show_done:       bool,
}

impl Default for KanbanFilters {
    fn default() -> Self {
        KanbanFilters {
            type_filter:     "all".into(),
            priority_filter: "all".into(),
            show_done:       false,
        }
    }
}

// ── Board ─────────────────────────────────────────────────────────────────────

#[component]
pub fn KanbanBoard(
    queue:      RwSignal<Vec<QueueItem>>,
    heartbeats: RwSignal<HeartbeatMap>,
) -> impl IntoView {
    let filters = RwSignal::new(KanbanFilters::default());

    view! {
        <div>
            <KanbanFiltersBar filters=filters />
            <div class="kanban-board">
                {COLUMNS.iter().map(|&(key, label)| {
                    view! {
                        <KanbanColumn
                            col_key=key
                            label=label
                            queue=queue
                            filters=filters
                            heartbeats=heartbeats
                        />
                    }
                }).collect_view()}
            </div>
        </div>
    }
}

// ── Filter bar ────────────────────────────────────────────────────────────────

#[component]
fn KanbanFiltersBar(filters: RwSignal<KanbanFilters>) -> impl IntoView {
    let type_options  = ["all", "bug", "idea", "feature", "proposal", "task"];
    let prio_options  = ["all", "urgent", "high", "medium", "low"];

    view! {
        <div class="kanban-filters">
            <span style="font-size:12px;color:var(--text-muted)">"Type:"</span>
            {type_options.iter().map(|&opt| {
                view! {
                    <button
                        class=move || {
                            if filters.get().type_filter == opt { "active" } else { "" }
                        }
                        on:click=move |_| {
                            filters.update(|f| f.type_filter = opt.to_string());
                        }
                    >
                        {opt}
                    </button>
                }
            }).collect_view()}
            <span style="font-size:12px;color:var(--text-muted);margin-left:8px">"Priority:"</span>
            {prio_options.iter().map(|&opt| {
                view! {
                    <button
                        class=move || {
                            if filters.get().priority_filter == opt { "active" } else { "" }
                        }
                        on:click=move |_| {
                            filters.update(|f| f.priority_filter = opt.to_string());
                        }
                    >
                        {opt}
                    </button>
                }
            }).collect_view()}
            <button
                style="margin-left:auto"
                class=move || if filters.get().show_done { "active" } else { "" }
                on:click=move |_| {
                    filters.update(|f| f.show_done = !f.show_done);
                }
            >
                "Show Completed"
            </button>
        </div>
    }
}

// ── Column ────────────────────────────────────────────────────────────────────

#[component]
fn KanbanColumn(
    col_key:    &'static str,
    label:      &'static str,
    queue:      RwSignal<Vec<QueueItem>>,
    filters:    RwSignal<KanbanFilters>,
    heartbeats: RwSignal<HeartbeatMap>,
) -> impl IntoView {
    let col_color = agent_color(col_key);

    let items = move || {
        let f = filters.get();
        queue.get()
            .into_iter()
            .filter(|item| {
                // Column assignment
                let matches_col = if col_key == "appeal" {
                    item.needs_human || item.status == "awaiting-jkh"
                } else {
                    item.assignee_or_unassigned().to_lowercase() == col_key
                };
                if !matches_col { return false; }

                // Type filter
                if f.type_filter != "all" && item.card_type() != f.type_filter { return false; }

                // Priority filter
                if f.priority_filter != "all" {
                    if item.priority_str() != f.priority_filter { return false; }
                }

                // Show done
                if !f.show_done && (item.status == "done" || item.status == "completed") {
                    return false;
                }

                true
            })
            .collect::<Vec<_>>()
    };

    // Heartbeat status dot for agent columns
    let hb_dot = move || -> Option<String> {
        match col_key {
            "unassigned" | "appeal" => None,
            agent => {
                let hbs = heartbeats.get();
                let hb = hbs.get(agent)?;
                Some(hb.status_class().to_string())
            }
        }
    };

    view! {
        <div class="kanban-col">
            <div class="kanban-col-header" style=format!("border-top: 2px solid {}", col_color)>
                {move || {
                    if let Some(dot) = hb_dot() {
                        view! {
                            <span class=format!("agent-dot {} ", dot)
                                style="margin-right:4px" />
                        }.into_any()
                    } else {
                        view! { <span /> }.into_any()
                    }
                }}
                <span>{label}</span>
                <span class="count">{move || items().len()}</span>
            </div>
            <div class="kanban-cards">
                {move || items().into_iter().map(|item| {
                    view! { <KanbanCard item=item queue=queue /> }
                }).collect_view()}
            </div>
        </div>
    }
}

// ── Card ──────────────────────────────────────────────────────────────────────

#[component]
fn KanbanCard(item: QueueItem, queue: RwSignal<Vec<QueueItem>>) -> impl IntoView {
    let card_type = item.card_type().to_string();
    let card_class = format!("kanban-card type-{}", card_type);
    let title = item.title.clone();
    let status = item.status.clone();
    let priority = item.priority_str().to_string();
    let tags = item.tags.clone();
    let has_blocking = !item.blocked_by.is_empty() || !item.blocks.is_empty();
    let _id = item.id.clone();
    let id_appeal = item.id.clone();
    let queue_appeal = queue;

    let on_appeal = move |e: web_sys::MouseEvent| {
        e.stop_propagation();
        let id = id_appeal.clone();
        let q = queue_appeal;
        spawn_local(async move {
            let patch = serde_json::json!({
                "needsHuman": true,
                "status": "awaiting-jkh"
            });
            if api::patch_item(&id, patch).await.is_ok() {
                if let Ok(items) = api::fetch_queue().await {
                    q.set(items);
                }
            }
        });
    };

    view! {
        <div class=card_class>
            <div class="kanban-card-title">{title}</div>
            <div class="kanban-tags">
                {tags.iter().map(|tag| {
                    let t = tag.clone();
                    let cls = match t.as_str() {
                        "bug" | "idea" | "feature" | "proposal" => t.clone(),
                        _ => "".to_string(),
                    };
                    view! { <span class=format!("tag {}", cls)>{t}</span> }
                }).collect_view()}
            </div>
            <div class="kanban-meta">
                <span class=format!("priority-chip {}", priority)>
                    {priority_emoji(&priority)}{" "}{priority.clone()}
                </span>
                <span class=format!("status-chip {}", status)>{status.clone()}</span>
                {if has_blocking {
                    view! { <span class="block-badge" title="Has blocking relationships">"🔗"</span> }.into_any()
                } else {
                    view! { <span /> }.into_any()
                }}
                <button
                    style="margin-left:auto;font-size:10px;background:transparent;border:none;color:var(--text-dim);cursor:pointer;padding:0"
                    title="Appeal to jkh"
                    on:click=on_appeal
                >
                    "🙋"
                </button>
            </div>
        </div>
    }
}

fn priority_emoji(p: &str) -> &'static str {
    match p {
        "urgent" => "🔴",
        "high"   => "🟠",
        "medium" => "🟡",
        "low"    => "🟢",
        _        => "",
    }
}
