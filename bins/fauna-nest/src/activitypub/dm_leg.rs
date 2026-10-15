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
///
/// The one widening is the AP loopback test hook's
/// (`outbound::test_loopback_hook_open` — a `test-hooks` build with
/// `FAUNA_TEST_AP_ALLOW_LOOPBACK` set, never a shipped one): an `http` actor
/// on a loopback host, the in-test fediverse server's spelling, so a journey
/// can deliver a direct Note the dial-side hook already lets it serve.
#[must_use]
pub fn canonical_actor(uri: &str) -> Option<String> {
    canonical_actor_with(uri, super::outbound::test_loopback_hook_open())
}

fn canonical_actor_with(uri: &str, loopback_http: bool) -> Option<String> {
    let mut url = url::Url::parse(uri.trim()).ok()?;
    let scheme_ok = url.scheme() == "https"
        || (loopback_http
            && url.scheme() == "http"
            && super::outbound::is_loopback_url(uri.trim()));
    if !scheme_ok || url.host_str().is_none() {
        return None;
    }
    url.set_fragment(None);
    Some(url.to_string())
}

/// The largest WebFinger answer [`resolve_account`] reads — real ones are a
/// few hundred bytes; the dial is to a host the user typed.
const WEBFINGER_MAX_BYTES: usize = 65_536;

/// The far room id of the leg's room with `address`: its canonical actor URI,
/// whether `address` is that URI or the `@user@host` it is known by (resolved
/// through `far`). `None` for an address that names no actor.
pub async fn far_room_id(far: &impl bridge_legs::FarResolve, address: &str) -> Option<String> {
    match fediverse_handle_with(address, super::outbound::test_loopback_hook_open()) {
        Some((user, host)) => canonical_actor(&far.fediverse_account(&user, &host).await?),
        None => canonical_actor(address),
    }
}

/// The `(user, host)` an `@user@host` names — the shared parse, which admits a
/// dotted DNS host only. The one widening is the AP loopback test hook's, as
/// for [`canonical_actor`]: a loopback `host:port`, the in-test fediverse
/// server's authority, so a journey can open a room by the account it serves.
fn fediverse_handle_with(address: &str, loopback: bool) -> Option<(String, String)> {
    use fauna_bridge_activitypub::webfinger::parse_fediverse_handle;
    parse_fediverse_handle(address).or_else(|| {
        let (user, host) = address.trim().strip_prefix('@')?.split_once('@')?;
        let ok = loopback
            && !user.is_empty()
            && !user
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '@' | '/' | ':' | '?' | '#' | '%'))
            && !host.contains(['/', '@', '?', '#'])
            && super::outbound::is_loopback_url(&format!("http://{host}/"));
        ok.then(|| (user.to_string(), host.to_ascii_lowercase()))
    })
}

/// The WebFinger query asking `host` which actor `acct:user@host` is: the
/// shared `https` query, or — under the loopback test hook only, for a
/// loopback `host` — the same query over `http`, the in-test server's scheme.
fn webfinger_query_url(user: &str, host: &str, loopback: bool) -> String {
    let url = fauna_bridge_activitypub::webfinger::acct_query_url(user, host);
    if loopback
        && super::outbound::is_loopback_url(&format!("http://{host}/"))
        && let Some(rest) = url.strip_prefix("https://")
    {
        return format!("http://{rest}");
    }
    url
}

/// The canonical actor URI the Fediverse account `user@host` names — asked of
/// `host` by WebFinger through the SSRF-guarded dialer, then the actor
/// document it names fetched and cached (`ap_remote_actors`), so an answer
/// that names no actor is no answer. `None` when either step fails.
pub async fn resolve_account(state: &AppState, user: &str, host: &str) -> Option<String> {
    match webfinger_actor(state, user, host).await {
        Ok(uri) => Some(uri),
        Err(e) => {
            tracing::debug!(%host, "activitypub leg: account did not resolve: {e:#}");
            None
        }
    }
}

async fn webfinger_actor(state: &AppState, user: &str, host: &str) -> anyhow::Result<String> {
    use fauna_bridge_activitypub::webfinger::self_actor_href;

    let query = webfinger_query_url(user, host, super::outbound::test_loopback_hook_open());
    let (client, url) = super::outbound::ap_outbound_client(&query).await?;
    let resp = client
        .get(url)
        .header("Accept", "application/jrd+json, application/json")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("WebFinger returned HTTP {}", resp.status());
    }
    let body = crate::ssrf::read_capped(resp, WEBFINGER_MAX_BYTES).await?;
    let jrd: fauna_bridge_activitypub::types::WebFingerResponse = serde_json::from_slice(&body)?;
    let uri = self_actor_href(&jrd)
        .and_then(canonical_actor)
        .ok_or_else(|| anyhow::anyhow!("the WebFinger answer names no https actor"))?;

    let cached = {
        let conn = state.db.conn().await;
        db_helpers::get_remote_actor(&conn, &uri)?
    };
    if cached.is_none() {
        let actor = crate::activitypub::inbox_routes::fetch_remote_actor(state, &uri).await?;
        let conn = state.db.conn().await;
        db_helpers::upsert_remote_actor(&conn, &actor)?;
    }
    Ok(uri)
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
    use crate::bridge_legs::test_support::FakeResolve;
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

    /// `conversations.md` § *The `Bridged` adapter*, ruling 2 (d): a typed
    /// `@user@host` opens the room keyed on the canonical actor URI WebFinger
    /// names; the actor URI itself opens the same room.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_fediverse_handle_opens_the_room_keyed_on_its_actor_uri() {
        let state = state(None).await;
        let far = FakeResolve::knowing(&[("mallory@remote.example", MALLORY)]);

        let room = family::open_resolving(&state, ALICE, "@mallory@Remote.Example", &far)
            .await
            .unwrap();
        assert_eq!(room.bridge_id, "activitypub");
        assert_eq!(room.far_room_id, MALLORY);
        assert_eq!(room.participants, vec![MALLORY.to_string()]);

        let by_uri = family::open(&state, ALICE, "activitypub", MALLORY)
            .await
            .unwrap();
        assert_eq!(by_uri.room_id, room.room_id);
        assert_eq!(family::rooms(&state, ALICE).await.len(), 1);
    }

    /// The family's typed refusal, no room opened: a bare `user@host` (an
    /// email address to every app's probe — the grammar admits only the
    /// two-`@` form), an account WebFinger does not know, and an answer that is
    /// no `https` actor URI.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_address_that_names_no_actor_is_address_refused() {
        let state = state(None).await;
        let far = FakeResolve::knowing(&[
            ("mallory@remote.example", MALLORY),
            ("odd@remote.example", "http://remote.example/users/odd"),
        ]);
        for address in [
            "mallory@remote.example",
            "@nobody@remote.example",
            "@odd@remote.example",
        ] {
            let err = family::open_resolving(&state, ALICE, address, &far)
                .await
                .unwrap_err();
            assert_eq!(err.code, "fauna.bridges.address_refused", "{address}");
        }
        assert!(family::rooms(&state, ALICE).await.is_empty());
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

    /// The canonical spelling is `https` only — save the loopback test hook's
    /// one widening, an `http` actor on a loopback host, and only while the
    /// hook is open; a non-loopback `http` actor stays refused either way.
    #[test]
    fn canonical_actor_is_https_save_the_open_loopback_hook() {
        for open in [false, true] {
            assert_eq!(
                canonical_actor_with("https://Remote.Example:443/users/bob#main-key", open)
                    .as_deref(),
                Some("https://remote.example/users/bob"),
            );
            assert_eq!(
                canonical_actor_with("http://remote.example/users/bob", open),
                None
            );
            assert_eq!(
                canonical_actor_with("mailto:bob@remote.example", open),
                None
            );
        }
        let fake = "http://127.0.0.1:4567/users/bob";
        assert_eq!(canonical_actor_with(fake, false), None);
        assert_eq!(canonical_actor_with(fake, true).as_deref(), Some(fake));
        assert_eq!(
            canonical_actor_with("http://localhost:4567/users/bob", true).as_deref(),
            Some("http://localhost:4567/users/bob"),
        );
        assert_eq!(
            canonical_actor_with("http://10.0.0.1/users/bob", true),
            None
        );
    }

    /// An `@user@host` names a dotted DNS host, asked over `https` — save the
    /// loopback test hook's widening, a loopback `host:port` asked over
    /// `http`, and only while the hook is open; a private-range or ported DNS
    /// host stays refused either way.
    #[test]
    fn fediverse_handle_is_dns_and_https_save_the_open_loopback_hook() {
        for open in [false, true] {
            assert_eq!(
                fediverse_handle_with("@Bob@Remote.Example", open),
                Some(("Bob".into(), "remote.example".into())),
            );
            for not_one in ["@bob@10.0.0.1:80", "@bob@remote.example:8443", "@bob"] {
                assert_eq!(fediverse_handle_with(not_one, open), None, "{not_one}");
            }
            assert_eq!(
                webfinger_query_url("bob", "remote.example", open),
                "https://remote.example/.well-known/webfinger?resource=acct%3Abob%40remote.example",
            );
        }
        let fake = "@frank@127.0.0.1:4567";
        assert_eq!(fediverse_handle_with(fake, false), None);
        assert_eq!(
            fediverse_handle_with(fake, true),
            Some(("frank".into(), "127.0.0.1:4567".into())),
        );
        assert_eq!(
            fediverse_handle_with("@frank@localhost:4567", true),
            Some(("frank".into(), "localhost:4567".into())),
        );
        assert!(webfinger_query_url("frank", "127.0.0.1:4567", false).starts_with("https://"));
        assert_eq!(
            webfinger_query_url("frank", "127.0.0.1:4567", true),
            "http://127.0.0.1:4567/.well-known/webfinger?resource=acct%3Afrank%40127.0.0.1%3A4567",
        );
    }
}
