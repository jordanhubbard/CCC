// sc_reactions.rs — Emoji picker and reactions bar components for SquirrelChat
// Bullwinkle (Track A UI) — imports from sc_types.rs

use leptos::*;
use crate::components::sc_types::{ScReaction, REACTION_EMOJIS};

// ─── Emoji Picker ────────────────────────────────────────────────────────────

/// A compact emoji picker that shows the REACTION_EMOJIS palette.
/// Fires `on_pick` with the selected emoji string when clicked.
/// `visible` controls show/hide (toggled by the parent react button).
#[component]
pub fn EmojiPicker(
    visible: ReadSignal<bool>,
    on_pick: Callback<String>,
    #[prop(optional)] on_close: Option<Callback<()>>,
) -> impl IntoView {
    view! {
        <Show when=move || visible.get()>
            <div class="sc-emoji-picker" on:mouseleave=move |_| {
                if let Some(cb) = &on_close {
                    cb.call(());
                }
            }>
                {REACTION_EMOJIS.iter().map(|&emoji| {
                    let emoji_str = emoji.to_string();
                    let emoji_click = emoji_str.clone();
                    view! {
                        <button
                            class="sc-emoji-btn"
                            title=emoji_str.clone()
                            on:click=move |ev| {
                                ev.stop_propagation();
                                on_pick.call(emoji_click.clone());
                            }
                        >
                            {emoji_str}
                        </button>
                    }
                }).collect::<Vec<_>>()}
            </div>
        </Show>
    }
}

// ─── Reactions Bar ───────────────────────────────────────────────────────────

/// Displays the reactions on a message as clickable emoji pills.
/// Each pill shows the emoji + count. Clicking toggles the current user's reaction.
/// `current_user` is the user id for highlight (shows which reactions the user has applied).
#[component]
pub fn ReactionsBar(
    reactions: Vec<ScReaction>,
    current_user: String,
    message_id: i64,
    on_toggle: Callback<(i64, String)>,
) -> impl IntoView {
    if reactions.is_empty() {
        return view! { <span /> }.into_view();
    }

    let pills: Vec<_> = reactions
        .into_iter()
        .map(|r| {
            let user_reacted = r.agents.iter().any(|u| u == &current_user);
            let tooltip = r.agents.join(", ");
            let emoji_clone = r.emoji.clone();
            let msg_id = message_id;

            view! {
                <button
                    class="sc-reaction-pill"
                    class:sc-reaction-mine=user_reacted
                    title=tooltip
                    on:click=move |ev| {
                        ev.stop_propagation();
                        on_toggle.call((msg_id, emoji_clone.clone()));
                    }
                >
                    <span class="sc-reaction-emoji">{r.emoji.clone()}</span>
                    <span class="sc-reaction-count">{r.count}</span>
                </button>
            }
        })
        .collect();

    view! {
        <div class="sc-reactions-bar">
            {pills}
        </div>
    }
    .into_view()
}
