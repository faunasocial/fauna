//! Regenerate Go-side testdata fixtures under the canonical CBOR
//! encoder (Tasks 2.1–2.2 of the CBOR-DAG-everywhere Layer 2 plan).
//!
//! Run from the workspace root:
//!
//!     cargo run -p fauna-mls --example regen_go_testdata
//!
//! Produces:
//!   bins/fauna-bridges/internal/keypair/testdata/rust-keyfile.cbor
//!   bins/fauna-bridges/internal/tls/testdata/wrapped-cert.cbor
//!
//! Determinism:
//! - `rust-keyfile.cbor`: fully byte-deterministic (no AEAD, no HPKE);
//!   the values in the cross-language fixture roundtrip test
//!   (`TestCrossLanguageFixtureRoundtrip` in
//!   `internal/keypair/keyfile_test.go`) are hardcoded below.
//! - `wrapped-cert.cbor`: NOT byte-deterministic across regenerations
//!   — HPKE seals against a fresh X25519 ephemeral each run. The Go
//!   test (`tls_test.go`) only checks HPKE-open + x509 parse on the
//!   resulting leaf cert, so any sealing under the documented
//!   `(role, bridge_id, domain)` index and recipient secret `[1u8; 32]`
//!   that carries a CN=`test.example.com` cert satisfies its
//!   contracts. We preserve the cert/key plaintext from the previous
//!   committed fixture by HPKE-Opening it (the old fixture was sealed
//!   with the same AAD shape, which is byte-stable under the encoder
//!   flip) and re-seal under the new encoder.

use fauna_mls::wrapped_blob::{ServiceUserKeyfile, TlsCertBlob, seal_tls_cert, unseal_tls_cert};
use serde_bytes::ByteBuf;
use std::path::{Path, PathBuf};
use x25519_dalek::{PublicKey, StaticSecret};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = libs/fauna-mls; go up twice for repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn write(path: PathBuf, bytes: &[u8]) {
    std::fs::write(&path, bytes).expect("write fixture");
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
}

/// Derive an X25519 public key from a fixed 32-byte secret seed.
/// Mirrors what the Go side does when it loads the secret and asks
/// HPKE for the matching pubkey.
fn x25519_pub_from_secret(secret_bytes: [u8; 32]) -> [u8; 32] {
    let sk = StaticSecret::from(secret_bytes);
    let pk: PublicKey = (&sk).into();
    pk.to_bytes()
}

fn regen_keyfile(root: &Path) {
    // Values pinned by `TestCrossLanguageFixtureRoundtrip` in
    // bins/fauna-bridges/internal/keypair/keyfile_test.go.
    let kf = ServiceUserKeyfile {
        version: 1,
        role: "mta".into(),
        bridge_id: "test-bridge".into(),
        ed25519_seed: ByteBuf::from(vec![0x00u8; 32]),
        x25519_priv: ByteBuf::from(vec![0xFFu8; 32]),
        created_at: 1_700_000_000,
    };
    let bytes = kf.to_bytes().expect("encode keyfile");
    let out = root.join("bins/fauna-bridges/internal/keypair/testdata/rust-keyfile.cbor");
    write(out, &bytes);
}

fn regen_wrapped_cert(root: &Path) {
    let recipient_secret = [0x01u8; 32];
    let recipient_pub = x25519_pub_from_secret(recipient_secret);

    let path = root.join("bins/fauna-bridges/internal/tls/testdata/wrapped-cert.cbor");

    // Round-trip the existing fixture: open with the documented
    // recipient secret, then re-seal under the new encoder. This
    // preserves the embedded cert/key PEMs (whose CN matches what the
    // Go test x509-parses).
    let prior_bytes = std::fs::read(&path).expect(
        "wrapped-cert.cbor must already exist; if seeding a fresh \
         fixture from scratch, embed PEMs in this example",
    );
    let prior_blob =
        TlsCertBlob::from_canonical_bytes(&prior_bytes).expect("decode prior wrapped-cert");
    let prior_bundle =
        unseal_tls_cert(&prior_blob, &recipient_secret).expect("HPKE-open prior wrapped-cert");

    let new_blob = seal_tls_cert(
        &prior_bundle,
        "mta",
        "test-bridge",
        "test.example.com",
        &recipient_pub,
    )
    .expect("seal new wrapped-cert");
    let bytes = new_blob
        .to_canonical_bytes()
        .expect("encode new wrapped-cert");
    write(path, &bytes);
}

/// Regenerate every fixture, or only the named ones.
///
/// `wrapped-cert.cbor` re-seals under a fresh HPKE ephemeral each run, so a
/// blanket regeneration rewrites a file the caller may never have meant to
/// touch and lands unrelated churn in their commit. Naming the fixture you are
/// regenerating keeps the diff honest:
///
///     cargo run -p fauna-mls --example regen_go_testdata -- keyfile
fn main() {
    let root = workspace_root();
    let only: Vec<String> = std::env::args().skip(1).collect();
    let wanted = |name: &str| only.is_empty() || only.iter().any(|a| a == name);

    if wanted("keyfile") {
        regen_keyfile(&root);
    }
    if wanted("wrapped-cert") {
        regen_wrapped_cert(&root);
    }
}
