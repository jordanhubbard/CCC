//! Native Anthropic agentic loop — no external binary required.
//!
//! Replaces `claude -p <prompt> --dangerously-skip-permissions`.
//! Calls /v1/messages via reqwest, loops on tool_use until end_turn,
//! implementing bash and str_replace_editor tool execution in-process.

use std::path::{Path, PathBuf};
use std::time::Duration;
use serde_json::{json, Value};

const MAX_TURNS: u32 = 60;
const BASH_TIMEOUT_DEFAULT: u64 = 60;
const MODEL_DEFAULT: &str = "claude-sonnet-4-6";
const API_CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Run an agentic loop for `prompt` inside `workspace`.
/// Uses ANTHROPIC_BASE_URL, ANTHROPIC_API_KEY, CLAUDE_CODE_DEFAULT_MODEL from env.
/// Returns the model's final text output.
pub async fn run_agent(prompt: &str, workspace: &Path) -> Result<String, String> {
    // sdk talks to the Anthropic API, not to ACC — it owns its own HTTP
    // client. Callers don't need to supply one.
    let client = reqwest::Client::builder()
        .timeout(API_CALL_TIMEOUT)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let api_base = std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
    let api_base = api_base.trim_end_matches('/').to_string();

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .or_else(|_| std::env::var("NVIDIA_API_KEY"))
        .unwrap_or_default();

    let model = std::env::var("CLAUDE_CODE_DEFAULT_MODEL")
        .unwrap_or_else(|_| MODEL_DEFAULT.to_string());

    let ws_str = workspace.display().to_string();
    let system = format!(
        "You are a coding agent executing a task. Your workspace is: {ws_str}\n\
         \n\
         You have two tools: `bash` for shell commands and `str_replace_editor` \
         for file operations (view/create/str_replace/insert). Paths may be \
         absolute or workspace-relative.\n\
         \n\
         RULES:\n\
         1. The task is verified by `git diff` against your edits in the \
            workspace, NOT by what you write in the response. A response \
            that only describes what you would do — without actually \
            calling `str_replace_editor` or `bash` to apply the changes \
            — is a failed task.\n\
         2. If the request is unclear, use `str_replace_editor view` or \
            `bash` to inspect existing files first, then make the changes.\n\
         3. Do not run `git commit` or `git push` — version control is \
            handled separately by the milestone-commit task.\n\
         4. After the edits are applied, briefly summarize what you \
            changed in your final text response."
    );

    let tools = tool_schemas();
    let mut messages: Vec<Value> = vec![json!({"role": "user", "content": prompt})];
    let mut final_text = String::new();
    // Track tool calls across the whole run. If a coding-agent run ends
    // without ANY tool use, the model has summarized instead of edited
    // (the dishonest-completion pattern). We give it one chance to fix.
    let mut total_tool_calls: u32 = 0;
    let mut nudged_for_no_tools = false;

    for turn in 0..MAX_TURNS {
        let body = json!({
            "model": model,
            "max_tokens": 8096,
            "system": system,
            "tools": tools,
            "messages": messages,
        });

        let resp = client
            .post(format!("{api_base}/v1/messages"))
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .timeout(API_CALL_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("API request failed: {e}"))?;

        if resp.status() == 429 {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60);
            let wait = retry_after.min(120);
            eprintln!("[sdk] 429 rate-limited — retrying in {wait}s");
            tokio::time::sleep(Duration::from_secs(wait)).await;
            continue;
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("API error {status}: {}", &text[..text.len().min(400)]));
        }

        let response: Value = resp.json().await
            .map_err(|e| format!("response parse error: {e}"))?;

        let stop_reason = response["stop_reason"].as_str().unwrap_or("").to_string();
        let content_arr = match response["content"].as_array() {
            Some(a) => a.clone(),
            None => return Err("response missing content array".into()),
        };

        let mut tool_uses: Vec<Value> = Vec::new();
        for block in &content_arr {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"].as_str() {
                        final_text = t.to_string();
                    }
                }
                Some("tool_use") => tool_uses.push(block.clone()),
                _ => {}
            }
        }
        total_tool_calls += tool_uses.len() as u32;

        messages.push(json!({"role": "assistant", "content": content_arr}));

        if stop_reason == "end_turn" || tool_uses.is_empty() {
            // No-tool-use guard: if the entire run ended without a single
            // tool call, the model summarized instead of editing. Give it
            // one chance to make this right before accepting completion.
            if total_tool_calls == 0 && !nudged_for_no_tools {
                eprintln!(
                    "[sdk] turn {turn}: model finished without using any tools — \
                     re-prompting once for actual edits"
                );
                messages.push(json!({
                    "role": "user",
                    "content": "You produced a description but did not apply the \
                                changes. Use `str_replace_editor` or `bash` now \
                                to actually make the edits in the workspace. The \
                                task is verified by `git diff`, not by what you \
                                write in this conversation."
                }));
                nudged_for_no_tools = true;
                continue;
            }
            break;
        }

        // max_tokens mid-response: continue without tool results
        if stop_reason == "max_tokens" && turn + 1 < MAX_TURNS {
            messages.push(json!({"role": "user", "content": "Continue."}));
            continue;
        }

        // Execute tool calls and collect results
        let mut results: Vec<Value> = Vec::new();
        for tool_use in &tool_uses {
            let id = tool_use["id"].as_str().unwrap_or("").to_string();
            let name = tool_use["name"].as_str().unwrap_or("");
            let input = &tool_use["input"];

            let (content, is_error) = match dispatch_tool(name, input, workspace).await {
                Ok(out) => (out, false),
                Err(e)  => (e,   true),
            };

            results.push(json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": content,
                "is_error": is_error,
            }));
        }

        messages.push(json!({"role": "user", "content": results}));
    }

    Ok(final_text)
}

// ── Tool dispatch ─────────────────────────────────────────────────────────────

async fn dispatch_tool(name: &str, input: &Value, workspace: &Path) -> Result<String, String> {
    match name {
        "bash"              => run_bash(input, workspace).await,
        "str_replace_editor" => run_editor(input, workspace),
        _                  => Err(format!("unknown tool '{name}'")),
    }
}

async fn run_bash(input: &Value, workspace: &Path) -> Result<String, String> {
    let command = input["command"].as_str().unwrap_or("true");
    let timeout_secs = input["timeout"].as_u64().unwrap_or(BASH_TIMEOUT_DEFAULT);

    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        tokio::process::Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(workspace)
            .output(),
    )
    .await
    .map_err(|_| format!("[timed out after {timeout_secs}s]"))?
    .map_err(|e| format!("[failed to start bash: {e}]"))?;

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    let code = result.status.code().unwrap_or(-1);

    let mut out = String::new();
    if !stdout.is_empty() { out.push_str(&stdout); }
    if !stderr.is_empty() {
        if !out.is_empty() { out.push('\n'); }
        out.push_str(&stderr);
    }
    if out.is_empty() { out.push_str("(no output)"); }
    if code != 0 { out.push_str(&format!("\n[exit {code}]")); }
    Ok(out)
}

fn run_editor(input: &Value, workspace: &Path) -> Result<String, String> {
    let command = input["command"].as_str().unwrap_or("");
    let rel_path = input["path"].as_str().unwrap_or("");

    let abs: PathBuf = if Path::new(rel_path).is_absolute() {
        PathBuf::from(rel_path)
    } else {
        workspace.join(rel_path)
    };

    match command {
        "view" => {
            let content = std::fs::read_to_string(&abs)
                .map_err(|e| format!("cannot read '{rel_path}': {e}"))?;
            if let Some(range) = input["view_range"].as_array() {
                let start = range.first().and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                let end   = range.get(1).and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as usize;
                let lines: Vec<&str> = content.lines().collect();
                let s = start.saturating_sub(1);
                let e = end.min(lines.len());
                Ok(lines.get(s..e).unwrap_or(&[]).join("\n"))
            } else {
                Ok(content)
            }
        }
        "create" => {
            let text = input["file_text"].as_str().unwrap_or("");
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir failed: {e}"))?;
            }
            std::fs::write(&abs, text)
                .map_err(|e| format!("write failed: {e}"))?;
            Ok(format!("Created '{rel_path}'"))
        }
        "str_replace" => {
            let old = input["old_str"].as_str().unwrap_or("");
            let new = input["new_str"].as_str().unwrap_or("");
            let content = std::fs::read_to_string(&abs)
                .map_err(|e| format!("cannot read '{rel_path}': {e}"))?;
            let count = content.matches(old).count();
            match count {
                0 => Err(format!("str not found in '{rel_path}'")),
                1 => {
                    std::fs::write(&abs, content.replacen(old, new, 1))
                        .map_err(|e| format!("write failed: {e}"))?;
                    Ok(format!("Replaced in '{rel_path}'"))
                }
                n => Err(format!("{n} matches in '{rel_path}' — make old_str more specific")),
            }
        }
        "insert" => {
            let line_num = input["insert_line"].as_u64().unwrap_or(0) as usize;
            let new_str  = input["new_str_insert"].as_str().unwrap_or("");
            let content  = std::fs::read_to_string(&abs)
                .map_err(|e| format!("cannot read '{rel_path}': {e}"))?;
            let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
            let at = line_num.min(lines.len());
            lines.insert(at, new_str.to_string());
            std::fs::write(&abs, lines.join("\n"))
                .map_err(|e| format!("write failed: {e}"))?;
            Ok(format!("Inserted at line {at} in '{rel_path}'"))
        }
        "undo_edit" => Ok("undo_edit not supported; use str_replace to correct the file".into()),
        _ => Err(format!("unknown editor command '{command}'")),
    }
}

// ── Tool schemas ──────────────────────────────────────────────────────────────

fn tool_schemas() -> Value {
    json!([
        {
            "name": "bash",
            "description": "Execute a bash command in the task workspace. Returns combined stdout+stderr.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to run."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default 60)."
                    }
                },
                "required": ["command"]
            }
        },
        {
            "name": "str_replace_editor",
            "description": "View or edit files in the workspace. \
                            Use 'view' to read, 'create' to write a new file, \
                            'str_replace' to replace a unique substring, \
                            'insert' to insert text at a line number.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "enum": ["view", "create", "str_replace", "insert", "undo_edit"]
                    },
                    "path": {
                        "type": "string",
                        "description": "Absolute or workspace-relative file path."
                    },
                    "file_text": {
                        "type": "string",
                        "description": "Full content for the 'create' command."
                    },
                    "old_str": {
                        "type": "string",
                        "description": "Exact unique string to replace (str_replace)."
                    },
                    "new_str": {
                        "type": "string",
                        "description": "Replacement string (str_replace)."
                    },
                    "insert_line": {
                        "type": "integer",
                        "description": "0-based line index to insert after (insert)."
                    },
                    "new_str_insert": {
                        "type": "string",
                        "description": "Text to insert (insert)."
                    },
                    "view_range": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "description": "Optional [start_line, end_line] for view."
                    }
                },
                "required": ["command", "path"]
            }
        }
    ])
}
