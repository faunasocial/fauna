//! `CardPlacementSegmentManager` — per-actor coordinator for the
//! `__card-placement` segment store.
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
//! See `docs/goal/behavior/carddav-server.md` § Storage model → Durability &
//! disaster recovery (the card-placement journal is the `CalPlacementManifest`
//! twin) and the IMAP/CalDAV restore design (tracked internally) § D1 (a
//! third kind is a small wrapper).

use anyhow::Result;
use fauna_contacts::segments::placement::{
    AddressbookState, CardPlacement, CardPlacementManifest, CardPlacementRecord, CardTombstoneRef,
    card_placement_manifest_path, card_placement_segments_root,
};
use fauna_segment_store::SegmentStoreError;
use std::path::{Path, PathBuf};

/// The addressbook placement journal's kind binding: the six mechanical items
/// plus the one that is not (`apply_record`). The manager itself is
/// [`super::PlacementJournal`], written once for all three placement kinds.
pub struct CardPlacementKind;

/// The addressbook placement journal.
pub type CardPlacementSegmentManager = super::PlacementJournal<CardPlacementKind>;

impl super::PlacementKind for CardPlacementKind {
    type Manifest = CardPlacementManifest;
    type Record = CardPlacementRecord;
    type Tombstone = CardTombstoneRef;

    const SCOPE_KIND: &'static str = "card-placement";

    fn segments_root(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
        card_placement_segments_root(data_dir, actor_id)
    }

    fn manifest_path(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
        card_placement_manifest_path(data_dir, actor_id)
    }

    fn encode_record(record: &Self::Record) -> Result<Vec<u8>, SegmentStoreError> {
        record.encode()
    }

    fn rebuild_from_segments(root: &Path) -> Result<Self::Manifest> {
        rebuild_card_placement_manifest_from_segments(root)
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

/// The addressbook journal is part of the backed-up card corpus — the calendar
/// twin's rule verbatim ([`super::cal_placement::CalPlacementKind`]'s
/// `AdoptableJournal` impl carries the reasoning).
impl super::AdoptableJournal for CardPlacementKind {
    /// Placed cards, deleted ones, and every change number an addressbook has
    /// handed out (a fresh addressbook starts at 1, so scaffolding counts
    /// nothing).
    fn held_history(fold: &CardPlacementManifest) -> u64 {
        let spent_modseqs: u64 = fold
            .addressbooks
            .iter()
            .map(|a| a.highestmodseq.saturating_sub(1))
            .sum();
        fold.cards.len() as u64 + fold.tombstones.len() as u64 + spent_modseqs
    }

    /// The same placed cards, and no tombstone the corpus does not have;
    /// addressbooks are container-level and not compared.
    fn is_adoption_of(current: &CardPlacementManifest, corpus: &CardPlacementManifest) -> bool {
        current.cards == corpus.cards
            && current
                .tombstones
                .iter()
                .all(|t| corpus.tombstones.contains(t))
    }
}

/// Rebuild a `CardPlacementManifest` by replaying every finalized journal
/// segment under `root` in order. Twin of
/// [`super::cal_placement::rebuild_cal_placement_manifest_from_segments`];
/// see it for the crash-tail semantics, and
/// [`super::open_replay_segment`] for why every other unopenable segment
/// fails closed instead of being skipped.
pub(crate) fn rebuild_card_placement_manifest_from_segments(
    root: &Path,
) -> Result<CardPlacementManifest> {
    super::replay_placement_journal(
        root,
        CardPlacementManifest::new(),
        |m| &mut m.kind_manifest,
        |bytes, m| {
            let record = CardPlacementRecord::decode(bytes).map_err(|e| anyhow::anyhow!("{e}"))?;
            apply_record_to_manifest(&record, m);
            Ok(())
        },
    )
}

/// Apply one placement record to the in-memory compacted manifest state.
/// Handles all 5 record variants (the `CardPlacementRecord` twins of the
/// `CalPlacementRecord` variants).
fn apply_record_to_manifest(record: &CardPlacementRecord, m: &mut CardPlacementManifest) {
    match record {
        CardPlacementRecord::ProvisionAddressbook {
            addressbook_id,
            encrypted_metadata,
        } => {
            m.addressbooks
                .retain(|a| a.addressbook_id != *addressbook_id);
            m.addressbooks.push(AddressbookState {
                addressbook_id: *addressbook_id,
                encrypted_metadata: encrypted_metadata.clone(),
                highestmodseq: 1,
            });
        }
        CardPlacementRecord::UpdateAddressbookMetadata {
            addressbook_id,
            encrypted_metadata,
            modseq,
        } => {
            if let Some(a) = m
                .addressbooks
                .iter_mut()
                .find(|a| a.addressbook_id == *addressbook_id)
            {
                a.encrypted_metadata = encrypted_metadata.clone();
                a.highestmodseq = (*modseq).max(a.highestmodseq);
            }
        }
        CardPlacementRecord::DeleteAddressbook { addressbook_id } => {
            m.addressbooks
                .retain(|a| a.addressbook_id != *addressbook_id);
            m.cards.retain(|c| c.addressbook_id != *addressbook_id);
            m.tombstones.retain(|t| t.addressbook_id != *addressbook_id);
        }
        CardPlacementRecord::PutCard {
            addressbook_id,
            uid_hash,
            etag,
            modseq,
            ciphertext_size,
            card_id,
            encrypted_fauna_ext,
        } => apply_put_card(
            m,
            addressbook_id,
            uid_hash,
            etag,
            *modseq,
            *ciphertext_size,
            *card_id,
            encrypted_fauna_ext.clone(),
        ),
        CardPlacementRecord::DeleteCard {
            addressbook_id,
            uid_hash,
            modseq,
            card_id,
            deleted_at,
        } => apply_delete_card(m, addressbook_id, uid_hash, *modseq, *card_id, *deleted_at),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_put_card(
    m: &mut CardPlacementManifest,
    addressbook_id: &[u8; 32],
    uid_hash: &[u8; 32],
    etag: &str,
    modseq: u64,
    ciphertext_size: u32,
    card_id: [u8; 32],
    encrypted_fauna_ext: Option<Vec<u8>>,
) {
    // PutCard supersedes any prior PUT of the same card —
    // dedup on (addressbook_id, uid_hash).
    m.cards
        .retain(|c| !(c.addressbook_id == *addressbook_id && c.uid_hash == *uid_hash));
    m.cards.push(CardPlacement {
        addressbook_id: *addressbook_id,
        uid_hash: *uid_hash,
        etag: etag.to_string(),
        modseq,
        ciphertext_size,
        card_id,
        encrypted_fauna_ext,
    });
    // Re-PUTting a deleted UID resurrects the card, so its tombstone
    // must go — see [`super::cal_placement`]'s twin for why (the
    // manifest is compacted current state, and a `uid_hash` is the
    // client's own vCard UID, stable across delete → re-add).
    m.tombstones
        .retain(|t| !(t.addressbook_id == *addressbook_id && t.uid_hash == *uid_hash));
    update_modseq(m, addressbook_id, modseq);
}

fn apply_delete_card(
    m: &mut CardPlacementManifest,
    addressbook_id: &[u8; 32],
    uid_hash: &[u8; 32],
    modseq: u64,
    card_id: [u8; 32],
    deleted_at: i64,
) {
    m.cards
        .retain(|c| !(c.addressbook_id == *addressbook_id && c.uid_hash == *uid_hash));
    // WebDAV-Sync needs tombstones to surface to the client even when
    // our manifest is drifted vs the upstream store — so a delete
    // always leaves exactly one tombstone, carrying the newest modseq.
    // De-duplicating is what bounds it: this is the surface, fixed here
    // for both twins at once (S6.8d). See the cal twin for the full
    // argument.
    m.tombstones
        .retain(|t| !(t.addressbook_id == *addressbook_id && t.uid_hash == *uid_hash));
    m.tombstones.push(CardTombstoneRef {
        addressbook_id: *addressbook_id,
        uid_hash: *uid_hash,
        modseq,
        card_id,
        deleted_at,
    });
    update_modseq(m, addressbook_id, modseq);
}

fn update_modseq(m: &mut CardPlacementManifest, addressbook_id: &[u8; 32], modseq: u64) {
    if let Some(a) = m
        .addressbooks
        .iter_mut()
        .find(|a| a.addressbook_id == *addressbook_id)
    {
        a.highestmodseq = modseq.max(a.highestmodseq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mail::segments::bucket_for;
    use fauna_segment_store::{FramedSegmentStore, VersionedManifest};
    use tempfile::TempDir;

    #[tokio::test]
    async fn provision_then_put_card_round_trips() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x77u8; 32];
        let book = [0x11u8; 32];
        let uid_hash = [0x22u8; 32];

        let seg = manager
            .append_event(
                &actor,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: book,
                    encrypted_metadata: vec![0xaa, 0xbb],
                },
            )
            .await
            .expect("append provision");
        assert_eq!(seg, 1);

        let _seg = manager
            .append_event(
                &actor,
                &CardPlacementRecord::PutCard {
                    card_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    addressbook_id: book,
                    uid_hash,
                    etag: "etag-1".to_string(),
                    modseq: 100,
                    ciphertext_size: 1234,
                },
            )
            .await
            .expect("append put card");

        let manifest = manager.current_manifest(&actor).await.expect("current");
        assert_eq!(manifest.addressbooks.len(), 1);
        assert_eq!(manifest.addressbooks[0].addressbook_id, book);
        assert_eq!(
            manifest.addressbooks[0].encrypted_metadata,
            vec![0xaa, 0xbb]
        );
        // PutCard bumps the addressbook's highestmodseq.
        assert_eq!(manifest.addressbooks[0].highestmodseq, 100);
        assert_eq!(manifest.cards.len(), 1);
        assert_eq!(manifest.cards[0].uid_hash, uid_hash);
        assert_eq!(manifest.cards[0].etag, "etag-1");
        assert_eq!(manifest.cards[0].ciphertext_size, 1234);
    }

    #[tokio::test]
    async fn delete_card_produces_tombstone_with_modseq() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x88u8; 32];
        let book = [0x33u8; 32];
        let uid_hash = [0x44u8; 32];

        manager
            .append_event(
                &actor,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: book,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::PutCard {
                    card_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    addressbook_id: book,
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
                &CardPlacementRecord::DeleteCard {
                    card_id: [0xEF; 32],
                    deleted_at: 1_752_000_000,
                    addressbook_id: book,
                    uid_hash,
                    modseq: 11,
                },
            )
            .await
            .unwrap();

        let m = manager.current_manifest(&actor).await.unwrap();
        assert!(
            m.cards
                .iter()
                .all(|c| !(c.addressbook_id == book && c.uid_hash == uid_hash)),
            "deleted card must not remain in cards"
        );
        assert_eq!(m.tombstones.len(), 1);
        assert_eq!(m.tombstones[0].addressbook_id, book);
        assert_eq!(m.tombstones[0].uid_hash, uid_hash);
        // Tombstone modseq is load-bearing for WebDAV-Sync.
        assert_eq!(m.tombstones[0].modseq, 11);
    }

    /// S6.8d, the surface the security review filed this on: ordinary
    /// PUT→DELETE card churn must not stack tombstones.
    #[tokio::test]
    async fn repeated_delete_of_the_same_uid_keeps_one_tombstone() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x8Au8; 32];
        let book = [0x33u8; 32];
        let uid_hash = [0x44u8; 32];

        manager
            .append_event(
                &actor,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: book,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        for round in 0..3u64 {
            manager
                .append_event(
                    &actor,
                    &CardPlacementRecord::PutCard {
                        card_id: [0xEE; 32],
                        encrypted_fauna_ext: None,
                        addressbook_id: book,
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
                    &CardPlacementRecord::DeleteCard {
                        card_id: [0xEF; 32],
                        deleted_at: 1_752_000_000,
                        addressbook_id: book,
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
        assert_eq!(m.tombstones[0].modseq, 15);
    }

    /// The half `PutCard`'s resurrect-clear cannot reach: two `DeleteCard`
    /// records for one UID with no PUT between them. Today's handler never
    /// emits that (`delete_card_not_found_appends_nothing`), but a pre-guard
    /// journal can hold it, and this reducer replays every journal.
    #[test]
    fn replaying_two_deletes_of_one_uid_yields_one_tombstone() {
        let book = [0x33u8; 32];
        let uid_hash = [0x44u8; 32];
        let mut m = CardPlacementManifest::default();
        apply_record_to_manifest(
            &CardPlacementRecord::ProvisionAddressbook {
                addressbook_id: book,
                encrypted_metadata: vec![],
            },
            &mut m,
        );
        for modseq in [11u64, 12] {
            apply_record_to_manifest(
                &CardPlacementRecord::DeleteCard {
                    card_id: [0xEF; 32],
                    deleted_at: 1_752_000_000,
                    addressbook_id: book,
                    uid_hash,
                    modseq,
                },
                &mut m,
            );
        }
        assert_eq!(m.tombstones.len(), 1);
        assert_eq!(m.tombstones[0].modseq, 12);
    }

    /// A re-PUT resurrects the card; the compacted current-state manifest must
    /// not keep claiming it is deleted.
    #[tokio::test]
    async fn re_putting_a_deleted_uid_clears_its_tombstone() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x8Bu8; 32];
        let book = [0x33u8; 32];
        let uid_hash = [0x44u8; 32];

        manager
            .append_event(
                &actor,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: book,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::DeleteCard {
                    card_id: [0xEF; 32],
                    deleted_at: 1_752_000_000,
                    addressbook_id: book,
                    uid_hash,
                    modseq: 11,
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::PutCard {
                    card_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    addressbook_id: book,
                    uid_hash,
                    etag: "etag-again".to_string(),
                    modseq: 12,
                    ciphertext_size: 64,
                },
            )
            .await
            .unwrap();

        let m = manager.current_manifest(&actor).await.unwrap();
        assert_eq!(m.cards.len(), 1, "the card is back");
        assert!(
            m.tombstones.is_empty(),
            "a resurrected card must not keep a tombstone"
        );
    }

    #[tokio::test]
    async fn manifest_persists_to_disk() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x99u8; 32];
        let book = [0x55u8; 32];
        let uid_hash = [0x66u8; 32];

        manager
            .append_event(
                &actor,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: book,
                    encrypted_metadata: vec![1, 2, 3],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::PutCard {
                    card_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    addressbook_id: book,
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
        let manager2 = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let m = manager2.current_manifest(&actor).await.unwrap();
        assert_eq!(m.addressbooks.len(), 1);
        assert_eq!(m.addressbooks[0].addressbook_id, book);
        assert_eq!(m.addressbooks[0].encrypted_metadata, vec![1, 2, 3]);
        assert_eq!(m.cards.len(), 1);
        assert_eq!(m.cards[0].etag, "persist-etag");
        assert_eq!(m.cards[0].modseq, 42);
    }

    #[tokio::test]
    async fn duplicate_provision_does_not_collide_on_record_id() {
        // Two identical ProvisionAddressbook events must both succeed at
        // the segment level — `record_id` is sequenced, not
        // content-hashed.
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xbbu8; 32];
        let book = [0xccu8; 32];
        for _ in 0..2 {
            manager
                .append_event(
                    &actor,
                    &CardPlacementRecord::ProvisionAddressbook {
                        addressbook_id: book,
                        encrypted_metadata: vec![0xde, 0xad],
                    },
                )
                .await
                .expect("provision does not collide");
        }
        let m = manager.current_manifest(&actor).await.unwrap();
        // Manifest-level dedup: ProvisionAddressbook retains away prior
        // entries with the same addressbook_id, so only one AddressbookState.
        assert_eq!(m.addressbooks.len(), 1);
        assert_eq!(m.addressbooks[0].addressbook_id, book);
    }

    #[tokio::test]
    async fn update_addressbook_metadata_replaces_and_bumps_modseq() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xddu8; 32];
        let book = [0xeeu8; 32];
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: book,
                    encrypted_metadata: vec![0x01],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::UpdateAddressbookMetadata {
                    addressbook_id: book,
                    encrypted_metadata: vec![0x02, 0x03],
                    modseq: 50,
                },
            )
            .await
            .unwrap();
        let m = manager.current_manifest(&actor).await.unwrap();
        assert_eq!(m.addressbooks.len(), 1);
        assert_eq!(m.addressbooks[0].encrypted_metadata, vec![0x02, 0x03]);
        assert_eq!(m.addressbooks[0].highestmodseq, 50);
    }

    #[tokio::test]
    async fn delete_addressbook_cascades_cards_and_tombstones() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xffu8; 32];
        let book = [0x11u8; 32];
        let uid = [0x22u8; 32];
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: book,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::PutCard {
                    card_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    addressbook_id: book,
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
                &CardPlacementRecord::DeleteCard {
                    card_id: [0xEF; 32],
                    deleted_at: 1_752_000_000,
                    addressbook_id: book,
                    uid_hash: uid,
                    modseq: 11,
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::DeleteAddressbook {
                    addressbook_id: book,
                },
            )
            .await
            .unwrap();
        let m = manager.current_manifest(&actor).await.unwrap();
        assert!(m.addressbooks.is_empty(), "addressbook dropped");
        assert!(m.cards.is_empty(), "cards cascaded away");
        assert!(m.tombstones.is_empty(), "tombstones cascaded away");
    }

    #[tokio::test]
    async fn put_card_supersedes_prior_put_for_same_uid_hash() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0x33u8; 32];
        let book = [0x44u8; 32];
        let uid = [0x55u8; 32];
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: book,
                    encrypted_metadata: vec![],
                },
            )
            .await
            .unwrap();
        manager
            .append_event(
                &actor,
                &CardPlacementRecord::PutCard {
                    card_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    addressbook_id: book,
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
                &CardPlacementRecord::PutCard {
                    card_id: [0xEE; 32],
                    encrypted_fauna_ext: None,
                    addressbook_id: book,
                    uid_hash: uid,
                    etag: "etag-new".to_string(),
                    modseq: 20,
                    ciphertext_size: 200,
                },
            )
            .await
            .unwrap();
        let m = manager.current_manifest(&actor).await.unwrap();
        assert_eq!(m.cards.len(), 1, "duplicate uid_hash supersedes");
        assert_eq!(m.cards[0].etag, "etag-new");
        assert_eq!(m.cards[0].modseq, 20);
        assert_eq!(m.cards[0].ciphertext_size, 200);
        assert_eq!(m.addressbooks[0].highestmodseq, 20);
    }

    /// Seed a one-segment journal: a provision, two PUTs (the second
    /// re-PUT later), a delete, and the re-PUT carrying a sidecar. Returns the journal root.
    async fn seed_journal(
        data_dir: &std::path::Path,
        actor: &[u8; 32],
        book: [u8; 32],
    ) -> std::path::PathBuf {
        let root = card_placement_segments_root(data_dir, actor);
        let mut store =
            FramedSegmentStore::new(root.clone(), "card-placement", *actor).expect("store");
        let bucket = bucket_for(super::super::placement_now_secs());
        let frames: Vec<Vec<u8>> = [
            CardPlacementRecord::ProvisionAddressbook {
                addressbook_id: book,
                encrypted_metadata: vec![0xaa],
            },
            CardPlacementRecord::PutCard {
                addressbook_id: book,
                uid_hash: [0x51; 32],
                etag: "\"1\"".into(),
                modseq: 1,
                ciphertext_size: 100,
                card_id: [0xE1; 32],
                encrypted_fauna_ext: None,
            },
            // Superseded below by a re-PUT of the same uid.
            CardPlacementRecord::PutCard {
                addressbook_id: book,
                uid_hash: [0x52; 32],
                etag: "\"2\"".into(),
                modseq: 2,
                ciphertext_size: 200,
                card_id: [0xE2; 32],
                encrypted_fauna_ext: None,
            },
            CardPlacementRecord::DeleteCard {
                addressbook_id: book,
                uid_hash: [0x53; 32],
                modseq: 3,
                card_id: [0xE3; 32],
                deleted_at: 1_752_000_000,
            },
            CardPlacementRecord::PutCard {
                addressbook_id: book,
                uid_hash: [0x52; 32],
                etag: "\"9\"".into(),
                modseq: 9,
                ciphertext_size: 900,
                card_id: [0xE2; 32],
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
        root
    }

    /// A journal frame in the pre-sweep v1 shape (a `PutCard` with no
    /// `card_id` / sidecar) fails the rebuild instead of replaying. The v1
    /// frame fallback (`decode_any` and its v1 reducer arm, which landed
    /// the entry with a `None` id) was retired by the compat-remnant sweep
    /// (program 4, 2026-09-24): no pre-sweep journal exists anywhere.
    #[tokio::test]
    async fn a_v1_journal_frame_is_refused_by_the_rebuild() {
        #[derive(serde::Serialize)]
        enum PreSweepRecord {
            PutCard {
                #[serde(with = "serde_bytes")]
                addressbook_id: [u8; 32],
                #[serde(with = "serde_bytes")]
                uid_hash: [u8; 32],
                etag: String,
                modseq: u64,
                ciphertext_size: u32,
            },
        }
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xC1u8; 32];
        let root = card_placement_segments_root(tmp.path(), &actor);
        let mut store =
            FramedSegmentStore::new(root.clone(), "card-placement", actor).expect("store");
        let bucket = bucket_for(super::super::placement_now_secs());
        let v1_frame = fauna_cbor::encode_canonical(&PreSweepRecord::PutCard {
            addressbook_id: [0x11; 32],
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

        let err = rebuild_card_placement_manifest_from_segments(&root)
            .expect_err("a v1 frame no longer replays");
        assert!(
            format!("{err:#}").contains("decode card-placement record"),
            "the refusal names the undecodable frame: {err:#}"
        );
    }

    /// The rebuild replays a journal into a coherent current-version
    /// manifest.
    #[tokio::test]
    async fn replaying_a_journal_into_a_manifest() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xD1u8; 32];
        let book = [0x21u8; 32];
        let root = seed_journal(tmp.path(), &actor, book).await;

        let m = rebuild_card_placement_manifest_from_segments(&root).expect("replay");
        assert_eq!(m.format_version, 2);
        assert_eq!(m.addressbooks.len(), 1);
        assert_eq!(m.cards.len(), 2);
        let c52 = m.cards.iter().find(|c| c.uid_hash == [0x52; 32]).unwrap();
        assert_eq!(c52.card_id, [0xE2; 32]);
        assert_eq!(c52.encrypted_fauna_ext, Some(vec![0xf0]));
        assert_eq!(m.tombstones.len(), 1);
        assert_eq!(m.tombstones[0].deleted_at, 1_752_000_000);
        assert_eq!(m.kind_manifest.live_segments, vec![1]);
    }

    /// Card twin of the S6.8d2 prune pin — see the calendar test for the
    /// full retention argument.
    #[tokio::test]
    async fn prune_drops_only_expired_stamped_tombstones() {
        let tmp = TempDir::new().expect("tmp");
        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let actor = [0xD2u8; 32];
        let book = [0x22u8; 32];
        manager
            .update_manifest(&actor, |m| {
                for (uid, deleted_at) in [([0x61u8; 32], 1_000), ([0x62u8; 32], 2_000_000)] {
                    m.tombstones.push(CardTombstoneRef {
                        addressbook_id: book,
                        uid_hash: uid,
                        modseq: 1,
                        card_id: uid,
                        deleted_at,
                    });
                }
                true
            })
            .await
            .unwrap();
        let pruned = manager.prune_tombstones(&actor, 1_000_000).await.unwrap();
        assert_eq!(pruned, 1);
        let m = manager.current_manifest(&actor).await.unwrap();
        let uids: Vec<[u8; 32]> = m.tombstones.iter().map(|t| t.uid_hash).collect();
        assert!(!uids.contains(&[0x61; 32]), "expired pruned");
        assert!(uids.contains(&[0x62; 32]), "fresh kept");
    }

    /// Spec § D3: the manifest is a cache, the journal is durable — a garbage
    /// manifest file must not brick the actor's cards. The card manager has
    /// carried this arm since S6.2 (copied from the calendar twin); only the
    /// test was missing, so a regression to a bare `?` would have stayed green.
    #[tokio::test]
    async fn corrupt_manifest_rebuilds_from_journal_and_persists_v2() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xD5u8; 32];
        let book = [0x25u8; 32];
        seed_journal(tmp.path(), &actor, book).await;
        let manifest_path = card_placement_manifest_path(tmp.path(), &actor);
        std::fs::write(&manifest_path, b"definitely not dag-cbor").expect("corrupt");

        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        let m = manager
            .current_manifest(&actor)
            .await
            .expect("a corrupt manifest must rebuild, not error");
        assert_eq!(m.cards.len(), 2);
        assert_eq!(m.tombstones.len(), 1);

        let on_disk = CardPlacementManifest::load(&manifest_path)
            .expect("load rebuilt")
            .expect("Some");
        assert_eq!(on_disk.format_version, 2, "rebuild persisted as v2");
        assert_eq!(on_disk.cards.len(), 2);
    }

    /// A manifest stamped with a FUTURE format version must refuse loudly
    /// (a downgraded binary silently rebuilding would strip fields it cannot
    /// decode — at-rest data loss), not fall through to the journal replay.
    /// Seed a THREE-segment journal, one record per segment, each in its own
    /// bucket so the store rotates, all three finalized. Segment 2 is a
    /// **middle** segment: a later segment finalized after it, so "crashed
    /// before finalize" is structurally false for it.
    fn seed_three_segment_journal(data_dir: &std::path::Path, actor: &[u8; 32], ab: [u8; 32]) {
        let root = card_placement_segments_root(data_dir, actor);
        let mut store = FramedSegmentStore::new(root, "card-placement", *actor).expect("store");
        let frames: Vec<Vec<u8>> = vec![
            CardPlacementRecord::ProvisionAddressbook {
                addressbook_id: ab,
                encrypted_metadata: vec![0xaa],
            }
            .encode()
            .unwrap(),
            CardPlacementRecord::PutCard {
                addressbook_id: ab,
                uid_hash: [0x02; 32],
                etag: "\"2\"".into(),
                modseq: 2,
                ciphertext_size: 200,
                card_id: [0xc2; 32],
                encrypted_fauna_ext: None,
            }
            .encode()
            .unwrap(),
            CardPlacementRecord::PutCard {
                addressbook_id: ab,
                uid_hash: [0x03; 32],
                etag: "\"3\"".into(),
                modseq: 3,
                ciphertext_size: 300,
                card_id: [0xc3; 32],
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
    /// `PutCard` frames (contacts vanish from CardDAV) and its `DeleteCard`
    /// frames (deleted contacts come back), then persists the result, which
    /// decodes cleanly forever after — so nothing rebuilds again and the loss
    /// is silent and terminal. Ruled 2026-08-30; the policy lives in
    /// `super::open_replay_segment`.
    #[tokio::test]
    async fn a_damaged_middle_segment_refuses_the_rebuild() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xB4u8; 32];
        seed_three_segment_journal(tmp.path(), &actor, [0x0b; 32]);
        let root = card_placement_segments_root(tmp.path(), &actor);
        super::super::replay_corruption_support::damage_sidecar(&root, 2);

        let err = rebuild_card_placement_manifest_from_segments(&root)
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
        let actor = [0xB5u8; 32];
        seed_three_segment_journal(tmp.path(), &actor, [0x0b; 32]);
        let root = card_placement_segments_root(tmp.path(), &actor);
        super::super::replay_corruption_support::remove_sidecar(&root, 3);

        let m = rebuild_card_placement_manifest_from_segments(&root)
            .expect("an unfinalized crash tail is the skip this rebuild exists to tolerate");
        assert_eq!(
            m.cards.len(),
            1,
            "segment 2's card replayed; only the tail lost"
        );
        assert_eq!(m.cards[0].uid_hash, [0x02; 32]);
        assert_eq!(m.kind_manifest.live_segments, vec![1, 2]);
    }

    /// `SchemaMismatch` is never swallowed — not even at the tail, where every
    /// other failure is tolerated. It is the same variant `actor_state` turns
    /// into a hard refusal on the manifest before falling into this rebuild,
    /// raised for the same reason: a newer binary wrote the file.
    #[tokio::test]
    async fn a_newer_binarys_segment_refuses_even_at_the_tail() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xB6u8; 32];
        seed_three_segment_journal(tmp.path(), &actor, [0x0b; 32]);
        let root = card_placement_segments_root(tmp.path(), &actor);
        super::super::replay_corruption_support::restamp_sidecar_to_future_version(&root, 3);

        let err = rebuild_card_placement_manifest_from_segments(&root)
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
        let actor = [0xD6u8; 32];
        let book = [0x26u8; 32];
        // A journal exists, so a fall-through to replay would SUCCEED — which
        // is exactly what this test must catch.
        seed_journal(tmp.path(), &actor, book).await;
        let path = card_placement_manifest_path(tmp.path(), &actor);
        let mut m = CardPlacementManifest::new();
        m.format_version = 999;
        m.save_atomic(&path).expect("save");

        let manager = CardPlacementSegmentManager::new(tmp.path().to_path_buf());
        assert!(
            manager.current_manifest(&actor).await.is_err(),
            "a future-versioned manifest must be a hard error"
        );
    }
}
