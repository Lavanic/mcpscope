//! Sorting wire lines into JSON-RPC frames.
//!
//! MCP messages are JSON-RPC 2.0, one JSON object per line. Shape tells the
//! story: a method plus an id is a request, a method alone is a notification,
//! an id with a result or error is a response. Anything else is noise,
//! usually a server accidentally printing to stdout, which is exactly the
//! kind of bug this tool exists to expose.

use serde_json::Value;

#[derive(Debug, PartialEq)]
pub enum Frame {
    Request {
        id: String,
        method: String,
    },
    Notification {
        method: String,
    },
    Response {
        id: String,
        error: Option<(i64, String)>,
    },
    Noise,
}

pub fn classify(line: &[u8]) -> Frame {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Frame::Noise;
    };
    let Some(obj) = value.as_object() else {
        return Frame::Noise;
    };
    let id = obj.get("id").map(id_text);
    let method = obj.get("method").and_then(Value::as_str).map(str::to_owned);
    match (id, method) {
        (Some(id), Some(method)) => Frame::Request { id, method },
        (None, Some(method)) => Frame::Notification { method },
        (Some(id), None) if obj.contains_key("result") || obj.contains_key("error") => {
            let error = obj.get("error").map(|e| {
                (
                    e.get("code").and_then(Value::as_i64).unwrap_or(0),
                    e.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                )
            });
            Frame::Response { id, error }
        }
        _ => Frame::Noise,
    }
}

/// Wire ids may be numbers or strings; store one canonical text form.
fn id_text(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request() {
        let frame = classify(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        assert_eq!(
            frame,
            Frame::Request {
                id: "1".into(),
                method: "tools/list".into()
            }
        );
    }

    #[test]
    fn notification() {
        let frame = classify(br#"{"jsonrpc":"2.0","method":"notifications/progress"}"#);
        assert_eq!(
            frame,
            Frame::Notification {
                method: "notifications/progress".into()
            }
        );
    }

    #[test]
    fn response_ok() {
        let frame = classify(br#"{"jsonrpc":"2.0","id":"abc","result":{}}"#);
        assert_eq!(
            frame,
            Frame::Response {
                id: "abc".into(),
                error: None
            }
        );
    }

    #[test]
    fn response_error() {
        let frame = classify(
            br#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"no such method"}}"#,
        );
        assert_eq!(
            frame,
            Frame::Response {
                id: "7".into(),
                error: Some((-32601, "no such method".into()))
            }
        );
    }

    #[test]
    fn plain_text_is_noise() {
        assert_eq!(classify(b"debug: starting up"), Frame::Noise);
    }

    #[test]
    fn json_but_not_jsonrpc_is_noise() {
        assert_eq!(classify(b"[1,2,3]"), Frame::Noise);
        assert_eq!(classify(br#"{"id":1}"#), Frame::Noise);
    }
}
