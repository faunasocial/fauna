//! The **nest-held pull-back** — the re-seed ceremony's delivery leg when the
//! surviving copy of a lost nest's corpus is held by **another nest**.
//!
//! Owner: `docs/goal/architecture/segment-backup-protocol.md` § Client-device
//! custodian (pull) → *Restore* → *The nest-held pull-back* (the design, its
//! mechanics and its proof obligation); the ceremony is
//! `docs/goal/behavior/backup-destinations.md` § Third destination kind →
//! *Re-seed*.
//!
//! # What this is, in one sentence
//!
//! The owner's app lists the surviving destination's live custody over its own
//! authenticated connection there, fetches every listed path's manifest and
//! chunks off the destination's open content-addressed routes, and lands them
//! **as-is** on the rebuilt nest over the same two surfaces the custodian leg
//! lands on — so the rebuilt nest ends up holding exactly what the destination
//! holds, and the driver's materialize flips it.
//!
//! # Why as-is, on both planes
//!
//! What a nest destination holds already IS the nest-posture corpus: the
//! source's own coordinator sealed the reserved segment sets under the granted
//! `NestBackupKey` root and mirrored each covered folder's at-rest ciphertext
//! verbatim. Both are exactly what the target's materialize reads, so this leg
//! opens nothing, derives neither `BackupKey` nor `NestBackupKey`, and
//! byte-identity with the destination is the identity function — the same
//! store keys, the same manifest bytes, the same manifest hash. A manifest
//! names its chunks' store keys and its plaintext `total_size` in the clear
//! (only the plaintext chunk hashes are sealed), so the walk needs no key to
//! address what it moves. Every fetched byte is verified against the hash
//! that addressed it before it is pushed — a transport check, since the holder
//! chose the hash. What anchors a row to the OWNER is ruled in
//! `client-device-custodian.md` § *What the pull-back trusts the holder for*
//! and built here (§ *What the holder is trusted for*, below).
//!
//! # What the holder is trusted for
//!
//! The holder cannot author a row (the reserved rails rest under the
//! `NestBackupKey`, the mirror plane under the folders' own keys); it can only
//! replay a genuine older state and omit. So on the reserved rails the leg
//! anchors every row to the owner twice:
//!
//! * **the set's sealed ledger** — opened first, under the owner's key
//!   ([`fauna_client_backup::audit::open_ledger`], the audit's own reader), and
//!   walked BY: the population is the ledger's `live` segments (both halves)
//!   plus the ledger itself; a named segment the holder cannot produce refuses
//!   the set `custody_incomplete` with nothing of it recorded; a listed row the
//!   ledger does not name is reported ([`ReseedReport::unanchored_rows`]) and
//!   neither landed nor recorded;
//! * **this device's generation pin** — [`preflight`] judges each ledger
//!   against it ([`LedgerVerdict`]) before anything is written — a pinned
//!   ledger the holder does not list, a family or the whole set, included, as
//!   [`LedgerVerdict::Withheld`] — the confirm
//!   shows the verdicts ([`fauna_client_backup::reseed::preflight_lines`]), and
//!   the run takes the owner's acceptance ([`NestPullBack::with_acceptance`]):
//!   a reserved set the acceptance does not name is refused
//!   [`SetRefusal::NotAccepted`], one whose ledger moved since is refused
//!   [`SetRefusal::PreflightStale`].
//!
//! The mirror plane (covered folders) is the ruling's declared residual: it
//! lands as the holder lists it, and the confirm says so in one line.
//!
//! # Order and crash-safety
//!
//! The account rails first, a kind at a time (`mail`, `post`, `calendar`,
//! `card`), each in [`fauna_client_backup::recover_leg::segment_set_order`]'s walk — the one
//! [`fauna_client_backup::recover_leg::RecoveryDelivery`] lands in: per family, every segment
//! `.dat` then its `.meta`, then the `manifest.<kind>` mirrors last; then every
//! covered-folder mirror set, path by path in a stable order. Per path: bytes
//! first, custody second. A tear anywhere leaves ordinary, GC-safe custody on
//! the target, and a re-run resumes by content address. Nothing here deletes
//! anything, on either side.
//!
//! # Where it lives, and the two `Send` impls
//!
//! Here, wasm-clean, since 2026-10-18 — so web runs the same leg
//! (`client-device-custodian.md` § Implementation status today → *Nest-held
//! pull-back leg*). It used to live in the native-only `fauna-sync-engine`,
//! which keeps the native composition
//! (`fauna_sync_engine::reseed_pull::pull_back_from_destination`). The body is
//! generic over its transports and carries whatever `Send`-ness they give it,
//! so each host wraps it in its own [`fauna_client_backup::reseed::ReseedDeliveryLeg`]
//! newtype over its concrete connection, delegating to
//! [`NestPullBack::deliver_corpus`] and [`NestPullBack::sign_folder_rehome`]:
//! `fauna_sync_engine::reseed_pull::NativePullBackLeg` natively (the run goes
//! on a spawned task, which needs the `Send` a generic transport cannot
//! promise), `fauna-wasm`'s pull-back door on web — the split the lived-in
//! recovery's leg makes (`fauna_client_backup::recover_leg`).
//!
//! # Honest bounds
//!
//! Only the destination's **live head** moves (`custody.list` is
//! latest-per-path): an owner who wants an older generation rolls it back at
//! the destination first (`fauna.backup.generation.restore`) and pulls after.
//! A row the list cannot address — no plaintext `path` — is refused by name,
//! `custody_unaddressable`, never skipped: skipping it would report a corpus
//! with a hole as delivered.

use std::collections::BTreeMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use fauna_client_backup::recover_leg::{
    BlobPushSink, DestGeneration, parse_manifest_hex, segment_set_order,
};
use fauna_client_backup::reseed::{
    DeliveredCorpus, DeliveredSet, DevicePins, FolderLabel, FolderRehome, LedgerPreflight,
    LedgerVerdict, PullBackPreflight, SetAxis, SetOutcome, SetPreflight, SetRefusal, SetResult,
};
use fauna_client_backup::trust::BackupNestSeam;

use fauna_core::data::{BackupDestination, ContentHash, parse_folder_backup_set_name};
use fauna_core::file_download::{BlobFetcher, FileDownloadKeys};
use fauna_protocol::RpcRequester;
use fauna_protocol::backup::CustodyItem;
use fauna_protocol::segments::{
    BACKED_UP_KINDS, LiveManifestMirror, SegmentFamily, landing_set_name, reserved_backup_set_name,
};

use crate::reseed_leg::{
    RehomeRow, ReseedReport, is_path_hash_hex, rehome_nonce, sign_rehome_rows,
};

/// The refusal a custody row the list cannot address earns — no plaintext
/// `path`, or a covered-folder row whose leaf is not a 64-hex `path_hash`.
pub const CUSTODY_UNADDRESSABLE: &str = "custody_unaddressable";

/// The surviving destination, as the owner's own connection reaches it.
pub struct PullBackDestination<'a> {
    /// `fauna.backup.custody.list`, walked to completion
    /// ([`fauna_client_backup::audit::read_full_custody`]) — the audit loop's
    /// read, which the source nest's death does not touch.
    pub seam: &'a dyn BackupNestSeam,
    /// The destination's open, bearer-less content-addressed routes
    /// (`/api/v1/manifests/{hash}`, `/api/v1/chunks/{hash}`).
    pub bytes: &'a dyn BlobFetcher,
}

/// The rebuilt nest, as the owner's own connection reaches it — the custodian
/// leg's own landing surfaces.
pub struct PullBackTarget<'a, P: BlobPushSink, R: RpcRequester> {
    /// The owner-session bulk-byte plane.
    pub bytes: &'a P,
    /// The client-authed `fauna.sync.changes.record` reserved-set arm.
    pub nest: &'a R,
    /// Hex device id, registered write-capable on the **target**.
    pub device_id: String,
}

/// A covered folder's name as the leg hands it to the driver: the display
/// name, and the address + sealed label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderNaming {
    pub display_name: Option<String>,
    pub label: Option<FolderLabel>,
}

/// Every covered folder's name at `destination_id`, read off the account
/// plane's coverage rows (`fauna.state.backup` — the lost box's own list,
/// which survives with the account): the one place the label lives for a
/// folder a nest destination holds, since destination custody never carries
/// it (`segment-backup-protocol.md` § *Where a restored folder's name comes
/// from*).
///
/// A row that recorded the sealed label but no plaintext name (a set sealed
/// past its name) is opened under `keys` — the owner's own seal, read on the
/// owner's own device, as the custodian's pull opens it; a label `keys` cannot
/// open names nothing, and the driver reports the set `folder_unnamed`.
pub fn folder_names_from_coverage(
    rows: &[BackupDestination],
    destination_id: &str,
    keys: Option<&FileDownloadKeys>,
) -> BTreeMap<String, FolderNaming> {
    rows.iter()
        .filter(|row| row.destination_id == destination_id)
        .filter(|row| parse_folder_backup_set_name(&row.folder_name).is_some())
        .map(|row| {
            let label = row.folder_label.as_ref().map(|l| FolderLabel {
                name_hash: l.name_hash,
                name_sealed: l.name_sealed.clone(),
            });
            let display_name = row
                .folder_display_name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .or_else(|| {
                    let (keys, label) = (keys?, label.as_ref()?);
                    fauna_core::label_custody::render_set_name(
                        keys,
                        Some(&label.name_sealed[..]),
                        "",
                        Some(&label.name_hash[..]),
                    )
                    .text()
                    .map(str::to_string)
                });
            (
                row.folder_name.clone(),
                FolderNaming {
                    display_name,
                    label,
                },
            )
        })
        .collect()
}

/// **The nest-held pull-back leg** — moves a surviving nest destination's
/// live custody onto the rebuilt nest, for the shared driver
/// `fauna_client_backup::reseed::run_reseed` to materialize.
///
/// All borrows, like `fauna_sync_engine::reseed::ReseedDelivery`: the ceremony's host
/// owns where the two connections live.
pub struct NestPullBack<'a, P: BlobPushSink, R: RpcRequester> {
    destination: PullBackDestination<'a>,
    target: PullBackTarget<'a, P, R>,
    /// The backup scope — the owner's own actor.
    scope_id: [u8; 32],
    /// Each covered folder's name, by set ([`folder_names_from_coverage`]).
    names: BTreeMap<String, FolderNaming>,
    /// The host's change signer and set-nonce source — what
    /// [`Self::sign_folder_rehome`] signs the re-homed rows with. Custody
    /// records need none: a reserved set is outside the writer signature by
    /// set class.
    signing: Option<crate::RecordSigning>,
    /// Every sealed folder row this leg delivered, by set — what the re-home
    /// signs, read back off the delivery rather than off the destination a
    /// second time, so the signed rows are exactly the delivered ones.
    delivered_rows: Mutex<BTreeMap<String, Vec<RehomeRow>>>,
    /// The owner's key the reserved sets' ledgers open under — the
    /// `NestBackupKey` the host derives.
    ledger_keys: FileDownloadKeys,
    /// This device's generation pins at the destination ([`preflight`]).
    pins: DevicePins,
    /// The preflight the owner confirmed. `None` — a host whose confirm shows
    /// no lines yet — takes a fresh preflight whole, so the ledger walk still
    /// applies but nothing was shown (§ *What the holder is trusted for*).
    acceptance: Option<PullBackPreflight>,
}

impl<'a, P: BlobPushSink, R: RpcRequester> NestPullBack<'a, P, R>
where
    R::Error: fauna_protocol::RpcErrorClass,
{
    /// `ledger_keys` opens the reserved sets' ledgers: the owner's
    /// `NestBackupKey` (`FileDownloadKeys::owner(NestBackupKey::derive(..))`).
    pub fn new(
        destination: PullBackDestination<'a>,
        target: PullBackTarget<'a, P, R>,
        scope_id: [u8; 32],
        ledger_keys: FileDownloadKeys,
    ) -> Self {
        Self {
            destination,
            target,
            scope_id,
            names: BTreeMap::new(),
            signing: None,
            delivered_rows: Mutex::new(BTreeMap::new()),
            ledger_keys,
            pins: DevicePins::default(),
            acceptance: None,
        }
    }

    /// This device's pins at the destination, read off its audit store
    /// ([`DevicePins::from_store`]). None given: every ledger is unvouched.
    #[must_use]
    pub fn with_pins(mut self, pins: DevicePins) -> Self {
        self.pins = pins;
        self
    }

    /// The preflight whose lines the owner confirmed — the run's acceptance.
    #[must_use]
    pub fn with_acceptance(mut self, accepted: PullBackPreflight) -> Self {
        self.acceptance = Some(accepted);
        self
    }

    /// The preflight over this leg's destination, key and pins ([`preflight`]).
    pub async fn preflight(&self) -> std::result::Result<PullBackPreflight, String> {
        preflight(
            &self.destination,
            &self.ledger_keys,
            self.scope_id,
            &self.pins,
        )
        .await
    }

    /// The covered folders' names, by set ([`folder_names_from_coverage`]). A
    /// folder set with no entry is delivered nameless and reported
    /// `folder_unnamed` by the driver — never restored under a guessed name.
    #[must_use]
    pub fn with_folder_names(mut self, names: BTreeMap<String, FolderNaming>) -> Self {
        self.names = names;
        self
    }

    /// Sign the folder re-homes with the host's signer, under the nonce of
    /// each restored folder's prepared target set.
    #[must_use]
    pub fn with_rehome_signing(mut self, signing: crate::RecordSigning) -> Self {
        self.signing = Some(signing);
        self
    }

    /// Deliver everything the destination holds live for this owner: every
    /// backed-up kind's reserved segment set, then every covered-folder mirror
    /// set. Failures are not swallowed — a one-shot recovery ceremony a human
    /// is watching must never report a part-delivered corpus as delivered.
    ///
    /// `accepted` is the owner's acceptance ([`Self::with_acceptance`]).
    pub async fn run_all(&self, accepted: &PullBackPreflight) -> Result<ReseedReport> {
        let live = fauna_client_backup::audit::read_full_custody(self.destination.seam)
            .await
            .map_err(|e| anyhow::anyhow!("pull-back: the destination's custody list: {e}"))?;

        let mut report = ReseedReport::default();
        for kind in BACKED_UP_KINDS {
            report.absorb(
                self.run_kind(kind, &live, accepted)
                    .await
                    .with_context(|| format!("pull-back for kind {kind}"))?,
            );
        }
        report.absorb(
            self.run_covered_folders(&live)
                .await
                .context("pull-back for the covered-folder mirror plane")?,
        );
        Ok(report)
    }

    /// One kind's reserved segment set, walked BY its sealed ledgers
    /// (§ *What the holder is trusted for*): opened first, checked against the
    /// owner's acceptance, the population read off them, and only then landed
    /// in the shared walk. A refusal lands and records nothing of the set.
    async fn run_kind(
        &self,
        kind: &str,
        live: &[CustodyItem],
        accepted: &PullBackPreflight,
    ) -> Result<ReseedReport> {
        let set = reserved_backup_set_name(kind, &self.scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no backup surface: {kind}"))?;
        // The destination's copy is read under its own set's name; the rebuilt
        // nest is recorded — and the verb named — under the landing name, never
        // the rail's (`reserved-folders.md` § __mail Sync → *The landing set*).
        let landing = landing_set_name(kind, &self.scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no landing set: {kind}"))?;
        let scope_hex = hex::encode(self.scope_id);
        let mut listed: BTreeMap<&str, &CustodyItem> = BTreeMap::new();
        for item in live.iter().filter(|i| i.folder_name == set) {
            listed.entry(addressable(item)?).or_insert(item);
        }
        let mut report = ReseedReport::default();

        let ledgers = open_set_ledgers(
            self.destination.bytes,
            &self.ledger_keys,
            kind,
            &scope_hex,
            &set,
            &listed,
        )
        .await
        .map_err(|e| anyhow::anyhow!("pull-back: {e}"))?;

        // The population is what the ledgers name: both halves of every live
        // segment, and each ledger itself (which lands last, by the walk).
        let mut named: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for opened in &ledgers {
            named.insert(opened.path.clone());
            for seg in &opened.mirror.live {
                named.insert(opened.family.dat_path(&scope_hex, seg.segment_id));
                named.insert(opened.family.meta_path(&scope_hex, seg.segment_id));
            }
        }
        report.unanchored_rows.extend(
            listed
                .keys()
                .filter(|path| !named.contains(**path))
                .map(|path| format!("{set}/{path}")),
        );

        let refuse = |report: &mut ReseedReport, refusal: SetRefusal, detail: String| {
            tracing::warn!(kind, set = %set, code = refusal.code(), %detail, "pull-back: set refused");
            report.refused_sets.push(SetResult {
                set: DeliveredSet {
                    set_name: landing.clone(),
                    axis: SetAxis::Segment,
                    folder_display_name: None,
                    folder_label: None,
                },
                outcome: SetOutcome::Refused { refusal, detail },
            });
        };
        // This device's pinned ledgers the holder does not list — a family
        // or the whole set withheld: judged like a rollback, so the set moves
        // only on the owner's word (anchor (ii)).
        let withheld = withheld_ledgers(&set, &ledgers, &self.pins);
        if ledgers.is_empty() && withheld.is_empty() {
            // No ledger and no pin: nothing anchors any row to the owner, so
            // nothing of the set moves (every row was reported unanchored above).
            return Ok(report);
        }
        let Some(acc) = accepted.set(&set) else {
            refuse(
                &mut report,
                SetRefusal::NotAccepted,
                format!("{set} was not among the sets you confirmed"),
            );
            return Ok(report);
        };
        // The acceptance names each ledger by its hash, and a withheld one by
        // its absence (an empty hash), so a ledger that appears or moves after
        // the confirm reads stale.
        let now: std::collections::BTreeSet<(&str, &str)> = ledgers
            .iter()
            .map(|l| (l.path.as_str(), l.manifest_hash_hex.as_str()))
            .chain(withheld.iter().map(|l| (l.path.as_str(), "")))
            .collect();
        let then: std::collections::BTreeSet<(&str, &str)> = acc
            .ledgers
            .iter()
            .map(|l| (l.path.as_str(), l.manifest_hash_hex.as_str()))
            .collect();
        if now != then {
            refuse(
                &mut report,
                SetRefusal::PreflightStale,
                format!("{set}'s ledger changed after you confirmed the restore"),
            );
            return Ok(report);
        }
        if ledgers.is_empty() {
            // The owner accepted a set the holder withholds whole: nothing
            // anchors a row, so nothing of it moves.
            return Ok(report);
        }
        let missing: Vec<&String> = named
            .iter()
            .filter(|path| !listed.contains_key(path.as_str()))
            .collect();
        if let Some(first) = missing.first() {
            refuse(
                &mut report,
                SetRefusal::CustodyIncomplete,
                format!(
                    "the copy of {set} lacks {} row(s) its own ledger names, first {first}",
                    missing.len(),
                ),
            );
            return Ok(report);
        }

        let mut generations = Vec::new();
        for path in &named {
            let item = listed[path.as_str()];
            let manifest_hash = parse_manifest_hex(&item.manifest_hash).ok_or_else(|| {
                anyhow::anyhow!("pull-back: {set}/{path} names a malformed manifest hash")
            })?;
            generations.push(DestGeneration {
                path: path.clone(),
                manifest_hash,
                order: (true, item.updated_at),
            });
        }
        let ordered = segment_set_order(kind, &scope_hex, &generations);
        for g in &ordered {
            let pulled = self
                .land_bytes(&mut report, g.manifest_hash, &g.path)
                .await
                .with_context(|| format!("pull-back: landing {}'s bytes", g.path))?;
            self.record(&landing, &g.path, &pulled, None)
                .await
                .with_context(|| format!("pull-back: recording {} on the target", g.path))?;
            report.delivered_paths.push(g.path.clone());
            report.plaintext_bytes = report.plaintext_bytes.saturating_add(pulled.total_size);
        }

        if !report.delivered_paths.is_empty() {
            report.delivered_sets.push(DeliveredSet {
                set_name: landing.clone(),
                axis: SetAxis::Segment,
                folder_display_name: None,
                folder_label: None,
            });
        }
        tracing::info!(
            kind,
            set = %set,
            delivered = report.delivered_paths.len(),
            unanchored = report.unanchored_rows.len(),
            "pull-back: kind complete",
        );
        Ok(report)
    }

    /// Every covered-folder mirror set the destination holds, path by path.
    ///
    /// Which folders were covered is read off custody itself — the source that
    /// knew is gone — admitted by [`parse_folder_backup_set_name`], the same
    /// door the target's provisioning applies, so every set this leg names is
    /// one the target will mint.
    async fn run_covered_folders(&self, live: &[CustodyItem]) -> Result<ReseedReport> {
        let mut rows: Vec<(&str, &str, &CustodyItem)> = Vec::new();
        for item in live {
            if parse_folder_backup_set_name(&item.folder_name).is_none() {
                continue;
            }
            let leaf = addressable(item)?;
            if !is_path_hash_hex(leaf) {
                anyhow::bail!(
                    "{CUSTODY_UNADDRESSABLE}: {}/{leaf} is not a path-hash leaf the target \
                     could re-home",
                    item.folder_name
                );
            }
            rows.push((item.folder_name.as_str(), leaf, item));
        }
        rows.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));

        let mut report = ReseedReport::default();
        let mut signable: BTreeMap<String, Vec<RehomeRow>> = BTreeMap::new();
        for (set, leaf, item) in rows {
            let manifest_hash = parse_manifest_hex(&item.manifest_hash).ok_or_else(|| {
                anyhow::anyhow!("pull-back: {set}/{leaf} names a malformed manifest hash")
            })?;
            let pulled = self
                .land_bytes(&mut report, manifest_hash, leaf)
                .await
                .with_context(|| format!("pull-back: landing {set}/{leaf}'s bytes"))?;
            let path_sealed = item.path_sealed.as_ref().map(|b| b.as_slice());
            self.record(set, leaf, &pulled, path_sealed)
                .await
                .with_context(|| format!("pull-back: recording folder custody for {set}/{leaf}"))?;

            let held = format!("{set}/{leaf}");
            match path_sealed {
                Some(path_sealed) => signable
                    .entry(set.to_string())
                    .or_default()
                    .push(RehomeRow {
                        path_hash: fauna_core::hex32::decode(leaf).map_err(|_| {
                            anyhow::anyhow!("pull-back: {held} has a malformed leaf")
                        })?,
                        manifest_hash: manifest_hash.digest(),
                        total_size: pulled.total_size,
                        path_sealed: path_sealed.to_vec(),
                    }),
                None => {
                    tracing::warn!(
                        set,
                        path = leaf,
                        "pull-back: the destination holds no sealed name for this mirrored \
                         path — its bytes and custody landed, but the folder cannot be \
                         materialized until it does"
                    );
                    report.folder_paths_without_seal.push(held.clone());
                }
            }
            report.delivered_paths.push(held);
            report.plaintext_bytes = report.plaintext_bytes.saturating_add(pulled.total_size);
            if report.delivered_sets.last().map(|s| s.set_name.as_str()) != Some(set) {
                let naming = self.names.get(set).cloned().unwrap_or_default();
                report.delivered_sets.push(DeliveredSet {
                    set_name: set.to_string(),
                    axis: SetAxis::Folder,
                    folder_display_name: naming.display_name,
                    folder_label: naming.label,
                });
            }
        }
        *self.delivered_rows.lock().expect("delivered rows") = signable;
        Ok(report)
    }

    /// Bytes first: one path streamed chunk by chunk from the destination
    /// onto the target ([`pull_as_is`]), counted into `report`.
    async fn land_bytes(
        &self,
        report: &mut ReseedReport,
        manifest_hash: ContentHash,
        path: &str,
    ) -> Result<PulledPath> {
        let pulled = pull_as_is(
            self.destination.bytes,
            self.target.bytes,
            manifest_hash,
            path,
        )
        .await?;
        report.absorb(ReseedReport {
            chunks_uploaded: pulled.chunks_uploaded,
            chunks_deduped: pulled.chunks_deduped,
            manifests_uploaded: 1,
            ..ReseedReport::default()
        });
        Ok(pulled)
    }

    /// Custody for one delivered path on the target, through the self-healing
    /// record entry ([`crate::SyncClient::changes_record`]): this leg runs on
    /// web too, where the device driving it may never have registered on the
    /// rebuilt nest, and that entry registers it on the nest's
    /// `device_unregistered` refusal and retries once, as the lived-in
    /// recovery's records do. A reserved set's row carries no sealed path (its
    /// paths are machine-authored routing keys); a mirrored folder path's
    /// carries the sealed name the destination held, with the leaf as the path
    /// (what the source's own folder pass recorded) and no `folder_id` — the
    /// client-authed door takes the `__folder/<nest>/<id>` set name as declared.
    async fn record(
        &self,
        set: &str,
        path: &str,
        pulled: &PulledPath,
        path_sealed: Option<&[u8]>,
    ) -> Result<()> {
        crate::SyncClient::new(self.target.nest)
            .changes_record(
                set,
                self.target.device_id.as_str(),
                path,
                Some(hex::encode(pulled.manifest_hash.digest())),
                // The nest derives the custody charge from the bytes it holds;
                // declared as the plaintext size so a delivered row reads as one
                // the source nest's own coordinator wrote.
                pulled.total_size as i64,
                "create",
                None,
                None,
                path_sealed.map(<[u8]>::to_vec),
                None,
                None,
                None,
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Phase 2 as the driver sees it: one [`Self::run_all`], projected onto
    /// the terms the owner is shown.
    ///
    /// Generic over any transport; each host's `ReseedDeliveryLeg` newtype
    /// delegates here (§ *Where it lives, and the two `Send` impls*).
    pub async fn deliver_corpus(&self) -> std::result::Result<DeliveredCorpus, String> {
        let fresh;
        let accepted = match &self.acceptance {
            Some(accepted) => accepted,
            None => {
                fresh = self.preflight().await?;
                &fresh
            }
        };
        let report = self.run_all(accepted).await.map_err(|e| format!("{e:#}"))?;
        Ok(DeliveredCorpus {
            sets: report.delivered_sets,
            sidecarless_segments: report.sidecarless_segments,
            folder_paths_without_seal: report.folder_paths_without_seal,
            plaintext_bytes: report.plaintext_bytes,
            refused: report.refused_sets,
            unanchored_rows: report.unanchored_rows,
        })
    }

    /// The owner's signature over every sealed row this leg delivered into
    /// `set`, under the nonce of the live set the folder re-homes into
    /// (`writer-signed-change-records.md` ruling (7)(a)(ii)) — the same
    /// statements the custodian leg signs, over the rows exactly as they
    /// landed on the target.
    pub async fn sign_folder_rehome(&self, set: &DeliveredSet) -> FolderRehome {
        let (signing, nonce) = match rehome_nonce(self.signing.as_ref(), set).await {
            Ok(resolved) => resolved,
            Err(unsigned) => return unsigned,
        };
        let rows = self
            .delivered_rows
            .lock()
            .expect("delivered rows")
            .get(&set.set_name)
            .cloned()
            .unwrap_or_default();
        sign_rehome_rows(signing, nonce, &rows)
    }
}

/// One reserved-set ledger the leg opened off the holder.
struct OpenedLedger {
    family: SegmentFamily,
    /// The mirror path inside the set (`<scope>/manifest.<kind>`, …).
    path: String,
    /// The custody row's manifest hash it was read under.
    manifest_hash_hex: String,
    mirror: LiveManifestMirror,
}

/// Every family ledger of `kind` the holder lists in `set`, opened under the
/// owner's key, in family order. A listed ledger that does not open is an
/// error — never a set walked without its anchor.
async fn open_set_ledgers(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    kind: &str,
    scope_hex: &str,
    set: &str,
    listed: &BTreeMap<&str, &CustodyItem>,
) -> std::result::Result<Vec<OpenedLedger>, String> {
    let mut out = Vec::new();
    for family in SegmentFamily::PASS_ORDER {
        if family.serve_kind(kind).is_none() {
            continue;
        }
        let path = family.mirror_path(scope_hex, kind);
        let Some(item) = listed.get(path.as_str()) else {
            continue;
        };
        let mirror = fauna_client_backup::audit::open_ledger(fetcher, keys, item)
            .await
            .map_err(|e| format!("{set}'s ledger does not open: {e}"))?;
        out.push(OpenedLedger {
            family,
            path,
            manifest_hash_hex: item.manifest_hash.to_ascii_lowercase(),
            mirror,
        });
    }
    Ok(out)
}

/// This device's pins on `set` for which the holder lists no ledger, judged
/// [`LedgerVerdict::Withheld`], in pin-key order. A pin is read back to its set
/// structurally ([`fauna_client_backup::audit::ledger_key_set`]).
fn withheld_ledgers(set: &str, opened: &[OpenedLedger], pins: &DevicePins) -> Vec<LedgerPreflight> {
    pins.pins
        .iter()
        .filter(|(key, _)| fauna_client_backup::audit::ledger_key_set(key) == Some(set))
        .filter_map(|(key, pin)| {
            let path = key.get(set.len() + 1..)?;
            (!opened.iter().any(|l| l.path == path)).then(|| LedgerPreflight {
                path: path.to_string(),
                manifest_hash_hex: String::new(),
                verdict: LedgerVerdict::withheld(pin, pins.last_passed_at),
            })
        })
        .collect()
}

/// **The pull-back's preflight** — the read the confirm is built from
/// (`client-device-custodian.md` § *What the pull-back trusts the holder
/// for*, the confirm): the holder's custody list, every reserved set's ledgers
/// opened under `keys` and judged against this device's `pins`
/// ([`LedgerVerdict::judge`]), and the covered-folder sets by name. Writes
/// nothing, anywhere. `Err` — the holder unreachable, a ledger that does not
/// open — is a failed gesture, never a verdict.
pub async fn preflight(
    destination: &PullBackDestination<'_>,
    keys: &FileDownloadKeys,
    scope_id: [u8; 32],
    pins: &DevicePins,
) -> std::result::Result<PullBackPreflight, String> {
    let live = fauna_client_backup::audit::read_full_custody(destination.seam)
        .await
        .map_err(|e| format!("the destination's custody list: {e}"))?;
    let scope_hex = hex::encode(scope_id);
    let mut out = PullBackPreflight::default();
    for kind in BACKED_UP_KINDS {
        let Some(set) = reserved_backup_set_name(kind, &scope_id) else {
            continue;
        };
        let listed: BTreeMap<&str, &CustodyItem> = live
            .iter()
            .filter(|i| i.folder_name == set)
            .filter_map(|i| i.path.as_deref().map(|p| (p, i)))
            .collect();
        let ledgers =
            open_set_ledgers(destination.bytes, keys, kind, &scope_hex, &set, &listed).await?;
        let withheld = withheld_ledgers(&set, &ledgers, pins);
        if ledgers.is_empty() && withheld.is_empty() {
            continue;
        }
        out.sets.push(SetPreflight {
            ledgers: ledgers
                .into_iter()
                .map(|l| LedgerPreflight {
                    verdict: LedgerVerdict::judge(
                        l.mirror.next_segment_id_seen,
                        pins.pins
                            .get(&fauna_client_backup::audit::ledger_key(&set, &l.path)),
                        pins.last_passed_at,
                    ),
                    path: l.path,
                    manifest_hash_hex: l.manifest_hash_hex,
                })
                .chain(withheld)
                .collect(),
            set_name: set,
        });
    }
    let folders: std::collections::BTreeSet<&str> = live
        .iter()
        .filter(|i| parse_folder_backup_set_name(&i.folder_name).is_some())
        .map(|i| i.folder_name.as_str())
        .collect();
    out.folder_sets = folders.into_iter().map(str::to_string).collect();
    Ok(out)
}

/// The plaintext `path` a custody row is addressed by, or the
/// [`CUSTODY_UNADDRESSABLE`] refusal naming the row.
fn addressable(item: &CustodyItem) -> Result<&str> {
    item.path.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "{CUSTODY_UNADDRESSABLE}: a row in {} (path hash {}) carries no path to land it under",
            item.folder_name,
            item.path_hash
        )
    })
}

/// One custody path as [`pull_as_is`] landed it on the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulledPath {
    /// The manifest's hash — the destination's, unchanged.
    pub manifest_hash: ContentHash,
    /// The manifest's plaintext `total_size`, which custody declares.
    pub total_size: u64,
    /// Chunk bodies actually uploaded (post-dedup).
    pub chunks_uploaded: usize,
    /// Chunk bodies the target already held.
    pub chunks_deduped: usize,
}

/// Move one custody path off the destination's open routes onto the target
/// **as it rests there**, a chunk at a time: the manifest by its hash, then
/// each chunk the target lacks fetched by its store key, verified against it
/// and pushed before the next is fetched, then the manifest last — so the
/// target never holds a manifest naming bytes it does not have, and the app
/// never holds more than one chunk body of the path at once
/// (`message-segment-store.md`'s streaming rule; a multi-GB covered-folder
/// file passes through a constant buffer). A store key the manifest repeats
/// is pushed once.
///
/// A byte that fails the hash that addressed it is refused before it lands;
/// chunks of the path already pushed stay, ordinary content-addressed bytes no
/// manifest names yet, and a re-run resumes by content address.
pub async fn pull_as_is<P: BlobPushSink + ?Sized>(
    fetcher: &dyn BlobFetcher,
    sink: &P,
    manifest_hash: ContentHash,
    path: &str,
) -> Result<PulledPath> {
    let manifest_hex = hex::encode(manifest_hash.digest());
    let manifest_bytes = fetcher
        .fetch_manifest(&manifest_hash)
        .await
        .with_context(|| format!("pull-back: fetching {path}'s manifest {manifest_hex}"))?;
    anyhow::ensure!(
        ContentHash::of_raw(&manifest_bytes) == manifest_hash,
        "pull-back: manifest {manifest_hex} for {path} failed hash verification"
    );
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).map_err(|e| {
            anyhow::anyhow!("pull-back: manifest {manifest_hex} does not decode: {e}")
        })?;
    let keys = manifest.store_keys();
    let mut missing: std::collections::HashSet<ContentHash> = sink
        .missing_chunks(&keys)
        .await
        .with_context(|| format!("pull-back: {path}'s chunk dedup check"))?
        .into_iter()
        .collect();
    let mut pulled = PulledPath {
        manifest_hash,
        total_size: manifest.total_size,
        chunks_uploaded: 0,
        chunks_deduped: keys.len().saturating_sub(missing.len()),
    };
    for key in &keys {
        // Taking the key out of the set pushes a repeated store key once.
        if !missing.remove(key) {
            continue;
        }
        let key_hex = hex::encode(key.digest());
        let mut bodies = fetcher
            .fetch_chunks(std::slice::from_ref(key), path)
            .await
            .with_context(|| format!("pull-back: fetching {path}'s chunk {key_hex}"))?;
        anyhow::ensure!(
            bodies.len() == 1,
            "pull-back: the destination returned {} bodies for {path}'s chunk {key_hex}",
            bodies.len()
        );
        let body = bodies.pop().expect("one body");
        anyhow::ensure!(
            ContentHash::of_raw(&body) == *key,
            "pull-back: chunk {key_hex} of {path} failed hash verification"
        );
        sink.put_chunk(key, &body)
            .await
            .with_context(|| format!("pull-back: uploading {path}'s chunk {key_hex}"))?;
        pulled.chunks_uploaded += 1;
    }
    sink.put_manifest(&manifest_bytes)
        .await
        .with_context(|| format!("pull-back: uploading {path}'s manifest"))?;
    Ok(pulled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::ClassifiedError;
    use fauna_core::blob_seal::{SealedBlob, seal_blob};
    use fauna_core::crypto::NestBackupKey;
    use fauna_protocol::backup::{CustodyListReply, CustodyListRequest};
    use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};
    use fauna_protocol::sync_writer_sig::SignedChange;
    use std::collections::HashMap;
    use std::sync::Arc;

    const SCOPE: [u8; 32] = [0xA7; 32];
    const SOURCE_NEST: [u8; 32] = [0x5C; 32];
    /// The rebuilt nest's write device, hex-spelled as the record arm takes it.
    fn device() -> String {
        hex::encode([0x1D; 32])
    }

    fn body(tag: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| tag ^ (i % 251) as u8).collect()
    }

    fn scope_hex() -> String {
        hex::encode(SCOPE)
    }

    fn folder_set(id: i64) -> String {
        fauna_core::data::folder_backup_set_name(&SOURCE_NEST, id)
    }

    /// One ordered log every fake appends to — the order across the two
    /// connections is the contract.
    type Log = Arc<Mutex<Vec<String>>>;

    /// The surviving destination: custody rows plus the bytes its open routes
    /// serve, keyed by hash. `tamper` serves one chunk with a byte flipped.
    #[derive(Default)]
    struct Destination {
        items: Mutex<Vec<CustodyItem>>,
        blobs: Mutex<HashMap<ContentHash, Vec<u8>>>,
        tamper: Mutex<Option<ContentHash>>,
    }

    impl Destination {
        /// Hold `sealed` at `path` in `set`, as the source's coordinator left it.
        fn hold(
            &self,
            set: &str,
            path: Option<&str>,
            sealed: &SealedBlob,
            path_sealed: Option<&[u8]>,
        ) {
            let mut blobs = self.blobs.lock().unwrap();
            blobs.insert(sealed.manifest_hash, sealed.manifest_bytes.clone());
            for (key, chunk) in &sealed.chunks {
                blobs.insert(*key, chunk.clone());
            }
            let mut items = self.items.lock().unwrap();
            items
                .retain(|i| !(i.folder_name == set && i.path.as_deref() == path && path.is_some()));
            items.push(CustodyItem {
                folder_name: set.to_string(),
                path: path.map(str::to_string),
                path_hash: "00".repeat(32),
                manifest_hash: hex::encode(sealed.manifest_hash.digest()),
                path_sealed: path_sealed.map(|b| fauna_protocol::ByteBuf::from(b.to_vec())),
                ..Default::default()
            });
        }
    }

    /// The owner's connection to the destination: `custody.list` alone.
    struct DestLink(Arc<Destination>);

    impl RpcRequester for DestLink {
        type Error = anyhow::Error;

        async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            anyhow::ensure!(
                kind == "fauna.backup.custody.list",
                "the pull-back asked the destination for {kind}"
            );
            let _: CustodyListRequest =
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&payload)?)?;
            let reply = CustodyListReply {
                items: self.0.items.lock().unwrap().clone(),
                ..Default::default()
            };
            Ok(fauna_protocol::decode_strict(
                &fauna_protocol::encode_canonical(&reply)?,
            )?)
        }
    }

    fauna_client_backup::impl_backup_nest_seam!(struct DestSeam<DestLink>);

    struct DestBytes(Arc<Destination>);

    #[async_trait::async_trait]
    impl BlobFetcher for DestBytes {
        async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
            self.0
                .blobs
                .lock()
                .unwrap()
                .get(hash)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404"))
        }

        async fn fetch_chunks(&self, keys: &[ContentHash], _path: &str) -> Result<Vec<Vec<u8>>> {
            let blobs = self.0.blobs.lock().unwrap();
            let tamper = *self.0.tamper.lock().unwrap();
            keys.iter()
                .map(|k| {
                    let mut body = blobs
                        .get(k)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("404"))?;
                    if Some(*k) == tamper {
                        body[0] ^= 1;
                    }
                    Ok(body)
                })
                .collect()
        }
    }

    /// The rebuilt nest: its byte plane and its record arm, logging in order.
    struct Target {
        log: Log,
        chunks: Mutex<HashMap<ContentHash, Vec<u8>>>,
        records: Mutex<Vec<SyncChangeRecordRequest>>,
        /// The device has never registered here (a browser on a rebuilt
        /// nest): a record is refused `device_unregistered` until it does.
        unregistered: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl BlobPushSink for Target {
        async fn missing_chunks(&self, keys: &[ContentHash]) -> Result<Vec<ContentHash>> {
            let held = self.chunks.lock().unwrap();
            Ok(keys
                .iter()
                .copied()
                .filter(|k| !held.contains_key(k))
                .collect())
        }

        async fn put_chunk(&self, key: &ContentHash, body: &[u8]) -> Result<()> {
            self.chunks.lock().unwrap().insert(*key, body.to_vec());
            Ok(())
        }

        async fn put_manifest(&self, bytes: &[u8]) -> Result<()> {
            let hash = ContentHash::of_raw(bytes);
            self.log
                .lock()
                .unwrap()
                .push(format!("bytes {}", hex::encode(&hash.digest()[..4])));
            Ok(())
        }
    }

    impl RpcRequester for Target {
        type Error = ClassifiedError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> std::result::Result<Reply, ClassifiedError>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use std::sync::atomic::Ordering;
            if kind == "fauna.sync.register" {
                self.unregistered.store(false, Ordering::SeqCst);
                self.log.lock().unwrap().push("register".into());
                let reply = fauna_protocol::sync::SyncRegisterReply {
                    device_id: device(),
                    extra: Default::default(),
                };
                let bytes = fauna_protocol::encode_canonical(&reply)
                    .map_err(|e| ClassifiedError::Transport(e.to_string()))?;
                return fauna_protocol::decode_strict(&bytes)
                    .map_err(|e| ClassifiedError::Transport(e.to_string()));
            }
            if kind == "fauna.sync.changes.record" && self.unregistered.load(Ordering::SeqCst) {
                return Err(ClassifiedError::Rejected(fauna_protocol::RpcError::new(
                    "fauna.sync.device_unregistered",
                    "sync.device_unregistered",
                )));
            }
            self.answer(kind, payload)
                .map_err(|e| ClassifiedError::Transport(e.to_string()))
        }
    }

    impl Target {
        fn answer<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            anyhow::ensure!(
                kind == "fauna.sync.changes.record",
                "the pull-back opened {kind} on the target"
            );
            let req: SyncChangeRecordRequest =
                fauna_protocol::decode_strict(&fauna_protocol::encode_canonical(&payload)?)?;
            self.log
                .lock()
                .unwrap()
                .push(format!("record {} {}", req.folder, req.path));
            self.records.lock().unwrap().push(req);
            Ok(fauna_protocol::decode_strict(
                &fauna_protocol::encode_canonical(&SyncChangeRecordReply {
                    seq: 0,
                    extra: Default::default(),
                })?,
            )?)
        }
    }

    struct Rig {
        dest: Arc<Destination>,
        seam: DestSeam,
        bytes: DestBytes,
        target: Target,
        log: Log,
    }

    impl Rig {
        fn new() -> Self {
            let dest = Arc::new(Destination::default());
            let log: Log = Arc::new(Mutex::new(Vec::new()));
            Self {
                seam: DestSeam {
                    client: fauna_client_backup::BackupClient::new(DestLink(dest.clone())),
                },
                bytes: DestBytes(dest.clone()),
                target: Target {
                    log: log.clone(),
                    chunks: Mutex::new(HashMap::new()),
                    records: Mutex::new(Vec::new()),
                    unregistered: std::sync::atomic::AtomicBool::new(false),
                },
                dest,
                log,
            }
        }

        fn leg(&self) -> NestPullBack<'_, Target, Target> {
            NestPullBack::new(
                PullBackDestination {
                    seam: &self.seam,
                    bytes: &self.bytes,
                },
                PullBackTarget {
                    bytes: &self.target,
                    nest: &self.target,
                    device_id: device(),
                },
                SCOPE,
                FileDownloadKeys::owner(NestBackupKey::from_bytes([9; 32])),
            )
        }

        /// The source's sealed ledger for `family`, naming `ids` live at
        /// generation `counter`, held at its mirror path.
        fn hold_ledger(&self, family: SegmentFamily, ids: &[u32], counter: u32) -> SealedBlob {
            let mirror = LiveManifestMirror {
                next_segment_id_seen: counter,
                live: ids
                    .iter()
                    .map(|&segment_id| fauna_protocol::segments::SegmentRef {
                        segment_id,
                        ..Default::default()
                    })
                    .collect(),
                extra: Default::default(),
            };
            let path = family.mirror_path(&scope_hex(), "mail");
            self.hold_segment(&path, &mirror.to_bytes().unwrap())
        }

        /// Both halves of content segment `id`.
        fn hold_pair(&self, id: u32) {
            let (dat, meta) = content(id);
            self.hold_segment(&dat, &body(id as u8, 100));
            self.hold_segment(&meta, &body(id as u8 ^ 0x80, 30));
        }

        /// Preflight with no pins, then run accepting all of it — the shape
        /// every test of the walk itself takes.
        async fn run(&self, leg: &NestPullBack<'_, Target, Target>) -> ReseedReport {
            let accepted = leg.preflight().await.unwrap();
            leg.run_all(&accepted).await.unwrap()
        }

        /// The source coordinator's seal of a segment-plane path: the granted
        /// `NestBackupKey` root.
        fn hold_segment(&self, path: &str, plain: &[u8]) -> SealedBlob {
            let root = NestBackupKey::from_bytes([9; 32]).convergent_chunk_root();
            let sealed = seal_blob(plain, Some((root, None))).unwrap();
            self.dest.hold("__mail", Some(path), &sealed, None);
            sealed
        }

        /// A covered folder's at-rest ciphertext, mirrored as-is — under a
        /// root neither the leg nor the target holds.
        fn hold_folder(
            &self,
            set: &str,
            leaf: &str,
            plain: &[u8],
            name: Option<&[u8]>,
        ) -> SealedBlob {
            let sealed = seal_blob(plain, Some(([0x5E; 32], None))).unwrap();
            self.dest.hold(set, Some(leaf), &sealed, name);
            sealed
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    fn short(sealed: &SealedBlob) -> String {
        format!("bytes {}", hex::encode(&sealed.manifest_hash.digest()[..4]))
    }

    fn content(id: u32) -> (String, String) {
        let f = SegmentFamily::Content;
        (f.dat_path(&scope_hex(), id), f.meta_path(&scope_hex(), id))
    }

    /// **Both planes land as-is, in the shared walk, bytes before custody.**
    /// Every segment `.dat` before its `.meta`, the journal family after the
    /// content family, the mirrors last; then the covered folder. What lands
    /// is the destination's own bytes — the same manifest hashes, store keys
    /// and bodies — and every custody row names its set and path as the
    /// destination held them.
    #[tokio::test]
    async fn both_planes_land_as_is_in_the_shared_walk_bytes_before_custody() {
        let rig = Rig::new();
        let (dat2, meta2) = content(2);
        let (dat1, meta1) = content(1);
        let mirror = SegmentFamily::Content.mirror_path(&scope_hex(), "mail");
        let jdat = SegmentFamily::Placement.dat_path(&scope_hex(), 1);
        let jmeta = SegmentFamily::Placement.meta_path(&scope_hex(), 1);
        let jmirror = SegmentFamily::Placement.mirror_path(&scope_hex(), "mail");
        // Held in an order the walk must not follow.
        let s_mirror = rig.hold_ledger(SegmentFamily::Content, &[1, 2], 3);
        let s_jmirror = rig.hold_ledger(SegmentFamily::Placement, &[1], 2);
        let s_meta2 = rig.hold_segment(&meta2, &body(0x12, 100));
        let s_jdat = rig.hold_segment(&jdat, &body(0x13, 900));
        let s_dat2 = rig.hold_segment(&dat2, &body(0x14, 9_000));
        let s_dat1 = rig.hold_segment(&dat1, &body(0x15, 7_000));
        let s_meta1 = rig.hold_segment(&meta1, &body(0x16, 120));
        let s_jmeta = rig.hold_segment(&jmeta, &body(0x17, 80));
        let set = folder_set(7);
        let leaf = hex::encode([0x22; 32]);
        let s_file = rig.hold_folder(&set, &leaf, &body(0x18, 4_000), Some(b"sealed-name"));

        let names: BTreeMap<String, FolderNaming> = [(
            set.clone(),
            FolderNaming {
                display_name: Some("Photos".into()),
                label: None,
            },
        )]
        .into();
        let leg = rig.leg().with_folder_names(names);
        let report = rig.run(&leg).await;

        let step = |sealed: &SealedBlob, set: &str, path: &str| {
            vec![short(sealed), format!("record {set} {path}")]
        };
        let expected: Vec<String> = [
            step(&s_dat1, "__landing/mail", &dat1),
            step(&s_meta1, "__landing/mail", &meta1),
            step(&s_dat2, "__landing/mail", &dat2),
            step(&s_meta2, "__landing/mail", &meta2),
            step(&s_jdat, "__landing/mail", &jdat),
            step(&s_jmeta, "__landing/mail", &jmeta),
            step(&s_mirror, "__landing/mail", &mirror),
            step(&s_jmirror, "__landing/mail", &jmirror),
            step(&s_file, &set, &leaf),
        ]
        .concat();
        assert_eq!(rig.log(), expected);

        // Byte identity: every chunk the target holds is the destination's,
        // under the destination's own store key.
        let held = rig.dest.blobs.lock().unwrap().clone();
        for (key, chunk) in rig.target.chunks.lock().unwrap().iter() {
            assert_eq!(held.get(key), Some(chunk), "a chunk landed re-sealed");
        }
        // The folder row carries the destination's sealed name; no segment
        // row carries one.
        let records = rig.target.records.lock().unwrap().clone();
        let file = records.iter().find(|r| r.path == leaf).unwrap();
        assert_eq!(
            file.path_sealed.as_ref().map(|b| b.as_slice()),
            Some(&b"sealed-name"[..])
        );
        assert!(
            records
                .iter()
                .filter(|r| r.path != leaf)
                .all(|r| r.path_sealed.is_none())
        );

        assert_eq!(
            report
                .delivered_sets
                .iter()
                .map(|s| (s.set_name.clone(), s.axis, s.folder_display_name.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("__landing/mail".to_string(), SetAxis::Segment, None),
                (set, SetAxis::Folder, Some("Photos".to_string())),
            ]
        );
        assert!(report.sidecarless_segments.is_empty());
        assert!(report.folder_paths_without_seal.is_empty());
    }

    /// A landing set the destination happens to hold (a delivered copy of the
    /// owner's corpus, `reserved-folders.md` § __mail Sync → *The landing set*)
    /// is not the destination's copy: the pull-back reads `__mail` alone, by
    /// shape, and lands what it read under the landing name.
    #[tokio::test]
    async fn the_pull_back_reads_the_copy_and_ignores_a_landing_set() {
        let rig = Rig::new();
        let (dat1, meta1) = content(1);
        rig.hold_pair(1);
        rig.hold_ledger(SegmentFamily::Content, &[1], 2);
        let mirror = SegmentFamily::Content.mirror_path(&scope_hex(), "mail");
        let root = NestBackupKey::from_bytes([9; 32]).convergent_chunk_root();
        let stray = seal_blob(&body(2, 100), Some((root, None))).unwrap();
        let (dat2, _) = content(2);
        rig.dest
            .hold("__landing/mail", Some(dat2.as_str()), &stray, None);

        let report = rig.run(&rig.leg()).await;

        assert_eq!(report.delivered_paths, vec![dat1.clone(), meta1, mirror]);
        let records = rig.target.records.lock().unwrap().clone();
        assert_eq!(records.len(), 3);
        assert!(records.iter().all(|r| r.folder == "__landing/mail"));
        assert_eq!(records[0].path, dat1);
    }

    /// A row the list carries no path for is refused by name — never skipped,
    /// which would report a corpus with a hole as delivered.
    #[tokio::test]
    async fn a_row_with_no_path_is_refused_by_name() {
        let rig = Rig::new();
        let (dat1, _) = content(1);
        rig.hold_segment(&dat1, &body(1, 100));
        let root = NestBackupKey::from_bytes([9; 32]).convergent_chunk_root();
        let orphan = seal_blob(&body(2, 100), Some((root, None))).unwrap();
        rig.dest.hold("__mail", None, &orphan, None);

        let err = rig
            .leg()
            .run_all(&PullBackPreflight::default())
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains(CUSTODY_UNADDRESSABLE),
            "{err:#}"
        );
        assert!(
            rig.target.records.lock().unwrap().is_empty(),
            "nothing is recorded before the set is known addressable"
        );
    }

    /// The device driving the restore has never registered on the rebuilt nest
    /// (a browser, which runs no folder sync): the first record registers it
    /// write-capable and lands, and every later record lands as is — the
    /// self-healing entry, never a refusal that strands the restore.
    #[tokio::test]
    async fn an_unregistered_device_is_registered_once_and_its_records_land() {
        let rig = Rig::new();
        rig.hold_pair(1);
        rig.hold_ledger(SegmentFamily::Content, &[1], 2);
        rig.target
            .unregistered
            .store(true, std::sync::atomic::Ordering::SeqCst);

        rig.run(&rig.leg()).await;

        let log = rig.log();
        assert_eq!(
            log.iter().filter(|l| *l == "register").count(),
            1,
            "{log:?}"
        );
        assert_eq!(rig.target.records.lock().unwrap().len(), 3, "{log:?}");
        assert!(
            rig.target
                .records
                .lock()
                .unwrap()
                .iter()
                .all(|r| r.device_id == device()),
            "every record names the device that registered"
        );
    }

    fn mail_ledger_key() -> String {
        fauna_client_backup::audit::ledger_key(
            "__mail",
            &SegmentFamily::Content.mirror_path(&scope_hex(), "mail"),
        )
    }

    fn pinned_at(generation: u32) -> DevicePins {
        DevicePins {
            pins: [(
                mail_ledger_key(),
                fauna_client_backup::audit::LedgerPin {
                    generation,
                    seen_live_at: Some(1_700_000_000),
                },
            )]
            .into(),
            last_passed_at: Some(1_600_000_000),
        }
    }

    fn refusals(corpus: &DeliveredCorpus) -> Vec<(String, SetRefusal)> {
        corpus
            .refused
            .iter()
            .map(|r| match &r.outcome {
                SetOutcome::Refused { refusal, .. } => (r.set.set_name.clone(), refusal.clone()),
                other => panic!("a refused set carries a refusal, not {other:?}"),
            })
            .collect()
    }

    /// **A ledger-named row the holder lacks refuses the set** — the ledger
    /// names what the copy lacks — and nothing of the set is landed or
    /// recorded, the ledger included.
    #[tokio::test]
    async fn a_ledger_named_row_the_holder_lacks_refuses_the_set() {
        let rig = Rig::new();
        rig.hold_pair(1);
        let (dat2, meta2) = content(2);
        rig.hold_segment(&dat2, &body(3, 100));
        rig.hold_ledger(SegmentFamily::Content, &[1, 2], 3);

        let corpus = rig.leg().deliver_corpus().await.unwrap();

        assert_eq!(
            refusals(&corpus),
            vec![("__landing/mail".to_string(), SetRefusal::CustodyIncomplete)]
        );
        match &corpus.refused[0].outcome {
            SetOutcome::Refused { detail, .. } => assert!(detail.contains(&meta2), "{detail}"),
            _ => unreachable!(),
        }
        assert!(
            corpus.sets.is_empty(),
            "a refused set is never materialized"
        );
        assert!(rig.target.records.lock().unwrap().is_empty());
        assert!(rig.target.chunks.lock().unwrap().is_empty());
    }

    /// **A listed row the ledger does not name is neither landed nor
    /// recorded**, and is reported by name; the ledger's own rows land.
    #[tokio::test]
    async fn a_listed_row_the_ledger_does_not_name_is_neither_landed_nor_recorded() {
        let rig = Rig::new();
        rig.hold_pair(1);
        let (dat5, _) = content(5);
        let stray = rig.hold_segment(&dat5, &body(5, 700));
        rig.hold_ledger(SegmentFamily::Content, &[1], 2);

        let corpus = rig.leg().deliver_corpus().await.unwrap();

        assert_eq!(corpus.unanchored_rows, vec![format!("__mail/{dat5}")]);
        let records = rig.target.records.lock().unwrap().clone();
        assert_eq!(records.len(), 3);
        assert!(records.iter().all(|r| r.path != dat5));
        for (key, _) in &stray.chunks {
            assert!(
                !rig.target.chunks.lock().unwrap().contains_key(key),
                "an unanchored row's bytes never land"
            );
        }
        assert_eq!(corpus.sets.len(), 1);
    }

    /// **A holder serving a ledger below this device's pin is a rollback**:
    /// refused when the owner's acceptance does not name it — nothing landed,
    /// recorded or signed — and pulled when it does.
    #[tokio::test]
    async fn a_ledger_below_the_pin_is_refused_unless_the_acceptance_names_it() {
        let rig = Rig::new();
        rig.hold_pair(1);
        rig.hold_ledger(SegmentFamily::Content, &[1], 2);

        let leg = rig.leg().with_pins(pinned_at(9));
        let preflight = leg.preflight().await.unwrap();
        assert_eq!(
            preflight.sets[0].verdict(),
            Some(LedgerVerdict::RolledBack {
                generation: 2,
                pinned: 9,
                verified_at: Some(1_700_000_000),
            })
        );

        // Declined: the owner's acceptance does not carry the set.
        let declined = PullBackPreflight {
            sets: Vec::new(),
            ..preflight.clone()
        };
        let corpus = rig
            .leg()
            .with_pins(pinned_at(9))
            .with_acceptance(declined)
            .deliver_corpus()
            .await
            .unwrap();
        assert_eq!(
            refusals(&corpus),
            vec![("__landing/mail".to_string(), SetRefusal::NotAccepted)]
        );
        assert!(corpus.sets.is_empty());
        assert!(rig.target.records.lock().unwrap().is_empty());
        assert!(rig.target.chunks.lock().unwrap().is_empty());

        // Accepted: the owner's word pulls the older copy.
        let corpus = rig
            .leg()
            .with_pins(pinned_at(9))
            .with_acceptance(preflight)
            .deliver_corpus()
            .await
            .unwrap();
        assert!(corpus.refused.is_empty());
        assert_eq!(corpus.sets.len(), 1);
        assert_eq!(rig.target.records.lock().unwrap().len(), 3);
    }

    /// A set this device holds no pin for is **unvouched**; one at or above
    /// its pin is vouched, dated by the pin's sighting.
    #[tokio::test]
    async fn a_set_with_no_pin_is_unvouched_and_one_at_its_pin_is_vouched() {
        let rig = Rig::new();
        rig.hold_pair(1);
        rig.hold_ledger(SegmentFamily::Content, &[1], 4);
        let folder = folder_set(7);
        rig.hold_folder(&folder, &hex::encode([0x22; 32]), &body(8, 10), Some(b"n"));

        let unpinned = rig.leg().preflight().await.unwrap();
        assert_eq!(
            unpinned.sets[0].verdict(),
            Some(LedgerVerdict::Unvouched { generation: 4 })
        );
        assert_eq!(unpinned.folder_sets, vec![folder]);

        let pinned = rig.leg().with_pins(pinned_at(4)).preflight().await.unwrap();
        assert_eq!(
            pinned.sets[0].verdict(),
            Some(LedgerVerdict::Vouched {
                generation: 4,
                pinned: 4,
                verified_at: Some(1_700_000_000),
            })
        );
        assert!(
            rig.target.records.lock().unwrap().is_empty(),
            "the preflight writes nothing"
        );
    }

    /// A ledger that moved between the confirm and the run is refused
    /// `preflight_stale`, and nothing of the set lands.
    #[tokio::test]
    async fn a_ledger_that_moved_after_the_confirm_is_refused_stale() {
        let rig = Rig::new();
        rig.hold_pair(1);
        rig.hold_ledger(SegmentFamily::Content, &[1], 2);
        let accepted = rig.leg().preflight().await.unwrap();

        rig.hold_pair(2);
        rig.hold_ledger(SegmentFamily::Content, &[1, 2], 3);
        let corpus = rig
            .leg()
            .with_acceptance(accepted)
            .deliver_corpus()
            .await
            .unwrap();

        assert_eq!(
            refusals(&corpus),
            vec![("__landing/mail".to_string(), SetRefusal::PreflightStale)]
        );
        assert!(rig.target.records.lock().unwrap().is_empty());
    }

    /// Pins on `__mail`'s Content ledger at `content` and its Placement
    /// ledger at `placement`.
    fn pinned_both(content: u32, placement: u32) -> DevicePins {
        let mut pins = pinned_at(content);
        pins.pins.insert(
            fauna_client_backup::audit::ledger_key(
                "__mail",
                &SegmentFamily::Placement.mirror_path(&scope_hex(), "mail"),
            ),
            fauna_client_backup::audit::LedgerPin {
                generation: placement,
                seen_live_at: Some(1_700_000_000),
            },
        );
        pins
    }

    /// **A pinned family the holder withholds reads older than verified**:
    /// the holder serves Content at its pin and lists no Placement ledger
    /// though this device verified one at 5. The confirm shows it withheld,
    /// the run refuses the set unless the acceptance names it, pulls the set
    /// without the family when it does, and an acceptance taken while the
    /// family was withheld reads stale once it appears.
    #[tokio::test]
    async fn a_withheld_pinned_family_is_refused_unless_the_acceptance_names_it() {
        let rig = Rig::new();
        rig.hold_pair(1);
        rig.hold_ledger(SegmentFamily::Content, &[1], 2);

        let preflight = rig
            .leg()
            .with_pins(pinned_both(2, 5))
            .preflight()
            .await
            .unwrap();
        assert_eq!(
            preflight.sets[0].verdict(),
            Some(LedgerVerdict::Withheld {
                pinned: 5,
                verified_at: Some(1_700_000_000),
            })
        );

        let declined = PullBackPreflight {
            sets: Vec::new(),
            ..preflight.clone()
        };
        let corpus = rig
            .leg()
            .with_pins(pinned_both(2, 5))
            .with_acceptance(declined)
            .deliver_corpus()
            .await
            .unwrap();
        assert_eq!(
            refusals(&corpus),
            vec![("__landing/mail".to_string(), SetRefusal::NotAccepted)]
        );
        assert!(rig.target.records.lock().unwrap().is_empty());
        assert!(rig.target.chunks.lock().unwrap().is_empty());

        // With no acceptance at all (a host showing no lines), the vouched-
        // only acceptance leaves a withheld family out too.
        assert!(preflight.clone().vouched_only().sets.is_empty());

        let corpus = rig
            .leg()
            .with_pins(pinned_both(2, 5))
            .with_acceptance(preflight.clone())
            .deliver_corpus()
            .await
            .unwrap();
        assert!(corpus.refused.is_empty());
        assert_eq!(corpus.sets.len(), 1);
        assert_eq!(rig.target.records.lock().unwrap().len(), 3);

        // The withheld family appears after the confirm: stale.
        let rig = Rig::new();
        rig.hold_pair(1);
        rig.hold_ledger(SegmentFamily::Content, &[1], 2);
        rig.hold_ledger(SegmentFamily::Placement, &[], 5);
        let corpus = rig
            .leg()
            .with_pins(pinned_both(2, 5))
            .with_acceptance(preflight)
            .deliver_corpus()
            .await
            .unwrap();
        assert_eq!(
            refusals(&corpus),
            vec![("__landing/mail".to_string(), SetRefusal::PreflightStale)]
        );
        assert!(rig.target.records.lock().unwrap().is_empty());
    }

    /// **A pinned set the holder withholds whole reads older than verified**:
    /// the confirm carries a line for it, and the run refuses it unless the
    /// acceptance names it; accepted, nothing of it moves and nothing is
    /// refused.
    #[tokio::test]
    async fn a_withheld_pinned_set_is_refused_unless_the_acceptance_names_it() {
        let rig = Rig::new();
        let folder = folder_set(7);
        rig.hold_folder(&folder, &hex::encode([0x22; 32]), &body(8, 10), Some(b"n"));

        let preflight = rig.leg().with_pins(pinned_at(4)).preflight().await.unwrap();
        assert_eq!(preflight.sets.len(), 1);
        assert_eq!(preflight.sets[0].set_name, "__mail");
        assert_eq!(
            preflight.sets[0].verdict(),
            Some(LedgerVerdict::Withheld {
                pinned: 4,
                verified_at: Some(1_700_000_000),
            })
        );
        assert_eq!(
            fauna_client_backup::reseed::preflight_lines(&preflight)[0].key,
            "backups.reseed_preflight_withheld"
        );

        let declined = PullBackPreflight {
            sets: Vec::new(),
            ..preflight.clone()
        };
        let corpus = rig
            .leg()
            .with_pins(pinned_at(4))
            .with_acceptance(declined)
            .deliver_corpus()
            .await
            .unwrap();
        assert_eq!(
            refusals(&corpus),
            vec![("__landing/mail".to_string(), SetRefusal::NotAccepted)]
        );

        let corpus = rig
            .leg()
            .with_pins(pinned_at(4))
            .with_acceptance(preflight)
            .deliver_corpus()
            .await
            .unwrap();
        assert!(corpus.refused.is_empty());
        assert_eq!(
            corpus
                .sets
                .iter()
                .map(|s| s.set_name.as_str())
                .collect::<Vec<_>>(),
            vec![folder.as_str()],
            "only the folder moves; the withheld set lands nothing"
        );
    }

    /// A byte that fails the hash it was addressed by is refused before it
    /// lands, and nothing is recorded for its path.
    #[tokio::test]
    async fn a_chunk_that_fails_its_hash_is_refused_before_it_lands() {
        let rig = Rig::new();
        let (dat1, meta1) = content(1);
        let sealed = rig.hold_segment(&dat1, &body(1, 5_000));
        rig.hold_segment(&meta1, &body(2, 30));
        rig.hold_ledger(SegmentFamily::Content, &[1], 2);
        *rig.dest.tamper.lock().unwrap() = Some(sealed.chunks[0].0);

        let leg = rig.leg();
        let accepted = leg.preflight().await.unwrap();
        let err = leg.run_all(&accepted).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("failed hash verification"),
            "{err:#}"
        );
        assert!(rig.target.chunks.lock().unwrap().is_empty());
        assert!(rig.target.records.lock().unwrap().is_empty());
    }

    /// A folder row the destination holds no sealed name for lands and is
    /// reported — the folder cannot be re-homed without it — and a set the
    /// coverage rows do not name is delivered nameless.
    #[tokio::test]
    async fn a_folder_row_without_a_sealed_name_is_delivered_and_reported() {
        let rig = Rig::new();
        let set = folder_set(3);
        let leaf = hex::encode([0x33; 32]);
        rig.hold_folder(&set, &leaf, &body(4, 100), None);

        let corpus = rig.leg().deliver_corpus().await.unwrap();
        assert_eq!(
            corpus.folder_paths_without_seal,
            vec![format!("{set}/{leaf}")]
        );
        assert_eq!(corpus.sets[0].folder_display_name, None);
    }

    /// **The re-home signs every sealed row the leg delivered**, under the
    /// target set's nonce, over the manifest's own `total_size` — the
    /// statement the target's folder materialize rebuilds from its custody.
    /// No nonce, or no signer, holds the set with the reason.
    #[tokio::test]
    async fn the_rehome_signs_every_delivered_sealed_row_under_the_targets_nonce() {
        let rig = Rig::new();
        let set = folder_set(7);
        let leaf = hex::encode([0x22; 32]);
        let sealed = rig.hold_folder(&set, &leaf, &body(5, 4_000), Some(b"sealed-name"));
        rig.hold_folder(&set, &hex::encode([0x44; 32]), &body(6, 10), None);

        let root = fauna_core::identity::ActorKeypair::from_secret([0x44; 32]);
        let owner = root.actor_id().0;
        let nonce = [0x6E; 32];
        let signing = |nonces: HashMap<String, [u8; 32]>| crate::RecordSigning {
            signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(&root)),
            set_nonce: crate::SetNonceSource::by_folder(nonces),
        };
        let target = DeliveredSet {
            set_name: set.clone(),
            axis: SetAxis::Folder,
            folder_display_name: Some("Photos".into()),
            folder_label: None,
        };

        let leg = rig
            .leg()
            .with_rehome_signing(signing([("Photos".to_string(), nonce)].into()));
        leg.run_all(&PullBackPreflight::default()).await.unwrap();
        let FolderRehome::Signed {
            signer_key,
            signatures,
        } = leg.sign_folder_rehome(&target).await
        else {
            panic!("a host with a signer and the target's nonce signs");
        };
        assert_eq!(signatures.len(), 1, "the nameless row is not signed");
        let statement = SignedChange::for_rehome(
            nonce,
            owner,
            [0x22; 32],
            sealed.manifest_hash.digest(),
            sealed.manifest.total_size as i64,
            b"sealed-name",
        );
        fauna_protocol::sync_writer_sig::verify_statement(
            &statement,
            &signatures[0].signature,
            &signer_key,
            &fauna_protocol::sync_writer_sig::SignerCertCache::new(),
            fauna_core::data::Timestamp::now(),
        )
        .expect("the signature covers the delivered row");

        let no_nonce = rig.leg().with_rehome_signing(signing(HashMap::new()));
        no_nonce
            .run_all(&PullBackPreflight::default())
            .await
            .unwrap();
        assert!(matches!(
            no_nonce.sign_folder_rehome(&target).await,
            FolderRehome::Unsigned { reason } if reason.contains("Photos")
        ));
        assert!(matches!(
            rig.leg().sign_folder_rehome(&target).await,
            FolderRehome::Unsigned { .. }
        ));
    }

    /// The names come from the coverage rows of the destination pulled from —
    /// never another destination's, never the enrollment row's.
    #[test]
    fn folder_names_come_from_this_destinations_coverage_rows() {
        let set = folder_set(7);
        let label = fauna_core::data::CoveredFolderLabel {
            name_hash: [0x4E; 32],
            name_sealed: b"sealed".to_vec(),
        };
        let rows = vec![
            BackupDestination {
                destination_id: "dest-1".into(),
                folder_name: "__mail".into(),
                ..Default::default()
            },
            BackupDestination {
                destination_id: "dest-1".into(),
                folder_name: set.clone(),
                folder_display_name: Some("Photos".into()),
                folder_label: Some(label.clone()),
                ..Default::default()
            },
            BackupDestination {
                destination_id: "dest-2".into(),
                folder_name: folder_set(8),
                folder_display_name: Some("Elsewhere".into()),
                ..Default::default()
            },
        ];
        let names = folder_names_from_coverage(&rows, "dest-1", None);
        assert_eq!(names.len(), 1);
        assert_eq!(
            names[&set],
            FolderNaming {
                display_name: Some("Photos".into()),
                label: Some(FolderLabel {
                    name_hash: label.name_hash,
                    name_sealed: label.name_sealed,
                }),
            }
        );
    }

    /// The byte plane both ends of `streams_one_chunk_at_a_time` log to.
    struct Streamed {
        dest: Arc<Destination>,
        log: Log,
        held: Mutex<HashMap<ContentHash, Vec<u8>>>,
    }

    impl Streamed {
        fn tag(&self, key: &ContentHash) -> String {
            hex::encode(&key.digest()[..2])
        }
    }

    #[async_trait::async_trait]
    impl BlobFetcher for Streamed {
        async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
            DestBytes(self.dest.clone()).fetch_manifest(hash).await
        }

        async fn fetch_chunks(&self, keys: &[ContentHash], path: &str) -> Result<Vec<Vec<u8>>> {
            let tags: Vec<String> = keys.iter().map(|k| self.tag(k)).collect();
            self.log
                .lock()
                .unwrap()
                .push(format!("fetch {}", tags.join(",")));
            DestBytes(self.dest.clone()).fetch_chunks(keys, path).await
        }
    }

    #[async_trait::async_trait]
    impl BlobPushSink for Streamed {
        async fn missing_chunks(&self, keys: &[ContentHash]) -> Result<Vec<ContentHash>> {
            let held = self.held.lock().unwrap();
            Ok(keys
                .iter()
                .copied()
                .filter(|k| !held.contains_key(k))
                .collect())
        }

        async fn put_chunk(&self, key: &ContentHash, body: &[u8]) -> Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("put {}", self.tag(key)));
            self.held.lock().unwrap().insert(*key, body.to_vec());
            Ok(())
        }

        async fn put_manifest(&self, _bytes: &[u8]) -> Result<()> {
            self.log.lock().unwrap().push("manifest".into());
            Ok(())
        }
    }

    /// **The pull streams chunk by chunk** (`message-segment-store.md`: a
    /// transfer's memory is a constant chunk, never the body). Each chunk the
    /// target lacks is fetched alone and pushed before the next is fetched, a
    /// store key the manifest repeats moves once, one the target already holds
    /// is never fetched, and the manifest lands last — so the app holds at
    /// most one chunk body of a path at once, however large the path.
    #[tokio::test]
    async fn streams_one_chunk_at_a_time() {
        let bodies: Vec<Vec<u8>> = (0..4u8).map(|t| body(t, 64 + usize::from(t))).collect();
        let keys: Vec<ContentHash> = bodies.iter().map(|b| ContentHash::of_raw(b)).collect();
        // Chunk 1 repeats at the end; chunk 3 the target already holds.
        let order = [0usize, 1, 2, 3, 1];
        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::of_raw(
                &order
                    .iter()
                    .flat_map(|&i| bodies[i].clone())
                    .collect::<Vec<_>>(),
            ),
            total_size: order.iter().map(|&i| bodies[i].len() as u64).sum(),
            chunk_hashes: order.iter().map(|&i| keys[i]).collect(),
            chunk_sizes: order.iter().map(|&i| bodies[i].len() as u64).collect(),
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);
        let dest = Arc::new(Destination::default());
        {
            let mut blobs = dest.blobs.lock().unwrap();
            blobs.insert(manifest_hash, manifest_bytes);
            for (key, body) in keys.iter().zip(&bodies) {
                blobs.insert(*key, body.clone());
            }
        }
        let plane = Streamed {
            dest,
            log: Arc::new(Mutex::new(Vec::new())),
            held: Mutex::new(HashMap::from([(keys[3], bodies[3].clone())])),
        };

        let pulled = pull_as_is(&plane, &plane, manifest_hash, "big.bin")
            .await
            .unwrap();

        let tag = |i: usize| plane.tag(&keys[i]);
        let expected: Vec<String> = [0, 1, 2]
            .into_iter()
            .flat_map(|i| [format!("fetch {}", tag(i)), format!("put {}", tag(i))])
            .chain(["manifest".to_string()])
            .collect();
        assert_eq!(*plane.log.lock().unwrap(), expected);
        assert_eq!(pulled.chunks_uploaded, 3);
        assert_eq!(pulled.manifest_hash, manifest_hash);
        assert_eq!(pulled.total_size, manifest.total_size);
        for (key, body) in keys.iter().zip(&bodies) {
            assert_eq!(plane.held.lock().unwrap().get(key), Some(body));
        }
    }
}
