//! Entry point for the stdio MCP server.
//!
//! **Stdout is JSON-RPC and nothing else.** A single stray `println!` corrupts
//! the framing and the client sees a protocol error with no explanation, so
//! every diagnostic in this process goes to stderr — including the ones from
//! dependencies, which is why the subscriber is installed before anything else
//! runs and pinned to `stderr` rather than left on its default.

use lore_mcp::{Endpoint, LoreServer};
use rmcp::ServiceExt;
use rmcp::transport::io::stdio;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        // Quiet by default: an MCP server's stderr lands in an editor's log
        // pane, where a per-request info line is noise, not signal.
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lore_mcp=warn".into()),
        )
        .init();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve())
}

async fn serve() -> anyhow::Result<()> {
    // Resolving the data directory can fail (no platform-local data dir); the
    // handshake inside it is read per call, not here, so a server started
    // before the daemon still works once the daemon appears.
    let server = LoreServer::new(Endpoint::discovered()?);
    let running = server.serve(stdio()).await?;
    let reason = running.waiting().await?;
    tracing::info!(?reason, "mcp session ended");
    Ok(())
}
