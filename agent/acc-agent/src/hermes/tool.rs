use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;

pub type ToolResult = Result<String, String>;

/// Object-safe tool trait. Uses boxed futures so Box<dyn Tool> works without async-trait.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    fn execute<'a>(&'a self, input: Value)
        -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Box<dyn Tool>>) -> Self {
        Self { tools }
    }

    pub fn default_tools() -> Self {
        Self::new(vec![
            Box::new(BashTool),
            Box::new(ReadFileTool),
            Box::new(WriteFileTool),
            Box::new(WebFetchTool::new()),
        ])
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }

    /// Serialize to Anthropic-format tool definitions.
    pub fn to_api_format(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name(),
                    "description": t.description(),
                    "input_schema": t.input_schema(),
                })
            })
            .collect()
    }
}

// ── Standard Tools ────────────────────────────────────────────────────────────

pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Execute a bash command and return stdout+stderr. Commands run in a subshell; \
         no state persists between calls. Avoid commands that require user interaction."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The bash command to run"},
                "timeout_secs": {"type": "integer", "description": "Max seconds (default 60, max 300)"}
            },
            "required": ["command"]
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let command = input["command"].as_str().unwrap_or("").to_string();
            if command.is_empty() {
                return Err("command is required".to_string());
            }
            let timeout = input["timeout_secs"].as_u64().unwrap_or(60).min(300);
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(timeout),
                tokio::process::Command::new("bash")
                    .arg("-c")
                    .arg(&command)
                    .output(),
            )
            .await;
            match result {
                Ok(Ok(out)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let combined = if stderr.is_empty() {
                        stdout.to_string()
                    } else if stdout.is_empty() {
                        stderr.to_string()
                    } else {
                        format!("{stdout}\n--- stderr ---\n{stderr}")
                    };
                    if out.status.success() {
                        Ok(combined)
                    } else {
                        Err(format!(
                            "exit {}: {combined}",
                            out.status.code().unwrap_or(-1)
                        ))
                    }
                }
                Ok(Err(e)) => Err(format!("exec error: {e}")),
                Err(_) => Err(format!("timed out after {timeout}s")),
            }
        })
    }
}

pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read the contents of a file."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute or relative file path"}
            },
            "required": ["path"]
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let path = input["path"].as_str().unwrap_or("").to_string();
            if path.is_empty() {
                return Err("path is required".to_string());
            }
            tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| format!("read error: {e}"))
        })
    }
}

pub struct WriteFileTool;

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write content to a file, creating parent directories if needed."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to write"},
                "content": {"type": "string", "description": "Content to write"}
            },
            "required": ["path", "content"]
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let path = input["path"].as_str().unwrap_or("").to_string();
            let content = input["content"].as_str().unwrap_or("").to_string();
            if path.is_empty() {
                return Err("path is required".to_string());
            }
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if parent != std::path::Path::new("") {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|e| format!("mkdir error: {e}"))?;
                }
            }
            tokio::fs::write(&path, content)
                .await
                .map_err(|e| format!("write error: {e}"))?;
            Ok(format!("written to {path}"))
        })
    }
}

pub struct WebFetchTool {
    client: reqwest::Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client for WebFetchTool");
        Self { client }
    }
}

impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn description(&self) -> &str {
        "Fetch a URL and return the response body as text."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL to fetch"},
                "method": {"type": "string", "description": "HTTP method: GET or POST (default: GET)"}
            },
            "required": ["url"]
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        let client = self.client.clone();
        Box::pin(async move {
            let url = input["url"].as_str().unwrap_or("").to_string();
            let method = input["method"].as_str().unwrap_or("GET").to_uppercase();
            if url.is_empty() {
                return Err("url is required".to_string());
            }
            let req = match method.as_str() {
                "GET" => client.get(&url),
                "POST" => client.post(&url),
                m => return Err(format!("unsupported method: {m}")),
            };
            let resp = req.send().await.map_err(|e| format!("fetch error: {e}"))?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| format!("read error: {e}"))?;
            if status.is_success() {
                Ok(text)
            } else {
                Err(format!("HTTP {status}: {}", &text[..text.len().min(500)]))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_registry_names() {
        let reg = ToolRegistry::default_tools();
        let names = reg.names();
        assert!(names.contains(&"bash".to_string()));
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"web_fetch".to_string()));
    }

    #[test]
    fn tool_registry_to_api_format() {
        let reg = ToolRegistry::default_tools();
        let api = reg.to_api_format();
        assert_eq!(api.len(), 4);
        for tool_def in &api {
            assert!(tool_def["name"].is_string());
            assert!(tool_def["description"].is_string());
            assert!(tool_def["input_schema"].is_object());
        }
    }

    #[test]
    fn tool_registry_get_unknown() {
        let reg = ToolRegistry::default_tools();
        assert!(reg.get("nonexistent").is_none());
    }

    #[tokio::test]
    async fn bash_tool_success() {
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}))
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn bash_tool_failure() {
        let tool = BashTool;
        let result = tool.execute(serde_json::json!({"command": "exit 1"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn bash_tool_no_command() {
        let tool = BashTool;
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_file_tool_missing() {
        let tool = ReadFileTool;
        let result = tool
            .execute(serde_json::json!({"path": "/nonexistent/path/file.txt"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn write_and_read_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let write_tool = WriteFileTool;
        let result = write_tool
            .execute(serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "hello world"
            }))
            .await;
        assert!(result.is_ok());

        let read_tool = ReadFileTool;
        let content = read_tool
            .execute(serde_json::json!({"path": path.to_str().unwrap()}))
            .await;
        assert_eq!(content.unwrap(), "hello world");
    }
}
