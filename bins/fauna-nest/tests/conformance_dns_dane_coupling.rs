//! **TLS Phase 6 — tier_3 acceptance for the self-signed-MX DANE/TLSA ↔
//! cert coupling** (`tls-certificates.md` § D; Phase 5b emit + verify).
//!
//! Proves the coupling holds **through the registered `fauna.dns.*` kinds** —
//! the real `list_records` / `verify_records` handlers dispatched over a real
//! `RpcRouter`, against a real [`fauna_nest::acme::MultiDomainCertResolver`]
//! serving a real leaf:
//!
//! - **on the floor** (a self-signed `mail.<primary>` leaf, `is_floor == true`),
//!   `fauna.dns.list_records` emits exactly one host-level
//!   `_25._tcp.mail.<primary>` `TLSA 3 1 1 <hex(SPKI)>` row on the **primary**
//!   domain, and `fauna.dns.verify_records` runs that row through the verifier;
//! - once a **trusted** (CA-issued, `is_floor == false`) covering leaf is served
//!   for the MX, the row **withdraws** (a floor-key TLSA against a trusted cert
//!   would DANE-fail).
//!
//! The *assembler-level* gate (`append_floor_mx_tlsa` in isolation) and the
//! record body (`fauna_mail::dns::host::build_mail_tlsa_record`) are unit-tested
//! in `dns_handlers.rs` / `fauna-mail`; this test owns the **full-handler**
//! integration — that `assemble_domain_views` actually appends the row for the
//! primary, that the served-cert SPKI flows through the handler into the wire
//! row, and that the kind is registered + admin-gated — the closest achievable
//! analog to a binary-nest run.
//!
//! *(Why not a binary `nest_instance`: the tier_3 e2e nest serves plain HTTP
//! with **no** TLS resolver — `served_cert_spki == None` — so it emits no
//! floor-MX TLSA, and the no-variant-config rule forbids a domain-configured
//! HTTPS nest. The real-HTTPS DANE acceptance is the Phase-6 tier_4 docker
//! slice. See `tls-certificates.md` § Implementation status.)*

mod common;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::SigningKey;
use fauna_nest::acme::ServedCertSpki;
use fauna_nest::db::CacheDb;
use fauna_nest::dns_handlers::register_dns_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::dns::{
    ListRecordsReply, ListRecordsRequest, VerifyRecordsReply, VerifyRecordsRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

const PRIMARY: &str = "fauna.test";
const MX_SNI: &str = "mail.fauna.test";
const TLSA_NAME: &str = "_25._tcp.mail.fauna.test";

/// An `AppState` whose RPC router speaks `fauna.dns.*` and whose
/// `served_cert_spki` is `resolver` (or `None` to model a plain-HTTP nest).
fn nest_with_resolver(resolver: Option<Arc<dyn ServedCertSpki>>) -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        register_dns_handlers(&mut b);
        b.build()
    });
    Arc::new(AppState {
        rpc_router,
        served_cert_spki: resolver,
        ..AppState::for_test(db)
    })
}

// `resolver_from_pem`/`floor_resolver`/`trusted_resolver`: this file and
// `conformance_mta_sts_serving.rs` had this exact trio under the same names
// (`common::floor_resolver`/`common::trusted_resolver`).

async fn dispatch(
    state: &Arc<AppState>,
    kind: &str,
    actor: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = state
        .rpc_router
        .kind_meta(kind)
        .unwrap_or_else(|| panic!("{kind} registered"));
    (meta.handler)(state.clone(), actor, payload).await
}

/// Seed `state` with an admin actor (whose id is returned) and a single
/// `is_primary=true` mail domain — the deployment whose MX the TLSA pins.
async fn seed_admin_and_primary(state: &Arc<AppState>) -> [u8; 32] {
    let actor_sk = SigningKey::from_bytes(&[0x11u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    state.db.add_admin_actor(&actor[..]).await.unwrap();
    state
        .db
        .add_mail_domain(PRIMARY, true, "enforce", "expand_primary", None, None)
        .await
        .unwrap();
    actor
}

/// On the floor, `fauna.dns.list_records` emits exactly one
/// `_25._tcp.mail.<primary>` TLSA `3 1 1 <hex>` on the primary domain, and the
/// hex pins the SPKI the resolver would actually serve for the MX.
#[tokio::test]
async fn floor_mx_tlsa_emitted_over_list_records() {
    let resolver = common::floor_resolver(PRIMARY, MX_SNI);
    let expected_spki = resolver
        .served_cert_spki_sha256(MX_SNI)
        .expect("floor leaf has an SPKI");
    let expected_rdata = format!("3 1 1 {}", hex::encode(expected_spki));

    let state = nest_with_resolver(Some(resolver));
    let actor = seed_admin_and_primary(&state).await;

    let req = ListRecordsRequest {
        extra: Default::default(),
        domain: None,
    };
    let reply: ListRecordsReply = decode(
        &dispatch(&state, "fauna.dns.list_records", actor, encode(&req))
            .await
            .expect("list_records ok for admin"),
    )
    .unwrap();

    let primary = reply
        .domains
        .iter()
        .find(|d| d.domain == PRIMARY)
        .expect("primary domain in matrix");
    assert!(primary.is_primary, "the seeded domain is the primary");

    let tlsa: Vec<_> = primary
        .records
        .iter()
        .filter(|r| r.record_type == "TLSA")
        .collect();
    assert_eq!(
        tlsa.len(),
        1,
        "exactly one host-level floor-MX TLSA on the floor, got {tlsa:?}"
    );
    assert_eq!(tlsa[0].name, TLSA_NAME, "TLSA pins the floor MX host");
    assert_eq!(
        tlsa[0].expected, expected_rdata,
        "TLSA RDATA is `3 1 1 <sha256(served SPKI)>`, pinning the served floor leaf"
    );
}

/// Once a trusted (CA-issued) covering cert is served for the MX,
/// `fauna.dns.list_records` **withdraws** the floor-key TLSA (it would DANE-fail
/// against a trusted cert).
#[tokio::test]
async fn trusted_mx_withdraws_tlsa_over_list_records() {
    let state = nest_with_resolver(Some(common::trusted_resolver(PRIMARY, MX_SNI)));
    let actor = seed_admin_and_primary(&state).await;

    let req = ListRecordsRequest {
        extra: Default::default(),
        domain: None,
    };
    let reply: ListRecordsReply = decode(
        &dispatch(&state, "fauna.dns.list_records", actor, encode(&req))
            .await
            .expect("list_records ok for admin"),
    )
    .unwrap();

    let primary = reply
        .domains
        .iter()
        .find(|d| d.domain == PRIMARY)
        .expect("primary domain in matrix");
    assert!(
        primary.records.iter().all(|r| r.record_type != "TLSA"),
        "a trusted MX cert must not carry the floor-key TLSA, got {:?}",
        primary.records
    );
}

/// A plain-HTTP nest (`served_cert_spki == None`) emits no floor-MX TLSA — the
/// row is *cert-coupled*, not unconditional. This is exactly the tier_3 e2e
/// `nest_instance` reality (no TLS resolver), documenting why the binary-nest
/// DANE acceptance is the tier_4 HTTPS slice, not a tier_3 `nest_instance` test.
#[tokio::test]
async fn no_resolver_emits_no_tlsa_over_list_records() {
    let state = nest_with_resolver(None);
    let actor = seed_admin_and_primary(&state).await;

    let req = ListRecordsRequest {
        extra: Default::default(),
        domain: None,
    };
    let reply: ListRecordsReply = decode(
        &dispatch(&state, "fauna.dns.list_records", actor, encode(&req))
            .await
            .expect("list_records ok for admin"),
    )
    .unwrap();

    let primary = reply
        .domains
        .iter()
        .find(|d| d.domain == PRIMARY)
        .expect("primary domain in matrix");
    assert!(
        primary.records.iter().all(|r| r.record_type != "TLSA"),
        "no served cert → no floor-MX TLSA"
    );
}

/// On the floor, `fauna.dns.verify_records` runs the floor-MX TLSA through the
/// verifier alongside the rest of the matrix — proving the row is *checked*, not
/// just *emitted*. The e2e/for-test state installs the Null DNS resolver, so the
/// verdict is `checking` (the real ok/missing/mismatch logic is unit-tested in
/// `dns_verifier` / `fauna_mail::dns::verify`); here we assert the TLSA row is
/// present in the verify matrix with a verdict.
#[tokio::test]
async fn floor_mx_tlsa_checked_over_verify_records() {
    let state = nest_with_resolver(Some(common::floor_resolver(PRIMARY, MX_SNI)));
    let actor = seed_admin_and_primary(&state).await;

    let req = VerifyRecordsRequest {
        extra: Default::default(),
        domain: None,
    };
    let reply: VerifyRecordsReply = decode(
        &dispatch(&state, "fauna.dns.verify_records", actor, encode(&req))
            .await
            .expect("verify_records ok for admin"),
    )
    .unwrap();

    let primary = reply
        .domains
        .iter()
        .find(|d| d.domain == PRIMARY)
        .expect("primary domain in verify matrix");
    let tlsa = primary
        .records
        .iter()
        .find(|r| r.record_type == "TLSA")
        .expect("floor-MX TLSA is run through the verifier");
    assert_eq!(tlsa.name, TLSA_NAME);
    assert_eq!(
        tlsa.status, "checking",
        "Null DNS resolver → checking (real verdict logic is unit-tested)"
    );
}
