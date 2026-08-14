use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lore", version, about = "Local context daemon for AI coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon in the foreground.
    Daemon,
    /// Register a project directory with the daemon.
    Add { path: String },
    /// Trigger (re)indexing of a registered project.
    Index { project: Option<String> },
    /// Show daemon and index status.
    Status,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon => daemon(),
        Command::Add { path } => anyhow::bail!("not implemented yet: add {path}"),
        Command::Index { project } => {
            anyhow::bail!("not implemented yet: index {project:?}")
        }
        Command::Status => anyhow::bail!("not implemented yet: status"),
    }
}

fn daemon() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lore=info".into()),
        )
        .init();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tracing::info!(
                api_version = lore_core::API_VERSION,
                "lore daemon starting (M1 slice not yet implemented)"
            );
            anyhow::bail!("not implemented yet: daemon")
        })
}
