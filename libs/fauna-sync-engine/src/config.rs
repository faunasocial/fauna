use fauna_protocol::folders::PlaceFlags;

/// Operating mode for a sync seat.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Bidirectional sync (default): upload local changes and pull remote changes.
    #[default]
    Sync,
    /// Backup mode: upload-only with automatic snapshot creation after each reconcile.
    Backup,
}

impl SyncMode {
    /// Whether a device in this mode applies a **peer's** delete to its own disk.
    ///
    /// `false` for [`Self::Backup`]. The mode is *upload-only* by its own
    /// definition above, so a backup device's copy **is** the historical record:
    /// erasing a file there because the source erased it is precisely the loss a
    /// backup exists to prevent. Stated in
    /// `docs/goal/behavior/file-sync.md` § 4 ("`delete` changes are not
    /// forwarded to destinations configured in backup-only mode, preserving
    /// historical versions") and required by `docs/goal/principles.md`
    /// § No user-data loss.
    ///
    /// **This is only about a delete arriving FROM the nest.** A backup device
    /// still records its own *local* deletes upward as tombstones — upload-only
    /// constrains what it applies downward, not what it reports.
    ///
    /// **Consult this from every rail that can write a peer's delete to disk,
    /// not just one.** The rule used to live solely in the nest orchestrator's
    /// destination forward, which only ever saw an admin-registered
    /// `folder_destinations` row — and no app or daemon ever created one, so
    /// that guard protected almost nobody while both client-side rails erased
    /// the file anyway. (That phantom rail — table, writer, forward — was
    /// deleted outright 2026-08-18, folders re-model row 7.) Same failure shape as the remote-change
    /// nudge's (`file-sync.md` § Config): a rule enforced on one of several rails
    /// reads as enforced everywhere until a test runs the configuration a real
    /// user reaches.
    pub fn applies_remote_deletes(self) -> bool {
        match self {
            Self::Sync => true,
            Self::Backup => false,
        }
    }

    /// Parse the device-local cache's spelling ([`Self::as_cache_str`]) back
    /// into the mode. `None` for anything else — a cache this binary cannot
    /// read is no cache, never a guess.
    pub fn from_cache_str(s: &str) -> Option<Self> {
        match s {
            "sync" => Some(Self::Sync),
            "backup" => Some(Self::Backup),
            _ => None,
        }
    }

    /// Project a device place's flags onto this two-point enum: **the archive
    /// seat is the one that accepts changes without applying deletes.**
    ///
    /// **The seat, never a folder-level value, is what decides
    /// [`Self::applies_remote_deletes`]** (`file-sync.md` § 4): a place names
    /// what one seat does with what arrives. Keying delete-suppression on a
    /// folder-level mode was wrong where it mattered: an archive seat of an
    /// ordinary folder (the "this laptop syncs, the NAS archives" shape)
    /// resolved to `Sync` and erased the very files it existed to keep (caught
    /// 2026-08-02 by `test_ws_backup_delete_suppression`, tier_3).
    ///
    /// Both flags are consulted, not just `applies_deletes` — and that stays
    /// true even now that `accepts` is REAL (phase 2 slice c gated the
    /// delivery rails on it). The slice-c plan expected this projection to
    /// simplify to `!applies_deletes` once the gate landed, reasoning only
    /// about the delete guard: a source seat's deletes never arrive, so its
    /// classification would be unreachable. **Refuted by the consumer audit
    /// (2026-08-19): `SyncMode` also drives UPLOAD-side behavior** — the
    /// legacy daemon's `push_essential` treated a `Backup` seat's chunk push as
    /// load-bearing for "synced" accounting (since removed with the daemon) —
    /// so remapping `source` (accepts:F, deletes:F) from `Sync` to `Backup`
    /// would change a source seat's upload accounting for zero benefit.
    /// Reading both flags keeps this projection what it was before the flags
    /// existed, for every point.
    pub fn from_place_flags(flags: PlaceFlags) -> Self {
        if flags.accepts && !flags.applies_deletes {
            Self::Backup
        } else {
            Self::Sync
        }
    }

    /// The device-local cache's spelling of this mode — the inverse of
    /// [`Self::from_cache_str`], used to persist the last authoritative answer
    /// ([`resolve_device_mode`]'s cache). The engine's own two-value spelling:
    /// no nest row speaks a mode any more (`file-sync.md` § 4, the mode-free
    /// paragraph), and the cache is device-local.
    pub fn as_cache_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Backup => "backup",
        }
    }
}

/// Whether a seat's [`SyncMode`] is actually **known** — the tri-state the
/// delete guard runs on (`file-sync.md` § 4, direction ratified 2026-08-02).
///
/// [`SyncMode`] answers *what a resolved seat does*; this type answers the prior
/// question, *did anything authoritative actually resolve it*. The distinction
/// exists because the two failure directions are not symmetric in
/// reversibility (`principles.md` § No user-data loss): a delete wrongly
/// **withheld** is recoverable — the file is still on disk, the tombstone can
/// re-deliver — while a delete wrongly **applied** on a backup seat destroys
/// the one copy whose purpose was to outlive the source's deletion. So a seat
/// whose role could not be read must not be guessed into the delete-applying
/// default: it declines remote deletes *and holds its anchor* so the declined
/// tombstone re-delivers once the role is readable, making **both** error
/// directions recoverable. (A resolved `Backup` seat's decline still advances
/// the anchor — that suppression is deliberate and permanent by design.)
///
/// Not serde-visible on purpose: `Unresolved` is a runtime condition, never a
/// value any persisted shape could spell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeResolution {
    /// An authoritative answer: the nest rows (or the persisted last
    /// authoritative answer) said what this seat is.
    Resolved(SyncMode),
    /// No authoritative answer exists — the reads failed and no cached answer
    /// was ever stored. Declines remote deletes and holds the anchor.
    Unresolved,
}

impl From<SyncMode> for ModeResolution {
    fn from(mode: SyncMode) -> Self {
        Self::Resolved(mode)
    }
}

impl ModeResolution {
    /// The guard's read side — `false` for a resolved `Backup` seat **and**
    /// for an unresolved one (never destroy on a guess).
    pub fn applies_remote_deletes(self) -> bool {
        match self {
            Self::Resolved(mode) => mode.applies_remote_deletes(),
            Self::Unresolved => false,
        }
    }

    /// Whether a declined remote delete must **hold** the anchor (be
    /// re-delivered later) rather than be accounted for and never retried.
    ///
    /// `true` only for [`Self::Unresolved`]: the decline is provisional — once
    /// the role is readable the seat either applies the tombstone (it was a
    /// sync seat all along) or declines it permanently (backup). A resolved
    /// `Backup` decline advances the anchor exactly as before: re-fetching a
    /// tombstone that will be declined forever is pure waste.
    pub fn holds_anchor_on_declined_delete(self) -> bool {
        matches!(self, Self::Unresolved)
    }
}

/// **The one device-mode resolution every production reader routes through**
/// (`file-sync.md` § 4: the agent's resident loop at entry + per tick, and `engine_lifecycle::build_engine`'s
/// drive paths — two readers must not disagree about the same seat, and the
/// 2026-08-02 probe proved they did when a third path resolved by the retired
/// set-mode-only key).
///
/// Inputs encode *what the read said*, not just its value:
/// - `seat`: `Some(..)` when the `fauna.folders.members.list` read succeeded —
///   see [`SeatRead`] for the two things a successful read can find; `None`
///   when the read itself failed.
/// - `cached`: the persisted last authoritative answer, if any.
///
/// **A seat is its place and nothing else** (`file-sync.md` § 4, the mode-free
/// paragraph): no folder-level value enters the resolution. Returns `(what to
/// install, what to persist)`. The second element is `Some` only for a
/// **fresh authoritative** answer — a cache read back is never re-persisted.
/// An answered roster with no row for this device ([`SeatRead::Absent`]) is
/// remembered like a row's flags are.
pub fn resolve_device_mode(
    seat: Option<SeatRead>,
    cached: Option<SyncMode>,
) -> (ModeResolution, Option<SyncMode>) {
    match seat {
        Some(SeatRead::Flags(flags)) => {
            let mode = SyncMode::from_place_flags(flags);
            (ModeResolution::Resolved(mode), Some(mode))
        }
        // The roster answered and holds NO row for this device: no place is
        // the **default point** — `{originates, accepts, applies_deletes}` all
        // true, which is `Sync` — exactly what the gesture that gives a device
        // a local presence writes (`file-sync.md` § 4, *A local presence
        // writes the place it needs*), so no seat changes behavior.
        //
        // Cached like every other answered read, and for the reason
        // `file-sync.md` § 4 states as a rule: *"`Unresolved` is only ever a
        // seat the nest has **never** answered for"*. Returning `None` instead
        // treated this answer as no answer at all, so the next degraded read
        // found an empty cache and demoted a converging seat to `Unresolved` —
        // decline every peer delete and hold the anchor. That is not a
        // hypothetical ordering: an in-process host resolves at engine start,
        // racing its own WS connect, so the degraded read lands a millisecond
        // after authenticating (found live 2026-08-28 — every `native`/`tui`
        // cell of `test_seats_converge` red, every `engine` cell green,
        // because the since-removed headless daemon resolved inside its
        // `run_ws_session` once its session was already up).
        //
        // Caching `Sync` cannot disarm a backup seat: `Absent` means NO row
        // exists for this device, so no future nest vocabulary can be hiding a
        // backup marker this reader failed to parse — there is nothing there
        // to fail to parse. The cache only ever repeats a decision the live
        // read just made, never widens it.
        Some(SeatRead::Absent) => (
            ModeResolution::Resolved(SyncMode::Sync),
            Some(SyncMode::Sync),
        ),
        // The read failed: the last authoritative answer governs; a seat that
        // never had one is Unresolved (decline + hold). Never the
        // delete-applying default — that guess is the leg-2 failure this
        // function exists to remove.
        None => fall_back_to_cache(cached),
    }
}

/// What a **successful** roster read found out about this device's seat.
///
/// Re-exported from [`fauna_protocol::folders`], which is where the rule
/// lives: its only two inputs — `FolderMember` and `PlaceFlags` — are that
/// crate's types. Every reader of a roster row goes through this, so a place
/// resolves the same way everywhere.
pub use fauna_protocol::folders::SeatRead;

fn fall_back_to_cache(cached: Option<SyncMode>) -> (ModeResolution, Option<SyncMode>) {
    match cached {
        Some(mode) => (ModeResolution::Resolved(mode), None),
        None => (ModeResolution::Unresolved, None),
    }
}

/// Whether the per-device role read (`fauna.folders.members.list`) is worth
/// a nest round trip for a set — the pure decision half of the I/O in
/// [`resolve_device_mode_from_nest`]. `found` is this set's row from
/// `fauna.folders.list`'s member-visible projection (`role: Some("owner")`
/// for the caller's own sets, `Some("member")` for a set shared *with* the
/// caller; absent/`None` when the row could not be resolved at all,
/// which reads like an owner's).
///
/// A roster **member** row can never carry a per-device role: `folder_members`
/// is a single-owner multi-device roster (the nest's `add_folder_member_for_user`
/// requires the device and the set to share one `actor_id`), so the read is a
/// guaranteed `not_found` for a member's own bound set, on every reconcile.
/// Resolve the known-empty answer locally instead of paying — and
/// logging — a round trip that can never succeed. Every other case (owner, or
/// the row's `role` could not be determined) is unaffected and still reads.
fn should_read_member_role(found: Option<&fauna_protocol::folders::FolderSummary>) -> bool {
    !found.is_some_and(|fs| fs.role.as_deref() == Some("member"))
}

/// The owner-attested declassification verdict for THIS seat, over a folder
/// list it **read successfully** — the one place an engine seat (the resident
/// engine's tick, the build-time binding) turns a row
/// into "may I write unsealed", so the four readers `encryption-at-rest.md`
/// § Readable classes → *The declassification is owner-ATTESTED* names cannot
/// compose the verifier and the replay memory differently.
///
/// Reads the seat's `AttestationMemory` from `db`, judges `found` (the list's
/// row for `acting_name`, or `None` when the list carried none — a sealed
/// verdict that burns) under the trusted owner `anchor` resolves for the row,
/// and persists the memory the verifier handed back **whatever the verdict**:
/// the sealed verdict is the one that burns, and an unpersisted burn is a
/// replay window. A persist failure warns and still returns the verdict — the
/// posture is armed in memory for this run, like the sync mode's own cache.
///
/// Not for an unreadable list: the caller keeps its posture and does not call
/// this (`SeatResolution::public_audience` is `None` then).
pub fn judge_seat_declassification(
    db: &crate::db::SyncDb,
    acting_name: &str,
    found: Option<&fauna_protocol::folders::FolderSummary>,
    anchor: &fauna_client_folders::DeclassificationAnchor<'_>,
) -> bool {
    use fauna_core::log_redact::log_folder_name;
    use fauna_protocol::folders::{AttestationMemory, FolderSummary};

    let memory = AttestationMemory::from_meta(
        db.audience_attestation_memory()
            .unwrap_or_else(|e| {
                // Unreadable is not absent: `from_meta(Some(""))` fails closed
                // (a floor no counter reaches), which is the right answer for
                // a row this seat cannot read — a seat that cannot remember
                // its floor must not arm.
                tracing::warn!(
                    folder = %log_folder_name(acting_name),
                    error = %e,
                    "reading the audience-attestation memory failed; judging fail-closed"
                );
                Some(String::new())
            })
            .as_deref(),
    );
    let trusted_owner = found.and_then(|row| anchor.trusted_owner_for(row));
    let (unsealed, memory) = FolderSummary::judge_listed_declassification(
        found,
        acting_name,
        trusted_owner.as_ref(),
        memory,
    );
    if let Err(e) = db.set_audience_attestation_memory(&memory.to_meta()) {
        tracing::warn!(
            folder = %log_folder_name(acting_name),
            error = %e,
            "persisting the audience-attestation memory failed (the verdict is still armed this run)"
        );
    }
    unsealed
}

/// [`fauna_client_folders::FolderChannelOwners`] over an [`MlsEngine`] — the
/// MLS-holding hosts' member-seat anchor (`MlsEngine::folder_channel_owner`,
/// the durable marker stamped at folder-group mint / folder-Welcome join and
/// re-pointed by the owner's verified succession).
pub struct MlsChannelOwners<'a>(pub &'a fauna_mls::engine::MlsEngine);

impl fauna_client_folders::FolderChannelOwners for MlsChannelOwners<'_> {
    fn folder_channel_owner(&self, channel_id: &[u8; 32]) -> Option<fauna_core::identity::ActorId> {
        self.0
            .folder_channel_owner(&fauna_mls::types::ChannelId(*channel_id))
    }
}

/// The I/O half of [`resolve_device_mode`] — **one** implementation of the two
/// authoritative reads plus the cache protocol, shared by the engine
/// ([`crate::engine::SyncEngine::resolve_sync_mode`]) and every other seat
/// reader, so no two production readers can compose the reads — or their
/// failure posture — differently (`file-sync.md` § 4).
///
/// `nest = None` means "no control plane this session": both reads count as
/// failed and the persisted last authoritative answer governs (else
/// `Unresolved`). A fresh authoritative answer is persisted to `db` before
/// returning; a persist failure only warns — the guard is still armed in
/// memory for this run. The two degrade shapes each log a loud warning: a
/// quieter log once hid a disarmed backup seat for a whole process lifetime.
///
/// `anchor` is the identity the folder's **audience attestation** is verified
/// against ([`judge_seat_declassification`]) — the seat's own actor id for a
/// folder its account owns, the channel's MLS-recorded owner for a member
/// seat; never a field of the row.
///
/// `key` names which listed row is this seat's
/// ([`crate::binding_edge::SeatRowKey`]) — never the name, which two sets can
/// share (`on-demand-files.md` § Hosting multiple on-demand folders); `folder`
/// is the set's name as a log label and the owner-scoped roster read's
/// argument only. A key that matches no row reads as an absent row — every
/// field's sealed / fail-closed direction.
pub async fn resolve_device_mode_from_nest(
    nest: Option<&std::sync::Arc<fauna_client::NestClient>>,
    key: crate::binding_edge::SeatRowKey,
    folder: &str,
    device_id_hex: &str,
    db: &crate::db::SyncDb,
    keys: &fauna_core::file_download::FileDownloadKeys,
    anchor: &fauna_client_folders::DeclassificationAnchor<'_>,
) -> SeatResolution {
    use fauna_core::log_redact::log_folder_name;

    let mut folder_bases: Option<Vec<(i64, crate::binding_edge::BindingBasis)>> = None;
    let mut unanchored_public_claim: Option<bool> = None;
    #[allow(clippy::type_complexity)]
    let (
        seat,
        public_audience,
        metadata_only_residency,
        website_enabled,
        selective_sync,
        exclusive_editing,
    ): (
        Option<SeatRead>,
        Option<bool>,
        Option<bool>,
        Option<bool>,
        SelectiveSyncResolution,
        ExclusiveEditingResolution,
    ) = match nest {
        None => (
            None,
            None,
            None,
            None,
            SelectiveSyncResolution::default(),
            ExclusiveEditingResolution::default(),
        ),
        Some(nest) => {
            let client = fauna_client_folders::FoldersClient::new(std::sync::Arc::clone(nest));
            let list_result = client.list_owned_and_shared_wire().await;
            // Decision 2's refresh edge rides the same read (`SeatResolution::folder_bases`).
            folder_bases = list_result.as_ref().ok().map(|reply| {
                reply
                    .folders
                    .iter()
                    .map(|fs| (fs.id, crate::binding_edge::BindingBasis::of(fs)))
                    .collect()
            });
            let found = list_result
                .as_ref()
                .ok()
                .and_then(|reply| key.find(&reply.folders));
            // Phase 4: the folder's audience rides the SAME list read the
            // selective-sync lists do, so a running seat learns a declassify/flip-back
            // within one tick. The verdict is the owner-attested one
            // (`judge_seat_declassification`): `Some(false)` for a row the
            // nest merely CLAIMS public, for one whose attestation does not
            // verify under this seat's anchor, for a replayed one, and for
            // a row absent from a successful list (a deleted set is not
            // world-readable) — every one of those also burns the seat's
            // armed counter; `None` only when the list itself was
            // unreadable — keep the last armed posture, exactly the mode's
            // failure discipline, and touch the memory not at all.
            let public_audience = match &list_result {
                Ok(_) => Some(judge_seat_declassification(db, folder, found, anchor)),
                Err(_) => None,
            };
            // Why a sealed verdict is sealed, for the one case a seat reports
            // (`SeatResolution::unanchored_public_claim`) — off the same read.
            unanchored_public_claim = list_result.as_ref().ok().map(|_| {
                found.is_some_and(|row| {
                    row.is_public_claim_unanchored(anchor.trusted_owner_for(row).as_ref())
                })
            });
            // Phase 5: the residency rides the same read. The fail-closed
            // direction is `FolderSummary::is_metadata_only`'s.
            let metadata_only_residency = match &list_result {
                Ok(_) => Some(
                    found.is_some_and(fauna_protocol::folders::FolderSummary::is_metadata_only),
                ),
                Err(_) => None,
            };
            // The website toggle rides the same read — the signal
            // `SyncEngine::converge_corpus_to_website` observes turning on
            // (`web-content-hosting.md` § Content model). Fail-closed to OFF:
            // an absent row is not a served site, and only a positively read
            // toggle ever costs a corpus walk.
            let website_enabled = match &list_result {
                Ok(_) => Some(found.is_some_and(|fs| fs.website_enabled)),
                Err(_) => None,
            };
            // Selective sync rides the SAME list read, rendered **sealed-first**
            // with this reader's own key material — the seam
            // `path-sealing.md` § `folders.include_paths`/`exclude_paths` names,
            // and the reason this resolver takes `keys` at all. The seal is
            // under the OWNER's root salted by `row.id`, which
            // `FileDownloadKeys` carries (plus the predecessor read candidates,
            // so a rotated owner still opens its own lists).
            //
            // A failed list, and a row absent from a successful one, both keep
            // the armed filter: an absent row says nothing about what the user
            // wants filtered, and blanking a filter is the one wrong direction
            // that syncs data the user excluded.
            let selective_sync = match (&list_result, found) {
                (Ok(_), Some(fs)) => SelectiveSyncResolution {
                    include_paths: fauna_core::label_custody::render_include_paths(
                        keys,
                        fs.include_paths_sealed.as_ref().map(|b| &b[..]),
                        fs.include_paths.as_deref(),
                        fs.id,
                    ),
                    exclude_paths: fauna_core::label_custody::render_exclude_paths(
                        keys,
                        fs.exclude_paths_sealed.as_ref().map(|b| &b[..]),
                        fs.exclude_paths.as_deref(),
                        fs.id,
                    ),
                },
                _ => SelectiveSyncResolution::default(),
            };
            // Exclusive editing rides the SAME list read (`file-sync.md`
            // § Exclusive editing). Both halves come off `found` so the flag
            // and the holder are never composed from two different moments —
            // a seat that read "governed" from one list and "unheld" from a
            // later one could write straight through a lease somebody took in
            // between. A row absent from a SUCCESSFUL list governs nothing and
            // is held by nobody; only an unreadable list answers `None` and
            // keeps the armed posture.
            let exclusive_editing = match &list_result {
                Ok(_) => ExclusiveEditingResolution {
                    governed: Some(found.is_some_and(|fs| fs.exclusive_editing)),
                    lease: Some(
                        found
                            .and_then(|fs| fs.lease.as_ref())
                            .map(crate::folder_lease::LeaseHolder::from),
                    ),
                },
                Err(_) => ExclusiveEditingResolution::default(),
            };
            if let Err(e) = &list_result {
                tracing::warn!(
                    folder = %log_folder_name(folder),
                    error = %e,
                    "reading the folder list failed"
                );
            }
            let seat = if should_read_member_role(found) {
                match client.members_list(folder.to_string()).await {
                    Ok(reply) => Some(SeatRead::find(&reply.members, device_id_hex)),
                    Err(e) => {
                        tracing::warn!(
                            folder = %log_folder_name(folder),
                            error = %e,
                            "reading this device's place in the folder failed"
                        );
                        None
                    }
                }
            } else {
                // Roster member of someone else's set: no per-device row can
                // exist for us there (see `should_read_member_role`), so this
                // is a known-good successful "no seat" read, not a degrade — it
                // takes the same match arm a real empty roster reply would.
                Some(SeatRead::Absent)
            };
            (
                seat,
                public_audience,
                metadata_only_residency,
                website_enabled,
                selective_sync,
                exclusive_editing,
            )
        }
    };

    let cached = db
        .get_cached_sync_mode()
        .ok()
        .flatten()
        .and_then(|s| SyncMode::from_cache_str(&s));
    let used_fallback = seat.is_none();

    let (resolution, to_cache) = resolve_device_mode(seat.clone(), cached);
    if let Some(fresh) = to_cache
        && let Err(e) = db.set_cached_sync_mode(fresh.as_cache_str())
    {
        tracing::warn!(
            folder = %log_folder_name(folder),
            error = %e,
            "persisting the authoritative sync mode failed (the guard is still armed this run)"
        );
    }
    if used_fallback {
        match resolution {
            ModeResolution::Resolved(mode) => tracing::warn!(
                folder = %log_folder_name(folder),
                ?mode,
                "nest rows unreadable; running on the last authoritative sync mode"
            ),
            ModeResolution::Unresolved => tracing::warn!(
                folder = %log_folder_name(folder),
                "nest rows unreadable and no authoritative sync mode was ever stored; \
                 declining remote deletes and holding the anchor until the place is readable"
            ),
        }
    }
    SeatResolution {
        mode: resolution,
        public_audience,
        unanchored_public_claim,
        accepts: accepts_from_seat(seat),
        metadata_only_residency,
        website_enabled,
        selective_sync,
        exclusive_editing,
        folder_bases,
    }
}

/// The pure accepts-resolution rule ([`SeatResolution::accepts`] owns the
/// semantics): flags answer directly; an **Absent** seat accepts — no row can
/// restrict it, and accepting is what a shared-set member without a roster row
/// is *for*; a failed read answers `None`, keeping the last armed posture.
/// Split out of the I/O resolver so the rule is testable without a nest — the `availability_from_probe` lesson.
pub fn accepts_from_seat(seat: Option<SeatRead>) -> Option<bool> {
    match seat {
        Some(SeatRead::Flags(flags)) => Some(flags.accepts),
        Some(SeatRead::Absent) => Some(true),
        None => None,
    }
}

/// What one authoritative nest read said about this seat — the sync-mode
/// resolution plus the folder's phase-4 audience, both off the ONE
/// `fauna.folders.list` fetch so no reader can compose them from two moments.
///
/// `public_audience`: `Some(true)` = the folder is declassified (`audience ==
/// "public"`, and not WebDAV-served — that nest-refused combination resolves
/// sealed); `Some(false)` = it is not (row absent from a successful list
/// included — a deleted set is not world-readable); `None` = the list was
/// unreadable, keep the last armed posture (the mode's own failure
/// discipline).
///
/// Deliberately **not** `Copy`: [`Self::selective_sync`] carries owned path
/// lists. Every consumer moves or clones the whole answer anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatResolution {
    pub mode: ModeResolution,
    pub public_audience: Option<bool>,
    /// Whether the row claims `public` on a seat that holds **no trusted
    /// owner** for it (`FolderSummary::is_public_claim_unanchored`) — a member
    /// seat on a host with no MLS state, which seals by construction
    /// (`encryption-at-rest.md` § Implementation status today). A diagnostic
    /// beside [`Self::public_audience`], never an input to it: the engine logs
    /// its edges and arms nothing off it.
    ///
    /// `Some(true)` = the claim stands and nothing anchors it; `Some(false)` =
    /// no such claim (an absent row included); `None` = the list was
    /// unreadable — nothing was read, so nothing changed.
    pub unanchored_public_claim: Option<bool>,
    /// Whether this seat's place **accepts** remote changes at all — the
    /// `PlaceFlags::accepts` gate (folders re-model § Places: *"a seat only
    /// pulls folders where its place accepts"*), riding the same roster read
    /// as the mode so no reader composes the two from different moments.
    ///
    /// `Some(false)` = the roster's flags positively say remote changes do
    /// not land here (a source-only place) — the engine skips its pull and
    /// placeholder-population rails entirely. `Some(true)` = they do; a seat
    /// with **no per-device row** (`SeatRead::Absent` — a member of someone
    /// else's set, whose single-owner roster can carry no row for it) also
    /// resolves `Some(true)`, because nothing authoritative restricts it and
    /// accepting is what a shared-set member is *for*. `None` = unknown (an
    /// unreadable seat vocabulary, or the reads failed) — keep the last armed
    /// posture, the mode's own failure discipline. The arm-side default is
    /// **accept** (every seat's behavior before the flag became real), and
    /// both wrong directions are recoverable: wrongly withheld delivery
    /// arrives on a later tick, wrongly delivered files sit on disk un-erased
    /// — neither destroys anything (`principles.md` § No user-data loss is
    /// about the *delete* guard, which keeps its own stricter tri-state).
    pub accepts: Option<bool>,
    /// Whether the folder's owner opted into **metadata-only content
    /// residency** (phase 5 — `file-sync.md` § Content residency), riding the
    /// SAME `fauna.folders.list` read as the mode and the audience so no
    /// reader composes them from different moments.
    ///
    /// `Some(true)` = the projection says chunk bytes never rest on the nest —
    /// the seat skips its byte uploads (metadata records normally).
    /// `Some(false)` = full residency (including a row absent from a
    /// successful list — fail-closed to full: only an explicit, parsed
    /// opt-in may stop bytes resting). `None` = the list itself was
    /// unreadable — keep the last armed posture, the mode's own failure
    /// discipline.
    pub metadata_only_residency: Option<bool>,
    /// Whether the folder's **website toggle** is on (`folders.website_enabled`
    /// — `web-content-hosting.md` § Content model), riding the SAME
    /// `fauna.folders.list` read as the mode, the audience and the residency so
    /// no reader composes them from different moments.
    ///
    /// `Some(true)` = the projection says this folder serves a website — the
    /// signal [`crate::engine::SyncEngine::converge_corpus_to_website`] watches
    /// for, so a SEALED folder's back-catalogue reaches `web_files` (which the
    /// nest cannot backfill for it: sealed heads rest no plaintext name, S9).
    /// `Some(false)` = it does not, a row absent from a successful list
    /// included (a deleted set serves nothing). `None` = the list itself was
    /// unreadable — keep the last armed posture, the mode's own failure
    /// discipline.
    pub website_enabled: Option<bool>,
    /// The folder's **selective-sync** filter lists (`include_paths` /
    /// `exclude_paths`), rendered sealed-first off the SAME
    /// `fauna.folders.list` read as everything above — see
    /// [`SelectiveSyncResolution`] for the per-list tri-state.
    pub selective_sync: SelectiveSyncResolution,
    /// The folder's **exclusive-editing** governance and live lease holder
    /// (`file-sync.md` § Exclusive editing), riding the SAME
    /// `fauna.folders.list` read as everything above — see
    /// [`ExclusiveEditingResolution`] for the per-field tri-state.
    pub exclusive_editing: ExclusiveEditingResolution,
    /// The content-key binding basis of every row the SAME `fauna.folders.list`
    /// read carried, by row id — decision 2's refresh edge
    /// (`on-demand-files.md` § Shared sets on a capability host): a resident
    /// engine finds its own set's row by identity here, re-installs its pre-seal
    /// floor, and asks its host to re-resolve when the row moved
    /// ([`crate::engine::SyncEngine::refresh_sync_mode`]). `None` = the list was
    /// unreadable or not read (a foreign set, a set-less engine, an unconnected
    /// control plane) — keep the armed posture, the discipline every field above
    /// keeps.
    pub folder_bases: Option<Vec<(i64, crate::binding_edge::BindingBasis)>>,
}

/// A folder's **exclusive-editing** state as one authoritative nest read saw it
/// (`file-sync.md` § Exclusive editing) — the per-folder opt-in, and who holds
/// the folder's lease right now.
///
/// Both ride the folder-list read every seat already performs, rather than
/// asking: `fauna.folders.lease.acquire` **takes** a free lease as a side effect
/// of being asked and is gated on the writable-folder resolver, so it can be
/// neither a probe nor a reader member's way of finding out. `FolderSummary`
/// owns both facts on the wire, so this type only reduces them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExclusiveEditingResolution {
    /// Whether the folder's owner turned exclusive editing on.
    ///
    /// `Some(true)` = governed — this seat takes a lease before an upload pass.
    /// `Some(false)` = not governed, **a row absent from a successful list
    /// included**: an absent row governs nothing, and this is the one posture on
    /// this read that fails **OPEN**, because failing the other way would freeze
    /// a user's own folder against their own writes over a field that did not
    /// parse (`FolderSummary::exclusive_editing` owns the direction and the
    /// reasoning). `None` = the list itself was unreadable — keep the last armed
    /// posture, the mode's own failure discipline.
    pub governed: Option<bool>,
    /// The folder's live lease holder as the projection reported it.
    ///
    /// `Some(Some(holder))` = held by that device until that expiry.
    /// `Some(None)` = the projection positively says nobody holds it (a row
    /// absent from a successful list included) — a real answer, and the one that
    /// lets a seat that was locked out write again. `None` = the list itself was
    /// unreadable — keep the last armed reading, which is safe here in a way it
    /// would not be for a plain flag: a lease carries its own expiry, so a stale
    /// holder stops binding within one TTL whether or not this seat ever reaches
    /// the nest again.
    pub lease: Option<Option<crate::folder_lease::LeaseHolder>>,
}

/// A folder's selective-sync filter lists as one authoritative nest read saw
/// them (`file-sync.md` § Config — the row is the single authoritative source
/// and **every device reads the row and applies it**).
///
/// Each list is its own tri-state:
///
/// - `Some(list)` — the row's authoritative list (possibly empty, which is a
///   real answer: the user cleared the filter). Install it.
/// - `None` — the row said nothing definitive: `label_custody::render_include_paths`/
///   `render_exclude_paths` returned the `Omit` degrade — the row carries a
///   seal this reader could not open, *or the row carries no seal at all*
///   (either never configured, or a seal a nest-side edit deleted — the two
///   are byte-identical to this reader, and this type deliberately does not
///   try to tell them apart).
///
/// ⚠ **`None` does NOT mean "install empty" — the one consumer,
/// [`crate::engine::install_selective_sync`], decides per field what `None`
/// means using state this type does not have.** A field that was never
/// configured must still resolve empty so the OTHER field (every production
/// row today configures at most one of the two independently) can arm; a
/// field a nest-side edit un-sealed after this reader had it armed must NOT
/// resolve empty (`path-sealing.md` § `folders.include_paths`/`exclude_paths`
/// is the authority; charter finding is the corollary this note
/// draws from it — deleting the seal, not corrupting it, used to flip a
/// seat's filter from *keep* to *blank* with no signal anywhere). The two
/// cases are indistinguishable from the CURRENT row alone — the consumer
/// tells them apart by reapplying whatever it is ALREADY armed with for that
/// one field, which is empty for a never-configured field by construction and
/// non-empty for one a deletion just orphaned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectiveSyncResolution {
    pub include_paths: Option<Vec<String>>,
    pub exclude_paths: Option<Vec<String>>,
}

#[cfg(test)]
mod sync_mode_tests {
    use super::{PlaceFlags, SyncMode};

    /// The whole point of backup mode, as one assertion: the destination keeps
    /// what the source erased (`file-sync.md` § 4; `principles.md` § No
    /// user-data loss).
    #[test]
    fn a_backup_device_never_applies_a_peers_delete() {
        assert!(!SyncMode::Backup.applies_remote_deletes());
    }

    /// The other half, equally load-bearing: suppressing deletes in *sync* mode
    /// would silently strand every device's copy of a file the user deleted on
    /// purpose — a bug that reads as "sync is broken", not as safety.
    #[test]
    fn a_sync_device_does_apply_a_peers_delete() {
        assert!(SyncMode::Sync.applies_remote_deletes());
    }

    /// `SyncMode::default()` is what every un-plumbed construction site gets, so
    /// pin that the default is the *bidirectional* one. A future variant that
    /// defaulted to `Backup` would silently stop deletes converging fleet-wide.
    #[test]
    fn the_default_mode_applies_remote_deletes() {
        assert_eq!(SyncMode::default(), SyncMode::Sync);
        assert!(SyncMode::default().applies_remote_deletes());
    }

    /// An unrecognized cache spelling is no cache — never a guess.
    #[test]
    fn an_unknown_cache_spelling_is_no_cache() {
        assert_eq!(SyncMode::from_cache_str("web"), None);
        assert_eq!(SyncMode::from_cache_str("mirror-of-the-future"), None);
        assert_eq!(SyncMode::from_cache_str(""), None);
    }

    /// The bug `test_ws_backup_delete_suppression` caught 2026-08-02, as one
    /// assertion: an archive seat resolves to `Backup` — the seat decides.
    /// Keying on a folder-level mode resolved this seat to `Sync` and erased
    /// the files it existed to keep.
    #[test]
    fn an_archive_seat_resolves_to_backup() {
        assert_eq!(
            SyncMode::from_place_flags(PlaceFlags::archive_place()),
            SyncMode::Backup,
            "the seat's own flags decide"
        );
    }

    /// Every other point converges deletes: a seat that applies them, and a
    /// seat that accepts nothing (so no delete ever arrives to decline).
    #[test]
    fn every_non_archive_point_applies_deletes() {
        for (o, a, d) in [
            (true, true, true),
            (false, true, true),
            (true, false, false),
            (false, false, false),
            (true, false, true),
            (false, false, true),
        ] {
            assert_eq!(
                SyncMode::from_place_flags(PlaceFlags::new(o, a, d)),
                SyncMode::Sync,
                "({o}, {a}, {d})"
            );
        }
        assert_eq!(
            SyncMode::from_place_flags(PlaceFlags::new(false, true, false)),
            SyncMode::Backup,
            "the receive-only archive point is an archive seat too"
        );
    }
}

#[cfg(test)]
mod should_read_member_role_tests {
    use super::should_read_member_role;
    use fauna_protocol::folders::FolderSummary;

    /// A roster member's own set: no per-device role row can exist for us
    /// there — skip the guaranteed-`not_found` round trip.
    #[test]
    fn skips_the_read_for_a_member_row() {
        let fs = FolderSummary {
            role: Some("member".to_string()),
            ..Default::default()
        };
        assert!(!should_read_member_role(Some(&fs)));
    }

    /// The caller's own set (or an unresolved row, whose `role` is `None`):
    /// unaffected, still reads.
    #[test]
    fn reads_for_an_owner_row_or_an_unstamped_row() {
        let owned = FolderSummary {
            role: Some("owner".to_string()),
            ..Default::default()
        };
        assert!(should_read_member_role(Some(&owned)));

        let unstamped = FolderSummary {
            role: None,
            ..Default::default()
        };
        assert!(should_read_member_role(Some(&unstamped)));
    }

    /// The row could not be resolved at all (list read failed, or no entry
    /// matched the name yet) — stay conservative and still attempt the read,
    /// exactly as before this change.
    #[test]
    fn reads_when_the_row_is_unknown() {
        assert!(should_read_member_role(None));
    }
}

#[cfg(test)]
mod resolve_device_mode_tests {
    use super::{
        ModeResolution, PlaceFlags, SeatRead, SyncMode, accepts_from_seat, resolve_device_mode,
    };

    /// **The leg-3 probe, inverted into a permanent pin**: every
    /// production reader routes through this one function, so the two
    /// The seat a roster row carrying these flags produces.
    fn seat(flags: PlaceFlags) -> SeatRead {
        SeatRead::from_member(&fauna_protocol::folders::FolderMember {
            flags,
            ..Default::default()
        })
    }

    /// resolutions that disagreed about the same seat (`build_engine`'s
    /// set-mode-only read vs the daemon/agent's seat-aware read) structurally
    /// cannot any more — and the answer for the shape that lost files (an
    /// archive seat) is `Backup`, cached as fresh.
    #[test]
    fn the_two_production_resolutions_agree_for_an_archive_seat() {
        assert_eq!(
            resolve_device_mode(Some(seat(PlaceFlags::archive_place())), None),
            (
                ModeResolution::Resolved(SyncMode::Backup),
                Some(SyncMode::Backup)
            )
        );
    }

    /// Leg-2's fix, memory half (`file-sync.md` § 4, ratified 2026-08-02): a
    /// failed read re-arms from the last thing the nest actually said, not
    /// from the delete-applying default. The cache read back is NOT
    /// re-persisted (second element `None`) — only fresh authority writes.
    #[test]
    fn a_failed_read_falls_back_to_the_cached_authoritative_answer() {
        assert_eq!(
            resolve_device_mode(None, Some(SyncMode::Backup)),
            (ModeResolution::Resolved(SyncMode::Backup), None)
        );
    }

    /// Leg-2's fix, direction half: a seat with no authoritative answer at all
    /// is UNRESOLVED — it declines remote deletes and holds its anchor, never
    /// guesses the delete-applying default.
    #[test]
    fn a_failed_read_with_no_cache_is_unresolved_and_declines_deletes() {
        let (resolution, cache) = resolve_device_mode(None, None);
        assert_eq!(resolution, ModeResolution::Unresolved);
        assert_eq!(cache, None);
        assert!(!resolution.applies_remote_deletes());
        assert!(resolution.holds_anchor_on_declined_delete());
    }

    /// No place is the **default point** (`file-sync.md` § 4, the mode-free
    /// paragraph): an answered roster with no row for this device keeps
    /// converging deletes — **and is remembered**, because it is an answer:
    /// the nest replied and holds no place for this device.
    ///
    /// It was deliberately NOT cached until 2026-08-28, on the reading that a
    /// no-opinion reply "is not an authoritative mode". The live consequence
    /// says otherwise — with nothing cached, the next degraded read has no last
    /// answer to fall back to and demotes the seat to `Unresolved`, which
    /// declines every peer delete and holds the anchor. `file-sync.md` § 4
    /// already ruled the invariant the other way: *"`Unresolved` is only ever a
    /// seat the nest has **never** answered for."* See
    /// `a_no_opinion_answer_is_remembered_so_a_later_failed_read_never_unresolves`
    /// for the sequence this pins the first half of.
    #[test]
    fn a_readable_roster_with_no_opinion_keeps_converging_and_is_cached() {
        assert_eq!(
            resolve_device_mode(Some(SeatRead::Absent), Some(SyncMode::Backup)),
            (
                ModeResolution::Resolved(SyncMode::Sync),
                Some(SyncMode::Sync)
            ),
            "an answered roster with no place for this device outranks the \
             cache — the cache follows the live read rather than preserving a \
             superseded one"
        );
        assert_eq!(
            resolve_device_mode(Some(SeatRead::Absent), None),
            (
                ModeResolution::Resolved(SyncMode::Sync),
                Some(SyncMode::Sync)
            )
        );
    }

    /// **A seat the nest HAS answered for never falls to `Unresolved`**
    /// (`file-sync.md` § 4: *"`Unresolved` is only ever a seat the nest has
    /// **never** answered for"*). The no-opinion answer is still an answer —
    /// both reads succeeded and neither said `backup` — so it must be
    /// remembered, or the very next degraded read demotes a converging seat
    /// to decline-and-hold and its peers' deletes stop arriving.
    ///
    /// Found live 2026-08-28 on `test_filesync_seats.py::test_seats_converge`,
    /// which reddened on all four `native`/`tui` cells while every
    /// `engine` cell passed. The app seat's engine resolves its mode at engine
    /// start, racing its own WS connect: both reads fail against a control
    /// plane that is not up yet, the cache this arm never wrote is empty, and
    /// the seat demotes `Resolved(Sync)` → `Unresolved` a millisecond after
    /// authenticating. It then declines every peer delete for the process
    /// lifetime — and the cell's rescan backstop is set beyond the run's
    /// ceiling by design, so nothing ever re-resolves it. The since-removed
    /// headless daemon resolved inside `run_ws_session`, after its session was
    /// up, which is the whole of why the control cell passed.
    ///
    /// The pair below is the actual production sequence, not two independent
    /// assertions: the answer this arm returns is fed to the degraded read as
    /// its cache.
    #[test]
    fn a_no_opinion_answer_is_remembered_so_a_later_failed_read_never_unresolves() {
        let (resolution, to_cache) = resolve_device_mode(Some(SeatRead::Absent), None);
        assert_eq!(
            resolution,
            ModeResolution::Resolved(SyncMode::Sync),
            "an answered roster with no backup marker keeps converging"
        );
        assert_eq!(
            to_cache,
            Some(SyncMode::Sync),
            "the nest answered — that answer is what a later failed read must \
             fall back to (`file-sync.md` § 4)"
        );

        // The degraded read that follows, handed exactly what was cached above.
        assert_eq!(
            resolve_device_mode(None, to_cache),
            (ModeResolution::Resolved(SyncMode::Sync), None),
            "a seat the nest already answered for must not decline its peers' \
             deletes just because a later read failed"
        );
    }

    /// A seat's flags are authoritative on their own.
    #[test]
    fn a_seat_s_flags_alone_are_authoritative() {
        assert_eq!(
            resolve_device_mode(Some(seat(PlaceFlags::archive_place())), None),
            (
                ModeResolution::Resolved(SyncMode::Backup),
                Some(SyncMode::Backup)
            )
        );
        assert_eq!(
            resolve_device_mode(Some(seat(PlaceFlags::default_place())), None),
            (
                ModeResolution::Resolved(SyncMode::Sync),
                Some(SyncMode::Sync)
            )
        );
    }

    /// A `role` key a nest still sends rides the member row's `extra` and
    /// is never read: the flags alone describe the seat. Uses the archive
    /// seat as the probe because it is the direction that matters — reading
    /// the wrong one here deletes files.
    #[test]
    fn a_stray_role_key_never_outranks_the_flags() {
        let mut member = fauna_protocol::folders::FolderMember {
            flags: PlaceFlags::archive_place(),
            ..Default::default()
        };
        member
            .extra
            .insert("role".into(), fauna_protocol::Value::String("sync".into()));
        let (resolution, cache) = resolve_device_mode(Some(SeatRead::from_member(&member)), None);
        assert_eq!(resolution, ModeResolution::Resolved(SyncMode::Backup));
        assert_eq!(cache, Some(SyncMode::Backup));
        assert!(!resolution.applies_remote_deletes());
    }

    /// `SeatRead::find` picks this device's row out of a roster and reports a
    /// roster that simply does not list us as `Absent`.
    #[test]
    fn finding_our_seat_in_a_roster() {
        let members = vec![
            fauna_protocol::folders::FolderMember {
                device_id: "aa".repeat(32),
                flags: PlaceFlags::new(true, false, false),
                ..Default::default()
            },
            fauna_protocol::folders::FolderMember {
                device_id: "bb".repeat(32),
                flags: PlaceFlags::archive_place(),
                ..Default::default()
            },
        ];
        assert_eq!(
            SeatRead::find(&members, &"bb".repeat(32)),
            SeatRead::Flags(PlaceFlags::archive_place())
        );
        assert_eq!(SeatRead::find(&members, &"cc".repeat(32)), SeatRead::Absent);
        assert_eq!(SeatRead::find(&[], &"aa".repeat(32)), SeatRead::Absent);
    }

    /// A resolved Backup decline still ADVANCES the anchor (permanent, by
    /// design); only Unresolved holds. The pair is what makes both error
    /// directions recoverable without re-fetching a forever-declined
    /// tombstone.
    #[test]
    fn only_the_unresolved_state_holds_the_anchor_on_a_decline() {
        assert!(!ModeResolution::Resolved(SyncMode::Backup).holds_anchor_on_declined_delete());
        assert!(!ModeResolution::Resolved(SyncMode::Sync).holds_anchor_on_declined_delete());
        assert!(ModeResolution::Unresolved.holds_anchor_on_declined_delete());
    }

    #[test]
    fn the_cache_str_round_trips_through_the_cache_parser() {
        for mode in [SyncMode::Sync, SyncMode::Backup] {
            assert_eq!(SyncMode::from_cache_str(mode.as_cache_str()), Some(mode));
        }
    }

    /// The accepts-resolution rule (phase 2 slice c): flags answer directly,
    /// an Absent seat accepts (a member of someone else's set has no roster
    /// row and accepting is what it is for), and a failed read answers None so
    /// the armed posture stands.
    #[test]
    fn accepts_resolves_from_flags_and_fails_open_only_by_keeping_the_posture() {
        assert_eq!(
            accepts_from_seat(Some(SeatRead::Flags(PlaceFlags::new(true, false, false)))),
            Some(false)
        );
        for flags in [PlaceFlags::default_place(), PlaceFlags::archive_place()] {
            assert_eq!(accepts_from_seat(Some(SeatRead::Flags(flags))), Some(true));
        }
        assert_eq!(accepts_from_seat(Some(SeatRead::Absent)), Some(true));
        assert_eq!(accepts_from_seat(None), None);
    }
}
