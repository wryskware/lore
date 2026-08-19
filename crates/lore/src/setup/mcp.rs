//! `lore setup mcp` — wiring an agent host's MCP client to Lore's stdio server.
//!
//! The third thing a host needs, after the skills and the machine-wide ignore
//! rules: a line in its own configuration saying which executable to speak MCP
//! to. It is a separate target rather than a step folded into
//! `lore setup <host>` because it is the one piece a user loses on its own — a
//! config gets reset, a project is copied, a machine is re-imaged — and
//! re-running a whole host install to repair one JSON key is the wrong shape.
//!
//! # Why project scope by default
//!
//! A user-level registration means every agent session, in every directory on
//! the machine, spawns a Lore MCP server — including the many folders that are
//! not registered Lore projects and never will be. Registering per project
//! keeps the server where the index is. `--global` is available for the case
//! where a user genuinely wants it everywhere, but it is a choice, never the
//! default.
//!
//! # Why the server path is resolved, not configured
//!
//! [`server_binary`] takes the `lore-mcp` sitting beside the running `lore`, so
//! the registration always names the half that matches the half you ran. The
//! failure this rules out is a stale `lore-mcp` from an older build left on
//! `PATH`: the daemon moves forward, the MCP server does not, and nothing in
//! either process's output says so.
//!
//! # Editing someone else's file
//!
//! There is no stamp here; JSON has no comments to hide one in, and a sidecar
//! key would be surface for a host's schema to reject. Instead the entry itself
//! is the evidence: an `mcpServers.lore` whose command is *a* `lore-mcp` binary
//! is one of ours and may be repointed silently, and anything else under that
//! key is the user's and is kept until `--force`. Everything else in the file
//! is round-tripped untouched, key order included.

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Map, Value, json};

use super::{Outcome, State};

/// The `lore setup` target that wires the MCP server. Not a host: it is one key
/// in a host's configuration, and which host is implied by the scope.
pub const MCP_TARGET: &str = "mcp";

/// The name the server is registered under, and therefore the prefix an agent
/// sees on every tool (`mcp__lore__search`).
pub const SERVER_KEY: &str = "lore";

/// The file a project-scoped registration lives in, relative to the repo root.
/// Claude Code reads it from the directory a session starts in.
pub const PROJECT_CONFIG_FILE: &str = ".mcp.json";

/// Where a registration is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `<project>/.mcp.json` — the server exists for sessions in this repo.
    Project,
    /// `~/.claude.json` — the server exists for every session on this machine.
    User,
}

impl Scope {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "global",
        }
    }
}

/// The `lore-mcp` executable that matches the running `lore`.
///
/// Resolved as a sibling of the current executable rather than by searching
/// `PATH`: the two binaries ship together, and the one on `PATH` may be an
/// older install. Nothing here is a security boundary — the worst case is a
/// clear "not found" naming the directory it looked in.
pub fn server_binary() -> Result<Utf8PathBuf> {
    let exe = std::env::current_exe().context("locating the running lore executable")?;
    let dir = Utf8PathBuf::from_path_buf(exe)
        .ok()
        .and_then(|exe| exe.parent().map(Utf8Path::to_path_buf))
        .context("the running lore executable is not at a UTF-8 path")?;
    let server = dir.join(format!("lore-mcp{}", std::env::consts::EXE_SUFFIX));
    if !server.is_file() {
        bail!("no lore-mcp beside this lore binary (looked in {dir})");
    }
    Ok(server)
}

/// Whether a registered command is one Lore wrote, whatever build it points at.
///
/// Compared by file stem rather than by full path, which is exactly the
/// distinction that lets a stale registration be repaired silently while a
/// hand-written entry under the same key is protected.
///
/// The last path segment is taken by splitting on both `/` and `\` rather
/// than through [`Utf8Path`]'s own separator handling, because a config file
/// travels with the project and may have been written by a `lore` on a
/// different platform: `Utf8Path` only recognises `\` as a separator on
/// Windows, so a Linux process reading a Windows-written entry would
/// otherwise see the whole string as one opaque stem and never recognise its
/// own binary.
fn is_ours(command: &str) -> bool {
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _ext)| stem);
    stem.eq_ignore_ascii_case("lore-mcp")
}

/// One planned registration: what the host's config says now, and what it would
/// say after.
#[derive(Debug, Clone)]
pub struct Registration {
    pub scope: Scope,
    /// The host configuration file this writes into.
    pub path: Utf8PathBuf,
    /// The server the registration would name.
    pub server: Utf8PathBuf,
    /// The command currently registered under [`SERVER_KEY`], if any.
    pub current: Option<String>,
    pub state: State,
}

/// Where the config for `scope` lives. `root` is the project directory, and is
/// ignored for [`Scope::User`].
pub fn config_path(scope: Scope, root: &Utf8Path) -> Result<Utf8PathBuf> {
    match scope {
        Scope::Project => Ok(root.join(PROJECT_CONFIG_FILE)),
        Scope::User => {
            let home = dirs::home_dir()
                .and_then(|home| Utf8PathBuf::from_path_buf(home).ok())
                .context("cannot locate your home directory; set HOME (or USERPROFILE)")?;
            Ok(home.join(".claude.json"))
        }
    }
}

/// What the host's configuration says about Lore's server right now.
pub fn plan(scope: Scope, root: &Utf8Path, server: &Utf8Path) -> Result<Registration> {
    let path = config_path(scope, root)?;
    let current = read_config(&path)?.and_then(|config| entry_command(&config).map(str::to_string));
    let state = match current.as_deref() {
        None => State::Missing,
        Some(command) if command == server.as_str() => State::UpToDate,
        // A different `lore-mcp` — an older install, or a copy that moved.
        // Ours to fix, and fixing it is most of the point of this command.
        Some(command) if is_ours(command) => State::Outdated,
        Some(_) => State::Modified,
    };
    Ok(Registration {
        scope,
        path,
        server: server.to_path_buf(),
        current,
        state,
    })
}

/// Parse the config, or `None` when there is nothing there yet.
///
/// A file that exists but is not JSON is an error rather than a `None` that
/// would be overwritten: `~/.claude.json` holds a great deal a user cannot
/// reconstruct, and "unparseable" is far more likely to mean a bad read than a
/// file worth replacing.
fn read_config(path: &Utf8Path) -> Result<Option<Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {path}")),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let config = serde_json::from_str(&text)
        .with_context(|| format!("{path} is not valid JSON; lore will not rewrite it"))?;
    Ok(Some(config))
}

fn entry_command(config: &Value) -> Option<&str> {
    config
        .get("mcpServers")?
        .get(SERVER_KEY)?
        .get("command")?
        .as_str()
}

/// Write the registration, honouring the same never-clobber rule the file
/// assets get.
pub fn apply(registration: &Registration, force: bool) -> Result<Outcome> {
    let outcome = match (registration.state, force) {
        (State::Missing, _) => Outcome::Installed,
        (State::Outdated, _) => Outcome::Updated,
        (State::UpToDate, _) => return Ok(Outcome::Unchanged),
        (State::Modified, true) => Outcome::Overwrote,
        (State::Modified, false) => return Ok(Outcome::Kept),
    };

    let mut config = read_config(&registration.path)?.unwrap_or_else(|| Value::Object(Map::new()));
    let root = config
        .as_object_mut()
        .with_context(|| format!("{} is JSON but not an object", registration.path))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("{}: `mcpServers` is not an object", registration.path))?;
    servers.insert(SERVER_KEY.to_string(), server_entry(&registration.server));

    write_atomically(&registration.path, &config)?;
    Ok(outcome)
}

/// The registration itself. `lore-mcp` takes no arguments and discovers the
/// daemon through the handshake file, so there is nothing here to go stale but
/// the path.
fn server_entry(server: &Utf8Path) -> Value {
    json!({
        "type": "stdio",
        "command": server.as_str(),
        "args": [],
    })
}

/// Temp file in the same directory, then rename over the target — the pattern
/// [`crate::daemon::handshake`] uses, for the same reason. It matters more
/// here: the target may be `~/.claude.json` with an agent running, and a
/// half-written config is the one failure this must never produce.
fn write_atomically(path: &Utf8Path, config: &Value) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{path} has no parent directory"))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
    let temp = parent.join(format!(
        "{}.{}.tmp",
        path.file_name().unwrap_or("mcp.json"),
        std::process::id()
    ));
    let mut body = serde_json::to_vec_pretty(config)?;
    body.push(b'\n');
    std::fs::write(&temp, &body).with_context(|| format!("writing {temp}"))?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            Err(err).with_context(|| format!("updating {path}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        (dir, root)
    }

    #[test]
    fn registering_creates_the_file_and_names_the_server_we_ran_beside() {
        let (_dir, root) = temp();
        let exe = root.join("lore-mcp.exe");
        let planned = plan(Scope::Project, &root, &exe).unwrap();
        assert_eq!(planned.path, root.join(PROJECT_CONFIG_FILE));
        assert_eq!(planned.state, State::Missing);
        assert_eq!(apply(&planned, false).unwrap(), Outcome::Installed);

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&planned.path).unwrap()).unwrap();
        assert_eq!(entry_command(&written), Some(exe.as_str()));
        assert_eq!(
            plan(Scope::Project, &root, &exe).unwrap().state,
            State::UpToDate
        );
    }

    /// The repair case this command exists for: a registration pointing at an
    /// older `lore-mcp` is ours, and is repointed without asking.
    #[test]
    fn a_registration_naming_a_stale_lore_mcp_is_repaired_silently() {
        let (_dir, root) = temp();
        let old = root.join("old").join("lore-mcp.exe");
        let new = root.join("new").join("lore-mcp.exe");
        apply(&plan(Scope::Project, &root, &old).unwrap(), false).unwrap();

        let stale = plan(Scope::Project, &root, &new).unwrap();
        assert_eq!(stale.state, State::Outdated);
        assert_eq!(stale.current.as_deref(), Some(old.as_str()));
        assert_eq!(apply(&stale, false).unwrap(), Outcome::Updated);
        assert_eq!(
            plan(Scope::Project, &root, &new).unwrap().state,
            State::UpToDate
        );
    }

    /// An entry under our key that is not ours is the user's, exactly like an
    /// unstamped file at one of our asset paths.
    #[test]
    fn a_hand_written_entry_under_our_key_is_kept_until_force() {
        let (_dir, root) = temp();
        let path = root.join(PROJECT_CONFIG_FILE);
        std::fs::write(
            &path,
            r#"{"mcpServers":{"lore":{"command":"my-own-proxy","args":["--wrapped"]}}}"#,
        )
        .unwrap();
        let exe = root.join("lore-mcp.exe");

        let mine = plan(Scope::Project, &root, &exe).unwrap();
        assert_eq!(mine.state, State::Modified);
        assert_eq!(apply(&mine, false).unwrap(), Outcome::Kept);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("my-own-proxy")
        );

        assert_eq!(apply(&mine, true).unwrap(), Outcome::Overwrote);
        assert_eq!(
            plan(Scope::Project, &root, &exe).unwrap().state,
            State::UpToDate
        );
    }

    /// The whole risk of writing someone else's config: everything we did not
    /// come for has to survive, key order included, because `~/.claude.json`
    /// holds state a user cannot reconstruct.
    #[test]
    fn every_other_key_survives_a_registration_untouched() {
        let (_dir, root) = temp();
        let path = root.join(PROJECT_CONFIG_FILE);
        std::fs::write(
            &path,
            r#"{"zeta":1,"projects":{"/some/repo":{"history":["a","b"]}},"mcpServers":{"other":{"command":"other-server"}},"alpha":true}"#,
        )
        .unwrap();

        let exe = root.join("lore-mcp.exe");
        apply(&plan(Scope::Project, &root, &exe).unwrap(), false).unwrap();

        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["zeta"], json!(1));
        assert_eq!(after["alpha"], json!(true));
        assert_eq!(
            after["projects"]["/some/repo"]["history"],
            json!(["a", "b"])
        );
        assert_eq!(
            after["mcpServers"]["other"]["command"],
            json!("other-server")
        );
        // `preserve_order` is on for exactly this: a rewrite that reordered
        // every key would be an unreadable diff in a file users hand-edit.
        let keys: Vec<&str> = after
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["zeta", "projects", "mcpServers", "alpha"]);
    }

    /// Better to refuse than to replace a config we could not read.
    #[test]
    fn an_unparseable_config_is_an_error_not_a_fresh_start() {
        let (_dir, root) = temp();
        let path = root.join(PROJECT_CONFIG_FILE);
        std::fs::write(&path, "{ not json").unwrap();
        let err = plan(Scope::Project, &root, &root.join("lore-mcp.exe"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not valid JSON"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn ours_is_recognised_by_name_whatever_directory_it_sits_in() {
        assert!(is_ours("/opt/lore/lore-mcp"));
        assert!(is_ours(r"C:\Users\x\.cargo\bin\lore-mcp.exe"));
        assert!(!is_ours("lore"));
        assert!(!is_ours("my-own-proxy"));
        assert!(!is_ours("npx"));
    }

    /// A registration written on one platform must still be recognised as
    /// ours when read back on the other — a POSIX path on Windows, a
    /// Windows path on Linux — because a project config travels with the
    /// repo, not with the machine that wrote it.
    #[test]
    fn ours_is_recognised_across_the_other_platforms_path_separator_too() {
        assert!(is_ours("/usr/local/bin/lore-mcp"));
        assert!(!is_ours(r"C:\tools\not-lore-mcp.exe"));
        assert!(!is_ours("/opt/bin/npx"));
    }
}
