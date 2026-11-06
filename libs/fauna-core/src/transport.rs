//! Transport-layer constants shared by nest and the native apps.

/// Maximum size (bytes) of a single client↔nest WS-RPC message / frame on the
/// per-actor (`/api/v1/ws/{actor}`) and anonymous (`/api/v1/ws`) WebSocket
/// endpoints. 2 MiB.
///
/// These endpoints carry DAG-CBOR RPC frames only; bulk binary transfer
/// (chunks, manifests, folder sync) rides separate HTTP routes.
///
/// nest caps inbound frames at this size on its TLS WS upgrade
/// (`docs/goal/architecture/transport-connection.md` § Abuse posture — "WS upgrades cap
/// message/frame size"). The native clients (`fauna-client`,
/// `fauna-anon-client`) apply the SAME cap symmetrically on their tungstenite
/// `WebSocketConfig`, so a malicious or compromised nest cannot force unbounded
/// buffering on the client by sending an oversized frame (tungstenite's default
/// is 64 MiB message / 16 MiB frame). See review finding F-CL1 (tracked
/// internally).
pub const MAX_RPC_WS_MESSAGE_SIZE: usize = 2 * 1024 * 1024;

/// The bearer-authenticated WS-RPC endpoint: `{base}/api/v1/ws/{actor_id_hex}`
/// (`transport.md` § Connection lifecycle). `nest_url` is the nest's `http(s)`
/// URL; only its leading scheme is rewritten to `ws(s)`. The one owner of the
/// route, shared by the native client, the anonymous client and the wasm
/// adapter.
pub fn actor_ws_url(nest_url: &str, actor_id_hex: &str) -> String {
    let base = crate::web::http_to_ws(nest_url);
    format!("{base}/api/v1/ws/{actor_id_hex}")
}

/// The anonymous (pre-identity) WS-RPC endpoint: `{base}/api/v1/ws`, no
/// `{actor_id}` segment — an anonymous connection has no proven actor to key on
/// (`transport.md` § Pre-identity). Sibling of [`actor_ws_url`].
pub fn anon_ws_url(nest_url: &str) -> String {
    format!("{}/api/v1/ws", crate::web::http_to_ws(nest_url))
}

#[cfg(test)]
mod ws_url_tests {
    use super::*;

    #[test]
    fn actor_ws_url_swaps_scheme_and_appends_actor() {
        assert_eq!(
            actor_ws_url("https://nest.example.com", "abcd"),
            "wss://nest.example.com/api/v1/ws/abcd"
        );
        assert_eq!(
            actor_ws_url("http://localhost:8080", "ff"),
            "ws://localhost:8080/api/v1/ws/ff"
        );
    }

    #[test]
    fn anon_ws_url_swaps_scheme_and_omits_actor_id() {
        assert_eq!(
            anon_ws_url("https://nest.example.com"),
            "wss://nest.example.com/api/v1/ws"
        );
        assert_eq!(
            anon_ws_url("http://localhost:8080"),
            "ws://localhost:8080/api/v1/ws"
        );
    }
}
