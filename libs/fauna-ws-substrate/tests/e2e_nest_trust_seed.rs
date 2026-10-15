//! The e2e nest-leg trust seed: `FAUNA_E2E_NEST_EXTRA_CA_PEM`, read inside
//! `tls_verify::webpki_verifier` — the one verifier every app nest leg
//! classifies a served certificate through
//! (`docs/goal/architecture/e2e-automation-surface-gating.md` § The nest-leg
//! trust seed).
//!
//! **Why this is its own test target, and therefore its own process.** The
//! seed is read from the *process* environment, so a test that set it would
//! reach every other test in the same binary — including `tls_verify`'s own
//! `captures_spki_and_marks_self_signed_invalid`, whose assertion is that the
//! default roots call a self-signed certificate not WebPKI-valid. One process,
//! one env var, no race.
//!
//! **And why one test function rather than four.** `cargo test` runs a
//! binary's tests on concurrent threads, and the phases below deliberately
//! disagree about what the environment holds.
//!
//! The observable is the public one the anon client reads after a handshake:
//! [`CapturedCert::webpki_valid`] from [`capturing_client_config`], over a real
//! (in-memory) rustls handshake, dialled by IP address — the journey's dial is
//! the nest's LAN address, so this also proves rustls accepts an IP SAN under
//! `ServerName::IpAddress`.
#![cfg(any(debug_assertions, feature = "test-helpers"))]

use std::net::{IpAddr, Ipv4Addr};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use fauna_ws_substrate::tls_verify::{CapturedCert, capturing_client_config};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};

/// Duplicated on purpose rather than imported: this file's subject is the
/// contract with the Python harness, which knows only the string.
/// `tests/e2e-unified/tests/test_nest_leg_tls_seam_gating.py` pins the two
/// spellings to each other.
const SEED_ENV: &str = "FAUNA_E2E_NEST_EXTRA_CA_PEM";

const DIAL_IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

struct Ca {
    cert: rcgen::Certificate,
    key: KeyPair,
}

fn mint_ca() -> Ca {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    Ca { cert, key }
}

/// A leaf for [`DIAL_IP`] signed by `ca`: `(der, pkcs8 key der)`.
fn ca_issued_ip_leaf(ca: &Ca) -> (Vec<u8>, Vec<u8>) {
    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![SanType::IpAddress(DIAL_IP)];
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &ca.cert, &ca.key).unwrap();
    (cert.der().to_vec(), key.serialize_der())
}

/// A self-signed leaf for [`DIAL_IP`] — the e2e nest's floor shape.
fn self_signed_ip_leaf() -> (Vec<u8>, Vec<u8>) {
    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![SanType::IpAddress(DIAL_IP)];
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    (cert.der().to_vec(), key.serialize_der())
}

/// Run one in-memory TLS handshake from a fresh `capturing_client_config`
/// (built NOW, so it reads the environment as it stands) against a server
/// presenting `leaf`, dialled as [`DIAL_IP`]; return what the verifier
/// captured.
fn classify(leaf: &(Vec<u8>, Vec<u8>)) -> CapturedCert {
    fauna_ws_substrate::ensure_tls_provider();
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(leaf.0.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf.1.clone())),
        )
        .unwrap();
    let (client_config, capture) = capturing_client_config();
    let mut client =
        rustls::ClientConnection::new(client_config, ServerName::IpAddress(DIAL_IP.into()))
            .unwrap();
    let mut server = rustls::ServerConnection::new(Arc::new(server_config)).unwrap();

    // Pump TLS records both ways until neither side has anything to say.
    for _ in 0..32 {
        let mut progressed = false;
        let mut buf = Vec::new();
        while client.wants_write() {
            client.write_tls(&mut buf).unwrap();
        }
        if !buf.is_empty() {
            progressed = true;
            server.read_tls(&mut buf.as_slice()).unwrap();
            server.process_new_packets().unwrap();
        }
        let mut buf = Vec::new();
        while server.wants_write() {
            server.write_tls(&mut buf).unwrap();
        }
        if !buf.is_empty() {
            progressed = true;
            client.read_tls(&mut buf.as_slice()).unwrap();
            client.process_new_packets().unwrap();
        }
        if !progressed {
            break;
        }
    }
    assert!(
        !client.is_handshaking(),
        "the in-memory handshake did not complete"
    );
    capture.lock().unwrap().clone()
}

#[test]
fn the_seed_adds_the_harness_ca_and_nothing_else() {
    // SAFETY (edition 2024 `set_var`): this binary has exactly one test, so
    // no other thread reads the environment concurrently.
    unsafe { std::env::remove_var(SEED_ENV) };

    let ca = mint_ca();
    let issued = ca_issued_ip_leaf(&ca);
    let self_signed = self_signed_ip_leaf();

    // ── Phase 1: unseeded, a harness-CA leaf is NOT publicly trusted. ──
    let before = classify(&issued);
    assert!(before.spki.is_some(), "the leaf's SPKI was captured");
    assert!(
        !before.webpki_valid,
        "with no seed, a leaf from a private CA must not read as WebPKI-valid"
    );

    // ── Phase 2: seeded with the CA, the same leaf IS — dialled by IP. ──
    unsafe { std::env::set_var(SEED_ENV, ca.cert.pem()) };
    let after = classify(&issued);
    assert!(
        after.webpki_valid,
        "with {SEED_ENV} naming the issuing CA, an IP-SAN leaf dialled as \
         ServerName::IpAddress must classify as WebPKI-valid — the branch the \
         nest-leg journey exists to reach"
    );

    // ── Phase 3: additive only — a self-signed leaf stays untrusted. ──
    let ss = classify(&self_signed);
    assert!(
        !ss.webpki_valid,
        "the seed must only ADD the harness CA; a self-signed leaf stays \
         not-WebPKI-valid under it"
    );

    // ── Phase 4: a malformed value panics, naming the variable. ──
    // A CERTIFICATE block whose base64 is garbage. Assembled from parts so no
    // literal PEM block sits in the source (the publish secret scan flags one).
    let bad_block = format!(
        "-----BEGIN {0}-----\n!!!!\n-----END {0}-----\n",
        "CERTIFICATE"
    );
    for bad in ["not a pem at all", bad_block.as_str()] {
        unsafe { std::env::set_var(SEED_ENV, bad) };
        let caught = panic::catch_unwind(AssertUnwindSafe(capturing_client_config));
        let err = caught.expect_err("a malformed seed must panic, never degrade silently");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(
            msg.contains(SEED_ENV),
            "the panic must name {SEED_ENV}; got: {msg}"
        );
    }

    // ── Phase 5: an empty value is "unset", not malformed. ──
    unsafe { std::env::set_var(SEED_ENV, "  ") };
    assert!(!classify(&issued).webpki_valid);
    unsafe { std::env::remove_var(SEED_ENV) };
}
