//! The admin's web-app origin choice, nest side: what this nest's reserved
//! `/app` answers (the bundled SPA, or a `302` to the central origin with this
//! nest pre-filled), the `fauna.admin.web_app_origin.{get,set}` kinds that
//! drive it, and the probe the router reads it through.
//!
//! Owner: `docs/goal/behavior/web-content-hosting.md` § Same-origin security
//! model → *The nest-served `/app/` and the central origin*. The mode, the
//! serving decision and the redirect address are derived once, in shared Rust
//! (`fauna_protocol::web_app_origin`); this module only reads the nest's state
//! into that derivation and answers HTTP with it. The share-link viewer
//! (`GET /share/<token>` on a navigation request — `share-links.md` § The
//! private-file extension) follows the same choice through the same
//! [`WebAppOriginProbe`] — it is served by [`app_router`] itself
//! (`share_routes::private_viewer`).

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::web_app_origin::{
    AdminWebAppOriginGetReply, AdminWebAppOriginGetRequest, AdminWebAppOriginSetReply,
    AdminWebAppOriginSetRequest, WebAppOrigin, WebAppOriginServing,
};

use crate::routes::AppState;
use crate::rpc_errors::internal;

/// What a reserved navigation path answers right now. Read per request: the
/// admin's choice swaps live, and the handle domain is learnt at claim.
pub type WebAppOriginProbe = Arc<dyn Fn() -> WebAppOriginServing + Send + Sync>;

/// A probe that always answers `serving` — for tests of the router alone.
pub fn fixed_probe(serving: WebAppOriginServing) -> WebAppOriginProbe {
    Arc::new(move || serving.clone())
}

/// The live probe: the boot-resolved, admin-swapped mode folded with this
/// nest's handle domain **if set** — never `handle_domain()`'s `localhost`
/// placeholder, which would redirect a domainless box with `nest=localhost`.
pub fn live_probe(state: Arc<AppState>) -> WebAppOriginProbe {
    Arc::new(move || serving(&state))
}

/// [`WebAppOriginServing::resolve`] over the live state.
pub fn serving(state: &AppState) -> WebAppOriginServing {
    WebAppOriginServing::resolve(
        **state.web_app_origin.load(),
        state.handle_domain_if_set().as_deref(),
    )
}

/// The projection `fauna.admin.web_app_origin.get` answers and
/// `fauna.setup.status` carries.
pub fn projection(state: &AppState) -> AdminWebAppOriginGetReply {
    AdminWebAppOriginGetReply::project(
        **state.web_app_origin.load(),
        state.handle_domain_if_set().as_deref(),
    )
}

/// Resolve the choice at boot: a present, readable `nest_web_app_origin` row
/// wins; absent — or unreadable, which is logged — is bundled, the
/// works-out-of-the-box default (the box must boot serving something).
pub async fn resolve_web_app_origin(db: &crate::db::CacheDb) -> WebAppOrigin {
    match db.get_web_app_origin().await {
        Ok(Some(mode)) => mode,
        Ok(None) => WebAppOrigin::Bundled,
        Err(e) => {
            tracing::error!("get_web_app_origin at boot failed: {e:#}; serving bundled");
            WebAppOrigin::Bundled
        }
    }
}

/// The single write path: upsert the row (the atomic decision point), then swap
/// the live value so the next `/app` request follows immediately.
pub async fn apply_web_app_origin_change(
    state: &AppState,
    mode: WebAppOrigin,
) -> anyhow::Result<()> {
    state.db.set_web_app_origin(mode).await?;
    state.web_app_origin.store(Arc::new(mode));
    Ok(())
}

/// The `/app` service: the redirect when the probe says central, else the
/// shipped SPA (`static_dir`), else 404 — `/app` is reserved and never falls
/// through to anything else's content (invariant 3). The caller wraps it in the
/// invariant-#5 security headers, so they ride every answer, the redirect's
/// included.
pub(crate) fn app_router(static_dir: Option<&str>, probe: WebAppOriginProbe) -> axum::Router {
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::response::IntoResponse;

    let serve_dir = static_dir.map(|dir| {
        tower_http::services::ServeDir::new(dir).fallback(tower_http::services::ServeFile::new(
            std::path::Path::new(dir).join("index.html"),
        ))
    });
    axum::Router::new().fallback(
        move |axum::extract::OriginalUri(original): axum::extract::OriginalUri,
              req: axum::extract::Request| {
            let probe = probe.clone();
            let serve_dir = serve_dir.clone();
            async move {
                let path_and_query = original
                    .path_and_query()
                    .map_or_else(|| original.path(), |pq| pq.as_str());
                if let Some(location) = probe().redirect_for(path_and_query)
                    && let Ok(location) = HeaderValue::from_str(&location)
                {
                    // 302, never 301/308: a browser caches a permanent redirect,
                    // and an admin who moves back to bundled could not undo it
                    // for users who already followed it. `no-store` for the same
                    // reason.
                    return (
                        StatusCode::FOUND,
                        [
                            (header::LOCATION, location),
                            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
                        ],
                    )
                        .into_response();
                }
                match serve_dir {
                    Some(mut sd) => {
                        use tower_service::Service;
                        match sd.call(req).await {
                            Ok(resp) => resp.map(axum::body::Body::new).into_response(),
                            Err(e) => match e {},
                        }
                    }
                    None => StatusCode::NOT_FOUND.into_response(),
                }
            }
        },
    )
}

// ── fauna.admin.web_app_origin.{get,set} ─────────────────────────────────────

pub const KIND_WEB_APP_ORIGIN_GET: &str = "fauna.admin.web_app_origin.get";
pub const KIND_WEB_APP_ORIGIN_SET: &str = "fauna.admin.web_app_origin.set";

const AUDIT_WEB_APP_ORIGIN_SET: &str = "admin:web_app_origin.set";

pub fn register_web_app_origin_handlers(b: &mut crate::rpc_router::RpcRouterBuilder) {
    // Mirror any change in `KindRegistry::register_admin_kinds`.
    b.add(
        KIND_WEB_APP_ORIGIN_GET,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_handler(),
        },
    );
    b.add(
        KIND_WEB_APP_ORIGIN_SET,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: set_handler(),
        },
    );
}

fn get_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                KIND_WEB_APP_ORIGIN_GET,
                internal,
            )
            .await?;
            let _req: AdminWebAppOriginGetRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("web_app_origin", e))?;
            crate::rpc_errors::encode_reply(&projection(&state))
        })
    })
}

fn set_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db,
                &actor_id,
                KIND_WEB_APP_ORIGIN_SET,
                internal,
            )
            .await?;
            let req: AdminWebAppOriginSetRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("web_app_origin", e))?;
            apply_web_app_origin_change(&state, req.mode)
                .await
                .map_err(internal)?;
            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    AUDIT_WEB_APP_ORIGIN_SET,
                    None,
                    Some(req.mode.as_str()),
                )
                .await;
            let reply: AdminWebAppOriginSetReply = projection(&state);
            crate::rpc_errors::encode_reply(&reply)
        })
    })
}
