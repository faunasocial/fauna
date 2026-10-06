//! Snapshot-driven provisioning orchestrator.
//!
//! The orchestrator runs four user-visible steps —  Domain, Server, Dns,
//! Online — each pre-flight-checking the world for already-desired state
//! before doing real work, retrying transient failures, and updating a
//! shared `ProvisioningSnapshot` (design tracked internally).
//!
//! Top-level entry points:
//!   * `provision_with_snapshot` — standard path; DNS provider already hosts
//!     the zone.
//!   * `provision_with_registration_snapshot` — extends the above with a
//!     `Registrar` call in step 1.
//!   * `provision_nest_no_dns` — "Set up DNS later"; runs only step 2,
//!     emits markdown for the user to add records manually.

use std::sync::Mutex;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::cloud_init::{CloudInitParams, build_cloud_init};
use crate::dns::{DnsProvider, DnsRecord, DnsZone};
use crate::error::ProvisionError;
use crate::progress::{
    CancelFlag, ProvisionResultPlain, ProvisionStep, ProvisioningSnapshot, SkipReason, StepOutcome,
    SubstepKey, run_step, set_cancelled, set_run_succeeded,
};
use crate::registrar::{ContactInfo, Registrar};
use crate::vps::{VpsInstance, VpsProvider};

/// Final outcome of a successful run. Same shape as
/// `progress::ProvisionResultPlain`; kept here for FFI surface continuity
/// (this is the type historical clients see returned from the orchestrator).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ProvisionResult {
    pub server_id: String,
    pub ipv4: String,
    pub domain: String,
    pub claim_code: String,
}

impl From<ProvisionResult> for ProvisionResultPlain {
    fn from(r: ProvisionResult) -> Self {
        Self {
            server_id: r.server_id,
            ipv4: r.ipv4,
            domain: r.domain,
            claim_code: r.claim_code,
        }
    }
}

/// Result of a "Set up DNS later" provisioning run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DeferredDnsResult {
    pub result: ProvisionResult,
    pub records: Vec<DnsRecord>,
    pub instructions_markdown: String,
}

/// Determine the DNS record name for a domain within a zone.
pub fn dns_record_name(domain: &str, zone: &str) -> String {
    if domain == zone {
        "@".to_string()
    } else {
        domain
            .strip_suffix(&format!(".{zone}"))
            .unwrap_or(domain)
            .to_string()
    }
}

/// Strip a single matched pair of surrounding double-quotes from a zone-file TXT
/// body, leaving the inner content the DNS-provider API expects (the inverse of
/// `fauna_mail::dns::per_domain::txt`'s framing). No-op when unquoted.
fn unquote_txt(body: &str) -> String {
    body.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(body)
        .to_string()
}

/// Convert a `fauna_mail::dns` zone-file record (quoted TXT, MX priority embedded
/// in the body, fully-qualified owner name) into the provider-API [`DnsRecord`]
/// the `DnsProvider` adapters consume (separate `priority`, unquoted `value`,
/// zone-relative `name` when `zone` is known). The transforms are the
/// deterministic inverse of the nest's `fauna_mail::dns::verify` normalization
/// (quote-strip, MX priority+target split, name relativization). Returns `None`
/// for `Ptr` — reverse DNS is set via the VPS provider's rDNS API
/// (`vps.set_ptr`), not the forward zone — and for `Srv`: the onboarding matrix
/// ([`build_records`]) does not include the CalDAV autodiscovery `SRV` record
/// (it is surfaced + published post-onboarding via the `admin-dns` page —
/// manual paste, or managed-mode reconcile through `fauna-client-dns`'s
/// `parse_rdata`), so onboarding never converts one.
fn to_provider_record(
    rec: &fauna_mail::dns::per_domain::DnsRecord,
    zone: Option<&str>,
) -> Option<DnsRecord> {
    use fauna_mail::dns::per_domain::DnsRecordType as T;
    let name = match zone {
        Some(z) => dns_record_name(&rec.name, z),
        None => rec.name.clone(),
    };
    let (record_type, value, priority) = match rec.record_type {
        T::A => ("A", rec.body.clone(), None),
        T::Aaaa => ("AAAA", rec.body.clone(), None),
        T::Txt => ("TXT", unquote_txt(&rec.body), None),
        T::Mx => {
            // Zone-file MX body is `"<priority> <host>"`.
            let (prio, host) = rec
                .body
                .split_once(' ')
                .expect("MX body is `<priority> <host>`");
            (
                "MX",
                host.to_string(),
                Some(prio.parse::<u32>().expect("MX priority is numeric")),
            )
        }
        T::Ptr => return None,
        // Onboarding does not publish the CalDAV autodiscovery SRV (not in
        // `build_records`); published post-onboarding via `admin-dns`.
        T::Srv => return None,
        // The floor-MX DANE TLSA is cert-coupled — the nest emits it post-claim
        // only while the MX is on the self-signed floor (`tls-certificates.md`
        // § D). Onboarding never publishes it (cert state isn't settled yet, and
        // `build_records` below emits no TLSA), so this arm is unreachable in
        // practice; drop it to stay exhaustive.
        T::Tlsa => return None,
    };
    Some(DnsRecord {
        record_type: record_type.to_string(),
        name,
        value,
        ttl: rec.ttl_seconds,
        priority,
    })
}

/// Assemble the onboarding DNS matrix for the deployment's primary domain from
/// the **shared** `fauna_mail::dns` builders, so it is byte-identical to what the
/// nest's `fauna.dns.{list,verify}_records` later renders
/// (`docs/goal/behavior/dns-management.md` § byte-for-byte). Records, in publish
/// order: apex `A`, `mail.<primary>` `A`, `MX` → `mail.<primary>` (pri 10), `SPF`
/// (`v=spf1 mx ~all`), `_dmarc` (default-strict). When `zone` is `Some`, owner
/// names are made zone-relative for the DNS-provider API; when `None` (the
/// deferred "Set up later" path — no provider chosen yet) names stay
/// fully-qualified for the manual-paste instructions.
///
/// **No DKIM record is published here.** The nest mints the DKIM signing key
/// when the mail domain is added (and at boot), which happens only post-boot —
/// after the box is claimed and mail enabled, long after this orchestrator
/// finishes. So the orchestrator cannot know the real key at provisioning
/// time; a client-generated one is a key the nest never signs with
/// (`dkim=fail` from day one). The DKIM TXT is published post-boot from the
/// nest's DKIM selector list (`fauna.bridges.list_dkim_selectors` /
/// `fauna.setup.status` `dkim_records`) via the `admin-dns` page
/// (`docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM provisioning).
fn build_records(domain: &str, zone: Option<&str>, instance_ipv4: &str) -> Vec<DnsRecord> {
    use fauna_mail::dmarc_publish::DmarcPublishPolicy;
    use fauna_mail::dns::host::{HostDnsInput, build_host_dns_records};
    use fauna_mail::dns::per_domain::{
        DEFAULT_SPF_BODY, build_dmarc_txt_record, build_mx_record, build_spf_record, mail_host,
    };

    // Single-host onboarding: the apex `<primary>` and the mail host
    // `mail.<primary>` (the MX target) both resolve to the freshly-provisioned
    // VPS. The two are persisted as distinct addresses nest-side post-claim
    // (memory `mail-host-ip-distinct-from-nest-ip`); here they are the same box.
    // `build_host_dns_records` also emits an advisory PTR → `mail.<primary>`,
    // which `to_provider_record` drops (rDNS goes through `vps.set_ptr`).
    let mut zone_records = build_host_dns_records(&HostDnsInput {
        primary_domain: domain,
        nest_ipv4: instance_ipv4,
        nest_ipv6: None,
        mail_ipv4: instance_ipv4,
        mail_ipv6: None,
        // Both infra subdomains are off at VPS-provisioning time: the iroh P2P
        // relay sidecar (the image's own decision, never a person's — it is
        // not running before the box is claimed; `p2p.md` § The relay) and
        // the ATProto PDS bridge (its service user always takes a
        // manual admin approval card, which cannot have happened before the box
        // exists). Their `relay.<primary>` / `pds.<primary>` A records surface
        // via the live DNS matrix (`dns_handlers::append_host_records`) once
        // enabled, not the initial zone seed.
        relay_enabled: false,
        pds_enabled: false,
    });

    let dmarc_body = DmarcPublishPolicy::default().to_txt_body(domain);
    zone_records.push(build_mx_record(domain, &mail_host(domain)));
    zone_records.push(build_spf_record(domain, DEFAULT_SPF_BODY));
    zone_records.push(build_dmarc_txt_record(domain, &dmarc_body));

    zone_records
        .iter()
        .filter_map(|r| to_provider_record(r, zone))
        .collect()
}

/// Standard provisioning flow with pre-existing DNS zone.
///
/// Drives the four-step snapshot model. Caller passes the same `state`,
/// `notify`, `cancel` triple that's wired into the `OnboardingMachine`.
/// On success returns the result and stamps it onto the snapshot; on
/// failure the snapshot's `overall == Failed` and `final_error` is set.
#[allow(clippy::too_many_arguments)]
pub async fn provision_with_snapshot<V, D, S, F, Fut>(
    client: &Client,
    // Builds the floor-cert-tolerant client for the Step-4 nest liveness poll,
    // given the box's captured static IP — carries a temporary DNS override so
    // the poll reaches the box before public DNS propagates (see
    // `run_all_steps`). The strict cloud-provider client is `client`.
    probe_for_ip: F,
    vps: &V,
    dns: &D,
    domain: &str,
    nest_base_url: &str,
    zone_id: &str,
    server_name: &str,
    location: &str,
    server_type: &str,
    cloud_init_params: &CloudInitParams,
    labels: &[(String, String)],
    state: &Mutex<ProvisioningSnapshot>,
    notify: impl Fn() + Send + Sync,
    // Fired exactly once with the box's public IPv4 and its [`ServerOrigin`]
    // the moment the Server step settles — see `run_all_steps` for why this is
    // a callback and not a snapshot field.
    on_server_ready: impl Fn(&str, ServerOrigin) + Send + Sync,
    cancel: &CancelFlag,
    sleep_fn: S,
) -> Result<ProvisionResult, ProvisionError>
where
    V: VpsProvider,
    D: DnsProvider,
    S: Fn(u64) -> Fut,
    F: Fn(&str) -> Client,
    Fut: std::future::Future<Output = ()>,
{
    let result = run_all_steps(
        client,
        probe_for_ip,
        vps,
        dns,
        None::<&NoRegistrar>, // no registration in standard flow
        domain,
        nest_base_url,
        zone_id,
        server_name,
        location,
        server_type,
        cloud_init_params,
        labels,
        state,
        &notify,
        on_server_ready,
        cancel,
        &sleep_fn,
        None,
        0,
    )
    .await;
    finalize_run(state, &notify, cancel, &result);
    result
}

/// Same as `provision_with_snapshot` but registers `domain` first via the
/// `Registrar` and then re-discovers `zone_id` by listing zones on the DNS
/// provider after registration.
#[allow(clippy::too_many_arguments)]
pub async fn provision_with_registration_snapshot<V, D, R, S, F, Fut>(
    client: &Client,
    // Builds the floor-cert-tolerant client for the Step-4 nest liveness poll,
    // given the box's captured static IP — carries a temporary DNS override so
    // the poll reaches the box before public DNS propagates (see
    // `run_all_steps`); the strict cloud-provider client is `client`.
    probe_for_ip: F,
    vps: &V,
    dns: &D,
    registrar: &R,
    domain: &str,
    nest_base_url: &str,
    years: u32,
    agreed_price_cents: u64,
    contact: Option<&ContactInfo>,
    server_name: &str,
    location: &str,
    server_type: &str,
    cloud_init_params: &CloudInitParams,
    labels: &[(String, String)],
    state: &Mutex<ProvisioningSnapshot>,
    notify: impl Fn() + Send + Sync,
    // Fired exactly once with the box's public IPv4 and its [`ServerOrigin`]
    // the moment the Server step settles — see `run_all_steps` for why this is
    // a callback and not a snapshot field.
    on_server_ready: impl Fn(&str, ServerOrigin) + Send + Sync,
    cancel: &CancelFlag,
    sleep_fn: S,
) -> Result<ProvisionResult, ProvisionError>
where
    V: VpsProvider,
    D: DnsProvider,
    R: Registrar,
    S: Fn(u64) -> Fut,
    F: Fn(&str) -> Client,
    Fut: std::future::Future<Output = ()>,
{
    let result = run_all_steps(
        client,
        probe_for_ip,
        vps,
        dns,
        Some(registrar),
        domain,
        nest_base_url,
        "", // zone_id is rediscovered post-registration
        server_name,
        location,
        server_type,
        cloud_init_params,
        labels,
        state,
        &notify,
        on_server_ready,
        cancel,
        &sleep_fn,
        contact,
        agreed_price_cents,
    )
    .await;
    finalize_run(state, &notify, cancel, &result);
    let _ = years; // years is registrar-specific; carried via register call
    result
}

/// "Set up DNS later" path. Step 1 is `Skipped` (no DNS provider), step 2
/// runs (VPS create), steps 3 and 4 are `Skipped`. Overall transitions to
/// `Succeeded` immediately after step 2; returns the records-to-paste markdown
/// for the dns_post_instructions page. The DKIM record is **not** in that
/// markdown — it is published post-boot from the nest's provisioned key via the
/// `admin-dns` page (see `build_records`).
#[allow(clippy::too_many_arguments)] // server-config + progress-callback plumbing; a params struct just relocates the fields
pub async fn provision_nest_no_dns<V>(
    client: &Client,
    vps: &V,
    domain: &str,
    server_name: &str,
    location: &str,
    server_type: &str,
    cloud_init_params: &CloudInitParams,
    labels: &[(String, String)],
    state: &Mutex<ProvisioningSnapshot>,
    notify: impl Fn() + Send + Sync,
    // Fired exactly once with the box's public IPv4 and its [`ServerOrigin`]
    // the moment the Server step settles — see `run_all_steps` for why this is
    // a callback and not a snapshot field.
    on_server_ready: impl Fn(&str, ServerOrigin) + Send + Sync,
    cancel: &CancelFlag,
) -> Result<DeferredDnsResult, ProvisionError>
where
    V: VpsProvider,
{
    let sleep_fn = |_secs: u64| async move {};
    // Step 1: Skipped (no DNS provider available).
    let _ = run_step(
        state,
        &notify,
        cancel,
        ProvisionStep::Domain,
        ProvisionStep::Domain.default_retry_policy(),
        Some(SubstepKey::DomainVerifyingZone),
        |_| async move { Ok(StepOutcome::Skipped(SkipReason::ZoneAlreadyVerified)) },
        &sleep_fn,
    )
    .await;

    // Step 2: Server create.
    let (instance, origin) = run_server_step(
        client,
        vps,
        domain,
        server_name,
        location,
        server_type,
        cloud_init_params,
        labels,
        state,
        &notify,
        cancel,
        &sleep_fn,
    )
    .await?;
    on_server_ready(&instance.ipv4, origin);

    // Step 3 & 4: Skipped — there's no domain pointing at the server yet.
    let _ = run_step(
        state,
        &notify,
        cancel,
        ProvisionStep::Dns,
        ProvisionStep::Dns.default_retry_policy(),
        Some(SubstepKey::DnsAddingDomainRecords),
        |_| async move { Ok(StepOutcome::Skipped(SkipReason::DnsRecordAlreadyExists)) },
        &sleep_fn,
    )
    .await;
    let _ = run_step(
        state,
        &notify,
        cancel,
        ProvisionStep::Online,
        ProvisionStep::Online.default_retry_policy(),
        Some(SubstepKey::OnlineWaiting),
        |_| async move { Ok(StepOutcome::Skipped(SkipReason::NestAlreadyOnline)) },
        &sleep_fn,
    )
    .await;

    // Records to surface to the user for manual paste. Zone is unknown (the user
    // picks a DNS provider later), so names stay fully-qualified.
    let records = build_records(domain, None, &instance.ipv4);
    let instructions_markdown = render_dns_instructions_markdown(domain, &instance.ipv4, &records);

    let result = ProvisionResult {
        server_id: instance.server_id,
        ipv4: instance.ipv4,
        domain: domain.to_string(),
        claim_code: cloud_init_params.claim_code.clone(),
    };
    set_run_succeeded(state, result.clone().into());
    notify();

    Ok(DeferredDnsResult {
        result,
        records,
        instructions_markdown,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Stand-in registrar type used to satisfy the generic parameter in the
/// non-registration path. Never instantiated.
struct NoRegistrar;
impl Registrar for NoRegistrar {
    async fn verify(&self, _client: &Client) -> Result<(), ProvisionError> {
        unreachable!()
    }
    async fn check(
        &self,
        _client: &Client,
        _domain: &str,
    ) -> Result<crate::registrar::DomainAvailability, ProvisionError> {
        unreachable!()
    }
    async fn register(
        &self,
        _client: &Client,
        _domain: &str,
        _years: u32,
        _agreed_price_cents: u64,
        _contact: Option<&ContactInfo>,
    ) -> Result<crate::registrar::RegistrationResult, ProvisionError> {
        unreachable!()
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_all_steps<V, D, R, S, F, Fut>(
    client: &Client,
    // Builds the self-signed-tolerant client used ONLY for the Step-4 liveness
    // poll of the freshly-provisioned nest, given the box's captured static IP
    // (a string, as returned by the VPS provider). The poll targets the nest by
    // its domain (`nest_base_url`), but the built client carries a temporary DNS
    // override `domain -> <ip>:443`, so it reaches the box the moment cloud-init
    // finishes — independent of public DNS propagation (Hetzner beta DNS can lag
    // ~30 min; docs/goal/architecture/testing.md § Gap 3). The nest serves its
    // self-signed floor cert until ACME issues a real one (domains-and-tls-
    // bootstrap.md — "floor first so HTTPS is up immediately"), and channel
    // binding makes reaching-by-IP MITM-safe (security.md § Transport trust).
    // The strict `client` above stays for the cloud-provider API calls.
    probe_for_ip: F,
    vps: &V,
    dns: &D,
    registrar: Option<&R>,
    domain: &str,
    nest_base_url: &str,
    initial_zone_id: &str,
    server_name: &str,
    location: &str,
    server_type: &str,
    cloud_init_params: &CloudInitParams,
    labels: &[(String, String)],
    state: &Mutex<ProvisioningSnapshot>,
    notify: impl Fn() + Send + Sync,
    // Called EXACTLY ONCE with the box's public IPv4, the moment `create_server`
    // returns (or the idempotent pre-flight finds the box already there) — the
    // one hook that exists so a caller can make a durable record of an address
    // that otherwise cannot be observed until the whole run succeeds. The
    // [`ServerOrigin`] beside the address says WHICH of those two happened, and
    // the caller needs it as badly as the address: a box created now boots with
    // this run's cloud-init (its claim code and injected identity), while a box
    // found already there boots with whatever an EARLIER run injected — the
    // identity this run must expect differs between the two, and nothing else
    // on this path can tell them apart (the pre-flight is name-only).
    //
    // Nothing publishes the instance to the shared `ProvisioningSnapshot` until
    // `set_run_succeeded`, i.e. after Step 4, so a caller watching the snapshot
    // learns the address only on a run that already finished. The crash window
    // the pending-provision slot exists for is exactly the opposite case — a
    // quit or crash *during* Server/Dns/Online (`docs/goal/behavior/onboarding.md`
    // § 6 *The pending-provision slot*) — so watching the snapshot would carry no
    // address in every case it is meant to rescue. Hence a callback rather than a
    // snapshot field: it fires once, at the only moment the fact becomes true,
    // and a durable side effect belongs there and not on a repeated `notify` tick.
    on_server_ready: impl Fn(&str, ServerOrigin) + Send + Sync,
    cancel: &CancelFlag,
    sleep_fn: &S,
    contact: Option<&ContactInfo>,
    agreed_price_cents: u64,
) -> Result<ProvisionResult, ProvisionError>
where
    V: VpsProvider,
    D: DnsProvider,
    R: Registrar,
    S: Fn(u64) -> Fut,
    F: Fn(&str) -> Client,
    Fut: std::future::Future<Output = ()>,
{
    // Step 1: Domain — register (if applicable) and verify the zone exists.
    let zone_id = run_step(
        state,
        &notify,
        cancel,
        ProvisionStep::Domain,
        ProvisionStep::Domain.default_retry_policy(),
        Some(SubstepKey::DomainVerifyingZone),
        |_attempt| async move {
            // List the provider's zones once (each carries id + name). We stash
            // BOTH: the id keys the provider's API paths, and the NAME is the
            // base `build_records` relativizes owner names against (a numeric or
            // opaque id can't be stripped as a suffix — see
            // `DnsProvider::record_names_relative_to_zone`).
            let zones = dns.verify(client).await?;

            // Pre-flight: does a zone with name == domain already exist?
            if let Some(zone) = zones.iter().find(|z| z.name == domain) {
                stash_zone(state, zone.id.clone(), Some(zone.name.clone()));
                return Ok(StepOutcome::Skipped(SkipReason::ZoneAlreadyVerified));
            }

            if let Some(reg) = registrar {
                reg.register(client, domain, 1, agreed_price_cents, contact)
                    .await?;
                // Re-discover the zone after registration completes.
                let zone = dns
                    .verify(client)
                    .await?
                    .into_iter()
                    .find(|z| z.name == domain)
                    .ok_or_else(|| {
                        ProvisionError::Other(format!(
                            "zone for {domain} not found via DNS provider post-registration",
                        ))
                    })?;
                stash_zone(state, zone.id.clone(), Some(zone.name.clone()));
                return Ok(StepOutcome::Succeeded);
            }

            if !initial_zone_id.is_empty() {
                // Recover the zone NAME for the caller-supplied id from the list
                // we just fetched. This is the branch the subdomain case
                // (`e2e-<id>.<zone>`) takes: without the name, Hetzner's numeric
                // id leaves owners fully-qualified and the RRset POST
                // double-suffixes them (`e2e-x.zone.tld.zone.tld`).
                let zone_name = zone_name_for_id(&zones, initial_zone_id);
                stash_zone(state, initial_zone_id.to_string(), zone_name);
                return Ok(StepOutcome::Succeeded);
            }

            Err(ProvisionError::Other(format!(
                "DNS provider has no zone for {domain}"
            )))
        },
        sleep_fn,
    )
    .await
    .map(|_| ())
    .map(|()| {
        let snap = state.lock().unwrap();
        snap.result
            .as_ref()
            .map(|_| String::new())
            .unwrap_or_default()
    });
    // The closure above stashed the zone id + name; pull them out of the
    // dedicated sidecar (a sidecar keeps them off the uniffi-exposed snapshot).
    drop(zone_id);
    let (stashed_zone_id, zone_name) = take_stashed_zone(state);
    let zone_id = stashed_zone_id.unwrap_or_else(|| initial_zone_id.to_string());

    // Step 2: Server — create the VPS from the cloud-init payload. The only
    // sub-step is the create_server call (idempotent: skips when
    // find_server_by_name hits). No DKIM keygen here — DKIM is nest-provisioned
    // on-read post-boot (see `build_records`).
    let (instance, origin) = run_server_step(
        client,
        vps,
        domain,
        server_name,
        location,
        server_type,
        cloud_init_params,
        labels,
        state,
        &notify,
        cancel,
        sleep_fn,
    )
    .await?;
    on_server_ready(&instance.ipv4, origin);

    // Step 3: DNS — apex A, mail A, MX, SPF, DMARC (+ PTR via the VPS rDNS API).
    // No DKIM record (published post-boot from the nest's provisioned key — see
    // `build_records`). Relativize owner names against the zone NAME for
    // providers whose record API wants zone-relative owners
    // (Hetzner/Gandi/Namecheap/Porkbun); pass None for Cloudflare (it wants
    // fully-qualified owners). The `unwrap_or` fallback to the threaded id
    // preserves the prior behavior for the registrar-style providers whose id
    // equals the zone name, should the name lookup ever come up empty.
    let zone_for_records: Option<&str> = if dns.record_names_relative_to_zone() {
        Some(zone_name.as_deref().unwrap_or(zone_id.as_str()))
    } else {
        None
    };
    let records = build_records(domain, zone_for_records, &instance.ipv4);
    // The FCrDNS / EHLO hostname the mail server presents (mail-multidomain.md
    // § One MX target; dns-management.md § Records covered) — the rDNS pointer
    // target, not the apex.
    let mail_fqdn = fauna_mail::dns::per_domain::mail_host(domain);
    let instance_for_step3 = instance.clone();
    let _ = run_step(
        state,
        &notify,
        cancel,
        ProvisionStep::Dns,
        ProvisionStep::Dns.default_retry_policy(),
        Some(SubstepKey::DnsAddingDomainRecords),
        |_attempt| {
            let records = records.clone();
            let instance = instance_for_step3.clone();
            let zone_id = zone_id.clone();
            let mail_fqdn = mail_fqdn.clone();
            async move {
                // Forward-zone records, in build order (apex A, mail A, MX, SPF,
                // DMARC); all idempotent.
                for rec in records.iter() {
                    create_record_idempotent(client, dns, &zone_id, rec).await?;
                }
                // Reverse DNS (PTR) → `mail.<primary>` for FCrDNS. Skip if already
                // set to the correct FQDN.
                match vps.get_ptr(client, &instance).await {
                    Ok(Some(cur)) if cur == mail_fqdn => {}
                    _ => {
                        vps.set_ptr(client, &instance, &mail_fqdn).await?;
                    }
                }
                Ok(StepOutcome::Succeeded)
            }
        },
        sleep_fn,
    )
    .await?;

    // Reach the freshly-provisioned box by its captured static IP: build the
    // floor-cert-tolerant liveness client with a temporary DNS override
    // (`domain -> <ip>:443`) so the Online poll below succeeds the moment the
    // nest is up, independent of public DNS propagation. `&`-bind so the
    // per-attempt poll closure captures it by shared reference (as before).
    let nest_probe_client = probe_for_ip(&instance.ipv4);
    let nest_probe_client = &nest_probe_client;

    // Step 4: Online — poll /api/v1/health until 200.
    let _ = run_step(
        state,
        &notify,
        cancel,
        ProvisionStep::Online,
        ProvisionStep::Online.default_retry_policy(),
        Some(SubstepKey::OnlineWaiting),
        |attempt| async move {
            let health_url = format!("{nest_base_url}/api/v1/health");
            // Poll with the floor-cert-tolerant client — the fresh nest serves
            // /health over its self-signed floor cert from boot; the strict
            // provider client would reject the handshake and stall until ACME.
            // Pre-flight check on attempt 1: if the nest is already up the
            // step is Skipped.
            if attempt == 1
                && let Ok(resp) = nest_probe_client.get(&health_url).send().await
                && resp.status().is_success()
            {
                return Ok(StepOutcome::Skipped(SkipReason::NestAlreadyOnline));
            }
            match nest_probe_client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => Ok(StepOutcome::Succeeded),
                Ok(resp) => Err(ProvisionError::Provider {
                    status: resp.status().as_u16(),
                    body: format!("health poll {nest_base_url}"),
                }),
                Err(e) => Err(e.into()),
            }
        },
        sleep_fn,
    )
    .await?;

    Ok(ProvisionResult {
        server_id: instance.server_id,
        ipv4: instance.ipv4,
        domain: domain.to_string(),
        claim_code: cloud_init_params.claim_code.clone(),
    })
}

/// Which arm of the idempotent Server step produced the box `on_server_ready`
/// reports — the fact a caller cannot recover from the address alone.
///
/// The pre-flight is a **name-only** lookup (`find_server_by_name`), so a box
/// of the right name is found whether this run built it, an earlier run of the
/// same wizard built it and then failed further on (the Retry-button case), or
/// an abandoned attempt left it behind. Only the *created* arm bakes this run's
/// `CloudInitParams` — claim code and injected identity — into the box; a
/// *found* box boots with whatever was injected when it was created, and a
/// caller that expects this run's identity of it can never be satisfied
/// (`docs/goal/behavior/onboarding.md` § 6 *The pending-provision slot*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerOrigin {
    /// `create_server` returned just now: the box boots with THIS run's
    /// cloud-init.
    Created,
    /// The pre-flight found a box of this name already at the provider: it
    /// boots with an EARLIER run's cloud-init, never this run's.
    Found,
}

#[allow(clippy::too_many_arguments)]
async fn run_server_step<V, S, Fut>(
    client: &Client,
    vps: &V,
    domain: &str,
    server_name: &str,
    location: &str,
    server_type: &str,
    cloud_init_params: &CloudInitParams,
    labels: &[(String, String)],
    state: &Mutex<ProvisioningSnapshot>,
    notify: impl Fn() + Send + Sync,
    cancel: &CancelFlag,
    sleep_fn: &S,
) -> Result<(VpsInstance, ServerOrigin), ProvisionError>
where
    V: VpsProvider,
    S: Fn(u64) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // We can't easily smuggle the VpsInstance out of the step's closure
    // through `StepOutcome`, so the closure stashes it (with the arm that
    // produced it) on a dedicated sidecar slot off the snapshot which we read
    // after run_step returns.
    let _ = domain; // unused; reserved for future server-naming hooks
    let labels = with_managed_by(labels);
    let labels = &labels[..];
    let captured: Mutex<Option<(VpsInstance, ServerOrigin)>> = Mutex::new(None);
    run_step(
        state,
        &notify,
        cancel,
        ProvisionStep::Server,
        ProvisionStep::Server.default_retry_policy(),
        Some(SubstepKey::ServerCreating),
        |_attempt| {
            let captured = &captured;
            async move {
                // Pre-flight: server with this name already exists.
                if let Some(inst) = vps.find_server_by_name(client, server_name).await? {
                    *captured.lock().unwrap() = Some((inst, ServerOrigin::Found));
                    return Ok(StepOutcome::Skipped(SkipReason::ServerAlreadyExists));
                }
                let user_data = build_cloud_init(cloud_init_params);
                let inst = vps
                    .create_server(
                        client,
                        server_name,
                        location,
                        server_type,
                        &user_data,
                        labels,
                    )
                    .await?;
                *captured.lock().unwrap() = Some((inst, ServerOrigin::Created));
                Ok(StepOutcome::Succeeded)
            }
        },
        sleep_fn,
    )
    .await?;
    captured.lock().unwrap().take().ok_or_else(|| {
        ProvisionError::Other("server step finished without producing an instance".into())
    })
}

/// Union the caller's labels with [`crate::vps::MANAGED_BY_LABEL`] — the
/// invariant that **every** fauna-provisioned box carries the stable
/// `managed-by=fauna` marker (`vps.md` § Uninstall), enforced at the single
/// create chokepoint (`run_server_step`) so no orchestration caller can forget
/// it. A caller-supplied `managed-by` key wins (no duplicate emitted — a
/// duplicate key would surface as two conflicting tag strings on the
/// tag-encoding providers).
fn with_managed_by(labels: &[(String, String)]) -> Vec<(String, String)> {
    let (key, value) = crate::vps::MANAGED_BY_LABEL;
    let mut out = Vec::with_capacity(labels.len() + 1);
    if !labels.iter().any(|(k, _)| k == key) {
        out.push((key.to_string(), value.to_string()));
    }
    out.extend(labels.iter().cloned());
    out
}

async fn create_record_idempotent<D: DnsProvider>(
    client: &Client,
    dns: &D,
    zone_id: &str,
    record: &DnsRecord,
) -> Result<(), ProvisionError> {
    // Find by name+type; if a record with the same value already exists,
    // skip creation. Mismatched values fall through to create_record (the
    // provider may reject duplicates, which is the right answer — the
    // user can resolve it manually).
    let existing = dns
        .find_records(client, zone_id, &record.name, &record.record_type)
        .await
        .unwrap_or_default();
    let already_correct = existing.iter().any(|r| r.value == record.value);
    if already_correct {
        return Ok(());
    }
    dns.create_record(client, zone_id, record).await
}

fn finalize_run(
    state: &Mutex<ProvisioningSnapshot>,
    notify: &impl Fn(),
    cancel: &CancelFlag,
    result: &Result<ProvisionResult, ProvisionError>,
) {
    match result {
        Ok(r) => {
            set_run_succeeded(state, r.clone().into());
            notify();
        }
        Err(ProvisionError::Cancelled) => {
            set_cancelled(state);
            notify();
        }
        Err(_) => {
            // run_step already marked overall = Failed for the failing step.
            notify();
        }
    }
    let _ = cancel; // referenced for symmetry; cancellation is observed inside run_step
}

// Sidecar for shuffling the resolved zone (id + name) out of the Domain step's
// closure. Implemented as `thread_local!`s so they don't appear on the
// `ProvisioningSnapshot` (which is FFI-exposed). The id keys the provider's API
// paths; the name is the relativization base for `build_records` owner names.
thread_local! {
    static STASHED_ZONE_ID: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static STASHED_ZONE_NAME: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn stash_zone(_state: &Mutex<ProvisioningSnapshot>, id: String, name: Option<String>) {
    STASHED_ZONE_ID.with(|s| *s.borrow_mut() = Some(id));
    STASHED_ZONE_NAME.with(|s| *s.borrow_mut() = name);
}

fn take_stashed_zone(_state: &Mutex<ProvisioningSnapshot>) -> (Option<String>, Option<String>) {
    let id = STASHED_ZONE_ID.with(|s| s.borrow_mut().take());
    let name = STASHED_ZONE_NAME.with(|s| s.borrow_mut().take());
    (id, name)
}

/// Find the zone NAME matching `id` in a provider's zone list. Recovers the name
/// for a caller-supplied numeric/opaque zone id (e.g. Hetzner Cloud's integer
/// ids) so `build_records` can relativize owner names against it.
fn zone_name_for_id(zones: &[DnsZone], id: &str) -> Option<String> {
    zones.iter().find(|z| z.id == id).map(|z| z.name.clone())
}

fn render_dns_instructions_markdown(domain: &str, ipv4: &str, records: &[DnsRecord]) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# DNS records for {domain}\n\n\
         Add these records at your DNS host. The server is running at \
         `{ipv4}` — once the records propagate (usually 1–10 minutes) your \
         nest will come online at `https://{domain}`. Then return to the \
         app and continue.\n\n\
         | Type | Name | Value | TTL | Priority |\n\
         |------|------|-------|-----|----------|\n"
    ));
    for r in records {
        let prio = r
            .priority
            .map(|p| p.to_string())
            .unwrap_or_else(|| "—".into());
        s.push_str(&format!(
            "| {} | `{}` | `{}` | {} | {} |\n",
            r.record_type, r.name, r.value, r.ttl, prio
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_managed_by_prepends_marker_to_empty_and_caller_labels() {
        let (key, value) = crate::vps::MANAGED_BY_LABEL;
        assert_eq!(
            with_managed_by(&[]),
            vec![(key.to_string(), value.to_string())]
        );
        let caller = vec![("fauna-e2e".to_string(), "1".to_string())];
        assert_eq!(
            with_managed_by(&caller),
            vec![
                (key.to_string(), value.to_string()),
                ("fauna-e2e".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn with_managed_by_lets_a_caller_supplied_key_win_without_duplicating() {
        let caller = vec![("managed-by".to_string(), "custom".to_string())];
        assert_eq!(with_managed_by(&caller), caller);
    }

    #[test]
    fn test_dns_record_name_apex() {
        assert_eq!(dns_record_name("fauna.social", "fauna.social"), "@");
    }

    #[test]
    fn test_dns_record_name_subdomain() {
        assert_eq!(dns_record_name("dev.fauna.social", "fauna.social"), "dev");
    }

    #[test]
    fn test_dns_record_name_deep_subdomain() {
        assert_eq!(
            dns_record_name("test.dev.fauna.social", "fauna.social"),
            "test.dev"
        );
    }

    /// The onboarding publish matrix MUST be byte-identical to what the nest's
    /// `fauna.dns.{list,verify}_records` renders from the shared
    /// `fauna_mail::dns` builders (`docs/goal/behavior/dns-management.md`
    /// § byte-for-byte). Standard path: zone known → zone-relative names for the
    /// DNS-provider API. DKIM is **not** in this matrix — it is published
    /// post-boot from the nest's provisioned key (see `build_records`).
    #[test]
    fn build_records_matches_canonical_shared_matrix() {
        use fauna_mail::dmarc_publish::DmarcPublishPolicy;
        use fauna_mail::dns::per_domain::{DEFAULT_SPF_BODY, mail_host};

        let recs = build_records("x.test", Some("x.test"), "1.2.3.4");

        // apex A, mail A, MX, SPF, DMARC — no DKIM (provisioned nest-side post-boot).
        assert_eq!(recs.len(), 5, "{recs:#?}");
        let find = |ty: &str, name: &str| {
            recs.iter()
                .find(|r| r.record_type == ty && r.name == name)
                .unwrap_or_else(|| panic!("missing {ty} {name} in {recs:#?}"))
        };

        // Apex + mail.<primary> A both resolve to the VPS IP (single-host onboarding).
        assert_eq!(find("A", "@").value, "1.2.3.4");
        assert_eq!(find("A", "mail").value, "1.2.3.4");

        // MX → mail.<primary> priority 10 (mail-multidomain.md:140), NOT the apex.
        // Target is the FQDN form (trailing dot) the shared builder emits so a
        // provider can't double it into mail.x.test.x.test (per_domain.rs builder).
        let mx = find("MX", "@");
        assert_eq!(mx.value, format!("{}.", mail_host("x.test")));
        assert_eq!(mx.priority, Some(10));

        // SPF default = v=spf1 mx ~all softfail (mail-multidomain.md:141), not `a mx -all`.
        assert_eq!(find("TXT", "@").value, DEFAULT_SPF_BODY);

        // DMARC body from the shared default-strict assembler.
        assert_eq!(
            find("TXT", "_dmarc").value,
            DmarcPublishPolicy::default().to_txt_body("x.test")
        );

        // No DKIM record is published at provisioning time (the orchestrator
        // can't know the nest-minted key yet — `build_records` doc).
        assert!(
            !recs.iter().any(|r| r.name.contains("_domainkey")),
            "{recs:#?}"
        );

        // Canonical 3600s TTL (mail-multidomain.md:152), not 300.
        assert!(recs.iter().all(|r| r.ttl == 3600), "{recs:#?}");
    }

    /// Deferred ("Set up later") path: no zone yet, so names stay fully-qualified
    /// for the user to paste at their registrar (orchestrator.rs § deferred-DNS).
    #[test]
    fn build_records_deferred_path_uses_full_names() {
        let recs = build_records("x.test", None, "1.2.3.4");
        assert!(
            recs.iter()
                .any(|r| r.record_type == "MX" && r.name == "x.test" && r.value == "mail.x.test.")
        );
        assert!(
            recs.iter()
                .any(|r| r.record_type == "A" && r.name == "mail.x.test")
        );
        assert!(recs.iter().any(|r| r.name == "_dmarc.x.test"));
        // No DKIM record in the manual-paste matrix (provisioned nest-side post-boot).
        assert!(!recs.iter().any(|r| r.name.contains("_domainkey")));
    }

    /// The subdomain case the live Hetzner e2e exercises (`e2e-<id>.<zone>`):
    /// when the zone NAME is known, every owner relativizes against it — the
    /// deployment subdomain's apex becomes the label (NOT `@`, since the
    /// deployment is not the zone apex), and `mail.`/`_dmarc.` sit under it.
    /// This is the regression guard for the numeric-zone-id bug: threading
    /// Hetzner's numeric `DnsZone.id` here left owners fully-qualified, which
    /// Hetzner Cloud then double-suffixes (`e2e-x.zone.tld.zone.tld`).
    #[test]
    fn build_records_relativizes_subdomain_against_zone_name() {
        let recs = build_records("e2e-abc.example.com", Some("example.com"), "1.2.3.4");
        let has = |ty: &str, name: &str| recs.iter().any(|r| r.record_type == ty && r.name == name);
        assert!(has("A", "e2e-abc"), "apex A → subdomain label: {recs:#?}");
        assert!(has("A", "mail.e2e-abc"), "mail A: {recs:#?}");
        assert!(has("MX", "e2e-abc"), "MX at subdomain apex: {recs:#?}");
        assert!(has("TXT", "e2e-abc"), "SPF at subdomain apex: {recs:#?}");
        assert!(has("TXT", "_dmarc.e2e-abc"), "DMARC: {recs:#?}");
        // No owner is fully-qualified — the bug's signature.
        assert!(
            !recs.iter().any(|r| r.name.contains("example.com")),
            "owner names must be zone-relative, got {recs:#?}",
        );
    }

    /// Contrast / documentation: threading a numeric or opaque zone id (e.g.
    /// Hetzner's `DnsZone.id`) instead of the zone NAME silently defeats
    /// relativization — owners stay fully-qualified. This is precisely the
    /// pre-fix failure the orchestrator now avoids by resolving the zone name
    /// (`zone_name_for_id`) before calling `build_records`.
    #[test]
    fn build_records_numeric_zone_id_leaves_owners_fully_qualified() {
        let recs = build_records("e2e-abc.example.com", Some("42"), "1.2.3.4");
        assert!(
            recs.iter()
                .any(|r| r.record_type == "A" && r.name == "e2e-abc.example.com"),
            "{recs:#?}"
        );
        assert!(recs.iter().any(|r| r.name == "mail.e2e-abc.example.com"));
    }

    #[test]
    fn zone_name_for_id_resolves_name_from_list() {
        let zones = vec![
            DnsZone {
                id: "42".into(),
                name: "example.com".into(),
            },
            DnsZone {
                id: "7".into(),
                name: "other.example".into(),
            },
        ];
        assert_eq!(
            zone_name_for_id(&zones, "42").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            zone_name_for_id(&zones, "7").as_deref(),
            Some("other.example")
        );
        assert_eq!(zone_name_for_id(&zones, "999"), None);
        assert_eq!(zone_name_for_id(&[], "42"), None);
    }
}
