//! JSON Pointer (RFC 6901) — block addressing on stage A.

use crate::error::{ErrorCode, UmError};
use serde_json::Value;

/// Encode one reference token: `~` → `~0`, `/` → `~1` (RFC 6901 §4).
pub fn encode_token(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    for ch in token.chars() {
        match ch {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            _ => out.push(ch),
        }
    }
    out
}

/// Decode one reference token: `~0` → `~`, `~1` → `/`.
pub fn decode_token(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                Some(other) => {
                    out.push('~');
                    out.push(other);
                }
                None => out.push('~'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Split a pointer into decoded reference tokens. The empty string points at
/// the whole document; otherwise it MUST start with `/` and may not end with
/// an unescaped `/`-separated empty token.
pub fn split(pointer: &str) -> Result<Vec<String>, UmError> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    if !pointer.starts_with('/') {
        return Err(UmError::new(
            ErrorCode::PointerNotFound,
            format!("JSON Pointer must be empty or start with '/': {pointer:?}"),
        )
        .with_hint("Pointers look like /content/2/content/0 (RFC 6901)."));
    }
    Ok(pointer[1..].split('/').map(decode_token).collect())
}

/// Resolve a JSON Pointer against a document (RFC 6901 §4 evaluation).
pub fn resolve<'a>(doc: &'a Value, pointer: &str) -> Result<&'a Value, UmError> {
    let mut current = doc;
    for token in split(pointer)? {
        current = match current {
            Value::Object(map) => map.get(&token).ok_or_else(|| not_found(pointer, &token))?,
            Value::Array(items) => {
                let index: usize = token.parse().map_err(|_| not_found(pointer, &token))?;
                items.get(index).ok_or_else(|| not_found(pointer, &token))?
            }
            _ => return Err(not_found(pointer, &token)),
        };
    }
    Ok(current)
}

fn not_found(pointer: &str, token: &str) -> UmError {
    UmError::new(
        ErrorCode::PointerNotFound,
        format!("JSON Pointer {pointer:?} does not resolve (failed at token {token:?})"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn token_escaping_round_trips() {
        for token in ["a", "a/b", "a~b", "a/b~c", "", "~0~1"] {
            assert_eq!(decode_token(&encode_token(token)), token);
        }
        assert_eq!(encode_token("a/b"), "a~1b");
        assert_eq!(encode_token("a~b"), "a~0b");
    }

    #[test]
    fn resolve_walks_documents() {
        let doc = json!({
            "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "hi"}]},
                {"type": "a/b~c"}
            ]
        });
        assert_eq!(resolve(&doc, "").unwrap(), &doc);
        assert_eq!(resolve(&doc, "/content/0/type").unwrap(), "paragraph");
        assert_eq!(resolve(&doc, "/content/1/type").unwrap(), "a/b~c");
        assert_eq!(resolve(&doc, "/content/0/content/0/text").unwrap(), "hi");
        assert!(resolve(&doc, "/content/5").is_err());
        assert!(resolve(&doc, "content/0").is_err());
    }
}
