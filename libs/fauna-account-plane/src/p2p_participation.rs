//! A device's **own p2p participation** — wormability rule 5's off switch
//! (`docs/goal/behavior/p2p.md` § Per-device participation, ratified
//! 2026-09-25, user-directed).
//!
//! Whether this device runs its two peer listeners (the cross-user share
//! seat, the same-account peer-sync leg) and dials peers for either plane is
//! the **device's own choice**: one meta-table row on this device's account
//! store — the one store the app and its co-located sync agent share, so
//! whichever process holds a listener reads the same fact — and an absent row
//! reads *on*, the works-out-of-the-box posture every device has had since the
//! planes shipped. The nest never holds the authority: its row carries this
//! device's last *report* and a pending *brake* another of the user's devices
//! may raise, and a brake can only ever bring a listener down (the fold below);
//! enabling is a local act on this device alone.
//!
//! The two consumers are the two bind doors, and nothing else reads this:
//! `fauna_sync_engine::peer_leg::ensure_bound` (the same-account leg, which also folds
//! the brake and sends the report — it is the pass that talks to the nest) and
//! `fauna_sync_engine::offline_share::SessionSeat` through `fauna_sync_engine::share_glue::run`
//! (the share seat). An app renders the toggle and writes the row through
//! `fauna_sync_engine::account_runtime::AccountStoreHandle::set_p2p_participation`; it
//! never gates a listener itself (`p2p.md` § Architectural rules, rule 5).

use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use serde::{Deserialize, Serialize};

/// The row's meta-table key. dag-cbor via the store's canonical encoding;
/// additive at rest (a field added later decodes with its default).
pub const META_P2P_PARTICIPATION: &str = "p2p_participation";

/// This device's participation state, as rested on its own account store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct P2pParticipation {
    /// The device's own choice — the authority. `true` = the listeners run.
    pub local_on: bool,
    /// The last value this device reported to the nest
    /// (`fauna.sync.devices.p2p_participation.set`, the self arm); `None` =
    /// never reported. A report is owed whenever this differs from
    /// [`Self::local_on`] ([`Self::report_owed`]).
    #[serde(default)]
    pub reported: Option<bool>,
}

impl Default for P2pParticipation {
    /// An absent row: on, never reported.
    fn default() -> Self {
        Self {
            local_on: true,
            reported: None,
        }
    }
}

impl P2pParticipation {
    /// Whether this device runs its peer listeners right now — the verdict
    /// both bind doors read. The local choice IS the verdict: a pending nest
    /// brake reaches it only through [`Self::fold_brake`], never by being read
    /// live, so an unreachable nest changes nothing.
    pub fn effective(&self) -> bool {
        self.local_on
    }

    /// The user's own switch on this device. Returns whether anything changed.
    pub fn set_local(&mut self, on: bool) -> bool {
        let changed = self.local_on != on;
        self.local_on = on;
        changed
    }

    /// Honour a pending nest-side brake (`SyncDevice::p2p_off_requested`):
    /// another of the user's devices asked this one to turn its listeners
    /// off. A brake only ever turns the device OFF — a row with no pending
    /// brake, or one already off, is left alone, which is what keeps "the
    /// nest can only ever bring a listener down" true by construction.
    /// Returns whether the local state changed.
    pub fn fold_brake(&mut self, off_requested: bool) -> bool {
        if off_requested && self.local_on {
            self.local_on = false;
            true
        } else {
            false
        }
    }

    /// The value the next self-arm report should carry, if one is owed: the
    /// local state whenever the nest has not been told it yet.
    pub fn report_owed(&self) -> Option<bool> {
        (self.reported != Some(self.local_on)).then_some(self.local_on)
    }

    /// The nest acknowledged a report of `on`.
    pub fn mark_reported(&mut self, on: bool) {
        self.reported = Some(on);
    }
}

/// Read this device's row. A missing row is the default (on, never
/// reported); an unreadable one reads the same way and is logged — a corrupt
/// blob must never strand a device off, since the user can always flip it
/// again, and the next save overwrites it.
pub async fn load<B: StoreBackend>(store: &AccountStore<B>) -> P2pParticipation {
    match store.backend().meta_get(META_P2P_PARTICIPATION).await {
        Ok(Some(bytes)) => match fauna_core::encoding::canonical_decode(&bytes) {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(
                    "p2p participation: stored row unreadable ({e}) — reading the default (on)"
                );
                P2pParticipation::default()
            }
        },
        Ok(None) => P2pParticipation::default(),
        Err(e) => {
            tracing::debug!("p2p participation: row unreadable: {e:#}");
            P2pParticipation::default()
        }
    }
}

/// Persist this device's row.
pub async fn save<B: StoreBackend>(
    store: &AccountStore<B>,
    row: &P2pParticipation,
) -> anyhow::Result<()> {
    let bytes = fauna_core::encoding::canonical_encode(row)
        .map_err(|e| anyhow::anyhow!("encode p2p participation: {e}"))?;
    store
        .backend()
        .meta_put(META_P2P_PARTICIPATION, &bytes)
        .await
        .map_err(|e| anyhow::anyhow!("persist p2p participation: {e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_row_is_on_and_owes_a_report() {
        let row = P2pParticipation::default();
        assert!(row.effective());
        assert_eq!(row.report_owed(), Some(true));
    }

    #[test]
    fn a_brake_only_ever_turns_the_device_off() {
        let mut row = P2pParticipation::default();
        assert!(!row.fold_brake(false), "no pending brake changes nothing");
        assert!(row.effective());
        assert!(row.fold_brake(true), "a pending brake folds to off");
        assert!(!row.effective());
        assert!(
            !row.fold_brake(false),
            "an absent brake never turns a device back on"
        );
        assert!(!row.effective());
        assert!(!row.fold_brake(true), "already off: nothing changes");
    }

    #[test]
    fn a_report_is_owed_exactly_while_the_nest_has_not_heard_the_local_state() {
        let mut row = P2pParticipation::default();
        row.mark_reported(true);
        assert_eq!(row.report_owed(), None);
        assert!(row.set_local(false));
        assert_eq!(row.report_owed(), Some(false));
        row.mark_reported(false);
        assert_eq!(row.report_owed(), None);
        assert!(
            !row.set_local(false),
            "setting the same value changes nothing"
        );
        assert!(row.set_local(true));
        assert_eq!(row.report_owed(), Some(true));
    }

    #[test]
    fn the_row_round_trips_and_a_pre_report_row_decodes_with_no_report() {
        let row = P2pParticipation {
            local_on: false,
            reported: Some(false),
        };
        let bytes = fauna_core::encoding::canonical_encode(&row).unwrap();
        let back: P2pParticipation = fauna_core::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, row);
        // Only `local_on` on the wire — the additive-at-rest posture.
        #[derive(Serialize)]
        struct Older {
            local_on: bool,
        }
        let bytes = fauna_core::encoding::canonical_encode(&Older { local_on: false }).unwrap();
        let back: P2pParticipation = fauna_core::encoding::canonical_decode(&bytes).unwrap();
        assert!(!back.local_on);
        assert_eq!(back.reported, None);
    }
}
