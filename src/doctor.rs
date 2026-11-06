//! Environment sanity in one shot: where the data lives, how much there is,
//! and which client configs are wrapped. Designed to be pasted into bug
//! reports.

use crate::db;
use crate::display::{LOCAL_STAMP, local_time_sql};
use crate::install;
use anyhow::Result;
use serde_json::Value;

pub fn run() -> Result<()> {
    let exe = std::env::current_exe()?;
    println!(
        "mcpscope {} at {}",
        env!("CARGO_PKG_VERSION"),
        exe.display()
    );

    let path = db::default_path()?;
    if path.exists() {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let conn = db::open(&path)?;
        let (sessions, messages): (i64, i64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM messages)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let span: Option<(String, String)> = conn
            .query_row(
                &format!(
                    "SELECT {}, {} FROM sessions",
                    local_time_sql(LOCAL_STAMP, "MIN(started_at)"),
                    local_time_sql(LOCAL_STAMP, "MAX(started_at)")
                ),
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .map(|(a, b)| a.zip(b))?;
        println!("database: {} ({})", path.display(), human_bytes(size));
        match span {
            Some((oldest, newest)) => {
                println!("  {sessions} sessions, {messages} messages, {oldest} to {newest}");
            }
            None => println!("  empty"),
        }
    } else {
        println!("database: {} (not created yet)", path.display());
    }

    println!("clients:");
    for (name, cfg) in install::all_clients()? {
        if !cfg.exists() {
            println!("  {name:<8} no config at {}", cfg.display());
            continue;
        }
        println!("  {name:<8} {}", cfg.display());
        let Ok(text) = std::fs::read_to_string(&cfg) else {
            println!("    (unreadable)");
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            println!("    (not valid json)");
            continue;
        };
        match v.get("mcpServers").and_then(Value::as_object) {
            Some(servers) if !servers.is_empty() => {
                for (key, entry) in servers {
                    let state = match entry.get("command").and_then(Value::as_str) {
                        Some(cmd) if install::is_mcpscope(cmd) => "wrapped",
                        Some(_) => "not wrapped",
                        None => "remote or unusual",
                    };
                    println!("    {key:<20} {state}");
                }
            }
            _ => println!("    no mcp servers configured"),
        }
    }
    Ok(())
}

fn human_bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}
