//! CardDAV discovery redirect — the only in-nest CardDAV HTTP surface.
//!
//! The real CardDAV store lives on the encrypted `bridge_carddav_*` path served
//! by the mail-bridge MDA (`mail.<primary>:443`, behind the SNI router). Unlike
//! CalDAV there was never an in-core plaintext PROPFIND/GET/PUT path: CardDAV is
//! seal-always, MDA-only from the start (`carddav-server` design § 7 — vCards
//! are written only by authenticated clients, no plaintext-at-rest purpose). All
//! that lives here is RFC 6764 service discovery:
//! - PROPFIND/GET /.well-known/carddav → `301` to the bridge host's own
//!   `/.well-known/carddav` (cross-host bootstrap), gated on CardDAV being
//!   enabled.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;

use crate::routes::AppState;
use crate::well_known_dav::{DavProtocol, well_known_dav_gate};

/// `/.well-known/carddav` — RFC 6764 service discovery for the CardDAV surface.
///
/// CardDAV is served by the mail-bridge MDA at `mail.<primary_domain>:443`
/// (behind the SNI router), never in-core, and it is **seal-always** — there is
/// no in-core `207` branch: discovery always `301`s to the bridge host.
/// Per RFC 6764 cross-host bootstrap, the apex `301`s to the bridge host's own
/// `/.well-known/carddav`, which (post-auth) redirects on to `/carddav/{user}/`,
/// letting a MUA be pointed at the bare apex `<domain>` and reach the right host.
///
/// Gating — the `_carddavs._tcp` SRV is a standing DNS row, but the apex only
/// *answers* once CardDAV is actually live, and it must not `503` while the MDA
/// serves:
/// - **CardDAV explicitly disabled** (`set_carddav_enabled(false)`) → `503`.
/// - **No primary mail domain** → `503` (mail not enabled → no MDA host to serve).
/// - Otherwise → `301` → `https://mail.<primary_domain>/.well-known/carddav`.
///
/// `carddav_enabled` is defaults-ON: an **unset** toggle inherits `mail_enabled`
/// (carddav-server.md § Independent enablement — `get_carddav_enabled()
/// .unwrap_or(mail_enabled)`, the same effective value `FetchConfigReply
/// .carddav_enabled` feeds the MDA, which "mounts `/carddav/` from boot" on it).
/// This gate previously required an explicit `Some(true)`, so a mail-enabled box
/// whose admin had never touched the toggle served CardDAV on the MDA while its
/// apex discovery answered `503` — the liveness predicate is
/// `unwrap_or(mail_enabled)`, not `Some(true)`.
pub async fn wellknown_carddav(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    well_known_dav_gate(&state, DavProtocol::CardDav).await
}
