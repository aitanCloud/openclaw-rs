use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Query parameters for cursor-based pagination.
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// Paginated response wrapper.
#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub pagination: PaginationInfo,
}

/// Pagination metadata in the response.
#[derive(Debug, Serialize)]
pub struct PaginationInfo {
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Entity cursor: keyset pagination by (created_at, id).
/// Used for instances, cycles, tasks, runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityCursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

/// Event cursor: keyset pagination by (instance_id, seq).
/// Used for the events endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventCursor {
    pub instance_id: Uuid,
    pub seq: i64,
}

impl EntityCursor {
    /// Encode the cursor as a URL-safe base64 JSON string.
    pub fn encode(&self) -> String {
        let json = serde_json::to_string(self).expect("EntityCursor serialization cannot fail");
        URL_SAFE_NO_PAD.encode(json.as_bytes())
    }

    /// Decode a cursor from a URL-safe base64 JSON string.
    pub fn decode(s: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(s).ok()?;
        let json_str = std::str::from_utf8(&bytes).ok()?;
        serde_json::from_str(json_str).ok()
    }
}

impl EventCursor {
    /// Encode the cursor as a URL-safe base64 JSON string.
    pub fn encode(&self) -> String {
        let json = serde_json::to_string(self).expect("EventCursor serialization cannot fail");
        URL_SAFE_NO_PAD.encode(json.as_bytes())
    }

    /// Decode a cursor from a URL-safe base64 JSON string.
    pub fn decode(s: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(s).ok()?;
        let json_str = std::str::from_utf8(&bytes).ok()?;
        serde_json::from_str(json_str).ok()
    }
}

/// Clamp the requested limit to [1, max_page_limit], defaulting to default_page_limit.
pub fn clamp_limit(requested: Option<u32>, default_limit: u32, max_limit: u32) -> u32 {
    match requested {
        Some(0) => default_limit,
        Some(n) if n > max_limit => max_limit,
        Some(n) => n,
        None => default_limit,
    }
}

/// Build a PaginatedResponse from a fetched page.
///
/// The caller should fetch `limit + 1` rows. If the vec has more than `limit`
/// items, there are more pages. The extra item is removed and the next cursor
/// is built from the last included item.
pub fn build_page<T, F>(mut items: Vec<T>, limit: u32, cursor_fn: F) -> PaginatedResponse<T>
where
    T: Serialize,
    F: Fn(&T) -> String,
{
    let has_more = items.len() > limit as usize;
    if has_more {
        items.truncate(limit as usize);
    }
    let next_cursor = if has_more {
        items.last().map(&cursor_fn)
    } else {
        None
    };
    PaginatedResponse {
        data: items,
        pagination: PaginationInfo {
            next_cursor,
            has_more,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    // ── EntityCursor ──

    #[test]
    fn entity_cursor_encode_decode_round_trip() {
        let cursor = EntityCursor {
            created_at: Utc::now(),
            id: Uuid::new_v4(),
        };
        let encoded = cursor.encode();
        let decoded = EntityCursor::decode(&encoded).expect("should decode");
        assert_eq!(cursor, decoded);
    }

    #[test]
    fn entity_cursor_decode_invalid_base64() {
        assert!(EntityCursor::decode("!!!not-base64!!!").is_none());
    }

    #[test]
    fn entity_cursor_decode_invalid_json() {
        let encoded = URL_SAFE_NO_PAD.encode(b"not json");
        assert!(EntityCursor::decode(&encoded).is_none());
    }

    #[test]
    fn entity_cursor_decode_wrong_json_shape() {
        let encoded = URL_SAFE_NO_PAD.encode(b"{\"wrong\": \"shape\"}");
        assert!(EntityCursor::decode(&encoded).is_none());
    }

    // ── EventCursor ──

    #[test]
    fn event_cursor_encode_decode_round_trip() {
        let cursor = EventCursor {
            instance_id: Uuid::new_v4(),
            seq: 42,
        };
        let encoded = cursor.encode();
        let decoded = EventCursor::decode(&encoded).expect("should decode");
        assert_eq!(cursor, decoded);
    }

    #[test]
    fn event_cursor_decode_invalid() {
        assert!(EventCursor::decode("garbage").is_none());
    }

    // ── clamp_limit ──

    #[test]
    fn clamp_limit_default() {
        assert_eq!(clamp_limit(None, 50, 100), 50);
    }

    #[test]
    fn clamp_limit_zero_returns_default() {
        assert_eq!(clamp_limit(Some(0), 50, 100), 50);
    }

    #[test]
    fn clamp_limit_within_range() {
        assert_eq!(clamp_limit(Some(25), 50, 100), 25);
    }

    #[test]
    fn clamp_limit_exceeds_max() {
        assert_eq!(clamp_limit(Some(200), 50, 100), 100);
    }

    #[test]
    fn clamp_limit_at_max() {
        assert_eq!(clamp_limit(Some(100), 50, 100), 100);
    }

    #[test]
    fn clamp_limit_one() {
        assert_eq!(clamp_limit(Some(1), 50, 100), 1);
    }

    // ── build_page ──

    #[test]
    fn build_page_no_more() {
        let items = vec![1, 2, 3];
        let page = build_page(items, 5, |i| i.to_string());
        assert_eq!(page.data, vec![1, 2, 3]);
        assert!(!page.pagination.has_more);
        assert!(page.pagination.next_cursor.is_none());
    }

    #[test]
    fn build_page_exact_limit() {
        let items = vec![1, 2, 3, 4, 5];
        let page = build_page(items, 5, |i| i.to_string());
        assert_eq!(page.data, vec![1, 2, 3, 4, 5]);
        assert!(!page.pagination.has_more);
        assert!(page.pagination.next_cursor.is_none());
    }

    #[test]
    fn build_page_has_more() {
        // Caller fetched limit+1 = 6 items
        let items = vec![1, 2, 3, 4, 5, 6];
        let page = build_page(items, 5, |i| i.to_string());
        assert_eq!(page.data, vec![1, 2, 3, 4, 5]);
        assert!(page.pagination.has_more);
        assert_eq!(page.pagination.next_cursor, Some("5".to_string()));
    }

    #[test]
    fn build_page_empty() {
        let items: Vec<i32> = vec![];
        let page = build_page(items, 5, |i| i.to_string());
        assert!(page.data.is_empty());
        assert!(!page.pagination.has_more);
        assert!(page.pagination.next_cursor.is_none());
    }
}
