//! mcpscope: records the JSON-RPC conversation between an agent and an
//! MCP server without changing it.

mod db;
mod install;
mod jsonrpc;
mod proxy;
mod record;
mod redact;
mod stats;
mod tail;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

/// Transparent recording proxy for MCP servers.
///
/// Wrap a server command and its stdio is relayed untouched while the
/// traffic is recorded locally:
///
///   mcpscope --name github -- npx -y @modelcontextprotocol/server-github
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// Label for this server in recordings (defaults to the command basename)
    #[arg(long)]
    name: Option<String>,

    /// Mask credential-shaped fields in stored bodies (the wire is untouched)
    #[arg(long)]
    redact: bool,

    /// The server command to wrap, after --
    #[arg(last = true, num_args = 1..)]
    server_cmd: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Follow recorded traffic live
    Tail {
        /// Only show traffic for this server
        #[arg(long)]
        server: Option<String>,
        /// Only show errors and noise
        #[arg(long)]
        errors: bool,
    },
    /// Per-method call counts, error rates, and latency percentiles
    Stats {
        /// Only count traffic for this server
        #[arg(long)]
        server: Option<String>,
        /// Only count traffic newer than this (30m, 24h, 7d)
        #[arg(long)]
        since: Option<String>,
    },
    /// Wrap the servers in a client's config so their traffic is recorded
    Install {
        /// Only edit this client's config (claude, cursor)
        #[arg(long)]
        client: Option<String>,
        /// Only wrap these servers (default: all)
        names: Vec<String>,
    },
    /// Undo install: restore wrapped servers to their original commands
    Uninstall {
        /// Only edit this client's config (claude, cursor)
        #[arg(long)]
        client: Option<String>,
        /// Only restore these servers (default: all)
        names: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Cmd::Tail { server, errors }) => tail::run(server.as_deref(), errors),
        Some(Cmd::Stats { server, since }) => stats::run(server.as_deref(), since.as_deref()),
        Some(Cmd::Install { client, names }) => install::install(client.as_deref(), &names),
        Some(Cmd::Uninstall { client, names }) => install::uninstall(client.as_deref(), &names),
        None => {
            if cli.server_cmd.is_empty() {
                bail!("nothing to do: wrap a server with -- or use a subcommand (see --help)");
            }
            let server = cli.name.unwrap_or_else(|| basename(&cli.server_cmd[0]));
            let code = proxy::run(&server, &cli.server_cmd, cli.redact)?;
            std::process::exit(code);
        }
    }
}

/// "npx" from "/usr/bin/npx", for a usable default server label.
fn basename(cmd: &str) -> String {
    std::path::Path::new(cmd)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cmd.to_owned())
}
