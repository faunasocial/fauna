//! Typed-call wrapper for the `fauna.filesync.snapshot.*` WS-RPC kinds —
//! the message-kind snapshot surface clients hit from the Backups page:
//! the restore-snapshot picker (`list`), restore history + divergence
//! reads (`list_restore_history` / `list_restore_divergence`), and the
//! restore action (`restore_message_kind`).
//!
//! First consumed by the Linux app (`apps/fauna-linux`); the other 5
//! clients lift this crate rather
//! than reimplement the kind-composition (priority #1/#2). The **folder
//! snapshot CRUD** surface (`create_folder` / `get` / `delete` /
//! `undelete` / `prune` / `check` / `diff`, plus the `list` `folder`
//! mode) is the Track B15 WS-RPC twin of the legacy `/api/v1/snapshots/*`
//! HTTP; web adopted it first. Only the
//! snapshot **byte downloads** (ZIP-archive restore + single-file
//! download) stay HTTP residue (`api-layers.md` § Snapshots).
//!
//! Pattern: same shape as `fauna-client-bridges` — a thin
//! `SnapshotsClient<R: RpcRequester>`, one async method per kind, no
//! state machine, wasm-clean (no `fauna-client` dependency).

use fauna_core::label_custody::{self, LabelCustody};
use fauna_core::path_crypto::{LabelField, SealedLabelRender};
use fauna_protocol::ByteBuf;
use fauna_protocol::RpcRequester;
use fauna_protocol::filesync::{
    SnapshotCheckReply, SnapshotCheckRequest, SnapshotCreateFolderReply,
    SnapshotCreateFolderRequest, SnapshotDeleteImmediateReply, SnapshotDeleteImmediateRequest,
    SnapshotDeleteReply, SnapshotDeleteRequest, SnapshotDiffReply, SnapshotDiffRequest,
    SnapshotGetReply, SnapshotGetRequest, SnapshotListReply, SnapshotListRequest,
    SnapshotPruneReply, SnapshotPruneRequest, SnapshotPruneSetPolicyReply,
    SnapshotPruneSetPolicyRequest, SnapshotRestoreDivergenceListReply,
    SnapshotRestoreDivergenceListRequest, SnapshotRestoreHistoryListReply,
    SnapshotRestoreHistoryListRequest, SnapshotRestoreMessageKindReply,
    SnapshotRestoreMessageKindRequest, SnapshotRetentionPolicy, SnapshotStampLabelsReply,
    SnapshotStampLabelsRequest, SnapshotUndeleteReply, SnapshotUndeleteRequest,
};
use fauna_protocol::filesync::{
    SnapshotDiffEntry, SnapshotDiffSummary, SnapshotFileEntry, SnapshotModifiedEntry,
};

/// A **rendered** snapshot diff — [`SnapshotDiffReply`] after the sealed-path
/// and set-name renders, the shape every app's diff view consumes.
///
/// The wire reply names its set unconditionally (`SnapshotDiffReply::folder`
/// is required); `folder` here is this reader's *rendered* view of that name,
/// through `label_custody::render_set_name` like every other set-name-carrying
/// surface: `None` is `SealedLabelRender::Omit` — the reader can open neither
/// the seal (withheld from a non-audience reader, or this seat holds no key)
/// nor a plaintext (the scrubbed empty-string sentinel) — the ratified degrade,
/// on a single top-level name rather than a list row, so nothing is dropped.
/// The three entry lists render in place (`render_paths`), and `summary` is
/// the nest's, untouched by any omission (it describes the diff, not what this
/// reader may see of it).
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotDiff {
    pub snapshot_a: i64,
    pub snapshot_b: i64,
    pub added: Vec<SnapshotDiffEntry>,
    pub removed: Vec<SnapshotDiffEntry>,
    pub modified: Vec<SnapshotModifiedEntry>,
    pub summary: SnapshotDiffSummary,
    /// The set's rendered name; `None` when this reader cannot name it.
    pub folder: Option<String>,
}

pub use fauna_protocol::filesync;

/// What can go wrong on a read whose caller will **write the reply's files to
/// disk** ([`SnapshotsClient::get_for_restore`]).
///
/// Generic over the transport error so this crate stays transport-agnostic (and
/// wasm-clean) exactly as [`RpcRequester`] does; each consumer maps it into its
/// own error surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreReadError<E> {
    /// The `fauna.filesync.snapshot.get` call itself failed.
    Rpc(E),
    /// The reply reached us whole and this client could not render every row —
    /// so a restore driven from it would silently write less than the snapshot
    /// holds. Almost always an un-wired label custody at the construction seam
    /// (`docs/goal/behavior/path-sealing.md` § THE CONSUMER-WIRING RULE).
    RowsOmitted { rendered: usize, file_count: i64 },
}

impl<E: std::fmt::Display> std::fmt::Display for RestoreReadError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc(e) => write!(f, "{e}"),
            // Names the fix, not just the symptom: whoever reads this in a log
            // is one `with_label_custody` away from a working restore, and the
            // counts tell them immediately whether they lost some rows or all.
            Self::RowsOmitted {
                rendered,
                file_count,
            } => write!(
                f,
                "snapshot holds {file_count} files but only {rendered} could be rendered by this \
                 client — {} dropped. Restoring would silently write less than the snapshot \
                 holds, so it is refused. This client is missing its label custody: wire \
                 `with_label_custody` at the seam that builds it (the owner `BackupKey`, and a \
                 folder key resolver for shared sets)",
                *file_count - *rendered as i64
            ),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for RestoreReadError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rpc(e) => Some(e),
            Self::RowsOmitted { .. } => None,
        }
    }
}

/// The three snapshot row shapes that carry a user-chosen path plus its sealed
/// sibling and convergent salt, unified so the sealed-first render is written
/// once (`SnapshotsClient::render_paths`) rather than three times.
///
/// Private: it exists to deduplicate this crate's own render, not to become a
/// wire concept. The wire types stay plain data.
trait SealedPathRow {
    fn path(&self) -> &str;
    fn set_path(&mut self, path: String);
    fn path_sealed(&self) -> Option<&[u8]>;
    fn path_hash(&self) -> Option<&[u8]>;
}

/// `Option<ByteBuf>::as_deref()` yields `Option<&Vec<u8>>`, not `Option<&[u8]>`
/// — the reslice is the fix (it bit S2 twice).
macro_rules! sealed_path_row {
    ($ty:ty) => {
        impl SealedPathRow for $ty {
            fn path(&self) -> &str {
                &self.path
            }
            fn set_path(&mut self, path: String) {
                self.path = path;
            }
            fn path_sealed(&self) -> Option<&[u8]> {
                self.path_sealed.as_ref().map(|b| &b[..])
            }
            fn path_hash(&self) -> Option<&[u8]> {
                self.path_hash.as_ref().map(|b| &b[..])
            }
        }
    };
}

sealed_path_row!(SnapshotFileEntry);
sealed_path_row!(SnapshotDiffEntry);
sealed_path_row!(SnapshotModifiedEntry);

/// Report of one [`SnapshotsClient::backfill_tag_seals`] pass (S8 D3).
/// All-zero = converged, the steady state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TagSealBackfillReport {
    /// Snapshots whose `tags_sealed` was stamped (or axis-re-stamped).
    pub stamped: usize,
    /// Tagged snapshots this custody could not mint a seal for (keyless, or
    /// bound-without-keys — fail closed, never the owner root). Non-zero is
    /// not an error: another audience client converges them.
    pub unsealable: usize,
    /// Per-row stamp submissions that failed (transient) — the pass reruns.
    pub stamp_failures: usize,
}

/// Typed `fauna.filesync.snapshot.*` call surface, generic over the WS-RPC
/// transport (`R: RpcRequester`): native call sites pass `Arc<NestClient>`,
/// the wasm SPA passes its `WsRpcClient`. The kind-composition logic is
/// written once here and shared across native + wasm (priority #2). Errors
/// propagate as the transport's `R::Error`.
pub struct SnapshotsClient<R: RpcRequester> {
    nest: R,
    /// Label-opening custody for the sealed-first render on [`Self::get`] and
    /// [`Self::diff`]. Empty by default, which renders from the plaintext path
    /// (a keyless writer, a public-audience folder) — so every existing call site
    /// keeps compiling and behaving identically, and a platform opts in by
    /// building with [`Self::with_label_custody`].
    custody: LabelCustody,
}

impl<R: RpcRequester> SnapshotsClient<R> {
    pub fn new(nest: R) -> Self {
        Self {
            nest,
            custody: LabelCustody::default(),
        }
    }

    /// Wire the reader's label custody, so snapshot browse and diff render file
    /// paths **sealed-first** (`docs/goal/behavior/file-sync.md` § Sealed names
    /// & paths).
    ///
    /// Builder-style rather than a `new` parameter because this crate is
    /// constructed ad hoc at ~20 call sites across linux, tui, the FFI mirror
    /// and the wasm mirror; an added parameter would be a signature sweep across
    /// all of them for a capability most of those sites (delete, prune, restore
    /// history) never use.
    ///
    /// Without this, a row whose plaintext has been scrubbed is unrenderable and
    /// omitted — the ratified degrade, not an error.
    pub fn with_label_custody(mut self, custody: LabelCustody) -> Self {
        self.custody = custody;
        self
    }

    /// The wired custody, read-only. Exists so a *consumer* can pin its own
    /// wiring at the construction seam (a custody-shape pin in
    /// `fauna-core` cannot observe a caller quietly downgrading to
    /// `owner_only` — the regression pin must read the custody this client will
    /// actually seal and render with).
    pub fn label_custody(&self) -> &LabelCustody {
        &self.custody
    }

    /// Render every entry's `path` sealed-first, in place, dropping the rows
    /// this reader can open neither half of.
    ///
    /// The omission is the ratified degrade (*omit from the listing, re-enter on
    /// re-record*) — never an empty name and never a failed page. Note the
    /// snapshot's own `file_count` / `total_bytes` are deliberately **left
    /// alone**: they describe what the snapshot holds and what a restore will
    /// write, which does not change because this particular reader cannot render
    /// some names. Shrinking them would misreport the restore.
    ///
    /// `folder` / `folder_hash` are the reply's set-name pair: custody resolves
    /// by the hash, so a bound set's paths keep rendering once the plaintext
    /// scrubs ([`LabelCustody::keys_for_row`]).
    async fn render_paths<T: SealedPathRow>(
        &self,
        folder: &str,
        folder_hash: Option<&[u8]>,
        entries: &mut Vec<T>,
    ) {
        // The ONLY safe skip: nothing on this page is sealed (a plaintext-resting
        // plane, or a row from one of the keyless writer seams). Deliberately NOT
        // "custody is empty" — a keyless reader meeting a sealed-only row must
        // reach `Omit`, and skipping would render its blank plaintext as the
        // name. See `LabelCustody::keys_for`.
        if entries.iter().all(|e| e.path_sealed().is_none()) {
            return;
        }
        let (keys, _) = self.custody.keys_for_row(folder, folder_hash).await;
        entries.retain_mut(|entry| {
            let rendered = label_custody::render_path(
                &keys,
                entry.path_sealed(),
                entry.path(),
                entry.path_hash(),
                LabelField::SyncChangePath,
            );
            match rendered {
                SealedLabelRender::Sealed(path) => {
                    entry.set_path(path);
                    true
                }
                // Already the plaintext we were handed — nothing to rewrite.
                SealedLabelRender::Plaintext(_) => true,
                SealedLabelRender::Omit => false,
            }
        });
    }

    /// `fauna.filesync.snapshot.list` — list snapshots, newest first.
    /// Two modes (the unified shape, one method per the one kind):
    ///
    /// - `folder == None` — owner-implicit message-kind list of the
    ///   bearer's snapshots; `message_kind` filters to one kind
    ///   (`"mail"` / `"calendar"`), `None` lists all. Backs the Backups
    ///   `restore-snapshot-select` local-snapshot picker.
    /// - `folder == Some(name)` — the folder-scoped list (Track B15
    ///   fold-in of `GET /api/v1/snapshots?folder=`); `message_kind` is
    ///   ignored. Backs the Backups folder snapshot table.
    ///
    /// `limit == 0` → server default. Replay-safe pure read.
    pub async fn list(
        &self,
        message_kind: Option<String>,
        folder: Option<String>,
        limit: u32,
    ) -> Result<SnapshotListReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.list",
                fauna_protocol::folders::addressed(SnapshotListRequest {
                    message_kind,
                    folder,
                    limit,
                    ..Default::default()
                }),
            )
            .await
    }

    /// The S8 D3 **snapshot-tag seal backfill** for one folder: walk the
    /// set's snapshots and stamp `tags_sealed` where the dual-write plaintext
    /// still rests unsealed — or re-stamp where the resting seal is on the
    /// wrong ROOT AXIS (an owner-root `gen: None` seal on a snapshot of a
    /// *bound* set — residue from an earlier fix's window: no roster member could open
    /// it, and the flip would scrub the plaintext out from under them).
    ///
    /// The axis is the ONLY re-stamp trigger: a seal stamped under an older
    /// generation number stays — `keys_for` chains prior generations, so it
    /// still opens for every member (re-stamping it would be pure churn). An
    /// unparseable envelope re-stamps (it opens for nobody).
    ///
    /// ⚠ Since the **nest enforces the resting half of this
    /// predicate itself** — a seal whose envelope already names a generation is
    /// frozen — and does the write as a compare-and-swap. The nest never looks
    /// at the incoming bytes (that would break envelope-revision compat), so
    /// this side stays the sole judge of the *incoming* root; widening
    /// `needs_stamp` to re-stamp a generation-bearing seal would start bouncing
    /// as `invalid_request`.
    ///
    /// Snapshots are immutable, so this stamp kind is the plane's ONLY
    /// catch-up path (S6-d: "no later pass to stamp it from" — this is that
    /// pass, via the one new wire kind the S8 design licenses). Keyless or
    /// bound-unresolvable custody stamps nothing — fail closed, never the
    /// owner root; another audience client converges the set. The refusal's
    /// mechanism: a bound-but-unresolvable resolve keeps
    /// its `mls_group_id`, so `label_seal_root()` bails below — before that
    /// fix the resolver collapsed the case to "unbound" and this pass stamped
    /// every such snapshot under the owner root despite this very sentence; a
    /// resolve *failure* likewise yields no keys, never the owner fallback.
    pub async fn backfill_tag_seals(
        &self,
        folder: &str,
    ) -> Result<TagSealBackfillReport, R::Error> {
        let mut report = TagSealBackfillReport::default();
        let reply = self.list(None, Some(folder.to_string()), 0).await?;
        // One root per set — every row seals under the same custody.
        let (keys, _) = self.custody.keys_for(folder).await;
        let Ok(Some(root)) = keys.label_seal_root() else {
            // Keyless (nothing to stamp with) or bound-without-keys (sealing
            // under the owner root would be exactly the mistake).
            report.unsealable = reply
                .rows
                .iter()
                .filter(|r| r.tags.as_ref().is_some_and(|t| !t.is_empty()))
                .count();
            return Ok(report);
        };
        let root_is_generation = root.generation().is_some();
        for row in &reply.rows {
            let Some(tags) = row.tags.as_ref().filter(|t| !t.is_empty()) else {
                continue;
            };
            let needs_stamp = match row.tags_sealed.as_deref() {
                None => true,
                Some(sealed) => {
                    root_is_generation
                        && fauna_core::path_crypto::SealedLabel::from_bytes(sealed)
                            .map(|e| e.generation.is_none())
                            .unwrap_or(true)
                }
            };
            if !needs_stamp {
                continue;
            }
            let Some(sealed) = self.seal_tags(folder, tags).await else {
                report.unsealable += 1;
                continue;
            };
            match self
                .nest
                .request::<_, SnapshotStampLabelsReply>(
                    "fauna.filesync.snapshot.stamp_labels",
                    SnapshotStampLabelsRequest {
                        snapshot_id: row.id,
                        tags_sealed: sealed,
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(_) => report.stamped += 1,
                // Best-effort per row — the pass reruns at the next start.
                Err(_) => report.stamp_failures += 1,
            }
        }
        Ok(report)
    }

    /// `fauna.filesync.snapshot.list_restore_history` — owner-implicit
    /// list of the bearer's `restore_history` rows, newest first.
    /// Replay-safe pure read. `limit == 0` → server default.
    pub async fn list_restore_history(
        &self,
        limit: u32,
    ) -> Result<SnapshotRestoreHistoryListReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.list_restore_history",
                SnapshotRestoreHistoryListRequest {
                    limit,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.filesync.snapshot.list_restore_divergence` — owner-only
    /// list of the divergence rows recorded against one snapshot's
    /// restore. The nest checks the bearer owns the snapshot. Replay-safe
    /// pure read; backs the Backups restore-divergence banner + modal.
    pub async fn list_restore_divergence(
        &self,
        snapshot_id: i64,
    ) -> Result<SnapshotRestoreDivergenceListReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.list_restore_divergence",
                SnapshotRestoreDivergenceListRequest {
                    snapshot_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.filesync.snapshot.restore_message_kind` — owner-only
    /// message-kind restore. `confirm_id` must equal `snapshot_id`
    /// stringified (the re-type friction bar); a mismatch returns
    /// `fauna.filesync.snapshot.confirm_mismatch` without mutating state.
    /// Replays the pinned placement manifest into `bridge_imap_*` /
    /// `bridge_caldav_*` and rebuilds `segment_records`. The reply's
    /// `config_present` / `note` warn when bridge AUTH will fail until
    /// the account's custody is restored.
    pub async fn restore_message_kind(
        &self,
        snapshot_id: i64,
        confirm_id: impl Into<String>,
    ) -> Result<SnapshotRestoreMessageKindReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.restore_message_kind",
                SnapshotRestoreMessageKindRequest {
                    snapshot_id,
                    confirm_id: confirm_id.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    // ── folder snapshot CRUD (Track B15) ──────────────────────────
    //
    // The WS-RPC twins of `/api/v1/snapshots/*`. `User | Admin`,
    // file_set-name / snapshot-id scoped (not owner-implicit). The byte
    // downloads (ZIP restore + single-file) stay HTTP residue — no twin.

    /// Seal a snapshot's tag list for the wire's `tags_sealed` sibling — the
    /// write half of `docs/goal/behavior/file-sync.md` § Sealed names & paths for
    /// `snapshots.tags` (path-sealing S6-d).
    ///
    /// **This gesture is the only writer of that column that will ever hold a
    /// key.** The nest mints the snapshot row and holds nothing that could seal
    /// it, and a snapshot — unlike a folder row — has no later bind/serve pass
    /// to stamp it from, so there is no catch-up stamp behind this. Seal here or
    /// the tags are lost at the flip.
    ///
    /// `None` = nothing to seal: no tags, or a keyless client (no
    /// [`Self::with_label_custody`]), or a set this reader holds no seal root for.
    /// **Best-effort by design** — a derivation failure must not fail the user's
    /// backup; the row lands plaintext-only as an S8 backfill row, exactly like
    /// the device-label and selective-sync seams.
    async fn seal_tags(&self, folder: &str, tags: &[String]) -> Option<ByteBuf> {
        if tags.is_empty() {
            return None;
        }
        let (keys, _) = self.custody.keys_for(folder).await;
        let root = keys.label_seal_root().ok().flatten()?;
        label_custody::seal_snapshot_tags(&root, folder, tags)
            .ok()
            .map(ByteBuf::from)
    }

    /// `fauna.filesync.snapshot.create_folder` — capture a point-in-time
    /// snapshot of a synced folder. `tags` empty + `device_id` `None`
    /// for the unattributed client-driven capture (the HTTP twin's bare
    /// `{ folder }` body).
    ///
    /// Tags ride **sealed** beside the plaintext when this client holds custody
    /// ([`Self::seal_tags`]); the signature is unchanged so no call site moves.
    pub async fn create_folder(
        &self,
        folder: impl Into<String>,
        tags: Vec<String>,
        device_id: Option<Vec<u8>>,
    ) -> Result<SnapshotCreateFolderReply, R::Error> {
        let folder = folder.into();
        let tags_sealed = self.seal_tags(&folder, &tags).await;
        self.nest
            .request(
                "fauna.filesync.snapshot.create_folder",
                fauna_protocol::folders::addressed(SnapshotCreateFolderRequest {
                    folder,
                    tags,
                    device_id: device_id.map(ByteBuf::from),
                    tags_sealed,
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.filesync.snapshot.get` — snapshot metadata + file listing
    /// (folder or message-kind row). Replay-safe pure read.
    pub async fn get(&self, snapshot_id: i64) -> Result<SnapshotGetReply, R::Error> {
        let mut reply: SnapshotGetReply = self
            .nest
            .request(
                "fauna.filesync.snapshot.get",
                SnapshotGetRequest {
                    snapshot_id,
                    extra: Default::default(),
                },
            )
            .await?;
        // Sealed-first, at ingest — the reply names its own folder, so custody
        // resolves once for the whole listing.
        let folder = reply.folder.clone();
        let folder_hash = reply.folder_hash.clone();
        self.render_paths(
            &folder,
            folder_hash.as_deref().map(|b| &b[..]),
            &mut reply.files,
        )
        .await;
        // The tag display copy, same posture (path-sealing S6-d). Salted by the
        // reply's `folder_hash` so it keeps rendering once `folder` scrubs.
        // Only when something is actually sealed — the same "nothing on this page
        // is sealed" guard `render_paths` uses, and for the same reason: a
        // keyless reader meeting a sealed-only row must reach `Omit` rather than
        // have the render skipped and show a blank list as the truth.
        if reply.tags_sealed.is_some() {
            let (keys, _) = self
                .custody
                .keys_for_row(&folder, reply.folder_hash.as_ref().map(|b| &b[..]))
                .await;
            reply.tags = label_custody::render_snapshot_tags(
                &keys,
                reply.tags_sealed.as_ref().map(|b| &b[..]),
                Some(&reply.tags),
                &folder,
                reply.folder_hash.as_ref().map(|b| &b[..]),
            )
            .unwrap_or_default();
        }
        Ok(reply)
    }

    /// [`Self::get`] for a caller that is about to **write these files to
    /// disk** — the same read, with the omission treated as the defect it is
    /// there rather than the ratified degrade it is on a listing.
    ///
    /// **Why a second method instead of hardening `get`:** dropping a row this
    /// reader cannot open is *correct* for a listing (§ `render_paths` — a
    /// roster member browsing a set they hold only some keys for should see the
    /// rows they can, never an error page). It is never correct for a restore:
    /// the caller is about to materialise the reply onto a filesystem, so a row
    /// silently missing from it is data the user asked for and did not get.
    /// Same read, two audiences, and only the restore audience wants a hard
    /// failure — so the strictness rides the *call site*, which knows which one
    /// it is.
    ///
    /// **Why `file_count` is a trustworthy witness** (checked nest-side
    /// 2026-08-03): it is stamped at capture as `files.len()` over the very rows
    /// the snapshot stores (`bins/fauna-nest/src/db/sync_storage.rs`, both
    /// `create_snapshot*` paths) and `get_snapshot_files` is unpaginated
    /// (`bins/fauna-nest/src/filesync_handlers.rs`), so on the wire
    /// `reply.files.len() == reply.file_count` **always**. Any shortfall a
    /// client observes was introduced on *this* side of the socket, by
    /// [`Self::render_paths`] dropping what it could not open. That is also why
    /// `render_paths` deliberately leaves `file_count` alone — it describes what
    /// the snapshot holds and what a restore owes, which is exactly the property
    /// this check needs.
    ///
    /// Compares `<`, not `!=`: only the data-loss direction is a defect, and a
    /// hypothetical future reply carrying *more* rows than its count must not
    /// break every restore.
    ///
    /// This is the **sink** for the consumer-wiring rule
    /// (`docs/goal/behavior/path-sealing.md` § THE CONSUMER-WIRING RULE): wiring
    /// each seam fixes today's instances, but a seam added tomorrow regrows the
    /// class silently. Routed through here it cannot — the next unwired restore
    /// consumer gets a loud error naming exactly what was dropped, instead of an
    /// empty directory and `Ok`.
    pub async fn get_for_restore(
        &self,
        snapshot_id: i64,
    ) -> Result<SnapshotGetReply, RestoreReadError<R::Error>> {
        let reply = self.get(snapshot_id).await.map_err(RestoreReadError::Rpc)?;
        let rendered = reply.files.len();
        if (rendered as i64) < reply.file_count {
            return Err(RestoreReadError::RowsOmitted {
                rendered,
                file_count: reply.file_count,
            });
        }
        Ok(reply)
    }

    /// `fauna.filesync.snapshot.delete` — queue a 48 h soft-delete pending
    /// action. `hard_floor_breach` if it would drop below 3 active.
    /// Distinct from the owner-only immediate delete.
    pub async fn delete(&self, snapshot_id: i64) -> Result<SnapshotDeleteReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.delete",
                SnapshotDeleteRequest {
                    snapshot_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.filesync.snapshot.undelete` — recover a soft-deleted
    /// snapshot before GC. `not_soft_deleted` otherwise.
    pub async fn undelete(&self, snapshot_id: i64) -> Result<SnapshotUndeleteReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.undelete",
                SnapshotUndeleteRequest {
                    snapshot_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.filesync.snapshot.delete_immediate` — owner-only immediate
    /// snapshot delete (spec D11; `backups.md` § User actions). Skips the
    /// 48 h soft-delete window; still enforces the hard floor of 3 active
    /// snapshots per folder (`hard_floor_breach`). Both `confirm_id` (the
    /// snapshot id retyped as a string) and `acknowledge` (exactly
    /// [`filesync::IMMEDIATE_DELETE_ACK_TEXT`]) must match or the nest
    /// returns `confirm_mismatch` / `acknowledge_mismatch` with no state
    /// mutation — the friction bar the Backups immediate-delete modal
    /// enforces client-side before enabling its confirm button.
    pub async fn delete_immediate(
        &self,
        snapshot_id: i64,
        confirm_id: impl Into<String>,
        acknowledge: impl Into<String>,
    ) -> Result<SnapshotDeleteImmediateReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.delete_immediate",
                SnapshotDeleteImmediateRequest {
                    snapshot_id,
                    confirm_id: confirm_id.into(),
                    acknowledge: acknowledge.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.filesync.snapshot.prune` — apply a retention policy.
    /// `dry_run` reports candidates without deleting.
    pub async fn prune(
        &self,
        folder: impl Into<String>,
        dry_run: bool,
        policy: SnapshotRetentionPolicy,
    ) -> Result<SnapshotPruneReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.prune",
                fauna_protocol::folders::addressed(SnapshotPruneRequest {
                    folder: folder.into(),
                    dry_run,
                    policy,
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.filesync.snapshot.prune_set_policy` — apply **this set's own
    /// resting** retention policy, preview then execute.
    ///
    /// This, not [`Self::prune`], is what the Backups page's
    /// `snapshot-prune-button` speaks (`ui/backups.md` § Snapshot-list shape,
    /// *Prune* ruling): the button means *"apply what the wizard recorded for
    /// this set"*, and there is deliberately **no policy parameter** — a
    /// client-supplied one is exactly the drift the kind exists to end (five
    /// apps had invented five different policies for one button). `prune`
    /// remains the surface for a caller that genuinely carries its own policy.
    ///
    /// The reply's `policy_state` is load-bearing: `not_set` and `unparseable`
    /// both prune nothing, and a page must render *why* the count is zero
    /// rather than an empty success.
    pub async fn prune_set_policy(
        &self,
        folder: impl Into<String>,
        dry_run: bool,
    ) -> Result<SnapshotPruneSetPolicyReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.prune_set_policy",
                fauna_protocol::folders::addressed(SnapshotPruneSetPolicyRequest {
                    folder: folder.into(),
                    dry_run,
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.filesync.snapshot.check` — integrity scan over the blob
    /// store for one folder. `backup_unavailable` if no backup service.
    pub async fn check(
        &self,
        folder: impl Into<String>,
        verify_content: bool,
    ) -> Result<SnapshotCheckReply, R::Error> {
        self.nest
            .request(
                "fauna.filesync.snapshot.check",
                fauna_protocol::folders::addressed(SnapshotCheckRequest {
                    folder: folder.into(),
                    verify_content,
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.filesync.snapshot.diff` — compare two snapshots of the same
    /// folder → added / removed / modified, rendered ([`SnapshotDiff`]).
    /// `invalid_request` if they belong to different folders.
    pub async fn diff(&self, a: i64, b: i64) -> Result<SnapshotDiff, R::Error> {
        let mut reply: SnapshotDiffReply = self
            .nest
            .request(
                "fauna.filesync.snapshot.diff",
                SnapshotDiffRequest {
                    a,
                    b,
                    extra: Default::default(),
                },
            )
            .await?;
        // A diff is always within ONE folder (the nest rejects a cross-set
        // pair), and the reply always names it — post-scrub as the
        // empty-string sentinel, which is why custody resolves by the reply's
        // `folder_hash` (`keys_for_row`) — once for all three lists.
        let folder = std::mem::take(&mut reply.folder);
        let folder_hash = reply.folder_hash.clone();
        let hash = folder_hash.as_deref().map(|b| &b[..]);
        self.render_paths(&folder, hash, &mut reply.added).await;
        self.render_paths(&folder, hash, &mut reply.removed).await;
        self.render_paths(&folder, hash, &mut reply.modified).await;

        // The set-name pair, through the same shared seam as the path renders
        // above (path-sealing S5c-2). `Omit` is `None` on the rendered view
        // rather than a dropped reply — this is a single top-level name, not
        // a list row. Custody resolves by `folder_hash`, so a BOUND set's name
        // still renders once `folder` blanks.
        let (keys, _) = self.custody.keys_for_row(&folder, hash).await;
        let folder = match label_custody::render_set_name(
            &keys,
            reply.folder_sealed.as_ref().map(|b| &b[..]),
            &folder,
            reply.folder_hash.as_ref().map(|b| &b[..]),
        ) {
            SealedLabelRender::Sealed(name) => Some(name),
            SealedLabelRender::Plaintext(name) => Some(name),
            SealedLabelRender::Omit => None,
        };
        Ok(SnapshotDiff {
            snapshot_a: reply.snapshot_a,
            snapshot_b: reply.snapshot_b,
            added: reply.added,
            removed: reply.removed,
            modified: reply.modified,
            summary: reply.summary,
            folder,
        })
    }
}

/// The Backups immediate-delete friction-bar predicate
/// (`immediate-delete-confirm-button`'s enabled flag, `docs/goal/ui/backups.md`
/// Architectural rule 4): both the retyped snapshot id
/// (`immediate-delete-confirm-input`) and the acknowledge phrase
/// (`immediate-delete-acknowledge-input`) must match exactly, and no delete
/// may already be in flight. `target_id` empty means no snapshot is selected
/// (the empty-string-means-none convention this page's other fields already
/// use) — always disabled in that state. Shared so every app computes the
/// identical boolean instead of re-deriving it (priority #2) — five apps
/// (web, windows, android, apple, linux) each hand-rolled this exact
/// four-way `&&` before this fn existed. Do NOT fold `restore-confirm-button`'s
/// sibling predicate in here — `backups.md` § Where logic lives keeps that one
/// per-app glue deliberately.
pub fn immediate_delete_button_enabled(
    deleting: bool,
    confirm_id: &str,
    target_id: &str,
    acknowledge_typed: &str,
) -> bool {
    !deleting
        && !target_id.is_empty()
        && confirm_id == target_id
        && acknowledge_typed == fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT
}

/// A `restore-snapshot-select` option's label: `"{kind} (#{id})"`, kind first
/// so two same-day snapshots are told apart by what they hold rather than by
/// an opaque number. `message_kind` absent (a folder snapshot) reads as an
/// empty kind, never a placeholder. Shared so every app renders the
/// identical text (priority #2) — five apps (linux, tui, android, apple,
/// windows) each hand-rolled this exact one-line format before this fn
/// existed.
pub fn snapshot_restore_option_label(message_kind: Option<&str>, id: i64) -> String {
    format!("{} (#{id})", message_kind.unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = SnapshotsClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // Pin each method's `fauna.filesync.snapshot.*` kind + request payload; the
    // construction-only mock above can't catch a kind typo or a request that
    // stops serializing to the shape the nest handler decodes. Transport-free,
    // mirroring `fauna-client-bridges`'s `RecordingRequester`; real end-to-end
    // round-trip dispatch lives in `tests/e2e-unified/tests/api/test_dr_restore.py`
    // and `test_snapshot_backups.py`.
    use filesync::{
        RestoreDivergenceRow, RestoreHistoryRow, SnapshotDiffSummary, SnapshotSummaryRow,
    };

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.filesync.snapshot.list" => {
                fauna_protocol::encode_canonical(&SnapshotListReply {
                    rows: Vec::<SnapshotSummaryRow>::new(),
                    extra: Default::default(),
                })
            }
            "fauna.filesync.snapshot.list_restore_history" => {
                fauna_protocol::encode_canonical(&SnapshotRestoreHistoryListReply {
                    rows: Vec::<RestoreHistoryRow>::new(),
                    extra: Default::default(),
                })
            }
            "fauna.filesync.snapshot.list_restore_divergence" => {
                fauna_protocol::encode_canonical(&SnapshotRestoreDivergenceListReply {
                    rows: Vec::<RestoreDivergenceRow>::new(),
                    extra: Default::default(),
                })
            }
            "fauna.filesync.snapshot.restore_message_kind" => {
                fauna_protocol::encode_canonical(&SnapshotRestoreMessageKindReply {
                    snapshot_id: 1,
                    kind: "mail".into(),
                    config_present: true,
                    note: String::new(),
                    extra: Default::default(),
                })
            }
            "fauna.filesync.snapshot.delete_immediate" => {
                fauna_protocol::encode_canonical(&SnapshotDeleteImmediateReply {
                    snapshot_id: 42,
                    segment_retention_days: 14,
                    extra: Default::default(),
                })
            }
            "fauna.filesync.snapshot.create_folder" => {
                fauna_protocol::encode_canonical(&SnapshotCreateFolderReply {
                    id: 1,
                    file_count: 0,
                    total_bytes: 0,
                    created_at: 0,
                    parent_id: None,
                    tags: vec![],
                    device_id: None,
                    extra: Default::default(),
                })
            }
            // Struct-update rather than an exhaustive literal, so two
            // branches independently growing this reply merge cleanly instead
            // of colliding on the new field: it grows on the sealing axis
            // every slice or two.
            "fauna.filesync.snapshot.get" => fauna_protocol::encode_canonical(&SnapshotGetReply {
                id: 1,
                folder: "__mail".into(),
                ..Default::default()
            }),
            "fauna.filesync.snapshot.delete" => {
                fauna_protocol::encode_canonical(&SnapshotDeleteReply {
                    pending_action_id: 1,
                    execute_after: 0,
                    status: "pending".into(),
                    extra: Default::default(),
                })
            }
            "fauna.filesync.snapshot.undelete" => {
                fauna_protocol::encode_canonical(&SnapshotUndeleteReply {
                    undeleted: true,
                    extra: Default::default(),
                })
            }
            "fauna.filesync.snapshot.prune" => {
                fauna_protocol::encode_canonical(&SnapshotPruneReply {
                    dry_run: false,
                    pruned: 0,
                    remaining: 0,
                    snapshots: vec![],
                    extra: Default::default(),
                })
            }
            "fauna.filesync.snapshot.prune_set_policy" => {
                fauna_protocol::encode_canonical(&SnapshotPruneSetPolicyReply {
                    policy_state: fauna_protocol::filesync::policy_state::APPLIED.into(),
                    ..Default::default()
                })
            }
            "fauna.filesync.snapshot.check" => {
                fauna_protocol::encode_canonical(&SnapshotCheckReply {
                    status: "ok".into(),
                    snapshots_checked: 0,
                    files_checked: 0,
                    manifests_checked: 0,
                    chunks_checked: 0,
                    missing_manifests: 0,
                    missing_chunks: 0,
                    corrupt_manifests: 0,
                    structured_errors: vec![],
                    extra: Default::default(),
                })
            }
            "fauna.filesync.snapshot.diff" => {
                // Struct-update form on purpose: two branches each
                // growing this reply then merge cleanly instead of
                // colliding on the new field. This literal has already
                // been broken once by exactly that.
                fauna_protocol::encode_canonical(&SnapshotDiffReply {
                    snapshot_a: 1,
                    snapshot_b: 2,
                    ..Default::default()
                })
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.list(Some("mail".into()), None, 10)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.list");
        let req: SnapshotListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.message_kind.as_deref(), Some("mail"));
        assert_eq!(req.limit, 10);
    }

    #[test]
    fn list_restore_history_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.list_restore_history(5)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.list_restore_history");
        let req: SnapshotRestoreHistoryListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.limit, 5);
    }

    #[test]
    fn list_restore_divergence_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.list_restore_divergence(42)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.list_restore_divergence");
        let req: SnapshotRestoreDivergenceListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.snapshot_id, 42);
    }

    #[test]
    fn restore_message_kind_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.restore_message_kind(42, "42")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.restore_message_kind");
        let req: SnapshotRestoreMessageKindRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.snapshot_id, 42);
        assert_eq!(req.confirm_id, "42");
    }

    #[test]
    fn delete_immediate_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.delete_immediate(42, "42", filesync::IMMEDIATE_DELETE_ACK_TEXT))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.delete_immediate");
        let req: SnapshotDeleteImmediateRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.snapshot_id, 42);
        assert_eq!(req.confirm_id, "42");
        assert_eq!(req.acknowledge, filesync::IMMEDIATE_DELETE_ACK_TEXT);
    }

    #[test]
    fn create_folder_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.create_folder("__mail", vec!["nightly".into()], None))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.create_folder");
        let req: SnapshotCreateFolderRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.folder, "__mail");
        assert_eq!(req.tags, vec!["nightly".to_string()]);
    }

    #[test]
    fn get_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.get(7)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.get");
        let req: SnapshotGetRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.snapshot_id, 7);
    }

    #[test]
    fn delete_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.delete(7)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.delete");
        let req: SnapshotDeleteRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.snapshot_id, 7);
    }

    #[test]
    fn undelete_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.undelete(7)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.undelete");
        let req: SnapshotUndeleteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.snapshot_id, 7);
    }

    #[test]
    fn prune_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        let policy = SnapshotRetentionPolicy {
            keep_last: Some(3),
            ..Default::default()
        };
        block_on(client.prune("__mail", true, policy)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.prune");
        let req: SnapshotPruneRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.folder, "__mail");
        assert!(req.dry_run);
        assert_eq!(req.policy.keep_last, Some(3));
    }

    #[test]
    fn check_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.check("__mail", true)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.check");
        let req: SnapshotCheckRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.folder, "__mail");
        assert!(req.verify_content);
    }

    /// The point of the kind: the request carries **no policy field at all**,
    /// so a client cannot smuggle its own bounds past the set's resting ones.
    /// A `decode_strict` into the set-policy shape would still succeed if the
    /// method had been pointed at the explicit kind, so the kind string is
    /// asserted too.
    #[test]
    fn prune_set_policy_composes_kind_and_payload_with_no_policy() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.prune_set_policy("docs", true)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.prune_set_policy");
        let req: SnapshotPruneSetPolicyRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "docs"
        ));
        assert!(req.dry_run);
        assert!(
            req.extra.is_empty(),
            "no policy rides this kind — the nest reads the set's own column"
        );
    }

    #[test]
    fn diff_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.diff(1, 2)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.diff");
        let req: SnapshotDiffRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.a, 1);
        assert_eq!(req.b, 2);
    }

    #[test]
    fn immediate_delete_button_enabled_true_on_exact_match() {
        let ack = fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT;
        assert!(immediate_delete_button_enabled(false, "7", "7", ack));
    }

    #[test]
    fn immediate_delete_button_enabled_false_with_no_target_selected() {
        let ack = fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT;
        assert!(!immediate_delete_button_enabled(false, "", "", ack));
    }

    #[test]
    fn immediate_delete_button_enabled_false_on_id_mismatch() {
        let ack = fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT;
        assert!(!immediate_delete_button_enabled(false, "6", "7", ack));
    }

    #[test]
    fn immediate_delete_button_enabled_false_on_ack_mismatch() {
        assert!(!immediate_delete_button_enabled(
            false,
            "7",
            "7",
            "close enough"
        ));
    }

    #[test]
    fn immediate_delete_button_enabled_false_while_deleting() {
        let ack = fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT;
        assert!(!immediate_delete_button_enabled(true, "7", "7", ack));
    }

    #[test]
    fn snapshot_restore_option_label_kind_first_then_id() {
        assert_eq!(snapshot_restore_option_label(Some("mail"), 7), "mail (#7)");
    }

    #[test]
    fn snapshot_restore_option_label_empty_kind_for_a_folder_snapshot() {
        assert_eq!(snapshot_restore_option_label(None, 12), " (#12)");
    }
    // ── Sealed-first path render (path-sealing S3) ───────────────────────────
    //
    // `docs/goal/behavior/file-sync.md` § Sealed names & paths: snapshot browse
    // and diff render the SEAL first, fall back to the plaintext path, and
    // omit a row they can open neither half of. These drive a real seal with the
    // plaintext deliberately **blanked** — the post-flip shape — through the
    // same `label_custody` seam the media list and conflict list use.

    fn owner_root() -> fauna_core::crypto::BackupKey {
        fauna_core::crypto::BackupKey::from_bytes([5u8; 32])
    }

    /// Seal `path` the way `SyncEngine::seal_recorded_path` does for an
    /// owner-only set: convergent nonce, salted by the path's own hash — via
    /// the same `label_custody::seal_path` funnel production code uses, so
    /// this fixture can't drift from the real sealing recipe.
    fn seal(root: &fauna_core::crypto::BackupKey, path: &str) -> ByteBuf {
        ByteBuf::from(
            fauna_core::label_custody::seal_path(
                &fauna_core::path_crypto::LabelRoot::owner_of(root),
                path,
            )
            .unwrap(),
        )
    }

    fn salt_of(path: &str) -> ByteBuf {
        ByteBuf::from(fauna_core::sync::path_hash(path).to_vec())
    }

    // ── S8 D3: the tag-seal backfill pass ───────────────────────────────────

    /// Serves a configured `snapshot.list` reply and records every
    /// `stamp_labels` request — the backfill pass's whole world.
    struct StampRequester {
        list: SnapshotListReply,
        stamps: std::sync::Mutex<Vec<SnapshotStampLabelsRequest>>,
    }

    impl RpcRequester for StampRequester {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                "fauna.filesync.snapshot.list" => fauna_protocol::encode_canonical(&self.list),
                "fauna.filesync.snapshot.stamp_labels" => {
                    self.stamps
                        .lock()
                        .unwrap()
                        .push(fauna_protocol::decode_strict(&bytes).expect("decode stamp"));
                    fauna_protocol::encode_canonical(&SnapshotStampLabelsReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                other => unreachable!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn tagged_row(id: i64, tags: &[&str], tags_sealed: Option<Vec<u8>>) -> SnapshotSummaryRow {
        SnapshotSummaryRow {
            id,
            tags: (!tags.is_empty()).then(|| tags.iter().map(|t| t.to_string()).collect()),
            tags_sealed: tags_sealed.map(ByteBuf::from),
            ..Default::default()
        }
    }

    fn stamp_client(
        rows: Vec<SnapshotSummaryRow>,
        custody: LabelCustody,
    ) -> SnapshotsClient<std::sync::Arc<StampRequester>> {
        SnapshotsClient::new(std::sync::Arc::new(StampRequester {
            list: SnapshotListReply {
                rows,
                extra: Default::default(),
            },
            stamps: std::sync::Mutex::new(Vec::new()),
        }))
        .with_label_custody(custody)
    }

    /// Owner-only (unbound) custody: an unsealed tagged row is stamped with a
    /// seal the audience opens; an already-sealed row and an untagged row are
    /// both left alone (no gen-axis re-stamp under a non-generation root).
    #[test]
    fn tag_backfill_stamps_unsealed_rows_and_opens_for_the_audience() {
        let owner = fauna_core::crypto::BackupKey::from_bytes([7u8; 32]);
        let resting = seal_tags_under(
            &fauna_core::path_crypto::LabelRoot::owner_of(&owner),
            &["manual"],
        );
        let client = stamp_client(
            vec![
                tagged_row(1, &["manual"], None),
                tagged_row(2, &["manual"], Some(resting)),
                tagged_row(3, &[], None),
            ],
            LabelCustody::owner_only(owner.clone()),
        );

        let report = block_on(client.backfill_tag_seals("docs")).unwrap();
        assert_eq!(report.stamped, 1);
        assert_eq!((report.unsealable, report.stamp_failures), (0, 0));

        let stamps = client.nest.stamps.lock().unwrap();
        assert_eq!(stamps.len(), 1);
        assert_eq!(stamps[0].snapshot_id, 1);
        // The minted seal opens for the audience — the round-trip assert, since
        // the nonce is random and exact bytes are unrecomputable.
        let keys = fauna_core::file_download::FileDownloadKeys::owner(owner);
        assert_eq!(
            label_custody::render_snapshot_tags(
                &keys,
                Some(&stamps[0].tags_sealed[..]),
                None,
                "docs",
                None,
            ),
            Some(vec!["manual".to_string()]),
        );
    }

    fn seal_tags_under(root: &fauna_core::path_crypto::LabelRoot, tags: &[&str]) -> Vec<u8> {
        let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        label_custody::seal_snapshot_tags(root, "docs", &tags).unwrap()
    }

    struct StubResolver {
        /// `None` = bound-but-unresolvable — the cell: the set IS
        /// bound and this custody cannot produce its content keys.
        keys: Option<fauna_core::folder_keys::FolderContentKeys>,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::folder_keys::FolderKeyResolver for StubResolver {
        async fn resolve(
            &self,
            _name_hash: &[u8; 32],
        ) -> anyhow::Result<fauna_core::folder_keys::ResolvedCustody> {
            Ok(fauna_core::folder_keys::ResolvedCustody::ContentKeyed(
                fauna_core::folder_keys::ResolvedFolderKeys {
                    mls_group_id: Some(b"raw-group-id".to_vec()),
                    content_keys: self.keys.clone(),
                    home_nest_url: None,
                    home_nest_actor_id: None,
                },
            ))
        }
    }

    /// Bound custody (generation root): an owner-root `gen: None` seal — residue
    /// from an earlier fix's window — is RE-stamped, while a generation-stamped seal is
    /// left alone (older generations open via the chained key history;
    /// re-stamping them would be pure churn).
    #[test]
    fn tag_backfill_re_stamps_only_the_owner_root_axis_on_a_bound_set() {
        let owner = fauna_core::crypto::BackupKey::from_bytes([7u8; 32]);
        let content = fauna_core::folder_keys::FolderContentKeys::genesis([4u8; 32], 1_000);
        let content_root = fauna_core::path_crypto::LabelRoot::content_key(
            *content.current_key(),
            content.current_version(),
        );
        let lxx_residue = seal_tags_under(
            &fauna_core::path_crypto::LabelRoot::owner_of(&owner),
            &["manual"],
        );
        let correctly_stamped = seal_tags_under(&content_root, &["manual"]);
        let client = stamp_client(
            vec![
                tagged_row(1, &["manual"], Some(lxx_residue)),
                tagged_row(2, &["manual"], Some(correctly_stamped)),
            ],
            LabelCustody::new(
                Some(std::sync::Arc::new(StubResolver {
                    keys: Some(content.clone()),
                })),
                Some(owner),
            ),
        );

        let report = block_on(client.backfill_tag_seals("docs")).unwrap();
        assert_eq!(report.stamped, 1, "only the owner-root-axis row re-stamps");
        let stamps = client.nest.stamps.lock().unwrap();
        assert_eq!(stamps[0].snapshot_id, 1);
        let restamped =
            fauna_core::path_crypto::SealedLabel::from_bytes(&stamps[0].tags_sealed[..]).unwrap();
        assert!(
            restamped.generation.is_some(),
            "the re-stamp is generation-stamped — the axis the residue got wrong"
        );
    }

    /// Keyless custody stamps nothing (fail closed — never the owner root for
    /// a set it cannot judge), and reports the rows it left for a keyed
    /// sibling to converge.
    #[test]
    fn tag_backfill_is_inert_without_keys() {
        let client = stamp_client(
            vec![tagged_row(1, &["manual"], None)],
            LabelCustody::default(),
        );
        let report = block_on(client.backfill_tag_seals("docs")).unwrap();
        assert_eq!(report.stamped, 0);
        assert_eq!(report.unsealable, 1);
        assert!(client.nest.stamps.lock().unwrap().is_empty());
    }

    /// The cell at this site: **bound + owner key + unresolvable
    /// keys** — the one shape the keyless test above cannot reach (it holds no
    /// owner key, so it reaches `Ok(None)` by a different route). The resolver
    /// answers bound-but-unresolvable; `label_seal_root()` bails; nothing is
    /// stamped under the owner root.
    #[test]
    fn tag_backfill_refuses_a_bound_set_with_unresolvable_keys() {
        let owner = fauna_core::crypto::BackupKey::from_bytes([7u8; 32]);
        let client = stamp_client(
            vec![tagged_row(1, &["manual"], None)],
            LabelCustody::new(
                Some(std::sync::Arc::new(StubResolver { keys: None })),
                Some(owner),
            ),
        );
        let report = block_on(client.backfill_tag_seals("docs")).unwrap();
        assert_eq!(
            report.stamped, 0,
            "a bound set with unresolvable keys must not be stamped under the owner root"
        );
        assert_eq!(
            report.unsealable, 1,
            "the row is counted for a keyed sibling to converge"
        );
        assert!(
            client.nest.stamps.lock().unwrap().is_empty(),
            "no stamp request may leave the client — an owner-root seal here would \
             be destroyed-plaintext at the S9 scrub for every roster member"
        );
    }

    /// A requester that answers `get` / `diff` with a caller-prepared reply, so
    /// a test can plant exact sealed rows.
    struct PlantedRequester {
        get: std::sync::Mutex<Option<SnapshotGetReply>>,
        diff: std::sync::Mutex<Option<SnapshotDiffReply>>,
    }

    impl PlantedRequester {
        fn with_get(reply: SnapshotGetReply) -> Self {
            Self {
                get: std::sync::Mutex::new(Some(reply)),
                diff: std::sync::Mutex::new(None),
            }
        }
        fn with_diff(reply: SnapshotDiffReply) -> Self {
            Self {
                get: std::sync::Mutex::new(None),
                diff: std::sync::Mutex::new(Some(reply)),
            }
        }
    }

    impl RpcRequester for PlantedRequester {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = match kind {
                "fauna.filesync.snapshot.get" => {
                    let r = self.get.lock().unwrap().take().expect("one get per test");
                    fauna_protocol::encode_canonical(&r)
                }
                "fauna.filesync.snapshot.diff" => {
                    let r = self.diff.lock().unwrap().take().expect("one diff per test");
                    fauna_protocol::encode_canonical(&r)
                }
                other => unreachable!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&bytes).expect("decode reply"))
        }
    }

    /// A snapshot-browse reply whose one file row is sealed-only (no plaintext).
    fn sealed_get_reply(root: &fauna_core::crypto::BackupKey, path: &str) -> SnapshotGetReply {
        SnapshotGetReply {
            id: 1,
            folder: "docs".into(),
            created_at: 0,
            file_count: 1,
            total_bytes: 10,
            tags: vec![],
            device_id: None,
            files: vec![SnapshotFileEntry {
                path: String::new(),
                manifest_hash: ByteBuf::from(vec![1u8; 32]),
                size_bytes: 10,
                mtime: 0,
                mode: 0,
                file_type: "file".into(),
                symlink_target: None,
                path_hash: Some(salt_of(path)),
                path_sealed: Some(seal(root, path)),
                extra: Default::default(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_get_renders_a_sealed_path_with_the_plaintext_blanked() {
        let root = owner_root();
        let req = std::sync::Arc::new(PlantedRequester::with_get(sealed_get_reply(
            &root,
            "taxes/2026-notice.pdf",
        )));
        let client =
            SnapshotsClient::new(req).with_label_custody(LabelCustody::owner_only(root.clone()));

        let reply = block_on(client.get(1)).expect("infallible mock");

        assert_eq!(reply.files.len(), 1, "the row must survive the render");
        assert_eq!(reply.files[0].path, "taxes/2026-notice.pdf");
    }

    #[test]
    fn snapshot_get_omits_a_sealed_only_row_for_a_keyless_reader() {
        let root = owner_root();
        let req = std::sync::Arc::new(PlantedRequester::with_get(sealed_get_reply(
            &root,
            "taxes/2026-notice.pdf",
        )));
        // No custody wired — a keyless reader, or a reader outside the set.
        let client = SnapshotsClient::new(req);

        let reply = block_on(client.get(1)).expect("infallible mock");

        assert!(
            reply.files.is_empty(),
            "the ratified degrade is OMIT — never an empty name"
        );
        // The snapshot's OWN totals stay truthful: the snapshot really does hold
        // one file of 10 bytes, and a restore would write it. Shrinking them
        // because this reader cannot render the name would misreport the restore.
        assert_eq!(reply.file_count, 1);
        assert_eq!(reply.total_bytes, 10);
    }

    // ── The sink: the same omission, met by a caller about to write to disk ──
    //
    // These two pin the fix. The pair matters more than either alone: the
    // first says the degrade is refused when it would become data loss, the
    // second says a *correctly wired* reader is not inconvenienced by the check
    // — which is what stops a future session from "fixing" a false positive by
    // deleting it.

    #[test]
    fn get_for_restore_refuses_a_restore_that_would_silently_write_nothing() {
        let req = std::sync::Arc::new(PlantedRequester::with_get(sealed_get_reply(
            &owner_root(),
            "taxes/2026-notice.pdf",
        )));
        // The exact shape of every instance: a restore consumer that
        // reached `get` with no custody wired at its construction seam.
        let client = SnapshotsClient::new(req);

        let err =
            block_on(client.get_for_restore(1)).expect_err("must refuse, not restore 0 files");

        assert_eq!(
            err,
            RestoreReadError::RowsOmitted {
                rendered: 0,
                file_count: 1,
            },
            "the caller is told exactly how many rows it lost"
        );
        // The message has to name the FIX, because the symptom (an empty
        // restore) is what sent three sessions hunting the wrong layer.
        let msg = err.to_string();
        assert!(
            msg.contains("with_label_custody"),
            "message names the fix: {msg}"
        );
        assert!(
            msg.contains("refused"),
            "message says it did not restore: {msg}"
        );
    }

    #[test]
    fn get_for_restore_passes_a_custody_wired_read_straight_through() {
        let root = owner_root();
        let req = std::sync::Arc::new(PlantedRequester::with_get(sealed_get_reply(
            &root,
            "taxes/2026-notice.pdf",
        )));
        let client = SnapshotsClient::new(req).with_label_custody(LabelCustody::owner_only(root));

        let reply = block_on(client.get_for_restore(1)).expect("a wired reader restores");

        assert_eq!(reply.files.len(), 1);
        assert_eq!(reply.files[0].path, "taxes/2026-notice.pdf");
    }

    /// A plaintext snapshot (keyless writer, public audience) has nothing sealed, so
    /// `render_paths` short-circuits and every row survives — the check must stay silent.
    /// (Guards the plaintext render arm: such snapshots restore unchanged.)
    #[test]
    fn get_for_restore_is_silent_on_a_plaintext_snapshot() {
        let mut reply = sealed_get_reply(&owner_root(), "notes.txt");
        reply.files[0].path = "notes.txt".into();
        reply.files[0].path_sealed = None;
        reply.files[0].path_hash = None;
        let req = std::sync::Arc::new(PlantedRequester::with_get(reply));

        let out = block_on(SnapshotsClient::new(req).get_for_restore(1))
            .expect("a plaintext snapshot needs no custody");
        assert_eq!(out.files.len(), 1);
    }

    #[test]
    fn snapshot_get_omits_under_the_wrong_key() {
        let req = std::sync::Arc::new(PlantedRequester::with_get(sealed_get_reply(
            &owner_root(),
            "taxes/2026-notice.pdf",
        )));
        let client = SnapshotsClient::new(req).with_label_custody(LabelCustody::owner_only(
            fauna_core::crypto::BackupKey::from_bytes([9u8; 32]),
        ));

        assert!(
            block_on(client.get(1))
                .expect("infallible mock")
                .files
                .is_empty()
        );
    }

    /// The expand-phase shape: a row that still carries its plaintext renders for
    /// every reader, custody or not — which is what makes the write half
    /// deployable ahead of the per-app render sweep.
    #[test]
    fn snapshot_get_keeps_a_plaintext_row_for_a_keyless_reader() {
        let mut reply = sealed_get_reply(&owner_root(), "taxes/2026-notice.pdf");
        reply.files[0].path = "taxes/2026-notice.pdf".into();
        reply.files[0].path_sealed = None;
        let req = std::sync::Arc::new(PlantedRequester::with_get(reply));
        let client = SnapshotsClient::new(req);

        let reply = block_on(client.get(1)).expect("infallible mock");

        assert_eq!(reply.files.len(), 1);
        assert_eq!(reply.files[0].path, "taxes/2026-notice.pdf");
    }

    /// All three diff lists render, and custody resolves off the reply's own
    /// `folder` — the field this slice added for exactly that reason.
    #[test]
    fn snapshot_diff_renders_added_removed_and_modified() {
        let root = owner_root();
        let (a, r, m) = ("new/a.txt", "gone/b.txt", "kept/c.txt");
        let reply = SnapshotDiffReply {
            snapshot_a: 1,
            snapshot_b: 2,
            added: vec![SnapshotDiffEntry {
                path: String::new(),
                size_bytes: 1,
                path_hash: Some(salt_of(a)),
                path_sealed: Some(seal(&root, a)),
                extra: Default::default(),
            }],
            removed: vec![SnapshotDiffEntry {
                path: String::new(),
                size_bytes: 2,
                path_hash: Some(salt_of(r)),
                path_sealed: Some(seal(&root, r)),
                extra: Default::default(),
            }],
            modified: vec![SnapshotModifiedEntry {
                path: String::new(),
                old_size: 3,
                new_size: 4,
                path_hash: Some(salt_of(m)),
                path_sealed: Some(seal(&root, m)),
                extra: Default::default(),
            }],
            summary: SnapshotDiffSummary {
                added_count: 1,
                removed_count: 1,
                modified_count: 1,
                added_bytes: 1,
                removed_bytes: 2,
                net_bytes: -1,
                extra: Default::default(),
            },
            folder: "docs".into(),
            folder_sealed: None,
            folder_hash: None,
            extra: Default::default(),
        };
        let req = std::sync::Arc::new(PlantedRequester::with_diff(reply));
        let client = SnapshotsClient::new(req).with_label_custody(LabelCustody::owner_only(root));

        let out = block_on(client.diff(1, 2)).expect("infallible mock");

        assert_eq!(out.added[0].path, a);
        assert_eq!(out.removed[0].path, r);
        assert_eq!(out.modified[0].path, m);
        assert_eq!(
            out.folder.as_deref(),
            Some("docs"),
            "an unstamped set falls back to the plaintext, same as before this slice"
        );
    }

    /// The LEAD's own success bar: blank the plaintext, stamp the pair, and the
    /// diff reply still names the set — the render this slice adds.
    #[test]
    fn snapshot_diff_renders_the_set_name_from_its_seal_once_the_plaintext_blanks() {
        let root = owner_root();
        let name = "Family docs";
        let sealed = fauna_core::label_custody::seal_set_name(
            &fauna_core::path_crypto::LabelRoot::owner_of(&root),
            name,
        )
        .expect("seal set name")
        .expect("not reserved");
        let reply = SnapshotDiffReply {
            snapshot_a: 1,
            snapshot_b: 2,
            added: vec![],
            removed: vec![],
            modified: vec![],
            summary: SnapshotDiffSummary::default(),
            // Blanked, as the flip leaves it — the seal is the only thing
            // left to recover the name from.
            folder: String::new(),
            folder_sealed: Some(ByteBuf::from(sealed)),
            folder_hash: Some(ByteBuf::from(
                fauna_core::path_crypto::set_name_hash(name).to_vec(),
            )),
            extra: Default::default(),
        };
        let req = std::sync::Arc::new(PlantedRequester::with_diff(reply));
        let client = SnapshotsClient::new(req).with_label_custody(LabelCustody::owner_only(root));

        let out = block_on(client.diff(1, 2)).expect("infallible mock");

        assert_eq!(
            out.folder.as_deref(),
            Some(name),
            "the seal must recover the name the blanked plaintext no longer carries"
        );
    }

    /// Answers a BOUND set's content keys for one set, addressed by its
    /// `name_hash` — and owner-only for every other hash, so a lookup by the
    /// scrubbed reply's blank plaintext finds no content keys.
    struct BoundByHashResolver {
        set: &'static str,
        keys: fauna_core::folder_keys::FolderContentKeys,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::folder_keys::FolderKeyResolver for BoundByHashResolver {
        async fn resolve(
            &self,
            name_hash: &[u8; 32],
        ) -> anyhow::Result<fauna_core::folder_keys::ResolvedCustody> {
            Ok(
                if *name_hash == fauna_core::path_crypto::set_name_hash(self.set) {
                    fauna_core::folder_keys::ResolvedCustody::ContentKeyed(
                        fauna_core::folder_keys::ResolvedFolderKeys {
                            mls_group_id: Some(b"raw-group-id".to_vec()),
                            content_keys: Some(self.keys.clone()),
                            home_nest_url: None,
                            home_nest_actor_id: None,
                        },
                    )
                } else {
                    fauna_core::folder_keys::ResolvedCustody::owner_only()
                },
            )
        }
    }

    /// A BOUND set's diff after the scrub: the plaintext `folder` is blank, so
    /// custody can only find the set's content keys by the reply's
    /// `folder_hash`. Both the set name and the entry paths render.
    #[test]
    fn snapshot_diff_of_a_scrubbed_bound_set_resolves_custody_by_its_hash() {
        let name = "Shared docs";
        let content = fauna_core::folder_keys::FolderContentKeys::genesis([4u8; 32], 1_000);
        let content_root = fauna_core::path_crypto::LabelRoot::content_key(
            *content.current_key(),
            content.current_version(),
        );
        let path = "new/a.txt";
        let reply = SnapshotDiffReply {
            snapshot_a: 1,
            snapshot_b: 2,
            added: vec![SnapshotDiffEntry {
                path: String::new(),
                size_bytes: 1,
                path_hash: Some(salt_of(path)),
                path_sealed: Some(ByteBuf::from(
                    fauna_core::label_custody::seal_path(&content_root, path).unwrap(),
                )),
                extra: Default::default(),
            }],
            removed: vec![],
            modified: vec![],
            summary: SnapshotDiffSummary::default(),
            folder: String::new(),
            folder_sealed: Some(ByteBuf::from(
                fauna_core::label_custody::seal_set_name(&content_root, name)
                    .unwrap()
                    .unwrap(),
            )),
            folder_hash: Some(ByteBuf::from(
                fauna_core::path_crypto::set_name_hash(name).to_vec(),
            )),
            extra: Default::default(),
        };
        let req = std::sync::Arc::new(PlantedRequester::with_diff(reply));
        let client = SnapshotsClient::new(req).with_label_custody(LabelCustody::new(
            Some(std::sync::Arc::new(BoundByHashResolver {
                set: name,
                keys: content,
            })),
            Some(owner_root()),
        ));

        let out = block_on(client.diff(1, 2)).expect("infallible mock");

        assert_eq!(out.folder.as_deref(), Some(name));
        assert_eq!(out.added.len(), 1, "the sealed path renders, not omits");
        assert_eq!(out.added[0].path, path);
    }

    /// The wire reply always names its set (`folder` is required — a reply
    /// without it is refused at decode, pinned in `fauna_protocol::filesync`),
    /// but post-scrub the name is the empty-string sentinel, and a reader the
    /// nest withholds the seal pair from can open nothing: the rendered
    /// `folder` is `None` (`Omit`), while the entry rows keep whatever
    /// plaintext they carry. A nameless view, not an error and not a dropped
    /// reply.
    #[test]
    fn snapshot_diff_of_a_scrubbed_set_without_the_seal_pair_renders_no_name() {
        let reply = SnapshotDiffReply {
            snapshot_a: 1,
            snapshot_b: 2,
            added: vec![SnapshotDiffEntry {
                path: "new/a.txt".into(),
                size_bytes: 1,
                path_hash: None,
                path_sealed: None,
                extra: Default::default(),
            }],
            removed: vec![],
            modified: vec![],
            summary: SnapshotDiffSummary {
                added_count: 1,
                removed_count: 0,
                modified_count: 0,
                added_bytes: 1,
                removed_bytes: 0,
                net_bytes: 1,
                extra: Default::default(),
            },
            folder: String::new(),
            folder_sealed: None,
            folder_hash: None,
            extra: Default::default(),
        };
        let req = std::sync::Arc::new(PlantedRequester::with_diff(reply));
        let client =
            SnapshotsClient::new(req).with_label_custody(LabelCustody::owner_only(owner_root()));

        let out = block_on(client.diff(1, 2)).expect("infallible mock");

        assert_eq!(
            out.folder, None,
            "the scrubbed sentinel is never shown as a name"
        );
        assert_eq!(out.added[0].path, "new/a.txt");
        assert_eq!(out.summary.added_count, 1);
    }

    /// The salt is the wire's `path_hash`, which is what keeps a scrubbed row
    /// openable. Planting a WRONG hash must break the open — proving the render
    /// genuinely consumes it rather than re-deriving from the plaintext.
    #[test]
    fn the_render_salts_from_the_wire_path_hash() {
        let root = owner_root();
        let mut reply = sealed_get_reply(&root, "taxes/2026-notice.pdf");
        reply.files[0].path_hash = Some(salt_of("a-different-path"));
        let req = std::sync::Arc::new(PlantedRequester::with_get(reply));
        let client = SnapshotsClient::new(req).with_label_custody(LabelCustody::owner_only(root));

        assert!(
            block_on(client.get(1))
                .expect("infallible mock")
                .files
                .is_empty(),
            "a wrong salt must fail the AEAD tag and omit, never render a wrong name"
        );
    }

    // ── snapshots.tags — seal on create, render on get (S6-d) ───────────────

    /// The whole write half in one assertion: a keyed client's create mints
    /// `tags_sealed` on the wire without any call site passing one.
    ///
    /// This is the slice's load-bearing property, because this gesture is the
    /// **only** writer `snapshots.tags` will ever have — the nest holds no key
    /// and a snapshot row has no later pass that revisits it, so a create that
    /// ships `None` here is a tag lost at the flip, not a deferred one.
    #[test]
    fn a_keyed_create_seals_its_tags_on_the_way_out() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone())
            .with_label_custody(LabelCustody::owner_only(owner_root()));
        block_on(client.create_folder("photos", vec!["manual".into()], None))
            .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.filesync.snapshot.create_folder");
        let req: SnapshotCreateFolderRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.tags,
            vec!["manual".to_string()],
            "plaintext still rides"
        );
        let sealed = req.tags_sealed.expect("a keyed create seals its tags");
        // Not merely non-empty: it must open under the same custody, salted by
        // the set name, which is what the reader will do.
        assert_eq!(
            fauna_core::label_custody::render_snapshot_tags(
                &fauna_core::file_download::FileDownloadKeys::owner(owner_root()),
                Some(&sealed),
                None,
                "photos",
                None,
            ),
            Some(vec!["manual".to_string()])
        );
    }

    #[test]
    fn a_keyless_create_ships_no_seal_rather_than_a_wrong_one() {
        // The ratified degrade: plaintext-only, an S8 backfill row. A client with
        // no custody must not invent a root.
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone());
        block_on(client.create_folder("photos", vec!["manual".into()], None))
            .expect("infallible mock");
        let (_, payload) = rec.recorded();
        let req: SnapshotCreateFolderRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.tags_sealed, None);
    }

    #[test]
    fn an_untagged_create_seals_nothing() {
        // Every app but windows sends an empty list today; sealing "no tags" would
        // put a blob on the wire that discloses a capture happened and opens to
        // nothing useful.
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SnapshotsClient::new(rec.clone())
            .with_label_custody(LabelCustody::owner_only(owner_root()));
        block_on(client.create_folder("photos", vec![], None)).expect("infallible mock");
        let (_, payload) = rec.recorded();
        let req: SnapshotCreateFolderRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.tags_sealed, None);
    }

    /// The read half, in the post-flip shape: the plaintext `tags` **and** the
    /// plaintext `folder` are both blank, so the render can only work by
    /// salting from the reply's `folder_hash`.
    #[test]
    fn get_renders_the_tags_sealed_first_once_the_plaintext_has_scrubbed() {
        let root = owner_root();
        let sealed = fauna_core::label_custody::seal_snapshot_tags(
            &fauna_core::path_crypto::LabelRoot::owner_of(&root),
            "photos",
            &["manual".to_string(), "before-upgrade".to_string()],
        )
        .unwrap();
        let reply = SnapshotGetReply {
            id: 1,
            folder: String::new(),
            tags: vec![],
            tags_sealed: Some(ByteBuf::from(sealed)),
            folder_hash: Some(ByteBuf::from(
                fauna_core::path_crypto::set_name_hash("photos").to_vec(),
            )),
            ..Default::default()
        };
        let req = std::sync::Arc::new(PlantedRequester::with_get(reply));
        let client = SnapshotsClient::new(req).with_label_custody(LabelCustody::owner_only(root));

        assert_eq!(
            block_on(client.get(1)).expect("infallible mock").tags,
            vec!["manual".to_string(), "before-upgrade".to_string()]
        );
    }

    #[test]
    fn a_keyless_reader_gets_an_empty_tag_list_rather_than_a_stale_one() {
        // The `Omit` degrade, at the list level: a reader who cannot open the seal
        // and has no resting plaintext shows *nothing*, never a half-truth.
        let sealed = fauna_core::label_custody::seal_snapshot_tags(
            &fauna_core::path_crypto::LabelRoot::owner_of(&owner_root()),
            "photos",
            &["manual".to_string()],
        )
        .unwrap();
        let reply = SnapshotGetReply {
            id: 1,
            folder: String::new(),
            tags: vec![],
            tags_sealed: Some(ByteBuf::from(sealed)),
            folder_hash: Some(ByteBuf::from(
                fauna_core::path_crypto::set_name_hash("photos").to_vec(),
            )),
            ..Default::default()
        };
        let req = std::sync::Arc::new(PlantedRequester::with_get(reply));
        let client = SnapshotsClient::new(req);
        assert!(
            block_on(client.get(1))
                .expect("infallible mock")
                .tags
                .is_empty()
        );
    }
}
