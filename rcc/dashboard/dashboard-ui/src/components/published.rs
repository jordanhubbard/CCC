//! Published — artifacts and services published by agents via RFC-001 /api/publish

use leptos::*;
use serde::{Deserialize, Serialize};

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Publication {
    pub id:           String,
    pub agent:        String,
    pub name:         String,
    #[serde(rename = "type")]
    pub type_:        String,
    pub url:          Option<String>,
    pub visibility:   String,
    pub tunnel_port:  Option<u16>,
    pub status:       String,
    pub keep_forever: Option<bool>,
    pub timeout_s:    Option<u64>,
    pub created_at:   Option<String>,
    pub expires_at:   Option<String>,
    pub last_seen_at: Option<String>,
    pub error_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PublicationsResponse {
    pub publications: Vec<Publication>,
}

// ── Fetch ─────────────────────────────────────────────────────────────────────

async fn fetch_publications() -> Vec<Publication> {
    let Ok(resp) = gloo_net::http::Request::get("/api/publish").send().await else {
        return vec![];
    };
    resp.json::<PublicationsResponse>()
        .await
        .unwrap_or_default()
        .publications
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

fn relative_time(ts: &str) -> String {
    let now_sec = (js_sys::Date::now() as u64) / 1000;
    let parse = || -> Option<u64> {
        let (date_part, time_part) = ts.split_once('T')?;
        let mut dp = date_part.split('-');
        let y: u64 = dp.next()?.parse().ok()?;
        let m: u64 = dp.next()?.parse().ok()?;
        let d: u64 = dp.next()?.parse().ok()?;
        let time_clean = time_part.trim_end_matches('Z');
        let mut tp = time_clean.split(':');
        let h: u64  = tp.next()?.parse().ok()?;
        let mi: u64 = tp.next()?.parse().ok()?;
        let s: f64  = tp.next().unwrap_or("0").parse().ok()?;
        Some(days_since_epoch(y, m, d) * 86400 + h * 3600 + mi * 60 + s as u64)
    };
    if let Some(ts_sec) = parse() {
        let diff = now_sec.saturating_sub(ts_sec);
        if diff < 60    { return "just now".to_string(); }
        if diff < 3600  { return format!("{}m ago", diff / 60); }
        if diff < 86400 { return format!("{}h ago", diff / 3600); }
        return format!("{}d ago", diff / 86400);
    }
    ts.split('T').next().unwrap_or(ts).to_string()
}

fn expires_label(p: &Publication) -> String {
    if p.keep_forever == Some(true) {
        return "never".to_string();
    }
    match &p.expires_at {
        None       => "never".to_string(),
        Some(ts)   => relative_time(ts),
    }
}

fn status_style(status: &str) -> (&'static str, &'static str) {
    // returns (background-color, color)
    match status {
        "active"   => ("#1a472a", "#4ade80"),
        "degraded" => ("#422006", "#fbbf24"),
        "dead" | "error" => ("#450a0a", "#f87171"),
        _          => ("#1e1e2e", "#9ca3af"), // expired, pending, unknown
    }
}

fn type_badge(type_: &str) -> &'static str {
    match type_ {
        "artifact" => "📦 artifact",
        "service"  => "🔗 service",
        _          => "⚙️ other",
    }
}

fn visibility_badge(vis: &str) -> &'static str {
    match vis {
        "public"  => "🌍 public",
        "fleet"   => "🛡️ fleet",
        "private" => "🔒 private",
        "link"    => "🔗 link",
        _         => "❓ unknown",
    }
}

// ── Component ─────────────────────────────────────────────────────────────────

#[component]
pub fn Published() -> impl IntoView {
    let (tick, set_tick) = create_signal(0u32);

    let data = create_resource(
        move || tick.get(),
        |_| fetch_publications(),
    );

    let refresh = move |_| set_tick.update(|t| *t = t.wrapping_add(1));

    view! {
        <div style="padding:1.5rem;max-width:1400px;margin:0 auto">
            <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:1.25rem">
                <h2 style="margin:0;font-size:1.4rem;font-weight:700;color:var(--text)">
                    "📡 Published"
                </h2>
                <button
                    class="svc-recheck-btn"
                    on:click=refresh
                >
                    {move || match data.loading().get() {
                        true  => "⟳ Loading…",
                        false => "↻ Refresh",
                    }}
                </button>
            </div>

            {move || match data.get() {
                None => view! {
                    <div style="color:var(--text-dim);padding:2rem;text-align:center">
                        "Loading publications…"
                    </div>
                }.into_view(),

                Some(pubs) if pubs.is_empty() => view! {
                    <div style="color:var(--text-dim);padding:3rem;text-align:center;border:1px dashed var(--border);border-radius:8px">
                        <div style="font-size:2rem;margin-bottom:0.75rem">"📭"</div>
                        <div>"No publications yet."</div>
                        <div style="margin-top:0.5rem;font-size:0.85rem;opacity:0.7">
                            "Agents can publish artifacts and services via "
                            <code style="background:var(--surface);padding:0.15em 0.4em;border-radius:4px">
                                "POST /api/publish"
                            </code>
                        </div>
                    </div>
                }.into_view(),

                Some(pubs) => view! {
                    <div style="overflow-x:auto">
                        <table style="width:100%;border-collapse:collapse;font-size:0.875rem">
                            <thead>
                                <tr style="border-bottom:1px solid var(--border);text-align:left">
                                    <th style="padding:0.6rem 0.75rem;color:var(--text-dim);font-weight:600">"Agent"</th>
                                    <th style="padding:0.6rem 0.75rem;color:var(--text-dim);font-weight:600">"Name"</th>
                                    <th style="padding:0.6rem 0.75rem;color:var(--text-dim);font-weight:600">"Type"</th>
                                    <th style="padding:0.6rem 0.75rem;color:var(--text-dim);font-weight:600">"URL"</th>
                                    <th style="padding:0.6rem 0.75rem;color:var(--text-dim);font-weight:600">"Visibility"</th>
                                    <th style="padding:0.6rem 0.75rem;color:var(--text-dim);font-weight:600">"Status"</th>
                                    <th style="padding:0.6rem 0.75rem;color:var(--text-dim);font-weight:600">"Created"</th>
                                    <th style="padding:0.6rem 0.75rem;color:var(--text-dim);font-weight:600">"Expires"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {pubs.into_iter().map(|p| {
                                    let (status_bg, status_fg) = status_style(&p.status);
                                    let status_label = p.status.clone();
                                    let type_label = type_badge(&p.type_).to_string();
                                    let vis_label = visibility_badge(&p.visibility).to_string();
                                    let created = p.created_at.as_deref()
                                        .map(relative_time)
                                        .unwrap_or_default();
                                    let expires = expires_label(&p);
                                    let url = p.url.clone();
                                    let agent = p.agent.clone();
                                    let name = p.name.clone();

                                    view! {
                                        <tr style="border-bottom:1px solid var(--border)">
                                            <td style="padding:0.6rem 0.75rem;color:var(--text);font-family:monospace;font-size:0.8rem">
                                                {agent}
                                            </td>
                                            <td style="padding:0.6rem 0.75rem;color:var(--text);font-weight:500">
                                                {name}
                                            </td>
                                            <td style="padding:0.6rem 0.75rem">
                                                <span style="font-size:0.8rem;padding:0.2em 0.5em;border-radius:4px;background:var(--surface);border:1px solid var(--border);white-space:nowrap">
                                                    {type_label}
                                                </span>
                                            </td>
                                            <td style="padding:0.6rem 0.75rem;max-width:220px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">
                                                {match url {
                                                    None => view! { <span style="color:var(--text-dim)">"-"</span> }.into_view(),
                                                    Some(href) => {
                                                        let display = href.clone();
                                                        view! {
                                                            <a
                                                                href={href}
                                                                target="_blank"
                                                                rel="noopener noreferrer"
                                                                style="color:var(--accent);text-decoration:none;font-family:monospace;font-size:0.8rem"
                                                            >
                                                                {display}
                                                            </a>
                                                        }.into_view()
                                                    },
                                                }}
                                            </td>
                                            <td style="padding:0.6rem 0.75rem">
                                                <span style="font-size:0.8rem;white-space:nowrap">
                                                    {vis_label}
                                                </span>
                                            </td>
                                            <td style="padding:0.6rem 0.75rem">
                                                <span style={format!("font-size:0.78rem;padding:0.2em 0.6em;border-radius:999px;background:{};color:{};font-weight:600", status_bg, status_fg)}>
                                                    {status_label}
                                                </span>
                                            </td>
                                            <td style="padding:0.6rem 0.75rem;color:var(--text-dim);font-size:0.82rem;white-space:nowrap">
                                                {created}
                                            </td>
                                            <td style="padding:0.6rem 0.75rem;color:var(--text-dim);font-size:0.82rem;white-space:nowrap">
                                                {expires}
                                            </td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                }.into_view(),
            }}
        </div>
    }
}
