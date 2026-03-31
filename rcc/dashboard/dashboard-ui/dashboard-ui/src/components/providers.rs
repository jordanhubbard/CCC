// providers.rs — Channel provider integrations (stub)
// Owned by Bullwinkle. This module will wire SquirrelChat into OpenClaw
// as a channel provider alongside Slack.
//
// TODO: implement ScProvider trait, SC channel adapter

use leptos::*;

#[component]
pub fn Providers() -> impl IntoView {
    view! {
        <div class="providers-panel">
            <h3>"Channel Providers"</h3>
            <p class="providers-stub">"Coming soon — SquirrelChat native channel integration"</p>
        </div>
    }
}
