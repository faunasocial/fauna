//! The **sweep plane's** lifecycle: ingest, NIP-09 deletion, NIP-40 expiry.
//!
//! Nostr rests foreign content on the nest by two routes, and they are separate
//! planes with separate tables:
//!
//! * the **relay store** (`nostr_events`) — the `/nostr` endpoint's own event
//!   store, with full NIP-01 replacement / NIP-09 deletion / NIP-40 expiry, all
//!   owned by [`crate::nostr::store`];
//! * the **inbound sweep** (this module) — [`crate::nostr::sync_worker`]
//!   subscribing to followed pubkeys on external relays and translating each
//!   event into a Fauna post (`content` + a `direction = 'inbound'`
//!   `nostr_event_map` row).
//!
//! Until 2026-08-02 the sweep plane had **no removal arm at all**: the
//! subscription asks for kind 5 (`sync_worker`'s `kinds:` filter) but
//! `translate::nostr_event_to_fauna` bails on it, so a followed author's
//! deletion was logged as a translate failure and dropped, and nothing anywhere
//! honoured a NIP-40 `expiration`. That is why `content-index.md` § Bridge
//! content in the Search corpus deliberately left this transit point
//! **unindexed** — indexing it would have minted corpus rows the policy's own
//! removal requirement could not reach. This module is that missing arm, and it
//! is the precondition for the indexing hook that now rides
//! [`ingest_translated_event`].
//!
//! **Goal docs.** `docs/goal/behavior/content-index.md` § Bridge content in the
//! Search corpus (the transit-point-in-lockstep rule and remainder (2b));
//! `docs/goal/ui/feed.md` § State & data shape → *Post deletion* (the teardown
//! order every removal follows); `docs/goal/ui/nostr.md` § The relay event
//! store (the store plane's NIP-09/NIP-40, which this mirrors).
//!
//! **The removal invariant, in one sentence:** every path here ends by deleting
//! the `nostr_event_map` row, and the `nostr_event_map_bridge_search_ad`
//! trigger (`nostr/db.rs`) hangs the Search-corpus removal off that delete — so
//! a future removal path cannot forget the corpus, the same way the store
//! plane's trigger makes its bulk deletes safe.

use anyhow::Result;
use rusqlite::Connection;

use fauna_bridge_nostr::translate;
use fauna_bridge_nostr::types::Event;

use fauna_segment_store::SegmentManager;

use crate::db::CacheDb;
use crate::nostr::{db, store};

/// The bridge id every nostr transit point indexes under.
const BRIDGE: &str = "nostr";

/// The bridged-author transit point (`bridges.md` § Unified feed ingestion →
/// *Bridged authors*): a followed author's kind-0 metadata event, swept in by
/// the subscription's kind-0 filter, becomes their `bridge_authors` row under
/// the same synthetic id their notes rest under. Never stored as an event,
/// never translated, never advancing the posts cursor.
///
/// The event's own `created_at` is the row's stamp, so a lagging relay serving
/// an older kind 0 than the one already projected cannot regress the face
/// (`bridge_authors::upsert` keeps the newest). The caller has already run the
/// signature verify and the author gate; this only maps and writes.
///
/// Returns `true` when a row was written (a malformed or empty kind 0 writes
/// nothing).
pub fn ingest_metadata_event(conn: &Connection, event: &Event) -> Result<bool> {
    debug_assert_eq!(event.kind, fauna_bridge_nostr::types::kind::METADATA);
    let Some(meta) = translate::parse_metadata(&event.content) else {
        tracing::debug!(event_id = %event.id, "nostr sweep: kind 0 with non-object content, dropped");
        return Ok(false);
    };
    let Ok(pubkey) = fauna_core::hex32::decode(&event.pubkey) else {
        return Ok(false);
    };
    let author = crate::db::bridge_authors::BridgeAuthor {
        actor_id: translate::synthetic_actor_id(&pubkey).0,
        bridge: crate::db::bridge_authors::BRIDGE_NOSTR.into(),
        external_id: event.pubkey.clone(),
        handle: meta.handle().map(String::from),
        display_name: meta.display_name.clone(),
        avatar_url: meta
            .picture
            .as_deref()
            .and_then(fauna_core::data::shared_media_proxy_url),
        updated_at: i64::try_from(event.created_at)
            .unwrap_or(i64::MAX / 1_000_000)
            .saturating_mul(1_000_000),
    };
    crate::db::bridge_authors::upsert(conn, &author)
}

/// Translate one inbound event into a Fauna post and record it — the sweep
/// plane's write half, extracted from the worker so the whole ingest contract
/// (expiry rejection, translation, the map row, the Search-corpus hook) is
/// reachable from a test rather than only from a live relay connection.
///
/// Returns the Fauna post id hex on a store, `None` when the event was
/// deliberately not stored (already expired, or untranslatable).
///
/// **NIP-40 at ingest.** An event whose `expiration` has already passed is
/// refused outright, mirroring [`store::store_event_with_origin`] — an expired
/// event must never become a resting row that only the next sweep tick
/// retracts. A live expiration is recorded on `content.expires_at`, which is
/// what [`sweep_expired_inbound`] later finds.
///
/// **Where the body rests.** In the author's `__post` segment, like every other
/// decodable post (`feed.md` § State & data shape → The read model): the write
/// goes through [`crate::segments::post::store_post_with_expiry`], the
/// expiry-carrying door of the post writer every other transit point uses, so
/// the `content` row is a projection with an empty payload and the body is
/// segment-backup-eligible. `post_segments` is the worker's `__post` manager.
pub async fn ingest_translated_event(
    db: &CacheDb,
    post_segments: &SegmentManager,
    event: &Event,
) -> Result<Option<String>> {
    let expiration = store::expiration_value(event);
    if let Some(exp) = expiration
        && exp <= crate::db::now_epoch_secs()
    {
        tracing::debug!(
            event_id = %event.id,
            "nostr sweep: refusing an already-expired inbound event (NIP-40)"
        );
        return Ok(None);
    }

    // `nostr.md` § Replying to and quoting a nostr note → *Reference
    // resolution*: an `e`/`q` target this nest mapped (a swept note, or one of
    // our own derived events) threads under its local post; an unmapped one
    // resolves to nothing and the note rests top-level.
    let local_targets = resolve_reference_targets(db, event).await?;
    let (post, _refs) =
        match translate::nostr_event_to_fauna(event, &|id| local_targets.get(id).copied()) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!("nostr sync: translate event {} failed: {e}", event.id);
                return Ok(None);
            }
        };

    let payload = fauna_core::encoding::canonical_encode(&post)?;
    let id_bytes: [u8; 32] = *blake3::hash(&payload).as_bytes();
    let post_id_hex = hex::encode(id_bytes);

    // The body into the author's segment, the `content` row + feed projection
    // (`content_meta`, which `query_feed` INNER JOINs) beside it, the NIP-40
    // `expiration` on `content.expires_at`. `put_post_with_source`'s bare-insert
    // bypass this replaced kept the expiry but rested the body inline for good.
    crate::segments::post::store_post_with_expiry(
        post_segments,
        db,
        &id_bytes,
        &payload,
        Some(BRIDGE),
        expiration,
    )
    .await?;

    let conn = db.conn().await;
    db::insert_event_map_with_replace_key(
        &conn,
        &post_id_hex,
        &event.id,
        &event.pubkey,
        "inbound",
        store::replace_key(event).as_deref(),
    )?;
    // The Search-corpus hook — `content-index.md` remainder (2b). The SAME
    // function the relay store's transit point calls, so both nostr routes key
    // the corpus identically (`bridge.nostr` : the event id) and an event seen
    // by both indexes exactly once.
    //
    // `derived = false` — the no-double-surfacing rule (`content-index.md`
    // § Bridge content in the Search corpus: "only **inbound, foreign-authored**
    // content indexes"). What makes that true here is that this is the *inbound*
    // plane: a local user's own post is materialized through the outbound path
    // and mapped `outbound`, so it never reaches this function.
    //
    // ⚠ This comment used to justify the flag differently — "everything
    // reaching the sweep is a followed foreign author's event by construction,
    // the subscription's `authors:` filter is the follow list". **That was
    // false**: the `authors:` filter is enforced by the relay, an
    // untrusted party, so nothing about it held by construction. Followed-ness
    // is now a real check, and it is *not* here — it is
    // `db::is_followed_by_sweeping_account`, applied at the relay boundary in
    // `sync_worker::process_inbound_event` before this function is called.
    // Foreign-authored-ness is what this flag rests on, and that one does hold
    // structurally, for the `direction` reason above.
    store::index_into_search_corpus(&conn, event, false, Some(&id_bytes));
    drop(conn);

    // A threaded reply or quote moves its target's counter, exactly as
    // `fauna.posts.create` does for a local post — idempotent on this post's
    // content id, non-fatal. Only then: a reaction's or repost's target is
    // still a synthetic id no row carries, and must not mint engagement rows.
    let threads = post.references.iter().any(|r| {
        matches!(
            r,
            fauna_core::data::Reference::Reply { .. } | fauna_core::data::Reference::Quote { .. }
        )
    });
    if threads {
        let now_us = fauna_core::data::Timestamp::now().as_i64();
        if let Err(e) = db
            .record_reference_engagements(&id_bytes, &post.author.0, &payload, now_us)
            .await
        {
            tracing::warn!("nostr sweep: record_reference_engagements: {e}");
        }
    }

    Ok(Some(post_id_hex))
}

/// The local post ids of the event's `e`/`q` targets this nest has mapped in
/// `nostr_event_map` — the pre-resolved map `translate::nostr_event_to_fauna`
/// reads, since the shared translator is DB-free. Keyed by nostr event id.
async fn resolve_reference_targets(
    db: &CacheDb,
    event: &Event,
) -> Result<std::collections::HashMap<String, fauna_core::data::ContentHash>> {
    let conn = db.conn().await;
    let mut out = std::collections::HashMap::new();
    for tag in &event.tags {
        if !matches!(tag.name(), Some("e") | Some("q")) {
            continue;
        }
        let Some(id) = tag.value() else { continue };
        if out.contains_key(id) {
            continue;
        }
        let Some(entry) = db::get_event_by_nostr_id(&conn, id)? else {
            continue;
        };
        let mut local = [0u8; 32];
        if hex::decode_to_slice(&entry.fauna_post_id, &mut local).is_ok() {
            out.insert(
                id.to_string(),
                fauna_core::data::ContentHash::from_digest_raw(local),
            );
        }
    }
    Ok(out)
}

/// NIP-09 on the sweep plane: apply a followed author's kind-5 deletion request
/// to the posts this nest swept from them. Returns how many were withdrawn.
///
/// The twin of [`store::apply_deletion`], which does the same job on the relay
/// store — same author-scoping rule, different table. `e` tags name event ids;
/// `a` tags name a `kind:pubkey[:d]` coordinate, resolvable because the sweep
/// records one on addressable kinds (four of the kinds it subscribes to are).
/// A tag naming another author's event is silently skipped, per NIP-09.
///
/// **Why this is safe against a local user's own content:** both lookups scope
/// to `direction = 'inbound'`, and a local post's nostr materialization is
/// mapped `outbound` — so a remote kind-5 cannot name one at all. That is the
/// nostr answer to the ownership question AP answers with
/// `is_local_account_post`: whose row is this, and what stops a destructive
/// remote verb from reaching a user's own post.
pub async fn apply_inbound_deletion(db: &CacheDb, deletion: &Event) -> usize {
    let mut targets: Vec<(String, String)> = Vec::new();
    {
        let conn = db.conn().await;
        for tag in &deletion.tags {
            let resolved = match tag.name() {
                Some("e") => tag.value().and_then(|id| {
                    db::resolve_inbound_by_event_id(&conn, id, &deletion.pubkey)
                        .unwrap_or(None)
                        .map(|post_id| (post_id, id.to_string()))
                }),
                Some("a") => tag.value().and_then(|coord| {
                    db::resolve_inbound_by_replace_key(&conn, coord, &deletion.pubkey)
                        .unwrap_or(None)
                }),
                _ => None,
            };
            if let Some(t) = resolved
                && !targets.contains(&t)
            {
                targets.push(t);
            }
        }
    }

    let mut removed = 0usize;
    for (post_id_hex, event_id) in targets {
        withdraw_swept_post(db, &post_id_hex, &event_id, "nip09-delete").await;
        removed += 1;
    }
    if removed > 0 {
        tracing::info!(
            deleter = %&deletion.pubkey[..8.min(deletion.pubkey.len())],
            removed,
            "nostr sweep: applied an inbound NIP-09 deletion"
        );
    }
    removed
}

/// NIP-40 on the sweep plane: withdraw every swept post whose source-side
/// `expiration` has passed. Returns how many were withdrawn.
///
/// Unlike the relay store's [`store::sweep_expired`] — which is pure disk
/// hygiene, because `store::query_events` already excludes expired rows at read
/// — **this sweep is the correctness mechanism**: no `content` reader filters
/// on `expires_at`, so an expired swept post stays servable until the tick that
/// removes it. The exposure is therefore bounded by the worker's cadence rather
/// than being zero, and an already-expired event is refused at ingest
/// ([`ingest_translated_event`]) so the bound only ever applies to an
/// expiration that passes while the post rests here.
pub async fn sweep_expired_inbound(db: &CacheDb) -> usize {
    let now = crate::db::now_epoch_secs();
    let expired: Vec<([u8; 32], String)> = {
        let conn = db.conn().await;
        let ids = match crate::db::content::list_expired_by_source(&conn, BRIDGE, now) {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!("nostr sweep: expired-content query failed: {e:#}");
                return 0;
            }
        };
        ids.into_iter()
            .filter_map(|id| {
                let hexed = hex::encode(id);
                db::inbound_event_id_for_post(&conn, &hexed)
                    .unwrap_or(None)
                    .map(|event_id| (id, event_id))
            })
            .collect()
    };

    let mut removed = 0usize;
    for (id, event_id) in expired {
        withdraw_swept_post(db, &hex::encode(id), &event_id, "nip40-expiry").await;
        removed += 1;
    }
    if removed > 0 {
        tracing::info!(removed, "nostr sweep: withdrew expired inbound posts");
    }
    removed
}

/// Retire one swept post: tear down its projection + segment record, then
/// delete its map row.
///
/// **Order matters, and it is the same order `delete_post_core` uses:** the
/// projection goes first, so a crash between the two steps leaves an
/// unreachable-but-mapped row (harmless, and healed by the next delete or
/// sweep) rather than a mapped-but-served one. The map delete is last because
/// it is the write the corpus-removal trigger hangs off — see this module's
/// header.
async fn withdraw_swept_post(db: &CacheDb, post_id_hex: &str, event_id: &str, verb: &str) {
    crate::bridge_withdraw::withdraw_translated_post(db, post_id_hex, BRIDGE, event_id, verb).await;
    let conn = db.conn().await;
    if let Err(e) = db_delete_map(&conn, post_id_hex, event_id) {
        tracing::warn!(event_id, verb, "nostr sweep: map-row delete failed: {e:#}");
    }
}

fn db_delete_map(conn: &Connection, post_id_hex: &str, event_id: &str) -> Result<()> {
    db::delete_inbound_event_map(conn, post_id_hex, event_id)?;
    Ok(())
}
