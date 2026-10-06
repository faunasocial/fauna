//! **Who may author a row of a third-party kind** — the account's replicas'
//! position (2) of `third-party-kinds.md` § Principal write authority.
//!
//! The delegable pair of an `ext.*` kind is one symmetric unit: a principal
//! granted it can seal as well as open, and its in-seal Ed25519 signature
//! verifies under its own `writer_id` like any device's. What separates write
//! authority from read is therefore checked above the seal: a row in an
//! `ext:<kind>` scope is admitted iff its writer is
//!
//! - **one of the account's own devices** — enrolled in the verified
//!   `fauna.state.device-set` view, or removed from it (a departed device's
//!   rows are still the account's own words); a principal holds no fleet-only
//!   key, so it can forge neither; or
//! - **a writer the owner authorized for that kind** — some `Mint` or `Renew`
//!   event in the owner's grant log carries a `content.write` tuple over the
//!   kind confined to that writer key
//!   ([`fauna_core::grant_event::content_write_authorizations`]). **Ever
//!   minted, revoked or not**: a row the user kept is the user's data, and a
//!   row carries no trusted time a revocation window could be checked
//!   against. The log is the succession ledger's chain-signed fold, so an
//!   event no owner signed authorizes nothing.
//!
//! Any other writer's row is skipped and counted
//! (`WalkReport::unmergeable`'s posture), never fatal — and re-presented, so a
//! row that arrives before its grant event is admitted by the first walk after
//! the event lands.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_core::generation::FleetView;
use fauna_core::identity::ActorId;
use fauna_protocol::merge_policy::KIND_DEVICE_SET;

use crate::generation_store::{live_rows, row_ref};

/// The writer authority one walk of an `ext:<kind>` scope checks against —
/// read once per walk ([`Self::read`]), so a page of rows costs two store
/// reads, not two per row.
#[derive(Debug, Clone, Default)]
pub struct ExtWriters {
    fleet: FleetView,
    principals: BTreeMap<String, BTreeSet<[u8; 32]>>,
}

impl ExtWriters {
    /// The authority this store's merged rows state, for the account `root`.
    pub async fn read<B: StoreBackend>(store: &AccountStore<B>, root: &ActorId) -> Result<Self> {
        let device_rows = live_rows(store, KIND_DEVICE_SET).await?;
        let fleet = FleetView::build(root, device_rows.iter().map(row_ref));
        let ledger = crate::succession_ledger_rows::read_succession_ledger(store, *root).await?;
        Ok(Self {
            fleet,
            principals: fauna_core::grant_event::content_write_authorizations(&ledger.grant_events),
        })
    }

    /// Built from parts — the fixtures' constructor.
    pub fn from_parts(fleet: FleetView, principals: BTreeMap<String, BTreeSet<[u8; 32]>>) -> Self {
        Self { fleet, principals }
    }

    /// May `writer` author rows of `kind`?
    pub fn admits(&self, writer: &[u8; 32], kind: &str) -> bool {
        self.fleet.is_verified_member(writer)
            || self.fleet.is_excluded(writer)
            || self
                .principals
                .get(kind)
                .is_some_and(|writers| writers.contains(writer))
    }
}
