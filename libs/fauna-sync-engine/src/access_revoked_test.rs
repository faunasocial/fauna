//! Engine-level pins for the **terminal `access-revoked` park**.
//!
//! Two properties, each of which failed differently before this landed:
//!
//! 1. **A grant refusal parks the set** — and does so identically on both
//!    planes, because a demotion folds differently on each (cross-nest
//!    `federation.forbidden` vs. the same-nest ST-RES-1 `sync.not_found`).
//!    Previously `record_change`'s only reaction to *any* failure was
//!    `tracing::warn!` at the `upload_file` call site, so a demoted writer
//!    re-recorded the same doomed change on every watcher event and every rescan
//!    tick, forever, while the UI went on claiming the folder was syncing.
//!
//! 2. **A parked engine refuses locally** — it does not re-ask the nest. This is
//!    what makes the park *terminal* rather than a very persistent retry. These
//!    engines point at a dead port, so the proof is the *shape* of the error: a
//!    parked record names the park, where one that reached the wire could only
//!    report a transport failure.
//!
//! Neither test touches the filesystem beyond the temp watch dir: D4's other
//! half — *local files and pending local edits are never touched* — is a
//! property of what the park **doesn't** do, and the engine's disk writes all
//! live behind the record it no longer performs.

use fauna_protocol::RpcError;

use crate::pull_remote_changes_test::test_engine;

/// The two authoritative refusals, one per plane. Both must park.
#[tokio::test]
async fn a_grant_refusal_parks_the_engine_on_either_plane() {
    for code in ["fauna.federation.forbidden", "fauna.sync.not_found"] {
        let tmp = tempfile::tempdir().unwrap();
        let engine = test_engine(tmp.path().to_path_buf());
        assert!(!engine.access_gate().is_revoked(), "a fresh engine is live");

        let refusal: fauna_client::NestClientError = RpcError::new(code, "error.test").into();
        engine.note_access_refusal("shared", &refusal);

        assert!(
            engine.access_gate().is_revoked(),
            "{code} is the owning nest's answer that this actor may not write: it must park"
        );
    }
}

/// The discriminating half: a refusal that does *not* assert "your grant is
/// gone" must leave the engine live. `peer_nest_outdated` is the sharp case —
/// it arrives over the same cross-nest relay as a real refusal, and parking on
/// it would strand a fully-granted writer on nothing worse than a peer running
/// an older nest.
#[tokio::test]
async fn a_version_gap_or_transport_fault_does_not_park_the_engine() {
    let cases: Vec<fauna_client::NestClientError> = vec![
        RpcError::new("fauna.federation.peer_nest_outdated", "error.test").into(),
        RpcError::new("fauna.sync.device_unregistered", "error.test").into(),
        RpcError::new("fauna.protocol.internal", "error.test").into(),
        fauna_client::NestClientError::RpcTimeout,
    ];
    for refusal in cases {
        let tmp = tempfile::tempdir().unwrap();
        let engine = test_engine(tmp.path().to_path_buf());
        engine.note_access_refusal("shared", &refusal);
        assert!(
            !engine.access_gate().is_revoked(),
            "{refusal} must stay retryable, not park the set"
        );
    }
}

/// Terminal means terminal: once parked, a record is refused **locally**.
///
/// The *message* is the discriminator — delete the `is_revoked()` guard from
/// `record_change` and this engine (pointed at a dead port) still fails, but
/// with a transport error instead of the park, so the assertion below goes red.
/// The timeout is the belt-and-braces half: it also bounds the case where the
/// removed guard lets the call sit on the RPC deadline instead of erroring
/// promptly, so the test cannot hang the suite either way.
#[tokio::test]
async fn a_parked_engine_refuses_a_record_without_asking_the_nest() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine(tmp.path().to_path_buf());
    engine.access_gate().revoke();

    let refused = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        engine.record_change(
            "shared",
            "notes.txt",
            Some("deadbeef"),
            7,
            "create",
            None,
            None,
            crate::causal::CausalStamp::unknown(),
        ),
    )
    .await
    .expect("a parked engine answers locally — it must not reach the wire at all")
    .expect_err("a parked set records nothing");

    let msg = refused.to_string();
    assert!(
        msg.contains("parked") && msg.contains("revoked"),
        "the refusal must name the park so a log reader can act on it, got: {msg}"
    );
}
