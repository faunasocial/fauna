//! Authentication: Bearer token extractors for API and admin routes.

use std::sync::Arc;

use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};

use crate::routes::AppState;
use crate::sidecar_tokens::SidecarScope;

/// Extract the raw Bearer token from the `Authorization` header: present,
/// `Bearer `-prefixed, non-empty. Callers still validate the token itself
/// against whichever store applies (`token_store`, `bulk_byte_tokens`,
/// `sidecar_tokens`) — this only pulls the shape apart.
fn extract_bearer_token(parts: &Parts) -> Result<&str, StatusCode> {
    bearer_token_from_headers(&parts.headers).ok_or(StatusCode::UNAUTHORIZED)
}

/// The header-map form of [`extract_bearer_token`], for a handler that takes
/// `HeaderMap` because its bearer is **conditional** — an otherwise-public
/// route with one actor-scoped arm (the chunk GET's folder-hinted relay,
/// `chunk_routes::download_chunk`) cannot use an extractor that rejects the
/// whole request when the header is absent. Same shape rule, no validation.
pub fn bearer_token_from_headers(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .strip_prefix("Bearer ")
        .filter(|t| !t.is_empty())
}

/// Why [`check_bearer_session`] refused a bearer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BearerRefusal {
    /// Not a live session bearer: unknown, expired, or revoked.
    Invalid,
    /// A live bearer whose actor may not act right now — suspended, locked
    /// out, no `users` row, a `pending`/`revoked` bridge.
    Standing,
}

/// **The one nest bearer validator** — every door that accepts a session
/// bearer (the extractors below, the authenticated WS upgrade, the hand-rolled
/// byte-plane validates) asks it, and none calls the token store's own
/// `validate` itself (pinned by source shape, `bearer_door_tests`).
///
/// A bearer outlives the moment it was minted, so a live token is not by
/// itself admission. This is the token-store check **plus** the standing
/// question the WS-RPC dispatch gate asks per call, answered by that same
/// function, [`caller_class_for_actor`](crate::bridge_method_allowlist::caller_class_for_actor),
/// rather than a re-derivation of its order: an approved bridge and a
/// custodian holder (no `users` row) keep exactly the doors dispatch grants
/// them, and an **admin resolves before the lockout read**, so a locked-out
/// sole admin is not bricked off the byte plane — the ruling dispatch makes
/// (`nest/common.md` § Client-state recoverability). Until this existed only
/// dispatch asked, and a suspended or locked-out actor's surviving bearer
/// uploaded chunks, downloaded snapshots and opened a WebSocket that received
/// Push frames.
///
/// A standing fault is refused, never waved through (fail closed).
pub(crate) async fn check_bearer_session(
    state: &AppState,
    token: &str,
) -> Result<crate::token_store::ValidatedSession, BearerRefusal> {
    let session = state
        .auth
        .token_store
        .validate_with_session(token)
        .await
        .ok_or(BearerRefusal::Invalid)?;
    match crate::bridge_method_allowlist::caller_class_for_actor(&state.db, &session.actor_id.0)
        .await
    {
        Ok(Some(_)) => Ok(session),
        Ok(None) => {
            tracing::info!(
                actor = %crate::bridge_method_allowlist::actor_prefix_hex(&session.actor_id.0),
                "bearer refused at use: the actor has no standing"
            );
            Err(BearerRefusal::Standing)
        }
        Err(e) => {
            tracing::error!("bearer standing consult failed: {e}");
            Err(BearerRefusal::Standing)
        }
    }
}

/// [`check_bearer_session`], reduced to the actor for a door that answers
/// every refusal alike.
pub(crate) async fn validate_bearer(
    state: &AppState,
    token: &str,
) -> Option<fauna_core::identity::ActorId> {
    check_bearer_session(state, token)
        .await
        .ok()
        .map(|s| s.actor_id)
}

/// Axum extractor that validates a Bearer token from the Authorization header.
/// Extracts the authenticated ActorId.
pub struct BearerAuth(pub fauna_core::identity::ActorId);

impl FromRequestParts<Arc<AppState>> for BearerAuth {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer_token(parts)?;

        match validate_bearer(state, token).await {
            Some(actor_id) => Ok(BearerAuth(actor_id)),
            None => Err(StatusCode::UNAUTHORIZED),
        }
    }
}

/// [`BearerAuth`] that keeps the whole validated session — which session
/// (`token_id`) and which device key minted it — for a long-lived socket
/// that must be findable by the revocations that end either
/// (`transport-connection.md` § *Revocation teardown*). Same refusals.
pub struct BearerSession(pub crate::token_store::ValidatedSession);

impl FromRequestParts<Arc<AppState>> for BearerSession {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer_token(parts)?;
        check_bearer_session(state, token)
            .await
            .map(BearerSession)
            .map_err(|_| StatusCode::UNAUTHORIZED)
    }
}

/// Axum extractor that validates a Bearer token and returns both the ActorId and the raw token string.
/// Needed for operations that must know the current token (e.g. revoke-all-except-current).
pub struct BearerAuthWithToken {
    pub actor_id: fauna_core::identity::ActorId,
    pub raw_token: String,
}

impl FromRequestParts<Arc<AppState>> for BearerAuthWithToken {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer_token(parts)?.to_string();

        match validate_bearer(state, &token).await {
            Some(actor_id) => Ok(BearerAuthWithToken {
                actor_id,
                raw_token: token,
            }),
            None => Err(StatusCode::UNAUTHORIZED),
        }
    }
}

/// Axum extractor for the bulk-byte **write** routes: `POST /api/v1/chunks`,
/// `/chunks/check`, `/manifests`, and `PUT /api/v1/blob/{cid}`. Accepts EITHER a
/// full session bearer (existing sync clients — validated against `token_store`)
/// OR a bulk-byte token (`webdav-server.md` § Bulk-byte plane) whose scope grants
/// `Write`.
///
/// **The write routes do not branch on the token's mint purpose, on purpose.**
/// Every purpose yields identical power to *write* at the byte layer (the store
/// is one global content-addressed store), so for a write the purpose is spent
/// entirely at the mint — stated at the owner site,
/// `fauna_protocol::wrapped_blob::BulkByteMintPurpose`. A purpose check on a
/// write route would read to a later session as if the token conferred
/// something narrower than it does.
///
/// Rejections: `401` for a missing/unknown/expired token; **`403`** for a valid
/// bulk-byte token scoped `Read` (defense in depth for the user's
/// read-only-over-WebDAV preference, which the MDA enforces authoritatively at the
/// PUT boundary, and what keeps a cross-nest reader's read
/// token off every write route).
///
/// Bulk tokens live in a store disjoint from `token_store`, so one can never
/// validate as a full session. **Two acceptors take one:** this extractor, and
/// [`relay_reader`] — the read door on the chunk route's store-miss arm, which
/// is the one place a route *does* read the purpose.
pub struct BulkWriteAuth(pub fauna_core::identity::ActorId);

/// Who a hinted, store-missing chunk read is for — the caller of the relay arm
/// of `GET /api/v1/chunks/{hash}` (`chunk_routes::relay_chunk_for_folder`).
pub enum RelayReader {
    /// A session on this nest: the folder's owner or a same-nest member.
    Session(fauna_core::identity::ActorId),
    /// A member whose account lives on another nest, named by a byte-plane
    /// token this nest minted for it through the federated member gate.
    ForeignMember(fauna_core::identity::ActorId),
}

/// Name the caller of the relay read arm from its bearer, or `None` (`401`).
///
/// A session bearer is taken as before. A bulk-byte token is taken **only**
/// when it was minted for a cross-nest member of a shared folder — the write
/// token its uploads carry, or the read-scoped twin
/// (`BulkByteMintPurpose::is_foreign_folder`) — and any access suffices to
/// read. Every other purpose (WebDAV, mail, index, nest backup, conversation
/// attachments) is refused exactly as an unknown bearer is.
///
/// **Here the purpose is read after the mint, and that is right** (`federation.md`
/// § Cross-nest shared folders + channel append → *Where a federated byte-plane
/// token is taken for a read*): it confers nothing narrower or wider at the
/// byte layer — it says which roster names the token's actor. The caller
/// resolves a [`RelayReader::ForeignMember`] through `channel_foreign_members`
/// and nothing else, so a federated token can never stand in for the session
/// of a same-nest account that happens to share its actor id.
pub async fn relay_reader(state: &Arc<AppState>, token: &str) -> Option<RelayReader> {
    if let Some(actor_id) = validate_bearer(state, token).await {
        return Some(RelayReader::Session(actor_id));
    }
    let scope = state.auth.bulk_byte_tokens.validate(token).await?;
    scope
        .purpose
        .is_foreign_folder()
        .then_some(RelayReader::ForeignMember(scope.actor_id))
}

impl FromRequestParts<Arc<AppState>> for BulkWriteAuth {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer_token(parts)?;

        // A full session bearer (sync clients) is always accepted.
        if let Some(actor_id) = validate_bearer(state, token).await {
            return Ok(BulkWriteAuth(actor_id));
        }

        // Otherwise it may be a WebDAV bulk-byte token. Only a `Write` scope may
        // hit a write route; a valid `Read` scope is a hard `403`.
        match state.auth.bulk_byte_tokens.validate(token).await {
            Some(scope) if scope.access == fauna_protocol::wrapped_blob::BulkByteAccess::Write => {
                Ok(BulkWriteAuth(scope.actor_id))
            }
            Some(_) => Err(StatusCode::FORBIDDEN),
            None => Err(StatusCode::UNAUTHORIZED),
        }
    }
}

/// Axum extractor: validates Bearer token AND checks admin status.
/// Returns 401 if token invalid, 403 if not admin.
pub struct AdminBearerAuth(pub fauna_core::identity::ActorId);

impl FromRequestParts<Arc<AppState>> for AdminBearerAuth {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer_token(parts)?;

        let actor_id = match validate_bearer(state, token).await {
            Some(id) => id,
            None => return Err(StatusCode::UNAUTHORIZED),
        };

        match state.db.is_admin(&actor_id.0).await {
            Ok(true) => Ok(AdminBearerAuth(actor_id)),
            _ => Err(StatusCode::FORBIDDEN),
        }
    }
}

// ---------------------------------------------------------------------------
// Sidecar token authentication
// ---------------------------------------------------------------------------

/// Extract and validate the bearer token from the Authorization header,
/// returning the granted scopes.  Returns 401 if the token is missing or
/// not found in the sidecar token map.
fn extract_sidecar_token(
    parts: &Parts,
    state: &Arc<AppState>,
) -> Result<Vec<SidecarScope>, StatusCode> {
    let token = extract_bearer_token(parts)?;

    match state.sidecar_tokens.get(token) {
        Some(scopes) => Ok(scopes.clone()),
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Axum extractor: validates a sidecar Bearer token and checks for the
/// `Bridge` scope.
pub struct BridgeSidecarAuth;

impl FromRequestParts<Arc<AppState>> for BridgeSidecarAuth {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let scopes = extract_sidecar_token(parts, state)?;
        if scopes.contains(&SidecarScope::Bridge) {
            Ok(BridgeSidecarAuth)
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// Axum extractor: validates a sidecar Bearer token and checks for the
/// `Dns` scope.
pub struct DnsSidecarAuth;

impl FromRequestParts<Arc<AppState>> for DnsSidecarAuth {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let scopes = extract_sidecar_token(parts, state)?;
        if scopes.contains(&SidecarScope::Dns) {
            Ok(DnsSidecarAuth)
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }
}

#[cfg(test)]
mod bulk_write_auth_tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::routes::AppState;
    use axum::http::Request;
    use fauna_core::identity::ActorId;
    use fauna_protocol::wrapped_blob::{BulkByteAccess, BulkByteMintPurpose};

    async fn test_state() -> Arc<AppState> {
        Arc::new(AppState::for_test(Arc::new(
            CacheDb::open_in_memory().unwrap(),
        )))
    }

    async fn extract(state: &Arc<AppState>, header: Option<&str>) -> Result<ActorId, StatusCode> {
        let mut builder = Request::builder();
        if let Some(h) = header {
            builder = builder.header("authorization", h);
        }
        let (mut parts, _) = builder
            .body(axum::body::Body::empty())
            .unwrap()
            .into_parts();
        BulkWriteAuth::from_request_parts(&mut parts, state)
            .await
            .map(|a| a.0)
    }

    #[tokio::test]
    async fn missing_header_is_401() {
        let state = test_state().await;
        assert_eq!(extract(&state, None).await, Err(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn full_session_bearer_is_accepted() {
        let state = test_state().await;
        let actor = ActorId([0x51; 32]);
        // A registered actor: the session arm asks the actor's standing, and an
        // actor with no `users` row has none.
        state
            .db
            .create_user(&actor.0, "free", "bulk-writer")
            .await
            .unwrap();
        let token = state.auth.token_store.insert(actor, 3600).await;
        assert_eq!(
            extract(&state, Some(&format!("Bearer {token}"))).await,
            Ok(actor)
        );
    }

    #[tokio::test]
    async fn a_suspended_actors_session_bearer_is_401() {
        let state = test_state().await;
        let actor = ActorId([0x52; 32]);
        state
            .db
            .create_user(&actor.0, "free", "suspended-writer")
            .await
            .unwrap();
        assert!(
            state
                .db
                .suspend_user_now(&actor.0, "test", "other")
                .await
                .unwrap()
        );
        let token = state.auth.token_store.insert(actor, 3600).await;
        assert_eq!(
            extract(&state, Some(&format!("Bearer {token}"))).await,
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[tokio::test]
    async fn bulk_write_token_is_accepted() {
        let state = test_state().await;
        let actor = ActorId([0x52; 32]);
        let (token, _) = state
            .auth
            .bulk_byte_tokens
            .mint(
                actor,
                "photos".into(),
                BulkByteAccess::Write,
                BulkByteMintPurpose::Folder,
                300,
            )
            .await;
        assert_eq!(
            extract(&state, Some(&format!("Bearer {token}"))).await,
            Ok(actor)
        );
    }

    #[tokio::test]
    async fn bulk_read_token_on_write_route_is_403() {
        let state = test_state().await;
        let actor = ActorId([0x53; 32]);
        let (token, _) = state
            .auth
            .bulk_byte_tokens
            .mint(
                actor,
                "photos".into(),
                BulkByteAccess::Read,
                BulkByteMintPurpose::Folder,
                300,
            )
            .await;
        assert_eq!(
            extract(&state, Some(&format!("Bearer {token}"))).await,
            Err(StatusCode::FORBIDDEN)
        );
    }

    #[tokio::test]
    async fn expired_bulk_token_is_401() {
        let state = test_state().await;
        let actor = ActorId([0x54; 32]);
        let (token, _) = state
            .auth
            .bulk_byte_tokens
            .mint(
                actor,
                "photos".into(),
                BulkByteAccess::Write,
                BulkByteMintPurpose::Folder,
                0,
            )
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert_eq!(
            extract(&state, Some(&format!("Bearer {token}"))).await,
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[tokio::test]
    async fn garbage_token_is_401() {
        let state = test_state().await;
        assert_eq!(
            extract(&state, Some("Bearer not-a-real-token")).await,
            Err(StatusCode::UNAUTHORIZED)
        );
    }
}

/// `extract_bearer_token` is now the single seam all 8 auth extractors in
/// this file share — pin its shape contract directly rather than relying on
/// each extractor's own (uneven) test coverage of the malformed-header path.
#[cfg(test)]
mod extract_bearer_token_tests {
    use super::*;
    use axum::http::Request;

    fn parts_with(header: Option<&str>) -> Parts {
        let mut builder = Request::builder();
        if let Some(h) = header {
            builder = builder.header("authorization", h);
        }
        builder
            .body(axum::body::Body::empty())
            .unwrap()
            .into_parts()
            .0
    }

    #[test]
    fn no_header_is_401() {
        assert_eq!(
            extract_bearer_token(&parts_with(None)),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn empty_bearer_token_is_401() {
        assert_eq!(
            extract_bearer_token(&parts_with(Some("Bearer "))),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn non_bearer_scheme_is_401() {
        assert_eq!(
            extract_bearer_token(&parts_with(Some("Basic abc123"))),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn well_formed_bearer_is_extracted() {
        assert_eq!(
            extract_bearer_token(&parts_with(Some("Bearer abc123"))),
            Ok("abc123")
        );
    }
}

/// Source-shape pin for the one bearer validator. A door
/// that validates a bearer against the token store directly is green in every
/// behavioral test that does not revoke its actor's standing — which is how the
/// WS upgrade and eight HTTP doors came to serve suspended actors — so the seam
/// is held by shape: across the whole crate, the token store's `validate` /
/// `validate_with_session` is called from `check_bearer_session` and from the
/// two test modules that assert a mint landed in the store, and nowhere else.
#[cfg(test)]
mod bearer_door_tests {
    /// Occurrences of the token-store validate call per `src/` file, whitespace
    /// stripped so a call split across lines still counts. A new entry is a
    /// new door bypassing the standing consult unless it is test-only code —
    /// route it through `check_bearer_session` / `validate_bearer` instead.
    const ALLOWED: &[(&str, usize)] = &[
        // `check_bearer_session` itself.
        ("auth.rs", 1),
        // Test modules asserting a mint is (or is not) in the store.
        ("auth_core.rs", 2),
        ("bridge_blob_handlers.rs", 1),
    ];

    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src/ is readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn no_door_validates_a_bearer_without_asking_the_actors_standing() {
        // Built from halves so this file's own source never matches it.
        let needle = format!("{}{}", "token_store.", "validate");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        let mut found = Vec::new();
        for path in files {
            let text = std::fs::read_to_string(&path).expect("source is utf-8");
            let squashed: String = text.split_whitespace().collect();
            let n = squashed.matches(needle.as_str()).count();
            if n > 0 {
                let rel = path
                    .strip_prefix(&src)
                    .expect("under src/")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push((rel, n));
            }
        }
        found.sort();
        let mut allowed: Vec<(String, usize)> =
            ALLOWED.iter().map(|(f, n)| (f.to_string(), *n)).collect();
        allowed.sort();
        assert_eq!(
            found, allowed,
            "a file validates a bearer against the token store directly — every \
             bearer door must go through `auth::check_bearer_session` / \
             `validate_bearer`, which also asks the actor's standing; a door \
             that skips it serves a suspended or locked-out actor's surviving \
             bearer"
        );
    }
}
