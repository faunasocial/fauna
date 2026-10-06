//! Shared DB-layer primitives for the CalDAV and CardDAV bridge planes.

use anyhow::{Context, Result};
use blake3::Hasher;
use tokio::sync::Mutex;

use crate::domain_hash::{HashField, write_fields};

/// `id = blake3(domain_tag || actor || timestamp_le_i64 || encrypted_body)`.
/// Domain-tagged so distinct call paths can never share an id, even on
/// byte-identical bodies. Length-prefix the domain tag so two tags of
/// different lengths but a coincidentally-shared concatenation can't collide.
/// Callers keep their own typed, documented wrapper (`derive_caldav_event_id`,
/// `derive_carddav_card_id`) over their own domain-tag constant.
pub(crate) fn derive_dav_content_id(
    domain_tag: &[u8],
    actor: &[u8; 32],
    timestamp: i64,
    encrypted_body: &[u8],
) -> [u8; 32] {
    let mut h = Hasher::new();
    write_fields(
        domain_tag,
        &[
            HashField::Fixed32(actor),
            HashField::I64(timestamp),
            HashField::Trailing(encrypted_body),
        ],
        |b| {
            h.update(b);
        },
    );
    *h.finalize().as_bytes()
}

/// Format a `modseq` as the hex ETag both DAV planes serve.
pub(crate) fn format_etag(modseq: i64) -> String {
    format!("{:016x}", modseq)
}

/// Run one paginated scope query, shared by `query_caldav_events` and
/// `query_carddav_cards`: dispatch the `(since, after)` optional-filter
/// combination to the correctly-typed `rusqlite::params!` (the match exists
/// because the macro needs the param count and types at compile time per
/// arm), execute, and apply the `limit+1` pagination-detection trim.
///
/// Callers build their own SQL string and keep their own table/column names
/// as compile-time literals — this function never interpolates a name into
/// SQL, only binds the two fixed positional filters every DAV scope query
/// takes (`since_modseq`, `after_id`) after the caller's own `actor_id`/
/// scope-id `WHERE` prefix (bound here as `?1`/`?2`). `sql` here stays
/// `&str`, checked at runtime by the `debug_assert!` below; where a helper's
/// callers truly never build the SQL at runtime, prefer `&'static str`
/// instead — a compile-time guarantee, as `reports::share_pref_enabled`/
/// `set_share_pref` do.
pub(crate) async fn paged_dav_query<R>(
    conn: &Mutex<rusqlite::Connection>,
    sql: &str,
    actor: &[u8; 32],
    scope_id: &[u8; 32],
    since_param: Option<i64>,
    after_param: Option<[u8; 32]>,
    fetch_limit: u32,
    map_row: fn(&rusqlite::Row<'_>) -> rusqlite::Result<R>,
    op_name: &str,
) -> Result<(Vec<R>, bool)> {
    // The tenant boundary this function serves rests entirely on the caller's
    // own SQL prefix binding `actor_id` as `?1` (doc comment above) — this is
    // the one cheap check turning that prose contract into something a third
    // caller can't silently drop. Debug-only: a whitespace-sensitive substring
    // match must never become a release-path panic.
    debug_assert!(
        sql.contains("actor_id = ?1"),
        "{op_name}: SQL must bind actor_id as ?1 — the tenant boundary for this query: {sql}"
    );
    let conn = conn.lock().await;
    let mut stmt = conn
        .prepare(sql)
        .with_context(|| format!("prepare {op_name}"))?;

    let mut rows: Vec<R> = match (since_param, after_param) {
        (Some(s), Some(a)) => stmt
            .query_map(
                rusqlite::params![&actor[..], &scope_id[..], s, &a[..]],
                map_row,
            )
            .with_context(|| format!("{op_name} (since+after)"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .with_context(|| format!("collect {op_name} (since+after)"))?,
        (Some(s), None) => stmt
            .query_map(rusqlite::params![&actor[..], &scope_id[..], s], map_row)
            .with_context(|| format!("{op_name} (since)"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .with_context(|| format!("collect {op_name} (since)"))?,
        (None, Some(a)) => stmt
            .query_map(
                rusqlite::params![&actor[..], &scope_id[..], &a[..]],
                map_row,
            )
            .with_context(|| format!("{op_name} (after)"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .with_context(|| format!("collect {op_name} (after)"))?,
        (None, None) => stmt
            .query_map(rusqlite::params![&actor[..], &scope_id[..]], map_row)
            .with_context(|| format!("{op_name} (base)"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .with_context(|| format!("collect {op_name} (base)"))?,
    };

    let more = fetch_limit > 0 && rows.len() as u32 == fetch_limit;
    if more {
        rows.pop();
    }

    Ok((rows, more))
}

/// The `(segment_id, record_cid)` refs of an actor's live calendar and card
/// content records — their share of the one storage budget mail draws on
/// (`caldav-server.md` § QUOTA — shared with IMAP). Each list is sized through
/// its own kind's segment manager (`segments::record_sizes` over
/// `cal_segments` / `card_segments`), exactly as the mail refs are.
#[derive(Debug, Default)]
pub(crate) struct DavQuotaSizeRefs {
    pub calendar: Vec<(u32, fauna_cbor::Cid)>,
    pub card: Vec<(u32, fauna_cbor::Cid)>,
}

impl super::CacheDb {
    /// Every live `bridge_caldav_events` / `bridge_carddav_cards` row's content
    /// record, as `(segment_id, record_cid)` off the `segment_records` mirror —
    /// the calendar/card half of `imap_quota_usage`'s STORAGE sum. A row's
    /// record resolves through its stored `record_cid` (the identity is not
    /// re-derivable from `event_id`/`card_id`), so a NULL-cid row — possible
    /// only mid additive reconcile — joins nothing and counts nothing, and a
    /// tombstoned record is excluded exactly as mail's is. One lock scope for
    /// both reads, so the two halves describe one instant.
    pub(crate) async fn list_bridge_dav_quota_size_refs(
        &self,
        actor: &[u8; 32],
    ) -> Result<DavQuotaSizeRefs> {
        let actor = *actor;
        let conn = self.conn.lock().await;
        let read = |sql: &str, what: &str| -> Result<Vec<(u32, fauna_cbor::Cid)>> {
            let mut stmt = conn
                .prepare(sql)
                .with_context(|| format!("prepare {what}"))?;
            let rows = stmt
                .query_map(rusqlite::params![&actor[..]], |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u32,
                        super::bridge_imap::cid_from_col(row.get::<_, Vec<u8>>(1)?, 1)?,
                    ))
                })
                .with_context(|| format!("query {what}"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .with_context(|| format!("collect {what}"))?;
            Ok(rows)
        };
        let calendar = read(
            "SELECT sr.segment_id, sr.record_cid \
             FROM bridge_caldav_events e \
             JOIN segment_records sr \
               ON sr.kind = 'calendar' \
              AND sr.scope_id = e.actor_id \
              AND sr.record_cid = e.record_cid \
              AND sr.tombstoned = 0 \
             WHERE e.actor_id = ?1",
            "dav quota refs (calendar)",
        )?;
        let card = read(
            "SELECT sr.segment_id, sr.record_cid \
             FROM bridge_carddav_cards c \
             JOIN segment_records sr \
               ON sr.kind = 'card' \
              AND sr.scope_id = c.actor_id \
              AND sr.record_cid = c.record_cid \
              AND sr.tombstoned = 0 \
             WHERE c.actor_id = ?1",
            "dav quota refs (card)",
        )?;
        Ok(DavQuotaSizeRefs { calendar, card })
    }
}
