//! Byte-transparent relay around the wrapped server process.
//!
//! The contract: bytes pass through unmodified and promptly, the child's
//! exit code is propagated, and nothing mcpscope does can break the session.

use anyhow::{Context, Result};
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::thread;

/// Spawn the server command and relay our stdin/stdout to it untouched.
/// Blocks until the server exits; returns its exit code.
pub fn run(server: &str, command: &[String]) -> Result<i32> {
    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // the server's stderr is for its own logging; let it through as-is
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("could not start server \"{server}\" ({})", command[0]))?;

    let child_in = child.stdin.take().expect("stdin was piped");
    let child_out = child.stdout.take().expect("stdout was piped");

    // client -> server. When the client closes our stdin, the pump ends and
    // drops child_in, which closes the server's stdin so it can exit cleanly.
    let to_server = thread::spawn(move || pump(io::stdin().lock(), child_in));

    // server -> client
    let to_client = thread::spawn(move || pump(child_out, io::stdout().lock()));

    let status = child.wait().context("waiting on server process")?;

    // make sure everything the server said reached the client before we exit
    let _ = to_client.join();
    // the stdin pump may still be blocked reading a client that went quiet;
    // it dies with the process, so don't join it
    drop(to_server);

    // no portable exit code when killed by a signal; 1 is the least-wrong answer
    Ok(status.code().unwrap_or(1))
}

/// Copy bytes from one end to the other until EOF, flushing as they arrive.
/// A write error (e.g. the other side died) just ends the pump: the session
/// is over and the exit path is handled by wait() above.
fn pump(mut from: impl Read, mut to: impl Write) -> io::Result<()> {
    let mut buf = [0u8; 8192];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&buf[..n])?;
        to.flush()?;
    }
}
