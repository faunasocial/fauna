use std::fmt;

use fauna_protocol::RpcError;

/// Errors returned by NestClient methods.
#[derive(Debug)]
pub enum NestClientError {
    /// Authentication failed (invalid signature, unregistered, suspended).
    Auth(String),
    /// HTTP transport error (network, TLS, timeout).
    Http(reqwest::Error),
    /// Server returned an error status code on an HTTP route.
    Api { status: u16, message: String },
    /// Response body could not be parsed.
    Decode(String),
    /// WebSocket connection error (connect / handshake / framing).
    WebSocket(String),

    // ── Spec Y, § 3.8: RPC errors ──
    /// Wire-level RPC error returned by the server (Reply.ok = false).
    Rpc(RpcError),
    /// The WS connection dropped while the request was outstanding.
    /// `was_in_flight` is true if the request had been sent on the wire
    /// before the disconnect (caller may want to re-issue with the same
    /// idempotency_key); false if the request never went out.
    RpcDisconnected { was_in_flight: bool },
    /// Local timeout fired before the server replied.
    RpcTimeout,
    /// Server closed with WS code 4426 — client crate version is incompatible
    /// with the nest's supported subprotocols.
    SubprotocolMismatch,
    /// The nest's **pinned deployment identity** changed mid-session — the
    /// `known_hosts` verdict, raised by the bearer mint's channel-binding
    /// graduation and carried here rather than flattened into
    /// [`Self::Auth`]/[`Self::WebSocket`].
    ///
    /// Terminal for the reconnect supervisor
    /// ([`crate::reconnect::ClientChannel::connect_error_is_terminal`]): no
    /// re-mint and no backoff can clear it, and retrying it is retrying a
    /// possible MITM. The app drops the bearer and blocks on the
    /// `launch_identity_changed` surface (`security.md` § Post-auth surfacing).
    ///
    /// Fields mirror `ApiError::NestIdentityChanged`, which mirrors
    /// `FfiError::NestIdentityChanged`'s detail line — one field set the whole
    /// way out, so no seam has to re-derive it.
    NestIdentityChanged {
        host: String,
        pinned_hex: String,
        seen_hex: Option<String>,
    },
}

impl fmt::Display for NestClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth(msg) => write!(f, "{}", fauna_i18n::strings::errors::auth_error(msg)),
            Self::Http(err) => write!(
                f,
                "{}",
                fauna_i18n::strings::errors::http_error(&err.to_string())
            ),
            Self::Api { status, message } => write!(
                f,
                "{}",
                fauna_i18n::strings::errors::api_error(&status.to_string(), message)
            ),
            Self::Decode(msg) => write!(f, "{}", fauna_i18n::strings::errors::decode_error(msg)),
            Self::WebSocket(msg) => {
                write!(f, "{}", fauna_i18n::strings::errors::websocket_error(msg))
            }
            // Dimension 4: render the wire error's localized, code-keyed message
            // (`RpcError::localized`) — never the raw wire `key`/`code`, which
            // are protocol internals, not user-facing text.
            Self::Rpc(err) => write!(f, "{}", err.localized()),
            // `was_in_flight` is an internal retry hint for the reconnect
            // supervisor (see the variant's doc comment), not user-facing —
            // both cases render the same localized "connection lost" text,
            // mirroring the wire-level `fauna.protocol.disconnected` string.
            Self::RpcDisconnected { .. } => {
                write!(f, "{}", fauna_i18n::strings::error::protocol::DISCONNECTED)
            }
            Self::RpcTimeout => write!(f, "{}", fauna_i18n::strings::error::protocol::TIMEOUT),
            Self::SubprotocolMismatch => {
                write!(f, "{}", fauna_i18n::strings::errors::SUBPROTOCOL_MISMATCH)
            }
            Self::NestIdentityChanged { host, .. } => write!(
                f,
                "{}",
                fauna_i18n::strings::errors::nest_identity_changed(host)
            ),
        }
    }
}

impl std::error::Error for NestClientError {}

impl fauna_protocol::RpcErrorClass for NestClientError {
    /// A wire-level `Rpc` error reached nest and was refused; every other
    /// variant (auth, HTTP/WS transport, decode, disconnect, timeout,
    /// subprotocol) is a transport fault. Mirrors the per-app `map_admin_err`
    /// the shared `Rpc*Nest` glue replaces.
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            NestClientError::Rpc(e) => Some(e),
            _ => None,
        }
    }

    fn is_rejection(&self) -> bool {
        self.as_rpc_error().is_some()
    }
}

/// A post-auth verdict a signed-in session cannot survive — each one routed
/// by the apps to the launch surface, never a banner (`security.md`
/// § Post-auth surfacing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndingVerdict {
    /// The nest's pinned deployment identity changed.
    NestIdentityChanged,
    /// This identity was succeeded (`fauna.auth.superseded`).
    Superseded,
    /// The nest stopped signing this identity in (`fauna.auth.not_registered`)
    /// — suspended or removed while signed in.
    SignInRefused,
}

impl NestClientError {
    /// Which session-ending verdict this failure is, if any. The one shared
    /// reading of [`crate::NestClient::supervisor_stop`] the apps' routing
    /// keys on, so no app matches wire codes of its own.
    pub fn session_ending_verdict(&self) -> Option<SessionEndingVerdict> {
        match self {
            Self::NestIdentityChanged { .. } => Some(SessionEndingVerdict::NestIdentityChanged),
            Self::Rpc(e) if e.code == RpcError::CODE_SUPERSEDED => {
                Some(SessionEndingVerdict::Superseded)
            }
            Self::Rpc(e) if e.is_not_registered() => Some(SessionEndingVerdict::SignInRefused),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for NestClientError {
    fn from(err: reqwest::Error) -> Self {
        Self::Http(err)
    }
}

impl From<RpcError> for NestClientError {
    fn from(err: RpcError) -> Self {
        Self::Rpc(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::LocalizedText;

    #[test]
    fn rpc_disconnected_display_is_localized_regardless_of_flag() {
        // `was_in_flight` is an internal retry hint, not user-facing — both
        // values must render the same localized text, never a raw debug
        // format with the flag baked in.
        let in_flight = format!(
            "{}",
            NestClientError::RpcDisconnected {
                was_in_flight: true
            }
        );
        let not_in_flight = format!(
            "{}",
            NestClientError::RpcDisconnected {
                was_in_flight: false
            }
        );
        assert_eq!(in_flight, not_in_flight);
        assert_eq!(
            in_flight,
            fauna_i18n::strings::error::protocol::DISCONNECTED
        );
    }

    #[test]
    fn rpc_error_from_protocol_converts() {
        let pe = RpcError {
            code: "fauna.test.boom".into(),
            message: Box::new(LocalizedText::new("error.test.boom")),
            details: None,
            extra: Default::default(),
        };
        let nce: NestClientError = pe.into();
        match nce {
            NestClientError::Rpc(r) => assert_eq!(r.code, "fauna.test.boom"),
            other => panic!("expected Rpc, got {other:?}"),
        }
    }

    #[test]
    fn timeout_and_subprotocol_mismatch_display_are_localized() {
        assert_eq!(
            format!("{}", NestClientError::RpcTimeout),
            fauna_i18n::strings::error::protocol::TIMEOUT
        );
        assert_eq!(
            format!("{}", NestClientError::SubprotocolMismatch),
            fauna_i18n::strings::errors::SUBPROTOCOL_MISMATCH
        );
    }

    /// Regression: `Self::Rpc`'s `Display` used to print the raw wire `key`
    /// and numeric-looking `code` (`"rpc error: fauna.x.y (error.x.y)"`) —
    /// protocol internals, not user-facing text. It must render through
    /// `RpcError::localized()` like every other client error type does.
    #[test]
    fn rpc_display_renders_localized_not_raw_wire_fields() {
        let err = NestClientError::Rpc(RpcError::new(
            "fauna.protocol.timeout",
            "error.protocol.timeout",
        ));
        let s = format!("{err}");
        assert_eq!(s, fauna_i18n::strings::error::protocol::TIMEOUT);
        assert!(!s.contains("fauna.protocol.timeout"), "got: {s}");
        assert!(!s.contains("error.protocol.timeout"), "got: {s}");
    }

    #[test]
    fn nest_identity_changed_display_names_the_host() {
        let err = NestClientError::NestIdentityChanged {
            host: "example.nest".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: Some("bb".repeat(32)),
        };
        let s = format!("{err}");
        assert!(s.contains("example.nest"), "got: {s}");
        // The raw pinned/seen hex digests are diagnostic detail, not
        // user-facing — never leak them into the rendered message.
        assert!(!s.contains(&"aa".repeat(32)), "got: {s}");
        assert!(!s.contains(&"bb".repeat(32)), "got: {s}");
    }

    /// Contract half: the nest's admin-debug
    /// `details` string must never reach the rendered error slot — the log
    /// (`RpcError::log_operator_details`, called at the construction site in
    /// `client.rs`) is its only home.
    #[test]
    fn display_never_includes_details() {
        let pe = RpcError::new("fauna.protocol.internal", "error.protocol.internal")
            .with_details_text("SECRET_OPERATOR_DEBUG_STRING");
        let err = NestClientError::Rpc(pe);
        assert!(!format!("{err}").contains("SECRET_OPERATOR_DEBUG_STRING"));
    }
}
