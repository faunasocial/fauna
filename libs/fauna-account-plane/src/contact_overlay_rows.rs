//! The private contact overlay's production writer and reader — the typed
//! door for `fauna.state.contact-overlay` (`docs/goal/ui/contacts.md` § The
//! private overlay owns the concept; `fauna_core::contact_overlay` owns the
//! record, the per-register join, the bounds and the staged-form diff).
//!
//! A write is a **read-modify-write on the store thread**: the stored overlay
//! is read, the caller's changed registers ([`OverlayWrite`]) are stamped
//! onto it, and the result is put through the fleet plane's REAL writer
//! door — the kind is `GenerationTip`-sealed, so a put while no tip resolves
//! is refused there and surfaces to the caller as the save error. Nothing
//! between the read and the write yields to a walk, so a concurrent merge of
//! the same item cannot interleave (`Cmd::is_local`, the read-marker
//! verdict's shape). Each put is the **local write only**
//! ([`AccountStatePlane::put_local`]); the account runtime's publish step
//! ships it.
//!
//! Exposed through `fauna_sync_engine::account_runtime::AccountStoreHandle`'s typed
//! doors so app glue never touches a plane handle directly.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::contact_overlay::{ContactOverlay, OverlayChanges, Stamp, overlay_key};
use fauna_core::localized::LocalizedText;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_CONTACT_OVERLAY;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// What a Save asks the door to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayWrite {
    /// Only the registers the user changed
    /// (`fauna_core::contact_overlay::changed_registers`).
    Changes(OverlayChanges),
    /// "Remove everything I wrote about this person": every register `None`.
    Clear,
}

/// What the door did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayWriteOutcome {
    /// The row is durable locally; the overlay as it now stands.
    Written(ContactOverlay),
    /// Nothing to write (an empty change set, or clearing an absent overlay).
    Unchanged(ContactOverlay),
    /// The shared validator refused (a label cap) — nothing was written.
    Refused(LocalizedText),
}

/// The overlay this store holds on `actor_id_hex` — the default (empty) one
/// when there is none. An entry that does not decode fails loudly: silently
/// reading it as empty would let the next Save overwrite it.
pub async fn read_contact_overlay<B: StoreBackend>(
    store: &AccountStore<B>,
    actor_id_hex: &str,
) -> Result<ContactOverlay> {
    let Some(key) = overlay_key(actor_id_hex) else {
        bail!("contact overlay: {actor_id_hex:?} is not an actor id");
    };
    match store.state(KIND_CONTACT_OVERLAY, &key).await? {
        Some(entry) if !entry.tombstone => {
            fauna_core::encoding::canonical_decode::<ContactOverlay>(&entry.value)
                .context("the stored contact overlay does not decode")
        }
        _ => Ok(ContactOverlay::default()),
    }
}

/// Write `write` onto the stored overlay on `actor_id_hex`, stamped
/// `(now_ms, device_id)`.
pub async fn write_contact_overlay<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    actor_id_hex: &str,
    write: &OverlayWrite,
    now_ms: i64,
) -> Result<OverlayWriteOutcome>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let key = overlay_key(actor_id_hex)
        .with_context(|| format!("contact overlay: {actor_id_hex:?} is not an actor id"))?;
    let current = read_contact_overlay(store, actor_id_hex).await?;
    let stamp = Stamp::new(now_ms, device_id);
    let next = match write {
        OverlayWrite::Changes(changes) if changes.is_empty() => {
            return Ok(OverlayWriteOutcome::Unchanged(current));
        }
        OverlayWrite::Changes(changes) => match current.apply(changes, stamp) {
            Ok(next) => next,
            Err(refusal) => return Ok(OverlayWriteOutcome::Refused(refusal)),
        },
        OverlayWrite::Clear if current.is_empty() => {
            return Ok(OverlayWriteOutcome::Unchanged(current));
        }
        OverlayWrite::Clear => current.cleared(stamp),
    };
    let value = fauna_core::encoding::canonical_encode(&next).context("encode contact overlay")?;
    fleet
        .put_local(
            &ItemId {
                kind: KIND_CONTACT_OVERLAY.to_string(),
                key,
            },
            value.to_vec(),
            // Every register carries its own stamp; the item has no outer one.
            None,
        )
        .await
        .context("contact overlay: plane put")?;
    Ok(OverlayWriteOutcome::Written(next))
}

/// Fold the overlay on `predecessor_hex` forward onto its verified successor
/// `successor_hex` (`fauna_core::contact_overlay::fold_succession` owns the
/// rule; `contacts.md` § The private overlay → *When a person's identity
/// succeeds* owns the trigger). Both items are read and written on this one
/// store thread, successor first — a crash between the two puts leaves the
/// predecessor still live, which the next reconcile folds again (the join
/// makes the repeat harmless). Whether anything was written: `false` when the
/// predecessor says nothing, the reconcile's fixed point.
///
/// Stamps come from the fold itself, never a clock, so two devices folding
/// the same pair write the same bytes.
pub async fn fold_contact_overlay<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    predecessor_hex: &str,
    successor_hex: &str,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let (Some(pred_key), Some(succ_key)) =
        (overlay_key(predecessor_hex), overlay_key(successor_hex))
    else {
        bail!(
            "contact overlay fold: {predecessor_hex:?} → {successor_hex:?} is not an actor-id pair"
        );
    };
    if pred_key == succ_key {
        return Ok(false);
    }
    let predecessor = read_contact_overlay(store, &pred_key).await?;
    let successor = read_contact_overlay(store, &succ_key).await?;
    let Some(fold) = fauna_core::contact_overlay::fold_succession(&predecessor, &successor) else {
        return Ok(false);
    };
    for (key, overlay) in [(succ_key, &fold.successor), (pred_key, &fold.predecessor)] {
        let value =
            fauna_core::encoding::canonical_encode(overlay).context("encode contact overlay")?;
        fleet
            .put_local(
                &ItemId {
                    kind: KIND_CONTACT_OVERLAY.to_string(),
                    key,
                },
                value.to_vec(),
                None,
            )
            .await
            .context("contact overlay fold: plane put")?;
    }
    Ok(true)
}

/// The non-empty overlays among `entries` of `fauna.state.contact-overlay`,
/// keyed by actor id — the projection's load. An entry that does not decode,
/// or whose key is not an actor id, is skipped (a later build's shape is not
/// this one's to read); an all-`None` overlay reads as no overlay.
pub fn overlays_of(entries: &[StateEntry]) -> BTreeMap<String, ContactOverlay> {
    entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_CONTACT_OVERLAY)
        .filter_map(|e| {
            let key = overlay_key(&e.key)?;
            let overlay: ContactOverlay = fauna_core::encoding::canonical_decode(&e.value).ok()?;
            (!overlay.is_empty()).then_some((key, overlay))
        })
        .collect()
}
