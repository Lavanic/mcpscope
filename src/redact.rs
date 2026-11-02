//! Masking of credential-shaped values before storage. The wire is never
//! touched: the recorder scrubs its own copy, after relaying.

use serde_json::Value;

const SENSITIVE: &[&str] = &[
    "token",
    "secret",
    "password",
    "passphrase",
    "authorization",
    "api_key",
    "api-key",
    "apikey",
    "cookie",
    "credential",
];

/// True for key names that commonly hold secrets. Matching is by substring,
/// which deliberately over-redacts (a pagination "next_page_token" is lost
/// too); losing a cursor from a recording is the cheap side of that trade.
fn sensitive(key: &str) -> bool {
    let key = key.to_lowercase();
    SENSITIVE.iter().any(|needle| key.contains(needle))
}

/// Replace the values of sensitive keys, recursively, whole subtrees at once.
pub fn scrub(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                if sensitive(key) {
                    *val = Value::String("[redacted]".to_owned());
                } else {
                    scrub(val);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_sensitive_keys_at_any_depth() {
        let mut v = json!({
            "api_key": "sk-live-123",
            "query": "weather",
            "nested": {"Authorization": "Bearer abc", "page": 2},
            "items": [{"session_token": "xyz", "name": "ok"}]
        });
        scrub(&mut v);
        assert_eq!(v["api_key"], "[redacted]");
        assert_eq!(v["nested"]["Authorization"], "[redacted]");
        assert_eq!(v["items"][0]["session_token"], "[redacted]");
        assert_eq!(v["query"], "weather");
        assert_eq!(v["nested"]["page"], 2);
        assert_eq!(v["items"][0]["name"], "ok");
    }

    #[test]
    fn masks_whole_subtrees_under_sensitive_keys() {
        let mut v = json!({"credentials": {"user": "a", "password": "b"}});
        scrub(&mut v);
        assert_eq!(v["credentials"], "[redacted]");
    }

    #[test]
    fn leaves_scalars_and_arrays_alone() {
        let mut v = json!([1, "two", null]);
        scrub(&mut v);
        assert_eq!(v, json!([1, "two", null]));
    }
}
