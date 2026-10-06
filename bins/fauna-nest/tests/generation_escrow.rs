//! The generation escrow doors, end to end through the real nest handlers —
//! R14 (account-data-plane.md § The ratified decisions) build step 4 (`account-data-plane.md` § The generation machinery →
//! *The escrow doors*; wire: `fauna_protocol::generation_escrow`).
//!
//! Tier: tier_3 (real nest handlers + real store — no mocks), the
//! `conformance_account_state_walk.rs` harness shape: an `RpcRequester`
//! dispatching straight into the nest's own handler table. Every assertion is
//! on latency-independent state (e2e convention 14) — no sleeps, nothing to
//! wait for.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::generation::{EscrowReceiptRecord, verify_escrow_receipt};
use fauna_nest::{
    db::CacheDb, generation_escrow_handlers, routes::AppState, rpc_router::RpcRouter,
};
use fauna_protocol::generation_escrow::{
    EscrowDeleteReply, EscrowDeleteRequest, EscrowGetReply, EscrowGetRequest, EscrowPutReply,
    EscrowPutRequest, KIND_ESCROW_DELETE, KIND_ESCROW_GET, KIND_ESCROW_PUT, MAX_ESCROW_WRAP_BYTES,
};
use fauna_protocol::{ByteBuf, RpcError, RpcRequester, encode_canonical};

const ACTOR: [u8; 32] = [0xA7; 32];
const OTHER_ACTOR: [u8; 32] = [0xB8; 32];

/// The nest's deployment identity for these tests — the receipt signer.
fn deployment_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x66; 32])
}

/// Dispatches into the real handler table as `actor`. Same shape as the
/// conformance walk's `RouterRequester`, with the actor a field so the
/// isolation legs can speak as a second account.
struct RouterRequester {
    router: RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
}

#[derive(Debug)]
struct Refused(RpcError);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}", self.0.code, self.0.message)
    }
}

impl RpcRequester for RouterRequester {
    type Error = Refused;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, Refused>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        common::seed_dispatch_actor(&self.state.db, &self.actor).await;
        let meta = self.router.kind_meta(kind).expect("kind registered");
        let bytes = Bytes::from(encode_canonical(&payload).unwrap().to_vec());
        let reply = (meta.handler)(Arc::clone(&self.state), self.actor, bytes)
            .await
            .map_err(Refused)?;
        Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
    }
}

fn build_state(with_deployment_key: bool) -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut state = AppState::for_test(db);
    if with_deployment_key {
        state.nest_signing_key = Some(deployment_key());
    }
    Arc::new(state)
}

fn nest_as(state: &Arc<AppState>, actor: [u8; 32]) -> RouterRequester {
    let mut b = RpcRouter::builder();
    generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
    RouterRequester {
        router: b.build(),
        state: Arc::clone(state),
        actor,
    }
}

fn put_req(generation: [u8; 32], wrap: &[u8]) -> EscrowPutRequest {
    EscrowPutRequest {
        generation_id: ByteBuf::from(generation.to_vec()),
        wrap: ByteBuf::from(wrap.to_vec()),
        target_key: "identity/test".into(),
        ..Default::default()
    }
}

async fn get_all(nest: &RouterRequester) -> EscrowGetReply {
    nest.request(KIND_ESCROW_GET, EscrowGetRequest::default())
        .await
        .expect("get")
}

/// The whole door contract in one flow: deposit → the receipt verifies under
/// the shared holder-generic contract and names the deployment identity +
/// the wrap's BLAKE3 → get serves the bytes back → delete shreds → get is
/// empty and re-delete deletes nothing.
#[tokio::test]
async fn a_deposit_returns_a_verifying_receipt_and_the_wrap_round_trips() {
    let state = build_state(true);
    let nest = nest_as(&state, ACTOR);
    let generation = [0x11; 32];
    let wrap = b"an-xwing-envelope-stand-in".to_vec();

    let put: EscrowPutReply = nest
        .request(KIND_ESCROW_PUT, put_req(generation, &wrap))
        .await
        .expect("put");
    let receipt: EscrowReceiptRecord =
        fauna_core::encoding::canonical_decode(&put.receipt).expect("receipt decodes");
    verify_escrow_receipt(&receipt).expect("receipt verifies holder-generically");
    assert_eq!(
        receipt.holder_id,
        deployment_key().verifying_key().to_bytes(),
        "v1 profile: the holder id IS the deployment identity clients pin"
    );
    assert_eq!(receipt.generation_id, generation);
    assert_eq!(
        receipt.wrap_hash,
        <[u8; 32]>::from(blake3::hash(&wrap)),
        "the receipt binds to this exact ciphertext"
    );
    assert_eq!(
        receipt.target_key, "identity/test",
        "the receipt binds the escrow target the deposit named"
    );

    let got = get_all(&nest).await;
    assert_eq!(got.wraps.len(), 1);
    assert_eq!(got.wraps[0].wrap.as_ref(), wrap.as_slice());
    assert_eq!(got.wraps[0].generation_id.as_ref(), &generation[..]);
    assert_eq!(got.wraps[0].deposited_at_ms, receipt.stamped_at_ms);

    let del: EscrowDeleteReply = nest
        .request(
            KIND_ESCROW_DELETE,
            EscrowDeleteRequest {
                generation_id: ByteBuf::from(generation.to_vec()),
                ..Default::default()
            },
        )
        .await
        .expect("delete");
    assert_eq!(del.deleted, 1);
    assert!(get_all(&nest).await.wraps.is_empty());
    // The shred is idempotent — a replay deletes nothing and does not error.
    let again: EscrowDeleteReply = nest
        .request(
            KIND_ESCROW_DELETE,
            EscrowDeleteRequest {
                generation_id: ByteBuf::from(generation.to_vec()),
                ..Default::default()
            },
        )
        .await
        .expect("re-delete");
    assert_eq!(again.deleted, 0);
}

/// The door's idempotency contract, receipt included: a byte-identical
/// re-deposit stores nothing new and returns a **byte-identical** receipt —
/// so a crash-retrying minter can never mint two receipt variants racing on
/// the immutable `fauna.state.escrow-receipt` plane row.
#[tokio::test]
async fn a_re_deposit_is_idempotent_down_to_the_receipt_bytes() {
    let state = build_state(true);
    let nest = nest_as(&state, ACTOR);
    let generation = [0x22; 32];
    let wrap = vec![0xAB; 64];

    let first: EscrowPutReply = nest
        .request(KIND_ESCROW_PUT, put_req(generation, &wrap))
        .await
        .expect("first put");
    let second: EscrowPutReply = nest
        .request(KIND_ESCROW_PUT, put_req(generation, &wrap))
        .await
        .expect("second put");
    assert_eq!(
        first.receipt, second.receipt,
        "the re-deposit's receipt must be byte-identical (same stamp)"
    );
    assert_eq!(get_all(&nest).await.wraps.len(), 1, "no duplicate row");

    // A DIFFERENT wrap for the same generation is a second deposit, not a
    // replay — redundancy of the same identity-targeted wrap is the multi-
    // holder story, but a re-escrow (succession) legitimately writes new
    // bytes under the same generation.
    let other: EscrowPutReply = nest
        .request(KIND_ESCROW_PUT, put_req(generation, &[0xCD; 64]))
        .await
        .expect("different wrap");
    assert_ne!(first.receipt, other.receipt);
    assert_eq!(get_all(&nest).await.wraps.len(), 2);
}

/// The doors are account-derived: another authenticated account neither sees
/// nor deletes this account's wraps.
#[tokio::test]
async fn another_account_cannot_read_or_shred_these_wraps() {
    let state = build_state(true);
    let nest = nest_as(&state, ACTOR);
    let stranger = nest_as(&state, OTHER_ACTOR);
    let generation = [0x33; 32];

    let _: EscrowPutReply = nest
        .request(KIND_ESCROW_PUT, put_req(generation, b"wrap-bytes"))
        .await
        .expect("put");

    assert!(
        get_all(&stranger).await.wraps.is_empty(),
        "a stranger's get must see nothing"
    );
    let del: EscrowDeleteReply = stranger
        .request(
            KIND_ESCROW_DELETE,
            EscrowDeleteRequest {
                generation_id: ByteBuf::from(generation.to_vec()),
                ..Default::default()
            },
        )
        .await
        .expect("stranger delete dispatches");
    assert_eq!(del.deleted, 0, "and deletes nothing");
    assert_eq!(
        get_all(&nest).await.wraps.len(),
        1,
        "the owner's wrap survives"
    );
}

/// Input hygiene: malformed generation ids, empty and oversized wraps are
/// refused before anything lands; a nest without its deployment identity
/// refuses to act as a holder rather than minting an unverifiable receipt.
#[tokio::test]
async fn malformed_deposits_and_a_keyless_holder_are_refused() {
    let state = build_state(true);
    let nest = nest_as(&state, ACTOR);

    let bad_id: Result<EscrowPutReply, _> = nest
        .request(
            KIND_ESCROW_PUT,
            EscrowPutRequest {
                generation_id: ByteBuf::from(vec![1, 2, 3]),
                wrap: ByteBuf::from(b"x".to_vec()),
                ..Default::default()
            },
        )
        .await;
    assert!(bad_id.is_err());

    let empty: Result<EscrowPutReply, _> = nest
        .request(KIND_ESCROW_PUT, put_req([0x44; 32], b""))
        .await;
    assert!(empty.is_err());

    // No target key: the door cannot sign a receipt that acks for nobody.
    let keyless: Result<EscrowPutReply, _> = nest
        .request(
            KIND_ESCROW_PUT,
            EscrowPutRequest {
                target_key: String::new(),
                ..put_req([0x44; 32], b"wrap")
            },
        )
        .await;
    assert!(
        keyless.is_err(),
        "a deposit naming no escrow target is refused"
    );

    let oversize: Result<EscrowPutReply, _> = nest
        .request(
            KIND_ESCROW_PUT,
            put_req([0x44; 32], &vec![0u8; MAX_ESCROW_WRAP_BYTES + 1]),
        )
        .await;
    assert!(oversize.is_err());
    assert!(get_all(&nest).await.wraps.is_empty(), "nothing landed");

    let keyless_state = build_state(false);
    let keyless = nest_as(&keyless_state, ACTOR);
    let refused: Result<EscrowPutReply, _> = keyless
        .request(KIND_ESCROW_PUT, put_req([0x55; 32], b"wrap"))
        .await;
    let err = refused.expect_err("a holder with no identity must refuse");
    assert!(err.to_string().contains("holder_unavailable"), "got: {err}");
}

/// A tampered receipt — any signed field — fails the shared verification, so
/// a depositor can never be handed a receipt that claims a different deposit
/// than the one it made.
#[tokio::test]
async fn a_tampered_receipt_fails_verification() {
    let state = build_state(true);
    let nest = nest_as(&state, ACTOR);
    let put: EscrowPutReply = nest
        .request(KIND_ESCROW_PUT, put_req([0x66; 32], b"wrap-bytes"))
        .await
        .expect("put");
    let receipt: EscrowReceiptRecord =
        fauna_core::encoding::canonical_decode(&put.receipt).expect("receipt decodes");

    let mut wrong_generation = receipt.clone();
    wrong_generation.generation_id[0] ^= 1;
    assert!(verify_escrow_receipt(&wrong_generation).is_err());

    let mut wrong_hash = receipt.clone();
    wrong_hash.wrap_hash[0] ^= 1;
    assert!(verify_escrow_receipt(&wrong_hash).is_err());

    let mut wrong_stamp = receipt;
    wrong_stamp.stamped_at_ms += 1;
    assert!(verify_escrow_receipt(&wrong_stamp).is_err());
}

// ── The mint sequence over the real doors (R14 build step 5) ─────────────────
//
// `fauna_sync_engine::generation_mint::mint_generation` deposits through the
// REAL `fauna.generation.escrow.put` handler here — real persistence, real
// deployment-identity receipt — and the whole distribution story is proven on
// the stored bytes: a second fleet member unwraps its inline wrap from the
// staged mint row, and a recovery ceremony (seed → escrow secret → the real
// `get` door) recovers the same key. The machinery entries stage door-lessly
// (the sanctioned pattern — the R14 boolean writer-door gate stands through
// step 5; step 6's tip resolution is what admits them in production).

/// The account's identity line for the mint-sequence leg.
fn fleet_root() -> fauna_core::identity::ActorKeypair {
    fauna_core::identity::ActorKeypair::from_secret([0x77; 32])
}

/// The identity seed a seed-holding surface derives the escrow target from —
/// `fleet_root()`'s own, since the target row is keyed by the actor the seed
/// derives.
const IDENTITY_SEED: [u8; 32] = [0x77; 32];

fn enrollment_entry(device: &ed25519_dalek::SigningKey) -> fauna_account_store::types::StateEntry {
    let id = device.verifying_key().to_bytes();
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    let cert = DeviceAuthorization {
        actor_id: fleet_root().actor_id(),
        device_key: id,
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = fauna_core::encoding::sign_envelope(&fleet_root(), &cert).unwrap();
    let authorization = fauna_core::encoding::canonical_encode(
        &fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env),
    )
    .unwrap();
    fauna_account_store::types::StateEntry {
        kind: fauna_protocol::merge_policy::KIND_DEVICE_SET.into(),
        key: fauna_core::hex32::encode(&id),
        scope: fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE.into(),
        // Production's own shape: self-signed, the KEM half derived from the
        // secret that signs.
        value: fauna_core::encoding::canonical_encode(
            &fauna_core::generation::sign_device_enrollment(device, authorization, 5_000),
        )
        .unwrap(),
        merge_meta: None,
        entry_version: 0,
        tombstone: false,
    }
}

/// The full step-5 chain: staged fleet + published target → the engine's
/// deposit-first sequence through the real put door → the deployment-signed
/// receipt is trusted and bound → the staged mint row hands the key to a
/// second device (commitment verified) → the real get door + the seed-derived
/// escrow secret recover the same key — under the identity's own target key,
/// the one string the wrap's AAD, the receipt and the target row all name.
#[tokio::test]
async fn the_mint_sequence_escrows_through_the_real_doors_and_recovers() {
    use fauna_account_store::{sqlite::SqliteBackend, store::AccountStore, types::WriterId};
    use fauna_mls::wrapped_blob::generation_wraps::{
        open_generation_key_as_device, open_generation_key_from_escrow,
    };
    use fauna_sync_engine::generation_mint::{MintContext, escrow_target_entry, mint_generation};

    let state = build_state(true);
    let nest = nest_as(&state, ACTOR);

    // Real device keys: `[seed; 32]` is the Ed25519 secret, the device id its
    // public half, and the device KEM keypair derives from the secret bytes —
    // the production shape (the mint is minter-signed since ST-007).
    let a_key = ed25519_dalek::SigningKey::from_bytes(&[0x0A; 32]);
    let (a, b) = (
        a_key.verifying_key().to_bytes(),
        ed25519_dalek::SigningKey::from_bytes(&[0x0B; 32])
            .verifying_key()
            .to_bytes(),
    );
    let store = AccountStore::open(
        SqliteBackend::open_in_memory().unwrap(),
        &fleet_root().actor_id_hex(),
        WriterId(a),
    )
    .await
    .unwrap();
    store.put_state(enrollment_entry(&a_key)).await.unwrap();
    store
        .put_state(enrollment_entry(&ed25519_dalek::SigningKey::from_bytes(
            &[0x0B; 32],
        )))
        .await
        .unwrap();
    store
        .put_state(escrow_target_entry(&IDENTITY_SEED).unwrap())
        .await
        .unwrap();

    let root_id = fleet_root().actor_id();
    let trusted = [deployment_key().verifying_key().to_bytes()];
    let minted = mint_generation(
        &store,
        &nest,
        &MintContext {
            root: &root_id,
            minter_key: &a_key,
            trusted_holders: &trusted,
        },
        vec![],
        9_000,
    )
    .await
    .expect("the sequence completes against the real door");
    assert_eq!(
        minted.receipt.holder_id,
        deployment_key().verifying_key().to_bytes(),
        "v1 profile: the receipt names the deployment identity"
    );

    // Stage the two returned rows door-lessly and read the mint row BACK —
    // distribution is proven on stored bytes, not in-memory values.
    store.put_state(minted.mint_entry.clone()).await.unwrap();
    store.put_state(minted.receipt_entry.clone()).await.unwrap();
    let stored = store
        .state(
            fauna_protocol::merge_policy::KIND_GENERATION_MINT,
            &fauna_core::hex32::encode(&minted.generation_id),
        )
        .await
        .unwrap()
        .expect("the mint row persisted");
    let record: fauna_core::generation::GenerationMintRecord =
        fauna_core::encoding::canonical_decode(&stored.value).unwrap();
    let fauna_core::generation::GenerationMintRecord::Minted { core, wraps, .. } = record else {
        panic!("a fresh mint is Minted");
    };

    // Device B — enrolled, not the minter — finds its inline wrap and
    // recovers the key, commitment verified against the stored core.
    let b_wrap = &wraps
        .iter()
        .find(|w| w.device_id == b)
        .expect("every member has an inline wrap")
        .wrap;
    let b_key = open_generation_key_as_device(
        b_wrap,
        &fauna_core::generation::derive_device_xwing_keypair(&[0x0B; 32]).secret,
        &minted.generation_id,
        &b,
        &core.key_commitment,
    )
    .expect("a fleet member opens its own wrap");
    assert_eq!(b_key.as_bytes(), minted.gen_key.as_bytes());

    // The recovery ceremony: the real get door serves the deposited wrap
    // back, and the seed-derived escrow secret opens it.
    let got = get_all(&nest).await;
    assert_eq!(got.wraps.len(), 1);
    assert_eq!(
        got.wraps[0].generation_id.as_ref(),
        &minted.generation_id[..]
    );
    let recovered = open_generation_key_from_escrow(
        &got.wraps[0].wrap,
        &fauna_core::generation::derive_escrow_xwing_keypair(&IDENTITY_SEED).secret,
        &minted.generation_id,
        &fauna_protocol::merge_policy::escrow_target_identity_key(&fleet_root().actor_id()),
        &core.key_commitment,
    )
    .expect("seed ceremony → escrow secret → the served wrap opens");
    assert_eq!(recovered.as_bytes(), minted.gen_key.as_bytes());
}

/// The kept wrap at the doors (`owner-key-material.md` § Path A-sibling-2 →
/// *Rotation*, the succession rider → *The kept wrap*), over a two-hop chain
/// the nest recorded: every retired identity's wraps are served to the
/// current one; a stranger's are not; the current identity's deposit of a
/// generation sweeps every chain predecessor's wrap of that generation and
/// nothing else; and its delete takes a generation across the chain.
#[tokio::test]
async fn the_doors_serve_and_sweep_a_successions_kept_wraps() {
    let state = build_state(true);
    let (first, middle, current) = ([0xA1u8; 32], [0xA2u8; 32], ACTOR);
    let (g1, g2, g3) = ([0x31u8; 32], [0x32u8; 32], [0x33u8; 32]);
    async fn put(state: &Arc<AppState>, actor: [u8; 32], generation: [u8; 32], wrap: &[u8]) {
        let _: EscrowPutReply = nest_as(state, actor)
            .request(KIND_ESCROW_PUT, put_req(generation, wrap))
            .await
            .expect("put");
    }
    fn served(reply: EscrowGetReply) -> Vec<Vec<u8>> {
        let mut w: Vec<Vec<u8>> = reply.wraps.into_iter().map(|w| w.wrap.into_vec()).collect();
        w.sort();
        w
    }
    put(&state, first, g1, b"first-g1").await;
    put(&state, first, g2, b"first-g2").await;
    state
        .db
        .record_succession(&first, &middle, b"s", 1)
        .await
        .unwrap()
        .unwrap();
    put(&state, middle, g3, b"middle-g3").await;
    state
        .db
        .record_succession(&middle, &current, b"s", 2)
        .await
        .unwrap()
        .unwrap();
    put(&state, OTHER_ACTOR, g1, b"stranger-g1").await;

    let nest = nest_as(&state, current);
    assert_eq!(
        served(get_all(&nest).await),
        vec![
            b"first-g1".to_vec(),
            b"first-g2".to_vec(),
            b"middle-g3".to_vec()
        ],
        "the current identity is served the whole chain's kept wraps, no stranger's"
    );
    let only_g1: EscrowGetReply = nest
        .request(
            KIND_ESCROW_GET,
            EscrowGetRequest {
                generation_id: Some(ByteBuf::from(g1.to_vec())),
                ..Default::default()
            },
        )
        .await
        .expect("filtered get");
    assert_eq!(served(only_g1), vec![b"first-g1".to_vec()]);

    // The deposit the kept wrap waited for sweeps it — two hops back.
    put(&state, current, g1, b"current-g1").await;
    assert_eq!(
        served(get_all(&nest).await),
        vec![
            b"current-g1".to_vec(),
            b"first-g2".to_vec(),
            b"middle-g3".to_vec()
        ],
    );
    assert_eq!(
        served(get_all(&nest_as(&state, OTHER_ACTOR)).await),
        vec![b"stranger-g1".to_vec()],
        "a stranger's wrap of the same generation is not the chain's"
    );

    // Delete takes a generation across the chain.
    let del: EscrowDeleteReply = nest
        .request(
            KIND_ESCROW_DELETE,
            EscrowDeleteRequest {
                generation_id: ByteBuf::from(g3.to_vec()),
                ..Default::default()
            },
        )
        .await
        .expect("delete");
    assert_eq!(del.deleted, 1);
    assert_eq!(
        served(get_all(&nest).await),
        vec![b"current-g1".to_vec(), b"first-g2".to_vec()],
    );
}
