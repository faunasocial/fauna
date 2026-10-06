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
