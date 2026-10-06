//! [`AnonClientError`] — the error type of [`AnonymousNestClient`](crate::AnonymousNestClient).
//!
//! A focused subset of `fauna-client`'s `NestClientError`: the anonymous
//! connector never does HTTP (`Http`/`Api`), authenticates nothing (`Auth`),
//! and carries no reconnect supervisor (`SubprotocolMismatch` is a supervisor
//! signal), so only the five RPC/transport variants the dispatch path can
//! actually produce are kept. `Display` for the two shared variants reuses the
//! same `fauna-i18n` strings the former `NestClientError` did, so user-visible
//! error text is unchanged after the extraction.

use std::fmt;

use fauna_protocol::RpcError;

/// Errors returned by [`AnonymousNestClient`](crate::AnonymousNestClient) methods.
#[derive(Debug)]
pub enum AnonClientError {
    /// Request/reply CBOR could not be encoded or decoded.
    Decode(String),
    /// WebSocket connection error (connect / handshake / framing / dispatch).
    WebSocket(String),
    /// Wire-level RPC error returned by the server (`Reply.ok = false`).
    Rpc(RpcError),
    /// The WS connection dropped while the request was outstanding.
    /// `was_in_flight` is true if the request had been sent on the wire before
    /// the disconnect.
    RpcDisconnected { was_in_flight: bool },
    /// Local timeout fired before the server replied.
    RpcTimeout,
    /// The connection failed first-contact trust graduation
    /// ([`AnonymousNestClient::graduate_first_contact`](crate::AnonymousNestClient::graduate_first_contact))
    /// — the channel binding did not verify, or the nest's identity did not
    /// match the pre-resolved root. The connection must be torn down; never
    /// retried as a transient (security.md § Connection-teardown rule).
    Trust(crate::trust::TrustError),
}

impl fmt::Display for AnonClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(msg) => write!(f, "{}", fauna_i18n::strings::errors::decode_error(msg)),
            Self::WebSocket(msg) => {
                write!(f, "{}", fauna_i18n::strings::errors::websocket_error(msg))
            }
            // Dimension 4: render the wire error's localized, code-keyed
            // message (`RpcError::localized`) — never the raw wire
            // `key`/`code`, which are protocol internals, not user-facing text.
            Self::Rpc(err) => write!(f, "{}", err.localized()),
            // `was_in_flight` is an internal retry hint, not user-facing —
            // both cases render the same localized "connection lost" text.
            Self::RpcDisconnected { .. } => {
                write!(f, "{}", fauna_i18n::strings::error::protocol::DISCONNECTED)
            }
            Self::RpcTimeout => write!(f, "{}", fauna_i18n::strings::error::protocol::TIMEOUT),
            Self::Trust(e) => write!(f, "nest trust: {e}"),
        }
    }
}

impl std::error::Error for AnonClientError {}

impl fauna_protocol::RpcErrorClass for AnonClientError {
    /// A wire-level `Rpc` error reached nest and was refused; every other
    /// variant (WS transport, decode, disconnect, timeout) is a transport
    /// fault. Lets the shared `WsRpcNestApi` mapping core distinguish a server
    /// rejection from a transport blip generically over `R::Error`.
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            AnonClientError::Rpc(e) => Some(e),
            _ => None,
        }
    }

    fn is_rejection(&self) -> bool {
        self.as_rpc_error().is_some()
    }
}

impl From<RpcError> for AnonClientError {
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
        let in_flight = format!(
            "{}",
            AnonClientError::RpcDisconnected {
                was_in_flight: true
            }
        );
        let not_in_flight = format!(
            "{}",
            AnonClientError::RpcDisconnected {
                was_in_flight: false
            }
        );
        assert_eq!(in_flight, not_in_flight);
        assert_eq!(
            in_flight,
            fauna_i18n::strings::error::protocol::DISCONNECTED
        );
    }

    /// Regression: `Self::Rpc`'s `Display` used to print the raw wire `key`
    /// and `code` — protocol internals, not user-facing text. Must render
    /// through `RpcError::localized()`, matching `fauna-client`'s
    /// `NestClientError` (the sibling this type was extracted from).
    #[test]
    fn rpc_display_renders_localized_not_raw_wire_fields() {
        let err = AnonClientError::Rpc(RpcError::new(
            "fauna.protocol.timeout",
            "error.protocol.timeout",
        ));
        let s = format!("{err}");
        assert_eq!(s, fauna_i18n::strings::error::protocol::TIMEOUT);
        assert!(!s.contains("fauna.protocol.timeout"), "got: {s}");
        assert!(!s.contains("error.protocol.timeout"), "got: {s}");
    }

    /// The nest's admin-debug
    /// `details` string must never reach the rendered error slot — the log
    /// (`RpcError::log_operator_details`, called at the construction site in
    /// `dispatch.rs`) is its only home.
    #[test]
    fn display_never_includes_details() {
        let pe = RpcError::new("fauna.protocol.internal", "error.protocol.internal")
            .with_details_text("SECRET_OPERATOR_DEBUG_STRING");
        let err = AnonClientError::Rpc(pe);
        assert!(!format!("{err}").contains("SECRET_OPERATOR_DEBUG_STRING"));
    }

    #[test]
    fn rpc_error_from_protocol_converts() {
        let pe = RpcError {
            code: "fauna.test.boom".into(),
            message: Box::new(LocalizedText::new("error.test.boom")),
            details: None,
            extra: Default::default(),
        };
        let ace: AnonClientError = pe.into();
        match ace {
            AnonClientError::Rpc(r) => assert_eq!(r.code, "fauna.test.boom"),
            other => panic!("expected Rpc, got {other:?}"),
        }
    }
}
