//! The in-process Bluesky DM leg of the bridged-conversation family
//! (`docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
//! adapter*, rulings 2 (e) and 3) — `chat.bsky` direct messages of a
//! consume-side linked account. The seam it shares with the other first-party
//! legs is [`crate::bridge_legs`].
//!
//! **Identity.** `{ bluesky, Bluesky, butterfly }`, principal id
//! `BLUESKY_LEG_PRINCIPAL_ID`. The leg serves an account exactly when the
//! consume-side poller may run for it ([`serves`]): a linked external-DID
//! account holding **no** active nest-hosted ATProto identity — D7's predicate
//! (`docs/goal/behavior/atproto-pds-full.md` § D7), the one the notification
//! and feed pollers gate on.
//!
//! **Inbound** ([`ingest_convo`]) is the ingest half of
//! [`super::dm_worker`]'s poll, split from the network half so every decision
//! about what came back is pinned without a far end. A room is the one-to-one
//! conversation with a peer, keyed on the peer's DID.
//!
//! **The peer identity the gate keys on** is the DID the chat service names,
//! under the account's own OAuth session: a message's `sender.did` and the
//! conversation's member list are the service's statements, not fields the
//! counterparty writes (`docs/goal/behavior/family-safety.md` § The bridge-DM
//! gate → *The peer identity the verdict keys on*). The branch that picks the
//! peer asks only "did this account send it?", answered from that same
//! service-stated sender.
//!
//! **Outbound** ([`drain_outbox`]) opens each queued item under the leg's key
//! — the family's one honest exception to the blind outbox — sends it through
//! `chat.bsky.convo.sendMessage` under the account's session, and stamps the
//! Sent row with the far message id, so the poll that later reads the same
//! message back finds it already stored.

use std::future::Future;

use fauna_bridge_atproto::chat::PolledConvo;

use crate::bluesky::db_helpers;
use crate::bridge_legs::{self, BLUESKY, Inbound, InboundDm};
use crate::db::CacheDb;
use crate::routes::AppState;

/// Items one drain pass takes — the family's page ceiling.
const DRAIN_PAGE: u32 = fauna_protocol::bridged_conversations::BRIDGED_PAGE_MAX;

/// Does the leg serve `account` — a consume-side link the poller may run for
/// (D7)?
///
/// # Errors
/// A database fault.
pub async fn serves(db: &CacheDb, account: &[u8; 32]) -> anyhow::Result<bool> {
    Ok(own_did(db, account).await?.is_some())
}

/// The account's linked DID, when the leg serves it.
async fn own_did(db: &CacheDb, account: &[u8; 32]) -> anyhow::Result<Option<String>> {
    let actor_hex = hex::encode(account);
    let conn = db.conn().await;
    if !db_helpers::consume_side_poll_allowed(&conn, &actor_hex)? {
        return Ok(None);
    }
    Ok(db_helpers::get_linked_account(&conn, &actor_hex)?.map(|a| a.bluesky_did))
}

/// A far `sentAt` as unix ms — the sender's claim, carried for display and
/// never ordered on; `0` when it does not parse.
fn sent_at_ms(sent_at: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(sent_at)
        .map(|t| t.timestamp_millis())
        .unwrap_or(0)
}

/// Store one polled conversation's messages through the leg seam. Returns how
/// many rows this call stored.
///
/// Only a one-to-one conversation is a room: one with any other member count
/// is skipped whole (the leg declares no membership affordance, and a room
/// keyed on one peer cannot hold two).
///
/// # Errors
/// A database or seal fault — the caller leaves the conversation's head
/// unrecorded, so the next pass reads it again.
pub async fn ingest_convo(
    state: &AppState,
    account: &[u8; 32],
    own_did: &str,
    convo: &PolledConvo,
) -> anyhow::Result<u64> {
    let mut others = convo.member_dids.iter().filter(|d| d.as_str() != own_did);
    let (Some(peer), None) = (others.next(), others.next()) else {
        tracing::debug!(
            convo = %convo.convo_id,
            members = convo.member_dids.len(),
            "bluesky dm: not a one-to-one conversation — skipped"
        );
        return Ok(0);
    };

    let mut stored = 0u64;
    for dm in &convo.messages {
        // The service-stated sender is this account (a copy of something it
        // sent from another app) or the peer; anything else is no member of a
        // one-to-one conversation and is not stored.
        if dm.sender_did != own_did && dm.sender_did != *peer {
            tracing::warn!(
                convo = %convo.convo_id,
                "bluesky dm: message from a non-member — skipped"
            );
            continue;
        }
        let inbound = InboundDm {
            peer,
            sender: &dm.sender_did,
            self_address: own_did,
            far_message_id: &dm.id,
            plaintext: dm.text.as_bytes(),
            created_at_ms: sent_at_ms(&dm.sent_at),
        };
        match bridge_legs::deposit_gated(&state.db, &BLUESKY, account, &inbound).await? {
            Inbound::Stored => stored += 1,
            Inbound::Duplicate => {}
            Inbound::Blocked => tracing::debug!(
                id = %dm.id,
                "bluesky dm: peer blocked by guardian — DM not stored"
            ),
            Inbound::NoSealKey => tracing::warn!(
                id = %dm.id,
                "bluesky dm: recipient has no seal key on file — DM not stored (fail closed)"
            ),
            Inbound::Full => tracing::warn!(
                id = %dm.id,
                "bluesky dm: recipient's DM plane is at capacity — DM not stored"
            ),
        }
    }
    if stored > 0 {
        bridge_legs::notify_changed(state, &BLUESKY, account, peer);
    }
    Ok(stored)
}

/// Why a far send did not happen.
#[derive(Debug)]
pub enum SendFailed {
    /// Worth another pass: the session did not restore, the service did not
    /// answer. The item stays queued.
    Retry(anyhow::Error),
}

/// The far side of the outbound drain — the one network step, behind a seam
/// so the drain's decisions are pinned without a chat service.
pub trait ChatFar: Sync {
    /// Send `text` to `peer_did` as `account`; the sent message's far id.
    fn send(
        &self,
        account: &[u8; 32],
        peer_did: &str,
        text: &str,
    ) -> impl Future<Output = Result<String, SendFailed>> + Send;
}

/// [`ChatFar`] over the account's restored OAuth session.
pub struct SessionChat<'a> {
    pub state: &'a AppState,
}

impl ChatFar for SessionChat<'_> {
    async fn send(
        &self,
        account: &[u8; 32],
        peer_did: &str,
        text: &str,
    ) -> Result<String, SendFailed> {
        let actor_hex = hex::encode(account);
        let agent = db_helpers::get_agent_for_actor(self.state, &actor_hex)
            .await
            .map_err(|_| {
                SendFailed::Retry(anyhow::anyhow!("no bluesky agent for actor {actor_hex}"))
            })?;
        fauna_bridge_atproto::chat::send_dm(&agent, peer_did, text)
            .await
            .map_err(SendFailed::Retry)
    }
}

/// Drain the leg's outbox: every queued item is opened, sent through `far`,
/// stamped with its far message id and acked. An item that can never be
/// delivered (it does not open, or the leg no longer serves the account — D7
/// is re-checked here, where the consume-side path starts) is acked with a
/// warning and its Sent row stays; a send worth retrying leaves that item and
/// the rest queued for the next pass. Returns each sent item's id with its
/// far message id.
///
/// # Errors
/// A database fault.
pub async fn drain_outbox(
    state: &AppState,
    far: &impl ChatFar,
) -> anyhow::Result<Vec<(i64, String)>> {
    // One pass at a time: the send's nudge and the worker's tick would both
    // fetch the same undrained item and send it twice.
    static DRAINING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one_pass = DRAINING.lock().await;
    let items = state
        .db
        .fetch_principal_outbox(BLUESKY.principal_id, DRAIN_PAGE)
        .await?;
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let (secret, _) = state.db.first_party_bridge_key(BLUESKY.bridge_id).await?;
    let mut sent = Vec::new();
    for (actor, item) in items {
        let Ok(account) = <[u8; 32]>::try_from(actor.as_slice()) else {
            continue;
        };
        let text = if own_did(&state.db, &account).await?.is_none() {
            tracing::warn!(
                id = item.id,
                "bluesky leg: outbound item dropped: the account has no consume-side link"
            );
            None
        } else {
            let text = bridge_legs::open_outbox_text(&item.ciphertext, &secret);
            if text.is_none() {
                tracing::warn!(
                    id = item.id,
                    "bluesky leg: outbound item dropped: it does not open under the leg's key"
                );
            }
            text
        };
        if let Some(text) = text {
            match far.send(&account, &item.far_room_id, &text).await {
                Ok(far_id) => {
                    state
                        .db
                        .stamp_bridged_sent_far_id(&account, BLUESKY.principal_id, item.id, &far_id)
                        .await?;
                    sent.push((item.id, far_id));
                }
                Err(SendFailed::Retry(e)) => {
                    tracing::warn!(id = item.id, "bluesky leg: send deferred: {e:#}");
                    break;
                }
            }
        }
        state
            .db
            .ack_bridged_outbox(&account, BLUESKY.principal_id, &[item.id])
            .await?;
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use fauna_bridge_atproto::types::BlueskyDm;

    use crate::bridged_conversation_handlers::test_support as family;

    const ALICE: [u8; 32] = [0xA1; 32];
    const OWN: &str = "did:plc:alice";
    const PEER: &str = "did:plc:bob";
    const SEED: [u8; 32] = [0x42; 32];

    /// A nest with ALICE consume-side linked as [`OWN`] and a seal key on file.
    async fn linked_state() -> Arc<AppState> {
        linked(None).await
    }

    /// [`linked_state`], with ALICE a ward whose guardian blocked `blocked`
    /// when one is named.
    async fn linked(blocked: Option<&str>) -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        crate::bridge_legs::test_support::init_leg_tables(&db).await;
        match blocked {
            Some(peer) => {
                crate::bridge_legs::test_support::supervised_with_block(
                    &db,
                    &[0x61; 32],
                    &ALICE,
                    "bluesky",
                    peer,
                )
                .await;
            }
            None => db.create_user(&ALICE, "free", "test").await.unwrap(),
        }
        {
            let conn = db.conn().await;
            db_helpers::upsert_linked_account(&conn, &hex::encode(ALICE), OWN, "alice.example")
                .unwrap();
        }
        crate::test_support::seed_recipient_seal_key(&db, &ALICE, &SEED).await;
        let state = AppState::for_test(db);
        // The leg's key is wrapped under the deployment seed.
        crate::test_support::seat_own_deployment_seed(&state).await;
        Arc::new(state)
    }

    fn dm(id: &str, sender: &str, text: &str) -> BlueskyDm {
        BlueskyDm {
            id: id.into(),
            convo_id: "convo-1".into(),
            sender_did: sender.into(),
            sender_handle: "h.example".into(),
            sender_display_name: None,
            text: text.into(),
            sent_at: "2026-10-03T10:00:00.000Z".into(),
        }
    }

    fn convo(members: &[&str], messages: Vec<BlueskyDm>) -> PolledConvo {
        PolledConvo {
            convo_id: "convo-1".into(),
            member_dids: members.iter().map(|m| m.to_string()).collect(),
            head_id: messages.last().map(|m| m.id.clone()).unwrap_or_default(),
            messages,
        }
    }

    fn opened(sealed: &[u8]) -> Vec<u8> {
        crate::test_support::open_recipient_record(sealed, &SEED)
    }

    /// The row's first failing test: a polled DM reaches
    /// `conversation.inbox.fetch` — as a room on the `bluesky` bridge keyed on
    /// the peer's DID, its row sealed to the recipient key, idempotent on the
    /// `chat.bsky` message id.
    #[tokio::test]
    async fn a_polled_dm_reaches_the_family_inbox() {
        let state = linked_state().await;
        let polled = convo(&[OWN, PEER], vec![dm("m1", PEER, "hello alice")]);

        assert_eq!(ingest_convo(&state, &ALICE, OWN, &polled).await.unwrap(), 1);
        // The same head page read again stores nothing.
        assert_eq!(ingest_convo(&state, &ALICE, OWN, &polled).await.unwrap(), 0);

        let rooms = family::rooms(&state, ALICE).await;
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].bridge_id, "bluesky");
        assert_eq!(rooms[0].bridge_label, "Bluesky");
        assert_eq!(rooms[0].glyph, "butterfly");
        assert_eq!(rooms[0].far_room_id, PEER);
        assert_eq!(rooms[0].participants, vec![PEER.to_string()]);
        assert!(!rooms[0].disconnected);

        let rows = family::inbox(&state, ALICE, &rooms[0].room_id).await;
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].outbound);
        assert_eq!(rows[0].sender, PEER);
        assert_eq!(opened(&rows[0].sealed_content), b"hello alice");
    }

    /// `family-safety.md` § The bridge-DM gate: a guardian-`block`ed peer's
    /// new DM is refused before storage, keyed on the service-stated DID.
    #[tokio::test]
    async fn a_blocked_peers_dm_is_refused_before_storage() {
        let state = linked(Some(PEER)).await;

        let polled = convo(&[OWN, PEER], vec![dm("m1", PEER, "let me in")]);
        assert_eq!(ingest_convo(&state, &ALICE, OWN, &polled).await.unwrap(), 0);
        assert!(family::rooms(&state, ALICE).await.is_empty());
    }

    /// A copy of something the account sent from another app lands in the
    /// peer's room as the account's own row — the peer is the other member,
    /// never the sender.
    #[tokio::test]
    async fn the_accounts_own_message_lands_in_the_peers_room() {
        let state = linked_state().await;
        let polled = convo(&[OWN, PEER], vec![dm("m1", OWN, "sent from the app")]);
        assert_eq!(ingest_convo(&state, &ALICE, OWN, &polled).await.unwrap(), 1);

        let rooms = family::rooms(&state, ALICE).await;
        assert_eq!(rooms[0].far_room_id, PEER);
        let rows = family::inbox(&state, ALICE, &rooms[0].room_id).await;
        assert_eq!(rows[0].sender, OWN);
    }

    /// A conversation that is not one-to-one is no room of this leg.
    #[tokio::test]
    async fn a_group_conversation_is_skipped() {
        let state = linked_state().await;
        let polled = convo(
            &[OWN, PEER, "did:plc:carol"],
            vec![dm("m1", PEER, "hi both")],
        );
        assert_eq!(ingest_convo(&state, &ALICE, OWN, &polled).await.unwrap(), 0);
        assert!(family::rooms(&state, ALICE).await.is_empty());
    }

    /// D7 at the leg: a nest-hosted-backed account is not served, even with a
    /// link row — no room can be opened, nothing is polled or sent for it.
    #[tokio::test]
    async fn a_hosted_backed_account_is_not_served() {
        let state = linked_state().await;
        assert!(serves(&state.db, &ALICE).await.unwrap());
        {
            let conn = state.db.conn().await;
            conn.execute(
                "INSERT INTO atproto_identities (actor_id, method, status, did, created_at, updated_at)
                 VALUES (?1, 'plc', 'active', 'did:plc:hosted', 0, 0)",
                rusqlite::params![&ALICE[..]],
            )
            .unwrap();
        }
        assert!(!serves(&state.db, &ALICE).await.unwrap());
    }

    /// A fake chat service: records every send, answers with a far id.
    #[derive(Default)]
    struct FakeChat {
        sent: Mutex<Vec<([u8; 32], String, String)>>,
        down: bool,
    }

    impl ChatFar for FakeChat {
        async fn send(
            &self,
            account: &[u8; 32],
            peer_did: &str,
            text: &str,
        ) -> Result<String, SendFailed> {
            if self.down {
                return Err(SendFailed::Retry(anyhow::anyhow!("chat service down")));
            }
            let mut sent = self.sent.lock().unwrap();
            sent.push((*account, peer_did.to_string(), text.to_string()));
            Ok(format!("far-{}", sent.len()))
        }
    }

    /// The outbound drain: the far call is `send(account, peer DID, text)`,
    /// the item is acked, and the Sent row carries the far id — so the poll
    /// that reads the sent message back stores no second row.
    #[tokio::test]
    async fn the_drain_sends_under_the_session_and_the_readback_is_no_second_row() {
        let state = linked_state().await;
        let room = family::open(&state, ALICE, "bluesky", PEER).await.unwrap();
        assert_eq!(room.far_room_id, PEER);
        let id = family::send(&state, ALICE, &room, "hi bob").await.unwrap();

        // A service that is down leaves the item queued.
        let down = FakeChat {
            down: true,
            ..Default::default()
        };
        assert!(drain_outbox(&state, &down).await.unwrap().is_empty());

        let far = FakeChat::default();
        let sent = drain_outbox(&state, &far).await.unwrap();
        assert_eq!(sent, vec![(id, "far-1".to_string())]);
        assert_eq!(
            *far.sent.lock().unwrap(),
            vec![(ALICE, PEER.to_string(), "hi bob".to_string())]
        );
        // Acked: a second pass sends nothing.
        assert!(drain_outbox(&state, &far).await.unwrap().is_empty());

        // The poll reads the sent message back under its far id.
        let polled = convo(&[OWN, PEER], vec![dm("far-1", OWN, "hi bob")]);
        assert_eq!(ingest_convo(&state, &ALICE, OWN, &polled).await.unwrap(), 0);
        let rows = family::inbox(&state, ALICE, &room.room_id).await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].outbound);
    }
}
