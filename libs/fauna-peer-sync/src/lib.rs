//! The same-account **peer sync leg** (W2.6 (account-data-plane.md § Workstreams)): store↔store convergence over
//! the P2P transport seam.
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § The peer leg —
//! *"the same sync contract over a different transport"*: the W2.4 walk
//! (`fauna_sync_engine::account_state_plane`, constructed pull-only) runs
//! against a peer's relay plane exactly as it runs against the nest feed,
//! over [`client::PeerRequester`]; the serve side ([`server::PeerSyncServer`])
//! answers the same `fauna.sync.changes.list` wire from
//! `fauna_account_store`'s relay plane, plus the want-list block pull. Wire
//! kinds: `fauna_protocol::peer_sync`.
//!
//! # The admission seam (§ The peer leg → *The admission seam*)
//!
//! **The core consumes a verdict, never a witness** ([`admission`]): witness
//! evaluation lives with each witness kind's mechanics
//! (`fauna_core::encoding::verify_device_admission_witness` for
//! `DeviceAuthorization`), and every pull or serve checks its scope against
//! the per-connection [`admission::AdmissionVerdict`]. That is what keeps
//! this engine **admission-agnostic**: the cross-account share twin
//! (`p2p.md` § Cross-user shared-set transfer, M2 membership) and the custody
//! leg (W8, the custody grant) plug their own verifiers into the same core.
//!
//! # Wormability posture (§ Wormability walk; rules owned by `p2p.md`)
//!
//! - **Rule 3 (least-kind dispatcher):** the serve set is the allowlist in
//!   [`server`] — `node_info`, the admission exchange, `changes.list`,
//!   `blocks.pull` — and nothing else; `PeerChannel::serve` answers every
//!   other kind `fauna.protocol.unknown_kind`. Class-2 state rides as sealed
//!   *content* inside transfer kinds, never as imperative kinds.
//! - **Rule 5 (no listener when off):** the listener exists iff a
//!   [`server::start_peer_sync_node`] node is alive — no unconditional bind;
//!   dropping the node closes it. Kind-family separability: everything here
//!   is `fauna.peer.sync.*` / the reused `fauna.sync.changes.list`, disjoint
//!   from any future cross-user share family, and this crate sits under no
//!   `p2p-share` gate.
//! - **Rule 7 (version brake):** [`server::start_peer_sync_node`] refuses to
//!   bring the leg up unless the nest advertises the `peer-sync` capability
//!   ([`discovery::peer_sync_enabled`]).
//! - **Rule 8 (bounded fan-out):** [`quota::QuotaLedger`] — per-peer,
//!   per-window connection and request quotas at the shared listener,
//!   admission-refused attempts included.
//!
//! Web is a declared structural absence (a browser origin cannot run the QUIC
//! seam — charter § The peer leg → *Web*); this crate is native-only and
//! never enters `fauna-wasm`'s graph.

#![forbid(unsafe_code)]

pub mod admission;
pub mod client;
pub mod discovery;
pub mod lan;
pub mod quota;
pub mod server;

pub use admission::{
    AdmissionVerdict, AdmittedConnection, AdmittedEntry, AdmittedScopes, EvaluatedWitness,
    evaluate_witness,
};
pub use client::{
    AdmissionOutcome, AdmissionViews, CustodyRevocationView, DeviceRemovedView, PeerRequester,
    PullReport, admit_over, admit_over_as, pull_missing_blocks,
};
pub use discovery::{
    PeerDialTarget, bind_carried_endpoints, custodian_dial_targets, dial_target_from,
    peer_sync_enabled, sibling_dial_targets,
};
pub use quota::{MeteredPlane, QuotaConfig, QuotaLedger, metered_handler_factory};
pub use server::{
    CustodyRevocationFn, DeviceRemovedFn, PeerSyncServer, PeerSyncServerConfig,
    start_peer_sync_node,
};
