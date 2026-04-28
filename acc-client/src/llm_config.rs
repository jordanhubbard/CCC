//! LLM provider configuration resolved from environment variables with
//! `~/.acc/.env` fallback.
//!
//! Precedence for every key: env var → `~/.acc/.env` → empty string.
//! Multiple candidate env var names are tried left-to-right; the first
//! non-empty value wins.

use crate::auth::{home_dir, parse_dotenv};

/// Resolved LLM provider configuration.
#[derive(Debug, Clone, Default)]
pub struct LlmConfig {
    /// OpenAI-compatible base URL.
    /// Env: `OPENAI_BASE_URL` → `LLM_URL` → `HERMES_BACKEND_URL`
    pub base_url: String,
    /// Bearer key for the OpenAI-compatible endpoint.
    /// Env: `OPENAI_API_KEY` → `LLM_KEY`
    pub api_key: String,
    /// Anthropic API key.
    /// Env: `ANTHROPIC_API_KEY`
    pub anthropic_key: String,
    /// Anthropic base URL override (empty → use default api.anthropic.com).
    /// Env: `ANTHROPIC_BASE_URL`
    pub anthropic_base_url: String,
    /// Preferred model name.
    /// Env: `OPENAI_MODEL` → `HERMES_MODEL`
    pub model: String,
    /// Embedding endpoint URL.
    /// Env: `NVIDIA_EMBED_URL` → `EMBED_URL`
    pub embed_url: String,
    /// API key for the embedding endpoint (falls back to api_key).
    /// Env: `NVIDIA_API_KEY` → `OPENAI_API_KEY` → `LLM_KEY`
    pub embed_key: String,
}

impl LlmConfig {
    /// Load config using the standard precedence: env var → `~/.acc/.env` → empty.
    pub fn load() -> Self {
        let text = acc_env_text();
        let embed_key_candidates = &["NVIDIA_API_KEY", "OPENAI_API_KEY", "LLM_KEY"];
        Self {
            base_url: resolve(&text, &["OPENAI_BASE_URL", "LLM_URL", "HERMES_BACKEND_URL"]),
            api_key: resolve(&text, &["OPENAI_API_KEY", "LLM_KEY"]),
            anthropic_key: resolve(&text, &["ANTHROPIC_API_KEY"]),
            anthropic_base_url: resolve(&text, &["ANTHROPIC_BASE_URL"]),
            model: resolve(&text, &["OPENAI_MODEL", "HERMES_MODEL"]),
            embed_url: resolve(&text, &["NVIDIA_EMBED_URL", "EMBED_URL"]),
            embed_key: resolve(&text, embed_key_candidates),
        }
    }

    /// `true` when an OpenAI-compatible endpoint is configured.
    pub fn is_openai_configured(&self) -> bool {
        !self.base_url.is_empty()
    }

    /// `true` when an Anthropic API key is available.
    pub fn is_anthropic_configured(&self) -> bool {
        !self.anthropic_key.is_empty()
    }

    /// Returns the Anthropic base URL, falling back to the public API.
    pub fn anthropic_base_url_or_default(&self) -> &str {
        if self.anthropic_base_url.is_empty() {
            "https://api.anthropic.com"
        } else {
            &self.anthropic_base_url
        }
    }
}

/// Read `~/.acc/.env` (or `~/.ccc/.env`) and return its raw text.
fn acc_env_text() -> String {
    let home = home_dir();
    for dir in [".acc", ".ccc"] {
        let path = home.join(dir).join(".env");
        if let Ok(text) = std::fs::read_to_string(&path) {
            return text;
        }
    }
    String::new()
}

/// Try each candidate key: env var first, then the dotenv file text.
fn resolve(dotenv_text: &str, keys: &[&str]) -> String {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    for key in keys {
        if let Some(v) = parse_dotenv(dotenv_text, key) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_env_over_file() {
        std::env::set_var("_TEST_LLM_KEY_A", "from-env");
        let text = "_TEST_LLM_KEY_A=from-file\n";
        assert_eq!(resolve(text, &["_TEST_LLM_KEY_A"]), "from-env");
        std::env::remove_var("_TEST_LLM_KEY_A");
    }

    #[test]
    fn resolve_falls_back_to_file() {
        std::env::remove_var("_TEST_LLM_KEY_B");
        let text = "_TEST_LLM_KEY_B=from-file\n";
        assert_eq!(resolve(text, &["_TEST_LLM_KEY_B"]), "from-file");
    }

    #[test]
    fn resolve_tries_candidates_in_order() {
        std::env::remove_var("_TEST_FIRST");
        std::env::set_var("_TEST_SECOND", "second-wins");
        let text = "";
        assert_eq!(
            resolve(text, &["_TEST_FIRST", "_TEST_SECOND"]),
            "second-wins"
        );
        std::env::remove_var("_TEST_SECOND");
    }
}
