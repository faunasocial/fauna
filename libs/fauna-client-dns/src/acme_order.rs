//! The **native** client-driven DNS-01 ACME order driver (Phase 3, S4 — "the
//! heart"), over `instant-acme`.
//!
//! A nest behind NAT (or on a registrar without an HTTP-01-reachable port) cannot
//! run ACME itself, and the product invariant forbids it ever holding the
//! DNS-provider key (`dns-management.md` § Where the credential lives). So the
//! **admin's client** — which holds the key — drives the order here: publish the
//! transient `_acme-challenge.<domain>` TXT through the shared
//! [`DnsProviderSeam`], let the CA validate, finalize, and hand the issued
//! `(cert_chain_pem, privkey_pem)` back to the caller (S5), which seals it to the
//! private nest over the existing namespace-sync channel
//! (`fauna_mls::wrapped_blob::lan_cert::seal_lan_tls_cert_entry`). The nest never
//! writes DNS and never runs the order.
//!
//! **This mirrors the nest's HTTP-01 loop** (`fauna_acme_http01::obtain_certificate`)
//! — account → order → per-authorization challenge → poll → finalize → fetch —
//! with three DNS-01 deltas:
//! 1. select [`ChallengeType::Dns01`] and present `key_authorization(c).dns_value()`
//!    (the base64url SHA-256 digest), not the raw HTTP-01 token;
//! 2. present via [`publish_acme_challenge`]/[`teardown_acme_challenge`] over the
//!    injected [`DnsProviderSeam`] (the same path every managed record takes — not
//!    a parallel one), wrapped in [`with_published_challenges`] so the TXT is
//!    **always** torn down, even on failure;
//! 3. a DNS-propagation wait before signalling the challenge ready (HTTP-01 is
//!    instant; a TXT must reach the authoritative NS first).
//!
//! The tail past that point — once the order is `Ready`: keypair, CSR, finalize,
//! poll for the issued certificate — is not just mirrored but literally
//! **shared** with the HTTP-01 loop, via [`fauna_acme_core::finalize_order_and_fetch_certificate`]
//! ([`finalize_and_fetch`] below is a thin wrapper over it).
//!
//! **Native-only (D2).** `instant-acme`'s crypto is hard-wired to `ring`/`aws-lc-rs`
//! (account-key JWS) and `rcgen` (CSR), neither browser-WASM-safe — so this module
//! is `#[cfg(not(target_arch = "wasm32"))]` and its deps are native-gated. The wasm
//! twin — same ACME v2 protocol on RustCrypto + reqwest — is
//! [`acme_pure`](crate::acme_pure); both drivers share the order data types, the
//! publish/teardown choreography, and the issued-cert bundle helpers via
//! [`acme_shared`](crate::acme_shared) (everything that is **not** the
//! instant-acme order-driving below).
//!
//! **Account reuse (D6).** The order threads the ACME `AccountCredentials` in and
//! back out (`Option<&[u8]>` → `Vec<u8>`); the caller persists them BackupKey-sealed
//! in `DnsConfig::acme_account` (`fauna.state.dns`), reused across the admin's devices and renewals (so any synced
//! device renews against the *same* account — `tls-certificates.md` § C mitigation
//! 3 — without churning new accounts against the CA's rate limits). A `None`
//! creates a fresh account and returns its credentials to persist.

use std::time::Duration;

use fauna_core::data::DnsZoneRef;
use fauna_core::secret::SecretString;

use crate::DnsProviderSeam;
use crate::acme_shared::{
    Dns01Challenge, Dns01Error, Dns01Issued, Dns01OrderConfig, Dns01ResolvabilityProbe,
    PropagationGate, with_published_challenges,
};

/// A DNS-01 order opened (account established, order created, per-SAN challenges
/// collected) but **not yet validated** — the suspendable handle for **manual
/// mode** (tier 3), where the admin pastes the `_acme-challenge` TXT(s) into their
/// registrar out-of-band between [`begin_dns01_order`] and [`complete_dns01_order`].
///
/// Managed mode (tier 2) never holds this open across a pause — it publishes the
/// challenges through the [`DnsProviderSeam`] and completes in a single
/// [`obtain_certificate_dns01`] call. Manual mode cannot: the challenge value is
/// order-specific (`order.key_authorization(c).dns_value()`), so the order must
/// stay open while the admin acts. The live [`instant_acme::Order`] survives the
/// pause — it owns an `Arc` of the account plus the order URL + state, and the CA
/// keeps the order's authorizations + challenge tokens valid for the order's
/// lifetime (nonces refresh on demand inside instant-acme).
///
/// Native-only (the module is native-gated — D2). The wasm machine holds the
/// `acme_pure` twin ([`acme_pure::Dns01OrderInProgress`](crate::acme_pure::Dns01OrderInProgress));
/// `lib.rs` re-exports whichever matches the target under the one name.
pub struct Dns01OrderInProgress {
    order: instant_acme::Order,
    account_credentials: Vec<u8>,
    challenges: Vec<Dns01Challenge>,
    domains: Vec<String>,
}

impl Dns01OrderInProgress {
    /// The transient `_acme-challenge.<domain>` TXT record(s) to present — one per
    /// still-pending SAN (a reused account's cached-`Valid` authorizations need
    /// none). Managed mode publishes these through the seam; manual mode shows them
    /// for the admin to paste. The record shape is the single shared builder
    /// ([`acme_challenge_publish_record`](crate::acme_challenge_publish_record)) —
    /// the same `_acme-challenge` row the managed path writes (`tls-certificates.md`
    /// § "The `_acme-challenge` record" — one writer, never a parallel path).
    pub fn challenges_to_publish(&self) -> Vec<crate::PublishRecord> {
        self.challenges
            .iter()
            .map(|c| crate::acme_challenge_publish_record_named(&c.publish_name, &c.dns_value))
            .collect()
    }

    /// The (possibly newly-created) ACME account's serialized credentials — the
    /// same bytes [`Dns01Issued::account_credentials`] carries on completion.
    /// The **manual** path needs this *before* completion too: a suspended
    /// order can outlive the process (an app restart, a device switch —
    /// `tls-certificates.md` § *Surviving an interrupted manual issuance*), and
    /// a resume that creates a fresh account instead of restoring this one gets
    /// a fresh authorization from the CA — RFC 8555 §7.4 scopes pending-
    /// authorization reuse to the *same* account, so the resume's "the CA asks
    /// for the same challenge" fast path silently never fires without this.
    pub fn account_credentials(&self) -> &[u8] {
        &self.account_credentials
    }
}

/// Phase 1 of a DNS-01 order: establish the ACME account (reuse the persisted one
/// or create a fresh one — D6), open the order over the SAN set, and collect each
/// pending authorization's `_acme-challenge` TXT. Returns the suspendable
/// [`Dns01OrderInProgress`]; the caller then presents the challenges (managed:
/// [`obtain_certificate_dns01`] publishes them via the seam and finalizes in one
/// call; manual: show them to paste, then [`complete_dns01_order`] once the admin
/// confirms). Native-only — see the module docs.
pub async fn begin_dns01_order(
    cfg: &Dns01OrderConfig,
    account_credentials: Option<&[u8]>,
) -> Result<Dns01OrderInProgress, Dns01Error> {
    let (account, account_credentials) = load_or_create_account(cfg, account_credentials).await?;
    open_order_and_collect(account, account_credentials, cfg).await
}

/// As [`begin_dns01_order`], but the ACME account/directory HTTP calls go through
/// the caller-supplied `http` client — the pebble real-wire entry (S4b); see
/// [`obtain_certificate_dns01_with_http`]. `test-helpers` only.
#[cfg(feature = "test-helpers")]
pub async fn begin_dns01_order_with_http(
    cfg: &Dns01OrderConfig,
    account_credentials: Option<&[u8]>,
    http: Box<dyn instant_acme::HttpClient>,
) -> Result<Dns01OrderInProgress, Dns01Error> {
    let (account, account_credentials) =
        load_or_create_account_with_http(cfg, account_credentials, http).await?;
    open_order_and_collect(account, account_credentials, cfg).await
}

/// Open the order over the full SAN set and collect each pending authorization's
/// DNS-01 challenge — the account-agnostic body shared by [`begin_dns01_order`] and
/// (under `test-helpers`) [`begin_dns01_order_with_http`], so the only production /
/// pebble difference is which HTTP client backs the account.
async fn open_order_and_collect(
    account: instant_acme::Account,
    account_credentials: Vec<u8>,
    cfg: &Dns01OrderConfig,
) -> Result<Dns01OrderInProgress, Dns01Error> {
    use instant_acme::{Identifier, NewOrder};

    // Open the order over the full SAN set.
    let identifiers: Vec<Identifier> = cfg
        .domains
        .iter()
        .map(|d| Identifier::Dns(d.clone()))
        .collect();
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .map_err(|e| Dns01Error::Ca(format!("create order: {e}")))?;

    // Collect the DNS-01 challenge each pending authorization needs.
    let challenges = collect_dns01_challenges(&mut order, cfg).await?;

    Ok(Dns01OrderInProgress {
        order,
        account_credentials,
        challenges,
        domains: cfg.domains.clone(),
    })
}

/// Drive a **managed-mode** client-published DNS-01 order to completion and return
/// the issued cert + the (possibly newly-created) account credentials.
///
/// `account_credentials` is `Some(serialized)` to reuse a persisted account (D6),
/// or `None` to create a fresh one. The injected `seam` + `(provider_id, fields,
/// zone)` are the admin's decrypted DNS-provider credential and the resolved zone;
/// the order publishes/​tears-down the transient `_acme-challenge` TXT through
/// them. Manual mode (no covering credential) instead drives the
/// [`begin_dns01_order`] / [`complete_dns01_order`] pair. Native-only.
pub async fn obtain_certificate_dns01(
    cfg: &Dns01OrderConfig,
    account_credentials: Option<&[u8]>,
    seam: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    probe: Option<&dyn Dns01ResolvabilityProbe>,
) -> Result<Dns01Issued, Dns01Error> {
    // Production path: build/restore the ACME account over instant-acme's default
    // HTTP client (`hyper-rustls`, native trust roots), open the order, then publish
    // the challenges through the seam and finalize.
    let in_progress = begin_dns01_order(cfg, account_credentials).await?;
    publish_then_complete(in_progress, cfg, seam, provider_id, fields, zone, probe).await
}

/// As [`obtain_certificate_dns01`], but the ACME directory/account/order HTTP
/// calls go through the caller-supplied `http` client.
///
/// The **only** purpose is the pebble real-wire acceptance test (S4b): pebble
/// serves its ACME endpoint over HTTPS signed by its own throwaway CA, which the
/// default native-roots client won't trust, so the test injects a `HttpClient`
/// whose rustls roots include pebble's CA (instant-acme's `HttpClient` is
/// pluggable — `Account::{create,from_credentials}_and_http`). Production always
/// uses [`obtain_certificate_dns01`]. Gated behind the `test-helpers` feature so
/// it never enters a release build.
// The parameter list mirrors `obtain_certificate_dns01`'s exactly, plus the
// injected client — that shadowing is the whole point, so collapsing these into
// a struct here would diverge from the production signature under test.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "test-helpers")]
pub async fn obtain_certificate_dns01_with_http(
    cfg: &Dns01OrderConfig,
    account_credentials: Option<&[u8]>,
    seam: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    probe: Option<&dyn Dns01ResolvabilityProbe>,
    http: Box<dyn instant_acme::HttpClient>,
) -> Result<Dns01Issued, Dns01Error> {
    let in_progress = begin_dns01_order_with_http(cfg, account_credentials, http).await?;
    publish_then_complete(in_progress, cfg, seam, provider_id, fields, zone, probe).await
}

/// The managed-mode tail of an order opened by [`begin_dns01_order`]: publish every
/// `_acme-challenge` TXT through the seam, run the CA validation+finalize dance,
/// and ALWAYS tear the TXTs back down (even on an early-return failure) — the
/// shared [`with_published_challenges`] choreography with this driver's
/// [`run_ca_dance`] plugged in.
async fn publish_then_complete(
    in_progress: Dns01OrderInProgress,
    cfg: &Dns01OrderConfig,
    seam: &dyn DnsProviderSeam,
    provider_id: &str,
    fields: &[(String, SecretString)],
    zone: &DnsZoneRef,
    probe: Option<&dyn Dns01ResolvabilityProbe>,
) -> Result<Dns01Issued, Dns01Error> {
    let Dns01OrderInProgress {
        mut order,
        account_credentials,
        challenges,
        domains,
    } = in_progress;
    let ready_urls: Vec<String> = challenges.iter().map(|c| c.challenge_url.clone()).collect();
    // The propagation gate (probe poll or fixed-wait fallback) is owned by
    // `with_published_challenges`, between publish and the CA dance — so the
    // dance itself starts with a zero wait here. `run_ca_dance` keeps its wait
    // parameter for the manual-mode path ([`complete_dns01_order`]), which has
    // no seam and no gate.
    let (cert_chain_pem, privkey_pem) = with_published_challenges(
        seam,
        provider_id,
        fields,
        zone,
        &challenges,
        PropagationGate::from_config(cfg, probe),
        run_ca_dance(&mut order, ready_urls, domains, Duration::ZERO),
    )
    .await?;

    Ok(Dns01Issued {
        cert_chain_pem,
        privkey_pem,
        account_credentials,
    })
}

/// Phase 2 of a **manual-mode** DNS-01 order: the admin has pasted the
/// `_acme-challenge` TXT(s) (`challenges_to_publish`) at their registrar and the
/// page has verified them green — signal every challenge ready, poll the order to
/// `Ready`, finalize with a fresh CSR, and fetch the issued chain. There is **no**
/// provider seam in manual mode (no held credential), so — unlike
/// [`obtain_certificate_dns01`] — there is no publish or teardown: the admin owns
/// the record they pasted. Native-only.
///
/// **The admin's confirmation is not a propagation signal.** They pasted the TXT
/// into their registrar's control plane; whether the *authoritative NS* serves it
/// yet is a separate question, and losing that race is exactly the 2026-07-24 live
/// failure (the order went `Invalid`, was consumed, and the admin had to re-begin —
/// only attempt 2 succeeded, off a reused pending authorization). So this path runs
/// the same [`PropagationGate`] as the managed one: with `probe`, poll `zone_name`
/// until every challenge TXT is really served, then signal ready immediately.
/// Without a probe (wasm) it falls back to `propagation_wait`, unchanged.
pub async fn complete_dns01_order(
    in_progress: Dns01OrderInProgress,
    propagation_wait: Duration,
    zone_name: &str,
    probe: Option<&dyn Dns01ResolvabilityProbe>,
) -> Result<Dns01Issued, Dns01Error> {
    let Dns01OrderInProgress {
        mut order,
        account_credentials,
        challenges,
        domains,
    } = in_progress;
    let ready_urls: Vec<String> = challenges.iter().map(|c| c.challenge_url.clone()).collect();
    // Gate before the CA dance — the manual twin of `with_published_challenges`'s
    // publish→gate→dance sequence (there is nothing to publish or tear down here).
    PropagationGate::manual(probe, propagation_wait)
        .wait(zone_name, &challenges)
        .await;
    let (cert_chain_pem, privkey_pem) =
        run_ca_dance(&mut order, ready_urls, domains, Duration::ZERO).await?;

    Ok(Dns01Issued {
        cert_chain_pem,
        privkey_pem,
        account_credentials,
    })
}

/// Load a persisted ACME account, or create a fresh one, over instant-acme's
/// default native-roots HTTP client (the production path). Returns the account
/// and the serialized credentials to hand back to the caller for persistence —
/// the input bytes unchanged when reusing, the new account's when creating.
async fn load_or_create_account(
    cfg: &Dns01OrderConfig,
    account_credentials: Option<&[u8]>,
) -> Result<(instant_acme::Account, Vec<u8>), Dns01Error> {
    load_or_create_account_inner(cfg, account_credentials, None).await
}

/// As [`load_or_create_account`], but over a caller-supplied HTTP client (the
/// pebble test injects one trusting pebble's throwaway CA — S4b). `test-helpers`
/// only; the production order never reaches this.
#[cfg(feature = "test-helpers")]
async fn load_or_create_account_with_http(
    cfg: &Dns01OrderConfig,
    account_credentials: Option<&[u8]>,
    http: Box<dyn instant_acme::HttpClient>,
) -> Result<(instant_acme::Account, Vec<u8>), Dns01Error> {
    load_or_create_account_inner(cfg, account_credentials, Some(http)).await
}

/// Shared account load/create. `http` is `Some` only on the `test-helpers` path
/// (pebble CA trust); `None` uses instant-acme's `hyper-rustls` default client
/// (`Account::{create,from_credentials}`, available via the default features).
async fn load_or_create_account_inner(
    cfg: &Dns01OrderConfig,
    account_credentials: Option<&[u8]>,
    http: Option<Box<dyn instant_acme::HttpClient>>,
) -> Result<(instant_acme::Account, Vec<u8>), Dns01Error> {
    use instant_acme::{AccountCredentials, NewAccount};

    match account_credentials {
        Some(bytes) => {
            let creds: AccountCredentials = serde_json::from_slice(bytes)
                .map_err(|e| Dns01Error::Account(format!("parse stored credentials: {e}")))?;
            let account = account_builder(http)?
                .from_credentials(creds)
                .await
                .map_err(|e| Dns01Error::Account(format!("restore account: {e}")))?;
            Ok((account, bytes.to_vec()))
        }
        None => {
            // An empty contact email is valid: Let's Encrypt does not require an
            // account contact, and Fauna drives renewal reminders in-product (the
            // floor + the nest's at-risk push — `tls-certificates.md` § C.4), not
            // via the CA's expiry email. So omit the contact entirely rather than
            // sending a malformed `mailto:` when no email is configured (S5 passes
            // an empty contact until a contact-email UI knob lands in Phase 4).
            let contact =
                (!cfg.contact_email.is_empty()).then(|| format!("mailto:{}", cfg.contact_email));
            let contacts: Vec<&str> = contact.as_deref().into_iter().collect();
            let new_account = NewAccount {
                contact: contacts.as_slice(),
                terms_of_service_agreed: true,
                only_return_existing: false,
            };
            let (account, creds) = account_builder(http)?
                .create(&new_account, cfg.directory_url.clone(), None)
                .await
                .map_err(|e| Dns01Error::Account(format!("create account: {e}")))?;
            let creds_bytes = serde_json::to_vec(&creds)
                .map_err(|e| Dns01Error::Account(format!("serialize credentials: {e}")))?;
            Ok((account, creds_bytes))
        }
    }
}

/// The instant-acme 0.8 account builder, with the pebble HTTP client injected
/// when the caller passed one. `Account::builder()` is fallible (it constructs
/// the default hyper-rustls client); `builder_with_http` is not. Mirrors
/// `fauna_acme_http01`'s helper of the same name — the two drivers construct
/// their accounts independently by design (`fauna-acme-core` owns only the
/// shared finalize tail).
fn account_builder(
    http: Option<Box<dyn instant_acme::HttpClient>>,
) -> Result<instant_acme::AccountBuilder, Dns01Error> {
    match http {
        Some(http) => Ok(instant_acme::Account::builder_with_http(http)),
        None => instant_acme::Account::builder()
            .map_err(|e| Dns01Error::Account(format!("build ACME HTTP client: {e}"))),
    }
}

/// Extract the DNS-01 challenge for each **pending** authorization. Authorizations
/// already `Valid` (cached from a reused account — D6) need no challenge and are
/// skipped; the order is already `Ready` for those names.
async fn collect_dns01_challenges(
    order: &mut instant_acme::Order,
    cfg: &Dns01OrderConfig,
) -> Result<Vec<Dns01Challenge>, Dns01Error> {
    use instant_acme::{AuthorizationStatus, ChallengeType, Identifier};

    let mut out = Vec::new();
    // Since instant-acme 0.8 the authorizations arrive as an async iterator of
    // handles borrowing the order, rather than a `Vec` fetched up front.
    let mut authorizations = order.authorizations();
    while let Some(auth) = authorizations.next().await {
        let mut auth = auth.map_err(|e| Dns01Error::Ca(format!("get authorizations: {e}")))?;
        // A cached-valid authorization is already proven — no challenge to present.
        if auth.status == AuthorizationStatus::Valid {
            continue;
        }
        // A DNS-01 order only ever carries `Dns` identifiers; an `Ip` one is
        // unreachable here (RFC 8738 has no DNS-01 for an IP address, and this
        // driver never builds one). `Identifier` gained that second variant in
        // instant-acme 0.8, so this must be a `match` rather than the
        // irrefutable `let` it was under 0.7.
        let Identifier::Dns(domain) = auth.identifier().identifier.clone() else {
            continue;
        };
        let challenge = auth
            .challenge(ChallengeType::Dns01)
            .ok_or_else(|| Dns01Error::NoChallenge(domain.clone()))?;
        let dns_value = challenge.key_authorization().dns_value();
        let challenge_url = challenge.url.clone();
        out.push(Dns01Challenge {
            // Publish at the delegated target name if this domain's renewal is
            // CNAME-delegated (S6b), else at the default `_acme-challenge.<domain>`.
            publish_name: cfg.publish_name_for(&domain),
            domain,
            dns_value,
            challenge_url,
        });
    }
    Ok(out)
}

/// The CA-side half of the order: wait for DNS propagation, signal every challenge
/// ready, poll the order to `Ready`, then finalize with a fresh CSR and fetch the
/// cert chain. Returns `(cert_chain_pem, privkey_pem)`. This driver's
/// `ca_dance` future, plugged into the shared [`with_published_challenges`].
async fn run_ca_dance(
    order: &mut instant_acme::Order,
    ready_urls: Vec<String>,
    domains: Vec<String>,
    propagation_wait: Duration,
) -> Result<(String, String), Dns01Error> {
    // Let the TXT(s) reach the authoritative NS before the CA validates.
    if !propagation_wait.is_zero() {
        tokio::time::sleep(propagation_wait).await;
    }

    set_challenges_ready(order, &ready_urls).await?;

    if poll_order_ready(order).await? {
        finalize_and_fetch(order, &domains).await
    } else {
        // `Valid` before we finalized: the CA issued without our CSR/key, so we
        // have no private key to ship. Unreachable for a fresh order; surfaced as
        // an error rather than returning a keyless bundle.
        Err(Dns01Error::ValidBeforeFinalize)
    }
}

/// Signal every challenge whose URL is in `ready_urls` ready for validation.
///
/// instant-acme 0.8 moved the signal from `Order::set_challenge_ready(&url)` to
/// [`instant_acme::ChallengeHandle::set_ready`], so the order's authorizations
/// are re-walked here and matched by challenge URL — the same set
/// [`collect_dns01_challenges`] recorded, so a challenge the caller chose not to
/// publish is still not signalled.
async fn set_challenges_ready(
    order: &mut instant_acme::Order,
    ready_urls: &[String],
) -> Result<(), Dns01Error> {
    use instant_acme::ChallengeType;

    if ready_urls.is_empty() {
        return Ok(());
    }
    let mut signalled = 0usize;
    let mut authorizations = order.authorizations();
    while let Some(auth) = authorizations.next().await {
        let mut auth = auth.map_err(|e| Dns01Error::Ca(format!("get authorizations: {e}")))?;
        let Some(mut challenge) = auth.challenge(ChallengeType::Dns01) else {
            continue;
        };
        if !ready_urls.iter().any(|u| u == &challenge.url) {
            continue;
        }
        challenge
            .set_ready()
            .await
            .map_err(|e| Dns01Error::Ca(format!("set challenge ready: {e}")))?;
        signalled += 1;
    }
    if signalled != ready_urls.len() {
        return Err(Dns01Error::Ca(format!(
            "signalled {signalled} of {} published DNS-01 challenge(s) ready — the CA no \
             longer offers a matching challenge on every authorization",
            ready_urls.len()
        )));
    }
    Ok(())
}

/// Poll the order until it is `Ready` (→ `Ok(true)`, finalize) or `Valid`
/// (→ `Ok(false)`, already issued). Mirrors the HTTP-01 loop's 20×2s budget.
async fn poll_order_ready(order: &mut instant_acme::Order) -> Result<bool, Dns01Error> {
    use instant_acme::{Identifier, OrderStatus};

    let mut retries = 20u8;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let state = order
            .refresh()
            .await
            .map_err(|e| Dns01Error::Ca(format!("refresh order: {e}")))?;
        match state.status {
            OrderStatus::Ready => return Ok(true),
            OrderStatus::Valid => return Ok(false),
            OrderStatus::Pending | OrderStatus::Processing => {
                retries = retries.checked_sub(1).ok_or_else(|| {
                    Dns01Error::Ca("order did not become ready after 20 polls".to_string())
                })?;
            }
            OrderStatus::Invalid => {
                // The order-level `state.error` is frequently `None` even when a
                // DNS-01 authorization failed — the reason lives on the
                // authorization, not the order — which cost a live diagnosis on
                // 2026-07-23 ("order became invalid: None"). Re-fetch the
                // authorizations so the surfaced error names the DNS-01
                // identifier(s) the CA could not validate; the near-certain cause
                // is that the `_acme-challenge` TXT was not yet resolvable at
                // validation time (see `DEFAULT_PROPAGATION_WAIT`).
                //
                // Capture the order-level error into an owned string first so the
                // `&mut order` borrow held by `state` (from `refresh`) is released
                // before the `order.authorizations()` re-borrow below.
                let order_error = format!("{:?}", state.error);
                let mut names: Vec<String> = Vec::new();
                let mut authorizations = order.authorizations();
                let failed = loop {
                    match authorizations.next().await {
                        Some(Ok(auth)) => match auth.identifier().identifier {
                            Identifier::Dns(d) => names.push(d.clone()),
                            other => names.push(format!("{other:?}")),
                        },
                        Some(Err(e)) => break format!("<authorizations unavailable: {e}>"),
                        None => break names.join(", "),
                    }
                };
                return Err(Dns01Error::Ca(format!(
                    "order became invalid (order-level error: {order_error}); the CA could \
                     not validate the DNS-01 authorization(s) for [{failed}] — typically the \
                     _acme-challenge TXT was not yet resolvable on the authoritative NS at \
                     validation time"
                )));
            }
        }
    }
}

/// Generate a fresh keypair + multi-SAN CSR, finalize the order, and poll for the
/// issued certificate chain (10×2s). Returns `(cert_chain_pem, privkey_pem)`.
async fn finalize_and_fetch(
    order: &mut instant_acme::Order,
    domains: &[String],
) -> Result<(String, String), Dns01Error> {
    fauna_acme_core::finalize_order_and_fetch_certificate(order, domains)
        .await
        .map_err(Dns01Error::from)
}

#[cfg(test)]
mod credential_interop {
    //! The **D6 cross-device ACME account** rests on one claim that no test used
    //! to check against the real crate: `acme_pure::AccountCredentials` (the wasm
    //! twin) and `instant_acme::AccountCredentials` (the native driver) are the
    //! same serialized blob, so whichever of the admin's devices created the
    //! account, the others restore it (`tls-certificates.md` § C.3).
    //!
    //! `acme_pure`'s own tests parse a hand-written literal of that shape, which
    //! would keep passing if instant-acme's serialization moved underneath us —
    //! and the only end-to-end proof is a pebble run behind Docker. These two
    //! pins cross the crates directly and cost nothing, so an instant-acme bump
    //! reds here first — which is precisely what the 0.7 → 0.8 bump needed and
    //! did not have.

    // The persisted base64url PKCS#8 account key both pins carry across:
    // `acme_pure`'s instant-acme test vector, borrowed so the fixture has one
    // home (and the publish secret scan one allowlisted site).
    use crate::acme_pure::INSTANT_ACME_KEY_PKCS8_B64URL;

    /// Native writes it, wasm reads it: an `instant_acme::AccountCredentials`
    /// re-serialized must still parse as the pure twin, with the account key
    /// surviving byte-for-byte.
    #[test]
    fn an_instant_acme_blob_restores_into_the_pure_twin() {
        let json = format!(
            r#"{{"id":"https://acme/acct/1","key_pkcs8":"{INSTANT_ACME_KEY_PKCS8_B64URL}","directory":"https://acme-staging-v02.api.letsencrypt.org/directory"}}"#
        );
        let native: instant_acme::AccountCredentials =
            serde_json::from_str(&json).expect("instant-acme parses its own persisted shape");
        let reserialized = serde_json::to_vec(&native).expect("instant-acme re-serializes");

        let pure: crate::acme_pure::AccountCredentials = serde_json::from_slice(&reserialized)
            .expect("the pure twin must restore an account instant-acme wrote");
        assert_eq!(pure.id, "https://acme/acct/1");
        assert_eq!(
            pure.directory,
            "https://acme-staging-v02.api.letsencrypt.org/directory"
        );
        crate::acme_pure::AccountKey::from_pkcs8_der(&pure.key_pkcs8)
            .expect("the carried account key must load");
    }

    /// And the other direction: wasm writes it, native reads it.
    #[test]
    fn a_pure_twin_blob_restores_into_instant_acme() {
        let key = crate::acme_pure::AccountKey::generate();
        let pure = crate::acme_pure::AccountCredentials {
            id: "https://acme/acct/9".to_string(),
            key_pkcs8: key.to_pkcs8_der().expect("encode account key"),
            directory: "https://acme/dir".to_string(),
        };
        let bytes = serde_json::to_vec(&pure).expect("the pure twin serializes");

        let _restored = serde_json::from_slice::<instant_acme::AccountCredentials>(&bytes)
            .expect("instant-acme must restore an account the pure twin wrote");
    }
}
