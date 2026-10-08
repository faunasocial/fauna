//! Shared fixtures for `fauna-sync-engine`'s own integration tests.
//!
//! Each file under `tests/` is its own crate, so a helper used by one file is
//! dead code in the next — the module-wide allow below is what lets this
//! module be `mod common;`-ed into a test that needs only one of its helpers.
#![allow(dead_code)]

use std::sync::Arc;

use fauna_sync_engine::account_runtime::{
    PeerLegBinding, PeerLegFactoryInputs, PeerTransportFactory,
};
use fauna_transport::testing::{Listeners, MemTransport};

/// An in-memory `PeerTransportFactory` dialing every peer over
/// `fauna_transport::testing::MemTransport` — the account-runtime peer leg
/// driven with no real network, one shared `Listeners` registry standing in
/// for discovery.
pub fn mem_factory(listeners: &Listeners) -> PeerTransportFactory {
    mem_factory_with(listeners, None)
}

/// [`mem_factory`] whose binding also carries a host's file-sync engines onto
/// the leg — the same-account peer data plane's serve door and sibling
/// registry.
pub fn mem_factory_with(
    listeners: &Listeners,
    file_sync: Option<fauna_sync_engine::account_runtime::PeerFileSync>,
) -> PeerTransportFactory {
    let listeners = Listeners::clone(listeners);
    Arc::new(move |inputs: PeerLegFactoryInputs| {
        let listeners = Listeners::clone(&listeners);
        let file_sync = file_sync.clone();
        Box::pin(async move {
            Ok(PeerLegBinding {
                transport: Arc::new(MemTransport {
                    me: fauna_transport::EndpointKey::from_bytes(
                        inputs.writer_key.verifying_key().to_bytes(),
                    ),
                    listeners,
                }),
                // Deterministic non-loopback candidate — the seam double
                // dials by node id, so the value only feeds the facts row.
                bound_addrs: vec!["203.0.113.9:4711".parse().unwrap()],
                file_sync,
            })
        })
    })
}
