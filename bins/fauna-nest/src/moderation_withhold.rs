//! Which blobs a legal takedown withholds from the blob-serve door.
//!
//! Owner: `docs/goal/behavior/moderation.md` § Legal takedown → *The
//! blob-serve door*.
//!
//! A takedown withholds a record's **body** at the read primitive every
//! per-record serve path flows through (`segments::conv::read_after_seq` for
//! conversations, `get_post_core` for posts). Its **attachments** are not in
//! that body: they are separate content-addressed blobs served by the
//! unauthenticated `GET /api/v1/blob/{id}`, named by a hash every recipient
//! already learned inside the message they received. Until this module the
//! door consulted no flag, so a compelled withhold blanked the text and kept
//! relaying the picture.
//!
//! # The predicate
//!
//! A blob is withheld **iff every live record that names it is under a legal
//! takedown**. The exemption is not decoration: a takedown is a legally
//! compelled act against *one record*, and its transparency triple — tombstone,
//! appeal, audit row — is per-record. Withholding a blob a *different*, live,
//! unflagged record also stands behind would break that record's media with no
//! tombstone, no appeal handle and no audit row, which is exactly the silent
//! discretionary removal `moderation.md` § Legal takedown is built to make
//! structurally impossible.
//!
//! How a blob comes to be shared at all differs by kind, and it is worth
//! knowing which cases the exemption can actually fire for: **public post
//! media** dedups by content address, so two posts carrying byte-identical
//! bytes really do name one blob; a **gated** post's attachments are sealed
//! per post (`encryption-at-rest.md` § Per-content-kind conformance → Posts),
//! so two gated posts can never share a sealed cid; and a conversation
//! attachment is sealed under the channel epoch key, so sharing is confined to
//! one channel re-listing a cid it already uploaded.
//!
//! Both sides of that difference move as records come and go, so the set is
//! **recomputed, never edited**:
//!
//! - the takedown/restore handler recomputes synchronously once the flag
//!   transaction has committed, so the door is correct the moment the admin's
//!   confirm returns; and
//! - every blob-GC sweep recomputes it from the walk it already performs,
//!   which is what keeps it true afterwards — a new record naming a withheld
//!   blob releases it, and the removal of the last unflagged record naming a
//!   shared blob withholds it. Only a walk that read every live record may
//!   release, though; see [`WithholdSets::store`].
//!
//! The alternative — a hook on every record write and delete on the box — was
//! rejected: a hook that is ever missed drifts in the direction that keeps
//! serving legally compelled bytes, and there is no cheap reverse index from a
//! blob back to the posts naming it (the GC walks live records rather than a
//! reference table, deliberately — `backup-restore.md` § 9 step 2f).
//!
//! # Why not ask the question at the door
//!
//! `GET /api/v1/blob/{id}` is unauthenticated and hot. The conversation half
//! *would* answer in SQL (`conv_attachment_refs` is a real reverse index), but
//! the post half has none — answering it per request means decoding every
//! stored post, i.e. handing an anonymous caller a full-store walk per GET.
//! So the walk is paid where it belongs: on a rare admin act, and on a sweep
//! that already walks.

use crate::db::CacheDb;
use anyhow::{Context, Result};
use std::collections::HashSet;

/// The two sides of the withhold predicate, accumulated over a walk of every
/// live record on the box.
///
/// Records are folded in one at a time so the blob GC can feed this from the
/// post walk it already runs (no second decode pass) while the takedown
/// handler feeds it from a walk of its own — one predicate, two callers.
#[derive(Default, Debug)]
pub struct WithholdSets {
    /// Blobs named by a live record that IS under a legal takedown.
    flagged: HashSet<[u8; 32]>,
    /// Blobs named by a live record that is NOT under a legal takedown.
    unflagged: HashSet<[u8; 32]>,
}

impl WithholdSets {
    /// Fold in one live record's blob references.
    ///
    /// `taken_down` is the naming record's own flag — `content_meta`'s for a
    /// post, `segment_records`' for a conversation record.
    pub fn add_record(&mut self, taken_down: bool, refs: impl IntoIterator<Item = [u8; 32]>) {
        let side = if taken_down {
            &mut self.flagged
        } else {
            &mut self.unflagged
        };
        side.extend(refs);
    }

    /// Fold in the conversation half — every attachment hash a live conv
    /// record names, split on that record's own takedown flag.
    pub async fn add_conversations(&mut self, db: &CacheDb) -> Result<()> {
        for (hash, taken_down) in db
            .conv_attachment_refs_by_takedown()
            .await
            .context("reading live conversation attachment refs by takedown")?
        {
            self.add_record(taken_down, [hash]);
        }
        Ok(())
    }

    /// Fold in every blob named by a post its author **deleted while taken
    /// down** (`moderation.md` § Legal takedown → *Posts*).
    ///
    /// These cannot come from the record walk, and the reason is the whole
    /// point of the marker: such a record is tombstoned, so `load_post_body`
    /// answers `None` for it and the walk `continue`s past it — and its
    /// `content` row is gone, so the walk never enumerates it in the first
    /// place. The digests were therefore captured inside the delete, the last
    /// moment they were readable.
    ///
    /// They land on the **flagged** side only, never the unflagged one: a
    /// deleted record is not a live record, so it exempts nothing. A blob a
    /// live unflagged record also names still keeps serving, exactly as the
    /// per-record exemption says — that record puts the digest in
    /// `unflagged`, and [`Self::resolve`]'s set-difference takes it back out.
    pub async fn add_deleted_taken_down_posts(&mut self, db: &CacheDb) -> Result<()> {
        for digest in db
            .taken_down_deleted_post_blob_digests()
            .await
            .context("reading blob digests of posts deleted while taken down")?
        {
            self.add_record(true, [digest]);
        }
        Ok(())
    }

    /// The withheld set: named by a taken-down record, named by nothing live
    /// that is not taken down. Sorted, so a rebuild is byte-stable and two
    /// runs over one store are trivially comparable in a test.
    pub fn resolve(&self) -> Vec<[u8; 32]> {
        let mut out: Vec<[u8; 32]> = self.flagged.difference(&self.unflagged).copied().collect();
        out.sort_unstable();
        out
    }

    /// Store this walk's verdict. Returns the number of blobs written.
    ///
    /// `complete` is the ONE thing a caller must get right: it says whether
    /// this walk read every live record on the box. A complete walk **replaces**
    /// the set, which is what lets a restore, a new record naming a withheld
    /// blob, or the deletion of a shared blob's last unflagged namer all take
    /// effect. An incomplete one may only **add** — it cannot know which blobs
    /// the records it missed name, so releasing from it could serve bytes a
    /// takedown compelled off the box, and that is the one direction this
    /// mechanism may never fail in. Over-withholding is corrected by the next
    /// complete walk; under-withholding is corrected by nothing.
    pub async fn store(&self, db: &CacheDb, complete: bool) -> Result<usize> {
        let withheld = self.resolve();
        if complete {
            db.replace_blob_legal_withhold(&withheld)
                .await
                .context("replacing the legal-takedown blob withhold set")
        } else {
            db.add_blob_legal_withhold(&withheld)
                .await
                .context("extending the legal-takedown blob withhold set")
        }
    }
}

/// Recompute the withheld set from a full walk of every live record, and
/// replace the stored set with it. Returns the number of withheld blobs.
///
/// This is the takedown/restore handler's leg: one walk on a rare, compelled
/// admin act, so the blob-serve door is already correct when the admin's
/// confirm returns rather than at the next sweep. The blob GC's leg feeds
/// [`WithholdSets`] from its own walk instead (`backup::gc`), so a sweep never
/// decodes the store twice.
///
/// A post whose body cannot be READ is skipped with a warning and the rebuild
/// still runs — but as an ADD, not a replace ([`WithholdSets::store`]): the
/// takedown that triggered this call must take effect even on a box with one
/// unreadable record, and refusing outright would leave the previous set
/// standing, which after a takedown is the set that has not withheld anything
/// yet. A post that reads but does not DECODE named no blob this nest could
/// serve (`backup-restore.md` § 9 step 2f), so it contributes nothing either
/// way and does not make the walk incomplete.
pub async fn recompute(
    db: &CacheDb,
    posts: crate::backup::gc::PostBodySource<'_>,
) -> Result<usize> {
    let mut sets = WithholdSets::default();
    let mut complete = true;
    let taken_down = db
        .taken_down_post_ids()
        .await
        .context("listing taken-down post ids for the withhold rebuild")?;
    let post_ids = db
        .list_all_post_ids()
        .await
        .context("listing post ids for the withhold rebuild")?;
    for post_id in &post_ids {
        let body = match crate::segments::post::load_post_body(posts.segments, db, post_id).await {
            Ok(Some(body)) => body,
            // No live body: tombstoned, or a mirror row whose segment record
            // is gone. Nothing this post can still serve, so it neither
            // withholds nor exempts a blob.
            Ok(None) => continue,
            Err(e) => {
                complete = false;
                tracing::warn!(
                    post = hex::encode(post_id),
                    error = %e,
                    "failed to READ a post body while rebuilding the legal-takedown blob \
                     withhold — this rebuild may only ADD, never release"
                );
                continue;
            }
        };
        if let Some(post) = crate::db::posts::decode_stored_post(&body) {
            let flagged = taken_down.contains(post_id);
            sets.add_record(flagged, post.blob_refs().into_iter().map(|h| h.digest()));
        }
    }
    sets.add_conversations(db).await?;
    sets.add_deleted_taken_down_posts(db).await?;
    sets.store(db, complete).await
}

#[cfg(test)]
mod tests {
    use super::WithholdSets;

    fn h(b: u8) -> [u8; 32] {
        [b; 32]
    }

    /// The predicate, stated as arithmetic: withheld = named-by-a-flagged-record
    /// MINUS named-by-anything-live-and-unflagged. Order of folding must not
    /// matter — the GC feeds posts then conversations, the handler's rebuild
    /// does the same, and a record's own arrival order in the walk is
    /// whatever `list_all_post_ids` returns.
    #[test]
    fn withheld_is_the_flagged_set_minus_every_live_unflagged_namer() {
        let only_flagged = h(1);
        let shared = h(2);
        let only_live = h(3);

        let mut a = WithholdSets::default();
        a.add_record(true, [only_flagged, shared]);
        a.add_record(false, [shared, only_live]);
        assert_eq!(a.resolve(), vec![only_flagged]);

        // Same facts, opposite fold order.
        let mut b = WithholdSets::default();
        b.add_record(false, [shared, only_live]);
        b.add_record(true, [only_flagged, shared]);
        assert_eq!(b.resolve(), vec![only_flagged]);
    }

    /// Two flagged records naming one blob withhold it once, and a blob no
    /// flagged record names is never withheld — the door must stay open for
    /// every blob a takedown does not reach.
    #[test]
    fn nothing_is_withheld_without_a_flagged_namer() {
        let mut sets = WithholdSets::default();
        sets.add_record(false, [h(4), h(5)]);
        assert!(sets.resolve().is_empty());

        let dup = h(6);
        let mut sets = WithholdSets::default();
        sets.add_record(true, [dup]);
        sets.add_record(true, [dup]);
        assert_eq!(sets.resolve(), vec![dup]);
    }

    /// An INCOMPLETE walk may add but must never release. The failure this
    /// pins is the one that matters: a walk that could not read the record
    /// holding a blob's only takedown would compute an empty verdict, and a
    /// replace on that basis puts legally compelled bytes back on the wire
    /// with nothing left to notice. A complete walk is what releases.
    #[tokio::test]
    async fn an_incomplete_walk_adds_but_never_releases() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let already = h(7);
        let newly = h(8);
        db.replace_blob_legal_withhold(&[already]).await.unwrap();

        let mut partial = WithholdSets::default();
        partial.add_record(true, [newly]);
        partial.store(&db, false).await.unwrap();
        assert!(
            db.blob_is_legally_withheld(&already).await.unwrap(),
            "an incomplete walk released a blob it never even saw"
        );
        assert!(
            db.blob_is_legally_withheld(&newly).await.unwrap(),
            "an incomplete walk must still withhold what it DID see flagged"
        );

        // The complete walk is the only thing that releases.
        WithholdSets::default().store(&db, true).await.unwrap();
        assert!(!db.blob_is_legally_withheld(&already).await.unwrap());
        assert!(!db.blob_is_legally_withheld(&newly).await.unwrap());
    }
}
