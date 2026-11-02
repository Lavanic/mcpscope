//! Byte-transparent relay around the wrapped server process.
//!
//! The contract: bytes pass through unmodified and promptly, the child's
//! exit code is propagated, and nothing mcpscope does can break the session.

use crate::record::{Direction, LineSink, Recorder};
use anyhow::{Context, Result};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::thread;

/// Spawn the server command and relay our stdin/stdout to it untouched,
/// recording a copy of the traffic. Blocks until the server exits; returns
/// its exit code.
pub fn run(server: &str, command: &[String], redact: bool) -> Result<i32> {
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

    let recorder = Recorder::start(server, command, redact);

    // client -> server. When the client closes our stdin, the pump ends and
    // drops child_in, which closes the server's stdin so it can exit cleanly.
    let to_server = {
        let sink = recorder.sink(Direction::ClientToServer);
        thread::spawn(move || pump(io::stdin().lock(), child_in, sink))
    };

    // server -> client
    let to_client = {
        let sink = recorder.sink(Direction::ServerToClient);
        thread::spawn(move || pump(child_out, io::stdout().lock(), sink))
    };

    let status = child.wait().context("waiting on server process")?;

    // make sure everything the server said reached the client before we exit
    let _ = to_client.join();
    // the stdin pump may still be blocked reading a client that went quiet;
    // it dies with the process, so don't join it
    drop(to_server);

    // no portable exit code when killed by a signal; 1 is the least-wrong answer
    let code = status.code().unwrap_or(1);
    recorder.finish(code);
    Ok(code)
}

/// Relay lines from one end to the other until EOF, flushing as they arrive.
/// Each line goes to the wire first and the recorder second: the session has
/// priority over the recording.
fn pump(from: impl Read, mut to: impl Write, sink: LineSink) -> io::Result<()> {
    let mut from = BufReader::new(from);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = from.read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&line)?;
        to.flush()?;
        sink.send(&line);
    }
}
