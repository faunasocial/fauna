use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::{Signer, SigningKey};
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::discovery_core::{self, SetupStatus};
use fauna_nest::token_store::TokenStore;

/// Unique counter so each harness gets its own data dir.
static HARNESS_COUNTER: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> i64 {
    fauna_core::data::Timestamp::now_millis() as i64
}

fn random_signing_key() -> SigningKey {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut b = [0u8; 32];
    SystemRandom::new().fill(&mut b).unwrap();
    SigningKey::from_bytes(&b)
}

struct Harness {
    addr: std::net::SocketAddr,
    // Held so the harness data dir outlives every request (never read).
    _data_dir: tempfile::TempDir,
    db: Arc<CacheDb>,
    token_store: Arc<TokenStore>,
    client: reqwest::Client,
    /// The same `Arc<AppState>` the HTTP server holds — lets a test dispatch
    /// WS-RPC kinds (e.g. `fauna.posts.create`, whose HTTP twin was deleted in
    /// T4) in-process and observe storage-mode
    /// commits made over HTTP (the `storage` `RwLock` is shared).
    state: Arc<fauna_nest::routes::AppState>,
}

/// Spin up a nest with a real on-disk DB inside a temp dir (so a test can assert
/// the `{data-dir}/storage-mode` marker is NEVER written), unclaimed (claim-code
/// file present). There is no storage mode to arrange: the axis is retired and
/// the box is sealed at rest from boot.
async fn harness() -> Harness {
    harness_with_domain(None).await
}

/// `harness()` with a configured `node.domain` — exercises the
/// `config.nest.domain → setup_status_core().domain` plumbing the deleted
/// `GET /api/v1/setup-status` CI smoke used to assert.
async fn harness_with_domain(domain: Option<String>) -> Harness {
    let _ = HARNESS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let data_dir = tempfile::tempdir().unwrap();
    let db_path = data_dir.path().join("nest.db");
    std::fs::write(data_dir.path().join("claim-code"), "ABCDEF").unwrap();

    let db = Arc::new(CacheDb::open(&db_path).unwrap());
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, data_dir.path().to_path_buf(), None).unwrap(),
    );
    let token_store = Arc::new(TokenStore::new());

    // Build the config with the real db_path so storage_mode_marker_path
    // resolves to the temp dir (for_test() sets db_path to "" which would
    // fall back to /data).
    let config = Arc::new(fauna_nest::config::NestConfig {
        nest: fauna_nest::config::NestSection {
            mode: fauna_nest::config::NodeMode::Public,
            listen: "127.0.0.1:0".into(),
            db_path: db_path.to_string_lossy().into_owned(),
            domain,
            ..Default::default()
        },
        bridges: None,
        submission: None,
        acme: None,
        email: None,
        update: Default::default(),
    });

    let nest_identity = Arc::new(fauna_nest::nest_identity::NestIdentity::from_seed(
        &[1u8; 32],
    ));
    let security_notifier = Arc::new(fauna_nest::security_notify::SecurityNotifier::new(
        db.clone(),
        Arc::new(fauna_nest::nest_identity::NestIdentity::from_seed(
            &[2u8; 32],
        )),
    ));

    let state = fauna_nest::routes::AppState {
        backup_service: Some(backup_svc),
        config,
        nest_identity,
        security_notifier,
        auth: fauna_nest::state::AuthState {
            token_store: token_store.clone(),
            ..Default::default()
        },
        // No storage override: `for_test` installs the one impl there is
        // (`SealedStorage`), which is exactly what production boots with — there
        // is no pre-commit state to align with any more.
        ..fauna_nest::routes::AppState::for_test(db.clone())
    };

    let state = Arc::new(state);
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    Harness {
        addr,
        _data_dir: data_dir,
        db,
        token_store,
        client: reqwest::Client::new(),
        state,
    }
}

/// Claim admin **in-process** through the transport-agnostic core
/// (`claim_admin_core`) so an admin actor exists and the claim-code file is
/// deleted. Returns the admin's signing key. Replaces the deprecated
/// `POST /api/v1/claim-admin` twin (removed in S4d): the route's ceremony lived
/// entirely in the core, and the live WS path (`fauna.auth.claim_admin`) is
/// exercised end-to-end in `onboarding_ws_nest_api_roundtrip.rs`.
async fn claim_admin(h: &Harness) -> SigningKey {
    let (sk, _token) = claim_admin_with_token(h).await;
    sk
}

/// `claim_admin` that also returns the bearer token the core mints — used by the
/// blob / search / CalDAV tests below that still drive HTTP routes with it.
async fn claim_admin_with_token(h: &Harness) -> (SigningKey, String) {
    use fauna_nest::claim_core::claim_admin_core;

    let sk = random_signing_key();
    let actor_hex = hex::encode(sk.verifying_key().to_bytes());
    let ts = now_ms();
    // claim-admin signs Ed25519 over the tagged `claim_admin_signed_message`;
    // the core accepts ms timestamps.
    let msg = fauna_protocol::claim::claim_admin_signed_message(
        &sk.verifying_key().to_bytes(),
        ts as u64,
    );
    let sig = hex::encode(sk.sign(&msg).to_bytes());

    // A handle is required to claim (the admin's handle is its identity); pass
    // one so the test harness gets a real, routable admin.
    let outcome = claim_admin_core(
        &h.state, &actor_hex, ts as u64, &sig, "ABCDEF", "admin", None,
    )
    .await
    .expect("claim-admin should succeed");
    (sk, outcome.token)
}

/// Claim through the core with an explicit `mail_domain`, returning the resulting
/// active mail-domain names. Mirrors `claim_admin_with_token`'s ceremony.
async fn claim_with_mail_domain(h: &Harness, mail_domain: Option<&str>) -> Vec<String> {
    let sk = random_signing_key();
    let actor_hex = hex::encode(sk.verifying_key().to_bytes());
    let ts = now_ms();
    let msg = fauna_protocol::claim::claim_admin_signed_message(
        &sk.verifying_key().to_bytes(),
        ts as u64,
    );
    let sig = hex::encode(sk.sign(&msg).to_bytes());
    fauna_nest::claim_core::claim_admin_core(
        &h.state,
        &actor_hex,
        ts as u64,
        &sig,
        "ABCDEF",
        "admin",
        mail_domain,
    )
    .await
    .expect("claim-admin should succeed");
    h.state
        .db
        .list_active_mail_domains()
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.domain_name)
        .collect()
}

/// Regression for the home2 symptom: claiming a **domainless** nest reached at a
/// **local target** (IP literal / `localhost` / `.local` / `host:port`) must NOT
/// auto-register that access address as the primary mail domain — `mail.<ip>` is
/// nonsense that breaks mail/CalDAV host-routing
/// (`domains-and-tls-bootstrap.md` § Claim). The admin claims handle-only and adds
/// a real domain later from a client.
#[tokio::test]
async fn claim_at_local_target_does_not_register_a_mail_domain() {
    for local in [
        "10.1.8.51",
        "localhost",
        "pi.local",
        "192.168.1.4:3000",
        "::1",
    ] {
        let h = harness().await;
        let domains = claim_with_mail_domain(&h, Some(local)).await;
        assert!(
            domains.is_empty(),
            "claiming at local target {local:?} must not auto-register a mail domain, got {domains:?}"
        );
    }
}

/// Positive control: a real registerable domain carried by the claim handle IS
/// auto-registered as the primary mail domain — the unchanged out-of-the-box path.
#[tokio::test]
async fn claim_with_real_domain_registers_it() {
    let h = harness().await;
    let domains = claim_with_mail_domain(&h, Some("example.com")).await;
    assert_eq!(
        domains,
        vec!["example.com".to_string()],
        "a real domain is auto-registered at claim"
    );
}

/// The other half of the local-target story, and the one the published guides
/// have to respect: **adding a real domain to a domainless box is a ONE-WAY
/// DOOR.**
///
/// A home nest claimed at its LAN address registers no mail domain (the test
/// above) — that is the goal doc's intended shape, not a gap
/// (`domains-and-tls-bootstrap.md` § Claim). The obvious follow-through is
/// "add your real domain later, from the app"
/// (§ Add domains after install). What is *not* obvious, and is what this test
/// pins, is that the step cannot be taken back from the app:
///
/// 1. the first domain added becomes the **primary** — `add_local_domain`
///    auto-determines `is_primary` from "no active rows yet"
///    (`bridge_routing_handlers.rs`, § First domain claimed is the deployment's
///    primary), and the primary `mail_domains` row **is** the deployment
///    identity (`apply_primary_identity` fires for a public apex), so
///    `handle_domain()` / discovery / the cert SAN graph all follow it; and
/// 2. the primary can never be removed — `soft_delete_mail_domain` refuses it
///    (`403 cannot_remove_primary_domain`, `mail-multidomain.md` § Removing a
///    local domain).
///
/// There is no revert-to-domainless anywhere: the only other exit is the
/// primary-**rename** ceremony, which is explicitly "gated on the primary
/// existing and being non-local, so an IP/domainless box is left untouched"
/// (`mail-primary-domain-rename.md` § Crash recovery) and in any case only
/// moves the identity to *another* real domain.
///
/// **Red-verified 2026-08-22, not merely green:** with the `is_primary` guard in
/// `soft_delete_mail_domain` temporarily disabled, this test FAILS at the
/// `expect_err` below — so it genuinely observes the guard rather than passing
/// for an unrelated reason. Restore that mutation if you ever need to re-check.
///
/// This is not a wedge — the box keeps working, and renaming onto the right
/// domain is a real client-side exit, so the client-state-recoverability
/// invariant holds. It is a one-way door, which is why
/// `deployment-home-with-public-relay.md` § MUA reach now says a LAN-only home
/// box should be left domainless (and never given the apex, whose A record
/// points at the *internet* nest), and why the published home/relay guides do
/// not casually invite the user through it.
#[tokio::test]
async fn adding_a_real_domain_to_a_domainless_box_is_a_one_way_door() {
    let h = harness().await;

    // A home box: claimed at its LAN address, so it starts domainless.
    let domains = claim_with_mail_domain(&h, Some("192.168.1.50")).await;
    assert!(
        domains.is_empty(),
        "precondition: a LAN-target claim leaves the box domainless, got {domains:?}"
    );

    // The user adds their real domain from the app. `add_local_domain` derives
    // `is_primary` from "no active rows yet", which is exactly this case.
    let is_primary = h
        .state
        .db
        .list_active_mail_domains()
        .await
        .unwrap()
        .is_empty();
    assert!(
        is_primary,
        "the first domain added to a domainless box is the primary"
    );
    h.state
        .db
        .add_mail_domain(
            "example.com",
            is_primary,
            "testing",
            "expand_primary",
            None,
            None,
        )
        .await
        .expect("adding a first real domain to a domainless box succeeds");

    // It is now the deployment identity...
    let primary = h
        .state
        .db
        .lookup_primary_mail_domain()
        .await
        .unwrap()
        .expect("the added domain became the primary");
    assert_eq!(
        primary.domain_name, "example.com",
        "the first added domain becomes the primary — i.e. the deployment identity"
    );

    // ...and the door has shut: the app cannot take it back off.
    let err = h
        .state
        .db
        .soft_delete_mail_domain("example.com")
        .await
        .expect_err(
            "removing the primary must be refused — if this ever starts succeeding, the \
             one-way door is gone and the home/relay guides can stop warning about it",
        );
    assert!(
        err.to_string().contains("cannot remove primary domain"),
        "the refusal must be the primary-domain rule (surfaced to clients as \
         `cannot_remove_primary_domain`), got: {err}"
    );

    // And the box is still domain-bearing: there is no revert-to-domainless.
    let after: Vec<String> = h
        .state
        .db
        .list_active_mail_domains()
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.domain_name)
        .collect();
    assert_eq!(
        after,
        vec!["example.com".to_string()],
        "the domain is still there after the refused removal — a domainless box cannot be \
         restored from the app"
    );
}

/// Read setup-wizard progress in-process (the deleted `GET /api/v1/setup-status`
/// twin). `discovery_core::setup_status_core` is the shared read both the old
/// HTTP route and the live `fauna.setup.status` WS kind call.
async fn setup_status(h: &Harness) -> SetupStatus {
    discovery_core::setup_status_core(&h.state, true).await
}

/// The deleted `GET /api/v1/setup-status` CI smoke asserted the configured
/// domain surfaced in the wizard heartbeat (`grep smoke-test.fauna.local`).
/// That plumbing now has only the `fauna.setup.status` WS transport, both
/// reading `discovery_core::setup_status_core`; assert it here in-process.
#[tokio::test]
async fn setup_status_reflects_configured_domain() {
    let h = harness_with_domain(Some("smoke-test.fauna.local".to_string())).await;
    let ss = setup_status(&h).await;
    assert_eq!(ss.domain, "smoke-test.fauna.local");
}

/// **Works out of the box.** A fresh nest with no mode ever committed accepts
/// content immediately: the pre-mode 409 gate (`require_storage_mode_resolved`)
/// is deleted along with the unresolved state it guarded
/// (`nest/storage-modes.md` § Boot story). The blob route still rejects this
/// body — but for its *shape* (no multipart upload sidecar → 400), never for a
/// missing deployment posture.
#[tokio::test]
async fn content_routes_are_open_from_first_boot() {
    let h = harness().await; // no mode committed, none committable
    let sk = random_signing_key();
    h.db.create_user(&sk.verifying_key().to_bytes(), "free", "test")
        .await
        .unwrap();
    let token = h
        .token_store
        .insert(
            fauna_core::identity::ActorId(sk.verifying_key().to_bytes()),
            3600,
        )
        .await;

    let resp = h
        .client
        .post(format!("http://{}/api/v1/blob", h.addr))
        .header("authorization", format!("Bearer {token}"))
        .body(vec![1u8, 2, 3])
        .send()
        .await
        .unwrap();

    assert_ne!(
        resp.status(),
        409,
        "no storage-mode gate may stand between a fresh nest and its content routes"
    );
    assert_eq!(
        resp.status(),
        400,
        "rejected on shape (sidecar absent), not posture"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap();
    assert!(
        !err.contains("storage mode"),
        "the rejection must not mention a storage mode: {err}"
    );
    assert!(
        err.contains("multipart/form-data"),
        "expected the sidecar-shape rejection, got: {err}"
    );
}

/// `fauna.posts.create` stores the content row and writes **no classifier
/// labels**: the nest is not a scoring position, on any box
/// (`content-scoring.md` § The placement matrix — a scorer runs only where a
/// capability for its inputs is held). No mode is committed anywhere in this
/// test, and none can be.
///
/// It then asserts the other half of the collapse: server-side search **answers
/// and finds the post**. The `not_server_side` refusal retired with the axis —
/// `content_fts` is fed only by floor/plaintext-by-design data (the post
/// projection's `Post::body_text()`, which is the *public preview* for a gated
/// post, plus profile handles), so serving it reads nothing sealed and every box
/// answers.
#[tokio::test]
async fn post_stores_row_without_labels_and_is_searchable() {
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    use fauna_core::identity::ActorKeypair;
    use fauna_nest::{posts_handlers, rpc_router::RpcRouter};
    use fauna_protocol::{
        decode_strict as decode, encode_canonical,
        posts::{PostCreateReply, PostCreateRequest},
    };

    let h = harness().await;
    let _sk = claim_admin(&h).await;

    // Strict ingest (`ingest_post_core` → `Storage::ingest_post`) requires a
    // BARE-decodable, signature-verified Post wrapped in the Layer 2
    // EmbedAsBytes wire shape. A regular user authors + submits it (the
    // connection actor must be a permitted caller for `fauna.posts.create`).
    let kp = ActorKeypair::generate();
    h.db.create_user(&kp.actor_id().0, "free", "poster")
        .await
        .unwrap();
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: "hello sealed world".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let post_body = sign_and_pack(&kp, &post).unwrap();

    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    let router = b.build();
    let req = PostCreateRequest {
        body: serde_bytes::ByteBuf::from(post_body),
        extra: std::collections::BTreeMap::new(),
    };
    let payload = bytes::Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let meta = router
        .kind_meta("fauna.posts.create")
        .expect("fauna.posts.create registered");
    let reply_bytes = (meta.handler)(h.state.clone(), kp.actor_id().0, payload)
        .await
        .expect("post stored");
    let reply: PostCreateReply = decode(&reply_bytes).unwrap();
    let post_id_hex = reply.post_id;

    // No content_labels rows: the nest classifies nothing at ingest.
    let labels = h.db.get_content_labels("post", &post_id_hex).await.unwrap();
    assert!(
        labels.is_empty(),
        "the nest must write no classifier labels at ingest, got {labels:?}"
    );

    // ...and yet search answers, and finds the post's floor-derived body text.
    let reply = dispatch_search(&h, &kp.actor_id().0, "sealed")
        .await
        .expect("server-side search must answer on every box");
    assert!(
        !reply.results.is_empty(),
        "the post's public body text is floor data and must be searchable"
    );
}

/// Dispatch `fauna.search.query` in-process against the harness's shared
/// `AppState`. Mirrors the post-create dispatch above.
async fn dispatch_search(
    h: &Harness,
    actor_id: &[u8; 32],
    query: &str,
) -> Result<fauna_protocol::search::SearchQueryReply, fauna_protocol::RpcError> {
    use fauna_nest::{rpc_router::RpcRouter, search_handlers};
    use fauna_protocol::{decode_strict as decode, encode_canonical, search::SearchQueryRequest};

    let mut b = RpcRouter::builder();
    search_handlers::register_search_handlers(&mut b);
    let router = b.build();
    let req = SearchQueryRequest {
        query: query.into(),
        content_type: None,
        before: None,
        after: None,
        limit: None,
        offset: None,
        extra: std::collections::BTreeMap::new(),
    };
    let payload = bytes::Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let meta = router
        .kind_meta("fauna.search.query")
        .expect("fauna.search.query registered");
    let reply_bytes = (meta.handler)(h.state.clone(), *actor_id, payload).await?;
    Ok(decode(&reply_bytes).expect("decode SearchQueryReply"))
}

/// A fresh nest's DAV apex no longer `503`s for want of a storage mode — that
/// gate died with the axis. It answers on the real liveness predicate alone (is
/// the MDA actually serving?), which with no mail domain configured is still a
/// `503`, but for the honest reason.
#[tokio::test]
async fn dav_apex_503_on_a_fresh_nest_is_about_mail_not_a_storage_mode() {
    let h = harness().await;
    for path in [
        ".well-known/caldav",
        ".well-known/carddav",
        ".well-known/webdav",
    ] {
        let resp = h
            .client
            .request(
                reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                format!("http://{}/{path}", h.addr),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 503, "{path}: no MDA host to point at");
        let body = resp.text().await.unwrap();
        assert!(
            !body.contains("storage mode"),
            "{path}: the 503 must not blame a storage mode: {body}"
        );
    }
}

/// The apex gates on the predicate that starts the listener, nothing else: with
/// no primary mail domain there is no MDA host to point at, so it `503`s. The
/// MDA is the only CalDAV server (`caldav-server.md` § Independent enablement) —
/// the in-core `/caldav/{actor}/` routes were retired in the Decision-B § 4c
/// cleanup, and nest's catch-all `.fallback(web_content_or_info)` answers every
/// unmatched path with a `200` info page, which is why a status assertion here
/// must only ever be made against a route confirmed to be registered.
#[tokio::test]
async fn caldav_apex_503_without_mail() {
    let h = harness().await;
    let _sk = claim_admin(&h).await;

    let resp = h
        .client
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            format!("http://{}/.well-known/caldav", h.addr),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "apex CalDAV discovery must 503 with no primary mail domain (no MDA host \
         to redirect to)"
    );
}

/// A mail-enabled box `301`s the apex to the MDA host. (The retired plaintext
/// mode is the arm that used to serve a `207` naming an in-core collection the
/// nest no longer has — that fork is gone with the axis.)
#[tokio::test]
async fn caldav_apex_301s_to_mail_host() {
    let h = harness().await;
    let _sk = claim_admin(&h).await;
    h.state
        .db
        .add_mail_domain("fauna.test", true, "testing", "self_signed", None, None)
        .await
        .expect("register primary mail domain");
    // Stage-5 default-off: "mail-enabled" means the explicit toggle — a
    // registered domain alone no longer implies enablement (the apex 503s on
    // unset, `dav_apexes_503_when_domain_registered_but_toggles_unset`).
    h.state
        .db
        .set_mail_enabled(true)
        .await
        .expect("enable mail explicitly");

    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = no_redirect
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            format!("http://{}/.well-known/caldav", h.addr),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        301,
        "a mail-enabled box must 301 the apex to the MDA host"
    );
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "https://mail.fauna.test/.well-known/caldav",
        "redirect Location must point at mail.<primary_domain>"
    );
}

/// The apex answers iff the MDA actually serves CalDAV: an explicit
/// `set_caldav_enabled(false)` `503`s even on a mail-enabled box. (An *unset*
/// toggle inherits `mail_enabled` — `caldav-server.md` § Independent enablement
/// — which `caldav_apex_301s_to_mail_host` covers.)
#[tokio::test]
async fn caldav_apex_503_when_caldav_explicitly_disabled() {
    let h = harness().await;
    let _sk = claim_admin(&h).await;
    h.state
        .db
        .add_mail_domain("fauna.test", true, "testing", "self_signed", None, None)
        .await
        .expect("register primary mail domain");
    h.state.db.set_caldav_enabled(false).await.unwrap();

    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = no_redirect
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            format!("http://{}/.well-known/caldav", h.addr),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "explicitly-disabled CalDAV must 503 the apex even with mail enabled"
    );
}

// (`encrypted_nest_caldav_apex_redirects_to_mail_host` lived here — the RFC-6764
// cross-host bootstrap `301`. Subsumed by
// `caldav_apex_301s_to_mail_host` above, which now makes the same
// assertion for BOTH storage modes, the apex having lost its mode branch.)

// ─── CardDAV apex discovery (/.well-known/carddav) — gated on carddav_enabled ──

#[tokio::test]
async fn carddav_apex_503_when_carddav_explicitly_disabled() {
    let h = harness().await;
    let _sk = claim_admin(&h).await;
    h.state
        .db
        .add_mail_domain("fauna.test", true, "testing", "self_signed", None, None)
        .await
        .expect("register primary mail domain");
    h.state.db.set_carddav_enabled(false).await.unwrap();

    let resp = h
        .client
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            format!("http://{}/.well-known/carddav", h.addr),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "explicitly-disabled CardDAV must 503 the apex even with mail enabled"
    );
}

/// The regression this fix is about: an **unset** `carddav_enabled` on a
/// mail-enabled box means the MDA serves CardDAV, so the apex must redirect —
/// not `503`.
#[tokio::test]
async fn carddav_apex_301s_when_toggle_unset_and_mail_enabled() {
    let h = harness().await;
    let _sk = claim_admin(&h).await;
    h.state
        .db
        .add_mail_domain("fauna.test", true, "testing", "self_signed", None, None)
        .await
        .expect("register primary mail domain");
    // Mail explicitly enabled (Stage-5 default-off: unset would read OFF);
    // `carddav_enabled` deliberately left UNSET — it inherits `mail_enabled`.
    h.state
        .db
        .set_mail_enabled(true)
        .await
        .expect("enable mail explicitly");

    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = no_redirect
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            format!("http://{}/.well-known/carddav", h.addr),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        301,
        "an unset carddav_enabled inherits mail_enabled — the MDA serves CardDAV, \
         so the apex must not 503"
    );
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "https://mail.fauna.test/.well-known/carddav",
    );
}

#[tokio::test]
async fn enabled_carddav_apex_redirects_to_mail_host() {
    // Encrypted mode + a registered primary mail domain + CardDAV enabled: the
    // apex /.well-known/carddav 301-redirects to the MDA host (RFC 6764 cross-host
    // bootstrap), the CardDAV twin of the CalDAV apex redirect.
    let h = harness().await;
    let (_sk, _token) = claim_admin_with_token(&h).await;
    h.state
        .db
        .add_mail_domain("fauna.test", true, "testing", "self_signed", None, None)
        .await
        .expect("register primary mail domain");
    h.state
        .db
        .set_carddav_enabled(true)
        .await
        .expect("enable CardDAV");

    // A no-redirect client so we observe the 301 itself (a following client would
    // chase it to mail.fauna.test and fail to connect).
    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = no_redirect
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            format!("http://{}/.well-known/carddav", h.addr),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        301,
        "encrypted + mail-enabled + carddav-enabled apex must 301 to the MDA host"
    );
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "https://mail.fauna.test/.well-known/carddav",
        "redirect Location must point at mail.<primary_domain>/.well-known/carddav"
    );
}

/// The Stage-5 default-off pin for the apex family: a registered primary mail
/// domain ALONE — every enable toggle unset — must 503 all three DAV apexes.
/// Claim auto-registers the handle domain as the primary mail domain before any
/// enable, so "domain registered" is not evidence of enablement; pre-flip the
/// apexes treated it as exactly that (the retired defaults-ON deferral), which
/// advertised DAV surfaces on a nest whose MDA never runs.
#[tokio::test]
async fn dav_apexes_503_when_domain_registered_but_toggles_unset() {
    let h = harness().await;
    let (_sk, _token) = claim_admin_with_token(&h).await;
    h.state
        .db
        .add_mail_domain("fauna.test", true, "testing", "self_signed", None, None)
        .await
        .expect("register primary mail domain");
    // Deliberately NO set_mail_enabled / set_*dav_enabled — the fresh-claim
    // state, post-flip.
    for path in [
        ".well-known/caldav",
        ".well-known/carddav",
        ".well-known/webdav",
    ] {
        let resp = h
            .client
            .request(
                reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                format!("http://{}/{path}", h.addr),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            503,
            "{path}: unset toggles must read OFF — a registered mail domain \
             alone is not enablement (Stage-5 default-off)"
        );
    }
}

// ─── WebDAV apex discovery (/.well-known/webdav) — defaults-ON, no SRV ─────────

#[tokio::test]
async fn webdav_wellknown_503_when_explicitly_disabled() {
    // Unlike CardDAV (which stays 503 until an explicit enable), WebDAV is
    // defaults-ON — so the ONLY thing that keeps a mail-enabled box's apex at 503
    // is an explicit `set_webdav_enabled(false)`.
    let h = harness().await;
    let (_sk, _token) = claim_admin_with_token(&h).await;
    h.state
        .db
        .add_mail_domain("fauna.test", true, "testing", "self_signed", None, None)
        .await
        .expect("register primary mail domain");
    h.state
        .db
        .set_webdav_enabled(false)
        .await
        .expect("disable WebDAV");

    let resp = h
        .client
        .get(format!("http://{}/.well-known/webdav", h.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "WebDAV apex must 503 when webdav_enabled is explicitly false"
    );
}

#[tokio::test]
async fn webdav_apex_redirects_to_collection_root_defaults_on() {
    // Encrypted mode + a registered primary mail domain + mail EXPLICITLY
    // enabled, with webdav_enabled UNSET: the follows-mail fallback
    // (unwrap_or(mail_enabled)) makes the apex 301 to the MDA's WebDAV
    // collection root WITHOUT an explicit WebDAV toggle — and it points
    // straight at /webdav/ (a NextCloud convention, not the RFC-6764 bridge
    // well-known). Stage-5 default-off: the mail enable itself must be
    // explicit — unset mail would 503 every DAV apex
    // (`dav_apexes_503_when_domain_registered_but_toggles_unset`).
    let h = harness().await;
    let (_sk, _token) = claim_admin_with_token(&h).await;
    h.state
        .db
        .add_mail_domain("fauna.test", true, "testing", "self_signed", None, None)
        .await
        .expect("register primary mail domain");
    h.state
        .db
        .set_mail_enabled(true)
        .await
        .expect("enable mail explicitly");

    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = no_redirect
        .get(format!("http://{}/.well-known/webdav", h.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        301,
        "mail-enabled encrypted box must 301 the WebDAV apex without an explicit enable (defaults-on)"
    );
    assert_eq!(
        resp.headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "https://mail.fauna.test/webdav/",
        "redirect Location must point straight at mail.<primary_domain>/webdav/"
    );
}

// (`plaintext_nest_caldav_unchanged` lived here. It pinned the plaintext apex to
// a `207` naming an in-core discovery collection, plus a `207` from
// `PROPFIND /caldav/{actor}/`. Both belong to the in-core CalDAV store retired in
// the Decision-B § 4c cleanup — the MDA is the only CalDAV server in either mode
// — so the apex is now mode-independent and the pins live in
// `caldav_apex_503_without_mail` +
// `caldav_apex_301s_to_mail_host` above.)
