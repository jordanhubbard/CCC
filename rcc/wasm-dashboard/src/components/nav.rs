use leptos::prelude::*;
use crate::types::Tab;

const TABS: &[Tab] = &[
    Tab::Overview,
    Tab::Kanban,
    Tab::Bus,
    Tab::Providers,
    Tab::Projects,
    Tab::Calendar,
    Tab::Audit,
    Tab::GeekView,
    Tab::Settings,
];

#[component]
pub fn NavBar(tab: RwSignal<Tab>) -> impl IntoView {
    view! {
        <nav class="topnav">
            <span class="logo">"🐿️ RCC"</span>
            {TABS.iter().map(|&t| {
                view! {
                    <button
                        class=move || if tab.get() == t { "active" } else { "" }
                        on:click=move |_| tab.set(t)
                    >
                        {t.label()}
                    </button>
                }
            }).collect_view()}
            <span class="spacer" />
            <span class="live-dot online">"● live"</span>
        </nav>
    }
}
