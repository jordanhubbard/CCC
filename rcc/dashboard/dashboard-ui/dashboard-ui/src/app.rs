use leptos::*;
use wasm_bindgen::JsValue;

use crate::context::DashboardContext;
use crate::components::{
    activity_feed::ActivityFeed,
    agent_cards::AgentCards,
    agent_detail::AgentDetail,
    bus_send::BusSend,
    changelog::Changelog,
    coding_agent::CodingAgent,
    geek_view::GeekView,
    health_banner::HealthBanner,
    idea_incubator::IdeaIncubator,
    issues::Issues,
    kanban::Kanban,
    metrics::Metrics,
    providers::Providers,
    services::Services,
    squirrelbus::SquirrelBus,
    squirrelchat::SquirrelChat,
    timeline::Timeline,
    work_queue::WorkQueue,
};

/// Map tab index → URL path segment
fn tab_to_path(tab: u8) -> &'static str {
    match tab {
        1 => "/geek",
        2 => "/kanban",
        3 => "/chat",
        4 => "/agents",
        5 => "/issues",
        6 => "/providers",
        7 => "/coding",
        8 => "/services",
        9 => "/timeline",
        _ => "/",
    }
}

/// Map URL path segment → tab index
fn path_to_tab(path: &str) -> u8 {
    // Strip trailing slash variants, case-insensitive
    match path.trim_end_matches('/') {
        "/geek"      => 1,
        "/kanban"    => 2,
        "/chat"      => 3,
        "/agents"    => 4,
        "/issues"    => 5,
        "/providers" => 6,
        "/coding"    => 7,
        "/services"  => 8,
        "/timeline"  => 9,
        _            => 0,
    }
}

/// Read the current window.location.pathname
fn current_path() -> String {
    web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .unwrap_or_default()
}

/// Push a new history entry for the given tab
fn push_tab(tab: u8) {
    let path = tab_to_path(tab);
    if let Some(window) = web_sys::window() {
        let history = window.history().expect("no history");
        let _ = history.push_state_with_url(
            &JsValue::NULL,
            "",
            Some(path),
        );
    }
}

#[component]
pub fn App() -> impl IntoView {
    // Shared data context — eliminates per-component fetch races / FOUC
    provide_context(DashboardContext::new());

    // Initialise tab from the current URL path
    let initial_tab = path_to_tab(&current_path());
    let (tab, set_tab) = create_signal(initial_tab);

    // Helper: set tab + push history
    let navigate = move |t: u8| {
        push_tab(t);
        set_tab.set(t);
    };

    view! {
        <div class="dashboard">
            <header class="dash-header">
                <div class="dash-logo">
                    <span class="logo-icon">"🐿️"</span>
                    <span class="logo-text">"Rocky Command Center"</span>
                </div>
                <div class="dash-subtitle">"v3 — Rust/WASM + GH Issues"</div>
                <div class="dash-tabs">
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 0
                        on:click=move |_| navigate(0)
                    >"Dashboard"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 1
                        on:click=move |_| navigate(1)
                    >"🧠 Geek View"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 2
                        on:click=move |_| navigate(2)
                    >"📋 Kanban"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 3
                        on:click=move |_| navigate(3)
                    >"💬 SquirrelChat"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 4
                        on:click=move |_| navigate(4)
                    >"🤖 Agents"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 5
                        on:click=move |_| navigate(5)
                    >"🐛 Issues"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 6
                        on:click=move |_| navigate(6)
                    >"🔌 Providers"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 7
                        on:click=move |_| navigate(7)
                    >"⚡ Coding"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 8
                        on:click=move |_| navigate(8)
                    >"🗺️ Services"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 9
                        on:click=move |_| navigate(9)
                    >"⏱️ Timeline"</button>
                </div>
            </header>
            <main class="dash-main">
                <HealthBanner />
                {move || match tab.get() {
                    1 => view! { <GeekView /> }.into_view(),
                    2 => view! { <Kanban /> }.into_view(),
                    3 => view! { <SquirrelChat /> }.into_view(),
                    4 => view! { <AgentDetail /> }.into_view(),
                    5 => view! { <Issues /> }.into_view(),
                    6 => view! { <Providers /> }.into_view(),
                    7 => view! { <CodingAgent /> }.into_view(),
                    8 => view! { <Services /> }.into_view(),
                    9 => view! { <Timeline /> }.into_view(),
                    _ => view! {
                        <div class="dash-main-content">
                            <div class="dash-row dash-row-top">
                                <AgentCards />
                                <Metrics />
                            </div>
                            <div class="dash-row">
                                <WorkQueue />
                                <ActivityFeed />
                            </div>
                            <div class="dash-row">
                                <SquirrelBus />
                                <BusSend />
                                <IdeaIncubator />
                            </div>
                            <div class="dash-row">
                                <Changelog />
                            </div>
                        </div>
                    }.into_view(),
                }}
            </main>
        </div>
    }
}
