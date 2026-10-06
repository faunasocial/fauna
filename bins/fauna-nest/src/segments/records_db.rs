//! `segment_records` DAO. SQL-side floor mirror for the message-segment
//! store; spec D3 + `docs/goal/architecture/message-segment-store.md`
//! § segment_records SQLite mirror.

use anyhow::{Context, Result};
use fauna_cbor::Cid;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::HashSet;

/// Decode a `record_cid` BLOB column (the full 36-byte `fauna_cbor::Cid`) into a
/// `Cid`. The mirror stores the exact key the CARv2 index uses, so reads never
/// reconstruct it from a bare digest.
fn cid_from_blob(v: Vec<u8>) -> Result<Cid> {
    let arr: [u8; 36] = v
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("record_cid wrong length: {} bytes (want 36)", v.len()))?;
    Cid::from_bytes(arr).map_err(|e| anyhow::anyhow!("record_cid not a valid Cid: {e:?}"))
}

/// One row from `segment_records`.
#[derive(Debug, Clone)]
pub struct SegmentRecordRow {
    pub scope_id: [u8; 32],
    pub kind: String,
    pub segment_id: u32,
    pub record_cid: Cid,
    pub bucket: String,
    pub tombstoned: bool,
    // Mail-kind sparse columns (NULL for other kinds).
    pub received_at: Option<i64>,
    pub sender_dom: Option<String>,
    pub spam_disp: Option<String>,
    pub is_own_submission: Option<bool>,
}

/// Minimal pointer needed to read a record from a segment file — plus the
/// row's `seq`, which a **scoped pre-append dedup hit** must hand back so the
/// caller can answer with the first record's coordinate instead of allocating
/// a second one (`segments::conv::append`; `message-segment-store.md` § Record
/// identity per kind). `None` where the kind writes no seq.
#[derive(Debug, Clone)]
pub struct SegmentRecordRef {
    pub segment_id: u32,
    pub record_cid: Cid,
    pub seq: Option<i64>,
}

/// The next free content-scope feed coordinate for `(scope_id, kind)`.
///
/// The cursor the generalized account-data feed's class-1 arm pages on
/// (`account-sync-plane.md` § Feeds and cursors). Monotonic within the scope and
/// **never reused**: a replica's frontier asserts "every coordinate ≤ this is
/// applied", so handing the same number to two records would make one of them
/// unreachable to every replica already past it.
///
/// Same shape as [`next_mail_seq`] and for the same reason — the writes run
/// under the single `CacheDb` connection mutex, so `MAX + 1` is not a race here
/// — but a *different counter*: `seq` is mail/conv record identity (gapless,
/// assigned once, never touched again), while this is the scope's change
/// ordering (re-assigned whenever a record changes state).
pub fn next_changed_seq(conn: &Connection, scope_id: &[u8; 32], kind: &str) -> Result<i64> {
    let max: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(changed_seq), 0) FROM segment_records
              WHERE scope_id = ?1 AND kind = ?2",
            params![&scope_id[..], kind],
            |r| r.get(0),
        )
        .context("next_changed_seq")?;
    Ok(max + 1)
}

/// One row of a content scope's feed: the record, and whether this coordinate
/// is its arrival or its deletion.
#[derive(Debug, Clone)]
pub struct ContentFeedRow {
    pub changed_seq: i64,
    pub record_cid: Cid,
    pub tombstoned: bool,
    /// The record's own timestamp (`received_at`), carried so the wire row's
    /// `created_at` states a fact rather than a placeholder. Nothing orders on
    /// it — the plane orders on [`Self::changed_seq`], never on a clock.
    pub created_at: i64,
}

/// The segment-store kinds this nest serves on the generalized feed's class-1
/// arm — the door's own answer to the charter's nest-gating ruling ("a nest
/// serves a content scope only for kinds it knows … a refusal means *this nest
/// cannot serve this scope yet*, version skew, not absence",
/// `account-sync-plane.md` § Feeds and cursors → *The scope string* ruling 4).
///
/// `mail` joined 2026-08-11: both bulk purge paths
/// ([`tombstone_mail_up_to_seq`], [`tombstone_conv_up_to_seq`]) now give every
/// row they tombstone its own fresh coordinate via
/// [`tombstone_up_to_seq_with_coordinates`], and mail's scope id is the owning
/// actor — the same own-actor equality every other served kind reduces to —
/// so nothing else was left to build for it.
///
/// `conv` joined 2026-08-13: its coordinates were already stamped on
/// every mutating path (the same purge fix as mail's, plus [`insert_conv`] /
/// [`mark_tombstoned`] / the compaction rebuild), and what it waited on was an
/// **authorization rule for a scope id that names an MLS channel, not an
/// actor**: admission is channel membership, read off the `actor_channels`
/// roster (`crate::sync_handlers::admit_content_scope` — the rule itself is
/// ruled in `account-sync-plane.md` § Implementation status today → *Built —
/// conv on the content-scope feed…*).
///
/// Adding a kind is therefore two things and no design: coordinates on every
/// path that mutates it, and an admission arm for its scope-id family in
/// `admit_content_scope` — which is **fail-closed** (a kind added here without
/// an arm there is refused, never silently served) and pinned by
/// `sync_handlers::tests::every_feed_served_kind_has_a_ruled_admission`, so
/// growing this list without ruling admission reds a test instead of shipping.
pub const FEED_SERVED_KINDS: &[&str] = &["post", "calendar", "card", "mail", "conv"];

/// A content scope's feed rows after `since`, in coordinate order.
///
/// **Live records and tombstones both**, because a tombstone is the event a
/// walking replica most needs: it is the only way a record that already reached
/// the replica ever leaves it. State is read off the mirror rather than stored
/// as a separate event log — the mirror row *is* the current truth, so the feed
/// can never drift from what the nest serves (the same "derive it, don't
/// duplicate it" rule the store's own block presence follows).
///
/// **The feed's unit is the RECORD, not the mirror row.** A row is a
/// `(placement, record)` pair, and placement is explicitly a local detail and
/// never an identity (charter § Store logical schema component 1). Compaction
/// makes the difference load-bearing: it files a record into a fresh segment
/// (a new live row) and tombstones the input segment's row for the *same* CID,
/// so a row-keyed feed would tell a replica that a perfectly live post was
/// deleted. Hence the grouping: a record is live if **any** of its rows is, and
/// its coordinate is the newest across them.
///
/// A `since` of 0 therefore returns the scope's whole current state, one row per
/// record: the class-1 twin of the class-2 arm's zero-frontier reconcile, and
/// not a replay of history — a record that arrived and was later deleted comes
/// back once, as a tombstone.
pub fn content_feed_after(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
    since: i64,
    limit: i64,
) -> Result<Vec<ContentFeedRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT MAX(changed_seq) AS at, record_cid, MIN(tombstoned) AS any_live,
                    COALESCE(MAX(received_at), 0)
               FROM segment_records
              WHERE scope_id = ?1 AND kind = ?2
              GROUP BY record_cid
             HAVING at > ?3
              ORDER BY at ASC LIMIT ?4",
        )
        .context("prepare content_feed_after")?;
    let rows = stmt
        .query_map(params![&scope_id[..], kind, since, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)? != 0,
                r.get::<_, i64>(3)?,
            ))
        })
        .context("query content_feed_after")?;
    let mut out = Vec::new();
    for row in rows {
        let (changed_seq, cid_blob, tombstoned, created_at) =
            row.context("read content feed row")?;
        out.push(ContentFeedRow {
            changed_seq,
            record_cid: cid_from_blob(cid_blob)?,
            tombstoned,
            created_at,
        });
    }
    Ok(out)
}

/// Mail-kind insert helper. Other kinds add their own helpers when
/// their plans land — keeps each kind's column set typed at the
/// boundary instead of forcing every caller to fill an Option<...>
/// jungle.
#[allow(clippy::too_many_arguments)]
pub fn insert_mail(
    conn: &Connection,
    scope_id: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
    bucket: &str,
    received_at: i64,
    sender_domain: &str,
    spam_disposition: &str,
    is_own_submission: bool,
    seq: i64,
    report_hash: Option<&[u8]>,
    continuation_role: u8,
    stored_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO segment_records
            (scope_id, kind, segment_id, record_cid, bucket,
             tombstoned, changed_seq,
             received_at, sender_dom, spam_disp, is_own_submission, seq,
             report_hash, continuation_role, stored_at)
         VALUES (?1, 'mail', ?2, ?3, ?4, 0, ?13, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            &scope_id[..],
            segment_id as i64,
            &cid.as_bytes()[..],
            bucket,
            received_at,
            sender_domain,
            spam_disposition,
            if is_own_submission { 1i64 } else { 0i64 },
            seq,
            report_hash,
            continuation_role as i64,
            // 0 = unknown -> NULL, which the headless-part reaper treats as
            // not-reapable. Never write 0: it would read as "stored at the
            // epoch", i.e. instantly past every grace.
            (stored_at > 0).then_some(stored_at),
            next_changed_seq(conn, scope_id, "mail")?,
        ],
    )
    .context("insert segment_records row (mail)")?;
    Ok(())
}

/// Next per-actor mail `seq` for a scope: `MAX(seq)+1`, or 1 when empty. Mail
/// sibling of [`next_conv_seq`] — same monotonic-counter contract: no
/// `tombstoned` filter (seq must never be reused, so tombstoned rows still hold
/// their slot), and the caller (`segments::mail::append_record`) holds a
/// per-actor seq lock so the query→append→insert chain is atomic per actor.
pub fn next_mail_seq(conn: &Connection, scope_id: &[u8; 32]) -> Result<i64> {
    let seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1
               FROM segment_records
              WHERE scope_id = ?1 AND kind = 'mail'",
            params![&scope_id[..]],
            |r| r.get(0),
        )
        .context("next_mail_seq")?;
    Ok(seq)
}

/// List live mail records for one actor with `seq > after_seq`, oldest first,
/// up to `limit`. Mail sibling of [`list_conv_after_seq`] — the relay's
/// after-cursor reader. Returns `(seq, segment_id, record_cid)`; the caller reads
/// the sealed envelope + floor per row. Tombstoned rows are excluded. Rows with
/// a NULL `seq` (mail written before the relay landed) are excluded by the
/// `seq > ?` comparison (NULL is never `> n`), which is correct: a fresh relay
/// nest starts empty, so no pre-seq rows exist to relay.
pub fn list_mail_after_seq(
    conn: &Connection,
    scope_id: &[u8; 32],
    after_seq: i64,
    limit: i64,
) -> Result<Vec<(i64, u32, Cid)>> {
    let mut stmt = conn
        .prepare(
            "SELECT seq, segment_id, record_cid
               FROM segment_records
              WHERE scope_id = ?1 AND kind = 'mail' AND seq > ?2 AND tombstoned = 0
              ORDER BY seq ASC
              LIMIT ?3",
        )
        .context("prepare list_mail_after_seq")?;
    let rows = stmt
        .query_map(params![&scope_id[..], after_seq, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)? as u32,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .context("query_map list_mail_after_seq")?;
    let mut out = Vec::new();
    for r in rows {
        let (seq, seg, cid_blob) = r.context("row list_mail_after_seq")?;
        out.push((seq, seg, cid_from_blob(cid_blob)?));
    }
    Ok(out)
}

/// Tombstone every `kind` record with `seq <= up_to_seq`, live in one of
/// `scope_ids`, giving each newly-tombstoned row its own fresh content-scope
/// feed coordinate. The bulk sibling of [`mark_tombstoned`]: the same
/// per-row contract — distinct, assigned above that row's own scope's current
/// max, never reused — but many rows (and, for conv, many scopes) in one
/// call instead of one. This is what let `mail` join [`FEED_SERVED_KINDS`]:
/// before this helper, [`tombstone_mail_up_to_seq`] and
/// [`tombstone_conv_up_to_seq`] each ran a single UPDATE that flipped
/// `tombstoned` without touching `changed_seq`, so a purged row's coordinate
/// never moved and a replica already past it would never learn of the delete.
///
/// Materialises `(rowid → coordinate)` into a temp table before writing,
/// exactly like `migrations::backfill_segment_records_changed_seq`: SQLite
/// makes no promise about which version of a row an in-flight correlated
/// subquery reading `MAX(changed_seq)` off the very table the UPDATE writes
/// would see. `ROW_NUMBER() OVER (PARTITION BY scope_id ORDER BY rowid)`
/// keeps each scope's assigned coordinates independent and gapless, matching
/// the per-`(scope_id, kind)` counter [`next_changed_seq`] hands out one row
/// at a time.
///
/// Chunks `scope_ids` at 900 (SQLite's practical bound-param limit), same as
/// [`list_conv_for_scopes_after_seq`]'s multi-channel query — mail's
/// single-scope call is one chunk of one.
fn tombstone_up_to_seq_with_coordinates(
    conn: &Connection,
    scope_ids: &[[u8; 32]],
    kind: &str,
    up_to_seq: i64,
) -> Result<usize> {
    const CHUNK: usize = 900;
    let mut total = 0usize;
    if scope_ids.is_empty() {
        return Ok(0);
    }
    for chunk in scope_ids.chunks(CHUNK) {
        conn.execute("DROP TABLE IF EXISTS _tombstone_seq_batch", [])
            .context("drop stale tombstone-batch temp table")?;

        // Params: [?1 = kind, ?2 = up_to_seq, ?3.. = scope ids].
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 3)).collect();
        let create_sql = format!(
            "CREATE TEMP TABLE _tombstone_seq_batch AS
                SELECT r.rowid AS rid,
                       (SELECT COALESCE(MAX(m.changed_seq), 0) FROM segment_records m
                         WHERE m.scope_id = r.scope_id AND m.kind = r.kind)
                       + ROW_NUMBER() OVER (PARTITION BY r.scope_id ORDER BY r.rowid) AS newseq
                  FROM segment_records r
                 WHERE r.kind = ?1 AND r.seq <= ?2 AND r.tombstoned = 0
                   AND r.scope_id IN ({})",
            placeholders.join(", ")
        );
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> =
            Vec::with_capacity(2 + chunk.len());
        params_vec.push(Box::new(kind.to_string()));
        params_vec.push(Box::new(up_to_seq));
        for s in chunk {
            params_vec.push(Box::new(s.to_vec()));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        conn.execute(&create_sql, param_refs.as_slice())
            .context("materialize tombstone-batch coordinates")?;

        let n = conn
            .execute(
                "UPDATE segment_records
                    SET tombstoned = 1,
                        changed_seq = (SELECT newseq FROM _tombstone_seq_batch
                                        WHERE rid = segment_records.rowid)
                  WHERE rowid IN (SELECT rid FROM _tombstone_seq_batch)",
                [],
            )
            .context("apply tombstone-batch coordinates")?;
        total += n;

        conn.execute("DROP TABLE _tombstone_seq_batch", [])
            .context("drop tombstone-batch temp table")?;
    }
    Ok(total)
}

/// Tombstone every live mail record for `scope_id` with `seq <= up_to_seq`. Mail
/// sibling of [`tombstone_conv_up_to_seq`] — the relay-ack purge step. Returns
/// the number of rows newly tombstoned (already-tombstoned rows are skipped by
/// the `tombstoned = 0` guard, so re-acking the same cursor is idempotent).
pub fn tombstone_mail_up_to_seq(
    conn: &Connection,
    scope_id: &[u8; 32],
    up_to_seq: i64,
) -> Result<usize> {
    tombstone_up_to_seq_with_coordinates(conn, std::slice::from_ref(scope_id), "mail", up_to_seq)
        .context("tombstone_mail_up_to_seq")
}

/// Conv-kind insert helper. Conv has no floor mirror columns (sender lives
/// only in the anti-spam `sender_behavior` table; see
/// `fauna_mls::segments::ConvFloorMetadata`), so `sender_dom` / `spam_disp` /
/// `is_own_submission` are NULL. The conv-specific `seq` (per-channel
/// monotonic counter) is mirrored into the shared `seq` column.
#[allow(clippy::too_many_arguments)]
pub fn insert_conv(
    conn: &Connection,
    scope_id: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
    bucket: &str,
    received_at: i64,
    seq: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO segment_records
            (scope_id, kind, segment_id, record_cid, bucket,
             tombstoned, changed_seq,
             received_at, sender_dom, spam_disp, is_own_submission, seq)
         VALUES (?1, 'conv', ?2, ?3, ?4, 0, ?7, ?5, NULL, NULL, NULL, ?6)",
        params![
            &scope_id[..],
            segment_id as i64,
            &cid.as_bytes()[..],
            bucket,
            received_at,
            seq,
            next_changed_seq(conn, scope_id, "conv")?,
        ],
    )
    .context("insert segment_records row (conv)")?;
    Ok(())
}

/// Record the plaintext attachment references a conv record's sender listed
/// beside its sealed envelope (`conv_attachment_refs`) — the conversation
/// kind's blob-reachability floor (`encryption-at-rest.md` § Per-content-kind
/// conformance → Conversation messages row, 2026-09-08). Called inside the
/// same transaction as [`insert_conv`] by `CacheDb::segment_records_insert_conv`,
/// so a record and its refs land together or not at all. Idempotent per
/// `(scope, seq, hash)`: a byte-replayed send re-lists the same hashes.
pub fn insert_conv_attachment_refs(
    conn: &Connection,
    scope_id: &[u8; 32],
    seq: i64,
    attachment_refs: &[[u8; 32]],
) -> Result<()> {
    if attachment_refs.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO conv_attachment_refs (channel_id, seq, blob_hash)
             VALUES (?1, ?2, ?3)",
        )
        .context("prepare insert conv_attachment_refs")?;
    for hash in attachment_refs {
        stmt.execute(params![&scope_id[..], seq, &hash[..]])
            .context("insert conv_attachment_refs row")?;
    }
    Ok(())
}

/// Record the actor this nest authenticated as the poster of one conv record
/// (`conv_record_authors`; `caldav-server.md` § Who may mutate an existing
/// event over the inbound rail). Called inside the same transaction as
/// [`insert_conv`], so a record and its attestation land together or not at
/// all. `INSERT OR IGNORE`: `(channel, seq)` is written once and an attestation
/// is never rewritten.
pub fn insert_conv_record_author(
    conn: &Connection,
    scope_id: &[u8; 32],
    seq: i64,
    author: &[u8; 32],
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO conv_record_authors (channel_id, seq, author)
         VALUES (?1, ?2, ?3)",
        params![&scope_id[..], seq, &author[..]],
    )
    .context("insert conv_record_authors row")?;
    Ok(())
}

/// The attested authors of one channel's records in `(after_seq, up_to_seq]`,
/// keyed by `seq` — the serve-side read both `channel.fetch` surfaces join onto
/// a page. A `seq` with no row (a record appended before v70, or by a path that
/// authenticated no actor) is simply absent: the wire field stays unset and the
/// client reads it as *no answer*.
pub fn conv_record_authors_in_range(
    conn: &Connection,
    scope_id: &[u8; 32],
    after_seq: i64,
    up_to_seq: i64,
) -> Result<std::collections::HashMap<i64, [u8; 32]>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT seq, author FROM conv_record_authors
              WHERE channel_id = ?1 AND seq > ?2 AND seq <= ?3",
        )
        .context("prepare conv_record_authors_in_range")?;
    let rows = stmt
        .query_map(params![&scope_id[..], after_seq, up_to_seq], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .context("query conv_record_authors_in_range")?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (seq, v) = row.context("read conv_record_authors row")?;
        if let Ok(arr) = <[u8; 32]>::try_from(v.as_slice()) {
            out.insert(seq, arr);
        }
    }
    Ok(out)
}

/// Every attachment hash a LIVE conv record names — the blob GC's step 2g
/// source (`backup-restore.md` § 9 step 2). Liveness is the mirror's own: a
/// reference whose `(channel, seq)` has no non-tombstoned `segment_records`
/// row points at a record the store has already let go (relay-acked and
/// purged, or compacted away), so its blobs reclaim with it. Reference rows
/// are never deleted here — over-retaining a row is free, while a row dropped
/// on a transient inconsistency is the over-delete direction.
pub fn list_live_conv_attachment_refs(conn: &Connection) -> Result<Vec<[u8; 32]>> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT r.blob_hash
               FROM conv_attachment_refs r
              WHERE EXISTS (SELECT 1 FROM segment_records s
                             WHERE s.scope_id = r.channel_id
                               AND s.kind = 'conv'
                               AND s.seq = r.seq
                               AND s.tombstoned = 0)",
        )
        .context("prepare list_live_conv_attachment_refs")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, Vec<u8>>(0))
        .context("query list_live_conv_attachment_refs")?;
    let mut out = Vec::new();
    for row in rows {
        let v = row.context("read conv_attachment_refs.blob_hash")?;
        if let Ok(arr) = <[u8; 32]>::try_from(v.as_slice()) {
            out.push(arr);
        }
    }
    Ok(out)
}

/// The attachment hashes recorded for one `(channel, seq)`, sorted by hash —
/// an audit / test read, never a serve surface (the nest hands these to no
/// client).
pub fn conv_attachment_refs_for(
    conn: &Connection,
    scope_id: &[u8; 32],
    seq: i64,
) -> Result<Vec<[u8; 32]>> {
    let mut stmt = conn
        .prepare(
            "SELECT blob_hash FROM conv_attachment_refs
              WHERE channel_id = ?1 AND seq = ?2
              ORDER BY blob_hash",
        )
        .context("prepare conv_attachment_refs_for")?;
    let rows = stmt
        .query_map(params![&scope_id[..], seq], |r| r.get::<_, Vec<u8>>(0))
        .context("query conv_attachment_refs_for")?;
    let mut out = Vec::new();
    for row in rows {
        let v = row.context("read conv_attachment_refs.blob_hash")?;
        if let Ok(arr) = <[u8; 32]>::try_from(v.as_slice()) {
            out.push(arr);
        }
    }
    Ok(out)
}

/// Next per-channel conv `seq` for a scope: `MAX(seq)+1`, or 1 when empty.
/// No `tombstoned` filter — `seq` is a monotonic per-channel counter and MUST
/// never be reused, so tombstoned rows still hold their slot.
pub fn next_conv_seq(conn: &Connection, scope_id: &[u8; 32]) -> Result<i64> {
    let seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1
               FROM segment_records
              WHERE scope_id = ?1 AND kind = 'conv'",
            params![&scope_id[..]],
            |r| r.get(0),
        )
        .context("next_conv_seq")?;
    Ok(seq)
}

/// List live conv records for one channel with `seq > after_seq`, oldest
/// first, up to `limit`. Returns `(seq, segment_id, record_cid,
/// legal_takedown_ref)` tuples; the caller reads the envelope bytes per row
/// (and **withholds** it when `legal_takedown_ref` is `Some` — the legal-
/// obligation relay-withhold gate). Tombstoned rows are excluded.
pub fn list_conv_after_seq(
    conn: &Connection,
    scope_id: &[u8; 32],
    after_seq: i64,
    limit: i64,
) -> Result<Vec<(i64, u32, Cid, Option<String>)>> {
    let mut stmt = conn
        .prepare(
            "SELECT seq, segment_id, record_cid, legal_takedown_ref
               FROM segment_records
              WHERE scope_id = ?1 AND kind = 'conv' AND seq > ?2 AND tombstoned = 0
              ORDER BY seq ASC
              LIMIT ?3",
        )
        .context("prepare list_conv_after_seq")?;
    let rows = stmt
        .query_map(params![&scope_id[..], after_seq, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)? as u32,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .context("query_map list_conv_after_seq")?;
    let mut out = Vec::new();
    for r in rows {
        let (seq, seg, cid_blob, legal_ref) = r.context("row list_conv_after_seq")?;
        out.push((seq, seg, cid_from_blob(cid_blob)?, legal_ref));
    }
    Ok(out)
}

/// Set (or clear) the legal-obligation takedown reference on a single conv
/// record, keyed by its `record_cid`. `Some(reference)` takes it down;
/// `None` overturns/restores. Returns the number of rows updated — `0` means no
/// such conv record (the caller maps that to `not_found`). The conv-kind guard
/// keeps this from ever touching a mail/filesync record that shares the
/// (globally-unique-per-message) `record_cid` space. The `record_cid` index
/// makes the lookup a point read. Mirror-only mutable flag, exactly like
/// `tombstoned` (`message-segment-store.md` § segment_records SQLite mirror).
pub fn set_conv_legal_takedown(
    conn: &Connection,
    record_cid: &Cid,
    reference: Option<&str>,
) -> Result<usize> {
    let n = conn
        .execute(
            "UPDATE segment_records
                SET legal_takedown_ref = ?1
              WHERE kind = 'conv' AND record_cid = ?2",
            params![reference, &record_cid.as_bytes()[..]],
        )
        .context("set_conv_legal_takedown")?;
    Ok(n)
}

/// Look up a conv record by `record_cid`, returning `(scope_id,
/// legal_takedown_ref)` — `scope_id` is the channel the message lives in
/// (audit context) and the ref is its current takedown state. `None` = no such
/// conv record (the handler maps that to `not_found`). Ignores `tombstoned`
/// (a user-deleted message can still be legally taken down / its takedown
/// overturned; the row survives per the tombstone-not-delete rule).
pub fn conv_record_scope_and_takedown(
    conn: &Connection,
    record_cid: &Cid,
) -> Result<Option<([u8; 32], Option<String>)>> {
    conn.query_row(
        "SELECT scope_id, legal_takedown_ref
           FROM segment_records
          WHERE kind = 'conv' AND record_cid = ?1
          LIMIT 1",
        params![&record_cid.as_bytes()[..]],
        |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Option<String>>(1)?)),
    )
    .optional()
    .context("conv_record_scope_and_takedown")?
    .map(|(scope_v, legal_ref)| {
        let scope: [u8; 32] = crate::db::blob_to_array(scope_v.as_slice(), "scope_id")?;
        Ok((scope, legal_ref))
    })
    .transpose()
}

/// List live conv records across many channels with `seq > after_seq`, oldest
/// first, up to `limit` total. Returns `(seq, scope_id, segment_id,
/// record_cid, legal_takedown_ref)`. The `scope_ids` `IN (...)` list is chunked (≤ ~900 params)
/// like [`live_set_for_segments`]; multi-chunk results are merged, re-sorted
/// by `seq`, then truncated to `limit`. Empty `scope_ids` → empty Vec.
pub fn list_conv_for_scopes_after_seq(
    conn: &Connection,
    scope_ids: &[[u8; 32]],
    after_seq: i64,
    limit: i64,
) -> Result<Vec<(i64, [u8; 32], u32, Cid, Option<String>)>> {
    const CHUNK: usize = 900; // leaves headroom for the after_seq + limit params
    let mut out: Vec<(i64, [u8; 32], u32, Cid, Option<String>)> = Vec::new();
    if scope_ids.is_empty() {
        return Ok(out);
    }
    for chunk in scope_ids.chunks(CHUNK) {
        // Params: [?1 = after_seq, ?2 = limit, ?3.. = scope ids].
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 3)).collect();
        let sql = format!(
            "SELECT seq, scope_id, segment_id, record_cid, legal_takedown_ref
               FROM segment_records
              WHERE kind = 'conv' AND seq > ?1 AND tombstoned = 0
                AND scope_id IN ({})
              ORDER BY seq ASC
              LIMIT ?2",
            placeholders.join(", ")
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare list_conv_for_scopes_after_seq")?;
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> =
            Vec::with_capacity(2 + chunk.len());
        params_vec.push(Box::new(after_seq));
        params_vec.push(Box::new(limit));
        for s in chunk {
            params_vec.push(Box::new(s.to_vec()));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, i64>(2)? as u32,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .context("query_map list_conv_for_scopes_after_seq")?;
        for r in rows {
            let (seq, scope_v, seg, cid_blob, legal_ref) =
                r.context("row list_conv_for_scopes_after_seq")?;
            let scope: [u8; 32] = crate::db::blob_to_array(scope_v.as_slice(), "scope_id")?;
            out.push((seq, scope, seg, cid_from_blob(cid_blob)?, legal_ref));
        }
    }
    // Merge across chunks: global oldest-first order, then cap at `limit`.
    out.sort_by_key(|(seq, _, _, _, _)| *seq);
    if limit >= 0 {
        out.truncate(limit as usize);
    }
    Ok(out)
}

/// Tombstone every live conv record with `seq <= up_to_seq` across the
/// supplied channels. Returns the total rows newly tombstoned (summed across
/// chunks). Empty `scope_ids` → 0.
pub fn tombstone_conv_up_to_seq(
    conn: &Connection,
    scope_ids: &[[u8; 32]],
    up_to_seq: i64,
) -> Result<usize> {
    tombstone_up_to_seq_with_coordinates(conn, scope_ids, "conv", up_to_seq)
        .context("tombstone_conv_up_to_seq")
}

// NOTE: `lookup_actor_for_record` (scope-agnostic `(kind, cid) -> actor`,
// LIMIT 1) is RETIRED with the record-identity cutover: the actor no longer
// lives inside the record id, so one cid can exist in two scopes (a byte
// replay) and "the first match" is an arbitrary owner. Scope-checks use
// [`lookup_record`] below.

/// Look up the SegmentRecordRef for one `(scope, kind, record_cid)`.
/// Used by single-record fetch paths (FETCH BODY, full-row diagnostics).
pub fn lookup_record(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
    cid: &Cid,
) -> Result<Option<SegmentRecordRef>> {
    let row = conn
        .query_row(
            "SELECT segment_id, seq
               FROM segment_records
              WHERE scope_id = ?1 AND kind = ?2 AND record_cid = ?3 AND tombstoned = 0",
            params![&scope_id[..], kind, &cid.as_bytes()[..]],
            |r| {
                Ok(SegmentRecordRef {
                    segment_id: r.get::<_, i64>(0)? as u32,
                    record_cid: *cid,
                    seq: r.get(1)?,
                })
            },
        )
        .optional()
        .context("lookup_record")?;
    Ok(row)
}

/// Does `(scope_id, kind)` hold **any** mirror row for `cid` — live **or
/// tombstoned**?
///
/// [`lookup_record`] answers "is it live", which is the append dedup's
/// question. This one answers the lived-in recovery's: a record the target
/// has EVER filed is the target's history, and its history wins — a message
/// the owner deleted after a rollback is tombstoned here, and recovering it
/// from a backup that still holds it would resurrect a deletion
/// (`segment-backup-protocol.md` § Client-device custodian (pull) → *Restore*
/// → *Recovery into the lived-in nest that regressed*, the recovery set).
pub fn held_ever(conn: &Connection, scope_id: &[u8; 32], kind: &str, cid: &Cid) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM segment_records
                        WHERE scope_id = ?1 AND kind = ?2 AND record_cid = ?3)",
        params![&scope_id[..], kind, &cid.as_bytes()[..]],
        |r| r.get::<_, bool>(0),
    )
    .context("held_ever")
}

/// Count tombstoned rows for one segment. Used by Plan 5's
/// `fauna.segments.list` reply to populate `SegmentRef.tombstone_count`.
pub fn count_tombstoned_for_segment(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
    segment_id: u32,
) -> Result<i64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM segment_records \
             WHERE scope_id = ?1 AND kind = ?2 AND segment_id = ?3 AND tombstoned = 1",
            params![&scope_id[..], kind, segment_id as i64],
            |r| r.get(0),
        )
        .context("count_tombstoned_for_segment")?;
    Ok(count)
}

/// Mark a single segment_records row tombstoned. Idempotent — re-tombstoning
/// a row is a no-op (only rows with `tombstoned = 0` are updated). Returns
/// the number of rows updated (0 or 1).
///
/// The tombstone **moves the record to a new feed coordinate**
/// ([`next_changed_seq`]). A delete that left the coordinate alone would be
/// invisible to every replica whose frontier had already passed the record —
/// it would keep serving a post the author deleted, and nothing would ever tell
/// it otherwise. The idempotence guard is what keeps a re-tombstone from
/// churning the coordinate and re-delivering the same news.
pub fn mark_tombstoned(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
    segment_id: u32,
    cid: &Cid,
) -> Result<usize> {
    let next = next_changed_seq(conn, scope_id, kind)?;
    let n = conn
        .execute(
            "UPDATE segment_records
             SET tombstoned = 1, changed_seq = ?5
             WHERE scope_id = ?1 AND kind = ?2 AND segment_id = ?3 AND record_cid = ?4
               AND tombstoned = 0",
            params![
                &scope_id[..],
                kind,
                segment_id as i64,
                &cid.as_bytes()[..],
                next
            ],
        )
        .context("UPDATE segment_records SET tombstoned=1")?;
    Ok(n)
}

/// Tombstone one content record addressed by CID alone, resolving its segment
/// through the mirror. Idempotent; returns rows updated (0 when the record is
/// absent or already tombstoned — e.g. an inline row whose body rests in its
/// SQLite column and has no segment record at all).
///
/// Plain SQL over a caller-held `&Connection`, so a DAO can tombstone a record
/// atomically with the `DELETE` of the row that referenced it, without ever
/// taking a `SegmentManager` (`message-segment-store.md:355`).
///
/// **Ordering rule for callers: DELETE the row first, then tombstone.** A
/// tombstoned record is eligible for physical reclaim by compaction, so
/// tombstoning while a live row still points at the record is user-irrecoverable
/// data loss. The reverse window merely leaks an unreachable record.
pub fn tombstone_by_cid(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
    cid: &Cid,
) -> Result<usize> {
    match lookup_record(conn, scope_id, kind, cid)? {
        Some(rec) => mark_tombstoned(conn, scope_id, kind, rec.segment_id, cid),
        None => Ok(0),
    }
}

/// Minimum age, in seconds, a live mirror row must reach before
/// [`reap_orphan_point_read_records`] will consider it an orphan.
///
/// Not a tunable: the only human-relevant effect of raising or lowering it is
/// how long a rejected PUT's unreachable record occupies disk, which no user or
/// admin would ever want to choose (product invariant — a value nobody chooses
/// is a Rust constant, never a config knob).
///
/// It exists to keep the reaper off **in-flight writes**. A PUT appends its
/// content record (mirror row committed) and only then inserts the metadata row,
/// so for a few milliseconds a perfectly healthy record looks exactly like an
/// orphan. Reaping it there would make the DAO's empty-body guard refuse the
/// write — recoverable (the client retries and re-appends), but a spurious
/// failure. One hour is ~6 orders of magnitude above the real window.
pub const ORPHAN_REAP_MIN_AGE_SECS: i64 = 3600;

/// Tombstone every **orphaned** live mirror row for a point-read kind
/// (`calendar` / `card`): a record whose `segment_records` row is live but whose
/// owning metadata row (`bridge_caldav_events` / `bridge_carddav_cards`) no
/// longer exists. Returns the number of records tombstoned.
///
/// Orphans arise two ways, both of them unreachable-by-construction (every read
/// starts from a metadata row) and neither data loss:
///
/// 1. **A rejected PUT.** `put_{event,card}_ciphertext_handler` appends the
///    content record *before* the DAO can answer `PreconditionFailed` /
///    `CalendarMissing` — an ordering that is itself load-bearing (the row must
///    never exist without its body). A rejected write leaves the record behind.
/// 2. **A crash** between the append and the row INSERT.
///
/// Until they are tombstoned these records are *live* to compaction, which
/// faithfully copies them into every rewritten segment forever.
///
/// # Safety — why this cannot lose a body
///
/// The inverse of the S6.8a ordering rule ("DELETE the row, then tombstone")
/// is that a record a live row still points at must **never** be tombstoned: a
/// tombstoned record is eligible for physical reclaim, so that would be
/// user-irrecoverable loss. Four defenses, all required:
///
/// - **`live_cids` and `kind` must name the same segment.** Both are unrepresentable
///   as an independently-chosen pair — the only caller,
///   [`super::reap_orphan_records`], assembles them from one
///   [`super::SegmentKind`] value, never two separate strings
///   (). This is not the same failure as the
///   empty-live-set guard below covers: a crossed pair (calendar's live set
///   against card's `kind`) is *non-empty and wrong*, not empty — it walks
///   past every other defense here, since the age watermark and the critical
///   section are both about the set's freshness, not its subject.
/// - **One critical section.** `live_cids` MUST be read from `conn` by the
///   caller, and this function tombstones on that same held connection lock. A
///   PUT's row INSERT needs the same lock, so it cannot interleave between the
///   caller's read and our write. Reading `live_cids` from a *separate*
///   acquisition would reintroduce exactly the loss this guards: snapshot the
///   live set, let a PUT commit its row, then tombstone the record it points at.
/// - **An age watermark** ([`ORPHAN_REAP_MIN_AGE_SECS`]) — see above.
/// - **Fail closed on an empty live set.** An actor with zero metadata rows but
///   live mirror rows is not a pile of orphans; it is a nest whose rows have not
///   been rebuilt yet — a pure-backup destination, or a restore caught between
///   its mirror rebuild and its row rebuild. Reaping there would tombstone the
///   actor's *entire* corpus. We skip and warn. The cost of being wrong in this
///   direction is leaked disk; the cost of being wrong in the other is the user's
///   calendar. (An actor who genuinely deleted every event has no live mirror
///   rows either — S6.8a tombstoned them at DELETE — so this costs a real nest
///   nothing.)
///
/// `pub(super)` only: reachable exclusively through
/// [`super::reap_orphan_records`], which assembles `kind` from a
/// [`super::SegmentKind`] rather than accepting one as an independent
/// argument. A `pub` visibility would let a crate-internal caller elsewhere
/// hand-pick `kind` again, reopening the first defense above by a different
/// door ().
pub(super) fn reap_orphan_point_read_records(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
    now: i64,
    live_cids: &HashSet<Cid>,
) -> Result<u32> {
    let cutoff = now - ORPHAN_REAP_MIN_AGE_SECS;
    let candidates = list_live_record_cids_older_than(conn, scope_id, kind, cutoff)?;
    if candidates.is_empty() {
        return Ok(0);
    }
    if live_cids.is_empty() {
        tracing::warn!(
            "orphan reap: skipping scope=0x{} kind={kind} — {} live mirror row(s) but zero \
             metadata rows; refusing to tombstone a corpus that may simply not be rebuilt yet",
            hex::encode(scope_id),
            candidates.len(),
        );
        return Ok(0);
    }
    let mut reaped = 0u32;
    for cid in candidates {
        if live_cids.contains(&cid) {
            continue;
        }
        reaped += tombstone_by_cid(conn, scope_id, kind, &cid)? as u32;
    }
    if reaped > 0 {
        tracing::info!(
            "orphan reap: tombstoned {reaped} unreachable {kind} record(s) for scope=0x{}",
            hex::encode(scope_id),
        );
    }
    Ok(reaped)
}

/// Live (non-tombstoned) record CIDs for `(scope, kind)` whose `received_at` is
/// strictly older than `cutoff`. `received_at` carries the record's own
/// timestamp — for `calendar`/`card` that is `created_at` in epoch **seconds**
/// (mail's is milliseconds; do not share a cutoff across the two).
fn list_live_record_cids_older_than(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
    cutoff: i64,
) -> Result<Vec<Cid>> {
    let mut stmt = conn
        .prepare(
            "SELECT record_cid FROM segment_records
              WHERE scope_id = ?1 AND kind = ?2 AND tombstoned = 0
                AND received_at IS NOT NULL AND received_at < ?3",
        )
        .context("prepare list_live_record_cids_older_than")?;
    let rows = stmt
        .query_map(params![&scope_id[..], kind, cutoff], |r| {
            r.get::<_, Vec<u8>>(0)
        })
        .context("query list_live_record_cids_older_than")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(cid_from_blob(row.context("read record_cid")?)?);
    }
    Ok(out)
}

/// Live (non-tombstoned) continuation **HEAD** records for `scope` —
/// `(segment_id, record_cid)` for each — so the headless-part reaper can decode
/// them and learn which parts are still referenced. Heads are rare (only
/// over-cap messages), so decoding them all is cheap.
/// (`message-segment-store.md` § Continuation records.)
pub fn list_live_continuation_heads(
    conn: &Connection,
    scope_id: &[u8; 32],
) -> Result<Vec<(u32, Cid)>> {
    let mut stmt = conn
        .prepare(
            "SELECT segment_id, record_cid FROM segment_records
              WHERE scope_id = ?1 AND kind = 'mail' AND tombstoned = 0
                AND continuation_role = 2",
        )
        .context("prepare list_live_continuation_heads")?;
    let rows = stmt
        .query_map(params![&scope_id[..]], |r| {
            Ok((r.get::<_, i64>(0)? as u32, r.get::<_, Vec<u8>>(1)?))
        })
        .context("query list_live_continuation_heads")?;
    let mut out = Vec::new();
    for row in rows {
        let (seg, blob) = row.context("read head row")?;
        out.push((seg, cid_from_blob(blob)?));
    }
    Ok(out)
}

/// Tombstone every **headless** continuation PART for `scope`: a live
/// (`continuation_role = 1`, non-tombstoned) part record, older than the age
/// watermark, whose CID is **not** in `live_part_cids` (the set of parts still
/// referenced by a live head). Returns the number tombstoned; compaction
/// physically reclaims them. The mail sibling of
/// [`reap_orphan_point_read_records`] — parts share the `mail` kind with normal
/// records + heads, so this filters on `continuation_role` rather than reusing
/// the kind-generic reaper.
///
/// # Safety — why this cannot lose a live body
///
/// A part is reaped only when it is (a) **aged** past [`ORPHAN_REAP_MIN_AGE_SECS`],
/// (b) referenced by **no** live head, and (c) **outranked by a live head at a
/// higher `seq`** — positive proof that its own head is never coming. Three facts
/// make each direction safe:
///
/// - **The age watermark covers the write race.** A continuation write appends
///   its parts (stored_at ≈ now) seconds before the head, so a freshly-written
///   family's parts are far younger than the watermark and are never candidates
///   — even if the reaper's live-head snapshot predates the head. This is the
///   mail analogue of the calendar/card single-critical-section: parts, like a
///   PUT's content record, look briefly orphaned but are shielded by age.
///
///   **The watermark keys on `stored_at`, never on `received_at`** — the
///   distinction is load-bearing, not stylistic. `received_at` is the *message's*
///   receive time and the relay forwards it verbatim, so for relayed historical
///   mail (a backfill/catch-up relay) it is days old *on arrival*: a part would
///   be born already past the grace, and a compaction pass landing between a
///   family's parts and its head would tombstone parts that are still needed
///   while the source has already purged on ack — user-irrecoverable body loss.
///   `stored_at` is assigned by *this* nest at append and overwritten on the
///   relay path (like `seq`), so "aged" always means "has been here a while",
///   which is the only thing the race argument above actually needs. A NULL
///   `stored_at` (a failed append-time clock read, or an unknown `0` floor) is **not** reapable:
///   erring toward a leak, never toward loss.
/// - **Delete fans out atomically.** Tombstoning a head tombstones its parts in
///   the same transaction, so a concurrently-deleted family's parts are already
///   `tombstoned` (never candidates). A part becomes a genuine candidate only
///   after its head is truly gone (crash between parts and head, or a
///   fanned-out delete that somehow missed a part) — exactly what should be
///   reclaimed.
/// - **A head at a higher `seq` is the only proof the head is not merely LATE.**
///   Age cannot tell "orphaned by a crashed write" from "awaiting its head from
///   the relay": a family *always* spans relay pull pages (parts are 1 MiB, the
///   page budget is one 2 MiB frame minus headroom), so the destination routinely
///   holds a headless part-run whose head is in the next page — and the relay acks
///   mid-family, so the source has already purged the parts it handed over. Reaping
///   such a run on age alone loses the body user-irrecoverably once the stall
///   outlasts the grace (the head lands later and its join finds nothing to
///   rejoin). Parts and their head take consecutive seqs under **one** lock hold
///   (`segments::mail::append_continuation_record`) and the relay never skips a
///   record, so a live head *above* a part-run proves the run's own head was never
///   allocated a seq — i.e. its write crashed and it is a true orphan. Anything
///   else above it (a *normal* record, e.g. a local append interleaving into a
///   relayed family — relayed records take no family-spanning lock) proves
///   nothing, so it does not license a reap.
///
///   ⚠ **The head-above proof is provenance-blind — it is sound today by a
///   TOPOLOGY invariant, not by this code**. A
///   *relayed* awaiting run's head was allocated on the **source**, so a
///   *locally-written* continuation family landing above that run on the
///   destination satisfies the `EXISTS` and would falsely license the reap —
///   the same body loss this guard exists to prevent. Unreachable today
///   because the only production caller of the gated family writer is MTA
///   ingest and the paired deployment keeps the MTA on the public box: **no
///   nest both relay-pulls an actor's mail and locally ingests mail for that
///   same actor** (also stated in `deployment-home-with-public-relay.md`
///   § Relay frame budget and `message-segment-store.md` § Continuation
///   records). Any change that breaks that topology — a nest-migration flow
///   reusing `mail_pull` beside a live MTA, or enabling a home-box MTA /
///   LAN-SMTP — must revisit this reaper FIRST. The structural fix, when a
///   schema touch warrants it: an additive `relayed` provenance flag on
///   mirror rows stamped by `append_sealed_record`, with per-provenance
///   proofs (a relayed run is orphaned once any *relayed* record sits above
///   it — the relay is in-order and never skips; a local run once headless
///   outside the family lock).
///
///   The fail direction is a **leak, never a loss**, as everywhere else here: an
///   orphan from a crashed family write is retained until that actor's next
///   over-cap message writes a head above it. That is bounded, quota-accounted,
///   and reclaimed as soon as the proof appears.
///
///   Why this shape and not the ack-side one. The review that raised this
///   proposed instead holding the relay's `ack_through` at the last complete-
///   family boundary — but at a mail relay the ack value *is* the destination's
///   next pull cursor (`nest_sync_worker::relay_actor_mail` returns `ack_through`,
///   the caller feeds it back as `since_seq`), so holding the ack also freezes the
///   cursor and the head's page is never requested: the relay livelocks. Fixing
///   the reaper instead of the ack also dissolves both cautions that shape
///   carried — there is **no re-append heal path** (the parts are simply never
///   reaped while the head is in flight, so no duplicate-CID collision on
///   re-append can arise) and **no held ack** (a source-side crash-orphaned run
///   relays, acks, and purges normally, then leaks on the destination under this
///   same rule until a head-above reclaims it — the ack never stalls).
///
/// `now_ms` is **epoch milliseconds** (mail's floor-timestamp unit — unlike
/// calendar/card seconds), so the cutoff subtracts `ORPHAN_REAP_MIN_AGE_SECS *
/// 1000`.
pub fn reap_headless_parts(
    conn: &Connection,
    scope_id: &[u8; 32],
    now_ms: i64,
    live_part_cids: &HashSet<Cid>,
) -> Result<u32> {
    let cutoff_ms = now_ms - ORPHAN_REAP_MIN_AGE_SECS * 1000;
    let mut stmt = conn
        .prepare(
            // `stored_at`, NOT `received_at` — see the safety argument above; a
            // NULL (unknown) stored_at is excluded by the IS NOT NULL, so it is
            // never reaped.
            //
            // The EXISTS is the family-awareness guard: reap only a part that a
            // live head at a HIGHER seq proves orphaned. Without it, a headless
            // part-run still awaiting its head from the relay is reaped once a
            // stall outlasts the grace — and the source purged on the mid-family
            // ack, so the body is gone. A NULL seq on either side makes `>` NULL
            // (never true), so the part is spared: the leak-never-lose direction.
            "SELECT p.segment_id, p.record_cid FROM segment_records AS p
              WHERE p.scope_id = ?1 AND p.kind = 'mail' AND p.tombstoned = 0
                AND p.continuation_role = 1
                AND p.stored_at IS NOT NULL AND p.stored_at < ?2
                AND EXISTS (
                    SELECT 1 FROM segment_records AS h
                     WHERE h.scope_id = p.scope_id AND h.kind = 'mail'
                       AND h.tombstoned = 0 AND h.continuation_role = 2
                       AND h.seq > p.seq
                )",
        )
        .context("prepare reap_headless_parts candidates")?;
    let rows = stmt
        .query_map(params![&scope_id[..], cutoff_ms], |r| {
            Ok((r.get::<_, i64>(0)? as u32, r.get::<_, Vec<u8>>(1)?))
        })
        .context("query reap_headless_parts candidates")?;
    let candidates: Vec<(u32, Cid)> = rows
        .map(|row| {
            let (seg, blob) = row.context("read candidate part row")?;
            Ok((seg, cid_from_blob(blob)?))
        })
        .collect::<Result<_>>()?;

    let mut reaped = 0u32;
    for (segment_id, cid) in candidates {
        if live_part_cids.contains(&cid) {
            continue;
        }
        reaped += mark_tombstoned(conn, scope_id, "mail", segment_id, &cid)? as u32;
    }
    if reaped > 0 {
        tracing::info!(
            "headless-part reap: tombstoned {reaped} orphaned continuation part(s) for \
             scope=0x{}",
            hex::encode(scope_id),
        );
    }
    Ok(reaped)
}

/// Per-segment stats for compaction input selection. One row per
/// `(scope_id, kind, segment_id)` within the requested bucket. Used by
/// the Plan 3 compaction worker.
///
/// Cross-reference: `fauna_segment_store::SegmentStats` — the
/// compaction-crate input shape that drives
/// [`fauna_segment_store::pick_compaction_inputs`]. `SegmentStats` has
/// only `segment_id`, `record_count`, and `tombstone_count`, exactly this
/// row's fields (the per-segment byte rollup is gone with the `byte_length`
/// column — disk-reclaim sizing reads the CARv2 index by `record_cid`).
#[derive(Debug, Clone)]
pub struct SegmentStatsRow {
    pub segment_id: u32,
    pub record_count: u32,
    pub tombstone_count: u32,
}

/// Per-segment record/tombstone rollup for a given `(scope, kind, bucket)`.
/// Results are ordered by `segment_id ASC`.
/// How many **live** (non-tombstoned) records a `(scope, kind)` holds, across
/// every bucket and segment.
///
/// The empty-target predicate for `fauna.backup.custody.materialize`
/// (`backup-destinations.md` § Third destination kind -> *Re-seed*, phase 3):
/// "a scope or folder already holding live records refuses". Tombstoned rows
/// deliberately do not count — a scope whose every record has been deleted is
/// empty in the only sense the rule is about (there is nothing left to merge
/// into or overwrite), while its rows survive because `seq` must never be
/// reused.
pub fn count_live_records(conn: &Connection, scope_id: &[u8; 32], kind: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM segment_records \
         WHERE scope_id = ?1 AND kind = ?2 AND tombstoned = 0",
        params![&scope_id[..], kind],
        |row| row.get(0),
    )
    .context("count_live_records")
}

pub fn count_segment_stats(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
    bucket: &str,
) -> Result<Vec<SegmentStatsRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT segment_id,
                    COUNT(*) AS record_count,
                    SUM(tombstoned) AS tombstone_count
             FROM segment_records
             WHERE scope_id = ?1 AND kind = ?2 AND bucket = ?3
             GROUP BY segment_id
             ORDER BY segment_id ASC",
        )
        .context("prepare count_segment_stats")?;
    let rows = stmt
        .query_map(params![&scope_id[..], kind, bucket], |row| {
            Ok(SegmentStatsRow {
                segment_id: row.get::<_, i64>(0)? as u32,
                record_count: row.get::<_, i64>(1)? as u32,
                tombstone_count: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u32,
            })
        })
        .context("query_map count_segment_stats")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("row count_segment_stats")?);
    }
    Ok(out)
}

/// List the distinct buckets containing at least one tombstoned record for
/// `(scope, kind)`. Compaction iterates these; empty result → no work.
pub fn list_buckets_with_tombstones(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT bucket
             FROM segment_records
             WHERE scope_id = ?1 AND kind = ?2 AND tombstoned = 1
             ORDER BY bucket ASC",
        )
        .context("prepare list_buckets_with_tombstones")?;
    let rows = stmt
        .query_map(params![&scope_id[..], kind], |r| r.get::<_, String>(0))
        .context("query_map list_buckets_with_tombstones")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("row list_buckets_with_tombstones")?);
    }
    Ok(out)
}

/// List every distinct scope_id with at least one `segment_records` row.
/// Optionally restrict to a specific `kind`. Used by the Plan 3 compaction
/// worker to drive the per-scope sweep.
///
/// For mail / calendar / post kinds, scope_id == actor_id (the owner). For
/// conversation kinds (Plan 7+), scope_id == channel_id. Returned ids are
/// de-duplicated; ordering is by raw blob byte comparison (the natural
/// index/scan order — call sites don't depend on specific ordering).
pub fn list_scopes_with_segments(conn: &Connection, kind: Option<&str>) -> Result<Vec<[u8; 32]>> {
    // Both branches share identical row-parsing; captured in a closure so
    // the duplication doesn't creep back in.
    let parse_row =
        |v: Vec<u8>| -> Result<[u8; 32]> { crate::db::blob_to_array(v.as_slice(), "scope_id") };

    let mut out: Vec<[u8; 32]> = Vec::new();
    if let Some(k) = kind {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT scope_id
                 FROM segment_records
                 WHERE kind = ?1
                 ORDER BY scope_id ASC",
            )
            .context("prepare list_scopes_with_segments(kind)")?;
        let rows = stmt
            .query_map(params![k], |r| r.get::<_, Vec<u8>>(0))
            .context("query_map list_scopes_with_segments(kind)")?;
        for r in rows {
            out.push(parse_row(
                r.context("row list_scopes_with_segments(kind)")?,
            )?);
        }
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT scope_id
                 FROM segment_records
                 ORDER BY scope_id ASC",
            )
            .context("prepare list_scopes_with_segments(*)")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .context("query_map list_scopes_with_segments(*)")?;
        for r in rows {
            out.push(parse_row(r.context("row list_scopes_with_segments(*)")?)?);
        }
    }
    Ok(out)
}

/// Pre-fetch the live `(segment_id, record_cid)` pairs for a given
/// `(scope, kind)` across the supplied input segment ids. Returns the
/// `HashSet` that the synchronous `fauna_segment_store::compact` closure
/// queries by `set.contains(&(segment_id, cid))`.
///
/// SQLite's default `SQLITE_MAX_VARIABLE_NUMBER` is 999 — chunk the input
/// segment list so each prepared statement stays under that bound.
pub fn live_set_for_segments(
    conn: &Connection,
    scope_id: &[u8; 32],
    kind: &str,
    segment_ids: &[u32],
) -> Result<HashSet<(u32, Cid)>> {
    const CHUNK: usize = 900; // leaves headroom for the leading scope/kind params
    let mut out: HashSet<(u32, Cid)> = HashSet::new();
    if segment_ids.is_empty() {
        return Ok(out);
    }
    for chunk in segment_ids.chunks(CHUNK) {
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 3)).collect();
        let sql = format!(
            "SELECT segment_id, record_cid
             FROM segment_records
             WHERE scope_id = ?1 AND kind = ?2 AND tombstoned = 0
               AND segment_id IN ({})",
            placeholders.join(", ")
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("prepare live_set_for_segments")?;
        // Build params: [scope, kind, seg1, seg2, ...]
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> =
            Vec::with_capacity(2 + chunk.len());
        params_vec.push(Box::new(scope_id.to_vec()));
        params_vec.push(Box::new(kind.to_string()));
        for s in chunk {
            params_vec.push(Box::new(*s as i64));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), |r| {
                Ok((r.get::<_, i64>(0)? as u32, r.get::<_, Vec<u8>>(1)?))
            })
            .context("query_map live_set_for_segments")?;
        for r in rows {
            let (seg, cid_blob) = r.context("row live_set_for_segments")?;
            out.insert((seg, cid_from_blob(cid_blob)?));
        }
    }
    Ok(out)
}

/// One mail-kind row to insert for the new compacted segment. Used by
/// [`apply_compaction_tx`] to avoid re-opening the segment inside the
/// SQL transaction body.
pub struct NewSegmentRecord {
    pub record_cid: Cid,
    pub bucket: String,
    pub received_at: i64,
    pub sender_domain: String,
    pub spam_disposition: String,
    pub is_own_submission: bool,
    /// Per-actor monotonic relay cursor, carried through compaction so a
    /// segment rewrite preserves the cursor (read back from the survivor's
    /// `MailFloorMetadata.seq` by `mail::compact_bucket`). 0 for records
    /// written before the relay landed (their floor carried no seq).
    pub seq: i64,
    /// Continuation-record role (0/1/2), carried through compaction so a
    /// rewrite preserves a PART/HEAD marking (read back from the survivor's
    /// `MailFloorMetadata.continuation_role`). A rewrite that reset this to 0
    /// would make the reaper treat a live head as a normal record and would
    /// unmark parts — so compaction must carry it verbatim.
    pub continuation_role: u8,
    /// When this nest stored the record (epoch ms), carried through compaction
    /// from the survivor's `MailFloorMetadata.stored_at` — the reaper's grace
    /// keys on it. A rewrite that re-stamped it to "now" would silently restart
    /// every part's grace, so a compaction cadence tighter than the grace would
    /// keep headless parts alive forever; one that dropped it to NULL would do
    /// the same via the not-reapable rule. Either way the reaper quietly stops
    /// reclaiming, so compaction carries it verbatim. `0` = unknown (a failed
    /// clock read) and is stored as NULL.
    pub stored_at: i64,
}

/// Execute the compaction SQL transaction body: tombstone every row that
/// belonged to an input segment, then INSERT a row for each record in the
/// new (rewritten) segment.
///
/// The caller (i.e. `compact_bucket`) must supply an **already-begun
/// transaction** from its own `CacheDb` connection so the lock-ordering
/// invariant `per-scope-mutex → conn-mutex` is preserved — the
/// connection mutex is acquired before calling this function, not inside.
///
/// `new_segment_id` is `None` when `fauna_segment_store::compact`
/// produced no survivors (every record was tombstoned); in that case the
/// INSERT loop is skipped.
pub fn apply_compaction_tx(
    tx: &Transaction,
    scope_id: &[u8; 32],
    kind: &str,
    input_segment_ids: &[u32],
    new_segment_id: Option<u32>,
    new_segment_records: &[NewSegmentRecord],
) -> Result<()> {
    // Tombstone every live row for each input segment (shared with conv).
    tombstone_input_segments(tx, scope_id, kind, input_segment_ids)?;
    // INSERT new segment's rows — only when compact() produced a non-empty output.
    if let Some(new_id) = new_segment_id {
        for rec in new_segment_records {
            tx.execute(
                "INSERT INTO segment_records
                    (scope_id, kind, segment_id, record_cid, bucket,
                     tombstoned,
                     received_at, sender_dom, spam_disp, is_own_submission, seq,
                     continuation_role, stored_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    scope_id.as_slice(),
                    kind,
                    new_id as i64,
                    &rec.record_cid.as_bytes()[..],
                    &rec.bucket,
                    rec.received_at,
                    &rec.sender_domain,
                    &rec.spam_disposition,
                    if rec.is_own_submission { 1i64 } else { 0i64 },
                    rec.seq,
                    rec.continuation_role as i64,
                    // 0 = unknown -> NULL (not reapable), never "epoch".
                    (rec.stored_at > 0).then_some(rec.stored_at),
                ],
            )
            .context("INSERT segment_records new segment row")?;
        }
    }
    Ok(())
}

/// Tombstone every live `segment_records` row belonging to one of the
/// `input_segment_ids` for `(scope, kind)`. Shared by [`apply_compaction_tx`]
/// (mail) and [`apply_conv_compaction_tx`] (conv) so the tombstone SQL lives
/// once — the only kind-divergent part of compaction is the new-segment INSERT
/// (mail-floor columns vs conv `seq`).
fn tombstone_input_segments(
    tx: &Transaction,
    scope_id: &[u8; 32],
    kind: &str,
    input_segment_ids: &[u32],
) -> Result<()> {
    for input_seg in input_segment_ids {
        tx.execute(
            "UPDATE segment_records
             SET tombstoned = 1
             WHERE scope_id = ?1 AND kind = ?2 AND segment_id = ?3
               AND tombstoned = 0",
            params![scope_id.as_slice(), kind, *input_seg as i64],
        )
        .context("UPDATE segment_records tombstone inputs")?;
    }
    Ok(())
}

/// One conv-kind row to insert for the new compacted segment. Conv's mirror
/// shape diverges from mail's [`NewSegmentRecord`]: it carries the per-channel
/// monotonic `seq` and leaves the mail-floor columns (`sender_dom` /
/// `spam_disp` / `is_own_submission`) NULL — exactly [`insert_conv`]'s shape.
/// Used by [`apply_conv_compaction_tx`] to avoid re-opening the segment inside
/// the SQL transaction body.
pub struct NewConvSegmentRecord {
    pub record_cid: Cid,
    pub bucket: String,
    pub received_at: i64,
    pub seq: i64,
}

/// Conv sibling of [`apply_compaction_tx`]: tombstone every row belonging to an
/// input segment, then INSERT one conv row per record in the new (rewritten)
/// segment. The new rows use the SAME column set as [`insert_conv`]
/// (`kind = 'conv'`, mail-floor columns NULL, `seq` set, `tombstoned = 0`).
///
/// The caller (`conv::compact_bucket`) supplies an **already-begun transaction**
/// so the lock-ordering invariant `per-scope-mutex → conn-mutex` is preserved
/// (the conn mutex is acquired before this call, not inside).
///
/// `new_segment_id` is `None` when `fauna_segment_store::compact` produced no
/// survivors (every record was tombstoned); the INSERT loop is then skipped.
pub fn apply_conv_compaction_tx(
    tx: &Transaction,
    scope_id: &[u8; 32],
    input_segment_ids: &[u32],
    new_segment_id: Option<u32>,
    new_segment_records: &[NewConvSegmentRecord],
) -> Result<()> {
    // Tombstone every live row for each input segment (shared with mail).
    tombstone_input_segments(tx, scope_id, "conv", input_segment_ids)?;
    // INSERT new segment's rows — only when compact() produced survivors.
    if let Some(new_id) = new_segment_id {
        for rec in new_segment_records {
            tx.execute(
                "INSERT INTO segment_records
                    (scope_id, kind, segment_id, record_cid, bucket,
                     tombstoned,
                     received_at, sender_dom, spam_disp, is_own_submission, seq)
                 VALUES (?1, 'conv', ?2, ?3, ?4, 0, ?5, NULL, NULL, NULL, ?6)",
                params![
                    scope_id.as_slice(),
                    new_id as i64,
                    &rec.record_cid.as_bytes()[..],
                    &rec.bucket,
                    rec.received_at,
                    rec.seq,
                ],
            )
            .context("INSERT segment_records new conv segment row")?;
        }
    }
    Ok(())
}

// ============================ post kind ============================
//
// Posts differ from mail/conv: they are **point-read-by-CID**, never
// range-by-seq. The record's CID is `Cid::of_dag_cbor(post_body)`, whose
// digest IS the `post_id` (`blake3(body)`), so a post is found by deriving
// its CID from the `post_id` a reader already holds — no `seq` and no
// per-record envelope. The mirror row therefore leaves `seq` and every
// mail-floor column NULL; `received_at` carries the post's `created_at`
// (for bucket recovery during compaction).

/// Insert a `segment_records` row for a **point-read-by-CID** kind — the shape
/// shared by `post`, `calendar`, and `card`: `seq` and every mail-floor column
/// are NULL, and `received_at` carries the record's own timestamp purely so
/// compaction can recover its bucket.
///
/// The `kind` is bound as a parameter, not interpolated. Prefer the named
/// per-kind wrappers ([`insert_post`], [`insert_calendar`]) at call sites so a
/// kind tag stays greppable; they exist to name the kind, not to duplicate this
/// statement. See [`insert_conv`] / [`insert_mail`] for the seq-bearing shapes.
fn insert_point_read_record(
    conn: &Connection,
    kind: &str,
    scope_id: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
    bucket: &str,
    received_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO segment_records
            (scope_id, kind, segment_id, record_cid, bucket,
             tombstoned, changed_seq,
             received_at, sender_dom, spam_disp, is_own_submission, seq)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, NULL, NULL, NULL, NULL)",
        params![
            &scope_id[..],
            kind,
            segment_id as i64,
            &cid.as_bytes()[..],
            bucket,
            next_changed_seq(conn, scope_id, kind)?,
            received_at,
        ],
    )
    .with_context(|| format!("insert segment_records row ({kind})"))?;
    Ok(())
}

/// Insert a post-kind `segment_records` row. `scope_id` is the author actor
/// id; `seq` and the mail-floor columns are NULL (posts are addressed by CID,
/// not by a per-scope counter). See [`insert_conv`] for the sibling shape.
pub fn insert_post(
    conn: &Connection,
    scope_id: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
    bucket: &str,
    received_at: i64,
) -> Result<()> {
    insert_point_read_record(conn, "post", scope_id, segment_id, cid, bucket, received_at)
}

/// Insert a calendar-kind `segment_records` row (S6.4). `scope_id` is the owner
/// actor id — calendar rows are actor-scoped exactly as mail is, with
/// `calendar_id` a sub-scope carried inside the record's floor rather than a
/// separate segment store.
///
/// The record CID is the content hash of the sealed envelope
/// (`segments::cal::append_record`'s mint — every kind's convention since the
/// identity cutover). `received_at` carries the row's `created_at`
/// (epoch **seconds**; mail's is milliseconds).
pub fn insert_calendar(
    conn: &Connection,
    scope_id: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
    bucket: &str,
    created_at: i64,
) -> Result<()> {
    insert_point_read_record(
        conn, "calendar", scope_id, segment_id, cid, bucket, created_at,
    )
}

/// Insert a card-kind `segment_records` row (S6.5). Structural twin of
/// [`insert_calendar`]: `scope_id` is the owner actor id, the record CID is
/// the content hash of the sealed envelope, and `created_at` is epoch **seconds**.
pub fn insert_card(
    conn: &Connection,
    scope_id: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
    bucket: &str,
    created_at: i64,
) -> Result<()> {
    insert_point_read_record(conn, "card", scope_id, segment_id, cid, bucket, created_at)
}

/// Resolve a record's `(scope_id, segment_id)` from its `(kind, record_cid)`
/// alone — for point reads where the caller holds only the CID (the post
/// read path: a reader has the `post_id` → derives the CID → must find which
/// author's segment holds it). Backed by `idx_segment_records_record_cid`.
/// Returns the first match **whether or not it is tombstoned** — the segment
/// file keeps a tombstoned record's bytes until compaction reclaims them, so a
/// caller asking "which segment still holds these bytes" must see it.
///
/// The legal-takedown withhold is that caller (`moderation.md` § Legal takedown
/// → *Posts*): a post its author deleted while taken down is tombstoned, and
/// resolving it through the live-only lookup below answers `None`, which would
/// silently withhold nothing and ship the compelled body in the very export the
/// withhold exists to keep it out of.
pub fn lookup_scope_and_segment_including_tombstoned(
    conn: &Connection,
    kind: &str,
    cid: &Cid,
) -> Result<Option<([u8; 32], u32)>> {
    let row: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT scope_id, segment_id FROM segment_records
              WHERE kind = ?1 AND record_cid = ?2
              LIMIT 1",
            params![kind, &cid.as_bytes()[..]],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("lookup_scope_and_segment_including_tombstoned")?;
    let Some((scope_blob, segment_id)) = row else {
        return Ok(None);
    };
    let scope: [u8; 32] = crate::db::blob_to_array(scope_blob.as_slice(), "scope_id")?;
    Ok(Some((scope, segment_id as u32)))
}

/// Returns the first live (non-tombstoned) match.
pub fn lookup_scope_and_segment(
    conn: &Connection,
    kind: &str,
    cid: &Cid,
) -> Result<Option<([u8; 32], u32)>> {
    let row: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT scope_id, segment_id FROM segment_records
              WHERE kind = ?1 AND record_cid = ?2 AND tombstoned = 0
              LIMIT 1",
            params![kind, &cid.as_bytes()[..]],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("lookup_scope_and_segment")?;
    let Some((scope_blob, segment_id)) = row else {
        return Ok(None);
    };
    let scope: [u8; 32] = crate::db::blob_to_array(scope_blob.as_slice(), "scope_id")?;
    Ok(Some((scope, segment_id as u32)))
}

/// One **point-read-by-CID** row to insert for the new compacted segment — the
/// shape shared by `post`, `calendar` and `card`. Mirror of
/// [`NewConvSegmentRecord`] minus `seq` (these kinds carry no per-scope counter).
///
/// `received_at` carries the record's own timestamp so a later compaction can
/// recover its bucket: milliseconds for `post`, **seconds** for `calendar` /
/// `card` (their floors' `created_at`). Each caller passes its own unit; nothing
/// here divides.
pub struct NewPointReadSegmentRecord {
    pub record_cid: Cid,
    pub bucket: String,
    pub received_at: i64,
}

/// Point-read sibling of [`apply_conv_compaction_tx`]: tombstone every row for
/// an input segment, then INSERT one row per surviving record in the new
/// (rewritten) segment. Caller supplies an already-begun transaction (lock-order
/// `per-scope-mutex → conn-mutex`).
///
/// The INSERT is [`insert_point_read_record`] itself — a `Transaction` derefs to
/// `Connection` — so the compaction path and the append path can never drift in
/// what a point-read mirror row looks like. Prefer the named per-kind wrappers
/// ([`apply_post_compaction_tx`], [`apply_calendar_compaction_tx`],
/// [`apply_card_compaction_tx`]) at call sites so the kind tag stays greppable.
fn apply_point_read_compaction_tx(
    tx: &Transaction,
    scope_id: &[u8; 32],
    kind: &str,
    input_segment_ids: &[u32],
    new_segment_id: Option<u32>,
    new_segment_records: &[NewPointReadSegmentRecord],
) -> Result<()> {
    tombstone_input_segments(tx, scope_id, kind, input_segment_ids)?;
    if let Some(new_id) = new_segment_id {
        for rec in new_segment_records {
            insert_point_read_record(
                tx,
                kind,
                scope_id,
                new_id,
                &rec.record_cid,
                &rec.bucket,
                rec.received_at,
            )?;
        }
    }
    Ok(())
}

/// Apply a post-kind bucket compaction to the mirror. See
/// [`apply_point_read_compaction_tx`].
pub fn apply_post_compaction_tx(
    tx: &Transaction,
    scope_id: &[u8; 32],
    input_segment_ids: &[u32],
    new_segment_id: Option<u32>,
    new_segment_records: &[NewPointReadSegmentRecord],
) -> Result<()> {
    apply_point_read_compaction_tx(
        tx,
        scope_id,
        "post",
        input_segment_ids,
        new_segment_id,
        new_segment_records,
    )
}

/// Apply a calendar-kind bucket compaction to the mirror (S6.8c). `scope_id` is
/// the owner actor id; `received_at` is epoch **seconds**.
pub fn apply_calendar_compaction_tx(
    tx: &Transaction,
    scope_id: &[u8; 32],
    input_segment_ids: &[u32],
    new_segment_id: Option<u32>,
    new_segment_records: &[NewPointReadSegmentRecord],
) -> Result<()> {
    apply_point_read_compaction_tx(
        tx,
        scope_id,
        "calendar",
        input_segment_ids,
        new_segment_id,
        new_segment_records,
    )
}

/// Apply a card-kind bucket compaction to the mirror (S6.8c). Structural twin of
/// [`apply_calendar_compaction_tx`].
pub fn apply_card_compaction_tx(
    tx: &Transaction,
    scope_id: &[u8; 32],
    input_segment_ids: &[u32],
    new_segment_id: Option<u32>,
    new_segment_records: &[NewPointReadSegmentRecord],
) -> Result<()> {
    apply_point_read_compaction_tx(
        tx,
        scope_id,
        "card",
        input_segment_ids,
        new_segment_id,
        new_segment_records,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        crate::db::migrations::run_migrations(&conn).expect("apply schema");
        conn
    }

    /// A deterministic, unique test record Cid from a short tag — the mirror
    /// key column is the full 36-byte Cid, so fixtures build one per tag.
    fn rc(tag: &[u8]) -> Cid {
        Cid::of_dag_cbor(tag)
    }

    #[test]
    fn insert_then_lookup_record() {
        let conn = open();
        let actor = [0x11u8; 32];
        let cid = rc(b"r-22");
        insert_mail(
            &conn,
            &actor,
            1,
            &cid,
            "2026-05",
            1_715_000_000_000,
            "example.com",
            "accept",
            false,
            1,
            Some(&[0xABu8; 32]),
            0,
            1_715_000_000_000,
        )
        .expect("insert");

        let rec = lookup_record(&conn, &actor, "mail", &cid)
            .expect("lookup")
            .expect("found");
        assert_eq!(rec.segment_id, 1);
        assert_eq!(rec.record_cid, cid);

        // The report-hash mirror column round-trips (report-sharing.md
        // § Content identity — the hot-path lookup Slice 2 consumes).
        let stored: Option<Vec<u8>> = conn
            .query_row(
                "SELECT report_hash FROM segment_records WHERE record_cid = ?1",
                rusqlite::params![&cid.as_bytes()[..]],
                |row| row.get(0),
            )
            .expect("read report_hash");
        assert_eq!(stored.as_deref(), Some(&[0xABu8; 32][..]));
    }

    #[test]
    fn report_hash_absent_stores_null() {
        let conn = open();
        let actor = [0x77u8; 32];
        let cid = rc(b"r-88");
        insert_mail(
            &conn, &actor, 1, &cid, "2026-05", 0, "x", "accept", false, 1, None, 0, 0,
        )
        .expect("insert");
        let stored: Option<Vec<u8>> = conn
            .query_row(
                "SELECT report_hash FROM segment_records WHERE record_cid = ?1",
                rusqlite::params![&cid.as_bytes()[..]],
                |row| row.get(0),
            )
            .expect("read report_hash");
        assert!(stored.is_none(), "absent hash is NULL, not empty blob");
    }

    #[test]
    fn mark_tombstoned_hides_from_lookup() {
        let conn = open();
        let actor = [0x55u8; 32];
        let cid = rc(b"r-66");
        insert_mail(
            &conn, &actor, 1, &cid, "2026-05", 0, "x", "accept", false, 1, None, 0, 0,
        )
        .expect("insert");
        assert!(
            lookup_record(&conn, &actor, "mail", &cid)
                .expect("ok")
                .is_some()
        );
        let updated = mark_tombstoned(&conn, &actor, "mail", 1, &cid).expect("mark");
        assert_eq!(updated, 1);
        assert!(
            lookup_record(&conn, &actor, "mail", &cid)
                .expect("ok")
                .is_none()
        );
    }

    #[test]
    fn count_tombstoned_for_segment_counts_only_tombstoned_rows() {
        let conn = open();
        let actor = [0x99u8; 32];
        // Seg 1: 2 live, 2 tombstoned. Seg 2: 1 tombstoned (excluded from
        // the seg-1 count).
        for (seg, rec, t) in &[
            (1u32, b"a".to_vec(), 0i64),
            (1u32, b"b".to_vec(), 1i64),
            (1u32, b"c".to_vec(), 1i64),
            (1u32, b"d".to_vec(), 0i64),
            (2u32, b"e".to_vec(), 1i64),
        ] {
            conn.execute(
                "INSERT INTO segment_records (scope_id, kind, segment_id, record_cid,
                                              bucket, tombstoned)
                 VALUES (?1, 'mail', ?2, ?3, '2026-05', ?4)",
                params![actor.as_slice(), seg, &rc(rec).as_bytes()[..], t],
            )
            .unwrap();
        }
        let n1 = count_tombstoned_for_segment(&conn, &actor, "mail", 1).expect("count seg 1");
        assert_eq!(n1, 2);
        let n2 = count_tombstoned_for_segment(&conn, &actor, "mail", 2).expect("count seg 2");
        assert_eq!(n2, 1);
        let n_none =
            count_tombstoned_for_segment(&conn, &actor, "mail", 999).expect("count missing");
        assert_eq!(n_none, 0);
    }

    #[test]
    fn mark_tombstoned_flips_the_bit_idempotently() {
        let conn = open();
        let actor = [0x11u8; 32];
        let cid = rc(b"record-id");
        // Seed one row.
        conn.execute(
            "INSERT INTO segment_records (scope_id, kind, segment_id, record_cid,
                                          bucket, tombstoned)
             VALUES (?1, 'mail', 1, ?2, '2026-05', 0)",
            params![actor.as_slice(), &cid.as_bytes()[..]],
        )
        .unwrap();
        let n = mark_tombstoned(&conn, &actor, "mail", 1, &cid).expect("mark");
        assert_eq!(n, 1);
        let again = mark_tombstoned(&conn, &actor, "mail", 1, &cid).expect("again");
        assert_eq!(again, 0, "second mark is idempotent");
        let t: i64 = conn
            .query_row(
                "SELECT tombstoned FROM segment_records WHERE record_cid = ?1",
                params![&cid.as_bytes()[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(t, 1);
    }

    #[test]
    fn count_segment_stats_groups_by_segment_id() {
        let conn = open();
        let actor = [0x22u8; 32];
        // Seg 1: 2 live + 1 tombstoned. Seg 2: 1 live.
        for (seg, rec, t) in &[
            (1u32, b"a".to_vec(), 0),
            (1u32, b"b".to_vec(), 1),
            (1u32, b"c".to_vec(), 0),
            (2u32, b"d".to_vec(), 0),
        ] {
            conn.execute(
                "INSERT INTO segment_records (scope_id, kind, segment_id, record_cid,
                                              bucket, tombstoned)
                 VALUES (?1, 'mail', ?2, ?3, '2026-05', ?4)",
                params![actor.as_slice(), seg, &rc(rec).as_bytes()[..], t],
            )
            .unwrap();
        }
        let stats = count_segment_stats(&conn, &actor, "mail", "2026-05").expect("stats");
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].segment_id, 1);
        assert_eq!(stats[0].record_count, 3);
        assert_eq!(stats[0].tombstone_count, 1);
        assert_eq!(stats[1].segment_id, 2);
        assert_eq!(stats[1].record_count, 1);
        assert_eq!(stats[1].tombstone_count, 0);
    }

    #[test]
    fn list_buckets_with_tombstones_returns_only_buckets_with_tombstoned_rows() {
        let conn = open();
        let actor = [0x77u8; 32];
        // Bucket A: 2 live, 1 tombstoned. Bucket B: 1 live. Bucket C: 1
        // tombstoned (different segment).
        for (seg, rec, bucket, t) in &[
            (1u32, b"a1".to_vec(), "2026-04", 0),
            (1u32, b"a2".to_vec(), "2026-04", 1),
            (1u32, b"a3".to_vec(), "2026-04", 0),
            (2u32, b"b1".to_vec(), "2026-05", 0),
            (3u32, b"c1".to_vec(), "2026-06", 1),
        ] {
            conn.execute(
                "INSERT INTO segment_records (scope_id, kind, segment_id, record_cid,
                                              bucket, tombstoned)
                 VALUES (?1, 'mail', ?2, ?3, ?4, ?5)",
                params![actor.as_slice(), seg, &rc(rec).as_bytes()[..], bucket, t],
            )
            .unwrap();
        }
        let buckets = list_buckets_with_tombstones(&conn, &actor, "mail").expect("buckets");
        assert_eq!(
            buckets,
            vec!["2026-04".to_string(), "2026-06".to_string()],
            "only buckets containing at least one tombstoned row, sorted ASC"
        );
    }

    #[test]
    fn list_scopes_with_segments_returns_distinct_scopes() {
        let conn = open();
        let a1 = [0x11u8; 32];
        let a2 = [0x22u8; 32];
        for (actor, rec) in &[
            (a1, b"r1".to_vec()),
            (a1, b"r2".to_vec()),
            (a2, b"r3".to_vec()),
        ] {
            conn.execute(
                "INSERT INTO segment_records (scope_id, kind, segment_id, record_cid,
                                              bucket, tombstoned)
                 VALUES (?1, 'mail', 1, ?2, '2026-05', 0)",
                params![actor.as_slice(), &rc(rec).as_bytes()[..]],
            )
            .unwrap();
        }
        let all = list_scopes_with_segments(&conn, Some("mail")).expect("list");
        assert_eq!(all.len(), 2);
        assert!(all.contains(&a1));
        assert!(all.contains(&a2));
    }

    #[test]
    fn live_set_for_segments_returns_only_non_tombstoned_pairs() {
        let conn = open();
        let actor = [0x55u8; 32];
        // Seg 1: r1 live, r2 tombstoned. Seg 2: r3 live. Seg 3: r4 tombstoned.
        for (seg, rec, t) in &[
            (1u32, b"r1".to_vec(), 0),
            (1u32, b"r2".to_vec(), 1),
            (2u32, b"r3".to_vec(), 0),
            (3u32, b"r4".to_vec(), 1),
        ] {
            conn.execute(
                "INSERT INTO segment_records (scope_id, kind, segment_id, record_cid,
                                              bucket, tombstoned)
                 VALUES (?1, 'mail', ?2, ?3, '2026-05', ?4)",
                params![actor.as_slice(), seg, &rc(rec).as_bytes()[..], t],
            )
            .unwrap();
        }
        let set = live_set_for_segments(&conn, &actor, "mail", &[1, 2, 3]).expect("live_set");
        assert_eq!(set.len(), 2);
        assert!(set.contains(&(1u32, rc(b"r1"))));
        assert!(set.contains(&(2u32, rc(b"r3"))));
        // r2 is tombstoned; r4 is tombstoned — both excluded.
        assert!(!set.contains(&(1u32, rc(b"r2"))));
        assert!(!set.contains(&(3u32, rc(b"r4"))));
    }

    #[test]
    fn live_set_for_segments_empty_inputs_returns_empty() {
        let conn = open();
        let actor = [0u8; 32];
        let set = live_set_for_segments(&conn, &actor, "mail", &[]).expect("empty");
        assert!(set.is_empty());
    }

    // --- mail-kind relay DAOs (Slice 1: public→private mail relay) ---

    #[allow(clippy::too_many_arguments)]
    fn insert_mail_seq(conn: &Connection, actor: &[u8; 32], rid: &[u8], seq: i64) {
        insert_mail(
            conn,
            actor,
            1,
            &rc(rid),
            "2026-05",
            1_715_000_000_000,
            "example.com",
            "accept",
            false,
            seq,
            None,
            0,
            1_715_000_000_000,
        )
        .expect("insert mail");
    }

    #[test]
    fn next_mail_seq_increments_and_ignores_tombstones() {
        let conn = open();
        let actor = [0xA1u8; 32];
        // Empty actor → first seq is 1.
        assert_eq!(next_mail_seq(&conn, &actor).expect("seq"), 1);
        insert_mail_seq(&conn, &actor, b"a", 1);
        assert_eq!(next_mail_seq(&conn, &actor).expect("seq"), 2);
        insert_mail_seq(&conn, &actor, b"b", 2);
        // Tombstoning must NOT lower the counter — seq is never reused.
        let n = tombstone_mail_up_to_seq(&conn, &actor, 2).expect("tombstone");
        assert_eq!(n, 2, "both rows tombstoned");
        assert_eq!(
            next_mail_seq(&conn, &actor).expect("seq"),
            3,
            "MAX(seq) still 2 even after tombstoning → next is 3"
        );
    }

    #[test]
    fn list_mail_after_seq_excludes_at_or_below_cursor_and_tombstoned() {
        let conn = open();
        let actor = [0xA2u8; 32];
        for s in 1..=3i64 {
            insert_mail_seq(&conn, &actor, &[s as u8; 32], s);
        }
        // after_seq = 1 → only seq 2, 3.
        let rows = list_mail_after_seq(&conn, &actor, 1, 100).expect("list");
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![2, 3]);
        // Tombstoning seq 2 hides it from subsequent reads.
        tombstone_mail_up_to_seq(&conn, &actor, 2).expect("tombstone");
        let rows = list_mail_after_seq(&conn, &actor, 1, 100).expect("list");
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn list_mail_after_seq_is_actor_scoped() {
        let conn = open();
        let a1 = [0xA3u8; 32];
        let a2 = [0xA4u8; 32];
        insert_mail_seq(&conn, &a1, b"a1r1", 1);
        insert_mail_seq(&conn, &a2, b"a2r1", 1);
        // Each actor has its own independent cursor space.
        let rows = list_mail_after_seq(&conn, &a1, 0, 100).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2, rc(b"a1r1"));
    }

    #[test]
    fn tombstone_mail_up_to_seq_is_idempotent() {
        let conn = open();
        let actor = [0xA5u8; 32];
        insert_mail_seq(&conn, &actor, b"a", 1);
        insert_mail_seq(&conn, &actor, b"b", 2);
        assert_eq!(tombstone_mail_up_to_seq(&conn, &actor, 2).expect("ack"), 2);
        // Re-acking the same cursor tombstones nothing new.
        assert_eq!(
            tombstone_mail_up_to_seq(&conn, &actor, 2).expect("re-ack"),
            0
        );
    }

    /// The row-4 fix, isolated: the relay-ack purge used to flip `tombstoned`
    /// in one bulk UPDATE without touching `changed_seq`, so a purged row's
    /// coordinate never moved and a replica already past it would never learn
    /// of the delete — the reason `mail` sat off `FEED_SERVED_KINDS`. Each
    /// purged row must now get its own coordinate, strictly above whatever the
    /// scope's max was before the purge, and no two purged rows may share one.
    #[test]
    fn tombstone_mail_up_to_seq_gives_each_row_its_own_feed_coordinate() {
        let conn = open();
        let actor = [0xA6u8; 32];
        insert_mail_seq(&conn, &actor, b"a", 1);
        insert_mail_seq(&conn, &actor, b"b", 2);
        let before = content_feed_after(&conn, &actor, "mail", 0, 100).expect("feed before");
        let max_before = before.iter().map(|r| r.changed_seq).max().unwrap();

        assert_eq!(tombstone_mail_up_to_seq(&conn, &actor, 2).expect("ack"), 2);

        let after = content_feed_after(&conn, &actor, "mail", 0, 100).expect("feed after");
        assert_eq!(after.len(), 2, "both records still appear — as tombstones");
        assert!(after.iter().all(|r| r.tombstoned));
        let mut coords: Vec<i64> = after.iter().map(|r| r.changed_seq).collect();
        coords.sort_unstable();
        assert!(
            coords.iter().all(|c| *c > max_before),
            "every purged row's coordinate must move strictly above the \
             pre-purge max ({max_before}): got {coords:?}"
        );
        assert_ne!(
            coords[0], coords[1],
            "the two purged rows must get DISTINCT coordinates, not one \
             shared bump"
        );
    }

    // --- conv-kind DAOs (Plan 7) ---

    #[test]
    fn insert_conv_then_list_after_seq_returns_it() {
        let conn = open();
        let channel = [0x10u8; 32];
        let cid = rc(b"conv-20");
        insert_conv(&conn, &channel, 1, &cid, "2026-05", 1_715_000_000_000, 1).expect("insert");
        let rows = list_conv_after_seq(&conn, &channel, 0, 100).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 1, "seq");
        assert_eq!(rows[0].1, 1, "segment_id");
        assert_eq!(rows[0].2, cid, "record_cid");
    }

    #[test]
    fn next_conv_seq_increments_and_ignores_tombstones() {
        let conn = open();
        let channel = [0x11u8; 32];
        // Empty channel → first seq is 1.
        assert_eq!(next_conv_seq(&conn, &channel).expect("seq"), 1);
        insert_conv(&conn, &channel, 1, &rc(b"a"), "2026-05", 0, 1).expect("a");
        assert_eq!(next_conv_seq(&conn, &channel).expect("seq"), 2);
        insert_conv(&conn, &channel, 1, &rc(b"b"), "2026-05", 0, 2).expect("b");
        // Tombstoning seq 2 must NOT lower the counter — seq is never reused.
        let n = tombstone_conv_up_to_seq(&conn, &[channel], 2).expect("tombstone");
        assert_eq!(n, 2, "both rows tombstoned");
        assert_eq!(
            next_conv_seq(&conn, &channel).expect("seq"),
            3,
            "MAX(seq) still 2 even after tombstoning → next is 3"
        );
    }

    #[test]
    fn list_conv_after_seq_excludes_at_or_below_cursor() {
        let conn = open();
        let channel = [0x12u8; 32];
        for s in 1..=3i64 {
            insert_conv(&conn, &channel, 1, &rc(&[s as u8; 32]), "2026-05", 0, s).expect("insert");
        }
        // after_seq = 1 → only seq 2, 3.
        let rows = list_conv_after_seq(&conn, &channel, 1, 100).expect("list");
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn list_conv_for_scopes_merges_ordered_by_seq() {
        let conn = open();
        let ch1 = [0x13u8; 32];
        let ch2 = [0x14u8; 32];
        // Interleave seqs across two channels (each channel's seq is independent).
        insert_conv(&conn, &ch1, 1, &rc(b"c1s1"), "2026-05", 0, 1).expect("c1s1");
        insert_conv(&conn, &ch1, 1, &rc(b"c1s2"), "2026-05", 0, 2).expect("c1s2");
        insert_conv(&conn, &ch2, 1, &rc(b"c2s1"), "2026-05", 0, 1).expect("c2s1");
        insert_conv(&conn, &ch2, 1, &rc(b"c2s3"), "2026-05", 0, 3).expect("c2s3");

        let rows = list_conv_for_scopes_after_seq(&conn, &[ch1, ch2], 0, 100).expect("list");
        // Merged ordering by seq ASC: 1, 1, 2, 3.
        assert_eq!(
            rows.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![1, 1, 2, 3]
        );
        // Both scopes represented; record_ids carried through.
        let scopes: HashSet<[u8; 32]> = rows.iter().map(|r| r.1).collect();
        assert!(scopes.contains(&ch1));
        assert!(scopes.contains(&ch2));
    }

    #[test]
    fn list_conv_for_scopes_empty_inputs_returns_empty() {
        let conn = open();
        let rows = list_conv_for_scopes_after_seq(&conn, &[], 0, 100).expect("list");
        assert!(rows.is_empty());
    }

    #[test]
    fn tombstone_up_to_seq_hides_rows_and_returns_count() {
        let conn = open();
        let channel = [0x15u8; 32];
        for s in 1..=3i64 {
            insert_conv(&conn, &channel, 1, &rc(&[s as u8; 32]), "2026-05", 0, s).expect("insert");
        }
        // Tombstone seq <= 2 → 2 rows updated.
        let n = tombstone_conv_up_to_seq(&conn, &[channel], 2).expect("tombstone");
        assert_eq!(n, 2);
        // Only seq 3 survives the list.
        let rows = list_conv_after_seq(&conn, &channel, 0, 100).expect("list");
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![3]);
        // Re-tombstoning is idempotent — already-tombstoned rows aren't counted.
        let again = tombstone_conv_up_to_seq(&conn, &[channel], 2).expect("again");
        assert_eq!(again, 0);
    }

    #[test]
    fn tombstone_up_to_seq_empty_inputs_returns_zero() {
        let conn = open();
        let n = tombstone_conv_up_to_seq(&conn, &[], 100).expect("zero");
        assert_eq!(n, 0);
    }

    /// Multi-channel sibling of
    /// `tombstone_mail_up_to_seq_gives_each_row_its_own_feed_coordinate`: the
    /// purge's coordinate assignment must partition by scope, not hand out one
    /// shared sequence across every channel in the call — each channel is its
    /// own `(scope_id, kind)` counter, exactly like ordinary conv appends.
    #[test]
    fn tombstone_conv_up_to_seq_partitions_coordinates_per_channel() {
        let conn = open();
        let ch1 = [0x16u8; 32];
        let ch2 = [0x17u8; 32];
        insert_conv(&conn, &ch1, 1, &rc(b"c1a"), "2026-05", 0, 1).expect("c1a");
        insert_conv(&conn, &ch1, 1, &rc(b"c1b"), "2026-05", 0, 2).expect("c1b");
        insert_conv(&conn, &ch2, 1, &rc(b"c2a"), "2026-05", 0, 1).expect("c2a");

        let n = tombstone_conv_up_to_seq(&conn, &[ch1, ch2], 100).expect("tombstone both");
        assert_eq!(n, 3, "all three rows across both channels purged");

        let ch1_feed = content_feed_after(&conn, &ch1, "conv", 0, 100).expect("ch1 feed");
        let ch2_feed = content_feed_after(&conn, &ch2, "conv", 0, 100).expect("ch2 feed");
        assert_eq!(ch1_feed.len(), 2);
        assert_eq!(ch2_feed.len(), 1);
        assert!(ch1_feed.iter().all(|r| r.tombstoned));
        assert!(ch2_feed.iter().all(|r| r.tombstoned));
        let mut ch1_coords: Vec<i64> = ch1_feed.iter().map(|r| r.changed_seq).collect();
        ch1_coords.sort_unstable();
        assert_ne!(
            ch1_coords[0], ch1_coords[1],
            "ch1's two purged rows get distinct coordinates from ch1's own counter"
        );
    }

    // --- conv compaction mirror DAOs (Plan 8 T1) ---

    /// `apply_conv_compaction_tx` tombstones every row across the input
    /// segments and INSERTs one conv row per `NewConvSegmentRecord` into the
    /// new segment, preserving `seq` and leaving the mail-floor columns NULL.
    #[test]
    fn apply_conv_compaction_tx_tombstones_inputs_and_inserts_conv_rows() {
        let conn = open();
        let channel = [0x30u8; 32];
        // Seed two input segments (1, 2) with three conv rows total.
        insert_conv(
            &conn,
            &channel,
            1,
            &rc(b"r1"),
            "2026-05",
            1_715_000_000_000,
            1,
        )
        .expect("r1");
        insert_conv(
            &conn,
            &channel,
            1,
            &rc(b"r2"),
            "2026-05",
            1_715_000_001_000,
            2,
        )
        .expect("r2");
        insert_conv(
            &conn,
            &channel,
            2,
            &rc(b"r3"),
            "2026-05",
            1_715_000_002_000,
            3,
        )
        .expect("r3");

        // Surviving records r1, r3 are rewritten into new segment 5 with their
        // original seq values.
        let new_records = vec![
            NewConvSegmentRecord {
                record_cid: rc(b"r1"),
                bucket: "2026-05".to_string(),
                received_at: 1_715_000_000_000,
                seq: 1,
            },
            NewConvSegmentRecord {
                record_cid: rc(b"r3"),
                bucket: "2026-05".to_string(),
                received_at: 1_715_000_002_000,
                seq: 3,
            },
        ];

        let tx = conn.unchecked_transaction().expect("begin tx");
        apply_conv_compaction_tx(&tx, &channel, &[1, 2], Some(5), &new_records).expect("apply");
        tx.commit().expect("commit");

        // Input rows (seg 1, 2) are all tombstoned.
        for (seg, rid) in &[
            (1u32, b"r1".to_vec()),
            (1u32, b"r2".to_vec()),
            (2u32, b"r3".to_vec()),
        ] {
            let t: i64 = conn
                .query_row(
                    "SELECT tombstoned FROM segment_records
                       WHERE scope_id = ?1 AND kind = 'conv' AND segment_id = ?2 AND record_cid = ?3",
                    params![channel.as_slice(), *seg as i64, &rc(rid).as_bytes()[..]],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(t, 1, "input row (seg {seg}) tombstoned");
        }

        // The live read returns exactly the two survivors, with their seq and
        // NULL mail-floor columns.
        let rows = list_conv_after_seq(&conn, &channel, 0, 100).expect("list");
        assert_eq!(
            rows.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![1, 3],
            "only the rewritten survivors are live"
        );
        assert!(
            rows.iter().all(|r| r.1 == 5),
            "survivors point at new segment 5"
        );

        // Mail-floor columns are NULL on the new rows; seq + received_at set.
        for (rid, want_seq, want_recv) in &[
            (b"r1".to_vec(), 1i64, 1_715_000_000_000i64),
            (b"r3".to_vec(), 3i64, 1_715_000_002_000i64),
        ] {
            let (sender_dom, spam_disp, own, seq, recv): (
                Option<String>,
                Option<String>,
                Option<i64>,
                i64,
                i64,
            ) = conn
                .query_row(
                    "SELECT sender_dom, spam_disp, is_own_submission, seq, received_at
                       FROM segment_records
                      WHERE scope_id = ?1 AND kind = 'conv' AND segment_id = 5 AND record_cid = ?2",
                    params![channel.as_slice(), &rc(rid).as_bytes()[..]],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .unwrap();
            assert!(sender_dom.is_none(), "sender_dom NULL for conv");
            assert!(spam_disp.is_none(), "spam_disp NULL for conv");
            assert!(own.is_none(), "is_own_submission NULL for conv");
            assert_eq!(seq, *want_seq);
            assert_eq!(recv, *want_recv);
        }
    }

    /// `new_segment_id = None` (every input tombstoned, no survivors): the
    /// inputs are still tombstoned but no new row is inserted.
    #[test]
    fn apply_conv_compaction_tx_none_segment_only_tombstones() {
        let conn = open();
        let channel = [0x31u8; 32];
        insert_conv(&conn, &channel, 1, &rc(b"a"), "2026-05", 0, 1).expect("a");
        insert_conv(&conn, &channel, 1, &rc(b"b"), "2026-05", 0, 2).expect("b");

        let tx = conn.unchecked_transaction().expect("begin tx");
        apply_conv_compaction_tx(&tx, &channel, &[1], None, &[]).expect("apply");
        tx.commit().expect("commit");

        // Both inputs tombstoned; nothing live.
        let rows = list_conv_after_seq(&conn, &channel, 0, 100).expect("list");
        assert!(
            rows.is_empty(),
            "all inputs tombstoned, no survivors inserted"
        );
        // No new segment rows inserted at all (only the two original rows exist).
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM segment_records WHERE scope_id = ?1 AND kind = 'conv'",
                params![channel.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 2, "no new rows inserted when new_segment_id is None");
    }

    // ---- headless-part reaper (continuation records) ----

    /// Insert a continuation part mirror row (role=1) at `seq`, with an explicit
    /// **stored_at** (ms) — the reaper's grace keys on when THIS nest stored the
    /// part, never on the message's `received_at`. The two are given deliberately
    /// different values here so a regression that keys on the wrong one shows up
    /// as a failure rather than a coincidence.
    fn insert_part(conn: &Connection, actor: &[u8; 32], cid: &Cid, seq: i64, stored_at_ms: i64) {
        insert_mail(
            conn,
            actor,
            1,
            cid,
            "2026-07",
            // received_at: an ancient message, always. If the reaper ever keys on
            // this again, every "spared" test below turns red.
            1_000_000_000_000,
            "",
            "",
            false,
            seq,
            None,
            1,
            stored_at_ms,
        )
        .expect("insert part");
    }

    /// Insert a live continuation HEAD mirror row (role=2) at `seq`. A head above
    /// a headless part-run is the reaper's proof that the run's own head was never
    /// coming (see [`reap_headless_parts`]).
    fn insert_head(conn: &Connection, actor: &[u8; 32], cid: &Cid, seq: i64) {
        insert_mail(
            conn,
            actor,
            2,
            cid,
            "2026-07",
            1_000_000_000_000,
            "",
            "",
            false,
            seq,
            None,
            2,
            aged_ms(),
        )
        .expect("insert head");
    }

    /// Insert a normal (role=0) mail record at `seq` — e.g. a local append that
    /// interleaves between a relayed family's parts and its head.
    fn insert_normal(conn: &Connection, actor: &[u8; 32], cid: &Cid, seq: i64) {
        insert_mail(
            conn,
            actor,
            1,
            cid,
            "2026-07",
            aged_ms(),
            "x.com",
            "accept",
            false,
            seq,
            None,
            0,
            aged_ms(),
        )
        .expect("insert normal");
    }

    fn ms_now() -> i64 {
        1_800_000_000_000 // a fixed "now" far past any watermark math
    }
    fn aged_ms() -> i64 {
        ms_now() - (ORPHAN_REAP_MIN_AGE_SECS + 60) * 1000 // comfortably past the watermark
    }

    #[test]
    fn reap_headless_parts_tombstones_an_aged_unreferenced_part() {
        let conn = open();
        let actor = [0x21u8; 32];
        let orphan = rc(b"orphan-part");
        insert_part(&conn, &actor, &orphan, 1, aged_ms());
        // A later live head proves this part's own head was never coming (a
        // crashed local family write) — without it the part is spared as
        // "awaiting its head", see the relay-stall test below.
        insert_head(&conn, &actor, &rc(b"later-head"), 2);
        // No live head references it → reaped.
        let reaped = reap_headless_parts(&conn, &actor, ms_now(), &HashSet::new()).expect("reap");
        assert_eq!(reaped, 1);
        assert!(
            lookup_record(&conn, &actor, "mail", &orphan)
                .expect("ok")
                .is_none(),
            "reaped part is tombstoned (hidden from lookup)"
        );
    }

    /// **The mid-family-ack data-loss regression**. A continuation family always spans relay pull pages (parts are
    /// 1 MiB, the page budget is one 2 MiB frame minus headroom), so the relay
    /// acks mid-family and the source purges the parts it just handed over. The
    /// destination therefore holds a *headless* part-run whose head is still in
    /// flight. If a relay stall outlasts the grace and a compaction pass lands in
    /// the gap, reaping that run loses the body user-irrecoverably (the head
    /// arrives later and its join finds no parts; the source already purged).
    ///
    /// So: aged + unreferenced is **not** sufficient. A part is only reapable once
    /// a live head at a HIGHER seq proves its own head was never coming.
    #[test]
    fn reap_headless_parts_spares_an_aged_part_whose_head_has_not_relayed_yet() {
        let conn = open();
        let actor = [0x28u8; 32];
        let awaiting = rc(b"part-awaiting-its-head");
        // Aged past the grace (a long relay stall) and referenced by no live head
        // — under the pre-fix rule this was reaped, losing the body.
        insert_part(&conn, &actor, &awaiting, 1, aged_ms());
        let reaped = reap_headless_parts(&conn, &actor, ms_now(), &HashSet::new()).expect("reap");
        assert_eq!(
            reaped, 0,
            "a headless part with no head above it is awaiting its head — never reap it"
        );
        assert!(
            lookup_record(&conn, &actor, "mail", &awaiting)
                .expect("ok")
                .is_some(),
            "the part survives the stall so the head's join can still rejoin the body"
        );
    }

    /// A *normal* record above a headless part-run is **not** proof the run is
    /// orphaned. Relayed records are appended one at a time (no family-spanning
    /// lock), so a local append can interleave between a family's parts and its
    /// head — only a HEAD above proves the run's own head was skipped.
    #[test]
    fn reap_headless_parts_spares_a_part_when_only_a_normal_record_sits_above_it() {
        let conn = open();
        let actor = [0x29u8; 32];
        let awaiting = rc(b"part-under-a-local-append");
        insert_part(&conn, &actor, &awaiting, 1, aged_ms());
        // A local append landed between the relayed parts and their (still
        // in-flight) head. It is role=0, so it proves nothing.
        insert_normal(&conn, &actor, &rc(b"interleaved-local-record"), 2);
        let reaped = reap_headless_parts(&conn, &actor, ms_now(), &HashSet::new()).expect("reap");
        assert_eq!(
            reaped, 0,
            "a normal record above a part-run is not proof the run's head was skipped"
        );
    }

    /// The head-above proof is **seq-ordered**: a live head *below* the part-run
    /// (an earlier family's head) says nothing about this run.
    #[test]
    fn reap_headless_parts_spares_a_part_whose_only_head_sits_below_it() {
        let conn = open();
        let actor = [0x2au8; 32];
        let awaiting = rc(b"part-above-an-older-head");
        insert_head(&conn, &actor, &rc(b"older-family-head"), 1);
        insert_part(&conn, &actor, &awaiting, 2, aged_ms());
        let reaped = reap_headless_parts(&conn, &actor, ms_now(), &HashSet::new()).expect("reap");
        assert_eq!(
            reaped, 0,
            "an earlier family's head is not proof about a later part-run"
        );
    }

    #[test]
    fn reap_headless_parts_spares_a_referenced_part() {
        let conn = open();
        let actor = [0x22u8; 32];
        let referenced = rc(b"live-part");
        insert_part(&conn, &actor, &referenced, 1, aged_ms());
        // A live head references it → spared, even though aged.
        let live: HashSet<Cid> = [referenced].into_iter().collect();
        let reaped = reap_headless_parts(&conn, &actor, ms_now(), &live).expect("reap");
        assert_eq!(reaped, 0);
        assert!(
            lookup_record(&conn, &actor, "mail", &referenced)
                .expect("ok")
                .is_some(),
            "referenced part survives"
        );
    }

    #[test]
    fn reap_headless_parts_spares_a_young_part() {
        let conn = open();
        let actor = [0x23u8; 32];
        let young = rc(b"young-part");
        // stored_at ≈ now → under the age watermark (the in-flight-write guard).
        // Note `insert_part` gives it an ancient `received_at`: this part models
        // relayed historical mail, which is exactly the case that used to be
        // reaped on arrival and lose the body before its head landed.
        insert_part(&conn, &actor, &young, 1, ms_now() - 1000);
        // A later head, so the part is a candidate on every rule *except* age —
        // this test must stay red-able by the age watermark alone.
        insert_head(&conn, &actor, &rc(b"young-test-later-head"), 2);
        let reaped = reap_headless_parts(&conn, &actor, ms_now(), &HashSet::new()).expect("reap");
        assert_eq!(
            reaped, 0,
            "a young orphan part is spared by the age watermark"
        );
        assert!(
            lookup_record(&conn, &actor, "mail", &young)
                .expect("ok")
                .is_some()
        );
    }

    #[test]
    fn reap_headless_parts_spares_a_part_with_an_unknown_stored_at() {
        let conn = open();
        let actor = [0x25u8; 32];
        let unknown = rc(b"unknown-stored-at-part");
        // A row whose append-time clock read failed (or a floor whose stored_at is
        // the 0 = unknown default) mirrors as NULL. Unknown must never be treated as "ancient" —
        // the reaper errs toward leaking a part, never toward losing one.
        insert_mail(
            &conn,
            &actor,
            1,
            &unknown,
            "2026-07",
            1_000_000_000_000,
            "",
            "",
            false,
            1,
            None,
            1,
            0, // stored_at = 0 -> NULL
        )
        .expect("insert part");
        let reaped = reap_headless_parts(&conn, &actor, ms_now(), &HashSet::new()).expect("reap");
        assert_eq!(reaped, 0, "a NULL (unknown) stored_at is never reapable");
        assert!(
            lookup_record(&conn, &actor, "mail", &unknown)
                .expect("ok")
                .is_some()
        );
    }

    #[test]
    fn reap_headless_parts_never_touches_a_normal_record() {
        let conn = open();
        let actor = [0x24u8; 32];
        let normal = rc(b"normal-record");
        // A NORMAL (role=0) aged record must never be reaped by the part reaper,
        // even with an empty live set — the reaper filters on continuation_role=1.
        insert_mail(
            &conn,
            &actor,
            1,
            &normal,
            "2026-07",
            aged_ms(),
            "x.com",
            "accept",
            false,
            1,
            None,
            0,
            aged_ms(),
        )
        .expect("insert normal");
        let reaped = reap_headless_parts(&conn, &actor, ms_now(), &HashSet::new()).expect("reap");
        assert_eq!(reaped, 0, "the part reaper must ignore normal records");
        assert!(
            lookup_record(&conn, &actor, "mail", &normal)
                .expect("ok")
                .is_some()
        );
    }

    #[test]
    fn reap_headless_parts_is_scope_scoped() {
        let conn = open();
        let a1 = [0x25u8; 32];
        let a2 = [0x26u8; 32];
        let p1 = rc(b"a1-orphan");
        let p2 = rc(b"a2-orphan");
        insert_part(&conn, &a1, &p1, 1, aged_ms());
        insert_part(&conn, &a2, &p2, 1, aged_ms());
        // Each actor gets a later head, so both parts are genuine orphans and
        // only the scope filter decides which is reaped.
        insert_head(&conn, &a1, &rc(b"a1-later-head"), 2);
        insert_head(&conn, &a2, &rc(b"a2-later-head"), 2);
        // Reaping a1 must not touch a2's parts.
        let reaped = reap_headless_parts(&conn, &a1, ms_now(), &HashSet::new()).expect("reap");
        assert_eq!(reaped, 1);
        assert!(
            lookup_record(&conn, &a2, "mail", &p2)
                .expect("ok")
                .is_some(),
            "a2's orphan part is untouched"
        );
    }

    #[test]
    fn list_live_continuation_heads_returns_only_live_heads() {
        let conn = open();
        let actor = [0x27u8; 32];
        let head = rc(b"the-head");
        let part = rc(b"a-part");
        let normal = rc(b"a-normal");
        // head (role=2), part (role=1), normal (role=0).
        insert_mail(
            &conn,
            &actor,
            3,
            &head,
            "2026-07",
            aged_ms(),
            "x",
            "accept",
            false,
            9,
            None,
            2,
            aged_ms(),
        )
        .expect("head");
        insert_part(&conn, &actor, &part, 1, aged_ms());
        insert_mail(
            &conn,
            &actor,
            1,
            &normal,
            "2026-07",
            aged_ms(),
            "x",
            "accept",
            false,
            1,
            None,
            0,
            aged_ms(),
        )
        .expect("normal");
        let heads = list_live_continuation_heads(&conn, &actor).expect("list");
        assert_eq!(heads.len(), 1, "only the role=2 head is returned");
        assert_eq!(heads[0], (3, head));
        // Tombstoning the head drops it from the live-head list.
        mark_tombstoned(&conn, &actor, "mail", 3, &head).expect("tombstone");
        assert!(
            list_live_continuation_heads(&conn, &actor)
                .expect("list2")
                .is_empty()
        );
    }
}
