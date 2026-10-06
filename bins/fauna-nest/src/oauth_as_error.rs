//! The authorization server's refusal vocabulary — the RFC 6749 §5.2 /
//! RFC 9449 error codes, how HTTP spells each one, and the body they are
//! written in.
//!
//! Ported by value from the bridge's since-retired Go original (`oauth_par.go`'s
//! constant block and `parErrorStatus`) as part of TP5 S2: the endpoints moved
//! to the nest, and the wire a client reads must not move with them. A client
//! that retried a `use_dpop_nonce` against the bridge then and the nest now sees the same
//! code, the same status, and the same two JSON members.
//!
//! # Why the status table is a lookup and never an interpretation
//!
//! Which refusal happened is the shared policy modules' decision
//! (`fauna_bridge_atproto::{oauth_client, oauth_par, dpop, client_assertion}`
//! all answer with one of these codes). All this module owns is how HTTP
//! spells it. Keeping that split means a policy change never needs a matching
//! edit here, and a status here can never quietly re-classify a refusal the
//! policy already named.
//!
//! `docs/goal/behavior/authorization-server.md` § As built owns the postures
//! these codes carry; this module owns none of them.

use axum::response::{IntoResponse, Response};

// ── The codes ────────────────────────────────────────────────────────────────

/// This server could not complete the request for a reason that is its own.
pub const ERR_SERVER: &str = "server_error";
/// The surface exists but is not currently serving — the closed-world answer
/// when a seam this endpoint needs is unwired.
pub const ERR_TEMPORARILY_UNAVAILABLE: &str = "temporarily_unavailable";
/// The request is malformed or contradicts itself.
pub const ERR_INVALID_REQUEST: &str = "invalid_request";
/// The client could not be resolved, or could not prove it is itself.
pub const ERR_INVALID_CLIENT: &str = "invalid_client";
/// A scope this server will not grant — including a permission set it cannot
/// resolve.
pub const ERR_INVALID_SCOPE: &str = "invalid_scope";
/// RFC 9449's code for a proof that does not satisfy the DPoP rules.
pub const ERR_INVALID_DPOP_PROOF: &str = "invalid_dpop_proof";
/// RFC 9449's **retryable** code: the response carrying it also carries the
/// nonce to retry with, in `DPoP-Nonce`.
pub const ERR_USE_DPOP_NONCE: &str = "use_dpop_nonce";
/// RFC 6749 §5.2's code for a grant that is not redeemable — an unknown,
/// expired or already-spent authorization code, a dead refresh token, or a
/// credential presented with the wrong key. Every one of them answers with this
/// single code deliberately: distinguishing them would tell whoever holds a
/// stolen artifact which kind of dead it is, and the client's remedy is the
/// same in all of them (start a new authorization flow).
pub const ERR_INVALID_GRANT: &str = "invalid_grant";

/// RFC 8628 §3.5 and CIBA Core §11 — the four answers a polled consent start
/// gives at `/oauth/token` before (or instead of) tokens. Both specs spell them
/// identically, which is why the two starts share one poll path.
pub const ERR_AUTHORIZATION_PENDING: &str = "authorization_pending";
pub const ERR_SLOW_DOWN: &str = "slow_down";
pub const ERR_EXPIRED_TOKEN: &str = "expired_token";
pub const ERR_ACCESS_DENIED: &str = "access_denied";
/// RFC 6749 §5.2's code for a `grant_type` this server does not serve.
pub const ERR_UNSUPPORTED_GRANT_TYPE: &str = "unsupported_grant_type";
/// CIBA Core §13's code for a `binding_message` the server will not display.
pub const ERR_INVALID_BINDING_MESSAGE: &str = "invalid_binding_message";

/// The response header a DPoP-protected endpoint issues its nonce in.
pub const DPOP_NONCE_HEADER: &str = "DPoP-Nonce";
/// The request header a client presents its proof in.
pub const DPOP_PROOF_HEADER: &str = "DPoP";

// ── The refusal ──────────────────────────────────────────────────────────────

/// An OAuth refusal: a stable code and a human-readable description.
///
/// Mirrors the `{error, description}` pair every refusal in the shared policy
/// modules carries, and is the single shape every endpoint here refuses with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthDeny {
    pub error: String,
    pub description: String,
}

impl OAuthDeny {
    pub fn new(error: &str, description: impl Into<String>) -> Self {
        Self {
            error: error.to_string(),
            description: description.into(),
        }
    }

    /// The closed-world refusal: a seam this endpoint needs is not wired, so it
    /// declines to start a flow it could only fail later — after the user has
    /// been redirected.
    pub fn unavailable(description: impl Into<String>) -> Self {
        Self::new(ERR_TEMPORARILY_UNAVAILABLE, description)
    }

    /// A refusal this server owns. Used where a seam returned neither an
    /// acceptance nor a refusal — a broken seam refuses rather than guesses.
    pub fn server(description: impl Into<String>) -> Self {
        Self::new(ERR_SERVER, description)
    }

    /// The HTTP status this refusal is carried in.
    pub fn status(&self) -> axum::http::StatusCode {
        error_status(&self.error)
    }
}

/// The mechanical code → status table.
///
/// A lookup, never an interpretation: the policy owns which refusal happened,
/// this owns only how HTTP spells it. Anything unrecognised is `400`, which is
/// RFC 6749 §5.2's default for a request this server will not act on.
pub fn error_status(code: &str) -> axum::http::StatusCode {
    use axum::http::StatusCode;
    match code {
        // RFC 6749 §5.2: client authentication failures are 401.
        ERR_INVALID_CLIENT => StatusCode::UNAUTHORIZED,
        // RFC 9449 §5 and §8: an authorization-server endpoint answers both
        // with 400. The 401 spelling of `use_dpop_nonce` is the resource
        // server's, and using it here would tell a client to look for a
        // `WWW-Authenticate` challenge that is not coming.
        ERR_INVALID_DPOP_PROOF | ERR_USE_DPOP_NONCE => StatusCode::BAD_REQUEST,
        ERR_SERVER => StatusCode::INTERNAL_SERVER_ERROR,
        ERR_TEMPORARILY_UNAVAILABLE => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_REQUEST,
    }
}

/// Render a refusal as RFC 6749 §5.2's two-member JSON body.
///
/// `Cache-Control: no-store` on every one: a refusal can name a nonce, a
/// client, or a grant state that is true for exactly one request, and a shared
/// cache replaying it would answer a later request with a stale verdict.
pub fn oauth_error_response(deny: &OAuthDeny) -> Response {
    let body = serde_json::json!({
        "error": deny.error,
        "error_description": deny.description,
    });
    let mut response = (deny.status(), axum::Json(body)).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// The three statuses that are not the default, pinned against the Go
    /// original they were ported from. A client that hard-codes "401 means
    /// re-authenticate the client" must keep being right across the move.
    #[test]
    fn the_status_table_matches_the_bridge_it_replaces() {
        assert_eq!(error_status(ERR_INVALID_CLIENT), StatusCode::UNAUTHORIZED);
        assert_eq!(error_status(ERR_SERVER), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            error_status(ERR_TEMPORARILY_UNAVAILABLE),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// Both DPoP codes are 400 here even though `use_dpop_nonce` is 401 at a
    /// resource server. Pinned because the difference is exactly the kind a
    /// later edit "corrects" toward the spelling it saw elsewhere.
    #[test]
    fn both_dpop_codes_are_bad_request_at_an_authorization_server() {
        assert_eq!(
            error_status(ERR_INVALID_DPOP_PROOF),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(error_status(ERR_USE_DPOP_NONCE), StatusCode::BAD_REQUEST);
    }

    /// An unrecognised code must still refuse. The table is default-deny in the
    /// only sense a status table can be: no code falls through to a success.
    #[test]
    fn an_unknown_code_still_refuses() {
        assert_eq!(error_status("something_new"), StatusCode::BAD_REQUEST);
        assert!(error_status("").is_client_error());
    }
}
