mod agent;
mod migrate;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("ccc-agent — CCC agent runtime CLI");
        eprintln!();
        eprintln!("USAGE:");
        eprintln!("  ccc-agent migrate <subcommand>");
        eprintln!("  ccc-agent agent   <subcommand>");
        eprintln!();
        eprintln!("MIGRATE:");
        eprintln!("  is-applied <name>          exit 0 if applied, 1 if not");
        eprintln!("  record <name> <ok|failed>  record a migration result");
        eprintln!("  list <migrations-dir>      print applied/pending table");
        eprintln!();
        eprintln!("AGENT:");
        eprintln!("  init <path> --name=X --host=X --version=X [--by=X]");
        eprintln!("                             write agent.json on first onboard");
        eprintln!("  upgrade <path> --version=X update ccc_version + last_upgraded_*");
        std::process::exit(1);
    }

    let sub_args = &args[2..];
    match args[1].as_str() {
        "migrate" => migrate::run(sub_args),
        "agent"   => agent::run(sub_args),
        cmd => {
            eprintln!("Unknown command: {cmd}");
            std::process::exit(1);
        }
    }
}
