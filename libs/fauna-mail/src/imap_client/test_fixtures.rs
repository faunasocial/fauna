//! Real-TLS server test fixtures for the native IMAP transport: mints a
//! self-signed cert (`rcgen`) and builds the `rustls` `TlsAcceptor` that
//! terminates it, so an integration test can stand up a fake IMAP-over-TLS
//! source server. Gated `tls-test-fixtures` — a REGULAR (non-dev) optional
//! dep on `rcgen`, deliberately not the narrower `test-helpers` feature
//! (reserved for the e2e trust-seed gate, `test_mail_import_tls_seam_gating.py`
//! protected) — so an EXTERNAL crate's own integration test can share this
//! instead of hand-rolling its own copy: dev-dependencies are invisible
//! outside this crate's own test/bench/example targets.
//!
//! Consumed by `tests/imap_client_native.rs`, `tests/imap_client_trust_seed.rs`,
//! and `bins/fauna-nest/tests/conformance_mail_import_client.rs` — all three
//! minted byte-identical copies before this lift.
//!
//! [`serve_imap`] joined the module the same way (round 92): the nest
//! conformance test's own doc comment already said its copy was "lifted from
//! `imap_client_native.rs` ... generalized from one hard-coded message to a
//! corpus" — the corpus-taking shape was the richer of the two, so it is what
//! moved here (priority #4, pick the richest existing pattern).

use std::sync::Arc;

/// A self-signed cert for `localhost`, plus its PEM (the client's trust anchor).
pub struct TestCert {
    pub cert_pem: String,
    pub key_pem: String,
}

pub fn mint_cert() -> TestCert {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    TestCert {
        cert_pem: cert.cert.pem(),
        key_pem: cert.signing_key.serialize_pem(),
    }
}

pub fn tls_acceptor(cert: &TestCert) -> tokio_rustls::TlsAcceptor {
    let _ = rustls::crypto::ring::default_provider().install_default();

    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

    let certs = CertificateDer::pem_slice_iter(cert.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key =
        PrivateKeyDer::from_pem_slice(cert.key_pem.as_bytes()).expect("a private key in the PEM");

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

/// One message a [`serve_imap`]-driven fake source can FETCH.
pub struct FixtureMessage {
    pub uid: u32,
    /// Source-side IMAP flags, verbatim (e.g. `"\\Seen \\Recent"`).
    pub flags: &'static str,
    pub body: &'static [u8],
}

/// Serve one IMAP session over an already-established byte stream, answering
/// exactly the commands `ImapSession` emits: `CAPABILITY`, `LOGIN`,
/// `EXAMINE`, the `UID FETCH n:* (UID RFC822.SIZE)` enumeration, the per-UID
/// `UID FETCH <uid> (UID FLAGS INTERNALDATE BODY.PEEK[])`, and `LOGOUT`.
///
/// Deliberately generic over the stream so the same script runs on a raw
/// `TcpStream` (STARTTLS's pre-upgrade phase) and on a `TlsStream` (after).
/// FETCH is pipelined client-side (up to 4 outstanding), but a line loop that
/// answers each command as it is read is correct regardless: RFC 3501 §7 only
/// requires a command's untagged data to precede its tagged completion.
pub async fn serve_imap<S>(stream: &mut S, uid_validity: u32, corpus: &[FixtureMessage])
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0u8; 8192];
    let mut pending = Vec::new();

    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        pending.extend_from_slice(&buf[..n]);

        while let Some(pos) = pending.windows(2).position(|w| w == b"\r\n") {
            let line: Vec<u8> = pending.drain(..pos + 2).collect();
            let line = String::from_utf8_lossy(&line).trim_end().to_string();
            let mut parts = line.splitn(3, ' ');
            let tag = parts.next().unwrap_or("").to_string();
            let verb = parts.next().unwrap_or("").to_ascii_uppercase();
            let rest = parts.next().unwrap_or("").to_string();

            let reply = match verb.as_str() {
                "CAPABILITY" => {
                    format!("* CAPABILITY IMAP4rev1 UIDPLUS\r\n{tag} OK CAPABILITY done\r\n")
                }
                "LOGIN" => format!("{tag} OK LOGIN done\r\n"),
                "EXAMINE" => format!(
                    "* {} EXISTS\r\n* OK [UIDVALIDITY {uid_validity}] uids valid\r\n\
                     {tag} OK [READ-ONLY] EXAMINE done\r\n",
                    corpus.len(),
                ),
                // The enumeration pass: every UID with its size, no bodies.
                "UID" if rest.to_ascii_uppercase().contains("RFC822.SIZE") => {
                    let mut out = String::new();
                    for (i, m) in corpus.iter().enumerate() {
                        out.push_str(&format!(
                            "* {} FETCH (UID {} RFC822.SIZE {})\r\n",
                            i + 1,
                            m.uid,
                            m.body.len(),
                        ));
                    }
                    out.push_str(&format!("{tag} OK UID FETCH done\r\n"));
                    out
                }
                // The body pass: `FETCH <uid> (...)` — serve that one message.
                "UID" => {
                    let uid: u32 = rest
                        .split_whitespace()
                        .nth(1)
                        .and_then(|t| t.parse().ok())
                        .unwrap_or(0);
                    match corpus.iter().position(|m| m.uid == uid) {
                        Some(i) => {
                            let m = &corpus[i];
                            format!(
                                "* {} FETCH (UID {} FLAGS ({}) INTERNALDATE \
                                 \"01-Jan-2020 00:00:00 +0000\" BODY[] {{{}}}\r\n{})\r\n\
                                 {tag} OK UID FETCH done\r\n",
                                i + 1,
                                m.uid,
                                m.flags,
                                m.body.len(),
                                String::from_utf8_lossy(m.body),
                            )
                        }
                        None => format!("{tag} NO no such uid\r\n"),
                    }
                }
                "LOGOUT" => format!("* BYE bye\r\n{tag} OK LOGOUT done\r\n"),
                _ => format!("{tag} BAD unknown command\r\n"),
            };
            if stream.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
            let _ = stream.flush().await;
        }
    }
}
