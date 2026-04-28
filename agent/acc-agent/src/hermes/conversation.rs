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
        Self {
            messages: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_starts_empty() {
        let h = ConversationHistory::new();
        assert!(h.messages.is_empty());
        assert_eq!(h.input_tokens, 0);
        assert_eq!(h.output_tokens, 0);
        assert_eq!(h.total_tokens(), 0);
    }

    #[test]
    fn push_user_text_adds_user_role_with_text_block() {
        let mut h = ConversationHistory::new();
        h.push_user_text("hello world");
        assert_eq!(h.messages.len(), 1);
        let msg = &h.messages[0];
        assert_eq!(msg["role"], "user");
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "hello world");
    }

    #[test]
    fn push_assistant_content_adds_assistant_role() {
        let mut h = ConversationHistory::new();
        let blocks = vec![json!({"type": "text", "text": "I did it"})];
        h.push_assistant_content(blocks);
        assert_eq!(h.messages.len(), 1);
        assert_eq!(h.messages[0]["role"], "assistant");
        assert_eq!(h.messages[0]["content"][0]["text"], "I did it");
    }

    #[test]
    fn push_tool_results_adds_user_role() {
        let mut h = ConversationHistory::new();
        let results = vec![json!({"type": "tool_result", "tool_use_id": "x", "content": "ok"})];
        h.push_tool_results(results);
        assert_eq!(h.messages.len(), 1);
        assert_eq!(h.messages[0]["role"], "user");
        assert_eq!(h.messages[0]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn total_tokens_sums_input_and_output() {
        let mut h = ConversationHistory::new();
        h.input_tokens = 100;
        h.output_tokens = 42;
        assert_eq!(h.total_tokens(), 142);
    }

    #[test]
    fn from_turns_reconstructs_history_and_tokens() {
        let turns = vec![
            json!({"role": "user",      "content": [{"type":"text","text":"hi"}], "input_tokens": 10, "output_tokens": 0,  "stop_reason": "end_turn"}),
            json!({"role": "assistant", "content": [{"type":"text","text":"yo"}], "input_tokens": 15, "output_tokens": 20, "stop_reason": "end_turn"}),
        ];
        let h = ConversationHistory::from_turns(&turns);
        assert_eq!(h.messages.len(), 2);
        assert_eq!(h.messages[0]["role"], "user");
        assert_eq!(h.messages[1]["role"], "assistant");
        assert_eq!(h.input_tokens, 25);
        assert_eq!(h.output_tokens, 20);
        assert_eq!(h.total_tokens(), 45);
    }

    #[test]
    fn from_turns_handles_non_array_content() {
        let turns = vec![
            json!({"role": "user", "content": "plain string", "input_tokens": 5, "output_tokens": 0}),
        ];
        let h = ConversationHistory::from_turns(&turns);
        assert_eq!(h.messages.len(), 1);
        // Non-array content gets wrapped into a text block
        let content = h.messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
    }

    #[test]
    fn push_sequence_builds_correct_alternation() {
        let mut h = ConversationHistory::new();
        h.push_user_text("do something");
        h.push_assistant_content(vec![
            json!({"type":"tool_use","id":"t1","name":"bash","input":{"command":"echo hi"}}),
        ]);
        h.push_tool_results(vec![
            json!({"type":"tool_result","tool_use_id":"t1","content":"hi\n"}),
        ]);
        h.push_assistant_content(vec![json!({"type":"text","text":"done"})]);
        assert_eq!(h.messages.len(), 4);
        assert_eq!(h.messages[0]["role"], "user");
        assert_eq!(h.messages[1]["role"], "assistant");
        assert_eq!(h.messages[2]["role"], "user");
        assert_eq!(h.messages[3]["role"], "assistant");
    }
}
