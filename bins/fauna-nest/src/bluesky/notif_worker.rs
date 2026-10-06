//! The Bluesky notification sync worker — the caller
//! [`super::notif_sync::poll_bluesky_notifications`] was written for and never
//! had.
//!
//! `bridges.md` § Bluesky bridge → *Notifications* specifies Bluesky likes,
//! replies, reposts, quotes, follows and mentions arriving in the unified
//! notification list. The translator has existed since the bridge landed; what
//! was missing is a worker that runs it for each linked account, which is what
//! this module is. Shaped after the sibling sync workers
//! (`nostr::sync_worker::NostrSyncWorker`, `activitypub::sync_worker::
//! ApSyncWorker`): constructed in `lib.rs` behind its cargo feature, spawned
//! with `state.spawn_scoped`, one interval tick driving one pass.
//!
//! **No configuration knob.** The cadence is a Rust constant. Nobody chooses
//! how often a bridge polls — it is not a user or admin preference, so by the
//! one-configuration-surface rule it is bucket (1), a constant, never a file,
//! env var or flag.
//!
//! **The D7 hosted-backing gate lives at this start site**
//! (`atproto-pds-full.md` § D7). The enumeration is
//! [`db_helpers::list_consume_side_linked_actors`], which excludes any actor
//! holding an active nest-hosted ATProto identity: such an account reads its
//! Bluesky activity through service-auth proxying, and the consume-side OAuth
//! path must never run for it — not even to fail, which would log an error
//! every tick for an account working exactly as designed.
//! [`notif_sync::poll_bluesky_notifications`] re-checks the same predicate
//! before it starts, for a caller that did not come through the enumeration
//! and for a hosted identity that goes active mid-pass. **Those two checks are
//! the whole D7 boundary: nothing further in fails closed.**
//! `get_agent_for_actor` restores the OAuth session by the linked DID alone and
//! never sees the actor's backing, so a hosted-backed actor that got past both
//! would be polled.

use std::sync::Arc;
use std::time::Duration;

use crate::bluesky::{db_helpers, notif_sync};
use crate::routes::AppState;

/// How often each linked account's Bluesky notifications are polled.
///
/// Two minutes: `listNotifications` is a cheap indexed read, but it is one
/// request per linked account per tick against a shared AppView, and a bridged
/// like is not a latency-critical event — the push that delivers it is.
pub(crate) const NOTIF_POLL_INTERVAL_SECS: u64 = 120;

/// Polls every consume-side linked account's Bluesky notifications into the
/// unified notification list.
pub struct BlueskyNotifWorker {
    state: Arc<AppState>,
}

impl BlueskyNotifWorker {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    /// Run the worker. This spawns as a long-lived tokio task.
    pub async fn run(self) {
        tracing::info!("Bluesky notification sync worker started");
        let mut interval = tokio::time::interval(Duration::from_secs(NOTIF_POLL_INTERVAL_SECS));
        // The first tick completes immediately; `Delay` keeps a missed tick
        // from stacking a burst of catch-up passes after a slow one.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            self.poll_once().await;
        }
    }

    /// One pass over every consume-side linked account.
    ///
    /// A single account's failure — an expired OAuth session, an AppView
    /// timeout — is logged and skipped, never allowed to end the pass: the
    /// next account's notifications are unrelated to this one's credential.
    async fn poll_once(&self) {
        let actors = {
            let conn = self.state.db.conn().await;
            match db_helpers::list_consume_side_linked_actors(&conn) {
                Ok(actors) => actors,
                Err(e) => {
                    tracing::warn!("bluesky notif sync: cannot enumerate linked accounts: {e:#}");
                    return;
                }
            }
        };

        for actor_hex in actors {
            match notif_sync::poll_bluesky_notifications(&self.state, &actor_hex).await {
                Ok(0) => {}
                Ok(n) => tracing::debug!(
                    actor = %actor_hex,
                    inserted = n,
                    "bluesky notif sync: bridged notifications"
                ),
                Err(e) => {
                    tracing::warn!(actor = %actor_hex, "bluesky notif sync: {e:#}")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bluesky::db_helpers::list_consume_side_linked_actors;
    use crate::db::CacheDb;

    /// The D7 gate, at the start site. Four actors: a consume-side link, a
    /// nest-hosted identity, a user holding both, and one with neither. Only
    /// the first may be polled.
    #[tokio::test]
    async fn only_consume_side_linked_actors_are_enumerated() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn().await;
        crate::bluesky::apply_schema(&conn).expect("apply bluesky schema");

        let linked = hex::encode([1u8; 32]);
        let hosted = hex::encode([2u8; 32]);
        let both = hex::encode([3u8; 32]);

        for actor_hex in [&linked, &both] {
            conn.execute(
                "INSERT INTO bluesky_accounts (actor_id, bluesky_did, bluesky_handle, access_token,
                                               refresh_token, dpop_key, token_expires,
                                               created_at, updated_at)
                 VALUES (?1, 'did:plc:ext', 'ext.bsky.social', x'', x'', x'', 0, 0, 0)",
                rusqlite::params![actor_hex],
            )
            .expect("insert consume-side link");
        }
        for actor_hex in [&hosted, &both] {
            conn.execute(
                "INSERT INTO atproto_identities (actor_id, method, status, did, created_at, updated_at)
                 VALUES (?1, 'plc', 'active', ?2, 0, 0)",
                rusqlite::params![
                    hex::decode(actor_hex).unwrap(),
                    format!("did:plc:hosted-{actor_hex}")
                ],
            )
            .expect("insert hosted identity");
        }

        let enumerated = list_consume_side_linked_actors(&conn).expect("enumerate");
        assert_eq!(
            enumerated,
            vec![linked],
            "only the consume-side-only actor may be polled: a nest-hosted \
             backing reads through service-auth proxying (D7), and an actor \
             with no link has nothing to poll"
        );
    }

    fn insert_link(conn: &rusqlite::Connection, actor_id: &str) {
        conn.execute(
            "INSERT INTO bluesky_accounts (actor_id, bluesky_did, bluesky_handle, access_token,
                                           refresh_token, dpop_key, token_expires,
                                           created_at, updated_at)
             VALUES (?1, 'did:plc:ext', 'ext.bsky.social', x'', x'', x'', 0, 0, 0)",
            rusqlite::params![actor_id],
        )
        .expect("insert consume-side link");
    }

    fn insert_active_hosted_identity(conn: &rusqlite::Connection, actor: [u8; 32]) {
        conn.execute(
            "INSERT INTO atproto_identities (actor_id, method, status, did, created_at, updated_at)
             VALUES (?1, 'plc', 'active', ?2, 0, 0)",
            rusqlite::params![
                actor.to_vec(),
                format!("did:plc:hosted-{}", hex::encode(actor))
            ],
        )
        .expect("insert hosted identity");
    }

    /// The gate turns on the ACTOR, never on how a row happens to spell it.
    /// `bluesky_accounts` keys on lowercase hex (`ActorKey::Hex`), but its
    /// production writer took the id straight out of the OAuth callback's
    /// attacker-controlled `state`, so a row spelled any other way is exactly
    /// the row a string comparison against `lower(hex(..))` fails to exclude —
    /// the uppercase spelling of a nest-hosted actor below is D7 failing open.
    /// No spelling but the canonical one names an actor in this table (every
    /// route looks a link up by `hex::encode` of the authenticated actor), so
    /// none of them is ever enumerated, hosted or not.
    #[tokio::test]
    async fn a_non_canonical_spelling_is_never_enumerated() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn().await;
        crate::bluesky::apply_schema(&conn).expect("apply bluesky schema");

        let linked = hex::encode([1u8; 32]);
        insert_link(&conn, &linked);

        let hosted = [0xabu8; 32];
        insert_active_hosted_identity(&conn, hosted);
        insert_link(&conn, &hex::encode(hosted).to_uppercase());

        // The same non-canonical shapes for an actor with no hosted identity.
        insert_link(&conn, &hex::encode([0xcdu8; 32]).to_uppercase());
        insert_link(&conn, &hex::encode([4u8; 31]));
        insert_link(&conn, &format!("{}zz", &hex::encode([5u8; 32])[..62]));

        let enumerated = list_consume_side_linked_actors(&conn).expect("enumerate");
        assert_eq!(
            enumerated,
            vec![linked],
            "only a canonically spelled consume-side-only link may be polled; \
             a nest-hosted actor must stay excluded however its link row is \
             spelled (D7)"
        );
    }

    /// The poll site re-checks D7 for itself rather than trusting whoever
    /// handed it the actor: `get_agent_for_actor` restores the OAuth session by
    /// DID alone and never sees the backing, so without this check a `both`
    /// actor that reached the poll site — a caller bypassing the enumeration,
    /// or a hosted identity activated between the enumeration and this poll —
    /// would run the consume-side path. The control actor proves the refusal
    /// discriminates on the backing: it passes the gate and fails one step on,
    /// at the agent (Bluesky OAuth is unconfigured in this harness).
    #[tokio::test]
    async fn a_both_actor_is_refused_at_the_poll_site() {
        use crate::bluesky::notif_sync::{ConsumeSidePollRefused, poll_bluesky_notifications};

        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        crate::bluesky::init_db(&db).await.unwrap();
        let state = std::sync::Arc::new(crate::routes::AppState::for_test(db));

        let linked = [1u8; 32];
        let both = [3u8; 32];
        {
            let conn = state.db.conn().await;
            insert_link(&conn, &hex::encode(linked));
            insert_link(&conn, &hex::encode(both));
            insert_active_hosted_identity(&conn, both);
        }

        let err = poll_bluesky_notifications(&state, &hex::encode(both))
            .await
            .expect_err("a nest-hosted-backed actor must never be polled consume-side");
        assert!(
            err.downcast_ref::<ConsumeSidePollRefused>().is_some(),
            "the poll site itself must refuse a `both` actor (D7), not fail \
             somewhere past it: {err:#}"
        );

        let err = poll_bluesky_notifications(&state, &hex::encode(linked))
            .await
            .expect_err("no Bluesky OAuth client is configured in this harness");
        assert!(
            err.downcast_ref::<ConsumeSidePollRefused>().is_none(),
            "a consume-side-only actor must pass the poll-site gate: {err:#}"
        );
    }
}
