//! Per-domain DNS-record body builders per
//! `docs/goal/behavior/mail-multidomain.md` § Per-domain DNS records.
//!
//! Each `build_*_record` returns a strongly-typed [`DnsRecord`] whose `body`
//! is the exact RFC zone-file representation (TXT bodies are quoted; the MX
//! body is `<priority> <host>`). Record formats are compile-time decisions,
//! NOT configurable, per the goal doc § Compile-time decisions:
//!
//! * MX priority is hardcoded `10` (single MX target across all domains).
//! * The DKIM / MTA-STS / TLSRPT / DMARC TXT formats are fixed by RFC.
//! * TTL is the fixed deployment default ([`DEFAULT_TTL_SECS`]); there is no
//!   per-domain TTL column in `mail_domains`.

#[cfg(feature = "multidomain")]
use crate::outbound::dkim::SigningAlg;
#[cfg(feature = "multidomain")]
use crate::outbound::mta_sts::compute_policy_version_hash;

/// DNS record class produced by the builders here. MX and TXT cover the six
/// per-domain mail records; `Srv` covers the CalDAV autodiscovery record
/// (§ CalDAV/CardDAV autodiscovery); `A`/`Aaaa`/`Ptr` and the DANE `Tlsa` are
/// emitted by the nest-host builder ([`super::host`]) for the deployment's own
/// `mail.<primary>` rows — see `docs/goal/behavior/dns-management.md`
/// § Records covered. The `Tlsa` pins the self-signed floor MX leaf and is
/// emitted only while the MX is on the floor (`tls-certificates.md` § D).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsRecordType {
    Mx,
    Txt,
    Srv,
    A,
    Aaaa,
    Ptr,
    Tlsa,
}

/// A single DNS record nest publishes for a local domain. `name` is the owner
/// name (left-hand side), `body` the RFC zone-file RDATA representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsRecord {
    pub record_type: DnsRecordType,
    pub name: String,
    pub body: String,
    pub ttl_seconds: u32,
}

/// Default TTL for every per-domain mail record, per the goal doc
/// § Publication lifecycle ("3600 s default; matches the primary domain's
/// TTL"). Not configurable.
pub const DEFAULT_TTL_SECS: u32 = 3600;

/// Hardcoded MX priority — single MX target across all domains per the goal
/// doc § Compile-time decisions (not a tunable priority).
const MX_PRIORITY: u16 = 10;

/// Default SPF TXT body for a freshly-added local domain, per
/// `mail-multidomain.md` § Per-domain DNS records (`mail_domains.spf_record`
/// default `v=spf1 mx ~all`): the `~all` softfail is the marathon-defensible
/// default (a harder `-all` is admin-opt-in). Shared so the nest's `mail_domains`
/// default, the rendered matrix, and the onboarding publish path agree.
pub const DEFAULT_SPF_BODY: &str = "v=spf1 mx ~all";

/// The single MX-target hostname for a deployment whose primary mail domain is
/// `primary_domain`: `mail.<primary-domain>`. Per `mail-multidomain.md` § One MX
/// target / § Compile-time decisions, every local domain's MX points here (one
/// mail server serving N domains via SNI); it is **not** the apex. Shared so the
/// nest matrix and the onboarding publish path derive the same target.
pub fn mail_host(primary_domain: &str) -> String {
    format!("mail.{primary_domain}")
}

fn txt(name: String, content: &str) -> DnsRecord {
    DnsRecord {
        record_type: DnsRecordType::Txt,
        name,
        body: format!("\"{content}\""),
        ttl_seconds: DEFAULT_TTL_SECS,
    }
}

/// A generic TXT row with this module's quoting/TTL conventions, for callers
/// whose (name, value) derivation lives elsewhere — e.g. the nest matrix's
/// `_atproto.<handle>` handle-verification row, whose strings are owned by
/// `fauna_protocol::atproto::handle_verification_txt` (one owner per claim;
/// this crate deliberately does not depend on fauna-protocol).
pub fn build_txt_record(name: String, content: &str) -> DnsRecord {
    txt(name, content)
}

/// `<domain> MX 10 <primary_mx_host>.` — every local domain points its MX at
/// the deployment's single mail server per the goal doc § Per-domain DNS
/// records. The target is emitted as a fully-qualified name (trailing dot): an
/// MX target without it is, per DNS convention, *relative* to the zone, so a
/// provider that honors that (e.g. Hetzner) appends the origin and serves
/// `mail.<apex>.<apex>` — an unresolvable MX that bounces all external inbound.
pub fn build_mx_record(domain: &str, primary_mx_host: &str) -> DnsRecord {
    let fqdn_host = format!("{}.", primary_mx_host.trim_end_matches('.'));
    DnsRecord {
        record_type: DnsRecordType::Mx,
        name: domain.to_string(),
        body: format!("{MX_PRIORITY} {fqdn_host}"),
        ttl_seconds: DEFAULT_TTL_SECS,
    }
}

/// `<domain> TXT "<spf_body>"` — the SPF record from `mail_domains.spf_record`
/// (default `v=spf1 mx ~all`). The caller supplies the body so per-domain SPF
/// customization flows through.
pub fn build_spf_record(domain: &str, spf_body: &str) -> DnsRecord {
    txt(domain.to_string(), spf_body)
}

/// `<selector>._domainkey.<domain> TXT "<public_dns_value>"` — frames the
/// **stored** DKIM public DNS value (`mail_dkim_keys.public_dns_value` /
/// onboarding's `cloud_init` `public_dns_value`, the full `v=DKIM1; k=…; p=…`
/// string) verbatim. The single publish/verify source per
/// `dns-management.md` § Records covered — NOT recomputed from the algorithm +
/// key, so a re-keyed-but-not-yet-republished selector still renders the value
/// peers actually need. Pure (no `outbound`); shared by the WASM onboarding
/// provisioner and the nest matrix aggregator.
pub fn build_dkim_txt_record_from_value(
    domain: &str,
    selector: &str,
    public_dns_value: &str,
) -> DnsRecord {
    txt(format!("{selector}._domainkey.{domain}"), public_dns_value)
}

/// `<selector>._domainkey.<domain> TXT "v=DKIM1; k=<rsa|ed25519>; p=<pubkey>"`
/// per RFC 6376 §3.6.1 + RFC 8463 §3, **recomputed** from the algorithm + raw
/// base64 public key. `selector` is the short label (e.g. `default`);
/// `algorithm` selects the `k=` tag via [`SigningAlg::as_dns_k_value`]. Prefer
/// [`build_dkim_txt_record_from_value`] when the stored `public_dns_value` is
/// available (the publish/verify source). Gated behind `multidomain` for
/// `SigningAlg::as_dns_k_value`.
#[cfg(feature = "multidomain")]
pub fn build_dkim_txt_record(
    domain: &str,
    selector: &str,
    algorithm: SigningAlg,
    public_key_b64: &str,
) -> DnsRecord {
    build_dkim_txt_record_from_value(
        domain,
        selector,
        &format!(
            "v=DKIM1; k={}; p={public_key_b64}",
            algorithm.as_dns_k_value()
        ),
    )
}

/// `_dmarc.<domain> TXT "<dmarc_body>"` per RFC 7489 §6. The DMARC body is
/// assembled by `dmarc-reporting.md`'s per-domain override logic upstream
/// (there is no DMARC TXT assembler in `fauna-mail` — `auth.rs` only verifies
/// inbound DMARC verdicts), so this builder takes the finished body and frames
/// it as the `_dmarc` TXT record.
pub fn build_dmarc_txt_record(domain: &str, dmarc_body: &str) -> DnsRecord {
    txt(format!("_dmarc.{domain}"), dmarc_body)
}

/// TTL for the transient DNS-01 `_acme-challenge` TXT — deliberately short
/// (unlike the steady-state [`DEFAULT_TTL_SECS`] mail records) so a value left
/// over from a prior order expires quickly and a fresh order's value is not
/// shadowed by a long-cached stale one. The record only needs to live long
/// enough for the CA to read it, then it is torn down. 120 s is the
/// conventional ACME-challenge TTL.
pub const ACME_CHALLENGE_TTL_SECS: u32 = 120;

/// `_acme-challenge.<domain> TXT "<dns_value>"` — the transient DNS-01 ACME
/// challenge record (RFC 8555 §8.4). `dns_value` is the order's
/// key-authorization digest (base64url-encoded SHA-256, e.g. from
/// `instant-acme`'s `KeyAuthorization::dns_value()`); it is framed verbatim as
/// a quoted TXT body at the short [`ACME_CHALLENGE_TTL_SECS`] TTL and torn down
/// once the CA validates. **The nest never publishes this — the admin's client
/// does** (it holds the DNS-provider key); see `tls-certificates.md`
/// § "The `_acme-challenge` record". Pure + WASM-safe (the `dns-records`
/// feature), the single source of the record's shape so the native DNS-01 order
/// (`libs/fauna-client-dns`) and the manual-paste surface render it identically.
pub fn build_acme_challenge_txt_record(domain: &str, dns_value: &str) -> DnsRecord {
    DnsRecord {
        record_type: DnsRecordType::Txt,
        name: format!("_acme-challenge.{domain}"),
        body: format!("\"{dns_value}\""),
        ttl_seconds: ACME_CHALLENGE_TTL_SECS,
    }
}

/// `_mta-sts.<domain> TXT "v=STSv1; id=<sha256_of_policy_body_hex>"` per RFC
/// 8461 §3.1. The `id=` is the hash of the served policy file body, so peer
/// MTAs re-fetch when the policy changes; computed via
/// [`compute_policy_version_hash`]. Gated behind `multidomain` for the hash.
#[cfg(feature = "multidomain")]
pub fn build_mta_sts_txt_record(domain: &str, policy_body: &str) -> DnsRecord {
    let id = compute_policy_version_hash(policy_body);
    txt(format!("_mta-sts.{domain}"), &format!("v=STSv1; id={id}"))
}

/// `_smtp._tls.<domain> TXT "v=TLSRPTv1; rua=mailto:tlsrpt@<primary_domain>"`
/// per RFC 8460 §3. All domains point TLSRPT at the single primary-domain
/// processor inbox per the goal doc § Per-domain TLSRPT.
pub fn build_tlsrpt_txt_record(domain: &str, primary_domain: &str) -> DnsRecord {
    txt(
        format!("_smtp._tls.{domain}"),
        &format!("v=TLSRPTv1; rua=mailto:tlsrpt@{primary_domain}"),
    )
}

/// The port the CalDAV autodiscovery `SRV` record advertises — always `443`
/// (the SNI-passthrough HTTPS surface, never a non-standard port), per
/// `caldav-server.md` § Network exposure. A compile-time decision, not
/// tunable.
pub const CALDAV_SRV_PORT: u16 = 443;

/// `_caldavs._tcp.<domain> SRV 0 1 443 <caldav_host>.` — the RFC 6764 § 4
/// CalDAV service-discovery record so a MUA pointed at the bare domain (macOS
/// Calendar.app *automatic* setup) finds the CalDAV host without the user typing
/// `mail.<domain>` in Advanced mode. `caldav_host` is the deployment's single
/// CalDAV/mail host (`mail.<primary>` = [`mail_host`], also the MX target — the
/// MDA serves every local domain's CalDAV there via SNI), emitted FQDN
/// (trailing dot) for the same zone-relative-doubling reason as
/// [`build_mx_record`]. Priority `0`, weight `1` per the RFC 6764 example (a
/// single target, so the load-balancing fields are nominal). Only the TLS
/// variant (`_caldavs`, never plaintext `_caldav`) is published — CalDAV is
/// HTTPS-only. A sibling `_carddavs._tcp` companion is now published too (see
/// [`build_carddavs_srv_record`]): the same MDA serves a CardDAV surface on the
/// same host/port. After the SRV hop the client uses the MDA's
/// `/.well-known/caldav` (already served), so the optional RFC 6764 § 6 `path=`
/// TXT companion is redundant and omitted. Pure + WASM-safe (no feature gate),
/// so the onboarding publish path can reuse it.
pub fn build_caldavs_srv_record(domain: &str, caldav_host: &str) -> DnsRecord {
    let fqdn_host = format!("{}.", caldav_host.trim_end_matches('.'));
    DnsRecord {
        record_type: DnsRecordType::Srv,
        name: format!("_caldavs._tcp.{domain}"),
        body: format!("0 1 {CALDAV_SRV_PORT} {fqdn_host}"),
        ttl_seconds: DEFAULT_TTL_SECS,
    }
}

/// The port the CardDAV autodiscovery `SRV` record advertises — always `443`,
/// the same SNI-passthrough HTTPS surface CalDAV uses (both DAV surfaces share
/// the one `mail.<domain>:443` listener), per `caldav-server.md` § Network
/// exposure. A compile-time decision, not tunable. Equal to
/// [`CALDAV_SRV_PORT`] by construction — kept as its own named constant so the
/// CardDAV surface's port is a self-contained decision, not a borrowed one.
pub const CARDDAV_SRV_PORT: u16 = 443;

/// `_carddavs._tcp.<domain> SRV 0 1 443 <carddav_host>.` — the RFC 6764 § 4
/// CardDAV service-discovery record, the sibling of [`build_caldavs_srv_record`]
/// so macOS Contacts.app / DAVx5 *automatic* account setup (which queries
/// `_carddavs._tcp.<domain>` and declines the apex cross-host redirect) finds the
/// address-book host without the user typing `mail.<domain>` in Advanced mode.
/// Byte-identical to the CalDAV record save the `_carddavs` service label: same
/// `carddav_host` (`mail.<primary>` = the shared MDA host, also the MX/CalDAV
/// target — every local domain's CardDAV is served there via SNI), same priority
/// `0` / weight `1` / port `443`, same FQDN trailing dot. Only the TLS variant
/// (`_carddavs`, never plaintext `_carddav`) is published — the DAV surface is
/// HTTPS-only. After the SRV hop the client uses the MDA's `/.well-known/carddav`
/// (RFC 6764 § 6 `path=` TXT companion redundant, omitted). Published since the
/// CardDAV surface shipped (slices 1–2d, 2026-07-05);
/// `caldav-server.md` § Network exposure sanctions it (2026-07-06 flip). Pure +
/// WASM-safe (no feature gate) so the onboarding publish path can reuse it.
pub fn build_carddavs_srv_record(domain: &str, carddav_host: &str) -> DnsRecord {
    let fqdn_host = format!("{}.", carddav_host.trim_end_matches('.'));
    DnsRecord {
        record_type: DnsRecordType::Srv,
        name: format!("_carddavs._tcp.{domain}"),
        body: format!("0 1 {CARDDAV_SRV_PORT} {fqdn_host}"),
        ttl_seconds: DEFAULT_TTL_SECS,
    }
}

/// One DKIM selector's published TXT, for [`DomainDnsInput`]. `public_dns_value`
/// is the **stored** `mail_dkim_keys.public_dns_value` (`v=DKIM1; k=…; p=…`),
/// used verbatim — the single publish/verify source per
/// `docs/goal/behavior/dns-management.md` § Records covered; NOT recomputed from
/// the algorithm + key, so a re-keyed-but-not-yet-republished selector still
/// renders the value peers actually need.
#[cfg(feature = "multidomain")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DkimSelectorDns {
    pub selector: String,
    pub public_dns_value: String,
}

/// Inputs to assemble one local domain's full per-domain mail-record matrix.
/// Bodies are caller-supplied (SPF/DMARC from `mail_domains` + the per-domain
/// DMARC override logic; the MTA-STS policy body from [`assemble_policy_body`])
/// so this stays a pure, I/O-free builder — the nest gathers the inputs from the
/// DB and frames them here.
#[cfg(feature = "multidomain")]
#[derive(Debug, Clone)]
pub struct DomainDnsInput<'a> {
    pub domain: &'a str,
    pub primary_mx_host: &'a str,
    /// The deployment's primary mail domain — TLSRPT reports route to its
    /// processor inbox for every domain (§ Per-domain TLSRPT).
    pub primary_domain: &'a str,
    pub spf_body: &'a str,
    pub dmarc_body: &'a str,
    pub mta_sts_policy_body: &'a str,
    pub dkim_selectors: &'a [DkimSelectorDns],
}

/// Assemble the full per-domain mail-record matrix for one local domain — the
/// single expected-value source for both publish and verify per
/// `docs/goal/behavior/dns-management.md` § Records covered. Emits, in a stable
/// order: `MX`, `SPF`, one `DKIM` TXT per selector (in input order), `DMARC`,
/// `MTA-STS` TXT, `TLSRPT`, `_caldavs._tcp` `SRV`, `_carddavs._tcp` `SRV`
/// (CalDAV + CardDAV autodiscovery). The nest-host `A`/`AAAA` + advisory `PTR`
/// are host config (not per-domain mail records) and are appended by the nest
/// caller, not here.
#[cfg(feature = "multidomain")]
pub fn build_domain_dns_records(input: &DomainDnsInput<'_>) -> Vec<DnsRecord> {
    let mut records = Vec::with_capacity(7 + input.dkim_selectors.len());
    records.push(build_mx_record(input.domain, input.primary_mx_host));
    records.push(build_spf_record(input.domain, input.spf_body));
    for dk in input.dkim_selectors {
        // Frame the stored TXT body verbatim at `<selector>._domainkey.<domain>`.
        records.push(build_dkim_txt_record_from_value(
            input.domain,
            &dk.selector,
            &dk.public_dns_value,
        ));
    }
    records.push(build_dmarc_txt_record(input.domain, input.dmarc_body));
    records.push(build_mta_sts_txt_record(
        input.domain,
        input.mta_sts_policy_body,
    ));
    records.push(build_tlsrpt_txt_record(input.domain, input.primary_domain));
    // CalDAV + CardDAV autodiscovery (RFC 6764 § 4) — every local domain's DAV
    // surfaces are served at the single mail host `mail.<primary>`
    // (= `primary_mx_host`) via SNI, so both SRV targets match the MX target. The
    // `_carddavs` companion is a standing matrix row exactly like `_caldavs`
    // (both unconditional — the per-deployment enable toggle gates the live
    // service/well-known, not the offered record; manual mode, admin publishes).
    records.push(build_caldavs_srv_record(
        input.domain,
        input.primary_mx_host,
    ));
    records.push(build_carddavs_srv_record(
        input.domain,
        input.primary_mx_host,
    ));
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "multidomain")]
    use crate::outbound::mta_sts::{MtaStsMode, MtaStsPolicy, assemble_policy_body};

    #[test]
    fn default_spf_body_is_softfail() {
        assert_eq!(DEFAULT_SPF_BODY, "v=spf1 mx ~all");
    }

    #[test]
    fn mail_host_is_mail_subdomain_of_primary() {
        assert_eq!(mail_host("example.com"), "mail.example.com");
        assert_eq!(mail_host("dev.fauna.social"), "mail.dev.fauna.social");
    }

    #[test]
    fn dkim_record_from_stored_value_frames_verbatim() {
        let r = build_dkim_txt_record_from_value(
            "example.com",
            "default",
            "v=DKIM1; k=rsa; p=MIIBstored",
        );
        assert_eq!(r.record_type, DnsRecordType::Txt);
        assert_eq!(r.name, "default._domainkey.example.com");
        assert_eq!(r.body, "\"v=DKIM1; k=rsa; p=MIIBstored\"");
        assert_eq!(r.ttl_seconds, 3600);
    }

    #[test]
    fn mx_record_uses_hardcoded_priority_10() {
        let r = build_mx_record("example.com", "mail.example.com");
        assert_eq!(r.record_type, DnsRecordType::Mx);
        assert_eq!(r.name, "example.com");
        // FQDN target (trailing dot): a relative target is doubled by providers
        // that append the zone origin (e.g. Hetzner → mail.example.com.example.com).
        assert_eq!(r.body, "10 mail.example.com.");
        assert_eq!(r.ttl_seconds, 3600);
    }

    #[test]
    fn mx_target_already_fqdn_is_not_double_dotted() {
        let r = build_mx_record("example.com", "mail.example.com.");
        assert_eq!(r.body, "10 mail.example.com.");
    }

    #[test]
    fn spf_record_quotes_body() {
        let r = build_spf_record("example.com", "v=spf1 mx ~all");
        assert_eq!(r.record_type, DnsRecordType::Txt);
        assert_eq!(r.name, "example.com");
        assert_eq!(r.body, "\"v=spf1 mx ~all\"");
    }

    #[cfg(feature = "multidomain")]
    #[test]
    fn dkim_record_rsa() {
        let r = build_dkim_txt_record("example.com", "default", SigningAlg::RsaSha256, "MIIBpub");
        assert_eq!(r.name, "default._domainkey.example.com");
        assert_eq!(r.body, "\"v=DKIM1; k=rsa; p=MIIBpub\"");
    }

    #[cfg(feature = "multidomain")]
    #[test]
    fn dkim_record_ed25519() {
        let r = build_dkim_txt_record("mail.example.com", "default", SigningAlg::Ed25519, "abc123");
        assert_eq!(r.name, "default._domainkey.mail.example.com");
        assert_eq!(r.body, "\"v=DKIM1; k=ed25519; p=abc123\"");
    }

    #[test]
    fn dmarc_record_frames_supplied_body() {
        let r = build_dmarc_txt_record(
            "example.com",
            "v=DMARC1; p=reject; sp=reject; rua=mailto:dmarc-report@example.com",
        );
        assert_eq!(r.name, "_dmarc.example.com");
        assert_eq!(
            r.body,
            "\"v=DMARC1; p=reject; sp=reject; rua=mailto:dmarc-report@example.com\""
        );
    }

    #[cfg(feature = "multidomain")]
    #[test]
    fn mta_sts_record_embeds_policy_hash() {
        let policy = MtaStsPolicy {
            version: "STSv1".to_string(),
            mode: MtaStsMode::Enforce,
            mx: vec!["mail.example.com".to_string()],
            max_age_secs: 604_800,
        };
        let body = assemble_policy_body(&policy);
        let expected_id = compute_policy_version_hash(&body);
        let r = build_mta_sts_txt_record("example.com", &body);
        assert_eq!(r.name, "_mta-sts.example.com");
        assert_eq!(r.body, format!("\"v=STSv1; id={expected_id}\""));
        // The id is a 64-char SHA-256 hex digest.
        assert_eq!(expected_id.len(), 64);
    }

    #[test]
    fn acme_challenge_record_frames_dns_value_at_short_ttl() {
        let r = build_acme_challenge_txt_record("example.com", "abc123-keyauth-digest");
        assert_eq!(r.record_type, DnsRecordType::Txt);
        assert_eq!(r.name, "_acme-challenge.example.com");
        assert_eq!(r.body, "\"abc123-keyauth-digest\"");
        // Transient → short TTL, NOT the 3600s steady-state mail-record default.
        assert_eq!(r.ttl_seconds, ACME_CHALLENGE_TTL_SECS);
        assert_eq!(r.ttl_seconds, 120);
        assert_ne!(r.ttl_seconds, DEFAULT_TTL_SECS);
    }

    #[test]
    fn acme_challenge_record_handles_subdomain() {
        let r = build_acme_challenge_txt_record("mail.example.com", "v");
        assert_eq!(r.name, "_acme-challenge.mail.example.com");
    }

    #[test]
    fn tlsrpt_record_points_at_primary_processor() {
        let r = build_tlsrpt_txt_record("additional.example", "example.com");
        assert_eq!(r.name, "_smtp._tls.additional.example");
        assert_eq!(r.body, "\"v=TLSRPTv1; rua=mailto:tlsrpt@example.com\"");
    }

    #[test]
    fn caldavs_srv_record_advertises_caldav_host_on_443() {
        let r = build_caldavs_srv_record("example.com", "mail.example.com");
        assert_eq!(r.record_type, DnsRecordType::Srv);
        assert_eq!(r.name, "_caldavs._tcp.example.com");
        // priority 0, weight 1, port 443, FQDN target (trailing dot like MX).
        assert_eq!(r.body, "0 1 443 mail.example.com.");
        assert_eq!(r.ttl_seconds, DEFAULT_TTL_SECS);
    }

    #[test]
    fn caldavs_srv_target_already_fqdn_is_not_double_dotted() {
        let r = build_caldavs_srv_record("example.com", "mail.example.com.");
        assert_eq!(r.body, "0 1 443 mail.example.com.");
    }

    #[test]
    fn caldavs_srv_secondary_domain_points_at_shared_mail_host() {
        // A secondary domain's CalDAV is served at the SAME mail host
        // (`mail.<primary>`), not `mail.<secondary>` — single MDA via SNI.
        let r = build_caldavs_srv_record("additional.example", "mail.example.com");
        assert_eq!(r.name, "_caldavs._tcp.additional.example");
        assert_eq!(r.body, "0 1 443 mail.example.com.");
    }

    #[test]
    fn carddavs_srv_record_mirrors_caldavs_on_443() {
        // The CardDAV autodiscovery SRV is byte-identical to the CalDAV one save
        // the `_carddavs` service label — same host, same 443, same SNI target
        // (`caldav-server.md` § Network exposure, the 2026-07-06 CardDAV-served
        // flip). Both DAV surfaces live on the one `mail.<domain>:443` listener.
        let card = build_carddavs_srv_record("example.com", "mail.example.com");
        assert_eq!(card.record_type, DnsRecordType::Srv);
        assert_eq!(card.name, "_carddavs._tcp.example.com");
        assert_eq!(card.body, "0 1 443 mail.example.com.");
        assert_eq!(card.ttl_seconds, DEFAULT_TTL_SECS);

        let cal = build_caldavs_srv_record("example.com", "mail.example.com");
        assert_eq!(card.body, cal.body, "carddavs SRV body must mirror caldavs");
        assert_eq!(CARDDAV_SRV_PORT, CALDAV_SRV_PORT);
    }

    #[test]
    fn carddavs_srv_target_already_fqdn_is_not_double_dotted() {
        let r = build_carddavs_srv_record("example.com", "mail.example.com.");
        assert_eq!(r.body, "0 1 443 mail.example.com.");
    }

    #[test]
    fn carddavs_srv_secondary_domain_points_at_shared_mail_host() {
        let r = build_carddavs_srv_record("additional.example", "mail.example.com");
        assert_eq!(r.name, "_carddavs._tcp.additional.example");
        assert_eq!(r.body, "0 1 443 mail.example.com.");
    }

    #[cfg(feature = "multidomain")]
    #[test]
    fn domain_matrix_assembles_all_records_in_stable_order() {
        let dkim = vec![
            DkimSelectorDns {
                selector: "ed25519".into(),
                public_dns_value: "v=DKIM1; k=ed25519; p=abc".into(),
            },
            DkimSelectorDns {
                selector: "default".into(),
                public_dns_value: "v=DKIM1; k=rsa; p=MIIBxyz".into(),
            },
        ];
        let policy = MtaStsPolicy {
            version: "STSv1".to_string(),
            mode: MtaStsMode::Enforce,
            mx: vec!["mail.example.com".to_string()],
            max_age_secs: 604_800,
        };
        let policy_body = assemble_policy_body(&policy);
        let input = DomainDnsInput {
            domain: "example.com",
            primary_mx_host: "mail.example.com",
            primary_domain: "example.com",
            spf_body: "v=spf1 mx ~all",
            dmarc_body: "v=DMARC1; p=reject; sp=reject; rua=mailto:dmarc-report@example.com",
            mta_sts_policy_body: &policy_body,
            dkim_selectors: &dkim,
        };
        let records = build_domain_dns_records(&input);

        // Stable order: MX, SPF, DKIM(in input order), DMARC, MTA-STS, TLSRPT,
        // CalDAV SRV, CardDAV SRV.
        assert_eq!(records.len(), 9);
        assert_eq!(
            records[0],
            build_mx_record("example.com", "mail.example.com")
        );
        assert_eq!(records[1].record_type, DnsRecordType::Txt);
        assert_eq!(records[1].name, "example.com");
        assert_eq!(records[1].body, "\"v=spf1 mx ~all\"");
        // DKIM rows: stored public_dns_value framed verbatim, NOT recomputed.
        assert_eq!(records[2].name, "ed25519._domainkey.example.com");
        assert_eq!(records[2].body, "\"v=DKIM1; k=ed25519; p=abc\"");
        assert_eq!(records[3].name, "default._domainkey.example.com");
        assert_eq!(records[3].body, "\"v=DKIM1; k=rsa; p=MIIBxyz\"");
        assert_eq!(records[4].name, "_dmarc.example.com");
        assert_eq!(records[5].name, "_mta-sts.example.com");
        assert_eq!(records[6].name, "_smtp._tls.example.com");
        assert_eq!(
            records[6].body,
            "\"v=TLSRPTv1; rua=mailto:tlsrpt@example.com\""
        );
        // CalDAV autodiscovery SRV, targeting the shared mail host on 443.
        assert_eq!(records[7].record_type, DnsRecordType::Srv);
        assert_eq!(records[7].name, "_caldavs._tcp.example.com");
        assert_eq!(records[7].body, "0 1 443 mail.example.com.");
        // CardDAV autodiscovery SRV, sibling to CalDAV's on the same host/port.
        assert_eq!(records[8].record_type, DnsRecordType::Srv);
        assert_eq!(records[8].name, "_carddavs._tcp.example.com");
        assert_eq!(records[8].body, "0 1 443 mail.example.com.");
        // Every record carries the fixed deployment TTL.
        assert!(records.iter().all(|r| r.ttl_seconds == DEFAULT_TTL_SECS));
    }

    #[cfg(feature = "multidomain")]
    #[test]
    fn domain_matrix_without_dkim_selectors_omits_dkim_rows() {
        let policy = MtaStsPolicy {
            version: "STSv1".to_string(),
            mode: MtaStsMode::Enforce,
            mx: vec!["mail.example.com".to_string()],
            max_age_secs: 604_800,
        };
        let policy_body = assemble_policy_body(&policy);
        let input = DomainDnsInput {
            domain: "secondary.example",
            primary_mx_host: "mail.example.com",
            primary_domain: "example.com",
            spf_body: "v=spf1 mx ~all",
            dmarc_body: "v=DMARC1; p=reject; sp=reject; rua=mailto:dmarc-report@example.com",
            mta_sts_policy_body: &policy_body,
            dkim_selectors: &[],
        };
        let records = build_domain_dns_records(&input);

        // MX, SPF, DMARC, MTA-STS, TLSRPT, CalDAV SRV, CardDAV SRV — no DKIM row.
        assert_eq!(records.len(), 7);
        assert!(records.iter().all(|r| !r.name.contains("_domainkey")));
        // TLSRPT points at the primary processor even for a secondary domain.
        assert_eq!(records[4].name, "_smtp._tls.secondary.example");
        assert_eq!(
            records[4].body,
            "\"v=TLSRPTv1; rua=mailto:tlsrpt@example.com\""
        );
        // CalDAV + CardDAV SRVs target the shared mail host even for a secondary domain.
        assert_eq!(records[5].record_type, DnsRecordType::Srv);
        assert_eq!(records[5].name, "_caldavs._tcp.secondary.example");
        assert_eq!(records[5].body, "0 1 443 mail.example.com.");
        assert_eq!(records[6].record_type, DnsRecordType::Srv);
        assert_eq!(records[6].name, "_carddavs._tcp.secondary.example");
        assert_eq!(records[6].body, "0 1 443 mail.example.com.");
    }
}
