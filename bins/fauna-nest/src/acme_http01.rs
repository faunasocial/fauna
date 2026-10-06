//! HTTP-01 ACME challenge solver for automatic TLS certificate acquisition.
//!
//! Provides a complete ACME HTTP-01 flow using Let's Encrypt:
//! - Serves `/.well-known/acme-challenge/{token}` responses on an HTTP listener
//! - Redirects all other HTTP traffic to HTTPS
//! - Obtains and renews certificates automatically
//!
//! The HTTP listener runs on a configurable port (default 8080), which should
//! be mapped to port 80 by Docker or a reverse proxy.
//!
//! The generic HTTP-01 machinery (challenge state, port-80 router/listener,
//! order flow, cert inspectors, retry budget) lives in `fauna-acme-http01`
//! (shared with the fauna.social front door — front-door.md § TLS policy) and
//! is re-exported below; this module keeps the NEST policy: SAN derivation,
//! resolve gates, and the certificate lifecycle task.

use std::sync::Arc;

use anyhow::Result;

use crate::acme::CERT_FILENAME;

pub use fauna_acme_http01::{
    ACME_FAILED_VALIDATION_BUDGET_PER_HOUR, ACME_RATE_WINDOW_SECS, ChallengeState, Http01Config,
    RETRY_STATE_FILENAME, RetryState, cert_covers_sans, cert_dns_sans, cert_seconds_remaining,
    http01_router, next_attempt_delay, now_unix, obtain_certificate, start_http01_listener,
};

/// Build the desired ACME SAN list for a mail deployment.
///
/// `node_domain` (the nest's apex, deployment-topology config) is always
/// first → it becomes the cert's primary name and is the one identifier we
/// know always resolves to the nest.
///
/// **The deployment has exactly ONE MX host — `mail.<primary>` (== the apex's
/// `mail.` host, since the primary mail domain IS the identity apex,
/// `mail-multidomain.md` § Architectural rules "One MX target").** Every local
/// domain, primary and secondary alike, MXes to it; there is **no**
/// `mail.<secondary>` host — a secondary domain's DNS set publishes only its
/// apex `A` → the nest (`fauna_mail::dns::host::build_secondary_apex_records`).
/// So the mail host is added **once**, for the apex, whenever the deployment
/// serves mail — never once per domain. Emitting `mail.<secondary>` (which has
/// no `A` record by design) would drop an unvalidatable identifier into the
/// all-or-nothing ACME order and fail the WHOLE order, taking the primary's
/// cert down with it — the exact multi-domain regression this shape prevents.
///
/// Each active mail domain then contributes its own **apex** `<domain>` — the
/// client-reachability name (a client that found a handle as `bob@<domain>`
/// reaches the nest via `https://<domain>`, `mail-multidomain.md` § Client
/// reachability). The primary dedups against `node_domain`. Callers
/// resolve-gate the *secondaries* they pass in ([`reachable_mail_domains`]) so
/// an unpublished or mis-pointed secondary apex never joins the order. (The
/// deferred `expand_primary` cert-mode dispatcher adds `mta-sts.<secondary>`
/// SANs; not built yet — `mail-multidomain.md` § Per-domain MTA-STS.)
/// Deduplicated, stable order.
///
/// `infra` gates the **infra-subdomain** SANs — `relay.<apex>` (the self-hosted
/// iroh P2P relay sidecar's SNI-routed HTTPS name) and `pds.<apex>` (the
/// out-of-process ATProto PDS bridge's). See [`InfraSans`] for why each one is
/// resolve-gated rather than flag-gated, and [`relay_san_included`] /
/// [`pds_san_included`] for the combinators callers use to compute it. One of
/// each per nest, on the apex; per-mail-domain infra hosts are not used.
/// `apex_sans` gates the **apex's own two** SANs — `<apex>` and `mail.<apex>` —
/// which every caller but one leaves included ([`ApexSans::INCLUDED`]); see
/// [`ApexSans`] for the single situation that drops them (an in-flight rename
/// away from a dead old primary). The infra SANs still hang off the apex name
/// and keep their own independent resolve gates — a dead apex fails those too,
/// so they need no second rule here.
pub fn desired_san_domains(
    node_domain: &str,
    mail_domain_names: &[String],
    infra: &InfraSans,
    apex_sans: ApexSans,
) -> Vec<String> {
    fn add(out: &mut Vec<String>, candidate: &str) {
        let d = candidate.trim().to_ascii_lowercase();
        if !d.is_empty() && !out.iter().any(|x| x == &d) {
            out.push(d);
        }
    }
    let apex_lc = node_domain.trim().to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    if apex_sans.apex {
        add(&mut out, node_domain);
    }
    // The single MX host (`mail.<primary>` == `mail.<apex>`), added once when the
    // deployment serves mail — NOT per-domain (no `mail.<secondary>`; see the doc
    // above). Gated on there being at least one active mail domain so a mail-off
    // box never orders an unresolvable `mail.<apex>`.
    let serves_mail = mail_domain_names.iter().any(|n| !n.trim().is_empty());
    if serves_mail && apex_sans.mail_host && !node_domain.trim().is_empty() {
        add(&mut out, &format!("mail.{}", node_domain.trim()));
    }
    // Each active mail domain's apex (client reachability); the primary dedups
    // against `node_domain`. Secondaries are resolve-gated by the caller.
    for name in mail_domain_names {
        // `list_active_mail_domains` carries the primary too, so a gated-out apex
        // has to be skipped HERE as well — otherwise this loop silently re-admits
        // the very name the gate just dropped.
        if !apex_sans.apex && name.trim().to_ascii_lowercase() == apex_lc {
            continue;
        }
        add(&mut out, name);
    }
    // Infra-subdomain hosts, each independently resolve-gated by the caller. Order
    // is stable (relay before pds) so a cert-coverage diff never churns. A
    // domainless box has no apex to hang them off, so it emits none.
    let apex = node_domain.trim();
    if !apex.is_empty() {
        for label in infra.included_labels() {
            add(&mut out, &format!("{label}.{apex}"));
        }
    }
    out
}

/// Which resolve-gated **infra-subdomain** SANs may join the apex ACME order on
/// this cycle — one named field per infra subdomain the deployment can serve, so
/// a call site can never silently swap two positional `bool`s (and adding the
/// next one is a compile error at every call site, which is the point: the blast
/// radius of getting a gate wrong is the **apex** cert, below).
///
/// Every field means the same thing: *the service is enabled **AND** its
/// `<label>.<apex>` name currently resolves*. Both halves are load-bearing. An
/// ACME order is **all-or-nothing** — any identifier that fails validation fails
/// the *whole* order, apex included (`tls-certificates.md` § HTTP-01, "HTTP-01
/// cannot validate an unreachable A record") — and an admin can always enable a
/// service before publishing its A record. So a flag-only gate would drop an
/// unresolvable name into the order and take **apex HTTPS down**.
/// Pre-resolving decouples them: a missing or
/// transiently-unresolvable infra name just defers its SAN to a later cycle.
///
/// None of these SANs is critical to serving, because the self-signed **floor**
/// cert carries `relay.<apex>` and `pds.<apex>` *unconditionally* (no order to
/// fail — `self_signed_cert::write_self_signed_bootstrap`), so LAN/floor serving
/// never depends on an A record. The managed cert only upgrades them to *trusted*.
///
/// `Default` is all-false (order the apex + mail names only) — the safe side, and
/// the shape that lets fixtures use `..Default::default()` so two branches growing
/// this struct merge cleanly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InfraSans {
    /// `relay.<apex>` — the self-hosted iroh P2P relay sidecar
    /// ([`relay_san_included`]).
    pub relay: bool,
    /// `pds.<apex>` — the out-of-process ATProto PDS bridge, which terminates
    /// that hostname's TLS itself from the sealed cert blob this order produces
    /// ([`pds_san_included`]).
    pub pds: bool,
}

/// Which of the **apex's own two** SANs — `<apex>` (the identity apex, the
/// client-reachability name) and `mail.<apex>` (the deployment's single MX host)
/// — may join the order this cycle.
///
/// Normally both, and that is the *safe* side here: the apex is the one name we
/// know reaches this nest, and dropping it narrows the deployment's own cert.
/// Exactly one situation licenses dropping them — an **in-flight primary-domain
/// rename**, where the apex is by construction the domain being renamed *away
/// from* (`mail-primary-domain-rename.md` § Renaming away from a dead domain).
/// When that old zone is lapsed or seized its names cannot answer an HTTP-01
/// challenge, and an ACME order is all-or-nothing — so leaving them in fails
/// *every* order, **including the `cert_issuance` order that must add
/// `mail.<new>`**: the machinery built to leave the dead zone is held hostage by
/// it. Gating them out lets the widened cert issue for what actually resolves,
/// and the flip proceeds. A **live** old zone passes the gate and keeps both
/// SANs, so the voluntary rename's dual-binding is untouched.
///
/// Deliberately **no `Default`**, unlike [`InfraSans`]: the two types' safe sides
/// are opposites (omit an unresolvable infra SAN; *keep* the apex), so a call
/// site must say [`ApexSans::INCLUDED`] out loud rather than inherit a polarity
/// from whichever neighbouring struct it copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApexSans {
    /// `<apex>` — the identity apex / client-reachability name.
    pub apex: bool,
    /// `mail.<apex>` — the deployment's single MX host.
    pub mail_host: bool,
}

impl ApexSans {
    /// Both apex SANs in the order — every caller outside an in-flight rename,
    /// and the verdict a rename over a **live** old zone produces.
    pub const INCLUDED: Self = Self {
        apex: true,
        mail_host: true,
    };

    /// Neither apex SAN in the order — the dead/uncertain old primary under an
    /// in-flight rename ([`apex_sans_for_cycle`]).
    pub const DROPPED: Self = Self {
        apex: false,
        mail_host: false,
    };
}

impl InfraSans {
    /// The infra subdomain labels currently let into the order, in stable order.
    fn included_labels(&self) -> impl Iterator<Item = &'static str> + '_ {
        [("relay", self.relay), ("pds", self.pds)]
            .into_iter()
            .filter_map(|(label, on)| on.then_some(label))
    }
}

/// Best-effort check that `<label>.<apex>` currently has a published `A`/`AAAA`
/// record — the resolve half of every [`InfraSans`] gate (see that type for why
/// an unresolvable infra name would take the **apex** cert down).
///
/// Fail-safe: returns `true` ONLY on a confirmed A/AAAA record; every other
/// outcome (NXDOMAIN / empty RRset / resolver error or timeout) returns `false`
/// so an infra-subdomain DNS hiccup can never take the apex cert down.
async fn infra_host_resolves(
    resolver: &dyn crate::dns_verifier::RecordResolver,
    label: &str,
    apex: &str,
) -> bool {
    use crate::dns_verifier::LookupOutcome;
    let host = format!("{label}.{}", apex.trim());
    for rtype in ["A", "AAAA"] {
        if let LookupOutcome::Records(recs) = resolver.lookup(&host, rtype).await
            && !recs.is_empty()
        {
            return true;
        }
    }
    false
}

/// Whether the apex managed cert should currently include the `relay.<apex>` SAN:
/// a relay sidecar is connected **AND** `relay.<apex>` resolves
/// ([`infra_host_resolves`]). Shared by the issuance loop
/// ([`cert_lifecycle_loop`]) and the at-risk cert nudge
/// ([`crate::cert_nudge`]) so they agree on the SAN set — a flag-only nudge would
/// loop forever, flagging `relay.<apex>` at-risk (served by the floor) while the
/// resolve-gated issuer declines to add it.
pub(crate) async fn relay_san_included(app_state: &crate::routes::AppState, apex: &str) -> bool {
    crate::discovery_core::relay_sidecar_connected(app_state)
        && infra_host_resolves(app_state.dns_verifier.resolver().as_ref(), "relay", apex).await
}

/// Whether the apex managed cert should currently include the `pds.<apex>` SAN:
/// this box actually runs the ATProto PDS bridge
/// ([`crate::bridge_atproto_handlers::atproto_pds_bridge_approved`]) **AND**
/// `pds.<apex>` resolves ([`infra_host_resolves`]). Exactly the shape of
/// [`relay_san_included`], for exactly the same reason — and shared by the same
/// two callers ([`cert_lifecycle_loop`] + [`crate::cert_nudge`]) so a flag-only
/// nudge can't loop forever against a resolve-gated issuer.
///
/// **Enable signal.** The clean `atproto_enabled` deployment state arrives with
/// mirror S4; until then the today-queryable equivalent is "an approved
/// `atproto.pds` bridge service user exists" — which is precisely the condition
/// under which anything is listening on `pds.<apex>` at all (the bridge is never
/// auto-approved; it always takes the manual admin approval card), so it is the
/// *correct* signal here rather than a stand-in. `atproto-pds-full.md`
/// § Implementation status today → "Not yet landed (F1 remainder) (a)".
pub(crate) async fn pds_san_included(app_state: &crate::routes::AppState, apex: &str) -> bool {
    crate::bridge_atproto_handlers::atproto_pds_bridge_approved(app_state).await
        && infra_host_resolves(app_state.dns_verifier.resolver().as_ref(), "pds", apex).await
}

/// The subset of `mail_domain_names` whose SANs may safely join the apex ACME
/// order **right now** — the primary/apex always, plus each **secondary** whose
/// apex actually resolves to this nest.
///
/// An ACME order is all-or-nothing: any identifier that fails HTTP-01 fails the
/// WHOLE order, so a secondary domain that is not yet published (or points
/// somewhere other than the nest) would take the **primary's** cert down with it
/// — the multi-domain regression this gate closes (symmetric to the
/// `relay.<apex>` resolve-gate, [`relay_san_included`]).
/// A deferred secondary is simply picked up on a later cycle once its DNS is
/// correct; the self-signed floor covers it meanwhile.
///
/// The **primary is never gated** — it is the identity apex, the one name we know
/// reaches the nest (and the standalone case the user already relies on). A
/// secondary is included only when its apex `A`/`AAAA` **serves the nest's own
/// persisted host address** (`nest_host_address`) — the strong check that also
/// rejects an `A` record pointed at some *other* host. When the host address is
/// not persisted yet, fall back to the weaker "has any `A`/`AAAA` record" test
/// (mirrors [`infra_host_resolves`]) — still enough to defer the common
/// no-record-at-all case without risking a false-negative on a box that never
/// ran the `set_host_address` onboarding hand-off.
///
/// Shared by [`cert_lifecycle_loop`] (the issuer) and [`crate::cert_nudge`] (the
/// at-risk nudge) so they compute the identical SAN set — a nudge that flagged a
/// deferred secondary at-risk while the issuer declined to order it would loop
/// forever (the same coupling the relay gate has).
pub(crate) async fn reachable_mail_domains(
    app_state: &crate::routes::AppState,
    apex: &str,
    mail_domain_names: &[String],
) -> Vec<String> {
    let apex_lc = apex.trim().to_ascii_lowercase();
    // One read of the persisted nest address for the whole domain set.
    let host = match app_state.db.get_host_address().await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                "cert-lifecycle: get_host_address failed: {e:#}; using weak resolve gate"
            );
            None
        }
    };
    let mut out = Vec::new();
    for name in mail_domain_names {
        let n = name.trim().to_ascii_lowercase();
        if n.is_empty() {
            continue;
        }
        // The primary/apex is always in the order (never gated).
        if n == apex_lc {
            out.push(name.clone());
            continue;
        }
        if secondary_apex_resolves_to_nest(&app_state.dns_verifier, &n, host.as_ref()).await {
            out.push(name.clone());
        } else {
            tracing::info!(
                domain = %n,
                "cert-lifecycle: secondary domain apex does not resolve to the nest yet — \
                 deferring its ACME SAN (the primary cert is unaffected; it is picked up \
                 automatically once its A/AAAA record points here)"
            );
        }
    }
    out
}

/// Whether a **secondary** domain's apex `A`/`AAAA` currently resolves to this
/// nest, so it is safe to add to the all-or-nothing apex ACME order (see
/// [`reachable_mail_domains`]). Strong check against the nest's persisted host
/// address when known; a weak "has any record" fallback otherwise. Fail-safe:
/// any resolver error / transient / empty result returns `false` (omit → defer),
/// so a DNS hiccup on a secondary can never risk the primary's renewal.
async fn secondary_apex_resolves_to_nest(
    verifier: &crate::dns_verifier::DnsVerifier,
    domain: &str,
    host: Option<&crate::db::nest_host_address::HostAddress>,
) -> bool {
    match host {
        // Strong: the apex must SERVE the nest's own (apex/WS-RPC) address
        // (rejects an A record pointed at a different host — the user's scenario).
        Some(h) => name_resolves_to(verifier, domain, &h.nest_ipv4, h.nest_ipv6.as_deref()).await,
        // Weak fallback (host address not persisted): any A/AAAA record at all.
        None => relay_or_any_host_resolves(verifier.resolver().as_ref(), domain).await,
    }
}

/// Strong DNS gate shared by the apex and mail-host resolve checks: `name`'s
/// `A` matches `expected_v4`, or its `AAAA` matches `expected_v6` (when set).
/// Verifying against the *expected* address (not merely "has any record") is
/// what rejects a name pointed at a **different** host. Fail-safe `false` on any
/// non-`Ok` verify (transient / empty / mismatch). Used by
/// [`secondary_apex_resolves_to_nest`] (secondary apex → nest addr) and
/// [`mail_host_resolves_to_mail_ip`] (rename MX host → mail addr).
async fn name_resolves_to(
    verifier: &crate::dns_verifier::DnsVerifier,
    name: &str,
    expected_v4: &str,
    expected_v6: Option<&str>,
) -> bool {
    use fauna_mail::dns::verify::RecordVerifyStatus;
    if verifier.verify(name, "A", expected_v4).await.status == RecordVerifyStatus::Ok {
        return true;
    }
    if let Some(v6) = expected_v6
        && verifier.verify(name, "AAAA", v6).await.status == RecordVerifyStatus::Ok
    {
        return true;
    }
    false
}

/// Whether the MX host `mail.<new-primary>` of an in-flight primary-domain
/// rename currently resolves to this deployment's **mail-host** address, so it
/// is safe to add `mail.<new-primary>` to the all-or-nothing managed-cert ACME
/// order (`mail-primary-domain-rename.md` § Behavior — cert chain re-issue
/// ordering; SLICE 2). Structurally identical to
/// [`secondary_apex_resolves_to_nest`] but keyed on the **mail** address
/// (`nest_host_address.mail_ipv4` — the MX target `mail.<new>`'s A record points
/// at, which may differ from the apex/nest address; see
/// [`fauna_mail::dns::host`] § two address roles) rather than the nest address.
/// Same fail-safe: any resolver error / transient / empty / wrong-IP result
/// returns `false` (defer), so an unpublished/unpropagated `mail.<new>` can never
/// fail the whole order and take the primary's cert down. **Strong check only —
/// there is deliberately no weak "any record" fallback here**; the caller
/// ([`rename_mail_host_reachable`]) drops the SAN outright when the host address
/// is unknown (`mail-primary-domain-rename.md` § Renaming away from a dead
/// domain owns that ruling).
async fn mail_host_resolves_to_mail_ip(
    verifier: &crate::dns_verifier::DnsVerifier,
    mail_host: &str,
    host: &crate::db::nest_host_address::HostAddress,
) -> bool {
    name_resolves_to(
        verifier,
        mail_host,
        &host.mail_ipv4,
        host.mail_ipv6.as_deref(),
    )
    .await
}

/// AppState-level convenience mirroring [`reachable_mail_domains`]: read the
/// persisted host address once, then the strong mail-host gate
/// ([`mail_host_resolves_to_mail_ip`]). Keeps the lifecycle loop's rename SAN
/// widening declarative. Gates both rename mail hosts — the pre-flip
/// `mail.<new>` widening and the grace `mail.<old>` keep-alive.
///
/// **Strong-check-or-DROP, like [`apex_sans_for_cycle`] and unlike the secondary
/// gate** (`mail-primary-domain-rename.md` § Renaming away from a dead domain —
/// ruling extended from the old apex to these two gates, 2026-08-13). An unknown
/// host address (or a `get_host_address` error) drops the SAN rather than falling
/// back to a weak "has any `A`/`AAAA` record" test. The deciding fact is
/// structural: `dns_handlers::append_rename_mail_host`, the **sole** publisher of
/// both `mail.<new>` and `mail.<old>`, reads this same `nest_host_address` and is
/// a no-op without it — so where the address was **never persisted** (the only
/// arm a pre-flip rename can reach, since the assembler cannot publish
/// `mail.<new>` without `mail_ipv4` either), this deployment has published
/// nothing at that name, and any record found there is by construction someone
/// else's (a zone wildcard, a stale row, a squatter). A weak test could not
/// produce a true positive there, only a false one, which admits an unvalidatable
/// name into the all-or-nothing order and fails **every** issuance
/// deployment-wide — the primary's included — while the only escapes cost the
/// thing in flight (abort the rename / force-complete the grace early).
///
/// The one arm where a weak test could have been right is a *transient*
/// `get_host_address` **error** during grace, with a correct `mail.<old>` still
/// published from before the flip. Dropping there is near-free — narrowing the
/// desired set forces no re-issue (`need_issue` keys on self-signedness, coverage
/// and freshness, and the installed cert still covers the superset), so the name
/// returns on the next readable tick — whereas keeping would leave this gate and
/// [`apex_sans_for_cycle`] disagreeing on the same tick over the same error.
/// Dropping instead is bounded and self-heals: pre-flip the rename waits visibly
/// in `cert_issuance` while every validatable SAN keeps renewing; during grace,
/// peers on a cached `MX → mail.<old>` fail TLS there and defer (mail retries,
/// nothing bounces) until their cache turns over — and against the dead or seized
/// zone this guards, `mail.<old>` no longer points here at all, so no peer reaches
/// it and the drop costs nothing.
pub(crate) async fn rename_mail_host_reachable(
    app_state: &crate::routes::AppState,
    mail_host: &str,
) -> bool {
    let host = match app_state.db.get_host_address().await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("cert-lifecycle: get_host_address failed: {e:#}");
            None
        }
    };
    let Some(host) = host else {
        tracing::warn!(
            mail_host = %mail_host,
            "cert-lifecycle: no persisted host address — dropping this rename mail host's \
             ACME SAN (strong-check-or-DROP; without that address we never published the \
             record, so a weak \"any record\" test could only ever match someone else's \
             and fail the whole order)"
        );
        return false;
    };
    mail_host_resolves_to_mail_ip(&app_state.dns_verifier, mail_host, &host).await
}

/// This cycle's [`ApexSans`] verdict: [`ApexSans::INCLUDED`] for every ordinary
/// tick, and the **strong-check-or-DROP** verdict while a primary-domain rename
/// is pre-flip — the remedy for the dead-apex ACME deadlock
/// (`mail-primary-domain-rename.md` § Renaming away from a dead domain).
///
/// Pre-flip the apex *is* the old primary (the flip is what moves the identity
/// projection), so a rename row in `requested`/`cert_issuance`/`cert_ready` is
/// the license: the old primary's `<apex>` and `mail.<apex>` become resolve-gated
/// exactly like a secondary apex. Post-flip needs nothing — the old primary is by
/// then an ordinary secondary carrying the secondary gate
/// ([`reachable_mail_domains`]), and the apex is the *new* domain, which must
/// never be gated.
///
/// **The fail direction inverts versus secondaries: DROP on uncertainty, never
/// keep.** A secondary's gate falls back to the weak "has any `A`/`AAAA` record"
/// test when `nest_host_address` isn't persisted, because a false negative there
/// merely defers a secondary. Here a false *positive* is what hurts: a seized old
/// apex parked at the squatter's A record passes the weak test, stays in the
/// all-or-nothing order, and re-arms the very deadlock this gate exists to kill —
/// with no escape, because the rename is the escape. So an unknown host address
/// (or a `get_host_address` error) drops both SANs rather than guessing. That
/// case is already anomalous: a rename is admin-initiated from an app whose
/// post-auth hook persists the host address (§ Implementation status today —
/// host-address acquisition), so "renaming with no persisted host address" means
/// something is wrong, and the fail direction that preserves the escape hatch is
/// the right one.
pub(crate) async fn apex_sans_for_cycle(
    app_state: &crate::routes::AppState,
    apex: &str,
) -> ApexSans {
    let Some(rename) = app_state.db.get_active_rename().await.ok().flatten() else {
        return ApexSans::INCLUDED;
    };
    if !rename.parsed_state().is_some_and(|s| s.is_pre_flip()) {
        return ApexSans::INCLUDED;
    }
    // Gate only the domain the rename actually leaves. The apex comes from the
    // in-memory identity projection and the rename row from the DB; if they
    // disagree (a mid-reconcile tick), the safe side is to gate nothing rather
    // than drop a name no rename is licensed to touch.
    let Some(old) = app_state
        .db
        .lookup_mail_domain_by_id(&rename.old_primary_domain_id)
        .await
        .ok()
        .flatten()
    else {
        return ApexSans::INCLUDED;
    };
    if !old.domain_name.eq_ignore_ascii_case(apex.trim()) {
        return ApexSans::INCLUDED;
    }
    let host = match app_state.db.get_host_address().await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("cert-lifecycle: get_host_address failed: {e:#}");
            None
        }
    };
    let Some(host) = host else {
        tracing::warn!(
            domain = %apex,
            "cert-lifecycle: renaming away from this primary with no persisted host address — \
             dropping its apex SANs from the order (strong-check-or-DROP; a weak \"any record\" \
             test would keep a seized/parked old apex and deadlock the rename)"
        );
        return ApexSans::DROPPED;
    };
    let apex_name = apex.trim();
    let verdict = ApexSans {
        apex: name_resolves_to(
            &app_state.dns_verifier,
            apex_name,
            &host.nest_ipv4,
            host.nest_ipv6.as_deref(),
        )
        .await,
        mail_host: name_resolves_to(
            &app_state.dns_verifier,
            &format!("mail.{apex_name}"),
            &host.mail_ipv4,
            host.mail_ipv6.as_deref(),
        )
        .await,
    };
    if verdict != ApexSans::INCLUDED {
        tracing::info!(
            domain = %apex_name,
            apex_san = verdict.apex,
            mail_host_san = verdict.mail_host,
            "cert-lifecycle: the primary being renamed away from no longer resolves to this nest \
             — dropping its unvalidatable SANs so the rename's own order can issue \
             (mail-primary-domain-rename.md § Renaming away from a dead domain)"
        );
    }
    verdict
}

/// The `(rename_id, "mail.<new-primary>", state)` of the active **pre-flip**
/// primary-domain rename, if any — the input to SLICE 2's cert-SAN widening.
/// `None` when there is no active rename, it is past `cert_ready` (`anchor_flip`
/// onward — the flip makes `<new>` the primary and its `mail.` host then flows
/// from the normal apex path, [`desired_san_domains`]), or its new-primary
/// domain row has vanished. Reads the new domain's name from `mail_domains`
/// (the rename row carries only the id).
async fn active_rename_new_mail_host(
    app_state: &crate::routes::AppState,
) -> Option<([u8; 16], String, fauna_mail::RenameState)> {
    let rename = app_state.db.get_active_rename().await.ok().flatten()?;
    let state = rename.parsed_state()?;
    if !state.is_pre_flip() {
        return None;
    }
    let dom = app_state
        .db
        .lookup_mail_domain_by_id(&rename.new_primary_domain_id)
        .await
        .ok()
        .flatten()?;
    Some((rename.rename_id, format!("mail.{}", dom.domain_name), state))
}

/// The `(rename_id, "mail.<old-primary>", state)` of the active **post-flip**
/// primary-domain rename, if any — the input to SLICE 3's grace-window `mail.<old>`
/// SAN keep-alive. `None` when there is no active rename, it is still pre-flip, it
/// is terminal, or its old-primary domain row has vanished. Post-flip the old
/// primary is a *secondary* whose apex-only shape drops its `mail.<old>` name from
/// [`desired_san_domains`]; keeping it in the desired set holds the managed cert as
/// a **superset** through grace, so a renewal can't narrow away `mail.<old>` while
/// peers with a cached `<domain> MX → mail.<old>` still reach it
/// (`mail-primary-domain-rename.md` § Behavior — cert chain re-issue ordering,
/// During grace). Reads the old domain's name from `mail_domains`.
async fn active_rename_old_mail_host(
    app_state: &crate::routes::AppState,
) -> Option<([u8; 16], String, fauna_mail::RenameState)> {
    let rename = app_state.db.get_active_rename().await.ok().flatten()?;
    let state = rename.parsed_state()?;
    if !state.is_post_flip_active() {
        return None;
    }
    let dom = app_state
        .db
        .lookup_mail_domain_by_id(&rename.old_primary_domain_id)
        .await
        .ok()
        .flatten()?;
    Some((rename.rename_id, format!("mail.{}", dom.domain_name), state))
}

/// Hex SPKI-SHA256 fingerprint of the leaf certificate in `pem_data` — the
/// fingerprint vocabulary the rest of the system already speaks
/// ([`crate::acme::spki_sha256_of_cert_der`], the DANE-TLSA / served-leaf pin),
/// reused for a rename's `new_cert_fingerprint` rather than a whole-cert digest
/// (`mail-primary-domain-rename.md` § Implementation status today). `None` on a
/// PEM/DER parse failure.
fn leaf_spki_fingerprint_hex(pem_data: &[u8]) -> Option<String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(pem_data).ok()?;
    let spki = crate::acme::spki_sha256_of_cert_der(&pem.contents)?;
    Some(hex::encode(spki))
}

/// "Does `host` have any `A`/`AAAA` record?" — the weak resolvability test shared
/// by the infra-subdomain SAN gates ([`infra_host_resolves`], which prefixes the
/// label) and
/// the host-address-unknown fallback in [`secondary_apex_resolves_to_nest`].
async fn relay_or_any_host_resolves(
    resolver: &dyn crate::dns_verifier::RecordResolver,
    host: &str,
) -> bool {
    use crate::dns_verifier::LookupOutcome;
    for rtype in ["A", "AAAA"] {
        if let LookupOutcome::Records(recs) = resolver.lookup(host, rtype).await
            && !recs.is_empty()
        {
            return true;
        }
    }
    false
}

/// Background task that manages the TLS certificate lifecycle.
///
/// Each tick it derives the desired SAN set from nest state
/// ([`desired_san_domains`] over `db.list_active_mail_domains()`) and
/// re-issues the cert when EITHER the on-disk chain doesn't cover that set
/// (a mail domain was added) OR it nears expiry (< 30 days). It re-issues
/// **only** on those triggers, so a healthy unchanged cert never burns
/// Let's Encrypt's issuance budget. The
/// [`cert_watcher_task`](crate::acme::cert_watcher_task) then hot-reloads the
/// new chain and seals+fans it out to approved bridges via
/// `store_acme_material`.
///
/// Cadence: a short steady poll (5 min) so a freshly-added domain gets a
/// covering cert promptly; on issuance *failure* (DNS not yet pointing at us,
/// port 80 blocked) it backs off (5 → 15 → 30 → 60 min) to stay under LE's
/// failed-validation rate limit. The streak is persisted to
/// `acme_dir/acme-retry-state.json` ([`RetryState`]) and *resumed* on the next
/// boot, so restarting the process during a misconfiguration cannot reset the
/// spacing and exhaust the budget (Gap B).
pub async fn cert_lifecycle_task(
    config: Http01Config,
    challenge_state: Arc<ChallengeState>,
    app_state: Arc<crate::routes::AppState>,
    cert_resolver: Option<Arc<crate::acme::MultiDomainCertResolver>>,
) {
    // Production entry point: issue via the real ACME HTTP-01 flow and run
    // forever. The loop body lives in `cert_lifecycle_loop` so tests can drive
    // it with a fake issuer (real ACME can't be faked) — see the integration
    // tests below.
    let issue_config = config.clone();
    let issue_challenge_state = challenge_state.clone();
    let issue = move |desired: Vec<String>| {
        let config = issue_config.clone();
        let challenge_state = issue_challenge_state.clone();
        async move {
            // The apex cert keeps its account and cert files in the same dir.
            let account_dir = config.acme_dir.clone();
            obtain_certificate(&config, &desired, &challenge_state, &account_dir).await
        }
    };
    // The § B-IP bridge order, sharing this task's account (one Let's Encrypt
    // account, one rate budget) and its port-80 challenge router. On success the
    // freshly-written `ip-*.pem` pair is loaded straight into the resolver: the
    // cert watcher only watches the *domain* cert's filenames, so nothing else
    // would ever install it.
    let ip_config = config.clone();
    let issue_ip = move |addrs: Vec<std::net::IpAddr>| {
        let config = ip_config.clone();
        let challenge_state = challenge_state.clone();
        async move {
            let account_dir = config.acme_dir.clone();
            fauna_acme_http01::obtain_ip_certificate(
                &config,
                &addrs,
                &challenge_state,
                &account_dir,
            )
            .await
        }
    };
    cert_lifecycle_loop(config, app_state, issue, issue_ip, cert_resolver, None).await;
}

/// [`cert_lifecycle_loop`] with the § B-IP bridge arm inert — no resolver, so
/// `ip_bridge_pass` returns before it reads the interface table or orders
/// anything. The domain-arm tests below drive this, so each one exercises
/// exactly what it did before the bridge arm existed; the bridge's own
/// behaviour is pinned by the tests that call [`cert_lifecycle_loop`] directly
/// with a resolver and a fake IP issuer.
#[cfg(test)]
async fn cert_lifecycle_loop_domain_only<I, Fut>(
    config: Http01Config,
    app_state: Arc<crate::routes::AppState>,
    issue: I,
    max_iterations: Option<usize>,
) where
    I: Fn(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    cert_lifecycle_loop(
        config,
        app_state,
        issue,
        |_addrs: Vec<std::net::IpAddr>| async { Ok(()) },
        None,
        max_iterations,
    )
    .await;
}

/// One IP-bridge pass, run at the top of every [`cert_lifecycle_loop`] tick.
///
/// Returns `true` when an order was attempted and **failed**, so the caller can
/// fold it into the tick's shared failed-validation pacing — the bridge
/// deliberately has no budget of its own (`tls-certificates.md` § B-IP
/// *Lifetime*: "under the persisted failed-validation pacing of § Keeping the
/// cert alive … no second budget").
///
/// Three decisions, in order:
///
/// 1. **Should this box bridge at all?** `ip_bridge_addresses` over the live
///    interface table (the derived, knob-free enable), then
///    `ip_bridge_should_renew` — false once a trusted primary-domain cert is
///    live, at which point the installed bridge is dropped and IP dials return
///    to the floor. A NAT box, a private-axis box, and a box that has finished
///    bootstrapping all take this arm and order nothing.
/// 2. **Is the current bridge still fresh?** Due on `ip_cert_needs_renewal`
///    over the on-disk chain's observed validity (a third of it as the lead,
///    ~53 h against Let's Encrypt's 160 h `shortlived` profile) — a missing or
///    unparseable chain is due — **or** on `ip_bridge_addrs_changed` finding
///    the live interface set has moved since the last order decision
///    (`IP_BRIDGE_ATTEMPTED_FILENAME`). Deliberately compared against what was
///    last **attempted**, not what the installed chain **covers**: the v4-only
///    retry below can leave the chain covering less than the live set by
///    design, and checking coverage against the live set would treat that gap
///    as a fresh change every tick, forever.
/// 3. **Order.** All addresses in one certificate — the resolver can serve
///    exactly one cert to a no-SNI dial, so ordering v4 and v6 separately would
///    produce a cert that can never be served. The "one failing family must not
///    sink the other" rule of § B's all-or-nothing note is honoured instead by a
///    **v4-only retry**: if a dual-family order fails and the box has a v4
///    address, retry with just that, since an IPv6 `:80` the CA cannot reach is
///    the common asymmetry and is exactly what would otherwise take the whole
///    order down.
async fn ip_bridge_pass<J, JFut>(
    config: &Http01Config,
    app_state: &Arc<crate::routes::AppState>,
    cert_resolver: Option<&Arc<crate::acme::MultiDomainCertResolver>>,
    issue_ip: &J,
    primary_domain_cert_trusted: bool,
) -> bool
where
    J: Fn(Vec<std::net::IpAddr>) -> JFut,
    JFut: std::future::Future<Output = Result<()>>,
{
    ip_bridge_pass_for_attached(
        config,
        app_state,
        cert_resolver,
        issue_ip,
        primary_domain_cert_trusted,
        &crate::acme::attached_interface_addresses(),
    )
    .await
}

/// [`ip_bridge_pass`] with the interface table injected — split out so the
/// live-`node_mode` derivation below is pinnable in a test without the box's
/// own addresses (mirrors [`ip_bridge_pass_inner`]'s split for `addrs`).
async fn ip_bridge_pass_for_attached<J, JFut>(
    config: &Http01Config,
    app_state: &Arc<crate::routes::AppState>,
    cert_resolver: Option<&Arc<crate::acme::MultiDomainCertResolver>>,
    issue_ip: &J,
    primary_domain_cert_trusted: bool,
    attached: &[std::net::IpAddr],
) -> bool
where
    J: Fn(Vec<std::net::IpAddr>) -> JFut,
    JFut: std::future::Future<Output = Result<()>>,
{
    // The live, DB-resolved `node_mode` — never `config.nest.mode`, the
    // immutable `FAUNA_MODE` boot seed — per `routes.rs`'s own doc comment on
    // `AppState::node_mode` enumerating its readers. A box seeded `private`
    // and later flipped `public` (restart-applied, `nat_mode_core.rs`'s
    // `apply_node_mode_change`) would otherwise never have its bridge cert
    // ordered after that restart: `ip_bridge_addresses` returns an empty set
    // for `Private`, so reading the stale seed here strands the box on the
    // self-signed floor indefinitely (`docs/goal/architecture/nest/common.md`
    // § NAT mode).
    let live_mode = *app_state.node_mode.read().await;
    // `ip_bridge_addresses` (node-mode gate → global-unicast filter) is
    // separately unit-pinned in `crate::acme`.
    let addrs = crate::acme::ip_bridge_addresses(live_mode, attached);
    ip_bridge_pass_inner(
        config,
        cert_resolver,
        issue_ip,
        primary_domain_cert_trusted,
        &addrs,
    )
    .await
}

/// [`ip_bridge_pass`] with the derived address set injected — see its doc for the
/// three decisions. Split out so the policy is testable without the box's own
/// interface table.
async fn ip_bridge_pass_inner<J, JFut>(
    config: &Http01Config,
    cert_resolver: Option<&Arc<crate::acme::MultiDomainCertResolver>>,
    issue_ip: &J,
    primary_domain_cert_trusted: bool,
    addrs: &[std::net::IpAddr],
) -> bool
where
    J: Fn(Vec<std::net::IpAddr>) -> JFut,
    JFut: std::future::Future<Output = Result<()>>,
{
    let Some(resolver) = cert_resolver else {
        // No TLS listener (a plain-HTTP dev/e2e nest) — nothing to serve a
        // bridge cert on, so nothing to order.
        return false;
    };

    let addrs = addrs.to_vec();
    if !crate::acme::ip_bridge_should_renew(!addrs.is_empty(), primary_domain_cert_trusted) {
        // Either nothing orderable, or the deployment's own domain cert is live
        // and the bridge has served its purpose. Dropping an installed one is the
        // documented switch-back; doing it unconditionally here also cleans up a
        // bridge left behind by a previous boot.
        if resolver.current_ip_cert().is_some() {
            tracing::info!(
                "IP bridge cert no longer needed (primary-domain cert trusted, or no \
                 orderable address) — dropping it; IP dials return to the floor"
            );
            resolver.clear_ip_cert();
        }
        return false;
    }

    let ip_cert_path = config.acme_dir.join(crate::acme::IP_CERT_FILENAME);
    let ip_key_path = config.acme_dir.join(crate::acme::IP_KEY_FILENAME);
    // `ip_cert_needs_renewal` works in signed seconds (an expired cert's remaining
    // window is legitimately negative); this module's clock is unsigned.
    let now = now_unix() as i64;
    let on_disk = tokio::fs::read(&ip_cert_path).await.ok();
    // Compared against what the bridge last ATTEMPTED, not what the installed
    // chain COVERS: a v4-only narrowing (below) leaves the chain covering less
    // than the live interface set by design, and checking coverage against the
    // live set instead would treat that gap as a fresh interface change on
    // every subsequent tick, forever (`tls-certificates.md` § B-IP
    // *Lifetime*).
    let last_attempted = load_ip_bridge_attempted(&config.acme_dir).await;
    let needs_order = match on_disk.as_deref() {
        Some(pem) => match crate::acme::pem_validity_window(pem) {
            Some((not_before, not_after)) => {
                crate::acme::ip_cert_needs_renewal(not_before, not_after, now)
                    || crate::acme::ip_bridge_addrs_changed(&addrs, &last_attempted)
            }
            None => true,
        },
        None => true,
    };

    // Reconcile after a restart: a still-fresh chain on disk that nothing has
    // installed yet is loaded without re-ordering (the CA's duplicate-certificate
    // limit is 5/week for one SAN set — a restart loop must not spend it).
    if !needs_order {
        if resolver.current_ip_cert().is_none() {
            install_ip_cert(resolver, &ip_cert_path, &ip_key_path);
        }
        return false;
    }

    tracing::info!(?addrs, "ACME: ordering the IP bridge cert (§ B-IP)");
    // Persisted BEFORE the attempt (not after): what matters for settling next
    // tick is what was asked for, regardless of whether the order below
    // succeeds outright, narrows, or fails entirely.
    save_ip_bridge_attempted(&config.acme_dir, &addrs).await;
    let mut attempt = addrs.clone();
    loop {
        match issue_ip(attempt.clone()).await {
            Ok(()) => {
                install_ip_cert(resolver, &ip_cert_path, &ip_key_path);
                tracing::info!(ordered = ?attempt, "IP bridge cert issued");
                return false;
            }
            Err(e) => {
                // One narrowing retry, v4-only: an IPv6 address the CA cannot
                // reach on `:80` is the common asymmetry, and an all-or-nothing
                // order would otherwise lose the v4 cert with it.
                let v4: Vec<std::net::IpAddr> =
                    attempt.iter().copied().filter(|a| a.is_ipv4()).collect();
                if v4.len() < attempt.len() && !v4.is_empty() {
                    tracing::warn!(
                        attempted = ?attempt,
                        "IP bridge order failed ({e:#}); retrying with the IPv4 address(es) only"
                    );
                    attempt = v4;
                    continue;
                }
                tracing::error!(attempted = ?attempt, "IP bridge order failed: {e:#}");
                return true;
            }
        }
    }
}

/// Load the IP bridge's last-attempted interface set
/// (`crate::acme::IP_BRIDGE_ATTEMPTED_FILENAME`) — see
/// `crate::acme::ip_bridge_addrs_changed`. Defaults to empty when missing or
/// unreadable: a missing/corrupt file must not wedge issuance, and comparing
/// against an empty set forces exactly one self-healing re-order rather than
/// silently trusting a set that failed to load.
async fn load_ip_bridge_attempted(acme_dir: &std::path::Path) -> Vec<std::net::IpAddr> {
    let path = acme_dir.join(crate::acme::IP_BRIDGE_ATTEMPTED_FILENAME);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Persist the interface set the bridge is about to attempt, so the next
/// tick's `ip_bridge_addrs_changed` compares against what was actually asked
/// for rather than the (possibly narrower) set the resulting chain covers.
/// Best-effort: a write failure is logged, never fatal (mirrors
/// [`RetryState::save`]).
async fn save_ip_bridge_attempted(acme_dir: &std::path::Path, addrs: &[std::net::IpAddr]) {
    let path = acme_dir.join(crate::acme::IP_BRIDGE_ATTEMPTED_FILENAME);
    match serde_json::to_vec(addrs) {
        Ok(bytes) => {
            if let Err(e) = tokio::fs::write(&path, bytes).await {
                tracing::warn!(
                    "failed to persist the IP bridge's attempted interface set to {}: {e}",
                    path.display()
                );
            }
        }
        Err(e) => {
            tracing::warn!("failed to serialize the IP bridge's attempted interface set: {e}")
        }
    }
}

/// Load the freshly-written IP bridge material into the resolver, so no-SNI
/// dials start being served it without a restart. A load failure is logged and
/// left alone — the floor keeps serving, and the next tick retries.
fn install_ip_cert(
    resolver: &Arc<crate::acme::MultiDomainCertResolver>,
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) {
    match crate::acme::load_certified_key(cert_path, key_path) {
        Ok(key) => resolver.set_ip_cert(key),
        Err(e) => tracing::error!(
            "could not load the IP bridge cert from {}: {e:#}",
            cert_path.display()
        ),
    }
}

/// The cert-lifecycle loop body, parameterised over the issuance operation and
/// an optional iteration cap. `cert_lifecycle_task` is the production caller
/// (real `obtain_certificate`, `max_iterations = None` → runs forever); tests
/// pass a fake issuer and a small cap. `issue` receives the desired SAN set.
async fn cert_lifecycle_loop<I, Fut, J, JFut>(
    config: Http01Config,
    app_state: Arc<crate::routes::AppState>,
    issue: I,
    issue_ip: J,
    cert_resolver: Option<Arc<crate::acme::MultiDomainCertResolver>>,
    max_iterations: Option<usize>,
) where
    I: Fn(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
    J: Fn(Vec<std::net::IpAddr>) -> JFut,
    JFut: std::future::Future<Output = Result<()>>,
{
    use std::time::Duration;

    let cert_path = config.acme_dir.join(CERT_FILENAME);
    // Renew when less than 30 days remain (the shared cert-lifecycle lead).
    let renewal_threshold_secs: i64 = crate::acme::CERT_RENEWAL_LEAD_SECS;
    let steady_poll = Duration::from_secs(5 * 60);

    // Resume the persisted failed-attempt log rather than resetting it, so a
    // restart loop during a misconfiguration can't bypass the rate budget. The
    // scheduler paces the first attempt within the budget (immediately if no
    // recent failures — the common case after a config fix like opening port 80).
    let mut retry = RetryState::load(&config.acme_dir);
    let initial_wait = next_attempt_delay(&retry, now_unix());
    if !initial_wait.is_zero() {
        tracing::info!(
            failures_this_hour = retry.failures_in_window(now_unix()),
            budget = ACME_FAILED_VALIDATION_BUDGET_PER_HOUR,
            wait_secs = initial_wait.as_secs(),
            "ACME: pacing first issuance attempt within the failed-validation budget after restart"
        );
        tokio::time::sleep(initial_wait).await;
    }

    let mut iterations: usize = 0;
    loop {
        // Read the apex per-iteration from the live identity domain (DB-backed
        // `handle_domain()`), NOT the boot-time `config.domain`: on a
        // domainless-booted box the apex is learned at *claim*, which wakes this
        // loop (`acme_retry_notify`) so issuance fires as soon as the domain lands
        // — no restart. Skip issuance while the apex is empty (pre-claim) or a
        // local/IP/`.local` identity (never orderable); the self-signed floor
        // serves meanwhile.
        let apex = app_state.handle_domain();
        let apex_orderable = !apex.is_empty() && fauna_core::resolve::is_public_dns_name(&apex);
        // Primary-domain rename (SLICE 3): capture whether a rename was already
        // `cert_ready` at the START of this tick, so the anchor flip fires on the
        // tick *after* cert_ready is reached (not the same tick that stamps it) —
        // keeping `cert_ready` an observable resting state and advancing one
        // lifecycle step per tick. The flip itself runs at the bottom of the tick.
        let rename_cert_ready_at_tick_start = app_state
            .db
            .get_active_rename()
            .await
            .ok()
            .flatten()
            .filter(|r| r.parsed_state() == Some(fauna_mail::RenameState::CertReady))
            .map(|r| r.rename_id);
        // Desired SAN set from nest state. Apex is present only when orderable;
        // mail hosts only for domains the admin has actually configured (see
        // desired_san_domains).
        let mail_domain_names: Vec<String> = match app_state.db.list_active_mail_domains().await {
            Ok(rows) => rows.into_iter().map(|d| d.domain_name).collect(),
            Err(e) => {
                tracing::warn!("cert_lifecycle_task: list_active_mail_domains failed: {e:#}");
                Vec::new()
            }
        };
        // Gate `relay.<apex>` on a relay sidecar being connected AND on
        // the name actually resolving (`relay_san_included` → `infra_host_resolves`).
        // An ACME order is all-or-nothing (any unvalidatable identifier fails the
        // WHOLE order, apex included — `desired_san_domains` doc), so the flag
        // alone is unsafe: an admin can enable the relay before publishing the
        // `relay.<apex>` A record, which would otherwise add an unresolvable name
        // to the apex order and take apex HTTPS down.
        // Pre-resolving decouples them — a missing/transient relay A record just
        // defers the relay SAN to a later cycle; the self-signed floor carries
        // `relay.<apex>` unconditionally, so LAN/floor serving is unaffected.
        // Re-read each cycle so a runtime flip (or a freshly-published A record)
        // reissues to add the relay SAN; the cert-coverage check below drives it.
        // `pds.<apex>` rides the identical gate (approved `atproto.pds` bridge AND
        // the name resolving) for the identical reason — see `InfraSans`.
        let infra = if apex_orderable {
            InfraSans {
                relay: relay_san_included(&app_state, &apex).await,
                pds: pds_san_included(&app_state, &apex).await,
            }
        } else {
            InfraSans::default()
        };
        // Resolve-gate secondaries: a secondary domain whose apex does not resolve
        // to this nest is deferred rather than dropped into the all-or-nothing
        // order, where its HTTP-01 failure would fail the WHOLE order and take the
        // primary's cert down with it (the multi-domain regression — see
        // `reachable_mail_domains`). The primary/apex is always kept.
        // …and resolve-gate the APEX's own two SANs while a rename is pre-flip:
        // the apex is then the domain being renamed away from, and if that zone is
        // dead its names fail HTTP-01 and take the whole order down — including the
        // `cert_issuance` order that must add `mail.<new>`, which is the deadlock
        // (`apex_sans_for_cycle`; `mail-primary-domain-rename.md` § Renaming away
        // from a dead domain). A live old zone keeps both SANs.
        let mut desired = if apex_orderable {
            let reachable = reachable_mail_domains(&app_state, &apex, &mail_domain_names).await;
            let apex_sans = apex_sans_for_cycle(&app_state, &apex).await;
            desired_san_domains(&apex, &reachable, &infra, apex_sans)
        } else {
            Vec::new()
        };

        // Primary-domain rename (SLICE 2): while a rename is pre-flip, widen the
        // single managed cert with the ONE new SAN `mail.<new-primary>`, resolve-
        // gated exactly like a secondary apex so an unpublished/unpropagated
        // `mail.<new>` can never fail the all-or-nothing order and take the
        // primary's cert down (`mail-primary-domain-rename.md` § Behavior — cert
        // chain re-issue ordering). The loop is the idempotent driver: it advances
        // `requested → cert_issuance`, adds the gated SAN, and — once the (re-)
        // issued cert covers it — stamps `cert_ready` (below, after issuance).
        // Crash-safe: every step re-derives from the persisted row each tick, and
        // there is no persisted-failure state (§ Crash recovery). The listeners
        // keep only the old SAN graph bound — `mail.<new>` is a cert SAN but a
        // dormant served hostname until anchor flip (slice 3).
        let mut rename_cert_ready_pending: Option<([u8; 16], String)> = None;
        if apex_orderable
            && let Some((rename_id, mail_host, state)) =
                active_rename_new_mail_host(&app_state).await
        {
            // Advance `requested → cert_issuance` (idempotent). The `start` handler
            // advances on the RPC; this re-assertion covers a crash between insert
            // and that advance (§ Crash recovery).
            let effective_state = if state == fauna_mail::RenameState::Requested {
                match app_state
                    .db
                    .advance_rename_to_cert_issuance(&rename_id)
                    .await
                {
                    Ok(r) => r
                        .parsed_state()
                        .unwrap_or(fauna_mail::RenameState::CertIssuance),
                    Err(e) => {
                        tracing::warn!(
                            "cert-lifecycle: advance rename to cert_issuance failed: {e:#}"
                        );
                        state
                    }
                }
            } else {
                state
            };
            // Resolve-gate the new MX host against the mail-host address, then add
            // the single SAN (deduped, lower-cased to match `desired_san_domains`).
            if rename_mail_host_reachable(&app_state, &mail_host).await {
                let m = mail_host.to_ascii_lowercase();
                if !desired.iter().any(|x| x == &m) {
                    desired.push(m.clone());
                }
                // Only stamp `cert_ready` from `cert_issuance` (strict). A re-
                // observe on a later tick (row already `cert_ready`) leaves it be.
                if effective_state == fauna_mail::RenameState::CertIssuance {
                    rename_cert_ready_pending = Some((rename_id, m));
                }
            } else {
                tracing::info!(
                    domain = %mail_host,
                    "cert-lifecycle: rename new MX host does not resolve to the mail address yet \
                     — deferring its ACME SAN (the primary cert is unaffected; it is picked up \
                     once `mail.<new>` points here)"
                );
            }
        }

        // Primary-domain rename (SLICE 3): while a rename is post-flip non-terminal
        // (grace), keep `mail.<old-primary>` in the desired SAN set so a cert
        // renewal during grace can't narrow the superset and drop the old MX host
        // that peers with a cached `<domain> MX → mail.<old>` still reach
        // (`mail-primary-domain-rename.md` § Behavior — cert chain, During grace).
        // Resolve-gated (mirroring the pre-flip `mail.<new>` gate) so it can never
        // fail the all-or-nothing order — `mail.<old>` is the established MX host
        // whose A record the assembler keeps alive, so this normally passes.
        // Strong-check-or-DROP on uncertainty (`rename_mail_host_reachable`): a
        // dead or seized old zone parks `mail.<old>` at someone else's address,
        // and keeping it here would fail every renewal of the NEW primary's cert
        // for the whole grace window — the pre-flip deadlock, re-armed after the
        // flip. Dropping it only costs peers still on a cached `MX → mail.<old>`,
        // who defer and retry until their cache turns over.
        if apex_orderable
            && let Some((_rid, old_mail_host, _state)) =
                active_rename_old_mail_host(&app_state).await
            && rename_mail_host_reachable(&app_state, &old_mail_host).await
        {
            let m = old_mail_host.to_ascii_lowercase();
            if !desired.iter().any(|x| x == &m) {
                desired.push(m);
            }
        }

        let need_issue = if desired.is_empty() {
            // No orderable apex yet (domainless-booted box pre-claim, or a
            // local/IP identity) — nothing to do; the floor serves.
            false
        } else {
            match tokio::fs::read(&cert_path).await {
                Ok(pem) => {
                    let covers = cert_covers_sans(&pem, &desired);
                    let fresh = cert_seconds_remaining(&pem)
                        .map(|r| r >= renewal_threshold_secs)
                        .unwrap_or(false);
                    // Self-heal: a self-signed cert on disk is a stopgap — an
                    // admin-synthesized cert (`provision_self_signed_cert`) or a
                    // bootstrap fallback. This task only runs when ACME is
                    // enabled, and then a real CA-issued cert is *always*
                    // preferred, so upgrade automatically. `issue()` obtains the
                    // real cert and overwrites the PEM (no delete-first), so the
                    // self-signed keeps serving until the real one lands — no
                    // outage, no admin action, no client RPC. A deployment that
                    // genuinely wants a self-signed cert disables ACME, so this
                    // task isn't running and the cert is left alone.
                    let self_signed = crate::acme::pem_is_self_signed(&pem);
                    if self_signed {
                        tracing::info!(
                            ?desired,
                            "on-disk TLS cert is self-signed and ACME is enabled — \
                             self-healing: obtaining a real certificate to replace it"
                        );
                    } else if !covers {
                        tracing::info!(
                            ?desired,
                            "TLS cert does not cover desired SAN set, re-issuing"
                        );
                    } else if !fresh {
                        tracing::info!("TLS cert nearing expiry, renewing");
                    }
                    self_signed || !covers || !fresh
                }
                Err(_) => {
                    tracing::info!("No TLS certificate on disk, obtaining via ACME HTTP-01");
                    true
                }
            }
        };

        // ── The § B-IP bridge arm ────────────────────────────────────────────
        // Runs inside this tick rather than in a loop of its own, deliberately:
        // the bridge shares this task's `retry` budget, its `acme_retry_notify`
        // wake and its steady poll, and `acme-retry-state.json` has exactly one
        // owner. A sibling loop would have two writers `load`/`save`-ing the
        // same failure log, each clobbering the other's record — which is the
        // opposite of § B-IP *Lifetime*'s "no second budget".
        //
        // It runs BEFORE the domain arm so a domainless box (where the domain
        // arm does nothing at all — `desired` is empty) still bridges promptly,
        // and it is told whether a trusted primary-domain cert is live, which is
        // what ends the bridge's life.
        let primary_domain_cert_trusted = apex_orderable
            && match tokio::fs::read(&cert_path).await {
                Ok(pem) => {
                    !crate::acme::pem_is_self_signed(&pem)
                        && cert_covers_sans(&pem, std::slice::from_ref(&apex))
                        && cert_seconds_remaining(&pem).is_some_and(|r| r > 0)
                }
                Err(_) => false,
            };
        let ip_issue_failed = ip_bridge_pass(
            &config,
            &app_state,
            cert_resolver.as_ref(),
            &issue_ip,
            primary_domain_cert_trusted,
        )
        .await;

        let mut issue_failed = ip_issue_failed;
        if need_issue {
            // Log every attempt (the "recent attempts" log the budget is built on).
            tracing::info!(
                ?desired,
                attempt_in_hour = retry.failures_in_window(now_unix()) + 1,
                budget = ACME_FAILED_VALIDATION_BUDGET_PER_HOUR,
                "ACME: attempting issuance"
            );
            match issue(desired.clone()).await {
                Ok(()) => {
                    retry.reset();
                    retry.save(&config.acme_dir);
                    tracing::info!(?desired, "ACME certificate issued/renewed");
                    // Prompt-refresh-on-provision: a running MDA/MTA fetches its
                    // TLS cert on a 12 h timer, so a cert that lands *under* it (a
                    // domain added post-claim → this issuance covers a new
                    // `mail.<domain>`; a self-signed→trusted self-heal) would not
                    // be served until that timer or a restart. Nudge every approved
                    // bridge to re-run `fetch_tls_cert_blob` now (seal-on-read hands
                    // it the cert just written to disk). Best-effort — a
                    // disconnected bridge re-reads on reconnect / its own timer
                    // (`mail-bridge-lifecycle.md` § TLS provisioning; the `"tls"`
                    // reason is the realized prompt-refresh-on-provision half).
                    crate::bridge_routing_handlers::notify_bridges_config_changed(
                        &app_state,
                        fauna_protocol::bridge_routing::config_change_reason::TLS,
                    )
                    .await;
                }
                Err(e) => {
                    retry.record_failure(now_unix());
                    retry.save(&config.acme_dir);
                    issue_failed = true;
                    tracing::error!(
                        ?desired,
                        failures_this_hour = retry.failures_in_window(now_unix()),
                        budget = ACME_FAILED_VALIDATION_BUDGET_PER_HOUR,
                        "ACME issuance failed: {e:#}"
                    );
                }
            }
        }

        // Primary-domain rename (SLICE 2): once the (re-)issued managed cert on
        // disk actually covers `mail.<new-primary>`, stamp the rename `cert_ready`
        // with the acquired leaf's SPKI fingerprint (`mail-primary-domain-
        // rename.md` § Lifecycle). Re-derived each tick from the on-disk cert, so
        // a crash between issuance and this stamp is recovered next tick (the cert
        // already covers `mail.<new>`, so no re-issue is needed to complete it);
        // `mark_rename_cert_ready` is strict `cert_issuance → cert_ready`, so a
        // later re-observe (row already `cert_ready`) is a harmless refusal that
        // never overwrites the original acquisition stamp.
        if let Some((rename_id, mail_host)) = rename_cert_ready_pending
            && let Ok(pem) = tokio::fs::read(&cert_path).await
            && cert_covers_sans(&pem, std::slice::from_ref(&mail_host))
        {
            match leaf_spki_fingerprint_hex(&pem) {
                Some(fp) => match app_state
                    .db
                    .mark_rename_cert_ready(&rename_id, &fp, crate::db::now_epoch_millis())
                    .await
                {
                    Ok(_) => tracing::info!(
                        domain = %mail_host,
                        "primary-domain rename: managed cert now covers the new MX host — cert_ready"
                    ),
                    Err(e) => {
                        tracing::warn!("primary-domain rename: mark cert_ready failed: {e:#}")
                    }
                },
                None => tracing::warn!(
                    "primary-domain rename: could not fingerprint the cert leaf; retry next tick"
                ),
            }
        }

        // Primary-domain rename (SLICE 3): the anchor flip. A rename that was
        // `cert_ready` at the START of this tick (captured above) flips now — the
        // tick *after* cert_ready, so `cert_ready` stays an observable resting
        // state. `advance_rename_to_grace` atomically flips `is_primary` + enters
        // `grace` (`mail-primary-domain-rename.md` § Lifecycle / § Data — Mutations
        // to mail_domains). The runtime identity projection + the
        // `config_changed`/`local_domains` push follow from the **unconditional
        // reconcile below**, which reads the freshly-flipped DB primary — so the
        // flip itself only owns the DB transition. Crash-safe: the flip is ONE DB
        // transaction, so a restart re-derives the identity from the flipped
        // primary row at boot.
        if let Some(rid) = rename_cert_ready_at_tick_start {
            match app_state.db.advance_rename_to_grace(&rid).await {
                Ok(_) => tracing::info!(
                    "primary-domain rename: anchor flip committed — is_primary flipped, entering grace"
                ),
                Err(e) => tracing::warn!("primary-domain rename: anchor flip failed: {e:#}"),
            }
        }

        // Identity-projection self-heal.
        // The deployment's identity domain is an in-memory projection of the
        // primary `mail_domains` row (`identity_domain_core`), so re-derive it
        // from the DB primary **unconditionally each tick** — NOT only while a
        // rename is post-flip-active. This one reconcile covers every source of
        // divergence with a single self-healing rule: the anchor flip (new
        // primary just promoted), the abort re-flip (old primary restored), AND
        // any raced divergence the old rename-gated reconcile could not heal —
        // a stale tick re-applying a since-aborted `new` (which then had no
        // active rename left to trigger a heal), or the abort handler's own
        // silent skip-on-lookup-failure. Whatever the source, the next tick
        // converges the projection onto the DB truth
        // (`mail-primary-domain-rename.md` § Crash recovery — self-healing).
        //
        // Gated exactly like the other identity setter (`add_local_domain_handler`)
        // and `apply_primary_identity`'s own contract: act only when a primary
        // mail domain exists and is non-local. An IP / domainless-claimed box
        // registers no `mail_domains` row (`lookup_primary_mail_domain` → None),
        // so its identity comes from the registration handle and is left
        // untouched. The `!=` guard makes the steady state a pure no-op — boot's
        // `resolve_identity_domain` seeds `identity_domain` from the same
        // `domain_name`, so no reconcile fires and no `config_changed` is pushed
        // unless the projection has actually drifted.
        if let Ok(Some(primary)) = app_state.db.lookup_primary_mail_domain().await
            && fauna_core::resolve::is_public_dns_name(&primary.domain_name)
            && app_state.handle_domain() != primary.domain_name
        {
            crate::identity_domain_core::apply_primary_identity(&app_state, &primary.domain_name);
            crate::bridge_routing_handlers::notify_bridges_config_changed(
                &app_state,
                fauna_protocol::bridge_routing::config_change_reason::LOCAL_DOMAINS,
            )
            .await;
            // Every ATProto handle on the box is derived from this domain, so the
            // identity domain moving renames the WHOLE roster at once
            // (`atproto-pds-bridge.md` § Identity — a Fauna handle *or domain*
            // change re-derives). No actor hint: the nudge covers all of them.
            // This one site catches every cause — the anchor flip, the abort
            // re-flip, and any raced divergence — because it is the single place
            // the projection actually converges onto the DB primary. Latency
            // only: the bridge's own pass compares derived-vs-published, so a
            // dropped push delays a rename, never loses it.
            crate::bridge_atproto_handlers::notify_bridges_atproto_projection_ready(
                &app_state, None,
            )
            .await;
            tracing::info!(
                domain = %primary.domain_name,
                "primary-domain identity reconcile: projection swapped to the DB primary + config_changed(local_domains) pushed"
            );
        }

        // Primary-domain rename (SLICE 4): the grace watcher. When a `grace` row's
        // wall-clock window has elapsed (`NOW() > grace_ends_at`), promote it to
        // `ready_to_complete` — the source-of-truth advancement past grace
        // (`mail-primary-domain-rename.md` § Architectural rules — no admin action
        // is needed for `grace → ready_to_complete`; the admin's `complete` gates
        // only `ready_to_complete → completed`). No cert/DNS/identity side effect:
        // `ready_to_complete` is functionally identical to `grace` for the keep-
        // alives (both `is_post_flip_active`), so nothing to re-derive. Idempotent
        // + crash-safe — re-read fresh from the persisted row each tick, so a
        // restart mid-grace resumes the watch; a row that just entered `grace` this
        // tick has `grace_ends_at = now + grace_days`, so it won't promote early.
        if let Some(rename) = app_state.db.get_active_rename().await.ok().flatten()
            && rename.parsed_state() == Some(fauna_mail::RenameState::Grace)
            && let Some(ends_at) = rename.grace_ends_at
            && crate::db::now_epoch_millis() > ends_at
        {
            match app_state
                .db
                .advance_rename_to_ready_to_complete(&rename.rename_id)
                .await
            {
                Ok(_) => tracing::info!(
                    "primary-domain rename: grace window elapsed — ready_to_complete (awaiting admin complete)"
                ),
                Err(e) => tracing::warn!(
                    "primary-domain rename: grace watcher advance to ready_to_complete failed: {e:#}"
                ),
            }
        }

        iterations += 1;
        if let Some(max) = max_iterations
            && iterations >= max
        {
            break;
        }

        // Charge a failed IP-bridge order to the shared budget HERE, after the
        // domain arm has had its say: a domain success calls `retry.reset()`,
        // which would otherwise erase a failure recorded earlier in the same
        // tick and let the bridge re-attempt unpaced.
        if ip_issue_failed {
            retry.record_failure(now_unix());
            retry.save(&config.acme_dir);
        }

        // After a failed attempt, pace the next one within the rate budget
        // (expedite while under it, hold once spent). Otherwise — a healthy cert,
        // or a fresh issuance — fall back to the steady poll. The persisted log
        // ages out on its own, so no explicit reset is needed on the happy path.
        let wait = if issue_failed {
            next_attempt_delay(&retry, now_unix())
        } else {
            steady_poll
        };
        // Wait until the next scheduled tick, but let an admin "retry issuance
        // now" wake (`acme_retry_notify`, fired by
        // `fauna.bridges.restore_real_tls_cert`) cut it short — and only when the
        // failed-validation budget is actually free. A wake that arrives
        // mid-backoff is ignored and we keep waiting out the deadline, so a wake
        // expedites the self-heal (e.g. right after port 80 opens) yet can never
        // fire an attempt that would blow Let's Encrypt's rate limit
        // (`mail-bridge-lifecycle.md` § Self-healing). The pacing sleep stays the
        // single source of truth for budget timing — the wake never shortens it
        // below what the budget permits.
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                _ = app_state.acme_retry_notify.notified() => {
                    if next_attempt_delay(&retry, now_unix()).is_zero() {
                        tracing::info!(
                            "ACME: admin retry-issuance wake accepted — budget free, attempting now"
                        );
                        break;
                    }
                    tracing::info!(
                        wait_secs = next_attempt_delay(&retry, now_unix()).as_secs(),
                        "ACME: admin retry-issuance wake held within the failed-validation budget"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_acme_http01::test_support::make_multi_san_cert_pem;

    use std::sync::Arc as StdArc;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_app_state() -> StdArc<crate::routes::AppState> {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        let app_state = StdArc::new(crate::routes::AppState::for_test(db));
        // The lifecycle loop reads its apex from `handle_domain()` (the live
        // identity domain), not the boot `config.domain` (domainless boot).
        // `for_test` defaults to the non-orderable `localhost`
        // fallback, so seed a real, orderable identity domain — otherwise the
        // issuance-timing tests below see an empty desired-SAN set and never
        // attempt issuance. (Matches `test_config`'s `nest.example.com`.)
        app_state
            .identity_domain
            .store(Some(StdArc::new("nest.example.com".to_string())));
        app_state
    }

    fn test_config(acme_dir: std::path::PathBuf) -> Http01Config {
        Http01Config::new("nest.example.com".into(), acme_dir, 8080, None)
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_resumes_persisted_budget_before_first_attempt() {
        use std::time::Duration;
        let dir = tempfile::tempdir().expect("tempdir");

        // A prior process spent the whole failed-validation budget "just now",
        // so a restart must NOT fire a fresh immediate attempt — it waits until
        // the oldest of those failures ages out of the rolling hour (~1 h).
        let now = now_unix();
        let prior = RetryState {
            recent_failures: (0..ACME_FAILED_VALIDATION_BUDGET_PER_HOUR as u64)
                .map(|i| now + i)
                .collect(),
        };
        prior.save(dir.path());

        let calls = StdArc::new(AtomicUsize::new(0));
        let first_at: StdArc<StdMutex<Option<tokio::time::Instant>>> =
            StdArc::new(StdMutex::new(None));
        let start = tokio::time::Instant::now();

        let issue = {
            let calls = calls.clone();
            let first_at = first_at.clone();
            move |_desired: Vec<String>| {
                let calls = calls.clone();
                let first_at = first_at.clone();
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        *first_at.lock().unwrap() = Some(tokio::time::Instant::now());
                    }
                    Ok(())
                }
            }
        };

        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            test_app_state(),
            issue,
            Some(1),
        )
        .await;

        assert_eq!(calls.load(Ordering::SeqCst), 1, "issuer should run once");
        let waited = first_at.lock().unwrap().expect("issuer ran") - start;
        assert!(
            waited >= Duration::from_secs(ACME_RATE_WINDOW_SECS - 60),
            "with the budget spent, the first attempt must wait ~1 h for the window \
             to clear rather than fire immediately on restart, waited {waited:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_attempts_immediately_with_no_persisted_failure() {
        use std::time::Duration;
        let dir = tempfile::tempdir().expect("tempdir");
        // No retry-state file → healthy → no initial wait.

        let first_at: StdArc<StdMutex<Option<tokio::time::Instant>>> =
            StdArc::new(StdMutex::new(None));
        let start = tokio::time::Instant::now();
        let issue = {
            let first_at = first_at.clone();
            move |_desired: Vec<String>| {
                let first_at = first_at.clone();
                async move {
                    let mut g = first_at.lock().unwrap();
                    if g.is_none() {
                        *g = Some(tokio::time::Instant::now());
                    }
                    Ok(())
                }
            }
        };

        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            test_app_state(),
            issue,
            Some(1),
        )
        .await;

        let waited = first_at.lock().unwrap().expect("issuer ran") - start;
        assert!(
            waited < Duration::from_secs(5),
            "a healthy boot must attempt immediately, waited {waited:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_persists_failed_attempt_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Fake issuer that always fails — the loop must log each failed attempt
        // so the budget survives the next boot.
        let issue = move |_desired: Vec<String>| async move {
            Err(anyhow::anyhow!("validation failed (port 80 blocked)"))
        };

        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            test_app_state(),
            issue,
            Some(2),
        )
        .await;

        let persisted = RetryState::load(dir.path());
        assert_eq!(
            persisted.recent_failures.len(),
            2,
            "two failed attempts must persist two log entries for the next boot to resume"
        );
    }

    // The admin "retry issuance now" accelerator (`acme_retry_notify`, fired by
    // `fauna.bridges.restore_real_tls_cert`): a wake expedites the next attempt
    // past the steady poll *when the budget is free*, but must NEVER fire an
    // attempt while the failed-validation budget is spent — that would blow
    // Let's Encrypt's rate limit (`mail-bridge-lifecycle.md` § Self-healing).

    #[tokio::test(start_paused = true)]
    async fn retry_notify_expedites_issuance_within_budget() {
        use std::time::Duration;
        let dir = tempfile::tempdir().expect("tempdir");
        // No cert on disk + no persisted failures → need_issue true, budget free.
        // The fake issuer succeeds but never writes a cert, so need_issue stays
        // true each tick; on success the loop falls back to the 5-min steady
        // poll, which a pre-armed wake must cut short.
        let app_state = test_app_state();
        let calls = StdArc::new(AtomicUsize::new(0));
        let second_at: StdArc<StdMutex<Option<tokio::time::Instant>>> =
            StdArc::new(StdMutex::new(None));
        let start = tokio::time::Instant::now();
        let issue = {
            let calls = calls.clone();
            let second_at = second_at.clone();
            move |_desired: Vec<String>| {
                let calls = calls.clone();
                let second_at = second_at.clone();
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                        *second_at.lock().unwrap() = Some(tokio::time::Instant::now());
                    }
                    Ok(())
                }
            }
        };

        // Pre-arm the wake so the post-iteration-1 select! takes the notify
        // branch instead of sleeping out the 5-min steady poll.
        app_state.acme_retry_notify.notify_one();

        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            app_state,
            issue,
            Some(2),
        )
        .await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "both iterations should attempt issuance"
        );
        let second = second_at.lock().unwrap().expect("second attempt ran") - start;
        assert!(
            second < Duration::from_secs(60),
            "an admin retry-wake must expedite the 2nd attempt past the 5-min \
             steady poll, but it waited {second:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_notify_does_not_expedite_while_budget_is_spent() {
        use std::time::Duration;
        let dir = tempfile::tempdir().expect("tempdir");
        // Empty start (immediate first attempt) + an always-failing issuer, so
        // after iteration 1 the loop is in its post-failure backoff with the
        // budget no longer free. A pre-armed wake interrupts that backoff — but
        // because the budget is spent it must be IGNORED: the 2nd attempt must
        // still wait out the full ~15-min pacing window, never fire immediately,
        // or repeated admin clicks could drive attempts past LE's rate limit.
        let app_state = test_app_state();
        let calls = StdArc::new(AtomicUsize::new(0));
        let second_at: StdArc<StdMutex<Option<tokio::time::Instant>>> =
            StdArc::new(StdMutex::new(None));
        let start = tokio::time::Instant::now();
        let issue = {
            let calls = calls.clone();
            let second_at = second_at.clone();
            move |_desired: Vec<String>| {
                let calls = calls.clone();
                let second_at = second_at.clone();
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                        *second_at.lock().unwrap() = Some(tokio::time::Instant::now());
                    }
                    Err::<(), anyhow::Error>(anyhow::anyhow!("validation failed (port 80 blocked)"))
                }
            }
        };

        // One wake permit: it interrupts the backoff sleep after iteration 1.
        app_state.acme_retry_notify.notify_one();

        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            app_state,
            issue,
            Some(2),
        )
        .await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "both iterations attempt (always-fail issuer, max 2)"
        );
        let second = second_at.lock().unwrap().expect("second attempt ran") - start;
        let min_interval = Duration::from_secs(
            ACME_RATE_WINDOW_SECS / ACME_FAILED_VALIDATION_BUDGET_PER_HOUR as u64,
        );
        assert!(
            second >= min_interval - Duration::from_secs(60),
            "a wake while the budget is spent must NOT expedite the 2nd attempt — \
             it must wait out the ~{min_interval:?} pacing window, but it fired after {second:?}"
        );
    }

    // -------------------------------------------------------------------------
    // desired_san_domains + cert SAN coverage
    // -------------------------------------------------------------------------

    #[test]
    fn desired_sans_apex_only_when_no_mail_domains() {
        let sans = desired_san_domains(
            "nest.example.com",
            &[],
            &InfraSans::default(),
            ApexSans::INCLUDED,
        );
        assert_eq!(sans, vec!["nest.example.com".to_string()]);
    }

    #[test]
    fn desired_sans_adds_mail_host_and_dedups_apex() {
        // Single-domain milestone: node apex == the mail domain. The domain
        // itself dedups against the apex; `mail.<domain>` is added.
        let sans = desired_san_domains(
            "example.com",
            &["example.com".to_string()],
            &InfraSans::default(),
            ApexSans::INCLUDED,
        );
        assert_eq!(
            sans,
            vec!["example.com".to_string(), "mail.example.com".to_string()]
        );
    }

    #[test]
    fn desired_sans_multi_domain_lowercased_and_trimmed() {
        // Realistic multi-domain shape: the apex IS the primary mail domain
        // (`example.com`), `other.org` is a secondary. Names lowercased/trimmed;
        // blank entries skipped.
        let sans = desired_san_domains(
            "Example.com",
            &[
                "Example.com".to_string(),
                "Other.org".to_string(),
                "  ".to_string(),
            ],
            &InfraSans::default(),
            ApexSans::INCLUDED,
        );
        assert_eq!(
            sans,
            vec![
                "example.com".to_string(),
                "mail.example.com".to_string(),
                "other.org".to_string(),
            ]
        );
    }

    #[test]
    fn desired_sans_no_mail_host_for_secondary_domains() {
        // The multi-domain regression guard: there is ONE MX (`mail.<primary>`).
        // A secondary domain contributes ONLY its apex — never `mail.<secondary>`
        // (which has no A record by design and would poison the all-or-nothing
        // ACME order, taking the primary's cert down). `mail-multidomain.md`
        // § Architectural rules "One MX target".
        let sans = desired_san_domains(
            "primary.test",
            &["primary.test".to_string(), "secondary.test".to_string()],
            &InfraSans::default(),
            ApexSans::INCLUDED,
        );
        assert_eq!(
            sans,
            vec![
                "primary.test".to_string(),
                "mail.primary.test".to_string(),
                "secondary.test".to_string(),
            ]
        );
        assert!(
            !sans.iter().any(|s| s == "mail.secondary.test"),
            "a secondary domain must NOT contribute a mail.<secondary> SAN — one MX \
             (mail.<primary>) serves all domains; got {sans:?}"
        );
    }

    #[test]
    fn desired_sans_adds_relay_host_only_when_iroh_relay_enabled() {
        let mails = ["nest.example.com".to_string()];
        // Relay OFF (the default, and what a non-relay real-domain nest sees): no
        // `relay.<apex>` — so the ACME order never includes an unresolvable name.
        let off = desired_san_domains(
            "nest.example.com",
            &mails,
            &InfraSans::default(),
            ApexSans::INCLUDED,
        );
        assert!(
            !off.iter().any(|s| s.starts_with("relay.")),
            "relay host must be absent when the sidecar is off; got {off:?}"
        );
        // Relay ON: `relay.<apex>` is appended (one relay per nest, on the apex),
        // alongside the apex + mail host.
        let on = desired_san_domains(
            "nest.example.com",
            &mails,
            &InfraSans {
                relay: true,
                ..Default::default()
            },
            ApexSans::INCLUDED,
        );
        assert_eq!(
            on,
            vec![
                "nest.example.com".to_string(),
                "mail.nest.example.com".to_string(),
                "relay.nest.example.com".to_string(),
            ],
            "relay.<apex> appended when enabled"
        );
    }

    #[test]
    fn desired_sans_adds_pds_host_only_when_atproto_bridge_gate_passes() {
        let mails = ["nest.example.com".to_string()];
        // PDS OFF (every box that has not stood the ATProto bridge up): no
        // `pds.<apex>` in the all-or-nothing order.
        let off = desired_san_domains(
            "nest.example.com",
            &mails,
            &InfraSans::default(),
            ApexSans::INCLUDED,
        );
        assert!(
            !off.iter().any(|s| s.starts_with("pds.")),
            "pds host must be absent when the bridge gate is closed; got {off:?}"
        );
        // PDS ON, relay OFF: the two gates are independent — enabling ATProto
        // hosting must not drag `relay.<apex>` into the order.
        let on = desired_san_domains(
            "nest.example.com",
            &mails,
            &InfraSans {
                pds: true,
                ..Default::default()
            },
            ApexSans::INCLUDED,
        );
        assert_eq!(
            on,
            vec![
                "nest.example.com".to_string(),
                "mail.nest.example.com".to_string(),
                "pds.nest.example.com".to_string(),
            ],
            "pds.<apex> appended when enabled, without relay"
        );
    }

    #[test]
    fn desired_sans_both_infra_hosts_in_stable_order() {
        // Both gates open: relay before pds, so a cert-coverage diff never churns
        // on ordering alone.
        let sans = desired_san_domains(
            "nest.example.com",
            &["nest.example.com".to_string()],
            &InfraSans {
                relay: true,
                pds: true,
            },
            ApexSans::INCLUDED,
        );
        assert_eq!(
            sans,
            vec![
                "nest.example.com".to_string(),
                "mail.nest.example.com".to_string(),
                "relay.nest.example.com".to_string(),
                "pds.nest.example.com".to_string(),
            ]
        );
    }

    #[test]
    fn desired_sans_infra_hosts_absent_on_a_domainless_box() {
        // A domainless nest has no apex to hang an infra subdomain off; even with
        // both gates nominally open the set must stay empty of `.`-less garbage
        // like "relay." / "pds." (the floor cert covers loopback serving).
        let sans = desired_san_domains(
            "",
            &[],
            &InfraSans {
                relay: true,
                pds: true,
            },
            ApexSans::INCLUDED,
        );
        assert!(
            sans.is_empty(),
            "a domainless box orders nothing; got {sans:?}"
        );
    }

    // ── infra-subdomain pre-resolution ─────────────

    /// A scripted [`RecordResolver`] returning a canned outcome per record type,
    /// recording the names it was asked about so a test can assert the query.
    struct StubResolver {
        a: crate::dns_verifier::LookupOutcome,
        aaaa: crate::dns_verifier::LookupOutcome,
        seen: StdMutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl crate::dns_verifier::RecordResolver for StubResolver {
        async fn lookup(
            &self,
            name: &str,
            record_type: &str,
        ) -> crate::dns_verifier::LookupOutcome {
            self.seen
                .lock()
                .unwrap()
                .push(format!("{record_type} {name}"));
            match record_type {
                "A" => self.a.clone(),
                "AAAA" => self.aaaa.clone(),
                _ => crate::dns_verifier::LookupOutcome::Empty,
            }
        }
    }
    fn stub(
        a: crate::dns_verifier::LookupOutcome,
        aaaa: crate::dns_verifier::LookupOutcome,
    ) -> StubResolver {
        StubResolver {
            a,
            aaaa,
            seen: StdMutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn relay_resolves_true_on_a_record_and_queries_relay_host() {
        use crate::dns_verifier::LookupOutcome;
        let r = stub(
            LookupOutcome::Records(vec!["203.0.113.7".into()]),
            LookupOutcome::Empty,
        );
        assert!(infra_host_resolves(&r, "relay", "nest.example.com").await);
        // It asks about `relay.<apex>`, not the bare apex.
        assert!(
            r.seen
                .lock()
                .unwrap()
                .iter()
                .any(|q| q == "A relay.nest.example.com")
        );
    }

    #[tokio::test]
    async fn relay_resolves_true_on_aaaa_only() {
        use crate::dns_verifier::LookupOutcome;
        // No A, but an AAAA exists → still resolvable (the loop tries both).
        let r = stub(
            LookupOutcome::Empty,
            LookupOutcome::Records(vec!["2001:db8::7".into()]),
        );
        assert!(infra_host_resolves(&r, "relay", "nest.example.com").await);
    }

    #[tokio::test]
    async fn relay_resolves_false_on_nxdomain() {
        use crate::dns_verifier::LookupOutcome;
        let r = stub(LookupOutcome::Empty, LookupOutcome::Empty);
        assert!(
            !infra_host_resolves(&r, "relay", "nest.example.com").await,
            "no A/AAAA → must not enter the apex order"
        );
    }

    #[tokio::test]
    async fn pds_resolves_queries_the_pds_host_not_the_relay_or_apex() {
        use crate::dns_verifier::LookupOutcome;
        // The label is what distinguishes the two infra gates — a copy-paste that
        // left `relay` in place would silently gate the `pds.<apex>` SAN on the
        // *relay's* A record (and vice versa), which is exactly how one service's
        // DNS state could take another's SAN down.
        let r = stub(
            LookupOutcome::Records(vec!["203.0.113.7".into()]),
            LookupOutcome::Empty,
        );
        assert!(infra_host_resolves(&r, "pds", "nest.example.com").await);
        let seen = r.seen.lock().unwrap().clone();
        assert!(
            seen.iter().any(|q| q == "A pds.nest.example.com"),
            "must query pds.<apex>; got {seen:?}"
        );
        assert!(
            !seen.iter().any(|q| q.contains("relay.")),
            "must not query the relay host; got {seen:?}"
        );
    }

    #[tokio::test]
    async fn pds_resolves_false_on_nxdomain_so_apex_is_never_risked() {
        use crate::dns_verifier::LookupOutcome;
        // The whole point of the gate: an admin who approved the ATProto bridge
        // but has not yet published `pds.<apex>` must not poison the apex order.
        let r = stub(LookupOutcome::Empty, LookupOutcome::Empty);
        assert!(!infra_host_resolves(&r, "pds", "nest.example.com").await);
        let t = stub(LookupOutcome::Transient, LookupOutcome::Transient);
        assert!(
            !infra_host_resolves(&t, "pds", "nest.example.com").await,
            "a resolver hiccup must fail-safe to omit"
        );
    }

    #[tokio::test]
    async fn relay_resolves_false_on_transient_so_apex_is_never_risked() {
        use crate::dns_verifier::LookupOutcome;
        // A resolver timeout/error must fail-safe to "omit" — never block or risk
        // the apex renewal on a relay DNS hiccup.
        let r = stub(LookupOutcome::Transient, LookupOutcome::Transient);
        assert!(!infra_host_resolves(&r, "relay", "nest.example.com").await);
    }

    // ── secondary-domain apex resolve gate (multi-domain ACME poisoning guard) ──
    //
    // An ACME order is all-or-nothing; a secondary domain that does not resolve to
    // the nest must be DEFERRED, never dropped into the order, or its HTTP-01
    // failure takes the primary's cert down too (the reported bug).

    fn verifier_from_stub(
        a: crate::dns_verifier::LookupOutcome,
        aaaa: crate::dns_verifier::LookupOutcome,
    ) -> crate::dns_verifier::DnsVerifier {
        crate::dns_verifier::DnsVerifier::new(StdArc::new(stub(a, aaaa)), StdArc::new(|| 1000i64))
    }

    fn host_addr(v4: &str, v6: Option<&str>) -> crate::db::nest_host_address::HostAddress {
        crate::db::nest_host_address::HostAddress {
            nest_ipv4: v4.to_string(),
            nest_ipv6: v6.map(str::to_string),
            mail_ipv4: v4.to_string(),
            mail_ipv6: None,
        }
    }

    #[tokio::test]
    async fn secondary_resolves_true_when_apex_serves_the_nest_ipv4() {
        use crate::dns_verifier::LookupOutcome;
        let v = verifier_from_stub(
            LookupOutcome::Records(vec!["203.0.113.7".into()]),
            LookupOutcome::Empty,
        );
        let host = host_addr("203.0.113.7", None);
        assert!(secondary_apex_resolves_to_nest(&v, "secondary.test", Some(&host)).await);
    }

    #[tokio::test]
    async fn secondary_resolves_false_when_apex_points_at_another_host() {
        use crate::dns_verifier::LookupOutcome;
        // The user's exact scenario: the second domain's A record exists but points
        // somewhere OTHER than the nest → HTTP-01 would fail → must be deferred so
        // it can't poison the primary's order.
        let v = verifier_from_stub(
            LookupOutcome::Records(vec!["198.51.100.9".into()]),
            LookupOutcome::Empty,
        );
        let host = host_addr("203.0.113.7", None);
        assert!(!secondary_apex_resolves_to_nest(&v, "secondary.test", Some(&host)).await);
    }

    #[tokio::test]
    async fn secondary_resolves_false_when_apex_has_no_record() {
        use crate::dns_verifier::LookupOutcome;
        let v = verifier_from_stub(LookupOutcome::Empty, LookupOutcome::Empty);
        let host = host_addr("203.0.113.7", None);
        assert!(!secondary_apex_resolves_to_nest(&v, "secondary.test", Some(&host)).await);
    }

    #[tokio::test]
    async fn secondary_resolves_true_on_ipv6_match_only() {
        use crate::dns_verifier::LookupOutcome;
        let v = verifier_from_stub(
            LookupOutcome::Empty,
            LookupOutcome::Records(vec!["2001:db8::7".into()]),
        );
        let host = host_addr("203.0.113.7", Some("2001:db8::7"));
        assert!(secondary_apex_resolves_to_nest(&v, "secondary.test", Some(&host)).await);
    }

    #[tokio::test]
    async fn secondary_resolves_transient_defers_never_risks_primary() {
        use crate::dns_verifier::LookupOutcome;
        // A resolver hiccup on a secondary must fail-safe to "defer", never risk the
        // primary's renewal.
        let v = verifier_from_stub(LookupOutcome::Transient, LookupOutcome::Transient);
        let host = host_addr("203.0.113.7", None);
        assert!(!secondary_apex_resolves_to_nest(&v, "secondary.test", Some(&host)).await);
    }

    #[tokio::test]
    async fn secondary_weak_fallback_when_host_address_unknown() {
        use crate::dns_verifier::LookupOutcome;
        // Host address not persisted → weak "has any record" gate: a domain with an
        // A record is admitted, one with none is deferred.
        let with_record = verifier_from_stub(
            LookupOutcome::Records(vec!["198.51.100.9".into()]),
            LookupOutcome::Empty,
        );
        assert!(secondary_apex_resolves_to_nest(&with_record, "secondary.test", None).await);
        let without = verifier_from_stub(LookupOutcome::Empty, LookupOutcome::Empty);
        assert!(!secondary_apex_resolves_to_nest(&without, "secondary.test", None).await);
    }

    #[tokio::test]
    async fn reachable_keeps_primary_and_defers_unresolved_secondary() {
        use crate::dns_verifier::LookupOutcome;
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        db.set_host_address(&host_addr("203.0.113.7", None))
            .await
            .expect("set host address");
        let mut st = crate::routes::AppState::for_test(db);
        // No A/AAAA for any name → every secondary is unresolved.
        st.dns_verifier = StdArc::new(verifier_from_stub(
            LookupOutcome::Empty,
            LookupOutcome::Empty,
        ));
        let st = StdArc::new(st);
        let out = reachable_mail_domains(
            &st,
            "primary.test",
            &["primary.test".to_string(), "secondary.test".to_string()],
        )
        .await;
        assert_eq!(
            out,
            vec!["primary.test".to_string()],
            "the primary is always kept; an unresolved secondary is deferred so it \
             can't poison the primary's order"
        );
    }

    #[tokio::test]
    async fn reachable_admits_secondary_that_points_at_the_nest() {
        use crate::dns_verifier::LookupOutcome;
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        db.set_host_address(&host_addr("203.0.113.7", None))
            .await
            .expect("set host address");
        let mut st = crate::routes::AppState::for_test(db);
        // Every name resolves to the nest IPv4 → the secondary is admitted.
        st.dns_verifier = StdArc::new(verifier_from_stub(
            LookupOutcome::Records(vec!["203.0.113.7".into()]),
            LookupOutcome::Empty,
        ));
        let st = StdArc::new(st);
        let out = reachable_mail_domains(
            &st,
            "primary.test",
            &["primary.test".to_string(), "secondary.test".to_string()],
        )
        .await;
        assert_eq!(
            out,
            vec!["primary.test".to_string(), "secondary.test".to_string()]
        );
    }

    // ── primary-rename `mail.<new>` MX-host resolve gate (SLICE 2) ────────────
    //
    // Symmetric to the secondary-apex gate, but keyed on the MAIL address
    // (`mail_ipv4`) — the rename's `mail.<new>` A record points at the MX host,
    // which may differ from the nest/apex address.

    #[tokio::test]
    async fn mail_host_resolves_true_when_it_serves_the_mail_addr() {
        use crate::dns_verifier::LookupOutcome;
        let v = verifier_from_stub(
            LookupOutcome::Records(vec!["203.0.113.7".into()]),
            LookupOutcome::Empty,
        );
        let host = host_addr("203.0.113.7", None); // mail_ipv4 == 203.0.113.7
        assert!(mail_host_resolves_to_mail_ip(&v, "mail.new.test", &host).await);
    }

    #[tokio::test]
    async fn mail_host_resolves_false_when_it_points_at_another_host() {
        use crate::dns_verifier::LookupOutcome;
        // An A record exists but points elsewhere → HTTP-01 would fail → defer so
        // it can't poison the primary's all-or-nothing order.
        let v = verifier_from_stub(
            LookupOutcome::Records(vec!["198.51.100.9".into()]),
            LookupOutcome::Empty,
        );
        let host = host_addr("203.0.113.7", None);
        assert!(!mail_host_resolves_to_mail_ip(&v, "mail.new.test", &host).await);
    }

    #[tokio::test]
    async fn mail_host_gate_keys_on_mail_ipv4_not_nest_ipv4() {
        use crate::dns_verifier::LookupOutcome;
        // Distinct mail vs nest addresses (mail-host-ip-distinct-from-nest-ip): an
        // A record serving the MAIL ip must pass even though it does NOT match the
        // nest ip — the gate keys on `mail_ipv4`.
        let v = verifier_from_stub(
            LookupOutcome::Records(vec!["198.51.100.9".into()]),
            LookupOutcome::Empty,
        );
        let host = crate::db::nest_host_address::HostAddress {
            nest_ipv4: "203.0.113.7".to_string(),
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9".to_string(),
            mail_ipv6: None,
        };
        assert!(
            mail_host_resolves_to_mail_ip(&v, "mail.new.test", &host).await,
            "the mail-host gate must verify against mail_ipv4, not nest_ipv4"
        );
    }

    // ── the rename mail-host gates' fail direction: strong-check-or-DROP ──────
    //
    // `mail-primary-domain-rename.md` § Renaming away from a dead domain owns the
    // ruling (extended 2026-08-13 from the old apex to these two gates). The
    // deciding fact is structural, not analogical: `dns_handlers.rs`
    // `append_rename_mail_host` — the SOLE publisher of both `mail.<new>`
    // (pre-flip) and `mail.<old>` (grace) — reads the same `nest_host_address`
    // this gate does, and is a no-op without it. So where the address was NEVER
    // persisted — the arm these pins cover, and the only one a pre-flip rename
    // can reach — this deployment had published NO record at that name, and
    // whatever the resolver answers is by construction someone else's (a zone
    // wildcard, a stale row, a squatter). The weak fallback could therefore never
    // produce a true positive there — only a false one, admitting an unvalidatable
    // name into the all-or-nothing order and failing every issuance
    // deployment-wide. (The transient-error arm during grace is the one place a
    // weak test could have been right; `rename_mail_host_reachable`'s own doc
    // comment records why dropping there is near-free anyway.)

    #[tokio::test]
    async fn rename_mail_host_dropped_when_the_host_address_is_not_persisted() {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        // No `set_host_address`, and `mail.<new>` DOES answer — with a record we
        // cannot have published, since the publisher is gated on the very address
        // we lack. The old weak fallback admitted exactly this.
        let st =
            rename_test_state_per_name(&db, &[("mail.new.example.com", SQUATTER_IP)], false).await;
        assert!(
            !rename_mail_host_reachable(&st, "mail.new.example.com").await,
            "strong-check-or-DROP: with no persisted host address there is no strong \
             check to pass and no record of ours to find, so the SAN goes rather than \
             gamble every issuance on a name we never published"
        );
    }

    #[tokio::test]
    async fn rename_mail_host_kept_when_it_serves_the_mail_addr() {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state_per_name(&db, &[("mail.new.example.com", NEST_IP)], true).await;
        assert!(
            rename_mail_host_reachable(&st, "mail.new.example.com").await,
            "the gate must still admit a mail host that genuinely serves this \
             deployment — DROP-on-uncertainty is not DROP-on-everything"
        );
    }

    #[tokio::test]
    async fn rename_mail_host_dropped_when_it_points_at_another_host() {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st =
            rename_test_state_per_name(&db, &[("mail.new.example.com", SQUATTER_IP)], true).await;
        assert!(
            !rename_mail_host_reachable(&st, "mail.new.example.com").await,
            "a record pointing at another host fails HTTP-01 — the strong check is \
             what keeps it out of the all-or-nothing order"
        );
    }

    // ── primary-rename cert-lifecycle driver (SLICE 2): requested → cert_ready ──

    // ── § B-IP bridge arm ────────────────────────────────────────────────────
    //
    // These drive `cert_lifecycle_loop` directly (with a resolver and a fake IP
    // issuer) rather than the `cert_lifecycle_loop_domain_only` shim the domain
    // tests use. The apex is left non-orderable in most of them so the domain
    // arm is a no-op and only the bridge's own decisions are under test — which
    // is also the state of the box the bridge exists for: domainless, pre-claim.

    /// A fake IP issuer that records every SAN set it was asked for and writes
    /// the `ip-*.pem` pair the real one would, so `install_ip_cert` can load it.
    /// `fail_families` makes an order fail whenever the requested set contains
    /// an IPv6 address — the asymmetry the v4-only retry exists for.
    fn ip_issuer(
        acme_dir: std::path::PathBuf,
        orders: StdArc<StdMutex<Vec<Vec<std::net::IpAddr>>>>,
        fail_on_v6: bool,
    ) -> impl Fn(
        Vec<std::net::IpAddr>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
        move |addrs: Vec<std::net::IpAddr>| {
            let acme_dir = acme_dir.clone();
            let orders = orders.clone();
            Box::pin(async move {
                orders.lock().unwrap().push(addrs.clone());
                if fail_on_v6 && addrs.iter().any(|a| a.is_ipv6()) {
                    anyhow::bail!("the CA could not reach :80 over IPv6");
                }
                let names: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
                let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                let (cert, key) =
                    fauna_acme_http01::test_support::make_multi_san_cert_and_key_pem(&refs);
                tokio::fs::write(acme_dir.join(crate::acme::IP_CERT_FILENAME), cert)
                    .await
                    .expect("write ip cert");
                tokio::fs::write(acme_dir.join(crate::acme::IP_KEY_FILENAME), key)
                    .await
                    .expect("write ip key");
                Ok(())
            })
        }
    }

    fn floor_resolver() -> Arc<crate::acme::MultiDomainCertResolver> {
        let (cert, key) =
            fauna_acme_http01::test_support::make_multi_san_cert_and_key_pem(&["localhost"]);
        let dir = tempfile::tempdir().expect("tempdir");
        let cert_path = dir.path().join("c.pem");
        let key_path = dir.path().join("k.pem");
        std::fs::write(&cert_path, cert).expect("cert");
        std::fs::write(&key_path, key).expect("key");
        Arc::new(crate::acme::MultiDomainCertResolver::new(
            crate::acme::load_certified_key(&cert_path, &key_path).expect("floor key"),
        ))
    }

    /// The happy path: a domainless box with a public address orders one cert
    /// over **all** its global-unicast addresses and installs it, so a no-SNI
    /// dial is served it immediately (`tls-certificates.md` § B-IP *Serving*).
    #[tokio::test]
    async fn ip_bridge_orders_one_cert_and_installs_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let orders = StdArc::new(StdMutex::new(Vec::new()));
        let resolver = floor_resolver();
        let addrs = vec![
            "203.0.113.7".parse().unwrap(),
            "2001:db8::1".parse::<std::net::IpAddr>().unwrap(),
        ];

        let failed = ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&resolver),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), false),
            false,
            &addrs,
        )
        .await;

        assert!(!failed, "a successful order is not a budget failure");
        let placed = orders.lock().unwrap().clone();
        assert_eq!(
            placed,
            vec![addrs.clone()],
            "exactly ONE order covering every address — the resolver can serve only \
             one cert to a no-SNI dial, so per-family certs would be unservable"
        );
        assert!(
            resolver.current_ip_cert().is_some(),
            "the issued bridge must be installed without waiting for a restart — the \
             cert watcher only watches the domain cert's filenames"
        );
    }

    /// § B's all-or-nothing rule, applied to the bridge: an IPv6 address the CA
    /// cannot reach on `:80` must not take the IPv4 cert down with it.
    #[tokio::test]
    async fn ip_bridge_retries_v4_only_when_the_dual_family_order_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let orders = StdArc::new(StdMutex::new(Vec::new()));
        let resolver = floor_resolver();
        let v4: std::net::IpAddr = "203.0.113.7".parse().unwrap();
        let v6: std::net::IpAddr = "2001:db8::1".parse().unwrap();

        let failed = ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&resolver),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), true),
            false,
            &[v4, v6],
        )
        .await;

        assert!(
            !failed,
            "the v4-only retry succeeded, so the tick did not fail"
        );
        assert_eq!(
            orders.lock().unwrap().clone(),
            vec![vec![v4, v6], vec![v4]],
            "the dual-family order is attempted first, then narrowed to v4 — never \
             the other way round, and never two independent per-family orders"
        );
        assert!(
            resolver.current_ip_cert().is_some(),
            "the v4 bridge is installed"
        );
    }

    /// After the v4-only narrowing has succeeded, the chain on disk covers only
    /// v4 while `addrs` still names v6, so a coverage check against the live
    /// derived set is false on EVERY subsequent tick and the bridge re-orders
    /// forever — two CA orders per 5-minute steady poll, indefinitely, on the
    /// shared ACME budget.
    #[tokio::test]
    async fn ip_bridge_v4_narrowed_cert_is_not_reordered_on_the_next_tick() {
        let dir = tempfile::tempdir().expect("tempdir");
        let orders = StdArc::new(StdMutex::new(Vec::new()));
        let resolver = floor_resolver();
        let v4: std::net::IpAddr = "203.0.113.7".parse().unwrap();
        let v6: std::net::IpAddr = "2001:db8::1".parse().unwrap();

        // Tick 1: dual-family order fails, v4-only retry succeeds and installs.
        ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&resolver),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), true),
            false,
            &[v4, v6],
        )
        .await;
        assert_eq!(orders.lock().unwrap().len(), 2, "tick 1 places two orders");

        // Tick 2, 5 minutes later: nothing has changed — same interfaces, a cert
        // minutes old, nowhere near its one-third renewal lead.
        ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&resolver),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), true),
            false,
            &[v4, v6],
        )
        .await;

        assert_eq!(
            orders.lock().unwrap().len(),
            2,
            "a fresh v4-narrowed bridge must not be re-ordered on the next tick — \
             the CA's duplicate-certificate budget is 5/week for one SAN set"
        );
    }

    /// § B-IP *Lifetime*: "once the domain cert is live the IP cert is left to
    /// lapse, and the floor serves IP dials again."
    #[tokio::test]
    async fn ip_bridge_is_dropped_once_the_primary_domain_cert_is_trusted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let orders = StdArc::new(StdMutex::new(Vec::new()));
        let resolver = floor_resolver();
        let addrs = vec!["203.0.113.7".parse::<std::net::IpAddr>().unwrap()];

        // Bridge first, to have something to drop.
        ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&resolver),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), false),
            false,
            &addrs,
        )
        .await;
        assert!(resolver.current_ip_cert().is_some(), "bridge installed");

        // …then the deployment's own domain cert goes live.
        let failed = ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&resolver),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), false),
            true,
            &addrs,
        )
        .await;

        assert!(!failed);
        assert!(
            resolver.current_ip_cert().is_none(),
            "the bridge is dropped and IP dials return to the floor"
        );
        assert_eq!(
            orders.lock().unwrap().len(),
            1,
            "and nothing is re-ordered — the bridge is left to lapse, never renewed onward"
        );
    }

    /// A restart must not respend the CA's duplicate-certificate budget (5/week
    /// for one SAN set): a still-fresh chain on disk is loaded, not re-ordered.
    #[tokio::test]
    async fn ip_bridge_reconciles_a_fresh_chain_on_disk_without_reordering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let orders = StdArc::new(StdMutex::new(Vec::new()));
        let addrs = vec!["203.0.113.7".parse::<std::net::IpAddr>().unwrap()];

        // First boot orders and writes the pair.
        let first = floor_resolver();
        ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&first),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), false),
            false,
            &addrs,
        )
        .await;
        assert_eq!(orders.lock().unwrap().len(), 1);

        // Second boot: fresh resolver, same acme_dir.
        let second = floor_resolver();
        ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&second),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), false),
            false,
            &addrs,
        )
        .await;

        assert_eq!(
            orders.lock().unwrap().len(),
            1,
            "the fresh chain on disk is reused — a restart loop must not respend the \
             duplicate-certificate budget"
        );
        assert!(
            second.current_ip_cert().is_some(),
            "…and it is installed, so the bridge survives a restart with no re-order"
        );
    }

    /// An address that appeared after issuance (a floating IP attached post-boot)
    /// is not covered by the chain on disk, which therefore cannot be served for
    /// it — so it is re-ordered even though it is nowhere near its renewal lead.
    #[tokio::test]
    async fn ip_bridge_reorders_when_the_san_set_no_longer_matches_the_interfaces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let orders = StdArc::new(StdMutex::new(Vec::new()));
        let resolver = floor_resolver();
        let v4: std::net::IpAddr = "203.0.113.7".parse().unwrap();
        let extra: std::net::IpAddr = "198.51.100.9".parse().unwrap();

        ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&resolver),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), false),
            false,
            &[v4],
        )
        .await;
        ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            Some(&resolver),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), false),
            false,
            &[v4, extra],
        )
        .await;

        assert_eq!(
            orders.lock().unwrap().clone(),
            vec![vec![v4], vec![v4, extra]],
            "the new address forces a re-order — otherwise it would go uncovered until \
             the renewal lead came due, and no-SNI dials to it could not be served"
        );
    }

    /// A plain-HTTP nest has no TLS listener to serve a bridge on, so nothing is
    /// ordered at all — the arm returns before it even reads the interface table.
    #[tokio::test]
    async fn ip_bridge_orders_nothing_without_a_tls_resolver() {
        let dir = tempfile::tempdir().expect("tempdir");
        let orders = StdArc::new(StdMutex::new(Vec::new()));

        let failed = ip_bridge_pass_inner(
            &test_config(dir.path().to_path_buf()),
            None,
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), false),
            false,
            &["203.0.113.7".parse().unwrap()],
        )
        .await;

        assert!(!failed);
        assert!(orders.lock().unwrap().is_empty(), "no listener, no bridge");
    }

    /// A box booted with `FAUNA_MODE=private` (the immutable `config.nest.mode`
    /// seed) and later flipped to `public` live via the app-set ceremony
    /// (`fauna.setup.nat_mode` → `AppState.node_mode`, restart-applied per
    /// `nat_mode_core.rs`) must still get its IP-bridge cert ordered on the
    /// FIRST pass after that restart — the live mode is what the box actually
    /// is now. Reading the stale boot seed instead returns an empty address
    /// set (`ip_bridge_addresses` no-ops on `Private`) and strands the box on
    /// the self-signed floor forever, since nothing else ever revisits this
    /// decision (`docs/goal/architecture/nest/common.md` § NAT mode).
    #[tokio::test]
    async fn ip_bridge_reads_the_live_node_mode_not_the_boot_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let orders = StdArc::new(StdMutex::new(Vec::new()));
        let resolver = floor_resolver();
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        let app_state = StdArc::new(crate::routes::AppState::for_test_with_node_mode(
            db,
            crate::config::NodeMode::Private,
        ));
        assert_eq!(
            app_state.config.nest.mode,
            crate::config::NodeMode::Private,
            "the boot seed stays private — only the live axis flips"
        );
        *app_state.node_mode.write().await = crate::config::NodeMode::Public;

        // A genuinely global-unicast address — unlike `ip_bridge_pass_inner`'s
        // other callers above, this path runs through `ip_bridge_addresses`'s
        // own global-unicast filter, which rejects the `203.0.113.0/24`
        // TEST-NET-3 documentation range those tests use.
        let addrs = vec!["93.184.216.34".parse().unwrap()];
        let failed = ip_bridge_pass_for_attached(
            &test_config(dir.path().to_path_buf()),
            &app_state,
            Some(&resolver),
            &ip_issuer(dir.path().to_path_buf(), orders.clone(), false),
            false,
            &addrs,
        )
        .await;

        assert!(!failed, "a successful order is not a budget failure");
        assert_eq!(
            orders.lock().unwrap().clone(),
            vec![addrs],
            "must order using the LIVE public mode, not the stale private boot seed \
             (which would derive an empty address set and order nothing)"
        );
    }

    /// Fake issuer that writes a self-signed cert covering the requested SAN set
    /// to `cert_path`, so the loop's post-issue coverage check sees the new SAN.
    fn cert_writing_issuer(
        cert_path: std::path::PathBuf,
    ) -> impl Fn(Vec<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
    {
        move |desired: Vec<String>| {
            let cert_path = cert_path.clone();
            Box::pin(async move {
                let refs: Vec<&str> = desired.iter().map(|s| s.as_str()).collect();
                tokio::fs::write(&cert_path, make_multi_san_cert_pem(&refs))
                    .await
                    .expect("write test cert");
                Ok(())
            })
        }
    }

    async fn rename_test_state(
        db: &StdArc<crate::db::CacheDb>,
        resolver_a: crate::dns_verifier::LookupOutcome,
    ) -> StdArc<crate::routes::AppState> {
        use crate::dns_verifier::LookupOutcome;
        db.set_host_address(&host_addr("203.0.113.7", None))
            .await
            .expect("host addr");
        let primary = db
            .add_mail_domain(
                "nest.example.com",
                true,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .expect("primary");
        let new_dom = db
            .add_mail_domain(
                "new.example.com",
                false,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .expect("new domain");
        db.insert_domain_rename(&primary.domain_id, &new_dom.domain_id, 7, &[9u8; 32])
            .await
            .expect("insert rename");
        let mut st = crate::routes::AppState::for_test(db.clone());
        st.identity_domain
            .store(Some(StdArc::new("nest.example.com".to_string())));
        st.dns_verifier = StdArc::new(verifier_from_stub(resolver_a, LookupOutcome::Empty));
        StdArc::new(st)
    }

    // `start_paused`: the loop's inter-tick wait is a 5-minute steady poll, so a
    // multi-tick run must ride tokio's virtual clock rather than sit out real
    // minutes (the file's own idiom — see `lifecycle_persists_failed_attempt_log`).
    #[tokio::test(start_paused = true)]
    async fn rename_drives_requested_to_cert_ready_when_new_mail_host_resolves() {
        use crate::dns_verifier::LookupOutcome;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        // Every name resolves to 203.0.113.7 (== nest_ipv4 AND mail_ipv4 via
        // host_addr), so both the secondary apex and `mail.<new>` pass their gates.
        let st = rename_test_state(&db, LookupOutcome::Records(vec!["203.0.113.7".into()])).await;
        let cert_path = dir.path().join(crate::acme::CERT_FILENAME);

        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            st,
            cert_writing_issuer(cert_path),
            Some(1),
        )
        .await;

        let row = db
            .get_active_rename()
            .await
            .unwrap()
            .expect("rename still active");
        assert_eq!(
            row.parsed_state(),
            Some(fauna_mail::RenameState::CertReady),
            "the rename must reach cert_ready once mail.<new> resolves and the cert covers it"
        );
        assert!(
            row.new_cert_fingerprint.is_some(),
            "cert_ready stamps the SPKI fingerprint"
        );
        assert!(
            row.cert_acquired_at.is_some(),
            "cert_ready stamps the acquisition time"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rename_stays_cert_issuance_when_new_mail_host_unresolved() {
        use crate::dns_verifier::LookupOutcome;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        // No records for any name → `mail.<new>` never resolves → its SAN is
        // deferred and the row waits in cert_issuance (advanced from requested, but
        // never stamped cert_ready). The primary's own cert is unaffected.
        let st = rename_test_state(&db, LookupOutcome::Empty).await;
        let cert_path = dir.path().join(crate::acme::CERT_FILENAME);

        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            st,
            cert_writing_issuer(cert_path),
            Some(1),
        )
        .await;

        let row = db.get_active_rename().await.unwrap().unwrap();
        assert_eq!(
            row.parsed_state(),
            Some(fauna_mail::RenameState::CertIssuance),
            "an unresolved mail.<new> leaves the row in cert_issuance, not cert_ready"
        );
        assert!(
            row.new_cert_fingerprint.is_none(),
            "no fingerprint until the cert actually covers mail.<new>"
        );
    }

    // ── primary-rename cert-lifecycle driver (SLICE 3): cert_ready → grace ──────

    #[tokio::test(start_paused = true)]
    async fn rename_anchor_flip_advances_cert_ready_to_grace_and_swaps_identity() {
        use crate::dns_verifier::LookupOutcome;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        // Every name resolves to the host addr, so `mail.<new>` passes its gate and
        // the rename reaches cert_ready on tick 1.
        let st = rename_test_state(&db, LookupOutcome::Records(vec!["203.0.113.7".into()])).await;
        let cert_path = dir.path().join(crate::acme::CERT_FILENAME);

        // Tick 1 → cert_ready; tick 2 → anchor flip → grace (the flip fires the
        // tick *after* cert_ready, so cert_ready stays observable).
        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            st.clone(),
            cert_writing_issuer(cert_path),
            Some(2),
        )
        .await;

        let row = db
            .get_active_rename()
            .await
            .unwrap()
            .expect("rename still active");
        assert_eq!(
            row.parsed_state(),
            Some(fauna_mail::RenameState::Grace),
            "cert_ready flips to grace on the next tick"
        );
        assert!(row.flipped_at.is_some(), "the flip stamps flipped_at");
        assert!(row.grace_ends_at.is_some(), "the flip stamps grace_ends_at");

        // is_primary flipped atomically: new.example.com is now the primary.
        let primary = db.lookup_primary_mail_domain().await.unwrap().unwrap();
        assert_eq!(primary.domain_name, "new.example.com");
        // The runtime identity projection followed the flip (else the cert loop
        // would keep ordering around the old apex).
        assert_eq!(
            st.handle_domain(),
            "new.example.com",
            "apply_primary_identity swapped the identity_domain cache to the new primary"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rename_grace_keeps_both_mail_hosts_in_cert_superset() {
        use crate::dns_verifier::LookupOutcome;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state(&db, LookupOutcome::Records(vec!["203.0.113.7".into()])).await;
        let cert_path = dir.path().join(crate::acme::CERT_FILENAME);

        // Tick 1 → cert_ready; tick 2 → flip → grace; tick 3 → a grace tick that
        // re-issues keeping `mail.<old>` in the desired SAN set (superset).
        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            st.clone(),
            cert_writing_issuer(cert_path.clone()),
            Some(3),
        )
        .await;

        assert_eq!(
            db.get_active_rename()
                .await
                .unwrap()
                .unwrap()
                .parsed_state(),
            Some(fauna_mail::RenameState::Grace),
        );
        let pem = tokio::fs::read(&cert_path).await.expect("cert on disk");
        // The grace-window superset covers BOTH the old and new MX hosts, so a
        // renewal during grace can't drop `mail.<old>` while peers still reach it.
        assert!(
            cert_covers_sans(&pem, &["mail.nest.example.com".to_string()]),
            "grace keeps mail.<old-primary> in the cert superset"
        );
        assert!(
            cert_covers_sans(&pem, &["mail.new.example.com".to_string()]),
            "the new primary's mail host stays covered"
        );
    }

    // ── renaming away from a DEAD old primary: the apex-SAN gate ──────────────
    //
    // `mail-primary-domain-rename.md` § Renaming away from a dead domain. The
    // rename is the ONLY re-home path off a lapsed or seized domain, and the ACME
    // order is all-or-nothing — so while the dead old domain is still primary its
    // un-gated `<apex>` / `mail.<apex>` fail *every* order, including the
    // `cert_issuance` order that must add `mail.<new>`. The machinery built to
    // leave the dead zone was held hostage by it.

    /// A [`RecordResolver`] with a per-name script (`A` records only, which is all
    /// these gates read). The single-outcome [`StubResolver`] cannot express a
    /// dead zone beside a live one — the whole point of these tests.
    struct PerNameResolver {
        a: std::collections::HashMap<String, Vec<String>>,
    }
    #[async_trait::async_trait]
    impl crate::dns_verifier::RecordResolver for PerNameResolver {
        async fn lookup(
            &self,
            name: &str,
            record_type: &str,
        ) -> crate::dns_verifier::LookupOutcome {
            use crate::dns_verifier::LookupOutcome;
            if !record_type.eq_ignore_ascii_case("A") {
                return LookupOutcome::Empty;
            }
            match self.a.get(&name.trim().to_ascii_lowercase()) {
                Some(recs) => LookupOutcome::Records(recs.clone()),
                None => LookupOutcome::Empty,
            }
        }
    }

    fn per_name_verifier(pairs: &[(&str, &str)]) -> crate::dns_verifier::DnsVerifier {
        let a = pairs
            .iter()
            .map(|(n, ip)| (n.to_ascii_lowercase(), vec![(*ip).to_string()]))
            .collect();
        crate::dns_verifier::DnsVerifier::new(
            StdArc::new(PerNameResolver { a }),
            StdArc::new(|| 1000i64),
        )
    }

    /// The nest's own address in these tests (`host_addr` sets it as BOTH
    /// `nest_ipv4` and `mail_ipv4`).
    const NEST_IP: &str = "203.0.113.7";
    /// Where a seized old zone points once the squatter parks it — an A record
    /// that exists (so the weak "any record" test would KEEP it) but answers no
    /// HTTP-01 challenge of ours.
    const SQUATTER_IP: &str = "198.51.100.9";

    /// Fake issuer that models ACME's **all-or-nothing** HTTP-01 order: every
    /// identifier is validated by being fetched AT that name, so an order holding
    /// one name that does not point at this nest fails ENTIRELY and writes no
    /// cert. `orders` records each requested SAN set. Without this the loop tests
    /// cannot see the deadlock at all — [`cert_writing_issuer`] always succeeds,
    /// which is precisely the assumption a dead zone breaks.
    fn all_or_nothing_issuer(
        cert_path: std::path::PathBuf,
        validatable: Vec<String>,
        orders: StdArc<StdMutex<Vec<Vec<String>>>>,
    ) -> impl Fn(Vec<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
    {
        move |desired: Vec<String>| {
            let cert_path = cert_path.clone();
            let validatable = validatable.clone();
            let orders = orders.clone();
            Box::pin(async move {
                orders.lock().unwrap().push(desired.clone());
                if let Some(bad) = desired.iter().find(|d| !validatable.contains(d)) {
                    anyhow::bail!("HTTP-01 validation failed for {bad}: the whole order fails");
                }
                let refs: Vec<&str> = desired.iter().map(|s| s.as_str()).collect();
                tokio::fs::write(&cert_path, make_multi_san_cert_pem(&refs))
                    .await
                    .expect("write test cert");
                Ok(())
            })
        }
    }

    /// `rename_test_state`'s per-name sibling: same two domains + pre-flip rename
    /// row, but a scripted resolver so the old and new zones can differ.
    async fn rename_test_state_per_name(
        db: &StdArc<crate::db::CacheDb>,
        dns: &[(&str, &str)],
        persist_host_address: bool,
    ) -> StdArc<crate::routes::AppState> {
        if persist_host_address {
            db.set_host_address(&host_addr(NEST_IP, None))
                .await
                .expect("host addr");
        }
        let primary = db
            .add_mail_domain(
                "nest.example.com",
                true,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .expect("primary");
        let new_dom = db
            .add_mail_domain(
                "new.example.com",
                false,
                "testing",
                "expand_primary",
                None,
                None,
            )
            .await
            .expect("new domain");
        db.insert_domain_rename(&primary.domain_id, &new_dom.domain_id, 7, &[9u8; 32])
            .await
            .expect("insert rename");
        let mut st = crate::routes::AppState::for_test(db.clone());
        st.identity_domain
            .store(Some(StdArc::new("nest.example.com".to_string())));
        st.dns_verifier = StdArc::new(per_name_verifier(dns));
        StdArc::new(st)
    }

    /// The deadlock, end to end: the old primary is **seized** (its A record now
    /// answers for the squatter), the new domain is live and published. Before the
    /// apex gate this order carried `nest.example.com` + `mail.nest.example.com`,
    /// failed wholesale at HTTP-01, never covered `mail.<new>`, and the rename sat
    /// in `cert_issuance` forever — the re-home held hostage by the zone it flees.
    #[tokio::test(start_paused = true)]
    async fn rename_away_from_a_seized_old_apex_still_reaches_cert_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state_per_name(
            &db,
            &[
                // The seized old zone: parked at the squatter, so it resolves (the
                // weak gate would keep it) but answers no challenge of ours.
                ("nest.example.com", SQUATTER_IP),
                ("mail.nest.example.com", SQUATTER_IP),
                // The new zone is published and points here.
                ("new.example.com", NEST_IP),
                ("mail.new.example.com", NEST_IP),
            ],
            true,
        )
        .await;
        let cert_path = dir.path().join(crate::acme::CERT_FILENAME);
        let orders = StdArc::new(StdMutex::new(Vec::new()));
        let validatable = vec![
            "new.example.com".to_string(),
            "mail.new.example.com".to_string(),
        ];

        // Tick 1 → cert_ready (the widened order issues at last); tick 2 → the
        // anchor flip, so this pins the whole escape, not just its first step.
        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            st,
            all_or_nothing_issuer(cert_path, validatable, orders.clone()),
            Some(2),
        )
        .await;

        let ordered = orders.lock().unwrap().clone();
        let first = ordered.first().expect("an order was attempted");
        assert!(
            !first.contains(&"nest.example.com".to_string())
                && !first.contains(&"mail.nest.example.com".to_string()),
            "the seized old primary's SANs must be gated out of the all-or-nothing order; \
             got {first:?}"
        );
        assert!(
            first.contains(&"mail.new.example.com".to_string()),
            "the rename's own new MX host is what the widened order exists to add; got {first:?}"
        );
        // Reaching `grace` proves the whole escape: the widened order issued (so the
        // row passed `cert_ready`) and the anchor flip ran. Before the gate this sat
        // in `cert_issuance` for every tick, forever — the deadlock.
        assert_eq!(
            db.get_active_rename()
                .await
                .unwrap()
                .expect("rename still active")
                .parsed_state(),
            Some(fauna_mail::RenameState::Grace),
            "a rename away from a dead/seized old primary must complete its cert_issuance \
             order and flip — it cannot be held hostage by the zone it flees; \
             orders attempted: {ordered:?}"
        );
        assert_eq!(
            db.lookup_primary_mail_domain()
                .await
                .unwrap()
                .unwrap()
                .domain_name,
            "new.example.com",
            "is_primary moved to the new domain"
        );
    }

    /// The beside-control: a **live** old zone keeps both its SANs, so the
    /// voluntary rename's dual-binding is untouched (this must have passed before
    /// the gate too — it is what proves the gate is surgical rather than a blanket
    /// narrowing).
    #[tokio::test(start_paused = true)]
    async fn rename_away_from_a_live_old_apex_keeps_its_sans() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state_per_name(
            &db,
            &[
                ("nest.example.com", NEST_IP),
                ("mail.nest.example.com", NEST_IP),
                ("new.example.com", NEST_IP),
                ("mail.new.example.com", NEST_IP),
            ],
            true,
        )
        .await;
        let cert_path = dir.path().join(crate::acme::CERT_FILENAME);
        let orders = StdArc::new(StdMutex::new(Vec::new()));

        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            st,
            all_or_nothing_issuer(
                cert_path,
                vec![
                    "nest.example.com".into(),
                    "mail.nest.example.com".into(),
                    "new.example.com".into(),
                    "mail.new.example.com".into(),
                ],
                orders.clone(),
            ),
            Some(1),
        )
        .await;

        assert_eq!(
            db.get_active_rename()
                .await
                .unwrap()
                .unwrap()
                .parsed_state(),
            Some(fauna_mail::RenameState::CertReady),
        );
        let last = orders.lock().unwrap().last().cloned().expect("an order");
        assert!(
            last.contains(&"nest.example.com".to_string())
                && last.contains(&"mail.nest.example.com".to_string()),
            "a live old zone keeps its apex SANs — the voluntary dual-binding is untouched; \
             got {last:?}"
        );
    }

    // ── `apex_sans_for_cycle`, the gate's verdict in isolation ────────────────

    #[tokio::test]
    async fn apex_sans_included_when_no_rename_is_active() {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        db.set_host_address(&host_addr(NEST_IP, None))
            .await
            .expect("host addr");
        let mut st = crate::routes::AppState::for_test(db.clone());
        // Nothing resolves — and it must not matter: with no rename in flight the
        // apex is never gated, or an ordinary DNS hiccup would narrow the cert.
        st.dns_verifier = StdArc::new(per_name_verifier(&[]));
        let st = StdArc::new(st);
        assert_eq!(
            apex_sans_for_cycle(&st, "nest.example.com").await,
            ApexSans::INCLUDED
        );
    }

    #[tokio::test]
    async fn apex_sans_dropped_when_the_old_primary_is_seized() {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state_per_name(
            &db,
            &[
                ("nest.example.com", SQUATTER_IP),
                ("mail.nest.example.com", SQUATTER_IP),
            ],
            true,
        )
        .await;
        assert_eq!(
            apex_sans_for_cycle(&st, "nest.example.com").await,
            ApexSans::DROPPED,
            "an A record pointing at someone else is exactly what the STRONG check \
             rejects — the weak \"has any record\" test would have kept it"
        );
    }

    #[tokio::test]
    async fn apex_sans_included_when_the_old_primary_still_serves_this_nest() {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state_per_name(
            &db,
            &[
                ("nest.example.com", NEST_IP),
                ("mail.nest.example.com", NEST_IP),
            ],
            true,
        )
        .await;
        assert_eq!(
            apex_sans_for_cycle(&st, "nest.example.com").await,
            ApexSans::INCLUDED
        );
    }

    #[tokio::test]
    async fn apex_sans_dropped_when_the_host_address_is_not_persisted() {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        // No `set_host_address`, and the old apex DOES have a record: the secondary
        // gate's weak fallback would admit it. Here the fail direction inverts —
        // keeping a name we cannot strongly verify re-arms the deadlock, and the
        // rename is the escape, so uncertainty DROPS.
        let st = rename_test_state_per_name(
            &db,
            &[
                ("nest.example.com", SQUATTER_IP),
                ("mail.nest.example.com", SQUATTER_IP),
            ],
            false,
        )
        .await;
        assert_eq!(
            apex_sans_for_cycle(&st, "nest.example.com").await,
            ApexSans::DROPPED,
            "strong-check-or-DROP: with no persisted host address there is no strong \
             check to pass, so the apex SANs go rather than gamble the rename"
        );
    }

    /// The rename gates' half of the same ruling, end to end and at deployment
    /// scope. The old zone is **live** here — this is an ordinary voluntary
    /// rename, not the dead-zone case — but the host address is unpersisted, so
    /// `mail.<new>` was never published by us and the record answering there is
    /// someone else's (a zone wildcard is the everyday shape). Under the weak
    /// fallback that name joined the all-or-nothing order and failed it wholesale:
    /// the deployment got **no cert at all**, the primary's included, for as long
    /// as the rename stayed open. Strong-check-or-DROP makes the rename wait
    /// instead — visibly, in `cert_issuance` — while every validatable SAN renews.
    #[tokio::test(start_paused = true)]
    async fn rename_with_no_persisted_host_address_waits_instead_of_poisoning_the_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state_per_name(
            &db,
            &[
                ("nest.example.com", NEST_IP),
                ("mail.nest.example.com", NEST_IP),
                ("new.example.com", NEST_IP),
                // The wildcard/stale answer at a name we never published.
                ("mail.new.example.com", SQUATTER_IP),
            ],
            false,
        )
        .await;
        let cert_path = dir.path().join(crate::acme::CERT_FILENAME);
        let orders = StdArc::new(StdMutex::new(Vec::new()));
        // Everything that genuinely points here validates; `mail.<new>` does not.
        let validatable = vec![
            "nest.example.com".to_string(),
            "mail.nest.example.com".to_string(),
            "new.example.com".to_string(),
        ];

        cert_lifecycle_loop_domain_only(
            test_config(dir.path().to_path_buf()),
            st,
            all_or_nothing_issuer(cert_path.clone(), validatable, orders.clone()),
            Some(2),
        )
        .await;

        let ordered = orders.lock().unwrap().clone();
        let first = ordered.first().expect("an order was attempted");
        assert!(
            !first.contains(&"mail.new.example.com".to_string()),
            "a mail host we never published must not join the all-or-nothing order; \
             got {first:?}"
        );
        assert!(
            tokio::fs::try_exists(&cert_path).await.unwrap_or(false),
            "the order must still ISSUE — that is the whole point: one ungated \
             rename SAN used to take the deployment's entire cert down. Orders: \
             {ordered:?}"
        );
        // The rename waits rather than advancing on a SAN the order never carried.
        assert_eq!(
            db.get_active_rename()
                .await
                .unwrap()
                .expect("rename still active")
                .parsed_state(),
            Some(fauna_mail::RenameState::CertIssuance),
            "deferring the SAN must leave the row waiting, not falsely `cert_ready`"
        );
    }

    #[tokio::test]
    async fn apex_sans_included_once_the_rename_is_post_flip() {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state_per_name(&db, &[("nest.example.com", SQUATTER_IP)], true).await;
        // Drive the row to grace: post-flip the apex is the NEW primary (which must
        // never be gated) and the old domain is an ordinary secondary carrying the
        // secondary gate.
        let rename = db.get_active_rename().await.unwrap().unwrap();
        db.advance_rename_to_cert_issuance(&rename.rename_id)
            .await
            .expect("cert_issuance");
        db.mark_rename_cert_ready(&rename.rename_id, "ff", crate::db::now_epoch_millis())
            .await
            .expect("cert_ready");
        db.advance_rename_to_grace(&rename.rename_id)
            .await
            .expect("grace");
        assert_eq!(
            apex_sans_for_cycle(&st, "new.example.com").await,
            ApexSans::INCLUDED
        );
    }

    #[tokio::test]
    async fn apex_sans_included_when_the_apex_is_not_the_renamed_domain() {
        let db = StdArc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        // A pre-flip rename exists, but the apex passed in is some other name (a
        // mid-reconcile tick where the projection and the DB disagree). Gate
        // nothing: the licence covers exactly the domain being renamed away from.
        let st = rename_test_state_per_name(&db, &[], true).await;
        assert_eq!(
            apex_sans_for_cycle(&st, "unrelated.example.com").await,
            ApexSans::INCLUDED
        );
    }

    #[test]
    fn desired_sans_omits_a_gated_apex_even_though_it_is_an_active_mail_domain() {
        // The primary is in `list_active_mail_domains` too, so the per-domain loop
        // would re-admit the very name the gate dropped if it did not skip it.
        let sans = desired_san_domains(
            "primary.test",
            &["primary.test".to_string(), "secondary.test".to_string()],
            &InfraSans::default(),
            ApexSans::DROPPED,
        );
        assert_eq!(
            sans,
            vec!["secondary.test".to_string()],
            "a gated apex contributes neither <apex> nor mail.<apex>, and its own \
             mail_domains row must not smuggle it back in"
        );
    }

    #[test]
    fn desired_sans_can_drop_the_mail_host_alone() {
        let sans = desired_san_domains(
            "primary.test",
            &["primary.test".to_string()],
            &InfraSans::default(),
            ApexSans {
                apex: true,
                mail_host: false,
            },
        );
        assert_eq!(
            sans,
            vec!["primary.test".to_string()],
            "the two apex SANs are gated independently — a dead MX host does not \
             have to cost the apex its own SAN"
        );
    }
}
