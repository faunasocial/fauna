//! Automatic TLS certificate management via ACME (Let's Encrypt).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::{ArcSwap, ArcSwapOption};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

pub use fauna_acme_http01::{CERT_FILENAME, KEY_FILENAME};

/// Where the previous *real* (CA-issued, e.g. Let's Encrypt) cert is preserved
/// when an admin-synthesized **self-signed** cert is about to overwrite the
/// live PEM. This is the "store the LE cert and re-use it on switch-back"
/// mechanism: `write_acme_pem_atomic` copies the real cert here before a
/// self-signed write, and `fauna.bridges.restore_real_tls_cert` copies it back.
/// See `docs/goal/behavior/mail-bridge-lifecycle.md` § TLS provisioning.
pub const REAL_CERT_BACKUP_FILENAME: &str = "fullchain.real-backup.pem";
/// Private-key companion to [`REAL_CERT_BACKUP_FILENAME`].
pub const REAL_KEY_BACKUP_FILENAME: &str = "privkey.real-backup.pem";

/// Best-effort "is this leaf certificate self-signed?" — true when the leaf's
/// issuer DN equals its subject DN. Used to decide whether a PEM about to be
/// overwritten is a *real* CA-issued cert worth preserving (issuer != subject)
/// versus an already-self-signed one (don't clobber the real backup with it).
/// A parse failure returns `false` (treat as real → preserve), erring toward
/// keeping a backup rather than silently losing a recoverable cert.
pub fn pem_is_self_signed(cert_pem: &[u8]) -> bool {
    // First PEM block is the leaf.
    let Some(pem) = x509_parser::pem::Pem::iter_from_buffer(cert_pem).next() else {
        return false;
    };
    let Ok(pem) = pem else {
        return false;
    };
    match pem.parse_x509() {
        Ok(cert) => cert.tbs_certificate.issuer == cert.tbs_certificate.subject,
        Err(_) => false,
    }
}

/// SHA-256 of the `SubjectPublicKeyInfo` of a DER-encoded X.509 cert — the
/// **public-key** fingerprint used by the TLS channel-binding leg
/// (`docs/goal/architecture/security.md` § Transport trust, Axis 1).
///
/// Re-exported from `fauna_protocol::tls_spki` (feature `tls-spki`): the client
/// recomputes the exact same fingerprint from the cert *it* received during the
/// rustls handshake, so both ends of the binding MUST share one implementation
/// (priority #4) — that shared home is `fauna-protocol`, next to the
/// `CertBinding` wire type that carries the value.
pub use fauna_protocol::tls_spki::spki_sha256_of_cert_der;

/// Renewal lead window: begin/expect cert renewal once fewer than this many
/// seconds remain before `notAfter` (**30 days**, matching the built HTTP-01
/// renewal lead in [`acme_http01`](crate::acme_http01)). Single source of truth
/// for every cert-lifecycle policy that keys on "near expiry": the HTTP-01
/// renewal loop, the `fauna.tls.cert_status` `expiring` state, the at-risk
/// renewal push, and managed-mode auto-issue (`tls-certificates.md` § C.2).
pub const CERT_RENEWAL_LEAD_SECS: i64 = 30 * 24 * 60 * 60;

/// Raw facts about the cert the nest's listener would serve for a given SNI —
/// the nest-side truth behind the `admin-dns` cert-status row
/// (`tls-certificates.md` § C.4). Deliberately **policy-free** (no clock, no
/// thresholds): the `fauna.tls.cert_status` handler folds these plus `now` and
/// [`CERT_RENEWAL_LEAD_SECS`] into the wire `valid-trusted` /
/// `on-floor — renew needed` / `expiring` state, so the resolver stays a pure
/// fact source and the lead-window policy lives in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServedCertFacts {
    /// `notBefore` of the served leaf, unix seconds.
    pub not_before_unix: i64,
    /// `notAfter` of the served leaf, unix seconds.
    pub not_after_unix: i64,
    /// True when the served leaf is self-signed (issuer DN == subject DN) — the
    /// floor (the Phase-2 minted floor or the self-signed bootstrap), as opposed
    /// to a CA-issued trusted cert. Same test as [`pem_is_self_signed`].
    pub is_floor: bool,
    /// True when a DNS SAN on the served leaf covers the requested name.
    pub covers: bool,
}

/// Parse a served `CertifiedKey`'s leaf into [`ServedCertFacts`] for `sni`.
/// `None` if the leaf is absent or unparseable (mirrors the fail-closed parse
/// in [`cert_valid_and_covers`]). Shared by both [`ServedCertSpki`] impls.
fn cert_facts(cert: &CertifiedKey, sni: &str) -> Option<ServedCertFacts> {
    let leaf = cert.end_entity_cert().ok()?;
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref()).ok()?;
    let validity = parsed.validity();
    let is_floor = parsed.tbs_certificate.issuer == parsed.tbs_certificate.subject;
    let covers = matches!(parsed.subject_alternative_name(), Ok(Some(san)) if san
        .value
        .general_names
        .iter()
        .any(|gn| matches!(gn, x509_parser::extensions::GeneralName::DNSName(dns) if dns_name_matches(dns, sni))));
    Some(ServedCertFacts {
        not_before_unix: validity.not_before.timestamp(),
        not_after_unix: validity.not_after.timestamp(),
        is_floor,
        covers,
    })
}

/// Fold served-cert [`ServedCertFacts`] (or their absence) plus `now` into the
/// wire `admin-dns` cert-status state (`tls-certificates.md` § C.4). The single
/// policy point: `OnFloorRenewNeeded` when no trusted covering cert is served
/// (floor, no cert, or a non-covering fallback), `Expiring` when a trusted cert
/// is within [`CERT_RENEWAL_LEAD_SECS`] of `notAfter`, else `ValidTrusted`.
pub fn cert_health_state(
    facts: Option<ServedCertFacts>,
    now_unix: i64,
) -> fauna_protocol::tls::CertHealthState {
    use fauna_protocol::tls::CertHealthState::*;
    match facts {
        None => OnFloorRenewNeeded,
        Some(f) if f.is_floor || !f.covers => OnFloorRenewNeeded,
        Some(f) if now_unix >= f.not_after_unix - CERT_RENEWAL_LEAD_SECS => Expiring,
        Some(_) => ValidTrusted,
    }
}

/// Read surface for the cert the nest's WS-RPC/HTTPS listener is **currently
/// serving**: its SPKI fingerprint (the channel-binding leg) and its validity
/// facts (the `admin-dns` cert-status row). Implemented by the live resolver so
/// readers see the hot-swapped cert without re-reading PEM from disk (the
/// resolver swaps on ACME renewal, so disk and served cert can momentarily
/// disagree).
pub trait ServedCertSpki: Send + Sync {
    /// SHA-256 SPKI fingerprint of the served **default/apex** cert, so the
    /// `fauna.auth.handshake` handler can sign the live SPKI. `None` while the
    /// resolver is pending (no cert issued yet).
    fn current_spki_sha256(&self) -> Option<[u8; 32]>;

    /// Validity facts for the cert that would be served for `sni` (per-SNI on
    /// [`MultiDomainCertResolver`]; the single served cert on
    /// [`ReloadableCertResolver`]). `None` while pending or the leaf won't
    /// parse. The `fauna.tls.cert_status` handler maps this into the wire state.
    fn served_cert_facts(&self, sni: &str) -> Option<ServedCertFacts>;

    /// SHA-256 SPKI fingerprint of the cert this resolver would actually serve
    /// for `sni` — the per-SNI served leaf on [`MultiDomainCertResolver`]
    /// (a valid covering per-domain cert, else the per-SNI floor, else the apex
    /// default), the single served cert on [`ReloadableCertResolver`]. `None`
    /// while pending or the leaf won't parse. The DANE-TLSA coupling pins this
    /// for `mail.<primary>` while that MX is on the floor: the served floor leaf
    /// reuses the *stable* floor key, so the pin is renewal-stable and matches
    /// exactly what a DANE sender sees on the wire (`tls-certificates.md` § D).
    fn served_cert_spki_sha256(&self, sni: &str) -> Option<[u8; 32]>;
}

impl ServedCertSpki for ReloadableCertResolver {
    fn current_spki_sha256(&self) -> Option<[u8; 32]> {
        let key = self.current()?;
        let leaf = key.end_entity_cert().ok()?;
        spki_sha256_of_cert_der(leaf.as_ref())
    }

    fn served_cert_facts(&self, sni: &str) -> Option<ServedCertFacts> {
        cert_facts(self.current()?.as_ref(), sni)
    }

    fn served_cert_spki_sha256(&self, _sni: &str) -> Option<[u8; 32]> {
        // A single served cert regardless of SNI (no per-SNI selection).
        let key = self.current()?;
        let leaf = key.end_entity_cert().ok()?;
        spki_sha256_of_cert_der(leaf.as_ref())
    }
}

/// ACME configuration.
#[derive(Debug, Clone)]
pub struct AcmeConfig {
    /// Domain to get certificates for.
    pub domain: String,
    /// Directory to store certs and account key.
    pub acme_dir: PathBuf,
    /// Explicit ACME directory URL override (`[acme].directory_url`): the nest's
    /// HTTP-01 client orders against this CA (a private/internal ACME CA, or the
    /// in-network `pebble` of the tier_4 acceptance) instead of Let's Encrypt.
    /// `None` ⇒ Let's Encrypt production — the CA and the (absent) account
    /// contact are constants (`tls-certificates.md` § ACME settings).
    pub directory_url: Option<String>,
}

impl Default for AcmeConfig {
    fn default() -> Self {
        Self {
            domain: String::new(),
            acme_dir: PathBuf::from("/var/lib/fauna/acme"),
            directory_url: None,
        }
    }
}

/// Build an `AcmeConfig` from the parsed TOML and the `--acme-dir` artifact flag.
///
/// The domain is `[nest].domain` (seeded by the harness, then the claim);
/// precedence for the directory (highest first): `--acme-dir`, then TOML
/// `[acme].dir`, then `AcmeConfig::default()`. Returns `(config, enabled)` where `enabled` is the
/// *derived* "does this nest run ACME?" decision — `true` iff the nest is on the
/// public NAT axis AND a real orderable domain is configured (non-empty, not
/// `localhost`). A private nest, or a domainless / `localhost` box, returns
/// `false` (it serves the always-live self-signed floor instead). There is no
/// `[acme].enabled` config field: enablement is derived, never configured (see
/// the trailing comment for the single source of truth).
pub fn build_acme_config(
    nest_config: &crate::config::NestConfig,
    node_mode: crate::config::NodeMode,
    cli_dir: Option<&str>,
) -> (AcmeConfig, bool) {
    let mut acme_config = AcmeConfig::default();
    if let Some(acme_section) = nest_config.acme.as_ref() {
        if let Some(dir) = acme_section.dir.as_deref() {
            acme_config.acme_dir = PathBuf::from(dir);
        }
        if let Some(url) = acme_section.directory_url.as_deref() {
            acme_config.directory_url = Some(url.to_string());
        }
    }
    if let Some(domain) = nest_config.nest.domain.as_deref() {
        acme_config.domain = domain.to_string();
    }
    if let Some(dir) = cli_dir {
        acme_config.acme_dir = PathBuf::from(dir);
    }
    // Whether this nest RUNS ACME is *derived*, not configured (the former
    // `[acme].enabled` seed was removed — it only ever duplicated "a real
    // orderable domain is present on the public NAT axis", which the Docker
    // entrypoint computed as `DOMAIN != localhost`). This is the single source
    // of truth for "should this nest run ACME", on two axes:
    //   • Public NAT axis (`node_mode != Private`) — § Don't do these
    //     (deployment-home-with-public-relay.md): a private/NAT box runs no ACME
    //     client at all (its optional LAN IMAP/CalDAV cert is self-signed or
    //     client-published-DNS-01, shipped over namespace-sync; HTTP-01 can't
    //     validate a non-public-routable LAN address and no nest holds
    //     DNS-provider keys). `node_mode` is the *resolved* (client-set) axis —
    //     the `nest_nat_mode` row when present, else the `config.nest.mode` seed
    //     — NOT `nest_config.nest.mode` directly, so a box a client set private
    //     (public seed) runs no ACME, and one set public (private seed) does
    //     (`nat_mode_core::resolve_node_mode`).
    //   • AND a real orderable domain is configured — non-empty and not the
    //     loopback `localhost`. A domainless or `localhost` box can never
    //     complete a public HTTP-01 order, so it serves the always-live
    //     self-signed floor instead (tls-certificates.md § A) and never orders.
    // `main.rs` ANDs this with `!force_plain_http` (the test/diagnostic
    // plain-HTTP escape) + HTTP-01 mode to gate the challenge LISTENER.
    let acme_enabled = node_mode != crate::config::NodeMode::Private
        && !acme_config.domain.is_empty()
        && acme_config.domain != "localhost";
    (acme_config, acme_enabled)
}

/// The IP bridge cert's chain/key filenames, inside `acme_dir` — kept
/// separate from the domain cert's [`CERT_FILENAME`] so the two lifecycles
/// never clobber each other (`tls-certificates.md` § B-IP). Re-exported from
/// `fauna_acme_http01` rather than redeclared: `obtain_ip_certificate` writes
/// these exact filenames, so a local copy could drift from what it actually
/// writes (the mirror of [`CERT_FILENAME`]/[`KEY_FILENAME`] above).
pub use fauna_acme_http01::{IP_CERT_FILENAME, IP_KEY_FILENAME};
/// File the IP bridge's last **attempted** interface set is persisted to,
/// beside [`IP_CERT_FILENAME`] — what [`ip_bridge_addrs_changed`] compares the
/// live derived set against. Deliberately not the same thing as what the
/// installed chain covers: a v4-only narrowing (§ B-IP *Lifetime*) leaves the
/// chain covering less than this by design, and re-deriving "changed" from
/// coverage against the *live* set re-triggers that gap every tick forever
/// (`tls-certificates.md` § B-IP *Lifetime*).
pub const IP_BRIDGE_ATTEMPTED_FILENAME: &str = "ip-bridge-attempted.json";

/// Every global-unicast address directly attached to one of this box's own
/// interfaces — the input to [`ip_bridge_addresses`], read from the OS interface
/// table. Loopback interfaces are skipped before classification; everything else
/// is left to [`fauna_core::resolve::is_global_ip`], the one classifier
/// (`resolve.rs`) the SSRF guard and the client host-address reporter also use.
///
/// Impure by nature (it reads the machine), so the policy above it is factored
/// into the pure [`ip_bridge_addresses`], which the unit pins drive directly.
pub fn attached_interface_addresses() -> Vec<std::net::IpAddr> {
    match if_addrs::get_if_addrs() {
        Ok(ifaces) => ifaces
            .into_iter()
            .filter(|iface| !iface.is_loopback())
            .map(|iface| iface.ip())
            .collect(),
        Err(e) => {
            tracing::warn!("could not read the interface table for the IP bridge cert: {e}");
            Vec::new()
        }
    }
}

/// The IP bridge cert's **derived** enable (`tls-certificates.md` § B-IP): the
/// addresses this nest should order a publicly-trusted short-lived certificate
/// for, given the resolved NAT axis and the addresses attached to its interfaces.
/// An empty result means "don't order" — there is no `enabled` knob of any kind,
/// exactly as ACME's own enable is derived rather than configured (see
/// [`build_acme_config`]'s trailing comment).
///
/// Two rules, both from § B-IP *When*:
///
/// * **The private-NAT-axis gate still wins.** A nest a client set private runs no
///   ACME client at all, and that is unchanged here — a NAT/LAN box could not
///   validate an IP identifier anyway (the CA must reach `:80` *on that address*).
/// * **Global-unicast only.** Only an address the CA can route to can carry an
///   RFC 8738 `ip` identifier, so the interface table is filtered through
///   [`fauna_core::resolve::is_global_ip`] — which already excludes loopback,
///   RFC 1918, CGNAT, link-local and friends on both families.
///
/// Deliberately **independent of the domain-derived `acme_enabled`**: the whole
/// point is a box that has no domain yet (a fresh client-provisioned VPS, whose
/// A record the browser cannot use and whose name the box will not learn until
/// the claim — `../../behavior/onboarding.md` § 6 *Reaching the box*).
///
/// Duplicates are collapsed and the order is stable (IPv4 before IPv6, then
/// numeric) so the SAN sets, and therefore the CA's duplicate-certificate
/// accounting, do not churn with the interface table's enumeration order.
pub fn ip_bridge_addresses(
    node_mode: crate::config::NodeMode,
    attached: &[std::net::IpAddr],
) -> Vec<std::net::IpAddr> {
    if node_mode == crate::config::NodeMode::Private {
        return Vec::new();
    }
    let mut out: Vec<std::net::IpAddr> = attached
        .iter()
        .copied()
        .filter(|ip| fauna_core::resolve::is_global_ip(*ip))
        .collect();
    out.sort_by_key(|ip| match ip {
        std::net::IpAddr::V4(v4) => (0u8, v4.octets().to_vec()),
        std::net::IpAddr::V6(v6) => (1u8, v6.octets().to_vec()),
    });
    out.dedup();
    out
}

/// Has the IP bridge's derived interface set genuinely changed since the last
/// order decision? `current` is this tick's [`ip_bridge_addresses`] output;
/// `last_attempted` is the set persisted at
/// [`IP_BRIDGE_ATTEMPTED_FILENAME`] the last time a decision was made.
/// Order-insensitive, since a value read back from disk (or handed by a test)
/// is not guaranteed to preserve `ip_bridge_addresses`' sort.
///
/// This — not coverage of the *installed* chain — is the re-order trigger a
/// genuinely new or departed interface address needs: comparing the derived
/// set to what the bridge last **attempted** rather than to what the chain
/// **covers** is what lets a v4-only narrowing (an IPv6 `:80` the CA cannot
/// reach) settle. Coverage against the live set would stay false forever in
/// that case — the chain only ever covers the narrowed subset — and re-order
/// every tick (`tls-certificates.md` § B-IP *Lifetime*).
pub fn ip_bridge_addrs_changed(
    current: &[std::net::IpAddr],
    last_attempted: &[std::net::IpAddr],
) -> bool {
    let mut current = current.to_vec();
    let mut last_attempted = last_attempted.to_vec();
    current.sort();
    last_attempted.sort();
    current != last_attempted
}

/// Fraction of an IP bridge certificate's **total** validity that must remain
/// before it is renewed — one third (`tls-certificates.md` § B-IP *Lifetime*).
/// Against Let's Encrypt's `shortlived` profile (160 h, the only profile that
/// issues IP certs) that is a ~53 h lead, i.e. a renewal roughly every 4 days,
/// which fits the 5-per-week duplicate-certificate limit for one SAN set.
///
/// Expressed as a fraction of the observed validity rather than a constant so the
/// cadence follows whatever the CA actually issued: a private/internal ACME CA
/// with a different profile gets a proportionate lead instead of a lead that
/// might exceed its whole lifetime.
pub const IP_CERT_RENEW_FRACTION: i64 = 3;

/// Does the IP bridge cert covering `not_before_unix..not_after_unix` need
/// renewing at `now_unix`? True once less than [`IP_CERT_RENEW_FRACTION`]⁻¹ of its
/// total validity remains — and true for an already-expired or unparseable-window
/// cert, so a box that missed its renewal window re-orders rather than sitting on
/// a dead cert.
///
/// Pure, so § B-IP's cadence is pinned without a clock or a CA.
pub fn ip_cert_needs_renewal(not_before_unix: i64, not_after_unix: i64, now_unix: i64) -> bool {
    let validity = not_after_unix.saturating_sub(not_before_unix);
    if validity <= 0 {
        // A degenerate or inverted window tells us nothing about a safe lead;
        // treat it as due rather than trust it.
        return true;
    }
    let lead = validity / IP_CERT_RENEW_FRACTION;
    now_unix >= not_after_unix.saturating_sub(lead)
}

/// The `(notBefore, notAfter)` unix seconds of the leaf in `cert_pem`, or `None`
/// if it will not parse. The clock-free input [`ip_cert_needs_renewal`] grades:
/// the bridge's lead is a fraction of its *observed* validity, so the window has
/// to be read off the issued certificate rather than assumed.
pub fn pem_validity_window(cert_pem: &[u8]) -> Option<(i64, i64)> {
    let pem = x509_parser::pem::Pem::iter_from_buffer(cert_pem)
        .next()?
        .ok()?;
    let cert = pem.parse_x509().ok()?;
    let validity = cert.validity();
    Some((
        validity.not_before.timestamp(),
        validity.not_after.timestamp(),
    ))
}

/// Should this nest still be **carrying** an IP bridge cert at all
/// (`tls-certificates.md` § B-IP *Lifetime* — "a bridge, not a permanent
/// identity")? False once the deployment has a live trusted certificate for its
/// primary domain: from then on the IP cert is left to lapse and IP dials fall
/// back to the floor, because the IP was never the deployment's identity
/// (`domains-and-tls-bootstrap.md` § Goal).
///
/// `has_orderable_address` folds in [`ip_bridge_addresses`]' emptiness, so this
/// one predicate answers both "may we" and "should we".
pub fn ip_bridge_should_renew(
    has_orderable_address: bool,
    primary_domain_cert_trusted: bool,
) -> bool {
    has_orderable_address && !primary_domain_cert_trusted
}

/// Load a TLS certificate and private key from PEM files.
/// Returns a rustls ServerConfig ready to use.
pub fn load_tls_config(cert_path: &Path, key_path: &Path) -> Result<Arc<rustls::ServerConfig>> {
    let cert_pem = std::fs::read(cert_path).context("read cert")?;
    let key_pem = std::fs::read(key_path).context("read key")?;

    use rustls::pki_types::pem::{self, PemObject};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certs: Vec<_> = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("parse certs: {e}"))?;

    let key = PrivateKeyDer::from_pem_slice(&key_pem).map_err(|e| match e {
        pem::Error::NoItemsFound => anyhow::anyhow!("no private key found in PEM"),
        e => anyhow::anyhow!("parse key: {e}"),
    })?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build TLS config")?;

    Ok(Arc::new(config))
}

/// Load an already-issued cert from disk into a rustls `ServerConfig`, or bail
/// if none is present yet.
///
/// This is the synchronous startup path. The chain it reads is placed on disk
/// by [`crate::acme_http01::cert_lifecycle_task`] (when HTTP-01 ACME is
/// enabled) or by an admin / reverse proxy otherwise. The ACME challenge
/// flow itself — including multi-SAN issuance for the mail hostnames — lives in
/// [`crate::acme_http01`].
pub fn obtain_or_load_cert(config: &AcmeConfig) -> Result<Arc<rustls::ServerConfig>> {
    let cert_path = config.acme_dir.join(CERT_FILENAME);
    let key_path = config.acme_dir.join(KEY_FILENAME);

    if cert_path.exists() && key_path.exists() {
        tracing::info!("Loading TLS certificate from {}", cert_path.display());
        return load_tls_config(&cert_path, &key_path);
    }

    anyhow::bail!(
        "No TLS certificate found at {}. \
         Place {} and {} there, or use a reverse proxy for TLS.",
        config.acme_dir.display(),
        CERT_FILENAME,
        KEY_FILENAME,
    )
}

// The hot-reloading resolver + PEM->CertifiedKey loaders are shared with the
// fauna.social front door via `fauna-acme-http01` (lifted 2026-08-21;
// front-door.md § TLS policy). The SNI resolver below stays nest-only.
pub use fauna_acme_http01::{
    ReloadableCertResolver, load_certified_key, load_certified_key_from_pem,
};

/// A TLS certificate resolver that supports SNI-based cert lookup for custom
/// web content domains.
///
/// Each custom domain has its own [`CertifiedKey`]. When a TLS handshake
/// arrives the SNI name is looked up in the `domains` map; if no match is
/// found the nest's own default cert is used instead.
///
/// All updates are lock-free: [`add_domain`](Self::add_domain),
/// [`remove_domain`](Self::remove_domain), and
/// [`reload_default`](Self::reload_default) all clone the current state,
/// apply the change, then atomically swap the pointer via `ArcSwap`.
pub struct MultiDomainCertResolver {
    /// Domain-specific certs, keyed by domain name.
    domains: ArcSwap<HashMap<String, Arc<CertifiedKey>>>,
    /// Default cert (nest's own domain) — used when SNI doesn't match any custom
    /// domain. `None` while **pending** (cold-boot-with-ACME before the first
    /// cert lands), exactly like [`ReloadableCertResolver::pending`]: the
    /// listener binds immediately and [`resolve`](Self::resolve) returns `None`
    /// (the handshake fails cleanly) until [`reload_default_from_pem`] swaps the
    /// first apex cert in — no process restart.
    default: ArcSwapOption<CertifiedKey>,
    /// PEM of the stable self-signed **floor** key, used to mint a per-SNI floor
    /// cert when a registered served domain has no *valid* trusted cert (expired
    /// or non-covering). `None` leaves the resolver with no floor — a per-SNI
    /// miss then falls to `default` (the state in unit tests, or before the key
    /// is wired at boot). Held as PEM (not an `rcgen::KeyPair`) so the field is
    /// trivially `Send + Sync`; minting re-parses it once per domain. See
    /// `tls-certificates.md` § A — "a floor cert that carries the requested SNI".
    floor_key_pem: ArcSwapOption<String>,
    /// Lazily-minted per-SNI floor certs (each covers exactly its SNI), signed by
    /// the floor key. Cached so synthesis is once-per-domain, not per-handshake.
    /// Bounded to domains the resolver *already* recognizes (a floor is minted
    /// only when the SNI has a registered — if invalid — trusted cert), so an
    /// arbitrary-SNI flood can't grow it without bound.
    floor_certs: ArcSwap<HashMap<String, Arc<CertifiedKey>>>,
    /// The **IP bridge cert** (`tls-certificates.md` § B-IP): a publicly-trusted
    /// short-lived cert for this box's own public IP, served to **no-SNI** dials
    /// while it is valid so a *browser* can reach a domainless box at all. `None`
    /// on every box that has no directly-attached global-unicast address, and on
    /// every box whose domain cert has gone live (the bridge is then left to
    /// lapse). Held separately from `default` rather than replacing it, so the
    /// channel-binding SPKI keeps following the default cert exactly as it always
    /// has (§ B-IP *Serving*) and the loopback floor stays beneath it
    /// unconditionally (`domains-and-tls-bootstrap.md` § Boot).
    ip_cert: ArcSwapOption<CertifiedKey>,
}

impl MultiDomainCertResolver {
    /// Create a new resolver with the given default cert already loaded.
    pub fn new(default_cert: CertifiedKey) -> Self {
        Self {
            domains: ArcSwap::from_pointee(HashMap::new()),
            default: ArcSwapOption::from_pointee(default_cert),
            floor_key_pem: ArcSwapOption::empty(),
            floor_certs: ArcSwap::from_pointee(HashMap::new()),
            ip_cert: ArcSwapOption::empty(),
        }
    }

    /// Create a resolver whose default cert is loaded from PEM — the
    /// boot-with-cert path (mirrors [`ReloadableCertResolver::from_pem`]).
    pub fn from_pem(cert_path: &Path, key_path: &Path) -> Result<Self> {
        Ok(Self::new(load_certified_key(cert_path, key_path)?))
    }

    /// Create a resolver with **no default cert yet** — the cold-boot-no-cert
    /// path. [`resolve`](Self::resolve) returns `None` (handshakes fail) until
    /// [`reload_default_from_pem`](Self::reload_default_from_pem) loads the first
    /// apex cert. Per-domain certs added via [`add_domain`](Self::add_domain)
    /// while pending still serve their own SNI.
    pub fn pending() -> Self {
        Self {
            domains: ArcSwap::from_pointee(HashMap::new()),
            default: ArcSwapOption::empty(),
            floor_key_pem: ArcSwapOption::empty(),
            floor_certs: ArcSwap::from_pointee(HashMap::new()),
            ip_cert: ArcSwapOption::empty(),
        }
    }

    /// Install the stable self-signed **floor** key (PEM) used to mint per-SNI
    /// floor certs (see [`cert_for_sni`](Self::cert_for_sni)). Wired at boot from
    /// the persisted `floor-privkey.pem`. Idempotent; replacing the key clears the
    /// minted-floor cache so later fallbacks use the new key.
    pub fn set_floor_key(&self, key_pem: String) {
        self.floor_key_pem.store(Some(Arc::new(key_pem)));
        self.floor_certs.store(Arc::new(HashMap::new()));
    }

    /// The currently-loaded default cert, or `None` if still pending.
    /// Primarily for tests and [`ServedCertSpki`].
    pub fn current_default(&self) -> Option<Arc<CertifiedKey>> {
        self.default.load_full()
    }

    /// Hot-reload the **default** (apex) cert from PEM on disk. The swap is
    /// atomic; on a pending resolver this is the moment it starts serving HTTPS
    /// for the nest's own hostname. Called by [`cert_watcher_task`].
    pub fn reload_default_from_pem(&self, cert_path: &Path, key_path: &Path) -> Result<()> {
        let key = load_certified_key(cert_path, key_path)?;
        self.default.store(Some(Arc::new(key)));
        tracing::info!(
            "Default TLS certificate reloaded from {}",
            cert_path.display()
        );
        Ok(())
    }

    /// Add (or replace) a cert for `domain`.
    ///
    /// Clones the current domain map, inserts the new entry, then atomically
    /// swaps the pointer. Concurrent readers always see a consistent snapshot.
    pub fn add_domain(&self, domain: &str, cert: CertifiedKey) {
        let mut map = (*self.domains.load_full()).clone();
        map.insert(domain.to_string(), Arc::new(cert));
        self.domains.store(Arc::new(map));
    }

    /// Remove the cert for `domain`. No-op if the domain is not registered. Also
    /// drops any minted floor cert for it (a deregistered domain needs no floor).
    pub fn remove_domain(&self, domain: &str) {
        let mut map = (*self.domains.load_full()).clone();
        map.remove(domain);
        self.domains.store(Arc::new(map));
        if self.floor_certs.load().contains_key(domain) {
            let mut floors = (*self.floor_certs.load_full()).clone();
            floors.remove(domain);
            self.floor_certs.store(Arc::new(floors));
        }
    }

    /// True iff a per-domain cert is currently registered for `domain`. Used by
    /// the per-domain cert loop to decide whether an on-disk cert still needs
    /// installing (e.g. after a restart, when the resolver starts empty).
    pub fn has_domain(&self, domain: &str) -> bool {
        self.domains.load().contains_key(domain)
    }

    /// Snapshot of every domain that currently has a per-domain cert installed.
    /// Used by the per-domain cert loop to reconcile against the set of `active`
    /// web domains (a domain in the resolver but no longer active is dropped).
    pub fn domain_names(&self) -> Vec<String> {
        self.domains.load().keys().cloned().collect()
    }

    /// Select the cert for an SNI hostname: a per-custom-domain cert if one is
    /// registered for `sni`, else the nest's default (apex) cert, else `None`
    /// while still pending (no default issued yet). This is the pure lookup
    /// behind [`resolve`](Self::resolve), extracted so SNI selection is
    /// unit-testable without constructing a rustls [`ClientHello`].
    pub fn cert_for_sni(&self, sni: Option<&str>) -> Option<Arc<CertifiedKey>> {
        if let Some(sni) = sni {
            let domains = self.domains.load();
            if let Some(cert) = domains.get(sni) {
                // Serve the registered trusted cert only while it is valid AND
                // covers the requested name; otherwise fall through to a floor
                // that carries this SNI, so a browser sees only an untrusted-CA
                // warning, never *also* a spurious name mismatch (and a native
                // app still gets a working channel-bound tunnel).
                // `tls-certificates.md` § A.
                if cert_valid_and_covers(cert, sni) {
                    return Some(Arc::clone(cert));
                }
                if let Some(floor) = self.floor_for_sni(sni) {
                    return Some(floor);
                }
            }
        }
        // No SNI at all → an IP dial (RFC 6066 forbids a literal address in SNI),
        // which on a domainless public box is the *only* way a browser can reach
        // this nest. Serve the IP bridge cert while it is valid; an expired or
        // absent one falls straight through to the floor/default below, so
        // nothing regresses when the bridge cannot be had (`tls-certificates.md`
        // § B-IP *Serving* / *When it cannot be had*). A **named** SNI never
        // reaches this: it keeps the per-SNI rule of § A above, unchanged.
        if sni.is_none()
            && let Some(ip_cert) = self.ip_cert.load_full()
            && cert_is_currently_valid(&ip_cert)
        {
            return Some(ip_cert);
        }
        // Unknown SNI (or no SNI, or no floor key wired) → the nest's own default
        // (apex) cert, or `None` while still pending (handshake fails cleanly,
        // same as ReloadableCertResolver).
        self.default.load_full()
    }

    /// Install (or replace) the **IP bridge cert** served to no-SNI dials — see
    /// [`cert_for_sni`](Self::cert_for_sni) and `tls-certificates.md` § B-IP. The
    /// swap is atomic, so a renewal rotates the served cert without a restart.
    pub fn set_ip_cert(&self, cert: CertifiedKey) {
        self.ip_cert.store(Some(Arc::new(cert)));
    }

    /// Drop the IP bridge cert, returning no-SNI dials to the floor/default. The
    /// switch-back once the deployment's own domain cert is live (§ B-IP
    /// *Lifetime* — the bridge is left to lapse, never renewed onward).
    pub fn clear_ip_cert(&self) {
        self.ip_cert.store(None);
    }

    /// The currently-installed IP bridge cert, if any. For tests and the
    /// lifecycle's "is one already loaded?" reconcile.
    pub fn current_ip_cert(&self) -> Option<Arc<CertifiedKey>> {
        self.ip_cert.load_full()
    }

    /// A self-signed floor cert covering exactly `sni`, minted from the stable
    /// floor key and cached. `None` if no floor key is installed (then
    /// [`cert_for_sni`](Self::cert_for_sni) falls to the apex default). Only
    /// called for an SNI that already has a registered (but invalid) trusted
    /// cert, so the cache is bounded by the served-domain set.
    fn floor_for_sni(&self, sni: &str) -> Option<Arc<CertifiedKey>> {
        if let Some(cached) = self.floor_certs.load().get(sni) {
            return Some(Arc::clone(cached));
        }
        let key_pem = self.floor_key_pem.load_full()?;
        let cert = Arc::new(mint_floor_cert(sni, &key_pem)?);
        // Copy-on-write insert (benign last-writer-wins race under concurrent
        // first handshakes for the same SNI — both return a valid covering cert).
        let mut map = (*self.floor_certs.load_full()).clone();
        map.insert(sni.to_string(), Arc::clone(&cert));
        self.floor_certs.store(Arc::new(map));
        Some(cert)
    }

    /// Hot-swap the default cert. In-flight handshakes keep the old cert;
    /// new handshakes get the new one.
    pub fn reload_default(&self, cert: CertifiedKey) {
        self.default.store(Some(Arc::new(cert)));
    }
}

impl fmt::Debug for MultiDomainCertResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MultiDomainCertResolver").finish()
    }
}

impl ResolvesServerCert for MultiDomainCertResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.cert_for_sni(client_hello.server_name())
    }
}

/// Mint a self-signed cert with SAN = \[`name`\], signed by the stable floor key
/// (PEM). The per-SNI floor fallback ([`MultiDomainCertResolver::floor_for_sni`]).
/// `None` on any parse/synthesis failure (the caller then falls to the apex
/// default). The cert carries only `name` as a SAN so a browser hitting that
/// domain sees no name mismatch — just the expected untrusted-CA warning.
fn mint_floor_cert(name: &str, floor_key_pem: &str) -> Option<CertifiedKey> {
    let key = rcgen::KeyPair::from_pem(floor_key_pem).ok()?;
    let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).ok()?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name.to_string());
    let cert = params.self_signed(&key).ok()?;
    load_certified_key_from_pem(cert.pem().as_bytes(), key.serialize_pem().as_bytes()).ok()
}

/// True iff `cert` is currently within its validity window (notBefore ≤ now <
/// notAfter) **and** a DNS SAN covers `name`. The per-SNI gate that decides
/// whether a registered trusted cert may be served or the resolver must fall
/// through to a name-covering floor (`tls-certificates.md` § A). A leaf that
/// won't parse is treated as not-coverable (fail closed → floor).
fn cert_valid_and_covers(cert: &CertifiedKey, name: &str) -> bool {
    let Ok(leaf) = cert.end_entity_cert() else {
        return false;
    };
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(leaf.as_ref()) else {
        return false;
    };
    if !within_validity_window(&parsed) {
        return false;
    }
    match parsed.subject_alternative_name() {
        Ok(Some(san)) => san.value.general_names.iter().any(|gn| {
            matches!(gn, x509_parser::extensions::GeneralName::DNSName(dns) if dns_name_matches(dns, name))
        }),
        _ => false,
    }
}

/// Is `parsed`'s `notBefore..notAfter` window open right now? The time half of
/// [`cert_valid_and_covers`], factored out because the **IP bridge cert** needs
/// exactly this and nothing else: a no-SNI dial carries no name to cover, so
/// § B-IP's "the no-SNI default **while valid**" is a pure validity question
/// (an IP SAN would not match the DNS-name test above in any case).
fn within_validity_window(parsed: &x509_parser::certificate::X509Certificate<'_>) -> bool {
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let validity = parsed.validity();
    now >= validity.not_before.timestamp() && now < validity.not_after.timestamp()
}

/// Is this certificate inside its validity window? The IP-bridge-cert half of
/// [`cert_valid_and_covers`] — see [`within_validity_window`]. An unparseable
/// leaf is treated as invalid, so the resolver falls through to the floor rather
/// than serving something it cannot reason about.
fn cert_is_currently_valid(cert: &CertifiedKey) -> bool {
    let Ok(leaf) = cert.end_entity_cert() else {
        return false;
    };
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(leaf.as_ref()) else {
        return false;
    };
    within_validity_window(&parsed)
}

/// Exact or single-label-wildcard DNS match (ASCII-case-insensitive).
/// `*.example.com` matches `a.example.com` but not `example.com` or
/// `a.b.example.com` — RFC 6125 wildcard scope (one leading label).
fn dns_name_matches(pattern: &str, name: &str) -> bool {
    if pattern.eq_ignore_ascii_case(name) {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.")
        && let Some((label, rest)) = name.split_once('.')
    {
        return !label.is_empty() && rest.eq_ignore_ascii_case(suffix);
    }
    false
}

/// Channel binding always signs the SPKI of the nest's **default** (apex) cert —
/// the cert a Fauna *client* connects to the nest with. Per-custom-domain certs
/// (added via [`add_domain`](MultiDomainCertResolver::add_domain)) serve *web
/// visitors* hitting `alice.com`, who do no Fauna channel binding, so selecting
/// one for a visitor's SNI never changes what `fauna.auth.handshake` signs.
/// Returns `None` while the default is still pending — binding is simply omitted
/// until the first apex cert lands. (See `security.md` § Transport trust, Axis 1
/// and `web-content-hosting.md` § Per-domain TLS.)
impl ServedCertSpki for MultiDomainCertResolver {
    fn current_spki_sha256(&self) -> Option<[u8; 32]> {
        // `cert_for_sni(None)`, NOT `current_default()`: the channel binding
        // must name the cert the connection was actually served, and once the
        // § B-IP bridge cert is installed a no-SNI dial is served *that*, not
        // the default. Reading the default here would sign one cert while the
        // client received another — an unconditional `TrustError` on exactly
        // the provisioned native path B-IP exists to unblock, and the reason a
        // security review called § B-IP *Serving* self-contradicting.
        //
        // With no bridge installed `cert_for_sni(None)` falls straight through
        // to the default, so this is behaviour-identical to what it replaced on
        // every box that is not bridging.
        //
        // Residual, stated rather than hidden: a **named**-SNI dial served
        // something other than the no-SNI cert (a registered domain whose
        // trusted cert has lapsed onto its per-SNI floor) still receives a
        // binding over a different cert. That is the single-SPKI binding's own
        // limitation — `build_cert_binding` has no connection context, so the
        // nest can sign exactly one SPKI — and it predates the bridge; closing
        // it needs an SNI-scoped binding, captured as its own track.
        let key = self.cert_for_sni(None)?;
        let leaf = key.end_entity_cert().ok()?;
        spki_sha256_of_cert_der(leaf.as_ref())
    }

    /// Facts for the cert this resolver would actually serve for `sni` — exactly
    /// what [`cert_for_sni`](Self::cert_for_sni) selects (a valid covering
    /// per-domain cert, else the per-SNI floor, else the apex default). So a
    /// domain whose trusted cert has expired reports `is_floor = true` (the
    /// resolver has already fallen through to the floor), matching what a client
    /// connecting to that domain sees.
    fn served_cert_facts(&self, sni: &str) -> Option<ServedCertFacts> {
        cert_facts(self.cert_for_sni(Some(sni))?.as_ref(), sni)
    }

    fn served_cert_spki_sha256(&self, sni: &str) -> Option<[u8; 32]> {
        // The SPKI of exactly what `cert_for_sni` would serve for `sni` — when
        // that is the per-SNI floor (e.g. `mail.<primary>` on the floor), this is
        // the stable floor-key SPKI the DANE TLSA pins.
        let key = self.cert_for_sni(Some(sni))?;
        let leaf = key.end_entity_cert().ok()?;
        spki_sha256_of_cert_der(leaf.as_ref())
    }
}

/// Background task that watches the ACME directory for cert file changes
/// and hot-reloads them into the [`MultiDomainCertResolver`]'s default cert.
///
/// Events are debounced (2 s) so that partial writes from the ACME client don't
/// trigger spurious reloads.
/// Watch `acme_dir` for cert changes and hot-reload the TLS config.
///
/// When `app_state` and `domain` are provided (Phase D1.6 / D3.2), every
/// successful file-change event calls `app_state.storage().store_acme_material(...)`
/// so that plaintext mode re-persists the PEM atomically (idempotent) and
/// encrypted mode wraps the cert to bridge pubkeys. Pulling the storage impl
/// via the accessor each time (rather than cloning at startup) ensures the
/// watcher always uses the current impl even if the mode was committed after
/// the watcher started. The call is non-fatal — errors are logged at ERROR level.
pub async fn cert_watcher_task(
    resolver: Arc<MultiDomainCertResolver>,
    acme_dir: PathBuf,
    app_state: Option<(std::sync::Arc<crate::routes::AppState>, String)>,
    relay_cert_changed: Option<tokio::sync::watch::Sender<u64>>,
) {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    let cert_path = acme_dir.join(CERT_FILENAME);
    let key_path = acme_dir.join(KEY_FILENAME);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res
            && matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_))
        {
            let _ = tx.try_send(());
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("Failed to create file watcher: {e}");
            return;
        }
    };

    if let Err(e) = watcher.watch(&acme_dir, RecursiveMode::NonRecursive) {
        tracing::error!("Failed to watch {}: {e}", acme_dir.display());
        return;
    }

    tracing::info!("Watching {} for cert changes", acme_dir.display());

    while rx.recv().await.is_some() {
        // Debounce: wait 2 s then drain any queued events.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        while rx.try_recv().is_ok() {}

        if cert_path.exists() && key_path.exists() {
            if let Err(e) = resolver.reload_default_from_pem(&cert_path, &key_path) {
                tracing::error!("Failed to reload TLS cert: {e}");
            }
            // The relay sidecar serves this same cert on `relay.<apex>`: tell
            // its channel, which tells the relay to fetch it again now. Its own
            // argument rather than a read through `app_state`, which is `None`
            // on a box that learned its domain at claim — every real one.
            if let Some(ref changed) = relay_cert_changed {
                changed.send_modify(|generation| *generation += 1);
            }

            // Phase D1.6 / D3.2: notify storage layer of the new cert. Pull the
            // current impl via the accessor so an encrypted-mode swap committed
            // after startup is picked up correctly. Non-fatal — TLS reload already done.
            if let Some((ref state, ref domain)) = app_state {
                match (std::fs::read(&cert_path), std::fs::read(&key_path)) {
                    (Ok(cert_pem), Ok(key_pem)) => {
                        if let Err(e) = state
                            .storage()
                            .store_acme_material(&crate::storage::AcmeMaterial {
                                domain,
                                cert_chain_pem: &cert_pem,
                                priv_key_pem: &key_pem,
                            })
                            .await
                        {
                            tracing::error!(
                                domain = %domain,
                                "cert_watcher_task: store_acme_material failed: {e}"
                            );
                        }
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        tracing::error!("cert_watcher_task: read PEM files failed: {e}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    // -------------------------------------------------------------------------
    // MultiDomainCertResolver tests
    // -------------------------------------------------------------------------
    //
    // We cannot construct a real `CertifiedKey` without actual PEM material,
    // and we cannot construct a real `ClientHello` outside of a TLS handshake.
    // The tests below therefore exercise the data-structure operations
    // (add/remove/reload) directly against the internal `ArcSwap` maps, and
    // verify that `ResolvesServerCert` is correctly implemented at the type
    // level (the impl block compiles and the trait bound is satisfied).

    /// Pre-generated self-signed Ed25519 certificate (test-only, not trusted by anyone).
    ///
    /// Generated with: openssl genpkey -algorithm ed25519 | openssl req -new -x509 -key /dev/stdin
    /// -subj "/CN=test.example.com" -days 3650 -out cert.pem
    const TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
        MIIBazCCAR2gAwIBAgIUMYMtgSkRpRwJGWbSKc9KW4Rly9wwBQYDK2VwMCIxIDAe\n\
        BgNVBAMMF3Rlc3QudGVzdC5leGFtcGxlLmNvbTAeFw0yNTAxMDEwMDAwMDBaFw0z\n\
        NTAxMDEwMDAwMDBaMCIxIDAeBgNVBAMMF3Rlc3QudGVzdC5leGFtcGxlLmNvbTAq\n\
        MAUGAytlcAMhAOVmEVAQS7V6mxBIEq6DjMz1HLKl7J9YdoVPgQpJRFSwo2MwYTAd\n\
        BgNVHQ4EFgQUzgxFJXiyEJiE5o1OhPnMPaCcNhMwHwYDVR0jBBgwFoAUzgxFJXiy\n\
        EJiE5o1OhPnMPaCcNhMwDwYDVR0TAQH/BAUwAwEB/zAOBgNVHQ8BAf8EBAMCAYYw\n\
        BQYDKzVwA0EAVuKPSZ/ywQgXuuqKYy5J3jVHqleFBmxEaSwD3vCqbLbSbakYFG9q\n\
        Sq5A9VNGFBa3Nx/dWBRoiBqKg2WF6bI2Cg==\n\
        -----END CERTIFICATE-----\n";

    const TEST_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
        MC4CAQAwBQYDK2VwBCIEIPevB/xmZCB9Ae3dBvU6A6FQMi1NTAmYvmLDJuFnIv8W\n\
        -----END PRIVATE KEY-----\n"; // gitleaks:allow

    /// Build a `CertifiedKey` from the embedded test PEM constants using the
    /// same logic as `ReloadableCertResolver::load_certified_key`.
    fn make_test_cert() -> CertifiedKey {
        use rustls::crypto::ring::sign::any_supported_type;

        let cert_pem = TEST_CERT_PEM.as_bytes();
        let key_pem = TEST_KEY_PEM.as_bytes();

        let certs: Vec<_> = CertificateDer::pem_slice_iter(cert_pem)
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("parse test cert");

        let key_der = PrivateKeyDer::from_pem_slice(key_pem).expect("parse test key");

        let signing_key = any_supported_type(&key_der).expect("signing key");
        CertifiedKey::new(certs, signing_key)
    }

    /// Generate a real self-signed cert (default rcgen ECDSA P-256, which ring
    /// can sign) and return its leaf DER plus a `CertifiedKey` for it. The
    /// embedded `TEST_CERT_PEM` above is opaque-but-unparseable filler that
    /// rustls stores without validating — it is *not* a real X.509 cert, so the
    /// SPKI parser rightly rejects it; these tests need a genuine cert.
    fn gen_real_cert() -> (Vec<u8>, CertifiedKey) {
        let key = rcgen::KeyPair::generate().expect("keygen");
        let params =
            rcgen::CertificateParams::new(vec!["test.local".to_string()]).expect("cert params");
        let cert = params.self_signed(&key).expect("self-sign");
        let der: Vec<u8> = cert.der().as_ref().to_vec();

        let key_pem = key.serialize_pem();
        let key_der = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).expect("parse key");
        let signing_key =
            rustls::crypto::ring::sign::any_supported_type(&key_der).expect("signing key");
        let certs = vec![rustls::pki_types::CertificateDer::from(der.clone())];
        (der, CertifiedKey::new(certs, signing_key))
    }

    /// Like [`gen_real_cert`] but with a caller-chosen DNS SAN and validity window
    /// (`days` until `notAfter`; `notBefore` = 1 day ago) — for the per-SNI
    /// validity/coverage tests (`days < 0` makes an already-expired cert).
    fn gen_real_cert_for(domain: &str, days: i64) -> (Vec<u8>, CertifiedKey) {
        let key = rcgen::KeyPair::generate().expect("keygen");
        let mut params =
            rcgen::CertificateParams::new(vec![domain.to_string()]).expect("cert params");
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let cert = params.self_signed(&key).expect("self-sign");
        let der: Vec<u8> = cert.der().as_ref().to_vec();
        let certified =
            load_certified_key_from_pem(cert.pem().as_bytes(), key.serialize_pem().as_bytes())
                .expect("load certified key");
        (der, certified)
    }

    /// A **CA-issued** leaf for `domain` (issuer DN != subject DN), valid for
    /// `days` — the "trusted" counterpart of the self-signed `gen_real_cert_for`,
    /// so `ServedCertFacts::is_floor` is exercised on a real chain shape.
    fn gen_ca_issued_cert_for(domain: &str, days: i64) -> CertifiedKey {
        let ca_key = rcgen::KeyPair::generate().expect("ca keygen");
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("ca params");
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Fauna Test Root CA");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca self-sign");

        let leaf_key = rcgen::KeyPair::generate().expect("leaf keygen");
        let mut leaf_params =
            rcgen::CertificateParams::new(vec![domain.to_string()]).expect("leaf params");
        leaf_params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        leaf_params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let leaf = leaf_params
            .signed_by(&leaf_key, &ca_cert, &ca_key)
            .expect("ca-sign leaf");
        load_certified_key_from_pem(leaf.pem().as_bytes(), leaf_key.serialize_pem().as_bytes())
            .expect("load certified key")
    }

    #[test]
    fn served_cert_facts_marks_self_signed_floor() {
        // A self-signed cert (issuer == subject) is the floor; its validity
        // window + coverage are reported truthfully.
        let (_der, floor) = gen_real_cert_for("alice.com", 90);
        let resolver = MultiDomainCertResolver::new(floor);
        let facts = resolver
            .served_cert_facts("alice.com")
            .expect("default cert facts");
        assert!(facts.is_floor, "self-signed leaf must read as the floor");
        assert!(facts.covers, "SAN covers the served name");
        assert!(
            facts.not_after_unix > facts.not_before_unix,
            "validity window is ordered"
        );
    }

    #[test]
    fn served_cert_facts_marks_ca_issued_trusted() {
        // A CA-issued leaf (issuer != subject) is NOT the floor.
        let resolver = MultiDomainCertResolver::new(gen_ca_issued_cert_for("alice.com", 90));
        let facts = resolver
            .served_cert_facts("alice.com")
            .expect("default cert facts");
        assert!(!facts.is_floor, "CA-issued leaf must NOT read as the floor");
        assert!(facts.covers);
    }

    #[test]
    fn served_cert_facts_follows_per_sni_selection() {
        // Per-domain trusted cert for alice.com; default (floor) for everything
        // else. served_cert_facts reports exactly what cert_for_sni serves.
        let (_d, default_floor) = gen_real_cert_for("nest.example", 90);
        let resolver = MultiDomainCertResolver::new(default_floor);
        resolver.add_domain("alice.com", gen_ca_issued_cert_for("alice.com", 90));

        let alice = resolver
            .served_cert_facts("alice.com")
            .expect("alice facts");
        assert!(!alice.is_floor, "alice serves its trusted per-domain cert");

        // An SNI with no per-domain cert falls to the (self-signed) apex default.
        let other = resolver
            .served_cert_facts("nest.example")
            .expect("default facts");
        assert!(other.is_floor, "fallback to the self-signed apex default");
    }

    #[test]
    fn served_cert_facts_reports_floor_when_per_domain_cert_expired() {
        // An expired per-domain trusted cert → cert_for_sni falls through to the
        // per-SNI floor, so the facts must read as the floor (renew needed), not
        // a stale "trusted".
        let (_d, default_cert) = gen_real_cert_for("nest.example", 90);
        let resolver = MultiDomainCertResolver::new(default_cert);
        resolver.set_floor_key(floor_key_pem());
        resolver.add_domain("alice.com", gen_ca_issued_cert_for("alice.com", -1));

        let facts = resolver
            .served_cert_facts("alice.com")
            .expect("alice facts");
        assert!(
            facts.is_floor,
            "expired trusted cert must fall through to the floor"
        );
        assert!(facts.covers, "the per-SNI floor still covers the name");
    }

    #[test]
    fn served_cert_facts_none_while_pending() {
        let pending = MultiDomainCertResolver::pending();
        assert!(pending.served_cert_facts("alice.com").is_none());
    }

    #[test]
    fn cert_health_state_maps_three_states() {
        use fauna_protocol::tls::CertHealthState;
        let now = 1_700_000_000;
        let trusted_far = ServedCertFacts {
            not_before_unix: now - 86_400,
            not_after_unix: now + 90 * 86_400,
            is_floor: false,
            covers: true,
        };
        // Trusted + comfortably valid → valid-trusted.
        assert_eq!(
            cert_health_state(Some(trusted_far), now),
            CertHealthState::ValidTrusted
        );
        // Trusted but within the 30-day lead → expiring.
        let near = ServedCertFacts {
            not_after_unix: now + 10 * 86_400,
            ..trusted_far
        };
        assert_eq!(
            cert_health_state(Some(near), now),
            CertHealthState::Expiring
        );
        // Exactly at the lead boundary is already expiring (>=).
        let boundary = ServedCertFacts {
            not_after_unix: now + CERT_RENEWAL_LEAD_SECS,
            ..trusted_far
        };
        assert_eq!(
            cert_health_state(Some(boundary), now),
            CertHealthState::Expiring
        );
        // Floor served → on-floor regardless of expiry.
        let floor = ServedCertFacts {
            is_floor: true,
            ..trusted_far
        };
        assert_eq!(
            cert_health_state(Some(floor), now),
            CertHealthState::OnFloorRenewNeeded
        );
        // Trusted but not covering this domain → on-floor (no usable cert here).
        let not_covering = ServedCertFacts {
            covers: false,
            ..trusted_far
        };
        assert_eq!(
            cert_health_state(Some(not_covering), now),
            CertHealthState::OnFloorRenewNeeded
        );
        // No cert served at all → on-floor.
        assert_eq!(
            cert_health_state(None, now),
            CertHealthState::OnFloorRenewNeeded
        );
    }

    /// A stable floor key PEM for the per-SNI floor tests.
    fn floor_key_pem() -> String {
        rcgen::KeyPair::generate().expect("keygen").serialize_pem()
    }

    #[test]
    fn spki_sha256_is_deterministic_and_parses() {
        let (der, _) = gen_real_cert();
        let fp1 = spki_sha256_of_cert_der(&der).expect("valid cert parses");
        let fp2 = spki_sha256_of_cert_der(&der).expect("valid cert parses");
        assert_eq!(fp1, fp2, "SPKI fingerprint is deterministic");
        assert_ne!(fp1, [0u8; 32]);
        // A different cert (different key) yields a different SPKI.
        let (der2, _) = gen_real_cert();
        assert_ne!(spki_sha256_of_cert_der(&der2).unwrap(), fp1);
    }

    #[test]
    fn spki_sha256_rejects_garbage() {
        assert!(spki_sha256_of_cert_der(b"not a certificate").is_none());
        assert!(spki_sha256_of_cert_der(&[]).is_none());
    }

    #[test]
    fn served_cert_spki_matches_loaded_leaf() {
        // A loaded resolver reports the SPKI of its leaf cert; a pending one
        // reports None (handshake binding is simply omitted while certless).
        let (der, certified) = gen_real_cert();
        let loaded = ReloadableCertResolver::from_certified_key(certified);
        let from_resolver = loaded.current_spki_sha256().expect("loaded cert");
        let from_der = spki_sha256_of_cert_der(&der).unwrap();
        assert_eq!(from_resolver, from_der);

        let pending = ReloadableCertResolver::pending();
        assert!(pending.current_spki_sha256().is_none());
    }

    #[test]
    fn served_cert_spki_for_floor_mx_pins_stable_floor_key() {
        // `mail.<primary>` served on the per-SNI floor: served_cert_spki_sha256
        // must equal the floor-key leaf SPKI (what a DANE sender pins) and be
        // stable across calls — the Phase-5b DANE coupling's pin value.
        let floor_pem = floor_key_pem();
        let resolver = MultiDomainCertResolver::new(gen_real_cert_for("nest.example", 90).1);
        resolver.set_floor_key(floor_pem.clone());
        // An expired trusted cert for the MX host → cert_for_sni falls to the floor.
        resolver.add_domain(
            "mail.example.com",
            gen_ca_issued_cert_for("mail.example.com", -1),
        );

        // We are pinning the floor, not a stale trusted cert.
        assert!(
            resolver
                .served_cert_facts("mail.example.com")
                .expect("facts")
                .is_floor,
            "MX must be on the floor for the TLSA to pin the floor key"
        );

        let served = resolver
            .served_cert_spki_sha256("mail.example.com")
            .expect("served spki on floor");
        // Stable across calls (cached floor cert from the same stable key).
        assert_eq!(
            served,
            resolver
                .served_cert_spki_sha256("mail.example.com")
                .unwrap()
        );
        // It is exactly the SPKI of a leaf minted from the same floor key.
        let minted = mint_floor_cert("mail.example.com", &floor_pem).expect("mint floor");
        let minted_leaf = minted.end_entity_cert().expect("minted leaf");
        let expected = spki_sha256_of_cert_der(minted_leaf.as_ref()).unwrap();
        assert_eq!(served, expected, "served SPKI pins the stable floor key");
    }

    #[test]
    fn served_cert_spki_for_trusted_mx_pins_the_trusted_leaf() {
        // A valid covering trusted cert is served → served_cert_spki_sha256 is
        // that cert's SPKI (not the floor), and is_floor is false (the TLSA
        // coupling will then withdraw — no floor pin while trusted).
        let (apex_der, apex) = gen_real_cert_for("nest.example", 90);
        let resolver = MultiDomainCertResolver::new(apex);
        resolver.set_floor_key(floor_key_pem());
        // A valid CA-issued (issuer != subject) cert for the MX host → trusted.
        let mx_cert = gen_ca_issued_cert_for("mail.example.com", 90);
        let mx_spki = spki_sha256_of_cert_der(mx_cert.end_entity_cert().unwrap().as_ref()).unwrap();
        resolver.add_domain("mail.example.com", mx_cert);

        assert!(
            !resolver
                .served_cert_facts("mail.example.com")
                .expect("facts")
                .is_floor,
            "a valid covering trusted cert is not the floor"
        );
        assert_eq!(
            resolver
                .served_cert_spki_sha256("mail.example.com")
                .expect("trusted spki"),
            mx_spki,
            "served SPKI is the trusted MX leaf's, not the floor's"
        );
        // An unknown SNI falls to the apex default cert's SPKI.
        assert_eq!(
            resolver.served_cert_spki_sha256("unknown.example").unwrap(),
            spki_sha256_of_cert_der(&apex_der).unwrap()
        );
    }

    #[test]
    fn served_cert_spki_none_while_pending() {
        let pending = MultiDomainCertResolver::pending();
        assert!(
            pending
                .served_cert_spki_sha256("mail.example.com")
                .is_none()
        );
    }

    #[test]
    fn multi_domain_resolver_starts_empty() {
        let resolver = MultiDomainCertResolver::new(make_test_cert());
        assert!(resolver.domains.load().is_empty());
    }

    #[test]
    fn multi_domain_resolver_add_and_remove_domain() {
        let resolver = MultiDomainCertResolver::new(make_test_cert());

        resolver.add_domain("custom.example.com", make_test_cert());
        assert!(resolver.domains.load().contains_key("custom.example.com"));

        resolver.add_domain("other.example.com", make_test_cert());
        assert_eq!(resolver.domains.load().len(), 2);

        resolver.remove_domain("custom.example.com");
        assert!(!resolver.domains.load().contains_key("custom.example.com"));
        assert!(resolver.domains.load().contains_key("other.example.com"));

        resolver.remove_domain("other.example.com");
        assert!(resolver.domains.load().is_empty());
    }

    #[test]
    fn multi_domain_resolver_remove_nonexistent_is_noop() {
        let resolver = MultiDomainCertResolver::new(make_test_cert());
        // Should not panic.
        resolver.remove_domain("never-added.example.com");
        assert!(resolver.domains.load().is_empty());
    }

    #[test]
    fn multi_domain_resolver_reload_default_swaps_cert() {
        let resolver = MultiDomainCertResolver::new(make_test_cert());
        let before = Arc::as_ptr(&resolver.current_default().expect("default loaded"));
        resolver.reload_default(make_test_cert());
        let after = Arc::as_ptr(&resolver.current_default().expect("default loaded"));
        // The pointer must have changed — a new Arc was stored.
        assert_ne!(before, after);
    }

    #[test]
    fn multi_domain_resolver_pending_has_no_default() {
        // pending() binds the listener with no default cert — resolve falls back
        // to None (handshake fails cleanly) until reload_default lands one.
        let resolver = MultiDomainCertResolver::pending();
        assert!(resolver.current_default().is_none());
        // Adding a per-domain cert while pending still tracks in the map; it does
        // not become a default.
        resolver.add_domain("custom.example.com", make_test_cert());
        assert!(resolver.current_default().is_none());
        assert!(resolver.domains.load().contains_key("custom.example.com"));
        // Loading the default starts serving the nest's own hostname.
        resolver.reload_default(make_test_cert());
        assert!(resolver.current_default().is_some());
    }

    #[test]
    fn multi_domain_served_cert_spki_tracks_default_only() {
        // Channel-binding invariant (security.md § Transport trust, Axis 1): the
        // handshake-signed SPKI is the DEFAULT (apex) cert's, never a
        // per-custom-domain cert's. A pending resolver reports None.
        let (default_der, default_cert) = gen_real_cert();
        let resolver = MultiDomainCertResolver::new(default_cert);

        let from_resolver = resolver.current_spki_sha256().expect("default cert SPKI");
        let from_der = spki_sha256_of_cert_der(&default_der).unwrap();
        assert_eq!(
            from_resolver, from_der,
            "SPKI must match the default cert's leaf"
        );

        // Adding a per-domain cert (a different cert/key) must NOT change the
        // reported SPKI — web visitors' certs never participate in binding.
        let (_other_der, other_cert) = gen_real_cert();
        resolver.add_domain("alice.com", other_cert);
        assert_eq!(
            resolver
                .current_spki_sha256()
                .expect("default still reported"),
            from_der,
            "per-domain cert must not change the channel-binding SPKI"
        );

        // A pending resolver omits binding.
        let pending = MultiDomainCertResolver::pending();
        assert!(pending.current_spki_sha256().is_none());
        // ...and a per-domain cert does not satisfy binding while pending.
        pending.add_domain("alice.com", gen_real_cert().1);
        assert!(pending.current_spki_sha256().is_none());
    }

    #[test]
    fn cert_for_sni_selects_per_domain_then_falls_back_to_default() {
        // The web-content success criterion at the resolver level: a registered
        // custom domain's SNI returns THAT domain's (valid, covering) cert; an
        // unknown SNI (or none) returns the default (apex) cert. Asserted via
        // `cert_for_sni` because a real rustls `ClientHello` can't be built
        // outside a handshake. Per-domain certs now must cover the name they're
        // registered under (the validity/coverage gate — see the floor tests).
        let (default_der, default_cert) = gen_real_cert();
        let (alice_der, alice_cert) = gen_real_cert_for("alice.com", 90);
        let resolver = MultiDomainCertResolver::new(default_cert);
        resolver.add_domain("alice.com", alice_cert);

        let spki = |key: &Arc<CertifiedKey>| {
            spki_sha256_of_cert_der(key.end_entity_cert().unwrap().as_ref()).unwrap()
        };
        let alice_fp = spki_sha256_of_cert_der(&alice_der).unwrap();
        let default_fp = spki_sha256_of_cert_der(&default_der).unwrap();
        assert_ne!(alice_fp, default_fp);

        // Matching SNI with a valid, covering cert → the per-domain cert.
        assert_eq!(
            spki(
                &resolver
                    .cert_for_sni(Some("alice.com"))
                    .expect("alice cert")
            ),
            alice_fp
        );
        // Unknown SNI → the default cert.
        assert_eq!(
            spki(
                &resolver
                    .cert_for_sni(Some("bob.com"))
                    .expect("default cert")
            ),
            default_fp
        );
        // No SNI → the default cert.
        assert_eq!(
            spki(&resolver.cert_for_sni(None).expect("default cert")),
            default_fp
        );

        // A pending resolver returns None for any SNI until a default lands,
        // but still serves a per-domain cert that was added while pending.
        let pending = MultiDomainCertResolver::pending();
        assert!(pending.cert_for_sni(Some("bob.com")).is_none());
        assert!(pending.cert_for_sni(None).is_none());
        pending.add_domain("carol.com", gen_real_cert_for("carol.com", 90).1);
        assert!(pending.cert_for_sni(Some("carol.com")).is_some());
        assert!(pending.cert_for_sni(Some("bob.com")).is_none());
    }

    #[test]
    fn cert_for_sni_serves_floor_when_per_domain_cert_is_expired() {
        // An expired registered cert must NOT be served (a browser rejects it);
        // with a floor key wired, the resolver mints a self-signed floor that
        // *covers* the SNI, so the browser sees only an untrusted-CA warning, not
        // a name mismatch (tls-certificates.md § A). The served SPKI is neither
        // the expired cert's nor the apex default's — it is the floor's.
        let (apex_der, apex) = gen_real_cert();
        let (expired_der, expired) = gen_real_cert_for("alice.com", -1); // already expired
        let resolver = MultiDomainCertResolver::new(apex);
        resolver.set_floor_key(floor_key_pem());
        resolver.add_domain("alice.com", expired);

        let served = resolver
            .cert_for_sni(Some("alice.com"))
            .expect("a floor cert");
        let served_fp =
            spki_sha256_of_cert_der(served.end_entity_cert().unwrap().as_ref()).unwrap();
        assert_ne!(
            served_fp,
            spki_sha256_of_cert_der(&expired_der).unwrap(),
            "the expired per-domain cert must not be served"
        );
        assert_ne!(
            served_fp,
            spki_sha256_of_cert_der(&apex_der).unwrap(),
            "the floor — not the bare apex default — covers the SNI"
        );
        // The floor cert actually carries the requested SNI as a SAN.
        let (_, parsed) =
            x509_parser::parse_x509_certificate(served.end_entity_cert().unwrap().as_ref())
                .unwrap();
        let san = parsed.subject_alternative_name().unwrap().unwrap();
        assert!(
            san.value.general_names.iter().any(|gn| matches!(
                gn,
                x509_parser::extensions::GeneralName::DNSName(d) if *d == "alice.com"
            )),
            "the floor must cover the requested name"
        );

        // Channel binding is unaffected: ServedCertSpki still reports the apex.
        assert_eq!(
            resolver.current_spki_sha256().unwrap(),
            spki_sha256_of_cert_der(&apex_der).unwrap(),
            "per-SNI floor fallback must not change the channel-binding SPKI"
        );

        // Cached: a second lookup returns the same floor cert (no re-mint).
        let again = resolver
            .cert_for_sni(Some("alice.com"))
            .expect("cached floor");
        assert!(Arc::ptr_eq(&served, &again), "floor cert is cached per SNI");
    }

    #[test]
    fn cert_for_sni_serves_floor_when_per_domain_cert_does_not_cover() {
        // A registered cert that is in-date but does NOT cover the SNI (a
        // mis-issued or stale-SAN cert) is also rejected in favour of a covering
        // floor — coverage, not just expiry, gates the trusted cert.
        let (_apex_der, apex) = gen_real_cert();
        let (noncov_der, noncov) = gen_real_cert_for("other.example", 90); // valid but wrong SAN
        let resolver = MultiDomainCertResolver::new(apex);
        resolver.set_floor_key(floor_key_pem());
        resolver.add_domain("alice.com", noncov);

        let served = resolver
            .cert_for_sni(Some("alice.com"))
            .expect("a floor cert");
        let served_fp =
            spki_sha256_of_cert_der(served.end_entity_cert().unwrap().as_ref()).unwrap();
        assert_ne!(
            served_fp,
            spki_sha256_of_cert_der(&noncov_der).unwrap(),
            "a non-covering per-domain cert must not be served"
        );
    }

    /// **Characterization of a known, deliberately-scoped gap** (the mint's bound). An **unregistered**
    /// SNI — which is what an active local *mail* domain is, since `add_domain`'s
    /// only production callers are the custom-web-domain loop in
    /// `web_content/cert.rs` — never reaches the per-SNI floor mint. It falls to
    /// the apex default, and whether that is name-clean depends entirely on the
    /// default's own SANs:
    ///
    /// - **ACME-healthy deployment (the designed steady state): fine.** A
    ///   resolve-verified secondary's apex joins the apex ACME order
    ///   (`acme_http01::reachable_mail_domains` → `desired_san_domains`, pinned by
    ///   `desired_sans_multi_domain_lowercased_and_trimmed`), so the default leaf
    ///   covers it — asserted below as the first half.
    /// - **Floor-only deployment: name mismatch.** The floor's SANs are
    ///   apex-derived (`self_signed_cert::write_self_signed_bootstrap`), so a
    ///   public secondary on a box that never obtained a trusted cert is served a
    ///   cert that does not carry its name — the second half below.
    ///
    /// This test exists to keep that boundary honest and machine-checked: it is
    /// the red a fix for row 105 must flip, and it stops a future reader from
    /// re-discovering the steady-state case as a bug (it is not one).
    #[test]
    fn cert_for_sni_covers_an_unregistered_secondary_only_via_the_default_cert() {
        // Steady state: the default leaf carries the secondary's SAN.
        let (_der, apex_with_secondary) = gen_real_cert_for("second.example", 90);
        let resolver = MultiDomainCertResolver::new(apex_with_secondary);
        resolver.set_floor_key(floor_key_pem());
        let served = resolver
            .cert_for_sni(Some("second.example"))
            .expect("the default cert is served for an unregistered SNI");
        assert!(
            cert_valid_and_covers(&served, "second.example"),
            "once the secondary's apex is in the ACME order, the default leaf \
             covers it and the unregistered-SNI fallthrough is name-clean"
        );

        // Floor-only: the apex-derived floor cannot cover a secondary, and the
        // per-SNI mint is out of reach because the name is unregistered.
        let (_floor_der, apex_floor) = gen_real_cert_for("primary.example", 90);
        let floor_only = MultiDomainCertResolver::new(apex_floor);
        floor_only.set_floor_key(floor_key_pem());
        let served = floor_only
            .cert_for_sni(Some("second.example"))
            .expect("the default cert is still served, never a dead handshake");
        assert!(
            !cert_valid_and_covers(&served, "second.example"),
            "row 105: on a floor-only box a public secondary is served a \
             name-mismatched cert — fixing row 105 means flipping this assert"
        );
    }

    #[test]
    fn cert_for_sni_falls_to_default_when_no_floor_key() {
        // Without a floor key (e.g. before boot wiring), an invalid per-domain
        // cert falls to the apex default — never serves the invalid cert, never
        // panics. (Degraded — a browser sees a name mismatch — but never dead.)
        let (apex_der, apex) = gen_real_cert();
        let (_, expired) = gen_real_cert_for("alice.com", -1);
        let resolver = MultiDomainCertResolver::new(apex);
        resolver.add_domain("alice.com", expired);

        let served = resolver
            .cert_for_sni(Some("alice.com"))
            .expect("default cert");
        assert_eq!(
            spki_sha256_of_cert_der(served.end_entity_cert().unwrap().as_ref()).unwrap(),
            spki_sha256_of_cert_der(&apex_der).unwrap(),
            "no floor key → fall to the apex default"
        );
    }

    #[test]
    fn dns_name_matches_exact_and_wildcard() {
        assert!(dns_name_matches("alice.com", "alice.com"));
        assert!(dns_name_matches("ALICE.com", "alice.COM")); // case-insensitive
        assert!(!dns_name_matches("alice.com", "bob.com"));
        assert!(dns_name_matches("*.example.com", "a.example.com"));
        assert!(!dns_name_matches("*.example.com", "example.com")); // apex not covered
        assert!(!dns_name_matches("*.example.com", "a.b.example.com")); // one label only
        assert!(!dns_name_matches("*.example.com", ".example.com")); // empty label
    }

    #[test]
    fn multi_domain_has_domain_and_domain_names_reflect_map() {
        let resolver = MultiDomainCertResolver::new(make_test_cert());
        assert!(resolver.domain_names().is_empty());
        assert!(!resolver.has_domain("alice.com"));

        resolver.add_domain("alice.com", make_test_cert());
        resolver.add_domain("bob.com", make_test_cert());
        assert!(resolver.has_domain("alice.com"));
        let mut names = resolver.domain_names();
        names.sort();
        assert_eq!(names, vec!["alice.com".to_string(), "bob.com".to_string()]);

        resolver.remove_domain("alice.com");
        assert!(!resolver.has_domain("alice.com"));
        assert_eq!(resolver.domain_names(), vec!["bob.com".to_string()]);
    }

    #[test]
    fn multi_domain_resolver_add_replaces_existing() {
        let resolver = MultiDomainCertResolver::new(make_test_cert());
        resolver.add_domain("replace.example.com", make_test_cert());
        let first_ptr = Arc::as_ptr(resolver.domains.load().get("replace.example.com").unwrap());
        resolver.add_domain("replace.example.com", make_test_cert());
        let second_ptr = Arc::as_ptr(resolver.domains.load().get("replace.example.com").unwrap());
        // Pointer must change — the cert was replaced.
        assert_ne!(first_ptr, second_ptr);
        assert_eq!(resolver.domains.load().len(), 1);
    }

    /// Verify that `MultiDomainCertResolver` satisfies the `ResolvesServerCert`
    /// trait bound so it can be used where rustls expects it.
    #[test]
    fn multi_domain_resolver_satisfies_resolves_server_cert_bound() {
        fn assert_resolves<T: ResolvesServerCert>(_: &T) {}
        let resolver = MultiDomainCertResolver::new(make_test_cert());
        assert_resolves(&resolver);
    }

    #[test]
    fn multi_domain_resolver_debug_impl() {
        let resolver = MultiDomainCertResolver::new(make_test_cert());
        let s = format!("{resolver:?}");
        assert!(s.contains("MultiDomainCertResolver"));
    }

    // -------------------------------------------------------------------------
    // ReloadableCertResolver tests (Gap A: no-restart first cert)
    // -------------------------------------------------------------------------

    /// A pending resolver (built before the first ACME cert exists) must report
    /// no cert, and `resolve` must therefore return `None` so the TLS handshake
    /// fails cleanly instead of serving plain HTTP. Once `cert_watcher_task`
    /// calls `reload()` with the freshly-issued PEM, the resolver serves it —
    /// without rebinding the listener.
    #[test]
    fn reloadable_resolver_starts_pending_then_serves_after_reload() {
        let resolver = ReloadableCertResolver::pending();
        assert!(
            resolver.current().is_none(),
            "a pending resolver must have no cert until ACME issues one"
        );

        // Write the embedded test PEM to a temp dir, exactly as the HTTP-01
        // ACME client writes fullchain.pem/privkey.pem into acme_dir.
        let dir = tempfile::tempdir().expect("tempdir");
        let cert_path = dir.path().join(CERT_FILENAME);
        let key_path = dir.path().join(KEY_FILENAME);
        std::fs::write(&cert_path, TEST_CERT_PEM).expect("write cert");
        std::fs::write(&key_path, TEST_KEY_PEM).expect("write key");

        // The watcher's reload path populates the pending resolver in place.
        resolver.reload(&cert_path, &key_path).expect("reload");
        assert!(
            resolver.current().is_some(),
            "resolver must serve the cert once the watcher reloads it"
        );
    }

    /// `from_pem` starts already-loaded (the boot-with-cert path).
    #[test]
    fn reloadable_resolver_from_pem_starts_loaded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert_path = dir.path().join(CERT_FILENAME);
        let key_path = dir.path().join(KEY_FILENAME);
        std::fs::write(&cert_path, TEST_CERT_PEM).expect("write cert");
        std::fs::write(&key_path, TEST_KEY_PEM).expect("write key");

        let resolver = ReloadableCertResolver::from_pem(&cert_path, &key_path).expect("from_pem");
        assert!(resolver.current().is_some());
    }

    // -------------------------------------------------------------------------
    // build_acme_config tests
    //
    // Regression coverage: nest used to ignore the [acme] section of nest.toml
    // entirely, falling back to AcmeConfig::default() (acme_dir =
    // "/var/lib/fauna/acme") which the unprivileged `fauna` user inside the
    // docker image cannot create. These tests pin down the precedence:
    // --acme-dir > [acme].dir > default; the domain is [nest].domain.
    // -------------------------------------------------------------------------

    fn parse_nest_config(toml: &str) -> crate::config::NestConfig {
        toml::from_str(toml).expect("test TOML parses")
    }

    const MINIMAL_NEST_TOML: &str = r#"
[nest]
mode = "public"
listen = "0.0.0.0:8443"
db_path = "nest.db"
"#;

    #[test]
    fn build_acme_config_defaults_when_unset() {
        let nest = parse_nest_config(MINIMAL_NEST_TOML);
        let (cfg, enabled) = build_acme_config(&nest, nest.nest.mode, None);
        assert!(!enabled);
        assert_eq!(cfg.acme_dir, PathBuf::from("/var/lib/fauna/acme"));
        assert_eq!(cfg.domain, "");
    }

    #[test]
    fn build_acme_config_reads_toml_acme_and_domain() {
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:8443"
db_path = "nest.db"
domain = "nest.example.com"

[acme]
mode = "http01"
dir = "/data/acme"
"#;
        let nest = parse_nest_config(toml);
        let (cfg, enabled) = build_acme_config(&nest, nest.nest.mode, None);
        assert!(
            enabled,
            "a configured public domain should mark the result enabled"
        );
        assert_eq!(
            cfg.acme_dir,
            PathBuf::from("/data/acme"),
            "[acme].dir from nest.toml must reach AcmeConfig"
        );
        assert_eq!(cfg.domain, "nest.example.com");
        assert_eq!(
            cfg.directory_url, None,
            "no [acme].directory_url ⇒ None (the HTTP-01 client uses Let's Encrypt)"
        );
    }

    #[test]
    fn build_acme_config_reads_toml_directory_url() {
        // The ACME directory-URL override (a private/internal CA, or the tier_4
        // pebble) flows from `[acme].directory_url` into AcmeConfig, where it is the
        // CA the HTTP-01 client orders against.
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:8443"
db_path = "nest.db"
domain = "nest.example.com"

[acme]
mode = "http01"
dir = "/data/acme"
directory_url = "https://pebble:14000/dir"
"#;
        let nest = parse_nest_config(toml);
        let (cfg, enabled) = build_acme_config(&nest, nest.nest.mode, None);
        assert!(enabled);
        assert_eq!(
            cfg.directory_url.as_deref(),
            Some("https://pebble:14000/dir"),
            "[acme].directory_url from nest.toml must reach AcmeConfig"
        );
    }

    #[test]
    fn build_acme_config_disabled_on_private_axis() {
        // deployment-home-with-public-relay.md § Don't do these: a private
        // NAT-axis nest runs no ACME client even with a domain configured
        // (e.g. a box re-purposed from a public role). build_acme_config is
        // the single source of truth that force-disables it on the private axis.
        let toml = r#"
[nest]
mode = "private"
listen = "0.0.0.0:8443"
db_path = "nest.db"
domain = "home.example.com"

[acme]
mode = "http01"
"#;
        let nest = parse_nest_config(toml);
        let (_cfg, enabled) = build_acme_config(&nest, nest.nest.mode, None);
        assert!(
            !enabled,
            "ACME must be force-disabled on the private NAT axis"
        );
    }

    #[test]
    fn build_acme_config_cli_dir_overrides_toml_dir() {
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:8443"
db_path = "nest.db"
domain = "toml.example.com"

[acme]
mode = "http01"
dir = "/data/acme"
"#;
        let nest = parse_nest_config(toml);
        let (cfg, enabled) = build_acme_config(&nest, nest.nest.mode, Some("/cli/acme"));
        assert!(enabled);
        assert_eq!(cfg.domain, "toml.example.com");
        assert_eq!(cfg.acme_dir, PathBuf::from("/cli/acme"));
    }

    #[test]
    fn build_acme_config_disabled_for_localhost_domain() {
        // A `DOMAIN=localhost` box is public NAT-wise but can never complete a
        // public HTTP-01 order — ACME must stay off (it serves the always-live
        // self-signed floor instead). This is exactly the behavior the removed
        // `[acme].enabled = (DOMAIN != localhost)` entrypoint seed carried; it
        // now lives in the derived gate. Same for a domainless box (covered by
        // `build_acme_config_defaults_when_unset`: empty domain ⇒ disabled).
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:8443"
db_path = "nest.db"
domain = "localhost"

[acme]
mode = "http01"
"#;
        let nest = parse_nest_config(toml);
        let (cfg, enabled) = build_acme_config(&nest, nest.nest.mode, None);
        assert!(
            !enabled,
            "a localhost domain must not enable ACME (no public order is possible)"
        );
        assert_eq!(cfg.domain, "localhost");
    }

    /// The CA and the account contact are constants (`tls-certificates.md` § ACME
    /// settings — constants, not choices): a `nest.toml` still carrying the retired
    /// `[acme] email` / `staging` keys parses (the config sets no
    /// `deny_unknown_fields`) and changes nothing — the result is exactly the one
    /// from a file without them.
    #[test]
    fn retired_acme_email_and_staging_keys_change_nothing() {
        let with_keys = parse_nest_config(
            r#"
[nest]
mode = "public"
listen = "0.0.0.0:8443"
db_path = "nest.db"
domain = "nest.example.com"

[acme]
email = "a@b"
staging = true
"#,
        );
        let without_keys = parse_nest_config(
            r#"
[nest]
mode = "public"
listen = "0.0.0.0:8443"
db_path = "nest.db"
domain = "nest.example.com"

[acme]
"#,
        );
        let built = |c: &crate::config::NestConfig| build_acme_config(c, c.nest.mode, None);
        let (with_cfg, with_enabled) = built(&with_keys);
        let (without_cfg, without_enabled) = built(&without_keys);
        assert_eq!(with_enabled, without_enabled);
        assert_eq!(format!("{with_cfg:?}"), format!("{without_cfg:?}"));
        assert!(
            !format!("{with_cfg:?}").contains("a@b"),
            "no contact address may survive anywhere in the ACME config"
        );
    }

    // -------------------------------------------------------------------------
    // The IP bridge cert (`tls-certificates.md` § B-IP)
    //
    // A domainless box on a public IP can hold no trusted cert for a name it does
    // not yet know, so a *browser* cannot reach it at all — the whole web
    // onboarding wizard runs pre-claim. These pin the three pure policy points:
    // which addresses are orderable, when the cert is renewed, and when the
    // bridge is dropped for good.
    // -------------------------------------------------------------------------

    fn ip(s: &str) -> std::net::IpAddr {
        s.parse().expect("test IP literal parses")
    }

    #[test]
    fn ip_bridge_orders_for_a_directly_attached_public_address() {
        let attached = [ip("93.184.216.34")];
        assert_eq!(
            ip_bridge_addresses(crate::config::NodeMode::Public, &attached),
            vec![ip("93.184.216.34")],
            "a public box with a global-unicast interface address is the whole point of § B-IP"
        );
    }

    #[test]
    fn ip_bridge_orders_nothing_on_the_private_nat_axis() {
        // § B-IP *When*: "the private-NAT-axis gate still wins". A private box
        // runs no ACME client at all, and an RFC 8738 identifier could not
        // validate from behind NAT even if it did.
        let attached = [ip("93.184.216.34"), ip("2606:4700:4700::1111")];
        assert!(
            ip_bridge_addresses(crate::config::NodeMode::Private, &attached).is_empty(),
            "a client-set-private nest must order no IP cert even with a routable address"
        );
    }

    #[test]
    fn ip_bridge_orders_nothing_for_a_nat_or_lan_box() {
        // The everyday home deployment: every attached address is non-global, so
        // the derive is empty and nothing about that box changes.
        let attached = [
            ip("192.168.1.40"),  // RFC 1918
            ip("10.0.0.5"),      // RFC 1918
            ip("100.64.0.1"),    // CGNAT — routable-looking, not orderable
            ip("169.254.10.10"), // link-local
            ip("fe80::1"),       // IPv6 link-local
            ip("fd00::1"),       // IPv6 unique-local
        ];
        assert!(
            ip_bridge_addresses(crate::config::NodeMode::Public, &attached).is_empty(),
            "only an address the CA can route to may carry an `ip` identifier"
        );
    }

    #[test]
    fn ip_bridge_addresses_are_deduped_and_ordered_v4_then_v6() {
        // The SAN set feeds the CA's duplicate-certificate accounting (5/week per
        // exact set), so it must not churn with the interface table's enumeration
        // order, and a repeated address must not order twice.
        let attached = [
            ip("2606:4700:4700::1111"),
            ip("8.8.8.8"),
            ip("93.184.216.34"),
            ip("8.8.8.8"),
            ip("192.168.1.40"),
        ];
        assert_eq!(
            ip_bridge_addresses(crate::config::NodeMode::Public, &attached),
            vec![
                ip("8.8.8.8"),
                ip("93.184.216.34"),
                ip("2606:4700:4700::1111")
            ],
            "stable v4-then-v6 numeric order, duplicates collapsed, private dropped"
        );
    }

    #[test]
    fn ip_bridge_addrs_changed_is_order_insensitive() {
        let a = [ip("203.0.113.7"), ip("2001:db8::1")];
        let b = [ip("2001:db8::1"), ip("203.0.113.7")];
        assert!(
            !ip_bridge_addrs_changed(&a, &b),
            "the same set in a different order is not a change — a value read \
             back from disk, or handed in by a caller, is not guaranteed to \
             preserve ip_bridge_addresses' sort"
        );
    }

    #[test]
    fn ip_bridge_addrs_changed_ignores_what_the_narrowed_chain_covers() {
        // The scenario fixes: a v4-only narrowing installed a chain
        // covering only the v4 address, but the box still has both. Comparing
        // against what was last ATTEMPTED (both) — not what the chain COVERS
        // (v4 only) — is what lets this settle instead of re-ordering forever.
        let live = [ip("203.0.113.7"), ip("2001:db8::1")];
        let last_attempted = [ip("203.0.113.7"), ip("2001:db8::1")];
        assert!(
            !ip_bridge_addrs_changed(&live, &last_attempted),
            "an unchanged interface set must not force a re-order, even though \
             the installed chain (v4-only) does not cover all of it"
        );
    }

    #[test]
    fn ip_bridge_addrs_changed_catches_a_newly_attached_address() {
        let live = [ip("203.0.113.7"), ip("198.51.100.9")];
        let last_attempted = [ip("203.0.113.7")];
        assert!(
            ip_bridge_addrs_changed(&live, &last_attempted),
            "a floating IP attached after the last decision must still force a \
             re-order — the one win the settling fix must not lose"
        );
    }

    /// Let's Encrypt issues IP certs **only** under the `shortlived` profile —
    /// 160 h — so this is the real window the cadence must fit.
    const SHORTLIVED_VALIDITY_SECS: i64 = 160 * 3600;

    #[test]
    fn a_fresh_shortlived_ip_cert_is_not_yet_due() {
        let issued = 1_800_000_000;
        let expires = issued + SHORTLIVED_VALIDITY_SECS;
        assert!(
            !ip_cert_needs_renewal(issued, expires, issued + 3600),
            "an hour-old 160 h cert has ~159 h left — far more than the one-third lead"
        );
    }

    #[test]
    fn a_shortlived_ip_cert_renews_with_one_third_of_its_life_left() {
        // § B-IP *Lifetime*: renewed at one third of the remaining validity —
        // ~53 h of a 160 h cert, i.e. roughly every 4 days, which fits the
        // 5-per-week duplicate limit for one SAN set.
        let issued = 1_800_000_000;
        let expires = issued + SHORTLIVED_VALIDITY_SECS;
        let lead = SHORTLIVED_VALIDITY_SECS / 3;
        assert_eq!(lead, 192_000, "one third of 160 h is ~53.3 h");
        assert!(
            !ip_cert_needs_renewal(issued, expires, expires - lead - 1),
            "one second before the lead opens, the cert is still fresh"
        );
        assert!(
            ip_cert_needs_renewal(issued, expires, expires - lead),
            "at exactly one third remaining the renewal is due"
        );
    }

    #[test]
    fn an_expired_or_degenerate_ip_cert_is_always_due() {
        let issued = 1_800_000_000;
        let expires = issued + SHORTLIVED_VALIDITY_SECS;
        assert!(
            ip_cert_needs_renewal(issued, expires, expires + 1),
            "a box that missed its window re-orders rather than sitting on a dead cert"
        );
        assert!(
            ip_cert_needs_renewal(expires, issued, issued + 1),
            "an inverted window tells us nothing about a safe lead — treat it as due"
        );
        assert!(
            ip_cert_needs_renewal(issued, issued, issued),
            "a zero-length window is due, not divided by three"
        );
    }

    #[test]
    fn the_bridge_stops_renewing_once_the_domain_cert_is_live() {
        // § B-IP *Lifetime* — "a bridge, not a permanent identity". The IP is
        // never the deployment's identity, so once the domain cert is live the IP
        // cert is left to lapse.
        assert!(
            ip_bridge_should_renew(true, false),
            "a domainless public box keeps the bridge alive"
        );
        assert!(
            !ip_bridge_should_renew(true, true),
            "a live trusted primary-domain cert ends the bridge"
        );
        assert!(
            !ip_bridge_should_renew(false, false),
            "no orderable address, nothing to renew"
        );
    }

    // ---- serving (§ B-IP *Serving*) ----------------------------------------

    #[test]
    fn a_no_sni_dial_is_served_the_ip_bridge_cert() {
        // An IP dial carries no SNI (RFC 6066 forbids a literal address there),
        // and that is exactly the browser reaching a domainless box.
        let (_, default_cert) = gen_real_cert_for("nest.example.com", 30);
        let (_, ip_cert) = gen_real_cert_for("203.0.113.7", 6);
        let resolver = MultiDomainCertResolver::new(default_cert);
        resolver.set_ip_cert(ip_cert);

        let served = resolver
            .cert_for_sni(None)
            .expect("a cert for a no-SNI dial");
        assert_ne!(
            spki_of(&served),
            spki_of(&resolver.current_default().expect("default installed")),
            "the bridge must actually displace the apex default for a no-SNI dial"
        );
        assert_eq!(
            spki_of(&served),
            spki_of(&resolver.current_ip_cert().expect("ip cert installed")),
            "the no-SNI dial gets the IP bridge cert, not the apex default"
        );
    }

    #[test]
    fn a_named_sni_never_gets_the_ip_bridge_cert() {
        // § B-IP *Serving*: "named SNIs keep the per-SNI rule of § A step 2".
        let (_, default_cert) = gen_real_cert_for("nest.example.com", 30);
        let (_, ip_cert) = gen_real_cert_for("203.0.113.7", 6);
        let resolver = MultiDomainCertResolver::new(default_cert);
        resolver.set_ip_cert(ip_cert);

        let served = resolver
            .cert_for_sni(Some("nest.example.com"))
            .expect("a cert for a named dial");
        assert_eq!(
            spki_of(&served),
            spki_of(&resolver.current_default().expect("default installed")),
            "a named SNI is unaffected by the bridge"
        );
    }

    #[test]
    fn an_expired_ip_bridge_cert_falls_through_to_the_default() {
        // § B-IP *When it cannot be had*: "nothing regresses — the box serves the
        // floor as today". The same holds once a bridge cert lapses unrenewed.
        let (_, default_cert) = gen_real_cert_for("nest.example.com", 30);
        let (_, stale_ip_cert) = gen_real_cert_for("203.0.113.7", -1);
        let resolver = MultiDomainCertResolver::new(default_cert);
        resolver.set_ip_cert(stale_ip_cert);

        let served = resolver
            .cert_for_sni(None)
            .expect("a cert for a no-SNI dial");
        assert_eq!(
            spki_of(&served),
            spki_of(&resolver.current_default().expect("default installed")),
            "an expired bridge cert must not be served — fall through to the default/floor"
        );
    }

    #[test]
    fn clearing_the_bridge_returns_no_sni_dials_to_the_default() {
        let (_, default_cert) = gen_real_cert_for("nest.example.com", 30);
        let (_, ip_cert) = gen_real_cert_for("203.0.113.7", 6);
        let resolver = MultiDomainCertResolver::new(default_cert);
        resolver.set_ip_cert(ip_cert);
        resolver.clear_ip_cert();

        let served = resolver
            .cert_for_sni(None)
            .expect("a cert for a no-SNI dial");
        assert_eq!(
            spki_of(&served),
            spki_of(&resolver.current_default().expect("default installed")),
            "the switch-back leaves no trace in the resolver"
        );
    }

    #[test]
    fn the_channel_binding_spki_follows_the_no_sni_served_cert() {
        // § B-IP *Serving*, as corrected 2026-09-02: the channel-binding SPKI names the
        // cert a **no-SNI dial is served**, so it is the bridge while one is
        // installed and the default otherwise.
        //
        // The superseded pin asserted the opposite ("still follows the default"),
        // on the premise that native apps always carry SNI and never ride the
        // bridge. That premise is false by construction — the bridge exists for
        // boxes with no domain to dial, so the provisioned native path reaches
        // them by IP, which carries no SNI (RFC 6066) — and honouring it would
        // sign one cert while the client was served another: an unconditional
        // `TrustError` on the one path B-IP exists to unblock.
        let (_, default_cert) = gen_real_cert_for("nest.example.com", 30);
        let (_, ip_cert) = gen_real_cert_for("203.0.113.7", 6);
        let resolver = MultiDomainCertResolver::new(default_cert);
        let default_spki = resolver.current_spki_sha256().expect("default spki");
        assert_eq!(
            default_spki,
            spki_of(&resolver.current_default().expect("default installed")),
            "with no bridge installed the binding names the default, as it always has"
        );

        resolver.set_ip_cert(ip_cert);
        let bridged = resolver.current_spki_sha256().expect("bridge spki");
        assert_eq!(
            bridged,
            spki_of(
                &resolver
                    .cert_for_sni(None)
                    .expect("a cert for a no-SNI dial")
            ),
            "while bridging, the binding names exactly what a no-SNI dial is served"
        );
        assert_ne!(
            bridged, default_spki,
            "the bridge cert is a different cert from the default — otherwise this pin is vacuous"
        );

        // …and the switch-back returns the binding to the default, so a native
        // app re-binds once when the bridge is dropped rather than being left
        // pinned to a cert the box no longer serves.
        resolver.clear_ip_cert();
        assert_eq!(
            resolver.current_spki_sha256().expect("default spki"),
            default_spki,
            "dropping the bridge returns the binding to the default"
        );
    }

    /// SPKI fingerprint of a certified key's leaf — the cheap identity check the
    /// serving tests compare by.
    fn spki_of(cert: &CertifiedKey) -> [u8; 32] {
        let leaf = cert.end_entity_cert().expect("leaf");
        spki_sha256_of_cert_der(leaf.as_ref()).expect("spki")
    }
}
