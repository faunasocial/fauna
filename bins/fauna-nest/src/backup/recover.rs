//! `fauna.backup.custody.recover` — the lived-in recovery: land a regressed
//! source's lost records back on it **as records**, never by path.
//!
//! Owner: `docs/goal/architecture/segment-backup-protocol.md` § Client-device
//! custodian (pull) → *Restore* → *Recovery into the lived-in nest that
//! regressed*, part (2). The identity rule it rests on:
//! `message-segment-store.md` § Record identity per kind, and § Invariants
//! binding any kind whose body moves out of a SQLite column, invariant 2.
//!
//! ## Materialize's sibling, with the one difference that matters
//!
//! [`super::materialize`] flips a destination-posture set into a **fresh**
//! target and refuses a lived-in one. This verb exists for exactly the target
//! that refusal turns away: an owner's source nest that came back from an older
//! copy of its data directory, whose saved counter went backwards, and whose
//! backup still holds the segments it lost. It never refuses a target for what
//! the target holds — a lived-in target needs a different verb, not a weaker
//! rule, which is why no force arm exists anywhere.
//!
//! After the rollback the nest and the copy disagree on what one segment id
//! holds, so nothing here writes by path. A record's identity is the CID of its
//! bytes, a content record holds only what its id determines, and compaction
//! already moves records between segment files under fresh ids with their CIDs
//! intact — so a segment id names a container, and a lost record needs no id
//! back: only its bytes, its mailbox and its flags.
//!
//! ## The order
//!
//! 1. The refusals that need no bytes: the set shape, the kind, the grant.
//! 2. Every `manifest.<kind>` mirror generation the set holds, **live and
//!    retained**, both families, opened under the grant. For each, every pair
//!    it names is found among the set's rows **by the hashes it names** (a
//!    path may hold several generations, the pre- and post-rollback `seg-5`
//!    among them); a pair the set lacks is `custody_incomplete`, unless the
//!    target itself holds that segment byte-identical — the delivery leg skips
//!    those as an economy, and the target's own copy is then the pair.
//! 3. Every pair blake3-verified in memory and admitted through
//!    `FramedSegment::open`, every record re-hashed against its CID on read
//!    (the carv2 reader's own check) — before anything is written.
//! 4. **The recovery set**: every record whose CID the target holds in no
//!    `segment_records` row of that `(scope, kind)`, live **or tombstoned**.
//!    The target's own history wins: a message the owner deleted after the
//!    rollback stays deleted, and a record the target kept is never doubled.
//! 5. Per record, the ordinary write: appended verbatim under the target's
//!    climbing counter, its mirror row by the kind's canonical helper, and its
//!    placement **in the same transaction**. Mail: the newest delivered
//!    journal generation naming it gives the mailbox and flags, the UID is the
//!    target's to mint; a record no journal names lands in the inbox unseen.
//!    Calendar and contacts: see *Kinds*.
//! 6. The counter floor: the greatest ledger generation over the content
//!    mirrors read, applied through `SegmentManager::floor_counter`.
//!
//! Idempotent by construction — held now implies filed — so a re-run recovers
//! what is missing and nothing twice, and a tear after k records leaves k
//! ordinary records live and filed for the re-run to finish.
//!
//! ## Kinds
//!
//! Mail, posts, calendar and contacts; only `conv` is not recovered (refused
//! [`MaterializeRefusal::UnsupportedKind`] — it has never been delivered).
//! Posts have no placement layer and recover as records alone.
//!
//! The two DAV kinds file a record **as the current version of one resource**
//! `(collection, UID)`, under the owner doc's *Recovery's calendar and contacts
//! arms — the three collisions ruled* bullet (2026-09-29):
//!
//! - **The fold** ([`DavFold`]): every delivered journal generation, live and
//!   retained, in any order, reduced per resource to the placement or
//!   tombstone with the highest change number — absence from a generation
//!   never deletes. A record is placeable only when the fold names its own
//!   `event_id`/`card_id` as its resource's current version; every other one
//!   counts `unplaceable` and is not appended.
//! - **(a) The resource is held — the later write wins, by server receive
//!   time.** A live row born no later than the lost record (or an expunge
//!   stamped no later) is what it superseded: the record files through the
//!   replace-by-UID door. One born after it is the owner's post-regression act
//!   and stands; the record counts `already_held`.
//! - **(b) The collection is gone** — created by id with the fold's metadata
//!   and journaled; one held under other metadata keeps the target's.
//! - **(c) The change counter is floored**, never restored: before the first
//!   record files into a collection, its `highestmodseq`/`ctag` rise to the
//!   fold's greatest for it. ETag and change number are the door's to mint.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context as _;
use fauna_calendar::segments::placement::CalPlacementRecord;
use fauna_cbor::Cid;
use fauna_contacts::segments::placement::CardPlacementRecord;
use fauna_core::crypto::{NestBackupKey, OwnerSealKey};
use fauna_core::data::ContentHash;
use fauna_core::file_download::{FileDownloadKeys, download_file_bytes_by_manifest};
use fauna_mail::segments::MailFloorMetadata;
use fauna_mail::segments::floor::CONTINUATION_ROLE_PART;
use fauna_mail::segments::placement::{MailPlacementRecord, RecordPlacement};
use fauna_protocol::segments::SegmentRef;
use fauna_sync_engine::segment_backup::{
    LiveManifestMirror, SegmentFamily, parse_reserved_backup_set_name,
};

use super::materialize::{LocalStoreFetcher, MaterializeRefusal, VerifiedPair, verify_pair};
use crate::db::nest_backup_keys::NEST_BACKUP_KEY_LEN;
use crate::routes::AppState;

/// What one recover call did — the reply's payload.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoverOutcome {
    /// Records appended to the target: the recovery set this call landed.
    pub recovered: u64,
    /// Records the delivered segments hold that the target already holds, live
    /// or tombstoned — its own history, left exactly as it is.
    /// For calendar and contacts it also counts a record whose resource the
    /// target holds in a **later** version (a row or an expunge born after the
    /// record) — the owner's post-regression act, which stands.
    pub already_held: u64,
    /// Recovered records filed where the delivered journals put them: for
    /// mail, where the newest generation naming it put it; for calendar and
    /// contacts, as the current version of their resource — always equal to
    /// `recovered` for those two kinds.
    pub filed: u64,
    /// Recovered mail records no delivered journal names, landed in the inbox
    /// unseen — the arrival's own landing. Always zero for the other kinds.
    pub inboxed: u64,
    /// Records the verb could not give a place and so did not append: a
    /// calendar or contacts record the journals' fold does not name as its
    /// resource's current version (superseded or deleted before the
    /// regression, or never journaled). Always zero for mail and posts.
    pub unplaceable: u64,
    /// The counter floor applied: the greatest ledger generation over the
    /// content mirrors read. `None` when the set's mirrors named nothing.
    pub floor: Option<u32>,
}

/// The kinds this verb recovers, and what differs between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    Mail,
    Post,
    Calendar,
    Card,
}

impl Arm {
    fn of(kind: &str) -> Option<Self> {
        match kind {
            "mail" => Some(Self::Mail),
            "post" => Some(Self::Post),
            "calendar" => Some(Self::Calendar),
            "card" => Some(Self::Card),
            _ => None,
        }
    }

    /// The `segment_records` kind tag.
    fn kind(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Post => "post",
            Self::Calendar => crate::segments::cal::KIND,
            Self::Card => crate::segments::card::KIND,
        }
    }

    fn segments(self, state: &AppState) -> &Arc<fauna_segment_store::SegmentManager> {
        match self {
            Self::Mail => &state.mail_segments,
            Self::Post => &state.post_segments,
            Self::Calendar => &state.cal_segments,
            Self::Card => &state.card_segments,
        }
    }

    /// The target's own placement journal for this kind — `None` for a kind
    /// with no placement layer (posts).
    fn journal(self, state: &AppState) -> Option<LocalFamily<'_>> {
        match self {
            Self::Mail => Some(LocalFamily::MailJournal(&state.mail_placement)),
            Self::Post => None,
            Self::Calendar => Some(LocalFamily::CalJournal(&state.cal_placement)),
            Self::Card => Some(LocalFamily::CardJournal(&state.card_placement)),
        }
    }
}

/// One record a delivered segment holds, read and CID-verified.
struct Candidate {
    cid: Cid,
    bytes: Vec<u8>,
    floor: Vec<u8>,
    bucket: String,
}

/// Every generation the set holds at each path — live and retained — and the
/// means to open them. Opening is memoized by manifest hash: the seal is
/// convergent, so one hash is one plaintext wherever it appears.
struct Generations<'a> {
    fetcher: &'a LocalStoreFetcher,
    keys: &'a FileDownloadKeys,
    by_path: BTreeMap<String, Vec<ContentHash>>,
    opened: HashMap<ContentHash, Vec<u8>>,
}

impl Generations<'_> {
    async fn open(&mut self, path: &str, hash: ContentHash) -> Result<Vec<u8>, MaterializeRefusal> {
        if let Some(bytes) = self.opened.get(&hash) {
            return Ok(bytes.clone());
        }
        let bytes = download_file_bytes_by_manifest(self.fetcher, self.keys, hash, None, path)
            .await
            .with_context(|| format!("open custody path `{path}`"))
            .map_err(MaterializeRefusal::Integrity)?;
        self.opened.insert(hash, bytes.clone());
        Ok(bytes)
    }

    /// Every generation at `path`, opened.
    async fn all_at(&mut self, path: &str) -> Result<Vec<Vec<u8>>, MaterializeRefusal> {
        let hashes = self.by_path.get(path).cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(hashes.len());
        for hash in hashes {
            out.push(self.open(path, hash).await?);
        }
        Ok(out)
    }

    /// The generation at `path` whose plaintext hashes to `blake3_hex`, if the
    /// set holds one.
    async fn find(
        &mut self,
        path: &str,
        blake3_hex: &str,
    ) -> Result<Option<Vec<u8>>, MaterializeRefusal> {
        for bytes in self.all_at(path).await? {
            if hex::encode(blake3::hash(&bytes).as_bytes()) == blake3_hex {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    fn holds_any(&self, family: SegmentFamily, scope_hex: &str, kind: &str) -> bool {
        let mirror = family.mirror_path(scope_hex, kind);
        self.by_path
            .keys()
            .any(|path| *path == mirror || family.parse(scope_hex, path).is_some())
    }

    /// Every mirror generation of `family`, decoded, oldest ledger first.
    async fn mirrors(
        &mut self,
        family: SegmentFamily,
        scope_hex: &str,
        kind: &str,
    ) -> Result<Vec<LiveManifestMirror>, MaterializeRefusal> {
        let path = family.mirror_path(scope_hex, kind);
        let mut out = Vec::new();
        for bytes in self.all_at(&path).await? {
            out.push(
                LiveManifestMirror::from_bytes(&bytes)
                    .map_err(|e| MaterializeRefusal::Integrity(e.into()))?,
            );
        }
        out.sort_by_key(|m| m.next_segment_id_seen);
        Ok(out)
    }
}

/// Where the target keeps its own copy of a family's segments — consulted only
/// for a pair the set does not carry, which the delivery leg skips when the
/// target already holds it byte-identical.
enum LocalFamily<'a> {
    Content(&'a fauna_segment_store::SegmentManager),
    MailJournal(&'a crate::segments::MailPlacementSegmentManager),
    CalJournal(&'a crate::segments::CalPlacementSegmentManager),
    CardJournal(&'a crate::segments::CardPlacementSegmentManager),
}

impl LocalFamily<'_> {
    async fn pair(&self, scope: &[u8; 32], id: u32) -> Option<(Vec<u8>, Vec<u8>)> {
        let pair = match self {
            Self::Content(m) => m.read_segment_pair(scope, id).await.ok(),
            Self::MailJournal(j) => j.read_segment_pair(scope, id).await.ok(),
            Self::CalJournal(j) => j.read_segment_pair(scope, id).await.ok(),
            Self::CardJournal(j) => j.read_segment_pair(scope, id).await.ok(),
        };
        pair.map(|p| (p.dat, p.meta))
    }
}

/// Every distinct pair `mirror` names, from the set or — when the set lacks it
/// and the target holds it byte-identical — from the target. Verified against
/// the mirror either way; nothing is written.
async fn pairs_of(
    gens: &mut Generations<'_>,
    local: &LocalFamily<'_>,
    family: SegmentFamily,
    scope: &[u8; 32],
    scope_hex: &str,
    mirror: &LiveManifestMirror,
) -> Result<Vec<VerifiedPair>, MaterializeRefusal> {
    let mut out = Vec::with_capacity(mirror.live.len());
    for seg in &mirror.live {
        let meta_hex = seg.meta_blake3_hex.as_str();
        let dat_path = family.dat_path(scope_hex, seg.segment_id);
        let meta_path = family.meta_path(scope_hex, seg.segment_id);
        let mut dat = gens.find(&dat_path, &seg.blake3_hex).await?;
        let mut meta = gens.find(&meta_path, meta_hex).await?;
        // A half the set lacks may be one the leg skipped because the target
        // holds it byte-identical; the target's own copy is then that half,
        // held to the same hash the mirror names.
        if (dat.is_none() || meta.is_none())
            && let Some((local_dat, local_meta)) = local.pair(scope, seg.segment_id).await
        {
            let hex_of = |b: &[u8]| hex::encode(blake3::hash(b).as_bytes());
            if dat.is_none() && hex_of(&local_dat) == seg.blake3_hex {
                dat = Some(local_dat);
            }
            if meta.is_none() && hex_of(&local_meta) == meta_hex {
                meta = Some(local_meta);
            }
        }
        let (dat, meta) = match (dat, meta) {
            (Some(dat), Some(meta)) => (dat, meta),
            (dat, _) => {
                return Err(MaterializeRefusal::IncompleteSegment {
                    segment_id: seg.segment_id,
                    missing_path: if dat.is_none() { dat_path } else { meta_path },
                });
            }
        };
        verify_pair(&dat, &meta, seg).map_err(MaterializeRefusal::Integrity)?;
        out.push((seg.segment_id, dat, meta));
    }
    Ok(out)
}

/// A scratch directory for opening delivered pairs, removed on drop. Inside the
/// segment store's own data dir so nothing crosses a filesystem, and named so
/// no store's replay (which lists `seg-*` files of a scope dir) ever sees it.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(under: &std::path::Path) -> anyhow::Result<Self> {
        let mut token = [0u8; 8];
        getrandom::fill(&mut token).map_err(|e| anyhow::anyhow!("scratch token: {e}"))?;
        let dir = under.join(format!(".recover-scratch-{}", hex::encode(token)));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create the recover scratch dir {}", dir.display()))?;
        Ok(Self(dir))
    }

    /// Write one pair under its own subdirectory (two generations of one
    /// segment id may both be in hand) and return the `.dat` path.
    fn put(&self, n: usize, (id, dat, meta): &VerifiedPair) -> anyhow::Result<std::path::PathBuf> {
        let dir = self.0.join(format!("{n:06}"));
        std::fs::create_dir_all(&dir)?;
        let dat_path = dir.join(format!("seg-{id:08}.dat"));
        std::fs::write(dir.join(format!("seg-{id:08}.meta")), meta)?;
        std::fs::write(&dat_path, dat)?;
        Ok(dat_path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Admit every pair through `FramedSegment::open` and read every record it
/// holds, CID-verified, deduplicated by CID across pairs.
fn read_candidates(
    scratch: &Scratch,
    pairs: &[VerifiedPair],
) -> Result<Vec<Candidate>, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let mut seen: HashSet<Cid> = HashSet::new();
    let mut out = Vec::new();
    for (n, pair) in pairs.iter().enumerate() {
        let path = scratch.put(n, pair).map_err(integrity)?;
        let seg = fauna_segment_store::FramedSegment::open(&path).map_err(|e| {
            integrity(anyhow::anyhow!(
                "the delivered segment {} does not reopen: {e}",
                pair.0
            ))
        })?;
        let entries: Vec<&fauna_segment_store::RecordEntry> = seg.iter_records().collect();
        // The carv2 reader re-hashes every block against the CID it is filed
        // under — the kind-agnostic admission check of § Record identity.
        let bodies = seg.read_records_bulk(&entries).map_err(|e| {
            integrity(anyhow::anyhow!(
                "read the delivered segment {}: {e}",
                pair.0
            ))
        })?;
        for (entry, body) in entries.iter().zip(bodies) {
            let body = body.ok_or_else(|| {
                integrity(anyhow::anyhow!(
                    "the delivered segment {} indexes record {} it does not hold",
                    pair.0,
                    entry.cid
                ))
            })?;
            if seen.insert(entry.cid) {
                out.push(Candidate {
                    cid: entry.cid,
                    bytes: body,
                    floor: entry.floor_metadata.clone(),
                    bucket: seg.header.bucket.clone(),
                });
            }
        }
    }
    Ok(out)
}

/// Stage one delivered journal mirror generation's pairs into a scratch dir of
/// their own, ready for the kind's replay; returns the dir. Nothing is written
/// to the target's journal area.
fn stage_journal_generation(
    scratch: &Scratch,
    g: usize,
    pairs: &[VerifiedPair],
) -> Result<std::path::PathBuf, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let dir = scratch.0.join(format!("journal-{g:04}"));
    std::fs::create_dir_all(&dir)
        .context("create a journal fold dir")
        .map_err(integrity)?;
    for (id, dat, meta) in pairs {
        std::fs::write(dir.join(format!("seg-{id:08}.meta")), meta)
            .context("stage a journal sidecar")
            .map_err(integrity)?;
        let dat_path = dir.join(format!("seg-{id:08}.dat"));
        std::fs::write(&dat_path, dat)
            .context("stage a journal segment")
            .map_err(integrity)?;
        // Every segment the ledger names must open: the replay would skip
        // an unopenable highest one as a crash tail, which is right for a
        // live journal and wrong for a delivered one.
        fauna_segment_store::FramedSegment::open(&dat_path).map_err(|e| {
            integrity(anyhow::anyhow!(
                "the delivered journal segment {id} does not reopen: {e}"
            ))
        })?;
    }
    Ok(dir)
}

/// Replay each delivered mail journal mirror generation into a scratch fold,
/// oldest ledger first, and keep for each record the placements the NEWEST
/// generation naming it gives.
fn placements_from_folds(
    scratch: &Scratch,
    folds: &[Vec<VerifiedPair>],
) -> Result<HashMap<Vec<u8>, Vec<RecordPlacement>>, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let mut out: HashMap<Vec<u8>, Vec<RecordPlacement>> = HashMap::new();
    for (g, pairs) in folds.iter().enumerate() {
        let dir = stage_journal_generation(scratch, g, pairs)?;
        let fold =
            crate::segments::mail_placement::rebuild_mail_placement_manifest_from_segments(&dir)
                .context("fold a delivered journal generation")
                .map_err(integrity)?;
        let mut this_gen: HashMap<Vec<u8>, Vec<RecordPlacement>> = HashMap::new();
        for p in fold.placements {
            this_gen
                .entry(p.content_record_id.clone())
                .or_default()
                .push(p);
        }
        out.extend(this_gen);
    }
    Ok(out)
}

/// One resource's version in a DAV journal: its current placement, or its
/// deletion. Both are numbered on the collection's one change counter.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DavVersion {
    Placed {
        /// The content-record id the resource's row carries
        /// (`event_id`/`card_id`) — the floor of the record that is current.
        id: [u8; 32],
        modseq: u64,
        /// The row's effective sealed sidecar after this placement.
        sidecar: Option<Vec<u8>>,
    },
    Deleted {
        id: [u8; 32],
        modseq: u64,
    },
}

impl DavVersion {
    /// The fold's order: the change number first; on a tie — possible only
    /// across a counter the regression reused — a deletion over a placement,
    /// then the id, so the fold never depends on the order it reads
    /// generations in.
    fn rank(&self) -> (u64, u8, [u8; 32]) {
        match self {
            Self::Placed { id, modseq, .. } => (*modseq, 0, *id),
            Self::Deleted { id, modseq } => (*modseq, 1, *id),
        }
    }
}

/// One collection as the fold knows it: the metadata of the generation that
/// numbers it highest, and that number.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DavCollection {
    encrypted_metadata: Vec<u8>,
    highestmodseq: u64,
}

/// Where a placeable record goes: its resource and the sidecar its row takes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DavPlacement {
    collection: [u8; 32],
    uid_hash: [u8; 32],
    sidecar: Option<Vec<u8>>,
}

/// The reduction of every delivered DAV journal generation, live and retained,
/// in any order (owner doc § *Recovery's calendar and contacts arms*, "What
/// the journal says"): per resource `(collection, uid_hash)` the version with
/// the highest change number — a tombstone above a placement is a later
/// deletion, a placement above a tombstone a later re-add — and **absence from
/// a generation never deletes**, because the newest generations are the
/// regressed source's own post-regression pushes and are silent about
/// everything it lost.
#[derive(Debug, Default)]
struct DavFold {
    resources: HashMap<([u8; 32], [u8; 32]), DavVersion>,
    collections: HashMap<[u8; 32], DavCollection>,
}

impl DavFold {
    fn absorb_version(&mut self, key: ([u8; 32], [u8; 32]), version: DavVersion) {
        match self.resources.get(&key) {
            Some(held) if held.rank() >= version.rank() => {}
            _ => {
                self.resources.insert(key, version);
            }
        }
    }

    fn absorb_collection(&mut self, id: [u8; 32], encrypted_metadata: &[u8], highestmodseq: u64) {
        let candidate = (highestmodseq, encrypted_metadata);
        match self.collections.get(&id) {
            Some(held) if (held.highestmodseq, held.encrypted_metadata.as_slice()) >= candidate => {
            }
            _ => {
                self.collections.insert(
                    id,
                    DavCollection {
                        encrypted_metadata: encrypted_metadata.to_vec(),
                        highestmodseq,
                    },
                );
            }
        }
    }

    /// Fold one calendar journal generation in.
    fn absorb_cal(&mut self, m: &fauna_calendar::segments::placement::CalPlacementManifest) {
        for c in &m.calendars {
            self.absorb_collection(c.calendar_id, &c.encrypted_metadata, c.highestmodseq);
        }
        for e in &m.events {
            self.absorb_version(
                (e.calendar_id, e.uid_hash),
                DavVersion::Placed {
                    id: e.event_id,
                    modseq: e.modseq,
                    sidecar: e.encrypted_fauna_ext.clone(),
                },
            );
        }
        for t in &m.tombstones {
            self.absorb_version(
                (t.calendar_id, t.uid_hash),
                DavVersion::Deleted {
                    id: t.event_id,
                    modseq: t.modseq,
                },
            );
        }
    }

    /// Fold one contacts journal generation in — [`Self::absorb_cal`]'s twin.
    fn absorb_card(&mut self, m: &fauna_contacts::segments::placement::CardPlacementManifest) {
        for b in &m.addressbooks {
            self.absorb_collection(b.addressbook_id, &b.encrypted_metadata, b.highestmodseq);
        }
        for c in &m.cards {
            self.absorb_version(
                (c.addressbook_id, c.uid_hash),
                DavVersion::Placed {
                    id: c.card_id,
                    modseq: c.modseq,
                    sidecar: c.encrypted_fauna_ext.clone(),
                },
            );
        }
        for t in &m.tombstones {
            self.absorb_version(
                (t.addressbook_id, t.uid_hash),
                DavVersion::Deleted {
                    id: t.card_id,
                    modseq: t.modseq,
                },
            );
        }
    }

    /// Every record id the fold names as its resource's current version, with
    /// where it goes. A placement in a collection the fold holds no metadata
    /// for could never be provisioned by id, so it places nothing.
    fn current(&self) -> HashMap<[u8; 32], DavPlacement> {
        self.resources
            .iter()
            .filter(|((collection, _), _)| self.collections.contains_key(collection))
            .filter_map(|(&(collection, uid_hash), version)| match version {
                DavVersion::Placed { id, sidecar, .. } => Some((
                    *id,
                    DavPlacement {
                        collection,
                        uid_hash,
                        sidecar: sidecar.clone(),
                    },
                )),
                DavVersion::Deleted { .. } => None,
            })
            .collect()
    }
}

/// Replay every delivered DAV journal generation into a scratch fold and
/// reduce them all into one [`DavFold`].
fn dav_fold_from_journals(
    scratch: &Scratch,
    arm: Arm,
    folds: &[Vec<VerifiedPair>],
) -> Result<DavFold, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let mut out = DavFold::default();
    for (g, pairs) in folds.iter().enumerate() {
        let dir = stage_journal_generation(scratch, g, pairs)?;
        match arm {
            Arm::Calendar => out.absorb_cal(
                &crate::segments::cal_placement::rebuild_cal_placement_manifest_from_segments(&dir)
                    .context("fold a delivered calendar journal generation")
                    .map_err(integrity)?,
            ),
            Arm::Card => out.absorb_card(
                &crate::segments::card_placement::rebuild_card_placement_manifest_from_segments(
                    &dir,
                )
                .context("fold a delivered contacts journal generation")
                .map_err(integrity)?,
            ),
            Arm::Mail | Arm::Post => unreachable!("only the DAV arms fold a DAV journal"),
        }
    }
    Ok(out)
}

/// What the lived-in recovery reads from a delivered record's DAV floor — the
/// fields the calendar and contacts floors share.
struct DavFloor {
    id: [u8; 32],
    ciphertext_size: u32,
    internal_date: i64,
    /// Server receive time, epoch seconds — the clock collision (a) reads.
    created_at: i64,
}

fn dav_floor(arm: Arm, bytes: &[u8]) -> Result<DavFloor, MaterializeRefusal> {
    let bad = |e: String| {
        MaterializeRefusal::Integrity(anyhow::anyhow!("decode a recovered record's floor: {e}"))
    };
    match arm {
        Arm::Calendar => {
            let f = fauna_calendar::segments::CalFloorMetadata::decode(bytes)
                .map_err(|e| bad(e.to_string()))?;
            Ok(DavFloor {
                id: f.event_id,
                ciphertext_size: f.ciphertext_size,
                internal_date: f.internal_date,
                created_at: f.created_at,
            })
        }
        Arm::Card => {
            let f = fauna_contacts::segments::CardFloorMetadata::decode(bytes)
                .map_err(|e| bad(e.to_string()))?;
            Ok(DavFloor {
                id: f.card_id,
                ciphertext_size: f.ciphertext_size,
                internal_date: f.internal_date,
                created_at: f.created_at,
            })
        }
        Arm::Mail | Arm::Post => unreachable!("only the DAV arms carry a DAV floor"),
    }
}

/// The delivered journals' reduction, in the arm's shape.
enum Placements {
    /// Posts: no placement layer.
    None,
    Mail(HashMap<Vec<u8>, Vec<RecordPlacement>>),
    Dav(DavFold),
}

/// Recover one segment-axis custody set for `actor` — the whole verb.
///
/// `stop_after` bounds how many records this call appends and skips the closing
/// floor when it cuts the run short: the production handler passes `None`; the
/// torn-run proofs pass `Some(k)` to leave exactly the state a crash after k
/// records leaves.
pub(crate) async fn recover_segment_set(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    set_name: &str,
    stop_after: Option<usize>,
) -> Result<RecoverOutcome, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let (kind, _named_scope) = parse_reserved_backup_set_name(set_name).ok_or_else(|| {
        MaterializeRefusal::NotASegmentSet {
            set_name: set_name.to_string(),
        }
    })?;
    let arm = Arm::of(kind).ok_or(MaterializeRefusal::UnsupportedKind { kind })?;
    let segments = arm.segments(state);
    // The owner is the authenticated connection's actor, never a wire
    // parameter, exactly as for materialize.
    let scope = *actor;
    let scope_hex = hex::encode(scope);

    // ── The key the owner granted at enrollment ──────────────────────────
    let granted = state
        .db
        .get_nest_backup_key(actor.as_slice())
        .await
        .map_err(integrity)?
        .ok_or(MaterializeRefusal::NotEnrolled)?;
    let granted: [u8; NEST_BACKUP_KEY_LEN] = granted.as_slice().try_into().map_err(|_| {
        integrity(anyhow::anyhow!(
            "the granted NestBackupKey is {} bytes, not {NEST_BACKUP_KEY_LEN}",
            granted.len()
        ))
    })?;
    let keys =
        FileDownloadKeys::owner(OwnerSealKey::SourceNest(NestBackupKey::from_bytes(granted)));
    let backup_svc = state
        .backup_service
        .as_ref()
        .ok_or_else(|| integrity(anyhow::anyhow!("this nest has no blob store configured")))?;
    let fetcher = LocalStoreFetcher {
        store: backup_svc.local_blob_store(),
        at_rest_key: backup_svc.encryption_key().cloned(),
    };

    // ── Every generation the set holds, live and retained ────────────────
    let mut by_path: BTreeMap<String, Vec<ContentHash>> = BTreeMap::new();
    let live = state
        .db
        .list_backup_custody_in_set(&scope, set_name)
        .await
        .map_err(integrity)?;
    let retained = state
        .db
        .list_backup_custody_generations_in_set(&scope, set_name)
        .await
        .map_err(integrity)?;
    let rows = live
        .into_iter()
        .map(|r| (r.path, r.manifest_hash))
        .chain(retained.into_iter().map(|r| (r.path, r.manifest_hash)));
    for (path, manifest_hash) in rows {
        let (Some(path), Ok(hash)) = (path, <[u8; 32]>::try_from(manifest_hash.as_slice())) else {
            continue;
        };
        let hash = ContentHash::from_digest_raw(hash);
        let at = by_path.entry(path).or_default();
        if !at.contains(&hash) {
            at.push(hash);
        }
    }
    let mut gens = Generations {
        fetcher: &fetcher,
        keys: &keys,
        by_path,
        opened: HashMap::new(),
    };

    // ── Both families, every generation, verified before anything is written ─
    let content_mirrors = gens
        .mirrors(SegmentFamily::Content, &scope_hex, kind)
        .await?;
    if content_mirrors.is_empty() {
        return Err(MaterializeRefusal::NoManifestMirror {
            set_name: set_name.to_string(),
            path: SegmentFamily::Content.mirror_path(&scope_hex, kind),
        });
    }
    let local_content = LocalFamily::Content(segments);
    let mut content_pairs: Vec<VerifiedPair> = Vec::new();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    for mirror in &content_mirrors {
        let named: Vec<SegmentRef> = mirror
            .live
            .iter()
            .filter(|s| seen_pairs.insert((s.blake3_hex.clone(), s.meta_blake3_hex.clone())))
            .cloned()
            .collect();
        let only_new = LiveManifestMirror {
            live: named,
            ..mirror.clone()
        };
        content_pairs.extend(
            pairs_of(
                &mut gens,
                &local_content,
                SegmentFamily::Content,
                &scope,
                &scope_hex,
                &only_new,
            )
            .await?,
        );
    }
    let floor = content_mirrors.iter().map(|m| m.next_segment_id_seen).max();

    let mut journal_folds: Vec<Vec<VerifiedPair>> = Vec::new();
    let local_journal = arm.journal(state);
    if let Some(local_journal) = &local_journal
        && gens.holds_any(SegmentFamily::Placement, &scope_hex, kind)
    {
        let journal_mirrors = gens
            .mirrors(SegmentFamily::Placement, &scope_hex, kind)
            .await?;
        if journal_mirrors.is_empty() {
            // Journal custody with no journal ledger is a delivery still under
            // way — the leg lands every mirror last.
            return Err(MaterializeRefusal::NoManifestMirror {
                set_name: set_name.to_string(),
                path: SegmentFamily::Placement.mirror_path(&scope_hex, kind),
            });
        }
        for mirror in &journal_mirrors {
            journal_folds.push(
                pairs_of(
                    &mut gens,
                    local_journal,
                    SegmentFamily::Placement,
                    &scope,
                    &scope_hex,
                    mirror,
                )
                .await?,
            );
        }
    }

    let scratch = Scratch::new(segments.data_dir()).map_err(integrity)?;
    let candidates = read_candidates(&scratch, &content_pairs)?;
    let placements = match arm {
        Arm::Post => Placements::None,
        Arm::Mail => Placements::Mail(placements_from_folds(&scratch, &journal_folds)?),
        Arm::Calendar | Arm::Card => {
            Placements::Dav(dav_fold_from_journals(&scratch, arm, &journal_folds)?)
        }
    };
    drop(scratch);

    let Landed { mut outcome, whole } =
        land(state, &scope, arm, candidates, &placements, stop_after).await?;
    outcome.floor = floor;
    if !whole {
        return Ok(outcome);
    }

    // ── The counter floor ────────────────────────────────────────────────
    //
    // `SegmentManager::floor_counter` — the one `fauna.segments.counter_floor`
    // applies: monotonic, idempotent, under the scope lock. A no-op after an
    // accepted regression floored the source; the guard for a recovery driven
    // before that floor landed.
    if let Some(floor) = floor {
        segments
            .floor_counter(&scope, floor)
            .await
            .context("apply the counter floor")
            .map_err(integrity)?;
    }
    if arm == Arm::Mail && outcome.recovered > 0 {
        crate::segments::notify_mail_received(&state.ws, &scope);
    }
    Ok(outcome)
}

/// What [`land`] did, and whether it finished the set.
struct Landed {
    outcome: RecoverOutcome,
    /// `false` when `stop_after` cut the run short — the torn-run seam.
    whole: bool,
}

/// The recovery set and the ordinary write per record: every candidate whose
/// CID the target holds in no mirror row — live or tombstoned — is appended,
/// mirrored and (mail, calendar, contacts) filed; every other one is counted
/// `already_held` and left exactly as it is. A calendar or contacts record the
/// fold does not name as its resource's current version counts `unplaceable`
/// and is not appended.
async fn land(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: Arm,
    candidates: Vec<Candidate>,
    placements: &Placements,
    stop_after: Option<usize>,
) -> Result<Landed, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let kind = arm.kind();
    let mut outcome = RecoverOutcome::default();
    let mut to_recover: Vec<Candidate> = Vec::new();
    {
        let conn = state.db.conn().await;
        for c in candidates {
            if crate::segments::records_db::held_ever(&conn, scope, kind, &c.cid)
                .map_err(integrity)?
            {
                outcome.already_held += 1;
            } else {
                to_recover.push(c);
            }
        }
    }

    let no_mail = HashMap::new();
    let mail = match placements {
        Placements::Mail(p) => p,
        _ => &no_mail,
    };
    if arm == Arm::Mail && !to_recover.is_empty() {
        prepare_mailboxes(state, scope, &to_recover, mail).await?;
    }

    // The DAV arms: only a record the fold names as its resource's current
    // version is placeable; its collection is made ready before any files.
    let work: Vec<Work> = match placements {
        Placements::Dav(fold) => {
            let current = fold.current();
            let mut work = Vec::new();
            for c in to_recover {
                let floor = dav_floor(arm, &c.floor)?;
                match current.get(&floor.id) {
                    Some(placement) => work.push(Work::Dav(c, floor, placement.clone())),
                    None => outcome.unplaceable += 1,
                }
            }
            prepare_collections(state, scope, arm, fold, &work).await?;
            work
        }
        _ => to_recover.into_iter().map(Work::Record).collect(),
    };

    let mut appended = 0usize;
    for w in work {
        if stop_after.is_some_and(|k| appended >= k) {
            return Ok(Landed {
                outcome,
                whole: false,
            });
        }
        let landed = match w {
            Work::Record(c) if arm == Arm::Mail => {
                recover_mail(state, scope, c, mail, &mut outcome).await?
            }
            Work::Record(c) => recover_post(state, scope, c).await?,
            Work::Dav(c, floor, placement) => {
                recover_dav(state, scope, arm, c, floor, placement, &mut outcome).await?
            }
        };
        if landed {
            outcome.recovered += 1;
            appended += 1;
        } else {
            // Filed by a concurrent writer between the read and the append.
            outcome.already_held += 1;
        }
    }
    Ok(Landed {
        outcome,
        whole: true,
    })
}

/// The mailboxes a recovered record will be filed into, in place before the
/// first record: the standard ones (seeded as an arrival seeds them), and each
/// other one the journal names that the owner removed after the rollback —
/// created by name with a fresh UIDVALIDITY, as IMAP CREATE mints one, so a
/// mail client never mistakes the new mailbox for the one it cached.
async fn prepare_mailboxes(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    records: &[Candidate],
    placements: &HashMap<Vec<u8>, Vec<RecordPlacement>>,
) -> Result<(), MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let seeded = state
        .db
        .ensure_bridge_imap_mailboxes(scope)
        .await
        .map_err(integrity)?;
    for s in seeded {
        journal(
            state,
            scope,
            MailPlacementRecord::Create {
                mailbox: s.name,
                uid_validity: s.uid_validity,
                attrs: s.attrs,
            },
        )
        .await?;
    }
    let mut named: Vec<&str> = records
        .iter()
        .filter_map(|c| placements.get(&c.cid.digest().to_vec()))
        .flatten()
        .map(|p| p.mailbox.as_str())
        .collect();
    named.sort_unstable();
    named.dedup();
    for mailbox in named {
        let uid_validity = (crate::segments::placement_now_secs() as u64 * 1000) as u32;
        if let crate::db::bridge_imap::CreateMailboxDbOutcome::Created { uid_validity } = state
            .db
            .create_bridge_imap_mailbox(scope, mailbox, uid_validity)
            .await
            .map_err(integrity)?
        {
            journal(
                state,
                scope,
                MailPlacementRecord::Create {
                    mailbox: mailbox.to_string(),
                    uid_validity,
                    attrs: Vec::new(),
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn journal(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    record: MailPlacementRecord,
) -> Result<(), MaterializeRefusal> {
    state
        .mail_placement
        .append_event(scope, &record)
        .await
        .map(|_| ())
        .context("journal a recovered placement")
        .map_err(MaterializeRefusal::Integrity)
}

/// One mail record: append, mirror row and placement(s) in one transaction,
/// then journal the placements — the arrival door's order. Returns whether it
/// landed (`false`: someone filed it first).
async fn recover_mail(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    c: Candidate,
    placements: &HashMap<Vec<u8>, Vec<RecordPlacement>>,
    outcome: &mut RecoverOutcome,
) -> Result<bool, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let floor = MailFloorMetadata::decode(&c.floor)
        .map_err(|e| integrity(anyhow::anyhow!("decode a recovered record's floor: {e}")))?;
    let digest: [u8; 32] = c.cid.digest();
    // A continuation part is a range of its head's seal, never a message: it
    // is recovered as a record and filed nowhere, and its head is filed.
    let targets: Vec<(String, String, i64)> = if floor.continuation_role == CONTINUATION_ROLE_PART {
        Vec::new()
    } else {
        match placements.get(&digest.to_vec()) {
            Some(named) => named
                .iter()
                .map(|p| (p.mailbox.clone(), p.flags.join(" "), p.internal_date))
                .collect(),
            // INTERNALDATE is epoch seconds and the floor's receipt instant is
            // millis — the arrival door's own conversion
            // (`nest_sync_worker`'s relay placement; materialize's inbox filing).
            None => vec![("INBOX".to_string(), String::new(), floor.received_at / 1000)],
        }
    };
    let journaled_by_name = placements.contains_key(&digest.to_vec());
    let from_norm = crate::db::bridge_imap::norm_for_storage(&floor.sender_domain);
    let created_at = crate::segments::placement_now_secs() * 1000;

    let landed = crate::segments::mail::append_recovered_record(
        &state.mail_segments,
        &state.db,
        scope,
        &c.bytes,
        &c.bucket,
        floor,
        |tx| {
            let mut filed = Vec::with_capacity(targets.len());
            for (mailbox, flags, internal_date) in &targets {
                let (uid, modseq) = crate::db::bridge_imap::place_new_message_in(
                    tx,
                    scope,
                    &digest,
                    mailbox,
                    *internal_date,
                    flags,
                    &from_norm,
                    created_at,
                )
                .with_context(|| format!("file a recovered record in {mailbox}"))?;
                filed.push(MailPlacementRecord::Append {
                    mailbox: mailbox.clone(),
                    uid,
                    modseq: modseq as u64,
                    flags: flags.split_whitespace().map(String::from).collect(),
                    content_record_id: digest.to_vec(),
                    internal_date: *internal_date,
                });
            }
            Ok(filed)
        },
    )
    .await
    .map_err(integrity)?;
    let Some((_seg, appends)) = landed else {
        return Ok(false);
    };
    if !appends.is_empty() {
        if journaled_by_name {
            outcome.filed += 1;
        } else {
            outcome.inboxed += 1;
        }
    }
    for record in appends {
        journal(state, scope, record).await?;
    }
    Ok(true)
}

/// One record of the recovery set, as [`land`] writes it.
enum Work {
    /// Mail or post: the kind's own write decides where it lands.
    Record(Candidate),
    /// A placeable calendar or contacts record: its floor and its resource.
    Dav(Candidate, DavFloor, DavPlacement),
}

/// Every collection a placeable record will be filed into, made ready before
/// the first record files: **(b)** one the target lacks is created by id with
/// the fold's metadata through the ordinary door and journaled on `Created`
/// (one held under other metadata keeps the target's — `Conflict` is a
/// no-op); **(c)** its change counter is floored at the fold's greatest for it.
async fn prepare_collections(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: Arm,
    fold: &DavFold,
    work: &[Work],
) -> Result<(), MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let named: std::collections::BTreeSet<[u8; 32]> = work
        .iter()
        .filter_map(|w| match w {
            Work::Dav(_, _, p) => Some(p.collection),
            Work::Record(_) => None,
        })
        .collect();
    for id in named {
        // `DavFold::current` names only collections the fold holds.
        let Some(col) = fold.collections.get(&id) else {
            continue;
        };
        let now = crate::db::now_epoch_secs();
        let floor = i64::try_from(col.highestmodseq).unwrap_or(i64::MAX);
        match arm {
            Arm::Calendar => {
                let created = state
                    .db
                    .insert_bridge_caldav_calendar(scope, &id, &col.encrypted_metadata, now)
                    .await
                    .map_err(integrity)?;
                if created == crate::db::bridge_caldav::ProvisionOutcome::Created {
                    dav_journal(
                        state,
                        scope,
                        DavRecord::Cal(CalPlacementRecord::ProvisionCalendar {
                            calendar_id: id,
                            encrypted_metadata: col.encrypted_metadata.clone(),
                        }),
                    )
                    .await?;
                }
                state
                    .db
                    .floor_bridge_caldav_calendar_counter(scope, &id, floor)
                    .await
                    .map_err(integrity)?;
            }
            Arm::Card => {
                let created = state
                    .db
                    .insert_bridge_carddav_addressbook(scope, &id, &col.encrypted_metadata, now)
                    .await
                    .map_err(integrity)?;
                if created == crate::db::bridge_carddav::ProvisionOutcome::Created {
                    dav_journal(
                        state,
                        scope,
                        DavRecord::Card(CardPlacementRecord::ProvisionAddressbook {
                            addressbook_id: id,
                            encrypted_metadata: col.encrypted_metadata.clone(),
                        }),
                    )
                    .await?;
                }
                state
                    .db
                    .floor_bridge_carddav_addressbook_counter(scope, &id, floor)
                    .await
                    .map_err(integrity)?;
            }
            Arm::Mail | Arm::Post => unreachable!("only the DAV arms have collections"),
        }
    }
    Ok(())
}

/// A DAV placement journal record, of either kind.
enum DavRecord {
    Cal(CalPlacementRecord),
    Card(CardPlacementRecord),
}

async fn dav_journal(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    record: DavRecord,
) -> Result<(), MaterializeRefusal> {
    match record {
        DavRecord::Cal(r) => state.cal_placement.append_event(scope, &r).await,
        DavRecord::Card(r) => state.card_placement.append_event(scope, &r).await,
    }
    .map(|_| ())
    .context("journal a recovered placement")
    .map_err(MaterializeRefusal::Integrity)
}

/// What the replace-by-UID door did to a recovered record's resource: the
/// write to journal, or `None` for a no-op (the row already carries this id).
struct DavPut {
    etag: String,
    modseq: i64,
    sidecar: Option<Vec<u8>>,
}

/// One placeable calendar or contacts record. First the resource decision —
/// collision (a), read before the append and outside its transaction (mail's
/// accepted read-then-append race): a live row or an expunge stamped **after**
/// the record's own receive time is the owner's post-regression act and
/// stands (`Ok(false)`, counted `already_held`); anything no later — a tie
/// included — is what the record superseded. Then the append, the mirror row
/// and the replace-by-UID door in one transaction; the `PutEvent`/`PutCard`
/// journaled after the commit and the collection's change notification sent,
/// as a PUT's. Returns whether it landed (`false`: held, by resource or by a
/// concurrent writer's CID).
async fn recover_dav(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: Arm,
    c: Candidate,
    floor: DavFloor,
    placement: DavPlacement,
    outcome: &mut RecoverOutcome,
) -> Result<bool, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let DavPlacement {
        collection,
        uid_hash,
        sidecar,
    } = placement;
    let (live, expunged) = match arm {
        Arm::Calendar => {
            state
                .db
                .caldav_resource_history(scope, &collection, &uid_hash)
                .await
        }
        _ => {
            state
                .db
                .carddav_resource_history(scope, &collection, &uid_hash)
                .await
        }
    }
    .map_err(integrity)?;
    if live
        .into_iter()
        .chain(expunged)
        .any(|at| at > floor.created_at)
    {
        return Ok(false);
    }

    let now = crate::db::now_epoch_secs();
    let cid = c.cid;
    let bad = |what: &str, e: String| integrity(anyhow::anyhow!("decode a recovered {what}: {e}"));
    let landed = match arm {
        Arm::Calendar => {
            let env = fauna_calendar::segments::CalRecordEnvelope::decode(&c.bytes)
                .map_err(|e| bad("envelope", e.to_string()))?;
            let meta = fauna_calendar::segments::CalFloorMetadata::decode(&c.floor)
                .map_err(|e| bad("floor", e.to_string()))?;
            crate::segments::cal::append_recovered_record(
                &state.cal_segments,
                &state.db,
                scope,
                &c.bytes,
                &meta,
                |tx| {
                    use crate::db::bridge_caldav::ReplaceCaldavEventOutcome as O;
                    match crate::db::bridge_caldav::replace_caldav_event_by_uid_in(
                        tx,
                        scope,
                        &collection,
                        &uid_hash,
                        None,
                        &floor.id,
                        &cid,
                        &env.encrypted_index_hint,
                        sidecar.as_deref(),
                        floor.internal_date,
                        floor.ciphertext_size,
                        now,
                    )? {
                        O::Created {
                            etag,
                            modseq,
                            encrypted_fauna_ext,
                            ..
                        }
                        | O::Updated {
                            etag,
                            modseq,
                            encrypted_fauna_ext,
                            ..
                        } => Ok(Some(DavPut {
                            etag,
                            modseq,
                            sidecar: encrypted_fauna_ext,
                        })),
                        O::Idempotent { .. } => Ok(None),
                        other => anyhow::bail!("file a recovered event: {other:?}"),
                    }
                },
            )
            .await
        }
        _ => {
            let env = fauna_contacts::segments::CardRecordEnvelope::decode(&c.bytes)
                .map_err(|e| bad("envelope", e.to_string()))?;
            let meta = fauna_contacts::segments::CardFloorMetadata::decode(&c.floor)
                .map_err(|e| bad("floor", e.to_string()))?;
            crate::segments::card::append_recovered_record(
                &state.card_segments,
                &state.db,
                scope,
                &c.bytes,
                &meta,
                |tx| {
                    use crate::db::bridge_carddav::ReplaceCarddavCardOutcome as O;
                    match crate::db::bridge_carddav::replace_carddav_card_by_uid_in(
                        tx,
                        scope,
                        &collection,
                        &uid_hash,
                        None,
                        &floor.id,
                        &cid,
                        &env.encrypted_index_hint,
                        sidecar.as_deref(),
                        floor.internal_date,
                        floor.ciphertext_size,
                        now,
                    )? {
                        O::Created {
                            etag,
                            modseq,
                            encrypted_fauna_ext,
                            ..
                        }
                        | O::Updated {
                            etag,
                            modseq,
                            encrypted_fauna_ext,
                            ..
                        } => Ok(Some(DavPut {
                            etag,
                            modseq,
                            sidecar: encrypted_fauna_ext,
                        })),
                        O::Idempotent { .. } => Ok(None),
                        other => anyhow::bail!("file a recovered card: {other:?}"),
                    }
                },
            )
            .await
        }
    }
    .map_err(integrity)?;
    let Some((_seg, put)) = landed else {
        return Ok(false);
    };
    outcome.filed += 1;
    if let Some(DavPut {
        etag,
        modseq,
        sidecar,
    }) = put
    {
        let modseq = modseq as u64;
        let record = match arm {
            Arm::Calendar => DavRecord::Cal(CalPlacementRecord::PutEvent {
                calendar_id: collection,
                uid_hash,
                etag,
                modseq,
                ciphertext_size: floor.ciphertext_size,
                event_id: floor.id,
                encrypted_fauna_ext: sidecar,
            }),
            _ => DavRecord::Card(CardPlacementRecord::PutCard {
                addressbook_id: collection,
                uid_hash,
                etag,
                modseq,
                ciphertext_size: floor.ciphertext_size,
                card_id: floor.id,
                encrypted_fauna_ext: sidecar,
            }),
        };
        dav_journal(state, scope, record).await?;
    }
    match arm {
        Arm::Calendar => {
            crate::bridge_caldav_handlers::notify_calendar_changed(state, scope, &collection)
        }
        _ => crate::bridge_carddav_handlers::notify_addressbook_changed(state, scope, &collection),
    }
    Ok(true)
}

/// One post: append, then its mirror row and feed projection in one
/// transaction. Posts have no placement layer — they recover as records alone.
async fn recover_post(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    c: Candidate,
) -> Result<bool, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let outcome = state
        .post_segments
        .append_record_with_bucket(scope, c.cid, &c.bytes, &c.floor, &c.bucket)
        .await
        .map_err(|e| integrity(anyhow::anyhow!("append a recovered post: {e}")))?;
    let conn = state.db.conn().await;
    let tx = conn
        .unchecked_transaction()
        .context("begin the recovered-post transaction")
        .map_err(integrity)?;
    let n = crate::segments::post::reassert_records(
        &tx,
        scope,
        &[(outcome.segment_id, c.cid, c.bytes, c.floor)],
    )
    .context("re-assert the recovered post")
    .map_err(integrity)?;
    tx.commit()
        .context("commit the recovered post")
        .map_err(integrity)?;
    Ok(n > 0)
}

/// The torn-run seam for the tier_3 proof: [`recover_segment_set`] with a
/// record budget, reduced to its outcome or its wire-facing refusal text.
/// Not a production entry point — the handler always runs the whole set.
#[doc(hidden)]
#[cfg(any(test, debug_assertions, feature = "test-hooks"))]
pub async fn recover_segment_set_for_test(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    set_name: &str,
    stop_after: Option<usize>,
) -> Result<RecoverOutcome, String> {
    recover_segment_set(state, actor, set_name, stop_after)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    //! Tier_1 pins for the write phase ([`land`]) — the recovery set, the
    //! landing, the one transaction, the torn run. The whole verb, from custody
    //! through the leg, is the tier_3 proof in `tests/nest_backup_coordinator.rs`.
    use super::*;
    use crate::segments::test_helpers::{build_state, floor};

    const OWNER: [u8; 32] = [0x21; 32];

    /// A nest whose mail, calendar and contacts content and journals all live
    /// in the fixture's tempdir.
    fn nest() -> (tempfile::TempDir, Arc<AppState>) {
        let (tmp, state) = build_state();
        let mut state = Arc::try_unwrap(state).unwrap_or_else(|_| unreachable!("fresh fixture"));
        let dir = tmp.path().to_path_buf();
        state.mail_placement = Arc::new(crate::segments::MailPlacementSegmentManager::new(
            dir.clone(),
        ));
        state.cal_segments = Arc::new(fauna_segment_store::SegmentManager::new(
            dir.clone(),
            crate::segments::cal::KIND,
        ));
        state.card_segments = Arc::new(fauna_segment_store::SegmentManager::new(
            dir.clone(),
            crate::segments::card::KIND,
        ));
        state.cal_placement = Arc::new(crate::segments::CalPlacementSegmentManager::new(
            dir.clone(),
        ));
        state.card_placement = Arc::new(crate::segments::CardPlacementSegmentManager::new(dir));
        (tmp, Arc::new(state))
    }

    /// One sealed mail record as a segment holds it: its outer envelope bytes,
    /// the CID derived from them, and its floor.
    fn record(tag: &str, received_at: i64) -> Candidate {
        let envelope = fauna_mail::segments::MailRecordEnvelope::new(
            format!("sealed-body-{tag}").into_bytes(),
            b"sealed-index-hint".to_vec(),
        );
        let (cid, bytes) = fauna_mail::segments::ops::encode_record(&envelope).unwrap();
        Candidate {
            cid,
            bytes,
            floor: floor(received_at).encode().unwrap(),
            bucket: fauna_segment_store::bucket_for(received_at / 1000),
        }
    }

    /// Land `c` on the target as an ordinary arrival would (the relay's
    /// verbatim carrier) — the target's own copy of a record.
    async fn hold(state: &Arc<AppState>, c: &Candidate) -> u32 {
        crate::segments::mail::append_sealed_record(
            &state.mail_segments,
            &state.db,
            &OWNER,
            &c.bytes,
            MailFloorMetadata::decode(&c.floor).unwrap(),
        )
        .await
        .unwrap()
        .seg_id
    }

    fn copy(c: &Candidate) -> Candidate {
        Candidate {
            cid: c.cid,
            bytes: c.bytes.clone(),
            floor: c.floor.clone(),
            bucket: c.bucket.clone(),
        }
    }

    /// `(mailbox, uid, flags)` of every placement row for `c`, and whether the
    /// mirror holds a live row for it.
    async fn filed(state: &Arc<AppState>, c: &Candidate) -> (Vec<(String, u32, String)>, bool) {
        let conn = state.db.conn().await;
        let mut stmt = conn
            .prepare(
                "SELECT mailbox, uid, flags FROM bridge_imap_messages \
                 WHERE actor_id = ?1 AND message_id = ?2 ORDER BY mailbox",
            )
            .unwrap();
        let rows = stmt
            .query_map(
                rusqlite::params![OWNER.as_slice(), c.cid.digest().as_slice()],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32, r.get(2)?)),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let live = crate::segments::records_db::lookup_record(&conn, &OWNER, "mail", &c.cid)
            .unwrap()
            .is_some();
        (rows, live)
    }

    fn placement(c: &Candidate, mailbox: &str, flags: &[&str]) -> (Vec<u8>, Vec<RecordPlacement>) {
        (
            c.cid.digest().to_vec(),
            vec![RecordPlacement {
                mailbox: mailbox.to_string(),
                uid: 77,
                modseq: 9,
                flags: flags.iter().map(|f| f.to_string()).collect(),
                content_record_id: c.cid.digest().to_vec(),
                internal_date: 1_715_000_000_000,
            }],
        )
    }

    /// **The target's own history wins — live AND tombstoned.** A record the
    /// target holds live is not doubled, and a record the owner deleted after
    /// the rollback (tombstoned here, still in the backup) is not resurrected.
    /// Mutation pinned: a live-only held check recovers the deleted message.
    #[tokio::test]
    async fn a_record_the_target_holds_live_or_tombstoned_is_never_recovered() {
        let (_tmp, state) = nest();
        let kept = record("kept", 1_715_000_000_000);
        let deleted = record("deleted", 1_715_000_100_000);
        let lost = record("lost", 1_715_000_200_000);
        hold(&state, &kept).await;
        let deleted_seg = hold(&state, &deleted).await;
        state
            .db
            .segment_records_mark_tombstoned(&OWNER, "mail", deleted_seg, &deleted.cid)
            .await
            .unwrap();
        // Sealed, as a lived-in scope's older mail is: a resurrection would then
        // land cleanly in a fresh segment, and only the held check stops it.
        state.mail_segments.finalize_open(&OWNER).await.unwrap();

        let Landed { outcome, whole } = land(
            &state,
            &OWNER,
            Arm::Mail,
            vec![copy(&kept), copy(&deleted), copy(&lost)],
            &Placements::Mail(HashMap::new()),
            None,
        )
        .await
        .unwrap();

        assert!(whole);
        assert_eq!((outcome.recovered, outcome.already_held), (1, 2));
        assert!(
            filed(&state, &lost).await.1,
            "the lost record is live again"
        );
        let conn = state.db.conn().await;
        let live_deleted =
            crate::segments::records_db::lookup_record(&conn, &OWNER, "mail", &deleted.cid)
                .unwrap();
        assert!(live_deleted.is_none(), "the owner's deletion stands");
    }

    /// **Where a recovered record lands.** One the newest delivered journal
    /// names goes back to that mailbox with its flags, under a UID the target
    /// mints; one no journal names lands in the inbox unseen — the arrival's own
    /// landing. Both are journaled, so the placement survives a journal rebuild.
    #[tokio::test]
    async fn a_named_record_is_filed_where_it_was_and_an_unnamed_one_lands_in_the_inbox() {
        let (_tmp, state) = nest();
        let named = record("named", 1_715_000_000_000);
        let unnamed = record("unnamed", 1_715_000_100_000);
        let placements: HashMap<_, _> = [placement(&named, "Archive", &["\\Seen"])].into();

        let Landed { outcome, .. } = land(
            &state,
            &OWNER,
            Arm::Mail,
            vec![copy(&named), copy(&unnamed)],
            &Placements::Mail(placements),
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            (outcome.recovered, outcome.filed, outcome.inboxed),
            (2, 1, 1)
        );
        let (rows, live) = filed(&state, &named).await;
        assert!(live);
        assert_eq!(rows, vec![("Archive".to_string(), 1, "\\Seen".to_string())]);
        let (rows, live) = filed(&state, &unnamed).await;
        assert!(live);
        assert_eq!(rows, vec![("INBOX".to_string(), 1, String::new())]);

        let journal = state.mail_placement.current_manifest(&OWNER).await.unwrap();
        let mut journaled: Vec<(String, Vec<u8>)> = journal
            .placements
            .iter()
            .map(|p| (p.mailbox.clone(), p.content_record_id.clone()))
            .collect();
        journaled.sort();
        assert_eq!(
            journaled,
            vec![
                ("Archive".to_string(), named.cid.digest().to_vec()),
                ("INBOX".to_string(), unnamed.cid.digest().to_vec()),
            ]
        );
    }

    /// **The mirror row and the placement commit together or not at all.** The
    /// placement is made to fail (a row already sits at the UID the mailbox
    /// would mint next); the record must not then be live and in no mailbox.
    /// Mutation pinned: committing the mirror row before placing leaves it live.
    #[tokio::test]
    async fn a_failed_placement_leaves_no_live_record_behind() {
        let (_tmp, state) = nest();
        let lost = record("lost", 1_715_000_000_000);
        state.db.ensure_bridge_imap_mailboxes(&OWNER).await.unwrap();
        {
            let conn = state.db.conn().await;
            conn.execute(
                "INSERT INTO bridge_imap_messages \
                 (actor_id, mailbox, uid, message_id, internal_date, created_at) \
                 VALUES (?1, 'INBOX', 1, ?2, 0, 0)",
                rusqlite::params![OWNER.as_slice(), [0xEEu8; 32].as_slice()],
            )
            .unwrap();
        }

        let err = land(
            &state,
            &OWNER,
            Arm::Mail,
            vec![copy(&lost)],
            &Placements::Mail(HashMap::new()),
            None,
        )
        .await
        .err()
        .expect("the placement collides, so the record fails");
        assert!(matches!(err, MaterializeRefusal::Integrity(_)), "{err}");
        let (rows, live) = filed(&state, &lost).await;
        assert!(rows.is_empty());
        assert!(
            !live,
            "no mirror row survives a placement that did not commit"
        );
    }

    /// **A torn run resumes to the same end state.** A run cut after one record
    /// leaves one ordinary record live and filed; the re-run lands the rest and
    /// nothing twice; a third run recovers nothing.
    #[tokio::test]
    async fn a_torn_run_resumes_and_a_repeat_recovers_nothing() {
        let (_tmp, state) = nest();
        let set: Vec<Candidate> = (0..3)
            .map(|i| record(&format!("lost-{i}"), 1_715_000_000_000 + i * 1000))
            .collect();
        let batch = || set.iter().map(copy).collect::<Vec<_>>();
        let none = Placements::Mail(HashMap::new());

        let torn = land(&state, &OWNER, Arm::Mail, batch(), &none, Some(1))
            .await
            .unwrap();
        assert!(!torn.whole);
        assert_eq!(torn.outcome.recovered, 1);

        let resumed = land(&state, &OWNER, Arm::Mail, batch(), &none, None)
            .await
            .unwrap();
        assert!(resumed.whole);
        assert_eq!(
            (resumed.outcome.recovered, resumed.outcome.already_held),
            (2, 1)
        );

        let repeat = land(&state, &OWNER, Arm::Mail, batch(), &none, None)
            .await
            .unwrap();
        assert_eq!(
            (repeat.outcome.recovered, repeat.outcome.already_held),
            (0, 3)
        );

        let mut uids = Vec::new();
        for c in &set {
            let (rows, live) = filed(&state, c).await;
            assert!(live);
            assert_eq!(rows.len(), 1, "each record filed exactly once");
            uids.push(rows[0].1);
        }
        uids.sort_unstable();
        assert_eq!(uids, vec![1, 2, 3]);
    }

    // ── The calendar and contacts arms ───────────────────────────────────

    use fauna_calendar::segments::placement::{
        CalPlacementManifest, CalendarState, EventPlacement, EventTombstoneRef,
    };
    use fauna_calendar::segments::{CalFloorMetadata, CalRecordEnvelope};

    const CAL: [u8; 32] = [0xCA; 32];
    /// The lost records' own server receive time (epoch seconds); every
    /// target-side act is placed relative to it.
    const T0: i64 = 1_760_000_000;

    fn uid(n: u8) -> [u8; 32] {
        [n; 32]
    }

    /// One sealed calendar record as a segment holds it, and its `event_id`.
    fn event(
        tag: &str,
        calendar: [u8; 32],
        uid_hash: [u8; 32],
        created_at: i64,
    ) -> (Candidate, [u8; 32]) {
        let body = format!("sealed-event-{tag}").into_bytes();
        let (cid, bytes) = CalRecordEnvelope::new(body.clone(), b"sealed-hint".to_vec())
            .encode_record()
            .unwrap();
        let id = *blake3::hash(tag.as_bytes()).as_bytes();
        let floor = CalFloorMetadata {
            calendar_id: calendar,
            event_id: id,
            uid_hash: uid_hash.to_vec(),
            ciphertext_size: body.len() as u32,
            internal_date: 1_900_000_000,
            created_at,
            ..Default::default()
        };
        let c = Candidate {
            cid,
            bytes,
            floor: floor.encode().unwrap(),
            bucket: fauna_segment_store::bucket_for(created_at),
        };
        (c, id)
    }

    fn placed(id: [u8; 32], calendar: [u8; 32], uid_hash: [u8; 32], modseq: u64) -> EventPlacement {
        EventPlacement {
            calendar_id: calendar,
            uid_hash,
            etag: format!("{modseq:016}"),
            modseq,
            ciphertext_size: 9,
            event_id: id,
            encrypted_fauna_ext: Some(format!("sidecar-{modseq}").into_bytes()),
        }
    }

    fn deleted(
        id: [u8; 32],
        calendar: [u8; 32],
        uid_hash: [u8; 32],
        modseq: u64,
    ) -> EventTombstoneRef {
        EventTombstoneRef {
            calendar_id: calendar,
            uid_hash,
            modseq,
            event_id: id,
            deleted_at: T0,
        }
    }

    /// One delivered calendar journal generation.
    fn generation(
        calendars: &[([u8; 32], &[u8], u64)],
        events: Vec<EventPlacement>,
        tombstones: Vec<EventTombstoneRef>,
    ) -> CalPlacementManifest {
        let mut m = CalPlacementManifest::new();
        m.calendars = calendars
            .iter()
            .map(|(id, meta, hms)| CalendarState {
                calendar_id: *id,
                encrypted_metadata: meta.to_vec(),
                highestmodseq: *hms,
            })
            .collect();
        m.events = events;
        m.tombstones = tombstones;
        m
    }

    fn fold_of(generations: &[&CalPlacementManifest]) -> Placements {
        let mut fold = DavFold::default();
        for g in generations {
            fold.absorb_cal(g);
        }
        Placements::Dav(fold)
    }

    /// Seed a live event row on the target, born at `born` (its `created_at`)
    /// — the target's own version of a resource. Returns its `event_id`.
    async fn seed_row(
        state: &Arc<AppState>,
        calendar: [u8; 32],
        uid_hash: [u8; 32],
        tag: &str,
        born: i64,
    ) -> [u8; 32] {
        match state
            .db
            .place_caldav_event(
                &OWNER,
                &calendar,
                &uid_hash,
                tag.as_bytes(),
                b"hint",
                born,
                9,
                born,
            )
            .await
            .unwrap()
        {
            crate::db::bridge_caldav::PlaceCaldavEventOutcome::Created { event_id, .. } => event_id,
            other => panic!("seed row: {other:?}"),
        }
    }

    /// `(uid_hash, event_id, modseq)` of every live event row in `calendar`.
    async fn event_rows(
        state: &Arc<AppState>,
        calendar: [u8; 32],
    ) -> Vec<(Vec<u8>, [u8; 32], i64)> {
        let conn = state.db.conn().await;
        let mut stmt = conn
            .prepare(
                "SELECT uid_hash, event_id, modseq FROM bridge_caldav_events \
                 WHERE actor_id = ?1 AND calendar_id = ?2 ORDER BY uid_hash",
            )
            .unwrap();
        stmt.query_map(
            rusqlite::params![OWNER.as_slice(), calendar.as_slice()],
            |r| {
                let id: Vec<u8> = r.get(1)?;
                Ok((r.get(0)?, id.try_into().unwrap(), r.get(2)?))
            },
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    }

    async fn provision(state: &Arc<AppState>, calendar: [u8; 32], meta: &[u8]) {
        state
            .db
            .insert_bridge_caldav_calendar(&OWNER, &calendar, meta, T0 - 10_000)
            .await
            .unwrap();
    }

    async fn land_cal(
        state: &Arc<AppState>,
        records: &[&Candidate],
        p: &Placements,
    ) -> RecoverOutcome {
        let Landed { outcome, whole } = land(
            state,
            &OWNER,
            Arm::Calendar,
            records.iter().map(|c| copy(c)).collect(),
            p,
            None,
        )
        .await
        .unwrap();
        assert!(whole);
        outcome
    }

    /// **The fold: the highest change number per resource, and absence never
    /// deletes — in any order.** Across three generations: a placement above
    /// a later generation's lower one stays current (U1); a resource the
    /// newest generation is silent about keeps its placement (U2); a
    /// tombstone above a placement retires it (U3); a placement above a
    /// tombstone is a re-add (U4). Mutations pinned: "the newest generation
    /// wins" breaks U1 in one order, "each generation replaces the fold"
    /// breaks U2, dropping the tombstone arm resurrects U3.
    #[test]
    fn the_fold_keeps_each_resources_highest_change_number_in_any_order() {
        let [a, b, c, d, e] = [[0xA1; 32], [0xB1; 32], [0xC1; 32], [0xD1; 32], [0xE1; 32]];
        let old = generation(
            &[(CAL, b"meta", 7)],
            vec![
                placed(a, CAL, uid(1), 5),
                placed(b, CAL, uid(2), 3),
                placed(c, CAL, uid(3), 7),
            ],
            vec![deleted(d, CAL, uid(4), 2)],
        );
        let mid = generation(
            &[(CAL, b"meta", 9)],
            vec![placed(c, CAL, uid(3), 7)],
            vec![deleted(c, CAL, uid(3), 9)],
        );
        // The post-regression generation: its counter reused 4 for another
        // version of U1, and it knows nothing of U2.
        let new = generation(
            &[(CAL, b"meta", 6)],
            vec![
                placed([0xF1; 32], CAL, uid(1), 4),
                placed(e, CAL, uid(4), 6),
            ],
            vec![],
        );
        let expect: HashSet<[u8; 32]> = [a, b, e].into();
        for order in [[&old, &mid, &new], [&new, &mid, &old], [&mid, &new, &old]] {
            let Placements::Dav(fold) = fold_of(&order) else {
                unreachable!()
            };
            let current: HashSet<[u8; 32]> = fold.current().into_keys().collect();
            assert_eq!(current, expect);
            assert_eq!(fold.collections[&CAL].highestmodseq, 9);
        }
    }

    /// **Collision (a): the later write wins, by server receive time.** A row
    /// born before the lost record (U1) is the version it superseded and is
    /// replaced through the replace-by-UID door, the old id tombstoned into
    /// the expunge table; a row born after it (U2) is the owner's
    /// post-regression act and stands (`already_held`); a tie in seconds (U3)
    /// files. Mutations pinned: `>=` for `>` keeps U3's stale row; not reading
    /// the live row replaces U2's.
    #[tokio::test]
    async fn a_lost_version_replaces_an_older_row_and_a_later_row_stands() {
        let (_tmp, state) = nest();
        provision(&state, CAL, b"meta").await;
        let stale1 = seed_row(&state, CAL, uid(1), "stale-1", T0 - 100).await;
        let later2 = seed_row(&state, CAL, uid(2), "later-2", T0 + 100).await;
        let _tie3 = seed_row(&state, CAL, uid(3), "tie-3", T0).await;
        let (l1, id1) = event("lost-1", CAL, uid(1), T0);
        let (l2, id2) = event("lost-2", CAL, uid(2), T0);
        let (l3, id3) = event("lost-3", CAL, uid(3), T0);
        let journal = fold_of(&[&generation(
            &[(CAL, b"meta", 12)],
            vec![
                placed(id1, CAL, uid(1), 10),
                placed(id2, CAL, uid(2), 11),
                placed(id3, CAL, uid(3), 12),
            ],
            vec![],
        )]);

        let outcome = land_cal(&state, &[&l1, &l2, &l3], &journal).await;

        assert_eq!(
            (
                outcome.recovered,
                outcome.filed,
                outcome.already_held,
                outcome.unplaceable
            ),
            (2, 2, 1, 0)
        );
        let rows: Vec<(Vec<u8>, [u8; 32])> = event_rows(&state, CAL)
            .await
            .into_iter()
            .map(|(u, id, _)| (u, id))
            .collect();
        assert_eq!(
            rows,
            vec![
                (uid(1).to_vec(), id1),
                (uid(2).to_vec(), later2),
                (uid(3).to_vec(), id3)
            ]
        );
        let conn = state.db.conn().await;
        let expunged: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bridge_caldav_expunged \
                 WHERE actor_id = ?1 AND event_id = ?2)",
                rusqlite::params![OWNER.as_slice(), stale1.as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(expunged, "the superseded row's id is in the expunge table");
        let held2 =
            crate::segments::records_db::held_ever(&conn, &OWNER, "calendar", &l2.cid).unwrap();
        assert!(
            !held2,
            "a version the owner's later write superseded is not appended"
        );
    }

    /// **The target's own expunge reads the same way.** A deletion stamped
    /// after the lost record's birth (U1) is the owner's post-regression
    /// delete and stands; one stamped before it (U2) is history the lost
    /// re-add overrode, and the record files. Mutation pinned: ignoring the
    /// expunge table resurrects U1.
    #[tokio::test]
    async fn an_expunge_after_the_lost_record_stands_and_an_older_one_does_not() {
        let (_tmp, state) = nest();
        provision(&state, CAL, b"meta").await;
        for (n, deleted_at) in [(1u8, T0 + 50), (2u8, T0 - 50)] {
            seed_row(&state, CAL, uid(n), &format!("old-{n}"), T0 - 200).await;
            state
                .db
                .delete_caldav_event_by_uid(&OWNER, &CAL, &uid(n), None, deleted_at)
                .await
                .unwrap();
        }
        let (l1, id1) = event("lost-1", CAL, uid(1), T0);
        let (l2, id2) = event("lost-2", CAL, uid(2), T0);
        let journal = fold_of(&[&generation(
            &[(CAL, b"meta", 8)],
            vec![placed(id1, CAL, uid(1), 7), placed(id2, CAL, uid(2), 8)],
            vec![],
        )]);

        let outcome = land_cal(&state, &[&l1, &l2], &journal).await;

        assert_eq!((outcome.recovered, outcome.already_held), (1, 1));
        let rows: Vec<[u8; 32]> = event_rows(&state, CAL)
            .await
            .into_iter()
            .map(|r| r.1)
            .collect();
        assert_eq!(rows, vec![id2]);
    }

    /// **Collision (b): a collection the target lacks is created by id with
    /// the fold's metadata, and journaled; one it holds under other metadata
    /// keeps the target's.** Both then take their recovered event, and the
    /// journal carries the provision and both `PutEvent`s. Mutation pinned:
    /// skipping the creation leaves the door `CalendarMissing` and the run
    /// fails.
    #[tokio::test]
    async fn a_missing_collection_is_created_by_id_and_journaled() {
        let (_tmp, state) = nest();
        let gone: [u8; 32] = [0x60; 32];
        provision(&state, CAL, b"renamed-after").await;
        let (l1, id1) = event("lost-1", gone, uid(1), T0);
        let (l2, id2) = event("lost-2", CAL, uid(2), T0);
        let journal = fold_of(&[&generation(
            &[(gone, b"gone-meta", 3), (CAL, b"meta-before", 3)],
            vec![placed(id1, gone, uid(1), 2), placed(id2, CAL, uid(2), 3)],
            vec![],
        )]);

        let outcome = land_cal(&state, &[&l1, &l2], &journal).await;

        assert_eq!(outcome.recovered, 2);
        let calendars: HashMap<[u8; 32], Vec<u8>> = state
            .db
            .list_bridge_caldav_calendars(&OWNER)
            .await
            .unwrap()
            .into_iter()
            .map(|c| (c.calendar_id, c.encrypted_metadata))
            .collect();
        assert_eq!(calendars[&gone], b"gone-meta".to_vec());
        assert_eq!(calendars[&CAL], b"renamed-after".to_vec());
        assert_eq!(event_rows(&state, gone).await[0].1, id1);

        let journaled = state.cal_placement.current_manifest(&OWNER).await.unwrap();
        let journaled_calendars: Vec<[u8; 32]> =
            journaled.calendars.iter().map(|c| c.calendar_id).collect();
        assert_eq!(
            journaled_calendars,
            vec![gone],
            "only the created one is provisioned"
        );
        let mut journaled_events: Vec<[u8; 32]> =
            journaled.events.iter().map(|e| e.event_id).collect();
        journaled_events.sort();
        let mut expect = vec![id1, id2];
        expect.sort();
        assert_eq!(journaled_events, expect);
    }

    /// **Collision (c): the change counter is floored, never restored.** The
    /// target's calendar came back at a low counter; the fold pins the
    /// pre-regression one at 40; the recovered event is numbered above it, and
    /// the `ctag` moves in lockstep. Mutation pinned: skipping the floor
    /// numbers the event 2, inside a range a client's sync token already
    /// covers.
    #[tokio::test]
    async fn the_collection_counter_is_floored_before_the_first_record_files() {
        let (_tmp, state) = nest();
        provision(&state, CAL, b"meta").await;
        let (l1, id1) = event("lost-1", CAL, uid(1), T0);
        let journal = fold_of(&[&generation(
            &[(CAL, b"meta", 40)],
            vec![placed(id1, CAL, uid(1), 40)],
            vec![],
        )]);

        land_cal(&state, &[&l1], &journal).await;

        assert_eq!(event_rows(&state, CAL).await[0].2, 41);
        let cal = state
            .db
            .list_bridge_caldav_calendars(&OWNER)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.calendar_id == CAL)
            .unwrap();
        assert_eq!((cal.highestmodseq, cal.ctag), (41, 41));
    }

    /// **A record the fold does not name as its resource's current version is
    /// `unplaceable`: counted, not appended.** One superseded before the
    /// regression, one deleted before it, one never journaled. Mutation
    /// pinned: placing a record under its floor's resource regardless files
    /// a second version of one UID.
    #[tokio::test]
    async fn a_record_the_fold_does_not_name_current_is_unplaceable() {
        let (_tmp, state) = nest();
        provision(&state, CAL, b"meta").await;
        let (superseded, sup_id) = event("superseded", CAL, uid(1), T0);
        let (removed, rm_id) = event("removed", CAL, uid(2), T0);
        let (unjournaled, _) = event("unjournaled", CAL, uid(3), T0);
        let journal = fold_of(&[&generation(
            &[(CAL, b"meta", 6)],
            vec![
                placed(sup_id, CAL, uid(1), 3),
                placed([0x99; 32], CAL, uid(1), 5),
            ],
            vec![deleted(rm_id, CAL, uid(2), 6)],
        )]);

        let outcome = land_cal(&state, &[&superseded, &removed, &unjournaled], &journal).await;

        assert_eq!(
            (outcome.recovered, outcome.unplaceable, outcome.already_held),
            (0, 3, 0)
        );
        assert!(event_rows(&state, CAL).await.is_empty());
        let conn = state.db.conn().await;
        for c in [&superseded, &removed, &unjournaled] {
            assert!(
                !crate::segments::records_db::held_ever(&conn, &OWNER, "calendar", &c.cid).unwrap()
            );
        }
    }

    /// **The contacts arm is the calendar arm's twin.** A lost card files into
    /// its address book — created by id, counter floored — and its `PutCard`
    /// is journaled; a re-run recovers nothing.
    #[tokio::test]
    async fn a_lost_card_files_into_its_recreated_address_book() {
        use fauna_contacts::segments::placement::{
            AddressbookState, CardPlacement, CardPlacementManifest,
        };
        use fauna_contacts::segments::{CardFloorMetadata, CardRecordEnvelope};
        let (_tmp, state) = nest();
        let book: [u8; 32] = [0xBB; 32];
        let (cid, bytes) = CardRecordEnvelope::new(b"sealed-card".to_vec(), b"hint".to_vec())
            .encode_record()
            .unwrap();
        let card_id = [0xC0; 32];
        let floor = CardFloorMetadata {
            addressbook_id: book,
            card_id,
            uid_hash: uid(1).to_vec(),
            ciphertext_size: 11,
            internal_date: T0,
            created_at: T0,
            ..Default::default()
        };
        let c = Candidate {
            cid,
            bytes,
            floor: floor.encode().unwrap(),
            bucket: fauna_segment_store::bucket_for(T0),
        };
        let mut m = CardPlacementManifest::new();
        m.addressbooks.push(AddressbookState {
            addressbook_id: book,
            encrypted_metadata: b"book-meta".to_vec(),
            highestmodseq: 20,
        });
        m.cards.push(CardPlacement {
            addressbook_id: book,
            uid_hash: uid(1),
            etag: "e".into(),
            modseq: 20,
            ciphertext_size: 11,
            card_id,
            encrypted_fauna_ext: None,
        });
        let mut fold = DavFold::default();
        fold.absorb_card(&m);
        let journal = Placements::Dav(fold);

        let first = land(&state, &OWNER, Arm::Card, vec![copy(&c)], &journal, None)
            .await
            .unwrap()
            .outcome;
        assert_eq!((first.recovered, first.filed), (1, 1));
        let second = land(&state, &OWNER, Arm::Card, vec![copy(&c)], &journal, None)
            .await
            .unwrap()
            .outcome;
        assert_eq!((second.recovered, second.already_held), (0, 1));

        let books = state
            .db
            .list_bridge_carddav_addressbooks(&OWNER)
            .await
            .unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(
            (
                books[0].encrypted_metadata.as_slice(),
                books[0].highestmodseq
            ),
            (&b"book-meta"[..], 21)
        );
        let journaled = state.card_placement.current_manifest(&OWNER).await.unwrap();
        assert_eq!(journaled.cards.len(), 1);
        assert_eq!(
            (journaled.cards[0].card_id, journaled.cards[0].modseq),
            (card_id, 21)
        );
    }
}
