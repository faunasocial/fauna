//! The in-process Nostr DM leg of the bridged-conversation family — first-party
//! use of the seam a third-party conversation bridge uses
//! (`docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
//! adapter*, rulings 2 (e) and 3; `docs/goal/ui/nostr.md` § Implementation
//! status today → DMs, the 2026-10-02 migration ruling).
//!
//! **Identity.** The leg is in-process, so it holds no roster row: its
//! principal id is [`NOSTR_LEG_PRINCIPAL_ID`], its declared shape
//! `nostr_leg_block` (identity `{ nostr, Nostr, bolt }`), and it serves an
//! account exactly when that account has a Nostr account linked
//! ([`serves`]). Its X25519 key is minted on first use and kept in
//! `first_party_bridge_keys`. The seam it shares with the other first-party
//! legs is [`crate::bridge_legs`].
//!
//! **Inbound** is an internal caller of the deposit core:
//! [`super::sync_worker::process_gift_wrap_inbound`] unwraps and hands the
//! plaintext, keyed on the seal's authenticated pubkey, to
//! [`crate::bridge_legs::deposit_gated`] — the family gate's verdict, the seal
//! to the recipient key and the deposit, a room per `(account, peer pubkey)`,
//! idempotent on the gift wrap's event id.
//!
//! **Outbound** is the family's outbox, drained here ([`drain_outbox`]): each
//! item is opened under the leg's key — the one honest exception to the blind
//! outbox, which the room's transport-only class already says — NIP-17
//! gift-wrapped with the account's custodial key, and queued on the relay
//! channel. The Sent copy is the row the family's `send` stored.

use fauna_bridge_nostr::nip17::wrap_dm;
use fauna_bridge_nostr::signing::Keypair;

use crate::db::CacheDb;
use crate::db::bridged_conversations::{NOSTR_LEG_BRIDGE_ID, NOSTR_LEG_PRINCIPAL_ID};
use crate::nostr::relays::resolve_relay_urls;
use crate::nostr::sync_worker::OutboundEvent;
use crate::nostr::{db, key_crypto};
use crate::routes::AppState;

/// Items one drain pass takes — the family's page ceiling.
const DRAIN_PAGE: u32 = fauna_protocol::bridged_conversations::BRIDGED_PAGE_MAX;

/// The far room id of the leg's room with `address`: the peer's canonical
/// lowercase 32-byte hex, whether `address` spells it `npub1…` or as hex.
/// `None` for an address that names no Nostr key.
#[must_use]
pub fn far_room_id(address: &str) -> Option<String> {
    crate::nostr::canonical_peer_pubkey(address).or_else(|| {
        fauna_bridge_nostr::nip19::decode_npub(address)
            .ok()
            .map(|bytes| fauna_core::hex32::encode(&bytes))
    })
}

/// Does the leg serve `account` — is a Nostr account linked?
///
/// # Errors
/// A database fault.
pub async fn serves(db: &CacheDb, account: &[u8; 32]) -> anyhow::Result<bool> {
    let conn = db.conn().await;
    Ok(db::get_account(&conn, &hex::encode(account))?.is_some())
}

/// Why the leg cannot deliver what an account sends, known before anything is
/// queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendRefused {
    /// The account's Nostr key is not on this nest (a `remote` / NIP-07 link).
    NoKey,
    /// The account emptied its relay list: there is nowhere to deliver.
    NoRelays,
}

/// Could the leg deliver a message `account` sends right now? Asked by the
/// family's `send` before it queues on a leg room, so a message that can never
/// leave is refused by name rather than stored as Sent and dropped at the
/// drain (`docs/goal/ui/nostr.md` § Errors & edge cases — an emptied relay
/// list is refused loudly, never defaulted).
///
/// # Errors
/// A database fault.
pub async fn send_precheck(
    db: &CacheDb,
    account: &[u8; 32],
) -> anyhow::Result<Option<SendRefused>> {
    let acct = {
        let conn = db.conn().await;
        db::get_account(&conn, &hex::encode(account))?
    };
    let Some(acct) = acct.filter(|a| a.encrypted_privkey.is_some()) else {
        return Ok(Some(SendRefused::NoKey));
    };
    if resolve_relay_urls(acct.relay_list.as_deref()).is_empty() {
        return Ok(Some(SendRefused::NoRelays));
    }
    Ok(None)
}

/// Why one outbox item was not relayed.
#[derive(Debug, thiserror::Error)]
enum Undeliverable {
    #[error("the item does not open under the leg's key")]
    Unopenable,
    #[error("the account has no custodial Nostr key")]
    NoKey,
    #[error("the account's relay list is empty")]
    NoRelays,
    #[error("the far room id is not a Nostr key")]
    BadPeer,
}

/// Drain the leg's outbox: every queued item is opened, gift-wrapped and
/// queued on the relay channel, then acked. An item that can never be
/// delivered (it does not open, the account unlinked its key or emptied its
/// relays, the room names no key) is acked with a warning — the Sent row
/// stays; a full relay channel leaves the rest for the next pass. Returns
/// each relayed item's id with its gift wrap's event id.
///
/// # Errors
/// A database fault.
pub async fn drain_outbox(state: &AppState) -> anyhow::Result<Vec<(i64, String)>> {
    // One pass at a time: the send's nudge and the sync worker's tick would
    // both fetch the same undrained item and relay it twice.
    static DRAINING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one_pass = DRAINING.lock().await;
    let items = state
        .db
        .fetch_principal_outbox(&NOSTR_LEG_PRINCIPAL_ID, DRAIN_PAGE)
        .await?;
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let (secret, _) = state.db.first_party_bridge_key(NOSTR_LEG_BRIDGE_ID).await?;
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let mut relayed = Vec::new();
    for (actor, item) in items {
        let Ok(account) = <[u8; 32]>::try_from(actor.as_slice()) else {
            continue;
        };
        let wrapped = wrap_item(state, &account, &nest_key, &secret, &item).await;
        match wrapped {
            Ok(outbound) => {
                let event_id = outbound.event.id.clone();
                if state.nostr.sync_tx.send(outbound).await.is_err() {
                    // No worker is running: leave the rest queued.
                    break;
                }
                relayed.push((item.id, event_id));
            }
            Err(e) => {
                tracing::warn!(id = item.id, "nostr leg: outbound item dropped: {e}");
            }
        }
        state
            .db
            .ack_bridged_outbox(&account, &NOSTR_LEG_PRINCIPAL_ID, &[item.id])
            .await?;
    }
    Ok(relayed)
}

async fn wrap_item(
    state: &AppState,
    account: &[u8; 32],
    nest_key: &[u8; 32],
    leg_secret: &[u8; 32],
    item: &crate::db::bridged_conversations::BridgedOutboxRow,
) -> anyhow::Result<OutboundEvent> {
    let body = crate::bridge_legs::open_outbox_text(&item.ciphertext, leg_secret)
        .ok_or(Undeliverable::Unopenable)?;
    let acct = {
        let conn = state.db.conn().await;
        db::get_account(&conn, &hex::encode(account))?
    };
    let encrypted = acct
        .as_ref()
        .and_then(|a| a.encrypted_privkey.clone())
        .ok_or(Undeliverable::NoKey)?;
    let relay_urls = resolve_relay_urls(acct.as_ref().and_then(|a| a.relay_list.as_deref()));
    if relay_urls.is_empty() {
        return Err(Undeliverable::NoRelays.into());
    }
    let recipient =
        fauna_core::hex32::decode(&item.far_room_id).map_err(|_| Undeliverable::BadPeer)?;
    let secret = key_crypto::decrypt_nostr_privkey(nest_key, &encrypted)
        .map_err(|e| anyhow::anyhow!("key decryption: {e}"))?;
    let sender = Keypair::from_secret_bytes(secret).map_err(|e| anyhow::anyhow!("keypair: {e}"))?;
    let event = wrap_dm(&sender, &recipient, &body).map_err(|e| anyhow::anyhow!("wrap: {e}"))?;
    Ok(OutboundEvent { event, relay_urls })
}
