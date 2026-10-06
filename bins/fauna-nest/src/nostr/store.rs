//! The persistent, owner-scoped NIP-01 relay event store.
//!
//! Backs the real relay (`docs/goal/ui/nostr.md` § The relay event store,
//! ratified 2026-07-13). Two classes of event land here:
//!
//! 1. **Native events** written via the `/nostr` EVENT verb by a local-account
//!    pubkey (the owner-only *authored-by* scope — slice A; the *addressed-to*
//!    gift-wrap inbox is slice B).
//! 2. **Materialized Fauna-post translations** — an exposed Fauna post is
//!    translated + signed **once** at the outbound-sync signing position and
//!    stored like any other event (deduped via `nostr_event_map`), so the relay
//!    serves signed rows from the store instead of re-signing per REQ. The relay
//!    never emits an unsigned event.
//!
//! At rest, public Nostr events are readable class 1 (signed, world-readable by
//! construction — `docs/goal/architecture/encryption-at-rest.md` § Per-content
//! -kind conformance, "Nostr relay event store"). The columns mirror the NIP-01
//! filter surface; `raw_json` carries the full signed wire event verbatim.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use fauna_bridge_nostr::filter::matches_any;
use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::{Event, Filter};
use fauna_core::data::Post;
use fauna_segment_store::SegmentManager;

use crate::db::CacheDb;

use super::db;

/// Maximum accepted size of one event's wire JSON, in bytes. Re-exported from
/// [`fauna_bridge_nostr::nip11`] so the NIP-11 `max_message_length`
/// advertisement and the EVENT-size enforcement here share one source of truth.
pub use fauna_bridge_nostr::nip11::MAX_EVENT_SIZE;

/// Default number of events a REQ returns when the filter sets no `limit`.
pub const DEFAULT_QUERY_LIMIT: usize = 500;

/// Hard ceiling on the events one REQ returns, regardless of a filter's
/// `limit`. Bounds a single subscription's fan-out.
pub const MAX_QUERY_LIMIT: usize = 5_000;

/// Hard ceiling on native events stored for one pubkey (author-scope writes).
/// A gift wrap uses a fresh random pubkey per NIP-59 send, so this does
/// **not** bound the inbox — [`MAX_GIFT_WRAP_INBOX_PER_RECIPIENT`] does. A
/// hard-coded Rust constant — no standard NIP-11 field advertises it, and it
/// is never a configuration surface.
pub const MAX_EVENTS_PER_ACCOUNT: usize = 100_000;

/// Hard ceiling on the whole relay event store, across every account —
/// bounds worst-case disk regardless of any one account's behavior. A
/// hard-coded Rust constant, never a configuration surface.
pub const MAX_STORE_EVENTS: usize = 2_000_000;

/// Hard per-recipient ceiling on stored kind-1059 (NIP-17) gift wraps — the
/// *total* bound the unauthenticated inbox's rate limit
/// ([`relay_endpoint::GIFT_WRAP_INBOX_PER_RECIPIENT_PER_MINUTE`](super::relay_endpoint::GIFT_WRAP_INBOX_PER_RECIPIENT_PER_MINUTE))
/// does **not** provide by itself: a sustained flood at the rate cap still
/// grows a recipient's inbox without bound, since [`MAX_EVENTS_PER_ACCOUNT`]
/// never engages (gift wraps carry a fresh random sender pubkey per send —
/// there is no stable "account" to count against). Checked in
/// [`relay_endpoint::handle_gift_wrap_inbox`](super::relay_endpoint::handle_gift_wrap_inbox)
/// via [`count_gift_wraps_for_recipient`], keyed on the `p`-tag recipient —
/// the actual owned resource, same keying as the rate limiter. A hard-coded
/// Rust constant, never a configuration surface.
pub const MAX_GIFT_WRAP_INBOX_PER_RECIPIENT: usize = 50_000;

/// Internal per-filter row-scan ceiling for [`count_events`] — bounds a
/// COUNT's worst-case DB scan. A filter matching more rows than this returns
/// the ceiling as an approximate count (`CountResult::approximate`) rather
/// than scanning unboundedly; generous at single-nest scale.
pub const MAX_COUNT_SCAN: usize = 50_000;

/// `nostr_events.origin` provenance value for a row that arrived via **local**
/// relay/materialization/sweep — the default for every write on this box's own
/// serving/agent paths (`nostr.md` § The bridging gate → Phase 2, R4 (account-data-plane.md § The ratified decisions)). The
/// federation legs select exactly these rows to push/pull.
pub const ORIGIN_INGEST: &str = "ingest";

/// `nostr_events.origin` provenance value for a row that arrived over a Phase-2
/// Nostr **federation** leg. Never re-exported by a federation leg — this is
/// what kills echo (a pushed row can't be pulled back) and loops.
pub const ORIGIN_FEDERATION: &str = "federation";

/// NIP-01 kind classes that govern an event's storage lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindClass {
    /// Stored as-is; many rows may share a `(pubkey, kind)`.
    Regular,
    /// Replaced per `(pubkey, kind)` — kinds 0, 3, 10000–19999.
    Replaceable,
    /// Replaced per `(pubkey, kind, d-tag)` — kinds 30000–39999.
    ParamReplaceable,
    /// Broadcast to live subscribers, never stored — kinds 20000–29999.
    Ephemeral,
}

/// Classify a kind per the NIP-01 ranges.
pub fn classify_kind(kind: u64) -> KindClass {
    match kind {
        0 | 3 | 10_000..=19_999 => KindClass::Replaceable,
        20_000..=29_999 => KindClass::Ephemeral,
        30_000..=39_999 => KindClass::ParamReplaceable,
        _ => KindClass::Regular,
    }
}

/// The replacement key for a (parameterized-)replaceable event: `kind:pubkey`
/// for replaceable kinds, `kind:pubkey:dtag` for parameterized-replaceable
/// (an absent `d` tag is the empty string, per NIP-01). `None` for regular and
/// ephemeral kinds, which are never replaced.
pub fn replace_key(event: &Event) -> Option<String> {
    match classify_kind(event.kind) {
        KindClass::Replaceable => Some(format!("{}:{}", event.kind, event.pubkey)),
        KindClass::ParamReplaceable => {
            let d = d_tag_value(event).unwrap_or("");
            Some(format!("{}:{}:{}", event.kind, event.pubkey, d))
        }
        KindClass::Regular | KindClass::Ephemeral => None,
    }
}

fn d_tag_value(event: &Event) -> Option<&str> {
    event
        .tags
        .iter()
        .find(|t| t.name() == Some("d"))
        .and_then(|t| t.value())
}

/// The NIP-40 `expiration` tag value, as a Unix timestamp — `None` if absent
/// or unparseable (an unparseable expiration is treated as no expiration,
/// not a rejection; NIP-40 leaves malformed tags to relay discretion and a
/// non-expiring event is the fail-open-to-visible choice, matching every
/// other event that carries no expiration).
pub(crate) fn expiration_value(event: &Event) -> Option<i64> {
    event
        .tags
        .iter()
        .find(|t| t.name() == Some("expiration"))
        .and_then(|t| t.value())
        .and_then(|v| v.parse::<i64>().ok())
}

/// The outcome of a [`store_event`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    /// A new event was persisted.
    Stored,
    /// A (parameterized-)replaceable event superseded an older one, which was
    /// removed.
    Replaced,
    /// The event's id is already stored; nothing changed (NIP-01 `duplicate:`).
    Duplicate,
    /// A (parameterized-)replaceable event was rejected: a newer one for the
    /// same `(pubkey, kind[, d])` is already stored.
    Superseded,
    /// An ephemeral kind — broadcast only, never persisted.
    Ephemeral,
    /// Rejected: the event's NIP-40 `expiration` tag is already in the past.
    Expired,
    /// Rejected: [`MAX_EVENTS_PER_ACCOUNT`] or [`MAX_STORE_EVENTS`] reached.
    CapExceeded,
}

impl StoreOutcome {
    /// Whether this outcome means the event newly entered the store (and so
    /// should be broadcast to live subscribers).
    pub fn is_newly_stored(self) -> bool {
        matches!(self, StoreOutcome::Stored | StoreOutcome::Replaced)
    }
}

/// Persist an event per NIP-01 lifecycle semantics. Ephemeral kinds are never
/// stored; a duplicate id is a no-op; a (parameterized-)replaceable event
/// replaces the older row for its `(pubkey, kind[, d])` (keeping the greater
/// `created_at`; ties broken by the lexicographically-lower id, per NIP-01).
///
/// `derived` marks a row materialized from a Fauna post — recreatable, so it is
/// deletable on `expose_content` toggle-off ([`delete_derived_for_pubkey`])
/// without violating no-data-loss.
///
/// The caller is responsible for the owner-only *scope* gate (only events whose
/// pubkey is a local account are offered here, in slice A) and for verifying the
/// signature before calling.
///
/// Records the row as [`ORIGIN_INGEST`] — arrived via a local relay/
/// materialization/sweep path. A row arriving over a Phase-2 federation leg is
/// stored via [`store_event_with_origin`] with [`ORIGIN_FEDERATION`] instead.
pub fn store_event(conn: &Connection, event: &Event, derived: bool) -> Result<StoreOutcome> {
    store_event_with_origin(conn, event, derived, ORIGIN_INGEST)
}

/// [`store_event`] with an explicit `origin` provenance
/// ([`ORIGIN_INGEST`]/[`ORIGIN_FEDERATION`], `nostr.md` § The bridging gate →
/// Phase 2, R4). Every store invariant (dedup, replaceable-supersede, caps,
/// FTS-exclusion, tag-indexing, NIP-40) is identical — `origin` only stamps how
/// the row entered *this* box's store, so the federation legs can select their
/// own `ORIGIN_INGEST` rows and never re-export a federation-arrived one.
pub fn store_event_with_origin(
    conn: &Connection,
    event: &Event,
    derived: bool,
    origin: &str,
) -> Result<StoreOutcome> {
    if classify_kind(event.kind) == KindClass::Ephemeral {
        return Ok(StoreOutcome::Ephemeral);
    }

    let now = crate::db::now_epoch_secs();
    let expiration = expiration_value(event);
    // NIP-40: an already-expired event is rejected outright, ahead of the
    // duplicate/replace checks — an expired id is never worth persisting,
    // even if it would otherwise be a fresh replaceable version.
    if let Some(exp) = expiration
        && exp <= now
    {
        return Ok(StoreOutcome::Expired);
    }

    let tx = conn.unchecked_transaction()?;

    // Duplicate by id?
    let exists: bool = tx
        .prepare("SELECT EXISTS(SELECT 1 FROM nostr_events WHERE id = ?1)")?
        .query_row([&event.id], |r| r.get::<_, i64>(0))
        .map(|c| c > 0)?;
    if exists {
        tx.commit()?;
        return Ok(StoreOutcome::Duplicate);
    }

    let rkey = replace_key(event);
    let raw_json = serde_json::to_string(event)?;

    if let Some(ref rk) = rkey {
        let existing: Option<(String, i64)> = tx
            .prepare("SELECT id, created_at FROM nostr_events WHERE replace_key = ?1")?
            .query_row([rk], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .optional()?;
        if let Some((existing_id, existing_created)) = existing {
            let new_created = event.created_at as i64;
            // NIP-01: keep the greater created_at; on a tie keep the lower id.
            let supersede = new_created > existing_created
                || (new_created == existing_created && event.id < existing_id);
            if !supersede {
                tx.commit()?;
                return Ok(StoreOutcome::Superseded);
            }
            // A replace nets zero rows (delete then insert), so it never
            // grows the store — no cap check needed on this path.
            delete_event_row(&tx, &existing_id)?;
            insert_event_row(
                &tx,
                event,
                &raw_json,
                rkey.as_deref(),
                derived,
                expiration,
                now,
                origin,
            )?;
            tx.commit()?;
            // The delete above fired the removal trigger for the superseded
            // row; index the replacement now that the write is durable.
            index_into_search_corpus(conn, event, derived, None);
            return Ok(StoreOutcome::Replaced);
        }
    }

    // Every path below is a genuinely new row — the only place the store
    // caps need checking.
    if store_caps_exceeded(&tx, &event.pubkey)? {
        tx.commit()?;
        return Ok(StoreOutcome::CapExceeded);
    }

    insert_event_row(
        &tx,
        event,
        &raw_json,
        rkey.as_deref(),
        derived,
        expiration,
        now,
        origin,
    )?;
    tx.commit()?;
    index_into_search_corpus(conn, event, derived, None);
    Ok(StoreOutcome::Stored)
}

/// Whether an event kind may enter the Fauna **Search corpus** (`content_fts`).
///
/// The policy's class split is what this encodes: a bridge's *public* content
/// is eligible, its *private* content is never (`content-index.md` § Bridge
/// content in the Search corpus). So both DM carriers are excluded at index
/// time, the same way `nostr_event_fts` excludes gift wraps — the search plane
/// stays structurally incapable of touching DM payloads rather than relying on
/// a serving-side filter:
///
/// * **1059** — NIP-59 gift wrap (NIP-17 DMs).
/// * **4** — the legacy NIP-04 encrypted DM.
///
/// Machine kinds carry no readable prose and are excluded as noise rather than
/// as a privacy matter: **0** (metadata), **3** (contact list), **5**
/// (deletion request), **7** (reaction), **9735** (zap receipt).
pub fn is_search_corpus_kind(kind: u64) -> bool {
    !matches!(kind, 0 | 3 | 4 | 5 | 7 | 1059 | 9735) && classify_kind(kind) != KindClass::Ephemeral
}

/// Index a just-stored event into the bridge Search corpus, if the policy is on
/// and the event qualifies.
///
/// **Only foreign-authored inbound content indexes.** `derived` rows are
/// materializations of the user's own Fauna posts, whose bodies are already in
/// `content_fts` as the post itself — excluding them here is what makes the
/// policy's no-double-surfacing rule structural rather than a later filter.
///
/// Removal needs no counterpart call: the `nostr_events_bridge_search_ad`
/// trigger (`nostr/db.rs`) drops the indexed row on *every* delete of the store
/// row — NIP-01 replacement, NIP-09 deletion, NIP-40 expiry sweep, and any
/// future path — so the corpus stays in lockstep by construction.
///
/// **Shared with the sweep plane** (`nostr::inbound_lifecycle`), deliberately:
/// calling the same function from both nostr transit points is what makes "an
/// event seen by both paths indexes once" true by construction rather than by
/// two call sites agreeing on the key, the body and the timestamp. The sweep's
/// own removal arm is the `nostr_event_map_bridge_search_ad` trigger, the twin
/// of the one named above.
///
/// `post_id` is the post the sweep rested for the event — the link the feed's
/// text filters follow (`feed.md` § The read model → *The list-card preview*
/// → Corollary). The relay store rests no post and passes `None`, which keeps
/// a link the sweep already wrote for the same event.
pub(crate) fn index_into_search_corpus(
    conn: &Connection,
    event: &Event,
    derived: bool,
    post_id: Option<&[u8; 32]>,
) {
    if derived || !is_search_corpus_kind(event.kind) {
        return;
    }
    if let Err(e) = crate::db::bridge_search::index_bridge_content(
        conn,
        "nostr",
        &event.id,
        &event.pubkey,
        &event.content,
        // `content_fts_map.created_at` is epoch MICROSECONDS (`db/schema.rs`),
        // the unit it must share with `content.created_at` because the search
        // window compares `COALESCE(c.created_at, m.created_at)`. A nostr
        // event's own `created_at` is NIP-01 seconds, so it scales here.
        event.created_at as i64 * 1_000_000,
        post_id,
    ) {
        // Never fail the store on an indexing problem: the event itself is the
        // durable thing, the search row is derived and re-creatable.
        tracing::warn!(
            target: "nostr_store",
            event_id = %event.id,
            error = %e,
            "bridge search indexing failed"
        );
    }
}

/// Whether storing one more row for `pubkey` would breach
/// [`MAX_EVENTS_PER_ACCOUNT`] or [`MAX_STORE_EVENTS`].
fn store_caps_exceeded(conn: &Connection, pubkey: &str) -> Result<bool> {
    store_caps_exceeded_with_limits(conn, pubkey, MAX_EVENTS_PER_ACCOUNT, MAX_STORE_EVENTS)
}

/// [`store_caps_exceeded`] parameterized on the two limits, so tests can
/// exercise the exact boundary without inserting hundreds of thousands of
/// rows. Production always calls it via [`store_caps_exceeded`] with the
/// real hard-coded constants — this split changes nothing about what ships.
fn store_caps_exceeded_with_limits(
    conn: &Connection,
    pubkey: &str,
    per_account_limit: usize,
    total_limit: usize,
) -> Result<bool> {
    let per_account: i64 = conn
        .prepare("SELECT COUNT(*) FROM nostr_events WHERE pubkey = ?1")?
        .query_row([pubkey], |r| r.get(0))?;
    if per_account as usize >= per_account_limit {
        return Ok(true);
    }
    let total: i64 = conn
        .prepare("SELECT COUNT(*) FROM nostr_events")?
        .query_row([], |r| r.get(0))?;
    Ok(total as usize >= total_limit)
}

#[allow(clippy::too_many_arguments)]
fn insert_event_row(
    conn: &Connection,
    event: &Event,
    raw_json: &str,
    replace_key: Option<&str>,
    derived: bool,
    expiration: Option<i64>,
    now: i64,
    origin: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO nostr_events
             (id, pubkey, kind, created_at, raw_json, replace_key, derived, stored_at, expiration, origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            event.id,
            event.pubkey,
            event.kind as i64,
            event.created_at as i64,
            raw_json,
            replace_key,
            derived as i64,
            now,
            expiration,
            origin,
        ],
    )?;
    // Index single-letter tags (`e`/`p`/`d`/…) — the NIP-01 filterable set.
    for tag in &event.tags {
        if let (Some(name), Some(value)) = (tag.name(), tag.value())
            && is_single_letter(name)
        {
            conn.execute(
                "INSERT OR IGNORE INTO nostr_event_tags (event_id, name, value)
                 VALUES (?1, ?2, ?3)",
                params![event.id, name, value],
            )?;
        }
    }
    Ok(())
}

fn delete_event_row(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM nostr_event_tags WHERE event_id = ?1", [id])?;
    conn.execute("DELETE FROM nostr_events WHERE id = ?1", [id])?;
    Ok(())
}

/// Delete one stored event by id — the key-less arm of post-delete
/// propagation (`nostr::propagate_post_delete`): the author's nsec is gone,
/// so no kind-5 can be signed, but a derived row must still not outlive its
/// Fauna post. Author scoping is the caller's obligation (it derives the ids
/// from the post's own `nostr_event_map` rows). FTS stays in trigger lockstep.
pub fn delete_event_by_id(conn: &Connection, id: &str) -> Result<()> {
    delete_event_row(conn, id)
}

/// The stored author (`pubkey`) of the event `id`, or `None` if this box does
/// not hold it. A point lookup on the `nostr_events` primary key.
///
/// The NIP-57 zap subject binding (`monetization.md` § Zap receipts — the
/// trust model) reads this to refuse a receipt attributing a zap to an event
/// the `p`-tagged payee did not author. A store-read error surfaces to the
/// caller, which fails closed (treats it as not-held) — the alternative is
/// believing a receipt we could not check.
pub fn event_author(conn: &Connection, id: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT pubkey FROM nostr_events WHERE id = ?1")?;
    Ok(stmt
        .query_row([id], |row| row.get::<_, String>(0))
        .optional()?)
}

fn is_single_letter(name: &str) -> bool {
    name.len() == 1 && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// Serve a REQ from the store. Runs each filter's index-friendly conditions in
/// SQL (ids/authors prefix-capable, kinds/since/until, single-letter tag
/// filters), unions the results (NIP-01 OR semantics), dedupes by id, and
/// returns newest-first up to `overall_cap`. The caller applies the
/// authoritative [`fauna_bridge_nostr::filter::matches_any`] as the final
/// in-memory check (it re-checks prefix matching and any non-indexed tag).
pub fn query_events(
    conn: &Connection,
    filters: &[Filter],
    overall_cap: usize,
) -> Result<Vec<Event>> {
    let cap = overall_cap.clamp(1, MAX_QUERY_LIMIT);
    let mut collected: Vec<Event> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    if filters.is_empty() {
        for e in run_filter_query(conn, &Filter::default(), cap)? {
            if seen.insert(e.id.clone()) {
                collected.push(e);
            }
        }
    } else {
        for f in filters {
            let limit = f
                .limit
                .map(|l| l as usize)
                .unwrap_or(DEFAULT_QUERY_LIMIT)
                .clamp(1, cap);
            for e in run_filter_query(conn, f, limit)? {
                if seen.insert(e.id.clone()) {
                    collected.push(e);
                }
            }
        }
    }

    // Newest first; deterministic tie-break by id (NIP-01 replaceable order).
    collected.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    collected.truncate(cap);
    Ok(collected)
}

/// The result of a [`count_events`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountResult {
    pub count: usize,
    /// `true` when a filter's raw row scan hit [`MAX_COUNT_SCAN`] — `count`
    /// is then a lower bound, not an exact count (NIP-45's optional
    /// `"approximate"` field).
    pub approximate: bool,
}

/// Serve a NIP-45 COUNT: the number of stored events matching any of
/// `filters`, without transmitting the events themselves. Shares
/// [`run_filter_query`]'s SQL narrowing and dedup with [`query_events`], then
/// applies the same two authoritative in-memory checks a REQ does —
/// [`fauna_bridge_nostr::filter::matches_any`] and [`gift_wrap_visible_to`] —
/// so a COUNT cannot be used to learn a gift-wrap recipient's inbox size from
/// an unauthenticated or wrongly-authenticated caller (the recipient gate
/// carries into every new serving verb, not just REQ).
pub fn count_events(
    conn: &Connection,
    filters: &[Filter],
    viewer: Option<&str>,
) -> Result<CountResult> {
    count_events_with_scan_cap(conn, filters, viewer, MAX_COUNT_SCAN)
}

/// [`count_events`] parameterized on the per-filter scan ceiling, so tests
/// can exercise the `approximate` truncation flag without inserting tens of
/// thousands of rows. Production always calls it via [`count_events`] with
/// the real [`MAX_COUNT_SCAN`] — this split changes nothing about what ships.
fn count_events_with_scan_cap(
    conn: &Connection,
    filters: &[Filter],
    viewer: Option<&str>,
    scan_cap: usize,
) -> Result<CountResult> {
    let owned_default;
    let effective: &[Filter] = if filters.is_empty() {
        owned_default = [Filter::default()];
        &owned_default
    } else {
        filters
    };

    let mut collected: Vec<Event> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut approximate = false;

    for f in effective {
        let rows = run_filter_query(conn, f, scan_cap)?;
        if rows.len() >= scan_cap {
            approximate = true;
        }
        for e in rows {
            if seen.insert(e.id.clone()) {
                collected.push(e);
            }
        }
    }

    let count = collected
        .into_iter()
        .filter(|e| matches_any(filters, e) && gift_wrap_visible_to(e, viewer))
        .count();
    Ok(CountResult { count, approximate })
}

/// The kind-1059 (NIP-17 gift wrap) recipient gate applied at every relay
/// serving site (REQ replay **and** live broadcast). A gift wrap is DM
/// ciphertext addressed to exactly one recipient — the `p`-tag pubkey — so it is
/// served only to that NIP-42-authenticated recipient; leaking it to any other
/// reader would expose DM ciphertext plus metadata (recipient, message count,
/// timing). Every non-1059 event is public (readable class 1) and always
/// visible.
///
/// `viewer` is the connection's NIP-42-authenticated pubkey (lowercase hex), or
/// `None` for an unauthenticated connection. A 1059 with no `p` tag (which the
/// accept path never admits) is visible to nobody — fail-closed. Authority:
/// `docs/goal/ui/nostr.md` § The relay event store (read/write policy —
/// "kind-1059 events are served only to the NIP-42-authed `p`-tag recipient").
pub fn gift_wrap_visible_to(event: &Event, viewer: Option<&str>) -> bool {
    if event.kind != 1059 {
        return true;
    }
    let Some(viewer) = viewer else {
        return false;
    };
    event
        .tags
        .iter()
        .any(|t| t.name() == Some("p") && t.value() == Some(viewer))
}

/// How many kind-1059 gift wraps are currently stored addressed (`p` tag) to
/// `recipient_pubkey_hex` — the total the unauthenticated inbox's per-
/// recipient rate limit alone does not bound (see
/// [`MAX_GIFT_WRAP_INBOX_PER_RECIPIENT`]). Queries by kind explicitly (a `p`
/// tag alone is not gift-wrap-specific — reactions/zaps/etc. also carry one).
pub fn count_gift_wraps_for_recipient(
    conn: &Connection,
    recipient_pubkey_hex: &str,
) -> Result<usize> {
    let count: i64 = conn
        .prepare(
            "SELECT COUNT(*) FROM nostr_events
              WHERE kind = 1059
                AND id IN (SELECT event_id FROM nostr_event_tags WHERE name = 'p' AND value = ?1)",
        )?
        .query_row([recipient_pubkey_hex], |r| r.get(0))?;
    Ok(count as usize)
}

/// Whether `recipient_pubkey_hex`'s stored gift-wrap inbox is at or over
/// [`MAX_GIFT_WRAP_INBOX_PER_RECIPIENT`] — checked by
/// [`relay_endpoint::handle_gift_wrap_inbox`](super::relay_endpoint::handle_gift_wrap_inbox)
/// before accepting a new wrap.
pub fn gift_wrap_inbox_full(conn: &Connection, recipient_pubkey_hex: &str) -> Result<bool> {
    gift_wrap_inbox_full_with_limit(
        conn,
        recipient_pubkey_hex,
        MAX_GIFT_WRAP_INBOX_PER_RECIPIENT,
    )
}

/// [`gift_wrap_inbox_full`] parameterized on the limit, so tests can exercise
/// the exact boundary without inserting tens of thousands of rows. Production
/// always calls it via [`gift_wrap_inbox_full`] with the real hard-coded
/// constant — this split changes nothing about what ships.
fn gift_wrap_inbox_full_with_limit(
    conn: &Connection,
    recipient_pubkey_hex: &str,
    limit: usize,
) -> Result<bool> {
    Ok(count_gift_wraps_for_recipient(conn, recipient_pubkey_hex)? >= limit)
}

/// Build and run the SQL for a single filter, newest-first up to `limit`.
/// Returns an empty vec (matching nothing) when a present array field is empty
/// — an explicit `"ids": []` / `"authors": []` / `"kinds": []` matches nothing.
fn run_filter_query(conn: &Connection, filter: &Filter, limit: usize) -> Result<Vec<Event>> {
    let mut sql = String::from("SELECT raw_json FROM nostr_events WHERE 1=1");
    let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1usize;

    // ids / authors — prefix-capable (`col LIKE ?||'%'`; a full-length value
    // matches only itself). Hex is lowercase with no LIKE metacharacters.
    if let Some(ids) = &filter.ids {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        push_prefix_clause(&mut sql, &mut binds, &mut idx, "id", ids);
    }
    if let Some(authors) = &filter.authors {
        if authors.is_empty() {
            return Ok(Vec::new());
        }
        push_prefix_clause(&mut sql, &mut binds, &mut idx, "pubkey", authors);
    }

    // kinds
    if let Some(kinds) = &filter.kinds {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<String> = kinds
            .iter()
            .map(|_| {
                let p = format!("?{idx}");
                idx += 1;
                p
            })
            .collect();
        sql.push_str(&format!(" AND kind IN ({})", placeholders.join(",")));
        for k in kinds {
            binds.push(Box::new(*k as i64));
        }
    }

    if let Some(since) = filter.since {
        sql.push_str(&format!(" AND created_at >= ?{idx}"));
        idx += 1;
        binds.push(Box::new(since as i64));
    }
    if let Some(until) = filter.until {
        sql.push_str(&format!(" AND created_at <= ?{idx}"));
        idx += 1;
        binds.push(Box::new(until as i64));
    }

    // Single-letter tag filters — each is a required membership (AND across
    // distinct tag names, OR within one name's values). Non-indexed (multi
    // -letter) tag keys are left to the caller's `matches_any`.
    for (raw_name, values) in &filter.tags {
        let name = raw_name.strip_prefix('#').unwrap_or(raw_name);
        if !is_single_letter(name) {
            continue;
        }
        if values.is_empty() {
            return Ok(Vec::new());
        }
        let name_ph = format!("?{idx}");
        idx += 1;
        binds.push(Box::new(name.to_string()));
        let value_ph: Vec<String> = values
            .iter()
            .map(|_| {
                let p = format!("?{idx}");
                idx += 1;
                p
            })
            .collect();
        sql.push_str(&format!(
            " AND id IN (SELECT event_id FROM nostr_event_tags WHERE name = {} AND value IN ({}))",
            name_ph,
            value_ph.join(",")
        ));
        for v in values {
            binds.push(Box::new(v.clone()));
        }
    }

    // NIP-50: the sanitized FTS5 verdict is authoritative for the search
    // dimension on the store-read path (`nostr.md` § The relay event store).
    // It must narrow in SQL, before LIMIT — an in-memory-only re-check would
    // let non-matching rows consume the LIMIT and hide genuine matches
    // deeper in the store. An empty/extensions-only search contributes no
    // clause (matches everything); kind-1059 rows are absent from the FTS
    // corpus by construction, so a search-carrying filter can never return
    // a gift wrap regardless of the caller's serving gates.
    if let Some(search) = &filter.search
        && let Some(match_q) = fauna_bridge_nostr::nip50::fts5_match_query(search)
    {
        sql.push_str(&format!(
            " AND rowid IN (SELECT rowid FROM nostr_event_fts WHERE nostr_event_fts MATCH ?{idx})"
        ));
        idx += 1;
        binds.push(Box::new(match_q));
    }

    // NIP-40: an expired event is never served, whether or not the periodic
    // sweep ([`sweep_expired`]) has already deleted its row.
    sql.push_str(&format!(" AND (expiration IS NULL OR expiration > ?{idx})"));
    idx += 1;
    binds.push(Box::new(crate::db::now_epoch_secs()));

    sql.push_str(&format!(" ORDER BY created_at DESC, id ASC LIMIT ?{idx}"));
    binds.push(Box::new(limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(refs.as_slice(), |r| r.get::<_, String>(0))?;

    let mut out = Vec::new();
    for raw in rows {
        let raw = raw?;
        if let Ok(ev) = serde_json::from_str::<Event>(&raw) {
            out.push(ev);
        }
    }
    Ok(out)
}

fn push_prefix_clause(
    sql: &mut String,
    binds: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    idx: &mut usize,
    col: &str,
    values: &[String],
) {
    let clauses: Vec<String> = values
        .iter()
        .map(|_| {
            let p = format!("{col} LIKE ?{} || '%'", idx);
            *idx += 1;
            p
        })
        .collect();
    sql.push_str(&format!(" AND ({})", clauses.join(" OR ")));
    for v in values {
        binds.push(Box::new(v.clone()));
    }
}

/// Delete every **derived** (Fauna-post-materialized) event for a pubkey, and
/// clear its outbound `nostr_event_map` entries so a later re-expose
/// re-materializes from scratch. Native events (`derived=0`) are untouched.
/// Derived rows are recreatable (re-derived from the Fauna posts), so this is an
/// allowed no-data-loss deletion — the shape `expose_content` toggle-off takes.
pub fn delete_derived_for_pubkey(conn: &Connection, pubkey: &str) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM nostr_event_tags WHERE event_id IN
             (SELECT id FROM nostr_events WHERE pubkey = ?1 AND derived = 1)",
        [pubkey],
    )?;
    let n = tx.execute(
        "DELETE FROM nostr_events WHERE pubkey = ?1 AND derived = 1",
        [pubkey],
    )?;
    tx.execute(
        "DELETE FROM nostr_event_map WHERE nostr_pubkey = ?1 AND direction = 'outbound'",
        [pubkey],
    )?;
    tx.commit()?;
    Ok(n)
}

/// Delete every row whose NIP-40 `expiration` has passed. [`query_events`]
/// already excludes expired rows from being served, so this is disk hygiene,
/// not a correctness requirement — run periodically (the sync worker's
/// existing 60s tick, alongside [`materialize_all_exposed`]).
pub fn sweep_expired(conn: &Connection) -> Result<usize> {
    let now = crate::db::now_epoch_secs();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM nostr_event_tags WHERE event_id IN
             (SELECT id FROM nostr_events WHERE expiration IS NOT NULL AND expiration <= ?1)",
        [now],
    )?;
    let n = tx.execute(
        "DELETE FROM nostr_events WHERE expiration IS NOT NULL AND expiration <= ?1",
        [now],
    )?;
    tx.commit()?;
    Ok(n)
}

/// NIP-09: apply an author's own kind-5 deletion request. `e` tags name
/// event ids directly; `a` tags name a (parameterized-)replaceable
/// coordinate (the same `kind:pubkey[:d]` shape as [`replace_key`]).
/// **Author-scoped only** — a referenced row is removed iff its stored
/// `pubkey` equals `deletion_event.pubkey`; a tag naming another author's
/// event or coordinate is silently skipped (NIP-09: relays SHOULD ignore
/// `e`/`a` tags referencing a different pubkey). Returns the number of rows
/// removed.
///
/// The kind-5 event itself is stored and served like any other event (the
/// caller's own [`store_event`] call) — this only removes what it
/// references. A **derived** (Fauna-post-materialized) row deleted this way
/// can reappear on the next materialize sweep if the source post is still
/// exposed — NIP-09 deletion acts on the relay's copy, not on Fauna's own
/// exposition state.
pub fn apply_deletion(conn: &Connection, deletion_event: &Event) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut deleted = 0usize;

    for tag in &deletion_event.tags {
        match tag.name() {
            Some("e") => {
                let Some(id) = tag.value() else { continue };
                let owner: Option<String> = tx
                    .prepare("SELECT pubkey FROM nostr_events WHERE id = ?1")?
                    .query_row([id], |r| r.get::<_, String>(0))
                    .optional()?;
                if owner.as_deref() == Some(deletion_event.pubkey.as_str()) {
                    delete_event_row(&tx, id)?;
                    deleted += 1;
                }
            }
            Some("a") => {
                let Some(coord) = tag.value() else { continue };
                let existing: Option<(String, String)> = tx
                    .prepare("SELECT id, pubkey FROM nostr_events WHERE replace_key = ?1")?
                    .query_row([coord], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })
                    .optional()?;
                if let Some((id, owner_pubkey)) = existing
                    && owner_pubkey == deletion_event.pubkey
                {
                    delete_event_row(&tx, &id)?;
                    deleted += 1;
                }
            }
            _ => {}
        }
    }

    tx.commit()?;
    Ok(deleted)
}

// ── Materialization of exposed Fauna posts ──────────────────────────────────

/// A Fauna post is **public** — readable class 1, eligible to be exposed as a
/// Nostr event — iff its stored payload decodes to a plaintext [`Post`] *and*
/// the post is not gated. This is the explicit public-audience predicate the
/// relay materializes on, replacing the pre-store REQ path's incidental
/// decode-error `continue`:
///
/// - A sealed restricted-post payload fails [`Post::decode_resolved_bytes`] by
///   construction (it is ciphertext, not a bare/embedded `Post`), so it is
///   never materialized — sealed content never leaks onto the public relay.
/// - A gated (monetized/paywalled) post is never world-broadcast, even though
///   its preview body decodes — `gated.is_some()` excludes it.
pub fn public_post_from_payload(payload: &[u8]) -> Option<Post> {
    let post = Post::decode_resolved_bytes(payload)?;
    if post.gated.is_some() {
        return None;
    }
    Some(post)
}

/// Materialize every not-yet-materialized exposed post for one account into the
/// store: read the body **segment-first** (`segments::post::load_post_body`,
/// falling back to the inline `content.payload` (an undecodable body) — the
/// same read entry point `get_post_core` uses, `routes.rs` § segment-first
/// serving), translate → sign (once, with the account's decrypted key) →
/// store as a `derived` event, deduped via `nostr_event_map` (outbound).
/// Returns the number newly materialized. Idempotent — a second call is a
/// no-op.
///
/// `actor_id_hex` is the lowercase-hex actor id (the `nostr_accounts.actor_id`
/// convention); the SQL lowercases SQLite's `hex()` to match.
pub async fn materialize_account(
    cache_db: &CacheDb,
    post_segments: &SegmentManager,
    actor_id_hex: &str,
    keypair: &Keypair,
) -> Result<usize> {
    let nostr_pubkey = keypair.public_key_hex();
    let pubkey_bytes = keypair.public_key_bytes();

    let post_ids = unmaterialized_exposed_post_ids(cache_db, actor_id_hex).await?;
    let mut count = 0usize;
    for content_id_hex in post_ids {
        let mut post_id = [0u8; 32];
        if hex::decode_to_slice(&content_id_hex, &mut post_id).is_err() {
            tracing::warn!("nostr materialize: malformed content id {content_id_hex}");
            continue;
        }
        let Some(body) =
            crate::segments::post::load_post_body(post_segments, cache_db, &post_id).await?
        else {
            continue;
        };
        let Some(post) = public_post_from_payload(&body) else {
            continue;
        };
        // The create-side arm's own resolution, so a reply the sweep reaches
        // first still threads (`nostr.md` § Replying to and quoting a nostr note).
        let resolved = {
            let conn = cache_db.conn().await;
            crate::nostr::publish::resolve_nostr_references(&conn, &post).unwrap_or_default()
        };
        let unsigned = match fauna_bridge_nostr::translate::fauna_post_to_nostr(
            &post,
            &pubkey_bytes,
            &resolved,
        ) {
            Ok(u) => u,
            Err(e) => {
                tracing::debug!("nostr materialize: skip post {content_id_hex}: {e}");
                continue;
            }
        };
        let event = keypair.sign_event(unsigned);
        let conn = cache_db.conn().await;
        store_event(&conn, &event, true)?;
        db::insert_event_map(&conn, &content_id_hex, &event.id, &nostr_pubkey, "outbound")?;
        drop(conn);
        count += 1;
    }
    Ok(count)
}

/// The lowercase-hex content ids of an account's `post/%` content that has no
/// outbound `nostr_event_map` entry yet (i.e. not materialized). Body bytes
/// are resolved separately by the caller, segment-first.
async fn unmaterialized_exposed_post_ids(
    cache_db: &CacheDb,
    actor_id_hex: &str,
) -> Result<Vec<String>> {
    let conn = cache_db.conn().await;
    let mut stmt = conn.prepare(&unmaterialized_posts_sql())?;
    let rows = stmt.query_map([actor_id_hex], |r| r.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// The account's post rows eligible to be signed onto the public relay.
///
/// Gated on [`crate::db::public_servability::PUBLIC_POST_SERVABLE`] — the single
/// owner of "may this post be served to an anonymous, off-box audience", shared
/// with the ActivityPub outbox and the ATProto projection stream. The relay is
/// the same class of surface those two are (`moderation.md` § Legal takedown:
/// publishing to a third-party public network, where "a takedown must stop them
/// or it is silently defeated where it matters most"), and until 2026-08-04 it
/// was the one such publisher that had never adopted the constant — so a
/// legally compelled takedown, a quarantine and a suppression were all ignored
/// here while being honoured everywhere else.
///
/// The predicate's `gated_tier IS NULL` arm overlaps [`public_post_from_payload`]'s
/// own gated check, deliberately: that one also rejects payloads that are not a
/// decodable plaintext `Post` at all, so neither subsumes the other and both
/// stay.
///
/// An event already signed and served is withdrawn by the kind-5 retraction
/// leg (`crate::nostr::propagate_post_delete`, triggered by both self-service
/// delete and legal takedown; `moderation.md` § Legal takedown), not by this
/// query — this stops **future** publication.
///
/// The `obligation_action_records` arm is the OVERTURN ruling (`moderation.md`
/// § Legal takedown, 2026-08-04): a restore clears the servability flag, but a
/// post that was ever legally taken down is never *auto*-re-materialized — its
/// retraction was already published, and re-publication to an external network
/// is a fresh author act (post again), never a side effect of an admin
/// overturn. The TakenDown row is written only by the takedown transaction
/// (`post_legal_takedown_txn`), is deliberately kept on restore (additive
/// history), and is matched case-insensitively because its `content_id` is the
/// wire-supplied hex string (the same case trap the takedown witness id
/// documents). Every materialization path shares this query, so the exclusion
/// binds the immediate toggle path, the periodic sweep, and a re-expose after
/// unlink alike.
fn unmaterialized_posts_sql() -> String {
    format!(
        "SELECT lower(hex(c.id))
       FROM content c
       LEFT JOIN content_meta cm ON cm.content_id = c.id
      WHERE lower(hex(c.author)) = ?1
        AND {servable}
        AND NOT EXISTS (
            SELECT 1 FROM nostr_event_map m
             WHERE m.fauna_post_id = lower(hex(c.id))
               AND m.direction = 'outbound'
        )
        AND NOT EXISTS (
            SELECT 1 FROM obligation_action_records o
             WHERE o.content_type = 'post'
               AND lower(o.content_id) = lower(hex(c.id))
               AND o.action_taken = {taken_down}
        )",
        servable = crate::db::public_servability::PUBLIC_POST_SERVABLE.as_str(),
        taken_down = fauna_core::obligation::ObligationAction::TakenDown as u8,
    )
}

/// Whether an account has any exposed post not yet materialized. Cheap gate the
/// periodic sweep checks *before* decrypting the nsec, so a steady-state box
/// (nothing new to materialize) never touches secret key material.
fn has_unmaterialized_posts(conn: &Connection, actor_id_hex: &str) -> Result<bool> {
    let exists: bool = conn
        .prepare(&format!(
            "SELECT EXISTS({} LIMIT 1)",
            unmaterialized_posts_sql()
        ))?
        .query_row([actor_id_hex], |r| r.get::<_, i64>(0))
        .map(|c| c > 0)?;
    Ok(exists)
}

/// Sweep every account that opted into `expose_content` and deposited an nsec,
/// materializing any new exposed posts. Runs on the sync worker's periodic tick
/// (the safety net catching posts created after `expose_content` was already
/// on); the immediate path is [`materialize_account`] on expose-toggle / link.
/// `nest_signing_key_bytes` decrypts each account's stored nsec.
pub async fn materialize_all_exposed(
    cache_db: &CacheDb,
    post_segments: &SegmentManager,
    nest_signing_key_bytes: &[u8; 32],
) -> Result<usize> {
    let accounts: Vec<(String, Vec<u8>)> = {
        let conn = cache_db.conn().await;
        let mut stmt = conn.prepare(
            "SELECT actor_id, encrypted_privkey
               FROM nostr_accounts
              WHERE expose_content = 1 AND encrypted_privkey IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let mut total = 0usize;
    for (actor_id, enc_key) in accounts {
        // Skip the nsec decrypt entirely when there is nothing new to sign.
        let has_new = {
            let conn = cache_db.conn().await;
            has_unmaterialized_posts(&conn, &actor_id)
        };
        match has_new {
            Ok(false) => continue,
            Ok(true) => {}
            Err(e) => {
                tracing::warn!("nostr materialize: unmaterialized check for {actor_id}: {e}");
                continue;
            }
        }
        let Ok(secret) = super::key_crypto::decrypt_nostr_privkey(nest_signing_key_bytes, &enc_key)
        else {
            tracing::warn!("nostr materialize: decrypt nsec failed for {actor_id}");
            continue;
        };
        let Ok(keypair) = Keypair::from_secret_bytes(secret) else {
            continue;
        };
        match materialize_account(cache_db, post_segments, &actor_id, &keypair).await {
            Ok(n) => total += n,
            Err(e) => tracing::warn!("nostr materialize: account {actor_id}: {e}"),
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn signed(kp: &Keypair, kind: u64, created_at: u64, tags: Vec<Tag>, content: &str) -> Event {
        kp.sign_event(UnsignedEvent {
            pubkey: kp.public_key_bytes(),
            created_at,
            kind,
            tags,
            content: content.to_string(),
        })
    }

    #[test]
    fn classify_kind_ranges() {
        assert_eq!(classify_kind(1), KindClass::Regular);
        assert_eq!(classify_kind(0), KindClass::Replaceable);
        assert_eq!(classify_kind(3), KindClass::Replaceable);
        assert_eq!(classify_kind(10_002), KindClass::Replaceable);
        assert_eq!(classify_kind(19_999), KindClass::Replaceable);
        assert_eq!(classify_kind(20_000), KindClass::Ephemeral);
        assert_eq!(classify_kind(22_242), KindClass::Ephemeral);
        assert_eq!(classify_kind(29_999), KindClass::Ephemeral);
        assert_eq!(classify_kind(30_023), KindClass::ParamReplaceable);
        assert_eq!(classify_kind(39_999), KindClass::ParamReplaceable);
        assert_eq!(classify_kind(40_000), KindClass::Regular);
    }

    #[test]
    fn regular_event_stored_and_queryable() {
        let conn = test_conn();
        let kp = keypair(1);
        let ev = signed(&kp, 1, 1000, vec![], "hello");
        assert_eq!(
            store_event(&conn, &ev, false).unwrap(),
            StoreOutcome::Stored
        );

        let got = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, ev.id);
        assert_eq!(got[0].content, "hello");
    }

    #[test]
    fn duplicate_id_is_noop() {
        let conn = test_conn();
        let kp = keypair(1);
        let ev = signed(&kp, 1, 1000, vec![], "hello");
        assert_eq!(
            store_event(&conn, &ev, false).unwrap(),
            StoreOutcome::Stored
        );
        assert_eq!(
            store_event(&conn, &ev, false).unwrap(),
            StoreOutcome::Duplicate
        );
        assert_eq!(
            query_events(&conn, &[Filter::default()], 100)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn ephemeral_never_stored() {
        let conn = test_conn();
        let kp = keypair(1);
        let ev = signed(&kp, 20_000, 1000, vec![], "ephemeral");
        assert_eq!(
            store_event(&conn, &ev, false).unwrap(),
            StoreOutcome::Ephemeral
        );
        assert!(
            query_events(&conn, &[Filter::default()], 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn replaceable_keeps_newest() {
        let conn = test_conn();
        let kp = keypair(1);
        let old = signed(&kp, 0, 1000, vec![], "old profile");
        let new = signed(&kp, 0, 2000, vec![], "new profile");
        assert_eq!(
            store_event(&conn, &old, false).unwrap(),
            StoreOutcome::Stored
        );
        assert_eq!(
            store_event(&conn, &new, false).unwrap(),
            StoreOutcome::Replaced
        );
        let got = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, "new profile");
    }

    #[test]
    fn replaceable_rejects_older() {
        let conn = test_conn();
        let kp = keypair(1);
        let new = signed(&kp, 0, 2000, vec![], "new profile");
        let old = signed(&kp, 0, 1000, vec![], "old profile");
        assert_eq!(
            store_event(&conn, &new, false).unwrap(),
            StoreOutcome::Stored
        );
        assert_eq!(
            store_event(&conn, &old, false).unwrap(),
            StoreOutcome::Superseded
        );
        let got = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, "new profile");
    }

    #[test]
    fn param_replaceable_scoped_by_d_tag() {
        let conn = test_conn();
        let kp = keypair(1);
        let d1a = signed(
            &kp,
            30_023,
            1000,
            vec![Tag::new(vec!["d".into(), "a".into()])],
            "a v1",
        );
        let d1b = signed(
            &kp,
            30_023,
            2000,
            vec![Tag::new(vec!["d".into(), "a".into()])],
            "a v2",
        );
        let d2 = signed(
            &kp,
            30_023,
            1000,
            vec![Tag::new(vec!["d".into(), "b".into()])],
            "b v1",
        );
        assert_eq!(
            store_event(&conn, &d1a, false).unwrap(),
            StoreOutcome::Stored
        );
        assert_eq!(
            store_event(&conn, &d2, false).unwrap(),
            StoreOutcome::Stored
        );
        // Same (pubkey, kind, d=a) replaces d1a; d2 (d=b) is untouched.
        assert_eq!(
            store_event(&conn, &d1b, false).unwrap(),
            StoreOutcome::Replaced
        );
        let got = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert_eq!(got.len(), 2);
        let contents: std::collections::HashSet<_> =
            got.iter().map(|e| e.content.as_str()).collect();
        assert!(contents.contains("a v2"));
        assert!(contents.contains("b v1"));
        assert!(!contents.contains("a v1"));
    }

    #[test]
    fn query_by_author_kind_since_until() {
        let conn = test_conn();
        let a = keypair(1);
        let b = keypair(2);
        let ea = signed(&a, 1, 1000, vec![], "from a");
        let eb = signed(&b, 1, 2000, vec![], "from b");
        let ec = signed(&a, 7, 3000, vec![], "reaction a");
        store_event(&conn, &ea, false).unwrap();
        store_event(&conn, &eb, false).unwrap();
        store_event(&conn, &ec, false).unwrap();

        // author = a
        let f = Filter {
            authors: Some(vec![a.public_key_hex()]),
            ..Default::default()
        };
        let got = query_events(&conn, &[f], 100).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|e| e.pubkey == a.public_key_hex()));

        // kind = 1
        let f = Filter {
            kinds: Some(vec![1]),
            ..Default::default()
        };
        assert_eq!(query_events(&conn, &[f], 100).unwrap().len(), 2);

        // since/until window [1500, 2500] → only eb
        let f = Filter {
            since: Some(1500),
            until: Some(2500),
            ..Default::default()
        };
        let got = query_events(&conn, &[f], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, "from b");
    }

    #[test]
    fn query_by_tag_filter() {
        let conn = test_conn();
        let kp = keypair(1);
        let target = "e".repeat(64);
        let tagged = signed(
            &kp,
            1,
            1000,
            vec![Tag::new(vec!["e".into(), target.clone()])],
            "reply",
        );
        let untagged = signed(&kp, 1, 1000, vec![], "not a reply");
        store_event(&conn, &tagged, false).unwrap();
        store_event(&conn, &untagged, false).unwrap();

        let mut tags = std::collections::HashMap::new();
        tags.insert("#e".to_string(), vec![target]);
        let f = Filter {
            tags,
            ..Default::default()
        };
        let got = query_events(&conn, &[f], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, "reply");
    }

    #[test]
    fn query_by_id_prefix() {
        let conn = test_conn();
        let kp = keypair(1);
        let ev = signed(&kp, 1, 1000, vec![], "hi");
        store_event(&conn, &ev, false).unwrap();
        let f = Filter {
            ids: Some(vec![ev.id[..10].to_string()]),
            ..Default::default()
        };
        let got = query_events(&conn, &[f], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, ev.id);
    }

    #[test]
    fn empty_ids_array_matches_nothing() {
        let conn = test_conn();
        let kp = keypair(1);
        store_event(&conn, &signed(&kp, 1, 1000, vec![], "hi"), false).unwrap();
        let f = Filter {
            ids: Some(vec![]),
            ..Default::default()
        };
        assert!(query_events(&conn, &[f], 100).unwrap().is_empty());
    }

    #[test]
    fn multiple_filters_union() {
        let conn = test_conn();
        let kp = keypair(1);
        let k1 = signed(&kp, 1, 1000, vec![], "note");
        let k7 = signed(&kp, 7, 2000, vec![], "reaction");
        let k4 = signed(&kp, 4, 3000, vec![], "other");
        store_event(&conn, &k1, false).unwrap();
        store_event(&conn, &k7, false).unwrap();
        store_event(&conn, &k4, false).unwrap();
        let f1 = Filter {
            kinds: Some(vec![1]),
            ..Default::default()
        };
        let f2 = Filter {
            kinds: Some(vec![7]),
            ..Default::default()
        };
        let got = query_events(&conn, &[f1, f2], 100).unwrap();
        assert_eq!(got.len(), 2);
        // newest first
        assert_eq!(got[0].content, "reaction");
        assert_eq!(got[1].content, "note");
    }

    #[test]
    fn delete_derived_leaves_native() {
        let conn = test_conn();
        let kp = keypair(1);
        let native = signed(&kp, 1, 1000, vec![], "native");
        let derived = signed(
            &kp,
            1,
            2000,
            vec![Tag::new(vec!["e".into(), "x".repeat(64)])],
            "derived",
        );
        store_event(&conn, &native, false).unwrap();
        store_event(&conn, &derived, true).unwrap();
        assert_eq!(
            query_events(&conn, &[Filter::default()], 100)
                .unwrap()
                .len(),
            2
        );

        let n = delete_derived_for_pubkey(&conn, &kp.public_key_hex()).unwrap();
        assert_eq!(n, 1);
        let got = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, "native");
        // the derived event's tags are gone too
        let tag_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM nostr_event_tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tag_count, 0);
    }

    /// A tempdir-backed `__post` segment store for the materialize tests below
    /// — a real `SegmentManager`, not a stub, since `materialize_account` now
    /// reads the body through it (segment-first). Per-call sequence (pid +
    /// atomic counter) keeps each instance's dir test-unique, the same
    /// convention `AppState::for_test` uses.
    fn test_post_segments() -> SegmentManager {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        SegmentManager::new(
            std::env::temp_dir().join(format!(
                "fauna-test-nostr-store-post-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )),
            "post",
        )
    }

    #[tokio::test]
    async fn materialize_account_signs_once_and_dedupes() {
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::identity::ActorId;

        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        crate::nostr::init_db(&db).await.unwrap();
        let post_segments = test_post_segments();
        let kp = keypair(1);
        let author_bytes = [7u8; 32];
        let actor_hex = hex::encode(author_bytes);

        let post = Post {
            author: ActorId(author_bytes),
            created_at: Timestamp(1_000_000_000_000),
            body: PostBody::Text {
                content: "hello nostr world".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
        let id = blake3::hash(&payload);
        // The real post-cutover write path (`segments::post::store_post`):
        // body → `__post` segment, `content` row with an EMPTY payload — the
        // exact shape that used to defeat `materialize_account`'s direct
        // `content.payload` read.
        crate::segments::post::store_post(&post_segments, &db, id.as_bytes(), &payload, None)
            .await
            .unwrap();

        // First run materializes the post as a signed, store-served event.
        assert_eq!(
            materialize_account(&db, &post_segments, &actor_hex, &kp)
                .await
                .unwrap(),
            1
        );
        let conn = db.conn().await;
        let f = Filter {
            authors: Some(vec![kp.public_key_hex()]),
            ..Default::default()
        };
        let got = query_events(&conn, &[f], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert!(fauna_bridge_nostr::signing::verify_event(&got[0]));
        assert!(got[0].content.contains("hello nostr world"));
        drop(conn);

        // Idempotent — a second run materializes nothing (deduped via the
        // outbound nostr_event_map).
        assert_eq!(
            materialize_account(&db, &post_segments, &actor_hex, &kp)
                .await
                .unwrap(),
            0
        );
        let conn = db.conn().await;
        assert_eq!(
            query_events(&conn, &[Filter::default()], 100)
                .unwrap()
                .len(),
            1
        );
    }

    /// A **sold** post is a gated post (`monetization.md` § Per-post
    /// pay-to-unlock: "a sold post is therefore authored as a gated post from
    /// the start"), and a gated post is never world-broadcast. Since
    /// [`materialize_account`] is the only writer of an *outbound*
    /// `nostr_event_map` row, and `resolve_zap_subject` resolves only outbound
    /// rows, this is the fact that makes the zap purchase leg unreachable for
    /// every real sale today — declared in `monetization.md` § Implementation
    /// status today rather than left for a later session to rediscover.
    ///
    /// The public post beside it is load-bearing, not scenery: without it a
    /// materializer that had simply stopped working would satisfy the gated
    /// assertion too, and the test would be a zero pin.
    #[tokio::test]
    async fn a_sold_post_is_never_materialized_so_no_zap_can_name_it() {
        use fauna_core::data::{ContentHash, Post, PostBody, Timestamp};
        use fauna_core::identity::ActorId;
        use fauna_core::subscription::types::{GatedInfo, KeyAccess};

        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        crate::nostr::init_db(&db).await.unwrap();
        let post_segments = test_post_segments();
        let kp = keypair(9);
        let author_bytes = [11u8; 32];
        let actor_hex = hex::encode(author_bytes);

        let store = async |content: &str, gated: Option<GatedInfo>| {
            let post = Post {
                author: ActorId(author_bytes),
                created_at: Timestamp(1_000_000_000_000),
                body: PostBody::Text {
                    content: content.into(),
                    facets: vec![],
                },
                references: vec![],
                expires_at: None,
                gated,
                content_warning: None,
                origin: None,
            };
            let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
            let id = *blake3::hash(&payload).as_bytes();
            crate::segments::post::store_post(&post_segments, &db, &id, &payload, None)
                .await
                .unwrap();
            id
        };

        // The sold post: its body is the public teaser, the sealed half lives
        // at `encrypted_ref`, and `unlocks_post` on the tier names this id.
        let sold = store(
            "buy my post — teaser",
            Some(GatedInfo {
                encrypted_ref: ContentHash::from_digest_raw([9u8; 32]),
                key_access: KeyAccess::Broadcast {
                    key_blob_ref: ContentHash::from_digest_raw([8u8; 32]),
                },
                tier: "post-unlock-x".into(),
                tier_rank: 2,
                seal_id: ContentHash::from_digest_raw([7u8; 32]),
                attachment_refs: vec![],
            }),
        )
        .await;
        let public = store("an ordinary public post", None).await;

        assert_eq!(
            materialize_account(&db, &post_segments, &actor_hex, &kp)
                .await
                .unwrap(),
            1,
            "exactly one of the two posts is world-broadcastable"
        );

        let conn = db.conn().await;
        let mapped = |id: &[u8; 32]| -> bool {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM nostr_event_map \
                  WHERE fauna_post_id = ?1 AND direction = 'outbound')",
                [hex::encode(id)],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
                > 0
        };
        assert!(mapped(&public), "the public post is materialized");
        assert!(
            !mapped(&sold),
            "a sold post must never reach the public relay — and because this row \
             is what `resolve_zap_subject` needs, its absence is exactly why a zap \
             cannot buy a real sold post today"
        );
    }

    /// The Nostr relay is a **serve path that publishes to a third-party public
    /// network**, so `moderation.md` § Legal takedown's rule for that class
    /// binds it exactly as it binds the ActivityPub outbox and the ATProto
    /// projection: "a takedown must stop them or it is silently defeated where
    /// it matters most". This is that rule's fourth surface — it was the one
    /// off-box publisher that never adopted `PUBLIC_POST_SERVABLE`, so a
    /// legally-compelled takedown, a quarantine and a suppression were all
    /// silently ignored here.
    ///
    /// Each flag is asserted separately: they are three independent columns and
    /// a predicate honouring only one would satisfy a test that set all three.
    #[tokio::test]
    async fn a_flagged_post_is_never_materialized_onto_the_public_relay() {
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::identity::ActorId;

        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        crate::nostr::init_db(&db).await.unwrap();
        let post_segments = test_post_segments();
        let kp = keypair(12);
        let author_bytes = [13u8; 32];
        let actor_hex = hex::encode(author_bytes);

        let store = async |content: &str| {
            let post = Post {
                author: ActorId(author_bytes),
                created_at: Timestamp(1_000_000_000_000),
                body: PostBody::Text {
                    content: content.into(),
                    facets: vec![],
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin: None,
            };
            let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
            let id = *blake3::hash(&payload).as_bytes();
            // `put_post`, not `store_post`: the flags live on `content_meta`,
            // which only the indexing write path mints.
            db.put_post(&id, &payload, None).await.unwrap();
            id
        };

        let taken_down = store("compelled down").await;
        let quarantined = store("quarantined").await;
        let suppressed = store("suppressed").await;
        let clean = store("an ordinary public post").await;

        db.set_post_legal_takedown(&taken_down, Some("court-order-1"))
            .await
            .unwrap();
        db.set_post_quarantined(&quarantined, true).await.unwrap();
        db.set_post_suppressed(&suppressed, true).await.unwrap();

        assert_eq!(
            materialize_account(&db, &post_segments, &actor_hex, &kp)
                .await
                .unwrap(),
            1,
            "only the unflagged post may be signed onto the public relay"
        );

        let conn = db.conn().await;
        let mapped = |id: &[u8; 32]| -> bool {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM nostr_event_map \
                  WHERE fauna_post_id = ?1 AND direction = 'outbound')",
                [hex::encode(id)],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
                > 0
        };
        assert!(mapped(&clean), "the unflagged post is materialized");
        assert!(
            !mapped(&taken_down),
            "a legally compelled takedown must stop publication to Nostr, or the \
             compulsion is defeated on the surface that reaches the widest audience"
        );
        assert!(!mapped(&quarantined), "a quarantined post is withheld");
        assert!(!mapped(&suppressed), "a suppressed post is withheld");
    }

    /// Ruling 1 (`archive-import.md` § Compatibility → *Slice-3 rulings*): an
    /// archive-imported PUBLIC post is served on Fauna and never materialized
    /// onto the relay — a backdated flood into other networks is the one
    /// irreversible outcome, and outward publication is elsewhere a deliberate
    /// author act. The control post beside it (same author, same instant, no
    /// origin) materializes as before.
    #[tokio::test]
    async fn an_archive_imported_post_is_never_materialized_onto_the_public_relay() {
        use fauna_core::data::{Post, PostBody, PostOrigin, Timestamp};
        use fauna_core::identity::ActorId;

        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        crate::nostr::init_db(&db).await.unwrap();
        let post_segments = test_post_segments();
        let kp = keypair(14);
        let author_bytes = [15u8; 32];
        let actor_hex = hex::encode(author_bytes);

        let store = async |content: &str, origin: Option<PostOrigin>| {
            let post = Post {
                author: ActorId(author_bytes),
                created_at: Timestamp(1_000_000_000_000),
                body: PostBody::Text {
                    content: content.into(),
                    facets: vec![],
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin,
            };
            let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
            let id = *blake3::hash(&payload).as_bytes();
            db.put_post(&id, &payload, None).await.unwrap();
            id
        };

        let imported = store(
            "re-authored from a Facebook export",
            Some(PostOrigin {
                platform: fauna_core::source::FACEBOOK.into(),
                url: None,
            }),
        )
        .await;
        let native = store("an ordinary public post", None).await;
        assert_eq!(
            db.get_post_source(&imported).await.unwrap().as_deref(),
            Some(fauna_core::source::FACEBOOK),
            "the index derives source from origin (slice 2)"
        );

        assert_eq!(
            materialize_account(&db, &post_segments, &actor_hex, &kp)
                .await
                .unwrap(),
            1,
            "only the native post may be signed onto the public relay"
        );

        let conn = db.conn().await;
        let mapped = |id: &[u8; 32]| -> bool {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM nostr_event_map \
                  WHERE fauna_post_id = ?1 AND direction = 'outbound')",
                [hex::encode(id)],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
                == 1
        };
        assert!(mapped(&native));
        assert!(!mapped(&imported));
    }

    #[tokio::test]
    async fn materialize_all_exposed_sweeps_depositor_accounts() {
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::identity::ActorId;

        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        crate::nostr::init_db(&db).await.unwrap();
        let post_segments = test_post_segments();
        let nest_key = [42u8; 32];
        let kp = keypair(5);
        let author_bytes = [7u8; 32];
        let actor_hex = hex::encode(author_bytes);

        // Deposit the (encrypted) nsec, link the account, and expose content.
        let ciphertext =
            crate::nostr::key_crypto::encrypt_nostr_privkey(&nest_key, &kp.secret_bytes()).unwrap();
        {
            let conn = db.conn().await;
            db::link_account(
                &conn,
                &actor_hex,
                &kp.public_key_hex(),
                "generate",
                Some(&ciphertext),
                None,
                None,
            )
            .unwrap();
            db::update_settings(
                &conn,
                &actor_hex,
                &db::NostrSettings {
                    expose_content: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        }

        let post = Post {
            author: ActorId(author_bytes),
            created_at: Timestamp(1_000_000_000_000),
            body: PostBody::Text {
                content: "swept into the relay".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
        let id = blake3::hash(&payload);
        crate::segments::post::store_post(&post_segments, &db, id.as_bytes(), &payload, None)
            .await
            .unwrap();

        // The sweep decrypts the nsec, materializes the post, and is idempotent
        // (the second call short-circuits at `has_unmaterialized_posts`).
        assert_eq!(
            materialize_all_exposed(&db, &post_segments, &nest_key)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            materialize_all_exposed(&db, &post_segments, &nest_key)
                .await
                .unwrap(),
            0
        );

        let conn = db.conn().await;
        let f = Filter {
            authors: Some(vec![kp.public_key_hex()]),
            ..Default::default()
        };
        let got = query_events(&conn, &[f], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert!(fauna_bridge_nostr::signing::verify_event(&got[0]));
        assert!(got[0].content.contains("swept into the relay"));
    }

    #[test]
    fn gift_wrap_visible_only_to_authed_recipient() {
        // A kind-1059 gift wrap addressed (p tag) to `recipient`.
        let recipient = keypair(1);
        let other = keypair(2);
        let sender = keypair(3);
        let recipient_hex = recipient.public_key_hex();
        let wrap = signed(
            &sender,
            1059,
            1000,
            vec![Tag::new(vec!["p".into(), recipient_hex.clone()])],
            "ciphertext",
        );

        // Served to the NIP-42-authed recipient only.
        assert!(gift_wrap_visible_to(&wrap, Some(&recipient_hex)));
        // Hidden from an anonymous (unauthenticated) reader.
        assert!(!gift_wrap_visible_to(&wrap, None));
        // Hidden from a different authed pubkey — no DM ciphertext/metadata leak.
        assert!(!gift_wrap_visible_to(&wrap, Some(&other.public_key_hex())));
    }

    #[test]
    fn non_gift_wrap_is_public_to_everyone() {
        let kp = keypair(1);
        let note = signed(&kp, 1, 1000, vec![], "public note");
        assert!(gift_wrap_visible_to(&note, None));
        assert!(gift_wrap_visible_to(&note, Some(&kp.public_key_hex())));
        assert!(gift_wrap_visible_to(&note, Some(&"f".repeat(64))));
    }

    #[test]
    fn gift_wrap_without_p_tag_visible_to_nobody() {
        // Defensive fail-closed: a 1059 with no `p` tag can be recipient-matched
        // by no one (the accept path never admits one, but the gate must not
        // fall open if a stray row exists).
        let sender = keypair(3);
        let wrap = signed(&sender, 1059, 1000, vec![], "ciphertext");
        assert!(!gift_wrap_visible_to(&wrap, Some(&"a".repeat(64))));
        assert!(!gift_wrap_visible_to(&wrap, None));
    }

    #[test]
    fn gated_post_is_not_public() {
        // A payload that decodes to a gated Post must be excluded (monetized
        // content is never world-broadcast onto the public relay).
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::identity::ActorId;
        use fauna_core::subscription::types::{GatedInfo, KeyAccess};

        let gated_info = GatedInfo {
            encrypted_ref: fauna_cbor::Cid::from_digest_dag_cbor([1u8; 32]),
            key_access: KeyAccess::Broadcast {
                key_blob_ref: fauna_cbor::Cid::from_digest_dag_cbor([2u8; 32]),
            },
            tier: "supporter".into(),
            tier_rank: 1,
            seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
            attachment_refs: vec![],
        };
        let post = Post {
            author: ActorId([9u8; 32]),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "preview".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: Some(gated_info),
            content_warning: None,
            origin: None,
        };
        let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
        assert!(public_post_from_payload(&payload).is_none());

        // Same post, not gated → public.
        let public = Post {
            gated: None,
            ..post
        };
        let payload = fauna_core::encoding::canonical_encode(&public).unwrap();
        assert!(public_post_from_payload(&payload).is_some());
    }

    // ── Slice C: NIP-40 expiration ──────────────────────────────────────

    #[test]
    fn already_expired_event_is_rejected_at_write() {
        let conn = test_conn();
        let kp = keypair(1);
        let now = crate::db::now_epoch_secs();
        let expired = signed(
            &kp,
            1,
            1000,
            vec![Tag::new(vec!["expiration".into(), (now - 100).to_string()])],
            "already expired",
        );
        assert_eq!(
            store_event(&conn, &expired, false).unwrap(),
            StoreOutcome::Expired
        );
        assert!(
            query_events(&conn, &[Filter::default()], 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn future_expiration_is_stored_and_served() {
        let conn = test_conn();
        let kp = keypair(1);
        let now = crate::db::now_epoch_secs();
        let ev = signed(
            &kp,
            1,
            1000,
            vec![Tag::new(vec![
                "expiration".into(),
                (now + 100_000).to_string(),
            ])],
            "expires later",
        );
        assert_eq!(
            store_event(&conn, &ev, false).unwrap(),
            StoreOutcome::Stored
        );
        let got = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, "expires later");
    }

    #[test]
    fn event_with_no_expiration_tag_never_expires() {
        let conn = test_conn();
        let kp = keypair(1);
        let ev = signed(&kp, 1, 1000, vec![], "no expiration");
        assert_eq!(
            store_event(&conn, &ev, false).unwrap(),
            StoreOutcome::Stored
        );
        assert_eq!(
            query_events(&conn, &[Filter::default()], 100)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_row_past_expiration_is_excluded_from_query_even_before_sweep() {
        // Simulates the window between an event's expiration passing and the
        // periodic sweep running: `query_events` must exclude it on its own
        // (`store_event` never lets an already-expired write in, so insert
        // directly to model a row that expired after being legitimately
        // stored with a future expiration).
        let conn = test_conn();
        let kp = keypair(1);
        let now = crate::db::now_epoch_secs();
        let ev = signed(&kp, 1, 1000, vec![], "stale");
        conn.execute(
            "INSERT INTO nostr_events (id, pubkey, kind, created_at, raw_json, derived, stored_at, expiration)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)",
            params![
                ev.id,
                ev.pubkey,
                ev.kind as i64,
                ev.created_at as i64,
                serde_json::to_string(&ev).unwrap(),
                now,
                now - 1,
            ],
        )
        .unwrap();
        assert!(
            query_events(&conn, &[Filter::default()], 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sweep_expired_removes_only_expired_rows() {
        let conn = test_conn();
        let kp = keypair(1);
        let now = crate::db::now_epoch_secs();

        let keeper = signed(&kp, 1, 1000, vec![], "keeper");
        store_event(&conn, &keeper, false).unwrap();

        let stale = signed(
            &kp,
            1,
            2000,
            vec![Tag::new(vec!["e".into(), "x".repeat(64)])],
            "stale",
        );
        conn.execute(
            "INSERT INTO nostr_events (id, pubkey, kind, created_at, raw_json, derived, stored_at, expiration)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)",
            params![
                stale.id,
                stale.pubkey,
                stale.kind as i64,
                stale.created_at as i64,
                serde_json::to_string(&stale).unwrap(),
                now,
                now - 1,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nostr_event_tags (event_id, name, value) VALUES (?1, 'e', ?2)",
            params![stale.id, "x".repeat(64)],
        )
        .unwrap();

        assert_eq!(sweep_expired(&conn).unwrap(), 1);
        let ids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT id FROM nostr_events").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(ids, vec![keeper.id]);
        let tag_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM nostr_event_tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tag_count, 0, "the swept row's tags go with it");
    }

    // ── Slice C: NIP-09 deletion ─────────────────────────────────────────

    #[test]
    fn deletion_removes_own_event_by_e_tag() {
        let conn = test_conn();
        let kp = keypair(1);
        let note = signed(&kp, 1, 1000, vec![], "delete me");
        store_event(&conn, &note, false).unwrap();

        let deletion = signed(
            &kp,
            5,
            2000,
            vec![Tag::new(vec!["e".into(), note.id.clone()])],
            "",
        );
        store_event(&conn, &deletion, false).unwrap();
        assert_eq!(apply_deletion(&conn, &deletion).unwrap(), 1);

        let remaining = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert!(remaining.iter().all(|e| e.id != note.id));
        // The deletion event itself is a normal stored event.
        assert!(remaining.iter().any(|e| e.id == deletion.id));
    }

    #[test]
    fn deletion_ignores_another_authors_event() {
        let conn = test_conn();
        let victim = keypair(1);
        let attacker = keypair(2);
        let note = signed(&victim, 1, 1000, vec![], "not yours to delete");
        store_event(&conn, &note, false).unwrap();

        let forged_deletion = signed(
            &attacker,
            5,
            2000,
            vec![Tag::new(vec!["e".into(), note.id.clone()])],
            "",
        );
        assert_eq!(apply_deletion(&conn, &forged_deletion).unwrap(), 0);

        let remaining = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert!(remaining.iter().any(|e| e.id == note.id), "untouched");
    }

    #[test]
    fn deletion_removes_replaceable_by_a_tag_coordinate() {
        let conn = test_conn();
        let kp = keypair(1);
        let article = signed(
            &kp,
            30_023,
            1000,
            vec![Tag::new(vec!["d".into(), "my-post".into()])],
            "long form",
        );
        store_event(&conn, &article, false).unwrap();
        let coord = format!("30023:{}:my-post", kp.public_key_hex());

        let deletion = signed(&kp, 5, 2000, vec![Tag::new(vec!["a".into(), coord])], "");
        store_event(&conn, &deletion, false).unwrap();
        assert_eq!(apply_deletion(&conn, &deletion).unwrap(), 1);

        let remaining = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert!(remaining.iter().all(|e| e.id != article.id));
    }

    #[test]
    fn deletion_with_no_matching_tags_deletes_nothing() {
        let conn = test_conn();
        let kp = keypair(1);
        let deletion = signed(
            &kp,
            5,
            1000,
            vec![Tag::new(vec!["e".into(), "f".repeat(64)])],
            "",
        );
        assert_eq!(apply_deletion(&conn, &deletion).unwrap(), 0);
    }

    // ── Slice C: enforced store caps ─────────────────────────────────────

    #[test]
    fn store_caps_exceeded_at_per_account_limit() {
        let conn = test_conn();
        let kp = keypair(1);
        store_event(&conn, &signed(&kp, 1, 1000, vec![], "one"), false).unwrap();
        assert!(
            !store_caps_exceeded_with_limits(&conn, &kp.public_key_hex(), 2, 100).unwrap(),
            "one stored, limit two — room for one more"
        );
        store_event(&conn, &signed(&kp, 1, 1001, vec![], "two"), false).unwrap();
        assert!(
            store_caps_exceeded_with_limits(&conn, &kp.public_key_hex(), 2, 100).unwrap(),
            "two stored, limit two — no more room"
        );
    }

    #[test]
    fn store_caps_exceeded_at_total_limit_across_accounts() {
        let conn = test_conn();
        let a = keypair(1);
        let b = keypair(2);
        store_event(&conn, &signed(&a, 1, 1000, vec![], "a"), false).unwrap();
        store_event(&conn, &signed(&b, 1, 1000, vec![], "b"), false).unwrap();
        // Neither account is near its own per-account limit; the store-wide
        // total is what trips.
        assert!(
            store_caps_exceeded_with_limits(&conn, &a.public_key_hex(), 100, 2).unwrap(),
            "store-wide total of two reached, regardless of per-account split"
        );
    }

    #[test]
    fn a_full_store_rejects_new_events_via_store_event() {
        // End-to-end wiring check with a real (small) limit substituted for
        // the production constant, proving `store_event` actually consults
        // the cap gate rather than just the standalone helper.
        let conn = test_conn();
        let kp = keypair(1);
        store_event(&conn, &signed(&kp, 1, 1000, vec![], "one"), false).unwrap();
        assert!(store_caps_exceeded_with_limits(&conn, &kp.public_key_hex(), 1, 100).unwrap());
        // `store_event` itself only ever consults the real MAX_* constants,
        // so at this (tiny) scale it still accepts — this test documents
        // that the production ceiling is intentionally far above any single
        // test's reach, per `store_caps_exceeded`'s doc link to the
        // parameterized helper this suite exercises directly above.
        assert_eq!(
            store_event(&conn, &signed(&kp, 1, 1001, vec![], "two"), false).unwrap(),
            StoreOutcome::Stored
        );
    }

    // ── gift-wrap-inbox TOTAL cap ──────────────
    // (rate-limit-bounds-rate-not-total: MAX_EVENTS_PER_ACCOUNT never engages
    // for gift wraps, which carry a fresh random sender pubkey per NIP-59
    // send — this is the total bound that actually applies.)

    #[test]
    fn count_gift_wraps_for_recipient_only_counts_kind_1059() {
        let conn = test_conn();
        let recipient = keypair(1);
        let other_recipient = keypair(2);
        let recipient_hex = recipient.public_key_hex();

        // Two wraps addressed to `recipient`, from distinct random senders
        // (as a real NIP-59 flood would send).
        store_event(
            &conn,
            &signed(
                &keypair(10),
                1059,
                1000,
                vec![Tag::new(vec!["p".into(), recipient_hex.clone()])],
                "wrap 1",
            ),
            false,
        )
        .unwrap();
        store_event(
            &conn,
            &signed(
                &keypair(11),
                1059,
                1001,
                vec![Tag::new(vec!["p".into(), recipient_hex.clone()])],
                "wrap 2",
            ),
            false,
        )
        .unwrap();
        // A wrap addressed to someone else does not count.
        store_event(
            &conn,
            &signed(
                &keypair(12),
                1059,
                1002,
                vec![Tag::new(vec!["p".into(), other_recipient.public_key_hex()])],
                "not yours",
            ),
            false,
        )
        .unwrap();
        // A non-1059 event carrying a `p` tag naming the same recipient must
        // NOT be miscounted as a gift wrap.
        store_event(
            &conn,
            &signed(
                &keypair(13),
                1,
                1003,
                vec![Tag::new(vec!["p".into(), recipient_hex.clone()])],
                "just a mention",
            ),
            false,
        )
        .unwrap();

        assert_eq!(
            count_gift_wraps_for_recipient(&conn, &recipient_hex).unwrap(),
            2
        );
    }

    #[test]
    fn gift_wrap_inbox_full_at_the_limit() {
        let conn = test_conn();
        let recipient_hex = keypair(1).public_key_hex();
        store_event(
            &conn,
            &signed(
                &keypair(10),
                1059,
                1000,
                vec![Tag::new(vec!["p".into(), recipient_hex.clone()])],
                "wrap",
            ),
            false,
        )
        .unwrap();

        assert!(
            !gift_wrap_inbox_full_with_limit(&conn, &recipient_hex, 2).unwrap(),
            "one stored, limit two — room for one more"
        );

        store_event(
            &conn,
            &signed(
                &keypair(11),
                1059,
                1001,
                vec![Tag::new(vec!["p".into(), recipient_hex.clone()])],
                "wrap 2",
            ),
            false,
        )
        .unwrap();

        assert!(
            gift_wrap_inbox_full_with_limit(&conn, &recipient_hex, 2).unwrap(),
            "two stored, limit two — full"
        );
    }

    // ── Slice C: tag-query completeness audit ────────────────────────────

    #[test]
    fn a_non_single_letter_tag_filter_is_not_narrowed_by_sql_alone() {
        // NIP-01 filter tag queries are `#<single-letter>` by spec, but the
        // Filter type accepts any key generically. `run_filter_query` only
        // indexes/filters single-letter tag names in SQL (`is_single_letter`)
        // — a non-standard multi-letter key is silently NOT applied at the
        // SQL layer, so `query_events` alone is over-broad here. This is
        // exactly why every caller (`relay_endpoint`'s REQ/COUNT dispatch)
        // MUST apply `fauna_bridge_nostr::filter::matches_any` as the
        // authoritative final check — `Filter::matches` (unlike the SQL
        // path) checks any tag name literally, letter-length notwithstanding
        // (see the companion `filter_by_multi_letter_tag_name` test in
        // `fauna_bridge_nostr::filter`).
        let conn = test_conn();
        let kp = keypair(1);
        let tagged = signed(
            &kp,
            1,
            1000,
            vec![Tag::new(vec!["subject".into(), "keep".into()])],
            "has subject",
        );
        let untagged = signed(&kp, 1, 1000, vec![], "no subject");
        store_event(&conn, &tagged, false).unwrap();
        store_event(&conn, &untagged, false).unwrap();

        let mut tags = std::collections::HashMap::new();
        tags.insert("subject".to_string(), vec!["keep".to_string()]);
        let f = Filter {
            tags,
            ..Default::default()
        };
        // Over-broad: the SQL layer never applied the "subject" constraint.
        let got = query_events(&conn, std::slice::from_ref(&f), 100).unwrap();
        assert_eq!(
            got.len(),
            2,
            "SQL alone did not narrow by the multi-letter tag"
        );

        // The authoritative in-memory check (what every real caller applies)
        // correctly narrows to just the matching event.
        let narrowed: Vec<_> = got
            .into_iter()
            .filter(|e| matches_any(std::slice::from_ref(&f), e))
            .collect();
        assert_eq!(narrowed.len(), 1);
        assert_eq!(narrowed[0].id, tagged.id);
    }

    // ── Slice C: NIP-45 COUNT ────────────────────────────────────────────

    #[test]
    fn count_matches_query_length_for_a_simple_filter() {
        let conn = test_conn();
        let kp = keypair(1);
        store_event(&conn, &signed(&kp, 1, 1000, vec![], "a"), false).unwrap();
        store_event(&conn, &signed(&kp, 1, 2000, vec![], "b"), false).unwrap();
        store_event(&conn, &signed(&kp, 7, 3000, vec![], "reaction"), false).unwrap();

        let f = Filter {
            kinds: Some(vec![1]),
            ..Default::default()
        };
        let result = count_events(&conn, std::slice::from_ref(&f), None).unwrap();
        assert_eq!(result.count, 2);
        assert!(!result.approximate);
        assert_eq!(result.count, query_events(&conn, &[f], 100).unwrap().len());
    }

    #[test]
    fn count_empty_filters_counts_everything() {
        let conn = test_conn();
        let kp = keypair(1);
        store_event(&conn, &signed(&kp, 1, 1000, vec![], "a"), false).unwrap();
        store_event(&conn, &signed(&kp, 7, 2000, vec![], "b"), false).unwrap();
        let result = count_events(&conn, &[], None).unwrap();
        assert_eq!(result.count, 2);
    }

    #[test]
    fn count_applies_the_gift_wrap_recipient_gate() {
        let recipient = keypair(1);
        let other = keypair(2);
        let sender = keypair(3);
        let recipient_hex = recipient.public_key_hex();
        let conn = test_conn();
        let wrap = signed(
            &sender,
            1059,
            1000,
            vec![Tag::new(vec!["p".into(), recipient_hex.clone()])],
            "ciphertext",
        );
        store_event(&conn, &wrap, false).unwrap();

        let f = Filter {
            kinds: Some(vec![1059]),
            ..Default::default()
        };
        assert_eq!(
            count_events(&conn, std::slice::from_ref(&f), Some(&recipient_hex))
                .unwrap()
                .count,
            1,
            "the authed recipient sees their own gift wrap counted"
        );
        assert_eq!(
            count_events(&conn, std::slice::from_ref(&f), None)
                .unwrap()
                .count,
            0,
            "an unauthenticated caller learns nothing about inbox size"
        );
        assert_eq!(
            count_events(&conn, &[f], Some(&other.public_key_hex()))
                .unwrap()
                .count,
            0,
            "a different authed pubkey learns nothing about someone else's inbox"
        );
    }

    #[test]
    fn count_flags_approximate_when_the_scan_cap_truncates() {
        let conn = test_conn();
        let kp = keypair(1);
        for i in 0..5u64 {
            store_event(&conn, &signed(&kp, 1, 1000 + i, vec![], "x"), false).unwrap();
        }
        let result = count_events_with_scan_cap(&conn, &[Filter::default()], None, 2).unwrap();
        assert!(
            result.approximate,
            "the scan hit its cap before seeing all rows"
        );
        assert_eq!(result.count, 2, "only the capped scan's rows are counted");

        let exact = count_events_with_scan_cap(&conn, &[Filter::default()], None, 100).unwrap();
        assert!(!exact.approximate);
        assert_eq!(exact.count, 5);
    }

    // ---- NIP-50 search (`nostr.md` § The relay event store — NIP-50 bullet) ----

    fn search_filter(q: &str) -> Filter {
        Filter {
            search: Some(q.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn search_narrows_to_matching_content() {
        let conn = test_conn();
        let kp = keypair(1);
        store_event(
            &conn,
            &signed(&kp, 1, 1000, vec![], "Company picnic on Saturday"),
            false,
        )
        .unwrap();
        store_event(
            &conn,
            &signed(&kp, 1, 1001, vec![], "quarterly report draft"),
            false,
        )
        .unwrap();

        let got = query_events(&conn, &[search_filter("PICNIC")], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].content.contains("picnic"));

        let none = query_events(&conn, &[search_filter("barbecue")], 100).unwrap();
        assert!(none.is_empty());

        // Empty / extensions-only search adds no MATCH clause: matches all.
        let all = query_events(&conn, &[search_filter("include:spam")], 100).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn search_never_reaches_gift_wraps() {
        let conn = test_conn();
        let kp = keypair(1);
        // A kind-1059 wrap whose (normally ciphertext) content carries a
        // searchable token — the FTS corpus must exclude it at index time,
        // so even a token-matching search never returns it.
        let wrap = signed(
            &kp,
            1059,
            1000,
            vec![Tag::new(vec!["p".to_string(), "c".repeat(64)])],
            "picnic-token-inside-a-wrap",
        );
        assert_eq!(
            store_event(&conn, &wrap, false).unwrap(),
            StoreOutcome::Stored
        );

        let got = query_events(&conn, &[search_filter("picnic-token-inside-a-wrap")], 100).unwrap();
        assert!(got.is_empty(), "a gift wrap must be invisible to search");

        // Sanity: the wrap is still served to non-search queries (the
        // recipient gate is the caller's job, unchanged).
        let plain = query_events(&conn, &[Filter::default()], 100).unwrap();
        assert_eq!(plain.len(), 1);
    }

    #[test]
    fn replacement_keeps_search_in_lockstep() {
        let conn = test_conn();
        let kp = keypair(1);
        store_event(&conn, &signed(&kp, 0, 1000, vec![], "alpha profile"), false).unwrap();
        store_event(&conn, &signed(&kp, 0, 2000, vec![], "beta profile"), false).unwrap();

        assert!(
            query_events(&conn, &[search_filter("alpha")], 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            query_events(&conn, &[search_filter("beta")], 100)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn deletion_keeps_search_in_lockstep() {
        let conn = test_conn();
        let kp = keypair(1);
        let ev = signed(&kp, 1, 1000, vec![], "delete me picnic");
        store_event(&conn, &ev, false).unwrap();
        assert_eq!(
            query_events(&conn, &[search_filter("picnic")], 100)
                .unwrap()
                .len(),
            1
        );

        let del = signed(
            &kp,
            5,
            1001,
            vec![Tag::new(vec!["e".to_string(), ev.id.clone()])],
            "",
        );
        store_event(&conn, &del, false).unwrap();
        apply_deletion(&conn, &del).unwrap();

        assert!(
            query_events(&conn, &[search_filter("picnic")], 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn expiration_sweep_keeps_search_in_lockstep() {
        let conn = test_conn();
        let kp = keypair(1);
        let future = (crate::db::now_epoch_secs() + 3600) as u64;
        let ev = signed(
            &kp,
            1,
            1000,
            vec![Tag::new(vec!["expiration".to_string(), future.to_string()])],
            "ephemeral picnic",
        );
        store_event(&conn, &ev, false).unwrap();
        assert_eq!(
            query_events(&conn, &[search_filter("picnic")], 100)
                .unwrap()
                .len(),
            1
        );

        // Force-expire the row, then sweep: the FTS entry must go with it.
        conn.execute(
            "UPDATE nostr_events SET expiration = 1 WHERE id = ?1",
            [&ev.id],
        )
        .unwrap();
        sweep_expired(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nostr_event_fts WHERE nostr_event_fts MATCH '\"picnic\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "swept row must leave no FTS entry behind");
    }

    #[test]
    fn expiration_sweep_never_touches_the_dm_plane() {
        // A NIP-40
        // short-expiry wrap's `nostr_events` row is swept, but the DM it
        // carried — a bridged-conversation row deposited by the Nostr leg — is
        // the owner's durable DM; sweeping it would lose real mail. The DM
        // plane is bounded by the family's own caps, never by source-expiry.
        let conn = test_conn();
        let kp = keypair(9);
        let ev = signed(&kp, 1059, 1000, vec![], "wrap");
        store_event(&conn, &ev, false).unwrap();
        conn.execute_batch(
            "INSERT INTO bridge_conversation_rooms
                 (room_id, actor_id, bridge_principal_id, bridge_id, far_room_id,
                  capabilities, bridge_x25519, created_at, last_at)
             VALUES (x'01', x'A1', x'51', 'nostr', 'p1', '{}', x'00', 1, 1);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bridge_conversation_messages
                 (room_id, actor_id, direction, far_message_id, sender, sealed_content,
                  created_at, received_at)
             VALUES (x'01', x'A1', 'in', ?1, 'p1', x'00', 1000, 1000)",
            [&ev.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE nostr_events SET expiration = 1 WHERE id = ?1",
            [&ev.id],
        )
        .unwrap();

        assert_eq!(sweep_expired(&conn).unwrap(), 1);

        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM nostr_events", [], |r| r.get(0))
            .unwrap();
        let dms: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_conversation_messages",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(events, 0, "the expired event row is swept");
        assert_eq!(dms, 1, "the sealed DM row survives the sweep");
    }

    #[test]
    fn hostile_search_strings_never_error() {
        let conn = test_conn();
        let kp = keypair(1);
        store_event(&conn, &signed(&kp, 1, 1000, vec![], "plain note"), false).unwrap();

        for q in [
            "a\" OR \"b",
            "NEAR(x, y)",
            "content:leak",
            "* ^ NOT plain",
            "\"\"\"",
            "(",
        ] {
            let r = query_events(&conn, &[search_filter(q)], 100);
            assert!(r.is_ok(), "hostile search {q:?} errored: {r:?}");
        }
    }

    #[test]
    fn count_honors_search() {
        let conn = test_conn();
        let kp = keypair(1);
        store_event(&conn, &signed(&kp, 1, 1000, vec![], "picnic one"), false).unwrap();
        store_event(&conn, &signed(&kp, 1, 1001, vec![], "picnic two"), false).unwrap();
        store_event(&conn, &signed(&kp, 1, 1002, vec![], "unrelated"), false).unwrap();

        let r = count_events(&conn, &[search_filter("picnic")], None).unwrap();
        assert_eq!(r.count, 2);
    }
}
