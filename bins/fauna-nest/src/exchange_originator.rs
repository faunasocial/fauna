//! The federation exchange originator plane (`federation.md` § Federation
//! residue surface, originator-gap note; 2026-07-12 distributed-moderation
//! plan, Phase 1).
//!
//! The `fauna.federation.reports.{exchange,export}` pair was
//! served-but-never-originated: the import/serve machinery was live, but no
//! nest ever pushed or pulled aggregates on its own. This worker is the
//! originator — a scheduler that, per cycle, **pushes** this nest's local
//! exportable aggregates (`*.exchange`) to each peer and **pulls** each peer's
//! (`*.export`), for the reports pair and the trends pair, importing through
//! the exact same validation + peer-bucket path the serving exchange handlers
//! use ([`import_report_entries`] / [`import_trend_entries`] — no second import
//! path exists).
//!
//! **Peer set** (re-assembled every cycle, deduped, self-excluded): on a
//! private nest, the distinct `nest_url`s of the live pairing rows whose actor
//! is an admin of this nest (the deployment's own topology —
//! `private-mode.md` § Pairing Flow), the
//! distinct `feed_contributors` nests, and prior exchange partners
//! (`exchange_peers`, recorded here after a successful cycle — the channel
//! handshake carries no origin URL, so inbound peers are unrecordable; data
//! still flows both ways in one cycle because every cycle pushes AND pulls).
//!
//! **Triggers:** startup (short delay), a periodic tick (~hourly),
//! local-aggregate transitions ([`crate::routes::AppState::exchange_transition_tx`],
//! debounced), and peering events on the same watch — a
//! `fauna.feed.contributors.grant` that adds a new peer nest URL nudges it.
//! The pairing flavor of "peering event" is picked up by the next cycle: the
//! set is re-read from the rows every cycle, and a `fauna.pair.add` on the
//! *public* side yields no dialable URL.
//!
//! Every cadence constant is hard-coded Rust (`principles.md` — a user never
//! chooses exchange cadence, and no operator exists to tune it).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::federation_handlers::{
    FedReportEntry, FedReportsExchangeRequest, FedTrendEntry, FedTrendsExchangeRequest,
    import_report_entries, import_trend_entries,
};
use crate::federation_pool::{
    originate_post_get, originate_reports_exchange, originate_reports_export,
    originate_trends_exchange, originate_trends_export,
};
use crate::routes::AppState;

/// Periodic full-cycle tick — the plan's "hard-coded cadence ~hourly".
const EXCHANGE_TICK: Duration = Duration::from_secs(60 * 60);

/// Delay before the startup cycle, letting the nest finish booting (listeners
/// up, discovery poller loaded) before dialing peers.
const STARTUP_DELAY: Duration = Duration::from_secs(5);

/// Debounce after a local-aggregate transition before pushing — several
/// captures in quick succession (a campaign being flagged) coalesce into one
/// exchange cycle.
const TRANSITION_DEBOUNCE: Duration = Duration::from_secs(10);

/// Per-peer origination throttle: never exchange with the same peer more often
/// than this (the origination-side half of "per-origin throttle both
/// directions"; the serving side's `federation_rate_limit` is the other half).
/// A cycle skips still-throttled peers and reschedules itself for when the
/// earliest becomes due, so a debounced push is delayed, never dropped.
const MIN_PEER_INTERVAL: Duration = Duration::from_secs(60);

/// Exchange epoch stamp: seconds since the Unix epoch. Latest-epoch-wins per
/// peer on the import side, so a coarse wall-clock stamp is exactly enough.
fn now_epoch_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

/// Outcome of one [`run_exchange_cycle`], driving the worker's rescheduling.
#[derive(Debug, Default)]
pub struct CycleStats {
    /// Peers exchanged with (push and/or pull succeeded).
    pub exchanged: usize,
    /// Peers skipped because their [`MIN_PEER_INTERVAL`] hasn't elapsed; the
    /// earliest instant one becomes due (the worker re-runs then if a push is
    /// pending).
    pub throttled_until: Option<Instant>,
    /// Peers that failed (unreachable, no channel, handler error) — logged,
    /// retried next cycle.
    pub failed: usize,
}

/// Assemble the peer URL set: the admins' pairing targets (private nests
/// only) + distinct contributor nests + prior partners, normalized (no
/// trailing `/`) and deduped, preserving source order (pairing first).
async fn assemble_peer_urls(state: &Arc<AppState>) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let push = |url: &str, urls: &mut Vec<String>| {
        let u = url.trim_end_matches('/').to_string();
        if !u.is_empty() && !urls.contains(&u) {
            urls.push(u);
        }
    };
    if crate::nest_sync_worker::is_private(state).await {
        // Rebuilt from the rows and the roster before this cycle's dials, so an
        // admin's private-address target is exempt and a demoted one is not.
        if let Some(rows) = crate::nest_sync_worker::refresh_pairing_targets(state).await {
            rows.iter()
                .filter(|r| r.actor_is_admin && !r.expired)
                .filter_map(|r| r.nest_url.as_deref())
                .for_each(|u| push(u, &mut urls));
        }
    }
    match state.db.list_contributor_nest_urls().await {
        Ok(list) => list.iter().for_each(|u| push(u, &mut urls)),
        Err(e) => tracing::warn!("exchange originator: contributor peer source failed: {e:#}"),
    }
    match state.db.list_exchange_peer_urls().await {
        Ok(list) => list.iter().for_each(|u| push(u, &mut urls)),
        Err(e) => tracing::warn!("exchange originator: partner peer source failed: {e:#}"),
    }
    urls
}

/// One full exchange with one peer: push local aggregates, pull the peer's,
/// record the partnership. The testable seam under [`run_exchange_cycle`]
/// (mirroring `outbox::drain_outbox_once`).
pub async fn exchange_with_peer(state: &Arc<AppState>, peer_url: &str) -> anyhow::Result<()> {
    // Resolve first so a self-referential URL (a contributor row pointing at
    // this nest) is skipped instead of self-dialed.
    let peer_nest_id = state
        .federation_pool
        .resolve_peer_nest_id(peer_url)
        .await
        .map_err(|e| anyhow::anyhow!("resolve {peer_url}: {e}"))?;
    if peer_nest_id == state.nest_identity.public_key_bytes() {
        tracing::debug!("exchange originator: {peer_url} is this nest; skipping");
        return Ok(());
    }
    let epoch = now_epoch_secs();
    // Microsecond wall-clock: the unit `engagement_events.created_at` and the
    // trend recompute/export decay against (`epoch` is coarse seconds, kept for
    // the reports latest-epoch-wins upsert key).
    let now_us = fauna_core::data::Timestamp::now().as_i64();

    // ── push (no push obligation: an empty export skips the leg) ──
    let report_entries: Vec<FedReportEntry> = state
        .db
        .export_report_aggregates()
        .await?
        .into_iter()
        .map(|(hash, factor, count)| FedReportEntry {
            content_hash: hex::encode(hash),
            factor,
            count,
        })
        .collect();
    if !report_entries.is_empty() {
        originate_reports_exchange(
            &state.federation_pool,
            state,
            peer_url,
            &FedReportsExchangeRequest {
                epoch,
                entries: report_entries,
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("push reports to {peer_url}: {e}"))?;
    }
    // Trends push — an **additive** kind (Slice 3): any error here (an unsupported
    // kind or a transport fault) must NOT fail the whole cycle (the reports
    // push already succeeded and the partner still deserves recording), so
    // we log-and-continue instead of `?`. The export is exactly the local
    // k-gate-passed head (`export_trend_entries`), so nothing below k ever rides.
    let trend_entries: Vec<FedTrendEntry> = state
        .db
        .export_trend_entries(now_us)
        .await?
        .into_iter()
        .map(|(id, score_pm, engager_count)| FedTrendEntry {
            content_id: hex::encode(id),
            score_pm,
            engager_count,
        })
        .collect();
    if !trend_entries.is_empty()
        && let Err(e) = originate_trends_exchange(
            &state.federation_pool,
            state,
            peer_url,
            &FedTrendsExchangeRequest {
                epoch,
                entries: trend_entries,
            },
        )
        .await
    {
        tracing::debug!("push trends to {peer_url}: {e} (best-effort; not fatal)");
    }

    // ── pull — imported through the SAME path the serving handlers use ──
    let reports = originate_reports_export(&state.federation_pool, state, peer_url)
        .await
        .map_err(|e| anyhow::anyhow!("pull reports from {peer_url}: {e}"))?;
    import_report_entries(state, &peer_nest_id, epoch, &reports.entries)
        .await
        .map_err(|e| anyhow::anyhow!("import pulled reports from {peer_url}: {e}"))?;
    // Trends pull + import-triggered fetch. Additive kind → swallow any error
    // (an unsupported kind or a transport fault). The import lands the presence bits (the ramp applies to
    // already-seen posts at once); the fetch then surfaces the *unseen* head so
    // its ramp can land too. A pull/import/fetch failure never fails the cycle.
    match originate_trends_export(&state.federation_pool, state, peer_url).await {
        Ok(export) => {
            if let Err(e) = import_trend_entries(state, &peer_nest_id, epoch, &export.entries).await
            {
                tracing::debug!("import pulled trends from {peer_url}: {e}");
            }
            fetch_unseen_trend_posts(state, peer_url, &export.entries, now_us).await;
        }
        Err(e) => {
            tracing::debug!("pull trends from {peer_url}: {e} (best-effort; not fatal)");
        }
    }

    // Remember the partner (prior-partner peer source, next cycles/boots).
    state
        .db
        .record_exchange_peer(peer_url, &peer_nest_id)
        .await?;
    Ok(())
}

/// Import-triggered `post.get` fetch (`trending.md` § Import-triggered fetch): a
/// pulled trend head names posts this nest may never have seen; without the body
/// there is no `content_meta` row, so the peer presence bit alone scores 0 (no
/// blind row). For each **unseen** id — highest `score_pm` hint first, capped at
/// [`MAX_TREND_FETCHES_PER_EXCHANGE`] round-trips per exchange — fetch the post,
/// verify + ingest it, and recompute so the ramp lands. A fetch/verify failure
/// drops just that id (its presence bit waits for a later cycle). Best-effort
/// throughout: never surfaces an error to the exchange cycle.
async fn fetch_unseen_trend_posts(
    state: &Arc<AppState>,
    peer_url: &str,
    entries: &[FedTrendEntry],
    now_us: i64,
) {
    // Unseen head, highest fetch-priority (`score_pm`) first.
    let mut unseen: Vec<([u8; 32], u16)> = Vec::new();
    for entry in entries {
        let Some(content_id) = hex::decode(&entry.content_id)
            .ok()
            .and_then(|v| <[u8; 32]>::try_from(v).ok())
        else {
            continue;
        };
        // A DB error is treated as "seen" — skip the fetch rather than hammer a
        // peer under local trouble (conservative; the sweep/ramp still converge).
        if state
            .db
            .content_meta_exists(&content_id)
            .await
            .unwrap_or(true)
        {
            continue;
        }
        // Nor is a post this nest has seen deleted — fetching it would only be
        // refused at ingest (`ingest_fetched_trend_post`), every cycle.
        if state.db.post_was_deleted(&content_id).await.unwrap_or(true) {
            continue;
        }
        unseen.push((content_id, entry.score_pm));
    }
    unseen.sort_by_key(|(_, score_pm)| std::cmp::Reverse(*score_pm));
    unseen.truncate(crate::db::trends::MAX_TREND_FETCHES_PER_EXCHANGE);

    for (content_id, _) in unseen {
        match originate_post_get(
            &state.federation_pool,
            state,
            peer_url,
            hex::encode(content_id),
        )
        .await
        {
            Ok(reply) => {
                // Empty body = missing/withheld (legal takedown discloses via the
                // marker but withholds the body) — nothing to ingest.
                if reply.post.is_empty() {
                    continue;
                }
                if let Err(e) =
                    ingest_fetched_trend_post(state, &content_id, &reply.post, now_us).await
                {
                    tracing::debug!("drop fetched trend post {}: {e}", hex::encode(content_id));
                }
            }
            Err(e) => {
                tracing::debug!(
                    "fetch trend post {} from {peer_url}: {e}",
                    hex::encode(content_id)
                );
            }
        }
    }
}

/// Verify a peer-fetched post's bytes and ingest it into the public trending
/// plane. The bytes are UNTRUSTED (a peer serves them), so — mirroring
/// `post_forward_handler`'s verify — we:
/// 1. **bind** the bytes to the requested `content_id`: a native post rests
///    under `blake3` of the exact wire bytes it was created as
///    (`routes::ingest_post_core`), so the exporter's id names those bytes and
///    nothing else — a peer must not substitute other content under an id this
///    nest trusts;
/// 2. decode the embed-as-bytes wire + verify the author's Ed25519 signature
///    (which also re-hashes the inner bytes against the envelope CID);
/// 3. accept **public posts only** (the trending plane is public — a gated post
///    never rides the peer plane);
/// 4. refuse a `created_at` past the native future bound — it becomes the
///    column the local feed sorts on (`docs/goal/ui/feed.md` § The read model).
///
/// The binding is deliberately not the envelope's inner CID digest. No native
/// post is keyed by that digest, so binding to it dropped every honest fetch;
/// a row of another plane can be (the since-retired group plane keyed its
/// messages by it), so it would let a hostile peer's fetch overwrite one.
/// (`store_post` refuses an id another plane's row holds, too.)
///
/// Then store via the post-cutover segment body store (which writes the
/// `content_meta` row with `gated_tier = NULL`, so the post is now *seen +
/// public*) and recompute so the distinct-peer ramp applies.
async fn ingest_fetched_trend_post(
    state: &Arc<AppState>,
    content_id: &[u8; 32],
    post_bytes: &[u8],
    now_us: i64,
) -> anyhow::Result<()> {
    if blake3::hash(post_bytes).as_bytes() != content_id {
        anyhow::bail!(
            "content_id mismatch — peer served bytes that do not hash to the requested id"
        );
    }
    let wire: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(post_bytes)
            .map_err(|e| anyhow::anyhow!("decode embed-as-bytes: {e}"))?;
    let (inner_bytes, env) = wire
        .into_signed()
        .map_err(|e| anyhow::anyhow!("split signed envelope: {e}"))?;
    let post: fauna_core::data::Post = fauna_core::encoding::decode_signed_bytes(&inner_bytes)
        .map_err(|e| anyhow::anyhow!("decode post: {e}"))?;
    fauna_core::encoding::verify_envelope(&post, &inner_bytes, &env)
        .map_err(|e| anyhow::anyhow!("verify post signature: {e}"))?;
    // Public trending plane only.
    if post.gated.is_some() {
        anyhow::bail!("gated post — not part of the public trending plane");
    }
    crate::storage::reject_future_created_at(post.created_at)
        .map_err(|r| anyhow::anyhow!("ingest rejected: {}", r.as_snake_case()))?;
    // A deleted post is never re-ingested by replay (`feed.md` § State & data
    // shape → *Post deletion*) — a peer that kept its bytes may still list it.
    if state.db.post_was_deleted(content_id).await? {
        anyhow::bail!("post was deleted on this nest — never re-ingested");
    }
    crate::segments::post::store_post(
        &state.post_segments,
        &state.db,
        content_id,
        post_bytes,
        None,
    )
    .await
    .map_err(|e| anyhow::anyhow!("store fetched post: {e}"))?;
    state
        .db
        .recompute_trend_score(content_id, now_us)
        .await
        .map_err(|e| anyhow::anyhow!("recompute trend score: {e}"))?;
    Ok(())
}

/// One cycle over the assembled peer set, honoring the per-peer throttle.
/// `last_attempt` is the worker's in-memory per-peer throttle clock (an
/// attempt counts whether it succeeded or failed — a flapping peer is not
/// re-dialed in a tight loop).
pub async fn run_exchange_cycle(
    state: &Arc<AppState>,
    last_attempt: &mut HashMap<String, Instant>,
) -> CycleStats {
    let mut stats = CycleStats::default();
    let now = Instant::now();
    for peer_url in assemble_peer_urls(state).await {
        if let Some(at) = last_attempt.get(&peer_url) {
            let due = *at + MIN_PEER_INTERVAL;
            if due > now {
                stats.throttled_until = Some(match stats.throttled_until {
                    Some(t) => t.min(due),
                    None => due,
                });
                continue;
            }
        }
        last_attempt.insert(peer_url.clone(), now);
        match exchange_with_peer(state, &peer_url).await {
            Ok(()) => stats.exchanged += 1,
            Err(e) => {
                stats.failed += 1;
                tracing::warn!("exchange originator: {e:#}; will retry next cycle");
            }
        }
    }
    stats
}

/// Spawn the originator worker. `transition_rx` is the local-aggregate
/// transition watch; `shutdown` mirrors the discovery poller's channel.
pub fn spawn_exchange_originator(
    state: Arc<AppState>,
    transition_rx: watch::Receiver<u64>,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): the handle is returned and its only
    // caller adopts it (`lib.rs`'s `state.scope_handle(spawn_exchange_originator(…))`).
    tokio::spawn(run(state, transition_rx, shutdown))
}

async fn run(
    state: Arc<AppState>,
    mut transition_rx: watch::Receiver<u64>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut last_attempt: HashMap<String, Instant> = HashMap::new();
    // A pending cycle request (startup, debounced transition, or a throttled
    // retry) and when to run it. `None` = only the periodic tick is armed.
    let mut pending_at: Option<Instant> = Some(Instant::now() + STARTUP_DELAY);

    let mut tick = tokio::time::interval(EXCHANGE_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // arm; the startup cycle covers "now"

    loop {
        // Sleep until the pending cycle (if any) — the tick and transition
        // arms below can each preempt with an earlier wake.
        let pending_sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(
            pending_at.unwrap_or_else(|| Instant::now() + EXCHANGE_TICK),
        ));

        tokio::select! {
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    tracing::info!("exchange originator: shutting down");
                    return;
                }
            }
            res = transition_rx.changed() => {
                if res.is_err() {
                    tracing::info!("exchange originator: transition channel closed, exiting");
                    return;
                }
                // Debounce: coalesce a burst of transitions into one cycle,
                // never pushing a pending cycle later than it already is.
                let due = Instant::now() + TRANSITION_DEBOUNCE;
                pending_at = Some(pending_at.map_or(due, |p| p.min(due)));
            }
            _ = tick.tick() => {
                pending_at = Some(Instant::now());
            }
            _ = pending_sleep, if pending_at.is_some() => {
                pending_at = None;
                let stats = run_exchange_cycle(&state, &mut last_attempt).await;
                if stats.exchanged > 0 || stats.failed > 0 {
                    tracing::info!(
                        exchanged = stats.exchanged,
                        failed = stats.failed,
                        "exchange originator: cycle complete"
                    );
                }
                // Throttled peers with a pending reason: re-run when the
                // earliest becomes due (a debounced push is delayed, never
                // dropped).
                if let Some(t) = stats.throttled_until {
                    pending_at = Some(t);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signed public post dated `created_at`, keyed the way a native post
    /// rests: under `blake3` of its exact wire bytes.
    fn signed_public_post_at(
        kp: &fauna_core::identity::ActorKeypair,
        created_at: fauna_core::data::Timestamp,
    ) -> ([u8; 32], Vec<u8>) {
        let post = fauna_core::data::Post {
            author: kp.actor_id(),
            created_at,
            body: fauna_core::data::PostBody::Text {
                content: "a peer's trending post".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let bytes = fauna_core::encoding::sign_and_pack(kp, &post).unwrap();
        (*blake3::hash(&bytes).as_bytes(), bytes)
    }

    /// A peer-served post's signed `created_at` becomes the column the local
    /// feed sorts on (`docs/goal/ui/feed.md` § The read model), so a fetch
    /// dated past the future bound is dropped like a failed verify — while an
    /// honestly dated one from the same author lands, so the refusal cannot be
    /// satisfied by ingesting nothing.
    #[tokio::test]
    async fn a_fetched_trend_post_dated_past_the_future_bound_is_not_ingested() {
        let state = Arc::new(AppState::for_test(Arc::new(
            crate::db::CacheDb::open_in_memory().unwrap(),
        )));
        let kp = fauna_core::identity::ActorKeypair::generate();
        let now = fauna_core::data::Timestamp::now();
        let (ahead_id, ahead) =
            signed_public_post_at(&kp, fauna_core::data::Timestamp(now.0 + 3_600_000_000));
        let (honest_id, honest) = signed_public_post_at(&kp, now);

        let refused = ingest_fetched_trend_post(&state, &ahead_id, &ahead, now.as_i64()).await;
        ingest_fetched_trend_post(&state, &honest_id, &honest, now.as_i64())
            .await
            .expect("an honestly dated trend post is ingested");

        assert!(
            !state.db.post_exists(&ahead_id).await.unwrap(),
            "a fetched trend post dated an hour ahead was stored (fetch result: {refused:?}) — \
             it would lead the local feed until its date"
        );
        assert!(refused.is_err(), "the refused fetch reports why");
        assert!(state.db.post_exists(&honest_id).await.unwrap());
    }

    /// A deleted post is never re-ingested by replay (`feed.md` § State
    /// & data shape → *Post deletion*): a peer that kept the bytes may still
    /// list the id in its trends, and the fetch must not bring the post back.
    /// A legal takedown writes the same witness over a still-live post, so the
    /// was-deleted predicate must read the post as live until the author's
    /// delete actually removes it.
    #[tokio::test]
    async fn a_fetched_trend_post_this_nest_saw_deleted_is_not_re_ingested() {
        let state = Arc::new(AppState::for_test(Arc::new(
            crate::db::CacheDb::open_in_memory().unwrap(),
        )));
        let kp = fauna_core::identity::ActorKeypair::generate();
        let author = kp.actor_id().0;
        let now = fauna_core::data::Timestamp::now();
        let (id, bytes) = signed_public_post_at(&kp, now);
        ingest_fetched_trend_post(&state, &id, &bytes, now.as_i64())
            .await
            .expect("precondition: the first fetch is ingested");

        state
            .db
            .post_legal_takedown_txn(
                &id,
                &hex::encode(id),
                Some("order-1"),
                &author,
                b"admin",
                "test",
                now.as_i64(),
            )
            .await
            .unwrap();
        assert!(
            !state.db.post_was_deleted(&id).await.unwrap(),
            "a taken-down post still stored is withheld, not deleted — its witness alone must not read as a delete"
        );

        let witness = fauna_core::encoding::canonical_encode(&fauna_core::data::Tombstone {
            author: kp.actor_id(),
            post_id: fauna_core::data::PostId::from_digest_dag_cbor(id),
            created_at: now,
        })
        .unwrap();
        assert!(
            state
                .db
                .delete_post_projection_with_witness(&id, Some((&author, &witness, now.as_i64())))
                .await
                .unwrap(),
            "precondition: the author's delete removed the post"
        );
        assert!(state.db.post_was_deleted(&id).await.unwrap());

        let refetched = ingest_fetched_trend_post(&state, &id, &bytes, now.as_i64()).await;
        assert!(refetched.is_err(), "the replayed fetch reports its refusal");
        assert!(
            !state.db.post_exists(&id).await.unwrap(),
            "the deletion stands — the peer's bytes were not re-stored"
        );
    }
}
