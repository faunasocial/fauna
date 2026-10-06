//! Unified async Rust client for communicating with fauna-nest servers.
//!
//! Provides [`AuthClient`] for shared authentication and [`NestClient`]
//! for messaging API + WebSocket push events.

pub mod auth_client;
pub mod chunk_download;
pub mod client;
pub mod error;
pub mod media_upload;
pub mod push;
mod reconnect;
pub mod remote_image;
/// Test doubles for driving a real [`NestClient`] with a mocked socket. Behind
/// the `test-util` feature, so nothing here can reach a release artifact.
#[cfg(feature = "test-util")]
pub mod testing;
mod token_cache;
pub mod types;
pub mod update_look;
pub mod write_token_bearer;
pub mod ws_adapter;
pub mod ws_challenge_bearer;
pub mod ws_custody_handshake_bearer;
pub mod ws_device_handshake_bearer;

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub use auth_client::HeldBearerForTest;
pub use auth_client::{AuthClient, BearerIdentityMismatch, pinned_http_client};
pub use chunk_download::{
    ForeignPublicChunkFetcher, NestAuthedByteStream, NestPublicChunkFetcher,
    graduate_home_nest_pin, open_authed_stream,
};
pub use client::{NestClient, SelfConnecting};
pub use error::{NestClientError, SessionEndingVerdict};
/// The channel-binding verification core moved to `fauna-anon-client` (the
/// lowest crate both native bearer paths share); re-exported here so existing
/// `fauna_client::cert_binding::…` paths keep resolving.
pub use fauna_anon_client::cert_binding;
/// Process-wide TLS-trust state (`install_pin_store`, `graduate_handshake`,
/// `pinned_spki`). Re-exported alongside [`cert_binding`] so every app that
/// already depends on `fauna-client` can install its disk-backed pin store at
/// startup without a direct `fauna-anon-client` dep.
pub use fauna_anon_client::trust;
/// Re-exported beside [`upload_prepared_blob`], whose argument type it is: an
/// app that seals through `FeedManager::seal_compose_attachment` and POSTs
/// through this crate would otherwise need its own `fauna-media` edge purely to
/// name the parts (seven apps, seven edges, for one struct literal).
pub use fauna_media::pipeline::MultipartBlob;
pub use media_upload::{
    UploadedBlob, upload_gated_post_blob, upload_prepared_blob, upload_public_post_blob,
    upload_sealed_post_blob,
};
pub use push::PushBroker;
pub use types::ConnectionState;

// Re-exports from fauna-protocol so consumers don't need a direct dep.
pub use fauna_protocol::discovery::{SetupStatusReply, SetupStatusRequest};
pub use fauna_protocol::{
    EchoReply, EchoRequest, KindRegistry, PushEvent, RpcError, RpcKindMeta, Value,
};
