//! The backup-destination state's production writer and reader — the typed
//! door for `fauna.state.backup` (`backup-destinations.md` owns the
//! destinations, `succession-aftermath.md` § Re-key scope the marks;
//! `fauna_core::backup_state` owns the records, their key grammar and their
//! join; `config-dissolution.md` § The `__config` dissolution schedule owns
//! the kind's birth, plane-only, and § Phases and gates → *Bounded rows* the
//! row shape).
//!
//! **Two row families**: one `destinations/<source nest>` row per source box
//! (that box's list, latest-wins on its own stamp) and one
//! `mark/<destination>/<predecessor>` row per mark, for the account. The
//! composite [`BackupState`] the consumers work on is the READ fold for ONE
//! box — the box the caller's connection is bound to (`backup-destinations.md`
//! § State & data shape → *Destination data model*) — which prunes that box's
//! list against every mark of the account.
//!
//! Both writes run whole on the store thread (`Cmd::is_local`), so nothing
//! interleaves a walk's merge between the read and the put:
//!
//! * [`write_backup_destinations`] replaces one box's list, stamped strictly
//!   above the row stored AT THAT KEY so a device whose clock trails still
//!   writes the newer value; the row's bounds
//!   ([`BackupDestinationsRow::check_bounds`]) are enforced here, before
//!   anything is sealed.
//! * [`merge_destination_marks`] joins each mark into its row and puts only
//!   the rows the join moved (the group-share door's shape).
//!
//! The kind is `GenerationTip`-sealed, so a put while no tip resolves is
//! refused at the fleet plane's REAL writer door and surfaces to the caller.
//! Each put is the **local write only** ([`AccountStatePlane::put_local`]);
//! the account runtime's publish step ships it.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::backup_state::{
    BackupDestinationsRow, BackupRecord, BackupState, decode_backup_row, destinations_key,
};
use fauna_core::data::{BackupConfig, DestinationUnattestedMark, Timestamp};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_BACKUP;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// `source_nest`'s backup-destination state, folded — the empty state when
/// no row rests yet.
pub async fn read_backup<B: StoreBackend>(
    store: &AccountStore<B>,
    source_nest: &[u8; 32],
) -> Result<BackupState> {
    backup_state_of(&store.states_of_kind(KIND_BACKUP).await?, source_nest)
}

/// The live `fauna.state.backup` rows among `entries`, as `(key, value)`.
fn live_rows(entries: &[StateEntry]) -> impl Iterator<Item = (&str, &[u8])> {
    entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_BACKUP)
        .map(|e| (e.key.as_str(), e.value.as_slice()))
}

/// The fold of every live `fauna.state.backup` row among `entries`, for
/// `source_nest` — the handle's read, over one `states_of_kind` load. A row
/// the decoder refuses fails loudly: silently skipping it would let the next
/// write put a bare list over state it held.
pub fn backup_state_of(entries: &[StateEntry], source_nest: &[u8; 32]) -> Result<BackupState> {
    BackupState::from_rows(live_rows(entries), source_nest)
        .context("the stored backup-destination rows")
}

/// Every box's destination-list row among `entries` — the all-boxes read
/// (the succession aftermath's mark raise; the rotated-box re-file).
pub fn destination_lists_of(entries: &[StateEntry]) -> Result<Vec<BackupDestinationsRow>> {
    fauna_core::backup_state::destination_lists(live_rows(entries))
        .context("the stored backup-destination rows")
}

/// Every mark of the account among `entries`, unioned.
pub fn destination_marks_of(entries: &[StateEntry]) -> Result<Vec<DestinationUnattestedMark>> {
    fauna_core::backup_state::destination_marks(live_rows(entries))
        .context("the stored backup-destination rows")
}

/// Replace `source_nest`'s destination list with `backup`, stamped
/// `max(now, stored + 1)` against the row at that box's key. Returns the
/// box's fold as it now stands, and whether a put happened (`false` when the
/// stored list already equals `backup` — the echo-stop).
///
/// # Errors
/// `backup` breaks the row's bounds (nothing is written), a stored row does
/// not decode, or the writer door refuses the put.
pub async fn write_backup_destinations<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    source_nest: &[u8; 32],
    backup: &BackupConfig,
    now: Timestamp,
) -> Result<(BackupState, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let key = destinations_key(source_nest);
    let stored = match store.state(KIND_BACKUP, &key).await? {
        Some(entry) if !entry.tombstone => {
            match decode_backup_row(&key, &entry.value)
                .context("the stored backup destinations row")?
            {
                BackupRecord::Destinations(row) => Some(row),
                BackupRecord::Mark(_) => unreachable!("the key names a destinations row"),
            }
        }
        _ => None,
    };
    if stored.as_ref().is_some_and(|row| row.backup == *backup) {
        return Ok((read_backup(store, source_nest).await?, false));
    }
    let updated_at = match &stored {
        Some(row) => now.max(Timestamp(row.updated_at.0.saturating_add(1))),
        None => now,
    };
    let row = BackupDestinationsRow {
        source_nest: *source_nest,
        backup: backup.clone(),
        updated_at,
    };
    row.check_bounds()
        .context("backup destinations: over the row's bounds")?;
    let value = BackupRecord::Destinations(row)
        .encode()
        .context("encode backup destinations row")?;
    fleet
        .put_local(
            &ItemId {
                kind: KIND_BACKUP.to_string(),
                key,
            },
            value,
            // The row carries its own stamp; no outer one.
            None,
        )
        .await
        .context("backup destinations: plane put")?;
    Ok((read_backup(store, source_nest).await?, true))
}

/// Join each of `marks` into its row and write every row the join moved.
/// Returns every mark of the account as it now stands, and whether any put
/// happened.
///
/// # Errors
/// A stored row does not decode, or the writer door refuses a put.
pub async fn merge_destination_marks<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    marks: &[DestinationUnattestedMark],
) -> Result<(Vec<DestinationUnattestedMark>, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let mut moved = false;
    for mark in marks {
        let key = mark.plane_key();
        let record = BackupRecord::Mark(mark.clone());
        let joined = match store.state(KIND_BACKUP, &key).await? {
            Some(entry) if !entry.tombstone => {
                let joined = decode_backup_row(&key, &entry.value)
                    .with_context(|| format!("the stored backup mark row {key:?}"))?
                    .merge(&record)
                    .context("backup mark row: join")?
                    .encode()
                    .context("encode backup mark row")?;
                if joined == entry.value {
                    continue;
                }
                joined
            }
            _ => record.encode().context("encode backup mark row")?,
        };
        fleet
            .put_local(
                &ItemId {
                    kind: KIND_BACKUP.to_string(),
                    key,
                },
                joined,
                // A mark is its own merge state; no outer stamp.
                None,
            )
            .await
            .context("backup mark row: plane put")?;
        moved = true;
    }
    Ok((
        destination_marks_of(&store.states_of_kind(KIND_BACKUP).await?)?,
        moved,
    ))
}
