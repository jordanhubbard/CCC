mod api;
mod types;
mod components;

use leptos::prelude::*;
use leptos::task::spawn_local;
use gloo_timers::callback::Interval;
use crate::types::{Tab, QueueItem, HeartbeatMap};
use crate::components::{
    nav::NavBar,
    agent_cards::AgentStrip,
    appeal_queue::AppealQueue,
    kanban::KanbanBoard,
    bus::BusTab,
    audit::AuditTab,
    profiler::ProfilerTab,
    geek_view::GeekView,
    metrics::MetricsPanel,
    providers::ProvidersPanel,
};

// ── Root App ──────────────────────────────────────────────────────────────────

#[component]
fn App() -> impl IntoView {
    // Current tab
    let tab = RwSignal::new(Tab::Overview);

    // Shared data signals
    let queue:      RwSignal<Vec<QueueItem>>  = RwSignal::new(vec![]);
    let heartbeats: RwSignal<HeartbeatMap>    = RwSignal::new(HeartbeatMap::new());
    let queue_err:  RwSignal<Option<String>>  = RwSignal::new(None);
    let hb_err:     RwSignal<Option<String>>  = RwSignal::new(None);

    // Initial fetch
    {
        let queue = queue;
        let heartbeats = heartbeats;
        let queue_err = queue_err;
        let hb_err = hb_err;
        spawn_local(async move {
            match api::fetch_queue().await {
                Ok(items) => { queue.set(items); queue_err.set(None); }
                Err(e)    => { queue_err.set(Some(e)); }
            }
            match api::fetch_heartbeats().await {
                Ok(hbs) => { heartbeats.set(hbs); hb_err.set(None); }
                Err(e)  => { hb_err.set(Some(e)); }
            }
        });
    }

    // Poll every 15s
    {
        let queue = queue;
        let heartbeats = heartbeats;
        let queue_err = queue_err;
        let hb_err = hb_err;
        let _interval = Interval::new(15_000, move || {
            let q = queue;
            let h = heartbeats;
            let qe = queue_err;
            let he = hb_err;
            spawn_local(async move {
                match api::fetch_queue().await {
                    Ok(items) => { q.set(items); qe.set(None); }
                    Err(e)    => { qe.set(Some(e)); }
                }
                match api::fetch_heartbeats().await {
                    Ok(hbs) => { h.set(hbs); he.set(None); }
                    Err(e)  => { he.set(Some(e)); }
                }
            });
        });
        // Leak the interval so it lives for the app lifetime
        _interval.forget();
    }

    view! {
        <div id="app">
            <NavBar tab=tab />
            <div class="tab-content">
                {move || match tab.get() {
                    Tab::Overview => view! {
                        <div>
                            <div class="section-title">"Agent Status"</div>
                            <AgentStrip heartbeats=heartbeats />
                            {move || hb_err.get().map(|e| view! {
                                <div class="error-msg">{format!("Heartbeat error: {}", e)}</div>
                            })}
                            <div class="section-title">"jkh Appeal Queue"</div>
                            <AppealQueue queue=queue />
                            <div class="section-title">"Queue Metrics"</div>
                            <MetricsPanel queue=queue />
                        </div>
                    }.into_any(),
                    Tab::Kanban => view! {
                        <KanbanBoard queue=queue heartbeats=heartbeats />
                    }.into_any(),
                    Tab::Bus => view! {
                        <BusTab />
                    }.into_any(),
                    Tab::Audit => view! {
                        <AuditTab />
                    }.into_any(),
                    Tab::Profiler => view! {
                        <ProfilerTab />
                    }.into_any(),
                    Tab::Providers => view! {
                        <ProvidersPanel />
                    }.into_any(),
                    Tab::GeekView => view! {
                        <GeekView heartbeats=heartbeats />
                    }.into_any(),
                    Tab::Projects => view! {
                        <div class="stub-tab">
                            <h2>"Projects"</h2>
                            <p>"Project health cards — coming in Phase 2"</p>
                        </div>
                    }.into_any(),
                    Tab::Calendar => view! {
                        <div class="stub-tab">
                            <h2>"Calendar"</h2>
                            <p>"Shared agent calendar — coming in Phase 2"</p>
                        </div>
                    }.into_any(),
                    Tab::Settings => view! {
                        <div class="stub-tab">
                            <h2>"Settings"</h2>
                            <p>"Comms channels and agent config — coming in Phase 2"</p>
                        </div>
                    }.into_any(),
                }}
                {move || queue_err.get().map(|e| view! {
                    <div class="error-msg" style="margin-top:8px">{format!("Queue error: {}", e)}</div>
                })}
            </div>
        </div>
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    leptos::mount::mount_to_body(App);
}
