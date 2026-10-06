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
//!   is gone, re-attempting `deleteRecord` once each for the author the
//!   post-delete witness names (the witness is the one row left that says
//!   whose post it was; a gone post without one is left alone).
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
/// instead of bursting at the remote end; the rows wait durably meanwhile.
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

/// A 32-byte id from its canonical lowercase hex spelling — anything else
/// names no post `delete_post_core` could have written a row for.
#[cfg(feature = "bluesky")]
fn decode_post_id(hex_id: &str) -> Option<[u8; 32]> {
    let id: [u8; 32] = hex::decode(hex_id).ok()?.try_into().ok()?;
    (hex::encode(id) == hex_id).then_some(id)
}

/// The Bluesky cross-posts stranded by a failed delete: `(author, post)` for
/// each write-through mapping whose post is gone and whose post-delete witness
/// names its author. At most [`MAX_REDRIVES_PER_PASS`].
#[cfg(feature = "bluesky")]
pub(crate) async fn bluesky_redrive_candidates(
    state: &AppState,
) -> anyhow::Result<Vec<([u8; 32], [u8; 32])>> {
    let ids = {
        let conn = state.db.conn().await;
        crate::bluesky::db_helpers::list_crosspost_post_ids(&conn)?
    };
    let mut out = Vec::new();
    for post_id in ids.iter().filter_map(|id| decode_post_id(id)) {
        if !post_is_gone(state, &post_id).await? {
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
/// stranded cross-post ([`bluesky_redrive_candidates`]). A failure leaves the
/// mapping for the next pass (`write_through_delete_inner` clears it only on
/// success).
#[cfg(feature = "bluesky")]
pub(crate) async fn redrive_bluesky_deletes(
    state: &Arc<AppState>,
) -> anyhow::Result<RedriveReport> {
    let mut report = RedriveReport::default();
    for (author, post_id) in bluesky_redrive_candidates(state).await? {
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
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        SWEEP_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move {
                #[cfg(feature = "bluesky")]
                log_report("Bluesky", redrive_bluesky_deletes(&state).await);
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
            bluesky_redrive_candidates(&state).await.unwrap(),
            vec![(AUTHOR, stranded)]
        );

        // The pass re-attempts it. Bluesky is unconfigured in this harness, so
        // the attempt fails at the agent step exactly as the outage did — and
        // the mapping stays for the next pass.
        let report = redrive_bluesky_deletes(&state).await.unwrap();
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
