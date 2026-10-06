//! The e2e source-trust seed: `FAUNA_E2E_IMAP_EXTRA_CA_PEM`, read inside
//! `NativeImapConnector::new`.
//!
//! `mailbox-migration.md` § The two TLS modes ratifies that a source IMAP
//! session has **no plaintext mode** — the user's password crosses it — so an
//! e2e walk of the mail-import wizard must point the app at a TLS server it can
//! verify, and a test rig cannot mint a publicly-chained certificate for
//! `localhost`. The app never builds its own connector either: the production
//! path is `NativeImapConnector::new`, several layers below the harness. The
//! seed closes exactly that gap, the same way the R14 (account-data-plane.md § The ratified decisions) nest-identity seed closes
//! its own (`e2e-automation-surface-gating.md` § The e2e trust seed).
//!
//! **Why this is its own test target, and therefore its own process.** The seed
//! is read from the *process* environment, so a test that sets it would reach
//! every other test in the same binary — including
//! `imap_client_native.rs`'s
//! `an_untrusted_source_certificate_fails_before_any_password_is_sent`, whose
//! entire assertion is that the default WebPKI roots refuse this exact kind of
//! self-signed certificate. Seeding it from a sibling test would turn that
//! test's failure into a silent pass. One process, one env var, no race.
//!
//! **And why one test function rather than three.** Same reason one step down:
//! `cargo test` runs a binary's tests on concurrent threads, and the phases
//! below deliberately disagree about what the environment holds.
#![cfg(any(debug_assertions, feature = "test-helpers"))]

use std::panic::{self, AssertUnwindSafe};

use fauna_mail::imap_client::test_fixtures::{TestCert, mint_cert, tls_acceptor};
use fauna_mail::imap_client::{NativeImapConnector, NativeTransportError, TlsMode};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// The variable's name is duplicated here on purpose rather than imported: the
/// constant is itself gated out of a release build, and this file's subject is
/// the *contract with the Python harness*, which knows only the string.
/// `tests/e2e-unified/tests/test_mail_import_tls_seam_gating.py` pins the two
/// spellings to each other so they cannot drift apart silently.
const SEED_ENV: &str = "FAUNA_E2E_IMAP_EXTRA_CA_PEM";

/// A TLS listener that accepts one connection and greets. The subject here is
/// the *handshake*, not IMAP — whether the client's trust store verifies this
/// certificate is settled before a single IMAP word is spoken, which is the
/// same boundary `an_untrusted_source_certificate_fails_before_any_password_is_sent`
/// asserts the password never crosses.
async fn spawn_tls_source(cert: &TestCert) -> u16 {
    let acceptor = tls_acceptor(cert);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        if let Ok(mut tls) = acceptor.accept(tcp).await {
            let _ = tls.write_all(b"* OK IMAP4rev1 ready\r\n").await;
            let _ = tls.flush().await;
        }
    });

    port
}

/// # Safety
///
/// Single-threaded by construction: the only caller is the one test below, and
/// this file is its own test binary precisely so nothing else in the process is
/// reading the environment concurrently (see the module docs).
fn set_seed(value: Option<&str>) {
    unsafe {
        match value {
            Some(v) => std::env::set_var(SEED_ENV, v),
            None => std::env::remove_var(SEED_ENV),
        }
    }
}

/// The seed's whole contract, in the order that makes each phase mean
/// something: without it the production connector refuses the harness's source,
/// with it the same call verifies, and a broken value fails loudly instead of
/// degrading into a refusal that reads like a wizard bug.
#[tokio::test]
async fn the_source_trust_seed_adds_an_anchor_and_never_weakens_verification() {
    let cert = mint_cert();

    // ── Phase 1: unseeded. This is the red the seed exists to turn green, and
    // it must keep failing — it is also the production posture, so a regression
    // here means a release build would trust something it should not.
    set_seed(None);
    let port = spawn_tls_source(&cert).await;
    let Err(err) = NativeImapConnector::new(TlsMode::Implicit)
        .connect("localhost", port)
        .await
    else {
        panic!(
            "with no {SEED_ENV} set, a self-signed source certificate must fail the \
             handshake — the default WebPKI roots chain to nothing here"
        );
    };
    assert!(
        matches!(err, NativeTransportError::Tls { .. }),
        "expected a TLS verification failure, got {err:?}"
    );

    // ── Phase 2: seeded with the harness's own CA. The connector is built by
    // the same `new()` the production path calls — that is the point: nothing
    // in `rpc_glue` or `fauna-ffi` had to opt in for the door to reach them.
    set_seed(Some(&cert.cert_pem));
    let port = spawn_tls_source(&cert).await;
    NativeImapConnector::new(TlsMode::Implicit)
        .connect("localhost", port)
        .await
        .expect("the seeded trust anchor must verify the harness's source server");

    // ── Phase 3: the seed ADDS an anchor; it is not an accept-any switch. A
    // *different* certificate is still refused while the seed is set, which is
    // what keeps this from being a re-introduction of the retired
    // `FAUNA_INSECURE_TLS` accept-any path.
    let other = mint_cert();
    let port = spawn_tls_source(&other).await;
    let Err(err) = NativeImapConnector::new(TlsMode::Implicit)
        .connect("localhost", port)
        .await
    else {
        panic!(
            "{SEED_ENV} must add ONE anchor, not disable verification — a server \
             presenting a different unverifiable certificate must still be refused"
        );
    };
    assert!(
        matches!(err, NativeTransportError::Tls { .. }),
        "expected a TLS verification failure, got {err:?}"
    );

    // ── Phase 4: a malformed value is loud. Silently ignoring it would hand the
    // suite a handshake error indistinguishable from a mail-import wizard bug —
    // the false-green shape convention 15's witnesses exist to prevent.
    set_seed(Some(
        "-----BEGIN RSA PRIVATE KEY-----\nnope\n-----END RSA PRIVATE KEY-----",
    ));
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {})); // the panic below is the assertion, not a failure
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        NativeImapConnector::new(TlsMode::Implicit)
    }));
    panic::set_hook(hook);
    assert!(
        outcome.is_err(),
        "a {SEED_ENV} that carries no CERTIFICATE block must panic, not be ignored"
    );

    set_seed(None);
}
