//! Nest-host DNS records — the deployment's own address rows, per
//! `docs/goal/behavior/dns-management.md` § Records covered (design
//! tracked internally, § Record matrix).
//!
//! Distinct from [`super::per_domain`] (the per-`mail_domains`-entry mail
//! records): these are **host config**, not per-domain mail records, so they
//! are assembled here and appended by the nest caller to the *primary* domain's
//! matrix. Two address roles, because **`mail.<primary>` (the MX target) may
//! resolve to a different IP than the apex `<primary>`** — the mail server can
//! be a separate box from the nest / WS-RPC-web host:
//!
//! * apex `<primary>` `A`/`AAAA` → the **nest** address (the WS-RPC / web
//!   endpoint clients reach).
//! * `mail.<primary>` `A`/`AAAA` → the **mail-host** address (the MX target peer
//!   MTAs connect to; matches [`super::per_domain::build_mx_record`]'s
//!   `mail.<primary_domain>` target).
//! * `PTR` (advisory, verify-only) → reverse of the **mail-host** IP, expected
//!   to point at `mail.<primary>` (forward-confirmed rDNS for inbound
//!   acceptance — `smtp-server.md` § FCrDNS). Reverse DNS is set at the IP
//!   owner, never zone-API-published, so this is never a managed record.
//!
//! Pure + I/O-free: the nest reads the persisted address from
//! `nest_host_address` and frames the records here, so the value `list_records`
//! reports and `verify_records` checks come from one source.

use std::net::IpAddr;

use super::per_domain::{DEFAULT_TTL_SECS, DnsRecord, DnsRecordType};

/// The deployment's persisted public address(es). Names are derived from the
/// primary mail domain; only the IPs are persisted nest-side (the public,
/// unsealed `nest_host_address` singleton). `*_ipv6` is `None` until the
/// deployment has an IPv6 address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDnsInput<'a> {
    /// The deployment's primary mail domain (the apex `<host>` + `mail.<primary>`
    /// base).
    pub primary_domain: &'a str,
    /// Nest (apex) IPv4 — the WS-RPC / web endpoint.
    pub nest_ipv4: &'a str,
    pub nest_ipv6: Option<&'a str>,
    /// Mail-host (MX target) IPv4 — may differ from `nest_ipv4`.
    pub mail_ipv4: &'a str,
    pub mail_ipv6: Option<&'a str>,
    /// Whether this deployment runs its self-hosted iroh P2P relay sidecar (one
    /// is connected to the nest, resolved by the caller). When set, a
    /// `relay.<primary>` `A`/`AAAA` row is emitted at the **nest** address(es):
    /// the relay's HTTPS listener (`bins/fauna-iroh-relay`) is SNI-routed through
    /// fauna-sni-router on the nest's :443, so `relay.<primary>` resolves to the
    /// apex IP, never the mail host. `docs/goal/architecture/transport.md`
    /// § Future directions; surfaced so an admin enabling the relay knows to
    /// publish the record the apex ACME order pre-checks before adding the SAN
    /// (`acme_http01::infra_host_resolves`).
    pub relay_enabled: bool,
    /// Whether this deployment runs the out-of-process ATProto PDS bridge (an
    /// approved `atproto.pds` bridge service user exists, resolved by the
    /// caller). When set, a `pds.<primary>` `A`/`AAAA` row is emitted at the
    /// **nest** address(es) — exactly like `relay_enabled` and for the same
    /// reason: the bridge's XRPC listener is SNI-routed through fauna-sni-router
    /// on the nest's :443 (`pds.*` → `127.0.0.1:8447`), so `pds.<primary>`
    /// resolves to the apex IP, never the mail host.
    /// `docs/goal/behavior/atproto-pds-full.md` § Wire & process topology
    /// (F1 packaging resolution); surfaced so an admin who stood the bridge up
    /// knows to publish the record the apex ACME order pre-checks before adding
    /// the SAN (`acme_http01::pds_san_included`).
    pub pds_enabled: bool,
}

fn addr_record(record_type: DnsRecordType, name: String, ip: &str) -> DnsRecord {
    // A/AAAA bodies are the bare IP literal (unquoted) — `verify::compare_addr`
    // parses both sides to `IpAddr`, so equivalent textual forms compare equal.
    DnsRecord {
        record_type,
        name,
        body: ip.to_string(),
        ttl_seconds: DEFAULT_TTL_SECS,
    }
}

fn ptr_record(ip: &str, expected_host: &str) -> Option<DnsRecord> {
    Some(DnsRecord {
        record_type: DnsRecordType::Ptr,
        name: reverse_ptr_name(ip)?,
        // PTR rdata is the FQDN the address should resolve back to; rendered
        // without a trailing dot to match the page's other names (the
        // `verify::compare_ptr` comparator normalizes trailing dots either way).
        body: expected_host.to_string(),
        ttl_seconds: DEFAULT_TTL_SECS,
    })
}

/// Build the nest-host record rows for the primary domain: apex + `mail.<primary>`
/// `A` (and `AAAA` when an IPv6 is set), an `A`/`AAAA` for each **enabled** infra
/// subdomain (`relay.<primary>`, `pds.<primary>`), plus an advisory `PTR` for the
/// mail-host IP(s). Emitted in a stable order (apex A/AAAA, mail A/AAAA, relay
/// A/AAAA, pds A/AAAA, PTR v4, PTR v6).
pub fn build_host_dns_records(input: &HostDnsInput<'_>) -> Vec<DnsRecord> {
    let apex = input.primary_domain;
    let mail_host = format!("mail.{apex}");
    let mut records = Vec::new();

    // Apex `<primary>` → nest (WS-RPC/web) address.
    records.push(addr_record(
        DnsRecordType::A,
        apex.to_string(),
        input.nest_ipv4,
    ));
    if let Some(v6) = input.nest_ipv6 {
        records.push(addr_record(DnsRecordType::Aaaa, apex.to_string(), v6));
    }

    // `mail.<primary>` → mail-host (MX target) address — may differ from apex.
    records.push(addr_record(
        DnsRecordType::A,
        mail_host.clone(),
        input.mail_ipv4,
    ));
    if let Some(v6) = input.mail_ipv6 {
        records.push(addr_record(DnsRecordType::Aaaa, mail_host.clone(), v6));
    }

    // Infra-subdomain hosts, each SNI-routed through fauna-sni-router on the
    // nest's :443, so each resolves to the **nest** address (the apex IP), NOT the
    // mail host:
    //   `relay.<primary>` → the self-hosted iroh P2P relay sidecar
    //                       (`bins/fauna-iroh-relay`)
    //   `pds.<primary>`   → the out-of-process ATProto PDS bridge's XRPC listener
    //                       (`cmd/fauna-atproto-bridge`, `pds.*` → 127.0.0.1:8447)
    // Each is emitted only when its service is on (the nest caller passes the
    // resolved flag). No PTR — neither is an MX / FCrDNS host. The matching apex
    // ACME SAN is gated on the same signal *and* on this record actually resolving
    // (`acme_http01::relay_san_included` / `pds_san_included`), so surfacing the
    // row here is what tells the admin to publish the A record the order waits for
    // — without it nothing would ever prompt them and the SAN would never appear.
    for label in [("relay", input.relay_enabled), ("pds", input.pds_enabled)]
        .into_iter()
        .filter_map(|(label, on)| on.then_some(label))
    {
        let host = format!("{label}.{apex}");
        records.push(addr_record(DnsRecordType::A, host.clone(), input.nest_ipv4));
        if let Some(v6) = input.nest_ipv6 {
            records.push(addr_record(DnsRecordType::Aaaa, host, v6));
        }
    }

    // Advisory PTR on the mail-host address(es) → `mail.<primary>` (the
    // FCrDNS / EHLO hostname). The apex address has no rDNS need.
    if let Some(r) = ptr_record(input.mail_ipv4, &mail_host) {
        records.push(r);
    }
    if let Some(v6) = input.mail_ipv6
        && let Some(r) = ptr_record(v6, &mail_host)
    {
        records.push(r);
    }

    records
}

/// The client-reachability row(s) for a **secondary** local domain: the apex
/// `<domain>` `A` (and `AAAA` when the nest has an IPv6) → the **nest** address,
/// so a client that found a handle as `bob@<domain>` can reach this nest via
/// that domain (`docs/goal/behavior/mail-multidomain.md` § Client reachability of
/// a secondary domain). This mirrors the primary's apex `A` in
/// [`build_host_dns_records`], but a secondary domain gets **only** the apex →
/// nest row: mail still flows to the shared `mail.<primary>` MX (so no `mail.` /
/// `PTR` / `relay.` rows belong here), and no `_fauna._tcp` SRV is needed — the
/// client's node resolver falls back to `:443` when the SRV is absent, exactly as
/// it does for the primary (which publishes no SRV either). Same nest address as
/// the primary's apex, since the nest serves every domain on one `:443` via SNI.
pub fn build_secondary_apex_records(
    domain: &str,
    nest_ipv4: &str,
    nest_ipv6: Option<&str>,
) -> Vec<DnsRecord> {
    let mut records = vec![addr_record(DnsRecordType::A, domain.to_string(), nest_ipv4)];
    if let Some(v6) = nest_ipv6 {
        records.push(addr_record(DnsRecordType::Aaaa, domain.to_string(), v6));
    }
    records
}

/// The `mail.<domain>` `A` (and `AAAA` when a mail-host IPv6 is set) → the
/// **mail-host** (MX target) address, for a domain that is becoming the
/// deployment's new primary via an in-flight rename
/// (`docs/goal/behavior/mail-primary-domain-rename.md` § Behavior — cert chain
/// re-issue ordering). Pre-flip, the new primary is still a *secondary* — whose
/// apex row [`build_secondary_apex_records`] emits (→ nest) — so this adds the
/// **mail-host** row the secondary shape deliberately omits, letting the client
/// publish `mail.<new> A → mail-host` so the in-process ACME HTTP-01 order can
/// resolve `mail.<new>` and add it as a resolve-gated cert SAN (gated in
/// `bins/fauna-nest/src/acme_http01.rs`). Same mail-host address as the primary's
/// `mail.` rows in [`build_host_dns_records`] (the deployment has one MX host),
/// scoped to just the `mail.` name — no apex / relay / PTR, those stay the
/// primary's. At anchor flip the domain becomes primary and its `mail.` row flows
/// from [`build_host_dns_records`] instead, so this row is only emitted while the
/// rename is pre-flip.
pub fn build_mail_host_records(
    domain: &str,
    mail_ipv4: &str,
    mail_ipv6: Option<&str>,
) -> Vec<DnsRecord> {
    let mail_host = format!("mail.{domain}");
    let mut records = vec![addr_record(DnsRecordType::A, mail_host.clone(), mail_ipv4)];
    if let Some(v6) = mail_ipv6 {
        records.push(addr_record(DnsRecordType::Aaaa, mail_host, v6));
    }
    records
}

/// `_25._tcp.mail.<primary> TLSA 3 1 1 <hex(sha256(SPKI))>` — DANE-EE / SPKI /
/// SHA-256 (RFC 7672 §3.1) pinning the served self-signed floor MX leaf's
/// SubjectPublicKeyInfo. **One** host-level record for the whole deployment:
/// every local domain MXes to the single `mail.<primary>` target
/// ([`super::per_domain::build_mx_record`], not a knob), so a DANE sender to any
/// domain looks up the TLSA here. The nest emits it **only while the MX is on
/// the floor** and pins the *stable floor-key* SPKI, so a routine renewal never
/// churns it and a trusted-cert takeover withdraws it
/// (`docs/goal/architecture/nest/tls-certificates.md` § D — the cert-coupling,
/// symmetric to the 5a published-MTA-STS-mode coupling). Pure: the nest reads
/// the served-leaf SPKI + on-floor signal and frames the record here.
pub fn build_mail_tlsa_record(primary_domain: &str, spki_sha256: &[u8; 32]) -> DnsRecord {
    use std::fmt::Write;
    let mut hex = String::with_capacity(64);
    for byte in spki_sha256 {
        let _ = write!(hex, "{byte:02x}");
    }
    DnsRecord {
        record_type: DnsRecordType::Tlsa,
        name: format!("_25._tcp.mail.{primary_domain}"),
        // DANE-EE (3) / SubjectPublicKeyInfo (1) / SHA-256 (1) — the exact
        // presentation form of `super::super::outbound::dane::tlsa_policy_string`.
        body: format!("3 1 1 {hex}"),
        ttl_seconds: DEFAULT_TTL_SECS,
    }
}

/// The reverse-DNS owner name for an IP literal: `in-addr.arpa` for IPv4
/// (`203.0.113.7` → `7.113.0.203.in-addr.arpa`) and the nibble-reversed
/// `ip6.arpa` for IPv6, per RFC 1035 §3.5 / RFC 3596 §2.5. `None` if `ip` isn't
/// a parseable address.
pub fn reverse_ptr_name(ip: &str) -> Option<String> {
    let addr: IpAddr = ip.trim().parse().ok()?;
    let suffix = match addr {
        IpAddr::V4(_) => "in-addr.arpa",
        IpAddr::V6(_) => "ip6.arpa",
    };
    Some(reversed_labels(addr, suffix))
}

/// `<reversed-address-labels>.<suffix>` for `ip` — reversed dotted octets
/// (IPv4) or reversed hex nibbles (IPv6), joined to a caller-chosen suffix.
/// The shared reversal behind both PTR names ([`reverse_ptr_name`]'s
/// `ip6.arpa`/`in-addr.arpa`) and [`crate::deliverability::dnsbl_query_name`]'s
/// caller-supplied blocklist suffix.
pub(crate) fn reversed_labels(ip: IpAddr, suffix: &str) -> String {
    let suffix = suffix.trim_matches('.');
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, d] = v4.octets();
            format!("{d}.{c}.{b}.{a}.{suffix}")
        }
        IpAddr::V6(v6) => {
            // Each of the 32 hex nibbles, least-significant first, dot-separated.
            let mut labels = String::with_capacity(64);
            for octet in v6.octets().iter().rev() {
                let lo = octet & 0x0f;
                let hi = (octet >> 4) & 0x0f;
                // Low nibble precedes high nibble (reverse byte order, reverse nibble).
                labels.push_str(&format!("{lo:x}.{hi:x}."));
            }
            format!("{labels}{suffix}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_tlsa_record_is_dane_ee_spki_sha256() {
        // A known 32-byte SPKI digest → lowercase hex `3 1 1 <64 hex chars>`.
        let mut spki = [0u8; 32];
        for (i, b) in spki.iter_mut().enumerate() {
            *b = i as u8;
        }
        let r = build_mail_tlsa_record("example.com", &spki);
        assert_eq!(r.record_type, DnsRecordType::Tlsa);
        // One host-level record at the shared MX host, NOT per-domain.
        assert_eq!(r.name, "_25._tcp.mail.example.com");
        assert_eq!(
            r.body,
            "3 1 1 000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        );
        assert_eq!(r.ttl_seconds, DEFAULT_TTL_SECS);
        // The hex tail is the 64-char lowercase SHA-256 of the SPKI.
        let (prefix, hex) = r.body.split_at(6);
        assert_eq!(prefix, "3 1 1 ");
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn mail_tlsa_record_anchors_at_primary_mail_host() {
        // The owner name is always `_25._tcp.mail.<primary>` — additional domains
        // share the one MX host, so the pin lives at the primary's mail host.
        let spki = [0xabu8; 32];
        let r = build_mail_tlsa_record("dev.fauna.social", &spki);
        assert_eq!(r.name, "_25._tcp.mail.dev.fauna.social");
        assert!(r.body.starts_with("3 1 1 "));
        assert!(r.body.ends_with(&"ab".repeat(32)));
    }

    #[test]
    fn secondary_apex_points_at_nest_ipv4_only() {
        // A secondary domain gets ONLY the apex A → nest address — no mail A, no
        // PTR, no relay row (mail rides the shared mail.<primary> MX).
        let records = build_secondary_apex_records("domain2.example", "203.0.113.7", None);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].record_type, DnsRecordType::A);
        assert_eq!(records[0].name, "domain2.example");
        assert_eq!(records[0].body, "203.0.113.7");
        assert_eq!(records[0].ttl_seconds, DEFAULT_TTL_SECS);
    }

    #[test]
    fn secondary_apex_emits_aaaa_when_nest_has_ipv6() {
        let records =
            build_secondary_apex_records("domain2.example", "203.0.113.7", Some("2001:db8::7"));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].record_type, DnsRecordType::A);
        assert_eq!(records[0].body, "203.0.113.7");
        assert_eq!(records[1].record_type, DnsRecordType::Aaaa);
        assert_eq!(records[1].name, "domain2.example");
        assert_eq!(records[1].body, "2001:db8::7");
    }

    #[test]
    fn rename_mail_host_points_at_mail_ipv4_only() {
        // The rename's new-primary `mail.<new>` gets ONLY the mail-host A → the
        // MAIL address (not the nest address) — no apex, no PTR, no relay.
        let records = build_mail_host_records("new.example", "198.51.100.9", None);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].record_type, DnsRecordType::A);
        assert_eq!(records[0].name, "mail.new.example");
        assert_eq!(records[0].body, "198.51.100.9");
        assert_eq!(records[0].ttl_seconds, DEFAULT_TTL_SECS);
    }

    #[test]
    fn rename_mail_host_emits_aaaa_when_mail_has_ipv6() {
        let records = build_mail_host_records("new.example", "198.51.100.9", Some("2001:db8::9"));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "mail.new.example");
        assert_eq!(records[0].body, "198.51.100.9");
        assert_eq!(records[1].record_type, DnsRecordType::Aaaa);
        assert_eq!(records[1].name, "mail.new.example");
        assert_eq!(records[1].body, "2001:db8::9");
    }

    #[test]
    fn reverse_ptr_v4() {
        assert_eq!(
            reverse_ptr_name("203.0.113.7").unwrap(),
            "7.113.0.203.in-addr.arpa"
        );
    }

    #[test]
    fn reverse_ptr_v6_nibble_reversed() {
        // 2001:db8::1 fully expanded to 32 nibbles, least-significant first.
        assert_eq!(
            reverse_ptr_name("2001:db8::1").unwrap(),
            "1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.8.b.d.0.1.0.0.2.ip6.arpa"
        );
    }

    #[test]
    fn reverse_ptr_rejects_garbage() {
        assert_eq!(reverse_ptr_name("not-an-ip"), None);
    }

    #[test]
    fn host_records_single_box_same_ip() {
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: None,
            mail_ipv4: "203.0.113.7",
            mail_ipv6: None,
            relay_enabled: false,
            pds_enabled: false,
        };
        let records = build_host_dns_records(&input);
        // apex A, mail A, PTR(mail v4).
        assert_eq!(records.len(), 3);

        assert_eq!(records[0].record_type, DnsRecordType::A);
        assert_eq!(records[0].name, "example.com");
        assert_eq!(records[0].body, "203.0.113.7");

        assert_eq!(records[1].record_type, DnsRecordType::A);
        assert_eq!(records[1].name, "mail.example.com");
        assert_eq!(records[1].body, "203.0.113.7");

        assert_eq!(records[2].record_type, DnsRecordType::Ptr);
        assert_eq!(records[2].name, "7.113.0.203.in-addr.arpa");
        assert_eq!(records[2].body, "mail.example.com");

        assert!(records.iter().all(|r| r.ttl_seconds == DEFAULT_TTL_SECS));
    }

    #[test]
    fn host_records_split_box_distinct_mail_ip() {
        // The MX target resolves to a DIFFERENT IP than the apex (mail server on
        // its own box) — the directive this slice exists for.
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9",
            mail_ipv6: None,
            relay_enabled: false,
            pds_enabled: false,
        };
        let records = build_host_dns_records(&input);
        let apex_a = records
            .iter()
            .find(|r| r.record_type == DnsRecordType::A && r.name == "example.com")
            .unwrap();
        let mail_a = records
            .iter()
            .find(|r| r.record_type == DnsRecordType::A && r.name == "mail.example.com")
            .unwrap();
        assert_eq!(apex_a.body, "203.0.113.7");
        assert_eq!(mail_a.body, "198.51.100.9");
        // PTR is on the mail-host IP (the FCrDNS-relevant one), not the apex.
        let ptr = records
            .iter()
            .find(|r| r.record_type == DnsRecordType::Ptr)
            .unwrap();
        assert_eq!(ptr.name, "9.100.51.198.in-addr.arpa");
        assert_eq!(ptr.body, "mail.example.com");
    }

    #[test]
    fn host_records_with_ipv6_emit_aaaa_and_v6_ptr() {
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: Some("2001:db8::7"),
            mail_ipv4: "203.0.113.7",
            mail_ipv6: Some("2001:db8::9"),
            relay_enabled: false,
            pds_enabled: false,
        };
        let records = build_host_dns_records(&input);
        // apex A+AAAA, mail A+AAAA, PTR v4, PTR v6 = 6.
        assert_eq!(records.len(), 6);
        let aaaa: Vec<_> = records
            .iter()
            .filter(|r| r.record_type == DnsRecordType::Aaaa)
            .collect();
        assert_eq!(aaaa.len(), 2);
        assert_eq!(aaaa[0].name, "example.com");
        assert_eq!(aaaa[0].body, "2001:db8::7");
        assert_eq!(aaaa[1].name, "mail.example.com");
        assert_eq!(aaaa[1].body, "2001:db8::9");
        // Two PTRs: v4 + v6, both → mail.example.com.
        let ptrs: Vec<_> = records
            .iter()
            .filter(|r| r.record_type == DnsRecordType::Ptr)
            .collect();
        assert_eq!(ptrs.len(), 2);
        assert!(ptrs.iter().all(|r| r.body == "mail.example.com"));
        assert!(ptrs.iter().any(|r| r.name.ends_with(".ip6.arpa")));
    }

    #[test]
    fn relay_row_emitted_at_nest_ip_when_enabled() {
        // Split box: the mail host is a different IP than the nest. The relay is
        // SNI-routed on the nest's :443, so `relay.<primary>` must point at the
        // NEST IP, never the mail-host IP — and only an A (no IPv6 set), no PTR.
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9",
            mail_ipv6: None,
            relay_enabled: true,
            pds_enabled: false,
        };
        let records = build_host_dns_records(&input);
        let relay_a = records
            .iter()
            .find(|r| r.record_type == DnsRecordType::A && r.name == "relay.example.com")
            .expect("relay A row");
        assert_eq!(relay_a.body, "203.0.113.7", "relay → nest IP, not mail IP");
        assert_eq!(relay_a.ttl_seconds, DEFAULT_TTL_SECS);
        // No relay AAAA (no nest IPv6) and no relay PTR.
        assert!(
            !records
                .iter()
                .any(|r| r.name == "relay.example.com" && r.record_type != DnsRecordType::A),
            "relay host has only the A row when no IPv6 is set"
        );
    }

    #[test]
    fn relay_aaaa_emitted_at_nest_ipv6_when_enabled() {
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: Some("2001:db8::7"),
            mail_ipv4: "203.0.113.7",
            mail_ipv6: None,
            relay_enabled: true,
            pds_enabled: false,
        };
        let records = build_host_dns_records(&input);
        let relay_aaaa = records
            .iter()
            .find(|r| r.record_type == DnsRecordType::Aaaa && r.name == "relay.example.com")
            .expect("relay AAAA row");
        // Relay AAAA tracks the NEST IPv6 (the apex), not the mail-host IPv6.
        assert_eq!(relay_aaaa.body, "2001:db8::7");
    }

    #[test]
    fn no_relay_row_when_disabled() {
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: Some("2001:db8::7"),
            mail_ipv4: "203.0.113.7",
            mail_ipv6: None,
            relay_enabled: false,
            pds_enabled: false,
        };
        let records = build_host_dns_records(&input);
        assert!(
            !records.iter().any(|r| r.name == "relay.example.com"),
            "no relay row when the iroh_relay service is disabled"
        );
    }

    // ── pds.<primary> (ATProto PDS bridge) ───────────────────────────────────

    #[test]
    fn pds_row_emitted_at_nest_ip_when_enabled() {
        // Split box: the mail host is a different IP than the nest. The PDS bridge
        // is SNI-routed on the nest's :443 (`pds.*` → 127.0.0.1:8447), so
        // `pds.<primary>` must point at the NEST IP, never the mail-host IP — and
        // only an A (no IPv6 set), no PTR.
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9",
            mail_ipv6: None,
            relay_enabled: false,
            pds_enabled: true,
        };
        let records = build_host_dns_records(&input);
        let pds_a = records
            .iter()
            .find(|r| r.record_type == DnsRecordType::A && r.name == "pds.example.com")
            .expect("pds A row");
        assert_eq!(pds_a.body, "203.0.113.7", "pds → nest IP, not mail IP");
        assert_eq!(pds_a.ttl_seconds, DEFAULT_TTL_SECS);
        assert!(
            !records
                .iter()
                .any(|r| r.name == "pds.example.com" && r.record_type != DnsRecordType::A),
            "pds host has only the A row when no IPv6 is set"
        );
    }

    #[test]
    fn pds_aaaa_emitted_at_nest_ipv6_when_enabled() {
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: Some("2001:db8::7"),
            mail_ipv4: "203.0.113.7",
            mail_ipv6: None,
            relay_enabled: false,
            pds_enabled: true,
        };
        let records = build_host_dns_records(&input);
        let pds_aaaa = records
            .iter()
            .find(|r| r.record_type == DnsRecordType::Aaaa && r.name == "pds.example.com")
            .expect("pds AAAA row");
        // PDS AAAA tracks the NEST IPv6 (the apex), not the mail-host IPv6.
        assert_eq!(pds_aaaa.body, "2001:db8::7");
    }

    #[test]
    fn no_pds_row_when_disabled() {
        // The default for every box that has not stood the ATProto PDS bridge up:
        // nothing prompts the admin to publish a record the ACME order will never
        // ask for (the safe side of the same foot-gun as the relay row).
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: Some("2001:db8::7"),
            mail_ipv4: "203.0.113.7",
            mail_ipv6: None,
            relay_enabled: false,
            pds_enabled: false,
        };
        let records = build_host_dns_records(&input);
        assert!(
            !records.iter().any(|r| r.name.starts_with("pds.")),
            "no pds row when no atproto.pds bridge is approved"
        );
    }

    #[test]
    fn relay_and_pds_rows_coexist_in_stable_order() {
        // Both infra subdomains on at once — each gets its own rows, relay before
        // pds, and neither displaces the apex/mail rows or the PTR.
        let input = HostDnsInput {
            primary_domain: "example.com",
            nest_ipv4: "203.0.113.7",
            nest_ipv6: Some("2001:db8::7"),
            mail_ipv4: "198.51.100.9",
            mail_ipv6: None,
            relay_enabled: true,
            pds_enabled: true,
        };
        let records = build_host_dns_records(&input);
        let names: Vec<&str> = records
            .iter()
            .filter(|r| r.record_type != DnsRecordType::Ptr)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "example.com",
                "example.com",
                "mail.example.com",
                "relay.example.com",
                "relay.example.com",
                "pds.example.com",
                "pds.example.com",
            ],
            "apex A/AAAA, mail A, then relay A/AAAA, then pds A/AAAA"
        );
        // Both infra hosts track the nest address, not the mail host.
        for host in ["relay.example.com", "pds.example.com"] {
            let a = records
                .iter()
                .find(|r| r.record_type == DnsRecordType::A && r.name == host)
                .expect("infra A row");
            assert_eq!(a.body, "203.0.113.7", "{host} → nest IP");
        }
    }
}
