//! The share plane's **landing policy** — which pulled bodies a replica
//! lands, and where (`p2p-shared-set-build.md` § *Phone peers — design*,
//! decision 1: *the ingest half lands rows always, and bodies by policy*).
//!
//! Pure functions, pinned at tier 1 (decision 5). The pump plans its fetches
//! by [`body_is_wanted`] and the state writer lands by the same answer, so a
//! body nobody will land is never pulled, priced or metered.
//!
//! A **resident** replica (the desktops) lands every accepted body into its
//! bound tree, judged by [`crate::peer_share_store::judge_materialization`].
//! An **on-demand** replica records every accepted row and lands a body only
//! when it is *wanted* — in this build, when the row is un-sequenced: the
//! nest cannot serve those bytes, so peers are their only source. A wanted
//! body lands in the kept root and stays there until the nest confirms it.

use std::path::{Path, PathBuf};

use fauna_core::data::ContentHash;

use crate::db::{SyncEntry, SyncState};
use crate::provider_face::owned_tree::BodyRoot;

/// How one replica lands the bodies its share plane pulls.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Landing {
    /// A bound local tree: every accepted body lands.
    #[default]
    Resident,
    /// An on-demand replica: rows always, bodies when wanted. `kept_root` is
    /// where a wanted body lands, and so the filesystem the storage floor is
    /// measured on.
    OnDemand { kept_root: PathBuf },
}

/// Whether a replica landing by `landing` wants the body of an accepted
/// create/modify row.
///
/// The on-demand arm's want is the **un-sequenced row**: a row the nest has
/// not recorded names bytes only peers hold. A sequenced row's bytes are
/// fetched from the nest when the file is opened. (A second want — the user
/// asked for the file and the nest could not answer — is a later slice; it
/// joins here.)
pub fn body_is_wanted(landing: &Landing, sequenced: bool) -> bool {
    match landing {
        Landing::Resident => true,
        Landing::OnDemand { .. } => !sequenced,
    }
}

/// The root a landed body takes on an on-demand replica: the kept root while
/// the nest may not hold it (an un-sequenced row — it stays there until the
/// reconcile's confirm stamps the dehydration proof), the cache root once the
/// nest does.
pub fn landing_root(sequenced: bool) -> BodyRoot {
    if sequenced {
        BodyRoot::Cache
    } else {
        BodyRoot::Kept
    }
}

/// The free space an on-demand replica never lands a peer's body into: 1 GiB,
/// twice the fixed low-storage threshold android applies, so a transfer stops
/// well before the OS starts reclaiming caches or refusing the user's own
/// writes. A constant, never a setting — nobody would choose it
/// (`principles.md` § One configuration surface).
pub const STORAGE_FLOOR_BYTES: u64 = 1 << 30;

/// Whether landing `needed` more bytes leaves the device at or above
/// [`STORAGE_FLOOR_BYTES`]. `free` is `None` where the platform cannot say
/// ([`free_space`]); the floor then cannot be evaluated and does not refuse.
pub fn landing_fits(free: Option<u64>, needed: u64) -> bool {
    free.is_none_or(|free| free.saturating_sub(needed) >= STORAGE_FLOOR_BYTES)
}

/// The bytes available to this process on the filesystem holding `path`, or
/// `None` where that cannot be measured (a platform without `statvfs`, or a
/// path that does not exist yet).
pub fn free_space(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let stat = rustix::fs::statvfs(path).ok()?;
        Some(stat.f_bavail.saturating_mul(stat.f_frsize))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// What the judge sees of one path on an on-demand replica.
pub struct OnDemandPathState {
    pub entry: Option<SyncEntry>,
    /// The hash of the body in the **kept root** (`None` = no body there),
    /// hashed at judge time.
    pub kept: Option<ContentHash>,
    /// The kept-root body is the one an earlier peer row landed — the path's
    /// overlay row says so and names this hash — and so not a write intent.
    pub kept_is_peer_landed: bool,
}

/// The landing decision for one accepted create/modify on an on-demand
/// replica.
#[derive(Debug, PartialEq, Eq)]
pub enum OnDemandVerdict {
    /// The tracked head already is the peer row's manifest and nothing more
    /// is owed — stamp the overlay, fetch nothing.
    AlreadyCurrent,
    /// Land the body in the kept root.
    Land,
    /// No body lands. `record_placeholder` says whether the path has no row
    /// yet and gets one — metadata only, so the file lists.
    RowOnly { record_placeholder: bool },
    /// Leave the path alone (reason for the report); the overlay row still
    /// stands and the nest's row converges the path later.
    Skip(&'static str),
}

/// Judge one accepted create/modify against an on-demand replica's path
/// state. Pure — every arm is tier_1-checkable without an engine.
///
/// It keeps the resident judge's conservative arms
/// ([`crate::peer_share_store::judge_materialization`]) and changes one: on
/// an on-demand replica a placeholder holds no bytes, so landing a wanted
/// body over it destroys nothing. And it adds the rule the two roots bring: a
/// body in the kept root is a local write intent and is never overwritten —
/// unless it is the body an earlier peer row landed there.
pub fn judge_on_demand_landing(
    local: &OnDemandPathState,
    wanted: bool,
    peer_manifest: &ContentHash,
) -> OnDemandVerdict {
    // A tombstone is no row.
    let entry = local
        .entry
        .as_ref()
        .filter(|e| e.state != SyncState::Deleted);
    let at_this_head = entry.is_some_and(|e| e.manifest_hash.as_ref() == Some(peer_manifest));
    // A placeholder at this head still owes a wanted body: an earlier pass
    // recorded the row and could not land its bytes (the floor, a spool miss).
    let owes_body = wanted && entry.is_some_and(|e| e.state == SyncState::Placeholder);
    if at_this_head && !owes_body {
        return OnDemandVerdict::AlreadyCurrent;
    }
    if local.kept.is_some() && !(wanted && local.kept_is_peer_landed) {
        return OnDemandVerdict::Skip("a kept-root body is a local write intent");
    }
    if !wanted {
        return OnDemandVerdict::RowOnly {
            record_placeholder: entry.is_none(),
        };
    }
    match entry.map(|e| e.state) {
        // No row, a placeholder (no bytes to lose), or a clean hydrated row
        // whose body the nest can serve again.
        None | Some(SyncState::Placeholder | SyncState::Synced) => OnDemandVerdict::Land,
        Some(_) => OnDemandVerdict::Skip("path not clean-synced locally"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SyncDb;

    fn h(byte: u8) -> ContentHash {
        ContentHash::of_raw(&[byte])
    }

    fn entry_with(state: SyncState, manifest: ContentHash) -> SyncEntry {
        let db = SyncDb::open_in_memory().unwrap();
        db.upsert_entry("p", None, None, Some(manifest), state, 0, 0, 1, 1, None)
            .unwrap();
        db.get_entry("p").unwrap().unwrap()
    }

    fn on_demand() -> Landing {
        Landing::OnDemand {
            kept_root: PathBuf::from("/kept"),
        }
    }

    /// The first build's want: a resident tree takes every body; an on-demand
    /// replica takes only what the nest cannot serve.
    #[test]
    fn an_on_demand_replica_wants_only_the_unsequenced_body() {
        assert!(body_is_wanted(&Landing::Resident, true));
        assert!(body_is_wanted(&Landing::Resident, false));
        assert!(body_is_wanted(&on_demand(), false));
        assert!(!body_is_wanted(&on_demand(), true));
    }

    /// An unconfirmed body rests where nothing frees it; a body the nest
    /// holds rests where the OS may reclaim it.
    #[test]
    fn an_unsequenced_body_takes_the_kept_root_and_a_sequenced_one_the_cache() {
        assert_eq!(landing_root(false), BodyRoot::Kept);
        assert_eq!(landing_root(true), BodyRoot::Cache);
    }

    /// The floor refuses a body that would take free space under it, and
    /// only then; a platform that cannot measure does not refuse.
    #[test]
    fn the_storage_floor_refuses_a_body_that_would_cross_it() {
        let floor = STORAGE_FLOOR_BYTES;
        assert!(landing_fits(Some(floor + 10), 10), "lands exactly on it");
        assert!(!landing_fits(Some(floor + 10), 11));
        assert!(!landing_fits(Some(floor - 1), 0), "already under it");
        assert!(!landing_fits(Some(5), 10), "more than is free");
        assert!(landing_fits(None, u64::MAX), "unmeasurable never refuses");
    }

    #[cfg(unix)]
    #[test]
    fn free_space_measures_an_existing_directory_and_not_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_space(dir.path()).is_some());
        assert_eq!(free_space(&dir.path().join("missing")), None);
    }

    fn state(entry: Option<SyncEntry>, kept: Option<ContentHash>, peer: bool) -> OnDemandPathState {
        OnDemandPathState {
            entry,
            kept,
            kept_is_peer_landed: peer,
        }
    }

    /// Every arm of the on-demand judge, wanted body.
    #[test]
    fn a_wanted_body_lands_only_where_nothing_local_can_be_lost() {
        let peer = h(9);
        let land = |s: OnDemandPathState| judge_on_demand_landing(&s, true, &peer);

        // Fresh path.
        assert_eq!(land(state(None, None, false)), OnDemandVerdict::Land);
        // THE CHANGED ARM: a placeholder holds no bytes.
        assert_eq!(
            land(state(
                Some(entry_with(SyncState::Placeholder, h(1))),
                None,
                false
            )),
            OnDemandVerdict::Land
        );
        // A placeholder already at this head still owes the wanted body.
        assert_eq!(
            land(state(
                Some(entry_with(SyncState::Placeholder, peer)),
                None,
                false
            )),
            OnDemandVerdict::Land
        );
        // A clean hydrated row (its body is in the cache root, or reclaimed).
        assert_eq!(
            land(state(
                Some(entry_with(SyncState::Synced, h(1))),
                None,
                false
            )),
            OnDemandVerdict::Land
        );
        // A tombstone is no row.
        assert_eq!(
            land(state(
                Some(entry_with(SyncState::Deleted, h(1))),
                None,
                false
            )),
            OnDemandVerdict::Land
        );
        // The hydrated head already is this manifest.
        assert_eq!(
            land(state(
                Some(entry_with(SyncState::Synced, peer)),
                None,
                false
            )),
            OnDemandVerdict::AlreadyCurrent
        );
        // A kept-root body is a write intent — never overwritten...
        assert_eq!(
            land(state(
                Some(entry_with(SyncState::Synced, h(1))),
                Some(h(2)),
                false
            )),
            OnDemandVerdict::Skip("a kept-root body is a local write intent")
        );
        assert_eq!(
            land(state(None, Some(h(2)), false)),
            OnDemandVerdict::Skip("a kept-root body is a local write intent")
        );
        // ...unless an earlier peer row landed it.
        assert_eq!(
            land(state(
                Some(entry_with(SyncState::Synced, h(1))),
                Some(h(2)),
                true
            )),
            OnDemandVerdict::Land
        );
        // Every other state is some other pass's business.
        for dirty in [
            SyncState::LocallyModified,
            SyncState::RemotelyModified,
            SyncState::Conflicted,
            SyncState::Uploading,
            SyncState::Downloading,
            SyncState::LocallyDeleted,
        ] {
            assert_eq!(
                land(state(Some(entry_with(dirty, h(1))), None, false)),
                OnDemandVerdict::Skip("path not clean-synced locally"),
                "{dirty:?}"
            );
        }
    }

    /// Every arm of the on-demand judge, body not wanted: the row is
    /// recorded, no byte lands.
    #[test]
    fn an_unwanted_body_records_the_row_and_lands_nothing() {
        let peer = h(9);
        let row = |s: OnDemandPathState| judge_on_demand_landing(&s, false, &peer);

        assert_eq!(
            row(state(None, None, false)),
            OnDemandVerdict::RowOnly {
                record_placeholder: true
            },
            "a new path lists as a placeholder"
        );
        assert_eq!(
            row(state(
                Some(entry_with(SyncState::Placeholder, h(1))),
                None,
                false
            )),
            OnDemandVerdict::RowOnly {
                record_placeholder: false
            },
            "an existing row is the nest fold's to re-point"
        );
        assert_eq!(
            row(state(
                Some(entry_with(SyncState::Placeholder, peer)),
                None,
                false
            )),
            OnDemandVerdict::AlreadyCurrent
        );
        assert_eq!(
            row(state(None, Some(h(2)), true)),
            OnDemandVerdict::Skip("a kept-root body is a local write intent"),
            "an unwanted row never displaces a kept body, whoever landed it"
        );
    }
}
