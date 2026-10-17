//! The web paywall's two visitor entitlement doors (`monetization.md`
//! § Pillar 2 → *Visitor entitlement doors*, ruled 2026-10-09):
//!
//! - `POST /api/v1/paywall/redeem` (form-encoded `code` + `path`) — the target
//!   of the static claim-code form on every paywall teaser. A browser
//!   presentation of a claim code is NOT a redemption: the claim row is read,
//!   never written, and an entitling code answers `303 See Other` to the
//!   ordinary 10-minute `?token=` URL (ruling (1)). The code is the visitor's
//!   durable web credential.
//! - `GET /api/v1/paywall/return/{provider}?ref=…` — where a provider's
//!   post-checkout redirect lands the buyer. It resolves the provider's
//!   reference to the claim the webhook already minted, in the nest's own
//!   table — no provider API call (ruling (2)).
//!
//! Both are unauthenticated, per-source throttled
//! ([`crate::anonymous_rate_limit::paywall_door_config`]), and resolve the
//! **site owner from the request's `Host`** exactly as the serving fallback
//! does — the owner is never a form field, so a code is honoured only on its
//! own creator's site. Registered in `build_router` above the serving
//! fallback and compiled in only under the `payments` feature, like the
//! webhook ingress. Every refusal is a nest-owned page with a non-2xx status
//! (ruling (3)): `403` *not accepted* (one message for every guessable
//! miss), `410` *no longer valid*, `404` no such paywalled resource, `429`
//! throttled — never `200`, never a teaser re-render.

use std::sync::Arc;

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::anonymous_rate_limit::{
    PAYWALL_REDEEM_BUCKET, PAYWALL_RETURN_BUCKET, ShedKey, check, report_shed,
};
use crate::registration::OptionalConnectInfo;
use crate::routes::AppState;

/// The redeem door's form body.
#[derive(Debug, Deserialize)]
pub struct RedeemForm {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub path: String,
}

/// The return door's query.
#[derive(Debug, Deserialize)]
pub struct ReturnQuery {
    #[serde(default, rename = "ref")]
    pub reference: String,
}

/// The one *not accepted* sentence: unknown code, another site's code, and a
/// code whose tier does not entitle this resource all read the same, so the
/// guessing surface learns nothing from the refusal (ruling (3)).
const NOT_ACCEPTED: &str = "That code was not accepted for this page.";

/// `POST /api/v1/paywall/redeem`.
pub async fn paywall_redeem(
    State(state): State<Arc<AppState>>,
    OptionalConnectInfo(peer): OptionalConnectInfo,
    headers: HeaderMap,
    Form(form): Form<RedeemForm>,
) -> Response {
    if !check(&state.paywall_door_rate_limit, peer, PAYWALL_REDEEM_BUCKET) {
        report_shed(
            &state.paywall_door_rate_limit_shed,
            PAYWALL_REDEEM_BUCKET,
            ShedKey::Peer(peer),
        );
        return throttled();
    }
    let Some(owner) = site_owner(&state, &headers).await else {
        return refusal(StatusCode::NOT_FOUND, "There is no site here.", None);
    };
    let back = back_link(&form.path);

    // The resource first: a `404` is a statement about the path, never about
    // any code, so it leaks nothing about the code's validity.
    let resource =
        match crate::web_content::paywall::paywall_resource(&state.db, &owner, &form.path).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                return refusal(
                    StatusCode::NOT_FOUND,
                    "There is no paywalled page at that address.",
                    Some(&back),
                );
            }
            Err(e) => return internal(e),
        };

    let code = form.code.trim();
    let claim = if code.is_empty() {
        None
    } else {
        match state.db.get_payment_claim(code).await {
            Ok(c) => c,
            Err(e) => return internal(e),
        }
    };
    let Some(claim) = claim.filter(|c| c.author_id.as_slice() == owner.as_slice()) else {
        return refusal(StatusCode::FORBIDDEN, NOT_ACCEPTED, Some(&back));
    };
    let now = crate::db::now_epoch_secs();
    if claim.voided_at.is_some() || claim.valid_until.is_some_and(|until| until < now) {
        return refusal(
            StatusCode::GONE,
            "That code is no longer valid. If you think this is a mistake, ask the creator.",
            Some(&back),
        );
    }
    match crate::payment_core::claim_entitles(&state.db, &owner, &claim.tier_name, &resource.tier)
        .await
    {
        Ok(true) => {}
        Ok(false) => return refusal(StatusCode::FORBIDDEN, NOT_ACCEPTED, Some(&back)),
        Err(e) => return internal(e),
    }

    let Some(holder) = &state.web_serve_holder else {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "Paid pages are not being served on this site right now.",
            Some(&back),
        );
    };
    let token = match crate::web_content::token::WebPaywallToken::mint(
        &holder.actor_keypair(),
        owner,
        &resource.path,
    ) {
        Ok((token, _)) => token,
        Err(e) => return internal(e),
    };
    // The claim row is not written (ruling (1)): an unredeemed code stays
    // bindable in the buyer's app later, and keeps opening the web.
    let location = format!("/{}?token={token}", resource.path);
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, location)
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(axum::body::Body::empty())
        .unwrap_or_else(internal)
}

/// `GET /api/v1/paywall/return/{provider}?ref=…`.
pub async fn paywall_return(
    State(state): State<Arc<AppState>>,
    OptionalConnectInfo(peer): OptionalConnectInfo,
    headers: HeaderMap,
    Path(provider): Path<String>,
    Query(query): Query<ReturnQuery>,
) -> Response {
    if !check(&state.paywall_door_rate_limit, peer, PAYWALL_RETURN_BUCKET) {
        report_shed(
            &state.paywall_door_rate_limit_shed,
            PAYWALL_RETURN_BUCKET,
            ShedKey::Peer(peer),
        );
        return throttled();
    }
    let Some(owner) = site_owner(&state, &headers).await else {
        return refusal(StatusCode::NOT_FOUND, "There is no site here.", None);
    };
    // `manual` claims never carry a reference (an author-chosen
    // `external_ref` is guessable), so the door cannot serve them.
    if provider == "manual" || !fauna_payments::known_kinds().contains(&provider.as_str()) {
        return refusal(
            StatusCode::FORBIDDEN,
            "This payment link is not recognised.",
            Some("/"),
        );
    }
    let reference = query.reference.trim();
    let claim = if reference.is_empty() {
        None
    } else {
        match state
            .db
            .find_payment_claim_by_return_ref(&owner, &provider, reference)
            .await
        {
            Ok(c) => c,
            Err(e) => return internal(e),
        }
    };
    let Some(claim) = claim else {
        return confirming_page();
    };

    // A direct link when the claim's tier sells exactly one web-published
    // post: `unlocks_post` corroborated by `get_tier_selling_post`, and the
    // post's sealed page present.
    let mut direct = None;
    if let Ok(Some(tier)) = state
        .db
        .get_subscription_tier(&owner, &claim.tier_name)
        .await
        && let Some(post_hex) = tier.unlocks_post.as_deref()
        && let Ok(Some(seller)) = state.db.get_tier_selling_post(&owner, post_hex).await
        && seller.name == claim.tier_name
        && let Ok(post_id) = fauna_core::hex32::decode(post_hex)
        && let Ok(Some(path)) = state
            .db
            .find_web_rendered_sealed_path_by_post(&owner, &post_id)
            .await
        && let Some(holder) = &state.web_serve_holder
        && let Ok((token, _)) =
            crate::web_content::token::WebPaywallToken::mint(&holder.actor_keypair(), owner, &path)
    {
        direct = Some(format!("/{path}?token={token}"));
    }
    code_page(&claim.code, direct.as_deref())
}

/// The site owner the request's `Host` names — the serving fallback's own
/// resolution (`lib.rs::web_content_or_info`).
async fn site_owner(state: &AppState, headers: &HeaderMap) -> Option<[u8; 32]> {
    let resolver = state.host_resolver.as_ref()?;
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    resolver.resolve(host).await
}

/// The link back to the page the visitor came from: the resource path when it
/// is a plain site-relative path, else the site root.
fn back_link(path: &str) -> String {
    let p = path.trim().trim_start_matches('/');
    if p.is_empty() || p.contains("://") || p.starts_with('/') || p.contains('\\') {
        "/".into()
    } else {
        format!("/{p}")
    }
}

fn escape(s: &str) -> String {
    crate::web_content::render::xml_escape(s)
}

/// A nest-owned page in the built-in teaser's style.
fn page(status: StatusCode, title: &str, body_html: &str, extra_head: &str) -> Response {
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  {extra_head}<title>{title}</title>
  <style>
    body {{ font-family: system-ui, sans-serif; max-width: 650px; margin: 2rem auto; padding: 0 1rem; color: #222; }}
    h1 {{ margin-bottom: 0.25rem; font-size: 1.4rem; }}
    .box {{ margin-top: 2rem; padding: 1.25rem; border: 1px solid #ddd; border-radius: 8px; background: #fafafa; }}
    .code {{ font-family: ui-monospace, monospace; font-size: 1.4rem; letter-spacing: 0.08em; font-weight: 600; }}
    a.button {{ display: inline-block; margin-top: 0.75rem; padding: 0.5rem 1rem; background: #0066cc; color: #fff; text-decoration: none; border-radius: 6px; }}
    .nav {{ margin-top: 2.5rem; font-size: 0.9rem; }}
    .nav a {{ color: #0066cc; text-decoration: none; }}
  </style>
</head>
<body>
  <h1>{title}</h1>
  <div class="box">{body_html}</div>
</body>
</html>"#,
        title = escape(title),
    );
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(axum::body::Body::from(html))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn refusal(status: StatusCode, message: &str, back: Option<&str>) -> Response {
    let back = back
        .map(|href| {
            format!(
                r#"<div class="nav"><a href="{}">Back</a></div>"#,
                escape(href)
            )
        })
        .unwrap_or_default();
    page(
        status,
        "Not unlocked",
        &format!("<p>{}</p>{back}", escape(message)),
        "",
    )
}

fn throttled() -> Response {
    page(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many attempts",
        "<p>Too many attempts from your connection. Wait a minute and try again.</p>",
        "",
    )
}

fn confirming_page() -> Response {
    page(
        StatusCode::ACCEPTED,
        "Confirming your payment",
        "<p>Your payment is being confirmed. This page refreshes on its own.</p>\
         <p>If nothing changes after a few minutes, ask the creator for your access code.</p>",
        r#"<meta http-equiv="refresh" content="5">
  "#,
    )
}

fn code_page(code: &str, direct: Option<&str>) -> Response {
    let direct = direct
        .map(|href| {
            format!(
                r#"<p><a class="button" href="{}">Open the post</a></p>"#,
                escape(href)
            )
        })
        .unwrap_or_default();
    page(
        StatusCode::OK,
        "Thank you",
        &format!(
            r#"<p>Your access code:</p><p class="code">{}</p>
<p>Keep this code. Enter it in the box on any paid page of this site to open it. When a link expires, enter the code again.</p>{direct}
<div class="nav"><a href="/">Home</a></div>"#,
            escape(code)
        ),
        "",
    )
}

fn internal(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "paywall door failed");
    page(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Something went wrong",
        "<p>Please try again in a moment.</p>",
        "",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_link_stays_on_the_site() {
        assert_eq!(back_link("post/a.html"), "/post/a.html");
        assert_eq!(back_link("/post/a.html"), "/post/a.html");
        assert_eq!(back_link("https://evil.example/x"), "/");
        // A protocol-relative path collapses to a same-site path.
        assert_eq!(back_link("//evil.example/x"), "/evil.example/x");
        assert_eq!(back_link(""), "/");
    }

    #[test]
    fn the_paywall_doors_throttle_per_source_and_trip_to_429() {
        // Mirrors `invite_verify_throttles_per_source`: the doors key on
        // (source, door) in a limiter sized as `paywall_door_config`.
        use crate::bridge_rate_limit::Limiter;
        let l = Limiter::with_config(crate::anonymous_rate_limit::paywall_door_config());
        let src = Some(std::net::SocketAddr::from(([203, 0, 113, 7], 5000)));
        let other = Some(std::net::SocketAddr::from(([198, 51, 100, 9], 5000)));
        let budget = crate::anonymous_rate_limit::paywall_door_config().max_events;
        for _ in 0..budget {
            assert!(check(&l, src, PAYWALL_REDEEM_BUCKET));
        }
        assert!(!check(&l, src, PAYWALL_REDEEM_BUCKET));
        // The return door has its own bucket; another source is unaffected.
        assert!(check(&l, src, PAYWALL_RETURN_BUCKET));
        assert!(check(&l, other, PAYWALL_REDEEM_BUCKET));
        assert_eq!(throttled().status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
