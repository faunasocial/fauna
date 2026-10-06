//! The **shared rpc port** — how a wasm chunk other than the SPA's core chunk
//! rides the core chunk's one authenticated WebSocket instead of dialling its
//! own (`transport.md` § Goal: *a single WebSocket per actor*; the web shape is
//! `docs/goal/architecture/apps/web.md` § Transport).
//!
//! Vite gives every wasm chunk its own instantiated module and its own linear
//! memory, so a `WsRpcClient` built in the core chunk cannot be handed to the
//! folders / media / backups / labeler-catalog / atproto-settings chunks as a
//! Rust value, and the chunk discipline forbids passing a wasm-bindgen object
//! across for exactly that reason. What CAN cross is pure data over a typed
//! JS interface — the same shape the token provider and `JsMlsQuery` already
//! use. So the SPA (`rpc.ts`) implements [`SharedRpcPort`] over its singleton
//! `WsRpcClient`, and a chunk builds its `WsRpcClient` with
//! [`crate::WsRpcClient::over_port`], whose requests are canonical DAG-CBOR
//! bytes in, canonical DAG-CBOR bytes out, run by the core chunk's
//! `requestRaw` on the one socket through the same reconnect-wait and
//! per-kind deadline every core request gets. Refusals cross as the tagged
//! object [`WsRpcError::to_js`] writes and [`WsRpcError::from_js`] reads —
//! this crate is compiled into both chunks, so both sides of every crossing
//! are one definition.
//!
//! A port-built client has **no reconnect loop, no socket and no bearer of
//! its own**: the core client's loop is the only loop, so a chunk's request
//! issued in a reconnect gap waits it out exactly as the core's own requests
//! do, and comes back the moment the core client is up
//! (`transport-connection.md` § Connection lifecycle, the shared-port
//! paragraph). Before this, six chunk clients each ran their own jittered
//! backoff, so after a nest restart the app's `connection` observable could
//! read online while a page's machine was still asleep for up to a minute
//! and answered a gesture `not connected`.

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::client::ConnectionState;
use crate::error::WsRpcError;

/// The TypeScript declaration of the port, emitted into every consuming
/// chunk's `.d.ts` so the SPA implements ONE typed interface and each chunk's
/// constructor is typed `port: SharedRpcPort` (never `any`). The Rust
/// [`JsRpcPort`] bindings below are its mirror; the two are kept in step by
/// being in one file.
#[wasm_bindgen(typescript_custom_section)]
const SHARED_RPC_PORT_TS: &'static str = r#"
/**
 * The SPA core chunk's one authenticated WS-RPC socket, lent to another wasm
 * chunk as pure data in / pure data out. Implemented once by `$lib/rpc`
 * (`sharedRpcPort`) over the singleton `WsRpcClient`; consumed by every
 * page-machine chunk's constructor.
 */
export interface SharedRpcPort {
  /**
   * Run one request on the shared socket. `payload` and the resolved reply
   * are the request's / reply's canonical DAG-CBOR bytes; `idempotencyKey` is
   * the 16-byte envelope key. Rejects with the tagged object
   * `WsRpcError::to_js` writes (`{ arm, message?, rpc? }`).
   */
  request(kind: string, idempotencyKey: Uint8Array, payload: Uint8Array): Promise<Uint8Array>;
  /** A current bearer for the HTTP surfaces beside the socket (`forceRefresh` busts the cache). */
  bearer(forceRefresh: boolean): Promise<string>;
  /** The nest base URL the shared socket is connected on (its current value). */
  nestUrl(): string;
  /** The lowercase-hex actor id the shared socket authenticates as. */
  actorIdHex(): string;
  /** The shared socket's state word: `"connecting" | "connected" | "disconnected" | "unreachable"`. */
  connectionState(): string;
}
"#;

#[wasm_bindgen]
extern "C" {
    /// The Rust view of a [`SharedRpcPort`] (see the TypeScript section
    /// above): duck-typed, so the object the SPA hands over needs only the
    /// five methods. Arguments cross as JS-owned copies — a `Vec<u8>` import
    /// argument is copied out of wasm memory on the way over — so the port's
    /// implementation may hold them across its own awaits.
    #[wasm_bindgen(typescript_type = "SharedRpcPort")]
    pub type JsRpcPort;

    #[wasm_bindgen(method, catch)]
    fn request(
        this: &JsRpcPort,
        kind: &str,
        idempotency_key: Vec<u8>,
        payload: Vec<u8>,
    ) -> Result<js_sys::Promise, JsValue>;

    #[wasm_bindgen(method, catch)]
    fn bearer(this: &JsRpcPort, force_refresh: bool) -> Result<js_sys::Promise, JsValue>;

    #[wasm_bindgen(method, catch, js_name = nestUrl)]
    fn nest_url(this: &JsRpcPort) -> Result<String, JsValue>;

    #[wasm_bindgen(method, catch, js_name = actorIdHex)]
    fn actor_id_hex(this: &JsRpcPort) -> Result<String, JsValue>;

    #[wasm_bindgen(method, catch, js_name = connectionState)]
    fn connection_state(this: &JsRpcPort) -> Result<String, JsValue>;
}

/// The five methods a port must carry, checked at [`SharedPort::new`] so a
/// wrong object fails the constructor with a name instead of throwing
/// `undefined is not a function` out of the first request.
const REQUIRED_METHODS: [&str; 5] = [
    "request",
    "bearer",
    "nestUrl",
    "actorIdHex",
    "connectionState",
];

/// A chunk client's transport when it rides the core chunk's socket.
pub(crate) struct SharedPort {
    port: JsRpcPort,
    /// Read once at construction: the actor is fixed for the life of the
    /// singleton the port fronts (an identity change builds a new singleton,
    /// and with it new ports).
    actor_id_hex: String,
}

/// A port method that threw is a fault of the port glue, never a nest
/// refusal — but a rejection VALUE may be one of ours, so it goes through the
/// tagged decoder first, and only an unrecognised value becomes `Connect`.
fn port_fault(what: &str, value: JsValue) -> WsRpcError {
    match WsRpcError::from_js(value) {
        WsRpcError::Connect(msg) => WsRpcError::Connect(format!("shared rpc port `{what}`: {msg}")),
        other => other,
    }
}

impl SharedPort {
    /// Wrap the SPA's port object, refusing one that lacks any of the five
    /// methods ([`REQUIRED_METHODS`]).
    pub(crate) fn new(port: JsValue) -> Result<Self, WsRpcError> {
        if !port.is_object() {
            return Err(WsRpcError::Connect(
                "shared rpc port is not an object".into(),
            ));
        }
        for name in REQUIRED_METHODS {
            let present = js_sys::Reflect::get(&port, &JsValue::from_str(name))
                .map(|m| m.is_function())
                .unwrap_or(false);
            if !present {
                return Err(WsRpcError::Connect(format!(
                    "shared rpc port lacks the `{name}` method"
                )));
            }
        }
        let port: JsRpcPort = port.unchecked_into();
        let actor_id_hex = port
            .actor_id_hex()
            .map_err(|e| port_fault("actorIdHex", e))?;
        Ok(Self { port, actor_id_hex })
    }

    /// One request on the shared socket: canonical bytes in, canonical bytes
    /// out, the refusal decoded back into the owner's [`WsRpcError`].
    pub(crate) async fn request_bytes(
        &self,
        kind: &str,
        idempotency_key: [u8; 16],
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, WsRpcError> {
        let promise = self
            .port
            .request(kind, idempotency_key.to_vec(), payload)
            .map_err(|e| port_fault("request", e))?;
        let reply = JsFuture::from(promise).await.map_err(WsRpcError::from_js)?;
        if !reply.is_instance_of::<js_sys::Uint8Array>() {
            return Err(WsRpcError::Codec(format!(
                "shared rpc port resolved `{kind}` with a non-bytes reply"
            )));
        }
        Ok(js_sys::Uint8Array::new(&reply).to_vec())
    }

    /// A current bearer from the port's owner (its token cache; `force_refresh`
    /// busts it) — for the HTTP surfaces that ride beside the socket.
    pub(crate) async fn bearer(&self, force_refresh: bool) -> Result<String, WsRpcError> {
        let promise = self
            .port
            .bearer(force_refresh)
            .map_err(|e| port_fault("bearer", e))?;
        let value = JsFuture::from(promise)
            .await
            .map_err(|e| WsRpcError::Token(format!("shared rpc port bearer rejected: {e:?}")))?;
        value
            .as_string()
            .ok_or_else(|| WsRpcError::Token("shared rpc port returned a non-string bearer".into()))
    }

    /// The nest URL the shared socket is on, read live (the owner's loop
    /// SRV-swaps it). A port whose getter throws answers the empty string,
    /// logged — a blob URL built on it fails loudly at the fetch.
    pub(crate) fn nest_url(&self) -> String {
        match self.port.nest_url() {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!("shared rpc port `nestUrl` threw: {e:?}");
                String::new()
            }
        }
    }

    pub(crate) fn actor_id_hex(&self) -> &str {
        &self.actor_id_hex
    }

    /// The shared socket's state, as the owner reports it. An unrecognised
    /// word reads `Disconnected` — the honest weaker claim, the rule
    /// `fauna_core::format::connection_state_label` applies to the same words.
    pub(crate) fn connection_state(&self) -> ConnectionState {
        match self.port.connection_state() {
            Ok(word) => {
                ConnectionState::from_js_str(&word).unwrap_or(ConnectionState::Disconnected)
            }
            Err(e) => {
                tracing::warn!("shared rpc port `connectionState` threw: {e:?}");
                ConnectionState::Disconnected
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::RpcRequester;
    use wasm_bindgen_test::wasm_bindgen_test;

    use crate::WsRpcClient;

    // `run_in_browser` is configured once per test binary, by `adapter.rs`.

    /// A port whose `request` decodes the canonical payload it was given,
    /// echoes it back encoded, and records the kind — the SPA's port with the
    /// socket replaced by a mirror. `reject_with` plants a rejection instead.
    #[wasm_bindgen(inline_js = r#"
        export function echo_port(actor, state, reject_with) {
            return {
                calls: [],
                request(kind, idem, payload) {
                    this.calls.push({ kind, idem: idem.length, payload: payload.length });
                    if (reject_with !== undefined && reject_with !== null) {
                        return Promise.reject(reject_with);
                    }
                    // Hand back a COPY, as the core chunk's `requestRaw` would.
                    return Promise.resolve(new Uint8Array(payload));
                },
                bearer(force) { return Promise.resolve(force ? "fresh" : "cached"); },
                nestUrl() { return "http://nest.example:9"; },
                actorIdHex() { return actor; },
                connectionState() { return state; },
            };
        }
        export function port_missing_request() {
            return { bearer() {}, nestUrl() {}, actorIdHex() { return "00"; }, connectionState() {} };
        }
    "#)]
    extern "C" {
        fn echo_port(actor: &str, state: &str, reject_with: JsValue) -> JsValue;
        fn port_missing_request() -> JsValue;
    }

    fn actor() -> String {
        "ab".repeat(32)
    }

    /// A typed request over the port is encoded, forwarded with a 16-byte
    /// key, and its bytes decoded back into the typed reply — the round trip
    /// every chunk machine's requests now take.
    #[wasm_bindgen_test]
    async fn a_typed_request_rides_the_port_as_canonical_bytes() {
        let port = echo_port(&actor(), "connected", JsValue::NULL);
        let client = WsRpcClient::over_port(port.clone()).expect("a well-formed port");
        assert_eq!(client.actor_id_hex(), actor());
        assert_eq!(client.nest_url(), "http://nest.example:9");
        assert_eq!(client.connection_state(), ConnectionState::Connected);

        let reply: fauna_protocol::EchoReply = client
            .request(
                "fauna.protocol.echo",
                fauna_protocol::EchoRequest {
                    data: vec![1, 2, 3],
                    extra: Default::default(),
                },
            )
            .await
            .expect("the echo port answers");
        // `EchoReply` and `EchoRequest` share the `data` field, so the mirror
        // reads back as the reply.
        assert_eq!(reply.data, vec![1, 2, 3]);

        let calls = js_sys::Reflect::get(&port, &JsValue::from_str("calls")).unwrap();
        let call = js_sys::Array::from(&calls).get(0);
        let kind = js_sys::Reflect::get(&call, &JsValue::from_str("kind")).unwrap();
        assert_eq!(kind.as_string().as_deref(), Some("fauna.protocol.echo"));
        let idem = js_sys::Reflect::get(&call, &JsValue::from_str("idem")).unwrap();
        assert_eq!(idem.as_f64(), Some(16.0));
    }

    /// The owner's refusal crosses as the tagged object and decodes back to
    /// the same arm — every arm, the `Rpc` one carrying its wire error whole.
    #[wasm_bindgen_test]
    async fn every_refusal_arm_survives_the_crossing() {
        let rpc =
            fauna_protocol::RpcError::new("fauna.folders.not_found", "error.folders.not_found");
        let arms = [
            WsRpcError::Token("no bearer".into()),
            WsRpcError::Connect("port down".into()),
            WsRpcError::Codec("bad bytes".into()),
            WsRpcError::Rpc(Box::new(rpc.clone())),
            WsRpcError::Disconnected,
            WsRpcError::NotConnected,
            WsRpcError::SubprotocolMismatch,
            WsRpcError::Timeout,
        ];
        for arm in arms {
            let port = echo_port(&actor(), "connected", arm.to_js());
            let client = WsRpcClient::over_port(port).unwrap();
            let err = client
                .request::<(), fauna_protocol::EchoReply>("fauna.protocol.echo", ())
                .await
                .expect_err("the port rejects");
            assert_eq!(err.to_string(), arm.to_string(), "arm {arm:?}");
            if let WsRpcError::Rpc(decoded) = &err {
                assert_eq!(**decoded, rpc);
            } else {
                assert!(!matches!(arm, WsRpcError::Rpc(_)));
            }
        }
    }

    /// A rejection that is not ours — the SPA's glue threw — is a port fault,
    /// never mistaken for a nest refusal.
    #[wasm_bindgen_test]
    async fn a_foreign_rejection_is_a_port_fault() {
        let port = echo_port(&actor(), "connected", JsValue::from_str("TypeError: boom"));
        let client = WsRpcClient::over_port(port).unwrap();
        let err = client
            .request::<(), fauna_protocol::EchoReply>("fauna.protocol.echo", ())
            .await
            .expect_err("the port rejects");
        assert!(matches!(err, WsRpcError::Connect(_)), "got {err:?}");
        assert!(err.to_string().contains("boom"), "got {err}");
    }

    /// A port missing a method is refused by name at construction, so the
    /// mistake surfaces where the port was built rather than at a page's
    /// first gesture.
    #[wasm_bindgen_test]
    fn a_port_missing_a_method_is_refused_by_name() {
        // `WsRpcClient` carries no `Debug` (its `Own` arm holds JS callbacks),
        // so the refusal is taken by match rather than `expect_err`.
        fn refused(built: Result<WsRpcClient, WsRpcError>) -> WsRpcError {
            match built {
                Ok(_) => panic!("a malformed port was accepted"),
                Err(e) => e,
            }
        }
        let err = refused(WsRpcClient::over_port(port_missing_request()));
        assert!(err.to_string().contains("`request`"), "got {err}");
        let err = refused(WsRpcClient::over_port(JsValue::from_str("not an object")));
        assert!(err.to_string().contains("not an object"), "got {err}");
    }

    /// The bearer and the connection state are the owner's, read through the
    /// port — and a state word this chunk does not know reads `Disconnected`.
    #[wasm_bindgen_test]
    async fn bearer_and_state_are_the_owners() {
        let client =
            WsRpcClient::over_port(echo_port(&actor(), "unreachable", JsValue::NULL)).unwrap();
        assert_eq!(client.connection_state(), ConnectionState::Unreachable);
        assert_eq!(client.bearer(false).await.unwrap(), "cached");
        assert_eq!(client.bearer(true).await.unwrap(), "fresh");
        let future_word =
            WsRpcClient::over_port(echo_port(&actor(), "hibernating", JsValue::NULL)).unwrap();
        assert_eq!(
            future_word.connection_state(),
            ConnectionState::Disconnected
        );
    }
}
