//! The daemon side of chunker plugins: installation, per-project opt-in, what
//! that costs the index, and what the surfaces say about all of it.
//!
//! `plugin_registry.rs` and `plugin_chunking.rs` cover the engine — what a
//! manifest means and what a plugin may do to a chunk. This file covers the
//! daemon: which plugins are in force for a project, what enabling one
//! re-chunks (and what it must *not*), what falls back, and how a human or a
//! pusher learns any of it.
//!
//! The invariant everything here defends is the one an existing installation
//! feels: **a file no enabled plugin claims must stamp exactly as it does
//! today**. Chunker plugins arrived without a `CHUNK_FORMAT_VERSION` bump, so
//! if that were wrong, every corpus on every machine would silently re-chunk
//! the first time it was touched.
//!
//! The plugins below use the `windows` strategy and carry no assets on purpose:
//! it is the one strategy that behaves identically with and without the
//! `wasm-grammars` feature, so these tests assert the same thing in both
//! builds. The one deliberately-broken plugin references a `.wasm` file that is
//! not there, which is unavailable in either build for the same reason.

mod daemon_support;

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use camino::{Utf8Path, Utf8PathBuf};
use daemon_support::Fixture;
use lore::config::Config;
use lore::daemon::http::{AppState, router};
use lore::daemon::index::{IndexContext, PassSummary, content_stamp, full_scan};
use lore::daemon::queue::IndexQueue;
use lore::daemon::watch;
use lore::embed::Embedder;
use lore::plugin::PluginRegistry;
use lore::store::NewEmbedding;
use lore::types::ChunkId;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A plugin that works everywhere: line windows over `toydata`, no assets.
const TOY: &str = "[plugin]\nname = \"toy\"\n\n[[chunker]]\nextensions = [\"toydata\"]\n\
                   strategy = \"windows\"\nwindow_lines = 4\noverlap_lines = 1\n\
                   language_tag = \"toydata\"\n";

/// A plugin that claims an extension and cannot possibly chunk it: the grammar
/// it names is not in the directory. Unavailable in every build.
const BROKEN: &str = "[plugin]\nname = \"broken\"\n\n[[chunker]]\nextensions = [\"toydata\"]\n\
                      strategy = \"grammar\"\ngrammar = \"absent.wasm\"\n\
                      language_tag = \"toydata\"\ncontainers = [\"element\"]\n";

/// Install plugin manifests into a temp directory and load them as the daemon
/// does at startup.
fn registry(dir: &Utf8Path, plugins: &[(&str, &str)]) -> Arc<PluginRegistry> {
    for (folder, manifest) in plugins {
        let root = dir.join(folder);
        std::fs::create_dir_all(&root).expect("plugin dir");
        std::fs::write(root.join("lore-plugin.toml"), manifest).expect("manifest");
    }
    let (registry, diagnostics) = PluginRegistry::load(dir);
    assert!(
        diagnostics
            .iter()
            .all(|d| d.to_string().contains("unavailable")),
        "unexpected load diagnostics: {diagnostics:?}"
    );
    Arc::new(registry)
}

/// A temp directory that outlives the test, standing in for `<data-dir>/plugins`.
fn plugin_dir() -> (tempfile::TempDir, Utf8PathBuf) {
    let dir = tempfile::tempdir().expect("plugin tempdir");
    let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8");
    (dir, path)
}

/// A project holding one file of each interesting kind: one a plugin claims,
/// one a built-in owns, one Markdown (never claimable), one nobody claims.
fn populate(fixture: &Fixture) {
    fixture.write(".loreignore", ".*\n");
    fixture.write("Assets/Level.toydata", lines(20));
    fixture.write("src/lib.rs", "pub fn alpha() -> u32 {\n    41\n}\n");
    fixture.write("README.md", "# Demo\n\nA project used by tests.\n");
    fixture.write("notes.rst", lines(6));
}

fn lines(count: usize) -> String {
    (0..count).map(|i| format!("line {i}\n")).collect()
}

/// The stored content stamp of every indexed file, path → stamp.
fn stamps(fixture: &Fixture) -> Vec<(String, String)> {
    let id = fixture.project.id;
    let mut rows: Vec<(String, String)> = fixture
        .store
        .blocking(move |store| store.list_files(id))
        .expect("list files")
        .into_iter()
        .map(|record| (record.path.into_string(), record.content_hash))
        .collect();
    rows.sort();
    rows
}

/// Every chunk of one indexed file.
fn file_chunks(fixture: &Fixture, path: &str) -> Vec<lore::types::Chunk> {
    let id = fixture.project.id;
    let path = Utf8PathBuf::from(path);
    fixture
        .store
        .blocking(move |store| store.get_file_chunks(id, &path))
        .expect("file chunks")
}

fn chunk_ids(fixture: &Fixture) -> Vec<ChunkId> {
    let id = fixture.project.id;
    let files = fixture
        .store
        .blocking(move |store| store.list_files(id))
        .expect("list files");
    let mut ids: Vec<ChunkId> = files
        .into_iter()
        .flat_map(|record| {
            file_chunks(fixture, record.path.as_str())
                .into_iter()
                .map(|chunk| chunk.id)
        })
        .collect();
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    ids
}

/// Enable these plugins in the project's `.lore.toml`.
fn enable(fixture: &Fixture, names: &[&str]) {
    let list = names
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fixture.write(
        lore::repo_config::REPO_CONFIG_FILE,
        format!("[plugins]\nenable = [{list}]\n"),
    );
}

fn scan(fixture: &Fixture, plugins: &Arc<PluginRegistry>) -> PassSummary {
    let ctx = fixture.context().with_plugins(plugins.clone());
    full_scan(&ctx, &fixture.project)
}

// ---------------------------------------------------------------------------
// The stamp
// ---------------------------------------------------------------------------

/// The no-storm guarantee, at the level it actually has to hold: the stored
/// stamp of a file no *enabled* plugin claims is byte-identical to the one a
/// daemon with no plugin support at all would have written.
///
/// Installing a plugin machine-wide must not move a single stamp either: until
/// a repository names it, an installed plugin is inert.
#[test]
fn an_unclaimed_file_stamps_exactly_as_it_did_before_plugins_existed() {
    let hash = "abc123";
    for path in [
        "src/lib.rs",
        "README.md",
        "notes.rst",
        "Assets/Level.toydata",
    ] {
        let path = Utf8Path::new(path);
        let bare = content_stamp(path, hash, None, None);
        assert_eq!(
            bare,
            content_stamp(path, hash, None, Some(&PluginRegistry::empty())),
            "an empty registry is not the same as no registry for {path}"
        );
        assert_eq!(
            bare,
            format!("v{}-{hash}", lore::chunk::CHUNK_FORMAT_VERSION),
            "the pre-plugin stamp shape moved for {path}"
        );
    }

    // The claimed one, and only it, gains the plugin's identity — and the
    // Markdown profile tag is unaffected in either direction.
    let (_guard, dir) = plugin_dir();
    let plugins = registry(&dir, &[("toy", TOY)]);
    let enabled = plugins.enabled_only(&BTreeSet::from(["toy".to_string()]));
    let fingerprint = &plugins.get("toy").unwrap().fingerprint;

    let claimed = Utf8Path::new("Assets/Level.toydata");
    assert_eq!(
        content_stamp(claimed, hash, None, Some(&enabled)),
        format!(
            "v{}+toy@{fingerprint}-{hash}",
            lore::chunk::CHUNK_FORMAT_VERSION
        )
    );
    for untouched in ["src/lib.rs", "notes.rst"] {
        let path = Utf8Path::new(untouched);
        assert_eq!(
            content_stamp(path, hash, None, Some(&enabled)),
            content_stamp(path, hash, None, None),
            "{untouched} is not claimed and must stamp unchanged"
        );
    }
    let md = Utf8Path::new("README.md");
    assert_eq!(
        content_stamp(
            md,
            hash,
            Some(lore::repo_config::Profile::LoreV1),
            Some(&enabled)
        ),
        content_stamp(md, hash, Some(lore::repo_config::Profile::LoreV1), None),
        "Markdown is never claimable, so a registry cannot change its stamp"
    );
}

/// The same property through the pipeline: a pass with plugins installed but
/// not enabled writes nothing at all — every file is `unchanged`.
#[test]
fn installing_a_plugin_re_chunks_nothing_until_a_repository_enables_it() {
    let fixture = Fixture::neutral("inert");
    populate(&fixture);
    let empty = Arc::new(PluginRegistry::empty());
    let first = scan(&fixture, &empty);
    assert!(first.indexed >= 4, "{first:?}");
    let before = stamps(&fixture);

    let (_guard, dir) = plugin_dir();
    let plugins = registry(&dir, &[("toy", TOY)]);
    let installed = scan(&fixture, &plugins);
    assert_eq!(
        (installed.indexed, installed.unchanged),
        (0, first.indexed),
        "an installed-but-unenabled plugin re-chunked something: {installed:?}"
    );
    assert_eq!(stamps(&fixture), before, "stamps moved without an opt-in");
    assert_eq!(installed.plugin_fallbacks, 0);
}

// ---------------------------------------------------------------------------
// Enabling, disabling, editing
// ---------------------------------------------------------------------------

/// Enabling a plugin re-chunks exactly the files it claims. Disabling it puts
/// them back. Nothing else in the project is touched by either.
#[test]
fn an_enablement_flip_re_chunks_the_claimed_files_and_only_those() {
    let fixture = Fixture::neutral("flip");
    populate(&fixture);
    let (_guard, dir) = plugin_dir();
    let plugins = registry(&dir, &[("toy", TOY)]);

    let first = scan(&fixture, &plugins);
    let before = stamps(&fixture);

    enable(&fixture, &["toy"]);
    let on = scan(&fixture, &plugins);
    assert_eq!(on.indexed, 1, "exactly the claimed file re-chunks: {on:?}");
    assert_eq!(on.unchanged, first.indexed - 1, "{on:?}");
    let claimed = stamps(&fixture);
    for (path, stamp) in &claimed {
        let was = &before
            .iter()
            .find(|(p, _)| p == path)
            .expect("the same files are indexed")
            .1;
        match path.as_str() {
            "Assets/Level.toydata" => {
                assert_ne!(stamp, was, "the claimed file's stamp must move");
                assert!(stamp.contains("+toy@"), "{stamp}");
            }
            other => assert_eq!(stamp, was, "{other} moved and nothing claims it"),
        }
    }

    // ...and the language tag the plugin declared is what the chunks carry,
    // which is the observable half of "the plugin actually ran".
    let languages: BTreeSet<String> = file_chunks(&fixture, "Assets/Level.toydata")
        .into_iter()
        .filter_map(|chunk| chunk.language)
        .collect();
    assert_eq!(languages, BTreeSet::from(["toydata".to_string()]));

    // Opting back out is the same move in reverse, and lands exactly where the
    // project started.
    fixture.write(
        lore::repo_config::REPO_CONFIG_FILE,
        "[plugins]\nenable = []\n",
    );
    let off = scan(&fixture, &plugins);
    assert_eq!(off.indexed, 1, "{off:?}");
    assert_eq!(stamps(&fixture), before, "opting out is not a one-way door");
}

/// Editing a plugin re-chunks the files it owns without re-embedding the chunks
/// whose text did not move — the same property the authority-profile flip has,
/// and for the same reason: chunk ids are content-addressed.
///
/// The edit here is a comment. It changes the fingerprint (which is over bytes,
/// deliberately) and changes nothing about how the file splits, which is
/// exactly the case where re-embedding would be pure waste.
#[test]
fn editing_a_plugin_re_chunks_its_files_without_moving_a_chunk_id() {
    let fixture = Fixture::neutral("edit");
    populate(&fixture);
    let (_guard, dir) = plugin_dir();
    let plugins = registry(&dir, &[("toy", TOY)]);
    enable(&fixture, &["toy"]);
    scan(&fixture, &plugins);

    let ids_before = chunk_ids(&fixture);
    let embeddings: Vec<NewEmbedding> = ids_before
        .iter()
        .map(|id| NewEmbedding {
            project: fixture.project.id,
            chunk_id: id.clone(),
            vector: vec![0.25, 0.5, 0.25, 0.5],
        })
        .collect();
    let embedded = fixture
        .store
        .blocking(move |store| store.upsert_embeddings(&embeddings))
        .expect("store embeddings");
    assert_eq!(embedded, ids_before.len());

    std::fs::write(
        dir.join("toy/lore-plugin.toml"),
        format!("{TOY}\n# a comment that changes nothing but the bytes\n"),
    )
    .unwrap();
    let (edited, _) = PluginRegistry::load(&dir);
    let edited = Arc::new(edited);
    assert_ne!(
        edited.get("toy").unwrap().fingerprint,
        plugins.get("toy").unwrap().fingerprint
    );

    let after = scan(&fixture, &edited);
    assert_eq!(after.indexed, 1, "the plugin's file re-chunks: {after:?}");
    assert_eq!(
        (after.chunks_inserted, after.chunks_deleted),
        (0, 0),
        "…onto the same chunk ids, so nothing is re-embedded: {after:?}"
    );
    assert!(after.chunks_kept > 0, "{after:?}");
    assert_eq!(chunk_ids(&fixture), ids_before);

    // The vectors are still there, which is the thing the id stability was
    // protecting.
    let id = fixture.project.id;
    let still_embedded = fixture
        .store
        .blocking(move |store| store.status())
        .expect("status")
        .projects
        .into_iter()
        .find(|p| p.project == id)
        .map(|p| p.embedded_chunks)
        .unwrap_or(0);
    assert_eq!(still_embedded as usize, ids_before.len());
}

/// A plugin that claims a file and cannot run is counted, per pass, per
/// project — the contract's one quality-cliff requirement, because the file it
/// produced looks exactly like a file nobody ever claimed.
#[test]
fn files_that_fall_back_are_counted_for_every_pass() {
    let fixture = Fixture::neutral("fallback");
    populate(&fixture);
    fixture.write("Assets/Other.toydata", lines(8));
    let (_guard, dir) = plugin_dir();
    let plugins = registry(&dir, &[("broken", BROKEN)]);
    let ctx = fixture.context().with_plugins(plugins.clone());

    // Nothing enabled: nothing claimed, so nothing falls back.
    let inert = full_scan(&ctx, &fixture.project);
    assert_eq!(inert.plugin_fallbacks, 0);
    assert_eq!(ctx.fallbacks.of(fixture.project.id), 0);

    enable(&fixture, &["broken"]);
    let fell_back = full_scan(&ctx, &fixture.project);
    assert_eq!(
        fell_back.plugin_fallbacks, 2,
        "both claimed files fell back: {fell_back:?}"
    );
    assert_eq!(ctx.fallbacks.of(fixture.project.id), 2);
    // They are still indexed — a fallback is a worse chunking, never a lost
    // file.
    assert!(
        fixture
            .indexed_paths()
            .contains(&"Assets/Level.toydata".to_string())
    );

    // The count describes the most recent pass, so a project that stops
    // falling back stops being reported.
    fixture.write(
        lore::repo_config::REPO_CONFIG_FILE,
        "[plugins]\nenable = []\n",
    );
    let recovered = full_scan(&ctx, &fixture.project);
    assert_eq!(recovered.plugin_fallbacks, 0);
    assert_eq!(ctx.fallbacks.of(fixture.project.id), 0);
}

// ---------------------------------------------------------------------------
// The surfaces
// ---------------------------------------------------------------------------

/// The router over a **caller-owned** index context, because the fallback count
/// `status` reports is per-context in-memory state: a harness that built its
/// own context would report on passes nobody ran.
fn harness(
    fixture: &Fixture,
    ctx: &IndexContext,
    plugins: Arc<PluginRegistry>,
    diagnostics: Vec<String>,
) -> Router {
    let (watch_tx, _watch_rx) = watch::channel();
    router(AppState {
        store: fixture.store.clone(),
        queue: IndexQueue::new(),
        watch: watch_tx,
        watch_status: watch::WatchStatus::new(),
        index: ctx.clone(),
        push: fixture.push_leases(),
        config: Arc::new(Config::default()),
        embeddings: Embedder::disabled(),
        latency: lore::daemon::latency::LatencyRecorder::default(),
        plugins,
        plugin_diagnostics: Arc::new(diagnostics),
        data_dir: fixture.data_dir.clone(),
        shutdown: fixture.cancel.clone(),
    })
}

async fn call(router: &Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let mut request = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .expect("router never fails");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).expect("JSON body")
}

/// What `status` says, in all three states: installed and in force, enabled but
/// absent, and installed but unable to run.
///
/// Absence is asserted as hard as presence. A project that enabled nothing must
/// carry no plugin fields at all, because a `[]` on every project is noise that
/// makes the one project with something to say invisible.
#[tokio::test]
async fn status_reports_the_installed_set_the_enabled_set_and_the_gap() {
    let fixture = Fixture::neutral("surface");
    populate(&fixture);
    let (_guard, dir) = plugin_dir();
    let plugins = registry(&dir, &[("toy", TOY)]);
    let ctx = fixture.context().with_plugins(plugins.clone());
    let router = harness(
        &fixture,
        &ctx,
        plugins.clone(),
        vec!["plugin \"ghost\" was not loaded: bad manifest".to_string()],
    );

    // Nothing enabled: the machine-wide half is present, the per-project half
    // is absent entirely.
    full_scan(&ctx, &fixture.project);
    let body = call(&router, "GET", "/v1/status", None).await;
    let installed = &body["plugins"][0];
    assert_eq!(installed["name"], "toy");
    assert_eq!(
        installed["fingerprint"],
        json!(plugins.get("toy").unwrap().fingerprint)
    );
    assert_eq!(installed["extensions"], json!(["toydata"]));
    assert_eq!(
        body["plugin_diagnostics"],
        json!(["plugin \"ghost\" was not loaded: bad manifest"])
    );
    let project = &body["projects"][0];
    assert!(project.get("plugins_enabled").is_none(), "{project}");
    assert!(project.get("plugins_missing").is_none(), "{project}");
    assert!(project.get("plugin_fallback_files").is_none(), "{project}");

    // Enabled, plus a name nobody installed: both halves are named, and the
    // missing one is not an error anywhere.
    enable(&fixture, &["toy", "unity"]);
    full_scan(&ctx, &fixture.project);
    let body = call(&router, "GET", "/v1/status", None).await;
    let project = &body["projects"][0];
    assert_eq!(project["plugins_enabled"][0]["name"], "toy");
    assert_eq!(
        project["plugins_enabled"][0]["fingerprint"],
        json!(plugins.get("toy").unwrap().fingerprint)
    );
    assert_eq!(project["plugins_missing"], json!(["unity"]));
    assert!(project.get("plugin_fallback_files").is_none(), "{project}");
}

/// The fallback count reaches `status`, and leaves it again when the condition
/// does: a number that stuck around after the plugin started working would be
/// worse than no number.
#[tokio::test]
async fn status_counts_the_files_a_plugin_could_not_chunk() {
    let fixture = Fixture::neutral("fell-back");
    populate(&fixture);
    let (_guard, dir) = plugin_dir();
    let plugins = registry(&dir, &[("broken", BROKEN)]);
    let ctx = fixture.context().with_plugins(plugins.clone());
    let router = harness(&fixture, &ctx, plugins.clone(), Vec::new());

    enable(&fixture, &["broken"]);
    full_scan(&ctx, &fixture.project);
    let body = call(&router, "GET", "/v1/status", None).await;
    assert_eq!(body["projects"][0]["plugin_fallback_files"], json!(1));
    assert_eq!(body["projects"][0]["plugins_enabled"][0]["name"], "broken");

    fixture.write(
        lore::repo_config::REPO_CONFIG_FILE,
        "[plugins]\nenable = []\n",
    );
    full_scan(&ctx, &fixture.project);
    let body = call(&router, "GET", "/v1/status", None).await;
    let project = &body["projects"][0];
    assert!(project.get("plugin_fallback_files").is_none(), "{project}");
    assert!(project.get("plugins_enabled").is_none(), "{project}");
}

/// The push lease advertises the receiver's installed plugins. One-way: a
/// pusher can see a mismatch, and there is nothing to negotiate about it.
#[tokio::test]
async fn a_push_lease_advertises_the_receiving_daemons_plugins() {
    let fixture = Fixture::neutral("push");
    let (_guard, dir) = plugin_dir();
    let plugins = registry(&dir, &[("toy", TOY)]);
    let ctx = fixture.context().with_plugins(plugins.clone());
    let router = harness(&fixture, &ctx, plugins.clone(), Vec::new());

    let lease = call(
        &router,
        "POST",
        "/v1/push/lease",
        Some(json!({ "project": "push" })),
    )
    .await;
    assert_eq!(
        lease["plugins"],
        json!([{
            "name": "toy",
            "fingerprint": plugins.get("toy").unwrap().fingerprint,
            "extensions": ["toydata"],
        }]),
        "{lease}"
    );

    // A receiver with nothing installed says nothing rather than saying `[]`:
    // the field is omitted, and a pusher reads that as "no plugins", which is
    // what it is.
    let bare = harness(
        &fixture,
        &fixture.context(),
        Arc::new(PluginRegistry::empty()),
        Vec::new(),
    );
    let lease = call(
        &bare,
        "POST",
        "/v1/push/lease",
        Some(json!({ "project": "push" })),
    )
    .await;
    assert!(lease.get("plugins").is_none(), "{lease}");
}
