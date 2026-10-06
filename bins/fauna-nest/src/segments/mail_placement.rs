//! `MailPlacementSegmentManager` — per-actor coordinator for the
//! `__mail-placement` segment store.
//!
//! The manager itself is not here: all three placement journals ARE
//! `super::PlacementJournal`, and this module supplies only the
//! `super::PlacementKind` binding — the paths, the two on-disk name spellings,
//! and `apply_record`. What genuinely differs per kind, and stays here, is the
//! record semantics: `apply_record_to_manifest` and the `update_modseq`
//! helper it drives.
//!
//! Two properties the shared manager keeps and this module must not re-derive:
//! placement records have no SQLite mirror table (the manifest's compacted
//! current state is the only secondary index, and the manifest is a cache —
//! the segments are the durable source of truth) and no per-record floor blob
//! (the record IS the data, so `FramedSegmentStore::append` gets `&[]` for
//! floor_metadata).
//!
//! See the IMAP/CalDAV restore design (tracked internally) § D3 (manifest
//! holds compacted current state).

use anyhow::Result;
use fauna_mail::segments::placement::{
    MailPlacementManifest, MailPlacementRecord, MailboxState, RecordPlacement, TombstoneRef,
    mail_placement_manifest_path, mail_placement_segments_root,
};
use fauna_segment_store::SegmentStoreError;
use std::path::{Path, PathBuf};

/// The mailbox placement journal's kind binding: the six mechanical items
/// plus the one that is not (`apply_record`). The manager itself is
/// [`super::PlacementJournal`], written once for all three placement kinds.
pub struct MailPlacementKind;

/// The mailbox placement journal.
pub type MailPlacementSegmentManager = super::PlacementJournal<MailPlacementKind>;

impl super::PlacementKind for MailPlacementKind {
    type Manifest = MailPlacementManifest;
    type Record = MailPlacementRecord;
    type Tombstone = TombstoneRef;

    const SCOPE_KIND: &'static str = "mail-placement";

    fn segments_root(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
        mail_placement_segments_root(data_dir, actor_id)
    }

    fn manifest_path(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
        mail_placement_manifest_path(data_dir, actor_id)
    }

    fn encode_record(record: &Self::Record) -> Result<Vec<u8>, SegmentStoreError> {
        record.encode()
    }

    fn rebuild_from_segments(root: &Path) -> Result<Self::Manifest> {
        rebuild_mail_placement_manifest_from_segments(root)
    }

    fn kind_manifest_mut(m: &mut Self::Manifest) -> &mut fauna_segment_store::KindManifest {
        &mut m.kind_manifest
    }

    fn tombstones_mut(m: &mut Self::Manifest) -> &mut Vec<Self::Tombstone> {
        &mut m.tombstones
    }

    fn apply_record(record: &Self::Record, m: &mut Self::Manifest) {
        apply_record_to_manifest(record, m);
    }

    fn tombstone_deleted_at(t: &Self::Tombstone) -> Option<i64> {
        Some(t.deleted_at)
    }
}

/// The mail journal is part of the backed-up mail corpus, so a rebuilt nest can
/// adopt one (`backup-destinations.md` § Third destination kind → *Where
/// restored mail lands*).
impl super::AdoptableJournal for MailPlacementKind {
    /// Filed messages, removed ones, and every UID a mailbox has handed out.
    ///
    /// The UID term is what keeps an *emptied* account from reading as a fresh
    /// one: tombstones are pruned after their retention window, but a
    /// mailbox's `uid_next` never goes back, and a restore that reused a spent
    /// UID would hand a mail client another message under a number it has
    /// already cached.
    fn held_history(fold: &MailPlacementManifest) -> u64 {
        let spent_uids: u64 = fold
            .mailboxes
            .iter()
            .map(|m| u64::from(m.uid_next.saturating_sub(1)))
            .sum();
        fold.placements.len() as u64 + fold.tombstones.len() as u64 + spent_uids
    }

    /// The same filed messages, and no tombstone the corpus does not have.
    ///
    /// Mailboxes and subscriptions are deliberately not compared: a mail client
    /// that connects between an interrupted run and its retry creates or
    /// subscribes to a folder, and that must not turn this ceremony's own
    /// adoption into a lived-in account it then refuses for ever. Tombstones
    /// are compared one way only for the matching reason — the retention prune
    /// may have dropped some since.
    fn is_adoption_of(current: &MailPlacementManifest, corpus: &MailPlacementManifest) -> bool {
        current.placements == corpus.placements
            && current
                .tombstones
                .iter()
                .all(|t| corpus.tombstones.contains(t))
    }
}

/// Rebuild a `MailPlacementManifest` by replaying every finalized journal
/// segment under `root` in order (ascending segment id = append order; the
/// per-actor lock serializes appends and buckets rotate monotonically).
/// Each frame decodes strictly as the current [`MailPlacementRecord`] (the
/// pre-sweep v1 frame fallback was retired by the compat-remnant sweep,
/// 2026-09-24).
///
/// Best-effort about exactly ONE thing: a crashed, never-finalized `.dat` (no
/// `.meta` sidecar) cannot be opened and is skipped with a warning — bounded
/// to the records appended after the last finalize, and strictly better than
/// the alternative (a corrupt manifest bricking the actor's mail writes
/// forever). Restore-design spec § D3 is the authority for this path.
///
/// Every OTHER way a segment can refuse to open fails closed, matching the `?`
/// the record decode below already uses: `FramedSegment::open` has eight
/// failure exits and seven of them (damaged `.meta`, damaged `.dat`, either
/// I/O error, drifted CARv2 block count, corrupt `floor_metadata`,
/// `SchemaMismatch`) are reachable on a **middle** segment, where "crashed
/// before finalize" is simply false and a skip silently drops delivered mail
/// and resurrects expunged mail — permanently, because the rebuilt manifest is
/// then persisted and never rebuilds again. The policy and its reasoning live
/// in [`super::open_replay_segment`].
///
/// The cal/card twin of this function predates it by six weeks; mail — the
/// kind the twins were copied *from* — had no rebuild at all until
/// 2026-08-23, so an undecodable manifest failed every append, read and
/// prune for that actor permanently.
pub(crate) fn rebuild_mail_placement_manifest_from_segments(
    root: &Path,
) -> Result<MailPlacementManifest> {
    let mut manifest = MailPlacementManifest::new();
    let files = super::list_segment_files_for_replay(root)?;
    // The crash tail is the highest id present — the ONLY unopenable segment
    // this replay may skip (`super::open_replay_segment`).
    let highest_id = files.last().map(|(id, _)| *id);
    for (seg_id, path) in files {
        let Some(seg) = super::open_replay_segment(
            "mail-placement",
            seg_id,
            &path,
            Some(seg_id) == highest_id,
        )?
        else {
            continue;
        };
        for entry in seg.iter_records() {
            let bytes = seg
                .read_record(&entry.cid)?
                .ok_or_else(|| anyhow::anyhow!("segment {seg_id} index lists a missing record"))?;
            let record = MailPlacementRecord::decode(&bytes).map_err(|e| {
                anyhow::anyhow!("decode mail-placement record in seg {seg_id}: {e}")
            })?;
            apply_record_to_manifest(&record, &mut manifest);
        }
        manifest.kind_manifest.live_segments.push(seg_id);
        manifest.kind_manifest.next_seg_id = manifest.kind_manifest.next_seg_id.max(seg_id + 1);
    }
    Ok(manifest)
}

/// Apply one placement record to the in-memory compacted manifest state.
/// Handles all 10 record variants per spec § D3.
fn apply_record_to_manifest(record: &MailPlacementRecord, m: &mut MailPlacementManifest) {
    match record {
        MailPlacementRecord::Create {
            mailbox,
            uid_validity,
            attrs,
        } => {
            m.mailboxes.retain(|s| s.name != *mailbox);
            m.mailboxes.push(MailboxState {
                name: mailbox.clone(),
                uid_validity: *uid_validity,
                uid_next: 1,
                highestmodseq: 1,
                attrs: attrs.clone(),
            });
        }
        MailPlacementRecord::Delete { mailbox } => {
            m.mailboxes.retain(|s| s.name != *mailbox);
            m.placements.retain(|p| p.mailbox != *mailbox);
            m.tombstones.retain(|t| t.mailbox != *mailbox);
        }
        MailPlacementRecord::Rename { old, new } => {
            for s in &mut m.mailboxes {
                if s.name == *old {
                    s.name = new.clone();
                }
            }
            for p in &mut m.placements {
                if p.mailbox == *old {
                    p.mailbox = new.clone();
                }
            }
            for t in &mut m.tombstones {
                if t.mailbox == *old {
                    t.mailbox = new.clone();
                }
            }
        }
        MailPlacementRecord::Append {
            mailbox,
            uid,
            modseq,
            flags,
            content_record_id,
            internal_date,
        } => {
            m.placements.push(RecordPlacement {
                mailbox: mailbox.clone(),
                uid: *uid,
                modseq: *modseq,
                flags: flags.clone(),
                content_record_id: content_record_id.clone(),
                internal_date: *internal_date,
            });
            if let Some(s) = m.mailboxes.iter_mut().find(|s| s.name == *mailbox) {
                s.uid_next = (*uid + 1).max(s.uid_next);
                s.highestmodseq = (*modseq).max(s.highestmodseq);
            }
        }
        MailPlacementRecord::StoreFlags {
            mailbox,
            uid_set,
            modseq,
            after_flags,
            ..
        } => {
            for p in &mut m.placements {
                if p.mailbox == *mailbox && uid_set.contains(&p.uid) {
                    p.flags = after_flags.clone();
                    p.modseq = *modseq;
                }
            }
            update_modseq(m, mailbox, *modseq);
        }
        MailPlacementRecord::Move {
            src_mailbox,
            src_uid_set,
            dst_mailbox,
            dst_uid_set,
            modseq_src,
            modseq_dst,
            deleted_at,
        } => {
            for (i, src_uid) in src_uid_set.iter().enumerate() {
                if let Some(pos) = m
                    .placements
                    .iter()
                    .position(|p| p.mailbox == *src_mailbox && p.uid == *src_uid)
                {
                    let mut moved = m.placements.remove(pos);
                    moved.mailbox = dst_mailbox.clone();
                    moved.uid = dst_uid_set[i];
                    moved.modseq = *modseq_dst;
                    m.placements.push(moved);
                }
                // QRESYNC needs VANISHED to surface even when our
                // manifest is drifted vs the MUA — push the tombstone
                // unconditionally.
                m.tombstones.push(TombstoneRef {
                    mailbox: src_mailbox.clone(),
                    uid: *src_uid,
                    modseq: *modseq_src,
                    deleted_at: *deleted_at,
                });
            }
            update_modseq(m, src_mailbox, *modseq_src);
            update_modseq(m, dst_mailbox, *modseq_dst);
        }
        MailPlacementRecord::Copy {
            src_mailbox,
            src_uid_set,
            dst_mailbox,
            dst_uid_set,
            modseq_dst,
        } => {
            for (i, src_uid) in src_uid_set.iter().enumerate() {
                if let Some(src) = m
                    .placements
                    .iter()
                    .find(|p| p.mailbox == *src_mailbox && p.uid == *src_uid)
                    .cloned()
                {
                    m.placements.push(RecordPlacement {
                        mailbox: dst_mailbox.clone(),
                        uid: dst_uid_set[i],
                        modseq: *modseq_dst,
                        flags: src.flags.clone(),
                        content_record_id: src.content_record_id.clone(),
                        internal_date: src.internal_date,
                    });
                }
            }
            update_modseq(m, dst_mailbox, *modseq_dst);
        }
        MailPlacementRecord::Expunge {
            mailbox,
            uid_set,
            modseq,
            deleted_at,
        } => {
            m.placements
                .retain(|p| !(p.mailbox == *mailbox && uid_set.contains(&p.uid)));
            for uid in uid_set {
                m.tombstones.push(TombstoneRef {
                    mailbox: mailbox.clone(),
                    uid: *uid,
                    modseq: *modseq,
                    deleted_at: *deleted_at,
                });
            }
            update_modseq(m, mailbox, *modseq);
        }
        MailPlacementRecord::Subscribe { mailbox } => {
            if !m.subscriptions.contains(mailbox) {
                m.subscriptions.push(mailbox.clone());
            }
        }
        MailPlacementRecord::Unsubscribe { mailbox } => {
            m.subscriptions.retain(|s| s != mailbox);
        }
    }
}

fn update_modseq(m: &mut MailPlacementManifest, mailbox: &str, modseq: u64) {
    if let Some(s) = m.mailboxes.iter_mut().find(|s| s.name == mailbox) {
        s.highestmodseq = modseq.max(s.highestmodseq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mail::segments::bucket_for;
    use fauna_segment_store::{FramedSegmentStore, VersionedManifest};
    use tempfile::TempDir;

    #[tokio::test]
    async fn append_then_current_manifest_round_trips() {
        let tmp = TempDir::new().expect("tmp");
        let manager = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x77u8; 32];

        let seg = manager
            .append_event(
                &actor,
                &MailPlacementRecord::Create {
                    mailbox: "INBOX".to_string(),
                    uid_validity: 1,
                    attrs: vec!["\\Inbox".to_string()],
                },
            )
            .await
            .expect("append create");
        assert_eq!(seg, 1);

        let _seg = manager
            .append_event(
                &actor,
                &MailPlacementRecord::Append {
                    mailbox: "INBOX".to_string(),
                    uid: 42,
                    modseq: 100,
                    flags: vec!["\\Seen".to_string()],
                    content_record_id: vec![1, 2, 3],
                    internal_date: 1_715_000_000,
                },
            )
            .await
            .expect("append message");

        let manifest = manager.current_manifest(&actor).await.expect("current");
        assert_eq!(manifest.mailboxes.len(), 1);
        assert_eq!(manifest.mailboxes[0].name, "INBOX");
        assert_eq!(manifest.placements.len(), 1);
        assert_eq!(manifest.placements[0].uid, 42);
        assert!(manifest.placements[0].flags.contains(&"\\Seen".to_string()));
    }

    #[tokio::test]
    async fn expunge_drops_placement_and_adds_tombstone() {
        let tmp = TempDir::new().expect("tmp");
        let manager = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x88u8; 32];
        manager
            .append_event(
                &actor,
                &MailPlacementRecord::Create {
                    mailbox: "Trash".to_string(),
                    uid_validity: 1,
                    attrs: vec![],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &MailPlacementRecord::Append {
                    mailbox: "Trash".to_string(),
                    uid: 5,
                    modseq: 10,
                    flags: vec![],
                    content_record_id: vec![],
                    internal_date: 0,
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &MailPlacementRecord::Expunge {
                    mailbox: "Trash".to_string(),
                    uid_set: vec![5],
                    modseq: 11,
                    deleted_at: 1_752_000_000,
                },
            )
            .await
            .unwrap();
        let m = manager.current_manifest(&actor).await.unwrap();
        assert!(m.placements.iter().all(|p| p.uid != 5));
        assert_eq!(m.tombstones.len(), 1);
        assert_eq!(m.tombstones[0].uid, 5);
        // Tombstone modseq is load-bearing for QRESYNC VANISHED.
        assert_eq!(m.tombstones[0].modseq, 11);
        assert_eq!(m.tombstones[0].deleted_at, 1_752_000_000);
    }

    #[tokio::test]
    async fn move_relocates_placement_and_records_src_tombstone() {
        let tmp = TempDir::new().expect("tmp");
        let manager = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xaau8; 32];
        for mb in ["INBOX", "Archive"] {
            manager
                .append_event(
                    &actor,
                    &MailPlacementRecord::Create {
                        mailbox: mb.to_string(),
                        uid_validity: 1,
                        attrs: vec![],
                    },
                )
                .await
                .unwrap();
        }
        manager
            .append_event(
                &actor,
                &MailPlacementRecord::Append {
                    mailbox: "INBOX".to_string(),
                    uid: 7,
                    modseq: 20,
                    flags: vec![],
                    content_record_id: vec![1, 2, 3],
                    internal_date: 1_715_000_000,
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &MailPlacementRecord::Move {
                    src_mailbox: "INBOX".to_string(),
                    src_uid_set: vec![7],
                    dst_mailbox: "Archive".to_string(),
                    dst_uid_set: vec![1],
                    modseq_src: 21,
                    modseq_dst: 22,
                    deleted_at: 1_752_000_100,
                },
            )
            .await
            .unwrap();
        let m = manager.current_manifest(&actor).await.unwrap();
        let inbox_placements: Vec<_> = m
            .placements
            .iter()
            .filter(|p| p.mailbox == "INBOX")
            .collect();
        assert!(inbox_placements.is_empty(), "src placement removed");
        let archive: Vec<_> = m
            .placements
            .iter()
            .filter(|p| p.mailbox == "Archive")
            .collect();
        assert_eq!(archive.len(), 1);
        assert_eq!(archive[0].uid, 1);
        assert_eq!(archive[0].modseq, 22);
        assert_eq!(archive[0].content_record_id, vec![1, 2, 3]);
        assert_eq!(m.tombstones.len(), 1);
        assert_eq!(m.tombstones[0].mailbox, "INBOX");
        assert_eq!(m.tombstones[0].uid, 7);
        assert_eq!(m.tombstones[0].modseq, 21);
        assert_eq!(
            m.tombstones[0].deleted_at, 1_752_000_100,
            "a Move tombstone carries the record's move time"
        );

        // Same retention as a delete: the Move tombstone ages out once its
        // stamp is past the cutoff, and not before.
        assert_eq!(
            manager
                .prune_tombstones(&actor, 1_752_000_100)
                .await
                .unwrap(),
            0,
            "a Move tombstone survives until its window passes"
        );
        assert_eq!(
            manager
                .prune_tombstones(&actor, 1_752_000_101)
                .await
                .unwrap(),
            1,
            "a Move tombstone prunes under the same retention as a delete"
        );
        let m = manager.current_manifest(&actor).await.unwrap();
        assert!(m.tombstones.is_empty());
    }

    #[tokio::test]
    async fn duplicate_subscribe_does_not_collide_on_record_id() {
        // Two identical Subscribe events must both succeed at the
        // segment level — `record_id` is sequenced, not content-hashed.
        let tmp = TempDir::new().expect("tmp");
        let manager = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xbbu8; 32];
        for _ in 0..2 {
            manager
                .append_event(
                    &actor,
                    &MailPlacementRecord::Subscribe {
                        mailbox: "Folder".to_string(),
                    },
                )
                .await
                .expect("subscribe does not collide");
        }
        let m = manager.current_manifest(&actor).await.unwrap();
        assert_eq!(m.subscriptions, vec!["Folder".to_string()]);
    }

    #[tokio::test]
    async fn manifest_persists_to_disk() {
        let tmp = TempDir::new().expect("tmp");
        let manager = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x99u8; 32];
        manager
            .append_event(
                &actor,
                &MailPlacementRecord::Subscribe {
                    mailbox: "Folder".to_string(),
                },
            )
            .await
            .unwrap();
        // New manager pointing at the same dir loads the persisted
        // manifest from disk.
        let manager2 = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        let m = manager2.current_manifest(&actor).await.unwrap();
        assert_eq!(m.subscriptions, vec!["Folder".to_string()]);
    }

    /// The prune drops exactly the tombstones whose recorded delete time is
    /// past the cutoff; a fresh one survives until the window passes.
    #[tokio::test]
    async fn prune_drops_only_expired_stamped_tombstones() {
        let tmp = TempDir::new().expect("tmp");
        let manager = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xC4u8; 32];
        manager
            .update_manifest(&actor, |m| {
                for (uid, deleted_at) in [
                    (61u32, 1_000),     // long expired
                    (62u32, 2_000_000), // fresh
                ] {
                    m.tombstones.push(TombstoneRef {
                        mailbox: "INBOX".to_string(),
                        uid,
                        modseq: 1,
                        deleted_at,
                    });
                }
                true
            })
            .await
            .unwrap();

        let pruned = manager.prune_tombstones(&actor, 1_000_000).await.unwrap();
        assert_eq!(pruned, 1, "exactly the expired stamped tombstone");
        let m = manager.current_manifest(&actor).await.unwrap();
        let uids: Vec<u32> = m.tombstones.iter().map(|t| t.uid).collect();
        assert!(!uids.contains(&61), "expired pruned");
        assert!(uids.contains(&62), "fresh kept");

        // Persisted: a fresh manager sees the pruned set.
        let manager2 = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        assert_eq!(
            manager2
                .current_manifest(&actor)
                .await
                .unwrap()
                .tombstones
                .len(),
            1
        );
    }

    /// Seed a one-segment journal: a mailbox, three appends, and an
    /// `Expunge` of uid 2 between them.
    async fn seed_journal(data_dir: &std::path::Path, actor: &[u8; 32]) {
        let root = mail_placement_segments_root(data_dir, actor);
        let mut store = FramedSegmentStore::new(root, "mail-placement", *actor).expect("store");
        let bucket = bucket_for(super::super::placement_now_secs());
        let frames: Vec<Vec<u8>> = [
            MailPlacementRecord::Create {
                mailbox: "INBOX".into(),
                uid_validity: 7,
                attrs: vec!["\\Inbox".into()],
            },
            MailPlacementRecord::Append {
                mailbox: "INBOX".into(),
                uid: 1,
                modseq: 1,
                flags: vec!["\\Seen".into()],
                content_record_id: vec![0xA1],
                internal_date: 1_000,
            },
            MailPlacementRecord::Append {
                mailbox: "INBOX".into(),
                uid: 2,
                modseq: 2,
                flags: vec![],
                content_record_id: vec![0xA2],
                internal_date: 2_000,
            },
            MailPlacementRecord::Expunge {
                mailbox: "INBOX".into(),
                uid_set: vec![2],
                modseq: 3,
                deleted_at: 1_752_000_000,
            },
            MailPlacementRecord::Append {
                mailbox: "INBOX".into(),
                uid: 3,
                modseq: 9,
                flags: vec!["\\Flagged".into()],
                content_record_id: vec![0xA3],
                internal_date: 3_000,
            },
        ]
        .iter()
        .map(|r| r.encode().unwrap())
        .collect();
        for (seq, bytes) in frames.iter().enumerate() {
            store
                .append(
                    &bucket,
                    1,
                    super::super::compute_placement_cid(seq as u64, bytes),
                    bytes,
                    &[],
                )
                .expect("append frame");
        }
        store.finalize_open().expect("finalize");
    }

    /// A journal frame in the pre-sweep v1 shape (an `Expunge` with no
    /// `deleted_at`) fails the rebuild instead of replaying. The v1 frame
    /// fallback (`VersionedMailPlacementRecord::decode_any` and its v1
    /// reducer arm) was retired by the compat-remnant sweep (program 4,
    /// 2026-09-24): no pre-sweep journal exists anywhere.
    #[tokio::test]
    async fn a_v1_journal_frame_is_refused_by_the_rebuild() {
        #[derive(serde::Serialize)]
        enum PreSweepRecord {
            Expunge {
                mailbox: String,
                uid_set: Vec<u32>,
                modseq: u64,
            },
        }
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xD1u8; 32];
        let root = mail_placement_segments_root(tmp.path(), &actor);
        let mut store =
            FramedSegmentStore::new(root.clone(), "mail-placement", actor).expect("store");
        let bucket = bucket_for(super::super::placement_now_secs());
        let v1_frame = fauna_cbor::encode_canonical(&PreSweepRecord::Expunge {
            mailbox: "INBOX".into(),
            uid_set: vec![2],
            modseq: 3,
        })
        .unwrap();
        store
            .append(
                &bucket,
                1,
                super::super::compute_placement_cid(0, &v1_frame),
                &v1_frame,
                &[],
            )
            .expect("append v1-shaped frame");
        store.finalize_open().expect("finalize");

        let err = rebuild_mail_placement_manifest_from_segments(&root)
            .expect_err("a v1 frame no longer replays");
        assert!(
            format!("{err:#}").contains("decode mail-placement record"),
            "the refusal names the undecodable frame: {err:#}"
        );
    }

    /// The rebuild replays a journal into a coherent current-version
    /// manifest: the `Expunge` lands its stamped tombstone, and the
    /// reducer semantics hold across the records.
    #[tokio::test]
    async fn replaying_a_journal_into_a_manifest() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xD4u8; 32];
        seed_journal(tmp.path(), &actor).await;

        let root = mail_placement_segments_root(tmp.path(), &actor);
        let m = rebuild_mail_placement_manifest_from_segments(&root).expect("replay");

        assert_eq!(m.format_version, 2, "rebuilt at the current version");
        assert_eq!(m.mailboxes.len(), 1);
        assert_eq!(m.mailboxes[0].uid_validity, 7);
        assert_eq!(m.mailboxes[0].highestmodseq, 9);
        let uids: Vec<u32> = m.placements.iter().map(|p| p.uid).collect();
        assert_eq!(uids, vec![1, 3], "uid 2 expunged");
        assert_eq!(m.tombstones.len(), 1);
        assert_eq!(m.tombstones[0].uid, 2);
        assert_eq!(m.tombstones[0].deleted_at, 1_752_000_000);
        assert_eq!(m.kind_manifest.live_segments, vec![1]);
        assert_eq!(m.kind_manifest.next_seg_id, 2);
    }

    /// Spec § D3: the manifest is a cache, the journal is durable — "if a
    /// manifest is corrupted, it rebuilds from segments by replaying every
    /// event." A garbage manifest file must not brick the actor's mail.
    ///
    /// Mail reached 2026-08-23 without this: `actor_state` propagated every
    /// `load` error, so an undecodable `manifest.mail-placement` failed every
    /// append, read and prune for that actor forever — while the cal and card
    /// managers copied *from* this file both recovered.
    #[tokio::test]
    async fn corrupt_manifest_rebuilds_from_journal_and_persists_v2() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xD2u8; 32];
        seed_journal(tmp.path(), &actor).await;
        let manifest_path = mail_placement_manifest_path(tmp.path(), &actor);
        std::fs::write(&manifest_path, b"definitely not dag-cbor").expect("corrupt");

        let manager = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        let m = manager
            .current_manifest(&actor)
            .await
            .expect("a corrupt manifest must rebuild, not error");
        assert_eq!(m.placements.len(), 2);
        assert_eq!(m.tombstones.len(), 1);

        let on_disk = MailPlacementManifest::load(&manifest_path)
            .expect("load rebuilt")
            .expect("Some");
        assert_eq!(on_disk.format_version, 2, "rebuild persisted as v2");
        assert_eq!(on_disk.placements.len(), 2);
    }

    /// A manifest stamped with a FUTURE format version must refuse loudly
    /// (a downgraded binary silently rebuilding would strip fields it cannot
    /// decode — at-rest data loss), not fall through to the journal replay.
    /// Seed a THREE-segment journal, one record per segment, each in its own
    /// bucket so the store rotates, all three finalized. Segment 2 is a
    /// **middle** segment: a later segment finalized after it, so "crashed
    /// before finalize" is structurally false for it.
    fn seed_three_segment_journal(data_dir: &std::path::Path, actor: &[u8; 32]) {
        let root = mail_placement_segments_root(data_dir, actor);
        let mut store = FramedSegmentStore::new(root, "mail-placement", *actor).expect("store");
        for seg_id in 1u32..=3 {
            let bytes = MailPlacementRecord::Append {
                mailbox: "INBOX".into(),
                uid: seg_id,
                modseq: u64::from(seg_id),
                flags: vec![],
                content_record_id: vec![seg_id as u8],
                internal_date: 1_000 * i64::from(seg_id),
            }
            .encode()
            .unwrap();
            store
                .append(
                    &format!("2026-{seg_id:02}"),
                    seg_id,
                    super::super::compute_placement_cid(u64::from(seg_id), &bytes),
                    &bytes,
                    &[],
                )
                .expect("append");
        }
        store.finalize_open().expect("finalize");
    }

    /// A MIDDLE segment that will not open is corruption, not a crash tail —
    /// the rebuild must refuse, not skip it.
    ///
    /// Skipping drops that segment's `Append` frames (delivered mail goes
    /// unreachable over IMAP) and its `Expunge` frames (deleted mail comes
    /// back), then persists the result, which decodes cleanly forever after —
    /// so nothing rebuilds again and the loss is silent and terminal, with a
    /// `tracing::warn!` as its only witness and no operator to read it. Ruled
    /// 2026-08-30; `super::open_replay_segment` carries the policy.
    #[tokio::test]
    async fn a_damaged_middle_segment_refuses_the_rebuild() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xD4u8; 32];
        seed_three_segment_journal(tmp.path(), &actor);
        let root = mail_placement_segments_root(tmp.path(), &actor);
        super::super::replay_corruption_support::damage_sidecar(&root, 2);

        let err = rebuild_mail_placement_manifest_from_segments(&root)
            .expect_err("a damaged middle segment must refuse, not be skipped");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("segment 2") && msg.contains("not the newest segment"),
            "the refusal must name the segment and why it is not a crash tail: {msg}"
        );
    }

    /// The one case the best-effort skip was reasoned for still skips: the
    /// crash tail — the HIGHEST id, sidecar never written — costs only the
    /// records appended after the last finalize, and the earlier segments
    /// replay intact.
    #[tokio::test]
    async fn the_crash_tail_is_still_skipped() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xD5u8; 32];
        seed_three_segment_journal(tmp.path(), &actor);
        let root = mail_placement_segments_root(tmp.path(), &actor);
        super::super::replay_corruption_support::remove_sidecar(&root, 3);

        let m = rebuild_mail_placement_manifest_from_segments(&root)
            .expect("an unfinalized crash tail is the skip this rebuild exists to tolerate");
        let uids: Vec<u32> = m.placements.iter().map(|p| p.uid).collect();
        assert_eq!(
            uids,
            vec![1, 2],
            "segments 1 and 2 replayed; only the tail lost"
        );
        assert_eq!(m.kind_manifest.live_segments, vec![1, 2]);
    }

    /// `SchemaMismatch` is never swallowed — not even at the tail, where every
    /// other failure is tolerated. It is the same variant `actor_state` turns
    /// into a hard refusal on the manifest eleven lines before falling into
    /// this rebuild, raised for the same reason: a newer binary wrote the
    /// file. Rebuilding past it performs precisely the
    /// downgrade-strips-fields the manifest arm exists to refuse.
    #[tokio::test]
    async fn a_newer_binarys_segment_refuses_even_at_the_tail() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xD6u8; 32];
        seed_three_segment_journal(tmp.path(), &actor);
        let root = mail_placement_segments_root(tmp.path(), &actor);
        super::super::replay_corruption_support::restamp_sidecar_to_future_version(&root, 3);

        let err = rebuild_mail_placement_manifest_from_segments(&root)
            .expect_err("a newer binary's segment must refuse at every position");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("written by a newer binary"),
            "the refusal must name the downgrade it is refusing: {msg}"
        );
    }

    #[tokio::test]
    async fn future_format_version_errors_instead_of_rebuilding() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xD3u8; 32];
        // A journal exists, so a fall-through to replay would SUCCEED — which
        // is exactly what this test must catch.
        seed_journal(tmp.path(), &actor).await;
        let path = mail_placement_manifest_path(tmp.path(), &actor);
        let mut m = MailPlacementManifest::new();
        m.format_version = 999;
        m.save_atomic(&path).expect("save");

        let manager = MailPlacementSegmentManager::new(tmp.path().to_path_buf());
        assert!(
            manager.current_manifest(&actor).await.is_err(),
            "a future-versioned manifest must be a hard error"
        );
    }
}
