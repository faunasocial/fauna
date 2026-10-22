//! Durable re-drive of a post deletion's outward legs.
//!
//! `routes::delete_post_core` carries a delete off the box through legs that
//! are each tried once, in the background, a few seconds after the local
//! delete lands: the Bluesky write-through delete retries three times over
//! ~36 s and gives up; the paired-replica twin is a no-op when no worker is
//! connected. Both keep their bookkeeping row on failure — the `bluesky_posts`
//! mapping, the `worker_replication` marker — precisely so the delete can be
//! chased later. The author's own retry chases them (the settled path of
//! `delete_post_core`), but an author whose delete answered `Deleted` has no
//! reason to retry, so a Bluesky outage or a disconnected worker at delete
//! time would otherwise leave the post live off-box for good — the outcome the
//! user-controls-their-data invariant forbids (`principles.md` § The user
//! always controls their data; `feed.md` § Post deletion → Propagation).
//!
//! This module is the chase that needs no retry:
//!
//! - **Bluesky** — an hourly pass over the write-through mappings whose post
//!   is gone or legally taken down (the delete leg's two triggers: the
//!   author's delete, and `moderation_handlers::legal_takedown_core`, whose
//!   withheld post stays on the box), re-attempting `deleteRecord` once each
//!   for the author the `tombstone/post` witness names (both triggers write
//!   it; for a gone post it is the one row left that says whose post it was,
//!   and a post without one is left alone).
//! - **Paired replica** — the same pass over `worker_replication` post markers
//!   whose post is gone, while a worker is connected, and a replay at every
//!   worker connect ([`spawn_replica_redrive`]) — the first moment a delete
//!   missed by a disconnected worker can reach the replica.
//!
//! ActivityPub and Nostr need no pass here: the AP `Delete` is enqueued in the
//! same background step into the durable delivery queue, and Nostr's kind-5
//! runs inline.

use std::sync::Arc;

use crate::nest_link::proxy::WorkerHandle;
use crate::routes::AppState;

/// Re-drive cadence — a hard-coded constant (`principles.md` § One
/// configuration surface: no user or admin chooses it). Hourly: the first
/// attempt already ran seconds after the delete, so this pass only ever chases
/// an outage, and outages are measured in hours; each chase is one outbound
/// request per stranded post, so a tighter cadence buys nothing but load on a
/// PDS that is already failing.
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The most outward deletes one pass attempts per leg. A backlog larger than
/// this (a long outage over a prolific author) drains over several passes
/// instead of bursting at the remote end; the rows wait durably meanwhile. The
/// Bluesky leg rotates its walk ([`BlueskyRedriveCursor`]) so rows that can
/// never succeed do not hold the window; the replica leg needs no rotation, as
/// its rows fail only while no worker is connected, and then all fail alike.
const MAX_REDRIVES_PER_PASS: usize = 256;

/// What one pass over one leg did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RedriveReport {
    /// Stranded posts the pass re-attempted.
    pub attempted: usize,
    /// Of those, the ones whose bookkeeping row is now cleared.
    pub cleared: usize,
}

/// Whether a post is gone from this nest — no live segment record and no
/// `content` row: the same predicate `routes::check_post_delete_authorization`
/// answers `AlreadyGone` on.
async fn post_is_gone(state: &AppState, post_id: &[u8; 32]) -> anyhow::Result<bool> {
    if crate::segments::post::lookup_scope_by_post_id(&state.db, post_id)
        .await?
        .is_some()
    {
        return Ok(false);
    }
    Ok(state.db.get_post_author(post_id).await?.is_none())
}

/// Whether a post's write-through cross-post is owed a delete: the post is
/// gone, or it is withheld by a legal takedown — a takedown keeps the post on
/// the box (tombstone, not delete), so "gone" alone would strand a takedown
/// whose first attempt failed. A restore clears the flag, and the mapping then
/// waits for the author's own delete.
#[cfg(feature = "bluesky")]
async fn crosspost_delete_owed(state: &AppState, post_id: &[u8; 32]) -> anyhow::Result<bool> {
    Ok(post_is_gone(state, post_id).await?
        || state.db.get_post_legal_takedown(post_id).await?.is_some())
}

/// A 32-byte id from its canonical lowercase hex spelling — anything else
/// names no post `delete_post_core` could have written a row for.
#[cfg(feature = "bluesky")]
fn decode_post_id(hex_id: &str) -> Option<[u8; 32]> {
    let id: [u8; 32] = hex::decode(hex_id).ok()?.try_into().ok()?;
    (hex::encode(id) == hex_id).then_some(id)
}

/// Where the Bluesky leg's next pass resumes: the post id (hex) of the last
/// cross-post the previous pass re-attempted. A mapping whose delete can never
/// succeed — its author unlinked Bluesky or revoked the agent, and the mapping
/// is kept so a relink can still delete — stays a candidate for good; without
/// the rotation the first [`MAX_REDRIVES_PER_PASS`] such rows would fill every
/// pass and no other author's stranded cross-post would be reached again. In
/// memory only: a restart begins the rotation afresh, which costs one pass's
/// order, never a row.
#[cfg(feature = "bluesky")]
#[derive(Debug, Default)]
pub(crate) struct BlueskyRedriveCursor(std::sync::Mutex<Option<String>>);

#[cfg(feature = "bluesky")]
impl BlueskyRedriveCursor {
    fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, after: String) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(after);
    }
}

/// The Bluesky cross-posts stranded by a failed delete: `(author, post)` for
/// each write-through mapping whose post is gone or taken down
/// ([`crosspost_delete_owed`]) and whose `tombstone/post` witness names its
/// author. At most [`MAX_REDRIVES_PER_PASS`], walking the mappings in
/// post-id order from just after `after` (the [`BlueskyRedriveCursor`]) and
/// wrapping round, so the cap is a per-pass fairness bound and every stranded
/// cross-post is reached within a bounded number of passes.
#[cfg(feature = "bluesky")]
pub async fn bluesky_redrive_candidates(
    state: &AppState,
    after: Option<&str>,
) -> anyhow::Result<Vec<([u8; 32], [u8; 32])>> {
    let ids = {
        let conn = state.db.conn().await;
        crate::bluesky::db_helpers::list_crosspost_post_ids(&conn)?
    };
    // `ids` is sorted; resume just past `after`, then wrap to the start.
    let split = after.map_or(0, |after| ids.partition_point(|id| id.as_str() <= after));
    let (wrapped, resumed) = ids.split_at(split);
    let mut out = Vec::new();
    for post_id in resumed
        .iter()
        .chain(wrapped)
        .filter_map(|id| decode_post_id(id))
    {
        if !crosspost_delete_owed(state, &post_id).await? {
            continue;
        }
        let Some(author) = state.db.post_delete_witness_author(&post_id).await? else {
            continue;
        };
        out.push((author, post_id));
        if out.len() == MAX_REDRIVES_PER_PASS {
            break;
        }
    }
    Ok(out)
}

/// One re-drive pass over the Bluesky leg: one `deleteRecord` attempt per
/// stranded cross-post ([`bluesky_redrive_candidates`]), resuming after
/// `cursor` and leaving it on the pass's last candidate. A failure leaves the
/// mapping for a later pass (`write_through_delete_inner` clears it only on
/// success).
#[cfg(feature = "bluesky")]
pub(crate) async fn redrive_bluesky_deletes(
    state: &Arc<AppState>,
    cursor: &BlueskyRedriveCursor,
) -> anyhow::Result<RedriveReport> {
    let mut report = RedriveReport::default();
    let candidates = bluesky_redrive_candidates(state, cursor.get().as_deref()).await?;
    if let Some((_, last)) = candidates.last() {
        cursor.set(hex::encode(last));
    }
    for (author, post_id) in candidates {
        report.attempted += 1;
        match crate::bluesky::write_through_delete_inner(state, author, post_id).await {
            Ok(_) => report.cleared += 1,
            Err(e) => tracing::warn!(
                post = %hex::encode(post_id),
                "post-delete re-drive: Bluesky delete still failing: {e:#}"
            ),
        }
    }
    Ok(report)
}

/// The replicated posts stranded by a missed delete: every `worker_replication`
/// post marker whose post is gone. At most [`MAX_REDRIVES_PER_PASS`].
pub(crate) async fn replica_redrive_candidates(state: &AppState) -> anyhow::Result<Vec<[u8; 32]>> {
    let mut out = Vec::new();
    for key in state.db.list_replicated_keys("post").await? {
        let Ok(post_id) = <[u8; 32]>::try_from(key.as_slice()) else {
            continue;
        };
        if post_is_gone(state, &post_id).await? {
            out.push(post_id);
            if out.len() == MAX_REDRIVES_PER_PASS {
                break;
            }
        }
    }
    Ok(out)
}

/// One re-drive pass over the paired-replica leg through `handle`: one
/// `Delete` per stranded replica ([`replica_redrive_candidates`]). The marker
/// clears only on the worker's ack (`routes::replicate_delete_once`).
pub(crate) async fn redrive_replica_deletes(
    state: &Arc<AppState>,
    handle: &WorkerHandle,
) -> anyhow::Result<RedriveReport> {
    let mut report = RedriveReport::default();
    for post_id in replica_redrive_candidates(state).await? {
        report.attempted += 1;
        if crate::routes::replicate_delete_once(state, handle, post_id).await {
            report.cleared += 1;
        }
    }
    Ok(report)
}

fn log_report(leg: &str, result: anyhow::Result<RedriveReport>) {
    match result {
        Ok(r) if r.attempted > 0 => tracing::info!(
            attempted = r.attempted,
            cleared = r.cleared,
            "post-delete re-drive: {leg} leg"
        ),
        Ok(_) => {}
        Err(e) => tracing::error!("post-delete re-drive: {leg} leg: {e:#}"),
    }
}

/// Replay the replica deletes a newly connected worker missed — spawned from
/// the worker's connect (`nest_link::proxy`), generation-scoped like every
/// other background leg.
pub fn spawn_replica_redrive(state: Arc<AppState>, handle: Arc<WorkerHandle>) {
    let scope = Arc::clone(&state);
    scope.spawn_scoped(async move {
        log_report("replica", redrive_replica_deletes(&state, &handle).await);
    });
}

/// Spawn the hourly re-drive via the shared
/// [`crate::sweeper::spawn_periodic_sweeper`] primitive. The first tick fires
/// at boot on purpose: a nest that crashed or restarted mid-outage owes the
/// chase as soon as it is back.
pub fn spawn_post_delete_redrive_sweeper(state: Arc<AppState>) {
    let scope = state.clone();
    #[cfg(feature = "bluesky")]
    let cursor = Arc::new(BlueskyRedriveCursor::default());
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        SWEEP_INTERVAL,
        false,
        move || {
            let state = state.clone();
            #[cfg(feature = "bluesky")]
            let cursor = Arc::clone(&cursor);
            async move {
                #[cfg(feature = "bluesky")]
                log_report("Bluesky", redrive_bluesky_deletes(&state, &cursor).await);
                if let Some(handle) = state.bridge.worker.get_handle().await {
                    log_report("replica", redrive_replica_deletes(&state, &handle).await);
                }
            }
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    const AUTHOR: [u8; 32] = [0x11; 32];

    async fn test_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        #[cfg(feature = "bluesky")]
        crate::bluesky::init_db(&db).await.unwrap();
        Arc::new(AppState::for_test(db))
    }

    /// The durable half of a delete's step 1 for a post that is already gone:
    /// the `tombstone/post` witness naming `author`.
    async fn witness_delete(state: &AppState, author: [u8; 32], post_id: [u8; 32]) {
        let tombstone = fauna_core::data::Tombstone {
            author: fauna_core::identity::ActorId(author),
            post_id: fauna_cbor::Cid::from_digest_dag_cbor(post_id),
            created_at: fauna_core::data::Timestamp(1_710_000_000_000_000),
        };
        let bytes = fauna_core::encoding::canonical_encode(&tombstone).unwrap();
        state
            .db
            .delete_post_projection_with_witness(
                &post_id,
                Some((&author, bytes.as_slice(), 1_710_000_000_000_000)),
            )
            .await
            .unwrap();
    }

    /// A live inline `content` row for `post_id`, authored by `author`.
    async fn live_post(state: &AppState, author: [u8; 32], post_id: [u8; 32]) {
        let conn = state.db.conn().await;
        crate::db::content::insert_content(
            &conn,
            &post_id,
            "post/text",
            &author,
            1_710_000_000_000_000,
            b"body",
            None,
            "fauna",
            None,
        )
        .unwrap();
    }

    #[cfg(feature = "bluesky")]
    fn map_crosspost(conn: &rusqlite::Connection, post_id: [u8; 32], rkey: &str) {
        crate::bluesky::db_helpers::store_crosspost_mapping(
            conn,
            &hex::encode(post_id),
            &format!("at://did:plc:example/app.bsky.feed.post/{rkey}"),
            "did:plc:example",
            "bafy-redrive",
        )
        .unwrap();
    }

    /// Finding: a cross-post whose delete gave up — its mapping retained, its
    /// post gone, its witness naming the author — is re-driven by the sweep,
    /// for that author, and nothing else is: a live post's mapping, an
    /// ingested (someone else's) record's mapping, and a gone post with no
    /// witness to say whose it was.
    #[cfg(feature = "bluesky")]
    #[tokio::test]
    async fn the_sweep_redrives_a_stranded_bluesky_crosspost_and_nothing_else() {
        let state = test_state().await;
        let stranded = [0x21; 32];
        let live = [0x22; 32];
        let ingested = [0x23; 32];
        let unwitnessed = [0x24; 32];
        {
            let conn = state.db.conn().await;
            map_crosspost(&conn, stranded, "a");
            map_crosspost(&conn, live, "b");
            map_crosspost(&conn, unwitnessed, "d");
            crate::bluesky::db_helpers::insert_ingested_post_mapping(
                &conn,
                &hex::encode(ingested),
                "at://did:plc:someone/app.bsky.feed.post/c",
                "bafyc",
                "did:plc:someone",
            )
            .unwrap();
        }
        witness_delete(&state, AUTHOR, stranded).await;
        witness_delete(&state, AUTHOR, ingested).await;
        live_post(&state, AUTHOR, live).await;

        assert_eq!(
            bluesky_redrive_candidates(&state, None).await.unwrap(),
            vec![(AUTHOR, stranded)]
        );

        // The pass re-attempts it. Bluesky is unconfigured in this harness, so
        // the attempt fails at the agent step exactly as the outage did — and
        // the mapping stays for the next pass.
        let report = redrive_bluesky_deletes(&state, &BlueskyRedriveCursor::default())
            .await
            .unwrap();
        assert_eq!(
            report,
            RedriveReport {
                attempted: 1,
                cleared: 0
            }
        );
        let conn = state.db.conn().await;
        assert!(
            crate::bluesky::db_helpers::get_crosspost_uri(&conn, &hex::encode(stranded))
                .unwrap()
                .is_some(),
            "a failed re-drive keeps the mapping for the next pass"
        );
    }

    /// A legal takedown withholds a post without deleting it (tombstone, not
    /// delete), so the post is never "gone" — yet its cross-post must still be
    /// chased when the takedown's own attempt failed (`takedown-and-block.md`
    /// § Legal takedown → *The linked Bluesky account's cross-post is retracted
    /// too*). A restore lifts the takedown and with it the chase: a mapping
    /// still present then waits for an author delete, as any live post's does.
    #[cfg(feature = "bluesky")]
    #[tokio::test]
    async fn a_taken_down_crosspost_is_redriven_until_a_restore_returns_it_to_the_author() {
        let state = test_state().await;
        let taken_down = [0x25; 32];
        live_post(&state, AUTHOR, taken_down).await;
        {
            let conn = state.db.conn().await;
            map_crosspost(&conn, taken_down, "e");
            conn.execute(
                "INSERT OR IGNORE INTO content_meta (content_id) VALUES (?1)",
                rusqlite::params![taken_down.as_slice()],
            )
            .unwrap();
        }
        let takedown = |reference| {
            let state = &state;
            async move {
                state
                    .db
                    .post_legal_takedown_txn(
                        &taken_down,
                        &hex::encode(taken_down),
                        reference,
                        &AUTHOR,
                        [0x2A; 32].as_slice(),
                        "admin=… reference=…",
                        1_720_000_000_000_000,
                    )
                    .await
                    .unwrap();
            }
        };

        assert!(
            bluesky_redrive_candidates(&state, None)
                .await
                .unwrap()
                .is_empty(),
            "a live cross-posted post is not stranded"
        );

        takedown(Some("EU-DSA-2026/1597")).await;
        assert_eq!(
            bluesky_redrive_candidates(&state, None).await.unwrap(),
            vec![(AUTHOR, taken_down)],
            "the takedown's witness names the author and the post is withheld"
        );

        takedown(None).await;
        assert!(
            bluesky_redrive_candidates(&state, None)
                .await
                .unwrap()
                .is_empty(),
            "a restored post's mapping waits for its author's delete"
        );
    }

    /// Finding: mappings whose delete can never succeed (here every one
    /// fails at the agent step) must not hold the per-pass cap for good. With
    /// one more stranded cross-post than a pass may attempt, the next pass
    /// reaches the one the first pass could not.
    #[cfg(feature = "bluesky")]
    #[tokio::test]
    async fn permanently_failing_crossposts_do_not_starve_the_rest() {
        let state = test_state().await;
        let ids: Vec<[u8; 32]> = (0..=MAX_REDRIVES_PER_PASS)
            .map(|i| {
                let mut id = [0x50; 32];
                id[..8].copy_from_slice(&(i as u64).to_be_bytes());
                id
            })
            .collect();
        {
            let conn = state.db.conn().await;
            for (i, id) in ids.iter().enumerate() {
                map_crosspost(&conn, *id, &format!("r{i}"));
            }
        }
        for id in &ids {
            witness_delete(&state, AUTHOR, *id).await;
        }

        let cursor = BlueskyRedriveCursor::default();
        let mut reached = std::collections::HashSet::new();
        for _ in 0..2 {
            let pass = bluesky_redrive_candidates(&state, cursor.get().as_deref())
                .await
                .unwrap();
            reached.extend(pass.into_iter().map(|(_, id)| id));
            let report = redrive_bluesky_deletes(&state, &cursor).await.unwrap();
            assert_eq!(
                report,
                RedriveReport {
                    attempted: MAX_REDRIVES_PER_PASS,
                    cleared: 0
                }
            );
        }
        assert_eq!(
            reached.len(),
            ids.len(),
            "two passes must reach every stranded cross-post"
        );
    }

    /// Wait up to 10 s for the fake worker to have seen `n` deletes.
    async fn await_deletes(deleted: &std::sync::Mutex<Vec<String>>, n: usize) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while deleted.lock().unwrap().len() < n {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the replica leg never sent its Delete"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Finding, the settled path of `delete_post_core`: a retried delete of a
    /// post that is already gone still chases its outward legs — here the
    /// replica a first attempt missed — but only for the author the post-delete
    /// witness names. A stranger's own tombstone over the gone digest passes
    /// the author check (it names the stranger), so the witness is the only
    /// thing between it and the author's legs.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_settled_delete_chases_the_replica_only_for_the_witnessed_author() {
        let state = test_state().await;
        let stranger = [0x12; 32];
        let post_id = [0x41; 32];
        state
            .db
            .mark_replicated("post", &post_id, None)
            .await
            .unwrap();
        witness_delete(&state, AUTHOR, post_id).await;
        let (handle, deleted) = WorkerHandle::fake_acking_deletes();
        state.bridge.worker.connect_for_test(handle).await;

        let tombstone_by = |actor: [u8; 32]| fauna_core::data::Tombstone {
            author: fauna_core::identity::ActorId(actor),
            post_id: fauna_cbor::Cid::from_digest_dag_cbor(post_id),
            created_at: fauna_core::data::Timestamp(1_710_000_000_000_001),
        };
        let delete_as = |actor: [u8; 32]| {
            let state = Arc::clone(&state);
            async move {
                crate::routes::delete_post_core(
                    &state,
                    actor,
                    &tombstone_by(actor),
                    post_id,
                    crate::routes::RenderSite::Now,
                )
                .await
                .map_err(|_| ())
                .expect("settled")
            }
        };

        assert!(matches!(
            delete_as(stranger).await,
            crate::routes::PostDeleteOutcome::AlreadyGone
        ));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            deleted.lock().unwrap().is_empty(),
            "a stranger's settled delete must run none of the author's legs"
        );

        assert!(matches!(
            delete_as(AUTHOR).await,
            crate::routes::PostDeleteOutcome::AlreadyGone
        ));
        await_deletes(&deleted, 1).await;
        assert_eq!(*deleted.lock().unwrap(), vec![hex::encode(post_id)]);
    }

    /// Finding: a replicated post deleted while no worker was connected keeps
    /// its `worker_replication` marker; the replay on the worker's connect
    /// sends its `Delete` and clears the marker. A live replicated post is
    /// left alone.
    #[tokio::test]
    async fn the_replay_chases_a_replica_a_disconnected_worker_missed() {
        let state = test_state().await;
        let gone = [0x31; 32];
        let live = [0x32; 32];
        state.db.mark_replicated("post", &gone, None).await.unwrap();
        state.db.mark_replicated("post", &live, None).await.unwrap();
        live_post(&state, AUTHOR, live).await;

        let (handle, deleted) = WorkerHandle::fake_acking_deletes();
        let report = redrive_replica_deletes(&state, &handle).await.unwrap();

        assert_eq!(
            report,
            RedriveReport {
                attempted: 1,
                cleared: 1
            }
        );
        assert_eq!(*deleted.lock().unwrap(), vec![hex::encode(gone)]);
        assert!(!state.db.is_replicated("post", &gone, None).await.unwrap());
        assert!(state.db.is_replicated("post", &live, None).await.unwrap());
    }
}
