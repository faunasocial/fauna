//! Shared registration/discovery helpers.
//!
//! The discovery HTTP twins (`handle-available` / `node-info` / `resolve-node`
//! / `actor/by-handle`) migrated to the pre-identity WS-RPC kinds
//! `fauna.{handle.available, nest.info, nest.resolve, actor.by_handle}`
//! (`discovery_handlers`, shared `discovery_core::*`); they were deleted in the
//! WS-RPC-everywhere rip-out. `POST
//! /api/v1/register` was already retired (S4f) in favour of the pre-identity
//! `fauna.account.register` kind.
//!
//! What remains here is the transport-agnostic helpers other call sites still
//! share: `validate_handle` (handle format rules, used by the account/claim/
//! invite cores) and `OptionalConnectInfo` (the client-IP extractor used by the
//! HTTP routes that stay — byte-bulk, lockout, etc.).

/// Extractor that optionally provides the client's IP address.
/// Populated on both the plain-HTTP path (`into_make_service_with_connect_info`)
/// and the TLS path (`serve_tls`'s `WithConnectInfo`, where it's the PROXY-v2
/// client IP conveyed by the SNI router, or the direct loopback peer). Returns
/// `None` only when no `ConnectInfo` is present (e.g. unit tests).
pub struct OptionalConnectInfo(pub Option<std::net::SocketAddr>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for OptionalConnectInfo {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let addr = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0);
        Ok(Self(addr))
    }
}

/// Validate a handle against format rules. Returns an error message if invalid.
///
/// The canonical rules now live in the shared `fauna_protocol::handle` module
/// (one source of truth for the nest *and* every app — see that module's
/// docs); this re-export keeps the nest's existing call sites (account register,
/// claim, invite, discovery, profile handle-change) pointed at it.
pub use fauna_protocol::handle::validate_handle;
