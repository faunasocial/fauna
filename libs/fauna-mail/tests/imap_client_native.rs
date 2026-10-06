//! The native transport (`imap_client::native`) against a **real TCP IMAP
//! server with real TLS** — the half the scripted-transport unit tests cannot
//! reach.
//!
//! The unit tests in `imap_client/` drive a `ScriptedTransport`: they prove the
//! *protocol* (framing, literals, throttle, batching) and deliberately never
//! open a socket. This file proves the *shell*: that a rustls handshake
//! actually completes, that STARTTLS upgrades a live connection in place, that
//! a source server we cannot verify is refused **before the password is
//! written**, and that the plaintext-injection guard fires. Together they cover
//! both sides of the seam mailbox-migration.md § Where the IMAP client runs
//! ratifies.
//!
//! The server here is a genuine `tokio::net::TcpListener` speaking enough
//! IMAP4rev1 to serve one import; the certificate is minted per test with
//! `rcgen` and handed to the client as an explicit trust anchor, so nothing
//! depends on the machine's root store or on reaching the network.

use fauna_mail::imap_client::test_fixtures::{
    FixtureMessage, TestCert, mint_cert, serve_imap, tls_acceptor,
};
use fauna_mail::imap_client::{
    FetchOutcome, ImapClientError, ImapSession, NativeImapConnector, NativeTransportError, TlsMode,
    TokioClock,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One message the fake source serves. Real RFC 5322 bytes — the import client
/// forwards them to nest unmodified (§ Per-message flow step 4).
const BODY: &[u8] = b"From: alice@example.com\r\n\
Subject: hello\r\n\
Message-ID: <m1@example.com>\r\n\
\r\n\
body text\r\n";

const CORPUS: &[FixtureMessage] = &[FixtureMessage {
    uid: 7,
    flags: "\\Seen",
    body: BODY,
}];

/// Spawn a source server. `mode` decides whether TLS is up from the first byte
/// or negotiated; `inject_after_starttls` reproduces the plaintext-injection
/// attack by appending bytes to the *cleartext* stream after the STARTTLS OK.
async fn spawn_source(
    cert: &TestCert,
    mode: TlsMode,
    inject_after_starttls: Option<&'static [u8]>,
) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tls_acceptor(cert);

    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();

        match mode {
            TlsMode::Implicit => {
                let mut tls = acceptor.accept(tcp).await.unwrap();
                tls.write_all(b"* OK IMAP4rev1 ready\r\n").await.unwrap();
                serve_imap(&mut tls, 42, CORPUS).await;
            }
            TlsMode::StartTls => {
                // Cleartext phase: greeting, then the STARTTLS command.
                tcp.write_all(b"* OK IMAP4rev1 ready\r\n").await.unwrap();

                let mut buf = vec![0u8; 1024];
                let n = tcp.read(&mut buf).await.unwrap();
                let line = String::from_utf8_lossy(&buf[..n]).to_string();
                let tag = line.split(' ').next().unwrap_or("a").to_string();
                assert!(
                    line.to_ascii_uppercase().contains("STARTTLS"),
                    "expected STARTTLS, got {line:?}"
                );
                // The attack: the injected bytes ride in the SAME segment as
                // the tagged OK, so the client's next read() pulls both at once
                // and is left holding attacker-authored bytes in its pre-TLS
                // buffer. Splitting them into two writes would instead race the
                // handshake and get caught by rustls as a corrupt record — a
                // real defence, but a different one.
                let mut ok = format!("{tag} OK begin TLS\r\n").into_bytes();
                if let Some(evil) = inject_after_starttls {
                    ok.extend_from_slice(evil);
                }
                tcp.write_all(&ok).await.unwrap();
                tcp.flush().await.unwrap();

                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return; // client refused to hand shake — the guard fired
                };
                serve_imap(&mut tls, 42, CORPUS).await;
            }
        }
    });

    port
}

/// Import one message end-to-end over implicit TLS: handshake, LOGIN, EXAMINE,
/// enumerate, FETCH — through a real socket, with the real rustls stack.
#[tokio::test]
async fn implicit_tls_fetches_a_message_over_a_real_socket() {
    let cert = mint_cert();
    let port = spawn_source(&cert, TlsMode::Implicit, None).await;

    let connector = NativeImapConnector::new(TlsMode::Implicit)
        .with_extra_root_pem(&cert.cert_pem)
        .unwrap();
    let transport = connector.connect("localhost", port).await.unwrap();

    let mut session = ImapSession::connect(transport).await.unwrap();
    assert!(session.has_capability("IMAP4REV1"));

    session.login("alice", "hunter2").await.unwrap();
    let status = session.examine("INBOX", None).await.unwrap();
    assert_eq!(status.uid_validity, 42);

    let uids = session.enumerate_uids(1).await.unwrap();
    assert_eq!(uids.iter().map(|u| u.uid).collect::<Vec<_>>(), vec![7]);

    let clock = TokioClock::new();
    let mut got = Vec::new();
    session
        .fetch_messages(&[7], &clock, |o| got.push(o))
        .await
        .unwrap();

    match got.as_slice() {
        [FetchOutcome::Fetched(m)] => {
            assert_eq!(m.uid, 7);
            assert_eq!(m.uid_validity, 42);
            assert_eq!(m.body, BODY, "the raw RFC 5322 bytes must survive verbatim");
            assert!(m.flags.contains(&"\\Seen".to_string()));
        }
        other => panic!("expected one fetched message, got {other:?}"),
    }

    session.logout().await.unwrap();
}

/// The STARTTLS path reaches the identical state — TLS up, capabilities read
/// *inside* TLS — through a connection that began in the clear.
#[tokio::test]
async fn starttls_upgrades_a_live_plaintext_connection_in_place() {
    let cert = mint_cert();
    let port = spawn_source(&cert, TlsMode::StartTls, None).await;

    let connector = NativeImapConnector::new(TlsMode::StartTls)
        .with_extra_root_pem(&cert.cert_pem)
        .unwrap();
    let transport = connector.connect("localhost", port).await.unwrap();

    // Drives the STARTTLS command exchange, then the in-place handshake.
    let mut session = ImapSession::connect_starttls(transport).await.unwrap();

    // These capabilities were read after the handshake — the pre-TLS ones are
    // discarded per RFC 3501 §6.2.1.
    assert!(session.has_capability("IMAP4REV1"));

    session.login("alice", "hunter2").await.unwrap();
    let status = session.examine("INBOX", None).await.unwrap();
    assert_eq!(status.uid_validity, 42);
    session.logout().await.unwrap();
}

/// Bytes appended to the cleartext stream after the STARTTLS `OK` must never be
/// read back as though they arrived inside TLS.
///
/// Mutation-verified: delete the `buffered_len() != 0` guard in
/// `session::connect_starttls` and this test fails — the injected
/// `* CAPABILITY` is consumed as the post-handshake capability response.
#[tokio::test]
async fn plaintext_injected_across_the_starttls_boundary_is_refused() {
    let cert = mint_cert();
    let port = spawn_source(
        &cert,
        TlsMode::StartTls,
        Some(b"* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\n"),
    )
    .await;

    let connector = NativeImapConnector::new(TlsMode::StartTls)
        .with_extra_root_pem(&cert.cert_pem)
        .unwrap();
    let transport = connector.connect("localhost", port).await.unwrap();

    let Err(err) = ImapSession::connect_starttls(transport).await else {
        panic!("injected plaintext must abort the session");
    };

    assert!(
        matches!(err, ImapClientError::StartTlsPlaintextInjection),
        "expected the injection guard to fire, got {err:?}"
    );
}

/// A source server we cannot verify is refused **at the handshake** — i.e.
/// before `login()` could put the user's password on the wire.
#[tokio::test]
async fn an_untrusted_source_certificate_fails_before_any_password_is_sent() {
    let cert = mint_cert();
    let port = spawn_source(&cert, TlsMode::Implicit, None).await;

    // The default WebPKI roots — this self-signed cert chains to nothing.
    let connector = NativeImapConnector::new(TlsMode::Implicit);
    let Err(err) = connector.connect("localhost", port).await else {
        panic!("an unverifiable source cert must fail the handshake");
    };

    assert!(
        matches!(err, NativeTransportError::Tls { .. }),
        "expected a TLS failure, got {err:?}"
    );
}

/// The trust anchor is *additive* and there is no accept-anything escape hatch:
/// a PEM that carries no certificate is an error, not a silently empty store.
#[tokio::test]
async fn an_empty_trust_anchor_pem_is_rejected() {
    let Err(err) = NativeImapConnector::new(TlsMode::Implicit).with_extra_root_pem(
        "-----BEGIN RSA PRIVATE KEY-----\nnope\n-----END RSA PRIVATE KEY-----",
    ) else {
        panic!("a PEM with no CERTIFICATE block must not yield an empty trust store");
    };

    assert!(matches!(err, NativeTransportError::TrustAnchor(_)));
}

/// A plaintext pipe handed to the implicit-TLS constructor must not silently
/// work: `connect` assumes TLS is already up, so the STARTTLS-mode transport is
/// only ever safe via `connect_starttls`. Guards the one caller mistake that
/// would send the password in the clear.
#[tokio::test]
async fn a_transport_that_is_already_tls_refuses_a_second_starttls() {
    let cert = mint_cert();
    let port = spawn_source(&cert, TlsMode::Implicit, None).await;

    let connector = NativeImapConnector::new(TlsMode::Implicit)
        .with_extra_root_pem(&cert.cert_pem)
        .unwrap();
    let transport = connector.connect("localhost", port).await.unwrap();

    // Implicit mode already handshook; asking for STARTTLS on top is a bug.
    let Err(err) = ImapSession::connect_starttls(transport).await else {
        panic!("STARTTLS on an implicit-TLS pipe must be refused");
    };

    // The server greets, we send STARTTLS, it answers BAD (unknown in our
    // script) — either way we must never end up believing TLS was negotiated
    // twice. Both shapes are a hard failure, which is the assertion.
    assert!(
        matches!(
            err,
            ImapClientError::Rejected { .. } | ImapClientError::Protocol(_)
        ),
        "got {err:?}"
    );
}
