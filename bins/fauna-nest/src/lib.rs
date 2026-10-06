// Crate-level clippy allows. fauna-nest is a large server crate whose shape
// triggers a few heuristic lints structurally rather than on genuinely tangled
// code:
//   - too_many_arguments: SQLite write helpers in `db::*` take one parameter
//     per column; folding them into params structs just relocates the column
//     list. axum handlers similarly thread state + several path/query args.
//   - type_complexity: `query_row` / `query_map` result tuples like
//     `Option<(Vec<u8>, Option<Vec<u8>>, i64)>` are clearer inline than behind
//     a single-use `type` alias.
//   - result_large_err: the route-helper pattern `Result<T, axum::Response>`
//     uses the response itself as the error; boxing it would force every `?`
//     call site to remap, for no real benefit.
// A genuinely unwieldy signature/type that shows up here should still be
// refactored — these allows are for the structural cases above only.
#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::result_large_err
)]

pub mod abuse_report_federation;
pub mod account_core;
pub mod account_handlers;
pub mod acme;
pub mod acme_http01;
#[cfg(feature = "activitypub")]
pub mod activitypub;
pub mod admin;
pub mod admin_export_routes;
pub mod admin_ws_handlers;
pub mod age_attest;
pub mod anonymous_rate_limit;
pub mod api_error;
pub mod atproto_authoring_key;
pub mod atproto_blob_sweeper;
#[cfg(feature = "test-hooks")]
pub mod atproto_identity_test_hook;
pub mod auth;
pub mod auth_core;
pub mod auth_handlers;
pub mod backup;
pub mod backup_handlers;
pub mod blob_routes;
pub mod blob_store;
#[cfg(feature = "bluesky")]
pub mod bluesky;
#[cfg(test)]
mod bridge_approval_test_support;
pub mod bridge_atproto_handlers;
pub mod bridge_blob_handlers;
pub mod bridge_caldav_handlers;
pub mod bridge_carddav_handlers;
pub mod bridge_dm_gate;
pub mod bridge_export_handlers;
pub mod bridge_imap_handlers;
pub mod bridge_import_handlers;
pub mod bridge_legs;
pub mod bridge_list_handlers;
pub mod bridge_management;
pub mod bridge_method_allowlist;
pub mod bridge_push_registry;
pub mod bridge_rate_limit;
pub mod bridge_routing_handlers;
#[cfg(any(feature = "nostr", feature = "bluesky", feature = "activitypub"))]
pub mod bridge_schema;
#[cfg(feature = "test-hooks")]
pub mod bridge_status_test_hook;
pub mod bridge_withdraw;
pub mod bridged_conversation_handlers;
pub mod bridges_ui_handlers;
pub mod bulk_byte_token;
pub mod caldav_bridge;
pub mod carddav_bridge;
pub mod cert_nudge;
pub mod challenge_auth;
#[cfg(feature = "test-hooks")]
pub mod channel_refusal_test_hook;
pub mod chunk_relay;
pub mod chunk_routes;
pub mod rpc_hold_test_hook;
// Moving a sealed mail body across the bulk-byte plane when it is too large for the
// 2 MiB RPC frame (smtp-server.md § Message size limits): resolve an upward
// reference the MTA staged, stage a stored body downward for the MDA to GET back.
pub mod build_identity;
pub mod claim;
pub mod claim_core;
pub mod claim_handlers;
pub mod config;
pub mod contacts_handlers;
pub mod content_index_handlers;
pub mod conversations_handlers;
pub mod cursor_seal;
pub mod custody_admission;
pub mod custody_hosting_handlers;
pub mod custody_hosting_test_hook;
pub mod custody_hosting_worker;
pub mod custody_receipt_handlers;
pub mod db;
pub mod degraded_serve;
pub mod delegation_handlers;
#[cfg(feature = "test-hooks")]
pub mod delegation_lease_test_hook;
pub mod delegation_registry;
pub mod delegation_runner;
pub mod deployment_key;
pub mod desktop_serve;
pub mod discovery;
pub mod discovery_core;
pub mod discovery_handlers;
pub mod dispatch_core;
pub mod dns_handlers;
pub mod dns_verifier;
pub mod domain_expiry;
pub mod domain_expiry_handlers;
#[cfg(feature = "test-hooks")]
pub mod domain_expiry_test_hook;
pub mod domain_hash;
pub mod drafts_handlers;
pub mod email_handlers;
pub mod factory_reset;
pub mod mail_body_plane;
pub mod webdav_bridge;
pub mod webdav_principal_admission;
pub mod well_known_dav;
// `email_filter_routes` deleted alongside `email_routes` in the T9+T10
// bridges/email WS-RPC sweep — clients use `fauna.email.filters.*` +
// `fauna.email.send` via `fauna-client-email::EmailClient`.
pub mod eviction;
pub mod exchange_originator;
pub mod export;
pub mod export_routes;
pub mod failed_credential_throttle;
pub mod family_handlers;
// The controversial-class feature gate's nest-side enforcement floor
// (`dynamic-features.md` § Evaluation points). Deliberately NOT gated on the
// `payments` cargo feature: the plane gates three registry members and is the
// one place the subset edge is discharged, so an excised-payments nest still
// compiles and runs it for whatever members it does ship.
pub mod events_doors;
pub mod events_webhook;
pub mod feature_gate;
pub mod federation_channel;
pub mod federation_handlers;
pub mod federation_pool;
pub mod federation_router;
pub mod federation_sig;
pub mod feed_handlers;
pub mod feed_routes;
pub mod folder_authz;
pub mod folder_deposit;
pub mod folder_handlers;
pub mod folder_public;
// folder_routes / user_folder_routes / lease_routes deleted in the
// WS-RPC-everywhere rip-out — user + admin folders, members, schedule,
// devices, sync conflicts and upload leases are now solely the
// `fauna.folders.*` / `fauna.sync.conflicts.*` / `fauna.admin.folders.*`
// WS-RPC kinds (folder_handlers / admin_ws_handlers).
#[cfg(feature = "test-hooks")]
pub mod content_seal_epoch_test_hook;
#[cfg(feature = "test-hooks")]
pub mod content_seal_test_hook;
pub mod files_handlers;
pub mod filesync_handlers;
pub mod generation_escrow_handlers;
pub mod host_maintenance;
pub mod http_range;
pub mod identity_domain_core;
pub mod inbox_handlers;
pub mod interact_routes;
pub mod invite_core;
pub mod invite_handlers;
pub mod label_handlers;
pub mod labeler_handlers;
pub(crate) mod lan_cert;
pub mod link_preview;
pub mod link_preview_handlers;
#[cfg(feature = "test-hooks")]
pub mod link_preview_test_hook;
pub mod log_plane;
pub mod mail_deliverability;
pub mod mail_dkim_key;
pub mod mail_enable;
pub mod mail_export_blobs;
#[cfg(feature = "test-hooks")]
pub mod mass_mailing_test_hook;
pub mod media_handlers;
pub mod media_proxy_routes;
pub mod media_ticket;
pub mod media_ticket_secret;
pub mod membership_lapse;
pub mod mls_replica_handlers;
pub mod mode_commit;
pub mod moderation_handlers;
pub mod moderation_withhold;
pub mod mta_sts_advance;
#[cfg(feature = "test-hooks")]
pub mod mta_sts_test_hook;
pub mod nat_mode_core;
pub mod nat_mode_handlers;
pub mod nest_identity;
/// The nest-internal key-encryption key family — its context strings, the one
/// walk that re-keys every member at deployment-seed rotation, and the one boot
/// step that seats its single-row members.
pub mod nest_kek;
pub mod nest_link;
pub mod nest_sync_worker;
pub mod node_policy_core;
pub mod node_policy_handlers;
#[cfg(feature = "nostr")]
pub mod nostr;
pub mod notifications_handlers;
// The nest-held OAuth issuer signing key set (TP5) — custody, rotation, the
// JWKS read and the admin surface over them. All three deliberately un-gated:
// the authorization server is up whenever the nest is up.
/// The consent ceremony's memory — the browser flow, the authorization code it
/// releases, and the wake-ups that make a resolution immediate.
pub mod oauth_as_ceremony;
/// Client-identity resolution — the guarded `client_id` fetch and the cache
/// over it.
pub mod oauth_as_client;
/// The refusal vocabulary — the RFC error codes, their statuses, their body.
pub mod oauth_as_error;
/// The DPoP and `private_key_jwt` gates every AS endpoint runs a caller through.
pub mod oauth_as_gates;
/// The OIDC layer (TP6) — the ID token's claims and `/oauth/userinfo`.
pub mod oauth_as_oidc;
/// The permission-set request call — `/oauth/par` resolving an
/// `include:<NSID>` through the PDS bridge's chain.
pub mod oauth_as_permission_sets;
/// The per-endpoint request budget, spent before the body.
pub mod oauth_as_rate_limit;
/// The request-taking endpoints — `/oauth/par` and, as they land, its siblings.
pub mod oauth_as_routes;
/// The server's runtime memory — the PAR store, the DPoP nonce minter, the
/// replay sets and the client-metadata cache.
pub mod oauth_as_state;
/// The token plane — the ES256 access token and the HS256 refresh token.
pub mod oauth_as_token;
/// The consent starts' user half —
/// `fauna.oauth.consent.{lookup_code,open_handoff,block_client,list_blocked_clients}`.
pub mod oauth_consent_handlers;
/// The admin surface —
/// `fauna.oauth.{issuer_key_status,rotate_issuer_key,force_rotate_issuer_key,force_rotate_session_secret}`.
pub mod oauth_issuer_handlers;
/// The key plane — the boot-seated signer, the two rotation arms, the horizon sweep.
pub mod oauth_issuer_key;
/// The public readers — `/oauth/jwks` and the two discovery documents.
pub mod oauth_issuer_routes;
/// The AS's second signer — the HS256 secret its refresh tokens are MACed under.
pub mod oauth_session_secret;
pub mod outbound_bounce;
#[cfg(feature = "test-hooks")]
pub mod outbound_clock_test_hook;
#[cfg(feature = "test-hooks")]
pub mod outbound_drain_test_hook;
pub mod outbound_retry;
pub mod outbound_tlsrpt;
#[cfg(feature = "test-hooks")]
pub mod outbound_tlsrpt_test_hook;
pub mod outbox;
pub mod pair_handlers;
/// Test-only: the scanner behind the crate's partition gates.
#[cfg(test)]
pub(crate) mod partition_scan;
#[cfg(feature = "test-hooks")]
pub mod post_fanout_test_hook;
#[cfg(feature = "test-hooks")]
pub mod region_relay_test_hook;
#[cfg(feature = "test-hooks")]
pub mod rescore_drain_test_hook;
pub mod room_post_view;
pub mod room_read_key;
#[cfg(feature = "test-hooks")]
pub mod spam_history_gc_test_hook;
// pairing_routes deleted — `fauna.pair.list` is the sole surface (pair_handlers).
pub mod payload_store;
// The money plane (`dynamic-features.md` § Charter members, the `payments`
// registry member): the entitlement engine, its RPC face, and the provider
// webhook ingress. Default-ON — excision is what a build FLAVOR asks for.
#[cfg(feature = "payments")]
pub mod payment_core;
#[cfg(feature = "payments")]
pub mod payment_handlers;
#[cfg(feature = "payments")]
pub mod payment_routes;
pub mod peer_query;
pub mod pending_action_handlers;
pub mod pending_actions;
pub mod personalization_handlers;
pub mod plugin_runner;
pub mod plugins_handlers;
pub mod post_delete_redrive;
pub mod posts_handlers;
pub mod pre_identity_allowlist;
pub mod principal_handlers;
pub mod principal_session;
pub mod principals_handlers;
pub mod profile_handlers;
// `fauna.protocol.echo` — a transport round-trip probe for tests, served only
// by a `test-hooks` build (gated 2026-10-01: a shipped nest answers it
// `fauna.protocol.unknown_kind`, e2e-automation-surface-gating.md rule (a)).
#[cfg(feature = "test-hooks")]
pub mod protocol_test;
pub mod push;
pub mod push_handlers;
pub mod router_status;
// push_routes deleted — `fauna.push.*` is the sole surface (push_handlers).
#[cfg(feature = "test-hooks")]
pub mod pending_actions_test_hook;
#[cfg(feature = "test-hooks")]
pub mod push_test_hooks;
pub mod rate_limit;
pub mod records_door;
pub mod recovery_handlers;
#[cfg(feature = "test-hooks")]
pub mod recovery_landing_test_hook;
pub mod region_relay;
pub mod region_tier;
pub mod registration;
pub mod relay_admission;
pub mod restore;
pub mod routes;
pub mod rpc_errors;
pub mod rpc_router;
pub mod s3_blob_store;
pub mod search_handlers;
#[cfg(feature = "test-hooks")]
pub mod version_prune_test_hook;
#[cfg(feature = "test-hooks")]
pub mod web_blank_site_test_hook;
#[cfg(feature = "test-hooks")]
pub mod web_domain_activate_test_hook;
// search_routes deleted — `fauna.search.query` is the sole surface (search_handlers).
pub mod security_notify;
pub mod segment_backup;
pub mod segment_backup_test_hook;
pub mod segments;
pub mod self_signed_cert;
pub mod services;
pub mod session_handlers;
pub mod share_handlers;
pub mod share_routes;
pub mod sidecar_channel;
pub mod sidecar_tokens;
pub mod snapshot_scheduler_test_hook;
pub mod spam_baseline;
pub mod spam_handlers;
pub mod spam_model_seal;
pub mod ssrf;
pub mod state;
pub mod stats_handlers;
pub mod storage;
pub mod succession_ownership;
pub mod succession_pull;
pub mod sweeper;
// Buy side of the `payments` member (§ Charter members: "…and tips").
#[cfg(feature = "payments")]
pub mod tip_handlers;
pub mod tls_handlers;
#[cfg(feature = "test-hooks")]
pub mod tlsa_test_hook;
pub use storage::SharedStorage;
pub(crate) mod change_signature;
pub mod streaming;
pub mod subscription_handlers;
pub mod subscription_routes;
pub mod sync_handlers;
/// In-process test-nest construction, lent to this crate's own integration
/// tests and to `fauna-sync-agent`'s `tier3-nest` harnesses. Gated per
/// `../../../docs/goal/architecture/e2e-automation-surface-gating.md` § rule
/// (a): the `debug_assertions` arm keeps a plain `cargo test -p fauna-nest` in
/// the default loop with no Cargo wiring, the feature arm is the escape hatch
/// for a release-profile test build, and a release artifact carries neither.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub mod test_support;
pub mod text_heuristic;
pub mod token_store;
pub mod transport_policy_handlers;
pub mod trend_sweeper;
pub mod ttl_gc;
#[cfg(unix)]
pub mod unix_signal;
pub mod video;
pub mod video_routes;
pub mod web_app_origin;
pub mod web_content;
pub(crate) mod web_files_projection;
pub mod web_handlers;
#[cfg(feature = "test-hooks")]
pub mod web_paywall_test_hook;
pub mod ws;

use std::sync::Arc;

use anyhow::Result;
use routes::AppState;

async fn info_page() -> axum::response::Html<&'static str> {
    axum::response::Html(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Fauna Nest</title></head>
<body style="font-family:system-ui;max-width:600px;margin:80px auto;text-align:center">
<h1>Fauna Nest API</h1>
<p>This server hosts a Fauna nest.</p>
<p>Open the web app: <a href="/app">local</a> · <a href="https://app.fauna.social">app.fauna.social</a></p>
</body></html>"#,
    )
}

/// Handler that checks the `Host` header for web content domains.
/// If the host resolves to a user's web content site, serve it.
/// Otherwise, fall through to the default info page.
async fn web_content_or_info(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    if let Some(resolver) = &state.host_resolver {
        let host = headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        if let Some(actor_id) = resolver.resolve(host).await {
            let path = uri.path();
            if let Some(ref wcs) = state.web_content_service {
                let blob_store = wcs.blob_store();
                // Web-paywall capability token (`?token=…`, Pillar 2): an
                // explicit URL-carried bearer, verified statelessly inside the
                // serve path — never a cookie.
                let token = uri.query().and_then(|q| {
                    q.split('&')
                        .find_map(|kv| kv.strip_prefix("token="))
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                });
                return web_content::serve::serve_web_content_with_token(
                    &state.db,
                    blob_store,
                    wcs.at_rest_key(),
                    &actor_id,
                    path,
                    state.web_serve_holder.as_deref(),
                    token.as_deref(),
                )
                .await;
            }
        }
    }
    info_page().await.into_response()
}

/// Assemble the MTA-STS policy body served at `/.well-known/mta-sts.txt` for
/// `domain`, coupling the published `mode:` to the served MX cert reality
/// (`docs/goal/architecture/nest/tls-certificates.md` § D — this doc owns the
/// coupling rule). A domain whose `mail.<domain>` MX is on the always-live
/// self-signed floor must **not** advertise `enforce`: an MTA-STS-enforcing
/// sender (RFC 8461 §5) that fetched `enforce` then refuses the non-WebPKI MX,
/// **bouncing inbound mail**. So `enforce` downgrades to `testing` while the MX
/// is untrusted (`cert_health_state == OnFloorRenewNeeded`, i.e. floor / no cert
/// / non-covering), restoring `enforce` once a trusted covering cert is live.
/// `mx_facts` is what the listener would serve for the MX SNI; `None` reads as
/// on-floor.
///
/// Assemble the served MTA-STS policy body for one `mail_domains` row, coupling
/// the domain's **stored** published mode to the served-MX cert reality (the
/// cert-honesty rule, `tls-certificates.md` § D). `mx_host` is the deployment's
/// single MX target (`mail.<primary-domain>` — every local domain MXes to it,
/// `mail-multidomain.md` § Architectural rules); `mx_facts` is what the TLS
/// listener would serve for that MX SNI (`None` reads as on-floor). The stored
/// mode is a **ceiling**: a floor MX downgrades `enforce → testing`, while
/// `testing` passes through unchanged (`MtaStsMode::coupled_to_cert`).
fn mta_sts_policy_body(
    mx_host: &str,
    stored_mode: fauna_mail::outbound::mta_sts::MtaStsMode,
    max_age_secs: u32,
    mx_facts: Option<crate::acme::ServedCertFacts>,
    now_unix: i64,
) -> String {
    use fauna_mail::outbound::mta_sts::{MtaStsPolicy, assemble_policy_body};
    let mx_trusted = !matches!(
        crate::acme::cert_health_state(mx_facts, now_unix),
        fauna_protocol::tls::CertHealthState::OnFloorRenewNeeded
    );
    let policy = MtaStsPolicy {
        version: "STSv1".to_string(),
        mode: stored_mode.coupled_to_cert(mx_trusted),
        mx: vec![mx_host.to_string()],
        max_age_secs,
    };
    assemble_policy_body(&policy)
}

/// Serve `/.well-known/mta-sts.txt` per RFC 8461 §3.2 + `mail-multidomain.md`
/// § Policy-file server: match the requested Host against the active
/// `mail_domains` set, read that domain's stored `mta_sts_mode` /
/// `mta_sts_max_age_seconds`, and serve the per-domain body with its published
/// mode coupled to the deployment's single MX (`mail.<primary>`) cert reality
/// (§ D). No match → 404 (the peer treats MTA-STS as unavailable). Keys on the
/// Host header — a well-formed fetch sends `Host: mta-sts.<domain>` matching its
/// SNI; the `mta-sts.` policy-host prefix is stripped.
async fn mta_sts_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use fauna_mail::outbound::mta_sts::{MIN_MTA_STS_MAX_AGE_SECS, MtaStsMode};

    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    // Drop any `:port`, then the conventional `mta-sts.` policy-host prefix
    // (peers fetch `https://mta-sts.<domain>/.well-known/mta-sts.txt`).
    let host = host.split(':').next().unwrap_or(host);
    let domain = host.strip_prefix("mta-sts.").unwrap_or(host);

    // Step 1 — match the requested host against the active mail_domains set.
    let Ok(Some(row)) = state.db.lookup_active_mail_domain(domain).await else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };

    // Step 2 — read this domain's stored mode + max_age. The stored mode is
    // `testing` or `enforce` (the nest's own advance is its only writer after the
    // add); anything else reads as the safe `testing`.
    let stored_mode = MtaStsMode::from_stored(&row.mta_sts_mode);
    // RFC 8461 §3.2 floors max_age at 1 day and caps it at ~1 year; clamp the
    // stored value into that range (the schema default-floors at 86400).
    let max_age_secs = row
        .mta_sts_max_age_seconds
        .clamp(MIN_MTA_STS_MAX_AGE_SECS as i64, 31_557_600) as u32;

    // The MX an inbound sender validates is the deployment's single
    // `mail.<primary-domain>` target (one MX across all domains, § Architectural
    // rules); couple the published mode to the cert the listener serves for THAT
    // SNI — the same `served_cert_facts("mail.<primary>")` truth the floor-MX
    // TLSA uses. The matched row is the primary itself in the single-domain case
    // (skip the extra lookup); else read the primary. Fall back to the matched
    // domain only if no primary is registered (a degenerate state the matched
    // active row should preclude).
    let primary_name = if row.is_primary {
        row.domain_name.clone()
    } else {
        state
            .db
            .lookup_primary_mail_domain()
            .await
            .ok()
            .flatten()
            .map(|p| p.domain_name)
            .unwrap_or_else(|| row.domain_name.clone())
    };
    let mx_host = format!("mail.{primary_name}");

    let now_unix = fauna_core::data::Timestamp::now_secs_or_zero();
    let mx_facts = state
        .served_cert_spki
        .as_ref()
        .and_then(|r| r.served_cert_facts(&mx_host));

    (
        axum::http::StatusCode::OK,
        [
            ("content-type", "text/plain; charset=utf-8"),
            ("cache-control", "max-age=86400"),
        ],
        mta_sts_policy_body(&mx_host, stored_mode, max_age_secs, mx_facts, now_unix),
    )
        .into_response()
}

/// The `?t=<token>` query of the RFC 8058 one-click List-Unsubscribe endpoint.
#[derive(serde::Deserialize)]
struct ListUnsubscribeQuery {
    #[serde(default)]
    t: String,
}

/// `<token>` is base64url (the shape `UnsubscribeTokenGenerator` mints) — so a
/// well-formed token carries no HTML-special bytes. Rejecting anything else
/// before it is reflected into the rendered page closes the reflected-XSS
/// surface (a malformed token can't match a member row anyway, so nothing is
/// lost), and keeps the GET confirm page from echoing attacker-controlled
/// markup.
fn is_unsubscribe_token_shape(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// A complete, self-contained HTML page for the external recipient (no Fauna
/// app, no assets, no `ui.yaml` id — `mail-mass-mailing.md` § The HTTPS
/// endpoint), shared by the unsubscribe and `List-Help` pages. `inner` is trusted markup the caller assembles from already-safe
/// pieces (validated token shape only).
fn list_page(heading: &str, inner: &str) -> String {
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{heading}</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:520px;\
         margin:80px auto;padding:0 16px;text-align:center;color:#222\">\
         <h1 style=\"font-size:1.4rem\">{heading}</h1>{inner}</body></html>"
    )
}

/// `GET /list/unsubscribe?t=<token>` — RFC 8058 fallback for MUAs that open the
/// link in a browser rather than firing the One-Click POST. **Read-only**: it
/// renders a confirmation page whose "Confirm unsubscribe" button POSTs back to
/// the same URL; the GET itself never unsubscribes — an ambiguous browser click
/// must not act (§ The HTTPS endpoint).
async fn list_unsubscribe_get(
    axum::extract::Query(q): axum::extract::Query<ListUnsubscribeQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let html = if is_unsubscribe_token_shape(&q.t) {
        // The token is known-safe (base64url) so embedding it in the form
        // action is injection-free.
        list_page(
            "Unsubscribe from this list",
            &format!(
                "<p>Click the button below to stop receiving messages from this \
                 mailing list.</p>\
                 <form method=\"post\" action=\"/list/unsubscribe?t={token}\">\
                 <input type=\"hidden\" name=\"List-Unsubscribe\" value=\"One-Click\">\
                 <button type=\"submit\" style=\"font-size:1rem;padding:10px 20px;\
                 cursor:pointer\">Confirm unsubscribe</button></form>",
                token = q.t
            ),
        )
    } else {
        list_page(
            "Unsubscribe link not recognized",
            "<p>This unsubscribe link is malformed or has expired. Please use the \
             link in the most recent message you received.</p>",
        )
    };
    (
        axum::http::StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// `GET /list/<list-id>/help` — the default RFC 2369 `List-Help` target
/// (`mail-mass-mailing.md` § RFC 2369 list headers): a static page explaining
/// how subscribing and unsubscribing work. Unauthenticated and deliberately
/// list-agnostic — the id is neither looked up nor echoed, so the page leaks no
/// list or member data and is no list-existence oracle.
async fn list_help_get(
    axum::extract::Path(_list_id): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let html = list_page(
        "About this mailing list",
        "<p>You received this message because the list's sender added your \
         address to their mailing list. Lists here only send: replies to the \
         list address are not accepted.</p>\
         <p><strong>To unsubscribe</strong>, use your mail program's \
         \u{201c}Unsubscribe\u{201d} button, or open the unsubscribe link in \
         any message from the list. One click is enough, and no account is \
         needed.</p>\
         <p><strong>To subscribe</strong>, or to be added back after \
         unsubscribing, ask the list's sender directly: members are added by \
         the list's owner.</p>",
    );
    (
        axum::http::StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// `POST /list/unsubscribe?t=<token>` — RFC 8058 §3.3 One-Click. The POST body
/// MUST carry `List-Unsubscribe=One-Click` (reject `400` otherwise). The token
/// resolves a member by its cached index and flips `unsubscribed_at`;
/// idempotent on a second click. Unauthenticated — the token IS the consent.
async fn list_unsubscribe_post(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Query(q): axum::extract::Query<ListUnsubscribeQuery>,
    body: String,
) -> axum::response::Response {
    use crate::db::mail_lists::UnsubscribeOutcome;
    use axum::response::IntoResponse;

    // RFC 8058 §3.3: the One-Click POST body is `List-Unsubscribe=One-Click`.
    if !body
        .to_ascii_lowercase()
        .contains("list-unsubscribe=one-click")
    {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            [("content-type", "text/html; charset=utf-8")],
            list_page(
                "Bad request",
                "<p>This request was not a valid one-click unsubscribe.</p>",
            ),
        )
            .into_response();
    }
    if !is_unsubscribe_token_shape(&q.t) {
        return (
            axum::http::StatusCode::NOT_FOUND,
            [("content-type", "text/html; charset=utf-8")],
            list_page(
                "Unsubscribe link not recognized",
                "<p>This unsubscribe link is malformed or has expired.</p>",
            ),
        )
            .into_response();
    }
    let (status, heading, msg) = match state.db.unsubscribe_member_by_token(&q.t).await {
        Ok(UnsubscribeOutcome::Unsubscribed) => (
            axum::http::StatusCode::OK,
            "You have been unsubscribed",
            "<p>You will no longer receive messages from this mailing list.</p>",
        ),
        Ok(UnsubscribeOutcome::AlreadyUnsubscribed) => (
            axum::http::StatusCode::OK,
            "Already unsubscribed",
            "<p>You were already unsubscribed from this mailing list.</p>",
        ),
        Ok(UnsubscribeOutcome::NotFound) => (
            axum::http::StatusCode::NOT_FOUND,
            "Unsubscribe link not recognized",
            "<p>This unsubscribe link is invalid or has expired. Please use the \
             link in the most recent message you received.</p>",
        ),
        Err(e) => {
            tracing::warn!(target: "mail_lists", error = %e, "one-click unsubscribe failed");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "<p>We couldn't process your unsubscribe request. Please try again \
                 later.</p>",
            )
        }
    };
    (
        status,
        [("content-type", "text/html; charset=utf-8")],
        list_page(heading, msg),
    )
        .into_response()
}

/// Build the axum Router with all routes.
//
// The WS-RPC-everywhere control-plane rip-out is COMPLETE: every
// `#[deprecated]` HTTP twin has been deleted and its consumers migrated to the
// corresponding WS-RPC kind. The LAST twin — `routes::post_auth_token`
// (`POST /api/v1/auth/token`, the bearer-bootstrap) — was deleted once every
// app minted over `fauna.auth.handshake` (the shared FFI `mint_bearer` /
// `WsChallengeBearer`; the Go mail-bridge rides `fauna.auth.{challenge,verify}`
// — never the token route). What remains on HTTP is Bucket C only: byte-bulk
// (blob/chunk/manifest/segment/video/snapshot/sync-file/share/posts-GET/
// subscription-MLS-blob-gets/`/admin/export/all`), WS upgrades (`/ws`,
// `/ws/{actor}`, `/federation/ws`, `/internal/relay/ws`), and external
// standards (mta-sts/bridge-relay/ActivityPub/WebFinger/NodeInfo/
// CalDAV/Nostr-relay) — all on pinned-SPKI TLS. No `#[allow(deprecated)]` is
// needed: no `#[deprecated]` twin remains wired here. The `ChallengeStore`
// stays — it backs the live `fauna.auth.{challenge,verify}` WS-RPC kinds.
/// HTTP body limit for the chunk/manifest upload routes. A chunk is bounded by
/// the chunker's `MAX_CHUNK` (8 MB); this sits above that (and axum's 2 MB
/// `Bytes` default) while matching the smallest tier's 10 MB "max blob" limit
/// (`docs/goal/behavior/file-sync.md` § Content-Addressed Storage). Kept ≤ the
/// global `RequestBodyLimitLayer` so the global never silently shadows it.
pub(crate) const CHUNK_BLOB_BODY_LIMIT: usize = 10 * 1024 * 1024;

pub fn build_router(state: Arc<AppState>) -> axum::Router {
    use axum::routing::{get, post};

    let router = axum::Router::new()
        .route("/", get(web_content_or_info))
        // Both `/api/v1/inbox/{actor_id}` HTTP twins are gone (WS-RPC-everywhere
        // rip): the POST cross-nest signed (CR,Post) delivery moved to
        // `fauna.inbox.send` (client→home-nest) + `fauna.federation.inbox.deliver`
        // (nest→nest); the GET drain moved to `fauna.inbox.fetch` (a pure peek) +
        // `fauna.inbox.ack` (consume-after-durable-apply), which fixes the GET's
        // mark-on-read data-loss bug. The shared `deliver_inbox_payload_core` /
        // `poll_inbox` / `mark_delivered` they ride are kept.
        .route("/api/v1/ws/{actor_id}", get(routes::ws_handler))
        // Anonymous (pre-identity) WS — no actor_id, no bearer; routes only the
        // pre-identity allowlist. transport.md § Pre-identity (anonymous) connection.
        .route("/api/v1/ws", get(routes::ws_anonymous_handler))
        // The principal session — a third-party principal's token-bearing
        // WS-RPC upgrade (`transport-connection.md` § *The principal session*).
        .route(
            fauna_bridge_atproto::oauth_metadata::PATH_PRINCIPAL_WS,
            get(principal_session::principal_ws_handler),
        )
        // A remote principal's folder deposit — HTTP residue for a server
        // that cannot speak WS-RPC (`file-sync.md` § Third-party deposit
        // ingress; `api-layers.md` § HTTP residue). DPoP-bound token, the
        // same admission and dispatch as the principal session.
        .route(
            fauna_bridge_atproto::oauth_metadata::PATH_FOLDER_DEPOSIT,
            post(folder_deposit::deposit_http_handler),
        )
        // A remote principal's own `ext.*` records — the HTTP record door
        // (`third-party-kinds.md` § The record doors; `api-layers.md` § HTTP
        // residue). The same admission, dispatch and plane as the principal
        // session's `fauna.account.state.put` / `fauna.sync.changes.list`.
        .route(
            fauna_bridge_atproto::oauth_metadata::PATH_RECORDS,
            get(records_door::walk_handler),
        )
        .route(
            fauna_bridge_atproto::oauth_metadata::PATH_RECORD,
            get(records_door::get_handler)
                .put(records_door::put_handler)
                .delete(records_door::delete_handler)
                // The size floor, before a byte past it is buffered: a body is
                // one sealed entry.
                .layer(axum::extract::DefaultBodyLimit::max(
                    fauna_protocol::account_state::MAX_STATE_ENTRY_BYTES,
                )),
        )
        // A remote principal's events long-poll — HTTP residue for a server
        // that cannot speak WS-RPC (`transport.md` § Push events →
        // *Third-party event doors*): the same admission as the deposit door,
        // dispatching `fauna.events.poll`.
        .route(
            fauna_bridge_atproto::oauth_metadata::PATH_EVENTS,
            get(events_doors::events_http_handler),
        )
        // Federation WS (nest↔nest) — no bearer; the L3 `fauna.federation.hello`
        // handshake (first frame) authenticates the peer nest. Keyed on peer
        // nest_id. Spec Y2 slice 4 / federation.md § Transport.
        .route(
            "/api/v1/federation/ws",
            get(federation_channel::federation_ws_handler),
        )
        // Knocks, contacts, inbox-mode + notifications migrated to the
        // `fauna.{knocks,contacts,inbox.mode,notifications}.*` WS-RPC kinds;
        // the HTTP twins + their route lines
        // were deleted in T4. All four are pure same-nest client surfaces (no
        // federation residue — `peer_query.rs`/`nest_link/` reference none).
        // Cross-nest post fetch — permanent public-byte residue
        // (`api-layers.md` § Remaining HTTP). Local clients use the WS-RPC
        // `fauna.posts.get`; this optional-auth HTTP route is the `fetch_url`
        // `peer_query.rs` builds for discovery-feed post bodies, read by a
        // client from the author's nest (like `/api/v1/blob`).
        // `posts.{create,interact}` moved fully to WS-RPC — T4.
        .route("/api/v1/posts/{post_id}", get(routes::get_post))
        .route("/api/v1/health", get(routes::health))
        // `setup-status` retired (S4c2): clients/tests read setup progress via
        // the `fauna.setup.status` WS-RPC kind (`discovery_handlers`).
        // `POST /api/v1/auth/token` retired (endgame): the bearer-bootstrap
        // rides the pre-identity kind `fauna.auth.handshake` (`auth_handlers`,
        // shared `auth_core::direct_auth_core`); every app mints over the
        // FFI `mint_bearer` / `WsChallengeBearer`. It was the LAST control-plane
        // HTTP twin — none remain.
        // Admin claim retired (S4d): the one-time admin claim rides the
        // `fauna.auth.claim_admin` WS-RPC kind (`claim_handlers`) on the
        // anonymous connection; the ceremony lives in `claim_core`.
        // Storage-mode commit retired (S4c2 moved it to a WS-RPC kind; the
        // kind itself left with the compat-remnant sweep, 2026-09-24).
        // Blob routes
        .route("/api/v1/blob", post(blob_routes::upload_blob))
        // GET dispatches by `{id}` format: multibase base32 CIDs (CBOR-DAG
        // Layer 3) and plain hex blake3 digests share the path — both shapes
        // are permanent (`api-layers.md` § Remaining HTTP); PUT is CID-only.
        //
        // The PUT body is one whole blob, so — exactly like the chunk and
        // manifest routes above — it needs a body limit above axum's 2 MB
        // `Bytes` default, which would otherwise 413 any blob >2 MB. The cap is
        // `blob_routes::BLOB_BODY_LIMIT`, the same 10 MB that handler's own doc
        // comment already claimed for "multipart total body **or octet-stream
        // body**" but which only the multipart half actually enforced. The
        // first caller to need it is the `__index` rail's segment publisher
        // (`content-index.md` § Ingest triggers, v1 — one segment is one blob,
        // there is no chunk-manifest form on that rail), whose segments
        // routinely exceed 2 MB for a real mailbox.
        .route(
            "/api/v1/blob/{id}",
            get(blob_routes::download_blob)
                .put(blob_routes::put_blob_by_cid)
                .layer(axum::extract::DefaultBodyLimit::max(
                    blob_routes::BLOB_BODY_LIMIT,
                )),
        )
        // Video segment routes. The upload deliberately carries no body-limit
        // layer: one segment per request, bounded by axum's 2 MB `Bytes`
        // default like every route not raised above or below. Each segment
        // writes its `blob_metadata` row, so an upload no post names reclaims.
        .route("/api/v1/video/segments", post(video_routes::upload_segment))
        .route(
            "/api/v1/video/segments/{hash}",
            get(video_routes::download_segment),
        )
        // Video manifest routes (HLS)
        .route(
            "/api/v1/video/{post_id}/master.m3u8",
            get(video_routes::serve_master_manifest),
        )
        .route(
            "/api/v1/video/{post_id}/{variant}",
            get(video_routes::serve_variant_manifest),
        )
        // Chunk routes. The upload body is a single chunk — bounded by the
        // chunker's MAX_CHUNK (8 MB) — so it needs a body limit above axum's 2 MB
        // `Bytes` default (which otherwise 413s any chunk >2 MB; F4). 10 MB
        // matches the smallest tier's "max blob" limit and leaves headroom over
        // an 8 MB chunk + compression framing. Other routes keep the 2 MB default.
        .route(
            "/api/v1/chunks",
            post(chunk_routes::upload_chunk)
                .layer(axum::extract::DefaultBodyLimit::max(CHUNK_BLOB_BODY_LIMIT)),
        )
        // NOTE: the GET `/api/v1/chunks/{hash}` + `/api/v1/manifests/{hash}`
        // byte routes live on the public-CORS sub-router below (S6) — only the
        // POST/check halves stay under the blanket credentialed layer here.
        .route("/api/v1/chunks/check", post(chunk_routes::check_chunks))
        // Relay serving's answer route (`file-sync.md` § Relay serving): an
        // announced seat's chunk for a `fauna.sync.chunk.wanted` ask, or its
        // decline. The body is one chunk, so it takes the upload's limit.
        .route(
            "/api/v1/chunks/relay/{request_id}",
            post(chunk_routes::answer_relay_chunk)
                .delete(chunk_routes::decline_relay_chunk)
                .layer(axum::extract::DefaultBodyLimit::max(CHUNK_BLOB_BODY_LIMIT)),
        )
        // Segment byte-source (Plan 5 — sibling of /api/v1/chunks). The
        // `/meta` sibling serves the pair's sidecar, which an adopting
        // replica needs and a backup pass does not (segment_route's module
        // docs argue the split).
        .route(
            "/api/v1/segments/{kind}/{actor_hex}/{segment_id}",
            get(crate::segments::segment_route::get_segment),
        )
        .route(
            "/api/v1/segments/{kind}/{actor_hex}/{segment_id}/meta",
            get(crate::segments::segment_route::get_segment_meta),
        )
        // Manifest routes. Manifests are small (a list of 32-byte hashes) but
        // share the chunk body limit for uniformity with the chunk route.
        .route(
            "/api/v1/manifests",
            post(chunk_routes::upload_manifest)
                .layer(axum::extract::DefaultBodyLimit::max(CHUNK_BLOB_BODY_LIMIT)),
        )
        // Share route (no auth — token is self-authorising)
        .route("/share/{token}", get(share_routes::handle_share))
        // Sync control plane migrated to the `fauna.sync.*` /
        // `fauna.filesync.files.list` WS-RPC kinds (sync_handlers) — the
        // deprecated HTTP twins (register/changes/backup-status/status/files/
        // devices) were deleted in the WS-RPC-everywhere rip-out, and the
        // server-side single-file reassembly route (`/sync/file/{*path}`,
        // `sync_routes::download_sync_file`) was deleted 2026-07-14: it never
        // had a production caller, and the unconditional owner-only chunk seal
        // means the nest cannot reassemble plaintext (backup-restore.md § 3).
        // No sync route stays HTTP.
        // Snapshot control plane migrated to the `fauna.filesync.snapshot.*`
        // WS-RPC kinds (filesync_handlers) — the deprecated HTTP twins were
        // deleted in the WS-RPC-everywhere rip-out, and the two legacy-plaintext
        // byte routes (single-file download, ZIP restore) in the compat-remnant
        // sweep (version-compatibility.md § Dimension 2). No snapshot route
        // stays HTTP.
        // Moderation/spam control plane migrated to `fauna.moderation.*` /
        // `fauna.spam.*` (moderation_handlers / spam_handlers) — deprecated HTTP
        // twins deleted. No moderation route stays HTTP.
        // Cross-nest key-package fetch and Welcome delivery (and the reputation
        // exchange/export, since removed) rode HTTP federation twins until Spec Y2 slice 5 retired the
        // HTTP interim; they are now served solely over the nest↔nest WS-RPC
        // federation channel (`fauna.federation.*`, `federation_handlers`).
        // Discovery (handle-available / node-info / resolve-node / actor-by-handle)
        // migrated to the pre-identity WS-RPC kinds `fauna.{handle.available,
        // nest.info, nest.resolve, actor.by_handle}` (discovery_handlers, shared
        // discovery_core) — deprecated HTTP twins deleted. `POST /api/v1/register`
        // was already retired (S4f) for `fauna.account.register`.
        // Session list/revoke/revoke-all migrated to `fauna.sessions.*`
        // (session_handlers) — deprecated HTTP twins deleted. The no-token
        // emergency lockout (`POST /api/v1/account/lockout`) was the last
        // HTTP twin here; it migrated to the pre-identity WS-RPC kind
        // `fauna.account.lockout` (account_handlers, signature-authed in-band
        // over `actor_id ‖ timestamp_be`, on the anonymous connection) and the
        // route was deleted in the WS-RPC-everywhere rip-out. The authed sibling
        // is the bearer kind `fauna.sessions.lockout`.
        // Ed25519 challenge-response auth (silent sign-in) migrated to the
        // pre-identity WS-RPC kinds `fauna.auth.{challenge,verify}`
        // (`auth_handlers`, shared `auth_core` ceremony); the
        // `POST /api/v1/auth/{challenge,verify}` HTTP twins were DELETED in the
        // WS-RPC-everywhere rip-out once the last consumer (apple
        // `APIClient.silentSignIn`) moved. `challenge_auth::ChallengeStore` stays
        // — it backs the live WS-RPC kinds.
        // (The user-side in-band invite HTTP twins — POST /api/v1/invite-requests,
        // GET /api/v1/invite-requests/{actor_id}/status, DELETE
        // /api/v1/invite-requests/{actor_id}, and POST /api/v1/invite-code/verify —
        // were retired; their only caller, onboarding, rides the pre-identity
        // `fauna.account.invite_request.{submit,status,cancel}` +
        // `fauna.account.invite_code.verify` WS-RPC kinds (S4a2/S4b). The
        // admin-side /admin/api/invite-requests twins were deleted in
        // the WS-RPC-everywhere rip-out too → `fauna.admin.invite_requests.{list,
        // approve,deny}` (admin_ws_handlers) — see below.)
        // Admin control plane migrated to the `fauna.admin.*` WS-RPC kinds
        // (admin_ws_handlers) — ALL of the deprecated `/admin/api/*` HTTP twins
        // (stats, users CRUD + evict/cancel-eviction/suspend/clear-handle, tiers
        // list/create/update, audit list/integrity, invite-codes create/list/
        // delete, invite-requests list/approve/deny, cluster.status, gc, status,
        // admins list/add/remove, services list/update, worker.status) were
        // deleted in the WS-RPC-everywhere rip-out. The shared cores stay:
        // `admin::generate_invite_code`, `services::ServiceIntent`. (The
        // wireguard admin + peer kinds that also lived here died with the
        // WireGuard stack, 2026-08-23.)
        // /admin/api/pairings RETIRED earlier — pairing is the user's own
        // `fauna.pair.{add,revoke}` (per-user-pairing design 2026-05-25).
        //
        // KEEP — the admin bulk export is byte-bulk (no kind; transport.md §
        // HTTP residue), rides the pinned TLS.
        .route(
            "/api/v1/admin/export/all",
            get(admin_export_routes::handle_admin_export_all),
        )
        // Folder HTTP twins (admin create/get/members/destinations, user
        // list/create/update/delete/devices/members/schedule, sync conflicts
        // list/report/resolve, upload lease acquire/release) deleted in the
        // WS-RPC-everywhere rip-out → `fauna.admin.folders.*` /
        // `fauna.folders.*` / `fauna.sync.conflicts.*` /
        // `fauna.folders.lease.*` (folder_handlers / admin_ws_handlers).
        // Feed CRUD + feed-post queries + discovery contributors moved fully
        // to the WS-RPC `fauna.feed.*` kinds — T4
        // deleted the HTTP twins. Cross-nest feed query rode an HTTP twin until
        // Spec Y2 slice 5 retired it; it now rides the
        // `fauna.federation.feed.query` channel kind. `GET /api/v1/context`
        // (the engagement-derived interest profile) was DELETED in the
        // WS-RPC-everywhere rip-out for having
        // zero client consumers fleet-wide. The dormant DB path it fronted
        // (`compute_user_context` + the `recompute_score_personalized` seam) is
        // now GONE too, retired with the rest of the pre-frame behavioral surface
        // (frame D9 — `docs/goal/behavior/engagement-cues.md` § Retirement): the
        // nest never holds a plaintext interest profile. Personalization is a
        // tier-1, client-side, sealed factor (`docs/goal/behavior/topic-factors.md`).
        // Likewise `fauna.engagement.{record,list}` (the per-actor plaintext
        // behavioral recorder) and `fauna.access_grants.*` (the superseded
        // "Algorithm Service reads your profile" grant surface — NOT to be confused
        // with the live `fauna.capabilities.*`) are deleted; all three were
        // client-dead in every version ever shipped.
        // Search migrated to `fauna.search.query` (search_handlers) — HTTP twin deleted.
        // Subscription control plane migrated to the `fauna.subscriptions.*`
        // WS-RPC kinds (registered by subscription_handlers below) — the
        // deprecated HTTP twins (tiers create/update/delete, subscribe/
        // unsubscribe, status, requests list/approve/reject, subscribers
        // list/remove, delegate upload) were deleted in the WS-RPC-everywhere
        // rip-out. The three subscriber key-material blob downloads
        // (key-blob / epoch-secret / archival-blob) were deleted in the
        // follow-on (2026-06-19) once every app read them over the
        // `fauna.subscriptions.*.get` kinds (verified zero HTTP consumers
        // fleet-wide); of those kinds only `key_blob.get` survives — the
        // tier-MLS pair went with its plane in the compat-remnant sweep
        // (2026-09-27, `ui/feed.md` § Encryption at rest, room ruling 8).
        // The surviving HTTP routes are:
        //   - `list_tiers` (`/subscriptions/tiers/{author_id}`) — the
        //     deliberately *unauthenticated* / external (web-paywall Pillar 2,
        //     federation) per-author tier read, kept as public residue per
        //     `monetization.md` § Pillar 1. Authenticated Fauna apps read the
        //     same data over WS-RPC: `fauna.subscriptions.offers.list` (another
        //     author, request-supplied `author_id`) and
        //     `fauna.subscriptions.tiers.list` (the caller's own tiers) — BOTH
        //     kinds exist; this HTTP route is NOT a deprecated twin of them;
        //   - `nest_info` (`/api/v1/nest/info`) + `get_delegation`
        //     (`/subscriptions/delegate/{author_id}`) — public federation
        //     bootstrap reads (no auth), Bucket-B residue.
        .route(
            "/api/v1/subscriptions/tiers/{author_id}",
            get(subscription_routes::list_tiers),
        )
        .route("/api/v1/nest/info", get(subscription_routes::nest_info))
        .route(
            "/api/v1/subscriptions/delegate/{author_id}",
            get(subscription_routes::get_delegation),
        )
        // Export routes
        .route("/api/v1/export", get(export_routes::handle_export))
        // The per-session mailbox-export blob (`mail-export.md` § Download
        // flow). A sibling of the whole-account route above, not a
        // replacement: that one is every plane of the account as a zip,
        // this one is a single sealed mail archive the user's own client
        // assembled and only their key opens. Sealed is not harmless: the
        // handler carries the same use-time duties (standing at use, the
        // owner rung on every download), in the byte plane's `401` shape.
        .route(
            "/api/v1/export/{session_id}",
            get(export_routes::handle_export_session_blob),
        );

    // Payment-provider webhook ingress (monetization.md § Pillar 3): external
    // unauthenticated-caller surface — providers can't speak WS-RPC. Registered
    // above the web-content fallback so it is un-shadowable and its rejections
    // are real non-2xx statuses, never the catch-all 200 info page.
    //
    // Split out of the chain above rather than gated in place: an attribute
    // cannot sit on a chained method call, and a store-safe nest must not merely
    // refuse this path — it must not HAVE it (`dynamic-features.md` § What
    // "completely compiled away" means, item 5: no re-enable path).
    #[cfg(feature = "payments")]
    let router = router.route(
        &format!(
            "{}/{{author_id}}/{{provider}}",
            fauna_payments::WEBHOOK_PATH_PREFIX
        ),
        post(payment_routes::payment_webhook),
    );
    // The legacy plaintext-`content`-table calendar/event control plane
    // (`fauna.{calendars,events}.*` WS-RPC kinds + the `calendar_handlers` /
    // `events_handlers` / `calendar_routes` / `event_social_routes` /
    // `calendar_import` cores + the `db::{calendar,events}` modules + the legacy
    // cross-nest event-invitation federation legs) was fully RETIRED in the
    // Decision-B § 4c cleanup (events.md / caldav-server.md § Implementation
    // status today) once all 6 apps lifted onto the encrypted
    // `bridge_caldav_*` store. Calendars/events now live solely on that
    // encrypted path (`fauna.bridges.*` + `fauna_client_caldav` / the MDA); the
    // only CalDAV HTTP surface kept here is the `/.well-known/caldav` discovery
    // redirect below.

    let router = router
        // Nest pairing routes RETIRED — authorization/revocation are the user's
        // bearer `fauna.pair.{add,revoke}` kinds and the read is `fauna.pair.list`
        // (pair_handlers); the deprecated `/api/v1/pairings/{actor_id}` HTTP twin
        // was deleted in the WS-RPC-everywhere rip-out.
        // Post forwarding and paired nest sync rode HTTP federation twins until
        // Spec Y2 slice 5 retired the HTTP interim; they are now served solely
        // over the nest↔nest WS-RPC federation channel
        // (`fauna.federation.post.forward` / `.sync.{pull,push,mls_pull,mls_ack}`,
        // `federation_handlers`).
        // CalDAV discovery redirect — the only in-nest CalDAV HTTP surface; the
        // encrypted-mode apex `301`s to `mail.<primary>` where the mail-bridge MDA
        // serves the real CalDAV store. (The legacy in-core plaintext PROPFIND/
        // REPORT/GET/PUT/DELETE handlers were retired with the § 4c cleanup.)
        .route(
            "/.well-known/caldav",
            axum::routing::any(caldav_bridge::wellknown_caldav),
        )
        // CardDAV discovery redirect — sibling of the CalDAV one; the apex `301`s
        // to `mail.<primary>` where the mail-bridge MDA serves the CardDAV store.
        // `503` until CardDAV is enabled (carddav-server design § 6). CardDAV is
        // seal-always + MDA-only, so (unlike CalDAV) there is no in-core plaintext
        // `207` branch — discovery `301`s in both storage modes once enabled.
        .route(
            "/.well-known/carddav",
            axum::routing::any(carddav_bridge::wellknown_carddav),
        )
        // The WebDAV files apex — a NextCloud-ecosystem convention (no SRV).
        // `301` straight to `mail.<primary>/webdav/` where the MDA serves the
        // folder view; `503` until WebDAV is live (webdav-server.md § Network
        // exposure & discovery).
        .route(
            "/.well-known/webdav",
            axum::routing::any(webdav_bridge::wellknown_webdav),
        );

    let router = router.route("/.well-known/mta-sts.txt", get(mta_sts_handler));

    // The OAuth issuer's whole plane (TP5) — discovery, JWKS and the
    // request-taking endpoints — is NOT mounted here: it carries its own
    // credential-less CORS layer and is merged after `.layer(cors)` at the
    // bottom of this function, beside `public_bytes` (`oauth_issuer_routes::routes`
    // owns the mount and says why the order is load-bearing).

    // RFC 8058 one-click List-Unsubscribe (mail-mass-mailing.md § The HTTPS
    // endpoint). Unauthenticated — the `?t=<token>` IS the consent. GET renders
    // a read-only confirm page; POST performs the unsubscribe.
    // The static RFC 2369 `List-Help` page (§ RFC 2369 list headers).
    let router = router
        .route(
            "/list/unsubscribe",
            get(list_unsubscribe_get).post(list_unsubscribe_post),
        )
        .route("/list/{list_id}/help", get(list_help_get));

    #[cfg(feature = "bluesky")]
    let router = router.merge(bluesky::routes());

    #[cfg(feature = "nostr")]
    let router = router.merge(nostr::routes());

    #[cfg(feature = "activitypub")]
    let router = router.merge(activitypub::routes());

    let router = router.merge(media_proxy_routes::routes());
    // push_routes merge deleted — `fauna.push.*` (push_handlers) is the sole surface.

    // The bridges-management, bridge-feeds, email-filters, and email-send
    // HTTP routes were deleted in the T9+T10 sweep; user clients use the
    // `fauna.bridges.*` / `fauna.email.*` WS-RPC kinds via the typed
    // `fauna-client-bridges::BridgesClient` + `fauna-client-email::
    // EmailClient` seams. The legacy in-nest inbox / email-deliver /
    // email-domain / email-export / email-alias HTTP routes were deleted at
    // the I6 mail-bridge cutover — nest's only mail entry point is the Go
    // `fauna-mail-bridge`. Alias management was rebuilt as the bridge-class
    // `fauna.bridges.{list,create,update,revoke,delete}_account_alias` (+
    // disposable / hits / policy) WS-RPC surface (`bridge_routing_handlers`,
    // `mail-aliases.md`); the old `email_aliases`-table `alias_routes.rs`
    // module was removed as superseded dead code (hub row B24 close,
    // Track B). The email mbox/Maildir
    // export has no WS-RPC replacement and is gone outright.
    #[cfg(feature = "test-hooks")]
    let router = router.merge(push_test_hooks::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(outbound_clock_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(outbound_drain_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(pending_actions_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(recovery_landing_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(version_prune_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(web_blank_site_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(web_domain_activate_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(rescore_drain_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(spam_history_gc_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(post_fanout_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(mta_sts_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(segment_backup_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(snapshot_scheduler_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(custody_hosting_test_hook::routes());
    #[cfg(all(feature = "test-hooks", feature = "bluesky"))]
    let router = router.merge(bluesky::feed_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(tlsa_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(domain_expiry_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(atproto_identity_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(outbound_tlsrpt_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(mass_mailing_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(link_preview_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(bridge_status_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(channel_refusal_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(rpc_hold_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(content_seal_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(content_seal_epoch_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(web_paywall_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(region_relay_test_hook::routes());
    #[cfg(feature = "test-hooks")]
    let router = router.merge(delegation_lease_test_hook::routes());

    let router = router
        .route(
            "/internal/worker/ws",
            get(nest_link::proxy::worker_ws_handler),
        )
        .route(
            "/internal/router-status",
            get(router_status::get_router_status),
        )
        // The internal sidecar WS-RPC channel: the iroh relay sidecar dials
        // here, runs the `fauna.sidecar.hello` token
        // handshake (bound to `SidecarScope::Relay`, attesting its X25519), then
        // fetches its `relay.<apex>` TLS cert as an HPKE-sealed blob it opens with
        // its own X25519 — so it never reads `/data/acme` (`security.md` § UID
        // isolation; `behavior/p2p.md` § Architecture).
        .route("/internal/relay/ws", get(sidecar_channel::relay_ws_handler));
    // /admin/api/worker/status migrated to the `fauna.admin.worker.status`
    // WS-RPC kind (admin_ws_handlers) — HTTP twin deleted in the
    // WS-RPC-everywhere rip-out.

    // The CORS allow-list is client-set + live-applied (Slice 3d, no-config-on-disk):
    // the `AllowOrigin::predicate` closure reads the `AppState.cors_origins` ArcSwap
    // on every cross-origin request, so `fauna.admin.set_cors_origins` takes effect
    // without rebuilding the listener or rebooting. `origin_allowed` collapses an
    // empty list to the built-in `DEFAULT_CORS_ORIGIN`, preserving the fresh-nest
    // posture the old static branch had. `allow_credentials(true)` forbids a
    // wildcard, so a predicate (which echoes the matched origin back) is required.
    let cors_origins = state.cors_origins.clone();
    let cors = tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::predicate(
            move |origin: &axum::http::HeaderValue, _parts: &_| {
                let allowed = cors_origins.load();
                origin
                    .to_str()
                    .map(|o| node_policy_core::origin_allowed(&allowed, o))
                    .unwrap_or(false)
            },
        ))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
        .expose_headers([axum::http::HeaderName::from_static("x-c2pa")])
        .allow_credentials(true);
    let body_limit = tower_http::limit::RequestBodyLimitLayer::new(10 * 1024 * 1024); // 10 MB

    // HSTS. Tells a conforming browser to pin
    // HTTPS for this host for a year, foreclosing first-visit/stripped-redirect
    // sslstrip. Set unconditionally on the shared router: per RFC 6797 § 8.1 a
    // browser MUST *ignore* an STS header received over plain HTTP, so it is a
    // no-op on the localhost/tier_3 plain path and only takes effect over the
    // TLS listener (`serve_tls`). `includeSubDomains` is deliberately OMITTED —
    // the `mail.<domain>` MDA subdomain serves CalDAV/IMAP, and forcing HTTPS on
    // every subdomain needs the coordination the review flagged; the apex pin is
    // the safe, always-correct subset. 1 year (no `preload`).
    let hsts = tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::STRICT_TRANSPORT_SECURITY,
        axum::http::HeaderValue::from_static("max-age=31536000"),
    );

    // Serve static SPA files from static_dir under /app/ with index.html fallback
    // — or the admin's central-origin redirect — wrapped with the SPA-origin security headers (see `mount_spa`).
    let router = mount_spa(
        router,
        state.config.nest.static_dir.as_deref(),
        web_app_origin::live_probe(state.clone()),
    );

    // S6 (`federation.md` § Cross-nest shared folders + channel append —
    // analysis 2026-07-18): the two content-addressed byte **GET** routes get a
    // route-scoped `Access-Control-Allow-Origin: *`, GET/HEAD-only, **no
    // credentials** — a cross-nest member's web app runs on a *foreign*
    // nest's app origin, unknowable to this nest's credentialed allowlist, and
    // the routes are unauthenticated-by-possession already (a ciphertext hash
    // is the capability), so open CORS adds no exposure a non-browser client
    // didn't have. Merged AFTER `.layer(cors)` so the blanket credentialed
    // layer — deliberately untouched for every other route — never stacks a
    // second, conflicting `Access-Control-Allow-Origin` on these responses.
    let public_bytes = axum::Router::new()
        .route("/api/v1/chunks/{hash}", get(chunk_routes::download_chunk))
        .route(
            "/api/v1/manifests/{hash}",
            get(chunk_routes::download_manifest),
        )
        // A fragment-keyed share link's two data reads (`share-links.md` §
        // The private-file extension): ciphertext and an opaque envelope,
        // unauthenticated-by-possession like the two routes above, and read
        // cross-origin by the viewer whenever the admin's web-app origin is
        // central — so the same open, credential-less CORS.
        .route(
            "/share/{token}/manifest",
            get(share_routes::handle_share_manifest),
        )
        .route(
            "/share/{token}/chunk/{index}",
            get(share_routes::handle_share_chunk),
        )
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods([axum::http::Method::GET, axum::http::Method::HEAD]),
        );

    // The OAuth issuer's plane — the two discovery documents, the JWKS and every
    // request-taking endpoint — is open to EVERY origin with no credentials
    // (`authorization-server.md` § The issuer → *Cross-origin access*): a
    // third-party website's origin is unknowable to the credentialed allowlist
    // above, and nothing on the plane is authenticated by cookie or bearer.
    // Merged after `.layer(cors)` for the same reason `public_bytes` is — one
    // `Access-Control-Allow-Origin` per response, never two.
    let oauth_issuer = oauth_issuer_routes::routes(Arc::clone(&state.oauth_limiter));

    router
        .fallback(web_content_or_info)
        .layer(cors)
        .merge(public_bytes)
        .merge(oauth_issuer)
        .layer(body_limit)
        .layer(hsts)
        .with_state(state)
}

/// Mount the SPA static dir at `/app`, wrapped with the defense-in-depth
/// security headers (2026-06-23 isolation & client-attack-surface review;
/// `docs/goal/behavior/web-content-hosting.md` § Same-origin security model,
/// invariant #5).
///
/// The SPA holds the user's raw Ed25519 master secret in `localStorage`
/// (`fauna_secret`), so any inline-script injection or framing of this origin is
/// permanent identity theft. Two protections must be HTTP **headers** (not the
/// `<meta>` CSP the SPA build also carries): the clickjacking lock
/// (`X-Frame-Options: DENY` + CSP `frame-ancestors 'none'` — `frame-ancestors`
/// is ignored inside a `<meta>` CSP, so it can only be enforced here) and the
/// transport hygiene (`X-Content-Type-Options: nosniff`, `Referrer-Policy:
/// no-referrer`). The *resource* CSP (`script-src 'self' 'wasm-unsafe-eval'`,
/// `style-src`, `connect-src`, …) is emitted as a hashed `<meta>` by SvelteKit's
/// `kit.csp` (`apps/fauna-web/svelte.config.js`) — it alone knows its inline
/// bootstrap hash — so it is deliberately NOT duplicated here.
///
/// Scoped to `/app` on purpose: the apex/subdomain web-content fallback
/// (`web_content_or_info`) serves admin-/user-authored HTML on *isolated origins*
/// (invariant #1); a framing/`default-src` lock there would break legitimate
/// authored sites. Factored out of `build_router` so the header wiring is
/// directly testable (`Router<()>` is itself a `Service`) without a full
/// `AppState` — see `tests/spa_security_headers.rs` and
/// `tests/spa_web_app_origin.rs`.
///
/// **What `/app` answers** is the admin's web-app origin choice, read per
/// request through `probe` ([`web_app_origin::app_router`]): the SPA in
/// `static_dir`, or a `302` to the central origin with this nest pre-filled.
/// `/app` is mounted even with no `static_dir` (a nest shipping no SPA): it is
/// reserved either way, so bundled then answers 404 rather than falling through
/// to the web-content fallback (invariant 3), and central still redirects.
pub fn mount_spa<S>(
    router: axum::Router<S>,
    static_dir: Option<&str>,
    probe: web_app_origin::WebAppOriginProbe,
) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let app = web_app_origin::app_router(static_dir, probe);
    router.nest_service("/app", with_spa_headers(app))
}

/// One header-setting layer of [`with_spa_headers`].
type SpaHeader<S> = tower_http::set_header::SetResponseHeader<S, axum::http::HeaderValue>;

/// Wrap a service answering the SPA's origin in the invariant-#5 headers
/// [`mount_spa`] documents. Shared by `/app` and the share-link viewer's
/// navigation (`share_routes::private_viewer`), which `share-links.md` § The
/// private-file extension serves "under the same reserved-path rule and
/// security headers as `/app/`" — one list, so the two cannot drift.
pub(crate) fn with_spa_headers<S>(svc: S) -> SpaHeader<SpaHeader<SpaHeader<SpaHeader<S>>>> {
    use tower_layer::Layer;

    // Apply innermost → outermost; each layer inserts one response header and
    // preserves the inner `Infallible` error + response body, so the wrapped
    // service still satisfies `nest_service`'s bounds.
    let svc = tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::X_FRAME_OPTIONS,
        axum::http::HeaderValue::from_static("DENY"),
    )
    .layer(svc);
    let svc = tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    )
    .layer(svc);
    let svc = tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::REFERRER_POLICY,
        axum::http::HeaderValue::from_static("no-referrer"),
    )
    .layer(svc);
    tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static("frame-ancestors 'none'"),
    )
    .layer(svc)
}

/// Boot-time guard: a deploy that
/// configures a **public** domain must not serve the API over plain HTTP, or it
/// silently hands every bearer token and request body to the network in
/// cleartext. A **local** domain (`localhost`, LAN IPs, the tier_3 e2e nests)
/// legitimately serves plain HTTP and is allowed; only a public domain with TLS
/// off is refused. The publicness test reuses the exact predicate the
/// enable-email default uses (`resolve_handle_domain(d).is_public_dns_name`)
/// so the notion of "public" is uniform across the nest.
fn refuse_plain_http_for_public_domain(domain: Option<&str>, tls_enabled: bool) -> Result<()> {
    if tls_enabled {
        return Ok(());
    }
    if let Some(d) = domain
        && fauna_provisioning::probe::resolve_handle_domain(d).is_public_dns_name
    {
        anyhow::bail!(
            "refusing to serve the API over plain HTTP: a public domain ({d}) is \
             configured but TLS is not enabled — cleartext would expose bearer tokens \
             and request bodies. Provision a certificate / enable ACME, or unset the \
             public domain for a local-only deploy."
        );
    }
    Ok(())
}

/// Whether this nest's client-facing listener is fronted by a separate proxy
/// (the `fauna-sni-router`, which owns the external `:443` and forwards to nest's
/// internal port). Detected from the `FAUNA_FRONTED_BY_ROUTER` IPC env the Docker
/// image's nest run-script sets (bucket-2 artifact-wiring — no operator). When
/// fronted, the external client-facing port is realized by the router + the
/// compose port-map + the provisioning orchestrator, so the admin-chosen
/// `serving_port` singleton is **inert for nest's own bind** (it still rides
/// `setup.status`). Absent (Windows desktop service / bare-metal `--bind` /
/// bare-IP-direct) ⇒ a direct listener whose bind IS the external port, so the
/// singleton overrides it. A bare `=0` / empty value counts as unset (off). See
/// `docs/goal/architecture/nest/common.md` § Serving ports.
pub(crate) fn is_fronted_by_router() -> bool {
    std::env::var_os("FAUNA_FRONTED_BY_ROUTER")
        .is_some_and(|v| !v.is_empty() && v != std::ffi::OsStr::new("0"))
}

/// Boot-resolve the effective client-facing bind address: on a direct-listener
/// deployment (`fronted == false`) the admin's client-set `serving_port`
/// singleton overrides the `--bind`/`listen` seed's **port** (the interface/host
/// stays from the seed); behind the SNI router (`fronted == true`, from
/// [`is_fronted_by_router`]) the seed is returned unchanged (the singleton is
/// inert for nest's own bind). Apply-on-restart: the nest cannot hot-rebind its
/// own `TcpListener`. `fronted` is a param (not read inline) so the resolve logic
/// is unit-testable without env-var races. See `nest/common.md` § Serving ports.
async fn resolve_serving_bind_addr(
    seed: std::net::SocketAddr,
    db: &db::CacheDb,
    fronted: bool,
) -> std::net::SocketAddr {
    if fronted {
        return seed;
    }
    let mut addr = seed;
    addr.set_port(node_policy_core::resolve_serving_port(db, seed.port()).await);
    addr
}

/// Whether an external-listener bind failure on a *resolved* serving port should
/// **fall back to the bind seed** (keeping the nest up) instead of being fatal.
///
/// Recoverability invariant (`nest/common.md` § Serving ports ⚠ + § Client-state
/// recoverability): `fauna.admin.set_serving_port` accepts any `1..=65535`, so an
/// admin on an **unprivileged** direct-listener (a per-user macOS LaunchAgent, a
/// Linux systemd unit without `CAP_NET_BIND_SERVICE`) can pick a privileged
/// (`<1024`) or already-taken port the process cannot bind → `EACCES` / `AddrInUse`
/// → `KeepAlive` relaunch → **permanent crash-loop** with the bad singleton
/// persisted in `nest.db`, **client-unrecoverable** (the nest never comes up to
/// accept a corrected value). Falling back to the seed keeps the nest reachable so
/// a client can re-choose. Fall back **only** when both hold:
///   - the failed port is the admin's **resolved choice**, not the seed itself
///     (`resolved != seed`) — a seed the artifact itself can't bind is a genuine
///     deploy fault, not client-induced, so propagating it is correct; and
///   - the error is a bind **permission/occupancy** failure (`EACCES` /
///     `AddrInUse`) — any other error is a real fault, not an "unbindable port".
///
/// Pure (takes the resolved/seed ports + the error) so the decision is unit-tested
/// without binding real sockets. The macOS machine-daemon re-shape independently
/// lets the nest bind `:443` via launchd socket activation (removing the most
/// common trigger), but this fallback is still required for genuinely-unbindable
/// ports on any direct-listener.
fn serving_port_bind_should_fall_back(
    resolved_port: u16,
    seed_port: u16,
    err: &std::io::Error,
) -> bool {
    resolved_port != seed_port
        && matches!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::AddrInUse
        )
}

/// The fixed internal-loopback port the artifact asks this nest to *also* bind for
/// co-located IPC, from the `FAUNA_INTERNAL_LOOPBACK_PORT` env (bucket-2 IPC — the
/// sibling of [`is_fronted_by_router`]'s `FAUNA_FRONTED_BY_ROUTER`). The in-process
/// Windows nest-service sets it to [`fauna_protocol::node_policy::CANONICAL_INTERNAL_LOOPBACK_PORT`]
/// so the bridge + same-box app reach the nest on a port that survives a
/// `serving_port` change; absent on Docker (already fronted, so its sole
/// `0.0.0.0:3000` bind is the co-located port) / dev / e2e / bare-metal. A bare
/// `=0` / empty / unparseable value counts as unset. See `nest/common.md`
/// § Serving ports.
fn internal_loopback_port_from_env() -> Option<u16> {
    std::env::var("FAUNA_INTERNAL_LOOPBACK_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|&p| p != 0)
}

/// Resolve the optional additive internal-loopback listener address for a
/// direct-listener nest: a desktop-direct box binds `127.0.0.1:<internal_port>`
/// *alongside* its external `serving_port` listener so co-located processes dial a
/// stable port that never moves when the admin changes `serving_port` (DoD #6 —
/// "no port change strands a client"). Returns `None` when no internal port is
/// requested, or when it equals the external bind's port (the external bind already
/// serves that loopback port — a second bind would `EADDRINUSE`). `internal_port`
/// is a param (not read inline) so the decision is unit-testable without env races,
/// mirroring [`resolve_serving_bind_addr`]'s `fronted` flag. See `nest/common.md`
/// § Serving ports.
fn resolve_internal_loopback_addr(
    external: std::net::SocketAddr,
    internal_port: Option<u16>,
) -> Option<std::net::SocketAddr> {
    let port = internal_port?;
    if port == external.port() {
        return None;
    }
    Some(std::net::SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        port,
    ))
}

/// Build the per-actor (authenticated client) RPC dispatch table.
///
/// The single source of truth for which `fauna.*` / `bluesky.*` kinds an
/// authenticated WS connection can dispatch. Extracted from the `AppState`
/// literal so the central capability gate's coverage test
/// (`bridge_method_allowlist::tests::every_registered_kind_is_gated`) builds the
/// *exact same* router `start_server` does — a kind added here without a matching
/// `bridge_method_allowlist::is_permitted` arm fails that test, which is the
/// tripwire that keeps the "every method is on the per-role allowlist" chokepoint
/// (`docs/goal/architecture/apps/bridges.md` § Why this shape) honest.
///
/// Each per-area module exposes `register_<area>_handlers(builder)`; this
/// composes them. Feature-gated clusters (`bluesky` / `nostr`) register only when
/// their feature is on, exactly as the live router does.
pub(crate) fn build_rpc_router() -> rpc_router::RpcRouter {
    // Per-area handler registrations. Plan 3 ships only protocol kinds.
    // Bridges + future feature crates extend this list.
    let mut b = rpc_router::RpcRouter::builder();
    #[cfg(feature = "test-hooks")]
    protocol_test::register_protocol_handlers(&mut b);
    auth_handlers::register_auth_handlers(&mut b);
    discovery_handlers::register_discovery_handlers(&mut b);
    account_handlers::register_account_handlers(&mut b);
    claim_handlers::register_claim_handlers(&mut b);
    account_handlers::register_account_user_handlers(&mut b);
    domain_expiry_handlers::register_domain_handlers(&mut b);
    profile_handlers::register_profile_handlers(&mut b);
    pending_action_handlers::register_pending_actions_handlers(&mut b);
    invite_handlers::register_invite_handlers(&mut b);
    nat_mode_handlers::register_nat_mode_handlers(&mut b);
    node_policy_handlers::register_node_policy_handlers(&mut b);
    host_maintenance::register_host_maintenance_handlers(&mut b);
    bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
    bridge_blob_handlers::register_capability_handlers(&mut b);
    backup_handlers::register_backup_handlers(&mut b);
    custody_hosting_handlers::register_custody_hosting_handlers(&mut b);
    custody_receipt_handlers::register_custody_receipt_handlers(&mut b);
    recovery_handlers::register_recovery_handlers(&mut b);
    labeler_handlers::register_labeler_handlers(&mut b);
    dns_handlers::register_dns_handlers(&mut b);
    tls_handlers::register_tls_handlers(&mut b);
    oauth_issuer_handlers::register_oauth_issuer_handlers(&mut b);
    principals_handlers::register_principals_handlers(&mut b);
    plugins_handlers::register_plugins_handlers(&mut b);
    oauth_consent_handlers::register_oauth_consent_handlers(&mut b);
    bridge_routing_handlers::register_bridge_routing_handlers(&mut b);
    bridge_atproto_handlers::register_bridge_atproto_handlers(&mut b);
    bridge_list_handlers::register_bridge_list_handlers(&mut b);
    bridge_imap_handlers::register_bridge_imap_handlers(&mut b);
    bridge_import_handlers::register_bridge_import_handlers(&mut b);
    bridge_export_handlers::register_bridge_export_handlers(&mut b);
    bridge_caldav_handlers::register_bridge_caldav_handlers(&mut b);
    bridge_carddav_handlers::register_bridge_carddav_handlers(&mut b);
    bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    bridged_conversation_handlers::register_bridged_conversation_handlers(&mut b);
    drafts_handlers::register_drafts_handlers(&mut b);
    content_index_handlers::register_content_index_handlers(&mut b);
    mls_replica_handlers::register_mls_replica_handlers(&mut b);
    delegation_handlers::register_delegation_handlers(&mut b);
    transport_policy_handlers::register_transport_policy_handlers(&mut b);
    conversations_handlers::register_conversations_handlers(&mut b);
    email_handlers::register_email_handlers(&mut b);
    posts_handlers::register_posts_handlers(&mut b);
    link_preview_handlers::register_link_preview_handlers(&mut b);
    media_ticket::register_media_ticket_handlers(&mut b);
    feed_handlers::register_feed_handlers(&mut b);
    personalization_handlers::register_personalization_handlers(&mut b);
    notifications_handlers::register_notifications_handlers(&mut b);
    contacts_handlers::register_contacts_handlers(&mut b);
    inbox_handlers::register_inbox_handlers(&mut b);
    search_handlers::register_search_handlers(&mut b);
    spam_handlers::register_spam_handlers(&mut b);
    moderation_handlers::register_moderation_handlers(&mut b);
    family_handlers::register_family_handlers(&mut b);
    stats_handlers::register_stats_handlers(&mut b);
    files_handlers::register_files_handlers(&mut b);
    web_handlers::register_web_handlers(&mut b);
    label_handlers::register_labels_handlers(&mut b);
    push_handlers::register_push_handlers(&mut b);
    session_handlers::register_sessions_handlers(&mut b);
    folder_handlers::register_folders_handlers(&mut b);
    folder_deposit::register_folder_deposit_handlers(&mut b);
    webdav_principal_admission::register_webdav_principal_admission_handler(&mut b);
    events_doors::register_events_handlers(&mut b);
    share_handlers::register_share_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
    media_handlers::register_media_handlers(&mut b);
    subscription_handlers::register_subscription_handlers(&mut b);
    #[cfg(feature = "payments")]
    payment_handlers::register_payment_handlers(&mut b);
    #[cfg(feature = "payments")]
    tip_handlers::register_tip_handlers(&mut b);
    // NOT `payments`-gated — the feature plane gates three registry members and
    // its transparency read answers for whichever ones a build ships.
    feature_gate::register_features_handlers(&mut b);
    // The region tier's one admin surface: which region claims this deployment
    // (`region-blocking.md` § Region determination — declared, never detected).
    region_tier::register_region_handlers(&mut b);
    // The admin's web-app origin choice: what this nest's `/app` answers.
    web_app_origin::register_web_app_origin_handlers(&mut b);
    region_relay::register_region_relay_handlers(&mut b);
    crate::segments::register_segments_handlers(&mut b);
    crate::filesync_handlers::register_filesync_handlers(&mut b);
    pair_handlers::register_pair_handlers(&mut b);
    admin_ws_handlers::register_admin_handlers(&mut b);
    // Bluesky-native thread view (`bluesky.feed.thread`) — the one
    // protocol-unique consume-side Bluesky kind; only wired under the
    // `bluesky` feature.
    #[cfg(feature = "bluesky")]
    bluesky::bluesky_handlers::register_bluesky_handlers(&mut b);
    // NIP-46 bunker control plane (`fauna.nostr.bunker.*`) — the user's
    // connected-apps roster (`docs/goal/ui/nostr.md` § The nest as the
    // user's NIP-46 signer).
    #[cfg(feature = "nostr")]
    nostr::bunker_handlers::register_nostr_bunker_handlers(&mut b);
    // Protocol-native Nostr content (`nostr.{zaps.total,badges.list,
    // events.publish_signed}`) — the WS-RPC successors to the deleted
    // `/api/v1/nostr/{zaps,badges,publish-signed}` HTTP routes (the
    // native-content HTTP→WS-RPC rip, `docs/goal/ui/nostr.md` § WS-RPC
    // migration contract).
    #[cfg(feature = "nostr")]
    nostr::content_handlers::register_nostr_content_handlers(&mut b);
    // NIP-57 zap trust root (`fauna.nostr.zap_signers.*`) — the payee's
    // designated signer list, which decides which zap receipts either
    // ingress point may believe (`docs/goal/behavior/monetization.md`
    // § Zap receipts — the trust model).
    #[cfg(all(feature = "nostr", feature = "zaps"))]
    nostr::zap_signer_handlers::register_nostr_zap_signer_handlers(&mut b);
    b.build()
}

/// Start the Node server. Returns the address it is listening on.
///
/// When `tls_config` is `Some`, incoming connections are wrapped with TLS via
/// `tokio_rustls::TlsAcceptor` and served through `hyper_util`.  When `None`,
/// plain HTTP is served via `axum::serve` as before.
#[allow(clippy::too_many_arguments)]
pub async fn start_server(
    bind_addr: std::net::SocketAddr,
    // An optional **pre-bound** external client-facing listener. `Some` on the
    // macOS machine-daemon launchd **socket-activation** path: root launchd
    // pre-binds the privileged `:443` and hands the listening fd to the non-root
    // `_fauna` daemon, which cannot bind `:443` itself (`installers/macos.md`
    // § Network-reachable nest, `nest/common.md` § Serving ports). When `Some`,
    // it is used verbatim as the external listener and the `bind_addr`
    // resolve+bind+seed-fallback below is skipped (launchd owns that port); the
    // internal-loopback listener + everything else are unaffected. `None` is the
    // standalone / Windows / direct-bind path — bind `bind_addr` ourselves with
    // the privileged-port bind-fallback.
    external_listener: Option<tokio::net::TcpListener>,
    db: Arc<db::CacheDb>,
    // Tier-quota enforcement — set by the deployment **artifact**, never by a human
    // (`principles.md` § One configuration surface, bucket 1). The standalone server
    // passes `true`; the embedded single-user desktop nest passes `false`. Not a
    // registration gate: see `AppState::enforce_tier_quotas`.
    enforce_tier_quotas: bool,
    backup_service: Option<Arc<backup::service::BackupService>>,
    token_store: Arc<token_store::TokenStore>,
    tls_config: Option<Arc<rustls::ServerConfig>>,
    // Live SPKI source for the `fauna.auth.handshake` channel-binding leg — the
    // nest's own listener cert resolver. `None` on a plain-HTTP nest.
    served_cert_spki: Option<Arc<dyn acme::ServedCertSpki>>,
    registration: routes::RegistrationConfig,
    nest_config: Arc<config::NestConfig>,
    // Where this artifact keeps the Bluesky OAuth keypair, or `None` to run
    // without the bridge. NOT a built client: the `client_id` derives from the
    // identity domain, learned at claim (`state::BlueskyState`).
    #[cfg(feature = "bluesky")] bluesky_keypair_path: Option<std::path::PathBuf>,
    push_service: Option<Arc<push::PushService>>,
    services_json_path: std::path::PathBuf,
    sidecar_tokens: std::collections::HashMap<String, Vec<sidecar_tokens::SidecarScope>>,
) -> Result<(
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
    Arc<AppState>,
)> {
    let chunk_blob = backup_service.as_ref().map(|svc| svc.local_blob_store());
    // The resolver decodes blob-store bytes (encode_blob format) back to raw
    // before serving them to readers, and encodes raw relayed answers
    // before caching — so it mirrors the BackupService's at-rest encode
    // params (key + compression). None/false on a no-blob-store deploy.
    let (chunk_key, chunk_compression) = backup_service
        .as_ref()
        .map(|svc| (svc.encryption_key().cloned(), svc.compression()))
        .unwrap_or((None, false));
    let chunk_resolver = Arc::new(chunk_relay::ChunkResolver::new(
        chunk_blob,
        chunk_key,
        chunk_compression,
    ));
    let (feed_event_tx, feed_event_rx) = tokio::sync::mpsc::channel(256);
    // Local-aggregate transition signal → the federation exchange originator
    // (see `AppState::exchange_transition_tx`). Created here so both the
    // AppState (senders at the mutation sites) and the worker spawn below
    // share one channel.
    let (exchange_transition_tx, exchange_transition_rx) = tokio::sync::watch::channel(0u64);
    // The same Arc<Mutex<TlsrptAggregator>> is shared between the outbound
    // delivery path's per-host recorder, the daily emitter at 00:00 UTC,
    // and the test-hooks endpoints. Constructed before AppState so all
    // downstream call sites clone the identical Arc.
    let tlsrpt_aggregator: std::sync::Arc<
        std::sync::Mutex<fauna_mail::outbound::tlsrpt::TlsrptAggregator>,
    > = std::sync::Arc::new(std::sync::Mutex::new(
        fauna_mail::outbound::tlsrpt::TlsrptAggregator::default(),
    ));
    #[cfg(feature = "nostr")]
    let (nostr_relay_tx, _nostr_relay_rx) = tokio::sync::broadcast::channel(256);
    #[cfg(feature = "nostr")]
    let (nostr_sync_tx, nostr_sync_rx) = tokio::sync::mpsc::channel(256);
    // Capacity 1: wakes coalesce — one pending nudge already means "reconcile
    // as soon as you wake", and the reconciler reads current state, not the
    // nudge.
    #[cfg(feature = "nostr")]
    let (nostr_bunker_wake_tx, nostr_bunker_wake_rx) = tokio::sync::mpsc::channel(1);
    #[cfg(feature = "activitypub")]
    let ap_delivery_nudge = Arc::new(tokio::sync::Notify::new());
    // Shutdown signal for long-running background tasks (discovery poller,
    // workers). The sender must outlive the receivers — dropping it here would
    // make `shutdown_rx.changed()` resolve `Err` immediately every poll, and
    // the discovery poller's `select!` would spin its worker thread at 100%
    // CPU instead of sleeping. We never trigger a graceful shutdown today, so
    // we leak the sender to keep the channel open for the program lifetime.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    Box::leak(Box::new(shutdown_tx));

    // The serving-generation scope (`box-recovery.md` § Deployment-seed
    // rotation → *Adoption by the running process*): minted per `start_server`
    // call, moved into `AppState`, and cancelled+awaited by the serve loop's
    // teardown before re-entering. Minted here — before the first long-lived
    // spawn below — so pre-AppState spawns (the update loop) can join it.
    let serve_generation = tokio_util::sync::CancellationToken::new();
    let serve_tasks = tokio_util::task::TaskTracker::new();
    let serve_restart = Arc::new(tokio::sync::Notify::new());

    let nest_signing_key = match db.get_nest_keypair().await {
        Ok(Some((secret, _public))) => {
            let secret_arr: [u8; 32] = secret.as_slice().try_into().expect("nest key is 32 bytes");
            Some(ed25519_dalek::SigningKey::from_bytes(&secret_arr))
        }
        _ => None,
    };

    // The single-row key-encryption satellites — the room-read keypair, the
    // OAuth issuer key's active signer and the OAuth session secret — and a
    // DKIM key for every mail domain that has none mint HERE,
    // before this generation serves anything, and never on a read (`nest_kek`'s
    // module docs; `key-material-hierarchy.md` § Room-read keypair → *When it
    // mints*). Both boot paths and every serving-generation re-entry pass
    // through, so a nest upgrading into the rule mints on that restart and a
    // rotation's successor finds the rows already re-keyed. Logged per member,
    // not fatal: every consumer answers `NotProvisioned` honestly (nest.info
    // omits the key, the issuer surfaces refuse) and the next start tries again.
    match nest_kek::mint_at_boot(&db).await {
        Ok(Some(minted)) => {
            for (member, e) in minted.failures() {
                tracing::error!("{member} boot mint failed: {e:#}");
            }
        }
        // No deployment keypair to seal under; the identity fallback below logs it.
        Ok(None) => {}
        Err(e) => tracing::error!("key-encryption satellite boot mint failed: {e:#}"),
    }

    // The nest's SINGLE identity is a view over the deployment signing key
    // (`box-recovery.md` § Single-identity unification, decision 2026-06-29). Both
    // boot paths reconcile `nest_deployment.key` into the `nest_keypair` row before
    // reaching here (`main.rs` / `desktop_serve.rs`), so `nest_signing_key` holds
    // the channel-binding deployment seed; derive `nest_identity` from it so
    // `fauna.nest.info`, federation, the backup-destination pubkey, multi-nest
    // pairing, and the nest's own sync `device_id`/`service_id` all key off the one
    // custodied seed. The `None` arm is a defensive fallback only (reconcile
    // guarantees a row) — a fresh random identity beats panicking.
    let nest_identity = Arc::new(match &nest_signing_key {
        Some(sk) => nest_identity::NestIdentity::from_seed(&sk.to_bytes()),
        None => {
            tracing::error!(
                "no deployment keypair present after boot reconcile; using an ephemeral random \
                 nest identity (nest.info/federation/sync will not match the pinned identity)"
            );
            nest_identity::NestIdentity::generate()
        }
    });
    tracing::info!(
        "Nest identity (deployment key): {}",
        hex::encode(nest_identity.public_key_bytes())
    );

    let claim_code_path = claim::claim_code_path_for_db(&nest_config.nest.db_path);
    // Boot reconcile: `ensure_claim_code_at` (pre-db startup) regenerates the
    // claim code whenever the file is absent, including on every restart of an
    // already-claimed box. Now that the db is open, delete any such resurrected
    // code (best-effort hygiene — `setup.status.claimed` is now DB-positive, so a
    // lingering code no longer wedges it). Pass whether an *admin* exists (not
    // merely any user): a half-completed claim that crashed after the `users`
    // insert but before `add_admin_actor` must NOT have its still-live claim code
    // deleted here, or the box is wedged (a user but no admin, and no code to
    // retry with). `admin_count > 0` is the same DB-positive claimed signal the
    // `already_claimed` gate + `setup.status.claimed` use.
    claim::reconcile_claim_code(db.admin_count().await.unwrap_or(0) > 0, &claim_code_path);

    // There is no storage mode to resolve: every nest is sealed at rest, content-
    // ready from the first instruction (`nest/storage-modes.md` § Boot story).

    // Resolve the client-set NAT axis: the `nest_nat_mode` row wins, falling
    // back to the `config.nest.mode` seed (`FAUNA_MODE`) when absent. `main`
    // resolved the same row for its pre-`AppState` ACME/STUN/HTTP-01 decisions;
    // this populates the live `AppState.node_mode` every runtime reader uses.
    let resolved_node_mode = nat_mode_core::resolve_node_mode(&db, &nest_config).await;
    tracing::info!("nat mode: {}", resolved_node_mode.as_str());

    // Resolve the client-set `[nest]`-policy toggles: the `nest_subhandles` /
    // `nest_max_storage_bytes` / `nest_cors_origins` DB row wins, else the
    // `config.nest.*` boot seed. These populate the live `AppState` RwLock fields
    // every runtime reader uses — never `config.nest.*` directly (the config value
    // is only the pre-claim seed). See `node_policy_core`.
    //
    // `enforce_tier_quotas` is deliberately NOT in this family: it is artifact-set
    // IPC, not an admin choice, so it has no DB row and no admin kind.
    //
    // The registration posture: the client-set row wins, else the `[nest]` seed.
    // An absent or unparseable seed resolves `closed` — never open (a box that
    // boots before its admin has picked a posture must not admit strangers).
    let registration_mode_seed = (
        nest_config
            .nest
            .registration_mode
            .as_deref()
            .and_then(fauna_protocol::node_policy::RegistrationMode::from_wire_str)
            .unwrap_or(fauna_protocol::node_policy::DEFAULT_REGISTRATION_MODE),
        nest_config.nest.max_free_users,
    );
    let resolved_registration_mode =
        node_policy_core::resolve_registration_mode(&db, registration_mode_seed).await;
    let resolved_subhandles =
        node_policy_core::resolve_subhandles(&db, nest_config.nest.subhandles).await;
    let resolved_age_verification_required =
        node_policy_core::resolve_age_verification_required(&db).await;
    let resolved_max_storage_bytes =
        node_policy_core::resolve_max_storage_bytes(&db, nest_config.nest.max_storage_bytes).await;
    // The CORS allow-list is the list member of the same family: the
    // `nest_cors_origins` DB row wins, else the `config.nest.cors_origins` seed
    // (which covers both the `--cors-origins` CLI and a TOML `[nest]` config).
    let resolved_cors_origins =
        node_policy_core::resolve_cors_origins(&db, nest_config.nest.cors_origins.clone()).await;
    // What `/app` answers: the `nest_web_app_origin` row, else bundled (there is
    // no seed — the choice is app-set only).
    let resolved_web_app_origin = web_app_origin::resolve_web_app_origin(&db).await;
    // The deployment's identity domain: a projection of the primary `mail_domains`
    // row (the single source of truth — that row IS the identity), else the
    // `config.nest.domain` seed (an admin `--domain` pin; `FAUNA_DOMAIN` is
    // retired), else `None` (a domainless box that learns its domain at claim).
    // Initializes the sync `identity_domain` cache read by `handle_domain()` /
    // `web_serving_domain()` + the boot-built HostResolver/web-cert apex (a box that
    // boots already-claimed picks up its domain here; a domain claimed post-boot
    // drives web routing after restart — see § Implementation status).
    let resolved_identity_domain =
        identity_domain_core::resolve_identity_domain(&db, &nest_config).await;

    // Boot-resolve the client-facing serving port (Pillar A — apply-on-restart).
    // On a direct-listener deployment (Windows desktop service / bare-metal
    // `--bind` / bare-IP-direct) the admin's client-set `serving_port` singleton
    // overrides the `--bind`/`listen` seed's PORT; the interface/host stays from
    // the seed (artifact-wiring). Behind the SNI router (Docker, which sets
    // `FAUNA_FRONTED_BY_ROUTER`) the whole bind is artifact-wiring and the
    // singleton is inert here — the external port is realized by the router +
    // compose port-map + the provisioning orchestrator. The nest cannot hot-rebind
    // its own `TcpListener`, so a later `set_serving_port` applies on the next
    // (supervisor-driven) restart. See `nest/common.md` § Serving ports.
    let original_bind_port = bind_addr.port();
    let bind_addr = resolve_serving_bind_addr(bind_addr, &db, is_fronted_by_router()).await;
    if bind_addr.port() != original_bind_port {
        tracing::info!(
            "client-facing serving port: {} (admin-set serving_port overrides the bind seed's {})",
            bind_addr.port(),
            original_bind_port
        );
    }

    let subscription_mls = if let Some(ref signing_key) = nest_signing_key {
        let secret_bytes = signing_key.to_bytes();
        let keypair = fauna_core::identity::ActorKeypair::from_secret(secret_bytes);
        let main_db = std::path::Path::new(&nest_config.nest.db_path);
        let mls_db_path = main_db.with_extension("").with_file_name(format!(
            "{}_subscription_mls.db",
            main_db.file_stem().unwrap_or_default().to_string_lossy()
        ));
        match fauna_mls::engine::MlsEngine::new(keypair, &mls_db_path) {
            Ok(engine) => Some(Arc::new(engine)),
            Err(e) => {
                tracing::warn!("subscription MLS init failed: {e}");
                None
            }
        }
    } else {
        None
    };

    // Built here rather than in the `AppState` literal below, which moves `db`.
    #[cfg(feature = "bluesky")]
    let bluesky_state = match bluesky_keypair_path {
        Some(path) => state::BlueskyState::new(path, Arc::clone(&db)),
        None => state::BlueskyState::default(),
    };

    // `mut` is used only when a bridge feature is enabled; the default
    // (no-bridge) build never registers a provider, so silence unused-mut.
    #[allow(unused_mut)]
    let mut bridge_registry = bridge_management::BridgeProviderRegistry::new();
    #[cfg(feature = "nostr")]
    bridge_registry.register(Box::new(nostr::bridge_provider::NostrProvider));
    #[cfg(feature = "bluesky")]
    bridge_registry.register(Box::new(bluesky::bridge_provider::BlueskyProvider));
    #[cfg(feature = "activitypub")]
    bridge_registry.register(Box::new(activitypub::bridge_provider::ActivityPubProvider));

    // Spawn update check loop (check-only, no auto-apply for bare-metal nests).
    let update_rx = if nest_config.update.check {
        let update_config = fauna_update::UpdateConfig {
            github_repo: fauna_core::version::RELEASE_REPO,
            current_version: env!("CARGO_PKG_VERSION"),
            artifact_prefix: "fauna-nest",
            check_interval: std::time::Duration::from_secs(86400),
            auto_apply: false,
            install_dir: std::path::PathBuf::from("/usr/local/bin"),
            github_token: nest_config.update.github_token.clone(),
        };
        // The generation token IS the update loop's shutdown signal: an old
        // generation's checker dies at teardown instead of surviving as a
        // duplicate (its cadence is 24 h, so tracker coverage is not needed).
        fauna_update::spawn_update_loop(update_config, serve_generation.clone())
    } else {
        let (_tx, rx) = tokio::sync::watch::channel(None);
        rx
    };

    let payload_store = backup_service.as_ref().map(|svc| {
        Arc::new(crate::payload_store::PayloadStore::new(
            svc.local_blob_store(),
            db.clone(),
            64 * 1024,
        ))
    });

    let security_notifier = Arc::new(crate::security_notify::SecurityNotifier::new(
        db.clone(),
        nest_identity.clone(),
    ));

    // Phase D1.3: derive the ACME cert directory the same way main.rs does.
    // Prefer [acme].dir from config; fall back to the default (/var/lib/fauna/acme).
    let acme_dir: std::path::PathBuf = nest_config
        .acme
        .as_ref()
        .and_then(|a| a.dir.as_deref())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/var/lib/fauna/acme"));

    // Always-live self-signed floor (tls-certificates.md § A): write it on EVERY
    // nest entry path, here in the shared `start_server` init. `main.rs`'s
    // `prepare_listener_tls` already ensures the floor before building its own TLS
    // listener, but callers that invoke `start_server` directly with no TLS — the
    // Windows `fauna-nest-service` (loopback plain-HTTP) and the plain-HTTP e2e
    // harness — never reach that path, so without this their in-process mail
    // bridges would bind CalDAV/IMAP with no floor cert to fetch+serve
    // (caldav-imap-any-locator). Idempotent (no-op when a cert already exists), so
    // it's harmless on the `main.rs` TLS path that already wrote one. The floor's
    // subject is the configured ACME domain when present, else a domainless
    // loopback floor (the case for the Windows/e2e callers that trigger this).
    {
        // Same canonical node-domain source `main.rs` passes to
        // `prepare_listener_tls` (`nest_config.nest.domain`) — `None`/empty for the
        // domainless Windows/e2e callers that actually trigger this write.
        let floor_domain = nest_config.nest.domain.as_deref().filter(|d| !d.is_empty());
        crate::self_signed_cert::ensure_floor_present(&acme_dir, floor_domain);
    }

    // The one storage impl, live from boot — no mode to resolve, nothing to swap.
    let storage = Arc::new(crate::storage::SealedStorage::new(
        db.clone(),
        acme_dir.clone(),
    )) as crate::storage::SharedStorage;

    // Plan 2 T6: per-process mail SegmentManager (kind = "mail"). Owns the
    // append-only segment files + `manifest.mail` under `{data-dir}/__mail/{actor}/`.
    // Same data-dir derivation as main.rs (parent of nest_config.nest.db_path).
    let mail_data_dir = std::path::Path::new(&nest_config.nest.db_path)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    let mail_segments = Arc::new(fauna_segment_store::SegmentManager::new(
        mail_data_dir.clone(),
        "mail",
    ));
    let conv_segments = Arc::new(fauna_segment_store::SegmentManager::new(
        mail_data_dir.clone(),
        "conv",
    ));
    // Track C posts: per-author post segment store under `{data-dir}/__post/`.
    let post_segments = Arc::new(fauna_segment_store::SegmentManager::new(
        mail_data_dir.clone(),
        "post",
    ));
    // S6.4: per-actor calendar CONTENT segment store under `{data-dir}/__calendar/`.
    // Sibling of `post_segments`; distinct from the `cal_placement` journal below.
    let cal_segments = Arc::new(fauna_segment_store::SegmentManager::new(
        mail_data_dir.clone(),
        "calendar",
    ));
    // S6.5: per-actor card CONTENT segment store under `{data-dir}/__card/`.
    let card_segments = Arc::new(fauna_segment_store::SegmentManager::new(
        mail_data_dir.clone(),
        "card",
    ));
    let mail_placement = Arc::new(crate::segments::MailPlacementSegmentManager::new(
        mail_data_dir.clone(),
    ));
    // Plan 1 T6: per-process CalPlacementSegmentManager. Shares the
    // mail data-dir (calendar placement events live under
    // `{data-dir}/segments/__calendar-placement/`). Spec § D2 / § D3.
    let cal_placement = Arc::new(crate::segments::CalPlacementSegmentManager::new(
        mail_data_dir.clone(),
    ));
    // S6.2: per-process CardPlacementSegmentManager — the CalPlacement
    // twin. Shares the mail data-dir (card placement events live under
    // `{data-dir}/segments/__card-placement/`).
    let card_placement = Arc::new(crate::segments::CardPlacementSegmentManager::new(
        mail_data_dir,
    ));

    // Succession-ownership boot heal (`succession-propagation.md`, the
    // ownership re-point bullet): finish any segment-dir rename a crash
    // interrupted between a succession's commit and its rename, before
    // serving starts. The rows need no pass — the succession transaction moved
    // them. Idempotent; a no-op on a nest with no successions. Covers the
    // placement journals too. Conv is deliberately absent — its scope is the
    // channel, not an actor.
    if let Err(e) = crate::succession_ownership::heal_at_boot(
        &db,
        &[
            &*mail_segments,
            &*post_segments,
            &*cal_segments,
            &*card_segments,
            &*mail_placement,
            &*cal_placement,
            &*card_placement,
        ],
    )
    .await
    {
        tracing::error!("succession-ownership boot heal failed: {e}");
    }
    // Export blobs no session row names (`mail-export.md` § Reclaim). A
    // succession burns a retired identity's `export_sessions` rows in its
    // transaction, and a burn cannot unlink — so the files it orphans are
    // collected here, along with anything a crash left between a row delete
    // and its unlink.
    crate::mail_export_blobs::reclaim_orphaned_export_blobs_best_effort(
        &db,
        &nest_config.nest.db_path,
        "boot",
    )
    .await;

    // T2.1a: MTA-STS policy fetcher for `fauna.bridges.fetch_mta_sts_policy`.
    // nest does the `_mta-sts.<domain>` TXT + `.well-known/mta-sts.txt` GET
    // and wraps a real `LiveMtaStsFetcher` in `CachingMtaStsFetcher` so each
    // domain's policy is fetched once per `max_age` window. A
    // `LiveMtaStsFetcher::new()` failure is non-fatal — fall through to
    // `NullMtaStsFetcher` so the handler attributes `not_published` rather
    // than failing the RPC (mirrors `legacy_smtp_outbound.rs` with the new
    // shared fetcher). Always present (no `test-hooks` gate); the test
    // override is consulted ahead of it inside the handler.
    let mta_sts_fetcher: Arc<dyn fauna_mail::outbound::mta_sts::MtaStsFetcher> =
        match fauna_mail::outbound::mta_sts::LiveMtaStsFetcher::new() {
            Ok(fetcher) => {
                let clock: fauna_mail::outbound::mta_sts::ClockFn =
                    Arc::new(fauna_core::data::Timestamp::now_secs_or_zero);
                Arc::new(fauna_mail::outbound::mta_sts::CachingMtaStsFetcher::new(
                    fetcher, clock,
                ))
            }
            Err(e) => {
                tracing::warn!(
                    "failed to build MTA-STS fetcher: {e}; falling back to no-policy-found"
                );
                Arc::new(fauna_mail::outbound::mta_sts::NullMtaStsFetcher)
            }
        };

    // Public-recursive DNS self-verifier (`fauna.dns.verify_records` +
    // web_content domain check). Reuses the same wall-clock `ClockFn` shape as
    // the MTA-STS fetcher; a resolver-build failure degrades to the null
    // resolver (verification answers "checking") rather than aborting startup.
    let dns_verifier = {
        let clock: fauna_mail::outbound::mta_sts::ClockFn =
            Arc::new(fauna_core::data::Timestamp::now_secs_or_zero);
        let resolver: Arc<dyn crate::dns_verifier::RecordResolver> =
            match crate::dns_verifier::LiveRecordResolver::new() {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    tracing::warn!(
                        "failed to build DNS verify resolver: {e}; verification reports 'checking'"
                    );
                    Arc::new(crate::dns_verifier::NullRecordResolver)
                }
            };
        Arc::new(crate::dns_verifier::DnsVerifier::new(resolver, clock))
    };

    // Web-content hosting: activate the serving layer (previously hardcoded
    // `None` — dormant, see `web-content-hosting.md` § Implementation status
    // today). `web_content_service` needs a blob store, so it tracks
    // `backup_service`'s presence: `None` on a no-blob-store deploy, where
    // `web_content_or_info` degrades to the built-in info page. The
    // `HostResolver` always builds (its apex/custom-domain/subdomain maps are
    // seeded from the db just after the AppState below). Built here, before the
    // literal, because `backup_service` is moved into it as a field. `db` is
    // cloned (the original moves into the `db` field).
    // Web-paywall holder identity (Pillar 2): the web-serve component's own
    // enrolled service-user + live grant registry. Tracks web hosting
    // (`backup_service` = the blob store the serving layer needs) and needs a
    // real data dir for its seed file; `None` (incl. an admin-revoked
    // enrollment or an init failure) darkens paywalled serving to teasers
    // without touching ungated serving. Built BEFORE the render service so the
    // render pipeline can decrypt+seal paywalled full pages under its grants.
    let web_serve_holder = if backup_service.is_some() {
        match crate::mail_enable::data_dir_from_db_path(&nest_config.nest.db_path) {
            Some(data_dir) => {
                match crate::web_content::holder::WebServeHolder::init(&data_dir, db.clone()).await
                {
                    Ok(holder) => holder,
                    Err(e) => {
                        tracing::warn!(
                            "web-serve holder init failed: {e:#}; paywalled serving stays dark"
                        );
                        None
                    }
                }
            }
            None => None,
        }
    } else {
        None
    };
    let web_content_service = backup_service.as_ref().map(|svc| {
        let mut service =
            crate::web_content::service::WebContentService::new(db.clone(), svc.local_blob_store())
                // Post bodies rest in the `__post` segment store after the
                // cutover; render reads segment-first, falling back to the inline
                // `content.payload` body when the post has no decodable author.
                .with_post_body_source(post_segments.clone())
                // Synced files (manifests + chunks) carry the nest's at-rest
                // framing; the serve/render read paths strip it with this key.
                .with_at_rest_key(svc.encryption_key().cloned());
        if let Some(holder) = &web_serve_holder {
            service = service.with_web_serve_holder(holder.clone());
        }
        Arc::new(service)
    });
    // The resolver/cert serving domain keys off the resolved identity domain
    // (the primary `mail_domains` row, set at claim), falling back to
    // `registration.handle_domain ?? node.domain` (empty fallback) for a box that
    // has neither. This captures the apex at boot: a box that boots already-claimed
    // gets its domain here; a domain claimed *post-boot* drives web routing only
    // after the next restart (the apex catch-all still serves — see
    // `domains-and-tls-bootstrap.md` § Implementation status). `main.rs`'s
    // `WebCertConfig.nest_domain` keys off the same resolved value.
    let host_resolver = Some(Arc::new(crate::web_content::serve::HostResolver::new(
        resolved_identity_domain.clone().unwrap_or_else(|| {
            crate::state::web_serving_domain(
                registration.handle_domain.as_deref(),
                nest_config.nest.domain.as_deref(),
            )
        }),
    )));

    let state = Arc::new(AppState {
        ws: Arc::new(ws::WsState::with_durable(Arc::clone(&db))),
        db,
        bridge_push_registry: Arc::new(crate::bridge_push_registry::BridgePushRegistry::new()),
        delegation_leases: Arc::new(crate::delegation_registry::LeaseRegistry::new()),
        delegation_runner_wake: Arc::new(tokio::sync::Notify::new()),
        backup_pass_health: Default::default(),
        spam_baseline_runs: Default::default(),
        succession_hints: Default::default(),
        oauth_as: Arc::new(crate::oauth_as_state::OAuthAsRuntime::production(
            fauna_core::data::Timestamp::now_secs_or_zero(),
        )),
        // Plugins live under the data dir (`plugins/<principal_id>/`), the
        // same derivation as the web-serve holder's seed above.
        plugins: Arc::new(crate::plugin_runner::PluginRunner::new(
            crate::mail_enable::data_dir_from_db_path(&nest_config.nest.db_path)
                .map(|d| d.join("plugins")),
        )),
        oauth_limiter: Arc::new(crate::oauth_as_rate_limit::EndpointLimiter::new()),
        rpc_router: Arc::new(build_rpc_router()),
        federation_router: Arc::new({
            // Serving table for the `fauna.federation.*` kinds a verified peer
            // nest may invoke over the long-lived federation channel (Spec Y2
            // slice 4 §4.C). Its registered kinds ARE the federation kind
            // allowlist. Each handler maps onto the same DB op its HTTP twin
            // does today (`federation_handlers`).
            let mut b = federation_router::FederationRouter::builder();
            federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        federation_pool: Arc::new(federation_pool::FederationChannelPool::new()),
        config: nest_config.clone(),
        nest_identity,
        nest_signing_key,
        served_cert_spki,
        acme_retry_notify: Arc::new(tokio::sync::Notify::new()),
        relay_cert_changed: tokio::sync::watch::Sender::new(0),
        relay_channels: Default::default(),
        serve_generation,
        serve_tasks,
        serve_restart,
        http_client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("failed to build HTTP client"),
        tls_enabled: tls_config.is_some(),
        cors_origins: Arc::new(arc_swap::ArcSwap::from_pointee(resolved_cors_origins)),
        web_app_origin: Arc::new(arc_swap::ArcSwap::from_pointee(resolved_web_app_origin)),
        identity_domain: Arc::new(arc_swap::ArcSwapOption::from(
            resolved_identity_domain.map(Arc::new),
        )),
        acme_dir: acme_dir.clone(),
        backup_service,
        payload_store,
        mail_segments,
        conv_segments,
        post_segments,
        cal_segments,
        card_segments,
        mail_placement,
        cal_placement,
        card_placement,
        feed_event_tx,
        exchange_transition_tx,
        security_notifier,
        update_status: update_rx,

        auth: state::AuthState {
            token_store,
            bulk_byte_tokens: std::sync::Arc::new(crate::bulk_byte_token::BulkByteTokenStore::new()),
            challenge_store: std::sync::Arc::new(challenge_auth::ChallengeStore::new()),
            escrow_challenge_store: std::sync::Arc::new(challenge_auth::ChallengeStore::new()),
            veto_challenge_store: std::sync::Arc::new(challenge_auth::ChallengeStore::new()),
            replay_guard: std::sync::Arc::new(crate::auth_core::ReplayGuard::new()),
            age_nonce_store: std::sync::Arc::new(age_attest::AgeNonceStore::new()),
            registration,
        },
        sync: state::SyncState { chunk_resolver },
        email: state::EmailState {
            inbound_deliver_key: std::env::var("FAUNA_INBOUND_DELIVER_KEY").ok(),
            tlsrpt_aggregator: tlsrpt_aggregator.clone(),
        },
        bridge: state::BridgeState {
            providers: Some(Arc::new(bridge_registry)),
            worker: nest_link::proxy::WorkerState::new(None),
        },
        mls: state::MlsState {
            engine: subscription_mls,
            transition_threshold: 2000,
        },
        #[cfg(feature = "bluesky")]
        bluesky: bluesky_state,
        #[cfg(feature = "nostr")]
        nostr: state::NostrState {
            relay_tx: nostr_relay_tx.clone(),
            sync_tx: nostr_sync_tx,
            bunker_wake_tx: nostr_bunker_wake_tx,
            gift_wrap_limiter: Some(Arc::new(nostr::relay_endpoint::new_gift_wrap_limiter())),
            bunker_limiter: Some(Arc::new(nostr::relay_endpoint::new_bunker_limiter())),
            link_challenges: Default::default(),
            // The production dial policy: every relay this nest reaches —
            // publish lists, bunker strings, follow hints, paired serving
            // boxes — is verified against the shared SSRF guard before a
            // socket opens.
            relay_dial_policy: nostr::relays::relay_dial_policy(),
        },
        #[cfg(feature = "activitypub")]
        activitypub: state::ActivityPubState {
            delivery_nudge: ap_delivery_nudge.clone(),
            // No `domain` snapshot — AP resolves it per request from the
            // claim-refreshed `identity_domain` cache (`ActivityPubState` docs).
            inbox_limiter: Some(Arc::new({
                let quota = governor::Quota::per_second(std::num::NonZeroU32::new(30).unwrap());
                governor::RateLimiter::dashmap(quota)
            })),
        },
        web_content_service,
        host_resolver,
        web_serve_holder,
        push_service,
        services_json_path,
        // Same data-dir derivation as main.rs (parent of nest.db_path); the
        // custody-hosting pump's per-owner custodied stores live under it.
        custody_hosting_root: std::path::Path::new(&nest_config.nest.db_path)
            .parent()
            .map(|d| d.join("custody-hosting")),
        sidecar_tokens,
        bridge_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::new()),
        spam_train_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
            crate::bridge_rate_limit::SPAM_TRAIN_LIMITER_CONFIG,
        )),
        channel_commit_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
            crate::bridge_rate_limit::CHANNEL_COMMIT_LIMITER_CONFIG,
        )),
        records_door_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
            crate::bridge_rate_limit::RECORDS_DOOR_LIMITER_CONFIG,
        )),
        // Default cap; `start_server` applies the boot-resolved value via
        // `set_max` on the TLS path (and hands this same Arc to `serve_tls`).
        per_ip_conn_limit: fauna_conn_limit::PerIpConnLimit::new(
            fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
        ),
        federation_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::new()),
        handler_semaphore: Arc::new(tokio::sync::Semaphore::new(
            crate::dispatch_core::MAX_INFLIGHT_HANDLERS,
        )),
        anonymous_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
            crate::anonymous_rate_limit::default_config(),
        )),
        anonymous_rate_limit_shed: Arc::new(fauna_conn_limit::ShedCounter::new()),
        failed_credential_throttle: Arc::default(),
        claim_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
            crate::anonymous_rate_limit::claim_config(),
        )),
        global_claim_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
            crate::anonymous_rate_limit::global_claim_config(),
        )),
        claim_rate_limit_shed: Arc::new(fauna_conn_limit::ShedCounter::new()),
        invite_verify_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
            crate::anonymous_rate_limit::invite_verify_config(),
        )),
        invite_verify_rate_limit_shed: Arc::new(fauna_conn_limit::ShedCounter::new()),
        register_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
            crate::anonymous_rate_limit::register_config(),
        )),
        register_rate_limit_shed: Arc::new(fauna_conn_limit::ShedCounter::new()),
        invite_request_rate_limit: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
            crate::anonymous_rate_limit::invite_request_config(),
        )),
        invite_request_rate_limit_shed: Arc::new(fauna_conn_limit::ShedCounter::new()),
        node_mode: Arc::new(tokio::sync::RwLock::new(resolved_node_mode)),
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(enforce_tier_quotas)),
        subhandles: Arc::new(tokio::sync::RwLock::new(resolved_subhandles)),
        registration_mode: Arc::new(tokio::sync::RwLock::new(resolved_registration_mode)),
        age_verification_required: Arc::new(tokio::sync::RwLock::new(
            resolved_age_verification_required,
        )),
        max_storage_bytes: Arc::new(tokio::sync::RwLock::new(resolved_max_storage_bytes)),
        storage,
        // Production never overrides the demand-door registry — only
        // `AppState::install_region_registry_for_test` (debug/test builds)
        // ever sets this.
        region_registry_override: None,
        #[cfg(feature = "test-hooks")]
        outbound_clock_override: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        #[cfg(feature = "test-hooks")]
        rescore_worklist_serves: Arc::new(Default::default()),
        #[cfg(feature = "test-hooks")]
        post_fanout_initiations: Arc::new(Default::default()),
        #[cfg(feature = "test-hooks")]
        epoch_sealing_test_override: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        #[cfg(feature = "test-hooks")]
        custody_hosting_periodic_held: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        mta_sts_fetcher,
        dns_verifier,
        #[cfg(feature = "test-hooks")]
        mta_sts_override: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        // T2.1b: DANE/TLSA resolver for `fauna.bridges.fetch_tlsa`. nest does
        // the `_25._tcp.<mx_host>` DNSSEC-validating lookup; the Go MTA bridge
        // pins the handshake against the records. Always present (no
        // `test-hooks` gate); the test override is consulted ahead of it in
        // the handler. Unlike the MTA-STS fetcher there's no fallible
        // construction — `LiveTlsaResolver` builds a fresh DNSSEC resolver per
        // lookup (DANE lookups are infrequent: once per surviving MX host).
        tlsa_resolver: Arc::new(fauna_mail::outbound::dane::LiveTlsaResolver),
        // The MX leg of the same split. Outbound DANE may only bind to
        // a name that came out of a DNSSEC-validated MX RRset (RFC 7672
        // §2.2), and the Go stdlib resolver cannot validate — so nest
        // resolves MX too, and reports the RRset's `secure` provenance
        // alongside the hosts. Same construction shape as `tlsa_resolver`:
        // infallible, a fresh DNSSEC resolver per lookup (one lookup per
        // delivery attempt).
        mx_resolver: Arc::new(fauna_mail::outbound::mx::LiveMxRrsetResolver),
        #[cfg(feature = "test-hooks")]
        tlsa_override: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        // Deliverability-diagnostic outbound-STARTTLS prober (the gmail `:25`
        // posture probe). Stateless; no fallible construction. The e2e/test-hooks
        // build installs the Null prober so a tier_3 run is deterministic + never
        // reaches out to gmail; production installs the live prober.
        #[cfg(not(feature = "test-hooks"))]
        starttls_prober: Arc::new(crate::mail_deliverability::LiveStarttlsProber),
        #[cfg(feature = "test-hooks")]
        starttls_prober: Arc::new(crate::mail_deliverability::NullStarttlsProber),
        // D4 link-preview resolver: real SSRF-safe fetcher + by-url cache +
        // per-actor limiter in both prod and the e2e build; the `test-hooks`
        // fixture override below is what lets the e2e resolve a served OG page.
        link_preview: crate::link_preview::LinkPreviewState::new(),
        #[cfg(feature = "test-hooks")]
        link_preview_override: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        #[cfg(feature = "test-hooks")]
        bridge_status_override: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        #[cfg(feature = "test-hooks")]
        atproto_identity_withheld: Arc::new(
            std::sync::Mutex::new(std::collections::HashSet::new()),
        ),
        #[cfg(feature = "test-hooks")]
        channel_send_refusal: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        #[cfg(feature = "test-hooks")]
        rpc_hold: Arc::new(crate::rpc_hold_test_hook::RpcHoldRegistry::default()),
    });

    // Boot-seed the live HostResolver from persisted state: the admin-designated
    // apex actor (if any) → the node domain, every `active` custom web domain →
    // its owner, and every opted-in actor's `<handle>` → its actor (Slice 3).
    // Live updates thereafter ride the RPC handlers (`fauna.web.set_apex_actor`,
    // `fauna.web.set_subdomain_enabled`, `fauna.web.domain.delete`), the domain
    // lifecycle task's per-pass custom-domain reconcile, and the
    // per-domain/-subdomain cert lifecycle. (web-content-hosting.md § Routing —
    // boot population of the resolver.)
    if let Some(resolver) = &state.host_resolver {
        match state.db.get_apex_actor().await {
            Ok(Some(apex)) => resolver.set_apex_actor(Some(apex)).await,
            Ok(None) => {}
            Err(e) => tracing::warn!("web host resolver: failed to read apex actor at boot: {e}"),
        }
        // The same projection the lifecycle task re-runs every pass, so boot and
        // steady state cannot drift into two different notions of "routable".
        if let Err(e) =
            crate::web_content::domain::reconcile_custom_domain_routing_once(&state.db, resolver)
                .await
        {
            tracing::warn!("web host resolver: failed to list web domains at boot: {e}")
        }
        // Subdomain hosting (Slice 3): each opted-in actor's `<handle>` →
        // its actor. The handle keys the subdomain (`<handle>.<node-domain>`);
        // an opted-in actor with no handle (or a reserved-label handle) is
        // skipped here — `resolve` excludes reserved hosts regardless, and the
        // 5-min cert reconcile re-evaluates once a handle exists.
        match state.db.list_subdomain_enabled().await {
            Ok(actors) => {
                for actor in actors {
                    match state.db.get_handle(&actor).await {
                        Ok(Some(handle))
                            if !handle.is_empty()
                                && !crate::web_content::serve::is_reserved_subdomain_label(
                                    &handle,
                                ) =>
                        {
                            resolver.register_subdomain(&handle, actor).await
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(
                            "web host resolver: failed to resolve handle for opted-in subdomain actor at boot: {e}"
                        ),
                    }
                }
            }
            Err(e) => tracing::warn!(
                "web host resolver: failed to list opted-in subdomain actors at boot: {e}"
            ),
        }
    }

    // Bridge rate-limiter bucket sweeper. Drops empty buckets every
    // 5 × window so a long-lived deployment can't grow the DashMap
    // unboundedly from a compromised approved bridge spraying
    // distinct credential_id strings. Default LimiterConfig window is
    // 60s ⇒ 5 min sweep.
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.bridge_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Per-actor spam-training limiter sweeper. Same 5 × window
    // cadence so idle actors' buckets don't accumulate (window is 60s ⇒ 5 min).
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.spam_train_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Per-(actor, channel) folder-commit rate-cap sweeper (same cadence, though
    // its own window is an hour — buckets accumulate one per distinct
    // non-claimant/channel pair, so an idle-actor sweep still bounds growth).
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.channel_commit_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Per-principal record-door limiter sweeper (window 60s ⇒ 5 min), so a
    // revoked app's bucket does not outlive it.
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.records_door_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Anonymous discovery-surface limiter sweeper. Same 5 × window cadence so a
    // sustained harvesting campaign can't grow the per-source DashMap
    // unboundedly. Default window is 60s ⇒ 5 min sweep.
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.anonymous_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Failed-credential throttle sweeper (same 5 × window cadence): a flood of
    // refusals from rotating sources or claimed identities would otherwise
    // grow one bucket per pair for ever.
    state.scope_handle(crate::failed_credential_throttle::spawn_sweeper(
        state.failed_credential_throttle.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Admin-claim attempt limiter sweeper (same cadence). Buckets are short-lived
    // — once the nest is claimed the surface is gone — but a pre-claim
    // brute-force campaign would otherwise accumulate per-source buckets.
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.claim_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Global admin-claim cap sweeper (same cadence). One source-independent
    // bucket per claim kind — the distributed-brute-force bound behind the
    // per-source limiter above.
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.global_claim_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Invite-code verify limiter sweeper (same cadence). Per-source buckets for
    // the anonymous invite-guess surface; same accumulation concern as the
    // discovery limiter under a sustained enumeration campaign.
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.invite_verify_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Registration limiter sweeper (same cadence). Per-source buckets for the
    // anonymous `fauna.account.register` write surface; same accumulation concern
    // as the other anonymous limiters under a sustained flood.
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.register_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // Invite-request submit limiter sweeper (same cadence). Per-source buckets for
    // the anonymous `fauna.account.invite_request.submit` write surface.
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.invite_request_rate_limit.clone(),
        std::time::Duration::from_secs(300),
    ));
    // D4 link-preview per-actor resolve limiter sweeper (same cadence) so the
    // per-actor DashMap can't grow unboundedly from a churn of distinct actors.
    state.scope_handle(crate::bridge_rate_limit::spawn_sweeper(
        state.link_preview.rate_limiter.clone(),
        std::time::Duration::from_secs(300),
    ));
    // D4 link-preview by-url cache sweeper. 1 h TTL ⇒ a 30 min cadence keeps the
    // cache bounded by genuinely re-requested links (lazy eviction only fires on
    // a re-read of an expired url).
    state.scope_handle(crate::link_preview::cache::spawn_sweeper(
        state.link_preview.cache.clone(),
        std::time::Duration::from_secs(1800),
    ));
    // bridge_audit_events retention sweeper. 90-day default; rare
    // cadence keeps the DB lock
    // brief.
    state.scope_handle(crate::db::bridge_audit::spawn_audit_retention_sweeper(
        state.db.clone(),
        crate::db::bridge_audit::DEFAULT_AUDIT_RETENTION,
    ));
    // tlsrpt_outbound_reports retention sweeper per
    // docs/goal/behavior/smtp-server.md § TLSRPT outbound reporter:
    // 7-day raw-JSON retention. Cadence is 1/24 of retention (≈7 h),
    // same pattern as the bridge_audit sweeper above.
    state.scope_handle(
        crate::db::outbound::spawn_tlsrpt_outbound_retention_sweeper(
            state.db.clone(),
            crate::db::outbound::DEFAULT_TLSRPT_OUTBOUND_RETENTION,
        ),
    );
    // TLSRPT outbound daily emitter (RFC 8460): drains the per-attempt
    // aggregator (fed by the `report_tls_attempt` handler) once per UTC day
    // and submits one aggregate report per recipient domain that publishes a
    // `_smtp._tls.<domain>` policy. The recipient-policy fetcher is the same
    // hickory-resolver + 24 h-cache shape as MTA-STS; a build failure (or a
    // reqwest-client build failure) degrades to the no-op fetcher/poster so a
    // resolver hiccup never aborts startup. docs/goal/behavior/smtp-server.md
    // § TLSRPT outbound reporter.
    state.scope_handle({
        let tlsrpt_fetcher: std::sync::Arc<dyn fauna_mail::outbound::tlsrpt::TlsrptPolicyFetcher> =
            match fauna_mail::outbound::tlsrpt::LiveTlsrptPolicyFetcher::new() {
                Ok(live) => {
                    let clock: fauna_mail::outbound::mta_sts::ClockFn =
                        std::sync::Arc::new(fauna_core::data::Timestamp::now_secs_or_zero);
                    std::sync::Arc::new(
                        fauna_mail::outbound::tlsrpt::CachingTlsrptPolicyFetcher::new(live, clock),
                    )
                }
                Err(e) => {
                    tracing::warn!("failed to build TLSRPT policy fetcher: {e}; reports disabled");
                    std::sync::Arc::new(fauna_mail::outbound::tlsrpt::NullTlsrptPolicyFetcher)
                }
            };
        let tlsrpt_poster: std::sync::Arc<dyn fauna_mail::outbound::tlsrpt::TlsrptHttpPoster> =
            match fauna_mail::outbound::tlsrpt::ReqwestTlsrptPoster::new() {
                Ok(p) => std::sync::Arc::new(p),
                Err(e) => {
                    tracing::warn!("failed to build TLSRPT https poster: {e}; https reports off");
                    std::sync::Arc::new(fauna_mail::outbound::tlsrpt::NullTlsrptHttpPoster)
                }
            };
        crate::outbound_tlsrpt::spawn_tlsrpt_daily_dispatch(
            state.clone(),
            tlsrpt_fetcher,
            tlsrpt_poster,
            crate::outbound_tlsrpt::TLSRPT_JITTER_WINDOW_SECS,
        )
    });
    // alias_hits retention sweeper (§ A2.4):
    // 30-day audit retention per docs/goal/behavior/mail-aliases.md
    // § Per-alias-hit audit list. Same 1/24-of-retention cadence as
    // the bridge_audit sweeper above.
    state.scope_handle(crate::db::mail_aliases::spawn_alias_hits_retention_sweeper(
        state.db.clone(),
        crate::db::mail_aliases::DEFAULT_ALIAS_HITS_RETENTION,
    ));
    // message_scan_results retention sweeper:
    // 30-day default per docs/goal/behavior/mail-content-scanning.md
    // § Retention. Same 1/24-of-retention cadence as the sweepers above;
    // `None` rejected-malware override → falls back to the 30-day window
    // until the (deferred) `mail.scanning.*` admin write-path lands.
    state.scope_handle(
        crate::db::bridge_routing::spawn_scan_result_retention_sweeper(
            state.db.clone(),
            crate::db::bridge_routing::DEFAULT_SCAN_RESULT_RETENTION,
            None,
        ),
    );

    // Greylist tuple GC — bound `greylist_tuples`
    // growth; a row untouched past the 30 d whitelist window can't matter.
    state.scope_handle(crate::db::bridge_routing::spawn_greylist_retention_sweeper(
        state.db.clone(),
        crate::db::bridge_routing::DEFAULT_GREYLIST_RETENTION,
    ));

    // Denied invite_requests retention sweeper (`onboarding.md` §
    // The pending-invite surface): a denied row otherwise persists forever
    // and is the only thing blocking that actor's resubmit. 90-day default;
    // same 1/24-of-retention cadence as the sweepers above.
    state.scope_handle(crate::db::admin::spawn_invite_request_retention_sweeper(
        state.db.clone(),
        crate::db::admin::DEFAULT_INVITE_REQUEST_RETENTION,
    ));

    // Durable idempotency retention sweeper (W4 (account-data-plane.md § Workstreams) phase 3,
    // `account-data-plane.md` § The offline-mutation contract → *Nest-side
    // durable idempotency*): recorded replies are a replay cache; 7-day
    // default, same 1/24-of-retention cadence as the sweepers above.
    state.scope_handle(
        crate::db::rpc_idempotency::spawn_rpc_idempotency_retention_sweeper(
            state.db.clone(),
            crate::db::rpc_idempotency::DEFAULT_RPC_IDEMPOTENCY_RETENTION,
        ),
    );

    // Mail-enable flag-vs-state reconciliation tick (Phase E) — every 60 s,
    // re-assert the `/data/imap-enabled` flag from the persisted `mail_enabled`
    // toggle (the flag is nest's output, not the admin's input; a hand-edit is
    // reconciled away with a `bridge_flag_file_diverged_from_state` warning).
    // Only spawned when the deployment has a data dir; the in-memory test/dev
    // path has no flag file to reconcile. Spec:
    // `docs/goal/behavior/mail-bridge-lifecycle.md` § Default-off on first claim.
    if let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
        state.scope_handle(crate::mail_enable::spawn_mail_enable_reconciliation(
            state.db.clone(),
            data_dir,
            crate::mail_enable::MAIL_ENABLE_RECONCILE_INTERVAL,
        ));
    }

    // CalDAV-enable flag-vs-state reconciliation tick — the calendar twin of the
    // mail reconciler above, re-asserting `/data/caldav-enabled` from the
    // persisted `caldav_enabled` toggle. Independent enablement per
    // `docs/goal/behavior/caldav-server.md` § Independent enablement.
    if let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
        state.scope_handle(crate::mail_enable::spawn_caldav_enable_reconciliation(
            state.db.clone(),
            data_dir,
            crate::mail_enable::MAIL_ENABLE_RECONCILE_INTERVAL,
        ));
    }

    // CardDAV-enable flag-vs-state reconciliation tick — the contacts twin of
    // the CalDAV reconciler above, re-asserting `/data/carddav-enabled` from the
    // persisted `carddav_enabled` toggle. Independent enablement per
    // the CardDAV server design (tracked internally), § 5.
    if let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
        state.scope_handle(crate::mail_enable::spawn_carddav_enable_reconciliation(
            state.db.clone(),
            data_dir,
            crate::mail_enable::MAIL_ENABLE_RECONCILE_INTERVAL,
        ));
    }

    // WebDAV-enable flag-vs-state reconciliation tick — the files twin of the
    // CardDAV reconciler above, re-asserting `/data/webdav-enabled` from the
    // persisted `webdav_enabled` toggle. Independent enablement per
    // `docs/goal/behavior/webdav-server.md` § Independent enablement.
    if let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
        state.scope_handle(crate::mail_enable::spawn_webdav_enable_reconciliation(
            state.db.clone(),
            data_dir,
            crate::mail_enable::MAIL_ENABLE_RECONCILE_INTERVAL,
        ));
    }

    // CalDAV-port flag-vs-state reconciliation tick — re-assert the
    // `/data/caldav-port` value flag from the persisted `caldav_port` singleton.
    // This is the desktop-supervisor's port mirror (it can't call `fetch_config`,
    // which is bridge-enrollment-only); the Docker MDA ignores the flag and reads
    // `fetch_config.caldav_port` directly. Per `docs/goal/behavior/caldav-server.md`
    // § Network exposure (Desktop / IP deployment).
    if let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
        state.scope_handle(crate::mail_enable::spawn_caldav_port_reconciliation(
            state.db.clone(),
            data_dir,
            crate::mail_enable::MAIL_ENABLE_RECONCILE_INTERVAL,
        ));
    }

    // Serving-port flag-vs-state reconciliation tick — re-assert the
    // `/data/serving-port` value flag from the persisted `serving_port` singleton
    // (the nest's own client-facing listener twin of the caldav-port reconciler
    // above). The desktop supervisor reads it to restart the nest on a port
    // change; the Docker entrypoint ignores it (the nest is fronted by the SNI
    // router, so the external port is the compose port-map + the provisioning
    // orchestrator). Per `docs/goal/architecture/nest/common.md` § Serving ports.
    if let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
        state.scope_handle(crate::mail_enable::spawn_serving_port_reconciliation(
            state.db.clone(),
            data_dir,
            crate::mail_enable::MAIL_ENABLE_RECONCILE_INTERVAL,
        ));
    }

    // Boot-time mail-domain safety net. A box that booted with mail already
    // enabled but no `mail_domains` row (e.g. a re-claim that carried no
    // `mail_domain`) would otherwise leave
    // the in-container MTA/MDA idling on an empty `local_domains` projection —
    // and an idling-on-empty bridge does NOT re-poll, it only re-reads on a
    // process cold-boot. So provision the nest's own (real) domain as the
    // primary HERE, before the WS server below starts accepting bridge
    // connections, so the bridge reads a non-empty `local_domains` on its
    // cold-boot `fetch_config` and binds. Awaited (not spawned) to guarantee the
    // row exists before any `fetch_config`. No-op unless `mail_domains` is empty
    // AND the nest domain is real (gated inside). Per `mail-bridge-lifecycle.md`
    // § Default-off on first claim.
    if matches!(state.db.get_mail_enabled().await, Ok(Some(true))) {
        crate::mail_enable::ensure_primary_mail_domain(&state).await;
        // Boot-time admin canonical-recipient-alias self-heal — the companion to
        // the domain net above. Backfills the admin's `<handle>@<domain>` exact
        // alias on a box whose claim carried a `mail_domain` (or whose
        // domain was registered some other way), so the admin's own address
        // resolves for IMAP/CalDAV/submission AUTH after this restart. Runs after
        // the domain net so the primary domain exists. Per `mail-aliases.md`
        // § Kind 1 — Exact.
        crate::mail_enable::ensure_admin_recipient_aliases(&state).await;
    }

    // Boot drain of owed web renders — the reconcile half of a revoke's
    // durability (`web-content-hosting.md` § Routing, render, serving → *A
    // revoke is durable*). Every door that removes content from a rendered
    // site records the owed render in its own transaction; a nest that stopped
    // before rendering restarts with that marker, and this pass renders (or,
    // failing, clears) each owed site so a deleted post's page does not serve
    // on. Awaited before serving. O(owed sites): a no-op on a box that stopped
    // cleanly. Non-fatal; an undrained marker is retried next boot.
    if let Some(wcs) = &state.web_content_service {
        match wcs.drain_owed_renders().await {
            // The drain also starts the between-boots restore retry and hands
            // back its handle; adopt it into this serving generation so a
            // deployment-seed rotation's teardown reaches the loop instead of
            // leaving it rendering under superseded key material.
            Ok(drain) => {
                if let Some(retry) = drain.restore_retry {
                    state.scope_handle(retry);
                }
                if drain.failed > 0 {
                    tracing::warn!(
                        failed = drain.failed,
                        "owed web renders: some sites failed to render and were cleared"
                    );
                }
            }
            Err(e) => tracing::error!("owed web renders: cannot list the owed sites: {e:#}"),
        }
    }

    // NOTE — there is deliberately NO boot-time `__index` reconcile here, and
    // nothing on the boot path may ever delete `__index` content again.
    //
    // Until rollout slice S2 (2026-08-02) this spot ran a content-blind purge
    // (`db/index_purge.rs`) that tombstoned and deleted every `__index` blob on
    // every boot. That was correct while the only `__index` bytes in existence
    // were the retired Plan-5 nest-side writer's unsealed residue — its
    // remediation, finished long since on every box. The moment a
    // capability-position builder syncs real *sealed* segments in, the same
    // pass becomes a data-destroying bug, so it is DELETED rather than gated:
    // inventing a predicate risks letting residue survive and reopening the
    // defect above, while `__index` is now a user at-rest store the nest cannot read.
    // Authority: `content-index.md` § Before Plan 5b writes its first byte
    // (constraint 1) + § Don't do these. Pinned by
    // `tests/index_survives_nest_restart.rs`, which drives this whole boot.

    // RETIRED (2026-08-17, row 61): the Phase-3 S6.7 boot-time CalDAV/CardDAV
    // column→segment back-fills. Their corpus was pre-cutover rows (body still
    // in `bridge_{caldav_events,carddav_cards}.encrypted_body`), which the
    // record-identity cutover wipes; every surviving row is born post-cutover
    // with its body in the segment and its content-hash `record_cid` stored.

    // RETIRED (2026-09-24, the compat-remnant sweep —
    // `version-compatibility.md` § Dimension 2, program 4): the S6.9
    // boot-time placement-manifest heal for the v1→v2 format bump. It filled
    // the fields a v1 manifest never recorded from the live SQLite rows; no
    // v1 manifest exists anywhere, and a v1 manifest or journal frame is now
    // refused at load/replay.

    // Spawn the nest's own task-delegation lease runner (slice 6): while a
    // sufficient capability grant is held, the nest heartbeats the granted
    // kinds' leases so clients see it as the runner and the re-score drain
    // gate opens (delegation_runner module doc).
    state.spawn_scoped(delegation_runner::run(state.clone()));

    // Start the installed WASM plugins (`third-party.md` § The principal
    // model → *Hosted principals*). A plugin whose files are gone or whose
    // module no longer compiles is listed `stopped`, never fatal to the boot.
    {
        let boot_state = state.clone();
        state.spawn_scoped(async move { boot_state.plugins.boot(&boot_state).await });
    }

    // Spawn Nostr sync worker for external relay communication
    #[cfg(feature = "nostr")]
    {
        let worker = nostr::sync_worker::NostrSyncWorker::new(
            state.db.clone(),
            state.post_segments.clone(),
            nostr_sync_rx,
            nostr_bunker_wake_rx,
            state.nest_identity.signing_key.to_bytes(),
            state.nostr.relay_dial_policy,
        )
        // Enables the zap purchase leg: a believed receipt meeting its target
        // tier's asking price goes through the payment waist rather than being
        // recorded as a tip (`monetization.md` § The asking price).
        .with_state(state.clone());
        state.spawn_scoped(worker.run());
        tracing::info!("Nostr sync worker spawned");
    }

    // Spawn the Bluesky notification sync worker: bridges likes, replies,
    // reposts, quotes, follows and mentions into the unified notification list
    // (`bridges.md` § Bluesky bridge → Notifications). It enumerates only
    // consume-side linked accounts, which is where D7's hosted-backing gate
    // lives (`atproto-pds-full.md` § D7).
    #[cfg(feature = "bluesky")]
    {
        let worker = bluesky::notif_worker::BlueskyNotifWorker::new(state.clone());
        state.spawn_scoped(worker.run());
        tracing::info!("Bluesky notification sync worker spawned");

        // The consume-side feed poller: each linked account's timeline and
        // subscribed custom feeds into the unified feed as `bluesky`-source
        // posts (`bridges.md` § Unified feed ingestion → Bridge ingestion).
        // Same enumeration, same D7 gate at its start site.
        let worker = bluesky::feed_worker::BlueskyFeedWorker::new(state.clone());
        state.spawn_scoped(worker.run());
        tracing::info!("Bluesky feed ingestion worker spawned");

        // The Bluesky DM leg of the bridged-conversation family: each linked
        // account's `chat.bsky` conversations in, the leg's outbox out
        // (`conversations.md` § Where logic lives → *The `Bridged` adapter*,
        // ruling 3). Same enumeration, same D7 gate at its start site.
        let worker = bluesky::dm_worker::BlueskyDmWorker::new(state.clone());
        state.spawn_scoped(worker.run());
        tracing::info!("Bluesky DM worker spawned");
    }

    // Spawn ActivityPub sync worker for outbound delivery
    #[cfg(feature = "activitypub")]
    {
        let worker = activitypub::sync_worker::ApSyncWorker::new(
            state.db.clone(),
            ap_delivery_nudge,
            state.nest_identity.signing_key.to_bytes(),
        );
        state.spawn_scoped(worker.run());
        tracing::info!("ActivityPub sync worker spawned");

        // Mint the instance actor now rather than inside the first inbox POST:
        // it is ~200ms of RSA generation, and the request that would otherwise
        // pay for it is a remote's activity delivery, which times out.
        // Idempotent, so a nest that already has one just reads it back.
        let ap_state = state.clone();
        state.spawn_scoped(async move {
            match activitypub::instance_actor::ensure(&ap_state).await {
                Ok(actor) => {
                    tracing::info!(actor = %actor.actor_url, "ActivityPub instance actor ready")
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    "ActivityPub instance actor unavailable — outbound actor fetches will be \
                     unsigned, which an AUTHORIZED_FETCH peer refuses",
                ),
            }
        });
    }

    let state_ret = state.clone();

    state.scope_handle(discovery::spawn_discovery_poller(
        state.db.clone(),
        state.clone(),
        discovery::PollConfig::default(),
        feed_event_rx,
        shutdown_rx.clone(),
    ));

    // The federation exchange originator plane (federation.md § the exchange
    // originator plane): pushes/pulls the report + trend aggregate pairs
    // toward the pairing target, discovery contributor nests, and prior
    // exchange partners — startup + hourly tick + debounced local-aggregate
    // transitions.
    state.scope_handle(exchange_originator::spawn_exchange_originator(
        state.clone(),
        exchange_transition_rx,
        shutdown_rx,
    ));

    // The domain-expiry watch (`domains-and-tls-bootstrap.md` § Domain loss →
    // *Detection*): RDAP-query the primary domain on a daily-class cadence so a
    // lapsing registration reaches every authenticated user's banner while
    // renewal is still a few minutes of work at the registrar, instead of
    // announcing itself as failures. Scopes itself; on a domainless deployment
    // its tick reads one row and makes no network request.
    domain_expiry::spawn_domain_expiry_watch(state.clone());

    // The spam baseline's standing publish (`mail-spam.md` § Cold start Path 2
    // → *Standing publish*): with the admin's setting on, the nest runs the
    // same publish the admin's click starts every 24 hours, bound by both
    // floors. With it off (the default) the tick reads one row and returns.
    spam_baseline::spawn_spam_baseline_cadence(state.clone());

    // The abuse-report triad's forwarder (`moderation.md` § Routing): drains
    // the durable queue of forwarded reports, withdrawals and outcomes, so a
    // peer that was down when a report was filed still receives it.
    abuse_report_federation::spawn_abuse_report_forwarder(state.clone());

    let app = build_router(state);
    let ip_limiter = rate_limit::new_ip_limiter(100);
    // GC the per-IP governor DashMap every 5 min so it can't grow unboundedly as
    // client IPs rotate — live now that the SNI router restores real client IPs
    // (security review § D11). Same cadence as the sliding-window sweepers above.
    state_ret.scope_handle(rate_limit::spawn_retain_sweeper(
        ip_limiter.clone(),
        std::time::Duration::from_secs(300),
    ));
    let app = app.layer(rate_limit::RateLimitLayer::new(ip_limiter));

    // The authorization server's per-(route, source) limiter needs the same
    // treatment, and for a sharper reason: its buckets used to be reclaimed from inside `allow`, so under a
    // flood — where nothing is expired and `retain` frees nothing — every
    // admission rescanned the whole map while holding the one lock all three AS
    // routes spend. Off the request path it is bounded work; on it, the cost per
    // request grew with the flood, before DPoP and before any authentication.
    state_ret.scope_handle(oauth_as_rate_limit::spawn_endpoint_sweeper(
        Arc::clone(&state_ret.oauth_limiter),
        std::time::Duration::from_secs(
            u64::try_from(oauth_as_rate_limit::OAUTH_WINDOW_SECS).unwrap_or(300),
        ),
    ));

    // Boot-time fail-safe: refuse to serve the
    // API in cleartext when a *public* domain is configured. The standard deploy
    // path always has a cert (self-signed bootstrap → ACME), so this never fires
    // there; it guards the hand-built/misconfigured deploy that sets a public
    // domain but disables TLS + ACME, which would otherwise silently serve the
    // full API — bearer tokens included — over plain HTTP.
    refuse_plain_http_for_public_domain(nest_config.nest.domain.as_deref(), tls_config.is_some())?;

    // Bind the external client-facing listener — with a recoverability fallback
    // (Issue A, `nest/common.md` § Serving ports ⚠). `bind_addr` carries the
    // boot-resolved admin `serving_port` (over the seed's `original_bind_port`).
    // If the admin chose a port this (possibly unprivileged) process cannot bind,
    // a plain `?` here would crash-loop the nest with the bad port persisted in
    // `nest.db` — client-unrecoverable. Instead, when the *resolved* port (not the
    // seed) is unbindable for a permission/occupancy reason, fall back to the seed
    // and stay reachable so a client can re-choose. A seed the artifact itself
    // can't bind, or any other error, is a genuine fault and still propagates.
    let listener = match external_listener {
        // launchd socket-activation path (macOS `_fauna` machine daemon): use the
        // pre-bound `:443` fd verbatim. The resolve/bind/seed-fallback dance below
        // does NOT apply — launchd owns this port, and a non-root daemon cannot
        // bind it itself. `local_addr` is read off the inherited listener, so the
        // internal-loopback + reported address all flow from the real bound fd.
        Some(pre_bound) => {
            tracing::info!(
                addr = ?pre_bound.local_addr().ok(),
                "using a pre-bound external listener (launchd socket activation)"
            );
            pre_bound
        }
        None => {
            let seed_addr = std::net::SocketAddr::new(bind_addr.ip(), original_bind_port);
            match tokio::net::TcpListener::bind(bind_addr).await {
                Ok(l) => l,
                Err(e)
                    if serving_port_bind_should_fall_back(
                        bind_addr.port(),
                        original_bind_port,
                        &e,
                    ) =>
                {
                    tracing::error!(
                        chosen_serving_port = bind_addr.port(),
                        seed_port = original_bind_port,
                        error = %e,
                        "cannot bind the admin-chosen serving port; falling back to the seed port \
                         to stay reachable (the nest stays up so a client can pick a bindable port). \
                         NOTE: setup.status still reports the chosen port — surfacing the failed \
                         choice is a captured follow-on"
                    );
                    // The seed is the artifact's own bind, which it is expected to be able
                    // to bind; if even that fails, it is a real deploy fault — propagate.
                    tokio::net::TcpListener::bind(seed_addr).await?
                }
                Err(e) => return Err(e.into()),
            }
        }
    };
    let local_addr = listener.local_addr()?;

    // Same-box reach (DoD #6): a desktop-direct nest additionally binds a FIXED
    // `127.0.0.1:<canonical-internal-port>` listener (requested by the in-process
    // Windows nest-service via `FAUNA_INTERNAL_LOOPBACK_PORT`) so the co-located
    // bridge + same-box app dial a port that never moves when the admin changes
    // the external `serving_port` — without it, a port change strands same-box
    // clients (latent in the boot-resolve, surfaced by the restart-trigger). It
    // serves the SAME router + SAME self-signed floor cert (SPKI-pinned per
    // authority, so a distinct loopback port is trusted identically). `None`
    // (Docker/dev/e2e/bare-metal — env unset, or the internal port already equals
    // the external bind's port) ⇒ no extra listener, zero behavior change. The
    // bind is best-effort: a failure is logged, never failing the external listener.
    let internal_loopback =
        resolve_internal_loopback_addr(local_addr, internal_loopback_port_from_env());

    // Per-source-IP connection cap — read from **nest state** (the client-set
    // `transport_policy`), not the env, per the product invariant (an abuse
    // knob is client-set config). `resolve_tls_per_ip_cap` applies the
    // DB-override → env-fallback → default precedence, the same resolver the
    // `get_policy` view uses. The cap is **hot-reloaded**: we apply the boot
    // value via `set_max` on the AppState-shared limiter and hand EVERY
    // listener below — TLS or plain, external or internal-loopback — a clone
    // of that **same** Arc, so a later `fauna.transport.put_policy` (which
    // calls `set_max` on it) binds the live accept loops without a restart.
    // See `transport-connection.md` § Abuse posture item (2). One limiter object also
    // means one loopback ceiling shared across the listeners: a leaking
    // co-resident process is bounded as one source, not once per listener.
    let per_ip_override = state_ret
        .db
        .get_transport_policy()
        .await
        .unwrap_or_default()
        .max_conns_per_ip;
    state_ret
        .per_ip_conn_limit
        .set_max(resolve_tls_per_ip_cap(per_ip_override));
    let per_ip_limit = std::sync::Arc::clone(&state_ret.per_ip_conn_limit);

    if let Some(tls) = tls_config {
        tracing::info!("HTTPS enabled on {local_addr}");
        tracing::info!(
            "TLS mode: per-IP rate limiting + the loopback trust gate use the connection source injected as ConnectInfo by serve_tls's WithConnectInfo middleware. On the single-box deploy the fauna-sni-router fronts :443 and prepends a PROXY-v2 header (when --send-proxy-to lists this backend), so the source is the real internet client; a headerless loopback connection (the in-container bridge dialing directly) keeps its genuine loopback address. A PROXY header is only trusted from a loopback TCP peer (the local router)."
        );
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        // Clone the router for the optional internal-loopback listener before the
        // primary consumes it (`into_make_service`). Cheap — axum `Router` clone is
        // Arc-based.
        let internal_app = internal_loopback.map(|_| app.clone());
        let app = app.into_make_service();
        if let (Some(iaddr), Some(iapp)) = (internal_loopback, internal_app) {
            let iapp = iapp.into_make_service();
            let iacceptor = acceptor.clone();
            let ilimit = std::sync::Arc::clone(&per_ip_limit);
            match tokio::net::TcpListener::bind(iaddr).await {
                Ok(il) => {
                    tracing::info!("internal-loopback co-located-IPC listener (HTTPS) on {iaddr}");
                    state_ret
                        .spawn_scoped(async move { serve_tls(il, iacceptor, iapp, ilimit).await });
                }
                Err(e) => {
                    tracing::error!("failed to bind internal-loopback listener {iaddr}: {e}")
                }
            }
        }
        // spawn-ok(server-handle): returned to the serve loop, aborted first by `teardown_serving_generation`
        let handle = tokio::spawn(async move {
            serve_tls(listener, acceptor, app, per_ip_limit).await;
        });
        return Ok((local_addr, handle, state_ret));
    }

    // Plain HTTP — dev, tier_3, and a local-domain deploy
    // (`transport-connection.md` § Abuse posture: a public domain never
    // serves plain). It rides the SAME accept loop as TLS (`serve_plain` /
    // `serve_tls` → `serve_admitted`):
    // global cap, per-IP permit, keepalive, shed warns. Until 2026-08-22 this
    // path was a bare `axum::serve` with none of them, which is how one leaking
    // e2e client accumulated 16 k accepted sockets on a nest that logged
    // nothing — and, since the e2e suite serves plain HTTP, how every accept-
    // loop defence stayed invisible to it.
    if let Some(iaddr) = internal_loopback {
        let iapp = app.clone().into_make_service();
        let ilimit = std::sync::Arc::clone(&per_ip_limit);
        match tokio::net::TcpListener::bind(iaddr).await {
            Ok(il) => {
                tracing::info!("internal-loopback co-located-IPC listener (HTTP) on {iaddr}");
                state_ret.spawn_scoped(async move { serve_plain(il, iapp, ilimit).await });
            }
            Err(e) => tracing::error!("failed to bind internal-loopback listener {iaddr}: {e}"),
        }
    }

    let app = app.into_make_service();
    // spawn-ok(server-handle): returned to the serve loop, aborted first by `teardown_serving_generation`
    let handle = tokio::spawn(async move {
        serve_plain(listener, app, per_ip_limit).await;
    });

    Ok((local_addr, handle, state_ret))
}

/// Graceful shutdown (`transport.md` § Graceful shutdown): abort the
/// accept-loop `handle` `start_server` returned and await it, broadcast WS
/// 1001 and wait out the drain, then flush. `handle.abort()` — never
/// `drop(handle)`, which only detaches the task rather than stopping it — is
/// what actually stops the accept loop. Lifted out of `main.rs::cmd_serve` so an integration test in `bins/fauna-nest/tests/` — which
/// links this lib, never the `main.rs` bin — can drive the exact production
/// sequence and mutation-test the one line that matters.
pub async fn graceful_shutdown(
    handle: tokio::task::JoinHandle<()>,
    ws_state: &ws::WsState,
    db: &db::CacheDb,
) {
    handle.abort();
    let _ = handle.await;
    tracing::info!("graceful shutdown: broadcasting WS 1001 and draining in-flight requests");
    ws_state.begin_shutdown();
    let drain_deadline = std::time::Instant::now() + ws::GRACEFUL_SHUTDOWN_TIMEOUT;
    while ws_state.connection_count() > 0 && std::time::Instant::now() < drain_deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let remaining = ws_state.connection_count();
    if remaining > 0 {
        tracing::warn!(
            "graceful shutdown: {remaining} connection(s) did not drain within {}s; closing anyway",
            ws::GRACEFUL_SHUTDOWN_TIMEOUT.as_secs()
        );
    }
    tracing::info!("Flushing database...");
    if let Err(e) = db.flush().await {
        tracing::error!("Error flushing database: {e}");
    }
    tracing::info!("Shutdown complete");
}

/// Resolve the effective per-source-IP connection cap, the single source of
/// truth shared by `start_server`'s boot-time `set_max` (every listener — TLS
/// or plain — shares the one limiter) and the `fauna.transport.get_policy`
/// view (so an admin reads exactly what the listeners enforce). The `tls` in
/// the name is historical: the cap predates the shared accept loop. Two sources and no third: the **client-set** DB override
/// (`transport_policy.max_conns_per_ip`, the product-invariant home), else
/// the constant `DEFAULT_MAX_CONNS_PER_IP` (256). No environment variable
/// names the cap — an abuse cap is an admin's in-app choice or a constant,
/// never a deployment variable.
///
/// A `Some(0)` override is treated as unset (falls through to the constant)
/// rather than locking every source out — the v1 cap is always on; widening
/// it means setting a high value. See `transport-connection.md` § Abuse posture item (2).
pub(crate) fn resolve_tls_per_ip_cap(db_override: Option<u32>) -> usize {
    db_override
        .filter(|&n| n > 0)
        .map(|n| n as usize)
        .unwrap_or(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP)
}

/// Accept TLS connections in a loop and serve each one with hyper — the TLS
/// flavour of the one shared accept loop ([`serve_admitted`]); [`serve_plain`]
/// is the other.
///
/// `pub` so `tests/tls_websocket_upgrade.rs` can drive it directly with a
/// self-signed acceptor and assert WS upgrades survive over TLS (the
/// regression guard for the `serve_connection_with_upgrades` fix — the e2e
/// suite serves plain HTTP, so this path is otherwise CI-invisible). The
/// per-source-IP cap is constructed from nest state by [`start_server`] and
/// passed in (client-set config, not an env read) — see
/// [`resolve_tls_per_ip_cap`].
pub async fn serve_tls(
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    make_service: axum::routing::IntoMakeService<axum::Router>,
    per_ip_limit: std::sync::Arc<fauna_conn_limit::PerIpConnLimit>,
) {
    serve_admitted(listener, make_service, per_ip_limit, Some(acceptor)).await
}

/// Accept plain-HTTP connections in a loop and serve each one with hyper — the
/// plain flavour of the one shared accept loop ([`serve_admitted`]), with the
/// same admission as [`serve_tls`]: global cap, per-IP permit, keepalive
/// arming, rate-limited shed warns. Serves dev, tier_3, and a local-domain
/// deploy (`transport-connection.md` § Abuse posture).
///
/// `pub` so `tests/plain_http_admission.rs` can drive it with a small loopback
/// ceiling and assert the shed — the regression guard for the 2026-08-22
/// socket-exhaustion incident, in which this path was a bare `axum::serve`
/// that admitted 16 k connections from one leaking loopback peer and logged
/// nothing.
pub async fn serve_plain(
    listener: tokio::net::TcpListener,
    make_service: axum::routing::IntoMakeService<axum::Router>,
    per_ip_limit: std::sync::Arc<fauna_conn_limit::PerIpConnLimit>,
) {
    serve_admitted(listener, make_service, per_ip_limit, None).await
}

/// The one accept loop behind [`serve_tls`] and [`serve_plain`]. Every
/// connection the nest admits — external or internal-loopback, TLS or plain —
/// passes the same gates in the same order: TCP keepalive armed → global cap →
/// PROXY-v2 source resolve → per-IP permit → (TLS handshake, when `acceptor`
/// is `Some`) → hyper with upgrades and a header-read timeout. One loop, so a
/// defence added for one listener cannot be missing on another (priority #3;
/// `transport-connection.md` § Abuse posture).
async fn serve_admitted(
    listener: tokio::net::TcpListener,
    mut make_service: axum::routing::IntoMakeService<axum::Router>,
    per_ip_limit: std::sync::Arc<fauna_conn_limit::PerIpConnLimit>,
    acceptor: Option<tokio_rustls::TlsAcceptor>,
) {
    use tower_service::Service;

    // Bound concurrent connections — an OOM / FD-exhaustion backstop against a
    // connection flood. A constant, 4096
    // (each idle rustls connection holds tens of KiB): a resource bound nobody
    // chooses, so no environment variable names it. It bounds every listener
    // this loop serves. When the cap is hit we *shed*
    // the new connection rather than block the accept loop, so existing
    // connections keep serving. The finer per-IP cap (keyed on the
    // PROXY-v2-resolved real client IP) is layered on below — see the
    // `per_ip_limit` after this Semaphore.
    const MAX_CONNECTIONS: usize = 4096;
    let conn_limit = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));

    // The router-auth secret a PROXY-v2 header must carry to be trusted
    // on this (router-fronted) box — loaded once here, cloned into each accept
    // task. `None` on a direct-listener nest (no router) or a provisioning gap,
    // in which case `read_optional_proxy_header` trusts a loopback PROXY header as it stands.
    let router_auth: Option<std::sync::Arc<[u8]>> =
        load_router_auth_secret().map(std::sync::Arc::from);

    // Per-source-IP concurrent-connection cap — the finer layer on top of the
    // global Semaphore, keyed on the real client IP the PROXY-v2 fix resolves
    // (`source_addr` below). One source can hold at most this many concurrent
    // connections rather than the whole pool; loopback (the in-container
    // bridge / the router / a same-box app) is counted against its own
    // hard-coded ceiling instead (`fauna_conn_limit::LOOPBACK_MAX_CONNS`). The
    // cap is **client-set nest config**
    // (`fauna.transport.put_policy` → `transport_policy` table → resolved by
    // `resolve_tls_per_ip_cap`), constructed by `start_server` and passed in
    // here — a per-IP cap is an admin-tunable abuse knob, so by the product
    // invariant it isn't a deployment env/CLI knob (no environment variable
    // names it). The cap is hot-reloadable (a `put_policy` calls
    // `set_max` on the shared `per_ip_limit`), so the shed-log below reads it
    // **live** rather than capturing it here. See `transport.md` § Abuse
    // posture item (2).

    // Shedding is what an admin needs to see when a cap bites, but logging
    // it per-rejection floods at connection-attempt rate — so both caps report
    // through a rate-limited counter that carries the batch count.
    let global_shed = fauna_conn_limit::ShedCounter::new();
    let per_ip_shed = std::sync::Arc::new(fauna_conn_limit::ShedCounter::new());

    // Bounds concurrent PROXY-header parses on loopback connections — the
    // window between the global permit above and the per-IP/loopback permit
    // below, during which a stalling co-resident peer isn't yet charged
    // against `LOOPBACK_MAX_CONNS` at all.
    // See `fauna_conn_limit::HEADER_PARSE_MAX_CONCURRENT`.
    let header_parse_gate = std::sync::Arc::new(tokio::sync::Semaphore::new(
        fauna_conn_limit::HEADER_PARSE_MAX_CONCURRENT,
    ));
    let header_parse_shed = std::sync::Arc::new(fauna_conn_limit::ShedCounter::new());

    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::error!("TCP accept error: {e}");
                continue;
            }
        };
        // Bound every permit's lifetime. A peer that vanishes without a clean
        // TCP close otherwise leaves an `ESTABLISHED` socket forever, burning
        // one of its IP's slots permanently — the failure that took the
        // fronting router's `:443` down on 2026-07-31 (`fauna-conn-limit` crate
        // docs). Nest's peer is usually the router over loopback, which cannot
        // vanish, but a direct-listener nest (no router, home/bare-IP deploys)
        // faces the real client here and has the identical exposure.
        if let Err(e) = fauna_conn_limit::arm_dead_peer_detection(&stream) {
            tracing::warn!("could not arm TCP keepalive for {addr}: {e}; connection is leak-prone");
        }
        let permit = match std::sync::Arc::clone(&conn_limit).try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                if let Some(shed) = global_shed.record() {
                    // `(sample: …)`, not the tripping address: this counter is
                    // global, so `shed` sums every accept refused during the
                    // window, not just the printed `addr`
                    // (transport-connection.md § Abuse posture, "the printed
                    // identifier on a shed line is a sample, never an
                    // attribution").
                    tracing::warn!(
                        "connection cap ({MAX_CONNECTIONS}) reached; shedding \
                         (sample: {addr}) ({shed} shed since last line)"
                    );
                }
                continue; // `stream` dropped here → connection closed
            }
        };
        let acceptor = acceptor.clone();
        let per_ip_limit = std::sync::Arc::clone(&per_ip_limit);
        let per_ip_shed = std::sync::Arc::clone(&per_ip_shed);
        let router_auth = router_auth.clone();
        let header_parse_gate = std::sync::Arc::clone(&header_parse_gate);
        let header_parse_shed = std::sync::Arc::clone(&header_parse_shed);
        let svc = match make_service.call(addr).await {
            Ok(s) => s,
            Err(e) => match e {},
        };
        // spawn-ok(connection-scoped): dies with its connection; WS conns 1001-drain at teardown, plain-HTTP keep-alives are bounded by hyper's idle timeout
        tokio::spawn(async move {
            // `permit` (the global slot) is moved into the `Admitted` socket
            // wrapper below, NOT held by this task — see `Admitted` for why a
            // task-held permit counts handshakes, not connections.
            // Resolve the connection source. On the single-box deploy the
            // fauna-sni-router fronts :443 and prepends a PROXY-v2 header
            // conveying the real client; we read it (only from a loopback TCP
            // peer — the local router) and use the conveyed address. A
            // headerless connection (the in-container bridge dialing nest's
            // loopback directly, whose first byte is the TLS handshake type
            // 0x16, never the PROXY signature 0x0D) keeps its genuine loopback
            // `addr`, so the loopback gate still recognises it.
            let (stream, source_addr) = match read_optional_proxy_header(
                stream,
                addr,
                router_auth.as_deref(),
                &header_parse_gate,
            )
            .await
            {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // The header-parse gate was saturated — shed rather than
                    // let this connection join a stall it can't yet be
                    // distinguished from.
                    // `stream` was already dropped inside
                    // `read_optional_proxy_header` when the gate rejected it;
                    // `permit` (the global slot) drops here, freeing it
                    // immediately rather than holding it for the full parse
                    // window.
                    if let Some(shed) = header_parse_shed.record() {
                        // `(sample: …)` — same reasoning as `global_shed`
                        // above: one counter for every concurrent parse this
                        // gate sheds, not just `addr`.
                        tracing::warn!(
                            "header-parse gate ({}) reached; shedding \
                             (sample: loopback connection {addr}) before its \
                             source resolved ({shed} shed since last line)",
                            fauna_conn_limit::HEADER_PARSE_MAX_CONCURRENT
                        );
                    }
                    return;
                }
                Err(e) => {
                    tracing::debug!("PROXY-v2 header read failed from {addr}: {e}");
                    return;
                }
            };
            // Per-source-IP cap: now that the real client IP is resolved, shed
            // this connection (before the expensive TLS handshake) if the source
            // already holds its ceiling of live connections — the admin's cap
            // for an external source, the hard-coded loopback ceiling for a
            // co-resident one (`fauna_conn_limit::LOOPBACK_MAX_CONNS`) — so one
            // source can't monopolise the global pool, and a leaking loopback
            // process is bounded AND visible: this warn is the line the
            // 2026-08-22 incident's nest never had (16 k accepted sockets, zero
            // log lines). The permit decrements when the SOCKET closes (it
            // rides the `Admitted` wrapper below), not when this task ends.
            let ip_permit = match per_ip_limit.try_acquire(source_addr.ip()) {
                Some(p) => p,
                None => {
                    if let Some(shed) = per_ip_shed.record() {
                        let kind = if source_addr.ip().is_loopback() {
                            "loopback connection ceiling"
                        } else {
                            "per-IP connection cap"
                        };
                        // `(sample: …)` — `per_ip_shed` is ONE counter shared
                        // across every source IP this cap meters (the
                        // ceiling is per-key, the counter is not), so `shed`
                        // sums sheds across all sources, not just
                        // `source_addr`.
                        tracing::warn!(
                            "{kind} ({}) reached; shedding (sample: \
                             {source_addr}) ({shed} shed since last line)",
                            per_ip_limit.ceiling_for(source_addr.ip())
                        );
                    }
                    return; // `stream` + `permit` dropped here → connection closed
                }
            };
            // From here on both permits live exactly as long as the socket —
            // through the TLS handshake, through the WebSocket upgrade, into
            // the upgrade task that owns the socket for the rest of its life.
            let stream = Admitted {
                io: stream,
                _global: permit,
                _per_ip: ip_permit,
            };
            // Inject ConnectInfo(source addr) into each request. Both flavours
            // build the per-connection service by hand (this loop has no
            // `into_make_service_with_connect_info`), and the loopback gate for
            // the bridge's pre-identity `fauna.bridges.request_enrollment`
            // reads `ConnectInfo` to confirm the caller is on the container
            // loopback; without it the bridge's self-enrollment (which rides
            // TLS, post loopback-dial fix) is rejected (`ok=false`) and mail
            // never serves. The wrapper only inserts an extension, so the
            // request (incl. hyper's `OnUpgrade`) is otherwise untouched and WS
            // upgrades still work.
            let hyper_svc = hyper_util::service::TowerToHyperService::new(WithConnectInfo {
                inner: svc,
                addr: source_addr,
            });
            match acceptor {
                Some(acceptor) => {
                    // Bound the TLS handshake so a peer that opens a connection
                    // and then stalls mid-handshake can't pin a task (slow-loris
                    // / handshake flood — security review § D1). 15 s is
                    // generous; a real handshake completes in well under a
                    // second.
                    let tls_stream = match tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        acceptor.accept(stream),
                    )
                    .await
                    {
                        Ok(Ok(s)) => s,
                        Ok(Err(e)) => {
                            tracing::debug!("TLS handshake failed from {source_addr}: {e}");
                            return;
                        }
                        Err(_) => {
                            tracing::debug!("TLS handshake timed out from {source_addr}");
                            return;
                        }
                    };
                    serve_http_connection(tls_stream, hyper_svc, addr).await;
                }
                None => serve_http_connection(stream, hyper_svc, addr).await,
            }
        });
    }
}

/// Serve one admitted connection with hyper — shared by both flavours of
/// [`serve_admitted`], so upgrade handling and the Slowloris guard cannot
/// diverge between TLS and plain.
async fn serve_http_connection<IO>(
    io: IO,
    hyper_svc: hyper_util::service::TowerToHyperService<WithConnectInfo<axum::Router>>,
    addr: std::net::SocketAddr,
) where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let io = hyper_util::rt::TokioIo::new(io);
    let mut builder =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    // Bound how long a connection may dawdle before sending its complete
    // request head — a slow-header (Slowloris) guard so a trickle of
    // header bytes can't hold a connection (and its semaphore permit)
    // open indefinitely (security review § D1). WS-RPC upgrades send
    // their head promptly, so 20 s never bites a real client.
    // `header_read_timeout` REQUIRES a registered hyper `Timer`; the
    // auto builder sets none by default, and hyper *panics*
    // ("timeout set, but no timer set") on the first request without
    // one — so the TokioTimer here is load-bearing, not optional.
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(std::time::Duration::from_secs(20));
    // `serve_connection_with_upgrades`, NOT `serve_connection`: the
    // WS-RPC transport (every client↔nest path) rides an HTTP/1.1
    // `Upgrade: websocket`. Plain `serve_connection` writes the 101 but
    // never drives the upgraded byte-stream, so over TLS the socket is
    // torn down the instant after the handshake — the client sees
    // "rpc disconnected (was_in_flight=false)". The plain-HTTP path used
    // to ride `axum::serve`, which handles upgrades, which is why only
    // HTTPS deployments (example.com / any real VPS) broke; both flavours
    // now share this one call. The connection future must be pinned for
    // the upgradeable variant.
    let conn = builder.serve_connection_with_upgrades(io, hyper_svc);
    tokio::pin!(conn);
    if let Err(e) = conn.as_mut().await {
        tracing::debug!("HTTP connection error from {addr}: {e}");
    }
}

/// An admitted connection's socket, carrying the permits its admission took —
/// the global slot and the per-IP slot — so they live **exactly as long as the
/// socket does**.
///
/// Why the permits cannot simply be held by the connection task: hyper's
/// connection future **resolves at a WebSocket upgrade**, handing the IO to
/// the `OnUpgrade` future that axum drives in its own spawned task. Every
/// fauna client↔nest connection is an upgraded WebSocket for all but its first
/// milliseconds, so a task-held permit was released the instant the connection
/// became a real one — the global and per-IP caps counted concurrent
/// *handshakes*, never live connections, and a leaking peer holding 16 k
/// upgraded sockets held zero permits. Measured on 2026-08-25 by the red form
/// of `tests/plain_http_admission.rs` (two held WebSockets, ceiling 2, third
/// connection admitted). Wrapping the socket puts the permits where the
/// socket goes: through the TLS handshake (`TlsStream<Admitted<TcpStream>>`),
/// through the upgrade, into the WS task, and out only when that task drops
/// the socket. The SNI router never had this problem — its permit lives with
/// the L4 splice, which IS the socket's lifetime.
///
/// Plain delegation; every field is `Unpin`, so no pin projection is needed.
struct Admitted<IO> {
    io: IO,
    _global: tokio::sync::OwnedSemaphorePermit,
    _per_ip: fauna_conn_limit::PerIpPermit,
}

impl<IO: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Admitted<IO> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<IO: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Admitted<IO> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
}

/// Read an optional PROXY-protocol-v2 header from a freshly-accepted,
/// TLS-bound connection and return the stream (positioned at the TLS
/// ClientHello) plus the resolved connection source address.
///
/// A header is **only trusted from a loopback TCP peer** — i.e. the in-container
/// `fauna-sni-router`, which (besides the local mail bridge) is the only thing
/// that can reach nest's unpublished loopback port. A non-loopback peer (a
/// hypothetical fronting load balancer on another host) is not trusted and
/// keeps its TCP address; that would need an explicit trusted-proxy allowlist.
/// A connection whose first byte isn't the PROXY signature `0x0D` (the bridge's
/// TLS ClientHello starts with `0x16`) is returned untouched, so the bridge's
/// direct loopback dial keeps its genuine loopback address and still passes the
/// `request_enrollment` gate. See `fauna_proxy_protocol` (security review
/// tracked internally).
///
/// **Router-distinguished trust.** Loopback alone is no longer enough:
/// after the co-resident UID split, the mail bridges (`fauna-mta`/`fauna-mda`)
/// are *also* loopback peers, so a compromised bridge could forge a PROXY header
/// to spoof a source IP (evading per-source rate limits / poisoning the MDA
/// AUTH-lockout key). When `expected_router_auth` is `Some` (a router-fronted
/// box that provisioned the secret), a header is honoured only if it carries a
/// matching [`fauna_proxy_protocol::TLV_TYPE_ROUTER_AUTH`] TLV — the router
/// writes the secret from a file only it and nest can read, so a bridge UID
/// cannot produce one. A header with a missing/wrong secret falls back to the
/// genuine loopback peer (untrusted), not the spoofed address. When `None`
/// (non-router box, or a provisioning gap), the trust-any-loopback-header
/// behaviour stands. See `docs/goal/architecture/security.md` § Co-resident
/// process trust boundary.
///
/// **A non-loopback `tcp_peer` never touches `header_parse_gate`** — the early
/// return below is unconditional, so a genuinely external client is admitted
/// regardless of loopback gate contention; only a loopback peer can stall this
/// read at all, which is what the gate bounds. A loopback peer acquires the gate with a non-blocking `try_acquire`
/// *before* the first byte is read; a full gate returns
/// `Err(ErrorKind::WouldBlock)` immediately — the caller sheds the connection
/// rather than let it join a stall it can't yet be distinguished from.
async fn read_optional_proxy_header(
    mut stream: tokio::net::TcpStream,
    tcp_peer: std::net::SocketAddr,
    expected_router_auth: Option<&[u8]>,
    header_parse_gate: &tokio::sync::Semaphore,
) -> std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
    use tokio::io::AsyncReadExt;

    if !tcp_peer.ip().is_loopback() {
        return Ok((stream, tcp_peer));
    }
    let _gate_permit = header_parse_gate.try_acquire().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "header-parse gate saturated",
        )
    })?;
    let parse = async {
        // A 1-byte peek (non-consuming) distinguishes a PROXY header (0x0D)
        // from a TLS ClientHello (0x16) without disturbing the latter.
        let mut first = [0u8; 1];
        let n = stream.peek(&mut first).await?;
        if n == 0 || first[0] != fauna_proxy_protocol::SIGNATURE[0] {
            return Ok::<std::net::SocketAddr, std::io::Error>(tcp_peer);
        }
        // Loopback peer + PROXY signature byte ⇒ a header follows. Consume it
        // with read_exact (not peek) so the ClientHello that follows stays
        // intact for the TLS acceptor.
        let mut prefix = [0u8; fauna_proxy_protocol::PREFIX_LEN];
        stream.read_exact(&mut prefix).await?;
        let Some(info) = fauna_proxy_protocol::parse_prefix(&prefix) else {
            return Ok(tcp_peer);
        };
        // Defensive cap: our router emits a 12/36-byte address block plus, when
        // authenticating, a small router-auth TLV (≤ ~1 KiB). A wildly large
        // declared length from a buggy/hostile loopback peer is treated as
        // malformed (the stream is now out of sync, so the TLS handshake fails
        // and the connection drops — acceptable).
        if info.addr_len > 1024 {
            return Ok(tcp_peer);
        }
        let mut block = vec![0u8; info.addr_len];
        stream.read_exact(&mut block).await?;
        let Some(src) = fauna_proxy_protocol::parse_src(info.fam_proto, &block) else {
            return Ok(tcp_peer);
        };
        // On a router-fronted box, honour the conveyed source only when
        // the header proves it came from the router (matching auth TLV);
        // otherwise fall back to the genuine loopback peer so a forging bridge
        // gains nothing. Constant-time compare avoids a timing oracle on the
        // secret.
        match expected_router_auth {
            Some(expected) => {
                match fauna_proxy_protocol::parse_router_auth_tlv(info.fam_proto, &block) {
                    Some(got) if fauna_core::secret::constant_time_eq(got, expected) => Ok(src),
                    _ => Ok(tcp_peer),
                }
            }
            None => Ok(src),
        }
    };
    // Bound a slow/stalled header so it can't pin the task; the broader
    // TLS-handshake timeout is tracked separately (security review § D1).
    let resolved = match tokio::time::timeout(std::time::Duration::from_secs(10), parse).await {
        Ok(Ok(addr)) => addr,
        Ok(Err(e)) => return Err(e),
        Err(_elapsed) => tcp_peer,
    };
    Ok((stream, resolved))
}

/// The router-auth secret nest expects in a PROXY-v2 header's auth TLV,
/// loaded once at serve start. Returns `None` — keeping the "trust any
/// loopback PROXY header" behaviour — unless this nest is fronted by the SNI
/// router (`is_fronted_by_router`) AND the artifact provisioned a secret,
/// supplied as hex via the `FAUNA_ROUTER_PROXY_SECRET` IPC env. The nest
/// run-script reads `/data/keys/router/proxy-secret` as root and env-passes it
/// (the same pattern as `FAUNA_INBOUND_DELIVER_KEY`); passing it via env, not
/// argv, keeps it out of the world-readable `/proc/<pid>/cmdline` while
/// `/proc/<pid>/environ` is readable only by the process's own UID — so a
/// co-resident bridge UID cannot read it. A router-fronted box with no secret
/// is a provisioning gap: we log loudly and stay permissive, since failing
/// closed would strand every external client as an un-capped loopback source
/// (worse than the spoofing risk, and against works-out-of-the-box).
fn load_router_auth_secret() -> Option<Vec<u8>> {
    if !is_fronted_by_router() {
        return None;
    }
    let raw = std::env::var("FAUNA_ROUTER_PROXY_SECRET").unwrap_or_default();
    let hex = raw.trim();
    if hex.is_empty() {
        tracing::warn!(
            "FAUNA_FRONTED_BY_ROUTER is set but FAUNA_ROUTER_PROXY_SECRET is empty — \
             PROXY-v2 headers are NOT authenticated; a co-resident process could \
             spoof a source IP. Check /data/keys/router/proxy-secret provisioning."
        );
        return None;
    }
    match hex::decode(hex) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        _ => {
            tracing::error!(
                "FAUNA_ROUTER_PROXY_SECRET is not valid non-empty hex — PROXY-v2 headers \
                 are NOT authenticated."
            );
            None
        }
    }
}

/// Tower middleware that inserts `ConnectInfo(addr)` into each request's
/// extensions. `serve_tls` builds its per-connection service by hand (unlike
/// the plain path's `into_make_service_with_connect_info`), so without this the
/// peer address is absent over TLS and the loopback gate for the bridge's
/// `request_enrollment` rejects it. axum 0.8's `Router` is generic over the
/// body, so this stays generic over `B` and only touches the head.
#[derive(Clone)]
struct WithConnectInfo<S> {
    inner: S,
    addr: std::net::SocketAddr,
}

impl<S, B> tower_service::Service<axum::http::Request<B>> for WithConnectInfo<S>
where
    S: tower_service::Service<axum::http::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: axum::http::Request<B>) -> Self::Future {
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(self.addr));
        self.inner.call(req)
    }
}

#[cfg(test)]
mod proxy_header_tests {
    //! `read_optional_proxy_header` is the nest half of the PROXY-v2 fix: an external client reaches nest via the
    //! SNI router, which prepends a header carrying the real client IP, while
    //! the in-container bridge dials nest's loopback directly with no header.
    //! These tests drive the parser over a real loopback socket pair.
    use super::read_optional_proxy_header;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Semaphore;

    /// An ample gate — large enough that no test in this module (other than
    /// the dedicated gate tests below, which build their own) ever contends
    /// on it, so it behaves as "no gate" for every other test's purposes.
    fn ample_gate() -> Semaphore {
        Semaphore::new(fauna_conn_limit::HEADER_PARSE_MAX_CONCURRENT)
    }

    /// Connect over loopback, send `payload`, and return what the parser
    /// resolves as the source plus the first 5 bytes the server reads *after*
    /// the (optional) header — i.e. proof the ClientHello survives.
    async fn resolve_with_auth(
        payload: Vec<u8>,
        expected_auth: Option<&[u8]>,
    ) -> (std::net::SocketAddr, [u8; 5]) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // spawn-ok(test)
        let client = tokio::spawn(async move {
            let mut s = TcpStream::connect(addr).await.unwrap();
            s.write_all(&payload).await.unwrap();
            s.flush().await.unwrap();
            let mut sink = [0u8; 1];
            let _ = s.read(&mut sink).await; // park until the server drops
        });
        let (stream, peer) = listener.accept().await.unwrap();
        let gate = ample_gate();
        let (mut stream, resolved) = read_optional_proxy_header(stream, peer, expected_auth, &gate)
            .await
            .unwrap();
        let mut trailing = [0u8; 5];
        stream.read_exact(&mut trailing).await.unwrap();
        drop(stream);
        let _ = client.await;
        (resolved, trailing)
    }

    /// Legacy (no router-auth expected) resolver — most tests run unauthenticated
    /// (a direct-listener nest), matching the prior behaviour.
    async fn resolve(payload: Vec<u8>) -> (std::net::SocketAddr, [u8; 5]) {
        resolve_with_auth(payload, None).await
    }

    const CLIENT_HELLO: [u8; 5] = [0x16, 0x03, 0x01, 0x00, 0x10];
    const ROUTER_SECRET: &[u8] = b"32-bytes-of-router-auth-secret!!";

    #[tokio::test]
    async fn proxy_header_resolves_real_client_and_preserves_clienthello() {
        let real: std::net::SocketAddr = "203.0.113.7:54321".parse().unwrap();
        let dst: std::net::SocketAddr = "198.51.100.1:443".parse().unwrap();
        let mut payload = fauna_proxy_protocol::encode_v2(real, dst);
        payload.extend_from_slice(&CLIENT_HELLO);
        let (resolved, trailing) = resolve(payload).await;
        assert_eq!(resolved, real, "the PROXY-conveyed client IP must be used");
        assert_eq!(
            trailing, CLIENT_HELLO,
            "the ClientHello must survive header consumption"
        );
    }

    #[tokio::test]
    async fn headerless_loopback_dial_keeps_loopback_addr() {
        // The bridge dials nest's loopback directly — no PROXY header, straight
        // to the TLS ClientHello (first byte 0x16). It must keep its loopback
        // address so the `request_enrollment` gate still recognises it.
        let (resolved, trailing) = resolve(CLIENT_HELLO.to_vec()).await;
        assert!(
            resolved.ip().is_loopback(),
            "a direct headerless loopback dial keeps its loopback address, got {resolved}"
        );
        assert_eq!(trailing, CLIENT_HELLO, "the ClientHello must be untouched");
    }

    // ---- Router-distinguished PROXY trust ----

    #[tokio::test]
    async fn authed_header_with_matching_secret_is_trusted() {
        // The router writes the auth TLV with the provisioned secret; nest,
        // expecting that secret, honours the conveyed client address.
        let real: std::net::SocketAddr = "203.0.113.7:54321".parse().unwrap();
        let dst: std::net::SocketAddr = "198.51.100.1:443".parse().unwrap();
        let mut payload = fauna_proxy_protocol::encode_v2_authed(real, dst, Some(ROUTER_SECRET));
        payload.extend_from_slice(&CLIENT_HELLO);
        let (resolved, trailing) = resolve_with_auth(payload, Some(ROUTER_SECRET)).await;
        assert_eq!(resolved, real, "a router-authenticated header is trusted");
        assert_eq!(trailing, CLIENT_HELLO, "the ClientHello must survive");
    }

    #[tokio::test]
    async fn spoofed_header_without_secret_is_not_trusted() {
        // A co-resident bridge forges a PROXY header (no auth TLV) to spoof a
        // source IP. nest expects the secret, so the forged address is rejected
        // and the connection keeps its genuine loopback peer.
        let spoofed: std::net::SocketAddr = "203.0.113.7:54321".parse().unwrap();
        let dst: std::net::SocketAddr = "198.51.100.1:443".parse().unwrap();
        let mut payload = fauna_proxy_protocol::encode_v2(spoofed, dst); // no TLV
        payload.extend_from_slice(&CLIENT_HELLO);
        let (resolved, trailing) = resolve_with_auth(payload, Some(ROUTER_SECRET)).await;
        assert!(
            resolved.ip().is_loopback(),
            "a header without the router secret must NOT be trusted; got {resolved}"
        );
        assert_eq!(trailing, CLIENT_HELLO, "the ClientHello must survive");
    }

    #[tokio::test]
    async fn header_with_wrong_secret_is_not_trusted() {
        // A bridge that knows the TLV shape but not the secret is still rejected.
        let spoofed: std::net::SocketAddr = "203.0.113.7:54321".parse().unwrap();
        let dst: std::net::SocketAddr = "198.51.100.1:443".parse().unwrap();
        let mut payload =
            fauna_proxy_protocol::encode_v2_authed(spoofed, dst, Some(b"the-wrong-secret-guess!!"));
        payload.extend_from_slice(&CLIENT_HELLO);
        let (resolved, trailing) = resolve_with_auth(payload, Some(ROUTER_SECRET)).await;
        assert!(
            resolved.ip().is_loopback(),
            "a header with a wrong secret must NOT be trusted; got {resolved}"
        );
        assert_eq!(trailing, CLIENT_HELLO, "the ClientHello must survive");
    }

    #[tokio::test]
    async fn headerless_dial_still_works_when_auth_expected() {
        // Even on an auth-expecting (router-fronted) nest, the bridge's direct
        // headerless loopback dial keeps its loopback address — the auth gate
        // only governs *PROXY headers*, never the headerless path.
        let (resolved, trailing) =
            resolve_with_auth(CLIENT_HELLO.to_vec(), Some(ROUTER_SECRET)).await;
        assert!(
            resolved.ip().is_loopback(),
            "a headerless dial is unaffected by the auth gate; got {resolved}"
        );
        assert_eq!(trailing, CLIENT_HELLO, "the ClientHello must be untouched");
    }

    // ---- Header-parse gate ----
    //
    // Before this fix, the accept loop took its global permit before
    // resolving a connection's source, so a stalling loopback peer held a
    // global permit for up to the 10s parse timeout without ever being
    // charged against `LOOPBACK_MAX_CONNS` — that ceiling only binds *after*
    // resolution. `header_parse_gate` bounds the pre-resolution window
    // itself. These tests build their own small gates, independent of
    // `HEADER_PARSE_MAX_CONCURRENT`, so they stay fast and deterministic.

    #[tokio::test]
    async fn non_loopback_peer_bypasses_the_header_parse_gate() {
        // A fully saturated gate (zero permits) — if the non-loopback early
        // return in `read_optional_proxy_header` ever regressed to route
        // through the gate, this would shed or hang instead of resolving
        // promptly. A real external client must never contend on a gate that
        // exists to bound *loopback* stalls.
        let gate = Semaphore::new(0);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // spawn-ok(test)
        let client = tokio::spawn(async move {
            let mut s = TcpStream::connect(addr).await.unwrap();
            s.write_all(&CLIENT_HELLO).await.unwrap();
            s.flush().await.unwrap();
            let mut sink = [0u8; 1];
            let _ = s.read(&mut sink).await; // park until the server drops
        });
        let (stream, _real_peer) = listener.accept().await.unwrap();
        let fake_external: std::net::SocketAddr = "203.0.113.7:54321".parse().unwrap();
        let (mut stream, resolved) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_optional_proxy_header(stream, fake_external, None, &gate),
        )
        .await
        .expect("a non-loopback peer must resolve promptly, unaffected by gate saturation")
        .unwrap();
        assert_eq!(
            resolved, fake_external,
            "a non-loopback peer keeps its TCP address and never touches the gate"
        );
        let mut trailing = [0u8; 5];
        stream.read_exact(&mut trailing).await.unwrap();
        assert_eq!(trailing, CLIENT_HELLO, "the ClientHello must survive");
        drop(stream);
        let _ = client.await;
    }

    #[tokio::test]
    async fn loopback_peer_is_shed_when_the_header_parse_gate_is_saturated() {
        // Simulates `HEADER_PARSE_MAX_CONCURRENT` stalling co-resident peers
        // already occupying every slot: the next loopback connection must be
        // shed with a distinguishable error, not left to stall on the peek.
        let gate = Semaphore::new(0);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // spawn-ok(test)
        let client = tokio::spawn(async move {
            let _s = TcpStream::connect(addr).await.unwrap();
            // Send nothing — irrelevant, since the gate must shed this
            // connection before any read is even attempted.
        });
        let (stream, peer) = listener.accept().await.unwrap();
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_optional_proxy_header(stream, peer, None, &gate),
        )
        .await
        .expect("a saturated gate must shed immediately, not stall for the 10s parse window")
        .expect_err("a saturated gate must shed a loopback connection, not admit it");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::WouldBlock,
            "a gate-saturated shed must be distinguishable from a genuine parse failure \
             (the caller logs the two very differently)"
        );
        let _ = client.await;
    }

    #[tokio::test]
    async fn loopback_overflow_past_the_header_parse_gate_is_shed_without_stalling() {
        // The end-to-end shape: fill a small gate with genuinely-stalling
        // connections, then prove the next one is shed fast rather than
        // holding a slot for the full parse window — the behavior that does
        // NOT hold on unfixed code (there, every loopback connection stalls
        // for up to 10s regardless of how many came before it).
        const GATE_MAX: usize = 2;
        let gate = Arc::new(Semaphore::new(GATE_MAX));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Two genuinely-stalling clients: connect and send nothing, so the
        // server's peek() blocks once each has taken a gate permit.
        let mut stallers = Vec::new();
        let mut server_tasks = Vec::new();
        for _ in 0..GATE_MAX {
            stallers.push(TcpStream::connect(addr).await.unwrap());
            let (stream, peer) = listener.accept().await.unwrap();
            let gate = Arc::clone(&gate);
            // spawn-ok(test)
            server_tasks.push(tokio::spawn(async move {
                read_optional_proxy_header(stream, peer, None, &gate).await
            }));
        }
        // Deadline-poll (convention 14) until both have actually taken their
        // gate permit, rather than guessing a settle time.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while gate.available_permits() > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the two stalling connections never took the gate"
            );
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        // A third, overflow loopback connection — also sends nothing — must
        // be shed almost immediately rather than stall for the full window.
        let _overflow_client = TcpStream::connect(addr).await.unwrap();
        let (overflow_stream, overflow_peer) = listener.accept().await.unwrap();
        let overflow_gate = Arc::clone(&gate);
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_optional_proxy_header(overflow_stream, overflow_peer, None, &overflow_gate),
        )
        .await
        .expect(
            "a gate-saturated connection must be shed within ~2s, not held for the 10s parse window",
        )
        .expect_err("the overflow connection must be shed, not admitted");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);

        // The two within-bound stallers are still parked, not shed — the
        // gate holds exactly GATE_MAX, not zero.
        for task in &server_tasks {
            assert!(
                !task.is_finished(),
                "a within-bound stalling connection must not be shed"
            );
        }

        drop(stallers);
        for task in server_tasks {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod per_ip_cap_resolve_tests {
    use super::resolve_tls_per_ip_cap;

    /// The cap has two sources and no third: the admin's `transport_policy`
    /// row, else the constant. An environment variable naming the cap is not
    /// read — a hand-set value there would be a configuration surface outside
    /// the apps (`principles.md` § One configuration surface).
    #[test]
    fn the_environment_names_no_per_ip_cap() {
        // SAFETY: the variable is read by nothing in this binary once the
        // resolver ignores it, so no parallel test observes the write.
        unsafe { std::env::set_var("FAUNA_MAX_TLS_CONNECTIONS_PER_IP", "7") };
        assert_eq!(
            resolve_tls_per_ip_cap(None),
            fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
            "no row ⇒ the constant, whatever the environment says"
        );
        assert_eq!(
            resolve_tls_per_ip_cap(Some(0)),
            fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
            "a zero row is unset, and falls to the constant"
        );
        assert_eq!(resolve_tls_per_ip_cap(Some(64)), 64, "the row binds");
    }
}

#[cfg(test)]
mod serving_bind_resolve_tests {
    //! `resolve_serving_bind_addr` — the Pillar A boot-resolve: a direct-listener
    //! nest overrides the bind seed's PORT with the admin-set `serving_port`
    //! (interface/host preserved); behind the SNI router it is inert. The
    //! `FAUNA_FRONTED_BY_ROUTER` env read lives in `is_fronted_by_router`; this
    //! drives the pure resolve with an explicit `fronted` flag (no env races).
    use super::resolve_serving_bind_addr;
    use crate::db::CacheDb;
    use std::sync::Arc;

    #[tokio::test]
    async fn direct_listener_overrides_port_keeps_host() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let seed: std::net::SocketAddr = "0.0.0.0:443".parse().unwrap();
        // No admin choice yet → the seed passes through unchanged.
        assert_eq!(resolve_serving_bind_addr(seed, &db, false).await, seed);
        // Admin picks 8443 → the PORT is overridden, the interface/host preserved.
        db.set_serving_port(8443).await.unwrap();
        let resolved = resolve_serving_bind_addr(seed, &db, false).await;
        assert_eq!(resolved.port(), 8443);
        assert_eq!(resolved.ip(), seed.ip());
    }

    #[tokio::test]
    async fn fronted_by_router_is_inert() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        // Docker internal seed; admin set a non-default serving_port.
        let seed: std::net::SocketAddr = "0.0.0.0:3000".parse().unwrap();
        db.set_serving_port(8443).await.unwrap();
        // Behind the SNI router the singleton is inert — the seed (internal port)
        // is returned unchanged so the router's `127.0.0.1:3000` forward holds.
        assert_eq!(resolve_serving_bind_addr(seed, &db, true).await, seed);
    }
}

#[cfg(test)]
mod serving_port_bind_fallback_tests {
    //! `serving_port_bind_should_fall_back` — Issue A recoverability decision: an
    //! admin-chosen serving port the (unprivileged) nest can't bind must fall back
    //! to the seed and stay up, NOT crash-loop (client-unrecoverable). The pure
    //! decision is tested without binding real sockets.
    use super::serving_port_bind_should_fall_back;
    use std::io::{Error, ErrorKind};

    /// The canonical brick scenario: admin picked a privileged port (`80`) over the
    /// `3000` seed on an unprivileged direct-listener → `EACCES`. Fall back so the
    /// nest stays reachable for a corrected choice.
    #[test]
    fn falls_back_on_eacces_for_resolved_port() {
        let err = Error::from(ErrorKind::PermissionDenied);
        assert!(serving_port_bind_should_fall_back(80, 3000, &err));
    }

    /// An already-taken admin-chosen port (`AddrInUse`) also falls back — same
    /// unrecoverable crash-loop risk if it were fatal.
    #[test]
    fn falls_back_on_addr_in_use_for_resolved_port() {
        let err = Error::from(ErrorKind::AddrInUse);
        assert!(serving_port_bind_should_fall_back(8443, 3000, &err));
    }

    /// The seed itself failing (resolved == seed, i.e. the admin never overrode it,
    /// or chose exactly the seed) is a genuine deploy fault — do NOT fall back
    /// (there is nowhere safer to fall back *to*; propagate so it surfaces).
    #[test]
    fn does_not_fall_back_when_failed_port_is_the_seed() {
        let eacces = Error::from(ErrorKind::PermissionDenied);
        let in_use = Error::from(ErrorKind::AddrInUse);
        assert!(!serving_port_bind_should_fall_back(3000, 3000, &eacces));
        assert!(!serving_port_bind_should_fall_back(443, 443, &in_use));
    }

    /// A non-permission/occupancy error (e.g. an invalid address) is a real fault,
    /// not "unbindable admin port" — propagate even for a resolved port.
    #[test]
    fn does_not_fall_back_on_other_errors() {
        for kind in [
            ErrorKind::AddrNotAvailable,
            ErrorKind::InvalidInput,
            ErrorKind::Other,
        ] {
            let err = Error::from(kind);
            assert!(
                !serving_port_bind_should_fall_back(80, 3000, &err),
                "{kind:?} must propagate, not fall back"
            );
        }
    }
}

#[cfg(test)]
mod internal_loopback_resolve_tests {
    //! `resolve_internal_loopback_addr` — the same-box reach fix: a direct-listener
    //! nest binds a FIXED `127.0.0.1:<port>` co-located-IPC listener alongside the
    //! external `serving_port`, so the bridge + same-box app dial a port that never
    //! moves when the admin changes `serving_port` (DoD #6). Requested via the
    //! `FAUNA_INTERNAL_LOOPBACK_PORT` IPC env (read in `internal_loopback_port_from_env`);
    //! this drives the pure resolve with an explicit `Option<u16>` (no env races).
    use super::resolve_internal_loopback_addr;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn unset_means_no_internal_listener() {
        let external: SocketAddr = "0.0.0.0:443".parse().unwrap();
        assert_eq!(resolve_internal_loopback_addr(external, None), None);
    }

    #[test]
    fn distinct_port_binds_ipv4_loopback() {
        // Windows desktop: external 0.0.0.0:443, internal fixed 3000.
        let external: SocketAddr = "0.0.0.0:443".parse().unwrap();
        assert_eq!(
            resolve_internal_loopback_addr(external, Some(3000)),
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000)),
        );
        // Also holds when the admin moved the external port (the internal stays put).
        let moved: SocketAddr = "0.0.0.0:9443".parse().unwrap();
        assert_eq!(
            resolve_internal_loopback_addr(moved, Some(3000)),
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000)),
        );
    }

    #[test]
    fn equal_port_skips_to_avoid_double_bind() {
        // If the external bind already covers this loopback port (e.g. it IS 3000),
        // a second 127.0.0.1:3000 bind would EADDRINUSE — skip it.
        let external: SocketAddr = "0.0.0.0:3000".parse().unwrap();
        assert_eq!(resolve_internal_loopback_addr(external, Some(3000)), None);
        let lo: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        assert_eq!(resolve_internal_loopback_addr(lo, Some(3000)), None);
    }
}

#[cfg(test)]
mod plain_http_guard_tests {
    //! `refuse_plain_http_for_public_domain` — the § L3 boot-time fail-safe.
    use super::refuse_plain_http_for_public_domain;

    #[test]
    fn public_domain_without_tls_is_refused() {
        assert!(
            refuse_plain_http_for_public_domain(Some("example.com"), false).is_err(),
            "a public domain in cleartext must refuse to boot"
        );
    }

    #[test]
    fn public_domain_with_tls_is_allowed() {
        assert!(
            refuse_plain_http_for_public_domain(Some("example.com"), true).is_ok(),
            "the standard deploy (public domain + TLS) must serve"
        );
    }

    #[test]
    fn local_domain_without_tls_is_allowed() {
        // localhost / LAN / tier_3 e2e legitimately serve plain HTTP.
        assert!(refuse_plain_http_for_public_domain(Some("localhost"), false).is_ok());
        assert!(refuse_plain_http_for_public_domain(Some("127.0.0.1"), false).is_ok());
    }

    #[test]
    fn no_domain_without_tls_is_allowed() {
        assert!(refuse_plain_http_for_public_domain(None, false).is_ok());
    }
}

#[cfg(test)]
mod mta_sts_honesty_tests {
    //! The MTA-STS↔cert-honesty coupling at the served `/.well-known/mta-sts.txt`
    //! body (`tls-certificates.md` § D): a domain whose MX is on the self-signed
    //! floor must never advertise `enforce`, or an enforcing sender refuses the
    //! non-WebPKI MX and inbound mail bounces.
    use super::mta_sts_policy_body;
    use crate::acme::{CERT_RENEWAL_LEAD_SECS, ServedCertFacts};
    use fauna_mail::outbound::mta_sts::MtaStsMode;

    const NOW: i64 = 1_000_000;
    const MX: &str = "mail.example.com";

    /// A WebPKI-trusted covering cert well beyond the renewal lead (ValidTrusted).
    fn facts(is_floor: bool, covers: bool) -> Option<ServedCertFacts> {
        Some(ServedCertFacts {
            not_before_unix: 0,
            not_after_unix: NOW + CERT_RENEWAL_LEAD_SECS + 86_400,
            is_floor,
            covers,
        })
    }

    #[test]
    fn floor_mx_downgrades_enforce_to_testing() {
        let body = mta_sts_policy_body(MX, MtaStsMode::Enforce, 86_400, facts(true, true), NOW);
        assert!(
            body.contains("mode: testing") && !body.contains("mode: enforce"),
            "a floor MX must downgrade enforce->testing: {body}"
        );
        assert!(body.contains("mx: mail.example.com"));
    }

    #[test]
    fn trusted_mx_advertises_enforce() {
        let body = mta_sts_policy_body(MX, MtaStsMode::Enforce, 86_400, facts(false, true), NOW);
        assert!(
            body.contains("mode: enforce"),
            "a trusted covering MX advertises enforce: {body}"
        );
    }

    #[test]
    fn no_served_cert_does_not_advertise_enforce() {
        // No resolver / pending cert reads as on-floor → never enforce.
        let body = mta_sts_policy_body(MX, MtaStsMode::Enforce, 86_400, None, NOW);
        assert!(body.contains("mode: testing"), "{body}");
    }

    #[test]
    fn non_covering_trusted_cert_does_not_advertise_enforce() {
        // A trusted cert that does not cover mail.<primary> is still on-floor for
        // the MX (cert_health_state → OnFloorRenewNeeded) → never enforce.
        let body = mta_sts_policy_body(MX, MtaStsMode::Enforce, 86_400, facts(false, false), NOW);
        assert!(body.contains("mode: testing"), "{body}");
    }

    #[test]
    fn stored_testing_stays_testing_even_on_trusted_mx() {
        // The stored mode is a ceiling — a trusted MX never upgrades testing.
        let body = mta_sts_policy_body(MX, MtaStsMode::Testing, 86_400, facts(false, true), NOW);
        assert!(
            body.contains("mode: testing") && !body.contains("mode: enforce"),
            "stored testing is never upgraded: {body}"
        );
    }

    #[test]
    fn stored_max_age_is_reflected() {
        let body = mta_sts_policy_body(MX, MtaStsMode::Testing, 604_800, facts(false, true), NOW);
        assert!(
            body.contains("max_age: 604800"),
            "stored max_age reflected: {body}"
        );
    }
}
