//! `fauna.backup.custody.materialize` — flip one custody set from **destination
//! posture** to **live source posture**. Both arms: the segment axis
//! ([`materialize_segment_set`]) and the covered-folder axis
//! ([`materialize_folder_set`]).
//!
//! Goal docs: `docs/goal/behavior/backup-destinations.md` § Third destination
//! kind → *Re-seed* (phase 3 — the ceremony, the authorization shape and the
//! empty-target rule) and `docs/goal/architecture/message-segment-store.md`
//! § Client-device custodian (pull) → *Restore* (the wire mechanics this
//! implements).
//!
//! ## What "destination posture" means here
//!
//! A backup destination holds the owner's corpus as **opaque sealed chunks plus
//! per-path custody rows** — bytes it can prove it holds and can hand back, but
//! cannot read and does not serve. Nothing about that posture makes the account
//! live: there are no `segment_records` rows, no segment files in the scope's
//! segment area, and the mail read path finds nothing.
//!
//! Materialize is the one gesture that changes that, and it does exactly one
//! thing: it **adds**. On the segment axis it unseals what the destination
//! already holds under the key the owner already granted, verifies both halves
//! of every segment against the hashes the source itself published, writes the
//! pair into the scope's segment area, and re-asserts the `segment_records`
//! mirror from the sidecar's own footers. It deletes nothing that holds or
//! ever held content — see [`ScopeNotEmpty`].
//!
//! ## Every backed-up kind, one arm each
//!
//! The segment axis reconstitutes every kind the sweep backs up
//! (`BACKED_UP_KINDS`: mail, posts, calendar, contacts) through the same
//! verify-then-write body; [`KindArm`] is the whole of what differs between
//! them — which segment store, which placement tables, whether a journal rides
//! the set, and which of the snapshot restore's own rebuilds re-asserts the
//! rows.
//!
//! ## Both families of the kind, in one transaction
//!
//! A backed-up message kind's corpus is its content segments **and** its
//! placement journal (`backup-destinations.md` § Third destination kind →
//! *Where restored mail lands*), and the journal rides the same set. So this
//! verb restores both: it verifies every pair of both families before writing
//! any, reconstitutes the content, adopts the journal
//! (`segments::PlacementJournal::adopt_staged`), and commits the mirror rebuild
//! together with the placement rows. One transaction is the point — no crash
//! leaves mail live and in no mailbox, and no event live in no calendar. Posts
//! have no placement layer; their mirror rows and feed projection commit in the
//! same one transaction.
//!
//! The journal is where the ceremony's **one replacement** happens. A target
//! that has never placed a message may still hold mailbox scaffolding (the
//! standard mailboxes, a folder a mail client created on connect), and the
//! corpus's mailbox tree supersedes it. A target whose mailboxes have ever held
//! mail is refused instead — [`PlacementNotFresh`]. Calendar and contacts
//! follow the same rule over their own journals.
//!
//! Every journaled kind — mail, calendar, contacts — rides its journal in the
//! set, so a copy holding content and no journal is a delivery that has not
//! finished, and is refused as one.
//!
//! ## An interrupted flip completes on retry — the in-flight marker
//!
//! The verb runs under the dispatcher's deadline and abort, and a nest can die
//! at any write, so a flip can be torn between its first write and its commit
//! while mail keeps arriving. The empty-target rule is therefore judged
//! **once, at the gesture** (`backup-destinations.md` § Third destination kind
//! → *Re-seed* → *An interrupted flip completes on retry, whatever arrived
//! since — the in-flight marker*): before its first write anything else can
//! observe, the verb records a `materialize_in_flight` row naming the set and
//! the corpus's segment ids, and the commit transaction deletes it. While it
//! stands, a retry asks no freshness question and keeps everything live
//! outside those ids. Three rules make the keep exact: the content counter is
//! **floored** to the corpus's ledger generation first, so an arrival's
//! segment never takes or reclaims a corpus id; a journaled mail copy writes
//! the corpus's **mailbox tree before the halves**, so an arrival mints its UID
//! above the corpus's under the corpus's UIDVALIDITY; and the corpus's journal
//! **supersedes** the target's, with every post-gesture placement **re-filed**
//! into it under the UID it holds. The marker is no force arm, never expires,
//! and is never finished at boot. Calendar and contacts keep the same three
//! rules over their own containers and journals: the corpus's calendars and
//! address books are written before the halves, so an arrival's change number
//! is minted above the corpus's, and an arrival is re-filed as the current
//! version of its `(collection, UID)` resource under the change number it was
//! served — a resource the corpus also holds going to the later write by
//! server receive time, recovery's collision rule (a) (`recover.rs`'s
//! [`super::recover::DavFold`]).
//!
//! ## The two axes, and why only one of them holds a key
//!
//! A segment corpus rests sealed under the owner's `NestBackupKey` root, so
//! reconstituting it is an unseal-verify-write. A **covered folder's** mirror is
//! the source's own at-rest ciphertext, pushed as-is — the manifests and chunks
//! already ARE the folder corpus — so its materialize is **pure row re-homing,
//! no keys anywhere**: re-create the folder set, mint one live record per
//! custody row from `(path_hash, path_sealed, manifest_hash)`, touch not one
//! byte of content. Same verb, same empty-target rule, same "deletes nothing";
//! the axes share this module because they share a contract, not a mechanism.
//!
//! ## Why both restore sources share this code
//!
//! Delivered custody is byte-identical whether the bytes arrived from a source
//! nest's own coordinator (the nest-kind destination) or from the owner's device
//! re-sealing its held corpus (the custodian re-seed leg,
//! `fauna_sync_engine::reseed`). The ceremony's whole uniformity payoff is that
//! after delivery the two are indistinguishable, so this verb serves both — it
//! reads custody rows and a `manifest.<kind>` mirror, and asks nothing about who
//! wrote them.
//!
//! [`ScopeNotEmpty`]: MaterializeRefusal::ScopeNotEmpty
//! [`PlacementNotFresh`]: MaterializeRefusal::PlacementNotFresh

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context as _;
use fauna_core::crypto::{NestBackupKey, OwnerSealKey};
use fauna_core::data::{ContentHash, parse_folder_backup_set_name};
use fauna_core::file_download::{BlobFetcher, FileDownloadKeys, download_file_bytes_by_manifest};
use fauna_protocol::segments::SegmentRef;
use fauna_sync_engine::segment_backup::{
    LiveManifestMirror, SegmentFamily, parse_reserved_backup_set_name, verify_meta_against,
};

use super::recover::{DavFold, DavRecord, DavVersion, dav_journal, held_write_stands};
use crate::db::nest_backup_keys::NEST_BACKUP_KEY_LEN;
use crate::routes::AppState;

/// Every way materialize declines, each a **typed** refusal the handler renders
/// as its own wire code. None of them mutates anything: the checks that can fail
/// all run before the first byte is written, and the write itself is one SQLite
/// transaction plus a segment-area write that only ever *adds* files.
#[derive(Debug)]
pub(crate) enum MaterializeRefusal {
    /// The set name is neither axis: not one [`parse_reserved_backup_set_name`]
    /// could have produced, and not one
    /// [`parse_folder_backup_set_name`] could have either. A typo, or an
    /// ordinary user folder name that happens to be in the caller's custody.
    NotASegmentSet { set_name: String },
    /// A folder-axis set was named with neither a target address
    /// (`folder_name_hash`) nor a display name.
    ///
    /// Custody deliberately never carried the label — a mirror holds the
    /// folder's sealed *paths*, and the set name carries only the source nest id
    /// and the source's folder rowid, neither of which means anything on the
    /// target. So the target's address rides the verb payload, from the driving
    /// custodian's own store, and its absence is a refusal rather than a guess.
    FolderNameRequired { set_name: String },
    /// The request's `folder_name_hash` is not the hash of its
    /// `folder_display_name` (or is not 32 bytes): the two would name different
    /// sets, so the request is refused rather than one of them believed.
    FolderNameHashMismatch { folder: String },
    /// **The empty-target rule, folder axis.** The named folder already holds
    /// live records this ceremony did not write, so it is a lived-in folder and
    /// not a fresh target. Same rule and the same wire code as
    /// [`Self::ScopeNotEmpty`], and the same absence of a force arm; a separate
    /// variant only because the thing it names is a folder rather than a scope.
    FolderNotEmpty { folder: String, records: i64 },
    /// **The empty-target rule's other half: empty, but not fresh.** The named
    /// folder holds no records, but it carries `property` — a publication-
    /// bearing column that already gives some party other than the owner a view
    /// of the folder, or write authority over it.
    ///
    /// Its own wire code rather than [`Self::FolderNotEmpty`]'s, because the two
    /// have opposite next steps and the handler's whole refusal catalogue is
    /// organised around that: `target_not_empty` means "this account is already
    /// live, you are pointing at the wrong nest", which would send an owner
    /// hunting for records that are genuinely not there. What they actually have
    /// to do is name a different folder or clear the property — so the refusal
    /// names which property it was.
    ///
    /// No force arm here either. Re-homing into a shared folder would hand every
    /// roster member the restored corpus's metadata (`folder_authz`'s read grant
    /// is per folder, not per row) and let a writer member tombstone it —
    /// destroying the corpus the owner just restored.
    FolderNotFresh {
        folder: String,
        property: &'static str,
    },
    /// **A folder-set request carrying no owner signatures.** Every re-homed
    /// row is owner-signed by the ceremony (`writer-signed-change-records.md`
    /// ruling (7)(a)(ii)) — the arm never mints an unsigned row, which every
    /// reader would refuse. Wire code `signature_required`.
    FolderRehomeUnsigned { set_name: String },
    /// **The target set does not exist.** The ceremony's seed-holding process
    /// creates it through the shared create helper (custody first, nonce
    /// minted) before materializing; the arm never creates a folder (ruling
    /// (7)(a)(i)). Wire code `target_missing` — the remedy is the caller
    /// preparing the set.
    FolderTargetMissing { folder: String },
    /// **A carried re-home signature was refused** — it does not verify under
    /// the target's stored nonce, the target has no stored nonce, or the signer
    /// is not the owner's. The whole page is refused, writing nothing. Wire
    /// code the record door's own (`signature_invalid` / `author_mismatch`).
    RehomeSignature {
        folder: String,
        refusal: crate::change_signature::IngestRefusal,
    },
    /// **A malformed page**: a signature naming no custody row of the set, a
    /// path named twice, a page over `MATERIALIZE_REHOME_PAGE`, or a
    /// mis-sized field. The whole page is refused, writing nothing.
    RehomePageMalformed { detail: String },
    /// A custody row carries no `path_sealed`, so it cannot become a live row.
    ///
    /// Legitimate and recoverable, not corruption: a custodian that pulled
    /// before the 2026-08-23 widening holds the bytes for a path whose content
    /// has not changed since, with no sealed name to go with them. Its delivery
    /// leg pushes the row anyway — the bytes are real custody — and reports the
    /// gap rather than counting the folder whole
    /// (`ReseedReport::folder_paths_without_seal`).
    ///
    /// Refused **here**, by its own code, rather than deeper: a live folder set
    /// is not one of the resting-plaintext classes, so minting the row would
    /// fail at `record_change_core`'s `path_seal_required` — one layer too late
    /// to say anything the owner could act on. The recovery is a fresh pull pass
    /// on the custodian then a re-delivery, and it moves no bytes: the write
    /// side already COALESCEs a newly-arriving sealed name onto the existing
    /// row.
    CustodyPathUnsealed { path_hash: String },
    /// A custody row carries no plaintext `path`, so the **source's** path hash
    /// — which the row's own `path_hash` is a hash *of*, not a copy of — cannot
    /// be recovered, and there is no address to re-home the row to.
    ///
    /// Structurally unreachable for a row either push arm wrote (a reserved
    /// backup set rests its machine-authored path, which on this axis is the
    /// source `path_hash` hex-spelled). Refusing names it rather than skipping
    /// it: a silently-dropped path is a file the owner never gets back.
    CustodyPathUnaddressable { path_hash: String },
    /// A custody row's manifest is not held here, or does not decode, so its
    /// `total_size` — the size the re-home statement signs and the minted row
    /// carries (`writer-signed-change-records.md` ruling (7)(a)(ii), *Where the
    /// statement reads its size*) — cannot be read.
    ///
    /// Unreachable for a row the record door admitted (it derived the charge
    /// from this same manifest); refusing names the row rather than minting it
    /// at a size nobody signed.
    CustodyManifestUnreadable { path_hash: String, detail: String },
    /// The folder set holds no live custody at all, so there is nothing to
    /// re-home.
    ///
    /// The folder axis's answer to the segment axis's [`Self::NoManifestMirror`]:
    /// custody rows ARE this plane's corpus (there is no mirror file), so an
    /// empty set means delivery has not happened — or every mirrored path has
    /// since been tombstoned by a detach at the source. Minting an empty folder
    /// and reporting success would be the wrong answer to both: the owner would
    /// be left deleting a folder they did not want, with nothing said about why
    /// their files are not in it.
    EmptyFolderSet { set_name: String },
    /// The display name names the nest's own reserved (`__`) rail namespace.
    ///
    /// `record_change_core` refuses this for every ordinary record door, and the
    /// reason is a silent-data-loss one: the GC classifies a reference held only
    /// by reserved sets as a **direct blob** — pinned without walking its chunks
    /// — so a chunked manifest recorded against a reserved set has its live
    /// chunks swept. This arm writes its rows through the metering core rather
    /// than through that door, so it owes the same refusal itself.
    FolderNameReserved { name: String },
    /// The re-homing was refused by the metering core — the owner's storage
    /// ceiling, in practice. Ratified as an honest ordinary outcome rather than
    /// a special case: "a corpus larger than the fresh nest's default quota
    /// surfaces the ordinary `storage_quota_exceeded`, raised in the admin app"
    /// (`backup-destinations.md` § Third destination kind → *Re-seed*). The
    /// transaction rolled back, so a raise-and-retry starts from a clean target.
    Quota(crate::db::sync_storage::StorageQuotaError),
    /// A segment-axis set this nest cannot reconstitute. The name derivation
    /// knows five kinds (`BACKUP_SET_KINDS`); every kind in the sweep that fills
    /// the corpus (`BACKED_UP_KINDS`) has a [`KindArm`], and `conv` — the one
    /// name outside it — has never been delivered. Refusing loudly beats writing
    /// a segment area for a kind whose mirror nothing would rebuild.
    ///
    /// **Adding a kind here:** its reconstitution MUST call
    /// `SegmentManager::adopt_segments` for every segment it commits, before
    /// those segments are served — the same call mail's path makes
    /// (`message-segment-store.md` § Restore). Skipping it is silent and
    /// severe: the manifest stays behind what's on disk, so the corpus's
    /// first ordinary append unlinks its own just-restored first segment as
    /// crash-recovery cleanup.
    UnsupportedKind { kind: &'static str },
    /// The owner has granted this nest no `NestBackupKey`, so there is nothing
    /// to unseal the custody with. In the ratified ceremony the grant is
    /// enrollment-time and already present; this is the honest refusal for a
    /// caller who reached the verb without it.
    NotEnrolled,
    /// **The empty-target rule.** The scope already holds live records, so this
    /// is not a fresh target: refuse, name what exists, and change nothing. There
    /// is deliberately **no force arm** — merging a backed-up corpus into a
    /// lived-in account is a named non-goal, and the alternative to refusing is a
    /// verb that could destroy data the owner cannot recreate. Not asked while
    /// the scope's in-flight marker stands.
    ScopeNotEmpty { kind: &'static str, records: i64 },
    /// **The empty-target rule's journal half, and it is freshness rather than
    /// emptiness.** The account's containers of `kind` have held content: its
    /// placement journal carries `history` placed items, removed ones, or
    /// numbers already handed out (mail's UIDs, calendar's and contacts'
    /// change numbers — [`crate::segments::AdoptableJournal::held_history`]).
    ///
    /// It can be raised on a target that holds **no** live record and **no**
    /// placement row — an account that received mail and deleted all of it.
    /// That account is empty and still not fresh: it carries tombstones and
    /// spent UIDs, and folding a backup over them would hand a mail client a
    /// vanished notice for a UID the restore had just made live.
    ///
    /// Same wire code as [`Self::ScopeNotEmpty`], because the next step is the
    /// same one: this is a lived-in account, point at another nest. What is
    /// NOT refused is a target holding container scaffolding and nothing else
    /// (the standard mailboxes, a lazily provisioned Personal calendar) — the
    /// corpus supersedes that. Not asked of mail while the scope's in-flight
    /// marker stands; calendar and contacts are still asked.
    PlacementNotFresh { kind: &'static str, history: u64 },
    /// The set holds no `manifest.<kind>` mirror. Delivery writes the mirror
    /// **last**, so its absence is the signature of a torn or never-finished
    /// delivery, not of a corrupt one — the fix is to finish delivering.
    NoManifestMirror { set_name: String, path: String },
    /// A segment the mirror lists has no custody row for one of its two halves.
    ///
    /// Overwhelmingly this is the `.meta`: a corpus whose sidecars have not all arrived
    /// carries `seg-N.dat` alone, and they arrive on the
    /// pusher's next pass through the backfill class. That destination is
    /// **not-yet-caught-up**, not broken — and reconstituting the `.dat` alone
    /// would produce a segment `FramedSegment::open` refuses, since the record
    /// footers live nowhere but the sidecar. So: refuse loudly, name the segment.
    IncompleteSegment {
        segment_id: u32,
        missing_path: String,
    },
    /// **A file already sits where one of the pair's halves would land.**
    ///
    /// Distinct from [`Self::ScopeNotEmpty`] deliberately, because the two have
    /// opposite next steps: that one reads the SQLite mirror and means *"you are
    /// pointing at a live account"* — do not retry; this one reads the disk and
    /// means *"something is in the way"* — reclaim it, then retry.
    ///
    /// It exists because DB rows are not disk files, and the mirror cannot see
    /// two cases that matter. A scope whose records are all tombstoned counts
    /// zero live while its `seg-*.dat` files remain (purge is SQL-only; the
    /// files die later, in the store's own GC). And the mirror check runs a
    /// whole corpus fetch before the write with no per-scope lock held, so mail
    /// delivered in that window creates a segment at the same base id a
    /// started-empty corpus's own ids use.
    ///
    /// Writing through either would destroy records the owner actually had and
    /// leave the mirror pointing at bytes that are gone — the rebuild only
    /// inserts. Refusing matches every other writer of this directory:
    /// `FramedSegment::create` uses `create_new`, and the store *removes* an
    /// orphan rather than writing through it.
    SegmentPathOccupied { segment_id: u32, path: String },

    /// A byte-level integrity failure: an unseal that would not open, a half that
    /// does not hash to what the source's own listing advertised, or a
    /// reconstituted pair `FramedSegment::open` will not read. Nothing has been
    /// written into the scope's live state when this is raised.
    Integrity(anyhow::Error),
}

impl std::fmt::Display for MaterializeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotASegmentSet { set_name } => write!(
                f,
                "`{set_name}` is not a reserved backup set name on either axis"
            ),
            Self::FolderNameRequired { set_name } => write!(
                f,
                "materializing the covered-folder set `{set_name}` needs \
                 folder_name_hash or folder_display_name: custody holds the folder's \
                 sealed paths but never its label, so the target's address comes from \
                 the custodian driving the ceremony"
            ),
            Self::FolderNameHashMismatch { folder } => write!(
                f,
                "folder_name_hash is not the 32-byte set name hash of the folder `{folder}` \
                 the request names"
            ),
            Self::FolderNotEmpty { folder, records } => write!(
                f,
                "the folder `{folder}` already holds {records} live record(s) this ceremony \
                 did not write; materialize seeds a fresh folder only and never merges or \
                 deletes"
            ),
            Self::FolderNotFresh { folder, property } => write!(
                f,
                "the folder `{folder}` holds no records but is not a fresh target: it \
                 carries `{property}`, so re-homing into it would expose the restored \
                 corpus to that property's audience; materialize seeds a fresh folder \
                 only — name an unshared folder, or clear `{property}` first"
            ),
            Self::FolderRehomeUnsigned { set_name } => write!(
                f,
                "materializing the covered-folder set `{set_name}` needs the owner's \
                 signature over every re-homed row: an unsigned change record is not a \
                 record"
            ),
            Self::FolderTargetMissing { folder } => write!(
                f,
                "the owner lists no folder `{folder}` to re-home into: the ceremony \
                 creates the target set first, then materializes"
            ),
            Self::RehomeSignature { folder, refusal } => write!(
                f,
                "a re-home signature for the folder `{folder}` was refused: {refusal:?}"
            ),
            Self::RehomePageMalformed { detail } => {
                write!(f, "malformed re-home page: {detail}")
            }
            Self::CustodyPathUnsealed { path_hash } => write!(
                f,
                "the custody row at {path_hash} carries no sealed name, and a live folder \
                 record cannot be minted without one; run another pull pass on the \
                 custodian and re-deliver — it moves no bytes"
            ),
            Self::CustodyPathUnaddressable { path_hash } => write!(
                f,
                "the custody row at {path_hash} carries no recorded path, so the source \
                 path hash it should be re-homed to cannot be recovered"
            ),
            Self::CustodyManifestUnreadable { path_hash, detail } => write!(
                f,
                "the custody row at {path_hash} names a manifest this nest cannot read \
                 ({detail}), so the size its re-home statement signs is unknown; re-deliver \
                 the folder's bytes"
            ),
            Self::EmptyFolderSet { set_name } => write!(
                f,
                "the covered-folder set `{set_name}` holds no live custody, so there is \
                 nothing to materialize; either it has not been delivered yet, or every \
                 mirrored path in it was tombstoned at the source"
            ),
            Self::FolderNameReserved { name } => write!(
                f,
                "`{name}` is in the nest's reserved \"__\" rail namespace and cannot be \
                 the display name of a materialized folder"
            ),
            Self::Quota(e) => write!(f, "the re-homed rows were refused: {e}"),
            Self::UnsupportedKind { kind } => write!(
                f,
                "this nest cannot yet reconstitute the `{kind}` segment kind from custody"
            ),
            Self::NotEnrolled => write!(
                f,
                "no NestBackupKey is granted for this actor, so its custody cannot be opened"
            ),
            Self::ScopeNotEmpty { kind, records } => write!(
                f,
                "this scope already holds {records} live `{kind}` record(s); \
                 materialize seeds a fresh scope only and never merges or deletes"
            ),
            Self::PlacementNotFresh { kind, history } => {
                let (containers, content) = match *kind {
                    "calendar" => ("calendars", "events"),
                    "card" => ("addressbooks", "contacts"),
                    _ => ("mailboxes", "mail"),
                };
                write!(
                    f,
                    "this account's {containers} have already held {content} ({history} \
                     placed, removed or numbered item(s) in its own history); materialize \
                     seeds a fresh account only and never merges a backup into one that \
                     has been lived in"
                )
            }
            Self::SegmentPathOccupied { segment_id, path } => write!(
                f,
                "segment {segment_id}'s path ({path}) holds bytes this ceremony cannot prove \
                 it wrote; residue of its own interrupted run is reclaimed automatically and \
                 a run interrupted after adopting resumes, so this is neither — and \
                 materialize never writes through what it did not put there. Re-deliver the \
                 set from the custodian, then materialize again"
            ),
            Self::NoManifestMirror { set_name, path } => write!(
                f,
                "custody set `{set_name}` holds no `{path}` mirror; \
                 delivery writes the mirror last, so it has not finished"
            ),
            Self::IncompleteSegment {
                segment_id,
                missing_path,
            } => write!(
                f,
                "segment {segment_id} is missing custody for `{missing_path}`; \
                 this destination has not caught up — a segment is both files"
            ),

            Self::Integrity(e) => write!(f, "custody failed verification: {e:#}"),
        }
    }
}

/// What one successful materialize did — the reply's payload, and the shape the
/// conformance tests assert against.
#[derive(Debug, Default)]
pub(crate) struct MaterializeOutcome {
    /// The segment ids written into the scope's segment area, ascending.
    pub segments: Vec<u32>,
    /// Mirror rows re-asserted from the sidecars' footers.
    pub records: u64,
    /// Placement rows replayed from the kind's placement journal; `Some(0)`
    /// when it files nothing. `None` for a kind with no placement layer
    /// (posts) and on the covered-folder axis.
    pub placements: Option<u64>,
    /// The covered-folder axis's paging: custody rows still not live in the
    /// target after this page. `None` on the segment axis.
    pub remaining: Option<u64>,
}

/// One segment pair, opened and verified: `(segment_id, dat, meta)`.
pub(super) type VerifiedPair = (u32, Vec<u8>, Vec<u8>);

/// The custody one set holds, and the means to open it.
struct CustodyReader<'a> {
    fetcher: &'a LocalStoreFetcher,
    keys: &'a FileDownloadKeys,
    by_path: &'a BTreeMap<String, ContentHash>,
}

impl CustodyReader<'_> {
    /// Open one custody path, or `None` if the set holds no live custody there.
    async fn open(&self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match self.by_path.get(path).copied() {
            None => Ok(None),
            Some(h) => download_file_bytes_by_manifest(self.fetcher, self.keys, h, None, path)
                .await
                .map(Some)
                .with_context(|| format!("open custody path `{path}`")),
        }
    }

    /// Does the set hold any live custody under this family's paths?
    fn holds_any(&self, family: SegmentFamily, scope_hex: &str, kind: &str) -> bool {
        let mirror = family.mirror_path(scope_hex, kind);
        self.by_path
            .keys()
            .any(|path| *path == mirror || family.parse(scope_hex, path).is_some())
    }

    /// Open one family's mirror and **every** pair it names, each verified
    /// against that mirror, all in memory. Nothing is written.
    ///
    /// One body for both families, so the journal is anchored exactly as the
    /// content is: the same missing-half refusal, the same unnamed-sidecar
    /// refusal, the same two hashes.
    ///
    /// Returns the pairs with the mirror's generation
    /// (`LiveManifestMirror::next_segment_id_seen`, the dead source's saved
    /// counter), which the content adoption raises the target's counter to.
    async fn verified_pairs(
        &self,
        family: SegmentFamily,
        scope_hex: &str,
        kind: &str,
        set_name: &str,
    ) -> Result<(Vec<VerifiedPair>, u32), MaterializeRefusal> {
        let mirror_path = family.mirror_path(scope_hex, kind);
        let mirror_bytes = self
            .open(&mirror_path)
            .await
            .map_err(MaterializeRefusal::Integrity)?
            .ok_or_else(|| MaterializeRefusal::NoManifestMirror {
                set_name: set_name.to_string(),
                path: mirror_path,
            })?;
        let mirror = LiveManifestMirror::from_bytes(&mirror_bytes)
            .map_err(|e| MaterializeRefusal::Integrity(e.into()))?;

        let mut pairs: Vec<VerifiedPair> = Vec::with_capacity(mirror.live.len());
        for seg in &mirror.live {
            let dat_path = family.dat_path(scope_hex, seg.segment_id);
            let meta_path = family.meta_path(scope_hex, seg.segment_id);
            let dat = self
                .open(&dat_path)
                .await
                .map_err(MaterializeRefusal::Integrity)?
                .ok_or_else(|| MaterializeRefusal::IncompleteSegment {
                    segment_id: seg.segment_id,
                    missing_path: dat_path,
                })?;
            let meta = self
                .open(&meta_path)
                .await
                .map_err(MaterializeRefusal::Integrity)?
                .ok_or_else(|| MaterializeRefusal::IncompleteSegment {
                    segment_id: seg.segment_id,
                    missing_path: meta_path,
                })?;
            verify_pair(&dat, &meta, seg).map_err(MaterializeRefusal::Integrity)?;
            pairs.push((seg.segment_id, dat, meta));
        }
        pairs.sort_by_key(|(id, _, _)| *id);
        Ok((pairs, mirror.next_segment_id_seen))
    }
}

/// A [`BlobFetcher`] over **this nest's own blob store**.
///
/// Materialize reads bytes the nest already holds, so it goes straight to the
/// store rather than round-tripping its own HTTP routes: same blobs, same
/// at-rest envelope decode (`backup::decode_blob`), no loopback socket and no
/// self-request that a bind-address or reverse-proxy change could break. The
/// confidentiality boundary is unchanged either way — the open byte routes carry
/// no bearer precisely because the seal, not the transport, is what protects
/// these bytes.
pub(super) struct LocalStoreFetcher {
    pub(super) store: Arc<dyn crate::blob_store::BlobStoreBackend>,
    pub(super) at_rest_key: Option<fauna_core::crypto::BackupKey>,
}

impl LocalStoreFetcher {
    async fn read(&self, hash: &ContentHash, what: &str) -> anyhow::Result<Vec<u8>> {
        let raw = self
            .store
            .get(hash)
            .await
            .with_context(|| format!("read {what} {} from the blob store", hash))?
            .with_context(|| {
                format!(
                    "{what} {} is in custody but absent from the blob store",
                    hash
                )
            })?;
        crate::backup::decode_blob(&raw, self.at_rest_key.as_ref())
            .with_context(|| format!("decode the at-rest envelope of {what} {}", hash))
    }
}

#[async_trait::async_trait]
impl BlobFetcher for LocalStoreFetcher {
    async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
        self.read(hash, "manifest").await
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut out = Vec::with_capacity(store_keys.len());
        for key in store_keys {
            out.push(
                self.read(key, "chunk")
                    .await
                    .with_context(|| format!("for {relative_path}"))?,
            );
        }
        Ok(out)
    }
}

/// Materialize one **segment-axis** custody set for `actor`.
///
/// The order is the load-bearing part, and it is the mirror image of delivery's
/// (bytes before custody, mirror last):
///
/// 1. every refusal that can be decided without reading a byte — set shape,
///    kind, grant, and the empty-target rule, which is not asked while the
///    kind's in-flight marker stands (the gesture already answered it);
/// 2. open the `manifest.<kind>` mirror, which names the segments to restore;
/// 3. open **and verify** every segment pair, entirely in memory, before
///    anything touches the scope's segment area — so an integrity failure on
///    the last segment leaves the target exactly as untouched as one on the
///    first. **Both families**: the placement journal's pairs are verified
///    here too, staged, and the journal's own freshness judged, so a journal
///    that fails any of it leaves the content unwritten as well;
/// 4. floor the content counter, re-assert the empty-target rule, record the
///    in-flight marker, and (a journaled mail copy) write the corpus's mailbox
///    tree — the containers before the halves;
/// 5. write the content pairs, `FramedSegment::open` each one as its own
///    admission check, adopt the journal superseding whatever it held, and
///    re-file mail that arrived since the gesture;
/// 6. rebuild the mirror rows and re-state the placement rows in **one**
///    SQLite transaction, which also retires the marker.
pub(crate) async fn materialize_segment_set(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    set_name: &str,
) -> Result<MaterializeOutcome, MaterializeRefusal> {
    let (kind, _named_scope) = parse_reserved_backup_set_name(set_name).ok_or_else(|| {
        MaterializeRefusal::NotASegmentSet {
            set_name: set_name.to_string(),
        }
    })?;
    // Every kind in the sweep that fills a backup corpus (`BACKED_UP_KINDS`)
    // has an arm. `__conv/<hex>` parses — it is legitimately reserved-shaped —
    // but nothing has ever delivered one, so it is refused.
    let arm = KindArm::of(kind).ok_or(MaterializeRefusal::UnsupportedKind { kind })?;
    let segments = arm.segments(state);
    // The scope of every set an arm serves is the owner, and the owner is the
    // authenticated connection's actor — never a wire parameter. (`__conv` is
    // the one kind whose name carries a scope; it is not reachable here.)
    let scope = *actor;

    // ── The in-flight marker: is a flip of this kind already under way? ──
    //
    // A standing marker means the empty-target rule was judged at the gesture
    // and an earlier run was interrupted after its first observable write. The
    // verb is then in MARKER MODE: it asks no freshness question, and what is
    // live outside the marker's ids is post-gesture activity it keeps
    // (`backup-destinations.md` § Third destination kind → *Re-seed* → *the
    // in-flight marker*).
    let in_flight = state
        .db
        .materialize_in_flight_get(&scope, kind)
        .await
        .map_err(MaterializeRefusal::Integrity)?;
    if let Some(marker) = &in_flight
        && marker.set_name != set_name
    {
        // Unreachable: a kind has exactly one reserved set.
        return Err(MaterializeRefusal::Integrity(anyhow::anyhow!(
            "the in-flight `{kind}` flip is of `{}`, not `{set_name}`",
            marker.set_name
        )));
    }
    let mid_flip = in_flight.is_some();

    // ── The empty-target rule, before anything is read ───────────────────
    if !mid_flip {
        let live = state
            .db
            .segment_records_count_live(&scope, kind)
            .await
            .map_err(MaterializeRefusal::Integrity)?;
        if live > 0 {
            return Err(MaterializeRefusal::ScopeNotEmpty {
                kind,
                records: live,
            });
        }
        // The journal half's cheap form: a placement row is a filed message.
        // The full form (history the rows no longer show) needs the journal's
        // fold and is judged below, once the corpus has been read.
        let filed = count_placed_rows(state, &scope, arm)
            .await
            .map_err(MaterializeRefusal::Integrity)?;
        if filed > 0 {
            return Err(MaterializeRefusal::PlacementNotFresh {
                kind,
                history: filed,
            });
        }
    }

    // ── The key the owner granted at enrollment ──────────────────────────
    let granted = state
        .db
        .get_nest_backup_key(actor.as_slice())
        .await
        .map_err(MaterializeRefusal::Integrity)?
        .ok_or(MaterializeRefusal::NotEnrolled)?;
    let granted: [u8; NEST_BACKUP_KEY_LEN] = granted.as_slice().try_into().map_err(|_| {
        MaterializeRefusal::Integrity(anyhow::anyhow!(
            "the granted NestBackupKey is {} bytes, not {NEST_BACKUP_KEY_LEN}",
            granted.len()
        ))
    })?;
    let keys =
        FileDownloadKeys::owner(OwnerSealKey::SourceNest(NestBackupKey::from_bytes(granted)));

    let backup_svc = state.backup_service.as_ref().ok_or_else(|| {
        MaterializeRefusal::Integrity(anyhow::anyhow!("this nest has no blob store configured"))
    })?;
    let fetcher = LocalStoreFetcher {
        store: backup_svc.local_blob_store(),
        at_rest_key: backup_svc.encryption_key().cloned(),
    };

    // ── The custody the set holds, keyed by the path it was recorded under ──
    let custody = state
        .db
        .list_backup_custody_in_set(&scope, set_name)
        .await
        .map_err(MaterializeRefusal::Integrity)?;
    let by_path: BTreeMap<String, ContentHash> = custody
        .into_iter()
        .filter_map(|row| {
            let path = row.path?;
            let hash: [u8; 32] = row.manifest_hash.as_slice().try_into().ok()?;
            Some((path, ContentHash::from_digest_raw(hash)))
        })
        .collect();

    let scope_hex = hex::encode(scope);
    let custody = CustodyReader {
        fetcher: &fetcher,
        keys: &keys,
        by_path: &by_path,
    };

    // ── Open and verify EVERY pair, of BOTH families, before writing ANY ──
    //
    // The content's mirror names what to restore, and its absence is a torn
    // delivery (`NoManifestMirror`). The journal's is read the same way, with
    // one difference in what absence means.
    let (pairs, ledger_generation) = custody
        .verified_pairs(SegmentFamily::Content, &scope_hex, kind, set_name)
        .await?;

    // A copy that holds content and no journal custody — or journal custody
    // but no journal mirror — is a delivery still under way: delivery writes
    // each family's mirror last, and restoring the content now would strand
    // the journal that is still on its way, since once the account is live
    // nothing may ever be adopted over it. For calendar and contacts it would
    // be worse still: an event row is built FROM the journal, so the content
    // would be live with no row behind it, which the compaction worker's
    // orphan reaper tombstones.
    let journal =
        if arm.has_journal() && custody.holds_any(SegmentFamily::Placement, &scope_hex, kind) {
            // The journal's generation needs no floor: a journal never retires
            // a segment without minting a higher one, so its rebuilt counter
            // (the greatest adopted id plus one) already equals the ledger's.
            let (journal_pairs, _journal_generation) = custody
                .verified_pairs(SegmentFamily::Placement, &scope_hex, kind, set_name)
                .await?;
            // Staged and judged: the empty-target rule's journal half, in full,
            // as a pre-flight — a lived-in account is refused HERE, with nothing
            // written in either family. Mid-flip, freshness was settled at the
            // gesture and is not asked again, for any journaled kind.
            let judge = !mid_flip;
            Some(StagedAny::stage(arm, state, &scope, &journal_pairs, kind, judge).await?)
        } else if arm.has_journal() && !pairs.is_empty() {
            return Err(MaterializeRefusal::NoManifestMirror {
                set_name: set_name.to_string(),
                path: SegmentFamily::Placement.mirror_path(&scope_hex, kind),
            });
        } else {
            None
        };

    // ── Write the segment area ───────────────────────────────────────────
    //
    // Nothing here overwrites, and that is now a property of the writes rather
    // than an inference about caller state drawn hundreds of lines earlier. The
    // old claim — the empty-target rule established the scope holds no live
    // records, and each pair goes under its own segment id — was an inference
    // about the SQLite mirror, and DB rows are not disk files (see
    // [`MaterializeRefusal::SegmentPathOccupied`] for the two ways they
    // diverge). Three guards, outermost first, with the counter floor between
    // the first two:
    //
    //   1. `finalize_open`, so an open writable segment for this scope (none on
    //      a fresh target, but a crashed earlier attempt could leave one) is
    //      sealed rather than raced.
    //   2. The empty-target rule re-asserted here, at the write, not only at the
    //      top of the verb. This is what closes the *window*: mail that arrived
    //      during the corpus fetch is visible now, including the case where its
    //      segment ids do NOT collide with ours and guard 3 would therefore let
    //      a backup corpus merge into a scope that has since become lived-in.
    //      Under a standing in-flight marker the rule was judged at the
    //      gesture, so what is live is post-gesture activity and is kept.
    //   3. Every destination path checked free before ANY half is written, and
    //      each half then written create-new. The pre-flight is what keeps
    //      "verify everything before writing anything" true of this block too:
    //      refusing halfway would leave orphan halves that the next attempt then
    //      refuses on. `create_new` is the backstop for the residual race
    //      between the pre-flight and the write, and it is the discipline the
    //      rest of this directory already keeps.
    segments
        .finalize_open(&scope)
        .await
        .context("finalize_open before materialize")
        .map_err(MaterializeRefusal::Integrity)?;
    // ── Partition the id space: floor the content counter ────────────────
    //
    // Before guard 2, so every arrival guard 2 lets through opens its segment
    // at or above the corpus's ledger generation. The store only ever unlinks
    // at the id it is about to open, and it now opens above every corpus id,
    // so an arrival's segment can never take, or reclaim, an id the corpus
    // needs — mid-flip or not. Raising a counter writes nothing anything reads
    // as content, so a refusal below leaves only spent ids behind.
    let ids: Vec<u32> = pairs.iter().map(|(id, _, _)| *id).collect();
    segments
        .floor_counter(&scope, ledger_generation)
        .await
        .context("floor the content counter to the corpus's ledger generation")
        .map_err(MaterializeRefusal::Integrity)?;
    if mid_flip {
        // Everything live is post-gesture activity, except a row in one of the
        // corpus's own segments: the transaction that writes those deletes the
        // marker, so one standing beside them is not a state this verb leaves.
        let corpus_rows = count_live_rows_in(state, &scope, kind, &ids)
            .await
            .map_err(MaterializeRefusal::Integrity)?;
        if corpus_rows > 0 {
            return Err(MaterializeRefusal::Integrity(anyhow::anyhow!(
                "{corpus_rows} live `{kind}` row(s) sit in the corpus's own segments while \
                 its in-flight marker still stands"
            )));
        }
    } else {
        let live_now = state
            .db
            .segment_records_count_live(&scope, kind)
            .await
            .context("re-read the live record count at the write")
            .map_err(MaterializeRefusal::Integrity)?;
        if live_now > 0 {
            return Err(MaterializeRefusal::ScopeNotEmpty {
                kind,
                records: live_now,
            });
        }
    }

    // ── The gesture, recorded: the in-flight marker ──────────────────────
    //
    // Before the first write anything else can observe. From here until the
    // commit deletes it, the target is mid-flip: an interruption anywhere below
    // — the dispatcher's deadline abort, a nest crash — is completed by the
    // owner's retry, whatever arrived in between. A retry re-puts the marker
    // with its own run's ids and keeps the gesture's time.
    state
        .db
        .materialize_in_flight_put(
            &scope,
            kind,
            &crate::db::materialize_in_flight::MaterializeInFlight {
                set_name: set_name.to_string(),
                segment_ids: ids.clone(),
                journal: if journal.is_some() {
                    crate::db::materialize_in_flight::InFlightJournal::Adopt
                } else {
                    crate::db::materialize_in_flight::InFlightJournal::None
                },
                started_at: in_flight
                    .as_ref()
                    .map_or_else(now_epoch_secs, |m| m.started_at),
            },
        )
        .await
        .context("record the in-flight marker")
        .map_err(MaterializeRefusal::Integrity)?;

    // ── The containers before the halves (any journaled copy) ────────────
    //
    // Right after the marker, so every arrival from here on mints its number
    // from the container it will end in — a UID above the corpus's, under the
    // corpus's UIDVALIDITY; a change number above the corpus calendar's or
    // address book's — and the re-filing keeps the number it was served under.
    // Every door allocates from these SQLite rows (a calendar or card PUT is
    // refused without one), so the rows are what the rule writes.
    match &journal {
        Some(StagedAny::Mail(staged)) => {
            write_corpus_mailboxes(state, &scope, &staged.fold().mailboxes)
                .await
                .map_err(MaterializeRefusal::Integrity)?;
        }
        Some(dav @ (StagedAny::Calendar(_) | StagedAny::Card(_))) => {
            write_corpus_collections(state, &scope, arm, &dav.dav_fold())
                .await
                .map_err(MaterializeRefusal::Integrity)?;
        }
        None => {}
    }

    let root = segments.scope_dir(&scope);
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create the segment area at {}", root.display()))
        .map_err(MaterializeRefusal::Integrity)?;
    //
    // Guard 3 CLASSIFIES an occupant rather than refusing every one of them.
    // `create_new` on its own traded a rare silent loss for a permanent hard
    // failure: a crash between a pair's two `write_half` calls leaves a durable
    // half that every later materialize refuses forever, and nothing in the
    // tree removes it. The store's own reclaim does not reach it, twice over —
    // it is keyed on the `.dat`, so a lone `.meta` (written and fsynced first,
    // hence the likeliest residue) is invisible to it; and it fires only inside
    // an APPEND's rotation, which would make the scope non-empty and trip guard
    // 2 instead. There is no operator to clear it by hand (`principles.md` §
    // One configuration surface), so an unreclaimed orphan is a
    // client-reachable, client-UNRECOVERABLE state, which `nest/common.md` §
    // Client-state recoverability calls a bug outright.
    //
    // The discriminator is deliberately NARROWER than "this scope holds no live
    // records, so anything here is fair game". That is true today and still too
    // weak to delete by: it licenses removing bytes whose provenance we never
    // established. What we CAN prove is whether a file is this run's own
    // interrupted write — a crash leaves a PREFIX of the half we were writing
    // (the empty file being the limiting case, and the exact live-nest shape),
    // and foreign content is not a prefix of ours. So:
    //
    //   * every occupant a prefix of what we mean to write, and the pair is
    //     already ADOPTED and complete → residue of a crashed run of this
    //     ceremony that got as far as adopting but not as far as committing the
    //     mirror. RESUME: an adopted segment is committed and is never deleted,
    //     so keep the bytes, skip the rewrite, and let the rebuild below finish
    //     the job the crash interrupted.
    //   * every occupant a prefix, not adopted → an uncommitted half-state of
    //     ours. Reclaim both halves and write the pair fresh.
    //   * anything else → refuse, exactly as before. Bytes we cannot prove are
    //     ours are not this ceremony's to clear.
    //
    // ⚠ **"foreign content is not a prefix of ours" is a FAUNA-CARV2 property,
    // not a materialize one — name the coupling**. It holds because `fauna_carv2::Writer::new` zero-fills the
    // 40-byte v2 header placeholder and `finalize()` backfills byte 11 to
    // `0x80`: a genuinely in-progress segment's header byte 11 is `0x00`, a
    // finalized one's is `0x80`, so an open segment for this id can never
    // satisfy the prefix test against the FINALIZED bytes we mean to write —
    // pinned at the realistic occupant shape by
    // `materialize_refuses_a_genuinely_open_segment_for_the_same_id`
    // (`tests/nest_backup_coordinator.rs`). If `fauna-carv2` ever writes a
    // self-describing or already-final header at `Writer::new`, that pin
    // catches the widening; every OTHER pin in this file's test suite stages
    // an occupant whose classification does not depend on this byte (garbage
    // content, or an already-full/adopted pair) and would stay green through
    // exactly that regression. **The `.meta` half decides nothing on its
    // own** — the sidecar is canonical dag-cbor, so its non-prefix-ness
    // against a foreign sidecar for the same id rests only on
    // `created_at_secs`/`record_order` bytes differing, a content accident,
    // not a structural property. Every real pair is decided by the `.dat`
    // half alone (a Foreign `.dat` refuses the whole pair via the `any`
    // check below), so the classification's soundness is entirely the
    // carv2 header's, not this function's and not the sidecar's.
    //
    // (Historical: a delivery's still-empty `create_new` landing between guard
    // 2 and this classification could once be reclaimed as our residue — a
    // race this verb had declined. The counter floor above closed it: every
    // segment an arrival opens is at or above the ledger generation, never at
    // a corpus id this loop classifies.)
    let manifest = segments
        .load_manifest(&scope)
        .await
        .context("read the kind manifest to classify the occupied segment paths")
        .map_err(MaterializeRefusal::Integrity)?;
    let km = &manifest.kind_manifest;
    let mut resume: Vec<u32> = Vec::new();
    for (segment_id, dat, meta) in &pairs {
        let dat_path = root.join(format!("seg-{segment_id:08}.dat"));
        let meta_path = root.join(format!("seg-{segment_id:08}.meta"));
        let halves = [(&meta_path, meta.as_slice()), (&dat_path, dat.as_slice())];
        let occupancy: Vec<Occupant> = halves
            .iter()
            .map(|(path, intended)| classify_occupant(path, intended))
            .collect();
        if occupancy.iter().all(|o| matches!(o, Occupant::Absent)) {
            continue;
        }
        if occupancy.iter().any(|o| matches!(o, Occupant::Foreign)) {
            let (path, _) = halves
                .iter()
                .zip(&occupancy)
                .find(|(_, o)| matches!(o, Occupant::Foreign))
                .map(|(h, _)| *h)
                .expect("just matched a Foreign occupant");
            return Err(MaterializeRefusal::SegmentPathOccupied {
                segment_id: *segment_id,
                path: path.display().to_string(),
            });
        }
        let adopted = *segment_id < km.next_seg_id && km.live_segments.contains(segment_id);
        let complete = occupancy
            .iter()
            .all(|o| matches!(o, Occupant::OurWrite { full: true }));
        if adopted && complete {
            resume.push(*segment_id);
            continue;
        }
        for (path, _) in halves {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(MaterializeRefusal::Integrity(anyhow::anyhow!(
                        "reclaim the uncommitted half-state at {}: {e}",
                        path.display()
                    )));
                }
            }
        }
    }
    for (segment_id, dat, meta) in &pairs {
        let dat_path = root.join(format!("seg-{segment_id:08}.dat"));
        let meta_path = root.join(format!("seg-{segment_id:08}.meta"));
        // A resumed pair is already on disk and already proven byte-identical to
        // what this run would write; rewriting it would only re-earn the
        // `create_new` refusal it is recovering from.
        if !resume.contains(segment_id) {
            write_half(*segment_id, &meta_path, meta)?;
            write_half(*segment_id, &dat_path, dat)?;
        }
        // The pair's own admission check: a segment that will not reopen is not
        // a segment, whatever its two halves hashed to in transit.
        fauna_segment_store::FramedSegment::open(&dat_path)
            .map_err(|e| {
                MaterializeRefusal::Integrity(anyhow::anyhow!(
                    "the reconstituted segment {segment_id} does not reopen: {e}"
                ))
            })
            .map(|_| ())?;
    }

    // ── Adopt the ids into the kind manifest ─────────────────────────────
    //
    // The halves are on disk, but the store's own counter has not moved. A
    // target that has never appended still reads `next_seg_id: 1`, and the
    // store reclaims whatever sits at the id it is about to open — a reclaim
    // that is right, and that every other writer of this area earns by
    // advancing the counter as it writes. Without this call the first ordinary
    // mail to arrive rotates onto `seg-00000001`, finds the corpus's first
    // segment and unlinks both halves: silently, because from the store's point
    // of view it is doing the crash recovery it was built to do. That would
    // break the ceremony's own ratified property ("deletes nothing anywhere")
    // one delivery after the flip, on the very first mail the re-seeded nest
    // receives.
    //
    // Before the mirror rebuild, not after: a crash between the two then leaves
    // the segments protected but unserved — recoverable by a re-run — rather
    // than served by mirror rows pointing at bytes the next append will delete.
    //
    // The counter lands at the LEDGER's generation, not merely past the
    // adopted ids: the dead source's counter can sit above its highest live id
    // (an empty-output compaction retires without minting), and a re-seeded
    // nest that restarted below it would mint retired ids again and hand every
    // device that verified the old ledger a rollback to explain.
    segments
        .adopt_segments(&scope, &ids, ledger_generation)
        .await
        .context("adopt the materialized segment ids into the kind manifest")
        .map_err(MaterializeRefusal::Integrity)?;

    // ── Adopt the journal ────────────────────────────────────────────────
    //
    // After the content, before the transaction: the journal is the durable
    // truth the placement rows are replayed FROM, so it is in place before any
    // row exists (append before commit —
    // `message-segment-store.md` § Invariants, invariant 1). A crash here
    // leaves both families on disk and the corpus unserved, which the retry
    // completes under the marker: the content by its adopted-and-complete
    // classification, the journal by keeping an adoption it already made (or
    // superseding one it never finished), and mail that arrived meanwhile by
    // the re-filing, from rows only the commit removes.
    let journal = match journal {
        None => None,
        Some(staged) => Some(staged.adopt(state, &scope, &ids).await?),
    };

    // A post corpus's records are read back through the store before the
    // transaction opens (the read is async; the write below is not).
    let post_records = if arm == KindArm::Post {
        crate::segments::post::read_restored_records(segments, &scope, &ids)
            .await
            .context("read the materialized post records")
            .map_err(MaterializeRefusal::Integrity)?
    } else {
        Vec::new()
    };

    // ── Make both families live — ONE transaction ────────────────────────
    //
    // The mirror rebuild and the placement rows commit together or not at all,
    // so the state this whole arm exists to end — mail that is live and in no
    // mailbox — is not a state a crash can leave behind. The marker leaves in
    // the same transaction: the flip is complete exactly when it commits.
    let (records, placements) = {
        let conn = state.db.conn().await;
        // The journal's fold is read again under the connection, so an
        // arrival whose door finished before this point is in it (its row and
        // its journal record both written); one whose door is between those
        // two writes keeps its row through the re-statement
        // ([`restate_mail_rows`], [`restate_dav_rows`]).
        let journal = match journal {
            Some(fold) => Some(current_fold(state, &scope, fold.arm()).await?),
            None => None,
        };
        let tx = conn
            .unchecked_transaction()
            .context("begin the materialize transaction")
            .map_err(MaterializeRefusal::Integrity)?;
        let placements = rebuild_rows(
            &tx,
            state,
            &scope,
            arm,
            &ids,
            journal.as_ref(),
            &post_records,
        )?;
        let n: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM segment_records WHERE scope_id = ?1 AND kind = ?2",
                rusqlite::params![scope.as_slice(), kind],
                |r| r.get(0),
            )
            .context("count the rebuilt mirror rows")
            .map_err(MaterializeRefusal::Integrity)?;
        tx.execute(
            "DELETE FROM materialize_in_flight WHERE actor_id = ?1 AND kind = ?2",
            rusqlite::params![scope.as_slice(), kind],
        )
        .context("retire the in-flight marker")
        .map_err(MaterializeRefusal::Integrity)?;
        tx.commit()
            .context("commit the materialize transaction")
            .map_err(MaterializeRefusal::Integrity)?;
        (n.max(0) as u64, placements)
    };

    Ok(MaterializeOutcome {
        segments: ids,
        records,
        placements,
        remaining: None,
    })
}

/// What differs between the backed-up kinds materialize reconstitutes — and
/// nothing else does. Every step the verb takes that is not named here is one
/// code path for all four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindArm {
    Mail,
    Post,
    Calendar,
    Card,
}

impl KindArm {
    /// The arm for a parsed set kind, or `None` for one no arm serves.
    fn of(kind: &str) -> Option<Self> {
        match kind {
            "mail" => Some(Self::Mail),
            "post" => Some(Self::Post),
            "calendar" => Some(Self::Calendar),
            "card" => Some(Self::Card),
            _ => None,
        }
    }

    /// The kind's content segment store.
    fn segments(self, state: &AppState) -> &Arc<fauna_segment_store::SegmentManager> {
        match self {
            Self::Mail => &state.mail_segments,
            Self::Post => &state.post_segments,
            Self::Calendar => &state.cal_segments,
            Self::Card => &state.card_segments,
        }
    }

    /// The actor's placement tables, in the order they are cleared and
    /// refilled; the first is the one whose rows are placed items (a filed
    /// message, a calendar's event, an addressbook's card). Empty for a kind
    /// with no placement layer.
    fn placement_tables(self) -> &'static [&'static str] {
        match self {
            Self::Mail => &[
                "bridge_imap_messages",
                "bridge_imap_mailbox_state",
                "bridge_imap_expunged",
                "bridge_imap_subscriptions",
            ],
            Self::Calendar => &[
                "bridge_caldav_events",
                "bridge_caldav_calendars",
                "bridge_caldav_expunged",
            ],
            Self::Card => &[
                "bridge_carddav_cards",
                "bridge_carddav_addressbooks",
                "bridge_carddav_expunged",
            ],
            Self::Post => &[],
        }
    }

    /// Does a placement journal ride this kind's set?
    fn has_journal(self) -> bool {
        !matches!(self, Self::Post)
    }

    /// The names a DAV kind's placement tables spell its parts with; `None`
    /// for mail and posts. The calendar and contacts arms are one code path
    /// over these.
    fn dav_tables(self) -> Option<DavTables> {
        match self {
            Self::Calendar => Some(DavTables {
                kind: "calendar",
                collections: "bridge_caldav_calendars",
                collection_id: "calendar_id",
                items: "bridge_caldav_events",
                item_id: "event_id",
            }),
            Self::Card => Some(DavTables {
                kind: "card",
                collections: "bridge_carddav_addressbooks",
                collection_id: "addressbook_id",
                items: "bridge_carddav_cards",
                item_id: "card_id",
            }),
            Self::Mail | Self::Post => None,
        }
    }
}

/// A DAV kind's placement tables, by the names its columns use.
#[derive(Debug, Clone, Copy)]
struct DavTables {
    /// The `segment_records` kind its content records are mirrored under.
    kind: &'static str,
    /// The containers' table and its id column.
    collections: &'static str,
    collection_id: &'static str,
    /// The placed items' table and its content-record id column.
    items: &'static str,
    item_id: &'static str,
}

/// A corpus's journal, staged and judged fresh, for whichever kind it is.
enum StagedAny {
    Mail(crate::segments::StagedJournal<crate::segments::mail_placement::MailPlacementKind>),
    Calendar(crate::segments::StagedJournal<crate::segments::cal_placement::CalPlacementKind>),
    Card(crate::segments::StagedJournal<crate::segments::card_placement::CardPlacementKind>),
}

/// An adopted journal's fold, for whichever kind it is.
enum AdoptedFold {
    Mail(fauna_mail::segments::placement::MailPlacementManifest),
    Calendar(fauna_calendar::segments::placement::CalPlacementManifest),
    Card(fauna_contacts::segments::placement::CardPlacementManifest),
}

impl StagedAny {
    /// Stage `pairs` as the corpus's journal and judge the target's own
    /// against it — the empty-target rule's journal half, in full, as a
    /// pre-flight. The adoption asks again under the lock it writes under,
    /// and that answer is the one that binds.
    async fn stage(
        arm: KindArm,
        state: &Arc<AppState>,
        scope: &[u8; 32],
        pairs: &[VerifiedPair],
        kind: &'static str,
        judge: bool,
    ) -> Result<Self, MaterializeRefusal> {
        Ok(match arm {
            KindArm::Mail => {
                Self::Mail(stage_journal(&state.mail_placement, scope, pairs, kind, judge).await?)
            }
            KindArm::Calendar => Self::Calendar(
                stage_journal(&state.cal_placement, scope, pairs, kind, judge).await?,
            ),
            KindArm::Card => {
                Self::Card(stage_journal(&state.card_placement, scope, pairs, kind, judge).await?)
            }
            KindArm::Post => {
                return Err(MaterializeRefusal::Integrity(anyhow::anyhow!(
                    "the post kind has no placement journal to stage"
                )));
            }
        })
    }

    /// The staged corpus's fold in recover's kind-neutral DAV shape (empty for
    /// mail, which has no DAV collections).
    fn dav_fold(&self) -> DavFold {
        let mut fold = DavFold::default();
        match self {
            Self::Calendar(s) => fold.absorb_cal(s.fold()),
            Self::Card(s) => fold.absorb_card(s.fold()),
            Self::Mail(_) => {}
        }
        fold
    }

    /// Adopt the staged journal under the journal's own per-actor lock.
    ///
    /// Every kind adopts **superseding** — it always runs under the in-flight
    /// marker, which the verb writes before it gets here — and then re-files
    /// what arrived since the gesture: mail by mailbox and UID
    /// ([`refile_mail_arrivals`]), calendar and contacts by resource and change
    /// number ([`refile_dav_arrivals`]).
    async fn adopt(
        self,
        state: &Arc<AppState>,
        scope: &[u8; 32],
        corpus_ids: &[u32],
    ) -> Result<AdoptedFold, MaterializeRefusal> {
        Ok(match self {
            Self::Mail(s) => {
                state
                    .mail_placement
                    .adopt_staged_superseding(scope, s)
                    .await
                    .context("adopt the corpus's placement journal")
                    .map_err(MaterializeRefusal::Integrity)?;
                refile_mail_arrivals(state, scope, corpus_ids).await?;
                AdoptedFold::Mail(
                    state
                        .mail_placement
                        .current_manifest(scope)
                        .await
                        .context("read the adopted journal's fold")
                        .map_err(MaterializeRefusal::Integrity)?,
                )
            }
            Self::Calendar(s) => {
                state
                    .cal_placement
                    .adopt_staged_superseding(scope, s)
                    .await
                    .context("adopt the corpus's placement journal")
                    .map_err(MaterializeRefusal::Integrity)?;
                refile_dav_arrivals(state, scope, KindArm::Calendar, corpus_ids).await?;
                current_fold(state, scope, KindArm::Calendar).await?
            }
            Self::Card(s) => {
                state
                    .card_placement
                    .adopt_staged_superseding(scope, s)
                    .await
                    .context("adopt the corpus's placement journal")
                    .map_err(MaterializeRefusal::Integrity)?;
                refile_dav_arrivals(state, scope, KindArm::Card, corpus_ids).await?;
                current_fold(state, scope, KindArm::Card).await?
            }
        })
    }
}

impl AdoptedFold {
    /// The kind whose journal this is.
    fn arm(&self) -> KindArm {
        match self {
            Self::Mail(_) => KindArm::Mail,
            Self::Calendar(_) => KindArm::Calendar,
            Self::Card(_) => KindArm::Card,
        }
    }

    /// The fold in recover's kind-neutral DAV shape (empty for mail).
    fn dav_fold(&self) -> DavFold {
        let mut fold = DavFold::default();
        match self {
            Self::Calendar(m) => fold.absorb_cal(m),
            Self::Card(m) => fold.absorb_card(m),
            Self::Mail(_) => {}
        }
        fold
    }

    /// Each placed item's ETag, by content-record id (empty for mail).
    fn dav_etags(&self) -> HashMap<[u8; 32], String> {
        match self {
            Self::Calendar(m) => m
                .events
                .iter()
                .map(|e| (e.event_id, e.etag.clone()))
                .collect(),
            Self::Card(m) => m
                .cards
                .iter()
                .map(|c| (c.card_id, c.etag.clone()))
                .collect(),
            Self::Mail(_) => HashMap::new(),
        }
    }
}

/// The fold `arm`'s journal holds now.
async fn current_fold(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: KindArm,
) -> Result<AdoptedFold, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let read = "read the adopted journal's fold";
    Ok(match arm {
        KindArm::Mail => AdoptedFold::Mail(
            state
                .mail_placement
                .current_manifest(scope)
                .await
                .context(read)
                .map_err(integrity)?,
        ),
        KindArm::Calendar => AdoptedFold::Calendar(
            state
                .cal_placement
                .current_manifest(scope)
                .await
                .context(read)
                .map_err(integrity)?,
        ),
        KindArm::Card => AdoptedFold::Card(
            state
                .card_placement
                .current_manifest(scope)
                .await
                .context(read)
                .map_err(integrity)?,
        ),
        KindArm::Post => {
            return Err(integrity(anyhow::anyhow!(
                "the post kind has no placement journal"
            )));
        }
    })
}

async fn stage_journal<K: crate::segments::AdoptableJournal>(
    journal: &crate::segments::PlacementJournal<K>,
    scope: &[u8; 32],
    pairs: &[VerifiedPair],
    kind: &'static str,
    judge: bool,
) -> Result<crate::segments::StagedJournal<K>, MaterializeRefusal> {
    let staged = journal
        .stage_corpus(scope, pairs)
        .await
        .context("stage the corpus's placement journal")
        .map_err(MaterializeRefusal::Integrity)?;
    if !judge {
        return Ok(staged);
    }
    if let crate::segments::AdoptionVerdict::LivedIn { history } = journal
        .adoption_verdict(scope, &staged)
        .await
        .context("judge the target's placement journal")
        .map_err(MaterializeRefusal::Integrity)?
    {
        return Err(MaterializeRefusal::PlacementNotFresh { kind, history });
    }
    Ok(staged)
}

/// How many items the actor has placed for `arm`'s kind — the rows of its
/// first placement table. Zero for a kind with no placement layer.
async fn count_placed_rows(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    arm: KindArm,
) -> anyhow::Result<u64> {
    let Some(table) = arm.placement_tables().first() else {
        return Ok(0);
    };
    let conn = state.db.conn().await;
    let n: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE actor_id = ?1"),
            rusqlite::params![actor.as_slice()],
            |r| r.get(0),
        )
        .with_context(|| format!("count the actor's {table} rows"))?;
    Ok(n.max(0) as u64)
}

/// How many live `kind` rows the scope's mirror holds in the segments `ids`
/// name — the corpus's own, under a standing in-flight marker.
async fn count_live_rows_in(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    kind: &str,
    ids: &[u32],
) -> anyhow::Result<u64> {
    let conn = state.db.conn().await;
    let mut stmt = conn
        .prepare(
            "SELECT segment_id FROM segment_records
             WHERE scope_id = ?1 AND kind = ?2 AND tombstoned = 0",
        )
        .context("prepare the corpus-segment row count")?;
    let segment_ids = stmt
        .query_map(rusqlite::params![scope.as_slice(), kind], |r| {
            r.get::<_, i64>(0)
        })
        .context("count the rows in the corpus's segments")?
        .collect::<Result<Vec<_>, _>>()
        .context("read the live rows' segment ids")?;
    Ok(segment_ids
        .into_iter()
        .filter(|id| u32::try_from(*id).is_ok_and(|id| ids.contains(&id)))
        .count() as u64)
}

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Write the corpus's mailbox tree into the target's mailbox state — the
/// containers before the halves (`backup-destinations.md` § *the in-flight
/// marker*, rule (2)).
///
/// One small transaction, and an upsert that never lowers a counter: a retry
/// over arrivals finds the corpus's mailboxes already advanced past it. A
/// mailbox's UIDVALIDITY becomes the corpus's, so an arrival is served under
/// the validity it will keep. The prune floor is raised to the corpus's and
/// never lowered either; the commit's re-statement carries it on.
async fn write_corpus_mailboxes(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    mailboxes: &[fauna_mail::segments::placement::MailboxState],
) -> anyhow::Result<()> {
    let conn = state.db.conn().await;
    let tx = conn
        .unchecked_transaction()
        .context("begin the corpus containers write")?;
    for m in mailboxes {
        tx.execute(
            "INSERT INTO bridge_imap_mailbox_state
                (actor_id, mailbox, uid_validity, uid_next, highestmodseq, pruned_modseq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(actor_id, mailbox) DO UPDATE SET
                uid_validity  = excluded.uid_validity,
                uid_next      = MAX(uid_next, excluded.uid_next),
                highestmodseq = MAX(highestmodseq, excluded.highestmodseq),
                pruned_modseq = MAX(pruned_modseq, excluded.pruned_modseq)",
            rusqlite::params![
                scope.as_slice(),
                &m.name,
                i64::from(m.uid_validity),
                i64::from(m.uid_next),
                m.highestmodseq as i64,
                m.pruned_modseq as i64,
            ],
        )
        .with_context(|| format!("write the corpus's mailbox `{}`", m.name))?;
    }
    tx.commit().context("commit the corpus containers write")?;
    Ok(())
}

/// One live arrival's placement as its door wrote it.
struct ArrivalRow {
    mailbox: String,
    uid: u32,
    flags: String,
    message_id: Vec<u8>,
    internal_date: i64,
    uid_validity: u32,
}

/// **Re-file what arrived since the gesture into the adopted journal**
/// (`backup-destinations.md` § *the in-flight marker*, rule (3)): by mailbox
/// name, under the UID it already holds, flags and dates kept, a mailbox
/// created where the corpus lacks one.
///
/// What arrived is read from the rows the arrival doors wrote
/// (`bridge_imap_messages`, joined to a live record outside the corpus's
/// segments), not from the journal the adoption superseded. Those rows are the
/// state the owner was served, and only the commit transaction removes them —
/// so a crash anywhere between the superseding adoption and the commit leaves
/// them for the retry, where a superseded journal's would be gone. A row the
/// adopted journal already places (an arrival journaled after an earlier
/// adoption, or one an earlier run re-filed) is skipped.
///
/// The UID is kept unless the corpus's mailbox has already handed it out —
/// only an arrival that minted before the containers landed — and then the
/// mailbox's next UID is taken. The mail's tombstones, subscriptions and empty
/// mailboxes are not re-filed: consequence (2) of *Where restored mail lands*.
async fn refile_mail_arrivals(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    corpus_ids: &[u32],
) -> Result<(), MaterializeRefusal> {
    use fauna_mail::segments::placement::MailPlacementRecord;
    let integrity = MaterializeRefusal::Integrity;

    let arrivals: Vec<ArrivalRow> = {
        let conn = state.db.conn().await;
        let mut stmt = conn
            .prepare(
                "SELECT m.mailbox, m.uid, m.flags, m.message_id, m.internal_date,
                        COALESCE(s.uid_validity, 1), sr.segment_id
                 FROM bridge_imap_messages m
                 JOIN segment_records sr
                   ON sr.kind = 'mail' AND sr.scope_id = m.actor_id
                  AND substr(sr.record_cid, 5) = m.message_id AND sr.tombstoned = 0
                 LEFT JOIN bridge_imap_mailbox_state s
                   ON s.actor_id = m.actor_id AND s.mailbox = m.mailbox
                 WHERE m.actor_id = ?1
                 ORDER BY m.mailbox, m.uid",
            )
            .context("prepare the arrivals read")
            .map_err(integrity)?;
        let rows = stmt
            .query_map(rusqlite::params![scope.as_slice()], |r| {
                Ok((
                    ArrivalRow {
                        mailbox: r.get(0)?,
                        uid: r.get::<_, i64>(1)? as u32,
                        flags: r.get(2)?,
                        message_id: r.get(3)?,
                        internal_date: r.get(4)?,
                        uid_validity: r.get::<_, i64>(5)? as u32,
                    },
                    r.get::<_, i64>(6)?,
                ))
            })
            .context("read the arrivals")
            .map_err(integrity)?
            .collect::<Result<Vec<_>, _>>()
            .context("collect the arrivals")
            .map_err(integrity)?;
        let mut seen = std::collections::HashSet::new();
        rows.into_iter()
            .filter(|(_, seg)| u32::try_from(*seg).is_ok_and(|s| !corpus_ids.contains(&s)))
            .map(|(row, _)| row)
            .filter(|row| seen.insert((row.mailbox.clone(), row.uid)))
            .collect()
    };

    for arrival in arrivals {
        let fold = state
            .mail_placement
            .current_manifest(scope)
            .await
            .context("read the adopted journal's fold")
            .map_err(integrity)?;
        if fold
            .placements
            .iter()
            .any(|p| p.content_record_id == arrival.message_id)
        {
            continue;
        }
        let mailbox = match fold.mailboxes.iter().find(|m| m.name == arrival.mailbox) {
            Some(m) => (m.uid_next, m.highestmodseq),
            None => {
                state
                    .mail_placement
                    .append_event(
                        scope,
                        &MailPlacementRecord::Create {
                            mailbox: arrival.mailbox.clone(),
                            uid_validity: arrival.uid_validity,
                            attrs: crate::db::bridge_imap::standard_mailbox_attrs(&arrival.mailbox),
                        },
                    )
                    .await
                    .context("re-create an arrival's mailbox in the adopted journal")
                    .map_err(integrity)?;
                (1, 1)
            }
        };
        let (uid_next, highestmodseq) = mailbox;
        let uid = if arrival.uid >= uid_next {
            arrival.uid
        } else {
            uid_next
        };
        state
            .mail_placement
            .append_event(
                scope,
                &MailPlacementRecord::Append {
                    mailbox: arrival.mailbox,
                    uid,
                    modseq: highestmodseq + 1,
                    flags: arrival.flags.split_whitespace().map(String::from).collect(),
                    content_record_id: arrival.message_id,
                    internal_date: arrival.internal_date,
                },
            )
            .await
            .context("re-file an arrival into the adopted journal")
            .map_err(integrity)?;
    }
    Ok(())
}

/// One `bridge_imap_messages` row as the target held it before the commit.
struct HeldMessageRow {
    mailbox: String,
    uid: i64,
    message_id: Vec<u8>,
    flags: String,
    modseq: i64,
    internal_date: i64,
    created_at: i64,
    from_norm: String,
    to_norm: String,
    cc_norm: String,
    subject_norm: String,
}

/// Re-state the actor's mail placement rows from the adopted journal's fold,
/// inside the commit transaction.
///
/// The four tables are replaced wholesale and replayed from `fold` — which
/// loses nothing, because every row the ceremony did not write is either
/// container scaffolding the corpus supersedes or a post-gesture placement the
/// re-filing put in the fold (`backup-destinations.md` § *the in-flight
/// marker*). Three things the fold cannot say are carried over from the rows
/// it replaces:
///
/// * a re-stated message's plaintext search floor (`created_at` and the
///   `*_norm` columns), which the journal does not hold;
/// * a row whose arrival door has written it but not yet journaled its
///   `Append` — kept as it is, with its mailbox's state, so the `Append` that
///   follows describes a served row;
/// * each mailbox's counters, never lowered under the same UIDVALIDITY: a
///   UID or modseq the target handed out (an arrival since expunged) is never
///   handed out again.
fn restate_mail_rows(
    tx: &rusqlite::Transaction<'_>,
    actor: &[u8; 32],
    fold: &fauna_mail::segments::placement::MailPlacementManifest,
) -> anyhow::Result<()> {
    let mut stmt = tx.prepare(
        "SELECT mailbox, uid_validity, uid_next, highestmodseq, pruned_modseq
         FROM bridge_imap_mailbox_state WHERE actor_id = ?1",
    )?;
    let held_state = stmt
        .query_map(rusqlite::params![actor.as_slice()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("read the held mailbox state")?;
    drop(stmt);
    let mut stmt = tx.prepare(
        "SELECT mailbox, uid, message_id, flags, modseq, internal_date, created_at,
                from_norm, to_norm, cc_norm, subject_norm
         FROM bridge_imap_messages WHERE actor_id = ?1",
    )?;
    let held_rows = stmt
        .query_map(rusqlite::params![actor.as_slice()], |r| {
            Ok(HeldMessageRow {
                mailbox: r.get(0)?,
                uid: r.get(1)?,
                message_id: r.get(2)?,
                flags: r.get(3)?,
                modseq: r.get(4)?,
                internal_date: r.get(5)?,
                created_at: r.get(6)?,
                from_norm: r.get(7)?,
                to_norm: r.get(8)?,
                cc_norm: r.get(9)?,
                subject_norm: r.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("read the held placement rows")?;
    drop(stmt);

    for table in KindArm::Mail.placement_tables() {
        tx.execute(
            &format!("DELETE FROM {table} WHERE actor_id = ?1"),
            rusqlite::params![actor.as_slice()],
        )
        .with_context(|| format!("clear the actor's {table} rows"))?;
    }
    crate::restore::mail::replay_mail_manifest_into_sqlite(tx, actor, fold)
        .context("replay the adopted placement journal")?;

    let placed: std::collections::HashSet<&[u8]> = fold
        .placements
        .iter()
        .map(|p| p.content_record_id.as_slice())
        .collect();
    for row in &held_rows {
        if placed.contains(row.message_id.as_slice()) {
            tx.execute(
                "UPDATE bridge_imap_messages
                 SET created_at = ?3, from_norm = ?4, to_norm = ?5, cc_norm = ?6,
                     subject_norm = ?7
                 WHERE actor_id = ?1 AND message_id = ?2",
                rusqlite::params![
                    actor.as_slice(),
                    row.message_id.as_slice(),
                    row.created_at,
                    &row.from_norm,
                    &row.to_norm,
                    &row.cc_norm,
                    &row.subject_norm,
                ],
            )
            .context("carry a re-stated message's search floor")?;
            continue;
        }
        if let Some((_, validity, ..)) = held_state.iter().find(|s| s.0 == row.mailbox) {
            tx.execute(
                "INSERT OR IGNORE INTO bridge_imap_mailbox_state (actor_id, mailbox, uid_validity)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![actor.as_slice(), &row.mailbox, validity],
            )
            .context("keep a mid-door arrival's mailbox")?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO bridge_imap_messages
                (actor_id, mailbox, uid, message_id, flags, modseq, internal_date,
                 created_at, from_norm, to_norm, cc_norm, subject_norm)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                actor.as_slice(),
                &row.mailbox,
                row.uid,
                row.message_id.as_slice(),
                &row.flags,
                row.modseq,
                row.internal_date,
                row.created_at,
                &row.from_norm,
                &row.to_norm,
                &row.cc_norm,
                &row.subject_norm,
            ],
        )
        .context("keep a mid-door arrival's row")?;
    }
    for (mailbox, validity, uid_next, highestmodseq, pruned_modseq) in &held_state {
        tx.execute(
            "UPDATE bridge_imap_mailbox_state
             SET uid_next = MAX(uid_next, ?4), highestmodseq = MAX(highestmodseq, ?5),
                 pruned_modseq = MAX(pruned_modseq, ?6)
             WHERE actor_id = ?1 AND mailbox = ?2 AND uid_validity = ?3",
            rusqlite::params![
                actor.as_slice(),
                mailbox,
                validity,
                uid_next,
                highestmodseq,
                pruned_modseq
            ],
        )
        .context("keep the mailbox counters the target handed out")?;
    }
    Ok(())
}

/// Write the corpus's calendars or address books into the target's container
/// rows — the containers before the halves (`backup-destinations.md`
/// § *the in-flight marker*, rule (2)), for a DAV kind.
///
/// Mail's [`write_corpus_mailboxes`] rule over a DAV kind's own counters: one
/// small transaction, the corpus's metadata, and `highestmodseq`/`ctag` raised
/// to the corpus's, never lowered — a retry over arrivals finds them already
/// advanced past it. The prune floor is raised to the corpus's and never
/// lowered either; the commit's re-statement carries it on. Both DAV put doors mint the next change number from this
/// row (and refuse a PUT into a collection that has none), so from here on an
/// arrival is numbered above every change the corpus holds.
async fn write_corpus_collections(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: KindArm,
    fold: &DavFold,
) -> anyhow::Result<()> {
    let t = arm
        .dav_tables()
        .context("only a DAV kind has collections")?;
    let now = now_epoch_secs();
    let conn = state.db.conn().await;
    let tx = conn
        .unchecked_transaction()
        .context("begin the corpus containers write")?;
    for (id, c) in &fold.collections {
        let counter = i64::try_from(c.highestmodseq).unwrap_or(i64::MAX);
        let floor = i64::try_from(c.pruned_modseq).unwrap_or(i64::MAX);
        tx.execute(
            &format!(
                "INSERT INTO {coll}
                    (actor_id, {col}, encrypted_metadata, ctag, highestmodseq, created_at,
                     pruned_modseq)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)
                 ON CONFLICT(actor_id, {col}) DO UPDATE SET
                    encrypted_metadata = excluded.encrypted_metadata,
                    ctag               = MAX(ctag, excluded.ctag),
                    highestmodseq      = MAX(highestmodseq, excluded.highestmodseq),
                    pruned_modseq      = MAX(pruned_modseq, excluded.pruned_modseq)",
                coll = t.collections,
                col = t.collection_id,
            ),
            rusqlite::params![
                scope.as_slice(),
                id.as_slice(),
                &c.encrypted_metadata,
                counter,
                now,
                floor
            ],
        )
        .with_context(|| format!("write the corpus's {} row", t.collections))?;
    }
    tx.commit().context("commit the corpus containers write")?;
    Ok(())
}

/// One live DAV arrival as its door wrote it, with its collection's row.
struct DavArrival {
    collection: [u8; 32],
    id: [u8; 32],
    uid_hash: [u8; 32],
    etag: String,
    modseq: u64,
    ciphertext_size: u32,
    /// Server receive time, epoch seconds — the clock collision (a) reads.
    created_at: i64,
    sidecar: Option<Vec<u8>>,
    /// The collection row's metadata and counter, as the door left them.
    collection_metadata: Option<Vec<u8>>,
    collection_counter: u64,
}

fn blob32(bytes: Vec<u8>, what: &str) -> anyhow::Result<[u8; 32]> {
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("a {what} of {} bytes, not 32", bytes.len()))
}

/// Every live item of `arm`'s kind the arrival doors wrote, joined to a live
/// mirror row in a segment outside the corpus's — what arrived since the
/// gesture — oldest change first.
async fn read_dav_arrivals(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    t: DavTables,
    corpus_ids: &[u32],
) -> anyhow::Result<Vec<DavArrival>> {
    type Row = (
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        String,
        i64,
        i64,
        i64,
        Option<Vec<u8>>,
    );
    let rows: Vec<(Row, Option<Vec<u8>>, i64, i64)> = {
        let conn = state.db.conn().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT e.{col}, e.{item}, e.uid_hash, e.etag, e.modseq, e.ciphertext_size,
                        e.created_at, e.encrypted_fauna_ext,
                        c.encrypted_metadata, COALESCE(c.highestmodseq, 0), sr.segment_id
                 FROM {items} e
                 JOIN segment_records sr
                   ON sr.kind = ?2 AND sr.scope_id = e.actor_id
                  AND sr.record_cid = e.record_cid AND sr.tombstoned = 0
                 LEFT JOIN {coll} c
                   ON c.actor_id = e.actor_id AND c.{col} = e.{col}
                 WHERE e.actor_id = ?1
                 ORDER BY e.modseq, e.{item}",
                col = t.collection_id,
                item = t.item_id,
                items = t.items,
                coll = t.collections,
            ))
            .context("prepare the DAV arrivals read")?;
        stmt.query_map(rusqlite::params![scope.as_slice(), t.kind], |r| {
            Ok((
                (
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ),
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })
        .context("read the DAV arrivals")?
        .collect::<Result<Vec<_>, _>>()
        .context("collect the DAV arrivals")?
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for ((col, id, uid, etag, modseq, size, created_at, sidecar), metadata, counter, seg) in rows {
        if u32::try_from(seg).is_ok_and(|s| corpus_ids.contains(&s)) || !seen.insert(id.clone()) {
            continue;
        }
        out.push(DavArrival {
            collection: blob32(col, "collection id")?,
            id: blob32(id, "content-record id")?,
            uid_hash: blob32(uid, "uid hash")?,
            etag,
            modseq: modseq.max(0) as u64,
            ciphertext_size: u32::try_from(size).context("an arrival's ciphertext size")?,
            created_at,
            sidecar,
            collection_metadata: metadata,
            collection_counter: counter.max(0) as u64,
        });
    }
    Ok(out)
}

/// The server receive time of every record in the corpus's own content
/// segments, by content-record id — read from the records' own floors, the
/// clock collision (a) compares an arrival against.
fn corpus_receive_times(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: KindArm,
    corpus_ids: &[u32],
) -> anyhow::Result<HashMap<[u8; 32], i64>> {
    let root = arm.segments(state).scope_dir(scope);
    let walk = "walk the corpus's segments for their receive times";
    Ok(match arm {
        KindArm::Calendar => {
            crate::filesync_handlers::walk_pinned_segments(&root, "calendar", corpus_ids, |b| {
                fauna_calendar::segments::CalFloorMetadata::decode(b)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .context(walk)?
            .0
            .into_iter()
            .map(|r| (r.floor.event_id, r.floor.created_at))
            .collect()
        }
        KindArm::Card => {
            crate::filesync_handlers::walk_pinned_segments(&root, "card", corpus_ids, |b| {
                fauna_contacts::segments::CardFloorMetadata::decode(b)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
            .context(walk)?
            .0
            .into_iter()
            .map(|r| (r.floor.card_id, r.floor.created_at))
            .collect()
        }
        KindArm::Mail | KindArm::Post => anyhow::bail!("only a DAV kind has a DAV corpus"),
    })
}

/// A collection's creation, as `arm`'s journal records it.
fn dav_provision(arm: KindArm, id: [u8; 32], encrypted_metadata: Vec<u8>) -> DavRecord {
    use fauna_calendar::segments::placement::CalPlacementRecord;
    use fauna_contacts::segments::placement::CardPlacementRecord;
    match arm {
        KindArm::Calendar => DavRecord::Cal(CalPlacementRecord::ProvisionCalendar {
            calendar_id: id,
            encrypted_metadata,
        }),
        _ => DavRecord::Card(CardPlacementRecord::ProvisionAddressbook {
            addressbook_id: id,
            encrypted_metadata,
        }),
    }
}

/// A resource's version, as `arm`'s journal records it under `modseq` and
/// `etag`.
fn dav_version_record(
    arm: KindArm,
    (collection, uid_hash): ([u8; 32], [u8; 32]),
    version: &DavVersion,
    modseq: u64,
    etag: String,
) -> DavRecord {
    use fauna_calendar::segments::placement::CalPlacementRecord;
    use fauna_contacts::segments::placement::CardPlacementRecord;
    match (arm, version.clone()) {
        (
            KindArm::Calendar,
            DavVersion::Placed {
                id,
                sidecar,
                ciphertext_size,
                ..
            },
        ) => DavRecord::Cal(CalPlacementRecord::PutEvent {
            calendar_id: collection,
            uid_hash,
            etag,
            modseq,
            ciphertext_size,
            event_id: id,
            encrypted_fauna_ext: sidecar,
        }),
        (KindArm::Calendar, DavVersion::Deleted { id, deleted_at, .. }) => {
            DavRecord::Cal(CalPlacementRecord::DeleteEvent {
                calendar_id: collection,
                uid_hash,
                modseq,
                event_id: id,
                deleted_at,
            })
        }
        (
            _,
            DavVersion::Placed {
                id,
                sidecar,
                ciphertext_size,
                ..
            },
        ) => DavRecord::Card(CardPlacementRecord::PutCard {
            addressbook_id: collection,
            uid_hash,
            etag,
            modseq,
            ciphertext_size,
            card_id: id,
            encrypted_fauna_ext: sidecar,
        }),
        (_, DavVersion::Deleted { id, deleted_at, .. }) => {
            DavRecord::Card(CardPlacementRecord::DeleteCard {
                addressbook_id: collection,
                uid_hash,
                modseq,
                card_id: id,
                deleted_at,
            })
        }
    }
}

/// Raise the door's change counter for one collection to at least `floor`,
/// so a number the re-filing takes is never minted again by a PUT.
async fn floor_dav_counter(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: KindArm,
    collection: &[u8; 32],
    floor: u64,
) -> Result<(), MaterializeRefusal> {
    let floor = i64::try_from(floor).unwrap_or(i64::MAX);
    match arm {
        KindArm::Calendar => {
            state
                .db
                .floor_bridge_caldav_calendar_counter(scope, collection, floor)
                .await
        }
        _ => {
            state
                .db
                .floor_bridge_carddav_addressbook_counter(scope, collection, floor)
                .await
        }
    }
    .context("floor the collection's change counter")
    .map_err(MaterializeRefusal::Integrity)
}

/// **Re-file the calendar or contacts items that arrived since the gesture
/// into the adopted journal** (`backup-destinations.md` § *the in-flight
/// marker*, rule (3), for a DAV kind) — [`refile_mail_arrivals`]' twin, read
/// through recover's [`DavFold`] so the two DAV kinds are one path and the
/// journal's version of a resource is judged exactly as recovery judges it.
///
/// What arrived is read from the rows the doors wrote, as mail's is, for
/// mail's reason: only the commit removes them, so a crash anywhere between the
/// superseding adoption and the commit leaves them for the retry. Each one
/// becomes the current version of its `(collection, UID)` resource:
///
/// * **Already the journal's current version** (an earlier run re-filed it,
///   or its door journaled it after the adoption) — nothing to do.
/// * **The corpus also holds the resource — collision (a): the later write
///   wins, by server receive time**, a tie going to the corpus, as in
///   recovery ([`held_write_stands`]). The corpus's version's time is its
///   record's floor (a placement) or its `deleted_at` (a deletion). When the
///   corpus's version stands it must still be numbered above the change the
///   arrival was served under — a client holding that sync token would
///   otherwise never hear the resource changed — so it is journaled again under
///   the collection's next change number, unless an earlier run already did.
/// * **Otherwise the arrival files**, its collection created by id where the
///   corpus lacks it (with the metadata the door's row holds), under the change
///   number and ETag it was served with — or, for an arrival numbered before
///   the containers landed, the collection's next number.
///
/// A number taken here raises the door's counter first, so no PUT mints it
/// again. Tombstones a door wrote since the gesture are not re-filed, as mail's
/// are not.
async fn refile_dav_arrivals(
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: KindArm,
    corpus_ids: &[u32],
) -> Result<(), MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let t = arm
        .dav_tables()
        .ok_or_else(|| integrity(anyhow::anyhow!("only a DAV kind re-files DAV arrivals")))?;
    let arrivals = read_dav_arrivals(state, scope, t, corpus_ids)
        .await
        .map_err(integrity)?;
    if arrivals.is_empty() {
        return Ok(());
    }
    let received = corpus_receive_times(state, scope, arm, corpus_ids).map_err(integrity)?;

    for a in arrivals {
        let fold = current_fold(state, scope, arm).await?.dav_fold();
        if fold.current().contains_key(&a.id) {
            continue;
        }
        let resource = (a.collection, a.uid_hash);
        let counter = fold
            .collections
            .get(&a.collection)
            .map_or(1, |c| c.highestmodseq);
        let next = counter.max(a.collection_counter) + 1;

        if let Some(held) = fold.resources.get(&resource) {
            let corpus_at = match held {
                DavVersion::Placed { id, .. } => received.get(id).copied(),
                DavVersion::Deleted { deleted_at, .. } => Some(*deleted_at),
            };
            let arrival_stands = match corpus_at {
                Some(at) => held_write_stands(a.created_at, at),
                // Not one of the corpus's records: a version this journal took
                // on its own since the adoption, and the higher change number
                // is the later write.
                None => a.modseq > held.modseq(),
            };
            if !arrival_stands {
                if held.modseq() <= a.modseq {
                    floor_dav_counter(state, scope, arm, &a.collection, next).await?;
                    let etag = crate::db::bridge_dav_common::format_etag(next as i64);
                    dav_journal(
                        state,
                        scope,
                        dav_version_record(arm, resource, held, next, etag),
                    )
                    .await?;
                }
                continue;
            }
        }

        if !fold.collections.contains_key(&a.collection) {
            let metadata = a.collection_metadata.clone().ok_or_else(|| {
                integrity(anyhow::anyhow!(
                    "an arrival sits in a {} the target holds no row for",
                    t.collections
                ))
            })?;
            dav_journal(state, scope, dav_provision(arm, a.collection, metadata)).await?;
        }
        let (modseq, etag) = if a.modseq > counter {
            (a.modseq, a.etag)
        } else {
            floor_dav_counter(state, scope, arm, &a.collection, next).await?;
            (next, crate::db::bridge_dav_common::format_etag(next as i64))
        };
        let version = DavVersion::Placed {
            id: a.id,
            modseq,
            sidecar: a.sidecar,
            ciphertext_size: a.ciphertext_size,
        };
        dav_journal(
            state,
            scope,
            dav_version_record(arm, resource, &version, modseq, etag),
        )
        .await?;
    }
    Ok(())
}

/// One DAV item row as the target held it before the commit.
struct HeldDavRow {
    collection: [u8; 32],
    id: [u8; 32],
    uid_hash: [u8; 32],
    encrypted_index_hint: Vec<u8>,
    etag: String,
    modseq: i64,
    ciphertext_size: i64,
    internal_date: i64,
    created_at: i64,
    sidecar: Option<Vec<u8>>,
    record_cid: Option<Vec<u8>>,
}

/// One DAV collection row as the target held it before the commit.
struct HeldDavCollection {
    id: Vec<u8>,
    encrypted_metadata: Vec<u8>,
    ctag: i64,
    highestmodseq: i64,
    created_at: i64,
    pruned_modseq: i64,
}

/// Re-state the actor's calendar or contacts rows from the adopted journal's
/// fold, inside the commit transaction — [`restate_mail_rows`]' twin, and for
/// its reason the three tables are replaced wholesale and still lose nothing.
///
/// The corpus's items are rebuilt through the snapshot restore's own rebuild
/// ([`crate::filesync_handlers::rebuild_calendar_rows_from_disk`] and its card
/// twin), from the corpus's segments. A post-gesture item's record is not in
/// those segments, so the row its door wrote is carried instead, under the
/// placement the fold gives it. Each held item row is decided by the fold,
/// change number against change number:
///
/// * the fold places it → carried, re-stated with the fold's collection,
///   change number, ETag and sidecar (the re-filing's decision);
/// * the fold numbers its resource **above** it → dropped: a later version
///   stands (collision (a) went to the corpus, which the re-filing re-numbered);
/// * otherwise its door is between its row and its journal record → kept as it
///   is, with its collection's row, so the record that follows describes a
///   served row; the corpus's version of that resource, older, is not placed.
///
/// Each held collection's counters are carried, never lowered: a change number
/// the target handed out is never handed out again. Its prune floor is carried
/// the same way, since the expunge log is replaced by the fold's tombstones.
fn restate_dav_rows(
    tx: &rusqlite::Transaction<'_>,
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: KindArm,
    ids: &[u32],
    fold: &AdoptedFold,
) -> anyhow::Result<u64> {
    let t = arm
        .dav_tables()
        .context("only a DAV kind re-states DAV rows")?;
    let dav = fold.dav_fold();
    let current = dav.current();
    let etags = fold.dav_etags();

    let mut stmt = tx.prepare(&format!(
        "SELECT {col}, {item}, uid_hash, encrypted_index_hint, etag, modseq,
                ciphertext_size, internal_date, created_at, encrypted_fauna_ext, record_cid
         FROM {items} WHERE actor_id = ?1",
        col = t.collection_id,
        item = t.item_id,
        items = t.items,
    ))?;
    type Raw = (
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        String,
        i64,
        i64,
        i64,
        i64,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
    );
    let raw: Vec<Raw> = stmt
        .query_map(rusqlite::params![scope.as_slice()], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("read the held item rows")?;
    drop(stmt);
    let mut held_rows = Vec::with_capacity(raw.len());
    for (col, id, uid, hint, etag, modseq, size, date, created, sidecar, cid) in raw {
        held_rows.push(HeldDavRow {
            collection: blob32(col, "collection id")?,
            id: blob32(id, "content-record id")?,
            uid_hash: blob32(uid, "uid hash")?,
            encrypted_index_hint: hint,
            etag,
            modseq,
            ciphertext_size: size,
            internal_date: date,
            created_at: created,
            sidecar,
            record_cid: cid,
        });
    }
    let mut stmt = tx.prepare(&format!(
        "SELECT {col}, encrypted_metadata, ctag, highestmodseq, created_at, pruned_modseq
         FROM {coll} WHERE actor_id = ?1",
        col = t.collection_id,
        coll = t.collections,
    ))?;
    let held_collections = stmt
        .query_map(rusqlite::params![scope.as_slice()], |r| {
            Ok(HeldDavCollection {
                id: r.get(0)?,
                encrypted_metadata: r.get(1)?,
                ctag: r.get(2)?,
                highestmodseq: r.get(3)?,
                created_at: r.get(4)?,
                pruned_modseq: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("read the held collection rows")?;
    drop(stmt);

    // Decide every held row against the fold.
    let mut carried: Vec<HeldDavRow> = Vec::new();
    let mut mid_door: Vec<HeldDavRow> = Vec::new();
    for mut row in held_rows {
        if let Some(p) = current.get(&row.id) {
            let resource = (p.collection, p.uid_hash);
            let modseq = dav.resources.get(&resource).map_or(0, DavVersion::modseq);
            row.collection = p.collection;
            row.uid_hash = p.uid_hash;
            row.modseq = i64::try_from(modseq).unwrap_or(i64::MAX);
            row.etag = etags
                .get(&row.id)
                .cloned()
                .unwrap_or_else(|| crate::db::bridge_dav_common::format_etag(row.modseq));
            row.sidecar = p.sidecar.clone();
            carried.push(row);
            continue;
        }
        match dav.resources.get(&(row.collection, row.uid_hash)) {
            Some(v) if i64::try_from(v.modseq()).unwrap_or(i64::MAX) > row.modseq => {}
            _ => mid_door.push(row),
        }
    }
    let carried_ids: HashSet<[u8; 32]> = carried.iter().map(|r| r.id).collect();
    let mid_door_resources: HashSet<([u8; 32], [u8; 32])> = mid_door
        .iter()
        .map(|r| (r.collection, r.uid_hash))
        .collect();

    for table in arm.placement_tables() {
        tx.execute(
            &format!("DELETE FROM {table} WHERE actor_id = ?1"),
            rusqlite::params![scope.as_slice()],
        )
        .with_context(|| format!("clear the actor's {table} rows"))?;
    }
    // The corpus's own items, rebuilt from its segments; what is carried or
    // kept below is left out of the rebuild, whose records are not there.
    let rebuilt = |collection: &[u8; 32], uid_hash: &[u8; 32], id: &[u8; 32]| {
        !carried_ids.contains(id) && !mid_door_resources.contains(&(*collection, *uid_hash))
    };
    match fold {
        AdoptedFold::Calendar(m) => {
            let mut corpus = m.clone();
            corpus
                .events
                .retain(|e| rebuilt(&e.calendar_id, &e.uid_hash, &e.event_id));
            crate::filesync_handlers::rebuild_calendar_rows_from_disk(
                tx, state, scope, ids, &corpus,
            )
            .context("rebuild the calendar rows from the materialized corpus")?;
        }
        AdoptedFold::Card(m) => {
            let mut corpus = m.clone();
            corpus
                .cards
                .retain(|c| rebuilt(&c.addressbook_id, &c.uid_hash, &c.card_id));
            crate::filesync_handlers::rebuild_card_rows_from_disk(tx, state, scope, ids, &corpus)
                .context("rebuild the card rows from the materialized corpus")?;
        }
        AdoptedFold::Mail(_) => anyhow::bail!("the adopted journal is not a DAV kind's"),
    }

    for row in carried.iter().chain(&mid_door) {
        tx.execute(
            &format!(
                "INSERT INTO {items}
                    (actor_id, {col}, {item}, uid_hash, encrypted_index_hint, etag, modseq,
                     ciphertext_size, internal_date, created_at, encrypted_fauna_ext, record_cid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                items = t.items,
                col = t.collection_id,
                item = t.item_id,
            ),
            rusqlite::params![
                scope.as_slice(),
                row.collection.as_slice(),
                row.id.as_slice(),
                row.uid_hash.as_slice(),
                &row.encrypted_index_hint,
                &row.etag,
                row.modseq,
                row.ciphertext_size,
                row.internal_date,
                row.created_at,
                row.sidecar.as_deref(),
                row.record_cid.as_deref(),
            ],
        )
        .context("carry an arrival's row")?;
    }
    for c in &held_collections {
        if mid_door
            .iter()
            .any(|r| r.collection.as_slice() == c.id.as_slice())
        {
            tx.execute(
                &format!(
                    "INSERT OR IGNORE INTO {coll}
                        (actor_id, {col}, encrypted_metadata, ctag, highestmodseq, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    coll = t.collections,
                    col = t.collection_id,
                ),
                rusqlite::params![
                    scope.as_slice(),
                    &c.id,
                    &c.encrypted_metadata,
                    c.ctag,
                    c.highestmodseq,
                    c.created_at
                ],
            )
            .context("keep a mid-door arrival's collection")?;
        }
        tx.execute(
            &format!(
                "UPDATE {coll}
                 SET highestmodseq = MAX(highestmodseq, ?3), ctag = MAX(ctag, ?4),
                     pruned_modseq = MAX(pruned_modseq, ?5)
                 WHERE actor_id = ?1 AND {col} = ?2",
                coll = t.collections,
                col = t.collection_id,
            ),
            rusqlite::params![
                scope.as_slice(),
                &c.id,
                c.highestmodseq,
                c.ctag,
                c.pruned_modseq
            ],
        )
        .context("keep the change counters the target handed out")?;
    }
    let placed: i64 = tx.query_row(
        &format!("SELECT COUNT(*) FROM {} WHERE actor_id = ?1", t.items),
        rusqlite::params![scope.as_slice()],
        |r| r.get(0),
    )?;
    Ok(placed.max(0) as u64)
}

/// Make the materialized corpus live, inside the caller's transaction: the
/// `segment_records` mirror rebuilt from the written sidecars, and the adopted
/// journal's placement rows replayed — each through the snapshot restore's own
/// rebuild for that kind, so the restored row shape cannot fork between the two
/// restore sources. Returns the placement count
/// ([`MaterializeOutcome::placements`]).
fn rebuild_rows(
    tx: &rusqlite::Transaction<'_>,
    state: &Arc<AppState>,
    scope: &[u8; 32],
    arm: KindArm,
    ids: &[u32],
    journal: Option<&AdoptedFold>,
    post_records: &[crate::segments::post::RestoredPost],
) -> Result<Option<u64>, MaterializeRefusal> {
    let integrity = MaterializeRefusal::Integrity;
    let kind = match arm {
        KindArm::Mail => "mail",
        KindArm::Post => "post",
        KindArm::Calendar => "calendar",
        KindArm::Card => "card",
    };
    match (arm, journal) {
        (KindArm::Mail, fold) => {
            crate::filesync_handlers::rebuild_mail_segment_records_from_disk(tx, state, scope, ids)
                .context("rebuild segment_records from the materialized sidecars")
                .map_err(integrity)?;
            let Some(AdoptedFold::Mail(fold)) = fold else {
                // No journal and (as the caller already proved) no content
                // either.
                return Ok(None);
            };
            restate_mail_rows(tx, scope, fold).map_err(integrity)?;
            Ok(Some(fold.placements.len() as u64))
        }
        (KindArm::Post, None) => {
            crate::segments::post::reassert_records(tx, scope, post_records)
                .context("re-assert the materialized posts")
                .map_err(integrity)?;
            Ok(None)
        }
        // No journal and (as the caller already proved) no content either.
        (KindArm::Calendar | KindArm::Card, None) => Ok(None),
        (KindArm::Calendar, Some(fold @ AdoptedFold::Calendar(_)))
        | (KindArm::Card, Some(fold @ AdoptedFold::Card(_))) => {
            let placed = restate_dav_rows(tx, state, scope, arm, ids, fold).map_err(integrity)?;
            Ok(Some(placed))
        }
        _ => Err(integrity(anyhow::anyhow!(
            "the adopted journal is not the `{kind}` kind's"
        ))),
    }
}

/// Materialize one **covered-folder** custody set for `actor` — the mirror axis.
///
/// Where the segments arm unseals, verifies and reconstitutes bytes, this one
/// touches no content at all. The mirror's manifests and chunks already ARE the
/// folder's at-rest corpus: they were pushed **as-is**, the source's own
/// ciphertext, because a covered folder is already sealed under keys only the
/// owner holds. So materializing one is **pure row re-homing, no keys anywhere**
/// (`message-segment-store.md` § Client-device custodian (pull) → *Restore*) —
/// re-creating the folder set and one live record per custody row, each minted
/// from the triple custody carries: `(path_hash, path_sealed, manifest_hash)`.
///
/// Two things custody deliberately does not carry, and where each comes from:
///
///   * **the folder's display name** — a mirror holds sealed paths, not a label,
///     and the set name carries only the *source* nest id and the *source's*
///     folder rowid, neither meaningful on the target. It rides the verb
///     payload, from the folder state of the custodian driving the ceremony.
///   * **the source's path hash** — the custody row's own `path_hash` is a hash
///     *of* the mirror's leaf, not the source's address. The source's hash is
///     the leaf itself, hex-spelled: both push arms record it that way
///     (`segment_backup.rs`'s `rest_path`, `reseed.rs`'s `record_folder`), which
///     is exactly what lets a federated relay's corpus and a custodian's key
///     alike.
///
/// The empty-target rule is **not** checked here. It is enforced once, at the
/// write, inside the transaction that does the re-homing
/// ([`crate::db::sync_storage::SyncStorage::materialize_folder_custody`]) — a
/// top-of-verb read holds no lock, and the segments arm has already paid for
/// that lesson once. Nothing in this function reads the target folder, so there
/// is no second copy of the rule here to drift out of step with the real one.
///
/// The rule itself is **freshness**, not just emptiness: a target refuses if it
/// holds foreign live records, and equally if it holds none but is group-bound,
/// public, website-serving, WebDAV-exposed or paywalled.
///
/// **Owner-signed, paged, into a set the ceremony prepared**
/// (`writer-signed-change-records.md` ruling (7)(a)). The target is a live set
/// the owner lists under `folder_name_hash` when the request carries one (else
/// `folder_display_name`), with a stored nonce — created by
/// the ceremony's seed-holding process, never here (a missing one refuses
/// `target_missing`). The request carries one page of the owner's signatures
/// keyed by `path_hash`; each is verified at the door
/// ([`crate::change_signature::verify_carried`]) over
/// [`fauna_protocol::sync_writer_sig::SignedChange::for_rehome`] under the
/// target's stored nonce before anything is written, and the page is refused
/// whole on one that does not. Exactly the carried-and-verified rows are
/// minted, one transaction per page; [`MaterializeOutcome::remaining`] says how
/// many custody rows are still owed.
pub(crate) async fn materialize_folder_set(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    set_name: &str,
    folder_display_name: Option<&str>,
    folder_name_hash: Option<&[u8]>,
    signer_key: Option<&[u8]>,
    signatures: &[fauna_protocol::backup::RehomeSignature],
) -> Result<MaterializeOutcome, MaterializeRefusal> {
    // The name has to parse on the folder axis, by the same parse-then-re-derive
    // rule the segment axis uses — an owner-authed caller may declare the source
    // nest id here (it crosses no principal boundary: the set is minted under
    // their own actor id), but only in a spelling this nest could have produced.
    parse_folder_backup_set_name(set_name).ok_or_else(|| MaterializeRefusal::NotASegmentSet {
        set_name: set_name.to_string(),
    })?;

    // The target is named by its address, its display name, or both. A sealed
    // set's row rests no plaintext name (`path-sealing.md` § the set-name
    // plane), so the address alone is a whole request; when both ride, the
    // hash must be the name's, so the two can never name different sets
    // (`CustodyMaterializeRequest::folder_name_hash`).
    let named = folder_display_name.map(str::trim).filter(|n| !n.is_empty());
    if let Some(name) = named
        && fauna_core::sync::is_reserved_folder_name(name)
    {
        return Err(MaterializeRefusal::FolderNameReserved {
            name: name.to_string(),
        });
    }
    let name_hash: Option<[u8; 32]> = match folder_name_hash {
        None => None,
        Some(carried) => {
            let carried: [u8; 32] =
                carried
                    .try_into()
                    .map_err(|_| MaterializeRefusal::FolderNameHashMismatch {
                        folder: named.map_or_else(|| hex::encode(carried), str::to_string),
                    })?;
            if let Some(name) = named
                && carried != fauna_core::path_crypto::set_name_hash(name)
            {
                return Err(MaterializeRefusal::FolderNameHashMismatch {
                    folder: name.to_string(),
                });
            }
            Some(carried)
        }
    };
    // How the refusals below name the folder: its display name when the
    // request carried one, else its address — never a guess at the label.
    let display_name: String = match (named, &name_hash) {
        (Some(name), _) => name.to_string(),
        (None, Some(hash)) => hex::encode(hash),
        (None, None) => {
            return Err(MaterializeRefusal::FolderNameRequired {
                set_name: set_name.to_string(),
            });
        }
    };
    let display_name = display_name.as_str();

    // ── The custody the set holds ────────────────────────────────────────
    //
    // Every row is validated before ANY of them is written, so a bad row late in
    // the set leaves the target as untouched as one first — the same ordering
    // discipline the segments arm keeps for its byte verification, and the
    // reason a refusal below can promise it changed nothing.
    let custody = state
        .db
        .list_backup_custody_in_set(actor, set_name)
        .await
        .map_err(MaterializeRefusal::Integrity)?;
    // Each row's size is its manifest's `total_size`, read through the custody
    // charge's own manifest read — the figure the owner's device signed, and
    // the one a live row carries everywhere else (the engine records
    // `uploaded.manifest.total_size`). The custody row's `size_bytes` is the
    // nest's DERIVED charge, never the source's size, so it is not read here
    // (`writer-signed-change-records.md` ruling (7)(a)(ii), *Where the statement
    // reads its size*).
    let backup_svc = state.backup_service.as_ref().ok_or_else(|| {
        MaterializeRefusal::Integrity(anyhow::anyhow!(
            "a covered-folder materialize with no blob store configured"
        ))
    })?;
    let blob_store = backup_svc.local_blob_store();
    let mut rows: Vec<crate::db::sync_storage::FolderRehomeRow> = Vec::with_capacity(custody.len());
    for row in custody {
        // The custody row's own address, for the refusals — the only stable
        // handle on a row whose content is otherwise opaque here.
        let at = hex::encode(&row.path_hash);
        let leaf = row
            .path
            .ok_or_else(|| MaterializeRefusal::CustodyPathUnaddressable {
                path_hash: at.clone(),
            })?;
        let mut path_hash = [0u8; 32];
        hex::decode_to_slice(&leaf, &mut path_hash).map_err(|_| {
            MaterializeRefusal::CustodyPathUnaddressable {
                path_hash: at.clone(),
            }
        })?;
        let path_sealed = row.path_sealed.filter(|b| !b.is_empty()).ok_or_else(|| {
            MaterializeRefusal::CustodyPathUnsealed {
                path_hash: at.clone(),
            }
        })?;
        let manifest_hash: [u8; 32] = row.manifest_hash.as_slice().try_into().map_err(|_| {
            MaterializeRefusal::Integrity(anyhow::anyhow!(
                "the custody row at {at} carries a {}-byte manifest hash, not 32",
                row.manifest_hash.len()
            ))
        })?;
        let manifest = super::custody_charge::held_manifest(
            &blob_store,
            backup_svc.encryption_key(),
            &manifest_hash,
        )
        .await
        .map_err(MaterializeRefusal::Integrity)?
        .map_err(|refusal| MaterializeRefusal::CustodyManifestUnreadable {
            path_hash: at.clone(),
            detail: refusal.to_string(),
        })?;
        let size_bytes = i64::try_from(manifest.total_size).map_err(|_| {
            MaterializeRefusal::CustodyManifestUnreadable {
                path_hash: at.clone(),
                detail: format!("total_size {} exceeds i64", manifest.total_size),
            }
        })?;
        rows.push(crate::db::sync_storage::FolderRehomeRow {
            path_hash,
            path_sealed,
            manifest_hash,
            size_bytes,
        });
    }

    if rows.is_empty() {
        return Err(MaterializeRefusal::EmptyFolderSet {
            set_name: set_name.to_string(),
        });
    }

    // Owner-pays, exactly as every other record door meters: the ceiling is the
    // set owner's tier, and the owner here is the authenticated actor.
    let max_storage = state
        .db
        .get_user_tier_max_storage_bytes(actor)
        .await
        .map_err(MaterializeRefusal::Integrity)?
        // No tier row means no ceiling, exactly as every other record door reads
        // it (`sync_handlers::record_change_core`).
        .unwrap_or(i64::MAX);

    // ── The page: owner-signed, or nothing ───────────────────────────────
    //
    // The arm never mints an unsigned row (ruling (7)(a)(ii)).
    let signer_key = match signer_key {
        Some(key) if !signatures.is_empty() => key,
        _ => {
            return Err(MaterializeRefusal::FolderRehomeUnsigned {
                set_name: set_name.to_string(),
            });
        }
    };
    if signatures.len() > fauna_protocol::backup::MATERIALIZE_REHOME_PAGE {
        return Err(MaterializeRefusal::RehomePageMalformed {
            detail: format!(
                "{} signatures exceed the page of {}",
                signatures.len(),
                fauna_protocol::backup::MATERIALIZE_REHOME_PAGE
            ),
        });
    }

    // The target: a live set the owner lists, carrying a stored nonce — the
    // ceremony prepared it (ruling (7)(a)(i)).
    // By its address when one rode the request (the folder's plaintext name
    // leaves the row: `path-sealing.md` § the set-name plane), else by name; the
    // write below then keys on the row id this resolved, never on a name.
    let target = match &name_hash {
        Some(hash) => {
            state
                .db
                .get_folder_for_actor_by_name_hash(hash, actor)
                .await
        }
        None => state.db.get_folder_for_actor(display_name, actor).await,
    }
    .map_err(MaterializeRefusal::Integrity)?
    .filter(|fs| !fs.custody_copy)
    .ok_or_else(|| MaterializeRefusal::FolderTargetMissing {
        folder: display_name.to_string(),
    })?;
    // A reserved set rests its name (it is a routing constant), so a request
    // that addressed one by hash alone is refused here as the named one was.
    if fauna_core::sync::is_reserved_folder_name(&target.name) {
        return Err(MaterializeRefusal::FolderNameReserved {
            name: target.name.clone(),
        });
    }
    let set_nonce = crate::change_signature::stored_set_nonce(&target).map_err(|refusal| {
        MaterializeRefusal::RehomeSignature {
            folder: display_name.to_string(),
            refusal,
        }
    })?;

    // Every carried signature names a custody row of this set, once, and
    // verifies over that row's re-home statement — all before any write.
    let by_path: std::collections::HashMap<[u8; 32], &crate::db::sync_storage::FolderRehomeRow> =
        rows.iter().map(|r| (r.path_hash, r)).collect();
    let mut seen = std::collections::HashSet::new();
    let mut verified = Vec::with_capacity(signatures.len());
    for carried in signatures {
        let path_hash: [u8; 32] = carried.path_hash.as_slice().try_into().map_err(|_| {
            MaterializeRefusal::RehomePageMalformed {
                detail: "a signature's path_hash is not 32 bytes".into(),
            }
        })?;
        let row =
            *by_path
                .get(&path_hash)
                .ok_or_else(|| MaterializeRefusal::RehomePageMalformed {
                    detail: format!(
                        "the signature for {} names no custody row of the set",
                        hex::encode(path_hash)
                    ),
                })?;
        if !seen.insert(path_hash) {
            return Err(MaterializeRefusal::RehomePageMalformed {
                detail: format!("{} is signed twice in one page", hex::encode(path_hash)),
            });
        }
        let statement = fauna_protocol::sync_writer_sig::SignedChange::for_rehome(
            set_nonce,
            *actor,
            row.path_hash,
            row.manifest_hash,
            row.size_bytes,
            &row.path_sealed,
        );
        let signature = crate::change_signature::verify_carried(
            &state.db,
            &statement,
            crate::change_signature::CarriedSignature {
                signature: Some(&carried.signature[..]),
                signer_key: Some(signer_key),
            },
            // The owner's own connection to the nest that holds their grants:
            // a delegated signer resolves by reference, exactly as the record
            // door does.
            crate::change_signature::CertCarriage::ByReference,
        )
        .await
        .map_err(|refusal| MaterializeRefusal::RehomeSignature {
            folder: display_name.to_string(),
            refusal,
        })?;
        verified.push((row, signature));
    }
    let page: Vec<crate::db::sync_storage::FolderRehomeSigned<'_>> = verified
        .iter()
        .map(|(row, sig)| crate::db::sync_storage::FolderRehomeSigned {
            row,
            signature: sig.as_row(),
        })
        .collect();

    // The re-homed rows carry the owner's re-seed pseudo-device: never the
    // ceremony device's own id (its engine would read them as a self-echo and
    // never hydrate the folder it restored), never the nest's identity
    // (ruling (7)(a)(ii)).
    let recorder_device = fauna_core::label_custody::reseed_pseudo_device_id(actor);

    match state
        .db
        .materialize_folder_custody(
            actor,
            target.id,
            &set_nonce,
            &recorder_device,
            &rows,
            &page,
            max_storage,
        )
        .await
    {
        Ok(crate::db::sync_storage::FolderMaterializeOutcome::Done {
            rehomed,
            resumed,
            remaining,
            ..
        }) => {
            // Remember each delegated signer's cert for the list replies'
            // side table, once its rows are accepted — as the record door does.
            for (_, sig) in &verified {
                sig.remember_cert(&state.db)
                    .await
                    .map_err(MaterializeRefusal::Integrity)?;
            }
            Ok(MaterializeOutcome {
                // No segment ids: this axis writes no segment area, which is
                // the whole point of it being key-free.
                segments: Vec::new(),
                records: rehomed + resumed,
                // A folder has no placement layer.
                placements: None,
                remaining: Some(remaining),
            })
        }
        Ok(crate::db::sync_storage::FolderMaterializeOutcome::Missing) => {
            Err(MaterializeRefusal::FolderTargetMissing {
                folder: display_name.to_string(),
            })
        }
        Ok(crate::db::sync_storage::FolderMaterializeOutcome::NonceMoved) => {
            Err(MaterializeRefusal::RehomeSignature {
                folder: display_name.to_string(),
                refusal: crate::change_signature::IngestRefusal::Invalid(
                    "the target set was re-created while the page was verified; \
                     re-sign under its current nonce"
                        .into(),
                ),
            })
        }
        Ok(crate::db::sync_storage::FolderMaterializeOutcome::NotEmpty { records }) => {
            Err(MaterializeRefusal::FolderNotEmpty {
                folder: display_name.to_string(),
                records,
            })
        }
        Ok(crate::db::sync_storage::FolderMaterializeOutcome::NotFresh { property }) => {
            Err(MaterializeRefusal::FolderNotFresh {
                folder: display_name.to_string(),
                property,
            })
        }
        Err(e) => Err(match e {
            crate::db::sync_storage::StorageQuotaError::Db(inner) => {
                MaterializeRefusal::Integrity(inner)
            }
            other => MaterializeRefusal::Quota(other),
        }),
    }
}

/// How a file already at a destination path relates to the bytes this run means
/// to write there — guard 3's discriminator.
enum Occupant {
    Absent,
    /// A prefix of what we mean to write: an interrupted write of OUR OWN half,
    /// caught wherever the crash left it. `full` when the whole half is there.
    OurWrite {
        full: bool,
    },
    /// Anything else. Not provably ours, and so not ours to delete.
    Foreign,
}

/// Classify one destination path against the half intended for it.
///
/// Reading the file back is affordable exactly here: the caller already holds
/// every pair in memory, because it verifies them all before writing any. An
/// unreadable-but-present file is `Foreign` — refusing on a path we cannot
/// inspect is the safe direction.
fn classify_occupant(path: &std::path::Path, intended: &[u8]) -> Occupant {
    let found = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Occupant::Absent,
        Err(_) => return Occupant::Foreign,
    };
    if found.len() <= intended.len() && intended[..found.len()] == found[..] {
        Occupant::OurWrite {
            full: found.len() == intended.len(),
        }
    } else {
        Occupant::Foreign
    }
}

/// One half of a pair onto disk, **create-new** and durable.
///
/// Not `crate::storage::atomic_write`: that is write-tmp + rename, and a rename
/// *replaces* whatever sits at the destination. This is the one writer of the
/// segment area that had given up the protection every other writer of that
/// directory keeps — `FramedSegment::create` opens `create_new`, and the store's
/// crash-recovery path removes an orphan rather than writing through it, both
/// because a 0-byte `seg-00000040.dat` once permanently 451'd all mail for an
/// actor.
///
/// The trade is deliberate and matches the store's: `create_new` cannot leave a
/// half belonging to someone else destroyed, but a crash mid-write can leave a
/// partial file. That residue is cleared by the caller's own guard-3 classifier
/// — NOT by the store's reclaim, which this comment once claimed and which was
/// never true of this path: that reclaim is keyed on the `.dat`, so the lone
/// `.meta` this function's own write order makes likeliest is invisible to it,
/// and it fires only during an append, which would trip the empty-target rule
/// instead.
fn write_half(
    segment_id: u32,
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<(), MaterializeRefusal> {
    use std::io::Write as _;

    let mut f = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(MaterializeRefusal::SegmentPathOccupied {
                segment_id,
                path: path.display().to_string(),
            });
        }
        Err(e) => {
            return Err(MaterializeRefusal::Integrity(
                anyhow::Error::new(e).context(format!(
                    "create {} for the materialized half",
                    path.display()
                )),
            ));
        }
    };
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .with_context(|| format!("write {}", path.display()))
        .map_err(MaterializeRefusal::Integrity)?;
    // The parent-dir fsync the shared atomic primitive would have done: without
    // it a power loss can lose the directory entry for a file that was itself
    // synced. Unix only — `MoveFileEx`-backed Windows semantics do not need it,
    // and opening a directory as a file fails there.
    #[cfg(unix)]
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::OpenOptions::new()
            .read(true)
            .open(parent)
            .and_then(|d| d.sync_all())
            .with_context(|| format!("fsync the segment area at {}", parent.display()))
            .map_err(MaterializeRefusal::Integrity)?;
    }
    Ok(())
}

/// Both halves against what the source's own listing advertised — each
/// unconditionally: the mirror's refs carry the sidecar hash as a required
/// field, so the mirror is always the sidecar's anchor
/// (`docs/goal/architecture/segment-backup-protocol.md` § *A sidecar in the
/// corpus is anchored, or it is not restored*).
///
/// Nothing weaker stands in for the `.meta` check, and the fallback this comment
/// once named does not exist: `FramedSegment::open` asserts only that the block
/// count equals `record_order.len()` and that `floor_metadata.len()` matches it
/// — two lengths, nothing about content. Two sidecars of the same owner with
/// equal record counts are interchangeable to it, and the convergent seal
/// carries no path or scope binding to tell them apart either.
pub(super) fn verify_pair(dat: &[u8], meta: &[u8], advertised: &SegmentRef) -> anyhow::Result<()> {
    let got = hex::encode(blake3::hash(dat).as_bytes());
    anyhow::ensure!(
        got == advertised.blake3_hex,
        "segment {}: the custody `.dat` opens to bytes hashing {got}, but the source's own \
         mirror advertised {}",
        advertised.segment_id,
        advertised.blake3_hex,
    );
    verify_meta_against(meta, advertised)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ref as the corpus's own `manifest.<kind>` mirror carries it.
    fn advertised(dat: &[u8], meta: &[u8]) -> SegmentRef {
        SegmentRef {
            segment_id: 7,
            blake3_hex: hex::encode(blake3::hash(dat).as_bytes()),
            meta_blake3_hex: hex::encode(blake3::hash(meta).as_bytes()),
            ..Default::default()
        }
    }

    #[test]
    fn an_anchored_pair_that_matches_the_mirror_verifies() {
        let (dat, meta) = (b"the dat".as_slice(), b"the meta".as_slice());
        verify_pair(dat, meta, &advertised(dat, meta)).expect("both halves match");
    }

    #[test]
    fn an_anchored_sidecar_that_contradicts_the_mirror_is_refused() {
        let (dat, meta) = (b"the dat".as_slice(), b"the meta".as_slice());
        let err = verify_pair(dat, b"another segment's sidecar", &advertised(dat, meta))
            .expect_err("the mirror's own hash catches the substitution");
        assert!(format!("{err:#}").contains(".meta"), "{err:#}");
    }
}
