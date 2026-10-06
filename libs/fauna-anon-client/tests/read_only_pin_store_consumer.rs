//! Pin-consumer graduation proof for `cert_binding::ReadOnlyDiskPinStore`
//! (security.md § Transport trust — pin custody across processes).
//!
//! A consumer process (a File Provider extension, a background sync agent)
//! installs the pin store read-only: it must **never mint** a TOFU pin — an
//! empty store fails the connect (`IdentityError::PinRequired`) instead of
//! silently trusting the first nest reached — and it must pick up a pin the
//! interactive app writes *after* the consumer launched, on the very next
//! graduation, without a process relaunch (the store is uncached).
//!
//! Lives in its own integration-test binary because `install_pin_store` mutates
//! a process-global `OnceLock`; an isolated process keeps the read-only install
//! from racing the in-crate unit tests (and the writer-store integration test)
//! that share that global.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_anon_client::cert_binding::{DiskPinStore, NestIdentityPinStore, ReadOnlyDiskPinStore};
use fauna_anon_client::tls_verify::CapturedCert;
use fauna_anon_client::trust::{self, TrustError};
use fauna_protocol::auth::CertBinding;

/// Forge the nest→client channel binding the way `auth_handlers::build_cert_binding`
/// does — through the one shared producer.
fn make_binding(nest: &SigningKey, spki: &[u8; 32], nonce: &[u8]) -> CertBinding {
    CertBinding::sign(nest, spki, nonce)
}

/// A unique temp dir without a tempfile dep — `trust::fresh_nonce` already
/// gives us OS randomness through the crate's public API.
fn unique_temp_dir() -> std::path::PathBuf {
    let r = trust::fresh_nonce();
    let dir = std::env::temp_dir().join(format!("fauna-ro-pins-{}", hex::encode(r)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn consumer_refuses_unpinned_then_verifies_the_app_minted_pin() {
    let dir = unique_temp_dir();

    let host = "pi.local:8443";
    let nest = SigningKey::from_bytes(&[9u8; 32]);
    let spki = [0x11u8; 32];
    let cap = CapturedCert {
        spki: Some(spki),
        webpki_valid: false,
    };

    // --- Consumer launch: read-only store over a dir with no pin file yet. ---
    trust::install_pin_store(Arc::new(ReadOnlyDiskPinStore::open_in_dir(&dir)));

    // A valid binding from a TOFU-rooted nest with no pin must be REFUSED, not
    // minted — the discriminating assertion: the interactive `Tofu` arm would
    // return Ok(Pinned) here.
    let nonce1 = trust::fresh_nonce();
    let binding1 = make_binding(&nest, &spki, &nonce1);
    let err = trust::graduate_handshake_with_root(host, &cap, &nonce1, Some(&binding1), None)
        .expect_err("a pin-consumer must refuse an unpinned TOFU host");
    assert!(
        matches!(err, TrustError::Identity(_)),
        "unpinned host fails the strict identity check, got {err:?}"
    );
    assert_eq!(
        DiskPinStore::open_in_dir(&dir).get(host),
        None,
        "the consumer must not have minted a pin"
    );

    // --- The interactive app pins the nest (onboarding / migration), in its own
    // writer store, while the consumer's read-only install stays live. ---
    DiskPinStore::open_in_dir(&dir).set(host, nest.verifying_key().to_bytes());

    // The consumer's next graduation sees the app's pin — no reinstall, no
    // relaunch — and verifies.
    let nonce2 = trust::fresh_nonce();
    let binding2 = make_binding(&nest, &spki, &nonce2);
    trust::graduate_handshake_with_root(host, &cap, &nonce2, Some(&binding2), None)
        .expect("the app-minted pin is visible to the live consumer and verifies");

    // A different identity at the pinned host still warns loudly.
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
            .expect_err("a changed identity must be rejected by the consumer");
    assert!(
        matches!(err, TrustError::Identity(_)),
        "changed identity fails the pin check, got {err:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
