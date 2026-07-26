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

/// Decompress a request body for capture, per its `content-encoding`.
///
/// Only the stored copy is decoded; the bytes forwarded upstream are always the
/// originals. This is not a rewrite of the exchange but a repair of the record:
/// Codex sends `content-encoding: zstd`, and lossily decoding those bytes as
/// UTF-8 produced a `request_body` of mojibake — no model, no tools, no prompt,
/// and nothing a rebuild could ever recover.
///
/// An encoding we do not implement, or a payload that fails to inflate, yields
/// the original bytes. Storing them undecoded is honest; a body invented from a
/// failed decode would not be.
pub fn decode_body(encoding: Option<&str>, body: &[u8]) -> Vec<u8> {
    use std::io::Read;

    let Some(encoding) = encoding else {
        return body.to_vec();
    };
    // `content-encoding` is an ordered list, but a client sending more than one
    // layer is vanishingly rare; take the whole value as a single codec name.
    let decoded = match encoding.trim().to_ascii_lowercase().as_str() {
        "zstd" => zstd::stream::decode_all(body).ok(),
        "gzip" | "x-gzip" => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(body)
                .read_to_end(&mut out)
                .ok()
                .map(|_| out)
        }
        "deflate" => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(body)
                .read_to_end(&mut out)
                .ok()
                .map(|_| out)
        }
        // "identity", anything unknown: nothing to undo.
        _ => None,
    };
    decoded.unwrap_or_else(|| body.to_vec())
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
        h.append(
            HeaderName::from_static("x-a"),
            HeaderValue::from_static("1"),
        );
        h.append(
            HeaderName::from_static("x-a"),
            HeaderValue::from_static("2"),
        );
        let json = headers_to_json(&h);
        assert_eq!(json["x-a"], "1, 2");
    }

    #[test]
    fn a_zstd_body_is_decoded_for_capture() {
        // The shape Codex actually sends: a JSON request body under zstd.
        let json = br#"{"model":"gpt-5.6-luna","input":[]}"#;
        let squashed = zstd::stream::encode_all(&json[..], 0).unwrap();
        assert_ne!(squashed, json.to_vec(), "fixture must really be compressed");

        let decoded = decode_body(Some("zstd"), &squashed);
        assert_eq!(decoded, json.to_vec());
        assert_eq!(body_to_json(&decoded).unwrap()["model"], "gpt-5.6-luna");
    }

    #[test]
    fn an_undecodable_body_is_kept_rather_than_invented() {
        // Wrong codec named: return the bytes untouched instead of guessing.
        let bytes = b"\x01\x02not really gzip";
        assert_eq!(decode_body(Some("gzip"), bytes), bytes.to_vec());
        assert_eq!(decode_body(Some("br"), bytes), bytes.to_vec());
        assert_eq!(decode_body(None, bytes), bytes.to_vec());
    }

    #[test]
    fn body_json_parses_or_falls_back() {
        assert_eq!(body_to_json(b""), None);
        assert_eq!(body_to_json(br#"{"a":1}"#).unwrap()["a"], 1);
        assert_eq!(
            body_to_json(b"not json").unwrap(),
            Value::String("not json".into())
        );
    }
}
