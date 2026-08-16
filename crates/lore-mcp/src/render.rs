//! Daemon responses -> the compact text an agent (or a human at a terminal)
//! reads. Pure string building over `lore_core` types; no IO, no state.
//!
//! Deliberately duplicated in `lore`'s CLI (`crates/lore/src/cli.rs`) rather
//! than shared: hoisting it into `lore-core` would make the wire-contract crate
//! own presentation, and the two renderings are free to drift where the
//! audiences differ (the CLI addresses a human who can run `lore daemon`; this
//! one addresses an agent who must ask). The snapshot tests on both sides are
//! what keep the shapes honest.
//!
//! Format rules, chosen for token economy:
//! - one header line per hit carrying everything an agent needs to decide
//!   whether to read further: project, path, span, score, language;
//! - indented metadata lines only when the field exists (no `symbol: none`);
//! - the excerpt verbatim and unindented, so code keeps its own indentation;
//! - `chunk_id` always present, because `expand` requires it and an agent that
//!   has to guess will guess wrong — but shortened, because the full id is 64
//!   hex characters of pure handle (see [`SHORT_CHUNK_ID`]).

use lore_core::{
    DaemonStatus, EmbeddingStatus, ExpandResponse, ProjectStatus, SearchResponse, SearchResult,
    WatchState,
};
use std::fmt::Write as _;

/// Degradation must be visible, never silent (D-0007).
const LEXICAL_ONLY_NOTE: &str = "note: embeddings are unavailable, so these are lexical matches only; \
     semantically related chunks may be missing (call `status` for why)\n";

/// Displayed chunk-id length, git-style.
///
/// A full blake3 id is 64 hex characters — roughly sixteen tokens spent twice
/// on every truncated hit, for a string whose only job is to be handed back.
/// Twelve keeps a collision inside one project a curiosity rather than a plan,
/// and `expand` resolves any prefix at least [`lore_core::MIN_CHUNK_ID_PREFIX`]
/// long, so what is printed is always enough to pass straight back. The wire
/// still carries the id whole.
const SHORT_CHUNK_ID: usize = 12;
const _: () = assert!(SHORT_CHUNK_ID >= lore_core::MIN_CHUNK_ID_PREFIX);

/// The leading [`SHORT_CHUNK_ID`] characters of an id.
///
/// Sliced on a character boundary rather than a byte offset: an id from a
/// future daemon that is not 64 hex characters must render short, not panic
/// mid-line.
fn short_id(chunk_id: &str) -> &str {
    match chunk_id.char_indices().nth(SHORT_CHUNK_ID) {
        Some((at, _)) => &chunk_id[..at],
        None => chunk_id,
    }
}

/// A symbol path as a reader wants it, with the chunker's discriminators
/// removed.
///
/// Two of them exist, and neither is meant for human eyes: `#w<n>` marks one
/// window of a span too large to chunk whole, and `#s<n>` names a run of
/// statements belonging to no declared symbol. They are *identity* — they keep
/// two such chunks apart in the derived id — so the wire keeps them and only
/// the display drops them. A path that is nothing but a filler ordinal is the
/// file's top level, which is worth saying rather than printing `#s0`.
///
/// The match is deliberately exact (`#s`/`#w` then digits) rather than "cut at
/// the first `#`": `Counter.#count` is a real symbol path for a JavaScript
/// private field, and truncating it would be a lie about the code.
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

/// Heading titles minus any element that is only a window discriminator (the
/// chunker appends `#w<n>` as its own element of a Markdown heading path).
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

/// Rendered `search` result set.
pub fn search(query: &str, response: &SearchResponse) -> String {
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
    // Only when Lore disagreed with the document. Printing "authority:
    // neutral" on every ordinary hit would cost tokens to say nothing; a line
    // that appears *only* when a declaration was refused is the one an agent
    // must not miss. A repository with no authority profile has no reading to
    // report at all (D-0012), so it never reaches this line either.
    if let (Some(note), Some(authority)) = (&result.authority_note, &result.effective_authority) {
        let _ = writeln!(out, "    authority: {authority} - {note}");
    }
    let _ = writeln!(
        out,
        "    project_key: {}  chunk_id: {}",
        result.project_key,
        short_id(&result.chunk_id)
    );

    out.push_str(result.excerpt.trim_end_matches('\n'));
    out.push('\n');
    if result.excerpt_truncated {
        let _ = writeln!(
            out,
            "    (excerpt truncated - expand project_key=\"{key}\" chunk_id=\"{chunk}\" for the full text)",
            key = result.project_key,
            chunk = short_id(&result.chunk_id),
        );
    }
}

/// Rendered `expand` payload. `project` comes from the request: the daemon
/// does not echo it, and without it the header would not name the file's home.
pub fn expand(project: &str, response: &ExpandResponse) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{project}  {path}:{start}-{end}  (file has {total} lines)",
        path = response.path,
        start = response.line_start,
        end = response.line_end,
        total = response.file_lines,
    );
    out.push_str(response.text.trim_end_matches('\n'));
    out.push('\n');
    out
}

/// Rendered `status` snapshot.
pub fn status(status: &DaemonStatus) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "lore daemon {version}  api v{api}  generation {generation}",
        version = status.daemon_version,
        api = status.api_version,
        generation = status.generation,
    );
    let _ = writeln!(out, "{}", embeddings(&status.embeddings));
    push_abandoned(&mut out, status.embed_abandoned);

    if status.projects.is_empty() {
        out.push_str("projects: none registered (ask the user to run `lore add <path>`)\n");
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

/// Chunks the embed worker gave up on, and only when there are any.
///
/// Silent when clean, on the same rule as every other note here: a line that
/// says "0 chunks were skipped" on every call costs tokens to say nothing.
/// Loud when it is not, because a corpus quietly missing some of its vectors
/// answers searches with a gap an agent cannot otherwise see (D-0007).
fn push_abandoned(out: &mut String, abandoned: u64) {
    if abandoned == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "EMBEDDING: {abandoned} chunk(s) refused by the endpoint this daemon run and not \
         embedded; they are retried periodically, so search may be missing them for now"
    );
}

/// Which authority profile this repository opted into, and how healthy it is.
///
/// Always printed, "none" included: an agent deciding how much to trust a
/// `design_status: decided` document needs to know whether that vocabulary
/// means anything in this repository at all (D-0012). A config error gets its
/// own shout, because the repository is running a *different* model than its
/// committed file asked for.
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

/// Documents that declare `decided` without citing an active decision. Silent
/// when there are none; loud when there are, because the author believes those
/// files are canon and Lore has quietly decided they are not.
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

/// Silent when the watch is armed, loud when it is not: an agent that cannot
/// tell "this index is live" from "this index froze an hour ago" will trust
/// stale results.
fn watch_note(state: WatchState) -> &'static str {
    match state {
        // `Unknown` means a daemon too old to report; saying nothing is more
        // honest than claiming either state.
        WatchState::Armed | WatchState::Unknown => "",
        WatchState::Retrying => "  WATCH RETRYING - edits are not being indexed live",
    }
}

/// `embedded 812/9134 (8%)` — the ratio matters more than either number: it is
/// the difference between "semantic search works here" and "it does not yet".
fn coverage(embedded: u64, chunks: u64) -> String {
    if chunks == 0 {
        return format!("{embedded}/0");
    }
    let percent = (embedded as f64 / chunks as f64 * 100.0).round() as u64;
    format!("{embedded}/{chunks} ({percent}%)")
}

/// One line, and the three states read differently on purpose: an agent that
/// cannot tell `unconfigured` from `unreachable` cannot tell the user whether
/// something is broken or merely unset.
fn embeddings(status: &EmbeddingStatus) -> String {
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
    use lore_core::ProjectStatus;

    /// A realistic 64-character chunk id starting with `head`. The fixtures
    /// carry full ids on purpose: a short one would let the shortening pass
    /// every assertion by doing nothing.
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
            decision_refs: vec!["D-0007".into(), "D-0008".into()],
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
            excerpt: "void Update() {\n    Tick();".into(),
            excerpt_truncated: true,
        }
    }

    #[test]
    fn vault_hit_shows_authority_and_heading_path() {
        let rendered = search(
            "vault authority",
            &SearchResponse {
                results: vec![vault_hit()],
                lexical_only: false,
            },
        );
        assert!(rendered.starts_with("1 result(s) for \"vault authority\" (hybrid)\n"));
        assert!(rendered.contains(
            "[1] lore  design/4_Interfaces/4.1_MCP_Surface.md:15-17  score 0.874  [markdown]\n"
        ));
        assert!(rendered.contains("    heading: MCP Tool Surface > v0.1 tools\n"));
        assert!(rendered.contains("    status: decided  refs: D-0007, D-0008\n"));
        assert!(rendered.contains("    project_key: lore  chunk_id: 9f3a1c2b7e4d\n"));
        assert!(!rendered.contains("truncated"));
        // Declared and effective agree here, so no authority line is spent.
        assert!(!rendered.contains("authority:"), "{rendered}");
    }

    /// The case an agent must not miss: the document says `decided`, Lore says
    /// otherwise, and the line naming the disagreement is the whole point.
    #[test]
    fn a_refused_declaration_renders_the_reason() {
        let mut demoted = vault_hit();
        demoted.path = "design/9_Scratch/notes.md".into();
        demoted.effective_authority = Some("deprecated".into());
        demoted.authority_note = Some("9_Scratch path cap".into());
        let rendered = search(
            "authority",
            &SearchResponse {
                results: vec![demoted],
                lexical_only: false,
            },
        );
        assert!(rendered.contains("    status: decided  refs: D-0007, D-0008\n"));
        assert!(
            rendered.contains("    authority: deprecated - 9_Scratch path cap\n"),
            "{rendered}"
        );
    }

    #[test]
    fn code_hit_shows_symbol_and_truncation_hint_naming_expand_arguments() {
        let rendered = search(
            "board update",
            &SearchResponse {
                results: vec![code_hit()],
                lexical_only: false,
            },
        );
        assert!(rendered.contains("    symbol: Board.Update\n"));
        // No vault frontmatter: neither line is emitted rather than emitted empty.
        assert!(!rendered.contains("status:"));
        assert!(!rendered.contains("refs:"));
        assert!(rendered.contains(
            "    (excerpt truncated - expand project_key=\"lexomancy\" chunk_id=\"4e77ba0193ab\" for the full text)\n"
        ));
    }

    /// Issue #9: `symbol: #s0` told a reader nothing at all. The chunker's
    /// discriminators are identity, not description, so they never reach the
    /// page — and a chunk that belongs to no symbol says what it actually is.
    #[test]
    fn chunker_discriminators_never_reach_the_reader() {
        assert_eq!(display_symbol("#s0"), "top-level statements");
        assert_eq!(display_symbol("Board.#s2"), "Board (statements)");
        assert_eq!(display_symbol("Parser.Parse#w1"), "Parser.Parse");
        assert_eq!(display_symbol("Board.Update"), "Board.Update");
        // A JavaScript private field is a real name, not a discriminator.
        assert_eq!(display_symbol("Counter.#count"), "Counter.#count");
        assert_eq!(display_symbol("Region.#section"), "Region.#section");
        // …and neither is a bare `#` or a suffix with no ordinal.
        assert_eq!(display_symbol("Odd.#w"), "Odd.#w");

        let headings = ["Ranking".to_string(), "#w0".to_string()];
        assert_eq!(display_headings(&headings), ["Ranking"]);
        assert_eq!(
            display_headings(&["A".to_string(), "B".to_string()]),
            ["A", "B"]
        );
    }

    #[test]
    fn a_filler_chunk_renders_as_what_it_is() {
        let mut filler = code_hit();
        filler.symbol_path = Some("#s0".into());
        let rendered = search(
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

    /// The whole point of the shortening: the id an agent reads is twelve
    /// characters, never sixty-four, and it is the same twelve in both places
    /// a truncated hit prints it — so what it copies back is a legal prefix.
    #[test]
    fn chunk_ids_are_shortened_everywhere_they_appear() {
        let hit = code_hit();
        let rendered = search(
            "board update",
            &SearchResponse {
                results: vec![hit.clone()],
                lexical_only: false,
            },
        );
        assert!(!rendered.contains(&hit.chunk_id), "{rendered}");
        assert_eq!(rendered.matches("4e77ba0193ab").count(), 2, "{rendered}");
        assert_eq!(short_id(&hit.chunk_id).len(), SHORT_CHUNK_ID);
        // A shorter id than we would print is left whole rather than padded,
        // and a non-hex one does not panic the renderer.
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(
            short_id("日本語のなにかとても長い識別子"),
            "日本語のなにかとても長い"
        );
    }

    #[test]
    fn empty_results_say_so_and_surface_lexical_only_degradation() {
        let rendered = search(
            "nothing here",
            &SearchResponse {
                results: vec![],
                lexical_only: true,
            },
        );
        assert_eq!(
            rendered,
            format!("no results for \"nothing here\" (lexical-only)\n{LEXICAL_ONLY_NOTE}")
        );
    }

    #[test]
    fn excerpts_never_double_space_between_hits() {
        let rendered = search(
            "two",
            &SearchResponse {
                results: vec![vault_hit(), code_hit()],
                lexical_only: false,
            },
        );
        assert!(!rendered.contains("\n\n\n"), "{rendered}");
        assert!(rendered.contains("\n[2] lexomancy"));
    }

    #[test]
    fn expand_header_carries_project_and_whole_file_size() {
        let rendered = expand(
            "lore",
            &ExpandResponse {
                path: "src/main.rs".into(),
                line_start: 10,
                line_end: 12,
                text: "fn main() {}\n\n".into(),
                file_lines: 57,
            },
        );
        assert_eq!(
            rendered,
            "lore  src/main.rs:10-12  (file has 57 lines)\nfn main() {}\n"
        );
    }

    #[test]
    fn status_distinguishes_the_three_embedding_states() {
        let base = DaemonStatus {
            api_version: 1,
            daemon_version: "0.1.0".into(),
            generation: 42,
            projects: vec![],
            embeddings: EmbeddingStatus::Unconfigured,
            latency: Vec::new(),
            embed_abandoned: 0,
        };
        assert!(status(&base).contains("embeddings: UNCONFIGURED"));
        assert!(
            status(&DaemonStatus {
                embeddings: EmbeddingStatus::Unreachable {
                    endpoint: "http://127.0.0.1:11434".into(),
                    error: "connection refused".into(),
                },
                ..base.clone()
            })
            .contains("embeddings: UNREACHABLE - http://127.0.0.1:11434 (connection refused)")
        );
        assert!(
            status(&DaemonStatus {
                embeddings: EmbeddingStatus::Ready {
                    endpoint: "http://127.0.0.1:11434".into(),
                    model: "nomic-embed-text".into(),
                },
                ..base.clone()
            })
            .contains("embeddings: ready - http://127.0.0.1:11434 model nomic-embed-text")
        );
        // Empty registry points at the CLI, since registration is CLI-only (4.1).
        assert!(status(&base).contains("projects: none registered"));
    }

    #[test]
    fn status_pads_names_and_reports_embedding_coverage() {
        let rendered = status(&DaemonStatus {
            api_version: 1,
            daemon_version: "0.1.0".into(),
            generation: 3,
            projects: vec![
                ProjectStatus {
                    id: 1,
                    name: "lexomancy".into(),
                    key: "lexomancy".into(),
                    root: r"C:\repos\Lexomancy".into(),
                    kind: "repo".into(),
                    files: 812,
                    chunks: 9134,
                    embedded_chunks: 0,
                    authority_violations: 0,
                    authority_violation_paths: Vec::new(),
                    watch: WatchState::Armed,
                    ..ProjectStatus::default()
                },
                ProjectStatus {
                    id: 2,
                    name: "lore".into(),
                    key: "lore".into(),
                    root: r"C:\repos\lore".into(),
                    kind: "repo".into(),
                    files: 96,
                    chunks: 1204,
                    embedded_chunks: 1204,
                    authority_violations: 0,
                    authority_violation_paths: Vec::new(),
                    watch: WatchState::Armed,
                    ..ProjectStatus::default()
                },
            ],
            embeddings: EmbeddingStatus::Unconfigured,
            latency: Vec::new(),
            embed_abandoned: 0,
        });
        assert!(rendered.contains("  lexomancy  files 812  chunks 9134  embedded 0/9134 (0%)"));
        assert!(rendered.contains("  lore       files 96  chunks 1204  embedded 1204/1204 (100%)"));
    }

    /// A frozen watch is the failure an agent cannot infer from results: the
    /// index simply stops changing. It must be on the line, and an armed one
    /// must not add noise.
    #[test]
    fn a_project_whose_watch_is_not_armed_says_so() {
        let mut status_body = DaemonStatus {
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
                authority_violations: 0,
                authority_violation_paths: Vec::new(),
                watch: WatchState::Armed,
                ..ProjectStatus::default()
            }],
            embeddings: EmbeddingStatus::Unconfigured,
            latency: Vec::new(),
            embed_abandoned: 0,
        };
        assert!(!status(&status_body).contains("WATCH RETRYING"));

        status_body.projects[0].watch = WatchState::Retrying;
        assert!(status(&status_body).contains("WATCH RETRYING"));
    }

    #[test]
    fn coverage_does_not_divide_by_zero_on_an_unindexed_project() {
        assert_eq!(coverage(0, 0), "0/0");
    }

    /// Chunks the endpoint refused are a gap in the corpus that answers no
    /// search and appears nowhere else — so it is reported, and only when
    /// there is something to report (#9).
    #[test]
    fn abandoned_chunks_are_reported_and_a_clean_run_says_nothing() {
        let body = |abandoned: u64| DaemonStatus {
            api_version: 1,
            daemon_version: "0.1.0".into(),
            generation: 1,
            projects: vec![],
            embeddings: EmbeddingStatus::Unconfigured,
            latency: Vec::new(),
            embed_abandoned: abandoned,
        };
        assert!(!status(&body(0)).contains("EMBEDDING"));

        let rendered = status(&body(7));
        assert!(rendered.contains("EMBEDDING: 7 chunk(s)"), "{rendered}");
        // Not a life sentence: the note has to say they come back, or an agent
        // reads a temporary gap as a permanent one.
        assert!(rendered.contains("retried"), "{rendered}");
    }

    /// The agent-facing copy of the CLI's violation block. It is duplicated
    /// code, which is exactly why it is tested separately: the two renderers
    /// can drift, and this one is what an agent reads before deciding whether
    /// a `decided` document is trustworthy.
    #[test]
    fn status_tells_the_agent_which_decided_declarations_were_refused() {
        let body = |violations: u64, paths: Vec<String>| DaemonStatus {
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
                watch: WatchState::Armed,
                ..ProjectStatus::default()
            }],
            embeddings: EmbeddingStatus::Unconfigured,
            latency: Vec::new(),
            embed_abandoned: 0,
        };

        assert!(!status(&body(0, Vec::new())).contains("AUTHORITY"));

        let rendered = status(&body(6, vec!["design/a.md".into()]));
        assert!(rendered.contains("AUTHORITY: 6 file(s)"), "{rendered}");
        assert!(rendered.contains("design/a.md"), "{rendered}");
        assert!(rendered.contains("... and 5 more"), "{rendered}");
    }
}
