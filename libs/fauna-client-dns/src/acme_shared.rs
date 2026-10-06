//! The **target-independent** core of the client-driven DNS-01 ACME order — the
//! data types, the publish/teardown choreography, and the issued-cert bundle
//! helpers shared by the two order drivers:
//!
//! - [`acme_order`](crate::acme_order) — the **native** driver over `instant-acme`
//!   (the 5 native apps; `ring`/`aws-lc-rs` JWS + `rcgen` CSR), and
//! - [`acme_pure`](crate::acme_pure) — the **wasm** driver over RustCrypto + reqwest
//!   (web; the browser can't link `instant-acme`'s crypto — D2).
//!
//! Neither driver re-implements the seam choreography or the wire data shapes: the
//! ACME-protocol difference between the two is purely *how the order is driven*
//! (`instant_acme::Order` vs the pure reqwest flow), so everything that is **not**
//! the order-driving lives here, compiled once for both targets. In particular
//! [`with_published_challenges`] — the "publish every `_acme-challenge` TXT through
//! the [`DnsProviderSeam`], run the CA dance, then **always** tear the TXT(s) back
//! down even on failure" invariant — is generic over the CA-dance future, so each
//! driver plugs its own `run_ca_dance` into the same orchestration
//! (`tls-certificates.md` § "The `_acme-challenge` record" — one writer, never a
//! parallel path).
//!
//! **Account reuse (D6).** [`Dns01Issued::account_credentials`] threads the ACME
//! `AccountCredentials` back out for the caller to persist BackupKey-sealed in
//! `DnsConfig::acme_account` (`fauna.state.dns`), reused across the admin's devices and renewals so any synced
//! device renews against the *same* account (`tls-certificates.md` § C.3) — the
//! byte format is interoperable between the two drivers
//! ([`acme_pure::AccountCredentials`](crate::acme_pure::AccountCredentials)).

use std::future::Future;
use std::time::Duration;

use fauna_core::data::{DnsZoneRef, Timestamp};
use fauna_core::secret::SecretString;
use fauna_mls::wrapped_blob::format::TlsCertBundle;

use crate::{DnsProviderError, DnsProviderSeam, publish_acme_challenge, teardown_acme_challenge};

/// The **production** Let's Encrypt ACME directory URL (publicly-trusted certs).
/// A plain constant — the value `instant_acme::LetsEncrypt::Production.url()`
/// returns — so the shared [`Dns01OrderConfig`] type stays free of the
/// instant-acme enum and compiles on wasm. The wasm driver re-points this host at
/// the credential-blind CORS proxy (browsers can't reach LE cross-origin — no
/// CORS); the native driver hits it directly. Must match the cors-proxy
/// `acme-le` base URL (`services/fauna-cors-proxy/src/main.rs`).
pub const LETS_ENCRYPT_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// The Let's Encrypt **staging** ACME directory URL (untrusted certs; generous
/// rate limits) — for integration testing against the real CA. Matches the
/// cors-proxy `acme-le-staging` base URL.
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// Fixed wait after publishing `_acme-challenge` before signalling the challenge
/// ready — the **fallback** used only when no [`Dns01ResolvabilityProbe`] is
/// available (a test that injects none; every production driver now supplies one
/// — native in-process, wasm via the nest). With a probe the order
/// instead *actively polls* the zone's authoritative NS and proceeds the moment
/// every challenge TXT is really served (see [`PropagationGate`]), because a fixed
/// sleep of any length cannot guarantee a provider has applied the record —
/// Let's Encrypt marks a DNS-01 authorization `Invalid` almost immediately when
/// its first query finds no record; it does **not** forgivingly re-check.
///
/// **Why 180 s.** A conservative floor above the ~1–2 min a healthy provider takes
/// to serve a fresh TXT from its authoritative NS. Hetzner has been *measured*
/// slower (a zone-publish batch took ≥ 15 min on 2026-07-23), which is exactly why
/// the probe, not this constant, is the primary mechanism. Issuance is infrequent
/// (cert renewal), so the extra wait is immaterial. Tests set
/// [`Dns01OrderConfig::propagation_wait`] to zero.
pub const DEFAULT_PROPAGATION_WAIT: Duration = Duration::from_secs(180);

/// How often [`PropagationGate`] re-probes the authoritative NS for the published
/// challenge TXT(s). Sized for a renewal-scale operation: frequent enough to
/// proceed within seconds of the record landing, infrequent enough to stay polite
/// to the NS across a [`DEFAULT_RESOLVABILITY_DEADLINE`]-long worst case.
pub const DEFAULT_RESOLVABILITY_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// How long [`PropagationGate`] keeps probing before proceeding **best-effort**
/// (signalling ready anyway — the order then either validates from a vantage that
/// does see the record, or goes `Invalid` with the authorization-naming
/// diagnostic). Sized from the measured distribution of Hetzner's authoritative
/// zone-publish latency, which is batchy and **irregular**: ~15 min (SOA serial
/// at all 3 NS at 889 s, 2026-07-23), > 21 min for a fresh rrset the next day,
/// and one ~5 h outlier batch. 45 min covers everything but the pathological
/// tail. A pending ACME order tolerates far longer, so a generous deadline
/// costs nothing on the happy path — with the record served, the gate exits on
/// the first probe; only a genuinely-slow provider publish ever spends it.
pub const DEFAULT_RESOLVABILITY_DEADLINE: Duration = Duration::from_secs(45 * 60);

/// Inputs for a DNS-01 order.
#[derive(Debug, Clone)]
pub struct Dns01OrderConfig {
    /// ACME directory URL — [`LETS_ENCRYPT_PRODUCTION`] in production, a local
    /// pebble directory in real-wire tests. A plain string so this type's boundary
    /// stays free of the instant-acme `LetsEncrypt` enum (it must compile on wasm).
    pub directory_url: String,
    /// Contact email for a newly-created ACME account (the `mailto:` prefix is
    /// added internally). Unused when an existing account is supplied.
    pub contact_email: String,
    /// The SAN set to order — one cert covers all of them (e.g.
    /// `[apex, mail.<domain>]`). Every identifier must DNS-01-validate.
    pub domains: Vec<String>,
    /// How long to wait after publishing the `_acme-challenge` TXT(s) before
    /// signalling the challenge ready, when **no** resolvability probe is
    /// available (see [`DEFAULT_PROPAGATION_WAIT`]). With a probe this is
    /// skipped — the active poll below replaces it.
    pub propagation_wait: Duration,
    /// Active-poll cadence for the resolvability probe
    /// (see [`DEFAULT_RESOLVABILITY_POLL_INTERVAL`]). Unused without a probe.
    pub resolvability_poll_interval: Duration,
    /// Active-poll bound: after this long without the probe confirming every
    /// challenge TXT served, proceed best-effort
    /// (see [`DEFAULT_RESOLVABILITY_DEADLINE`]). Unused without a probe.
    pub resolvability_deadline: Duration,
    /// **CNAME-delegated renewal (S6b) publish redirects.** For each `(san,
    /// publish_name)` entry, that SAN's `_acme-challenge` TXT is published at the
    /// delegated `publish_name` (the `CnameDelegation.target_name`, inside a zone
    /// a held credential controls) instead of the default `_acme-challenge.<san>`
    /// in the SAN's own zone. The CA still queries `_acme-challenge.<san>` and
    /// follows the admin's one-time CNAME to `publish_name`. A SAN absent from
    /// this list publishes at its default name. Empty for an ordinary
    /// (non-delegated) order. The seam-driven `zone` the order publishes into is
    /// the credential's covering zone for the delegated `publish_name`, supplied
    /// by the caller (`DnsManagementMachine::issue_cert`).
    pub challenge_publish_names: Vec<(String, String)>,
}

impl Dns01OrderConfig {
    /// Order against the **production** Let's Encrypt directory (publicly-trusted
    /// certs) with the default propagation wait.
    pub fn lets_encrypt(contact_email: String, domains: Vec<String>) -> Self {
        Self {
            directory_url: LETS_ENCRYPT_PRODUCTION.to_string(),
            contact_email,
            domains,
            propagation_wait: DEFAULT_PROPAGATION_WAIT,
            resolvability_poll_interval: DEFAULT_RESOLVABILITY_POLL_INTERVAL,
            resolvability_deadline: DEFAULT_RESOLVABILITY_DEADLINE,
            challenge_publish_names: Vec::new(),
        }
    }

    /// Order against the Let's Encrypt **staging** directory (untrusted certs;
    /// generous rate limits) — for integration testing against the real CA.
    pub fn lets_encrypt_staging(contact_email: String, domains: Vec<String>) -> Self {
        Self {
            directory_url: LETS_ENCRYPT_STAGING.to_string(),
            contact_email,
            domains,
            propagation_wait: DEFAULT_PROPAGATION_WAIT,
            resolvability_poll_interval: DEFAULT_RESOLVABILITY_POLL_INTERVAL,
            resolvability_deadline: DEFAULT_RESOLVABILITY_DEADLINE,
            challenge_publish_names: Vec::new(),
        }
    }

    /// The publish owner name for `san`'s `_acme-challenge` TXT: the delegated
    /// `target_name` if one is configured (S6b), else the default
    /// `_acme-challenge.<san>` ([`crate::acme_challenge_record_name`]).
    pub(crate) fn publish_name_for(&self, san: &str) -> String {
        self.challenge_publish_names
            .iter()
            .find(|(d, _)| d == san)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| crate::acme_challenge_record_name(san))
    }
}

/// The product of a successful DNS-01 order. The caller (S5 orchestration) seals
/// `(cert_chain_pem, privkey_pem)` to the private nest via `seal_lan_tls_cert_entry`
/// and persists `account_credentials` BackupKey-sealed in `DnsConfig::acme_account` for reuse
/// across the admin's devices and the next renewal (D6).
pub struct Dns01Issued {
    /// PEM cert chain (leaf first) returned by the CA.
    pub cert_chain_pem: String,
    /// PEM private key for the issued leaf (generated here for the CSR). Flows
    /// immediately into the zeroizing `TlsCertBundle` at the seal boundary.
    pub privkey_pem: String,
    /// Serialized ACME `AccountCredentials` — the input if one was supplied, else
    /// the freshly-created account's credentials. Persist and pass back next time.
    pub account_credentials: Vec<u8>,
}

/// Failure modes of a DNS-01 order. Every variant is non-fatal to the deployment:
/// a failed issuance just leaves the nest on the Phase-2 self-signed floor
/// (`tls-certificates.md` § A) until the next attempt.
#[derive(Debug, thiserror::Error)]
pub enum Dns01Error {
    /// Loading/creating or restoring the ACME account failed.
    #[error("ACME account: {0}")]
    Account(String),
    /// A CA-side step failed (order creation, authorizations, set-ready, refresh,
    /// finalize, certificate fetch) or the order went `invalid`.
    #[error("ACME CA: {0}")]
    Ca(String),
    /// An authorization offered no DNS-01 challenge (the CA would not let us prove
    /// control of this name via DNS) — the bare domain is the payload.
    #[error("no dns-01 challenge offered for {0}")]
    NoChallenge(String),
    /// Generating the keypair or serializing the CSR failed.
    #[error("certificate signing request: {0}")]
    Csr(String),
    /// Publishing an `_acme-challenge` TXT through the provider seam failed.
    #[error("publish _acme-challenge: {0}")]
    Publish(#[from] DnsProviderError),
    /// The order reached `valid` before we finalized — so no private key was
    /// generated on this device. Unreachable for a freshly-created order; treated
    /// as an error rather than returning a keyless bundle.
    #[error("order reached `valid` before finalize — no private key was generated")]
    ValidBeforeFinalize,
    /// The issued leaf certificate could not be parsed to read its `notAfter`
    /// (needed for the `TlsCertBundle.expires_at` the consumer renews against).
    #[error("parse issued certificate: {0}")]
    CertParse(String),
}

// `fauna-acme-core` is a `not(target_arch = "wasm32")`-only dependency
// (Cargo.toml § native-gated ACME deps: `instant-acme`/`rcgen`'s crypto isn't
// browser-WASM-safe) — this impl doesn't exist for wasm32 either, since
// `mod acme_shared` itself is declared unconditionally in lib.rs.
#[cfg(not(target_arch = "wasm32"))]
impl From<fauna_acme_core::FinalizeError> for Dns01Error {
    fn from(e: fauna_acme_core::FinalizeError) -> Self {
        match e {
            fauna_acme_core::FinalizeError::Csr(s) => Dns01Error::Csr(s),
            fauna_acme_core::FinalizeError::Ca(s) => Dns01Error::Ca(s),
        }
    }
}

/// One DNS-01 challenge to present: the `_acme-challenge.<domain>` TXT value plus
/// the CA challenge URL to mark ready once it is published. Constructed by each
/// driver's `collect_dns01_challenges`; consumed by [`with_published_challenges`]
/// and the suspended-order `challenges_to_publish` projections.
#[derive(Debug, Clone)]
pub(crate) struct Dns01Challenge {
    /// Bare domain the authorization covers (the CA validates by querying
    /// `_acme-challenge.<domain>`; that query is not under our control).
    pub(crate) domain: String,
    /// The owner name we **publish** the `_acme-challenge` TXT at:
    /// `_acme-challenge.<domain>` by default, or the delegated
    /// `CnameDelegation.target_name` when this domain's renewal is CNAME-delegated
    /// into a controlled zone (S6b — [`Dns01OrderConfig::challenge_publish_names`]).
    pub(crate) publish_name: String,
    /// `key_authorization(challenge).dns_value()` — the TXT's raw value.
    pub(crate) dns_value: String,
    /// CA challenge URL passed to `set_challenge_ready`.
    pub(crate) challenge_url: String,
}

/// Build the sealable [`TlsCertBundle`] from a finished DNS-01 order (S5): the
/// PEM cert chain + private key become the bundle bytes, `expires_at` is parsed
/// from the issued **leaf** certificate's `notAfter` (mirroring the nest's
/// `acme_http01.rs::cert_seconds_remaining` x509 parse — the consumer renews
/// against it), and `issued_at` is the current wall-clock second. The private
/// key zeroizes on the returned bundle's drop. The S5 orchestration seals this
/// to the private nest with `fauna_mls::wrapped_blob::seal_lan_tls_cert_entry`.
pub fn tls_cert_bundle_from_issued(issued: &Dns01Issued) -> Result<TlsCertBundle, Dns01Error> {
    let expires_at = leaf_not_after_secs(issued.cert_chain_pem.as_bytes()).ok_or_else(|| {
        Dns01Error::CertParse("issued cert has no parseable leaf notAfter".to_string())
    })?;
    Ok(TlsCertBundle {
        cert_chain: issued.cert_chain_pem.clone().into_bytes(),
        priv_key: issued.privkey_pem.clone().into_bytes(),
        expires_at,
        issued_at: Timestamp::now_secs().max(0) as u64,
    })
}

/// Parse the leaf (first PEM block) certificate's `notAfter` as a Unix second.
/// `None` on any parse failure or a pre-epoch `notAfter`.
fn leaf_not_after_secs(pem_data: &[u8]) -> Option<u64> {
    use x509_parser::pem::parse_x509_pem;
    let (_, pem) = parse_x509_pem(pem_data).ok()?;
    let cert = pem.parse_x509().ok()?;
    u64::try_from(cert.validity().not_after.timestamp()).ok()
}

/// Answers "is this challenge TXT really being **served**?" for the active
/// propagation poll ([`PropagationGate`]). Injectable so the gate is
/// unit-testable with a fake; the production native impl
/// ([`crate::resolvability::AuthoritativeNsProbe`]) queries every authoritative
/// NS of the publish zone directly — the only honest signal, since a provider's
/// control plane can claim a record it does not serve (the Hetzner
/// doubled-owner incident, 2026-07-23/24) and recursive resolvers negative-cache
/// a miss for up to the zone's SOA minimum. Wasm cannot run that query itself
/// (no raw DNS in the browser), so it supplies
/// [`NestRelayedProbe`](crate::NestRelayedProbe) instead — the *same* query, run
/// nest-side over `fauna.dns.probe_txt_visible`. The fixed
/// [`Dns01OrderConfig::propagation_wait`] is now only the no-probe fallback
/// (tests, and a caller that deliberately injects `None`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Dns01ResolvabilityProbe: crate::MaybeSendSync {
    /// `true` iff every authoritative NS of `zone_name` serves a TXT record at
    /// `record_name` whose value is exactly `txt_value`. Any failure (NS
    /// discovery, timeout, wrong value) is `false` — "not visible yet"; the
    /// gate's deadline bounds the retries.
    async fn txt_visible(&self, zone_name: &str, record_name: &str, txt_value: &str) -> bool;
}

/// How [`with_published_challenges`] waits, after publishing, for the challenge
/// TXT(s) to become CA-visible before running the CA dance.
///
/// With a probe: poll until [`Dns01ResolvabilityProbe::txt_visible`] holds for
/// **every** challenge, then proceed immediately — no fixed wait. After
/// `deadline` without that, warn and proceed **best-effort** (the order then
/// either validates from a vantage that does see the records, or goes `Invalid`
/// carrying the authorization-naming diagnostic — no worse than today, and the
/// stray TXT is torn down either way). Without a probe: sleep `fixed_wait`
/// (the pre-probe behavior, still the wasm path).
pub(crate) struct PropagationGate<'a> {
    pub probe: Option<&'a dyn Dns01ResolvabilityProbe>,
    pub fixed_wait: Duration,
    pub poll_interval: Duration,
    pub deadline: Duration,
}

impl PropagationGate<'_> {
    /// A no-wait gate for tests and for callers whose ca-dance still owns its
    /// own wait (the manual-mode path).
    #[cfg(test)]
    pub(crate) fn zero() -> Self {
        Self {
            probe: None,
            fixed_wait: Duration::ZERO,
            poll_interval: Duration::ZERO,
            deadline: Duration::ZERO,
        }
    }

    /// The gate for **manual mode** ([`crate::acme_order::complete_dns01_order`]),
    /// where there is no `Dns01OrderConfig` left to read — the order was configured
    /// in phase 1 and suspended while the admin pasted the TXT at their registrar.
    ///
    /// Uses the same active-poll bounds as the managed path: the admin's "I
    /// published it" is a claim about their *control plane*, not evidence their
    /// authoritative NS serves the record yet, and that race is precisely the
    /// 2026-07-24 live failure (first attempt always lost it). `fixed_wait` stays
    /// the caller's, which for the manual path is `Duration::ZERO` — so before
    /// wasm had a probe (2026-08-22) the browser's complete-button signalled ready
    /// with no wait *and* no check. It now supplies
    /// [`NestRelayedProbe`](crate::NestRelayedProbe), so both drivers gate here.
    pub(crate) fn manual<'a>(
        probe: Option<&'a dyn Dns01ResolvabilityProbe>,
        fixed_wait: Duration,
    ) -> PropagationGate<'a> {
        PropagationGate {
            probe,
            fixed_wait,
            poll_interval: DEFAULT_RESOLVABILITY_POLL_INTERVAL,
            deadline: DEFAULT_RESOLVABILITY_DEADLINE,
        }
    }

    /// Build from an order config + optional probe (the driver entry points).
    pub(crate) fn from_config<'a>(
        cfg: &Dns01OrderConfig,
        probe: Option<&'a dyn Dns01ResolvabilityProbe>,
    ) -> PropagationGate<'a> {
        PropagationGate {
            probe,
            fixed_wait: cfg.propagation_wait,
            poll_interval: cfg.resolvability_poll_interval,
            deadline: cfg.resolvability_deadline,
        }
    }

    /// Run the gate for `challenges` published into `zone_name`.
    pub(crate) async fn wait(&self, zone_name: &str, challenges: &[Dns01Challenge]) {
        let Some(probe) = self.probe else {
            if !self.fixed_wait.is_zero() {
                sleep_cross_target(self.fixed_wait).await;
            }
            return;
        };
        // Deadline as a poll count, not an `Instant` — `std::time::Instant`
        // panics on wasm32-unknown-unknown, and a count keeps the gate
        // target-independent should a wasm probe ever exist.
        let max_polls = if self.poll_interval.is_zero() {
            1
        } else {
            (self.deadline.as_millis() / self.poll_interval.as_millis().max(1)).max(1) as u64
        };
        for attempt in 0..max_polls {
            let mut all_visible = true;
            for ch in challenges {
                if !probe
                    .txt_visible(zone_name, &ch.publish_name, &ch.dns_value)
                    .await
                {
                    all_visible = false;
                    break;
                }
            }
            if all_visible {
                tracing::info!(
                    attempt,
                    "acme dns-01: every challenge TXT confirmed served by the \
                     authoritative NS; proceeding to CA validation"
                );
                return;
            }
            sleep_cross_target(self.poll_interval).await;
        }
        tracing::warn!(
            deadline_secs = self.deadline.as_secs(),
            "acme dns-01: challenge TXT(s) still not confirmed served at the \
             resolvability deadline; signalling ready best-effort"
        );
    }
}

/// Cross-target async sleep. The `#[cfg]` split — and the twin copies in
/// `acme_pure` and the onboarding machines this doc used to point at — now live
/// once, in `fauna-sleep`.
async fn sleep_cross_target(d: Duration) {
    fauna_sleep::sleep(d).await;
}

/// Publish every challenge's `_acme-challenge` TXT, await `ca_dance` (the CA-side
/// set-ready → poll → finalize → fetch sequence each driver supplies), and
/// **always** tear the TXTs down afterward — even when `ca_dance` errors or a
/// `publish` fails partway.
///
/// Generic over the CA-dance future so both drivers (native `instant-acme`, wasm
/// `acme_pure`) plug their own order-driving into the one publish-then-teardown
/// choreography — the load-bearing "teardown always, even on failure" invariant —
/// which is unit-testable with a fake seam and a trivial `ca_dance`, without a live
/// CA. A stranded `_acme-challenge` TXT is harmless (the CA accepts any matching
/// value — D3) but accumulates, so teardown is best-effort: a teardown error is
/// logged, never allowed to mask the order result, and `ca_dance` runs only if
/// every publish succeeded.
pub(crate) async fn with_published_challenges<T, F>(
    seam: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    challenges: &[Dns01Challenge],
    gate: PropagationGate<'_>,
    ca_dance: F,
) -> Result<T, Dns01Error>
where
    F: Future<Output = Result<T, Dns01Error>>,
{
    let mut publish_result = Ok(());
    for ch in challenges {
        if let Err(e) = publish_acme_challenge(
            seam,
            provider_id,
            fields,
            zone,
            &ch.publish_name,
            &ch.dns_value,
        )
        .await
        {
            publish_result = Err(Dns01Error::from(e));
            break;
        }
    }

    // Run the CA dance only if every TXT published; otherwise short-circuit to the
    // teardown below (which no-ops on the ones that never landed). The propagation
    // gate (active probe poll, or the fixed-wait fallback) runs between the two, so
    // the CA is only signalled once the TXT(s) are confirmed served — or the gate's
    // deadline forces a best-effort attempt.
    let result = match publish_result {
        Ok(()) => {
            gate.wait(&zone.name, challenges).await;
            ca_dance.await
        }
        Err(e) => Err(e),
    };

    for ch in challenges {
        if let Err(e) = teardown_acme_challenge(
            seam,
            provider_id,
            fields,
            zone,
            &ch.publish_name,
            &ch.dns_value,
        )
        .await
        {
            tracing::warn!(
                domain = %ch.domain,
                error = %e,
                "acme dns-01: failed to tear down _acme-challenge TXT (harmless; the next order overwrites it)"
            );
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PublishRecord;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// Records publish/teardown calls (and can fail publish) so the choreography
    /// tests assert what reached the provider seam without a live CA. Mirrors the
    /// `FakeProvider` in `lib.rs`'s tests, kept local to this module.
    #[derive(Default)]
    struct RecordingSeam {
        published: Mutex<Vec<(String, Vec<PublishRecord>)>>,
        torn_down: Mutex<Vec<(String, Vec<PublishRecord>)>>,
        fail_publish: bool,
    }
    impl RecordingSeam {
        fn new() -> Self {
            Self::default()
        }
        fn failing_publish() -> Self {
            Self {
                fail_publish: true,
                ..Self::default()
            }
        }
    }
    #[async_trait]
    impl DnsProviderSeam for RecordingSeam {
        async fn verify(
            &self,
            _: &str,
            _: &[(String, SecretString)],
        ) -> Result<Vec<DnsZoneRef>, DnsProviderError> {
            Ok(vec![])
        }
        async fn publish(
            &self,
            _: &str,
            _: &[(String, SecretString)],
            zone: &DnsZoneRef,
            records: &[PublishRecord],
        ) -> Result<(), DnsProviderError> {
            if self.fail_publish {
                return Err(DnsProviderError::Rejected("publish boom".into()));
            }
            self.published
                .lock()
                .unwrap()
                .push((zone.name.clone(), records.to_vec()));
            Ok(())
        }
        async fn teardown(
            &self,
            _: &str,
            _: &[(String, SecretString)],
            zone: &DnsZoneRef,
            records: &[PublishRecord],
        ) -> Result<(), DnsProviderError> {
            self.torn_down
                .lock()
                .unwrap()
                .push((zone.name.clone(), records.to_vec()));
            Ok(())
        }
        async fn find_records(
            &self,
            _: &str,
            _: &[(String, SecretString)],
            _: &DnsZoneRef,
            _: &str,
            _: &str,
        ) -> Result<Vec<PublishRecord>, DnsProviderError> {
            // This module's tests exercise the `_acme-challenge` publish/teardown
            // choreography, not the TLSA converge — nothing published to find.
            Ok(vec![])
        }
    }

    fn zone() -> DnsZoneRef {
        DnsZoneRef {
            id: "z1".to_string(),
            name: "example.com".to_string(),
        }
    }

    fn challenges() -> Vec<Dns01Challenge> {
        vec![
            Dns01Challenge {
                domain: "example.com".to_string(),
                publish_name: "_acme-challenge.example.com".to_string(),
                dns_value: "value-apex".to_string(),
                challenge_url: "https://ca/chal/1".to_string(),
            },
            Dns01Challenge {
                domain: "mail.example.com".to_string(),
                publish_name: "_acme-challenge.mail.example.com".to_string(),
                dns_value: "value-mail".to_string(),
                challenge_url: "https://ca/chal/2".to_string(),
            },
        ]
    }

    /// On success: every challenge's `_acme-challenge` TXT is published (right name
    /// + raw value), the CA dance runs, and every TXT is torn back down.
    #[tokio::test]
    async fn challenges_published_then_torn_down_on_success() {
        let seam = RecordingSeam::new();
        let chals = challenges();
        let out = with_published_challenges(
            &seam,
            "cloudflare",
            &[],
            &zone(),
            &chals,
            PropagationGate::zero(),
            async { Ok::<u32, Dns01Error>(42) },
        )
        .await
        .expect("order succeeds");
        assert_eq!(out, 42);

        let published = seam.published.lock().unwrap();
        let torn = seam.torn_down.lock().unwrap();
        assert_eq!(published.len(), 2, "both TXTs published");
        assert_eq!(torn.len(), 2, "both TXTs torn down");
        assert_eq!(published[0].1[0].name, "_acme-challenge.example.com");
        assert_eq!(published[0].1[0].value, "value-apex");
        assert_eq!(published[0].1[0].ttl_seconds, 120);
        assert_eq!(published[1].1[0].name, "_acme-challenge.mail.example.com");
        assert_eq!(published[1].1[0].value, "value-mail");
        // Teardown retracts the same records (value-based inverse).
        assert_eq!(torn[0].1[0].name, "_acme-challenge.example.com");
        assert_eq!(torn[1].1[0].name, "_acme-challenge.mail.example.com");
    }

    /// A probe that reports not-visible for its first `visible_after` calls,
    /// then visible — the behavioural fake for the propagation gate.
    struct FakeProbe {
        calls: std::sync::atomic::AtomicU32,
        visible_after: u32,
    }
    impl FakeProbe {
        fn visible_after(n: u32) -> Self {
            Self {
                calls: std::sync::atomic::AtomicU32::new(0),
                visible_after: n,
            }
        }
        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
    }
    #[async_trait]
    impl Dns01ResolvabilityProbe for FakeProbe {
        async fn txt_visible(&self, _zone: &str, _name: &str, _value: &str) -> bool {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            n >= self.visible_after
        }
    }

    /// With a probe, the gate polls until every challenge TXT is visible and
    /// then proceeds — no fixed wait involved (the fixed wait is deliberately
    /// poisoned huge here; a green run proves it was never slept).
    #[tokio::test]
    async fn gate_proceeds_once_probe_confirms_visibility() {
        let seam = RecordingSeam::new();
        let chals = challenges();
        let probe = FakeProbe::visible_after(3);
        let gate = PropagationGate {
            probe: Some(&probe),
            fixed_wait: Duration::from_secs(3600),
            poll_interval: Duration::from_millis(1),
            deadline: Duration::from_secs(1),
        };
        let out =
            with_published_challenges(&seam, "cloudflare", &[], &zone(), &chals, gate, async {
                Ok::<u32, Dns01Error>(7)
            })
            .await
            .expect("order succeeds after the probe confirms");
        assert_eq!(out, 7);
        assert!(
            probe.calls() >= 4,
            "the gate kept polling through the not-visible phase (got {} calls)",
            probe.calls()
        );
        assert_eq!(
            seam.torn_down.lock().unwrap().len(),
            2,
            "teardown still runs"
        );
    }

    /// A never-visible probe exhausts the deadline and the gate proceeds
    /// **best-effort** — the CA dance still runs (and teardown still follows),
    /// rather than the order silently never being attempted.
    #[tokio::test]
    async fn gate_deadline_proceeds_best_effort() {
        let seam = RecordingSeam::new();
        let chals = challenges();
        let probe = FakeProbe::visible_after(u32::MAX);
        let gate = PropagationGate {
            probe: Some(&probe),
            fixed_wait: Duration::from_secs(3600),
            poll_interval: Duration::from_millis(1),
            deadline: Duration::from_millis(10),
        };
        let ran = AtomicBool::new(false);
        with_published_challenges(&seam, "cloudflare", &[], &zone(), &chals, gate, async {
            ran.store(true, Ordering::SeqCst);
            Ok::<(), Dns01Error>(())
        })
        .await
        .expect("best-effort attempt still runs the CA dance");
        assert!(
            ran.load(Ordering::SeqCst),
            "ca_dance ran despite the deadline"
        );
        assert_eq!(
            probe.calls(),
            10,
            "the gate polled exactly deadline/interval times before giving way"
        );
    }

    /// **Manual mode gets the same active gate as managed.** The admin pasted the
    /// TXT at their registrar and pressed complete; "the admin says it is live" is
    /// not evidence the *authoritative NS* serves it yet, so the manual gate must
    /// poll exactly like the managed one before the CA is signalled.
    ///
    /// Regression guard on the live 2026-07-24 bug: `complete_manual_issue` passed
    /// `Duration::ZERO` and no probe, so the CA was told to validate immediately
    /// and the first attempt always lost the race with the registrar's publish.
    #[tokio::test]
    async fn manual_gate_polls_probe_until_visible() {
        let chals = challenges();
        let probe = FakeProbe::visible_after(3);
        let mut gate = PropagationGate::manual(Some(&probe), Duration::ZERO);
        gate.poll_interval = Duration::from_millis(1);
        gate.deadline = Duration::from_secs(1);
        gate.wait("example.com", &chals).await;
        assert!(
            probe.calls() >= 4,
            "the manual gate kept polling through the not-visible phase (got {} calls)",
            probe.calls()
        );
    }

    /// The manual gate's poll bounds are the **real** ones, not the degenerate
    /// zeros the bug shipped: a `deadline`/`poll_interval` of zero collapses
    /// `PropagationGate::wait` to a single probe call, which is barely better than
    /// no gate at all on a provider measured at 15–30 min.
    #[test]
    fn manual_gate_bounds_are_not_degenerate() {
        let probe = FakeProbe::visible_after(0);
        let gate = PropagationGate::manual(Some(&probe), Duration::ZERO);
        assert_eq!(gate.deadline, DEFAULT_RESOLVABILITY_DEADLINE);
        assert_eq!(gate.poll_interval, DEFAULT_RESOLVABILITY_POLL_INTERVAL);
    }

    /// The probe-less manual path (wasm today) keeps **exactly** its pre-fix
    /// behavior: the caller's fixed wait, unchanged. The wasm probe gap is a
    /// separate queued track; this fix must not
    /// silently introduce a blind sleep into the browser's complete-button.
    #[tokio::test]
    async fn manual_gate_without_probe_keeps_the_callers_fixed_wait() {
        let chals = challenges();
        let gate = PropagationGate::manual(None, Duration::ZERO);
        assert_eq!(gate.fixed_wait, Duration::ZERO);
        // Returns immediately — no probe, zero fixed wait (today's wasm path).
        gate.wait("example.com", &chals).await;
    }

    /// Regression guard on the **fallback** fixed wait (the probe-less path —
    /// wasm today): Let's Encrypt marks a DNS-01 authorization `Invalid` on the
    /// first failed query rather than re-checking forgivingly, so wherever the
    /// fixed wait still applies it must stay comfortably above a healthy
    /// provider's ~1–2 min authoritative propagation. (The primary mechanism is
    /// now the active resolvability poll — `PropagationGate` + the behavioural
    /// fake-probe tests above; this floor covers only the no-probe arm.)
    #[test]
    fn default_propagation_wait_exceeds_observed_provider_floor() {
        assert!(
            DEFAULT_PROPAGATION_WAIT >= Duration::from_secs(120),
            "DEFAULT_PROPAGATION_WAIT {DEFAULT_PROPAGATION_WAIT:?} is below the ~1–2 min \
             Hetzner authoritative-NS propagation floor that made a live DNS-01 order fail \
             on 2026-07-23; the fixed wait must exceed provider propagation, not merely start it"
        );
    }

    /// A CNAME-delegated renewal (S6b) publishes the `_acme-challenge` TXT at the
    /// delegated `publish_name` (inside the controlled zone), not at
    /// `_acme-challenge.<domain>` — the CA follows the admin's one-time CNAME.
    #[tokio::test]
    async fn delegated_challenge_publishes_at_target_name() {
        let seam = RecordingSeam::new();
        // A single-SAN order whose challenge is re-homed into a controlled zone.
        let chals = vec![Dns01Challenge {
            domain: "home.example.test".to_string(),
            publish_name: "_acme-challenge.home.example.test.controlled.example".to_string(),
            dns_value: "delegated-val".to_string(),
            challenge_url: "https://ca/chal/1".to_string(),
        }];
        let controlled_zone = DnsZoneRef {
            id: "zc".to_string(),
            name: "controlled.example".to_string(),
        };
        with_published_challenges(
            &seam,
            "cloudflare",
            &[],
            &controlled_zone,
            &chals,
            PropagationGate::zero(),
            async { Ok::<(), Dns01Error>(()) },
        )
        .await
        .expect("order succeeds");

        let published = seam.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        // Published into the controlled zone, at the delegated target name.
        assert_eq!(published[0].0, "controlled.example");
        assert_eq!(
            published[0].1[0].name,
            "_acme-challenge.home.example.test.controlled.example"
        );
        assert_eq!(published[0].1[0].value, "delegated-val");
        assert_eq!(
            seam.torn_down.lock().unwrap()[0].1[0].name,
            "_acme-challenge.home.example.test.controlled.example"
        );
    }

    /// `publish_name_for` returns the delegated target for a configured SAN and
    /// the default `_acme-challenge.<san>` for an unconfigured one.
    #[test]
    fn publish_name_for_honours_delegation_else_defaults() {
        let mut cfg =
            Dns01OrderConfig::lets_encrypt(String::new(), vec!["home.example.test".to_string()]);
        cfg.challenge_publish_names = vec![(
            "home.example.test".to_string(),
            "_acme-challenge.home.example.test.controlled.example".to_string(),
        )];
        assert_eq!(
            cfg.publish_name_for("home.example.test"),
            "_acme-challenge.home.example.test.controlled.example"
        );
        assert_eq!(
            cfg.publish_name_for("other.example.test"),
            "_acme-challenge.other.example.test"
        );
    }

    /// The TXTs are torn down even when the CA dance fails — a failed order must
    /// not strand the transient challenge records.
    #[tokio::test]
    async fn challenges_torn_down_even_when_ca_dance_fails() {
        let seam = RecordingSeam::new();
        let chals = challenges();
        let err = with_published_challenges(
            &seam,
            "cloudflare",
            &[],
            &zone(),
            &chals,
            PropagationGate::zero(),
            async { Err::<(), Dns01Error>(Dns01Error::Ca("order invalid".to_string())) },
        )
        .await
        .expect_err("order fails");
        assert!(matches!(err, Dns01Error::Ca(_)));
        assert_eq!(seam.published.lock().unwrap().len(), 2);
        assert_eq!(
            seam.torn_down.lock().unwrap().len(),
            2,
            "teardown runs despite the CA failure"
        );
    }

    /// A publish failure short-circuits before the CA dance runs, still attempts
    /// teardown, and surfaces as `Dns01Error::Publish`.
    #[tokio::test]
    async fn publish_failure_skips_ca_dance_but_tears_down() {
        let seam = RecordingSeam::failing_publish();
        let chals = challenges();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in = ran.clone();
        let err = with_published_challenges(
            &seam,
            "cloudflare",
            &[],
            &zone(),
            &chals,
            PropagationGate::zero(),
            async move {
                ran_in.store(true, Ordering::SeqCst);
                Ok::<(), Dns01Error>(())
            },
        )
        .await
        .expect_err("publish fails");
        assert!(matches!(err, Dns01Error::Publish(_)));
        assert!(!ran.load(Ordering::SeqCst), "CA dance must not run");
        // Teardown is still attempted (idempotent no-op on the never-published TXTs).
        assert_eq!(seam.torn_down.lock().unwrap().len(), 2);
    }

    /// `tls_cert_bundle_from_issued` carries the PEM bytes through verbatim and
    /// parses `expires_at` from the leaf's `notAfter`. Uses an rcgen self-signed
    /// cert with a known `notAfter` so the parse is asserted exactly. (`rcgen` is a
    /// native-only dep, and `#[cfg(test)]` only builds on the native test runner,
    /// so this stays out of the wasm build.)
    #[test]
    fn bundle_from_issued_parses_leaf_not_after() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params =
            rcgen::CertificateParams::new(vec!["home.example.com".to_string()]).unwrap();
        let not_after = rcgen::date_time_ymd(2099, 1, 1);
        params.not_after = not_after;
        let cert = params.self_signed(&key).unwrap();
        let issued = Dns01Issued {
            cert_chain_pem: cert.pem(),
            privkey_pem: key.serialize_pem(),
            account_credentials: b"creds".to_vec(),
        };
        let bundle = tls_cert_bundle_from_issued(&issued).expect("bundle built");
        assert_eq!(bundle.expires_at, not_after.unix_timestamp() as u64);
        assert_eq!(bundle.cert_chain, issued.cert_chain_pem.as_bytes());
        assert_eq!(bundle.priv_key, issued.privkey_pem.as_bytes());
        assert!(bundle.issued_at > 0, "issued_at is the current second");
    }

    /// A cert chain that does not parse as PEM/X.509 yields `CertParse` rather
    /// than a bundle with a bogus expiry.
    #[test]
    fn bundle_from_issued_errors_on_unparseable_cert() {
        let issued = Dns01Issued {
            cert_chain_pem: "not a certificate".to_string(),
            privkey_pem: "k".to_string(),
            account_credentials: vec![],
        };
        assert!(matches!(
            tls_cert_bundle_from_issued(&issued),
            Err(Dns01Error::CertParse(_))
        ));
    }
}
