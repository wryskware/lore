//! `lore setup` — installing Lore's agent-side assets into a coding agent host.
//!
//! Lore ships two halves. The daemon indexes; the agent has to know how to
//! *use* what it indexed, and some of that knowledge is procedural rather than
//! retrievable — "tune this project's `.loreignore` by measuring the repo" is a
//! method, not a fact, and no amount of search surfaces it at the moment it is
//! needed. Those methods ship as assets installed into whatever agent host the
//! user runs.
//!
//! The unit is therefore the **host**, not the asset kind: Claude Code keeps
//! skills in `~/.claude/skills`, wires MCP servers through its own config, and
//! reads `CLAUDE.md`; another host answers all three questions differently.
//! `lore setup <host>` installs everything Lore ships for one host, and bare
//! `lore setup` reports without writing anything — discovery must never be a
//! mutation.
//!
//! One target is not a host: `lore setup ignore` writes [`USER_IGNORE`] into
//! lore's own data directory. It lives here because it answers the same
//! question — what does this machine need for lore to be useful — and because
//! since D-0020 it is the *only* way lore excludes anything by default. The
//! evaluator ships no rules; this file is a starting point a human installs,
//! reads and owns.
//!
//! # What this deliberately does not do
//!
//! It does not edit `CLAUDE.md`, `AGENTS.md`, or any other file the repository
//! owns. Those encode a team's conventions, and a tool that appends to them is
//! guessing at a house style it cannot see. Integrating Lore into a repo's own
//! documentation is a job for an agent that has read the repo — a future
//! asset — not for this installer.
//!
//! # Never clobbering an edit
//!
//! An installed asset is meant to be edited; it is a prompt, and a user who
//! sharpens one is using it correctly. So each file carries a stamp naming the
//! Lore version that wrote it and the BLAKE3 hash of the body as shipped. On a
//! later `lore setup`, rehashing the on-disk body answers the only question
//! that matters — has the user touched this? — without a sidecar manifest that
//! could drift from the files it describes or vanish with the data directory.
//! Unmodified assets upgrade silently; modified ones are named and skipped
//! until `--force`.

use std::fmt;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

pub mod mcp;

/// One installable prompt asset.
///
/// `body` is embedded in the binary rather than read from disk at run time:
/// `lore` is distributed as a single executable, and an installer that needed
/// its source tree beside it would work only on the machine that built it.
pub struct Skill {
    pub name: &'static str,
    /// One line for the report. Not the skill's own `description:` — that one
    /// addresses the agent deciding whether to load it, this one addresses a
    /// human reading `lore setup`.
    pub summary: &'static str,
    pub body: &'static str,
}

pub const SKILLS: &[Skill] = &[
    Skill {
        name: "lore-search",
        summary: "how to actually use lore's index: search, expand, authority, when to grep",
        body: include_str!("assets/lore-search.md"),
    },
    Skill {
        name: "lore-ignore",
        summary: "tune a project's .loreignore by measuring the repo",
        body: include_str!("assets/lore-ignore.md"),
    },
];

/// The `lore setup` target that installs the user-level ignore rules. Not a
/// host: it writes into lore's own data directory.
pub const USER_IGNORE_TARGET: &str = "ignore";

/// A commented starting point for `<data-dir>/loreignore` — the lowest of the
/// three rule sources the walker evaluates (D-0020).
///
/// Shipped as an asset rather than compiled into the evaluator, which is the
/// substance of the decision: lore applies no ignore rules of its own, so every
/// exclusion is a line in a file a user can read, edit and delete. Installing it
/// is an explicit act (`lore setup ignore`), and an existing file at that
/// path is never overwritten — the never-clobber rule above covers it, because
/// a file lore did not stamp reads as the user's.
const USER_IGNORE: Skill = Skill {
    name: USER_IGNORE_TARGET,
    summary: "machine-wide ignore rules: dot-files, credentials, build output",
    body: include_str!("assets/user-loreignore"),
};

/// A coding agent Lore knows how to install into.
///
/// An enum rather than a registry of trait objects: hosts differ in ways that
/// are answered by `match`, not by configuration, and there is no case for
/// third parties adding one at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    ClaudeCode,
}

impl Host {
    pub const ALL: &'static [Host] = &[Host::ClaudeCode];

    pub const fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
        }
    }

    /// The host's per-user configuration directory. Its existence is what
    /// "detected" means — the host has run at least once for this user.
    fn home(self) -> Option<Utf8PathBuf> {
        let home = Utf8PathBuf::from_path_buf(dirs::home_dir()?).ok()?;
        match self {
            Self::ClaudeCode => Some(home.join(".claude")),
        }
    }

    pub fn detected(self) -> bool {
        self.home().is_some_and(|dir| dir.is_dir())
    }

    /// Where this host reads personal skills from.
    pub fn skills_dir(self) -> Result<Utf8PathBuf> {
        let home = self
            .home()
            .context("cannot locate your home directory; set HOME (or USERPROFILE)")?;
        match self {
            Self::ClaudeCode => Ok(home.join("skills")),
        }
    }

    pub fn parse(id: &str) -> Result<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|host| host.id() == id)
            .with_context(|| {
                let known = Self::ALL
                    .iter()
                    .map(|host| host.id())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unknown host `{id}`; lore ships assets for: {known}")
            })
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

// ---------------------------------------------------------------------------
// The stamp
// ---------------------------------------------------------------------------

/// How the stamp line is commented out, in the asset's own syntax.
///
/// A comment is the one construct that is inert in every format Lore ships — a
/// frontmatter key would be extra surface for a host's schema validation to
/// reject, and in a gitignore file it would be a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    prefix: &'static str,
    suffix: &'static str,
}

/// Markdown, for the agent-host skills.
const MARKDOWN: Stamp = Stamp {
    prefix: "<!-- lore-asset:",
    suffix: " -->",
};

/// Gitignore syntax, for the user-level ignore rules. `#` to end of line, so
/// there is nothing to close.
const IGNORE: Stamp = Stamp {
    prefix: "# lore-asset:",
    suffix: "",
};

fn hash(body: &str) -> String {
    blake3::hash(body.as_bytes()).to_hex().to_string()
}

/// The shipped body plus its stamp — exactly the bytes that hit disk.
///
/// Trailing whitespace is normalised away before hashing so that the hash
/// [`unstamp`] can recompute from disk is the hash written here. Without that
/// every asset would read as locally modified the moment it was installed.
fn stamped(body: &str, stamp: Stamp) -> String {
    let body = body.trim_end();
    format!(
        "{body}\n\n{prefix} lore {version} {hash}{suffix}\n",
        prefix = stamp.prefix,
        suffix = stamp.suffix,
        version = env!("CARGO_PKG_VERSION"),
        hash = hash(body),
    )
}

/// Split installed text back into the body it was written from and the hash
/// recorded at install time.
///
/// `None` when there is no stamp at all: a file at one of our paths that Lore
/// did not write is treated as the user's, which is the safe reading.
fn unstamp(text: &str, stamp: Stamp) -> Option<(&str, &str)> {
    let (before, line) = text.trim_end().rsplit_once(stamp.prefix)?;
    let line = line.trim();
    // Guarded rather than an unconditional `trim_end_matches`: an empty pattern
    // matches everywhere, and this must not depend on what that does.
    let line = match stamp.suffix.trim() {
        "" => line,
        closing => line.trim_end_matches(closing).trim(),
    };
    let recorded = line.rsplit(' ').next()?;
    // `stamped` writes body, then a blank line, then the stamp. Trimming the
    // trailing newlines here rather than matching them exactly means an editor
    // that normalises line endings does not read as an edit.
    Some((before.trim_end(), recorded))
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Nothing at that path.
    Missing,
    /// Lore wrote it, it is untouched, and it matches what this build ships.
    UpToDate,
    /// Lore wrote it, it is untouched, and a newer body is available.
    Outdated,
    /// Edited since install, or written by something other than Lore.
    Modified,
}

impl State {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Missing => "not installed",
            Self::UpToDate => "up to date",
            Self::Outdated => "outdated",
            Self::Modified => "modified locally",
        }
    }

    /// Whether `lore setup` writes this one without `--force`.
    const fn writes(self) -> bool {
        matches!(self, Self::Missing | Self::Outdated)
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub name: &'static str,
    pub summary: &'static str,
    pub path: Utf8PathBuf,
    pub state: State,
    /// What [`apply`] writes. Carried here rather than looked up by name, so
    /// assets from different lists travel through one code path.
    body: &'static str,
    stamp: Stamp,
}

/// What every shipped skill looks like on disk right now.
///
/// Takes the directory rather than reading it from `Host` so the planner is
/// testable against a temporary tree — the alternative is a test that writes
/// into the developer's real `~/.claude`.
pub fn plan(skills_dir: &Utf8Path) -> Vec<Item> {
    SKILLS
        .iter()
        .map(|skill| {
            item(
                skill,
                skills_dir.join(skill.name).join("SKILL.md"),
                MARKDOWN,
            )
        })
        .collect()
}

/// The user-level ignore rules, as they are on this machine right now.
///
/// The path is [`crate::daemon::walk::USER_IGNORE_FILE`] in the data directory —
/// the file the walker reads, from the same constant, so the installer cannot
/// write somewhere the evaluator does not look.
pub fn plan_user_ignore(data_dir: &Utf8Path) -> Item {
    item(
        &USER_IGNORE,
        data_dir.join(crate::daemon::walk::USER_IGNORE_FILE),
        IGNORE,
    )
}

fn item(skill: &'static Skill, path: Utf8PathBuf, stamp: Stamp) -> Item {
    Item {
        name: skill.name,
        summary: skill.summary,
        state: state_of(&path, skill.body, stamp),
        path,
        body: skill.body,
        stamp,
    }
}

fn state_of(path: &Utf8Path, shipped: &str, stamp: Stamp) -> State {
    let Ok(text) = std::fs::read_to_string(path) else {
        return State::Missing;
    };
    let Some((body, recorded)) = unstamp(&text, stamp) else {
        return State::Modified;
    };
    if hash(body) != recorded {
        return State::Modified;
    }
    if recorded == hash(shipped.trim_end()) {
        State::UpToDate
    } else {
        State::Outdated
    }
}

// ---------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    Updated,
    Overwrote,
    Unchanged,
    /// Skipped because it was edited and `--force` was not given.
    Kept,
}

/// Write one item, honouring the never-clobber rule.
pub fn apply(item: &Item, force: bool) -> Result<Outcome> {
    let outcome = match (item.state, force) {
        (State::Missing, _) => Outcome::Installed,
        (State::Outdated, _) => Outcome::Updated,
        (State::UpToDate, _) => return Ok(Outcome::Unchanged),
        (State::Modified, true) => Outcome::Overwrote,
        (State::Modified, false) => return Ok(Outcome::Kept),
    };

    let parent = item
        .path
        .parent()
        .expect("asset paths always have a parent directory");
    std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
    std::fs::write(&item.path, stamped(item.body.trim_end(), item.stamp))
        .with_context(|| format!("writing {}", item.path))?;
    Ok(outcome)
}

/// Whether anything would be written — used to decide what to tell the user
/// when nothing will be.
pub fn pending(items: &[Item], force: bool) -> bool {
    items
        .iter()
        .any(|item| item.state.writes() || (force && item.state == State::Modified))
}

/// The line `lore add` prints so a user who has never heard of the skill still
/// learns that lore indexes everything until something says otherwise.
///
/// It matters more since D-0020: nothing is generated into the project and lore
/// ships no rules of its own, so a fresh project's ignore rules are whatever the
/// repo's `.gitignore` already said.
///
/// It names the command that is actually the next step, which depends on
/// whether the asset is installed — a nudge to run something already installed
/// is noise, and a nudge to invoke a skill that is absent is a dead end.
pub fn ignore_nudge() -> String {
    let installed = Host::ALL.iter().filter(|host| host.detected()).any(|host| {
        host.skills_dir().is_ok_and(|dir| {
            plan(&dir)
                .iter()
                .any(|item| item.name == "lore-ignore" && item.state != State::Missing)
        })
    });
    let action = if installed {
        "ask your agent to run /lore-ignore".to_string()
    } else {
        format!(
            "run `lore setup {}` to install the skill that does it",
            Host::ClaudeCode
        )
    };
    format!("lore indexes what this repo's .gitignore allows; to tune it, {action}")
}

/// Guard against shipping an asset a host would reject, or one that would fight
/// the stamp.
pub fn validate_shipped() -> Result<()> {
    // The ignore template is not Markdown and has no frontmatter, but it must
    // still be stampable — and gitignore syntax means a stray stamp in the body
    // would be an inert comment rather than a visible defect.
    if USER_IGNORE.body.contains(IGNORE.prefix) {
        bail!(
            "{}: body must not contain a stamp; lore appends one",
            USER_IGNORE.name
        );
    }
    for skill in SKILLS {
        let mut lines = skill.body.lines();
        if lines.next() != Some("---") {
            bail!("{}: body must open with YAML frontmatter", skill.name);
        }
        let front: Vec<&str> = lines.take_while(|line| *line != "---").collect();
        if !front
            .iter()
            .any(|line| *line == format!("name: {}", skill.name))
        {
            bail!(
                "{}: frontmatter `name:` must match the asset name",
                skill.name
            );
        }
        if !front.iter().any(|line| line.starts_with("description:")) {
            bail!("{}: frontmatter needs a `description:`", skill.name);
        }
        if skill.body.contains(MARKDOWN.prefix) {
            bail!(
                "{}: body must not contain a stamp; lore appends one",
                skill.name
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, root)
    }

    /// The frontmatter contract every host checks before it will load a skill.
    #[test]
    fn every_shipped_skill_is_a_valid_skill_file() {
        validate_shipped().unwrap();
        assert!(!SKILLS.is_empty());
    }

    /// Both stamp syntaxes round-trip, because both files get rehashed from disk
    /// to answer "has the user touched this?".
    #[test]
    fn a_stamp_round_trips_to_the_body_it_was_written_from() {
        for (stamp, body) in [(MARKDOWN, "# hello\n\nsome prose\n"), (IGNORE, "target/\n")] {
            let text = stamped(body, stamp);
            let (parsed, recorded) = unstamp(&text, stamp).unwrap();
            assert_eq!(parsed, body.trim_end());
            assert_eq!(recorded, hash(body.trim_end()));
            assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        }
    }

    /// The ignore template's stamp has to be inert *as a rule*: a stamp that
    /// parsed as a pattern would exclude something, and nothing in the file
    /// would say why.
    #[test]
    fn the_ignore_templates_stamp_is_a_comment() {
        let text = stamped(USER_IGNORE.body, IGNORE);
        let stamp = text
            .lines()
            .find(|line| line.contains(IGNORE.prefix))
            .expect("stamped");
        assert!(stamp.starts_with('#'), "{stamp}");
        // And the template itself is rules and comments only — a blank-indented
        // line or a stray `-->` would be a silently dead rule.
        for line in USER_IGNORE.body.lines() {
            assert_eq!(line, line.trim_end(), "trailing space: {line:?}");
            assert!(!line.starts_with(char::is_whitespace), "indented: {line:?}");
        }
    }

    /// The installer must write where the evaluator reads, from one constant.
    #[test]
    fn the_user_level_rules_land_where_the_walker_looks_for_them() {
        let (_dir, root) = temp();
        let item = plan_user_ignore(&root);
        assert_eq!(item.path, root.join(crate::daemon::walk::USER_IGNORE_FILE));
        assert_eq!(item.state, State::Missing, "never installed by default");
        assert_eq!(apply(&item, false).unwrap(), Outcome::Installed);
        assert_eq!(plan_user_ignore(&root).state, State::UpToDate);
    }

    /// A `loreignore` the user wrote themselves is theirs, and an install must
    /// not eat it — the same never-clobber rule the skills get, on a file that is
    /// much more likely to already exist.
    #[test]
    fn a_hand_written_user_loreignore_is_never_replaced() {
        let (_dir, root) = temp();
        std::fs::write(
            root.join(crate::daemon::walk::USER_IGNORE_FILE),
            "my-own-rule/\n",
        )
        .unwrap();
        let item = plan_user_ignore(&root);
        assert_eq!(item.state, State::Modified);
        assert_eq!(apply(&item, false).unwrap(), Outcome::Kept);
        assert_eq!(
            std::fs::read_to_string(&item.path).unwrap(),
            "my-own-rule/\n"
        );
    }

    /// The whole point of the stamp: an installed asset is a prompt, and a user
    /// who sharpens one must not lose it to the next `lore setup`.
    #[test]
    fn an_edited_asset_is_kept_until_force() {
        let (_dir, root) = temp();
        let items = plan(&root);
        let item = &items[0];
        assert_eq!(item.state, State::Missing);
        assert_eq!(apply(item, false).unwrap(), Outcome::Installed);

        let installed = std::fs::read_to_string(&item.path).unwrap();
        std::fs::write(&item.path, format!("{installed}\nmy own note\n")).unwrap();

        let edited = &plan(&root)[0];
        assert_eq!(edited.state, State::Modified);
        assert_eq!(apply(edited, false).unwrap(), Outcome::Kept);
        assert!(
            std::fs::read_to_string(&edited.path)
                .unwrap()
                .contains("my own note")
        );

        assert_eq!(apply(edited, true).unwrap(), Outcome::Overwrote);
        assert!(
            !std::fs::read_to_string(&edited.path)
                .unwrap()
                .contains("my own note")
        );
    }

    #[test]
    fn installing_twice_changes_nothing_the_second_time() {
        let (_dir, root) = temp();
        for item in &plan(&root) {
            assert_eq!(apply(item, false).unwrap(), Outcome::Installed);
        }

        let after = plan(&root);
        for item in &after {
            assert_eq!(item.state, State::UpToDate);
            let before_bytes = std::fs::read(&item.path).unwrap();
            assert_eq!(apply(item, false).unwrap(), Outcome::Unchanged);
            assert_eq!(std::fs::read(&item.path).unwrap(), before_bytes);
        }
        assert!(!pending(&after, false));
        assert!(!pending(&after, true));
    }

    /// An older Lore's asset is untouched-but-stale, and must upgrade without
    /// asking — otherwise every improvement to a skill needs `--force`, and
    /// `--force` stops meaning anything.
    #[test]
    fn an_untouched_asset_from_an_older_lore_upgrades_silently() {
        let (_dir, root) = temp();
        let item = &plan(&root)[0];
        std::fs::create_dir_all(item.path.parent().unwrap()).unwrap();
        std::fs::write(&item.path, stamped("# an older body\n", MARKDOWN)).unwrap();

        let stale = &plan(&root)[0];
        assert_eq!(stale.state, State::Outdated);
        assert!(pending(std::slice::from_ref(stale), false));
        assert_eq!(apply(stale, false).unwrap(), Outcome::Updated);
        assert_eq!(plan(&root)[0].state, State::UpToDate);
    }

    /// A file Lore did not write is the user's, whatever it is doing at that
    /// path.
    #[test]
    fn an_unstamped_file_is_never_silently_replaced() {
        let (_dir, root) = temp();
        let item = &plan(&root)[0];
        std::fs::create_dir_all(item.path.parent().unwrap()).unwrap();
        std::fs::write(&item.path, "---\nname: mine\n---\n").unwrap();

        let found = &plan(&root)[0];
        assert_eq!(found.state, State::Modified);
        assert_eq!(apply(found, false).unwrap(), Outcome::Kept);
    }

    #[test]
    fn hosts_round_trip_through_their_ids_and_unknown_ones_name_the_known() {
        for host in Host::ALL {
            assert_eq!(Host::parse(host.id()).unwrap(), *host);
        }
        let err = Host::parse("emacs").unwrap_err().to_string();
        assert!(err.contains("emacs"), "{err}");
        assert!(err.contains("claude-code"), "{err}");
    }

    #[test]
    fn the_planned_path_is_the_one_the_host_actually_reads() {
        let (_dir, root) = temp();
        let items = plan(&root);
        for item in &items {
            assert_eq!(item.path, root.join(item.name).join("SKILL.md"));
        }
        assert!(items.iter().any(|item| item.name == "lore-ignore"));
    }
}
