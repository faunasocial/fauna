//! Wire types for the unified DNS-management surface (`fauna.dns.*`), per
//! `docs/goal/behavior/dns-management.md`; implementation design tracked
//! internally.
//!
//! Phase 0 ships the read surface (`fauna.dns.list_records`): per active local
//! domain, every DNS record the deployment needs and the exact value Fauna
//! would publish — the single source both publish and verify consume. The
//! live red/green verdicts come from a separate `fauna.dns.verify_records`
//! reply (Slice 2), and the credential + managed-publish kinds land in later
//! slices. All `fauna.dns.*` kinds are Admin-only.

use crate::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Request for `fauna.dns.list_records`. `domain: None` → every active local
/// domain; `Some(d)` → just that one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRecordsRequest {
    pub domain: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One DNS record the deployment needs, carrying the value Fauna would publish.
/// `record_type` is the RFC type label (`"MX"`, `"TXT"`, `"A"`, `"AAAA"`,
/// `"PTR"`). `expected` is the RFC zone-file RDATA representation (TXT bodies are
/// quoted; MX is `"<priority> <host>"`; A/AAAA are the bare IP literal; PTR is the
/// target FQDN) — byte-identical to what Fauna publishes, so the verify surface
/// can compare observed-vs-expected exactly. Verification status is NOT on this
/// type: `list_records` reports only what *should* exist;
/// `fauna.dns.verify_records` reports what *does* (keyed by `name`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsRecordView {
    pub name: String,
    pub record_type: String,
    pub expected: String,
    pub ttl_seconds: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// All DNS a single domain needs, plus its management mode. `mode` is `"manual"`
/// until Fauna-managed auto-publish lands (Slice 3/4) — the persisted
/// DNS-provider credential store that enables `"managed"` does not exist yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainDns {
    pub domain: String,
    pub mode: String,
    /// Whether this is the deployment's **primary** mail domain — the one whose
    /// matrix carries the host-level records every domain shares: the
    /// `mail.<primary>` A/AAAA host rows and the cert-coupled floor-MX DANE
    /// `_25._tcp.mail.<primary>` TLSA (`tls-certificates.md` § D). The managed
    /// reconcile keys the TLSA withdraw-on-trusted pass on this flag — only the
    /// primary's `publish` may touch the shared TLSA slot, and it must know it is
    /// primary even when the TLSA row is *absent* (a trusted cert withdrew it),
    /// which neither the cert-gated TLSA row nor the address-gated host A row can
    /// signal on their own.
    pub is_primary: bool,
    pub records: Vec<DnsRecordView>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.dns.list_records`: the per-domain record matrix the client
/// renders. Empty on a fresh nest with no mail domains.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRecordsReply {
    pub domains: Vec<DomainDns>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.dns.verify_records`. Same `domain` filter as
/// `list_records` (`None` → every active local domain) so the client can verify
/// exactly the matrix it just rendered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyRecordsRequest {
    pub domain: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The live public-DNS verdict for one expected record. Carries no `expected`
/// value — it pairs with the `DnsRecordView` of the same `(name, record_type)`
/// from `fauna.dns.list_records`, which the client merges to render
/// "expected X, found Y". `(name, record_type)` is the merge key: a domain's
/// `MX` and SPF `TXT` share the bare-domain `name` and are disambiguated by
/// `record_type`. `observed` is what public DNS actually serves (empty when
/// `status == "missing"`). `status` ∈ `ok | missing | mismatch | checking`
/// (`checking` = the lookup is in-flight / rate-limited / errored transiently —
/// not yet a verdict).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsRecordStatus {
    pub name: String,
    pub record_type: String,
    pub observed: Vec<String>,
    pub status: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One domain's per-record verdicts. Mirrors `DomainDns` (same `domain` +
/// per-record split) so the verify reply and the record-matrix reply share one
/// per-domain envelope shape; the client zips `DomainDns.records` with the
/// matching `DomainVerifyStatus.records` by `(name, record_type)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainVerifyStatus {
    pub domain: String,
    pub records: Vec<DnsRecordStatus>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.dns.verify_records`: per-domain live red/green verdicts the
/// client overlays on the `list_records` matrix. Empty on a fresh nest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyRecordsReply {
    pub domains: Vec<DomainVerifyStatus>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.dns.probe_txt_visible` (Admin) — "is this exact TXT value
/// really being **served** by every authoritative NS of `zone_name` right now?"
///
/// The DNS-01 propagation gate's readiness question, asked one RPC hop away.
/// The 5 native apps and the tui ask it in-process (`fauna_client_dns`'s
/// `AuthoritativeNsProbe`); the **web** app cannot — a browser has no raw DNS —
/// so it asks the nest, which runs the identical authoritative-direct query
/// (`fauna_core::authoritative_dns`). Same mechanism, two transports: web is not
/// handed a weaker recursive/DoH approximation, whose negative caching is exactly
/// what the authoritative-direct query exists to avoid
/// (`tls-certificates.md` § B tier 2).
///
/// **Not** the recursive `fauna.dns.verify_records` path: that one answers "what
/// does public DNS serve for the record matrix", cached and recursive, which
/// cannot decide a freshly-published challenge.
///
/// Carries no credential and mutates nothing — the answer is public DNS state.
/// Admin-gated only because a deployment's cert plumbing is an admin concern,
/// matching `verify_records` / `cert_status` on the same surface.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeTxtVisibleRequest {
    /// The zone whose authoritative NS set is asked (the *publish* zone — for a
    /// CNAME-delegated renewal that is the delegation target's zone, not the
    /// SAN's own).
    pub zone_name: String,
    /// The record's FQDN-without-trailing-dot (e.g.
    /// `_acme-challenge.example.com`).
    pub record_name: String,
    /// The exact TXT value that must be served. Compared against the
    /// concatenated character-strings of each answer.
    pub txt_value: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.dns.probe_txt_visible`: `visible` is `true` only when
/// **every** authoritative NS of the zone serves the exact value.
///
/// A `false` is deliberately indistinguishable from "not yet" — NS discovery
/// failure, timeout, mismatch, and genuine absence all read the same, because
/// the caller's propagation gate treats every one of them identically (keep
/// polling until its deadline, then proceed best-effort). That also makes an
/// *older* nest — one that does not know this kind at all, so the call errors —
/// degrade to the same "never confirmed → best-effort at the deadline" behavior
/// the fixed wait had, with no version branch anywhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeTxtVisibleReply {
    pub visible: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.dns.set_host_address` (Admin) — the onboarding hand-off
/// that persists the deployment's own public address nest-side so the host
/// `A`/`AAAA`/`PTR` rows can be assembled + verified. The address is **public,
/// not a secret** (unlike DNS-provider credentials, which stay client-held), so
/// it is ordinary nest state.
///
/// Two address roles, because **`mail.<primary>` (the MX target) may resolve to
/// a different IP than the apex `<primary>`** (the mail server can be a separate
/// box from the nest): `nest_*` → the apex `A`/`AAAA` (WS-RPC / web endpoint),
/// `mail_*` → `mail.<primary>` `A`/`AAAA` + the advisory `PTR` target. In a
/// single-box deployment the caller sends the same IP for both. `*_ipv6` is
/// `None` until the deployment has an IPv6 address. Names are derived nest-side
/// from the primary mail domain; only the IPs are carried here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetHostAddressRequest {
    pub nest_ipv4: String,
    pub nest_ipv6: Option<String>,
    pub mail_ipv4: String,
    pub mail_ipv6: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.dns.set_host_address`: an empty ack (the persisted address
/// surfaces through the next `fauna.dns.list_records` / `verify_records`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetHostAddressReply {}
