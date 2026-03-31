use leptos::*;

use crate::context::DashboardContext;

async fn upvote(id: String) -> bool {
    let Ok(resp) = gloo_net::http::Request::post(&format!("/api/upvote/{id}"))
        .header("Content-Type", "application/json")
        .body("{}")
        .unwrap()
        .send()
        .await
    else {
        return false;
    };
    resp.ok()
}

#[component]
pub fn IdeaIncubator() -> impl IntoView {
    let ctx = use_context::<DashboardContext>().expect("DashboardContext missing");
    let queue = ctx.queue;
    // Derive ideas from the shared queue resource
    let ideas = move || {
        queue.get().map(|q| {
            q.items.into_iter()
                .filter(|i| {
                    i.priority.as_deref() == Some("idea")
                        || i.status.as_deref() == Some("incubating")
                })
                .collect::<Vec<_>>()
        }).unwrap_or_default()
    };

    view! {
        <section class="section section-ideas">
            <div class="section-header">
                <h2 class="section-title">
                    <span class="section-icon">"✦"</span>
                    "Idea Incubator"
                </h2>
                {move || {
                    let n = ideas().len();
                    view! { <span class="badge">{n}</span> }
                }}
            </div>

            <div class="ideas-list">
                {move || {
                    let items = ideas();
                    if items.is_empty() {
                        return view! { <p class="empty">"No ideas incubating"</p> }.into_view();
                    }
                    items
                            .into_iter()
                            .map(|idea| {
                                let id = idea.id.clone();
                                let id2 = id.clone();
                                let title = idea.title.clone();
                                let body = idea
                                    .body
                                    .clone()
                                    .unwrap_or_default()
                                    .chars()
                                    .take(160)
                                    .collect::<String>();
                                let tags = idea.tags.clone().unwrap_or_default();
                                let (upvoted, set_upvoted) = create_signal(false);
                                view! {
                                    <div class=move || {
                                        if upvoted.get() { "idea-card upvoted" } else { "idea-card" }
                                    }>
                                        <div class="idea-header">
                                            <span class="idea-id">{id2}</span>
                                            <span class="idea-title">{title}</span>
                                        </div>
                                        {if !body.is_empty() {
                                            view! { <p class="idea-body">{body}</p> }.into_view()
                                        } else {
                                            view! { <></> }.into_view()
                                        }}
                                        {if !tags.is_empty() {
                                            let tag_str = tags.join(", ");
                                            view! {
                                                <div class="idea-tags">{tag_str}</div>
                                            }
                                                .into_view()
                                        } else {
                                            view! { <></> }.into_view()
                                        }}
                                        <div class="idea-actions">
                                            <button
                                                class=move || {
                                                    if upvoted.get() {
                                                        "btn btn-upvoted"
                                                    } else {
                                                        "btn btn-upvote"
                                                    }
                                                }
                                                disabled=move || upvoted.get()
                                                on:click=move |_| {
                                                    let id = id.clone();
                                                    leptos::spawn_local(async move {
                                                        if upvote(id).await {
                                                            set_upvoted.set(true);
                                                        }
                                                    });
                                                }
                                            >
                                                {move || if upvoted.get() { "▲ upvoted" } else { "▲ upvote" }}
                                            </button>
                                        </div>
                                    </div>
                                }
                            })
                            .collect::<Vec<_>>()
                            .into_view()
                }}
            </div>
        </section>
    }
}
