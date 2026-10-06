//! Client-side logic for the hosted ATProto identity: did:plc audit-log
//! custody verification ([`genesis_verify`], [`plc_chain`]), the user-custodied
//! senior rotation key ([`rotation_key`]), published-handle binding
//! ([`handle_binding`]), the terminal PLC tombstone ([`tombstone`]) and the
//! 72 h recovery-fork contest ([`recovery_fork`]).
//!
//! Everything here is protocol-level — it works the same for any ATProto app,
//! so it carries the protocol's name (`docs/goal/behavior/atproto-pds-bridge.md`
//! § Naming). The consume-side Bluesky client (`bluesky.feed.thread`) stays in
//! `fauna-client-bluesky`. No nest dependency by design: the checks talk to the
//! PLC directory directly, which is what makes them an independent audit of the
//! box.

mod directory_submit;
pub mod genesis_verify;
pub mod handle_binding;
pub mod identity_store;
pub mod plc_chain;
pub mod port;
pub mod recovery_fork;
pub mod rotation_key;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;
pub mod tombstone;
