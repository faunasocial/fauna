//! Background worker that drains the outbox, forwarding a private nest's posts
//! to the nests their authors paired it with over the federation WS-RPC channel.
//!
//! Producer: [`crate::routes::maybe_enqueue_outbox`], called from
//! `ingest_post_core` on every `fauna.posts.create` that lands on a private nest
//! whose author holds a local pairing row carrying `post_forward` — the user's
//! own choice in the app, no config-file switch (`private-mode.md`
//! § Implementation status today, ruled 2026-10-01). It queues the post's canonical
//! `EmbedAsBytes` body **verbatim** — the same bytes `store_post` wrote, whose
//! blake3 is the content-addressed `post_id`. Consumer: this worker, which
//! splits that body back into `(post_bytes, post_envelope)` and sends it as
//! `fauna.federation.post.forward`.
//!
//! Carrier = channel only. The HTTP twin `POST /api/v1/forward` this worker used
//! to target was deleted in Spec Y2 slice 5; only the post's **own** author
//! sign-over-CID envelope rides the wire, since the channel authenticates this
//! nest to the peer once at handshake.
//!
//! Goal: `docs/goal/architecture/nest/private-mode.md` § Post Forwarding.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use tokio::time;

use crate::db::OutboxEntry;
use crate::db::pairing::bounded_failure_reason;
use crate::routes::AppState;

/// `outbox.entry_type` for a forwarded post.
/// Posts only: there is no blob-forwarding kind (`private-mode.md` § Post
/// Forwarding).
pub const ENTRY_TYPE_FORWARDED_POST: &str = "forwarded_post";

/// `outbox.entry_type` for a forwarded post **deletion** — the delete twin of
/// [`ENTRY_TYPE_FORWARDED_POST`]. The payload is the author-signed
/// embed-as-bytes `Tombstone` (the `req.body` from `fauna.posts.delete`),
/// relayed to the paired public nest over `fauna.federation.post.delete` so a
/// forwarded post's copy there does not outlive the original (`feed.md` § Post
/// deletion → Propagation).
pub const ENTRY_TYPE_FORWARDED_DELETE: &str = "forwarded_delete";

/// Entries drained per pass.
const BATCH: i64 = 50;

/// Poll interval after a pass that found work — drain a backlog quickly.
const BUSY_POLL: Duration = Duration::from_secs(1);

/// Poll interval when the queue is empty.
const IDLE_POLL: Duration = Duration::from_secs(5);

/// Backoff after the `outbox_pending` read itself failed.
const QUERY_ERROR_BACKOFF: Duration = Duration::from_secs(10);

/// Outcome of one [`drain_outbox_once`], driving the worker's backoff. Mirrors
/// `nest_sync_worker::SyncCycleOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// `forwarded` entries were accepted by the peer and left the queue;
    /// `failed` had their retry counter advanced — kept, except one that
    /// reached the retry ceiling with nothing able to deliver it.
    Processed { forwarded: usize, failed: usize },
    /// Nothing pending. Long backoff.
    Idle,
    /// The resolved NAT mode is not private: the pass did nothing.
    NotPrivate,
    /// The `outbox_pending` read itself failed; longest backoff before retry.
    QueryFailed,
}

pub async fn run_outbox_worker(state: Arc<AppState>) {
    loop {
        let backoff = match drain_outbox_once(&state).await {
            DrainOutcome::Processed { .. } => BUSY_POLL,
            DrainOutcome::Idle => {
                // "Idle" only means nothing is *due*. Surface a queue that is
                // backed-off or stuck: an entry past the retry ceiling is a
                // refusal that has outlasted the whole backoff and is still
                // waiting for a grant, and a peer that keeps refusing shows up
                // as depth long before that.
                if let Ok(stuck) = state.db.outbox_stuck_count().await
                    && stuck > 0
                {
                    tracing::warn!(
                        "outbox: {stuck} entries still refused past the retry ceiling \
                         (retrying about every 8.5 hours)"
                    );
                } else if let Ok(depth) = state.db.outbox_depth().await
                    && depth > 0
                {
                    tracing::debug!("outbox: {depth} entries awaiting retry backoff");
                }
                IDLE_POLL
            }
            DrainOutcome::NotPrivate => IDLE_POLL,
            DrainOutcome::QueryFailed => QUERY_ERROR_BACKOFF,
        };
        time::sleep(backoff).await;
    }
}

/// One drain pass: forward every pending entry over the federation channel to
/// the `nest_url` of each of its author's live local pairing rows carrying
/// `post_forward` — delivered only once every such nest accepted it (the kind
/// is idempotent, so a retry re-sending to a nest that already holds the post
/// is harmless). Acts only while the resolved NAT mode is private. The
/// testable seam of [`run_outbox_worker`], mirroring
/// `nest_sync_worker::run_sync_cycle`.
///
/// A failed entry stays queued and advances its retry counter
/// ([`crate::db::CacheDb::outbox_record_failure`]). Two failure classes, told
/// apart by where the failure happened:
/// - **the send failed** — a transient peer outage, or a *permanent* refusal
///   (the pairing lacks `post_forward`, or the relay rejected the content).
///   Retried for ever at the capped backoff, so a later capability grant or
///   policy change on the relay still delivers it, however late.
/// - **the request could not even be built** from the stored bytes (they do
///   not decode, or the type has no federation kind), **or its author holds no
///   pairing row that could carry it** (none left with `post_forward` and a
///   `nest_url` — a revoked or narrowed pairing). Nothing this nest holds can
///   make it deliverable, so it leaves at the retry ceiling — kept until then,
///   so one decode bug or a re-pair inside the window does not eat a stored
///   post on its first failure.
///
/// An entry whose author no longer has an account here also leaves at the
/// ceiling, whatever the failure (`private-mode.md` § Post Forwarding).
pub async fn drain_outbox_once(state: &Arc<AppState>) -> DrainOutcome {
    if !crate::nest_sync_worker::is_private(state).await {
        return DrainOutcome::NotPrivate;
    }
    let pending = match state.db.outbox_pending(BATCH).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("outbox query failed: {e}");
            return DrainOutcome::QueryFailed;
        }
    };
    if pending.is_empty() {
        return DrainOutcome::Idle;
    }

    // Rebuild the pins and the address-guard exemption from the rows and the
    // admin roster as they stand for this pass's dials.
    crate::nest_sync_worker::refresh_pairing_targets(state).await;

    // Each author's forwarding targets, read once per pass.
    let mut targets_by_author: HashMap<Option<Vec<u8>>, anyhow::Result<Vec<String>>> =
        HashMap::new();
    let (mut forwarded, mut failed) = (0usize, 0usize);
    for entry in pending {
        if !targets_by_author.contains_key(&entry.author_id) {
            let t = forwarding_targets(state, entry.author_id.as_deref()).await;
            targets_by_author.insert(entry.author_id.clone(), t);
        }
        let targets = &targets_by_author[&entry.author_id];
        let (undeliverable, result) = match (build_request(&entry), targets) {
            (Err(e), _) => (true, Err(e)),
            (Ok(_), Err(e)) => (false, Err(anyhow!("{e:#}"))),
            (Ok(_), Ok(t)) if t.is_empty() => (
                true,
                Err(anyhow!(
                    "the author holds no pairing row carrying post_forward with a nest_url"
                )),
            ),
            (Ok(request), Ok(t)) => (false, send_to_all(state, t, request).await),
        };
        match result {
            Ok(()) => {
                if let Err(e) = state.db.outbox_mark_sent(entry.id).await {
                    // The peer already holds the post and the kind is idempotent,
                    // so the redelivery a surviving row causes is harmless.
                    tracing::warn!("outbox: entry {} forwarded but not dequeued: {e}", entry.id);
                }
                forwarded += 1;
            }
            Err(e) => {
                // Bounded before it is logged or stored: a refusal's text is
                // partly the relay's own error code.
                let reason = bounded_failure_reason(&format!("{e:#}"));
                match state
                    .db
                    .outbox_record_failure(entry.id, undeliverable, &reason)
                    .await
                {
                    Ok(true) => tracing::error!(
                        "outbox: forwarding entry {} failed: {reason}; it reached the retry \
                         ceiling and can never be delivered, so it left the queue",
                        entry.id
                    ),
                    Ok(false) => tracing::warn!(
                        "outbox: forwarding entry {} failed: {reason}; will retry",
                        entry.id
                    ),
                    Err(db_err) => tracing::warn!(
                        "outbox: forwarding entry {} failed: {reason}; recording the \
                         failure failed too: {db_err}",
                        entry.id
                    ),
                }
                failed += 1;
            }
        }
    }
    DrainOutcome::Processed { forwarded, failed }
}

/// The `nest_url`s of `author`'s live local pairing rows carrying
/// `post_forward`, normalized and deduped. An entry with no stamped author
/// has none.
async fn forwarding_targets(
    state: &Arc<AppState>,
    author: Option<&[u8]>,
) -> anyhow::Result<Vec<String>> {
    let Some(author) = author else {
        return Ok(Vec::new());
    };
    let rows = state
        .db
        .author_pairing_targets(author)
        .await
        .context("read the author's pairing rows")?;
    let mut urls: Vec<String> = Vec::new();
    for row in rows {
        if row.expired || !row.has_capability(fauna_protocol::pair::capability::POST_FORWARD) {
            continue;
        }
        if let Some(url) = row.nest_url.as_deref() {
            let url = url.trim_end_matches('/').to_string();
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    Ok(urls)
}

/// Send `request` to every target; the first failure is the entry's.
async fn send_to_all(
    state: &Arc<AppState>,
    targets: &[String],
    request: ForwardRequest,
) -> anyhow::Result<()> {
    let mut first_err = None;
    for target in targets {
        if let Err(e) = send_request(state, target, request.clone()).await {
            first_err.get_or_insert_with(|| anyhow!("{target}: {e:#}"));
        }
    }
    first_err.map_or(Ok(()), Err)
}

/// The federation request one queued entry becomes.
#[derive(Clone)]
enum ForwardRequest {
    Post {
        post_bytes: Vec<u8>,
        post_envelope: Vec<u8>,
        signer_auth: Option<Vec<u8>>,
    },
    Delete {
        tombstone: Vec<u8>,
    },
}

/// Build the request from the stored bytes, touching nothing outside this
/// nest — so a failure here is the entry's own, and no retry can cure it.
/// Splits the stored `EmbedAsBytes` body back into the `(post_bytes,
/// post_envelope)` pair the `fauna.federation.post.forward` request carries —
/// no re-encode and no re-sign, so the peer derives the same content-addressed
/// `post_id` this nest did.
fn build_request(entry: &OutboxEntry) -> anyhow::Result<ForwardRequest> {
    match entry.entry_type.as_str() {
        ENTRY_TYPE_FORWARDED_POST => {
            let wire: fauna_core::encoding::EmbedAsBytes =
                fauna_core::encoding::canonical_decode(&entry.payload)
                    .context("outbox payload is not a canonical embed-as-bytes post")?;
            // Carry the delegated-authoring cert (D10), when present, so the
            // peer verifies + re-stores the same wire — no split/drop.
            let signer_auth = wire
                .signer_auth
                .as_deref()
                .map(fauna_core::encoding::canonical_encode)
                .transpose()
                .context("re-encode signer_auth cert for forward")?;
            Ok(ForwardRequest::Post {
                post_bytes: wire.bytes,
                post_envelope: wire.envelope,
                signer_auth,
            })
        }
        // The payload is the signed embed-as-bytes tombstone verbatim; the
        // peer re-verifies the author envelope (`decode_tombstone`), so no
        // split/re-encode here — relay the bytes as stored.
        ENTRY_TYPE_FORWARDED_DELETE => Ok(ForwardRequest::Delete {
            tombstone: entry.payload.clone(),
        }),
        other => Err(anyhow!("unknown outbox entry_type {other:?}")),
    }
}

/// Send one built request to the peer.
async fn send_request(
    state: &Arc<AppState>,
    peer_url: &str,
    request: ForwardRequest,
) -> anyhow::Result<()> {
    match request {
        ForwardRequest::Post {
            post_bytes,
            post_envelope,
            signer_auth,
        } => {
            crate::federation_pool::originate_post_forward(
                &state.federation_pool,
                state,
                peer_url,
                post_bytes,
                post_envelope,
                signer_auth,
            )
            .await?;
        }
        ForwardRequest::Delete { tombstone } => {
            crate::federation_pool::originate_post_delete(
                &state.federation_pool,
                state,
                peer_url,
                tombstone,
            )
            .await?;
        }
    }
    Ok(())
}
