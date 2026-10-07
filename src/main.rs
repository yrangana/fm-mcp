//! fm-mcp: an MCP server that lets coding agents hand simple, private work to
//! the on-device model for Apple Foundation Models, through the `fm` CLI.

mod backend;
mod chunk;
mod fm;
mod orphans;
mod schema;
mod server;
mod summarise;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::{ServiceExt, transport::stdio};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::{
    fm::{FmConfig, FmServe},
    server::FmMcp,
};

/// MCP server for Apple Foundation Models (the on-device `fm` model on macOS 27).
///
/// Run with no subcommand to start the stdio MCP server.
#[derive(Parser)]
#[command(name = "fm-mcp", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Configure Claude Code and Codex to use fm-mcp (not implemented yet).
    Install,
    /// Check everything fm-mcp needs and explain what is missing (not implemented yet).
    Doctor,
    /// Internal: started by fm-mcp next to each `fm serve`. Stops it if fm-mcp
    /// is force-killed and can't clean up.
    #[command(name = "__watch", hide = true)]
    Watch {
        #[arg(long)]
        parent: i32,
        #[arg(long)]
        child: i32,
        #[arg(long)]
        socket_dir: PathBuf,
    },
}

/// How long to wait for runtime tasks at exit. Tokio reads stdin on a blocking
/// thread that can't be cancelled, so after a signal the runtime would otherwise
/// wait forever for input that never comes.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(200);

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Watch {
        parent,
        child,
        socket_dir,
    }) = &cli.command
    {
        // Plain threads only: the watchdog should stay tiny.
        orphans::watch(*parent, *child, socket_dir);
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        match cli.command {
            None => serve().await,
            Some(Command::Install) => anyhow::bail!("`fm-mcp install` is not implemented yet"),
            Some(Command::Doctor) => anyhow::bail!("`fm-mcp doctor` is not implemented yet"),
            Some(Command::Watch { .. }) => Ok(()),
        }
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    result
}

async fn serve() -> Result<()> {
    // Logs go to stderr: stdout carries the MCP protocol.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("FM_MCP_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info,rmcp=warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    info!("fm-mcp {} starting", env!("CARGO_PKG_VERSION"));
    // Stop `fm serve` processes orphaned by earlier, force-killed sessions.
    // In the background, so a slow `ps` never delays the MCP handshake.
    tokio::task::spawn_blocking(|| {
        let bases: Vec<PathBuf> = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .into_iter()
            .chain([PathBuf::from("/tmp")])
            .collect();
        orphans::clean_up(&bases);
    });
    let backend = Arc::new(FmServe::new(FmConfig::from_env()));
    let service = FmMcp::new(backend.clone()).serve(stdio()).await?;

    let result = tokio::select! {
        quit = service.waiting() => quit.map(|_| ()).map_err(anyhow::Error::from),
        () = shutdown_signal() => Ok(()),
    };
    backend.shutdown().await;
    info!("fm-mcp stopped");
    result
}

/// Resolves on SIGINT or SIGTERM.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            warn!("cannot listen for SIGTERM: {e}");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("received SIGINT"),
        _ = terminate.recv() => info!("received SIGTERM"),
    }
}
