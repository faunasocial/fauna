//! The `__index` rail's WS-RPC plane — `fauna.index.{record,list}`.
//!
//! This is how a **capability-position builder** (a user's client today; the MDA
//! bridge with rollout slice S5) publishes its sealed content-index segments to
//! the user's nest, and how a second device enumerates them to refresh its
//! replica (`docs/goal/behavior/content-index.md` § Ingest triggers, v1).
//!
//! ## Why bytes are NOT on this plane
//!
//! Unlike its structural twin `fauna.drafts.{get,put}`, neither kind here
//! carries blob bytes. A drafts blob is one small snapshot; an index segment is
//! bulk binary, and WS-RPC frames cap at
//! [`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`] (2 MiB) precisely because
//! bulk transfer belongs on the HTTP routes. So the split is:
//!
//! - **bytes** → `PUT /api/v1/blob/<cid>` (and `GET` to read them back);
//! - **the reference** → [`KIND_RECORD`], carrying only `(path, blob hash,
//!   size)`, which the nest turns into a `sync_changes` row so the blob
//!   replicates to the user's other locations.
//!
//! ## Why a dedicated kind rather than `fauna.sync.changes.record`
//!
//! Because that kind is closed to reserved rails, on two independent counts —
//! the nest refuses a non-`backup` reserved set outright, and the GC classifies
//! a reserved-only reference as a **direct blob** it never walks for chunks, so
//! a chunked manifest recorded there would have its chunks swept. Both are
//! stated at the owner site (`content-index.md` § Ingest triggers, v1) and
//! pinned by `conformance_folders.rs`
//! `changes_record_refuses_a_reserved_rail_so_no_client_writer_can_use_it`.
//!
//! **Consequence the writer must honour: one segment is one blob.** There is no
//! chunk-manifest form on this rail, so a builder bounds its segment size at
//! flush rather than spilling to the chunk route.
//!
//! ## Two planes, and why the split is structural rather than a policy choice
//!
//! [`KIND_RECORD`] / [`KIND_LIST`] are the **user plane**: **User-class**
//! (`bridge_method_allowlist.rs`) and *self-scoped* — the nest derives the owning
//! actor from the authenticated connection, so neither request carries an
//! `actor_id` and a caller only ever reaches their own `__index`.
//!
//! [`KIND_BRIDGE_LIST`] is the **bridge plane**: the MDA's index reach,
//! **BridgeMda-class** and *explicitly targeted* — the MDA connects as its own
//! service actor and names the MUA session's actor, exactly as every other
//! MDA-on-behalf-of-user kind does (`fauna.bridges.fetch_mls_snapshot_blob` is
//! the exemplar). **Read-only since the 2026-08-10 carrier ruling**
//! (`content-index.md` § Where the index is built): the MDA's build half is
//! retired, so its write twin `fauna.bridges.index_record` is gone from the
//! router, the registry and this module together — a dark BridgeMda write
//! authorization is not a surface to leave standing.
//!
//! The MDA could not have been admitted to the user plane instead: those kinds
//! have no `actor_id` to name a target with. Keeping the planes separate also
//! makes the inverse abuse unrepresentable — a `User` has no field with which
//! to spoof another actor, because the kind they can reach does not have one.
//!
//! **The bridge plane is mail/calendar-only, gated on the PATH.** Per
//! `content-index.md` § Architectural rules #3 the MDA holds only the
//! MSEK-derived mail/calendar index-segment key, never the cross-kind master key
//! (`key-material-hierarchy.md` § Path B-sibling-4, rule #7's blast-radius
//! invariant). The nest cannot infer a blob's key class from its bytes — both
//! classes use the same seal framing and magic — so it derives the class from
//! the virtual path (`fauna_index::path_key_class`), which is total over every
//! path the rail accepts.
//!
//! The nest stores and lists opaque bytes on both planes — it holds no index key
//! and can never read a segment (`content-index.md` § Encryption posture).

use crate::Value;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

/// WS-RPC kind: record an already-uploaded `__index` blob at a virtual path
/// (User). One source of truth for the client wrapper, the nest router, and the
/// caller-class allowlist.
pub const KIND_RECORD: &str = "fauna.index.record";
/// WS-RPC kind: list the calling actor's live `__index` blobs (User).
pub const KIND_LIST: &str = "fauna.index.list";
/// WS-RPC kind: the MDA enumerates a named actor's live **mail/calendar-class**
/// `__index` blobs (BridgeMda). The bridge-plane twin of [`KIND_LIST`].
pub const KIND_BRIDGE_LIST: &str = "fauna.bridges.index_list";

/// `fauna.index.record` (User) — point `path` at an already-uploaded blob.
///
/// **Ordering contract: upload the bytes first.** The nest verifies it actually
/// holds `blob_hash` before writing the row and refuses otherwise, so a
/// journal row can never reference a blob that does not exist — the invariant
/// the builder's segment-then-manifest publish order rests on.
///
/// Idempotent: recording the same `(path, blob_hash)` twice converges to the
/// same state, so the kind is replay-safe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RecordIndexBlobRequest {
    /// The `__index`-relative virtual path, including the `__index/` prefix —
    /// `fauna_index::paths` (`segment_path` / `manifest_path` /
    /// `mailcal_manifest_path`) is the source of truth for the string, and the
    /// nest validates the shape rather than trusting the writer.
    pub path: String,
    /// Hex blake3 hash of the sealed blob, as returned by the blob upload.
    pub blob_hash: String,
    /// Size of the sealed blob in bytes — what the row meters.
    pub size_bytes: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode (transport.md § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.index.record` reply — the assigned journal sequence number, which is
/// what a device's catch-up pull orders by.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RecordIndexBlobReply {
    pub seq: i64,
    /// Forward-compat catch-all (see [`RecordIndexBlobRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.index.list` (User) — enumerate the calling actor's live `__index`
/// blobs, so a device can refresh its replica: fetch each entry's bytes by hash
/// over the blob route, then open the manifests and segments locally.
///
/// Pure read, replay-safe.
///
/// **Paged (additive, 2026-09-20), the same shape the backup rail already
/// ratified** (`backup::GenerationListRequest`). A rail whose segment-path
/// count is deliberately unbounded (`content-index.md` § Where the index is
/// built) is exactly the read that must not depend on the whole set fitting one
/// frame: past the 2 MiB cap the unpaged reply became *undeliverable*, so a
/// large-enough index could never be refreshed again — a client-reachable dead
/// end. The page is cut at the ratified frame budget rather than a row count,
/// which keeps the fields optional: a first-page caller sends no cursor, and
/// a reply that fits one page carries no `next_cursor`, which ends the walk
/// after that page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListIndexBlobsRequest {
    /// Opaque resume token from the previous page's
    /// [`ListIndexBlobsReply::next_cursor`]. Absent = first page.
    ///
    /// Server-minted and opaque on purpose: only a value a reply handed out is
    /// meaningful, and a malformed one is refused loudly rather than silently
    /// restarting the walk from the beginning.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Max entries for this page. Non-positive/absent = no row bound before the
    /// byte budget, which is what keeps a cursor-less caller's view at least as
    /// complete as any reply the frame could ever have delivered.
    #[serde(default)]
    pub limit: i64,
    /// Forward-compat catch-all (see [`RecordIndexBlobRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One live `__index` path and the blob it currently points at.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct IndexBlobEntry {
    /// The `__index`-relative virtual path, as recorded.
    pub path: String,
    /// Hex blake3 hash of the sealed blob.
    pub blob_hash: String,
    pub size_bytes: i64,
    /// Forward-compat catch-all (see [`RecordIndexBlobRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.index.list` reply. Empty for an actor that has never published — the
/// first-run state, which the builder starts from with a fresh manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ListIndexBlobsReply {
    pub entries: Vec<IndexBlobEntry>,
    /// Present iff more entries remain past this page — pass it back as
    /// [`ListIndexBlobsRequest::cursor`]. Absent on the final page (including a reply
    /// that fits one page).
    ///
    /// **A page may be empty while this is present.** The bridge plane filters
    /// master-class entries out *after* the page is read, so a page of nothing
    /// but master-class rows serves nothing and still advances — walk to the
    /// cursor's absence, never to the first empty page.
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// Forward-compat catch-all (see [`RecordIndexBlobRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.index_list` (BridgeMda) — the MDA enumerates `actor_id`'s live
/// **mail/calendar-class** `__index` blobs, so it can open the manifest and the
/// segments its session key covers.
///
/// The reply is [`ListIndexBlobsReply`], filtered to that class: master-class
/// entries are omitted rather than refused, because the MDA holds no key for
/// them — and withholding them keeps the user's per-kind segment counts away
/// from a MUA-credential-reachable position.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BridgeListIndexBlobsRequest {
    /// The actor whose `__index` set to enumerate.
    pub actor_id: ByteBuf,
    /// Opaque resume token from the previous page's
    /// [`ListIndexBlobsReply::next_cursor`]. Absent = first page. Same paging
    /// contract as the user plane's [`ListIndexBlobsRequest::cursor`] — see it
    /// for why the page is cut at the frame budget and why an empty page with a
    /// cursor is normal *here in particular*.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Max entries read for this page, before the mail/calendar-class filter.
    /// Non-positive/absent = no row bound before the byte budget.
    #[serde(default)]
    pub limit: i64,
    /// Forward-compat catch-all (see [`RecordIndexBlobRequest::extra`]).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}
