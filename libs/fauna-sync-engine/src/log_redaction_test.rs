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
//!    That list knows only the old spellings, so
//!    [`no_log_macro_carries_an_unwrapped_name_or_path_value`] is the
//!    spelling-independent half: it reddens on any bare name/path-shaped value
//!    in any log macro body (shorthand, `{x}`, `%x`, `.display()`).
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
use fauna_core::log_redact_scan::{leaked_words, macro_bodies};
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

/// The re-seed's delivery legs moved to `fauna-client-sync` (2026-10-18) so
/// web runs them; the engine still drives them natively, so their error and
/// log strings stay under this crate's scan. Each is named by its crate and
/// file in the allowlist below.
const MOVED_LEG_FILES: &[&str] = &["reseed_pull.rs", "reseed_leg.rs"];

/// Every file the scans read: this crate's own `src/`, then the moved legs.
fn scanned_files(src_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    rust_files(src_dir, &mut files);
    let moved = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fauna-client-sync/src");
    files.extend(MOVED_LEG_FILES.iter().map(|f| moved.join(f)));
    files
}

/// A scanned file's allowlist name: its path under this crate's `src/`, or
/// `fauna-client-sync/<file>` for a moved leg.
fn scanned_name(path: &Path, src_dir: &Path) -> String {
    match path.strip_prefix(src_dir) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => format!(
            "fauna-client-sync/{}",
            path.file_name().unwrap().to_string_lossy()
        ),
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
    let files = scanned_files(&src_dir);
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

const LOG_MACROS: &[&str] = &["trace!(", "debug!(", "info!(", "warn!(", "error!("];

/// The error-string constructors: an error message is a confidentiality
/// boundary exactly like a log line (S7 says "log line and error string"), and
/// the host apps log `{e:#}`. A `format!(` is scanned only as the argument of
/// one of these (a bare `format!` builds file names, URLs and prefixes too).
const ERROR_MACROS: &[&str] = &[
    "anyhow!(",
    "bail!(",
    "ensure!(",
    "context(",
    "with_context(",
];

/// Every log-macro body in `src`, with its 1-based line.
fn log_macro_bodies(src: &str) -> Vec<(usize, String)> {
    macro_bodies(src, LOG_MACROS)
}

/// The spelling-independent pin: no `tracing` macro in this crate carries a
/// folder-name/path-shaped value that has not been through a `log_*` wrapper.
/// Unlike the shape list above it does not know the bug's spelling — a new
/// shorthand field, `{folder}` interpolation, `%req.folder` or
/// `path = %p.display()` all redden it.
#[test]
fn no_log_macro_carries_an_unwrapped_name_or_path_value() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = scanned_files(&src_dir);

    let mut offenders = Vec::new();
    for path in &files {
        let contents = std::fs::read_to_string(path).unwrap();
        for (line, body) in log_macro_bodies(&contents) {
            let leaked = leaked_words(&body);
            if !leaked.is_empty() {
                offenders.push(format!("{}:{line}: {leaked:?}", path.display()));
            }
        }
    }
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "a log macro carries a bare folder-name/path-shaped value — wrap it in \
         `fauna_core::log_redact::{{log_folder_name,log_path}}` (or, for a value that is not \
         user-chosen, rename it so it is not name-shaped):\n{}",
        offenders.join("\n")
    );
}

/// The cleared classes of name/path-shaped words inside an error constructor:
/// `(file under src/, a substring of the macro body, why it is not a
/// user-chosen name or path)`. Every entry is one shape in one file — never a
/// blanket per-file skip — and the test below also reddens on an entry that no
/// longer matches anything, so the list cannot rot.
const ERROR_STRING_ALLOWLIST: &[(&str, &str, &str)] = &[
    // custodian_store.rs: internal storage roots keyed by `actor_id_hex` or a fixed
    // subdirectory name — never a user-chosen name or folder-relative path.
    (
        "custodian_store.rs",
        "read custodian index {}",
        "internal store root path",
    ),
    (
        "custodian_store.rs",
        "custodian index {} was written",
        "internal store root path",
    ),
    (
        "custodian_store.rs",
        "parse custodian index {}",
        "internal store root path",
    ),
    (
        "custodian_store.rs",
        "read custodian manifest {}",
        "internal store root path",
    ),
    (
        "custodian_store.rs",
        "read staged chunk {}",
        "internal store root path",
    ),
    (
        "custodian_store.rs",
        "scan {}",
        "internal store root directory",
    ),
    (
        "custodian_store.rs",
        "reclaim {}",
        "internal store root entry",
    ),
    // custodian_store.rs: `{path}` is a held address — a set-qualified reserved name plus hashes.
    (
        "custodian_store.rs",
        "holds no live generation at {path}",
        "held address, reserved name plus hashes",
    ),
    (
        "custodian_store.rs",
        "custodian row {path}",
        "held address, reserved name plus hashes",
    ),
    (
        "custodian_store.rs",
        "{path}: ",
        "held address, reserved name plus hashes",
    ),
    // reseed.rs: `{path}` is a held address (reserved name plus hashes), not a folder-relative path.
    (
        "reseed.rs",
        "sealing {path} under",
        "held address, reserved name plus hashes",
    ),
    (
        "reseed.rs",
        "for {kind} path {path}",
        "held address, reserved name plus hashes",
    ),
    (
        "reseed.rs",
        "retiring {path} in this",
        "held address, reserved name plus hashes",
    ),
    // The pull-back's `path`/`g.path` is the same held address.
    (
        "fauna-client-sync/reseed_pull.rs",
        "{set}/{path} names",
        "held address, reserved name plus hashes",
    ),
    (
        "fauna-client-sync/reseed_pull.rs",
        "landing {}'s bytes",
        "held address, reserved name plus hashes",
    ),
    (
        "fauna-client-sync/reseed_pull.rs",
        "recording {} on the target",
        "held address, reserved name plus hashes",
    ),
    (
        "fauna-client-sync/reseed_pull.rs",
        "fetching {path}'s manifest",
        "held address, reserved name plus hashes",
    ),
    (
        "fauna-client-sync/reseed_pull.rs",
        "for {path} failed hash",
        "held address, reserved name plus hashes",
    ),
    (
        "fauna-client-sync/reseed_pull.rs",
        "fetching {path}'s chunks",
        "held address, reserved name plus hashes",
    ),
    (
        "fauna-client-sync/reseed_pull.rs",
        "of {path}'s {} chunks",
        "held address, reserved name plus hashes",
    ),
    (
        "fauna-client-sync/reseed_pull.rs",
        "chunk {} of {path} failed",
        "held address, reserved name plus hashes",
    ),
];

/// The error-string half of the spelling-independent pin: no `anyhow!` /
/// `bail!` / `ensure!` / `context` / `with_context` body carries a
/// folder-name/path-shaped value that has not been through a `log_*` wrapper,
/// except the named cleared classes in [`ERROR_STRING_ALLOWLIST`].
#[test]
fn no_error_string_carries_an_unwrapped_name_or_path_value() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = scanned_files(&src_dir);

    let mut offenders = Vec::new();
    let mut used = vec![false; ERROR_STRING_ALLOWLIST.len()];
    for path in &files {
        let rel = scanned_name(path, &src_dir);
        let contents = std::fs::read_to_string(path).unwrap();
        for (line, body) in macro_bodies(&contents, ERROR_MACROS) {
            let leaked = leaked_words(&body);
            if leaked.is_empty() {
                continue;
            }
            let cleared = ERROR_STRING_ALLOWLIST
                .iter()
                .position(|(file, shape, _)| rel == *file && body.contains(shape));
            match cleared {
                Some(i) => used[i] = true,
                None => offenders.push(format!("{rel}:{line}: {leaked:?}: {}", body.trim())),
            }
        }
    }
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "an error constructor carries a bare folder-name/path-shaped value — wrap it in \
         `fauna_core::log_redact::{{log_folder_name,log_path}}`, or add a `file:shape` entry with a \
         reason to ERROR_STRING_ALLOWLIST if it is not user-chosen:\n{}",
        offenders.join("\n")
    );
    let stale: Vec<_> = ERROR_STRING_ALLOWLIST
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|((file, shape, _), _)| format!("{file}: {shape:?}"))
        .collect();
    assert!(
        stale.is_empty(),
        "ERROR_STRING_ALLOWLIST entries that match nothing — delete them:\n{}",
        stale.join("\n")
    );
}

/// The error-macro scanner itself: each constructor spelling is found, and a
/// wrapped value passes.
#[test]
fn the_scanner_sees_every_error_constructor() {
    for bad in [
        r#"fn f() { anyhow::bail!("x {folder}"); }"#,
        r#"fn f() { return Err(anyhow!("not a path: {rel:?}")); }"#,
        r#"fn f() { ensure!(ok, "bad {name}"); }"#,
        r#"fn f() { r.with_context(|| format!("listing {}", dir.display()))?; }"#,
        r#"fn f() { r.context(format!("removing {path}"))?; }"#,
    ] {
        let flagged = macro_bodies(bad, ERROR_MACROS)
            .iter()
            .any(|(_, b)| !leaked_words(b).is_empty());
        assert!(flagged, "{bad} must be flagged");
    }
    let good = r#"fn f() { bail!("{} is excluded", log_path(rel)); r.context("plain")?; }"#;
    assert!(
        macro_bodies(good, ERROR_MACROS)
            .iter()
            .all(|(_, b)| leaked_words(b).is_empty()),
        "wrapped values must pass"
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
