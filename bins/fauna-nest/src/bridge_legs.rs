//! The first-party leg seam of the bridged-conversation family — the one
//! place the family's handlers and the in-process legs meet
//! (`docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
//! adapter*, rulings 2 (e) and 3).
//!
//! A first-party leg is a bridge of the family's own shape that runs inside
//! the nest: it holds no roster row, its principal id is a constant, its
//! X25519 key is minted on first use (`first_party_bridge_keys`), and it
//! serves an account exactly when that account has the far network linked.
//! Three exist, each behind its cargo feature:
//!
//! | leg | serves an account when | inbound caller | outbound drain |
//! |---|---|---|---|
//! | [`NOSTR`] | a Nostr account is linked | `nostr::sync_worker` (NIP-17 unwrap) | `nostr::bridge_leg` |
//! | [`BLUESKY`] | a consume-side Bluesky link, no nest-hosted identity (D7) | `bluesky::dm_worker` (`chat.bsky` poll) | `bluesky::dm_leg` |
//! | [`ACTIVITYPUB`] | an enabled ActivityPub actor | `activitypub::dm_leg` (inbox `Create{Note}`) | `activitypub::dm_leg` |
//!
//! **Inbound is one function, [`deposit_gated`]:** every leg hands it the
//! plaintext it just received and the peer identity its transport proved, and
//! the family gate's verdict, the seal to the recipient key and the deposit
//! happen here, in that order — so no leg can store a deliverable DM without
//! composing the verdict (`docs/goal/behavior/family-safety.md` § The
//! bridge-DM gate → *Enforcement points*).

use std::sync::Arc;

use fauna_core::data::{DmVerdict, UnknownPeerDm, supervised_dm_verdict};
use fauna_protocol::RpcError;
use fauna_protocol::kind_manifest::BridgeBlock;

use crate::db::CacheDb;
use crate::db::bridged_conversations::{
    self as family, BridgeSeat, BridgedRefused, DepositPrecheck, Deposited, RoomShape,
};
use crate::routes::AppState;

/// One first-party leg's fixed identity.
pub struct LegDef {
    /// The bridge id — the string rooms and the gate's verdict rows key on.
    pub bridge_id: &'static str,
    /// The declared label.
    pub label: &'static str,
    /// The principal id the leg's rooms seat as their bridge member.
    pub principal_id: &'static [u8; 16],
    /// The declared block (identity, grammar, capability vector).
    pub block: fn() -> BridgeBlock,
    /// What the seal's warn-log calls this leg's rows.
    seal_label: &'static str,
}

/// The Nostr DM leg.
pub const NOSTR: LegDef = LegDef {
    bridge_id: family::NOSTR_LEG_BRIDGE_ID,
    label: family::NOSTR_LEG_LABEL,
    principal_id: &family::NOSTR_LEG_PRINCIPAL_ID,
    block: family::nostr_leg_block,
    seal_label: "nostr-dm",
};

/// The Bluesky DM leg.
pub const BLUESKY: LegDef = LegDef {
    bridge_id: family::BLUESKY_LEG_BRIDGE_ID,
    label: family::BLUESKY_LEG_LABEL,
    principal_id: &family::BLUESKY_LEG_PRINCIPAL_ID,
    block: family::bluesky_leg_block,
    seal_label: "bluesky-dm",
};

/// The ActivityPub DM leg.
pub const ACTIVITYPUB: LegDef = LegDef {
    bridge_id: family::ACTIVITYPUB_LEG_BRIDGE_ID,
    label: family::ACTIVITYPUB_LEG_LABEL,
    principal_id: &family::ACTIVITYPUB_LEG_PRINCIPAL_ID,
    block: family::activitypub_leg_block,
    seal_label: "activitypub-dm",
};

/// A leg as one of an account's serving bridges.
pub struct Leg {
    pub principal_id: Vec<u8>,
    pub label: String,
    pub block: BridgeBlock,
    pub x25519: [u8; 32],
}

impl LegDef {
    // reason: called only from a bridge feature's arm of `serving`; a build
    // with none of the three compiles no caller.
    #[allow(dead_code)]
    async fn leg(&self, db: &CacheDb) -> anyhow::Result<Leg> {
        let (_, x25519) = db.first_party_bridge_key(self.bridge_id).await?;
        Ok(Leg {
            principal_id: self.principal_id.to_vec(),
            label: self.label.to_string(),
            block: (self.block)(),
            x25519,
        })
    }
}

/// The first-party legs that serve `account`, in a fixed order.
///
/// # Errors
/// A database fault.
#[allow(unused_variables, unused_mut)]
pub async fn serving(db: &CacheDb, account: &[u8; 32]) -> anyhow::Result<Vec<Leg>> {
    let mut legs = Vec::new();
    #[cfg(feature = "nostr")]
    if crate::nostr::bridge_leg::serves(db, account).await? {
        legs.push(NOSTR.leg(db).await?);
    }
    #[cfg(feature = "bluesky")]
    if crate::bluesky::dm_leg::serves(db, account).await? {
        legs.push(BLUESKY.leg(db).await?);
    }
    #[cfg(feature = "activitypub")]
    if crate::activitypub::dm_leg::serves(db, account).await? {
        legs.push(ACTIVITYPUB.leg(db).await?);
    }
    Ok(legs)
}

/// The far room id `address` names on the leg `principal_id`, where the leg
/// keys its rooms on something other than the address as typed: the Nostr
/// leg's canonical hex, whichever spelling names the key, and the ActivityPub
/// leg's canonical actor URI. `None` for a principal that is no such leg, or
/// an address the leg does not canonicalise.
#[must_use]
#[allow(unused_variables)]
pub fn far_room_id(principal_id: &[u8], address: &str) -> Option<String> {
    #[cfg(feature = "nostr")]
    if principal_id == NOSTR.principal_id {
        return crate::nostr::bridge_leg::far_room_id(address);
    }
    #[cfg(feature = "activitypub")]
    if principal_id == ACTIVITYPUB.principal_id {
        return crate::activitypub::dm_leg::canonical_actor(address);
    }
    None
}

/// Could the leg `principal_id` deliver a message `account` sends right now?
/// Asked by the family's `send` before it queues on a leg room, so a message
/// that can never leave is refused by name rather than stored as Sent and
/// dropped at the drain. `Ok` for a principal that is no leg.
///
/// # Errors
/// The leg's typed refusal; a database fault.
#[allow(unused_variables)]
pub async fn send_precheck(
    state: &AppState,
    principal_id: &[u8],
    account: &[u8; 32],
) -> Result<(), RpcError> {
    #[cfg(feature = "nostr")]
    if principal_id == NOSTR.principal_id {
        use crate::nostr::bridge_leg::SendRefused;
        return match crate::nostr::bridge_leg::send_precheck(&state.db, account)
            .await
            .map_err(crate::rpc_errors::internal)?
        {
            Some(SendRefused::NoKey) => Err(crate::rpc_errors::invalid_params_ns(
                "nostr",
                "sending DMs requires a locally-stored private key (generate or import mode)",
            )),
            Some(SendRefused::NoRelays) => Err(crate::rpc_errors::no_relays_configured_ns("nostr")),
            None => Ok(()),
        };
    }
    Ok(())
}

/// Nudge the leg `principal_id` to drain its outbox now rather than at its
/// worker's next tick. A no-op for a principal that is no leg.
#[allow(unused_variables)]
pub fn nudge_drain(state: &Arc<AppState>, principal_id: &[u8]) {
    #[cfg(feature = "nostr")]
    if principal_id == NOSTR.principal_id {
        let drain = state.clone();
        state.spawn_scoped(async move {
            if let Err(e) = crate::nostr::bridge_leg::drain_outbox(&drain).await {
                tracing::warn!("nostr leg: drain after send: {e:#}");
            }
        });
    }
    #[cfg(feature = "bluesky")]
    if principal_id == BLUESKY.principal_id {
        let drain = state.clone();
        state.spawn_scoped(async move {
            let far = crate::bluesky::dm_leg::SessionChat { state: &drain };
            if let Err(e) = crate::bluesky::dm_leg::drain_outbox(&drain, &far).await {
                tracing::warn!("bluesky leg: drain after send: {e:#}");
            }
        });
    }
    #[cfg(feature = "activitypub")]
    if principal_id == ACTIVITYPUB.principal_id {
        let drain = state.clone();
        state.spawn_scoped(async move {
            if let Err(e) = crate::activitypub::dm_leg::drain_outbox(&drain).await {
                tracing::warn!("activitypub leg: drain after send: {e:#}");
            }
        });
    }
}

/// Push the family's content-free nudge for the leg's room with `peer` — what
/// a leg calls after its deposit stored a row.
pub fn notify_changed(state: &AppState, leg: &LegDef, account: &[u8; 32], peer: &str) {
    crate::bridged_conversation_handlers::notify_changed(
        state,
        account,
        &family::bridged_room_id(account, leg.bridge_id, peer),
    );
}

/// One direct message a leg received, as plaintext in flight.
pub struct InboundDm<'a> {
    /// The far party of the room — **an identity the leg's transport proved**
    /// (a decryption, a session binding, a signature check), never a field the
    /// counterparty merely asserts (`family-safety.md` § The bridge-DM gate →
    /// *The peer identity the verdict keys on*). The room and the verdict key
    /// on it.
    pub peer: &'a str,
    /// Who wrote the message: `peer`, or the account's own far address for a
    /// copy of something it sent from another app.
    pub sender: &'a str,
    /// The account's own address on the far network.
    pub self_address: &'a str,
    /// The far network's id of the message — the deposit is idempotent on it.
    pub far_message_id: &'a str,
    pub plaintext: &'a [u8],
    /// The far side's claimed time, unix ms — carried, never ordered on.
    pub created_at_ms: i64,
}

/// What [`deposit_gated`] did with a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inbound {
    /// A new row, sealed to the recipient key.
    Stored,
    /// The far message id is already stored.
    Duplicate,
    /// The account's guardian has blocked this peer: refused before storage.
    Blocked,
    /// The recipient has no seal key on file: nothing stored (fail closed).
    NoSealKey,
    /// An inbound cap is reached: nothing stored.
    Full,
}

/// Would a deposit of `far_message_id` on `leg` for `account` be refused
/// anyway — already stored, or past an inbound cap? The cheap answer a leg
/// asks before paying for an unwrap or a fetch it would then throw away.
///
/// # Errors
/// A database fault.
pub async fn precheck(
    db: &CacheDb,
    leg: &LegDef,
    account: &[u8; 32],
    far_message_id: &str,
) -> anyhow::Result<Option<DepositPrecheck>> {
    db.bridged_deposit_precheck(account, leg.principal_id, far_message_id)
        .await
}

/// The one inbound seam every first-party leg writes through: compose the
/// family gate's verdict on `dm.peer`, seal the plaintext to the recipient's
/// key, and store the row through the deposit core — the room for `(account,
/// peer)` found or minted, the row idempotent on the far message id.
///
/// Only `Blocked` acts, and it acts before storage; `Held` stores exactly as
/// `Deliver` does, because the hold is computed at read time and never stored.
/// A policy or verdict read that fails is an error, never a store past an
/// unread policy.
///
/// # Errors
/// A database fault; a seal fault.
pub async fn deposit_gated(
    db: &CacheDb,
    leg: &LegDef,
    account: &[u8; 32],
    dm: &InboundDm<'_>,
) -> anyhow::Result<Inbound> {
    match precheck(db, leg, account, dm.far_message_id).await? {
        Some(DepositPrecheck::Duplicate) => return Ok(Inbound::Duplicate),
        Some(DepositPrecheck::Full) => return Ok(Inbound::Full),
        None => {}
    }

    // Only a supervised recipient pays for the verdict lookup.
    if let Some(policy) = db.get_guardian_policy(account).await? {
        let verdict = db.dm_peer_verdict(account, leg.bridge_id, dm.peer).await?;
        if supervised_dm_verdict(
            Some(UnknownPeerDm::from_wire(&policy.unknown_peer_dm)),
            verdict.as_deref(),
        ) == DmVerdict::Blocked
        {
            return Ok(Inbound::Blocked);
        }
    }

    let Some(seal_key) = db.get_recipient_seal_key(account).await? else {
        return Ok(Inbound::NoSealKey);
    };
    let sealed = crate::bridge_routing_handlers::seal_recipient_blob(
        dm.plaintext,
        &seal_key.mls_pubkey,
        Some(seal_key.mlkem_ek.as_slice()),
        leg.seal_label,
    )
    .map_err(|e| anyhow::anyhow!("seal {}: {e:?}", leg.seal_label))?;
    let sealed = fauna_mls::wrapped_blob::SealedRecordBytes::verify(sealed)
        .map_err(|e| anyhow::anyhow!("verify own seal: {e}"))?;

    deposit_sealed(
        db,
        leg,
        account,
        &SealedDm {
            peer: dm.peer,
            sender: dm.sender,
            self_address: dm.self_address,
            far_message_id: dm.far_message_id,
            sealed: sealed.as_slice(),
            created_at_ms: dm.created_at_ms,
        },
    )
    .await
}

/// One direct message already sealed to the recipient key — [`InboundDm`]
/// past the gate and the seal.
pub struct SealedDm<'a> {
    pub peer: &'a str,
    pub sender: &'a str,
    pub self_address: &'a str,
    pub far_message_id: &'a str,
    pub sealed: &'a [u8],
    pub created_at_ms: i64,
}

/// The storage half of [`deposit_gated`]: the room for `(account, peer)`
/// found or minted on the leg's seat, the sealed row stored idempotently.
/// **Composes no verdict** — a leg's inbound calls [`deposit_gated`]; this is
/// public for a suite that seeds a stored conversation to test what the read
/// surfaces then compute from it.
///
/// # Errors
/// A database fault.
pub async fn deposit_sealed(
    db: &CacheDb,
    leg: &LegDef,
    account: &[u8; 32],
    dm: &SealedDm<'_>,
) -> anyhow::Result<Inbound> {
    let (_, x25519) = db.first_party_bridge_key(leg.bridge_id).await?;
    let block = (leg.block)();
    let seat = BridgeSeat {
        principal_id: leg.principal_id,
        bridge: &block,
        bridge_x25519: &x25519,
    };
    let shape = RoomShape {
        label: None,
        participants: vec![dm.peer.to_string()],
        self_address: Some(dm.self_address.to_string()),
    };
    let stored = async {
        db.upsert_bridged_room(account, &seat, dm.peer, &shape)
            .await?;
        db.deposit_bridged_message(
            account,
            leg.principal_id,
            dm.peer,
            dm.far_message_id,
            dm.sender,
            dm.sealed,
            dm.created_at_ms,
        )
        .await
    }
    .await;
    match stored {
        Ok((_, Deposited::Stored(_))) => Ok(Inbound::Stored),
        Ok((_, Deposited::Duplicate(_))) => Ok(Inbound::Duplicate),
        Err(e) if e.downcast_ref::<BridgedRefused>() == Some(&BridgedRefused::Full) => {
            Ok(Inbound::Full)
        }
        Err(e) => Err(e),
    }
}

/// Open one outbox item under a leg's key: the UTF-8 text the user's app
/// sealed to the leg — the one honest exception to the blind outbox, which
/// the room's transport-only class already says. `None` for an item that does
/// not open, or is not text.
#[must_use]
pub fn open_outbox_text(ciphertext: &[u8], leg_secret: &[u8; 32]) -> Option<String> {
    let env = fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(ciphertext).ok()?;
    let plaintext = fauna_mls::wrapped_blob::unseal_mail_record(&env, leg_secret).ok()?;
    String::from_utf8(plaintext).ok()
}

/// What every leg's in-crate tests set up the same way.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::db::CacheDb;

    /// Apply every compiled-in bridge's tables — what [`super::serving`] reads
    /// to decide which legs serve an account, and what a booted nest of that
    /// flavor always holds.
    pub(crate) async fn init_leg_tables(db: &CacheDb) {
        let _ = db;
        #[cfg(feature = "nostr")]
        crate::nostr::init_db(db).await.unwrap();
        #[cfg(feature = "bluesky")]
        crate::bluesky::init_db(db).await.unwrap();
        #[cfg(feature = "activitypub")]
        crate::activitypub::init_db(db).await.unwrap();
    }

    /// Put `ward` under a guardian's supervision (creating both accounts) and
    /// record the guardian's `block` on `peer` over `bridge_id`. Compiled only
    /// with a leg whose tests call it.
    #[cfg(any(feature = "bluesky", feature = "activitypub"))]
    pub(crate) async fn supervised_with_block(
        db: &CacheDb,
        guardian: &[u8; 32],
        ward: &[u8; 32],
        bridge_id: &str,
        peer: &str,
    ) {
        db.create_user_with_handle(guardian, "personal", "parent", None)
            .await
            .unwrap();
        db.create_user_with_handle(ward, "personal", "kid", Some(&guardian[..]))
            .await
            .unwrap();
        db.set_dm_peer_verdict(
            &ward[..],
            bridge_id,
            peer,
            fauna_core::data::DmPeerVerdict::Block,
        )
        .await
        .unwrap();
    }
}
