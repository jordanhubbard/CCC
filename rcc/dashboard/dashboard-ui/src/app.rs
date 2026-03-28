use leptos::*;

use crate::components::{
    activity_feed::ActivityFeed,
    agent_cards::AgentCards,
    bus_send::BusSend,
    changelog::Changelog,
    geek_view::GeekView,
    idea_incubator::IdeaIncubator,
    kanban::Kanban,
    metrics::Metrics,
    squirrelbus::SquirrelBus,
    squirrelchat::SquirrelChat,
    work_queue::WorkQueue,
};

#[component]
pub fn App() -> impl IntoView {
    // 0 = Dashboard, 1 = Geek View, 2 = Kanban, 3 = Chat
    let (tab, set_tab) = create_signal(0u8);

    view! {
        <div class="dashboard">
            <header class="dash-header">
                <div class="dash-logo">
                    <span class="logo-icon">"⚡"</span>
                    <span class="logo-text">"Rocky Command Center"</span>
                </div>
                <div class="dash-subtitle">"v2 — Rust/WASM"</div>
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
                    >"💬 Chat"</button>
                </div>
            </header>
            <main class="dash-main">
                {move || match tab.get() {
                    1 => view! { <GeekView /> }.into_view(),
                    2 => view! { <Kanban /> }.into_view(),
                    3 => view! { <SquirrelChat /> }.into_view(),
                    _ => view! {
                        <div class="dash-main-content">
                            <div class="dash-row dash-row-top">
                                <AgentCards />
                                <Metrics />
                                <BusSend />
                            </div>
                            <div class="dash-row">
                                <WorkQueue />
                            </div>
                            <div class="dash-row">
                                <SquirrelBus />
                                <IdeaIncubator />
                                <ActivityFeed />
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
