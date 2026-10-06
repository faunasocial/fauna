//! The cross-user share leg — the W8 (account-data-plane.md § Workstreams) share twin's client crate.
//!
//! Contract owner: `docs/goal/behavior/p2p.md` § Cross-user shared-set
//! transfer (*Wormability walk — the share leg*) + `p2p-shared-set-build.md`
//! § *Build contract*. This
//! crate is the `p2p-share` registry feature's data plane on the client
//! side:
//!
//! - [`admission`] — the M2-membership witness verifier, the admission seam's
//!   third witness kind (a claim + the evaluator's own roster consult).
//! - [`ceremony`] — the offline share-initiation ceremony's carriage: the
//!   three initiator-originated `fauna.peer.share.ceremony.*` kinds and the
//!   [`CeremonyState`](ceremony::CeremonyState) seam the serve side consults
//!   (actor-level admission BEFORE parsing — rule 1's receive-act
//!   expectation shape).
//! - [`provenance`] — the peer-served change-row ruling as code, both halves:
//!   the serve-side own-authored filter and the ingest-side fail-closed
//!   cached-writer check.
//! - [`server`] — the allowlisted share serve set (five kinds, nothing else),
//!   its per-connection verdict slot, rule-8 quotas, and the [`ShareStore`]
//!   seam it reads local sync state through.
//! - [`endpoints`] — the discovery carriage (slice F): this device's own
//!   advertisement, and the binding that ties a received one to the identity
//!   the MLS channel proved before any dial row is written.
//! - [`client`] — the pull side: the mutual admit exchange, a
//!   [`BlobFetcher`](fauna_core::file_download::BlobFetcher) over the peer
//!   channel (so the **existing** shared download walk verifies peer-served
//!   bytes with no new code), and the row read behind the ingest check.
//!
//! **The whole crate is the gated plane.** Consumers take it only behind
//! their own `p2p-share` feature (the excision spine's forwarding rule), so
//! a store-safe flavor ships none of it and the store-safe witness's
//! `fauna.peer.share.` absence column stays provable.

pub mod admission;
pub mod ceremony;
pub mod client;
pub mod endpoints;
pub mod provenance;
pub mod server;

pub use admission::{
    GroupRosterState, SetMembership, evaluate_share_witness, folder_scope_string,
    group_scope_string, verdict_admits_group, verdict_admits_set, verdict_for_group_membership,
    verdict_for_m2_membership,
};
pub use ceremony::{
    CeremonyRefusal, CeremonyState, poll_ceremony_accept, send_ceremony_deliver,
    send_ceremony_offer,
};
pub use client::{
    PeerShareBlobFetcher, ShareAdmission, admit_group_over, admit_share_over, fetch_share_changes,
};
pub use endpoints::{AdvertisementRefusal, bind_share_advertisement, own_advertisement};
pub use provenance::{
    LocalShareChange, PeerRowAdmission, RowRefusal, judge_peer_row, reader_over_cached_roster,
    screen_peer_row, serves_held_row,
};
pub use server::{ShareServer, ShareServerConfig, ShareStore, allowlisted_kinds};
