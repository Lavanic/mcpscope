//! mcpscope: records the JSON-RPC conversation between an agent and an
//! MCP server without changing it.

use anyhow::{Result, bail};
use clap::Parser;

/// Transparent recording proxy for MCP servers.
///
/// Wrap a server command and its stdio is relayed untouched while the
/// traffic is recorded locally:
///
///   mcpscope --name github -- npx -y @modelcontextprotocol/server-github
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Label for this server in recordings (defaults to the command basename)
    #[arg(long)]
    name: Option<String>,

    /// The server command to wrap, after --
    #[arg(last = true, required = true, num_args = 1..)]
    command: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let server = cli.name.unwrap_or_else(|| basename(&cli.command[0]));
    // relay comes next; fail loudly rather than pretend to record
    bail!("relay for \"{server}\" not implemented yet");
}

/// "npx" from "/usr/bin/npx", for a usable default server label.
fn basename(cmd: &str) -> String {
    std::path::Path::new(cmd)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cmd.to_owned())
}
