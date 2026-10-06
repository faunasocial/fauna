//! `CalPlacementSegmentManager` — per-actor coordinator for the
//! `__calendar-placement` segment store.
//!
//! The manager itself is not here: all three placement journals ARE
//! `super::PlacementJournal`, and this module supplies only the
//! `super::PlacementKind` binding — the paths, the two on-disk name spellings,
//! and `apply_record`. What genuinely differs per kind, and stays here, is the
//! record semantics: `apply_record_to_manifest` and the `apply_put_*` /
//! `apply_delete_*` / `update_modseq` family it drives.
//!
//! Two properties the shared manager keeps and this module must not re-derive:
//! placement records have no SQLite mirror table (the manifest's compacted
//! current state is the only secondary index, and the manifest is a cache —
//! the segments are the durable source of truth) and no per-record floor blob
//! (the record IS the data, so `FramedSegmentStore::append` gets `&[]` for
//! floor_metadata).
//!
//! See the IMAP/CalDAV restore design (tracked internally) § D2 (calendar
//! record schemas), § D3 (manifest holds compacted current state).

use anyhow::Result;
use fauna_calendar::segments::placement::{
    CalPlacementManifest, CalPlacementRecord, CalendarState, EventPlacement, EventTombstoneRef,
    cal_placement_manifest_path, cal_placement_segments_root,
};
use fauna_segment_store::SegmentStoreError;
use std::path::{Path, PathBuf};

/// The calendar placement journal's kind binding: the six mechanical items
/// plus the one that is not (`apply_record`). The manager itself is
/// [`super::PlacementJournal`], written once for all three placement kinds.
pub struct CalPlacementKind;

/// The calendar placement journal.
pub type CalPlacementSegmentManager = super::PlacementJournal<CalPlacementKind>;

impl super::PlacementKind for CalPlacementKind {
    type Manifest = CalPlacementManifest;
    type Record = CalPlacementRecord;
    type Tombstone = EventTombstoneRef;

    const SCOPE_KIND: &'static str = "calendar-placement";

    fn segments_root(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
        cal_placement_segments_root(data_dir, actor_id)
    }

    fn manifest_path(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
        cal_placement_manifest_path(data_dir, actor_id)
    }

    fn encode_record(record: &Self::Record) -> Result<Vec<u8>, SegmentStoreError> {
        record.encode()
    }

    fn rebuild_from_segments(root: &Path) -> Result<Self::Manifest> {
        rebuild_cal_placement_manifest_from_segments(root)
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

/// The calendar journal is part of the backed-up calendar corpus, so a rebuilt
/// nest can adopt one — mail's rule for its own journal, applied to the second
/// kind whose corpus carries placement (`backup-destinations.md` § Third
/// destination kind → *Where restored mail lands*).
impl super::AdoptableJournal for CalPlacementKind {
    /// Placed events, deleted ones, and every change number a calendar has
    /// handed out.
    ///
    /// The change-number term is mail's spent-UID term: tombstones are pruned
    /// after their retention window, but a calendar's `highestmodseq` never
    /// goes back, and a CalDAV client holding a sync token from the target
    /// would be told "nothing changed" across a restore that reused its
    /// numbers. A freshly provisioned calendar starts at 1, so the lazily
    /// provisioned Personal calendar a calendar app's first PROPFIND creates
    /// is scaffolding and counts nothing.
    fn held_history(fold: &CalPlacementManifest) -> u64 {
        let spent_modseqs: u64 = fold
            .calendars
            .iter()
            .map(|c| c.highestmodseq.saturating_sub(1))
            .sum();
        fold.events.len() as u64 + fold.tombstones.len() as u64 + spent_modseqs
    }

    /// The same placed events, and no tombstone the corpus does not have.
    ///
    /// Calendars are not compared, for mail's reason: a calendar app that
    /// connects between an interrupted run and its retry provisions its
    /// Personal calendar, and that must not turn this ceremony's own adoption
    /// into a lived-in account. Tombstones compare one way only, because the
    /// retention prune may have dropped some since.
    fn is_adoption_of(current: &CalPlacementManifest, corpus: &CalPlacementManifest) -> bool {
        current.events == corpus.events
            && current
                .tombstones
                .iter()
                .all(|t| corpus.tombstones.contains(t))
    }
}

/// Rebuild a `CalPlacementManifest` by replaying every finalized journal
/// segment under `root` in order (ascending segment id = append order; the
/// per-actor lock serializes appends and buckets rotate monotonically).
/// Each frame decodes strictly as the current [`CalPlacementRecord`] (the
/// pre-sweep v1 frame fallback was retired by the compat-remnant sweep,
/// 2026-09-24).
///
/// Best-effort about exactly ONE thing: a crashed, never-finalized `.dat` (no
/// `.meta` sidecar) cannot be opened and is skipped with a warning — bounded
/// to the records appended after the last finalize, and strictly better than
/// the alternative (a corrupt manifest bricking the actor's calendar writes
/// forever). Restore-design spec § D3 is the authority for this path.
///
/// Every OTHER way a segment can refuse to open fails closed, matching the `?`
/// the record decode below already uses — a middle segment that will not open
/// is corruption, not a crash tail, and skipping it silently drops the events
/// it placed and resurrects the ones it deleted. Policy and reasoning:
/// [`super::open_replay_segment`].
pub(crate) fn rebuild_cal_placement_manifest_from_segments(
    root: &Path,
) -> Result<CalPlacementManifest> {
    super::replay_placement_journal(
        root,
        CalPlacementManifest::new(),
        |m| &mut m.kind_manifest,
        |bytes, m| {
            let record = CalPlacementRecord::decode(bytes).map_err(|e| anyhow::anyhow!("{e}"))?;
            apply_record_to_manifest(&record, m);
            Ok(())
        },
    )
}

/// Apply one placement record to the in-memory compacted manifest state.
/// Handles all 5 record variants per spec § D2 / § D3.
fn apply_record_to_manifest(record: &CalPlacementRecord, m: &mut CalPlacementManifest) {
    match record {
        CalPlacementRecord::ProvisionCalendar {
            calendar_id,
            encrypted_metadata,
        } => {
            m.calendars.retain(|c| c.calendar_id != *calendar_id);
            m.calendars.push(CalendarState {
                calendar_id: *calendar_id,
                encrypted_metadata: encrypted_metadata.clone(),
                highestmodseq: 1,
            });
        }
        CalPlacementRecord::UpdateCalendarMetadata {
            calendar_id,
            encrypted_metadata,
            modseq,
        } => {
            if let Some(c) = m
                .calendars
                .iter_mut()
                .find(|c| c.calendar_id == *calendar_id)
            {
                c.encrypted_metadata = encrypted_metadata.clone();
                c.highestmodseq = (*modseq).max(c.highestmodseq);
            }
        }
        CalPlacementRecord::DeleteCalendar { calendar_id } => {
            m.calendars.retain(|c| c.calendar_id != *calendar_id);
            m.events.retain(|e| e.calendar_id != *calendar_id);
            m.tombstones.retain(|t| t.calendar_id != *calendar_id);
        }
        CalPlacementRecord::PutEvent {
            calendar_id,
            uid_hash,
            etag,
            modseq,
            ciphertext_size,
            event_id,
            encrypted_fauna_ext,
        } => apply_put_event(
            m,
            calendar_id,
            uid_hash,
            etag,
            *modseq,
            *ciphertext_size,
            *event_id,
            encrypted_fauna_ext.clone(),
        ),
        CalPlacementRecord::DeleteEvent {
            calendar_id,
            uid_hash,
            modseq,
            event_id,
            deleted_at,
        } => apply_delete_event(m, calendar_id, uid_hash, *modseq, *event_id, *deleted_at),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_put_event(
    m: &mut CalPlacementManifest,
    calendar_id: &[u8; 32],
    uid_hash: &[u8; 32],
    etag: &str,
    modseq: u64,
    ciphertext_size: u32,
    event_id: [u8; 32],
    encrypted_fauna_ext: Option<Vec<u8>>,
) {
    // PutEvent supersedes any prior PUT of the same event —
    // dedup on (calendar_id, uid_hash).
    m.events
        .retain(|e| !(e.calendar_id == *calendar_id && e.uid_hash == *uid_hash));
    m.events.push(EventPlacement {
        calendar_id: *calendar_id,
        uid_hash: *uid_hash,
        etag: etag.to_string(),
        modseq,
        ciphertext_size,
        event_id,
        encrypted_fauna_ext,
    });
    // Re-PUTting a deleted UID resurrects the event, so its tombstone
    // must go: this manifest is compacted CURRENT state, and a
    // tombstone for an event that exists is a contradiction a restore
    // would replay. Unlike mail — whose UIDs are monotonic and never
    // reused, so an expunged uid can never come back — a CalDAV
    // `uid_hash` is the client's own iCalendar UID and is stable across
    // delete → re-add.
    m.tombstones
        .retain(|t| !(t.calendar_id == *calendar_id && t.uid_hash == *uid_hash));
    update_modseq(m, calendar_id, modseq);
}

fn apply_delete_event(
    m: &mut CalPlacementManifest,
    calendar_id: &[u8; 32],
    uid_hash: &[u8; 32],
    modseq: u64,
    event_id: [u8; 32],
    deleted_at: i64,
) {
    m.events
        .retain(|e| !(e.calendar_id == *calendar_id && e.uid_hash == *uid_hash));
    // WebDAV-Sync needs tombstones to surface to the client even when
    // our manifest is drifted vs the upstream store — so a delete
    // always leaves exactly one tombstone, carrying the newest modseq.
    //
    // Exactly one. PUT→DELETE churn on a live UID is already bounded by
    // PutEvent's resurrect-clear above; this `retain` covers the case
    // that clear cannot reach — two DeleteEvent records for one UID with
    // no PUT between them. Today's handler cannot emit that (a DELETE of
    // an absent event no-ops before appending, `delete_event_not_found_
    // appends_nothing`), but journals written before that guard landed
    // can hold it, and this reducer replays every journal we have ever
    // written. Together they bound the tombstone set by the number of
    // distinct UIDs deleted and not re-added — the same order as
    // `m.events`, so growth is linear in the user's own data rather than
    // in their edit count.
    m.tombstones
        .retain(|t| !(t.calendar_id == *calendar_id && t.uid_hash == *uid_hash));
    m.tombstones.push(EventTombstoneRef {
        calendar_id: *calendar_id,
        uid_hash: *uid_hash,
        modseq,
        event_id,
        deleted_at,
    });
    update_modseq(m, calendar_id, modseq);
}

fn update_modseq(m: &mut CalPlacementManifest, calendar_id: &[u8; 32], modseq: u64) {
    if let Some(c) = m
        .calendars
        .iter_mut()
        .find(|c| c.calendar_id == *calendar_id)
    {
        c.highestmodseq = modseq.max(c.highestmodseq);
    }
}

/// The placement journal is part of the corpus succession moves
/// (`succession-aftermath.md` § Re-key scope) — it is per-actor state describing
/// where that actor's records live, so leaving it behind would strand the
/// successor's mailboxes/collections under an identity the nest refuses.
///
#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mail::segments::bucket_for;
    use fauna_segment_store::{FramedSegmentStore, VersionedManifest};
    use tempfile::TempDir;

    #[tokio::test]
    async fn provision_then_put_event_round_trips() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x77u8; 32];
        let calendar = [0x11u8; 32];
        let uid_hash = [0x22u8; 32];

        let seg = manager
            .append_event(
                &actor,
                &CalPlacementRecord::ProvisionCalendar {
                    calendar_id: calendar,
                    encrypted_metadata: vec![0xaa, 0xbb],
                },
            )
            .await
            .expect("append provision");
        assert_eq!(seg, 1);

        let _seg = manager
            .append_event(
                &actor,
                &CalPlacementRecord::PutEvent {
                    event_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    calendar_id: calendar,
                    uid_hash,
                    etag: "etag-1".to_string(),
                    modseq: 100,
                    ciphertext_size: 1234,
                },
            )
            .await
            .expect("append put event");

        let manifest = manager.current_manifest(&actor).await.expect("current");
        assert_eq!(manifest.calendars.len(), 1);
        assert_eq!(manifest.calendars[0].calendar_id, calendar);
        assert_eq!(manifest.calendars[0].encrypted_metadata, vec![0xaa, 0xbb]);
        // PutEvent bumps the calendar's highestmodseq.
        assert_eq!(manifest.calendars[0].highestmodseq, 100);
        assert_eq!(manifest.events.len(), 1);
        assert_eq!(manifest.events[0].uid_hash, uid_hash);
        assert_eq!(manifest.events[0].etag, "etag-1");
        assert_eq!(manifest.events[0].ciphertext_size, 1234);
    }

    #[tokio::test]
    async fn delete_event_produces_tombstone_with_modseq() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x88u8; 32];
        let calendar = [0x33u8; 32];
        let uid_hash = [0x44u8; 32];

        manager
            .append_event(
                &actor,
                &CalPlacementRecord::ProvisionCalendar {
                    calendar_id: calendar,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::PutEvent {
                    event_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    calendar_id: calendar,
                    uid_hash,
                    etag: "etag-1".to_string(),
                    modseq: 10,
                    ciphertext_size: 64,
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::DeleteEvent {
                    event_id: [0xEF; 32],
                    deleted_at: 1_752_000_000,
                    calendar_id: calendar,
                    uid_hash,
                    modseq: 11,
                },
            )
            .await
            .unwrap();

        let m = manager.current_manifest(&actor).await.unwrap();
        assert!(
            m.events
                .iter()
                .all(|e| !(e.calendar_id == calendar && e.uid_hash == uid_hash)),
            "deleted event must not remain in events"
        );
        assert_eq!(m.tombstones.len(), 1);
        assert_eq!(m.tombstones[0].calendar_id, calendar);
        assert_eq!(m.tombstones[0].uid_hash, uid_hash);
        // Tombstone modseq is load-bearing for WebDAV-Sync.
        assert_eq!(m.tombstones[0].modseq, 11);
    }

    /// S6.8d. A `uid_hash` is the client's own iCalendar UID and is
    /// stable across delete → re-add, so an unconditional tombstone push made
    /// ordinary PUT→DELETE churn grow the manifest without bound. Exactly one
    /// tombstone survives per UID, carrying the newest modseq.
    #[tokio::test]
    async fn repeated_delete_of_the_same_uid_keeps_one_tombstone() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x8Au8; 32];
        let calendar = [0x33u8; 32];
        let uid_hash = [0x44u8; 32];

        manager
            .append_event(
                &actor,
                &CalPlacementRecord::ProvisionCalendar {
                    calendar_id: calendar,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        // Three PUT→DELETE cycles on one UID.
        for round in 0..3u64 {
            manager
                .append_event(
                    &actor,
                    &CalPlacementRecord::PutEvent {
                        event_id: [0xEE; 32],
                        encrypted_fauna_ext: None,
                        calendar_id: calendar,
                        uid_hash,
                        etag: format!("etag-{round}"),
                        modseq: 10 + round * 2,
                        ciphertext_size: 64,
                    },
                )
                .await
                .unwrap();
            manager
                .append_event(
                    &actor,
                    &CalPlacementRecord::DeleteEvent {
                        event_id: [0xEF; 32],
                        deleted_at: 1_752_000_000,
                        calendar_id: calendar,
                        uid_hash,
                        modseq: 11 + round * 2,
                    },
                )
                .await
                .unwrap();
        }

        let m = manager.current_manifest(&actor).await.unwrap();
        assert_eq!(
            m.tombstones.len(),
            1,
            "PUT→DELETE churn on one UID must not stack tombstones"
        );
        assert_eq!(
            m.tombstones[0].modseq, 15,
            "the surviving tombstone carries the newest modseq"
        );
    }

    /// The half `PutEvent`'s resurrect-clear cannot reach: two `DeleteEvent`
    /// records for one UID with no PUT between them. Today's handler never
    /// emits that (`delete_event_not_found_appends_nothing`), but a journal
    /// written before that guard landed can hold it — and this reducer replays
    /// every journal we have ever written. Driven at the reducer because no
    /// handler call sequence can produce it.
    #[test]
    fn replaying_two_deletes_of_one_uid_yields_one_tombstone() {
        let calendar = [0x33u8; 32];
        let uid_hash = [0x44u8; 32];
        let mut m = CalPlacementManifest::default();
        apply_record_to_manifest(
            &CalPlacementRecord::ProvisionCalendar {
                calendar_id: calendar,
                encrypted_metadata: vec![],
            },
            &mut m,
        );
        for modseq in [11u64, 12] {
            apply_record_to_manifest(
                &CalPlacementRecord::DeleteEvent {
                    event_id: [0xEF; 32],
                    deleted_at: 1_752_000_000,
                    calendar_id: calendar,
                    uid_hash,
                    modseq,
                },
                &mut m,
            );
        }
        assert_eq!(
            m.tombstones.len(),
            1,
            "a journal's repeated blind DeleteEvent must collapse to one tombstone"
        );
        assert_eq!(m.tombstones[0].modseq, 12, "the newest modseq wins");
    }

    /// A re-PUT resurrects the event, so the manifest — compacted CURRENT
    /// state — must not keep claiming it is deleted. Otherwise a restore
    /// replays a tombstone for an event that exists.
    #[tokio::test]
    async fn re_putting_a_deleted_uid_clears_its_tombstone() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x8Bu8; 32];
        let calendar = [0x33u8; 32];
        let uid_hash = [0x44u8; 32];

        manager
            .append_event(
                &actor,
                &CalPlacementRecord::ProvisionCalendar {
                    calendar_id: calendar,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::DeleteEvent {
                    event_id: [0xEF; 32],
                    deleted_at: 1_752_000_000,
                    calendar_id: calendar,
                    uid_hash,
                    modseq: 11,
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::PutEvent {
                    event_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    calendar_id: calendar,
                    uid_hash,
                    etag: "etag-again".to_string(),
                    modseq: 12,
                    ciphertext_size: 64,
                },
            )
            .await
            .unwrap();

        let m = manager.current_manifest(&actor).await.unwrap();
        assert_eq!(m.events.len(), 1, "the event is back");
        assert!(
            m.tombstones.is_empty(),
            "a resurrected event must not keep a tombstone"
        );
    }

    #[tokio::test]
    async fn manifest_persists_to_disk() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x99u8; 32];
        let calendar = [0x55u8; 32];
        let uid_hash = [0x66u8; 32];

        manager
            .append_event(
                &actor,
                &CalPlacementRecord::ProvisionCalendar {
                    calendar_id: calendar,
                    encrypted_metadata: vec![1, 2, 3],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::PutEvent {
                    event_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    calendar_id: calendar,
                    uid_hash,
                    etag: "persist-etag".to_string(),
                    modseq: 42,
                    ciphertext_size: 99,
                },
            )
            .await
            .unwrap();

        // New manager pointing at the same dir loads the persisted
        // manifest from disk.
        let manager2 = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let m = manager2.current_manifest(&actor).await.unwrap();
        assert_eq!(m.calendars.len(), 1);
        assert_eq!(m.calendars[0].calendar_id, calendar);
        assert_eq!(m.calendars[0].encrypted_metadata, vec![1, 2, 3]);
        assert_eq!(m.events.len(), 1);
        assert_eq!(m.events[0].etag, "persist-etag");
        assert_eq!(m.events[0].modseq, 42);
    }

    #[tokio::test]
    async fn duplicate_provision_does_not_collide_on_record_id() {
        // Two identical ProvisionCalendar events must both succeed at
        // the segment level — `record_id` is sequenced, not
        // content-hashed (mirrors T5's duplicate-Subscribe fix).
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xbbu8; 32];
        let calendar = [0xccu8; 32];
        for _ in 0..2 {
            manager
                .append_event(
                    &actor,
                    &CalPlacementRecord::ProvisionCalendar {
                        calendar_id: calendar,
                        encrypted_metadata: vec![0xde, 0xad],
                    },
                )
                .await
                .expect("provision does not collide");
        }
        let m = manager.current_manifest(&actor).await.unwrap();
        // Manifest-level dedup: ProvisionCalendar retains away prior
        // entries with the same calendar_id, so only one CalendarState.
        assert_eq!(m.calendars.len(), 1);
        assert_eq!(m.calendars[0].calendar_id, calendar);
    }

    #[tokio::test]
    async fn update_calendar_metadata_replaces_and_bumps_modseq() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xddu8; 32];
        let cal = [0xeeu8; 32];
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::ProvisionCalendar {
                    calendar_id: cal,
                    encrypted_metadata: vec![0x01],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::UpdateCalendarMetadata {
                    calendar_id: cal,
                    encrypted_metadata: vec![0x02, 0x03],
                    modseq: 50,
                },
            )
            .await
            .unwrap();
        let m = manager.current_manifest(&actor).await.unwrap();
        assert_eq!(m.calendars.len(), 1);
        assert_eq!(m.calendars[0].encrypted_metadata, vec![0x02, 0x03]);
        assert_eq!(m.calendars[0].highestmodseq, 50);
    }

    #[tokio::test]
    async fn delete_calendar_cascades_events_and_tombstones() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xffu8; 32];
        let cal = [0x11u8; 32];
        let uid = [0x22u8; 32];
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::ProvisionCalendar {
                    calendar_id: cal,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::PutEvent {
                    event_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    calendar_id: cal,
                    uid_hash: uid,
                    etag: "etag-a".to_string(),
                    modseq: 10,
                    ciphertext_size: 42,
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::DeleteEvent {
                    event_id: [0xEF; 32],
                    deleted_at: 1_752_000_000,
                    calendar_id: cal,
                    uid_hash: uid,
                    modseq: 11,
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::DeleteCalendar { calendar_id: cal },
            )
            .await
            .unwrap();
        let m = manager.current_manifest(&actor).await.unwrap();
        assert!(m.calendars.is_empty(), "calendar dropped");
        assert!(m.events.is_empty(), "events cascaded away");
        assert!(m.tombstones.is_empty(), "tombstones cascaded away");
    }

    #[tokio::test]
    async fn put_event_supersedes_prior_put_for_same_uid_hash() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x33u8; 32];
        let cal = [0x44u8; 32];
        let uid = [0x55u8; 32];
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::ProvisionCalendar {
                    calendar_id: cal,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::PutEvent {
                    event_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    calendar_id: cal,
                    uid_hash: uid,
                    etag: "etag-old".to_string(),
                    modseq: 10,
                    ciphertext_size: 100,
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CalPlacementRecord::PutEvent {
                    event_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    calendar_id: cal,
                    uid_hash: uid,
                    etag: "etag-new".to_string(),
                    modseq: 20,
                    ciphertext_size: 200,
                },
            )
            .await
            .unwrap();
        let m = manager.current_manifest(&actor).await.unwrap();
        assert_eq!(m.events.len(), 1, "duplicate uid_hash supersedes");
        assert_eq!(m.events[0].etag, "etag-new");
        assert_eq!(m.events[0].modseq, 20);
        assert_eq!(m.events[0].ciphertext_size, 200);
        assert_eq!(m.calendars[0].highestmodseq, 20);
    }

    /// Seed a one-segment journal: a provision, two PUTs (the second
    /// re-PUT later), a delete, and the re-PUT carrying a sidecar.
    async fn seed_journal(data_dir: &std::path::Path, actor: &[u8; 32], cal: [u8; 32]) {
        let root = cal_placement_segments_root(data_dir, actor);
        let mut store = FramedSegmentStore::new(root, "cal-placement", *actor).expect("store");
        let bucket = bucket_for(super::super::placement_now_secs());
        let frames: Vec<Vec<u8>> = [
            CalPlacementRecord::ProvisionCalendar {
                calendar_id: cal,
                encrypted_metadata: vec![0xaa],
            },
            CalPlacementRecord::PutEvent {
                calendar_id: cal,
                uid_hash: [0x51; 32],
                etag: "\"1\"".into(),
                modseq: 1,
                ciphertext_size: 100,
                event_id: [0xE1; 32],
                encrypted_fauna_ext: None,
            },
            // Superseded below by a re-PUT of the same uid.
            CalPlacementRecord::PutEvent {
                calendar_id: cal,
                uid_hash: [0x52; 32],
                etag: "\"2\"".into(),
                modseq: 2,
                ciphertext_size: 200,
                event_id: [0xE2; 32],
                encrypted_fauna_ext: None,
            },
            CalPlacementRecord::DeleteEvent {
                calendar_id: cal,
                uid_hash: [0x53; 32],
                modseq: 3,
                event_id: [0xE3; 32],
                deleted_at: 1_752_000_000,
            },
            CalPlacementRecord::PutEvent {
                calendar_id: cal,
                uid_hash: [0x52; 32],
                etag: "\"9\"".into(),
                modseq: 9,
                ciphertext_size: 900,
                event_id: [0xE2; 32],
                encrypted_fauna_ext: Some(vec![0xf0]),
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

    /// A journal frame in the pre-sweep v1 shape (a `PutEvent` with no
    /// `event_id` / sidecar) fails the rebuild instead of replaying. The v1
    /// frame fallback (`decode_any` and its v1 reducer arm, which landed
    /// the entry with a `None` id) was retired by the compat-remnant sweep
    /// (program 4, 2026-09-24): no pre-sweep journal exists anywhere.
    #[tokio::test]
    async fn a_v1_journal_frame_is_refused_by_the_rebuild() {
        #[derive(serde::Serialize)]
        enum PreSweepRecord {
            PutEvent {
                #[serde(with = "serde_bytes")]
                calendar_id: [u8; 32],
                #[serde(with = "serde_bytes")]
                uid_hash: [u8; 32],
                etag: String,
                modseq: u64,
                ciphertext_size: u32,
            },
        }
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xC1u8; 32];
        let root = cal_placement_segments_root(tmp.path(), &actor);
        let mut store =
            FramedSegmentStore::new(root.clone(), "cal-placement", actor).expect("store");
        let bucket = bucket_for(super::super::placement_now_secs());
        let v1_frame = fauna_cbor::encode_canonical(&PreSweepRecord::PutEvent {
            calendar_id: [0x11; 32],
            uid_hash: [0x51; 32],
            etag: "\"1\"".into(),
            modseq: 1,
            ciphertext_size: 100,
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

        let err = rebuild_cal_placement_manifest_from_segments(&root)
            .expect_err("a v1 frame no longer replays");
        assert!(
            format!("{err:#}").contains("decode cal-placement record"),
            "the refusal names the undecodable frame: {err:#}"
        );
    }

    /// The rebuild replays a journal into a coherent current-version
    /// manifest: supersede and tombstone semantics hold, and every entry
    /// carries its content-record id.
    #[tokio::test]
    async fn replaying_a_journal_into_a_manifest() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xC5u8; 32];
        let cal = [0x11u8; 32];
        seed_journal(tmp.path(), &actor, cal).await;

        let root = cal_placement_segments_root(tmp.path(), &actor);
        let m = rebuild_cal_placement_manifest_from_segments(&root).expect("replay");

        assert_eq!(m.format_version, 2);
        assert_eq!(m.calendars.len(), 1);
        assert_eq!(m.events.len(), 2, "uid 0x51 + uid 0x52 (re-PUT)");
        let e52 = m
            .events
            .iter()
            .find(|e| e.uid_hash == [0x52; 32])
            .expect("superseded event");
        assert_eq!(e52.event_id, [0xE2; 32]);
        assert_eq!(e52.encrypted_fauna_ext, Some(vec![0xf0]));
        assert_eq!(e52.etag, "\"9\"", "the re-PUT superseded the first");
        assert_eq!(m.tombstones.len(), 1);
        assert_eq!(m.tombstones[0].uid_hash, [0x53; 32]);
        assert_eq!(m.tombstones[0].deleted_at, 1_752_000_000);
        assert_eq!(m.kind_manifest.live_segments, vec![1]);
        assert_eq!(m.kind_manifest.next_seg_id, 2);
    }

    /// Spec § D3: "if a manifest is corrupted, it rebuilds from segments by
    /// replaying every event." A garbage manifest file must not brick the
    /// actor — the manager replays the journal and persists the rebuilt v2
    /// manifest.
    #[tokio::test]
    async fn corrupt_manifest_rebuilds_from_journal_and_persists_v2() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xC2u8; 32];
        let cal = [0x12u8; 32];
        seed_journal(tmp.path(), &actor, cal).await;
        let manifest_path = cal_placement_manifest_path(tmp.path(), &actor);
        std::fs::write(&manifest_path, b"definitely not dag-cbor").expect("corrupt");

        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let m = manager
            .current_manifest(&actor)
            .await
            .expect("a corrupt manifest must rebuild, not error");
        assert_eq!(m.events.len(), 2);
        assert_eq!(m.tombstones.len(), 1);

        let on_disk = CalPlacementManifest::load(&manifest_path)
            .expect("load rebuilt")
            .expect("Some");
        assert_eq!(on_disk.format_version, 2, "rebuild persisted as v2");
        assert_eq!(on_disk.events.len(), 2);
    }

    /// A manifest stamped with a FUTURE format version must refuse loudly
    /// (a downgraded binary silently rebuilding would strip fields), not
    /// fall through to the journal replay.
    /// Seed a THREE-segment journal, one record per segment, each in its own
    /// bucket so the store rotates, all three finalized. Segment 2 is a
    /// **middle** segment: a later segment finalized after it, so "crashed
    /// before finalize" is structurally false for it.
    fn seed_three_segment_journal(data_dir: &std::path::Path, actor: &[u8; 32], cal: [u8; 32]) {
        let root = cal_placement_segments_root(data_dir, actor);
        let mut store = FramedSegmentStore::new(root, "cal-placement", *actor).expect("store");
        let frames: Vec<Vec<u8>> = vec![
            CalPlacementRecord::ProvisionCalendar {
                calendar_id: cal,
                encrypted_metadata: vec![0xaa],
            }
            .encode()
            .unwrap(),
            CalPlacementRecord::PutEvent {
                calendar_id: cal,
                uid_hash: [0x02; 32],
                etag: "\"2\"".into(),
                modseq: 2,
                ciphertext_size: 200,
                event_id: [0xe2; 32],
                encrypted_fauna_ext: None,
            }
            .encode()
            .unwrap(),
            CalPlacementRecord::PutEvent {
                calendar_id: cal,
                uid_hash: [0x03; 32],
                etag: "\"3\"".into(),
                modseq: 3,
                ciphertext_size: 300,
                event_id: [0xe3; 32],
                encrypted_fauna_ext: None,
            }
            .encode()
            .unwrap(),
        ];
        for (idx, bytes) in frames.iter().enumerate() {
            let seg_id = idx as u32 + 1;
            store
                .append(
                    &format!("2026-{seg_id:02}"),
                    seg_id,
                    super::super::compute_placement_cid(u64::from(seg_id), bytes),
                    bytes,
                    &[],
                )
                .expect("append");
        }
        store.finalize_open().expect("finalize");
    }

    /// A MIDDLE segment that will not open is corruption, not a crash tail —
    /// the rebuild must refuse, not skip it. Skipping drops that segment's
    /// `PutEvent` frames (events vanish from CalDAV) and its `DeleteEvent`
    /// frames (deleted events come back), then persists the result, which
    /// decodes cleanly forever after — so nothing rebuilds again and the loss
    /// is silent and terminal. Ruled 2026-08-30; the policy lives in
    /// `super::open_replay_segment`.
    #[tokio::test]
    async fn a_damaged_middle_segment_refuses_the_rebuild() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xC4u8; 32];
        seed_three_segment_journal(tmp.path(), &actor, [0x0c; 32]);
        let root = cal_placement_segments_root(tmp.path(), &actor);
        super::super::replay_corruption_support::damage_sidecar(&root, 2);

        let err = rebuild_cal_placement_manifest_from_segments(&root)
            .expect_err("a damaged middle segment must refuse, not be skipped");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("segment 2") && msg.contains("not the newest segment"),
            "the refusal must name the segment and why it is not a crash tail: {msg}"
        );
    }

    /// The one case the best-effort skip was reasoned for still skips: the
    /// crash tail — the HIGHEST id, sidecar never written.
    #[tokio::test]
    async fn the_crash_tail_is_still_skipped() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xC5u8; 32];
        seed_three_segment_journal(tmp.path(), &actor, [0x0c; 32]);
        let root = cal_placement_segments_root(tmp.path(), &actor);
        super::super::replay_corruption_support::remove_sidecar(&root, 3);

        let m = rebuild_cal_placement_manifest_from_segments(&root)
            .expect("an unfinalized crash tail is the skip this rebuild exists to tolerate");
        assert_eq!(
            m.events.len(),
            1,
            "segment 2's event replayed; only the tail lost"
        );
        assert_eq!(m.events[0].uid_hash, [0x02; 32]);
        assert_eq!(m.kind_manifest.live_segments, vec![1, 2]);
    }

    /// `SchemaMismatch` is never swallowed — not even at the tail, where every
    /// other failure is tolerated. It is the same variant `actor_state` turns
    /// into a hard refusal on the manifest before falling into this rebuild,
    /// raised for the same reason: a newer binary wrote the file.
    #[tokio::test]
    async fn a_newer_binarys_segment_refuses_even_at_the_tail() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xC6u8; 32];
        seed_three_segment_journal(tmp.path(), &actor, [0x0c; 32]);
        let root = cal_placement_segments_root(tmp.path(), &actor);
        super::super::replay_corruption_support::restamp_sidecar_to_future_version(&root, 3);

        let err = rebuild_cal_placement_manifest_from_segments(&root)
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
        let actor = [0xC3u8; 32];
        let path = cal_placement_manifest_path(tmp.path(), &actor);
        let mut m = CalPlacementManifest::new();
        m.format_version = 999;
        m.save_atomic(&path).expect("save");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        assert!(
            manager.current_manifest(&actor).await.is_err(),
            "a future-versioned manifest must be a hard error"
        );
    }

    /// S6.8d2 — the prune drops exactly the tombstones whose recorded
    /// delete time is past the cutoff; a fresh one survives until the
    /// window passes.
    #[tokio::test]
    async fn prune_drops_only_expired_stamped_tombstones() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xC4u8; 32];
        let cal = [0x13u8; 32];
        manager
            .update_manifest(&actor, |m| {
                for (uid, deleted_at) in [
                    ([0x61u8; 32], 1_000),     // long expired
                    ([0x62u8; 32], 2_000_000), // fresh
                ] {
                    m.tombstones.push(EventTombstoneRef {
                        calendar_id: cal,
                        uid_hash: uid,
                        modseq: 1,
                        event_id: uid,
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
        let uids: Vec<[u8; 32]> = m.tombstones.iter().map(|t| t.uid_hash).collect();
        assert!(!uids.contains(&[0x61; 32]), "expired pruned");
        assert!(uids.contains(&[0x62; 32]), "fresh kept");

        // Persisted: a fresh manager sees the pruned set.
        let manager2 = CalPlacementSegmentManager::new(tmp.path().to_path_buf());
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
}
