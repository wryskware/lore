//! Thin MCP stdio proxy (design: D-0007). This binary must stay thin:
//! it forwards MCP tool calls to the daemon's HTTP API and never owns
//! watchers, indexes, or embeddings. stdout is reserved for JSON-RPC.

fn main() -> anyhow::Result<()> {
    anyhow::bail!(
        "not implemented yet: lore-mcp (proxies MCP to the lore daemon, API v{})",
        lore_core::API_VERSION
    )
}
