//! MX resolution — priority sort, same-priority round-robin, implicit
//! MX fallback. docs/goal/behavior/smtp-server.md § MX resolution +
//! IPv4/IPv6 mixed handling.

#![cfg(feature = "outbound")]

use std::net::{Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use fauna_mail::outbound::mx::{MxResolver, resolve_mx};

#[derive(Clone, Default)]
struct MockResolver {
    /// Each entry is one MX record: (priority, hostname).
    mx: Vec<(u16, String)>,
    /// Empty list emulates an NXDOMAIN-equivalent for A/AAAA.
    a: std::collections::HashMap<String, Vec<Ipv4Addr>>,
    aaaa: std::collections::HashMap<String, Vec<Ipv6Addr>>,
    /// The MX lookup itself fails — SERVFAIL, timeout, no route. Distinct
    /// from `mx: vec![]`, which is the resolver authoritatively answering
    /// "this domain publishes no MX records" (the implicit-MX case).
    mx_lookup_fails: bool,
}

#[async_trait]
impl MxResolver for MockResolver {
    async fn lookup_mx(&self, _domain: &str) -> anyhow::Result<Vec<(u16, String)>> {
        if self.mx_lookup_fails {
            anyhow::bail!("SERVFAIL");
        }
        Ok(self.mx.clone())
    }
    async fn lookup_a(&self, host: &str) -> anyhow::Result<Vec<Ipv4Addr>> {
        Ok(self.a.get(host).cloned().unwrap_or_default())
    }
    async fn lookup_aaaa(&self, host: &str) -> anyhow::Result<Vec<Ipv6Addr>> {
        Ok(self.aaaa.get(host).cloned().unwrap_or_default())
    }
}

#[tokio::test]
async fn priority_sort_lowest_first() {
    let mut r = MockResolver {
        mx: vec![
            (20, "mx2.example.com".into()),
            (10, "mx1.example.com".into()),
            (30, "mx3.example.com".into()),
        ],
        ..Default::default()
    };
    r.a.insert("mx1.example.com".into(), vec![Ipv4Addr::new(1, 1, 1, 1)]);
    r.a.insert("mx2.example.com".into(), vec![Ipv4Addr::new(2, 2, 2, 2)]);
    r.a.insert("mx3.example.com".into(), vec![Ipv4Addr::new(3, 3, 3, 3)]);

    let hosts = resolve_mx("example.com", true, &r).await.unwrap();
    let prio: Vec<u16> = hosts.iter().map(|h| h.priority).collect();
    assert_eq!(prio, vec![10, 20, 30]);
}

#[tokio::test]
async fn same_priority_hosts_both_returned_for_round_robin() {
    let mut r = MockResolver {
        mx: vec![
            (10, "a.example.com".into()),
            (10, "b.example.com".into()),
            (10, "c.example.com".into()),
            (20, "fallback.example.com".into()),
        ],
        ..Default::default()
    };
    for host in [
        "a.example.com",
        "b.example.com",
        "c.example.com",
        "fallback.example.com",
    ] {
        r.a.insert(host.into(), vec![Ipv4Addr::new(127, 0, 0, 1)]);
    }
    let hosts = resolve_mx("example.com", true, &r).await.unwrap();
    let names: Vec<String> = hosts.iter().map(|h| h.hostname.clone()).collect();
    // Priority 10 cluster (3 hosts) precedes the priority 20 fallback.
    assert_eq!(names.len(), 4);
    let first_three: std::collections::HashSet<&str> =
        names[..3].iter().map(String::as_str).collect();
    let expect: std::collections::HashSet<&str> =
        ["a.example.com", "b.example.com", "c.example.com"]
            .into_iter()
            .collect();
    assert_eq!(first_three, expect);
    assert_eq!(names[3], "fallback.example.com");
}

#[tokio::test]
async fn ipv6_disabled_skips_aaaa() {
    let mut r = MockResolver {
        mx: vec![(10, "mx.example.com".into())],
        ..Default::default()
    };
    r.a.insert("mx.example.com".into(), vec![Ipv4Addr::new(1, 2, 3, 4)]);
    r.aaaa
        .insert("mx.example.com".into(), vec!["::1".parse().unwrap()]);

    let hosts = resolve_mx("example.com", false, &r).await.unwrap();
    assert_eq!(hosts.len(), 1);
    assert!(hosts[0].addrs.iter().all(|a| a.is_ipv4()));
}

#[tokio::test]
async fn ipv6_enabled_returns_both_families() {
    let mut r = MockResolver {
        mx: vec![(10, "mx.example.com".into())],
        ..Default::default()
    };
    r.a.insert("mx.example.com".into(), vec![Ipv4Addr::new(1, 2, 3, 4)]);
    r.aaaa
        .insert("mx.example.com".into(), vec!["::1".parse().unwrap()]);

    let hosts = resolve_mx("example.com", true, &r).await.unwrap();
    assert_eq!(hosts.len(), 1);
    let has_v4 = hosts[0].addrs.iter().any(|a| a.is_ipv4());
    let has_v6 = hosts[0].addrs.iter().any(|a| a.is_ipv6());
    assert!(has_v4 && has_v6);
}

#[tokio::test]
async fn implicit_mx_falls_back_to_a_aaaa_with_priority_0() {
    // RFC 5321 §5: no MX records → use A/AAAA of the domain itself.
    let mut r = MockResolver::default();
    r.a.insert("example.com".into(), vec![Ipv4Addr::new(9, 9, 9, 9)]);

    let hosts = resolve_mx("example.com", true, &r).await.unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].priority, 0);
    assert_eq!(hosts[0].hostname, "example.com");
    assert_eq!(hosts[0].addrs.len(), 1);
}

#[tokio::test]
async fn a_failed_mx_lookup_is_an_error_not_an_implicit_mx() {
    // RFC 5321 §5.1 + `smtp-server.md:494` ("Implicit MX (**no MX RR**,
    // A/AAAA only)"): the implicit-MX rule fires on an authoritative "this
    // domain publishes no MX records", NOT on a lookup that failed. A
    // SERVFAIL or timeout says nothing about what the domain publishes.
    //
    // The distinction has teeth. Collapsing a failed lookup into "no MX RR"
    // delivers mail to the recipient domain's own A record — which for any
    // domain whose real MX is a third party (a hosted provider, or the
    // domain's own `mx.` host) is a machine that was never meant to receive
    // its mail, and is often a web server. A transient DNS failure must
    // tempfail into the retry curve instead, exactly as the Go MTA's
    // `LiveMXResolver` already does: it applies implicit MX only on
    // `dnsErr.IsNotFound` and returns every other error to the caller
    // (`bins/fauna-bridges/internal/mta/outbound.go`).
    let r = MockResolver {
        mx_lookup_fails: true,
        ..Default::default()
    };

    let err = resolve_mx("example.com", true, &r)
        .await
        .expect_err("a failed MX lookup must surface as an error");
    assert!(
        err.to_string().contains("SERVFAIL"),
        "the resolver's own failure should reach the caller, got: {err}"
    );
}

#[tokio::test]
async fn a_failed_mx_lookup_does_not_deliver_to_the_domains_own_a_record() {
    // The same defect stated as the delivery outcome it causes, because that
    // is the part a reader has to care about: with an A record present for
    // the domain itself, a fail-open produces a perfectly plausible
    // one-host result, and nothing downstream can tell it was wrong.
    let mut r = MockResolver {
        mx_lookup_fails: true,
        ..Default::default()
    };
    r.a.insert("example.com".into(), vec![Ipv4Addr::new(9, 9, 9, 9)]);

    assert!(
        resolve_mx("example.com", true, &r).await.is_err(),
        "a failed lookup must not silently become delivery to 9.9.9.9"
    );
}
