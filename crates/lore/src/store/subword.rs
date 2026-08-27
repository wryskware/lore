//! Identifier subword splitting: the one function the index side and the query
//! side both run, so a natural-language word can reach inside a compound
//! identifier.
//!
//! # The problem this exists to solve
//!
//! The FTS5 tokenizer keeps `_` as a token character on purpose (see
//! [`super::schema`]): `content_hash` and `_privateField` have to survive as
//! single tokens or exact identifier search stops working. The cost is that
//! they *only* exist as single tokens — `_dispatch_fanout` is one opaque
//! posting, `ConcurrentOrchestration` is another — so the query "dispatch
//! fanout" can never match the first and "concurrent orchestration" can never
//! match the second. FTS5 has no infix search, so no amount of query rewriting
//! fixes that from the query side alone: a prefix term reaches into the *front*
//! of a token and nowhere else. The index has to carry the subwords.
//!
//! # Why an extra column rather than a custom tokenizer
//!
//! The conceptually clean fix is a custom FTS5 tokenizer emitting subwords as
//! `FTS5_TOKEN_COLOCATED` beside the whole identifier. That needs the fts5 C
//! API (`fts5_api`, `xCreateTokenizer`) reached through
//! `sqlite3_prepare/step/bind_pointer` on the bundled amalgamation — rusqlite
//! 0.40 exposes no safe wrapper for it, so it would mean hand-written `unsafe`
//! FFI plus a vtab lifetime contract, all of it load-bearing for a database
//! that would then be unreadable by any process that did not register the same
//! tokenizer *before opening it* (FTS5 resolves the tokenizer at table open,
//! not at query time — a missing one is a hard error, not a degraded search).
//! An extra FTS column costs postings and nothing else: the table still opens
//! everywhere, every existing query keeps working, and the whole mechanism is
//! one pure function plus one SQL view.
//!
//! # What is emitted
//!
//! **Only the expansions.** A run that is already a plain word (`dispatch`,
//! `README`, `café`) contributes *nothing* to the subword column. That is the
//! property that keeps prose search untouched: a Markdown chunk's subword
//! column is empty or nearly so, so its BM25 score is what it always was, and
//! the new column can only ever *add* recall on identifier-bearing text.
//!
//! Splitting is deliberately conservative about scripts without case. The case
//! rules test `char::is_uppercase`/`is_lowercase`, which are both false for CJK,
//! Hebrew, Arabic and the like, and the digit rule tests `is_ascii_digit` rather
//! than `is_numeric` so that Han numerals do not become boundaries. A run of
//! non-Latin text therefore splits into itself, which means it is emitted not at
//! all — passing through unharmed rather than being shredded.
//!
//! # What is *not* expanded: `path`
//!
//! Only `text` and `anchor` feed the subword column. Paths are compound far
//! more reliably than prose is (`1_Architecture/`, `agent_framework/`), and a
//! path belongs to every chunk of its file — so expanding it would add the same
//! tokens to every one of them, lengthening rows that are not about the path
//! and, because `bm25` normalizes by total row length, demoting them on every
//! other term. That penalty would land hardest on a design vault, whose
//! directory names are numbered-and-underscored by convention: exactly the
//! prose corpus this must not disturb. (This is not hypothetical — it flipped
//! `authority_laundering`, where the one honest document lives under
//! `1_Architecture/` and its five forgeries do not.) Filenames stay searchable
//! through the `path` column as they always were; what is given up is reaching
//! a `_`-joined path *segment* by one of its words, which the file's own text
//! and symbol names almost always offer anyway.

use std::collections::HashSet;

/// Split one word-run (alphanumerics and `_`, i.e. exactly what the FTS5
/// tokenizer would keep as a single token) into its subwords.
///
/// Boundaries, in the order they are tested per character:
/// - `_`, which is dropped rather than kept with either side, so
///   `_dispatch_fanout` and `SCREAMING_SNAKE` both split cleanly and a leading
///   or doubled underscore contributes no empty part;
/// - a lower/digit → upper transition (`fooBar`, `utf8Encode`);
/// - the tail of an acronym run, i.e. upper → upper where the *next* character
///   is lower (`HTTPServer` → `HTTP` + `Server`, `parseJSONResponse` → `parse`
///   + `JSON` + `Response`);
/// - any ASCII letter ↔ ASCII digit transition (`utf8`, `v1`, `base64`).
///
/// Returns one element for a run that has no internal boundary. Callers
/// distinguish "did not split" from "split into one part" by comparing the
/// single part against the input — `_private` yields `["private"]`, which is
/// one part but *is* an expansion worth indexing.
pub(crate) fn split_parts(run: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    for segment in run.split('_') {
        push_case_and_digit_parts(segment, &mut parts);
    }
    parts
}

/// [`split_parts`] for one underscore-free segment.
fn push_case_and_digit_parts<'a>(segment: &'a str, parts: &mut Vec<&'a str>) {
    if segment.is_empty() {
        return;
    }
    // Short by construction (one identifier), and the acronym rule needs a
    // one-character lookahead, so materializing the run is cheaper than
    // threading a peekable iterator through three predicates.
    let chars: Vec<(usize, char)> = segment.char_indices().collect();
    let mut start = 0;
    for i in 1..chars.len() {
        let (offset, cur) = chars[i];
        let prev = chars[i - 1].1;
        let next = chars.get(i + 1).map(|&(_, c)| c);
        let boundary = (cur.is_uppercase() && !prev.is_uppercase())
            || (cur.is_uppercase() && next.is_some_and(char::is_lowercase))
            || (cur.is_ascii_digit() != prev.is_ascii_digit());
        if boundary {
            parts.push(&segment[start..offset]);
            start = offset;
        }
    }
    parts.push(&segment[start..]);
}

/// Whether `run`'s subwords say anything its own token does not.
///
/// One part equal to the input is a plain word: indexing it again would only
/// double-count it in BM25. One part *different* from the input is still an
/// expansion (`_private` → `private`), and so is any split into two or more.
fn is_expansion(run: &str, parts: &[&str]) -> bool {
    parts.len() > 1 || parts.first().is_some_and(|only| *only != run)
}

/// The subword expansion of one chunk's indexed text, space separated: the
/// value of the `subwords` FTS column.
///
/// `inputs` are concatenated conceptually — the caller passes a chunk's `text`
/// and `anchor`, and the split is the same for both.
///
/// Word runs are found with the same character class the tokenizer uses
/// (alphanumeric or `_`); everything between them is a separator here exactly
/// as it is there, so the runs this walks are the tokens FTS5 would produce.
/// Nothing is emitted for runs that are already plain words, so prose yields an
/// empty string.
///
/// **Each distinct run contributes at most once.** Not a size optimization —
/// a correctness one. FTS5's `bm25` divides by the row's *total* token count
/// across all columns, so every token this emits makes the chunk score slightly
/// worse for every term that is not in it. Emitting `dispatch fanout` once per
/// occurrence of `_dispatch_fanout` would inflate a code chunk by most of its
/// own length and quietly demote it on ordinary word queries — the exact
/// outcome this column exists to reverse. Once per identifier also says the
/// truer thing: the column is evidence that a name is *present*, not a count of
/// how often it is written.
///
/// Two caveats, both accepted deliberately:
///
/// - Positions run on across the seam between identifiers, so a phrase query
///   can in principle match two adjacent expansions. Preventing that means a
///   filler token per identifier, buying a giant posting list to stop a rare,
///   low-scoring extra hit in a column whose whole job is extra recall.
/// - Deduplication is per chunk, so an identifier's parts are adjacent at its
///   *first* occurrence, which is all a phrase query needs.
pub(crate) fn expand<'a>(inputs: &[&'a str]) -> String {
    let mut seen: HashSet<&'a str> = HashSet::new();
    let mut out = String::new();
    for input in inputs {
        for run in runs(input) {
            if !seen.insert(run) {
                continue;
            }
            let parts = split_parts(run);
            if !is_expansion(run, &parts) {
                continue;
            }
            for part in parts {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(part);
            }
        }
    }
    out
}

/// The maximal runs of token characters in `text` — what FTS5 would tokenize
/// it into, before any of the splitting above.
fn runs(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !is_run_char(c))
        .filter(|run| !run.is_empty())
}

fn is_run_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`expand`] over one string, which is all most of these need.
    fn expand1(text: &str) -> String {
        expand(&[text])
    }

    #[test]
    fn camel_and_pascal_case_split_at_the_hump() {
        assert_eq!(split_parts("fooBar"), ["foo", "Bar"]);
        assert_eq!(
            split_parts("ConcurrentOrchestration"),
            ["Concurrent", "Orchestration"]
        );
        assert_eq!(split_parts("a"), ["a"]);
    }

    #[test]
    fn underscores_are_boundaries_and_are_dropped() {
        assert_eq!(split_parts("dispatch_fanout"), ["dispatch", "fanout"]);
        assert_eq!(
            split_parts("SCREAMING_SNAKE_CASE"),
            ["SCREAMING", "SNAKE", "CASE"]
        );
        // Leading, trailing and doubled underscores contribute no empty parts.
        assert_eq!(split_parts("_dispatch_fanout"), ["dispatch", "fanout"]);
        assert_eq!(split_parts("__init__"), ["init"]);
        assert_eq!(split_parts("_private"), ["private"]);
        assert!(split_parts("_").is_empty());
    }

    /// The acronym rule: an upper run belongs to itself until the character
    /// that starts the next word.
    #[test]
    fn acronym_runs_stay_whole() {
        assert_eq!(split_parts("HTTPServer"), ["HTTP", "Server"]);
        assert_eq!(
            split_parts("parseJSONResponse"),
            ["parse", "JSON", "Response"]
        );
        assert_eq!(split_parts("HTTP"), ["HTTP"]);
        assert_eq!(split_parts("IOError"), ["IO", "Error"]);
    }

    #[test]
    fn digits_are_their_own_subword() {
        assert_eq!(split_parts("utf8"), ["utf", "8"]);
        assert_eq!(split_parts("v1"), ["v", "1"]);
        assert_eq!(split_parts("base64"), ["base", "64"]);
        assert_eq!(split_parts("sha256Digest"), ["sha", "256", "Digest"]);
        assert_eq!(split_parts("12345"), ["12345"]);
    }

    /// Scripts without case must survive: no rule may fire inside them, so
    /// they split into themselves and are therefore never emitted.
    #[test]
    fn unicode_text_passes_through_unharmed() {
        for word in ["café", "naïve", "日本語", "מפתח", "мир"] {
            assert_eq!(split_parts(word), [word], "{word} must not be split");
            assert_eq!(
                expand1(word),
                "",
                "{word} is a plain word, not an expansion"
            );
        }
        // Cased non-ASCII still splits at the hump, and the boundary must land
        // on a character boundary rather than panicking on a byte index.
        assert_eq!(split_parts("ÜberBar"), ["Über", "Bar"]);
    }

    /// The invariant the non-regression claim rests on: a chunk of prose puts
    /// nothing in the subword column, so its BM25 length — and therefore its
    /// score — is exactly what it was before the column existed.
    #[test]
    fn prose_expands_to_nothing() {
        assert_eq!(
            expand1("the daemon owns a single instance of the index"),
            ""
        );
        assert_eq!(expand1(""), "");
        assert_eq!(expand1("...  --- ???"), "");
        assert_eq!(expand(&[]), "");
    }

    #[test]
    fn expansion_walks_every_run_in_the_text() {
        assert_eq!(
            expand1("fn _dispatch_fanout() -> ConcurrentOrchestration { }"),
            "dispatch fanout Concurrent Orchestration"
        );
        // Separators between runs are the tokenizer's, not this function's:
        // `.`, `:` and `/` all end a run.
        assert_eq!(
            expand1("Board.UpdateAll::inner_thing"),
            "Update All inner thing"
        );
    }

    /// Several inputs (a chunk's text and its anchor) share one expansion and
    /// one dedup set, so the anchor's symbol name is not re-emitted just
    /// because the body mentions it too.
    #[test]
    fn inputs_share_one_deduplicated_expansion() {
        assert_eq!(
            expand(&[
                "code:ConcurrentOrchestration",
                "class ConcurrentOrchestration:"
            ]),
            "Concurrent Orchestration"
        );
    }

    /// Repetition must not inflate the column: `bm25` divides by total row
    /// length, so an identifier written fifty times would demote the chunk on
    /// every unrelated word.
    #[test]
    fn a_repeated_identifier_is_expanded_once() {
        assert_eq!(
            expand1("_dispatch_fanout(); _dispatch_fanout(); _dispatch_fanout();"),
            "dispatch fanout"
        );
        assert_eq!(
            expand1("readItem writeItem readItem"),
            "read Item write Item",
            "distinct identifiers each keep their own parts adjacent"
        );
    }

    /// The invariant the whole design rests on: a word already plain adds
    /// nothing, so prose BM25 cannot move.
    #[test]
    fn plain_words_are_never_re_emitted() {
        assert!(!is_expansion("dispatch", &split_parts("dispatch")));
        assert!(!is_expansion("README", &split_parts("README")));
        assert!(is_expansion("_private", &split_parts("_private")));
        assert!(is_expansion("fooBar", &split_parts("fooBar")));
    }
}
