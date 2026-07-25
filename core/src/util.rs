//! Small shared helpers.

use axum::http::HeaderMap;
use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Current time as an RFC3339 string (UTC). Falls back to epoch on formatting
/// error, which should never happen for a valid `OffsetDateTime`.
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Convert an HTTP header map into a JSON object. Duplicate header names are
/// joined with ", " (matching HTTP list semantics). Non-UTF8 values are
/// lossily decoded so nothing is silently dropped.
pub fn headers_to_json(headers: &HeaderMap) -> Value {
    let mut map: Map<String, Value> = Map::new();
    for (name, value) in headers.iter() {
        let key = name.as_str().to_string();
        let val = String::from_utf8_lossy(value.as_bytes()).to_string();
        map.entry(key)
            .and_modify(|existing| {
                if let Value::String(s) = existing {
                    s.push_str(", ");
                    s.push_str(&val);
                }
            })
            .or_insert(Value::String(val));
    }
    Value::Object(map)
}

/// Best-effort parse of a byte body into JSON. Returns `None` for empty bodies
/// and falls back to a JSON string for non-JSON payloads so the raw content is
/// never lost.
pub fn body_to_json(body: &[u8]) -> Option<Value> {
    if body.is_empty() {
        return None;
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(v) => Some(v),
        Err(_) => Some(Value::String(String::from_utf8_lossy(body).to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    #[test]
    fn headers_join_duplicates() {
        let mut h = HeaderMap::new();
        h.append(HeaderName::from_static("x-a"), HeaderValue::from_static("1"));
        h.append(HeaderName::from_static("x-a"), HeaderValue::from_static("2"));
        let json = headers_to_json(&h);
        assert_eq!(json["x-a"], "1, 2");
    }

    #[test]
    fn body_json_parses_or_falls_back() {
        assert_eq!(body_to_json(b""), None);
        assert_eq!(body_to_json(br#"{"a":1}"#).unwrap()["a"], 1);
        assert_eq!(body_to_json(b"not json").unwrap(), Value::String("not json".into()));
    }
}
