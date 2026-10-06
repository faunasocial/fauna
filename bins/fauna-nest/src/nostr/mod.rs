//! Nostr protocol integration routes and background tasks.
//! Gated behind the `nostr` cargo feature.

pub mod bridge_leg;
pub mod bridge_provider;
pub mod bunker;
pub mod bunker_handlers;
pub mod content_handlers;
pub mod db;
pub mod federation;
pub mod inbound_lifecycle;
pub mod interact;
pub mod key_crypto;
pub mod nip05;
pub mod oracle;
pub mod publish;
pub mod relay_endpoint;
pub mod relays;
pub mod store;
pub mod sync_worker;
// The NIP-57 zap surfaces — the `zaps` registry member, a SUBSET of `payments`
// (`dynamic-features.md` § Charter members; the Damus precedent). Their
// composition with `nostr` is STRUCTURAL: they sit inside this already-
// `nostr`-gated module, so the reachable surface is `nostr AND zaps` with no
// cargo edge between the two features and no `cfg(all(...))` at each site.
#[cfg(feature = "zaps")]
pub mod zap_ingest;
#[cfg(feature = "zaps")]
pub mod zap_signer_handlers;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

use crate::db::CacheDb;
use crate::routes::AppState;

/// The canonical spelling of a nostr peer pubkey — lowercase 64-hex, no
/// surrounding whitespace — which is what EVERY `guardian_dm_peers` key and
/// Nostr leg room id must be built from, on both arms of the gate.
///
/// The gate compares peer ids as opaque strings, but a nostr pubkey has many
/// spellings for one person: `fauna_core::hex32::decode` accepts either case
/// and trims, so 2^64 mixed-case spellings plus every whitespace-padded
/// variant name a single recipient. Keying a verdict lookup on the spelling
/// the *client* sent (outbound) or the *relay* served (inbound) therefore let a
/// ward walk straight past a guardian's `block` by re-spelling the pubkey, and
/// mint a second `allow` row that shadowed the block rather than being
/// suppressed by its `INSERT OR IGNORE`.
///
/// Canonicalize HERE, where the bridge is known — deliberately **not** in
/// `db/family.rs`. That table's `peer_id` is bridge-generic ("a nostr pubkey
/// hex, a bluesky DID, ...", opaque to the gate), and case-folding is not sound
/// for every future bridge id.
///
/// `None` when `s` is not a 32-byte hex value at all; callers on the gate path
/// fall back to the raw string, which is exactly today's behaviour for a peer
/// id this bridge cannot parse.
pub(crate) fn canonical_peer_pubkey(s: &str) -> Option<String> {
    fauna_core::hex32::decode(s)
        .ok()
        .map(|bytes| fauna_core::hex32::encode(&bytes))
}

/// Is server-side Nostr bridging available on this box?
///
/// Server-side bridging acts as a user's Nostr *agent* — it signs and relays
/// with the user's key and unwraps NIP-17 gift-wrap DMs — so it is keyed on
/// **a user's deposited nsec** (`nostr_accounts.encrypted_privkey`), the
/// explicit per-user trust act (`docs/goal/ui/nostr.md` § The bridging gate;
/// `storage-modes.md` § The transition contract rule 3). This is the ratified
/// Phase-4 S8.9 gate, replacing an interim key on the retired storage mode: any
/// box whose user deposits a key bridges for that user; a box with no
/// depositor has the surface cleanly unavailable.
///
/// The widening is safe because inbound gift-wrap DM content seals at ingest
/// through the D2 resolver (`CacheDb::get_recipient_seal_key` →
/// `seal_recipient_blob`) before storage — a bridging box writes no new
/// plaintext DM bodies at rest.
///
/// This is the box-level half (run the worker, serve the relay endpoint,
/// report the provider available). Per-user enforcement is structural: every
/// per-user act (event signing, gift-wrap unwrap, DM send) requires that
/// account's own deposited key, and the inbound subscription sweep selects
/// depositor accounts only.
pub async fn nostr_bridging_available(state: &crate::routes::AppState) -> bool {
    let conn = state.db.conn().await;
    db::any_nsec_deposited(&conn).unwrap_or_else(|e| {
        tracing::warn!("nostr_bridging_available: read nsec deposits: {e}");
        false // fail closed: no agent act without a positive answer
    })
}

/// Is the box allowed to **serve** the Nostr relay endpoints (`/nostr` WS +
/// `/nostr/info`)? Serving ≠ agency (R8 (account-data-plane.md § The ratified decisions)): a keyless *public* box holds and
/// serves the relay store on behalf of a paired **head** that holds the nsec —
/// it never signs, unwraps, or seals. So this gates the relay endpoints ONLY;
/// [`nostr_bridging_available`] (the agent acts — provider `available`, the
/// sync worker's `bridging_enabled`) stays nsec-only.
///
/// Serving is available when **either** a user deposited an nsec on this box
/// (it is itself a head — the Phase-1 case) **or** some actor holds a
/// non-expired `nostr_push` pairing (this box is the public serving face of a
/// paired head — Phase 2). Both halves fail **closed** on a DB error: no serve
/// without a positive answer.
///
/// Evaluated per-request (not cached), so revoking the `nostr_push` pairing
/// flips the box back to 503 immediately with no boot reconcile — the predicate
/// is derived, never stored (`docs/goal/ui/nostr.md` § The bridging gate →
/// Phase 2; extends the ratified "identity ≠ agency" split: *serving ≠ agency*
/// too).
pub async fn nostr_serving_available(state: &crate::routes::AppState) -> bool {
    // Half 1 — a local nsec deposit (this box is itself a head). Scoped so the
    // db lock is released before half 2 takes it again (same `CacheDb` mutex).
    let deposited = {
        let conn = state.db.conn().await;
        db::any_nsec_deposited(&conn).unwrap_or_else(|e| {
            tracing::warn!("nostr_serving_available: read nsec deposits: {e}");
            false // fail closed
        })
    };
    if deposited {
        return true;
    }
    // Half 2 — a paired head with the `nostr_push` capability (this box proxies
    // for it). Box-wide: any actor's pairing suffices to serve the endpoints.
    state
        .db
        .any_pairing_with_capability(fauna_protocol::pair::capability::NOSTR_PUSH)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("nostr_serving_available: read nostr_push pairings: {e}");
            false // fail closed
        })
}

/// **The one place this bridge's schema is applied** — `init_db` and the
/// `actor_tables` registry guards' seeding both call it (the twin of `activitypub::apply_schema`, whose doc carries the full
/// reasoning).
///
/// `db::CREATE_TABLES_SQL` is this bridge's genesis, applied in the one shape
/// every bridge shares ([`crate::bridge_schema::apply_genesis`]: the block plus
/// the additive column reconciler, no hand-written `ALTER`).
pub fn apply_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    crate::bridge_schema::apply_genesis(conn, db::CREATE_TABLES_SQL)
}

/// Initialize Nostr bridge tables in the nest database.
pub async fn init_db(db: &CacheDb) -> anyhow::Result<()> {
    let conn = db.conn().await;
    apply_schema(&conn)
}

/// Nostr API routes merged into the main router. HTTP/WS residue only — the
/// far end of every route here is a non-Fauna Nostr client (NIP-01/NIP-11/
/// NIP-05); Fauna apps ride WS-RPC (`fauna.nostr.*` + `nostr.*` kinds; the
/// native-content HTTP routes were deleted 2026-07-22, `nostr.md` § WS-RPC
/// migration contract).
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/nostr", get(relay_endpoint::ws_handler))
        .route("/nostr/info", get(relay_endpoint::info_handler))
        // NIP-05 `you@<nest-domain>` identity — served only when the nest is
        // built with the `nostr` feature (this whole router is feature-gated).
        .route("/.well-known/nostr.json", get(nip05::nip05_handler))
}

/// Outbound deletion propagation — the kind-5 leg of self-service post
/// deletion (`feed.md` § State & data shape → *Post deletion*; `nostr.md`
/// § The relay event store, NIP-09): when a deleted Fauna post was
/// materialized into the relay store, a derived Nostr event must not outlive
/// the post it was derived from.
///
/// Looks up the derived event id(s) via `nostr_event_map` (keyed on the
/// lowercase-hex 32-byte digest, `materialize_account`'s convention), signs a
/// native kind-5 with the author's deposited key at the established signing
/// position, stores it (NIP-09 `apply_deletion` then removes the derived
/// rows), broadcasts it live, and enqueues it to the author's configured
/// external relays — the first path that actively pushes a *derived* event to
/// the user's `relay_list` (materialization itself relies on the NIP-65 outbox
/// model; deletion must chase, not wait to be pulled).
///
/// Key-less degradation: if the author's nsec is no longer decryptable (the
/// account unlinked between materialize and delete), the derived rows are
/// still removed locally — local store consistency never depends on key
/// availability; only the outward kind-5 is skipped (logged).
///
/// Idempotent: no map rows → no-op, and the map rows are dropped after
/// propagation so a delete retry doesn't re-publish. Called by
/// `routes::delete_post_core` regardless of its outcome (a crash-retry must
/// still propagate). Non-fatal to the delete — the caller logs and proceeds.
/// Reconcile arm of takedown retraction (`moderation.md` § Legal takedown):
/// retract every post that is under a legal takedown but still has outbound
/// derived event(s) mapped — the state left by a crash between the takedown
/// transaction and its propagation. The immediate path is the takedown handler's own
/// [`propagate_post_delete`] call; this is the safety net, the same
/// immediate-plus-sweep shape materialization itself uses.
///
/// Keyed on `content_meta.legal_takedown_ref` — NOT on quarantine or
/// suppression, which gate future materialization but never retract: they are
/// reversible policy levers, and a kind-5 is a permanent-intent act the
/// un-quarantine could not undo off-box.
///
/// Idempotent by map-row absence: propagation drops the map rows, so a healed
/// post never re-enters the query. Called from the sync worker's tick as a
/// spawned task (never inline — [`propagate_post_delete`] awaits a send on
/// the worker's own outbound channel, which from inside the tick arm would
/// deadlock on a full channel).
pub async fn retract_taken_down_posts(state: &Arc<AppState>) -> anyhow::Result<usize> {
    let flagged: Vec<(String, String)> = {
        let conn = state.db.conn().await;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT m.fauna_post_id, lower(hex(c.author))
               FROM nostr_event_map m
               JOIN content c ON lower(hex(c.id)) = m.fauna_post_id
               JOIN content_meta cm ON cm.content_id = c.id
              WHERE m.direction = 'outbound'
                AND cm.legal_takedown_ref IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let mut retracted = 0usize;
    for (post_hex, author_hex) in flagged {
        let (mut post, mut author) = ([0u8; 32], [0u8; 32]);
        if hex::decode_to_slice(&post_hex, &mut post).is_err()
            || hex::decode_to_slice(&author_hex, &mut author).is_err()
        {
            tracing::warn!("nostr takedown reconcile: malformed ids for {post_hex}");
            continue;
        }
        match propagate_post_delete(state, &author, &post).await {
            Ok(()) => retracted += 1,
            Err(e) => tracing::warn!("nostr takedown reconcile: retract {post_hex}: {e}"),
        }
    }
    Ok(retracted)
}

pub async fn propagate_post_delete(
    state: &Arc<AppState>,
    author: &[u8; 32],
    post_digest: &[u8; 32],
) -> anyhow::Result<()> {
    use fauna_bridge_nostr::signing::Keypair;

    let fauna_post_id = hex::encode(post_digest);
    let actor_hex = hex::encode(author);

    let conn = state.db.conn().await;
    let event_ids = db::list_event_ids_by_fauna_id(&conn, &fauna_post_id)?;
    if event_ids.is_empty() {
        return Ok(());
    }

    // The author's deposited key — the established signing position
    // (`store::materialize_all_exposed`'s decrypt), inlined for one account.
    let account = db::get_account(&conn, &actor_hex)?;
    let keypair = account.as_ref().and_then(|a| {
        let enc = a.encrypted_privkey.as_ref()?;
        let nest_key = state.nest_identity.signing_key.to_bytes();
        let secret = key_crypto::decrypt_nostr_privkey(&nest_key, enc).ok()?;
        Keypair::from_secret_bytes(secret).ok()
    });

    match keypair {
        Some(kp) => {
            let unsigned = fauna_bridge_nostr::translate::kind5_deletion(
                &kp.public_key_bytes(),
                crate::db::now_epoch_secs() as u64,
                &event_ids,
            );
            let event = kp.sign_event(unsigned);
            // Store the kind-5 (`derived=false` — it records a real deletion
            // act, not a recreatable materialization) and apply NIP-09: the
            // derived rows disappear author-scoped, FTS in trigger lockstep.
            store::store_event(&conn, &event, false)?;
            store::apply_deletion(&conn, &event)?;
            db::delete_event_map_by_fauna_id(&conn, &fauna_post_id)?;
            drop(conn);

            // Live subscribers + the author's external relays (crosspost).
            let wire = serde_json::to_string(&event)?;
            let _ = state.nostr.relay_tx.send(relay_endpoint::NostrRelayEvent {
                event_json: wire,
                author_pubkey: event.pubkey.clone(),
            });
            // Best-effort background crosspost, no synchronous caller to refuse
            // to — an explicitly empty relay list (the user removed every
            // relay) degrades correctly here: `handle_outbound` iterates zero
            // relays and does nothing, which is the correct "user opted out of
            // crossposting" behavior, not the bug this file's other callers
            // (`content_handlers.rs`, `interact.rs`) refuse.
            let relay_urls =
                relays::resolve_relay_urls(account.as_ref().and_then(|a| a.relay_list.as_deref()));
            if let Err(e) = state
                .nostr
                .sync_tx
                .send(sync_worker::OutboundEvent { event, relay_urls })
                .await
            {
                tracing::warn!("nostr kind-5 crosspost enqueue failed: {e}");
            }
        }
        None => {
            // Key-less: remove the derived rows directly (author-scoped by
            // construction — the map rows were written by materialization).
            for id in &event_ids {
                store::delete_event_by_id(&conn, id)?;
            }
            db::delete_event_map_by_fauna_id(&conn, &fauna_post_id)?;
            tracing::warn!(
                "nostr post-delete: derived events removed for {fauna_post_id}, \
                 but no deposited key to sign the outward kind-5"
            );
        }
    }
    Ok(())
}
