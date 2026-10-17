//! **Authoritative-direct** DNS visibility — "does the zone's own nameserver set
//! really serve this TXT record?", asked of every authoritative NS directly
//! rather than through a recursive resolver.
//!
//! This is the honest readiness signal behind the DNS-01 propagation gate
//! (`docs/goal/architecture/nest/tls-certificates.md` § B tier 2). It lives here,
//! in the shared core, because **two callers need the identical query**:
//!
//! - `fauna_client_dns::resolvability::AuthoritativeNsProbe` — the in-process
//!   probe the 5 native apps and the tui run while driving their own order, and
//! - the nest's `fauna.dns.probe_txt_visible` handler — the same query run one
//!   RPC hop away, so the **web** app (which has no raw DNS in the browser) gets
//!   the same signal instead of a weaker approximation.
//!
//! One mechanism, two transports. Keeping the query itself here is what makes
//! that literally true rather than a claim about two similar implementations.
//!
//! **Why authoritative-direct, and never a recursive resolver or DoH:**
//! - a recursive resolver **negative-caches** a miss for up to the zone's SOA
//!   minimum (an hour for typical zones), so polling one can report "absent" long
//!   after the record went live — and the poll itself *plants* that negative
//!   entry, blinding every later poll in the same gate;
//! - the DNS provider's own control-plane API is worthless here: it reports what
//!   it *stored*, not what it *serves* (the 2026-07-23/24 Hetzner incident — the
//!   API confirmed a record it was serving at a doubled owner name, and its zone
//!   publishes have been measured >= 10-15 min behind the API besides).
//!
//! The CA (Let's Encrypt) resolves fresh from the authoritative servers, so
//! "every authoritative NS serves the value" is the strongest local predictor of
//! a validation succeeding.
//!
//! Mechanics: NS discovery runs over the system resolver (hickory
//! `TokioResolver`, the same construction [`crate::resolve`]'s lookups use); the
//! per-NS TXT check is a raw non-recursive hickory-proto query — UDP first with
//! one TCP retry (RFC 7766) — so no resolver cache can sit between the poll and
//! the authoritative answer.

use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::time::Duration;

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::op::{Message, MessageType, OpCode, Query};
use hickory_resolver::proto::rr::{Name, RData, RecordType};
use tokio::net::UdpSocket;

/// How long one authoritative NS gets to answer one TXT query, on each
/// transport, before the check counts it as "not visible yet". Generous for a direct authoritative
/// round-trip; the caller's poll loop retries anyway.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// `true` iff **every** authoritative NS of `zone_name` serves a TXT record at
/// `record_name` whose (concatenated character-string) value is exactly
/// `expected`.
///
/// Any failure — NS discovery, timeout, wrong value, an empty NS set — is
/// `false`, i.e. "not visible yet"; the caller's deadline bounds the retries.
/// Requiring *every* NS is deliberate: the CA may query any of them, and a
/// partially-published zone is exactly the state the propagation gate exists to
/// wait out.
///
/// `ns_override` skips NS discovery and treats those addresses as the zone's
/// authoritative set — for tests pointing the check at an in-process responder.
pub async fn authoritative_txt_visible(
    zone_name: &str,
    record_name: &str,
    expected: &str,
    ns_override: Option<&[SocketAddr]>,
) -> bool {
    let addrs = match ns_override {
        Some(ns) => ns.to_vec(),
        None => authoritative_addrs(zone_name).await,
    };
    if addrs.is_empty() {
        return false;
    }
    for addr in addrs {
        if !txt_served_by(addr, record_name, expected).await {
            return false;
        }
    }
    true
}

/// The zone's authoritative NS as socket addresses: NS names via the system
/// resolver, then each name's first resolved IP. Empty on any failure — the
/// caller then reports "not visible" and its deadline bounds it.
async fn authoritative_addrs(zone_name: &str) -> Vec<SocketAddr> {
    let resolver = match TokioResolver::builder_tokio().and_then(|b| b.build()) {
        Ok(resolver) => resolver,
        Err(_) => return Vec::new(),
    };
    let Ok(ns) = resolver.ns_lookup(zone_name).await else {
        return Vec::new();
    };
    // hickory 0.26: lookups return a generic `Lookup`; records come via
    // `answers()` and rdata by matching the `RData` enum (same pattern as
    // `crate::resolve::lookup_fauna_txt`).
    let ns_hosts: Vec<String> = ns
        .answers()
        .iter()
        .filter_map(|record| match &record.data {
            RData::NS(ns) => Some(ns.0.to_utf8()),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for host in ns_hosts {
        if let Ok(ips) = resolver.lookup_ip(host).await
            && let Some(ip) = ips.iter().next()
        {
            out.push(SocketAddr::new(ip, 53));
        }
    }
    out
}

/// Does the nameserver at `addr` serve a TXT record at `record_name` whose
/// (concatenated character-string) value is exactly `expected`? One raw
/// non-recursive query, RFC 7766's shape (`tls-certificates.md` § The browser's
/// arm of the propagation gate → **Transport**): UDP first on a connected
/// socket; a truncated answer, a UDP transport error or the UDP timeout retries
/// the same query once over TCP. A UDP answer that is not the value is
/// definitive and retries nothing; any TCP failure is `false`.
async fn txt_served_by(addr: SocketAddr, record_name: &str, expected: &str) -> bool {
    let Ok(name) = Name::from_utf8(record_name) else {
        return false;
    };
    // A deterministic per-name query id (no RNG dep; spoofing-resistance is not
    // load-bearing for a local readiness poll — the CA does its own resolution).
    let id = query_id(&(record_name, addr));

    // hickory-proto 0.26: id/type/op_code go through the constructor; the rest
    // are public `metadata` fields.
    let mut msg = Message::new(id, MessageType::Query, OpCode::Query);
    // Non-recursive: this is an authoritative server; asking it to recurse would
    // only invite a referral path we don't want.
    msg.metadata.recursion_desired = false;
    msg.add_query(Query::query(name, RecordType::TXT));
    let Ok(bytes) = msg.to_vec() else {
        return false;
    };

    let resp = match udp_exchange(addr, &bytes, id).await {
        UdpOutcome::Answer(resp) => resp,
        UdpOutcome::RetryOverTcp => match tcp_exchange(addr, &bytes).await {
            Some(resp) if resp.metadata.id == id => resp,
            _ => return false,
        },
    };
    serves_value(&resp, expected)
}

fn query_id(key: &impl Hash) -> u16 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish() as u16
}

/// What the UDP leg of one query came back with.
enum UdpOutcome {
    /// A complete answer carrying our id — definitive, whatever it says.
    Answer(Message),
    /// Truncated, unreadable, a transport error or the timeout: ask again over
    /// TCP.
    RetryOverTcp,
}

/// Send `bytes` to `addr` on a *connected* UDP socket and read the answer. The
/// connect is what makes a far end with nothing listening an error at once —
/// the kernel hands a connected socket the ICMP port-unreachable — rather than
/// a wait for the timeout.
async fn udp_exchange(addr: SocketAddr, bytes: &[u8], id: u16) -> UdpOutcome {
    let Ok(socket) = UdpSocket::bind(match addr {
        SocketAddr::V4(_) => "0.0.0.0:0",
        SocketAddr::V6(_) => "[::]:0",
    })
    .await
    else {
        return UdpOutcome::RetryOverTcp;
    };
    if socket.connect(addr).await.is_err() || socket.send(bytes).await.is_err() {
        return UdpOutcome::RetryOverTcp;
    }
    let mut buf = [0u8; 1500];
    // Wait for ERROR readiness as well as READABLE: the port-unreachable is a
    // pending socket error, not data, and a bare `recv` future waits only for
    // data — measured, it sat out the whole timeout on a closed loopback port.
    // `try_recv` consults READABLE readiness only, so the pending error is
    // taken (`SO_ERROR`) rather than received.
    let recv = async {
        loop {
            let ready = socket
                .ready(tokio::io::Interest::READABLE | tokio::io::Interest::ERROR)
                .await?;
            if ready.is_error() {
                return Err(socket
                    .take_error()?
                    .unwrap_or_else(|| std::io::ErrorKind::ConnectionRefused.into()));
            }
            match socket.try_recv(&mut buf) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                other => return other,
            }
        }
    };
    let Ok(Ok(n)) = tokio::time::timeout(QUERY_TIMEOUT, recv).await else {
        return UdpOutcome::RetryOverTcp;
    };
    match Message::from_vec(&buf[..n]) {
        Ok(resp) if resp.metadata.id == id && !resp.metadata.truncation => UdpOutcome::Answer(resp),
        _ => UdpOutcome::RetryOverTcp,
    }
}

/// One DNS exchange over TCP (two-byte length framing, RFC 1035 § 4.2.2 and
/// RFC 7766), bounded by [`QUERY_TIMEOUT`]; `None` on any failure.
async fn tcp_exchange(addr: SocketAddr, bytes: &[u8]) -> Option<Message> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let len = u16::try_from(bytes.len()).ok()?;
    let exchange = async {
        let mut conn = tokio::net::TcpStream::connect(addr).await.ok()?;
        let mut framed = Vec::with_capacity(bytes.len() + 2);
        framed.extend_from_slice(&len.to_be_bytes());
        framed.extend_from_slice(bytes);
        conn.write_all(&framed).await.ok()?;
        let n = conn.read_u16().await.ok()?;
        let mut buf = vec![0u8; usize::from(n)];
        conn.read_exact(&mut buf).await.ok()?;
        Message::from_vec(&buf).ok()
    };
    tokio::time::timeout(QUERY_TIMEOUT, exchange)
        .await
        .ok()
        .flatten()
}

/// Does `resp` answer a TXT whose concatenated character-strings are exactly
/// `expected`?
fn serves_value(resp: &Message, expected: &str) -> bool {
    resp.answers.iter().any(|rec| {
        if let RData::TXT(txt) = &rec.data {
            let joined: String = txt
                .txt_data
                .iter()
                .map(|seg| String::from_utf8_lossy(seg))
                .collect();
            joined == expected
        } else {
            false
        }
    })
}

/// Write TXT records into the name server at `server` as one RFC 2136 dynamic
/// UPDATE over TCP: zone `zone`, a delete-this-RR (class NONE) per `deleted`
/// pair, then an add (class IN) per `added` pair, unsigned. The e2e ACME
/// directory seed's managed-path mirror is the one caller
/// (`fauna_client_dns::e2e_acme_seam::mirror_fake_txt`; the write is ruled in
/// `e2e-automation-surface-gating.md` § The ACME directory seed → *The name
/// server's reach*) — its far end is the harness's fake on loopback, so nothing
/// here signs. Compiled out of a release build with the seed; a release-profile
/// e2e build forwards `test-helpers`.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub async fn send_txt_update(
    server: SocketAddr,
    zone: &str,
    added: &[(String, String)],
    deleted: &[(String, String)],
) -> Result<(), String> {
    use hickory_resolver::proto::op::ResponseCode;
    use hickory_resolver::proto::rr::rdata::TXT;
    use hickory_resolver::proto::rr::{DNSClass, Record};

    let name_of = |n: &str| Name::from_utf8(n).map_err(|e| format!("{n:?} is not a name: {e}"));
    let txt = |value: &str| RData::TXT(TXT::new(vec![value.to_string()]));
    let id = query_id(&(zone, added, deleted));
    let mut msg = Message::new(id, MessageType::Query, OpCode::Update);
    // RFC 2136 § 2.3: the zone section is one question naming the zone, type SOA.
    msg.add_query(Query::query(name_of(zone)?, RecordType::SOA));
    for (name, value) in deleted {
        // § 2.5.4: delete an RR from an RRset — class NONE, TTL 0, the rdata.
        let mut rec = Record::from_rdata(name_of(name)?, 0, txt(value));
        rec.dns_class = DNSClass::NONE;
        msg.authorities.push(rec);
    }
    for (name, value) in added {
        msg.authorities
            .push(Record::from_rdata(name_of(name)?, 60, txt(value)));
    }
    let bytes = msg
        .to_vec()
        .map_err(|e| format!("encoding the UPDATE: {e}"))?;
    let resp = tcp_exchange(server, &bytes)
        .await
        .ok_or_else(|| format!("no answer from {server} over TCP"))?;
    if resp.metadata.id != id || resp.metadata.response_code != ResponseCode::NoError {
        return Err(format!(
            "{server} answered the UPDATE with {:?}",
            resp.metadata.response_code
        ));
    }
    Ok(())
}

/// The reply [`spawn_responder`] gives: TXT for `name` → `value`, NXDOMAIN for
/// anything else.
#[cfg(any(test, feature = "test-helpers"))]
fn responder_reply(query: &Message, name: &Name, value: &str) -> Message {
    use hickory_resolver::proto::op::ResponseCode;
    use hickory_resolver::proto::rr::Record;
    use hickory_resolver::proto::rr::rdata::TXT;

    let mut resp = Message::new(query.metadata.id, MessageType::Response, OpCode::Query);
    resp.metadata.authoritative = true;
    for q in &query.queries {
        resp.add_query(q.clone());
        if q.query_type() == RecordType::TXT && q.name() == name {
            resp.add_answer(Record::from_rdata(
                name.clone(),
                60,
                RData::TXT(TXT::new(vec![value.to_string()])),
            ));
        } else {
            resp.metadata.response_code = ResponseCode::NXDomain;
        }
    }
    resp
}

/// Minimal in-process authoritative responder: answers TXT queries for
/// `served_name` with `served_value`, NXDOMAIN otherwise. Shared test
/// scaffolding — this crate's own unit tests and
/// `fauna_client_dns::resolvability`'s (the production caller, one crate
/// away) each faked the identical responder to exercise the identical query;
/// one implementation now, not two.
///
/// Deliberately NOT shared with the pebble acceptance harnesses
/// (`fauna-client-dns/tests/pebble_dns01.rs`, `fauna-acme-http01/tests/pebble_http01.rs`):
/// those answer **empty-NOERROR** for a non-matching query so pebble's CAA
/// walk finds no restriction, where this responder answers **NXDOMAIN** — a
/// real behavioral difference, not just a shape difference.
#[cfg(any(test, feature = "test-helpers"))]
pub async fn spawn_responder(served_name: &str, served_value: &str) -> SocketAddr {
    use std::str::FromStr;

    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let addr = socket.local_addr().expect("local addr");
    let name = Name::from_str(served_name).expect("name");
    let value = served_value.to_string();
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        loop {
            let Ok((len, peer)) = socket.recv_from(&mut buf).await else {
                return;
            };
            let Ok(query) = Message::from_vec(&buf[..len]) else {
                continue;
            };
            if let Ok(bytes) = responder_reply(&query, &name, &value).to_vec() {
                let _ = socket.send_to(&bytes, peer).await;
            }
        }
    });
    addr
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_resolver::proto::rr::DNSClass;

    #[tokio::test]
    async fn visible_when_every_ns_serves_the_exact_value() {
        let a = spawn_responder("_acme-challenge.example.test.", "tok-123").await;
        let b = spawn_responder("_acme-challenge.example.test.", "tok-123").await;
        assert!(
            authoritative_txt_visible(
                "example.test",
                "_acme-challenge.example.test",
                "tok-123",
                Some(&[a, b]),
            )
            .await
        );
    }

    #[tokio::test]
    async fn not_visible_on_wrong_value_or_absent_record() {
        let ns = spawn_responder("_acme-challenge.example.test.", "tok-123").await;
        assert!(
            !authoritative_txt_visible(
                "example.test",
                "_acme-challenge.example.test",
                "other",
                Some(&[ns]),
            )
            .await,
            "wrong value must read as not-visible"
        );
        assert!(
            !authoritative_txt_visible(
                "example.test",
                "_acme-challenge.other.test",
                "tok-123",
                Some(&[ns]),
            )
            .await,
            "absent record must read as not-visible"
        );
    }

    #[tokio::test]
    async fn not_visible_when_one_ns_lags() {
        let live = spawn_responder("_acme-challenge.example.test.", "tok-123").await;
        let lagging = spawn_responder("_acme-challenge.unrelated.test.", "x").await;
        assert!(
            !authoritative_txt_visible(
                "example.test",
                "_acme-challenge.example.test",
                "tok-123",
                Some(&[live, lagging]),
            )
            .await,
            "a partially-published zone (one NS lagging) must read as not-visible"
        );
    }

    /// Serve `reply` over TCP (two-byte length framing, one query per
    /// accepted connection) on `listener`.
    fn serve_tcp(listener: tokio::net::TcpListener, name: Name, value: String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        tokio::spawn(async move {
            loop {
                let Ok((mut conn, _)) = listener.accept().await else {
                    return;
                };
                let Ok(len) = conn.read_u16().await else {
                    continue;
                };
                let mut buf = vec![0u8; usize::from(len)];
                if conn.read_exact(&mut buf).await.is_err() {
                    continue;
                }
                let Ok(query) = Message::from_vec(&buf) else {
                    continue;
                };
                if let Ok(bytes) = responder_reply(&query, &name, &value).to_vec() {
                    let _ = conn.write_u16(bytes.len() as u16).await;
                    let _ = conn.write_all(&bytes).await;
                }
            }
        });
    }

    /// A responder that answers over TCP only: nothing listens on its UDP port.
    async fn spawn_tcp_responder(served_name: &str, served_value: &str) -> SocketAddr {
        use std::str::FromStr;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        serve_tcp(
            listener,
            Name::from_str(served_name).expect("name"),
            served_value.to_string(),
        );
        addr
    }

    /// A responder whose UDP answer is truncated (`TC`, no answer) and whose
    /// TCP answer on the same port is the full one.
    async fn spawn_truncating_responder(served_name: &str, served_value: &str) -> SocketAddr {
        use std::str::FromStr;
        let (listener, socket) = loop {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let port = listener.local_addr().expect("local addr").port();
            if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)).await {
                break (listener, socket);
            }
        };
        let addr = listener.local_addr().expect("local addr");
        serve_tcp(
            listener,
            Name::from_str(served_name).expect("name"),
            served_value.to_string(),
        );
        tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((len, peer)) = socket.recv_from(&mut buf).await else {
                    return;
                };
                let Ok(query) = Message::from_vec(&buf[..len]) else {
                    continue;
                };
                let mut resp =
                    Message::new(query.metadata.id, MessageType::Response, OpCode::Query);
                resp.metadata.truncation = true;
                for q in &query.queries {
                    resp.add_query(q.clone());
                }
                if let Ok(bytes) = resp.to_vec() {
                    let _ = socket.send_to(&bytes, peer).await;
                }
            }
        });
        addr
    }

    #[tokio::test]
    async fn visible_through_a_tcp_only_nameserver() {
        let ns = spawn_tcp_responder("_acme-challenge.example.test.", "tok-123").await;
        assert!(
            authoritative_txt_visible(
                "example.test",
                "_acme-challenge.example.test",
                "tok-123",
                Some(&[ns]),
            )
            .await,
            "a UDP leg with nothing listening must retry the query over TCP"
        );
    }

    #[tokio::test]
    async fn a_truncated_udp_answer_is_retried_over_tcp() {
        let ns = spawn_truncating_responder("_acme-challenge.example.test.", "tok-123").await;
        assert!(
            authoritative_txt_visible(
                "example.test",
                "_acme-challenge.example.test",
                "tok-123",
                Some(&[ns]),
            )
            .await,
            "a TC answer must retry the same query over TCP"
        );
    }

    #[tokio::test]
    async fn a_closed_udp_port_fails_at_once_on_the_connected_socket() {
        // Bind and drop: nothing listens on that UDP port any more. The
        // connected socket reports the port-unreachable as an error, so the
        // UDP leg is over in milliseconds instead of the 5 s timeout — the
        // android venue's fast path to the TCP retry rests on this.
        let addr = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind")
            .local_addr()
            .expect("local addr");
        let started = std::time::Instant::now();
        assert!(matches!(
            udp_exchange(addr, &[0u8; 12], 0).await,
            UdpOutcome::RetryOverTcp
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the UDP leg waited {:?} on a closed port",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_wrong_udp_value_is_definitive_and_never_retried() {
        // UDP serves the wrong value; a TCP listener on the same port would
        // serve the right one. A UDP answer is definitive, so the TCP leg is
        // never asked.
        let (listener, socket) = loop {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let port = listener.local_addr().expect("local addr").port();
            if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)).await {
                break (listener, socket);
            }
        };
        let addr = listener.local_addr().expect("local addr");
        let name: Name = "_acme-challenge.example.test.".parse().expect("name");
        serve_tcp(listener, name.clone(), "tok-123".into());
        tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            while let Ok((len, peer)) = socket.recv_from(&mut buf).await {
                if let Ok(query) = Message::from_vec(&buf[..len])
                    && let Ok(bytes) = responder_reply(&query, &name, "stale").to_vec()
                {
                    let _ = socket.send_to(&bytes, peer).await;
                }
            }
        });
        assert!(
            !authoritative_txt_visible(
                "example.test",
                "_acme-challenge.example.test",
                "tok-123",
                Some(&[addr]),
            )
            .await
        );
    }

    #[tokio::test]
    async fn a_txt_update_is_sent_over_tcp_as_rfc_2136() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let captured = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.expect("accept");
            let len = conn.read_u16().await.expect("len");
            let mut buf = vec![0u8; usize::from(len)];
            conn.read_exact(&mut buf).await.expect("body");
            let update = Message::from_vec(&buf).expect("an UPDATE message");
            let reply = Message::new(update.metadata.id, MessageType::Response, OpCode::Update);
            let bytes = reply.to_vec().expect("reply");
            conn.write_u16(bytes.len() as u16).await.expect("len");
            conn.write_all(&bytes).await.expect("reply");
            update
        });
        send_txt_update(
            addr,
            "renew.test",
            &[("_acme-challenge.renew.test".into(), "new".into())],
            &[("_acme-challenge.renew.test".into(), "old".into())],
        )
        .await
        .expect("acknowledged");
        let update = captured.await.expect("captured");
        assert_eq!(update.metadata.op_code, OpCode::Update);
        assert_eq!(update.queries.len(), 1, "one zone");
        assert_eq!(update.queries[0].name().to_utf8(), "renew.test.");
        assert_eq!(update.queries[0].query_type(), RecordType::SOA);
        let txt = |rec: &hickory_resolver::proto::rr::Record| match &rec.data {
            RData::TXT(t) => String::from_utf8_lossy(&t.txt_data[0]).into_owned(),
            other => panic!("not TXT: {other:?}"),
        };
        let updates = &update.authorities;
        assert_eq!(updates.len(), 2, "{updates:?}");
        let delete = &updates[0];
        assert_eq!(delete.dns_class, DNSClass::NONE, "a delete-this-RR");
        assert_eq!(delete.ttl, 0);
        assert_eq!(txt(delete), "old");
        let add = &updates[1];
        assert_eq!(add.dns_class, DNSClass::IN, "an add");
        assert_eq!(add.name.to_utf8(), "_acme-challenge.renew.test.");
        assert_eq!(txt(add), "new");
    }

    #[tokio::test]
    async fn empty_ns_set_reads_as_not_visible() {
        assert!(!authoritative_txt_visible("example.test", "x.example.test", "v", Some(&[])).await);
    }
}
