use serde::{Deserialize, Serialize};

/// Cursor-based pagination parameters (spec section 3.1).
///
/// Cursors are opaque base64-encoded strings encoding the last seen item's
/// sort key. The first page is requested with no cursor.
#[derive(Debug, Default, Deserialize)]
pub struct PageParams {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    pub fn empty() -> Self {
        Self { items: vec![], next_cursor: None }
    }
}

/// Decode an opaque cursor into a raw string payload.
pub fn decode_cursor(cursor: &str) -> Option<String> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// Encode a raw string payload into an opaque cursor.
pub fn encode_cursor(payload: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip() {
        let encoded = encode_cursor("2026-10-22T12:00:00Z|some-id");
        assert_eq!(decode_cursor(&encoded).as_deref(), Some("2026-10-22T12:00:00Z|some-id"));
        assert!(decode_cursor("!!!invalid!!!").is_none());
    }
}