//! FTS5 query sanitization and `SearchFilter` → SQL translation.

use rusqlite::types::Value;

use super::subword;
use super::{SearchFilter, StatusFilter, status_str};

/// Hard cap on terms in one FTS query — pasted-blob queries should not turn
/// into thousand-term MATCH expressions.
const MAX_TERMS: usize = 64;

/// Turn arbitrary user text into an FTS5 MATCH expression that cannot be a
/// syntax error.
///
/// Approach: **whitelist, don't escape.** Every run of alphanumeric/`_`
/// characters becomes one double-quoted phrase term; everything else
/// (parentheses, quotes, `:`, `^`, `-`, `+`, `NEAR/`, stray `*`) is dropped.
/// A `*` immediately following a term is preserved as a prefix operator
/// because that is the one piece of query syntax worth exposing. Because the
/// terms only ever contain alphanumerics and `_`, no quote escaping is even
/// reachable, so `a AND (`, `"unterminated`, `*`, `)))` all produce valid
/// (possibly empty) expressions instead of an `fts5: syntax error`.
///
/// Bare uppercase `AND`/`OR`/`NOT`/`NEAR` are dropped rather than quoted: a
/// user typing them means the operator, not the literal word, and FTS5's
/// default for juxtaposed terms is already AND. Consequence: `OR` degrades to
/// AND rather than erroring. Raw FTS5 syntax is deliberately not exposed at
/// this layer; if an advanced mode is ever wanted it belongs behind an
/// explicit opt-in in the daemon, not in the default path.
///
/// Returns an empty vector when the input contains no usable terms; callers
/// treat that as "no results" rather than running a MATCH.
///
/// Terms are returned rather than pre-joined because juxtaposition is AND in
/// FTS5, and the caller needs to be able to relax that — see
/// [`or_fts_query`]. Each returned element is a self-contained FTS5
/// sub-expression, not necessarily a bare term, precisely so that both joins
/// stay correct after [`expand_term`] parenthesizes one.
pub(crate) fn sanitize_fts_terms(input: &str) -> Vec<String> {
    fn flush(current: &mut String, prefix: bool, terms: &mut Vec<String>) {
        if current.is_empty() {
            return;
        }
        let term = std::mem::take(current);
        if !matches!(term.as_str(), "AND" | "OR" | "NOT" | "NEAR") && terms.len() < MAX_TERMS {
            terms.push(expand_term(&term, prefix));
        }
    }

    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();

    for c in input.chars() {
        if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else {
            let prefix = c == '*' && !current.is_empty();
            flush(&mut current, prefix, &mut terms);
        }
    }
    flush(&mut current, false, &mut terms);

    terms
}

/// One sanitized term as the FTS5 sub-expression that reaches both the
/// verbatim columns and the subword column.
///
/// Symmetry with the index side is the whole contract: the subword column was
/// filled by [`subword::expand_into`], and the query side splits the term with
/// the *same* [`subword::split_parts`], so a term matches its own expansion by
/// construction rather than by two rules that happen to agree today.
///
/// A term that is already a plain word is left exactly as it was — no
/// parentheses, no alternation, no extra work — which is why prose queries
/// produce byte-identical MATCH expressions to before this existed. A compound
/// term becomes `("theTerm" OR "the Term")`:
///
/// - the first branch still finds the identifier verbatim, so exact-identifier
///   search is untouched (that is what `tokenchars '_'` bought and this must
///   not spend);
/// - the second is a *phrase*, not an AND of the parts, so `parseJSONResponse`
///   matches an identifier whose subwords are adjacent rather than any chunk
///   that happens to mention parsing, JSON and responses in three unrelated
///   places.
///
/// A trailing `*` distributes onto both branches. FTS5 applies a prefix
/// operator to the last token of a phrase, so `"parse json resp"*` is the
/// prefix search the user asked for, one level down.
fn expand_term(term: &str, prefix: bool) -> String {
    let star = if prefix { "*" } else { "" };
    let parts = subword::split_parts(term);
    // `parts.is_empty()` is reachable: the term `_` is all separator.
    if parts.is_empty() || (parts.len() == 1 && parts[0] == term) {
        return format!("\"{term}\"{star}");
    }
    let phrase = parts.join(" ");
    format!("(\"{term}\"{star} OR \"{phrase}\"{star})")
}

/// The terms as one conjunction: every term must match.
///
/// Spelled with an explicit `AND` rather than by juxtaposition, and that is not
/// a style choice. FTS5's implicit-AND is a rule about *adjacent phrases*: the
/// moment either side of the join is a parenthesized expression — which is
/// exactly what [`expand_term`] produces for a compound term — juxtaposition
/// stops parsing and the whole query is an `fts5: syntax error`. `"how"
/// ("a" OR "b")` is a syntax error; `"how" AND ("a" OR "b")` is the same
/// conjunction FTS5 always meant. Writing the operator makes the expression
/// legal whatever its operands turn out to be.
pub(crate) fn and_fts_query(terms: &[String]) -> String {
    terms.join(" AND ")
}

/// The same terms joined with `OR` instead of `AND`.
///
/// FTS5 reads `"a" "b"` as `"a" AND "b"`, so a five-word natural-language
/// question only matches a chunk containing all five words — which for prose
/// questions is usually no chunk at all. The lexical arm then contributes
/// nothing to fusion, silently, on exactly the queries the MCP `search`
/// description tells agents to ask. Relaxing to OR keeps BM25 in charge of
/// ranking: it already scores chunks carrying more (and rarer) of the terms
/// above chunks carrying one common one.
///
/// Returns an empty string for fewer than two terms — a single-term query is
/// identical under either operator, and the caller should not pay for a
/// second round trip to learn that.
pub(crate) fn or_fts_query(terms: &[String]) -> String {
    if terms.len() < 2 {
        return String::new();
    }
    terms.join(" OR ")
}

/// A `WHERE`-clause fragment plus its positional parameters, in order.
pub(crate) struct FilterSql {
    pub(crate) sql: String,
    pub(crate) params: Vec<Value>,
}

/// Whether a stored path and a requested prefix that differ only in case name
/// the same file. Mirrors [`crate::daemon::paths`]: Windows paths are
/// ASCII-case-insensitive, everything else is exact. Full Unicode folding is
/// deliberately not attempted — `daemon::paths` does not do it either, so
/// `Ä`/`ä` in a Windows directory name still fails to match (a documented
/// gap, not a new one).
const PATHS_IGNORE_CASE: bool = cfg!(windows);

/// Build the filter fragment for a query whose chunk table is aliased `c`.
///
/// Path prefixing uses `substr(path, 1, n) = prefix` rather than `LIKE`:
/// `_` and `%` are ordinary characters in real paths, and `LIKE` would treat
/// them as wildcards.
pub(crate) fn filter_sql(filter: &SearchFilter) -> FilterSql {
    let mut sql = String::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(project) = filter.project {
        sql.push_str(" AND c.project_id = ?");
        params.push(Value::Integer(project));
    }
    if let Some(prefix) = &filter.path_prefix {
        // SQLite counts *characters*; Rust's `len()` counts bytes. Passing
        // the byte length would slice `données/parser.cs` to `données/p` and
        // compare it against an eight-character prefix, hiding every file
        // under the directory.
        let chars = prefix.chars().count() as i64;
        if PATHS_IGNORE_CASE {
            // `lower()` without ICU folds ASCII only — exactly the policy
            // `daemon::paths` uses for Windows containment — so the Rust side
            // must fold the same way or `DONNÉES/` would diverge.
            sql.push_str(" AND lower(substr(c.path, 1, ?)) = ?");
            params.push(Value::Integer(chars));
            params.push(Value::Text(prefix.to_ascii_lowercase()));
        } else {
            sql.push_str(" AND substr(c.path, 1, ?) = ?");
            params.push(Value::Integer(chars));
            params.push(Value::Text(prefix.clone()));
        }
    }
    if let Some(language) = &filter.language {
        sql.push_str(" AND c.language = ?");
        params.push(Value::Text(language.clone()));
    }
    // Effective, not declared: `min_authority` is a ranking-side floor, and a
    // caller asking to skip low-authority material means the authority Lore
    // actually assigns, not the one a document claims for itself.
    if let Some(min) = filter.min_authority {
        sql.push_str(" AND c.effective_tier >= ?");
        params.push(Value::Integer(i64::from(min)));
    }
    if let Some(kinds) = &filter.source_kinds {
        // Source kind lives on the project row, not the chunk: chunks would
        // have to be rewritten when a source is reclassified, and a subquery
        // over a table with a handful of rows costs nothing.
        if kinds.is_empty() {
            sql.push_str(" AND 0");
        } else {
            let placeholders = vec!["?"; kinds.len()].join(", ");
            sql.push_str(&format!(
                " AND c.project_id IN (SELECT id FROM projects WHERE kind IN ({placeholders}))"
            ));
            for kind in kinds {
                params.push(Value::Text(kind.as_str().to_string()));
            }
        }
    }
    if let Some(statuses) = &filter.statuses {
        // An explicitly empty allowlist means "nothing is acceptable".
        if statuses.is_empty() {
            sql.push_str(" AND 0");
        } else {
            let mut alts: Vec<String> = Vec::new();
            if statuses.contains(&StatusFilter::Unclassified) {
                alts.push("c.design_status IS NULL".to_string());
            }
            let named: Vec<&str> = statuses
                .iter()
                .filter_map(|s| match s {
                    StatusFilter::Unclassified => None,
                    StatusFilter::Status(st) => Some(status_str(*st)),
                })
                .collect();
            if !named.is_empty() {
                let placeholders = vec!["?"; named.len()].join(", ");
                alts.push(format!("c.design_status IN ({placeholders})"));
                for name in named {
                    params.push(Value::Text(name.to_string()));
                }
            }
            sql.push_str(&format!(" AND ({})", alts.join(" OR ")));
        }
    }

    FilterSql { sql, params }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conjunctive expression `lexical_search` builds first, so these
    /// assertions read as the string that actually reaches FTS5.
    fn sanitize_fts_query(input: &str) -> String {
        and_fts_query(&sanitize_fts_terms(input))
    }

    /// The join has to be an explicit operator, because juxtaposition is only
    /// legal between phrases: `"how" ("a" OR "b")` is an `fts5: syntax error`,
    /// and a query mixing a plain word with a compound one is the *ordinary*
    /// case, not an edge.
    #[test]
    fn a_mixed_query_joins_with_an_explicit_operator() {
        let terms = sanitize_fts_terms("how does _dispatch_fanout work");
        assert_eq!(
            and_fts_query(&terms),
            r#""how" AND "does" AND ("_dispatch_fanout" OR "dispatch fanout") AND "work""#
        );
        assert_eq!(
            or_fts_query(&terms),
            r#""how" OR "does" OR ("_dispatch_fanout" OR "dispatch fanout") OR "work""#
        );
    }

    /// A compound term keeps its verbatim branch — that is the exact-identifier
    /// search `tokenchars '_'` exists to protect — and gains the subword phrase
    /// that reaches the `subwords` column.
    #[test]
    fn sanitize_keeps_identifier_terms() {
        assert_eq!(
            sanitize_fts_query("content_hash"),
            "(\"content_hash\" OR \"content hash\")"
        );
        assert_eq!(
            sanitize_fts_query("Board.Update"),
            "\"Board\" AND \"Update\"".to_string(),
            "`.` already separates, and neither half is compound"
        );
    }

    /// Words that were never compound must reach FTS5 as themselves: prose
    /// queries pay nothing for the identifier machinery beyond the operator
    /// that was always implied.
    #[test]
    fn plain_words_are_left_exactly_as_they_were() {
        assert_eq!(
            sanitize_fts_query("how does the daemon own the index"),
            r#""how" AND "does" AND "the" AND "daemon" AND "own" AND "the" AND "index""#
        );
        assert_eq!(sanitize_fts_query("café"), "\"café\"");
    }

    /// The index side split `_dispatch_fanout` into adjacent subwords, so the
    /// query side must ask for a *phrase*, not three unrelated words.
    #[test]
    fn compound_terms_expand_to_a_phrase_over_the_subword_column() {
        assert_eq!(
            sanitize_fts_query("parseJSONResponse"),
            "(\"parseJSONResponse\" OR \"parse JSON Response\")"
        );
        assert_eq!(
            sanitize_fts_query("_dispatch_fanout"),
            "(\"_dispatch_fanout\" OR \"dispatch fanout\")"
        );
        // One part that differs from the term is still an expansion.
        assert_eq!(
            sanitize_fts_query("_private"),
            "(\"_private\" OR \"private\")"
        );
        // A term that is all separator has no parts at all and must not
        // produce `OR ""`.
        assert_eq!(sanitize_fts_query("_"), "\"_\"");
    }

    #[test]
    fn sanitize_preserves_prefix_star() {
        assert_eq!(sanitize_fts_query("cont*"), "\"cont\"*");
        // A bare star is not a term and must not become one.
        assert_eq!(sanitize_fts_query("*"), "");
        // FTS5 applies `*` to the last token of a phrase, so the prefix search
        // survives one level down into the expansion.
        assert_eq!(
            sanitize_fts_query("dispatch_fan*"),
            "(\"dispatch_fan\"* OR \"dispatch fan\"*)"
        );
    }

    #[test]
    fn sanitize_strips_operator_syntax() {
        assert_eq!(sanitize_fts_query("a AND ("), "\"a\"");
        assert_eq!(sanitize_fts_query("\"unterminated"), "\"unterminated\"");
        assert_eq!(
            sanitize_fts_query("NEAR(a b, 3)"),
            "\"a\" AND \"b\" AND \"3\""
        );
        assert_eq!(sanitize_fts_query(")))"), "");
        assert_eq!(sanitize_fts_query("   "), "");
    }

    /// `substr` counts characters, so the bound handed to it must too.
    #[test]
    fn path_prefix_bound_is_a_character_count() {
        let filter = SearchFilter {
            path_prefix: Some("données/".into()),
            ..SearchFilter::default()
        };
        let f = filter_sql(&filter);
        assert!(f.sql.contains("substr(c.path, 1, ?)"), "{}", f.sql);
        assert_eq!(
            f.params[0],
            Value::Integer(8),
            "eight characters, not nine bytes"
        );
        // ASCII folding on both sides, or neither.
        assert_eq!(f.sql.contains("lower("), PATHS_IGNORE_CASE);
        if PATHS_IGNORE_CASE {
            let filter = SearchFilter {
                path_prefix: Some("Assets/Scripts/".into()),
                ..SearchFilter::default()
            };
            assert_eq!(
                filter_sql(&filter).params[1],
                Value::Text("assets/scripts/".into())
            );
        }
    }

    /// The cap counts *terms*, which is no longer the same as counting
    /// space-separated words — one term can expand into a parenthesized
    /// alternation — so assert on the vector the cap actually governs.
    #[test]
    fn sanitize_caps_term_count() {
        let long = (0..200)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(sanitize_fts_terms(&long).len(), MAX_TERMS);
    }
}
