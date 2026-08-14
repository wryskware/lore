//! `POST /v1/search` — query execution.
//!
//! This package ships the lexical half only: BM25 over FTS5, with the wire
//! filters pushed down into SQL so ranking never sees rows the caller
//! excluded. The response says so honestly (`lexical_only: true`), which is
//! the visible-degradation requirement of D-0007 rather than a placeholder.
//!
//! The fusion seam is [`execute`]: it is the one place that turns a
//! [`SearchRequest`] into ranked hits, and the hybrid package adds a vector
//! arm plus RRF *here*, leaving the handler, the filter translation and the
//! result mapping untouched. See the marked point in the body.

use std::collections::HashMap;

use lore_core::{SearchRequest, SearchResponse, SearchResult};

use crate::store::{ProjectId, SearchFilter, SearchHit, StatusFilter, Store};
use crate::types::{ChunkKind, DesignStatus};

/// Results returned when the caller does not ask for a specific number.
pub const DEFAULT_LIMIT: u32 = 20;

/// Hard ceiling; `search` is meant to stay token-lean (`expand` exists for
/// depth, 3.1).
pub const MAX_LIMIT: u32 = 100;

/// Excerpts are capped so a handful of results cannot blow an agent's
/// context window on one tool call.
pub const EXCERPT_MAX_CHARS: usize = 2000;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("unknown project `{0}`")]
    UnknownProject(String),
    #[error(
        "unknown design status `{0}` (expected exploration, leaning, decided, deprecated or unclassified)"
    )]
    UnknownStatus(String),
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
}

/// Run a search against the store.
///
/// Takes `&mut Store` (not a handle) so the caller controls the lock scope:
/// project resolution, ranking and name lookup all happen inside one
/// acquisition, which also makes the result set internally consistent.
pub fn execute(store: &mut Store, request: &SearchRequest) -> Result<SearchResponse, SearchError> {
    let projects = store.list_projects()?;
    let names: HashMap<ProjectId, String> =
        projects.iter().map(|p| (p.id, p.name.clone())).collect();

    let mut filter = SearchFilter::default();
    if let Some(key) = &request.project {
        let project = super::resolve_project(&projects, key)
            .ok_or_else(|| SearchError::UnknownProject(key.clone()))?;
        filter.project = Some(project.id);
    }
    filter.path_prefix = request
        .path_prefix
        .as_ref()
        .map(|prefix| prefix.replace('\\', "/"));
    filter.language = request.language.as_ref().map(|l| l.to_ascii_lowercase());
    if !request.status.is_empty() {
        let statuses = request
            .status
            .iter()
            .map(|s| parse_status(s).ok_or_else(|| SearchError::UnknownStatus(s.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        filter.statuses = Some(statuses);
    }

    let limit = request.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT) as usize;

    // --- fusion seam -------------------------------------------------------
    // The hybrid package runs `store.vector_search(query_vec, &filter, k)`
    // alongside this call and fuses the two ranked lists with RRF (3.1),
    // then applies the vault-status modifier. `lexical_only` becomes
    // "vectors did not participate" rather than a constant.
    let hits = store.lexical_search(&request.query, &filter, limit)?;
    let lexical_only = true;
    // -----------------------------------------------------------------------

    let results = hits.into_iter().map(|hit| to_result(&names, hit)).collect();
    Ok(SearchResponse {
        results,
        lexical_only,
    })
}

fn parse_status(status: &str) -> Option<StatusFilter> {
    Some(match status {
        "unclassified" => StatusFilter::Unclassified,
        "exploration" => StatusFilter::Status(DesignStatus::Exploration),
        "leaning" => StatusFilter::Status(DesignStatus::Leaning),
        "decided" => StatusFilter::Status(DesignStatus::Decided),
        "deprecated" => StatusFilter::Status(DesignStatus::Deprecated),
        _ => return None,
    })
}

fn to_result(names: &HashMap<ProjectId, String>, hit: SearchHit) -> SearchResult {
    let chunk = hit.chunk;
    let (symbol_path, heading_path) = match &chunk.kind {
        ChunkKind::Code { symbol_path, .. } => (Some(symbol_path.clone()), None),
        ChunkKind::Section { heading_path } => (None, Some(heading_path.clone())),
        ChunkKind::Window { .. } => (None, None),
    };
    let (design_status, decision_refs) = match &chunk.vault {
        Some(vault) => (
            vault.design_status.map(status_label),
            merge_refs(&vault.decision_refs, &vault.body_decision_refs),
        ),
        None => (None, Vec::new()),
    };
    let (excerpt, excerpt_truncated) = excerpt(&chunk.text);

    SearchResult {
        chunk_id: chunk.id.0,
        project: names
            .get(&hit.project)
            .cloned()
            .unwrap_or_else(|| hit.project.to_string()),
        path: chunk.path.into_string(),
        line_start: chunk.line_start,
        line_end: chunk.line_end,
        language: chunk.language,
        symbol_path,
        heading_path,
        design_status: design_status.map(str::to_string),
        decision_refs,
        score: f64::from(hit.score),
        excerpt,
        excerpt_truncated,
    }
}

/// Frontmatter refs first (they describe the whole document), then refs found
/// in this chunk's own body; duplicates dropped, order preserved.
fn merge_refs(frontmatter: &[String], body: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(frontmatter.len() + body.len());
    for reference in frontmatter.iter().chain(body) {
        if !out.iter().any(|existing| existing == reference) {
            out.push(reference.clone());
        }
    }
    out
}

/// Truncate on a character boundary — chunk text is arbitrary UTF-8 and a
/// byte-sliced excerpt would panic on the first non-ASCII file.
fn excerpt(text: &str) -> (String, bool) {
    match text.char_indices().nth(EXCERPT_MAX_CHARS) {
        Some((byte, _)) => (text[..byte].to_string(), true),
        None => (text.to_string(), false),
    }
}

fn status_label(status: DesignStatus) -> &'static str {
    match status {
        DesignStatus::Exploration => "exploration",
        DesignStatus::Leaning => "leaning",
        DesignStatus::Decided => "decided",
        DesignStatus::Deprecated => "deprecated",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_truncates_on_a_character_boundary() {
        let text: String = std::iter::repeat_n('é', EXCERPT_MAX_CHARS + 10).collect();
        let (clipped, truncated) = excerpt(&text);
        assert!(truncated);
        assert_eq!(clipped.chars().count(), EXCERPT_MAX_CHARS);

        let short = "fn main() {}";
        assert_eq!(excerpt(short), (short.to_string(), false));
    }

    #[test]
    fn decision_refs_merge_without_duplicates() {
        let merged = merge_refs(
            &["D-0003".into(), "D-0004".into()],
            &["D-0004".into(), "D-0007".into()],
        );
        assert_eq!(merged, ["D-0003", "D-0004", "D-0007"]);
    }
}
