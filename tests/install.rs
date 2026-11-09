//! install/uninstall edit files people rely on, so every promise gets a test:
//! only command and args change, a backup of the pristine original is kept,
//! running twice is harmless, remote servers are untouched, and uninstall
//! restores the exact original shape.
#![cfg(unix)]

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

/// A fake home directory holding a fake ~/.claude.json; returns (home, config path).
fn fake_home(name: &str) -> (PathBuf, PathBuf) {
    let home = std::env::temp_dir().join(format!("mcpscope-home-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create fake home");
    let cfg = home.join(".claude.json");
    let original = json!({
        "someUserSetting": true,
        "mcpServers": {
            "alpha": {
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "server-alpha"],
                "env": {"ALPHA_TOKEN": "x"}
            },
            "remote": {"type": "http", "url": "https://example.com/mcp"}
        }
    });
    std::fs::write(&cfg, serde_json::to_string_pretty(&original).unwrap()).unwrap();
    (home, cfg)
}

fn run(home: &PathBuf, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_mcpscope"))
        .env("HOME", home)
        .args(args)
        .output()
        .expect("run mcpscope");
    assert!(out.status.success(), "{args:?} failed: {out:?}");
    String::from_utf8(out.stdout).expect("utf8")
}

fn read(cfg: &PathBuf) -> Value {
    serde_json::from_str(&std::fs::read_to_string(cfg).unwrap()).unwrap()
}

#[test]
fn install_wraps_and_uninstall_restores() {
    let (home, cfg) = fake_home("roundtrip");
    let original = read(&cfg);

    run(&home, &["install", "--client", "claude"]);
    let wrapped = read(&cfg);

    // alpha is wrapped: mcpscope + the original command line after "--"
    let alpha = &wrapped["mcpServers"]["alpha"];
    let cmd = alpha["command"].as_str().unwrap();
    assert!(
        cmd.contains("mcpscope"),
        "command points at mcpscope: {cmd}"
    );
    assert_eq!(
        alpha["args"],
        json!(["--name", "alpha", "--", "npx", "-y", "server-alpha"])
    );
    // everything else is untouched
    assert_eq!(alpha["env"], original["mcpServers"]["alpha"]["env"]);
    assert_eq!(wrapped["someUserSetting"], json!(true));
    assert_eq!(
        wrapped["mcpServers"]["remote"],
        original["mcpServers"]["remote"]
    );

    // the backup is the pristine original
    let bak: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".claude.json.mcpscope-bak")).unwrap(),
    )
    .unwrap();
    assert_eq!(bak, original);

    // a second install changes nothing
    let out = run(&home, &["install", "--client", "claude"]);
    assert!(out.contains("already wrapped"), "{out}");
    assert_eq!(read(&cfg), wrapped);

    // uninstall restores the exact original entry
    run(&home, &["uninstall", "--client", "claude"]);
    let restored = read(&cfg);
    assert_eq!(restored["mcpServers"], original["mcpServers"]);
    assert_eq!(restored["someUserSetting"], json!(true));
}

#[test]
fn install_can_target_a_single_server() {
    let (home, cfg) = fake_home("filter");
    run(&home, &["install", "--client", "claude", "nosuch"]);
    // nothing matched, nothing changed, no backup made
    assert_eq!(read(&cfg)["mcpServers"]["alpha"]["command"], json!("npx"));
    assert!(!home.join(".claude.json.mcpscope-bak").exists());
}

#[test]
fn unknown_client_is_an_error() {
    let (home, _cfg) = fake_home("unknown");
    let out = Command::new(env!("CARGO_BIN_EXE_mcpscope"))
        .env("HOME", &home)
        .args(["install", "--client", "nonexistent"])
        .output()
        .expect("run mcpscope");
    assert!(!out.status.success());
}

#[test]
fn doctor_flags_a_wrapped_entry_whose_binary_is_gone() {
    let (home, cfg) = fake_home("stale");
    // wrap, then rewrite the command to a path that does not exist, as if
    // the repo had moved or the binary been uninstalled
    run(&home, &["install", "--client", "claude"]);
    let mut v = read(&cfg);
    v["mcpServers"]["alpha"]["command"] = json!("/nowhere/at/all/mcpscope");
    std::fs::write(&cfg, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    let out = run(&home, &["doctor"]);
    assert!(out.contains("MISSING"), "doctor names the brick: {out}");
}
