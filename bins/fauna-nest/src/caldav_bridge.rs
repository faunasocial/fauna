//! CalDAV discovery redirect — the only in-nest CalDAV HTTP surface.
//!
//! The real CalDAV store lives on the `bridge_caldav_*` path served by the
//! mail-bridge MDA (`mail.<primary>:443`, behind the SNI router). The legacy
//! in-core plaintext PROPFIND/REPORT/GET/PUT/DELETE handlers over the `content`
//! table were retired in the Decision-B § 4c cleanup (events.md /
//! caldav-server.md § Implementation status today). All that remains here is RFC
//! 6764 service discovery:
//! - PROPFIND/GET /.well-known/caldav → `301` to the bridge host's own
//!   `/.well-known/caldav` (cross-host bootstrap), gated on CalDAV being live.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;

use crate::routes::AppState;
use crate::well_known_dav::{DavProtocol, well_known_dav_gate};

/// `/.well-known/caldav` — RFC 6764 service discovery for the CalDAV surface.
///
/// The MDA is the *only* CalDAV server (caldav-server.md § Independent
/// enablement) — the in-core plaintext store this apex once advertised as a
/// `207 Multi-Status` collection was retired with the § 4c cleanup, and calendar
/// bodies rest sealed in `__calendar` since Phase 3. A `207` would name a
/// collection the nest does not serve, dead-ending a MUA that was pointed at the
/// bare apex, so discovery `301`s — as the CardDAV
/// ([`crate::carddav_bridge::wellknown_carddav`]) and WebDAV
/// ([`crate::webdav_bridge::wellknown_webdav`]) siblings already do.
///
/// Per RFC 6764 cross-host bootstrap the apex `301`s to the bridge host's own
/// `/.well-known/caldav`, which (post-auth) redirects on to `/caldav/{user}/` —
/// so a MUA can be pointed at the bare apex `<domain>` and reach the right host.
///
/// Gating — the apex answers exactly when the MDA is actually serving CalDAV, so
/// a client is redirected iff there is a live surface to reach:
/// - **CalDAV explicitly disabled** (`set_caldav_enabled(false)`) → `503`.
/// - **CalDAV unset AND effective mail off** → `503` (unset follows the mail
///   toggle, and Stage-5 default-off makes an unset mail toggle OFF — a
///   registered primary mail domain is no longer proof of enablement, since
///   claim auto-registers the handle domain before any enable).
/// - **No primary mail domain** → `503` (no MDA host to point at).
/// - Otherwise → `301` → `https://mail.<primary>/.well-known/caldav`.
///
/// `caldav_enabled` unset falls back to `mail_enabled` (§ Independent
/// enablement; the same effective value `FetchConfigReply.caldav_enabled`
/// feeds the MDA), so a mail-enabled box answers without an explicit CalDAV
/// toggle — the apex must not `503` while the MDA serves.
pub async fn wellknown_caldav(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    well_known_dav_gate(&state, DavProtocol::CalDav).await
}
