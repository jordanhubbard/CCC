use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub acc_dir: PathBuf,
    pub acc_url: String,
    pub acc_token: String,
    pub agent_name: String,
    pub agentbus_token: String,
    pub pair_programming: bool,
    /// Fully-qualified hostname for this agent (self-healing registry updates).
    pub host: String,
    /// SSH connection details — authoritative source for how to reach this agent.
    /// Set via AGENT_SSH_USER / AGENT_SSH_HOST / AGENT_SSH_PORT in ~/.acc/.env.
    pub ssh_user: String,
    pub ssh_host: String,
    pub ssh_port: u16,
}

/// Returns the FQDN for this machine.
/// On macOS, bare hostnames (no dot) are served via mDNS as `<name>.local`.
pub fn resolve_hostname() -> String {
    // `hostname -f` gives FQDN on Linux; on macOS it usually echoes short name
    let fqdn_attempt = std::process::Command::new("hostname")
        .arg("-f")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    if !fqdn_attempt.is_empty() && fqdn_attempt.contains('.') {
        return fqdn_attempt;
    }

    let short = std::process::Command::new("hostname")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".into());

    // macOS: bare hostname is resolvable via Bonjour mDNS as <name>.local
    #[cfg(target_os = "macos")]
    if !short.is_empty() && !short.contains('.') {
        return format!("{}.local", short);
    }

    if short.is_empty() {
        "unknown".into()
    } else {
        short
    }
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home".into()));
        let acc_dir = if home.join(".acc").exists() {
            home.join(".acc")
        } else {
            home.join(".ccc")
        };

        load_env_file(&acc_dir.join(".env"));

        let acc_url = std::env::var("ACC_URL")
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string();

        let acc_token = std::env::var("ACC_AGENT_TOKEN").unwrap_or_default();

        let agent_name = std::env::var("AGENT_NAME").unwrap_or_default();

        let agentbus_token = std::env::var("AGENTBUS_TOKEN")
            .or_else(|_| std::env::var("SQUIRRELBUS_TOKEN"))
            .unwrap_or_default();

        let pair_programming = std::env::var("ACC_PAIR_PROGRAMMING")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);

        let host = resolve_hostname();

        let ssh_user = std::env::var("AGENT_SSH_USER")
            .unwrap_or_else(|_| std::env::var("USER").unwrap_or_else(|_| "unknown".into()));
        let ssh_host = std::env::var("AGENT_SSH_HOST").unwrap_or_else(|_| host.clone());
        let ssh_port = std::env::var("AGENT_SSH_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(22);

        if acc_url.is_empty() {
            return Err("ACC_URL not set in environment or ~/.acc/.env".into());
        }

        Ok(Config {
            acc_dir,
            acc_url,
            acc_token,
            agent_name,
            agentbus_token,
            pair_programming,
            host,
            ssh_user,
            ssh_host,
            ssh_port,
        })
    }

    pub fn log_file(&self, name: &str) -> PathBuf {
        self.acc_dir.join("logs").join(format!("{name}.log"))
    }

    pub fn quench_file(&self) -> PathBuf {
        self.acc_dir.join("quench")
    }

    pub fn session_data_dir(&self) -> PathBuf {
        self.acc_dir.join("data")
    }

    pub fn session_registry_file(&self) -> PathBuf {
        self.session_data_dir().join("coding-sessions.json")
    }

    pub fn max_cli_sessions(&self) -> u32 {
        std::env::var("ACC_MAX_CLI_SESSIONS")
            .or_else(|_| std::env::var("AGENT_MAX_CLI_SESSIONS"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4)
    }

    pub fn session_busy_window_secs(&self) -> i64 {
        std::env::var("ACC_SESSION_BUSY_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120)
    }

    pub fn session_stuck_threshold_secs(&self) -> i64 {
        std::env::var("ACC_SESSION_STUCK_THRESHOLD_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15 * 60)
    }

    pub fn session_min_free_memory_mb(&self) -> u64 {
        std::env::var("ACC_SESSION_MIN_FREE_MEMORY_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2048)
    }
}

pub fn load_env_file(path: &PathBuf) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'');
            if std::env::var(key).is_err() {
                // SAFETY: single-threaded init, no concurrent access
                unsafe { std::env::set_var(key, val) };
            }
        }
    }
}

pub fn acc_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home".into()));
    if home.join(".acc").exists() {
        home.join(".acc")
    } else {
        home.join(".ccc")
    }
}
