//! Platform-aware service registry and restart.
//!
//! Each service has a logical name, a Linux systemd unit, and a macOS launchd label.
//! Restart is asynchronous but waits for the command to exit (up to 30s timeout).
//! Returns Ok(()) on success, Err(String) on failure.

use std::time::Duration;

pub struct ServiceDef {
    pub name: &'static str,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub linux_unit: &'static str,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub macos_label: &'static str,
}

/// The compiled-in ACC service registry.
pub fn registry() -> &'static [ServiceDef] {
    static REGISTRY: &[ServiceDef] = &[
        ServiceDef {
            name: "acc-bus-listener",
            linux_unit: "acc-bus-listener.service",
            macos_label: "com.acc.bus-listener",
        },
        ServiceDef {
            name: "acc-queue-worker",
            linux_unit: "acc-queue-worker.service",
            macos_label: "com.acc.queue-worker",
        },
        ServiceDef {
            name: "acc-task-worker",
            linux_unit: "acc-task-worker.service",
            macos_label: "com.acc.task-worker",
        },
        ServiceDef {
            name: "acc-hermes-worker",
            linux_unit: "acc-hermes-worker.service",
            macos_label: "com.acc.hermes-worker",
        },
        ServiceDef {
            name: "acc-nvidia-proxy",
            linux_unit: "acc-nvidia-proxy.service",
            macos_label: "com.acc.nvidia-proxy",
        },
        ServiceDef {
            name: "acc-server",
            linux_unit: "acc-server.service",
            macos_label: "com.acc.server",
        },
    ];
    REGISTRY
}

/// Look up a service by logical name.
pub fn find(name: &str) -> Option<&'static ServiceDef> {
    registry().iter().find(|s| s.name == name)
}

/// Returns true for the self-service (acc-bus-listener IS the upgrade runner).
pub fn is_self(name: &str) -> bool {
    name == "acc-bus-listener"
}

/// Restart a service.  Synchronous (waits up to 30 s).
/// `log` receives human-readable progress lines.
pub async fn restart(def: &ServiceDef, log: impl Fn(&str)) -> Result<(), String> {
    let timeout = Duration::from_secs(30);

    #[cfg(target_os = "macos")]
    {
        // launchctl kickstart -k gui/<uid>/<label>
        // Get UID via `id -u` to avoid pulling in libc
        let uid_out = tokio::process::Command::new("id")
            .arg("-u")
            .output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "501".to_string());
        let uid = uid_out.trim();
        let target = format!("gui/{}/{}", uid, def.macos_label);
        log(&format!("restarting {} via launchctl kickstart -k {}", def.name, target));
        let result = tokio::time::timeout(
            timeout,
            tokio::process::Command::new("launchctl")
                .args(["kickstart", "-k", &target])
                .status(),
        )
        .await;
        return match result {
            Ok(Ok(s)) if s.success() => Ok(()),
            Ok(Ok(s)) => Err(format!("launchctl exited {s}")),
            Ok(Err(e)) => Err(format!("launchctl error: {e}")),
            Err(_) => Err("launchctl timed out after 30s".into()),
        };
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Linux: systemctl restart <unit>
        log(&format!("restarting {} via systemctl restart {}", def.name, def.linux_unit));
        let result = tokio::time::timeout(
            timeout,
            tokio::process::Command::new("systemctl")
                .args(["restart", def.linux_unit])
                .status(),
        )
        .await;
        match result {
            Ok(Ok(s)) if s.success() => Ok(()),
            Ok(Ok(s)) => Err(format!("systemctl exited {s}")),
            Ok(Err(e)) => Err(format!("systemctl error: {e}")),
            Err(_) => Err("systemctl timed out after 30s".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_has_all_services() {
        let names: Vec<&str> = registry().iter().map(|s| s.name).collect();
        assert!(names.contains(&"acc-bus-listener"));
        assert!(names.contains(&"acc-queue-worker"));
        assert!(names.contains(&"acc-task-worker"));
        assert!(names.contains(&"acc-hermes-worker"));
        assert!(names.contains(&"acc-nvidia-proxy"));
        assert!(names.contains(&"acc-server"));
    }

    #[test]
    fn test_find_known_service() {
        let def = find("acc-queue-worker").expect("must find acc-queue-worker");
        assert_eq!(def.linux_unit, "acc-queue-worker.service");
        assert_eq!(def.macos_label, "com.acc.queue-worker");
    }

    #[test]
    fn test_find_unknown_service() {
        assert!(find("acc-nonexistent").is_none());
    }

    #[test]
    fn test_is_self_bus_listener() {
        assert!(is_self("acc-bus-listener"));
        assert!(!is_self("acc-queue-worker"));
        assert!(!is_self("acc-server"));
    }
}
