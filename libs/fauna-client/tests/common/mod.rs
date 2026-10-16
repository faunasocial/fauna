//! Shared test harness for fauna-client integration tests.
//!
//! Both halves now live in library code and are re-exported here so existing
//! `common::mpsc_pair()` / `common::MockClientChannel` call sites keep working:
//!
//! * the in-memory transport (`mpsc_pair` / `MpscAdapter` / `ServerSide`) is
//!   `fauna_ws_substrate::testing`, shared with the substrate's own supervisor
//!   tests and the federation channel tests (Spec Y2 slice 4);
//! * [`MockClientChannel`] — the client's [`SupervisedChannel`] with a *mocked*
//!   connect but the *real* push bridge and bearer refresh — is
//!   `fauna_client::testing`, behind the `test-util` feature. It moved out of
//!   this file so integration tests in *other* crates can drive a real
//!   `NestClient` through its connect lifecycle too; `fauna-client-mail-settings`'s
//!   `hydrate_waits_for_socket` is the first such consumer.

#![allow(dead_code)]
// `common` is compiled into every integration-test binary; the re-exported
// helpers below are used by some but not all of them.
#![allow(unused_imports)]

pub use fauna_client::testing::{
    ConnectQueue, MockClientChannel, MpscAdapter, ServerSide, connect_queue, mpsc_pair,
    mpsc_pair_with_capacity,
};

/// Whether the self-signed fixture server offers TLS session resumption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerSessions {
    /// Every connection is a FULL handshake (the default rig): the server keeps
    /// no session state and sends no tickets, so the client's cert verifier runs
    /// on each connection and a trust assertion observes the verifier alone.
    FullHandshakeOnly,
    /// rustls' default server-side session cache and tickets stay on, so a client
    /// that caches sessions can RESUME its next connection — and a resumed TLS 1.3
    /// session never runs the client's cert verifier. The rig for proving the
    /// pinned client configs never resume (security.md § Cross-connection binding
    /// → *No session resumption on a pinned config*).
    Resumable,
}

/// A self-signed TLS HTTP/1.1 fixture server on `127.0.0.1:0` — the one rig the
/// pinned-reqwest-leg tests share (`reqwest_pinned_tls`, `ranged_nest_reader`).
///
/// Every connection reads one request head (to the blank line) and writes the
/// bytes `respond` returns for it, then closes (`Connection: close` is the
/// caller's to send). A failed handshake — the client refused the cert, the
/// expected path of every no-pin / wrong-pin case — is dropped silently.
/// Returns `(port, spki_sha256_of_the_served_cert)`. Full handshakes only; the
/// resumption witness takes [`spawn_self_signed_server_with`].
pub async fn spawn_self_signed_server<F>(respond: F) -> (u16, [u8; 32])
where
    F: Fn(&str) -> Vec<u8> + Send + Sync + 'static,
{
    spawn_self_signed_server_with(ServerSessions::FullHandshakeOnly, respond).await
}

/// [`spawn_self_signed_server`] with the server's session-resumption posture
/// chosen by the caller.
pub async fn spawn_self_signed_server_with<F>(
    sessions: ServerSessions,
    respond: F,
) -> (u16, [u8; 32])
where
    F: Fn(&str) -> Vec<u8> + Send + Sync + 'static,
{
    use std::sync::Arc;

    use fauna_protocol::tls_spki::spki_sha256_of_cert_der;
    use rustls::ServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("self-signed cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let spki = spki_sha256_of_cert_der(cert_der.as_ref()).expect("served cert SPKI");
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

    let mut server_cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("server tls config");
    if sessions == ServerSessions::FullHandshakeOnly {
        // Every connection is a FULL handshake: a resumed session skips the
        // client's cert verifier, which would hide exactly what the trust tests
        // assert about the verifier.
        server_cfg.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        server_cfg.send_tls13_tickets = 0;
    }
    let acceptor = TlsAcceptor::from(Arc::new(server_cfg));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let respond = Arc::new(respond);

    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let respond = Arc::clone(&respond);
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tls.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let response = respond(&String::from_utf8_lossy(&head));
                let _ = tls.write_all(&response).await;
                let _ = tls.shutdown().await;
            });
        }
    });

    (port, spki)
}
