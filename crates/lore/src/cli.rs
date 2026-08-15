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
use lore_core::discovery;
use lore_core::{
    DaemonStatus, EmbeddingStatus, ExpandResponse, IndexRequest, IndexResponse, ProjectInfo,
    RegisterProjectRequest, SearchRequest, SearchResponse, SearchResult, WatchState,
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
    /// Restrict to one registered project, by name or id.
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
            path_prefix: args.path_prefix.clone(),
            language: args.language.clone(),
            status: args.status.clone(),
            limit: args.limit,
        }
    }
}

/// `lore add <path>` — register a project root.
///
/// The path is made absolute against the current directory before it is sent,
/// because a relative path means nothing to a daemon started from somewhere
/// else. Canonicalization (symlinks, casing, `..`) stays the daemon's job: it
/// is the one that has to decide whether two spellings are the same project.
pub async fn add(path: String) -> Result<()> {
    let root = absolute_utf8(&path)?;
    let client = Client::connect()?;
    let body = client
        .post(
            "projects",
            &RegisterProjectRequest {
                root: root.to_string(),
                name: None,
            },
        )
        .await?;
    let project: ProjectInfo = parse(&body)?;
    println!("registered {} (id {})", project.name, project.id);
    println!("  {}", project.root);
    Ok(())
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

/// `lore status` — daemon and index health.
pub async fn status(json: bool) -> Result<()> {
    let client = Client::connect()?;
    let body = client.get("status").await?;
    if json {
        println!("{body}");
        return Ok(());
    }
    print!("{}", render_status(&parse::<DaemonStatus>(&body)?));
    Ok(())
}

/// `lore search <query>` — the same query surface agents get over MCP, so a
/// human can reproduce and debug exactly what an agent saw.
pub async fn search(args: SearchArgs) -> Result<()> {
    let client = Client::connect()?;
    let body = client.post("search", &SearchRequest::from(&args)).await?;
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
    }
    out
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

    // -- rendering ---------------------------------------------------------
    // Kept in step with `lore-mcp/src/render.rs`; the assertions below are the
    // shape the two are agreed on.

    fn vault_hit() -> SearchResult {
        SearchResult {
            chunk_id: "9f3a1c2b".into(),
            project: "lore".into(),
            path: "design/4_Interfaces/4.1_MCP_Surface.md".into(),
            line_start: 15,
            line_end: 17,
            language: Some("markdown".into()),
            symbol_path: None,
            heading_path: Some(vec!["MCP Tool Surface".into(), "v0.1 tools".into()]),
            design_status: Some("decided".into()),
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
            path: "Assets/Scripts/Board.cs".into(),
            line_start: 120,
            line_end: 141,
            language: Some("csharp".into()),
            symbol_path: Some("Board.Update".into()),
            heading_path: None,
            design_status: None,
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
