//! Canonical JSON (JCS, RFC 8785) — needed to hash note descriptors for
//! `note_rev` (spec §5.4). A JCS-lite implementation: object keys sorted,
//! no whitespace, minimal string escaping. Numbers go through serde_json's
//! default formatting (integers and typical manifest values are exact); the
//! hash contract is owned by this CLI.

use serde_json::Value;

/// Serialize a value canonically: keys sorted lexicographically by their
/// UTF-16 code units (RFC 8785 §3.2.3), no insignificant whitespace.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(string) => write_string(string, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            // RFC 8785 sorts by UTF-16 code units; for the BMP subset used in
            // manifests this coincides with byte order of UTF-8 only for
            // ASCII, so sort explicitly by UTF-16 sequences.
            keys.sort_by_key(|key| key.encode_utf16().collect::<Vec<u16>>());
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(&map[key.as_str()], out);
            }
            out.push('}');
        }
    }
}

fn write_string(string: &str, out: &mut String) {
    out.push('"');
    for ch in string.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys_and_removes_whitespace() {
        let value = json!({"b": 1, "a": {"d": [true, null], "c": "x"}});
        assert_eq!(
            canonical_json(&value),
            r#"{"a":{"c":"x","d":[true,null]},"b":1}"#
        );
    }

    #[test]
    fn escapes_specials() {
        let value = json!("a\"b\\c\nd\u{1}");
        assert_eq!(canonical_json(&value), r#""a\"b\\c\nd\u0001""#);
    }

    #[test]
    fn determinism() {
        let a = json!({"z": 1, "y": 2, "x": {"k": [1, 2, 3]}});
        let b = json!({"x": {"k": [1, 2, 3]}, "y": 2, "z": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }
}
