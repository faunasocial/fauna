//! Bluesky OAuth residue routes: client metadata + the OAuth callback.
//!
//! These two stay HTTP — they are OAuth 2.0 spec surfaces whose far end is
//! Bluesky's OAuth authorization server, not a Fauna binary (api-layers.md
//! § HTTP residue). The lower-level control-plane twins (`auth/start`,
//! `auth/status`, `auth` DELETE, `profile`) were **deleted**
//! (2026-06-05, as part of the WS-RPC unification effort)
//! — they were deprecated exact dups of the unified
//! `fauna.bridges.{link,list,unlink}` kinds (each dispatched into the same
//! `BlueskyProvider` trait methods). Clients link / read status / unlink Bluesky
//! through the unified bridge surface; the OAuth *redirect* leg below is the
//! only Bluesky-specific HTTP that remains.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Redirect};
use axum::routing::get;

use fauna_bridge_atproto::oauth::{Agent, CallbackParams, SessionManager};
use fauna_bridge_atproto::xrpc::get_profile_for;

use crate::bluesky::db_helpers;
use crate::routes::AppState;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The OAuth client derived from this deployment's identity domain, or 503
/// Service Unavailable with the reason.
///
/// Returns the `Arc` rather than a borrow because the client is derived and
/// cached behind a lock ([`crate::state::BlueskyState::resolve`]) rather than
/// held in a field, and 503 rather than 404 because "this nest has no public
/// domain yet" is a state that changes when the box is claimed onto one — the
/// document is not permanently absent.
fn get_oauth(
    state: &AppState,
) -> Result<
    std::sync::Arc<fauna_bridge_atproto::oauth::BlueskyOAuthClient>,
    (StatusCode, &'static str),
> {
    state.bluesky_oauth().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Bluesky OAuth is unavailable: this nest has no public domain name, so \
         it cannot present an OAuth client identity Bluesky can resolve.",
    ))
}

/// Validate an OAuth `return_url` as a **same-origin relative path** before it is
/// used as a redirect target, returning the candidate iff safe else the default
/// `/bridges`. The open-redirect guard: the `return_url` a caller hands
/// `link` rides the OAuth app state back to the callback, so an unvalidated one
/// would 302 a victim off our trusted origin (and a CR/LF in it would panic
/// `Redirect::to`). The callback no longer reads it from the attacker-controlled
/// query `state` at all — only from the app state the OAuth client returns once
/// that key matched a stored state. Safe = a single
/// leading `/` (a path on this origin), never a protocol-relative `//host` or a
/// backslash-smuggled `/\host`, and no CR/LF. Applied at both the construction
/// site (`bridge_provider::link`) and here, the load-bearing use site.
pub(super) fn sanitize_return_url(candidate: &str) -> &str {
    const DEFAULT: &str = "/bridges";
    // Some browsers fold `\` to `/` when resolving the authority, so normalize
    // before the `//` test to catch `/\evil` / `/\/evil`.
    let normalized = candidate.replace('\\', "/");
    let safe = normalized.starts_with('/')
        && !normalized.starts_with("//")
        && !candidate.contains(['\n', '\r']);
    if safe { candidate } else { DEFAULT }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// GET /.well-known/atproto-oauth-client
///
/// Return the OAuth client metadata document as JSON.
pub async fn client_metadata(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let oauth = match get_oauth(&state) {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };
    Json(&oauth.client_metadata).into_response()
}

/// GET /api/v1/bluesky/auth/callback
///
/// OAuth callback from the Bluesky authorization server.
/// Exchanges the code for tokens and redirects the browser.
pub async fn auth_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CallbackQuery>,
) -> impl IntoResponse {
    // The query's `state` is the OAuth client's own random key, never the
    // `{actor_hex}|{return_url}` `BlueskyProvider::link` handed it: the client
    // keeps that app state server-side under the key and returns it from
    // `callback()` below, once the key has matched a stored state. So until the
    // callback succeeds, where the user came from is unknown and every early
    // exit redirects to the default.
    let return_url = "/bridges";

    // Handle error from the authorization server
    if let Some(error) = &params.error {
        let desc = params
            .error_description
            .as_deref()
            .unwrap_or(error.as_str());
        let encoded = urlencoding::encode(desc);
        return Redirect::to(&format!(
            "{return_url}?bridge=bluesky&result=error&reason={encoded}"
        ))
        .into_response();
    }

    let oauth = match get_oauth(&state) {
        Ok(o) => o,
        Err(_) => {
            return Redirect::to(&format!(
                "{return_url}?bridge=bluesky&result=error&reason=oauth_not_configured"
            ))
            .into_response();
        }
    };

    let code = match params.code {
        Some(c) => c,
        None => {
            return Redirect::to(&format!(
                "{return_url}?bridge=bluesky&result=error&reason=missing_code"
            ))
            .into_response();
        }
    };

    let callback_params = CallbackParams {
        code,
        state: params.state.clone(),
        iss: params.iss,
    };

    match oauth.callback(callback_params).await {
        Ok((session, app_state)) => {
            // Extract the DID from the session
            let did = session.did().await.map(|d| d.to_string());

            // The app state `link` stored: "{actor_hex}|{return_url}".
            let (actor_hex, return_url) = match app_state.as_deref().and_then(|s| s.split_once('|'))
            {
                Some((actor, url)) => (Some(actor.to_string()), url.to_string()),
                None => (app_state.clone(), return_url.to_string()),
            };
            // Open-redirect guard, kept at the use site although the
            // app state is now server-held: `link` clamps it too, and this is
            // the load-bearing check.
            let return_url = sanitize_return_url(&return_url);

            if let (Some(actor_hex), Some(did_str)) = (&actor_hex, &did) {
                // Call getProfile to get the handle and verify the session works
                let agent = Agent::new(session);
                let handle = match get_profile_for(&agent, did_str).await {
                    Ok(profile) => profile.handle,
                    Err(e) => {
                        tracing::warn!("getProfile failed during callback (non-fatal): {e}");
                        String::new()
                    }
                };

                let conn = state.db.conn().await;
                if let Err(e) =
                    db_helpers::upsert_linked_account(&conn, actor_hex, did_str, &handle)
                {
                    drop(conn);
                    tracing::error!("failed to store bluesky account link: {e}");
                    return Redirect::to(&format!(
                        "{return_url}?bridge=bluesky&result=error&reason=storage_error"
                    ))
                    .into_response();
                }
                drop(conn);
                tracing::info!(
                    "Bluesky account linked: actor={actor_hex} did={did_str} handle={handle}"
                );
            }

            Redirect::to(&format!("{return_url}?bridge=bluesky&result=linked")).into_response()
        }
        Err(e) => {
            tracing::warn!("bluesky auth callback error: {e}");
            Redirect::to(&format!(
                "{return_url}?bridge=bluesky&result=error&reason=auth_failed"
            ))
            .into_response()
        }
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/.well-known/atproto-oauth-client", get(client_metadata))
        .route("/api/v1/bluesky/auth/callback", get(auth_callback))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower_service::Service;

    #[test]
    fn sanitize_keeps_same_origin_paths() {
        assert_eq!(sanitize_return_url("/bridges"), "/bridges");
        assert_eq!(sanitize_return_url("/status"), "/status");
        assert_eq!(
            sanitize_return_url("/settings/bridges?tab=bluesky"),
            "/settings/bridges?tab=bluesky"
        );
    }

    /// Load-bearing: every cross-origin / injection shape clamps to the
    /// `/bridges` default — absolute URLs, protocol-relative `//host`, the
    /// backslash-smuggled `/\host` variants, a scheme with no leading slash, the
    /// empty string, and a CR/LF that would otherwise panic `Redirect::to`.
    #[test]
    fn sanitize_rejects_cross_origin_and_injection() {
        for evil in [
            "https://evil.example/phish",
            "http://evil.example",
            "//evil.example",
            "/\\evil.example",
            "/\\/evil.example",
            "javascript:alert(1)",
            "",
            "bridges",
            "/ok\r\nLocation: https://evil.example",
        ] {
            assert_eq!(
                sanitize_return_url(evil),
                "/bridges",
                "{evil:?} must clamp to the default"
            );
        }
    }

    fn build_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        Arc::new(AppState::for_test(db))
    }

    /// End-to-end: the callback error branch fires before any CSRF check on
    /// a fully attacker-controlled `state`, so it must still redirect to a
    /// same-origin path — proving the open redirect is neutralized at the use
    /// site, not just in the sanitizer unit.
    #[tokio::test]
    async fn callback_error_branch_clamps_open_redirect() {
        let mut router = routes().with_state(build_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri(
                "/api/v1/bluesky/auth/callback?error=denied\
                 &state=deadbeef%7Chttps%3A%2F%2Fevil.example%2Fphish",
            )
            .body(Body::empty())
            .unwrap();
        let resp = router.call(req).await.unwrap();
        let loc = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            loc.starts_with("/bridges?"),
            "open redirect not clamped: Location = {loc:?}"
        );
    }
}
