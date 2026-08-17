//! A project's **extent**: which directories on disk it is made of.
//!
//! D-0021 settled that filesystem topology does not define project topology —
//! a symlink cannot quietly enlarge a project, and there is no
//! `follow_symlinks`. That left the legitimate need it displaced: a project
//! genuinely composed of a tree that lives somewhere else (a shared engine, a
//! design vault two repos use, a corpus checked out beside the code). The
//! answer is to **declare** it:
//!
//! ```toml
//! [[sources]]
//! path = "."
//!
//! [[sources]]
//! path = "../shared-engine"
//! mount = "shared-engine"
//! ```
//!
//! A declared source is auditable, gives every file exactly one logical path,
//! and does not depend on how this machine happens to be wired. That is the
//! whole difference from following a link, and it is why this is configuration
//! rather than traversal.
//!
//! ## Extent is not exclusion
//!
//! `[[sources]]` in `.lore.toml` does not reopen what D-0020 closed. D-0020
//! deleted `[ingest] allow_secret_paths` because *exclusion rules* belong in a
//! file at a precedence the user can argue with, and the working rule it left
//! behind — "a new rule system needs a decision, not a code path" — is about
//! rule systems. This declares no rules. It says which directories the project
//! is, the same category of question as the repository boundary and the link
//! boundary, both of which already live outside the ignore stack.
//!
//! ## Roots are independent
//!
//! Ignore rules travel **down** a source root and never **between** roots.
//! Within a root they compose exactly as they always have — a nested
//! `.loreignore` sits on top of that root's own, most local winning. Across
//! roots nothing is inherited in either direction: the declaring project's
//! `.loreignore` does not reach into a mount, and a mount's rules do not leak
//! back out.
//!
//! A mounted tree is somebody else's directory that this project happens to
//! name. Reaching into it would be the declaring project crossing a boundary
//! it does not own — the instinct D-0021 refuses for links, arriving as
//! configuration instead. It is also the cheap shape: each root gets its own
//! walk with its own matcher stack, which is what the `ignore` crate does
//! naturally.
//!
//! This rests on the walker's `parents(false)`. With ancestor scanning on, a
//! mount at `../shared-engine` would pick up ignore files from `..` and above
//! — quite possibly the declaring project's own parent — and "independent
//! root" would be a description rather than a fact. The machine-wide
//! `loreignore` still applies to every root, because it is added explicitly as
//! the lowest rung rather than discovered by walking upward.
//!
//! ## One file, one logical path
//!
//! Every path the store, the wire and search deal in is **logical**:
//! `<mount>/<path within that root>`, or just the path for the source that is
//! the project root itself (whose mount is empty). The store is keyed
//! `(project, path)`, so a single logical path per file is what keeps one
//! content from competing with itself — the same reason D-0021 refuses to
//! index a file through a link to it. [`Sources::resolve`] is the only place
//! that turns a logical path back into a physical one, and [`Sources::locate`]
//! the only place that goes the other way — while [`Sources::from_declared`]
//! is what decides, once, which roots exist at all.
//!
//! ## Failing visibly
//!
//! Same posture as [`crate::repo_config`]: a broken `[[sources]]` table does
//! not stop the project indexing. It falls back to the project root alone —
//! which is what every project did before this existed — and the error rides
//! in [`Sources::error`] for `lore status` to put in front of the user.
//! Silently indexing *nothing* because a mount path had a typo would be the
//! worst of the available failures.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::daemon::paths;

/// The mount of the source that *is* the project root: no prefix at all, so
/// that a project without mounts has exactly the paths it always had.
pub const ROOT_MOUNT: &str = "";

/// One declared directory a project is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Logical prefix carried by every path from this root. Empty for the
    /// project root itself; otherwise one path component.
    pub mount: String,
    /// The canonical physical directory. Canonical because a source may be
    /// reached through a link — the one place D-0021 resolves one — and
    /// because the watcher compares event paths against it.
    pub root: Utf8PathBuf,
}

impl Source {
    /// The logical path of `within` (a path relative to this source's root).
    pub fn logical(&self, within: &Utf8Path) -> Utf8PathBuf {
        if self.mount.is_empty() {
            within.to_owned()
        } else {
            Utf8PathBuf::from(format!("{}/{within}", self.mount))
        }
    }

    /// Whether this source is the project root itself.
    pub fn is_root(&self) -> bool {
        self.mount.is_empty()
    }
}

/// Everything a project is made of, resolved and validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources {
    sources: Vec<Source>,
    error: Option<String>,
}

impl Sources {
    /// Read `<root>/.lore.toml` and resolve its extent. Never fails; see the
    /// module header on failing visibly.
    pub fn load(project_root: &Utf8Path) -> Self {
        let path = project_root.join(crate::repo_config::REPO_CONFIG_FILE);
        let declared = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<SourcesFile>(&text) {
                Ok(file) => file.sources,
                // A malformed file is already reported by `RepoAuthority`,
                // which parses the same bytes for its own table. Repeating the
                // parser's message here would put the same defect in front of
                // the user twice, in two voices.
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        Self::from_declared(project_root, &declared)
    }

    /// Resolve declared entries against a project root.
    ///
    /// Split from [`Self::load`] so the rules below are testable without a
    /// `.lore.toml` on disk — the validation is the substance here, and the
    /// file read is not.
    pub fn from_declared(project_root: &Utf8Path, declared: &[SourceEntry]) -> Self {
        // No table at all is the overwhelmingly common case and is not a
        // degenerate one: a project is its root.
        if declared.is_empty() {
            return Self::just_the_root(project_root);
        }
        match Self::try_resolve(project_root, declared) {
            Ok(sources) => Self {
                sources,
                error: None,
            },
            Err(message) => Self {
                error: Some(message),
                ..Self::just_the_root(project_root)
            },
        }
    }

    fn just_the_root(project_root: &Utf8Path) -> Self {
        Self {
            sources: vec![Source {
                mount: ROOT_MOUNT.to_string(),
                root: project_root.to_owned(),
            }],
            error: None,
        }
    }

    fn try_resolve(
        project_root: &Utf8Path,
        declared: &[SourceEntry],
    ) -> Result<Vec<Source>, String> {
        // Compared against the *canonical* project root, never the one handed
        // in. The daemon canonicalizes at registration so the two normally
        // agree, but "is this entry the project root?" decides whether a mount
        // is required — and answering it wrongly turns `path = "."` into a
        // rejected table and the whole project into a fallback. Too sharp an
        // edge to leave resting on a caller's habit.
        let canonical_root = paths::canonicalize_root(project_root.as_std_path())
            .unwrap_or_else(|_| project_root.to_owned());
        let mut sources: Vec<Source> = Vec::new();
        for entry in declared {
            let source = Self::resolve_one(project_root, &canonical_root, entry)?;
            // Mounts are the store's path prefixes, so a collision would put
            // two trees at one logical address. Folded, because two mounts
            // differing only in case is human error rather than intent.
            if let Some(clash) = sources
                .iter()
                .find(|other| other.mount.eq_ignore_ascii_case(&source.mount))
            {
                return Err(match source.mount.as_str() {
                    ROOT_MOUNT => format!(
                        "two sources both resolve to the project root ({}); \
                         a project root may only be declared once",
                        clash.root
                    ),
                    mount => format!("two sources share the mount `{mount}`"),
                });
            }
            // Nesting would walk the same tree twice, once under each mount —
            // the duplicate-content failure this whole design exists to avoid.
            if let Some(clash) = sources.iter().find(|other| {
                paths::is_within(&other.root, &source.root)
                    || paths::is_within(&source.root, &other.root)
            }) {
                return Err(format!(
                    "source `{}` overlaps source `{}`; one would be indexed twice",
                    source.root, clash.root
                ));
            }
            sources.push(source);
        }
        Ok(sources)
    }

    fn resolve_one(
        project_root: &Utf8Path,
        canonical_root: &Utf8Path,
        entry: &SourceEntry,
    ) -> Result<Source, String> {
        let declared = Utf8Path::new(entry.path.trim());
        if declared.as_str().is_empty() {
            return Err("a source has an empty `path`".to_string());
        }
        // Absolute paths are refused because `.lore.toml` is committed and
        // travels: `C:\Users\someone\engine` is a path that works on exactly
        // one machine, and a project whose extent silently differs per
        // checkout is worse than one that refuses to load.
        if declared.is_absolute() {
            return Err(format!(
                "source path `{declared}` is absolute; \
                 paths are relative to the project root so that {} travels with the repo",
                crate::repo_config::REPO_CONFIG_FILE,
            ));
        }
        let joined = project_root.join(declared);
        // Canonical, so that `.`, `..` and any link in the chain are resolved
        // once, here — and so the watcher can compare raw event paths against
        // it. This is also the existence check.
        let root = paths::canonicalize_root(joined.as_std_path())
            .map_err(|err| format!("source path `{declared}` cannot be resolved: {err}"))?;
        if !root.is_dir() {
            return Err(format!("source path `{declared}` is not a directory"));
        }

        let is_project_root = root == canonical_root;
        let mount = match entry.mount.as_deref().map(str::trim) {
            Some(mount) => mount.to_string(),
            // Defaulted rather than required: `../shared-engine` mounting at
            // `shared-engine` is what anyone would have typed.
            None if is_project_root => ROOT_MOUNT.to_string(),
            None => root
                .file_name()
                .ok_or_else(|| {
                    format!("source path `{declared}` has no final component to mount at")
                })?
                .to_string(),
        };
        Self::check_mount(&mount, is_project_root, declared)?;
        Ok(Source { mount, root })
    }

    /// A mount is one path component, and the empty mount belongs to the
    /// project root and only to it.
    ///
    /// The stricter half is the second: an external tree with an empty mount
    /// would splice its files directly into the project's own namespace, where
    /// `src/lib.rs` from two roots is one logical path and one silently
    /// overwrites the other.
    fn check_mount(mount: &str, is_project_root: bool, declared: &Utf8Path) -> Result<(), String> {
        if mount.is_empty() {
            return if is_project_root {
                Ok(())
            } else {
                Err(format!(
                    "source `{declared}` is outside the project root and needs a `mount` name; \
                     only the project root itself may mount at the top level"
                ))
            };
        }
        if is_project_root {
            return Err(format!(
                "source `{declared}` is the project root, which always mounts at the top level; \
                 remove its `mount`"
            ));
        }
        if mount.contains(['/', '\\']) || mount == "." || mount == ".." {
            return Err(format!(
                "mount `{mount}` must be a single path component with no separators"
            ));
        }
        Ok(())
    }

    /// Every source, in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = &Source> {
        self.sources.iter()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// True when this project is nothing but its own root — the shape every
    /// project had before `[[sources]]` existed, and the one that needs no
    /// extra machinery anywhere downstream.
    pub fn is_only_root(&self) -> bool {
        self.sources.len() == 1 && self.sources[0].is_root()
    }

    /// A `[[sources]]` table Lore could not use. The project indexed as its
    /// root alone; this is what `lore status` shows.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Logical path → the physical file it names.
    ///
    /// `None` when no source owns the path, which is how a stale index row
    /// under a mount that has since been removed reads as "gone" rather than
    /// resolving to something else.
    pub fn resolve_path(&self, logical: &Utf8Path) -> Option<Utf8PathBuf> {
        self.resolve(logical).map(|(_, abs)| abs)
    }

    /// [`Self::resolve_path`], keeping the source that answered.
    ///
    /// Callers that are about to walk or watch need the owning root as well as
    /// the file, and asking twice would invite the two answers to drift.
    pub fn resolve(&self, logical: &Utf8Path) -> Option<(&Source, Utf8PathBuf)> {
        let logical = logical.as_str();
        for source in &self.sources {
            if source.is_root() {
                continue;
            }
            if let Some(rest) = strip_mount(logical, &source.mount) {
                return Some((source, source.root.join(rest)));
            }
        }
        // The root source is tried last so that a mount always wins over a
        // same-named directory inside the project root. A project that has
        // both has a genuine ambiguity, and resolving it toward the thing the
        // user explicitly declared is the less surprising answer.
        self.sources
            .iter()
            .find(|source| source.is_root())
            .map(|source| (source, source.root.join(logical)))
    }

    /// Physical path → the source that owns it and the logical path it has.
    ///
    /// This is the watcher's direction: an event names a real path on disk and
    /// has to become the path the index knows. `None` when no source contains
    /// it.
    pub fn locate(&self, abs: &Utf8Path) -> Option<(&Source, Utf8PathBuf)> {
        // Longest root first, so a source nested inside another would still
        // attribute correctly. Overlap is refused at resolve time, so this is
        // belt and braces rather than the load-bearing part.
        let mut candidates: Vec<&Source> = self.sources.iter().collect();
        candidates.sort_by_key(|source| std::cmp::Reverse(source.root.as_str().len()));
        candidates.into_iter().find_map(|source| {
            paths::relative_to(&source.root, abs).map(|rel| (source, source.logical(&rel)))
        })
    }
}

/// Strip `mount/` from the front of a logical path, case-folded on Windows the
/// way every other path comparison here is.
fn strip_mount<'a>(logical: &'a str, mount: &str) -> Option<&'a str> {
    let rest = logical.get(mount.len()..)?;
    let head = logical.get(..mount.len())?;
    let matches = if cfg!(windows) {
        head.eq_ignore_ascii_case(mount)
    } else {
        head == mount
    };
    if !matches {
        return None;
    }
    rest.strip_prefix('/')
}

/// The `[[sources]]` array as written. Public because [`Sources::resolve`]
/// takes it, which is what makes the validation testable without a file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEntry {
    /// Relative to the project root.
    pub path: String,
    /// Logical prefix; defaults to the path's final component, or to nothing
    /// for the project root itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
}

/// Just enough of the file to read one table. Deliberately *not*
/// `deny_unknown_fields`: `[authority]` and `[project]` are somebody else's
/// business, and this parse must not fail because of them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SourcesFile {
    sources: Vec<SourceEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project root and a sibling directory to mount from, under one parent
    /// so the declared path is an ordinary `../outside/...` — which is what a
    /// committed `.lore.toml` would actually contain.
    ///
    /// Both canonical, because that is what the daemon hands this module and
    /// what makes the `root == project_root` comparison in `resolve_one` mean
    /// anything.
    struct Fixture {
        _parent: tempfile::TempDir,
        root: Utf8PathBuf,
        outside: Utf8PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let parent = tempfile::tempdir().unwrap();
            let parent_path = paths::canonicalize_root(parent.path()).unwrap();
            let root = parent_path.join("project");
            let outside = parent_path.join("outside");
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::create_dir_all(outside.join("shared-engine")).unwrap();
            Self {
                _parent: parent,
                root,
                outside,
            }
        }

        fn outside_dir(&self, name: &str) -> Utf8PathBuf {
            let path = self.outside.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        fn resolve(&self, declared: &[SourceEntry]) -> Sources {
            Sources::from_declared(&self.root, declared)
        }
    }

    fn entry(path: &str, mount: Option<&str>) -> SourceEntry {
        SourceEntry {
            path: path.to_string(),
            mount: mount.map(str::to_string),
        }
    }

    /// The shape every project has today, and the one that must stay free: no
    /// table means the project is its root, with no prefix on anything.
    #[test]
    fn no_table_means_the_project_is_its_root() {
        let fixture = Fixture::new();
        let sources = fixture.resolve(&[]);

        assert!(sources.is_only_root());
        assert_eq!(sources.error(), None);
        assert_eq!(
            sources.resolve_path(Utf8Path::new("src/main.rs")),
            Some(fixture.root.join("src/main.rs")),
            "paths are unprefixed, exactly as before this existed"
        );
    }

    /// The motivating shape, from the decision brief.
    #[test]
    fn a_declared_mount_carries_its_prefix_in_both_directions() {
        let fixture = Fixture::new();
        let sources = fixture.resolve(&[
            entry(".", None),
            entry("../outside/shared-engine", Some("engine")),
        ]);

        assert_eq!(sources.error(), None);
        assert_eq!(sources.len(), 2);

        // Logical to physical: the mount resolves outside the project root.
        assert_eq!(
            sources.resolve_path(Utf8Path::new("engine/render/pass.rs")),
            Some(fixture.outside.join("shared-engine/render/pass.rs"))
        );
        // ...and the root source is untouched by its presence.
        assert_eq!(
            sources.resolve_path(Utf8Path::new("src/main.rs")),
            Some(fixture.root.join("src/main.rs"))
        );

        // Physical to logical: the watcher's direction.
        let (source, logical) = sources
            .locate(&fixture.outside.join("shared-engine/render/pass.rs"))
            .expect("the mount contains it");
        assert_eq!(source.mount, "engine");
        assert_eq!(logical, "engine/render/pass.rs");

        let (source, logical) = sources
            .locate(&fixture.root.join("src/main.rs"))
            .expect("the root contains it");
        assert!(source.is_root());
        assert_eq!(logical, "src/main.rs");
    }

    /// A path nobody declared belongs to nobody — which is how a watcher event
    /// from an unrelated tree is dropped rather than attributed to whichever
    /// source happens to be listed first.
    #[test]
    fn a_path_outside_every_source_is_located_nowhere() {
        let fixture = Fixture::new();
        let sources = fixture.resolve(&[entry(".", None)]);
        assert!(
            sources
                .locate(&fixture.outside.join("shared-engine"))
                .is_none()
        );
    }

    /// `mount` is a convenience, not a requirement: the final component is
    /// what anyone would have typed anyway.
    #[test]
    fn a_mount_defaults_to_the_paths_final_component() {
        let fixture = Fixture::new();
        let sources = fixture.resolve(&[entry(".", None), entry("../outside/shared-engine", None)]);

        assert_eq!(sources.error(), None);
        assert_eq!(
            sources.resolve_path(Utf8Path::new("shared-engine/lib.rs")),
            Some(fixture.outside.join("shared-engine/lib.rs"))
        );
    }

    /// The store is keyed `(project, path)`, so an external tree mounted at
    /// the top level would put its `src/lib.rs` at the same logical address as
    /// the project's own, one silently overwriting the other on every pass.
    /// That is the duplicate-content failure D-0021 refuses for links, which
    /// is why this is refused outright rather than settled by precedence.
    #[test]
    fn an_external_source_may_not_mount_at_the_top_level() {
        let fixture = Fixture::new();
        let sources = fixture.resolve(&[
            entry(".", None),
            entry("../outside/shared-engine", Some("")),
        ]);

        let error = sources.error().expect("refused");
        assert!(error.contains("needs a `mount` name"), "{error}");
        assert!(
            sources.is_only_root(),
            "a broken table falls back to the root, never to nothing"
        );
    }

    #[test]
    fn two_sources_may_not_share_a_mount() {
        let fixture = Fixture::new();
        fixture.outside_dir("other");
        let sources = fixture.resolve(&[
            entry("../outside/shared-engine", Some("engine")),
            // Folded: two mounts differing only in case is human error.
            entry("../outside/other", Some("Engine")),
        ]);

        let error = sources.error().expect("refused");
        assert!(error.contains("share the mount"), "{error}");
    }

    /// Nesting would walk one tree twice, once under each mount — the exact
    /// duplication the declaration exists to avoid.
    #[test]
    fn sources_may_not_overlap() {
        let fixture = Fixture::new();
        fixture.outside_dir("shared-engine/render");
        let sources = fixture.resolve(&[
            entry("../outside/shared-engine", Some("engine")),
            entry("../outside/shared-engine/render", Some("render")),
        ]);

        let error = sources.error().expect("refused");
        assert!(error.contains("overlaps"), "{error}");
    }

    /// `.lore.toml` is committed and travels between machines and
    /// contributors. An absolute path works on exactly one of them, and a
    /// project whose extent silently differs per checkout is worse than one
    /// that refuses to load.
    #[test]
    fn an_absolute_source_path_is_refused() {
        let fixture = Fixture::new();
        let absolute = fixture.outside.join("shared-engine").to_string();
        let sources = fixture.resolve(&[entry(".", None), entry(&absolute, Some("engine"))]);

        let error = sources.error().expect("refused");
        assert!(error.contains("is absolute"), "{error}");
    }

    #[test]
    fn a_source_that_does_not_exist_is_refused_visibly() {
        let fixture = Fixture::new();
        let sources = fixture.resolve(&[entry(".", None), entry("../nope", Some("nope"))]);

        let error = sources.error().expect("refused");
        assert!(error.contains("cannot be resolved"), "{error}");
        assert!(sources.is_only_root());
    }

    /// A mount is one path component. `vendor/engine` would invent a logical
    /// namespace nothing else in the system knows how to reason about.
    #[test]
    fn a_mount_must_be_a_single_component() {
        let fixture = Fixture::new();
        for bad in ["vendor/engine", "..", "."] {
            let sources = fixture.resolve(&[entry("../outside/shared-engine", Some(bad))]);
            assert!(sources.error().is_some(), "`{bad}` should be refused");
        }
    }

    /// Declaring the root twice — under any spelling — would index it twice.
    #[test]
    fn the_project_root_may_only_be_declared_once() {
        let fixture = Fixture::new();
        let sources = fixture.resolve(&[entry(".", None), entry("src/..", None)]);

        let error = sources.error().expect("refused");
        assert!(error.contains("project root"), "{error}");
    }

    /// A project may legitimately consist only of trees living elsewhere —
    /// the bench shape, whose root holds nothing but a couple of loose files.
    #[test]
    fn a_project_need_not_include_its_own_root() {
        let fixture = Fixture::new();
        let sources = fixture.resolve(&[entry("../outside/shared-engine", Some("engine"))]);

        assert_eq!(sources.error(), None);
        assert_eq!(sources.len(), 1);
        assert!(!sources.is_only_root());
        assert_eq!(
            sources.resolve_path(Utf8Path::new("src/main.rs")),
            None,
            "with no root source, an unprefixed path names nothing"
        );
    }

    /// Read off disk, from the same committed file `[authority]` and
    /// `[project]` live in — and without disturbing either. The last assertion
    /// is the one that matters: `RepoConfigFile` denies unknown fields, so a
    /// `[[sources]]` table it did not know about would have made every
    /// configured repo fail to declare its profile.
    #[test]
    fn the_table_is_read_from_the_committed_file_beside_the_others() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.root.join(crate::repo_config::REPO_CONFIG_FILE),
            "[project]\nname = \"demo\"\n\n\
             [authority]\nprofile = \"lore-v1\"\n\n\
             [[sources]]\npath = \".\"\n\n\
             [[sources]]\npath = \"../outside/shared-engine\"\nmount = \"engine\"\n",
        )
        .unwrap();

        let sources = Sources::load(&fixture.root);
        assert_eq!(sources.error(), None);
        assert_eq!(sources.len(), 2);
        assert_eq!(
            sources.resolve_path(Utf8Path::new("engine/lib.rs")),
            Some(fixture.outside.join("shared-engine/lib.rs"))
        );

        let authority = crate::repo_config::RepoAuthority::load(&fixture.root);
        assert_eq!(authority.error, None, "{authority:?}");
        assert_eq!(authority.profile, Some(crate::repo_config::Profile::LoreV1));
    }
}
