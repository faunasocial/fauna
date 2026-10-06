//! WS-RPC handlers for the unified DNS-management surface (`fauna.dns.*`), per
//! `docs/goal/behavior/dns-management.md` (design tracked internally).
//!
//! Phase 0 ships `fauna.dns.list_records` — the read surface: per active local
//! domain, every DNS record the deployment needs and the exact value Fauna
//! would publish. Expected values come from the single source — the
//! `fauna_mail::dns::per_domain` builders + `mail_dkim_keys.public_dns_value`
//! — so publish (later slices) and verify never diverge. All `fauna.dns.*` kinds
//! are Admin-only (gated in `bridge_method_allowlist`). Credential storage,
//! managed publish, and live verification land in later slices.

use std::sync::Arc;
use std::time::Duration;

use fauna_mail::dmarc_publish::{DmarcPublishPolicy, apply_overrides_json};
use fauna_mail::dns::host::{
    HostDnsInput, build_host_dns_records, build_mail_host_records, build_mail_tlsa_record,
    build_secondary_apex_records,
};
use fauna_mail::dns::per_domain::{
    DkimSelectorDns, DnsRecord, DnsRecordType, DomainDnsInput, build_domain_dns_records,
};
use fauna_mail::outbound::mta_sts::{MtaStsMode, MtaStsPolicy, assemble_policy_body};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    dns::{
        DnsRecordStatus, DnsRecordView, DomainDns, DomainVerifyStatus, ListRecordsReply,
        ListRecordsRequest, ProbeTxtVisibleReply, ProbeTxtVisibleRequest, SetHostAddressReply,
        SetHostAddressRequest, VerifyRecordsReply, VerifyRecordsRequest,
    },
};

use crate::db::mail_domains::MailDomain;
use crate::db::nest_host_address::HostAddress;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Error helpers (mirror bridge_blob_handlers; namespaced to fauna.dns) ──

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

// Every `fauna.dns.*` kind is Admin-only per
// `bridge_method_allowlist::is_permitted`.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── Record-matrix assembly ───────────────────────────────────────────

fn record_type_label(t: DnsRecordType) -> &'static str {
    match t {
        DnsRecordType::Mx => "MX",
        DnsRecordType::Txt => "TXT",
        DnsRecordType::Srv => "SRV",
        DnsRecordType::A => "A",
        DnsRecordType::Aaaa => "AAAA",
        DnsRecordType::Ptr => "PTR",
        DnsRecordType::Tlsa => "TLSA",
    }
}

/// Map a shared-builder [`DnsRecord`] to the wire [`DnsRecordView`].
fn record_view(r: DnsRecord) -> DnsRecordView {
    DnsRecordView {
        name: r.name,
        record_type: record_type_label(r.record_type).to_string(),
        expected: r.body,
        ttl_seconds: r.ttl_seconds,
        extra: Default::default(),
    }
}

/// Assemble the published `_dmarc.<domain>` TXT body from the deployment-wide
/// default policy + this domain's stored per-domain override
/// (`mail_domains.dmarc_overrides`), per `docs/goal/behavior/dmarc-reporting.md`
/// § Record shape. `primary_domain` fills the default `rua=` template — every
/// domain's aggregate reports route to the single deployment-wide processor
/// `dmarc-report@<primary>` (`mail-multidomain.md` § Per-domain DMARC), so the
/// onboarding provisioner (`fauna-provisioning`) publishes the identical body
/// via the same shared assembler.
///
/// The deployment-wide `mail.dmarc.*` catalog write-path isn't built yet, so the
/// base is [`DmarcPublishPolicy::default`]; when the catalog lands, swap it for
/// the catalog-derived base (the assembler is unchanged).
fn dmarc_body_for(domain: &MailDomain, primary_domain: &str) -> String {
    apply_overrides_json(
        DmarcPublishPolicy::default(),
        domain.dmarc_overrides_json.as_deref(),
    )
    .to_txt_body(primary_domain)
}

/// Build the wire-shaped record matrix for one domain, sourcing every expected
/// value from the shared builders + the stored DKIM selector values.
fn build_domain_view(
    domain: &MailDomain,
    primary_domain: &str,
    dkim_selectors: Vec<DkimSelectorDns>,
) -> DomainDns {
    let primary_mx_host = format!("mail.{primary_domain}");
    let dmarc_body = dmarc_body_for(domain, primary_domain);
    let policy = MtaStsPolicy {
        version: "STSv1".to_string(),
        mode: MtaStsMode::from_stored(&domain.mta_sts_mode),
        mx: vec![primary_mx_host.clone()],
        max_age_secs: domain.mta_sts_max_age_seconds.clamp(0, u32::MAX as i64) as u32,
    };
    let mta_sts_policy_body = assemble_policy_body(&policy);

    let input = DomainDnsInput {
        domain: &domain.domain_name,
        primary_mx_host: &primary_mx_host,
        primary_domain,
        spf_body: &domain.spf_record,
        dmarc_body: &dmarc_body,
        mta_sts_policy_body: &mta_sts_policy_body,
        dkim_selectors: &dkim_selectors,
    };

    let records = build_domain_dns_records(&input)
        .into_iter()
        .map(record_view)
        .collect();

    DomainDns {
        domain: domain.domain_name.clone(),
        // Phase 0: managed mode (auto-publish) isn't built — every domain is
        // manual (admin pastes records; Slice 2 adds live red/green).
        mode: "manual".to_string(),
        // The primary domain owns the deployment's host-level rows (the
        // `mail.<primary>` host A/AAAA appended by `append_host_records` + the
        // cert-coupled floor-MX TLSA appended by `append_floor_mx_tlsa`); the
        // managed reconcile keys the TLSA withdraw-on-trusted pass on this so
        // only the primary's `publish` touches the shared `_25._tcp.mail.<primary>`
        // slot (`tls-certificates.md` § D).
        is_primary: domain.is_primary,
        records,
        extra: Default::default(),
    }
}

/// Assemble the per-domain expected-record matrix (the single source both
/// `list_records` and `verify_records` consume). `filter` restricts to one
/// domain (case-insensitive); `None` → every active local domain. Empty on a
/// fresh nest with no primary mail domain.
async fn assemble_domain_views(
    state: &Arc<AppState>,
    filter: Option<&str>,
) -> Result<Vec<DomainDns>, RpcError> {
    let primary = state
        .db
        .lookup_primary_mail_domain()
        .await
        .map_err(internal)?;
    // No primary domain yet (fresh nest) → nothing to publish.
    let Some(primary) = primary else {
        return Ok(vec![]);
    };

    let all = state
        .db
        .list_active_mail_domains()
        .await
        .map_err(internal)?;

    // A `mail.<x>` MX-host A/AAAA row must stay published for one extra domain
    // during a rename, so the client keeps it resolvable:
    //   - **pre-flip** → `mail.<new-primary>` (the new primary is still a secondary
    //     today; the in-process ACME HTTP-01 order needs it to resolve before
    //     adding it as a resolve-gated cert SAN — SLICE 2);
    //   - **post-flip (grace)** → `mail.<old-primary>` (the old primary is now a
    //     secondary, whose apex-only shape drops its `mail.<old>` row, but peers
    //     holding a cached `<domain> MX → mail.<old>` must still resolve it through
    //     grace — SLICE 3; `mail-primary-domain-rename.md` § Data — Mutations to
    //     per-domain DNS records, grace-window keep-alive).
    // Read the active rename once here (not per-domain), then match its id in the
    // secondary branch.
    let rename_extra_mail_host_id: Option<[u8; 16]> = state
        .db
        .get_active_rename()
        .await
        .map_err(internal)?
        .and_then(|r| match r.parsed_state() {
            Some(s) if s.is_pre_flip() => Some(r.new_primary_domain_id),
            Some(s) if s.is_post_flip_active() => Some(r.old_primary_domain_id),
            _ => None,
        });

    let mut domains = Vec::new();
    for d in all {
        if let Some(filter) = filter
            && !d.domain_name.eq_ignore_ascii_case(filter)
        {
            continue;
        }
        let dkim = state
            .db
            .list_dkim_selectors(Some(&d.domain_name))
            .await
            .map_err(internal)?
            .into_iter()
            .map(|s| DkimSelectorDns {
                selector: s.selector,
                public_dns_value: s.public_dns_value,
            })
            .collect();
        let mut view = build_domain_view(&d, &primary.domain_name, dkim);
        // The nest-host A/AAAA/PTR rows are deployment-host config (not per-domain
        // mail records), so they attach to the *primary* domain's matrix — and
        // only when the host address has been persisted (the
        // `fauna.dns.set_host_address` hand-off has run). `dns-management.md`
        // § Records covered. The DANE TLSA for the floor MX is likewise a
        // host-level record (one per deployment at the shared `mail.<primary>`),
        // but cert-coupled rather than address-gated.
        if d.is_primary {
            append_host_records(state, &primary.domain_name, &mut view).await?;
            append_floor_mx_tlsa(
                state.served_cert_spki.as_ref(),
                &primary.domain_name,
                &mut view,
            );
            append_atproto_handle_txts(state, &primary.domain_name, &mut view).await?;
        } else {
            // A secondary domain needs the client to be able to *reach* the nest
            // via that domain (`bob@domain2` → this nest), so it gets the apex
            // `A`/`AAAA` → nest row — parity with the primary's apex, per
            // `mail-multidomain.md` § Client reachability of a secondary domain.
            // (Mail delivery is unaffected: its MX is the shared `mail.<primary>`.)
            append_secondary_client_reachability(state, &d.domain_name, &mut view).await?;
            // If this secondary is the rename's extra mail-host domain (the new
            // primary pre-flip, or the old primary during grace), it *also* gets
            // the `mail.<x>` MX-host row (→ mail address) so the client keeps it
            // published (`mail-primary-domain-rename.md` § Data / § cert chain).
            if rename_extra_mail_host_id == Some(d.domain_id) {
                append_rename_mail_host(state, &d.domain_name, &mut view).await?;
            }
        }
        // The identity root is per **domain**, not per deployment-primary: every
        // active local domain is a client entry point (the branch above gives a
        // secondary its apex A/AAAA → nest for exactly that reason), so a fresh
        // app arriving at any of them needs the same expected-identity answer.
        // Outside the branch, and gated per-domain on public-ness inside.
        // `dns-management.md` § Records covered → the `_fauna.<domain>` bullet.
        append_fauna_self_txt(state, &d.domain_name, &mut view);
        domains.push(view);
    }
    Ok(domains)
}

/// Append the deployment's host `A`/`AAAA`/`PTR` rows to the primary domain's
/// matrix, sourced from the persisted `nest_host_address` via the shared
/// `fauna_mail::dns::host` builder. No-op on a fresh nest (no address persisted).
///
/// When an infra subdomain's service is on — the iroh P2P relay sidecar
/// (connected to the nest, `relay_sidecar_connected`) or the ATProto PDS bridge (an approved
/// `atproto.pds` bridge service user) — the builder also emits that host's
/// `relay.<primary>` / `pds.<primary>` A/AAAA row at the nest address, so the
/// admin sees the A record they must publish: the apex ACME
/// order only adds the matching SAN once that record
/// resolves, so without this row nothing would tell the admin to
/// make it resolve and the name would serve the untrusted floor cert forever.
async fn append_host_records(
    state: &Arc<AppState>,
    primary_domain: &str,
    view: &mut DomainDns,
) -> Result<(), RpcError> {
    let Some(addr) = state.db.get_host_address().await.map_err(internal)? else {
        return Ok(());
    };
    let input = HostDnsInput {
        primary_domain,
        nest_ipv4: &addr.nest_ipv4,
        nest_ipv6: addr.nest_ipv6.as_deref(),
        mail_ipv4: &addr.mail_ipv4,
        mail_ipv6: addr.mail_ipv6.as_deref(),
        relay_enabled: crate::discovery_core::relay_sidecar_connected(state),
        pds_enabled: crate::bridge_atproto_handlers::atproto_pds_bridge_approved(state).await,
    };
    view.records
        .extend(build_host_dns_records(&input).into_iter().map(record_view));
    Ok(())
}

/// Append the client-reachability apex `A`/`AAAA` row (→ the nest address) to a
/// **secondary** domain's matrix, so a client that found a handle as
/// `bob@<secondary>` can reach this nest via that domain — parity with the
/// primary's apex `A` (`mail-multidomain.md` § Client reachability of a secondary
/// domain). Sourced from the same persisted `nest_host_address`; no-op until the
/// host address is set. Mail delivery to the secondary is unaffected — its MX
/// points at the shared `mail.<primary>`, whose address row the primary emits.
async fn append_secondary_client_reachability(
    state: &Arc<AppState>,
    domain: &str,
    view: &mut DomainDns,
) -> Result<(), RpcError> {
    let Some(addr) = state.db.get_host_address().await.map_err(internal)? else {
        return Ok(());
    };
    view.records.extend(
        build_secondary_apex_records(domain, &addr.nest_ipv4, addr.nest_ipv6.as_deref())
            .into_iter()
            .map(record_view),
    );
    Ok(())
}

/// Append the `mail.<domain>` `A`/`AAAA` (→ the **mail-host** address) row for a
/// domain that needs its MX-host name kept published across a rename even though it
/// is a *secondary* in `mail_domains` (whose apex-only shape omits `mail.`). Two
/// cases use it: the **new primary pre-flip** (`mail.<new>`), so the client
/// publishes it and the in-process ACME HTTP-01 order can resolve `mail.<new>`
/// before adding it as a resolve-gated cert SAN (SLICE 2); and the **old primary
/// during grace** (`mail.<old>`), so peers holding a cached `<domain> MX →
/// mail.<old>` keep resolving it until `complete` (SLICE 3).
///
/// The caller has already matched this domain as the rename's extra mail-host
/// domain (`mail-primary-domain-rename.md` § Behavior — cert chain re-issue
/// ordering / § Data — Mutations to per-domain DNS records; the cert SAN is gated
/// in `acme_http01`). Sourced from the same persisted `nest_host_address`; no-op
/// until the host address is set.
async fn append_rename_mail_host(
    state: &Arc<AppState>,
    domain: &str,
    view: &mut DomainDns,
) -> Result<(), RpcError> {
    let Some(addr) = state.db.get_host_address().await.map_err(internal)? else {
        return Ok(());
    };
    view.records.extend(
        build_mail_host_records(domain, &addr.mail_ipv4, addr.mail_ipv6.as_deref())
            .into_iter()
            .map(record_view),
    );
    Ok(())
}

/// Append the DANE TLSA for the self-signed floor MX to the primary domain's
/// matrix — **only while** the served `mail.<primary>` leaf is the self-signed
/// floor (`tls-certificates.md` § D, the cert-honesty coupling symmetric to the
/// 5a published-MTA-STS-mode coupling). The record pins the served floor leaf's
/// SPKI, which reuses the *stable* floor key, so a routine renewal never churns
/// it; once a trusted cert covers the MX the row leaves the matrix (a floor-key
/// TLSA against a trusted cert would DANE-fail), so the published record is
/// withdrawn in lockstep. No-op when no resolver is wired (`None`) or no cert is
/// served yet. Cert-coupled, not address-gated — independent of
/// `append_host_records`. Takes the resolver directly so the coupling is
/// unit-testable without a full `AppState`.
fn append_floor_mx_tlsa(
    resolver: Option<&Arc<dyn crate::acme::ServedCertSpki>>,
    primary_domain: &str,
    view: &mut DomainDns,
) {
    let Some(resolver) = resolver else {
        return;
    };
    let mx_sni = format!("mail.{primary_domain}");
    // Emit only while on the floor (self-signed served leaf). A trusted cert
    // (or no cert / pending) → no floor-key TLSA.
    if !matches!(resolver.served_cert_facts(&mx_sni), Some(f) if f.is_floor) {
        return;
    }
    let Some(spki) = resolver.served_cert_spki_sha256(&mx_sni) else {
        return;
    };
    view.records
        .push(record_view(build_mail_tlsa_record(primary_domain, &spki)));
}

/// Append one `_atproto.<localpart>.<primary>` TXT per ACTIVE ATProto identity
/// (`did=<did>`) to the primary domain's matrix — the handle-verification
/// record (`atproto-pds-bridge.md` § Identity). The handle is derived at READ
/// time from the user's current Fauna handle, so a rename re-derives here with
/// no stored state; a managed-mode client publishes the row through the
/// generic create-only publish with no client change. Skipped entirely on a
/// non-public domain (no ATProto presence on localhost/LAN), and per-row when
/// the handle doesn't derive (reserved label / malformed claim-era handle) or
/// the identity has no DID yet (pending mint — nothing to verify).
async fn append_atproto_handle_txts(
    state: &Arc<AppState>,
    primary_domain: &str,
    view: &mut DomainDns,
) -> Result<(), RpcError> {
    if !fauna_provisioning::probe::resolve_handle_domain(primary_domain).is_public_dns_name {
        return Ok(());
    }
    let identities = state.db.list_atproto_identities().await.map_err(internal)?;
    for row in identities {
        let Some(did) = row.did.as_deref() else {
            continue;
        };
        let handle = state
            .db
            .get_handle(&row.actor_id)
            .await
            .map_err(internal)?
            .unwrap_or_default();
        let Ok(atproto_handle) =
            fauna_protocol::atproto::derive_atproto_handle(&handle, primary_domain)
        else {
            continue;
        };
        let (name, value) = fauna_protocol::atproto::handle_verification_txt(&atproto_handle, did);
        view.records
            .push(record_view(fauna_mail::dns::per_domain::build_txt_record(
                name, &value,
            )));
    }
    Ok(())
}

/// Append the `_fauna.<domain>` TXT `self=<nest_actor_id>` identity root to
/// `domain`'s matrix — the row that lets a **fresh** client resolve this box's
/// expected identity before it has anything pinned (`dns-management.md`
/// § Records covered, the `_fauna.<domain>` bullet; `../architecture/security.md`
/// § Transport trust, Axis 2). Two ratified consumers read it: the self-signed
/// floor's fresh-client acceptance (`tls-certificates.md` § E) and the
/// deployment-seed rotation ceremony's post-flip re-publish (`box-recovery.md`
/// § Deployment-seed rotation).
///
/// **Emitted for every public active local domain, primary and secondary alike**
/// (ruled 2026-08-13, reconciling the two owner docs). It is *not* primary-only
/// like `_atproto` and the wildcard, and the difference is substantive rather
/// than conventional: those two derive their names from something that exists
/// only on the primary (handles minted there; per-user subdomain sites served
/// there), whereas this row's value is the **deployment's** identity — the same
/// answer at every domain of one box. A secondary is a client entry point by
/// construction ([`append_secondary_client_reachability`] publishes its apex
/// A/AAAA → nest, and `acme_http01::desired_san_domains` puts its apex in the
/// ACME order, both for client reachability), so omitting the row there made a
/// user's trust posture depend on which of the box's domains their handle sat
/// on. Widening is a name-set change only: the value is byte-identical, and the
/// client's withdraw-aware converge pass is already per-domain (it resolves the
/// covering credential + zone for each), so it publishes into the secondary's
/// own zone with no client change.
///
/// The value is read from the **live** deployment key on every assembly, with no
/// stored state — so a rotation (which swaps `nest_identity` when the serving
/// generation re-enters, `box-recovery.md` § Adoption by the running process)
/// re-derives a new value at this **stable name**. That is precisely the case
/// the generic create-only publish cannot converge, which is why the client
/// publishes this slot through the withdraw-aware
/// `fauna-client-dns::reconcile_fauna_self_txt` rather than the create loop.
/// The nest only *offers* the row — it never writes DNS (`dns-management.md`).
///
/// Skipped on a non-public domain, and the gate is **per domain** rather than
/// inherited from the primary: the reader classifies loopback / IP-literal /
/// `.local` authorities as having no DNS authority and returns `None`
/// (`fauna-anon-client::resolve_dns_self_root`), so the row would be
/// unconsultable there.
fn append_fauna_self_txt(state: &Arc<AppState>, domain: &str, view: &mut DomainDns) {
    if !fauna_provisioning::probe::resolve_handle_domain(domain).is_public_dns_name {
        return;
    }
    let value = format!(
        "self={}",
        hex::encode(state.nest_identity.public_key_bytes())
    );
    view.records
        .push(record_view(fauna_mail::dns::per_domain::build_txt_record(
            format!("_fauna.{domain}"),
            &value,
        )));
}

// ── Handlers ─────────────────────────────────────────────────────────

/// `fauna.dns.list_records` (Admin) — the per-domain expected-record matrix the
/// unified DNS page renders. `domain: None` → all active local domains.
fn list_records_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.dns.list_records").await?;
            let req: ListRecordsRequest = decode(&payload).map_err(malformed)?;
            let domains = assemble_domain_views(&state, req.domain.as_deref()).await?;
            encode_reply(&ListRecordsReply {
                domains,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.dns.verify_records` (Admin) — the live red/green truth the unified DNS
/// page overlays on the `list_records` matrix. Resolves every expected record
/// against a public-recursive resolver (`state.dns_verifier`) and reports
/// `ok | missing | mismatch | checking` per record, keyed by `(name,
/// record_type)`. Status is displayed truth, never a gate (per
/// `docs/goal/behavior/dns-management.md` § Live verification).
fn verify_records_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.dns.verify_records").await?;
            let req: VerifyRecordsRequest = decode(&payload).map_err(malformed)?;
            let domains = assemble_domain_views(&state, req.domain.as_deref()).await?;

            let mut out = Vec::with_capacity(domains.len());
            for d in domains {
                let mut records = Vec::with_capacity(d.records.len());
                for r in &d.records {
                    let result = state
                        .dns_verifier
                        .verify(&r.name, &r.record_type, &r.expected)
                        .await;
                    records.push(DnsRecordStatus {
                        name: r.name.clone(),
                        record_type: r.record_type.clone(),
                        observed: result.observed,
                        status: result.status.as_wire_str().to_string(),
                        extra: Default::default(),
                    });
                }
                out.push(DomainVerifyStatus {
                    domain: d.domain,
                    records,
                    extra: Default::default(),
                });
            }

            encode_reply(&VerifyRecordsReply {
                domains: out,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.dns.probe_txt_visible` (Admin) — the DNS-01 propagation gate's
/// readiness probe, run nest-side on behalf of a client that cannot run it
/// itself.
///
/// A browser has no raw DNS, so the **web** app's ACME order had no way to ask
/// "is the challenge TXT actually being served yet?" and fell back to a blind
/// fixed wait — the precise defect that made live issuance fail in 2026-07-24
/// (`tls-certificates.md` § B tier 2). This kind lets web ask the same question
/// the 5 native apps and the tui answer in-process, over the *identical*
/// authoritative-direct query ([`fauna_core::authoritative_dns`]).
///
/// Deliberately **not** `state.dns_verifier`: that one is public-*recursive* and
/// caches, so it negative-caches a miss for up to the zone's SOA minimum — it
/// would report "absent" long after the record went live, and the gate's own
/// polling is what would plant that entry. The record-matrix verdicts want a
/// recursive view; a freshly-published challenge does not.
///
/// Reads public DNS and mutates nothing; carries no credential (those stay
/// client-held — `dns-management.md` § Where the credential lives). Admin-gated
/// to match `verify_records` / `cert_status` on the same admin surface.
fn probe_txt_visible_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.dns.probe_txt_visible").await?;
            let req: ProbeTxtVisibleRequest = decode(&payload).map_err(malformed)?;
            let visible = fauna_core::authoritative_dns::authoritative_txt_visible(
                &req.zone_name,
                &req.record_name,
                &req.txt_value,
                None,
            )
            .await;
            encode_reply(&ProbeTxtVisibleReply {
                visible,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.dns.set_host_address` (Admin) — the onboarding hand-off that persists
/// the deployment's own public address nest-side so the host `A`/`AAAA`/`PTR`
/// rows can be assembled + verified. The address is **public, not a secret**
/// (the registrar/DNS-provider credentials, by contrast, stay client-held), so
/// this is ordinary nest state. `mail_*` may carry a different IP than `nest_*`
/// (the MX target can be a separate box). Idempotent upsert.
fn set_host_address_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.dns.set_host_address").await?;
            let req: SetHostAddressRequest = decode(&payload).map_err(malformed)?;
            let addr = HostAddress {
                nest_ipv4: req.nest_ipv4,
                nest_ipv6: req.nest_ipv6,
                mail_ipv4: req.mail_ipv4,
                mail_ipv6: req.mail_ipv6,
            };
            state.db.set_host_address(&addr).await.map_err(internal)?;
            encode_reply(&SetHostAddressReply {})
        })
    })
}

/// Register the `fauna.dns.*` kinds. Called from `lib.rs::build_app`.
pub fn register_dns_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.dns.list_records",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: list_records_handler(),
        },
    );
    b.add(
        "fauna.dns.verify_records",
        RpcKindMeta {
            forbid_replay: false,
            // Each record may trigger a recursive DNS lookup (cache-first); a
            // full uncached domain matrix can be ~7 lookups. Allow more budget
            // than the pure-read `list_records`.
            default_deadline: Duration::from_secs(15),
            handler: verify_records_handler(),
        },
    );
    b.add(
        "fauna.dns.set_host_address",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_host_address_handler(),
        },
    );
    b.add(
        "fauna.dns.probe_txt_visible",
        RpcKindMeta {
            forbid_replay: false,
            // One authoritative-NS round: NS discovery plus a 5 s-timeout UDP
            // query per server. Must exceed the query's own worst case, or the
            // deadline would report a slow NS set as "not visible".
            default_deadline: Duration::from_secs(30),
            handler: probe_txt_visible_handler(),
        },
    );
}

#[cfg(test)]
use bytes::Bytes;
#[cfg(test)]
use fauna_protocol::encode_canonical;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    async fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    /// The primary domain's matrix carries one `_atproto.<localpart>.<primary>`
    /// TXT per ACTIVE ATProto identity (derived at read; `did=<did>` value),
    /// none for pending mints, and none at all on a non-public domain.
    #[tokio::test]
    async fn matrix_carries_atproto_handle_txt_for_active_identities() {
        let state = fixture_state().await;
        state
            .db
            .add_mail_domain(
                "fauna.example",
                true,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();

        let alice = [0x81u8; 32];
        state
            .db
            .create_user_with_handle(&alice, "free", "alice", None)
            .await
            .unwrap();
        state
            .db
            .upsert_atproto_identity_intent(&alice, "plc", "did:key:zDnaeUSER")
            .await
            .unwrap();

        // Pending (no DID yet) → no TXT row.
        let views = assemble_domain_views(&state, None).await.unwrap();
        assert!(
            !views[0]
                .records
                .iter()
                .any(|r| r.name.starts_with("_atproto.")),
            "pending mint must not publish a handle TXT"
        );

        state
            .db
            .record_atproto_minted(&alice, "did:plc:abc123", None)
            .await
            .unwrap();
        let views = assemble_domain_views(&state, None).await.unwrap();
        let row = views[0]
            .records
            .iter()
            .find(|r| r.name == "_atproto.alice.fauna.example")
            .expect("active identity publishes the handle TXT on the primary matrix");
        assert_eq!(row.record_type, "TXT");
        assert_eq!(row.expected, "\"did=did:plc:abc123\"");
    }

    /// A handle change re-derives the row under the NEW `_atproto` name and the
    /// OLD name leaves the matrix entirely (the handle is derived at read time,
    /// never frozen at mint — `atproto-pds-bridge.md` § Handle). This is the
    /// premise the client-side withdraw-aware converge is built on: because the
    /// old name is gone from here, only the client's remembered-name memory
    /// (`DnsConfig.atproto_published_names`) can still reach the stale TXT
    /// the rename left published (`fauna-client-dns::reconcile_atproto_txt`).
    /// The DID is unchanged — a rename never re-mints.
    #[tokio::test]
    async fn matrix_moves_the_atproto_txt_to_the_new_handle_on_rename() {
        let state = fixture_state().await;
        state
            .db
            .add_mail_domain(
                "fauna.example",
                true,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();

        let alice = [0x82u8; 32];
        state
            .db
            .create_user_with_handle(&alice, "free", "alice", None)
            .await
            .unwrap();
        state
            .db
            .upsert_atproto_identity_intent(&alice, "plc", "did:key:zDnaeUSER")
            .await
            .unwrap();
        state
            .db
            .record_atproto_minted(&alice, "did:plc:abc123", None)
            .await
            .unwrap();

        state.db.set_handle(&alice, "bob").await.unwrap();

        let views = assemble_domain_views(&state, None).await.unwrap();
        let atproto_rows: Vec<_> = views[0]
            .records
            .iter()
            .filter(|r| r.name.starts_with("_atproto."))
            .collect();
        assert_eq!(
            atproto_rows.len(),
            1,
            "a rename moves the row, never adds a second one"
        );
        assert_eq!(atproto_rows[0].name, "_atproto.bob.fauna.example");
        assert_eq!(
            atproto_rows[0].expected, "\"did=did:plc:abc123\"",
            "the DID is carried across a rename, never re-minted"
        );
    }

    /// A non-public primary (e.g. a `.local` LAN nest) never emits `_atproto`
    /// rows — ATProto presence is impossible there.
    #[tokio::test]
    async fn matrix_skips_atproto_txt_on_non_public_domain() {
        let state = fixture_state().await;
        state
            .db
            .add_mail_domain("nest.local", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let alice = [0x82u8; 32];
        state
            .db
            .create_user_with_handle(&alice, "free", "alice", None)
            .await
            .unwrap();
        state
            .db
            .upsert_atproto_identity_intent(&alice, "web", "")
            .await
            .unwrap();
        state
            .db
            .record_atproto_minted(&alice, "did:web:alice.nest.local", None)
            .await
            .unwrap();
        let views = assemble_domain_views(&state, None).await.unwrap();
        assert!(
            !views[0]
                .records
                .iter()
                .any(|r| r.name.starts_with("_atproto.")),
            "non-public domain must not emit _atproto rows"
        );
    }

    /// The primary domain's matrix carries `_fauna.<primary>` TXT
    /// `self=<nest_actor_id>` — the public-domain identity root a **fresh**
    /// client resolves to accept the self-signed floor (`tls-certificates.md`
    /// § E) and the row the deployment-seed rotation ceremony re-publishes
    /// (`box-recovery.md` § Deployment-seed rotation → *DNS row and the
    /// propagation window*). The value is read from the LIVE deployment key
    /// with no stored state, which is what makes a rotation move the value at
    /// this stable name — and in turn why the client side must converge
    /// (withdraw-aware) rather than only create.
    #[tokio::test]
    async fn matrix_carries_fauna_self_txt_on_the_primary_public_domain() {
        let state = fixture_state().await;
        state
            .db
            .add_mail_domain(
                "fauna.example",
                true,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();

        let views = assemble_domain_views(&state, None).await.unwrap();
        let row = views[0]
            .records
            .iter()
            .find(|r| r.name == "_fauna.fauna.example")
            .expect("the primary's matrix carries the identity-root TXT");
        assert_eq!(row.record_type, "TXT");
        assert_eq!(
            row.expected,
            format!(
                "\"self={}\"",
                hex::encode(state.nest_identity.public_key_bytes())
            ),
            "the value is the live deployment identity, hex-encoded exactly as \
             the reader decodes it (`fauna_core::hex32::decode` via \
             `resolve_dns_self_root`)"
        );
    }

    /// A non-public primary (a `.local` / LAN nest) emits no `_fauna` row: the
    /// reader refuses to resolve one there by construction
    /// (`resolve_dns_self_root` returns `None` for loopback / IP-literal /
    /// `.local`), so publishing it would be a record nothing can consult.
    #[tokio::test]
    async fn matrix_skips_fauna_self_txt_on_non_public_domain() {
        let state = fixture_state().await;
        state
            .db
            .add_mail_domain("nest.local", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let views = assemble_domain_views(&state, None).await.unwrap();
        assert!(
            !views[0]
                .records
                .iter()
                .any(|r| r.name.starts_with("_fauna.")),
            "non-public domain must not emit the identity-root TXT"
        );
    }

    /// The identity root is emitted on **every public active local domain**, not
    /// the primary alone (`dns-management.md` § Records covered, the
    /// `_fauna.<domain>` bullet — ruled 2026-08-13, `tls-certificates.md` § E).
    /// A secondary is a client entry point by construction (it gets apex A/AAAA
    /// → nest for reachability, and its apex joins the ACME order for the same
    /// reason), so a fresh app can arrive there and needs the same identity root
    /// the primary offers. The **value is identical** on every domain — one
    /// deployment, one identity — which is why this is a name-set widening and
    /// not a semantics change.
    #[tokio::test]
    async fn matrix_carries_fauna_self_txt_on_a_public_secondary_domain() {
        let state = fixture_state().await;
        state
            .db
            .add_mail_domain(
                "fauna.example",
                true,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        state
            .db
            .add_mail_domain(
                "second.example",
                false,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();

        let views = assemble_domain_views(&state, None).await.unwrap();
        let secondary = views
            .iter()
            .find(|v| v.domain == "second.example")
            .expect("the secondary domain has a matrix");
        let row = secondary
            .records
            .iter()
            .find(|r| r.name == "_fauna.second.example")
            .expect("a public secondary carries the deployment identity root");
        assert_eq!(row.record_type, "TXT");
        let expected = format!(
            "\"self={}\"",
            hex::encode(state.nest_identity.public_key_bytes())
        );
        assert_eq!(
            row.expected, expected,
            "the secondary's value is the SAME live deployment identity as the \
             primary's — one deployment has one identity, so arriving at any of \
             its domains must resolve the same expected nest"
        );
        let primary = views
            .iter()
            .find(|v| v.domain == "fauna.example")
            .expect("the primary domain has a matrix");
        let primary_row = primary
            .records
            .iter()
            .find(|r| r.name == "_fauna.fauna.example")
            .expect("the primary still carries its identity root");
        assert_eq!(
            primary_row.expected, expected,
            "widening must not change the primary's own row"
        );
    }

    /// The public-ness gate is **per domain**, not inherited from the primary: a
    /// `.local` secondary on an otherwise-public deployment emits no `_fauna`
    /// row, because the reader (`resolve_dns_self_root`) returns `None` for a
    /// name with no DNS authority, so the row would be unconsultable there.
    #[tokio::test]
    async fn matrix_skips_fauna_self_txt_on_a_non_public_secondary_domain() {
        let state = fixture_state().await;
        state
            .db
            .add_mail_domain(
                "fauna.example",
                true,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        state
            .db
            .add_mail_domain("nest.local", false, "enforce", "expand_primary", None, None)
            .await
            .unwrap();

        let views = assemble_domain_views(&state, None).await.unwrap();
        let secondary = views
            .iter()
            .find(|v| v.domain == "nest.local")
            .expect("the secondary domain has a matrix");
        assert!(
            !secondary
                .records
                .iter()
                .any(|r| r.name.starts_with("_fauna.")),
            "a non-public secondary must not emit the identity-root TXT"
        );
    }

    /// Like [`fixture_state`] but with a relay sidecar connected (or not) — the
    /// live fact `relay_sidecar_connected` reads.
    async fn fixture_state_with_relay(connected: bool) -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let st = AppState::for_test(db);
        st.relay_channels
            .store(usize::from(connected), std::sync::atomic::Ordering::Relaxed);
        Arc::new(st)
    }

    // ── 5b.3: cert-coupled DANE TLSA emission ────────────────────────────
    use crate::acme::{ServedCertFacts, ServedCertSpki};

    /// A [`ServedCertSpki`] double with canned facts + SPKI for `mail.<primary>`.
    struct FakeServed {
        facts: Option<ServedCertFacts>,
        spki: Option<[u8; 32]>,
    }
    impl ServedCertSpki for FakeServed {
        fn current_spki_sha256(&self) -> Option<[u8; 32]> {
            self.spki
        }
        fn served_cert_facts(&self, _sni: &str) -> Option<ServedCertFacts> {
            self.facts
        }
        fn served_cert_spki_sha256(&self, _sni: &str) -> Option<[u8; 32]> {
            self.spki
        }
    }

    fn facts(is_floor: bool) -> ServedCertFacts {
        ServedCertFacts {
            not_before_unix: 0,
            not_after_unix: 1,
            is_floor,
            covers: true,
        }
    }

    fn empty_view() -> DomainDns {
        DomainDns {
            domain: "example.com".to_string(),
            mode: "manual".to_string(),
            is_primary: true,
            records: vec![],
            extra: Default::default(),
        }
    }

    #[test]
    fn floor_mx_emits_one_tlsa_pinning_served_spki() {
        let spki = [0x11u8; 32];
        let resolver: Arc<dyn ServedCertSpki> = Arc::new(FakeServed {
            facts: Some(facts(true)),
            spki: Some(spki),
        });
        let mut view = empty_view();
        append_floor_mx_tlsa(Some(&resolver), "example.com", &mut view);
        let tlsa: Vec<_> = view
            .records
            .iter()
            .filter(|r| r.record_type == "TLSA")
            .collect();
        assert_eq!(tlsa.len(), 1, "exactly one host-level TLSA on the floor");
        assert_eq!(tlsa[0].name, "_25._tcp.mail.example.com");
        assert_eq!(tlsa[0].expected, format!("3 1 1 {}", "11".repeat(32)));
    }

    #[test]
    fn trusted_mx_withdraws_the_tlsa() {
        let resolver: Arc<dyn ServedCertSpki> = Arc::new(FakeServed {
            facts: Some(facts(false)),
            spki: Some([0x22u8; 32]),
        });
        let mut view = empty_view();
        append_floor_mx_tlsa(Some(&resolver), "example.com", &mut view);
        assert!(
            view.records.iter().all(|r| r.record_type != "TLSA"),
            "a trusted MX cert must not carry the floor-key TLSA"
        );
    }

    #[test]
    fn no_resolver_or_pending_emits_no_tlsa() {
        // No resolver wired at all.
        let mut view = empty_view();
        append_floor_mx_tlsa(None, "example.com", &mut view);
        assert!(view.records.is_empty());
        // On-floor but no SPKI served yet (pending) → still nothing to pin.
        let resolver: Arc<dyn ServedCertSpki> = Arc::new(FakeServed {
            facts: Some(facts(true)),
            spki: None,
        });
        append_floor_mx_tlsa(Some(&resolver), "example.com", &mut view);
        assert!(view.records.iter().all(|r| r.record_type != "TLSA"));
    }

    #[tokio::test]
    async fn list_records_requires_admin_class() {
        let state = fixture_state().await;
        let non_admin = [99u8; 32];
        // A KNOWN user of the wrong class — an unseeded actor is refused one
        // arm earlier with the central `fauna.bridges.permission_denied`, which
        // is not what this test pins.
        state
            .db
            .create_user(&non_admin, "free", "test")
            .await
            .unwrap();
        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = list_records_handler()(state, non_admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.dns.permission_denied");
    }

    #[tokio::test]
    async fn list_records_empty_on_fresh_nest() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();
        assert!(reply.domains.is_empty());
    }

    #[tokio::test]
    async fn list_records_assembles_matrix_with_dkim() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        // A provisioned DKIM selector for the domain (unsealed value + a dummy
        // sealed blob; only the public_dns_value is surfaced here).
        state
            .db
            .seat_dkim_selector_for_test("example.com", "ed25519", "v=DKIM1; k=ed25519; p=abc")
            .await;

        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();

        assert_eq!(reply.domains.len(), 1);
        let d = &reply.domains[0];
        assert_eq!(d.domain, "example.com");
        assert_eq!(d.mode, "manual");

        let by_name = |suffix: &str| d.records.iter().find(|r| r.name.contains(suffix));
        // MX → mail.<primary>. — emitted as an FQDN (trailing dot) so providers
        // that treat a dotless target as zone-relative don't double-append the
        // origin (regression; live example.com inbound-bounce fix).
        let mx = d.records.iter().find(|r| r.record_type == "MX").unwrap();
        assert_eq!(mx.expected, "10 mail.example.com.");
        // DKIM uses the stored public_dns_value verbatim.
        let dkim = by_name("ed25519._domainkey").unwrap();
        assert_eq!(dkim.name, "ed25519._domainkey.example.com");
        assert_eq!(dkim.expected, "\"v=DKIM1; k=ed25519; p=abc\"");
        // And the selector the nest minted when the domain was added is on the
        // page beside it — a nest-held key lists exactly as a sealed one does.
        let minted = by_name("default._domainkey").unwrap();
        assert!(
            minted.expected.starts_with("\"v=DKIM1; k=ed25519; p="),
            "{}",
            minted.expected
        );
        // The other four mail records are present.
        assert!(by_name("_dmarc.example.com").is_some());
        assert!(by_name("_mta-sts.example.com").is_some());
        assert!(by_name("_smtp._tls.example.com").is_some());
        // CalDAV autodiscovery SRV → mail.<primary> on 443, type label "SRV".
        let srv = d.records.iter().find(|r| r.record_type == "SRV").unwrap();
        assert_eq!(srv.name, "_caldavs._tcp.example.com");
        assert_eq!(srv.expected, "0 1 443 mail.example.com.");
        // SPF is the domain's stored body (default), quoted.
        let spf = d
            .records
            .iter()
            .find(|r| r.record_type == "TXT" && r.name == "example.com")
            .unwrap();
        assert_eq!(spf.expected, "\"v=spf1 mx ~all\"");
    }

    /// The `_dmarc` body is the goal-doc default on a plain domain and reflects a
    /// stored per-domain override; the `rua` always points at the deployment-wide
    /// `dmarc-report@<primary>` processor, not the per-domain owner name.
    /// `docs/goal/behavior/dmarc-reporting.md` § Record shape + Multi-domain.
    #[tokio::test]
    async fn list_records_dmarc_body_default_and_override() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        // Primary domain (no override) + a secondary domain rolling out at p=none.
        state
            .db
            .add_mail_domain(
                "primary.example",
                true,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        state
            .db
            .add_mail_domain(
                "secondary.example",
                false,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();
        state
            .db
            .update_mail_domain_config(
                "secondary.example",
                crate::db::mail_domains::MailDomainUpdate {
                    dmarc_overrides_json: Some(Some(r#"{"policy_mode":"none","pct":50}"#.into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();

        let dmarc_for = |domain: &str| -> String {
            let d = reply
                .domains
                .iter()
                .find(|d| d.domain == domain)
                .unwrap_or_else(|| panic!("domain {domain} present"));
            d.records
                .iter()
                .find(|r| r.name == format!("_dmarc.{domain}"))
                .unwrap_or_else(|| panic!("_dmarc.{domain} present"))
                .expected
                .clone()
        };

        // Primary: full default-strict body, rua at the primary processor.
        assert_eq!(
            dmarc_for("primary.example"),
            "\"v=DMARC1; p=reject; sp=reject; pct=100; adkim=s; aspf=s; \
             rua=mailto:dmarc-report@primary.example; ri=86400\""
        );
        // Secondary: override softens p/pct; unspecified tags inherit; rua still
        // points at the deployment-wide primary processor (not secondary.example).
        assert_eq!(
            dmarc_for("secondary.example"),
            "\"v=DMARC1; p=none; sp=reject; pct=50; adkim=s; aspf=s; \
             rua=mailto:dmarc-report@primary.example; ri=86400\""
        );
    }

    #[tokio::test]
    async fn verify_records_requires_admin_class() {
        let state = fixture_state().await;
        let non_admin = [99u8; 32];
        // A KNOWN user of the wrong class (see list_records_requires_admin_class).
        state
            .db
            .create_user(&non_admin, "free", "test")
            .await
            .unwrap();
        let req = VerifyRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = verify_records_handler()(state, non_admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.dns.permission_denied");
    }

    /// The propagation-gate probe is Admin-only like the rest of `fauna.dns.*`.
    /// It reads only public DNS and carries no credential, but it sits on the
    /// admin cert surface and must not become the one un-gated hole in the
    /// namespace.
    #[tokio::test]
    async fn probe_txt_visible_requires_admin_class() {
        let state = fixture_state().await;
        let non_admin = [99u8; 32];
        // A KNOWN user of the wrong class (see list_records_requires_admin_class).
        state
            .db
            .create_user(&non_admin, "free", "test")
            .await
            .unwrap();
        let req = ProbeTxtVisibleRequest {
            zone_name: "example.com".into(),
            record_name: "_acme-challenge.example.com".into(),
            txt_value: "tok".into(),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = probe_txt_visible_handler()(state, non_admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.dns.permission_denied");
    }

    #[tokio::test]
    async fn set_host_address_requires_admin_class() {
        let state = fixture_state().await;
        let non_admin = [99u8; 32];
        // A KNOWN user of the wrong class (see list_records_requires_admin_class).
        state
            .db
            .create_user(&non_admin, "free", "test")
            .await
            .unwrap();
        let req = SetHostAddressRequest {
            nest_ipv4: "203.0.113.7".into(),
            mail_ipv4: "203.0.113.7".into(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = set_host_address_handler()(state, non_admin, payload)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.dns.permission_denied");
    }

    /// End-to-end (nest half): admin persists a host address via
    /// `set_host_address`, then `list_records` surfaces the host A/AAAA/PTR rows
    /// on the *primary* domain — with the mail-host A resolving to a DIFFERENT IP
    /// than the apex A (the mx-IP-≠-apex-IP directive).
    #[tokio::test]
    async fn set_host_address_then_list_includes_host_rows() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();

        // Persist the address: apex on one box, mail host on another.
        let set_req = SetHostAddressRequest {
            nest_ipv4: "203.0.113.7".into(),
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9".into(),
            mail_ipv6: Some("2001:db8::9".into()),
            extra: Default::default(),
        };
        let set_payload = Bytes::from(encode_canonical(&set_req).unwrap().to_vec());
        set_host_address_handler()(state.clone(), admin, set_payload)
            .await
            .expect("admin set ok");

        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();
        let d = &reply.domains[0];

        // Apex A → nest IP; mail A → distinct mail-host IP.
        let apex_a = d
            .records
            .iter()
            .find(|r| r.record_type == "A" && r.name == "example.com")
            .expect("apex A row");
        assert_eq!(apex_a.expected, "203.0.113.7");
        let mail_a = d
            .records
            .iter()
            .find(|r| r.record_type == "A" && r.name == "mail.example.com")
            .expect("mail A row");
        assert_eq!(mail_a.expected, "198.51.100.9");
        // AAAA only for the mail host (the only IPv6 set).
        let mail_aaaa = d
            .records
            .iter()
            .find(|r| r.record_type == "AAAA" && r.name == "mail.example.com")
            .expect("mail AAAA row");
        assert_eq!(mail_aaaa.expected, "2001:db8::9");
        assert!(
            !d.records
                .iter()
                .any(|r| r.record_type == "AAAA" && r.name == "example.com"),
            "no apex AAAA — nest has no IPv6"
        );
        // Advisory PTR(s) → mail.<primary>; v4 owner name is the reverse pointer.
        let ptr_v4 = d
            .records
            .iter()
            .find(|r| r.record_type == "PTR" && r.name == "9.100.51.198.in-addr.arpa")
            .expect("mail v4 PTR row");
        assert_eq!(ptr_v4.expected, "mail.example.com");
        assert!(
            d.records
                .iter()
                .any(|r| r.record_type == "PTR" && r.name.ends_with(".ip6.arpa")),
            "mail v6 PTR row"
        );
    }

    /// A **secondary** local domain gets the client-reachability apex `A`/`AAAA`
    /// → the **nest** address (parity with the primary's apex), so a client can
    /// reach this nest via `bob@domain2`. It must NOT carry the primary-only
    /// mail-host `A` / `PTR` rows (mail rides the shared `mail.<primary>`).
    /// `mail-multidomain.md` § Client reachability of a secondary domain.
    #[tokio::test]
    async fn secondary_domain_gets_client_reachability_apex_a() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        state
            .db
            .add_mail_domain(
                "domain2.example",
                false,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();

        let set_req = SetHostAddressRequest {
            nest_ipv4: "203.0.113.7".into(),
            nest_ipv6: Some("2001:db8::7".into()),
            mail_ipv4: "198.51.100.9".into(),
            mail_ipv6: None,
            extra: Default::default(),
        };
        let set_payload = Bytes::from(encode_canonical(&set_req).unwrap().to_vec());
        set_host_address_handler()(state.clone(), admin, set_payload)
            .await
            .expect("admin set ok");

        let req = ListRecordsRequest {
            domain: Some("domain2.example".into()),
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();
        let d = &reply.domains[0];
        assert_eq!(d.domain, "domain2.example");

        // Apex A/AAAA → the NEST address (so `domain2.example` resolves here).
        let apex_a = d
            .records
            .iter()
            .find(|r| r.record_type == "A" && r.name == "domain2.example")
            .expect("secondary apex A row");
        assert_eq!(apex_a.expected, "203.0.113.7");
        let apex_aaaa = d
            .records
            .iter()
            .find(|r| r.record_type == "AAAA" && r.name == "domain2.example")
            .expect("secondary apex AAAA row");
        assert_eq!(apex_aaaa.expected, "2001:db8::7");

        // The primary-only host rows must NOT leak onto the secondary.
        assert!(
            !d.records.iter().any(|r| r.name.starts_with("mail.")),
            "secondary must not carry a mail-host A row (mail rides mail.<primary>)"
        );
        assert!(
            !d.records.iter().any(|r| r.record_type == "PTR"),
            "secondary must not carry a PTR row"
        );
    }

    #[tokio::test]
    async fn rename_new_primary_gets_mail_host_row() {
        // While a primary-domain rename is pre-flip, its new-primary domain (still
        // a secondary) ALSO gets a `mail.<new>` A/AAAA → mail-host row so the client
        // publishes it and ACME HTTP-01 can resolve `mail.<new>` for the widened
        // cert SAN (mail-primary-domain-rename.md § cert chain re-issue ordering).
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        let old = state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let new = state
            .db
            .add_mail_domain(
                "new.example",
                false,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();

        let set_req = SetHostAddressRequest {
            nest_ipv4: "203.0.113.7".into(),
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9".into(),
            mail_ipv6: None,
            extra: Default::default(),
        };
        let set_payload = Bytes::from(encode_canonical(&set_req).unwrap().to_vec());
        set_host_address_handler()(state.clone(), admin, set_payload)
            .await
            .expect("admin set ok");

        // No rename yet → the secondary carries no `mail.` row.
        let list = |domain: &str| {
            let state = state.clone();
            let domain = domain.to_string();
            async move {
                let req = ListRecordsRequest {
                    domain: Some(domain),
                    extra: Default::default(),
                };
                let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
                let bytes = list_records_handler()(state, admin, payload)
                    .await
                    .expect("admin ok");
                decode::<ListRecordsReply>(&bytes).unwrap().domains
            }
        };
        let before = list("new.example").await;
        assert!(
            !before[0]
                .records
                .iter()
                .any(|r| r.name.starts_with("mail.")),
            "no mail.<new> row before a rename references the domain"
        );

        // Start a rename (pre-flip) referencing new.example as the new primary.
        state
            .db
            .insert_domain_rename(&old.domain_id, &new.domain_id, 7, &admin)
            .await
            .expect("insert rename");

        let after = list("new.example").await;
        let d = &after[0];
        // The secondary apex A still points at the NEST address …
        let apex_a = d
            .records
            .iter()
            .find(|r| r.record_type == "A" && r.name == "new.example")
            .expect("secondary apex A");
        assert_eq!(apex_a.expected, "203.0.113.7");
        // … and now ALSO a `mail.new.example` A → the MAIL address (not the nest).
        let mail_a = d
            .records
            .iter()
            .find(|r| r.record_type == "A" && r.name == "mail.new.example")
            .expect("rename mail-host A row must be emitted while pre-flip");
        assert_eq!(
            mail_a.expected, "198.51.100.9",
            "the rename mail-host row must point at mail_ipv4, not nest_ipv4"
        );

        // Aborting the rename (terminal) withdraws the `mail.<new>` row again.
        let active = state.db.get_active_rename().await.unwrap().unwrap();
        state
            .db
            .mark_rename_aborted(&active.rename_id, None)
            .await
            .unwrap();
        let after_abort = list("new.example").await;
        assert!(
            !after_abort[0]
                .records
                .iter()
                .any(|r| r.name.starts_with("mail.")),
            "aborting the rename withdraws the mail.<new> row"
        );
    }

    #[tokio::test]
    async fn rename_grace_keeps_old_primary_mail_host_row() {
        // Post-flip (grace), the old primary becomes a secondary; the assembler
        // keeps its `mail.<old>` A → mail-host row alive so peers holding a cached
        // `<domain> MX → mail.<old>` keep resolving it through grace
        // (mail-primary-domain-rename.md § Data — grace-window keep-alive; SLICE 3).
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        let old = state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let new = state
            .db
            .add_mail_domain(
                "new.example",
                false,
                "enforce",
                "expand_primary",
                None,
                None,
            )
            .await
            .unwrap();

        let set_req = SetHostAddressRequest {
            nest_ipv4: "203.0.113.7".into(),
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9".into(),
            mail_ipv6: None,
            extra: Default::default(),
        };
        let set_payload = Bytes::from(encode_canonical(&set_req).unwrap().to_vec());
        set_host_address_handler()(state.clone(), admin, set_payload)
            .await
            .expect("admin set ok");

        // Drive the rename through the atomic flip (is_primary old→new).
        let row = state
            .db
            .insert_domain_rename(&old.domain_id, &new.domain_id, 7, &admin)
            .await
            .unwrap();
        state
            .db
            .advance_rename_to_cert_issuance(&row.rename_id)
            .await
            .unwrap();
        state
            .db
            .mark_rename_cert_ready(&row.rename_id, "fp", 111)
            .await
            .unwrap();
        state
            .db
            .advance_rename_to_grace(&row.rename_id)
            .await
            .unwrap();
        // new.example is now the primary; example.com is the demoted secondary.
        let primary = state
            .db
            .lookup_primary_mail_domain()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(primary.domain_name, "new.example");

        let list = |domain: &str| {
            let state = state.clone();
            let domain = domain.to_string();
            async move {
                let req = ListRecordsRequest {
                    domain: Some(domain),
                    extra: Default::default(),
                };
                let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
                let bytes = list_records_handler()(state, admin, payload)
                    .await
                    .expect("admin ok");
                decode::<ListRecordsReply>(&bytes).unwrap().domains
            }
        };

        // The demoted old primary keeps its `mail.<old>` A → the MAIL address …
        let old_view = list("example.com").await;
        let d = &old_view[0];
        let mail_a = d
            .records
            .iter()
            .find(|r| r.record_type == "A" && r.name == "mail.example.com")
            .expect("grace keeps mail.<old> A alive on the demoted primary");
        assert_eq!(
            mail_a.expected, "198.51.100.9",
            "mail.<old> points at the mail address, not the nest"
        );
        // … and, now a secondary, its apex A points at the nest.
        let apex_a = d
            .records
            .iter()
            .find(|r| r.record_type == "A" && r.name == "example.com")
            .expect("secondary apex A");
        assert_eq!(apex_a.expected, "203.0.113.7");
    }

    /// No host address persisted → no host rows (fresh nest stays mail-only).
    #[tokio::test]
    async fn list_omits_host_rows_when_address_unset() {
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();

        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();
        let d = &reply.domains[0];
        assert!(
            !d.records
                .iter()
                .any(|r| matches!(r.record_type.as_str(), "A" | "AAAA" | "PTR")),
            "no host rows before the address hand-off runs"
        );
    }

    /// With the host address persisted AND the `iroh_relay` service enabled, the
    /// expected-DNS matrix surfaces the `relay.<apex>` A row at the **nest** IP —
    /// so an admin enabling the relay knows to publish the A record the apex ACME
    /// order pre-checks before adding the SAN.
    #[tokio::test]
    async fn list_includes_relay_a_row_when_iroh_relay_enabled() {
        let state = fixture_state_with_relay(true).await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        // Split box: mail host on a different IP than the nest, to prove the relay
        // row tracks the NEST address (it is SNI-routed on the nest's :443).
        let set_req = SetHostAddressRequest {
            nest_ipv4: "203.0.113.7".into(),
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9".into(),
            mail_ipv6: None,
            extra: Default::default(),
        };
        let set_payload = Bytes::from(encode_canonical(&set_req).unwrap().to_vec());
        set_host_address_handler()(state.clone(), admin, set_payload)
            .await
            .expect("admin set ok");

        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();
        let d = &reply.domains[0];
        let relay_a = d
            .records
            .iter()
            .find(|r| r.record_type == "A" && r.name == "relay.example.com")
            .expect("relay A row");
        assert_eq!(relay_a.expected, "203.0.113.7", "relay → nest IP");
    }

    /// Relay disabled (the default) → no `relay.<apex>` row even with the host
    /// address set: the safe side of the foot-gun (nothing prompts the admin to
    /// publish a record the ACME order will never ask for).
    #[tokio::test]
    async fn list_omits_relay_row_when_iroh_relay_disabled() {
        let state = fixture_state_with_relay(false).await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        let set_req = SetHostAddressRequest {
            nest_ipv4: "203.0.113.7".into(),
            nest_ipv6: None,
            mail_ipv4: "203.0.113.7".into(),
            mail_ipv6: None,
            extra: Default::default(),
        };
        let set_payload = Bytes::from(encode_canonical(&set_req).unwrap().to_vec());
        set_host_address_handler()(state.clone(), admin, set_payload)
            .await
            .expect("admin set ok");

        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();
        let d = &reply.domains[0];
        assert!(
            !d.records.iter().any(|r| r.name == "relay.example.com"),
            "no relay row when iroh_relay is disabled"
        );
    }

    // ── pds.<primary> (ATProto PDS bridge) ───────────────────────────────────

    /// Drive the whole matrix flow for a box with an **approved** `atproto.pds`
    /// bridge service user: the row must appear at the **nest** IP, because the
    /// bridge's XRPC listener is SNI-routed on the nest's :443. Without this row
    /// nothing tells the admin to publish the A record, and the apex ACME order's
    /// resolve gate (`acme_http01::pds_san_included`) would defer the
    /// `pds.<apex>` SAN forever — leaving Bluesky clients on the untrusted floor
    /// cert (`atproto-pds-full.md` § Implementation status → F1 remainder (a)).
    #[tokio::test]
    async fn list_includes_pds_a_row_when_atproto_pds_bridge_approved() {
        use crate::db::bridge_service_users::BridgeRole;

        let state = fixture_state_with_relay(false).await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        // Stand the bridge up the way an admin does: enroll, then approve.
        let bridge_pk = [42u8; 32];
        state
            .db
            .create_pending_bridge_service_user(&bridge_pk, BridgeRole::AtprotoPds, "atproto-1")
            .await
            .unwrap();
        state
            .db
            .approve_bridge_service_user(&bridge_pk, Some(&admin))
            .await
            .unwrap();
        // Split box: mail host on a different IP than the nest, to prove the pds
        // row tracks the NEST address.
        let set_req = SetHostAddressRequest {
            nest_ipv4: "203.0.113.7".into(),
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9".into(),
            mail_ipv6: None,
            extra: Default::default(),
        };
        let set_payload = Bytes::from(encode_canonical(&set_req).unwrap().to_vec());
        set_host_address_handler()(state.clone(), admin, set_payload)
            .await
            .expect("admin set ok");

        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();
        let d = &reply.domains[0];
        let pds_a = d
            .records
            .iter()
            .find(|r| r.record_type == "A" && r.name == "pds.example.com")
            .expect("pds A row");
        assert_eq!(pds_a.expected, "203.0.113.7", "pds → nest IP");
        // The relay is off here — the two gates must be independent.
        assert!(
            !d.records.iter().any(|r| r.name == "relay.example.com"),
            "an approved atproto.pds bridge must not drag in the relay row"
        );
    }

    /// A **pending** (un-approved) `atproto.pds` enrollment is not a running
    /// bridge: nothing is listening on `pds.<apex>`, so the matrix must stay
    /// quiet. The role always takes the manual admin approval card, so this is
    /// the state every box sits in between enrollment and approval.
    #[tokio::test]
    async fn list_omits_pds_row_when_atproto_pds_bridge_only_pending() {
        use crate::db::bridge_service_users::BridgeRole;

        let state = fixture_state_with_relay(false).await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        state
            .db
            .create_pending_bridge_service_user(&[42u8; 32], BridgeRole::AtprotoPds, "atproto-1")
            .await
            .unwrap();
        let set_req = SetHostAddressRequest {
            nest_ipv4: "203.0.113.7".into(),
            nest_ipv6: None,
            mail_ipv4: "203.0.113.7".into(),
            mail_ipv6: None,
            extra: Default::default(),
        };
        let set_payload = Bytes::from(encode_canonical(&set_req).unwrap().to_vec());
        set_host_address_handler()(state.clone(), admin, set_payload)
            .await
            .expect("admin set ok");

        let req = ListRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = list_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: ListRecordsReply = decode(&reply_bytes).unwrap();
        let d = &reply.domains[0];
        assert!(
            !d.records.iter().any(|r| r.name.starts_with("pds.")),
            "a pending (un-approved) atproto.pds enrollment emits no pds row"
        );
    }

    #[tokio::test]
    async fn verify_records_reports_checking_for_full_matrix_with_null_resolver() {
        // `for_test` installs the Null resolver, so every record resolves to
        // "checking" — this asserts the handler assembles the same matrix as
        // `list_records` and runs each record through the verifier. The real
        // ok/missing/mismatch logic is covered by the `dns_verifier` +
        // `fauna_mail::dns::verify` unit tests.
        let state = fixture_state().await;
        let admin = [7u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();
        state
            .db
            .add_mail_domain("example.com", true, "enforce", "expand_primary", None, None)
            .await
            .unwrap();
        state
            .db
            .seat_dkim_selector_for_test("example.com", "ed25519", "v=DKIM1; k=ed25519; p=abc")
            .await;

        let req = VerifyRecordsRequest {
            domain: None,
            extra: Default::default(),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply_bytes = verify_records_handler()(state, admin, payload)
            .await
            .expect("admin ok");
        let reply: VerifyRecordsReply = decode(&reply_bytes).unwrap();

        assert_eq!(reply.domains.len(), 1);
        let d = &reply.domains[0];
        assert_eq!(d.domain, "example.com");
        // Full per-domain matrix: MX, SPF, 2×DKIM (the selector this test
        // provisions and the one the nest minted when the domain was added),
        // DMARC, MTA-STS, TLSRPT, CalDAV SRV, CardDAV SRV, `_fauna`
        // identity-root TXT = 10. (The last is the deployment identity root —
        // `append_fauna_self_txt`, emitted on every public local domain, so a
        // secondary's matrix carries it too.)
        assert_eq!(d.records.len(), 10);
        // Null resolver → every verdict is "checking" with no observed values,
        // and the (name, record_type) merge keys mirror `list_records`.
        assert!(d.records.iter().all(|r| r.status == "checking"));
        assert!(d.records.iter().all(|r| r.observed.is_empty()));
        let mx = d.records.iter().find(|r| r.record_type == "MX").unwrap();
        assert_eq!(mx.name, "example.com");
        assert!(
            d.records
                .iter()
                .any(|r| r.name == "ed25519._domainkey.example.com")
        );
        // CalDAV + CardDAV autodiscovery SRVs are both part of the verified matrix.
        assert!(
            d.records
                .iter()
                .any(|r| r.record_type == "SRV" && r.name == "_caldavs._tcp.example.com")
        );
        assert!(
            d.records
                .iter()
                .any(|r| r.record_type == "SRV" && r.name == "_carddavs._tcp.example.com")
        );
    }
}
