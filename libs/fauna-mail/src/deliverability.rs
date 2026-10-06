//! Pure deliverability-diagnostic primitives.
//!
//! Implements the *check logic* of `docs/goal/behavior/mail-deliverability.md`
//! § Symptom diagnostics (the SPF/DKIM/DMARC/MTA-STS/TLSRPT/reverse-DNS check
//! table) and § Blocklist self-check (the DNSBL query). These are PURE std-only
//! functions over already-resolved DNS strings — the async I/O (the DNS lookups,
//! the gmail `:25` STARTTLS probe, the HTTPS MTA-STS policy-file fetch) is the
//! nest-side orchestrator's job over its `RecordResolver` / `StarttlsProber`
//! seams. Splitting the verdicts out here keeps the shared check logic in one
//! place (priority #2): a future client "Run diagnostics" preview or the Go MTA
//! bridge can reuse the identical pass/warn/fail interpretation.
//!
//! Nothing here does network I/O, so it is WASM-safe and unit-testable in
//! isolation. The orchestration (which checks to run, in what order, assembling
//! the result list, persisting the run) lives in `bins/fauna-nest` adjacent to
//! the `fauna.bridges.run_deliverability_diagnostics` handler, because it is
//! bound to nest's DNS/STARTTLS seams + the deployment's expected-record state.

use std::net::{IpAddr, Ipv4Addr};

/// The deployment's outbound IP is checked against this DNSBL set on the 24h
/// timer + admin force-refresh (`mail-deliverability.md` § Blocklist self-check).
/// This is the self-check's OWN default — deliberately broader than the inbound
/// `mail.inbound.dnsbl_servers` default (`["zen.spamhaus.org"]` only): a
/// self-check sweeps lists major receivers consult even when we don't gate
/// inbound on them. SORBS was dropped 2026-07-08 (service decommissioned 2024 —
/// its dark zones NXDOMAIN, rendering a permanently-green "not listed").
/// Admin-tunable via `mail.outbound.blocklist_self_check_servers` (Tier 2).
pub const DEFAULT_BLOCKLIST_SELF_CHECK_SERVERS: &[&str] = &[
    "zen.spamhaus.org",
    "b.barracudacentral.org",
    "bl.spamcop.net",
];

/// The known peer the outbound-TLS posture probe connects to
/// (`mail-deliverability.md` § Symptom diagnostics → "Outbound TLS to gmail.com"
/// → `gmail-smtp-in.l.google.com:25`).
pub const GMAIL_PROBE_MX_HOST: &str = "gmail-smtp-in.l.google.com";
/// SMTP port for the outbound-TLS posture probe (server-to-server, not submission).
pub const GMAIL_PROBE_MX_PORT: u16 = 25;

// ---------------------------------------------------------------------------
// SPF (RFC 7208)
// ---------------------------------------------------------------------------

/// Outcome of linting the apex SPF TXT record (`mail-deliverability.md`
/// § Symptom diagnostics → "SPF record present" + "SPF record valid").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpfLint {
    /// A `v=spf1 …` TXT record exists at the apex.
    pub found: bool,
    /// The record contains the `mx` mechanism (this deployment relies on `mx`
    /// for SPF — `mail-deliverability.md` "SPF doesn't include `mx`" → Fail).
    pub includes_mx: bool,
    /// Count of DNS-lookup-incurring terms (`include`/`a`/`mx`/`ptr`/`exists` +
    /// the `redirect=` modifier) — RFC 7208 §4.6.4.
    pub lookup_count: u8,
    /// `lookup_count > 10` (RFC 7208 §4.6.4 permerror threshold).
    pub too_many_lookups: bool,
}

/// The first TXT record at the apex that is an SPF policy (`v=spf1`).
pub fn find_spf(txt_records: &[String]) -> Option<&str> {
    txt_records
        .iter()
        .map(|s| s.as_str())
        .find(|s| s.trim_start().to_ascii_lowercase().starts_with("v=spf1"))
}

/// Lint the apex TXT records for SPF presence + validity per RFC 7208.
pub fn lint_spf(txt_records: &[String]) -> SpfLint {
    let Some(spf) = find_spf(txt_records) else {
        return SpfLint::default();
    };
    let mut lint = SpfLint {
        found: true,
        ..SpfLint::default()
    };
    // Skip the leading `v=spf1` version term; count lookup-incurring mechanisms.
    for term in spf.split_whitespace().skip(1) {
        // Strip a leading qualifier (`+ - ~ ?`) then take the mechanism name up
        // to its first `: / =` separator (`include:d`, `mx/24`, `redirect=d`).
        let stripped = term.trim_start_matches(['+', '-', '~', '?']);
        let mech = stripped
            .split([':', '/', '='])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match mech.as_str() {
            "mx" => {
                lint.includes_mx = true;
                lint.lookup_count = lint.lookup_count.saturating_add(1);
            }
            "include" | "a" | "ptr" | "exists" | "redirect" => {
                lint.lookup_count = lint.lookup_count.saturating_add(1);
            }
            _ => {}
        }
    }
    lint.too_many_lookups = lint.lookup_count > 10;
    lint
}

// ---------------------------------------------------------------------------
// DMARC (RFC 7489)
// ---------------------------------------------------------------------------

/// The published DMARC policy mode (`p=` tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmarcMode {
    /// `p=none` — monitor-only (Warn: not enforcing).
    None,
    /// `p=quarantine`.
    Quarantine,
    /// `p=reject`.
    Reject,
}

/// `true` iff a `v=DMARC1` TXT record exists at `_dmarc.<domain>`.
pub fn dmarc_record_present(txt_records: &[String]) -> bool {
    txt_records
        .iter()
        .any(|s| s.trim_start().to_ascii_lowercase().starts_with("v=dmarc1"))
}

/// The published DMARC policy mode, or `None` if no record / no parseable `p=`.
pub fn dmarc_policy_mode(txt_records: &[String]) -> Option<DmarcMode> {
    let rec = txt_records
        .iter()
        .find(|s| s.trim_start().to_ascii_lowercase().starts_with("v=dmarc1"))?;
    let lower = rec.to_ascii_lowercase();
    for tag in lower.split(';') {
        let tag = tag.trim();
        if let Some(v) = tag.strip_prefix("p=") {
            return match v.trim() {
                "none" => Some(DmarcMode::None),
                "quarantine" => Some(DmarcMode::Quarantine),
                "reject" => Some(DmarcMode::Reject),
                _ => None,
            };
        }
    }
    None
}

// ---------------------------------------------------------------------------
// MTA-STS (RFC 8461) + TLSRPT (RFC 8460) record presence
// ---------------------------------------------------------------------------

/// `true` iff the `_mta-sts.<domain>` TXT carries `v=STSv1` + an `id=` tag.
pub fn mta_sts_record_present(txt_records: &[String]) -> bool {
    txt_records.iter().any(|s| {
        let l = s.to_ascii_lowercase();
        l.contains("v=stsv1") && l.contains("id=")
    })
}

/// `true` iff the `_smtp._tls.<domain>` TXT carries `v=TLSRPTv1`.
pub fn tlsrpt_record_present(txt_records: &[String]) -> bool {
    txt_records
        .iter()
        .any(|s| s.to_ascii_lowercase().contains("v=tlsrptv1"))
}

// ---------------------------------------------------------------------------
// DKIM (RFC 6376) per-selector presence + pubkey match
// ---------------------------------------------------------------------------

/// `true` iff a DKIM TXT record (carries a `p=` public-key tag) exists at
/// `<selector>._domainkey.<domain>`.
pub fn dkim_record_present(txt_records: &[String]) -> bool {
    extract_dkim_pubkey(txt_records).is_some()
}

/// Extract the base64 `p=` public-key value from a DKIM TXT record (joining the
/// record's character-strings first), whitespace-stripped. `None` if absent.
pub fn extract_dkim_pubkey(txt_records: &[String]) -> Option<String> {
    let joined: String = txt_records.concat();
    let lower = joined.to_ascii_lowercase();
    let idx = lower.find("p=")?;
    let value: String = joined[idx + 2..]
        .chars()
        .take_while(|c| *c != ';')
        .collect();
    let normalized: String = value.split_whitespace().collect();
    if normalized.is_empty() {
        // `p=` with an empty value is a *revoked* key, not a usable record.
        None
    } else {
        Some(normalized)
    }
}

/// `true` iff the observed DKIM record's `p=` pubkey matches the deployment's
/// expected `public_dns_value` (the full TXT body from `list_dkim_selectors`).
/// Both sides have their `p=` extracted + whitespace-normalized before compare.
pub fn dkim_pubkey_matches(observed_txt: &[String], expected_dns_value: &str) -> bool {
    let observed = extract_dkim_pubkey(observed_txt);
    let expected = extract_dkim_pubkey(std::slice::from_ref(&expected_dns_value.to_string()));
    match (observed, expected) {
        (Some(o), Some(e)) => o == e,
        // Expected value was a bare base64 (no `v=DKIM1; … p=` wrapper): compare
        // the observed pubkey to it directly, whitespace-normalized.
        (Some(o), None) => o == expected_dns_value.split_whitespace().collect::<String>(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// DNSBL (blocklist self-check)
// ---------------------------------------------------------------------------

/// Build the DNSBL query name for `ip` against `suffix`
/// (`<reversed-IP>.<dnsbl-suffix>`, `mail-deliverability.md` § Blocklist
/// self-check). IPv4 → reversed dotted octets; IPv6 → reversed nibble labels.
pub fn dnsbl_query_name(ip: IpAddr, suffix: &str) -> String {
    crate::dns::host::reversed_labels(ip, suffix)
}

/// The reverse-DNS (PTR) query name for `ip` — `<reversed-octets>.in-addr.arpa`
/// (IPv4) or `<reversed-nibbles>.ip6.arpa` (IPv6). Reuses the same reversal as
/// [`dnsbl_query_name`]; the diagnostic's reverse-DNS checks resolve PTR at this
/// name (`mail-deliverability.md` § Symptom diagnostics → "Reverse-DNS for
/// outbound IP").
pub fn reverse_dns_ptr_name(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(_) => dnsbl_query_name(ip, "in-addr.arpa"),
        IpAddr::V6(_) => dnsbl_query_name(ip, "ip6.arpa"),
    }
}

/// Interpret a DNSBL A-record answer: listed iff any returned A is in
/// `127.0.0.0/8` (the convention — the canonical "listed" is `127.0.0.2`, but
/// providers return `127.0.0.x` sub-codes). NXDOMAIN / empty → not listed.
pub fn dnsbl_listed(a_records: &[String]) -> bool {
    a_records.iter().any(|r| {
        r.trim()
            .parse::<Ipv4Addr>()
            .map(|ip| ip.octets()[0] == 127)
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    #[test]
    fn spf_missing() {
        let lint = lint_spf(&["v=DMARC1; p=reject".to_string()]);
        assert!(!lint.found);
        assert!(!lint.includes_mx);
        assert_eq!(lint.lookup_count, 0);
    }

    #[test]
    fn spf_with_mx_passes() {
        let lint = lint_spf(&["v=spf1 mx ~all".to_string()]);
        assert!(lint.found);
        assert!(lint.includes_mx);
        assert_eq!(lint.lookup_count, 1);
        assert!(!lint.too_many_lookups);
    }

    #[test]
    fn spf_without_mx() {
        let lint = lint_spf(&["v=spf1 include:_spf.example.com -all".to_string()]);
        assert!(lint.found);
        assert!(!lint.includes_mx);
        assert_eq!(lint.lookup_count, 1); // one include
    }

    #[test]
    fn spf_counts_all_lookup_mechanisms() {
        // mx + a + 4 includes + ptr + exists + redirect = 8 lookups; "all" + "ip4" don't count.
        let rec = "v=spf1 mx a:mail.example.com include:a.com include:b.com include:c.com \
                   include:d.com ptr exists:%{i}.e.com ip4:203.0.113.0/24 redirect=fallback.com"
            .to_string();
        let lint = lint_spf(&[rec]);
        assert!(lint.includes_mx);
        assert_eq!(lint.lookup_count, 9);
        assert!(!lint.too_many_lookups);
    }

    #[test]
    fn spf_too_many_lookups() {
        let includes: Vec<String> = (0..11).map(|i| format!("include:d{i}.com")).collect();
        let rec = format!("v=spf1 {} ~all", includes.join(" "));
        let lint = lint_spf(&[rec]);
        assert_eq!(lint.lookup_count, 11);
        assert!(lint.too_many_lookups);
    }

    #[test]
    fn dmarc_modes() {
        assert!(!dmarc_record_present(&["v=spf1 mx ~all".to_string()]));
        assert!(dmarc_record_present(&[
            "v=DMARC1; p=reject; rua=mailto:x@y".to_string()
        ]));
        assert_eq!(
            dmarc_policy_mode(&["v=DMARC1; p=none".to_string()]),
            Some(DmarcMode::None)
        );
        assert_eq!(
            dmarc_policy_mode(&["v=DMARC1; p=quarantine; pct=100".to_string()]),
            Some(DmarcMode::Quarantine)
        );
        assert_eq!(
            dmarc_policy_mode(&["V=DMARC1; P=REJECT".to_string()]),
            Some(DmarcMode::Reject)
        );
        assert_eq!(
            dmarc_policy_mode(&["v=DMARC1; sp=reject".to_string()]),
            None
        );
        assert_eq!(dmarc_policy_mode(&[]), None);
    }

    #[test]
    fn mta_sts_and_tlsrpt_presence() {
        assert!(mta_sts_record_present(&[
            "v=STSv1; id=20260612T120000Z".to_string()
        ]));
        assert!(!mta_sts_record_present(&["v=STSv1".to_string()])); // missing id=
        assert!(!mta_sts_record_present(&["v=spf1 mx ~all".to_string()]));
        assert!(tlsrpt_record_present(&[
            "v=TLSRPTv1; rua=mailto:tlsrpt@example.com".to_string()
        ]));
        assert!(!tlsrpt_record_present(&["v=STSv1; id=x".to_string()]));
    }

    #[test]
    fn dkim_pubkey_extract_and_match() {
        // The pubkey from the deploy-verify NEXT's example.com DKIM record.
        let p = "CcxUQyfB/vzMnyjSzF/sK/WDzuNE3Be/sAqpqOWXrss=";
        let observed = vec![format!("v=DKIM1; k=ed25519; p={p}")];
        assert!(dkim_record_present(&observed));
        assert_eq!(extract_dkim_pubkey(&observed).as_deref(), Some(p));
        // Expected = the full DNS value (as list_dkim_selectors stores it).
        assert!(dkim_pubkey_matches(
            &observed,
            &format!("v=DKIM1; k=ed25519; p={p}")
        ));
        // A mismatched key fails.
        assert!(!dkim_pubkey_matches(
            &observed,
            "v=DKIM1; k=ed25519; p=AAAAdifferentkeyAAAA="
        ));
        // Multi-string TXT record (character-strings joined) still extracts.
        let split = vec!["v=DKIM1; k=ed25519; ".to_string(), format!("p={p}")];
        assert_eq!(extract_dkim_pubkey(&split).as_deref(), Some(p));
        // Revoked key (`p=`) is treated as absent.
        assert!(!dkim_record_present(
            &["v=DKIM1; k=ed25519; p=".to_string()]
        ));
    }

    #[test]
    fn dnsbl_query_name_ipv4() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(
            dnsbl_query_name(ip, "zen.spamhaus.org"),
            "7.113.0.203.zen.spamhaus.org"
        );
        // Trailing/leading dots on the suffix are normalized.
        assert_eq!(
            dnsbl_query_name(ip, ".bl.spamcop.net."),
            "7.113.0.203.bl.spamcop.net"
        );
    }

    #[test]
    fn dnsbl_query_name_ipv6() {
        // ::1 → 31 zero-nibbles then "1", little-nibble-first.
        let ip = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let name = dnsbl_query_name(ip, "example.dnsbl");
        assert!(name.ends_with(".example.dnsbl"));
        let labels: Vec<&str> = name.trim_end_matches(".example.dnsbl").split('.').collect();
        assert_eq!(labels.len(), 32);
        assert_eq!(labels[0], "1");
        assert!(labels[1..].iter().all(|l| *l == "0"));
    }

    #[test]
    fn reverse_dns_ptr_name_v4_and_v6() {
        let v4: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(reverse_dns_ptr_name(v4), "7.113.0.203.in-addr.arpa");
        let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let name = reverse_dns_ptr_name(v6);
        assert!(name.ends_with(".ip6.arpa"));
        assert!(name.starts_with("1.0.0.0."));
    }

    #[test]
    fn dnsbl_listed_interpretation() {
        assert!(dnsbl_listed(&["127.0.0.2".to_string()]));
        assert!(dnsbl_listed(&["127.0.0.10".to_string()])); // sub-code
        assert!(!dnsbl_listed(&[])); // NXDOMAIN
        assert!(!dnsbl_listed(&["203.0.113.7".to_string()])); // non-loopback (shouldn't happen, but not "listed")
    }
}
