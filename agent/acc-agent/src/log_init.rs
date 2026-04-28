//! Tracing initialization for acc-agent (CCC-u3c).
//!
//! Long-running daemon subcommands get a journald layer (where systemd
//! is reachable) so a fleet operator can `journalctl -t acc-agent -f`
//! across all agents and see real-time activity in one place. Stderr
//! always also gets a fmt layer so SSH-tailing the supervisor log
//! continues to work.
//!
//! Short-lived subcommands (migrate, agent, json) skip journald to
//! avoid spamming the journal with one-shot CLI invocations.

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const DAEMON_SUBCOMMANDS: &[&str] = &[
    "bus",
    "listen",
    "queue",
    "tasks",
    "hermes",
    "proxy",
    "supervise",
];

pub fn init(subcommand: &str) {
    let env_filter = tracing_subscriber::EnvFilter::new(
        std::env::var("RUST_LOG").unwrap_or_else(|_| "acc_agent=info,info".into()),
    );
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(std::io::stderr);
    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer);

    let want_journald = DAEMON_SUBCOMMANDS.contains(&subcommand);
    if want_journald {
        match tracing_journald::layer() {
            Ok(layer) => {
                // tracing_journald uses the binary name as syslog identifier
                // by default; tag explicitly so cross-agent grep is clean.
                registry
                    .with(layer.with_syslog_identifier(format!("acc-agent-{subcommand}")))
                    .init();
                return;
            }
            Err(_) => {
                // Not on systemd (macOS, container without /run/systemd, etc.)
            }
        }
    }
    registry.init();
}
