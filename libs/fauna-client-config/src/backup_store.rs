//! The one read and the one mutation door every backup-destination consumer
//! runs over `fauna.state.backup` — the kind the destination list and its
//! unattested marks rest in since the consumer cut (`config-dissolution.md`
//! § The `__config` dissolution schedule, the kinds table's row: born
//! plane-only, never bridged).
//!
//! **Per source box** (`backup-destinations.md` § State & data shape →
//! *Destination data model*): every read and write names the box the caller's
//! connection is bound to — `source_nest`, the id
//! `fauna_client_pair::LinkedNestsMachine::bound_nest_id` proves, never the
//! nest's own `fauna.nest.info` claim — and a row under another box's
//! identity is never read, rendered or registered here. The marks are the
//! account's.
//!
//! [`mutate_backup`] is the write every Backups-page gesture composes: read
//! the box's state, apply the gesture to a copy (the `crate::mutate` helpers),
//! refuse a list over the row's bounds with the ONE shared localized refusal
//! ([`BackupWriteError::ListFull`]) before anything is written, then put the
//! marks the gesture moved and the list if it moved. There is no CAS: the
//! door stamps the list above the stored row, and a mark joins its row.

use std::collections::BTreeSet;

use fauna_core::backup_state::{BackupBoundsError, BackupDestinationsRow, BackupState};
use fauna_core::data::{DestinationUnattestedMark, Timestamp, UnattestedVerdict};
use fauna_core::identity::ActorId;
use fauna_protocol::RpcRequester;

use crate::store_seam::{BackupStateStore, StoreError};

/// A backup-state write that did not land.
#[derive(Debug, thiserror::Error)]
pub enum BackupWriteError {
    /// The box's list is full — its row would pass the kind's size bound
    /// (`config-dissolution.md` *Bounded rows* → *The backup state*).
    /// Nothing was written. `Display` is the one localized refusal every app
    /// renders.
    #[error("{}", fauna_i18n::strings::backups::BACKUP_DESTINATIONS_FULL)]
    ListFull,
    /// One destination's text field is over its cap — a URL or a label no
    /// form admits. Nothing was written.
    #[error("{0}")]
    Bounds(BackupBoundsError),
    /// The store refused or failed (the account store not up yet, the
    /// transient no-tip refusal, the runtime gone).
    #[error("{0}")]
    Store(#[from] StoreError),
}

/// `source_nest`'s backup-destination state — the list the Backups page
/// renders, pruned of every removed destination, and the account's marks.
///
/// # Errors
/// The store failed or is not up yet ([`crate::LEDGER_NOT_READY`]).
pub async fn load_backup_state(
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
) -> Result<BackupState, StoreError> {
    store.backup_state(source_nest).await
}

/// Apply `gesture` to `source_nest`'s state and write what it moved: the
/// marks first (a `Removed` verdict must rest before the row it prunes is
/// dropped, so a crash between the two puts still reads pruned), then the
/// list. Answers the state as it now reads and the gesture's own result.
///
/// The list is checked against the row's bounds BEFORE any put, so a
/// refusal writes nothing.
///
/// # Errors
/// [`BackupWriteError`].
pub async fn mutate_backup<T>(
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
    gesture: impl FnOnce(&mut BackupState) -> T,
) -> Result<(BackupState, T), BackupWriteError> {
    let before = store.backup_state(source_nest).await?;
    let mut after = before.clone();
    let out = gesture(&mut after);
    let list_moved = after.backup != before.backup;
    if list_moved {
        check_list_bounds(source_nest, &after)?;
    }
    let marks: Vec<DestinationUnattestedMark> = after
        .marks
        .iter()
        .filter(|m| !before.marks.contains(m))
        .cloned()
        .collect();
    let marks_moved = !marks.is_empty();
    if marks_moved {
        store.merge_destination_marks(marks).await?;
    }
    let state = if list_moved {
        store
            .write_backup_destinations(source_nest, after.backup)
            .await?
    } else if marks_moved {
        store.backup_state(source_nest).await?
    } else {
        before
    };
    Ok((state, out))
}

/// The row's bounds, checked at the widest stamp the door could write, so a
/// list this admits the door admits too.
fn check_list_bounds(source_nest: [u8; 32], state: &BackupState) -> Result<(), BackupWriteError> {
    BackupDestinationsRow {
        source_nest,
        backup: state.backup.clone(),
        updated_at: Timestamp(u64::MAX),
    }
    .check_bounds()
    .map_err(|e| match e {
        BackupBoundsError::ListFull { .. } => BackupWriteError::ListFull,
        other => BackupWriteError::Bounds(other),
    })
}

/// **The succession's destination-mark raise** (`succession-aftermath.md`
/// § Re-key scope → *Adjudicating what the aftermath carries across*, the
/// 2026-09-30 paragraph): an `Open` mark keyed on `(destination, predecessor)`
/// for every destination the account lists on ANY box — the succession is
/// the account's event and a mark names no box. Idempotent by the mark row's
/// join: a decided verdict is never demoted. Returns whether any mark was
/// written.
///
/// # Errors
/// The store refused or failed; the raise stays owed.
pub async fn raise_succession_destination_marks(
    store: &dyn BackupStateStore,
    predecessor: ActorId,
) -> Result<bool, StoreError> {
    let ids: BTreeSet<String> = store
        .backup_destination_lists()
        .await?
        .into_iter()
        .flat_map(|row| row.backup.destinations)
        .map(|d| d.destination_id)
        .collect();
    let marks: Vec<DestinationUnattestedMark> = ids
        .into_iter()
        .map(|destination_id| DestinationUnattestedMark {
            destination_id,
            predecessor,
            verdict: UnattestedVerdict::Open,
        })
        .collect();
    if marks.is_empty() {
        return Ok(false);
    }
    store.merge_destination_marks(marks).await?;
    Ok(true)
}

/// **The rotated-box re-file** (`backup-destinations.md` § State & data
/// shape → *Destination data model*, the *A rotated box keeps its list*
/// paragraph): when no list row rests under `bound` while the account holds
/// list rows under other identities, fetch the bound nest's rotation chain
/// over `nest` (the connection that proved `bound`) and re-file, under
/// `bound`, the list of the latest identity a verified chain links to it
/// (`fauna_protocol::nest_rotation::verify_chain` — both signatures at every
/// hop), each covered folder's `__folder/<ancestor-hex>/<folder-id>` row
/// re-keyed under `bound` in the same write. No verified link, a fetch
/// failure, or a nest with no chain kind: nothing is written and the other
/// rows stay another box's. Returns whether a list was re-filed.
///
/// Every step is best-effort: a read the store refuses answers `false`,
/// never an error, so a Backups-page read never fails on it.
pub async fn refile_rotated_box_list<R>(
    store: &dyn BackupStateStore,
    nest: &R,
    bound: [u8; 32],
) -> bool
where
    R: RpcRequester,
{
    use fauna_protocol::nest_rotation::{
        ROTATION_CHAIN_KIND, RotationChainReply, RotationChainRequest, verified_ancestors,
        verify_chain,
    };
    let Ok(lists) = store.backup_destination_lists().await else {
        return false;
    };
    if lists.iter().any(|row| row.source_nest == bound) || lists.is_empty() {
        return false;
    }
    let reply: RotationChainReply = match nest
        .request(ROTATION_CHAIN_KIND, RotationChainRequest::default())
        .await
    {
        Ok(reply) => reply,
        Err(e) => {
            tracing::info!(error = %e, "no rotation chain for the bound box; no list re-filed");
            return false;
        }
    };
    // The candidate latest in the chain: the one whose hop out of it comes
    // last, among those a verified chain links to the bound identity.
    let best = lists
        .iter()
        .filter_map(|row| {
            let pos = reply
                .chain
                .iter()
                .rposition(|hop| hop.statement.old_nest_actor_id == row.source_nest)?;
            verify_chain(&reply.chain, &row.source_nest, &bound).ok()?;
            Some((pos, row))
        })
        .max_by_key(|(pos, _)| *pos)
        .map(|(_, row)| row.clone());
    let Some(mut predecessor) = best else {
        return false;
    };
    // A covered folder's mirror set is named for the box's identity, so the
    // name follows the box: in the same write, each coverage row named under a
    // verified ancestor is re-keyed under the bound identity
    // (`backup-destinations.md` § State & data shape → *A rotated box keeps its
    // list*) — the name the source now answers `attach_folder` with, and the
    // one the destination holds once the seat has moved.
    let ancestors = verified_ancestors(&reply.chain, &bound);
    for row in &mut predecessor.backup.destinations {
        if let Some((source, folder_id)) =
            fauna_core::data::parse_folder_backup_set_name(&row.folder_name)
            && ancestors.contains(&source)
        {
            row.folder_name = fauna_core::data::folder_backup_set_name(&bound, folder_id);
        }
    }
    match store
        .write_backup_destinations(bound, predecessor.backup)
        .await
    {
        Ok(_) => {
            tracing::info!("re-filed a rotated box's backup list under its new identity");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "re-filing the rotated box's backup list was refused");
            false
        }
    }
}

/// [`load_backup_state`] after [`refile_rotated_box_list`] — the read a
/// Backups page (and the status heal) runs, so a rotated box's list follows
/// it the first time a device bound to the new identity reads it.
///
/// # Errors
/// The store failed or is not up yet.
pub async fn load_backup_state_refiled<R>(
    store: &dyn BackupStateStore,
    nest: &R,
    source_nest: [u8; 32],
) -> Result<BackupState, StoreError>
where
    R: RpcRequester,
{
    let state = store.backup_state(source_nest).await?;
    // A row rests (its stamp is never 0) or the account holds no other list:
    // nothing to re-file.
    if state.updated_at != Timestamp(0) {
        return Ok(state);
    }
    if refile_rotated_box_list(store, nest, source_nest).await {
        return store.backup_state(source_nest).await;
    }
    Ok(state)
}
