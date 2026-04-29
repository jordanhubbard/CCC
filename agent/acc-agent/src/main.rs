mod agent;
mod bus;
mod cli_sanity;
mod cli_tmux_adapter;
mod config;
mod exec_registry;
mod hermes;
#[cfg(test)]
mod hub_mock;
mod json;
mod log_init;
mod migrate;
mod peers;
mod proxy;
mod queue;
mod sdk;
mod services;
mod session_discovery;
mod session_registry;
mod slack_ingest;
mod slack_notify;
mod supervise;
mod tasks;
mod tmux;
mod upgrade;
mod worker;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_help();
        std::process::exit(1);
    }
    let sub = args[1].as_str();
    let rest = &args[2..];
    // Initialize tracing once per process. Long-running daemons (bus,
    // queue, tasks, hermes, supervise, proxy, upgrade) get journald +
    // stderr; short-lived subcommands (migrate, agent, json) get only
    // stderr to avoid journal noise.
    log_init::init(sub);
    match sub {
        "migrate" => migrate::run(rest),
        "agent" => agent::run(rest),
        "json" => json::run(rest),
        // listen is kept as alias for bus (backward compat)
        "listen" | "bus" => tokio_run(bus::run(rest)),
        "queue" => tokio_run(queue::run(rest)),
        "hermes" => tokio_run(hermes::run(rest)),
        "proxy" => tokio_run(proxy::run(rest)),
        "tasks" => tokio_run(tasks::run(rest)),
        "slack-ingest" => tokio_run(slack_ingest::run(rest)),
        "supervise" => tokio_run(supervise::run(rest)),
        "upgrade" => tokio_run(upgrade::run_cli(rest)),
        cmd => {
            eprintln!("Unknown command: {cmd}");
            std::process::exit(1);
        }
    }
}

fn tokio_run(fut: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(fut);
}

fn print_help() {
    eprintln!("acc-agent — ACC agent runtime CLI");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("  acc-agent migrate <subcommand>");
    eprintln!("  acc-agent agent   <subcommand>");
    eprintln!("  acc-agent json    <subcommand>");
    eprintln!("  acc-agent bus     (long-running daemon: AgentBus SSE listener)");
    eprintln!("  acc-agent queue   (long-running daemon: queue worker)");
    eprintln!("  acc-agent hermes  (hermes session driver)");
    eprintln!("  acc-agent proxy   (long-running daemon: NVIDIA header-strip proxy)");
    eprintln!("  acc-agent tasks     [--max=N]    (long-running daemon: fleet task worker)");
    eprintln!(
        "  acc-agent slack-ingest [--once]  (long-running daemon: Slack→Qdrant memory ingest)"
    );
    eprintln!("  acc-agent supervise [--dry-run]  (master supervisor: spawns all children)");
    eprintln!();
    eprintln!("MIGRATE:");
    eprintln!("  is-applied <name>            exit 0 if applied, 1 if not");
    eprintln!("  record <name> <ok|failed>    record a migration result");
    eprintln!("  list [migrations-dir]        print applied/pending table");
    eprintln!();
    eprintln!("AGENT:");
    eprintln!("  init <path> --name=X --host=X --version=X [--by=X]");
    eprintln!("  upgrade <path> --version=X");
    eprintln!();
    eprintln!("JSON (reads from stdin):");
    eprintln!("  get <path> [fallback]        print scalar at dotted path");
    eprintln!("  lines <path>                 print array elements one per line");
    eprintln!("  pairs <path>                 print object as key=value lines");
    eprintln!("  env-merge <path> <file>      merge flat strings into .env file");
    eprintln!();
    eprintln!("BUS / LISTEN:");
    eprintln!("  (no flags) — connects to ACC bus SSE stream, dispatches messages");
    eprintln!("  --test       test connection and exit");
    eprintln!();
    eprintln!("QUEUE:");
    eprintln!("  (no flags) — polls /api/queue and executes tasks");
    eprintln!("  --once       poll once and exit");
    eprintln!();
    eprintln!("HERMES:");
    eprintln!("  --task <id> --query <text>   run hermes for a fleet task");
    eprintln!("  --item <id> --query <text>   run hermes for a legacy queue item");
    eprintln!("  --poll                       poll /api/tasks continuously for hermes work");
    eprintln!("  --poll-queue                 legacy mode: poll /api/queue continuously");
    eprintln!();
    eprintln!("PROXY:");
    eprintln!("  --port <n>    listen port (default: 9099)");
    eprintln!("  --target <u>  upstream URL (default: NVIDIA_API_BASE env var)");
    eprintln!();
    eprintln!("UPGRADE:");
    eprintln!("  (no flags) — run pending migrations, restart services, post heartbeat");
    eprintln!("  --dry-run    show what would run without making changes");
}
