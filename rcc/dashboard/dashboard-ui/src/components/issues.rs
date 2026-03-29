use leptos::*;
use serde::{Deserialize, Serialize};

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GhLabel {
    pub name: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GhIssueItem {
    pub number: Option<i64>,
    pub title: Option<String>,
    pub state: Option<String>,
    pub url: Option<String>,
    pub html_url: Option<String>,
    pub labels: Option<Vec<GhLabel>>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub user: Option<GhUser>,
    pub body: Option<String>,
    // flat label names (fallback)
    pub label_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GhUser {
    pub login: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GhPrItem {
    pub number: Option<i64>,
    pub title: Option<String>,
    pub state: Option<String>,
    pub url: Option<String>,
    pub html_url: Option<String>,
    pub draft: Option<bool>,
    pub labels: Option<Vec<GhLabel>>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub user: Option<GhUser>,
    pub merged_at: Option<String>,
    pub review_decision: Option<String>,
    pub mergeable: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GhProjectData {
    pub repo: Option<String>,
    #[serde(rename = "fetchedAt")]
    pub fetched_at: Option<String>,
    pub issues: Vec<GhIssueItem>,
    pub prs: Vec<GhPrItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectEntry {
    pub id: String,
    pub display_name: Option<String>,
    pub enabled: Option<bool>,
}

// ── Fetch helpers ─────────────────────────────────────────────────────────────

async fn fetch_projects() -> Vec<ProjectEntry> {
    let Ok(resp) = gloo_net::http::Request::get("/api/projects").send().await else {
        return vec![];
    };
    resp.json::<Vec<ProjectEntry>>().await.unwrap_or_default()
}

async fn fetch_github_data(repo: String) -> GhProjectData {
    if repo.is_empty() {
        return GhProjectData::default();
    }
    // URL-encode the repo slug (replace / with %2F)
    let encoded = repo.replace('/', "%2F");
    let url = format!("/api/projects/{}/github", encoded);
    let Ok(resp) = gloo_net::http::Request::get(&url).send().await else {
        return GhProjectData::default();
    };
    resp.json::<GhProjectData>().await.unwrap_or_default()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn relative_time(ts: &str) -> String {
    // Very basic relative time — just show the date part
    if let Some(d) = ts.split('T').next() {
        d.to_string()
    } else {
        ts.to_string()
    }
}

fn issue_state_icon(state: &str) -> &'static str {
    match state {
        "open" => "🟢",
        "closed" => "⚫",
        _ => "⬜",
    }
}

fn pr_state_icon(state: &str, draft: bool, merged: bool) -> &'static str {
    if merged { return "🟣"; }
    if draft { return "⬜"; }
    match state {
        "open" => "🟢",
        "closed" => "🔴",
        _ => "⬜",
    }
}

fn label_color_style(color: &str) -> String {
    // color is a hex string without #
    let r = u8::from_str_radix(&color[..2.min(color.len())], 16).unwrap_or(100);
    let g = u8::from_str_radix(&color[2..4.min(color.len())], 16).unwrap_or(100);
    let b = u8::from_str_radix(&color[4..6.min(color.len())], 16).unwrap_or(100);
    // Compute perceived brightness to pick text color
    let brightness = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000;
    let text_color = if brightness > 128 { "#0d1117" } else { "#c9d1d9" };
    format!("background:#{};color:{};padding:1px 6px;border-radius:10px;font-size:10px;font-weight:600;white-space:nowrap;", color, text_color)
}

fn get_labels(labels: &Option<Vec<GhLabel>>) -> Vec<(String, String)> {
    labels.as_ref().map(|ls| {
        ls.iter().map(|l| {
            let name = l.name.clone().unwrap_or_default();
            let color = l.color.clone().unwrap_or_else(|| "6e7681".to_string());
            (name, color)
        }).collect()
    }).unwrap_or_default()
}

// ── Component ─────────────────────────────────────────────────────────────────

#[component]
pub fn Issues() -> impl IntoView {
    let (repo, set_repo) = create_signal(String::new());
    let (tick, set_tick) = create_signal(0u32);
    let (issue_filter, set_issue_filter) = create_signal(String::new());
    let (pr_filter, set_pr_filter) = create_signal(String::new());
    let (show_closed_issues, set_show_closed_issues) = create_signal(false);
    let (show_closed_prs, set_show_closed_prs) = create_signal(false);

    let projects = create_resource(|| (), |_| fetch_projects());

    let gh_data = create_resource(
        move || (repo.get(), tick.get()),
        |(r, _)| fetch_github_data(r),
    );

    // When projects load, auto-select first enabled one
    let projects_loaded = move || {
        if let Some(ps) = projects.get() {
            if repo.get().is_empty() {
                if let Some(first) = ps.iter().find(|p| p.enabled.unwrap_or(true)) {
                    set_repo.set(first.id.clone());
                }
            }
        }
    };

    view! {
        <div class="issues-panel">
            // Header
            <div class="issues-header">
                <h2 style="display:flex;align-items:center;gap:8px;">
                    "🐛 " <span>"Issues & PRs"</span>
                </h2>
                <div class="issues-controls">
                    // Project selector
                    <select
                        style="background:var(--surface2);color:var(--text);border:1px solid var(--border);border-radius:var(--radius);padding:4px 8px;font-family:var(--font);font-size:12px;"
                        on:change=move |ev| {
                            set_repo.set(event_target_value(&ev));
                            set_tick.update(|t| *t = t.wrapping_add(1));
                        }
                    >
                        <option value="">"— select project —"</option>
                        {move || {
                            projects_loaded();
                            projects.get().unwrap_or_default().iter()
                                .filter(|p| p.enabled.unwrap_or(true))
                                .map(|p| {
                                    let id = p.id.clone();
                                    let name = p.display_name.clone().unwrap_or_else(|| p.id.clone());
                                    let selected = repo.get() == id;
                                    view! {
                                        <option value={id.clone()} selected=selected>{name}</option>
                                    }
                                })
                                .collect::<Vec<_>>()
                        }}
                    </select>

                    // Refresh
                    <button
                        style="background:var(--surface2);color:var(--text-dim);border:1px solid var(--border);border-radius:var(--radius);padding:4px 10px;cursor:pointer;font-size:12px;"
                        on:click=move |_| set_tick.update(|t| *t = t.wrapping_add(1))
                    >"🔄 Refresh"</button>

                    // Fetch info
                    {move || {
                        let data = gh_data.get().unwrap_or_default();
                        if let Some(ts) = data.fetched_at {
                            view! {
                                <span style="font-size:11px;color:var(--text-dimmer);">
                                    {format!("cached {}", &ts[..ts.len().min(16)])}
                                </span>
                            }.into_view()
                        } else {
                            view! { <span></span> }.into_view()
                        }
                    }}
                </div>
            </div>

            // Empty state when no project selected
            {move || {
                if repo.get().is_empty() {
                    view! {
                        <div style="text-align:center;padding:48px;color:var(--text-dim);">
                            "← Select a project to view issues and PRs"
                        </div>
                    }.into_view()
                } else {
                    view! { <span></span> }.into_view()
                }
            }}

            // Two-column layout: Issues + PRs
            {move || {
                if repo.get().is_empty() {
                    return view! { <span></span> }.into_view();
                }
                let data = gh_data.get().unwrap_or_default();

                // Filter issues
                let ifilter = issue_filter.get().to_lowercase();
                let hide_closed_i = !show_closed_issues.get();
                let visible_issues: Vec<GhIssueItem> = data.issues.iter().filter(|iss| {
                    let state = iss.state.as_deref().unwrap_or("open");
                    if hide_closed_i && state == "closed" { return false; }
                    if ifilter.is_empty() { return true; }
                    iss.title.as_deref().unwrap_or("").to_lowercase().contains(&ifilter)
                }).cloned().collect();

                // Filter PRs
                let pfilter = pr_filter.get().to_lowercase();
                let hide_closed_p = !show_closed_prs.get();
                let visible_prs: Vec<GhPrItem> = data.prs.iter().filter(|pr| {
                    let state = pr.state.as_deref().unwrap_or("open");
                    if hide_closed_p && state == "closed" { return false; }
                    if pfilter.is_empty() { return true; }
                    pr.title.as_deref().unwrap_or("").to_lowercase().contains(&pfilter)
                }).cloned().collect();

                let total_issues = data.issues.len();
                let open_issues = data.issues.iter().filter(|i| i.state.as_deref() == Some("open")).count();
                let total_prs = data.prs.len();
                let open_prs = data.prs.iter().filter(|p| p.state.as_deref() == Some("open")).count();

                view! {
                    <div style="display:grid;grid-template-columns:1fr 1fr;gap:16px;align-items:start;">
                        // ── Issues column ──────────────────────────────────
                        <div style="display:flex;flex-direction:column;gap:8px;">
                            <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;">
                                <h3 style="font-size:14px;color:var(--text);margin:0;">
                                    {format!("🐛 Issues ({} open / {} total)", open_issues, total_issues)}
                                </h3>
                                <input
                                    type="text"
                                    placeholder="filter..."
                                    style="background:var(--surface2);color:var(--text);border:1px solid var(--border);border-radius:var(--radius-sm);padding:2px 6px;font-size:11px;font-family:var(--font);flex:1;min-width:80px;"
                                    on:input=move |e| set_issue_filter.set(event_target_value(&e))
                                />
                                <label style="font-size:11px;color:var(--text-dim);display:flex;align-items:center;gap:4px;cursor:pointer;">
                                    <input
                                        type="checkbox"
                                        on:change=move |e| set_show_closed_issues.set(event_target_checked(&e))
                                    />
                                    "closed"
                                </label>
                            </div>

                            {if visible_issues.is_empty() {
                                view! {
                                    <div style="color:var(--text-dimmer);font-size:12px;padding:16px 0;text-align:center;">
                                        {if total_issues == 0 {
                                            "No issues — either none exist or GitHub scout hasn't run yet."
                                        } else {
                                            "No issues match the current filter."
                                        }}
                                    </div>
                                }.into_view()
                            } else {
                                visible_issues.iter().map(|iss| {
                                    let iss = iss.clone();
                                    let state = iss.state.as_deref().unwrap_or("open").to_string();
                                    let icon = issue_state_icon(&state);
                                    let num = iss.number.unwrap_or(0);
                                    let title = iss.title.clone().unwrap_or_default();
                                    let url = iss.html_url.clone()
                                        .or_else(|| iss.url.clone())
                                        .unwrap_or_default();
                                    let date = iss.updated_at.as_deref()
                                        .or(iss.created_at.as_deref())
                                        .map(relative_time).unwrap_or_default();
                                    let author = iss.user.as_ref()
                                        .and_then(|u| u.login.clone())
                                        .unwrap_or_default();
                                    let labels = get_labels(&iss.labels);
                                    let is_closed = state == "closed";

                                    view! {
                                        <div style=format!(
                                            "background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);padding:8px 10px;display:flex;flex-direction:column;gap:4px;{}",
                                            if is_closed { "opacity:0.6;" } else { "" }
                                        )>
                                            <div style="display:flex;align-items:flex-start;gap:6px;">
                                                <span style="flex-shrink:0;font-size:13px;">{icon}</span>
                                                <a
                                                    href={url}
                                                    target="_blank"
                                                    rel="noopener noreferrer"
                                                    style="color:var(--text);text-decoration:none;font-size:12px;font-weight:500;line-height:1.4;flex:1;"
                                                    // hover handled inline isn't great but works
                                                >
                                                    {format!("#{} {}", num, title)}
                                                </a>
                                            </div>
                                            <div style="display:flex;align-items:center;gap:6px;flex-wrap:wrap;margin-left:19px;">
                                                {labels.iter().map(|(name, color)| {
                                                    let style = label_color_style(color);
                                                    view! {
                                                        <span style={style}>{name.clone()}</span>
                                                    }
                                                }).collect::<Vec<_>>()}
                                                {if !author.is_empty() {
                                                    view! {
                                                        <span style="font-size:10px;color:var(--text-dimmer);">
                                                            {format!("@{}", author)}
                                                        </span>
                                                    }.into_view()
                                                } else { view! { <span></span> }.into_view() }}
                                                <span style="font-size:10px;color:var(--text-dimmer);margin-left:auto;">{date}</span>
                                            </div>
                                        </div>
                                    }
                                }).collect::<Vec<_>>().into_view()
                            }}
                        </div>

                        // ── PRs column ─────────────────────────────────────
                        <div style="display:flex;flex-direction:column;gap:8px;">
                            <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;">
                                <h3 style="font-size:14px;color:var(--text);margin:0;">
                                    {format!("🔀 Pull Requests ({} open / {} total)", open_prs, total_prs)}
                                </h3>
                                <input
                                    type="text"
                                    placeholder="filter..."
                                    style="background:var(--surface2);color:var(--text);border:1px solid var(--border);border-radius:var(--radius-sm);padding:2px 6px;font-size:11px;font-family:var(--font);flex:1;min-width:80px;"
                                    on:input=move |e| set_pr_filter.set(event_target_value(&e))
                                />
                                <label style="font-size:11px;color:var(--text-dim);display:flex;align-items:center;gap:4px;cursor:pointer;">
                                    <input
                                        type="checkbox"
                                        on:change=move |e| set_show_closed_prs.set(event_target_checked(&e))
                                    />
                                    "closed"
                                </label>
                            </div>

                            {if visible_prs.is_empty() {
                                view! {
                                    <div style="color:var(--text-dimmer);font-size:12px;padding:16px 0;text-align:center;">
                                        {if total_prs == 0 {
                                            "No PRs — either none exist or GitHub scout hasn't run yet."
                                        } else {
                                            "No PRs match the current filter."
                                        }}
                                    </div>
                                }.into_view()
                            } else {
                                visible_prs.iter().map(|pr| {
                                    let pr = pr.clone();
                                    let state = pr.state.as_deref().unwrap_or("open").to_string();
                                    let draft = pr.draft.unwrap_or(false);
                                    let merged = pr.merged_at.is_some();
                                    let icon = pr_state_icon(&state, draft, merged);
                                    let num = pr.number.unwrap_or(0);
                                    let title = pr.title.clone().unwrap_or_default();
                                    let url = pr.html_url.clone()
                                        .or_else(|| pr.url.clone())
                                        .unwrap_or_default();
                                    let date = pr.updated_at.as_deref()
                                        .or(pr.created_at.as_deref())
                                        .map(relative_time).unwrap_or_default();
                                    let author = pr.user.as_ref()
                                        .and_then(|u| u.login.clone())
                                        .unwrap_or_default();
                                    let labels = get_labels(&pr.labels);
                                    let review = pr.review_decision.clone();
                                    let mergeable = pr.mergeable.clone();
                                    let is_closed = state == "closed" && !merged;

                                    view! {
                                        <div style=format!(
                                            "background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);padding:8px 10px;display:flex;flex-direction:column;gap:4px;{}",
                                            if is_closed { "opacity:0.6;" } else { "" }
                                        )>
                                            <div style="display:flex;align-items:flex-start;gap:6px;">
                                                <span style="flex-shrink:0;font-size:13px;">{icon}</span>
                                                <a
                                                    href={url}
                                                    target="_blank"
                                                    rel="noopener noreferrer"
                                                    style="color:var(--text);text-decoration:none;font-size:12px;font-weight:500;line-height:1.4;flex:1;"
                                                >
                                                    {format!("#{} {}", num, title)}
                                                </a>
                                                {if draft {
                                                    view! {
                                                        <span style="font-size:10px;background:#6e7681;color:#c9d1d9;padding:1px 6px;border-radius:10px;white-space:nowrap;flex-shrink:0;">"Draft"</span>
                                                    }.into_view()
                                                } else { view! { <span></span> }.into_view() }}
                                                {if merged {
                                                    view! {
                                                        <span style="font-size:10px;background:#6e40c9;color:#fff;padding:1px 6px;border-radius:10px;white-space:nowrap;flex-shrink:0;">"Merged"</span>
                                                    }.into_view()
                                                } else { view! { <span></span> }.into_view() }}
                                            </div>
                                            <div style="display:flex;align-items:center;gap:6px;flex-wrap:wrap;margin-left:19px;">
                                                {labels.iter().map(|(name, color)| {
                                                    let style = label_color_style(color);
                                                    view! {
                                                        <span style={style}>{name.clone()}</span>
                                                    }
                                                }).collect::<Vec<_>>()}
                                                {match review.as_deref() {
                                                    Some("APPROVED") => view! {
                                                        <span style="font-size:10px;color:#3fb950;">"✅ Approved"</span>
                                                    }.into_view(),
                                                    Some("CHANGES_REQUESTED") => view! {
                                                        <span style="font-size:10px;color:#f85149;">"❌ Changes"</span>
                                                    }.into_view(),
                                                    Some("REVIEW_REQUIRED") => view! {
                                                        <span style="font-size:10px;color:#e3b341;">"👀 Review needed"</span>
                                                    }.into_view(),
                                                    Some(other) => {
                                                        let other = other.to_string();
                                                        view! {
                                                            <span style="font-size:10px;color:#8b949e;">{other}</span>
                                                        }.into_view()
                                                    },
                                                    None => view! { <span></span> }.into_view(),
                                                }}
                                                {if let Some(mg) = &mergeable {
                                                    if mg == "MERGEABLE" {
                                                        view! {
                                                            <span style="font-size:10px;color:var(--green);">"✓ Mergeable"</span>
                                                        }.into_view()
                                                    } else if mg == "CONFLICTING" {
                                                        view! {
                                                            <span style="font-size:10px;color:var(--red);">"⚠ Conflict"</span>
                                                        }.into_view()
                                                    } else {
                                                        view! { <span></span> }.into_view()
                                                    }
                                                } else { view! { <span></span> }.into_view() }}
                                                {if !author.is_empty() {
                                                    view! {
                                                        <span style="font-size:10px;color:var(--text-dimmer);">
                                                            {format!("@{}", author)}
                                                        </span>
                                                    }.into_view()
                                                } else { view! { <span></span> }.into_view() }}
                                                <span style="font-size:10px;color:var(--text-dimmer);margin-left:auto;">{date}</span>
                                            </div>
                                        </div>
                                    }
                                }).collect::<Vec<_>>().into_view()
                            }}
                        </div>
                    </div>
                }.into_view()
            }}
        </div>
    }
}
