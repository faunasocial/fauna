//! ActivityPub actor profile routes (serving actor JSON, public keys, etc.).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};

use crate::api_error::ApiError;
use axum::Router;
use axum::routing::get;

use fauna_bridge_activitypub::translate::fauna_profile_to_ap_person;
use fauna_bridge_activitypub::types::{
    ApOrderedCollection, ApPerson, NODEINFO_SCHEMA_REL, NODEINFO_SCHEMA_VERSION, NodeInfoDocument,
    NodeInfoLink, NodeInfoWellKnown,
};
use fauna_bridge_activitypub::webfinger::{build_webfinger_response, parse_acct_uri};

use rusqlite::OptionalExtension;

use crate::activitypub::db_helpers;
use crate::routes::AppState;

const CONTENT_TYPE_AP: &str = "application/activity+json; charset=utf-8";
const CONTENT_TYPE_JRD: &str = "application/jrd+json; charset=utf-8";
/// NodeInfo spec § Protocol: the document is served with a `profile` parameter
/// naming the schema it conforms to.
const CONTENT_TYPE_NODEINFO: &str =
    "application/json; charset=utf-8; profile=\"http://nodeinfo.diaspora.software/ns/schema/2.1#\"";

// ── Query param structs ──────────────────────────────────────────

#[derive(serde::Deserialize)]
struct WebFingerQuery {
    resource: String,
}

// ── Routes ──────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ap/users/{username}", get(get_actor))
        .route("/ap/users/{username}/followers", get(get_followers))
        .route("/ap/users/{username}/following", get(get_following))
        .route("/ap/users/{username}/outbox", get(get_outbox))
        .route("/ap/users/{username}/notes/{post_id}", get(get_note))
        .route("/ap/instance", get(get_instance_actor))
        .route("/ap/instance/outbox", get(get_instance_outbox))
        .route("/.well-known/webfinger", get(webfinger))
        .route("/.well-known/nodeinfo", get(nodeinfo))
        .route("/nodeinfo/2.1", get(nodeinfo_document))
}

// ── Helpers ─────────────────────────────────────────────────────

/// Returns true if the Accept header indicates an ActivityPub client.
fn accepts_activity_json(headers: &HeaderMap) -> bool {
    headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|accept| {
            accept.contains("application/activity+json") || accept.contains("application/ld+json")
        })
        .unwrap_or(false)
}

/// Build a JSON response with a custom Content-Type header.
fn ap_json_response<T: serde::Serialize>(body: &T, content_type: &'static str) -> Response {
    let json_bytes = match serde_json::to_vec(body) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("ap_json_response serialize error: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let mut resp = axum::response::Response::new(axum::body::Body::from(json_bytes));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static(content_type),
    );
    resp
}

// ── Handlers ────────────────────────────────────────────────────

/// GET /ap/users/{username}
/// Serve the AP Person actor document.
async fn get_actor(
    State(state): State<Arc<AppState>>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Response {
    let domain = state.handle_domain();

    // Look up AP account by username
    let conn = state.db.conn().await;
    let account = match db_helpers::get_account_by_username(&conn, &username) {
        Ok(Some(a)) => a,
        Ok(None) => return ApiError::not_found("user not found").into_response(),
        Err(e) => {
            tracing::error!("ap get_actor db error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    drop(conn);

    if !account.enabled {
        return ApiError::not_found("user not found").into_response();
    }

    // The follow path this account is in — the negation of its *accept follows
    // by itself* setting, on every shape of the document below
    // (`activitypub.md` § Follow requests).
    let manually_approves = !account.auto_accept_follows;

    // Attempt to load the Fauna profile for this actor from the content table.
    // actor_id in ap_accounts is stored as a lowercase hex string.
    let actor_id_bytes = fauna_core::hex32::decode(&account.actor_id).ok();

    let person: ApPerson = if let Some(actor_bytes) = actor_id_bytes {
        // Query for the latest profile content row authored by this actor
        let conn = state.db.conn().await;
        let profile_row: Option<(Vec<u8>, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT payload, blob_hash FROM content WHERE author = ?1 AND schema = 'profile'
                 ORDER BY created_at DESC LIMIT 1",
                rusqlite::params![actor_bytes.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap_or(None);
        let profile_payload: Option<Vec<u8>> = profile_row.map(|(p, _)| p);
        drop(conn);

        if let Some(payload) = profile_payload {
            // Shared signed-only verify-then-decode (the SAME helper the
            // `fauna.profile.get` read uses) — so the federation serve and the
            // client read never drift in how they verify a profile, and the
            // signed `EmbedAsBytes` `fauna.profile.set` stores is verified here
            // too (priority #2; `profile.md` § Encryption at rest).
            match fauna_core::encoding::decode_profile(&payload) {
                Ok((profile, _origin)) => fauna_profile_to_ap_person(
                    &profile,
                    &username,
                    &domain,
                    account.public_key_pem.clone(),
                    manually_approves,
                ),
                Err(e) => {
                    tracing::warn!("ap get_actor: failed to decode profile: {e}");
                    minimal_person(
                        &username,
                        &domain,
                        &account.public_key_pem,
                        manually_approves,
                    )
                }
            }
        } else {
            minimal_person(
                &username,
                &domain,
                &account.public_key_pem,
                manually_approves,
            )
        }
    } else {
        tracing::warn!("ap get_actor: could not parse actor_id hex for {username}");
        minimal_person(
            &username,
            &domain,
            &account.public_key_pem,
            manually_approves,
        )
    };

    // Content negotiation: only serve AP JSON when client asks for it (or no preference)
    let _ = accepts_activity_json(&headers); // serve JSON regardless for now
    ap_json_response(&person, CONTENT_TYPE_AP)
}

/// GET /ap/instance — the nest-level instance actor.
///
/// Anonymous by necessity, not by oversight: a peer verifying one of our signed
/// requests dereferences the `keyId` from the Signature header, and *that* fetch
/// cannot itself be verified — the recursion has to bottom out somewhere, so
/// every AP implementation serves its instance actor unauthenticated. It carries
/// no user content: a domain, an RSA public key, and the shared inbox URL that
/// `/.well-known/nodeinfo` already advertises.
///
/// Mints the key on first call, so the document a peer follows our `keyId` to
/// can never 404 (`instance_actor::ensure` is idempotent under a race).
async fn get_instance_actor(State(state): State<Arc<AppState>>) -> Response {
    match crate::activitypub::instance_actor::document(&state).await {
        Ok(person) => ap_json_response(&person, CONTENT_TYPE_AP),
        Err(e) => {
            tracing::error!(error = %e, "ap: cannot serve the instance actor");
            ApiError::internal("instance actor unavailable").into_response()
        }
    }
}

/// GET /ap/instance/outbox — always empty.
///
/// The instance actor authors nothing. The route exists because its document
/// must name an `outbox` and every link an actor advertises has to resolve —
/// the `/.well-known/nodeinfo` href that dangled for six weeks is what a
/// plausible-looking unserved link costs.
async fn get_instance_outbox(State(state): State<Arc<AppState>>) -> Response {
    let domain = match state.handle_domain_if_set() {
        Some(d) => d,
        None => return ApiError::not_found("ActivityPub not configured").into_response(),
    };
    let collection = ApOrderedCollection {
        context: Some(fauna_bridge_activitypub::types::default_context()),
        r#type: "OrderedCollection".into(),
        id: format!(
            "{}/outbox",
            fauna_bridge_activitypub::translate::instance_actor_url(&domain)
        ),
        total_items: 0,
        first: None,
        last: None,
    };
    ap_json_response(&collection, CONTENT_TYPE_AP)
}

/// Build a minimal Person when no Fauna profile is available — the
/// no-profile-override default of the shared skeleton both this route and
/// `fauna_profile_to_ap_person` (`translate.rs`) build from, so the two can
/// never again drift on how the actor URL is derived (see that function's
/// doc comment for the drift this once caused).
fn minimal_person(
    username: &str,
    domain: &str,
    public_key_pem: &str,
    manually_approves_followers: bool,
) -> ApPerson {
    fauna_bridge_activitypub::translate::ap_person_skeleton(
        username,
        domain,
        public_key_pem.into(),
        manually_approves_followers,
    )
}

/// GET /ap/users/{username}/followers
async fn get_followers(
    State(state): State<Arc<AppState>>,
    Path(username): Path<String>,
) -> Response {
    let domain = state.handle_domain();

    let conn = state.db.conn().await;
    let account = match db_helpers::get_account_by_username(&conn, &username) {
        Ok(Some(a)) => a,
        Ok(None) => return ApiError::not_found("user not found").into_response(),
        Err(e) => {
            tracing::error!("ap get_followers db error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };

    if !account.enabled {
        return ApiError::not_found("user not found").into_response();
    }

    let followers = match db_helpers::list_followers(&conn, &account.actor_id) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("ap get_followers list error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    drop(conn);

    let accepted_count = followers.iter().filter(|f| f.state == "accepted").count() as u64;
    let actor_url = format!("https://{}/ap/users/{}", domain, username);

    let collection = ApOrderedCollection {
        context: Some(fauna_bridge_activitypub::types::default_context()),
        r#type: "OrderedCollection".into(),
        id: format!("{}/followers", actor_url),
        total_items: accepted_count,
        first: None,
        last: None,
    };

    ap_json_response(&collection, CONTENT_TYPE_AP)
}

/// GET /ap/users/{username}/following
async fn get_following(
    State(state): State<Arc<AppState>>,
    Path(username): Path<String>,
) -> Response {
    let domain = state.handle_domain();

    let conn = state.db.conn().await;
    let account = match db_helpers::get_account_by_username(&conn, &username) {
        Ok(Some(a)) => a,
        Ok(None) => return ApiError::not_found("user not found").into_response(),
        Err(e) => {
            tracing::error!("ap get_following db error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };

    if !account.enabled {
        return ApiError::not_found("user not found").into_response();
    }

    let following = match db_helpers::list_outbound_follows(&conn, &account.actor_id) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("ap get_following list error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    drop(conn);

    let accepted_count = following.iter().filter(|f| f.state == "accepted").count() as u64;
    let actor_url = format!("https://{}/ap/users/{}", domain, username);

    let collection = ApOrderedCollection {
        context: Some(fauna_bridge_activitypub::types::default_context()),
        r#type: "OrderedCollection".into(),
        id: format!("{}/following", actor_url),
        total_items: accepted_count,
        first: None,
        last: None,
    };

    ap_json_response(&collection, CONTENT_TYPE_AP)
}

/// GET /ap/users/{username}/outbox
const OUTBOX_PAGE_SIZE: i64 = 20;

#[derive(serde::Deserialize)]
struct OutboxQuery {
    page: Option<i64>,
}

async fn get_outbox(
    State(state): State<Arc<AppState>>,
    Path(username): Path<String>,
    Query(query): Query<OutboxQuery>,
) -> Response {
    let domain = state.handle_domain();

    let conn = state.db.conn().await;
    let account = match db_helpers::get_account_by_username(&conn, &username) {
        Ok(Some(a)) => a,
        Ok(None) => return ApiError::not_found("user not found").into_response(),
        Err(e) => {
            tracing::error!("ap get_outbox db error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };

    if !account.enabled {
        drop(conn);
        return ApiError::not_found("user not found").into_response();
    }

    let actor_url = format!("https://{}/ap/users/{}", domain, username);
    let outbox_url = format!("{}/outbox", actor_url);

    // If backfill is disabled, return an empty collection.
    if !account.backfill {
        drop(conn);
        let collection = ApOrderedCollection {
            context: Some(fauna_bridge_activitypub::types::default_context()),
            r#type: "OrderedCollection".into(),
            id: outbox_url,
            total_items: 0,
            first: None,
            last: None,
        };
        return ap_json_response(&collection, CONTENT_TYPE_AP);
    }

    // Decode actor_id hex to bytes for the DB query.
    let author_bytes: [u8; 32] = match fauna_core::hex32::decode(&account.actor_id) {
        Ok(b) => b,
        Err(_) => {
            drop(conn);
            return ApiError::internal("invalid actor_id").into_response();
        }
    };

    // Count total posts for this author. Gated posts are excluded — see
    // `db_helpers::public_outbox_count` for why the filter lives there.
    let total_items: u64 =
        db_helpers::public_outbox_count(&conn, author_bytes.as_slice()).unwrap_or(0);

    // If no page param, return the collection root with a link to the first page.
    if query.page.is_none() {
        drop(conn);
        let collection = ApOrderedCollection {
            context: Some(fauna_bridge_activitypub::types::default_context()),
            r#type: "OrderedCollection".into(),
            id: outbox_url.clone(),
            total_items,
            first: if total_items > 0 {
                Some(format!("{}?page=0", outbox_url))
            } else {
                None
            },
            last: None,
        };
        return ap_json_response(&collection, CONTENT_TYPE_AP);
    }

    // Fetch a page of posts.
    let page_num = query.page.unwrap_or(0).max(0);
    let offset = page_num * OUTBOX_PAGE_SIZE;

    // Page of post IDs (ordered), gated posts excluded. Bodies are read from
    // the `__post` segment store per id below — `content.payload` is empty for
    // posts after the segment-store cutover, so it is no longer selected here.
    let rows: Vec<Vec<u8>> = match db_helpers::public_outbox_page(
        &conn,
        author_bytes.as_slice(),
        OUTBOX_PAGE_SIZE,
        offset,
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("ap get_outbox fetch error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    drop(conn);

    // Translate each post to an AP Create{Note} activity.
    let mut ordered_items = Vec::new();
    for post_id_bytes in &rows {
        let Ok(post_id) = <[u8; 32]>::try_from(post_id_bytes.as_slice()) else {
            continue;
        };
        let body =
            match crate::segments::post::load_post_body(&state.post_segments, &state.db, &post_id)
                .await
            {
                Ok(Some(b)) => b,
                _ => continue,
            };
        // Native signed posts are embed-as-bytes; bridge-ingested posts are
        // bare — `decode_stored_post` handles both.
        let Some(post) = crate::db::posts::decode_stored_post(&body) else {
            continue;
        };

        let post_id_hex = hex::encode(post_id_bytes);
        // The push's own reference resolution, so the pulled copy of a reply
        // is the same object as the pushed one.
        let refs = {
            let conn = state.db.conn().await;
            super::push::resolve_ap_references(&conn, &post).unwrap_or_default()
        };
        let ctx = fauna_bridge_activitypub::translate::OutboundContext {
            domain: domain.clone(),
            username: username.clone(),
            post_id_hex: post_id_hex.clone(),
            in_reply_to: refs.in_reply_to,
            quote_of: refs.quote_of,
        };

        // Same addressing as the Create-push, so a pulled and a pushed copy
        // of the same post are the same AP object (`activitypub.md` § The
        // produce direction).
        let visibility = fauna_bridge_activitypub::translate::NoteVisibility::from_setting(
            &account.default_visibility,
        );
        let note =
            fauna_bridge_activitypub::translate::fauna_post_to_ap_note(&post, &ctx, visibility);
        let activity_id = format!("{}/activities/{}", actor_url, post_id_hex);
        let create = fauna_bridge_activitypub::translate::build_create_activity(
            &actor_url,
            &activity_id,
            &note,
        );

        if let Ok(val) = serde_json::to_value(&create) {
            ordered_items.push(val);
        }
    }

    let has_more = (offset + OUTBOX_PAGE_SIZE) < total_items as i64;
    let page = fauna_bridge_activitypub::types::ApOrderedCollectionPage {
        context: Some(fauna_bridge_activitypub::types::default_context()),
        r#type: "OrderedCollectionPage".into(),
        id: format!("{}?page={}", outbox_url, page_num),
        part_of: outbox_url.clone(),
        ordered_items,
        next: if has_more {
            Some(format!("{}?page={}", outbox_url, page_num + 1))
        } else {
            None
        },
        prev: if page_num > 0 {
            Some(format!("{}?page={}", outbox_url, page_num - 1))
        } else {
            None
        },
    };

    ap_json_response(&page, CONTENT_TYPE_AP)
}

/// GET /ap/users/{username}/notes/{post_id} — serve a single Note object.
///
/// The dereference target for the note ids the produce direction mints
/// (`fauna_post_to_ap_note` builds `{actor}/notes/{post_id_hex}` for the
/// Create-push and the pull outbox alike). Boost fan-out depends on this
/// route: a third-party server receiving an `Announce` of the note fetches
/// it by id before rendering it, and search-by-URL on mainstream servers
/// resolves the same way.
///
/// Servability filter mirrors the outbox (enabled account, `post/%` row, not
/// gated — `db_helpers::public_note_exists`) plus the public-addressing
/// check; everything else is an indistinguishable 404. Deliberately NOT
/// gated on `backfill`: that flag controls bulk history enumeration, while a
/// pushed note's id must stay fetchable or boosts of a no-backfill account's
/// posts break on every third-party server.
async fn get_note(
    State(state): State<Arc<AppState>>,
    Path((username, post_id_hex)): Path<(String, String)>,
) -> Response {
    let domain = state.handle_domain();

    let conn = state.db.conn().await;
    let account = match db_helpers::get_account_by_username(&conn, &username) {
        Ok(Some(a)) => a,
        Ok(None) => return ApiError::not_found("not found").into_response(),
        Err(e) => {
            tracing::error!("ap get_note db error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    if !account.enabled {
        drop(conn);
        return ApiError::not_found("not found").into_response();
    }
    let Ok(post_id) = fauna_core::hex32::decode(&post_id_hex) else {
        drop(conn);
        return ApiError::not_found("not found").into_response();
    };
    let author_bytes: [u8; 32] = match fauna_core::hex32::decode(&account.actor_id) {
        Ok(b) => b,
        Err(_) => {
            drop(conn);
            return ApiError::internal("invalid actor_id").into_response();
        }
    };
    match db_helpers::public_note_exists(&conn, author_bytes.as_slice(), post_id.as_slice()) {
        Ok(true) => {}
        Ok(false) => {
            drop(conn);
            return ApiError::not_found("not found").into_response();
        }
        Err(e) => {
            tracing::error!("ap get_note db error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    }
    drop(conn);

    let body = match crate::segments::post::load_post_body(
        &state.post_segments,
        &state.db,
        &post_id,
    )
    .await
    {
        Ok(Some(b)) => b,
        Ok(None) => return ApiError::not_found("not found").into_response(),
        Err(e) => {
            tracing::error!("ap get_note body load error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    let Some(post) = crate::db::posts::decode_stored_post(&body) else {
        return ApiError::not_found("not found").into_response();
    };
    // Belt-and-braces beside the SQL filter: the gated stance is enforced at
    // every produce sink (push, outbox, and here).
    if post.gated.is_some() {
        return ApiError::not_found("not found").into_response();
    }

    let refs = {
        let conn = state.db.conn().await;
        super::push::resolve_ap_references(&conn, &post).unwrap_or_default()
    };
    let ctx = fauna_bridge_activitypub::translate::OutboundContext {
        domain,
        username,
        post_id_hex,
        in_reply_to: refs.in_reply_to,
        quote_of: refs.quote_of,
    };
    let visibility = fauna_bridge_activitypub::translate::NoteVisibility::from_setting(
        &account.default_visibility,
    );
    let note = fauna_bridge_activitypub::translate::fauna_post_to_ap_note(&post, &ctx, visibility);
    // Only a publicly addressed note is world-fetchable — a followers-only
    // default keeps the id private to delivered copies.
    if !fauna_bridge_activitypub::translate::is_publicly_addressed(&note.to, &note.cc) {
        return ApiError::not_found("not found").into_response();
    }
    ap_json_response(&note, CONTENT_TYPE_AP)
}

/// GET /.well-known/webfinger?resource=acct:user@domain
async fn webfinger(
    State(state): State<Arc<AppState>>,
    Query(params): Query<WebFingerQuery>,
) -> Response {
    let domain = match state.handle_domain_if_set() {
        Some(d) => d,
        None => return ApiError::not_found("ActivityPub not configured").into_response(),
    };

    let (username, req_domain) = match parse_acct_uri(&params.resource) {
        Ok(pair) => pair,
        Err(_) => {
            return ApiError::bad_request("invalid resource URI").into_response();
        }
    };

    // Domain must match our AP domain
    if req_domain != domain {
        return ApiError::not_found("user not found").into_response();
    }

    // `acct:<domain>@<domain>` is the instance actor, Mastodon's own shape for
    // it. No local account can shadow this: a Fauna handle may not contain a
    // dot, so a username equal to the domain is unmintable.
    if username == domain {
        let wf = fauna_bridge_activitypub::webfinger::build_instance_webfinger_response(&domain);
        return ap_json_response(&wf, CONTENT_TYPE_JRD);
    }

    let conn = state.db.conn().await;
    let account = match db_helpers::get_account_by_username(&conn, &username) {
        Ok(Some(a)) => a,
        Ok(None) => return ApiError::not_found("user not found").into_response(),
        Err(e) => {
            tracing::error!("ap webfinger db error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    drop(conn);

    if !account.enabled {
        return ApiError::not_found("user not found").into_response();
    }

    let wf = build_webfinger_response(&username, &domain);
    ap_json_response(&wf, CONTENT_TYPE_JRD)
}

/// GET /.well-known/nodeinfo
///
/// Discovery only — it carries no metadata itself, just the link to the
/// [`nodeinfo_document`] below. The href must name a route this nest actually
/// serves: from 2026-06-05 (which ripped the HTTP discovery twins
/// for the `fauna.nest.info` WS-RPC kind) to 2026-07-16 it pointed at the
/// deleted `/api/v1/node-info`, so every peer following it got the SPA's HTML
/// fallback instead of a NodeInfo document.
async fn nodeinfo(State(state): State<Arc<AppState>>) -> Response {
    let domain = state.handle_domain();

    let wk = NodeInfoWellKnown {
        links: vec![NodeInfoLink {
            rel: NODEINFO_SCHEMA_REL.into(),
            href: format!("https://{}/nodeinfo/{}", domain, NODEINFO_SCHEMA_VERSION),
        }],
    };

    Json(wk).into_response()
}

/// GET /nodeinfo/2.1 — the NodeInfo document itself.
///
/// Anonymous and crawled by fediverse observatories, so it is fingerprint-lean
/// by construction: the version is coarsened to `major.minor` (same posture as
/// `discovery_core::nest_info_core`), `usage.users.total` counts only
/// AP-*enabled* accounts rather than nest population, and `metadata` is empty.
async fn nodeinfo_document(State(state): State<Arc<AppState>>) -> Response {
    // NodeInfo's `openRegistrations` means self-service signup without an
    // invite. That is narrower than the `nest.info` `open` boolean (which means
    // "the register endpoint is enabled at all", so invite-only projects to
    // open + invite_required) — mapping that field straight through would
    // advertise an invite-only nest to crawlers as accepting signups.
    // The claimed identity, not the `--handle-domain` boot seed — the same fix
    // `discovery_core::nest_info_core` carries, and for the same reason: a real
    // box has no seed, so this crawler-facing field read `false` on every nest
    // that had actually opened self-service signup.
    let open_registrations = if state.handle_domain_if_set().is_some() {
        let (mode, _cap) = *state.registration_mode.read().await;
        mode == fauna_protocol::node_policy::RegistrationMode::Open
    } else {
        false
    };

    let conn = state.db.conn().await;
    let users_total = match db_helpers::count_enabled_accounts(&conn) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!("ap nodeinfo db error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    drop(conn);

    let doc = NodeInfoDocument::new(
        "fauna",
        concat!(
            env!("CARGO_PKG_VERSION_MAJOR"),
            ".",
            env!("CARGO_PKG_VERSION_MINOR")
        ),
        open_registrations,
        users_total,
    );

    ap_json_response(&doc, CONTENT_TYPE_NODEINFO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::{InboxMode, Profile, Timestamp};
    use fauna_core::encoding::{decode_profile, sign_and_pack};
    use fauna_core::identity::ActorKeypair;

    /// The federation actor route (`get_actor`) decodes the stored
    /// `schema='profile'` content row via the SHARED
    /// `fauna_core::encoding::decode_profile` (the same signed-only verify helper
    /// `fauna.profile.get` uses) and renders it with `fauna_profile_to_ap_person`.
    /// This guards the reconciliation: the signed `EmbedAsBytes` wire that
    /// `fauna.profile.set` stores must decode HERE too — the pre-reconciliation
    /// bare-only `canonical_decode::<Profile>` would have FAILED on the signed
    /// wire and fallen back to `minimal_person`, dropping display_name/bio.
    #[test]
    fn federation_serve_renders_display_name_and_bio_from_signed_profile() {
        let kp = ActorKeypair::generate();
        let profile = Profile {
            actor_id: kp.actor_id(),
            display_name: Some("Alice".into()),
            bio: Some("building fauna".into()),
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp(0),
        };
        // Exactly what `fauna.profile.set` stores as the content-row payload.
        let stored = sign_and_pack(&kp, &profile).expect("sign+pack");
        // Exactly what `get_actor` does with those stored bytes.
        let (decoded, _origin) = decode_profile(&stored).expect("decode the signed wire");
        let person =
            fauna_profile_to_ap_person(&decoded, "alice", "example.com", String::new(), false);
        assert_eq!(person.name, "Alice");
        assert_eq!(person.summary.as_deref(), Some("<p>building fauna</p>"));
    }

    // ── Note dereference route ──────────────────────────────────

    use crate::db::CacheDb;
    use crate::routes::AppState;

    async fn ap_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        {
            let conn = db.conn().await;
            conn.execute_batch(fauna_bridge_activitypub::db::CREATE_TABLES_SQL)
                .expect("AP schema");
        }
        Arc::new(AppState::for_test(db))
    }

    fn encoded_post(author: [u8; 32]) -> Vec<u8> {
        let post = fauna_core::data::Post {
            author: fauna_core::identity::ActorId(author),
            created_at: fauna_core::data::Timestamp(1_710_892_800_000_000),
            body: fauna_core::data::PostBody::Text {
                content: "hello fediverse".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        fauna_core::encoding::canonical_encode(&post).expect("encode")
    }

    async fn seed_account(state: &Arc<AppState>, username: &str, author: [u8; 32]) {
        let conn = state.db.conn().await;
        db_helpers::create_account(
            &conn,
            &hex::encode(author),
            username,
            &format!("https://localhost/ap/users/{username}"),
            &[],
            "PEM",
        )
        .expect("create account");
    }

    /// Seed a `content` row whose inline payload is a decodable post body —
    /// the inline-payload read path `load_post_body` takes when the segment
    /// store has no record, which is all the route needs.
    async fn seed_note(
        state: &Arc<AppState>,
        post_id: [u8; 32],
        author: [u8; 32],
        tier: Option<&str>,
    ) {
        let body = encoded_post(author);
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO content (id, author, schema, created_at, payload)
             VALUES (?1, ?2, 'post/text', 1000, ?3)",
            rusqlite::params![post_id.as_slice(), author.as_slice(), body],
        )
        .expect("seed content row");
        conn.execute(
            "INSERT INTO content_meta (content_id, gated_tier) VALUES (?1, ?2)",
            rusqlite::params![post_id.as_slice(), tier],
        )
        .expect("seed content_meta row");
    }

    // ── Domainless boot → claim (the provisioned-box flow) ──────

    /// A **domainless-booted** AP nest — exactly what a provisioned VPS is
    /// before its admin claims it: no `[nest] domain`, no `handle_domain`
    /// registration, no primary `mail_domains` row, so all three of
    /// `handle_domain_if_set`'s sources are empty. Returns the state plus the
    /// tempdir backing `acme_dir` (the floor re-synth `apply_primary_identity`
    /// runs needs somewhere real to write; dropping it early would only make
    /// that step warn, but keeping it keeps the test's logs honest).
    async fn domainless_ap_state() -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        {
            let conn = db.conn().await;
            conn.execute_batch(fauna_bridge_activitypub::db::CREATE_TABLES_SQL)
                .expect("AP schema");
        }
        let mut state = AppState::for_test(db);
        state.acme_dir = dir.path().to_path_buf();
        assert!(
            state.handle_domain_if_set().is_none(),
            "precondition: a provisioned box boots with no identity domain at all"
        );
        (Arc::new(state), dir)
    }

    async fn webfinger_response(state: &Arc<AppState>, resource: &str) -> Response {
        webfinger(
            State(state.clone()),
            Query(WebFingerQuery {
                resource: resource.to_string(),
            }),
        )
        .await
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    /// **The bug `test_activitypub_federation_live.py` caught on real infra
    /// (2026-07-23), pinned headlessly.**
    ///
    /// Flow-trace: a provisioned box boots domainless → admin claims it with
    /// `alice@claimed.test` → `claim_admin_core` →
    /// `ensure_mail_domain_registered` → `identity_domain_core::apply_primary_identity`
    /// swaps `AppState.identity_domain` → **every AP surface must serve
    /// `claimed.test` from that instant, with no restart.**
    ///
    /// It did not. `ActivityPubState.domain` was snapshotted once at boot from
    /// `config.nest.domain` — `None` on a provisioned box — so WebFinger 404'd
    /// `"ActivityPub not configured"` and every other AP route served
    /// `https://localhost/...` URLs, until a restart baked the domain into
    /// `nest.toml`. Federation was dead on every provisioned+claimed nest;
    /// example.com only worked because it was configured with a domain *before*
    /// boot. Exactly the split-brain `domains-and-tls-bootstrap.md` § *Claim:
    /// the handle domain IS the deployment's identity domain* forbids.
    ///
    /// Every existing AP e2e nest registers a `[nest] domain` in its fixture,
    /// which is precisely why no headless suite caught this.
    #[tokio::test(flavor = "multi_thread")]
    async fn ap_surfaces_follow_the_claimed_domain_without_a_restart() {
        let (state, _acme_dir) = domainless_ap_state().await;
        let author = [9u8; 32];
        let post_id = [0xB2u8; 32];
        seed_account(&state, "alice", author).await;
        seed_note(&state, post_id, author, None).await;

        // ── Before the claim: no identity, so no federation identity to serve.
        let resp = webfinger_response(&state, "acct:alice@claimed.test").await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "a domainless box has no WebFinger identity yet"
        );
        let resp = get_instance_outbox(State(state.clone())).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "nor an instance actor collection"
        );

        // ── The claim. The one production primitive every claim / add-first-
        //    domain path funnels through (`identity_domain_core` module docs).
        crate::identity_domain_core::apply_primary_identity(&state, "claimed.test");

        // ── After the claim, same process, no restart: WebFinger resolves…
        let resp = webfinger_response(&state, "acct:alice@claimed.test").await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "WebFinger must follow the claim — this is the assertion the live run reddened on"
        );
        let json = body_json(resp).await;
        assert_eq!(json["subject"], "acct:alice@claimed.test");

        // …the instance actor exists (a peer verifying our signature fetches
        // this key; `ensure` used to bail "domain not configured")…
        let resp = get_instance_outbox(State(state.clone())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["id"], "https://claimed.test/ap/instance/outbox");

        // …and every served URL is on the claimed domain, not `localhost`.
        let resp = get_actor(
            State(state.clone()),
            Path("alice".to_string()),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["id"], "https://claimed.test/ap/users/alice");
        assert_eq!(json["inbox"], "https://claimed.test/ap/users/alice/inbox");

        let resp = get_note_response(&state, "alice", &hex::encode(post_id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(
            json["id"],
            format!(
                "https://claimed.test/ap/users/alice/notes/{}",
                hex::encode(post_id)
            ),
            "the note id a remote dereferences must be on the claimed domain"
        );

        let resp = nodeinfo(State(state.clone())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(
            json["links"][0]["href"]
                .as_str()
                .expect("nodeinfo href")
                .starts_with("https://claimed.test/nodeinfo/"),
            "nodeinfo advertised {:?}, not the claimed domain",
            json["links"][0]["href"]
        );
    }

    /// `manuallyApprovesFollowers` is the negation of the account's *accept
    /// follows by itself* setting on every shape of the served document — the
    /// no-profile one and the profile-backed one — and the profile's
    /// conversation inbox mode has no say in it (`activitypub.md` § Follow
    /// requests). Peers render the lock and their "request sent" state from
    /// this field, so an account that holds follows back must say so, and one
    /// that accepts at once must not claim a lock because its inbox is closed.
    #[tokio::test(flavor = "multi_thread")]
    async fn actor_document_manual_approval_follows_the_setting_and_ignores_the_inbox_mode() {
        async fn served(state: &Arc<AppState>) -> serde_json::Value {
            let resp = get_actor(
                State(state.clone()),
                Path("alice".to_string()),
                HeaderMap::new(),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
            body_json(resp).await
        }
        async fn set_auto_accept(state: &Arc<AppState>, actor_hex: &str, on: bool) {
            let conn = state.db.conn().await;
            db_helpers::update_settings(
                &conn,
                actor_hex,
                &db_helpers::ApSettings {
                    auto_accept_follows: Some(on),
                    ..Default::default()
                },
            )
            .expect("update settings");
        }

        let state = ap_state().await;
        let kp = ActorKeypair::generate();
        let author = kp.actor_id().0;
        let actor_hex = hex::encode(author);
        seed_account(&state, "alice", author).await;

        // No profile row: the `minimal_person` document.
        assert_eq!(
            served(&state).await["manuallyApprovesFollowers"],
            false,
            "the setting defaults to on, so a fresh account advertises open follows"
        );
        set_auto_accept(&state, &actor_hex, false).await;
        let doc = served(&state).await;
        assert_eq!(
            doc["name"], "alice",
            "precondition: the no-profile document"
        );
        assert_eq!(
            doc["manuallyApprovesFollowers"], true,
            "a no-profile account holding follows back must say so"
        );

        // A profile whose inbox is CLOSED, on an account that accepts follows
        // by itself: the old inbox-mode derivation said `true` here.
        let profile = Profile {
            actor_id: kp.actor_id(),
            display_name: Some("Alice".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Closed,
            recovery_head: None,
            updated_at: Timestamp(0),
        };
        let stored = sign_and_pack(&kp, &profile).expect("sign+pack");
        {
            let conn = state.db.conn().await;
            conn.execute(
                "INSERT INTO content (id, author, schema, created_at, payload)
                 VALUES (?1, ?2, 'profile', 1000, ?3)",
                rusqlite::params![[0x70u8; 32].as_slice(), author.as_slice(), stored],
            )
            .expect("seed profile row");
        }
        set_auto_accept(&state, &actor_hex, true).await;
        let doc = served(&state).await;
        assert_eq!(
            doc["name"], "Alice",
            "precondition: the profile-backed document"
        );
        assert_eq!(
            doc["manuallyApprovesFollowers"], false,
            "a closed conversation inbox must not lock the follow path"
        );

        // …and an OPEN inbox on an account that holds follows back.
        let open = Profile {
            inbox_mode: InboxMode::Open,
            updated_at: Timestamp(1),
            ..profile
        };
        let stored = sign_and_pack(&kp, &open).expect("sign+pack");
        {
            let conn = state.db.conn().await;
            conn.execute(
                "INSERT INTO content (id, author, schema, created_at, payload)
                 VALUES (?1, ?2, 'profile', 2000, ?3)",
                rusqlite::params![[0x71u8; 32].as_slice(), author.as_slice(), stored],
            )
            .expect("seed profile row");
        }
        set_auto_accept(&state, &actor_hex, false).await;
        let doc = served(&state).await;
        assert_eq!(doc["name"], "Alice");
        assert_eq!(
            doc["manuallyApprovesFollowers"], true,
            "an open conversation inbox must not unlock the follow path"
        );
    }

    async fn get_note_response(
        state: &Arc<AppState>,
        username: &str,
        post_id_hex: &str,
    ) -> Response {
        get_note(
            State(state.clone()),
            Path((username.to_string(), post_id_hex.to_string())),
        )
        .await
    }

    /// The produce direction mints `{actor}/notes/{hex}` as every note's id;
    /// this pins that the id dereferences to the same AP object (Announce
    /// fan-out on third-party servers fetches it by id before rendering).
    #[tokio::test(flavor = "multi_thread")]
    async fn note_url_serves_public_note() {
        let state = ap_state().await;
        let author = [7u8; 32];
        let post_id = [0xA1u8; 32];
        seed_account(&state, "alice", author).await;
        seed_note(&state, post_id, author, None).await;

        let resp = get_note_response(&state, "alice", &hex::encode(post_id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            CONTENT_TYPE_AP
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(json["type"], "Note");
        assert_eq!(
            json["id"],
            format!(
                "https://localhost/ap/users/alice/notes/{}",
                hex::encode(post_id)
            )
        );
        assert!(
            json.get("@context").is_some(),
            "top-level object needs @context"
        );
        assert!(
            json["content"]
                .as_str()
                .unwrap()
                .contains("hello fediverse"),
            "content: {}",
            json["content"]
        );
        assert!(
            json["to"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some("https://www.w3.org/ns/activitystreams#Public")),
            "a served note must be publicly addressed"
        );
    }

    /// The world-readable gates, each an indistinguishable 404: a gated
    /// (paywalled) post, a disabled account, a followers-only default
    /// visibility, an author mismatch, an unknown user, and malformed hex.
    #[tokio::test(flavor = "multi_thread")]
    async fn note_url_404s_every_non_public_case() {
        let state = ap_state().await;
        let author = [7u8; 32];
        let other_author = [8u8; 32];
        let public = [0xA1u8; 32];
        let gated = [0xB1u8; 32];
        seed_account(&state, "alice", author).await;
        seed_account(&state, "bob", other_author).await;
        seed_note(&state, public, author, None).await;
        seed_note(&state, gated, author, Some("premium")).await;

        let hex_public = hex::encode(public);

        // Gated post.
        let resp = get_note_response(&state, "alice", &hex::encode(gated)).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "gated");

        // Author mismatch: bob's URL, alice's post.
        let resp = get_note_response(&state, "bob", &hex_public).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "author mismatch");

        // Unknown user + malformed hex.
        let resp = get_note_response(&state, "nobody", &hex_public).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "unknown user");
        let resp = get_note_response(&state, "alice", "zz-not-hex").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "malformed hex");

        // Followers-only default visibility: the note is not publicly
        // addressed, so its id is private to delivered copies.
        {
            let conn = state.db.conn().await;
            db_helpers::update_settings(
                &conn,
                &hex::encode(author),
                &db_helpers::ApSettings {
                    default_visibility: Some("followers_only".into()),
                    ..Default::default()
                },
            )
            .expect("update settings");
        }
        let resp = get_note_response(&state, "alice", &hex_public).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "followers-only visibility"
        );

        // Disabled account: everything 404s, the previously-public note included.
        {
            let conn = state.db.conn().await;
            db_helpers::update_settings(
                &conn,
                &hex::encode(author),
                &db_helpers::ApSettings {
                    enabled: Some(false),
                    default_visibility: Some("public".into()),
                    ..Default::default()
                },
            )
            .expect("update settings");
        }
        let resp = get_note_response(&state, "alice", &hex_public).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "disabled account");
    }
}
