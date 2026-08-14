//! Shared scaffolding for the daemon integration tests.
//!
//! Every test gets two temp directories: a project root and a data directory.
//! They are separate on purpose — the daemon's own writes living inside a
//! watched project is a real failure mode, and tests that share one directory
//! would never catch it.

#![allow(dead_code)]

use camino::{Utf8Path, Utf8PathBuf};
use lore::daemon::index::IndexContext;
use lore::daemon::paths::canonicalize_root;
use lore::daemon::store_handle::StoreHandle;
use lore::store::Project;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

pub struct Fixture {
    /// Kept alive; dropping it deletes the directories.
    _project_dir: TempDir,
    _data_dir: TempDir,
    pub root: Utf8PathBuf,
    pub data_dir: Utf8PathBuf,
    pub store: StoreHandle,
    pub project: Project,
    pub cancel: CancellationToken,
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        let project_dir = tempfile::tempdir().expect("project tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        // Canonicalized exactly as the daemon does, so paths compare equal.
        let root = canonicalize_root(project_dir.path()).expect("canonical root");
        let data = canonicalize_root(data_dir.path()).expect("canonical data dir");

        let store = StoreHandle::open(data.join("lore.db")).expect("open store");
        let id = {
            let root = root.clone();
            let name = name.to_string();
            store
                .blocking(move |store| store.register_project(&root, &name))
                .expect("register project")
        };

        Self {
            _project_dir: project_dir,
            _data_dir: data_dir,
            project: Project {
                id,
                root: root.clone(),
                name: name.to_string(),
            },
            root,
            data_dir: data,
            store,
            cancel: CancellationToken::new(),
        }
    }

    pub fn context(&self) -> IndexContext {
        IndexContext::new(
            self.store.clone(),
            self.data_dir.clone(),
            self.cancel.clone(),
        )
    }

    /// Create (or overwrite) a project file, creating parent directories.
    pub fn write(&self, rel: &str, contents: impl AsRef<[u8]>) -> Utf8PathBuf {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&path, contents).expect("write fixture file");
        path
    }

    pub fn remove(&self, rel: &str) {
        std::fs::remove_file(self.root.join(rel)).expect("remove fixture file");
    }

    pub fn remove_dir(&self, rel: &str) {
        std::fs::remove_dir_all(self.root.join(rel)).expect("remove fixture dir");
    }

    /// Project-relative paths currently recorded in the index, sorted.
    pub fn indexed_paths(&self) -> Vec<String> {
        let id = self.project.id;
        let mut paths: Vec<String> = self
            .store
            .blocking(move |store| store.list_files(id))
            .expect("list files")
            .into_iter()
            .map(|record| record.path.into_string())
            .collect();
        paths.sort();
        paths
    }

    pub fn chunk_count(&self) -> u64 {
        let id = self.project.id;
        self.store
            .blocking(move |store| store.status())
            .expect("status")
            .projects
            .into_iter()
            .find(|p| p.project == id)
            .map(|p| p.chunks)
            .unwrap_or(0)
    }

    pub fn generation(&self) -> u64 {
        self.store
            .blocking(|store| store.generation())
            .expect("generation")
    }
}

/// A small but realistic project: code, vault-style Markdown, a `.gitignore`
/// with both a directory and a glob rule, plus every category of thing that
/// must never be indexed.
pub fn populate_standard_tree(fixture: &Fixture) {
    fixture.write(
        "src/lib.rs",
        "pub fn alpha() -> u32 {\n    41\n}\n\npub fn beta() -> u32 {\n    alpha() + 1\n}\n",
    );
    fixture.write(
        "docs/design.md",
        "---\ndesign_status: decided\ndecision_refs: [D-0007]\n---\n\n# Topology\n\n\
         The daemon owns index state.\n\n## Watcher\n\nDebounced, coalescing.\n",
    );
    fixture.write("README.md", "# Demo\n\nA project used by tests.\n");

    fixture.write(".gitignore", "ignored/\n*.log\n");
    fixture.write("ignored/secret.txt", "should never be indexed\n");
    fixture.write("noisy.log", "log line\n");

    fixture.write("target/debug/build.rs", "fn generated() {}\n");
    fixture.write("node_modules/pkg/index.js", "module.exports = {};\n");
    fixture.write("Library/ScriptAssemblies/Asm.cs", "class Asm {}\n");
    fixture.write(".vs/state.txt", "editor state\n");
    fixture.write(".hidden/notes.txt", "hidden\n");
}

/// What [`populate_standard_tree`] should leave in the index.
pub const STANDARD_TREE_INDEXED: [&str; 3] = ["README.md", "docs/design.md", "src/lib.rs"];

pub fn as_utf8(path: &std::path::Path) -> &Utf8Path {
    Utf8Path::from_path(path).expect("utf-8 path")
}
