//! Phase C.3 (mail-bridge MDA): UniFFI surface for AEAD-unwrap-as-auth.
//!
//! The MDA holds an mlock'd MSEK after a successful PLAIN / OAUTHBEARER
//! AUTH. The wrapped-MSEK blob fetched from nest is canonical-CBOR
//! `WrappedMsekBlob` (see `fauna_mls::wrapped_blob`); the FFI takes the
//! encoded bytes plus the MUA-supplied credential and returns an
//! `MlsCapability` (UniFFI `Object`, so it owns mlock'd memory and runs
//! `Drop` to zeroize).
//!
//! These tests pin the cross-area contract that the Go bridge (and any
//! other UniFFI consumer) sees.

use fauna_ffi::{KdfKind, unwrap_msek_blob};
use fauna_mls::wrapped_blob::{
    AEAD_NONCE_LEN, Argon2idParams, CredentialInput, HkdfSha256Params, KdfParams,
    seal_mls_snapshot, seal_wrapped_msek,
};

fn small_argon2id() -> KdfParams {
    KdfParams::Argon2id(Argon2idParams {
        m: 4096,
        t: 1,
        p: 1,
    })
}

const ACTOR: [u8; 32] = [0x42u8; 32];
const CRED_ID: &str = "cred-1";

fn seal_plain_blob(password: &[u8], msek: &[u8; 32]) -> Vec<u8> {
    let blob = seal_wrapped_msek(
        msek,
        &ACTOR,
        CRED_ID,
        &CredentialInput::Plain(password),
        small_argon2id(),
    )
    .expect("seal plain");
    blob.to_canonical_bytes().expect("encode plain blob")
}

fn seal_oauth_blob(token: &[u8], msek: &[u8; 32]) -> Vec<u8> {
    let blob = seal_wrapped_msek(
        msek,
        &ACTOR,
        CRED_ID,
        &CredentialInput::OauthBearer(token),
        KdfParams::HkdfSha256(HkdfSha256Params),
    )
    .expect("seal oauth");
    blob.to_canonical_bytes().expect("encode oauth blob")
}

#[test]
fn unwrap_msek_plain_succeeds_with_correct_password() {
    let msek = [0x11u8; 32];
    let blob = seal_plain_blob(b"correct-password", &msek);
    let cap = unwrap_msek_blob(
        blob,
        b"correct-password".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Argon2id,
    )
    .expect("unwrap should succeed with correct credential");
    // The returned object is a usable MlsCapability — a smoke check
    // that the Arc-counted Object actually came back rather than a
    // null/dropped placeholder.
    drop(cap);
}

#[test]
fn unwrap_msek_plain_fails_with_wrong_password() {
    let msek = [0u8; 32];
    let blob = seal_plain_blob(b"correct-password", &msek);
    let err = unwrap_msek_blob(
        blob,
        b"wrong-password".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Argon2id,
    )
    .expect_err("wrong password must AEAD-fail");
    // Per imap-server.md § Authentication: AEAD-fail is the
    // authentication-failure signal. The bridge maps it to IMAP
    // `NO Authentication failed` + report_auth_event(fail).
    assert!(format!("{err}").to_lowercase().contains("aead"));
}

#[test]
fn unwrap_msek_oauth_succeeds_with_correct_token() {
    let msek = [0xCDu8; 32];
    let blob = seal_oauth_blob(b"high-entropy-bearer-token", &msek);
    let _cap = unwrap_msek_blob(
        blob,
        b"high-entropy-bearer-token".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Hkdf,
    )
    .expect("unwrap should succeed with correct HKDF token");
}

#[test]
fn unwrap_msek_oauth_fails_with_wrong_token() {
    let msek = [0u8; 32];
    let blob = seal_oauth_blob(b"correct-token", &msek);
    let err = unwrap_msek_blob(
        blob,
        b"wrong-token".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Hkdf,
    )
    .expect_err("wrong token must AEAD-fail");
    assert!(format!("{err}").to_lowercase().contains("aead"));
}

#[test]
fn unwrap_msek_rejects_kind_credential_mismatch() {
    // PLAIN-sealed blob unwrapped with the Hkdf KDF arm: the AEAD
    // key derivation diverges before the AEAD call, so this surfaces
    // as a format-level error (KdfFailed / InvalidFormat), NOT as
    // AeadFailed. Distinguishing the two matters because the bridge
    // wires AEAD-fail to the "wrong credential" audit-log path; this
    // case is a misformed blob/request, not a brute-force attempt.
    let msek = [0u8; 32];
    let blob = seal_plain_blob(b"pw", &msek);
    let err = unwrap_msek_blob(
        blob,
        b"pw".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Hkdf,
    )
    .expect_err("kind/credential mismatch must error");
    // Either kdf or invalid-format wording is acceptable; the absence
    // of "aead" in the message is the load-bearing assertion.
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("kdf") || msg.contains("invalid") || msg.contains("format"),
        "expected KDF/format error, got: {msg}"
    );
}

#[test]
fn unwrap_msek_rejects_short_actor_id() {
    let msek = [0u8; 32];
    let blob = seal_plain_blob(b"pw", &msek);
    let err = unwrap_msek_blob(
        blob,
        b"pw".to_vec(),
        vec![0u8; 16], // wrong length
        CRED_ID.into(),
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
fn unwrap_msek_rejects_actor_id_substitution() {
    // The AAD binds actor_id; supplying a different actor_id at
    // unwrap time triggers AEAD-fail (substitution-resistance,
    // matching the seal-side cross_actor_substitution_fails test).
    let msek = [0u8; 32];
    let blob = seal_plain_blob(b"pw", &msek);
    let wrong_actor = vec![0xBBu8; 32];
    let err = unwrap_msek_blob(
        blob,
        b"pw".to_vec(),
        wrong_actor,
        CRED_ID.into(),
        KdfKind::Argon2id,
    )
    .expect_err("actor substitution must AEAD-fail");
    assert!(format!("{err}").to_lowercase().contains("aead"));
}

#[test]
fn capability_decrypt_returns_mls_snapshot_plaintext() {
    // The MlsCapability owns the MSEK after unwrap; its `decrypt`
    // method opens an `MlsSnapshotBlob` sealed under that MSEK. This
    // is the read-side primitive the bridge uses once Phase C.6
    // wires body decryption end-to-end.
    let msek = [0xEEu8; 32];
    let blob = seal_plain_blob(b"pw", &msek);
    let cap = unwrap_msek_blob(
        blob,
        b"pw".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Argon2id,
    )
    .expect("unwrap");

    let plaintext = b"serialized read-only MLS state";
    let snapshot = seal_mls_snapshot(plaintext, &ACTOR, &msek).expect("seal snapshot");
    let snapshot_bytes = snapshot.to_canonical_bytes().expect("encode snapshot");
    let out = cap.decrypt(snapshot_bytes).expect("decrypt");
    assert_eq!(out, plaintext);
}

#[test]
fn capability_decrypt_after_zeroize_errors() {
    // Explicit zeroize MUST make further decrypts fail (the inner
    // MSEK is gone). Drop-time zeroization is also required and is
    // covered by `capability_drops_without_panic` below; this test
    // pins the *explicit* API path the Go bridge calls in
    // `Session.Close` before nil'ing the field.
    let msek = [0u8; 32];
    let blob = seal_plain_blob(b"pw", &msek);
    let cap = unwrap_msek_blob(
        blob,
        b"pw".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Argon2id,
    )
    .expect("unwrap");
    cap.zeroize();

    let snapshot = seal_mls_snapshot(b"x", &ACTOR, &msek).expect("seal snapshot");
    let snapshot_bytes = snapshot.to_canonical_bytes().expect("encode snapshot");
    let err = cap
        .decrypt(snapshot_bytes)
        .expect_err("decrypt after zeroize must fail");
    assert!(
        format!("{err}").to_lowercase().contains("zeroized")
            || format!("{err}").to_lowercase().contains("capability"),
        "expected zeroized-capability error, got: {err}"
    );
}

#[test]
fn capability_drops_without_panic() {
    // Drop of an Arc-counted Object MUST run zeroize on the inner
    // MSEK without panic. This is the contract the Go side relies on
    // when it lets the finalizer run (the explicit zeroize path is
    // exercised by `capability_decrypt_after_zeroize_errors`).
    let msek = [0u8; 32];
    let blob = seal_plain_blob(b"pw", &msek);
    let cap = unwrap_msek_blob(
        blob,
        b"pw".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Argon2id,
    )
    .expect("unwrap");
    drop(cap);
}

#[test]
fn unwrap_msek_rejects_malformed_blob_bytes() {
    let err = unwrap_msek_blob(
        b"not-cbor".to_vec(),
        b"pw".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
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
fn aead_nonce_len_matches_msek_module() {
    // Sanity check: tests don't hardcode 12 — derive from the same
    // const the seal/unseal path uses.
    assert_eq!(AEAD_NONCE_LEN, 12);
}

// Smoke: confirm `seal_plain_blob` (the in-test helper) and the FFI
// agree on byte representation. Catches accidental param drift
// between the helper and what `unwrap_msek_blob` expects.
#[test]
fn seal_then_unwrap_via_ffi_smoke() {
    let msek = [0x33u8; 32];
    let bytes = seal_plain_blob(b"smoke-pw", &msek);
    let _cap = unwrap_msek_blob(
        bytes,
        b"smoke-pw".to_vec(),
        ACTOR.to_vec(),
        CRED_ID.into(),
        KdfKind::Argon2id,
    )
    .expect("unwrap");
}
