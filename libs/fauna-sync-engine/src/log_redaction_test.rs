//! — the S7 log + error-string scrub
//! (`docs/goal/behavior/path-sealing.md` § Sealed names & paths) applied to
//! this crate's engine: `fauna_core::log_redact::{log_path, log_folder_name}`
//! must sit between every user-chosen folder name / folder-relative path and
//! every `tracing` line or error string in `fauna-sync-engine` — a log line
//! is a confidentiality boundary exactly like the wire and the DB
//! (`libs/fauna-core/src/log_redact.rs`).
//!
//! Two guards, mirroring `bins/fauna-nest/tests/conformance_log_redaction.rs`'s
//! own two-tier proof (source-level pins plus one end-to-end drive):
//!
//! 1. [`no_plaintext_folder_or_path_shape_reaches_a_tracing_call`] — a
//!    source-level scan of every production `.rs` file in this crate for the
//!    exact unredacted shapes the sweep found and fixed. It is a
//!    regression net for *every* site the sweep touched (the pull's two
//!    lines, the revoked-folder park line, and the ~80 further sites across
//!    `engine.rs`, `always_resident.rs`, `share_pump.rs`, `share_glue.rs` and
//!    `watcher.rs`), not just the two driven end to end below.
//! 2. [`pull_reaches_the_ring_with_only_the_folder_name_redacted`] — proven
//!    end to end, not just at the source level: a real `SyncEngine::pull_remote_changes`
//!    call, over a real (mocked-socket) `NestClient`/supervisor, with its
//!    `tracing` output captured through the real `fauna_log` ring.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fauna_client::testing::{MockClientChannel, connect_queue, mpsc_pair};
use fauna_client::{AuthClient, NestClient, PushBroker};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::sync::SyncChangesListReply;
use fauna_protocol::{Frame, Reply, RpcError, decode_frame, encode_frame};
use fauna_ws_substrate::{Supervisor, run_supervisor};

use crate::pull_remote_changes_test::test_engine_with_nest_client;

/// The folder name under test — deliberately readable, so a plaintext leak
/// into the ring is unmistakable at a glance and not confusable with a hash.
const SECRET_FOLDER: &str = "My Secret Tax Documents 2026";

// ─────────────────────────────────────────────────────────────────────
// Guard 1: source-level scan
// ─────────────────────────────────────────────────────────────────────

/// Exact substrings the sweep found unredacted and fixed. Each is
/// specific enough (a field name plus its raw, un-wrapped value expression)
/// that it cannot appear by coincidence in unrelated code — confirmed by this
/// same scan finding zero hits anywhere in the crate, tests included, right
/// after every real site was fixed.
const BANNED_SHAPES: &[&str] = &[
    // The pull's two lines, pre-fix (`engine.rs`).
    "folder = folder.unwrap_or(",
    // The park line and every `always_resident.rs` / `share_pump.rs` /
    // `share_glue.rs` field-shorthand site, pre-fix.
    "folder = %folder,",
    "folder = %spec.folder",
    // `engine.rs`'s ~48 `path` tracing-field sites, pre-fix.
    "path = relative_path,",
    "path = path,",
    // `share_pump.rs`'s spool sites, pre-fix.
    "%entry.context",
];

/// Recursively collect every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && path.file_name().and_then(|n| n.to_str()) != Some("log_redaction_test.rs")
        {
            // This file itself holds the banned shapes as string literals (the
            // scan's own pattern list, and this exclusion's own file name) —
            // scanning it would self-match on those literals, not on a real
            // regression.
            out.push(path);
        }
    }
}

/// A source-level pin over the WHOLE crate, not just the two sites driven end
/// to end below: every `.rs` file under `src/` (production and test code
/// alike — the banned shapes are specific enough that no test fixture should
/// ever legitimately need one either) must be free of every raw, unredacted
/// shape the sweep fixed. Mutate any one fix back to its pre-fix form
/// and this reds.
#[test]
fn no_plaintext_folder_or_path_shape_reaches_a_tracing_call() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src_dir, &mut files);
    assert!(
        files.len() > 50,
        "the walk found suspiciously few files ({}) under {src_dir:?} — the scan is \
         probably looking in the wrong place, not proving anything",
        files.len()
    );

    let mut offenders = Vec::new();
    for path in &files {
        let contents = std::fs::read_to_string(path).unwrap();
        for shape in BANNED_SHAPES {
            if contents.contains(shape) {
                offenders.push(format!("{}: {shape:?}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "an unredacted folder-name/path shape fixed has come back:\n{}",
        offenders.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────
// Guard 2: end-to-end drive of the pull's two lines
// ─────────────────────────────────────────────────────────────────────

/// Answers every request the pull needs with a genuine (empty) reply —
/// this test's subject is the log line the SUCCESS path writes, not the nest
/// double's fidelity, so every kind not needed for that succeeds trivially.
async fn serve(mut server: fauna_client::testing::ServerSide) {
    while let Some(bytes) = server.rx_from_client.recv().await {
        let Frame::Request(req) = decode_frame(&bytes).expect("decodable frame") else {
            continue;
        };
        let payload = match req.kind.as_str() {
            "fauna.sync.changes.list" => {
                let bytes = fauna_core::encoding::canonical_encode(&SyncChangesListReply {
                    changes: vec![],
                    ..Default::default()
                })
                .unwrap();
                fauna_core::encoding::canonical_decode(&bytes).unwrap()
            }
            _ => fauna_core::encoding::canonical_decode(
                &fauna_core::encoding::canonical_encode(&RpcError::new(
                    "fauna.test.unexpected_kind",
                    "error.test.unexpected_kind",
                ))
                .unwrap(),
            )
            .unwrap(),
        };
        let reply = Frame::Reply(Reply {
            ty: Reply::TYPE,
            correlation_id: req.correlation_id,
            payload,
            ok: req.kind == "fauna.sync.changes.list",
        });
        if server
            .tx_to_client
            .send(encode_frame(&reply).unwrap())
            .await
            .is_err()
        {
            break;
        }
    }
}

/// The real production pull (`SyncEngine::fetch_changes`, driven through
/// `pull_remote_changes`) over a real `NestClient` + `Supervisor`, with only
/// the socket mocked — same shape as `connected_arm_heal_test.rs`'s harness,
/// minus the mode-resolution dance this test does not need. Its `tracing`
/// output is captured through the real process-wide `fauna_log` ring, exactly
/// as an admin would see it over `fauna.admin.logs` on the nest (this crate
/// has no such RPC surface of its own to dispatch through, so the ring is
/// read directly via `fauna_log::snapshot()`).
#[tokio::test]
async fn pull_reaches_the_ring_with_only_the_folder_name_redacted() {
    use tracing_subscriber::prelude::*;

    let watch_dir = tempfile::tempdir().unwrap();
    let auth = Arc::new(AuthClient::new(
        "http://127.0.0.1:0".into(),
        ActorKeypair::from_secret([9u8; 32]),
    ));
    let client = NestClient::with_auth(Arc::clone(&auth));
    let (slot, state_tx) = client.supervisor_channels_for_test();
    let (adapter, server) = mpsc_pair();
    let server_task = tokio::spawn(serve(server));
    let channel = Arc::new(MockClientChannel::new(
        connect_queue([Ok(adapter)]),
        Arc::clone(&auth),
        PushBroker::new(16),
    ));
    let supervisor = tokio::spawn(async move {
        let _ = run_supervisor(Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: state_tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        })
        .await;
    });

    let engine =
        test_engine_with_nest_client(watch_dir.path().to_path_buf(), SECRET_FOLDER, client);

    fauna_log::clear();
    let sub = tracing_subscriber::registry().with(fauna_log::RingLayer);
    let guard = tracing::subscriber::set_default(sub);

    let outcome = tokio::time::timeout(Duration::from_secs(10), engine.pull_remote_changes())
        .await
        .expect("pull_remote_changes must not hang against a connected, serving double")
        .expect("pull_remote_changes");

    drop(guard);
    supervisor.abort();
    server_task.abort();

    assert_eq!(outcome, 0, "the double served an empty change set");

    let messages: Vec<String> = fauna_log::snapshot()
        .into_iter()
        .map(|e| e.message)
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("fauna.sync.changes: pull")),
        "the pull's own log line must be in the ring (otherwise this test drives \
         nothing): {messages:?}"
    );
    assert!(
        !messages.iter().any(|m| m.contains(SECRET_FOLDER)),
        "the plaintext folder name leaked into the log ring: {messages:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.contains("Secret") || m.contains("Tax")),
        "a fragment of the plaintext folder name leaked into the log ring: {messages:?}"
    );

    let redacted = fauna_core::log_redact::log_folder_name(SECRET_FOLDER);
    assert!(
        messages.iter().any(|m| m.contains(&redacted)),
        "expected the redacted form {redacted:?} in the log ring, got {messages:?}"
    );
}
