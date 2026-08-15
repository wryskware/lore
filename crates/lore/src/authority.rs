//! Declared vs **effective** authority (adversarial review S1#2).
//!
//! `design_status` and [`crate::types::authority_tier`] stay *declared*: they
//! are what a document says about itself, they are parsed, stored, filterable,
//! and Lore never edits them. What ranking actually uses is the **effective
//! tier** computed here — a pure function of
//!
//! - the declared status,
//! - the project-relative path,
//! - the file's frontmatter `decision_refs`,
//! - the project's active-decision set (parsed from its ledger), and
//! - the source kind (`repo` today, `session` at M3).
//!
//! # Why a separate tier at all
//!
//! Canon (`design/0_Canon/README.md` §Authority order, D-0001) says authority
//! flows from *active accepted ledger entries*, not from a word in
//! frontmatter. Before this module, any file declaring `design_status:
//! decided` was ranked as ratified canon, and any unclassified file that
//! merely *quoted* a `D-NNNN` was promoted to the `leaning` multiplier. Both
//! are authority laundering: a file edit crossed the promotion gate without a
//! ledger entry, and `9_Scratch` notes — which the same README places at the
//! very bottom, with `deprecated` — outranked their station by quoting the
//! decisions they were arguing about.
//!
//! # The policy
//!
//! 1. The ledger file itself (`**/0_Canon/DECISIONS.md`) is **pinned** to
//!    [`LEDGER_TIER`]. It *is* the canon; it must never lose to a document
//!    quoting it.
//! 2. Start from the declared tier.
//! 3. `decided` is honored only if the frontmatter cites at least one
//!    **active** decision (accepted, not superseded) in the same project.
//!    Otherwise it is demoted to [`UNCITED_DECIDED_TIER`] — neutral, not
//!    deprecated: an invalid declaration is a mistake to surface, not content
//!    to bury — and recorded as a [`Demotion::UncitedDecided`] violation.
//! 4. Path ceilings apply to *any* segment of the project-relative path:
//!    `9_Scratch` → [`SCRATCH_TIER_CEILING`], `7_Research` →
//!    [`RESEARCH_TIER_CEILING`]. A declared `deprecated` is already 0 and no
//!    ceiling can raise it.
//! 5. A `session` source is capped at [`SESSION_TIER_CEILING`], below vault
//!    material (3.1's required cap; the exact value is M3 tuning).
//!
//! Citations carry **no** ranking weight of their own any more: they remain
//! visible metadata on every result, and that is all. Authority flows *from*
//! the ledger, never *to* it from whoever quotes a number.

use std::collections::BTreeSet;

use camino::Utf8Path;

use crate::chunk::decision_refs;
use crate::types::{DesignStatus, SourceKind, authority_tier};

/// Directory that contains a project's decision ledger, by convention.
pub const LEDGER_DIR: &str = "0_Canon";

/// The ledger file itself, inside [`LEDGER_DIR`].
pub const LEDGER_FILE: &str = "DECISIONS.md";

/// The ledger is pinned here regardless of what its own frontmatter says.
pub const LEDGER_TIER: u8 = 3;

/// Where a `decided` declaration lands when it cites no active decision.
/// Neutral on purpose — visible and searchable, just not authoritative.
pub const UNCITED_DECIDED_TIER: u8 = 1;

/// Path segment for scratch material (README authority order #6).
pub const SCRATCH_SEGMENT: &str = "9_Scratch";

/// Ceiling applied to anything under a [`SCRATCH_SEGMENT`] directory.
pub const SCRATCH_TIER_CEILING: u8 = 0;

/// Path segment for research reports.
pub const RESEARCH_SEGMENT: &str = "7_Research";

/// Ceiling applied to anything under a [`RESEARCH_SEGMENT`] directory —
/// research is evidence, not decisions (README promotion rules).
pub const RESEARCH_TIER_CEILING: u8 = 1;

/// Ceiling applied to `session` sources so tier-2 memory can never outrank
/// vault material (D-0006/D-0008 + 3.1). Named because M3 will tune it.
pub const SESSION_TIER_CEILING: u8 = 1;

/// Path comparison policy, mirroring [`crate::daemon::paths`] and
/// [`crate::store`]'s filter SQL: Windows paths are ASCII-case-insensitive,
/// everything else is exact.
const PATHS_IGNORE_CASE: bool = cfg!(windows);

fn segment_eq(a: &str, b: &str) -> bool {
    if PATHS_IGNORE_CASE {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Why a chunk's effective tier is below its declared one.
///
/// Persisted by its [`Demotion::code`] so the note text can be reworded
/// without a migration, and so `status` can count violations with an index
/// rather than by string-matching prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Demotion {
    /// Declared `decided` but the frontmatter cites no *active* decision.
    /// The only demotion that is a **violation**: the others are policy
    /// working as intended, this one is a document making a false claim.
    UncitedDecided,
    /// Under a `9_Scratch` directory.
    ScratchPath,
    /// Under a `7_Research` directory.
    ResearchPath,
    /// Belongs to a `session` source.
    SessionSource,
}

impl Demotion {
    /// Stable on-disk spelling. Exhaustive match so a new variant is a
    /// compile error here rather than a silent NULL.
    pub fn code(self) -> &'static str {
        match self {
            Demotion::UncitedDecided => "uncited_decided",
            Demotion::ScratchPath => "scratch_path",
            Demotion::ResearchPath => "research_path",
            Demotion::SessionSource => "session_source",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "uncited_decided" => Demotion::UncitedDecided,
            "scratch_path" => Demotion::ScratchPath,
            "research_path" => Demotion::ResearchPath,
            "session_source" => Demotion::SessionSource,
            _ => return None,
        })
    }

    /// Human/agent-facing explanation, surfaced as `authority_note` on search
    /// results and in the daemon log.
    pub fn note(self) -> &'static str {
        match self {
            Demotion::UncitedDecided => "decided declared but cites no active decision",
            Demotion::ScratchPath => "9_Scratch path cap",
            Demotion::ResearchPath => "7_Research path cap",
            Demotion::SessionSource => "session source cap",
        }
    }

    /// Whether this demotion means the *document* is wrong (as opposed to the
    /// policy simply placing it where it belongs). Only violations are counted
    /// and listed by `status`.
    pub fn is_violation(self) -> bool {
        matches!(self, Demotion::UncitedDecided)
    }
}

/// The computed authority of one file (and therefore of each of its chunks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authority {
    /// Effective tier — what ranking and `min_authority` read.
    pub tier: u8,
    /// Present only when the effective tier is *below* the declared one.
    pub demotion: Option<Demotion>,
}

impl Default for Authority {
    fn default() -> Self {
        Self {
            tier: authority_tier(None),
            demotion: None,
        }
    }
}

impl Authority {
    pub fn is_violation(&self) -> bool {
        self.demotion.is_some_and(Demotion::is_violation)
    }

    /// Lowercase wire label for the effective tier.
    ///
    /// Deliberately not always a `design_status` word: tier 1 is reported as
    /// `neutral` because it covers `exploration`, unclassified, plain code and
    /// *demoted* declarations alike, and calling any of those "exploration"
    /// would be inventing a declaration the document never made.
    pub fn label(&self) -> &'static str {
        match self.tier {
            3 => "decided",
            2 => "leaning",
            0 => "deprecated",
            _ => "neutral",
        }
    }
}

/// What a file declares about itself — the inputs Lore never edits.
#[derive(Debug, Clone, Copy)]
pub struct Declared<'a> {
    pub status: Option<DesignStatus>,
    /// Frontmatter `decision_refs` only. Body references are metadata and
    /// carry no weight (the removed laundering vector).
    pub decision_refs: &'a [String],
}

/// Is this project-relative path the project's decision ledger?
///
/// Convention, not configuration: the last two segments must be
/// `0_Canon/DECISIONS.md`, anywhere in the tree (`design/0_Canon/DECISIONS.md`
/// in this repo, `Docs/design/0_Canon/DECISIONS.md` in another).
pub fn is_ledger_path(path: &Utf8Path) -> bool {
    let mut segments = path.iter().rev();
    let file = segments.next();
    let dir = segments.next();
    matches!((file, dir), (Some(f), Some(d))
        if segment_eq(f, LEDGER_FILE) && segment_eq(d, LEDGER_DIR))
}

/// The effective authority of a file.
///
/// Pure: every input is either persisted on the chunk row, derivable from the
/// path, or the project's stored active set — which is exactly why the
/// recompute pass can rebuild every tier without re-chunking or re-embedding.
pub fn effective(
    path: &Utf8Path,
    declared: Declared<'_>,
    active: &BTreeSet<String>,
    kind: SourceKind,
) -> Authority {
    // The ledger outranks everything, including its own frontmatter.
    if is_ledger_path(path) {
        return Authority {
            tier: LEDGER_TIER,
            demotion: None,
        };
    }

    let declared_tier = authority_tier(declared.status);
    let mut tier = declared_tier;

    // Reason precedence: a violation always wins the note, even when a path
    // ceiling drives the tier lower still. It is the one the reader can act on.
    let mut demotion: Option<Demotion> = None;
    let mut record = |reason: Demotion, tier: &mut u8, ceiling: u8| {
        if ceiling < *tier {
            *tier = ceiling;
            if demotion.is_none_or(|current| !current.is_violation()) {
                demotion = Some(reason);
            }
        }
    };

    if declared.status == Some(DesignStatus::Decided)
        && !declared
            .decision_refs
            .iter()
            .any(|reference| active.contains(reference))
    {
        record(Demotion::UncitedDecided, &mut tier, UNCITED_DECIDED_TIER);
    }

    for segment in path.iter() {
        if segment_eq(segment, SCRATCH_SEGMENT) {
            record(Demotion::ScratchPath, &mut tier, SCRATCH_TIER_CEILING);
        } else if segment_eq(segment, RESEARCH_SEGMENT) {
            record(Demotion::ResearchPath, &mut tier, RESEARCH_TIER_CEILING);
        }
    }

    if kind == SourceKind::Session {
        record(Demotion::SessionSource, &mut tier, SESSION_TIER_CEILING);
    }

    Authority { tier, demotion }
}

// ---------------------------------------------------------------------------
// Ledger parsing
// ---------------------------------------------------------------------------

/// Parse a decision ledger into its **active** decision set.
///
/// Format (see `design/0_Canon/DECISIONS.md`): each entry is an ATX heading
/// `## D-NNNN — Title` followed by a bullet list carrying `**Status:**` and
/// `**Supersedes:**` fields. An entry is active when its status is `Accepted`
/// *and* no entry's `Supersedes` field names it — which is how an append-only
/// ledger retires canon without rewriting history (README promotion rules).
///
/// Deliberately lenient about decoration (`- `, `* `, bold markers, the `:`
/// inside or outside the emphasis) and deliberately strict about the id shape
/// (`D-` + exactly four digits), matching [`crate::chunk::decision_refs`].
/// Unparseable prose is skipped, never an error: a malformed ledger degrades
/// to a smaller active set — which demotes over-claiming documents — rather
/// than failing the index pass.
pub fn parse_ledger(src: &str) -> BTreeSet<String> {
    let mut accepted: BTreeSet<String> = BTreeSet::new();
    let mut superseded: BTreeSet<String> = BTreeSet::new();
    let mut current: Option<(String, bool)> = None;

    let flush = |current: &mut Option<(String, bool)>, accepted: &mut BTreeSet<String>| {
        if let Some((id, is_accepted)) = current.take()
            && is_accepted
        {
            accepted.insert(id);
        }
    };

    for line in src.trim_start_matches('\u{feff}').lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("##") {
            // Any heading closes the previous entry; only a `D-NNNN` one opens
            // a new entry, so a trailing "## Notes" section cannot absorb
            // fields into the last decision.
            flush(&mut current, &mut accepted);
            let rest = rest.trim_start_matches('#');
            current = leading_decision_id(rest).map(|id| (id, false));
            continue;
        }
        let Some((_, is_accepted)) = current.as_mut() else {
            continue;
        };
        if let Some(value) = field_value(trimmed, "Status") {
            *is_accepted = value
                .trim()
                .trim_end_matches(['*', '.', ' '])
                .eq_ignore_ascii_case("accepted");
        } else if let Some(value) = field_value(trimmed, "Supersedes") {
            superseded.extend(decision_refs(value));
        }
    }
    flush(&mut current, &mut accepted);

    accepted
        .into_iter()
        .filter(|id| !superseded.contains(id))
        .collect()
}

/// `D-NNNN` at the very start of `text`, if any.
fn leading_decision_id(text: &str) -> Option<String> {
    let text = text.trim_start();
    let bytes = text.as_bytes();
    let shaped = bytes.len() >= 6
        && bytes[0] == b'D'
        && bytes[1] == b'-'
        && bytes[2..6].iter().all(u8::is_ascii_digit)
        && !bytes
            .get(6)
            .copied()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_');
    shaped.then(|| text[..6].to_string())
}

/// Value of a `- **Name:** value` style field line, ignoring decoration.
fn field_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = line.trim();
    for marker in ['-', '*', '+'] {
        if let Some(stripped) = rest.strip_prefix(marker) {
            // `*` is both a list marker and an emphasis marker: only treat it
            // as a bullet when whitespace follows.
            if stripped.starts_with([' ', '\t']) {
                rest = stripped.trim_start();
                break;
            }
        }
    }
    rest = rest.trim_start_matches('*').trim_start();
    let head = rest.get(..name.len())?;
    if !head.eq_ignore_ascii_case(name) {
        return None;
    }
    let rest = rest[name.len()..].trim_start_matches('*');
    Some(rest.strip_prefix(':')?.trim_start_matches('*').trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn active(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    fn refs(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    fn tier(path: &str, status: Option<DesignStatus>, cited: &[&str], live: &[&str]) -> Authority {
        let cited = refs(cited);
        effective(
            &Utf8PathBuf::from(path),
            Declared {
                status,
                decision_refs: &cited,
            },
            &active(live),
            SourceKind::Repo,
        )
    }

    #[test]
    fn the_ledger_outranks_every_document_that_quotes_it() {
        let pinned = tier("design/0_Canon/DECISIONS.md", None, &[], &[]);
        assert_eq!(pinned.tier, LEDGER_TIER);
        assert_eq!(pinned.demotion, None);
        // Convention is positional, not rooted at `design/`.
        assert!(is_ledger_path(&Utf8PathBuf::from(
            "Docs/0_Canon/DECISIONS.md"
        )));
        assert!(!is_ledger_path(&Utf8PathBuf::from("0_Canon/README.md")));
        assert!(!is_ledger_path(&Utf8PathBuf::from("DECISIONS.md")));
        assert!(!is_ledger_path(&Utf8PathBuf::from(
            "design/2_Memory/DECISIONS.md"
        )));
    }

    #[test]
    fn decided_is_honored_only_when_it_cites_an_active_decision() {
        let honored = tier(
            "design/1_Architecture/1.1.md",
            Some(DesignStatus::Decided),
            &["D-0007"],
            &["D-0003", "D-0007"],
        );
        assert_eq!(honored.tier, 3);
        assert_eq!(honored.demotion, None);

        // Cites nothing at all.
        let bare = tier("design/x.md", Some(DesignStatus::Decided), &[], &["D-0007"]);
        assert_eq!(bare.tier, UNCITED_DECIDED_TIER);
        assert_eq!(bare.demotion, Some(Demotion::UncitedDecided));
        assert!(bare.is_violation());

        // Cites a decision that exists but has been superseded (not active).
        let stale = tier(
            "design/x.md",
            Some(DesignStatus::Decided),
            &["D-0002"],
            &["D-0007"],
        );
        assert_eq!(stale.tier, UNCITED_DECIDED_TIER);
        assert_eq!(stale.demotion, Some(Demotion::UncitedDecided));

        // One valid citation among invented ones is enough.
        let mixed = tier(
            "design/x.md",
            Some(DesignStatus::Decided),
            &["D-9999", "D-0007"],
            &["D-0007"],
        );
        assert_eq!(mixed.tier, 3);
    }

    /// The laundering vector the review found: unclassified scratch notes that
    /// quote D-numbers densely. They now rank at the bottom, and citations buy
    /// nothing anywhere.
    #[test]
    fn path_ceilings_apply_to_any_segment_and_ignore_citations() {
        let scratch = tier(
            "design/9_Scratch/notes.md",
            Some(DesignStatus::Decided),
            &["D-0007"],
            &["D-0007"],
        );
        assert_eq!(scratch.tier, SCRATCH_TIER_CEILING);
        assert_eq!(scratch.demotion, Some(Demotion::ScratchPath));
        assert!(!scratch.is_violation(), "policy, not a false claim");

        let research = tier(
            "design/7_Research/raw/E_rust-stack.md",
            Some(DesignStatus::Leaning),
            &[],
            &[],
        );
        assert_eq!(research.tier, RESEARCH_TIER_CEILING);
        assert_eq!(research.demotion, Some(Demotion::ResearchPath));

        // A ceiling only demotes; it never lifts material already below it.
        let deprecated = tier(
            "design/7_Research/old.md",
            Some(DesignStatus::Deprecated),
            &[],
            &[],
        );
        assert_eq!(deprecated.tier, 0);
        assert_eq!(deprecated.demotion, None);

        // Nothing to cap: an ordinary exploration doc keeps its declared tier.
        let plain = tier(
            "design/3_Retrieval/3.1.md",
            Some(DesignStatus::Leaning),
            &[],
            &[],
        );
        assert_eq!(plain.tier, 2);
        assert_eq!(plain.demotion, None);

        // Code under a scratch directory is capped too — the directory is the
        // statement, not the file type.
        assert_eq!(tier("9_Scratch/spike/main.rs", None, &[], &[]).tier, 0);
    }

    #[test]
    fn a_violation_keeps_the_note_even_when_a_path_cap_bites_harder() {
        let both = tier(
            "design/9_Scratch/x.md",
            Some(DesignStatus::Decided),
            &[],
            &[],
        );
        assert_eq!(
            both.tier, SCRATCH_TIER_CEILING,
            "lowest ceiling wins the tier"
        );
        assert_eq!(
            both.demotion,
            Some(Demotion::UncitedDecided),
            "the actionable reason wins the note"
        );
    }

    #[test]
    fn session_sources_are_capped_below_vault_material() {
        let cited = refs(&["D-0007"]);
        let session = effective(
            &Utf8PathBuf::from("sessions/2026-08-14-thread.md"),
            Declared {
                status: Some(DesignStatus::Decided),
                decision_refs: &cited,
            },
            &active(&["D-0007"]),
            SourceKind::Session,
        );
        assert_eq!(session.tier, SESSION_TIER_CEILING);
        assert_eq!(session.demotion, Some(Demotion::SessionSource));
    }

    #[test]
    fn labels_do_not_invent_a_declaration_the_document_never_made() {
        let label = |tier| {
            Authority {
                tier,
                demotion: None,
            }
            .label()
        };
        assert_eq!(label(3), "decided");
        assert_eq!(label(2), "leaning");
        assert_eq!(label(1), "neutral");
        assert_eq!(label(0), "deprecated");
    }

    // -- ledger parsing ----------------------------------------------------

    const LEDGER: &str = "---\ndesign_status: decided\n---\n\n# Lore Decision Ledger\n\n\
        ## D-0001 — Vault authority model\n\n\
        - **Date:** 2026-08-14\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
        ## D-0002 — Superseded later\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
        ## D-0003 — Proposed only\n\n- **Status:** Proposed\n- **Supersedes:** None\n\n\
        ## D-0004 — Replaces D-0002\n\n- **Status:** Accepted\n- **Supersedes:** D-0002\n\n\
        ## Appendix\n\n- **Status:** Accepted\n";

    #[test]
    fn ledger_parsing_keeps_only_accepted_and_unsuperseded_entries() {
        let active = parse_ledger(LEDGER);
        assert_eq!(
            active.iter().map(String::as_str).collect::<Vec<_>>(),
            ["D-0001", "D-0004"]
        );
    }

    #[test]
    fn ledger_parsing_tolerates_decoration_and_ignores_prose() {
        let src = "## D-0007 — Interfaces\n\
                   * **Status**: accepted\n\
                   **Supersedes:** none\n\
                   Some prose mentioning D-0001 that is not a Supersedes field.\n";
        let active = parse_ledger(src);
        assert_eq!(
            active.iter().map(String::as_str).collect::<Vec<_>>(),
            ["D-0007"]
        );
    }

    #[test]
    fn ledger_parsing_of_junk_yields_an_empty_set_rather_than_an_error() {
        assert!(parse_ledger("").is_empty());
        assert!(parse_ledger("# Not a ledger\n\nJust prose about D-0001.\n").is_empty());
        // A heading that is not a decision id opens no entry.
        assert!(parse_ledger("## Decisions\n- **Status:** Accepted\n").is_empty());
        // Malformed ids are not decisions.
        assert!(parse_ledger("## D-12 — short\n- **Status:** Accepted\n").is_empty());
    }

    #[test]
    fn the_real_vault_ledger_parses_to_a_plausible_active_set() {
        // Not the repo's file (tests must not read the vault) — a faithful
        // excerpt of its shape, including the em-dash titles and field order.
        let active = parse_ledger(LEDGER);
        assert!(active.contains("D-0001"));
        assert!(!active.contains("D-0002"), "superseded");
        assert!(!active.contains("D-0003"), "not accepted");
    }
}
