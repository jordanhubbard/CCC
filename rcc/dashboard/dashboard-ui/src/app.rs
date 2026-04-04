use leptos::*;
use leptos_router::*;

use crate::context::DashboardContext;
use crate::components::{
    activity_feed::ActivityFeed,
    agent_cards::AgentCards,
    agent_detail::AgentDetail,
    clawfs::AgentFs,
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
    settings::Settings,
    model_deploy::ModelDeploy,
    clawbus::ClawBus,
    work_queue::WorkQueue,
    published::Published,
};

/// Map URL path → tab index
fn path_to_tab(path: &str) -> u8 {
    match path {
        "/geek-view"  | "/geek_view"  => 1,
        "/kanban"                      => 2,
        "/agents"                      => 4,
        "/issues"                      => 5,
        "/providers"                   => 6,
        "/coding"                      => 7,
        "/services"                    => 8,
        "/clawfs"                     => 10,
        "/settings"                    => 11,
        "/models"                      => 12,
        "/published"                   => 13,
        _                              => 0, // default: Dashboard
    }
}

/// Map tab index → canonical URL path
fn tab_to_path(tab: u8) -> &'static str {
    match tab {
        1 => "/geek-view",
        2 => "/kanban",
        4 => "/agents",
        5 => "/issues",
        6 => "/providers",
        7 => "/coding",
        8 => "/services",
        10 => "/clawfs",
        11 => "/settings",
        12 => "/models",
        13 => "/published",
        _  => "/",
    }
}

#[component]
pub fn App() -> impl IntoView {
    view! {
        <Router>
            <AppInner />
        </Router>
    }
}

#[component]
fn AppInner() -> impl IntoView {
    // Shared data context — eliminates per-component fetch races / FOUC
    provide_context(DashboardContext::new());

    let location = use_location();
    let navigate = use_navigate();

    // Derive current tab from the URL path (reactive)
    let tab = create_memo(move |_| path_to_tab(&location.pathname.get()));

    // Navigate to the path for a tab, updating the URL bar.
    // Wrap in StoredValue so each on:click closure can access it without moving.
    let nav = store_value(navigate);
    let select_tab = move |t: u8| {
        let path = tab_to_path(t);
        nav.with_value(|n| n(path, Default::default()));
    };
    let select_tab = store_value(select_tab);

    view! {
        <div class="dashboard">
            <header class="dash-header">
                <div class="dash-logo">
                    <span class="logo-icon">"🐿️"</span>
                    <span class="logo-text">"Claw Command Center"</span>
                </div>
                <div class="dash-subtitle">"v3 — Rust/WASM + GH Issues"</div>
                <div class="dash-tabs">
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 0
                        on:click=move |_| select_tab.with_value(|f| f(0))
                    >"Dashboard"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 1
                        on:click=move |_| select_tab.with_value(|f| f(1))
                    >"🧠 Geek View"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 2
                        on:click=move |_| select_tab.with_value(|f| f(2))
                    >"📋 Kanban"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 4
                        on:click=move |_| select_tab.with_value(|f| f(4))
                    >"🤖 Agents"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 5
                        on:click=move |_| select_tab.with_value(|f| f(5))
                    >"🐛 Issues"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 6
                        on:click=move |_| select_tab.with_value(|f| f(6))
                    >"🔌 Providers"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 7
                        on:click=move |_| select_tab.with_value(|f| f(7))
                    >"⚡ Coding"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 8
                        on:click=move |_| select_tab.with_value(|f| f(8))
                    >"🗺️ Services"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 10
                        on:click=move |_| select_tab.with_value(|f| f(10))
                    >"📁 ClawFS"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 11
                        on:click=move |_| select_tab.with_value(|f| f(11))
                    >"⚙️ Settings"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 12
                        on:click=move |_| select_tab.with_value(|f| f(12))
                    >"🚀 Models"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 13
                        on:click=move |_| select_tab.with_value(|f| f(13))
                    >"📡 Published"</button>
                </div>
            </header>
            <main class="dash-main">
                <HealthBanner />
                {move || match tab.get() {
                    1 => view! { <GeekView /> }.into_view(),
                    2 => view! { <Kanban /> }.into_view(),
                    4 => view! { <AgentDetail /> }.into_view(),
                    5 => view! { <Issues /> }.into_view(),
                    6 => view! { <Providers /> }.into_view(),
                    7 => view! { <CodingAgent /> }.into_view(),
                    8 => view! { <Services /> }.into_view(),
                    10 => view! { <AgentFs /> }.into_view(),
                    11 => view! { <Settings /> }.into_view(),
                    12 => view! { <ModelDeploy /> }.into_view(),
                    13 => view! { <Published /> }.into_view(),
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
                                <ClawBus />
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
