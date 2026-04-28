use crate::config::resolve_hostname;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AgentMeta {
    pub name: String,
    pub host: String,
    pub ccc_version: String,
    pub ccc_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_upgraded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_upgraded_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onboarded_at: Option<String>,
}

pub fn run(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("init") => cmd_init(args),
        Some("upgrade") => cmd_upgrade(args),
        _ => {
            eprintln!("Usage: acc-agent agent <init|upgrade> <path> [--flag=value ...]");
            std::process::exit(1);
        }
    }
}

fn cmd_init(args: &[String]) {
    let path = match args.get(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("Usage: acc-agent agent init <path> --name=X --host=X --version=X [--by=X]");
            std::process::exit(1);
        }
    };

    let flags = parse_flags(&args[2..]);
    let name = flags.get("name").cloned().unwrap_or_default();
    let host = flags.get("host").cloned().unwrap_or_else(resolve_hostname);
    let version = flags.get("version").cloned().unwrap_or_default();
    let by = flags.get("by").cloned();

    if path.exists() {
        eprintln!(
            "{} already exists — use 'upgrade' to update it",
            path.display()
        );
        std::process::exit(1);
    }

    let meta = AgentMeta {
        name,
        host,
        ccc_version: version,
        ccc_agent: env!("CARGO_PKG_VERSION").into(),
        onboarded_at: Some(chrono::Utc::now().to_rfc3339()),
        last_upgraded_by: by,
        ..Default::default()
    };

    write_meta(&path, &meta);
    eprintln!("Initialized {}", path.display());
}

fn cmd_upgrade(args: &[String]) {
    let path = match args.get(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("Usage: acc-agent agent upgrade <path> --version=X");
            std::process::exit(1);
        }
    };

    let flags = parse_flags(&args[2..]);
    let version = flags.get("version").cloned().unwrap_or_default();

    let mut meta: AgentMeta = if path.exists() {
        let s = std::fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str(&s).unwrap_or_default()
    } else {
        AgentMeta::default()
    };

    meta.ccc_version = version;
    meta.ccc_agent = env!("CARGO_PKG_VERSION").into();
    meta.last_upgraded_at = Some(chrono::Utc::now().to_rfc3339());

    write_meta(&path, &meta);
    eprintln!("Upgraded {}", path.display());
}

fn write_meta(path: &PathBuf, meta: &AgentMeta) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(meta).unwrap_or_default();
    std::fs::write(path, json).expect("failed to write agent.json");
}

fn parse_flags(args: &[String]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for arg in args {
        if let Some(kv) = arg.strip_prefix("--") {
            if let Some((k, v)) = kv.split_once('=') {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_flags() {
        let args = vec!["--name=boris".into(), "--host=h".into()];
        let flags = parse_flags(&args);
        assert_eq!(flags["name"], "boris");
        assert_eq!(flags["host"], "h");
    }

    #[test]
    fn test_init_upgrade_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agent.json");
        let meta = AgentMeta {
            name: "test".into(),
            host: "localhost".into(),
            ccc_version: "1.0.0".into(),
            ccc_agent: "0.1.0".into(),
            ..Default::default()
        };
        write_meta(&path, &meta);
        let raw = std::fs::read_to_string(&path).unwrap();
        let loaded: AgentMeta = serde_json::from_str(&raw).unwrap();
        assert_eq!(loaded.name, "test");
        assert_eq!(loaded.ccc_version, "1.0.0");
    }
}
