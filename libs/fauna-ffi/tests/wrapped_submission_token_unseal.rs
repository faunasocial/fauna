//! Phase D.2 (mail-bridge MTA submission): UniFFI surface for
//! AEAD-unwrap-as-auth on `WrappedSubmissionTokenBlob`.
//!
//! Mirrors `wrapped_msek_unwrap.rs` but for the submission-token shape:
//! the unseal returns a plaintext `SubmissionTokenFfi` Record (not an
//! mlock'd capability — submission tokens carry no decryption material)
//! and additionally verifies the inner Ed25519 signature against the
//! actor_id-as-VerifyingKey (per the codebase invariant `actor_id ==
//! Ed25519 verifying-key bytes`; cf.
//! `bins/fauna-nest/src/registration.rs:184` and
//! `bridge_blob_handlers.rs:565`).
//!
//! Substitution-resistance is baked in: AAD reconstruction uses the
//! caller-supplied (expected_actor_id, expected_credential_id), not
//! the blob's `ix` field; the FFI additionally cross-checks blob.index
//! before delegating to the inner unseal so a wrong-shape blob fails
//! cheaply rather than waiting for AEAD verify.

use ed25519_dalek::{Signer, SigningKey};
use fauna_ffi::{KdfKind, SubmissionTokenFfi, unseal_submission_token_blob};
use fauna_mls::wrapped_blob::submission_token::fresh_signed_token;
use fauna_mls::wrapped_blob::{
    Argon2idParams, CredentialInput, HkdfSha256Params, KdfParams, SIGNATURE_LEN, SubmissionToken,
    seal_submission_token,
};
use serde_bytes::ByteBuf;

fn small_argon2id() -> KdfParams {
    KdfParams::Argon2id(Argon2idParams {
        m: 4096,
        t: 1,
        p: 1,
    })
}

/// Per-test signing key. The invariant under test is that `actor_id`
/// IS the Ed25519 verifying key — so the seal-time signer's verifying
/// key must equal the actor_id bytes the unseal call passes in.
fn fresh_keypair() -> (SigningKey, [u8; 32]) {
    let mut secret = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut secret);
    let sk = SigningKey::from_bytes(&secret);
    let pk = sk.verifying_key().to_bytes();
    (sk, pk)
}

#[test]
fn unseal_plain_succeeds_with_correct_password() {
    let (sk, pk) = fresh_keypair();
    let token = fresh_signed_token(&sk, &pk, "cred-1");
    let blob = seal_submission_token(
        &token,
        &pk,
        "cred-1",
        &CredentialInput::Plain(b"correct-password"),
        small_argon2id(),
    )
    .expect("seal");
    let blob_bytes = blob.to_canonical_bytes().expect("encode");

    let out: SubmissionTokenFfi = unseal_submission_token_blob(
        blob_bytes,
        b"correct-password".to_vec(),
        pk.to_vec(),
        "cred-1".into(),
        KdfKind::Argon2id,
    )
    .expect("unseal must succeed");

    assert_eq!(out.actor_id, pk.to_vec());
    assert_eq!(out.credential_id, "cred-1");
    assert_eq!(out.issued_at, 1_700_000_000);
    assert_eq!(out.expires_at, 1_700_000_000 + 86_400);
    assert_eq!(out.max_recipients, 100);
    assert_eq!(out.max_messages_per_day, 1000);
}

#[test]
fn unseal_plain_fails_with_wrong_password() {
    let (sk, pk) = fresh_keypair();
    let token = fresh_signed_token(&sk, &pk, "cred-1");
    let blob = seal_submission_token(
        &token,
        &pk,
        "cred-1",
        &CredentialInput::Plain(b"correct-password"),
        small_argon2id(),
    )
    .expect("seal");
    let blob_bytes = blob.to_canonical_bytes().expect("encode");

    let err = unseal_submission_token_blob(
        blob_bytes,
        b"wrong-password".to_vec(),
        pk.to_vec(),
        "cred-1".into(),
        KdfKind::Argon2id,
    )
    .expect_err("wrong password must AEAD-fail");
    assert!(format!("{err}").to_lowercase().contains("aead"));
}

#[test]
fn unseal_oauth_succeeds_with_correct_token() {
    let (sk, pk) = fresh_keypair();
    let token = fresh_signed_token(&sk, &pk, "oauth-cred");
    let blob = seal_submission_token(
        &token,
        &pk,
        "oauth-cred",
        &CredentialInput::OauthBearer(b"high-entropy-bearer-token"),
        KdfParams::HkdfSha256(HkdfSha256Params),
    )
    .expect("seal");
    let blob_bytes = blob.to_canonical_bytes().expect("encode");

    let out = unseal_submission_token_blob(
        blob_bytes,
        b"high-entropy-bearer-token".to_vec(),
        pk.to_vec(),
        "oauth-cred".into(),
        KdfKind::Hkdf,
    )
    .expect("unseal must succeed");
    assert_eq!(out.credential_id, "oauth-cred");
}

#[test]
fn unseal_oauth_fails_with_wrong_token() {
    let (sk, pk) = fresh_keypair();
    let token = fresh_signed_token(&sk, &pk, "oauth-cred");
    let blob = seal_submission_token(
        &token,
        &pk,
        "oauth-cred",
        &CredentialInput::OauthBearer(b"correct-token"),
        KdfParams::HkdfSha256(HkdfSha256Params),
    )
    .expect("seal");
    let blob_bytes = blob.to_canonical_bytes().expect("encode");

    let err = unseal_submission_token_blob(
        blob_bytes,
        b"wrong-token".to_vec(),
        pk.to_vec(),
        "oauth-cred".into(),
        KdfKind::Hkdf,
    )
    .expect_err("wrong token must AEAD-fail");
    assert!(format!("{err}").to_lowercase().contains("aead"));
}

#[test]
fn unseal_rejects_kind_credential_mismatch() {
    // PLAIN-sealed blob unwrapped with the Hkdf KDF arm: same shape as
    // wrapped_msek_unwrap's parallel test. The KDF dispatch diverges
    // before AEAD, so the error wording must NOT contain "aead".
    let (blob_bytes, pk) = {
        let (sk, pk) = fresh_keypair();
        let token = fresh_signed_token(&sk, &pk, "cred-1");
        let blob = seal_submission_token(
            &token,
            &pk,
            "cred-1",
            &CredentialInput::Plain(b"pw"),
            small_argon2id(),
        )
        .expect("seal");
        (blob.to_canonical_bytes().expect("encode"), pk)
    };

    let err = unseal_submission_token_blob(
        blob_bytes,
        b"pw".to_vec(),
        pk.to_vec(),
        "cred-1".into(),
        KdfKind::Hkdf,
    )
    .expect_err("kind/credential mismatch must error");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("kdf") || msg.contains("invalid") || msg.contains("format"),
        "expected KDF/format error, got: {msg}"
    );
}

#[test]
fn unseal_rejects_short_actor_id() {
    let (blob_bytes, _pk) = {
        let (sk, pk) = fresh_keypair();
        let token = fresh_signed_token(&sk, &pk, "cred-1");
        let blob = seal_submission_token(
            &token,
            &pk,
            "cred-1",
            &CredentialInput::Plain(b"pw"),
            small_argon2id(),
        )
        .expect("seal");
        (blob.to_canonical_bytes().expect("encode"), pk)
    };
    let err = unseal_submission_token_blob(
        blob_bytes,
        b"pw".to_vec(),
        vec![0u8; 16],
        "cred-1".into(),
        KdfKind::Argon2id,
    )
    .expect_err("short actor_id must error");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("32") || msg.contains("actor"),
        "expected actor_id-shape error, got: {msg}"
    );
}

#[test]
fn unseal_rejects_actor_id_substitution() {
    // The blob was sealed under pk; if the caller passes a different
    // actor_id, the AAD reconstruction at unwrap differs from seal-time
    // → AEAD-fail (defense in depth even before the inner Ed25519 verify
    // would catch a fully-forged token).
    let (blob_bytes, _pk) = {
        let (sk, pk) = fresh_keypair();
        let token = fresh_signed_token(&sk, &pk, "cred-1");
        let blob = seal_submission_token(
            &token,
            &pk,
            "cred-1",
            &CredentialInput::Plain(b"pw"),
            small_argon2id(),
        )
        .expect("seal");
        (blob.to_canonical_bytes().expect("encode"), pk)
    };
    // Use the verifying key of an *unrelated* keypair so the actor_id
    // is structurally valid (32 bytes) but doesn't match the seal-time
    // actor.
    let (_other_sk, other_pk) = fresh_keypair();
    let err = unseal_submission_token_blob(
        blob_bytes,
        b"pw".to_vec(),
        other_pk.to_vec(),
        "cred-1".into(),
        KdfKind::Argon2id,
    )
    .expect_err("actor substitution must error");
    let msg = format!("{err}").to_lowercase();
    // Either the pre-AEAD blob.index check trips, or the AAD-bound
    // AEAD-open fails. Both are acceptable; both reject substitution.
    assert!(
        msg.contains("aead") || msg.contains("actor") || msg.contains("substitution"),
        "expected substitution-reject error, got: {msg}"
    );
}

#[test]
fn unseal_rejects_credential_id_substitution() {
    // Blob sealed under credential_id="cred-1"; caller passes "cred-2".
    // Same defense path as actor-id substitution: AAD divergence →
    // AEAD-fail or pre-AEAD blob.index mismatch.
    let (blob_bytes, pk) = {
        let (sk, pk) = fresh_keypair();
        let token = fresh_signed_token(&sk, &pk, "cred-1");
        let blob = seal_submission_token(
            &token,
            &pk,
            "cred-1",
            &CredentialInput::Plain(b"pw"),
            small_argon2id(),
        )
        .expect("seal");
        (blob.to_canonical_bytes().expect("encode"), pk)
    };
    let err = unseal_submission_token_blob(
        blob_bytes,
        b"pw".to_vec(),
        pk.to_vec(),
        "cred-2".into(),
        KdfKind::Argon2id,
    )
    .expect_err("credential_id substitution must error");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("aead") || msg.contains("credential") || msg.contains("substitution"),
        "expected substitution-reject error, got: {msg}"
    );
}

#[test]
fn unseal_rejects_malformed_blob_bytes() {
    let (_sk, pk) = fresh_keypair();
    let err = unseal_submission_token_blob(
        b"not-cbor".to_vec(),
        b"pw".to_vec(),
        pk.to_vec(),
        "cred-1".into(),
        KdfKind::Argon2id,
    )
    .expect_err("malformed blob bytes must error");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("decode") || msg.contains("cbor") || msg.contains("format"),
        "expected decode-error wording, got: {msg}"
    );
}

#[test]
fn unseal_rejects_forged_token_with_wrong_signer() {
    // Construct a blob where the inner SubmissionToken is signed by a
    // DIFFERENT key than the actor_id == verifying_key invariant
    // implies. AEAD passes (we have the credential), but the inner
    // Ed25519 verify must catch the mismatch. This is the load-bearing
    // defense against a coerced/compromised nest fabricating tokens.
    let (signer_sk, _signer_pk) = fresh_keypair();
    let (_actor_sk, actor_pk) = fresh_keypair();
    // Sign the token with signer_sk but claim actor_id = actor_pk in
    // both the SubmissionToken.actor_id field and the seal call. The
    // seal succeeds (AEAD doesn't care about signature validity); the
    // unseal's inner verify against actor_pk-as-VerifyingKey must fail
    // because the signature was made with signer_sk, not actor_sk.
    let token = SubmissionToken {
        actor_id: actor_pk.to_vec(),
        credential_id: "cred-1".into(),
        issued_at: 1_700_000_000,
        expires_at: 1_700_000_000 + 86_400,
        max_recipients: 100,
        max_messages_per_day: 1000,
        user_sig: ByteBuf::from(vec![0u8; SIGNATURE_LEN]),
    };
    // Sign with signer_sk over the canonical bytes-with-zeroed-sig.
    let bytes_for_sig = {
        let mut tmp = token.clone();
        tmp.user_sig = ByteBuf::from(vec![0u8; SIGNATURE_LEN]);
        tmp.to_canonical_bytes().expect("encode")
    };
    let sig = signer_sk.sign(&bytes_for_sig);
    let mut forged = token;
    forged.user_sig = ByteBuf::from(sig.to_bytes().to_vec());

    let blob = seal_submission_token(
        &forged,
        &actor_pk,
        "cred-1",
        &CredentialInput::Plain(b"pw"),
        small_argon2id(),
    )
    .expect("seal");
    let blob_bytes = blob.to_canonical_bytes().expect("encode");

    let err = unseal_submission_token_blob(
        blob_bytes,
        b"pw".to_vec(),
        actor_pk.to_vec(),
        "cred-1".into(),
        KdfKind::Argon2id,
    )
    .expect_err("forged signature must be rejected");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("signature") || msg.contains("verify"),
        "expected signature-verify error, got: {msg}"
    );
}
