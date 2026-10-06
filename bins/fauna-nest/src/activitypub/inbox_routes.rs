//! ActivityPub inbox routes (receiving Follow, Create, Delete, etc.).
//!
//! Two endpoints are provided:
//!   POST /ap/users/{username}/inbox  — per-user inbox
//!   POST /ap/inbox                   — shared inbox
//!
//! Both follow the same pipeline:
//!   1. Check Content-Type
//!   2. Enforce body size limit
//!   3. Parse Signature header
//!   4. Verify Digest header
//!   5. Parse activity JSON
//!   6. Fetch / cache remote actor and extract public key
//!   7. Verify HTTP Signature
//!   8. Dispatch by activity type

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use crate::api_error::ApiError;
use axum::Router;
use axum::routing::post;
use serde_json::Value;

use fauna_bridge_activitypub::http_signatures::{
    compute_digest, parse_signature_header, verify_signature,
};
use fauna_bridge_activitypub::identity::synthetic_actor_id;
use fauna_bridge_activitypub::translate::{
    ap_note_to_fauna_post, build_accept_activity, build_reject_activity, follow_object_for_answer,
    is_publicly_addressed,
};
use fauna_bridge_activitypub::types::ApNote;

use crate::activitypub::db_helpers::{self, RemoteActor};
use crate::routes::AppState;

const MAX_BODY_BYTES: usize = 1_048_576; // 1 MB

/// How far a delivery's signed `Date` may sit from this nest's clock, either
/// direction, before the inbox refuses it — the replay window's width.
///
/// A **hard-coded constant, and it must stay one.** Nobody would ever want to
/// choose this: it is not a preference, it is the width of a security window,
/// and `activitypub.md` § Security posture states the rule for this whole
/// surface — "no configuration surface beyond the app: hard-coded constants
/// (body cap, rate limit, retry policy, dead-inbox threshold)". This joins that
/// list; a file or env knob for it would be configuration-file theatre.
///
/// One hour, matching what mainstream fediverse servers enforce (Mastodon
/// rejects deliveries outside a ~1 h window), so the floor costs no legitimate
/// peer anything. It is generous against clock skew on purpose: the value being
/// bounded at all is what turns an observed delivery from a permanent
/// credential into a briefly-usable one, and shaving it toward the true
/// network-delay bound would buy little while starting to reject honest peers
/// with drifting clocks.
///
/// A *tight* window is not the mechanism that makes replay uninteresting
/// within it — per-activity suppression would be — so do not read this number
/// as the whole defence. It is the bound; § Security posture records what
/// remains inside it.
const AP_INBOX_DATE_SKEW_SECS: i64 = 3600;

/// Ingest-time timestamp, used when an inbound Note omits `published` (AS2 lets
/// it): the post is dated "first seen here" rather than dropped for a missing
/// field. `now_epoch_secs` is whole seconds; `Timestamp` is microseconds.
fn ingest_now() -> fauna_core::data::Timestamp {
    fauna_core::data::Timestamp(crate::db::now_epoch_secs().max(0) as u64 * 1_000_000)
}

// ── Route registration ───────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ap/users/{username}/inbox", post(user_inbox))
        .route("/ap/inbox", post(shared_inbox))
}

// ── Endpoint handlers ────────────────────────────────────────────────────────

/// Per-user inbox: `POST /ap/users/{username}/inbox`
async fn user_inbox(
    State(state): State<Arc<AppState>>,
    Path(username): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Verify the local user exists.
    let conn = state.db.conn().await;
    let account = match db_helpers::get_account_by_username(&conn, &username) {
        Ok(Some(a)) => a,
        Ok(None) => {
            return ApiError::not_found("user not found").into_response();
        }
        Err(e) => {
            tracing::error!(username, error = %e, "ap user_inbox: db lookup failed");
            return ApiError::internal("storage error").into_response();
        }
    };
    drop(conn);

    process_inbox(&state, &headers, body, Some(account.username)).await
}

/// Shared inbox: `POST /ap/inbox`
async fn shared_inbox(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    process_inbox(&state, &headers, body, None).await
}

// ── Core processing pipeline ─────────────────────────────────────────────────

/// Split a `Host`-header value or an identity-domain string into a comparable
/// authority: `(normalized host, port)` — host via
/// [`fauna_core::web::normalize_dns_name`] (ASCII-lowercase + trailing-FQDN-dot
/// trim), an absent port defaulting to **443** (AP rides https, so
/// `example.com` and `example.com:443` are the same authority; an `ip:port`
/// dev/e2e domain carries its explicit port on both sides of the comparison).
///
/// Deliberately parses ONLY `host[:port]` / `[v6][:port]` — no userinfo, no
/// path. Anything else (an `@`, a second unbracketed colon, a non-numeric
/// port) yields `None` or a host that equals no real authority, so the
/// comparison in `process_inbox` fails closed on it.
///
/// **A look-alike of [`fauna_core::web::split_host_port`], deliberately not
/// composed onto it.** That primitive hands back malformed input *whole* on
/// a bad/second colon or an unparseable port (`"host:junk"` → `("host:junk",
/// None)`), by design, for callers that need a fail-open host string. This
/// function's own contract — and
/// `host_authority_normalizes_equivalent_spellings_and_rejects_non_authorities`,
/// which pins it — needs the opposite: reject to `None` outright so the
/// pipeline's `!=` comparison never has to reason about a half-parsed
/// string. Reconciling the two would need re-adding a malformed-shape guard
/// on top of `split_host_port`'s output — more code than this hand-rolled
/// version, not less — so scouted 2026-08-19 and left as-is.
fn host_authority(value: &str) -> Option<(String, u16)> {
    let value = value.trim();
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        // IPv6 literal: `[::1]` or `[::1]:8443`.
        let end = rest.find(']')?;
        let after = &rest[end + 1..];
        let port = if after.is_empty() {
            None
        } else {
            Some(after.strip_prefix(':')?)
        };
        (format!("[{}]", &rest[..end]), port)
    } else if let Some((h, p)) = value.rsplit_once(':') {
        if h.contains(':') {
            // A second colon outside brackets is not a Host-grammar authority.
            return None;
        }
        (h.to_string(), Some(p))
    } else {
        (value.to_string(), None)
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(p) => p.parse::<u16>().ok()?,
        None => 443,
    };
    Some((fauna_core::web::normalize_dns_name(&host), port))
}

/// Decide whether `url` names a local actor on THIS nest's authority,
/// returning the username if so: `https://<authority>/ap/users/<username>[/…]`
/// with `<authority>` equal to `our_domain` under the ratified
/// accepted-authority set ([`host_authority`] — `activitypub.md` § Security
/// posture: host case-insensitive, trailing FQDN dot trimmed, absent port
/// defaulting to 443, deliberately not port-blind).
///
/// This is the ONE comparison every inbound actor-URL door runs —
/// `resolve_username_from_activity`, `handle_follow`, `handle_undo{Follow}`,
/// `handle_accept` — so none of them can drift from the `Host` door's set
/// again (they raw-prefix-matched exactly one
/// spelling). The scheme is literal `https` — AP rides https and every actor
/// URL this nest advertises is https, so anything else is not ours.
pub(super) fn local_actor_username(our_domain: &str, url: &str) -> Option<String> {
    let ours = host_authority(our_domain)?;
    let rest = url.strip_prefix("https://")?;
    let slash = rest.find('/')?;
    if host_authority(&rest[..slash])? != ours {
        return None;
    }
    let username = rest[slash..]
        .strip_prefix("/ap/users/")?
        .split('/')
        .next()?;
    if username.is_empty() {
        return None;
    }
    Some(username.to_string())
}

async fn process_inbox(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    body: Bytes,
    // For the per-user inbox this is the resolved username;
    // for the shared inbox it is None and the username is inferred from the activity.
    target_username: Option<String>,
) -> axum::response::Response {
    // ── 0. This nest must have an authority to receive AS ─────────────────
    //
    // A domainless box (a provisioned VPS before its admin claims it) refuses
    // inbound deliveries outright, with the same not-configured posture the
    // discovery routes already hold (`actor_routes::webfinger`): there is no
    // authority for the `host` check below to bind, WebFinger refuses
    // discovery, and the actor URIs such a box would advertise are
    // placeholders — no peer can legitimately hold a URL of ours to deliver
    // to. Owner: `activitypub.md` § Security posture.
    let Some(our_domain) = state.handle_domain_if_set() else {
        return ApiError::not_found("ActivityPub not configured").into_response();
    };

    // ── 1. Check Content-Type ──────────────────────────────────────────────
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let acceptable = content_type.contains("application/activity+json")
        || content_type.contains("application/ld+json")
        || content_type.contains("application/json");

    if !acceptable {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported content type",
        )
            .into_response();
    }

    // ── 2. Body size limit ────────────────────────────────────────────────
    if body.len() > MAX_BODY_BYTES {
        return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    }

    // ── 3. Parse Signature header ─────────────────────────────────────────
    let sig_header = match headers.get("signature").and_then(|v| v.to_str().ok()) {
        Some(h) => h,
        None => {
            return ApiError::unauthorized("missing Signature header").into_response();
        }
    };

    let parsed_sig = match parse_signature_header(sig_header) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("ap inbox: bad Signature header: {e}");
            return ApiError::unauthorized("malformed Signature header").into_response();
        }
    };

    // ── 4. Verify Digest header ───────────────────────────────────────────
    //
    // Required, not merely checked-if-present: the AP spec calls Digest
    // optional, but "optional" only describes whether a compliant *sender*
    // must send one — it says nothing about what a signature that omits it
    // actually proves. `verify_signature` (step 7) checks only the headers
    // named in the signer's own `headers=` list; a signature that never
    // named `digest` never bound the body to anything, so accepting a
    // digest-less request would let an attacker who can swap the Signature
    // header's body pairing (a relay, a compromised intermediary) replace
    // the body under an otherwise-valid signature. Every delivery this nest
    // sends already includes Digest (`sync_worker::deliver`), so this holds
    // peers to the same bar we hold ourselves to.
    let digest_hdr = match headers.get("digest").and_then(|v| v.to_str().ok()) {
        Some(d) => d,
        None => {
            return ApiError::bad_request("missing Digest header").into_response();
        }
    };
    let expected = compute_digest(&body);
    if digest_hdr != expected {
        tracing::debug!("ap inbox: digest mismatch (got {digest_hdr}, expected {expected})");
        return ApiError::bad_request("digest mismatch").into_response();
    }

    // ── 5. Parse activity JSON ────────────────────────────────────────────
    let activity: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("ap inbox: JSON parse failed: {e}");
            return ApiError::bad_request("invalid JSON").into_response();
        }
    };

    let activity_type = match activity.get("type").and_then(|v| v.as_str()) {
        Some(t) => t.to_string(),
        None => {
            return ApiError::bad_request("missing activity type").into_response();
        }
    };

    let actor_uri = match activity.get("actor").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => {
            return ApiError::bad_request("missing actor").into_response();
        }
    };

    // ── 5a. Rate limit per remote domain ────────────────────────────────
    if let Ok(url) = url::Url::parse(&actor_uri)
        && let Some(domain) = url.host_str()
        && let Some(ref limiter) = state.activitypub.inbox_limiter
        && limiter.check_key(&domain.to_string()).is_err()
    {
        tracing::debug!(domain, "ap inbox: rate limited");
        return ApiError::too_many_requests("rate limited").into_response();
    }

    // ── 6. Fetch / cache remote actor ─────────────────────────────────────
    let remote_actor = {
        let conn = state.db.conn().await;
        let cached = db_helpers::get_remote_actor(&conn, &actor_uri)
            .ok()
            .flatten();
        drop(conn);

        // Re-fetch if not cached, if the cache is older than 24 h, or if the
        // cached row carries no usable key. That last arm heals a keyless
        // row (an `Update{Person}` can still store one with an empty
        // `public_key_pem`) on the next activity instead of leaving every
        // activity from that actor failing verification for a full day.
        let needs_fetch = match &cached {
            None => true,
            Some(a) => {
                let age_secs = crate::db::now_epoch_secs() - a.last_fetched;
                age_secs > 86_400 || a.public_key_pem.is_empty()
            }
        };

        if needs_fetch {
            match fetch_remote_actor(state.as_ref(), &actor_uri).await {
                Ok(actor) => {
                    let conn = state.db.conn().await;
                    if let Err(e) = db_helpers::upsert_remote_actor(&conn, &actor) {
                        tracing::warn!(error = %e, "ap inbox: upsert_remote_actor failed");
                    }
                    actor
                }
                Err(e) => {
                    tracing::warn!(actor = %actor_uri, error = %e, "ap inbox: fetch remote actor failed");
                    // Fall back to the cache only if it can actually verify a
                    // signature. A keyless row (the `Update{Person}` case
                    // above) would otherwise send us into verification with an
                    // empty PEM, whose parse error reads like a crypto fault
                    // rather than the failed fetch that really caused it.
                    match cached.filter(|a| !a.public_key_pem.is_empty()) {
                        Some(a) => a,
                        None => {
                            return ApiError::unauthorized("cannot verify actor").into_response();
                        }
                    }
                }
            }
        } else {
            cached.unwrap()
        }
    };

    // ── 7. Verify HTTP Signature ──────────────────────────────────────────
    let method = "post";
    // Build the request path.  For shared inbox it is /ap/inbox; for per-user
    // inbox the path must be reconstructed from the username we already have.
    let path = match &target_username {
        Some(u) => format!("/ap/users/{}/inbox", u),
        None => "/ap/inbox".to_string(),
    };

    // `verify_signature` only checks the headers the SIGNER chose to cover
    // (step 4's mandatory Digest-header match is a separate check — it
    // proves this request's body matches this request's Digest header, not
    // that the *signature* covers Digest at all). Without this, a signer
    // could sign `(request-target) host date` only, and a party able to
    // rewrite the body + Digest header together (a relay, a compromised
    // intermediary) would produce a self-consistent-but-forged request that
    // still verifies. Every delivery this nest sends signs Digest
    // (`sync_worker::deliver`), so this asks nothing of a peer we don't
    // already do ourselves.
    if !parsed_sig.headers.iter().any(|h| h == "digest") {
        tracing::warn!(actor = %actor_uri, "ap inbox: signature does not cover Digest");
        return ApiError::unauthorized("signature must cover Digest").into_response();
    }

    // The other two legs of the same floor. The floor above binds the
    // body; these bind the request to an AUTHORITY and to a MOMENT, and neither
    // works without the other, which is why they land together.
    //
    // `host`: `(request-target)` covers `method path` and nothing else, so the
    // authority lives only in the `Host` header. The shared-inbox path is the
    // constant `/ap/inbox` on EVERY fauna nest, so a delivery signed without
    // `host` produces a signing string that is byte-identical at every nest on
    // the internet — one capture replays to all of them. Coverage alone is only
    // half the leg: the VALUE check further down is what makes the
    // covered `host` actually name THIS nest.
    //
    // `date` + the window: requiring `date` coverage alone accomplishes nothing
    // (the signature stays valid forever), and enforcing a window on an
    // *unsigned* Date accomplishes nothing (the replayer rewrites it). Only the
    // pair closes it. Without both, an observed delivery is a permanent
    // credential: `handle_follow`/`handle_undo{Follow}` act on the follow edge
    // with no replay suppression, and `create_follow`'s `INSERT OR IGNORE` is
    // NOT a defence — it suppresses the insert exactly when the edge already
    // exists (when a replay would be harmless) and succeeds once the user has
    // removed it (when it would not).
    //
    // Cost to legitimate peers: nothing, by this posture's own standard. This
    // nest's signer always covers all four (`sync_worker::deliver`), and
    // Mastodon/GoToSecial sign `(request-target) host date digest`. Mastodon
    // itself rejects outside a comparable window.
    for required in ["host", "date"] {
        if !parsed_sig.headers.iter().any(|h| h == required) {
            tracing::warn!(
                actor = %actor_uri,
                header = required,
                "ap inbox: signature does not cover a required header"
            );
            return ApiError::unauthorized("signature must cover Host and Date").into_response();
        }
    }

    // The VALUE half of the `host` leg. Coverage (above) stops the
    // fleet-wide byte-identical signing string of a host-less signature, but
    // `verify_signature` (step 7) reconstructs the signing string by reading
    // each covered header back out of the request AS SENT — so a delivery
    // signed for some OTHER nest's authority, replayed here with its captured
    // `Host` intact, still reproduces byte-for-byte and verifies. Only
    // comparing the sent value to this nest's own authority binds the
    // delivery to us. The accepted set is exactly our identity authority
    // (`handle_domain_if_set`, checked non-empty at step 0), compared
    // component-wise by `host_authority`: host case-insensitively, ports
    // after defaulting an absent one to 443. Pinned by PROBE-374-A.
    let ours = host_authority(&our_domain);
    let theirs = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(host_authority);
    if ours.is_none() {
        // An identity domain that is not a parseable authority cannot
        // federate at all — refuse rather than accept everything.
        tracing::warn!(
            domain = %our_domain,
            "ap inbox: this nest's identity domain is not a parseable authority"
        );
        return ApiError::unauthorized("nest authority unavailable").into_response();
    }
    // `ours` is Some past the check above, so an absent/unparseable request
    // Host compares unequal here and is refused with the rest.
    if theirs != ours {
        tracing::warn!(
            actor = %actor_uri,
            host = ?headers.get(axum::http::header::HOST),
            "ap inbox: signed Host does not name this nest's authority"
        );
        return ApiError::unauthorized("signed Host does not name this nest").into_response();
    }

    // The window is symmetric: a future-dated delivery is as unusable as a
    // stale one, and refusing it also refuses a peer whose clock would let it
    // mint a credential valid for a future moment.
    let signed_date = headers
        .get(axum::http::header::DATE)
        .and_then(|v| v.to_str().ok())
        .and_then(super::sync_worker::parse_http_date);
    let Some(signed_date) = signed_date else {
        // Unparseable is refused, never treated as fresh — the header is
        // present (it is in the signed set, checked above) and covered, so a
        // value we cannot read is a peer we cannot date, not a peer to trust.
        tracing::warn!(actor = %actor_uri, "ap inbox: signed Date is not an RFC 7231 HTTP-date");
        return ApiError::unauthorized("signed Date is unreadable").into_response();
    };
    let now = fauna_core::data::Timestamp::now_secs();
    if (now - signed_date).abs() > AP_INBOX_DATE_SKEW_SECS {
        tracing::warn!(
            actor = %actor_uri,
            skew_secs = now - signed_date,
            "ap inbox: signed Date is outside the freshness window"
        );
        return ApiError::unauthorized("signed Date is outside the freshness window")
            .into_response();
    }

    let headers_ref = headers;
    let verify_result = verify_signature(
        &parsed_sig,
        &remote_actor.public_key_pem,
        method,
        &path,
        |name| {
            headers_ref
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        },
    );

    if let Err(e) = verify_result {
        tracing::warn!(actor = %actor_uri, error = %e, "ap inbox: signature verification failed");
        return ApiError::unauthorized("signature verification failed").into_response();
    }

    // ── 8. Dispatch ───────────────────────────────────────────────────────

    // Resolve target username from the shared inbox path if not already known.
    let resolved_username = match target_username {
        Some(u) => Some(u),
        None => resolve_username_from_activity(state, &activity).await,
    };

    let result = match activity_type.as_str() {
        "Create" => handle_create(state, &activity, resolved_username.as_deref()).await,
        "Follow" => handle_follow(state, &activity, resolved_username.as_deref()).await,
        "Undo" => handle_undo(state, &activity).await,
        "Like" => handle_like(state, &activity, resolved_username.as_deref()).await,
        "Announce" => handle_announce(state, &activity, resolved_username.as_deref()).await,
        "Delete" => handle_delete(state, &activity).await,
        "Update" => handle_update(state, &activity, resolved_username.as_deref()).await,
        "Accept" => handle_accept(state, &activity).await,
        _ => {
            tracing::debug!(activity_type, "ap inbox: ignoring unknown activity type");
            Ok(StatusCode::ACCEPTED)
        }
    };

    match result {
        Ok(code) => code.into_response(),
        Err(e) => {
            tracing::error!(activity_type, error = %e, "ap inbox: handler error");
            ApiError::internal("handler error").into_response()
        }
    }
}

// ── Remote actor fetching ────────────────────────────────────────────────────

/// Fetch and parse a remote actor document.
///
/// `actor_uri` is attacker-supplied — it arrives in an inbox POST we have not
/// yet verified (verification needs the very key this fetch retrieves) — so the
/// dial goes through the shared SSRF guard via `outbound::ap_outbound_client`.
/// See that module for why the delivery POST is guarded too.
///
/// **Signed with the nest's instance actor** (`super::instance_actor`): an
/// `AUTHORIZED_FETCH` peer refuses unsigned `GET`s of its objects, and this
/// fetch has no per-user actor whose key it could use — it runs on arrival of
/// an activity, before we know which local user it concerns. Signing failure is
/// deliberately *not* fatal: a nest whose instance key is unavailable still
/// federates with the permissive majority, so we log and dial unsigned rather
/// than turning a key problem into a total federation outage.
pub(crate) async fn fetch_remote_actor(
    state: &AppState,
    actor_uri: &str,
) -> anyhow::Result<RemoteActor> {
    let (client, url) = super::outbound::ap_outbound_client(actor_uri).await?;

    let mut req = client
        .get(url.clone())
        .header("Accept", "application/activity+json");

    match super::instance_actor::ensure(state).await {
        Ok(instance) => {
            let signed = instance.sign_get(&url, crate::db::now_epoch_secs())?;
            req = req
                .header("Date", &signed.date)
                .header("Host", &signed.host)
                .header("Signature", &signed.signature);
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "ap: no instance actor — dialing the remote actor UNSIGNED, which \
                 an AUTHORIZED_FETCH peer will refuse",
            );
        }
    }

    let resp = req.send().await?;
    read_actor_response(actor_uri, resp).await
}

/// Turn the actor fetch's reply into a `RemoteActor`, or refuse it.
///
/// Split out of the dial so the body handling is pinned against a served
/// response without the SSRF guard's loopback refusal in the way.
async fn read_actor_response(
    actor_uri: &str,
    resp: reqwest::Response,
) -> anyhow::Result<RemoteActor> {
    // Status FIRST. Without this a 401/404/502 body was parsed as if it were an
    // actor document and every field below silently defaulted to "" — so a
    // secure-mode peer's `{"error":"Request not signed"}` became a cached actor
    // with an EMPTY public key, and the real failure surfaced 24h-cacheably as
    // an inscrutable "PEM preamble contains invalid data (NUL byte)" at
    // signature verification. Observed against real Mastodon, 2026-07-22.
    let status = resp.status();
    if !status.is_success() {
        let body = super::outbound::error_body_snippet(resp).await;
        let body = body.chars().take(200).collect::<String>();
        anyhow::bail!("remote actor fetch returned HTTP {status}: {body}");
    }

    // Capped: this reply comes from a URI an unverified activity named, and is
    // read before any signature check.
    let body = crate::ssrf::read_capped(resp, super::outbound::AP_ACTOR_DOCUMENT_MAX_BYTES).await?;
    let person: Value = serde_json::from_slice(&body)?;
    parse_remote_actor(actor_uri, &person)
}

/// Turn a fetched actor document into a `RemoteActor`, or refuse it.
///
/// Split out of the fetch so the refusal rules are pinned without a peer.
///
/// **An actor document without an inbox or a public key is not an actor.** It
/// used to become one anyway: every field was read with `unwrap_or("")`, so a
/// body that was not an actor at all yielded a `RemoteActor` with empty
/// strings, which the caller then cached for 24 hours. Since the cached key is
/// what verifies every subsequent activity from that actor, one bad response
/// broke the actor for a day — and reported it as a PEM parse error, pointing
/// the reader at crypto instead of at the fetch.
fn parse_remote_actor(actor_uri: &str, person: &Value) -> anyhow::Result<RemoteActor> {
    let inbox = person["inbox"].as_str().unwrap_or_default();
    let public_key_pem = person["publicKey"]["publicKeyPem"]
        .as_str()
        .unwrap_or_default();
    if inbox.is_empty() || public_key_pem.is_empty() {
        anyhow::bail!(
            "remote actor document is missing {}",
            if inbox.is_empty() {
                "an inbox"
            } else {
                "a public key"
            }
        );
    }

    Ok(RemoteActor {
        uri: actor_uri.to_string(),
        inbox: inbox.to_string(),
        shared_inbox: person
            .get("endpoints")
            .and_then(|e| e.get("sharedInbox"))
            .and_then(|s| s.as_str())
            .map(String::from),
        public_key_pem: public_key_pem.to_string(),
        preferred_username: person
            .get("preferredUsername")
            .and_then(|v| v.as_str())
            .map(String::from),
        display_name: person
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from),
        // Optional rich fields — not always present.
        avatar_url: person
            .get("icon")
            .and_then(|v| v.get("url"))
            .and_then(|v| v.as_str())
            .map(String::from),
        banner_url: person
            .get("image")
            .and_then(|v| v.get("url"))
            .and_then(|v| v.as_str())
            .map(String::from),
        summary: person
            .get("summary")
            .and_then(|v| v.as_str())
            .map(String::from),
        last_fetched: crate::db::now_epoch_secs(),
    })
}

// ── Shared-inbox username resolution ────────────────────────────────────────

/// For activities delivered to the shared inbox, inspect `to` and `cc` to find
/// which local user(s) are addressed.  Returns the first match found, or None.
async fn resolve_username_from_activity(state: &Arc<AppState>, activity: &Value) -> Option<String> {
    let domain = state.handle_domain();

    let collect_uris = |field: &str| -> Vec<String> {
        match activity.get(field) {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            Some(Value::String(s)) => vec![s.clone()],
            _ => vec![],
        }
    };

    let mut uris: Vec<String> = collect_uris("to");
    uris.extend(collect_uris("cc"));

    for uri in uris {
        // The URI may be "…/ap/users/username" or "…/username/inbox" — the
        // predicate takes the first path segment after the actor prefix.
        if let Some(username) = local_actor_username(&domain, &uri) {
            let conn = state.db.conn().await;
            let found = db_helpers::get_account_by_username(&conn, &username)
                .ok()
                .flatten();
            drop(conn);
            if found.is_some() {
                return Some(username);
            }
        }
    }
    None
}

// ── Activity handlers ────────────────────────────────────────────────────────

/// Extract a `to`/`cc`-style field as strings — AS2 allows a single string or
/// an array; absent/other shapes yield empty.
pub(super) fn string_array(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// The two gates a Note must clear before it may enter the public `post/*`
/// projection, shared by `Create` and `Update` so the two can never drift (an
/// `Update{Note}` that skipped them was a backdoor around both — audit
/// 2026-07-22). Returns `Ok(true)` to ingest, `Ok(false)` to ACCEPT-and-drop
/// (logged); errors only on a DB failure.
///
/// - **Audience gate**: only a publicly addressed Note may enter the public
///   projection — `store_post` rows are full-body, ungated, and served to every
///   authenticated user, so a remote DM / followers-only Note is dropped.
/// - **Relationship gate**: stored only when a local account opted
///   in — the signature-verified top-level `actor` is followed by an enabled
///   local account, or the note was addressed to one (the resolved target).
///   Keyed on the verified `actor`, never the Note's self-claimed `attributedTo`.
async fn note_passes_ingest_gates(
    state: &Arc<AppState>,
    activity: &Value,
    note: &ApNote,
    target_username: Option<&str>,
    verb: &str,
) -> anyhow::Result<bool> {
    let activity_to = string_array(activity.get("to"));
    let activity_cc = string_array(activity.get("cc"));
    if !is_publicly_addressed(&note.to, &note.cc)
        && !is_publicly_addressed(&activity_to, &activity_cc)
    {
        tracing::info!(
            ap_url = %note.id,
            "ap {verb}: dropping non-public Note (DM / followers-only); no public projection"
        );
        return Ok(false);
    }

    let signed_actor = activity
        .get("actor")
        .and_then(|v| v.as_str())
        .unwrap_or(note.attributed_to.as_str());
    let opted_in = {
        let conn = state.db.conn().await;
        let followed = db_helpers::any_enabled_account_follows(&conn, signed_actor)?;
        let addressed = match target_username {
            Some(u) => db_helpers::get_account_by_username(&conn, u)?
                .map(|a| a.enabled)
                .unwrap_or(false),
            None => false,
        };
        followed || addressed
    };
    if !opted_in {
        tracing::info!(
            ap_url = %note.id,
            "ap {verb}: dropping unsolicited Note (actor not followed by, and note not addressed to, any enabled local account)"
        );
        return Ok(false);
    }
    Ok(true)
}

/// Handle a `Create` activity carrying a Note.
pub(super) async fn handle_create(
    state: &Arc<AppState>,
    activity: &Value,
    target_username: Option<&str>,
) -> anyhow::Result<StatusCode> {
    let obj = match activity.get("object") {
        Some(o) => o,
        None => {
            tracing::debug!("ap Create: missing object");
            return Ok(StatusCode::BAD_REQUEST);
        }
    };

    // Only handle Note objects for now.
    if obj.get("type").and_then(|t| t.as_str()) != Some("Note") {
        tracing::debug!(
            obj_type = ?obj.get("type"),
            "ap Create: skipping non-Note object"
        );
        return Ok(StatusCode::ACCEPTED);
    }

    let note: ApNote = match serde_json::from_value(obj.clone()) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "ap Create: failed to parse Note");
            return Ok(StatusCode::BAD_REQUEST);
        }
    };

    // A Note the audience gate keeps out of the public projection is the DM
    // leg's to deliver or drop (`activitypub.md` § Architecture → *The inbound
    // audience gate*): a direct Note naming an enabled local actor becomes a
    // row in that account's bridged room, sealed; a followers-only one, or one
    // addressed to nobody here, is dropped as before. Either way it never
    // reaches the public path below.
    if !is_publicly_addressed(&note.to, &note.cc)
        && !is_publicly_addressed(
            &string_array(activity.get("to")),
            &string_array(activity.get("cc")),
        )
    {
        super::dm_leg::ingest_non_public_note(state, activity, &note).await?;
        return Ok(StatusCode::ACCEPTED);
    }

    // The audience + relationship gates that bound what/whose Note may enter the
    // public projection. Factored into `note_passes_ingest_gates` so `Update`
    // enforces the identical pair — an ungated `Update{Note}` was a backdoor
    // around both (audit 2026-07-22).
    if !note_passes_ingest_gates(state, activity, &note, target_username, "Create").await? {
        return Ok(StatusCode::ACCEPTED);
    }

    // Ownership (`activitypub.md` § Security posture → Object ownership): a
    // Create claims authorship, so the signer must BE the Note's author and
    // the Note must live on the signer's host. The relationship gate above
    // decides *whether* to store, keyed on the signer; without this the Note's
    // own `attributedTo`/`id` would decide *whose* post it becomes — any
    // remote could plant a post under any actor, or claim another server's
    // note URL before the real one arrives.
    let signed_actor = activity.get("actor").and_then(|v| v.as_str()).unwrap_or("");
    if !same_actor_identity(signed_actor, &note.attributed_to)
        || !same_origin(signed_actor, &note.id)
    {
        tracing::warn!(
            signed_actor,
            attributed_to = %note.attributed_to,
            ap_url = %note.id,
            "ap Create: dropping Note not authored by its signer (spoof attempt)"
        );
        return Ok(StatusCode::ACCEPTED);
    }

    let actor_uri = note.attributed_to.clone();
    let ap_url = note.id.clone();

    let author = synthetic_actor_id(&actor_uri);
    let references = inbound_references(state, &note).await?;
    let post = ap_note_to_fauna_post(&note, &author, ingest_now(), references)?;

    // The bridged planes' future bound (`feed.md` § The read model): the Note's
    // `published` becomes the column the local feed sorts on, so a Note dated
    // past the cushion would lead every local user's feed until real time
    // reached it. Refused rather than re-dated — a clamped date would change
    // the hashed post each replay re-derives. ACCEPTED-and-dropped like the
    // gates above: no retry, no oracle.
    if crate::storage::reject_future_bridged_created_at(post.created_at).is_err() {
        tracing::info!(
            ap_url,
            "ap Create: dropping a Note published past this nest's future bound"
        );
        return Ok(StatusCode::ACCEPTED);
    }

    // BARE-encode the translated post
    let payload = fauna_core::encoding::canonical_encode(&post)?;

    // Content ID = BLAKE3 hash of encoded bytes (same as Fauna's content-addressing)
    let content_id: [u8; 32] = *blake3::hash(&payload).as_bytes();

    // Store the post: body → `__post` segment store, projection with empty
    // payload, source "activitypub" (the post-cutover authoritative body store).
    crate::segments::post::store_post(
        &state.post_segments,
        &state.db,
        &content_id,
        &payload,
        Some("activitypub"),
    )
    .await?;

    // Record the AP URL ↔ Fauna post ID mapping (+ the owning actor URI, so
    // the outbound interact path can resolve the real inbox).
    let post_id_hex = hex::encode(content_id);
    let conn = state.db.conn().await;
    db_helpers::insert_post_map(
        &conn,
        &post_id_hex,
        &ap_url,
        &hex::encode(author.0),
        Some(&actor_uri),
    )?;
    index_into_search_corpus(&conn, &ap_url, &actor_uri, &post, &content_id);
    drop(conn);
    record_inbound_reference_engagements(state, &content_id, &author.0, &payload).await;

    tracing::info!(ap_url, post_id = %post_id_hex, "ap Create: stored inbound Note");
    Ok(StatusCode::ACCEPTED)
}

/// Resolve an inbound Note's threading (`activitypub.md` § Reply and quote →
/// *The inbound half*): an `inReplyTo` naming an object this nest mapped — an
/// ingested note or one of our own pushed notes — becomes `Reference::Reply`
/// to its local id; anything else resolves to nothing and the Note rests
/// top-level, never under a synthetic id.
async fn inbound_references(
    state: &AppState,
    note: &ApNote,
) -> anyhow::Result<Vec<fauna_core::data::Reference>> {
    let Some(parent_url) = note.in_reply_to.as_deref() else {
        return Ok(vec![]);
    };
    let conn = state.db.conn().await;
    let Some(parent_hex) = db_helpers::get_post_id_for_ap_url(&conn, parent_url)? else {
        return Ok(vec![]);
    };
    let mut parent = [0u8; 32];
    if hex::decode_to_slice(&parent_hex, &mut parent).is_err() {
        return Ok(vec![]);
    }
    Ok(vec![fauna_core::data::Reference::Reply {
        post_id: fauna_core::data::ContentHash::from_digest_raw(parent),
    }])
}

/// Move the referenced post's interaction-bar counter for a stored inbound
/// Note, exactly as `fauna.posts.create` does for a local post. Idempotent on
/// the Note's content id and non-fatal: a counter hiccup must not refuse an
/// activity that was already stored.
async fn record_inbound_reference_engagements(
    state: &AppState,
    post_id: &[u8; 32],
    author: &[u8; 32],
    payload: &[u8],
) {
    let now_us = fauna_core::data::Timestamp::now().as_i64();
    if let Err(e) = state
        .db
        .record_reference_engagements(post_id, author, payload, now_us)
        .await
    {
        tracing::warn!("ap ingest: record_reference_engagements: {e}");
    }
}

/// Index an ingested Note into the bridge Search corpus (`content-index.md`
/// § Bridge content in the Search corpus — the AP transit point).
///
/// Safe to call unconditionally from the two inbound ingest paths: everything
/// reaching them has already cleared `note_passes_ingest_gates`, whose audience
/// arm admits **only publicly addressed** Notes — which is exactly the policy's
/// public-content class. Private AP content (DMs, followers-only) is dropped
/// before it is stored at all, so it can never reach this call.
///
/// Removal needs no counterpart here: the `ap_post_map_bridge_search_*` triggers
/// drop the row whenever the map row is tombstoned or deleted.
fn index_into_search_corpus(
    conn: &rusqlite::Connection,
    ap_url: &str,
    actor_uri: &str,
    post: &fauna_core::data::Post,
    post_id: &[u8; 32],
) {
    if let Err(e) = crate::db::bridge_search::index_bridge_content(
        conn,
        "activitypub",
        ap_url,
        actor_uri,
        &post.body_text(),
        // `content_fts_map.created_at` is epoch MICROSECONDS (`db/schema.rs`) —
        // the same unit as `Post::created_at`, so it passes through unscaled.
        // It read SECONDS here until 2026-08-03, on the strength of a
        // since-refuted claim about that column's unit; the search window
        // compares it against `SearchRequest`'s epoch-micros `before`/`after`,
        // so a seconds value put every AP corpus hit at ~1970.
        post.created_at.0 as i64,
        // The rested post — the feed's text filters reach it through this link.
        Some(post_id),
    ) {
        // Never fail the ingest on an indexing problem: the note is the durable
        // thing, the search row is derived and re-creatable.
        tracing::warn!(ap_url, error = %e, "ap: search-corpus indexing failed");
    }
}

/// Handle a `Follow` activity (remote user following a local user).
async fn handle_follow(
    state: &Arc<AppState>,
    activity: &Value,
    target_username: Option<&str>,
) -> anyhow::Result<StatusCode> {
    let actor_uri = match activity.get("actor").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => return Ok(StatusCode::BAD_REQUEST),
    };

    let activity_id = activity.get("id").and_then(|v| v.as_str()).unwrap_or("");

    // Look up the local account being followed. Whichever way the username is
    // determined, the activity's own `object` MUST name this nest's actor URL
    // for that username — the same body-vs-authority agreement `handle_undo`
    // enforces. Without it, the per-user-inbox `Some(u)` arm trusted the ROUTE
    // alone, and on replay the route is the attacker's choice: a captured
    // `Follow` for `bob@mallory.example` replayed to *our* `/ap/users/bob/inbox`
    // would forge `<follower> → bob@here`. The
    // signed-body `object` is what pins the follow to a target the follower
    // actually chose; the route is untrusted input.
    let domain = state.handle_domain();
    let obj_url = activity
        .get("object")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let object_username = match local_actor_username(&domain, obj_url) {
        Some(u) => u,
        None => {
            tracing::debug!(
                obj_url,
                "ap Follow: object does not name a local actor on this nest's authority"
            );
            return Ok(StatusCode::NOT_FOUND);
        }
    };
    let username = match target_username {
        // The route named a username, but the signed body is authoritative: a
        // route/body disagreement is a replay aimed at a username the follower
        // never signed for.
        Some(u) if u == object_username => u.to_string(),
        Some(u) => {
            tracing::debug!(
                route_username = u,
                object_username,
                "ap Follow: route username disagrees with the signed object — dropping a \
                 route-supplied target the follower did not sign for"
            );
            return Ok(StatusCode::NOT_FOUND);
        }
        None => object_username,
    };

    let conn = state.db.conn().await;
    let account = match db_helpers::get_account_by_username(&conn, &username)? {
        Some(a) => a,
        None => {
            drop(conn);
            return Ok(StatusCode::NOT_FOUND);
        }
    };

    db_helpers::create_follow(
        &conn,
        &account.actor_id,
        &actor_uri,
        "inbound",
        Some(activity_id),
    )?;
    drop(conn);

    tracing::debug!(local_user = username, remote = %actor_uri, "ap Follow: recorded inbound follow");

    // Auto-accept if configured — through the one function an approval from
    // the app also runs. With the setting off the row rests `pending`: that
    // row is the follow request (`activitypub.md` § Follow requests).
    if account.auto_accept_follows {
        accept_inbound_follow(state, &account, &actor_uri, activity_id).await?;
        tracing::debug!(local_user = username, remote = %actor_uri, "ap Follow: auto-accepted");
    }

    Ok(StatusCode::ACCEPTED)
}

/// The id our answer to a `Follow` carries: derived from the `Follow`'s own
/// id, so an answer built at once and one built later from the stored row are
/// the same activity, and a retried answer never mints a second one.
fn follow_answer_id(local_actor_url: &str, verb: &str, follow_activity_id: &str) -> String {
    format!(
        "{local_actor_url}/activities/{verb}/{}",
        hex::encode(blake3::hash(follow_activity_id.as_bytes()).as_bytes())
    )
}

/// Enqueue our answer to a `Follow` for the requester's inbox, then nudge the
/// delivery worker — the same durable path every other producer takes. The
/// inbox comes from the cached remote actor the verified inbound `Follow`
/// recorded, never from a client-supplied URL. Best-effort: a requester whose
/// actor is no longer cached, or names no inbox, is logged and skipped — the
/// row's own move does not wait on the delivery.
async fn enqueue_follow_answer(state: &AppState, remote_actor_uri: &str, activity_json: &str) {
    let conn = state.db.conn().await;
    let remote = db_helpers::get_remote_actor(&conn, remote_actor_uri)
        .ok()
        .flatten();
    let target_inbox = remote
        .map(|r| r.shared_inbox.filter(|s| !s.is_empty()).unwrap_or(r.inbox))
        .unwrap_or_default();
    if target_inbox.is_empty() {
        tracing::warn!(remote = %remote_actor_uri, "ap Follow: no cached inbox to answer the requester at");
        return;
    }
    if let Err(e) = db_helpers::enqueue_delivery(&conn, activity_json, &target_inbox) {
        tracing::warn!(error = %e, target_inbox = %target_inbox, "ap Follow: enqueue answer failed");
        return;
    }
    drop(conn);
    state.activitypub.delivery_nudge.notify_one();
}

/// Accept an inbound `Follow`: build `Accept{Follow}`, enqueue-then-nudge it
/// to the requester's inbox, mark the row `accepted`.
///
/// **The one function both callers share** (`activitypub.md` § Follow
/// requests → *Approve*): `handle_follow`'s auto-accept arm and the app's
/// approval of a held-back request, so the two paths cannot drift. It reads
/// only what the stored row keeps — the requester and the `Follow` activity
/// id — which is why the `Accept` embeds a rebuilt `Follow`
/// ([`follow_object_for_answer`]) rather than the inbound activity's JSON.
pub(crate) async fn accept_inbound_follow(
    state: &AppState,
    account: &db_helpers::ApAccount,
    remote_actor_uri: &str,
    follow_activity_id: &str,
) -> anyhow::Result<()> {
    let actor_url = format!(
        "https://{}/ap/users/{}",
        state.handle_domain(),
        account.username
    );
    let accept = build_accept_activity(
        &actor_url,
        &follow_answer_id(&actor_url, "accept", follow_activity_id),
        follow_object_for_answer(follow_activity_id, remote_actor_uri, &actor_url),
    );
    enqueue_follow_answer(state, remote_actor_uri, &serde_json::to_string(&accept)?).await;

    let conn = state.db.conn().await;
    db_helpers::accept_follow(&conn, &account.actor_id, remote_actor_uri, "inbound")?;
    Ok(())
}

/// Refuse a held-back `Follow`: enqueue `Reject{Follow}` naming the stored
/// `Follow` id, then delete the row (`activitypub.md` § Follow requests →
/// *Refuse*). Refusing is not blocking — the requester may ask again, and a
/// later `Follow` is a new request.
pub(crate) async fn reject_inbound_follow(
    state: &AppState,
    account: &db_helpers::ApAccount,
    remote_actor_uri: &str,
    follow_activity_id: &str,
) -> anyhow::Result<()> {
    let actor_url = format!(
        "https://{}/ap/users/{}",
        state.handle_domain(),
        account.username
    );
    let reject = build_reject_activity(
        &actor_url,
        &follow_answer_id(&actor_url, "reject", follow_activity_id),
        follow_object_for_answer(follow_activity_id, remote_actor_uri, &actor_url),
    );
    enqueue_follow_answer(state, remote_actor_uri, &serde_json::to_string(&reject)?).await;

    let conn = state.db.conn().await;
    db_helpers::delete_follow(&conn, &account.actor_id, remote_actor_uri, "inbound")?;
    Ok(())
}

/// Answer one waiting follow request of `account`'s — approve or refuse.
///
/// Idempotent: `Ok(false)` when no *pending* inbound row names
/// `remote_actor_uri` (withdrawn, or already answered from another device),
/// and nothing is sent or changed — so a refusal can never delete a follower
/// that was meanwhile accepted. The requester is the stored row's, matched by
/// the id the list handed out.
pub(crate) async fn resolve_follow_request(
    state: &AppState,
    account: &db_helpers::ApAccount,
    remote_actor_uri: &str,
    approve: bool,
) -> anyhow::Result<bool> {
    let pending = {
        let conn = state.db.conn().await;
        db_helpers::get_pending_inbound_follow(&conn, &account.actor_id, remote_actor_uri)?
    };
    let Some(row) = pending else {
        return Ok(false);
    };
    let follow_id = row.follow_activity_id.as_deref().unwrap_or("");
    if approve {
        accept_inbound_follow(state, account, &row.remote_actor_uri, follow_id).await?;
    } else {
        reject_inbound_follow(state, account, &row.remote_actor_uri, follow_id).await?;
    }
    Ok(true)
}

/// Approve every follow request waiting on `account` — what turning *accept
/// follows by itself* back on does, so requests that arrived while it was off
/// are not stranded behind a switch that says follows are accepted
/// (`activitypub.md` § Follow requests). Returns how many it approved.
pub(crate) async fn accept_all_pending_follows(
    state: &AppState,
    account: &db_helpers::ApAccount,
) -> anyhow::Result<usize> {
    let pending = {
        let conn = state.db.conn().await;
        db_helpers::list_pending_inbound_follows(&conn, &account.actor_id)?
    };
    for row in &pending {
        accept_inbound_follow(
            state,
            account,
            &row.remote_actor_uri,
            row.follow_activity_id.as_deref().unwrap_or(""),
        )
        .await?;
    }
    Ok(pending.len())
}

/// Handle an `Undo` activity (undo a Follow or Like).
async fn handle_undo(state: &Arc<AppState>, activity: &Value) -> anyhow::Result<StatusCode> {
    let actor_uri = match activity.get("actor").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => return Ok(StatusCode::BAD_REQUEST),
    };

    let inner = match activity.get("object") {
        Some(o) => o,
        None => return Ok(StatusCode::BAD_REQUEST),
    };

    let inner_type = inner.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match inner_type {
        "Follow" => {
            // The Follow object's `object` field is the local actor URL.
            let local_url = inner.get("object").and_then(|v| v.as_str()).unwrap_or("");
            let domain = state.handle_domain();
            if let Some(username) = local_actor_username(&domain, local_url) {
                let conn = state.db.conn().await;
                if let Some(account) = db_helpers::get_account_by_username(&conn, &username)? {
                    db_helpers::delete_follow(&conn, &account.actor_id, &actor_uri, "inbound")?;
                    tracing::debug!(local_user = username, remote = %actor_uri, "ap Undo Follow: removed");
                }
            }
        }
        "Like" => {
            let liked_url = inner.get("object").and_then(|v| v.as_str()).unwrap_or("");
            retract_reaction(state, &actor_uri, liked_url, ReactionVerb::Like).await?;
        }
        "Announce" => {
            let boosted_url = inner.get("object").and_then(|v| v.as_str()).unwrap_or("");
            retract_reaction(state, &actor_uri, boosted_url, ReactionVerb::Announce).await?;
        }
        _ => {
            tracing::debug!(inner_type, "ap Undo: unhandled inner type");
        }
    }

    Ok(StatusCode::ACCEPTED)
}

/// The two inbound reaction verbs. One mint/retract path serves both
/// (`mint_synthetic_reaction` / `retract_reaction`) so the relationship
/// gate, the dedupe key, and the Undo retraction can never drift between
/// them.
#[derive(Clone, Copy)]
enum ReactionVerb {
    Like,
    Announce,
}

impl ReactionVerb {
    /// The synthetic map key — stable per (actor, verb, object), deliberately
    /// independent of the activity id: a replayed activity (or one re-sent
    /// under a fresh id) dedupes to the same key, which is what bounds
    /// synthetic-reaction minting to one row per actor+object. It is also
    /// Undo's lookup key, so retraction works from exactly the fields an
    /// Undo's inner object carries. (Announce rows are keyed
    /// on actor+object, never on the activity id.)
    fn map_key(self, actor_uri: &str, object_url: &str) -> String {
        match self {
            ReactionVerb::Like => format!("like:{actor_uri}:{object_url}"),
            ReactionVerb::Announce => format!("announce:{actor_uri}:{object_url}"),
        }
    }

    fn reference(self, target: fauna_cbor::Cid) -> fauna_core::data::Reference {
        match self {
            ReactionVerb::Like => fauna_core::data::Reference::Upvote { post_id: target },
            ReactionVerb::Announce => fauna_core::data::Reference::Repost { post_id: target },
        }
    }

    fn name(self) -> &'static str {
        match self {
            ReactionVerb::Like => "Like",
            ReactionVerb::Announce => "Announce",
        }
    }
}

/// Store a synthetic upvote/repost for an inbound reaction on a mapped
/// object — the shared body of `handle_like` and `handle_announce`.
async fn mint_synthetic_reaction(
    state: &Arc<AppState>,
    actor_uri: &str,
    object_url: &str,
    verb: ReactionVerb,
) -> anyhow::Result<StatusCode> {
    let conn = state.db.conn().await;
    let Some(target_hex) = db_helpers::get_post_id_for_ap_url(&conn, object_url)? else {
        drop(conn);
        tracing::debug!(actor = %actor_uri, object_url, verb = verb.name(), "ap reaction: unmapped object, ignoring");
        return Ok(StatusCode::ACCEPTED);
    };

    // ── Relationship gate ── the same opt-in shape as
    // `handle_create`'s, adapted to reactions: the signature-verified
    // reacting actor is followed by an enabled local account (a followed
    // actor's likes/boosts are subscribed timeline content), or the
    // reacted-to object is an enabled local account's own post (engagement
    // on your own federated posts is what enabling federation subscribes
    // you to — Mastodon-parity, anyone may favorite/boost a public post).
    // A stranger reacting to an ingested remote object is ACCEPTED-and-
    // dropped: no retry, no oracle.
    let opted_in = db_helpers::any_enabled_account_follows(&conn, actor_uri)?
        || db_helpers::enabled_account_owns_ap_url(&conn, object_url)?;
    if !opted_in {
        drop(conn);
        tracing::info!(
            actor = %actor_uri, object_url, verb = verb.name(),
            "ap reaction: dropping (actor not followed by, and object not owned by, any enabled local account)"
        );
        return Ok(StatusCode::ACCEPTED);
    }

    // ── Replay idempotency (same ruling) ── one synthetic reaction per
    // (actor, verb, object): the stable map key dedupes before any store
    // side-effect. Nothing downstream would collapse duplicates — the
    // synthetic post id embeds its mint time, so every replay used to mint a
    // fresh `content` + map row. A tombstoned row (Undo) is deliberately NOT
    // a duplicate: un-like then re-like mints again.
    let map_key = verb.map_key(actor_uri, object_url);
    if db_helpers::get_post_id_for_ap_url(&conn, &map_key)?.is_some() {
        drop(conn);
        tracing::debug!(actor = %actor_uri, object_url, verb = verb.name(), "ap reaction: duplicate, ignoring");
        return Ok(StatusCode::ACCEPTED);
    }
    drop(conn);

    let Ok(target_arr) = fauna_core::hex32::decode(&target_hex) else {
        tracing::warn!(target_hex, "ap reaction: mapped post id is not hex32");
        return Ok(StatusCode::ACCEPTED);
    };

    let synthetic_actor = synthetic_actor_id(actor_uri);
    let post = fauna_core::data::Post {
        author: synthetic_actor,
        created_at: fauna_core::data::Timestamp::now(),
        body: fauna_core::data::PostBody::Text {
            content: String::new(),
            facets: vec![],
        },
        references: vec![verb.reference(fauna_cbor::Cid::from_digest_dag_cbor(target_arr))],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let payload = fauna_core::encoding::canonical_encode(&post)?;
    let content_id: [u8; 32] = *blake3::hash(&payload).as_bytes();
    crate::segments::post::store_post(
        &state.post_segments,
        &state.db,
        &content_id,
        &payload,
        Some("activitypub"),
    )
    .await?;

    let post_id_hex = hex::encode(content_id);
    let conn = state.db.conn().await;
    db_helpers::insert_post_map(
        &conn,
        &post_id_hex,
        &map_key,
        &hex::encode(synthetic_actor.0),
        Some(actor_uri),
    )?;
    drop(conn);

    tracing::debug!(actor = %actor_uri, object_url, verb = verb.name(), "ap reaction: stored synthetic reaction");
    Ok(StatusCode::ACCEPTED)
}

/// Undo's retraction arm: resolve the synthetic reaction row by its stable
/// map key, tombstone the map row, and WITHDRAW the synthetic content post the
/// mint created. Actor-scoped by construction — the key embeds the acting
/// actor, so an actor can only retract its own reaction. (The pre-2026-07-19
/// Undo{Like} hashed the key and passed the digest as a fauna_post_id — it
/// matched nothing, so an un-like never retracted; and Undo{Announce} was
/// entirely unhandled. Pinned by the `undo_*_tombstones_*` tests.)
///
/// The content withdrawal (not just the map-row tombstone) closes a gap: because a tombstoned map row is
/// deliberately not a duplicate (un-react → re-react mints again), tombstoning
/// only the map row would orphan the synthetic `content`/`content_meta`/segment
/// rows one-per-cycle. Minting creates a map row + a content projection + a
/// segment record, so retraction tears down all three — mirroring
/// `delete_post_core`'s projection + segment teardown, minus the counter
/// reversal (the AP mint never calls `record_reference_engagements`) and the
/// propagation legs (a synthetic reaction post is never itself pushed or
/// replicated). Best-effort + non-fatal like every retraction leg: the map-row
/// tombstone already hides the reaction, so a projection hiccup must not fail
/// the Undo.
async fn retract_reaction(
    state: &Arc<AppState>,
    actor_uri: &str,
    object_url: &str,
    verb: ReactionVerb,
) -> anyhow::Result<()> {
    let map_key = verb.map_key(actor_uri, object_url);
    let conn = state.db.conn().await;
    let Some(post_id) = db_helpers::get_post_id_for_ap_url(&conn, &map_key)? else {
        return Ok(());
    };
    db_helpers::tombstone_post_map(&conn, &post_id)?;
    drop(conn);

    withdraw_translated_post(state, &post_id, object_url, verb.name()).await;
    tracing::debug!(object_url, remote = %actor_uri, verb = verb.name(), "ap Undo: retracted synthetic reaction");
    Ok(())
}

/// Tear down a mapped post's **projection + segment record** — the destructive
/// half both inbound AP retraction paths share: `Undo{Like|Announce}`
/// withdrawing a synthetic reaction, and a remote author's `Delete` of a Note
/// we ingested.
///
/// A thin AP-flavoured wrapper over [`crate::bridge_withdraw`], which owns the
/// teardown itself — nostr's NIP-09 / NIP-40 arms call the same function, so
/// the two bridges cannot drift on what "withdraw an ingested post" means. Read
/// that module for the ordering rationale and the best-effort posture.
///
/// Callers own the authz decision. In particular `handle_delete` calls this
/// only for **ingested** notes — a local account's own pushed note is user
/// data, and `fauna.posts.delete` (three author checks) is the only verb that
/// may destroy it.
async fn withdraw_translated_post(
    state: &Arc<AppState>,
    post_id: &str,
    object_url: &str,
    verb: &str,
) {
    crate::bridge_withdraw::withdraw_translated_post(
        &state.db,
        post_id,
        "activitypub",
        object_url,
        verb,
    )
    .await;
}

/// Handle a `Like` activity.
async fn handle_like(
    state: &Arc<AppState>,
    activity: &Value,
    _target_username: Option<&str>,
) -> anyhow::Result<StatusCode> {
    let actor_uri = match activity.get("actor").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return Ok(StatusCode::BAD_REQUEST),
    };
    let liked_url = match activity.get("object").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return Ok(StatusCode::BAD_REQUEST),
    };
    mint_synthetic_reaction(state, actor_uri, liked_url, ReactionVerb::Like).await
}

/// Handle an `Announce` (boost) activity.
async fn handle_announce(
    state: &Arc<AppState>,
    activity: &Value,
    _target_username: Option<&str>,
) -> anyhow::Result<StatusCode> {
    let actor_uri = match activity.get("actor").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return Ok(StatusCode::BAD_REQUEST),
    };
    let boosted_url = match activity.get("object").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return Ok(StatusCode::BAD_REQUEST),
    };
    mint_synthetic_reaction(state, actor_uri, boosted_url, ReactionVerb::Announce).await
}

/// True iff both URLs parse and share a host — the standard AP object-
/// ownership approximation (an actor may only affect objects on its own
/// server). Parse failure → false (fail closed).
pub(super) fn same_origin(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(ua), Ok(ub)) => match (ua.host_str(), ub.host_str()) {
            (Some(ha), Some(hb)) => ha.eq_ignore_ascii_case(hb),
            _ => false,
        },
        _ => false,
    }
}

/// True iff both URLs parse to the same actor identity — full URL equality
/// after normalization (`url::Url` case-folds scheme + host, drops default
/// ports). For a semantically self-authored activity (`Update{Actor}`) host
/// equality is not enough: on a shared multi-user instance it lets a
/// signature-verified sibling (mallory@host) poison another actor's
/// (alice@host) cached key. Parse failure → false (fail closed).
pub(super) fn same_actor_identity(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(ua), Ok(ub)) => ua == ub,
        _ => false,
    }
}

/// Handle a `Delete` activity.
async fn handle_delete(state: &Arc<AppState>, activity: &Value) -> anyhow::Result<StatusCode> {
    let obj = match activity.get("object") {
        Some(o) => o,
        None => return Ok(StatusCode::BAD_REQUEST),
    };

    // Object may be a URI string or an object with "id".
    let ap_url = if let Some(s) = obj.as_str() {
        s.to_string()
    } else if let Some(id) = obj.get("id").and_then(|v| v.as_str()) {
        id.to_string()
    } else {
        return Ok(StatusCode::BAD_REQUEST);
    };

    // Object-ownership gate (`activitypub.md` § Post deletion, inbound
    // hardening): the signing actor may only Delete objects on its own host.
    // Without this, any authenticated remote could tombstone any mapped post
    // — including our own pushed local notes. Dropped as ACCEPTED (no retry).
    let actor_uri = activity.get("actor").and_then(|v| v.as_str()).unwrap_or("");
    if !same_origin(actor_uri, &ap_url) {
        tracing::warn!(
            actor = %actor_uri,
            object = %ap_url,
            "ap Delete: cross-origin object, dropped"
        );
        return Ok(StatusCode::ACCEPTED);
    }

    let conn = state.db.conn().await;
    let mapped = db_helpers::get_post_id_for_ap_url(&conn, &ap_url)?;
    // Is this one of OUR accounts' posts? Decided before the tombstone, and
    // deliberately with a predicate that ignores `tombstoned`/`enabled`, so the
    // answer cannot change under a retry or a federation toggle.
    let ours = db_helpers::is_local_account_post(&conn, &ap_url)?;
    if let Some(ref post_id) = mapped {
        db_helpers::tombstone_post_map(&conn, post_id)?;
        tracing::debug!(
            ap_url,
            fauna_post_id = post_id,
            "ap Delete: tombstoned post"
        );
    } else {
        tracing::debug!(ap_url, "ap Delete: no local post found for URL");
    }
    drop(conn);

    // Withdraw the translated post, mirroring `Undo`'s teardown. Tombstoning
    // only the map row orphaned the `content`/`content_meta`/segment rows —
    // and since nothing in serving reads `ap_post_map.tombstoned` (its only
    // readers are the push/interact URL lookups), a note its author deleted
    // upstream stayed in the Fauna feed forever.
    //
    // **Ingested notes only.** The same-origin gate above already refuses a
    // cross-origin Delete, but this verb is now destructive, so it does not
    // rest on that gate alone: `activitypub.md` § Security posture knowingly
    // accepts that a signature-verified sibling on a shared instance may Delete
    // a same-host actor's object, and that residual must not extend to
    // destroying a *local* user's own post. `fauna.posts.delete`, with its
    // three author checks, stays the only verb that may.
    match mapped {
        Some(post_id) if !ours => {
            withdraw_translated_post(state, &post_id, &ap_url, "Delete").await;
        }
        Some(_) => {
            tracing::debug!(
                ap_url,
                "ap Delete: local account's own post — map tombstoned, projection kept"
            );
        }
        None => {}
    }

    Ok(StatusCode::ACCEPTED)
}

/// Handle an `Update` activity.
/// - If object is a Person, refresh the cached remote actor.
/// - If object is a Note, update the translated post.
///
/// **Object-ownership gates (audit 2026-07-22, Person arm tightened same day):**
/// an `Update` targets an object by id. The **Note** arm — like `Delete`
/// (§ Post deletion, same-origin hardening) — requires the signature-verified
/// actor and the object to share a host; without it any remote could overwrite
/// or tombstone any mapped post, including a local user's own pushed note. The
/// **Person** arm is stricter: an actor update is always self-authored, so the
/// signer must BE the updated actor (`same_actor_identity`, not host equality)
/// — host equality still let a sibling on a shared multi-user instance poison
/// another actor's cached public key, an impersonation vector (subsequent
/// activities "from" the victim would verify against the attacker's key).
/// `Update{Note}` additionally clears the SAME audience + relationship gates
/// as `Create` (`note_passes_ingest_gates`); without them it was a backdoor
/// around both.
async fn handle_update(
    state: &Arc<AppState>,
    activity: &Value,
    target_username: Option<&str>,
) -> anyhow::Result<StatusCode> {
    let obj = match activity.get("object") {
        Some(o) => o,
        None => return Ok(StatusCode::BAD_REQUEST),
    };

    let signed_actor = activity.get("actor").and_then(|v| v.as_str()).unwrap_or("");
    let obj_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match obj_type {
        "Person" | "Service" | "Group" | "Organization" | "Application" => {
            // Refresh the cached remote actor from the Update payload itself.
            let actor_uri = obj.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if actor_uri.is_empty() {
                return Ok(StatusCode::BAD_REQUEST);
            }

            // Ownership: an actor update is always self-authored, so the signer
            // must BE the updated actor — exact identity, not host equality
            // (which would let a sibling on a shared instance poison another
            // actor's cached key; ruling 2026-07-22, § Security posture).
            if !same_actor_identity(signed_actor, actor_uri) {
                tracing::warn!(
                    signed_actor,
                    actor_uri,
                    "ap Update Person: dropping non-self actor update (spoof attempt)"
                );
                return Ok(StatusCode::ACCEPTED);
            }

            let updated_actor = RemoteActor {
                uri: actor_uri.to_string(),
                inbox: obj["inbox"].as_str().unwrap_or("").to_string(),
                shared_inbox: obj
                    .get("endpoints")
                    .and_then(|e| e.get("sharedInbox"))
                    .and_then(|s| s.as_str())
                    .map(String::from),
                public_key_pem: obj["publicKey"]["publicKeyPem"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
                preferred_username: obj
                    .get("preferredUsername")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                display_name: obj.get("name").and_then(|v| v.as_str()).map(String::from),
                avatar_url: obj
                    .get("icon")
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
                banner_url: obj
                    .get("image")
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
                summary: obj
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                last_fetched: crate::db::now_epoch_secs(),
            };

            let conn = state.db.conn().await;
            db_helpers::upsert_remote_actor(&conn, &updated_actor)?;
            drop(conn);

            tracing::debug!(actor_uri, "ap Update Person: refreshed cached remote actor");
        }
        "Note" => {
            let note: ApNote = match serde_json::from_value(obj.clone()) {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(error = %e, "ap Update Note: failed to parse Note");
                    return Ok(StatusCode::BAD_REQUEST);
                }
            };

            // Ownership: a remote may only Update a Note on its own host — else
            // it could overwrite or tombstone another actor's mapped post (or a
            // local user's own pushed note). Mirrors the Delete same-origin gate.
            if !same_origin(signed_actor, &note.id) {
                tracing::warn!(
                    signed_actor, ap_url = %note.id,
                    "ap Update Note: dropping cross-origin note update (spoof attempt)"
                );
                return Ok(StatusCode::ACCEPTED);
            }

            // The identical audience + relationship gates as Create — an Update
            // that stores a Note (including the "unseen post" branch below) must
            // not be a backdoor around them.
            if !note_passes_ingest_gates(state, activity, &note, target_username, "Update").await? {
                return Ok(StatusCode::ACCEPTED);
            }

            let actor_uri = note.attributed_to.clone();
            let ap_url = note.id.clone();

            let author = synthetic_actor_id(&actor_uri);
            // Resolved on every Update, so an edited reply keeps its thread.
            let references = inbound_references(state, &note).await?;
            let post = ap_note_to_fauna_post(&note, &author, ingest_now(), references)?;

            // The same future bound as `handle_create`, before any map row moves.
            if crate::storage::reject_future_bridged_created_at(post.created_at).is_err() {
                tracing::info!(
                    ap_url,
                    "ap Update Note: dropping a Note published past this nest's future bound"
                );
                return Ok(StatusCode::ACCEPTED);
            }

            let payload = fauna_core::encoding::canonical_encode(&post)?;
            let new_content_id: [u8; 32] = *blake3::hash(&payload).as_bytes();

            // Look up the old mapping; if content changed, tombstone the old entry.
            let conn = state.db.conn().await;
            let old_mapping = db_helpers::get_post_id_for_ap_url(&conn, &ap_url)?;
            let unseen = old_mapping.is_none();
            // A first sighting stores like a Create, so it takes Create's
            // authorship rule (§ Object ownership): the same-origin gate above
            // alone would let a same-host sibling plant a post under another
            // actor.
            if unseen && !same_actor_identity(signed_actor, &actor_uri) {
                tracing::warn!(
                    signed_actor,
                    attributed_to = %actor_uri,
                    ap_url,
                    "ap Update Note: dropping unseen Note not authored by its signer (spoof attempt)"
                );
                return Ok(StatusCode::ACCEPTED);
            }
            if let Some(old_post_id) = old_mapping {
                let new_hex = hex::encode(new_content_id);
                if old_post_id != new_hex {
                    db_helpers::tombstone_post_map(&conn, &old_post_id)?;
                    db_helpers::insert_post_map(
                        &conn,
                        &new_hex,
                        &ap_url,
                        &hex::encode(author.0),
                        Some(&actor_uri),
                    )?;
                }
            } else {
                // No existing mapping — treat like a new post (some servers
                // send Update for posts we haven't seen before).
                let new_hex = hex::encode(new_content_id);
                db_helpers::insert_post_map(
                    &conn,
                    &new_hex,
                    &ap_url,
                    &hex::encode(author.0),
                    Some(&actor_uri),
                )?;
            }
            drop(conn);

            // Store the (possibly updated) content — body → `__post` segment
            // store, projection with empty payload; idempotent on the
            // content-addressed id, so re-storing the same id is harmless.
            crate::segments::post::store_post(
                &state.post_segments,
                &state.db,
                &new_content_id,
                &payload,
                Some("activitypub"),
            )
            .await?;
            // Only a first sighting moves the parent's counter: an edit re-hashes
            // the reply, and a second count for the same reply would inflate it.
            if unseen {
                record_inbound_reference_engagements(state, &new_content_id, &author.0, &payload)
                    .await;
            }

            // After the store, so the corpus carries the edited body. The
            // natural id is the AP object id, unchanged by an edit, so this
            // replaces the row rather than adding one — and it runs after the
            // old map row's tombstone above, whose trigger removed the
            // superseded row first.
            {
                let conn = state.db.conn().await;
                index_into_search_corpus(&conn, &ap_url, &actor_uri, &post, &new_content_id);
            }

            tracing::info!(ap_url, post_id = %hex::encode(new_content_id), "ap Update Note: stored updated post");
        }
        _ => {
            tracing::debug!(obj_type, "ap Update: unhandled object type");
        }
    }

    Ok(StatusCode::ACCEPTED)
}

/// Handle an `Accept` activity (remote server accepted our Follow).
async fn handle_accept(state: &Arc<AppState>, activity: &Value) -> anyhow::Result<StatusCode> {
    let actor_uri = match activity.get("actor").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => return Ok(StatusCode::BAD_REQUEST),
    };

    // The object of an Accept is normally the original Follow activity.
    let inner = match activity.get("object") {
        Some(o) => o,
        None => return Ok(StatusCode::BAD_REQUEST),
    };

    // The Follow's `actor` field is the local user who sent the Follow.
    let local_actor_uri = inner.get("actor").and_then(|v| v.as_str()).unwrap_or("");

    if local_actor_uri.is_empty() {
        tracing::debug!("ap Accept: missing actor in wrapped Follow object");
        return Ok(StatusCode::BAD_REQUEST);
    }

    // Extract the local username from the actor URL.
    let domain = state.handle_domain();
    if let Some(username) = local_actor_username(&domain, local_actor_uri) {
        let conn = state.db.conn().await;
        if let Some(account) = db_helpers::get_account_by_username(&conn, &username)? {
            db_helpers::accept_follow(&conn, &account.actor_id, &actor_uri, "outbound")?;
            tracing::debug!(local_user = username, remote = %actor_uri, "ap Accept: outbound follow marked accepted");
        }
        drop(conn);
    } else {
        tracing::debug!(
            local_actor_uri,
            "ap Accept: actor URI does not name a local actor on this nest's authority"
        );
    }

    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn state() -> Arc<AppState> {
        let state = crate::activitypub::ap_state_for_test().await;
        // The inbox binds the signed Host VALUE to this nest's authority
        // and a domainless box refuses deliveries outright — so the
        // pipeline tests run as a CLAIMED box, on the authority the signature
        // fixtures sign for. `domainless_state` below is the un-claimed shape.
        state
            .identity_domain
            .store(Some(Arc::new("local.example".to_string())));
        Arc::new(state)
    }

    /// The pre-claim shape `state()` deliberately is not: all three of
    /// `handle_domain_if_set`'s sources empty, as on a provisioned VPS before
    /// its admin claims it.
    async fn domainless_state() -> Arc<AppState> {
        let state = Arc::new(crate::activitypub::ap_state_for_test().await);
        assert!(
            state.handle_domain_if_set().is_none(),
            "precondition: for_test must stay domainless, or this helper pins nothing"
        );
        state
    }

    // ── Remote-actor document parsing ───────────────────────────────────────
    //
    // Regression set for the 2026-07-22 real-Mastodon finding: a secure-mode
    // peer refused our unsigned actor GET with a 401 whose body was JSON, the
    // fetch read every field with `unwrap_or("")`, and the resulting husk was
    // cached for 24 h. Signature verification then failed against an empty PEM
    // — reported as "PEM preamble contains invalid data (NUL byte)", which
    // reads like a crypto fault and hid the refused fetch entirely.

    /// A peer's error body is JSON, and JSON parses. Only the *required fields*
    /// distinguish it from an actor — so they must be required.
    #[test]
    fn an_error_body_is_not_an_actor() {
        let body = json!({"error": "Request not signed"});
        let err = parse_remote_actor("https://mastodon.test/users/bob", &body)
            .expect_err("an error body must not become an actor");
        assert!(
            err.to_string().contains("missing"),
            "unhelpful diagnosis: {err}",
        );
    }

    /// A key-less document is refused rather than cached. Caching one poisons
    /// every activity from that actor for the 24 h cache lifetime.
    #[test]
    fn an_actor_without_a_public_key_is_refused() {
        let body = json!({"inbox": "https://r.example/inbox"});
        assert!(parse_remote_actor("https://r.example/users/bob", &body).is_err());
    }

    /// Likewise no inbox: we could never deliver to it, and an empty inbox URL
    /// would surface later as an unexplained delivery failure.
    #[test]
    fn an_actor_without_an_inbox_is_refused() {
        let body = json!({"publicKey": {"publicKeyPem": "-----BEGIN PUBLIC KEY-----"}});
        assert!(parse_remote_actor("https://r.example/users/bob", &body).is_err());
    }

    /// The happy path still parses, including the optional fields.
    #[test]
    fn a_complete_actor_document_parses() {
        let body = json!({
            "inbox": "https://r.example/users/bob/inbox",
            "publicKey": {"publicKeyPem": "-----BEGIN PUBLIC KEY-----\nabc\n"},
            "endpoints": {"sharedInbox": "https://r.example/inbox"},
            "preferredUsername": "bob",
            "name": "Bob",
        });
        let actor = parse_remote_actor("https://r.example/users/bob", &body).expect("parses");
        assert_eq!(actor.inbox, "https://r.example/users/bob/inbox");
        assert_eq!(
            actor.shared_inbox.as_deref(),
            Some("https://r.example/inbox")
        );
        assert!(
            actor
                .public_key_pem
                .starts_with("-----BEGIN PUBLIC KEY-----")
        );
        assert_eq!(actor.preferred_username.as_deref(), Some("bob"));
    }

    // ── Remote-actor fetch body cap  ────────────────────────────────
    //
    // The fetch dials the attacker-named `actor` URI BEFORE any signature is
    // checked, so an unbounded read of its reply was an unauthenticated
    // memory-exhaustion lever. These serve a reply from loopback and drive
    // `read_actor_response` — the fetch's own body handling — directly.

    /// A complete actor document, padded with JSON whitespace to exactly `len`
    /// bytes: valid, so only the cap can refuse it.
    fn actor_document_of_len(len: usize) -> Vec<u8> {
        let mut body = serde_json::to_vec(&json!({
            "inbox": "https://r.example/users/bob/inbox",
            "publicKey": {"publicKeyPem": "-----BEGIN PUBLIC KEY-----\nabc\n"},
        }))
        .unwrap();
        assert!(body.len() <= len);
        body.resize(len, b' ');
        body
    }

    async fn read_served(
        head: String,
        body: Vec<u8>,
        endless: bool,
    ) -> anyhow::Result<RemoteActor> {
        use crate::ssrf::test_server::{get, serve_once, within};
        let url = serve_once(head, body, endless).await;
        within(read_actor_response(
            "https://r.example/users/bob",
            get(&url).await,
        ))
        .await
    }

    #[tokio::test]
    async fn an_actor_document_at_the_cap_parses() {
        use crate::ssrf::test_server::head_without_length;
        let body = actor_document_of_len(super::super::outbound::AP_ACTOR_DOCUMENT_MAX_BYTES);
        let actor = read_served(head_without_length("200 OK"), body, false)
            .await
            .expect("a document at the cap is an actor");
        assert_eq!(actor.inbox, "https://r.example/users/bob/inbox");
    }

    /// One byte over, with no `Content-Length` to refuse on: the streaming
    /// read must stop it. `resp.json()` parses it happily — which is the bug.
    #[tokio::test]
    async fn an_actor_document_one_byte_over_the_cap_is_refused() {
        use crate::ssrf::test_server::head_without_length;
        let body = actor_document_of_len(super::super::outbound::AP_ACTOR_DOCUMENT_MAX_BYTES + 1);
        let err = read_served(head_without_length("200 OK"), body, false)
            .await
            .expect_err("an over-cap document must be refused");
        assert!(err.to_string().contains("size cap"), "wrong refusal: {err}");
    }

    /// A 2xx that never stops sending is refused at the cap, not buffered for
    /// the whole dial timeout.
    #[tokio::test]
    async fn an_endless_actor_document_is_refused_at_the_cap() {
        use crate::ssrf::test_server::head_without_length;
        let err = read_served(head_without_length("200 OK"), vec![b' '; 8192], true)
            .await
            .expect_err("an endless document must be refused");
        assert!(err.to_string().contains("size cap"), "wrong refusal: {err}");
    }

    /// An error reply that never stops sending: the diagnostic keeps a short
    /// prefix and the read ends at the error-body cap.
    #[tokio::test]
    async fn an_endless_error_body_is_cut_at_the_cap() {
        use crate::ssrf::test_server::head_without_length;
        let err = read_served(
            head_without_length("500 Internal Server Error"),
            vec![b'e'; 8192],
            true,
        )
        .await
        .expect_err("a 500 is not an actor");
        let msg = err.to_string();
        assert!(msg.contains("HTTP 500"), "status lost: {msg}");
        assert!(msg.ends_with(&"e".repeat(200)), "prefix lost: {msg}");
        assert!(
            !msg.contains(&"e".repeat(201)),
            "diagnostic not truncated: {msg}"
        );
    }

    fn create_note_activity(ap_url: &str, content: &str, to: Value, cc: Value) -> Value {
        json!({
            "type": "Create",
            "id": format!("{ap_url}/activity"),
            "actor": "https://remote.example/users/mallory",
            "to": to,
            "cc": cc,
            "object": {
                "type": "Note",
                "id": ap_url,
                "attributedTo": "https://remote.example/users/mallory",
                "content": content,
                "published": "2026-07-14T00:00:00Z",
                "to": to,
                "cc": cc,
            }
        })
    }

    // ── HTTP Signature digest-coverage gate ─────────────────────────────────
    //
    // `verify_signature` only
    // checks the headers the SIGNER's own `headers=` list names. Step 4's
    // Digest-header match proves the body matches THIS request's Digest
    // header — it does not prove the signature covers Digest at all. A
    // signer who names only `(request-target) host date` produces a
    // signature that verifies no matter what body (and freshly recomputed,
    // unsigned Digest) accompanies it, which is exactly the shape a
    // body-substituting relay would produce.

    async fn seed_remote_actor(state: &Arc<AppState>, actor_uri: &str, public_key_pem: String) {
        let conn = state.db.conn().await;
        db_helpers::upsert_remote_actor(
            &conn,
            &RemoteActor {
                uri: actor_uri.to_string(),
                inbox: format!("{actor_uri}/inbox"),
                shared_inbox: None,
                public_key_pem,
                preferred_username: None,
                display_name: None,
                avatar_url: None,
                banner_url: None,
                summary: None,
                last_fetched: crate::db::now_epoch_secs(),
            },
        )
        .unwrap();
    }

    /// A `Date` inside the inbox's freshness window, formatted by the one
    /// formatter that owns the grammar.
    ///
    /// Every signature test needs this, and needs it to be **now** rather than
    /// a literal: a hard-coded date ages silently past
    /// [`AP_INBOX_DATE_SKEW_SECS`] and then every test using it starts passing
    /// or failing for the wrong reason. That already bit once — PROBE-373-B
    /// asserts a host-omitting signature is refused, and with a stale literal
    /// it would have been refused for its DATE, quietly proving nothing about
    /// `host`. A test whose subject is one floor must satisfy every other.
    fn fresh_date() -> String {
        crate::activitypub::sync_worker::format_http_date(fauna_core::data::Timestamp::now_secs())
    }

    fn sign_inbox_post(
        actor_uri: &str,
        privkey_der: &[u8],
        date: &str,
        host: &str,
        digest: Option<&str>,
    ) -> String {
        fauna_bridge_activitypub::http_signatures::build_signature_header(
            &format!("{actor_uri}#main-key"),
            privkey_der,
            "post",
            "/ap/inbox",
            host,
            date,
            digest,
        )
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn inbox_rejects_a_signature_that_does_not_cover_digest() {
        let state = state().await;
        let (privkey_der, pubkey_pem) =
            fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
        let actor_uri = "https://remote.example/users/mallory";
        seed_remote_actor(&state, actor_uri, pubkey_pem).await;

        let date = &fresh_date();
        let host = "local.example";
        let body = create_note_activity(
            "https://remote.example/notes/1",
            "hello",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
            json!([]),
        )
        .to_string();

        // Signed over (request-target)+host+date only — Digest deliberately
        // left out of the SIGNED set, even though the request carries a
        // Digest header that correctly matches this body.
        let signature = sign_inbox_post(actor_uri, &privkey_der, date, host, None);
        let digest = compute_digest(body.as_bytes());

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/activity+json".parse().unwrap());
        headers.insert("signature", signature.parse().unwrap());
        headers.insert("date", date.parse().unwrap());
        headers.insert("host", host.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());

        let response = shared_inbox(State(state), headers, Bytes::from(body))
            .await
            .into_response();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "a signature that never covered Digest must be refused, even with an \
             internally-consistent Digest header",
        );
    }

    /// Control for the test above: the identical request, correctly signed
    /// over `(request-target) host date digest`, must still be accepted.
    #[tokio::test(flavor = "multi_thread")]
    async fn inbox_accepts_a_signature_that_covers_digest() {
        let state = state().await;
        let (privkey_der, pubkey_pem) =
            fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
        let actor_uri = "https://remote.example/users/mallory";
        seed_remote_actor(&state, actor_uri, pubkey_pem).await;

        let date = &fresh_date();
        let host = "local.example";
        // An activity type this route never special-cases: it clears
        // signature verification, then falls into the `_` dispatch arm, so
        // the assertion below is purely about signature verification, not
        // entangled with Create/Follow ingest-gate business logic.
        let body = json!({
            "type": "SomeUnknownActivityType",
            "id": "https://remote.example/activities/1",
            "actor": actor_uri,
        })
        .to_string();
        let digest = compute_digest(body.as_bytes());
        let signature = sign_inbox_post(actor_uri, &privkey_der, date, host, Some(&digest));

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/activity+json".parse().unwrap());
        headers.insert("signature", signature.parse().unwrap());
        headers.insert("date", date.parse().unwrap());
        headers.insert("host", host.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());

        let response = shared_inbox(State(state), headers, Bytes::from(body))
            .await
            .into_response();
        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "a signature that DOES cover Digest, matching the body, must verify",
        );
    }

    // ── PROBE-373: the two legs the digest-coverage floor did not reach ─────
    //
    // An earlier fix established the right principle — the signed-header set is
    // the SIGNER's choice, so the verifier must impose its own floor — and
    // applied it to `(request-target)` and `digest`. Two legs of the same
    // argument were not taken, and each is red-verified below on unmodified
    // landed code.

    /// Sign the given header pairs by hand. `build_signature_header` always
    /// covers `(request-target) host date [digest]`, so it cannot express a
    /// signature that omits `host` — which is exactly the shape PROBE-373-B
    /// needs.
    fn sign_pairs_by_hand(privkey_der: &[u8], key_id: &str, pairs: &[(String, String)]) -> String {
        use base64::Engine as _;
        let sig_string = fauna_bridge_activitypub::http_signatures::build_signature_string(pairs);
        let signature_bytes =
            fauna_bridge_activitypub::identity::rsa_sign(privkey_der, sig_string.as_bytes())
                .unwrap();
        let signature_b64 = base64::engine::general_purpose::STANDARD.encode(&signature_bytes);
        let headers_list = pairs
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "keyId=\"{key_id}\",algorithm=\"rsa-sha256\",headers=\"{headers_list}\",signature=\"{signature_b64}\""
        )
    }

    /// PROBE-373-A — **an arbitrarily stale Date is accepted.** Nothing on the
    /// inbound path ever parses `Date`: there is no skew window, no nonce, and
    /// no per-activity replay cache. So a signed delivery, once observed by
    /// anyone in the path (a relay, an intermediary, or the receiving nest
    /// itself), stays a valid credential **forever**. `handle_follow` calls
    /// `create_follow` unconditionally and `handle_undo` calls `delete_follow`
    /// the same way, so a captured `Follow`/`Undo` pair is a permanent remote
    /// control over that edge of the follow graph.
    ///
    /// This test signs FULL coverage — `(request-target) host date digest`, so
    /// the signature floor is satisfied — and dates it four years in the past.
    #[tokio::test(flavor = "multi_thread")]
    async fn probe_373_a_a_years_stale_signed_date_must_not_be_accepted() {
        let state = state().await;
        let (privkey_der, pubkey_pem) =
            fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
        let actor_uri = "https://remote.example/users/mallory";
        seed_remote_actor(&state, actor_uri, pubkey_pem).await;

        let stale_date = "Sun, 20 Mar 2022 00:00:00 GMT";
        let host = "local.example";
        let body = json!({
            "type": "SomeUnknownActivityType",
            "id": "https://remote.example/activities/stale",
            "actor": actor_uri,
        })
        .to_string();
        let digest = compute_digest(body.as_bytes());
        let signature = sign_inbox_post(actor_uri, &privkey_der, stale_date, host, Some(&digest));

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/activity+json".parse().unwrap());
        headers.insert("signature", signature.parse().unwrap());
        headers.insert("date", stale_date.parse().unwrap());
        headers.insert("host", host.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());

        let response = shared_inbox(State(state), headers, Bytes::from(body))
            .await
            .into_response();
        assert_ne!(
            response.status(),
            StatusCode::ACCEPTED,
            "a signed delivery dated four years ago must not still be accepted — \
             with no freshness window, every captured delivery is a permanent credential",
        );
    }

    /// PROBE-373-B — **the signature is not bound to any host.**
    /// `(request-target)` covers `method path` and nothing else; the authority
    /// lives only in the `host` header, and `host` is not part of the floor.
    /// The shared-inbox path is the constant `/ap/inbox` on every fauna nest,
    /// so a delivery signed without `host` produces a signing string that is
    /// byte-identical at every nest on the internet — one capture replays to
    /// all of them.
    ///
    /// Signed over `(request-target) date digest` — the signature floor is fully
    /// satisfied — then delivered to a nest whose Host is somebody else's.
    #[tokio::test(flavor = "multi_thread")]
    async fn probe_373_b_a_signature_omitting_host_must_not_verify_at_another_nest() {
        let state = state().await;
        let (privkey_der, pubkey_pem) =
            fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
        let actor_uri = "https://remote.example/users/mallory";
        seed_remote_actor(&state, actor_uri, pubkey_pem).await;

        let date = &fresh_date();
        let body = json!({
            "type": "SomeUnknownActivityType",
            "id": "https://remote.example/activities/replayed",
            "actor": actor_uri,
        })
        .to_string();
        let digest = compute_digest(body.as_bytes());

        // Signed for delivery to nest-a; `host` deliberately NOT covered.
        let pairs = vec![
            ("(request-target)".to_string(), "post /ap/inbox".to_string()),
            ("date".to_string(), date.to_string()),
            ("digest".to_string(), digest.clone()),
        ];
        let signature = sign_pairs_by_hand(&privkey_der, &format!("{actor_uri}#main-key"), &pairs);

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/activity+json".parse().unwrap());
        headers.insert("signature", signature.parse().unwrap());
        headers.insert("date", date.parse().unwrap());
        // A DIFFERENT nest than the one the delivery was signed for.
        headers.insert("host", "some-other-nest.example".parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());

        let response = shared_inbox(State(state), headers, Bytes::from(body))
            .await
            .into_response();
        assert_ne!(
            response.status(),
            StatusCode::ACCEPTED,
            "a signature that never covered `host` is valid at every nest at once — \
             the shared-inbox signing string is identical fleet-wide",
        );
    }

    /// PROBE-374-A — **a delivery signed for ANOTHER nest's authority must
    /// not verify here.**
    ///
    /// A prior finding added `host` to the coverage floor; coverage alone binds no
    /// authority, because `verify_signature` rebuilds the signing string by
    /// reading each covered header back out of *the request as sent*
    /// (`headers_ref.get(name)`) — a replayer forwards the captured `Host`
    /// verbatim alongside the captured signature and the string reproduces
    /// byte-for-byte. The VALUE check (the `host_authority`
    /// comparison in `process_inbox`) is what this probe pins: it is the only
    /// gate standing between this request and 202, so a mutation neutering
    /// that check turns exactly this test red.
    ///
    /// This is not the same statement as PROBE-373-B, which omits `host` from
    /// the signed set. This one satisfies the whole coverage floor —
    /// `(request-target) host date digest`, fresh Date — and differs only in
    /// *whose* authority was signed. The capture is trivially available:
    /// ActivityPub delivers each signed activity to every recipient's nest,
    /// so any nest that receives a delivery holds a valid one to replay.
    ///
    /// The premise is asserted rather than assumed: a hard-coded "foreign"
    /// host that silently became this nest's own would make the test vacuous —
    /// the failure mode that bit PROBE-373-B's stale date literal.
    #[tokio::test(flavor = "multi_thread")]
    async fn probe_374_a_a_delivery_signed_for_another_nest_must_not_verify_here() {
        let state = state().await;
        let (privkey_der, pubkey_pem) =
            fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
        let actor_uri = "https://remote.example/users/mallory";
        seed_remote_actor(&state, actor_uri, pubkey_pem).await;

        // The authority the delivery was signed for — deliberately not ours.
        let foreign_host = "some-other-nest.example";
        assert_ne!(
            state.handle_domain(),
            foreign_host,
            "premise: the signed host must be a DIFFERENT authority than this nest's, \
             or this test proves nothing",
        );

        let date = &fresh_date();
        let body = json!({
            "type": "SomeUnknownActivityType",
            "id": "https://remote.example/activities/cross-nest-replay",
            "actor": actor_uri,
        })
        .to_string();
        let digest = compute_digest(body.as_bytes());

        // FULL coverage — `(request-target) host date digest` — so every leg of
        // the floor is satisfied. Only the authority is somebody else's.
        let signature = sign_inbox_post(actor_uri, &privkey_der, date, foreign_host, Some(&digest));

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/activity+json".parse().unwrap());
        headers.insert("signature", signature.parse().unwrap());
        headers.insert("date", date.parse().unwrap());
        headers.insert("host", foreign_host.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());

        let response = shared_inbox(State(state), headers, Bytes::from(body))
            .await
            .into_response();
        assert_ne!(
            response.status(),
            StatusCode::ACCEPTED,
            "a delivery signed for `{foreign_host}` was accepted by a nest that is not \
             `{foreign_host}` — covering `host` binds nothing while its value goes \
             unchecked, so one captured delivery still replays fleet-wide",
        );
    }

    /// **A domainless box refuses inbound deliveries as not-configured** —
    /// the same posture `actor_routes::webfinger` holds on the discovery
    /// direction. There is no authority for the host-value check to bind, and
    /// no peer can legitimately hold a URL of ours to deliver to. The request
    /// here is otherwise fully valid — correctly signed, full coverage, fresh
    /// Date — so the refusal is attributable to the missing domain alone.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_domainless_box_refuses_deliveries_as_not_configured() {
        let state = domainless_state().await;
        let (privkey_der, pubkey_pem) =
            fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
        let actor_uri = "https://remote.example/users/mallory";
        seed_remote_actor(&state, actor_uri, pubkey_pem).await;

        let date = &fresh_date();
        let host = "local.example";
        let body = json!({
            "type": "SomeUnknownActivityType",
            "id": "https://remote.example/activities/pre-claim",
            "actor": actor_uri,
        })
        .to_string();
        let digest = compute_digest(body.as_bytes());
        let signature = sign_inbox_post(actor_uri, &privkey_der, date, host, Some(&digest));

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/activity+json".parse().unwrap());
        headers.insert("signature", signature.parse().unwrap());
        headers.insert("date", date.parse().unwrap());
        headers.insert("host", host.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());

        let response = shared_inbox(State(state), headers, Bytes::from(body))
            .await
            .into_response();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "a box with no identity domain must refuse deliveries as not-configured, \
             matching the WebFinger discovery posture",
        );
    }

    /// **The `:443` spelling of our own authority is accepted.** AP rides
    /// https, so `local.example` and `local.example:443` name the same
    /// authority; a peer spelling the default port explicitly must not be
    /// refused. (The signature still verifies because the signer and the
    /// request carry the same spelling — the value check is the only place
    /// normalization happens.)
    #[tokio::test(flavor = "multi_thread")]
    async fn a_default_port_spelling_of_our_authority_is_accepted() {
        let state = state().await;
        let (privkey_der, pubkey_pem) =
            fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
        let actor_uri = "https://remote.example/users/mallory";
        seed_remote_actor(&state, actor_uri, pubkey_pem).await;

        let date = &fresh_date();
        let host = "local.example:443";
        let body = json!({
            "type": "SomeUnknownActivityType",
            "id": "https://remote.example/activities/default-port",
            "actor": actor_uri,
        })
        .to_string();
        let digest = compute_digest(body.as_bytes());
        let signature = sign_inbox_post(actor_uri, &privkey_der, date, host, Some(&digest));

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/activity+json".parse().unwrap());
        headers.insert("signature", signature.parse().unwrap());
        headers.insert("date", date.parse().unwrap());
        headers.insert("host", host.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());

        let response = shared_inbox(State(state), headers, Bytes::from(body))
            .await
            .into_response();
        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "`local.example:443` is the same https authority as `local.example` — \
             the default-port spelling must not be refused",
        );
    }

    /// **Our hostname on a DIFFERENT port is another authority.** The
    /// identity domain of a dev/e2e box is an `ip:port` netloc, and two such
    /// nests can share a hostname while differing only in port — a delivery
    /// signed for one must not verify at the other. Port-blind comparison
    /// (the web HostResolver's deliberate catch-all posture) would silently
    /// re-open exactly the cross-authority replay the value check refuses.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_delivery_for_our_host_on_another_port_is_refused() {
        let state = state().await;
        let (privkey_der, pubkey_pem) =
            fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
        let actor_uri = "https://remote.example/users/mallory";
        seed_remote_actor(&state, actor_uri, pubkey_pem).await;

        let date = &fresh_date();
        let host = "local.example:8443";
        let body = json!({
            "type": "SomeUnknownActivityType",
            "id": "https://remote.example/activities/port-mismatch",
            "actor": actor_uri,
        })
        .to_string();
        let digest = compute_digest(body.as_bytes());
        let signature = sign_inbox_post(actor_uri, &privkey_der, date, host, Some(&digest));

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/activity+json".parse().unwrap());
        headers.insert("signature", signature.parse().unwrap());
        headers.insert("date", date.parse().unwrap());
        headers.insert("host", host.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());

        let response = shared_inbox(State(state), headers, Bytes::from(body))
            .await
            .into_response();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "`local.example:8443` is not this nest's authority (`local.example` = :443)",
        );
    }

    /// The `host_authority` normalization table: which spellings are the SAME
    /// authority (case, trailing FQDN dot, explicit `:443`), which are
    /// different (other ports), and which are not authorities at all
    /// (userinfo, paths, unbracketed IPv6, garbage ports) — the last group
    /// must never accidentally equal a real authority, which is what makes
    /// the pipeline's `!=` comparison fail closed on them.
    #[test]
    fn host_authority_normalizes_equivalent_spellings_and_rejects_non_authorities() {
        let base = host_authority("local.example");
        assert_eq!(base, Some(("local.example".to_string(), 443)));
        // Equivalent spellings of the same https authority.
        for spelling in [
            "LOCAL.example",
            "local.example.",
            "local.example:443",
            " local.example ",
        ] {
            assert_eq!(host_authority(spelling), base, "{spelling:?}");
        }
        // Different authorities.
        assert_ne!(host_authority("local.example:8443"), base);
        assert_ne!(host_authority("other.example"), base);
        // An explicit-port identity domain (the e2e netloc shape) keeps its port.
        assert_eq!(
            host_authority("127.0.0.1:13101"),
            Some(("127.0.0.1".to_string(), 13101)),
        );
        // IPv6 literals: bracketed forms parse, the port default applies.
        assert_eq!(host_authority("[::1]"), Some(("[::1]".to_string(), 443)));
        assert_eq!(
            host_authority("[::1]:8443"),
            Some(("[::1]".to_string(), 8443))
        );
        // Non-authorities: none may equal a real authority.
        for garbage in [
            "",
            ":443",
            "local.example:",
            "local.example:x",
            "::1",
            "[::1",
        ] {
            assert_eq!(host_authority(garbage), None, "{garbage:?}");
        }
        // Userinfo/path shapes parse as hosts that equal no DNS authority —
        // refused by inequality rather than by grammar-policing.
        assert_ne!(host_authority("evil@local.example"), base);
        assert_ne!(host_authority("local.example/evil"), base);
    }

    /// The actor-URL twin of the table above: `local_actor_username` accepts
    /// exactly the spellings whose authority equals ours (the Host door's
    /// set) and refuses every non-actor shape — scheme, path, empty
    /// username, userinfo, non-authority netlocs.
    #[test]
    fn local_actor_username_accepts_exactly_our_authority_spellings() {
        let domain = "local.example";
        for (spelling, same) in authority_spellings(domain) {
            let url = format!("https://{spelling}/ap/users/alice");
            let got = local_actor_username(domain, &url);
            assert_eq!(got.is_some(), same, "{url:?}");
            if same {
                assert_eq!(got.as_deref(), Some("alice"));
            }
        }
        // Trailing path segments keep the first segment (the inbox form).
        assert_eq!(
            local_actor_username(domain, "https://local.example/ap/users/alice/inbox").as_deref(),
            Some("alice"),
        );
        // Shapes that are never a local actor URL.
        for bad in [
            "http://local.example/ap/users/alice", // wrong scheme — AP rides https
            "https://local.example/ap/users/",     // empty username
            "https://local.example/users/alice",   // wrong path
            "https://local.example",               // no path at all
            "https://evil@local.example/ap/users/alice", // userinfo netloc
            "https://local.example:x/ap/users/alice", // garbage port — not an authority
        ] {
            assert_eq!(local_actor_username(domain, bad), None, "{bad:?}");
        }
        // An `ip:port` identity domain (the dev/e2e netloc shape) matches
        // only its own explicit port — the set is deliberately not port-blind.
        assert_eq!(
            local_actor_username("127.0.0.1:13101", "https://127.0.0.1:13101/ap/users/alice")
                .as_deref(),
            Some("alice"),
        );
        assert_eq!(
            local_actor_username("127.0.0.1:13101", "https://127.0.0.1/ap/users/alice"),
            None,
        );
    }

    /// **A covered-but-unreadable `Date` is refused, not waved through.**
    ///
    /// The freshness window's third branch, and the one no probe covered: the
    /// signer names `date` in its header list and signs it, satisfying the
    /// coverage floor, but the value is not an RFC 7231 HTTP-date. The
    /// signature verifies — the signing string contains whatever bytes the
    /// signer put there — so coverage alone cannot decide this request; only
    /// parsing can, and parsing fails.
    ///
    /// The tempting shape is to treat unparseable as "no evidence of
    /// staleness" and continue. That inverts the whole remedy: it hands any
    /// replayer a one-token bypass of the window (`Date: whatever`), and it
    /// would also make an obsolete-format sender — RFC 850 / asctime, which
    /// `parse_http_date` deliberately refuses — silently unbounded instead of
    /// loudly rejected. Found by the mutation round: replacing the refusal with
    /// "treat as now" left all forty tests in this module green.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_covered_but_unparseable_date_is_refused_not_treated_as_fresh() {
        let state = state().await;
        let (privkey_der, pubkey_pem) =
            fauna_bridge_activitypub::identity::generate_rsa_keypair().unwrap();
        let actor_uri = "https://remote.example/users/mallory";
        seed_remote_actor(&state, actor_uri, pubkey_pem).await;

        // Well-formed enough to be a header, not an HTTP-date. The RFC 850
        // spelling is deliberate: it is a real date a real sender could emit,
        // so this pins the refusal of the obsolete grammars too.
        let bad_date = "Thursday, 01-Jan-70 00:00:00 GMT";
        let host = "local.example";
        let body = json!({
            "type": "SomeUnknownActivityType",
            "id": "https://remote.example/activities/unreadable-date",
            "actor": actor_uri,
        })
        .to_string();
        let digest = compute_digest(body.as_bytes());
        // FULL coverage, including `date` — the floor is satisfied and the
        // signature is valid over exactly these bytes.
        let signature = sign_inbox_post(actor_uri, &privkey_der, bad_date, host, Some(&digest));

        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/activity+json".parse().unwrap());
        headers.insert("signature", signature.parse().unwrap());
        headers.insert("date", bad_date.parse().unwrap());
        headers.insert("host", host.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());

        let response = shared_inbox(State(state), headers, Bytes::from(body))
            .await
            .into_response();
        assert_ne!(
            response.status(),
            StatusCode::ACCEPTED,
            "a Date this nest cannot read is a peer it cannot date; accepting it \
             would make `Date: <garbage>` a one-token bypass of the whole window",
        );
    }

    /// A remote DM / followers-only Note — `to`/`cc` without the
    /// AS Public collection — must NOT enter the public `post/*` projection.
    /// Before the audience gate it was stored ungated with the full body and
    /// served to any authenticated user via `fauna.search.query`.
    #[tokio::test(flavor = "multi_thread")]
    async fn non_public_inbound_note_is_not_stored_or_indexed() {
        let state = state().await;
        let ap_url = "https://remote.example/notes/dm1";
        let activity = create_note_activity(
            ap_url,
            "SECRETDMWORD only for alice",
            json!(["https://local.example/ap/users/alice"]),
            json!([]),
        );

        let code = handle_create(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED, "drop, don't error (no retry)");

        let conn = state.db.conn().await;
        let mapped = db_helpers::get_post_id_for_ap_url(&conn, ap_url).unwrap();
        drop(conn);
        assert!(
            mapped.is_none(),
            "a non-public inbound Note must not acquire a post/* row"
        );

        let hits = state
            .db
            .search_fts("SECRETDMWORD", None, None, None, 10, 0)
            .await
            .unwrap();
        assert!(
            hits.is_empty(),
            "a non-public inbound Note must not be full-text-searchable"
        );
    }

    /// The relationship gate: a validly-signed, publicly
    /// addressed Note from a stranger, POSTed to the shared inbox of a nest
    /// where NO local account follows the actor and the note addresses no
    /// enabled local account, must NOT be stored — no `content`/`content_meta`
    /// row, no post map. ACCEPTED (no retry, no oracle), but dropped.
    #[tokio::test(flavor = "multi_thread")]
    async fn unsolicited_public_inbound_note_is_dropped() {
        let state = state().await;
        let ap_url = "https://remote.example/notes/unsolicited1";
        let activity = create_note_activity(
            ap_url,
            "spam nobody here asked for",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
            json!([]),
        );

        let code = handle_create(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        let mapped = db_helpers::get_post_id_for_ap_url(&conn, ap_url).unwrap();
        drop(conn);
        assert!(
            mapped.is_none(),
            "an unsolicited public Note (no local follow, no addressed enabled account) must not be stored"
        );
    }

    /// Seed an enabled local account that follows `remote` — the opt-in the
    /// relationship gate requires for follow-driven ingest.
    async fn seed_follow_of(state: &Arc<AppState>, remote: &str) {
        let conn = state.db.conn().await;
        db_helpers::create_account(
            &conn,
            "actor-alice",
            "alice",
            "https://nest.example/ap/users/alice",
            &[],
            "pem",
        )
        .unwrap();
        db_helpers::create_follow(&conn, "actor-alice", remote, "outbound", None).unwrap();
    }

    /// Companion: a genuinely Public inbound Note (AS Public in `to`) where `handle_follow` must not let a **route**-supplied
    /// username outvote the signed **object**. On replay the route is the
    /// attacker's choice: a `Follow` captured for one target, redelivered to a
    /// different `/ap/users/{u}/inbox`, would forge `<follower> → u@here`. The
    /// signed `object` names the target the follower actually chose; when the
    /// route disagrees with it, or the object is not a local actor on this
    /// nest's authority at all, the follow is dropped and NO edge is created.
    #[tokio::test(flavor = "multi_thread")]
    async fn follow_drops_a_route_username_the_signed_object_did_not_name() {
        let state = state().await;
        let domain = state.handle_domain();
        // Two local accounts on this nest's authority.
        {
            let conn = state.db.conn().await;
            for u in ["bob", "alice"] {
                db_helpers::create_account(
                    &conn,
                    &format!("actor-{u}"),
                    u,
                    &format!("https://{domain}/ap/users/{u}"),
                    &[],
                    "pem",
                )
                .unwrap();
            }
        }
        let remote = "https://mallory.example/users/mallory";

        // (a) Route says `bob`, but the signed object names `alice` on our own
        // authority — a replay aimed at a username the follower never signed
        // for. Refused, and neither account gains a follower.
        let cross = json!({
            "type": "Follow",
            "id": "https://mallory.example/activities/replay-1",
            "actor": remote,
            "object": format!("https://{domain}/ap/users/alice"),
        });
        assert_eq!(
            handle_follow(&state, &cross, Some("bob")).await.unwrap(),
            StatusCode::NOT_FOUND,
        );

        // (b) Route says `bob`, object names `bob` on a FOREIGN authority — the
        // captured-for-another-nest shape. Refused too.
        let foreign = json!({
            "type": "Follow",
            "id": "https://mallory.example/activities/replay-2",
            "actor": remote,
            "object": "https://mallory.example/ap/users/bob",
        });
        assert_eq!(
            handle_follow(&state, &foreign, Some("bob")).await.unwrap(),
            StatusCode::NOT_FOUND,
        );

        {
            let conn = state.db.conn().await;
            for u in ["bob", "alice"] {
                assert!(
                    db_helpers::list_followers(&conn, &format!("actor-{u}"))
                        .unwrap()
                        .is_empty(),
                    "a route/object disagreement must create no follow edge for {u}",
                );
            }
        }

        // (c) The honest case: route and signed object agree on `bob@here`.
        // The follow is recorded.
        let honest = json!({
            "type": "Follow",
            "id": "https://mallory.example/activities/honest",
            "actor": remote,
            "object": format!("https://{domain}/ap/users/bob"),
        });
        assert_eq!(
            handle_follow(&state, &honest, Some("bob")).await.unwrap(),
            StatusCode::ACCEPTED,
        );
        {
            let conn = state.db.conn().await;
            let followers = db_helpers::list_followers(&conn, "actor-bob").unwrap();
            assert_eq!(followers.len(), 1, "the agreeing follow is recorded");
            assert_eq!(followers[0].remote_actor_uri, remote);
        }
    }

    // ── Follow requests (`activitypub.md` § Follow requests) ────────────

    const REQUESTER: &str = "https://remote.example/users/bob";
    const FOLLOW_ID: &str = "https://remote.example/activities/follow-1";

    /// A claimed nest with one local account, `alice`, whose *accept follows
    /// by itself* setting is `auto_accept`, and the requester's actor cached —
    /// as the signature check leaves it before `handle_follow` runs.
    async fn follow_request_state(auto_accept: bool) -> Arc<AppState> {
        let state = state().await;
        let domain = state.handle_domain();
        {
            let conn = state.db.conn().await;
            db_helpers::create_account(
                &conn,
                "actor-alice",
                "alice",
                &format!("https://{domain}/ap/users/alice"),
                &[],
                "pem",
            )
            .unwrap();
            db_helpers::update_settings(
                &conn,
                "actor-alice",
                &db_helpers::ApSettings {
                    auto_accept_follows: Some(auto_accept),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        seed_remote_actor(&state, REQUESTER, "pem".into()).await;
        state
    }

    async fn deliver_follow(state: &Arc<AppState>, requester: &str, follow_id: &str) {
        let follow = json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "type": "Follow",
            "id": follow_id,
            "actor": requester,
            "object": format!("https://{}/ap/users/alice", state.handle_domain()),
            // A field of the requester's own choosing: no answer echoes it.
            "summary": "text the requester chose",
        });
        assert_eq!(
            handle_follow(state, &follow, Some("alice")).await.unwrap(),
            StatusCode::ACCEPTED,
        );
    }

    async fn alice(state: &Arc<AppState>) -> db_helpers::ApAccount {
        let conn = state.db.conn().await;
        db_helpers::get_account(&conn, "actor-alice")
            .unwrap()
            .expect("alice")
    }

    /// Every queued delivery as `(activity, target inbox)`, oldest first.
    async fn queued_answers(state: &Arc<AppState>) -> Vec<(Value, String)> {
        let conn = state.db.conn().await;
        let mut stmt = conn
            .prepare("SELECT activity_json, target_inbox FROM ap_delivery_queue ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(|row| {
                let (activity, inbox) = row.unwrap();
                (serde_json::from_str(&activity).unwrap(), inbox)
            })
            .collect()
    }

    /// The inbound rows of `alice` as `(requester, state)`.
    async fn inbound_rows(state: &Arc<AppState>) -> Vec<(String, String)> {
        let conn = state.db.conn().await;
        db_helpers::list_followers(&conn, "actor-alice")
            .unwrap()
            .into_iter()
            .map(|f| (f.remote_actor_uri, f.state))
            .collect()
    }

    /// The auto-accept arm and an approval from the app are ONE function, so
    /// a `Follow` answered at the door and one answered later from the stored
    /// row produce the same `Accept`, to the same inbox, and the same row.
    #[tokio::test(flavor = "multi_thread")]
    async fn auto_accept_and_approval_produce_the_same_accept_and_row_state() {
        let auto = follow_request_state(true).await;
        deliver_follow(&auto, REQUESTER, FOLLOW_ID).await;

        let held = follow_request_state(false).await;
        deliver_follow(&held, REQUESTER, FOLLOW_ID).await;
        assert!(
            queued_answers(&held).await.is_empty(),
            "a held-back follow answers nothing until the account decides"
        );
        assert_eq!(
            inbound_rows(&held).await,
            vec![(REQUESTER.to_string(), "pending".to_string())],
            "the pending row IS the follow request"
        );
        assert!(
            resolve_follow_request(&held, &alice(&held).await, REQUESTER, true)
                .await
                .unwrap(),
            "the waiting request was answered"
        );

        let from_auto = queued_answers(&auto).await;
        let from_approval = queued_answers(&held).await;
        assert_eq!(from_auto.len(), 1);
        assert_eq!(
            from_auto, from_approval,
            "the two paths must enqueue the same Accept to the same inbox"
        );
        let (accept, inbox) = &from_auto[0];
        assert_eq!(inbox, &format!("{REQUESTER}/inbox"));
        assert_eq!(accept["type"], "Accept");
        assert_eq!(accept["actor"], "https://local.example/ap/users/alice");
        assert_eq!(accept["object"]["type"], "Follow");
        assert_eq!(accept["object"]["id"], FOLLOW_ID);
        assert_eq!(accept["object"]["actor"], REQUESTER);
        assert_eq!(
            accept["object"]["object"],
            "https://local.example/ap/users/alice"
        );
        assert!(
            !accept.to_string().contains("text the requester chose"),
            "the answer embeds a rebuilt Follow, never the requester's own JSON"
        );

        let accepted = vec![(REQUESTER.to_string(), "accepted".to_string())];
        assert_eq!(inbound_rows(&auto).await, accepted);
        assert_eq!(inbound_rows(&held).await, accepted);
    }

    /// Refusing sends `Reject{Follow}` naming the stored `Follow` id and
    /// deletes the row; refusing is not blocking, so the same requester's
    /// later `Follow` is a new request, pending again.
    #[tokio::test(flavor = "multi_thread")]
    async fn refusing_a_follow_enqueues_reject_deletes_the_row_and_a_second_follow_is_pending_again()
     {
        let state = follow_request_state(false).await;
        deliver_follow(&state, REQUESTER, FOLLOW_ID).await;

        assert!(
            resolve_follow_request(&state, &alice(&state).await, REQUESTER, false)
                .await
                .unwrap()
        );
        let answers = queued_answers(&state).await;
        assert_eq!(answers.len(), 1, "exactly one answer: the Reject");
        let (reject, inbox) = &answers[0];
        assert_eq!(inbox, &format!("{REQUESTER}/inbox"));
        assert_eq!(reject["type"], "Reject");
        assert_eq!(reject["actor"], "https://local.example/ap/users/alice");
        assert_eq!(reject["object"]["type"], "Follow");
        assert_eq!(
            reject["object"]["id"], FOLLOW_ID,
            "the Reject names the stored Follow activity id"
        );
        assert_eq!(reject["object"]["actor"], REQUESTER);
        assert!(
            inbound_rows(&state).await.is_empty(),
            "a refused request leaves no row"
        );

        // Answering it again — from another device, say — succeeds and does
        // nothing (the wire kind is idempotent).
        assert!(
            !resolve_follow_request(&state, &alice(&state).await, REQUESTER, false)
                .await
                .unwrap()
        );
        assert_eq!(queued_answers(&state).await.len(), 1);

        // The requester asks again: a new request, held back like the first.
        let second = "https://remote.example/activities/follow-2";
        deliver_follow(&state, REQUESTER, second).await;
        assert_eq!(
            inbound_rows(&state).await,
            vec![(REQUESTER.to_string(), "pending".to_string())],
        );
        let conn = state.db.conn().await;
        let row = db_helpers::get_pending_inbound_follow(&conn, "actor-alice", REQUESTER)
            .unwrap()
            .expect("the second request");
        assert_eq!(row.follow_activity_id.as_deref(), Some(second));
    }

    /// A refusal acts on a *request* only: once a follower is accepted, a
    /// late refuse (a stale card on another device) must not delete it or
    /// send a `Reject`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_late_refusal_never_removes_an_accepted_follower() {
        let state = follow_request_state(false).await;
        deliver_follow(&state, REQUESTER, FOLLOW_ID).await;
        let account = alice(&state).await;
        assert!(
            resolve_follow_request(&state, &account, REQUESTER, true)
                .await
                .unwrap()
        );
        assert!(
            !resolve_follow_request(&state, &account, REQUESTER, false)
                .await
                .unwrap(),
            "nothing is waiting any more"
        );
        assert_eq!(
            inbound_rows(&state).await,
            vec![(REQUESTER.to_string(), "accepted".to_string())],
        );
        let answers = queued_answers(&state).await;
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].0["type"], "Accept");
    }

    /// Turning *accept follows by itself* back on — through the provider's
    /// own `update_settings`, the path `fauna.bridges.set_settings` takes —
    /// approves every request that arrived while it was off, and the list
    /// kind's read then shows none.
    #[tokio::test(flavor = "multi_thread")]
    async fn turning_auto_accept_on_approves_every_pending_follow() {
        use crate::activitypub::bridge_provider::ActivityPubProvider;
        use crate::bridge_management::BridgeProvider;

        let state = follow_request_state(false).await;
        let carol = "https://other.example/users/carol";
        {
            let conn = state.db.conn().await;
            db_helpers::upsert_remote_actor(
                &conn,
                &RemoteActor {
                    uri: carol.to_string(),
                    inbox: format!("{carol}/inbox"),
                    shared_inbox: None,
                    public_key_pem: "pem".into(),
                    preferred_username: Some("carol".into()),
                    display_name: Some("Carol".into()),
                    avatar_url: None,
                    banner_url: None,
                    summary: None,
                    last_fetched: crate::db::now_epoch_secs(),
                },
            )
            .unwrap();
        }
        deliver_follow(&state, REQUESTER, FOLLOW_ID).await;
        deliver_follow(&state, carol, "https://other.example/activities/f-9").await;

        let provider = ActivityPubProvider;
        assert!(provider.supports_follow_requests());
        let listed = provider
            .list_follow_requests(&state, "actor-alice")
            .await
            .expect("list");
        let mut ids: Vec<&str> = listed.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![carol, REQUESTER]);
        let carol_row = listed.iter().find(|r| r.id == carol).unwrap();
        assert_eq!(carol_row.name.as_deref(), Some("Carol"));
        assert_eq!(
            fauna_cbor::encode_canonical(&carol_row.extra).unwrap(),
            fauna_cbor::encode_canonical(&Some(fauna_protocol::Value::Map(
                std::collections::BTreeMap::from([(
                    "handle".to_string(),
                    fauna_protocol::Value::String("@carol@other.example".into()),
                )])
            )))
            .unwrap(),
            "the requester's typed address rides `extra.handle`"
        );
        assert!(carol_row.requested_at.is_some());

        // A write that leaves the switch OFF answers nothing.
        let settings = |on: bool| {
            fauna_protocol::Value::Map(std::collections::BTreeMap::from([(
                "auto_accept_follows".to_string(),
                fauna_protocol::Value::Bool(on),
            )]))
        };
        provider
            .update_settings(&state, "actor-alice", settings(false))
            .await
            .expect("set off");
        assert!(queued_answers(&state).await.is_empty());

        provider
            .update_settings(&state, "actor-alice", settings(true))
            .await
            .expect("set on");
        let mut rows = inbound_rows(&state).await;
        rows.sort();
        assert_eq!(
            rows,
            vec![
                (carol.to_string(), "accepted".to_string()),
                (REQUESTER.to_string(), "accepted".to_string()),
            ],
            "off→on approves every pending row"
        );
        let answers = queued_answers(&state).await;
        assert_eq!(answers.len(), 2);
        assert!(answers.iter().all(|(a, _)| a["type"] == "Accept"));
        let mut inboxes: Vec<&str> = answers.iter().map(|(_, i)| i.as_str()).collect();
        inboxes.sort_unstable();
        assert_eq!(
            inboxes,
            vec![
                "https://other.example/users/carol/inbox",
                "https://remote.example/users/bob/inbox",
            ]
        );
        assert!(
            provider
                .list_follow_requests(&state, "actor-alice")
                .await
                .expect("list")
                .is_empty()
        );

        // Saving the switch on again re-sends nothing: nothing is waiting.
        provider
            .update_settings(&state, "actor-alice", settings(true))
            .await
            .expect("set on again");
        assert_eq!(queued_answers(&state).await.len(), 2);
    }

    /// A requester who withdraws (`Undo{Follow}`) drops off the list with no
    /// user action, and answering the request afterwards is a no-op.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_withdrawn_request_leaves_the_list_by_itself() {
        let state = follow_request_state(false).await;
        deliver_follow(&state, REQUESTER, FOLLOW_ID).await;
        let undo = json!({
            "type": "Undo",
            "actor": REQUESTER,
            "object": {
                "type": "Follow",
                "id": FOLLOW_ID,
                "actor": REQUESTER,
                "object": format!("https://{}/ap/users/alice", state.handle_domain()),
            },
        });
        handle_undo(&state, &undo).await.unwrap();
        assert!(inbound_rows(&state).await.is_empty());
        assert!(
            !resolve_follow_request(&state, &alice(&state).await, REQUESTER, true)
                .await
                .unwrap()
        );
        assert!(queued_answers(&state).await.is_empty());
    }

    /// The spelling table for the door-agreement tests below: `(spelling,
    /// same_authority)` pairs where `same_authority` is computed as
    /// `host_authority(spelling) == host_authority(domain)` — i.e. door 1's
    /// own verdict (the ratified accepted-authority set, activitypub.md
    /// § Security posture). Each actor-URL door drives its flow with a URL
    /// built on the spelling and asserts its accept/refuse MATCHES this
    /// verdict — the general invariant "every door agrees with the Host
    /// door", not a hand-picked case list, so a door loosened or tightened
    /// on its own reddens here.
    fn authority_spellings(domain: &str) -> Vec<(String, bool)> {
        let ours = host_authority(domain);
        assert!(ours.is_some(), "test domain must be a parseable authority");
        [
            domain.to_string(),          // the canonical spelling
            domain.to_ascii_uppercase(), // host case
            format!("{domain}."),        // trailing FQDN dot
            format!("{domain}:443"),     // explicit default port
            format!("{domain}:8443"),    // a DIFFERENT authority — other port
            "other.example".to_string(), // a different host entirely
        ]
        .into_iter()
        .map(|s| {
            let same = host_authority(&s) == ours;
            (s, same)
        })
        .collect()
    }

    /// Door 2 — shared-inbox `to`/`cc` username resolution
    /// (`resolve_username_from_activity`) agrees with the Host door on every
    /// authority spelling.
    #[tokio::test(flavor = "multi_thread")]
    async fn shared_inbox_username_resolution_agrees_with_the_host_door_on_spelling() {
        let state = state().await;
        let domain = state.handle_domain();
        {
            let conn = state.db.conn().await;
            db_helpers::create_account(
                &conn,
                "actor-alice",
                "alice",
                &format!("https://{domain}/ap/users/alice"),
                &[],
                "pem",
            )
            .unwrap();
        }
        for (spelling, same) in authority_spellings(&domain) {
            let activity = json!({
                "to": [format!("https://{spelling}/ap/users/alice")],
            });
            let resolved = resolve_username_from_activity(&state, &activity).await;
            assert_eq!(
                resolved.is_some(),
                same,
                "to:-target spelling {spelling:?} must resolve iff its authority is ours"
            );
            if same {
                assert_eq!(resolved.as_deref(), Some("alice"));
            }
        }
    }

    /// Door 3 — `handle_follow`'s signed-object check agrees with the Host
    /// door on every authority spelling: the follow is recorded exactly when
    /// the object's authority is ours.
    #[tokio::test(flavor = "multi_thread")]
    async fn follow_object_check_agrees_with_the_host_door_on_spelling() {
        let state = state().await;
        let domain = state.handle_domain();
        {
            let conn = state.db.conn().await;
            db_helpers::create_account(
                &conn,
                "actor-bob",
                "bob",
                &format!("https://{domain}/ap/users/bob"),
                &[],
                "pem",
            )
            .unwrap();
        }
        let remote = "https://remote.example/users/mallory";
        for (i, (spelling, same)) in authority_spellings(&domain).into_iter().enumerate() {
            let follow = json!({
                "type": "Follow",
                "id": format!("https://remote.example/activities/spelling-{i}"),
                "actor": remote,
                "object": format!("https://{spelling}/ap/users/bob"),
            });
            let code = handle_follow(&state, &follow, None).await.unwrap();
            let expected = if same {
                StatusCode::ACCEPTED
            } else {
                StatusCode::NOT_FOUND
            };
            assert_eq!(code, expected, "Follow object spelling {spelling:?}");
            let conn = state.db.conn().await;
            let has_edge = !db_helpers::list_followers(&conn, "actor-bob")
                .unwrap()
                .is_empty();
            assert_eq!(
                has_edge, same,
                "follow edge for object spelling {spelling:?}"
            );
            // Reset for the next spelling.
            db_helpers::delete_follow(&conn, "actor-bob", remote, "inbound").unwrap();
        }
    }

    /// Door 4 — `handle_undo`'s `Follow` arm agrees with the Host door on
    /// every authority spelling: the inbound edge is removed exactly when the
    /// undone Follow's object authority is ours.
    #[tokio::test(flavor = "multi_thread")]
    async fn undo_follow_agrees_with_the_host_door_on_spelling() {
        let state = state().await;
        let domain = state.handle_domain();
        {
            let conn = state.db.conn().await;
            db_helpers::create_account(
                &conn,
                "actor-bob",
                "bob",
                &format!("https://{domain}/ap/users/bob"),
                &[],
                "pem",
            )
            .unwrap();
        }
        let remote = "https://remote.example/users/mallory";
        for (spelling, same) in authority_spellings(&domain) {
            {
                let conn = state.db.conn().await;
                db_helpers::create_follow(&conn, "actor-bob", remote, "inbound", None).unwrap();
            }
            let undo = json!({
                "type": "Undo",
                "actor": remote,
                "object": {
                    "type": "Follow",
                    "actor": remote,
                    "object": format!("https://{spelling}/ap/users/bob"),
                },
            });
            assert_eq!(
                handle_undo(&state, &undo).await.unwrap(),
                StatusCode::ACCEPTED,
            );
            let conn = state.db.conn().await;
            let removed = db_helpers::list_followers(&conn, "actor-bob")
                .unwrap()
                .is_empty();
            assert_eq!(
                removed, same,
                "Undo{{Follow}} object spelling {spelling:?} must remove the edge iff its \
                 authority is ours"
            );
            // Reset for the next spelling.
            db_helpers::delete_follow(&conn, "actor-bob", remote, "inbound").unwrap();
        }
    }

    /// Door 5 — `handle_accept`'s wrapped-Follow actor check agrees with the
    /// Host door on every authority spelling: the outbound follow flips to
    /// `accepted` exactly when the wrapped actor's authority is ours. (The
    /// The finding named three doors; this fourth raw-prefix site was
    /// found by re-deriving the inventory.)
    #[tokio::test(flavor = "multi_thread")]
    async fn accept_agrees_with_the_host_door_on_spelling() {
        let state = state().await;
        let domain = state.handle_domain();
        {
            let conn = state.db.conn().await;
            db_helpers::create_account(
                &conn,
                "actor-alice",
                "alice",
                &format!("https://{domain}/ap/users/alice"),
                &[],
                "pem",
            )
            .unwrap();
        }
        let remote = "https://remote.example/users/mallory";
        for (spelling, same) in authority_spellings(&domain) {
            {
                let conn = state.db.conn().await;
                db_helpers::create_follow(&conn, "actor-alice", remote, "outbound", None).unwrap();
            }
            let accept = json!({
                "type": "Accept",
                "actor": remote,
                "object": {
                    "type": "Follow",
                    "actor": format!("https://{spelling}/ap/users/alice"),
                    "object": remote,
                },
            });
            assert_eq!(
                handle_accept(&state, &accept).await.unwrap(),
                StatusCode::ACCEPTED,
            );
            let conn = state.db.conn().await;
            let accepted = db_helpers::list_outbound_follows(&conn, "actor-alice")
                .unwrap()
                .iter()
                .any(|f| f.state == "accepted");
            assert_eq!(
                accepted, same,
                "Accept wrapped-actor spelling {spelling:?} must mark the follow accepted \
                 iff its authority is ours"
            );
            // Reset for the next spelling.
            db_helpers::delete_follow(&conn, "actor-alice", remote, "outbound").unwrap();
        }
    }

    /// **followed** actor still lands in the projection — the gates must not
    /// hide the federated posts a local account opted into.
    #[tokio::test(flavor = "multi_thread")]
    async fn public_inbound_note_from_followed_actor_is_stored() {
        let state = state().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ap_url = "https://remote.example/notes/pub1";
        let activity = create_note_activity(
            ap_url,
            "a perfectly public federated post",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
            json!(["https://remote.example/users/mallory/followers"]),
        );

        let code = handle_create(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        let mapped = db_helpers::get_post_id_for_ap_url(&conn, ap_url).unwrap();
        drop(conn);
        assert!(
            mapped.is_some(),
            "a Public inbound Note from a followed actor is stored"
        );
    }

    /// An unlisted-style Note (Public in `cc`, followers in `to`) counts as
    /// public — Mastodon's unlisted addressing. (Followed actor, so the
    /// relationship gate passes; this pins the audience semantics.)
    #[tokio::test(flavor = "multi_thread")]
    async fn unlisted_inbound_note_counts_as_public() {
        let state = state().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ap_url = "https://remote.example/notes/unlisted1";
        let activity = create_note_activity(
            ap_url,
            "an unlisted federated post",
            json!(["https://remote.example/users/mallory/followers"]),
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
        );

        let code = handle_create(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        let mapped = db_helpers::get_post_id_for_ap_url(&conn, ap_url).unwrap();
        drop(conn);
        assert!(mapped.is_some(), "Public-in-cc (unlisted) is still public");
    }

    /// The addressed arm of the relationship gate: a public Note addressed to
    /// an enabled local account (reply/mention — the per-user inbox or a
    /// shared-inbox `to`/`cc` resolution) is stored even with no follow.
    #[tokio::test(flavor = "multi_thread")]
    async fn public_note_addressed_to_enabled_account_is_stored() {
        let state = state().await;
        {
            let conn = state.db.conn().await;
            db_helpers::create_account(
                &conn,
                "actor-alice",
                "alice",
                "https://nest.example/ap/users/alice",
                &[],
                "pem",
            )
            .unwrap();
        }
        let ap_url = "https://remote.example/notes/reply1";
        let activity = create_note_activity(
            ap_url,
            "a reply to a local user",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
            json!([]),
        );

        let code = handle_create(&state, &activity, Some("alice"))
            .await
            .unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        let mapped = db_helpers::get_post_id_for_ap_url(&conn, ap_url).unwrap();
        drop(conn);
        assert!(
            mapped.is_some(),
            "a Note addressed to an enabled local account is stored"
        );
    }

    /// Seeds the enabled local account `alice`, so a Note addressed to her
    /// clears the relationship gate with no follow.
    async fn seed_enabled_alice(state: &Arc<AppState>) {
        let conn = state.db.conn().await;
        db_helpers::create_account(
            &conn,
            "actor-alice",
            "alice",
            "https://nest.example/ap/users/alice",
            &[],
            "pem",
        )
        .unwrap();
    }

    /// A `Create` whose Note claims a different author than the signer, or
    /// carries an id on another host.
    fn spoofed_create(ap_url: &str, attributed_to: &str) -> Value {
        let mut activity = create_note_activity(
            ap_url,
            "planted",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
            json!([]),
        );
        activity["object"]["attributedTo"] = json!(attributed_to);
        activity
    }

    /// A `Create` claims authorship, so the signer must BE the Note's
    /// `attributedTo` — a same-host sibling (mallory signing a Note attributed
    /// to bob) must not plant a post under bob's synthetic author.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_note_attributed_to_another_actor_is_dropped() {
        let state = state().await;
        seed_enabled_alice(&state).await;
        let ap_url = "https://remote.example/notes/planted1";
        let activity = spoofed_create(ap_url, "https://remote.example/users/bob");

        let code = handle_create(&state, &activity, Some("alice"))
            .await
            .unwrap();
        assert_eq!(code, StatusCode::ACCEPTED, "dropped, not an error");

        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .is_none(),
            "a Create whose attributedTo is not the signer must store nothing"
        );
    }

    /// The Note's id must sit on the signer's own host — else a
    /// remote could claim another server's note URL first and capture the
    /// Likes, replies and Updates that later resolve through `ap_post_map`.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_note_with_cross_origin_id_is_dropped() {
        let state = state().await;
        seed_enabled_alice(&state).await;
        let ap_url = "https://victim.example/notes/n1";
        let activity = spoofed_create(ap_url, "https://remote.example/users/mallory");

        let code = handle_create(&state, &activity, Some("alice"))
            .await
            .unwrap();
        assert_eq!(code, StatusCode::ACCEPTED, "dropped, not an error");

        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .is_none(),
            "a Create whose note id is on another host must store nothing"
        );
    }

    /// The `Update{Note}` unseen-post arm stores like a `Create`, so it
    /// takes the same authorship rule: a same-host sibling may not plant a
    /// first sighting under another actor.
    #[tokio::test(flavor = "multi_thread")]
    async fn update_note_unseen_attributed_to_another_actor_is_dropped() {
        let state = state().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ap_url = "https://remote.example/notes/planted2";
        let activity = update_note_activity(
            "https://remote.example/users/mallory",
            ap_url,
            "https://remote.example/users/bob",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
        );
        let code = handle_update(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .is_none(),
            "an unseen Update{{Note}} attributed to another actor must store nothing"
        );
    }

    /// A disabled account is not an opt-in: addressing it stores nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn note_addressed_to_disabled_account_is_dropped() {
        let state = state().await;
        {
            let conn = state.db.conn().await;
            db_helpers::create_account(
                &conn,
                "actor-alice",
                "alice",
                "https://nest.example/ap/users/alice",
                &[],
                "pem",
            )
            .unwrap();
            conn.execute("UPDATE ap_accounts SET enabled = 0", [])
                .unwrap();
        }
        let ap_url = "https://remote.example/notes/reply2";
        let activity = create_note_activity(
            ap_url,
            "a reply to a disabled account",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
            json!([]),
        );

        let code = handle_create(&state, &activity, Some("alice"))
            .await
            .unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        let mapped = db_helpers::get_post_id_for_ap_url(&conn, ap_url).unwrap();
        drop(conn);
        assert!(
            mapped.is_none(),
            "a disabled account's inbox is not an ingest opt-in"
        );
    }

    /// The Delete object-ownership gate: a signing actor on one host must not
    /// tombstone a mapped object on another host (spoofed-delete hardening —
    /// with the Create-push, local note map rows would otherwise be hideable
    /// by any authenticated remote).
    #[tokio::test(flavor = "multi_thread")]
    async fn inbound_delete_cross_origin_is_dropped() {
        let state = state().await;
        let ap_url = "https://victim.example/notes/n1";
        {
            let conn = state.db.conn().await;
            db_helpers::insert_post_map(&conn, "post1", ap_url, "actor1", None).unwrap();
        }

        let activity = json!({
            "type": "Delete",
            "actor": "https://evil.example/users/mallory",
            "object": ap_url,
        });
        let code = handle_delete(&state, &activity).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED, "dropped, not an error");

        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .is_some(),
            "the mapping must survive a cross-origin Delete"
        );
    }

    /// Companion: a same-origin Delete still tombstones the mapping.
    #[tokio::test(flavor = "multi_thread")]
    async fn inbound_delete_same_origin_tombstones() {
        let state = state().await;
        let ap_url = "https://remote.example/notes/n2";
        {
            let conn = state.db.conn().await;
            db_helpers::insert_post_map(&conn, "post2", ap_url, "actor2", None).unwrap();
        }

        let activity = json!({
            "type": "Delete",
            "actor": "https://remote.example/users/mallory",
            "object": ap_url,
        });
        let code = handle_delete(&state, &activity).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .is_none(),
            "a same-origin Delete tombstones the mapping"
        );
    }

    // ── The future bound on a Note's `published` ─────────────────────────────
    //
    // A translated Note's `published` becomes `content.created_at`, the column
    // the local feed sorts on — so a Note dated ahead of this nest's clock
    // would lead every local user's feed until real time reached it
    // (`docs/goal/ui/feed.md` § The read model). This plane refuses past the
    // bridged cushion, an hour: the drift it already allows a peer's signed
    // `Date`.

    /// `published` `offset_secs` from now, as RFC 3339.
    fn published_from_now(offset_secs: i64) -> String {
        let secs = fauna_core::data::Timestamp::now_secs() + offset_secs;
        chrono::TimeZone::timestamp_opt(&chrono::Utc, secs, 0)
            .single()
            .expect("an instant in range")
            .to_rfc3339()
    }

    /// Past the cushion, `Create` stores nothing — whether the Note claims next
    /// century or two hours from now — while a Note ten minutes fast, inside
    /// the cushion, still lands, so the refusal can be satisfied neither by
    /// breaking ingest nor by the two-minute native cushion.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_create_note_published_past_the_bridged_cushion_is_not_stored() {
        let state = state().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let cases = [
            (
                "https://remote.example/notes/2100",
                "2100-01-01T00:00:00Z".to_string(),
                false,
            ),
            (
                "https://remote.example/notes/2h",
                published_from_now(2 * 3600),
                false,
            ),
            (
                "https://remote.example/notes/10m",
                published_from_now(10 * 60),
                true,
            ),
        ];
        for (ap_url, published, stored) in cases {
            let mut activity = create_note_activity(
                ap_url,
                "a federated post",
                json!(["https://www.w3.org/ns/activitystreams#Public"]),
                json!([]),
            );
            activity["object"]["published"] = json!(published);
            let code = handle_create(&state, &activity, None).await.unwrap();
            assert_eq!(code, StatusCode::ACCEPTED, "dropped, not an error");

            let conn = state.db.conn().await;
            let mapped = db_helpers::get_post_id_for_ap_url(&conn, ap_url).unwrap();
            drop(conn);
            assert_eq!(
                mapped.is_some(),
                stored,
                "a Create{{Note}} published {published} must {}be stored",
                if stored { "" } else { "not " }
            );
        }
    }

    /// Our own pushed note: a stored local post mapped the way the
    /// `Create`-push maps it. Returns its id.
    async fn seed_our_pushed_note(state: &Arc<AppState>, note_url: &str) -> [u8; 32] {
        let post = fauna_core::data::Post {
            author: fauna_core::identity::ActorId([7u8; 32]),
            created_at: fauna_core::data::Timestamp(1_710_892_800_000_000),
            body: fauna_core::data::PostBody::Text {
                content: "our note".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
        let id: [u8; 32] = *blake3::hash(&payload).as_bytes();
        crate::segments::post::store_post(
            &state.post_segments,
            &state.db,
            &id,
            &payload,
            Some("fauna"),
        )
        .await
        .unwrap();
        let conn = state.db.conn().await;
        db_helpers::insert_post_map(&conn, &hex::encode(id), note_url, "actor-alice", None)
            .unwrap();
        id
    }

    async fn stored_references(
        state: &Arc<AppState>,
        ap_url: &str,
    ) -> Vec<fauna_core::data::Reference> {
        let conn = state.db.conn().await;
        let hex_id = db_helpers::get_post_id_for_ap_url(&conn, ap_url)
            .unwrap()
            .expect("stored");
        drop(conn);
        let mut id = [0u8; 32];
        hex::decode_to_slice(&hex_id, &mut id).unwrap();
        let body = crate::segments::post::load_post_body(&state.post_segments, &state.db, &id)
            .await
            .unwrap()
            .expect("body");
        crate::db::posts::decode_stored_post(&body)
            .unwrap()
            .references
    }

    /// `activitypub.md` § Reply and quote → *The inbound half*: a remote reply
    /// whose `inReplyTo` names our pushed note threads under it — the stored
    /// post carries `Reference::Reply` to our note's local id, and our note's
    /// `reply_count` moves 0 → 1 (once: a redelivery does not move it again).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_inbound_reply_to_our_pushed_note_threads_and_counts_once() {
        let state = state().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ours_url = "https://local.example/ap/users/alice/notes/ours";
        let ours = seed_our_pushed_note(&state, ours_url).await;
        assert_eq!(
            state
                .db
                .get_engagement_counts(&ours)
                .await
                .unwrap()
                .reply_count,
            0
        );

        let reply_url = "https://remote.example/notes/re-ours";
        let mut activity = create_note_activity(
            reply_url,
            "nice post",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
            json!([]),
        );
        activity["object"]["inReplyTo"] = json!(ours_url);
        for _ in 0..2 {
            let code = handle_create(&state, &activity, None).await.unwrap();
            assert_eq!(code, StatusCode::ACCEPTED);
        }

        assert_eq!(
            stored_references(&state, reply_url).await,
            vec![fauna_core::data::Reference::Reply {
                post_id: fauna_core::data::ContentHash::from_digest_raw(ours),
            }]
        );
        assert_eq!(
            state
                .db
                .get_engagement_counts(&ours)
                .await
                .unwrap()
                .reply_count,
            1
        );
    }

    /// An `inReplyTo` this nest never mapped resolves to nothing: the Note is
    /// stored top-level, never under a synthetic id.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_inbound_reply_to_an_unknown_object_stays_top_level() {
        let state = state().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let reply_url = "https://remote.example/notes/re-elsewhere";
        let mut activity = create_note_activity(
            reply_url,
            "replying elsewhere",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
            json!([]),
        );
        activity["object"]["inReplyTo"] = json!("https://elsewhere.example/notes/1");
        handle_create(&state, &activity, None).await.unwrap();
        assert!(stored_references(&state, reply_url).await.is_empty());
    }

    /// `Update{Note}` stores through its "unseen post" branch, so it carries the
    /// same bound — applied before any map row moves. The same Update dated in
    /// the past is the beside-control.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_update_note_published_past_the_bridged_cushion_moves_no_mapping() {
        let state = state().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ap_url = "https://remote.example/notes/u-future";
        let mut activity = update_note_activity(
            "https://remote.example/users/mallory",
            ap_url,
            "https://remote.example/users/mallory",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
        );

        activity["object"]["published"] = json!("2100-01-01T00:00:00Z");
        let code = handle_update(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED, "dropped, not an error");
        {
            let conn = state.db.conn().await;
            assert!(
                db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                    .unwrap()
                    .is_none(),
                "an Update{{Note}} published in 2100 must not create a mapping or a post"
            );
        }

        activity["object"]["published"] = json!("2026-07-22T00:00:00Z");
        handle_update(&state, &activity, None).await.unwrap();
        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .is_some(),
            "the same Update dated in the past is stored"
        );
    }

    // ── Inbound Update object-ownership + ingest gates (audit 2026-07-22) ──
    //
    // `Update` targets an object by id. Before this audit it enforced neither
    // ownership nor the Create gates, so any signature-verified remote could
    // (Note) overwrite/tombstone any mapped post — including a local user's own
    // pushed note — or slip an unsolicited/non-public note in through the
    // "unseen post" branch, and (Person) poison another actor's cached key.

    fn update_note_activity(signer: &str, note_id: &str, attributed_to: &str, to: Value) -> Value {
        json!({
            "type": "Update",
            "id": format!("{note_id}/update"),
            "actor": signer,
            "to": to,
            "object": {
                "type": "Note",
                "id": note_id,
                "attributedTo": attributed_to,
                "content": "edited content",
                "published": "2026-07-22T00:00:00Z",
                "to": to,
            }
        })
    }

    /// A remote may not Update a Note on another host: the existing mapping must
    /// survive untouched (not overwritten, not tombstoned).
    #[tokio::test(flavor = "multi_thread")]
    async fn update_note_cross_origin_is_dropped() {
        let state = state().await;
        let ap_url = "https://victim.example/notes/n1";
        {
            let conn = state.db.conn().await;
            db_helpers::insert_post_map(&conn, "post1", ap_url, "actor1", None).unwrap();
        }

        let activity = update_note_activity(
            "https://evil.example/users/mallory",
            ap_url,
            "https://victim.example/users/alice",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
        );
        let code = handle_update(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED, "dropped, not an error");

        let conn = state.db.conn().await;
        assert_eq!(
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .as_deref(),
            Some("post1"),
            "a cross-origin Update must not overwrite or tombstone the mapping"
        );
    }

    /// Even same-origin, an Update is not a backdoor around the Create gates: an
    /// unsolicited Note (no local follow, addressed to no enabled account) must
    /// create no mapping via the "unseen post" branch.
    #[tokio::test(flavor = "multi_thread")]
    async fn update_note_unsolicited_is_dropped() {
        let state = state().await;
        let ap_url = "https://remote.example/notes/u1";
        let activity = update_note_activity(
            "https://remote.example/users/mallory",
            ap_url,
            "https://remote.example/users/mallory",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
        );
        let code = handle_update(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .is_none(),
            "an unsolicited Update{{Note}} must not create a mapping (gates apply)"
        );
    }

    /// The companion happy path: a same-origin Update from a followed actor is
    /// applied — the ownership + ingest gates must not block a legitimate edit.
    #[tokio::test(flavor = "multi_thread")]
    async fn update_note_same_origin_from_followed_actor_is_applied() {
        let state = state().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ap_url = "https://remote.example/notes/edit1";
        let activity = update_note_activity(
            "https://remote.example/users/mallory",
            ap_url,
            "https://remote.example/users/mallory",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
        );
        let code = handle_update(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .is_some(),
            "a legit same-origin Update from a followed actor is applied"
        );
    }

    /// `Update{Person}` from a different host must not overwrite (poison) the
    /// victim's cached actor — that cached key verifies every activity claiming
    /// to be from the victim, so poisoning it is an impersonation vector.
    #[tokio::test(flavor = "multi_thread")]
    async fn update_person_cross_origin_does_not_poison_cache() {
        let state = state().await;
        let victim = "https://victim.example/users/alice";
        {
            let conn = state.db.conn().await;
            db_helpers::upsert_remote_actor(
                &conn,
                &RemoteActor {
                    uri: victim.to_string(),
                    inbox: "https://victim.example/users/alice/inbox".into(),
                    shared_inbox: None,
                    public_key_pem: "victim-real-key".into(),
                    preferred_username: Some("alice".into()),
                    display_name: Some("Alice".into()),
                    avatar_url: None,
                    banner_url: None,
                    summary: None,
                    last_fetched: crate::db::now_epoch_secs(),
                },
            )
            .unwrap();
        }

        let activity = json!({
            "type": "Update",
            "actor": "https://evil.example/users/mallory",
            "object": {
                "type": "Person",
                "id": victim,
                "inbox": "https://evil.example/inbox",
                "publicKey": {"publicKeyPem": "MALLORY-KEY"},
            }
        });
        let code = handle_update(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        let cached = db_helpers::get_remote_actor(&conn, victim)
            .unwrap()
            .unwrap();
        assert_eq!(
            cached.public_key_pem, "victim-real-key",
            "a cross-origin Update{{Person}} must not poison the cached key"
        );
    }

    /// `Update{Actor}` is always self-authored, so host equality is not enough:
    /// a signature-verified sibling on the SAME instance (mallory@host) must not
    /// be able to poison another actor's (alice@host) cached key. The Person arm requires exact self-identity.
    #[tokio::test(flavor = "multi_thread")]
    async fn update_person_same_host_sibling_does_not_poison_cache() {
        let state = state().await;
        let victim = "https://shared.example/users/alice";
        {
            let conn = state.db.conn().await;
            db_helpers::upsert_remote_actor(
                &conn,
                &RemoteActor {
                    uri: victim.to_string(),
                    inbox: "https://shared.example/users/alice/inbox".into(),
                    shared_inbox: None,
                    public_key_pem: "alice-real-key".into(),
                    preferred_username: Some("alice".into()),
                    display_name: Some("Alice".into()),
                    avatar_url: None,
                    banner_url: None,
                    summary: None,
                    last_fetched: crate::db::now_epoch_secs(),
                },
            )
            .unwrap();
        }

        let activity = json!({
            "type": "Update",
            "actor": "https://shared.example/users/mallory",
            "object": {
                "type": "Person",
                "id": victim,
                "inbox": "https://shared.example/users/mallory/inbox",
                "publicKey": {"publicKeyPem": "MALLORY-KEY"},
            }
        });
        let code = handle_update(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED, "dropped, not an error");

        let conn = state.db.conn().await;
        let cached = db_helpers::get_remote_actor(&conn, victim)
            .unwrap()
            .unwrap();
        assert_eq!(
            cached.public_key_pem, "alice-real-key",
            "a same-host sibling Update{{Person}} must not poison the cached key"
        );
    }

    /// The companion happy path: an actor updating ITSELF is applied — and the
    /// identity comparison is URL-normalized (host case-insensitive), so a peer
    /// serializing its host in a different case is not spuriously refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn update_person_self_identity_is_applied() {
        let state = state().await;
        let actor = "https://remote.example/users/alice";
        {
            let conn = state.db.conn().await;
            db_helpers::upsert_remote_actor(
                &conn,
                &RemoteActor {
                    uri: actor.to_string(),
                    inbox: "https://remote.example/users/alice/inbox".into(),
                    shared_inbox: None,
                    public_key_pem: "alice-old-key".into(),
                    preferred_username: Some("alice".into()),
                    display_name: Some("Alice".into()),
                    avatar_url: None,
                    banner_url: None,
                    summary: None,
                    last_fetched: crate::db::now_epoch_secs(),
                },
            )
            .unwrap();
        }

        let activity = json!({
            "type": "Update",
            // Host differs only in case from object.id — must still count as self.
            "actor": "https://REMOTE.example/users/alice",
            "object": {
                "type": "Person",
                "id": actor,
                "inbox": "https://remote.example/users/alice/inbox",
                "publicKey": {"publicKeyPem": "alice-new-key"},
            }
        });
        let code = handle_update(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);

        let conn = state.db.conn().await;
        let cached = db_helpers::get_remote_actor(&conn, actor).unwrap().unwrap();
        assert_eq!(
            cached.public_key_pem, "alice-new-key",
            "a self-authored Update{{Person}} refreshes the cached actor"
        );
    }

    // ── Inbound reaction gate + retraction ──

    fn like_activity(actor: &str, object: &str) -> Value {
        json!({
            "type": "Like",
            "id": format!("{object}/like-by-{}", actor.len()),
            "actor": actor,
            "object": object,
        })
    }

    fn announce_activity(actor: &str, object: &str, activity_id: &str) -> Value {
        json!({
            "type": "Announce",
            "id": activity_id,
            "actor": actor,
            "object": object,
        })
    }

    fn undo_activity(actor: &str, inner_type: &str, object: &str) -> Value {
        json!({
            "type": "Undo",
            "actor": actor,
            "object": { "type": inner_type, "actor": actor, "object": object },
        })
    }

    /// Rows in the map for a given ap_url, tombstoned included — the replay
    /// pins need the raw count, not the first-row lookup.
    async fn map_rows_for(state: &Arc<AppState>, ap_url: &str) -> i64 {
        let conn = state.db.conn().await;
        conn.query_row(
            "SELECT COUNT(*) FROM ap_post_map WHERE ap_url = ?1",
            [ap_url],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// Seed an enabled local account plus a mapped post it owns (the
    /// Create-push map-row shape: `actor_id` = the account's actor id).
    /// Returns the note URL.
    async fn seed_local_account_post(state: &Arc<AppState>) -> String {
        let note_url = "https://nest.example/ap/users/alice/notes/aa11".to_string();
        let conn = state.db.conn().await;
        db_helpers::create_account(
            &conn,
            "actor-alice",
            "alice",
            "https://nest.example/ap/users/alice",
            &[],
            "pem",
        )
        .unwrap();
        db_helpers::insert_post_map(
            &conn,
            &hex::encode([7u8; 32]),
            &note_url,
            "actor-alice",
            None,
        )
        .unwrap();
        note_url
    }

    /// Seed a mapped *ingested remote* note (owner = a synthetic remote
    /// actor, not any local account). Returns the note URL.
    async fn seed_ingested_remote_note(state: &Arc<AppState>) -> String {
        let note_url = "https://elsewhere.example/notes/n9".to_string();
        let conn = state.db.conn().await;
        db_helpers::insert_post_map(
            &conn,
            &hex::encode([9u8; 32]),
            &note_url,
            &hex::encode([0xEEu8; 32]),
            Some("https://elsewhere.example/users/eve"),
        )
        .unwrap();
        note_url
    }

    /// The reaction relationship gate, drop arm: a stranger's Like of an
    /// ingested remote note mints nothing — even with an enabled local
    /// account present (the drop is about relationship, not enablement).
    #[tokio::test(flavor = "multi_thread")]
    async fn stranger_like_of_ingested_note_is_dropped() {
        let state = state().await;
        {
            let conn = state.db.conn().await;
            db_helpers::create_account(
                &conn,
                "actor-alice",
                "alice",
                "https://nest.example/ap/users/alice",
                &[],
                "pem",
            )
            .unwrap();
        }
        let note_url = seed_ingested_remote_note(&state).await;
        let stranger = "https://spam.example/users/mallory";

        let code = handle_like(&state, &like_activity(stranger, &note_url), None)
            .await
            .unwrap();
        assert_eq!(code, StatusCode::ACCEPTED, "drop, don't error (no retry)");
        assert_eq!(
            map_rows_for(&state, &format!("like:{stranger}:{note_url}")).await,
            0,
            "a stranger's Like of an ingested remote note must not mint a synthetic upvote"
        );
    }

    /// Follow arm: a followed actor's Like of an ingested note is stored —
    /// followed actors' reactions are subscribed content.
    #[tokio::test(flavor = "multi_thread")]
    async fn like_from_followed_actor_is_stored() {
        let state = state().await;
        let reactor = "https://remote.example/users/mallory";
        seed_follow_of(&state, reactor).await;
        let note_url = seed_ingested_remote_note(&state).await;

        let code = handle_like(&state, &like_activity(reactor, &note_url), None)
            .await
            .unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);
        assert_eq!(
            map_rows_for(&state, &format!("like:{reactor}:{note_url}")).await,
            1,
            "a followed actor's Like is stored"
        );
    }

    /// Ownership arm (the ruling's Mastodon-parity half): ANY verified remote
    /// actor's Like of an enabled local account's own post is stored —
    /// engagement on your own federated posts is what enabling federation
    /// subscribes you to. (This deliberately refutes the finding's proposed
    /// "unfollowed actor → no row" pin for local posts.)
    #[tokio::test(flavor = "multi_thread")]
    async fn stranger_like_of_local_account_post_is_stored() {
        let state = state().await;
        let note_url = seed_local_account_post(&state).await;
        let stranger = "https://remote.example/users/somefan";

        let code = handle_like(&state, &like_activity(stranger, &note_url), None)
            .await
            .unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);
        assert_eq!(
            map_rows_for(&state, &format!("like:{stranger}:{note_url}")).await,
            1,
            "a Like on a local account's own post is stored (ownership arm)"
        );
    }

    /// A disabled account's posts are not an opt-in: reactions on them drop.
    #[tokio::test(flavor = "multi_thread")]
    async fn like_of_disabled_account_post_is_dropped() {
        let state = state().await;
        let note_url = seed_local_account_post(&state).await;
        {
            let conn = state.db.conn().await;
            conn.execute("UPDATE ap_accounts SET enabled = 0", [])
                .unwrap();
        }
        let stranger = "https://remote.example/users/somefan";

        let code = handle_like(&state, &like_activity(stranger, &note_url), None)
            .await
            .unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);
        assert_eq!(
            map_rows_for(&state, &format!("like:{stranger}:{note_url}")).await,
            0,
            "a disabled account's post is not a reaction opt-in"
        );
    }

    /// Replay idempotency: the same Like delivered twice mints exactly one
    /// synthetic row. (Pre-ruling, the synthetic post id embedded its mint
    /// time, so every replay minted a fresh `content` + map row — unbounded
    /// amplification from one actor against one target.)
    #[tokio::test(flavor = "multi_thread")]
    async fn replayed_like_mints_no_second_synthetic_row() {
        let state = state().await;
        let note_url = seed_local_account_post(&state).await;
        let reactor = "https://remote.example/users/somefan";
        let activity = like_activity(reactor, &note_url);

        handle_like(&state, &activity, None).await.unwrap();
        handle_like(&state, &activity, None).await.unwrap();
        assert_eq!(
            map_rows_for(&state, &format!("like:{reactor}:{note_url}")).await,
            1,
            "a replayed Like must not mint a second synthetic row"
        );
    }

    /// Replay idempotency holds for Announce even when the attacker mints a
    /// fresh activity id per replay — the dedupe key is (actor, object),
    /// never the activity id.
    #[tokio::test(flavor = "multi_thread")]
    async fn replayed_announce_mints_no_second_synthetic_row() {
        let state = state().await;
        let note_url = seed_local_account_post(&state).await;
        let reactor = "https://remote.example/users/somefan";

        let a1 = announce_activity(reactor, &note_url, "https://remote.example/act/1");
        let a2 = announce_activity(reactor, &note_url, "https://remote.example/act/2");
        handle_announce(&state, &a1, None).await.unwrap();
        handle_announce(&state, &a2, None).await.unwrap();
        assert_eq!(
            map_rows_for(&state, &format!("announce:{reactor}:{note_url}")).await,
            1,
            "a re-sent Announce with a fresh activity id must not mint a second row"
        );
    }

    /// Announce drop arm (companion to the Like one — the shared mint path
    /// must gate both verbs identically).
    #[tokio::test(flavor = "multi_thread")]
    async fn stranger_announce_of_ingested_note_is_dropped() {
        let state = state().await;
        let note_url = seed_ingested_remote_note(&state).await;
        let stranger = "https://spam.example/users/mallory";

        let a = announce_activity(stranger, &note_url, "https://spam.example/act/1");
        let code = handle_announce(&state, &a, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);
        assert_eq!(
            map_rows_for(&state, &format!("announce:{stranger}:{note_url}")).await,
            0,
            "a stranger's Announce of an ingested remote note must not mint a synthetic repost"
        );
    }

    /// Undo{Like} retracts: the synthetic upvote's map row is tombstoned.
    /// (Pre-ruling this was a silent no-op — the Undo arm hashed the map key
    /// and passed the digest as a fauna_post_id, matching nothing, so an
    /// un-like from a remote server never retracted the upvote.)
    #[tokio::test(flavor = "multi_thread")]
    async fn undo_like_tombstones_the_synthetic_upvote() {
        let state = state().await;
        let note_url = seed_local_account_post(&state).await;
        let reactor = "https://remote.example/users/somefan";

        handle_like(&state, &like_activity(reactor, &note_url), None)
            .await
            .unwrap();
        let key = format!("like:{reactor}:{note_url}");
        {
            let conn = state.db.conn().await;
            assert!(
                db_helpers::get_post_id_for_ap_url(&conn, &key)
                    .unwrap()
                    .is_some(),
                "precondition: the Like minted"
            );
        }

        handle_undo(&state, &undo_activity(reactor, "Like", &note_url))
            .await
            .unwrap();
        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, &key)
                .unwrap()
                .is_none(),
            "Undo{{Like}} must tombstone the synthetic upvote's map row"
        );
    }

    /// Undo{Announce} (Mastodon's unboost) retracts the synthetic repost —
    /// previously an unhandled inner type, so an unboost never retracted.
    #[tokio::test(flavor = "multi_thread")]
    async fn undo_announce_tombstones_the_synthetic_repost() {
        let state = state().await;
        let note_url = seed_local_account_post(&state).await;
        let reactor = "https://remote.example/users/somefan";

        let a = announce_activity(reactor, &note_url, "https://remote.example/act/9");
        handle_announce(&state, &a, None).await.unwrap();
        let key = format!("announce:{reactor}:{note_url}");
        assert_eq!(
            map_rows_for(&state, &key).await,
            1,
            "precondition: the Announce minted under the stable key"
        );

        handle_undo(&state, &undo_activity(reactor, "Announce", &note_url))
            .await
            .unwrap();
        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, &key)
                .unwrap()
                .is_none(),
            "Undo{{Announce}} must tombstone the synthetic repost's map row"
        );
    }

    /// A tombstoned reaction is not a duplicate: un-like then re-like mints
    /// again (the dedupe key must ignore tombstoned rows).
    #[tokio::test(flavor = "multi_thread")]
    async fn undo_then_relike_mints_again() {
        let state = state().await;
        let note_url = seed_local_account_post(&state).await;
        let reactor = "https://remote.example/users/somefan";
        let activity = like_activity(reactor, &note_url);
        let key = format!("like:{reactor}:{note_url}");

        handle_like(&state, &activity, None).await.unwrap();
        handle_undo(&state, &undo_activity(reactor, "Like", &note_url))
            .await
            .unwrap();
        handle_like(&state, &activity, None).await.unwrap();

        let conn = state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_ap_url(&conn, &key)
                .unwrap()
                .is_some(),
            "a re-Like after an Undo mints a fresh synthetic upvote"
        );
    }

    /// Retraction withdraws the synthetic post's CONTENT projection, not just
    /// its map row — so un-react → re-react (which deliberately mints again,
    /// pinned by `undo_then_relike_mints_again`) cannot accumulate one orphaned
    /// synthetic `content`/`content_meta` row per cycle.
    #[tokio::test(flavor = "multi_thread")]
    async fn undo_withdraws_the_synthetic_content_projection() {
        let state = state().await;
        let note_url = seed_local_account_post(&state).await;
        let reactor = "https://remote.example/users/somefan";
        let activity = like_activity(reactor, &note_url);
        let key = format!("like:{reactor}:{note_url}");

        // Cycle 1: a Like mints a synthetic content post; capture its id.
        handle_like(&state, &activity, None).await.unwrap();
        let id1_hex = {
            let conn = state.db.conn().await;
            db_helpers::get_post_id_for_ap_url(&conn, &key)
                .unwrap()
                .unwrap()
        };
        let id1 = fauna_core::hex32::decode(&id1_hex).unwrap();
        assert!(
            state.db.post_exists(&id1).await.unwrap(),
            "precondition: the Like minted a synthetic content row"
        );

        // The Undo must WITHDRAW that content row, not merely tombstone the map.
        handle_undo(&state, &undo_activity(reactor, "Like", &note_url))
            .await
            .unwrap();
        assert!(
            !state.db.post_exists(&id1).await.unwrap(),
            "Undo{{Like}} must withdraw the synthetic content projection, not orphan it"
        );

        // Cycle 2: the re-Like mints a fresh row; its Undo must likewise
        // withdraw it — the corpus never accumulates across cycles.
        handle_like(&state, &activity, None).await.unwrap();
        let id2_hex = {
            let conn = state.db.conn().await;
            db_helpers::get_post_id_for_ap_url(&conn, &key)
                .unwrap()
                .unwrap()
        };
        let id2 = fauna_core::hex32::decode(&id2_hex).unwrap();
        assert!(state.db.post_exists(&id2).await.unwrap());
        handle_undo(&state, &undo_activity(reactor, "Like", &note_url))
            .await
            .unwrap();
        assert!(
            !state.db.post_exists(&id1).await.unwrap()
                && !state.db.post_exists(&id2).await.unwrap(),
            "no synthetic reaction content survives a full react→unreact cycle"
        );
    }

    // ── The Search corpus: the ActivityPub transit point ─────────────────────
    //
    // `content-index.md` § Bridge content in the Search corpus, remainder (2c).
    // The AP inbox is one of the section's named transit points; until this set
    // existed, an ingested Note was a `post/*` projection row with **no** FTS
    // row at all (`put_post_with_source_index_only` — bridge-ingested posts are
    // deliberately not FTS-indexed), so `fauna.search.query` never returned a
    // federated post even though the two per-bridge controls were already
    // rendered for the AP bridge. These pin the hook and, more importantly, its
    // removal arm.

    use crate::db::bridge_search::SearchPolicy;
    use fauna_protocol::bridge_search_policy::DEFAULT_SEARCH_POST_LIMIT;

    const SEARCH_ACTOR: [u8; 32] = [7u8; 32];

    /// The AP test state plus a real user row — the union rule counts actors,
    /// so a nest with none has no effective policy and indexes nothing.
    async fn state_with_user() -> Arc<AppState> {
        let state = state().await;
        state
            .db
            .create_user(&SEARCH_ACTOR, "free", "test")
            .await
            .unwrap();
        state
    }

    /// Hits `fauna.search.query` serves, restricted to the ActivityPub bridge
    /// corpus — read through the production search path, not the raw table.
    async fn ap_corpus_hits(state: &Arc<AppState>, q: &str) -> usize {
        state
            .db
            .search_with_scoping(
                q,
                &SEARCH_ACTOR,
                Some("bridge.activitypub"),
                None,
                None,
                50,
                0,
            )
            .await
            .expect("search")
            .len()
    }

    /// A public Note from a followed actor is searchable — the works-out-of-the
    /// -box default (show-in-search ON, nobody configured anything), through
    /// both a bridge-filtered query and the default unfiltered one.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_public_inbound_note_enters_the_search_corpus() {
        let state = state_with_user().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let activity = create_note_activity(
            "https://remote.example/notes/s1",
            "a federated rhubarb crumble recipe",
            json!(["https://www.w3.org/ns/activitystreams#Public"]),
            json!([]),
        );

        handle_create(&state, &activity, None).await.unwrap();

        assert_eq!(ap_corpus_hits(&state, "rhubarb").await, 1);
        let unfiltered = state
            .db
            .search_with_scoping("rhubarb", &SEARCH_ACTOR, None, None, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(
            unfiltered.len(),
            1,
            "the row must reach the corpus a user actually searches, not only a filtered view"
        );
    }

    /// The removal arm, and the reason it is a trigger: a remote author's
    /// `Delete` must take the row out of the corpus. `handle_delete` tombstones
    /// the `ap_post_map` row, so the trigger keyed on that transition is what
    /// keeps the corpus in lockstep for *every* path that ever tombstones —
    /// today's inbound Delete, and any future one.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_inbound_delete_removes_the_indexed_row() {
        let state = state_with_user().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ap_url = "https://remote.example/notes/s2";
        handle_create(
            &state,
            &create_note_activity(
                ap_url,
                "a federated marmalade recipe",
                json!(["https://www.w3.org/ns/activitystreams#Public"]),
                json!([]),
            ),
            None,
        )
        .await
        .unwrap();
        assert_eq!(ap_corpus_hits(&state, "marmalade").await, 1, "precondition");

        handle_delete(
            &state,
            &json!({
                "type": "Delete",
                "actor": "https://remote.example/users/mallory",
                "object": ap_url,
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            ap_corpus_hits(&state, "marmalade").await,
            0,
            "a remote Delete must not leave the note searchable"
        );
    }

    /// A remote author's `Delete` must withdraw the **translated post**, not
    /// merely tombstone the map row.
    ///
    /// `retract_reaction`'s own doc states the rule for the sibling inbound
    /// retraction — "tombstoning only the map row would orphan the synthetic
    /// `content`/`content_meta`/segment rows" — and the `Undo` path tears all
    /// three down. `handle_delete` never did, so a note its author deleted
    /// upstream stayed in the Fauna feed forever: `ap_post_map.tombstoned` is
    /// read only by the push/interact URL lookups, never by serving.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_inbound_delete_withdraws_the_translated_post() {
        let state = state_with_user().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ap_url = "https://remote.example/notes/s6";
        handle_create(
            &state,
            &create_note_activity(
                ap_url,
                "a federated note its author later retracts",
                json!(["https://www.w3.org/ns/activitystreams#Public"]),
                json!([]),
            ),
            None,
        )
        .await
        .unwrap();

        let post_id_hex = {
            let conn = state.db.conn().await;
            db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .expect("precondition: the Note was ingested")
        };
        let digest = fauna_core::hex32::decode(&post_id_hex).unwrap();
        assert!(
            state.db.post_exists(&digest).await.unwrap(),
            "precondition: the ingested Note has a content projection"
        );

        handle_delete(
            &state,
            &json!({
                "type": "Delete",
                "actor": "https://remote.example/users/mallory",
                "object": ap_url,
            }),
        )
        .await
        .unwrap();

        assert!(
            !state.db.post_exists(&digest).await.unwrap(),
            "a remote Delete must withdraw the translated post, not orphan it in the feed"
        );
    }

    /// The withdrawal's ownership guard: a `Delete` naming a **local account's
    /// own** pushed note must never destroy it, even when the same-origin gate
    /// passes — which it does exactly when the signer is on our own host, the
    /// residual `activitypub.md` § Security posture knowingly accepts for
    /// shared instances. That residual may cost a hidden map row; it must not
    /// cost the user their post. `fauna.posts.delete` is the only verb that
    /// destroys a local post, and it checks the author three ways.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_inbound_delete_never_destroys_a_local_accounts_own_post() {
        let state = state_with_user().await;
        let note_url = seed_local_account_post(&state).await;
        let digest = [7u8; 32];
        {
            // Give the mapped local note a real projection to destroy.
            let post = fauna_core::data::Post {
                author: fauna_core::identity::ActorId([1u8; 32]),
                created_at: fauna_core::data::Timestamp(1_700_000_000_000_000),
                body: fauna_core::data::PostBody::Text {
                    content: "a local user's own federated post".into(),
                    facets: vec![],
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin: None,
            };
            let bytes = fauna_core::encoding::canonical_encode(&post).unwrap();
            state.db.put_post(&digest, &bytes, None).await.unwrap();
        }
        assert!(
            state.db.post_exists(&digest).await.unwrap(),
            "precondition: the local note has a projection"
        );

        // Same-origin by construction: signer and object share our host.
        handle_delete(
            &state,
            &json!({
                "type": "Delete",
                "actor": "https://nest.example/ap/users/mallory",
                "object": note_url,
            }),
        )
        .await
        .unwrap();

        assert!(
            state.db.post_exists(&digest).await.unwrap(),
            "a local account's own post survives an inbound Delete"
        );
    }

    /// An `Update{Note}` re-indexes rather than double-surfacing: the natural id
    /// is the AP object id, so an edit lands on the same key.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_updated_note_leaves_exactly_one_indexed_row() {
        let state = state_with_user().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ap_url = "https://remote.example/notes/s3";
        handle_create(
            &state,
            &create_note_activity(
                ap_url,
                "a federated quince recipe",
                json!(["https://www.w3.org/ns/activitystreams#Public"]),
                json!([]),
            ),
            None,
        )
        .await
        .unwrap();

        handle_update(
            &state,
            &json!({
                "type": "Update",
                "actor": "https://remote.example/users/mallory",
                "to": ["https://www.w3.org/ns/activitystreams#Public"],
                "object": {
                    "type": "Note",
                    "id": ap_url,
                    "attributedTo": "https://remote.example/users/mallory",
                    "content": "a federated quince and damson recipe",
                    "to": ["https://www.w3.org/ns/activitystreams#Public"],
                    "cc": [],
                },
            }),
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            ap_corpus_hits(&state, "quince").await,
            1,
            "an edit replaces its corpus row; it must not double-surface"
        );
        assert_eq!(
            ap_corpus_hits(&state, "damson").await,
            1,
            "and the corpus must carry the edited body, not the superseded one"
        );
    }

    /// The feed's text filters reach an ingested Note through its corpus row
    /// (`feed.md` § The read model → *The list-card preview* → Corollary): the
    /// map row carries the rested post's id, and an edit re-links it to the
    /// edited post, so a keyword feed lists the current version only.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_keyword_feed_lists_an_ingested_note_and_follows_its_edit() {
        use fauna_core::scoring::{FilterCombination, FilterRule};
        async fn matching(state: &Arc<AppState>, term: &str) -> Vec<Vec<u8>> {
            state
                .db
                .query_feed(
                    &[FilterRule::BodyContains {
                        terms: vec![term.into()],
                    }],
                    FilterCombination::All,
                    &[],
                    None,
                    50,
                )
                .await
                .unwrap()
                .into_iter()
                .map(|r| r.post_id)
                .collect()
        }
        async fn post_of(state: &Arc<AppState>, ap_url: &str) -> Vec<u8> {
            let conn = state.db.conn().await;
            let hex_id = db_helpers::get_post_id_for_ap_url(&conn, ap_url)
                .unwrap()
                .expect("mapped");
            hex::decode(hex_id).unwrap()
        }

        let state = state_with_user().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        let ap_url = "https://remote.example/notes/kw1";
        handle_create(
            &state,
            &create_note_activity(
                ap_url,
                "a federated medlar recipe",
                json!(["https://www.w3.org/ns/activitystreams#Public"]),
                json!([]),
            ),
            None,
        )
        .await
        .unwrap();
        let original = post_of(&state, ap_url).await;
        assert_eq!(matching(&state, "medlar").await, vec![original.clone()]);

        handle_update(
            &state,
            &json!({
                "type": "Update",
                "actor": "https://remote.example/users/mallory",
                "to": ["https://www.w3.org/ns/activitystreams#Public"],
                "object": {
                    "type": "Note",
                    "id": ap_url,
                    "attributedTo": "https://remote.example/users/mallory",
                    "content": "a federated sloe recipe",
                    "to": ["https://www.w3.org/ns/activitystreams#Public"],
                    "cc": [],
                },
            }),
            None,
        )
        .await
        .unwrap();
        let edited = post_of(&state, ap_url).await;
        assert_ne!(edited, original, "an edit rests a new post");
        assert_eq!(
            matching(&state, "sloe").await,
            vec![edited],
            "the corpus row now links the edited post"
        );
        assert!(
            matching(&state, "medlar").await.is_empty(),
            "the superseded body matches nothing"
        );
    }

    /// The no-double-surfacing rule, structurally: only **inbound, foreign-
    /// authored** content indexes. A local note pushed outbound writes an
    /// `ap_post_map` row too (the push witness) — its body is already in
    /// `content_fts` as the Fauna post, so it must never gain a bridge row.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pushed_local_note_never_enters_the_corpus() {
        let state = state_with_user().await;
        let note_url = seed_local_account_post(&state).await;

        // A positive control in the same test, deliberately: the bare absence
        // assertion below also passes when indexing is off *entirely*, so on
        // its own it cannot tell "correctly excluded" from "nothing works".
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        handle_create(
            &state,
            &create_note_activity(
                "https://remote.example/notes/s5",
                "a federated persimmon recipe",
                json!(["https://www.w3.org/ns/activitystreams#Public"]),
                json!([]),
            ),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            ap_corpus_hits(&state, "persimmon").await,
            1,
            "control: inbound foreign content DOES index"
        );

        let conn = state.db.conn().await;
        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_index_map \
                 WHERE content_type = 'bridge.activitypub' AND natural_id = ?1",
                [&note_url],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(
            indexed, 0,
            "an outbound push witness is not inbound foreign content"
        );
    }

    /// Toggling the bridge off purges what was already indexed — the same
    /// setting-write arm nostr uses, reached here for AP.
    #[tokio::test(flavor = "multi_thread")]
    async fn toggling_the_bridge_off_purges_the_activitypub_corpus() {
        let state = state_with_user().await;
        seed_follow_of(&state, "https://remote.example/users/mallory").await;
        handle_create(
            &state,
            &create_note_activity(
                "https://remote.example/notes/s4",
                "a federated tamarind recipe",
                json!(["https://www.w3.org/ns/activitystreams#Public"]),
                json!([]),
            ),
            None,
        )
        .await
        .unwrap();
        assert_eq!(ap_corpus_hits(&state, "tamarind").await, 1, "precondition");

        {
            let conn = state.db.conn().await;
            crate::db::bridge_search::set_policy(
                &conn,
                &hex::encode(SEARCH_ACTOR),
                "activitypub",
                SearchPolicy {
                    show_in_search: false,
                    post_limit: DEFAULT_SEARCH_POST_LIMIT,
                },
            )
            .unwrap();
            crate::db::bridge_search::reconcile_bridge_corpus(&conn, "activitypub").unwrap();
        }

        assert_eq!(
            ap_corpus_hits(&state, "tamarind").await,
            0,
            "the last actor turning it off purges the bridge's rows"
        );
    }
}
