//! The e2e ACME directory seed — a **test-capable build only** switch that points
//! the client's DNS-01 orders — manual and managed alike — at the harness's pebble
//! CA instead of production Let's Encrypt (`e2e-automation-surface-gating.md`
//! § The ACME directory seed).
//!
//! The CA is a constant no human chooses (`tls-certificates.md` § ACME settings —
//! constants, not choices), so a walk that must open a real order cannot reach
//! one through any app or nest setting: there is none, and there must not be
//! one. This module is the one door. It reads three variables, all compiled out
//! of every release artifact (the whole module is gated in `lib.rs`), and turns
//! them into the [`AcmeOrderTestSeam`] the shared native
//! machine builder installs — so every native app that builds its admin-dns
//! machine through `build_dns_management_machine_with_credentials` honours it
//! with no per-app glue.
//!
//! - [`E2E_ACME_DIRECTORY_ENV`] — the pebble directory URL. `https://` only: a
//!   plaintext directory is refused even under the gate. Unset or empty → no
//!   seam; production's constant stands.
//! - [`E2E_ACME_CA_PEM_ENV`] — the PEM text of pebble's directory CA. **Required**
//!   once the directory is set; it is added to an otherwise EMPTY root store for
//!   the ACME client alone (pebble's directory is the only far end that client
//!   talks to under the seed), never to the app's general trust.
//! - [`E2E_ACME_NAMESERVER_ENV`] — optional `ip:port` of the harness's fake name
//!   server. Set → the propagation gate polls it as the domain's authoritative
//!   set (`AuthoritativeNsProbe::with_nameservers`); unset → the gate is skipped,
//!   exactly as the seam's `probe: None` documents. The same name server is
//!   where the e2e fake DNS provider writes every TXT it publishes or
//!   withdraws, as an RFC 2136 UPDATE over TCP ([`mirror_fake_txt`]), so a
//!   MANAGED order's challenge is really served; unset → the fake keeps its
//!   records in-process only.
//!
//! A malformed value panics: only the harness sets these, so it is a harness bug,
//! and a loud failure beats an issuance error that reads like an app bug (the
//! source-IMAP seed's rule).

use std::net::SocketAddr;

use crate::{AcmeOrderTestSeam, AuthoritativeNsProbe};

/// The pebble ACME directory URL both order paths open against.
pub const E2E_ACME_DIRECTORY_ENV: &str = "FAUNA_E2E_ACME_DIRECTORY_URL";
/// PEM text of the directory's CA (pebble's `pebble.minica.pem`).
pub const E2E_ACME_CA_PEM_ENV: &str = "FAUNA_E2E_ACME_CA_PEM";
/// `ip:port` of the fake authoritative name server the propagation gate polls.
pub const E2E_ACME_NAMESERVER_ENV: &str = "FAUNA_E2E_ACME_NAMESERVER";

/// The `User-Agent` the seeded ACME client sends (pebble rejects a request with
/// none — RFC 8555 § 6.1).
const USER_AGENT: &str = "fauna-e2e-acme-seed/0";

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The seam the launch environment describes, or `None` when the directory is
/// not seeded.
pub fn acme_order_test_seam_from_env() -> Option<AcmeOrderTestSeam> {
    let directory_url = non_empty(E2E_ACME_DIRECTORY_ENV)?;
    seam_from_values(
        directory_url,
        non_empty(E2E_ACME_CA_PEM_ENV),
        non_empty(E2E_ACME_NAMESERVER_ENV),
    )
    .map(Some)
    .unwrap_or_else(|e| {
        panic!(
            "the e2e ACME directory seed is set but unusable: {e}. Only the e2e \
             harness sets {E2E_ACME_DIRECTORY_ENV}, so this is a harness bug."
        )
    })
}

fn seam_from_values(
    directory_url: String,
    ca_pem: Option<String>,
    nameserver: Option<String>,
) -> Result<AcmeOrderTestSeam, String> {
    if !directory_url.starts_with("https://") {
        return Err(format!(
            "{E2E_ACME_DIRECTORY_ENV} must be an https:// URL, got {directory_url:?}"
        ));
    }
    let ca_pem = ca_pem.ok_or_else(|| {
        format!("{E2E_ACME_CA_PEM_ENV} is required once {E2E_ACME_DIRECTORY_ENV} is set")
    })?;
    if !ca_pem.contains("BEGIN CERTIFICATE") {
        return Err(format!("{E2E_ACME_CA_PEM_ENV} holds no CERTIFICATE block"));
    }
    let probe = nameserver
        .map(|ns| {
            ns.parse::<SocketAddr>()
                .map_err(|e| format!("{E2E_ACME_NAMESERVER_ENV}={ns:?} is not ip:port: {e}"))
        })
        .transpose()?
        .map(|addr| {
            Box::new(AuthoritativeNsProbe::with_nameservers(vec![addr]))
                as Box<dyn crate::Dns01ResolvabilityProbe>
        });
    let ca_bytes = ca_pem.into_bytes();
    Ok(AcmeOrderTestSeam {
        directory_url,
        http_client: Box::new(move || {
            fauna_pebble_testkit::pebble_http_client(&ca_bytes, USER_AGENT)
        }),
        probe,
    })
}

/// Mirror a fake-provider TXT write into the seed's name server
/// ([`E2E_ACME_NAMESERVER_ENV`]) as RFC 2136 dynamic UPDATEs over TCP, when the
/// launch environment names one; otherwise do nothing. Called by the e2e
/// `SentinelDnsProvider` after every publish (`published`) and withdrawal
/// (`withdrawn`), each a list of `(name, value)` TXT pairs.
///
/// One UPDATE per zone — the zone is the challenge name's parent — carrying a
/// delete-this-RR (class NONE) per withdrawn pair and an add (class IN) per
/// published pair (`fauna_core::authoritative_dns::send_txt_update`); the
/// harness's fake applies it to an in-memory overlay above its records file, so
/// the records the harness wrote itself (`AcmePebble.publish_txt`) are kept, and
/// the write rides the same TCP port the propagation probe's retry does — the
/// one an android venue reverses (`e2e-automation-surface-gating.md` § The ACME
/// directory seed → *The name server's reach*). A failure is logged, never
/// raised — the order then fails at its propagation gate with the gate's own
/// error, which names the record nobody served.
pub async fn mirror_fake_txt(published: &[(String, String)], withdrawn: &[(String, String)]) {
    let Some(ns) = non_empty(E2E_ACME_NAMESERVER_ENV) else {
        return;
    };
    let result = match ns.parse::<SocketAddr>() {
        Ok(server) => mirror_to(server, published, withdrawn).await,
        Err(e) => Err(format!(
            "{E2E_ACME_NAMESERVER_ENV}={ns:?} is not ip:port: {e}"
        )),
    };
    if let Err(e) = result {
        tracing::warn!(
            target: "fauna_dns",
            "the e2e fake DNS provider could not mirror into the name server {ns}: {e}"
        );
    }
}

async fn mirror_to(
    server: SocketAddr,
    published: &[(String, String)],
    withdrawn: &[(String, String)],
) -> Result<(), String> {
    type Pairs = Vec<(String, String)>;
    let mut zones: std::collections::BTreeMap<String, (Pairs, Pairs)> = Default::default();
    for (name, value) in published {
        zones
            .entry(parent_zone(name))
            .or_default()
            .0
            .push((name.clone(), value.clone()));
    }
    for (name, value) in withdrawn {
        zones
            .entry(parent_zone(name))
            .or_default()
            .1
            .push((name.clone(), value.clone()));
    }
    for (zone, (added, deleted)) in zones {
        fauna_core::authoritative_dns::send_txt_update(server, &zone, &added, &deleted).await?;
    }
    Ok(())
}

/// `_acme-challenge.renew.test.` → `renew.test`: the zone an UPDATE for a
/// challenge name names.
fn parent_zone(name: &str) -> String {
    let name = name.trim_end_matches('.');
    name.split_once('.')
        .map_or(name, |(_, parent)| parent)
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::Message;
    use hickory_proto::rr::{DNSClass, RData};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-shot name server on loopback TCP: acknowledges `n` UPDATEs with
    /// NOERROR and hands back what it received.
    async fn capturing_name_server(
        n: usize,
    ) -> (SocketAddr, tokio::task::JoinHandle<Vec<Message>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let mut got = Vec::new();
            for _ in 0..n {
                let (mut conn, _) = listener.accept().await.expect("accept");
                let len = conn.read_u16().await.expect("len");
                let mut buf = vec![0u8; usize::from(len)];
                conn.read_exact(&mut buf).await.expect("body");
                let update = Message::from_vec(&buf).expect("an UPDATE");
                let mut reply = update.clone();
                reply.metadata.message_type = hickory_proto::op::MessageType::Response;
                reply.authorities.clear();
                let bytes = reply.to_vec().expect("reply");
                conn.write_u16(bytes.len() as u16).await.expect("len");
                conn.write_all(&bytes).await.expect("reply");
                got.push(update);
            }
            got
        });
        (addr, handle)
    }

    fn txt(rec: &hickory_proto::rr::Record) -> String {
        match &rec.data {
            RData::TXT(t) => t
                .txt_data
                .iter()
                .map(|s| String::from_utf8_lossy(s))
                .collect(),
            other => panic!("not TXT: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_publish_is_an_add_in_the_challenge_names_parent_zone() {
        let (ns, got) = capturing_name_server(1).await;
        mirror_to(
            ns,
            &[("_acme-challenge.Renew.test.".into(), "tok".into())],
            &[],
        )
        .await
        .expect("acknowledged");
        let update = &got.await.unwrap()[0];
        assert_eq!(update.metadata.op_code, hickory_proto::op::OpCode::Update);
        assert_eq!(update.queries[0].name().to_utf8(), "renew.test.");
        assert_eq!(update.authorities.len(), 1);
        assert_eq!(update.authorities[0].dns_class, DNSClass::IN);
        assert_eq!(txt(&update.authorities[0]), "tok");
    }

    #[tokio::test]
    async fn a_withdrawal_deletes_only_the_value_it_withdraws() {
        let (ns, got) = capturing_name_server(1).await;
        mirror_to(ns, &[], &[("_acme-challenge.a.test".into(), "old".into())])
            .await
            .expect("acknowledged");
        let update = &got.await.unwrap()[0];
        assert_eq!(update.authorities.len(), 1);
        let rec = &update.authorities[0];
        assert_eq!(
            rec.dns_class,
            DNSClass::NONE,
            "delete this RR, not the RRset"
        );
        assert_eq!(rec.ttl, 0);
        assert_eq!(rec.name.to_utf8(), "_acme-challenge.a.test.");
        assert_eq!(txt(rec), "old");
    }

    #[tokio::test]
    async fn one_update_per_zone() {
        let (ns, got) = capturing_name_server(2).await;
        mirror_to(
            ns,
            &[
                ("_acme-challenge.one.test".into(), "a".into()),
                ("_acme-challenge.one.test".into(), "b".into()),
                ("_acme-challenge.two.test".into(), "c".into()),
            ],
            &[],
        )
        .await
        .expect("acknowledged");
        let got = got.await.unwrap();
        let zones: Vec<_> = got.iter().map(|m| m.queries[0].name().to_utf8()).collect();
        assert_eq!(zones, ["one.test.", "two.test."]);
        assert_eq!(
            got[0].authorities.len(),
            2,
            "both values of one name in one UPDATE"
        );
    }

    #[tokio::test]
    async fn no_name_server_answering_is_an_error_the_mirror_logs() {
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(
            mirror_to(
                closed,
                &[("_acme-challenge.x.test".into(), "v".into())],
                &[]
            )
            .await
            .is_err()
        );
    }

    // A shape-only stand-in, never parsed (the client factory is not called);
    // spelled in pieces so no literal PEM block sits in the source.
    const PEM: &str = concat!("BEGIN", " CERTIFICATE", " placeholder");

    #[test]
    fn a_plaintext_directory_is_refused() {
        let err = seam_from_values("http://127.0.0.1:1/dir".into(), Some(PEM.into()), None)
            .err()
            .expect("plaintext refused");
        assert!(err.contains("https://"), "{err}");
    }

    #[test]
    fn the_ca_is_required_once_the_directory_is_set() {
        let err = seam_from_values("https://127.0.0.1:1/dir".into(), None, None)
            .err()
            .expect("CA required");
        assert!(err.contains(E2E_ACME_CA_PEM_ENV), "{err}");
    }

    #[test]
    fn a_bad_nameserver_is_refused_and_a_good_one_installs_the_probe() {
        assert!(
            seam_from_values(
                "https://127.0.0.1:1/dir".into(),
                Some(PEM.into()),
                Some("not-an-addr".into()),
            )
            .is_err()
        );
        let seam = seam_from_values(
            "https://127.0.0.1:1/dir".into(),
            Some(PEM.into()),
            Some("127.0.0.1:5353".into()),
        )
        .expect("valid seed");
        assert_eq!(seam.directory_url, "https://127.0.0.1:1/dir");
        assert!(seam.probe.is_some());
        let no_probe =
            seam_from_values("https://127.0.0.1:1/dir".into(), Some(PEM.into()), None).unwrap();
        assert!(
            no_probe.probe.is_none(),
            "no name server → the gate is skipped"
        );
    }
}
