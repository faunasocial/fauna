//! Phase-2 proxy-delegation federation groundwork.
//!
//! The keyless public *serving* box and the paired head holding the deposited
//! `nsec` bridge the actor's Nostr relay over two head-originated channel kinds
//! `fauna.federation.sync.nostr_{push,pull}` (`docs/goal/ui/nostr.md` § The
//! bridging gate → Phase 2). This module holds the location-independent store
//! groundwork both legs stand on:
//!
//! - **Selection** — [`list_events_for_push`] / [`list_events_for_pull`] pick an
//!   actor's `origin='ingest'` rows in compound `(stored_at, id)` order, strictly
//!   after a head-persisted cursor (spec R5 (account-data-plane.md § The ratified decisions)/R6). Federation-arrived rows are
//!   never re-exported (they carry [`store::ORIGIN_FEDERATION`], not
//!   [`store::ORIGIN_INGEST`]) — that kills echo and loops (spec R4).
//! - **Ingest** — [`ingest_federated_event`] is the class-1 relay arm's contract
//!   factored for a federation leg (spec R7): parse → verify → scope-gate → store
//!   `origin='federation'` → kind-5 deletion → broadcast → the kind-1059 seal
//!   seam (which self-gates keyless, so a public box no-ops it). Every store
//!   invariant carries because the same store functions run (constraint (ii)).
//!
//! The handlers that call these (`nostr_push_handler`/`nostr_pull_handler`) and
//! the head-side worker arm are later slices (P2.3/P2.4).

use std::sync::Arc;

use anyhow::Result;
use rusqlite::{Connection, params};

use fauna_bridge_nostr::signing::verify_event;
use fauna_bridge_nostr::types::Event;

use crate::nostr::relay_endpoint::NostrRelayEvent;
use crate::nostr::store;
use crate::routes::AppState;

/// One row selected for a federation leg: the verbatim signed wire JSON plus the
/// compound `(stored_at, id)` cursor position that ordered it. The head advances
/// its persisted cursor to `(stored_at, id)` after the leg succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FederationRow {
    pub stored_at: i64,
    pub id: String,
    pub raw_json: String,
}

/// Select the head's **push** rows for `pubkey`, strictly after the compound
/// cursor `after = (stored_at, id)`, newest-cursor-last (ascending). Scope
/// (spec R6, push leg): `origin='ingest'` **and** either authored by the actor
/// (`pubkey = ?`) **or** a kind-1059 gift wrap addressed to them
/// (`#p = pubkey`) — the public box may hold the actor's inbox wraps per
/// constraint (ii). Ordered `(stored_at, id)` ascending; the cursor comparison
/// is the compound `(stored_at > ?) OR (stored_at = ? AND id > ?)`.
pub(crate) fn list_events_for_push(
    conn: &Connection,
    pubkey: &str,
    after: (i64, &str),
    limit: usize,
) -> Result<Vec<FederationRow>> {
    // Push additionally restricts the `#p` branch to kind-1059 wraps.
    list_events_after_cursor(
        conn,
        pubkey,
        after,
        limit,
        "pubkey = ?1
         OR (kind = 1059
             AND id IN (SELECT event_id FROM nostr_event_tags
                        WHERE name = 'p' AND value = ?1))",
    )
}

/// Select the head's **pull** rows for `pubkey`, strictly after the compound
/// cursor `after = (stored_at, id)`, ascending. Scope (spec R6, pull leg):
/// `origin='ingest'` **and** either authored by the actor (`pubkey = ?`) **or**
/// any event addressed to them (`#p = pubkey`, unrestricted by kind — wraps +
/// class-2 addressed events the user's external clients deposited on the public
/// relay). Same `(stored_at, id)` ordering and strict-after cursor as
/// [`list_events_for_push`].
pub(crate) fn list_events_for_pull(
    conn: &Connection,
    pubkey: &str,
    after: (i64, &str),
    limit: usize,
) -> Result<Vec<FederationRow>> {
    // Pull's `#p` branch is unrestricted by kind (the ingest scope gate matches).
    list_events_after_cursor(
        conn,
        pubkey,
        after,
        limit,
        "pubkey = ?1
         OR id IN (SELECT event_id FROM nostr_event_tags
                   WHERE name = 'p' AND value = ?1)",
    )
}

/// Shared body of the two selection queries — identical `origin='ingest'`
/// filter, `(stored_at, id)` compound ordering, and strict-after cursor; only
/// the `scope_clause` (which references `?1` = `pubkey`) differs. Bind order:
/// `?1` pubkey, `?2` cursor stored_at, `?3` cursor id, `?4` limit.
fn list_events_after_cursor(
    conn: &Connection,
    pubkey: &str,
    after: (i64, &str),
    limit: usize,
    scope_clause: &str,
) -> Result<Vec<FederationRow>> {
    let sql = format!(
        "SELECT stored_at, id, raw_json FROM nostr_events
          WHERE origin = '{origin}'
            AND ({scope_clause})
            AND (stored_at > ?2 OR (stored_at = ?2 AND id > ?3))
          ORDER BY stored_at ASC, id ASC
          LIMIT ?4",
        origin = store::ORIGIN_INGEST,
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![pubkey, after.0, after.1, limit as i64], |r| {
            Ok(FederationRow {
                stored_at: r.get(0)?,
                id: r.get(1)?,
                raw_json: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Whether `event` is in the proxied actor's federation scope (spec R6/R7, the
/// `#p ∪ author` rule): authored by `actor_pubkey`, or addressed to them via a
/// `#p` tag. The ingest gate uses the unrestricted-by-kind form (matching the
/// pull leg); the push leg additionally narrows its `#p` branch to kind-1059 in
/// SQL, which is a *selection* refinement, not an acceptance one.
fn event_in_actor_scope(event: &Event, actor_pubkey: &str) -> bool {
    event.pubkey == actor_pubkey
        || event
            .tags
            .iter()
            .any(|t| t.name() == Some("p") && t.value() == Some(actor_pubkey))
}

/// The outcome of an [`ingest_federated_event`] call — what the push handler
/// turns into its `FedNostrPushReply` counters (spec R6, reject handling).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IngestOutcome {
    /// The event newly entered the store (fresh insert or a replaceable
    /// supersede) — it was broadcast, and a kind-1059 fed the seal seam.
    Stored,
    /// A no-op the leg treats as success: the id was already present, the row
    /// was superseded by an existing newer version, or the kind is ephemeral.
    Duplicate,
    /// A gate failed (malformed JSON, bad signature, out-of-scope pubkey, or a
    /// store cap / expiry). The row is neither stored nor destroyed; the push
    /// cursor advances past it (a permanently-rejectable row must not stall the
    /// leg). `reason` is the wire-facing `FedNostrReject.reason`.
    Rejected(String),
}

/// Ingest one federation-arrived event for the proxied `actor_pubkey` (spec R7).
/// The class-1 relay EVENT arm's contract, factored: parse the signed wire JSON
/// → [`verify_event`] → scope-gate (author or `#p` == `actor_pubkey`) →
/// [`store::store_event_with_origin`] with [`store::ORIGIN_FEDERATION`] → on
/// kind 5 apply the author-scoped NIP-09 deletion → broadcast a newly-stored
/// row to live subscribers → on kind 1059 feed the shared S8.9 seal seam
/// ([`sync_worker::process_gift_wrap_inbound`](crate::nostr::sync_worker::process_gift_wrap_inbound)),
/// which self-gates on the deposited nsec (on the head it unwraps + seals into
/// the bridged-conversation family; on a keyless public box it no-ops). Every store invariant (caps,
/// FTS exclusion, replacement, recipient gating) carries because the same store
/// functions run — constraint (ii) by construction.
pub(crate) async fn ingest_federated_event(
    state: &Arc<AppState>,
    actor_pubkey: &str,
    raw_json: &str,
) -> Result<IngestOutcome> {
    let event: Event = match serde_json::from_str(raw_json) {
        Ok(e) => e,
        Err(e) => {
            return Ok(IngestOutcome::Rejected(format!(
                "malformed event JSON: {e}"
            )));
        }
    };

    if !verify_event(&event) {
        return Ok(IngestOutcome::Rejected("invalid signature".into()));
    }

    if !event_in_actor_scope(&event, actor_pubkey) {
        return Ok(IngestOutcome::Rejected(
            "event out of scope for actor (not authored by nor addressed to it)".into(),
        ));
    }

    let conn = state.db.conn().await;
    // `derived=false` — a genuinely received event, not a Fauna-post
    // materialization; `origin='federation'` so it is never re-exported.
    let outcome = store::store_event_with_origin(&conn, &event, false, store::ORIGIN_FEDERATION)?;

    // NIP-09: a stored kind-5 deletion removes the events/coordinates its
    // e/a tags name, author-scoped. Mirrors the relay EVENT arm — runs on any
    // Ok outcome (including Duplicate: a resent deletion re-applying is a
    // harmless no-op, and skipping it on Duplicate would drop a legit retry).
    if event.kind == 5
        && let Err(e) = store::apply_deletion(&conn, &event)
    {
        tracing::warn!("nostr federation ingest: apply deletion failed: {e}");
    }
    drop(conn);

    let ingest = match outcome {
        store::StoreOutcome::Stored | store::StoreOutcome::Replaced => IngestOutcome::Stored,
        store::StoreOutcome::Duplicate
        | store::StoreOutcome::Superseded
        | store::StoreOutcome::Ephemeral => IngestOutcome::Duplicate,
        store::StoreOutcome::Expired => IngestOutcome::Rejected("event is expired".into()),
        store::StoreOutcome::CapExceeded => {
            IngestOutcome::Rejected("store capacity reached".into())
        }
    };

    if ingest == IngestOutcome::Stored {
        // Broadcast to live subscribers (the receiver applies the recipient
        // gate, so a 1059 reaches only the authed recipient's subscription).
        let _ = state.nostr.relay_tx.send(NostrRelayEvent {
            event_json: raw_json.to_string(),
            author_pubkey: event.pubkey.clone(),
        });

        // The shared S8.9 seal-at-rest seam for a newly-stored gift wrap. It
        // self-gates on the deposited key: on the head it unwraps → D2-seals →
        // the bridged family; on a keyless public box (no deposited key) it
        // no-ops, so the wrap rests opaque and no plaintext-derived DM row is
        // minted (constraint (i)). Only on a *new* store — a duplicate must not
        // re-run the seam and mint a second sealed row.
        if event.kind == 1059 {
            crate::nostr::sync_worker::process_gift_wrap_inbound(
                &state.db,
                &state.nest_identity.signing_key.to_bytes(),
                event,
            )
            .await;
        }
    }

    Ok(ingest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::nostr::db;
    use fauna_bridge_nostr::signing::Keypair;
    use fauna_bridge_nostr::types::{Tag, UnsignedEvent};
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::migrations::run_migrations(&conn).unwrap();
        crate::nostr::apply_schema(&conn).unwrap();
        conn
    }

    fn keypair(seed: u8) -> Keypair {
        Keypair::from_secret_bytes([seed.max(1); 32]).unwrap()
    }

    fn pubkey_hex(kp: &Keypair) -> String {
        hex::encode(kp.public_key_bytes())
    }

    fn signed(kp: &Keypair, kind: u64, created_at: u64, tags: Vec<Tag>, content: &str) -> Event {
        kp.sign_event(UnsignedEvent {
            pubkey: kp.public_key_bytes(),
            created_at,
            kind,
            tags,
            content: content.to_string(),
        })
    }

    /// Insert a store row with a controlled `stored_at`/`origin`/`#p` so the
    /// cursor-ordering tests are deterministic (`store_event` would stamp the
    /// real epoch as `stored_at`, collapsing the ordering axis).
    fn insert_row(
        conn: &Connection,
        id: &str,
        pubkey: &str,
        kind: u64,
        stored_at: i64,
        origin: &str,
        p_tag: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO nostr_events
                 (id, pubkey, kind, created_at, raw_json, derived, stored_at, origin)
             VALUES (?1, ?2, ?3, 0, ?4, 0, ?5, ?6)",
            params![
                id,
                pubkey,
                kind as i64,
                format!("{{\"id\":\"{id}\"}}"),
                stored_at,
                origin
            ],
        )
        .unwrap();
        if let Some(p) = p_tag {
            conn.execute(
                "INSERT INTO nostr_event_tags (event_id, name, value) VALUES (?1, 'p', ?2)",
                params![id, p],
            )
            .unwrap();
        }
    }

    fn ids(rows: &[FederationRow]) -> Vec<String> {
        rows.iter().map(|r| r.id.clone()).collect()
    }

    // ── origin column ──────────────────────────────────────────────

    #[test]
    fn origin_defaults_to_ingest_and_federation_round_trips() {
        let conn = test_conn();
        let kp = keypair(1);

        // Default path stamps ORIGIN_INGEST.
        let a = signed(&kp, 1, 1000, vec![], "ingested");
        store::store_event(&conn, &a, false).unwrap();
        // Explicit federation origin.
        let b = signed(&kp, 1, 1001, vec![], "federated");
        store::store_event_with_origin(&conn, &b, false, store::ORIGIN_FEDERATION).unwrap();

        let origin_of = |id: &str| -> String {
            conn.prepare("SELECT origin FROM nostr_events WHERE id = ?1")
                .unwrap()
                .query_row([id], |r| r.get::<_, String>(0))
                .unwrap()
        };
        assert_eq!(origin_of(&a.id), store::ORIGIN_INGEST);
        assert_eq!(origin_of(&b.id), store::ORIGIN_FEDERATION);
    }

    // ── selection: scope + cursor ordering ─────────────────────────

    #[test]
    fn push_and_pull_select_ingest_scope_only() {
        let conn = test_conn();
        let actor = "aa".repeat(32); // 64-hex actor pubkey
        let other = "bb".repeat(32);

        // In scope, ingest: authored by actor.
        insert_row(&conn, "auth", &actor, 1, 10, store::ORIGIN_INGEST, None);
        // In scope, ingest: 1059 wrap addressed to actor.
        insert_row(
            &conn,
            "wrap",
            &other,
            1059,
            11,
            store::ORIGIN_INGEST,
            Some(&actor),
        );
        // In scope for pull (addressed, non-1059) but NOT for push.
        insert_row(
            &conn,
            "addr",
            &other,
            1,
            12,
            store::ORIGIN_INGEST,
            Some(&actor),
        );
        // Out of scope: another author, not addressed to actor.
        insert_row(&conn, "nope", &other, 1, 13, store::ORIGIN_INGEST, None);
        // Federation-arrived: never re-exported by either leg.
        insert_row(&conn, "fed", &actor, 1, 14, store::ORIGIN_FEDERATION, None);

        let push = list_events_for_push(&conn, &actor, (0, ""), 100).unwrap();
        assert_eq!(ids(&push), vec!["auth", "wrap"]); // no "addr" (non-1059 #p), no "fed"

        let pull = list_events_for_pull(&conn, &actor, (0, ""), 100).unwrap();
        assert_eq!(ids(&pull), vec!["auth", "wrap", "addr"]); // #p unrestricted, no "fed"
    }

    #[test]
    fn cursor_orders_by_stored_at_then_id_strictly_after() {
        let conn = test_conn();
        let actor = "cc".repeat(32);

        // Two rows share stored_at=20 (differing id); one is earlier, one later.
        insert_row(&conn, "id_b", &actor, 1, 20, store::ORIGIN_INGEST, None);
        insert_row(&conn, "id_a", &actor, 1, 20, store::ORIGIN_INGEST, None);
        insert_row(&conn, "id_z", &actor, 1, 10, store::ORIGIN_INGEST, None);
        insert_row(&conn, "id_y", &actor, 1, 30, store::ORIGIN_INGEST, None);

        // Full ascending order: (10,id_z) < (20,id_a) < (20,id_b) < (30,id_y).
        let all = list_events_for_pull(&conn, &actor, (0, ""), 100).unwrap();
        assert_eq!(ids(&all), vec!["id_z", "id_a", "id_b", "id_y"]);

        // Strict-after the (20, "id_a") cursor: the equal-stored_at "id_a" is
        // excluded (not > itself), "id_b" (same stored_at, greater id) is
        // included, and the earlier (10, id_z) stays excluded.
        let after = list_events_for_pull(&conn, &actor, (20, "id_a"), 100).unwrap();
        assert_eq!(ids(&after), vec!["id_b", "id_y"]);

        // Limit truncates the ascending window.
        let limited = list_events_for_pull(&conn, &actor, (0, ""), 2).unwrap();
        assert_eq!(ids(&limited), vec!["id_z", "id_a"]);
    }

    // ── federation cursor accessors (db.rs) ────────────────────────

    #[test]
    fn federation_cursors_upsert_independently() {
        let conn = test_conn();
        assert_eq!(
            db::get_federation_cursors(&conn, "actorX", "peerY").unwrap(),
            None
        );

        db::set_federation_push_cursor(&conn, "actorX", "peerY", 42, "pushid").unwrap();
        let c = db::get_federation_cursors(&conn, "actorX", "peerY")
            .unwrap()
            .unwrap();
        assert_eq!((c.push_stored_at, c.push_id.as_str()), (42, "pushid"));
        // Pull side still at its zero default.
        assert_eq!((c.pull_stored_at, c.pull_id.as_str()), (0, ""));

        // Advancing pull leaves push intact.
        db::set_federation_pull_cursor(&conn, "actorX", "peerY", 7, "pullid").unwrap();
        let c = db::get_federation_cursors(&conn, "actorX", "peerY")
            .unwrap()
            .unwrap();
        assert_eq!((c.push_stored_at, c.push_id.as_str()), (42, "pushid"));
        assert_eq!((c.pull_stored_at, c.pull_id.as_str()), (7, "pullid"));
    }

    // ── ingest_federated_event ─────────────────────────────────────

    async fn build_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::nostr::init_db(&db).await.expect("init_db");
        Arc::new(AppState::for_test(db))
    }

    #[tokio::test]
    async fn ingest_accepts_in_scope_signed_event_as_federation() {
        let state = build_state().await;
        let kp = keypair(2);
        let actor = pubkey_hex(&kp);
        let ev = signed(&kp, 1, 1000, vec![], "hi from the wire");
        let raw = serde_json::to_string(&ev).unwrap();

        let outcome = ingest_federated_event(&state, &actor, &raw).await.unwrap();
        assert_eq!(outcome, IngestOutcome::Stored);

        let conn = state.db.conn().await;
        let origin: String = conn
            .prepare("SELECT origin FROM nostr_events WHERE id = ?1")
            .unwrap()
            .query_row([&ev.id], |r| r.get(0))
            .unwrap();
        assert_eq!(origin, store::ORIGIN_FEDERATION);
    }

    #[tokio::test]
    async fn ingest_rejects_out_of_scope_pubkey() {
        let state = build_state().await;
        let author = keypair(3);
        let actor = pubkey_hex(&keypair(4)); // a different, unaddressed actor
        let ev = signed(&author, 1, 1000, vec![], "not yours");
        let raw = serde_json::to_string(&ev).unwrap();

        let outcome = ingest_federated_event(&state, &actor, &raw).await.unwrap();
        assert!(matches!(outcome, IngestOutcome::Rejected(_)));

        let conn = state.db.conn().await;
        let n: i64 = conn
            .prepare("SELECT COUNT(*) FROM nostr_events")
            .unwrap()
            .query_row([], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn ingest_rejects_bad_signature() {
        let state = build_state().await;
        let kp = keypair(5);
        let actor = pubkey_hex(&kp);
        let mut ev = signed(&kp, 1, 1000, vec![], "tampered");
        // Corrupt the content after signing → signature no longer verifies.
        ev.content = "different".into();
        let raw = serde_json::to_string(&ev).unwrap();

        let outcome = ingest_federated_event(&state, &actor, &raw).await.unwrap();
        assert!(matches!(outcome, IngestOutcome::Rejected(_)));
    }

    #[tokio::test]
    async fn ingest_dedups_by_id() {
        let state = build_state().await;
        let kp = keypair(6);
        let actor = pubkey_hex(&kp);
        let ev = signed(&kp, 1, 1000, vec![], "once");
        let raw = serde_json::to_string(&ev).unwrap();

        assert_eq!(
            ingest_federated_event(&state, &actor, &raw).await.unwrap(),
            IngestOutcome::Stored
        );
        assert_eq!(
            ingest_federated_event(&state, &actor, &raw).await.unwrap(),
            IngestOutcome::Duplicate
        );

        let conn = state.db.conn().await;
        let n: i64 = conn
            .prepare("SELECT COUNT(*) FROM nostr_events")
            .unwrap()
            .query_row([], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn ingest_applies_kind5_deletion() {
        let state = build_state().await;
        let kp = keypair(7);
        let actor = pubkey_hex(&kp);

        // A note, then the author's kind-5 deletion referencing it by e tag.
        let note = signed(&kp, 1, 1000, vec![], "delete me");
        let note_raw = serde_json::to_string(&note).unwrap();
        ingest_federated_event(&state, &actor, &note_raw)
            .await
            .unwrap();

        let deletion = signed(
            &kp,
            5,
            1001,
            vec![Tag::new(vec!["e".into(), note.id.clone()])],
            "",
        );
        let del_raw = serde_json::to_string(&deletion).unwrap();
        ingest_federated_event(&state, &actor, &del_raw)
            .await
            .unwrap();

        let conn = state.db.conn().await;
        let note_present: bool = conn
            .prepare("SELECT EXISTS(SELECT 1 FROM nostr_events WHERE id = ?1)")
            .unwrap()
            .query_row([&note.id], |r| r.get::<_, i64>(0))
            .map(|c| c > 0)
            .unwrap();
        assert!(!note_present, "kind-5 should have removed the note");
    }

    #[tokio::test]
    async fn ingest_stores_1059_wrap_verbatim_keylessly_without_dm_row() {
        let state = build_state().await;
        let recipient = keypair(8);
        let actor = pubkey_hex(&recipient);
        let actor_id = hex::encode([0x21u8; 32]);

        // A keyless *proxied* account: knows the pubkey, holds NO deposited key.
        {
            let conn = state.db.conn().await;
            db::link_account(&conn, &actor_id, &actor, "proxied", None, None, None).unwrap();
        }

        // A gift wrap authored by a random ephemeral key, addressed to the actor.
        let ephemeral = keypair(9);
        let wrap = signed(
            &ephemeral,
            1059,
            1000,
            vec![Tag::new(vec!["p".into(), actor.clone()])],
            "opaque ciphertext",
        );
        let raw = serde_json::to_string(&wrap).unwrap();

        let outcome = ingest_federated_event(&state, &actor, &raw).await.unwrap();
        assert_eq!(outcome, IngestOutcome::Stored);

        let conn = state.db.conn().await;
        // The wrap rests verbatim as a federation row.
        let wrap_origin: String = conn
            .prepare("SELECT origin FROM nostr_events WHERE id = ?1")
            .unwrap()
            .query_row([&wrap.id], |r| r.get(0))
            .unwrap();
        assert_eq!(wrap_origin, store::ORIGIN_FEDERATION);
        // No sealed DM row — the seal seam self-gated on the absent key.
        let dm_rows: i64 = conn
            .prepare("SELECT COUNT(*) FROM bridge_conversation_messages")
            .unwrap()
            .query_row([], |r| r.get(0))
            .unwrap();
        assert_eq!(dm_rows, 0, "keyless box must mint no DM row");
    }
}
