//! The consume-side Bluesky feed poller — the worker `bridges.md` § Unified
//! feed ingestion → *Bridge ingestion* describes: each linked account's
//! timeline and subscribed custom feeds, into the unified feed as
//! `bluesky`-source posts.
//!
//! Shaped after [`super::notif_worker::BlueskyNotifWorker`], which is the
//! precedent in every respect that matters: constructed in `lib.rs` behind the
//! `bluesky` feature, spawned with `state.spawn_scoped`, one interval tick
//! driving one pass over `list_consume_side_linked_actors`, and split into a
//! network half (this module's [`poll_bluesky_feeds`]) and an ingest half
//! ([`super::feed_ingest::ingest_feed_posts`]) so every decision about what
//! came back is pinned without a far end.
//!
//! **No configuration knob.** The cadence is a Rust constant — nobody chooses
//! how often a bridge polls (`principles.md` § One configuration surface).
//!
//! **No persisted cursor** (ruling 5). `getTimeline`'s cursor pages into the
//! past, not forward from a mark, so each tick reads the head page and the
//! map's `UNIQUE(at_uri)` discards what the nest already holds.
//!
//! **The D7 hosted-backing gate lives at this start site** (`atproto-pds-full.md`
//! § D7): the enumeration excludes every actor holding an active nest-hosted
//! ATProto identity, and [`poll_bluesky_feeds`] re-checks the same predicate
//! before its first network step — the notification poller's two checks,
//! mirrored, for the same reason (`get_agent_for_actor` never sees the backing).

use std::sync::Arc;
use std::time::Duration;

use fauna_bridge_atproto::atrium_api::app::bsky::feed::{get_feed, get_timeline};
use fauna_bridge_atproto::ingest::{IngestablePost, ingestables_from_feed_item};

use crate::bluesky::notif_sync::ConsumeSidePollRefused;
use crate::bluesky::{db_helpers, feed_ingest};
use crate::routes::AppState;

/// How often each linked account's timeline and custom feeds are polled.
///
/// Five minutes: one `getTimeline` plus one `getFeed` per subscription per
/// account per tick against a shared AppView, and a timeline post is not a
/// latency-critical event — the notification that answers it is, and that
/// poller runs on its own cadence.
pub(crate) const FEED_POLL_INTERVAL_SECS: u64 = 300;

/// How many posts one page asks for. The AppView's ceiling is 100; 50 keeps a
/// tick's translation bounded while a five-minute window on a busy timeline
/// still fits in one page.
const PAGE_LIMIT: u8 = 50;

/// Polls every consume-side linked account's timeline and custom feeds into
/// the unified feed.
pub struct BlueskyFeedWorker {
    state: Arc<AppState>,
}

impl BlueskyFeedWorker {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    /// Run the worker. This spawns as a long-lived tokio task.
    pub async fn run(self) {
        tracing::info!("Bluesky feed ingestion worker started");
        let mut interval = tokio::time::interval(Duration::from_secs(FEED_POLL_INTERVAL_SECS));
        // The first tick completes immediately; `Delay` keeps a missed tick
        // from stacking a burst of catch-up passes after a slow one.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            self.poll_once().await;
        }
    }

    /// One pass over every consume-side linked account. One account's failure
    /// is logged and skipped, never allowed to end the pass.
    async fn poll_once(&self) {
        let actors = {
            let conn = self.state.db.conn().await;
            match db_helpers::list_consume_side_linked_actors(&conn) {
                Ok(actors) => actors,
                Err(e) => {
                    tracing::warn!("bluesky feed ingest: cannot enumerate linked accounts: {e:#}");
                    return;
                }
            }
        };

        for actor_hex in actors {
            match poll_bluesky_feeds(&self.state, &actor_hex).await {
                Ok(0) => {}
                Ok(n) => tracing::debug!(
                    actor = %actor_hex,
                    stored = n,
                    "bluesky feed ingest: stored posts"
                ),
                Err(e) => tracing::warn!(actor = %actor_hex, "bluesky feed ingest: {e:#}"),
            }
        }
    }
}

/// Poll one actor's timeline and every custom feed they subscribe to through
/// this bridge, and store what the nest does not yet hold. Returns how many
/// posts this call stored.
///
/// The one network step of the feature. Referenced posts a page item carries
/// (the reply parent, the quoted record) come out of the translator before the
/// item, and the page is walked oldest-first, so a thread the window covers
/// resolves within the pass.
pub async fn poll_bluesky_feeds(state: &Arc<AppState>, actor_hex: &str) -> anyhow::Result<u64> {
    // D7, re-checked where the consume-side path starts rather than trusted
    // from the caller — the same window the notification poller closes.
    {
        let conn = state.db.conn().await;
        if !db_helpers::consume_side_poll_allowed(&conn, actor_hex)? {
            return Err(ConsumeSidePollRefused {
                actor_hex: actor_hex.to_string(),
            }
            .into());
        }
    }

    let actor: [u8; 32] = hex::decode(actor_hex)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("actor id is not 32 bytes: {actor_hex}"))?;

    let agent = db_helpers::get_agent_for_actor(state, actor_hex)
        .await
        .map_err(|_| anyhow::anyhow!("no bluesky agent for actor {actor_hex}"))?;

    let limit = Some(
        PAGE_LIMIT
            .try_into()
            .expect("a non-zero page limit within the ceiling"),
    );

    let mut page: Vec<IngestablePost> = Vec::new();

    let timeline = agent
        .api
        .app
        .bsky
        .feed
        .get_timeline(
            get_timeline::ParametersData {
                algorithm: None,
                cursor: None,
                limit,
            }
            .into(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("Bluesky getTimeline error: {e}"))?;
    // Newest-first on the wire; oldest-first into the store, so a parent that
    // arrived earlier in the window rests before its reply asks for it.
    page.extend(
        timeline
            .feed
            .iter()
            .rev()
            .flat_map(ingestables_from_feed_item),
    );

    let feeds = state.db.list_bridge_feeds(&actor).await?;
    for sub in feeds.into_iter().filter(|f| f.bridge == "bluesky") {
        let feed = match agent
            .api
            .app
            .bsky
            .feed
            .get_feed(
                get_feed::ParametersData {
                    cursor: None,
                    feed: sub.feed_uri.clone(),
                    limit,
                }
                .into(),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => {
                // One feed generator's outage is not the timeline's problem.
                tracing::warn!(
                    actor = %actor_hex,
                    feed = %sub.feed_uri,
                    "bluesky feed ingest: getFeed error: {e}"
                );
                continue;
            }
        };
        page.extend(feed.feed.iter().rev().flat_map(ingestables_from_feed_item));
    }

    let stored = feed_ingest::ingest_feed_posts(&state.db, &state.post_segments, &page).await?;
    Ok(stored.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

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

    /// D7 at this poller's poll site, the notification poller's pin mirrored:
    /// a `both` actor is refused before any network step; a consume-side-only
    /// actor passes the gate and fails one step on, at the agent (Bluesky
    /// OAuth is unconfigured in this harness).
    #[tokio::test]
    async fn a_both_actor_is_refused_at_the_poll_site() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        crate::bluesky::init_db(&db).await.unwrap();
        let state = Arc::new(crate::routes::AppState::for_test(db));

        let linked = [1u8; 32];
        let both = [3u8; 32];
        {
            let conn = state.db.conn().await;
            insert_link(&conn, &hex::encode(linked));
            insert_link(&conn, &hex::encode(both));
            insert_active_hosted_identity(&conn, both);
        }

        let err = poll_bluesky_feeds(&state, &hex::encode(both))
            .await
            .expect_err("a nest-hosted-backed actor must never be polled consume-side");
        assert!(
            err.downcast_ref::<ConsumeSidePollRefused>().is_some(),
            "the poll site itself must refuse a `both` actor (D7): {err:#}"
        );

        let err = poll_bluesky_feeds(&state, &hex::encode(linked))
            .await
            .expect_err("no Bluesky OAuth client is configured in this harness");
        assert!(
            err.downcast_ref::<ConsumeSidePollRefused>().is_none(),
            "a consume-side-only actor must pass the poll-site gate: {err:#}"
        );
    }
}
