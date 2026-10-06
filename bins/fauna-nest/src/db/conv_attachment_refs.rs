//! Conversation attachment references — the conversation kind's
//! blob-reachability floor (`docs/goal/architecture/encryption-at-rest.md`
//! § Per-content-kind conformance → Conversation messages row, ratified
//! 2026-09-08; behavior owner `docs/goal/ui/conversations.md` § Encryption at
//! rest → *Attachment reachability*).
//!
//! A conversation attachment is uploaded through `POST /api/v1/blob` sealed
//! under the channel's `derive_blob_key(epoch_secret)`, and the message that
//! names it is an MLS application envelope the nest cannot open — so without
//! a plaintext reference nothing on the box reaches the blob and the GC
//! sweeps it ~30 minutes after upload. The sender therefore lists the sealed
//! blobs' content addresses beside the envelope
//! (`ChannelSendRequest::attachment_refs`), and `channel_send_core` records
//! them in `conv_attachment_refs` in the same transaction as the record's
//! `segment_records` mirror row (`CacheDb::segment_records_insert_conv`).
//!
//! Read by three consumers. Two ask the same liveness join a different
//! question: the blob GC's step 2g (`backup::gc::collect_reachable_hashes`),
//! which **pins** every hash whose `(channel, seq)` still has a live mirror
//! row; and the legal-takedown blob-serve withhold
//! ([`crate::moderation_withhold`]), which splits those same hashes on the
//! naming record's `legal_takedown_ref` to decide which the blob door must
//! **withhold**. The third asks a per-record question instead
//! ([`CacheDb::conv_attachment_refs`]): a community room's **attachment
//! facet** reads one record's refs to bound which of the addresses its
//! SEALED body names the nest will open for a declaring labeler — the inner list is
//! sealed, so this row is the only plaintext statement the nest ever made
//! about which blobs that message pins. ⚠ For that reader an EMPTY set means
//! "this record made no such statement", never "it pinned nothing": the
//! sender's `attachment_refs` is an additive field it may omit. Pinning is deliberately flag-blind — withheld is not deleted,
//! so a restore re-serves the same bytes. The nest still never serves, relays
//! or opens these hashes — they are routing metadata in the floor's own sense,
//! the same class as a gated post's `GatedInfo::attachment_refs`.

use super::CacheDb;
use anyhow::Result;

impl CacheDb {
    /// Every attachment hash a LIVE conversation record names — the GC's
    /// step 2g source. Liveness is the segment mirror's own (a non-tombstoned
    /// `segment_records` row at the reference's `(channel, seq)`), so a
    /// record the store has let go — relay-acked and purged, or compacted
    /// away — pins nothing and its attachments reclaim with it. Reference
    /// rows are never deleted: over-retaining a row costs bytes, dropping one
    /// on a transient inconsistency is the over-delete direction.
    pub async fn list_live_conv_attachment_refs(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::list_live_conv_attachment_refs(&conn)
    }

    /// The attachment hashes recorded for one `(channel, seq)`, sorted — an
    /// audit / test read, not a serve surface.
    pub async fn conv_attachment_refs(
        &self,
        channel_id: &[u8; 32],
        seq: i64,
    ) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        crate::segments::records_db::conv_attachment_refs_for(&conn, channel_id, seq)
    }
}
