//! Restart-survives-pin proof for the disk-backed nest-identity pin store
//! (security.md § Transport trust — Axis 2 TOFU; a deferred deliverable).
//!
//! The `cert_binding::DiskPinStore` reopen round-trip is unit-tested in
//! `cert_binding.rs`; what this binary proves is the *combination through the
//! process-global trust state* that every app relies on at startup: after
//! `trust::install_pin_store(DiskPinStore::open(path))`, a TOFU pin learned by
//! `graduate_handshake_with_root` (the sync verification core — this is a TOFU
//! `.local` host, so the DNS-resolving async wrapper would resolve no root) is
//! written to disk, and after a *fresh* store is
//! installed from the same path (a simulated restart) a **different** identity
//! at that host is rejected — which can only happen if the original pin
//! survived to disk and was reloaded.
//!
//! It lives in its own integration-test binary (not a `#[cfg(test)]` unit test)
//! because `install_pin_store` mutates a process-global `OnceLock`; an isolated
//! process keeps it from racing the in-crate unit tests that share that global.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_anon_client::cert_binding::{DiskPinStore, NestIdentityPinStore};
use fauna_anon_client::tls_verify::CapturedCert;
use fauna_anon_client::trust::{self, TrustError};
use fauna_protocol::auth::CertBinding;

/// Forge the nest→client channel binding the way `auth_handlers::build_cert_binding`
/// does — through the one shared producer.
fn make_binding(nest: &SigningKey, spki: &[u8; 32], nonce: &[u8]) -> CertBinding {
    CertBinding::sign(nest, spki, nonce)
}

/// A unique temp path without a tempfile dep — `trust::fresh_nonce` already
/// gives us OS randomness through the crate's public API.
fn unique_temp_path() -> std::path::PathBuf {
    let r = trust::fresh_nonce();
    std::env::temp_dir().join(format!("fauna-pins-{}.json", hex::encode(r)))
}

#[test]
fn tofu_pin_survives_a_simulated_restart() {
    let path = unique_temp_path();
    let _ = std::fs::remove_file(&path);

    let host = "pi.local:8443";
    let nest = SigningKey::from_bytes(&[9u8; 32]);
    let spki = [0x11u8; 32];

    // --- First launch: install the disk store and TOFU-pin the nest. ---
    trust::install_pin_store(Arc::new(DiskPinStore::open(path.clone())));
    let nonce1 = trust::fresh_nonce();
    let binding1 = make_binding(&nest, &spki, &nonce1);
    let cap = CapturedCert {
        spki: Some(spki),
        webpki_valid: false,
    };
    trust::graduate_handshake_with_root(host, &cap, &nonce1, Some(&binding1), None)
        .expect("first connect TOFU-pins the self-signed nest");

    // The pin must have been written to disk by the install-time store.
    assert!(
        DiskPinStore::open(path.clone()).get(host).is_some(),
        "pin was persisted to disk on first connect"
    );

    // --- Simulated restart: a brand-new store opened from the same file. ---
    trust::install_pin_store(Arc::new(DiskPinStore::open(path.clone())));

    // Same identity reconnecting → confirmed, no error (the pin was reloaded).
    let nonce2 = trust::fresh_nonce();
    let binding2 = make_binding(&nest, &spki, &nonce2);
    trust::graduate_handshake_with_root(host, &cap, &nonce2, Some(&binding2), None)
        .expect("same nest identity confirms against the restored pin");

    // A *different* identity at the same host must be rejected — the
    // discriminating assertion: if the pin had been lost on restart this would
    // silently re-pin (Ok) instead of failing closed.
    let imposter = SigningKey::from_bytes(&[42u8; 32]);
    let imposter_spki = [0x22u8; 32];
    let nonce3 = trust::fresh_nonce();
    let binding3 = make_binding(&imposter, &imposter_spki, &nonce3);
    let cap_imposter = CapturedCert {
        spki: Some(imposter_spki),
        webpki_valid: false,
    };
    let err =
        trust::graduate_handshake_with_root(host, &cap_imposter, &nonce3, Some(&binding3), None)
            .expect_err("a changed identity must be rejected against the restored pin");
    assert!(
        matches!(err, TrustError::Identity(_)),
        "changed identity fails the TOFU identity check, got {err:?}"
    );

    let _ = std::fs::remove_file(&path);
}
