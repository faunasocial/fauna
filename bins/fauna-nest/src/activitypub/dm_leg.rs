//! The in-process ActivityPub DM leg of the bridged-conversation family
//! (`docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
//! adapter*, rulings 2 (e) and 3) — a Fediverse direct message: an AS2 `Note`
//! addressed to actors and to no collection (`docs/goal/behavior/activitypub.md`
//! § Architecture → *The inbound audience gate*). The seam it shares with the
//! other first-party legs is [`crate::bridge_legs`].
//!
//! **Identity.** `{ activitypub, Fediverse, globe }`, principal id
//! `ACTIVITYPUB_LEG_PRINCIPAL_ID`. The leg serves an account exactly when it
//! has an enabled ActivityPub actor ([`serves`]).
//!
//! **Inbound** ([`ingest_non_public_note`]) is where the inbox's `Create`
//! sends every Note the audience gate keeps out of the public projection. A
//! Note that is *direct* — no Public, no followers collection — and names an
//! enabled local actor in its `to` becomes a row in that account's room with
//! the sender; every other non-public Note (followers-only, addressed to
//! nobody here) is dropped exactly as before.
//!
//! **The peer identity the gate keys on** is the activity's top-level `actor`
//! — the actor whose fetched key verified the delivery's HTTP signature — and
//! never the Note's self-claimed `attributedTo`, which must *equal* it or the
//! Note is dropped (`docs/goal/behavior/family-safety.md` § The bridge-DM gate
//! → *The peer identity the verdict keys on*). The room and the verdict key on
//! its canonical spelling ([`canonical_actor`]).
//!
//! **Outbound** ([`drain_outbox`]) opens each queued item under the leg's key
//! — the family's one honest exception to the blind outbox — builds a
//! `Create{Note}` addressed to the peer alone, and queues it on the signed
//! delivery queue every other outbound activity rides.

use std::sync::Arc;

use fauna_bridge_activitypub::translate::{
    build_create_activity, build_direct_note, direct_note_text, is_directly_addressed,
};
use fauna_bridge_activitypub::types::ApNote;
use serde_json::Value;

use crate::activitypub::db_helpers;
use crate::activitypub::inbox_routes::{
    local_actor_username, same_actor_identity, same_origin, string_array,
};
use crate::bridge_legs::{self, ACTIVITYPUB, Inbound, InboundDm};
use crate::db::CacheDb;
use crate::routes::AppState;

/// Items one drain pass takes — the family's page ceiling.
const DRAIN_PAGE: u32 = fauna_protocol::bridged_conversations::BRIDGED_PAGE_MAX;

/// Does the leg serve `account` — an enabled ActivityPub actor?
///
/// # Errors
/// A database fault.
pub async fn serves(db: &CacheDb, account: &[u8; 32]) -> anyhow::Result<bool> {
    Ok(enabled_account(db, account).await?.is_some())
}

async fn enabled_account(
    db: &CacheDb,
    account: &[u8; 32],
) -> anyhow::Result<Option<db_helpers::ApAccount>> {
    let conn = db.conn().await;
    Ok(db_helpers::get_account(&conn, &hex::encode(account))?.filter(|a| a.enabled))
}

/// The one spelling of an actor URI the leg's rooms and the gate's verdict
/// rows key on: an `https` URL as the URL parser normalises it (lowercase
/// host, default port elided), its fragment dropped. `None` for anything else.
#[must_use]
pub fn canonical_actor(uri: &str) -> Option<String> {
    let mut url = url::Url::parse(uri.trim()).ok()?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return None;
    }
    url.set_fragment(None);
    Some(url.to_string())
}

/// A far `published` as unix ms — the sender's claim, carried for display and
/// never ordered on; `0` when it is absent or does not parse.
fn published_ms(published: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(published)
        .map(|t| t.timestamp_millis())
        .unwrap_or(0)
}

/// Deliver one non-public `Create{Note}` to the local actors it directly
/// addresses, or drop it. Returns how many rows this call stored. The caller
/// answers `202 Accepted` either way — no retry, and no oracle distinguishing
/// "no such user" from "not delivered".
///
/// # Errors
/// A database or seal fault.
pub async fn ingest_non_public_note(
    state: &Arc<AppState>,
    activity: &Value,
    note: &ApNote,
) -> anyhow::Result<u64> {
    let mut to = note.to.clone();
    to.extend(string_array(activity.get("to")));
    let mut cc = note.cc.clone();
    cc.extend(string_array(activity.get("cc")));
    if !is_directly_addressed(&to, &cc) {
        tracing::info!(
            ap_url = %note.id,
            "ap Create: dropping non-public Note (followers-only); no projection"
        );
        return Ok(0);
    }

    // The peer is the signer. A `Create` claims authorship, so the Note must
    // be the signer's own and live on the signer's host — the ownership rule
    // the public path enforces (`activitypub.md` § Security posture → Object
    // ownership), without which the Note's `attributedTo` would choose whose
    // message this becomes.
    let signed_actor = activity.get("actor").and_then(Value::as_str).unwrap_or("");
    let Some(peer) = canonical_actor(signed_actor) else {
        tracing::warn!(ap_url = %note.id, "ap Create: direct Note with no usable signer");
        return Ok(0);
    };
    if !same_actor_identity(signed_actor, &note.attributed_to)
        || !same_origin(signed_actor, &note.id)
    {
        tracing::warn!(
            signed_actor,
            attributed_to = %note.attributed_to,
            ap_url = %note.id,
            "ap Create: dropping direct Note not authored by its signer (spoof attempt)"
        );
        return Ok(0);
    }

    // Every enabled local actor the Note names in `to`. `cc` alone is not
    // direct address: it is how a followers-only post carries its mentions.
    let domain = state.handle_domain();
    let mut recipients: Vec<db_helpers::ApAccount> = Vec::new();
    for uri in &to {
        let Some(username) = local_actor_username(&domain, uri) else {
            continue;
        };
        let account = {
            let conn = state.db.conn().await;
            db_helpers::get_account_by_username(&conn, &username)?
        };
        if let Some(account) = account.filter(|a| a.enabled)
            && !recipients.iter().any(|r| r.actor_id == account.actor_id)
        {
            recipients.push(account);
        }
    }
    if recipients.is_empty() {
        tracing::info!(
            ap_url = %note.id,
            "ap Create: dropping direct Note addressed to no enabled local account"
        );
        return Ok(0);
    }

    let text = direct_note_text(note);
    let mut stored = 0u64;
    for recipient in recipients {
        let Ok(account) = fauna_core::hex32::decode(&recipient.actor_id) else {
            continue;
        };
        let inbound = InboundDm {
            peer: &peer,
            sender: &peer,
            self_address: &recipient.actor_url,
            far_message_id: &note.id,
            plaintext: text.as_bytes(),
            created_at_ms: published_ms(&note.published),
        };
        match bridge_legs::deposit_gated(&state.db, &ACTIVITYPUB, &account, &inbound).await? {
            Inbound::Stored => {
                stored += 1;
                bridge_legs::notify_changed(state, &ACTIVITYPUB, &account, &peer);
            }
            Inbound::Duplicate => {}
            Inbound::Blocked => tracing::debug!(
                ap_url = %note.id,
                "ap Create: peer blocked by guardian — direct Note not stored"
            ),
            Inbound::NoSealKey => tracing::warn!(
                ap_url = %note.id,
                "ap Create: recipient has no seal key on file — direct Note not stored (fail closed)"
            ),
            Inbound::Full => tracing::warn!(
                ap_url = %note.id,
                "ap Create: recipient's DM plane is at capacity — direct Note not stored"
            ),
        }
    }
    Ok(stored)
}

/// Drain the leg's outbox: every queued item is opened, built into a
/// `Create{Note}` addressed to the room's peer alone, queued for signed
/// delivery to the peer's inbox, stamped with the Note's id and acked. An item
/// that can never be delivered (it does not open, the account has no enabled
/// actor) is acked with a warning and its Sent row stays. Returns each queued
/// item's id with its Note id.
///
/// # Errors
/// A database fault.
pub async fn drain_outbox(state: &AppState) -> anyhow::Result<Vec<(i64, String)>> {
    // One pass at a time: the send's nudge and a concurrent pass would both
    // fetch the same undrained item and queue it twice.
    static DRAINING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one_pass = DRAINING.lock().await;
    let items = state
        .db
        .fetch_principal_outbox(ACTIVITYPUB.principal_id, DRAIN_PAGE)
        .await?;
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let (secret, _) = state
        .db
        .first_party_bridge_key(ACTIVITYPUB.bridge_id)
        .await?;
    let mut queued = Vec::new();
    for (actor, item) in items {
        let Ok(account) = <[u8; 32]>::try_from(actor.as_slice()) else {
            continue;
        };
        let local = enabled_account(&state.db, &account).await?;
        let text = bridge_legs::open_outbox_text(&item.ciphertext, &secret);
        match (local, text) {
            (Some(local), Some(text)) => {
                // The id is the item's own, so a pass that crashed between the
                // queue and the ack re-queues the same Note, which the far
                // server dedupes on its id.
                let note_id = format!("{}/dm/{}", local.actor_url, item.id);
                let note = build_direct_note(
                    &local.actor_url,
                    &note_id,
                    &item.far_room_id,
                    &text,
                    fauna_core::data::Timestamp::now(),
                );
                let create =
                    build_create_activity(&local.actor_url, &format!("{note_id}/activity"), &note);
                let activity_json = serde_json::to_string(&create)?;
                {
                    let conn = state.db.conn().await;
                    let inbox = db_helpers::resolve_delivery_inbox(
                        &conn,
                        Some(&item.far_room_id),
                        &item.far_room_id,
                    );
                    db_helpers::enqueue_delivery(&conn, &activity_json, &inbox)?;
                }
                state
                    .db
                    .stamp_bridged_sent_far_id(
                        &account,
                        ACTIVITYPUB.principal_id,
                        item.id,
                        &note_id,
                    )
                    .await?;
                queued.push((item.id, note_id));
            }
            (None, _) => tracing::warn!(
                id = item.id,
                "activitypub leg: outbound item dropped: the account has no enabled actor"
            ),
            (_, None) => tracing::warn!(
                id = item.id,
                "activitypub leg: outbound item dropped: it does not open under the leg's key"
            ),
        }
        state
            .db
            .ack_bridged_outbox(&account, ACTIVITYPUB.principal_id, &[item.id])
            .await?;
    }
    if !queued.is_empty() {
        state.activitypub.delivery_nudge.notify_one();
    }
    Ok(queued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use serde_json::json;

    use crate::activitypub::inbox_routes::handle_create;
    use crate::bridged_conversation_handlers::test_support as family;

    const ALICE: [u8; 32] = [0xA1; 32];
    const ALICE_URL: &str = "https://local.example/ap/users/alice";
    const MALLORY: &str = "https://remote.example/users/mallory";
    const PUBLIC: &str = "https://www.w3.org/ns/activitystreams#Public";
    const SEED: [u8; 32] = [0x42; 32];

    /// A claimed nest (`local.example`) where ALICE has an enabled actor and a
    /// seal key on file; a ward whose guardian blocked `blocked` when named.
    async fn state(blocked: Option<&str>) -> Arc<AppState> {
        let state = crate::activitypub::ap_state_for_test().await;
        state
            .identity_domain
            .store(Some(Arc::new("local.example".to_string())));
        crate::bridge_legs::test_support::init_leg_tables(&state.db).await;
        match blocked {
            Some(peer) => {
                crate::bridge_legs::test_support::supervised_with_block(
                    &state.db,
                    &[0x61; 32],
                    &ALICE,
                    "activitypub",
                    peer,
                )
                .await;
            }
            None => state.db.create_user(&ALICE, "free", "test").await.unwrap(),
        }
        {
            let conn = state.db.conn().await;
            db_helpers::create_account(&conn, &hex::encode(ALICE), "alice", ALICE_URL, &[], "pem")
                .unwrap();
        }
        crate::test_support::seed_recipient_seal_key(&state.db, &ALICE, &SEED).await;
        Arc::new(state)
    }

    fn create(ap_url: &str, content: &str, to: Value, cc: Value) -> Value {
        json!({
            "type": "Create",
            "id": format!("{ap_url}/activity"),
            "actor": MALLORY,
            "to": to,
            "cc": cc,
            "object": {
                "type": "Note",
                "id": ap_url,
                "attributedTo": MALLORY,
                "content": content,
                "published": "2026-07-14T00:00:00Z",
                "to": to,
                "cc": cc,
            }
        })
    }

    fn opened(sealed: &[u8]) -> Vec<u8> {
        crate::test_support::open_recipient_record(sealed, &SEED)
    }

    async fn mapped(state: &Arc<AppState>, ap_url: &str) -> bool {
        let conn = state.db.conn().await;
        db_helpers::get_post_id_for_ap_url(&conn, ap_url)
            .unwrap()
            .is_some()
    }

    /// The row's failing test: an addressed, non-public Note delivered to the
    /// inbox → the room appears on the `activitypub` bridge, keyed on the
    /// signer, the body sealed as plain text — and never as a post.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_addressed_note_becomes_a_room() {
        let state = state(None).await;
        let ap_url = "https://remote.example/notes/dm1";
        let activity = create(
            ap_url,
            "<p>psst <b>alice</b></p>",
            json!([ALICE_URL]),
            json!([]),
        );

        let code = handle_create(&state, &activity, None).await.unwrap();
        assert_eq!(code, StatusCode::ACCEPTED);
        // Redelivery stores nothing new.
        handle_create(&state, &activity, None).await.unwrap();

        let rooms = family::rooms(&state, ALICE).await;
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].bridge_id, "activitypub");
        assert_eq!(rooms[0].bridge_label, "Fediverse");
        assert_eq!(rooms[0].glyph, "globe");
        assert_eq!(rooms[0].far_room_id, MALLORY);
        assert_eq!(rooms[0].self_address.as_deref(), Some(ALICE_URL));
        let rows = family::inbox(&state, ALICE, &rooms[0].room_id).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sender, MALLORY);
        assert_eq!(opened(&rows[0].sealed_content), b"psst alice");
        assert!(
            !mapped(&state, ap_url).await,
            "a direct Note must never acquire a post/* row"
        );
    }

    /// The other half of the row's test: a public Note addressed to the same
    /// account still goes to the feed, and opens no room.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_public_note_still_goes_to_the_feed() {
        let state = state(None).await;
        let ap_url = "https://remote.example/notes/reply1";
        let activity = create(
            ap_url,
            "a public reply",
            json!([PUBLIC, ALICE_URL]),
            json!([]),
        );

        handle_create(&state, &activity, Some("alice"))
            .await
            .unwrap();
        assert!(mapped(&state, ap_url).await);
        assert!(family::rooms(&state, ALICE).await.is_empty());
    }

    /// A followers-only Note stays dropped — with or without a mention of a
    /// local actor, and through the per-user inbox too.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_followers_only_note_stays_dropped() {
        let state = state(None).await;
        let followers = "https://remote.example/users/mallory/followers";
        for (n, (to, cc)) in [
            (json!([followers]), json!([])),
            (json!([followers]), json!([ALICE_URL])),
            (json!([ALICE_URL]), json!([followers])),
        ]
        .into_iter()
        .enumerate()
        {
            let ap_url = format!("https://remote.example/notes/fo{n}");
            let activity = create(&ap_url, "for my followers", to, cc);
            let code = handle_create(&state, &activity, Some("alice"))
                .await
                .unwrap();
            assert_eq!(code, StatusCode::ACCEPTED);
            assert!(!mapped(&state, &ap_url).await);
        }
        assert!(family::rooms(&state, ALICE).await.is_empty());
    }

    /// A direct Note naming a local actor only in `cc`, or naming nobody here,
    /// is delivered to nobody — the per-user inbox it arrived at is not
    /// address.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_note_not_addressed_to_the_account_is_dropped() {
        let state = state(None).await;
        for (n, (to, cc)) in [
            (
                json!(["https://other.example/users/bob"]),
                json!([ALICE_URL]),
            ),
            (json!(["https://other.example/users/bob"]), json!([])),
        ]
        .into_iter()
        .enumerate()
        {
            let ap_url = format!("https://remote.example/notes/na{n}");
            let activity = create(&ap_url, "not for alice", to, cc);
            handle_create(&state, &activity, Some("alice"))
                .await
                .unwrap();
        }
        assert!(family::rooms(&state, ALICE).await.is_empty());
    }

    /// The gate keys on the signer: a Note whose `attributedTo` names someone
    /// else is dropped rather than keyed on either, and a guardian-`block`ed
    /// signer's Note is refused before storage.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_gate_keys_on_the_signer() {
        let st = state(None).await;
        let mut spoofed = create(
            "https://remote.example/notes/spoof",
            "hi",
            json!([ALICE_URL]),
            json!([]),
        );
        spoofed["object"]["attributedTo"] = json!("https://remote.example/users/bob");
        handle_create(&st, &spoofed, None).await.unwrap();
        assert!(family::rooms(&st, ALICE).await.is_empty());

        let st = state(Some(MALLORY)).await;
        let activity = create(
            "https://remote.example/notes/blocked",
            "let me in",
            json!([ALICE_URL]),
            json!([]),
        );
        handle_create(&st, &activity, None).await.unwrap();
        assert!(family::rooms(&st, ALICE).await.is_empty());
    }

    /// The outbound drain: the far call is a `Create{Note}` from the account's
    /// actor, addressed to the peer alone, queued for the peer's inbox.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_drain_queues_a_note_addressed_to_the_peer_alone() {
        let state = state(None).await;
        let room = family::open(&state, ALICE, "activitypub", MALLORY)
            .await
            .unwrap();
        assert_eq!(room.far_room_id, MALLORY);
        let id = family::send(&state, ALICE, &room, "hi <mallory>\nbye")
            .await
            .unwrap();

        // `family::send` nudged a drain of its own, and passes run one at a
        // time — so whichever pass took the item, it is queued once and acked
        // by the time this one returns.
        drain_outbox(&state).await.unwrap();
        assert!(drain_outbox(&state).await.unwrap().is_empty(), "acked");
        let note_id = format!("{ALICE_URL}/dm/{id}");

        let jobs = {
            let conn = state.db.conn().await;
            db_helpers::get_pending_deliveries(&conn, 10).unwrap()
        };
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].target_inbox, "https://remote.example/inbox");
        let activity: Value = serde_json::from_str(&jobs[0].activity_json).unwrap();
        assert_eq!(activity["type"], "Create");
        assert_eq!(activity["actor"], ALICE_URL);
        assert_eq!(activity["to"], json!([MALLORY]));
        assert!(activity.get("cc").is_none_or(|cc| cc == &json!([])));
        let note = &activity["object"];
        assert_eq!(note["id"], note_id);
        assert_eq!(note["attributedTo"], ALICE_URL);
        assert_eq!(note["to"], json!([MALLORY]));
        assert_eq!(note["content"], "<p>hi &lt;mallory&gt;<br>bye</p>");
        assert_eq!(note["tag"][0]["type"], "Mention");
        assert_eq!(note["tag"][0]["href"], MALLORY);
    }
}
