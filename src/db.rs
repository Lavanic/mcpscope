//! Sqlite storage for recorded sessions.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

// bump when the schema changes; pre-release we recreate rather than migrate
const SCHEMA_VERSION: i32 = 4;

const SCHEMA: &str = "
CREATE TABLE sessions (
  id          INTEGER PRIMARY KEY,
  server_name TEXT NOT NULL,
  command     TEXT NOT NULL,
  started_at  INTEGER NOT NULL,
  ended_at    INTEGER,
  exit_code   INTEGER,
  client      TEXT,
  server_info TEXT,
  protocol    TEXT
);
CREATE TABLE messages (
  id         INTEGER PRIMARY KEY,
  session_id INTEGER NOT NULL REFERENCES sessions(id),
  ts_ms      INTEGER NOT NULL,
  direction  TEXT NOT NULL CHECK (direction IN ('c2s', 's2c')),
  kind       TEXT NOT NULL CHECK (kind IN ('request', 'response', 'notification', 'noise')),
  method     TEXT,
  rpc_id     TEXT,
  error_code INTEGER,
  error_msg  TEXT,
  body       TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  pair_id    INTEGER REFERENCES messages(id),
  latency_ms INTEGER
);
CREATE INDEX idx_messages_session_ts ON messages(session_id, ts_ms);
";

/// Default database location, e.g. ~/.local/share/mcpscope/mcpscope.db
pub fn default_path() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("no data directory for this platform")?
        .join("mcpscope");
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    Ok(dir.join("mcpscope.db"))
}

pub fn open(path: &Path) -> Result<Connection> {
    let conn =
        Connection::open(path).with_context(|| format!("could not open {}", path.display()))?;
    // WAL lets the proxy write while other commands read the same file
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 2000)?;
    let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        conn.execute_batch("DROP TABLE IF EXISTS messages; DROP TABLE IF EXISTS sessions;")?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(conn)
}
