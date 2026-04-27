use serde_json::{json, Value};

/// In-memory conversation history for the Anthropic messages API.
/// Messages are stored as raw JSON to match the API format directly.
pub struct ConversationHistory {
    pub messages: Vec<Value>,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl ConversationHistory {
    pub fn new() -> Self {
        Self { messages: Vec::new(), input_tokens: 0, output_tokens: 0 }
    }

    pub fn push_user_text(&mut self, text: &str) {
        self.messages.push(json!({
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }));
    }

    pub fn push_assistant_content(&mut self, content: Vec<Value>) {
        self.messages.push(json!({
            "role": "assistant",
            "content": content
        }));
    }

    pub fn push_tool_results(&mut self, results: Vec<Value>) {
        self.messages.push(json!({
            "role": "user",
            "content": results
        }));
    }

    pub fn from_turns(turns: &[serde_json::Value]) -> Self {
        let mut history = Self::new();
        for turn in turns {
            let role = turn["role"].as_str().unwrap_or("user");
            let content = turn["content"].clone();
            let content_arr = if content.is_array() {
                content.as_array().cloned().unwrap_or_default()
            } else {
                vec![serde_json::json!({"type": "text", "text": content.to_string()})]
            };
            history.messages.push(serde_json::json!({
                "role": role,
                "content": content_arr
            }));
            history.input_tokens += turn["input_tokens"].as_u64().unwrap_or(0) as u32;
            history.output_tokens += turn["output_tokens"].as_u64().unwrap_or(0) as u32;
        }
        history
    }

    pub fn total_tokens(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }
}
