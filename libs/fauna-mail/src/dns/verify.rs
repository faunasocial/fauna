//! Pure observed-vs-expected DNS record comparison for the unified
//! DNS-management verify surface, per
//! `docs/goal/behavior/dns-management.md` § Live verification (design
//! tracked internally, § Verification).
//!
//! The *resolution* (a public-recursive `hickory-resolver` lookup) is nest-side
//! I/O — these functions are the pure half: given the value Fauna would publish
//! (the zone-file `expected` body the [`per_domain`](super::per_domain) builders
//! emit) and the set of values public DNS actually serves, decide whether the
//! record is [`Ok`](RecordVerifyStatus::Ok) (green), [`Missing`] or
//! [`Mismatch`] (red). The split mirrors the perimeter-scorer / MTA-STS pattern
//! (pure logic shared, network I/O at the edge) so the nest verifier and any
//! future client preview agree byte-for-byte on what "correct" means.
//!
//! "Correct" = exact-match after type-appropriate normalization (design
//! § Record matrix): TXT segments joined + zone-file quoting stripped; names
//! compared trailing-dot- and case-insensitively; MX as a (priority, target)
//! tuple; A/AAAA as address-set membership.

/// Per-record verification verdict. `Ok`/`Missing`/`Mismatch` are the pure
/// comparison outcomes this module produces; `Checking` is the transient state
/// the nest verifier layers on when a lookup is in-flight, timed out, or
/// rate-limited (we genuinely don't know yet) — never returned by the
/// comparators here. The wire `status` string (`ok|missing|mismatch|checking`)
/// is produced by [`RecordVerifyStatus::as_wire_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordVerifyStatus {
    /// Public DNS serves the expected value.
    Ok,
    /// No record of this type exists at this name (NXDOMAIN / empty RRset).
    Missing,
    /// Record(s) exist but none match the expected value.
    Mismatch,
    /// The lookup hasn't resolved yet (transient error / rate-limited).
    Checking,
}

impl RecordVerifyStatus {
    /// The wire `status` token consumed by `fauna.dns.verify_records`.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            RecordVerifyStatus::Ok => "ok",
            RecordVerifyStatus::Missing => "missing",
            RecordVerifyStatus::Mismatch => "mismatch",
            RecordVerifyStatus::Checking => "checking",
        }
    }
}

/// Normalize a DNS owner/target name for comparison: ASCII-lowercase and drop a
/// single trailing dot (the root label hickory appends on FQDNs). Idempotent.
pub fn normalize_name(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Strip the surrounding double-quotes the zone-file wire form wraps TXT bodies
/// in (the [`per_domain`](super::per_domain) builders frame every TXT body as
/// `"\"...\""`). Observed values from a resolver are already unquoted (the joined
/// character-string segments), so only the expected side needs unwrapping. A
/// body without the wrapping quotes is returned unchanged.
fn unquote_txt(wire: &str) -> &str {
    wire.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(wire)
}

/// Compare an expected TXT body (zone-file form, e.g. `"\"v=spf1 mx ~all\""`)
/// against the TXT records public DNS serves at the name. `observed` holds one
/// entry per RR, each the RR's character-string segments already joined. The
/// record is [`Ok`](RecordVerifyStatus::Ok) when any observed RR equals the
/// unquoted expected body, [`Missing`](RecordVerifyStatus::Missing) when no TXT
/// exists, else [`Mismatch`](RecordVerifyStatus::Mismatch).
pub fn compare_txt(expected_wire: &str, observed: &[String]) -> RecordVerifyStatus {
    if observed.is_empty() {
        return RecordVerifyStatus::Missing;
    }
    let expected = unquote_txt(expected_wire);
    if observed.iter().any(|o| o == expected) {
        RecordVerifyStatus::Ok
    } else {
        RecordVerifyStatus::Mismatch
    }
}

/// Parse a `<priority> <host>` MX zone-file body into its (priority, normalized
/// target) tuple. Returns `None` if the body isn't the expected two-token shape.
fn parse_mx(body: &str) -> Option<(u16, String)> {
    let mut parts = body.split_whitespace();
    let pref: u16 = parts.next()?.parse().ok()?;
    let host = parts.next()?;
    // No third token: a well-formed MX body is exactly "<pref> <host>".
    if parts.next().is_some() {
        return None;
    }
    Some((pref, normalize_name(host)))
}

/// Compare an expected MX body (`"10 mail.example.com"`) against the MX records
/// public DNS serves. Each `observed` entry is a `<priority> <host>` string.
/// Matching is on the (priority, normalized-target) tuple per design
/// § Record matrix.
pub fn compare_mx(expected_wire: &str, observed: &[String]) -> RecordVerifyStatus {
    if observed.is_empty() {
        return RecordVerifyStatus::Missing;
    }
    let Some(expected) = parse_mx(expected_wire) else {
        // A malformed expected body can never match anything observed.
        return RecordVerifyStatus::Mismatch;
    };
    if observed
        .iter()
        .filter_map(|o| parse_mx(o))
        .any(|got| got == expected)
    {
        RecordVerifyStatus::Ok
    } else {
        RecordVerifyStatus::Mismatch
    }
}

/// Parse a `<priority> <weight> <port> <target>` SRV zone-file body into its
/// (priority, weight, port, normalized-target) tuple. Returns `None` if the body
/// isn't the expected four-token shape (so a malformed body never spuriously
/// matches). Per RFC 2782 / RFC 6764 § 4.
fn parse_srv(body: &str) -> Option<(u16, u16, u16, String)> {
    let mut parts = body.split_whitespace();
    let priority: u16 = parts.next()?.parse().ok()?;
    let weight: u16 = parts.next()?.parse().ok()?;
    let port: u16 = parts.next()?.parse().ok()?;
    let target = parts.next()?;
    // No fifth token: a well-formed SRV body is exactly four fields.
    if parts.next().is_some() {
        return None;
    }
    Some((priority, weight, port, normalize_name(target)))
}

/// Compare an expected `SRV` body (`"0 1 443 mail.example.com."`) against the
/// SRV records public DNS serves at the name (e.g. `_caldavs._tcp.<domain>`).
/// Each `observed` entry is a `<priority> <weight> <port> <target>` string.
/// Matching is on the full (priority, weight, port, normalized-target) tuple —
/// a self-published record has one exact expected value, mirroring the MX
/// (priority, target) tuple match.
pub fn compare_srv(expected_wire: &str, observed: &[String]) -> RecordVerifyStatus {
    if observed.is_empty() {
        return RecordVerifyStatus::Missing;
    }
    let Some(expected) = parse_srv(expected_wire) else {
        // A malformed expected body can never match anything observed.
        return RecordVerifyStatus::Mismatch;
    };
    if observed
        .iter()
        .filter_map(|o| parse_srv(o))
        .any(|got| got == expected)
    {
        RecordVerifyStatus::Ok
    } else {
        RecordVerifyStatus::Mismatch
    }
}

/// Compare an expected A/AAAA address (an IP literal) against the address set
/// public DNS serves, by set membership (design § Record matrix: "A/AAAA =
/// address-set membership"). Both sides are parsed to [`std::net::IpAddr`] so
/// equivalent textual forms (e.g. compressed vs expanded IPv6) compare equal;
/// an unparseable observed entry simply never matches.
pub fn compare_addr(expected: &str, observed: &[String]) -> RecordVerifyStatus {
    if observed.is_empty() {
        return RecordVerifyStatus::Missing;
    }
    let Ok(want) = expected.trim().parse::<std::net::IpAddr>() else {
        return RecordVerifyStatus::Mismatch;
    };
    if observed
        .iter()
        .filter_map(|o| o.trim().parse::<std::net::IpAddr>().ok())
        .any(|got| got == want)
    {
        RecordVerifyStatus::Ok
    } else {
        RecordVerifyStatus::Mismatch
    }
}

/// Compare an expected `PTR` target (the FQDN the address should reverse-resolve
/// to, e.g. `mail.example.com`) against the names public DNS serves at the
/// reverse-pointer name. Matching is name-set membership after
/// [`normalize_name`] (trailing-dot- and case-insensitive), mirroring the
/// forward-name comparison so forward-confirmed rDNS lines up. An empty observed
/// set is [`Missing`](RecordVerifyStatus::Missing).
pub fn compare_ptr(expected_name: &str, observed: &[String]) -> RecordVerifyStatus {
    if observed.is_empty() {
        return RecordVerifyStatus::Missing;
    }
    let expected = normalize_name(expected_name);
    if observed.iter().any(|o| normalize_name(o) == expected) {
        RecordVerifyStatus::Ok
    } else {
        RecordVerifyStatus::Mismatch
    }
}

/// Parse a `<usage> <selector> <matching> <hex>` TLSA presentation body (RFC
/// 7672 §3) into its four fields, `hex` un-normalized (caller's case to keep).
/// `None` if the body isn't the 4-token shape or the numeric params aren't
/// `u8` — a malformed expected/observed body never matches. Shared with
/// `fauna_provisioning::dns::cloudflare`'s TLSA record builder, which needs
/// the same fields but preserves the certificate's original case for the API
/// body rather than normalizing it for comparison.
pub fn parse_tlsa_fields(body: &str) -> Option<(u8, u8, u8, &str)> {
    let mut parts = body.split_whitespace();
    let usage: u8 = parts.next()?.parse().ok()?;
    let selector: u8 = parts.next()?.parse().ok()?;
    let matching: u8 = parts.next()?.parse().ok()?;
    let hex = parts.next()?;
    // Exactly four tokens — a well-formed TLSA body is `<u> <s> <m> <hex>`.
    if parts.next().is_some() {
        return None;
    }
    Some((usage, selector, matching, hex))
}

/// [`parse_tlsa_fields`] with the hex normalized to lowercase, for comparing
/// an expected body against what DNS observed.
fn parse_tlsa(body: &str) -> Option<(u8, u8, u8, String)> {
    let (usage, selector, matching, hex) = parse_tlsa_fields(body)?;
    Some((usage, selector, matching, hex.to_ascii_lowercase()))
}

/// Compare an expected `TLSA` body (`"3 1 1 <hex>"`, the floor-MX DANE record —
/// [`super::host::build_mail_tlsa_record`]) against the TLSA records public DNS
/// serves at `_25._tcp.mail.<primary>`. Each `observed` entry is a `<usage>
/// <selector> <matching> <hex>` presentation string. Matches on the normalized
/// tuple (hex case-insensitive). This verifies *what we publish is published*
/// (the admin-dns red/green), **not** DANE security — DNSSEC-proof validation is
/// the sending MTA's job (`tls-certificates.md` § D). Empty observed →
/// [`Missing`](RecordVerifyStatus::Missing).
pub fn compare_tlsa(expected_wire: &str, observed: &[String]) -> RecordVerifyStatus {
    if observed.is_empty() {
        return RecordVerifyStatus::Missing;
    }
    let Some(expected) = parse_tlsa(expected_wire) else {
        return RecordVerifyStatus::Mismatch;
    };
    if observed
        .iter()
        .filter_map(|o| parse_tlsa(o))
        .any(|got| got == expected)
    {
        RecordVerifyStatus::Ok
    } else {
        RecordVerifyStatus::Mismatch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_normalization_strips_trailing_dot_and_case() {
        assert_eq!(normalize_name("Mail.Example.COM."), "mail.example.com");
        assert_eq!(normalize_name("example.com"), "example.com");
        // Idempotent.
        let once = normalize_name("Mail.Example.COM.");
        assert_eq!(normalize_name(&once), once);
    }

    #[test]
    fn tlsa_ok_when_tuple_matches_case_insensitively() {
        let expected = format!("3 1 1 {}", "ab".repeat(32));
        // Same tuple, uppercase hex from the resolver still matches.
        let observed = vec![format!("3 1 1 {}", "AB".repeat(32))];
        assert_eq!(compare_tlsa(&expected, &observed), RecordVerifyStatus::Ok);
    }

    #[test]
    fn tlsa_missing_when_no_records() {
        assert_eq!(
            compare_tlsa("3 1 1 deadbeef", &[]),
            RecordVerifyStatus::Missing
        );
    }

    #[test]
    fn tlsa_mismatch_on_different_pin_or_params() {
        let expected = "3 1 1 deadbeef";
        // Different SPKI digest.
        assert_eq!(
            compare_tlsa(expected, &["3 1 1 cafebabe".to_string()]),
            RecordVerifyStatus::Mismatch
        );
        // Right digest, wrong usage (a CA-pin, not our DANE-EE floor pin).
        assert_eq!(
            compare_tlsa(expected, &["2 1 1 deadbeef".to_string()]),
            RecordVerifyStatus::Mismatch
        );
    }

    #[test]
    fn tlsa_malformed_expected_never_matches() {
        // Not the 4-token shape → can't match anything observed.
        assert_eq!(
            compare_tlsa("3 1 deadbeef", &["3 1 1 deadbeef".to_string()]),
            RecordVerifyStatus::Mismatch
        );
    }

    #[test]
    fn txt_ok_when_unquoted_expected_matches_an_observed_rr() {
        let status = compare_txt("\"v=spf1 mx ~all\"", &["v=spf1 mx ~all".to_string()]);
        assert_eq!(status, RecordVerifyStatus::Ok);
    }

    #[test]
    fn txt_ok_among_multiple_observed_records() {
        let status = compare_txt(
            "\"v=DKIM1; k=ed25519; p=abc\"",
            &[
                "v=spf1 mx ~all".to_string(),
                "v=DKIM1; k=ed25519; p=abc".to_string(),
            ],
        );
        assert_eq!(status, RecordVerifyStatus::Ok);
    }

    #[test]
    fn txt_missing_when_no_records() {
        assert_eq!(
            compare_txt("\"v=spf1 mx ~all\"", &[]),
            RecordVerifyStatus::Missing
        );
    }

    #[test]
    fn txt_mismatch_when_present_but_different() {
        let status = compare_txt("\"v=spf1 mx ~all\"", &["v=spf1 -all".to_string()]);
        assert_eq!(status, RecordVerifyStatus::Mismatch);
    }

    #[test]
    fn mx_ok_ignores_target_case_and_trailing_dot() {
        let status = compare_mx("10 mail.example.com", &["10 Mail.Example.com.".to_string()]);
        assert_eq!(status, RecordVerifyStatus::Ok);
    }

    #[test]
    fn mx_mismatch_on_wrong_priority() {
        let status = compare_mx("10 mail.example.com", &["20 mail.example.com".to_string()]);
        assert_eq!(status, RecordVerifyStatus::Mismatch);
    }

    #[test]
    fn mx_mismatch_on_wrong_target() {
        let status = compare_mx("10 mail.example.com", &["10 mx.other.test".to_string()]);
        assert_eq!(status, RecordVerifyStatus::Mismatch);
    }

    #[test]
    fn mx_missing_when_no_records() {
        assert_eq!(
            compare_mx("10 mail.example.com", &[]),
            RecordVerifyStatus::Missing
        );
    }

    #[test]
    fn srv_ok_ignores_target_case_and_trailing_dot() {
        let status = compare_srv(
            "0 1 443 mail.example.com",
            &["0 1 443 Mail.Example.com.".to_string()],
        );
        assert_eq!(status, RecordVerifyStatus::Ok);
    }

    #[test]
    fn srv_ok_among_multiple_observed_records() {
        let status = compare_srv(
            "0 1 443 mail.example.com",
            &[
                "10 0 8443 other.host.test.".to_string(),
                "0 1 443 mail.example.com.".to_string(),
            ],
        );
        assert_eq!(status, RecordVerifyStatus::Ok);
    }

    #[test]
    fn srv_mismatch_on_wrong_port() {
        let status = compare_srv(
            "0 1 443 mail.example.com",
            &["0 1 8443 mail.example.com.".to_string()],
        );
        assert_eq!(status, RecordVerifyStatus::Mismatch);
    }

    #[test]
    fn srv_mismatch_on_wrong_target() {
        let status = compare_srv(
            "0 1 443 mail.example.com",
            &["0 1 443 cal.other.test.".to_string()],
        );
        assert_eq!(status, RecordVerifyStatus::Mismatch);
    }

    #[test]
    fn srv_missing_when_no_records() {
        assert_eq!(
            compare_srv("0 1 443 mail.example.com", &[]),
            RecordVerifyStatus::Missing
        );
    }

    #[test]
    fn srv_mismatch_on_malformed_expected() {
        // A non-four-token body parses to None → never matches.
        let status = compare_srv(
            "443 mail.example.com",
            &["0 1 443 mail.example.com.".to_string()],
        );
        assert_eq!(status, RecordVerifyStatus::Mismatch);
    }

    #[test]
    fn addr_ok_on_set_membership() {
        let status = compare_addr(
            "203.0.113.7",
            &["203.0.113.1".to_string(), "203.0.113.7".to_string()],
        );
        assert_eq!(status, RecordVerifyStatus::Ok);
    }

    #[test]
    fn addr_ok_across_ipv6_textual_forms() {
        // Expanded expected vs compressed observed — IpAddr canonicalizes both.
        let status = compare_addr("2001:db8:0:0:0:0:0:1", &["2001:db8::1".to_string()]);
        assert_eq!(status, RecordVerifyStatus::Ok);
    }

    #[test]
    fn addr_mismatch_when_not_in_set() {
        let status = compare_addr("203.0.113.7", &["203.0.113.1".to_string()]);
        assert_eq!(status, RecordVerifyStatus::Mismatch);
    }

    #[test]
    fn addr_missing_when_no_records() {
        assert_eq!(
            compare_addr("203.0.113.7", &[]),
            RecordVerifyStatus::Missing
        );
    }

    #[test]
    fn ptr_ok_ignores_case_and_trailing_dot() {
        let status = compare_ptr("mail.example.com", &["Mail.Example.com.".to_string()]);
        assert_eq!(status, RecordVerifyStatus::Ok);
    }

    #[test]
    fn ptr_ok_among_multiple_observed_names() {
        let status = compare_ptr(
            "mail.example.com",
            &[
                "other.host.test.".to_string(),
                "mail.example.com.".to_string(),
            ],
        );
        assert_eq!(status, RecordVerifyStatus::Ok);
    }

    #[test]
    fn ptr_mismatch_when_points_elsewhere() {
        let status = compare_ptr("mail.example.com", &["vps-123.provider.test.".to_string()]);
        assert_eq!(status, RecordVerifyStatus::Mismatch);
    }

    #[test]
    fn ptr_missing_when_no_records() {
        assert_eq!(
            compare_ptr("mail.example.com", &[]),
            RecordVerifyStatus::Missing
        );
    }

    #[test]
    fn wire_strings_cover_all_variants() {
        assert_eq!(RecordVerifyStatus::Ok.as_wire_str(), "ok");
        assert_eq!(RecordVerifyStatus::Missing.as_wire_str(), "missing");
        assert_eq!(RecordVerifyStatus::Mismatch.as_wire_str(), "mismatch");
        assert_eq!(RecordVerifyStatus::Checking.as_wire_str(), "checking");
    }
}
