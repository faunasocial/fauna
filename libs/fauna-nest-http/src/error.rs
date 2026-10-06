//! The canonical error taxonomy for native Rust nest-HTTP calls.
//!
//! Four variants: the two failure channels every consumer already
//! distinguishes, plus the two verdicts that must never be retried.
//!
//! - [`ApiError::Status`] — the nest answered with a non-2xx. `message` is
//!   the nest's structured `{"error": "<msg>"}` text (every nest handler
//!   renders failures that way — `fauna_nest::api_error::ApiError`), or the
//!   raw body verbatim when the response was off-contract (HTML from a
//!   reverse proxy, an empty body, …). Surfaced as a per-endpoint API
//!   failure (e.g. Linux's `ActionResult::ApiFailed`, the wizard's
//!   `Invalid { reason }` / `Closed` / `RateLimited` states).
//! - [`ApiError::Transport`] — no bearer (the [`crate`]'s bearer source
//!   couldn't produce one), a connection / TLS failure, a timeout, or a
//!   body-read error. Surfaced as a generic failure (Linux's
//!   `ActionResult::Failed`, the wizard's `Transient { cause }`).
//! - [`ApiError::NestIdentityChanged`] — the nest's pinned deployment identity
//!   changed. Never a retry: consumers drop the bearer and block on the
//!   `launch_identity_changed` surface (`security.md` § Post-auth surfacing).
//! - [`ApiError::SignInRefused`] — the nest stopped signing a held identity in
//!   mid-session (suspended or removed). Never a retry: consumers route to the
//!   launch surface's previously-signed-in row.
//!
//! Lifted verbatim from `apps/fauna-linux/src/nest_content_api/types.rs`. The onboarding wizard's `nest_api` keeps its richer
//! per-endpoint error enums (each carries semantics the wizard needs —
//! `403 → admission closed`, `404 → invite-request gone`, …), but those are
//! refinements of this taxonomy — each carries a `From<ApiError>`
//! (design doc §5 / §6 decision 2).

use std::fmt;

/// Failure returned by a native Rust nest-HTTP call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The nest answered with a non-2xx status. `message` is the nest's
    /// structured `{"error": "<msg>"}` text, or the raw body verbatim when
    /// the response was off-contract (reverse-proxy / CDN HTML, empty body,
    /// …).
    Status { code: u16, message: String },
    /// No bearer available (the bearer source isn't ready and a refresh
    /// didn't recover it), a connection / TLS failure, a timeout, or the
    /// response body couldn't be read.
    Transport(String),
    /// The nest's **pinned deployment identity** changed (or a pinned nest could
    /// no longer prove any identity — the withdrawn/downgrade case): the
    /// `known_hosts` "REMOTE HOST IDENTIFICATION HAS CHANGED" verdict, raised by
    /// the bearer mint's channel-binding graduation.
    ///
    /// Its own variant because **it is the one failure here that must never be
    /// retried**: routed as [`Self::Transport`] it reads to a reconnect
    /// supervisor as a transient blip, so the client backs off and re-signs a
    /// doomed handshake forever while the user watches an indefinite
    /// "Connecting…" — a transient-retry loop on a MITM signal
    /// (`security.md` § Post-auth surfacing, § Connection-teardown rule).
    /// Consumers drop the bearer and route to the blocking
    /// `launch_identity_changed` surface.
    ///
    /// Carries exactly the field set `FfiError::NestIdentityChanged` renders in
    /// its warning's detail line — `host` + the two hex `nest_actor_id`
    /// fingerprints, `seen_hex` being `None` in the withdrawn case — so the FFI
    /// seam is a rename rather than a re-derivation. Rotation-chain `fork`
    /// evidence is deliberately *not* here: it has a home already, the launch
    /// machine's `LaunchSnapshot::identity_fork`, and the apps read it there.
    ///
    /// Contrast the supersession refusal, which rides a side channel
    /// (`SupersededLatch`) instead of widening this taxonomy: that one carries a
    /// whole wire `RpcError` whose `details` the import flow parses. Three owned
    /// strings describing the transport-trust verdict this taxonomy already sits
    /// in front of is a different weight class.
    NestIdentityChanged {
        host: String,
        pinned_hex: String,
        seen_hex: Option<String>,
    },
    /// The nest no longer signs this identity in (`fauna.auth.not_registered`)
    /// after the session had held a bearer — its user was suspended or removed
    /// mid-session; the client cannot tell which (no oracle on the wire).
    ///
    /// Its own variant for the reason [`Self::NestIdentityChanged`] has one: as
    /// [`Self::Transport`] a reconnect supervisor would retry it forever behind
    /// "Connecting…", where the honest answer is the launch surface's
    /// previously-signed-in row (`onboarding.md` § App-launch routing;
    /// `security.md` § Post-auth surfacing — the typed verdict must survive
    /// every seam). Raised by a bearer source that can tell a refusal from a
    /// fault — `LaunchMachineBearer`, off the machine's own verdict.
    SignInRefused,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Status { code, message } => write!(f, "HTTP {code}: {message}"),
            ApiError::Transport(cause) => f.write_str(cause),
            ApiError::NestIdentityChanged { host, .. } => write!(
                f,
                "{}",
                fauna_i18n::strings::errors::nest_identity_changed(host)
            ),
            ApiError::SignInRefused => {
                f.write_str(fauna_i18n::strings::onboarding::launch::SIGN_IN_REFUSED)
            }
        }
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    /// Whether this failure says nothing about the resource — the nest was
    /// unreachable, timed out, or answered that it could not serve *right now*
    /// (`408`, `429`, any `5xx`) — so the same request may well succeed later.
    /// Every other status is the nest's answer about the resource itself (a
    /// `404` blob stays absent, a `403` stays refused).
    ///
    /// [`Self::NestIdentityChanged`] is **never** transient: it is the one
    /// failure no retry may paper over (its own doc says why).
    ///
    /// The per-key load caches consult it to decide between remembering a
    /// failure and forgetting it (`fauna_core::load_cache::Finished`).
    pub fn is_transient(&self) -> bool {
        match self {
            ApiError::Transport(_) => true,
            ApiError::Status { code, .. } => matches!(code, 408 | 429 | 500..=599),
            ApiError::NestIdentityChanged { .. } | ApiError::SignInRefused => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_unreachable_or_overloaded_nest_is_transient() {
        let status = |code| ApiError::Status {
            code,
            message: String::new(),
        };
        assert!(ApiError::Transport("timed out".into()).is_transient());
        for code in [408, 429, 500, 502, 503, 504] {
            assert!(status(code).is_transient(), "{code} is transient");
        }
        for code in [400, 401, 403, 404, 410, 413] {
            assert!(!status(code).is_transient(), "{code} is the nest's answer");
        }
        assert!(
            !ApiError::NestIdentityChanged {
                host: "h".into(),
                pinned_hex: String::new(),
                seen_hex: None,
            }
            .is_transient()
        );
    }

    /// Regression: this used to be a hardcoded English sentence built by hand
    /// instead of going through the shared `LocalizedText`-style i18n path
    /// every other client error type uses (`fauna_client::NestClientError`,
    /// `fauna_anon_client::AnonClientError`).
    #[test]
    fn nest_identity_changed_display_is_localized_and_names_the_host() {
        let err = ApiError::NestIdentityChanged {
            host: "example.nest".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: Some("bb".repeat(32)),
        };
        let s = format!("{err}");
        assert_eq!(
            s,
            fauna_i18n::strings::errors::nest_identity_changed("example.nest")
        );
        assert!(s.contains("example.nest"), "got: {s}");
        assert!(!s.contains(&"aa".repeat(32)), "got: {s}");
        assert!(!s.contains(&"bb".repeat(32)), "got: {s}");
    }
}
