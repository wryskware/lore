//! `.lore.toml` — the repository's own committed Lore configuration (D-0012).
//!
//! Authority semantics are **repository-opt-in**. A registered repo that does
//! not ship a `.lore.toml` naming a profile gets neutral retrieval: no
//! `design_status`/`decision_refs` parsing, no ledger parse, no path ceilings,
//! no authority metadata on results and no authority weights in ranking. Only
//! this file turns any of that on, it is committed so it follows the repo
//! across machines and contributors, and it lives at the **registered root**
//! only — a nested `.lore.toml` in a subdirectory is not configuration, it is
//! a file like any other.
//!
//! # Strictness
//!
//! Same posture as [`crate::config`], for the same reason: a misspelled key
//! must not be silently ignored. `deny_unknown_fields` everywhere, `profile`
//! is required once an `[authority]` table exists, and an unknown profile name
//! is an error rather than a fallback.
//!
//! # Failing visibly
//!
//! A malformed file, an unknown profile or an unknown behavior does **not**
//! stop the repo from being indexed, and does **not** silently select some
//! other authority model. The repo indexes exactly as an unconfigured one
//! ([`RepoAuthority::active`] is `None`) and the error rides along in
//! [`RepoAuthority::error`], where `lore status` and the refresh diagnostics
//! put it in front of the user. D-0012's requirement is that the failure is
//! loud, not that it is fatal.

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

/// File name at the registered repo root. Nested copies are ignored.
pub const REPO_CONFIG_FILE: &str = ".lore.toml";

/// An authority profile Lore knows how to apply.
///
/// This enum is the registry seam D-0012 asks for. `adr` (a MADR-based
/// conventional profile) is a *reserved name and nothing more* — it is
/// deliberately not a variant here, so declaring it produces the same visible
/// error as any other unknown profile rather than a half-implemented model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The Lexomancy-derived D-NNNN workflow: `design_status` frontmatter,
    /// `decision_refs`, a `**/0_Canon/DECISIONS.md` ledger, per-file decision
    /// records (D-0013) and the `9_Scratch`/`7_Research` path ceilings.
    LoreV1,
}

impl Profile {
    pub const LORE_V1: &'static str = "lore-v1";

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            Self::LORE_V1 => Some(Profile::LoreV1),
            _ => None,
        }
    }

    /// Canonical on-disk / on-the-wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::LoreV1 => Self::LORE_V1,
        }
    }

    /// Tag mixed into the stored content hash of profile-sensitive files.
    ///
    /// Whether a Markdown file's frontmatter is interpreted at all is a
    /// function of the profile, so the *same bytes* chunk differently under a
    /// different profile. Riding in the hash (exactly as
    /// [`crate::chunk::CHUNK_FORMAT_VERSION`] does) is what makes
    /// adding/changing/removing `.lore.toml` re-chunk the repo's Markdown
    /// instead of leaving stale metadata behind forever.
    pub fn chunk_tag(self) -> &'static str {
        match self {
            Profile::LoreV1 => "+lore-v1",
        }
    }
}

/// How far a declared profile is allowed to reach.
///
/// Activation and reranking are deliberately *not* the same act (D-0012):
/// annotation's value is far better evidenced than reranking's, so the
/// conservative half is the default when the key is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Behavior {
    /// Declared but suspended: index exactly like an unconfigured repo. The
    /// profile still shows up in `lore status`, so "off" is visible rather
    /// than indistinguishable from never having configured anything.
    Off,
    /// Compute and expose all authority metadata; leave ordering alone.
    #[default]
    Annotate,
    /// Annotate *and* apply the authority weights in ranking.
    Rank,
}

impl Behavior {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "off" => Behavior::Off,
            "annotate" => Behavior::Annotate,
            "rank" => Behavior::Rank,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Behavior::Off => "off",
            Behavior::Annotate => "annotate",
            Behavior::Rank => "rank",
        }
    }
}

/// A repository's resolved authority configuration — the thing the indexer,
/// the store and ranking all read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoAuthority {
    /// The profile the repo declared, when Lore understands it. `None` for an
    /// unconfigured repo *and* for a config error: both index neutrally.
    pub profile: Option<Profile>,
    /// Only meaningful when [`Self::profile`] is `Some`.
    pub behavior: Behavior,
    /// A config problem to shout about. Present with `profile: None` — the
    /// repo still indexes, neutrally.
    pub error: Option<String>,
}

impl RepoAuthority {
    /// The profile whose semantics are actually in force.
    ///
    /// `None` for an unconfigured repo, a config error, **and** for a declared
    /// profile suspended with `behavior = "off"` — all three index identically.
    pub fn active(&self) -> Option<Profile> {
        match self.behavior {
            Behavior::Off => None,
            _ => self.profile,
        }
    }

    /// Is authority metadata computed and exposed on results?
    pub fn annotates(&self) -> bool {
        self.active().is_some()
    }

    /// Are the authority weights applied to result ordering?
    pub fn ranks(&self) -> bool {
        self.profile.is_some() && self.behavior == Behavior::Rank
    }

    /// Read `<root>/.lore.toml`. Never fails: every problem becomes a neutral
    /// repo plus a visible [`Self::error`].
    pub fn load(root: &Utf8Path) -> Self {
        let path = root.join(REPO_CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => Self::failed(format!("reading {REPO_CONFIG_FILE}: {err}")),
        }
    }

    pub fn parse(text: &str) -> Self {
        let file: RepoConfigFile = match toml::from_str(text) {
            Ok(file) => file,
            Err(err) => {
                return Self::failed(format!(
                    "{REPO_CONFIG_FILE} is not valid Lore configuration: {}",
                    err.message()
                ));
            }
        };
        // A `.lore.toml` with no `[authority]` table is not an error: it
        // declares no profile, which means exactly what no file means. (An
        // *unknown* table still is one — a misspelled `[authority]` must not
        // read as "this repo opted out".)
        let Some(authority) = file.authority else {
            return Self::default();
        };
        let Some(profile) = Profile::parse(&authority.profile) else {
            return Self::failed(format!(
                "unknown authority profile `{}` (this build knows `{}`)",
                authority.profile,
                Profile::LORE_V1,
            ));
        };
        let behavior = match authority.behavior.as_deref() {
            None => Behavior::default(),
            Some(name) => match Behavior::parse(name) {
                Some(behavior) => behavior,
                None => {
                    return Self::failed(format!(
                        "unknown authority behavior `{name}` (expected off, annotate or rank)"
                    ));
                }
            },
        };
        Self {
            profile: Some(profile),
            behavior,
            error: None,
        }
    }

    fn failed(message: String) -> Self {
        Self {
            profile: None,
            behavior: Behavior::default(),
            error: Some(message),
        }
    }
}

/// The literal file shape. Private: callers work with [`RepoAuthority`], which
/// has already resolved the strings into the vocabulary the daemon uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct RepoConfigFile {
    authority: Option<AuthorityTable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthorityTable {
    /// Required once the table exists: a table with no profile declares
    /// nothing, and guessing one is exactly the silent behavior D-0012 bans.
    profile: String,
    /// Absent means [`Behavior::Annotate`].
    #[serde(default)]
    behavior: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_or_authority_less_file_is_a_neutral_repo() {
        // An empty file is exactly an absent one — the same rule
        // `config.toml` follows.
        assert_eq!(RepoAuthority::parse(""), RepoAuthority::default());
        assert_eq!(
            RepoAuthority::parse("# only a comment\n"),
            RepoAuthority::default()
        );
        let neutral = RepoAuthority::default();
        assert_eq!(neutral.active(), None);
        assert!(!neutral.annotates() && !neutral.ranks());
        assert_eq!(neutral.error, None);
    }

    #[test]
    fn a_declared_profile_defaults_to_annotate() {
        let annotating = RepoAuthority::parse("[authority]\nprofile = \"lore-v1\"\n");
        assert_eq!(annotating.profile, Some(Profile::LoreV1));
        assert_eq!(annotating.behavior, Behavior::Annotate);
        assert!(annotating.annotates(), "metadata is computed");
        assert!(!annotating.ranks(), "ordering is untouched");

        let ranking =
            RepoAuthority::parse("[authority]\nprofile = \"lore-v1\"\nbehavior = \"rank\"\n");
        assert!(ranking.annotates() && ranking.ranks());
    }

    /// `off` keeps the declaration visible while suspending every effect.
    #[test]
    fn off_indexes_like_an_unconfigured_repo_but_still_reports_the_profile() {
        let off = RepoAuthority::parse("[authority]\nprofile = \"lore-v1\"\nbehavior = \"off\"\n");
        assert_eq!(off.profile, Some(Profile::LoreV1), "status still shows it");
        assert_eq!(off.active(), None, "…and nothing is parsed or applied");
        assert!(!off.annotates() && !off.ranks());
        assert_eq!(off.error, None);
    }

    /// Every failure mode indexes neutrally *and* says so. Silence would mean
    /// a repo quietly running a different authority model than its file says.
    #[test]
    fn every_config_failure_is_neutral_and_loud() {
        for text in [
            // Unknown profile — including the reserved-but-unimplemented name.
            "[authority]\nprofile = \"adr\"\n",
            "[authority]\nprofile = \"lore-v2\"\n",
            // Unknown behavior.
            "[authority]\nprofile = \"lore-v1\"\nbehavior = \"prefer\"\n",
            // Misspelled key: silently ignoring it would present as "the
            // profile mysteriously never turned on".
            "[authority]\nprofil = \"lore-v1\"\n",
            "[authority]\nprofile = \"lore-v1\"\nbehaviour = \"rank\"\n",
            // Missing the required key, and outright malformed TOML.
            "[authority]\n",
            "[authority\nprofile =",
            // An unknown *table* is a misspelled `[authority]`.
            "[authorities]\nprofile = \"lore-v1\"\n",
        ] {
            let parsed = RepoAuthority::parse(text);
            assert_eq!(parsed.active(), None, "{text:?} must index neutrally");
            assert!(!parsed.annotates() && !parsed.ranks(), "{text:?}");
            assert!(parsed.error.is_some(), "{text:?} must be visible");
        }
    }

    #[test]
    fn load_reads_the_root_file_and_tolerates_its_absence() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        assert_eq!(RepoAuthority::load(&root), RepoAuthority::default());

        std::fs::write(
            root.join(REPO_CONFIG_FILE),
            "[authority]\nprofile = \"lore-v1\"\nbehavior = \"rank\"\n",
        )
        .unwrap();
        assert!(RepoAuthority::load(&root).ranks());

        // Only the registered root counts: a nested copy is an ordinary file.
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(
            root.join("nested").join(REPO_CONFIG_FILE),
            "not toml at [all",
        )
        .unwrap();
        assert!(RepoAuthority::load(&root).ranks(), "nested files are inert");
    }
}
