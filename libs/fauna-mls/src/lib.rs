//! MLS (RFC 9420) encryption layer for the Fauna protocol.
//!
//! This crate provides end-to-end encrypted group messaging using the
//! Messaging Layer Security protocol via OpenMLS. It supports:
//!
//! - **DM channels**: two members at creation; the wrapper offers no add path,
//!   but nothing enforces the count (see [`channel::DmChannel`])
//! - **Group channels**: multi-party messaging with add/remove semantics
//! - **Device-sync channels**: syncing state across a single actor's devices

pub mod blob_crypto;
pub mod channel;
pub mod engine;
pub mod error;
pub mod replenish;
pub mod room_message;
pub mod room_policy;
pub mod segments;
pub mod state_replica;
#[cfg(feature = "native")]
pub mod storage;
pub mod succession;
pub mod types;
pub mod version;
pub mod wrapped_blob;
