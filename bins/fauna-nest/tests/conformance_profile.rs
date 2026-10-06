//! Integration round-trip for the `fauna.profile.*` surface — the per-user
//! profile *detail* read (`get`) and the owner's own-write (`set`)
//! (`docs/goal/ui/profile.md` § Where logic lives). Exercises the WS-RPC layer
//! end-to-end: request decode, the `content`-table read/write keyed on the
//! `actor_id`, reply encoding, and the `User | Admin` allowlist.
//!
//! `set` proves the publish path (ratified 2026-06-18): a client-signed
//! `EmbedAsBytes` profile is verified at ingest, the inner `actor_id` is
//! asserted to be the caller, the signed bytes are stored as the
//! `schema='profile'` content row, and superseded rows are pruned (keep-latest).
//! `get` then serves those exact bytes verbatim; the shared
//! `fauna_core::encoding::decode_profile` (also used by `activitypub::
//! actor_routes`) round-trips them back to a typed `Profile`.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/profile.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::data::{InboxMode, Profile, Timestamp};
use fauna_core::encoding::{decode_profile, sign_and_pack};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_nest::{db::CacheDb, profile_handlers, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    ByteBuf, RpcError, decode_strict as decode, encode_canonical,
    profile::{ProfileGetReply, ProfileGetRequest, ProfileSetReply, ProfileSetRequest},
};

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    // A real `BackupService` over a temp dir: `AppState::for_test` wires
    // `backup_service: None`, and the blob-backed handlers refuse without one.
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();
    std::mem::forget(dir); // outlive the test; the process is the harness
    let backup_svc = Arc::new(
        fauna_nest::backup::service::BackupService::new(db.clone(), None, false, dir_path, None)
            .unwrap(),
    );
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        ..AppState::for_test(db.clone())
    });
    let mut b = RpcRouter::builder();
    profile_handlers::register_profile_handlers(&mut b);
    (b.build(), state)
}

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

/// An [`RpcRequester`] over this file's `dispatch` — the seam that lets a real
/// shared-Rust *client* call surface run against the real handlers, so a test
/// can assert the whole production flow (client composes → nest stores → peer
/// decodes) rather than each end against a mock of the other.
///
/// Bound to one caller actor, exactly as a real connection is.
struct RouterRequester {
    router: RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
}

/// `RpcError` needs a `Display` + [`RpcErrorClass`] wrapper to satisfy
/// `RpcRequester::Error`. Every failure here IS a server rejection — there is
/// no transport to fault — so `as_rpc_error` always answers, which is what
/// lets a caller branch on the code the way it does over a real transport.
#[derive(Debug)]
struct RouterError(RpcError);

impl std::fmt::Display for RouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.code)
    }
}

impl fauna_protocol::RpcErrorClass for RouterError {
    fn is_rejection(&self) -> bool {
        true
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        Some(&self.0)
    }
}

impl fauna_protocol::RpcRequester for &RouterRequester {
    type Error = RouterError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = encode_canonical(&payload).expect("encode request");
        let reply = dispatch(
            &self.router,
            Arc::clone(&self.state),
            self.actor,
            kind,
            Bytes::from(bytes.to_vec()),
        )
        .await
        .map_err(RouterError)?;
        Ok(decode(&reply).expect("decode reply"))
    }
}

fn sample_profile(actor: [u8; 32]) -> Profile {
    profile_named(actor, "Alice", "building fauna")
}

fn profile_named(actor: [u8; 32], display_name: &str, bio: &str) -> Profile {
    Profile {
        actor_id: ActorId(actor),
        display_name: Some(display_name.into()),
        bio: Some(bio.into()),
        avatar: None,
        banner: None,
        links: vec![],
        nests: vec![],
        admin_nests: vec![],
        load_hint: None,
        inbox_mode: InboxMode::Open,
        recovery_head: None,
        updated_at: Timestamp(0),
    }
}

/// Encode a `ProfileSetRequest` carrying the signed `EmbedAsBytes` wire of
/// `profile`, signed by `kp` — exactly what a client builds via
/// `fauna-client-profile::build_profile`.
fn set_request(kp: &ActorKeypair, profile: &Profile) -> Bytes {
    let body = sign_and_pack(kp, profile).expect("sign+pack");
    let req = encode_canonical(&ProfileSetRequest {
        body: ByteBuf::from(body),
        extra: Default::default(),
    })
    .unwrap();
    Bytes::from(req.to_vec())
}

/// Count the author's `schema='profile'` rows — proves the keep-latest prune.
async fn profile_row_count(state: &Arc<AppState>, author: &[u8; 32]) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row(
        "SELECT COUNT(*) FROM content WHERE author = ?1 AND schema = 'profile'",
        rusqlite::params![author.as_slice()],
        |row| row.get(0),
    )
    .unwrap()
}

/// Seed the latest `schema = 'profile'` content row for `actor`, returning the
/// stored bytes (the signed embed-as-bytes wire — the canonical shape).
async fn seed_profile(state: &Arc<AppState>, kp: &ActorKeypair) -> Vec<u8> {
    let actor = kp.actor_id().0;
    let profile = sample_profile(actor);
    let payload = sign_and_pack(kp, &profile).expect("sign+pack profile");
    // The content `id` is irrelevant here — the read keys on `author + schema`
    // (`ORDER BY created_at DESC LIMIT 1`), not the id. An arbitrary distinct
    // value suffices for a single-row in-memory fixture.
    let id: [u8; 32] = [0xAB; 32];
    let conn = state.db.conn().await;
    fauna_nest::db::content::insert_content(
        &conn, &id, "profile", &actor, 1_000_000, &payload, None, "fauna", None,
    )
    .expect("seed profile content row");
    drop(conn);
    payload
}

#[tokio::test]
async fn profile_get_returns_stored_profile_bytes_verbatim() {
    let (router, state) = router_with_db().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    // The calling actor must be a known user for the permission gate.
    state.db.create_user(&actor, "free", "alice").await.unwrap();
    let stored = seed_profile(&state, &kp).await;

    let req = encode_canonical(&ProfileGetRequest {
        actor_id: hex::encode(actor),
        extra: Default::default(),
    })
    .unwrap();
    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.profile.get",
        Bytes::from(req.to_vec()),
    )
    .await
    .expect("profile.get ok");
    let reply: ProfileGetReply = decode(&reply_bytes).expect("decode reply");
    // Served verbatim: the stored bytes round-trip unchanged.
    assert_eq!(reply.body.as_ref(), stored.as_slice());
}

#[tokio::test]
async fn profile_get_not_found_for_actor_without_profile() {
    let (router, state) = router_with_db().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    state.db.create_user(&actor, "free", "bob").await.unwrap();
    // No profile row seeded.

    let req = encode_canonical(&ProfileGetRequest {
        actor_id: hex::encode(actor),
        extra: Default::default(),
    })
    .unwrap();
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.profile.get",
        Bytes::from(req.to_vec()),
    )
    .await
    .expect_err("missing profile is an error");
    assert_eq!(err.code, "fauna.profile.not_found");
}

#[tokio::test]
async fn profile_get_rejects_malformed_actor_id_hex() {
    let (router, state) = router_with_db().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    state.db.create_user(&actor, "free", "carol").await.unwrap();

    let req = encode_canonical(&ProfileGetRequest {
        actor_id: "not-hex".into(),
        extra: Default::default(),
    })
    .unwrap();
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.profile.get",
        Bytes::from(req.to_vec()),
    )
    .await
    .expect_err("bad hex is rejected");
    assert_eq!(err.code, "fauna.profile.invalid_request");
}

// ── fauna.profile.set ──────────────────────────────────────────

#[tokio::test]
async fn profile_set_then_get_round_trips() {
    let (router, state) = router_with_db().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    state.db.create_user(&actor, "free", "alice").await.unwrap();

    // Publish: the signed EmbedAsBytes wire (what build_profile produces).
    let profile = sample_profile(actor);
    let body = sign_and_pack(&kp, &profile).expect("sign+pack");
    let set_payload = set_request(&kp, &profile);
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.profile.set",
        set_payload,
    )
    .await
    .expect("profile.set ok");
    let _: ProfileSetReply = decode(&reply_bytes).expect("decode set reply");

    // Read back: served verbatim, and the shared decoder round-trips it.
    let get_req = encode_canonical(&ProfileGetRequest {
        actor_id: hex::encode(actor),
        extra: Default::default(),
    })
    .unwrap();
    let got = dispatch(
        &router,
        state,
        actor,
        "fauna.profile.get",
        Bytes::from(get_req.to_vec()),
    )
    .await
    .expect("profile.get ok");
    let reply: ProfileGetReply = decode(&got).expect("decode get reply");
    assert_eq!(reply.body.as_ref(), body.as_slice(), "stored verbatim");
    let (decoded, _) = decode_profile(reply.body.as_ref()).expect("shared decode");
    assert_eq!(decoded.display_name.as_deref(), Some("Alice"));
    assert_eq!(decoded.bio.as_deref(), Some("building fauna"));
}

#[tokio::test]
async fn profile_set_rejects_caller_actor_mismatch() {
    let (router, state) = router_with_db().await;
    // Caller is bob; the signed profile belongs to alice. Bob cannot publish
    // alice's profile even though the signature itself is valid.
    let alice = ActorKeypair::generate();
    let bob = ActorKeypair::generate();
    let bob_actor = bob.actor_id().0;
    state
        .db
        .create_user(&bob_actor, "free", "bob")
        .await
        .unwrap();

    let alice_profile = sample_profile(alice.actor_id().0);
    let payload = set_request(&alice, &alice_profile); // signed by alice
    let err = dispatch(
        &router,
        state,
        bob_actor, // dispatched as bob
        "fauna.profile.set",
        payload,
    )
    .await
    .expect_err("caller≠actor must be rejected");
    assert_eq!(err.code, "fauna.profile.permission_denied");
}

#[tokio::test]
async fn profile_set_rejects_unsigned_bare_profile() {
    let (router, state) = router_with_db().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    state.db.create_user(&actor, "free", "carol").await.unwrap();

    // A bare canonical Profile (no signature envelope) must NOT be accepted by
    // the write path — the nest verifies the signature at ingest.
    let bare = encode_canonical(&sample_profile(actor)).unwrap();
    let req = encode_canonical(&ProfileSetRequest {
        body: ByteBuf::from(bare.to_vec()),
        extra: Default::default(),
    })
    .unwrap();
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.profile.set",
        Bytes::from(req.to_vec()),
    )
    .await
    .expect_err("unsigned bare profile must be rejected");
    assert_eq!(err.code, "fauna.profile.invalid_request");
}

#[tokio::test]
async fn profile_set_prunes_superseded_keeps_latest() {
    let (router, state) = router_with_db().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    state.db.create_user(&actor, "free", "dave").await.unwrap();

    // First publish.
    let p1 = profile_named(actor, "Dave", "v1");
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.profile.set",
        set_request(&kp, &p1),
    )
    .await
    .expect("first set ok");
    assert_eq!(profile_row_count(&state, &actor).await, 1);

    // Second publish supersedes the first (different bytes ⇒ different id).
    let p2 = profile_named(actor, "Dave Updated", "v2");
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.profile.set",
        set_request(&kp, &p2),
    )
    .await
    .expect("second set ok");

    // Keep-latest prune: exactly one profile row remains, and get returns p2.
    assert_eq!(
        profile_row_count(&state, &actor).await,
        1,
        "superseded row must be pruned"
    );
    let get_req = encode_canonical(&ProfileGetRequest {
        actor_id: hex::encode(actor),
        extra: Default::default(),
    })
    .unwrap();
    let got = dispatch(
        &router,
        state,
        actor,
        "fauna.profile.get",
        Bytes::from(get_req.to_vec()),
    )
    .await
    .expect("get ok");
    let reply: ProfileGetReply = decode(&got).expect("decode reply");
    let (decoded, _) = decode_profile(reply.body.as_ref()).expect("decode profile");
    assert_eq!(decoded.display_name.as_deref(), Some("Dave Updated"));
}

/// The avatar/banner lifecycle across the **real** client builders and the
/// **real** nest handlers — set → text-only re-edit → clear.
///
/// This is the headless half of the deferred image-upload increment
/// (`profile.md` § Where logic lives → *Field ownership*): everything except
/// the file picker and the rendered `<img>`. The client crate's own unit tests
/// cover `ProfileImageEdit` in isolation; this proves the part that only a
/// cross-binary test can — that a profile carrying an `avatar` survives the
/// nest's ingest-verify + caller-match + store + keep-latest prune, and comes
/// back off `fauna.profile.get` with the reference intact.
///
/// The middle step is the one that would silently break every app: a
/// *text-only* save (the shipped v1 form, still calling the unchanged
/// `build_edited_profile`) must not drop a picture set by an earlier save.
#[tokio::test]
async fn avatar_survives_a_text_only_edit_and_can_be_cleared() {
    use fauna_client_profile::{
        ProfileImageEdit, build_edited_profile, build_edited_profile_with_images,
    };

    let (router, state) = router_with_db().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    state.db.create_user(&actor, "free", "eve").await.unwrap();

    let avatar_hex = "ab".repeat(32);
    let expected = fauna_core::data::ContentHash::from_digest_raw([0xAB; 32]);

    /// Publish `body` and read the stored profile back through the nest.
    async fn publish_and_read(
        router: &RpcRouter,
        state: &Arc<AppState>,
        actor: [u8; 32],
        body: Vec<u8>,
    ) -> Profile {
        dispatch(
            router,
            state.clone(),
            actor,
            "fauna.profile.set",
            Bytes::from(
                encode_canonical(&ProfileSetRequest {
                    body: ByteBuf::from(body),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("profile.set ok");

        let get_req = encode_canonical(&ProfileGetRequest {
            actor_id: hex::encode(actor),
            extra: Default::default(),
        })
        .unwrap();
        let got = dispatch(
            router,
            state.clone(),
            actor,
            "fauna.profile.get",
            Bytes::from(get_req.to_vec()),
        )
        .await
        .expect("profile.get ok");
        let reply: ProfileGetReply = decode(&got).expect("decode get reply");
        decode_profile(reply.body.as_ref())
            .expect("shared decode")
            .0
    }

    // 1. First publish carries a picture (the first-publish branch — no base).
    let first = build_edited_profile_with_images(
        &kp,
        None,
        &[],
        Some("Eve".into()),
        None,
        vec![],
        ProfileImageEdit::set_from_hex(&avatar_hex).expect("valid hex"),
        ProfileImageEdit::Keep,
    )
    .expect("build first");
    let stored = publish_and_read(&router, &state, actor, first).await;
    assert_eq!(
        stored.avatar,
        Some(expected),
        "the nest must store and serve back the avatar reference"
    );
    assert_eq!(stored.banner, None, "Keep on a fresh profile stays None");

    // 2. A TEXT-ONLY re-edit — the shipped v1 form's exact call — must preserve
    //    the picture through a real round-trip, not just in the unit test.
    let base = sign_and_pack(&kp, &stored).expect("re-pack stored as the edit base");
    let text_only = build_edited_profile(
        &kp,
        Some(&base),
        &[],
        Some("Eve Updated".into()),
        None,
        vec![],
    )
    .expect("build text-only edit");
    let stored = publish_and_read(&router, &state, actor, text_only).await;
    assert_eq!(stored.display_name.as_deref(), Some("Eve Updated"));
    assert_eq!(
        stored.avatar,
        Some(expected),
        "a text-only save must NOT drop the avatar"
    );

    // 3. Clearing removes it — the remove affordance's half of the mechanism.
    let base = sign_and_pack(&kp, &stored).expect("re-pack");
    let cleared = build_edited_profile_with_images(
        &kp,
        Some(&base),
        &[],
        Some("Eve Updated".into()),
        None,
        vec![],
        ProfileImageEdit::Clear,
        ProfileImageEdit::Keep,
    )
    .expect("build cleared");
    let stored = publish_and_read(&router, &state, actor, cleared).await;
    assert_eq!(stored.avatar, None, "Clear removes the avatar");

    // The keep-latest prune held across all three publishes.
    assert_eq!(profile_row_count(&state, &actor).await, 1);
}

/// The step-2 success criterion of identity-succession slice 2
/// (`identity-succession.md:36`): a peer that cached the actor's signed
/// `Profile` verifies an `IdentitySuccession` from the profile's
/// `recovery_head` field alone — with no `fauna.recovery.registration.chain`
/// fetch. Structurally pinned: this router registers ONLY the profile
/// handlers, so a chain call is not even dispatchable here. Since the
/// close the mirror is the COUPLED head, so the cached binding also
/// drives the seq-advance check — no consumer state exists in which that
/// check is silently off.
#[tokio::test]
async fn a_succession_verifies_from_the_cached_profile_without_a_chain_fetch() {
    use fauna_core::recovery::{ChainHead, IdentitySuccession, RecoveryKey};

    let (router, state) = router_with_db().await;
    let owner = ActorKeypair::generate();
    let actor = owner.actor_id().0;
    state.db.create_user(&actor, "free", "alice").await.unwrap();

    // The owner registers a RecoveryKey and mirrors the chain head into the
    // profile (what the client's kit-creation leg will do — a first
    // registration lands at seq 1).
    let recovery = RecoveryKey::generate();
    let mut profile = sample_profile(actor);
    profile.recovery_head = Some(ChainHead::new(recovery.public(), 1));
    dispatch(
        &router,
        Arc::clone(&state),
        actor,
        "fauna.profile.set",
        set_request(&owner, &profile),
    )
    .await
    .expect("profile.set ok");

    // A peer fetches and caches the profile.
    let peer = ActorKeypair::generate();
    let peer_id = peer.actor_id().0;
    state.db.create_user(&peer_id, "free", "bob").await.unwrap();
    let get_req = encode_canonical(&ProfileGetRequest {
        actor_id: hex::encode(actor),
        extra: Default::default(),
    })
    .unwrap();
    let got = dispatch(
        &router,
        Arc::clone(&state),
        peer_id,
        "fauna.profile.get",
        Bytes::from(get_req.to_vec()),
    )
    .await
    .expect("profile.get ok");
    let reply: ProfileGetReply = decode(&got).expect("decode reply");
    let (cached, _) = decode_profile(reply.body.as_ref()).expect("decode profile");
    let cached_head = cached
        .recovery_head
        .expect("the profile carries the RecoveryKey binding");

    // Later, a succession statement arrives. The peer holds the WHOLE head
    // from the cached profile, so the seq-advance check runs — a valid
    // succession must advance past the head the peer cached.
    let successor = ActorKeypair::generate();
    let statement = IdentitySuccession {
        old_actor_id: owner.actor_id(),
        new_actor_id: successor.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 2,
        created_at: Timestamp(1_753_000_000),
    };
    let signed = statement
        .sign(&recovery, successor.signing_key(), None)
        .expect("sign succession");
    signed
        .verify(&cached_head)
        .expect("the cached profile field alone verifies the succession");

    // The seq half of the binding is load-bearing: a statement that does NOT
    // advance past the cached head — the archived-statement replay — is
    // refused on the cache alone.
    let stale = IdentitySuccession {
        seq: 1,
        ..statement.clone()
    }
    .sign(&recovery, successor.signing_key(), None)
    .expect("sign stale");
    stale
        .verify(&cached_head)
        .expect_err("a statement at the cached head's own seq must be refused");

    // And the pubkey half is load-bearing: a statement authorized by a
    // DIFFERENT key is refused against the cached binding.
    let thief_recovery = RecoveryKey::generate();
    let forged = IdentitySuccession {
        recovery_pubkey: thief_recovery.public(),
        ..statement
    }
    .sign(&thief_recovery, successor.signing_key(), None)
    .expect("sign forged");
    forged
        .verify(&cached_head)
        .expect_err("a foreign RecoveryKey must not verify against the cached binding");
}

/// The mirror's **producer** half, end to end through the real handlers
/// (landed 2026-08-02). The test above hand-built the cached binding and
/// noted it was "what the client's kit-creation leg will do"; that leg now
/// exists as `fauna_client_profile::publish_recovery_head`, so the flow is
/// asserted whole: the shared client writer composes get→set, the real nest
/// verifies and stores the signed bytes, and a peer's `get` decodes the
/// binding — no step mocked at either end.
///
/// Both arms of the writer run here, because both happen in production and
/// only one of them is obvious:
///
/// 1. **No profile published yet** — the ordinary FIRST mirror, since the
///    onboarding kit screen precedes every profile edit. `fauna.profile.get`
///    answers `fauna.profile.not_found`, which the writer treats as "no base
///    document" and mints a minimal profile carrying the binding.
/// 2. **A profile already published** — the retrofit / replacement case. The
///    binding is added and every authored field survives, which is the
///    property that keeps a kit ceremony from being a back door that blanks
///    the user's identity.
#[tokio::test]
async fn the_shared_writer_mirrors_the_chain_head_through_the_real_handlers() {
    use fauna_client_profile::publish_recovery_head;
    use fauna_core::recovery::{ChainHead, RecoveryKey};

    let (router, state) = router_with_db().await;
    let owner = ActorKeypair::generate();
    let actor = owner.actor_id().0;
    state.db.create_user(&actor, "free", "alice").await.unwrap();
    let nest = RouterRequester {
        router,
        state: Arc::clone(&state),
        actor,
    };

    // ── Arm 1: the account has never published a profile ───────────────────
    let first = RecoveryKey::generate();
    publish_recovery_head(&nest, &owner, &[], ChainHead::new(first.public(), 1), None)
        .await
        .expect("a never-published profile is the ordinary first-mirror case, not an error");

    let stored = read_profile(&nest, actor).await;
    assert_eq!(
        stored.recovery_head,
        Some(ChainHead::new(first.public(), 1)),
        "the minted profile carries the binding a peer verifies successions from"
    );
    assert_eq!(stored.actor_id.0, actor);

    // ── Arm 2: the user then authors a profile, and REPLACES the kit ───────
    // The edit path is the one every app already uses; it must preserve the
    // binding, and the next mirror must preserve the edit.
    let edited = fauna_client_profile::build_edited_profile(
        &owner,
        Some(&encode_stored(&nest, actor).await),
        &[],
        Some("Ada".into()),
        Some("counts things".into()),
        vec![],
    )
    .expect("build edited");
    dispatch(
        &nest.router,
        Arc::clone(&state),
        actor,
        "fauna.profile.set",
        Bytes::from(
            encode_canonical(&ProfileSetRequest {
                body: ByteBuf::from(edited),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("profile.set ok");

    let after_edit = read_profile(&nest, actor).await;
    assert_eq!(
        after_edit.recovery_head,
        Some(ChainHead::new(first.public(), 1)),
        "an ordinary profile edit must not retire the user's recovery binding"
    );

    let replacement = RecoveryKey::generate();
    publish_recovery_head(
        &nest,
        &owner,
        &[],
        ChainHead::new(replacement.public(), 2),
        None,
    )
    .await
    .expect("mirror the replacement head");

    let stored = read_profile(&nest, actor).await;
    assert_eq!(
        stored.recovery_head,
        Some(ChainHead::new(replacement.public(), 2)),
        "a replacement moves the head to seq+1, and the cache must follow it — \
         a peer left on the old binding would refuse the succession the new kit \
         authorizes"
    );
    assert_eq!(
        stored.display_name.as_deref(),
        Some("Ada"),
        "mirroring a binding is not a licence to blank what the user authored"
    );
    assert_eq!(stored.bio.as_deref(), Some("counts things"));

    // The keep-latest prune held across every publish above.
    assert_eq!(profile_row_count(&state, &actor).await, 1);
}

/// `fauna.profile.get` for `actor`, decoded — the peer's half of the flow.
async fn read_profile(nest: &RouterRequester, actor: [u8; 32]) -> Profile {
    let (profile, _) = decode_profile(&encode_stored(nest, actor).await).expect("decode profile");
    profile
}

/// The raw stored bytes `fauna.profile.get` serves for `actor`.
async fn encode_stored(nest: &RouterRequester, actor: [u8; 32]) -> Vec<u8> {
    let got = dispatch(
        &nest.router,
        Arc::clone(&nest.state),
        actor,
        "fauna.profile.get",
        Bytes::from(
            encode_canonical(&ProfileGetRequest {
                actor_id: hex::encode(actor),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("profile.get ok");
    let reply: ProfileGetReply = decode(&got).expect("decode reply");
    reply.body.to_vec()
}

/// **The member-path anchor, end to end across the binary boundary.** A member
/// harvests the peer who added them — the real `fauna.profile.get` handler
/// serving a profile the real client writer published, through the real
/// harvest door into the member's anchor store.
///
/// Every existing pin of `harvest_peer_anchor`
/// (`libs/fauna-client-recovery/tests/witness.rs`) serves the profile from a
/// stub nest and stores into an in-memory map. That is the right shape for the
/// harvest's four trust rules — the envelope, not the transport, carries the
/// trust — but it means the harvest has never once run against the handlers it
/// runs against in production. The store half is the account plane's
/// (`fauna.state.peer-anchors`), a client-side store with no nest leg: its
/// production door is pinned in `fauna-sync-engine`'s `peer_leg_convergence`
/// tests, so here it is the in-memory double of the same join. Since this anchor is what
/// decides whether a Welcome-joined member renders continuity or "a stranger
/// joined" (`identity-succession.md` § The succession statement → *the
/// peer-profile harvest*), a gap between stub and real here is invisible
/// everywhere else and fatal to the whole member path.
///
/// Both halves are production code and neither is re-implemented: alice's head
/// reaches her profile through `publish_recovery_head`, and bob reads it back
/// through `harvest_peer_anchor` over his own authenticated requester — the
/// same "the member's own connection" reach the sweep has (a same-nest peer
/// today; cross-nest is the declared follow-on).
#[tokio::test]
async fn a_member_harvests_the_peer_anchor_through_the_real_handlers() {
    use fauna_client_recovery::harvest::{HarvestOutcome, harvest_peer_anchor};
    use fauna_core::recovery::{ChainHead, RecoveryKey};

    let (router, state) = router_with_db().await;
    let router = Arc::new(router);

    let alice_kp = ActorKeypair::from_secret([31u8; 32]);
    let alice = alice_kp.actor_id().0;
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    let alice_nest = SharedRouter {
        router: Arc::clone(&router),
        state: Arc::clone(&state),
        actor: alice,
    };

    let bob_kp = ActorKeypair::from_secret([32u8; 32]);
    let bob = bob_kp.actor_id().0;
    state.db.create_user(&bob, "free", "bob").await.unwrap();
    let bob_nest = SharedRouter {
        router,
        state: Arc::clone(&state),
        actor: bob,
    };

    // Alice registers a kit: the mirror lands her chain head in her profile.
    let kit = RecoveryKey::generate();
    let head = ChainHead::new(kit.public(), 1);
    // `None` home: this arm asserts the HEAD mirror + harvest, not the
    // fill-if-absent home-nests entry added (that behaviour has its
    // own red-first, mutation-graded pins in `fauna-client-profile`).
    fauna_client_profile::publish_recovery_head(alice_nest, &alice_kp, &[], head, None)
        .await
        .expect("the head mirror is the harvest's producer");

    // Bob's anchor store — empty, which is the state every real member is in
    // the first time they harvest anyone.
    let bob_store = fauna_conversations::backend::MemoryPeerAnchorStore::default();

    let outcome = harvest_peer_anchor(&bob_nest, &alice_kp.actor_id(), &bob_store).await;
    assert!(
        matches!(outcome, HarvestOutcome::Seeded(seed) if seed.seeded_head),
        "bob must seed alice's chain head from her own signed profile — without \
         it a Welcome-joined member holds no anchor and every succession \
         statement degrades to the bare add. got {outcome:?}"
    );

    // And it is READABLE BACK, which is the half the witness actually consults.
    assert_eq!(
        bob_store.current().known_chain_head(&alice_kp.actor_id()),
        Some(head),
        "the harvested head must rest in the anchor store — it is exactly what \
         `ChainWitness` tier 1 reads to settle a statement offline"
    );
}

/// A cloneable requester over one shared in-process router, so the same nest
/// answers both the publishing writer and the borrowing harvest.
/// `RouterRequester` above holds its router by value and cannot be shared.
#[derive(Clone)]
struct SharedRouter {
    router: Arc<RpcRouter>,
    state: Arc<AppState>,
    actor: [u8; 32],
}

impl fauna_protocol::RpcRequester for SharedRouter {
    type Error = RouterError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = encode_canonical(&payload).expect("encode request");
        let reply = dispatch(
            &self.router,
            Arc::clone(&self.state),
            self.actor,
            kind,
            Bytes::from(bytes.to_vec()),
        )
        .await
        .map_err(RouterError)?;
        Ok(decode(&reply).expect("decode reply"))
    }
}

// `&SharedRouter` needs no impl of its own: `fauna_protocol` blankets
// `impl<T: RpcRequester> RpcRequester for &T`, so the borrow the harvest takes
// routes through the owning impl above. (`RouterRequester` predates that and
// implements the reference form directly, which is why the blanket does not
// collide with it.)
