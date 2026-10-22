//! Shared RFC 6764-style `.well-known` discovery-redirect gating for the
//! CalDAV/CardDAV/WebDAV apex handlers (`caldav_bridge::wellknown_caldav`,
//! `carddav_bridge::wellknown_carddav`, `webdav_bridge::wellknown_webdav`).
//!
//! All three apexes answer `301` to the mail-bridge MDA's own discovery path
//! exactly when the MDA is actually serving that protocol, `503` otherwise —
//! their own doc comments already cross-reference each other as siblings
//! ("as the CardDAV and WebDAV siblings already do"; "the CalDAV and CardDAV
//! apexes were aligned to it on 2026-07-12"), they just never shared the gate
//! itself. [`well_known_dav_gate`] is that shared gate.
//!
//! On a box whose DAV listener is direct (`AppState.dav_direct_listener ==
//! Some(true)`) there is no `mail.<primary>:443` route, so the redirect names
//! the host the request arrived on, at the admin-set DAV port — the address
//! the member's own mail settings page shows (`caldav-server.md` § Network
//! exposure).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api_error::ApiError;
use crate::routes::AppState;

/// Which `.well-known` apex is asking. Each variant knows its own
/// enabled-toggle getter, display name, and redirect-location shape — the
/// location shape genuinely differs (CalDAV/CardDAV redirect to the bridge
/// host's own `/.well-known/<proto>`; WebDAV redirects straight to the
/// collection root — see `webdav_bridge`'s doc comment for why), so this
/// stays a match over 3 known variants rather than a closure parameter.
#[derive(Clone, Copy)]
pub enum DavProtocol {
    CalDav,
    CardDav,
    WebDav,
}

impl DavProtocol {
    fn display_name(self) -> &'static str {
        match self {
            DavProtocol::CalDav => "CalDAV",
            DavProtocol::CardDav => "CardDAV",
            DavProtocol::WebDav => "WebDAV",
        }
    }

    /// The DB field name, used only in the internal-error message when the
    /// enabled-toggle read itself fails.
    fn enabled_field_label(self) -> &'static str {
        match self {
            DavProtocol::CalDav => "caldav_enabled",
            DavProtocol::CardDav => "carddav_enabled",
            DavProtocol::WebDav => "webdav_enabled",
        }
    }

    async fn get_enabled(self, state: &AppState) -> anyhow::Result<Option<bool>> {
        match self {
            DavProtocol::CalDav => state.db.get_caldav_enabled().await,
            DavProtocol::CardDav => state.db.get_carddav_enabled().await,
            DavProtocol::WebDav => state.db.get_webdav_enabled().await,
        }
    }

    /// The redirect target on the DAV listener at `origin` (`host` or
    /// `host:port`, from [`dav_origin`]).
    fn location(self, origin: &str) -> String {
        match self {
            DavProtocol::CalDav => format!("https://{origin}/.well-known/caldav"),
            DavProtocol::CardDav => format!("https://{origin}/.well-known/carddav"),
            DavProtocol::WebDav => format!("https://{origin}/webdav/"),
        }
    }
}

/// Where the DAV listener answers: `mail.<primary>` (the router's `443`) on a
/// routed or undeclared box; on a direct listener, the host the request
/// arrived on (its `Host` header, port dropped) at the admin-set DAV `port`.
/// `None` when a direct listener's request carries no plain host — the nest
/// knows no other name for its LAN listener.
fn dav_origin(
    primary_domain: &str,
    direct_listener: Option<bool>,
    request_host: Option<&str>,
    port: u16,
) -> Option<String> {
    if direct_listener != Some(true) {
        return Some(format!("mail.{primary_domain}"));
    }
    let authority: axum::http::uri::Authority = request_host?.trim().parse().ok()?;
    if authority.host().is_empty() || authority.as_str().contains('@') {
        return None;
    }
    Some(format!("{}:{port}", authority.host()))
}

/// The shared gate: `503` unless `protocol` is live (its own toggle, or an
/// unset toggle following `effective_mail_enabled` — Stage-5 default-off
/// means an unset toggle is OFF, not proof of enablement), `301` to the
/// DAV listener when it is ([`dav_origin`] picks the host).
pub async fn well_known_dav_gate(
    state: &Arc<AppState>,
    protocol: DavProtocol,
    headers: &HeaderMap,
) -> Response {
    let state: &AppState = state;

    // Explicitly-disabled short-circuits to `503`; `None` (unset) follows the
    // effective mail toggle (`CacheDb::effective_mail_enabled`, the Stage-5
    // default's single owner), matching `fetch_config`'s
    // `unwrap_or(mail_enabled)` projection exactly.
    match protocol.get_enabled(state).await {
        Ok(Some(false)) => {
            return ApiError::service_unavailable(format!(
                "{} is not enabled on this nest",
                protocol.display_name()
            ))
            .into_response();
        }
        Ok(Some(true)) => {}
        Ok(None) => match state.db.effective_mail_enabled().await {
            Ok(true) => {}
            Ok(false) => {
                return ApiError::service_unavailable(format!(
                    "{} is not enabled on this nest",
                    protocol.display_name()
                ))
                .into_response();
            }
            Err(e) => {
                return ApiError::internal(format!("read mail_enabled: {e}")).into_response();
            }
        },
        Err(e) => {
            return ApiError::internal(format!("read {}: {e}", protocol.enabled_field_label()))
                .into_response();
        }
    }

    match state.db.lookup_primary_mail_domain().await {
        Ok(Some(primary)) => {
            // The port the member's page shows (`fauna.bridges.get_caldav_port`).
            let port = match state.db.get_caldav_port().await {
                Ok(p) => p.unwrap_or(fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT),
                Err(e) => {
                    return ApiError::internal(format!("read caldav_port: {e}")).into_response();
                }
            };
            let request_host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
            let Some(origin) = dav_origin(
                &primary.domain_name,
                state.dav_direct_listener,
                request_host,
                port,
            ) else {
                return ApiError::service_unavailable(format!(
                    "{} is served on this nest's own address; the request named no host",
                    protocol.display_name()
                ))
                .into_response();
            };
            let location = protocol.location(&origin);
            Response::builder()
                .status(StatusCode::MOVED_PERMANENTLY)
                .header("Location", location)
                .body(Body::empty())
                .unwrap()
                .into_response()
        }
        Ok(None) => ApiError::service_unavailable(format!(
            "{} is served by the mail-bridge MDA; mail is not enabled on this nest",
            protocol.display_name()
        ))
        .into_response(),
        Err(e) => ApiError::internal(format!("lookup primary mail domain: {e}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CalDAV/CardDAV redirect to the bridge host's own `/.well-known/<proto>`
    /// (the RFC 6764 cross-host bootstrap hop); WebDAV redirects straight to
    /// the collection root with no bridge-side well-known hop. Pinning this
    /// divergence is the whole reason `location` stays a match over 3 known
    /// variants rather than a single format string parameterized on protocol
    /// name — a naive merge would have put WebDAV through the wrong shape.
    #[test]
    fn location_shape_diverges_between_caldav_carddav_and_webdav() {
        assert_eq!(
            DavProtocol::CalDav.location("mail.example.com"),
            "https://mail.example.com/.well-known/caldav"
        );
        assert_eq!(
            DavProtocol::CardDav.location("mail.example.com"),
            "https://mail.example.com/.well-known/carddav"
        );
        assert_eq!(
            DavProtocol::WebDav.location("mail.example.com"),
            "https://mail.example.com/webdav/"
        );
    }

    /// A routed or undeclared box keeps `mail.<primary>`; a direct listener
    /// names the request's own host at the admin port (dropping any port the
    /// `Host` header carried) and has no origin without a plain host.
    #[test]
    fn dav_origin_follows_the_listener_shape() {
        for direct in [Some(false), None] {
            assert_eq!(
                dav_origin("fauna.example", direct, Some("fauna.example"), 8443).as_deref(),
                Some("mail.fauna.example")
            );
        }
        let direct = Some(true);
        assert_eq!(
            dav_origin("fauna.example", direct, Some("fauna.example"), 8443).as_deref(),
            Some("fauna.example:8443")
        );
        assert_eq!(
            dav_origin("fauna.example", direct, Some("home.lan:3000"), 9443).as_deref(),
            Some("home.lan:9443")
        );
        assert_eq!(
            dav_origin("fauna.example", direct, Some("[fd00::5]:443"), 8443).as_deref(),
            Some("[fd00::5]:8443")
        );
        for bad in [
            None,
            Some(""),
            Some("a b"),
            Some("u@evil.example"),
            Some("x/y"),
        ] {
            assert_eq!(
                dav_origin("fauna.example", direct, bad, 8443),
                None,
                "{bad:?}"
            );
        }
    }

    /// Through the gate: CalDAV and CardDAV on a direct listener never name
    /// `mail.<primary>`; routed and undeclared boxes keep the `301` to it.
    #[tokio::test]
    async fn apex_redirect_follows_the_dav_listener_shape() {
        for (direct, expect) in [
            (Some(true), "https://fauna.example:9443"),
            (Some(false), "https://mail.fauna.example"),
            (None, "https://mail.fauna.example"),
        ] {
            let mut state = crate::test_support::fixture_state();
            Arc::get_mut(&mut state).unwrap().dav_direct_listener = direct;
            state
                .db
                .add_mail_domain(
                    "fauna.example",
                    true,
                    "enforce",
                    "expand_primary",
                    None,
                    None,
                )
                .await
                .unwrap();
            state.db.set_caldav_enabled(true).await.unwrap();
            state.db.set_carddav_enabled(true).await.unwrap();
            state.db.set_caldav_port(9443).await.unwrap();
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, "fauna.example".parse().unwrap());
            for (protocol, path) in [
                (DavProtocol::CalDav, "/.well-known/caldav"),
                (DavProtocol::CardDav, "/.well-known/carddav"),
            ] {
                let resp = well_known_dav_gate(&state, protocol, &headers).await;
                assert_eq!(resp.status(), StatusCode::MOVED_PERMANENTLY, "{direct:?}");
                assert_eq!(
                    resp.headers()[header::LOCATION].to_str().unwrap(),
                    format!("{expect}{path}"),
                    "{direct:?}"
                );
            }
            if direct == Some(true) {
                let resp =
                    well_known_dav_gate(&state, DavProtocol::CalDav, &HeaderMap::new()).await;
                assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }

    #[test]
    fn enabled_field_label_names_the_right_db_column_per_protocol() {
        assert_eq!(DavProtocol::CalDav.enabled_field_label(), "caldav_enabled");
        assert_eq!(
            DavProtocol::CardDav.enabled_field_label(),
            "carddav_enabled"
        );
        assert_eq!(DavProtocol::WebDav.enabled_field_label(), "webdav_enabled");
    }

    #[test]
    fn display_name_matches_the_protocol() {
        assert_eq!(DavProtocol::CalDav.display_name(), "CalDAV");
        assert_eq!(DavProtocol::CardDav.display_name(), "CardDAV");
        assert_eq!(DavProtocol::WebDav.display_name(), "WebDAV");
    }
}
