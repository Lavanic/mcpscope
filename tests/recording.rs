//! What ends up in the database: pairing, latency, and classification checked
//! against a scripted server that actually answers.
#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;

/// Run one proxied session against `server` with its own throwaway data dir,
/// and hand back the path of the database it recorded into.
fn run_recorded(name: &str, flags: &[&str], server: &[&str], input: &[u8]) -> PathBuf {
    let data = std::env::temp_dir().join(format!("mcpscope-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let mut child = Command::new(env!("CARGO_BIN_EXE_mcpscope"))
        .env("XDG_DATA_HOME", &data)
        .args(["--name", name])
        .args(flags)
        .arg("--")
        .args(server)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcpscope");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = input.to_vec();
    let feeder = thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    child.wait().expect("session ends");
    feeder.join().expect("feeder thread");
    data.join("mcpscope/mcpscope.db")
}

#[test]
fn response_is_paired_with_its_request() {
    // a server that waits a beat, then answers ticket 1
    let db = run_recorded(
        "pairing",
        &[],
        &[
            "sh",
            "-c",
            r#"read -r _; sleep 0.1; echo '{"jsonrpc":"2.0","id":1,"result":{"ok":true}}'; sleep 0.3"#,
        ],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{}}\n",
    );
    let conn = rusqlite::Connection::open(db).expect("open test db");
    let (method, pair_id, latency): (Option<String>, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT method, pair_id, latency_ms FROM messages WHERE kind = 'response'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("one response row");
    let request_row: i64 = conn
        .query_row(
            "SELECT id FROM messages WHERE kind = 'request'",
            [],
            |row| row.get(0),
        )
        .expect("one request row");

    assert_eq!(
        method.as_deref(),
        Some("tools/call"),
        "method copied from the request"
    );
    assert_eq!(pair_id, Some(request_row), "response points at its request");
    let latency = latency.expect("latency recorded");
    assert!(
        latency >= 50,
        "latency should reflect the 100ms pause, got {latency}ms"
    );
}

#[test]
fn unmatched_response_stays_orphan() {
    // answers a ticket nobody opened
    let db = run_recorded(
        "orphan",
        &[],
        &[
            "sh",
            "-c",
            r#"echo '{"jsonrpc":"2.0","id":99,"result":{}}'; sleep 0.2"#,
        ],
        b"",
    );
    let conn = rusqlite::Connection::open(db).expect("open test db");
    let (method, pair_id, latency): (Option<String>, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT method, pair_id, latency_ms FROM messages WHERE kind = 'response'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("one response row");
    assert_eq!(method, None);
    assert_eq!(pair_id, None);
    assert_eq!(latency, None);
}

#[test]
fn initialize_handshake_lands_on_the_session() {
    let db = run_recorded(
        "handshake",
        &[],
        &[
            "sh",
            "-c",
            r#"read -r _; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"fake-server","version":"9.9"},"capabilities":{}}}'; sleep 0.2"#,
        ],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"clientInfo\":{\"name\":\"test-client\",\"version\":\"1.0\"},\"capabilities\":{}}}\n",
    );
    let conn = rusqlite::Connection::open(db).expect("open test db");
    let (client, server_info, protocol): (Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT client, server_info, protocol FROM sessions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("one session row");
    assert_eq!(client.as_deref(), Some("test-client 1.0"));
    assert_eq!(server_info.as_deref(), Some("fake-server 9.9"));
    assert_eq!(protocol.as_deref(), Some("2025-06-18"));
}

#[test]
fn stats_reports_paired_latency() {
    let db = run_recorded(
        "stats",
        &[],
        &[
            "sh",
            "-c",
            r#"read -r _; sleep 0.1; echo '{"jsonrpc":"2.0","id":1,"result":{}}'; sleep 0.3"#,
        ],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{}}\n",
    );
    let root = db.parent().unwrap().parent().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mcpscope"))
        .env("XDG_DATA_HOME", root)
        .arg("stats")
        .output()
        .expect("run stats");
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        text.contains("tools/call"),
        "stats lists the method:\n{text}"
    );
    assert!(text.contains("ms"), "stats shows a latency figure:\n{text}");
}

#[test]
fn redact_masks_credentials_in_storage_only() {
    let db = run_recorded(
        "redact",
        &["--redact"],
        &["sh", "-c", "read -r _; sleep 0.2"],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"api_key\":\"sk-live-12345\",\"query\":\"weather\"}}\n",
    );
    let conn = rusqlite::Connection::open(db).expect("open test db");
    let body: String = conn
        .query_row(
            "SELECT body FROM messages WHERE kind = 'request'",
            [],
            |row| row.get(0),
        )
        .expect("one request row");
    assert!(
        !body.contains("sk-live-12345"),
        "the secret is gone: {body}"
    );
    assert!(body.contains("[redacted]"), "the mask is visible: {body}");
    assert!(body.contains("weather"), "harmless values survive: {body}");
}

#[test]
fn doctor_reports_database_and_configs() {
    let db = run_recorded(
        "doctor",
        &[],
        &["cat"],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
    );
    let root = db.parent().unwrap().parent().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mcpscope"))
        .env("XDG_DATA_HOME", root)
        .env("HOME", root) // a home with no client configs
        .arg("doctor")
        .output()
        .expect("run doctor");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("1 sessions, 2 messages"), "{text}");
    assert!(text.contains("no config"), "{text}");
}
