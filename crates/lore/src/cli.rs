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

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use lore::daemon::ignorefile;
use lore::repo_config::{self, DeclaredName};
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

/// `lore init [path]` — write the project's `.loreignore`.
///
/// The one subcommand that needs no daemon: the file belongs to the project,
/// not to the index, and it is useful to look at (and edit) before anything
/// has been registered. Synchronous for the same reason — there is nothing to
/// await.
pub fn init(path: Option<String>) -> Result<()> {
    let root = absolute_utf8(path.as_deref().unwrap_or("."))?;
    if !root.is_dir() {
        bail!("not a directory: {root}");
    }

    match ignorefile::write_new(&root) {
        Ok(ecosystems) => {
            println!("wrote {}", ignorefile::path(&root));
            if ecosystems.is_empty() {
                println!("  no ecosystem detected; the file is a header and nothing else");
            } else {
                println!("  detected: {}", ignorefile::labels(&ecosystems));
            }
            println!("  edit it to change what lore indexes; lore will not rewrite it");
            Ok(())
        }
        // Not an accident to recover from: refusing is the promise the header
        // of a generated file makes, so the fix is the user's to make.
        Err(ignorefile::WriteError::Exists(path)) => bail!(
            "{path} already exists, and lore never rewrites it.\nDelete it and run `lore init` again to regenerate."
        ),
        Err(err) => Err(anyhow::Error::new(err)),
    }
}

/// `lore index [project]` — queue a full rescan.
pub async fn index(project: Option<String>) -> Result<()> {
    let client = Client::connect()?;
    let body = client.post("index", &IndexRequest { project }).await?;
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
pub async fn status(json: bool, project: Option<String>) -> Result<()> {
    let client = Client::connect()?;
    let route = match &project {
        Some(p) => format!("status?project={}", urlencode(p)),
        None => "status".to_string(),
    };
    let body = client.get(&route).await?;
    if json {
        println!("{body}");
        return Ok(());
    }
    print!("{}", render_status(&parse::<DaemonStatus>(&body)?));
    Ok(())
}

/// `lore search <query>` — the same query surface agents get over MCP, so a
/// human can reproduce and debug exactly what an agent saw.
///
/// Every query is scoped to one project. `--project` says which; without it
/// the daemon is asked which registered project contains the current directory
/// — the interim, local-only convenience the scoping resolution sanctions (see
/// `GET /v1/resolve`). The flag still wins, because the local CLI is the
/// admin surface: a human may deliberately query a project they are not
/// standing in, where an agent may not.
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
                    "the daemon handshake at {} is unreadable; if the daemon is not running, delete it and start it with: lore daemon",
                    discovery::handshake_path(data_dir)
                )
            })?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the lore daemon is not running (no handshake at {}).\nStart it with: lore daemon",
                    discovery::handshake_path(data_dir)
                )
            })?;

        if handshake.api_version != lore_core::API_VERSION {
            bail!(
                "the running lore daemon ({}) speaks API v{}, but this build speaks v{}.\nStop the old daemon and start this build with: lore daemon",
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
                    "the lore daemon published a handshake but is not answering at {url} ({err}).\nIt may have crashed; restart it with: lore daemon"
                )
            } else {
                anyhow::anyhow!(
                    "the lore daemon's handshake is stale and it is not answering at {url} ({err}).\nStart it with: lore daemon"
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
        let _ = writeln!(out, "    symbol: {symbol}");
    }
    if let Some(headings) = &result.heading_path
        && !headings.is_empty()
    {
        let _ = writeln!(out, "    heading: {}", headings.join(" > "));
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
    let _ = writeln!(out, "    chunk_id: {}", result.chunk_id);

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

fn render_status(status: &DaemonStatus) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "lore daemon {version}  api v{api}  generation {generation}",
        version = status.daemon_version,
        api = status.api_version,
        generation = status.generation,
    );
    let _ = writeln!(out, "{}", render_embeddings(&status.embeddings));
    for l in &status.latency {
        let _ = writeln!(
            out,
            "latency {endpoint}: p50 {p50}ms  p90 {p90}ms  p95 {p95}ms  p99 {p99}ms  max {max}ms  ({n} requests)",
            endpoint = l.endpoint,
            p50 = l.p50_ms,
            p90 = l.p90_ms,
            p95 = l.p95_ms,
            p99 = l.p99_ms,
            max = l.max_ms,
            n = l.samples,
        );
    }

    if status.projects.is_empty() {
        out.push_str("projects: none registered (run `lore add <path>`)\n");
        return out;
    }

    let _ = writeln!(out, "projects ({}):", status.projects.len());
    let width = status
        .projects
        .iter()
        .map(|project| project.name.chars().count())
        .max()
        .unwrap_or(0);
    for project in &status.projects {
        let pad = width - project.name.chars().count();
        let _ = writeln!(
            out,
            "  {name}{blank}  files {files}  chunks {chunks}  embedded {embedded}  {root}{watch}",
            name = project.name,
            blank = " ".repeat(pad),
            files = project.files,
            chunks = project.chunks,
            embedded = coverage(project.embedded_chunks, project.chunks),
            root = project.root,
            watch = watch_note(project.watch),
        );
        push_authority(&mut out, project);
        push_authority_violations(&mut out, project);
    }
    out
}

/// The repository's authority profile, its health, and its decision corpus.
///
/// Always printed, including the "none" case: which repositories participate
/// in authority semantics at all is now a per-repository choice (D-0012), and
/// a reader who cannot see the choice cannot tell an opted-out repo from a
/// broken one. A config error is shouted about on its own line, because the
/// repo is indexing under a *different model* than its file asked for.
fn push_authority(out: &mut String, project: &ProjectStatus) {
    if let Some(error) = &project.authority_config_error {
        let _ = writeln!(
            out,
            "    AUTHORITY CONFIG: {error}; this project indexes with no authority semantics"
        );
    }
    let Some(profile) = &project.authority_profile else {
        if project.authority_config_error.is_none() {
            let _ = writeln!(out, "    authority: none (no .lore.toml profile)");
        }
        return;
    };
    let behavior = project.authority_behavior.as_deref().unwrap_or("annotate");
    let _ = writeln!(
        out,
        "    authority: {profile} ({behavior})  decisions {active}/{total} active{violations}",
        active = project.decisions_active,
        total = project.decisions_total,
        violations = match project.decision_violations.len() {
            0 => String::new(),
            n => format!("  {n} record violation(s)"),
        },
    );
    for violation in &project.decision_violations {
        let _ = writeln!(out, "      {violation}");
    }
}

/// Documents declaring `decided` that Lore refused to honor. Silent when there
/// are none; loud when there are, on the same reasoning as the watch note:
/// otherwise the demotion is invisible and the author keeps believing those
/// files are canon.
fn push_authority_violations(out: &mut String, project: &ProjectStatus) {
    if project.authority_violations == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "    AUTHORITY: {n} file(s) declare `decided` without citing an active decision; \
         they rank as neutral",
        n = project.authority_violations,
    );
    for path in &project.authority_violation_paths {
        let _ = writeln!(out, "      {path}");
    }
    let listed = project.authority_violation_paths.len() as u64;
    if project.authority_violations > listed {
        let _ = writeln!(
            out,
            "      ... and {} more",
            project.authority_violations - listed
        );
    }
}

/// Silent when the watch is armed — the common case should not add noise —
/// and loud when it is not, because the failure is otherwise invisible: the
/// index simply stops keeping up.
fn watch_note(state: WatchState) -> &'static str {
    match state {
        // `Unknown` means an older daemon that cannot report; saying nothing
        // is more honest than claiming either state.
        WatchState::Armed | WatchState::Unknown => "",
        WatchState::Retrying => "  WATCH RETRYING - not indexing live; use `lore index`",
    }
}

fn coverage(embedded: u64, chunks: u64) -> String {
    if chunks == 0 {
        return format!("{embedded}/0");
    }
    let percent = (embedded as f64 / chunks as f64 * 100.0).round() as u64;
    format!("{embedded}/{chunks} ({percent}%)")
}

fn render_embeddings(status: &EmbeddingStatus) -> String {
    match status {
        EmbeddingStatus::Unconfigured => {
            "embeddings: UNCONFIGURED - no endpoint set; search is lexical-only".to_string()
        }
        EmbeddingStatus::Unreachable { endpoint, error } => format!(
            "embeddings: UNREACHABLE - {endpoint} ({error}); search is lexical-only until it answers"
        ),
        EmbeddingStatus::Ready { endpoint, model } => {
            format!("embeddings: ready - {endpoint} model {model}")
        }
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
        assert!(text.contains("Start it with: lore daemon"), "{text}");
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

    fn vault_hit() -> SearchResult {
        SearchResult {
            chunk_id: "9f3a1c2b".into(),
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
            chunk_id: "4e77ba01".into(),
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
        assert!(rendered.contains("    chunk_id: 9f3a1c2b\n"));
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
        assert!(rendered.contains("    chunk_id: 4e77ba01\n"));
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
        assert!(render_embeddings(&EmbeddingStatus::Unconfigured).contains("UNCONFIGURED"));
        assert!(
            render_embeddings(&EmbeddingStatus::Unreachable {
                endpoint: "http://127.0.0.1:11434".into(),
                error: "connection refused".into(),
            })
            .contains("UNREACHABLE - http://127.0.0.1:11434 (connection refused)")
        );
        assert!(
            render_embeddings(&EmbeddingStatus::Ready {
                endpoint: "http://127.0.0.1:11434".into(),
                model: "nomic-embed-text".into(),
            })
            .contains("ready - http://127.0.0.1:11434 model nomic-embed-text")
        );
    }

    #[test]
    fn an_empty_registry_points_at_the_command_that_fixes_it() {
        let rendered = render_status(&DaemonStatus {
            api_version: 1,
            daemon_version: "0.1.0".into(),
            generation: 0,
            projects: vec![],
            embeddings: EmbeddingStatus::Unconfigured,
            latency: Vec::new(),
        });
        assert!(rendered.contains("projects: none registered (run `lore add <path>`)"));
        assert!(rendered.starts_with("lore daemon 0.1.0  api v1  generation 0\n"));
    }

    #[test]
    fn coverage_reports_a_ratio_and_survives_an_unindexed_project() {
        assert_eq!(coverage(0, 0), "0/0");
        assert_eq!(coverage(0, 9134), "0/9134 (0%)");
        assert_eq!(coverage(1204, 1204), "1204/1204 (100%)");
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
        };

        // Clean vault: not a word about authority.
        assert!(!render_status(&project(0, Vec::new())).contains("AUTHORITY"));

        let rendered = render_status(&project(
            2,
            vec!["design/a.md".into(), "design/b.md".into()],
        ));
        assert!(rendered.contains("AUTHORITY: 2 file(s)"), "{rendered}");
        assert!(rendered.contains("\n      design/a.md\n"), "{rendered}");
        assert!(rendered.contains("\n      design/b.md\n"), "{rendered}");
        assert!(
            !rendered.contains("more"),
            "nothing was truncated: {rendered}"
        );

        // The daemon caps the list; the count is the complete figure, so the
        // difference has to be stated rather than quietly dropped.
        let truncated = render_status(&project(
            9,
            vec!["design/a.md".into(), "design/b.md".into()],
        ));
        assert!(truncated.contains("... and 7 more"), "{truncated}");
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
