//! Thin owner-side WS-RPC seam for the `fauna.labelers.*` community-labeler
//! registry (browse/inspect/subscribe/unsubscribe). Consumed by
//! `fauna-labeler-catalog-machine`'s `rpc-glue` production impl, exactly as
//! `fauna-client-folders`/`fauna-client-sync` are consumed by
//! `fauna-devices-machine`.
//!
//! Design tracked internally.

pub mod rpc;

pub use rpc::LabelersClient;
