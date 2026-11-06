//! The agent's half of the **reach hint** (`onboarding-provisioning.md`
//! § Reach hint → *The session connector's half*).
//!
//! The agent is a process of its own with no launch machine, so nothing else
//! fills its reach table (`fauna_ws_substrate::reach_hint`). The capability
//! carries the hint instead ([`SyncCapability::reach_ipv4`]): every provision
//! and every boot restore adopts it, before the capability is published, and
//! from there every native dial in this process follows the table's rule — the
//! domain first, the hint only when the domain reached nothing — with no
//! agent-side code on any dial path.
//!
//! **The slot stays the app's.** The entry's deleter is empty: a bearer socket
//! that opens over the domain removes the agent's own table entry and touches
//! nothing else, while the persisted slot is deleted by the app's session on
//! its own first domain success, and the app's next provision then arrives
//! without a hint and clears the entry here.

use std::sync::Arc;

use fauna_ipc::sync::SyncCapability;
use fauna_ws_substrate::reach_hint;

/// Make this process's reach table say what `cap` says about its domain: its
/// hint installed, or no entry at all. A malformed hint dialled nothing on the
/// app's side either, so it reads as none.
pub(crate) fn adopt(cap: &SyncCapability) {
    let Some(host) = reach_hint::host_key_of_url(&cap.nest_url) else {
        return;
    };
    let addr = cap
        .reach_ipv4
        .as_deref()
        .and_then(|ip| fauna_core::web::reach_socket_addr(&cap.nest_url, ip));
    match addr {
        Some(addr) => reach_hint::install(&host, addr, Arc::new(|| {})),
        None => reach_hint::clear(&host),
    }
}

/// The capability that named `nest_url` is gone: so is its table entry.
pub(crate) fn forget(nest_url: &str) {
    if let Some(host) = reach_hint::host_key_of_url(nest_url) {
        reach_hint::clear(&host);
    }
}
