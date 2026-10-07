//! SQLite-backed storage for the Node.

pub mod account_state;
pub mod actor_tables;
pub mod admin;
pub mod atproto_identities;
pub mod atproto_pds;
pub mod atproto_projection;
pub mod backup_destinations;
pub mod backup_writer_grants;
pub mod blob_withhold;
pub mod blobs;
pub mod bridge;
pub mod bridge_audit;
pub mod bridge_authors;
pub mod bridge_blobs;
pub mod bridge_caldav;
pub mod bridge_carddav;
pub mod bridge_dav_common;
pub mod bridge_imap;
pub mod bridge_routing;
pub mod bridge_search;
pub mod bridge_service_users;
pub mod bridged_conversations;
pub mod caldav_enable;
pub mod caldav_port;
pub mod capability_grants;
pub mod carddav_enable;
pub mod chain_version;
pub mod channels;
pub mod contacts;
pub mod content;
pub mod content_index_rail;
pub mod conv_attachment_refs;
pub mod custody_hosting;
pub mod custody_receipts;
pub mod delivery;
pub mod domain_expiry;
pub mod drafts;
pub mod email;
pub mod engagement;
pub mod exchange_peers;
pub mod family;
pub mod feature_gate;
pub mod feeds;
pub mod folder_deposit_inbox;
pub mod forward_queue;
pub mod fts;
pub mod generation_escrow;
#[cfg(test)]
mod genesis_shape;
pub mod inbox;
pub mod labelers;
pub mod links;
pub mod mail_account;
pub mod mail_aliases;
pub mod mail_deliverability;
pub mod mail_domain_renames;
pub mod mail_domains;
pub mod mail_enable;
pub mod mail_export;
pub mod mail_health;
pub mod mail_import;
pub mod mail_lists;
pub mod mail_policy;
pub mod mail_serving;
pub mod mail_srs;
pub mod mail_warmup;
pub mod membership;
pub mod meta;
pub mod migrations;
pub mod mls_replica;
pub mod model_versions;
pub mod moderation;
pub mod nest_backup_keys;
pub mod nest_host_address;
pub mod nest_nat_mode;
/// The append-only deployment-seed rotation log + the ceremony's one
/// transaction (`nest/box-recovery.md` § Deployment-seed rotation).
pub mod nest_rotation;
pub mod nest_trust;
pub mod node_policy;
pub mod notifications;
pub mod operations;
pub mod outbound;
pub mod pairing;
pub mod payments;
pub mod pending_actions;
pub mod personalization;
pub mod posts;
pub mod public_servability;
pub mod push;
pub mod recovery_escrow;
pub mod recovery_pending;
pub mod recovery_registrations;
pub mod region_tier;
pub mod reports;
pub mod room_labels;
pub mod rooms;
pub mod rpc_idempotency;
pub mod schema;
pub mod search;
pub mod sender_behavior;
pub mod serving_port;
pub mod share_tokens;
pub mod signals;
pub mod singleton;
pub mod snapshots;
pub mod spam_baseline;
pub mod subscriptions;
pub mod successions;
pub mod sync_storage;
pub mod third_party_principals;
pub mod transport_policy;
pub mod trends;
pub mod web;
pub mod web_apex;
pub mod web_subdomain;
pub mod webdav_enable;

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::segments::records_db::SegmentStatsRow;

pub use feeds::ScoreCursor;
pub use sync_storage::{
    ConflictWinnerFacts, FolderOptions, FolderUpdate, GenerationRestore, GrantRevokeOutcome,
    GrantStoreOutcome, ReportRowSignatures, RowSignature, VersionPruneCandidate,
};

/// Extracted metadata from a decoded Post, used by the indexer.
pub(crate) struct PostMetadata {
    author: [u8; 32],
    /// Epoch **microseconds**, straight off `Post::created_at`
    /// (`fauna_core::data::Timestamp`) — the unit `content.created_at` stores.
    pub(crate) created_at: i64,
    has_media: i64,
    is_reply: i64,
    tags: Vec<String>,
    source: String,
    /// 32-byte content id (BLAKE3 digest) of the first `Reference::Quote`
    /// target, projected into a `content_links link_type='quote'` row so the
    /// feed read model can surface the quoted-post embed without reading
    /// `content.payload` (`feed.md` § The read model). `None` for non-quoting
    /// posts.
    quoted_post_id: Option<[u8; 32]>,
    /// 32-byte content id of the first `Reference::Repost` target, projected
    /// into an **actor-keyed** `content_links link_type='repost'` row (the
    /// actor column is what makes the per-viewer `viewer_repost_id` read one
    /// indexed lookup — `ui/feed.md` § Interaction bar → Repost, ratified
    /// 2026-08-10). `None` for non-reposting posts.
    reposted_post_id: Option<[u8; 32]>,
    /// Tier name of a gated-to-tier post (`Post.gated.tier` — plaintext-floor
    /// attribute, `ui/feed.md` § Encryption at rest), projected into
    /// `content_meta.gated_tier` so the feed read model serves the
    /// `gated-post-badge` without reading the body. `None` for public posts.
    gated_tier: Option<String>,
    /// The 32-byte channel id of the room a **room-restricted** post
    /// addresses (`KeyAccess::Room.group_id`, read through the shared
    /// `room_post_of` — the same plaintext floor as the tier), projected into
    /// `content_meta.gated_room` so a member's card can name the room. `None`
    /// for every other post, and for a room arm that names no 32-byte room.
    gated_room: Option<[u8; 32]>,
    /// The list-card text, projected into `content_meta.preview` (`ui/feed.md`
    /// § The read model → *The list-card preview*): the first
    /// [`PREVIEW_CHARS`] characters of `Post::body_text()` — exactly the
    /// string a native post's FTS row indexes, so the projection holds no
    /// plaintext it did not hold before.
    preview: String,
}

/// How many characters of `Post::body_text()` the list card carries — the
/// `substr(f.body, 1, 500)` the feed reads took from the FTS row before the
/// column, counted in characters as SQLite's `substr` counts them.
pub(crate) const PREVIEW_CHARS: usize = 500;

/// The list-card preview of a post body: its first [`PREVIEW_CHARS`]
/// characters, cut on a character boundary.
pub(crate) fn post_preview(body_text: &str) -> String {
    body_text.chars().take(PREVIEW_CHARS).collect()
}

/// Extract indexable metadata from a decoded Post.
///
/// Every derivation here is a shared `fauna_core::data::Post` accessor rather
/// than a local facet/reference walk: the client-side single-post projection
/// (`libs/fauna-feed`'s `map_fetched_post`, the search deep-link path) reads the
/// same fields off the same decoded post, so a second walk here would let the
/// two ends drift on which body variants carry facets, which references count as
/// a reply, or how a quote CID is stripped — and a post would then render
/// differently depending on whether the feed query or a `fauna.posts.get`
/// delivered it (priority #4).
pub(crate) fn extract_post_metadata(post: &fauna_core::data::Post) -> PostMetadata {
    PostMetadata {
        author: post.author.0,
        created_at: post.created_at.0 as i64,
        has_media: post.has_media() as i64,
        is_reply: post.is_reply() as i64,
        tags: post.indexed_tags(),
        // The post's own origin platform when it carries one (an archive
        // import — `archive-import.md` § What each category becomes), else the
        // native token. A shared `Post` accessor like every other field here.
        // Bridge ingest overrides this with its own token afterwards
        // (`put_post_with_source*`), which is why a bridged post never reads
        // as native even if it carried an origin.
        source: post.source_token(),
        quoted_post_id: post.quoted_post_id(),
        reposted_post_id: post.reposted_post_id(),
        gated_tier: post.gated.as_ref().map(|g| g.tier.clone()),
        // The room arm's channel id, by the one accessor every reader of a
        // room post shares (`fauna_core::room_post`), so the index and the
        // client's own decode name the same room.
        gated_room: post
            .gated
            .as_ref()
            .and_then(|g| fauna_core::room_post::room_post_of(&g.key_access))
            .map(|(room, _)| room),
        preview: post_preview(&post.body_text()),
    }
}

/// Derive content schema string from a Post's body variant.
fn post_body_schema(post: &fauna_core::data::Post) -> &'static str {
    use fauna_core::data::PostBody;
    match &post.body {
        PostBody::Text { .. } => "post/text",
        PostBody::Media { .. } => "post/media",
        PostBody::TextWithMedia { .. } => "post/text_with_media",
        PostBody::Structured { .. } => "post/structured",
        PostBody::Video { .. } => "post/video",
    }
}

/// Compute a deterministic 32-byte content ID for non-post content types (profiles, etc.).
pub(crate) fn content_id_for_document(content_type: &str, content_id_str: &str) -> [u8; 32] {
    let input = format!("{content_type}:{content_id_str}");
    *blake3::hash(input.as_bytes()).as_bytes()
}

/// Write post metadata to content_meta + content_links (tags).
/// Caller must already hold the connection lock.
///
/// **Every writer that wants its content in a feed must call this** — it is
/// what mints the `content_meta` row `query_feed` INNER JOINs. A writer that
/// only calls `content::insert_content` produces a row reachable by
/// `fauna.posts.get`/`list_posts_by_author` and invisible to every feed.
pub(crate) fn write_post_index(
    conn: &Connection,
    post_id: &[u8; 32],
    meta_data: &PostMetadata,
) -> Result<()> {
    meta::upsert_meta(
        conn,
        post_id,
        0.0,
        meta_data.has_media != 0,
        meta_data.is_reply != 0,
        meta_data.gated_tier.as_deref(),
        meta_data.gated_room.as_ref(),
        Some(meta_data.preview.as_str()),
    )
    .context("upsert content_meta")?;

    links::delete_links_by_source(conn, post_id.as_slice(), "tag").context("delete old tags")?;

    let now = now_epoch_millis();
    for tag in &meta_data.tags {
        let _ = links::insert_link(
            conn,
            "tag",
            Some(post_id.as_slice()),
            None,
            None,
            Some(tag.as_str()),
            None,
            now,
        );
    }

    // Quoted-post projection: a `content_links link_type='quote'` row pointing
    // at the quote target's 32-byte content id, so `query_feed` can surface the
    // embed from the index alone (never `content.payload` — `feed.md` § read
    // model). Re-index is idempotent (delete-then-insert).
    links::delete_links_by_source(conn, post_id.as_slice(), "quote")
        .context("delete old quote link")?;
    if let Some(target) = &meta_data.quoted_post_id {
        let _ = links::insert_link(
            conn,
            "quote",
            Some(post_id.as_slice()),
            Some(target.as_slice()),
            None,
            None,
            None,
            now,
        );
    }

    // Reposted-post projection — the quote twin, with one deliberate extra:
    // the row is **actor-keyed** (actor = the repost's author), so the
    // per-viewer `viewer_repost_id` read is a single indexed lookup on
    // `(target_id, link_type, actor_id)` instead of a join through `content`
    // (`feed.md` § Interaction bar → Repost, ratified 2026-08-10).
    links::delete_links_by_source(conn, post_id.as_slice(), "repost")
        .context("delete old repost link")?;
    if let Some(target) = &meta_data.reposted_post_id {
        let _ = links::insert_link(
            conn,
            "repost",
            Some(post_id.as_slice()),
            Some(target.as_slice()),
            Some(meta_data.author.as_slice()),
            None,
            None,
            now,
        );
    }
    Ok(())
}

pub struct NestPairing {
    pub actor_id: Vec<u8>,
    pub private_nest_id: Vec<u8>,
    pub capabilities: Vec<String>,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub nest_url: Option<String>,
    /// User-supplied display name for the linked nest (`fauna.pair.add`).
    pub label: Option<String>,
}

pub struct OutboxEntry {
    pub id: i64,
    pub payload: Vec<u8>,
    pub entry_type: String,
    pub attempts: i64,
    /// The author the producer stamped — whose pairing rows say where the
    /// entry goes. `None` only for a row written before the stamp existed.
    pub author_id: Option<Vec<u8>>,
}

/// One author's view of the forward queue — what their app renders on the
/// Nests page (`private-mode.md` § Post Forwarding → the queue is the user's
/// to see). Read by [`CacheDb::outbox_status_for_author`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutboxAuthorStatus {
    /// Every entry this author has queued, backed-off ones included.
    pub queued: i64,
    /// Those past the retry ceiling — refusals that have outlasted the whole
    /// backoff and are still retrying.
    pub stuck: i64,
    /// The most recent failure the worker recorded on any of them; `None`
    /// until a send has failed.
    pub last_error: Option<String>,
}

pub struct NamespaceEntry {
    pub entry_id: Vec<u8>,
    pub seq: i64,
    pub ciphertext: Vec<u8>,
    pub actor_sig: Vec<u8>,
    pub source: String,
    pub updated_at: i64,
}

pub struct CacheDb {
    conn: Mutex<Connection>,
}

pub(crate) fn now_epoch_millis() -> i64 {
    fauna_core::data::Timestamp::now_millis() as i64
}

/// Epoch microseconds — the unit `content.created_at` stores and
/// `fauna_core::data::Timestamp` declares. Reach for this, not
/// [`now_epoch_millis`], whenever the value lands in a `content` row.
pub(crate) fn now_epoch_micros() -> i64 {
    fauna_core::data::Timestamp::now().as_i64()
}

pub(crate) fn now_epoch_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

/// Convert a blob column's bytes into a fixed-size array outside a rusqlite
/// row-mapper closure (once the value is already an owned `Vec<u8>`/`&[u8]`),
/// naming the field in the error on a length mismatch. Twin of
/// [`blob_col_to_array`] for callers already holding a plain `anyhow::Result`
/// rather than a rusqlite one, and with no column index to report.
pub(crate) fn blob_to_array<const N: usize>(blob: &[u8], field: &str) -> anyhow::Result<[u8; N]> {
    blob.try_into()
        .map_err(|_| anyhow::anyhow!("{field} wrong length: {} bytes (want {N})", blob.len()))
}

/// Does this connection's schema carry `table`?
///
/// The bridge tables (`nostr_*`, and the atproto/activitypub families beside
/// them) are created by their feature's own `init_db`, so a nest built
/// without that feature — or one that has simply never run the bridge — does
/// not have them. Statements against them must therefore be guarded rather
/// than assumed, exactly as [`actor_tables::purge_orphaned_actor_rows`]
/// guards its deletes: inside a transaction, one `no such table` aborts
/// everything around it.
pub(crate) fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            rusqlite::params![table],
            |_| Ok(true),
        )
        .optional()
        .with_context(|| format!("check {table} exists"))?
        .unwrap_or(false))
}

/// Convert a rusqlite BLOB column into a fixed-size array inside a row
/// mapper, producing a `FromSqlConversionFailure` naming the field and the
/// observed/expected lengths on mismatch. `col` is the 0-based column index
/// for error reporting (surfaced verbatim in the rusqlite error). Twin of
/// [`blob_to_array`] for callers inside a `rusqlite::Result`-returning row
/// mapper, where the column index is available and worth reporting.
pub(crate) fn blob_col_to_array<const N: usize>(
    blob: Vec<u8>,
    col: usize,
    field: &str,
) -> rusqlite::Result<[u8; N]> {
    blob.try_into().map_err(|v: Vec<u8>| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Blob,
            format!("{field} wrong length: {} bytes (want {N})", v.len()).into(),
        )
    })
}

/// Spawns a tokio task that ticks every `retention / 24` (skipping the
/// immediate first tick) and awaits `sweep_once` on each tick, forever.
/// This module's per-table retention sweepers all call this — each supplies
/// its own cutoff computation, prune call, and result logging as the closure
/// (cutoff units and log fields differ per table); only the
/// timer/skip-first-tick/loop shape is shared, via
/// [`crate::sweeper::spawn_periodic_sweeper`] (the crate-wide primitive
/// behind every periodic sweeper, not just this module's retention ones).
pub(crate) fn spawn_retention_sweeper<F, Fut>(
    retention: std::time::Duration,
    sweep_once: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let interval = retention.checked_div(24).unwrap_or(retention);
    crate::sweeper::spawn_periodic_sweeper(interval, true, sweep_once)
}

/// Map a DB blob to Option<Vec<u8>> (empty blob = None).
fn author_id_from_db(blob: Vec<u8>) -> Option<Vec<u8>> {
    if blob.is_empty() { None } else { Some(blob) }
}

// ---------- Data types for admin API ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct AdminRow {
    pub id: i64,
    pub username: String,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TierRow {
    pub name: String,
    pub max_inbox_bytes: i64,
    pub max_storage_bytes: i64,
    pub max_devices: i64,
    pub max_blob_size: i64,
    pub max_feeds: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserRow {
    pub actor_id: Vec<u8>,
    pub tier: String,
    pub label: String,
    /// The account's handle. `None` (NULL) or `Some("")` both mean a
    /// handle-less admission — the wire projection folds either to absent
    /// (`AdminUser.handle`).
    pub handle: Option<String>,
    pub suspended: bool,
    pub created_at: i64,
    pub inbox_bytes_used: i64,
    pub storage_bytes_used: i64,
    pub eviction_status: String,
    pub eviction_reason: String,
    pub eviction_category: String,
    pub eviction_warned_at: Option<i64>,
    pub eviction_suspend_at: Option<i64>,
    pub eviction_delete_at: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Stats {
    pub total_users: i64,
    pub users_by_tier: Vec<(String, i64)>,
    pub suspended_users: i64,
    pub total_inbox_bytes: i64,
    pub total_storage_bytes: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuditRow {
    pub id: i64,
    pub ts: i64,
    pub actor_id: Option<Vec<u8>>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<String>,
    pub prev_hash: String,
    pub entry_hash: String,
    /// Which preimage `entry_hash` was computed under
    /// ([`chain_version::ChainVersion`]). Served alongside the hash because an
    /// admin verifies by recomputing from the columns they were served: without
    /// this, the recomputation is back to guessing the format, which is the one
    /// thing the record exists to prevent.
    pub entry_hash_version: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlobMetadataRow {
    pub hash: Vec<u8>,
    pub size_bytes: i64,
    pub content_type: String,
    pub created_at: i64,
    pub last_accessed: i64,
    pub storage_local: bool,
    pub storage_s3: bool,
    pub storage_nodes: Option<String>,
    pub ref_count: i64,
    pub has_c2pa: Option<bool>,
    pub thumbnail_hash: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlobStorageStats {
    pub total_blobs: i64,
    pub total_bytes: i64,
    pub local_blobs: i64,
    pub s3_blobs: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InviteCodeRow {
    pub code: String,
    pub tier: String,
    pub uses_left: i64,
    pub created_at: i64,
    /// Supervised admission: the guardian the redeemed account is linked to
    /// (`family-safety.md` § Wire & data shape). `None` = ordinary code.
    #[serde(default)]
    pub guardian_actor: Option<Vec<u8>>,
    /// The age band the code admits under (`family-safety.md` § The account
    /// age band — the guardian's dial; wire token, validated at mint).
    /// `None` = no band chosen.
    #[serde(default)]
    pub age_band: Option<String>,
}

/// What a valid invite code grants at redemption — the read `peek_invite_code`
/// / `validate_invite_code` answer with. A struct rather than a widening tuple
/// so a new admission-carry field cannot be positionally transposed at a call
/// site (`family-safety.md` § The account age band grew the carry 2026-08-24).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteCodeGrant {
    pub tier: String,
    /// Supervised admission: the guardian the redeemed account is linked to.
    pub guardian_actor: Option<Vec<u8>>,
    /// The band the code admits under (wire token; `guardian-asserted`
    /// provenance at redemption). Only ever present beside `guardian_actor`
    /// (mint-validated).
    pub age_band: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct InviteRequestRow {
    pub id: i64,
    pub actor_id: Vec<u8>,
    pub handle: String,
    pub message: String,
    pub status: String,
    pub created_at: i64,
    pub decided_at: Option<i64>,
    pub decided_by: Option<Vec<u8>>,
    pub denial_reason: Option<String>,
    /// The applicant's age-claim band, when the submit carried one —
    /// absence-as-signal for the deciding admin (`public-mode.md` § Age at
    /// registration). Wire token, validated at submit.
    #[serde(default)]
    pub age_band: Option<String>,
    /// How that claim was established: `attested-ios` / `attested-android`
    /// for a claim the nest verified at submit, `none` for declared-only.
    #[serde(default)]
    pub age_provenance: Option<String>,
}

/// One file version — a live `sync_changes` row projected as a version
/// (file-sync.md § File Versions, ratified 2026-07-09). `version_num` is the
/// recording row's `seq` (stable, never renumbered).
#[derive(Debug, Serialize, Deserialize)]
pub struct FileVersionRow {
    pub path_hash: Vec<u8>,
    pub version_num: i64,
    pub manifest_hash: Vec<u8>,
    pub size_bytes: i64,
    pub created_at: i64,
    /// The set the version belongs to — the handler authz-checks and names it.
    pub folder_id: i64,
    /// M2 content-key generation the chunks were sealed under (echoed on the
    /// wire so a restore record can carry it verbatim).
    pub content_key_version: Option<i64>,
    /// The recording actor (`sync_changes.actor_id`) — the wire
    /// `author_actor_id` attribution (multi-writer Phase 1).
    pub actor_id: Vec<u8>,
    /// Epoch-millis soft-prune stamp (`sync_changes.pruned_at`); `Some` only on
    /// rows an `include_pruned` recovery browse serves (file-versions.md
    /// § Retention (3)). The default projection never returns such a row.
    pub pruned_at: Option<i64>,
    /// The 30-day recovery deadline (`sync_changes.purge_after`, epoch secs);
    /// set/cleared together with [`Self::pruned_at`].
    pub purge_after: Option<i64>,
    // The rest of the version row's writer-signed statement
    // (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    // ruling (2)) — projected so a restore can verify the version it re-points.
    pub device_id: Option<Vec<u8>>,
    pub change_type: String,
    pub path_sealed: Option<Vec<u8>>,
    pub thumbnail_hash: Option<String>,
    pub derived_through: Option<i64>,
    pub is_resolution: Option<bool>,
    pub signature: Option<Vec<u8>>,
    pub signer_key: Option<Vec<u8>>,
    /// `Some(true)` on a conflict report's retained loser — a covered field of
    /// its reporter's statement (ruling (10)(d)), projected so the reader
    /// judges the version by that signature (ruling (10)(e)).
    pub is_retention: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncDeviceRow {
    pub actor_id: Vec<u8>,
    pub device_id: Vec<u8>,
    pub label: String,
    /// [`Self::label`] sealed under the registering owner's root — opaque to the
    /// nest, forwarded verbatim to the owner's own client, which renders it
    /// through `fauna_core::label_custody::render_device_label`
    /// (`file-sync.md` § Sealed names & paths). `None` on a machine-authored
    /// label or a keyless writer's registration.
    pub label_sealed: Option<Vec<u8>>,
    pub registered_at: i64,
    pub last_seen: i64,
    pub capabilities: String,
    /// The device principal the enrollment ceremony granted on this row
    /// (`sync_devices.auth_device_key` — the T10 writer key's public, the
    /// identity the generation plane's wraps target). `None` on a registered-but-not-enrolled
    /// device row no principal has enrolled on. Read-only surface: the badge derivations
    /// key on it (`ui/devices.md` § Custody facet piece 1).
    pub principal: Option<Vec<u8>>,
    /// The guardian-enrolled-device marker (`family-safety.md` § Full
    /// visibility). Meaningful only while the owning actor is supervised.
    pub guardian_marked: bool,
    /// The device's own last report of whether it runs its peer listeners
    /// (`p2p.md` § Per-device participation); `None` = never reported.
    pub p2p_participation: Option<bool>,
    /// A pending brake another of the account's devices raised, not yet
    /// folded by this one.
    pub p2p_off_requested: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncChangeRow {
    pub seq: i64,
    pub path_hash: Vec<u8>,
    pub manifest_hash: Option<Vec<u8>>,
    pub size_bytes: i64,
    pub change_type: String,
    pub created_at: i64,
    pub path: Option<String>,
    pub device_id: Option<Vec<u8>>,
    /// M2 content-key generation these chunks were sealed under (shared file
    /// sets); `None` for owner-only sets and deletes. The nest
    /// stores it opaque and echoes it back so the reader selects `key_for`.
    pub content_key_version: Option<i64>,
    /// Hex thumbnail-blob hash the uploader recorded (the
    /// `UploadSidecar.thumbnail_hash` `?thumb=1` pointer); `None` until a producer
    /// supplies one. Stored opaque, surfaced via `fauna.media.list`.
    pub thumbnail_hash: Option<String>,
    /// The `sync_changes.actor_id` column — the authenticated connection actor
    /// that recorded the change (multi-writer Phase 1 attribution; the wire
    /// `SyncChange.author_actor_id`). On every pre-multi-writer row this is the
    /// set owner, the only possible recorder then.
    pub actor_id: Vec<u8>,
    /// The client's opaque `SealedLabel` envelope over `path`
    /// (`docs/goal/behavior/file-sync.md` § Sealed names & paths). Stored and
    /// echoed verbatim — the nest holds no key that opens it. `None` on every
    /// row recorded before the expand phase and from any keyless writer.
    pub path_sealed: Option<Vec<u8>>,
    /// Causal watermark — client-stamped, stored opaque, echoed verbatim
    /// (`SyncChange::derived_through` carries the wire contract). `None` =
    /// unknown causality (a writer that sends none, e.g. the peer-sync relay).
    pub derived_through: Option<i64>,
    /// Resolution marker (`SyncChange::is_resolution`), stored/echoed opaque.
    pub is_resolution: Option<bool>,
    /// Retention marker (`SyncChange::is_retention` — loser-row ruling,
    /// 2026-08-05): set only by the nest itself, on the resolved report's
    /// loser-retention row. Echoed on list; receivers account-and-skip.
    pub is_retention: Option<bool>,
    /// W2.3 (account-data-plane.md § Workstreams) item-class discriminator (`fauna_protocol::account_state::
    /// ItemClass`); `None` = the shipped file-row semantics.
    pub item_class: Option<String>,
    /// W2.3 authoring writer id (32 bytes) for a relayed device-writer row;
    /// `None` = the nest is the writer, whose coordinate is `seq`.
    pub origin_writer: Option<Vec<u8>>,
    /// W2.3 authoring writer's own log sequence; `None` alongside a `None`
    /// [`Self::origin_writer`].
    pub origin_seq: Option<i64>,
    /// W2.3 sealed class-2 entry, stored and echoed verbatim — the nest holds
    /// no key that opens it. `None` on every non-`state-entry` row.
    pub entry_sealed: Option<Vec<u8>>,
    /// The writer's Ed25519 signature over the row's `SignedChange` statement
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*),
    /// verified at ingest and echoed verbatim. `None` on the exempt classes.
    pub signature: Option<Vec<u8>>,
    /// The key [`Self::signature`] verifies under — the device principal key,
    /// or the actor id itself for a direct signature.
    pub signer_key: Option<Vec<u8>>,
}

impl SyncChangeRow {
    /// The row's variable-length wire footprint, for the serve page's frame
    /// budget (`crate::segments::take_page_within_budget`).
    ///
    /// Sums **every** variable-length field rather than naming two of them, so
    /// the budget cannot silently under-count as the row grows — which matters
    /// most as the sealed-names expand phase proceeds: `path_sealed` will carry
    /// the bytes that `path` carries today (`docs/goal/behavior/file-sync.md`
    /// § Sealed names & paths), and a budget hard-coded to `path.len()` would
    /// read a sealed-only row as nearly free and overflow the frame. The
    /// fixed-width scalars are covered by
    /// [`crate::segments::RECORD_WIRE_OVERHEAD`], which the caller adds.
    pub fn wire_len(&self) -> usize {
        self.path_hash.len()
            + self.manifest_hash.as_ref().map_or(0, |h| h.len())
            + self.change_type.len()
            + self.path.as_ref().map_or(0, |p| p.len())
            + self.device_id.as_ref().map_or(0, |d| d.len())
            + self.thumbnail_hash.as_ref().map_or(0, |t| t.len())
            + self.actor_id.len()
            // Both sealed envelopes ride the row verbatim, so both are part of
            // its wire footprint. `entry_sealed` is the load-bearing one: a
            // class-2 entry is bounded by `MAX_STATE_ENTRY_BYTES` (64 KiB), so
            // omitting it would let a page of state entries overrun the frame
            // budget the paging helper exists to respect. `path_sealed` was
            // already missing here and is the same one-line class — a few
            // hundred bytes per row, only ever making a page more conservative.
            + self.path_sealed.as_ref().map_or(0, |p| p.len())
            + self.entry_sealed.as_ref().map_or(0, |e| e.len())
            // The writer signature (64) + key (32) ride every signed row.
            + self.signature.as_ref().map_or(0, |s| s.len())
            + self.signer_key.as_ref().map_or(0, |k| k.len())
    }
}

pub struct SyncFileInfo {
    /// The resting plaintext path — `None` since the S9 flip scrubbed every
    /// sealed row (v32): present only on reserved-rail rows (machine-authored
    /// paths, deliberately plaintext) and public-audience rows. Readers
    /// render user paths from `path_sealed` client-side.
    pub path: Option<String>,
    pub manifest_hash: Vec<u8>,
    pub size_bytes: i64,
    pub updated_at: i64,
    /// Hex thumbnail-blob hash the uploader recorded, or `None` until a producer
    /// supplies one — threaded into `MediaItem.thumbnail_hash` by the media
    /// handler (`media.md` § State & data shape).
    pub thumbnail_hash: Option<String>,
    /// The M2 content-key **generation** this version's chunks were sealed under
    /// (`sync_changes.content_key_version`). `None` for owner-only sets, and for
    /// the `backup_custody` projection (which does not track it). Consumed by the
    /// WebDAV MDA serving path (`webdav_list_files`) to select `key_for(version)`
    /// on GET decrypt (`webdav-server.md` § Protocol surface); the media path
    /// ignores it.
    pub content_key_version: Option<i64>,
    /// The client's opaque `SealedLabel` envelope over `path`
    /// (`docs/goal/behavior/file-sync.md` § Sealed names & paths) — carried onto
    /// every listing so a sealed-first renderer never needs the plaintext
    /// column. `None` from keyless writers (accepted only on a plane that rests
    /// plaintext paths) and from the `backup_custody` projection, whose
    /// machine-authored segment paths are a declared non-seal.
    pub path_sealed: Option<Vec<u8>>,
    /// `path_hash` — the row's stable equality-only path key, `NOT NULL` on
    /// both projections. Two jobs, both of which outlive the plaintext column:
    /// it is the **convergent salt** a sealed-first renderer needs to open
    /// [`Self::path_sealed`] (`fauna.media.list` carries it onto the wire for
    /// exactly that reason), and it is the v2 keyset **sort key**.
    pub path_hash: Vec<u8>,
    // The rest of the head row's writer-signed statement
    // (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    // ruling (2)) — what `fauna.media.list` projects so a reader with no other
    // row source can verify the item.
    /// The recording sync device id.
    pub device_id: Option<Vec<u8>>,
    /// The recorder (`sync_changes.actor_id`) — the statement's signed actor.
    pub actor_id: Vec<u8>,
    pub change_type: String,
    pub derived_through: Option<i64>,
    pub is_resolution: Option<bool>,
    pub signature: Option<Vec<u8>>,
    pub signer_key: Option<Vec<u8>>,
}

// `Default` is for FIXTURES, not production: the one production constructor is
// `db::sync_storage`'s SQL row mapper, which sets every field. It is derived so a
// test row can name only the columns under test and two branches independently
// growing this struct still merge cleanly — a struct-update fixture
// (`..Default::default()`) does not conflict the way a hand-listed one does.
// Never build a row this way outside a test.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FolderRow {
    pub id: i64,
    pub name: String,
    pub actor_id: Vec<u8>,
    pub created_at: i64,
    pub node_cache: bool,
    /// `folders.custody_copy` — this reserved row is another location's blind
    /// sealed mirror, provisioned by this nest (`reserved-folders.md`
    /// § Destination capability). Read it only through
    /// [`crate::db::snapshots::is_reserved_custody_copy`].
    pub custody_copy: bool,
    pub retention_policy: Option<String>,
    pub cached_snapshot_count: i64,
    pub cached_total_bytes: i64,
    pub cached_last_snapshot_at: Option<i64>,
    pub include_paths: Option<String>,
    pub exclude_paths: Option<String>,
    pub high_cadence: bool,
    /// The MLS group this set is bound to, for a cross-user *shared* set
    /// (shared-folders Slice 1). `None` = owner-only (chunks on `BackupKey`);
    /// `Some(group_id)` = shared, chunks sealed under `chunk_crypto` keyed by the
    /// group's `export_chunk_key`. Additive — `key-material-hierarchy.md`
    /// § Audience: an MLS group at a specific epoch.
    pub mls_group_id: Option<Vec<u8>>,
    /// Whether the user has flagged this set "serve over WebDAV" — the per-set
    /// exposure gate (`webdav-server.md` § Independent enablement). `false` = not
    /// served (default); `true` = the MDA exposes it read/write to generic DAV
    /// clients. A reserved `__` set is never served; the deployment-wide
    /// `webdav_enabled` is the protocol switch, this is the per-set gate.
    pub webdav_enabled: bool,
    /// Per-set conflict policy wire string (`'auto'` | `'latest_wins_always'`,
    /// `fauna_core::format::ConflictPolicy` — file-sync.md § Conflicts,
    /// ratified 2026-07-10). The syncing device reads it off this
    /// authoritative row to decide how a detected conflict auto-resolves.
    pub conflict_policy: String,
    /// Web-paywall tier (monetization.md § Pillar 2, the folder half):
    /// `None` = not paywalled (a website folder serves publicly); `Some(tier)` =
    /// paywalled to the owner's named subscription tier. The entitlement seam
    /// the visitor token mint consults.
    pub web_paywall_tier: Option<String>,
    /// `folders.name_sealed` — the user-chosen [`Self::name`] sealed under the
    /// root that already seals this set's chunks (`file-sync.md` § Sealed names &
    /// paths). Opaque to the nest: stored, projected onto the wire, and never
    /// opened here. `None` for a reserved `__` set (a routing constant that never
    /// seals), and for any row no keyed writer has stamped yet.
    pub name_sealed: Option<Vec<u8>>,
    /// `folders.name_hash` — `fauna_core::path_crypto::set_name_hash(name)`.
    /// Two jobs, both of which outlive the plaintext column: the addressing +
    /// uniqueness key (`UNIQUE(name_hash, actor_id)`) and the convergent **salt**
    /// a sealed-first renderer needs to open [`Self::name_sealed`]. `Option`
    /// because reserved folders are inserted by name alone and carry no hash.
    pub name_hash: Option<Vec<u8>>,
    /// `folders.include_paths_sealed` — [`Self::include_paths`] sealed under
    /// the **owner's** `BackupKey::convergent_chunk_root()`, salted by
    /// [`Self::id`] (S6-c; `encryption-at-rest.md` § Carve-outs names this field
    /// *"the sharpest: the owner's absolute local filesystem layout"*). Opaque to
    /// the nest: stored, projected onto the **owner's** row only, never opened
    /// here.
    ///
    /// ⚠ Unlike [`Self::name_sealed`], this one is **owner-only rather than
    /// label-audience** — the projection that ships it must be `owner_summary`,
    /// never `member_summary`, which already withholds the plaintext. `None` for
    /// a row no keyed writer has stamped, and after any keyless write of the
    /// plaintext (the pair moves together — `db::FolderUpdate`).
    pub include_paths_sealed: Option<Vec<u8>>,
    /// `folders.exclude_paths_sealed` — the [`Self::include_paths_sealed`]
    /// twin, under its own field domain tag and the same owner-only audience.
    pub exclude_paths_sealed: Option<Vec<u8>>,
    /// `folders.retention_policy_sealed` — [`Self::retention_policy`] sealed
    /// under the set's **label-audience** root, salted by [`Self::name_hash`]
    /// (S6-e). Opaque to the nest: stored, projected, never opened here.
    ///
    /// ⚠ **Label-audience, so it ships on BOTH projection arms** — the opposite
    /// of [`Self::include_paths_sealed`] two fields up, and for a reason worth
    /// re-reading before copying either: `member_summary` withholds the
    /// include/exclude plaintext but ships `retention_policy` unmodified, so a
    /// member is inside this seal's audience and outside that one's. The salt is
    /// [`Self::name_hash`] rather than [`Self::id`] because retention is settable
    /// at create, when no id exists yet. `None` for a row no keyed writer has
    /// stamped, and after any keyless write of the plaintext (the pair moves
    /// together — `db::FolderUpdate`).
    pub retention_policy_sealed: Option<Vec<u8>>,
    /// `folders.nest_snapshots` — whether **the nest place** keeps snapshots of
    /// this folder (folders re-model phase 2 § Places → the nest place;
    /// `backup-restore.md` § 8 *The nest place's snapshot policy*). `Some(true)` /
    /// `Some(false)` = the owner chose; `None` = **unset**, resolving to the
    /// nest-wide behavior — which is what every folder whose
    /// owner never touched the knob rests at.
    ///
    /// ⚠ `Some(true)` is a *preference*, not a guarantee: the structural refusals
    /// in [`CacheDb::folder_needs_snapshot`] still apply on top (a reserved `__`
    /// destination set is never snapshotted, whatever this says). Read the
    /// resolved verdict from that method, never this column alone.
    pub nest_snapshots: Option<bool>,
    /// `folders.nest_snapshot_quiet_secs` — how long the nest place waits for
    /// quiet before cutting a snapshot. `None` = unset ⇒ the nest-wide scheduler
    /// quiet period (`SnapshotScheduler::quiet_secs`), which is the only cadence
    /// that existed before v37.
    pub nest_snapshot_quiet_secs: Option<i64>,
    /// `folders.version_retention` — the per-set version-retention bounds JSON
    /// (`fauna_protocol::folders::VersionRetention`; `file-versions.md`
    /// § Retention ruling 1 — the `retention_policy` SIBLING, never a re-map).
    /// `None` = keep everything, where every folder rests until its owner
    /// chooses. Parsed only by `backup::version_prune::parse_folder_version_retention`.
    pub version_retention: Option<String>,
    /// `folders.audience` (v41, phase 4) — the owner's explicit
    /// **declassification** and nothing else: `Some("public")` = world-readable
    /// by design, chunks and names/paths rest unsealed (`principles.md` § The
    /// user always controls their data, the one deliberate exception); `None` =
    /// not declassified. The wire tri-state is **derived** — public if this,
    /// else shared if [`Self::mls_group_id`] is `Some`, else private
    /// (`folder_handlers::audience_of` is the one derivation; never open-code
    /// it). Bound-ness stays authoritative in `mls_group_id`.
    pub audience: Option<String>,
    /// `folders.website_enabled` (v41, phase 4) — the per-folder website
    /// toggle that replaced `mode == "web"` as the `web_files` fan-out key.
    /// `true` ⇒ recorded changes fan out to `web_files` **in addition to** the
    /// head row (never instead — `web_files` is not a GC reachability source)
    /// and the folder serves as the owner's website, failing closed at serve
    /// time unless the content is openable (public plaintext, or paywalled
    /// under [`Self::web_paywall_tier`]'s grant).
    pub website_enabled: bool,
    /// `folders.public_floor_seq` (v44, phase 4 slice 4f-i) — the change-log
    /// seq this folder's head stood at when it was most recently declassified
    /// to `audience='public'`. The public read plane serves **only rows
    /// strictly above this**: what the owner declassified is the folder *from
    /// the flip forward*, never the private era's metadata
    /// (`folders.md` § Publicly-synced follow owns the rule).
    ///
    /// `0` on a born-public folder (nothing preceded it) and on every folder
    /// that has never been declassified (where the value is simply unread).
    /// Stamped by [`CacheDb::update_folder_for_user`], the one audience writer;
    /// never cleared on a flip-back (the gate is audience-keyed, so a stale
    /// floor serves nothing, and the next →public re-stamps it higher).
    pub public_floor_seq: i64,
    /// `folders.nest_content_residency` (v45, phase 5): `None` = full (the
    /// default — the nest keeps chunk bytes); `Some("metadata_only")` = the
    /// owner's consent-gated choice that chunk bytes never rest here. The wire
    /// projection is derived by `folder_handlers::residency_of` (fail-closed
    /// to full).
    pub nest_content_residency: Option<String>,
    /// The owner's per-folder **exclusive editing** choice (v72;
    /// `file-sync.md` § Exclusive editing): `true` ⇒ one device at a time
    /// may write, so a seat takes `fauna.folders.lease.acquire` before an
    /// upload pass. The standing choice only — who holds the lease right
    /// now is `upload_leases`, projected onto the wire as
    /// `FolderSummary::lease`.
    pub exclusive_editing: bool,
    /// `folders.audience_attestation` (v75) — the owner's signed `public`
    /// statement, canonical-encoded. **Opaque here**: no nest decision may read
    /// it (the seats verify it against the nest — `encryption-at-rest.md`
    /// § Readable classes); it exists to be served back on `FolderSummary`.
    pub audience_attestation: Option<Vec<u8>>,
    /// `folders.set_nonce` — the client-minted 32-byte set binding
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (2) and its custody sub-bullet (f)): stored opaque at create,
    /// overwritten by the owner's update, echoed as `FolderSummary::set_nonce`.
    /// The ONE nest decision that reads it is ingest's equality check — a signed
    /// statement is rebuilt under this copy, never a nest-chosen value.
    pub set_nonce: Option<Vec<u8>>,
}

impl FolderRow {
    /// Is this folder served over WebDAV — the ONE served-set predicate every
    /// gate reads (the bridge's byte-token mint, its folder listings, its
    /// record twins): the owner flagged it `webdav_enabled` AND it is not a
    /// reserved `__` rail (`webdav-server.md` § What the namespace is — a
    /// folder has no type, so the former sync-type gate retired with the mode).
    #[must_use]
    pub fn is_webdav_served(&self) -> bool {
        self.webdav_enabled && !crate::db::snapshots::is_reserved_folder_name(&self.name)
    }

    /// Has the owner explicitly **declassified** this folder?
    ///
    /// The boolean twin of [`audience_of`](crate::folder_handlers::audience_of),
    /// whose derivation the [`Self::audience`] doc already tells callers never to
    /// open-code. The tri-state obeyed that; this predicate did not — it was
    /// spelled `self.audience.as_deref() == Some("public")` at every site that
    /// needed the yes/no, including the paywall refusal and the content-plane
    /// plaintext gate, where getting it wrong decides whether a user's bytes rest
    /// readable by the world.
    ///
    /// Use [`Self::rests_plaintext_paths`] for the names-and-paths question.
    #[must_use]
    pub fn is_public_audience(&self) -> bool {
        self.audience.as_deref() == Some(fauna_protocol::folders::AUDIENCE_PUBLIC)
    }

    /// Do this folder's **names and paths** rest as plaintext by ratified design?
    ///
    /// The S9-flip plaintext class (`encryption-at-rest.md` § Carve-outs): a
    /// `public`-audience folder. (The legacy `mode == "web"` spelling of the
    /// same class retired 2026-09-28 with the folders re-model's mode
    /// contraction — no production path ever wrote it.) Every rail that
    /// admits a sealless record asks exactly this, so a future class is one
    /// edit here.
    ///
    /// **One owner because the copies were already mirroring each other by hand.**
    /// Four rails spelled the pair out — the WS-RPC record gate, the sync-WS
    /// twin (whose comment says outright it is "mirrored from the WS-RPC twin
    /// `record_change_core`"), the conflict-row rail and the web-serve gate — so
    /// a fifth class, or the `mode` column's removal, had four independent edits
    /// to get right. Callers needing the *reserved-custody* exemption still OR it
    /// in themselves: it is a property of the caller's rail, not of the folder.
    #[must_use]
    pub fn rests_plaintext_paths(&self) -> bool {
        self.is_public_audience()
    }

    /// Whether this folder's **plaintext** files feed the render pipeline and
    /// serve — the website toggle on AND the folder in the plaintext-paths
    /// class above. (A sealed row needs only the toggle; it takes the paywall
    /// token gate from there, and is never a render input.)
    ///
    /// **One owner, because this is the predicate a revoke is decided on.**
    /// `web_content::serve::gate_web_file_folder` asks it per served row, and
    /// `update_folder_for_user` marks the owner's site owed a render when its
    /// answer *changes* — not when two named columns do
    /// (`web-content-hosting.md` § Routing, render, serving → *A revoke is
    /// durable*). Spelling the pair out at the marker is what let the `mode`
    /// leg through: moving a legacy `mode = "web"` folder to `sync` takes its
    /// templates out of the render's inputs while touching neither
    /// `website_enabled` nor `audience`, so nothing marked and the pages it had
    /// rendered kept serving. Any
    /// future column `rests_plaintext_paths` grows is covered for free.
    #[must_use]
    pub fn serves_plaintext_web_files(&self) -> bool {
        self.website_enabled && self.rests_plaintext_paths()
    }
}

/// [`FolderRow::serves_plaintext_web_files`] off the raw columns, for a caller
/// holding an open transaction rather than a mapped row
/// (`update_folder_for_user`, which reads the two columns either side of its
/// own UPDATE). Same predicate, one place.
#[must_use]
pub(crate) fn serves_plaintext_web_files(website_enabled: bool, audience: Option<&str>) -> bool {
    website_enabled && audience == Some(fauna_protocol::folders::AUDIENCE_PUBLIC)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceWithStatus {
    pub device_id: Vec<u8>,
    pub label: String,
    pub capabilities: String,
    pub registered_at: i64,
    pub last_seen: i64,
    pub online: bool,
    pub folders: Vec<(String, String)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub device_id: Vec<u8>,
    pub label: String,
    pub last_change_at: i64,
    pub change_count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FolderMemberRow {
    pub device_id: Vec<u8>,
    pub flags: fauna_protocol::folders::PlaceFlags,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncConflictRow {
    pub id: i64,
    pub folder_id: i64,
    pub device_id: Vec<u8>,
    pub path: String,
    pub conflict_type: String,
    pub details: Option<String>,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
    /// How the conflict resolved: `'merged'` | `'latest_wins'` for an
    /// auto-resolved row (ratified 2026-07-10); `None` for unresolved and
    /// chooser-resolved rows (`resolve_conflict_with_winner`).
    pub resolution: Option<String>,
    /// Raw winner manifest hash (the head devices converged on); `None` while
    /// unresolved / candidate-free (mark-only).
    pub winning_manifest_hash: Option<Vec<u8>>,
    /// BLAKE3 of the normalized conflicting path — the key
    /// `resolve_conflict_choose_winner` reads (path-sealing S1). `NOT NULL`:
    /// the one writer (`report_conflict`) stamps it at insert.
    pub path_hash: Vec<u8>,
    /// The client's opaque `SealedLabel` over `path`, and over the free-text
    /// `details` (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
    /// Stored and echoed verbatim; the nest opens neither.
    pub path_sealed: Option<Vec<u8>>,
    pub details_sealed: Option<Vec<u8>>,
}

/// One candidate version of a sync conflict (a `sync_conflict_candidates` row).
/// Self-contained display metadata so `conflicts.list` needs no join against
/// the version history. See `docs/goal/behavior/file-sync.md` § Conflicts.
/// Hashes are raw bytes here; the handler hex-encodes/decodes for the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictCandidateRow {
    pub manifest_hash: Vec<u8>,
    pub device_id: Vec<u8>,
    pub size_bytes: i64,
    pub created_at: i64,
    /// M2 content-key generation this candidate's manifest was sealed under
    /// (bound sets), echoed so the review-list re-point carries it verbatim.
    pub content_key_version: Option<u64>,
}

/// The resolved half of a pre-resolved conflict report (auto-resolve,
/// file-sync.md § Conflicts ratified 2026-07-10): how the detecting device
/// resolved and what it resolved to. `report_conflict` propagates it
/// transactionally — the loser retention row + the winner head row land in the
/// same transaction as the conflict row, so there is no client-side ordering
/// and no crash window between "conflict recorded" and "versions retained".
#[derive(Debug, Clone)]
pub struct ResolvedReport {
    /// `"merged"` | `"latest_wins"`.
    pub resolution: String,
    /// Raw manifest hash of the winning version (the new head).
    pub winning_manifest_hash: Vec<u8>,
    /// Byte size of the winning version (a candidate's recorded size, or the
    /// merged result's size from the wire).
    pub winning_size_bytes: i64,
    /// M2 content-key generation of the winning version's chunks, echoed into
    /// the head row so sealed-set readers select the right key.
    pub winning_content_key_version: Option<u64>,
    /// Device to attribute the winner head row to (self-echo-skip semantics,
    /// the choose-winner precedent): the winning candidate's device when the
    /// winner is a candidate, else the reporter.
    pub winner_device_id: Vec<u8>,
    /// Causal watermark for the winner head row (the 2026-08-02 ruling,
    /// `ConflictReportRequest::winning_derived_through`), minted exactly as
    /// sent — the loser-row upgrade is retired (2026-09-27). The head row is
    /// additionally stamped `is_resolution = !winning_carries_novelty`.
    /// `None` from a reporter that sent none → the row rests NULL.
    pub winning_derived_through: Option<i64>,
    /// Causal watermark for the loser retention row (a fresh edit, never a
    /// resolution) — the reporter's ledger-ancestor seq, when known.
    pub losing_derived_through: Option<i64>,
    /// The winner CARRIES NOVEL CONTENT (the same-anchor ruling, 2026-08-05
    /// — `ConflictReportRequest::winning_carries_novelty`): the report
    /// consumed unpublished pre-merge novelty whose only carrier is the
    /// winner row, so the head row mints EDIT-class instead of
    /// `is_resolution = 1`. Absent/false → the resolution stamp.
    pub winning_carries_novelty: Option<bool>,
}

impl ResolvedReport {
    /// The reporter's own losing candidate the report retains as an
    /// `is_retention` row: the FIRST candidate carrying the reporter's
    /// `device_id` whose manifest is not the winner's — never the winner
    /// itself (the head row covers it). The one pick, shared by the handler's
    /// verify of the loser signature and the insert in
    /// [`CacheDb::report_conflict_signed`], and mirrored by the signer's
    /// `SignedChange::for_retained_loser` (ruling (10)(d)).
    pub fn retained_loser<'a>(
        &self,
        candidates: &'a [ConflictCandidateRow],
        reporter_device: &[u8],
    ) -> Option<&'a ConflictCandidateRow> {
        candidates.iter().find(|c| {
            c.device_id == reporter_device && c.manifest_hash != self.winning_manifest_hash
        })
    }
}

/// The client-minted sealed companions for one conflict report (path-sealing
/// S6-a, `file-sync.md` § Sealed names & paths).
///
/// A struct rather than three more positional parameters on
/// [`CacheDb::report_conflict`]: it already takes nine, and growing such a
/// signature has twice landed a main-red by leaving `cfg(test)` call sites
/// behind (S5a, S5b). `Default` means an unsealed caller — a keyless writer,
/// and every test that does not care — writes `SealedConflictLabels::default()`
/// and keeps compiling when this grows again.
///
/// All three are `None` from a keyless writer, the unsealed shape:
/// the nest derives [`Self::path_hash`] from the plaintext path and stores no
/// seal. Nothing here is nest-computable except that hash — the seals are
/// client-keyed by construction.
#[derive(Debug, Clone, Default)]
pub struct SealedConflictLabels {
    /// BLAKE3 of the normalized path. `None` → derived from the plaintext.
    pub path_hash: Option<[u8; 32]>,
    /// The path sealed under the set's label root.
    pub path_sealed: Option<Vec<u8>>,
    /// The free-text details sealed under the same root.
    pub details_sealed: Option<Vec<u8>>,
}

/// Outcome of a choose-winner resolve (`resolve_conflict_choose_winner`).
#[derive(Debug, Clone, PartialEq)]
pub enum ResolveWinner {
    /// No matching unresolved conflict for this id.
    NotFound,
    /// The chosen `winning_manifest_hash` is not one of the conflict's
    /// recorded candidates.
    BadCandidate,
    /// Winner recorded + propagated; `change_seq` is the `sync_changes` row
    /// that carries the winner to every device via catch-up.
    Resolved { change_seq: i64 },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KnockRow {
    pub id: i64,
    #[serde(with = "serde_bytes")]
    pub sender_id: [u8; 32],
    pub sender_node: Vec<u8>,
    pub summary: String,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ContactRow {
    pub peer_id: Vec<u8>,
    pub status: String,
    pub accepted_at: Option<i64>,
    pub created_at: i64,
    /// The peer's handle, joined from the `users` table (`LEFT JOIN`).
    /// `None` when the peer is not a local user (federated / no `users` row);
    /// `Some("")` for a local user with no handle set. Populated by
    /// [`CacheDb::list_contacts_full`].
    pub handle: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FeedRow {
    pub feed_id: String,
    pub owner: Vec<u8>,
    pub name: String,
    pub rules: Vec<u8>,
    pub combination: String,
    pub created_at: i64,
    pub scope: String,
    pub contributor_seeds: String,
    /// Canonical dag-cbor `Vec<fauna_core::scoring::CompositionEntry>`;
    /// `None` = the feed has no composition (frame § Composition) and
    /// `order=score` reads the single engagement score.
    pub composition: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct ContributorRow {
    pub feed_id: String,
    pub nest_url: String,
    pub author_id: Option<Vec<u8>>,
    pub hit_count: i64,
    pub last_seen: i64,
    pub poll_priority: String,
    pub discovered_via: String,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct FeedPostRow {
    pub post_id: Vec<u8>,
    pub author: Vec<u8>,
    pub body: String,
    /// Epoch microseconds, read straight off `content.created_at`
    /// (`db/schema.rs` `SCHEMA_CONTENT`'s own invariant) — carried through
    /// unconverted onto `FeedPostItem.created_at`.
    pub created_at: i64,
    pub has_media: bool,
    pub is_reply: bool,
    /// Interaction-bar counters projected from `content_meta` (ratified
    /// 2026-06-27, `feed.md` § Interaction bar). Drive the wire
    /// `FeedPostItem.{like,reply,repost,quote}_count` → the snapshot's icon+count
    /// bar. Read via `COALESCE(cm.*, 0)` so a NULL reads 0.
    pub like_count: i64,
    pub reply_count: i64,
    pub repost_count: i64,
    pub quote_count: i64,
    pub tags: Vec<String>,
    pub source: String,
    /// 32-byte content id of the quote target, read from the
    /// `content_links link_type='quote'` projection (`None` for non-quoting
    /// posts). Drives the wire `FeedPostItem.quoted_post_id` / the snapshot's
    /// quoted-post embed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quoted_post_id: Option<Vec<u8>>,
    /// 32-byte content id of the repost target (`content_links
    /// link_type='repost'`), the quote twin — marks the row a repost row
    /// (`feed.md` § Interaction bar → Repost). Drives
    /// `FeedPostItem.reposted_post_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reposted_post_id: Option<Vec<u8>>,
    /// 32-byte post id of the **viewer's** own live repost of this row's post,
    /// filled by `augment_viewer_state` (never by the viewer-independent query
    /// fns). Drives `FeedPostItem.viewer_repost_id` — `unrepost`'s argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer_repost_id: Option<Vec<u8>>,
    /// Whether the viewer holds a live like-toggle row on this post, filled by
    /// `augment_viewer_state`. Drives `FeedPostItem.viewer_liked`.
    #[serde(default)]
    pub viewer_liked: bool,
    /// Composite score from `content_meta.score`. Populated by `query_feed_scored`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// Tier name of a gated-to-tier post from `content_meta.gated_tier`
    /// (`None` = public). Drives the wire
    /// `FeedPostItem.gated_tier` → the apps' `gated-post-badge`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gated_tier: Option<String>,
    /// The 32-byte channel id of the room a room-restricted post addresses,
    /// from `content_meta.gated_room` (`None` = not a room post).
    /// Hex-encoded onto the wire
    /// `FeedPostItem.gated_room` → the member's card names the room.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gated_room: Option<Vec<u8>>,
    /// The post's web-publish slug, read from the `content_links
    /// link_type='web_published'` projection (`None` = not published to the
    /// web). Drives the wire `FeedPostItem.web_slug` → the own-post web verbs
    /// on the apps' `feed-post-actions-menu` ⋯ overflow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_slug: Option<String>,
    /// Per-category content-label verdicts (`moderation.md` § Per-row badge
    /// data path), one entry per category — the highest-confidence
    /// `content_labels` row. Drives the wire `FeedPostItem.labels` → the
    /// apps' `content-label-badge`.
    #[serde(default)]
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotRow {
    pub id: i64,
    pub folder_id: i64,
    pub created_at: i64,
    pub file_count: i64,
    pub total_bytes: i64,
    pub parent_id: Option<i64>,
    pub device_id: Option<Vec<u8>>,
    pub max_change_seq: Option<i64>,
    pub deletion_pending: bool,
    pub soft_deleted: bool,
    pub purge_after: Option<i64>,
    /// `NULL` for folder snapshots; `'mail'` or `'calendar'` for message-kind snapshots.
    pub message_kind: Option<String>,
    /// BARE-serialised `Manifest` (or equivalent) for message-kind snapshots; `NULL` otherwise.
    pub message_manifest: Option<Vec<u8>>,
    /// BARE-serialised `MailPlacementManifest` (or equivalent); `NULL` until
    /// IMAP-restore Plan 1 lands.
    pub placement_manifest: Option<Vec<u8>>,
    /// JSON array of hex per-tag digests, 1:1 with the create's wire tags —
    /// the key the retention pruner matches a policy's `keep_tags` against
    /// (path-sealing S1, `docs/goal/behavior/file-sync.md` § Sealed names &
    /// paths). The tags never rest in plaintext: this and [`Self::tags_sealed`]
    /// are all that rests. `None` on an untagged snapshot.
    pub tag_hashes: Option<String>,
    /// The creating client's sealed display copy of the tags (path-sealing S6-d,
    /// `docs/goal/behavior/file-sync.md` § Sealed names & paths). Opaque here —
    /// this nest holds no key that opens it, and nothing server-side reads its
    /// *contents*: retention matching is hash-to-hash on [`Self::tag_hashes`]
    /// and stays that way. The one server-side reader of any part of this
    /// column is `stamp_labels`'s re-stamp predicate, which
    /// reads the envelope HEADER's key generation — unsealed metadata by
    /// design — to tell a licensed axis upgrade from an overwrite. `None` on an
    /// untagged snapshot and on one created by a keyless client (an S8
    /// backfill row).
    pub tags_sealed: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotFileRow {
    pub manifest_hash: Vec<u8>,
    pub size_bytes: i64,
    pub mtime: i64,
    pub mode: i64,
    pub file_type: String,
    pub symlink_target: Option<String>,
    /// The file's routing key (`fauna_core::sync::path_hash`) — the PK's
    /// second column since the S9 flip rebuilt the table and dropped the
    /// plaintext `path` (v32; `docs/goal/behavior/file-sync.md` § Sealed
    /// names & paths § Migration). The plaintext is client-side: a reader
    /// renders the name from `path_sealed`, or omits it.
    pub path_hash: Vec<u8>,
    /// The sealed label over `path`, opaque to the nest. Carried so a reader
    /// that holds the set's key can render the name once clients seal (S2); the
    /// nest never opens it. `None` for every row written before a client seals.
    pub path_sealed: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailFilterRow {
    pub id: i64,
    pub owner: Vec<u8>,
    pub name: String,
    pub rules: Vec<u8>,
    pub combination: String,
    pub action: String,
    pub priority: i32,
    /// Sieve `continue` (`email_filters.continue_on_match`, 0/1): a match is
    /// terminal unless set. See `fauna_protocol::email::EmailFilter`.
    pub continue_on_match: bool,
    /// The Forward action's copy mode (`email_filters.forward_redirect`, 0/1):
    /// `true` = `redirect` (forward, no local delivery). Meaningful only when
    /// `action` is `forward:<address>`; always `false` otherwise. Projected onto
    /// `EmailFilterAction::Forward { redirect }` by `email_handlers`.
    pub forward_redirect: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailDomainRow {
    pub domain: String,
    pub dkim_selector: String,
    pub dkim_ed25519_selector: String,
    pub enabled: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DomainUserRow {
    pub domain: String,
    pub actor_id: Vec<u8>,
    pub local_part: String,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BridgeMessageRow {
    pub id: i64,
    pub bridge_type: String,
    pub actor_id: Vec<u8>,
    pub external_id: String,
    pub sender: String,
    pub recipient: String,
    pub subject: String,
    pub size_bytes: i64,
    pub flags: i64,
    pub received_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BridgeFeedSubscription {
    pub id: i64,
    pub actor_id: Vec<u8>,
    pub bridge: String,
    pub feed_uri: String,
    pub name: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct ContentLabelRow {
    pub id: i64,
    pub category: String,
    pub confidence: f64,
    pub mechanism_type: u8,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObligationActionRow {
    pub id: i64,
    pub content_type: String,
    pub content_id: String,
    pub obligation_id: Vec<u8>,
    pub rule_index: i64,
    pub category: String,
    pub confidence: f64,
    pub action_taken: u8,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpamPreferences {
    pub spam_threshold: f64,
    pub phishing_threshold: f64,
    /// Whether the actor opts their per-user training into the admin-opt-in
    /// deployment spam baseline (`mail-spam.md` § Cold start, Path 2 —
    /// `mail-spam-contribute-baseline-toggle`). Default off (the
    /// user-controls-their-data invariant). It gates whether this actor's
    /// holder copy may be merged into the deployment-wide baseline — the one
    /// sanctioned model sharing (`mail-spam.md` § Implicit signals are
    /// forbidden).
    pub contribute_baseline: bool,
}

impl Default for SpamPreferences {
    fn default() -> Self {
        Self {
            spam_threshold: 0.5,
            phishing_threshold: 0.3,
            contribute_baseline: false,
        }
    }
}

/// One `spam_training_history` row read back (`mail-spam.md` § Training-sample
/// retention). `label`/`source` are the snake_case wire strings (`spam`/`ham`;
/// `imap_junk_flag`/`imap_junk_move`/`manual_other`); the handler maps them to
/// the `fauna_protocol::bridge_routing::{SpamLabel,TrainingSource}` enums for the
/// wire row. `model_delta_applied` is the stored event delta (the distinct n-gram
/// set, sealed to the actor's own recipient key — an opaque `wrapped_blob`); the
/// list read returns it so a client-side undo can replay its inverse.
#[derive(Debug, Clone)]
pub struct SpamTrainingHistoryRecord {
    pub history_id: Vec<u8>,
    pub mailbox: String,
    pub label: String,
    pub source: String,
    pub model_delta_applied: Vec<u8>,
    pub created_at: i64,
    /// The message subject sealed to the actor's own recipient key (opaque
    /// `wrapped_blob` bytes) — the only place the subject rests.
    pub sealed_subject: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionTierRow {
    pub author_id: Vec<u8>,
    pub name: String,
    pub rank: i64,
    pub description: Option<String>,
    pub price_hint: Option<String>,
    pub payment_url: Option<String>,
    pub auto_approve: bool,
    pub created_at: i64,
    /// Per-post pay-to-unlock designation — the hex `post_id` this tier sells
    /// (`monetization.md` § Per-post pay-to-unlock); `None` on every ordinary
    /// tier. Create-time immutable, and never resolved against a post row:
    /// the post is authored *after* the tier exists, and the tier outlives it.
    pub unlocks_post: Option<String>,
    /// The machine-comparable asking price (`monetization.md` § The asking
    /// price) — the amount half of the unit-tagged pair. `None` (with
    /// [`Self::asking_price_unit`] also `None`) means no *inferring*
    /// mechanism may buy this tier, whatever it is zapped.
    ///
    /// The pair is written and read together; [`Self::asking_price`] is the
    /// only sanctioned way to consume it, because it is what refuses a
    /// half-set row rather than guessing a missing half.
    pub asking_price_value: Option<i64>,
    /// The denomination half of the asking-price pair — `"msat"` today, and
    /// stored verbatim even for a unit this build does not know (which then
    /// compares as not-met, fail-closed).
    pub asking_price_unit: Option<String>,
    /// Hidden from every offer surface (`monetization.md` § The unifying
    /// model — *A tier may be hidden*); `false` on every ordinary tier.
    pub hidden: bool,
}

impl SubscriptionTierRow {
    /// The tier's asking price as the **wire** carries it, or `None` when the
    /// tier has none.
    ///
    /// Deliberately the ungated wire type (`fauna_protocol`) and not the
    /// comparison type (`fauna_payments::asking_price::AskingPrice`): a nest
    /// built without the `payments` member must still store and re-serve a price
    /// a full client authored — an excised build is *a peer without a
    /// capability, never a fork of the wire*, and excising a feature removes the
    /// ability to OPERATE it, never the data at rest (`dynamic-features.md`
    /// § Wire-compat posture). What excises is the *judgement* that money met
    /// this price, which happens at exactly one site — the zap purchase path —
    /// and converts to the comparison type there.
    ///
    /// **A half-set row answers `None`**, deliberately and silently: an amount
    /// with no denomination is not a price this model can compare (the
    /// comparison is unit-equality-first), and a denomination with no amount
    /// names nothing. The write path never produces a half-set row, so this
    /// arm is unreachable through the handlers — it exists because the columns
    /// are individually nullable at rest and the fail-closed direction for an
    /// impossible row is "this tier is not for sale", never "sold at the half
    /// we happen to have".
    ///
    /// A negative `value` cannot arise through the wire (`u64`) and is
    /// likewise refused rather than wrapped.
    pub fn asking_price(&self) -> Option<fauna_protocol::subscriptions::TierAskingPrice> {
        match (self.asking_price_value, self.asking_price_unit.as_deref()) {
            (Some(value), Some(unit)) if value >= 0 => {
                Some(fauna_protocol::subscriptions::TierAskingPrice {
                    value: value as u64,
                    unit: unit.to_string(),
                    extra: Default::default(),
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SubscriberRow {
    pub subscriber_id: Vec<u8>,
    pub tier_name: String,
    pub approved_at: i64,
    /// Subscriber's published 1184-byte ML-KEM-768 encapsulation key, or `None`
    /// for a classical-only subscriber. The author/nest wraps a hybrid X-Wing
    /// `KeyBlobEntry` to it when present (post-quantum surface B).
    pub mlkem_encaps_key: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubscribeRequestRow {
    pub id: i64,
    pub subscriber_id: Vec<u8>,
    pub tier_name: String,
    pub created_at: i64,
    pub kind: String,
    /// Subscriber's published ML-KEM ek (post-quantum surface B), carried on the
    /// pending `subscribe` request until approval lands it on the `subscribers`
    /// row. `None` for unsubscribe rows / classical subscribers.
    pub mlkem_encaps_key: Option<Vec<u8>>,
    /// Verified-payment marker (monetization.md § Pillar 3): the author's drain
    /// pump approves this request without creator judgment — the third grant
    /// source next to manual approval + tier `auto_approve`.
    pub payment_entitled: bool,
    /// The paid window the payment carried (epoch seconds; `None` = none),
    /// stamped onto the `subscribers` row at approval.
    pub valid_until: Option<i64>,
}

/// One configured payment provider for an author (monetization.md § Pillar 3).
#[derive(Debug, Clone)]
pub struct PaymentProviderRow {
    pub kind: String,
    /// Verify-only webhook secret (never leaves the nest; the WS-RPC list
    /// reply deliberately omits it).
    pub webhook_secret: String,
    pub tier_name: String,
    pub created_at: i64,
    /// Evidence-based provider-status stamps (monetization.md § Pillar 3 →
    /// Provider status), epoch seconds. Stamped at webhook ingress only —
    /// never by an active probe. Drives `ProviderItem.{last_verified_at,
    /// last_rejected_at}` on the wire.
    pub last_verified_at: Option<i64>,
    pub last_rejected_at: Option<i64>,
}

/// One membership designation (monetization.md § Pillar 4 — paid nest access):
/// the link making an admin's own subscription tier mean membership of this
/// nest. `tier_name` names a `subscription_tiers` row owned by `admin_id`;
/// `admin_tier` / `lapse_tier` name `tiers` (quota) rows — the two systems stay
/// distinct concepts joined here, never merged.
#[derive(Debug, Clone, PartialEq)]
pub struct MembershipTierRow {
    pub tier_name: String,
    /// Quota tier an admitted member is assigned (`users.tier`).
    pub admin_tier: String,
    /// Quota tier a lapsed member degrades to (default `free`) — a reversible
    /// downgrade, never a suspension.
    pub lapse_tier: String,
    pub created_at: i64,
}

/// One post-payment claim code (monetization.md § Pillar 3 Q4 — the universal
/// buyer↔actor binding fallback).
#[derive(Debug, Clone)]
pub struct PaymentClaimRow {
    pub code: String,
    pub author_id: Vec<u8>,
    pub tier_name: String,
    pub provider: String,
    pub external_ref: String,
    pub valid_until: Option<i64>,
    pub created_at: i64,
    pub redeemed_by: Option<Vec<u8>>,
    pub redeemed_at: Option<i64>,
    pub voided_at: Option<i64>,
}

/// One of a subscriber's own subscriptions across all creators — the
/// caller-scoped consumer enumeration (`fauna.subscriptions.mine.list`). Merges
/// approved `subscribers` rows (`status = "active"`) with not-yet-approved
/// `subscribe_requests` of kind `"subscribe"` (`status = "pending"`).
#[derive(Debug, Clone, Serialize)]
pub struct MySubscriptionRow {
    pub author_id: Vec<u8>,
    pub tier_name: String,
    /// `"active"` | `"pending"`.
    pub status: String,
    /// `approved_at` (active) or `created_at` (pending), epoch seconds.
    pub since: i64,
}

impl CacheDb {
    /// Open (or create) the database at `path` and run migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref()).context("open sqlite")?;
        // A1b: enable FK enforcement before running migrations
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .context("enable foreign_keys")?;
        // A database the genesis did not write is refused before anything
        // reads or writes it (`migrations.rs` § Genesis).
        migrations::check_genesis(&conn)?;
        // Schema-compatibility gate (version-compatibility.md § 2.2), BEFORE
        // `run_migrations` mutates anything. An `Incompatible` DB — written by a
        // newer nest carrying a breaking change this binary predates — must NOT
        // be migrated destructively or failed lazily at first insert; surface a
        // typed, downcastable error the boot path turns into the degraded
        // "needs-update" serve mode (no off-box brick — `nest/common.md`
        // § Client-state recoverability). `UpgradeOrCurrent` / `NewerCompatible`
        // both proceed: the additive reconciler tolerates a newer DB's extra
        // columns, and `record_schema_meta` declines to restamp the version down.
        if let migrations::SchemaVerdict::Incompatible {
            db_v,
            db_min,
            bin_v,
        } = migrations::check_schema_compatibility(&conn)?
        {
            return Err(migrations::SchemaIncompatible {
                db_v,
                db_min,
                bin_v,
            }
            .into());
        }
        migrations::run_migrations(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database (for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory sqlite")?;
        // A1b: enable FK enforcement before running migrations
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .context("enable foreign_keys")?;
        migrations::run_migrations(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Acquire the underlying SQLite connection lock.
    ///
    /// Prefer purpose-built methods when available. This is exposed for
    /// generic adapters (e.g. [`crate::bluesky::storage_backend::CacheDbBackend`]).
    pub async fn conn(&self) -> tokio::sync::MutexGuard<'_, Connection> {
        self.conn.lock().await
    }

    /// Get a blocking lock on the connection. For use in spawn_blocking contexts.
    pub fn conn_blocking(&self) -> tokio::sync::MutexGuard<'_, Connection> {
        self.conn.blocking_lock()
    }

    /// Execute a batch of SQL statements (e.g. `CREATE TABLE` migrations).
    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch(sql).context("execute_batch")?;
        Ok(())
    }

    /// Flush pending WAL writes to the main database file.
    ///
    /// Runs `PRAGMA wal_checkpoint(TRUNCATE)` to ensure all committed
    /// transactions are written to the main `.db` file and the WAL is reset.
    /// Called on graceful shutdown so no data is lost.
    pub async fn flush(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .context("wal_checkpoint")?;
        Ok(())
    }

    /// Record a completed database backup snapshot.
    pub async fn record_backup_snapshot(
        &self,
        hash: &[u8; 32],
        size: i64,
        format: &str,
        created_at: i64,
    ) -> Result<()> {
        let hash = hash.to_vec();
        let format = format.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO backup_snapshots (blob_hash, size_bytes, format, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![hash, size, format, created_at],
        )?;
        Ok(())
    }

    /// List `backup_snapshots` of a format, newest first (`created_at DESC, id
    /// DESC`), as `(id, blob_hash, created_at)`. Backs the keep-last-N retention
    /// in `BackupService::prune_backup_snapshots`.
    pub async fn list_backup_snapshots(&self, format: &str) -> Result<Vec<(i64, [u8; 32], i64)>> {
        let format = format.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, blob_hash, created_at FROM backup_snapshots \
             WHERE format = ?1 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![format], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, hash, created_at) in rows {
            let arr: [u8; 32] = hash
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("backup_snapshots.blob_hash is not 32 bytes"))?;
            out.push((id, arr, created_at));
        }
        Ok(out)
    }

    /// Delete a `backup_snapshots` row by id. Returns rows affected.
    pub async fn delete_backup_snapshot(&self, id: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        Ok(conn.execute(
            "DELETE FROM backup_snapshots WHERE id = ?1",
            rusqlite::params![id],
        )?)
    }

    /// Count `backup_snapshots` rows referencing a blob hash (any format). Used
    /// by retention to avoid deleting a content-addressed blob still shared by a
    /// remaining row (an unchanged DB across cycles hashes identically).
    pub async fn count_backup_snapshots_with_hash(&self, hash: &[u8; 32]) -> Result<i64> {
        let hash = hash.to_vec();
        let conn = self.conn.lock().await;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM backup_snapshots WHERE blob_hash = ?1",
            rusqlite::params![hash],
            |r| r.get(0),
        )?)
    }
}

impl CacheDb {
    /// Insert a mail-kind segment_records row. Locks the connection
    /// internally; callers (`segments::mail`) keep no lock state.
    #[allow(clippy::too_many_arguments)]
    pub async fn segment_records_insert_mail(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
        cid: &fauna_cbor::Cid,
        bucket: &str,
        received_at: i64,
        sender_domain: &str,
        spam_disposition: &str,
        is_own_submission: bool,
        seq: i64,
        report_hash: Option<&[u8]>,
        continuation_role: u8,
        stored_at: i64,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::insert_mail(
            &conn,
            scope_id,
            segment_id,
            cid,
            bucket,
            received_at,
            sender_domain,
            spam_disposition,
            is_own_submission,
            seq,
            report_hash,
            continuation_role,
            stored_at,
        )
    }

    /// Next per-actor mail `seq` for a scope (`MAX(seq)+1`, 1 when empty).
    /// Ignores tombstones — `seq` is never reused. The caller
    /// (`segments::mail::append_record`/`append_sealed_record`) holds the
    /// per-actor seq lock so the query→append→insert sequence is atomic.
    pub async fn segment_records_next_mail_seq(&self, scope_id: &[u8; 32]) -> anyhow::Result<i64> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::next_mail_seq(&conn, scope_id)
    }

    /// List live mail records for one actor with `seq > after_seq`, oldest
    /// first, up to `limit`. Returns `(seq, segment_id, record_cid)`. The
    /// relay's after-cursor reader (`segments::mail::read_after_seq`).
    pub async fn segment_records_list_mail_after_seq(
        &self,
        scope_id: &[u8; 32],
        after_seq: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<(i64, u32, fauna_cbor::Cid)>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::list_mail_after_seq(&conn, scope_id, after_seq, limit)
    }

    /// The S8 D4 **plaintext scrub mechanism**: per sealed plane, destroy the
    /// plaintext of every row **whose sealed sibling rests** — the contract
    /// step's `scrubs plaintext where a sealed sibling exists`
    /// (`docs/goal/behavior/file-sync.md` § Sealed names & paths → *Migration*),
    /// as executable code. Returns `(plane, rows scrubbed)` per plane.
    ///
    /// ⚠ **MECHANISM ONLY — S8 lands zero production scrubs.** This compiles
    /// exclusively under `test-hooks` (the D5 at-rest proof is its one caller);
    /// there is no admin kind, no boot flag, and no production trigger. Wiring
    /// one is the S9 flip's decision, gated on the explicit per-case user
    /// approval the goal doc requires — the flip is **major-gated by default**.
    /// This is the first plaintext-destroying write in the codebase; the gate
    /// is the point.
    ///
    /// Shape rules, each load-bearing:
    /// - **`WHERE <sealed> IS NOT NULL` is the whole predicate.** A row with no
    ///   sealed sibling keeps its plaintext — that is what protects the
    ///   machine-authored device labels no writer ever seals (the WebDAV
    ///   pseudo-device, the self-heal placeholder, the backup coordinator) and
    ///   every other plaintext plane with no sealed sibling, whose fate is the
    ///   S9 ask's to decide, not this mechanism's.
    /// - **Nullable planes scrub to `NULL`; `NOT NULL` planes scrub to `''`** —
    ///   the ratified scrub sentinel every render seam already reads as
    ///   "scrubbed" and degrades to `Omit` (`label_custody`'s empty-plaintext
    ///   contract).
    /// - **One plane is deliberately ABSENT, S9-gated:** `folders.name` (apps
    ///   still ADDRESS folders by name — the S5b hash arm has no production
    ///   sender yet). `snapshot_files` has no plaintext path column at all (it
    ///   is hash-keyed), and `import_sessions.source_descriptor` is scrubbable
    ///   because its per-source lock keys on `source_hash`.
    #[cfg(feature = "test-hooks")]
    pub async fn scrub_plaintext_where_sealed(&self) -> anyhow::Result<Vec<(&'static str, usize)>> {
        // The plane table + runner are production's:
        // `migrations::SCRUB_PLANES` / `run_scrub_plaintext`, run by
        // `run_migrations` on every boot. This wrapper
        // stays as the test observation surface over the SAME table — a
        // divergent copy here is exactly the drift the one-funnel rule exists
        // to prevent.
        let conn = self.conn.lock().await;
        migrations::run_scrub_plaintext(&conn)
    }

    /// Tombstone every live mail record for `scope_id` with `seq <= up_to_seq`.
    /// Returns the number of rows newly tombstoned. The relay-ack purge step.
    pub async fn segment_records_tombstone_mail_up_to_seq(
        &self,
        scope_id: &[u8; 32],
        up_to_seq: i64,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::tombstone_mail_up_to_seq(&conn, scope_id, up_to_seq)
    }

    /// Look up the SegmentRecordRef for one `(scope, kind, record_cid)`.
    pub async fn segment_records_lookup_record(
        &self,
        scope_id: &[u8; 32],
        kind: &str,
        cid: &fauna_cbor::Cid,
    ) -> anyhow::Result<Option<crate::segments::records_db::SegmentRecordRef>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::lookup_record(&conn, scope_id, kind, cid)
    }

    /// Count tombstoned rows for one segment. Used by the
    /// `fauna.segments.list` WS-RPC reply (Plan 5) to populate the
    /// `SegmentRef.tombstone_count` field.
    pub async fn count_tombstoned_segment_records(
        &self,
        scope_id: &[u8; 32],
        kind: &str,
        segment_id: u32,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::count_tombstoned_for_segment(&conn, scope_id, kind, segment_id)
    }

    /// Mark a record as tombstoned. Idempotent — returns the number of rows
    /// updated (0 if already tombstoned, 1 if newly tombstoned).
    pub async fn segment_records_mark_tombstoned(
        &self,
        scope_id: &[u8; 32],
        kind: &str,
        segment_id: u32,
        cid: &fauna_cbor::Cid,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::mark_tombstoned(&conn, scope_id, kind, segment_id, cid)
    }

    /// One page of a content scope's generalized-feed rows, after `since`.
    /// The class-1 twin of [`Self::get_account_state_changes`]
    /// (`account-sync-plane.md` § Feeds and cursors).
    pub async fn content_scope_feed(
        &self,
        scope_id: &[u8; 32],
        kind: &str,
        since: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<crate::segments::records_db::ContentFeedRow>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::content_feed_after(&conn, scope_id, kind, since, limit)
    }

    /// Per-segment record/tombstone/byte rollup for compaction input selection.
    /// Live (non-tombstoned) record count for `(scope, kind)` — the
    /// empty-target predicate `fauna.backup.custody.materialize` refuses on.
    pub async fn segment_records_count_live(
        &self,
        scope_id: &[u8; 32],
        kind: &str,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::count_live_records(&conn, scope_id, kind)
    }

    pub async fn segment_records_count_segment_stats(
        &self,
        scope_id: &[u8; 32],
        kind: &str,
        bucket: &str,
    ) -> anyhow::Result<Vec<SegmentStatsRow>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::count_segment_stats(&conn, scope_id, kind, bucket)
    }

    /// List distinct buckets containing at least one tombstoned record for
    /// `(scope, kind)`. Empty result means no compaction work pending.
    pub async fn segment_records_list_buckets_with_tombstones(
        &self,
        scope_id: &[u8; 32],
        kind: &str,
    ) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::list_buckets_with_tombstones(&conn, scope_id, kind)
    }

    /// List distinct scope ids with at least one `segment_records` row.
    /// `kind = None` lists across all kinds. Plan 3 compaction worker.
    /// For mail/calendar/post, scope_id == actor_id; for conv (Plan 7+),
    /// scope_id == channel_id.
    pub async fn segment_records_list_scopes(
        &self,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::list_scopes_with_segments(&conn, kind)
    }

    /// Pre-fetch the live `(segment_id, record_cid)` pairs across the supplied
    /// input segment ids for `(scope, kind)`. The synchronous
    /// `fauna_segment_store::compact` `is_alive` closure does an in-memory
    /// lookup on this set — keeping the inner loop sync-clean.
    pub async fn segment_records_live_set_for_segments(
        &self,
        scope_id: &[u8; 32],
        kind: &str,
        segment_ids: &[u32],
    ) -> anyhow::Result<std::collections::HashSet<(u32, fauna_cbor::Cid)>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::live_set_for_segments(&conn, scope_id, kind, segment_ids)
    }

    /// Insert a conv-kind segment_records row (Plan 7). `scope_id` is the
    /// channel id; `seq` is the per-channel monotonic counter. Floor mirror
    /// columns are NULL for conv.
    #[allow(clippy::too_many_arguments)]
    pub async fn segment_records_insert_conv(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
        cid: &fauna_cbor::Cid,
        bucket: &str,
        received_at: i64,
        seq: i64,
        // The sender's plaintext attachment references — the conversation
        // kind's blob-reachability floor (`db/conv_attachment_refs.rs`).
        // Written in the SAME transaction as the mirror row: a record whose
        // refs never landed would leave its attachments unpinned with no
        // reconcile possible (the nest cannot read the body to rebuild them).
        attachment_refs: &[[u8; 32]],
        // The actor this nest authenticated on the send (`conv_record_authors`).
        // Same transaction for the same reason: the envelope is sealed, so an
        // attestation that never landed can never be rebuilt.
        author: Option<&[u8; 32]>,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin conv mirror insert tx")?;
        crate::segments::records_db::insert_conv(
            &tx,
            scope_id,
            segment_id,
            cid,
            bucket,
            received_at,
            seq,
        )?;
        crate::segments::records_db::insert_conv_attachment_refs(
            &tx,
            scope_id,
            seq,
            attachment_refs,
        )?;
        if let Some(author) = author {
            crate::segments::records_db::insert_conv_record_author(&tx, scope_id, seq, author)?;
        }
        tx.commit().context("commit conv mirror insert tx")
    }

    /// The nest-attested authors of one channel's conv records in
    /// `(after_seq, up_to_seq]`, keyed by `seq` (`conv_record_authors`).
    pub async fn conv_record_authors_in_range(
        &self,
        scope_id: &[u8; 32],
        after_seq: i64,
        up_to_seq: i64,
    ) -> anyhow::Result<std::collections::HashMap<i64, [u8; 32]>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::conv_record_authors_in_range(
            &conn, scope_id, after_seq, up_to_seq,
        )
    }

    /// Next per-channel conv `seq` for a scope (`MAX(seq)+1`, 1 when empty).
    /// Ignores tombstones — `seq` is never reused. The caller
    /// (`segments::conv::append`) holds the per-channel seq lock so the
    /// query→derive→append→insert sequence is atomic per channel.
    pub async fn segment_records_next_conv_seq(&self, scope_id: &[u8; 32]) -> anyhow::Result<i64> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::next_conv_seq(&conn, scope_id)
    }

    /// List live conv records for one channel with `seq > after_seq`, oldest
    /// first, up to `limit`. Returns `(seq, segment_id, record_cid)`.
    pub async fn segment_records_list_conv_after_seq(
        &self,
        scope_id: &[u8; 32],
        after_seq: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<(i64, u32, fauna_cbor::Cid, Option<String>)>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::list_conv_after_seq(&conn, scope_id, after_seq, limit)
    }

    /// List live conv records across many channels with `seq > after_seq`,
    /// oldest first, up to `limit` total. Returns `(seq, scope_id,
    /// segment_id, record_cid, legal_takedown_ref)`.
    pub async fn segment_records_list_conv_for_scopes_after_seq(
        &self,
        scope_ids: &[[u8; 32]],
        after_seq: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<(i64, [u8; 32], u32, fauna_cbor::Cid, Option<String>)>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::list_conv_for_scopes_after_seq(
            &conn, scope_ids, after_seq, limit,
        )
    }

    /// Atomically apply (`reference = Some`) or overturn (`None`) a
    /// conversation record's legal-obligation takedown: the
    /// `segment_records.legal_takedown_ref` flag write and the permanent audit
    /// row commit in ONE SQLite transaction — the conv twin of
    /// [`Self::post_legal_takedown_txn`] (no obligation row: conv records
    /// persist no sender, so there is no author queue to key it to; the
    /// in-thread tombstone is the member-visible transparency surface). The
    /// flag write must match exactly one conv record, else the transaction
    /// rolls back with an error — never a silent no-op that still audits
    /// (review 2026-07-06 §§ F1/F4; `nest/common.md` § single atomic decision
    /// point).
    pub async fn conv_legal_takedown_txn(
        &self,
        record_cid: &fauna_cbor::Cid,
        content_id_hex: &str,
        reference: Option<&str>,
        audit_detail: &str,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin conv legal-takedown tx")?;
        let n = crate::segments::records_db::set_conv_legal_takedown(&tx, record_cid, reference)?;
        if n != 1 {
            anyhow::bail!(
                "legal-takedown flag write matched {n} conv records for content {content_id_hex} (want exactly 1); rolled back"
            );
        }
        let audit_action = if reference.is_some() {
            "moderation:legal-takedown"
        } else {
            "moderation:legal-takedown-restore"
        };
        admin::audit_on_conn(
            &tx,
            None,
            audit_action,
            Some(content_id_hex),
            Some(audit_detail),
        )?;
        tx.commit().context("commit conv legal-takedown tx")
    }

    /// Set (`Some(reference)`) or clear (`None`) the legal-obligation takedown
    /// flag on the conv record identified by `record_cid`. Returns rows updated
    /// (`0` = no such conv record). The relay-withhold half of the Q5 carve-out
    /// (`moderation.md` § Categories & enforcement item 1); the conv twin of
    /// [`Self::set_post_legal_takedown`]. **Bare flag write, no audit** — the
    /// admin takedown handler must use
    /// [`Self::conv_legal_takedown_txn`] instead; this stays as a
    /// test-seeding helper.
    pub async fn set_conv_legal_takedown(
        &self,
        record_cid: &fauna_cbor::Cid,
        reference: Option<&str>,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::set_conv_legal_takedown(&conn, record_cid, reference)
    }

    /// Look up a conv record by `record_cid` → `(scope_id/channel,
    /// legal_takedown_ref)`, or `None` if there is no such conv record.
    pub async fn conv_record_scope_and_takedown(
        &self,
        record_cid: &fauna_cbor::Cid,
    ) -> anyhow::Result<Option<([u8; 32], Option<String>)>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::conv_record_scope_and_takedown(&conn, record_cid)
    }

    /// Tombstone every live conv record with `seq <= up_to_seq` across the
    /// supplied channels. Returns the total rows newly tombstoned.
    pub async fn segment_records_tombstone_conv_up_to_seq(
        &self,
        scope_ids: &[[u8; 32]],
        up_to_seq: i64,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::tombstone_conv_up_to_seq(&conn, scope_ids, up_to_seq)
    }

    /// Insert a post-kind `segment_records` row. `scope_id` is the author
    /// actor id; posts carry no `seq` and no mail-floor columns (they are
    /// addressed by CID — the record CID's digest is the `post_id`).
    pub async fn segment_records_insert_post(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
        cid: &fauna_cbor::Cid,
        bucket: &str,
        received_at: i64,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::insert_post(
            &conn,
            scope_id,
            segment_id,
            cid,
            bucket,
            received_at,
        )
    }

    /// Insert a calendar-kind `segment_records` row (S6.4). `scope_id` is the
    /// owner actor id; the record CID is the content hash of the event's
    /// sealed envelope (`segments::cal::append_record`'s mint), and
    /// `created_at` is epoch **seconds**.
    pub async fn segment_records_insert_calendar(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
        cid: &fauna_cbor::Cid,
        bucket: &str,
        created_at: i64,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::insert_calendar(
            &conn, scope_id, segment_id, cid, bucket, created_at,
        )
    }

    /// Insert a card-kind `segment_records` row (S6.5). Twin of
    /// [`Self::segment_records_insert_calendar`]; the record CID is derived from
    /// the card's `bridge_carddav_cards` PK, and `created_at` is epoch
    /// **seconds**.
    pub async fn segment_records_insert_card(
        &self,
        scope_id: &[u8; 32],
        segment_id: u32,
        cid: &fauna_cbor::Cid,
        bucket: &str,
        created_at: i64,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::insert_card(
            &conn, scope_id, segment_id, cid, bucket, created_at,
        )
    }

    /// Resolve a record's `(scope_id, segment_id)` from `(kind, record_cid)`
    /// alone — the post point-read path (reader holds only the `post_id` →
    /// derives the CID → must find which author's segment holds it).
    pub async fn segment_records_lookup_scope_and_segment(
        &self,
        kind: &str,
        cid: &fauna_cbor::Cid,
    ) -> anyhow::Result<Option<([u8; 32], u32)>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::lookup_scope_and_segment(&conn, kind, cid)
    }

    /// The same lookup, tombstone included — which segment still *holds* a
    /// record's bytes, live or awaiting compaction. The legal-takedown
    /// segment-pair withhold is its caller; see the free function's doc for
    /// why the live-only form is the wrong question there.
    pub async fn segment_records_lookup_scope_and_segment_including_tombstoned(
        &self,
        kind: &str,
        cid: &fauna_cbor::Cid,
    ) -> anyhow::Result<Option<([u8; 32], u32)>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::lookup_scope_and_segment_including_tombstoned(&conn, kind, cid)
    }

    /// Load every active (not soft-deleted, not deletion-pending) message-kind
    /// snapshot's `message_manifest` blob for `(scope_id, kind)`. Returns each
    /// blob alongside the snapshot id so callers can correlate; the Plan 3
    /// compaction worker only needs the blobs to seed its `PinSet`.
    pub async fn list_active_message_kind_snapshot_manifests(
        &self,
        scope_id: &[u8; 32],
        kind: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::list_active_message_kind_snapshot_manifests(&conn, scope_id, kind)
    }

    /// Hard-delete a snapshot and its `snapshot_files` rows in one transaction.
    /// Skips the pending-action and soft-delete windows (spec D11 immediate-delete).
    pub async fn hard_delete_snapshot(&self, snapshot_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::hard_delete_snapshot(&conn, snapshot_id)
    }

    /// Resolve the actor that owns a snapshot via `folders.actor_id`.
    /// Returns `None` if the snapshot or its folder no longer exists.
    pub async fn resolve_snapshot_owner(
        &self,
        snapshot_id: i64,
    ) -> anyhow::Result<Option<[u8; 32]>> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::resolve_snapshot_owner(&conn, snapshot_id)
    }

    /// Get or create the per-actor `__<kind>` pseudo folder that anchors
    /// message-kind snapshots to an owner.  Returns the `folders.id`.
    pub async fn get_or_create_reserved_folder(
        &self,
        actor_id: &[u8; 32],
        kind: &str,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::get_or_create_reserved_folder(&conn, actor_id, kind)
    }

    /// Get or create the per-channel `__conv/<channel_hex>` pseudo folder
    /// that anchors conv message-kind snapshots to a channel (Plan 8). The
    /// `folders.actor_id` column carries the `channel_id` (the scope key).
    /// Returns the `folders.id`.
    pub async fn get_or_create_reserved_conv_folder(
        &self,
        channel_id: &[u8; 32],
    ) -> anyhow::Result<i64> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::get_or_create_reserved_conv_folder(&conn, channel_id)
    }

    /// Insert a message-kind snapshot row (T6). The `folder_id` must be the
    /// reserved `__<kind>` folder for the actor.  Returns the new snapshot id.
    pub async fn create_message_kind_snapshot_row(
        &self,
        folder_id: i64,
        message_kind: &str,
        message_manifest: Option<&[u8]>,
        placement_manifest: Option<&[u8]>,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::create_message_kind_snapshot_row(
            &conn,
            folder_id,
            message_kind,
            message_manifest,
            placement_manifest,
        )
    }

    /// Fetch the BARE-serialised `(message_manifest, placement_manifest)`
    /// blob pair for a snapshot row. T7 restore-dispatch reads this to load
    /// the pinned manifests before replay. Returns `(None, None)` if the
    /// row doesn't exist (caller should have looked it up already; this
    /// path keeps the DAO non-fatal).
    pub async fn get_snapshot_kind_manifests(
        &self,
        snapshot_id: i64,
    ) -> anyhow::Result<(Option<Vec<u8>>, Option<Vec<u8>>)> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::get_snapshot_kind_manifests(&conn, snapshot_id)
    }

    /// Append a row to `restore_history` after a kind-aware snapshot
    /// restore commits. Not transactional with the restore SQL — call
    /// only on success. `source_member_id` is `None` until Plan 4 wires
    /// backup-destination provenance.
    pub async fn insert_restore_history(
        &self,
        actor_id: &[u8; 32],
        snapshot_id: i64,
        kinds_restored: &str,
        source_member_id: Option<&[u8]>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::insert_restore_history(
            &conn,
            actor_id,
            snapshot_id,
            kinds_restored,
            source_member_id,
        )
    }

    /// Is a bridge currently serving this actor? Pre-condition for
    /// message-kind snapshot restore (HTTP 409 if true). Stubbed today;
    /// wires to `subscribe_mailbox_state` once IMAP-restore
    /// Plan 1 lands the registration table.
    pub async fn bridge_active_for_actor(&self, actor_id: &[u8; 32]) -> anyhow::Result<bool> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::bridge_active_for_actor(&conn, actor_id)
    }

    /// Does the actor have any wrapped MLS blobs? Advisory check that
    /// warns the restore caller when bridge AUTH will fail until the
    /// bridge's wrapped-MLS-blob bundle is restored.
    pub async fn has_wrapped_mls_blobs(&self, actor_id: &[u8; 32]) -> anyhow::Result<bool> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::has_wrapped_mls_blobs(&conn, actor_id)
    }

    /// List the actor's restore history, newest first (owner-scoped).
    /// Backs `fauna.filesync.snapshot.list_restore_history`.
    pub async fn list_restore_history(
        &self,
        actor_id: &[u8; 32],
        limit: u32,
    ) -> anyhow::Result<Vec<fauna_protocol::filesync::RestoreHistoryRow>> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::list_restore_history(&conn, actor_id, limit)
    }

    /// List the divergence rows recorded against one snapshot's restore,
    /// newest first. Caller enforces owner-only access first. Backs
    /// `fauna.filesync.snapshot.list_restore_divergence`.
    pub async fn list_restore_divergence(
        &self,
        snapshot_id: i64,
    ) -> anyhow::Result<Vec<fauna_protocol::filesync::RestoreDivergenceRow>> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::list_restore_divergence(&conn, snapshot_id)
    }

    /// List the actor's message-kind snapshots, newest first, optionally
    /// filtered to one `kind` (owner-scoped). Backs
    /// `fauna.filesync.snapshot.list`.
    pub async fn list_message_kind_snapshots(
        &self,
        actor_id: &[u8; 32],
        kind: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<Vec<fauna_protocol::filesync::SnapshotSummaryRow>> {
        let conn = self.conn.lock().await;
        crate::db::snapshots::list_message_kind_snapshots(&conn, actor_id, kind, limit)
    }
}

/// A result from the FTS5 search index.
pub struct SearchResult {
    pub content_type: String,
    pub content_id: String,
    pub created_at: i64,
    pub rank: f64,
    pub snippet: String,
}

/// Sanitize a user-supplied search query for FTS5 MATCH.
/// Wraps each whitespace-separated term in double quotes to escape special characters.
/// If the query starts with "raw:", passes it through as-is (advanced mode).
/// Returns None if the query is empty after processing.
fn sanitize_fts_query(query: &str) -> Option<String> {
    if let Some(raw) = query.strip_prefix("raw:") {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(trimmed.to_string());
    }
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| {
            let escaped = term.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();
    if terms.is_empty() {
        return None;
    }
    Some(terms.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(audience: Option<&str>) -> FolderRow {
        let mut row = FolderRow {
            audience: audience.map(str::to_string),
            ..Default::default()
        };
        row.name = "photos".into();
        row
    }

    /// Only an explicit `public` declassification counts. `None` (the resting
    /// value for every folder that was never declassified) and any token this
    /// build does not recognise are NOT public — the fail-closed direction,
    /// since this predicate gates the paywall refusal and the content plane's
    /// plaintext arm.
    #[test]
    fn only_an_explicit_public_audience_is_public() {
        assert!(folder(Some("public")).is_public_audience());
        for not_public in [
            None,
            Some(""),
            Some("private"),
            Some("shared"),
            Some("Public"),
        ] {
            assert!(
                !folder(not_public).is_public_audience(),
                "audience {not_public:?} must not read as declassified"
            );
        }
    }

    /// An ordinary folder rests NO plaintext paths — the case the S9 flip
    /// refuses a sealless record for.
    #[test]
    fn an_ordinary_folder_rests_no_plaintext_paths() {
        assert!(!folder(None).rests_plaintext_paths());
        assert!(!folder(Some("shared")).rests_plaintext_paths());
    }

    #[test]
    fn table_exists_distinguishes_present_from_absent_and_never_created() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE real_one (id INTEGER PRIMARY KEY)", [])
            .unwrap();

        assert!(table_exists(&conn, "real_one").unwrap());
        assert!(!table_exists(&conn, "never_created").unwrap());
    }

    /// The S8 D4 scrub mechanism (`test-hooks`-gated; run these with
    /// `cargo test -p fauna-nest --lib --features test-hooks scrub_plaintext`).
    #[cfg(feature = "test-hooks")]
    mod scrub_plaintext {
        use super::*;

        fn text_at(conn: &Connection, sql: &str) -> Option<String> {
            conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
                .unwrap()
        }

        /// One seeded row per plane in each of the two states — sealed sibling
        /// resting vs. absent — then one scrub. Sealed rows lose their
        /// plaintext (to NULL, or `''` for the NOT NULL planes — the ratified
        /// scrub sentinel the render seams read as `Omit`); unsealed rows keep
        /// theirs verbatim, which is the whole predicate: it is what protects
        /// the machine-authored device labels no writer ever seals AND every
        /// other plaintext plane with no sealed sibling, whose fate is the S9
        /// ask's to decide.
        #[tokio::test]
        async fn scrub_destroys_plaintext_only_where_a_seal_rests() {
            let db = CacheDb::open_in_memory().unwrap();
            let actor = [9u8; 32];
            let fs = db.create_folder("docs", &actor).await.unwrap();
            {
                let conn = db.conn().await;
                conn.execute(
                    "UPDATE folders SET include_paths = '[\"Documents/2026 taxes\"]',
                            include_paths_sealed = x'CC', exclude_paths = '[\"Documents/cache\"]',
                            exclude_paths_sealed = x'DD', retention_policy = '{\"keep_last\":3}',
                            retention_policy_sealed = x'EE', name_sealed = x'FF'
                     WHERE id = ?1",
                    rusqlite::params![fs],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO sync_changes (actor_id, path_hash, change_type, created_at,
                            folder_id, path, path_sealed)
                     VALUES (?1, ?2, 'create', 1, ?3, '2026/eviction_notice.pdf', x'AA')",
                    rusqlite::params![&actor[..], &[1u8; 32][..], fs],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO sync_changes (actor_id, path_hash, change_type, created_at,
                            folder_id, path)
                     VALUES (?1, ?2, 'create', 1, ?3, 'historical/unsealed.txt')",
                    rusqlite::params![&actor[..], &[2u8; 32][..], fs],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO sync_devices (actor_id, device_id, label, label_sealed,
                            registered_at, last_seen)
                     VALUES (?1, ?2, 'Living-room laptop', x'BB', 1, 1)",
                    rusqlite::params![&actor[..], &[3u8; 32][..]],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO sync_devices (actor_id, device_id, label, registered_at, last_seen)
                     VALUES (?1, ?2, 'WebDAV', 1, 1)",
                    rusqlite::params![&actor[..], &[4u8; 32][..]],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO sync_conflicts (folder_id, device_id, path, conflict_type,
                            details, created_at, path_hash, path_sealed, details_sealed)
                     VALUES (?1, ?2, 'taxes/c1.txt', 'divergent', 'two writers raced', 1,
                            ?3, x'11', x'22')",
                    rusqlite::params![
                        fs,
                        &[3u8; 32][..],
                        fauna_core::sync::path_hash("taxes/c1.txt").to_vec()
                    ],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO sync_conflicts (folder_id, device_id, path, conflict_type,
                            details, created_at, path_hash)
                     VALUES (?1, ?2, 'taxes/c2.txt', 'divergent', 'kept plaintext', 1, ?3)",
                    rusqlite::params![
                        fs,
                        &[3u8; 32][..],
                        fauna_core::sync::path_hash("taxes/c2.txt").to_vec()
                    ],
                )
                .unwrap();
            }

            let report = db.scrub_plaintext_where_sealed().await.unwrap();
            let count = |plane: &str| {
                report
                    .iter()
                    .find(|(p, _)| *p == plane)
                    .map(|(_, n)| *n)
                    .unwrap()
            };
            assert_eq!(count("sync_changes.path"), 1);
            assert_eq!(count("sync_devices.label"), 1);
            assert_eq!(count("sync_conflicts.path"), 1);
            assert_eq!(count("sync_conflicts.details"), 1);
            assert_eq!(count("folders.include_paths"), 1);
            assert_eq!(count("folders.exclude_paths"), 1);
            assert!(
                !report.iter().any(|(p, _)| *p == "folders.retention_policy"),
                "retention_policy left the scrub planes at the flip — the ARMED \
                 auto-prune ruling parses its plaintext knobs server-side \
                 (encryption-at-rest.md § Carve-outs); scrubbing it would \
                 silently re-darken scheduled pruning"
            );

            let conn = db.conn().await;
            assert_eq!(
                text_at(
                    &conn,
                    "SELECT path FROM sync_changes WHERE path_sealed IS NOT NULL"
                ),
                None,
                "the sealed change's plaintext path is destroyed"
            );
            assert_eq!(
                text_at(
                    &conn,
                    "SELECT path FROM sync_changes WHERE path_sealed IS NULL"
                )
                .as_deref(),
                Some("historical/unsealed.txt"),
                "a row with no sealed sibling keeps its plaintext — the S9 ask's to decide"
            );
            assert_eq!(
                text_at(
                    &conn,
                    "SELECT label FROM sync_devices WHERE label_sealed IS NOT NULL"
                )
                .as_deref(),
                Some(""),
                "NOT NULL plane scrubs to '', the ratified sentinel"
            );
            assert_eq!(
                text_at(
                    &conn,
                    "SELECT label FROM sync_devices WHERE label_sealed IS NULL"
                )
                .as_deref(),
                Some("WebDAV"),
                "the machine-authored pseudo-device label survives — no writer ever seals it"
            );
            assert_eq!(
                text_at(
                    &conn,
                    "SELECT details FROM sync_conflicts WHERE details_sealed IS NOT NULL"
                ),
                None
            );
            assert_eq!(
                text_at(
                    &conn,
                    "SELECT path FROM sync_conflicts WHERE path_sealed IS NOT NULL"
                )
                .as_deref(),
                Some("")
            );
            assert_eq!(
                text_at(
                    &conn,
                    "SELECT path FROM sync_conflicts WHERE path_sealed IS NULL"
                )
                .as_deref(),
                Some("taxes/c2.txt")
            );
            assert_eq!(
                text_at(
                    &conn,
                    &format!("SELECT include_paths FROM folders WHERE id = {fs}")
                ),
                None
            );
            assert_eq!(
                text_at(
                    &conn,
                    &format!("SELECT retention_policy FROM folders WHERE id = {fs}")
                )
                .as_deref(),
                Some("{\"keep_last\":3}"),
                "retention_policy plaintext RESTS post-flip (ARMED auto-prune \
                 parses it server-side; the sealed sibling is the display copy)"
            );
        }

        /// `folders.name` joined the scrub at schema 114 (`path-sealing.md`
        /// § the set-name plane): a set whose seal and hash rest scrubs its
        /// plaintext name to NULL, while a sealless set and a `public` folder's
        /// URL segment keep theirs. The import plane scrubs alongside it to its
        /// `''` sentinel.
        #[tokio::test]
        async fn scrub_rests_a_sealed_sets_name_null_and_keeps_the_by_design_names() {
            let db = CacheDb::open_in_memory().unwrap();
            let actor = [9u8; 32];
            let fs = db.create_folder("docs", &actor).await.unwrap();
            let unsealed = db.create_folder("notes", &actor).await.unwrap();
            let public = db.create_folder("blog", &actor).await.unwrap();
            {
                let conn = db.conn().await;
                // Raw stamps, so the boot scrub (not a writer's blank) is what
                // this pins.
                conn.execute(
                    "UPDATE folders SET name_sealed = x'FF' WHERE id IN (?1, ?2)",
                    rusqlite::params![fs, public],
                )
                .unwrap();
                conn.execute(
                    "UPDATE folders SET audience = 'public' WHERE id = ?1",
                    rusqlite::params![public],
                )
                .unwrap();
                // The import plane, by contrast, IS scrubbed now.
                conn.execute(
                    "INSERT INTO import_sessions (session_id, actor_id, source_descriptor, state,
                            started_at, last_progress_at, expires_at, source_sealed)
                     VALUES ('s1', ?1, 'imap://mail.example', 'completed', 1, 1, 9999999999, x'33')",
                    rusqlite::params![&actor[..]],
                )
                .unwrap();
            }

            db.scrub_plaintext_where_sealed().await.unwrap();

            let conn = db.conn().await;
            assert_eq!(
                text_at(&conn, &format!("SELECT name FROM folders WHERE id = {fs}")),
                None,
                "a sealed set's name scrubs to NULL"
            );
            assert_eq!(
                text_at(
                    &conn,
                    &format!("SELECT name FROM folders WHERE id = {unsealed}")
                )
                .as_deref(),
                Some("notes"),
                "a sealless set keeps its name"
            );
            assert_eq!(
                text_at(
                    &conn,
                    &format!("SELECT name FROM folders WHERE id = {public}")
                )
                .as_deref(),
                Some("blog"),
                "a public folder's name is its URL segment"
            );
            assert_eq!(
                text_at(&conn, "SELECT source_descriptor FROM import_sessions").as_deref(),
                Some(""),
                "the import descriptor scrubs to '' (NOT NULL sentinel) since the flip"
            );
        }

        /// A covered-folder mirror's custody path survives the boot scrub
        /// beside its sealed name, while an ordinary set's custody row still
        /// scrubs. On a reserved (`__`) backup set the path is the source row's
        /// `path_hash`, hex-spelled — a routing key the nest-held pull-back
        /// addresses the row by, and nothing can re-derive it from the row's
        /// own one-way `path_hash` (`segment-backup-protocol.md` § *The
        /// nest-held pull-back*).
        #[tokio::test]
        async fn scrub_keeps_a_reserved_backup_sets_custody_path_beside_its_seal() {
            let db = CacheDb::open_in_memory().unwrap();
            let actor = [9u8; 32];
            let mirror = db.create_folder("mirror", &actor).await.unwrap();
            let ordinary = db.create_folder("vault", &actor).await.unwrap();
            let leaf = "ab".repeat(32);
            {
                let conn = db.conn().await;
                conn.execute(
                    "UPDATE folders SET name = '__folder/aa/7', custody_copy = 1 WHERE id = ?1",
                    rusqlite::params![mirror],
                )
                .unwrap();
                for (folder, path) in [(mirror, leaf.as_str()), (ordinary, "vault/deed.tiff")] {
                    conn.execute(
                        "INSERT INTO backup_custody (uploader_actor, folder_id, path_hash,
                                size_bytes, updated_at, path, path_sealed)
                         VALUES (?1, ?2, ?3, 1, 1, ?4, x'5E')",
                        rusqlite::params![
                            &actor[..],
                            folder,
                            &fauna_core::sync::path_hash(path)[..],
                            path
                        ],
                    )
                    .unwrap();
                }
            }

            db.scrub_plaintext_where_sealed().await.unwrap();

            let conn = db.conn().await;
            assert_eq!(
                text_at(
                    &conn,
                    &format!("SELECT path FROM backup_custody WHERE folder_id = {mirror}")
                ),
                Some(leaf),
                "a reserved backup set keeps the leaf the pull-back addresses by"
            );
            assert_eq!(
                text_at(
                    &conn,
                    &format!("SELECT path FROM backup_custody WHERE folder_id = {ordinary}")
                ),
                None,
                "an ordinary set's custody path still scrubs beside its seal"
            );
        }

        /// A folder whose paths rest plaintext BY DESIGN (a `public` audience)
        /// keeps its plaintext `path` on
        /// both path planes even where an over-sealing writer rested a seal
        /// beside it. The public follower's projection withholds the seal
        /// (`folder_public::strip_for_public`), so a scrubbed row would reach it
        /// with neither and be refused as `NoSeal` — the file lost to every
        /// follower until the owner re-records it. The same row on an ordinary
        /// folder (and on a folder flipped back to private) is still scrubbed:
        /// the exemption is exactly `FolderRow::rests_plaintext_paths`, never
        /// wider.
        #[tokio::test]
        async fn scrub_leaves_a_public_folder_s_plaintext_path_resting_beside_its_seal() {
            let db = CacheDb::open_in_memory().unwrap();
            let actor = [9u8; 32];
            let public = db.create_folder("site", &actor).await.unwrap();
            let private = db.create_folder("private", &actor).await.unwrap();
            {
                let conn = db.conn().await;
                conn.execute(
                    "UPDATE folders SET audience = 'public' WHERE id = ?1",
                    rusqlite::params![public],
                )
                .unwrap();
                for (n, fs) in [(1u8, public), (3, private)] {
                    conn.execute(
                        "INSERT INTO sync_changes (actor_id, path_hash, change_type, created_at,
                                folder_id, path, path_sealed)
                         VALUES (?1, ?2, 'create', 1, ?3, ?4, x'AA')",
                        rusqlite::params![&actor[..], &[n; 32][..], fs, format!("f{n}.md")],
                    )
                    .unwrap();
                    let path = format!("c{n}.md");
                    conn.execute(
                        "INSERT INTO sync_conflicts (folder_id, device_id, path, conflict_type,
                                created_at, path_hash, path_sealed)
                         VALUES (?1, ?2, ?3, 'divergent', 1, ?4, x'11')",
                        rusqlite::params![
                            fs,
                            &[3u8; 32][..],
                            path,
                            fauna_core::sync::path_hash(&path).to_vec()
                        ],
                    )
                    .unwrap();
                }
            }

            db.scrub_plaintext_where_sealed().await.unwrap();

            let conn = db.conn().await;
            let change = |fs: i64| {
                text_at(
                    &conn,
                    &format!("SELECT path FROM sync_changes WHERE folder_id = {fs}"),
                )
            };
            let conflict = |fs: i64| {
                text_at(
                    &conn,
                    &format!("SELECT path FROM sync_conflicts WHERE folder_id = {fs}"),
                )
            };
            assert_eq!(
                change(public).as_deref(),
                Some("f1.md"),
                "a public folder's plaintext path is its ratified at-rest shape — \
                 the only label a public follower ever receives"
            );
            assert_eq!(change(private), None, "an ordinary folder still scrubs");
            assert_eq!(conflict(public).as_deref(), Some("c1.md"));
            assert_eq!(conflict(private).as_deref(), Some(""));
        }

        /// The scrub's SQL spelling of the plaintext-paths class agrees with
        /// [`FolderRow::rests_plaintext_paths`] over every audience — the two
        /// must never drift, or the scrub either widens the
        /// exemption past the ratified classes or re-opens the follower hole.
        #[tokio::test]
        async fn the_scrub_s_plaintext_class_is_the_folder_row_predicate() {
            let db = CacheDb::open_in_memory().unwrap();
            let actor = [9u8; 32];
            let conn_sql = format!(
                "SELECT EXISTS ({}) FROM folders WHERE id = ?1",
                migrations::plaintext_paths_folder_sql!("f.id = ?1")
            );
            for (i, audience) in [None, Some("public"), Some("shared"), Some("bogus")]
                .into_iter()
                .enumerate()
            {
                let fs = db.create_folder(&format!("f{i}"), &actor).await.unwrap();
                let conn = db.conn().await;
                conn.execute(
                    "UPDATE folders SET audience = ?1 WHERE id = ?2",
                    rusqlite::params![audience, fs],
                )
                .unwrap();
                let sql_says: bool = conn
                    .query_row(&conn_sql, rusqlite::params![fs], |r| r.get(0))
                    .unwrap();
                assert_eq!(
                    sql_says,
                    folder(audience).rests_plaintext_paths(),
                    "audience {audience:?}"
                );
            }
        }
    }

    /// `CacheDb::open` on a DB stamped to an incompatible `min_reader_version`
    /// returns the typed, downcastable `SchemaIncompatible` (NOT a generic open
    /// error and NOT a panic) — the signal the boot path turns into degraded
    /// "needs-update" serve mode (version-compatibility.md § 2.2). It must also
    /// have left the DB unmigrated/untouched.
    #[test]
    fn open_on_incompatible_db_returns_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nest.db");

        // First open: records (CURRENT, MIN) = (1,1).
        CacheDb::open(&path).unwrap();

        // Simulate a newer nest having written a breaking schema: stamp the DB's
        // schema_meta above this binary's reader floor.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE schema_meta SET schema_version = ?1, min_reader_version = ?2 WHERE id = 1",
                rusqlite::params![
                    migrations::CURRENT_SCHEMA_VERSION as i64 + 7,
                    migrations::CURRENT_SCHEMA_VERSION as i64 + 2,
                ],
            )
            .unwrap();
        }

        // Re-open: the compatibility gate must reject it with SchemaIncompatible.
        let err = match CacheDb::open(&path) {
            Ok(_) => panic!("incompatible DB must not open"),
            Err(e) => e,
        };
        let inc = err
            .downcast_ref::<migrations::SchemaIncompatible>()
            .expect("error must downcast to SchemaIncompatible");
        assert_eq!(inc.db_v, migrations::CURRENT_SCHEMA_VERSION + 7);
        assert_eq!(inc.db_min, migrations::CURRENT_SCHEMA_VERSION + 2);
        assert_eq!(inc.bin_v, migrations::CURRENT_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn inbox_roundtrip() {
        let db = CacheDb::open_in_memory().unwrap();
        let recipient = [1u8; 32];
        let payload = b"hello world";

        let id = db.push_inbox(&recipient, payload, None).await.unwrap();
        assert!(id > 0);

        let msgs = db.poll_inbox(&recipient).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].0, id);
        assert_eq!(msgs[0].1, payload);
        assert!(msgs[0].2.is_none());

        db.ack_inbox(&recipient, &[id]).await.unwrap();
        let msgs = db.poll_inbox(&recipient).await.unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn post_roundtrip() {
        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [2u8; 32];
        let data = b"post data bytes";

        assert!(db.get_post(&post_id).await.unwrap().is_none());
        db.put_post(&post_id, data, None).await.unwrap();
        let (got, blob_hash) = db.get_post(&post_id).await.unwrap().unwrap();
        assert_eq!(got, data);
        assert!(blob_hash.is_none());
    }

    #[tokio::test]
    async fn admin_actor_crud() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [42u8; 32];

        // Initially no admins
        assert_eq!(db.admin_count().await.unwrap(), 0);
        assert!(!db.is_admin(&actor).await.unwrap());

        // Add admin
        db.add_admin_actor(&actor).await.unwrap();
        assert!(db.is_admin(&actor).await.unwrap());
        assert_eq!(db.admin_count().await.unwrap(), 1);

        // List admins
        let admins = db.list_admin_actors().await.unwrap();
        assert_eq!(admins.len(), 1);
        assert_eq!(admins[0].0, actor.to_vec());

        // Duplicate add is a no-op
        db.add_admin_actor(&actor).await.unwrap();
        assert_eq!(db.admin_count().await.unwrap(), 1);

        // Removing the sole superadmin is floor-refused (admin.md § 2): the
        // writer itself holds the last-superadmin floor.
        assert_eq!(
            db.remove_admin_actor(&actor).await.unwrap(),
            admin::RosterWrite::RefusedLastSuperadmin
        );
        assert!(db.is_admin(&actor).await.unwrap());

        // A peer superadmin makes the removal legal.
        let peer = [43u8; 32];
        db.add_admin_actor(&peer).await.unwrap();
        assert_eq!(
            db.remove_admin_actor(&actor).await.unwrap(),
            admin::RosterWrite::Applied
        );
        assert!(!db.is_admin(&actor).await.unwrap());
        assert_eq!(db.admin_count().await.unwrap(), 1);

        // Remove non-existent reports NotAnAdmin (idempotent completion).
        assert_eq!(
            db.remove_admin_actor(&actor).await.unwrap(),
            admin::RosterWrite::NotAnAdmin
        );
    }

    #[tokio::test]
    async fn tiers_seeded() {
        let db = CacheDb::open_in_memory().unwrap();
        let tiers = db.list_tiers().await.unwrap();
        // free / personal / community + the storage-only backup tier
        // (held-for-friends), all `SEED_TIERS` — see `backup_tier_is_seeded_storage_only`.
        assert_eq!(tiers.len(), 4);

        let free = db.get_tier("free").await.unwrap().unwrap();
        assert_eq!(free.max_inbox_bytes, 104857600); // 100 MB
        // The shipped free tier's device allowance is 3 — a laptop, a phone,
        // and one more co-located app (which keeps its own `sync_devices`
        // row) — ruled 2026-09-26, `admin.md` § 2 Users → *Device
        // enforcement*. Only a fresh nest gets this seed (`INSERT OR
        // IGNORE`); an admin raises or lowers it from the app UI.
        assert_eq!(free.max_devices, 3);
        assert_eq!(
            db.get_tier("personal").await.unwrap().unwrap().max_devices,
            5
        );
        assert_eq!(
            db.get_tier("community").await.unwrap().unwrap().max_devices,
            10
        );
    }

    #[tokio::test]
    async fn user_crud() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [3u8; 32];

        db.create_user(&actor, "free", "test user").await.unwrap();

        let users = db.list_users().await.unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].tier, "free");
        assert_eq!(users[0].label, "test user");
        assert!(!users[0].suspended);

        db.update_user(&actor, "personal", "updated").await.unwrap();
        let user = db.get_user(&actor).await.unwrap().unwrap();
        assert_eq!(user.tier, "personal");
        assert_eq!(user.label, "updated");

        db.delete_user(&actor).await.unwrap();
        assert!(db.get_user(&actor).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn quota_enforcement() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [4u8; 32];
        db.create_user(&actor, "free", "quota test").await.unwrap();

        // Should pass for small payload
        db.check_quota(&actor, 100).await.unwrap();

        // Should fail for payload exceeding max_blob_size (10 MB)
        let result = db.check_quota(&actor, 11_000_000).await;
        assert!(result.is_err());

        // Unregistered user should fail
        let unknown = [5u8; 32];
        assert!(db.check_quota(&unknown, 100).await.is_err());
    }

    #[tokio::test]
    async fn push_inbox_with_quota_updates_bytes() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [6u8; 32];
        db.create_user(&actor, "free", "").await.unwrap();

        let payload = vec![0u8; 1000];
        let id = db
            .push_inbox_with_quota(&actor, &payload, None)
            .await
            .unwrap();

        let user = db.get_user(&actor).await.unwrap().unwrap();
        assert_eq!(user.inbox_bytes_used, 1000);

        db.ack_inbox(&actor, &[id]).await.unwrap();
        let user = db.get_user(&actor).await.unwrap().unwrap();
        assert_eq!(user.inbox_bytes_used, 0);
    }

    /// A refund never exceeds the charge: an uncharged push (the
    /// same-nest Welcome off enforcement, a security notice, a subscription
    /// welcome) that is then acked leaves `inbox_bytes_used` where the charged,
    /// still-pending traffic put it. Before links recorded their charge the ack
    /// refunded every row's size, so each acked uncharged row deflated the
    /// counter and the tier cap stopped binding.
    #[tokio::test]
    async fn acking_an_uncharged_row_refunds_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [8u8; 32];
        db.create_user(&actor, "free", "").await.unwrap();

        db.push_inbox_with_quota(&actor, &[0u8; 1000], None)
            .await
            .unwrap();
        let inline = db.push_inbox(&actor, &[1u8; 700], None).await.unwrap();
        let spilled = db
            .push_inbox(&actor, &[2u8; 500], Some(&[9u8; 32]))
            .await
            .unwrap();
        db.put_blob_metadata(&[9u8; 32], 500, "application/octet-stream", None, None)
            .await
            .unwrap();
        assert_eq!(
            db.get_user(&actor).await.unwrap().unwrap().inbox_bytes_used,
            1000
        );

        assert_eq!(db.ack_inbox(&actor, &[inline, spilled]).await.unwrap(), 2);
        assert_eq!(
            db.get_user(&actor).await.unwrap().unwrap().inbox_bytes_used,
            1000,
            "acking uncharged rows must not refund the charged, pending bytes"
        );
    }

    /// The ack refunds what the link recorded, not what the blob row says: a
    /// charged spilled envelope with no `blob_metadata` row still refunds its
    /// charge. A link recording no charge refunds nothing (`inbox::refund_for`).
    #[tokio::test]
    async fn the_ack_refunds_the_recorded_charge_and_nothing_without_one() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [10u8; 32];
        db.create_user(&actor, "free", "").await.unwrap();

        let spilled = db
            .push_inbox_with_quota(&actor, &[3u8; 400], Some(&[11u8; 32]))
            .await
            .unwrap();
        let uncharged = db
            .push_inbox_with_quota(&actor, &[4u8; 250], None)
            .await
            .unwrap();
        db.conn
            .lock()
            .await
            .execute(
                "UPDATE content_links SET metadata = ?1 WHERE id = ?2",
                rusqlite::params![br#"{"mailbox":"INBOX"}"#.to_vec(), uncharged],
            )
            .unwrap();
        assert_eq!(
            db.get_user(&actor).await.unwrap().unwrap().inbox_bytes_used,
            650
        );

        db.ack_inbox(&actor, &[spilled]).await.unwrap();
        assert_eq!(
            db.get_user(&actor).await.unwrap().unwrap().inbox_bytes_used,
            250,
            "a charged spill refunds its recorded charge without a blob row"
        );
        db.ack_inbox(&actor, &[uncharged]).await.unwrap();
        assert_eq!(
            db.get_user(&actor).await.unwrap().unwrap().inbox_bytes_used,
            250,
            "a link recording no charge refunds nothing"
        );
    }

    /// Standing evidence for `fauna.inbox.send`'s `forbid_replay = true`
    /// (71st pass). Asserts the **hazard**, not the flag — the same shape as
    /// `conformance_payments::minting_twice_yields_two_independently_redeemable_claims`
    /// — so it stays meaningful if the handler is ever made idempotent: change
    /// this test first, then the flag, then both metadata tables.
    ///
    /// The identical payload delivered twice is what a post-reconnect
    /// `request_auto_retry` would produce. Nothing dedups it: `content_id` is
    /// `blake3(recipient ‖ now_millis ‖ INBOX_NONCE ‖ payload)`, so the key is
    /// unique by construction on every call.
    #[tokio::test]
    async fn sending_twice_double_charges_the_recipients_quota() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];
        db.create_user(&actor, "free", "").await.unwrap();

        let payload = vec![0u8; 1000];
        let first = db
            .push_inbox_with_quota(&actor, &payload, None)
            .await
            .unwrap();
        // Byte-identical replay of the same logical send.
        let second = db
            .push_inbox_with_quota(&actor, &payload, None)
            .await
            .unwrap();

        // Two distinct deliveries, not one deduplicated one.
        assert_ne!(
            first, second,
            "the replay produced a second, independent delivery row"
        );
        assert_eq!(
            db.poll_inbox(&actor).await.unwrap().len(),
            2,
            "the recipient sees the same message twice"
        );

        // ...and the quota is charged for both copies. This is the harm that
        // makes the kind replay-forbidden: the user never spent these bytes,
        // and there is no compensating decrement short of recomputation.
        let user = db.get_user(&actor).await.unwrap().unwrap();
        assert_eq!(
            user.inbox_bytes_used, 2000,
            "inbox_bytes_used counts the replayed copy a second time"
        );
    }

    /// Standing evidence for `fauna.admin.invite_codes.create`'s
    /// `forbid_replay = true` (71st pass). Asserts the **hazard**, not the
    /// flag: the handler's minting branch (empty `req.code`) allocates a fresh
    /// `generate_invite_code()` per call and plain-INSERTs a row keyed on it,
    /// so a replayed create leaves a SECOND independently redeemable admission
    /// credential — each with its own full `uses_left` — against one admin
    /// action. On an invite-gated nest that is admission capacity the admin
    /// never authorized.
    ///
    /// The admin-supplied-code branch is idempotent (the UNIQUE on `code`
    /// rejects the repeat), and that asymmetry is the point: `forbid_replay`
    /// is per-kind, so the non-idempotent branch decides it.
    #[tokio::test]
    async fn creating_twice_yields_two_independently_redeemable_invite_codes() {
        let db = CacheDb::open_in_memory().unwrap();

        // What the handler does twice when `req.code` is empty.
        let first = crate::admin::generate_invite_code();
        let second = crate::admin::generate_invite_code();
        assert_ne!(
            first, second,
            "each call mints a fresh code, so nothing can dedup the insert"
        );

        db.create_invite_code_with_guardian(&first, "free", 1, None, None)
            .await
            .unwrap();
        db.create_invite_code_with_guardian(&second, "free", 1, None, None)
            .await
            .unwrap();

        // Both redeem, independently — two admissions for one admin intent.
        assert!(
            db.validate_invite_code(&first).await.unwrap().is_some(),
            "first code admits"
        );
        assert!(
            db.validate_invite_code(&second).await.unwrap().is_some(),
            "the replayed code admits a SECOND time"
        );

        // Positive control: the admin-supplied-code branch really is safe —
        // re-inserting the same code is refused, so only that branch could
        // ever have justified `forbid_replay = false`.
        assert!(
            db.create_invite_code_with_guardian(&first, "free", 1, None, None)
                .await
                .is_err(),
            "a caller-supplied duplicate code is rejected by the UNIQUE"
        );
    }

    #[tokio::test]
    async fn audit_log_roundtrip() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin_actor = [1u8; 32];
        db.audit(
            Some(&admin_actor),
            "user.create",
            Some("abc123"),
            Some("created user"),
        )
        .await
        .unwrap();
        db.audit(Some(&admin_actor), "user.delete", Some("abc123"), None)
            .await
            .unwrap();

        let entries = db.list_audit(100, None).await.unwrap();
        assert_eq!(entries.len(), 2);
        // Most recent first
        assert_eq!(entries[0].action, "user.delete");
        assert_eq!(entries[1].action, "user.create");
        assert_eq!(entries[1].detail.as_deref(), Some("created user"));

        // Pagination: before_id
        let page = db.list_audit(100, Some(entries[0].id)).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].action, "user.create");
    }

    #[tokio::test]
    async fn audit_hash_chain() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin_actor = [1u8; 32];

        db.audit(
            Some(&admin_actor),
            "user.create",
            Some("actor1"),
            Some("created"),
        )
        .await
        .unwrap();
        db.audit(
            Some(&admin_actor),
            "tier.update",
            Some("actor1"),
            Some("free->paid"),
        )
        .await
        .unwrap();
        db.audit(Some(&admin_actor), "user.delete", Some("actor2"), None)
            .await
            .unwrap();

        let entries = db.list_audit(100, None).await.unwrap();
        assert_eq!(entries.len(), 3);

        // Entries come back newest-first, reverse for chain verification
        let mut entries = entries;
        entries.reverse();

        // First entry's prev_hash should be the genesis constant
        use sha2::Digest;
        let genesis = format!("{:x}", sha2::Sha256::digest(b"fauna-audit-genesis-v1"));
        assert_eq!(entries[0].prev_hash, genesis);

        // Each subsequent entry's prev_hash should equal the previous entry's entry_hash
        assert_eq!(entries[1].prev_hash, entries[0].entry_hash);
        assert_eq!(entries[2].prev_hash, entries[1].entry_hash);

        // entry_hash should be non-empty
        for e in &entries {
            assert!(!e.entry_hash.is_empty());
        }
    }

    /// An auditor's verification, exactly as `succession-aftermath.md` describes
    /// it: recompute each served row's `entry_hash` from its own columns and
    /// check every `prev_hash` link. Returns true when the chain certifies.
    ///
    /// The preimage comes from the version the row **records** — one format per
    /// row, and a version this binary does not know is refused rather than
    /// tried against another format. That refusal is the point:
    /// [`crate::db::chain_version`] carries the argument, and
    /// `an_unknown_recorded_version_is_refused_not_retried` below is the pin
    /// that fails against a verifier written the other way.
    fn auditor_verifies(entries: &[AuditRow]) -> bool {
        use crate::db::chain_version::{self, ChainVersion};
        use sha2::Digest;
        let mut expected_prev = format!("{:x}", sha2::Sha256::digest(b"fauna-audit-genesis-v1"));
        for e in entries {
            if e.prev_hash != expected_prev {
                return false;
            }
            let Some(version) = ChainVersion::from_recorded(e.entry_hash_version) else {
                return false;
            };
            let recomputed = chain_version::audit_entry_hash(
                version,
                &chain_version::AuditPreimage {
                    id: e.id,
                    ts: e.ts,
                    actor_id: e.actor_id.as_deref(),
                    action: &e.action,
                    target: e.target.as_deref(),
                    detail: e.detail.as_deref(),
                    prev_hash: &e.prev_hash,
                },
            );
            if recomputed != e.entry_hash {
                return false;
            }
            expected_prev = e.entry_hash.clone();
        }
        true
    }

    /// `succession-aftermath.md` argues `audit_log.actor_id` must stay put
    /// because "a move is a break an admin's own verification finds" — so the
    /// chain has to bind the COLUMN BOUNDARIES, not just the concatenation of
    /// the columns. An unframed preimage binds only the concatenation: this
    /// exact re-partition recomputed to the identical `entry_hash` under one,
    /// and the verification certified the tampered row. Red-verified against
    /// the unframed preimage when the framing landed.
    #[tokio::test]
    async fn audit_chain_binds_column_boundaries_not_just_their_concatenation() {
        let db = CacheDb::open_in_memory().unwrap();
        let admin_actor = [1u8; 32];

        db.audit(
            Some(&admin_actor),
            "role.grant.superadmin",
            Some("mallory"),
            None,
        )
        .await
        .unwrap();

        let mut entries = db.list_audit(100, None).await.unwrap();
        entries.reverse();
        assert!(
            auditor_verifies(&entries),
            "baseline: the untampered chain must certify"
        );
        let stored_hash = entries[0].entry_hash.clone();

        // Re-partition the row: same concatenation, different columns. The
        // event now reads as an innocuous "role.grant" and no `WHERE action =
        // 'role.grant.superadmin'` query, filter or admin-UI grouping can see
        // what actually happened.
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE audit_log SET action = ?1, target = ?2 WHERE id = ?3",
                rusqlite::params!["role.grant", ".superadminmallory", entries[0].id],
            )
            .unwrap();
        }

        let mut tampered = db.list_audit(100, None).await.unwrap();
        tampered.reverse();
        assert_eq!(
            tampered[0].action, "role.grant",
            "the row really did change"
        );
        assert_eq!(
            tampered[0].entry_hash, stored_hash,
            "the stored chain hash was not touched"
        );
        assert!(
            !auditor_verifies(&tampered),
            "the admin's verification must FIND the move: with length framing \
             the re-partitioned row recomputes to a different entry_hash"
        );
    }

    /// The refuse rule end to end: a version this binary does not implement is
    /// unverifiable, not an invitation to try the formats it does implement.
    #[tokio::test]
    async fn an_unknown_recorded_version_is_refused_not_retried() {
        let db = CacheDb::open_in_memory().unwrap();
        db.audit(Some(&[3u8; 32]), "user.suspend", Some("bob"), None)
            .await
            .unwrap();
        let mut entries = db.list_audit(100, None).await.unwrap();
        entries.reverse();
        assert!(auditor_verifies(&entries));

        // A v3 row, arriving on a v2 binary: the columns and the stored digest
        // are untouched and genuinely DO verify under v2 — only the recorded
        // version says otherwise. A verifier that guesses would certify it.
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE audit_log SET entry_hash_version = 3 WHERE id = ?1",
                rusqlite::params![entries[0].id],
            )
            .unwrap();
        }
        let mut future = db.list_audit(100, None).await.unwrap();
        future.reverse();
        assert!(
            !auditor_verifies(&future),
            "a version this binary does not implement must be refused"
        );

        // And the retired numbers — `0` (unrecorded) and `1` (the unframed
        // preimage) — are refused on the same terms: no arm for either exists.
        for retired in [0, 1] {
            {
                let conn = db.conn.lock().await;
                conn.execute(
                    "UPDATE audit_log SET entry_hash_version = ?1 WHERE id = ?2",
                    rusqlite::params![retired, entries[0].id],
                )
                .unwrap();
            }
            let mut stamped = db.list_audit(100, None).await.unwrap();
            stamped.reverse();
            assert!(!auditor_verifies(&stamped), "version {retired}");
        }
    }

    #[tokio::test]
    async fn stats_query() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];
        db.create_user(&actor, "free", "").await.unwrap();
        db.push_inbox(&actor, b"msg1", None).await.unwrap();

        let stats = db.get_stats().await.unwrap();
        assert_eq!(stats.total_users, 1);
        assert!(stats.total_inbox_bytes > 0);
    }

    #[tokio::test]
    async fn handle_resolution() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [10u8; 32];
        db.create_user(&actor, "free", "alice test").await.unwrap();

        // No handle set yet
        assert!(db.resolve_handle("alice").await.unwrap().is_none());

        // Set handle
        db.set_handle(&actor, "alice").await.unwrap();
        let resolved = db.resolve_handle("alice").await.unwrap().unwrap();
        assert_eq!(resolved, actor);

        // Duplicate handle should fail
        let actor2 = [11u8; 32];
        db.create_user(&actor2, "free", "bob test").await.unwrap();
        assert!(db.set_handle(&actor2, "alice").await.is_err());

        // Clear handle
        db.set_handle(&actor, "").await.unwrap();
        assert!(db.resolve_handle("alice").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn enqueue_outbound_creates_one_row_per_recipient() {
        use crate::db::outbound::{InboundVerdictsSnapshot, NewOutbound, OutboundStatus};

        let db = CacheDb::open_in_memory().unwrap();
        let ids = db
            .enqueue_outbound(NewOutbound {
                original_msgid: "<m1@example.com>",
                original_sender: "alice@example.com",
                recipients: &["bob@example.net", "carol@example.org"],
                raw_message: b"raw RFC5322 message",
                inbound_verdicts: InboundVerdictsSnapshot {
                    spf: "pass".into(),
                    dmarc: "pass".into(),
                    dmarc_policy: "none".into(),
                },
                is_forwarded: false,
                forward_actor_id: None,
                forward_rule_id: None,
                forward_copy_mode: None,
                submit_actor_id: None,
            })
            .await
            .unwrap();
        assert_eq!(ids.len(), 2, "one row per recipient");

        let due = db.fetch_due_outbound(i64::MAX, 10).await.unwrap();
        assert_eq!(due.len(), 2, "both rows are pending and due");
        let mut recipients: Vec<_> = due.iter().map(|r| r.recipient.clone()).collect();
        recipients.sort();
        assert_eq!(recipients, vec!["bob@example.net", "carol@example.org"]);
        assert!(due.iter().all(|r| r.original_msgid == "<m1@example.com>"));
        assert!(
            due.iter()
                .all(|r| matches!(r.status, OutboundStatus::Pending))
        );
        assert!(due.iter().all(|r| r.attempt_count == 0));
    }

    #[tokio::test]
    async fn outbound_mark_attempt_advances_next_attempt_at() {
        use crate::db::outbound::{InboundVerdictsSnapshot, NewOutbound};

        let db = CacheDb::open_in_memory().unwrap();
        let ids = db
            .enqueue_outbound(NewOutbound {
                original_msgid: "<m2@example.com>",
                original_sender: "alice@example.com",
                recipients: &["bob@example.net"],
                raw_message: b"message",
                inbound_verdicts: InboundVerdictsSnapshot {
                    spf: "pass".into(),
                    dmarc: "pass".into(),
                    dmarc_policy: "none".into(),
                },
                is_forwarded: false,
                forward_actor_id: None,
                forward_rule_id: None,
                forward_copy_mode: None,
                submit_actor_id: None,
            })
            .await
            .unwrap();

        let row_id = ids[0];
        db.mark_outbound_attempt(row_id, i64::MAX / 2, Some("421 try again"), Some("4.4.7"))
            .await
            .unwrap();

        // Now-relative fetch (with `now = 0`) should not return the row — next_attempt_at is in the future.
        let due = db.fetch_due_outbound(0, 10).await.unwrap();
        assert!(due.is_empty(), "row not yet due");

        // Pulling far in the future should return it with attempt_count incremented.
        let due = db.fetch_due_outbound(i64::MAX, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].attempt_count, 1);
        assert_eq!(due[0].last_error.as_deref(), Some("421 try again"));
        assert_eq!(due[0].last_enhanced.as_deref(), Some("4.4.7"));
    }

    #[tokio::test]
    async fn tlsrpt_reports_insert_and_retention_sweep() {
        let db = CacheDb::open_in_memory().unwrap();
        let t0: i64 = 1_700_000_000;

        db.insert_tlsrpt_outbound_report(
            "r-1",
            "example.net",
            "2026-05-14",
            "mailto",
            "mailto:tlsrpt@example.net",
            br#"{"organization-name":"fauna.example"}"#,
            t0,
        )
        .await
        .unwrap();

        // Idempotent — same unique tuple stays at one row.
        db.insert_tlsrpt_outbound_report(
            "r-1",
            "example.net",
            "2026-05-14",
            "mailto",
            "mailto:tlsrpt@example.net",
            br#"{"x":1}"#,
            t0 + 60,
        )
        .await
        .unwrap();

        // Day-2 row, distinct transport — separate row.
        db.insert_tlsrpt_outbound_report(
            "r-2",
            "example.net",
            "2026-05-15",
            "https",
            "https://tlsrpt.example.com/upload",
            b"{}",
            t0 + 86_400,
        )
        .await
        .unwrap();

        // 8 days later, sweep with 7-day retention — first row gone,
        // second still recent.
        let removed = db
            .sweep_tlsrpt_outbound_reports(t0 + 8 * 86_400, 7 * 86_400)
            .await
            .unwrap();
        assert_eq!(removed, 1);
    }

    #[tokio::test]
    async fn bounce_rate_limit_suppresses_within_window() {
        let db = CacheDb::open_in_memory().unwrap();

        let t0: i64 = 1_700_000_000;
        // No prior bounce — caller is clear.
        assert!(
            !db.bounce_rate_limit_hit("alice@example.com", "<m1@example.com>", t0, 7 * 86_400)
                .await
                .unwrap()
        );

        db.record_bounce("alice@example.com", "<m1@example.com>", t0)
            .await
            .unwrap();

        // 1 day later — still within 7-day window, suppressed.
        assert!(
            db.bounce_rate_limit_hit(
                "alice@example.com",
                "<m1@example.com>",
                t0 + 86_400,
                7 * 86_400
            )
            .await
            .unwrap()
        );

        // 8 days later — outside window, OK to bounce again.
        assert!(
            !db.bounce_rate_limit_hit(
                "alice@example.com",
                "<m1@example.com>",
                t0 + 8 * 86_400,
                7 * 86_400
            )
            .await
            .unwrap()
        );

        // Different msgid — independent of the first.
        assert!(
            !db.bounce_rate_limit_hit("alice@example.com", "<m2@example.com>", t0, 7 * 86_400)
                .await
                .unwrap()
        );

        // Different sender — independent of the first.
        assert!(
            !db.bounce_rate_limit_hit("bob@example.com", "<m1@example.com>", t0, 7 * 86_400)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn outbound_status_transitions() {
        use crate::db::outbound::{
            InboundVerdictsSnapshot, NewOutbound, OutboundStatus, SuppressReason,
        };

        let db = CacheDb::open_in_memory().unwrap();
        let ids = db
            .enqueue_outbound(NewOutbound {
                original_msgid: "<m3@example.com>",
                original_sender: "alice@example.com",
                recipients: &["bob@example.net", "carol@example.org", "dave@example.com"],
                raw_message: b"message",
                inbound_verdicts: InboundVerdictsSnapshot {
                    spf: "pass".into(),
                    dmarc: "pass".into(),
                    dmarc_policy: "none".into(),
                },
                is_forwarded: false,
                forward_actor_id: None,
                forward_rule_id: None,
                forward_copy_mode: None,
                submit_actor_id: None,
            })
            .await
            .unwrap();

        db.mark_outbound_sent(ids[0]).await.unwrap();
        db.mark_outbound_permfail(ids[1], "550 nope", "5.1.1")
            .await
            .unwrap();
        db.mark_outbound_suppressed(ids[2], SuppressReason::SpfHardfail)
            .await
            .unwrap();

        // None are pending now.
        let due = db.fetch_due_outbound(i64::MAX, 10).await.unwrap();
        assert!(due.is_empty());

        // Verify the statuses by direct lookup helper.
        let all = db.fetch_all_outbound_for_test().await.unwrap();
        let by_id: std::collections::HashMap<_, _> = all.iter().map(|r| (r.id, r.status)).collect();
        assert!(matches!(by_id[&ids[0]], OutboundStatus::Sent));
        assert!(matches!(by_id[&ids[1]], OutboundStatus::PermFail));
        assert!(matches!(
            by_id[&ids[2]],
            OutboundStatus::SuppressedBackscatter
        ));
    }

    #[tokio::test]
    async fn replication_tracking() {
        let db = CacheDb::open_in_memory().unwrap();
        let key = [8u8; 32];

        // Not replicated initially
        assert!(!db.is_replicated("post", &key, None).await.unwrap());
        assert_eq!(db.replication_count().await.unwrap(), 0);

        // Mark replicated
        db.mark_replicated("post", &key, None).await.unwrap();
        assert!(db.is_replicated("post", &key, None).await.unwrap());
        assert_eq!(db.replication_count().await.unwrap(), 1);

        // Idempotent (INSERT OR IGNORE)
        db.mark_replicated("post", &key, None).await.unwrap();
        assert_eq!(db.replication_count().await.unwrap(), 1);

        // Different inbox_row_id is a separate entry
        db.mark_replicated("inbox", &key, Some(42)).await.unwrap();
        assert!(db.is_replicated("inbox", &key, Some(42)).await.unwrap());
        assert!(!db.is_replicated("inbox", &key, Some(99)).await.unwrap());
        assert_eq!(db.replication_count().await.unwrap(), 2);

        // Unmark (the paired-replica delete twin) removes exactly one entry and
        // is idempotent — a second unmark of the same key is a clean no-op.
        db.unmark_replicated("post", &key, None).await.unwrap();
        assert!(!db.is_replicated("post", &key, None).await.unwrap());
        assert!(db.is_replicated("inbox", &key, Some(42)).await.unwrap());
        assert_eq!(db.replication_count().await.unwrap(), 1);
        db.unmark_replicated("post", &key, None).await.unwrap();
        assert_eq!(db.replication_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn blob_metadata_crud() {
        let db = CacheDb::open_in_memory().unwrap();
        let hash = [42u8; 32];

        db.put_blob_metadata(&hash, 1024, "blob", None, None)
            .await
            .unwrap();
        let meta = db.get_blob_metadata(&hash).await.unwrap().unwrap();
        assert_eq!(meta.size_bytes, 1024);
        assert_eq!(meta.content_type, "blob");
        assert!(meta.storage_local);
        assert!(!meta.storage_s3);
        assert_eq!(meta.ref_count, 1);

        db.set_blob_s3(&hash, true).await.unwrap();
        let meta = db.get_blob_metadata(&hash).await.unwrap().unwrap();
        assert!(meta.storage_s3);

        db.increment_blob_ref(&hash).await.unwrap();
        let meta = db.get_blob_metadata(&hash).await.unwrap().unwrap();
        assert_eq!(meta.ref_count, 2);

        db.decrement_blob_ref(&hash).await.unwrap();
        let meta = db.get_blob_metadata(&hash).await.unwrap().unwrap();
        assert_eq!(meta.ref_count, 1);

        db.delete_blob_metadata(&hash).await.unwrap();
        assert!(db.get_blob_metadata(&hash).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn blob_metadata_storage_stats() {
        let db = CacheDb::open_in_memory().unwrap();

        db.put_blob_metadata(&[1u8; 32], 100, "chunk", None, None)
            .await
            .unwrap();
        db.put_blob_metadata(&[2u8; 32], 200, "chunk", None, None)
            .await
            .unwrap();
        db.put_blob_metadata(&[3u8; 32], 300, "manifest", None, None)
            .await
            .unwrap();

        let stats = db.blob_storage_stats().await.unwrap();
        assert_eq!(stats.total_blobs, 3);
        assert_eq!(stats.total_bytes, 600);
        assert_eq!(stats.local_blobs, 3);
        assert_eq!(stats.s3_blobs, 0);
    }

    /// Version history = a projection over `sync_changes` (file-sync.md § File
    /// Versions): recorded changes ARE the versions (`version_num` = `seq`);
    /// deletes and superseded rows are not restorable versions; the scope is a
    /// folder-id allowlist.
    #[tokio::test]
    async fn file_versions_project_sync_changes() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [49u8; 32];
        let device = [53u8; 32];
        let path_hash = [50u8; 32];
        let manifest1 = [51u8; 32];
        let manifest2 = [52u8; 32];

        let seq1 = db
            .record_sync_change(
                &actor,
                &path_hash,
                Some(&manifest1),
                1000,
                "create",
                Some(7),
                Some(&device),
                Some("docs/a.txt"),
            )
            .await
            .unwrap();
        let seq2 = db
            .record_sync_change(
                &actor,
                &path_hash,
                Some(&manifest2),
                2000,
                "modify",
                Some(7),
                Some(&device),
                Some("docs/a.txt"),
            )
            .await
            .unwrap();
        // A delete tombstone (no manifest) is not a restorable version.
        db.record_sync_change(
            &actor,
            &path_hash,
            None,
            0,
            "delete",
            Some(7),
            Some(&device),
            Some("docs/a.txt"),
        )
        .await
        .unwrap();
        // Same path_hash in ANOTHER set: outside the scope, invisible.
        db.record_sync_change(
            &actor,
            &path_hash,
            Some(&manifest1),
            1000,
            "create",
            Some(8),
            Some(&device),
            Some("docs/a.txt"),
        )
        .await
        .unwrap();

        let versions = db
            .list_file_versions_in_sets(&[7], &path_hash, false)
            .await
            .unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version_num, seq1);
        assert_eq!(versions[0].size_bytes, 1000);
        assert_eq!(versions[1].version_num, seq2);
        assert_eq!(versions[1].size_bytes, 2000);
        assert_eq!(versions[0].folder_id, 7);

        let v1 = db
            .get_file_version_by_seq(&path_hash, seq1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(v1.manifest_hash, manifest1.to_vec());

        // Empty scope → nothing (the handler's unreadable-set shape).
        assert!(
            db.list_file_versions_in_sets(&[], &path_hash, false)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn sync_device_registration() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = [70u8; 32];
        let device_id = [71u8; 32];

        db.register_sync_device(&actor_id, &device_id, "iPhone", None, "read,write")
            .await
            .unwrap();

        let devices = db.list_sync_devices(&actor_id).await.unwrap();
        assert_eq!(devices.len(), 1);
        // S9 flip: a user-chosen label rests sealed-only — the plaintext
        // column takes the '' sentinel even at register.
        assert_eq!(devices[0].label, "");
        assert_eq!(devices[0].device_id, device_id.to_vec());
    }

    #[tokio::test]
    async fn sync_change_tracking() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = [72u8; 32];
        let path_hash = [73u8; 32];
        let manifest_hash = [74u8; 32];

        let seq = db
            .record_sync_change(
                &actor_id,
                &path_hash,
                Some(&manifest_hash),
                1000,
                "created",
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(seq > 0);

        let changes = db.get_sync_changes(&actor_id, 0).await.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, "created");

        let changes2 = db.get_sync_changes(&actor_id, seq).await.unwrap();
        assert!(changes2.is_empty());
    }

    #[tokio::test]
    async fn contacts_crud() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [80u8; 32];
        let peer = [81u8; 32];

        // No contact initially
        assert!(
            db.get_contact_status(&actor, &peer)
                .await
                .unwrap()
                .is_none()
        );
        let contacts = db.list_contacts(&actor).await.unwrap();
        assert!(contacts.is_empty());

        // Create pending contact
        db.upsert_contact(&actor, &peer, "pending").await.unwrap();
        assert_eq!(
            db.get_contact_status(&actor, &peer)
                .await
                .unwrap()
                .as_deref(),
            Some("pending")
        );

        // Accept the contact
        db.accept_contact(&actor, &peer).await.unwrap();
        assert_eq!(
            db.get_contact_status(&actor, &peer)
                .await
                .unwrap()
                .as_deref(),
            Some("accepted")
        );

        // Promote to confirmed
        db.promote_to_confirmed(&actor, &peer).await.unwrap();
        assert_eq!(
            db.get_contact_status(&actor, &peer)
                .await
                .unwrap()
                .as_deref(),
            Some("confirmed")
        );

        // Block the contact
        db.block_contact(&actor, &peer).await.unwrap();
        assert_eq!(
            db.get_contact_status(&actor, &peer)
                .await
                .unwrap()
                .as_deref(),
            Some("blocked")
        );

        // List contacts
        let contacts = db.list_contacts(&actor).await.unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].1, "blocked");

        // Delete the contact
        db.delete_contact(&actor, &peer).await.unwrap();
        assert!(
            db.get_contact_status(&actor, &peer)
                .await
                .unwrap()
                .is_none()
        );
        let contacts = db.list_contacts(&actor).await.unwrap();
        assert!(contacts.is_empty());
    }

    /// `behavior/notifications.md` § Retention, rule 3: the hourly sweep
    /// deletes the doorbell of every knock it expires — and only those. A
    /// fresh knock's doorbell stays; a doorbell whose knock is already gone
    /// (an accepted request) stays; a non-knock row from the same sender
    /// stays. The expired knock's `pending` contact edge goes with it too
    /// (`ui/contacts.md` § Persistence) — a fresh knock's stays — so the
    /// expired knocker is not refused as still-pending for ever.
    #[tokio::test]
    async fn expire_old_knocks_takes_the_expired_knocks_doorbells_with_them() {
        use crate::db::notifications::NotificationText;
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [87u8; 32];
        let stale = [88u8; 32];
        let fresh = [89u8; 32];
        let accepted = [90u8; 32];

        db.push_knock(&actor, &stale, b"n", "old", &[])
            .await
            .unwrap();
        db.push_knock(&actor, &fresh, b"n", "new", &[])
            .await
            .unwrap();
        // The pending edge `store_knock` writes beside each knock.
        db.upsert_contact(&actor, &stale, "pending").await.unwrap();
        db.upsert_contact(&actor, &fresh, "pending").await.unwrap();
        // The accepted sender's knock row is gone already; only the doorbell
        // remains, as its message's only home.
        for (sender, summary) in [
            (&stale, "stale"),
            (&fresh, "fresh"),
            (&accepted, "accepted"),
        ] {
            db.insert_notification(
                &actor,
                &fauna_protocol::notifications::NotifType::Knock,
                "fauna",
                Some(sender),
                None,
                None,
                &NotificationText::untranslated(summary),
                1000,
            )
            .await
            .unwrap()
            .expect("inserted");
        }
        db.insert_notification(
            &actor,
            &fauna_protocol::notifications::NotifType::Like,
            "fauna",
            Some(&stale),
            Some(&[1u8; 32]),
            None,
            &NotificationText::untranslated("a like from the stale knocker"),
            1000,
        )
        .await
        .unwrap()
        .expect("inserted");
        {
            let conn = db.conn.lock().await;
            let old_time = now_epoch_millis() - 7_200_000; // 2 hours ago
            conn.execute(
                "UPDATE knocks SET created_at = ?1 WHERE actor_id = ?2 AND sender_id = ?3",
                rusqlite::params![old_time, actor.as_slice(), stale.as_slice()],
            )
            .unwrap();
        }

        let expired = db.expire_old_knocks(3600).await.unwrap();
        assert_eq!(expired, 1);

        assert_eq!(
            db.get_contact_status(&actor, &stale).await.unwrap(),
            None,
            "the expired knock's pending edge went with it"
        );
        assert_eq!(
            db.get_contact_status(&actor, &fresh)
                .await
                .unwrap()
                .as_deref(),
            Some("pending"),
            "the fresh knock's pending edge stays"
        );

        let mut left: Vec<String> = db
            .list_notifications(&actor, None, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.summary)
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec!["a like from the stale knocker", "accepted", "fresh"],
            "exactly the expired knock's doorbell went"
        );
    }

    #[tokio::test]
    async fn knocks_crud() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [82u8; 32];
        let sender = [83u8; 32];
        let sender_node = b"https://node.example.com";

        // No knocks initially
        assert!(!db.has_pending_knock(&actor, &sender).await.unwrap());
        let knocks = db.poll_knocks(&actor).await.unwrap();
        assert!(knocks.is_empty());

        // Push a knock
        let id = db
            .push_knock(&actor, &sender, sender_node, "Hello, want to connect?", &[])
            .await
            .unwrap();
        assert!(id > 0);

        // Should now have a pending knock
        assert!(db.has_pending_knock(&actor, &sender).await.unwrap());

        // Poll knocks
        let knocks = db.poll_knocks(&actor).await.unwrap();
        assert_eq!(knocks.len(), 1);
        assert_eq!(knocks[0].id, id);
        assert_eq!(knocks[0].sender_id, sender);
        assert_eq!(knocks[0].summary, "Hello, want to connect?");

        // Dismiss knock
        db.dismiss_knock(&actor, &sender).await.unwrap();
        assert!(!db.has_pending_knock(&actor, &sender).await.unwrap());
        let knocks = db.poll_knocks(&actor).await.unwrap();
        assert!(knocks.is_empty());
    }

    #[tokio::test]
    async fn content_tables_exist() {
        let db = CacheDb::open_in_memory().unwrap();
        // Should not panic — unified tables exist after migrations
        let conn = db.conn.lock().await;
        conn.execute(
            "SELECT id, schema, author, created_at, payload, source FROM content LIMIT 1",
            [],
        )
        .unwrap();
        conn.execute(
            "SELECT content_id, score, has_media, is_reply FROM content_meta LIMIT 1",
            [],
        )
        .unwrap();
        conn.execute("SELECT id, link_type, source_id, target_id, actor_id, status FROM content_links LIMIT 1", []).unwrap();
        conn.execute(
            "SELECT feed_id, owner, name, rules, combination, created_at FROM feeds LIMIT 1",
            [],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn expire_accepted_contacts() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [84u8; 32];
        let old_peer = [85u8; 32];
        let new_peer = [86u8; 32];

        // Accept both contacts
        db.accept_contact(&actor, &old_peer).await.unwrap();
        db.accept_contact(&actor, &new_peer).await.unwrap();

        // Manually backdate old_peer's accepted_at to simulate an old contact
        {
            let conn = db.conn.lock().await;
            let old_time = now_epoch_secs() - 7200; // 2 hours ago
            conn.execute(
                "UPDATE contacts SET accepted_at = ?1 WHERE actor_id = ?2 AND peer_id = ?3",
                rusqlite::params![old_time, actor.as_slice(), old_peer.as_slice()],
            )
            .unwrap();
        }

        // Expire contacts older than 1 hour (3600 secs)
        let expired = db.expire_accepted_contacts(3600).await.unwrap();
        assert_eq!(expired, 1);

        // old_peer should be gone, new_peer should remain
        assert!(
            db.get_contact_status(&actor, &old_peer)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.get_contact_status(&actor, &new_peer)
                .await
                .unwrap()
                .as_deref(),
            Some("accepted")
        );
    }

    #[tokio::test]
    async fn create_folder_with_node_cache_stamps_the_column() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        db.create_folder_with_node_cache("docs", &actor, true)
            .await
            .unwrap();
        db.create_folder("plain", &actor).await.unwrap();
        assert!(db.get_folder("docs").await.unwrap().unwrap().node_cache);
        assert!(!db.get_folder("plain").await.unwrap().unwrap().node_cache);
    }

    #[tokio::test]
    async fn tiers_have_max_feeds() {
        let db = CacheDb::open_in_memory().unwrap();
        let tiers = db.list_tiers().await.unwrap();
        let free = tiers.iter().find(|t| t.name == "free").unwrap();
        assert_eq!(free.max_feeds, 5);
    }

    /// The storage-only `backup` seed tier for held-for-friends backups
    /// (`docs/goal/behavior/backup-destinations.md` § Held-for-friends enrollment). Storage-only =
    /// `max_inbox_bytes == 0` (no MX / inbox / plaintext-readable content) and
    /// `max_feeds == 0` (no feeds); the offered space is a non-zero
    /// `max_storage_bytes`. Guards that the seed sets `max_feeds` to 0 rather
    /// than leaving the column default 5.
    #[tokio::test]
    async fn backup_tier_is_seeded_storage_only() {
        let db = CacheDb::open_in_memory().unwrap();
        let tiers = db.list_tiers().await.unwrap();
        let backup = tiers
            .iter()
            .find(|t| t.name == "backup")
            .expect("backup tier seeded");
        assert_eq!(backup.max_inbox_bytes, 0, "no inbox on the backup tier");
        assert_eq!(backup.max_feeds, 0, "no feeds on the backup tier");
        assert!(
            backup.max_storage_bytes > 0,
            "the backup tier offers storage space"
        );
        assert!(
            backup.max_devices > 0 && backup.max_blob_size > 0,
            "the backup tier can chunk (devices + per-chunk ceiling)"
        );
    }

    #[tokio::test]
    async fn index_post_roundtrip() {
        use fauna_core::data::*;
        use fauna_core::identity::ActorId;

        let db = CacheDb::open_in_memory().unwrap();
        let post_id = [42u8; 32];
        let author = ActorId([1u8; 32]);
        let post = Post {
            author,
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: "I love #cat photos".into(),
                facets: vec![Facet {
                    byte_start: 7,
                    byte_end: 11,
                    feature: FacetFeature::Tag { name: "cat".into() },
                }],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };

        db.index_post(&post_id, &post).await.unwrap();

        // Verify content_meta row
        let conn = db.conn.lock().await;
        let (got_media, got_reply): (i64, i64) = conn
            .query_row(
                "SELECT has_media, is_reply FROM content_meta WHERE content_id = ?1",
                rusqlite::params![post_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(got_media, 0);
        assert_eq!(got_reply, 0);

        // Verify tag in content_links
        let tag: String = conn
            .query_row(
                "SELECT status FROM content_links WHERE source_id = ?1 AND link_type = 'tag'",
                rusqlite::params![post_id.as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tag, "cat");
    }

    #[tokio::test]
    async fn put_post_then_check_index() {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;

        let db = CacheDb::open_in_memory().unwrap();
        let post = Post {
            author: ActorId([7u8; 32]),
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: "hello #cat".into(),
                facets: vec![Facet {
                    byte_start: 6,
                    byte_end: 10,
                    feature: FacetFeature::Tag { name: "cat".into() },
                }],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let data = canonical_encode(&post).unwrap();
        let hash = blake3::hash(&data);
        let post_id: [u8; 32] = *hash.as_bytes();

        db.put_post(&post_id, &data, None).await.unwrap();

        // content_meta should be populated now
        let conn = db.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM content_meta WHERE content_id = ?1",
                rusqlite::params![post_id.as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // content table should also have the row
        let count2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM content WHERE id = ?1",
                rusqlite::params![post_id.as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count2, 1);
    }

    #[tokio::test]
    async fn put_post_with_source_indexes_with_override() {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;
        use fauna_core::scoring::{FilterCombination, FilterRule};

        let db = CacheDb::open_in_memory().unwrap();
        let post = Post {
            author: ActorId([8u8; 32]),
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: "bluesky imported post".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let data = canonical_encode(&post).unwrap();
        let hash = blake3::hash(&data);
        let post_id: [u8; 32] = *hash.as_bytes();

        db.put_post_with_source(&post_id, &data, "bluesky")
            .await
            .unwrap();

        // Query with Source filter for bluesky — should find it
        let rules = vec![FilterRule::Source {
            protocols: vec!["bluesky".into()],
        }];
        let results = db
            .query_feed(&rules, FilterCombination::All, &[], None, 50)
            .await
            .unwrap();
        assert_eq!(results.len(), 1, "expected 1 bluesky post");
        assert_eq!(results[0].source, "bluesky");

        // Query with Source filter for fauna — should NOT find it
        let rules_fauna = vec![FilterRule::Source {
            protocols: vec!["fauna".into()],
        }];
        let results_fauna = db
            .query_feed(&rules_fauna, FilterCombination::All, &[], None, 50)
            .await
            .unwrap();
        assert_eq!(
            results_fauna.len(),
            0,
            "bluesky post should not appear in fauna source filter"
        );
    }

    /// `content_meta.source` comes from the post itself when it carries an
    /// origin (`archive-import.md` § Compatibility — an older nest keeps
    /// indexing `"fauna"`; this one names the platform so the badge can).
    #[test]
    fn extract_post_metadata_indexes_the_origin_platform_as_source() {
        use fauna_core::data::*;
        use fauna_core::identity::ActorId;
        let mut post = Post {
            author: ActorId([8u8; 32]),
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: "re-authored from an export".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        assert_eq!(extract_post_metadata(&post).source, "fauna");

        post.origin = Some(PostOrigin {
            platform: " Facebook ".into(),
            url: None,
        });
        assert_eq!(extract_post_metadata(&post).source, "facebook");

        // A token `source::normalize` refuses never reaches the index.
        post.origin.as_mut().unwrap().platform = "fauna, bluesky".into();
        assert_eq!(extract_post_metadata(&post).source, "fauna");

        // A bridge token is never reachable through a client-authored origin
        // either — the vocabulary is closed to `source::ARCHIVE_PLATFORMS`,
        // so a post can't be indexed under "bluesky" and routed into a
        // bridge interact arm it has no bridge record for.
        post.origin.as_mut().unwrap().platform = "bluesky".into();
        assert_eq!(extract_post_metadata(&post).source, "fauna");
    }

    #[tokio::test]
    async fn feed_crud() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [10u8; 32];
        let rules = b"some bare bytes".to_vec();

        // Create
        let feed_id = db
            .create_feed(&owner, "cats", &rules, "all", "local", "[]", None)
            .await
            .unwrap();
        assert!(!feed_id.is_empty());

        // Get
        let feed = db.get_feed(&feed_id).await.unwrap().unwrap();
        assert_eq!(feed.name, "cats");
        assert_eq!(feed.rules, rules);
        assert_eq!(feed.combination, "all");
        assert_eq!(feed.owner, owner.to_vec());

        // List by owner
        let feeds = db.list_feeds_by_owner(&owner).await.unwrap();
        assert_eq!(feeds.len(), 1);

        // List all
        let all = db.list_feeds().await.unwrap();
        assert_eq!(all.len(), 1);

        // Update
        let new_rules = b"updated rules".to_vec();
        db.update_feed(&feed_id, &owner, "cats2", &new_rules, "any", None)
            .await
            .unwrap();
        let updated = db.get_feed(&feed_id).await.unwrap().unwrap();
        assert_eq!(updated.name, "cats2");
        assert_eq!(updated.combination, "any");

        // Delete
        db.delete_feed(&feed_id, &owner).await.unwrap();
        assert!(db.get_feed(&feed_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn query_feed_by_hashtag() {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;
        use fauna_core::scoring::{FilterCombination, FilterRule};

        let db = CacheDb::open_in_memory().unwrap();

        // Insert two posts: one with #cat, one with #dog
        let make_post = |tag: &str, ts: u64| {
            let post = Post {
                author: ActorId([1u8; 32]),
                created_at: Timestamp(ts),
                body: PostBody::Text {
                    content: format!("hello #{tag}"),
                    facets: vec![Facet {
                        byte_start: 6,
                        byte_end: 6 + tag.len() as u32 + 1,
                        feature: FacetFeature::Tag { name: tag.into() },
                    }],
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin: None,
            };
            let data = canonical_encode(&post).unwrap();
            let hash = blake3::hash(&data);
            (*hash.as_bytes(), data)
        };

        let (id1, data1) = make_post("cat", 2_000_000);
        let (id2, data2) = make_post("dog", 1_000_000);
        db.put_post(&id1, &data1, None).await.unwrap();
        db.put_post(&id2, &data2, None).await.unwrap();

        // Query for #cat
        let rules = vec![FilterRule::HasHashtag {
            tags: vec!["cat".into()],
        }];
        let results = db
            .query_feed(&rules, FilterCombination::All, &[], None, 50)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].post_id, id1.to_vec());
    }

    /// A hashtag rule combines with a feed's OTHER rules under both modes
    /// (`feed.md` § Feed-rule types: `All` — every rule must match — or `Any` —
    /// at least one). It used to inner-join the post's tag links, which broke
    /// both: under `Any` every UNTAGGED post fell out before the OR was even
    /// read, so a post matching only the word rule never showed; and under
    /// `All` two hashtag rules were tested against the same joined tag row, so a
    /// post carrying both tags could never match.
    #[tokio::test]
    async fn query_feed_hashtag_rule_combines_with_other_rules() {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;
        use fauna_core::scoring::{FilterCombination, FilterRule};

        let db = CacheDb::open_in_memory().unwrap();
        let mut ts = 1_000_000u64;
        let mut put = |content: &str, tags: &[&str]| {
            ts += 1_000_000;
            let facets = tags
                .iter()
                .map(|tag| {
                    let start = content.find(&format!("#{tag}")).expect("tag in content") as u32;
                    Facet {
                        byte_start: start,
                        byte_end: start + tag.len() as u32 + 1,
                        feature: FacetFeature::Tag {
                            name: (*tag).into(),
                        },
                    }
                })
                .collect();
            let post = Post {
                author: ActorId([1u8; 32]),
                created_at: Timestamp(ts),
                body: PostBody::Text {
                    content: content.into(),
                    facets,
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin: None,
            };
            let data = canonical_encode(&post).unwrap();
            let id: [u8; 32] = *blake3::hash(&data).as_bytes();
            (id, data)
        };
        let tagged_only = put("hello #cat", &["cat"]);
        let word_only = put("plain rust post", &[]);
        let both = put("hello #cat and rust", &["cat"]);
        let neither = put("nothing to see", &[]);
        let two_tags = put("hello #cat #dog", &["cat", "dog"]);
        for (id, data) in [&tagged_only, &word_only, &both, &neither, &two_tags] {
            db.put_post(id, data, None).await.unwrap();
        }
        let ids = |rows: Vec<FeedPostRow>| -> std::collections::BTreeSet<Vec<u8>> {
            rows.into_iter().map(|r| r.post_id).collect()
        };
        let set = |posts: &[&([u8; 32], Vec<u8>)]| -> std::collections::BTreeSet<Vec<u8>> {
            posts.iter().map(|(id, _)| id.to_vec()).collect()
        };
        let cat = FilterRule::HasHashtag {
            tags: vec!["cat".into()],
        };
        let rust = FilterRule::BodyContains {
            terms: vec!["rust".into()],
        };

        let any = db
            .query_feed(
                &[cat.clone(), rust.clone()],
                FilterCombination::Any,
                &[],
                None,
                50,
            )
            .await
            .unwrap();
        assert_eq!(
            ids(any),
            set(&[&tagged_only, &word_only, &both, &two_tags]),
            "Any: every post matching either rule, the untagged word post included"
        );

        let all = db
            .query_feed(&[cat.clone(), rust], FilterCombination::All, &[], None, 50)
            .await
            .unwrap();
        assert_eq!(ids(all), set(&[&both]), "All: only the post matching both");

        let both_tags = db
            .query_feed(
                &[
                    cat,
                    FilterRule::HasHashtag {
                        tags: vec!["dog".into()],
                    },
                ],
                FilterCombination::All,
                &[],
                None,
                50,
            )
            .await
            .unwrap();
        assert_eq!(
            ids(both_tags),
            set(&[&two_tags]),
            "All over two hashtag rules: the post carrying both tags"
        );
    }

    #[tokio::test]
    async fn feed_end_to_end() {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;
        use fauna_core::scoring::{FilterCombination, FilterRule};

        let db = CacheDb::open_in_memory().unwrap();
        let owner = [10u8; 32];

        // Create 3 posts: two with #cat, one with #dog
        let make_post = |tag: &str, ts: u64, author: [u8; 32]| {
            let post = Post {
                author: ActorId(author),
                created_at: Timestamp(ts),
                body: PostBody::Text {
                    content: format!("hello #{tag}"),
                    facets: vec![Facet {
                        byte_start: 6,
                        byte_end: 6 + tag.len() as u32 + 1,
                        feature: FacetFeature::Tag { name: tag.into() },
                    }],
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin: None,
            };
            let data = canonical_encode(&post).unwrap();
            let hash = blake3::hash(&data);
            (*hash.as_bytes(), data)
        };

        let (id1, data1) = make_post("cat", 3_000_000, [1u8; 32]);
        let (id2, data2) = make_post("cat", 2_000_000, [2u8; 32]);
        let (id3, data3) = make_post("dog", 1_000_000, [3u8; 32]);
        db.put_post(&id1, &data1, None).await.unwrap();
        db.put_post(&id2, &data2, None).await.unwrap();
        db.put_post(&id3, &data3, None).await.unwrap();

        // Create a "cats" feed
        let rules = vec![FilterRule::HasHashtag {
            tags: vec!["cat".into()],
        }];
        let rules_bytes = canonical_encode(&rules).unwrap();
        let feed_id = db
            .create_feed(&owner, "cats", &rules_bytes, "all", "local", "[]", None)
            .await
            .unwrap();

        // Query the feed
        let feed = db.get_feed(&feed_id).await.unwrap().unwrap();
        let parsed_rules: Vec<FilterRule> =
            fauna_core::encoding::canonical_decode(&feed.rules).unwrap();
        let results = db
            .query_feed(&parsed_rules, FilterCombination::All, &[], None, 50)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        // Newest first
        assert_eq!(results[0].created_at, 3_000_000);
        assert_eq!(results[1].created_at, 2_000_000);
        // Both have "cat" tag
        assert!(results[0].tags.contains(&"cat".to_string()));
        assert!(results[1].tags.contains(&"cat".to_string()));

        // Cursor pagination: get posts older than the first
        let results2 = db
            .query_feed(
                &parsed_rules,
                FilterCombination::All,
                &[],
                Some(3_000_000),
                50,
            )
            .await
            .unwrap();
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].created_at, 2_000_000);
    }

    #[tokio::test]
    async fn query_feed_with_body_contains() {
        use fauna_core::data::*;
        use fauna_core::identity::ActorId;
        use fauna_core::scoring::{FilterCombination, FilterRule};

        let db = CacheDb::open_in_memory().unwrap();
        let author = ActorId([8u8; 32]);

        // Insert two posts: one about rust, one about python
        for (text, id_byte) in [
            ("I love rust programming", 1u8),
            ("I love python scripting", 2u8),
        ] {
            let post = Post {
                author,
                created_at: Timestamp(1_700_000_000_000_000 + id_byte as u64),
                body: PostBody::Text {
                    content: text.to_string(),
                    facets: vec![],
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin: None,
            };
            let data = fauna_core::encoding::canonical_encode(&post).unwrap();
            let hash = blake3::hash(&data);
            let post_id: [u8; 32] = *hash.as_bytes();
            db.put_post(&post_id, &data, None).await.unwrap();
        }

        // BodyContains "rust" should find 1 post
        let results = db
            .query_feed(
                &[FilterRule::BodyContains {
                    terms: vec!["rust".into()],
                }],
                FilterCombination::All,
                &[],
                None,
                10,
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        // BodyExcludes "rust" should find 1 post (the python one)
        let results = db
            .query_feed(
                &[FilterRule::BodyExcludes {
                    terms: vec!["rust".into()],
                }],
                FilterCombination::All,
                &[],
                None,
                10,
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        // MANDATORY group vs an `Any` rule set: a catch-all `Any` feed (its own
        // rule matches every post) must still be narrowed by a mandatory
        // BodyContains — the search filter is a constraint, never one of the
        // feed's OR-alternatives. Pre-fix, pushing search into the rules under
        // `Any` returned every post (the test_feed_search_filters_posts apple
        // red: the default e2e feed is empty-rules + any).
        let results = db
            .query_feed(
                &[FilterRule::HasMedia { required: false }],
                FilterCombination::Any,
                &[FilterRule::BodyContains {
                    terms: vec!["rust".into()],
                }],
                None,
                10,
            )
            .await
            .unwrap();
        assert_eq!(
            results.len(),
            1,
            "mandatory search must AND with an Any rule group"
        );
    }

    #[tokio::test]
    async fn query_feed_by_source() {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;
        use fauna_core::scoring::{FilterCombination, FilterRule};

        let db = CacheDb::open_in_memory().unwrap();
        let author: [u8; 32] = [10; 32];

        // Create two posts: one fauna, one bluesky
        let fauna_post = Post {
            author: ActorId(author),
            created_at: Timestamp(2_000_000),
            body: PostBody::Text {
                content: "fauna post".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let fauna_data = canonical_encode(&fauna_post).unwrap();
        let fauna_post_id: [u8; 32] = *blake3::hash(&fauna_data).as_bytes();
        db.put_post(&fauna_post_id, &fauna_data, None)
            .await
            .unwrap();

        let bluesky_post = Post {
            author: ActorId(author),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "bluesky post".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let bluesky_data = canonical_encode(&bluesky_post).unwrap();
        let bluesky_post_id: [u8; 32] = *blake3::hash(&bluesky_data).as_bytes();
        db.put_post_with_source(&bluesky_post_id, &bluesky_data, "bluesky")
            .await
            .unwrap();

        // Query for bluesky-only posts
        let rules = vec![FilterRule::Source {
            protocols: vec!["bluesky".into()],
        }];
        let results = db
            .query_feed(&rules, FilterCombination::All, &[], None, 50)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].post_id, bluesky_post_id.to_vec());
        assert_eq!(results[0].source, "bluesky");

        // Query for fauna-only posts
        let rules = vec![FilterRule::Source {
            protocols: vec!["fauna".into()],
        }];
        let results = db
            .query_feed(&rules, FilterCombination::All, &[], None, 50)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].post_id, fauna_post_id.to_vec());
        assert_eq!(results[0].source, "fauna");

        // Query for both sources
        let rules = vec![FilterRule::Source {
            protocols: vec!["fauna".into(), "bluesky".into()],
        }];
        let results = db
            .query_feed(&rules, FilterCombination::All, &[], None, 50)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_list_inbox_all() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [1; 32];
        db.create_user(&actor, "free", "test").await.unwrap();
        db.push_inbox(&actor, b"msg1", None).await.unwrap();
        db.push_inbox(&actor, b"msg2", None).await.unwrap();
        let msgs = db.poll_inbox(&actor).await.unwrap();
        db.ack_inbox(&actor, &[msgs[0].0]).await.unwrap();
        let all = db.list_inbox_all(&actor).await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].4); // delivered
        assert!(!all[1].4); // not delivered
    }

    #[tokio::test]
    async fn test_list_contacts_full() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [1; 32];
        let peer: [u8; 32] = [2; 32];
        db.create_user(&actor, "free", "test").await.unwrap();
        db.upsert_contact(&actor, &peer, "confirmed").await.unwrap();
        let contacts = db.list_contacts_full(&actor).await.unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].status, "confirmed");
        assert_eq!(contacts[0].peer_id, peer.to_vec());
    }

    #[tokio::test]
    async fn test_list_posts_by_author() {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;

        let db = CacheDb::open_in_memory().unwrap();
        let author: [u8; 32] = [1; 32];
        let post = Post {
            author: ActorId(author),
            created_at: Timestamp(1000),
            body: PostBody::Text {
                content: "test post".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let data = canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
        db.put_post(&post_id, &data, None).await.unwrap();

        let ids = db.list_posts_by_author(&author).await.unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], post_id.to_vec());
    }

    #[tokio::test]
    async fn test_list_key_packages_for_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [1; 32];
        db.create_user(&actor, "free", "test").await.unwrap();
        let now = fauna_core::data::Timestamp::now_secs() as u64;
        db.put_key_package("kp1", &actor, b"data1", now, now + 3600)
            .await
            .unwrap();
        db.put_key_package("kp2", &actor, b"data2", now, now + 3600)
            .await
            .unwrap();
        let packages = db.list_key_packages_for_actor(&actor).await.unwrap();
        assert_eq!(packages.len(), 2);
    }

    #[tokio::test]
    async fn test_get_folders_for_actor_full() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [1; 32];
        db.create_user(&actor, "free", "test").await.unwrap();
        db.create_folder("photos", &actor).await.unwrap();
        let sets = db.get_folders_for_actor_full(&actor).await.unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].name, "photos");
    }

    /// The backups projection (`get_folders_for_actor`, behind
    /// `fauna.sync.backup_status`) must exclude reserved `__<kind>` internal
    /// sync sets — they are never user backup targets and, having zero
    /// snapshots, would poison the backups dropdown if they leaked through.
    #[tokio::test]
    async fn get_folders_for_actor_excludes_reserved_sets() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [1; 32];
        db.create_user(&actor, "free", "test").await.unwrap();

        // One real user backup set …
        db.create_folder("documents-abc", &actor).await.unwrap();
        // … alongside reserved internal sets the actor accumulates from
        // config/index/mail writes.
        db.get_or_create_reserved_folder(&actor, "config")
            .await
            .unwrap();
        db.get_or_create_reserved_folder(&actor, "index")
            .await
            .unwrap();
        db.get_or_create_reserved_folder(&actor, "mail")
            .await
            .unwrap();

        let sets = db.get_folders_for_actor(&actor).await.unwrap();
        let names: Vec<&str> = sets.iter().map(|(l, _)| l.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["documents-abc"],
            "backups projection must return only the user set, not reserved __ sets"
        );
    }

    #[tokio::test]
    async fn snapshot_enhanced_fields_roundtrip() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = [1u8; 32];
        let device_id = [2u8; 32];
        db.create_folder("test-fs", &actor_id).await.unwrap();
        let fs = db.get_folder("test-fs").await.unwrap().unwrap();

        // Record a file change with metadata
        let path_hash = blake3::hash(b"photos/cat.jpg");
        let manifest_hash = [0xABu8; 32];
        db.record_sync_change(
            &actor_id,
            path_hash.as_bytes(),
            Some(&manifest_hash),
            1024,
            "create",
            Some(fs.id),
            Some(&device_id),
            Some("photos/cat.jpg"),
        )
        .await
        .unwrap();

        let snap = db
            .create_snapshot_v2(
                fs.id,
                Some(&device_id),
                &["before-upgrade".to_string()],
                None, // no seal
                None, // no parent
            )
            .await
            .unwrap();

        assert_eq!(snap.device_id, Some(device_id.to_vec()));
        // The tags never rest: `tag_hashes` carries the retention key,
        // `tags_sealed` (absent here — keyless writer) the display copy.
        assert!(snap.tag_hashes.is_some());
        assert_eq!(snap.tags_sealed, None);
        assert_eq!(snap.parent_id, None);

        // Verify snapshot files have metadata fields
        let files = db.get_snapshot_files(snap.id).await.unwrap();
        assert!(!files.is_empty());
        assert_eq!(files[0].file_type, "regular");
    }

    #[tokio::test]
    async fn delete_snapshot_removes_files() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = [1u8; 32];
        db.create_folder("fs1", &actor_id).await.unwrap();
        let fs = db.get_folder("fs1").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"a.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&[0xABu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("a.txt"),
        )
        .await
        .unwrap();

        let snap = db.create_snapshot(fs.id).await.unwrap();
        assert_eq!(snap.file_count, 1);

        db.delete_snapshots(&[snap.id]).await.unwrap();
        assert!(db.get_snapshot(snap.id).await.unwrap().is_none());
        assert!(db.get_snapshot_files(snap.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn collect_referenced_manifest_hashes() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = [1u8; 32];
        db.create_folder("fs2", &actor_id).await.unwrap();
        let fs = db.get_folder("fs2").await.unwrap().unwrap();

        let hash_a = [0xAAu8; 32];
        let hash_b = [0xBBu8; 32];
        let ph1: [u8; 32] = *blake3::hash(b"a.txt").as_bytes();
        let ph2: [u8; 32] = *blake3::hash(b"b.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph1,
            Some(&hash_a),
            100,
            "create",
            Some(fs.id),
            None,
            Some("a.txt"),
        )
        .await
        .unwrap();
        db.record_sync_change(
            &actor_id,
            &ph2,
            Some(&hash_b),
            200,
            "create",
            Some(fs.id),
            None,
            Some("b.txt"),
        )
        .await
        .unwrap();

        let snap = db.create_snapshot(fs.id).await.unwrap();
        let hashes = db.snapshot_manifest_hashes(&[snap.id]).await.unwrap();
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains(&hash_a.to_vec()));
        assert!(hashes.contains(&hash_b.to_vec()));
    }

    #[tokio::test]
    async fn gc_lock_acquire_release() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.try_acquire_op_lock("gc", -1, "test-holder")
                .await
                .unwrap()
        );
        // Second acquire should fail
        assert!(
            !db.try_acquire_op_lock("gc", -1, "other-holder")
                .await
                .unwrap()
        );
        db.release_op_lock("gc", -1).await.unwrap();
        // Now should succeed
        assert!(
            db.try_acquire_op_lock("gc", -1, "other-holder")
                .await
                .unwrap()
        );
        db.release_op_lock("gc", -1).await.unwrap();
    }

    #[tokio::test]
    async fn list_all_blob_hashes_returns_stored() {
        let db = CacheDb::open_in_memory().unwrap();
        let hash1 = [1u8; 32];
        let hash2 = [2u8; 32];
        db.put_blob_metadata(&hash1, 100, "chunk", None, None)
            .await
            .unwrap();
        db.put_blob_metadata(&hash2, 200, "manifest", None, None)
            .await
            .unwrap();

        let all = db.list_all_blob_hashes().await.unwrap();
        assert!(all.len() >= 2);
        assert!(all.contains(&hash1.to_vec()));
        assert!(all.contains(&hash2.to_vec()));
    }

    #[tokio::test]
    async fn snapshot_manifest_hashes_empty_input() {
        let db = CacheDb::open_in_memory().unwrap();
        let result = db.snapshot_manifest_hashes(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn search_index_tables_exist() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.lock().await;
        // FTS5 virtual table exists and is queryable
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM content_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        // Companion map table exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM content_fts_map", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn index_and_remove_document() {
        let db = CacheDb::open_in_memory().unwrap();

        // Index a document
        db.index_document(
            "post",
            "abc123",
            "My Title",
            "hello world body text",
            "Alice",
            "rust wasm",
            1000,
        )
        .await
        .unwrap();

        // Verify it's in the FTS index
        let results = db
            .search_fts("hello", None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content_type, "post");
        // content_id is the hex of blake3("post:abc123") — **lowercase**, like
        // `fauna_core::hex32::encode` and every other 32-byte-id surface. The
        // query selects `lower(hex(...))` precisely so it does not disagree
        // with them (`db/fts.rs` module doc); this used to expect SQLite's
        // uppercase `hex()`.
        let expected_id = hex::encode(content_id_for_document("post", "abc123"));
        assert_eq!(results[0].content_id, expected_id);

        // Remove it
        db.remove_document("post", "abc123").await.unwrap();

        // Verify it's gone
        let results = db
            .search_fts("hello", None, None, None, 10, 0)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn put_post_indexes_for_search() {
        use fauna_core::data::*;
        use fauna_core::identity::ActorId;

        let db = CacheDb::open_in_memory().unwrap();
        let author = ActorId([7u8; 32]);
        let post = Post {
            author,
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: "Hello world this is a searchable post about rust programming".to_string(),
                facets: vec![Facet {
                    byte_start: 0,
                    byte_end: 4,
                    feature: FacetFeature::Tag {
                        name: "rust".to_string(),
                    },
                }],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let data = fauna_core::encoding::canonical_encode(&post).unwrap();
        let hash = blake3::hash(&data);
        let post_id: [u8; 32] = *hash.as_bytes();

        db.put_post(&post_id, &data, None).await.unwrap();

        // Should be findable via FTS
        let results = db
            .search_fts("searchable", None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content_type, "post/text");
        // A post row's `content_id` IS the post id, spelled exactly as
        // `fauna.posts.create` hands it back (lowercase hex) — the ratified
        // contract in `ui/search.md` § The page's wire surface, pinned
        // end-to-end by `conformance_search.rs`.
        assert_eq!(results[0].content_id, hex::encode(post_id));

        // Should also be findable by tag
        let results = db
            .search_fts("rust", None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn index_document_is_idempotent() {
        let db = CacheDb::open_in_memory().unwrap();

        db.index_document("post", "abc123", "", "first version", "", "", 1000)
            .await
            .unwrap();
        db.index_document("post", "abc123", "", "second version", "", "", 2000)
            .await
            .unwrap();

        // Should find only the second version
        let results = db
            .search_fts("second", None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        let results = db
            .search_fts("first", None, None, None, 10, 0)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    /// A hit's snippet is the text that MATCHED, whichever column that was
    /// (`docs/goal/ui/search.md` § State & data shape — a row carries "the
    /// cleaned snippet"). Every account admitted under a handle is indexed with
    /// its handle as the name and an EMPTY bio (`admin_ws_handlers.rs`,
    /// `account_core.rs`), so a snippet pinned to the body column painted every
    /// such profile row blank — found by the Search page's "each result shows a
    /// snippet of the text that matched" witness.
    #[tokio::test]
    async fn a_hit_snippets_the_column_that_matched() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [13u8; 32];
        db.index_profile(&actor, "lovelace", "").await.unwrap();

        let results = db
            .search_fts("lovelace", None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0].snippet.contains("lovelace"),
            "a profile matched by its name must snippet the name, got {:?}",
            results[0].snippet
        );

        // The scoped path is the one `fauna.search.query` serves.
        let results = db
            .search_with_scoping("lovelace", &actor, None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0].snippet.contains("lovelace"),
            "the scoped search must snippet the matched name too, got {:?}",
            results[0].snippet
        );
    }

    /// The filter names a CLASS, and a post's class is spelled with its body
    /// subtype (`post_body_schema`: `post/text`, `post/media`, …), so the
    /// Search page's `post` filter must reach every subtype — the client reads
    /// `post/*` as a post on its own side (`fauna_client_search::kind`). An
    /// exact `schema = 'post'` match found no post at all: narrowing a search
    /// to posts emptied the page on every app, found by the type-filter
    /// witness. A full subtype (`post/text`) still narrows to itself alone.
    #[tokio::test]
    async fn a_post_filter_finds_every_post_subtype_and_nothing_else() {
        use fauna_core::data::*;
        use fauna_core::identity::ActorId;

        let db = CacheDb::open_in_memory().unwrap();
        let actor = [14u8; 32];
        let post = Post {
            author: ActorId(actor),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "ferrywharf timetable".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let data = fauna_core::encoding::canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
        db.put_post(&post_id, &data, None).await.unwrap();
        db.index_profile(&[15u8; 32], "ferrywharf", "")
            .await
            .unwrap();

        for (label, results) in [
            (
                "search_fts",
                db.search_fts("ferrywharf", Some("post"), None, None, 10, 0)
                    .await
                    .unwrap(),
            ),
            (
                "search_with_scoping",
                db.search_with_scoping("ferrywharf", &actor, Some("post"), None, None, 10, 0)
                    .await
                    .unwrap(),
            ),
        ] {
            let types: Vec<&str> = results.iter().map(|r| r.content_type.as_str()).collect();
            assert_eq!(
                types,
                ["post/text"],
                "{label}: the post filter must find the text post only"
            );
        }

        let exact = db
            .search_with_scoping("ferrywharf", &actor, Some("post/media"), None, None, 10, 0)
            .await
            .unwrap();
        let exact: Vec<&str> = exact.iter().map(|r| r.content_type.as_str()).collect();
        assert!(
            exact.is_empty(),
            "a full subtype narrows to itself: {exact:?}"
        );
    }

    #[tokio::test]
    async fn profile_indexed_for_search() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [12u8; 32];

        db.index_profile(
            &actor,
            "Alice Wonderland",
            "Rust developer and cat enthusiast",
        )
        .await
        .unwrap();

        let results = db
            .search_fts("alice", None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content_type, "profile");

        let results = db
            .search_fts("cat enthusiast", None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        // Update profile re-indexes
        db.index_profile(&actor, "Alice Updated", "New bio")
            .await
            .unwrap();
        let results = db
            .search_fts("alice", None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        // Type filter works
        let results = db
            .search_fts("alice", Some("post"), None, None, 10, 0)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn bridge_messages_crud() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];

        let id = db
            .insert_bridge_message(
                "imap",
                &actor,
                "msg-001",
                "alice@example.com",
                "bob@example.com",
                "Hello Subject",
                1024,
                0,
                1_700_000_000,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(id > 0);

        // Duplicate external_id should upsert
        let id2 = db
            .insert_bridge_message(
                "imap",
                &actor,
                "msg-001",
                "alice@example.com",
                "bob@example.com",
                "Updated Subject",
                1024,
                0,
                1_700_000_000,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(id2 > 0);

        // List bridge messages for actor
        let msgs = db
            .list_bridge_messages("imap", &actor, None, 10)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].subject, "Updated Subject");

        // Delete
        db.delete_bridge_message("imap", &actor, "msg-001")
            .await
            .unwrap();
        let msgs = db
            .list_bridge_messages("imap", &actor, None, 10)
            .await
            .unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn search_scoping_hides_other_users_bridge_messages() {
        let db = CacheDb::open_in_memory().unwrap();
        let alice = [10u8; 32];
        let bob = [11u8; 32];

        // Alice's email
        db.insert_bridge_message(
            "imap",
            &alice,
            "msg-a1",
            "sender@x.test",
            "alice@x.test",
            "Alice Invoice",
            100,
            0,
            1000,
            Some("payment due"),
            None,
        )
        .await
        .unwrap();

        // Bob's email
        db.insert_bridge_message(
            "imap",
            &bob,
            "msg-b1",
            "sender@x.test",
            "bob@x.test",
            "Bob Invoice",
            100,
            0,
            1000,
            Some("payment due"),
            None,
        )
        .await
        .unwrap();

        // A public post
        db.index_document(
            "post",
            "post001",
            "",
            "public post about invoices",
            "",
            "",
            1000,
        )
        .await
        .unwrap();

        // Alice searching "invoice" should see her email + the post, but NOT Bob's email
        let results = db
            .search_with_scoping("invoice", &alice, None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        let types: Vec<&str> = results.iter().map(|r| r.content_type.as_str()).collect();
        assert!(types.contains(&"post"));
        assert!(types.contains(&"imap"));

        // Bob searching should see his email + the post
        let results = db
            .search_with_scoping("invoice", &bob, None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn sanitize_fts_query_basic() {
        assert_eq!(
            sanitize_fts_query("hello world"),
            Some("\"hello\" \"world\"".into())
        );
    }

    #[test]
    fn sanitize_fts_query_special_chars() {
        // FTS5 operators should be escaped
        assert_eq!(
            sanitize_fts_query("NOT bad"),
            Some("\"NOT\" \"bad\"".into())
        );
        assert_eq!(sanitize_fts_query("foo*"), Some("\"foo*\"".into()));
        assert_eq!(
            sanitize_fts_query("col:value"),
            Some("\"col:value\"".into())
        );
    }

    #[test]
    fn sanitize_fts_query_raw_passthrough() {
        assert_eq!(
            sanitize_fts_query("raw:hello AND world"),
            Some("hello AND world".into())
        );
    }

    #[test]
    fn sanitize_fts_query_quotes() {
        assert_eq!(
            sanitize_fts_query(r#"say "hello""#),
            Some(r#""say" """hello""""#.into())
        );
    }

    #[test]
    fn sanitize_fts_query_empty() {
        assert_eq!(sanitize_fts_query(""), None);
        assert_eq!(sanitize_fts_query("   "), None);
        assert_eq!(sanitize_fts_query("raw:"), None);
        assert_eq!(sanitize_fts_query("raw:  "), None);
    }

    #[tokio::test]
    async fn search_everything_integration() {
        use fauna_core::data::*;
        use fauna_core::identity::ActorId;

        let db = CacheDb::open_in_memory().unwrap();
        let actor = [13u8; 32];

        // 1. Index a post via put_post
        let post = Post {
            author: ActorId(actor),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: "Fauna is a distributed social protocol".to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let data = fauna_core::encoding::canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
        db.put_post(&post_id, &data, None).await.unwrap();

        // 2. Index a profile
        db.index_document(
            "profile",
            &hex::encode(actor),
            "Fauna User",
            "Building the future of social media",
            "",
            "",
            1_000_000,
        )
        .await
        .unwrap();

        // 3. Index an email via bridge_messages
        db.insert_bridge_message(
            "imap",
            &actor,
            "email-1",
            "alice@fauna.social",
            "user@fauna.social",
            "Welcome to Fauna",
            500,
            0,
            1_000_000,
            Some("Welcome aboard the fauna network"),
            None,
        )
        .await
        .unwrap();

        // Search "fauna" should find all three (with scoping for the actor)
        let results = db
            .search_with_scoping("fauna", &actor, None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 3);

        // Filter by type — post schema is now "post/text"
        let posts = db
            .search_with_scoping("fauna", &actor, Some("post/text"), None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(posts.len(), 1);

        let emails = db
            .search_with_scoping("fauna", &actor, Some("imap"), None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(emails.len(), 1);

        let profiles = db
            .search_with_scoping("fauna", &actor, Some("profile"), None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(profiles.len(), 1);

        // Different user can't see the email
        let other = [14u8; 32];
        let results = db
            .search_with_scoping("fauna", &other, None, None, None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 2); // post + profile only
    }

    /// Store a text post by `author` (optionally re-authored from an archive
    /// platform) through the indexing writer, returning its id.
    async fn seed_searchable_post(
        db: &CacheDb,
        author: [u8; 32],
        text: &str,
        platform: Option<&str>,
    ) -> [u8; 32] {
        use fauna_core::data::*;
        let post = Post {
            author: fauna_core::identity::ActorId(author),
            created_at: Timestamp(1_000_000),
            body: PostBody::Text {
                content: text.to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: platform.map(|p| PostOrigin {
                platform: p.to_string(),
                url: None,
            }),
        };
        let data = fauna_core::encoding::canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
        db.put_post(&post_id, &data, None).await.unwrap();
        post_id
    }

    /// **Search never quotes a flagged post's body** (`moderation.md` § Legal
    /// takedown → *Posts*). Every hit carries a snippet of the indexed body, so
    /// search is a serve path for post content — to any signed-in caller — and
    /// until 2026-09-10 it applied no moderation flag at all: a word from a
    /// taken-down post found it and quoted it back.
    ///
    /// One post per arm, each asserted on its own, so reverting any one arm of
    /// the gate reddens exactly its own line: the takedown, quarantine and
    /// suppression arms of the public phase, and the takedown arm of the
    /// author's own phase — reached through an archive import, the one post
    /// that phase can hold (authored by the importer, `source` = the platform).
    /// There quarantine must NOT bind: it is author-visible.
    #[tokio::test]
    async fn search_never_quotes_a_flagged_posts_body() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = [0x51u8; 32];
        let reader = [0x52u8; 32];

        let clean = seed_searchable_post(&db, author, "marmalade clean", None).await;
        let taken = seed_searchable_post(&db, author, "marmalade taken", None).await;
        let quarantined = seed_searchable_post(&db, author, "marmalade quarantined", None).await;
        let suppressed = seed_searchable_post(&db, author, "marmalade suppressed", None).await;
        db.set_post_legal_takedown(&taken, Some("EU-DSA-2024/881"))
            .await
            .unwrap();
        db.set_post_quarantined(&quarantined, true).await.unwrap();
        db.set_post_suppressed(&suppressed, true).await.unwrap();

        let hits = |rows: &[SearchResult]| -> Vec<String> {
            rows.iter().map(|r| r.content_id.clone()).collect()
        };
        let public = hits(
            &db.search_with_scoping("marmalade", &reader, None, None, None, 50, 0)
                .await
                .unwrap(),
        );
        assert!(
            public.contains(&hex::encode(clean)),
            "control: an unflagged post is found: {public:?}"
        );
        assert!(
            !public.contains(&hex::encode(taken)),
            "a TAKEN-DOWN post's body must never be quoted back by search"
        );
        assert!(
            !public.contains(&hex::encode(quarantined)),
            "a QUARANTINED post's body must never be quoted back by search"
        );
        assert!(
            !public.contains(&hex::encode(suppressed)),
            "a SUPPRESSED post's body must never be quoted back by search"
        );

        // The author's own phase: an explicit platform filter routes the query
        // to it alone (`search_with_scoping`'s phase 2).
        let own_clean = seed_searchable_post(&db, author, "quince clean", Some("facebook")).await;
        let own_taken = seed_searchable_post(&db, author, "quince taken", Some("facebook")).await;
        let own_quarantined =
            seed_searchable_post(&db, author, "quince quarantined", Some("facebook")).await;
        db.set_post_legal_takedown(&own_taken, Some("EU-DSA-2024/882"))
            .await
            .unwrap();
        db.set_post_quarantined(&own_quarantined, true)
            .await
            .unwrap();
        let own = hits(
            &db.search_with_scoping("quince", &author, Some("facebook"), None, None, 50, 0)
                .await
                .unwrap(),
        );
        assert!(
            own.contains(&hex::encode(own_clean)),
            "control: the author finds their own archive import: {own:?}"
        );
        assert!(
            !own.contains(&hex::encode(own_taken)),
            "a takedown withholds the body from its AUTHOR too — their own search must not \
             quote it"
        );
        assert!(
            own.contains(&hex::encode(own_quarantined)),
            "quarantine is author-visible: the author's own search still finds it: {own:?}"
        );
    }

    #[tokio::test]
    async fn list_folders_returns_all() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_folder("photos", &[1u8; 32]).await.unwrap();
        db.create_folder("docs", &[2u8; 32]).await.unwrap();
        let sets = db.list_folders().await.unwrap();
        assert_eq!(sets.len(), 2);
        let names: Vec<&str> = sets.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"photos"));
        assert!(names.contains(&"docs"));
    }

    #[tokio::test]
    async fn folder_needs_snapshot_no_changes() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_folder("empty", &[1u8; 32]).await.unwrap();
        let fs = db.get_folder("empty").await.unwrap().unwrap();
        assert!(!db.folder_needs_snapshot(fs.id, 0).await.unwrap());
    }

    #[tokio::test]
    async fn folder_needs_snapshot_with_changes_no_snapshot() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_folder("new", &[1u8; 32]).await.unwrap();
        let fs = db.get_folder("new").await.unwrap().unwrap();
        let ph: [u8; 32] = *blake3::hash(b"f.txt").as_bytes();
        db.record_sync_change(
            &[1u8; 32],
            &ph,
            Some(&[0xAAu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("f.txt"),
        )
        .await
        .unwrap();
        // quiet_secs = 0 means no waiting
        assert!(db.folder_needs_snapshot(fs.id, 0).await.unwrap());
    }

    #[tokio::test]
    async fn folder_needs_snapshot_covered_by_existing() {
        let db = CacheDb::open_in_memory().unwrap();
        db.create_folder("covered", &[1u8; 32]).await.unwrap();
        let fs = db.get_folder("covered").await.unwrap().unwrap();
        let ph: [u8; 32] = *blake3::hash(b"g.txt").as_bytes();
        db.record_sync_change(
            &[1u8; 32],
            &ph,
            Some(&[0xBBu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("g.txt"),
        )
        .await
        .unwrap();
        // Create snapshot (which now captures max_change_seq)
        let _snap = db
            .create_snapshot_v2(fs.id, None, &[], None, None)
            .await
            .unwrap();
        // No new changes since snapshot
        assert!(!db.folder_needs_snapshot(fs.id, 0).await.unwrap());
    }

    /// An ordinary Backup folder's records ride the `sync_changes` head feed
    /// since the phase 3 head unification (2026-08-17, `file-sync.md` § Per-mode
    /// membership → *Target state — head unification*) — the snapshot subsystem
    /// reads the one plane and the one `max_change_seq` watermark for it.
    #[tokio::test]
    async fn folder_needs_snapshot_backup_folder_reads_the_head_feed() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        db.create_folder_with_options("photo-library", &actor, crate::db::FolderOptions::default())
            .await
            .unwrap();
        let fs = db.get_folder("photo-library").await.unwrap().unwrap();

        // No changes yet — nothing to snapshot.
        assert!(!db.folder_needs_snapshot(fs.id, 0).await.unwrap());

        let ph: [u8; 32] = *blake3::hash(b"IMG_0001.heic").as_bytes();
        db.record_sync_change(
            &actor,
            &ph,
            Some(&[0xAAu8; 32]),
            2048,
            "create",
            Some(fs.id),
            None,
            Some("IMG_0001.heic"),
        )
        .await
        .unwrap();

        // An unsnapshotted head record fires.
        assert!(
            db.folder_needs_snapshot(fs.id, 0).await.unwrap(),
            "a Backup folder with a new head record must need a snapshot"
        );

        db.create_snapshot_v2(fs.id, None, &[], None, None)
            .await
            .unwrap();
        assert!(
            !db.folder_needs_snapshot(fs.id, 0).await.unwrap(),
            "changes covered by the max_change_seq watermark must not re-fire"
        );

        // A delete record advances the seq — the removal is itself a
        // point-in-time change the next snapshot must capture.
        db.record_sync_change(
            &actor,
            &ph,
            None,
            0,
            "delete",
            Some(fs.id),
            None,
            Some("IMG_0001.heic"),
        )
        .await
        .unwrap();
        assert!(
            db.folder_needs_snapshot(fs.id, 0).await.unwrap(),
            "a delete recorded after the last snapshot must re-fire"
        );
    }

    #[tokio::test]
    async fn create_snapshot_v2_backup_folder_builds_from_the_head_feed() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        db.create_folder_with_options("wizard-backup", &actor, crate::db::FolderOptions::default())
            .await
            .unwrap();
        let fs = db.get_folder("wizard-backup").await.unwrap().unwrap();

        let live: [u8; 32] = *blake3::hash(b"docs/keep.txt").as_bytes();
        db.record_sync_change(
            &actor,
            &live,
            Some(&[0xAAu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("docs/keep.txt"),
        )
        .await
        .unwrap();
        let gone: [u8; 32] = *blake3::hash(b"docs/deleted.txt").as_bytes();
        db.record_sync_change(
            &actor,
            &gone,
            Some(&[0xBBu8; 32]),
            50,
            "create",
            Some(fs.id),
            None,
            Some("docs/deleted.txt"),
        )
        .await
        .unwrap();
        db.record_sync_change(
            &actor,
            &gone,
            None,
            0,
            "delete",
            Some(fs.id),
            None,
            Some("docs/deleted.txt"),
        )
        .await
        .unwrap();

        let snap = db
            .create_snapshot_v2(fs.id, None, &[], None, None)
            .await
            .unwrap();
        assert_eq!(
            snap.file_count, 1,
            "the snapshot must capture the live head path and exclude the deleted one"
        );
        assert_eq!(snap.total_bytes, 100);

        let files = db.get_snapshot_files(snap.id).await.unwrap();
        assert_eq!(files.len(), 1);
        // Post-flip the row is hash-keyed — no plaintext path rests at all.
        assert_eq!(
            files[0].path_hash,
            fauna_core::sync::path_hash("docs/keep.txt").to_vec()
        );
        assert_eq!(files[0].manifest_hash, vec![0xAAu8; 32]);
    }

    /// A reserved (`__`) backup **destination** set is custodian-held
    /// latest-per-path custody for another location's data — snapshotting it
    /// would pin superseded segment manifests on the custodian with no owner
    /// control, defeating the custody reclamation contract
    /// (`message-segment-store.md` § GC-safety). Refused, like the other
    /// pure-backup-destination gates.
    #[tokio::test]
    async fn create_snapshot_v2_refuses_reserved_backup_destination_set() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        db.create_folder_with_options(
            "__mail",
            &actor,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let fs = db.get_folder("__mail").await.unwrap().unwrap();
        let ph: [u8; 32] = *blake3::hash(b"seg-00000001.dat").as_bytes();
        db.upsert_backup_custody(
            &actor,
            fs.id,
            &ph,
            Some("seg-00000001.dat"),
            &[0xCCu8; 32],
            4096,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();

        assert!(
            db.create_snapshot_v2(fs.id, None, &[], None, None)
                .await
                .is_err(),
            "a reserved backup destination set must refuse snapshot create"
        );
        assert!(
            !db.folder_needs_snapshot(fs.id, 0).await.unwrap(),
            "the scheduler must never target a reserved backup destination set"
        );
    }

    #[tokio::test]
    async fn upsert_contributor_inserts_and_updates() {
        let db = CacheDb::open_in_memory().unwrap();
        let feed_id = db
            .create_feed(&[1; 32], "test", &[], "all", "discovery", "[]", None)
            .await
            .unwrap();

        db.upsert_contributor(&feed_id, "https://bob.nest", Some(&[2; 32]), "seed")
            .await
            .unwrap();
        let contributors = db.list_contributors(&feed_id).await.unwrap();
        assert_eq!(contributors.len(), 1);
        assert_eq!(contributors[0].nest_url, "https://bob.nest");
        assert_eq!(contributors[0].discovered_via, "seed");

        // Upsert with higher signal channel should update
        db.upsert_contributor(&feed_id, "https://bob.nest", Some(&[2; 32]), "manual")
            .await
            .unwrap();
        let contributors = db.list_contributors(&feed_id).await.unwrap();
        assert_eq!(contributors.len(), 1);
        assert_eq!(contributors[0].discovered_via, "manual");

        // Upsert with lower signal channel should NOT downgrade
        db.upsert_contributor(&feed_id, "https://bob.nest", Some(&[2; 32]), "seed")
            .await
            .unwrap();
        let contributors = db.list_contributors(&feed_id).await.unwrap();
        assert_eq!(contributors[0].discovered_via, "manual");
    }

    #[tokio::test]
    async fn record_contributor_hit_increments() {
        let db = CacheDb::open_in_memory().unwrap();
        let feed_id = db
            .create_feed(&[1; 32], "test", &[], "all", "discovery", "[]", None)
            .await
            .unwrap();
        db.upsert_contributor(&feed_id, "https://bob.nest", Some(&[2; 32]), "seed")
            .await
            .unwrap();

        db.record_contributor_hit(&feed_id, "https://bob.nest", Some(&[2; 32]), 1000)
            .await
            .unwrap();
        db.record_contributor_hit(&feed_id, "https://bob.nest", Some(&[2; 32]), 2000)
            .await
            .unwrap();

        let contribs = db.list_contributors(&feed_id).await.unwrap();
        assert_eq!(contribs[0].hit_count, 2);
        assert_eq!(contribs[0].last_seen, 2000);
    }

    #[tokio::test]
    async fn remove_contributor_works() {
        let db = CacheDb::open_in_memory().unwrap();
        let feed_id = db
            .create_feed(&[1; 32], "test", &[], "all", "discovery", "[]", None)
            .await
            .unwrap();
        db.upsert_contributor(&feed_id, "https://bob.nest", Some(&[2; 32]), "seed")
            .await
            .unwrap();

        let removed = db
            .remove_contributor(&feed_id, "https://bob.nest", Some(&[2; 32]))
            .await
            .unwrap();
        assert!(removed);

        let contribs = db.list_contributors(&feed_id).await.unwrap();
        assert_eq!(contribs.len(), 0);
    }

    #[tokio::test]
    async fn delete_contributors_for_feed_clears_all() {
        let db = CacheDb::open_in_memory().unwrap();
        let feed_id = db
            .create_feed(&[1; 32], "test", &[], "all", "discovery", "[]", None)
            .await
            .unwrap();
        db.upsert_contributor(&feed_id, "https://a.nest", Some(&[2; 32]), "seed")
            .await
            .unwrap();
        db.upsert_contributor(&feed_id, "https://b.nest", Some(&[3; 32]), "seed")
            .await
            .unwrap();

        db.delete_contributors_for_feed(&feed_id).await.unwrap();
        let contribs = db.list_contributors(&feed_id).await.unwrap();
        assert_eq!(contribs.len(), 0);
    }

    #[tokio::test]
    async fn list_contributors_by_priority_filters() {
        let db = CacheDb::open_in_memory().unwrap();
        let feed_id = db
            .create_feed(&[1; 32], "test", &[], "all", "discovery", "[]", None)
            .await
            .unwrap();
        db.upsert_contributor(&feed_id, "https://hot.nest", Some(&[2; 32]), "seed")
            .await
            .unwrap();
        // Insert a warm one manually
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO feed_contributors (feed_id, nest_url, author_id, poll_priority, created_at)
                 VALUES (?1, ?2, ?3, 'warm', ?4)",
                rusqlite::params![feed_id, "https://warm.nest", vec![3u8; 32], now_epoch_secs()],
            ).unwrap();
        }

        let hot = db
            .list_contributors_by_priority(&feed_id, "hot")
            .await
            .unwrap();
        assert_eq!(hot.len(), 1);
        assert_eq!(hot[0].nest_url, "https://hot.nest");

        let warm = db
            .list_contributors_by_priority(&feed_id, "warm")
            .await
            .unwrap();
        assert_eq!(warm.len(), 1);
        assert_eq!(warm[0].nest_url, "https://warm.nest");
    }

    #[tokio::test]
    async fn upsert_contributor_nest_level_entry() {
        let db = CacheDb::open_in_memory().unwrap();
        let feed_id = db
            .create_feed(&[1; 32], "test", &[], "all", "discovery", "[]", None)
            .await
            .unwrap();

        db.upsert_contributor(&feed_id, "https://bob.nest", None, "seed")
            .await
            .unwrap();
        let contribs = db.list_contributors(&feed_id).await.unwrap();
        assert_eq!(contribs.len(), 1);
        assert!(contribs[0].author_id.is_none());

        db.upsert_contributor(&feed_id, "https://bob.nest", Some(&[2; 32]), "seed")
            .await
            .unwrap();
        let contribs = db.list_contributors(&feed_id).await.unwrap();
        assert_eq!(contribs.len(), 2);
    }

    #[tokio::test]
    async fn inbox_source_column() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [20u8; 32];
        db.create_user(&actor, "free", "").await.unwrap();

        // Regular push_inbox should have source = 'fauna' (default)
        let id = db.push_inbox(&actor, b"test payload", None).await.unwrap();
        let conn = db.conn.lock().await;
        let source: String = conn
            .query_row(
                "SELECT source FROM content WHERE id = (SELECT source_id FROM content_links WHERE id = ?1 AND link_type = 'delivery')",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source, "fauna");
    }

    #[tokio::test]
    async fn fauna_native_inbox_no_metadata() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [21u8; 32];
        db.create_user(&actor, "free", "").await.unwrap();

        let id = db
            .push_inbox(&actor, b"encrypted-payload", None)
            .await
            .unwrap();
        let conn = db.conn.lock().await;
        let meta_bytes: Option<Vec<u8>> = conn
            .query_row(
                "SELECT metadata FROM content_links WHERE id = ?1 AND link_type = 'delivery'",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        // Metadata should exist but have only a mailbox field (no subject/sender)
        let meta: serde_json::Value = serde_json::from_slice(&meta_bytes.unwrap()).unwrap();
        assert_eq!(meta.get("subject"), None);
        assert_eq!(meta.get("sender"), None);
        assert_eq!(meta.get("mailbox").and_then(|v| v.as_str()), Some("INBOX"));
    }

    #[tokio::test]
    async fn operation_lock_acquire_release() {
        let db = CacheDb::open_in_memory().unwrap();
        // Acquire a GC lock (folder_id = -1 for global)
        assert!(db.try_acquire_op_lock("gc", -1, "holder-a").await.unwrap());
        // Second acquire should fail
        assert!(!db.try_acquire_op_lock("gc", -1, "holder-b").await.unwrap());
        // Release
        db.release_op_lock("gc", -1).await.unwrap();
        // Now should succeed
        assert!(db.try_acquire_op_lock("gc", -1, "holder-b").await.unwrap());
        db.release_op_lock("gc", -1).await.unwrap();
    }

    #[tokio::test]
    async fn operation_lock_different_types_no_conflict() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.try_acquire_op_lock("prune", 1, "holder-a")
                .await
                .unwrap()
        );
        // Different lock type on same folder — no conflict
        assert!(
            db.try_acquire_op_lock("check", 1, "holder-b")
                .await
                .unwrap()
        );
        db.release_op_lock("prune", 1).await.unwrap();
        db.release_op_lock("check", 1).await.unwrap();
    }

    #[tokio::test]
    async fn operation_lock_expires() {
        let db = CacheDb::open_in_memory().unwrap();
        // Manually insert an expired lock
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO operation_locks (lock_type, folder_id, holder, acquired_at, expires_at) VALUES ('gc', -1, 'stale', 0, 1)",
                [],
            ).unwrap();
        }
        // Should succeed because expired lock is cleaned up
        assert!(db.try_acquire_op_lock("gc", -1, "fresh").await.unwrap());
        db.release_op_lock("gc", -1).await.unwrap();
    }

    #[tokio::test]
    async fn op_lock_gc_conflicts_with_restore() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.try_acquire_op_lock("restore", 1, "restore-1")
                .await
                .unwrap()
        );
        assert!(!db.try_acquire_op_lock("gc", -1, "gc-run").await.unwrap());
        db.release_op_lock("restore", 1).await.unwrap();
        assert!(db.try_acquire_op_lock("gc", -1, "gc-run").await.unwrap());
    }

    #[tokio::test]
    async fn op_lock_prune_does_not_conflict_with_restore() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(db.try_acquire_op_lock("prune", 1, "prune-1").await.unwrap());
        assert!(
            db.try_acquire_op_lock("restore", 2, "restore-2")
                .await
                .unwrap()
        );
        assert!(db.try_acquire_op_lock("check", 1, "check-1").await.unwrap());
    }

    #[tokio::test]
    async fn op_lock_prune_conflicts_with_gc() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(db.try_acquire_op_lock("prune", 1, "prune-1").await.unwrap());
        assert!(!db.try_acquire_op_lock("gc", -1, "gc").await.unwrap());
    }

    #[tokio::test]
    async fn upload_lease_acquire_release() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let device = [1u8; 32];
        assert!(
            db.try_acquire_upload_lease(1, &actor, &device, 300)
                .await
                .unwrap()
        );
        // Same actor + device can renew
        assert!(
            db.try_acquire_upload_lease(1, &actor, &device, 300)
                .await
                .unwrap()
        );
        // Different device blocked
        let other = [2u8; 32];
        assert!(
            !db.try_acquire_upload_lease(1, &actor, &other, 300)
                .await
                .unwrap()
        );

        // A release naming the WRONG device is a no-op — the holder's
        // lease survives, so a writer naming another device can't grief-grab it.
        db.release_upload_lease(1, &actor, &other).await.unwrap();
        assert!(
            !db.try_acquire_upload_lease(1, &actor, &other, 300)
                .await
                .unwrap(),
            "the holder's lease survived a mis-scoped release"
        );
        // The holder's own device_id releases it. (The device-id-less clear
        // that used to follow here was retired 2026-09-24 — every release
        // names its device.)
        db.release_upload_lease(1, &actor, &device).await.unwrap();
        assert!(
            db.try_acquire_upload_lease(1, &actor, &other, 300)
                .await
                .unwrap()
        );
    }

    /// The lease is bound to the ACTOR, not just the client-asserted device id
    /// (`file-sync.md` § Exclusive editing): another account naming the holder's
    /// device can neither release, renew nor take it over.
    #[tokio::test]
    async fn upload_lease_is_bound_to_the_acquiring_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [9u8; 32];
        let member = [8u8; 32];
        let device = [1u8; 32];
        assert!(
            db.try_acquire_upload_lease(1, &owner, &device, 300)
                .await
                .unwrap()
        );
        assert!(
            !db.try_acquire_upload_lease(1, &member, &device, 300)
                .await
                .unwrap(),
            "a same-device acquire from another account is a refused takeover, not a renewal"
        );
        db.release_upload_lease(1, &member, &device).await.unwrap();
        assert!(
            !db.try_acquire_upload_lease(1, &member, &[2u8; 32], 300)
                .await
                .unwrap(),
            "another account naming the holder's device cannot release it"
        );
        let live = db.live_upload_leases_for(&[1]).await.unwrap();
        assert_eq!(live[&1].actor_id, owner.to_vec());
        assert!(
            db.try_acquire_upload_lease(1, &owner, &device, 300)
                .await
                .unwrap(),
            "the holder still renews"
        );
    }

    #[tokio::test]
    async fn upload_lease_expired_takeover() {
        let db = CacheDb::open_in_memory().unwrap();
        let device = [1u8; 32];
        // Insert an already-expired lease
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO upload_leases (folder_id, device_id, acquired_at, expires_at, heartbeat_at, actor_id) VALUES (1, ?1, 0, 1, 0, ?2)",
                rusqlite::params![device.as_slice(), [8u8; 32].as_slice()],
            ).unwrap();
        }
        let actor = [9u8; 32];
        let other = [2u8; 32];
        assert!(
            db.try_acquire_upload_lease(1, &actor, &other, 300)
                .await
                .unwrap()
        );
        db.release_upload_lease(1, &actor, &other).await.unwrap();
    }

    #[tokio::test]
    async fn store_and_retrieve_pairing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = vec![1u8; 32];
        let private_nest_id = vec![2u8; 32];
        let capabilities = vec!["submit_posts".into(), "submit_blobs".into()];

        db.store_pairing(&actor_id, &private_nest_id, &capabilities, None, None, None)
            .await
            .unwrap();

        let pairing = db.get_pairing(&actor_id, &private_nest_id).await.unwrap();
        assert!(pairing.is_some());
        let p = pairing.unwrap();
        assert_eq!(p.actor_id, actor_id);
        assert_eq!(p.private_nest_id, private_nest_id);
        assert_eq!(p.capabilities.len(), 2);
    }

    #[tokio::test]
    async fn revoke_pairing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = vec![1u8; 32];
        let private_nest_id = vec![2u8; 32];

        db.store_pairing(&actor_id, &private_nest_id, &[], None, None, None)
            .await
            .unwrap();
        db.revoke_pairing(&actor_id, &private_nest_id)
            .await
            .unwrap();

        let pairing = db.get_pairing(&actor_id, &private_nest_id).await.unwrap();
        assert!(pairing.is_none());
    }

    #[tokio::test]
    async fn is_paired_respects_expiry() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = vec![1u8; 32];
        let nest_id = vec![2u8; 32];

        // No expiry — always paired
        db.store_pairing(&actor_id, &nest_id, &[], None, None, None)
            .await
            .unwrap();
        assert!(db.is_paired(&actor_id, &nest_id).await.unwrap());

        // Expired pairing
        db.store_pairing(&actor_id, &nest_id, &[], Some(1), None, None)
            .await
            .unwrap();
        assert!(!db.is_paired(&actor_id, &nest_id).await.unwrap());
    }

    #[tokio::test]
    async fn pairing_has_capability_exact_match_and_expiry() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_id = vec![1u8; 32];
        let nest_id = vec![2u8; 32];

        // A pairing granting mls_pull but NOT mail_pull.
        db.store_pairing(
            &actor_id,
            &nest_id,
            &["mls_pull".to_string(), "namespace_sync".to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            db.pairing_has_capability(&actor_id, &nest_id, "mls_pull")
                .await
                .unwrap()
        );
        assert!(
            !db.pairing_has_capability(&actor_id, &nest_id, "mail_pull")
                .await
                .unwrap(),
            "mail_pull not granted → relay gate closed"
        );
        // Exact match, not substring: 'mls_pull' must not satisfy 'mail_pull'
        // and vice-versa.
        assert!(
            !db.pairing_has_capability(&actor_id, &nest_id, "pull")
                .await
                .unwrap()
        );

        // Re-pair WITH mail_pull → gate opens.
        db.store_pairing(
            &actor_id,
            &nest_id,
            &["mail_pull".to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            db.pairing_has_capability(&actor_id, &nest_id, "mail_pull")
                .await
                .unwrap()
        );

        // Expired pairing → capability check is false even if granted.
        db.store_pairing(
            &actor_id,
            &nest_id,
            &["mail_pull".to_string()],
            Some(1),
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            !db.pairing_has_capability(&actor_id, &nest_id, "mail_pull")
                .await
                .unwrap(),
            "expired pairing → gate closed"
        );

        // Unknown pairing → false.
        assert!(
            !db.pairing_has_capability(&actor_id, &[9u8; 32], "mail_pull")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn any_pairing_with_capability_box_wide_and_parse_verified() {
        let db = CacheDb::open_in_memory().unwrap();

        // Empty box → false.
        assert!(
            !db.any_pairing_with_capability("nostr_push").await.unwrap(),
            "no pairings → serving predicate closed"
        );

        // A pairing WITHOUT the cap → still false.
        db.store_pairing(
            &[1u8; 32],
            &[2u8; 32],
            &["mls_pull".to_string(), "post_forward".to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            !db.any_pairing_with_capability("nostr_push").await.unwrap(),
            "a pairing lacking nostr_push does not open serving"
        );

        // A DIFFERENT actor grants nostr_push → box-wide true (actor-agnostic).
        db.store_pairing(
            &[3u8; 32],
            &[4u8; 32],
            &["namespace_sync".to_string(), "nostr_push".to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            db.any_pairing_with_capability("nostr_push").await.unwrap(),
            "any actor's nostr_push pairing opens serving box-wide"
        );

        // Expired nostr_push pairing does NOT count (fresh box to isolate).
        let db2 = CacheDb::open_in_memory().unwrap();
        db2.store_pairing(
            &[5u8; 32],
            &[6u8; 32],
            &["nostr_push".to_string()],
            Some(1), // long-expired
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            !db2.any_pairing_with_capability("nostr_push").await.unwrap(),
            "an expired nostr_push pairing must not open serving"
        );

        // Parse-verification, not bare LIKE: a substring-only match must fail.
        let db3 = CacheDb::open_in_memory().unwrap();
        db3.store_pairing(
            &[7u8; 32],
            &[8u8; 32],
            &["nostr_push_extra".to_string()], // contains "nostr_push" as a prefix
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            !db3.any_pairing_with_capability("nostr_push").await.unwrap(),
            "a capability that merely CONTAINS 'nostr_push' must not satisfy the exact gate"
        );

        // Corrupt caps JSON that the LIKE prefilter matches must NOT return true
        // (the row is parse-verified; unparseable → empty caps → no match).
        let db4 = CacheDb::open_in_memory().unwrap();
        {
            let conn = db4.conn().await;
            conn.execute(
                "INSERT INTO nest_pairings (actor_id, private_nest_id, capabilities, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    &[9u8; 32][..],
                    &[10u8; 32][..],
                    "not-json-but-mentions-nostr_push",
                    1i64
                ],
            )
            .unwrap();
        }
        assert!(
            !db4.any_pairing_with_capability("nostr_push").await.unwrap(),
            "a LIKE-matching but unparseable caps row must not open serving (parse-verify beats bare LIKE)"
        );
    }

    #[tokio::test]
    async fn actor_has_pairing_with_capability_scoped_and_scans_all_peers() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = vec![1u8; 32];
        let other = vec![9u8; 32];

        // No pairing → false for the actor.
        assert!(
            !db.actor_has_pairing_with_capability(&actor, "nostr_push")
                .await
                .unwrap()
        );

        // Peer A grants only mls_pull; the gate for nostr_push stays closed.
        db.store_pairing(
            &actor,
            &[2u8; 32],
            &["mls_pull".to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            !db.actor_has_pairing_with_capability(&actor, "nostr_push")
                .await
                .unwrap(),
            "a pairing lacking nostr_push does not open the actor's gate"
        );

        // Peer B grants nostr_push → the actor's gate opens (scans ALL peer rows).
        db.store_pairing(
            &actor,
            &[3u8; 32],
            &["nostr_push".to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            db.actor_has_pairing_with_capability(&actor, "nostr_push")
                .await
                .unwrap(),
            "any of the actor's peer pairings granting nostr_push opens the gate"
        );

        // A DIFFERENT actor's gate stays closed (actor-scoped, not box-wide).
        assert!(
            !db.actor_has_pairing_with_capability(&other, "nostr_push")
                .await
                .unwrap(),
            "the gate is actor-scoped: another actor's nostr_push does not leak in"
        );

        // Expired pairing → the actor's gate closes (fresh actor to isolate).
        let expired_actor = vec![4u8; 32];
        db.store_pairing(
            &expired_actor,
            &[5u8; 32],
            &["nostr_push".to_string()],
            Some(1),
            None,
            None,
        )
        .await
        .unwrap();
        assert!(
            !db.actor_has_pairing_with_capability(&expired_actor, "nostr_push")
                .await
                .unwrap(),
            "an expired nostr_push pairing must not open the actor's gate"
        );
    }

    #[tokio::test]
    async fn outbox_enqueue_and_drain() {
        let db = CacheDb::open_in_memory().unwrap();

        db.outbox_enqueue(&[1u8; 32], b"post1_bytes", "forwarded_post")
            .await
            .unwrap();
        db.outbox_enqueue(&[1u8; 32], b"post2_bytes", "forwarded_post")
            .await
            .unwrap();

        let pending = db.outbox_pending(10).await.unwrap();
        assert_eq!(pending.len(), 2);

        db.outbox_mark_sent(pending[0].id).await.unwrap();
        let remaining = db.outbox_pending(10).await.unwrap();
        assert_eq!(remaining.len(), 1);
    }

    /// Due-ness ignores the retry ceiling: a refusal past it is still pending
    /// once its backoff runs out, so a later grant can deliver it
    /// (`private-mode.md` § Post Forwarding). It is counted as stuck.
    #[tokio::test]
    async fn outbox_refusal_past_the_retry_ceiling_stays_due() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = [1u8; 32];
        db.create_user(&author, "free", "author").await.unwrap();
        db.outbox_enqueue(&author, b"refused", "forwarded_post")
            .await
            .unwrap();
        let id = db.outbox_pending(10).await.unwrap()[0].id;

        for _ in 0..=CacheDb::OUTBOX_MAX_ATTEMPTS {
            assert!(
                !db.outbox_record_failure(id, false, "refused")
                    .await
                    .unwrap(),
                "a refusal for a live author never leaves"
            );
        }
        assert!(
            db.outbox_pending(10).await.unwrap().is_empty(),
            "backed off after each failure"
        );
        db.test_age_outbox_failures(&author, CacheDb::OUTBOX_REARM_MIN_GAP_SECS)
            .await
            .unwrap();
        db.outbox_retry_now_for_author(&author).await.unwrap();
        let pending = db.outbox_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1, "past the ceiling, still due");
        assert_eq!(pending[0].attempts, CacheDb::OUTBOX_MAX_ATTEMPTS + 1);
        assert_eq!(db.outbox_stuck_count().await.unwrap(), 1);
    }

    /// The backoff doubles from 30 s and stops doubling at about 8.5 hours, so
    /// a refusal past the ceiling is retried roughly three times a day, never
    /// days apart.
    #[tokio::test]
    async fn outbox_backoff_is_capped_at_about_eight_and_a_half_hours() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = [2u8; 32];
        db.create_user(&author, "free", "author").await.unwrap();
        db.outbox_enqueue(&author, b"refused", "forwarded_post")
            .await
            .unwrap();
        let id = db.outbox_pending(10).await.unwrap()[0].id;
        for _ in 0..20 {
            db.outbox_record_failure(id, false, "refused")
                .await
                .unwrap();
        }
        let next_retry: i64 = db
            .conn
            .lock()
            .await
            .query_row(
                "SELECT next_retry FROM outbox WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        let wait_secs = (next_retry - fauna_core::data::Timestamp::now().as_i64()) / 1_000_000;
        assert!(
            (30 * 1024 - 60..=30 * 1024).contains(&wait_secs),
            "the 20th failure backs off 30 s × 2^10, not more: {wait_secs} s"
        );
    }

    /// A refusal's recorded reason is bounded and control-stripped before it
    /// is stored: part of it is the relay's own error code, and it is served to
    /// the author's app in `fauna.pair.list` (`private-mode.md` § Post
    /// Forwarding → the queue is the user's to see).
    #[tokio::test]
    async fn outbox_failure_reason_is_bounded_and_control_stripped() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = [3u8; 32];
        db.create_user(&author, "free", "author").await.unwrap();
        db.outbox_enqueue(&author, b"refused", "forwarded_post")
            .await
            .unwrap();
        let id = db.outbox_pending(10).await.unwrap()[0].id;
        let hostile = format!("peer post.forward: \u{1b}[31m{}\n", "x".repeat(4 << 20));
        db.outbox_record_failure(id, false, &hostile).await.unwrap();

        let stored = db
            .outbox_status_for_author(&author)
            .await
            .unwrap()
            .last_error
            .unwrap();
        assert!(
            stored.chars().count() <= CacheDb::OUTBOX_LAST_ERROR_MAX_CHARS + 1,
            "stored {} chars",
            stored.chars().count()
        );
        assert!(
            stored.starts_with("peer post.forward: [31mxxx"),
            "{stored:.60}"
        );
        assert!(stored.ends_with('…'), "a cut reason says it was cut");
        assert!(
            !stored.chars().any(char::is_control),
            "no control characters"
        );
    }

    /// One author's due rows cannot fill the pass: the batch takes each
    /// author's oldest due row in turn, so another author's newer row is
    /// attempted in the same pass however many rows the first has queued.
    #[tokio::test]
    async fn outbox_pending_takes_every_authors_rows_in_turn() {
        let db = CacheDb::open_in_memory().unwrap();
        let (hog, other) = ([4u8; 32], [5u8; 32]);
        for i in 0..60 {
            db.outbox_enqueue(&hog, format!("hog-{i}").as_bytes(), "forwarded_post")
                .await
                .unwrap();
        }
        db.outbox_enqueue(&other, b"other-1", "forwarded_post")
            .await
            .unwrap();
        db.outbox_enqueue(&other, b"other-2", "forwarded_post")
            .await
            .unwrap();

        let batch: Vec<Vec<u8>> = db
            .outbox_pending(50)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.payload)
            .collect();
        assert_eq!(batch.len(), 50);
        assert_eq!(
            &batch[..4],
            &[
                b"hog-0".to_vec(),
                b"other-1".to_vec(),
                b"hog-1".to_vec(),
                b"other-2".to_vec()
            ],
            "authors alternate, each author's own rows oldest first"
        );
    }

    /// The re-arm is throttled per row: it makes a backed-off row due no
    /// sooner than `OUTBOX_REARM_MIN_GAP_SECS` after that row's last attempt,
    /// so re-arming every second cannot turn the queue into a busy loop.
    #[tokio::test]
    async fn outbox_rearm_waits_out_the_minimum_gap_since_the_last_attempt() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = [6u8; 32];
        db.create_user(&author, "free", "author").await.unwrap();
        db.outbox_enqueue(&author, b"refused", "forwarded_post")
            .await
            .unwrap();
        let id = db.outbox_pending(10).await.unwrap()[0].id;
        for _ in 0..12 {
            db.outbox_record_failure(id, false, "refused")
                .await
                .unwrap();
        }

        // Just attempted: the re-arm pulls the 8.5 h backoff in to the gap,
        // but not to now.
        assert_eq!(db.outbox_retry_now_for_author(&author).await.unwrap(), 1);
        assert!(db.outbox_pending(10).await.unwrap().is_empty());
        assert_eq!(
            db.outbox_retry_now_for_author(&author).await.unwrap(),
            0,
            "already at the gap: nothing more to pull in"
        );

        // Once the gap has passed since the last attempt, it is due.
        db.test_age_outbox_failures(&author, CacheDb::OUTBOX_REARM_MIN_GAP_SECS)
            .await
            .unwrap();
        assert_eq!(db.outbox_pending(10).await.unwrap().len(), 1);
    }

    /// The forward queue's deletion leg, through the door an account deletion
    /// actually runs. The two entry types want opposite things: a queued post
    /// must not be published after its author is gone, a queued deletion must
    /// still be sent — unless it has already had its last chance.
    #[tokio::test]
    async fn a_deleted_authors_queue_keeps_only_a_tombstone_that_can_still_be_sent() {
        let db = CacheDb::open_in_memory().unwrap();
        let (gone, bystander) = ([0xA1u8; 32], [0xB2u8; 32]);

        db.outbox_enqueue(&gone, b"post", "forwarded_post")
            .await
            .unwrap();
        db.outbox_enqueue(&gone, b"live-tombstone", "forwarded_delete")
            .await
            .unwrap();
        db.outbox_enqueue(&gone, b"dead-tombstone", "forwarded_delete")
            .await
            .unwrap();
        db.outbox_enqueue(&bystander, b"their-post", "forwarded_post")
            .await
            .unwrap();
        db.create_user(&gone, "free", "gone").await.unwrap();
        let dead = db
            .outbox_pending(10)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.payload == b"dead-tombstone")
            .unwrap();
        for _ in 0..CacheDb::OUTBOX_MAX_ATTEMPTS {
            db.outbox_record_failure(dead.id, false, "refused")
                .await
                .unwrap();
        }

        db.purge_orphaned_actor_rows(&gone).await.unwrap();

        let conn = db.conn.lock().await;
        let mut left: Vec<Vec<u8>> = conn
            .prepare("SELECT payload FROM outbox")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![b"live-tombstone".to_vec(), b"their-post".to_vec()],
            "the deleted author's post and spent tombstone go; the tombstone the \
             worker can still send, and everyone else's queue, stay"
        );
    }

    /// A tombstone kept past its author's deletion keeps only the retries it
    /// has left: refused at the ceiling, it leaves, where a live author's
    /// refusal would stay queued for a later grant. So no row rests for ever
    /// naming a deleted account.
    #[tokio::test]
    async fn a_tombstone_kept_past_its_authors_deletion_leaves_at_the_ceiling() {
        let db = CacheDb::open_in_memory().unwrap();
        let (gone, live) = ([0xC1u8; 32], [0xC2u8; 32]);
        db.create_user(&gone, "free", "gone").await.unwrap();
        db.create_user(&live, "free", "live").await.unwrap();
        db.outbox_enqueue(&gone, b"orphan-tombstone", "forwarded_delete")
            .await
            .unwrap();
        db.outbox_enqueue(&live, b"live-tombstone", "forwarded_delete")
            .await
            .unwrap();
        let ids: Vec<(Vec<u8>, i64)> = db
            .outbox_pending(10)
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.payload, e.id))
            .collect();
        let id_of = |p: &[u8]| ids.iter().find(|(q, _)| q == p).unwrap().1;
        let (orphan, kept) = (id_of(b"orphan-tombstone"), id_of(b"live-tombstone"));

        // Two failures short of the ceiling, then the author is deleted.
        for id in [orphan, kept] {
            for _ in 0..CacheDb::OUTBOX_MAX_ATTEMPTS - 2 {
                db.outbox_record_failure(id, false, "refused")
                    .await
                    .unwrap();
            }
        }
        db.delete_user(&gone).await.unwrap();
        db.purge_orphaned_actor_rows(&gone).await.unwrap();
        assert_eq!(db.outbox_depth().await.unwrap(), 2, "still sendable: kept");

        assert!(
            !db.outbox_record_failure(orphan, false, "refused")
                .await
                .unwrap()
        );
        assert!(
            db.outbox_record_failure(orphan, false, "refused")
                .await
                .unwrap(),
            "its last chance refused, the deleted author's tombstone leaves"
        );
        for _ in 0..2 {
            assert!(
                !db.outbox_record_failure(kept, false, "refused")
                    .await
                    .unwrap(),
                "a live author's refusal stays at the ceiling"
            );
        }
        assert_eq!(db.outbox_depth().await.unwrap(), 1);
    }

    /// The subscribe queue's SECOND person, through the door an account
    /// deletion actually runs. `subscribe_requests` is registered by its
    /// author, so the registry walk never reaches the requesting reader: this
    /// leg does. A deleted reader's pending `subscribe` requests go — paid or
    /// not, each is only an instruction to grant a tier to an account that no
    /// longer exists, and it carries their ML-KEM key — while their pending
    /// `unsubscribe` stays, because draining it is what removes their retained
    /// roster row. The author's queue is otherwise untouched.
    #[tokio::test]
    async fn a_deleted_readers_pending_subscribe_requests_leave_the_authors_queue() {
        let db = CacheDb::open_in_memory().unwrap();
        let (author, gone, bystander) = ([0xC3u8; 32], [0xD4u8; 32], [0xE5u8; 32]);
        for tier in ["gold", "silver"] {
            db.create_subscription_tier(
                &author, tier, 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();
        }
        db.insert_subscribe_request(&author, &gone, "gold", "subscribe", Some(&[7u8; 1184]))
            .await
            .unwrap();
        db.upsert_payment_entitled_request(&author, &gone, "silver", Some(i64::MAX))
            .await
            .unwrap();
        db.insert_subscribe_request(&author, &gone, "gold", "unsubscribe", None)
            .await
            .unwrap();
        db.insert_subscribe_request(&author, &bystander, "gold", "subscribe", None)
            .await
            .unwrap();

        db.purge_orphaned_actor_rows(&gone).await.unwrap();

        let mut left: Vec<(Vec<u8>, String, String)> = db
            .list_subscribe_requests(&author)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.subscriber_id, r.tier_name, r.kind))
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                (gone.to_vec(), "gold".into(), "unsubscribe".into()),
                (bystander.to_vec(), "gold".into(), "subscribe".into()),
            ],
            "the deleted reader's subscribe requests (manual and paid) go; their \
             unsubscribe and every other reader's request stay"
        );
    }

    #[tokio::test]
    async fn store_and_retrieve_namespace_entries() {
        let db = CacheDb::open_in_memory().unwrap();
        let ns = vec![1u8; 32];
        let entry_id = vec![2u8; 32];
        let ciphertext = b"encrypted_data".to_vec();
        let actor_sig = vec![0u8; 64];

        db.namespace_put(&ns, &entry_id, &ciphertext, &actor_sig)
            .await
            .unwrap();

        let entries = db.namespace_entries_since(&ns, 0, 100).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_id, entry_id);
        assert_eq!(entries[0].ciphertext, ciphertext);
    }

    #[tokio::test]
    async fn namespace_sync_sequence_numbers() {
        let db = CacheDb::open_in_memory().unwrap();
        let ns = vec![1u8; 32];

        db.namespace_put(&ns, &[1u8; 32], b"data1", &[0u8; 64])
            .await
            .unwrap();
        db.namespace_put(&ns, &[2u8; 32], b"data2", &[0u8; 64])
            .await
            .unwrap();

        let entries = db.namespace_entries_since(&ns, 0, 100).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries[1].seq > entries[0].seq,
            "sequence numbers must be monotonic"
        );

        let new_entries = db
            .namespace_entries_since(&ns, entries[0].seq, 100)
            .await
            .unwrap();
        assert_eq!(new_entries.len(), 1);
        assert_eq!(new_entries[0].entry_id, vec![2u8; 32]);
    }

    #[tokio::test]
    async fn namespace_conflict_detection() {
        let db = CacheDb::open_in_memory().unwrap();
        let ns = vec![1u8; 32];
        let entry_id = vec![1u8; 32];

        db.namespace_put_with_source(&ns, &entry_id, b"version_a", &[0u8; 64], "nest_a")
            .await
            .unwrap();
        db.namespace_put_with_source(&ns, &entry_id, b"version_b", &[0u8; 64], "nest_b")
            .await
            .unwrap();

        let conflicts = db.namespace_conflicts(&ns, &entry_id).await.unwrap();
        assert_eq!(conflicts.len(), 2);
    }

    #[tokio::test]
    async fn auto_reply_rate_limiting() {
        let db = CacheDb::open_in_memory().unwrap();
        let recipient = [10u8; 32];
        let sender = [20u8; 32];

        // First check should allow (no previous reply)
        let allowed = db.check_auto_reply(&recipient, &sender, 24).await.unwrap();
        assert!(allowed, "first auto-reply should be allowed");

        // Record the reply
        db.record_auto_reply(&recipient, &sender).await.unwrap();

        // Second check within interval should block
        let allowed = db.check_auto_reply(&recipient, &sender, 24).await.unwrap();
        assert!(
            !allowed,
            "second auto-reply within interval should be blocked"
        );

        // Check with 0-hour interval: cutoff equals now, so the just-sent reply
        // has last_sent_at >= cutoff, which means it may or may not match the
        // strict `>` comparison. With a very large interval it should block.
        let allowed = db
            .check_auto_reply(&recipient, &sender, 999_999)
            .await
            .unwrap();
        assert!(!allowed, "large interval should block recently sent reply");

        // Different sender should be allowed
        let other_sender = [30u8; 32];
        let allowed = db
            .check_auto_reply(&recipient, &other_sender, 24)
            .await
            .unwrap();
        assert!(allowed, "different sender should be allowed");
    }

    #[tokio::test]
    async fn try_claim_auto_reply_is_atomic_and_rate_limited() {
        let db = CacheDb::open_in_memory().unwrap();
        let recipient = [11u8; 32];
        let sender = [22u8; 32];

        // First claim wins (records the slot).
        assert!(
            db.try_claim_auto_reply(&recipient, &sender, 24)
                .await
                .unwrap(),
            "first claim should win"
        );
        // Second claim within the interval is suppressed (slot consumed).
        assert!(
            !db.try_claim_auto_reply(&recipient, &sender, 24)
                .await
                .unwrap(),
            "second claim within interval should be suppressed"
        );
        // A different sender is an independent slot.
        let other_sender = [33u8; 32];
        assert!(
            db.try_claim_auto_reply(&recipient, &other_sender, 24)
                .await
                .unwrap(),
            "a different sender should get its own slot"
        );
        // An expired interval (0 h) frees the slot again (cutoff == now; the
        // strict `>` excludes the just-recorded ts).
        assert!(
            db.try_claim_auto_reply(&recipient, &sender, 0)
                .await
                .unwrap(),
            "a 0-hour interval should re-allow"
        );
    }

    #[tokio::test]
    async fn email_domain_crud() {
        let db = CacheDb::open_in_memory().unwrap();

        // Create a domain
        db.create_email_domain("example.com", "default", "ed25519")
            .await
            .unwrap();

        // List domains
        let domains = db.list_email_domains().await.unwrap();
        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].domain, "example.com");
        assert_eq!(domains[0].dkim_selector, "default");
        assert_eq!(domains[0].dkim_ed25519_selector, "ed25519");
        assert!(domains[0].enabled);

        // Delete domain (no users)
        let deleted = db.delete_email_domain("example.com").await.unwrap();
        assert!(deleted);

        // Delete non-existent domain
        let deleted = db.delete_email_domain("nonexistent.com").await.unwrap();
        assert!(!deleted);

        // List after delete
        let domains = db.list_email_domains().await.unwrap();
        assert!(domains.is_empty());
    }

    #[tokio::test]
    async fn email_domain_user_assignment() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [50u8; 32];

        db.create_email_domain("test.org", "sel1", "ed1")
            .await
            .unwrap();

        // Assign user
        db.assign_domain_user("test.org", &actor, "alice")
            .await
            .unwrap();

        // List users
        let users = db.list_domain_users("test.org").await.unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].local_part, "alice");
        assert_eq!(users[0].actor_id, actor.to_vec());

        // Remove user
        let removed = db.remove_domain_user("test.org", &actor).await.unwrap();
        assert!(removed);

        // Remove again returns false
        let removed = db.remove_domain_user("test.org", &actor).await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn email_domain_delete_fails_with_users() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [51u8; 32];

        db.create_email_domain("blocked.com", "s", "e")
            .await
            .unwrap();
        db.assign_domain_user("blocked.com", &actor, "bob")
            .await
            .unwrap();

        // Should fail because there are assigned users
        let result = db.delete_email_domain("blocked.com").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot delete domain")
        );
    }

    #[tokio::test]
    async fn resolve_email_address_by_domain_found() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [52u8; 32];

        db.create_email_domain("resolve.net", "s", "e")
            .await
            .unwrap();
        db.assign_domain_user("resolve.net", &actor, "carol")
            .await
            .unwrap();

        let found = db
            .resolve_email_address_by_domain("carol", "resolve.net")
            .await
            .unwrap();
        assert_eq!(found, Some(actor));
    }

    #[tokio::test]
    async fn resolve_email_address_by_domain_not_found() {
        let db = CacheDb::open_in_memory().unwrap();

        db.create_email_domain("empty.net", "s", "e").await.unwrap();

        let found = db
            .resolve_email_address_by_domain("nobody", "empty.net")
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn list_all_email_domains_set_and_selectors() {
        let db = CacheDb::open_in_memory().unwrap();

        db.create_email_domain("a.com", "dkim1", "ed1")
            .await
            .unwrap();
        db.create_email_domain("b.com", "dkim2", "ed2")
            .await
            .unwrap();

        let set = db.list_all_email_domains_set().await.unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains("a.com"));
        assert!(set.contains("b.com"));

        let sels = db.get_domain_selectors("a.com").await.unwrap();
        assert_eq!(sels, Some(("dkim1".to_string(), "ed1".to_string())));

        let sels = db.get_domain_selectors("nonexistent.com").await.unwrap();
        assert!(sels.is_none());
    }

    #[tokio::test]
    async fn test_create_folder_with_options() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [20; 32];
        db.create_user(&actor, "free", "mode test").await.unwrap();

        let id = db
            .create_folder_with_options(
                "my-backup",
                &actor,
                FolderOptions {
                    retention_policy: Some("keep-all".to_string()),
                    conflict_policy: Some("latest_wins_always".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(id > 0);

        let fs = db.get_folder("my-backup").await.unwrap().unwrap();
        assert!(
            !fs.custody_copy,
            "a client-shaped create is never a custody copy"
        );
        assert_eq!(fs.retention_policy.as_deref(), Some("keep-all"));
        // Create-time policy lands on the row (COALESCE keeps 'auto' when absent).
        assert_eq!(fs.conflict_policy, "latest_wins_always");
        assert_eq!(fs.cached_snapshot_count, 0);
        assert_eq!(fs.cached_total_bytes, 0);
        assert!(fs.cached_last_snapshot_at.is_none());
    }

    #[tokio::test]
    async fn test_get_folder_for_actor_scoping() {
        let db = CacheDb::open_in_memory().unwrap();
        let alice: [u8; 32] = [21; 32];
        let bob: [u8; 32] = [22; 32];
        db.create_user(&alice, "free", "alice").await.unwrap();
        db.create_user(&bob, "free", "bob").await.unwrap();

        // Name is globally unique, so use different names per actor
        db.create_folder_with_options(
            "alice-photos",
            &alice,
            crate::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
        db.create_folder_with_options(
            "bob-photos",
            &bob,
            crate::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Alice can see her own
        let fs = db
            .get_folder_for_actor("alice-photos", &alice)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs.actor_id, alice.to_vec());

        // Bob can see his own
        let fs = db
            .get_folder_for_actor("bob-photos", &bob)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs.actor_id, bob.to_vec());

        // Alice cannot see Bob's folder via user-scoped query
        assert!(
            db.get_folder_for_actor("bob-photos", &alice)
                .await
                .unwrap()
                .is_none()
        );

        // Bob cannot see Alice's folder via user-scoped query
        assert!(
            db.get_folder_for_actor("alice-photos", &bob)
                .await
                .unwrap()
                .is_none()
        );

        // Non-existent user sees nothing
        let charlie: [u8; 32] = [23; 32];
        assert!(
            db.get_folder_for_actor("alice-photos", &charlie)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_update_folder_for_user() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor: [u8; 32] = [24; 32];
        db.create_user(&actor, "free", "update test").await.unwrap();
        db.create_folder_with_options(
            "docs",
            &actor,
            crate::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Update retention_policy only
        let updated = db
            .update_folder_for_user(
                "docs",
                &actor,
                crate::db::FolderUpdate {
                    retention_policy: Some(Some("30d")),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(updated);
        let fs = db
            .get_folder_for_actor("docs", &actor)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs.retention_policy.as_deref(), Some("30d"));

        // Update it again
        let updated = db
            .update_folder_for_user(
                "docs",
                &actor,
                crate::db::FolderUpdate {
                    retention_policy: Some(Some("7d")),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(updated);
        let fs = db
            .get_folder_for_actor("docs", &actor)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs.retention_policy.as_deref(), Some("7d"));

        // Update the WebDAV serve flag only (persisted as folders.webdav_enabled)
        assert!(
            !db.get_folder_for_actor("docs", &actor)
                .await
                .unwrap()
                .unwrap()
                .webdav_enabled,
            "webdav_enabled defaults to false"
        );
        let updated = db
            .update_folder_for_user(
                "docs",
                &actor,
                crate::db::FolderUpdate {
                    webdav_enabled: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(updated);
        assert!(
            db.get_folder_for_actor("docs", &actor)
                .await
                .unwrap()
                .unwrap()
                .webdav_enabled,
            "webdav_enabled round-trips to true"
        );

        // Update the conflict policy only (persisted as folders.conflict_policy)
        assert_eq!(
            db.get_folder_for_actor("docs", &actor)
                .await
                .unwrap()
                .unwrap()
                .conflict_policy,
            "auto",
            "conflict_policy defaults to the ratified auto"
        );
        let updated = db
            .update_folder_for_user(
                "docs",
                &actor,
                crate::db::FolderUpdate {
                    conflict_policy: Some("latest_wins_always"),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(updated);
        assert_eq!(
            db.get_folder_for_actor("docs", &actor)
                .await
                .unwrap()
                .unwrap()
                .conflict_policy,
            "latest_wins_always",
            "conflict_policy round-trips"
        );

        // The keyed-writer set-name seal round-trips as an opaque blob (S5a).
        // Asserted rather than merely passed: an `update_folder_for_user`
        // argument no test reads is a column the write path can silently stop
        // persisting.
        let updated = db
            .update_folder_for_user(
                "docs",
                &actor,
                crate::db::FolderUpdate {
                    name_sealed: Some(&[0xAB, 0xCD][..]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(updated);
        // The seal rests the name NULL (schema 114): read back by the hash.
        let docs_hash = fauna_core::path_crypto::set_name_hash("docs");
        let sealed = db
            .get_folder_for_actor_by_name_hash(&docs_hash, &actor)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            sealed.name_sealed.as_deref(),
            Some(&[0xAB, 0xCD][..]),
            "the sealed set name round-trips byte-for-byte"
        );
        assert_eq!(sealed.name, "", "a sealed set rests no plaintext name");

        // No-op (all None) returns false — and leaves the seal untouched.
        let updated = db
            .update_folder_by_id(sealed.id, crate::db::FolderUpdate::default())
            .await
            .unwrap();
        assert!(!updated);
        assert_eq!(
            db.get_folder_for_actor_by_name_hash(&docs_hash, &actor)
                .await
                .unwrap()
                .unwrap()
                .name_sealed
                .as_deref(),
            Some(&[0xAB, 0xCD][..]),
            "`name_sealed: None` means leave unchanged, never blank"
        );
    }

    #[tokio::test]
    async fn test_delete_folder_for_user_wrong_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner: [u8; 32] = [25; 32];
        let intruder: [u8; 32] = [26; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();
        db.create_user(&intruder, "free", "intruder").await.unwrap();

        db.create_folder_with_options(
            "secret",
            &owner,
            crate::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Wrong actor cannot delete
        let deleted = db
            .delete_folder_for_user("secret", &intruder)
            .await
            .unwrap();
        assert!(!deleted);

        // Folder still exists
        let fs = db.get_folder_for_actor("secret", &owner).await.unwrap();
        assert!(fs.is_some());

        // Correct actor can delete
        let deleted = db.delete_folder_for_user("secret", &owner).await.unwrap();
        assert!(deleted);

        // Folder is gone
        let fs = db.get_folder_for_actor("secret", &owner).await.unwrap();
        assert!(fs.is_none());
    }

    /// the DB backstop: `delete_folder_for_user` refuses a
    /// reserved NON-backup set (a live rail) even if a future caller bypasses
    /// the handler guard. The custody shape stays deletable (the
    /// destination-removal handshake —
    /// `sync_storage.rs::deleting_the_custody_set_credits_back_retained_generations`
    /// proves that half).
    #[tokio::test]
    async fn test_delete_folder_for_user_refuses_a_live_rail() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner: [u8; 32] = [27; 32];
        db.create_user(&owner, "free", "owner").await.unwrap();

        // A genuine rail: minted by the production drafts-put path (mode
        // defaults to 'sync' — a rail, not a custody copy).
        db.record_drafts_blob_change(
            &owner,
            fauna_protocol::drafts::RAIL_CONVERSATIONS,
            &[0xEF; 32],
            128,
        )
        .await
        .unwrap();

        db.delete_folder_for_user("__drafts", &owner)
            .await
            .expect_err("the backstop must refuse a live rail loudly, not delete or Ok(false)");

        assert!(
            db.get_folder_for_actor("__drafts", &owner)
                .await
                .unwrap()
                .is_some(),
            "the rail survives the refused delete"
        );
    }

    /// `delete_all_folders_for_actor` (the reclaim half of holder-side "stop
    /// hosting" a held-for-friends guest) removes EVERY set the target actor owns
    /// and ONLY that actor's — a co-tenant's identically-named set survives.
    #[tokio::test]
    async fn test_delete_all_folders_for_actor_scoping() {
        let db = CacheDb::open_in_memory().unwrap();
        let guest: [u8; 32] = [0x42; 32];
        let other: [u8; 32] = [0x99; 32];
        db.create_user(&guest, "backup", "friend").await.unwrap();
        db.create_user(&other, "free", "co-tenant").await.unwrap();

        // The guest owns two reserved backup sets; the co-tenant owns one set
        // with the SAME name (allowed by UNIQUE(name, actor_id)).
        db.create_folder_with_options(
            "__mail",
            &guest,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        db.create_folder_with_options(
            "__conv",
            &guest,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        db.create_folder_with_options(
            "__mail",
            &other,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let removed = db.delete_all_folders_for_actor(&guest).await.unwrap();
        assert_eq!(removed, 2, "both of the guest's sets removed");

        assert!(
            db.get_folder_for_actor("__mail", &guest)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_folder_for_actor("__conv", &guest)
                .await
                .unwrap()
                .is_none()
        );
        // The co-tenant's identically-named set is untouched.
        assert!(
            db.get_folder_for_actor("__mail", &other)
                .await
                .unwrap()
                .is_some()
        );

        // Idempotent: deleting again removes nothing.
        assert_eq!(db.delete_all_folders_for_actor(&guest).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_list_devices_for_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_a = [0xA0; 32];
        let actor_b = [0xB0; 32];
        let dev1 = [0xD1; 32];
        let dev2 = [0xD2; 32];
        let dev3 = [0xD3; 32];

        db.register_sync_device(&actor_a, &dev1, "Laptop", None, "read,write")
            .await
            .unwrap();
        db.register_sync_device(&actor_a, &dev2, "Phone", None, "read")
            .await
            .unwrap();
        db.register_sync_device(&actor_b, &dev3, "Tablet", None, "read,write")
            .await
            .unwrap();

        let devices_a = db.list_devices_for_actor(&actor_a).await.unwrap();
        assert_eq!(devices_a.len(), 2);
        // S9 flip: user labels rest '' — the rows stay distinguishable (and
        // revocable) by device_id, which is the property that matters.
        assert_eq!(devices_a[0].device_id, dev1.to_vec());
        assert_eq!(devices_a[0].label, "");
        assert_eq!(devices_a[1].device_id, dev2.to_vec());

        let devices_b = db.list_devices_for_actor(&actor_b).await.unwrap();
        assert_eq!(devices_b.len(), 1);
        assert_eq!(devices_b[0].device_id, dev3.to_vec());
    }

    #[tokio::test]
    async fn test_get_device_folder_places() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0xA1; 32];
        let dev = [0xD1; 32];

        db.register_sync_device(&actor, &dev, "Laptop", None, "read,write")
            .await
            .unwrap();
        let fs_id = db.create_folder("photos", &actor).await.unwrap();
        let source_only = fauna_protocol::folders::PlaceFlags::new(true, false, false);
        db.add_folder_member(fs_id, &dev, &source_only)
            .await
            .unwrap();

        let places = db.get_device_folder_places(&dev, &actor).await.unwrap();
        assert_eq!(places.len(), 1);
        assert_eq!(places[0].0.name, "photos");
        // The place carries the set's address beside its name, so the
        // devices.list reader renders it once the plaintext scrubs.
        assert_eq!(
            places[0].0.name_hash.as_deref(),
            Some(&fauna_core::path_crypto::set_name_hash("photos")[..])
        );
        assert_eq!(places[0].1, source_only);
    }

    #[tokio::test]
    async fn test_delete_device_removes_memberships() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0xA2; 32];
        let dev = [0xD2; 32];

        db.register_sync_device(&actor, &dev, "Laptop", None, "read,write")
            .await
            .unwrap();
        let fs_id = db.create_folder("docs", &actor).await.unwrap();
        db.add_folder_member(
            fs_id,
            &dev,
            &fauna_protocol::folders::PlaceFlags::default_place(),
        )
        .await
        .unwrap();

        // Verify member exists
        let members = db.get_folder_members(fs_id).await.unwrap();
        assert_eq!(members.len(), 1);

        // Delete device
        let (deleted, members_removed, _) = db.delete_device(&dev, &actor).await.unwrap();
        assert!(deleted);
        assert_eq!(members_removed, 1);

        // Device is gone
        let device = db.get_device_for_user(&dev, &actor).await.unwrap();
        assert!(device.is_none());

        // Membership is gone
        let members = db.get_folder_members(fs_id).await.unwrap();
        assert!(members.is_empty());
    }

    /// The borrowed-device-id fixture: `sync_devices` is keyed `(actor_id,
    /// device_id)` and `fauna.sync.register` takes the id from the request, so
    /// account M can register victim V's device id X (read off
    /// `fauna.sync.changes.list`). V has X seated in a folder of V's own; M has
    /// X seated in a folder of M's. Returns V's and M's folder ids.
    async fn borrowed_device_id_fixture(
        db: &CacheDb,
        victim: &[u8; 32],
        mallory: &[u8; 32],
        dev: &[u8; 32],
    ) -> (i64, i64) {
        db.register_sync_device(victim, dev, "Laptop", None, "read,write")
            .await
            .unwrap();
        let victim_fs = db.create_folder("victim-photos", victim).await.unwrap();
        db.add_folder_member(
            victim_fs,
            dev,
            &fauna_protocol::folders::PlaceFlags::new(true, false, false),
        )
        .await
        .unwrap();
        db.register_sync_device(mallory, dev, "Laptop", None, "read,write")
            .await
            .unwrap();
        let mallory_fs = db.create_folder("mallory-notes", mallory).await.unwrap();
        db.add_folder_member(
            mallory_fs,
            dev,
            &fauna_protocol::folders::PlaceFlags::new(true, false, false),
        )
        .await
        .unwrap();
        (victim_fs, mallory_fs)
    }

    /// Borrowed-device-id door (a): `delete_device` removes the remover's own seats
    /// only (`devices.md` § Removing a Device, step 4).
    #[tokio::test]
    async fn delete_device_under_a_borrowed_id_leaves_the_other_accounts_seats() {
        let db = CacheDb::open_in_memory().unwrap();
        let (victim, mallory, dev) = ([0x7A; 32], [0x7B; 32], [0x7C; 32]);
        let (victim_fs, mallory_fs) =
            borrowed_device_id_fixture(&db, &victim, &mallory, &dev).await;

        let (deleted, members_removed, _) = db.delete_device(&dev, &mallory).await.unwrap();
        assert!(deleted);
        assert_eq!(members_removed, 1, "only M's own seat is M's to remove");
        assert!(db.get_folder_members(mallory_fs).await.unwrap().is_empty());
        assert_eq!(db.get_folder_members(victim_fs).await.unwrap().len(), 1);
    }

    /// Borrowed-device-id door (b): a grant revoke removes no seats at all — it
    /// clears the named row's grant columns and keeps the row
    /// (`sync-agent-credentials.md` § Credential model, the RULED 2026-09-28
    /// block, decision 4), so neither account's seat under the shared id moves.
    #[tokio::test]
    async fn revoke_device_grant_under_a_borrowed_id_leaves_every_seat() {
        let db = CacheDb::open_in_memory().unwrap();
        let (victim, mallory, dev) = ([0x7A; 32], [0x7B; 32], [0x7C; 32]);
        let (victim_fs, mallory_fs) =
            borrowed_device_id_fixture(&db, &victim, &mallory, &dev).await;
        db.set_sync_device_grant(&mallory, &dev, &dev, b"grant")
            .await
            .unwrap();

        let outcome = db.revoke_device_grant(&mallory, &dev).await.unwrap();
        assert!(outcome.cleared);
        assert_eq!(db.get_folder_members(mallory_fs).await.unwrap().len(), 1);
        assert_eq!(db.get_folder_members(victim_fs).await.unwrap().len(), 1);
    }

    /// Borrowed-device-id, the fifth reader: the places on a
    /// `fauna.sync.devices.list` row are the listing account's own folders only.
    #[tokio::test]
    async fn device_folder_places_under_a_borrowed_id_are_the_callers_own() {
        let db = CacheDb::open_in_memory().unwrap();
        let (victim, mallory, dev) = ([0x7A; 32], [0x7B; 32], [0x7C; 32]);
        borrowed_device_id_fixture(&db, &victim, &mallory, &dev).await;
        let originates_only = fauna_protocol::folders::PlaceFlags::new(true, false, false);

        let named = |places: Vec<(crate::db::sync_storage::SetLabel, _)>| {
            places
                .into_iter()
                .map(|(l, f)| (l.name, f))
                .collect::<Vec<_>>()
        };
        let places = db.get_device_folder_places(&dev, &mallory).await.unwrap();
        assert_eq!(
            named(places),
            vec![("mallory-notes".to_string(), originates_only.clone())]
        );
        let places = db.get_device_folder_places(&dev, &victim).await.unwrap();
        assert_eq!(
            named(places),
            vec![("victim-photos".to_string(), originates_only)]
        );
    }

    /// `sync_devices` is keyed `(actor_id, device_id)`, so two accounts can
    /// register the same device id. The label joins behind
    /// `fauna.folders.members.list` and `fauna.folders.devices` pair a seat
    /// with the READER's own registration only: one row per seat whoever else
    /// registered that id, and never another account's label
    /// (`path-sealing.md` § device label, gap (a)). The synthetic labels are
    /// the only ones that rest readable, which is what makes the pairing
    /// observable here.
    #[tokio::test]
    async fn folder_device_label_joins_are_scoped_to_the_reader() {
        use fauna_core::label_custody::{SELF_REGISTER_LABEL, WEBDAV_PSEUDO_DEVICE_LABEL};
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0xA1; 32];
        let other = [0xB2; 32];
        let stranger = [0xC3; 32];
        let dev = [0xD4; 32];

        db.register_sync_device(&owner, &dev, SELF_REGISTER_LABEL, None, "read,write")
            .await
            .unwrap();
        db.register_sync_device(&other, &dev, WEBDAV_PSEUDO_DEVICE_LABEL, None, "read,write")
            .await
            .unwrap();
        let fs_id = db.create_folder("shared-id", &owner).await.unwrap();
        db.add_folder_member(
            fs_id,
            &dev,
            &fauna_protocol::folders::PlaceFlags::default_place(),
        )
        .await
        .unwrap();
        for writer in [&owner, &other] {
            db.record_sync_change(
                writer,
                &[0x11; 32],
                None,
                1,
                "add",
                Some(fs_id),
                Some(&dev),
                None,
            )
            .await
            .unwrap();
        }

        let members = db
            .list_folder_members_with_labels(fs_id, &owner)
            .await
            .unwrap();
        assert_eq!(members.len(), 1, "one row per seat: {members:?}");
        assert_eq!(members[0].label, SELF_REGISTER_LABEL);
        let devices = db.get_folder_devices(fs_id, &owner).await.unwrap();
        assert_eq!(devices.len(), 1, "one row per device: {devices:?}");
        assert_eq!(devices[0].label, SELF_REGISTER_LABEL);

        // A reader with no registration of its own for the id sees the seat
        // nameless — never the label another account registered.
        let members = db
            .list_folder_members_with_labels(fs_id, &stranger)
            .await
            .unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].label, "");
        let devices = db.get_folder_devices(fs_id, &stranger).await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].label, "");
    }

    /// `fauna.folders.places.set`'s at-rest half enrols a device, rewrites its
    /// place whole, and refuses a device the actor does not own; removing the
    /// member empties the roster.
    #[tokio::test]
    async fn set_folder_place_flags_writes_the_place_whole() {
        use fauna_protocol::folders::PlaceFlags;
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0xA4; 32];
        let dev = [0xD5; 32];

        db.register_sync_device(&actor, &dev, "Laptop", None, "read,write")
            .await
            .unwrap();
        let fs_id = db.create_folder("music", &actor).await.unwrap();

        db.set_folder_place_flags(fs_id, &actor, &dev, &PlaceFlags::default_place())
            .await
            .unwrap();
        let members = db
            .list_folder_members_with_labels(fs_id, &actor)
            .await
            .unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].device_id, dev.to_vec());
        // S9 flip: the label column rests '' for user-chosen names.
        assert_eq!(members[0].label, "");
        assert_eq!(members[0].flags, PlaceFlags::default_place());

        // A rewrite replaces the point whole — here the receive-only point.
        let receive_only = PlaceFlags::new(false, true, false);
        db.set_folder_place_flags(fs_id, &actor, &dev, &receive_only)
            .await
            .unwrap();
        let members = db
            .list_folder_members_with_labels(fs_id, &actor)
            .await
            .unwrap();
        assert_eq!(members.len(), 1, "a rewrite never duplicates the seat");
        assert_eq!(members[0].flags, receive_only);

        // A device the actor does not own is refused.
        let other_actor = [0xBB; 32];
        assert!(
            db.set_folder_place_flags(fs_id, &other_actor, &dev, &receive_only)
                .await
                .is_err()
        );

        assert!(db.remove_folder_member(fs_id, &dev).await.unwrap());
        assert!(db.get_folder_members(fs_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn is_pure_backup_destination_returns_true_when_mode_backup() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x11u8; 32];
        db.create_folder_with_options(
            "__mail",
            &actor,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(db.is_pure_backup_destination("mail", &actor).await.unwrap());
    }

    #[tokio::test]
    async fn is_pure_backup_destination_returns_false_for_a_rail() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x22u8; 32];
        db.create_folder_with_options(
            "__mail",
            &actor,
            crate::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(!db.is_pure_backup_destination("mail", &actor).await.unwrap());
    }

    /// A re-seeded nest keeps the custody set in `backup` mode after the
    /// materialize, but its scope now holds live records: it is the owner's live
    /// account and must be served, snapshotted and compacted
    /// (`behavior/backup-destinations.md` § Re-seed, "seeded means a live
    /// account"). "Pure" is the corpus shape — opaque chunks, no local segments.
    #[tokio::test]
    async fn a_materialized_backup_set_is_no_longer_a_pure_backup_destination() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x66u8; 32];
        db.create_folder_with_options(
            "__mail",
            &actor,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(db.is_pure_backup_destination("mail", &actor).await.unwrap());

        let cid = fauna_cbor::Cid::from_digest_dag_cbor([0x77u8; 32]);
        db.segment_records_insert_mail(
            &actor,
            1,
            &cid,
            "inbox",
            1,
            "example.org",
            "ham",
            false,
            1,
            None,
            0,
            1,
        )
        .await
        .unwrap();
        assert!(
            !db.is_pure_backup_destination("mail", &actor).await.unwrap(),
            "a custody-copy set whose scope holds live records is a live account"
        );
    }

    #[tokio::test]
    async fn is_pure_backup_destination_returns_false_when_no_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x33u8; 32];
        assert!(!db.is_pure_backup_destination("mail", &actor).await.unwrap());
    }

    #[tokio::test]
    async fn is_pure_backup_destination_isolates_by_actor() {
        // Actor A's __mail is mode=backup; actor B has no __mail row.
        // NOTE: folders.name is globally UNIQUE (schema migration V1), so two
        // different actors cannot each own an '__mail' row in the same DB — that
        // is a latent multi-user bug tracked separately.  This test covers the
        // observable isolation property without triggering the constraint: A has
        // a backup row, B does not — predicate must return false for B.
        let db = CacheDb::open_in_memory().unwrap();
        let a = [0x44u8; 32];
        let b = [0x55u8; 32];
        db.create_folder_with_options(
            "__mail",
            &a,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(db.is_pure_backup_destination("mail", &a).await.unwrap());
        assert!(!db.is_pure_backup_destination("mail", &b).await.unwrap());
    }

    // ── conv (Plan 9): the reserved set encodes the channel in the NAME
    // (`__conv/<channel_hex>`) and scopes by `actor_id = channel_id` — see
    // `get_or_create_reserved_conv_folder` (db/snapshots.rs). The predicate
    // derives the same name from `(kind="conv", scope_id=channel)`.

    #[tokio::test]
    async fn is_pure_backup_destination_conv_returns_true_when_mode_backup() {
        let db = CacheDb::open_in_memory().unwrap();
        let channel = [0x66u8; 32];
        let name = format!("__conv/{}", hex::encode(channel));
        db.create_folder_with_options(
            &name,
            &channel,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            db.is_pure_backup_destination("conv", &channel)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn is_pure_backup_destination_conv_returns_false_when_mode_sync() {
        let db = CacheDb::open_in_memory().unwrap();
        let channel = [0x77u8; 32];
        let name = format!("__conv/{}", hex::encode(channel));
        db.create_folder_with_options(
            &name,
            &channel,
            crate::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            !db.is_pure_backup_destination("conv", &channel)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn is_pure_backup_destination_conv_returns_false_when_no_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let channel = [0x88u8; 32];
        assert!(
            !db.is_pure_backup_destination("conv", &channel)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn is_pure_backup_destination_conv_isolates_by_channel() {
        // Channel C1's `__conv/<C1>` set is mode=backup; C2 has no row. Unlike
        // mail's fixed `__mail`, the conv set name embeds the channel hex so two
        // channels never collide on the global-UNIQUE `folders.name`.
        let db = CacheDb::open_in_memory().unwrap();
        let c1 = [0x99u8; 32];
        let c2 = [0xAAu8; 32];
        let name1 = format!("__conv/{}", hex::encode(c1));
        db.create_folder_with_options(
            &name1,
            &c1,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(db.is_pure_backup_destination("conv", &c1).await.unwrap());
        assert!(!db.is_pure_backup_destination("conv", &c2).await.unwrap());
    }

    /// The row-mapper twin: a wrong-length BLOB column errors — naming the
    /// column and field — rather than silently zero-defaulting, zero-padding,
    /// or panicking.
    #[test]
    fn blob_col_to_array_errors_on_wrong_length_instead_of_defaulting_or_panicking() {
        let ok: [u8; 4] = blob_col_to_array(vec![1, 2, 3, 4], 0, "test_field").unwrap();
        assert_eq!(ok, [1, 2, 3, 4]);

        let err = blob_col_to_array::<4>(vec![1, 2, 3], 0, "test_field").unwrap_err();
        match err {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, msg) => {
                assert!(msg.to_string().contains("test_field"));
            }
            other => panic!("expected FromSqlConversionFailure, got {other:?}"),
        }
    }

    /// The `anyhow::Result` twin, for callers already holding an owned
    /// `Vec<u8>`/`&[u8]` outside a row mapper: same erroring shape, no column
    /// index to report.
    #[test]
    fn blob_to_array_errors_on_wrong_length_instead_of_defaulting_or_panicking() {
        let ok: [u8; 4] = blob_to_array(&[1, 2, 3, 4], "test_field").unwrap();
        assert_eq!(ok, [1, 2, 3, 4]);

        let err = blob_to_array::<4>(&[1, 2, 3], "test_field").unwrap_err();
        assert!(err.to_string().contains("test_field"));
        // The expected length is half the message's value to a reader, and the
        // twin above carries it; asserting it here is what keeps the two the
        // twins their doc comments claim they are.
        assert!(err.to_string().contains("want 4"), "{err}");
    }
}
