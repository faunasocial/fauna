//! S7 (the log + error-string scrub, `docs/goal/behavior/file-sync.md`
//! § Sealed names & paths). `fauna.admin.logs` hands the nest's process-wide
//! `fauna_log` ring to any allowlisted admin — the same confidentiality
//! boundary as an unsealed DB column. This test drives a real production log
//! site end to end (a real warning, through the real `tracing` ring, out
//! through the real `fauna.admin.logs` RPC handler) and proves a user-chosen
//! folder name reaches the admin's reply only in redacted, hash-prefix
//! form — never as plaintext.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::admin_actor;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{admin_ws_handlers, db::CacheDb, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::admin::{AdminLogsReply, AdminLogsRequest};
use fauna_protocol::{RpcError, decode_strict as decode};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    (b.build(), state)
}

/// Serializes the `clear() → drive → read` window of the two tests below.
///
/// Both drive the **process-global** `fauna_log` ring, and that is the point:
/// the property under test is what an admin actually receives over
/// `fauna.admin.logs`, so a per-test ring would prove nothing. The consequence
/// is that `fauna_log::clear()` is a shared mutation — run the two tests
/// concurrently and the sibling's `clear()` can land between this test's driven
/// call and its read, wiping the very entry it is about to assert on.
///
/// Measured 2026-08-23 under a 31-binary parallel run:
/// `unowned_folder_warning_…` failed reporting the ring held only
/// `folder=name~5f4f38401d78` — the hash of `"my-website"`, the *sibling* test's
/// folder. Green 5/5 at the same `--test-threads=4` on a quiet box, which is
/// what makes it the worst kind of red: it appears only under the load these
/// machines actually run at.
///
/// E2E convention 10 ("delete a machine-global-state dependency, never
/// serialize on it") does not reach this: the shared state is not the machine's,
/// it is the production singleton these tests exist to exercise, and deleting
/// the dependency would delete the test. Same shape and same construction as
/// `libs/fauna-client-dns/tests/pebble_dns01.rs`'s `PEBBLE_SERIAL` — a
/// `tokio::sync::Mutex`, not a `std` one, because the guard is held across the
/// driven `.await` (`clippy::await_holding_lock`).
static LOG_RING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

/// A driven site. A website set's change record
/// naming a server-side-executable path is REJECTED before it ever enters
/// `web_files` — so the path never becomes a public URL and the "web-type
/// paths" carve-out does not cover it. The rejection warning must reach an
/// admin only in redacted form; the matched extension is a fixed public
/// constant and stays literal.
///
/// Driven through `fauna.sync.changes.record`, signed as a production writer
/// signs it — the signed record rail is the one that reaches the `web_files`
/// fan-out.
#[tokio::test]
async fn rejected_web_file_warning_reaches_the_admin_log_only_in_redacted_form() {
    use tracing_subscriber::prelude::*;

    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;

    let mut b = RpcRouter::builder();
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    let sync_router = b.build();

    let kp = common::signing_actor(3);
    let actor = kp.actor_id().0;
    let device = [4u8; 32];
    state
        .db
        .register_sync_device(&actor, &device, "laptop", None, "write")
        .await
        .unwrap();
    state
        .db
        .create_folder_with_options(
            "my-website",
            &actor,
            fauna_nest::db::FolderOptions {
                set_nonce: Some(common::SET_NONCE.to_vec()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Phase 4: the `web_files` fan-out (and with it the rejection under test)
    // keys on the website toggle, not `mode`.
    state
        .db
        .update_folder_for_user(
            "my-website",
            &actor,
            fauna_nest::db::FolderUpdate {
                website_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // The reviewer's own motivating example: a filename the admin would
    // otherwise never see, refused before publication.
    let secret_path = "admin-pages/client-list.php";
    let manifest_hash_hex = "0".repeat(64);

    let _ring = LOG_RING.lock().await;
    fauna_log::clear();
    let sub = tracing_subscriber::registry().with(fauna_log::RingLayer);
    let guard = tracing::subscriber::set_default(sub);

    let record = common::signed_record(
        fauna_protocol::sync::SyncChangeRecordRequest {
            folder: "my-website".to_string(),
            device_id: hex::encode(device),
            path: secret_path.to_string(),
            manifest_hash: Some(manifest_hash_hex),
            size_bytes: 0,
            change_type: "create".to_string(),
            path_sealed: Some(fauna_protocol::ByteBuf::from(
                b"e2e-synthetic-seal".to_vec(),
            )),
            ..Default::default()
        },
        &kp,
    );
    common::dispatch(
        &sync_router,
        state.clone(),
        actor,
        "fauna.sync.changes.record",
        encode(&record),
    )
    .await
    .expect("the signed record itself lands; only its web_files fan-out is refused");

    drop(guard);

    let bytes = dispatch(
        &router,
        state,
        admin,
        "fauna.admin.logs",
        encode(&AdminLogsRequest::default()),
    )
    .await
    .expect("logs ok");
    let reply: AdminLogsReply = decode(&bytes).unwrap();

    let messages: Vec<&str> = reply.entries.iter().map(|e| e.message.as_str()).collect();
    assert!(
        !messages
            .iter()
            .any(|m| m.contains("client-list") || m.contains("admin-pages")),
        "the rejected web file's plaintext path leaked into the admin log ring: {messages:?}"
    );

    let redacted = fauna_core::log_redact::log_path(secret_path);
    let rejection: Vec<&&str> = messages
        .iter()
        .filter(|m| m.contains("server-side extension"))
        .collect();
    assert!(
        !rejection.is_empty(),
        "the rejection warning itself must be in the ring (otherwise this test \
         drives nothing): {messages:?}"
    );
    assert!(
        rejection.iter().any(|m| m.contains(&redacted)),
        "expected the redacted form {redacted:?} on the rejection warning, got {rejection:?}"
    );
    assert!(
        rejection.iter().any(|m| m.contains(".php")),
        "the matched extension is a fixed public constant and should stay \
         literal for diagnosability: {rejection:?}"
    );
}
