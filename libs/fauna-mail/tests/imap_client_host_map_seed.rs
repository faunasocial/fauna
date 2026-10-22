//! The preset-host redirect seed: `FAUNA_E2E_IMAP_HOST_MAP`, read inside
//! `NativeImapConnector::new` (`e2e-gated-seeds.md` § The preset-host redirect
//! seed).
//!
//! An Outlook pick dials the fixed preset `outlook.office365.com:993`, so the
//! Outlook import's `AUTHENTICATE XOAUTH2` leg had no server a test could
//! reach. The seed moves the DIAL of a mapped host to a loopback address and
//! nothing else: the handshake still verifies the provider's own name, so the
//! far end must present a leaf for that name.
//!
//! **Its own test target, and therefore its own process**, for the reason
//! `imap_client_trust_seed.rs` gives: the seed is read from the process
//! environment, so setting it would reach every other test in a shared binary.
//! One test function for the same reason one step down — `cargo test` runs a
//! binary's tests on concurrent threads, and the phases below disagree about
//! what the environment holds. The trust anchor is added with
//! `with_extra_root_pem` rather than the trust seed's variable, so this process
//! sets exactly one.
#![cfg(any(debug_assertions, feature = "test-helpers"))]

use std::panic::{self, AssertUnwindSafe};

use fauna_mail::imap_client::test_fixtures::{TestCert, mint_cert, mint_cert_for, tls_acceptor};
use fauna_mail::imap_client::{NativeImapConnector, NativeTransportError, TlsMode};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Duplicated rather than imported, as in `imap_client_trust_seed.rs`: the
/// constant is gated out of a release build, and this file's subject is the
/// contract with the Python harness, which knows only the string.
/// `tests/e2e-unified/tests/test_mail_import_tls_seam_gating.py` pins the
/// spellings to each other.
const SEED_ENV: &str = "FAUNA_E2E_IMAP_HOST_MAP";

/// The one host the harness maps — `provider_preset`'s Outlook arm.
const OUTLOOK: &str = "outlook.office365.com";

/// A TLS listener that accepts one connection and greets; the subject is the
/// dial and the handshake, not IMAP. The receiver resolves once THIS listener
/// has completed a handshake — what tells a redirected dial apart from one
/// that reached the real host, whose certificate would verify too.
async fn spawn_tls_source(cert: &TestCert) -> (u16, oneshot::Receiver<()>) {
    let acceptor = tls_acceptor(cert);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (handshaken, rx) = oneshot::channel();

    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        if let Ok(mut tls) = acceptor.accept(tcp).await {
            let _ = handshaken.send(());
            let _ = tls.write_all(b"* OK IMAP4rev1 ready\r\n").await;
            let _ = tls.flush().await;
        }
    });

    (port, rx)
}

/// # Safety
///
/// Single-threaded by construction: the only caller is the one test below, and
/// this file is its own test binary (see the module docs).
fn set_seed(value: Option<&str>) {
    unsafe {
        match value {
            Some(v) => std::env::set_var(SEED_ENV, v),
            None => std::env::remove_var(SEED_ENV),
        }
    }
}

/// A connector built by the production `new()`, trusting `cert` as an extra
/// anchor.
fn connector_trusting(cert: &TestCert) -> NativeImapConnector {
    NativeImapConnector::new(TlsMode::Implicit)
        .with_extra_root_pem(&cert.cert_pem)
        .unwrap()
}

/// `new()` panics with the variable named in its message.
fn assert_new_panics_naming_the_seed(value: &str) {
    set_seed(Some(value));
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {})); // the panic below is the assertion
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        NativeImapConnector::new(TlsMode::Implicit)
    }));
    panic::set_hook(hook);
    let Err(payload) = outcome else {
        panic!("{SEED_ENV}={value:?} must panic, not be ignored");
    };
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        message.contains(SEED_ENV),
        "the panic for {SEED_ENV}={value:?} must name the variable, got {message:?}"
    );
}

/// The seed's whole contract, in order: a mapped host is dialled at its
/// loopback entry and verified under its own name; a leaf for any other name
/// fails there; an unmapped host is dialled as given; a value that would point
/// off the box, or that does not parse, is loud.
#[tokio::test]
async fn the_host_map_moves_the_dial_and_keeps_the_verified_name() {
    // ── Phase 1: mapped, and the far end presents a leaf for the Outlook name.
    // The caller asks for Outlook's real port; the map's entry wins.
    let outlook = mint_cert_for(OUTLOOK);
    let (port, handshaken) = spawn_tls_source(&outlook).await;
    set_seed(Some(&format!("{OUTLOOK}=127.0.0.1:{port}")));
    let _transport = connector_trusting(&outlook)
        .connect(OUTLOOK, 993)
        .await
        .expect(
            "a mapped host must be dialled at its loopback entry and verify under its own name",
        );
    tokio::time::timeout(std::time::Duration::from_secs(10), handshaken)
        .await
        .expect("the harness listener never completed the handshake")
        .expect("the harness listener never completed the handshake");

    // ── Phase 2: mapped, but the far end presents a leaf for `localhost`. The
    // verified name is still Outlook's, so the handshake refuses — the seed
    // moved the dial, not the name — and the error names the host the caller
    // gave, as the wizard shows it.
    let local = mint_cert();
    let (port, _) = spawn_tls_source(&local).await;
    set_seed(Some(&format!("{OUTLOOK}=127.0.0.1:{port}")));
    let Err(err) = connector_trusting(&local).connect(OUTLOOK, 993).await else {
        panic!("a leaf for `localhost` must not verify under the mapped Outlook name");
    };
    match err {
        NativeTransportError::Tls { host, .. } => assert_eq!(host, OUTLOOK),
        other => panic!("expected a TLS verification failure, got {other:?}"),
    }

    // ── Phase 3: an unmapped host is dialled exactly as given, map set or not.
    let (port, _) = spawn_tls_source(&local).await;
    set_seed(Some(&format!("{OUTLOOK}=127.0.0.1:1")));
    connector_trusting(&local)
        .connect("localhost", port)
        .await
        .expect("an unmapped host must be dialled as given");

    // ── Phase 4: loud refusals, each naming the variable.
    assert_new_panics_naming_the_seed(&format!("{OUTLOOK}=192.0.2.1:993"));
    assert_new_panics_naming_the_seed("garbage");

    set_seed(None);
}
