//! In-process WS-RPC handler tests for the pre-identity public-discovery kinds
//! `fauna.nest.info`, `fauna.handle.available`, `fauna.nest.resolve`,
//! `fauna.actor.by_handle`, `fauna.setup.status` — dispatch each registered
//! handler directly (no socket), exercising the shared `discovery_core` reads
//! and the `DiscoveryError` → `RpcError` mapping. Socket-level coverage (the
//! anonymous endpoint + the kinds resolving over the wire) lives in
//! `pre_identity_ws.rs`. (Discovery slice; tracked internally.)

mod common;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::discovery_handlers::register_discovery_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::discovery::{
    ActorByHandleReply, ActorByHandleRequest, HandleAvailableReply, HandleAvailableRequest,
    NestInfoReply, NestInfoRequest, NestResolveReply, NestResolveRequest, SetupStatusReply,
    SetupStatusRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_discovery_handlers(&mut b);
    b.build()
}

async fn state() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    Arc::new(AppState::for_test(db))
}

/// Dispatch a kind through the registered handler. The anonymous connection
/// binds no actor — the discovery reads never depended on one, so the
/// dispatcher's actor arg is irrelevant here.
async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, [0u8; 32], payload).await
}

#[tokio::test]
async fn nest_info_reports_public_metadata() {
    let r = router();
    let st = state().await;
    let bytes = dispatch(
        &r,
        st,
        "fauna.nest.info",
        encode(&NestInfoRequest::default()),
    )
    .await
    .expect("nest.info ok");
    let reply: NestInfoReply = decode(&bytes).unwrap();
    assert_eq!(reply.software, "fauna");
    assert!(reply.protocols.iter().any(|p| p == "fauna"));
    assert_eq!(reply.nest_id.len(), 64, "nest_id is 32-byte hex");
    assert!(reply.nest_id.chars().all(|c| c.is_ascii_hexdigit()));
    // Anonymous version is coarsened to `major.minor` (anti-fingerprinting,
    // spec § 8.1) — exactly one dot, both parts numeric, no patch level.
    assert_coarsened_version(&reply.version);

    // Build/version capability set (`version-compatibility.md` § Dimension 3):
    // the four always-on families. The retired `pq-hybrid` and
    // `mail-epoch-schedule` tokens (2026-09-24 ruling,
    // `architecture/security/post-quantum.md` § Capability negotiation) must
    // not come back.
    use fauna_protocol::discovery::capability;
    for advertised in [
        capability::MAIL,
        capability::CALENDAR,
        capability::SUBSCRIPTIONS,
        capability::FILE_SYNC,
    ] {
        assert!(
            reply.capabilities.iter().any(|c| c == advertised),
            "nest.info advertises the capability {advertised:?}"
        );
    }
    for retired in ["pq-hybrid", "mail-epoch-schedule"] {
        assert!(
            !reply.capabilities.iter().any(|c| c == retired),
            "nest.info must not advertise the retired token {retired:?}"
        );
    }
}

/// `nest.info` must report the host the web `HostResolver` ACTUALLY routes on,
/// and must report it as the empty string on a box that serves no web content.
///
/// This is the nest half of the copy-a-dead-link defect
/// (`web-content-hosting.md` § Published-post management → *Implementation
/// status today*). Clients had no way to ask this question, so each invented an
/// answer — the address it dialed, or the sign-in reply's `domain`. The latter
/// is `handle_domain()`, which is this same chain **plus a `"localhost"` final
/// fallback**, and `<handle>.localhost` is exactly the host the resolver never
/// strips. So the distinction this asserts — empty, never `"localhost"` — is the
/// entire point of the field, not a detail of it.
#[tokio::test]
async fn nest_info_reports_the_web_serving_domain_and_leaves_it_empty_when_there_is_none() {
    let r = router();
    let st = state().await;
    // `AppState::for_test` has no identity domain, no registration handle
    // domain and no node domain — the domainless localhost/IP box.
    assert_eq!(
        st.handle_domain(),
        "localhost",
        "precondition: the SIGN-IN domain on a domainless box is the placeholder"
    );

    let bytes = dispatch(
        &r,
        Arc::clone(&st),
        "fauna.nest.info",
        encode(&NestInfoRequest::default()),
    )
    .await
    .expect("nest.info ok");
    let reply: NestInfoReply = decode(&bytes).unwrap();

    assert_eq!(
        reply.web_serving_domain.as_deref(),
        Some(""),
        "a domainless box serves NO web content, and must say so with an empty \
         string — reporting `localhost` here is what made every app compose \
         `<handle>.localhost`, a host this nest never answers on"
    );
    assert_eq!(
        reply.web_serving_domain.as_deref(),
        Some(st.web_serving_domain().as_str()),
        "the reported value must be the resolver's own accessor verbatim, so the \
         URL a client hands out and the host this nest answers on cannot drift"
    );
}

/// The anonymous discovery surface must expose only the `major.minor` line, not
/// the exact patch-level `CARGO_PKG_VERSION` (version-fingerprinting defense).
fn assert_coarsened_version(version: &str) {
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(
        parts.len(),
        2,
        "anonymous version must be coarsened to major.minor, got {version:?}"
    );
    assert!(
        parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "coarsened version parts must be numeric, got {version:?}"
    );
}

#[tokio::test]
async fn setup_status_reports_progress() {
    let r = router();
    let st = state().await;
    let bytes = dispatch(
        &r,
        st,
        "fauna.setup.status",
        encode(&SetupStatusRequest::default()),
    )
    .await
    .expect("setup.status ok");
    let reply: SetupStatusReply = decode(&bytes).unwrap();
    // `setup.status` is anonymous too — same coarsened-version rule.
    assert_coarsened_version(&reply.version);
    // The storage-mode axis is retired (`nest/storage-modes.md`); the constant
    // `mode` it once projected left the wire with the compat-remnant sweep.
    assert!(!reply.extra.contains_key("mode"));
    // No user created yet.
    assert!(!reply.admin_exists);
}

/// `email_enabled` must track the admin's `set_mail_enabled` toggle (the db
/// `mail_enabled` singleton), not a legacy in-nest-SMTP domain field. The old
/// derivation keyed on the boot-time `state.email.domain` (always `None` on the
/// Go-MTA build, since removed), which made `email_enabled` permanently `false`
/// in production even with mail operational — regression guard.
#[tokio::test]
async fn setup_status_email_enabled_tracks_mail_enabled_toggle() {
    async fn email_enabled(r: &RpcRouter, st: Arc<AppState>) -> bool {
        let bytes = dispatch(
            r,
            st,
            "fauna.setup.status",
            encode(&SetupStatusRequest::default()),
        )
        .await
        .expect("setup.status ok");
        decode::<SetupStatusReply>(&bytes).unwrap().email_enabled
    }

    let r = router();
    let st = state().await;

    // Default: no `mail_enabled` row → disabled.
    assert!(
        !email_enabled(&r, st.clone()).await,
        "mail disabled by default"
    );

    // Admin enables mail → the field flips true.
    st.db.set_mail_enabled(true).await.unwrap();
    assert!(
        email_enabled(&r, st.clone()).await,
        "enabled after set_mail_enabled(true)"
    );

    // …and back to disabled.
    st.db.set_mail_enabled(false).await.unwrap();
    assert!(
        !email_enabled(&r, st.clone()).await,
        "disabled after set_mail_enabled(false)"
    );
}

/// `mail_subsystem_ok` is the client-visible mail-health signal.
/// The outage was invisible: a `mail_domains` schema break crash-looped
/// the mail bridge (no mail ports) while nest still reported `health: ok`. The
/// nest-side proxy for that break is `list_active_mail_domains` failing — the very
/// query the bridge boots from. This asserts the signal: `true` when mail is
/// disabled (no bridge to brick) or the query is healthy, and `false` once the
/// query fails (here: dropping the column the SELECT reads — verbatim the
/// failure shape).
#[tokio::test]
async fn setup_status_mail_subsystem_ok_reflects_config_query_health() {
    async fn mail_ok(r: &RpcRouter, st: Arc<AppState>) -> bool {
        let bytes = dispatch(
            r,
            st,
            "fauna.setup.status",
            encode(&SetupStatusRequest::default()),
        )
        .await
        .expect("setup.status ok");
        decode::<SetupStatusReply>(&bytes)
            .unwrap()
            .mail_subsystem_ok
    }

    let r = router();
    let st = state().await;

    // Mail disabled → no bridge to brick → ok.
    assert!(mail_ok(&r, st.clone()).await, "ok when mail disabled");

    // Mail enabled, schema intact → the bridge's config query succeeds → ok.
    st.db.set_mail_enabled(true).await.unwrap();
    assert!(
        mail_ok(&r, st.clone()).await,
        "ok when mail enabled and schema intact"
    );

    // Reproduce the break: drop the column `list_active_mail_domains`
    // SELECTs (`dkim_selector_activated_at`). The query now errors "no such
    // column" — exactly the failure that crash-looped the live bridge — so the
    // signal must flip false (a *visible* unhealthy, not a silent brick).
    st.db
        .conn()
        .await
        .execute_batch("ALTER TABLE mail_domains DROP COLUMN dkim_selector_activated_at;")
        .expect("drop column to simulate schema break");
    assert!(
        !mail_ok(&r, st.clone()).await,
        "NOT ok when mail enabled but the config query fails (schema break)"
    );
}

/// `dkim_records` surfaces the nest's CURRENT DKIM public records on the
/// anonymous `setup.status` heartbeat (gate 6 / memory
/// `dkim-publish-goes-stale-silently`). This is the credential-less
/// surface the deploy-verify gate reads to compare the *published* DNS TXT
/// against the *live* signing key — DKIM has no DNS auto-reconcile, so a key
/// re-mint silently orphans the published record. Flow-traced end to end: a
/// `mail_dkim_keys` row (the authoritative `public_dns_value` of the key the
/// nest signs with) → `setup_status_core`'s `list_dkim_selectors` read →
/// handler map → wire. No key → empty (not an error).
#[tokio::test]
async fn setup_status_surfaces_provisioned_dkim_records() {
    async fn dkim_records(
        r: &RpcRouter,
        st: Arc<AppState>,
    ) -> Vec<fauna_protocol::discovery::DkimRecord> {
        let bytes = dispatch(
            r,
            st,
            "fauna.setup.status",
            encode(&SetupStatusRequest::default()),
        )
        .await
        .expect("setup.status ok");
        decode::<SetupStatusReply>(&bytes).unwrap().dkim_records
    }

    let r = router();
    let st = state().await;

    // No DKIM provisioned → empty list (a fresh nest, not an error).
    assert!(
        dkim_records(&r, st.clone()).await.is_empty(),
        "no records before provisioning"
    );

    // Mint a key — its public half is the TXT body to publish.
    fauna_nest::test_support::seat_deployment_seed(&st.db, &[0x5eu8; 32]).await;
    assert!(
        st.db
            .mint_mail_dkim_key("nest.example", "default")
            .await
            .unwrap()
    );
    let stored = st.db.list_dkim_selectors(None).await.unwrap();

    let recs = dkim_records(&r, st.clone()).await;
    assert_eq!(recs.len(), 1, "the record is surfaced");
    assert_eq!(recs[0].domain, "nest.example");
    assert_eq!(recs[0].selector, "default");
    assert_eq!(recs[0].public_dns_value, stored[0].public_dns_value);
    assert!(
        recs[0]
            .public_dns_value
            .starts_with("v=DKIM1; k=ed25519; p=")
    );
}

#[tokio::test]
async fn handle_available_for_unused_handle() {
    let r = router();
    let st = state().await;
    let bytes = dispatch(
        &r,
        st,
        "fauna.handle.available",
        encode(&HandleAvailableRequest {
            handle: "alice".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("handle.available ok");
    let reply: HandleAvailableReply = decode(&bytes).unwrap();
    assert!(reply.available);
    assert_eq!(reply.handle, "alice");
    assert!(!reply.cooldown);
}

#[tokio::test]
async fn handle_available_for_taken_handle() {
    let r = router();
    let st = state().await;
    let kp = ActorKeypair::generate();
    st.db
        .create_user_with_handle(&kp.actor_id().0, "free", "bob", None)
        .await
        .unwrap();
    let bytes = dispatch(
        &r,
        st,
        "fauna.handle.available",
        encode(&HandleAvailableRequest {
            handle: "bob".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("handle.available ok");
    let reply: HandleAvailableReply = decode(&bytes).unwrap();
    assert!(!reply.available, "an assigned handle is unavailable");
}

#[tokio::test]
async fn handle_available_rejects_invalid_handle() {
    let r = router();
    let st = state().await;
    let err = dispatch(
        &r,
        st,
        "fauna.handle.available",
        encode(&HandleAvailableRequest {
            handle: "ab".into(), // too short (< 3)
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("too-short handle is rejected");
    assert_eq!(err.code, "fauna.handle.invalid");
}

#[tokio::test]
async fn nest_resolve_rejects_internal_targets() {
    let r = router();
    let st = state().await;
    for domain in ["localhost", "1.2.3.4", "foo.local", "bar.internal"] {
        let err = dispatch(
            &r,
            st.clone(),
            "fauna.nest.resolve",
            encode(&NestResolveRequest {
                domain: domain.into(),
                extra: Default::default(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, "fauna.nest.invalid_domain",
            "{domain} should be rejected"
        );
    }
}

#[tokio::test]
async fn nest_resolve_returns_url_for_explicit_port() {
    let r = router();
    let st = state().await;
    // An explicit port short-circuits `resolve_full_url` before any SRV/DNS
    // lookup, keeping the test offline-deterministic.
    let bytes = dispatch(
        &r,
        st,
        "fauna.nest.resolve",
        encode(&NestResolveRequest {
            domain: "example.com:8443".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("nest.resolve ok");
    let reply: NestResolveReply = decode(&bytes).unwrap();
    assert_eq!(reply.url, "https://example.com:8443");
}

#[tokio::test]
async fn actor_by_handle_resolves_known_handle() {
    let r = router();
    let st = state().await;
    let kp = ActorKeypair::generate();
    st.db
        .create_user_with_handle(&kp.actor_id().0, "free", "carol", None)
        .await
        .unwrap();
    let bytes = dispatch(
        &r,
        st,
        "fauna.actor.by_handle",
        encode(&ActorByHandleRequest {
            handle: "carol".into(),
            domain: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("actor.by_handle ok");
    let reply: ActorByHandleReply = decode(&bytes).unwrap();
    assert_eq!(reply.actor_id, hex::encode(kp.actor_id().0));
    assert_eq!(reply.handle, "carol");
}

#[tokio::test]
async fn actor_by_handle_not_found_for_unknown_handle() {
    let r = router();
    let st = state().await;
    let err = dispatch(
        &r,
        st,
        "fauna.actor.by_handle",
        encode(&ActorByHandleRequest {
            handle: "nobody".into(),
            domain: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("unknown handle is not found");
    assert_eq!(err.code, "fauna.actor.not_found");
}
