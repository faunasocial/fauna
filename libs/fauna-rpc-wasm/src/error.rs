//! `WsRpcError` — the wasm transport's `RpcRequester::Error`.
//!
//! Mirrors the RPC-relevant arms of `fauna_client::NestClientError` (native);
//! the two stay aligned by hand for now (a shared error type would have to
//! leave `fauna-client`, which carries the native-only `reqwest::Error` arm).

use fauna_protocol::{RpcError, RpcErrorClass};
use wasm_bindgen::{JsCast, JsValue};

/// Errors surfaced by the wasm `WsRpcClient`.
#[derive(Debug, thiserror::Error)]
pub enum WsRpcError {
    /// Token-provider callback failed or returned a non-string.
    #[error("token provider: {0}")]
    Token(String),
    /// WebSocket open / handshake failure.
    #[error("ws connect: {0}")]
    Connect(String),
    /// CBOR encode/decode of the request payload or reply.
    #[error("codec: {0}")]
    Codec(String),
    /// Wire-level RPC error returned by the server (`Reply.ok = false`).
    /// Boxed — `RpcError` carries an `Option<serde_json::Value>` that made
    /// this the fattest arm by far, bloating every `Result<_, WsRpcError>`
    /// (including the common `Ok` path) on the size-sensitive wasm bundle.
    // Dimension 4: render the wire error's localized, code-keyed message
    // (`RpcError::localized`) — never the raw wire `key`/`code`, which are
    // protocol internals, not user-facing text.
    #[error("{}", .0.localized())]
    Rpc(Box<RpcError>),
    /// The WS connection dropped while the request was outstanding.
    #[error("{}", fauna_i18n::strings::error::protocol::DISCONNECTED)]
    Disconnected,
    /// No live dispatcher — not connected yet, or mid-reconnect.
    #[error("not connected")]
    NotConnected,
    /// The nest rejected this client's subprotocol: a version skew no retry
    /// clears. Only a reconnect loop that stopped on a 4426 close reports it
    /// (`WsRpcClient::supervisor_stop`). Mirrors native
    /// `NestClientError::SubprotocolMismatch` and shares its string.
    #[error("{}", fauna_i18n::strings::errors::SUBPROTOCOL_MISMATCH)]
    SubprotocolMismatch,
    /// Local deadline fired before the server replied.
    #[error("{}", fauna_i18n::strings::error::protocol::TIMEOUT)]
    Timeout,
}

impl From<RpcError> for WsRpcError {
    fn from(err: RpcError) -> Self {
        Self::Rpc(Box::new(err))
    }
}

// ── The shared-port error crossing ────────────────────────────────────────
//
// A wasm chunk that borrows the SPA core chunk's socket through the shared
// rpc port (`crate::shared_port`) gets its request refusals back across a JS
// promise rejection, and the two chunks share no memory — so the refusal
// crosses as pure data, a tagged JS object, and this crate is compiled into
// BOTH chunks, so the encoder and the decoder are the same two functions on
// both sides and cannot drift (`transport.md` § Design decisions, the
// wasm-chunk single-socket entry). Everything an arm carries is reproduced:
// a `String` arm keeps its message, and `Rpc` carries the wire `RpcError`
// itself as canonical DAG-CBOR bytes (it is a wire type, so its own codec is
// the one that already exists) — the chunk that decodes it renders the same
// localized text, the same `RpcErrorClass` verdict and the same
// `NestSeamError` classification the core chunk would have.

/// The rejection object's tag key: which [`WsRpcError`] arm this is.
const ARM_KEY: &str = "arm";
/// The rejection object's message key, set for the arms that carry one.
const MESSAGE_KEY: &str = "message";
/// The rejection object's `Rpc` payload key: the `RpcError`, canonical CBOR.
const RPC_KEY: &str = "rpc";

impl WsRpcError {
    /// The rejection tag of this arm — one word per variant, the same table
    /// [`Self::from_js`] reads.
    fn arm_name(&self) -> &'static str {
        match self {
            Self::Token(_) => "token",
            Self::Connect(_) => "connect",
            Self::Codec(_) => "codec",
            Self::Rpc(_) => "rpc",
            Self::Disconnected => "disconnected",
            Self::NotConnected => "not_connected",
            Self::SubprotocolMismatch => "subprotocol_mismatch",
            Self::Timeout => "timeout",
        }
    }

    /// Encode this error as the tagged JS object a shared rpc port's
    /// `request` promise rejects with: `{ arm, message?, rpc? }`, where `rpc`
    /// is the wire `RpcError` in canonical DAG-CBOR (a `Uint8Array`). Pure
    /// data — nothing wasm-bindgen-owned crosses the chunk boundary. The
    /// inverse is [`Self::from_js`].
    pub fn to_js(&self) -> JsValue {
        let obj = js_sys::Object::new();
        let set = |key: &str, value: JsValue| {
            // `Reflect::set` on a fresh plain object cannot fail.
            let _ = js_sys::Reflect::set(&obj, &JsValue::from_str(key), &value);
        };
        set(ARM_KEY, JsValue::from_str(self.arm_name()));
        match self {
            Self::Token(m) | Self::Connect(m) | Self::Codec(m) => {
                set(MESSAGE_KEY, JsValue::from_str(m));
            }
            Self::Rpc(rpc) => match fauna_protocol::encode_canonical(&**rpc) {
                Ok(bytes) => set(RPC_KEY, js_sys::Uint8Array::from(&bytes[..]).into()),
                // A wire type that cannot encode is a codec fault of ours, not
                // a refusal of the nest's; say so rather than send a bare tag.
                Err(e) => {
                    set(ARM_KEY, JsValue::from_str("codec"));
                    set(
                        MESSAGE_KEY,
                        JsValue::from_str(&format!("encode RpcError for the shared port: {e}")),
                    );
                }
            },
            Self::Disconnected | Self::NotConnected | Self::SubprotocolMismatch | Self::Timeout => {
            }
        }
        obj.into()
    }

    /// Decode a shared rpc port's rejection back into the error the port's
    /// owner refused with — the inverse of [`Self::to_js`]. A rejection that is
    /// not ours (the SPA's port glue threw, or its promise rejected with an
    /// `Error`) is a transport fault of the port itself, reported as
    /// [`Self::Connect`] carrying the JS value's string form so the console
    /// names what went wrong; it is never mistaken for a nest refusal.
    pub fn from_js(value: JsValue) -> Self {
        let field = |key: &str| js_sys::Reflect::get(&value, &JsValue::from_str(key)).ok();
        let message = || {
            field(MESSAGE_KEY)
                .and_then(|m| m.as_string())
                .unwrap_or_default()
        };
        let arm = field(ARM_KEY).and_then(|a| a.as_string());
        match arm.as_deref() {
            Some("token") => Self::Token(message()),
            Some("connect") => Self::Connect(message()),
            Some("codec") => Self::Codec(message()),
            Some("rpc") => {
                let Some(bytes) =
                    field(RPC_KEY).filter(|b| b.is_instance_of::<js_sys::Uint8Array>())
                else {
                    return Self::Codec("shared port rejected `rpc` with no RpcError bytes".into());
                };
                let bytes = js_sys::Uint8Array::new(&bytes).to_vec();
                match fauna_protocol::decode_strict::<RpcError>(&bytes) {
                    Ok(rpc) => Self::Rpc(Box::new(rpc)),
                    Err(e) => Self::Codec(format!("decode RpcError from the shared port: {e}")),
                }
            }
            Some("disconnected") => Self::Disconnected,
            Some("not_connected") => Self::NotConnected,
            Some("subprotocol_mismatch") => Self::SubprotocolMismatch,
            Some("timeout") => Self::Timeout,
            // Not a rejection this crate wrote: the port glue itself failed.
            _ => Self::Connect(format!(
                "shared rpc port failed: {}",
                js_sys::JSON::stringify(&value)
                    .ok()
                    .and_then(|s| s.as_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("{value:?}"))
            )),
        }
    }
}

/// Mirrors the native `impl RpcErrorClass for NestClientError`: only a
/// wire-level `Reply.ok = false` ([`WsRpcError::Rpc`]) reached nest and was
/// refused; every other arm (token/connect/codec/disconnect/timeout) is a
/// transport fault. Lets the shared `Rpc*Nest` seam glue
/// (`fauna-client-dns` / `fauna-client-mail-settings`) classify `R::Error` into
/// the two-class `NestError` without knowing the concrete transport.
impl RpcErrorClass for WsRpcError {
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            WsRpcError::Rpc(e) => Some(e),
            _ => None,
        }
    }

    fn is_rejection(&self) -> bool {
        self.as_rpc_error().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract half: the nest's admin-debug
    /// `details` string must never reach the rendered error slot — the log
    /// (`RpcError::log_operator_details`, called at the construction site in
    /// `client.rs`) is its only home.
    #[test]
    fn display_never_includes_details() {
        let pe = RpcError::new("fauna.protocol.internal", "error.protocol.internal")
            .with_details_text("SECRET_OPERATOR_DEBUG_STRING");
        let err = WsRpcError::Rpc(Box::new(pe));
        assert!(!format!("{err}").contains("SECRET_OPERATOR_DEBUG_STRING"));
    }

    /// Regression: `Rpc`'s `Display` used to print the raw wire `key`/`code`
    /// — protocol internals, not user-facing text. Must render through
    /// `RpcError::localized()`, matching the native `NestClientError`/
    /// `AnonClientError` siblings this type mirrors.
    #[test]
    fn rpc_display_renders_localized_not_raw_wire_fields() {
        let err = WsRpcError::Rpc(Box::new(RpcError::new(
            "fauna.protocol.timeout",
            "error.protocol.timeout",
        )));
        let s = format!("{err}");
        assert_eq!(s, fauna_i18n::strings::error::protocol::TIMEOUT);
        assert!(!s.contains("fauna.protocol.timeout"), "got: {s}");
        assert!(!s.contains("error.protocol.timeout"), "got: {s}");
    }

    #[test]
    fn disconnected_and_timeout_display_are_localized() {
        assert_eq!(
            format!("{}", WsRpcError::Disconnected),
            fauna_i18n::strings::error::protocol::DISCONNECTED
        );
        assert_eq!(
            format!("{}", WsRpcError::Timeout),
            fauna_i18n::strings::error::protocol::TIMEOUT
        );
    }
}
