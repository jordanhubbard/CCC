use leptos::*;

use crate::types::GitCommit;

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

#[component]
pub fn Changelog() -> impl IntoView {
    let (open, set_open) = create_signal(false);
    let (tick, set_tick) = create_signal(0u32);

    leptos::spawn_local(async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(300_000).await; // refresh every 5min
            set_tick.update(|t| *t = t.wrapping_add(1));
        }
    });

    let commits = create_resource(move || tick.get(), |_| async move {
        let Ok(resp) = gloo_net::http::Request::get(
            "/api/projects/jordanhubbard/rockyandfriends/github"
        ).send().await else {
            return Vec::<GitCommit>::new();
        };
        resp.json::<Vec<GitCommit>>().await.unwrap_or_default()
    });

    view! {
        <section class="section section-changelog">
            <div
                class="section-header changelog-toggle"
                on:click=move |_| set_open.update(|v| *v = !*v)
            >
                <h2 class="section-title">
                    <span class="section-icon">"⎇"</span>
                    "Changelog"
                </h2>
                <span class="collapse-indicator">
                    {move || if open.get() { "▲" } else { "▼" }}
                </span>
            </div>
            {move || {
                if !open.get() {
                    return view! { <div></div> }.into_view();
                }
                let items = commits.get().unwrap_or_default();
                let rows: Vec<GitCommit> = items.into_iter().take(10).collect();
                if rows.is_empty() {
                    return view! {
                        <div class="changelog-body">
                            <div class="changelog-empty">"No commits found."</div>
                        </div>
                    }.into_view();
                }
                view! {
                    <div class="changelog-body">
                        {rows.into_iter().map(|c| {
                            let sha = c.sha.as_deref().unwrap_or("???????");
                            let short_sha = if sha.len() >= 7 { &sha[..7] } else { sha };
                            let detail = c.commit.as_ref();
                            let msg = detail
                                .and_then(|d| d.message.as_deref())
                                .unwrap_or("")
                                .lines()
                                .next()
                                .unwrap_or("")
                                .to_string();
                            let author = detail
                                .and_then(|d| d.author.as_ref())
                                .and_then(|a| a.name.as_deref())
                                .unwrap_or("unknown")
                                .to_string();
                            let ts = detail
                                .and_then(|d| d.author.as_ref())
                                .and_then(|a| a.date.as_deref())
                                .map(relative_time)
                                .unwrap_or_default();
                            let short_sha = short_sha.to_string();
                            view! {
                                <div class="changelog-entry">
                                    <span class="commit-hash">{short_sha}</span>
                                    <span class="commit-msg">{msg}</span>
                                    <span class="commit-meta">
                                        {author}
                                        {if !ts.is_empty() { format!(" · {ts}") } else { String::new() }}
                                    </span>
                                </div>
                            }
                        }).collect::<Vec<_>>().into_view()}
                    </div>
                }.into_view()
            }}
        </section>
    }
}
