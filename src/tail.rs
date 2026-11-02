//! Live follow of recorded traffic, in the spirit of tail -f.
//!
//! The proxy writes rows; we poll the same database for anything newer than
//! the last row we printed. WAL mode (set in db.rs) is what lets two
//! processes share the file without stepping on each other.

use crate::db;
use anyhow::Result;
use rusqlite::params;
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(200);

pub fn run(server: Option<&str>) -> Result<()> {
    let conn = db::open(&db::default_path()?)?;

    // start at the end: tail follows new traffic, it does not replay history
    let mut last_id: i64 =
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |row| {
            row.get(0)
        })?;

    eprintln!("mcpscope: waiting for traffic (ctrl-c to stop)");
    let mut stmt = conn.prepare(
        "SELECT m.id, m.ts_ms, s.server_name, m.direction, m.kind, m.method, m.rpc_id, m.body
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.id > ?1 AND (?2 IS NULL OR s.server_name = ?2)
         ORDER BY m.id",
    )?;
    loop {
        let rows = stmt.query_map(params![last_id, server], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        for row in rows {
            let (id, ts_ms, server_name, direction, kind, method, rpc_id, body) = row?;
            print_row(
                ts_ms,
                &server_name,
                &direction,
                &kind,
                method,
                rpc_id,
                &body,
            );
            last_id = id;
        }
        thread::sleep(POLL);
    }
}

fn print_row(
    ts_ms: i64,
    server: &str,
    direction: &str,
    kind: &str,
    method: Option<String>,
    rpc_id: Option<String>,
    body: &str,
) {
    // >> toward the server, << back toward the client
    let arrow = if direction == "c2s" { ">>" } else { "<<" };
    let id = rpc_id.map(|id| format!(" id={id}")).unwrap_or_default();
    let what = match kind {
        "request" | "notification" => format!("{}{id}", method.unwrap_or_default()),
        "response" => format!("response{id}"),
        // the body itself is the information here
        _ => truncate(body, 100).to_string(),
    };
    println!("{} {server:<12} {arrow} {kind:<12} {what}", hms(ts_ms));
}

/// Wall-clock time of day in UTC. Good enough for following along; local
/// timezones can come with the full viewer.
fn hms(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    let (h, m, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((cut, _)) => &text[..cut],
        None => text,
    }
}
