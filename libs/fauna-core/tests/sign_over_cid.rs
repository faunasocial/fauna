//! Sign-over-CID API tests covering every in-tree signed Fauna kind.
//!
//! Task 2.4 introduced the new `Signed` trait + `sign_envelope` /
//! `verify_envelope` functions and added the proof-of-concept Profile
//! tests. Task 2.5 dropped the `signature: Vec<u8>` field from `Profile`
//! and `Post`. Task 2.6 finishes the migration for `DeliveryReceipt`,
//! `ContactRequest`, `DeviceAuthorization`, `Tombstone`,
//! and `KeyBlob`.
//!
//! Each kind gets four cases:
//! 1. positive roundtrip — `sign_envelope` + `verify_envelope` succeed,
//! 2. tampered bytes — CID mismatch surfaces as `CidMismatch`,
//! 3. tampered envelope signature — `SignatureInvalid`,
//! 4. wrong pubkey — `SignatureInvalid` (model by swapping the
//!    `signer_public_key()` field in a clone of the value).

use ed25519_dalek::SigningKey;
use fauna_core::data::{
    Capability, ContactRequest, DeliveryReceipt, DeviceAuthorization, InboxMode, Post, PostBody,
    Profile, Timestamp, Tombstone,
};
use fauna_core::encoding::{
    EmbedAsBytes, sign_envelope, verify_authoring_envelope, verify_envelope,
};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::types::{KemSuiteId, KeyBlob, KeyBlobEntry};

fn fixed_keypair() -> ActorKeypair {
    // Same seed used by fauna_cbor's envelope tests.
    ActorKeypair::from_secret([7u8; 32])
}

fn profile_for(kp: &ActorKeypair) -> Profile {
    Profile {
        actor_id: kp.actor_id(),
        display_name: Some("alice".into()),
        bio: None,
        avatar: None,
        banner: None,
        links: vec![],
        nests: vec![],
        admin_nests: vec![],
        load_hint: None,
        inbox_mode: InboxMode::default(),
        recovery_head: None,
        updated_at: Timestamp(1_700_000_000),
    }
}

fn post_for(kp: &ActorKeypair) -> Post {
    Post {
        author: kp.actor_id(),
        created_at: Timestamp(1_700_000_000),
        body: PostBody::Text {
            content: "hello fauna".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    }
}

// ── Profile ──────────────────────────────────────────────────────────

#[test]
fn profile_sign_verify_roundtrip() {
    let kp = fixed_keypair();
    let profile = profile_for(&kp);

    let (bytes, env) = sign_envelope(&kp, &profile).expect("sign");
    verify_envelope(&profile, &bytes, &env).expect("verify");
}

#[test]
fn profile_verify_rejects_tampered_bytes() {
    let kp = fixed_keypair();
    let profile = profile_for(&kp);

    let (mut bytes, env) = sign_envelope(&kp, &profile).expect("sign");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;

    let err =
        verify_envelope(&profile, &bytes, &env).expect_err("tampered bytes must fail verification");
    assert!(
        err.to_string().contains("CidMismatch"),
        "expected CidMismatch, got: {err}"
    );
}

#[test]
fn profile_verify_rejects_tampered_sig() {
    let kp = fixed_keypair();
    let profile = profile_for(&kp);

    let (bytes, mut env) = sign_envelope(&kp, &profile).expect("sign");
    env.sig_mut()[0] ^= 0xff;

    let err = verify_envelope(&profile, &bytes, &env)
        .expect_err("tampered signature must fail verification");
    assert!(
        err.to_string().contains("SignatureInvalid"),
        "expected SignatureInvalid, got: {err}"
    );
}

#[test]
fn profile_verify_rejects_wrong_pubkey() {
    let kp = fixed_keypair();
    let profile = profile_for(&kp);

    let (bytes, env) = sign_envelope(&kp, &profile).expect("sign");

    // Build a Profile carrying a DIFFERENT signer pubkey. Since the new
    // API reads the expected pubkey from `value.signer_public_key()`,
    // swapping out actor_id is the way to model "wrong pubkey".
    let other_pk = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
    let mut wrong = profile.clone();
    wrong.actor_id = ActorId(other_pk.to_bytes());

    let err =
        verify_envelope(&wrong, &bytes, &env).expect_err("wrong pubkey must fail verification");
    assert!(
        err.to_string().contains("SignatureInvalid"),
        "expected SignatureInvalid, got: {err}"
    );
}

// ── Post ─────────────────────────────────────────────────────────────

#[test]
fn post_sign_verify_roundtrip() {
    let kp = fixed_keypair();
    let post = post_for(&kp);

    let (bytes, env) = sign_envelope(&kp, &post).expect("sign");
    verify_envelope(&post, &bytes, &env).expect("verify");
}

#[test]
fn post_verify_rejects_tampered_bytes() {
    let kp = fixed_keypair();
    let post = post_for(&kp);

    let (mut bytes, env) = sign_envelope(&kp, &post).expect("sign");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;

    let err =
        verify_envelope(&post, &bytes, &env).expect_err("tampered bytes must fail verification");
    assert!(
        err.to_string().contains("CidMismatch"),
        "expected CidMismatch, got: {err}"
    );
}

#[test]
fn post_verify_rejects_tampered_sig() {
    let kp = fixed_keypair();
    let post = post_for(&kp);

    let (bytes, mut env) = sign_envelope(&kp, &post).expect("sign");
    env.sig_mut()[0] ^= 0xff;

    let err = verify_envelope(&post, &bytes, &env)
        .expect_err("tampered signature must fail verification");
    assert!(
        err.to_string().contains("SignatureInvalid"),
        "expected SignatureInvalid, got: {err}"
    );
}

#[test]
fn post_verify_rejects_wrong_pubkey() {
    let kp = fixed_keypair();
    let post = post_for(&kp);

    let (bytes, env) = sign_envelope(&kp, &post).expect("sign");

    // Swap out the author to model "wrong pubkey" — Signed reads the
    // expected pubkey from the value.
    let other_pk = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
    let mut wrong = post.clone();
    wrong.author = ActorId(other_pk.to_bytes());

    let err =
        verify_envelope(&wrong, &bytes, &env).expect_err("wrong pubkey must fail verification");
    assert!(
        err.to_string().contains("SignatureInvalid"),
        "expected SignatureInvalid, got: {err}"
    );
}

// ── Shared helper for the Task 2.6 kinds ─────────────────────────────

fn other_actor_id() -> ActorId {
    let pk = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
    ActorId(pk.to_bytes())
}

// ── DeliveryReceipt ──────────────────────────────────────────────────

fn delivery_receipt_for(kp: &ActorKeypair) -> DeliveryReceipt {
    DeliveryReceipt {
        post_id: fauna_cbor::Cid::of_dag_cbor(b"delivery-receipt-fixture-9"),
        recipient: kp.actor_id(),
        received_at: Timestamp(1_700_000_000),
    }
}

#[test]
fn delivery_receipt_sign_verify_roundtrip() {
    let kp = fixed_keypair();
    let receipt = delivery_receipt_for(&kp);
    let (bytes, env) = sign_envelope(&kp, &receipt).expect("sign");
    verify_envelope(&receipt, &bytes, &env).expect("verify");
}

#[test]
fn delivery_receipt_verify_rejects_tampered_bytes() {
    let kp = fixed_keypair();
    let receipt = delivery_receipt_for(&kp);
    let (mut bytes, env) = sign_envelope(&kp, &receipt).expect("sign");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    let err = verify_envelope(&receipt, &bytes, &env).expect_err("tampered bytes");
    assert!(err.to_string().contains("CidMismatch"));
}

#[test]
fn delivery_receipt_verify_rejects_tampered_sig() {
    let kp = fixed_keypair();
    let receipt = delivery_receipt_for(&kp);
    let (bytes, mut env) = sign_envelope(&kp, &receipt).expect("sign");
    env.sig_mut()[0] ^= 0xff;
    let err = verify_envelope(&receipt, &bytes, &env).expect_err("tampered sig");
    assert!(err.to_string().contains("SignatureInvalid"));
}

#[test]
fn delivery_receipt_verify_rejects_wrong_pubkey() {
    let kp = fixed_keypair();
    let receipt = delivery_receipt_for(&kp);
    let mut wrong = receipt.clone();
    wrong.recipient = other_actor_id();
    let (bytes, env) = sign_envelope(&kp, &receipt).expect("sign");
    let err = verify_envelope(&wrong, &bytes, &env).expect_err("wrong pk");
    assert!(err.to_string().contains("SignatureInvalid"));
}

// ── ContactRequest ───────────────────────────────────────────────────

fn contact_request_for(kp: &ActorKeypair) -> ContactRequest {
    ContactRequest {
        sender: kp.actor_id(),
        post_id: fauna_cbor::Cid::of_dag_cbor(b"contact-request-fixture-7"),
        sender_node: b"http://localhost:3000".to_vec(),
        summary: "test".into(),
        created_at: Timestamp(1_700_000_000),
    }
}

#[test]
fn contact_request_sign_verify_roundtrip() {
    let kp = fixed_keypair();
    let cr = contact_request_for(&kp);
    let (bytes, env) = sign_envelope(&kp, &cr).expect("sign");
    verify_envelope(&cr, &bytes, &env).expect("verify");
}

#[test]
fn contact_request_verify_rejects_tampered_bytes() {
    // Already covered by the matrix above; keep the per-case test alive so
    // grep'ing for the spec name (per-kind × 4 cases) finds something.
    let kp = fixed_keypair();
    let cr = contact_request_for(&kp);
    let (mut bytes, env) = sign_envelope(&kp, &cr).expect("sign");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    let err = verify_envelope(&cr, &bytes, &env).expect_err("tampered bytes");
    assert!(err.to_string().contains("CidMismatch"));
}

#[test]
fn contact_request_verify_rejects_tampered_sig() {
    let kp = fixed_keypair();
    let cr = contact_request_for(&kp);
    let (bytes, mut env) = sign_envelope(&kp, &cr).expect("sign");
    env.sig_mut()[0] ^= 0xff;
    let err = verify_envelope(&cr, &bytes, &env).expect_err("tampered sig");
    assert!(err.to_string().contains("SignatureInvalid"));
}

#[test]
fn contact_request_verify_rejects_wrong_pubkey() {
    let kp = fixed_keypair();
    let cr = contact_request_for(&kp);
    let mut wrong = cr.clone();
    wrong.sender = other_actor_id();
    let (bytes, env) = sign_envelope(&kp, &cr).expect("sign");
    let err = verify_envelope(&wrong, &bytes, &env).expect_err("wrong pk");
    assert!(err.to_string().contains("SignatureInvalid"));
}

// ── DeviceAuthorization ─────────────────────────────────────────────

fn device_auth_for(kp: &ActorKeypair) -> DeviceAuthorization {
    DeviceAuthorization {
        actor_id: kp.actor_id(),
        device_key: [0x11u8; 32],
        capabilities: vec![Capability::ManageSubscribers],
        created_at: Timestamp(1_700_000_000),
        expires_at: None,
    }
}

#[test]
fn device_authorization_sign_verify_roundtrip() {
    let kp = fixed_keypair();
    let auth = device_auth_for(&kp);
    let (bytes, env) = sign_envelope(&kp, &auth).expect("sign");
    verify_envelope(&auth, &bytes, &env).expect("verify");
}

#[test]
fn device_authorization_verify_rejects_tampered_bytes() {
    let kp = fixed_keypair();
    let auth = device_auth_for(&kp);
    let (mut bytes, env) = sign_envelope(&kp, &auth).expect("sign");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    let err = verify_envelope(&auth, &bytes, &env).expect_err("tampered bytes");
    assert!(err.to_string().contains("CidMismatch"));
}

#[test]
fn device_authorization_verify_rejects_tampered_sig() {
    let kp = fixed_keypair();
    let auth = device_auth_for(&kp);
    let (bytes, mut env) = sign_envelope(&kp, &auth).expect("sign");
    env.sig_mut()[0] ^= 0xff;
    let err = verify_envelope(&auth, &bytes, &env).expect_err("tampered sig");
    assert!(err.to_string().contains("SignatureInvalid"));
}

#[test]
fn device_authorization_verify_rejects_wrong_pubkey() {
    let kp = fixed_keypair();
    let auth = device_auth_for(&kp);
    let mut wrong = auth.clone();
    wrong.actor_id = other_actor_id();
    let (bytes, env) = sign_envelope(&kp, &auth).expect("sign");
    let err = verify_envelope(&wrong, &bytes, &env).expect_err("wrong pk");
    assert!(err.to_string().contains("SignatureInvalid"));
}

// ── Tombstone ────────────────────────────────────────────────────────

fn tombstone_for(kp: &ActorKeypair) -> Tombstone {
    Tombstone {
        author: kp.actor_id(),
        post_id: fauna_cbor::Cid::of_dag_cbor(b"tombstone-fixture-3"),
        created_at: Timestamp(1_700_000_000),
    }
}

#[test]
fn tombstone_sign_verify_roundtrip() {
    let kp = fixed_keypair();
    let tombstone = tombstone_for(&kp);
    let (bytes, env) = sign_envelope(&kp, &tombstone).expect("sign");
    verify_envelope(&tombstone, &bytes, &env).expect("verify");
}

#[test]
fn tombstone_verify_rejects_tampered_bytes() {
    let kp = fixed_keypair();
    let tombstone = tombstone_for(&kp);
    let (mut bytes, env) = sign_envelope(&kp, &tombstone).expect("sign");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    let err = verify_envelope(&tombstone, &bytes, &env).expect_err("tampered bytes");
    assert!(err.to_string().contains("CidMismatch"));
}

#[test]
fn tombstone_verify_rejects_tampered_sig() {
    let kp = fixed_keypair();
    let tombstone = tombstone_for(&kp);
    let (bytes, mut env) = sign_envelope(&kp, &tombstone).expect("sign");
    env.sig_mut()[0] ^= 0xff;
    let err = verify_envelope(&tombstone, &bytes, &env).expect_err("tampered sig");
    assert!(err.to_string().contains("SignatureInvalid"));
}

#[test]
fn tombstone_verify_rejects_wrong_pubkey() {
    let kp = fixed_keypair();
    let tombstone = tombstone_for(&kp);
    let mut wrong = tombstone.clone();
    wrong.author = other_actor_id();
    let (bytes, env) = sign_envelope(&kp, &tombstone).expect("sign");
    let err = verify_envelope(&wrong, &bytes, &env).expect_err("wrong pk");
    assert!(err.to_string().contains("SignatureInvalid"));
}

// ── KeyBlob ─────────────────────────────────────────────────────────

fn key_blob_for(kp: &ActorKeypair) -> KeyBlob {
    KeyBlob {
        author: kp.actor_id(),
        tier: "tier1".into(),
        rotated_at: Timestamp(1_700_000_000),
        entries: vec![KeyBlobEntry {
            subscriber: kp.actor_id(),
            encrypted_key: vec![0u8; 92],
            suite: KemSuiteId::Classical,
        }],
        signer: kp.actor_id().0,
        key_commitment: [0x6b; 32],
    }
}

#[test]
fn key_blob_sign_verify_roundtrip() {
    let kp = fixed_keypair();
    let blob = key_blob_for(&kp);
    let (bytes, env) = sign_envelope(&kp, &blob).expect("sign");
    verify_envelope(&blob, &bytes, &env).expect("verify");
}

#[test]
fn key_blob_verify_rejects_tampered_bytes() {
    let kp = fixed_keypair();
    let blob = key_blob_for(&kp);
    let (mut bytes, env) = sign_envelope(&kp, &blob).expect("sign");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    let err = verify_envelope(&blob, &bytes, &env).expect_err("tampered bytes");
    assert!(err.to_string().contains("CidMismatch"));
}

#[test]
fn key_blob_verify_rejects_tampered_sig() {
    let kp = fixed_keypair();
    let blob = key_blob_for(&kp);
    let (bytes, mut env) = sign_envelope(&kp, &blob).expect("sign");
    env.sig_mut()[0] ^= 0xff;
    let err = verify_envelope(&blob, &bytes, &env).expect_err("tampered sig");
    assert!(err.to_string().contains("SignatureInvalid"));
}

#[test]
fn key_blob_verify_rejects_wrong_pubkey() {
    let kp = fixed_keypair();
    let blob = key_blob_for(&kp);
    let mut wrong = blob.clone();
    let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
    wrong.signer = other.to_bytes();
    let (bytes, env) = sign_envelope(&kp, &blob).expect("sign");
    let err = verify_envelope(&wrong, &bytes, &env).expect_err("wrong pk");
    assert!(err.to_string().contains("SignatureInvalid"));
}

// ── The weak-key class at the shared sign-over-CID door ──────────────
//
// `verify_envelope` reads the verifying key out of the very payload it is
// checking (`Signed::signer_public_key`), so the key is **wire-supplied and
// attacker-chosen** at every one of its production call sites — the exact
// "prove you hold the key you name" shape that
// `docs/goal/architecture/security.md` § Key material and signature
// verification requires `verify_strict` + a weak-key refusal for. These two
// probes pin the refusal at both of the crate's `SignedEnvelope::verify_permissive` call
// sites (`encoding.rs` — plain and delegated), which are the only two in the
// tree: every other signed-kind verification reaches the primitive through
// them.

/// PROBE-381-A — a small-order author key with an all-zero signature must
/// never author anything.
///
/// The permissive `ed25519_dalek::Verifier` accepts `(A=small-order, sig=0)`
/// for a large fraction of messages, so an attacker who is free to vary the
/// payload gets acceptance on demand. The forged author is the all-zero key —
/// an identity **nobody holds a key to** — which is the same harm as: authorship attributed to an unheld identity.
#[test]
fn probe_381_a_a_small_order_author_never_authors_a_post() {
    let mut accepted = 0usize;
    for n in 0..256u32 {
        let mut post = post_for(&fixed_keypair());
        post.author = ActorId([0u8; 32]); // nobody holds this key
        post.body = PostBody::Text {
            content: format!("forged-{n}"),
            facets: vec![],
        };
        let bytes = fauna_cbor::encode_canonical(&post).expect("encode");
        let env = fauna_cbor::SignedEnvelope::from_parts(
            fauna_cbor::Cid::of_dag_cbor(&bytes),
            [0u8; 64], // the all-zero signature
        );
        if verify_envelope(&post, &bytes, &env).is_ok() {
            accepted += 1;
        }
    }
    assert_eq!(
        accepted, 0,
        "{accepted}/256 forged posts verified under a small-order author key"
    );
}

/// PROBE-381-B — the delegated arm's step 6 runs the same refusal.
///
/// The cert here is honestly author-signed; only its `device_key` is the
/// unheld all-zero key. Step 6 must refuse rather than hand back
/// `AuthoringOrigin::Delegated` for a delegation nobody can exercise.
#[test]
fn probe_381_b_a_small_order_device_key_never_carries_a_delegation() {
    let kp = fixed_keypair();
    let mut accepted = 0usize;
    for n in 0..256u32 {
        let cert = DeviceAuthorization {
            actor_id: kp.actor_id(),
            device_key: [0u8; 32], // nobody holds this key
            capabilities: vec![Capability::All],
            created_at: Timestamp(1_700_000_000),
            expires_at: None,
        };
        let (cert_bytes, cert_env) = sign_envelope(&kp, &cert).expect("sign cert");
        let cert_wire = EmbedAsBytes::from_signed(cert_bytes, cert_env);

        let mut post = post_for(&kp);
        post.body = PostBody::Text {
            content: format!("delegated-forgery-{n}"),
            facets: vec![],
        };
        let bytes = fauna_cbor::encode_canonical(&post).expect("encode");
        let env = fauna_cbor::SignedEnvelope::from_parts(
            fauna_cbor::Cid::of_dag_cbor(&bytes),
            [0u8; 64], // the all-zero signature
        );
        if verify_authoring_envelope(
            &post,
            &bytes,
            &env,
            Some(&cert_wire),
            &Capability::All,
            Timestamp(1_700_000_001),
        )
        .is_ok()
        {
            accepted += 1;
        }
    }
    assert_eq!(
        accepted, 0,
        "{accepted}/256 forged posts verified under a small-order delegated device key"
    );
}
