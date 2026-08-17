//! Lore's built-in default ignore rules — the lowest rung of the one
//! evaluator.
//!
//! D-0020 stacks exactly three rule sources, lowest to highest:
//!
//! 1. **these defaults** — ordinary, overridable gitignore rules;
//! 2. the repo's own **`.gitignore`**, honored as a courtesy;
//! 3. the project's **`.loreignore`**, sovereign.
//!
//! All three are gitignore syntax and all three are evaluated by one `ignore`
//! crate walker (see [`super::walk`]), which is what makes the precedence
//! uniform instead of five rule systems each with its own quirk. A `!` line in
//! `.loreignore` beats a `.gitignore` rule *and* beats anything here — the
//! credential patterns included. That trade is stated openly in D-0020: a bad
//! ignore file can admit a secret, hygiene here is best-effort user
//! responsibility, and an encrypted store is the substantive measure.
//!
//! ## Why these rules reach the walker as a file
//!
//! The `ignore` crate's lowest-precedence rung is its "explicit" ignore list —
//! documented as "lower precedence than all other sources of ignore rules",
//! which is exactly rung 1 above. The only public way into it is
//! [`ignore::WalkBuilder::add_ignore`], and that takes a *path*. So the
//! constant below is materialized to a scratch file and the walker is pointed
//! at it.
//!
//! Nothing about that file is state, and it is not a configuration surface:
//! its name carries a hash of its content, so an upgraded lore writes a new
//! file rather than trusting a stale one and two lore versions running at once
//! cannot fight over it; and an edited one is detected by content and replaced.
//! It lives in the OS temp directory rather than the daemon's data directory
//! because it is a compiled-in constant that happens to need a path, not
//! something the daemon knows — which also keeps [`super::walk`] testable
//! without a writable data directory.

use camino::Utf8PathBuf;

/// Hidden-ness, as an ordinary rule.
///
/// The `ignore` crate's `hidden(true)` flag is off under D-0020 precisely so
/// this can be argued with: a project that wants its dot-files indexed writes
/// `!.github/` in its `.loreignore` rather than losing to a flag it cannot see.
///
/// **No re-includes, deliberately.** This reproduces exactly what
/// `hidden(true)` did before D-0020 — no dot-file was indexed, `.github/` and
/// `.lore.toml` included — and widening it would change what lore indexes in
/// every project on the strength of nobody's decision. The sovereign layer is
/// where that call belongs, per project, in a committed file a reviewer sees.
pub const DOT_FILE_RULES: &str = "\
# Dot-files and dot-directories. Overridable like every rule here: `!.github/`
# in a project's .loreignore beats it (D-0020).
.*
";

/// Credential patterns (D-0015's list, verbatim).
///
/// Under D-0020 these are ordinary overridable rules rather than a refusal
/// nobody can argue with: re-including one is a `!` line in a committed
/// `.loreignore`, visible in review, and deliberately the user's call. The list
/// is a floor and knowingly incomplete — pattern-based only, no entropy
/// scanning (D-0015 killed that by name: false-positive-prone, and it trains
/// the reflex of overriding the guard).
///
/// Two known-broad edges, kept rather than narrowed because a missed key is
/// unrecoverable while a missed *document* is one `!` line away: `*.key` also
/// catches Keynote documents and some engine asset formats, and `*.pem` catches
/// public certificate chains — PEM does not distinguish them by name and lore
/// must not read the file to find out.
///
/// Gitignore syntax has one trap worth naming here: a pattern containing a
/// slash is anchored to the file's own directory, so `.config/gcloud/` alone
/// would match at the project root and nowhere else. `**/` restores the
/// any-depth matching the D-0015 list had.
pub const CREDENTIAL_RULES: &str = "\
# Process environment files: `.env.local` and `.env.production` are the same
# file with a suffix, and that is where deployed secrets actually live.
.env
.env.*
# OpenSSH private keys, as `ssh-keygen` names them. The `.pub` half is public,
# but refusing it too costs nothing anybody wanted indexed.
id_rsa*
id_ecdsa*
id_ed25519*
id_dsa*
# PEM containers and generic key files.
*.pem
*.key
# Credential directories, at any depth: a vendored `home/.ssh/` is as much a
# private key as a root-level one.
.ssh/
.aws/
.gnupg/
**/.config/gcloud/
";

/// Ecosystem build output and vendored trees.
///
/// Inherited from the `.loreignore` generation this replaces (D-0020 retires
/// generation): the same per-ecosystem catalog, now unconditional defaults
/// rather than patterns written into a project only when a marker was detected.
/// Detection paid a bounded directory walk to decide which groups to emit, and
/// what it bought — `target/` absent from a project with no `Cargo.toml` — is
/// worth nothing once the rules are overridable.
///
/// Unconditional is not free, and one direction of the cost is real: `[Bb]in/`
/// and `dist/` hide authored content in ecosystems that commit scripts there
/// (`bin/rails`, an npm package's `bin/`). A project in that position re-includes
/// them (`![Bb]in/`), which is the sovereign layer working as designed.
///
/// Leading `[Xx]` classes where real tooling varies the case — Unity and
/// MSBuild both do — because the crate matches case-sensitively and the old
/// in-memory fallback list did not. This is what the canonical Unity
/// `.gitignore` does for the same reason.
///
/// **Separable on purpose.** Nothing outside this constant and its entry in
/// [`GROUPS`] depends on it; deleting both (and the ecosystem tests below)
/// leaves the dot-file rule, the credential rules and the precedence machinery
/// untouched.
pub const ECOSYSTEM_RULES: &str = "\
# Rust
[Tt]arget/
# Node
node_modules/
dist/
# Python
__pycache__/
venv/
.venv/
*.pyc
# .NET / MSBuild
[Bb]in/
[Oo]bj/
# Unity
[Ll]ibrary/
[Tt]emp/
[Ll]ogs/
[Bb]uild/
[Bb]uilds/
[Uu]serSettings/
[Mm]emoryCaptures/
*.meta
";

/// The default rule groups, in the order they are written to the file.
///
/// Order matters within one gitignore document — later lines win — so a group
/// must never re-include what a later group ignores. Today no group re-includes
/// anything, which is the cheapest possible way to hold that property.
const GROUPS: &[&str] = &[DOT_FILE_RULES, CREDENTIAL_RULES, ECOSYSTEM_RULES];

const HEADER: &str = "\
# Lore's built-in default ignore rules (D-0020), written from a constant in the
# lore binary. Editing this file does nothing: lore compares it against that
# constant and replaces it. The file a project edits is its own .loreignore,
# which outranks everything here.
";

/// The whole default rule set as one gitignore document.
pub fn rules() -> String {
    let mut out = String::from(HEADER);
    for group in GROUPS {
        out.push('\n');
        out.push_str(group);
    }
    out
}

/// The materialized rule file, for [`ignore::WalkBuilder::add_ignore`].
///
/// `None` means the rules could not be written and the walk will run on
/// `.gitignore` and `.loreignore` alone — logged by the caller, because it
/// silently changes what is indexed.
pub fn rules_file() -> Option<Utf8PathBuf> {
    let dir = Utf8PathBuf::from_path_buf(std::env::temp_dir()).ok()?;
    let rules = rules();
    // Content-addressed: a file with this name either holds these exact rules
    // or was tampered with, and two lore builds with different defaults never
    // write the same path.
    let path = dir.join(format!(
        "lore-default-rules-{}.loreignore",
        &blake3::hash(rules.as_bytes()).to_hex()[..16]
    ));
    if std::fs::read_to_string(&path).is_ok_and(|body| body == rules) {
        return Some(path);
    }
    // Written elsewhere and renamed into place: a walk in another process must
    // never read this file half-written, and `fs::rename` replaces atomically
    // on Windows as well as POSIX.
    let staging = dir.join(format!(
        "{}.{}.tmp",
        path.file_name().unwrap_or("lore-default-rules"),
        std::process::id()
    ));
    if let Err(err) = std::fs::write(&staging, &rules) {
        tracing::error!(path = %staging, error = %err, "could not write lore's default ignore rules");
        return None;
    }
    if let Err(err) = std::fs::rename(&staging, &path) {
        tracing::error!(path = %path, error = %err, "could not install lore's default ignore rules");
        let _ = std::fs::remove_file(&staging);
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rule reaches the file, in group order, under a header that says
    /// the file is not a knob.
    #[test]
    fn the_document_is_the_groups_in_order() {
        let rules = rules();
        assert!(rules.starts_with(HEADER), "{rules}");
        let mut at = 0;
        for group in GROUPS {
            let found = rules[at..]
                .find(group)
                .unwrap_or_else(|| panic!("group missing or out of order: {group}"));
            at += found + group.len();
        }
        // Nothing but comments and patterns: a stray blank-prefixed pattern or
        // a tab would be a silently dead rule.
        for line in rules.lines() {
            assert_eq!(line, line.trim_end(), "trailing space: {line:?}");
            assert!(!line.starts_with(char::is_whitespace), "indented: {line:?}");
        }
    }

    /// The materialized file is byte-identical to the constant, and a tampered
    /// copy is replaced rather than trusted — which is what makes it not a
    /// configuration surface.
    #[test]
    fn the_file_is_the_constant_and_heals_an_edit() {
        let path = rules_file().expect("the temp directory is writable");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), rules());

        std::fs::write(&path, "# somebody edited this\n").unwrap();
        let again = rules_file().expect("the temp directory is writable");
        assert_eq!(again, path, "the name is content-addressed, so it is stable");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), rules());
    }

    /// Deleting [`ECOSYSTEM_RULES`] must not need a single edit anywhere else,
    /// so the only thing that may know about it is [`GROUPS`].
    #[test]
    fn the_ecosystem_group_is_the_generation_catalog() {
        // One line per pattern the retired `.loreignore` generation emitted,
        // so a pattern cannot be dropped silently.
        for pattern in [
            "[Tt]arget/",
            "node_modules/",
            "dist/",
            "[Ll]ibrary/",
            "[Tt]emp/",
            "[Ll]ogs/",
            "[Oo]bj/",
            "[Uu]serSettings/",
            "[Mm]emoryCaptures/",
            "[Bb]uild/",
            "[Bb]uilds/",
            "*.meta",
            "[Bb]in/",
            "__pycache__/",
            "venv/",
            ".venv/",
            "*.pyc",
        ] {
            assert!(
                ECOSYSTEM_RULES
                    .lines()
                    .any(|line| line == pattern),
                "{pattern}"
            );
        }
    }
}
