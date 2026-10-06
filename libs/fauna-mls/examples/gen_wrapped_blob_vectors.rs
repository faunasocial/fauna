//! Generate canonical-CBOR test vectors for each wrapped-blob shape.
//! Run from the workspace root:
//!
//!     cargo run -p fauna-mls --example gen_wrapped_blob_vectors
//!
//! Writes binary fixtures into
//! libs/fauna-protocol/schemas/test_vectors/. Inputs are
//! deterministic for inner content; randomness comes from the seal
//! functions' salt/nonce/HPKE-ephemeral generation.

use ed25519_dalek::SigningKey;
use fauna_mls::wrapped_blob::{
    Argon2idParams, CredentialInput, HkdfSha256Params, KdfParams, SIGNATURE_LEN, SubmissionToken,
    TlsCertBundle, seal_mls_snapshot, seal_submission_token, seal_tls_cert, seal_wrapped_msek,
};
use serde_bytes::ByteBuf;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.join("../fauna-protocol/schemas/test_vectors")
}

fn write(name: &str, bytes: &[u8]) {
    let path = fixtures_dir().join(name);
    std::fs::write(&path, bytes).expect("write fixture");
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
}

fn deterministic_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[0xAAu8; 32])
}

fn main() {
    // Ensure fixtures directory exists.
    std::fs::create_dir_all(fixtures_dir()).expect("create fixtures dir");

    let actor = [0x42u8; 32];

    // wrapped-msek (small argon2 for fast generation; this fixture is
    // for CBOR-shape conformance only, not crypto correctness).
    let cred = CredentialInput::Plain(b"deterministic-pw");
    let msek = [0x11u8; 32];
    let blob = seal_wrapped_msek(
        &msek,
        &actor,
        "default",
        &cred,
        KdfParams::Argon2id(Argon2idParams {
            m: 4096,
            t: 1,
            p: 1,
        }),
    )
    .unwrap();
    write("wrapped_msek.bin", &blob.to_canonical_bytes().unwrap());

    // wrapped-msek (OAUTHBEARER / HKDF arm). Used by the Phase C.3
    // Go bridge tests at bins/fauna-bridges/internal/mda/imap/
    // — the bridge needs both a PLAIN-sealed and an HKDF-sealed
    // fixture to exercise the AUTH=PLAIN / AUTH=OAUTHBEARER paths
    // end-to-end. Same (actor, credential_id) tuple as the PLAIN
    // fixture above so a single set of validate_recipient stubs
    // covers both.
    let oauth_cred = CredentialInput::OauthBearer(b"oauth-deterministic-token");
    let oauth_blob = seal_wrapped_msek(
        &msek,
        &actor,
        "default",
        &oauth_cred,
        KdfParams::HkdfSha256(HkdfSha256Params),
    )
    .unwrap();
    write(
        "wrapped_msek_oauth.bin",
        &oauth_blob.to_canonical_bytes().unwrap(),
    );

    // mls-snapshot
    let snap = seal_mls_snapshot(b"deterministic snapshot bytes", &actor, &msek).unwrap();
    write("mls_snapshot.bin", &snap.to_canonical_bytes().unwrap());

    // submission-token. The codebase invariant `actor_id == Ed25519
    // verifying-key bytes` (cf. bins/fauna-nest/src/registration.rs:184,
    // bridge_blob_handlers.rs:565) requires the fixture's actor_id to
    // equal the signer's verifying key — otherwise downstream tests
    // that reconstruct the verifying key from actor_id can't verify
    // the inner signature.
    //
    // The fixture's `default` credential_id and the PLAIN
    // `deterministic-pw` secret stay shared with the wrapped-MSEK
    // fixture above so a single stub `validate_recipient` + credential
    // pair covers both AUTH surfaces in cross-area tests.
    let sk = deterministic_signing_key();
    let submission_actor: [u8; 32] = sk.verifying_key().to_bytes();
    let token = SubmissionToken {
        actor_id: submission_actor.to_vec(),
        credential_id: "default".into(),
        issued_at: 1_700_000_000,
        expires_at: 1_700_000_000 + 86_400,
        max_recipients: 100,
        max_messages_per_day: 1000,
        user_sig: ByteBuf::from(vec![0u8; SIGNATURE_LEN]),
    }
    .sign(&sk)
    .unwrap();
    let stoken = seal_submission_token(
        &token,
        &submission_actor,
        "default",
        &cred,
        KdfParams::Argon2id(Argon2idParams {
            m: 4096,
            t: 1,
            p: 1,
        }),
    )
    .unwrap();
    write(
        "wrapped_submission_token.bin",
        &stoken.to_canonical_bytes().unwrap(),
    );

    // submission-token (OAUTHBEARER / HKDF arm). Mirror of the PLAIN
    // fixture above but sealed under the HKDF KDF arm so the Go
    // submission AUTH tests can exercise both PLAIN and OAUTHBEARER
    // round-trips against committed bytes. Same (actor, credential_id)
    // pair so a single stub set covers both.
    let oauth_token = SubmissionToken {
        actor_id: submission_actor.to_vec(),
        credential_id: "default".into(),
        issued_at: 1_700_000_000,
        expires_at: 1_700_000_000 + 86_400,
        max_recipients: 100,
        max_messages_per_day: 1000,
        user_sig: ByteBuf::from(vec![0u8; SIGNATURE_LEN]),
    }
    .sign(&sk)
    .unwrap();
    let oauth_stoken = seal_submission_token(
        &oauth_token,
        &submission_actor,
        "default",
        &oauth_cred,
        KdfParams::HkdfSha256(HkdfSha256Params),
    )
    .unwrap();
    write(
        "wrapped_submission_token_oauth.bin",
        &oauth_stoken.to_canonical_bytes().unwrap(),
    );

    // tls-cert
    let (_, mta_pub) = fauna_mls::wrapped_blob::generate_x25519_keypair();
    let tls_bundle = TlsCertBundle {
        cert_chain: b"cert chain bytes".to_vec(),
        priv_key: vec![0xEEu8; 32],
        expires_at: 1_700_000_000 + 90 * 86_400,
        issued_at: 1_700_000_000,
    };
    let tls_blob = seal_tls_cert(
        &tls_bundle,
        "mta",
        "bridge-deterministic",
        "example.com",
        &mta_pub,
    )
    .unwrap();
    write("tls_cert_blob.bin", &tls_blob.to_canonical_bytes().unwrap());
}
