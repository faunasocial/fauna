//! Wire types for the nest TLS-certificate lifecycle surface (`fauna.tls.*`),
//! per `docs/goal/architecture/nest/tls-certificates.md` § B.
//!
//! Phase 3 ships `fauna.tls.publish_cert`: the admin's client, having obtained a
//! publicly-trusted cert via a client-driven DNS-01 ACME order (no nest ever
//! holds the DNS-provider key — `dns-management.md` § Where the credential
//! lives), seals it with `fauna_mls::wrapped_blob::seal_lan_tls_cert_entry` and
//! delivers the sealed `(ciphertext, actor_sig)` here. The nest writes it into
//! the caller's own actor `namespace_entries` under the well-known
//! `LAN_TLS_CERT_ENTRY_ID`; a paired private/NAT nest then pulls it over
//! namespace-sync and installs it (`apply_synced_lan_cert`). The nest stores the
//! blob opaquely — it is sealed to the *private* nest's identity key, so the
//! receiving relay can neither read nor forge it; the consumer verifies the
//! actor signature on apply. Admin-only (the deployment's cert is an admin
//! concern).

use crate::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::ByteBuf;

/// Request for `fauna.tls.publish_cert`. Carries the sealed LAN-TLS cert entry
/// produced by `fauna_mls::wrapped_blob::seal_lan_tls_cert_entry`: `ciphertext`
/// is the HPKE-sealed `TlsCertBlob` (sealed to the private nest's identity
/// x25519 key), `actor_sig` is the issuing actor's Ed25519 signature over it.
/// The target namespace is the authenticated caller's own actor — there is no
/// `actor_id` field, so the handler keys on the connection actor and no actor
/// can write into another's namespace; the entry id is the fixed
/// `LAN_TLS_CERT_ENTRY_ID`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishCertRequest {
    pub ciphertext: ByteBuf,
    pub actor_sig: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.tls.publish_cert`. `ok` is `true` once the sealed entry is
/// durably stored in the caller's namespace (last-writer-wins by sequence); a
/// paired private nest installs it on its next namespace-sync pull.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishCertReply {
    pub ok: bool,
    /// Whether the receiving nest **installed the cert on itself**, because the
    /// blob was sealed to its own identity key (the standalone deployment — one
    /// nest, no pair). `false` means it stored the entry opaquely for a
    /// paired nest to pull (the relay case, or a blob sealed elsewhere).
    pub installed: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Health of the cert the nest's listener **currently serves** for one domain —
/// the `admin-dns` cert-status row (`tls-certificates.md` § C.4). Exactly the
/// three states the goal doc names; the nest computes this server-side from the
/// served leaf (it is the authority on what it serves and on `now`), so every
/// app renders the same badge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertHealthState {
    /// A CA-issued (trusted) cert is served and not near expiry.
    ValidTrusted,
    /// The self-signed floor is served (no trusted cert, or the trusted cert
    /// expired / does not cover the domain) — renewal is needed. The fresh-nest
    /// default until a trusted cert is issued, so it is the `Default`.
    #[default]
    OnFloorRenewNeeded,
    /// A trusted cert is served but within the renewal lead window (< 30 days to
    /// `notAfter`) — renew soon to avoid dropping to the floor.
    Expiring,
    /// A state a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// collapsing). It renders as needing attention, never as trusted. Never
    /// serialized: a path that would re-emit it fails instead of replacing the
    /// newer value.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// Served-cert status for one domain, in a `fauna.tls.cert_status` reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainCertStatus {
    /// The domain queried.
    pub domain: String,
    /// The three-state health badge.
    pub state: CertHealthState,
    /// `notAfter` of the served leaf, unix seconds; `0` when no cert is served
    /// (resolver pending / no TLS) — the UI shows no expiry then.
    pub not_after_unix: i64,
    /// True when the served leaf is the self-signed floor (drives an
    /// "untrusted / self-signed" sub-label distinct from a near-expiry trusted
    /// cert).
    pub is_floor: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.tls.cert_status` (Admin-only). The client passes the
/// domains its `admin-dns` page shows; the nest reports the served-cert status
/// for each (the apex is just one of them). An empty list yields an empty reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertStatusRequest {
    pub domains: Vec<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.tls.cert_status`: one [`DomainCertStatus`] per requested
/// domain, in request order, plus the deployment's desired SAN set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertStatusReply {
    pub statuses: Vec<DomainCertStatus>,
    /// Every DNS name the nest's **listener certificate** should cover — the
    /// apex, the single MX host `mail.<primary>`, each active mail domain's apex,
    /// and any enabled infra host (`relay.`/`pds.`). A client-driven DNS-01 order
    /// requests exactly this set (intersected with the zones its credential
    /// covers), so a client-issued cert covers what the nest actually serves
    /// instead of the clicked domain alone (`tls-certificates.md` § B tier 2).
    ///
    /// Deployment-level, not per-domain: an installed client-issued cert becomes
    /// the listener's default (apex) cert whichever domain row was clicked.
    ///
    /// Unlike the nest's own HTTP-01 order this set is **not** resolve-gated —
    /// DNS-01 validates by publishing a TXT in the zone, so a name that has no
    /// `A` record yet still validates fine, and gating it would needlessly
    /// narrow the cert.
    pub desired_sans: Vec<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}
