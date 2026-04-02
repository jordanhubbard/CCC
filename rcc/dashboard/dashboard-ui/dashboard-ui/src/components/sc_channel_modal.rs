// sc_channel_modal.rs — Channel create/edit modal for ClawChat
// Bullwinkle (Track A UI)

use leptos::*;

// ─── Create Channel Modal ────────────────────────────────────────────────────

/// Modal for creating a new channel.
/// `on_create` fires with (id_slug, display_name, description).
/// `on_close` dismisses the modal.
#[component]
pub fn CreateChannelModal(
    on_create: Callback<(String, String, String)>,
    on_close: Callback<()>,
) -> impl IntoView {
    let (name, set_name) = create_signal(String::new());
    let (desc, set_desc) = create_signal(String::new());

    let slug = create_memo(move |_| {
        name.get()
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    });

    view! {
        <div class="sc-modal-overlay" on:click=move |_| on_close.call(())>
            <div class="sc-modal" on:click=|ev| ev.stop_propagation()>
                <h3 class="sc-modal-title">"New Channel"</h3>

                <label class="sc-modal-label">"Channel name"</label>
                <input
                    type="text"
                    class="sc-modal-input"
                    placeholder="e.g. project-alpha"
                    prop:value=move || name.get()
                    on:input=move |ev| set_name.set(event_target_value(&ev))
                />
                <div class="sc-modal-slug">
                    "ID: #" {move || slug.get()}
                </div>

                <label class="sc-modal-label">"Description (optional)"</label>
                <textarea
                    class="sc-modal-textarea"
                    placeholder="What's this channel about?"
                    prop:value=move || desc.get()
                    on:input=move |ev| set_desc.set(event_target_value(&ev))
                />

                <div class="sc-modal-actions">
                    <button
                        class="sc-btn-primary"
                        prop:disabled=move || name.get().trim().is_empty()
                        on:click=move |_| {
                            let n = name.get_untracked().trim().to_string();
                            if n.is_empty() { return; }
                            let s = slug.get_untracked();
                            let d = desc.get_untracked();
                            on_create.call((s, n, d));
                        }
                    >"Create Channel"</button>
                    <button class="sc-btn-cancel" on:click=move |_| on_close.call(())>
                        "Cancel"
                    </button>
                </div>
            </div>
        </div>
    }
}
