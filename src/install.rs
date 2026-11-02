//! Wrapping and unwrapping of client configs. This edits files people rely
//! on, so the rules are strict: back up the original once, change nothing
//! but command and args, print exactly what changed, and stay safe to run
//! twice. Uninstall restores from the wrapped entry itself, so it works even
//! if the backup is gone.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

struct Client {
    name: &'static str,
    path: PathBuf,
}

/// Known client config files present on this machine.
fn known_clients(only: Option<&str>) -> Result<Vec<Client>> {
    let home = dirs::home_dir().context("no home directory")?;
    let all = [
        ("claude", home.join(".claude.json")),
        ("cursor", home.join(".cursor/mcp.json")),
    ];
    let mut found = Vec::new();
    for (name, path) in all {
        if only.is_some_and(|o| o != name) {
            continue;
        }
        if path.exists() {
            found.push(Client { name, path });
        }
    }
    if found.is_empty() {
        match only {
            Some(o) => bail!("no config file found for client \"{o}\""),
            None => bail!("no known client configs found (~/.claude.json, ~/.cursor/mcp.json)"),
        }
    }
    Ok(found)
}

pub fn install(client: Option<&str>, names: &[String]) -> Result<()> {
    let me = std::env::current_exe().context("cannot locate own binary")?;
    let me = me.to_string_lossy().into_owned();
    edit_clients(client, names, |key, entry| wrap(key, entry, &me))
}

pub fn uninstall(client: Option<&str>, names: &[String]) -> Result<()> {
    edit_clients(client, names, unwrap)
}

/// What happened to one server entry.
enum Outcome {
    Changed(String),
    Unchanged(&'static str),
}

fn edit_clients(
    client: Option<&str>,
    names: &[String],
    edit: impl Fn(&str, &mut Value) -> Outcome,
) -> Result<()> {
    for client in known_clients(client)? {
        let text = std::fs::read_to_string(&client.path)
            .with_context(|| format!("reading {}", client.path.display()))?;
        let mut cfg: Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", client.path.display()))?;
        let Some(servers) = cfg.get_mut("mcpServers").and_then(Value::as_object_mut) else {
            println!("{}: no mcpServers section, skipping", client.name);
            continue;
        };
        let mut changed = 0;
        for (key, entry) in servers.iter_mut() {
            if !names.is_empty() && !names.iter().any(|n| n == key) {
                continue;
            }
            match edit(key, entry) {
                Outcome::Changed(what) => {
                    println!("  {key}: {what}");
                    changed += 1;
                }
                Outcome::Unchanged(why) => println!("  {key}: {why}"),
            }
        }
        if changed > 0 {
            backup(&client.path)?;
            let mut out = serde_json::to_string_pretty(&cfg)?;
            out.push('\n');
            std::fs::write(&client.path, out)
                .with_context(|| format!("writing {}", client.path.display()))?;
            println!(
                "{}: updated {changed} server(s) in {} (restart the client to apply)",
                client.name,
                client.path.display()
            );
        } else {
            println!("{}: nothing to change", client.name);
        }
    }
    Ok(())
}

fn wrap(key: &str, entry: &mut Value, me: &str) -> Outcome {
    let Some(obj) = entry.as_object_mut() else {
        return Outcome::Unchanged("not a server object, skipped");
    };
    if obj.contains_key("url")
        || obj
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t != "stdio")
    {
        return Outcome::Unchanged("not a stdio server, skipped");
    }
    let Some(command) = obj
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Outcome::Unchanged("no command field, skipped");
    };
    if is_mcpscope(&command) {
        return Outcome::Unchanged("already wrapped");
    }
    let old_args: Vec<Value> = obj
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut args = vec![json!("--name"), json!(key), json!("--"), json!(command)];
    args.extend(old_args);
    obj.insert("command".into(), json!(me));
    obj.insert("args".into(), Value::Array(args));
    Outcome::Changed(format!("wrapped ({command} ...)"))
}

fn unwrap(_key: &str, entry: &mut Value) -> Outcome {
    let Some(obj) = entry.as_object_mut() else {
        return Outcome::Unchanged("not a server object, skipped");
    };
    let wrapped = obj
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(is_mcpscope);
    if !wrapped {
        return Outcome::Unchanged("not wrapped");
    }
    let args = obj
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // the real command line sits after the "--" marker
    let Some(sep) = args.iter().position(|a| a.as_str() == Some("--")) else {
        return Outcome::Unchanged("wrapped but no -- marker; left alone");
    };
    let Some(command) = args.get(sep + 1).and_then(Value::as_str).map(str::to_owned) else {
        return Outcome::Unchanged("wrapped but no inner command; left alone");
    };
    let rest: Vec<Value> = args[sep + 2..].to_vec();
    obj.insert("command".into(), json!(command));
    obj.insert("args".into(), Value::Array(rest));
    Outcome::Changed(format!("restored ({command} ...)"))
}

fn is_mcpscope(command: &str) -> bool {
    Path::new(command)
        .file_stem()
        .is_some_and(|stem| stem == "mcpscope")
}

/// One backup per file, ever: the first install keeps the pristine original,
/// and later runs never clobber it.
fn backup(path: &Path) -> Result<()> {
    let mut bak = path.as_os_str().to_owned();
    bak.push(".mcpscope-bak");
    let bak = PathBuf::from(bak);
    if !bak.exists() {
        std::fs::copy(path, &bak).with_context(|| format!("backing up to {}", bak.display()))?;
        println!("  backup: {}", bak.display());
    }
    Ok(())
}
