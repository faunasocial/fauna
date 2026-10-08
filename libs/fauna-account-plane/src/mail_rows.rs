//! The account's mail custody's production writer and reader — the typed
//! door for `fauna.state.mail` (`fauna_core::mail_rows` owns the rows, their
//! key grammar and their joins; `mail-credentials.md` owns the concept;
//! `config-dissolution.md` § Phases and gates → *Bounded rows* → *The mail
//! plane* owns the two-family shape).
//!
//! **Three row families:** `self`, the ONE `MailStateRow` (the MSEK, the
//! burns, the rotation sentinel, the flags), `credential/<credential_id>`, one
//! `MailCredential` each, and `generation/<fingerprint>`, one retired MSEK
//! generation each — every one ever retired, uncapped. The composite
//! `MailConfig` the mail stack works on is the READ fold ([`mail_of`]): the
//! state row, every generation as the priors, plus every credential that is
//! not revoked, burned rows shown.
//!
//! **Every write is a read-join-put on the store thread**, per row: the
//! stored row is read, the caller's intent is stamped strictly above it,
//! joined with it (the half the plane arm runs on a sibling's row) and put
//! through the fleet plane's REAL writer door only when the join moved it —
//! so a write never loses what the store gained since the caller last read
//! (another device's burn, merged in by a walk), and an unchanged intent
//! writes nothing. There is no deletion: a revoke writes the monotone marker
//! (with `wrapped_under = None` and an empty secret, together), and a marked
//! row is never re-wrapped. The kind is `GenerationTip`-sealed, so a put while
//! no tip resolves is refused at the writer door and surfaces to the caller.
//! Nothing between the read and the put yields to a walk (`Cmd::is_local`).
//! Each put is the **local write only** ([`AccountStatePlane::put_local`]);
//! the account runtime's publish step ships it.
//!
//! Exposed through `fauna_sync_engine::account_runtime::AccountStoreHandle`'s
//! typed doors (`mail`, `write_mail_state`, `put_mail_credential`,
//! `mark_mail_credential_wrapped`, `revoke_mail_credential`,
//! `retire_mail_generation`).

use anyhow::{Context, Result, ensure};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::{
    MailConfig, MailCredential, MsekFingerprint, PriorMsekRetirement, Timestamp,
};
use fauna_core::mail_rows::{MailRecord, MailRowKey, MailRows, MailStateRow, decode_mail_row};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_MAIL;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// The account's mail rows among `entries` (a `states_of_kind(KIND_MAIL)`
/// read), decoded and joined — revoked rows included. A row whose key does
/// not parse or whose value does not decode exactly fails loudly: silently
/// skipping it could hide the MSEK.
pub fn mail_rows_of(entries: &[StateEntry]) -> Result<MailRows> {
    let mut rows = MailRows::default();
    for entry in entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_MAIL)
    {
        rows.fold_row(&entry.key, &entry.value)
            .with_context(|| format!("the stored mail row {:?}", entry.key))?;
    }
    Ok(rows)
}

/// The READ fold — the composite `MailConfig` over `entries`
/// ([`MailRows::config`]).
pub fn mail_of(entries: &[StateEntry]) -> Result<MailConfig> {
    Ok(mail_rows_of(entries)?.config())
}

/// This account's mail custody, folded — the default (mail never enabled)
/// when no row rests.
pub async fn read_mail<B: StoreBackend>(store: &AccountStore<B>) -> Result<MailConfig> {
    mail_of(&store.states_of_kind(KIND_MAIL).await?)
}

/// The stamp a write takes: `now`, or strictly above the stored row's when the
/// clock is behind it, so the write orders after what it read.
fn stamp_above(stored: Option<Timestamp>, now: Timestamp) -> Timestamp {
    match stored {
        Some(at) if at >= now => Timestamp(at.0.saturating_add(1)),
        _ => now,
    }
}

async fn read_row<B: StoreBackend>(
    store: &AccountStore<B>,
    key: &MailRowKey,
) -> Result<Option<(MailRecord, Vec<u8>)>> {
    let key = key.key();
    match store.state(KIND_MAIL, &key).await? {
        Some(entry) if !entry.tombstone => {
            let record = decode_mail_row(&key, &entry.value)
                .with_context(|| format!("the stored mail row {key:?}"))?;
            Ok(Some((record, entry.value.to_vec())))
        }
        _ => Ok(None),
    }
}

/// Join `intent` into the stored row at its key and put the join when it
/// moved the bytes. Whether anything was written.
async fn join_and_put<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    stored: Option<(MailRecord, Vec<u8>)>,
    intent: MailRecord,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let key = intent.plane_key().context("mail row: key")?;
    let joined = match &stored {
        Some((record, _)) => record.merge(&intent),
        // A self-join normalizes (a marked credential's secret emptied).
        None => intent.merge(&intent),
    }
    .context("mail row: join")?
    .encode()
    .context("encode mail row")?;
    if stored.as_ref().is_some_and(|(_, bytes)| *bytes == joined) {
        return Ok(false);
    }
    fleet
        .put_local(
            &ItemId {
                kind: KIND_MAIL.to_string(),
                key,
            },
            joined,
            // A per-field CRDT row carrying its own stamp: no outer one.
            None,
        )
        .await
        .context("mail row: plane put")?;
    Ok(true)
}

/// Write the account's mail-state row: `state`'s content, stamped strictly
/// above the stored row and joined with it (the MSEK present-wins, the
/// window and burns union; the recreatable four follow this newer stamp).
/// `state.updated_at` is ignored. Whether anything was written — `false` when
/// the stored row already holds this content.
pub async fn write_mail_state<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    state: &MailStateRow,
    now: Timestamp,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let stored = read_row(store, &MailRowKey::State).await?;
    let stored_state = match &stored {
        Some((MailRecord::State(s), _)) => Some(s),
        Some(_) => unreachable!("decode_mail_row keys the state row as a state"),
        None => None,
    };
    if stored_state.is_some_and(|s| s.same_content(state)) {
        return Ok(false);
    }
    let intent = MailStateRow {
        updated_at: stamp_above(stored_state.map(|s| s.updated_at), now),
        ..state.clone()
    };
    join_and_put(fleet, stored, MailRecord::State(intent)).await
}

async fn stored_credential<B: StoreBackend>(
    store: &AccountStore<B>,
    credential_id: &str,
) -> Result<Option<(MailCredential, Vec<u8>)>> {
    Ok(
        match read_row(store, &MailRowKey::Credential(credential_id.to_string())).await? {
            Some((MailRecord::Credential(c), bytes)) => Some((c, bytes)),
            Some(_) => unreachable!("decode_mail_row keys a credential row as a credential"),
            None => None,
        },
    )
}

/// Put `credential` at its own `credential/<credential_id>` — a read-join-put:
/// stamped strictly above the stored row and joined with it, so a marker the
/// store holds survives (a burned or revoked row is never un-marked by a
/// re-put). `credential.updated_at` is ignored. Whether anything was written.
pub async fn put_credential<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    credential: &MailCredential,
    now: Timestamp,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    ensure!(
        !credential.credential_id.is_empty(),
        "a mail credential needs a credential_id to key its row"
    );
    let stored = stored_credential(store, &credential.credential_id).await?;
    if let Some((c, _)) = &stored
        && *c
            == (MailCredential {
                updated_at: c.updated_at,
                ..credential.clone()
            })
    {
        return Ok(false);
    }
    let intent = MailCredential {
        updated_at: stamp_above(stored.as_ref().map(|(c, _)| c.updated_at), now),
        ..credential.clone()
    };
    join_and_put(
        fleet,
        stored.map(|(c, b)| (MailRecord::Credential(c), b)),
        MailRecord::Credential(intent),
    )
    .await
}

/// Record that `credential_id`'s nest-side blobs are wrapped under the
/// generation `fingerprint` names (*The generation marker*): the remainder's
/// stamp advanced, the marker written. Whether anything was written —
/// `false` when no row rests there, when it is already marked burned or
/// revoked (a marked row is never re-wrapped), or when it already names the
/// generation.
pub async fn mark_credential_wrapped<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    credential_id: &str,
    fingerprint: MsekFingerprint,
    now: Timestamp,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let Some((stored, bytes)) = stored_credential(store, credential_id).await? else {
        return Ok(false);
    };
    if stored.is_marked() || stored.wrapped_under == Some(fingerprint) {
        return Ok(false);
    }
    let intent = MailCredential {
        wrapped_under: Some(fingerprint),
        updated_at: stamp_above(Some(stored.updated_at), now),
        ..stored.clone()
    };
    join_and_put(
        fleet,
        Some((MailRecord::Credential(stored), bytes)),
        MailRecord::Credential(intent),
    )
    .await
}

/// Revoke `credential_id` — the soft-revoke marker (`revoked_at_unix`, the
/// instant `now` names) written together with `wrapped_under = None` and the
/// secret emptied, stamped above the stored row. The id stays spent. Whether
/// anything was written — `false` when no row rests there or it is already
/// revoked.
pub async fn revoke_credential<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    credential_id: &str,
    now: Timestamp,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let Some((stored, bytes)) = stored_credential(store, credential_id).await? else {
        return Ok(false);
    };
    if stored.revoked_at_unix.is_some() {
        return Ok(false);
    }
    let intent = MailCredential {
        revoked_at_unix: Some(now.0 / 1_000_000),
        wrapped_under: None,
        secret: Default::default(),
        updated_at: stamp_above(Some(stored.updated_at), now),
        ..stored.clone()
    };
    join_and_put(
        fleet,
        Some((MailRecord::Credential(stored), bytes)),
        MailRecord::Credential(intent),
    )
    .await
}

/// Record a retired MSEK generation at `generation/<fingerprint>` — a
/// read-join-put: a row already there keeps the later instant
/// (`fauna_core::mail_rows::merge_generation`), and nothing is written when it
/// already holds this one. The row carries no stamp of its own: it is
/// immutable but for that join. Whether anything was written.
pub async fn put_generation<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    generation: &PriorMsekRetirement,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let key = MailRowKey::Generation(MsekFingerprint::of(&generation.msek));
    let stored = read_row(store, &key).await?;
    join_and_put(fleet, stored, MailRecord::Generation(generation.clone())).await
}

#[cfg(test)]
mod tests {
    use super::stamp_above;
    use fauna_core::data::Timestamp;

    /// A write orders strictly after the row it read, whatever the clock.
    #[test]
    fn a_write_stamps_strictly_above_the_stored_row() {
        assert_eq!(stamp_above(None, Timestamp(5)), Timestamp(5));
        assert_eq!(stamp_above(Some(Timestamp(3)), Timestamp(5)), Timestamp(5));
        assert_eq!(stamp_above(Some(Timestamp(5)), Timestamp(5)), Timestamp(6));
        assert_eq!(stamp_above(Some(Timestamp(9)), Timestamp(5)), Timestamp(10));
    }
}
