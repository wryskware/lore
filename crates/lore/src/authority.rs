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
//! ledger entry, and `99_Scratch` notes — which the same README places at the
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
//!    `99_Scratch` → [`SCRATCH_TIER_CEILING`], `7_Research` →
//!    [`RESEARCH_TIER_CEILING`]. A declared `deprecated` is already 0 and no
//!    ceiling can raise it.
//! 5. A `session` source is capped at [`SESSION_TIER_CEILING`], below vault
//!    material (3.1's required cap; the exact value is M3 tuning).
//!
//! Citations carry **no** ranking weight of their own any more: they remain
//! visible metadata on every result, and that is all. Authority flows *from*
//! the ledger, never *to* it from whoever quotes a number.
//!
//! # None of this is on by default (D-0012)
//!
//! Every rule above is **profile-gated**. [`effective`] takes the repository's
//! active [`Profile`], and `None` — an unconfigured repo, a broken
//! `.lore.toml`, or a profile suspended with `behavior = "off"` — means the
//! whole vault policy is skipped: no ledger pin, no `decided` validation, no
//! path ceilings, and the declared status is not consulted at all (a neutral
//! repo does not parse `design_status` in the first place, so there is nothing
//! to consult). A repository that never adopted the vault workflow must not
//! acquire accidental directory or frontmatter semantics.
//!
//! The one rule that is *not* gated is the `session` source cap: it is a
//! statement about corpus provenance (D-0006/D-0008), not about a vault
//! convention, and the session corpus is the daemon's own — it has no repo to
//! commit a `.lore.toml` to.

use std::collections::{BTreeMap, BTreeSet};

use camino::{Utf8Path, Utf8PathBuf};

use crate::repo_config::Profile;
use crate::types::{DesignStatus, SourceKind, authority_tier};

/// Directory that contains a project's decision ledger, by convention.
pub const LEDGER_DIR: &str = "0_Canon";

/// The ledger file itself, inside [`LEDGER_DIR`].
pub const LEDGER_FILE: &str = "DECISIONS.md";

/// Directory of one-decision-per-file records, inside [`LEDGER_DIR`] (D-0013).
pub const RECORDS_DIR: &str = "decisions";

/// The ledger is pinned here regardless of what its own frontmatter says.
pub const LEDGER_TIER: u8 = 3;

/// Where a `decided` declaration lands when it cites no active decision.
/// Neutral on purpose — visible and searchable, just not authoritative.
pub const UNCITED_DECIDED_TIER: u8 = 1;

/// Path segment for scratch material (README authority order #6).
pub const SCRATCH_SEGMENT: &str = "99_Scratch";

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
    /// Under a `99_Scratch` directory.
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
            Demotion::ScratchPath => "99_Scratch path cap",
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

/// Is this project-relative path a per-file decision record, and if so which
/// decision does its **filename** claim (D-0013)?
///
/// Convention, positional like [`is_ledger_path`]: the last three segments
/// must be `0_Canon/decisions/D-NNNN-<slug>.md`. The filename's `D-NNNN`
/// prefix is authoritative for identity — a heading that disagrees is a
/// violation, not an alternative source of truth — so this returns the id
/// rather than a bare bool.
///
/// The `-<slug>` is required: `D-0012.md` is not a record. A record filename
/// is meant to say what the decision is at a glance, and accepting the bare
/// form would make `D-0012.md` and `D-0012-authority.md` two records for one
/// id purely by accident.
pub fn decision_record_id(path: &Utf8Path) -> Option<String> {
    let mut segments = path.iter().rev();
    let file = segments.next()?;
    if !segment_eq(segments.next()?, RECORDS_DIR) || !segment_eq(segments.next()?, LEDGER_DIR) {
        return None;
    }
    let stem = file
        .strip_suffix(".md")
        .or_else(|| file.strip_suffix(".MD"))?;
    let id = leading_decision_id(stem)?;
    // `D-0012` followed by anything other than `-slug` is not a record.
    stem[id.len()..].strip_prefix('-')?;
    Some(id)
}

/// Does this path feed the project's active-decision set — a mono ledger or a
/// per-file record? The one predicate the incremental index pass asks before
/// deciding whether a batch is worth a recompute.
pub fn is_decision_source(path: &Utf8Path) -> bool {
    is_ledger_path(path) || decision_record_id(path).is_some()
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
    profile: Option<Profile>,
) -> Authority {
    // No active profile: the vault policy below does not exist for this repo
    // (D-0012). Everything is neutral, including files whose *path* happens to
    // spell `99_Scratch` and files whose frontmatter happens to say `decided` —
    // a repository that never opted in must not acquire either meaning.
    let Some(Profile::LoreV1) = profile else {
        let mut tier = authority_tier(None);
        let mut demotion = None;
        if kind == SourceKind::Session {
            tier = SESSION_TIER_CEILING.min(tier);
            demotion = Some(Demotion::SessionSource);
        }
        return Authority { tier, demotion };
    };

    // The ledger outranks everything, including its own frontmatter. Per-file
    // decision records get the same pin: they are the same canon in a
    // different file layout (D-0013).
    if is_ledger_path(path) || decision_record_id(path).is_some() {
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
/// *and* no **accepted** entry's `Supersedes` field retires it — which is how
/// an append-only ledger retires canon without rewriting history (README
/// promotion rules).
///
/// A `Supersedes` field retires an entry only when its value is a **bare ID
/// list** ("D-0004." / "D-0002, D-0003" / "D-0002 and D-0003") — D-0010.
/// Real ledgers supersede *parts* of decisions in qualified prose ("D-0002's
/// consumed-by-inscription clause only; the rest stands", "None (extends
/// D-0015)"), and the entry's surviving clauses remain the canonical statement
/// of what stands, so a partial supersession must leave it active. Any
/// non-ID token in the value makes the whole field partial: it retires
/// nothing.
///
/// The accepted-only restriction on `Supersedes` is load-bearing, not
/// tidiness. Canon explicitly invites agents to "draft proposed entries but
/// leave them unpromoted" (README §Promotion rules), so a `Proposed` — or
/// `Rejected` — entry naming `Supersedes: D-0001` is an ordinary, expected
/// state of the file. Letting it retire D-0001 would hand any agent that can
/// append to the ledger the power to deactivate live canon without
/// authorization, and would demote every document citing it. Supersession is
/// an act of accepted canon or it is nothing.
///
/// Fields are collected per entry and resolved when the entry closes, so a
/// `Supersedes` line that appears *before* its own `Status` line is judged by
/// the same rule as one that appears after it.
///
/// Deliberately lenient about decoration (`- `, `* `, bold markers, the `:`
/// inside or outside the emphasis) and deliberately strict about the id shape
/// (`D-` + exactly four digits), matching the chunker's `decision_refs`.
/// Unparseable prose is skipped, never an error: a malformed ledger degrades
/// to a smaller active set — which demotes over-claiming documents — rather
/// than failing the index pass.
pub fn parse_ledger(src: &str) -> BTreeSet<String> {
    let mut index = DecisionIndex::default();
    index.add_ledger(Utf8Path::new(LEDGER_FILE), src);
    index.resolve().active
}

/// One parsed decision, before supersession is resolved. Identical shape
/// whether it came from a mono-ledger heading or a per-file record (D-0013):
/// same field grammar, same D-0010 supersession semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionEntry {
    pub id: String,
    pub accepted: bool,
    pub supersedes: Vec<String>,
}

/// A decision-set problem worth putting in front of the author.
///
/// Distinct from [`Demotion`]: a demotion is a verdict about one *document's*
/// ranking, this is a defect in the decision corpus itself — a record whose
/// heading and filename disagree, or two records claiming one id. Both kinds
/// end up in `lore status`, from different columns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DecisionViolation {
    /// Project-relative path of the offending file.
    pub path: Utf8PathBuf,
    pub detail: String,
}

impl std::fmt::Display for DecisionViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.detail)
    }
}

/// The resolved decision corpus of one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decisions {
    /// Accepted and not retired — the set `decided` declarations are validated
    /// against.
    pub active: BTreeSet<String>,
    /// Distinct decisions that survived identity checks, whatever their
    /// status. `active.len() / total` is the "12/15 decisions active" figure
    /// `lore status` reports; excluded records are counted as violations
    /// instead, not as decisions.
    pub total: usize,
    pub violations: Vec<DecisionViolation>,
}

/// Accumulates every decision source in a project, then resolves them together.
///
/// Together, deliberately: a per-file record may supersede a mono-ledger entry
/// and vice versa (D-0013 says the two formats coexist), so resolution cannot
/// happen per file.
#[derive(Debug, Default)]
pub struct DecisionIndex {
    entries: Vec<Origin>,
    violations: Vec<DecisionViolation>,
}

#[derive(Debug)]
struct Origin {
    path: Utf8PathBuf,
    entry: DecisionEntry,
    /// From a mono ledger (as opposed to a per-file record). The duplicate-id
    /// rule is asymmetric, so the provenance has to survive parsing.
    mono: bool,
}

impl DecisionIndex {
    /// Add every `## D-NNNN` entry of a mono ledger.
    ///
    /// Unparseable prose is skipped, never an error: a malformed ledger
    /// degrades to a smaller active set — which demotes over-claiming
    /// documents — rather than failing the index pass.
    pub fn add_ledger(&mut self, path: &Utf8Path, src: &str) {
        for entry in ledger_entries(src) {
            self.entries.push(Origin {
                path: path.to_owned(),
                entry,
                mono: true,
            });
        }
    }

    /// Add one per-file decision record (D-0013). `path` must have already
    /// been recognized by [`decision_record_id`]; the id it yields is the
    /// record's identity, and a heading that disagrees is a violation rather
    /// than a second opinion.
    pub fn add_record(&mut self, path: &Utf8Path, src: &str) {
        let Some(id) = decision_record_id(path) else {
            return;
        };
        match parse_record(&id, src) {
            Ok(entry) => self.entries.push(Origin {
                path: path.to_owned(),
                entry,
                mono: false,
            }),
            Err(detail) => self.violations.push(DecisionViolation {
                path: path.to_owned(),
                detail,
            }),
        }
    }

    /// Resolve identity collisions, then supersession.
    pub fn resolve(mut self) -> Decisions {
        // -- identity ------------------------------------------------------
        //
        // Duplicate ids are resolved before anything is believed, because an
        // ambiguous id makes both `Supersedes: D-NNNN` and a document's
        // citation of it mean two different things at once.
        let mut by_id: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, origin) in self.entries.iter().enumerate() {
            by_id
                .entry(origin.entry.id.as_str())
                .or_default()
                .push(index);
        }

        let mut excluded: BTreeSet<usize> = BTreeSet::new();
        let mut collisions: Vec<DecisionViolation> = Vec::new();
        for (id, indices) in &by_id {
            if indices.len() < 2 {
                continue;
            }
            let mono: Vec<usize> = indices
                .iter()
                .copied()
                .filter(|i| self.entries[*i].mono)
                .collect();
            let records: Vec<usize> = indices
                .iter()
                .copied()
                .filter(|i| !self.entries[*i].mono)
                .collect();

            if !mono.is_empty() && !records.is_empty() {
                // The mono ledger wins and stays active; every record claiming
                // the same id is excluded. Deciding in favour of the ledger
                // keeps the *existing* corpus authoritative during a migration,
                // so half-migrating a vault cannot deactivate live canon.
                let ledger = self.entries[mono[0]].path.clone();
                for index in records {
                    excluded.insert(index);
                    collisions.push(DecisionViolation {
                        path: self.entries[index].path.clone(),
                        detail: format!(
                            "duplicate decision id {id}: the ledger entry in {ledger} holds it, \
                             so this record is excluded from the active set"
                        ),
                    });
                }
                continue;
            }
            if mono.is_empty() {
                // Two records, no ledger entry: there is no principled winner,
                // so neither is believed and both are named.
                for index in records {
                    excluded.insert(index);
                    collisions.push(DecisionViolation {
                        path: self.entries[index].path.clone(),
                        detail: format!(
                            "duplicate decision id {id} across per-file records; \
                             all of them are excluded from the active set"
                        ),
                    });
                }
            }
            // Two *mono* entries sharing an id means two ledgers in one repo.
            // Their active sets are unioned rather than reconciled — see the
            // note on `Decisions` and D-0012's deferred multi-root namespace
            // resolution.
        }
        self.violations.extend(collisions);
        self.violations
            .sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.detail.cmp(&b.detail)));

        // -- supersession --------------------------------------------------
        let mut accepted: BTreeSet<String> = BTreeSet::new();
        let mut superseded: BTreeSet<String> = BTreeSet::new();
        let mut total: BTreeSet<String> = BTreeSet::new();
        for (index, origin) in self.entries.iter().enumerate() {
            if excluded.contains(&index) {
                continue;
            }
            total.insert(origin.entry.id.clone());
            if origin.entry.accepted {
                accepted.insert(origin.entry.id.clone());
                superseded.extend(origin.entry.supersedes.iter().cloned());
            }
        }

        Decisions {
            active: accepted
                .into_iter()
                .filter(|id| !superseded.contains(id))
                .collect(),
            total: total.len(),
            violations: self.violations,
        }
    }
}

/// Every `## D-NNNN` entry of a mono ledger, in file order.
fn ledger_entries(src: &str) -> Vec<DecisionEntry> {
    let mut out: Vec<DecisionEntry> = Vec::new();
    let mut current: Option<DecisionEntry> = None;

    for line in src.trim_start_matches('\u{feff}').lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("##") {
            // Any heading closes the previous entry; only a `D-NNNN` one opens
            // a new entry, so a trailing "## Notes" section cannot absorb
            // fields into the last decision.
            out.extend(current.take());
            let rest = rest.trim_start_matches('#');
            current = leading_decision_id(rest).map(|id| DecisionEntry {
                id,
                accepted: false,
                supersedes: Vec::new(),
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        absorb_field(entry, trimmed);
    }
    out.extend(current);
    out
}

/// Parse one per-file decision record (D-0013).
///
/// `file_id` is the authoritative identity, taken from the filename. The
/// record's **first** heading identifies it: if that heading carries a
/// `D-NNNN` it must be `file_id`, and the entry's fields are the lines between
/// it and the next heading. A first heading with no id (a record titled after
/// its subject rather than its number) is fine — the filename already said
/// which decision this is — and the fields are then read from the whole file.
///
/// Only the first heading is consulted on purpose: a record that *quotes*
/// `## D-0013 — something else` further down is discussing another decision,
/// not claiming to be one, and flagging that as a mismatch would punish
/// ordinary prose.
///
/// Both `# ` and `## ` are accepted as the heading level: a one-decision file
/// has an equally good claim to either, and rejecting one would make the
/// format a formatting trap.
fn parse_record(file_id: &str, src: &str) -> Result<DecisionEntry, String> {
    let mut entry = DecisionEntry {
        id: file_id.to_string(),
        accepted: false,
        supersedes: Vec::new(),
    };

    let src = src.trim_start_matches('\u{feff}');
    let mut lines = src.lines();
    let mut scoped = false;
    for line in lines.by_ref() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix('#') else {
            continue;
        };
        let rest = rest.trim_start_matches('#');
        match leading_decision_id(rest) {
            Some(heading_id) if heading_id != file_id => {
                return Err(format!(
                    "decision record names {heading_id} in its heading but the filename \
                     claims {file_id}; the filename is authoritative, so this record is \
                     excluded from the active set until they agree"
                ));
            }
            Some(_) => scoped = true,
            // The first heading identifies the record whether or not it
            // carries the id; a title-only heading simply scopes nothing.
            None => {}
        }
        break;
    }

    if scoped {
        // Fields belong to this entry until the next heading of any level.
        for line in lines {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                break;
            }
            absorb_field(&mut entry, trimmed);
        }
    } else {
        for line in src.lines() {
            absorb_field(&mut entry, line.trim_start());
        }
    }
    Ok(entry)
}

/// Apply one `- **Status:** …` / `- **Supersedes:** …` line to an entry.
fn absorb_field(entry: &mut DecisionEntry, trimmed: &str) {
    if let Some(value) = field_value(trimmed, "Status") {
        entry.accepted = value
            .trim()
            .trim_end_matches(['*', '.', ' '])
            .eq_ignore_ascii_case("accepted");
    } else if let Some(value) = field_value(trimmed, "Supersedes") {
        entry.supersedes.extend(bare_supersedes_list(value));
    }
}

/// IDs from a `Supersedes` field value, honored only when the value is a
/// bare ID list (D-0010). Tokens may carry list decoration (`[[..]]`,
/// emphasis, parentheses, `.,;`) and be joined by "and"/"&"; any other token
/// — a possessive ("D-0002's"), a qualifier ("in part"), a negation ("None
/// (extends D-0015)") — marks the field as partial prose and retires nothing.
fn bare_supersedes_list(value: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for token in value.split_whitespace() {
        let token =
            token.trim_matches(|c| matches!(c, '[' | ']' | '*' | '(' | ')' | '.' | ',' | ';'));
        if token.is_empty() || token.eq_ignore_ascii_case("and") || token == "&" {
            continue;
        }
        let bytes = token.as_bytes();
        let shaped = bytes.len() == 6
            && bytes[0] == b'D'
            && bytes[1] == b'-'
            && bytes[2..6].iter().all(u8::is_ascii_digit);
        if !shaped {
            return Vec::new();
        }
        ids.push(token.to_string());
    }
    ids
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
            Some(Profile::LoreV1),
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
            "design/99_Scratch/notes.md",
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
        assert_eq!(tier("99_Scratch/spike/main.rs", None, &[], &[]).tier, 0);
    }

    #[test]
    fn a_violation_keeps_the_note_even_when_a_path_cap_bites_harder() {
        let both = tier(
            "design/99_Scratch/x.md",
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
            Some(Profile::LoreV1),
        );
        assert_eq!(session.tier, SESSION_TIER_CEILING);
        assert_eq!(session.demotion, Some(Demotion::SessionSource));

        // The session cap is provenance, not a vault convention, so it is the
        // one rule that survives a profile-less source (D-0006/D-0008): the
        // session corpus is the daemon's own and has no repo to configure.
        let neutral = effective(
            &Utf8PathBuf::from("sessions/2026-08-14-thread.md"),
            Declared {
                status: Some(DesignStatus::Decided),
                decision_refs: &cited,
            },
            &active(&["D-0007"]),
            SourceKind::Session,
            None,
        );
        assert_eq!(neutral.tier, SESSION_TIER_CEILING);
        assert_eq!(neutral.demotion, Some(Demotion::SessionSource));
    }

    /// D-0012's core promise: a repository that never opted in acquires no
    /// directory semantics, no frontmatter semantics, and no ledger pin.
    #[test]
    fn without_a_profile_nothing_declares_anything() {
        let neutral = |path: &str, status: Option<DesignStatus>| {
            let cited = refs(&["D-0001"]);
            effective(
                &Utf8PathBuf::from(path),
                Declared {
                    status,
                    decision_refs: &cited,
                },
                &active(&["D-0001"]),
                SourceKind::Repo,
                None,
            )
        };
        for (path, status) in [
            ("design/0_Canon/DECISIONS.md", None),
            ("design/0_Canon/decisions/D-0012-profiles.md", None),
            ("design/99_Scratch/notes.md", Some(DesignStatus::Deprecated)),
            ("design/7_Research/raw/E.md", Some(DesignStatus::Leaning)),
            ("design/x.md", Some(DesignStatus::Decided)),
            ("docs/y.md", None),
        ] {
            let verdict = neutral(path, status);
            assert_eq!(
                verdict.tier,
                authority_tier(None),
                "{path} must be neutral without a profile"
            );
            assert_eq!(verdict.demotion, None, "{path}");
            assert!(!verdict.is_violation(), "{path}");
        }
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

    /// Every phrasing here is lifted from the live Lexomancy ledger, which
    /// supersedes *parts* of decisions in qualified prose. Harvesting any
    /// D-NNNN mention retired D-0002/D-0003/D-0005/D-0013/D-0014/D-0015
    /// against the ledger's own words — "the rest of D-0002 stands",
    /// "**Supersedes:** None (extends D-0015)" (D-0010).
    #[test]
    fn a_qualified_or_negated_supersession_is_partial_and_retires_nothing() {
        let src = "# Ledger\n\n\
                   ## D-0002 — Forge economy\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
                   ## D-0013 — Wildcards\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
                   ## D-0014 — Preview strip\n\n- **Status:** Accepted\n\
                   - **Supersedes:** none (refines [[D-0013]]'s presentation half).\n\n\
                   ## D-0015 — Unified cast\n\n- **Status:** Accepted\n\
                   - **Supersedes:** D-0014 in the single Focus-chip-multiplier detail; otherwise none.\n\n\
                   ## D-0016 — Targeting\n\n- **Status:** Accepted\n\
                   - **Supersedes:** None (extends D-0015).\n\n\
                   ## D-0017 — Inscription\n\n- **Status:** Accepted\n\
                   - **Supersedes:** D-0002's \"consumed by inscription\" clause only; the rest stands.\n";
        assert_eq!(
            parse_ledger(src)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["D-0002", "D-0013", "D-0014", "D-0015", "D-0016", "D-0017"],
            "qualified prose must not retire the entries it mentions"
        );
    }

    #[test]
    fn a_bare_id_list_retires_every_named_entry() {
        let src = "# Ledger\n\n\
                   ## D-0001 — First\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
                   ## D-0002 — Second\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
                   ## D-0003 — Third\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
                   ## D-0004 — Sweep\n\n- **Status:** Accepted\n\
                   - **Supersedes:** [[D-0001]], D-0002 and D-0003.\n";
        assert_eq!(
            parse_ledger(src)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["D-0004"],
            "a decorated bare list is still a bare list"
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

    /// Canon invites agents to draft proposed entries and leave them
    /// unpromoted (README §Promotion rules). A drafted entry that *proposes*
    /// replacing D-0001 is therefore an ordinary state of the file — and it
    /// must not be able to retire D-0001 on its own say-so, because that would
    /// let any agent with write access to the ledger deactivate live canon and
    /// mass-demote every document citing it.
    #[test]
    fn an_unpromoted_entrys_supersedes_cannot_retire_live_canon() {
        let src = "# Ledger\n\n\
                   ## D-0001 — Live canon\n\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
                   ## D-0009 — A draft an agent left behind\n\n\
                   - **Status:** Proposed\n- **Supersedes:** D-0001\n\n\
                   ## D-0010 — Considered and declined\n\n\
                   - **Status:** Rejected\n- **Supersedes:** D-0001\n";
        assert_eq!(
            parse_ledger(src)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["D-0001"],
            "only an accepted entry may supersede"
        );

        // The moment the same entry *is* accepted, the supersession takes
        // effect — proving the gate is the status and nothing else.
        let promoted = src.replace("- **Status:** Proposed", "- **Status:** Accepted");
        assert_eq!(
            parse_ledger(&promoted)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["D-0009"]
        );
    }

    /// Field order inside an entry is a formatting choice, so the accepted-only
    /// rule cannot depend on `Status` being read before `Supersedes`.
    #[test]
    fn supersedes_is_judged_by_status_whichever_line_comes_first() {
        let ignored = "## D-0001 — Canon\n- **Status:** Accepted\n\n\
                       ## D-0009 — Draft\n- **Supersedes:** D-0001\n- **Status:** Proposed\n";
        assert!(parse_ledger(ignored).contains("D-0001"));

        let honored = "## D-0001 — Canon\n- **Status:** Accepted\n\n\
                       ## D-0009 — Replacement\n- **Supersedes:** D-0001\n- **Status:** Accepted\n";
        assert_eq!(
            parse_ledger(honored)
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["D-0009"]
        );
    }

    // -- per-file decision records (D-0013) --------------------------------

    fn record(id: &str) -> Utf8PathBuf {
        Utf8PathBuf::from(format!("design/0_Canon/decisions/{id}-slug.md"))
    }

    #[test]
    fn a_record_path_is_recognized_by_its_filename_and_position() {
        assert_eq!(
            decision_record_id(&record("D-0012")).as_deref(),
            Some("D-0012")
        );
        assert_eq!(
            decision_record_id(&Utf8PathBuf::from(
                "Docs/0_Canon/decisions/D-0001-authority-model.md"
            ))
            .as_deref(),
            Some("D-0001")
        );
        for not_a_record in [
            "design/0_Canon/decisions/D-0012.md",    // no slug
            "design/0_Canon/decisions/notes.md",     // no id
            "design/0_Canon/D-0012-slug.md",         // wrong directory
            "design/decisions/D-0012-slug.md",       // no 0_Canon
            "design/0_Canon/decisions/D-12-a.md",    // malformed id
            "design/0_Canon/decisions/D-0012-a.txt", // not Markdown
        ] {
            assert_eq!(
                decision_record_id(&Utf8PathBuf::from(not_a_record)),
                None,
                "{not_a_record}"
            );
        }
        assert!(is_decision_source(&record("D-0012")));
        assert!(is_decision_source(&Utf8PathBuf::from(
            "design/0_Canon/DECISIONS.md"
        )));
    }

    /// Records and the mono ledger are one corpus: either may supersede the
    /// other, and both are pinned to the ledger tier.
    #[test]
    fn records_and_the_mono_ledger_resolve_together() {
        let mut index = DecisionIndex::default();
        index.add_ledger(
            Utf8Path::new("design/0_Canon/DECISIONS.md"),
            "## D-0001 — First\n- **Status:** Accepted\n- **Supersedes:** None\n\n\
             ## D-0002 — Second\n- **Status:** Accepted\n- **Supersedes:** None\n",
        );
        // `# ` heading level, and it retires a mono-ledger entry.
        index.add_record(
            &record("D-0003"),
            "# D-0003 — Replaces the second\n\n- **Status:** Accepted\n- **Supersedes:** D-0002\n",
        );
        // `## ` heading level, and merely proposed.
        index.add_record(
            &record("D-0004"),
            "## D-0004 — A draft\n\n- **Status:** Proposed\n- **Supersedes:** D-0001\n",
        );
        let resolved = index.resolve();
        assert_eq!(
            resolved
                .active
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["D-0001", "D-0003"],
            "a record supersedes exactly like a ledger entry, and an \
             unpromoted one retires nothing"
        );
        assert_eq!(resolved.total, 4);
        assert!(resolved.violations.is_empty());

        // Both formats are pinned canon.
        assert_eq!(
            tier("design/0_Canon/decisions/D-0003-x.md", None, &[], &[]).tier,
            LEDGER_TIER
        );
    }

    #[test]
    fn identity_defects_are_surfaced_and_excluded() {
        // Heading disagrees with the filename: excluded, and named.
        let mut index = DecisionIndex::default();
        index.add_record(
            &record("D-0005"),
            "# D-0006 — Copy-pasted from another record\n- **Status:** Accepted\n",
        );
        let resolved = index.resolve();
        assert!(resolved.active.is_empty());
        assert_eq!(resolved.total, 0);
        assert_eq!(resolved.violations.len(), 1);
        assert!(
            resolved.violations[0].detail.contains("D-0006")
                && resolved.violations[0].detail.contains("D-0005"),
            "{:?}",
            resolved.violations[0]
        );

        // Duplicate against the mono ledger: the ledger keeps the decision.
        let mut index = DecisionIndex::default();
        index.add_ledger(
            Utf8Path::new("design/0_Canon/DECISIONS.md"),
            "## D-0007 — Interfaces\n- **Status:** Accepted\n",
        );
        index.add_record(
            &record("D-0007"),
            "# D-0007 — Same number, different file\n- **Status:** Accepted\n",
        );
        let resolved = index.resolve();
        assert_eq!(
            resolved
                .active
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["D-0007"],
            "the ledger entry stays active"
        );
        assert_eq!(resolved.total, 1);
        assert_eq!(resolved.violations.len(), 1);

        // Two records colliding with each other: neither is believed.
        let mut index = DecisionIndex::default();
        for slug in ["a", "b"] {
            index.add_record(
                &Utf8PathBuf::from(format!("design/0_Canon/decisions/D-0008-{slug}.md")),
                "- **Status:** Accepted\n",
            );
        }
        let resolved = index.resolve();
        assert!(resolved.active.is_empty(), "{resolved:?}");
        assert_eq!(resolved.total, 0);
        assert_eq!(resolved.violations.len(), 2, "both are named");
    }

    /// A record whose first heading is a plain title is still identified by
    /// its filename, and its fields are still read.
    #[test]
    fn a_record_without_an_id_heading_is_identified_by_its_filename() {
        let mut index = DecisionIndex::default();
        index.add_record(
            &record("D-0009"),
            "---\ndesign_status: decided\n---\n\n# Authority is repository-opt-in\n\n\
             - **Status:** Accepted\n- **Supersedes:** None\n",
        );
        let resolved = index.resolve();
        assert_eq!(
            resolved
                .active
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["D-0009"]
        );
        assert!(resolved.violations.is_empty());

        // …and a `D-NNNN` heading further down is prose, not a claim.
        let mut index = DecisionIndex::default();
        index.add_record(
            &record("D-0010"),
            "# Supersession semantics\n\n- **Status:** Accepted\n\n\
             ## D-0099 — quoted for contrast\n\nprose\n",
        );
        assert!(index.resolve().violations.is_empty());
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
