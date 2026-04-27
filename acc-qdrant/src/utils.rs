//! Shared utilities for text chunking and deterministic point ID generation.

use md5;

/// Split text into overlapping chunks at paragraph boundaries.
///
/// Mirrors the Python qdrant_common.chunk_text logic exactly.
pub fn chunk_text(text: &str, max_chars: usize, overlap: usize) -> Vec<String> {
    if text.trim().is_empty() {
        return vec![];
    }
    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for para in &paragraphs {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if !current.is_empty() && current.len() + para.len() + 2 > max_chars {
            chunks.push(current.trim().to_string());
            if overlap > 0 && current.len() > overlap {
                let start = current.len() - overlap;
                current = format!("{}\n\n{}", &current[start..], para);
            } else {
                current = para.to_string();
            }
        } else if current.is_empty() {
            current = para.to_string();
        } else {
            current = format!("{}\n\n{}", current, para);
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    if chunks.is_empty() {
        vec![text.trim().chars().take(max_chars).collect()]
    } else {
        chunks
    }
}

/// Generate a deterministic 63-bit unsigned integer ID from a namespace and parts.
///
/// Mirrors the Python `deterministic_point_id(namespace, *parts)`:
///   key = ":".join([namespace] + [str(p) for p in parts])
///   h = hashlib.md5(key.encode()).hexdigest()
///   return int(h[:16], 16) & 0x7FFFFFFFFFFFFFFF
pub fn deterministic_id(namespace: &str, parts: &[&str]) -> u64 {
    let mut key = namespace.to_string();
    for p in parts {
        key.push(':');
        key.push_str(p);
    }
    let digest = md5::compute(key.as_bytes());
    let hex = format!("{:x}", digest);
    let first16 = &hex[..16];
    u64::from_str_radix(first16, 16).unwrap_or(0) & 0x7FFFFFFFFFFFFFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_text_empty() {
        assert!(chunk_text("", 1500, 200).is_empty());
    }

    #[test]
    fn chunk_text_short_returns_single() {
        let chunks = chunk_text("hello world", 1500, 200);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello world");
    }

    #[test]
    fn deterministic_id_stable() {
        let id1 = deterministic_id("hermes_session", &["abc123", "0"]);
        let id2 = deterministic_id("hermes_session", &["abc123", "0"]);
        assert_eq!(id1, id2);
    }

    #[test]
    fn deterministic_id_different_inputs() {
        let id1 = deterministic_id("hermes_session", &["abc123", "0"]);
        let id2 = deterministic_id("hermes_session", &["abc123", "1"]);
        assert_ne!(id1, id2);
    }
}
