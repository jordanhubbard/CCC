//! Token resolution matching `acc-cli`'s precedence.

use crate::{Error, Result};
use std::path::PathBuf;

/// Resolve a bearer token.
///
/// Precedence (highest first):
/// 1. Explicit argument (passed to `flag`)
/// 2. `ACC_TOKEN` environment variable
/// 3. `~/.acc/.env` keys `ACC_TOKEN` then `ACC_AGENT_TOKEN`
pub fn resolve_token(flag: Option<String>) -> Result<String> {
    if let Some(t) = flag.filter(|s| !s.is_empty()) {
        return Ok(t);
    }
    if let Ok(t) = std::env::var("ACC_TOKEN") {
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let env_path = home_dir().join(".acc").join(".env");
    if env_path.exists() {
        let text = std::fs::read_to_string(&env_path)?;
        for key in ["ACC_TOKEN", "ACC_AGENT_TOKEN"] {
            if let Some(val) = parse_dotenv(&text, key) {
                return Ok(val);
            }
        }
    }
    Err(Error::NoToken)
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Minimal `.env` parser: `KEY=value` lines, `#` comments, optional quotes.
pub(crate) fn parse_dotenv(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line.split_once('=')?;
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        let stripped = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(v);
        return Some(stripped.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_beats_env_and_file() {
        let t = resolve_token(Some("from-flag".into())).unwrap();
        assert_eq!(t, "from-flag");
    }

    #[test]
    fn empty_flag_falls_through() {
        // Empty flag is treated as absent; the test env has no ACC_TOKEN
        // and no ~/.acc/.env, so we expect NoToken.
        // Use a scratch HOME so stray developer state doesn't influence this.
        let tmp = std::env::temp_dir().join(format!("acc-client-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let prev_home = std::env::var_os("HOME");
        let prev_token = std::env::var_os("ACC_TOKEN");
        std::env::set_var("HOME", &tmp);
        std::env::remove_var("ACC_TOKEN");

        let got = resolve_token(Some(String::new()));
        assert!(matches!(got, Err(Error::NoToken)));

        // Restore
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        if let Some(v) = prev_token {
            std::env::set_var("ACC_TOKEN", v);
        }
    }

    #[test]
    fn parse_dotenv_handles_quotes_and_comments() {
        let text = "# a comment\nACC_TOKEN=\"abc123\"\nACC_AGENT_TOKEN=plain\nOTHER=ignore";
        assert_eq!(parse_dotenv(text, "ACC_TOKEN").as_deref(), Some("abc123"));
        assert_eq!(
            parse_dotenv(text, "ACC_AGENT_TOKEN").as_deref(),
            Some("plain")
        );
        assert!(parse_dotenv(text, "MISSING").is_none());
    }
}
