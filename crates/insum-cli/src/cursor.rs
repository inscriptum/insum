//! Opaque pagination cursors (spec §6.2): base64url-encoded JSON bound to
//! the path, note, note revision, and the last returned position.

use serde_json::{Value, json};

use insum_core::UmError;

/// A decoded cursor. `note_index` orders notes within a multi-note response,
/// `block_index` is the next block to emit for that note.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    pub kind: String,
    pub path: String,
    pub note_id: Option<String>,
    /// Logical note revision the cursor was issued against.
    pub note_rev: String,
    pub note_index: usize,
    pub block_index: usize,
    /// Options the cursor was issued with; a resumed call must repeat them.
    pub depth: Option<usize>,
    pub text_limit: usize,
}

const KIND_TREE: &str = "tree";
const KIND_READ: &str = "read";

fn alphabet64() -> &'static [u8; 64] {
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
}

pub fn base64url_encode(data: &[u8]) -> String {
    let alphabet = alphabet64();
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(alphabet[(triple >> 18 & 63) as usize] as char);
        out.push(alphabet[(triple >> 12 & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(alphabet[(triple >> 6 & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(alphabet[(triple & 63) as usize] as char);
        }
    }
    out
}

pub fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let alphabet = alphabet64();
    let mut buffer = Vec::with_capacity(text.len() * 3 / 4);
    let bytes: Vec<usize> = text
        .bytes()
        .map(|b| alphabet.iter().position(|&a| a == b))
        .collect::<Option<_>>()?;
    for chunk in bytes.chunks(4) {
        match chunk.len() {
            4 => {
                let triple = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6) | chunk[3];
                buffer.push((triple >> 16) as u8);
                buffer.push((triple >> 8) as u8);
                buffer.push(triple as u8);
            }
            3 => {
                let triple = (chunk[0] << 18) | (chunk[1] << 12) | (chunk[2] << 6);
                buffer.push((triple >> 16) as u8);
                buffer.push((triple >> 8) as u8);
            }
            2 => {
                let triple = (chunk[0] << 18) | (chunk[1] << 12);
                buffer.push((triple >> 16) as u8);
            }
            _ => return None, // a single trailing character is never valid
        }
    }
    Some(buffer)
}

impl Cursor {
    pub fn tree(
        path: String,
        note_id: Option<String>,
        note_rev: String,
        note_index: usize,
        block_index: usize,
        depth: Option<usize>,
        text_limit: usize,
    ) -> Self {
        Self {
            kind: KIND_TREE.to_owned(),
            path,
            note_id,
            note_rev,
            note_index,
            block_index,
            depth,
            text_limit,
        }
    }

    pub fn read(
        path: String,
        note_id: Option<String>,
        note_rev: String,
        note_index: usize,
        block_index: usize,
        depth: Option<usize>,
        text_limit: usize,
    ) -> Self {
        Self {
            kind: KIND_READ.to_owned(),
            path,
            note_id,
            note_rev,
            note_index,
            block_index,
            depth,
            text_limit,
        }
    }

    pub fn encode(&self) -> String {
        let value = json!({
            "k": self.kind,
            "p": self.path,
            "n": self.note_id,
            "r": self.note_rev,
            "ni": self.note_index,
            "bi": self.block_index,
            "d": self.depth,
            "t": self.text_limit,
        });
        base64url_encode(value.to_string().as_bytes())
    }

    pub fn decode(token: &str) -> Result<Self, UmError> {
        let invalid = || {
            UmError::usage("cursor token is not a valid insum cursor")
                .with_hint("Use the next_cursor value from the previous response verbatim.")
        };
        let bytes = base64url_decode(token).ok_or_else(invalid)?;
        let text = String::from_utf8(bytes).map_err(|_| invalid())?;
        let value: Value = serde_json::from_str(&text).map_err(|_| invalid())?;
        Ok(Self {
            kind: value
                .get("k")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            path: value
                .get("p")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            note_id: value.get("n").and_then(Value::as_str).map(str::to_owned),
            note_rev: value
                .get("r")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            note_index: value.get("ni").and_then(Value::as_u64).unwrap_or(0) as usize,
            block_index: value.get("bi").and_then(Value::as_u64).unwrap_or(0) as usize,
            depth: value.get("d").and_then(Value::as_u64).map(|v| v as usize),
            text_limit: value.get("t").and_then(Value::as_u64).unwrap_or(0) as usize,
        })
    }

    /// A cursor only resumes the exact same query position (spec §6.2).
    pub fn validate(
        &self,
        kind: &str,
        path: &str,
        note_id: Option<&str>,
        note_rev: &str,
    ) -> Result<(), UmError> {
        if self.kind != kind {
            return Err(UmError::usage("cursor belongs to another command"));
        }
        if self.path != path {
            return Err(cursor_stale(format!(
                "cursor was issued for {:?}, not for {path:?}",
                self.path
            )));
        }
        if self.note_id.as_deref() != note_id {
            return Err(cursor_stale(
                "cursor was issued for another note".to_owned(),
            ));
        }
        if self.note_rev != note_rev {
            return Err(cursor_stale(
                "the note revision changed since the cursor was issued".to_owned(),
            ));
        }
        Ok(())
    }
}

fn cursor_stale(message: String) -> UmError {
    UmError::new(insum_core::ErrorCode::CursorStale, message)
        .with_hint("Re-run the command without --cursor and paginate from the start.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let cursor = Cursor::tree(
            "kb/agents.um".to_owned(),
            Some("note_01".to_owned()),
            "sha256:abcd".to_owned(),
            1,
            42,
            Some(3),
            120,
        );
        let token = cursor.encode();
        assert!(!token.contains('='));
        let decoded = Cursor::decode(&token).unwrap();
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Cursor::decode("not-a-cursor!").is_err());
        assert!(Cursor::decode("").is_err());
    }
}
