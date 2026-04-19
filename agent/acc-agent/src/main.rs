mod agent;
mod bus;
#[cfg(test)]
mod hub_mock;
mod config;
mod exec_registry;
mod hermes;
mod json;
mod migrate;
mod proxy;
mod queue;
mod tasks;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_help();
        std::process::exit(1);
    }
    let sub = args[1].as_str();
    let rest = &args[2..];
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
    eprintln!("  acc-agent tasks  [--max=N]  (long-running daemon: fleet task worker)");
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
    eprintln!("  --item <id> --query <text>   run hermes for a queue item");
    eprintln!("  --resume <session-id>         resume existing session");
    eprintln!("  --poll                        poll queue continuously for hermes tasks");
    eprintln!();
    eprintln!("PROXY:");
    eprintln!("  --port <n>    listen port (default: 9099)");
    eprintln!("  --target <u>  upstream URL (default: NVIDIA_API_BASE env var)");
}
