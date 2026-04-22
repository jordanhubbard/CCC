//! Structured command execution registry.
//!
//! Loads ~/.acc/commands.json (or workspace fallback) and dispatches
//! named commands with typed parameter validation. No shell interpreter
//! is involved — args are passed directly to the OS via Command::new.
//! This replaces raw `/bin/sh -c` execution with a finite, auditable
//! vocabulary of named operations, analogous to MCP tool_use.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, Clone)]
pub struct ParamDef {
    #[serde(rename = "type")]
    pub param_type: String,        // "string" | "integer" | "boolean" | "enum"
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub max_len: Option<usize>,
    pub pattern: Option<String>,   // prefix:<p> | suffix:<s> | contains:<c> | literal
    pub values: Option<Vec<String>>,  // for enum
    pub default: Option<Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CommandDef {
    pub name: String,
    #[allow(dead_code)]
    pub description: Option<String>,
    /// Executable path or name (looked up via PATH). No shell.
    pub program: String,
    /// Argument list. Supports {param} and {ACC_DIR} substitution.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub params: HashMap<String, ParamDef>,
    /// If set, only run on these platforms. "linux" | "macos" | absent = all.
    pub platforms: Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
    pub working_dir: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct CommandRegistry {
    #[serde(default)]
    pub commands: Vec<CommandDef>,
}

impl CommandRegistry {
    pub fn load(acc_dir: &PathBuf) -> Self {
        let paths = [
            acc_dir.join("commands.json"),
            acc_dir.join("workspace/deploy/commands.json"),
        ];
        for path in &paths {
            if let Ok(raw) = std::fs::read_to_string(path) {
                if let Ok(reg) = serde_json::from_str::<CommandRegistry>(&raw) {
                    return reg;
                }
            }
        }
        CommandRegistry::default()
    }

    pub fn find(&self, name: &str) -> Option<&CommandDef> {
        self.commands.iter().find(|c| c.name == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.commands.iter().map(|c| c.name.as_str()).collect()
    }
}

/// Execute a named command from the registry.
/// Returns `(stdout+stderr, exit_code)`.
pub async fn execute(
    cmd: &CommandDef,
    params: &Value,
    acc_dir: &PathBuf,
    timeout_secs: u64,
) -> (String, i32) {
    // Platform gate
    if let Some(platforms) = &cmd.platforms {
        let current = platform_name();
        if !platforms.iter().any(|p| p == current || p == "all") {
            return (
                format!("command '{}' not supported on {current}", cmd.name),
                1,
            );
        }
    }

    // Validate params and build resolved map
    let mut resolved: HashMap<String, String> = HashMap::new();
    for (pname, pdef) in &cmd.params {
        let raw_val = params.get(pname).or(pdef.default.as_ref());
        match validate_param(pname, pdef, raw_val) {
            Ok(s) => { resolved.insert(pname.clone(), s); }
            Err(e) => return (format!("param error: {e}"), 1),
        }
    }

    // Render {param} and {ACC_DIR} tokens in program and args
    let acc_dir_str = acc_dir.to_string_lossy().into_owned();
    let render = |s: &str| -> String {
        let mut out = s.replace("{ACC_DIR}", &acc_dir_str);
        for (k, v) in &resolved {
            out = out.replace(&format!("{{{k}}}"), v);
        }
        out
    };

    let program = render(&cmd.program);
    let rendered_args: Vec<String> = cmd.args.iter().map(|a| render(a)).collect();

    let mut command = tokio::process::Command::new(&program);
    command.args(&rendered_args);
    if let Some(wd) = &cmd.working_dir {
        command.current_dir(render(wd));
    }

    let timeout = Duration::from_secs(cmd.timeout_secs.unwrap_or(timeout_secs));
    let result = tokio::time::timeout(timeout, command.output()).await;

    match result {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            if !out.stderr.is_empty() {
                text.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            (text.trim_end().to_string(), out.status.code().unwrap_or(1))
        }
        Ok(Err(e)) => (format!("exec error launching '{program}': {e}"), 1),
        Err(_) => (format!("[timed out after {}s]", timeout.as_secs()), 124),
    }
}

fn validate_param(name: &str, def: &ParamDef, val: Option<&Value>) -> Result<String, String> {
    let val = val.ok_or_else(|| format!("required param '{name}' not provided"))?;

    match def.param_type.as_str() {
        "string" => {
            let s = val.as_str().ok_or_else(|| format!("'{name}' must be a string"))?;
            if let Some(max_len) = def.max_len {
                if s.len() > max_len {
                    return Err(format!("'{name}' length {} exceeds max {max_len}", s.len()));
                }
            }
            if let Some(pattern) = &def.pattern {
                if !pattern_match(s, pattern) {
                    return Err(format!("'{name}' does not match pattern '{pattern}'"));
                }
            }
            Ok(s.to_string())
        }
        "integer" => {
            let n = val.as_i64().ok_or_else(|| format!("'{name}' must be an integer"))?;
            if let Some(min) = def.min { if n < min { return Err(format!("'{name}' is {n}, min is {min}")); } }
            if let Some(max) = def.max { if n > max { return Err(format!("'{name}' is {n}, max is {max}")); } }
            Ok(n.to_string())
        }
        "boolean" => {
            let b = val.as_bool().ok_or_else(|| format!("'{name}' must be a boolean"))?;
            Ok(b.to_string())
        }
        "enum" => {
            let s = val.as_str().ok_or_else(|| format!("'{name}' must be a string"))?;
            let allowed = def.values.as_deref().unwrap_or(&[]);
            if !allowed.iter().any(|v| v == s) {
                return Err(format!("'{name}' must be one of: {}", allowed.join(", ")));
            }
            Ok(s.to_string())
        }
        t => Err(format!("unknown param type '{t}' for '{name}'")),
    }
}

/// Simple pattern matching that avoids adding the regex crate.
/// Pattern prefix: "prefix:<p>", "suffix:<s>", "contains:<c>", else literal equality.
fn pattern_match(s: &str, pattern: &str) -> bool {
    if let Some(p) = pattern.strip_prefix("prefix:") {
        s.starts_with(p)
    } else if let Some(p) = pattern.strip_prefix("suffix:") {
        s.ends_with(p)
    } else if let Some(p) = pattern.strip_prefix("contains:") {
        s.contains(p)
    } else {
        s == pattern
    }
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "linux") { "linux" } else { "macos" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn dummy_acc_dir() -> PathBuf {
        PathBuf::from("/tmp")
    }

    fn make_registry(cmds: Vec<CommandDef>) -> CommandRegistry {
        CommandRegistry { commands: cmds }
    }

    #[test]
    fn test_registry_find() {
        let reg = make_registry(vec![
            CommandDef {
                name: "ping".into(),
                description: None,
                program: "echo".into(),
                args: vec!["pong".into()],
                params: HashMap::new(),
                platforms: None,
                timeout_secs: None,
                working_dir: None,
            },
        ]);
        assert!(reg.find("ping").is_some());
        assert!(reg.find("missing").is_none());
    }

    #[test]
    fn test_validate_integer_bounds() {
        let def = ParamDef { param_type: "integer".into(), min: Some(1), max: Some(100), max_len: None, pattern: None, values: None, default: None };
        assert!(validate_param("n", &def, Some(&json!(50))).is_ok());
        assert!(validate_param("n", &def, Some(&json!(0))).is_err());
        assert!(validate_param("n", &def, Some(&json!(101))).is_err());
    }

    #[test]
    fn test_validate_enum() {
        let def = ParamDef { param_type: "enum".into(), min: None, max: None, max_len: None, pattern: None, values: Some(vec!["a".into(), "b".into()]), default: None };
        assert!(validate_param("x", &def, Some(&json!("a"))).is_ok());
        assert!(validate_param("x", &def, Some(&json!("c"))).is_err());
    }

    #[test]
    fn test_validate_string_pattern() {
        let def = ParamDef { param_type: "string".into(), min: None, max: None, max_len: None, pattern: Some("prefix:acc-".into()), values: None, default: None };
        assert!(validate_param("svc", &def, Some(&json!("acc-server"))).is_ok());
        assert!(validate_param("svc", &def, Some(&json!("other"))).is_err());
    }

    #[tokio::test]
    async fn test_execute_echo() {
        let cmd = CommandDef {
            name: "test".into(),
            description: None,
            program: "echo".into(),
            args: vec!["hello".into()],
            params: HashMap::new(),
            platforms: None,
            timeout_secs: Some(5),
            working_dir: None,
        };
        let (out, code) = execute(&cmd, &json!({}), &dummy_acc_dir(), 5).await;
        assert_eq!(code, 0);
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn test_execute_param_substitution() {
        let mut params = HashMap::new();
        params.insert("n".into(), ParamDef {
            param_type: "integer".into(),
            min: Some(1), max: Some(10),
            max_len: None, pattern: None, values: None,
            default: Some(json!(3)),
        });
        let cmd = CommandDef {
            name: "repeat".into(),
            description: None,
            program: "echo".into(),
            args: vec!["count={n}".into()],
            params,
            platforms: None,
            timeout_secs: Some(5),
            working_dir: None,
        };
        let (out, code) = execute(&cmd, &json!({"n": 7}), &dummy_acc_dir(), 5).await;
        assert_eq!(code, 0);
        assert_eq!(out, "count=7");
    }
}
