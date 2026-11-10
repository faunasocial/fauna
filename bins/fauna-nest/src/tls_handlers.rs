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
        CertIssuanceError, CertStatusReply, CertStatusRequest, DomainCertStatus, IssuanceOrder,
        PublishCertReply, PublishCertRequest,
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
///
/// An in-flight primary-domain rename's MX hosts (pre-flip `mail.<new>`,
/// in-grace `mail.<old>`) are in the set too, from the one builder every
/// consumer shares ([`crate::acme_http01::rename_sans_for_cycle`]), un-gated
/// like the rest (`mail-primary-domain-rename.md` § *Issuer-independence — the
/// rename driver*, rule 1) — so a DNS-01 client's order widens the cert the
/// rename is waiting on.
pub(crate) async fn desired_listener_sans(state: &Arc<AppState>) -> Vec<String> {
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
    let rename = crate::acme_http01::rename_sans_for_cycle(
        state,
        crate::acme_http01::RenameSanGate::Ungated,
    )
    .await;
    crate::acme_http01::desired_san_domains(
        &apex,
        &mail_domain_names,
        &infra,
        crate::acme_http01::ApexSans::INCLUDED,
        &rename,
    )
}

/// The names in `desired` the listener does not serve under a **trusted
/// covering** cert right now — no cert for the name, a served cert that does
/// not cover it, or only the self-signed floor (`CertStatusReply::uncovered_sans`;
/// `tls-certificates.md` § B tier 2 → *The set carries a rename's hosts, and the
/// nest says what the served cert lacks*). Expiry is not this signal's: a
/// covering cert near `notAfter` is the per-domain `expiring` state's. A nest
/// with no TLS resolver (plain HTTP) serves no cert, so every desired name is
/// uncovered. Order follows `desired`.
pub(crate) fn uncovered_listener_sans(
    desired: &[String],
    resolver: Option<&Arc<dyn crate::acme::ServedCertSpki>>,
) -> Vec<String> {
    desired
        .iter()
        .filter(|name| {
            let facts = resolver.and_then(|r| r.served_cert_facts(name));
            !matches!(facts, Some(f) if f.covers && !f.is_floor)
        })
        .cloned()
        .collect()
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
            let desired_sans = desired_listener_sans(&state).await;
            let uncovered_sans = uncovered_listener_sans(&desired_sans, resolver);
            encode_reply(&CertStatusReply {
                statuses,
                desired_sans,
                uncovered_sans,
                issuance_errors: issuance_errors(&state.acme_dir),
                extra: Default::default(),
            })
        })
    })
}

/// The last error of each failing order the nest places itself, read from the
/// ACME state file `cert_lifecycle_loop` keeps (`tls-certificates.md` § Keeping
/// the cert alive, *The admin sees why issuance is failing*). `AppState::acme_dir`
/// is the directory the lifecycle task writes (both resolve `[acme].dir`). A
/// missing or unreadable file is no error. A tag this build does not know — a
/// newer build's order, read after a downgrade — is left out: the wire's unknown
/// arm is never written, and this build never places or clears that order.
fn issuance_errors(acme_dir: &std::path::Path) -> Vec<CertIssuanceError> {
    use crate::acme_http01::{IssuanceOrder as Stored, RetryState};
    RetryState::load(acme_dir)
        .last_errors
        .into_iter()
        .filter_map(|e| {
            let order = match Stored::from_tag(&e.order)? {
                Stored::Domain => IssuanceOrder::Domain,
                Stored::IpBridge => IssuanceOrder::IpBridge,
            };
            Some(CertIssuanceError {
                order,
                detail: e.detail,
                failed_at_unix: i64::try_from(e.failed_at_unix).unwrap_or(i64::MAX),
                extra: Default::default(),
            })
        })
        .collect()
}

/// This module's admin-plane kinds for an admin-consented principal —
/// the DNS page's certificate-health read (every certificate write is sovereign and absent) — each its actor handler behind
/// [`crate::principal_handlers::admin_twin`] (`authorization-server.md`
/// § Scope grammar → *The admin arms* names the set).
pub(crate) fn principal_admin_handlers()
-> Vec<(&'static str, crate::principal_handlers::PrincipalHandler)> {
    use crate::principal_handlers::admin_twin;
    let kinds: [(&'static str, crate::rpc_router::RpcHandler); 1] =
        [("fauna.tls.cert_status", cert_status_handler())];
    kinds
        .into_iter()
        .map(|(kind, actor)| (kind, admin_twin(kind, actor)))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acme::{ServedCertFacts, ServedCertSpki};
    use crate::acme_http01::tests::rename_test_state;
    use crate::dns_verifier::LookupOutcome;
    use std::collections::HashMap;

    #[tokio::test]
    async fn desired_listener_sans_carries_a_pre_flip_renames_new_mail_host_ungated() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        // Nothing resolves: `desired_sans` is un-gated (DNS-01 validates by a TXT
        // in the zone), so `mail.<new>` is in the set before its A record exists.
        let st = rename_test_state(&db, LookupOutcome::Empty).await;
        let sans = desired_listener_sans(&st).await;
        assert!(
            sans.contains(&"mail.new.example.com".to_string()),
            "a DNS-01 client must order the rename's new MX host; got {sans:?}"
        );
    }

    #[tokio::test]
    async fn desired_listener_sans_keeps_the_old_mail_host_through_grace() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("db"));
        let st = rename_test_state(&db, LookupOutcome::Empty).await;
        let rename = db.get_active_rename().await.unwrap().expect("active");
        db.advance_rename_to_cert_issuance(&rename.rename_id)
            .await
            .expect("cert_issuance");
        db.mark_rename_cert_ready(&rename.rename_id, "ff", crate::db::now_epoch_millis())
            .await
            .expect("cert_ready");
        db.advance_rename_to_grace(&rename.rename_id)
            .await
            .expect("grace");
        st.identity_domain
            .store(Some(Arc::new("new.example.com".to_string())));

        let sans = desired_listener_sans(&st).await;
        assert!(
            sans.contains(&"mail.nest.example.com".to_string()),
            "a DNS-01 renewal during grace must keep mail.<old> in the superset; got {sans:?}"
        );
        assert!(
            sans.contains(&"mail.new.example.com".to_string()),
            "the new primary's own MX host is the apex's; got {sans:?}"
        );
    }

    /// A served-cert double: canned facts per SNI.
    struct Served(HashMap<String, ServedCertFacts>);
    impl ServedCertSpki for Served {
        fn current_spki_sha256(&self) -> Option<[u8; 32]> {
            None
        }
        fn served_cert_facts(&self, sni: &str) -> Option<ServedCertFacts> {
            self.0.get(sni).copied()
        }
        fn served_cert_spki_sha256(&self, _sni: &str) -> Option<[u8; 32]> {
            None
        }
    }

    fn facts(covers: bool, is_floor: bool) -> ServedCertFacts {
        ServedCertFacts {
            not_before_unix: 0,
            not_after_unix: i64::MAX / 2,
            is_floor,
            covers,
        }
    }

    #[test]
    fn uncovered_sans_lists_what_the_served_cert_lacks_and_empties_once_covered() {
        let desired = vec![
            "nest.example.com".to_string(),
            "mail.nest.example.com".to_string(),
            "relay.nest.example.com".to_string(),
            "mail.new.example.com".to_string(),
        ];
        let lacking: Arc<dyn ServedCertSpki> = Arc::new(Served(HashMap::from([
            ("nest.example.com".to_string(), facts(true, false)),
            ("mail.nest.example.com".to_string(), facts(true, false)),
            // Only the floor answers for it: not a trusted cover.
            ("relay.nest.example.com".to_string(), facts(true, true)),
            // The served cert does not name the rename's new MX host.
            ("mail.new.example.com".to_string(), facts(false, false)),
        ])));
        assert_eq!(
            uncovered_listener_sans(&desired, Some(&lacking)),
            vec![
                "relay.nest.example.com".to_string(),
                "mail.new.example.com".to_string()
            ],
        );

        let covering: Arc<dyn ServedCertSpki> = Arc::new(Served(
            desired
                .iter()
                .map(|d| (d.clone(), facts(true, false)))
                .collect(),
        ));
        assert!(
            uncovered_listener_sans(&desired, Some(&covering)).is_empty(),
            "a served cert covering the whole set leaves nothing uncovered"
        );
        assert_eq!(
            uncovered_listener_sans(&desired, None),
            desired,
            "no TLS resolver serves no cert: every desired name is uncovered"
        );
    }
}
