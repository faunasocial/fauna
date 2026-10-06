//! Round-trip tests for the seal-side wrapped-blob FFI exports added in
//! I3 Phase A.2. The bridge-side unseal exports are tested by
//! `libs/fauna-mls/tests/wrapped_blob_conformance.rs`; this file
//! exercises the seal/encode path Swift / Kotlin / C# / Go / WASM
//! callers will use to provision blobs through nest's
//! `fauna.bridges.provision_*` RPCs.
//!
//! Argon2id parameters are deliberately small (m = 4096, t = 1, p = 1)
//! so the suite finishes in milliseconds. The library-default
//! `Argon2id Interactive` (m = 65_536, t = 2) is exercised once via the
//! `kdf_params: None` path.

use fauna_ffi::*;

fn small_argon2id() -> KdfParamsFfi {
    KdfParamsFfi {
        alg: "argon2id".into(),
        argon2_m_kib: Some(4096),
        argon2_t: Some(1),
        argon2_p: Some(1),
    }
}

fn hkdf() -> KdfParamsFfi {
    KdfParamsFfi {
        alg: "hkdf-sha256".into(),
        argon2_m_kib: None,
        argon2_t: None,
        argon2_p: None,
    }
}

#[test]
fn wrapped_msek_round_trip_plain() {
    let msek = vec![0x42u8; 32];
    let actor_id = vec![0x01u8; 32];
    let password = b"correct horse battery staple".to_vec();

    let blob_bytes = seal_wrapped_msek_blob(
        msek.clone(),
        actor_id.clone(),
        "default".into(),
        "plain".into(),
        password.clone(),
        Some(small_argon2id()),
    )
    .expect("seal");

    let recovered = unseal_wrapped_msek_blob(blob_bytes, "plain".into(), password).expect("unseal");
    assert_eq!(recovered, msek);
}

#[test]
fn wrapped_msek_round_trip_oauthbearer() {
    let msek = vec![0xCDu8; 32];
    let actor_id = vec![0x02u8; 32];
    let token = b"high-entropy-bearer-token-128-bits-or-more".to_vec();

    let blob_bytes = seal_wrapped_msek_blob(
        msek.clone(),
        actor_id,
        "iphone-mail".into(),
        "oauthbearer".into(),
        token.clone(),
        Some(hkdf()),
    )
    .expect("seal");

    let recovered =
        unseal_wrapped_msek_blob(blob_bytes, "oauthbearer".into(), token).expect("unseal");
    assert_eq!(recovered, msek);
}

#[test]
fn wrapped_msek_default_kdf_picks_argon2id_interactive_for_plain() {
    // None on PLAIN must select Argon2id with the library default
    // (Interactive: m = 65_536 KiB, t = 2, p = 1) per
    // `docs/goal/behavior/mail-credentials.md` § KDF choice. This
    // single test exercises the slow path; the others use small
    // params to stay fast.
    let msek = vec![0x55u8; 32];
    let actor_id = vec![0x77u8; 32];
    let password = b"defaults".to_vec();
    let blob_bytes = seal_wrapped_msek_blob(
        msek.clone(),
        actor_id,
        "default".into(),
        "plain".into(),
        password.clone(),
        None,
    )
    .expect("seal");
    let recovered = unseal_wrapped_msek_blob(blob_bytes, "plain".into(), password).expect("unseal");
    assert_eq!(recovered, msek);
}

#[test]
fn wrapped_msek_wrong_credential_returns_aead_error() {
    let msek = vec![0u8; 32];
    let actor_id = vec![0u8; 32];
    let blob_bytes = seal_wrapped_msek_blob(
        msek,
        actor_id,
        "default".into(),
        "plain".into(),
        b"correct".to_vec(),
        Some(small_argon2id()),
    )
    .expect("seal");
    let err = unseal_wrapped_msek_blob(blob_bytes, "plain".into(), b"wrong".to_vec())
        .expect_err("must fail AEAD");
    // FfiError::General carries the inner error message; AEAD failure
    // surfaces as "AeadFailed" in the message per the inner UnwrapError
    // Display impl.
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("aead"),
        "expected AEAD failure, got: {msg}"
    );
}

// The unseal legs below go through the inner
// `fauna_mls::wrapped_blob::unseal_mls_snapshot` — there is deliberately
// NO `unseal_mls_snapshot_blob` FFI export (deleted 2026-07-15: a bare-MSEK unseal with no Go/client
// caller; the MDA opens via `MlsCapability::decrypt`). The seal leg stays
// on the export, which is the ratified test-minting surface.

#[test]
fn mls_snapshot_round_trip() {
    use fauna_mls::wrapped_blob::{MlsSnapshotBlob, unseal_mls_snapshot};

    let msek = [0x55u8; 32];
    let actor_id = vec![0x77u8; 32];
    let state = b"serialized read-only MLS state bytes (test fixture)".to_vec();

    let blob_bytes = seal_mls_snapshot_blob(state.clone(), actor_id, msek.to_vec()).expect("seal");
    let blob = MlsSnapshotBlob::from_canonical_bytes(&blob_bytes).expect("decode");
    let recovered = unseal_mls_snapshot(&blob, &msek).expect("unseal");
    assert_eq!(recovered.as_slice(), state.as_slice());
}

#[test]
fn mls_snapshot_wrong_msek_returns_aead_error() {
    use fauna_mls::wrapped_blob::{MlsSnapshotBlob, unseal_mls_snapshot};

    let msek = vec![0x55u8; 32];
    let wrong = [0x66u8; 32];
    let actor_id = vec![0x77u8; 32];
    let blob_bytes = seal_mls_snapshot_blob(b"x".to_vec(), actor_id, msek).expect("seal");
    let blob = MlsSnapshotBlob::from_canonical_bytes(&blob_bytes).expect("decode");
    let err = unseal_mls_snapshot(&blob, &wrong).expect_err("must fail");
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("aead"),
        "expected AEAD failure, got: {msg}"
    );
}

#[test]
fn submission_token_round_trip_via_inner_unseal() {
    // The submission-token unseal needs a verifying pubkey, which we
    // don't expose through FFI in Phase A (the bridge consumes the
    // wrapped token via its own surface). Verify the seal output by
    // round-tripping through `fauna_mls::wrapped_blob::unseal_submission_token`
    // directly.
    use ed25519_dalek::SigningKey;
    use fauna_mls::wrapped_blob::{
        CredentialInput, SIGNATURE_LEN, SubmissionToken, WrappedSubmissionTokenBlob,
        unseal_submission_token,
    };
    use serde_bytes::ByteBuf;

    let mut sk_bytes = [0u8; 32];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut sk_bytes);
    let sk = SigningKey::from_bytes(&sk_bytes);
    let vk = sk.verifying_key();

    let actor_id = vec![0x33u8; 32];
    let credential_id = "default";
    let token = SubmissionToken {
        actor_id: actor_id.clone(),
        credential_id: credential_id.into(),
        issued_at: 1_700_000_000,
        expires_at: 1_700_000_000 + 86_400,
        max_recipients: 100,
        max_messages_per_day: 1000,
        user_sig: ByteBuf::from(vec![0u8; SIGNATURE_LEN]),
    }
    .sign(&sk)
    .expect("sign");

    let token_bytes = token.to_canonical_bytes().expect("encode token");

    let password = b"correct".to_vec();
    let blob_bytes = seal_submission_token_blob(
        token_bytes,
        actor_id,
        credential_id.into(),
        "plain".into(),
        password.clone(),
        Some(small_argon2id()),
    )
    .expect("seal");

    let blob = WrappedSubmissionTokenBlob::from_canonical_bytes(&blob_bytes).expect("decode blob");
    let recovered =
        unseal_submission_token(&blob, &CredentialInput::Plain(&password), &vk).expect("unseal");
    assert_eq!(recovered.credential_id, credential_id);
}

#[test]
fn seal_validates_input_byte_lengths() {
    // 31-byte msek must reject — the inner type is [u8; 32].
    let err = seal_wrapped_msek_blob(
        vec![0u8; 31],
        vec![0u8; 32],
        "default".into(),
        "plain".into(),
        b"x".to_vec(),
        Some(small_argon2id()),
    )
    .expect_err("must reject");
    let msg = format!("{err}");
    assert!(msg.contains("32"), "expected length error, got: {msg}");

    // 31-byte actor_id must reject.
    let err = seal_wrapped_msek_blob(
        vec![0u8; 32],
        vec![0u8; 31],
        "default".into(),
        "plain".into(),
        b"x".to_vec(),
        Some(small_argon2id()),
    )
    .expect_err("must reject");
    let msg = format!("{err}");
    assert!(msg.contains("32"), "expected length error, got: {msg}");
}

#[test]
fn unknown_credential_kind_returns_error() {
    let err = seal_wrapped_msek_blob(
        vec![0u8; 32],
        vec![0u8; 32],
        "default".into(),
        "totp".into(),
        b"x".to_vec(),
        None,
    )
    .expect_err("must reject");
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("credential_kind") || msg.contains("totp"),
        "expected unknown-kind error, got: {msg}"
    );
}

#[test]
fn tls_cert_round_trip_via_existing_unseal() {
    use fauna_mls::wrapped_blob::{TlsCertBundle, generate_x25519_keypair};

    let (sk, pk) = generate_x25519_keypair();
    let bundle = TlsCertBundle {
        cert_chain: b"-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----".to_vec(),
        priv_key: vec![0xCDu8; 64],
        expires_at: 1_700_000_000 + 90 * 86_400,
        issued_at: 1_700_000_000,
    };
    let bundle_bytes = bundle.to_canonical_bytes().expect("encode bundle");

    let blob_bytes = seal_tls_cert_blob(
        bundle_bytes,
        "mta".into(),
        "bridge-1".into(),
        "example.com".into(),
        pk.to_vec(),
    )
    .expect("seal");

    // Existing FFI unseal exposed at libs/fauna-ffi/src/mail.rs:135.
    let recovered = unseal_tls_cert_blob(blob_bytes, sk.to_vec()).expect("unseal");
    assert_eq!(recovered.cert_chain, bundle.cert_chain);
    assert_eq!(recovered.priv_key, bundle.priv_key);
    assert_eq!(recovered.expires_at, bundle.expires_at);
    assert_eq!(recovered.issued_at, bundle.issued_at);
}

#[test]
fn tls_cert_wrong_recipient_returns_hpke_error() {
    use fauna_mls::wrapped_blob::{TlsCertBundle, generate_x25519_keypair};

    let (_, pk) = generate_x25519_keypair();
    let (wrong_sk, _) = generate_x25519_keypair();
    let bundle = TlsCertBundle {
        cert_chain: b"x".to_vec(),
        priv_key: vec![0u8; 32],
        expires_at: 2,
        issued_at: 1,
    };
    let bundle_bytes = bundle.to_canonical_bytes().expect("encode bundle");
    let blob_bytes = seal_tls_cert_blob(
        bundle_bytes,
        "mta".into(),
        "bridge-1".into(),
        "example.com".into(),
        pk.to_vec(),
    )
    .expect("seal");
    let err = match unseal_tls_cert_blob(blob_bytes, wrong_sk.to_vec()) {
        Ok(_) => panic!("expected unseal to fail with wrong recipient secret"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("hpke"),
        "expected HPKE failure, got: {msg}"
    );
}

// ── Capability-grant unseal (Slice 4 — the Go holder loop's FFI) ──
//
// There is no capability *seal* FFI (the mint is client-side); the tests build
// a `GrantBlob` directly via the inner `seal_capability` (as
// `submission_token_round_trip_via_inner_unseal` uses inner types), then
// exercise the `unseal_capability_grant` FFI the Go holder calls.

/// Build a canonical `GrantBlob` sealing `keys` (one per `(scope, payload)`) to
/// `holder_pk`, for `owner`/`grant_id`/`window`.
fn build_grant_blob(
    owner: [u8; 32],
    grant_id: [u8; 16],
    window: (u64, u64),
    holder_pk: [u8; 32],
    keys: &[(fauna_mls::wrapped_blob::ScopeTuple, Vec<u8>)],
) -> Vec<u8> {
    use fauna_mls::wrapped_blob::{
        BLOB_FORMAT_VERSION, GrantBlob, GrantIndex, GrantWindow, seal_capability,
    };
    use serde_bytes::ByteBuf;

    let wrapped_keys: Vec<_> = keys
        .iter()
        .map(|(scope, payload)| {
            seal_capability(payload, &owner, scope, None, &holder_pk).expect("seal capability key")
        })
        .collect();
    let blob = GrantBlob {
        version: BLOB_FORMAT_VERSION,
        kind: GrantBlob::KIND.into(),
        index: GrantIndex(owner.to_vec(), grant_id.to_vec()),
        holder: ByteBuf::from(holder_pk.to_vec()),
        window: GrantWindow(window.0, window.1),
        scope: keys.iter().map(|(s, _)| s.clone()).collect(),
        wrapped_keys,
    };
    blob.to_canonical_bytes().expect("encode grant blob")
}

#[test]
fn capability_grant_round_trips_all_scope_keys() {
    use fauna_mls::wrapped_blob::{ScopeTuple, generate_x25519_keypair};

    let (holder_sk, holder_pk) = generate_x25519_keypair();
    let owner = [0x11u8; 32];
    let grant_id = [0x22u8; 16];

    let mail_scope = ScopeTuple {
        class: "content.read".into(),
        kind: Some("mail".into()),
        tier: None,
        set: None,
        factor: None,
    };
    let post_scope = ScopeTuple {
        class: "content.read".into(),
        kind: Some("post".into()),
        tier: Some("t0".into()),
        set: None,
        factor: None,
    };
    let mail_key = vec![0xAAu8; 32];
    let post_key = vec![0xBBu8; 32];

    let blob_bytes = build_grant_blob(
        owner,
        grant_id,
        (1_000, 2_000),
        holder_pk,
        &[
            (mail_scope, mail_key.clone()),
            (post_scope, post_key.clone()),
        ],
    );

    let grant =
        unseal_capability_grant(blob_bytes, holder_sk.to_vec(), None).expect("unseal grant");
    assert_eq!(grant.owner_actor_id, owner.to_vec());
    assert_eq!(grant.grant_id, grant_id.to_vec());
    assert_eq!(grant.epoch_start, 1_000);
    assert_eq!(grant.epoch_end, 2_000);
    assert_eq!(grant.keys.len(), 2);

    // Keys are returned in `wrapped_keys` order.
    assert_eq!(grant.keys[0].class, "content.read");
    assert_eq!(grant.keys[0].kind.as_deref(), Some("mail"));
    assert_eq!(grant.keys[0].tier, None);
    assert_eq!(grant.keys[0].epoch, None);
    assert_eq!(grant.keys[0].key, mail_key);

    assert_eq!(grant.keys[1].kind.as_deref(), Some("post"));
    assert_eq!(grant.keys[1].tier.as_deref(), Some("t0"));
    assert_eq!(grant.keys[1].key, post_key);
}

#[test]
fn capability_grant_wrong_holder_returns_hpke_error() {
    use fauna_mls::wrapped_blob::{ScopeTuple, generate_x25519_keypair};

    let (_, holder_pk) = generate_x25519_keypair();
    let (wrong_sk, _) = generate_x25519_keypair();
    let owner = [0x11u8; 32];
    let scope = ScopeTuple {
        class: "content.read".into(),
        kind: Some("mail".into()),
        tier: None,
        set: None,
        factor: None,
    };
    let blob_bytes = build_grant_blob(
        owner,
        [0x22u8; 16],
        (1, 2),
        holder_pk,
        &[(scope, vec![0xCDu8; 32])],
    );

    // `UnsealedCapabilityGrant` deliberately does not derive `Debug` (it carries
    // secret key bytes), so match rather than `expect_err`.
    let err = match unseal_capability_grant(blob_bytes, wrong_sk.to_vec(), None) {
        Ok(_) => panic!("expected unseal to fail with wrong holder secret"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("hpke"),
        "expected HPKE failure, got: {msg}"
    );
}

#[test]
fn capability_grant_rejects_short_holder_secret() {
    let err = match unseal_capability_grant(vec![0u8; 8], vec![0u8; 31], None) {
        Ok(_) => panic!("expected 31-byte holder secret to reject"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(msg.contains("32"), "expected length error, got: {msg}");
}

// ── PQ-CAP-2: hybrid (X-Wing) capability-grant unseal ──
//
// A grant sealed X-Wing to the holder's post-quantum identity — the ML-KEM half
// derived from the bridge's Ed25519 seed via the new `derive_bridge_service_user_
// mlkem768` FFI, the X25519 half its keyfile key — opens with the holder's
// X25519 secret + derived ML-KEM dk. Mirrors the mail S3 seal_to_recipient_xwing
// / unseal_mail_record_hybrid pair.

/// Build a canonical `GrantBlob` sealing `keys` X-Wing (hybrid) to
/// `holder_xwing_pk` — the post-quantum counterpart of [`build_grant_blob`]. The
/// `holder` metadata field carries the X25519 pubkey (32 B), same as the
/// classical shape; the unseal opens from the passed secrets, not this field.
fn build_grant_blob_xwing(
    owner: [u8; 32],
    grant_id: [u8; 16],
    window: (u64, u64),
    holder_pk_x25519: [u8; 32],
    holder_xwing_pk: &fauna_mls::wrapped_blob::XWingPublicKey,
    keys: &[(fauna_mls::wrapped_blob::ScopeTuple, Vec<u8>)],
) -> Vec<u8> {
    use fauna_mls::wrapped_blob::{
        BLOB_FORMAT_VERSION, GrantBlob, GrantIndex, GrantWindow, seal_capability_xwing,
    };
    use serde_bytes::ByteBuf;

    let wrapped_keys: Vec<_> = keys
        .iter()
        .map(|(scope, payload)| {
            seal_capability_xwing(payload, &owner, scope, None, holder_xwing_pk)
                .expect("seal capability key x-wing")
        })
        .collect();
    let blob = GrantBlob {
        version: BLOB_FORMAT_VERSION,
        kind: GrantBlob::KIND.into(),
        index: GrantIndex(owner.to_vec(), grant_id.to_vec()),
        holder: ByteBuf::from(holder_pk_x25519.to_vec()),
        window: GrantWindow(window.0, window.1),
        scope: keys.iter().map(|(s, _)| s.clone()).collect(),
        wrapped_keys,
    };
    blob.to_canonical_bytes().expect("encode grant blob")
}

#[test]
fn capability_grant_hybrid_round_trips_with_derived_bridge_mlkem() {
    use fauna_mls::wrapped_blob::{ScopeTuple, XWingPublicKey, generate_x25519_keypair};

    // Derive the holder's ML-KEM keypair via the PQ-CAP-2 FFI — the exact call
    // the Go bridge makes from its keyfile Ed25519 seed — and pair it with an
    // X25519 keyfile keypair to form the holder's X-Wing identity.
    let bridge_kp =
        derive_bridge_service_user_mlkem768(vec![0x5Au8; 32]).expect("derive bridge mlkem");
    assert_eq!(
        bridge_kp.mlkem_ek.len(),
        1184,
        "ML-KEM-768 ek is 1184 bytes"
    );
    assert_eq!(
        bridge_kp.mlkem_dk.len(),
        2400,
        "ML-KEM-768 dk is 2400 bytes"
    );
    let mlkem_ek: [u8; 1184] = bridge_kp.mlkem_ek.as_slice().try_into().unwrap();
    let (holder_x25519_sk, holder_x25519_pk) = generate_x25519_keypair();
    let holder_xwing_pk = XWingPublicKey::from_parts(mlkem_ek, holder_x25519_pk);

    let owner = [0x11u8; 32];
    let grant_id = [0x22u8; 16];
    let scope = ScopeTuple {
        class: "content.read".into(),
        kind: Some("mail".into()),
        tier: None,
        set: None,
        factor: None,
    };
    let mail_key = vec![0xAAu8; 32];
    let blob_bytes = build_grant_blob_xwing(
        owner,
        grant_id,
        (1, 2),
        holder_x25519_pk,
        &holder_xwing_pk,
        &[(scope, mail_key.clone())],
    );

    // The holder opens the X-Wing wrap with its X25519 secret + derived ML-KEM dk.
    let grant = unseal_capability_grant(
        blob_bytes,
        holder_x25519_sk.to_vec(),
        Some(bridge_kp.mlkem_dk),
    )
    .expect("unseal hybrid grant");
    assert_eq!(grant.keys.len(), 1);
    assert_eq!(grant.keys[0].kind.as_deref(), Some("mail"));
    assert_eq!(grant.keys[0].key, mail_key);
}

#[test]
fn capability_grant_hybrid_wrap_wont_open_classically() {
    // A hybrid (X-Wing) grant reaching the CLASSICAL open path (dk = None) fails
    // with a typed error naming the hybrid opener — never a silent mis-decrypt.
    use fauna_mls::wrapped_blob::{ScopeTuple, XWingPublicKey, generate_x25519_keypair};

    let bridge_kp = derive_bridge_service_user_mlkem768(vec![0x33u8; 32]).unwrap();
    let mlkem_ek: [u8; 1184] = bridge_kp.mlkem_ek.as_slice().try_into().unwrap();
    let (holder_x25519_sk, holder_x25519_pk) = generate_x25519_keypair();
    let holder_xwing_pk = XWingPublicKey::from_parts(mlkem_ek, holder_x25519_pk);
    let scope = ScopeTuple {
        class: "content.read".into(),
        kind: Some("mail".into()),
        tier: None,
        set: None,
        factor: None,
    };
    let blob_bytes = build_grant_blob_xwing(
        [0x11u8; 32],
        [0x22u8; 16],
        (1, 2),
        holder_x25519_pk,
        &holder_xwing_pk,
        &[(scope, vec![0xAAu8; 32])],
    );
    let err = match unseal_capability_grant(blob_bytes, holder_x25519_sk.to_vec(), None) {
        Ok(_) => panic!("a hybrid grant must not open on the classical path"),
        Err(e) => e,
    };
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("hybrid"),
        "expected an error naming the hybrid opener, got: {msg}"
    );
}

#[test]
fn capability_grant_classical_opens_even_when_dk_supplied() {
    // Superset property: a classical grant still opens when the holder passes its
    // ML-KEM dk (the hybrid opener ignores the dk for a classical wrap), so a
    // holder drains classical grants (minted while it had no published ek)
    // with one code path.
    use fauna_mls::wrapped_blob::{ScopeTuple, generate_x25519_keypair};

    let (holder_sk, holder_pk) = generate_x25519_keypair();
    let bridge_kp = derive_bridge_service_user_mlkem768(vec![0x44u8; 32]).unwrap();
    let scope = ScopeTuple {
        class: "content.read".into(),
        kind: Some("mail".into()),
        tier: None,
        set: None,
        factor: None,
    };
    let mail_key = vec![0xBBu8; 32];
    // A CLASSICAL wrap (build_grant_blob uses seal_capability).
    let blob_bytes = build_grant_blob(
        [0x11u8; 32],
        [0x22u8; 16],
        (1, 2),
        holder_pk,
        &[(scope, mail_key.clone())],
    );
    let grant = unseal_capability_grant(blob_bytes, holder_sk.to_vec(), Some(bridge_kp.mlkem_dk))
        .expect("classical grant opens with dk supplied");
    assert_eq!(grant.keys[0].key, mail_key);
}

#[test]
fn unseal_capability_grant_rejects_wrong_length_dk() {
    let err = match unseal_capability_grant(vec![0u8; 8], vec![0u8; 32], Some(vec![0u8; 100])) {
        Ok(_) => panic!("expected a 100-byte ML-KEM dk to reject"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("2400"),
        "expected the 2400-byte dk length error, got: {msg}"
    );
}

#[test]
fn derive_bridge_service_user_mlkem768_is_deterministic_and_seed_gated() {
    // Deterministic in the seed (so the bridge re-derives the same key across
    // restarts) and distinct across seeds.
    let a = derive_bridge_service_user_mlkem768(vec![0x01u8; 32]).unwrap();
    let a2 = derive_bridge_service_user_mlkem768(vec![0x01u8; 32]).unwrap();
    let b = derive_bridge_service_user_mlkem768(vec![0x02u8; 32]).unwrap();
    assert_eq!(a.mlkem_ek, a2.mlkem_ek, "same seed → same ek");
    assert_eq!(a.mlkem_dk, a2.mlkem_dk, "same seed → same dk");
    assert_ne!(a.mlkem_ek, b.mlkem_ek, "distinct seeds → distinct ek");
    // A non-32-byte seed is rejected.
    let err = derive_bridge_service_user_mlkem768(vec![0u8; 31]);
    assert!(err.is_err(), "31-byte seed must reject");
}

// ── Capability-grant BUILD (mint-side) FFI ──
//
// The client-side mint the native apps (UniFFI) and the tier_3 seal-helper
// harness call: `build_capability_grant_blob` derives no keys itself — the
// caller supplies each already-derived minimal payload — HPKE-seals the
// key-bearing tuples to the holder, and returns canonical `GrantBlob` bytes
// ready for `fauna.capabilities.mint`. HPKE seal is randomized, so these assert
// via round-trip through `unseal_capability_grant`, never byte-equality.

#[test]
fn build_capability_grant_blob_round_trips_and_declares_keyless_scope() {
    use fauna_mls::wrapped_blob::{GrantBlob as InnerBlob, generate_x25519_keypair};

    let (holder_sk, holder_pk) = generate_x25519_keypair();
    let owner = [0x11u8; 32];
    let grant_id = [0x22u8; 16];
    let mail_key = vec![0xAAu8; 32];

    // Mint a mail-re-score grant: a key-bearing content.read{mail} tuple plus a
    // keyless content.label-write tuple (declared in `scope`, no wrapped key).
    let blob_bytes = build_capability_grant_blob(
        owner.to_vec(),
        grant_id.to_vec(),
        holder_pk.to_vec(),
        None, // classical wrap
        1_000,
        2_000,
        vec![
            CapabilityScopeInput {
                class: "content.read".into(),
                kind: Some("mail".into()),
                tier: None,
                factor: None,
                payload: Some(mail_key.clone()),
            },
            CapabilityScopeInput {
                class: "content.label-write".into(),
                kind: None,
                tier: None,
                factor: None,
                payload: None,
            },
        ],
    )
    .expect("build capability grant via ffi");

    // Both tuples are declared; only the key-bearing one wraps a key.
    let decoded = InnerBlob::from_canonical_bytes(&blob_bytes).expect("decode ffi-built blob");
    assert_eq!(decoded.scope.len(), 2, "both tuples declared in scope");
    assert_eq!(
        decoded.wrapped_keys.len(),
        1,
        "only the key-bearing tuple wraps a key"
    );
    assert_eq!(decoded.scope[1].class, "content.label-write");

    // The holder unseals the mail key end-to-end via the open-side FFI.
    let grant = unseal_capability_grant(blob_bytes, holder_sk.to_vec(), None)
        .expect("unseal ffi-built grant");
    assert_eq!(grant.owner_actor_id, owner.to_vec());
    assert_eq!(grant.grant_id, grant_id.to_vec());
    assert_eq!(grant.epoch_start, 1_000);
    assert_eq!(grant.epoch_end, 2_000);
    assert_eq!(grant.keys.len(), 1);
    assert_eq!(grant.keys[0].class, "content.read");
    assert_eq!(grant.keys[0].kind.as_deref(), Some("mail"));
    assert_eq!(grant.keys[0].epoch, None);
    assert_eq!(grant.keys[0].key, mail_key);
}

#[test]
fn build_capability_grant_blob_selects_xwing_when_ek_supplied_and_holder_drains() {
    // PQ-CAP-3 mint via FFI (the native-client / seal-helper path): a holder ek
    // makes the wrap X-Wing, the classical-only opener refuses it (loud, not a
    // silent mis-decrypt), and the holder opens it with its X25519 secret + the
    // matching ML-KEM dk — the tier_3 drain shape PQ-CAP-4 proves end-to-end.
    use fauna_mls::wrapped_blob::{
        FAUNA_KEM_XWING, GrantBlob as InnerBlob, generate_x25519_keypair,
    };

    let (holder_sk, holder_pk) = generate_x25519_keypair();
    let bridge_kp = derive_bridge_service_user_mlkem768(vec![0x7Au8; 32]).unwrap();
    let owner = [0x11u8; 32];
    let mail_key = vec![0xABu8; 32];

    let blob_bytes = build_capability_grant_blob(
        owner.to_vec(),
        [0x22u8; 16].to_vec(),
        holder_pk.to_vec(),
        Some(bridge_kp.mlkem_ek.clone()),
        1,
        2,
        vec![CapabilityScopeInput {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
            factor: None,
            payload: Some(mail_key.clone()),
        }],
    )
    .expect("build hybrid capability grant via ffi");

    let decoded = InnerBlob::from_canonical_bytes(&blob_bytes).expect("decode ffi-built blob");
    assert_eq!(
        decoded.wrapped_keys[0].hpke.kem_suite.kem, FAUNA_KEM_XWING,
        "holder ek ⇒ X-Wing wrap"
    );

    // A hybrid wrap reaching the classical open path (dk None) fails loudly.
    if unseal_capability_grant(blob_bytes.clone(), holder_sk.to_vec(), None).is_ok() {
        panic!("a hybrid wrap must not open on the classical (None dk) path");
    }
    // The holder opens it with its X25519 secret + derived ML-KEM dk.
    let grant = unseal_capability_grant(blob_bytes, holder_sk.to_vec(), Some(bridge_kp.mlkem_dk))
        .expect("hybrid grant opens with the holder dk");
    assert_eq!(grant.keys[0].key, mail_key);
}

#[test]
fn build_capability_grant_blob_rejects_bad_lengths() {
    // owner must be 32 bytes.
    let err = match build_capability_grant_blob(
        vec![0u8; 31],
        vec![0u8; 16],
        vec![0u8; 32],
        None,
        0,
        1,
        vec![],
    ) {
        Ok(_) => panic!("expected 31-byte owner to reject"),
        Err(e) => e,
    };
    assert!(
        format!("{err}").contains("32"),
        "expected owner length error, got: {err}"
    );

    // grant_id must be 16 bytes.
    let err = match build_capability_grant_blob(
        vec![0u8; 32],
        vec![0u8; 15],
        vec![0u8; 32],
        None,
        0,
        1,
        vec![],
    ) {
        Ok(_) => panic!("expected 15-byte grant_id to reject"),
        Err(e) => e,
    };
    assert!(
        format!("{err}").contains("16"),
        "expected grant_id length error, got: {err}"
    );

    // holder pubkey must be 32 bytes.
    let err = match build_capability_grant_blob(
        vec![0u8; 32],
        vec![0u8; 16],
        vec![0u8; 31],
        None,
        0,
        1,
        vec![CapabilityScopeInput {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
            factor: None,
            payload: Some(vec![0u8; 32]),
        }],
    ) {
        Ok(_) => panic!("expected 31-byte holder pubkey to reject"),
        Err(e) => e,
    };
    assert!(
        format!("{err}").contains("32"),
        "expected holder length error, got: {err}"
    );
}
