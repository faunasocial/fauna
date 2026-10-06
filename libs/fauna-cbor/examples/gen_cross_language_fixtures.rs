//! Generates the cross-language interop fixture under
//! `libs/fauna-cbor/tests/fixtures/cross-language/`.
//!
//! The fixture proves the load-bearing claim of `docs/goal/architecture/serialization.md`
//! § 4: any language with BLAKE3 + Ed25519 + a dag-cbor decoder can verify
//! Rust-produced content.
//!
//! Three files are written (deterministic — fixed seed `[7u8; 32]`):
//!
//!   - `rust-signed.bin`          — canonical dag-cbor bytes for the demo payload.
//!   - `rust-signed.envelope.bin` — 100 bytes: 36-byte CID || 64-byte Ed25519 sig.
//!   - `rust-pubkey.bin`          — 32 bytes: the Ed25519 verifying key.
//!
//! The Go-side verifier
//! (`bins/fauna-bridges/internal/dagcbor/cross_language_test.go`) reads
//! all three files and runs:
//!
//!   1. `dagcbor.ValidateCanonical(bytes)` — canonical-form check.
//!   2. `blake3(bytes)` equals the 32-byte digest at `envelope[4..36]`.
//!   3. `ed25519.Verify(pubkey, envelope[0..36], envelope[36..100])`.
//!
//! Demo payload is an ad-hoc `Demo` struct (NOT `WrappedMsek`) so this example
//! stays in `fauna-cbor` without taking a dep on `fauna-mls`. The contract being
//! proven is the canonical-bytes + envelope round-trip across languages, not the
//! specific payload shape.
//!
//! Run with: `cargo run -p fauna-cbor --example gen_cross_language_fixtures`

use std::fs;
use std::path::PathBuf;

use ed25519_dalek::SigningKey;
use fauna_cbor::SignedEnvelope;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

/// Demo payload: a small struct with a few primitive fields and a byte blob.
/// Field names are deliberately mixed lengths so the canonical-form
/// length-first-then-bytewise key sort gets exercised (sorted order:
/// `v`, `name`, `data`).
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Demo {
    v: u32,
    name: String,
    data: ByteBuf,
}

fn fixture_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR points at libs/fauna-cbor when running examples for this crate.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    PathBuf::from(manifest).join("tests/fixtures/cross-language")
}

fn main() {
    // Fixed seed for determinism — matches the Layer 1 envelope tests so the
    // verifying key and signature stay stable across re-runs.
    let sk = SigningKey::from_bytes(&[7u8; 32]);
    let pk = sk.verifying_key();

    let payload = Demo {
        v: 42,
        name: "fauna".to_string(),
        data: ByteBuf::from(vec![0x01, 0x02, 0x03, 0x04, 0x05]),
    };

    let (bytes, env) = SignedEnvelope::sign(&payload, &sk).expect("sign demo payload");

    // Envelope wire layout: 36-byte CID || 64-byte sig = 100 bytes total.
    // No wrapping format — the Go verifier reads the same fixed offsets.
    let mut envelope_bytes = Vec::with_capacity(36 + 64);
    envelope_bytes.extend_from_slice(env.cid().as_bytes());
    envelope_bytes.extend_from_slice(env.sig());
    assert_eq!(
        envelope_bytes.len(),
        100,
        "envelope must be 36+64 = 100 bytes"
    );

    let dir = fixture_dir();
    fs::create_dir_all(&dir).expect("create fixture dir");

    fs::write(dir.join("rust-signed.bin"), &bytes).expect("write rust-signed.bin");
    fs::write(dir.join("rust-signed.envelope.bin"), &envelope_bytes)
        .expect("write rust-signed.envelope.bin");
    fs::write(dir.join("rust-pubkey.bin"), pk.to_bytes()).expect("write rust-pubkey.bin");

    eprintln!(
        "wrote cross-language fixture to {}: signed={} bytes, envelope={} bytes, pubkey={} bytes",
        dir.display(),
        bytes.len(),
        envelope_bytes.len(),
        pk.to_bytes().len(),
    );
}
