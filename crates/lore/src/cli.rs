//! The client half of the `lore` binary: everything except `lore daemon`.
//!
//! These subcommands are thin in exactly the same way `lore-mcp` is thin — they
//! discover the running daemon through `daemon.json` and talk to it over
//! loopback HTTP (D-0007). None of them opens the store, because a CLI process
//! that touched the index directly would be a second owner of it.
//!
//! Registration and reindex live here and *only* here (design 4.1): agents get
//! `search`/`expand`/`status` over MCP and nothing that enrolls a directory.
//!
//! The exception is `init`, which writes a file into the project and talks to
//! nothing at all.
//!
//! ## Why the renderer is duplicated
//!
//! `crates/lore-mcp/src/render.rs` renders the same three responses into nearly
//! the same text. Sharing it would mean either putting presentation into
//! `lore-core` (the wire-contract crate, which should stay free of it) or making
//! this crate depend on `lore-mcp` (dragging rmcp into the daemon binary).
//! Neither is worth it for ~120 lines of `format!`, and the two audiences do
//! differ: this one addresses a human who can run `lore daemon`, the other
//! addresses an agent who has to ask. Both sides are covered by tests, which is
//! what keeps them from drifting apart silently.

use std::fmt::Write as _;
use std::future::Future;
use std::io::IsTerminal as _;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use lore::repo_config::{self, DeclaredName};
use lore::setup;
use lore_core::discovery;
use lore_core::{
    DaemonStatus, EmbeddingStatus, ExpandResponse, IndexRequest, IndexResponse, ProjectInfo,
    ProjectStatus, RegisterProjectRequest, RemoveProjectResponse, SearchRequest, SearchResponse,
    SearchResult, WatchState,
};
use serde::Serialize;

/// Run one client subcommand to completion.
///
/// A current-thread runtime: every client command is a single request with a
/// single await, so a worker pool would be pure startup cost.
pub fn run<F: Future<Output = Result<()>>>(task: F) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(task)
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

#[derive(Debug, clap::Args)]
pub struct SearchArgs {
    /// What to look for. Natural language and literal identifiers both work.
    pub query: String,
    /// Scope to one registered project, by name or id. Defaults to the project
    /// containing the current directory.
    #[arg(long)]
    pub project: Option<String>,
    /// Project-relative path prefix filter, forward slashes.
    #[arg(long)]
    pub path_prefix: Option<String>,
    /// Lowercase language tag filter ("csharp", "rust", "markdown").
    #[arg(long)]
    pub language: Option<String>,
    /// Vault status filter, comma-separated: exploration, leaning, decided,
    /// deprecated, unclassified.
    #[arg(long, value_delimiter = ',')]
    pub status: Vec<String>,
    /// Corpus kind filter, comma-separated: repo, session. Empty = all kinds
    /// (v1 indexes repos only; sessions arrive with the M3 ledger).
    #[arg(long, value_delimiter = ',')]
    pub source: Vec<String>,
    /// Maximum results. The daemon clamps this to a sane ceiling.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Print the daemon's raw JSON response instead of the reading view.
    #[arg(long)]
    pub json: bool,
}

impl From<&SearchArgs> for SearchRequest {
    fn from(args: &SearchArgs) -> Self {
        SearchRequest {
            query: args.query.clone(),
            project: args.project.clone(),
            // Filled in from `/v1/resolve` when `--project` was not given; see
            // [`search`].
            project_key: None,
            path_prefix: args.path_prefix.clone(),
            language: args.language.clone(),
            status: args.status.clone(),
            sources: (!args.source.is_empty()).then(|| args.source.clone()),
            limit: args.limit,
        }
    }
}

/// `lore add [path] [--name <name>]` — register a project root under a name
/// the repository commits.
///
/// The path is made absolute against the current directory before it is sent,
/// because a relative path means nothing to a daemon started from somewhere
/// else. Canonicalization (symlinks, casing, `..`) stays the daemon's job: it
/// is the one that has to decide whether two spellings are the same project.
///
/// Naming is the client's job, not the daemon's, and the file is written only
/// *after* the daemon accepts the name — a `.lore.toml` naming a project that
/// failed to register would be a lie committed into the repo.
pub async fn add(path: Option<String>, name: Option<String>) -> Result<()> {
    let root = absolute_utf8(path.as_deref().unwrap_or("."))?;
    if !root.is_dir() {
        bail!("not a directory: {root}");
    }
    let declared = repo_config::declared_name(&root)?;
    let resolved = resolve_name(&root, name.as_deref(), &declared)?;

    let client = Client::connect()?;
    let body = client
        .post(
            "projects",
            &RegisterProjectRequest {
                root: root.to_string(),
                name: Some(resolved.clone()),
            },
        )
        .await?;
    let project: ProjectInfo = parse(&body)?;
    println!("registered {} (key {})", project.name, project.key);
    println!("  {}", project.root);

    match commit_name(&root, &resolved, &declared) {
        Ok(note) => {
            if let Some(note) = note {
                println!("  {note}");
            }
        }
        // The registration stands; only the repo's copy of the name is
        // missing, and saying so is the difference between a user who re-runs
        // one command and one who wonders why the name did not stick.
        Err(err) => println!(
            "  warning: registered, but {} could not be written: {err:#}",
            root.join(repo_config::REPO_CONFIG_FILE)
        ),
    }

    // Registration is exactly when the generated `.loreignore` is about to be
    // written and exactly when nobody is thinking about it. Marker detection
    // cannot see a vendored SDK, a corpus, or a checked-in credential, so the
    // one place a user will read this is here.
    println!("  {}", setup::ignore_nudge());
    if let Some(nudge) = setup::vault_nudge(&root) {
        println!("  {nudge}");
    }
    Ok(())
}

/// `--name` > the repo's committed `[project].name` > the root's basename.
///
/// A flag that contradicts a committed name is refused rather than applied:
/// the file is the repo's own answer to "what is this project called", and
/// silently registering under a different one would leave the two disagreeing
/// with no sign of it.
fn resolve_name(root: &Utf8Path, flag: Option<&str>, declared: &DeclaredName) -> Result<String> {
    let name = match (flag.map(str::trim), declared) {
        (Some(flag), DeclaredName::Named(existing)) if flag != existing => bail!(
            "{path} already names this project `{existing}`, and lore will not rewrite it.\n\
             Drop --name to register as `{existing}`, or edit {path} to rename the project.",
            path = root.join(repo_config::REPO_CONFIG_FILE),
        ),
        (Some(flag), _) => flag.to_string(),
        (None, DeclaredName::Named(existing)) => existing.clone(),
        (None, _) => root
            .file_name()
            .ok_or_else(|| {
                anyhow::anyhow!("cannot derive a project name from {root}; pass --name")
            })?
            .to_string(),
    };
    validate_name(&name)?;
    Ok(name)
}

/// Names travel through URLs, TOML and log lines, and address a directory
/// nobody should be able to escape from — but they are a human's label, so the
/// rule is "nothing that breaks a downstream", not an identifier grammar.
fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("a project name cannot be empty; pass --name <name>");
    }
    if name.contains(['/', '\\']) {
        bail!("a project name cannot contain a path separator: `{name}`; pass --name <name>");
    }
    if name.chars().any(char::is_control) {
        bail!("a project name cannot contain control characters; pass --name <name>");
    }
    Ok(())
}

/// Write the resolved name into the repo's `.lore.toml`, and say what happened
/// when the answer is not simply "wrote it".
///
/// Appending is textual on purpose. Re-serializing the file through `toml`
/// would round-trip a document Lore only partly understands, discarding the
/// user's comments and key order to add one line.
fn commit_name(root: &Utf8Path, name: &str, declared: &DeclaredName) -> Result<Option<String>> {
    let path = root.join(repo_config::REPO_CONFIG_FILE);
    let table = format!("[project]\nname = {}\n", toml_string(name));
    match declared {
        DeclaredName::Named(_) => Ok(None),
        DeclaredName::Absent => {
            std::fs::write(
                &path,
                format!(
                    "# Lore project configuration, committed so the name follows this repo\n\
                     # across machines and contributors. Edit it freely; lore only ever\n\
                     # appends to it.\n{table}"
                ),
            )
            .with_context(|| format!("writing {path}"))?;
            Ok(Some(format!("wrote {path}")))
        }
        DeclaredName::NoTable => {
            let existing =
                std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
            let separator = if existing.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            std::fs::write(&path, format!("{existing}{separator}{table}"))
                .with_context(|| format!("writing {path}"))?;
            Ok(Some(format!("named the project in {path}")))
        }
        // A second `[project]` table would make the file unparseable, and
        // editing inside the existing one is the user's call, not ours.
        DeclaredName::Unnamed => Ok(Some(format!(
            "{path} has a [project] table with no `name`; add `name = {}` to it so the name \
             follows this repo",
            toml_string(name)
        ))),
    }
}

/// A TOML basic string. Names are permissive, so quoting them by hand is not
/// safe; this is the one place the escaping has to be right.
fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

/// `lore remove <name-or-key>` — deregister a project and drop its index.
///
/// The counterpart `lore add` never had: a stale worktree or bench copy stays
/// registered forever otherwise, mixing its results into every query that
/// resolves to a sibling.
pub async fn remove(project: String) -> Result<()> {
    let client = Client::connect()?;
    let body = client
        .delete(&format!("projects/{}", urlencode(&project)))
        .await?;
    let removed: RemoveProjectResponse = parse(&body)?;
    println!(
        "removed {} (key {})",
        removed.project.name, removed.project.key
    );
    println!("  {}", removed.project.root);
    println!(
        "  dropped {} file(s) and {} chunk(s) from the index",
        removed.files, removed.chunks
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// `lore plugin`
// ---------------------------------------------------------------------------

/// The chunker-plugin surface: two commands, no registry and no fetching
/// (2026-08-17 contract). Distribution is "clone the plugin repo and add it".
#[derive(Debug, clap::Subcommand)]
pub enum PluginCommand {
    /// List the chunker plugins installed on this machine.
    List,
    /// Install a chunker plugin directory into the daemon's data directory.
    Add {
        /// Directory holding the plugin's `lore-plugin.toml` and its assets.
        path: String,
    },
}

/// `lore plugin list` — what this machine has installed.
///
/// **The running daemon is the truth**, because it is the thing that actually
/// routes: it loaded its registry at startup, so a plugin added since then is
/// on disk but not in force, and a report read off the disk would say the
/// opposite of what the index is doing.
///
/// With no daemon running there is nothing to be authoritative, so the
/// directory is read directly and the answer is labeled as what it is: what the
/// *next* daemon will load.
pub async fn plugin_list() -> Result<()> {
    let client = match Client::connect() {
        Ok(client) => client,
        Err(err) => return plugin_list_from_disk(&format!("{err}")),
    };
    let status: DaemonStatus = parse(&client.get("status").await?)?;
    if status.plugins.is_empty() && status.plugin_diagnostics.is_empty() {
        println!("no chunker plugins installed ({})", plugins_dir()?);
        println!("install one with `lore plugin add <path>`");
        return Ok(());
    }
    println!(
        "plugins ({}), as loaded by the running daemon:",
        status.plugins.len()
    );
    for plugin in &status.plugins {
        println!(
            "  {name}  {fingerprint}  {extensions}",
            name = plugin.name,
            fingerprint = short_fingerprint(&plugin.fingerprint),
            extensions = extension_list(&plugin.extensions),
        );
    }
    for diagnostic in &status.plugin_diagnostics {
        println!("  PLUGIN: {diagnostic}");
    }
    Ok(())
}

/// The no-daemon half of [`plugin_list`]: the same directory the daemon reads,
/// read by the client, and said out loud to be exactly that.
fn plugin_list_from_disk(why: &str) -> Result<()> {
    let dir = plugins_dir()?;
    let (registry, diagnostics) = lore::plugin::PluginRegistry::load(&dir);
    println!("{why}");
    println!("\nreading {dir} directly; this is what the next daemon will load:");
    if registry.plugins().is_empty() {
        println!("  (no chunker plugins installed)");
    }
    for plugin in registry.plugins() {
        println!(
            "  {name}  {fingerprint}  {extensions}",
            name = plugin.name,
            fingerprint = short_fingerprint(&plugin.fingerprint),
            extensions = extension_list(
                &plugin
                    .extensions()
                    .into_iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            ),
        );
    }
    for diagnostic in &diagnostics {
        println!("  PLUGIN: {diagnostic}");
    }
    Ok(())
}

/// `lore plugin add <path>` — copy a plugin directory into the data dir.
///
/// Validation is **loading it**, with the daemon's own loader: a manifest
/// checked by anything else would eventually disagree with the thing that runs
/// it. A name already installed under a different fingerprint is refused rather
/// than replaced — plugin identity is its name, and silently swapping one out
/// would re-chunk every file it owns without anybody asking for it.
///
/// Writing into the daemon's directory from a client is safe because nothing
/// here opens the store: the daemon reads `plugins/` exactly once, at startup,
/// which is also why the closing line says so.
pub fn plugin_add(path: String) -> Result<()> {
    let source = absolute_utf8(&path)?;
    let installed = install_plugin(&source, &plugins_dir()?)?;
    if installed.unchanged {
        println!(
            "{name} is already installed and unchanged ({target})",
            name = installed.name,
            target = installed.target,
        );
        return Ok(());
    }

    println!(
        "installed {name}  {fingerprint}  {extensions}",
        name = installed.name,
        fingerprint = short_fingerprint(&installed.fingerprint),
        extensions = extension_list(&installed.extensions),
    );
    println!("  {}", installed.target);
    // Both halves, because either one alone leaves a user with a plugin that
    // appears to do nothing.
    println!(
        "  restart the daemon to load it (`lore stop`, then `lore start`), and enable it in a \
         project's .lore.toml:\n\n    [plugins]\n    enable = [\"{name}\"]\n",
        name = installed.name
    );
    println!("  then `lore index <project>` re-chunks the files it claims (nothing else moves)");
    Ok(())
}

/// What `lore plugin add` installed, or found already there.
#[derive(Debug)]
struct Installed {
    name: String,
    fingerprint: String,
    extensions: Vec<String>,
    target: Utf8PathBuf,
    /// The same plugin, byte for byte, was already installed and nothing was
    /// copied.
    unchanged: bool,
}

/// The whole of `lore plugin add` except the printing, so the refusals are
/// testable against a directory a test owns rather than the machine's real
/// data directory.
fn install_plugin(source: &Utf8Path, plugins_dir: &Utf8Path) -> Result<Installed> {
    if !source.is_dir() {
        bail!(
            "not a directory: {source}\nA chunker plugin is a directory holding lore-plugin.toml and its assets."
        );
    }
    let plugin = lore::plugin::Plugin::load(source)
        .map_err(|diagnostic| anyhow::anyhow!("{diagnostic}\nFix the plugin, then try again."))?;

    let target = plugins_dir.join(&plugin.name);
    let found = Installed {
        name: plugin.name.clone(),
        fingerprint: plugin.fingerprint.clone(),
        extensions: plugin
            .extensions()
            .into_iter()
            .map(ToString::to_string)
            .collect(),
        target: target.clone(),
        unchanged: false,
    };

    if target.exists() {
        match lore::plugin::Plugin::load(&target) {
            // Byte-for-byte the same plugin: installing it again is a no-op,
            // and saying "already installed" is more useful than a refusal.
            Ok(installed) if installed.fingerprint == plugin.fingerprint => {
                return Ok(Installed {
                    unchanged: true,
                    ..found
                });
            }
            Ok(installed) => bail!(
                "a different plugin named `{name}` is already installed at {target}\n  \
                 installed: {installed}\n  candidate: {candidate}\n\
                 Plugin names are unique, and replacing one re-chunks every file it owns.\n\
                 Delete that directory yourself if the replacement is intended, then add this one.",
                name = plugin.name,
                installed = short_fingerprint(&installed.fingerprint),
                candidate = short_fingerprint(&plugin.fingerprint),
            ),
            // Something is at that path that is not a loadable plugin. Refusing
            // is the same answer for the same reason: whatever it is, it is not
            // this command's to delete.
            Err(diagnostic) => bail!(
                "`{name}` cannot be installed: {target} already exists and is not a plugin this \
                 build can load ({diagnostic}).\nDelete that directory yourself, then add this one.",
                name = plugin.name,
            ),
        }
    }

    copy_tree(source, &target).with_context(|| format!("copying {source} to {target}"))?;
    Ok(found)
}

/// Where installed plugins live: the daemon's data directory, resolved exactly
/// as the daemon resolves it (`LORE_DATA_DIR` included).
fn plugins_dir() -> Result<Utf8PathBuf> {
    Ok(discovery::data_dir()?.join(lore::daemon::paths::PLUGINS_DIR))
}

/// Copy a plugin directory. Recursive, files and directories only: a plugin is
/// data, and a link inside one would point at bytes the fingerprint never
/// hashed.
fn copy_tree(source: &Utf8Path, target: &Utf8Path) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in source.read_dir_utf8()? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        // `file_type` does not follow links, so a symlinked file or directory
        // reports as neither and is skipped by the `else`.
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(from, &to)?;
        } else if kind.is_file() {
            std::fs::copy(from, &to)?;
        } else {
            println!("  skipped {from} (not a regular file or directory)");
        }
    }
    Ok(())
}

/// Fingerprints are 64 hex characters; a human comparing two of them needs a
/// handle, not a hash. Same rule as chunk ids: the wire carries the whole
/// thing, the renderer shortens it.
fn short_fingerprint(fingerprint: &str) -> String {
    fingerprint.chars().take(12).collect()
}

fn extension_list(extensions: &[String]) -> String {
    match extensions.is_empty() {
        true => "(claims nothing)".to_string(),
        false => extensions.join(", "),
    }
}

/// `lore setup [target]` — install Lore's host-side assets, or report on them.
///
/// The one command that needs no daemon: these files belong to the user's agent
/// host and to this machine, not to the index, and being able to install them
/// before anything is registered is the point — the skill they carry is what a
/// user needs at the moment they add their first project.
///
/// Bare `lore setup` never writes. Discovery that mutates is a trap: the
/// command a user runs to find out what a command does must be the safe one.
///
/// A target is an agent host (`claude-code`), [`setup::USER_IGNORE_TARGET`],
/// which installs the user-level `loreignore` starting point beside the
/// daemon's own `config.toml`, or [`setup::mcp::MCP_TARGET`], which registers
/// the MCP server with the host's client. Neither of the last two is a host;
/// all three live here because all three answer "what does lore need on this
/// machine to be useful", and none is ever written without being asked for.
///
/// The host branch deliberately does *not* also wire MCP. Installing skills is
/// a machine-level act a user may run from anywhere, and the MCP registration
/// defaults to the current project: silently writing a `.mcp.json` into
/// whatever directory `lore setup claude-code` happened to run in would be a
/// surprise, and an unwanted one in the common case where that directory is
/// not a Lore project at all.
pub fn setup(target: Option<String>, dry_run: bool, force: bool, global: bool) -> Result<()> {
    let Some(target) = target else {
        return setup_report();
    };
    if target == setup::USER_IGNORE_TARGET {
        return setup_user_ignore(dry_run, force);
    }
    if target == setup::mcp::MCP_TARGET {
        return setup_mcp(dry_run, force, global);
    }
    if global {
        bail!(
            "--global applies only to `lore setup {}`",
            setup::mcp::MCP_TARGET
        );
    }
    let host = setup::Host::parse(&target)?;
    let dir = host.skills_dir()?;
    let items = setup::plan(&dir);

    if !setup::pending(&items, force) {
        println!("{host}: nothing to do");
        for item in &items {
            println!("  {:<14}{}", item.name, item.state.label());
        }
        // The one state a user can act on, so it gets the instruction rather
        // than leaving them to guess that a flag exists.
        if items
            .iter()
            .any(|item| item.state == setup::State::Modified)
        {
            println!("\nrun `lore setup {host} --force` to replace your edited copies");
        }
        return Ok(());
    }

    for item in &items {
        write_asset(item, dry_run, force)?;
    }
    if !dry_run {
        println!("\nstart a new agent session to pick them up");
    }
    Ok(())
}

/// `lore setup mcp` — register Lore's MCP server with the agent host.
///
/// Scoped to the current directory unless `--global`, so a machine full of
/// folders that are not Lore projects does not gain a server with nothing to
/// serve. The registration is repairable on its own, which is why this is a
/// target and not a step inside the host install: losing MCP wiring is a thing
/// that happens by itself, and reinstalling skills to fix it is a detour.
fn setup_mcp(dry_run: bool, force: bool, global: bool) -> Result<()> {
    let server = setup::mcp::server_binary()?;
    let scope = match global {
        true => setup::mcp::Scope::User,
        false => setup::mcp::Scope::Project,
    };
    let root = cwd()?;
    let registration = setup::mcp::plan(scope, &root, &server)?;
    let key = setup::mcp::SERVER_KEY;

    println!("{} scope   {}", scope.label(), registration.path);
    if dry_run {
        println!(
            "  would register {key} -> {server} ({})",
            registration.state.label()
        );
        return Ok(());
    }

    match setup::mcp::apply(&registration, force)? {
        setup::Outcome::Installed => println!("  registered {key} -> {server}"),
        setup::Outcome::Updated => println!(
            "  repointed  {key} -> {server}\n  was {}",
            registration.current.as_deref().unwrap_or("(nothing)")
        ),
        setup::Outcome::Overwrote => println!("  replaced   {key} -> {server}"),
        setup::Outcome::Unchanged => {
            println!("  {key} already points at {server}");
            return Ok(());
        }
        setup::Outcome::Kept => {
            println!(
                "  kept       {key} -> {} (lore did not write this; --force replaces it)",
                registration.current.as_deref().unwrap_or("(nothing)")
            );
            return Ok(());
        }
    }

    // The registration points at an index that may not exist yet. Better to say
    // so here than to let the first search come back empty with no explanation.
    if scope == setup::mcp::Scope::Project
        && !root.join(lore::repo_config::REPO_CONFIG_FILE).is_file()
    {
        println!("\nnote: {root} is not a registered Lore project yet — run `lore add .`");
    }
    println!("\nstart a new agent session to pick it up");
    Ok(())
}

/// The current directory, as the UTF-8 path everything else here speaks.
fn cwd() -> Result<Utf8PathBuf> {
    let dir = std::env::current_dir().context("reading the current directory")?;
    Utf8PathBuf::from_path_buf(dir)
        .map_err(|path| anyhow::anyhow!("path is not valid UTF-8: {}", path.display()))
}

/// `lore setup ignore` — install the user-level ignore rules.
///
/// Separate from the host branch because the destination is lore's own data
/// directory rather than an agent's, and because there is exactly one item: the
/// "nothing to do" reporting a list of skills needs would only be noise here.
fn setup_user_ignore(dry_run: bool, force: bool) -> Result<()> {
    let item = setup::plan_user_ignore(&lore_core::discovery::data_dir()?);
    write_asset(&item, dry_run, force)?;
    if !dry_run && item.state != setup::State::UpToDate {
        println!(
            "  it applies to every project you index, and any repo's .gitignore \
             or project's .loreignore overrides it"
        );
    }
    Ok(())
}

fn write_asset(item: &setup::Item, dry_run: bool, force: bool) -> Result<()> {
    if dry_run {
        println!("  would write {:<14}{}", item.name, item.path);
        return Ok(());
    }
    match setup::apply(item, force)? {
        setup::Outcome::Installed => println!("  installed {} -> {}", item.name, item.path),
        setup::Outcome::Updated => println!("  updated   {} -> {}", item.name, item.path),
        setup::Outcome::Overwrote => println!("  replaced  {} -> {}", item.name, item.path),
        setup::Outcome::Unchanged => println!("  {:<14}already up to date", item.name),
        setup::Outcome::Kept => println!(
            "  kept      {} (edited since install; --force replaces it)",
            item.name
        ),
    }
    Ok(())
}

/// The read-only half: every host Lore ships for, whether it is on this
/// machine, what state each asset is in, and the machine-wide ignore rules.
fn setup_report() -> Result<()> {
    for host in setup::Host::ALL {
        if !host.detected() {
            println!("{host}   not detected");
            continue;
        }
        let dir = host.skills_dir()?;
        println!("{host}   detected  {dir}");
        for item in setup::plan(&dir) {
            println!(
                "  {:<14}{:<18}{}",
                item.name,
                item.state.label(),
                item.summary
            );
        }
    }
    // Reported even when absent, because absent is the default state and the
    // user-level rules are the one thing here a user might not know exists.
    let user = setup::plan_user_ignore(&lore_core::discovery::data_dir()?);
    println!("machine       {}", user.path);
    println!(
        "  {:<14}{:<18}{}",
        user.name,
        user.state.label(),
        user.summary
    );
    // Both scopes, always: which one is in play is the question a user comes
    // here with, and someone who registered globally months ago has no other
    // way to remember that.
    match setup::mcp::server_binary() {
        Ok(server) => {
            let root = cwd()?;
            for scope in [setup::mcp::Scope::Project, setup::mcp::Scope::User] {
                match setup::mcp::plan(scope, &root, &server) {
                    Ok(registration) => println!(
                        "mcp {:<9} {:<18}{}",
                        scope.label(),
                        registration.state.label(),
                        registration.path
                    ),
                    Err(err) => println!("mcp {:<9} unreadable ({err})", scope.label()),
                }
            }
        }
        Err(err) => println!("mcp           unavailable ({err})"),
    }
    println!(
        "\nrun `lore setup <host|{}|{}>` to install; nothing above was written",
        setup::USER_IGNORE_TARGET,
        setup::mcp::MCP_TARGET
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// lore start
// ---------------------------------------------------------------------------

/// Where a backgrounded daemon's log goes, inside the data directory.
///
/// The foreground `lore daemon` writes to stderr and an operator redirects it
/// wherever they like; a detached one has nowhere to write, and a daemon whose
/// crash left no trace is the failure `lore start` would otherwise introduce.
const DAEMON_LOG: &str = "daemon.log";

/// Where a spawned embedding server's output goes. Same reasoning, and kept
/// separate so a model server's loading chatter never interleaves with the
/// daemon's own log.
const EMBED_LOG: &str = "embed.log";

/// How long `lore start` waits for the daemon it spawned to answer.
///
/// Startup is a lock, a handshake write and an HTTP bind; the opening rescan
/// runs *after* the daemon is discoverable, so this covers process spawn and
/// not the size of anyone's repo.
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// How long `lore start` waits for a spawned embedding server to answer.
///
/// Generous, because a cold GPU server loads weights before it will embed
/// anything — and cheap to overrun, because the embed worker re-probes an
/// unreachable endpoint forever at a one-minute ceiling
/// (`embed::worker::PROBE_BACKOFF_MAX`). A server that arrives late is picked
/// up either way, so expiring here is reported and *not* an error: all this
/// bound really decides is how long the terminal is held.
const EMBED_READY_TIMEOUT: Duration = Duration::from_secs(180);

/// Ceiling on one readiness probe. Short because it is retried in a loop —
/// this is how often we look, not how long we are willing to wait.
const EMBED_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How often either wait re-checks.
const START_POLL: Duration = Duration::from_millis(200);

/// `lore start` — bring up the embedding server and the daemon, in that order,
/// and return once they are answering.
///
/// Every "the daemon is not running" path in this binary — and in the MCP
/// proxy, which can only ask — now ends in "start it with: lore start". This
/// is the command those messages name: `lore daemon` without a terminal held
/// open for as long as the daemon lives.
///
/// Idempotent by design: both halves check before they spawn, so running it
/// twice is a status report rather than a second process. The daemon's
/// ownership lock (`daemon::ownership`) is the real guarantee — this only
/// makes the common case say something useful instead of failing.
///
/// Embeddings go first so the daemon's very first probe finds a live endpoint.
/// Started in the other order it would still converge, but only after the
/// worker's backoff walked back down, which is up to a minute of lexical-only
/// search for no reason.
///
/// What this deliberately is *not* is a supervisor. Nothing here restarts a
/// child that dies, and `lore stop` does not stop the embedding server: the
/// daemon treats the endpoint as something that may be absent or unhealthy at
/// any moment (D-0007) and degrades visibly when it is, which is exactly the
/// property that lets this command be a one-shot launcher instead of a process
/// manager living inside a context daemon.
pub async fn start() -> Result<()> {
    let data_dir = discovery::data_dir()?;
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating the data directory {data_dir}"))?;
    let config = lore::config::Config::load(&data_dir)?;
    start_embeddings(&config.embeddings, &data_dir).await?;
    start_daemon(&data_dir).await
}

/// The daemon half of [`start`].
async fn start_daemon(data_dir: &Utf8Path) -> Result<()> {
    // A handshake that is present, current and *answering* is the only thing
    // that means "already running". A present one that answers nothing means
    // the daemon died without withdrawing it, which is precisely the state
    // `lore start` should resolve rather than report.
    if let Ok(Some(handshake)) = discovery::read(data_dir) {
        if handshake.api_version != lore_core::API_VERSION {
            bail!(
                "the running lore daemon ({}) speaks API v{}, but this build speaks v{}.\nStop it with: lore stop",
                handshake.daemon_version,
                handshake.api_version,
                lore_core::API_VERSION
            );
        }
        let client = Client::connect_at(data_dir)?;
        if client.get("status").await.is_ok() {
            println!(
                "the lore daemon is already running (pid {}) at {}",
                client.pid, client.base_url
            );
            return Ok(());
        }
        println!(
            "{} names pid {} but nothing is answering there; starting a new daemon",
            discovery::handshake_path(data_dir),
            client.pid
        );
    }

    let exe =
        std::env::current_exe().context("finding this executable to start the daemon from")?;
    let log_path = data_dir.join(DAEMON_LOG);
    let mut command = std::process::Command::new(exe);
    command.arg("daemon");
    redirect(&mut command, &log_path)?;
    detach(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("starting `lore daemon`; its log would be {log_path}"))?;
    println!("starting the lore daemon (pid {})", child.id());

    let deadline = std::time::Instant::now() + START_TIMEOUT;
    loop {
        // Checked before the handshake: a daemon refused admission by the
        // ownership lock exits in milliseconds, and waiting out the full
        // timeout to then guess why is worse than naming the log.
        if let Some(status) = child.try_wait().context("waiting on `lore daemon`")? {
            bail!("`lore daemon` exited immediately ({status}); see {log_path}");
        }
        // The pid must be *ours*. Any other handshake is some other daemon,
        // and reporting it as the one we just started would be a lie that
        // survives until the next command fails.
        if let Ok(client) = Client::connect_at(data_dir)
            && client.pid == child.id()
            && client.get("status").await.is_ok()
        {
            println!("  answering at {}; log: {log_path}", client.base_url);
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "the lore daemon (pid {}) did not answer within {}s; it is still running, so check {log_path}",
                child.id(),
                START_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(START_POLL).await;
    }
}

/// The embedding half of [`start`]: probe, and only if that fails run the
/// configured `[embeddings] start_command` and wait for the endpoint to come
/// up.
///
/// Never fatal. Lexical-only search is a supported state (D-0007), so a
/// missing key, an unreachable endpoint or a command that never finishes
/// loading all end in a printed line and a daemon that starts anyway.
async fn start_embeddings(
    config: &lore::config::EmbeddingsConfig,
    data_dir: &Utf8Path,
) -> Result<()> {
    let Some(settings) = lore::embed::EmbedSettings::from_config(config) else {
        if !config.start_command.is_empty() {
            println!(
                "embeddings: `start_command` is configured but `endpoint` is not, so there is \
                 nothing to wait for; not starting it"
            );
        }
        return Ok(());
    };
    // One attempt, short ceiling: the retry policy the worker uses is for
    // getting real work done through a flaky server, whereas this asks a
    // question we are about to ask again in 200ms.
    let client = lore::embed::EmbedClient::new(lore::embed::EmbedSettings {
        retry: lore::embed::RetryPolicy {
            max_attempts: 1,
            ..Default::default()
        },
        request_timeout: EMBED_PROBE_TIMEOUT,
        ..settings
    })
    .context("building the embedding probe client")?;
    let endpoint = client.endpoint().to_string();

    if client.probe().await.is_ok() {
        println!("embeddings: {endpoint} is already answering");
        return Ok(());
    }
    let Some((program, args)) = config.start_command.split_first() else {
        println!(
            "embeddings: {endpoint} is not answering and no `start_command` is configured; \
             search stays lexical-only until it is up"
        );
        return Ok(());
    };

    let log_path = data_dir.join(EMBED_LOG);
    let mut command = std::process::Command::new(program);
    command.args(args);
    redirect(&mut command, &log_path)?;
    detach(&mut command);
    // The handle is dropped rather than held: this process is not the server's
    // supervisor, and letting it go is what says so. On Windows a detached
    // child outlives its parent outright; on POSIX it is reparented.
    let child = command.spawn().with_context(|| {
        format!("running the configured `[embeddings] start_command`: {program}")
    })?;
    println!(
        "embeddings: started `{program}` (pid {}) for {endpoint}",
        child.id()
    );

    if await_embed_ready(&client, EMBED_READY_TIMEOUT).await {
        println!("  answering; log: {log_path}");
    } else {
        println!(
            "  still not answering after {}s; check {log_path}. The daemon re-probes on \
             its own, so search turns hybrid without another command once it is up \
             (`lore status`)",
            EMBED_READY_TIMEOUT.as_secs()
        );
    }
    Ok(())
}

/// Poll `client` until it embeds something or `timeout` expires. `false` means
/// the deadline won, which is a report and not a failure — see
/// [`start_embeddings`].
///
/// Split out from its caller because the giving-up path is the one worth a
/// test and a 180-second constant is not testable in place.
async fn await_embed_ready(client: &lore::embed::EmbedClient, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if client.probe().await.is_ok() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(START_POLL).await;
    }
}

/// Point a child's stdio at an append-only log file and close its stdin.
///
/// Append rather than truncate: the log of the run that crashed is the one
/// worth having, and a `lore stop` / `lore start` cycle is exactly when it
/// would otherwise be thrown away.
fn redirect(command: &mut std::process::Command, log_path: &Utf8Path) -> Result<()> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("opening {log_path}"))?;
    let err = log
        .try_clone()
        .with_context(|| format!("duplicating the handle to {log_path}"))?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(err));
    Ok(())
}

/// Detach a child from this console so it survives the terminal that started
/// it, and — the part that is not optional — so it does not hold this
/// process's own stdout open after we exit.
///
/// `CreateProcessW` is called with `bInheritHandles = TRUE` by
/// `std::process::Command` unconditionally, which means a child inherits every
/// *inheritable* handle this process holds, not merely the three named in
/// `STARTUPINFO`. Redirecting the daemon's stdio to a log file therefore does
/// not stop it inheriting our stdout — and when our stdout is a pipe (anything
/// that captures output: `$(lore start)`, a PowerShell assignment, a test
/// harness), the read end never sees EOF because a daemon that intends to run
/// for weeks is still holding the write end. The caller hangs forever on a
/// command that already printed everything it was going to print.
///
/// Clearing `HANDLE_FLAG_INHERIT` first is the fix. It mutates this process,
/// not the `Command`, which is why it lives here rather than at a call site:
/// both spawns want it, and this process is about to exit anyway.
#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: both calls take a handle we did not open and do not keep,
        // and neither writes through a pointer. A process with no console and
        // no redirection has null std handles, which `SetHandleInformation`
        // rejects — hence the failure being ignored rather than reported:
        // there is nothing to fix and nothing the user could do about it.
        unsafe {
            let handle = GetStdHandle(id);
            if !handle.is_null() {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }

    // DETACHED_PROCESS: no inherited console, so closing the window that ran
    // `lore start` does not deliver CTRL_CLOSE_EVENT to the daemon and take
    // the index owner down with it.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    // CREATE_NEW_PROCESS_GROUP: Ctrl-C in that window is not the daemon's.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// With stdio already redirected to a file, a POSIX child outlives this
/// process on its own. It stays in the terminal's process group, so Ctrl-C
/// there reaches it — accepted rather than fixed with a `setsid` dance,
/// because Lore is Windows-native (D-0003) and this build exists so the suite
/// runs, not so anyone daemonizes on it.
#[cfg(unix)]
fn detach(_command: &mut std::process::Command) {}

/// How long `lore stop` waits for the daemon to actually be gone.
///
/// The daemon gives its own tasks [`lore::daemon::SHUTDOWN_GRACE`] and then
/// exits regardless, so anything past that plus a margin for the final writes
/// means something is genuinely wrong rather than merely slow — and saying so
/// is more useful than waiting forever.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// How often the handshake is re-read while waiting. Frequent enough that the
/// common case (a daemon with nothing in flight) feels instant.
const STOP_POLL: Duration = Duration::from_millis(100);

/// `lore stop` — ask the running daemon to shut down, and wait until it has.
///
/// The wait is the point. A killed daemon leaves a handshake whose heartbeat
/// is still fresh, so every client follows it to a dead port until the record
/// goes stale — 45 seconds the stop/rebuild/start loop pays every time (#8).
/// A clean stop removes the record on the way out, and this command does not
/// claim success until it is actually gone: reporting "stopped" while the old
/// process still holds the ownership lock would just move the confusion into
/// whatever the user runs next.
///
/// Gone is judged by the handshake, not by the port. The daemon removes it as
/// its last act, and it removes it only if it is still *its* record — so a
/// successor that published over it reads as "gone" here too, which is
/// correct: the daemon we asked to stop is no longer the one being discovered.
pub async fn stop() -> Result<()> {
    let client = Client::connect()?;
    let body = client.post("shutdown", &()).await?;
    let stopping: lore_core::ShutdownResponse = parse(&body)?;
    println!("asked the lore daemon (pid {}) to stop", stopping.pid);

    match client.await_gone(STOP_TIMEOUT).await? {
        true => {
            println!(
                "  stopped; {} removed",
                discovery::handshake_path(&client.data_dir)
            );
            Ok(())
        }
        // Not an assertion that it is hung — a shutdown draining a very long
        // request looks identical from here — so the message says what was
        // observed and what to check, and exits non-zero so a script that
        // chains a restart does not proceed on a guess.
        false => bail!(
            "the daemon (pid {pid}) accepted the stop but {path} still names it after {secs}s.\n\
             Check the daemon's log; if the process is gone, delete that file and start a new one with: lore start",
            pid = stopping.pid,
            path = discovery::handshake_path(&client.data_dir),
            secs = STOP_TIMEOUT.as_secs(),
        ),
    }
}

/// `lore index [project]` — queue a full rescan.
///
/// `allow_mass_delete` overrides the guard that refuses a pass which would
/// drop most of a project's files (D-0015). It rides on this one request and
/// nowhere else: there is deliberately no configuration key for it, so the
/// only way to delete an index's worth of files is for a human to say so
/// again.
pub async fn index(project: Option<String>, allow_mass_delete: bool) -> Result<()> {
    let client = Client::connect()?;
    let body = client
        .post(
            "index",
            &IndexRequest {
                project,
                allow_mass_delete,
            },
        )
        .await?;
    let response: IndexResponse = parse(&body)?;
    if response.queued.is_empty() {
        println!("nothing to index: no projects registered (run `lore add <path>`)");
        return Ok(());
    }
    println!("queued {} project(s) for reindex:", response.queued.len());
    for project in &response.queued {
        println!("  {}  {}", project.name, project.root);
    }
    Ok(())
}

/// `lore status` — daemon and index health. `project` additionally reports
/// that project's per-corpus store-scan latency window.
pub async fn status(
    json: bool,
    project: Option<String>,
    short: bool,
    watch: Option<u64>,
) -> Result<()> {
    if let Some(every) = watch {
        return status_watch(project, short, every).await;
    }
    let body = fetch_status(&project).await?;
    if json {
        println!("{body}");
        return Ok(());
    }
    print!(
        "{}",
        render_status_with(&parse::<DaemonStatus>(&body)?, short, Palette::detect())
    );
    Ok(())
}

/// One `GET /status`, connection included: `--watch` reconnects every tick
/// rather than holding a handle, so a daemon restarted underneath it is picked
/// back up instead of ending the session.
async fn fetch_status(project: &Option<String>) -> Result<String> {
    let client = Client::connect()?;
    let route = match project {
        Some(p) => format!("status?project={}", urlencode(p)),
        None => "status".to_string(),
    };
    client.get(&route).await
}

/// `lore status --watch` — redraw in place until interrupted.
///
/// Repaints from the home position and clears forward rather than clearing
/// first, so the frame is replaced in one write instead of blinking through an
/// empty screen at every tick.
///
/// A daemon that goes away is drawn as a problem and polled for, not treated
/// as the end of the watch: restarting the daemon is the single most likely
/// thing to happen while someone is watching it.
async fn status_watch(project: Option<String>, short: bool, every: u64) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!("--watch repaints the screen and needs a terminal; use `lore status` for a pipe");
    }
    let p = Palette::detect();
    let every = Duration::from_secs(every.max(1));
    // Clear once on entry; every later frame overwrites in place.
    print!("\x1b[2J\x1b[H");
    loop {
        let frame = match fetch_status(&project).await {
            Ok(body) => match parse::<DaemonStatus>(&body) {
                Ok(status) => render_status_with(&status, short, p),
                Err(e) => format!("{}\n", warn(p, &format!("unreadable status: {e}"))),
            },
            Err(e) => format!("{}\n", warn(p, &format!("daemon unreachable: {e}"))),
        };
        print!(
            "\x1b[H{frame}\n{}\x1b[J",
            p.dim(&format!(
                "  watching · every {}s · ctrl-c to stop",
                every.as_secs()
            )),
        );
        std::io::Write::flush(&mut std::io::stdout())?;
        tokio::time::sleep(every).await;
    }
}

/// `lore search <query>` — the same query surface agents get over MCP, so a
/// human can reproduce and debug exactly what an agent saw.
///
/// Every query is scoped to one project. `--project` says which; without it
/// the daemon is asked which registered project contains the current
/// directory — the local discovery convenience D-0016 sanctions (see
/// `GET /v1/resolve`), which fills the identifier in and never replaces one.
/// The flag therefore still wins, and not only as a courtesy: the local CLI is
/// the admin surface, and a human may deliberately query a project they are
/// not standing in, where an agent may not.
pub async fn search(args: SearchArgs) -> Result<()> {
    let client = Client::connect()?;
    let mut request = SearchRequest::from(&args);
    if request.project.is_none() {
        request.project_key = Some(client.resolve_here().await?.key);
    }
    let body = client.post("search", &request).await?;
    if args.json {
        println!("{body}");
        return Ok(());
    }
    print!(
        "{}",
        render_search(&args.query, &parse::<SearchResponse>(&body)?)
    );
    Ok(())
}

/// Minimal percent-encoding for a query value (RFC 3986 unreserved set).
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

fn absolute_utf8(path: &str) -> Result<Utf8PathBuf> {
    let absolute = std::path::absolute(path)
        .with_context(|| format!("resolving `{path}` against the current directory"))?;
    Utf8PathBuf::from_path_buf(absolute)
        .map_err(|path| anyhow::anyhow!("path is not valid UTF-8: {}", path.display()))
}

fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T> {
    serde_json::from_str(body).context("the daemon returned a response this build cannot parse")
}

// ---------------------------------------------------------------------------
// Daemon client
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Client {
    base_url: String,
    /// Whether the handshake's heartbeat was recent when we resolved it. Only
    /// consulted if a request fails, to say *which* kind of gone the daemon is.
    heartbeat_fresh: bool,
    /// Where the handshake lives. `lore stop` watches it; nothing else needs
    /// it, but a client that found a daemon by reading a file should be able
    /// to say which file.
    data_dir: Utf8PathBuf,
    /// The pid the handshake named, so `lore stop` can tell the daemon it
    /// asked to stop from a successor that published over it.
    pid: u32,
    http: reqwest::Client,
}

impl Client {
    fn connect() -> Result<Self> {
        Self::connect_at(&discovery::data_dir()?)
    }

    /// Split out from [`Self::connect`] so the failure paths are testable
    /// without a real data directory or a real daemon.
    fn connect_at(data_dir: &Utf8Path) -> Result<Self> {
        let handshake = discovery::read(data_dir)
            .with_context(|| {
                format!(
                    "the daemon handshake at {} is unreadable; if the daemon is not running, delete it and start it with: lore start",
                    discovery::handshake_path(data_dir)
                )
            })?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the lore daemon is not running (no handshake at {}).\nStart it with: lore start",
                    discovery::handshake_path(data_dir)
                )
            })?;

        if handshake.api_version != lore_core::API_VERSION {
            bail!(
                "the running lore daemon ({}) speaks API v{}, but this build speaks v{}.\nStop the old daemon and start this build with: lore stop, then lore start",
                handshake.daemon_version,
                handshake.api_version,
                lore_core::API_VERSION
            );
        }

        Ok(Self {
            base_url: handshake.base_url(),
            // Staleness is not a veto: a busy daemon can lag a heartbeat and
            // still answer. Liveness is decided by the request itself.
            heartbeat_fresh: discovery::is_fresh(&handshake, discovery::unix_now()),
            data_dir: data_dir.to_owned(),
            pid: handshake.pid,
            http: reqwest::Client::new(),
        })
    }

    async fn get(&self, route: &str) -> Result<String> {
        let url = format!("{}/{route}", self.base_url);
        self.finish(self.http.get(&url), &url).await
    }

    async fn post<T: Serialize>(&self, route: &str, body: &T) -> Result<String> {
        let url = format!("{}/{route}", self.base_url);
        self.finish(self.http.post(&url).json(body), &url).await
    }

    async fn delete(&self, route: &str) -> Result<String> {
        let url = format!("{}/{route}", self.base_url);
        self.finish(self.http.delete(&url), &url).await
    }

    /// Wait until the daemon this client is talking to is no longer the one
    /// `daemon.json` names. `Ok(false)` means the deadline passed first.
    ///
    /// A *replaced* record counts as gone: the pid moved on, so whatever is
    /// there now is a different daemon and the one we asked to stop is done
    /// being discovered. An unreadable record is treated as "not yet" rather
    /// than "gone" — a half-written file is a moment, not an answer, and the
    /// timeout is what stops the wait if it never resolves.
    async fn await_gone(&self, timeout: Duration) -> Result<bool> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match discovery::read(&self.data_dir) {
                Ok(None) => return Ok(true),
                Ok(Some(current)) if current.pid != self.pid => return Ok(true),
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(STOP_POLL).await;
        }
    }

    /// The registered project containing the current directory.
    ///
    /// The daemon's 404 already names the remedy (`lore add <path>`), so it is
    /// relayed rather than restated — a second, differently-worded remedy for
    /// the same condition is how the two drift apart.
    async fn resolve_here(&self) -> Result<ProjectInfo> {
        let cwd = std::env::current_dir().context("reading the current directory")?;
        let cwd = Utf8PathBuf::from_path_buf(cwd)
            .map_err(|path| anyhow::anyhow!("path is not valid UTF-8: {}", path.display()))?;
        let body = self
            .get(&format!("resolve?path={}", urlencode(cwd.as_str())))
            .await?;
        parse(&body)
    }

    /// Returns the raw body so `--json` can print exactly what the daemon said
    /// rather than a re-serialization of it.
    async fn finish(&self, request: reqwest::RequestBuilder, url: &str) -> Result<String> {
        let response = request.send().await.map_err(|err| {
            if self.heartbeat_fresh {
                anyhow::anyhow!(
                    "the lore daemon published a handshake but is not answering at {url} ({err}).\nIt may have crashed; restart it with: lore start"
                )
            } else {
                anyhow::anyhow!(
                    "the lore daemon's handshake is stale and it is not answering at {url} ({err}).\nStart it with: lore start"
                )
            }
        })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("reading the daemon's response body")?;
        if !status.is_success() {
            // Non-2xx bodies are `ApiError` JSON by contract; anything else is
            // relayed as-is rather than swallowed.
            let message = serde_json::from_str::<lore_core::ApiError>(&body)
                .map(|api| api.message)
                .unwrap_or(body);
            bail!("daemon error ({}): {message}", status.as_u16());
        }
        Ok(body)
    }
}

// ---------------------------------------------------------------------------
// Rendering (see the module header for why this is not shared with lore-mcp)
// ---------------------------------------------------------------------------

const LEXICAL_ONLY_NOTE: &str = "note: embeddings are unavailable, so these are lexical matches only; \
     semantically related chunks may be missing (run `lore status`)\n";

/// Displayed chunk-id length, git-style, and the same twelve `lore-mcp` uses.
///
/// A full blake3 id is 64 hex characters whose only job is to be handed back
/// to `expand`, which resolves any prefix at least
/// [`lore_core::MIN_CHUNK_ID_PREFIX`] long — so a shortened id still
/// round-trips, and the wire keeps carrying the whole one.
const SHORT_CHUNK_ID: usize = 12;
const _: () = assert!(SHORT_CHUNK_ID >= lore_core::MIN_CHUNK_ID_PREFIX);

/// The leading [`SHORT_CHUNK_ID`] characters of an id, sliced on a character
/// boundary so an unexpected id shape renders short rather than panicking.
fn short_id(chunk_id: &str) -> &str {
    match chunk_id.char_indices().nth(SHORT_CHUNK_ID) {
        Some((at, _)) => &chunk_id[..at],
        None => chunk_id,
    }
}

/// A symbol path with the chunker's discriminators removed — the same rule
/// `lore-mcp/src/render.rs` applies, and for the same reason.
///
/// `#w<n>` marks one window of an oversized span and `#s<n>` names a run of
/// statements under no declared symbol. Both are identity (they keep derived
/// chunk ids distinct), so the wire keeps them and only the display drops
/// them; a path that is nothing but a filler ordinal is the file's top level.
///
/// Matching `#s`/`#w` followed by digits, rather than cutting at the first
/// `#`, keeps `Counter.#count` — a JavaScript private field — intact.
fn display_symbol(symbol_path: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut filler = false;
    for segment in symbol_path.split('.') {
        match trim_discriminator(segment) {
            "" => filler = true,
            trimmed => kept.push(trimmed),
        }
    }
    match (kept.is_empty(), filler) {
        (true, _) => "top-level statements".to_string(),
        (false, true) => format!("{} (statements)", kept.join(".")),
        (false, false) => kept.join("."),
    }
}

/// Heading titles minus any element that is only a window discriminator.
fn display_headings(headings: &[String]) -> Vec<&str> {
    headings
        .iter()
        .map(String::as_str)
        .filter(|title| !trim_discriminator(title).is_empty())
        .collect()
}

/// One path segment without its trailing `#s<n>`/`#w<n>` discriminator; empty
/// when the segment was nothing but one.
fn trim_discriminator(segment: &str) -> &str {
    let Some((head, tail)) = segment.rsplit_once('#') else {
        return segment;
    };
    let discriminator = matches!(tail.as_bytes().first(), Some(b's' | b'w'))
        && tail.len() > 1
        && tail[1..].bytes().all(|byte| byte.is_ascii_digit());
    if discriminator { head } else { segment }
}

fn render_search(query: &str, response: &SearchResponse) -> String {
    let mut out = String::new();
    let mode = if response.lexical_only {
        "lexical-only"
    } else {
        "hybrid"
    };

    if response.results.is_empty() {
        let _ = writeln!(out, "no results for \"{query}\" ({mode})");
        if response.lexical_only {
            out.push_str(LEXICAL_ONLY_NOTE);
        }
        return out;
    }

    let _ = writeln!(
        out,
        "{} result(s) for \"{query}\" ({mode})",
        response.results.len()
    );
    if response.lexical_only {
        out.push_str(LEXICAL_ONLY_NOTE);
    }
    for (index, result) in response.results.iter().enumerate() {
        out.push('\n');
        push_result(&mut out, index + 1, result);
    }
    out
}

fn push_result(out: &mut String, rank: usize, result: &SearchResult) {
    let language = match &result.language {
        Some(language) => format!("  [{language}]"),
        None => String::new(),
    };
    let _ = writeln!(
        out,
        "[{rank}] {project}  {path}:{start}-{end}  score {score:.3}{language}",
        project = result.project,
        path = result.path,
        start = result.line_start,
        end = result.line_end,
        score = result.score,
    );

    if let Some(symbol) = &result.symbol_path {
        let _ = writeln!(out, "    symbol: {}", display_symbol(symbol));
    }
    if let Some(headings) = &result.heading_path {
        let titles = display_headings(headings);
        if !titles.is_empty() {
            let _ = writeln!(out, "    heading: {}", titles.join(" > "));
        }
    }
    match (&result.design_status, result.decision_refs.is_empty()) {
        (Some(status), true) => {
            let _ = writeln!(out, "    status: {status}");
        }
        (Some(status), false) => {
            let _ = writeln!(
                out,
                "    status: {status}  refs: {}",
                result.decision_refs.join(", ")
            );
        }
        (None, false) => {
            let _ = writeln!(out, "    refs: {}", result.decision_refs.join(", "));
        }
        (None, true) => {}
    }
    // Printed only when Lore refused the document's own declaration — see
    // the same rule in `lore-mcp/src/render.rs`. Absent for a repository with
    // no authority profile: there was no declaration to refuse (D-0012).
    if let (Some(note), Some(authority)) = (&result.authority_note, &result.effective_authority) {
        let _ = writeln!(out, "    authority: {authority} - {note}");
    }
    let _ = writeln!(out, "    chunk_id: {}", short_id(&result.chunk_id));

    out.push_str(result.excerpt.trim_end_matches('\n'));
    out.push('\n');
    if result.excerpt_truncated {
        let _ = writeln!(out, "    (excerpt truncated)");
    }
}

/// Unused today — `lore expand` is not a subcommand, because a human reading a
/// hit opens the file. Kept next to its siblings so the CLI can grow one
/// without re-deriving the format.
#[allow(dead_code)]
fn render_expand(project: &str, response: &ExpandResponse) -> String {
    format!(
        "{project}  {path}:{start}-{end}  (file has {total} lines)\n{text}\n",
        path = response.path,
        start = response.line_start,
        end = response.line_end,
        total = response.file_lines,
        text = response.text.trim_end_matches('\n'),
    )
}

/// Terminal styling, resolved once per invocation.
///
/// Colour is dropped wholesale when stdout is not a terminal or `NO_COLOR` is
/// set (https://no-color.org). `lore status` gets piped into logs and grepped
/// often enough that escape sequences in a redirect would be a regression —
/// and the layout carries every distinction on its own, so a plain render
/// loses decoration and no meaning.
///
/// Box glyphs and bars are *not* gated: they are ordinary UTF-8, they survive
/// a redirect into a file, and gating them would give the piped form a
/// different shape rather than the same shape without colour.
#[derive(Clone, Copy)]
pub struct Palette {
    color: bool,
}

impl Palette {
    fn detect() -> Self {
        Palette {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    /// The palette every test renders through, and the one a pipe gets.
    #[cfg(test)]
    fn plain() -> Self {
        Palette { color: false }
    }

    /// Deliberately non-nesting: each call closes with a full reset, so
    /// wrapping already-painted text would end the outer style early. Compose
    /// by painting the pieces, never by painting a painted string.
    fn paint(self, code: &str, text: &str) -> String {
        if !self.color || text.is_empty() {
            return text.to_string();
        }
        format!("\x1b[{code}m{text}\x1b[0m")
    }

    fn dim(self, text: &str) -> String {
        self.paint("2", text)
    }
    fn bold(self, text: &str) -> String {
        self.paint("1", text)
    }
    fn green(self, text: &str) -> String {
        self.paint("32", text)
    }
    fn yellow(self, text: &str) -> String {
        self.paint("33", text)
    }
    fn red(self, text: &str) -> String {
        self.paint("31", text)
    }
}

/// Display width, ignoring ANSI escapes because they occupy no columns.
///
/// Counts `char`s, so it is wrong for east-asian-wide and combining
/// characters. Everything measured here is a project name, a number, or a
/// path; the failure mode is a frame one column out of true, not a panic.
fn vis_len(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // A CSI sequence is ESC '[' then parameter/intermediate bytes then
            // a final byte in 0x40..=0x7E. The '[' is itself inside that range,
            // so it has to be consumed before the scan for the final byte
            // starts — otherwise the sequence "ends" immediately and its
            // parameters get counted as visible text.
            if chars.next() != Some('[') {
                continue;
            }
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }
    width
}

/// `55931` as `55,931`. Nine projects' chunk counts are the numbers a reader
/// actually compares, and unseparated six-digit figures do not compare by eye.
fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Cells in a coverage bar. Twelve divides into halves, thirds, and quarters,
/// which is what makes a partial bar readable at a glance.
const BAR_CELLS: usize = 12;

/// A coverage bar that is full only at genuinely complete.
///
/// The fill floors, and a project one chunk short renders eleven cells rather
/// than twelve. During a 53k-chunk re-embed the whole value of this surface is
/// the difference between "nearly done" and "done"; a bar that saturates early
/// erases exactly that.
fn bar(p: Palette, embedded: u64, chunks: u64) -> String {
    let filled = if chunks == 0 {
        0
    } else if embedded >= chunks {
        BAR_CELLS
    } else {
        let cells = (embedded as f64 / chunks as f64 * BAR_CELLS as f64).floor() as usize;
        cells.min(BAR_CELLS - 1)
    };
    let done: String = "█".repeat(filled);
    let rest: String = "░".repeat(BAR_CELLS - filled);
    let done = if filled == BAR_CELLS {
        p.green(&done)
    } else {
        p.yellow(&done)
    };
    format!("{done}{}", p.dim(&rest))
}

/// The percentage beside the bar, floored on the same reasoning: `100%` is a
/// claim of completeness, so it is reserved for actual completeness.
fn percent(embedded: u64, chunks: u64) -> u64 {
    if chunks == 0 {
        return 0;
    }
    if embedded >= chunks {
        return 100;
    }
    (embedded as f64 / chunks as f64 * 100.0).floor() as u64
}

/// Embedding coverage summed across every registered project.
///
/// Chunk-weighted, not an average of per-project percentages: a 24-chunk
/// design scratchpad at 100% must not offset an 18k-chunk Unity project at
/// 0%. During a re-embed this is the only number that answers "how far in am
/// I" without mentally summing nine rows.
fn totals(status: &DaemonStatus) -> (u64, u64) {
    (
        status.projects.iter().map(|p| p.embedded_chunks).sum(),
        status.projects.iter().map(|p| p.chunks).sum(),
    )
}

/// The fleet line: project count, corpus size, and how much of it is embedded.
///
/// Carries the raw `embedded/total` pair and not just the percentage. The
/// percentage answers "am I nearly done"; the pair answers "how much is left",
/// which is the question during the ~6 minutes a full re-embed takes.
fn fleet_line(p: Palette, status: &DaemonStatus) -> String {
    let (embedded, chunks) = totals(status);
    format!(
        "{} {} {} {} {} {} {}% {}",
        p.bold(&status.projects.len().to_string()),
        p.dim(if status.projects.len() == 1 {
            "project"
        } else {
            "projects"
        }),
        p.dim("·"),
        p.bold(&format!("{}/{}", commas(embedded), commas(chunks))),
        p.dim("chunks ·"),
        bar(p, embedded, chunks),
        percent(embedded, chunks),
        p.dim("embedded"),
    )
}

/// A rounded frame around the daemon-identity rows.
///
/// Sized to its own content rather than to the terminal: there is no terminal
/// width available here without a dependency, and a frame that is narrower
/// than the window is merely modest, while one that is wider wraps and looks
/// broken. `rows` are pre-styled, so every measurement goes through
/// [`vis_len`].
fn panel(title: &str, right: &str, rows: &[String]) -> String {
    let content = rows.iter().map(|r| vis_len(r)).max().unwrap_or(0);
    // A row costs its own width plus two spaces of padding on each side; the
    // top rule costs title + right + the six literal chars around them, and
    // needs at least one fill dash between the two.
    let header = vis_len(title) + vis_len(right) + 7;
    let inner = content.saturating_add(4).max(header);

    let mut out = String::new();
    let fill = inner - vis_len(title) - vis_len(right) - 6;
    let _ = writeln!(out, "╭─ {title} {} {right} ─╮", "─".repeat(fill));
    for row in rows {
        let pad = inner.saturating_sub(vis_len(row) + 4);
        let _ = writeln!(out, "│  {row}{}  │", " ".repeat(pad));
    }
    let _ = writeln!(out, "╰{}╯", "─".repeat(inner));
    out
}

/// The wordmark, in the block-shadow style figlet calls ANSI Shadow.
///
/// Long form only. `--short` exists precisely for the reader who wants the
/// verdict without the ceremony, and `--json` has no room for ceremony at all.
fn banner(p: Palette) -> String {
    const ROWS: [&str; 6] = [
        r"██╗      ██████╗ ██████╗ ███████╗",
        r"██║     ██╔═══██╗██╔══██╗██╔════╝",
        r"██║     ██║   ██║██████╔╝█████╗  ",
        r"██║     ██║   ██║██╔══██╗██╔══╝  ",
        r"███████╗╚██████╔╝██║  ██║███████╗",
        r"╚══════╝ ╚═════╝ ╚═╝  ╚═╝╚══════╝",
    ];
    let mut out = String::new();
    for (i, row) in ROWS.iter().enumerate() {
        // The shadow rows read as shadow: dimming the last two keeps the
        // wordmark from competing with the status it introduces.
        let painted = if i >= 4 { p.dim(row) } else { p.bold(row) };
        let _ = writeln!(out, "{painted}");
    }
    out
}

/// A status dot: the one glyph that says whether this line is good news.
fn dot(p: Palette, ok: bool) -> String {
    if ok { p.green("●") } else { p.red("●") }
}

/// The warning marker every problem line carries.
///
/// The UPPERCASE keyword that follows it is load-bearing and deliberately
/// kept: it is what the existing surface trained readers (and greps) to look
/// for, and the glyph is an addition to that signal rather than a replacement
/// for it.
fn warn(p: Palette, text: &str) -> String {
    format!("{} {}", p.yellow("⚠"), p.yellow(text))
}

/// A hard stop rather than a degradation — the index is frozen, not merely
/// imperfect — so it gets the colour that means stop.
fn blocked(p: Palette, text: &str) -> String {
    format!("{} {}", p.red("⚠"), p.red(text))
}

/// Tests render through the plain palette: escape sequences in an assertion
/// would pin the decoration rather than the content, and the content is what
/// these tests are about.
#[cfg(test)]
fn render_status(status: &DaemonStatus, short: bool) -> String {
    render_status_with(status, short, Palette::plain())
}

fn render_status_with(status: &DaemonStatus, short: bool, p: Palette) -> String {
    let mut out = String::new();
    let title = p.bold(&format!("lore {}", status.daemon_version));
    let right = p.dim(&format!(
        "api v{} · gen {}",
        status.api_version, status.generation
    ));

    if short {
        // --short is a glance, not a smaller frame: no panel, three lines, the
        // same three verdicts. Per-project detail belongs to the long form.
        let _ = writeln!(out, "{title} {right}");
        let _ = writeln!(out, "{}", embedding_row(p, &status.embeddings));
        push_abandoned(&mut out, p, status.embed_abandoned);
        let _ = writeln!(out, "{}", fleet_line(p, status));
        return out;
    }

    out.push_str(&banner(p));
    out.push('\n');
    let mut rows = vec![embedding_row(p, &status.embeddings)];
    if let Some(row) = plugin_row(p, status) {
        rows.push(row);
    }
    out.push_str(&panel(&title, &right, &rows));
    for diagnostic in &status.plugin_diagnostics {
        let _ = writeln!(out, "{}", warn(p, &format!("PLUGIN: {diagnostic}")));
    }
    push_abandoned(&mut out, p, status.embed_abandoned);
    out.push('\n');

    if status.projects.is_empty() {
        let _ = writeln!(
            out,
            "  {}",
            p.dim("no projects registered — add one with `lore add <path>`")
        );
        return out;
    }

    let _ = writeln!(out, "  {}", fleet_line(p, status));
    out.push('\n');

    let width = status
        .projects
        .iter()
        .map(|project| project.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(28);
    for project in &status.projects {
        let name = truncate(&project.name, width);
        let pad = width.saturating_sub(vis_len(&name));
        let _ = writeln!(
            out,
            "  {name}{blank}  {bar} {pct:>3}%  {chunks:>9} {chunks_label} {files:>6} {files_label}",
            name = p.bold(&name),
            blank = " ".repeat(pad),
            bar = bar(p, project.embedded_chunks, project.chunks),
            pct = percent(project.embedded_chunks, project.chunks),
            chunks = commas(project.chunks),
            chunks_label = p.dim("chunks"),
            files = commas(project.files),
            files_label = p.dim("files"),
        );
        let _ = writeln!(out, "    {}", p.dim(&project.root));
        push_watch(&mut out, p, project.watch);
        push_sources(&mut out, p, project);
        push_plugins(&mut out, p, project);
        push_authority(&mut out, p, project);
        push_authority_violations(&mut out, p, project);
        push_mass_delete_guard(&mut out, p, project);
        push_lease_state(&mut out, p, project);
        out.push('\n');
    }

    for l in &status.latency {
        let _ = writeln!(
            out,
            "  {} {}",
            p.dim(&format!("latency {}", l.endpoint)),
            p.dim(&format!(
                "p50 {}ms · p90 {}ms · p95 {}ms · p99 {}ms · max {}ms ({} requests)",
                l.p50_ms, l.p90_ms, l.p95_ms, l.p99_ms, l.max_ms, l.samples
            )),
        );
    }
    out
}

/// A name too long for its column, cut with an ellipsis rather than allowed to
/// shove every number on its row out of alignment.
fn truncate(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        return name.to_string();
    }
    let keep = width.saturating_sub(1);
    format!("{}…", name.chars().take(keep).collect::<String>())
}

/// The embedding verdict, as a panel row: the one line that decides whether
/// search is semantic or lexical-only.
fn embedding_row(p: Palette, status: &EmbeddingStatus) -> String {
    match status {
        EmbeddingStatus::Unconfigured => format!(
            "{} {}   {}",
            dot(p, false),
            p.dim("embeddings"),
            p.yellow("UNCONFIGURED — no endpoint set; search is lexical-only"),
        ),
        EmbeddingStatus::Unreachable { endpoint, error } => format!(
            "{} {}   {}",
            dot(p, false),
            p.dim("embeddings"),
            p.red(&format!(
                "UNREACHABLE — {endpoint} ({error}); search is lexical-only until it answers"
            )),
        ),
        EmbeddingStatus::Ready { endpoint, model } => format!(
            "{} {}   {} {} {} {} {}",
            dot(p, true),
            p.dim("embeddings"),
            p.green("ready"),
            p.dim("·"),
            p.bold(model),
            p.dim("·"),
            p.dim(endpoint),
        ),
    }
}

/// The chunker plugins this daemon has installed.
///
/// Absent on a machine with no plugins, which is every machine until someone
/// installs one — the same silent-when-clean rule the abandoned-chunk line
/// follows. What the load *refused* is never silent, but that is a diagnostic
/// line outside the panel, not a row inside it: a plugin that half-loaded is
/// indistinguishable from one that is working, and this is the only place that
/// difference surfaces.
fn plugin_row(p: Palette, status: &DaemonStatus) -> Option<String> {
    if status.plugins.is_empty() {
        return None;
    }
    Some(format!(
        "{} {}      {}",
        dot(p, status.plugin_diagnostics.is_empty()),
        p.dim("plugins"),
        status
            .plugins
            .iter()
            .map(|plugin| format!(
                "{} {} {}",
                p.bold(&plugin.name),
                p.dim(&short_fingerprint(&plugin.fingerprint)),
                p.dim(&format!("({})", extension_list(&plugin.extensions))),
            ))
            .collect::<Vec<_>>()
            .join("  ")
    ))
}

/// Silent when the watch is armed — the common case should not add noise —
/// and loud when it is not, because the failure is otherwise invisible: the
/// index simply stops keeping up.
fn push_watch(out: &mut String, p: Palette, state: WatchState) {
    match state {
        // `Unknown` means an older daemon that cannot report; saying nothing
        // is more honest than claiming either state.
        WatchState::Armed | WatchState::Unknown => {}
        WatchState::Retrying => {
            let _ = writeln!(
                out,
                "    {}",
                warn(p, "WATCH RETRYING — not indexing live; use `lore index`")
            );
        }
    }
}

/// Which chunker plugins are in force for this project, and the two ways that
/// can be disappointing.
///
/// Silent for a project that enabled none, which is nearly all of them. Loud
/// about a name enabled but not installed, and about files that fell back:
/// both produce a perfectly ordinary index of plain line windows, which is
/// exactly why neither is discoverable from the results.
fn push_plugins(out: &mut String, p: Palette, project: &ProjectStatus) {
    if !project.plugins_enabled.is_empty() {
        let _ = writeln!(
            out,
            "    {} {}",
            p.dim("plugins"),
            project
                .plugins_enabled
                .iter()
                .map(|plugin| format!("{} {}", plugin.name, short_fingerprint(&plugin.fingerprint)))
                .collect::<Vec<_>>()
                .join("  ")
        );
    }
    if !project.plugins_missing.is_empty() {
        let _ = writeln!(
            out,
            "    {}",
            warn(
                p,
                &format!(
                    "PLUGINS: {names} enabled in .lore.toml but not installed; the files they \
                     would claim are chunked as plain text (install with `lore plugin add <path>`)",
                    names = project.plugins_missing.join(", "),
                )
            )
        );
    }
    if project.plugin_fallback_files > 0 {
        let _ = writeln!(
            out,
            "    {}",
            warn(
                p,
                &format!(
                    "PLUGINS: {files} file(s) fell back to the built-in chunker in the last \
                     index pass because a plugin claimed them and could not run (see the PLUGIN \
                     line above)",
                    files = project.plugin_fallback_files,
                )
            )
        );
    }
}

/// An apply the mass-delete guard refused (D-0015). Loud, and naming the one
/// command that overrides it: until someone does, this project's index is
/// frozen against a filesystem it no longer matches.
fn push_mass_delete_guard(out: &mut String, p: Palette, project: &ProjectStatus) {
    let Some(trip) = project.mass_delete_guard else {
        return;
    };
    let _ = writeln!(
        out,
        "    {}",
        blocked(
            p,
            &format!(
                "INDEX BLOCKED: {trip}; re-run with `lore index {name} --allow-mass-delete` if \
                 that is intended",
                name = project.name,
            )
        )
    );
}

/// Who is pushing this project, and whether anything is staged (D-0015).
///
/// Silent for a project nobody has a lease on, which is every purely local
/// project: local indexing never takes a lease, and a line saying so on every
/// project would be noise. Loud when a lease exists, because takeover degrades
/// sustained contention into epoch churn — and churn is only diagnosable if
/// the epoch is visible.
fn push_lease_state(out: &mut String, p: Palette, project: &ProjectStatus) {
    let Some(epoch) = project.push_lease_epoch else {
        return;
    };
    let _ = writeln!(
        out,
        "    {} {}{}",
        p.dim("push"),
        p.dim(&format!("lease held at epoch {epoch}")),
        if project.push_staged {
            p.dim("  (content staged, not yet committed)")
        } else {
            String::new()
        },
    );
}

/// Chunks the embed worker gave up on, and only when there are any — the same
/// silent-when-clean rule `lore-mcp`'s renderer follows, and the same reason:
/// a corpus missing some of its vectors is invisible in the results.
///
/// The remedy is the daemon log, which names what the endpoint actually said;
/// nothing on this surface can. Printed in both the long and short forms:
/// `--short` is a summary, not a quieter failure mode.
fn push_abandoned(out: &mut String, p: Palette, abandoned: u64) {
    if abandoned == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "{}",
        warn(
            p,
            &format!(
                "EMBEDDING: {abandoned} chunk(s) refused by the endpoint this daemon run and not \
                 embedded; they are retried periodically — see the daemon log for what it said"
            )
        )
    );
}

/// Where this project's files actually come from (D-0022).
///
/// Silent for a project that is simply its own root, which is nearly all of
/// them — a line saying "this project is its directory" would be noise. Loud
/// as soon as it is not, because the line above prints one root and a project
/// with mounts has files that live nowhere near it: `engine/render/pass.rs` in
/// a search result is otherwise a path the reader will go looking for under
/// the wrong directory.
///
/// A refused table is shouted about on its own line for the same reason a
/// broken `.lore.toml` profile is: the project indexed as its root alone,
/// which is a different project from the one the file described.
fn push_sources(out: &mut String, p: Palette, project: &ProjectStatus) {
    if let Some(error) = &project.sources_error {
        let _ = writeln!(
            out,
            "    {}",
            warn(
                p,
                &format!("SOURCES: {error}; this project indexed as its root alone")
            )
        );
    }
    if project.sources.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "    {} {}",
        p.dim("sources"),
        p.dim(&project.sources.len().to_string())
    );
    let width = project
        .sources
        .iter()
        .map(|source| mount_label(source).chars().count())
        .max()
        .unwrap_or(0);
    for source in &project.sources {
        let label = mount_label(source);
        let pad = width - label.chars().count();
        let _ = writeln!(
            out,
            "      {label}{blank}  {root}",
            label = p.dim(&label),
            blank = " ".repeat(pad),
            root = p.dim(&source.root),
        );
    }
}

/// How a source's prefix reads in the listing. The root source has no prefix
/// at all, and printing an empty column for it would look like a defect rather
/// than the meaning.
fn mount_label(source: &lore_core::SourceInfo) -> String {
    if source.mount.is_empty() {
        "(project root)".to_string()
    } else {
        format!("{}/", source.mount)
    }
}

/// The repository's authority profile, its health, and its decision corpus.
///
/// Always printed, including the "none" case: which repositories participate
/// in authority semantics at all is a per-repository choice (D-0012), and a
/// reader who cannot see the choice cannot tell an opted-out repo from a
/// broken one. A config error is shouted about on its own line, because the
/// repo is indexing under a *different model* than its file asked for.
fn push_authority(out: &mut String, p: Palette, project: &ProjectStatus) {
    if let Some(error) = &project.authority_config_error {
        let _ = writeln!(
            out,
            "    {}",
            warn(
                p,
                &format!(
                    "AUTHORITY CONFIG: {error}; this project indexes with no authority semantics"
                )
            )
        );
    }
    let Some(profile) = &project.authority_profile else {
        if project.authority_config_error.is_none() {
            let _ = writeln!(
                out,
                "    {} {}",
                p.dim("authority"),
                p.dim("none (no .lore.toml profile)")
            );
        }
        return;
    };
    let behavior = project.authority_behavior.as_deref().unwrap_or("annotate");
    let _ = writeln!(
        out,
        "    {} {} {} {}{}",
        p.dim("authority"),
        p.dim(&format!("{profile} ({behavior})")),
        p.dim("· decisions"),
        p.dim(&format!(
            "{}/{} active",
            project.decisions_active, project.decisions_total
        )),
        match project.decision_violations.len() {
            0 => String::new(),
            n => format!("  {}", p.yellow(&format!("{n} record violation(s)"))),
        },
    );
    for violation in &project.decision_violations {
        let _ = writeln!(out, "      {}", p.dim(violation));
    }
}

/// Documents declaring `decided` that Lore refused to honor. Silent when there
/// are none; loud when there are, on the same reasoning as the watch note:
/// otherwise the demotion is invisible and the author keeps believing those
/// files are canon.
fn push_authority_violations(out: &mut String, p: Palette, project: &ProjectStatus) {
    if project.authority_violations == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "    {}",
        warn(
            p,
            &format!(
                "AUTHORITY: {n} file(s) declare `decided` without citing an active decision; \
                 they rank as neutral",
                n = project.authority_violations,
            )
        )
    );
    for path in &project.authority_violation_paths {
        let _ = writeln!(out, "      {}", p.dim(path));
    }
    let listed = project.authority_violation_paths.len() as u64;
    if project.authority_violations > listed {
        let _ = writeln!(
            out,
            "      {}",
            p.dim(&format!(
                "... and {} more",
                project.authority_violations - listed
            ))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// `SearchArgs` is a flattened `Args` on a subcommand variant; this wrapper
    /// lets the mapping be tested through real argv parsing rather than through
    /// a hand-built struct that could not fail the way clap can.
    #[derive(Parser)]
    struct SearchCli {
        #[command(flatten)]
        args: SearchArgs,
    }

    fn parse_args(argv: &[&str]) -> SearchArgs {
        SearchCli::try_parse_from(argv)
            .expect("args should parse")
            .args
    }

    /// The give-up path, which the 180-second constant makes untestable in
    /// place: waiting on a server that never arrives has to *end*, and it has
    /// to end by returning rather than by failing — the daemon starts either
    /// way, lexical-only, which is the designed degradation (D-0007).
    #[tokio::test]
    async fn waiting_for_an_endpoint_that_never_arrives_gives_up_and_returns() {
        // Port 1 refuses on connect, so each probe fails immediately and the
        // loop is bounded by the deadline rather than by a socket timeout.
        let client = lore::embed::EmbedClient::new(lore::embed::EmbedSettings {
            retry: lore::embed::RetryPolicy {
                max_attempts: 1,
                ..Default::default()
            },
            request_timeout: EMBED_PROBE_TIMEOUT,
            ..lore::embed::EmbedSettings::from_config(&lore::config::EmbeddingsConfig {
                endpoint: Some("http://127.0.0.1:1/v1".into()),
                ..Default::default()
            })
            .expect("a configured endpoint yields settings")
        })
        .expect("probe client builds");

        let timeout = Duration::from_millis(500);
        let started = std::time::Instant::now();
        assert!(!await_embed_ready(&client, timeout).await);
        assert!(
            started.elapsed() < timeout * 10,
            "the wait outlived its own deadline by an order of magnitude"
        );
    }

    #[test]
    fn every_flag_maps_onto_the_wire_request() {
        let request = SearchRequest::from(&parse_args(&[
            "lore-search",
            "chunk boundaries",
            "--project",
            "lore",
            "--path-prefix",
            "design/",
            "--language",
            "markdown",
            "--status",
            "decided,leaning",
            "--limit",
            "5",
        ]));

        assert_eq!(request.query, "chunk boundaries");
        assert_eq!(request.project.as_deref(), Some("lore"));
        assert_eq!(request.path_prefix.as_deref(), Some("design/"));
        assert_eq!(request.language.as_deref(), Some("markdown"));
        // One `--status` with a comma list, not five repetitions of the flag.
        assert_eq!(request.status, vec!["decided", "leaning"]);
        assert_eq!(request.limit, Some(5));
    }

    #[test]
    fn a_bare_query_sends_no_filters_at_all() {
        let args = parse_args(&["lore-search", "how does expand work"]);
        assert!(!args.json);
        let request = SearchRequest::from(&args);
        assert_eq!(request.query, "how does expand work");
        assert!(request.project.is_none());
        assert!(request.path_prefix.is_none());
        assert!(request.language.is_none());
        assert!(request.limit.is_none());
        // Absent must serialize as `[]`, which the daemon reads as "no filter".
        assert!(request.status.is_empty());
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["status"], serde_json::json!([]));
    }

    #[test]
    fn repeated_status_flags_accumulate_rather_than_overwrite() {
        let request = SearchRequest::from(&parse_args(&[
            "lore-search",
            "q",
            "--status",
            "decided",
            "--status",
            "deprecated",
        ]));
        assert_eq!(request.status, vec!["decided", "deprecated"]);
    }

    #[test]
    fn no_handshake_is_a_friendly_error_naming_lore_daemon() {
        let empty = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(empty.path().to_path_buf()).unwrap();

        let err = Client::connect_at(&data_dir).unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("the lore daemon is not running"), "{text}");
        assert!(text.contains("Start it with: lore start"), "{text}");
        assert!(text.contains("daemon.json"), "{text}");
    }

    #[test]
    fn api_version_skew_names_both_versions_instead_of_claiming_absence() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let handshake = discovery::Handshake {
            pid: 4242,
            port: 53412,
            api_version: lore_core::API_VERSION + 1,
            daemon_version: "9.9.9".into(),
            started_at: 0,
            heartbeat_at: discovery::unix_now(),
        };
        std::fs::write(
            discovery::handshake_path(&data_dir),
            serde_json::to_string(&handshake).unwrap(),
        )
        .unwrap();

        let text = Client::connect_at(&data_dir).unwrap_err().to_string();
        assert!(text.contains("speaks API v2"), "{text}");
        assert!(text.contains("this build speaks v1"), "{text}");
        assert!(!text.contains("not running"), "{text}");
    }

    #[test]
    fn a_live_handshake_resolves_to_the_loopback_base_url() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let handshake = discovery::Handshake {
            pid: 4242,
            port: 53412,
            api_version: lore_core::API_VERSION,
            daemon_version: "0.1.0".into(),
            started_at: 0,
            heartbeat_at: discovery::unix_now(),
        };
        std::fs::write(
            discovery::handshake_path(&data_dir),
            serde_json::to_string(&handshake).unwrap(),
        )
        .unwrap();

        let client = Client::connect_at(&data_dir).unwrap();
        assert_eq!(client.base_url, "http://127.0.0.1:53412/v1");
        assert!(client.heartbeat_fresh);
    }

    /// The half of `lore stop` that makes it worth having (#8): it does not
    /// report success until the daemon is actually gone, and it gives up
    /// honestly rather than waiting forever.
    #[tokio::test]
    async fn stop_waits_for_the_handshake_to_disappear_and_admits_a_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let publish = |pid: u32| {
            std::fs::write(
                discovery::handshake_path(&data_dir),
                serde_json::to_string(&discovery::Handshake {
                    pid,
                    port: 53412,
                    api_version: lore_core::API_VERSION,
                    daemon_version: "0.1.0".into(),
                    started_at: 0,
                    heartbeat_at: discovery::unix_now(),
                })
                .unwrap(),
            )
            .unwrap();
        };
        publish(4242);
        let client = Client::connect_at(&data_dir).unwrap();
        assert_eq!(client.pid, 4242);

        // Still there when the deadline passes: no success is claimed, and the
        // caller gets the failure rather than a hang.
        let waited = std::time::Instant::now();
        assert!(!client.await_gone(Duration::from_millis(250)).await.unwrap());
        assert!(waited.elapsed() >= Duration::from_millis(250));

        // Withdrawn — what a clean shutdown does last — is gone.
        std::fs::remove_file(discovery::handshake_path(&data_dir)).unwrap();
        assert!(client.await_gone(Duration::from_secs(5)).await.unwrap());

        // A successor publishing over it is also gone, for our purposes: the
        // daemon we asked to stop is no longer the one being discovered.
        publish(9999);
        assert!(client.await_gone(Duration::from_secs(5)).await.unwrap());
    }

    #[test]
    fn add_makes_a_relative_path_absolute_without_touching_the_filesystem() {
        let resolved = absolute_utf8("some/nonexistent/dir").unwrap();
        assert!(
            resolved.is_absolute(),
            "the daemon cannot resolve a path relative to *our* cwd: {resolved}"
        );
        assert!(
            resolved.as_str().ends_with("nonexistent\\dir")
                || resolved.as_str().ends_with("nonexistent/dir")
        );
        // Already-absolute input survives unchanged in shape.
        let absolute = absolute_utf8(resolved.as_str()).unwrap();
        assert_eq!(absolute, resolved);
    }

    // -- `lore add` naming --------------------------------------------------

    /// A temp project root, plus the `.lore.toml` it ships (if any).
    fn repo(config: Option<&str>) -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8 tempdir");
        if let Some(config) = config {
            std::fs::write(root.join(repo_config::REPO_CONFIG_FILE), config).expect("write config");
        }
        (dir, root)
    }

    fn resolved(root: &Utf8Path, flag: Option<&str>) -> Result<String> {
        resolve_name(root, flag, &repo_config::declared_name(root)?)
    }

    #[test]
    fn the_flag_wins_then_the_committed_name_then_the_directory() {
        // Nothing committed: the directory's own name, which is what a user
        // adding a repo without ceremony expects to see in `lore status`.
        let (_dir, root) = repo(None);
        let basename = root.file_name().unwrap().to_string();
        assert_eq!(resolved(&root, None).unwrap(), basename);
        assert_eq!(resolved(&root, Some("chosen")).unwrap(), "chosen");

        // A committed name beats the directory — that is the whole point of
        // committing it: two checkouts of one repo answer to one name.
        let (_dir, root) = repo(Some("[project]\nname = \"lore\"\n"));
        assert_eq!(resolved(&root, None).unwrap(), "lore");

        // …and a table with no name falls through rather than erroring.
        let (_dir, root) = repo(Some("[project]\n"));
        assert_eq!(
            resolved(&root, None).unwrap(),
            root.file_name().unwrap().to_string()
        );

        // Flags are trimmed, since a shell quote easily adds a space.
        let (_dir, root) = repo(None);
        assert_eq!(resolved(&root, Some("  spaced  ")).unwrap(), "spaced");
    }

    /// The file is the repo's own answer to "what is this called". Overriding
    /// it silently would leave the registry and the repo disagreeing with no
    /// sign of it, so the flag that contradicts it is refused.
    #[test]
    fn a_flag_contradicting_the_committed_name_is_refused_with_both_options() {
        let (_dir, root) = repo(Some("[project]\nname = \"lore\"\n"));
        let err = resolved(&root, Some("something-else"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already names this project `lore`"), "{err}");
        assert!(err.contains("Drop --name"), "{err}");
        assert!(err.contains("edit"), "{err}");
        assert!(err.contains(repo_config::REPO_CONFIG_FILE), "{err}");

        // Restating the name it already has is agreement, not a conflict.
        assert_eq!(resolved(&root, Some("lore")).unwrap(), "lore");
    }

    #[test]
    fn a_broken_lore_toml_is_never_written_into() {
        let (_dir, root) = repo(Some("[project\nname ="));
        let err = resolved(&root, Some("lore")).unwrap_err().to_string();
        assert!(err.contains("Fix the file"), "{err}");
        // Untouched: refusing is the point.
        let text = std::fs::read_to_string(root.join(repo_config::REPO_CONFIG_FILE)).unwrap();
        assert_eq!(text, "[project\nname =");
    }

    #[test]
    fn names_are_permissive_but_never_break_a_downstream() {
        for good in [
            "lore",
            "my design vault",
            "Lexomancy-bench",
            "lore.v2",
            "日本語",
        ] {
            validate_name(good).unwrap_or_else(|err| panic!("{good:?} should be legal: {err}"));
        }
        for (bad, expected) in [
            ("", "empty"),
            ("   ", "empty"),
            ("a/b", "path separator"),
            (r"a\b", "path separator"),
            ("a\nb", "control characters"),
            ("a\tb", "control characters"),
        ] {
            let err = validate_name(bad).unwrap_err().to_string();
            assert!(err.contains(expected), "{bad:?}: {err}");
            assert!(err.contains("--name"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn an_absent_lore_toml_is_created_with_a_header_and_the_name() {
        let (_dir, root) = repo(None);
        let note = commit_name(&root, "my design vault", &DeclaredName::Absent).unwrap();
        assert!(note.unwrap().starts_with("wrote "));

        let text = std::fs::read_to_string(root.join(repo_config::REPO_CONFIG_FILE)).unwrap();
        assert!(text.starts_with("# Lore project configuration"), "{text:?}");
        assert!(
            text.contains("[project]\nname = \"my design vault\"\n"),
            "{text:?}"
        );
        // What was written reads back as what was meant, quoting included.
        assert_eq!(
            repo_config::parse_declared_name(&text),
            Ok(DeclaredName::Named("my design vault".into()))
        );
        // …and it is still a neutral repo: naming is not an authority opt-in.
        assert_eq!(
            lore::repo_config::RepoAuthority::parse(&text),
            lore::repo_config::RepoAuthority::default()
        );
    }

    /// Appending is textual on purpose: re-serializing through `toml` would
    /// discard the user's comments and key order to add one line.
    #[test]
    fn an_existing_lore_toml_is_appended_to_byte_for_byte() {
        let original =
            "# hand-written, do not lose me\n\n[authority]\n# and this\nprofile = \"lore-v1\"\n";
        let (_dir, root) = repo(Some(original));
        let note = commit_name(&root, "lore", &DeclaredName::NoTable).unwrap();
        assert!(note.unwrap().contains("named the project"));

        let text = std::fs::read_to_string(root.join(repo_config::REPO_CONFIG_FILE)).unwrap();
        assert!(
            text.starts_with(original),
            "the original survives: {text:?}"
        );
        assert!(text.contains("# hand-written, do not lose me"));
        assert!(text.contains("# and this"));
        assert_eq!(
            repo_config::parse_declared_name(&text),
            Ok(DeclaredName::Named("lore".into()))
        );
        // The profile it already declared is untouched.
        assert!(lore::repo_config::RepoAuthority::parse(&text).annotates());
    }

    #[test]
    fn a_file_that_already_names_the_project_is_left_alone() {
        let original = "[project]\nname = \"lore\"\n";
        let (_dir, root) = repo(Some(original));
        let note = commit_name(&root, "lore", &DeclaredName::Named("lore".into())).unwrap();
        assert_eq!(note, None, "nothing to say and nothing to write");
        assert_eq!(
            std::fs::read_to_string(root.join(repo_config::REPO_CONFIG_FILE)).unwrap(),
            original
        );
    }

    /// A `[project]` table with no `name` cannot get a second one appended, and
    /// editing inside it is the user's call. The registration still stands; the
    /// user is told what to add so the name follows the repo.
    #[test]
    fn an_unnamed_project_table_is_reported_rather_than_rewritten() {
        let original = "[project]\n";
        let (_dir, root) = repo(Some(original));
        let note = commit_name(&root, "lore", &DeclaredName::Unnamed)
            .unwrap()
            .expect("the user is told");
        assert!(note.contains("no `name`"), "{note}");
        assert!(note.contains("name = \"lore\""), "{note}");
        assert_eq!(
            std::fs::read_to_string(root.join(repo_config::REPO_CONFIG_FILE)).unwrap(),
            original
        );
    }

    #[test]
    fn a_name_needing_escaping_round_trips_through_toml() {
        for name in ["quote\"inside", "back\\slash-free", "emoji 🙂", "lore"] {
            let rendered = format!("[project]\nname = {}\n", toml_string(name));
            assert_eq!(
                repo_config::parse_declared_name(&rendered),
                Ok(DeclaredName::Named(name.to_string())),
                "{name:?} -> {rendered:?}"
            );
        }
    }

    // -- rendering ---------------------------------------------------------
    // Kept in step with `lore-mcp/src/render.rs`; the assertions below are the
    // shape the two are agreed on.

    /// A realistic 64-character chunk id starting with `head` — the same
    /// helper `lore-mcp`'s renderer tests use, and for the same reason: a
    /// fixture shorter than what is printed would test nothing.
    fn full_id(head: &str) -> String {
        let mut id = head.to_string();
        while id.len() < 64 {
            id.push_str("0123456789abcdef");
        }
        id.truncate(64);
        id
    }

    fn vault_hit() -> SearchResult {
        SearchResult {
            chunk_id: full_id("9f3a1c2b7e4d"),
            project: "lore".into(),
            project_key: "lore".into(),
            path: "design/4_Interfaces/4.1_MCP_Surface.md".into(),
            line_start: 15,
            line_end: 17,
            language: Some("markdown".into()),
            symbol_path: None,
            heading_path: Some(vec!["MCP Tool Surface".into(), "v0.1 tools".into()]),
            design_status: Some("decided".into()),
            effective_authority: Some("decided".into()),
            authority_note: None,
            decision_refs: vec!["D-0007".into()],
            score: 0.8741,
            excerpt: "- **`search`** - one unified hybrid query.\n".into(),
            excerpt_truncated: false,
        }
    }

    fn code_hit() -> SearchResult {
        SearchResult {
            chunk_id: full_id("4e77ba0193ab"),
            project: "lexomancy".into(),
            project_key: "lexomancy".into(),
            path: "Assets/Scripts/Board.cs".into(),
            line_start: 120,
            line_end: 141,
            language: Some("csharp".into()),
            symbol_path: Some("Board.Update".into()),
            heading_path: None,
            design_status: None,
            effective_authority: Some("neutral".into()),
            authority_note: None,
            decision_refs: vec![],
            score: 0.612,
            excerpt: "void Update() {".into(),
            excerpt_truncated: true,
        }
    }

    #[test]
    fn a_vault_hit_shows_authority_heading_path_and_chunk_id() {
        let rendered = render_search(
            "authority",
            &SearchResponse {
                results: vec![vault_hit()],
                lexical_only: false,
            },
        );
        assert!(rendered.starts_with("1 result(s) for \"authority\" (hybrid)\n"));
        assert!(rendered.contains(
            "[1] lore  design/4_Interfaces/4.1_MCP_Surface.md:15-17  score 0.874  [markdown]\n"
        ));
        assert!(rendered.contains("    heading: MCP Tool Surface > v0.1 tools\n"));
        assert!(rendered.contains("    status: decided  refs: D-0007\n"));
        assert!(rendered.contains("    chunk_id: 9f3a1c2b7e4d\n"));
    }

    /// The CLI half of issue #9's `symbol: #s0`. Kept in step with
    /// `lore-mcp/src/render.rs`, which asserts the same table.
    #[test]
    fn chunker_discriminators_are_display_only() {
        assert_eq!(display_symbol("#s0"), "top-level statements");
        assert_eq!(display_symbol("Board.#s2"), "Board (statements)");
        assert_eq!(display_symbol("Parser.Parse#w1"), "Parser.Parse");
        assert_eq!(display_symbol("Board.Update"), "Board.Update");
        assert_eq!(display_symbol("Counter.#count"), "Counter.#count");
        assert_eq!(
            display_headings(&["Ranking".to_string(), "#w0".to_string()]),
            ["Ranking"]
        );

        let mut filler = code_hit();
        filler.symbol_path = Some("#s0".into());
        let rendered = render_search(
            "statements",
            &SearchResponse {
                results: vec![filler],
                lexical_only: false,
            },
        );
        assert!(
            rendered.contains("    symbol: top-level statements\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("#s0"), "{rendered}");
    }

    /// Same rule as `lore-mcp`'s renderer: twelve characters, never sixty-four,
    /// and what is printed is a prefix `expand` accepts.
    #[test]
    fn chunk_ids_print_short_enough_to_read_and_long_enough_to_use() {
        let hit = vault_hit();
        let rendered = render_search(
            "authority",
            &SearchResponse {
                results: vec![hit.clone()],
                lexical_only: false,
            },
        );
        assert!(!rendered.contains(&hit.chunk_id), "{rendered}");
        assert_eq!(short_id(&hit.chunk_id).len(), SHORT_CHUNK_ID);
        assert!(short_id(&hit.chunk_id).len() >= lore_core::MIN_CHUNK_ID_PREFIX);
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn a_code_hit_shows_its_symbol_and_flags_truncation() {
        let rendered = render_search(
            "update",
            &SearchResponse {
                results: vec![code_hit()],
                lexical_only: false,
            },
        );
        assert!(rendered.contains("    symbol: Board.Update\n"));
        assert!(rendered.contains("    chunk_id: 4e77ba0193ab\n"));
        assert!(rendered.contains("    (excerpt truncated)\n"));
        // Vault fields are omitted, not rendered empty.
        assert!(!rendered.contains("status:"));
        assert!(!rendered.contains("heading:"));
    }

    #[test]
    fn empty_results_report_the_degradation_that_may_explain_them() {
        let rendered = render_search(
            "nothing",
            &SearchResponse {
                results: vec![],
                lexical_only: true,
            },
        );
        assert_eq!(
            rendered,
            format!("no results for \"nothing\" (lexical-only)\n{LEXICAL_ONLY_NOTE}")
        );
        assert!(rendered.contains("lore status"));
    }

    #[test]
    fn status_names_all_three_embedding_states_distinctly() {
        let p = Palette::plain();
        assert!(embedding_row(p, &EmbeddingStatus::Unconfigured).contains("UNCONFIGURED"));
        let unreachable = embedding_row(
            p,
            &EmbeddingStatus::Unreachable {
                endpoint: "http://127.0.0.1:11434".into(),
                error: "connection refused".into(),
            },
        );
        assert!(unreachable.contains("UNREACHABLE"), "{unreachable}");
        assert!(
            unreachable.contains("http://127.0.0.1:11434"),
            "{unreachable}"
        );
        assert!(
            unreachable.contains("(connection refused)"),
            "{unreachable}"
        );

        let ready = embedding_row(
            p,
            &EmbeddingStatus::Ready {
                endpoint: "http://127.0.0.1:11434".into(),
                model: "nomic-embed-text".into(),
            },
        );
        assert!(ready.contains("ready"), "{ready}");
        assert!(ready.contains("nomic-embed-text"), "{ready}");
        // The three states must not be distinguishable only by colour: a
        // NO_COLOR reader gets exactly this string.
        assert!(!ready.contains("UNREACHABLE"), "{ready}");
    }

    /// A daemon carrying exactly the projects described, everything else bare.
    fn daemon_with(projects: Vec<(u64, u64)>) -> DaemonStatus {
        DaemonStatus {
            api_version: 1,
            daemon_version: "0.1.0".into(),
            generation: 0,
            projects: projects
                .into_iter()
                .enumerate()
                .map(|(i, (embedded, chunks))| ProjectStatus {
                    name: format!("p{i}"),
                    root: format!(r"C:\repos\p{i}"),
                    chunks,
                    embedded_chunks: embedded,
                    ..ProjectStatus::default()
                })
                .collect(),
            embeddings: EmbeddingStatus::Ready {
                endpoint: "http://127.0.0.1:8000/v1".into(),
                model: "Qwen/Qwen3-Embedding-4B".into(),
            },
            latency: Vec::new(),
            embed_abandoned: 0,
            plugins: Vec::new(),
            plugin_diagnostics: Vec::new(),
        }
    }

    #[test]
    fn fleet_coverage_weights_by_chunk_not_by_project() {
        // A tiny finished project must not drag a huge unfinished one upward:
        // the mean of the percentages here is 50%, the honest figure is 1%.
        let rendered = render_status(&daemon_with(vec![(24, 24), (0, 18_504)]), false);
        assert!(rendered.contains("24/18,528 chunks"), "{rendered}");
        assert!(rendered.contains("0% embedded"), "{rendered}");
        assert!(!rendered.contains("50%"), "{rendered}");
    }

    #[test]
    fn fleet_coverage_sums_every_project_on_the_header_line() {
        let rendered = render_status(&daemon_with(vec![(10, 20), (30, 40)]), false);
        // Floored, not rounded: 66.67% is not yet 67% of the way done.
        assert!(
            rendered.contains("2 projects · 40/60 chunks ·"),
            "{rendered}"
        );
        assert!(rendered.contains("66% embedded"), "{rendered}");
    }

    #[test]
    fn a_registry_with_no_projects_has_no_coverage_to_divide_by() {
        // Must not panic or print NaN%: `coverage` short-circuits at zero.
        let rendered = render_status(&daemon_with(Vec::new()), true);
        assert!(rendered.contains("0 projects · 0/0 chunks"), "{rendered}");
        assert!(rendered.contains("0% embedded"), "{rendered}");
        assert!(!rendered.contains("NaN"), "{rendered}");
    }

    #[test]
    fn short_keeps_the_verdict_lines_and_drops_the_per_project_table() {
        let full = daemon_with(vec![(10, 20), (30, 40)]);
        let short = render_status(&full, true);

        // Kept: is the daemon up, are embeddings working, how far along is it.
        assert!(short.starts_with("lore 0.1.0 api v1 · gen 0\n"), "{short}");
        assert!(short.contains("http://127.0.0.1:8000/v1"), "{short}");
        assert!(short.contains("2 projects · 40/60 chunks ·"), "{short}");

        // Dropped: the per-project rows are the whole point of the long form.
        assert!(!short.contains(r"C:\repos\p0"), "{short}");
        assert!(!short.contains("files 0"), "{short}");
        assert_eq!(short.lines().count(), 3, "{short}");

        // No banner and no frame either: --short is for the reader who wants
        // the verdict without the ceremony.
        assert!(!short.contains('╭'), "{short}");
        assert!(!short.contains("██╗"), "{short}");
    }

    #[test]
    fn short_still_reports_a_stalled_embedding_backlog() {
        // --short is a summary, not a quieter failure mode: the abandoned-chunk
        // warning outranks brevity.
        let mut status = daemon_with(vec![(10, 20)]);
        status.embed_abandoned = 19;
        let short = render_status(&status, true);
        assert!(short.contains("EMBEDDING: 19 chunk(s)"), "{short}");
    }

    #[test]
    fn an_empty_registry_points_at_the_command_that_fixes_it() {
        let rendered = render_status(
            &DaemonStatus {
                api_version: 1,
                daemon_version: "0.1.0".into(),
                generation: 0,
                projects: vec![],
                embeddings: EmbeddingStatus::Unconfigured,
                latency: Vec::new(),
                embed_abandoned: 0,
                plugins: Vec::new(),
                plugin_diagnostics: Vec::new(),
            },
            false,
        );
        assert!(rendered.contains("no projects registered"), "{rendered}");
        assert!(rendered.contains("lore add <path>"), "{rendered}");
        // The long form opens with the wordmark; the daemon's identity moved
        // into the frame's title and right rule.
        assert!(rendered.starts_with("██╗"), "{rendered}");
        assert!(rendered.contains("╭─ lore 0.1.0 "), "{rendered}");
        assert!(rendered.contains("api v1 · gen 0 ─╮"), "{rendered}");
    }

    /// The CLI half of the same rule as `lore-mcp`'s renderer: silent when the
    /// run is clean, loud when the endpoint refused chunks (#9).
    #[test]
    fn abandoned_chunks_are_reported_only_when_there_are_any() {
        let body = |abandoned: u64| DaemonStatus {
            api_version: 1,
            daemon_version: "0.1.0".into(),
            generation: 1,
            projects: vec![],
            embeddings: EmbeddingStatus::Unconfigured,
            latency: Vec::new(),
            embed_abandoned: abandoned,
            plugins: Vec::new(),
            plugin_diagnostics: Vec::new(),
        };
        assert!(!render_status(&body(0), false).contains("EMBEDDING"));

        let rendered = render_status(&body(19), false);
        assert!(rendered.contains("EMBEDDING: 19 chunk(s)"), "{rendered}");
        assert!(rendered.contains("daemon log"), "{rendered}");
    }

    #[test]
    fn coverage_floors_so_that_a_hundred_percent_means_complete() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(0, 9134), 0);
        assert_eq!(percent(1204, 1204), 100);
        // The case the whole rule exists for: one chunk short of done must not
        // round up into a claim of completeness.
        assert_eq!(percent(9133, 9134), 99);
        assert_eq!(percent(55_930, 55_931), 99);
    }

    #[test]
    fn the_bar_fills_completely_only_when_the_corpus_is_complete() {
        let p = Palette::plain();
        assert_eq!(bar(p, 9134, 9134), "████████████");
        // 99.99% still shows an unfilled cell, matching the floored percentage.
        assert!(bar(p, 9133, 9134).ends_with('░'), "{}", bar(p, 9133, 9134));
        assert_eq!(bar(p, 0, 9134), "░░░░░░░░░░░░");
        // An empty project has nothing to be a fraction of.
        assert_eq!(bar(p, 0, 0), "░░░░░░░░░░░░");
    }

    #[test]
    fn counts_over_a_thousand_are_separated() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_204), "1,204");
        assert_eq!(commas(55_931), "55,931");
        assert_eq!(commas(1_000_000), "1,000,000");
    }

    #[test]
    fn escape_sequences_take_no_columns() {
        let p = Palette { color: true };
        let painted = p.green("ready");
        assert!(painted.len() > "ready".len(), "not actually painted");
        assert_eq!(vis_len(&painted), 5);
        assert_eq!(vis_len("plain"), 5);
    }

    #[test]
    fn a_frame_is_square_whether_or_not_its_rows_are_painted() {
        // The panel measures pre-styled rows, so a mis-measure here shows up as
        // a ragged right edge only when colour is on — the case tests miss.
        for color in [false, true] {
            let p = Palette { color };
            let rows = vec![
                embedding_row(
                    p,
                    &EmbeddingStatus::Ready {
                        endpoint: "http://127.0.0.1:8000/v1".into(),
                        model: "Qwen/Qwen3-Embedding-4B".into(),
                    },
                ),
                p.dim("a shorter row"),
            ];
            let framed = panel(&p.bold("lore 0.1.0"), &p.dim("api v1 · gen 7"), &rows);
            let widths: Vec<usize> = framed.lines().map(vis_len).collect();
            assert!(
                widths.windows(2).all(|w| w[0] == w[1]),
                "ragged frame at color={color}: {widths:?}"
            );
        }
    }

    #[test]
    fn the_palette_obeys_no_color() {
        let plain = Palette { color: false };
        assert_eq!(plain.green("ready"), "ready");
        assert_eq!(plain.dim("quiet"), "quiet");
        assert!(!render_status(&daemon_with(vec![(1, 2)]), false).contains(''));
    }

    /// The `status` half of "degradation is never silent" (1d). A refused
    /// `decided` declaration is invisible everywhere else unless the author
    /// happens to search for that exact file, so `lore status` has to name the
    /// offenders — and say how many it did not name, or a vault with fifty
    /// violations looks like a vault with five.
    #[test]
    fn status_names_the_refused_declarations_and_admits_what_it_truncated() {
        let project = |violations: u64, paths: Vec<String>| DaemonStatus {
            api_version: 1,
            daemon_version: "0.1.0".into(),
            generation: 3,
            projects: vec![ProjectStatus {
                id: 1,
                name: "lore".into(),
                key: "lore".into(),
                root: r"C:\repos\lore".into(),
                kind: "repo".into(),
                files: 96,
                chunks: 1204,
                embedded_chunks: 1204,
                authority_violations: violations,
                authority_violation_paths: paths,
                authority_profile: Some("lore-v1".into()),
                authority_behavior: Some("rank".into()),
                watch: WatchState::Armed,
                ..ProjectStatus::default()
            }],
            embeddings: EmbeddingStatus::Unconfigured,
            latency: Vec::new(),
            embed_abandoned: 0,
            plugins: Vec::new(),
            plugin_diagnostics: Vec::new(),
        };

        // Clean vault: not a word about authority.
        assert!(!render_status(&project(0, Vec::new()), false).contains("AUTHORITY"));

        let rendered = render_status(
            &project(2, vec!["design/a.md".into(), "design/b.md".into()]),
            false,
        );
        assert!(rendered.contains("AUTHORITY: 2 file(s)"), "{rendered}");
        assert!(rendered.contains("\n      design/a.md\n"), "{rendered}");
        assert!(rendered.contains("\n      design/b.md\n"), "{rendered}");
        assert!(
            !rendered.contains("more"),
            "nothing was truncated: {rendered}"
        );

        // The daemon caps the list; the count is the complete figure, so the
        // difference has to be stated rather than quietly dropped.
        let truncated = render_status(
            &project(9, vec!["design/a.md".into(), "design/b.md".into()]),
            false,
        );
        assert!(truncated.contains("... and 7 more"), "{truncated}");
    }

    /// The two ways a chunker plugin disappoints, and the one way it works.
    ///
    /// All three are invisible in search results — a plugin that never ran
    /// produces a perfectly ordinary index of line windows — so `status` is the
    /// only place the difference exists. And silent when there is nothing to
    /// say: a project that enabled no plugins gets no plugin lines at all.
    #[test]
    fn status_reports_plugins_only_where_there_is_something_to_report() {
        let daemon = |plugins: Vec<lore_core::PluginInfo>,
                      diagnostics: Vec<String>,
                      project: ProjectStatus| DaemonStatus {
            api_version: 1,
            daemon_version: "0.1.0".into(),
            generation: 3,
            projects: vec![project],
            embeddings: EmbeddingStatus::Unconfigured,
            latency: Vec::new(),
            embed_abandoned: 0,
            plugins,
            plugin_diagnostics: diagnostics,
        };
        let unity = || lore_core::PluginInfo {
            name: "unity".into(),
            fingerprint: "6f1d2a3b4c5d6e7f8091a2b3c4d5e6f7".into(),
            extensions: vec!["uxml".into(), "uss".into()],
        };
        let bare = ProjectStatus {
            id: 1,
            name: "lore".into(),
            root: r"C:\repos\lore".into(),
            ..ProjectStatus::default()
        };

        // A machine with no plugins and a project that enabled none: silence.
        let quiet = render_status(&daemon(Vec::new(), Vec::new(), bare.clone()), false);
        assert!(!quiet.contains("plugin"), "{quiet}");
        assert!(!quiet.contains("PLUGIN"), "{quiet}");

        // Installed and in force: named, with a fingerprint short enough to
        // compare by eye and long enough to be a handle.
        let working = render_status(
            &daemon(
                vec![unity()],
                Vec::new(),
                ProjectStatus {
                    plugins_enabled: vec![unity()],
                    ..bare.clone()
                },
            ),
            false,
        );
        assert!(
            working.contains("plugins      unity 6f1d2a3b4c5d (uxml, uss)"),
            "{working}"
        );
        assert!(
            working.contains("    plugins unity 6f1d2a3b4c5d\n"),
            "{working}"
        );
        assert!(!working.contains("PLUGIN"), "nothing is wrong: {working}");

        // Enabled but never installed: the files are indexed, just not the way
        // the repository asked for, so the remedy is named.
        let missing = render_status(
            &daemon(
                Vec::new(),
                Vec::new(),
                ProjectStatus {
                    plugins_missing: vec!["unity".into()],
                    ..bare.clone()
                },
            ),
            false,
        );
        assert!(
            missing.contains("PLUGINS: unity enabled in .lore.toml but not installed"),
            "{missing}"
        );
        assert!(missing.contains("lore plugin add"), "{missing}");

        // Installed, enabled, and unable to run: the count of what fell back,
        // plus the machine-wide diagnostic that says why.
        let broken = render_status(
            &daemon(
                vec![unity()],
                vec!["plugin \"unity\" grammar `xml.wasm` is unavailable".into()],
                ProjectStatus {
                    plugins_enabled: vec![unity()],
                    plugin_fallback_files: 12,
                    ..bare
                },
            ),
            false,
        );
        assert!(
            broken.contains("PLUGIN: plugin \"unity\" grammar `xml.wasm` is unavailable"),
            "{broken}"
        );
        assert!(
            broken.contains("PLUGINS: 12 file(s) fell back to the built-in chunker"),
            "{broken}"
        );
    }

    /// Installing is a filesystem act with two refusals, and both of them exist
    /// to keep a plugin's *identity* meaningful: a name is unique, and the
    /// fingerprint is the version.
    #[test]
    fn installing_a_plugin_validates_it_and_refuses_to_replace_a_different_one() {
        let dir = tempfile::tempdir().unwrap();
        let dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let plugins = dir.join("plugins");
        let source = dir.join("src/toy");
        std::fs::create_dir_all(&source).unwrap();
        let manifest = |extra: &str| {
            format!(
                "[plugin]\nname = \"toy\"\n\n[[chunker]]\nextensions = [\"toydata\"]\n\
                 strategy = \"windows\"\nmax_file_bytes = 8192\n{extra}"
            )
        };
        std::fs::write(source.join("lore-plugin.toml"), manifest("")).unwrap();

        let installed = install_plugin(&source, &plugins).expect("a valid plugin installs");
        assert_eq!(installed.name, "toy");
        assert_eq!(installed.extensions, ["toydata"]);
        assert!(!installed.unchanged);
        assert!(plugins.join("toy/lore-plugin.toml").is_file());

        // Idempotent: the same bytes are already there, so nothing is copied
        // and nothing is refused.
        let again = install_plugin(&source, &plugins).expect("re-adding is a no-op");
        assert!(again.unchanged);
        assert_eq!(again.fingerprint, installed.fingerprint);

        // A different plugin under the same name is refused, naming both
        // fingerprints — replacing it would re-chunk every file it owns.
        std::fs::write(source.join("lore-plugin.toml"), manifest("# edited\n")).unwrap();
        let err = install_plugin(&source, &plugins).unwrap_err().to_string();
        assert!(err.contains("already installed"), "{err}");
        assert!(err.contains("Delete that directory yourself"), "{err}");
        assert_eq!(
            std::fs::read_to_string(plugins.join("toy/lore-plugin.toml")).unwrap(),
            manifest(""),
            "a refused install must not have written anything"
        );

        // Validation is loading it: a directory that is not a plugin, and a
        // manifest that does not parse, are both refused before any copying.
        let empty = dir.join("src/empty");
        std::fs::create_dir_all(&empty).unwrap();
        let err = install_plugin(&empty, &plugins).unwrap_err().to_string();
        assert!(err.contains("lore-plugin.toml"), "{err}");
        std::fs::write(empty.join("lore-plugin.toml"), "[plugin]\nname = \"E\"\n").unwrap();
        let err = install_plugin(&empty, &plugins).unwrap_err().to_string();
        assert!(err.contains("lowercase slug"), "{err}");
        assert!(!plugins.join("E").exists() && !plugins.join("e").exists());
        let err = install_plugin(&dir.join("src/nope"), &plugins)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a directory"), "{err}");
    }

    #[test]
    fn expand_renders_a_span_header_over_the_text() {
        assert_eq!(
            render_expand(
                "lore",
                &ExpandResponse {
                    path: "src/main.rs".into(),
                    line_start: 10,
                    line_end: 12,
                    text: "fn main() {}\n".into(),
                    file_lines: 57,
                }
            ),
            "lore  src/main.rs:10-12  (file has 57 lines)\nfn main() {}\n"
        );
    }
}
