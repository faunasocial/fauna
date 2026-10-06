//! The third events door — the **webhook** (`transport.md` § Push events →
//! *Third-party event doors*): a remote server whose consented document
//! declares `events_uri` ([`fauna_protocol::kind_manifest::validate_events_uri`]
//! rules the URL) is POSTed a signed, payload-free notification whenever a
//! scope its principal reaches changes, and comes to fetch what moved through
//! the poll (`GET /api/v1/events`) or the record door. Delivery is
//! best-effort; the poll's cursor is the correctness backstop — the plane's
//! own nudge-plus-walk rule, as for the push.
//!
//! # The notification is a security event token
//!
//! The body is a **Security Event Token** (RFC 8417): a JWT whose header
//! carries `typ: secevent+jwt` and whose `events` claim names the one event
//! type this nest emits, [`SCOPE_CHANGED_EVENT`], with the cursor as its only
//! member — no scope, no key, no content ([`EventClaims`]). It is delivered
//! by **push** (RFC 8935): one `POST` to `events_uri` with
//! `Content-Type: application/secevent+jwt`, any `2xx` meaning accepted.
//!
//! **Which key signs it — the issuer's ES256 key (ruled 2026-10-05,
//! `key-material-hierarchy.md` § Audience: deployment infrastructure →
//! *Issuer signing key* → *What it signs*).** The receiver is, by
//! construction, an OAuth client of this nest's issuer: the only trust anchor
//! it holds for this nest is the issuer's discovery document and the key set
//! at `/oauth/jwks`, which it already uses to verify ID tokens, and whose
//! rotation is already built. The deployment Ed25519 key would need a second
//! anchor published somewhere a remote server could find, and a second
//! rotation story, for no gain. The `typ` inside the signed bytes separates
//! the event token from the two token classes the same key signs
//! (`at+jwt`, `JWT`) exactly as those two are separated from each other —
//! the JWT-family form of rule #8's tag — and every verifier of that key
//! pins `typ`, so an event token presents as nothing else
//! ([`crate::oauth_as_token::verify_access_token`] refuses it; pinned by this
//! module's tests).
//!
//! # Delivery
//!
//! - **The walk reads principal rows, never the session registry**
//!   ([`crate::db::CacheDb::list_event_webhooks`]): a remote server with no
//!   live session is exactly who the webhook is for. The filter is the
//!   push's live reach — [`fauna_scope::event_reaches`] over the row's
//!   granted scopes, the account's authority, its external-apps switch
//!   ([`targets`]).
//! - **Off the put path, coalesced per principal.** [`notify`] spawns the
//!   walk; one delivery runs per principal at a time, and a change landing
//!   while it runs is owed one more delivery when it ends
//!   ([`crate::ws::WsState::webhook_begin`] / `webhook_end`) — a burst of
//!   writes costs the publisher's server at most two notifications, each
//!   carrying the cursor current when it was minted.
//! - **The dial is the guarded one** ([`crate::oauth_as_client::dial`]: the
//!   SSRF guard's two seats, no redirects, a pinned resolution; under
//!   `test-hooks` a mapped host dials its loopback test server). One request
//!   is bounded by [`REQUEST_TIMEOUT`]; a network error or a server-side
//!   status is retried after [`RETRY_BACKOFF`]'s waits, a client-side status
//!   is final. After the last attempt the delivery is dropped — the cursor
//!   catches it up at the server's next poll.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use fauna_bridge_atproto::fauna_scope;
use serde::{Deserialize, Serialize};

use crate::db::CacheDb;
use crate::db::third_party_principals::EventWebhook;
use crate::oauth_as_token::{b64, grant_subject, mint_jti, sign_es256};
use crate::oauth_issuer_key::IssuerSigner;
use crate::routes::AppState;
use crate::ws::WsState;

/// The JWT `typ` of a security event token (RFC 8417 § 2.3).
pub const EVENT_TOKEN_TYP: &str = "secevent+jwt";
/// The media type the push carries it under (RFC 8935 § 2).
pub const EVENT_TOKEN_MEDIA_TYPE: &str = "application/secevent+jwt";
/// The one event type this nest emits: a scope the principal reaches moved.
pub const SCOPE_CHANGED_EVENT: &str = "urn:fauna:event-type:scope-changed";

/// Bounds one `POST`.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The waits before the second, third and fourth attempt. A server still not
/// answering after them is caught up by its next poll.
pub const RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// The claim set of one notification — everything a receiver needs to decide
/// whether to fetch, and nothing about what changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventClaims {
    /// This nest's issuer identifier — the `iss` of every token it mints.
    pub iss: String,
    /// The principal's `client_id`: the document that declared `events_uri`.
    pub aud: String,
    /// The subject the principal's tokens carry for this account
    /// ([`grant_subject`]), so the server maps the event to the user it
    /// already knows.
    pub sub: String,
    pub iat: i64,
    pub jti: String,
    /// RFC 8417 § 2.2: event type → its payload. One entry,
    /// [`SCOPE_CHANGED_EVENT`].
    pub events: BTreeMap<String, ScopeChanged>,
}

/// [`SCOPE_CHANGED_EVENT`]'s payload: the cursor to poll from — the newest
/// nest-log `seq` among the scopes the principal reaches, so a server whose
/// own cursor is already there knows it has nothing to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeChanged {
    pub cursor: i64,
}

impl EventClaims {
    /// One scope-changed notification, with a fresh `jti`.
    ///
    /// # Errors
    /// Only the entropy source failing.
    pub fn scope_changed(
        iss: String,
        aud: String,
        sub: String,
        cursor: i64,
        now: i64,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            iss,
            aud,
            sub,
            iat: now,
            jti: b64(&mint_jti()?),
            events: BTreeMap::from([(SCOPE_CHANGED_EVENT.to_string(), ScopeChanged { cursor })]),
        })
    }
}

/// Sign a notification under the issuer key as a security event token — the
/// same signer as the two token classes, told apart by [`EVENT_TOKEN_TYP`]
/// inside the signed bytes.
///
/// # Errors
/// As [`crate::oauth_as_token::mint_access_token`].
pub fn mint_event_token(signer: &IssuerSigner, claims: &EventClaims) -> anyhow::Result<String> {
    sign_es256(signer, EVENT_TOKEN_TYP, claims)
}

/// One webhook the walk selected, with the reach it was selected under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub hook: EventWebhook,
    /// The row's granted scopes, split — what the cursor is computed over.
    pub scopes: Vec<String>,
}

/// Which of `account`'s webhooks a change on `scope` reaches: the rows that
/// declare one and whose granted scopes admit the frame
/// ([`fauna_scope::event_reaches`]), on an account with authority whose
/// external-apps switch is ON — the push's own filter, read at delivery.
///
/// # Errors
/// A DB read failing.
pub async fn targets(db: &CacheDb, account: &[u8; 32], scope: &str) -> anyhow::Result<Vec<Target>> {
    let reached: Vec<Target> = db
        .list_event_webhooks(account)
        .await?
        .into_iter()
        .filter_map(|hook| {
            let scopes: Vec<String> = hook
                .granted_scopes
                .split_whitespace()
                .map(str::to_string)
                .collect();
            fauna_scope::event_reaches(&scopes, scope).then_some(Target { hook, scopes })
        })
        .collect();
    if reached.is_empty() {
        return Ok(reached);
    }
    // The account half, once, only when a row is reached at all.
    match db.actor_authority(&account[..]).await? {
        Some(authority) if !authority.is_revoked_at(crate::db::now_epoch_secs()) => {}
        _ => return Ok(Vec::new()),
    }
    if !db.get_atproto_external_apps_enabled(account).await? {
        return Ok(Vec::new());
    }
    Ok(reached)
}

/// What a delivery needs of the nest, cloned off the put path so the task
/// owns nothing of the request that triggered it.
#[derive(Clone)]
struct Nest {
    db: Arc<CacheDb>,
    ws: Arc<WsState>,
    /// The deployment seed the issuer key set is sealed under.
    seed: [u8; 32],
    issuer: String,
}

/// A scope-tagged change landed on `account`'s `scope`: notify every webhook
/// it reaches, off the caller's path. Returns at once.
///
/// A nest with no deployment signing key or no claimed domain has no issuer,
/// so no remote server could have consented to it: nothing to notify.
pub(crate) fn notify(state: &AppState, account: [u8; 32], scope: String) {
    let Some(seed) = state.nest_signing_key.as_ref().map(|k| k.to_bytes()) else {
        return;
    };
    let Some(issuer) = crate::oauth_issuer_routes::issuer(state) else {
        return;
    };
    let nest = Nest {
        db: Arc::clone(&state.db),
        ws: Arc::clone(&state.ws),
        seed,
        issuer,
    };
    // The task carries the deployment seed, so it rides the serve generation: a
    // seed rotation tears it (and the deliveries it owns) down rather than
    // letting it keep signing with the superseded key.
    state.spawn_scoped(async move {
        match targets(&nest.db, &account, &scope).await {
            Ok(reached) => {
                // A JoinSet aborts its deliveries when this scoped task is cancelled.
                let mut deliveries = tokio::task::JoinSet::new();
                for target in reached {
                    if nest.ws.webhook_begin(&target.hook.principal_id) {
                        deliveries.spawn(deliver(nest.clone(), account, target));
                    }
                }
                while deliveries.join_next().await.is_some() {}
            }
            Err(e) => tracing::warn!(
                error = %format!("{e:#}"),
                "events webhook: walk failed; the poll's cursor is the backstop"
            ),
        }
    });
}

/// Deliver to one principal until no change is owed — the coalescing loop.
async fn deliver(nest: Nest, account: [u8; 32], target: Target) {
    loop {
        match notification(&nest, &account, &target).await {
            Ok(token) => post_with_retries(&target.hook, &token).await,
            Err(e) => tracing::warn!(
                client_id = %target.hook.client_id,
                error = %format!("{e:#}"),
                "events webhook: could not mint the notification"
            ),
        }
        if !nest.ws.webhook_end(&target.hook.principal_id) {
            return;
        }
    }
}

/// Mint one signed notification for `target`, current as of now.
async fn notification(nest: &Nest, account: &[u8; 32], target: &Target) -> anyhow::Result<String> {
    // A lookup, never a mint — the boot step seated the key.
    let db = Arc::clone(&nest.db);
    let seed = nest.seed;
    let signer = tokio::task::spawn_blocking(move || {
        let conn = db.conn_blocking();
        crate::oauth_issuer_key::active_signer(&conn, &seed)
    })
    .await??;
    let login_did = nest
        .db
        .get_atproto_identity(account)
        .await?
        .filter(|r| r.status == "active")
        .and_then(|r| r.did);
    let sub = grant_subject(&target.scopes, login_did.as_deref(), account)
        .unwrap_or_else(|| hex::encode(account));
    let cursor = nest
        .db
        .ext_scope_heads_since(account, 0, u32::MAX)
        .await?
        .into_iter()
        .filter(|(scope, _)| fauna_scope::event_reaches(&target.scopes, scope))
        .map(|(_, head)| head)
        .max()
        .unwrap_or(0);
    let claims = EventClaims::scope_changed(
        nest.issuer.clone(),
        target.hook.client_id.clone(),
        sub,
        cursor,
        crate::db::now_epoch_secs(),
    )?;
    mint_event_token(&signer, &claims)
}

/// How one `POST` ended.
enum Outcome {
    Delivered,
    /// The server answered with a client-side status, or the URL did not
    /// parse: trying again changes nothing.
    Final(String),
    /// No answer, a server-side status, or a transient refusal.
    Retry(String),
}

async fn post_with_retries(hook: &EventWebhook, token: &str) {
    let waits = std::iter::once(None).chain(RETRY_BACKOFF.iter().copied().map(Some));
    for (attempt, wait) in waits.enumerate() {
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        match post_once(&hook.events_uri, token).await {
            Outcome::Delivered => return,
            Outcome::Final(why) => {
                tracing::warn!(
                    client_id = %hook.client_id,
                    attempt,
                    %why,
                    "events webhook: refused; not retried"
                );
                return;
            }
            Outcome::Retry(why) => tracing::debug!(
                client_id = %hook.client_id,
                attempt,
                %why,
                "events webhook: attempt failed"
            ),
        }
    }
    tracing::warn!(
        client_id = %hook.client_id,
        attempts = RETRY_BACKOFF.len() + 1,
        "events webhook: giving up; the poll's cursor is the backstop"
    );
}

async fn post_once(events_uri: &str, token: &str) -> Outcome {
    use crate::oauth_as_client::MetadataFetchError;
    let (client, url) = match crate::oauth_as_client::dial(events_uri, REQUEST_TIMEOUT).await {
        Ok(dialled) => dialled,
        Err(MetadataFetchError::InvalidUrl) => return Outcome::Final("url did not parse".into()),
        Err(e) => return Outcome::Retry(e.to_string()),
    };
    match client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, EVENT_TOKEN_MEDIA_TYPE)
        .body(token.to_string())
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => Outcome::Delivered,
        Ok(resp)
            if resp.status().is_client_error()
                && resp.status() != reqwest::StatusCode::REQUEST_TIMEOUT
                && resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS =>
        {
            Outcome::Final(format!("status {}", resp.status()))
        }
        Ok(resp) => Outcome::Retry(format!("status {}", resp.status())),
        Err(_) => Outcome::Retry("network error".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};

    use crate::oauth_as_token::verify_access_token;
    use crate::oauth_issuer_key::IssuerPublicKey;

    const ISS: &str = "https://nest.example";
    const AUD: &str = "https://app.example/client.json";
    const NOW: i64 = 1_800_000_000;

    fn signer() -> (IssuerSigner, Vec<IssuerPublicKey>) {
        let minted = fauna_provisioning::oauth_issuer::mint_issuer_key().expect("mint");
        let signer = IssuerSigner {
            kid: minted.kid.clone(),
            secret_scalar: minted.secret_scalar,
        };
        let public = vec![IssuerPublicKey {
            kid: minted.kid,
            x: minted.x,
            y: minted.y,
            retired_at: None,
        }];
        (signer, public)
    }

    fn part(token: &str, index: usize) -> serde_json::Value {
        let part = token.split('.').nth(index).expect("three parts");
        serde_json::from_slice(&B64.decode(part).expect("base64url")).expect("json")
    }

    /// Verify as a remote server would: the `kid`'s key from the served set,
    /// ES256 over the signing input, the pinned `alg` and `typ`.
    fn verify_as_a_receiver(keys: &[IssuerPublicKey], token: &str) -> Option<EventClaims> {
        use p256::ecdsa::signature::Verifier as _;
        let mut parts = token.split('.');
        let (header_b64, claims_b64, sig_b64) = (parts.next()?, parts.next()?, parts.next()?);
        let header = part(token, 0);
        if header["alg"] != "ES256" || header["typ"] != EVENT_TOKEN_TYP {
            return None;
        }
        let key = keys.iter().find(|k| header["kid"] == k.kid)?;
        let point = p256::EncodedPoint::from_affine_coordinates(
            B64.decode(&key.x).ok()?.as_slice().into(),
            B64.decode(&key.y).ok()?.as_slice().into(),
            false,
        );
        let verifying = p256::ecdsa::VerifyingKey::from_encoded_point(&point).ok()?;
        let signature = p256::ecdsa::Signature::from_slice(&B64.decode(sig_b64).ok()?).ok()?;
        verifying
            .verify(format!("{header_b64}.{claims_b64}").as_bytes(), &signature)
            .ok()?;
        serde_json::from_slice(&B64.decode(claims_b64).ok()?).ok()
    }

    /// The ruling's two halves: the notification verifies as a security
    /// event token under the served key set, with the pinned `typ` and the
    /// one event; and the same key's access-token verifier refuses it — the
    /// separation lives in the signed bytes, like `at+jwt` vs `JWT`.
    #[test]
    fn the_notification_is_a_security_event_token_no_token_verifier_accepts() {
        let (signer, public) = signer();
        let claims = EventClaims::scope_changed(ISS.into(), AUD.into(), "ab".into(), 42, NOW)
            .expect("claims");
        let token = mint_event_token(&signer, &claims).expect("mint");

        let header = part(&token, 0);
        assert_eq!(header["typ"], EVENT_TOKEN_TYP);
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], public[0].kid);
        let received = verify_as_a_receiver(&public, &token).expect("verifies as a receiver");
        assert_eq!(received, claims);
        assert_eq!(
            received.events.get(SCOPE_CHANGED_EVENT),
            Some(&ScopeChanged { cursor: 42 })
        );
        // Payload-free: the claim set is exactly these members.
        let body = part(&token, 1);
        let mut members: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        members.sort_unstable();
        assert_eq!(members, ["aud", "events", "iat", "iss", "jti", "sub"]);
        assert_eq!(
            body["events"][SCOPE_CHANGED_EVENT],
            serde_json::json!({ "cursor": 42 })
        );

        assert!(
            verify_access_token(&public, &token, ISS, &[AUD], NOW + 1).is_none(),
            "an event token verified as an access token"
        );
    }

    /// Every notification has its own `jti`.
    #[test]
    fn each_notification_carries_a_fresh_jti() {
        let a = EventClaims::scope_changed(ISS.into(), AUD.into(), "ab".into(), 1, NOW).unwrap();
        let b = EventClaims::scope_changed(ISS.into(), AUD.into(), "ab".into(), 1, NOW).unwrap();
        assert_ne!(a.jti, b.jti);
    }

    /// The coalescing contract: one delivery in flight per principal, a
    /// change landing meanwhile owed exactly one more.
    #[test]
    fn deliveries_coalesce_per_principal() {
        let ws = WsState::new();
        assert!(ws.webhook_begin(b"p1"), "the first claim owns the delivery");
        assert!(!ws.webhook_begin(b"p1"), "a second claim queues");
        assert!(!ws.webhook_begin(b"p1"), "and a third changes nothing");
        assert!(
            ws.webhook_begin(b"p2"),
            "another principal is its own delivery"
        );
        assert!(ws.webhook_end(b"p1"), "one more delivery is owed");
        assert!(!ws.webhook_end(b"p1"), "then it is released");
        assert!(ws.webhook_begin(b"p1"), "and claimable again");
        assert!(!ws.webhook_end(b"p2"));
    }
}
