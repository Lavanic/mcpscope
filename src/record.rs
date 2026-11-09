//! Recording pipeline. The relay hands a copy of each line to a channel and
//! a dedicated thread writes rows to sqlite, so the relay never waits on the
//! database. Any storage problem disables recording with one warning and the
//! session carries on unrecorded.

use crate::db;
use crate::jsonrpc::{self, Frame};
use crate::redact;
use rusqlite::{Connection, params};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    ClientToServer,
    ServerToClient,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::ClientToServer => "c2s",
            Direction::ServerToClient => "s2c",
        }
    }

    /// MCP is two-way: servers can send requests too, and the two sides
    /// number their tickets independently. A response pairs with the open
    /// request that traveled the opposite way.
    fn opposite(self) -> Direction {
        match self {
            Direction::ClientToServer => Direction::ServerToClient,
            Direction::ServerToClient => Direction::ClientToServer,
        }
    }
}

/// An open request waiting for its answer.
struct OpenRequest {
    row_id: i64,
    method: String,
    ts_ms: i64,
}

enum Event {
    Line(Direction, Vec<u8>),
    Finish(i32),
}

/// What a traffic recorder keeps. Shown by the first recording and by
/// install, because a proxy's stderr often ends up in a client's log file
/// rather than in front of a person.
pub const RETENTION_NOTICE: &str = "note: requests and results are stored in full and can \
     include secrets or file contents; the database never leaves this machine. \
     Add --redact to a wrapped server to mask credential-shaped fields.";

/// Owned by the proxy; one writer thread lives behind it.
pub struct Recorder {
    tx: Sender<Event>,
    writer: JoinHandle<()>,
}

/// Per-direction sender held by each pump.
pub struct LineSink {
    tx: Sender<Event>,
    dir: Direction,
}

impl LineSink {
    /// Never blocks and never fails; worst case the line goes unrecorded.
    pub fn send(&self, line: &[u8]) {
        let _ = self.tx.send(Event::Line(self.dir, line.to_vec()));
    }
}

impl Recorder {
    pub fn start(server: &str, command: &[String], redact: bool, max_body: usize) -> Recorder {
        let (tx, rx) = mpsc::channel();
        let server = server.to_owned();
        // display only; args with spaces are not round-trippable from this
        let command = command.join(" ");
        let writer = thread::spawn(move || write_loop(&rx, &server, &command, redact, max_body));
        Recorder { tx, writer }
    }

    pub fn sink(&self, dir: Direction) -> LineSink {
        LineSink {
            tx: self.tx.clone(),
            dir,
        }
    }

    /// Record the exit and wait for pending rows to reach disk.
    pub fn finish(self, exit_code: i32) {
        let _ = self.tx.send(Event::Finish(exit_code));
        let _ = self.writer.join();
    }
}

fn write_loop(rx: &Receiver<Event>, server: &str, command: &str, redact: bool, max_body: usize) {
    let path = match db::default_path() {
        Ok(path) => path,
        Err(e) => return disabled(rx, &format!("{e:#}")),
    };
    let conn = match db::open(&path) {
        Ok(conn) => conn,
        Err(e) => return disabled(rx, &format!("{e:#}")),
    };
    // Said once, before the first session is ever stored. Keyed on the
    // sessions table rather than the file: readers create the file too, and
    // a notice skipped because `tail` ran first is a notice nobody ever got.
    let first_ever = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|n| n == 0)
        .unwrap_or(false);
    if first_ever {
        eprintln!("mcpscope: recording to {}", path.display());
        eprintln!("mcpscope: {RETENTION_NOTICE}");
    }
    let session = match start_session(&conn, server, command) {
        Ok(id) => id,
        Err(e) => return disabled(rx, &e.to_string()),
    };
    // open requests waiting for answers, keyed by who asked and the ticket id
    let mut open: HashMap<(Direction, String), OpenRequest> = HashMap::new();
    loop {
        let Ok(event) = rx.recv() else { return };
        match event {
            Event::Line(dir, bytes) => {
                let now = now_ms();
                let frame = jsonrpc::classify(&bytes);
                // a response is matched to its request before inserting, so
                // its row carries the method, the request row, and the time
                // the answer took
                let answered = match &frame {
                    Frame::Response { id, .. } => open.remove(&(dir.opposite(), id.clone())),
                    _ => None,
                };
                let (kind, method, rpc_id, error_code, error_msg) = match &frame {
                    Frame::Request { id, method } => (
                        "request",
                        Some(method.as_str()),
                        Some(id.as_str()),
                        None,
                        None,
                    ),
                    Frame::Notification { method } => {
                        ("notification", Some(method.as_str()), None, None, None)
                    }
                    Frame::Response { id, error } => (
                        "response",
                        answered.as_ref().map(|req| req.method.as_str()),
                        Some(id.as_str()),
                        error.as_ref().map(|(code, _)| *code),
                        error.as_ref().map(|(_, msg)| msg.as_str()),
                    ),
                    Frame::Noise => ("noise", None, None, None, None),
                };
                // stored body drops the line ending; size_bytes keeps the
                // true wire size. Redaction rewrites only this stored copy,
                // long after the original bytes went over the wire.
                let body = match redact {
                    true => match serde_json::from_slice::<Value>(&bytes) {
                        Ok(mut v) => {
                            redact::scrub(&mut v);
                            v.to_string().into()
                        }
                        Err(_) => String::from_utf8_lossy(&bytes),
                    },
                    false => String::from_utf8_lossy(&bytes),
                };
                // the cap also applies to storage only: a 100MB tool result
                // reaches the client whole and the cabinet keeps the head
                let stored = body.trim_end();
                let truncated = stored.len() > max_body;
                let stored = clamp_utf8(stored, max_body);
                let result = conn.execute(
                    "INSERT INTO messages
                     (session_id, ts_ms, direction, kind, method, rpc_id,
                      error_code, error_msg, body, size_bytes, truncated,
                      pair_id, latency_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        session,
                        now,
                        dir.as_str(),
                        kind,
                        method,
                        rpc_id,
                        error_code,
                        error_msg,
                        stored,
                        bytes.len() as i64,
                        truncated,
                        answered.as_ref().map(|req| req.row_id),
                        answered.as_ref().map(|req| now - req.ts_ms),
                    ],
                );
                if let Err(e) = result {
                    return disabled(rx, &e.to_string());
                }
                // the opening handshake names both parties; copy it onto the
                // session row so old recordings stay identifiable
                if let Frame::Request { method, .. } = &frame
                    && method == "initialize"
                {
                    let client = serde_json::from_slice::<Value>(&bytes)
                        .ok()
                        .and_then(|v| name_version(v.get("params")?.get("clientInfo")?));
                    let _ = conn.execute(
                        "UPDATE sessions SET client = COALESCE(?1, client) WHERE id = ?2",
                        params![client, session],
                    );
                }
                if answered
                    .as_ref()
                    .is_some_and(|req| req.method == "initialize")
                    && let Ok(v) = serde_json::from_slice::<Value>(&bytes)
                {
                    let server_info = v
                        .get("result")
                        .and_then(|r| name_version(r.get("serverInfo")?));
                    // the request proposes a protocol version; the response's
                    // word is the negotiated one, so that is what we keep
                    let protocol = v
                        .get("result")
                        .and_then(|r| r.get("protocolVersion"))
                        .and_then(Value::as_str);
                    let _ = conn.execute(
                        "UPDATE sessions SET server_info = COALESCE(?1, server_info),
                                             protocol = COALESCE(?2, protocol)
                         WHERE id = ?3",
                        params![server_info, protocol, session],
                    );
                }
                if let Frame::Request { id, method } = frame {
                    open.insert(
                        (dir, id),
                        OpenRequest {
                            row_id: conn.last_insert_rowid(),
                            method,
                            ts_ms: now,
                        },
                    );
                }
            }
            Event::Finish(code) => {
                let _ = conn.execute(
                    "UPDATE sessions SET ended_at = ?1, exit_code = ?2 WHERE id = ?3",
                    params![now_ms(), code, session],
                );
                return;
            }
        }
    }
}

fn start_session(conn: &Connection, server: &str, command: &str) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO sessions (server_name, command, started_at) VALUES (?1, ?2, ?3)",
        params![server, command, now_ms()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Keep accepting events so senders stay unblocked, but write nothing.
/// A pump can outlive the session while parked on a quiet stdin, so wait
/// for Finish rather than for the channel to close.
fn disabled(rx: &Receiver<Event>, why: &str) {
    eprintln!("mcpscope: recording disabled: {why}");
    while let Ok(event) = rx.recv() {
        if matches!(event, Event::Finish(_)) {
            return;
        }
    }
}

/// The longest prefix within max bytes that is still whole utf-8; cutting a
/// multi-byte character in half would make the stored text invalid.
fn clamp_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

/// "name 1.2.3" out of {"name": ..., "version": ...}, however much is there.
fn name_version(v: &Value) -> Option<String> {
    let name = v.get("name")?.as_str()?;
    Some(match v.get("version").and_then(Value::as_str) {
        Some(version) => format!("{name} {version}"),
        None => name.to_owned(),
    })
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
