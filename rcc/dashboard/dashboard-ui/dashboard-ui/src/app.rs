use leptos::*;

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
    work_queue::WorkQueue,
};

#[component]
pub fn App() -> impl IntoView {
    // Shared data context — eliminates per-component fetch races / FOUC
    provide_context(DashboardContext::new());

    // 0=Dashboard 1=GeekView 2=Kanban 3=SquirrelChat 4=Agents 5=Issues 6=Providers
    let (tab, set_tab) = create_signal(0u8);

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
                        on:click=move |_| set_tab.set(0)
                    >"Dashboard"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 1
                        on:click=move |_| set_tab.set(1)
                    >"🧠 Geek View"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 2
                        on:click=move |_| set_tab.set(2)
                    >"📋 Kanban"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 3
                        on:click=move |_| set_tab.set(3)
                    >"💬 SquirrelChat"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 4
                        on:click=move |_| set_tab.set(4)
                    >"🤖 Agents"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 5
                        on:click=move |_| set_tab.set(5)
                    >"🐛 Issues"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 6
                        on:click=move |_| set_tab.set(6)
                    >"🔌 Providers"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 7
                        on:click=move |_| set_tab.set(7)
                    >"⚡ Coding"</button>
                    <button
                        class="tab-btn"
                        class:tab-active=move || tab.get() == 8
                        on:click=move |_| set_tab.set(8)
                    >"🗺️ Services"</button>
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
