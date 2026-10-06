//! Browser WebSocket transport for the WS-RPC façade (Phase 3 of the
//! WS-RPC adoption migration; tracked internally).
//!
//! Provides [`WsRpcClient`], a wasm implementation of
//! `fauna_protocol::RpcRequester` over a gloo-net browser `WebSocket`. The
//! shared per-feature wrappers (`fauna-client-bridges`, `fauna-client-email`)
//! are generic over `RpcRequester`, so the web SPA gets the same typed
//! `fauna.bridges.*` / `fauna.email.*` surface as native — only the transport
//! differs (native: tungstenite + tokio; here: browser WebSocket + spawn_local).
//!
//! Also home to [`post_multipart_blob`] — the browser twin of native
//! `fauna_nest_http`'s content API, for the bulk-binary `POST /api/v1/blob`
//! carve-out. It lives beside the transport because it is built from the same
//! [`WsRpcClient`] (single origin + JS bearer) that native builds its content
//! API from, and because one owner of the nest's strict multipart shape beats
//! one per uploader (see `blob_http`).
//!
//! The whole crate is wasm-only: on native it compiles to an empty rlib (it
//! pulls none of the browser deps). The SPA's core bundle (`libs/fauna-wasm`)
//! consumes it and re-exposes a typed handle via `#[wasm_bindgen]` (Phase 4).
#![cfg(target_arch = "wasm32")]

mod adapter;
mod anonymous;
mod blob_http;
mod client;
mod error;
mod fixed_conn;
mod identity;
mod shared_port;
mod token_client;

pub use adapter::ReconnectSignal;
pub use anonymous::AnonymousWsRpcClient;
pub use blob_http::{
    BLOB_UPLOAD_PATH, BlobHttpError, CHUNKS_UPLOAD_PATH, MANIFESTS_UPLOAD_PATH,
    post_multipart_blob, post_multipart_blob_with_bearer, post_octets_keyed,
};
#[cfg(feature = "test-helpers")]
pub use client::connection_reports_json;
pub use client::{ConnectionState, WsRpcClient};
pub use error::WsRpcError;
pub use identity::{arc_from_secret_hex, keypair_from_secret_hex};
pub use shared_port::JsRpcPort;
pub use token_client::TokenWsRpcClient;
