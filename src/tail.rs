//! Live follow of recorded traffic, in the spirit of tail -f.
//!
//! The proxy writes rows; we poll the same database for anything newer than
//! the last row we printed. WAL mode (set in db.rs) is what lets two
//! processes share the file without stepping on each other.

use crate::db;
use crate::display::{hms, sanitize_line};
use anyhow::Result;
use rusqlite::params;
use std::io::{IsTerminal, stdout};
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(200);

struct Row {
    id: i64,
    ts_ms: i64,
    server: String,
    direction: String,
    kind: String,
    method: Option<String>,
    rpc_id: Option<String>,
    body: String,
    latency_ms: Option<i64>,
    error_msg: Option<String>,
}

/// ANSI colors, or all-empty strings when the output is not a terminal.
struct Palette {
    dim: &'static str,
    cyan: &'static str,
    green: &'static str,
    yellow: &'static str,
    red: &'static str,
    reset: &'static str,
}

impl Palette {
    fn new(on: bool) -> Palette {
        if on {
            Palette {
                dim: "\x1b[2m",
                cyan: "\x1b[36m",
                green: "\x1b[32m",
                yellow: "\x1b[33m",
                red: "\x1b[31m",
                reset: "\x1b[0m",
            }
        } else {
            Palette {
                dim: "",
                cyan: "",
                green: "",
                yellow: "",
                red: "",
                reset: "",
            }
        }
    }
}

pub fn run(server: Option<&str>, errors_only: bool) -> Result<()> {
    let conn = db::open(&db::default_path()?)?;

    // start at the end: tail follows new traffic, it does not replay history
    let mut last_id: i64 =
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |row| {
            row.get(0)
        })?;

    // the NO_COLOR convention plus pipe detection keep output grep-friendly
    let palette = Palette::new(std::env::var_os("NO_COLOR").is_none() && stdout().is_terminal());

    eprintln!("mcpscope: waiting for traffic (ctrl-c to stop)");
    let mut stmt = conn.prepare(
        "SELECT m.id, m.ts_ms, s.server_name, m.direction, m.kind, m.method, m.rpc_id,
                m.body, m.latency_ms, m.error_msg
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.id > ?1
           AND (?2 IS NULL OR s.server_name = ?2)
           AND (?3 = 0 OR m.kind = 'noise' OR m.error_code IS NOT NULL)
         ORDER BY m.id",
    )?;
    loop {
        let rows = stmt.query_map(params![last_id, server, errors_only], |row| {
            Ok(Row {
                id: row.get(0)?,
                ts_ms: row.get(1)?,
                server: row.get(2)?,
                direction: row.get(3)?,
                kind: row.get(4)?,
                method: row.get(5)?,
                rpc_id: row.get(6)?,
                body: row.get(7)?,
                latency_ms: row.get(8)?,
                error_msg: row.get(9)?,
            })
        })?;
        for row in rows {
            let row = row?;
            print_row(&row, &palette);
            last_id = row.id;
        }
        thread::sleep(POLL);
    }
}

fn print_row(row: &Row, p: &Palette) {
    // >> toward the server, << back toward the client
    let arrow = if row.direction == "c2s" { ">>" } else { "<<" };
    let kind_color = match row.kind.as_str() {
        "noise" => p.yellow,
        "request" => p.green,
        "response" if row.error_msg.is_some() => p.red,
        _ => "",
    };
    let id = match &row.rpc_id {
        Some(id) => format!(" id={id}"),
        None => String::new(),
    };
    let latency = match row.latency_ms {
        Some(ms) => format!(" {}({ms}ms){}", p.dim, p.reset),
        None => String::new(),
    };
    // every field below came off the wire, so none of it is printed raw
    let mut rest = match row.kind.as_str() {
        "request" | "notification" => {
            format!("{}{id}", sanitize_line(row.method.as_deref().unwrap_or("")))
        }
        // "?" marks an orphan response whose request we never saw
        "response" => format!(
            "{}{id}{latency}",
            sanitize_line(row.method.as_deref().unwrap_or("?"))
        ),
        // for noise the body itself is the information
        _ => sanitize_line(truncate(&row.body, 120)),
    };
    if let Some(msg) = &row.error_msg {
        rest = format!("{rest} {}{}{}", p.red, sanitize_line(msg), p.reset);
    }
    println!(
        "{}{}{} {}{:<12}{} {}{arrow}{} {kind_color}{:<12}{} {rest}",
        p.dim,
        hms(row.ts_ms),
        p.reset,
        p.cyan,
        sanitize_line(&row.server),
        p.reset,
        p.dim,
        p.reset,
        row.kind,
        p.reset,
    );
}

fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((cut, _)) => &text[..cut],
        None => text,
    }
}
