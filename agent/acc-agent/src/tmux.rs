use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    pub session_name: String,
    pub pane_id: String,
    pub pane_pid: Option<u32>,
    pub current_command: String,
    pub current_path: String,
    pub pane_title: String,
    pub active: bool,
    pub window_active: bool,
    pub dead: bool,
    pub start_command: String,
    pub activity_epoch: Option<i64>,
}

const LIST_PANES_FORMAT: &str = "#{session_name}\t#{pane_id}\t#{pane_pid}\t#{pane_current_command}\t#{pane_current_path}\t#{pane_title}\t#{pane_active}\t#{window_active}\t#{pane_dead}\t#{pane_start_command}\t#{pane_activity}";

pub async fn list_panes() -> Result<Vec<PaneInfo>, String> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", LIST_PANES_FORMAT])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().filter_map(parse_pane_line).collect())
}

pub async fn new_session(
    session_name: &str,
    cwd: Option<&std::path::Path>,
    command: &str,
) -> Result<(), String> {
    let mut cmd = Command::new("tmux");
    cmd.args(["new-session", "-d", "-s", session_name]);
    if let Some(dir) = cwd {
        cmd.args(["-c", &dir.display().to_string()]);
    }
    cmd.arg(command);
    let output = cmd.output().await.map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub async fn kill_session(session_name: &str) -> Result<(), String> {
    let output = Command::new("tmux")
        .args(["kill-session", "-t", session_name])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub async fn capture_pane(pane_id: &str, lines: usize) -> Result<String, String> {
    let start = format!("-{}", lines.max(1));
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", pane_id, "-S", &start])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub async fn set_buffer(name: &str, text: &str) -> Result<(), String> {
    let output = Command::new("tmux")
        .args(["set-buffer", "-b", name, text])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub async fn paste_buffer(target: &str, buffer_name: &str) -> Result<(), String> {
    let output = Command::new("tmux")
        .args(["paste-buffer", "-b", buffer_name, "-t", target, "-d"])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[allow(dead_code)]
pub async fn send_keys(target: &str, text: &str, press_enter: bool) -> Result<(), String> {
    let mut args = vec!["send-keys", "-t", target, text];
    if press_enter {
        args.push("Enter");
    }
    let status = Command::new("tmux")
        .args(args)
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux send-keys exited with {status}"))
    }
}

#[allow(dead_code)]
pub async fn wait_for_quiet(
    pane_id: &str,
    quiet_for_secs: u64,
    timeout_secs: u64,
) -> Result<bool, String> {
    let start = std::time::Instant::now();
    loop {
        let panes = list_panes().await?;
        let Some(pane) = panes.into_iter().find(|p| p.pane_id == pane_id) else {
            return Ok(false);
        };
        if pane.dead {
            return Ok(true);
        }
        if let Some(activity_epoch) = pane.activity_epoch {
            let age = chrono::Utc::now().timestamp() - activity_epoch;
            if age >= quiet_for_secs as i64 {
                return Ok(true);
            }
        }
        if start.elapsed().as_secs() >= timeout_secs {
            return Ok(false);
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

fn parse_pane_line(line: &str) -> Option<PaneInfo> {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() != 11 {
        return None;
    }
    Some(PaneInfo {
        session_name: parts[0].to_string(),
        pane_id: parts[1].to_string(),
        pane_pid: parts[2].parse().ok(),
        current_command: parts[3].to_string(),
        current_path: parts[4].to_string(),
        pane_title: parts[5].to_string(),
        active: parts[6] == "1",
        window_active: parts[7] == "1",
        dead: parts[8] == "1",
        start_command: parts[9].to_string(),
        activity_epoch: parts[10].parse().ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pane_line_extracts_expected_fields() {
        let line = "claude:proj\t%1\t1234\tclaude\t/tmp/work\tclaude\t1\t1\t0\tclaude --dangerously-skip-permissions\t1714240000";
        let pane = parse_pane_line(line).unwrap();
        assert_eq!(pane.session_name, "claude:proj");
        assert_eq!(pane.pane_id, "%1");
        assert_eq!(pane.pane_pid, Some(1234));
        assert_eq!(pane.current_command, "claude");
        assert!(pane.active);
        assert!(!pane.dead);
        assert_eq!(pane.activity_epoch, Some(1714240000));
    }
}
