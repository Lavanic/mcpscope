//! Recording pipeline. The relay hands a copy of each line to a channel and
//! a dedicated thread writes rows to sqlite, so the relay never waits on the
//! database. Any storage problem disables recording with one warning and the
//! session carries on unrecorded.

use crate::db;
use rusqlite::{Connection, params};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
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
    loop {
        let Ok(event) = rx.recv() else { return };
        match event {
            Event::Line(dir, bytes) => {
                // stored body drops the line ending; size_bytes keeps the
                // true wire size
                let body = String::from_utf8_lossy(&bytes);
                let result = conn.execute(
                    "INSERT INTO messages (session_id, ts_ms, direction, body, size_bytes)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        session,
                        now_ms(),
                        dir.as_str(),
                        body.trim_end(),
                        bytes.len() as i64
                    ],
                );
                if let Err(e) = result {
                    return disabled(rx, &e.to_string());
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
