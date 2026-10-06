//! The consume-side Bluesky DM worker — the start site per-user DM polling
//! never had: each linked account's `chat.bsky` conversations into the
//! bridged-conversation family, and the leg's outbox out
//! (`docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
//! adapter*, ruling 3).
//!
//! Shaped after [`super::feed_worker::BlueskyFeedWorker`] and
//! [`super::notif_worker::BlueskyNotifWorker`] in every respect that matters:
//! constructed in `lib.rs` behind the `bluesky` feature, spawned with
//! `state.spawn_scoped`, one interval tick driving one pass over
//! `list_consume_side_linked_actors`, and split into a network half
//! ([`poll_bluesky_dms`], over `fauna_bridge_atproto::chat`) and an ingest
//! half ([`super::dm_leg::ingest_convo`]) so every decision about what came
//! back is pinned without a far end.
//!
//! **No configuration knob.** The cadence is a Rust constant — nobody chooses
//! how often a bridge polls (`principles.md` § One configuration surface).
//!
//! **No persisted cursor.** The deposit is idempotent on the `chat.bsky`
//! message id, so a head page read twice stores nothing the second time. What
//! the worker keeps is in memory only: the newest message id it last ingested
//! per conversation, so an unchanged conversation costs no `getMessages`. A
//! restart forgets it and re-reads each head page once.
//!
//! **The D7 hosted-backing gate lives at this start site**
//! (`atproto-pds-full.md` § D7, and the binding instruction in its
//! § Implementation status): the enumeration excludes every actor holding an
//! active nest-hosted ATProto identity, and [`poll_bluesky_dms`] re-checks the
//! same predicate before its first network step — the notification poller's
//! two checks, mirrored, for the same reason (`get_agent_for_actor` never sees
//! the backing). The leg's `serves` is that predicate too, so a hosted-backed
//! account has no Bluesky leg to open a room on or send through.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::bluesky::notif_sync::ConsumeSidePollRefused;
use crate::bluesky::{db_helpers, dm_leg};
use crate::routes::AppState;

/// How often each linked account's conversations are polled and the leg's
/// outbox drained.
///
/// Thirty seconds: a direct message is the one bridged event a person waits
/// on, and an unchanged account costs a single `listConvos`. A send does not
/// wait for the tick — the family's `send` nudges the drain itself.
pub(crate) const DM_POLL_INTERVAL_SECS: u64 = 30;

/// The newest message id last ingested, per `(actor, conversation)`.
pub(crate) type Heads = HashMap<(String, String), String>;

/// Polls every consume-side linked account's Bluesky DMs into the bridged
/// family and drains the Bluesky leg's outbox.
pub struct BlueskyDmWorker {
    state: Arc<AppState>,
    heads: Heads,
}

impl BlueskyDmWorker {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            heads: Heads::new(),
        }
    }

    /// Run the worker. This spawns as a long-lived tokio task.
    pub async fn run(mut self) {
        tracing::info!("Bluesky DM worker started");
        let mut interval = tokio::time::interval(Duration::from_secs(DM_POLL_INTERVAL_SECS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            self.poll_once().await;
        }
    }

    /// One pass: every consume-side linked account's conversations in, then
    /// the leg's outbox out. A single account's failure — an expired OAuth
    /// session, a chat-service timeout — is logged and skipped.
    async fn poll_once(&mut self) {
        let actors = {
            let conn = self.state.db.conn().await;
            match db_helpers::list_consume_side_linked_actors(&conn) {
                Ok(actors) => actors,
                Err(e) => {
                    tracing::warn!("bluesky dm: cannot enumerate linked accounts: {e:#}");
                    return;
                }
            }
        };
        // An account that is no longer enumerated keeps no heads.
        self.heads.retain(|(actor, _), _| actors.contains(actor));

        for actor_hex in actors {
            match poll_bluesky_dms(&self.state, &actor_hex, &mut self.heads).await {
                Ok(0) => {}
                Ok(n) => tracing::debug!(actor = %actor_hex, stored = n, "bluesky dm: stored"),
                Err(e) => tracing::warn!(actor = %actor_hex, "bluesky dm: {e:#}"),
            }
        }

        let far = dm_leg::SessionChat { state: &self.state };
        if let Err(e) = dm_leg::drain_outbox(&self.state, &far).await {
            tracing::warn!("bluesky dm: outbox drain: {e:#}");
        }
    }
}

/// Poll one actor's conversations and store what the nest does not yet hold.
/// Returns how many rows this call stored.
pub async fn poll_bluesky_dms(
    state: &Arc<AppState>,
    actor_hex: &str,
    heads: &mut Heads,
) -> anyhow::Result<u64> {
    // D7, re-checked where the consume-side path starts rather than trusted
    // from the caller — the same window the notification poller closes.
    let own_did = {
        let conn = state.db.conn().await;
        if !db_helpers::consume_side_poll_allowed(&conn, actor_hex)? {
            return Err(ConsumeSidePollRefused {
                actor_hex: actor_hex.to_string(),
            }
            .into());
        }
        db_helpers::get_linked_account(&conn, actor_hex)?
            .ok_or_else(|| anyhow::anyhow!("no linked Bluesky account for actor {actor_hex}"))?
            .bluesky_did
    };

    let actor: [u8; 32] = hex::decode(actor_hex)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("actor id is not 32 bytes: {actor_hex}"))?;

    let agent = db_helpers::get_agent_for_actor(state, actor_hex)
        .await
        .map_err(|_| anyhow::anyhow!("no bluesky agent for actor {actor_hex}"))?;

    let key = |convo: &str| (actor_hex.to_string(), convo.to_string());
    let polled = fauna_bridge_atproto::chat::poll_convos(&agent, |convo, head| {
        heads.get(&key(convo)).is_some_and(|seen| seen == head)
    })
    .await?;

    let mut stored = 0u64;
    for convo in &polled {
        stored += dm_leg::ingest_convo(state, &actor, &own_did, convo).await?;
        // Recorded only once the page is ingested: a fault above leaves the
        // head unrecorded, so the next pass reads the page again.
        heads.insert(key(&convo.convo_id), convo.head_id.clone());
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    fn insert_link(conn: &rusqlite::Connection, actor_id: &str) {
        db_helpers::upsert_linked_account(conn, actor_id, "did:plc:ext", "ext.bsky.social")
            .expect("insert consume-side link");
    }

    /// D7 at this poller's poll site, the notification poller's pin mirrored:
    /// a `both` actor is refused before any network step; a consume-side-only
    /// actor passes the gate and fails one step on, at the agent (Bluesky
    /// OAuth is unconfigured in this harness).
    #[tokio::test]
    async fn a_both_actor_is_refused_at_the_poll_site() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        crate::bluesky::init_db(&db).await.unwrap();
        let state = Arc::new(AppState::for_test(db));

        let linked = [1u8; 32];
        let both = [3u8; 32];
        {
            let conn = state.db.conn().await;
            insert_link(&conn, &hex::encode(linked));
            insert_link(&conn, &hex::encode(both));
            conn.execute(
                "INSERT INTO atproto_identities (actor_id, method, status, did, created_at, updated_at)
                 VALUES (?1, 'plc', 'active', 'did:plc:hosted', 0, 0)",
                rusqlite::params![both.to_vec()],
            )
            .expect("insert hosted identity");
        }

        let mut heads = Heads::new();
        let err = poll_bluesky_dms(&state, &hex::encode(both), &mut heads)
            .await
            .expect_err("a nest-hosted-backed actor must never be polled consume-side");
        assert!(
            err.downcast_ref::<ConsumeSidePollRefused>().is_some(),
            "the poll site itself must refuse a `both` actor (D7): {err:#}"
        );

        let err = poll_bluesky_dms(&state, &hex::encode(linked), &mut heads)
            .await
            .expect_err("no Bluesky OAuth client is configured in this harness");
        assert!(
            err.downcast_ref::<ConsumeSidePollRefused>().is_none(),
            "a consume-side-only actor must pass the poll-site gate: {err:#}"
        );
    }
}
