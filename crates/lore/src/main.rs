use clap::{Parser, Subcommand};

mod cli;

#[derive(Parser)]
#[command(
    name = "lore",
    version,
    about = "Local context daemon for AI coding agents"
)]
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
    Status {
        /// Print the daemon's raw JSON response instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Search the index — the same surface agents get over MCP.
    Search(cli::SearchArgs),
}

/// Client subcommands return `Err` with a message that already tells the user
/// what to do about it, and `main`'s `Result` turns that into `Error: …` on
/// stderr plus a non-zero exit — which is the whole contract a script needs.
fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    match args.command {
        Command::Daemon => daemon(),
        Command::Add { path } => cli::run(cli::add(path)),
        Command::Index { project } => cli::run(cli::index(project)),
        Command::Status { json } => cli::run(cli::status(json)),
        Command::Search(search) => cli::run(cli::search(search)),
    }
}

/// Foreground daemon. Logs go to stderr so stdout stays free for anything a
/// future `--json` mode wants to print, and so `lore daemon 2> lore.log`
/// works the way an operator expects.
fn daemon() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lore=info".into()),
        )
        .init();

    let data_dir = lore::daemon::data_dir()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(lore::daemon::run(lore::daemon::DaemonOptions::new(
            data_dir,
        )))
}
