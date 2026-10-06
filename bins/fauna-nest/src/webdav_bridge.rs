//! WebDAV discovery redirect — the only in-nest WebDAV HTTP surface.
//!
//! The real WebDAV files surface is a *view over the existing folder store*
//! served by the mail-bridge MDA (`mail.<primary>:443`, behind the SNI router);
//! there is no in-core WebDAV path (webdav-server.md § Process topology). All
//! that lives here is the apex discovery redirect:
//! - GET /.well-known/webdav → `301` to `https://mail.<primary>/webdav/`
//!   (a NextCloud-ecosystem convention, NOT RFC 6764 — there is deliberately no
//!   `_webdavs._tcp` SRV row), `503` until WebDAV is live.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;

use crate::routes::AppState;
use crate::well_known_dav::{DavProtocol, well_known_dav_gate};

/// `/.well-known/webdav` — apex service discovery for the WebDAV files surface
/// (webdav-server.md § Network exposure & discovery).
///
/// Unlike the RFC-6764 CardDAV/CalDAV apexes (which redirect to the bridge
/// host's own `/.well-known/<proto>` for the cross-host well-known dance), the
/// WebDAV apex redirects **straight to the collection root**
/// `https://mail.<primary>/webdav/` — the NextCloud-ecosystem convention a
/// generic WebDAV client follows; there is deliberately no SRV record and no
/// bridge-side well-known hop.
///
/// Gating — the apex answers exactly when the MDA is actually serving WebDAV, so
/// a client is redirected iff there is a live surface to reach:
/// - **No primary mail domain** → `503` (no MDA host to serve — mail not enabled).
/// - **WebDAV explicitly disabled** (`set_webdav_enabled(false)`) → `503`.
/// - Otherwise → `301` → `https://mail.<primary>/webdav/`. `webdav_enabled` is
///   defaults-ON — unset falls back to `mail_enabled` (webdav-server.md
///   § Enablement; the same effective value `FetchConfigReply.webdav_enabled`
///   feeds the MDA), so a mail-enabled real-domain box answers without an
///   explicit toggle: the apex must not `503` while the MDA serves. (This gate
///   was once the only one of the three that got that right; the CalDAV and
///   CardDAV apexes were aligned to it on 2026-07-12.)
pub async fn wellknown_webdav(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    well_known_dav_gate(&state, DavProtocol::WebDav).await
}
