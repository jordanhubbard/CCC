/// Health roll-up banner — surfaces "needs attention" items at a glance.
///
/// Uses the shared DashboardContext (no independent fetch) to eliminate
/// the FOUC / layout-shift race during mount.
use leptos::*;

use crate::context::DashboardContext;

fn parse_ts_sec(ts: &str) -> Option<u64> {
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
    let days = days_since_epoch(y, m, d);
    Some(days * 86400 + h * 3600 + mi * 60 + s as u64)
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

fn age_secs(ts: &str) -> u64 {
    let now = (js_sys::Date::now() as u64) / 1000;
    parse_ts_sec(ts).map(|t| now.saturating_sub(t)).unwrap_or(0)
}

fn fmt_age(secs: u64) -> String {
    if secs < 3600 { format!("{}m", secs / 60) }
    else if secs < 86400 { format!("{}h", secs / 3600) }
    else { format!("{}d", secs / 86400) }
}

#[derive(Clone)]
struct Alert {
    level: &'static str, // "red" | "orange" | "yellow"
    icon: &'static str,
    text: String,
}

#[component]
pub fn HealthBanner() -> impl IntoView {
    let ctx = use_context::<DashboardContext>().expect("DashboardContext missing");
    let (dismissed, set_dismissed) = create_signal(false);
    let heartbeats = ctx.heartbeats;
    let queue = ctx.queue;

    let alerts = move || -> Vec<Alert> {
        if dismissed.get() { return vec![]; }

        let hb_map = heartbeats.get().unwrap_or_default();
        let q = queue.get().unwrap_or_default();
        let now = (js_sys::Date::now() as u64) / 1000;
        let mut alerts: Vec<Alert> = vec![];

        // ── Stale / offline agents ──────────────────────────────────────
        for (name, hb) in &hb_map {
            if hb.decommissioned.unwrap_or(false) { continue; }
            let stale_secs = hb.ts.as_deref()
                .and_then(|ts| parse_ts_sec(ts))
                .map(|t| now.saturating_sub(t))
                .unwrap_or(u64::MAX);

            if stale_secs > 7200 {
                // >2h stale — flag regardless of onlineStatus field
                let age = fmt_age(stale_secs);
                let level = if stale_secs > 86400 { "red" } else { "orange" };
                alerts.push(Alert {
                    level,
                    icon: "⚠",
                    text: format!("{} — last heartbeat {}+ ago (stale)", name, age),
                });
            }
        }

        // ── High-priority queue items ─────────────────────────────────
        let high_pending: Vec<String> = q.items.iter()
            .filter(|i| {
                let s = i.status.as_deref().unwrap_or("pending");
                let p = i.priority.as_deref().unwrap_or("medium");
                (p == "high" || p == "critical") && (s == "pending" || s == "claimed")
            })
            .map(|i| {
                let assignee = i.assignee.as_deref().unwrap_or("?");
                let age = i.created_at.as_deref().map(|t| {
                    let secs = age_secs(t);
                    fmt_age(secs)
                }).unwrap_or_default();
                format!("{} (→{}, {})", &i.title.chars().take(60).collect::<String>(), assignee, age)
            })
            .collect();

        if !high_pending.is_empty() {
            alerts.push(Alert {
                level: "orange",
                icon: "🔥",
                text: format!("{} high-priority item{} pending: {}",
                    high_pending.len(),
                    if high_pending.len() == 1 { "" } else { "s" },
                    high_pending.join(" · ")),
            });
        }

        // ── jkh decision items ────────────────────────────────────────
        let jkh_decisions: usize = q.items.iter()
            .filter(|i| {
                let s = i.status.as_deref().unwrap_or("pending");
                let assignee = i.assignee.as_deref().unwrap_or("").to_lowercase();
                s == "pending" && assignee == "jkh"
            })
            .count();

        if jkh_decisions > 0 {
            alerts.push(Alert {
                level: "yellow",
                icon: "👤",
                text: format!("{} item{} awaiting jkh decision",
                    jkh_decisions,
                    if jkh_decisions == 1 { "" } else { "s" }),
            });
        }

        alerts
    };

    view! {
        {move || {
            let items = alerts();
            if items.is_empty() {
                return view! { <div></div> }.into_view();
            }
            view! {
                <div style="
                    background: var(--surface2);
                    border-bottom: 1px solid var(--border);
                    padding: 8px 16px;
                    display: flex;
                    flex-direction: column;
                    gap: 4px;
                ">
                    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:2px;">
                        <span style="font-size:11px;font-weight:700;color:var(--text-dim);letter-spacing:0.06em;text-transform:uppercase;">
                            "⚡ Needs Attention"
                        </span>
                        <button
                            style="background:none;border:none;color:var(--text-dimmer);cursor:pointer;font-size:14px;padding:0 4px;line-height:1;"
                            on:click=move |_| set_dismissed.set(true)
                        >"×"</button>
                    </div>
                    {items.into_iter().map(|alert| {
                        let color = match alert.level {
                            "red"    => "var(--red)",
                            "orange" => "var(--orange)",
                            _        => "#b8960c",
                        };
                        let bg = match alert.level {
                            "red"    => "rgba(248,81,73,0.08)",
                            "orange" => "rgba(227,179,65,0.08)",
                            _        => "rgba(184,150,12,0.08)",
                        };
                        view! {
                            <div style=format!(
                                "display:flex;align-items:flex-start;gap:8px;padding:4px 8px;border-radius:var(--radius-sm);background:{};border-left:3px solid {};",
                                bg, color
                            )>
                                <span style="flex-shrink:0;">{alert.icon}</span>
                                <span style=format!("font-size:12px;color:{};line-height:1.4;", color)>
                                    {alert.text}
                                </span>
                            </div>
                        }
                    }).collect::<Vec<_>>()}
                </div>
            }.into_view()
        }}
    }
}
