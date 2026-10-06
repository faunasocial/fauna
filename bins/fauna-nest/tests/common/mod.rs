//! Shared fixtures for the `fauna-nest` integration tests.
//!
//! Each file under `tests/` is its own crate, so a helper used by one file is
//! dead code in the next — the module-wide allow below is what lets this module
//! be `mod common;`-ed into a test that needs only one of its helpers.
#![allow(dead_code)]

pub mod encrypted_keyblob;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ed25519_dalek::{Signer, SigningKey};
use fauna_client::NestClient;
use fauna_client::types::ConnectionState;
use fauna_conversations::Rail;
use fauna_conversations::address::TypedAddress;
use fauna_conversations::capabilities::derive_capabilities;
use fauna_conversations::compose::ComposeState;
use fauna_conversations::snapshot::ThreadDetail;
use fauna_conversations::thread::{ThreadFlavor, ThreadId};
use fauna_core::data::{Capability, ContentHash, Timestamp};
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::recovery::{IdentitySuccession, RecoveryKey, RecoveryKeyRegistration};
use fauna_mail::segments::MailRecordEnvelope as SegmentMailEnvelope;
use fauna_mls::wrapped_blob::format::TlsCertBundle;
use fauna_mls::wrapped_blob::{
    MailRecordEnvelope as BridgeMailEnvelope, derive_recipient_hpke_keypair, seal_to_recipient,
};
use fauna_nest::acme::{MultiDomainCertResolver, ServedCertSpki};
use fauna_nest::blob_store::{BlobStoreBackend, DiskBlobStore};
use fauna_nest::config::{NestConfig, NestSection, NodeMode, SubmissionPolicy, SubmissionSection};
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::auth::{HandshakeRequest, handshake_signed_message};
use fauna_protocol::bridge_routing::{
    AuthVerdicts, DkimVerdict, DmarcVerdict, IngestInboundMailReply, IngestInboundMailRequest,
    PublicMailMetadata, SpfVerdict,
};
use fauna_protocol::email::{InboxFetchReply, InboxFetchRequest};
use fauna_protocol::recovery::{
    RegistrationSubmitReply, RegistrationSubmitRequest, SuccessionSubmitReply,
    SuccessionSubmitRequest,
};
use fauna_protocol::subscriptions::{
    ApproveRequestReply, ApproveRequestRequest, TierCreateReply, TierCreateRequest,
};
use fauna_protocol::{
    Frame, Reply, RpcError, decode_frame, decode_strict as decode, encode_canonical,
};
use futures_util::StreamExt;
use serde_bytes::ByteBuf;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

/// One non-default record of each kind in the delegable preference cluster
/// (muted words and the hidden-content list, sync prefs, trained topics, task
/// delegations), as the preference pages store them: the kind, and its
/// sub-record in canonical dag-cbor, built through the mutations the pages
/// share (`fauna_client_config::preference_records`).
pub fn preference_cluster() -> Vec<(&'static str, Vec<u8>)> {
    use fauna_client_config::preference_records;
    use fauna_core::data::{
        DelegationConfig, ModerationConfig, ParticipantRef, PersonalizationConfig, SyncPrefsConfig,
    };
    use fauna_protocol::merge_policy::{
        KIND_DELEGATION, KIND_MODERATION, KIND_PERSONALIZATION, KIND_SYNC_PREFS,
    };

    let mut moderation = ModerationConfig::default();
    preference_records::set_muted_keywords(&mut moderation, vec!["witness".into()]);
    preference_records::hide_reported_content(&mut moderation, "post-1");
    let mut sync_prefs = SyncPrefsConfig::default();
    preference_records::set_default_conflict_policy(&mut sync_prefs, Some("latest_wins_always"));
    let mut personalization = PersonalizationConfig::default();
    preference_records::add_trained_factor(&mut personalization, "birds")
        .expect("an empty registry takes a factor");
    let mut delegation = DelegationConfig::default();
    delegation.set_pin(
        "index",
        Some(ParticipantRef::Device {
            device_id: "0a".repeat(32),
        }),
    );
    vec![
        (KIND_MODERATION, canonical_encode(&moderation).unwrap()),
        (KIND_SYNC_PREFS, canonical_encode(&sync_prefs).unwrap()),
        (
            KIND_PERSONALIZATION,
            canonical_encode(&personalization).unwrap(),
        ),
        (KIND_DELEGATION, canonical_encode(&delegation).unwrap()),
    ]
}

/// An ATProto senior rotation key — key material that exists nowhere but its
/// `fauna.state.atproto-identity` row.
pub fn senior_rotation_key() -> fauna_core::data::AtprotoRotationKey {
    fauna_core::data::AtprotoRotationKey {
        secret_scalar: fauna_core::secret::SecretArray32::new([0x5e; 32]),
        pubkey_did_key: "did:key:zDnaeSeniorRotationKey".into(),
        created_at: 1_700_000_000,
        published_for_dids: vec!["did:plc:ewvi7nxzyoun6zhxrhs64oiz".into()],
    }
}

/// A subscription tier's period keys — key material that exists nowhere but
/// its `fauna.state.subscriptions` rows: the nest holds no period key, and a
/// subscriber holds only the blob-wrapped copy of the periods it was granted.
pub fn held_period_key() -> fauna_core::data::SubscriptionsConfig {
    fauna_core::data::SubscriptionsConfig {
        tiers: vec![fauna_core::data::TierPeriodKeys {
            tier_name: "gold".into(),
            current: fauna_core::data::TierPeriod {
                version: 2,
                key: [0x9e; 32].into(),
                rotated_at: 1_700_000_000_000_002,
                minted_by: None,
            },
            prior: vec![fauna_core::data::TierPeriod {
                version: 1,
                key: [0x9d; 32].into(),
                rotated_at: 1_700_000_000_000_001,
                minted_by: None,
            }],
        }],
        pending_removals: Vec::new(),
    }
}

/// An account's shared-folder content-key custody: an owned set rotated once
/// by a member removal (both generations held), that removal's settled
/// staging, and a foreign set this account is a member of. A distributed
/// content key cannot be regenerated, so these `fauna.state.folder-keys` rows
/// are the only copy.
pub fn folder_key_custody() -> fauna_core::data::FoldersConfig {
    use fauna_core::data::{FolderKeyCustody, FoldersConfig, ForeignFolder};
    use fauna_core::folder_keys::{ContentKeyGeneration, FolderContentKeys};
    let rotated = ContentKeyGeneration {
        version: 2,
        key: [0xF2; 32].into(),
        rotated_at: 2_000,
    };
    FoldersConfig {
        sets: vec![FolderKeyCustody {
            channel_id: Some([0xC1; 32]),
            keys: Some(
                FolderContentKeys::genesis([0xF1; 32], 1_000).merge(&FolderContentKeys {
                    current: rotated,
                    prior: vec![],
                }),
            ),
            set_nonce: Some([0x01; 32]),
            name: Some("photos".into()),
            created_at: 900,
            ..Default::default()
        }],
        pending_removals: vec![],
        foreign_sets: vec![ForeignFolder {
            channel_id: [0xF0; 32],
            mls_group_id: vec![0xF0; 16],
            home_nest_url: "https://home.example".into(),
            home_nest_actor_id: None,
            set_name: Some("their-docs".into()),
            access: Some("reader".into()),
            content_key_floor: None,
            ..Default::default()
        }],
    }
}

/// A root-signed `RenewBearer` device grant over a **freshly generated**
/// device keypair: `(signed grant wire, device signing seed)`, with no expiry.
///
/// A fixture for the nest's device-grant doors — a device key the test holds
/// the private half of, so it can drive `fauna.auth.device_handshake` against
/// the grant it registered. Production mints no such grant: the machine's one
/// renewal credential is the store principal, whose grant is over the store's
/// writer key (`fauna_client_sync::build_principal_grant`;
/// `sync-agent-credentials.md` § Credential model → the RULED 2026-09-28
/// block). The nest's doors treat any device key alike, which is what these
/// tests pin.
pub fn fresh_device_grant(
    identity: &ActorKeypair,
) -> (fauna_core::encoding::EmbedAsBytes, [u8; 32]) {
    let device = ActorKeypair::generate();
    let auth = fauna_core::data::DeviceAuthorization {
        actor_id: identity.actor_id(),
        device_key: device.actor_id().0,
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp::now(),
        expires_at: None,
    };
    let (bytes, env) = fauna_core::encoding::sign_envelope(identity, &auth).expect("sign grant");
    (
        fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
        device.signing_key().to_bytes(),
    )
}

/// dag-cbor-encode a request payload into the `Bytes` a handler takes.
///
/// **Fifty-two** integration test files had each hand-copied this one-liner, in
/// four bodies that differed only in `Serialize` vs `serde::Serialize`, the
/// parameter's name, and `.unwrap()` vs `.expect("encode req")`. The count is
/// the finding: fifty-two copies is not "these functions are similar", it is
/// "this contract has no owner".
///
/// The contract worth owning is *which encoder*. `encode_canonical` is the
/// wire's own dag-cbor encoding — the same bytes a real client puts on the
/// socket — so a fixture built through it exercises the byte shape the handler
/// will actually meet. A file that reached for `serde_ipld_dagcbor::to_vec`
/// instead would still compile, still pass, and be testing a shape production
/// never produces. One owner makes the canonical encoder the path of least
/// resistance for the next test file.
pub fn encode<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).expect("encode canonical").to_vec())
}

// ── Writer-signed change records ─────────────────────────────────────────────
//
// Every record kind refuses an unsigned record `signature_required`, so a
// fixture that records a change signs it exactly as a writer engine does: the shared
// statement over the request, under the set's stored nonce, by a real key. A
// fixture set is created under [`SET_NONCE`]; a fixture actor that records is
// a real keypair ([`signing_actor`]), never a synthetic `[n; 32]` id, which no
// key can sign for.

/// The set nonce every fixture set is created under (`FolderCreateRequest::
/// set_nonce` / `FolderOptions::set_nonce`) and every fixture record is signed
/// under.
pub const SET_NONCE: [u8; 32] = [0x5e; 32];

/// [`SET_NONCE`] as the wire field.
pub fn set_nonce_field() -> Option<ByteBuf> {
    Some(ByteBuf::from(SET_NONCE.to_vec()))
}

/// A real keypair standing in for a test actor: deterministic per `tag`, so a
/// test can name "the same actor" twice. Its actor id is the key's public half.
pub fn signing_actor(tag: u8) -> ActorKeypair {
    ActorKeypair::from_secret([tag; 32])
}

/// Sign `req` directly with `actor`'s identity key (`signer_key == actor_id`)
/// under `nonce` — the record a writer engine sends. Sign LAST: every covered
/// field must already hold its final value.
pub fn sign_record(
    req: &mut fauna_protocol::sync::SyncChangeRecordRequest,
    actor: &ActorKeypair,
    nonce: [u8; 32],
) {
    fauna_protocol::sync_writer_sig::ChangeSigner::direct(actor)
        .sign_record(req, nonce)
        .expect("the fixture record signs");
}

/// [`sign_record`] by value, under [`SET_NONCE`].
pub fn signed_record(
    mut req: fauna_protocol::sync::SyncChangeRecordRequest,
    actor: &ActorKeypair,
) -> fauna_protocol::sync::SyncChangeRecordRequest {
    sign_record(&mut req, actor, SET_NONCE);
    req
}

/// A direct signer for an engine or client fixture (`SyncEngine::
/// set_change_signer`, `RecordSigning`).
pub fn direct_signer(actor: &ActorKeypair) -> Arc<fauna_protocol::sync_writer_sig::ChangeSigner> {
    Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(actor))
}

/// Assert that a locally-delivered message is byte-for-byte what the sender
/// submitted, once the nest's own delivery stamps are removed — and that the
/// stamping actually happened.
///
/// A local delivery is **never** the sender's bytes verbatim. Every deliverable
/// outcome passes through `bridge_routing_handlers::resolve_local_recipient`,
/// which folds the three spam-threshold tiers into one number and prepends it as
/// `X-Fauna-Spam-Threshold` (`mail-aliases.md` § Spam-threshold override, ruled
/// 2026-08-17) — deliberately in that one wrapper "so that a future arm cannot
/// ship an unstamped delivery", sub-address suffix and role-address routes
/// included. Three CalDAV tests still asserted raw equality against the
/// submitted bytes and had been red ever since; this is the assertion they
/// wanted, and it is strictly stronger than the one it replaces:
///
///  * the sender's bytes must survive as an exact **suffix** — nothing inserted
///    in the middle, nothing mutated, nothing truncated;
///  * what precedes them must be *only* reserved-namespace stamps, checked with
///    the production strip (`fauna_mail::received_header::strip_fauna_headers`,
///    the inbound forgery strip) rather than a hand-rolled scan, so this cannot
///    drift from the namespace the nest actually stamps in;
///  * and at least one stamp must be there, so a regression that stopped
///    stamping is a failure rather than a silent pass.
pub fn assert_delivered_intact(delivered: &[u8], sent: &[u8], what: &str) {
    let stripped = fauna_mail::received_header::strip_fauna_headers(delivered);
    assert_eq!(
        stripped.as_slice(),
        sent,
        "{what}: with the nest's X-Fauna-* delivery stamps removed, the delivered \
         message must be exactly what the sender submitted"
    );
    assert!(
        delivered.len() > sent.len() && delivered.ends_with(sent),
        "{what}: the delivery stamps must be PREPENDED, leaving the sender's \
         bytes an exact suffix"
    );
    assert!(
        delivered
            .windows(fauna_mail::aliases::HEADER_SPAM_THRESHOLD.len())
            .any(|w| w.eq_ignore_ascii_case(fauna_mail::aliases::HEADER_SPAM_THRESHOLD.as_bytes())),
        "{what}: every deliverable outcome is stamped with {} — its absence means \
         a delivery arm bypassed resolve_local_recipient",
        fauna_mail::aliases::HEADER_SPAM_THRESHOLD
    );
}

/// Deadline-poll helper. A green run returns on the first or second pass and
/// pays nothing; the budget only has to exceed scheduling jitter on a loaded
/// box, so it is sized far above any non-pathological delay rather than tuned.
///
/// Three WS-heartbeat conformance files had this exact body under the same
/// name.
pub async fn poll_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if f() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The mail domain the nest conformance fixtures serve.
///
/// Four files that provision a mail recipient each declared their own
/// `const DOMAIN: &str = "fauna.test";` and then had to keep it equal to the
/// others by hand, because [`provision_recipient`] seeds an exact alias under it.
pub const TEST_DOMAIN: &str = "fauna.test";

/// Seed a registered user on the `free` tier with `handle`.
///
/// Seven files had this verbatim. The values it fixes are the point: the `free`
/// tier and a `None` guardian are what a plain self-registered account looks
/// like, so a fixture built through this one is arguing about the same account
/// shape as its neighbours rather than a hand-picked one.
pub async fn register_user(state: &Arc<AppState>, actor: [u8; 32], handle: &str) {
    state
        .db
        .create_user_with_handle(&actor, "free", handle, None)
        .await
        .unwrap();
}

/// A signed `fauna.account.register` request body: sign-over-`register_signed_message`
/// then encode a [`fauna_protocol::account::RegisterRequest`]. `domain` is the
/// caller's own `DOMAIN` const (kept per-file since several also use it to
/// build `handle_domain` elsewhere), not fixed here.
pub fn register_payload(
    kp: &ActorKeypair,
    handle: &str,
    domain: &str,
    invite_code: Option<&str>,
    age_claim: Option<fauna_protocol::age::AgeClaim>,
) -> Bytes {
    let ts = fauna_core::data::Timestamp::now_millis();
    let msg =
        fauna_protocol::account::register_signed_message(&kp.actor_id().0, handle, domain, ts);
    let sig = kp.signing_key().sign(&msg);
    encode(&fauna_protocol::account::RegisterRequest {
        actor_id: hex::encode(kp.actor_id().0),
        handle: handle.to_string(),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        invite_code: invite_code.map(str::to_string),
        age_claim,
        extra: Default::default(),
    })
}

/// The MSEK a fixture recipient's seal key derives from when the test never
/// opens what was sealed to it and so has no reason to name its own.
pub const FIXTURE_MSEK: [u8; 32] = [9u8; 32];

/// Give `actor` a recipient seal key the way the provision door does: both
/// halves, derived from `msek`, in one write through the production writer
/// (`CacheDb::put_actor_recipient_seal_key`). Returns the X25519 public half.
///
/// The one fixture writer for `actor_mls_pubkeys` in the integration tests.
/// The key is a real MSEK-derived pair, so a seal to it selects the X-Wing
/// suite as it does for an app user, and [`open_recipient_record`] opens the
/// result.
pub async fn seed_recipient_seal_key(db: &CacheDb, actor: &[u8; 32], msek: &[u8; 32]) -> [u8; 32] {
    let (_secret, pubkey) = derive_recipient_hpke_keypair(msek);
    let xwing = fauna_mls::wrapped_blob::derive_recipient_xwing_keypair(msek);
    db.put_actor_recipient_seal_key(actor, &pubkey, xwing.public.mlkem_encaps_key())
        .await
        .expect("seed recipient seal key");
    pubkey
}

/// The secret halves of the recipient key [`seed_recipient_seal_key`] wrote for
/// an MSEK — what the recipient's own app derives to open their mail.
pub struct RecipientKeys {
    x25519_secret: [u8; 32],
    mlkem_dk: [u8; fauna_mls::wrapped_blob::MLKEM768_DECAPS_KEY_LEN],
}

impl RecipientKeys {
    pub fn derive(msek: &[u8; 32]) -> Self {
        let (x25519_secret, _pubkey) = derive_recipient_hpke_keypair(msek);
        let xwing = fauna_mls::wrapped_blob::derive_recipient_xwing_keypair(msek);
        Self {
            x25519_secret,
            mlkem_dk: *xwing.secret.mlkem_decaps_key(),
        }
    }

    /// Open one sealed record — either suite, as the envelope names its own.
    pub fn open(
        &self,
        env: &BridgeMailEnvelope,
    ) -> Result<Vec<u8>, fauna_mls::wrapped_blob::UnwrapError> {
        fauna_mls::wrapped_blob::unseal_mail_record_hybrid(env, &self.x25519_secret, &self.mlkem_dk)
    }
}

/// Open the canonical bytes of a record sealed to the recipient key
/// [`seed_recipient_seal_key`] wrote for `msek`.
pub fn open_recipient_record(sealed: &[u8], msek: &[u8; 32]) -> Vec<u8> {
    let env = BridgeMailEnvelope::from_canonical_bytes(sealed).expect("decode envelope");
    RecipientKeys::derive(msek)
        .open(&env)
        .expect("open recipient record")
}

/// Give `recipient` a published seal key derived from `msek` and an exact
/// alias at `local_part@`[`TEST_DOMAIN`], returning the keys that open their mail.
///
/// Four mail/CalDAV fixtures had this verbatim. It is a *pair*, which is why it
/// wants an owner: the alias is what routes a message to the actor and the
/// pubkey is what seals it, and a fixture that seeds one without the other
/// produces a recipient that either cannot be addressed or cannot be opened —
/// both of which surface far from the seeding as a puzzling delivery failure.
pub async fn provision_recipient(
    state: &Arc<AppState>,
    recipient: [u8; 32],
    local_part: &str,
    msek: [u8; 32],
) -> RecipientKeys {
    seed_recipient_seal_key(&state.db, &recipient, &msek).await;
    state
        .db
        .put_exact_alias(TEST_DOMAIN, local_part, "exact", &recipient)
        .await
        .expect("seed exact alias");
    RecipientKeys::derive(&msek)
}

/// A public-mode [`NestConfig`] on an ephemeral port with `db_path`.
///
/// Four files had this verbatim, and each one **listed every section of
/// `NestConfig` explicitly** — the fixture shape this project's rebase-conflict
/// guidance warns against: prefer struct-update fixtures over hand-listing every
/// field, so two branches independently growing the same struct still merge
/// cleanly. `NestConfig` has no `Default` impl, so the hand-listing cannot be
/// removed from here without changing a production config type; consolidating
/// the four copies into one is the part that is free, and it turns the next
/// section-add from four conflict-prone edits into one.
pub fn config_with_db_path(db_path: String) -> Arc<NestConfig> {
    Arc::new(NestConfig {
        nest: NestSection {
            mode: NodeMode::Public,
            listen: "127.0.0.1:0".into(),
            db_path,
            ..Default::default()
        },
        bridges: None,
        submission: None,
        acme: None,
        email: None,
        update: Default::default(),
    })
}

/// A public-mode [`NestConfig`] on an ephemeral port whose only interesting
/// axis is the submission policy.
pub fn test_config(policy: SubmissionPolicy) -> Arc<NestConfig> {
    Arc::new(NestConfig {
        nest: NestSection {
            mode: NodeMode::Public,
            listen: "127.0.0.1:0".into(),
            db_path: String::new(),
            ..Default::default()
        },
        bridges: None,
        submission: Some(SubmissionSection { policy }),
        acme: None,
        email: None,
        update: Default::default(),
    })
}

/// Seed the `users` row a dispatching actor needs in order to hold a
/// [`fauna_nest::bridge_method_allowlist::CallerClass`].
///
/// The central authority gate (`caller_class_for_actor`) resolves an actor with
/// **no `users` row** to no class at all, so every handler that consults it — some
/// 48 families — refuses a synthetic actor id outright. Production reaches the
/// seeded state through `fauna.account.register`, or through the auth handshake's
/// auto-provision arm on an open-registration nest (`auth_core.rs`); an
/// integration test that invokes `(meta.handler)(state, actor, payload)` directly
/// runs *neither*, so it must seed the row itself.
///
/// Idempotent, unlike [`CacheDb::create_user`] (a bare `INSERT`, so a second call
/// for the same actor is a `UNIQUE` violation). That lets a file call it once per
/// dispatch — including from inside a local `dispatch` helper — rather than
/// threading "have I seeded this actor yet?" through the test body.
///
/// **Sequentially idempotent only — NOT concurrency-safe**: the check-then-insert
/// races when two tasks/threads seed the same actor at once (both see no row,
/// the loser hits the `UNIQUE` violation). A fixture whose requester is
/// dispatched from more than one thread — e.g. `conformance_account_runtime.rs`,
/// whose store-thread runtimes dispatch beside the test thread — seeds once at
/// fixture build instead of per dispatch.
///
/// Seeds the `free` tier, matching what the auto-provision arm creates. A test
/// needing another tier (or a handle, or a guardian link) should call
/// [`CacheDb::create_user_with_handle`] itself before dispatching.
///
/// The all-zero actor is **not** seedable: it is the anonymous pre-identity
/// placeholder and the gate refuses it before any DB lookup. Tests that dispatch
/// as `[0u8; 32]` exercise anonymous or pre-identity kinds and need no row.
pub async fn seed_user(db: &CacheDb, actor: &[u8; 32]) {
    assert_ne!(
        actor, &[0u8; 32],
        "the all-zero actor is the anonymous placeholder — it can never hold a \
         CallerClass, so seeding a `users` row for it would be misleading"
    );
    if db.get_user(actor).await.expect("get_user").is_none() {
        db.create_user(actor, "free", "test-seeded")
            .await
            .expect("seed users row");
    }
}

/// Seed a dispatching actor's `users` row, leaving the anonymous actor alone.
///
/// This is the form a test's local `dispatch` helper calls, one line ahead of
/// `(meta.handler)(state, actor, payload)`. Production never reaches a handler
/// without having created that row first — `fauna.account.register`, or the auth
/// handshake's auto-provision arm — so seeding here is what makes the fixture
/// model production rather than a state no live actor is ever in.
///
/// Unlike [`seed_user`] this tolerates `[0u8; 32]`, because a `dispatch` helper
/// is shared by every kind in its file: the pre-identity and anonymous kinds
/// (`register`, `claim`, `setup.status`, the handshake) dispatch as the anonymous
/// actor and legitimately hold no row. Seeding is a no-op for them rather than a
/// panic, so one call at the top of `dispatch` is correct for the whole file.
pub async fn seed_dispatch_actor(db: &CacheDb, actor: &[u8; 32]) {
    if actor != &[0u8; 32] {
        seed_user(db, actor).await;
    }
}

/// Invoke one registered RPC kind's handler the way the router would, seeding
/// the caller's `users` row first.
///
/// The seeding half is the load-bearing half, and it is exactly why this lives
/// here rather than in each file: [`seed_dispatch_actor`] documents a *production*
/// precondition — `caller_class_for_actor` resolves an actor with no `users` row
/// to no class at all, so some 48 handler families refuse a synthetic actor id
/// outright — and a test that calls `(meta.handler)(...)` directly runs neither
/// of the two production paths that create that row. Fifty-one integration test
/// files had each hand-copied this pair of lines; one owner means a change to the
/// precondition is one edit, not fifty-one, with no file left silently testing a
/// different entry contract than its neighbours.
///
/// Errors are returned, not unwrapped: refusal is the assertion in a good share
/// of the callers.
pub async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

/// A typed RPC round trip: encode `req` via [`encode`], dispatch it against
/// `kind`, decode the reply as `Rep`. Unlike [`dispatch`], this does NOT seed
/// the caller — most of this helper's callers are pre-identity or drive their
/// own account setup deliberately.
///
/// Five integration files (`conformance_recovery_escrow.rs`,
/// `conformance_recovery_replacement.rs`, `conformance_recovery_registration.rs`,
/// `conformance_succession.rs`, `succession_propagation.rs`) each hand-copied
/// this byte-identical function.
pub async fn call<Req: serde::Serialize, Rep: serde::de::DeserializeOwned>(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    req: &Req,
) -> Result<Rep, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    let out = (meta.handler)(state.clone(), actor, encode(req)).await?;
    Ok(decode(&out).unwrap())
}

/// Register `recovery` as `actor`'s `RecoveryKey` over its own authenticated
/// connection — the precondition every escrow fetch or replacement is
/// verified against. `conformance_recovery_escrow.rs` and
/// `conformance_recovery_replacement.rs` each hand-copied this
/// byte-identical function under the same name.
pub async fn register_recovery_key(
    router: &RpcRouter,
    state: &Arc<AppState>,
    seed: &SigningKey,
    actor: [u8; 32],
    recovery: &RecoveryKey,
    prior: Option<&RecoveryKey>,
    seq: u64,
) {
    state
        .db
        .create_user_with_handle(&actor, "free", &format!("u{seq}{}", actor[0]), None)
        .await
        .ok();
    let reg = RecoveryKeyRegistration {
        actor_id: ActorId(actor),
        recovery_pubkey: recovery.public(),
        seq,
        created_at: Timestamp(1_753_000_000),
    };
    let signed = reg.sign(seed, recovery, prior).expect("sign");
    let _: RegistrationSubmitReply = call(
        router,
        state,
        actor,
        "fauna.recovery.registration.submit",
        &RegistrationSubmitRequest {
            registration: ByteBuf::from(canonical_encode(&signed).expect("encode")),
            extra: Default::default(),
        },
    )
    .await
    .expect("registration lands");
}

/// Build and sign an identity-succession statement: `recovery` signs, the
/// successor's `new_seed` signs, and the predecessor's `old_seed` signs only
/// when a test supplies one (the home nest never consults it — a thief can
/// always produce it). Lifted out of `conformance_succession.rs` when the
/// conversation-rooms conformance suite needed a real succession too.
/// (`succession_propagation.rs` keeps a narrower copy that never signs with the
/// predecessor's seed — the next lift pass folds it into this one.)
pub fn succession_bytes(
    recovery: &RecoveryKey,
    old: [u8; 32],
    new_seed: &SigningKey,
    old_seed: Option<&SigningKey>,
    seq: u64,
) -> Vec<u8> {
    let statement = IdentitySuccession {
        old_actor_id: ActorId(old),
        new_actor_id: ActorId(new_seed.verifying_key().to_bytes()),
        recovery_pubkey: recovery.public(),
        seq,
        created_at: Timestamp(1_753_200_000),
    };
    let signed = statement
        .sign(recovery, new_seed, old_seed)
        .expect("sign succession");
    canonical_encode(&signed).expect("encode")
}

/// Submit a succession statement to `fauna.recovery.succession.submit` on the
/// ANONYMOUS connection — the kind is pre-identity, and submitting without a
/// session is the property the ceremony has to hold, not a shortcut.
pub async fn submit_succession(
    router: &RpcRouter,
    state: &Arc<AppState>,
    bytes: Vec<u8>,
) -> Result<SuccessionSubmitReply, RpcError> {
    call(
        router,
        state,
        [0u8; 32],
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(bytes),
            extra: Default::default(),
        },
    )
    .await
}

/// Like [`call`], but pre-encoded: caller supplies the raw request `Bytes`
/// and gets the raw reply `Bytes` back, with no seeding of `actor`'s `users`
/// row. Eight integration files (`conformance_feature_gate_surfaces.rs`,
/// `conformance_feature_gate.rs`, `conformance_post_unlock_tiers.rs`,
/// `conformance_subscription_followers.rs`, `conformance_payments.rs`,
/// `conformance_region_tier.rs`, `conformance_subscription_unsubscribe.rs`,
/// `conformance_unlock_rank_fanout.rs`) each hand-copied this byte-identical
/// function under the same name.
pub async fn call_raw(
    router: &RpcRouter,
    state: Arc<AppState>,
    kind: &str,
    actor: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

/// A `fauna.subscriptions.tiers.create` request for `name` at `rank`, carrying
/// the REQUIRED birth envelope ([`encrypted_keyblob::birth_upload`]) the
/// author signs. Every other field is the plain default; callers override with
/// struct update (`TierCreateRequest { auto_approve: true, ..tier_create_request(..) }`).
pub fn tier_create_request(author: &ActorKeypair, name: &str, rank: u32) -> TierCreateRequest {
    TierCreateRequest {
        name: name.into(),
        rank,
        description: None,
        price_hint: None,
        payment_url: None,
        auto_approve: false,
        encrypted_upload: encrypted_keyblob::birth_upload(author, name),
        unlocks_post: None,
        asking_price: None,
        hidden: false,
        extra: Default::default(),
    }
}

/// **The birth-blob tier fixture.** Create a tier through the real
/// `fauna.subscriptions.tiers.create` door as `author`, so it carries the birth
/// `KeyBlob` at version 1 exactly as every production tier does — the prior
/// every upload door (`requests.approve`, `subscribers.remove`,
/// `key_blob.rotate`) requires. A tier seeded straight into the DB has no
/// stored blob, and those doors refuse it. The birth blob's `rotated_at` is the
/// earliest instant, so any later upload advances past it; a first approve
/// therefore lands at `key_version` 2. Four integration files each hand-copied
/// a `create_tier` over this dispatch.
pub async fn create_tier(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: &ActorKeypair,
    req: TierCreateRequest,
) -> Result<TierCreateReply, RpcError> {
    let bytes = encode_canonical(&req).unwrap();
    let reply = call_raw(
        router,
        state,
        "fauna.subscriptions.tiers.create",
        author.actor_id().0,
        Bytes::from(bytes.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode tiers.create reply"))
}

/// Mint a birth `KeyBlob` for `tier` and drive `fauna.subscriptions.requests.approve`
/// with it as the `encrypted_upload`. `conformance_subscription_unsubscribe.rs`
/// and `conformance_unlock_rank_fanout.rs` each hand-copied this byte-identical
/// function, unblocked by [`call_raw`]'s own lift (round 39/40 of the shared-Rust
/// lift sweep).
#[allow(clippy::too_many_arguments)] // mirrors the mint call's own field list; a struct would just relocate them
pub async fn approve_with_mint(
    router: &RpcRouter,
    state: Arc<AppState>,
    author_kp: &ActorKeypair,
    request_id: i64,
    tier: &str,
    roster: &[ActorId],
    period_key: &[u8; 32],
    rotated_at: u64,
) -> Result<ApproveRequestReply, RpcError> {
    let auth = encrypted_keyblob::make_device_authorization(
        author_kp,
        author_kp,
        vec![Capability::ManageSubscribers],
    );
    let (_blob, upload) = encrypted_keyblob::mint_test_key_blob(
        author_kp,
        &auth,
        tier,
        Timestamp(rotated_at),
        roster,
        period_key,
    );
    let req = ApproveRequestRequest {
        request_id,
        encrypted_upload: Some(upload),
        extra: Default::default(),
    };
    let bytes = encode_canonical(&req).unwrap();
    let reply = call_raw(
        router,
        state,
        "fauna.subscriptions.requests.approve",
        author_kp.actor_id().0,
        Bytes::from(bytes.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode approve reply"))
}

/// Fetch `actor`'s INBOX via `fauna.email.inbox.fetch`, assert exactly one
/// message is waiting, and unseal it back to plaintext RFC 5322 bytes.
/// `conformance_caldav_imip_send.rs` and `conformance_caldav_imip_reply.rs`
/// each hand-rolled this exact fetch+decode+unseal under the same name.
pub async fn open_only_inbox_message(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    keys: &RecipientKeys,
) -> Vec<u8> {
    let req = InboxFetchRequest {
        extra: Default::default(),
        after_uid: 0,
        limit: 0,
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.email.inbox.fetch",
        payload,
    )
    .await
    .expect("inbox.fetch ok");
    let inbox: InboxFetchReply = decode(&reply).expect("decode inbox.fetch reply");
    assert_eq!(inbox.messages.len(), 1, "exactly one message delivered");
    let segment_env = SegmentMailEnvelope::decode(&inbox.messages[0].sealed_envelope)
        .expect("decode outer segment envelope");
    let bridge_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("decode inner bridge envelope");
    keys.open(&bridge_env)
        .expect("recipient opens their own mail")
}

/// The default service id — what eleven of the twelve hand-copied
/// `approve_bridge` helpers passed before they were lifted here.
const TEST_BRIDGE_SERVICE_ID: &str = "test-bridge";

/// Enroll a bridge service-user with `role`, record its x25519 transport key,
/// and flip it to `approved` — the three-call sequence that puts a synthetic
/// bridge actor into the state every bridge-plane handler expects.
///
/// Twelve integration test files had each hand-copied this, in four bodies
/// differing in exactly two values: the service id and the x25519 key. The id
/// is defaulted here because eleven of the twelve passed the same one; the
/// twelfth enrolls an MDA *and* an MTA in one fixture and needs them distinct,
/// so it calls [`approve_bridge_as`]. Same `_as` split as
/// `fauna_peer_sync::{admit_over, admit_over_as}`.
pub async fn approve_bridge(db: &CacheDb, pk: &[u8; 32], role: BridgeRole, x25519: &[u8; 32]) {
    approve_bridge_as(db, pk, role, TEST_BRIDGE_SERVICE_ID, x25519).await;
}

/// [`approve_bridge`] with the service id spelled out — for a fixture enrolling
/// more than one bridge, where the ids must differ.
pub async fn approve_bridge_as(
    db: &CacheDb,
    pk: &[u8; 32],
    role: BridgeRole,
    service_id: &str,
    x25519: &[u8; 32],
) {
    db.create_pending_bridge_service_user(pk, role, service_id)
        .await
        .expect("create pending bridge");
    db.upsert_bridge_x25519(pk, x25519)
        .await
        .expect("upsert x25519");
    db.approve_bridge_service_user(pk, None)
        .await
        .expect("approve bridge");
}

/// [`approve_bridge_as`] with the `Mda` role, the `"mda-1"` service id, and a
/// `[1u8; 32]` transport key — the exact combination
/// `conformance_bridge_blobs.rs` and
/// `conformance_caldav_autoschedule_in_domain.rs` each hand-rolled, eleven
/// call sites total.
pub async fn approve_mda(state: &Arc<AppState>, bridge_actor: [u8; 32]) {
    approve_bridge_as(
        &state.db,
        &bridge_actor,
        BridgeRole::Mda,
        "mda-1",
        &[1u8; 32],
    )
    .await;
}

/// How long [`connected_client`] waits for the client to reach
/// [`ConnectionState::Connected`].
///
/// **Generous on purpose** (e2e convention 14 — assert latency-independent
/// state, never wall-clock timing). What the caller asserts is that the
/// connection *reaches* `Connected`, never that it does so quickly: the auth
/// handshake plus the WS upgrade is real work, and every dev machine here runs
/// several sessions' builds and e2e suites at once, so a connect taking several
/// seconds is ordinary load, not a defect. A genuine hang still fails — at this
/// ceiling instead of a tight one — and the per-test cap is the real backstop.
///
/// ⚠ **This budget was never the reason the fixture flaked, and raising it did
/// NOT help — that was tried and measured.** Pinning a development machine with
/// `taskset` down to a CI runner's CPU count reproduced the flake on demand,
/// and the ceiling made no difference at that size:
///
/// | pinning | budget | failures |
/// |---|---|---|
/// | 4 CPUs | 60 s  | 4 / 15 |
/// | 4 CPUs | 180 s | 4 / 15 |
/// | 2 CPUs | 60 s  | 4 / 4  |
/// | 2 CPUs | 180 s | 4 / 8  |
///
/// **The cause is now known, and it was not a scheduling starvation.** The
/// state reported on failure — **`Disconnected`**, the watch channel's initial
/// value — was read at the time as proof that the spawned supervisor "had not
/// been polled once". It was not proof: `Disconnected` is equally the value a
/// *running* supervisor leaves behind, because its publications were being
/// **discarded**. `tokio::sync::watch::Sender::send` reports `Err` and leaves
/// the stored value untouched when no receiver is alive at that instant, and
/// `NestClient` keeps none of its own between `with_registry` (which drops the
/// channel's original receiver) and the subscribe that `connect()` performs
/// *after* spawning the supervisor. The supervisor ran, dialled, connected and
/// served — and the watch still read `Disconnected`. CPU pinning did not starve
/// it; it merely widened the window in which the spawning thread was preempted
/// between those two statements, which is why the flake tracked core count.
/// The corroborating detail that "no supervisor log output" appeared is void
/// on its own terms: the retry logs are `tracing::info!` and these tests
/// initialise tracing at `WARN`.
///
/// Fixed by publishing with `send_replace` at every site
/// (`fauna_ws_substrate::run_supervisor`'s `report!`, its `Connected` /
/// `Disconnected` publications, and `NestClient::disconnect`), pinned by
/// `a_supervisor_started_with_no_subscriber_still_reports_the_truth` and
/// `disconnect_is_visible_to_a_subscriber_that_attaches_afterwards`. The
/// budget stays at 60 s on its **original** justification — a generous
/// ceiling for real work on a loaded machine — and not as a flake mitigation;
/// there is no longer a flake for it to mitigate. Still do not widen it: a
/// failure here is now a real one.
///
/// The number this replaces is half the reason the helper exists: of the
/// eighteen hand-copied waits, seventeen said 5s and one —
/// `conformance_public_follow_client.rs` — had been bumped to 10s on its own.
/// One file discovering a budget is too tight while the other seventeen never
/// hear about it is exactly the drift a copied fixture produces.
const CONNECT_BUDGET: Duration = Duration::from_secs(60);

/// Bind the loopback listener a TLS test nest serves on, and hand it back with
/// its authority (`127.0.0.1:<port>`) — through [`fresh_nest_listener`], so the
/// process's identity pin for that authority starts empty.
pub async fn nest_listener() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port for the test nest");
    fresh_nest_listener(listener)
}

/// Declare `listener`'s authority a NEW nest's: forget this process's identity
/// pin (and graduated SPKI) for it before any client dials it.
///
/// The client's TOFU pin store is process-wide and keyed by host:port
/// (`fauna_anon_client::trust`), while every test in one binary shares the
/// process and binds `127.0.0.1:0`. When a test ends its nest's listener
/// closes, the OS may hand that port to a later test's nest — which carries a
/// fresh identity — and the earlier test's pin still names the dead nest, so
/// the next connect fails `NestIdentityChanged` exactly as a swapped box must
/// in production. How often depends on how the OS recycles ports under load,
/// which is why it surfaced first on a 4-core CI runner. Forgetting at bind
/// time is race-free: while this listener holds the port no other live nest
/// can be at this authority, so any pin for it belongs to a nest that is gone.
pub fn fresh_nest_listener(listener: tokio::net::TcpListener) -> (tokio::net::TcpListener, String) {
    let port = listener.local_addr().expect("listener address").port();
    let authority = format!("127.0.0.1:{port}");
    fauna_anon_client::forget_identity_pin(&authority);
    (listener, authority)
}

/// Connect a real [`NestClient`] to `base` as `keypair`, handing it back only
/// once its connection state has actually reached [`ConnectionState::Connected`].
///
/// `NestClient::connect()` returning `Ok` means the auth handshake succeeded and
/// the reconnect supervisor is spawned — **not** that the WS upgrade completed.
/// That API's own doc is explicit: the handshake "has been initiated but not
/// necessarily completed; observe `connection_state()`". The upgrade lands in
/// the supervisor task, which publishes `Connected` once it does and retries on
/// backoff when it does not (see [`CONNECT_BUDGET`]). A test that issues an RPC in between races that
/// publication, so every `*_client.rs` conformance fixture waits for it — a
/// deadline poll on the watch channel, never a settle-sleep.
///
/// Eighteen of those files had each hand-copied this wait, in six bodies whose
/// only semantic difference was the budget (see [`CONNECT_BUDGET`]); the rest of
/// the spread was a blank line, a local named `st` rather than `state`, and one
/// `std::time::Duration` spelled out in full.
pub async fn connected_client(base: &str, keypair: ActorKeypair) -> Arc<NestClient> {
    let nest = NestClient::new(base.to_string(), keypair);
    nest.connect().await.expect("connect (auth + WS upgrade)");
    wait_for_connected(&nest).await;
    nest
}

/// Wait for an already-`connect()`-ing (or reconnecting) [`NestClient`] to
/// reach [`ConnectionState::Connected`] on its watch channel — the deadline
/// poll half of [`connected_client`], factored out for a caller that already
/// holds a client (a pin-graduation retry, a reconnect after a drop) and only
/// needs to wait, never to construct or dial one.
pub async fn wait_for_connected(nest: &Arc<NestClient>) {
    let started = std::time::Instant::now();
    let mut state = nest.connection_state();
    // Every transition seen while waiting, with the offset it arrived at. The
    // final state alone is not enough to say what went wrong — that is exactly
    // the reading error that cost this fixture a session (see [`CONNECT_BUDGET`]):
    // `Disconnected` is both the value left behind by a supervisor that
    // published nothing and the one a *running* supervisor reports after a
    // failed dial and throughout its backoff nap. An empty trail versus a
    // `Connecting` → `Disconnected` cycle separates those two on the first
    // failure, rather than costing a rerun to find out (e2e convention 6 —
    // failures diagnose themselves).
    let history: std::cell::RefCell<Vec<(Duration, ConnectionState)>> =
        std::cell::RefCell::new(Vec::new());
    tokio::time::timeout(CONNECT_BUDGET, async {
        loop {
            if *state.borrow() == ConnectionState::Connected {
                return;
            }
            state
                .changed()
                .await
                .expect("connection-state channel open");
            history
                .borrow_mut()
                .push((started.elapsed(), *state.borrow()));
        }
    })
    .await
    .unwrap_or_else(|_| {
        // Which state the supervisor was left in separates "still dialling"
        // from `Unreachable` — its own verdict that it has given up — so the
        // failure says which without needing a rerun to find out.
        let last = *nest.connection_state().borrow();
        let seen = history.borrow();
        let trail = if seen.is_empty() {
            "none at all — nothing was ever published to the watch".to_string()
        } else {
            seen.iter()
                .map(|(at, s)| format!("{:.1}s {s:?}", at.as_secs_f64()))
                .collect::<Vec<_>>()
                .join(" → ")
        };
        panic!(
            "client never reached Connected within {CONNECT_BUDGET:?} \
             (last observed state: {last:?}; transitions: {trail})"
        )
    });
}

/// A bearer-token `SyncClient` + its underlying `NestClient`, both bound to
/// `owner_secret`/`device_id` — the auth/transport half of a `SyncEngine` test
/// fixture, factored out from three conformance files
/// (`conformance_content_key_chunk_route.rs`, `conformance_sync_mode_role_flip.rs`,
/// `conformance_sync_engine_record_commit.rs`) that each hand-copied it
/// byte-identically around their own `SyncEngine::new(...)` call, which keeps
/// its per-test params (`folder`, `backup_key` vs `content_keys`, the `SyncDb`)
/// inline at the call site rather than folded in here.
///
/// The body now lives in `fauna_nest::test_support`, because
/// `bins/fauna-sync-agent`'s `tier3-nest` harnesses need the same pairing and a
/// `tests/common/` module is reachable only from this crate's own test binaries.
/// This stays as the name those three files already call.
pub fn sync_engine_auth_client(
    dest_url: &str,
    http_token: &str,
    owner_secret: [u8; 32],
    device_id: &[u8; 32],
) -> (
    fauna_sync_engine::nest_client::SyncClient,
    Arc<fauna_client::NestClient>,
) {
    fauna_nest::test_support::sync_engine_auth_client(dest_url, http_token, owner_secret, device_id)
}

/// A raw WS connection to a test nest, bypassing `fauna-client` entirely — for
/// tests that assert on wire-level behavior (close codes, heartbeat frames)
/// [`connected_client`] can't reach because it hides the socket.
pub type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open an authenticated raw WS connection: the `bearer.<token>` subprotocol
/// upgrade every low-level WS test drives by hand.
///
/// Four integration files (`ws_close_codes.rs`, `graceful_shutdown.rs`,
/// `conformance_revocation_teardown.rs`, `ws_server_heartbeat.rs`) each
/// hand-copied this byte-identical function against their own local `Harness`
/// type; it takes the three fields it actually reads directly so no shared
/// `Harness` shape is needed.
pub async fn open_authed(url: &str, kp: &ActorKeypair, token: &str) -> Ws {
    let full_url = format!("{url}/api/v1/ws/{}", hex::encode(kp.actor_id().0));
    let mut req = full_url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{token}").parse().unwrap(),
    );
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("authed upgrade should succeed");
    ws
}

/// Read frames until a Close arrives or an 8s deadline elapses. Returns the
/// close code (if any) and whether a **successful** Reply for `expect_corr` was
/// observed first — pass `None` when the caller never sent a request worth
/// tracking (so `reply_seen` always reads `false`).
///
/// ⚠ **`ok` is load-bearing, and was added 2026-09-12 after it cost a session an
/// hour.** This used to count any `Frame::Reply` at that correlation id, which
/// an RPC *refusal* also is: the central capability gate
/// (`routes.rs` gate (1d), `bridge_method_allowlist::is_permitted`'s
/// `_ => false` arm) refuses any registered, non-pre-identity kind that is not
/// on the allowlist — and a test harness that registers its own
/// `fauna.test.<something>` kind into its own router is exactly that case. So
/// `reply_seen` read `true` for a request whose handler had never run, and an
/// assertion written as "the in-flight reply was delivered" silently became "a
/// frame came back". `graceful_shutdown.rs` sidesteps the gate by naming a real
/// User-class kind (`fauna.posts.create`); requiring `ok` is what makes a
/// harness that forgets to notice.
///
/// Two integration files (`graceful_shutdown.rs`,
/// `conformance_revocation_teardown.rs`) each hand-copied this — round 75 of
/// the shared-Rust lift sweep.
pub async fn read_until_close(ws: &mut Ws, expect_corr: Option<u64>) -> (Option<u16>, bool) {
    let mut reply_seen = false;
    let overall = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let remaining = overall.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return (None, reply_seen);
        }
        let next = tokio::time::timeout(remaining, ws.next()).await;
        match next {
            Err(_) => return (None, reply_seen),   // overall timeout
            Ok(None) => return (None, reply_seen), // stream ended w/o close frame
            Ok(Some(Err(_))) => return (None, reply_seen),
            Ok(Some(Ok(Message::Close(frame)))) => {
                return (frame.map(|f| u16::from(f.code)), reply_seen);
            }
            Ok(Some(Ok(Message::Binary(bytes)))) => {
                if let (Some(corr), Ok(Frame::Reply(r))) = (expect_corr, decode_frame(&bytes))
                    && r.correlation_id == corr
                    && r.ok
                {
                    reply_seen = true;
                }
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Open a raw, unauthenticated WS connection to a test nest's public
/// `fauna.v1` endpoint — no bearer element, no actor bound.
///
/// Two integration files (`anonymous_rate_limit.rs`, `pre_identity_ws.rs`)
/// each hand-copied this byte-identical function.
pub async fn open_anonymous(base: &str) -> Ws {
    let url = format!("{base}/api/v1/ws");
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert(SEC_WEBSOCKET_PROTOCOL, "fauna.v1".parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("anonymous upgrade should succeed");
    ws
}

/// The next `Frame::Reply` on `ws` (control/other frames skipped), or a 2s
/// per-frame timeout.
///
/// Two integration files (`anonymous_rate_limit.rs`, `pre_identity_ws.rs`)
/// each hand-copied this byte-identical function — round 175 of the
/// shared-Rust lift sweep.
pub async fn recv_reply(ws: &mut Ws) -> Reply {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("recv timeout")
            .expect("stream ended")
            .expect("ws error");
        if let Message::Binary(b) = msg {
            match decode_frame(&b).expect("decode frame") {
                Frame::Reply(r) => return r,
                _ => continue,
            }
        }
    }
}

/// A signed `fauna.auth.handshake` payload authenticating `kp` at "now",
/// addressed to the nest whose identity is `nest_id` (`AppState::bound_identity`;
/// `login.md` § Binding the nest).
///
/// Two integration files (`anonymous_rate_limit.rs`, `pre_identity_ws.rs`)
/// each hand-copied this byte-identical function.
pub fn handshake_request(kp: &ActorKeypair, nest_id: [u8; 32]) -> HandshakeRequest {
    let ts = fauna_core::data::Timestamp::now_millis();
    let nonce = [0x42u8; 32];
    let msg = handshake_signed_message(&kp.actor_id().0, ts, &nest_id, &nonce);
    let sig = kp.signing_key().sign(&msg);
    HandshakeRequest {
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
        nest_id: hex::encode(nest_id),
        extra: Default::default(),
    }
}

/// Seal `plaintext` as a genuine recipient envelope under the fixed test MSEK
/// `[0x5E; 32]` — the S6.12(b) wire edge verifies both payload halves, so a
/// driven request (`IngestInboundMailRequest`, `PutCardCiphertextRequest`,
/// …) must carry real seals, not byte literals. HPKE encapsulation is NOT
/// deterministic: seal once and reuse the returned bytes when a test asserts
/// stored-vs-served byte identity, rather than calling this twice expecting
/// equal output.
///
/// Five conformance files (`segments_changed_push.rs`,
/// `mail_segment_round_trip.rs`, `conformance_family_mail_gate.rs`,
/// `conformance_carddav.rs`, `bridge_mailbox_state_push.rs`) each hand-rolled
/// this byte-identical function under the same name; unlike
/// [`seal_and_ingest`] below (which also dispatches the RPC), each caller
/// still assembles its own request around the sealed bytes, so this stays
/// the narrower standalone primitive.
pub fn sealed(plaintext: &[u8]) -> Vec<u8> {
    let (_secret, pubkey) = derive_recipient_hpke_keypair(&[0x5Eu8; 32]);
    seal_to_recipient(plaintext, &pubkey)
        .expect("seal test fixture")
        .to_canonical_bytes()
        .expect("canonical test fixture")
}

/// Fixed MSEK for DM-gate fixtures — `sealed_dm` seals to its derived
/// pubkey (the same envelope shape the production D2-resolved seal writes);
/// a caller opening the sealed bytes back derives its secret from the same
/// constant.
pub const TEST_DM_MSEK: [u8; 32] = [0x42u8; 32];

/// Seal `text` as a verified `SealedRecordBytes` DM envelope under
/// [`TEST_DM_MSEK`]. `conformance_family_dm_gate.rs` and
/// `conformance_nostr_bridge_leg.rs` each hand-rolled this byte-identical function.
pub fn sealed_dm(text: &str) -> fauna_mls::wrapped_blob::SealedRecordBytes {
    let (_secret, pubkey) = derive_recipient_hpke_keypair(&TEST_DM_MSEK);
    let env = seal_to_recipient(text.as_bytes(), &pubkey).expect("seal");
    fauna_mls::wrapped_blob::SealedRecordBytes::verify(env.to_canonical_bytes().expect("bytes"))
        .expect("verifies")
}

/// Seal `body` to `recipient_pubkey`, then ingest it into `recipient`'s
/// default INBOX via the BridgeMta `fauna.bridges.ingest_inbound_mail` kind
/// (the new path — seals into the `__mail/<actor>` segment store AND places a
/// `bridge_imap_messages` row). Returns the sealed bridge envelope bytes so
/// the caller can assert a round trip.
///
/// Three conformance files had this near-verbatim, differing only in the
/// `sender_domain` literal, whether the row was delivered as inbound mail or
/// as the account's own submission, and whether the caller kept the return
/// value. Two entry points — this one and [`seal_and_ingest_sent`] — cover
/// both shapes over one shared, `#[allow]`-annotated implementation, rather
/// than one public function clippy's argument-count lint would reject.
pub async fn seal_and_ingest(
    router: &RpcRouter,
    state: &Arc<AppState>,
    mta_actor: [u8; 32],
    recipient: [u8; 32],
    recipient_pubkey: &[u8; 32],
    body: &[u8],
    timestamp: i64,
) -> Vec<u8> {
    seal_and_ingest_inner(
        router,
        state,
        mta_actor,
        recipient,
        recipient_pubkey,
        body,
        timestamp,
        "external.test",
        "fauna.bridges.ingest_inbound_mail",
    )
    .await
}

/// [`seal_and_ingest`], but delivered as the account's **own submission** —
/// the BridgeMta `fauna.bridges.submit_inbound_mail` kind, which the nest
/// files into the **`Sent`** mailbox server-side — and sealed under the
/// actor's own domain, matching an external MUA's Sent copy of a message it
/// submitted.
///
/// It must be this door. `ingest_inbound_mail` never places inbound mail in
/// `Sent`, whatever mailbox the request names: a `Sent` record reads as the
/// user's own send, so a named `Sent` target falls back to the spam
/// disposition's mailbox (`email-filters.md` § Email filter rules). Only the
/// own-submission handler sets `is_own_submission`, and only it files into
/// `Sent`.
pub async fn seal_and_ingest_sent(
    router: &RpcRouter,
    state: &Arc<AppState>,
    mta_actor: [u8; 32],
    recipient: [u8; 32],
    recipient_pubkey: &[u8; 32],
    body: &[u8],
    timestamp: i64,
) -> Vec<u8> {
    seal_and_ingest_inner(
        router,
        state,
        mta_actor,
        recipient,
        recipient_pubkey,
        body,
        timestamp,
        "self.test",
        "fauna.bridges.submit_inbound_mail",
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn seal_and_ingest_inner(
    router: &RpcRouter,
    state: &Arc<AppState>,
    mta_actor: [u8; 32],
    recipient: [u8; 32],
    recipient_pubkey: &[u8; 32],
    body: &[u8],
    timestamp: i64,
    sender_domain: &str,
    kind: &str,
) -> Vec<u8> {
    let sealed_body = seal_to_recipient(body, recipient_pubkey)
        .expect("seal body")
        .to_canonical_bytes()
        .expect("encode sealed body envelope");
    let sealed_hint = seal_to_recipient(b"index-hint", recipient_pubkey)
        .expect("seal hint")
        .to_canonical_bytes()
        .expect("encode sealed hint envelope");
    let ingest_req = IngestInboundMailRequest {
        dedup_key: "env:v1:fixture".into(),
        envelope_key: "env:v1:fixture".into(),
        actor_id: recipient.to_vec(),
        encrypted_body: sealed_body.clone(),
        encrypted_index_hint: sealed_hint,
        public_metadata: PublicMailMetadata {
            timestamp,
            ciphertext_size: sealed_body.len() as u32,
            sender_domain: sender_domain.into(),
        },
        verdicts: AuthVerdicts {
            dkim: DkimVerdict::Pass,
            spf: SpfVerdict::Pass,
            dmarc: DmarcVerdict::Pass,
            ..Default::default()
        },
        ..Default::default()
    };
    let payload = Bytes::from(
        encode_canonical(&ingest_req)
            .expect("encode ingest")
            .to_vec(),
    );
    let reply_bytes = dispatch(router, state.clone(), mta_actor, kind, payload)
        .await
        .expect("MTA delivery must succeed");
    let reply: IngestInboundMailReply = decode(&reply_bytes).expect("decode ingest reply");
    assert_eq!(reply.message_id.len(), 32, "server-assigned 32-byte id");
    sealed_body
}

/// Register `[7u8; 32]` as an admin actor and return it.
///
/// Six conformance files had this exact body (a fixed, arbitrary admin
/// identity — no test asserts on the specific bytes, only that the actor
/// carries the admin role). Three other `admin_actor` helpers elsewhere in
/// this directory differ meaningfully (a caller-chosen handle routed through
/// `plain_user`, or an `Arc<AppState>` parameter for a different call
/// convention) and stay local.
pub async fn admin_actor(state: &AppState) -> [u8; 32] {
    let admin = [7u8; 32];
    state.db.add_admin_actor(&admin[..]).await.unwrap();
    admin
}

/// Chunk `body` as a single-chunk manifest, store both the chunk and the
/// encoded manifest in `store`/`db`, and return the manifest's content hash.
/// `conformance_path_sealing.rs` and `conformance_at_rest_byte_scan.rs` each
/// hand-rolled this exact body under the same name.
pub async fn put_manifest(store: &DiskBlobStore, db: &CacheDb, body: &[u8]) -> ContentHash {
    let chunk = ContentHash::of_raw(body);
    store.put(&chunk, body).await.unwrap();
    db.put_blob_metadata(&chunk.digest(), body.len() as i64, "chunk", None, None)
        .await
        .unwrap();
    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash: chunk,
        total_size: body.len() as u64,
        chunk_hashes: vec![chunk],
        chunk_sizes: vec![body.len() as u64],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
    };
    let bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
    let h = ContentHash::of_raw(&bytes);
    store.put(&h, &bytes).await.unwrap();
    db.put_blob_metadata(&h.digest(), bytes.len() as i64, "manifest", None, None)
        .await
        .unwrap();
    h
}

/// Stage one "uploaded" chunk: store its bytes + `blob_metadata`. Returns the
/// chunk's content hash and length. Three segment-backup conformance files
/// had this exact body under the same name.
pub async fn put_chunk(store: &DiskBlobStore, db: &CacheDb, data: &[u8]) -> (ContentHash, u64) {
    let h = ContentHash::of_raw(data);
    store.put(&h, data).await.unwrap();
    db.put_blob_metadata(&h.digest(), data.len() as i64, "chunk", None, None)
        .await
        .unwrap();
    (h, data.len() as u64)
}

/// Stage a `ChunkManifest` over an ALREADY-staged chunk (unlike [`put_manifest`],
/// which chunks + manifests raw bytes in one call, this pairs with
/// [`put_chunk`] when a test needs the chunk hash before the manifest exists).
/// Three segment-backup conformance files had this exact body under the same
/// name.
pub async fn put_manifest_over(
    store: &DiskBlobStore,
    db: &CacheDb,
    chunk: ContentHash,
    chunk_len: u64,
) -> ContentHash {
    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash: chunk,
        total_size: chunk_len,
        chunk_hashes: vec![chunk],
        chunk_sizes: vec![chunk_len],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
    };
    let bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
    let h = ContentHash::of_raw(&bytes);
    store.put(&h, &bytes).await.unwrap();
    db.put_blob_metadata(&h.digest(), bytes.len() as i64, "manifest", None, None)
        .await
        .unwrap();
    h
}

/// Build a 1:1 `ThreadDetail` whose only peer is `chip` (the resolved foreign
/// Fauna address). `conformance_custody_ceremony_client.rs` and
/// `conformance_cross_nest_conversations_client.rs` each hand-rolled this
/// exact body under the same name. Mirrors the tier_1 `fauna_mls_thread`
/// helper in `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`
/// (a separate crate, so not itself shareable here).
pub fn one_to_one_thread(thread_id: ThreadId, chip: TypedAddress) -> ThreadDetail {
    let participants = vec![chip];
    ThreadDetail {
        thread_id,
        rail: Rail::FaunaMls,
        glyph: Rail::FaunaMls.glyph(),
        flavor: ThreadFlavor::OneToOne,
        label: "thread".into(),
        participant_displays: participants.iter().map(|p| p.display()).collect(),
        participants,
        capabilities: derive_capabilities(Rail::FaunaMls, ThreadFlavor::OneToOne),
        messages: vec![],
        compose: ComposeState::default(),
        // View state, not a fact about any message — and this fixture starts with
        // an empty `messages`, so there is nothing selectable to point at.
        selected_message_id: None,
        bridge: None,
        room: None,
        guardian_state: None,
    }
}

/// Build a group `ThreadDetail` over `chips` — the multi-peer twin of
/// [`one_to_one_thread`]. Two or more Fauna peers is what makes
/// `FaunaMlsBackend::bootstrap_group` found a **governed** room (a 1:1 carries
/// no policy; `conversation-rooms.md` § The room), so this is the fixture for
/// every room-plane journey that needs roles on the floor.
pub fn group_thread(thread_id: ThreadId, chips: Vec<TypedAddress>) -> ThreadDetail {
    ThreadDetail {
        thread_id,
        rail: Rail::FaunaMls,
        glyph: Rail::FaunaMls.glyph(),
        flavor: ThreadFlavor::MlsGroup,
        label: "room".into(),
        participant_displays: chips.iter().map(|p| p.display()).collect(),
        participants: chips,
        capabilities: derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup),
        messages: vec![],
        compose: ComposeState::default(),
        selected_message_id: None,
        bridge: None,
        room: None,
        guardian_state: None,
    }
}

/// An identity seed and the actor id it *is* (`actor_id ≡ Ed25519 pubkey`),
/// derived from a single seed byte via `SigningKey::from_bytes(&[seed_byte;
/// 32])`. Five recovery/succession conformance files had this exact body
/// under the same name.
pub fn identity(seed_byte: u8) -> (SigningKey, [u8; 32]) {
    let sk = SigningKey::from_bytes(&[seed_byte; 32]);
    let actor = sk.verifying_key().to_bytes();
    (sk, actor)
}

/// A `ServedCertSpki` that reports a fixed fingerprint — stands in for the
/// live TLS resolver in in-process (no-socket) handler tests. Four TLS/auth
/// conformance files had this exact struct + impl under the same name.
pub struct FixedSpki(pub [u8; 32]);
impl ServedCertSpki for FixedSpki {
    fn current_spki_sha256(&self) -> Option<[u8; 32]> {
        Some(self.0)
    }
    fn served_cert_facts(&self, _sni: &str) -> Option<fauna_nest::acme::ServedCertFacts> {
        None
    }
    fn served_cert_spki_sha256(&self, _sni: &str) -> Option<[u8; 32]> {
        Some(self.0)
    }
}

/// A resolver serving a **self-signed** leaf (issuer DN == subject DN)
/// covering `primary`/`mx_sni` — the untrusted-cert floor case (`is_floor ==
/// true`). `conformance_mta_sts_serving.rs` and
/// `conformance_dns_dane_coupling.rs` had this exact body under the same
/// name.
pub fn floor_resolver(primary: &str, mx_sni: &str) -> Arc<dyn ServedCertSpki> {
    let (cert_pem, key_pem) = fauna_nest::self_signed_cert::synthesize_self_signed_pem(
        primary,
        vec![primary.to_string(), mx_sni.to_string()],
    )
    .expect("synthesize floor cert");
    resolver_from_pem(&cert_pem, &key_pem)
}

/// A real self-signed cert bundle (valid PEM, so `store_acme_material`'s
/// self-signed-backup logic runs against a parseable cert) standing in for the
/// publicly-trusted cert the client would obtain via DNS-01. Two files had
/// this exact body under the same name.
pub fn lan_cert_bundle(domain: &str) -> TlsCertBundle {
    let (cert_pem, key_pem) =
        fauna_nest::self_signed_cert::synthesize_self_signed_pem(domain, vec![domain.to_string()])
            .expect("synthesize cert");
    TlsCertBundle {
        cert_chain: cert_pem.into_bytes(),
        priv_key: key_pem.into_bytes(),
        issued_at: 1_715_000_000,
        expires_at: 1_715_000_000 + 90 * 24 * 3600,
    }
}

/// A resolver serving a **CA-issued** leaf (issuer DN != subject DN) covering
/// `primary`/`mx_sni` — the trusted-cert takeover (`is_floor == false`). Same
/// two files, same exact body under the same name.
pub fn trusted_resolver(primary: &str, mx_sni: &str) -> Arc<dyn ServedCertSpki> {
    let ca_key = rcgen::KeyPair::generate().expect("ca keygen");
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("ca params");
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Fauna Test Root CA");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).expect("ca self-sign");

    let leaf_key = rcgen::KeyPair::generate().expect("leaf keygen");
    let leaf_params = rcgen::CertificateParams::new(vec![primary.to_string(), mx_sni.to_string()])
        .expect("leaf params");
    let leaf = leaf_params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .expect("ca-sign leaf");
    resolver_from_pem(&leaf.pem(), &leaf_key.serialize_pem())
}

/// Load a PEM cert+key pair into a live `MultiDomainCertResolver`. Leaks the
/// temp dir it reads from — the resolver only reads the PEM at load time, so
/// keeping the files until process exit is harmless and avoids a guard. Same
/// two files, same exact body under the same name.
fn resolver_from_pem(cert_pem: &str, key_pem: &str) -> Arc<dyn ServedCertSpki> {
    let dir = tempfile::tempdir().unwrap();
    let cert_path = dir.path().join("fullchain.pem");
    let key_path = dir.path().join("privkey.pem");
    std::fs::write(&cert_path, cert_pem).unwrap();
    std::fs::write(&key_path, key_pem).unwrap();
    let resolver = MultiDomainCertResolver::from_pem(&cert_path, &key_path).expect("load leaf");
    std::mem::forget(dir);
    Arc::new(resolver)
}

/// Extract the raw MLS Welcome bytes from a recipient inbox row payload by
/// unwrapping the canonical DAG-CBOR inbox envelope (layer 1) — a `Welcome`
/// envelope carries the MLS welcome (plus channel metadata + the cross-nest
/// `nest_url`) inside its `WelcomeInbox`; any other payload is returned
/// unchanged. Four conformance files had this exact body under the same
/// name.
pub fn welcome_bytes_from_inbox(payload: &[u8]) -> Vec<u8> {
    if let Ok(env) = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(payload)
        && env.kind == fauna_protocol::inbox::InboxKind::Welcome
        && let Ok(w) = env.decode_welcome()
    {
        return w.welcome_bytes;
    }
    payload.to_vec()
}

/// A signed `(ContactRequest, Post)` inbox payload — the bytes a client sends
/// and a federation peer relays. `conformance_succession.rs` and
/// `succession_propagation.rs` each hand-rolled this exact body under the
/// same name.
pub fn signed_inbox_payload(seed: &SigningKey, author: [u8; 32], recipient: [u8; 32]) -> Vec<u8> {
    signed_inbox_payload_saying(seed, author, recipient, "hello")
}

/// [`signed_inbox_payload`] with the post's text chosen by the caller — two
/// sends in one test that must be *different* posts (the post id is computed
/// over the body) rather than one payload replayed.
pub fn signed_inbox_payload_saying(
    seed: &SigningKey,
    author: [u8; 32],
    recipient: [u8; 32],
    content: &str,
) -> Vec<u8> {
    use fauna_core::data::{ContactRequest, Post, PostBody, StructuredField, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, compute_post_id, sign_envelope};
    use fauna_core::identity::ActorId;

    let kp = ActorKeypair::from_secret(seed.to_bytes());
    let post = Post {
        author: ActorId(author),
        created_at: Timestamp(1_753_400_000),
        body: PostBody::Structured {
            schema: "note".into(),
            fields: vec![StructuredField {
                key: "to".into(),
                value: hex::encode(recipient),
            }],
            content: Some(content.into()),
            facets: vec![],
            items: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let (post_bytes, post_env) = sign_envelope(&kp, &post).expect("sign post");
    let post_wire = EmbedAsBytes::from_signed(post_bytes, post_env);

    let cr = ContactRequest {
        sender: ActorId(author),
        post_id: compute_post_id(&post).expect("post id"),
        sender_node: b"http://localhost:3000".to_vec(),
        summary: "hi".into(),
        created_at: Timestamp(1_753_400_000),
    };
    let (cr_bytes, cr_env) = sign_envelope(&kp, &cr).expect("sign cr");
    let cr_wire = EmbedAsBytes::from_signed(cr_bytes, cr_env);

    canonical_encode(&(&cr_wire, &post_wire)).expect("encode payload")
}

/// A real signed text post (embed-as-bytes wire), the production shape
/// clients submit — routes through `store_post`'s decode path, which scopes
/// the segment to the real author. Six test files hand-rolled this exact
/// body under the same name across `bins/fauna-nest/tests/`.
pub fn signed_text_post(kp: &ActorKeypair, content: &str) -> Vec<u8> {
    use fauna_core::data::Timestamp;
    signed_text_post_at(kp, content, Timestamp::now())
}

/// [`signed_text_post`] with an explicit `created_at` — a future instant for
/// the created_at-future-bound suite, a past one for archive-import
/// backdating scenarios.
pub fn signed_text_post_at(
    kp: &ActorKeypair,
    content: &str,
    created_at: fauna_core::data::Timestamp,
) -> Vec<u8> {
    use fauna_core::data::{Post, PostBody};
    use fauna_core::encoding::sign_and_pack;
    let post = Post {
        author: kp.actor_id(),
        created_at,
        body: PostBody::Text {
            content: content.into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    sign_and_pack(kp, &post).unwrap()
}

/// A signed text post carrying a `PostOrigin` — the shape the archive import
/// re-authors (`archive-import.md` § What each category becomes).
pub fn signed_text_post_with_origin(kp: &ActorKeypair, content: &str, platform: &str) -> Vec<u8> {
    use fauna_core::data::{Post, PostBody, PostOrigin, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: content.into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: Some(PostOrigin {
            platform: platform.into(),
            url: None,
        }),
    };
    sign_and_pack(kp, &post).unwrap()
}

/// A text post AUTHORED by `identity` and SIGNED by its delegated authoring
/// sub-key `sub`, carrying the identity-signed `Capability::Post` cert in
/// `signer_auth` — the delegated-authoring wire (`atproto-pds-full.md` D10).
pub fn delegated_text_post(identity: &ActorKeypair, sub: &ActorKeypair, content: &str) -> Vec<u8> {
    use fauna_core::data::{Capability, DeviceAuthorization, Post, PostBody, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    let post = Post {
        author: identity.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: content.into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let (bytes, env) = sign_envelope(sub, &post).unwrap();
    let cert = DeviceAuthorization {
        actor_id: identity.actor_id(),
        device_key: sub.actor_id().0,
        capabilities: vec![Capability::Post],
        created_at: Timestamp(1),
        expires_at: None,
    };
    let (cb, ce) = sign_envelope(identity, &cert).unwrap();
    let wire =
        EmbedAsBytes::from_signed(bytes, env).with_signer_auth(EmbedAsBytes::from_signed(cb, ce));
    canonical_encode(&wire).unwrap()
}

/// The exact bytes `ingest_post_core` stores and `maybe_enqueue_outbox` queues:
/// the canonical dag-cbor `EmbedAsBytes{envelope, bytes}` wire shape. Its blake3
/// is the content-addressed `post_id` on **both** nests — that identity is what
/// makes forwarding the stored body verbatim lossless.
pub fn signed_post_wire(author: &ActorKeypair, content: &str) -> (Vec<u8>, [u8; 32]) {
    let wire_bytes = signed_text_post(author, content);
    let post_id: [u8; 32] = *blake3::hash(&wire_bytes).as_bytes();
    (wire_bytes, post_id)
}

/// [`signed_post_wire`] with an explicit `created_at`.
pub fn signed_post_wire_at(
    author: &ActorKeypair,
    content: &str,
    created_at: fauna_core::data::Timestamp,
) -> (Vec<u8>, [u8; 32]) {
    let wire_bytes = signed_text_post_at(author, content, created_at);
    let post_id: [u8; 32] = *blake3::hash(&wire_bytes).as_bytes();
    (wire_bytes, post_id)
}

/// The signed embed-as-bytes `Tombstone` for `post_id` — the same `req.body`
/// `fauna.posts.delete` receives and `maybe_enqueue_delete_outbox` queues.
pub fn signed_tombstone_wire(author: &ActorKeypair, post_id: [u8; 32]) -> Vec<u8> {
    let tombstone = fauna_core::data::Tombstone {
        author: author.actor_id(),
        post_id: fauna_core::data::PostId::from_digest_dag_cbor(post_id),
        created_at: Timestamp::now(),
    };
    fauna_core::encoding::sign_and_pack(author, &tombstone).unwrap()
}

/// A minimal `MailFloorMetadata` — only `received_at` matters to most
/// callers, every other field a permissive "this passed" default. Struct-update
/// so the next additive floor field doesn't break this fixture (the reason
/// `MailFloorMetadata` carries a `Default` at all).
pub fn floor(received_at_ms: i64) -> fauna_mail::segments::MailFloorMetadata {
    fauna_mail::segments::MailFloorMetadata {
        format_version: fauna_mail::segments::MAIL_FLOOR_FORMAT_VERSION,
        received_at: received_at_ms,
        timestamp: received_at_ms / 1000,
        ciphertext_size: 0,
        sender_domain: "example.com".into(),
        spam_disposition: "accept".into(),
        is_own_submission: false,
        spf: "pass".into(),
        dkim: "pass".into(),
        dmarc: "pass".into(),
        dmarc_policy: "reject".into(),
        arc: "pass".into(),
        spam_score: 0,
        seq: 0,
        continuation_role: fauna_mail::segments::CONTINUATION_ROLE_NORMAL,
        ..Default::default()
    }
}

/// An in-memory nest DB with the nostr tables migrated and `actor` seeded as
/// a `free`-tier user. `conformance_bridge_search.rs` and
/// `conformance_nostr_inbound_lifecycle.rs` each hand-rolled this bootstrap.
#[cfg(feature = "nostr")]
pub async fn nest_db(actor: [u8; 32]) -> CacheDb {
    let db = CacheDb::open_in_memory().unwrap();
    {
        let conn = db.conn().await;
        fauna_nest::nostr::apply_schema(&conn).unwrap();
    }
    db.create_user(&actor, "free", "test").await.unwrap();
    db
}

/// Nostr-zap test fixtures — gated behind the `nostr` feature (an optional
/// dependency of this crate) so this module still compiles for every caller
/// that pulls in `mod common;` without it. `conformance_tips.rs`'s
/// `with_zaps` module, `conformance_zap_purchase.rs`'s `purchase` module and
/// `conformance_nostr_zap_trust_gate.rs` each hand-rolled `link_payee`,
/// `designate_zap_signer` and `zap_receipt` under these exact bodies.
#[cfg(feature = "nostr")]
pub mod zap {
    use fauna_bridge_nostr::signing::Keypair;
    use fauna_bridge_nostr::types::{Event, Tag, UnsignedEvent};
    use fauna_nest::nostr::db;
    use fauna_nest::routes::AppState;

    /// A payee: a local actor with a linked Nostr account.
    pub async fn link_payee(state: &AppState, actor: [u8; 32]) -> Keypair {
        let kp = Keypair::generate();
        let conn = state.db.conn().await;
        db::link_account(
            &conn,
            &hex::encode(actor),
            &kp.public_key_hex(),
            "generated",
            None,
            None,
            None,
        )
        .unwrap();
        drop(conn);
        kp
    }

    pub async fn designate_zap_signer(state: &AppState, actor: [u8; 32], signer: &Keypair) {
        let conn = state.db.conn().await;
        db::add_zap_signer(&conn, &hex::encode(actor), &signer.public_key_hex(), "Alby").unwrap();
        drop(conn);
    }

    /// A real, signature-valid kind-9735 receipt — the gate verifies the
    /// signature itself, so a forged fixture would be refused before the
    /// trust question is even asked and would make a green test vacuous.
    /// `request_recipient` is the embedded kind-9734 request's own `p` tag —
    /// equal to `payee_pubkey` for every ordinary receipt, but a distinct
    /// value lets a caller build a receipt whose outer envelope and inner
    /// request name different recipients (the trust gate's stapling-forgery
    /// case).
    #[allow(clippy::too_many_arguments)]
    pub fn zap_receipt(
        signer: &Keypair,
        payee_pubkey: &str,
        request_recipient: &str,
        target_event_id: &str,
        sender_pubkey: &str,
        bolt11: Option<&str>,
        created_at: u64,
    ) -> Event {
        let description = serde_json::json!({
            "kind": 9734,
            "pubkey": sender_pubkey,
            "tags": [["p", request_recipient]],
            "content": "",
        })
        .to_string();
        let mut tags = vec![
            Tag::new(vec!["p".into(), payee_pubkey.to_string()]),
            Tag::new(vec!["e".into(), target_event_id.to_string()]),
            Tag::new(vec!["description".into(), description]),
        ];
        if let Some(invoice) = bolt11 {
            tags.push(Tag::new(vec!["bolt11".into(), invoice.to_string()]));
        }
        signer.sign_event(UnsignedEvent {
            pubkey: signer.public_key_bytes(),
            created_at,
            kind: 9735,
            tags,
            content: String::new(),
        })
    }
}
