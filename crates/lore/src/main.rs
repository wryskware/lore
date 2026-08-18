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
    /// Install Lore's host-side assets: an agent host's skills, or this
    /// machine's user-level ignore rules.
    ///
    /// Bare `lore setup` reports what is installed and writes nothing.
    Setup {
        /// What to install, as `lore setup` names it: an agent host
        /// (`claude-code`), or `ignore` for the machine-wide ignore rules.
        target: Option<String>,
        /// Print what would change without writing it.
        #[arg(long)]
        dry_run: bool,
        /// Replace assets that have been edited since they were installed.
        #[arg(long)]
        force: bool,
    },
    /// Trigger (re)indexing of a registered project.
    Index {
        project: Option<String>,
        /// Index even if the pass would drop most of the project's indexed
        /// files. Applies to this invocation only — there is no setting.
        #[arg(long)]
        allow_mass_delete: bool,
    },
    /// Inspect and install chunker plugins.
    ///
    /// A plugin is a directory of data — a manifest plus assets — that teaches
    /// Lore to chunk file types it has no built-in chunker for. Installing one
    /// is machine-wide; using one is per project, via `[plugins] enable` in
    /// that project's `.lore.toml`.
    Plugin {
        #[command(subcommand)]
        command: cli::PluginCommand,
    },
    /// Show daemon and index status.
    Status {
        /// Print the daemon's raw JSON response instead of the table.
        #[arg(long)]
        json: bool,
        /// Additionally report this project's store-scan latency window.
        #[arg(long)]
        project: Option<String>,
        /// Collapse to the daemon, embedding, and fleet-coverage lines only.
        #[arg(long)]
        short: bool,
        /// Redraw in place every N seconds until interrupted (default 2).
        ///
        /// Refuses `--json` rather than quietly winning over it: a watch emits
        /// escape sequences and never terminates, which is not what anything
        /// asking for JSON wants.
        #[arg(
            long,
            value_name = "SECONDS",
            num_args = 0..=1,
            default_missing_value = "2",
            conflicts_with = "json",
        )]
        watch: Option<u64>,
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
        Command::Setup {
            target,
            dry_run,
            force,
        } => cli::setup(target, dry_run, force),
        Command::Index {
            project,
            allow_mass_delete,
        } => cli::run(cli::index(project, allow_mass_delete)),
        Command::Plugin { command } => match command {
            cli::PluginCommand::List => cli::run(cli::plugin_list()),
            // Installing touches only the filesystem, so it needs no runtime
            // and no daemon — deliberately, since the moment a user installs a
            // plugin is often before anything is running.
            cli::PluginCommand::Add { path } => cli::plugin_add(path),
        },
        Command::Status {
            json,
            project,
            short,
            watch,
        } => cli::run(cli::status(json, project, short, watch)),
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(lore::daemon::run(lore::daemon::DaemonOptions::new(
        data_dir,
    )));

    // Dropping the runtime would wait *unboundedly* for in-flight blocking
    // work — a startup rescan hashing a multi-hundred-MB repo kept the process
    // (and the store's ownership lock) alive for minutes after `run` had
    // already withdrawn the handshake, so `lore stop` reported a daemon gone
    // whose successor could not start. `run` already spent SHUTDOWN_GRACE on
    // stragglers and logged that it is exiting anyway; this makes that true.
    // SQLite's journaling makes dying mid-write safe, and the ownership lock
    // ends with the process, which is what admission actually waits on.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}
