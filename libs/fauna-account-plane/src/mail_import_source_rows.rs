//! The mail import's remembered source — the typed door for
//! `fauna.state.mail-import-source` (`fauna_core::mail_import_source` owns the
//! value; `mailbox-migration.md` § Credential handling owns the concept).
//!
//! **One row per account, at [`MAIL_IMPORT_SOURCE_ROW_KEY`]**, whole-record
//! latest-wins, the `dns_rows` shape: a write replaces the record under a
//! fresh `LwwStamp`; the user's Forget is a stamped tombstone, so a stale copy
//! on another device cannot bring a forgotten server back. No merge arm reads
//! the value, so the decode is tolerant.
//!
//! The kind is `GenerationTip`-sealed, so a put while no tip resolves is
//! refused at the fleet plane's REAL writer door and surfaces to the caller as
//! the write error. Each put is the **local write only**; the account
//! runtime's publish step ships it.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::mail_import_source::RememberedImportSource;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_MAIL_IMPORT_SOURCE;
pub use fauna_protocol::merge_policy::MAIL_IMPORT_SOURCE_ROW_KEY;

use crate::account_state_plane::{AccountStatePlane, put_lww_row_local, tombstone_lww_row_local};

/// The remembered source among `entries` (a `states_of_kind` read) — `None`
/// when nothing is remembered or the user forgot it. A row that does not
/// decode reads as nothing remembered: the record only prefills a form the
/// user can see and correct, and the next connection replaces it.
pub fn mail_import_source_of(entries: &[StateEntry]) -> Option<RememberedImportSource> {
    let entry = entries.iter().find(|e| {
        e.kind == KIND_MAIL_IMPORT_SOURCE && e.key == MAIL_IMPORT_SOURCE_ROW_KEY && !e.tombstone
    })?;
    match fauna_core::encoding::canonical_decode::<RememberedImportSource>(&entry.value) {
        Ok(source) => Some(source),
        Err(e) => {
            tracing::warn!("the remembered mail-import source does not decode: {e:#}");
            None
        }
    }
}

/// The remembered source this store holds.
pub async fn read_mail_import_source<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<Option<RememberedImportSource>> {
    let entry = store
        .state(KIND_MAIL_IMPORT_SOURCE, MAIL_IMPORT_SOURCE_ROW_KEY)
        .await?;
    Ok(entry.and_then(|e| mail_import_source_of(std::slice::from_ref(&e))))
}

/// Remember `next` (`Some`) or forget the stored record (`None`), stamped
/// `(now, device_id)`. Whether anything was written: `false` when the store
/// already holds exactly that. A record past its bounds
/// ([`RememberedImportSource::is_bounded`]) is refused.
pub async fn write_mail_import_source<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    next: Option<&RememberedImportSource>,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let stored = read_mail_import_source(store).await?;
    if stored.as_ref() == next {
        return Ok(false);
    }
    match next {
        Some(next) => {
            anyhow::ensure!(
                next.is_bounded(),
                "the mail-import source is past the remembered bounds"
            );
            let value = fauna_core::encoding::canonical_encode(next)
                .context("encode the mail-import source")?;
            put_lww_row_local(
                fleet,
                KIND_MAIL_IMPORT_SOURCE,
                MAIL_IMPORT_SOURCE_ROW_KEY,
                value.to_vec(),
                device_id,
            )
            .await
            .context("mail-import source: plane put")?;
        }
        None => {
            tombstone_lww_row_local(
                fleet,
                KIND_MAIL_IMPORT_SOURCE,
                MAIL_IMPORT_SOURCE_ROW_KEY,
                device_id,
            )
            .await
            .context("mail-import source: plane tombstone")?;
        }
    }
    Ok(true)
}
