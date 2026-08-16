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
    /// Stop the running daemon cleanly and wait until it is gone.
    ///
    /// Killing it instead leaves a fresh handshake behind, which every client
    /// follows to a dead port until it goes stale.
    Stop,
    /// Register a project directory with the daemon.
    ///
    /// The name is written into the project's `.lore.toml` so it follows the
    /// repo; an existing one there is never rewritten.
    Add {
        /// Project directory. Defaults to the current directory.
        path: Option<String>,
        /// Name to register under. Defaults to the name in the project's
        /// `.lore.toml`, then to the directory's own name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Deregister a project and drop its index.
    Remove {
        /// Project name or key, as `lore status` reports it.
        project: String,
    },
    /// Write a project's `.loreignore` from the ecosystems it looks like.
    ///
    /// Never overwrites an existing one, and needs no running daemon.
    Init {
        /// Project directory. Defaults to the current directory.
        path: Option<String>,
    },
    /// Trigger (re)indexing of a registered project.
    Index { project: Option<String> },
    /// Show daemon and index status.
    Status {
        /// Print the daemon's raw JSON response instead of the table.
        #[arg(long)]
        json: bool,
        /// Additionally report this project's store-scan latency window.
        #[arg(long)]
        project: Option<String>,
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
        Command::Stop => cli::run(cli::stop()),
        Command::Add { path, name } => cli::run(cli::add(path, name)),
        Command::Remove { project } => cli::run(cli::remove(project)),
        Command::Init { path } => cli::init(path),
        Command::Index { project } => cli::run(cli::index(project)),
        Command::Status { json, project } => cli::run(cli::status(json, project)),
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
