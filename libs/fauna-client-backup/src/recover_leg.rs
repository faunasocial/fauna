//! **The lived-in recovery's delivery leg** — one body for every app, web
//! included: land every generation a backup copy holds for a kind's reserved
//! set on the regressed source, call `fauna.backup.custody.recover` there,
//! then run the post-recovery duties.
//!
//! Owner: `docs/goal/architecture/segment-backup-protocol.md` § Client-device
//! custodian (pull) → *Restore* → *Recovery into the lived-in nest that
//! regressed*, part (1) ("web-capable like the pull-back"; what the leg does,
//! in order), and *Recovery from this device's own store* (the second end).
//! The driver that puts a run in order and reads its result is
//! [`crate::recover`]; this module is the [`crate::recover::RecoveryLeg`] it
//! drives.
//!
//! # Why it lives here
//!
//! The leg used to live in the native-only `fauna-sync-engine`, so the browser
//! could not run it. Everything it needs is wasm-clean — the deterministic
//! re-seal ([`fauna_core::blob_seal::seal_blob`]), the open-route byte walk
//! ([`fauna_core::file_download::download_file_bytes_by_manifest`]), the segment
//! grammar ([`fauna_protocol::segments`]) — except three things each app
//! supplies, which are therefore seams here:
//!
//! - [`BlobPushSink`] — the source's owner-session byte plane (`POST
//!   /chunks/check`, `/chunks`, `/manifests`): the sync engine's pooled
//!   transfer client natively, the page's bearer-carrying `fetch` on web;
//! - [`CustodyRecorder`] — the self-healing `fauna.sync.changes.record`
//!   reserved-set arm, which `fauna_client_sync::SyncClient` implements (that
//!   crate depends on this one, so the leg cannot name it);
//! - [`HeldCopy`] — this device's own custodian store, the second end a copy is
//!   read off, which only a native host keeps
//!   (`fauna_sync_engine::reseed::RecoveryStore`).
//!
//! # Send, per target
//!
//! The body is generic and its futures carry whatever `Send`-ness the concrete
//! transports give them. On wasm the [`crate::recover::RecoveryLeg`] impl below
//! is generic, since nothing there is `Send`. Natively the seam's future must
//! be `Send` (the run goes on a spawned task), which a generic transport cannot
//! promise, so the native impl is the sync engine's, over its concrete
//! `fauna_client::NestClient` (`fauna_sync_engine::reseed::NativeRecoveryLeg`),
//! delegating to [`RecoveryDelivery::recover_set`].

use std::collections::HashSet;

use anyhow::{Context, Result};
use async_trait::async_trait;
use fauna_core::blob_seal::{SealedBlob, seal_blob};
use fauna_core::crypto::{NestBackupKey, OwnerSealKey};
use fauna_core::data::ContentHash;
use fauna_core::file_download::{FileDownloadKeys, download_file_bytes_by_manifest};
use fauna_protocol::segments::{
    BACKED_UP_KINDS, LiveManifestMirror, SegmentFamily, SegmentHalf,
    parse_reserved_backup_set_name, reserved_backup_set_name,
};
use fauna_protocol::{MaybeSendSync, RpcRequester};

use crate::recover::LegAnswer;

// ═════════════════════════════════════════════════════════════════════════════
// The seams an app supplies
// ═════════════════════════════════════════════════════════════════════════════

/// A nest's **bulk-byte** plane, as a leg that lands custody needs it.
///
/// A trait rather than a concrete client for two reasons: the leg's
/// interesting behaviour is *what* it pushes and *in what order*, which must
/// be assertable without a nest; and each target reaches the plane its own way
/// — the sync engine's `SyncClientSink` natively, the page's own `fetch`
/// (`fauna-wasm`'s recovery door) on web.
///
/// `MaybeSendSync` + the dual `async_trait` arm, exactly as
/// [`fauna_core::file_download::BlobFetcher`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait BlobPushSink: MaybeSendSync {
    /// `POST /chunks/check` — of these store keys, which does the target NOT
    /// already hold? Dedup is the whole reason a re-push after a tear is cheap;
    /// a sink with no check answers every key (the store is content-addressed,
    /// so a re-put is idempotent).
    async fn missing_chunks(&self, store_keys: &[ContentHash]) -> Result<Vec<ContentHash>>;
    /// `POST /chunks` — one chunk body, addressed by its store key.
    async fn put_chunk(&self, store_key: &ContentHash, body: &[u8]) -> Result<()>;
    /// `POST /manifests` — the canonical-encoded [`fauna_core::chunk::ChunkManifest`].
    async fn put_manifest(&self, manifest_bytes: &[u8]) -> Result<()>;
}

/// What one [`push_sealed`] moved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PushedBlob {
    /// Chunk bodies actually uploaded (post-dedup).
    pub chunks_uploaded: usize,
    /// Chunk bodies the target already held.
    pub chunks_deduped: usize,
}

/// Bytes first: dedup-check, upload what is missing, then the manifest — the
/// one push every leg that lands custody on a nest shares (this leg, the
/// re-seed's and the nest-held pull-back).
///
/// The manifest lands **after** its chunks so the target never holds a
/// manifest naming bytes it does not have — the same rule the custodian
/// store's own `put` follows locally.
pub async fn push_sealed<P: BlobPushSink + ?Sized>(
    sink: &P,
    sealed: &SealedBlob,
) -> Result<PushedBlob> {
    let keys: Vec<ContentHash> = sealed.chunks.iter().map(|(k, _)| *k).collect();
    let missing = sink
        .missing_chunks(&keys)
        .await
        .context("chunk dedup check")?;
    let mut pushed = PushedBlob {
        chunks_deduped: keys.len().saturating_sub(missing.len()),
        ..PushedBlob::default()
    };
    for (key, body) in &sealed.chunks {
        if !missing.contains(key) {
            continue;
        }
        sink.put_chunk(key, body)
            .await
            .with_context(|| format!("uploading chunk {}", hex::encode(key.digest())))?;
        pushed.chunks_uploaded += 1;
    }
    sink.put_manifest(&sealed.manifest_bytes)
        .await
        .context("uploading manifest")?;
    Ok(pushed)
}

/// The client-authed `fauna.sync.changes.record` **reserved-set** arm, through
/// the self-healing record entry: the device driving a recovery is often
/// unregistered at the destination (nothing else of its own lives there), and
/// may be unregistered on its own nest too when no folder sync ever ran there
/// (web never runs one). `fauna_client_sync::SyncClient` is the
/// implementation; `path_sealed` is never sent — a reserved backup set's
/// custody paths are machine-authored routing keys, the class the record
/// funnel exempts from the seal requirement.
///
/// Static dispatch, like [`RpcRequester`]: each impl's future keeps its own
/// `Send`-ness.
#[allow(async_fn_in_trait)]
pub trait CustodyRecorder {
    async fn record_custody(
        &self,
        set: &str,
        device_id: &str,
        path: &str,
        manifest_hash: Option<String>,
        size_bytes: i64,
        change_type: &str,
    ) -> std::result::Result<(), String>;
}

/// The [`CustodyRecorder`] of an end that records nothing — the destination
/// recorder of a leg reading this device's own store ([`RecoveryDelivery::from_store`]),
/// whose duties tombstone in the store instead.
pub struct NoCustodyRecorder;

impl CustodyRecorder for NoCustodyRecorder {
    async fn record_custody(
        &self,
        _set: &str,
        _device_id: &str,
        path: &str,
        _manifest_hash: Option<String>,
        _size_bytes: i64,
        _change_type: &str,
    ) -> std::result::Result<(), String> {
        Err(format!("{path}: this end records no custody"))
    }
}

/// The device's own record of an **accepted source regression** — the notice's
/// record the audit loop keeps (`backup-restore.md` § Background Tasks →
/// *Implementation status (audit loop)*, the accepted-regression bullet), which
/// the recovery's post-recovery duties clear on a whole reply.
///
/// A seam, because the record lives in a client-local store and the leg must
/// not know how a shell keeps it: [`AuditStoreRegressionRecord`] is the
/// implementation over the audit's store, the sync engine's
/// `CustodianStoreRegressionRecord` the one over a custodian store.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait AcceptedRegressionRecord: MaybeSendSync {
    /// Clear the record this device holds for `kind`'s set at the destination
    /// the recovery drew from. Called once, only after a whole `recover` reply.
    async fn clear(&self, kind: &str) -> Result<()>;
}

/// The [`AcceptedRegressionRecord`] over the audit's own client-local store
/// ([`crate::audit::AuditStateStore`] — a file natively, `localStorage` on
/// web): clears the entries this device holds for the recovered kind's
/// reserved set at the destination the recovery drew from — the content
/// ledger's and the journal's alike.
pub struct AuditStoreRegressionRecord<'a> {
    pub store: &'a dyn crate::audit::AuditStateStore,
    /// The destination the recovery drew from, as the audit records name it.
    pub destination_id: &'a str,
    /// The owner scope whose reserved sets were recovered.
    pub scope_id: [u8; 32],
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl AcceptedRegressionRecord for AuditStoreRegressionRecord<'_> {
    async fn clear(&self, kind: &str) -> Result<()> {
        let set = reserved_backup_set_name(kind, &self.scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no backup surface: {kind}"))?;
        crate::audit::clear_accepted_regressions(self.store, self.destination_id, &set)
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("the audit store: {e}"))
    }
}

/// The [`AcceptedRegressionRecord`] of a device that keeps none — a recovery
/// driven where no audit store exists.
pub struct NoAcceptedRegressionRecord;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl AcceptedRegressionRecord for NoAcceptedRegressionRecord {
    async fn clear(&self, _kind: &str) -> Result<()> {
        Ok(())
    }
}

/// One generation a copy holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestGeneration {
    /// The path inside the set.
    pub path: String,
    pub manifest_hash: ContentHash,
    /// `(live, stamp)`: retained generations land first (oldest supersede
    /// first), the live one last, so the source's own live row at each path is
    /// the copy's.
    pub order: (bool, i64),
}

/// A copy held on **this device** — the second end a recovery reads
/// (*Recovery from this device's own store*): only a native host keeps a
/// custodian store, so the implementation is the sync engine's
/// `RecoveryStore`. The caller is the store's one writer for the run.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait HeldCopy: MaybeSendSync {
    /// Every generation held of `set`, tombstones left out (they hold no
    /// bytes).
    async fn generations(&self, set: &str) -> Result<Vec<DestGeneration>>;
    /// One generation's plaintext.
    async fn read(&self, set: &str, generation: &DestGeneration) -> Result<Vec<u8>>;
    /// Tombstone every live segment path of `kind`'s families in `set` whose
    /// id `named` (the source's live listing per family, read after the verb)
    /// does not carry; answer the paths retired.
    async fn retire_unnamed(
        &self,
        kind: &str,
        set: &str,
        scope_hex: &str,
        named: &[(SegmentFamily, HashSet<u32>)],
    ) -> Result<Vec<String>>;
}

// ═════════════════════════════════════════════════════════════════════════════
// The walk
// ═════════════════════════════════════════════════════════════════════════════

/// The order a reserved segment set's generations land on a nest in — the one
/// walk every leg that moves a copy shares ([`RecoveryDelivery`] and the sync
/// engine's nest-held pull-back): each family of `kind` in
/// [`SegmentFamily::PASS_ORDER`], its segments by id with the `.dat` before the
/// `.meta` (generations of one path oldest first), then every family's
/// `manifest.<kind>` mirror **last** — a mirror naming a segment whose bytes
/// have not landed is the ordering that makes a torn delivery look whole.
///
/// A generation whose path is neither a segment half nor a mirror of `kind` is
/// not part of the corpus and is left out.
pub fn segment_set_order<'g>(
    kind: &str,
    scope_hex: &str,
    generations: &'g [DestGeneration],
) -> Vec<&'g DestGeneration> {
    let mut ordered: Vec<&DestGeneration> = Vec::new();
    for family in SegmentFamily::PASS_ORDER {
        if family.serve_kind(kind).is_none() {
            continue;
        }
        let mut segs: Vec<(u32, u8, (bool, i64), &DestGeneration)> = generations
            .iter()
            .filter_map(|g| {
                family.parse(scope_hex, &g.path).map(|(id, half)| {
                    let half = match half {
                        SegmentHalf::Dat => 0,
                        SegmentHalf::Meta => 1,
                    };
                    (id, half, g.order, g)
                })
            })
            .collect();
        segs.sort_by_key(|(id, half, order, _)| (*id, *half, *order));
        ordered.extend(segs.into_iter().map(|(_, _, _, g)| g));
    }
    for family in SegmentFamily::PASS_ORDER {
        if family.serve_kind(kind).is_none() {
            continue;
        }
        let path = family.mirror_path(scope_hex, kind);
        let mut gens: Vec<&DestGeneration> =
            generations.iter().filter(|g| g.path == path).collect();
        gens.sort_by_key(|g| g.order);
        ordered.extend(gens);
    }
    ordered
}

/// A custody row's hex manifest hash, or `None` when it is not one.
pub fn parse_manifest_hex(hex_text: &str) -> Option<ContentHash> {
    let mut digest = [0u8; 32];
    hex::decode_to_slice(hex_text, &mut digest).ok()?;
    Some(ContentHash::from_digest_raw(digest))
}

// ═════════════════════════════════════════════════════════════════════════════
// The leg
// ═════════════════════════════════════════════════════════════════════════════

/// Where the recovery's copy is read: the two ends [`RecoveryDelivery`] runs
/// over, everything else about the leg shared.
// One value per leg, built once and never moved in bulk: boxing the nest end
// would buy nothing.
#[allow(clippy::large_enum_variant)]
enum RecoveryCopy<'a, D: CustodyRecorder> {
    /// A nest destination, over the owner's own connection to it; its copy is
    /// the source's nest-posture ciphertext, opened under the `NestBackupKey`.
    Nest {
        destination: RecoveryDestination<'a, D>,
        keys: FileDownloadKeys,
    },
    /// This device's own custodian store.
    Held(&'a dyn HeldCopy),
}

/// The destination end of the recovery leg: the owner's own connection to it.
pub struct RecoveryDestination<'a, D: CustodyRecorder> {
    /// `fauna.backup.custody.list` + `fauna.backup.generation.list`, walked to
    /// completion ([`crate::audit::read_full_custody`] /
    /// [`crate::audit::read_full_generations`]).
    pub seam: &'a dyn crate::trust::BackupNestSeam,
    /// The destination's open, hash-verified byte routes.
    pub bytes: &'a dyn fauna_core::file_download::BlobFetcher,
    /// The reserved-set arm at the destination — the post-recovery tombstones.
    pub records: &'a D,
    /// Hex device id the tombstones are recorded under at the **destination**
    /// — registered there by the record entry's own self-heal when it is not.
    pub device_id: String,
}

/// The source end of the recovery leg: the owner's own connection to the
/// regressed nest.
pub struct RecoverySource<'a, P: BlobPushSink, R: RpcRequester, C: CustodyRecorder> {
    pub bytes: &'a P,
    /// `fauna.segments.list` (the skip) and `fauna.backup.custody.recover`
    /// (the verb).
    pub nest: &'a R,
    /// The reserved-set arm on the source — the landing.
    pub records: &'a C,
    /// Hex device id the landed rows are recorded under on the **source** —
    /// self-healed like the destination's.
    pub device_id: String,
}

/// What one recovery run did.
#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// Every `(path, manifest hash)` generation landed on the source, in the
    /// order landed.
    pub delivered: Vec<(String, String)>,
    /// Segment generations not landed because the source already holds that
    /// half byte-identical — the economy; the verb reads the source's own copy.
    pub skipped: Vec<String>,
    /// The verb's reply.
    pub recovered: fauna_protocol::backup::CustodyRecoverReply,
    /// Copy paths tombstoned after the whole reply — the recovered segment
    /// paths the source no longer names.
    pub tombstoned_at_destination: Vec<String>,
    pub chunks_uploaded: usize,
    pub chunks_deduped: usize,
}

/// **The recovery's delivery leg** (`segment-backup-protocol.md` § Client-device
/// custodian (pull) → *Restore* → *Recovery into the lived-in nest that
/// regressed*, part (1)): land every generation the copy holds for a kind's
/// reserved set — live and retained, both families — on the regressed source,
/// then call `fauna.backup.custody.recover` there, then the post-recovery
/// duties.
///
/// What a nest destination holds is the source's own nest-posture ciphertext
/// under the `NestBackupKey` root the source holds a grant for, so the leg
/// opens each generation under that root and re-seals it through the
/// deterministic [`seal_blob`] — landing byte-identical custody, same store
/// keys and manifest hash — over the owner-session byte plane and the
/// client-authed reserved-set arm, **under the path it had at the copy**. No
/// key material moves and nothing new is authorized; it runs in the app, web
/// included.
///
/// A path recorded twice on the source supersedes, and the source's own window
/// retains the displaced generation, so the verb reads every generation the
/// leg landed. Order: [`segment_set_order`]. Resumable by content addressing:
/// every tear leaves an ordinary, GC-safe set.
///
/// **Two ends, one leg** (*Recovery from this device's own store*, question
/// (1)): the copy is read off a nest destination ([`Self::new`]) or off this
/// device's own custodian store ([`Self::from_store`], a [`HeldCopy`]). Only
/// where the copy is read and where the post-recovery tombstones land differ;
/// the walk, the skip, the re-sealed landing, the verb and the record's
/// clearing are the same code for both.
///
/// `P` the source's byte plane, `R` its requester, `C` its record arm, `D` the
/// destination's record arm ([`NoCustodyRecorder`] for the store end).
pub struct RecoveryDelivery<'a, P, R, C, D>
where
    P: BlobPushSink,
    R: RpcRequester,
    C: CustodyRecorder,
    D: CustodyRecorder,
{
    copy: RecoveryCopy<'a, D>,
    source: RecoverySource<'a, P, R, C>,
    scope_id: [u8; 32],
    nest_root: [u8; 32],
    regression: &'a dyn AcceptedRegressionRecord,
}

impl<'a, P, R, C> RecoveryDelivery<'a, P, R, C, NoCustodyRecorder>
where
    P: BlobPushSink,
    R: RpcRequester,
    C: CustodyRecorder,
{
    /// The store end: recover from this device's own custodian store. The
    /// caller has granted `nest_key` to the source first (the re-seed's phase 1
    /// verb, idempotent — [`crate::recover::run_store_recovery`]) — the verb
    /// opens the delivered custody under that grant — and is the store's one
    /// writer for the run. `regression` is the store's own record.
    pub fn from_store(
        store: &'a dyn HeldCopy,
        source: RecoverySource<'a, P, R, C>,
        scope_id: [u8; 32],
        nest_key: &NestBackupKey,
        regression: &'a dyn AcceptedRegressionRecord,
    ) -> Self {
        Self {
            copy: RecoveryCopy::Held(store),
            source,
            scope_id,
            nest_root: nest_key.convergent_chunk_root(),
            regression,
        }
    }
}

/// Both ends' requesters classify their errors: the verb's refusal reaches the
/// result view by its wire code ([`LegAnswer`]).
impl<'a, P, R, C, D> RecoveryDelivery<'a, P, R, C, D>
where
    P: BlobPushSink,
    R: RpcRequester,
    C: CustodyRecorder,
    D: CustodyRecorder,
    R::Error: fauna_protocol::RpcErrorClass,
{
    /// The nest end: recover from a nest destination over the owner's own
    /// connection to it.
    pub fn new(
        destination: RecoveryDestination<'a, D>,
        source: RecoverySource<'a, P, R, C>,
        scope_id: [u8; 32],
        nest_key: &NestBackupKey,
        regression: &'a dyn AcceptedRegressionRecord,
    ) -> Self {
        Self {
            copy: RecoveryCopy::Nest {
                destination,
                // Sealed-only: the destination names every manifest this
                // leg opens, and the leg reseals what it opens under the
                // owner's `nest_root` — a plaintext copy it built itself
                // would be laundered into an owner seal
                // (`client-device-custodian.md` § Client-device custodian
                // (pull) → *Restore* → *Recovery into the lived-in nest that
                // regressed*).
                keys: FileDownloadKeys::owner(OwnerSealKey::SourceNest(nest_key.clone()))
                    .sealed_only(),
            },
            source,
            scope_id,
            nest_root: nest_key.convergent_chunk_root(),
            regression,
        }
    }

    /// Recover one backed-up kind: land, recover, then the duties.
    pub async fn run_kind(&self, kind: &str) -> Result<RecoveryReport> {
        let set = reserved_backup_set_name(kind, &self.scope_id)
            .ok_or_else(|| anyhow::anyhow!("kind has no backup surface: {kind}"))?;
        let mut report = self.deliver(kind, &set).await?;
        report.recovered = self
            .verb(&set)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("recovery: the source's recover of {set}"))?;
        report.tombstoned_at_destination = self.duties(kind, &set).await?;
        Ok(report)
    }

    /// The leg the shared recovery driver runs ([`crate::recover::run_recovery`]):
    /// one reserved set at a time, its delivery and verb failures sorted into
    /// what the owner can act on (a refusal by its code) and what a re-run
    /// cures (a failure), and a duty that did not complete after a whole reply
    /// reported beside the reply rather than in its place — the records are
    /// back either way. Every [`crate::recover::RecoveryLeg`] impl delegates
    /// here.
    pub async fn recover_set(&self, set_name: &str) -> LegAnswer {
        use fauna_protocol::RpcErrorClass;
        // Only a backed-up kind's own reserved set for this scope — the name
        // the notice's record carries is data this device wrote, but the leg
        // derives the set it lands into, never trusts it.
        let kind = parse_reserved_backup_set_name(set_name)
            .map(|(kind, _)| kind)
            .filter(|kind| BACKED_UP_KINDS.contains(kind))
            .filter(|kind| {
                reserved_backup_set_name(kind, &self.scope_id).as_deref() == Some(set_name)
            });
        let Some(kind) = kind else {
            return LegAnswer::Failed(format!("{set_name} is not a backed-up segment set"));
        };
        if let Err(e) = self.deliver(kind, set_name).await {
            return LegAnswer::Failed(format!("{e:#}"));
        }
        let reply = match self.verb(set_name).await {
            Ok(reply) => reply,
            Err(e) => {
                return match e.as_rpc_error() {
                    Some(rpc) => LegAnswer::Refused(rpc.clone()),
                    None => LegAnswer::Failed(e.to_string()),
                };
            }
        };
        let duties = self
            .duties(kind, set_name)
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:#}"));
        LegAnswer::Replied { reply, duties }
    }

    /// `fauna.backup.custody.recover` on the source, its error kept typed so a
    /// refusal reaches the owner by its code.
    async fn verb(
        &self,
        set: &str,
    ) -> std::result::Result<fauna_protocol::backup::CustodyRecoverReply, R::Error> {
        self.source
            .nest
            .request(
                "fauna.backup.custody.recover",
                fauna_protocol::backup::CustodyRecoverRequest {
                    set_name: set.to_string(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// The post-recovery duties — only ever after a whole reply: clear this
    /// device's notice record, then retire the copy's unnamed paths.
    async fn duties(&self, kind: &str, set: &str) -> Result<Vec<String>> {
        self.regression
            .clear(kind)
            .await
            .context("recovery: clearing the accepted-regression record")?;
        self.retire_unnamed(kind, set).await
    }

    /// Every generation of the set the copy holds, landed on the source — the
    /// landing alone, before the verb ([`Self::run_kind`] is the whole run).
    /// Public so a host's tests can pin what each end lands, byte for byte.
    pub async fn deliver(&self, kind: &str, set: &str) -> Result<RecoveryReport> {
        let scope_hex = hex::encode(self.scope_id);
        let generations = match &self.copy {
            RecoveryCopy::Nest { destination, .. } => {
                Self::destination_generations(destination, set).await?
            }
            RecoveryCopy::Held(store) => store
                .generations(set)
                .await
                .context("recovery: this device's store")?,
        };

        // Segments of each family in pass order, then every mirror last.
        let held = self.source_segments(kind, &scope_hex).await;
        let mut report = RecoveryReport::default();
        for g in segment_set_order(kind, &scope_hex, &generations) {
            let plain = match &self.copy {
                RecoveryCopy::Nest { destination, keys } => download_file_bytes_by_manifest(
                    destination.bytes,
                    keys,
                    g.manifest_hash,
                    None,
                    &g.path,
                )
                .await
                .with_context(|| format!("recovery: opening {} at the destination", g.path))?,
                RecoveryCopy::Held(store) => store.read(set, g).await.with_context(|| {
                    format!("recovery: opening {} in this device's store", g.path)
                })?,
            };
            if source_holds(&held, &scope_hex, &g.path, &plain) {
                report.skipped.push(g.path.clone());
                continue;
            }
            let sealed = seal_blob(&plain, Some((self.nest_root, None)))
                .with_context(|| format!("recovery: sealing {}", g.path))?;
            let pushed = push_sealed(self.source.bytes, &sealed)
                .await
                .with_context(|| format!("recovery: landing {}'s bytes", g.path))?;
            report.chunks_uploaded += pushed.chunks_uploaded;
            report.chunks_deduped += pushed.chunks_deduped;
            // The plaintext size, as every custody writer declares it: the
            // nest derives the charge from the bytes it holds.
            let manifest_hex = hex::encode(sealed.manifest_hash.digest());
            self.source
                .records
                .record_custody(
                    set,
                    &self.source.device_id,
                    &g.path,
                    Some(manifest_hex.clone()),
                    plain.len() as i64,
                    "create",
                )
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .with_context(|| format!("recovery: recording {} on the source", g.path))?;
            report.delivered.push((g.path.clone(), manifest_hex));
        }
        Ok(report)
    }

    /// The nest end's generations: its live custody and its retained window,
    /// walked to completion.
    async fn destination_generations(
        destination: &RecoveryDestination<'_, D>,
        set: &str,
    ) -> Result<Vec<DestGeneration>> {
        let live = crate::audit::read_full_custody(destination.seam)
            .await
            .map_err(|e| anyhow::anyhow!("recovery: the destination's custody list: {e}"))?;
        let (_grace, retained) = crate::audit::read_full_generations(destination.seam)
            .await
            .map_err(|e| anyhow::anyhow!("recovery: the destination's generation list: {e}"))?;
        let generations: Vec<DestGeneration> = live
            .into_iter()
            .filter(|i| i.folder_name == set)
            .filter_map(|i| {
                Some(DestGeneration {
                    path: i.path?,
                    manifest_hash: parse_manifest_hex(&i.manifest_hash)?,
                    order: (true, i.updated_at),
                })
            })
            .chain(
                retained
                    .into_iter()
                    .filter(|g| g.folder_name == set)
                    .filter_map(|g| {
                        Some(DestGeneration {
                            path: g.path?,
                            manifest_hash: parse_manifest_hex(&g.manifest_hash)?,
                            order: (false, g.superseded_at),
                        })
                    }),
            )
            .collect();
        Ok(generations)
    }

    /// The source's own live segments of each family of `kind`, as its
    /// `fauna.segments.list` reports them — the skip's evidence. A family the
    /// source will not list is simply not skipped: the skip is an economy.
    async fn source_segments(
        &self,
        kind: &str,
        scope_hex: &str,
    ) -> Vec<(SegmentFamily, fauna_protocol::segments::SegmentRef)> {
        let mut out = Vec::new();
        for family in SegmentFamily::PASS_ORDER {
            let Some(tag) = family.serve_kind(kind) else {
                continue;
            };
            let reply: std::result::Result<fauna_protocol::segments::SegmentsListReply, _> = self
                .source
                .nest
                .request(
                    fauna_protocol::segments::KIND_SEGMENTS_LIST,
                    fauna_protocol::segments::SegmentsListRequest {
                        kind: tag.to_string(),
                        actor_id: scope_hex.to_string(),
                        extra: Default::default(),
                    },
                )
                .await;
            if let Ok(reply) = reply {
                out.extend(reply.segments.into_iter().map(|s| (family, s)));
            }
        }
        out
    }

    /// Retire the copy's recovered segment paths the source no longer names,
    /// at whichever end the copy is read.
    async fn retire_unnamed(&self, kind: &str, set: &str) -> Result<Vec<String>> {
        match &self.copy {
            RecoveryCopy::Nest { destination, keys } => {
                self.retire_at_destination(destination, keys, kind, set)
                    .await
            }
            RecoveryCopy::Held(store) => {
                let scope_hex = hex::encode(self.scope_id);
                let named = self.source_named(kind, &scope_hex).await?;
                store.retire_unnamed(kind, set, &scope_hex, &named).await
            }
        }
    }

    /// The store end's evidence for its tombstones: each family's live segment
    /// ids as the source lists them **after** the verb — read strictly, since
    /// a family read as empty would retire the whole held copy of it.
    async fn source_named(
        &self,
        kind: &str,
        scope_hex: &str,
    ) -> Result<Vec<(SegmentFamily, HashSet<u32>)>> {
        let mut out = Vec::new();
        for family in SegmentFamily::PASS_ORDER {
            let Some(tag) = family.serve_kind(kind) else {
                continue;
            };
            let reply: fauna_protocol::segments::SegmentsListReply = self
                .source
                .nest
                .request(
                    fauna_protocol::segments::KIND_SEGMENTS_LIST,
                    fauna_protocol::segments::SegmentsListRequest {
                        kind: tag.to_string(),
                        actor_id: scope_hex.to_string(),
                        extra: Default::default(),
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("recovery: the source's {tag} listing: {e}"))?;
            out.push((
                family,
                reply.segments.iter().map(|s| s.segment_id).collect(),
            ));
        }
        Ok(out)
    }

    /// Tombstone at the destination the segment paths its live ledger does not
    /// name, for both families — so the copy stops charging for what the source
    /// now holds under new ids, and re-uploads by its next pass. Through the
    /// same reserved-set arm, so each tombstone is retained `T` and reversible
    /// by `generation.restore`.
    async fn retire_at_destination(
        &self,
        destination: &RecoveryDestination<'_, D>,
        keys: &FileDownloadKeys,
        kind: &str,
        set: &str,
    ) -> Result<Vec<String>> {
        let scope_hex = hex::encode(self.scope_id);
        let live = crate::audit::read_full_custody(destination.seam)
            .await
            .map_err(|e| anyhow::anyhow!("recovery: re-reading the destination's custody: {e}"))?;
        let live: Vec<_> = live.into_iter().filter(|i| i.folder_name == set).collect();
        let mut retired = Vec::new();
        for family in SegmentFamily::PASS_ORDER {
            if family.serve_kind(kind).is_none() {
                continue;
            }
            let mirror_path = family.mirror_path(&scope_hex, kind);
            let Some(ledger) = live
                .iter()
                .find(|i| i.path.as_deref() == Some(mirror_path.as_str()))
            else {
                continue;
            };
            let hash = parse_manifest_hex(&ledger.manifest_hash)
                .ok_or_else(|| anyhow::anyhow!("recovery: the destination's ledger hash"))?;
            let bytes =
                download_file_bytes_by_manifest(destination.bytes, keys, hash, None, &mirror_path)
                    .await
                    .context("recovery: opening the destination's live ledger")?;
            let ledger = LiveManifestMirror::from_bytes(&bytes)
                .context("recovery: decoding the destination's live ledger")?;
            let named: HashSet<u32> = ledger.live.iter().map(|s| s.segment_id).collect();
            for item in &live {
                let Some(path) = item.path.as_deref() else {
                    continue;
                };
                let Some((id, _half)) = family.parse(&scope_hex, path) else {
                    continue;
                };
                if named.contains(&id) {
                    continue;
                }
                destination
                    .records
                    .record_custody(set, &destination.device_id, path, None, 0, "delete")
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))
                    .with_context(|| format!("recovery: retiring {path} at the destination"))?;
                retired.push(path.to_string());
            }
        }
        Ok(retired)
    }
}

/// On wasm nothing is `Send`, so the leg is the driver's seam for any
/// transport; natively the impl is the sync engine's, over a concrete one
/// (module docs, *Send, per target*).
#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl<P, R, C, D> crate::recover::RecoveryLeg for RecoveryDelivery<'_, P, R, C, D>
where
    P: BlobPushSink,
    R: RpcRequester,
    C: CustodyRecorder,
    D: CustodyRecorder,
    R::Error: fauna_protocol::RpcErrorClass,
{
    async fn recover_set(&self, set_name: &str) -> LegAnswer {
        RecoveryDelivery::recover_set(self, set_name).await
    }
}

/// Does the source already hold this segment half byte-identical?
fn source_holds(
    held: &[(SegmentFamily, fauna_protocol::segments::SegmentRef)],
    scope_hex: &str,
    path: &str,
    plain: &[u8],
) -> bool {
    let got = hex::encode(blake3::hash(plain).as_bytes());
    held.iter()
        .any(|(family, seg)| match family.parse(scope_hex, path) {
            Some((id, SegmentHalf::Dat)) => id == seg.segment_id && seg.blake3_hex == got,
            Some((id, SegmentHalf::Meta)) => id == seg.segment_id && seg.meta_blake3_hex == got,
            None => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(path: &str, live: bool, stamp: i64) -> DestGeneration {
        DestGeneration {
            path: path.to_string(),
            manifest_hash: ContentHash::from_digest_raw([0u8; 32]),
            order: (live, stamp),
        }
    }

    /// The walk lands a family's segments by id, `.dat` before `.meta`, a
    /// path's retained generations before its live one, then every mirror
    /// last — and leaves out what is not the kind's corpus.
    #[test]
    fn the_walk_lands_segments_first_and_every_mirror_last() {
        let scope = hex::encode([7u8; 32]);
        let gens = vec![
            generation(&format!("{scope}/manifest.mail"), true, 9),
            generation(&format!("{scope}/seg-00000002.meta"), true, 5),
            generation(&format!("{scope}/placement/seg-00000001.dat"), true, 3),
            generation(&format!("{scope}/seg-00000002.dat"), true, 6),
            generation(&format!("{scope}/seg-00000002.dat"), false, 1),
            generation(&format!("{scope}/placement/manifest.mail"), true, 8),
            generation("elsewhere/seg-00000001.dat", true, 1),
        ];
        let order: Vec<(String, bool)> = segment_set_order("mail", &scope, &gens)
            .into_iter()
            .map(|g| (g.path.trim_start_matches(&scope).to_string(), g.order.0))
            .collect();
        assert_eq!(
            order,
            vec![
                ("/seg-00000002.dat".to_string(), false),
                ("/seg-00000002.dat".to_string(), true),
                ("/seg-00000002.meta".to_string(), true),
                ("/placement/seg-00000001.dat".to_string(), true),
                ("/manifest.mail".to_string(), true),
                ("/placement/manifest.mail".to_string(), true),
            ]
        );
    }
}
