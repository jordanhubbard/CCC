use leptos::prelude::*;
use leptos::task::spawn_local;
use gloo_timers::callback::Interval;
use crate::api;
use crate::types::Provider;

#[component]
pub fn ProvidersPanel() -> impl IntoView {
    let providers: RwSignal<Vec<Provider>> = RwSignal::new(vec![]);
    let error: RwSignal<Option<String>>    = RwSignal::new(None);
    let loading: RwSignal<bool>            = RwSignal::new(true);

    // Initial fetch
    {
        let providers = providers;
        let error = error;
        let loading = loading;
        spawn_local(async move {
            match api::fetch_providers().await {
                Ok(p)  => { providers.set(p); error.set(None); }
                Err(e) => { error.set(Some(e)); }
            }
            loading.set(false);
        });
    }

    // Poll every 30s
    {
        let providers = providers;
        let error = error;
        let _interval = Interval::new(30_000, move || {
            let p = providers;
            let e = error;
            spawn_local(async move {
                match api::fetch_providers().await {
                    Ok(items) => { p.set(items); e.set(None); }
                    Err(err)  => { e.set(Some(err)); }
                }
            });
        });
        _interval.forget();
    }

    view! {
        <div class="providers-panel">
            <div class="section-title">"⚡ Token Providers"</div>

            {move || error.get().map(|e| view! {
                <div class="error-msg">{format!("Error: {}", e)}</div>
            })}

            {move || {
                if loading.get() {
                    return view! { <div class="loading-msg">"Loading providers..."</div> }.into_any();
                }
                let items = providers.get();
                if items.is_empty() {
                    return view! {
                        <div class="empty-state">"No providers registered. Use POST /api/providers to register one."</div>
                    }.into_any();
                }
                view! {
                    <div class="provider-grid">
                        {items.into_iter().map(|p| view! { <ProviderCard provider=p /> }).collect::<Vec<_>>()}
                    </div>
                }.into_any()
            }}
        </div>
    }
}

#[component]
fn ProviderCard(provider: Provider) -> impl IntoView {
    let status_class = provider.status_class().to_string();
    let ctx_label = provider.context_label();
    let tags = provider.tags.join(", ");
    let owner = provider.owner.clone().unwrap_or_else(|| "—".to_string());
    let base_url = provider.base_url.clone().unwrap_or_else(|| "—".to_string());

    view! {
        <div class="provider-card">
            <div class="provider-header">
                <span class={format!("status-dot {}", status_class)}></span>
                <span class="provider-id">{provider.id.clone()}</span>
                <span class={format!("provider-status-badge {}", status_class)}>
                    {provider.status.clone()}
                </span>
            </div>
            <div class="provider-model">{provider.model.clone()}</div>
            <div class="provider-meta">
                <div class="meta-row">
                    <span class="meta-label">"Owner"</span>
                    <span class="meta-value">{owner}</span>
                </div>
                <div class="meta-row">
                    <span class="meta-label">"Context"</span>
                    <span class="meta-value">{ctx_label}</span>
                </div>
                <div class="meta-row">
                    <span class="meta-label">"URL"</span>
                    <span class="meta-value provider-url">{base_url}</span>
                </div>
                {if !tags.is_empty() {
                    view! {
                        <div class="meta-row">
                            <span class="meta-label">"Tags"</span>
                            <span class="meta-value provider-tags">{tags}</span>
                        </div>
                    }.into_any()
                } else {
                    view! { <span></span> }.into_any()
                }}
            </div>
        </div>
    }
}
