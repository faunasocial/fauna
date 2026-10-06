//! WS-RPC handlers for the nest TLS-certificate lifecycle surface (`fauna.tls.*`),
//! per `docs/goal/architecture/nest/tls-certificates.md` § B.
//!
//! `fauna.tls.publish_cert` is the delivery seam for a **client-issued** cert.
//! The admin's client obtains a publicly-trusted cert via a client-driven DNS-01
//! ACME order (no nest ever holds the DNS-provider key — `dns-management.md`
//! § Where the credential lives), seals it with
//! `fauna_mls::wrapped_blob::seal_lan_tls_cert_entry` to the identity key of the
//! nest it is *for*, and delivers the sealed `(ciphertext, actor_sig)` here.
//!
//! The handler always writes the entry into the **caller's own** actor
//! `namespace_entries` under the fixed `LAN_TLS_CERT_ENTRY_ID`
//! (`source = "local"`), and then hands it to
//! `lan_cert::install_client_issued_cert`, which installs it iff the blob opens
//! with *this* nest's identity key. So both topologies work through one seam:
//!
//! * **standalone** (one nest, no pair) — the cert was sealed to the receiving
//!   nest, so it installs immediately;
//! * **relay + private** — the client publishes to the reachable relay, whose
//!   seal does not open (it stores the blob opaquely, unable to read or forge
//!   it), and the paired private nest installs it on its next namespace-sync
//!   pull.
//!
//! Admin-only (the deployment's cert is an admin concern, gated in
//! `bridge_method_allowlist`).

use std::sync::Arc;
use std::time::Duration;

use fauna_mls::wrapped_blob::LAN_TLS_CERT_ENTRY_ID;
use fauna_protocol::{
    decode_strict as decode,
    tls::{
        CertStatusReply, CertStatusRequest, DomainCertStatus, PublishCertReply, PublishCertRequest,
    },
};

use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Error helpers (mirror dns_handlers; namespaced to fauna.tls) ──

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

// Every `fauna.tls.*` kind is Admin-only per
// `bridge_method_allowlist::is_permitted`.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// `fauna.tls.publish_cert` — receive a client-issued, sealed LAN-TLS cert entry.
///
/// Two things happen, in order, and they are independent:
///
/// 1. **Store** the entry in the caller's own actor namespace under the fixed
///    `LAN_TLS_CERT_ENTRY_ID`, so a paired private nest can pull it over
///    namespace-sync. Writing only to the caller's own namespace means there is
///    no cross-actor escalation surface; a re-publish is an idempotent
///    last-writer-wins upsert (no client-causable unrecoverable state).
/// 2. **Install it on this nest** if the blob is sealed to *this* nest's identity
///    key ([`crate::lan_cert::install_client_issued_cert`]) — the standalone
///    deployment, where the nest the admin issued the cert for is the very nest
///    they published it to, and no peer will ever pull it. On a relay the seal
///    does not open, so this is a no-op and step 1's stored entry is the whole
///    job (`lan_cert` module docs: the seal target is the authorization).
///
/// Step 2 is deliberately **not** gated on the NAT axis. A public nest whose :80
/// is unreachable (a cloud firewall, an ISP block) has no HTTP-01 path, and
/// client-driven DNS-01 is then its *only* route to a trusted cert
/// (`tls-certificates.md` § B — the tiers are ordered by applicability, not by
/// axis). Storing a cert the admin issued for this nest and then declining to
/// serve it would leave a deployment stranded on the floor with no client-side
/// recovery.
///
/// Storage failures are fatal to the call (the client must know the entry did
/// not persist); an install failure is not — the entry is stored, the nest keeps
/// serving its current cert, and `installed: false` reports it honestly.
fn publish_cert_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.tls.publish_cert").await?;
            let req: PublishCertRequest = decode(&payload).map_err(malformed)?;
            state
                .db
                .namespace_put(
                    &actor,
                    LAN_TLS_CERT_ENTRY_ID,
                    req.ciphertext.as_ref(),
                    req.actor_sig.as_ref(),
                )
                .await
                .map_err(internal)?;
            let installed = crate::lan_cert::install_client_issued_cert(
                &state,
                &actor,
                req.ciphertext.as_ref(),
                req.actor_sig.as_ref(),
            )
            .await;
            encode_reply(&PublishCertReply {
                ok: true,
                installed,
                extra: Default::default(),
            })
        })
    })
}

/// Every DNS name this nest's **listener** cert should cover, for a client about
/// to drive a DNS-01 order (`CertStatusReply::desired_sans`).
///
/// Reuses the issuer's own set-builder ([`crate::acme_http01::desired_san_domains`])
/// so a client-issued cert and a nest-issued one cover exactly the same names —
/// one source of truth for "what the listener serves", not a second hand-rolled
/// list that drifts.
///
/// **The resolve gates are deliberately omitted.** HTTP-01 gates each infra and
/// secondary-domain SAN on the name actually resolving, because that challenge
/// validates by being *fetched* at the name and an all-or-nothing order would
/// otherwise take the apex cert down. DNS-01 validates by publishing a TXT into the zone,
/// so an unpublished `A` record is irrelevant — applying the same gate here would
/// silently narrow a client-issued cert for no reason. The client applies the gate
/// that *is* load-bearing for DNS-01: it drops names outside the zones its
/// credential covers.
///
/// That covers the rename's apex gate too ([`crate::acme_http01::ApexSans`]):
/// HTTP-01 must drop a dead old primary's SANs to break the ACME deadlock, but a
/// DNS-01 client renaming away from a **live** old zone still holds that zone's
/// credential and should keep publishing for it — so this reports the un-gated
/// set ([`crate::acme_http01::ApexSans::INCLUDED`]) and lets the credential
/// decide, exactly as for secondaries and infra names.
async fn desired_listener_sans(state: &Arc<AppState>) -> Vec<String> {
    let Some(apex) = state.handle_domain_if_set() else {
        return Vec::new(); // domainless box — floor only, nothing orderable
    };
    let mail_domain_names: Vec<String> = match state.db.list_active_mail_domains().await {
        Ok(rows) => rows.into_iter().map(|d| d.domain_name).collect(),
        Err(e) => {
            tracing::warn!("cert_status: list_active_mail_domains failed: {e:#}");
            Vec::new()
        }
    };
    let infra = crate::acme_http01::InfraSans {
        relay: crate::discovery_core::relay_sidecar_connected(state),
        pds: crate::bridge_atproto_handlers::atproto_pds_bridge_approved(state).await,
    };
    crate::acme_http01::desired_san_domains(
        &apex,
        &mail_domain_names,
        &infra,
        crate::acme_http01::ApexSans::INCLUDED,
    )
}

/// `fauna.tls.cert_status` — report the health of the cert the nest's listener
/// **currently serves** for each requested domain, the nest-side truth behind
/// the `admin-dns` cert-status row (`tls-certificates.md` § C.4). Read-only,
/// Admin-only. For each domain the handler asks the live cert resolver
/// (`state.served_cert_spki`) what it would serve for that SNI and folds the
/// served leaf's facts + `now` into one of the three states
/// (`valid-trusted` / `on-floor — renew needed` / `expiring`) via
/// [`crate::acme::cert_health_state`]. A nest with no TLS resolver (plain HTTP)
/// reports every domain as on-floor (`not_after_unix = 0`).
///
/// The reply also carries [`desired_listener_sans`] — the SAN set a client-driven
/// DNS-01 order should request — because the client refreshes cert status
/// immediately before issuing, so the set it needs rides the read it already makes
/// rather than a second round trip.
fn cert_status_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.tls.cert_status").await?;
            let req: CertStatusRequest = decode(&payload).map_err(malformed)?;
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            let resolver = state.served_cert_spki.as_ref();
            let statuses = req
                .domains
                .into_iter()
                .map(|domain| {
                    let facts = resolver.and_then(|r| r.served_cert_facts(&domain));
                    let (not_after_unix, is_floor) = match facts {
                        Some(f) => (f.not_after_unix, f.is_floor),
                        None => (0, true),
                    };
                    DomainCertStatus {
                        domain,
                        state: crate::acme::cert_health_state(facts, now),
                        not_after_unix,
                        is_floor,
                        extra: Default::default(),
                    }
                })
                .collect();
            encode_reply(&CertStatusReply {
                statuses,
                desired_sans: desired_listener_sans(&state).await,
                extra: Default::default(),
            })
        })
    })
}

/// Register the `fauna.tls.*` kinds with the WS-RPC router.
pub fn register_tls_handlers(b: &mut RpcRouterBuilder) {
    fn meta(handler: RpcHandler) -> RpcKindMeta {
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler,
        }
    }
    b.add("fauna.tls.publish_cert", meta(publish_cert_handler()));
    b.add("fauna.tls.cert_status", meta(cert_status_handler()));
}
