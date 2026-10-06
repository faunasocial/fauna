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
//! per-NS TXT check is a raw non-recursive hickory-proto query over UDP — no
//! resolver cache can sit between the poll and the authoritative answer.

use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::time::Duration;

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::op::{Message, MessageType, OpCode, Query};
use hickory_resolver::proto::rr::{Name, RData, RecordType};
use tokio::net::UdpSocket;

/// How long one authoritative NS gets to answer one UDP TXT query before the
/// check counts it as "not visible yet". Generous for a direct authoritative
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
/// non-recursive UDP query; any error, timeout, or mismatch is `false`.
async fn txt_served_by(addr: SocketAddr, record_name: &str, expected: &str) -> bool {
    let Ok(name) = Name::from_utf8(record_name) else {
        return false;
    };
    // A deterministic per-name query id (no RNG dep; spoofing-resistance is not
    // load-bearing for a local readiness poll — the CA does its own resolution).
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (record_name, addr).hash(&mut hasher);
    let id = hasher.finish() as u16;

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

    let Ok(socket) = UdpSocket::bind(match addr {
        SocketAddr::V4(_) => "0.0.0.0:0",
        SocketAddr::V6(_) => "[::]:0",
    })
    .await
    else {
        return false;
    };
    if socket.send_to(&bytes, addr).await.is_err() {
        return false;
    }

    let mut buf = [0u8; 1500];
    let Ok(Ok((n, _))) = tokio::time::timeout(QUERY_TIMEOUT, socket.recv_from(&mut buf)).await
    else {
        return false;
    };
    let Ok(resp) = Message::from_vec(&buf[..n]) else {
        return false;
    };
    if resp.metadata.id != id {
        return false;
    }
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
    use hickory_resolver::proto::op::ResponseCode;
    use hickory_resolver::proto::rr::Record;
    use hickory_resolver::proto::rr::rdata::TXT;
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
            let mut resp = Message::new(query.metadata.id, MessageType::Response, OpCode::Query);
            resp.metadata.authoritative = true;
            for q in &query.queries {
                resp.add_query(q.clone());
                if q.query_type() == RecordType::TXT && q.name() == &name {
                    resp.add_answer(Record::from_rdata(
                        name.clone(),
                        60,
                        RData::TXT(TXT::new(vec![value.clone()])),
                    ));
                } else {
                    resp.metadata.response_code = ResponseCode::NXDomain;
                }
            }
            if let Ok(bytes) = resp.to_vec() {
                let _ = socket.send_to(&bytes, peer).await;
            }
        }
    });
    addr
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn empty_ns_set_reads_as_not_visible() {
        assert!(!authoritative_txt_visible("example.test", "x.example.test", "v", Some(&[])).await);
    }
}
