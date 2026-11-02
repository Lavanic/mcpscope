//! Recording pipeline. The relay hands a copy of each line to a channel and
//! a dedicated thread writes rows to sqlite, so the relay never waits on the
//! database. Any storage problem disables recording with one warning and the
//! session carries on unrecorded.

use crate::db;
use crate::jsonrpc::{self, Frame};
use rusqlite::{Connection, params};
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
    pub fn start(server: &str, command: &[String]) -> Recorder {
        let (tx, rx) = mpsc::channel();
        let server = server.to_owned();
        // display only; args with spaces are not round-trippable from this
        let command = command.join(" ");
        let writer = thread::spawn(move || write_loop(&rx, &server, &command));
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

fn write_loop(rx: &Receiver<Event>, server: &str, command: &str) {
    let conn = match db::default_path().and_then(|path| db::open(&path)) {
        Ok(conn) => conn,
        Err(e) => return disabled(rx, &format!("{e:#}")),
    };
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
                // true wire size
                let body = String::from_utf8_lossy(&bytes);
                let result = conn.execute(
                    "INSERT INTO messages
                     (session_id, ts_ms, direction, kind, method, rpc_id,
                      error_code, error_msg, body, size_bytes, pair_id, latency_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        session,
                        now,
                        dir.as_str(),
                        kind,
                        method,
                        rpc_id,
                        error_code,
                        error_msg,
                        body.trim_end(),
                        bytes.len() as i64,
                        answered.as_ref().map(|req| req.row_id),
                        answered.as_ref().map(|req| now - req.ts_ms),
                    ],
                );
                if let Err(e) = result {
                    return disabled(rx, &e.to_string());
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
