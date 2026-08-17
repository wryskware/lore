//! Chunker plugins where two features meet: the push receiving side (D-0015)
//! and the stamp lifecycle.
//!
//! `daemon_plugins.rs` covers a plugin against a local pass. This file covers
//! the seams that a per-feature suite cannot see from inside itself, because
//! each one is a claim about two subsystems *agreeing*:
//!
//! * chunking is receiver-side, permanently, so a pushed project must chunk
//!   through the plugins the **receiver's** `.lore.toml` enables — a pusher
//!   never sends one and never negotiates about one;
//! * the push manifest diff and the indexing pass ask the same question ("does
//!   the store already have this content?") from two different call sites. If
//!   the diff's answer were the looser one, every pushed file would be
//!   re-requested forever; if it were the tighter one, a file that had to
//!   re-chunk would never be asked for and the index would silently keep chunks
//!   no plugin would produce today;
//! * enabling a plugin and disabling it again are one round trip, and the store
//!   has to land exactly where it started — same chunks, same rows, nothing
//!   orphaned behind a stamp that moved.
//!
//! Everything here uses the `windows` strategy and carries no assets, for the
//! reason `daemon_plugins.rs` gives: it is the one strategy that behaves
//! identically with and without `wasm-grammars`.

mod daemon_support;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use camino::Utf8PathBuf;
use daemon_support::Fixture;
use lore::config::Config;
use lore::daemon::http::{AppState, router};
use lore::daemon::index::{IndexContext, full_scan};
use lore::daemon::queue::IndexQueue;
use lore::daemon::watch;
use lore::embed::Embedder;
use lore::plugin::PluginRegistry;
use lore::types::{Chunk, ChunkKind};
use lore_core::snapshot::{Manifest, ManifestEntry};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A plugin that works in every build: capped line windows over `toydata`.
const TOY: &str = "[plugin]\nname = \"toy\"\n\n[[chunker]]\nextensions = [\"toydata\"]\n\
                   strategy = \"windows\"\nwindow_lines = 4\noverlap_lines = 1\n\
                   language_tag = \"toydata\"\n";

/// A second, unrelated plugin, so "editing one" has an "other" to leave alone.
const OTHER: &str = "[plugin]\nname = \"other\"\n\n[[chunker]]\nextensions = [\"otherdata\"]\n\
                     strategy = \"windows\"\nwindow_lines = 4\noverlap_lines = 1\n\
                     language_tag = \"otherdata\"\n";

/// The pushed file's content and path. It is never written to the project root:
/// if its chunks exist, they came through the staging area.
const PUSHED_PATH: &str = "Assets/Level.toydata";
const OTHER_PATH: &str = "src/lib.rs";
const OTHER_BODY: &str = "pub fn alpha() -> u32 {\n    41\n}\n";

fn lines(count: usize) -> String {
    (0..count).map(|i| format!("line {i}\n")).collect()
}

/// A temp directory standing in for `<data-dir>/plugins`, plus the registry
/// loaded out of it exactly as the daemon loads it at startup.
fn plugins(manifests: &[(&str, &str)]) -> (tempfile::TempDir, Utf8PathBuf, Arc<PluginRegistry>) {
    let guard = tempfile::tempdir().expect("plugin tempdir");
    let dir = Utf8PathBuf::from_path_buf(guard.path().to_path_buf()).expect("utf-8");
    for (folder, manifest) in manifests {
        let root = dir.join(folder);
        std::fs::create_dir_all(&root).expect("plugin dir");
        std::fs::write(root.join("lore-plugin.toml"), manifest).expect("manifest");
    }
    let (registry, diagnostics) = PluginRegistry::load(&dir);
    assert_eq!(diagnostics, vec![], "the test plugins must load cleanly");
    (guard, dir, Arc::new(registry))
}

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

/// The router over a caller-owned index context, so a test drives the same
/// pipeline a push commit applies through.
fn harness(fixture: &Fixture, ctx: &IndexContext, registry: Arc<PluginRegistry>) -> Router {
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
        plugins: registry,
        plugin_diagnostics: Arc::new(Vec::new()),
        data_dir: fixture.data_dir.clone(),
        shutdown: fixture.cancel.clone(),
    })
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router never fails");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = match bytes.is_empty() {
        true => Value::Null,
        false => serde_json::from_slice(&bytes).expect("a JSON body"),
    };
    (status, json)
}

async fn post(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    send(
        router,
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

fn manifest_entry(path: &str, body: &str) -> ManifestEntry {
    ManifestEntry {
        path: path.into(),
        hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
        size: body.len() as u64,
    }
}

/// The whole listing this suite pushes: one file a plugin may claim, one it
/// never can. Built through [`Manifest::new`] so it carries the integrity
/// checksum a real observer computes.
fn manifest(claimed: &str) -> Value {
    let manifest = Manifest::new(vec![
        manifest_entry(PUSHED_PATH, claimed),
        manifest_entry(OTHER_PATH, OTHER_BODY),
    ]);
    serde_json::to_value(manifest).expect("a manifest serializes")
}

async fn lease(router: &Router, project: &str) -> (String, u64) {
    let (status, body) = post(router, "/v1/push/lease", json!({ "project": project })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        body["session"].as_str().expect("a session").to_string(),
        body["epoch"].as_u64().expect("an epoch"),
    )
}

/// Step 2: send the listing, learn what the receiver still needs. This is the
/// call whose answer has to agree with the pass.
async fn negotiate(
    router: &Router,
    project: &str,
    session: &str,
    epoch: u64,
    manifest: &Value,
) -> Vec<String> {
    let (status, body) = post(
        router,
        "/v1/push/manifest",
        json!({
            "project": project,
            "session": session,
            "epoch": epoch,
            "manifest": manifest,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["needed"]
        .as_array()
        .expect("a needed list")
        .iter()
        .map(|v| v.as_str().expect("a path").to_string())
        .collect()
}

async fn upload(router: &Router, project: &str, session: &str, epoch: u64, path: &str, body: &str) {
    let (status, response) = send(
        router,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/v1/push/file?project={project}&session={session}&epoch={epoch}&path={path}"
            ))
            .body(Body::from(body.as_bytes().to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
}

async fn commit(router: &Router, project: &str, session: &str, epoch: u64) -> Value {
    let (status, body) = post(
        router,
        "/v1/push/commit",
        json!({ "project": project, "session": session, "epoch": epoch }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// One full push of `claimed` as the `.toydata` file, from lease to commit.
async fn push(router: &Router, project: &str, claimed: &str) -> Vec<String> {
    let (session, epoch) = lease(router, project).await;
    let listing = manifest(claimed);
    let needed = negotiate(router, project, &session, epoch, &listing).await;
    for path in &needed {
        let body = match path.as_str() {
            PUSHED_PATH => claimed,
            OTHER_PATH => OTHER_BODY,
            other => panic!("unexpected path {other}"),
        };
        upload(router, project, &session, epoch, path, body).await;
    }
    commit(router, project, &session, epoch).await;
    needed
}

fn file_chunks(fixture: &Fixture, path: &str) -> Vec<Chunk> {
    let id = fixture.project.id;
    let path = Utf8PathBuf::from(path);
    fixture
        .store
        .blocking(move |store| store.get_file_chunks(id, &path))
        .expect("file chunks")
}

/// Every indexed file's stamp, path → stamp, sorted.
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

// ---------------------------------------------------------------------------
// The push path
// ---------------------------------------------------------------------------

/// A pushed snapshot chunks through the **receiver's** plugins, read from the
/// receiver's own `.lore.toml` at the registered root.
///
/// This is D-0015's half of the contract: chunking is daemon-side permanently,
/// so the receiving daemon's plugin set governs and the pusher only ever *sees*
/// what that set is. Nothing in a push request names a plugin.
#[tokio::test]
async fn a_pushed_project_chunks_through_the_receivers_own_plugins() {
    let fixture = Fixture::neutral("remote");
    let (_guard, _dir, registry) = plugins(&[("toy", TOY)]);
    let ctx = fixture.context().with_plugins(registry.clone());
    let router = harness(&fixture, &ctx, registry.clone());
    // The opt-in lives on the receiver's disk. The pushed content never does.
    enable(&fixture, &["toy"]);

    let claimed = lines(10);
    let needed = push(&router, "remote", &claimed).await;
    assert_eq!(needed.len(), 2, "a fresh project needs everything");

    // Never written to the project root: these chunks can only have come
    // through the staging area and the apply pipeline.
    assert!(!fixture.root.join(PUSHED_PATH).exists());

    // The plugin ran: its geometry (4-line windows, 1 of overlap) and its
    // language tag, neither of which core would have produced.
    let chunks = file_chunks(&fixture, PUSHED_PATH);
    assert_eq!(
        chunks
            .iter()
            .map(|c| (c.line_start, c.line_end))
            .collect::<Vec<_>>(),
        [(1, 4), (4, 7), (7, 10)]
    );
    assert!(
        chunks
            .iter()
            .all(|c| c.language.as_deref() == Some("toydata"))
    );
    assert!(
        chunks
            .iter()
            .all(|c| matches!(c.kind, ChunkKind::Window { .. }) && c.vault.is_none())
    );

    // ...and it is recorded as having run, which is what the next diff reads.
    let stamps = stamps(&fixture);
    let stamp = &stamps
        .iter()
        .find(|(path, _)| path == PUSHED_PATH)
        .expect("the pushed file is indexed")
        .1;
    assert!(stamp.contains("+toy@"), "{stamp}");
    // The file the plugin does not claim is stamped as it always was.
    let other = &stamps
        .iter()
        .find(|(path, _)| path == OTHER_PATH)
        .unwrap()
        .1;
    assert!(!other.contains("+toy"), "{other}");
}

/// The manifest diff and the indexing pass ask one question, and it has to be
/// the same question: a pusher re-sending an unchanged listing is told there is
/// nothing to send, and a receiver that has since enabled a plugin asks for
/// exactly the files that plugin now claims.
///
/// Both failure modes are silent. A diff that ignored plugins would consider
/// every claimed file unchanged forever, so enabling a plugin over a pushed
/// project would do nothing at all; a diff that stamped differently from the
/// pass would re-request the same file on every push, forever.
#[tokio::test]
async fn the_manifest_diff_and_the_indexing_pass_ask_one_question() {
    let fixture = Fixture::neutral("agree");
    let (_guard, _dir, registry) = plugins(&[("toy", TOY)]);
    let ctx = fixture.context().with_plugins(registry.clone());
    let router = harness(&fixture, &ctx, registry.clone());

    // No opt-in yet: the file is chunked by the built-in fallback.
    let claimed = lines(10);
    push(&router, "agree", &claimed).await;
    let before = file_chunks(&fixture, PUSHED_PATH);
    assert!(before.iter().all(|c| c.language.is_none()));

    // Push the very same listing again: nothing is needed. This is the loop
    // the two stamping rules exist to prevent.
    let (session, epoch) = lease(&router, "agree").await;
    let idle = negotiate(&router, "agree", &session, epoch, &manifest(&claimed)).await;
    assert_eq!(idle, Vec::<String>::new(), "an unchanged push re-uploaded");
    commit(&router, "agree", &session, epoch).await;

    // The receiver enables the plugin. The pusher's bytes have not moved by a
    // byte — but the claimed file's *stamp* has, so the diff must ask for it,
    // and must ask for nothing else.
    enable(&fixture, &["toy"]);
    let (session, epoch) = lease(&router, "agree").await;
    let needed = negotiate(&router, "agree", &session, epoch, &manifest(&claimed)).await;
    assert_eq!(
        needed,
        vec![PUSHED_PATH.to_string()],
        "the diff did not see the receiver's new opt-in"
    );
    upload(&router, "agree", &session, epoch, PUSHED_PATH, &claimed).await;
    commit(&router, "agree", &session, epoch).await;

    // The pass agreed with the diff: the file it asked for is the file that
    // re-chunked, through the plugin.
    let after = file_chunks(&fixture, PUSHED_PATH);
    assert!(
        after
            .iter()
            .all(|c| c.language.as_deref() == Some("toydata"))
    );
    assert_ne!(before.len(), after.len(), "the geometry should have moved");

    // And it has settled: the next identical push needs nothing again.
    let (session, epoch) = lease(&router, "agree").await;
    let settled = negotiate(&router, "agree", &session, epoch, &manifest(&claimed)).await;
    assert_eq!(
        settled,
        Vec::<String>::new(),
        "the pass and the diff disagree about what is now stored"
    );
}

/// A receiver that has the plugin installed but whose project never enabled it
/// advertises the installed set anyway, and chunks nothing through it.
///
/// The advertisement is deliberately machine-wide — a pusher is told what the
/// receiver *has*, not what this project uses — so this is the case where the
/// two halves differ and the pusher can see the gap it is being warned about.
#[tokio::test]
async fn a_lease_advertises_what_is_installed_even_where_nothing_is_enabled() {
    let fixture = Fixture::neutral("gap");
    let (_guard, _dir, registry) = plugins(&[("toy", TOY), ("other", OTHER)]);
    let ctx = fixture.context().with_plugins(registry.clone());
    let router = harness(&fixture, &ctx, registry.clone());

    let (status, body) = post(&router, "/v1/push/lease", json!({ "project": "gap" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let advertised: Vec<&str> = body["plugins"]
        .as_array()
        .expect("the installed set")
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(advertised, ["other", "toy"], "{body}");

    // Nothing is enabled, so nothing routes: the pusher sees the plugin and
    // gets the built-in chunking, which is exactly the observable mismatch the
    // contract asks for instead of a negotiation.
    let claimed = lines(10);
    push(&router, "gap", &claimed).await;
    assert!(
        file_chunks(&fixture, PUSHED_PATH)
            .iter()
            .all(|c| c.language.is_none())
    );
}

// ---------------------------------------------------------------------------
// The stamp lifecycle, out and back
// ---------------------------------------------------------------------------

/// Disabling a plugin is a full round trip: the file comes back to the chunks
/// core would have made, and the store holds exactly those rows — no chunk of
/// the plugin's geometry left behind under a stamp nobody will ever match.
///
/// An orphan here is invisible in every surface and permanent: the file's
/// stamp is current, so no pass re-visits it, and a stale row keeps answering
/// searches with text that is no longer chunked that way.
#[test]
fn disabling_a_plugin_puts_the_built_in_chunks_back_and_leaves_no_orphans() {
    let fixture = Fixture::neutral("roundtrip");
    fixture.write(".loreignore", ".*\n");
    fixture.write(PUSHED_PATH, lines(10));
    fixture.write(OTHER_PATH, OTHER_BODY);
    let (_guard, _dir, registry) = plugins(&[("toy", TOY)]);
    let ctx = fixture.context().with_plugins(registry.clone());

    full_scan(&ctx, &fixture.project);
    let before_chunks = file_chunks(&fixture, PUSHED_PATH);
    let before_stamps = stamps(&fixture);
    let before_total = fixture.chunk_count();
    assert!(before_chunks.iter().all(|c| c.language.is_none()));

    enable(&fixture, &["toy"]);
    full_scan(&ctx, &fixture.project);
    let during = file_chunks(&fixture, PUSHED_PATH);
    assert_ne!(
        during.len(),
        before_chunks.len(),
        "the plugin did not change the file's geometry, so this proves nothing"
    );
    // The plugin's rows replaced the built-in ones rather than joining them.
    assert_eq!(
        fixture.chunk_count(),
        before_total - before_chunks.len() as u64 + during.len() as u64
    );

    enable(&fixture, &[]);
    full_scan(&ctx, &fixture.project);
    assert_eq!(
        file_chunks(&fixture, PUSHED_PATH),
        before_chunks,
        "opting out did not restore the built-in chunking"
    );
    assert_eq!(stamps(&fixture), before_stamps);
    assert_eq!(
        fixture.chunk_count(),
        before_total,
        "a chunk of the plugin's geometry outlived the opt-in"
    );

    // A third pass changes nothing, which is what says the store and the stamp
    // agree about where they ended up.
    let settled = full_scan(&ctx, &fixture.project);
    assert_eq!(settled.indexed, 0, "{settled:?}");
}

/// Editing one plugin re-chunks its files and leaves every other plugin's
/// alone. Fingerprints are per plugin precisely so that one author's rebuild
/// is not a machine-wide re-index.
#[test]
fn editing_one_plugin_leaves_every_other_plugins_files_alone() {
    let fixture = Fixture::neutral("independent");
    fixture.write(".loreignore", ".*\n");
    fixture.write(PUSHED_PATH, lines(10));
    fixture.write("Assets/Level.otherdata", lines(10));
    fixture.write(OTHER_PATH, OTHER_BODY);
    let (_guard, dir, registry) = plugins(&[("toy", TOY), ("other", OTHER)]);
    let ctx = fixture.context().with_plugins(registry);
    enable(&fixture, &["toy", "other"]);
    full_scan(&ctx, &fixture.project);
    let before = stamps(&fixture);

    std::fs::write(
        dir.join("toy/lore-plugin.toml"),
        format!("{TOY}\n# an edit that changes nothing but the bytes\n"),
    )
    .unwrap();
    let (edited, diagnostics) = PluginRegistry::load(&dir);
    assert_eq!(diagnostics, vec![]);
    let ctx = fixture.context().with_plugins(Arc::new(edited));

    let pass = full_scan(&ctx, &fixture.project);
    assert_eq!(pass.indexed, 1, "one plugin's edit touched more: {pass:?}");
    for (path, stamp) in stamps(&fixture) {
        let was = &before
            .iter()
            .find(|(p, _)| *p == path)
            .expect("the same files are indexed")
            .1;
        match path.as_str() {
            PUSHED_PATH => assert_ne!(&stamp, was, "the edited plugin's file must re-chunk"),
            other => assert_eq!(&stamp, was, "{other} moved for another plugin's edit"),
        }
    }
}

/// A path that ends in an extension a plugin claims, spelled in the case a
/// Unity project actually writes it, survives the whole daemon round trip:
/// claimed, chunked, stamped, and unchanged on the next pass.
///
/// The three lowercasing sites are exercised separately in `plugin_edges.rs`;
/// what this adds is that they agree *through the store*, where a disagreement
/// presents as a file that re-indexes on every pass and never converges.
#[test]
fn an_uppercase_extension_converges_instead_of_re_chunking_forever() {
    let fixture = Fixture::neutral("shouty");
    fixture.write(".loreignore", ".*\n");
    fixture.write("Assets/LEVEL.TOYDATA", lines(10));
    let (_guard, _dir, registry) = plugins(&[("toy", TOY)]);
    let ctx = fixture.context().with_plugins(registry);
    enable(&fixture, &["toy"]);

    let first = full_scan(&ctx, &fixture.project);
    assert_eq!(first.indexed, 1, "{first:?}");
    let chunks = file_chunks(&fixture, "Assets/LEVEL.TOYDATA");
    assert!(
        chunks
            .iter()
            .all(|c| c.language.as_deref() == Some("toydata")),
        "the uppercase spelling did not route through the plugin"
    );

    let second = full_scan(&ctx, &fixture.project);
    assert_eq!(
        (second.indexed, second.unchanged),
        (0, 1),
        "the stamp and the route disagree: {second:?}"
    );
}

/// The receiver reads its opt-in from the registered root on every pass, so a
/// `.lore.toml` that appears between two pushes takes effect on the second one
/// without the daemon restarting or the pusher knowing.
#[tokio::test]
async fn the_receivers_opt_in_is_read_per_pass_not_per_daemon() {
    let fixture = Fixture::neutral("late");
    let (_guard, _dir, registry) = plugins(&[("toy", TOY)]);
    let ctx = fixture.context().with_plugins(registry.clone());
    let router = harness(&fixture, &ctx, registry);

    let claimed = lines(10);
    push(&router, "late", &claimed).await;
    assert!(
        file_chunks(&fixture, PUSHED_PATH)
            .iter()
            .all(|c| c.language.is_none())
    );

    // No restart, no reload: the file appears and the next pass honours it.
    enable(&fixture, &["toy"]);
    push(&router, "late", &claimed).await;
    assert!(
        file_chunks(&fixture, PUSHED_PATH)
            .iter()
            .all(|c| c.language.as_deref() == Some("toydata")),
        "the opt-in was cached past the pass that read it"
    );

    // And a `.lore.toml` naming a plugin nobody installed is still not an
    // error: the files fall back, the push succeeds, and `status` carries the
    // gap.
    fixture.write(
        lore::repo_config::REPO_CONFIG_FILE,
        "[plugins]\nenable = [\"unity\"]\n",
    );
    push(&router, "late", &claimed).await;
    assert!(
        file_chunks(&fixture, PUSHED_PATH)
            .iter()
            .all(|c| c.language.is_none())
    );
    let (status, body) = send(
        &router,
        Request::builder()
            .method("GET")
            .uri("/v1/status?project=late")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["projects"][0]["plugins_missing"], json!(["unity"]));
}
