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

use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
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

    fn location(self, primary_domain: &str) -> String {
        match self {
            DavProtocol::CalDav => format!("https://mail.{primary_domain}/.well-known/caldav"),
            DavProtocol::CardDav => format!("https://mail.{primary_domain}/.well-known/carddav"),
            DavProtocol::WebDav => format!("https://mail.{primary_domain}/webdav/"),
        }
    }
}

/// The shared gate: `503` unless `protocol` is live (its own toggle, or an
/// unset toggle following `effective_mail_enabled` — Stage-5 default-off
/// means an unset toggle is OFF, not proof of enablement), `301` to the
/// bridge host when it is.
pub async fn well_known_dav_gate(state: &Arc<AppState>, protocol: DavProtocol) -> Response {
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
            let location = protocol.location(&primary.domain_name);
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
            DavProtocol::CalDav.location("example.com"),
            "https://mail.example.com/.well-known/caldav"
        );
        assert_eq!(
            DavProtocol::CardDav.location("example.com"),
            "https://mail.example.com/.well-known/carddav"
        );
        assert_eq!(
            DavProtocol::WebDav.location("example.com"),
            "https://mail.example.com/webdav/"
        );
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
