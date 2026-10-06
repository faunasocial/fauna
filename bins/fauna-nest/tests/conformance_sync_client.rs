//! Cross-binary round-trip for the **device-sync control plane** as the engine
//! and `fauna-sync` daemon now drive it: the real shared `fauna-client-sync`
//! `SyncClient` adapter (`fauna.sync.{register,changes.{list,record}}`) against
//! the real nest `sync_handlers` over a real in-memory `CacheDb`. This is the
//! proof that the adapter and nest agree — the engine's exact call sequence
//! (register a device → record a change → another device catches up via
//! `changes.list`, the recording device excluding its own echo) survives the
//! WS-RPC transport contract end to end.
//!
//! `conformance_sync.rs` already proves the handlers in isolation (direct
//! dispatch); this proves the *client adapter* composes the right kinds and
//! payloads against those handlers, closing the loop the engine relies on after
//! its migration off the `/api/v1/sync/*` HTTP twins (S3b). The byte
//! routes (`/chunks`, `/manifests`) stay HTTP and are out of
//! scope here.
//!
//! The WS transport seam is replaced by a direct router dispatch (the same seam
//! `conformance_caldav_client.rs` uses): `RouterRequester` implements the
//! adapter's `RpcRequester` by encoding the payload, invoking the registered
//! handler with a fixed connection actor, and decoding the reply.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks; the
//! only stand-in is the in-process dispatch for the WebSocket).

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_client_sync::{RecordSigning, SetNonceSource, SyncClient};
use fauna_core::path_crypto::{LabelField, LabelRoot, seal_convergent};
use fauna_nest::{
    db::CacheDb, folder_handlers, routes::AppState, rpc_router::RpcRouter, sync_handlers,
};
use fauna_protocol::{
    RpcError, RpcErrorClass, RpcRequester, decode_strict, encode_canonical,
    folders::{FolderCreateReply, FolderCreateRequest},
};

/// A regular user actor — not a bridge service user, not admin. A real
/// keypair: every change record is writer-signed, and a synthetic id has no
/// key that could sign for it.
fn actor_kp() -> fauna_core::identity::ActorKeypair {
    common::signing_actor(11)
}

/// [`actor_kp`]'s actor id — the connection actor every dispatch runs as.
fn actor() -> [u8; 32] {
    actor_kp().actor_id().0
}

/// Seal `path` under an owner label root — the minimum real S9 shape.
///
/// Since the S9 flip (v32, 2026-08-02) this nest rests no plaintext paths: a
/// `changes.record` without `path_sealed` is refused
/// (`fauna.sync.path_seal_required`), so this adapter conformance seals like
/// the engine does. The full render/custody round-trip is
/// `conformance_path_sealing.rs`'s job — here the seal only needs to be real
/// enough for the funnel, and the assertion is that it echoes back verbatim
/// on `changes.list`.
fn owner_seal(path: &str) -> Vec<u8> {
    let owner_key = fauna_core::crypto::BackupKey::from_bytes([0xc4; 32]);
    seal_convergent(
        &LabelRoot::owner_of(&owner_key),
        &fauna_core::sync::path_hash(path),
        LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

/// The adapter's `RpcRequester`, with the WebSocket transport replaced by a
/// direct dispatch into the registered handler keyed on `actor`. Encodes the
/// request, runs the handler, decodes the reply — the exact wire contract the
/// live WS path drives, minus the socket.
struct RouterRequester {
    router: Arc<RpcRouter>,
    state: Arc<AppState>,
    actor: [u8; 32],
}

/// The fake's error, drawing the same two classes as [`RpcErrorClass`].
///
/// The in-process dispatch *is* nest here, so a handler `Err` is a genuine
/// server rejection — it reached nest and was refused — and keeps the wire
/// [`RpcError`] it returned; an encode / decode / unregistered-kind failure is
/// the local analogue of a transport fault, which never reached a handler.
///
/// This replaced a bare `String`, which cannot classify itself and — being a
/// foreign type, like the trait — cannot be given the impl here either (orphan
/// rule). `SyncClient`'s `changes_record` / `changes_supersede` gained an
/// `R::Error: RpcErrorClass` bound, so the fake has to answer the question.
#[derive(Debug)]
enum RequesterError {
    /// Reached the handler and was refused, carrying the wire error.
    Rejected(RpcError),
    /// Never reached the handler.
    Transport(String),
}

impl std::fmt::Display for RequesterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(e) => write!(f, "rejected: {e:?}"),
            Self::Transport(m) => write!(f, "transport: {m}"),
        }
    }
}

impl RpcErrorClass for RequesterError {
    fn is_rejection(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }

    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            Self::Rejected(e) => Some(e),
            Self::Transport(_) => None,
        }
    }
}

impl RpcRequester for RouterRequester {
    type Error = RequesterError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        common::seed_dispatch_actor(&self.state.db, &self.actor).await;
        let bytes = Bytes::from(
            encode_canonical(&payload)
                .map_err(|e| RequesterError::Transport(e.to_string()))?
                .to_vec(),
        );
        let meta = self
            .router
            .kind_meta(kind)
            .ok_or_else(|| RequesterError::Transport(format!("kind not registered: {kind}")))?;
        let reply = (meta.handler)(self.state.clone(), self.actor, bytes)
            .await
            .map_err(RequesterError::Rejected)?;
        decode_strict(&reply).map_err(|e| RequesterError::Transport(e.to_string()))
    }
}

/// Build a router with the folder + sync handlers and a fresh in-memory db,
/// returning the shared router/state plus the `fauna-client-sync` adapter the
/// engine consumes — signing every record directly with the actor's identity
/// key under the set's nonce, as an engine bound to one set does
/// (`SetNonceSource::Fixed`).
fn harness() -> (Arc<RpcRouter>, Arc<AppState>, SyncClient<RouterRequester>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    let router = Arc::new(b.build());
    let client = SyncClient::new(RouterRequester {
        router: router.clone(),
        state: state.clone(),
        actor: actor(),
    })
    .with_record_signing(RecordSigning {
        signer: common::direct_signer(&actor_kp()),
        set_nonce: SetNonceSource::Fixed(common::SET_NONCE),
    });
    (router, state, client)
}

/// Create a `sync`-mode folder named `name` for [`actor`], under the fixture
/// set nonce the client signs with (the precondition
/// `changes.{record,list}` scope against — driven by direct dispatch, since the
/// folder surface is a sibling crate, not `fauna-client-sync`).
async fn create_set(router: &RpcRouter, state: &Arc<AppState>, name: &str) {
    common::seed_dispatch_actor(&state.db, &actor()).await;
    let bytes = Bytes::from(
        encode_canonical(&FolderCreateRequest {
            name: name.into(),
            retention_policy: None,
            set_nonce: common::set_nonce_field(),
            ..Default::default()
        })
        .unwrap()
        .to_vec(),
    );
    let meta = router
        .kind_meta("fauna.folders.create")
        .expect("registered");
    let reply = (meta.handler)(state.clone(), actor(), bytes)
        .await
        .expect("create ok");
    let _: FolderCreateReply = decode_strict(&reply).unwrap();
}

#[tokio::test]
async fn engine_control_plane_round_trips_via_adapter() {
    let (router, state, client) = harness();
    create_set(&router, &state, "docs").await;

    let dev = "a1".repeat(32); // 64 hex chars = 32 bytes
    let manifest = "c3".repeat(32);

    // register (daemon startup / in-process per-set registration)
    let reg = client
        .register(dev.clone(), "laptop", None) // label_sealed — this adapter test holds no seal root
        .await
        .expect("register");
    assert_eq!(reg.device_id, dev, "register echoes the stored device id");

    // record a change (engine upload path) — sealed, as the S9 flip requires
    let seal = owner_seal("notes/todo.md");
    let rec = client
        .changes_record(
            "docs",
            dev.clone(),
            "notes/todo.md",
            Some(manifest.clone()),
            4096,
            "Created",
            None, // content_key_version — owner-only/plain set
            None, // thumbnail_hash — no producer yet
            Some(seal.clone()),
            None, // derived_through
            None, // is_resolution
        )
        .await
        .expect("record");
    assert!(rec.seq > 0, "record returns an assigned seq");

    // catch-up poll (engine pull path), no device exclusion → sees the change
    let list = client
        .changes_list(Some("docs".into()), None, 0)
        .await
        .expect("list");
    assert_eq!(list.changes.len(), 1);
    let c = &list.changes[0];
    assert_eq!(c.seq, rec.seq);
    // Post-S9-flip, a sealed set rests no plaintext path: the seal is the
    // row's only label and must round-trip verbatim (rendering it is
    // `conformance_path_sealing.rs`'s province).
    assert_eq!(c.path_sealed.as_deref().map(|b| &b[..]), Some(&seal[..]));
    assert_eq!(c.manifest_hash.as_deref(), Some(manifest.as_str()));
    assert_eq!(c.device_id.as_deref(), Some(dev.as_str()));
    assert_eq!(c.size_bytes, 4096);

    // the recording device excludes its own echo (engine self-echo filter)
    let excluded = client
        .changes_list(Some("docs".into()), Some(dev.clone()), 0)
        .await
        .expect("list excl");
    assert!(excluded.changes.is_empty(), "own device's changes excluded");

    // a read naming no folder is refused: there is no actor-wide feed
    assert!(
        client.changes_list(None, None, 0).await.is_err(),
        "a folder-less list is refused"
    );
}

/// The M2 re-seal reclaim leg (Piece B): the engine's exact sequence — record
/// the pre-bind version, re-record the re-sealed head, then
/// `changes_supersede` naming the verified head — marks the old row and hides
/// it from the catch-up feed, end to end through the real adapter + handler.
#[tokio::test]
async fn reseal_supersede_round_trips_via_adapter() {
    let (router, state, client) = harness();
    create_set(&router, &state, "shared").await;

    let dev = "b2".repeat(32);
    client
        .register(dev.clone(), "owner dev", None) // label_sealed — as above
        .await
        .expect("register");

    let (m_prebind, m_resealed) = ("d1".repeat(32), "d2".repeat(32));
    for (mh, ckv) in [(&m_prebind, None), (&m_resealed, Some(1))] {
        client
            .changes_record(
                "shared",
                dev.clone(),
                "prebind.bin",
                Some(mh.clone()),
                150_000,
                "modify",
                ckv, // the re-seal stamps the current generation
                None,
                Some(owner_seal("prebind.bin")), // S9: sealed like the engine
                None,
                None,
            )
            .await
            .expect("record");
    }

    // The owner verified the re-sealed head end-to-end, then supersedes.
    let reply = client
        .changes_supersede("shared", dev.clone(), "prebind.bin", m_resealed.clone())
        .await
        .expect("supersede");
    assert_eq!(reply.superseded, 1, "the pre-bind row is marked");

    // A catching-up member/device sees only the re-sealed head — it can never
    // attempt a fetch of the reclaimable pre-bind manifest.
    let list = client
        .changes_list(Some("shared".into()), None, 0)
        .await
        .expect("list");
    assert_eq!(list.changes.len(), 1);
    assert_eq!(
        list.changes[0].manifest_hash.as_deref(),
        Some(m_resealed.as_str())
    );
    assert_eq!(list.changes[0].content_key_version, Some(1));

    // Naming a stale (non-head) manifest refuses with the typed error.
    let err = client
        .changes_supersede("shared", dev, "prebind.bin", m_prebind)
        .await
        .expect_err("stale manifest refused");
    // Asserted on the wire `code` the handler returned — now reachable through
    // `RpcErrorClass::as_rpc_error`, which is the point of the bound. Matches
    // how `conformance_sync.rs` pins the same refusal.
    assert_eq!(
        err.as_rpc_error().map(|e| e.code.as_str()),
        Some("fauna.sync.supersede_head_mismatch"),
        "typed head-mismatch surfaces through the adapter: {err}"
    );
}
