use std::collections::HashSet;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::cache::WorkspaceCache;
use crate::slack_api::SlackApi;
use crate::socket_mode::IncomingMessage;
use crate::thread_map::ThreadMap;

/// The core reflector loop. Receives messages from both workspaces
/// and mirrors them to the peer workspace when user+channel match by name.
pub async fn run_reflector(
    mut rx: mpsc::Receiver<IncomingMessage>,
    apis: [SlackApi; 2],
    caches: [WorkspaceCache; 2],
    exclude_channels: HashSet<String>,
    exclude_users: HashSet<String>,
    thread_map: ThreadMap,
) {
    info!("reflector loop started");

    while let Some(msg) = rx.recv().await {
        let src_idx = msg.workspace_idx;
        let dst_idx = if src_idx == 0 { 1 } else { 0 };
        let src_cache = &caches[src_idx];
        let dst_cache = &caches[dst_idx];
        let dst_api = &apis[dst_idx];

        // 1. Skip messages from our own bot
        if src_cache.is_bot_message(&msg.user_id) {
            debug!("[reflector] skipping bot's own message");
            continue;
        }

        // 2. Resolve source user_id -> username
        let username = match src_cache.user_name(&msg.user_id) {
            Some(name) => name,
            None => {
                debug!(
                    "[reflector] unknown user_id {} on workspace {}, skipping",
                    msg.user_id, src_cache.workspace_name
                );
                continue;
            }
        };

        // 3. Check user exclusion
        if exclude_users.contains(&username) {
            debug!("[reflector] user '{}' is excluded, skipping", username);
            continue;
        }

        // 4. Check if user exists on destination by name
        if dst_cache.users_by_name.get(&username).is_none() {
            debug!(
                "[reflector] user '{}' not found on {}, skipping",
                username, dst_cache.workspace_name
            );
            continue;
        }

        // 5. Resolve source channel_id -> channel name
        let channel_name = match src_cache.channel_name(&msg.channel_id) {
            Some(name) => name,
            None => {
                debug!(
                    "[reflector] unknown channel_id {} on workspace {}, skipping",
                    msg.channel_id, src_cache.workspace_name
                );
                continue;
            }
        };

        // 6. Check channel exclusion
        if exclude_channels.contains(&channel_name) {
            debug!("[reflector] channel '{}' is excluded, skipping", channel_name);
            continue;
        }

        // 7. Resolve destination channel by name
        let dst_channel_id = match dst_cache.channel_id(&channel_name) {
            Some(id) => id,
            None => {
                debug!(
                    "[reflector] channel '{}' not found on {}, skipping",
                    channel_name, dst_cache.workspace_name
                );
                continue;
            }
        };

        // 8. Handle threading — look up mirrored thread_ts
        let dst_thread_ts = msg.thread_ts.as_ref().and_then(|src_thread_ts| {
            thread_map.get(src_idx, src_thread_ts)
        });

        info!(
            "[reflector] {} #{} @{}: mirroring to {} #{}",
            caches[src_idx].workspace_name,
            channel_name,
            username,
            caches[dst_idx].workspace_name,
            channel_name
        );

        // 9. Post the message
        match dst_api
            .post_message(
                &dst_channel_id,
                &msg.text,
                &username,
                dst_thread_ts.as_deref(),
            )
            .await
        {
            Ok(Some(mirrored_ts)) => {
                // Record the thread mapping in both directions:
                // source_ts -> mirrored_ts (so replies on source find the mirror thread)
                thread_map.insert(src_idx, &msg.ts, &mirrored_ts);
                // mirrored_ts -> source_ts (so replies on mirror find the source thread)
                thread_map.insert(dst_idx, &mirrored_ts, &msg.ts);

                // If this message was in a thread, also map the parent
                if let Some(ref src_thread_ts) = msg.thread_ts {
                    if let Some(ref dst_thread) = dst_thread_ts {
                        // Already mapped
                        let _ = (src_thread_ts, dst_thread);
                    }
                }
            }
            Ok(None) => {
                warn!(
                    "[reflector] failed to post to {} #{}",
                    caches[dst_idx].workspace_name, channel_name
                );
            }
            Err(e) => {
                warn!(
                    "[reflector] error posting to {} #{}: {:#}",
                    caches[dst_idx].workspace_name, channel_name, e
                );
            }
        }

        // Periodic thread map maintenance
        thread_map.prune_if_needed(50_000);
    }

    warn!("reflector loop exited — message channel closed");
}
