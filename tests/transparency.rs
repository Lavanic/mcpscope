//! The transparency contract as executable proof: whatever goes in comes out
//! byte for byte, exit codes pass through, and a misbehaving server cannot be
//! made worse by the proxy. The servers here are deliberately badly behaved.
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

/// The real compiled binary, pointed at a throwaway data dir so test runs
/// never touch the real recording database. One dir per test: these run in
/// parallel, and sharing a database makes them fight over schema recreation
/// (observed as a 59s suite after a version bump). Test threads are named
/// after their test, which gives us a free unique label.
fn mcpscope() -> Command {
    let test = thread::current()
        .name()
        .unwrap_or("test")
        .replace("::", "-");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mcpscope"));
    cmd.env(
        "MCPSCOPE_DATA_DIR",
        std::env::temp_dir().join(format!("mcpscope-test-{test}-{}", std::process::id())),
    );
    cmd
}

/// Run one full session: feed `input` to the proxy wrapping `server`, collect
/// everything that comes back plus the exit code.
fn run_session(server: &[&str], input: &[u8]) -> (Vec<u8>, i32) {
    let mut child = mcpscope()
        .args(["--name", "test", "--"])
        .args(server)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcpscope");

    // feed stdin from its own thread: with an echoing server, writing all
    // input before reading any output would deadlock once the pipes fill
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = input.to_vec();
    let feeder = thread::spawn(move || {
        let _ = stdin.write_all(&input);
        // stdin drops here, closing the pipe so the server sees EOF
    });

    let out = child.wait_with_output().expect("collect output");
    feeder.join().expect("feeder thread");
    (out.stdout, out.status.code().unwrap_or(-1))
}

/// Deterministic pseudo-random bytes (xorshift). A fixed seed means a failing
/// run can be reproduced exactly, which truly random bytes would not allow.
fn noisy_bytes(len: usize, mut seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        out.extend_from_slice(&seed.to_le_bytes());
    }
    out.truncate(len);
    // sprinkle newlines so the line-based relay actually exercises its framing
    for i in (0..out.len()).step_by(97) {
        out[i] = b'\n';
    }
    out
}

#[test]
fn bytes_pass_through_unchanged() {
    let input = noisy_bytes(1 << 20, 0xB0A710AD);
    let (output, code) = run_session(&["cat"], &input);
    assert_eq!(code, 0);
    assert_eq!(
        output, input,
        "1MB of arbitrary bytes must survive untouched"
    );
}

#[test]
fn final_line_without_newline_survives() {
    let (output, code) = run_session(&["cat"], b"{\"id\":1}\nno trailing newline");
    assert_eq!(code, 0);
    assert_eq!(output, b"{\"id\":1}\nno trailing newline");
}

#[test]
fn huge_single_line_survives() {
    let mut input = noisy_bytes(2 << 20, 0x5EED);
    for byte in &mut input {
        if *byte == b'\n' {
            *byte = b'x';
        }
    }
    input.push(b'\n');
    let (output, code) = run_session(&["cat"], &input);
    assert_eq!(code, 0);
    assert_eq!(output, input, "a 2MB single line must survive untouched");
}

#[test]
fn exit_code_passes_through() {
    let (_, code) = run_session(&["sh", "-c", "exit 7"], b"");
    assert_eq!(code, 7);
}

#[test]
fn noise_is_relayed_not_fixed() {
    let (output, code) = run_session(
        &["sh", "-c", "echo not json at all; cat"],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
    );
    assert_eq!(code, 0);
    let text = String::from_utf8(output).expect("utf8");
    assert!(
        text.starts_with("not json at all\n"),
        "noise reaches the client as-is"
    );
    assert!(
        text.contains("\"method\":\"ping\""),
        "real traffic still flows"
    );
}

#[test]
fn server_dying_mid_session_does_not_hang() {
    // reads one line, echoes it, dies; the second line has nowhere to go
    let (output, code) = run_session(
        &["sh", "-c", "read -r line; printf '%s\\n' \"$line\"; exit 3"],
        b"first\nsecond\n",
    );
    assert_eq!(code, 3, "the server's own exit code is preserved");
    assert_eq!(
        output, b"first\n",
        "everything said before death is delivered"
    );
}
