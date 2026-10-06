//! The import run — step 5 of `archive-import.md` § The wizard and its
//! machine, and the durable `Start` commit that precedes it.
//!
//! Order inside a run, exactly as § The wizard and its machine step 5 gives
//! it: the raw zip, then the model files (**every** category — nothing the
//! export carried is lost, even where phase one authors nothing from it),
//! then media + posts oldest-first, then albums, then events. Between records
//! the machine checkpoints `state/import.cbor` into the folder, so a restart —
//! on this device or another — resumes from the folder alone (§ Storage: the
//! folder *is* the session; there is no nest-side twin).
//!
//! **The zip is read synchronously, never across an `await`.**
//! [`fauna_archive::ArchiveReader`] borrows its source and is `!Send`, so a
//! reader living across one of the nest seam's await points would make
//! `run_import`'s future `!Send` and un-`tokio::spawn`-able. Every zip read
//! here is therefore its own synchronous function ([`collect_category`],
//! [`read_media`], [`read_all`]) that opens a reader, takes what it needs and
//! drops it — the same rule `machine::index_archive` is written to.
//!
//! **Memory, honestly.** Three places hold more than a record at once, each a
//! captured follow-up rather than a surprise: the raw zip is read whole for
//! its upload ([`read_all`] — a streaming seal through the chunk store is the
//! fix), the glue's `write_file` then holds it beside its sealed chunk set,
//! and one record's media members are all read before its post is built
//! ([`read_media`] — a ten-video album is a ten-video allocation, cloned once
//! more per seam call). A multi-gigabyte export is fine to *read* — the
//! parser streams it member by member — and expensive only to *upload*.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

use fauna_archive::model::{ArchiveAlbum, ArchiveEvent, ArchiveMediaRef, ArchivePost, Rsvp};
use fauna_archive::{
    ArchiveReader, ArchiveSource, Category, Entity, EntityKind, ExternalId, Platform,
};
use fauna_client_core::post::{
    PostAuthoring, build_gated_post_at, build_post_at, build_post_with_media_at,
};
use fauna_core::data::{ContentHash, MediaItem, PostBody, Timestamp};

use crate::audience::{PostAudience, map_audience};
use crate::machine::{
    ArchiveImportMachine, DispatchError, RunHandle, StopRequest, index_archive, resolve_date_range,
    set_snapshot_error,
};
use crate::nest::{
    ArchiveNest, ArchiveNestError, ImportedEvent, MediaSeal, SharedSource, TierGate, UploadedMedia,
};
use crate::snapshot::{
    ArchiveImportSnapshot, ArchiveImportStatus, ArchiveImportStep, AudienceMode, RunState,
};
use crate::state::{
    ArchiveMarker, ImportPhase, ImportScope, ImportState, ImportTarget, ImportedRecord,
    InFlightPost, MARKER_PATH, STATE_PATH, SUMMARY_PATH, SkipEntry, StoredAudienceMode, model_path,
    raw_path,
};

/// The archive model's epoch-micros instant as Fauna's — the one place the two
/// `Timestamp` newtypes meet (`fauna-archive` carries its own so that it
/// depends on no `fauna-*` crate; the two are byte-identical on the wire).
fn at(t: fauna_archive::Timestamp) -> Timestamp {
    Timestamp(t.0)
}

/// The category an imported record belongs to — the inverse of the three
/// entity kinds phase one authors. `None` for a kind phase one never authors.
pub(crate) fn category_of_kind(kind: &EntityKind) -> Option<Category> {
    match kind {
        EntityKind::Post => Some(Category::Posts),
        EntityKind::Album => Some(Category::Albums),
        EntityKind::Event => Some(Category::Events),
        _ => None,
    }
}

/// Clears [`ArchiveImportMachine::running`] on **every** way out of
/// `run_import` — the returns, the `?`s, and a panic alike — so a run that
/// ends badly cannot leave the machine permanently refusing to run again.
struct InFlight<'a>(&'a AtomicBool);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// One record's outcome when it is not an imported record: a skip that the
/// log keeps and the run walks past, or a failure that stops the run with the
/// folder left resumable (§ The wizard and its machine, step 5).
enum AuthorOutcome {
    Skip(SkipEntry),
    Fatal(DispatchError),
}

/// A seam failure on ONE record: transport is fatal (the whole run stops and
/// resumes later), everything else is that record's skip, carrying the nest's
/// own code so the log says what the nest said.
fn classify(
    error: ArchiveNestError,
    category: Category,
    external_id: &ExternalId,
) -> AuthorOutcome {
    match error {
        ArchiveNestError::Transport(_) => AuthorOutcome::Fatal(DispatchError::Nest(error)),
        other => AuthorOutcome::Skip(SkipEntry {
            category,
            external_id: Some(external_id.clone()),
            reason: other.to_string(),
        }),
    }
}

fn skip(category: Category, external_id: &ExternalId, reason: &str) -> AuthorOutcome {
    AuthorOutcome::Skip(SkipEntry {
        category,
        external_id: Some(external_id.clone()),
        reason: reason.to_string(),
    })
}

/// The tiers a run gates under. Each is provisioned **on its first need** and
/// then reused for the rest of the run: an import with no followers-audience
/// record never mints the followers tier, one with no owner-only record never
/// mints that, and a public-only import never touches tiers at all. Both mints are
/// idempotent nest-side, so the lazy shape costs nothing a re-import would
/// have to undo (`archive-import.md` § Audience mapping — "Gating needs a live
/// tier").
#[derive(Default)]
struct TierGates {
    followers: Option<TierGate>,
    owner_only: Option<TierGate>,
    /// A permanent refusal per reserved tier — a `Rejected` from the seam:
    /// the followers key is lost, the nest offers the tier it should hide —
    /// remembered so every later record of that audience skip-logs at once
    /// instead of re-running the provisioning round trip.
    followers_refused: Option<ArchiveNestError>,
    owner_only_refused: Option<ArchiveNestError>,
}

impl TierGates {
    async fn provision(
        &mut self,
        audience: PostAudience,
        nest: &dyn ArchiveNest,
    ) -> Result<(), ArchiveNestError> {
        let (slot, refused) = match audience {
            PostAudience::Public => return Ok(()),
            PostAudience::Followers => (&mut self.followers, &mut self.followers_refused),
            PostAudience::OwnerOnly => (&mut self.owner_only, &mut self.owner_only_refused),
        };
        if slot.is_some() {
            return Ok(());
        }
        if let Some(e) = refused {
            return Err(e.clone());
        }
        let minted = match audience {
            PostAudience::Followers => nest.provision_followers_tier().await,
            _ => nest.provision_owner_only_tier().await,
        };
        match minted {
            Ok(gate) => {
                *slot = Some(gate);
                Ok(())
            }
            Err(e @ ArchiveNestError::Rejected { .. }) => {
                *refused = Some(e.clone());
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    fn get(&self, audience: PostAudience) -> Option<&TierGate> {
        match audience {
            PostAudience::Public => None,
            PostAudience::Followers => self.followers.as_ref(),
            PostAudience::OwnerOnly => self.owner_only.as_ref(),
        }
    }
}

/// Everything the per-record authoring needs that does not change inside a
/// run. Held by value/borrow rather than re-read per record so one run makes
/// exactly one `supports_hidden_tiers`, `calendar_ready` and dedup round trip.
struct RunContext<'a> {
    platform: Platform,
    scope: &'a ImportScope,
    hidden_tiers: bool,
    calendar_ready: bool,
    /// Every external id already imported by this folder *or* any other
    /// archive folder of the same platform (§ Storage — re-imports are
    /// continuous through dedup).
    dedup: &'a BTreeSet<ExternalId>,
    /// Media paths already carried by a post in `model/posts.cbor`; an album
    /// re-authors only what no post did.
    carried: &'a BTreeSet<String>,
    source: &'a SharedSource,
}

impl RunContext<'_> {
    /// The two record-level filters every category shares: the dedup map and
    /// the Scope step's optional date range.
    fn admit(
        &self,
        category: Category,
        external_id: &ExternalId,
        at: Timestamp,
    ) -> Result<(), AuthorOutcome> {
        if self.dedup.contains(external_id) {
            return Err(skip(category, external_id, "already imported"));
        }
        // A coarse filter over the archive's own timestamps, applied to every
        // authored category alike — a range that silently spared one of them
        // would not be the range the Scope step showed (§ The wizard, step 3).
        if !self.scope.admits_date(at) {
            return Err(skip(category, external_id, "outside the date range"));
        }
        Ok(())
    }

    /// § Audience mapping → "Gating needs a live tier": against a nest with no
    /// `hidden-tiers` the public categories still import and every non-public
    /// record is skip-logged with the reason that names the fix — never
    /// silently downgraded, because a signed post cannot be re-gated later.
    fn admit_audience(
        &self,
        category: Category,
        external_id: &ExternalId,
        audience: PostAudience,
    ) -> Result<(), AuthorOutcome> {
        if audience != PostAudience::Public && !self.hidden_tiers {
            return Err(skip(
                category,
                external_id,
                "this nest predates hidden tiers — update it, then re-import",
            ));
        }
        Ok(())
    }
}

/// One post to author, whatever archive record it came from.
struct Authored<'a> {
    category: Category,
    external_id: &'a ExternalId,
    created_at: Timestamp,
    audience: PostAudience,
    text: String,
    media: &'a [ArchiveMediaRef],
}

impl ArchiveImportMachine {
    /// Step 4's Start — the durable commit: folder + marker + the initial
    /// state, then Progress. The app spawns [`Self::run_import`] next, which
    /// uploads the raw zip and everything after it.
    ///
    /// Nothing is minted here: tiers are provisioned lazily at the first
    /// non-public post, so a public-only import never touches tiers.
    pub(crate) async fn start(&self) -> Result<(), DispatchError> {
        // Start is step 4's button and nothing else's: it creates a folder and
        // uploads an archive, so reaching it from anywhere but the Confirm
        // screen would commit an import the user never saw the totals for.
        if self.inner.lock().expect("snapshot mutex").step != ArchiveImportStep::Confirm {
            return Err(DispatchError::InvalidState(
                "confirm the import first".into(),
            ));
        }
        // The marker's `raw_file_name` is derived from `archive_path` while
        // the raw upload sends `self.source`, so the two must name the same
        // archive: a path change forgets the source (`machine::set_archive_path`),
        // which is exactly what makes refusing here sufficient.
        if self.source.lock().expect("source mutex").is_none() {
            return Err(DispatchError::InvalidState("open the archive first".into()));
        }
        let summary = self
            .summary
            .lock()
            .expect("summary mutex")
            .clone()
            .ok_or_else(|| DispatchError::InvalidState("open the archive first".into()))?;
        let (archive_path, scope, total) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            (
                snap.archive_path.clone(),
                scope_of(&snap)?,
                snap.confirm_records,
            )
        };
        let file_name = self.opener.file_name(&archive_path);

        let import_id = {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|e| DispatchError::Archive(e.to_string()))?;
            hex::encode(bytes)
        };
        let folder = self.create_named_folder(&summary.platform).await?;
        let marker = ArchiveMarker {
            platform: summary.platform.clone(),
            owner: summary.owner.clone(),
            import_id: import_id.clone(),
            parser_version: summary.parser_version,
            raw_file_name: file_name,
            created_at: Timestamp::now(),
            // Filled in by the raw upload, which is what reads the zip whole.
            raw_len: None,
            raw_blake3: None,
        };
        self.nest
            .write_file(&folder, MARKER_PATH, encode(&marker)?)
            .await?;
        let state = ImportState {
            import_id,
            scope,
            phase: ImportPhase::RawUpload,
            imported: vec![],
            skipped: vec![],
            updated_at: Timestamp::now(),
            in_flight: None,
            extra: Default::default(),
        };
        self.nest
            .write_file(&folder, STATE_PATH, encode(&state)?)
            .await?;

        // `stop` only — NOT `test_pause_after`. The test arm is armed BEFORE
        // Start by contract (`set_test_pause_after_records`; the e2e
        // restart-resume journey's causal anchor, convention 14), so clearing it
        // here would disarm the hook's only usage. It cannot leak either: it is
        // one-shot (`test_pause_due` spends it), the run's tail spends an
        // un-fired one, and its sole setter is compiled out of release builds.
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        *self.run.lock().expect("run mutex") = Some(RunHandle {
            folder: folder.clone(),
            state,
            marker,
        });

        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.step = ArchiveImportStep::Progress;
        snap.run_state = Some(RunState::Running);
        snap.folder_name = Some(folder);
        snap.total = total;
        snap.imported = 0;
        snap.skipped = 0;
        snap.errored = 0;
        snap.current_category = None;
        snap.skip_log.clear();
        for row in &mut snap.categories {
            row.imported = 0;
            row.skipped = 0;
        }
        snap.status = ArchiveImportStatus::Working;
        Ok(())
    }

    /// `<Label> archive <YYYY-MM-DD>`, with `-2`, `-3`, … on a name the
    /// Folders list already holds (§ Storage — one folder per import).
    async fn create_named_folder(&self, platform: &Platform) -> Result<String, DispatchError> {
        let base = format!("{} archive {}", platform.label(), today_ymd());
        for attempt in 1..=100u32 {
            let name = if attempt == 1 {
                base.clone()
            } else {
                format!("{base}-{attempt}")
            };
            match self.nest.create_folder(&name).await {
                Ok(()) => return Ok(name),
                Err(ArchiveNestError::Rejected { ref code, .. })
                    if code == "fauna.folders.conflict" => {}
                Err(other) => return Err(other.into()),
            }
        }
        Err(DispatchError::InvalidState(
            "a hundred archive folders were already imported today".into(),
        ))
    }

    /// The drive loop the app spawns after a successful `Start`/`Resume`
    /// (native `tokio::spawn`, wasm `spawn_local`). Returns when the run
    /// finishes, pauses, cancels or fails.
    ///
    /// **Single-flight.** A second call while one is in flight is refused
    /// outright: each loop drives its own clone of the `RunHandle`, so two
    /// would author the same records twice and then checkpoint conflicting
    /// maps over each other. The refusal returns before the error tail below,
    /// so it leaves the live run's rendered state exactly as it was.
    pub async fn run_import(&self) -> Result<(), DispatchError> {
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(DispatchError::InvalidState(
                "an import is already running".into(),
            ));
        }
        let _in_flight = InFlight(&self.running);
        let result = self.run_import_inner().await;
        if let Err(ref e) = result {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            set_snapshot_error(&mut snap.error, e.to_string());
            snap.run_state = Some(RunState::Errored);
            snap.status = ArchiveImportStatus::Idle;
        }
        result
    }

    async fn run_import_inner(&self) -> Result<(), DispatchError> {
        let Some(mut run) = self.run.lock().expect("run mutex").clone() else {
            return Err(DispatchError::InvalidState("no import to run".into()));
        };
        refuse_newer(&run)?;
        let source = self
            .source
            .lock()
            .expect("source mutex")
            .clone()
            .ok_or_else(|| DispatchError::InvalidState("open the archive first".into()))?;

        // Phase: the raw zip — ground truth, kept forever (§ Storage).
        if run.state.phase == ImportPhase::RawUpload {
            // The whole archive is materialized once here. Sealing it as a
            // stream through the chunk store is a captured follow-up; the
            // seam takes `Vec<u8>` today.
            let bytes = read_all(&source)?;
            // The marker learns the zip's length and hash from this one whole
            // read, so a later resume can tell the export it is handed back
            // from any other (`machine::verify_handed_back`).
            run.marker.raw_len = Some(bytes.len() as u64);
            run.marker.raw_blake3 = Some(blake3::hash(&bytes).to_hex().to_string());
            self.nest
                .write_file(&run.folder, MARKER_PATH, encode(&run.marker)?)
                .await?;
            self.nest
                .write_file(&run.folder, &raw_path(&run.marker.raw_file_name), bytes)
                .await?;
            run.state.phase = ImportPhase::Model;
            self.checkpoint(&mut run).await?;
        }

        // Phase: the model — EVERY category, so nothing the export carried is
        // lost even where phase one authors nothing from it.
        if run.state.phase == ImportPhase::Model {
            let (_, summary) = index_archive(&source)?;
            self.nest
                .write_file(&run.folder, SUMMARY_PATH, encode(&summary)?)
                .await?;
            for category in Category::ALL {
                let (entities, errors) = collect_category(&source, category.clone())?;
                for error in errors {
                    let entry = SkipEntry {
                        category: category.clone(),
                        external_id: None,
                        reason: format!("{}: {}", error.member, error.reason),
                    };
                    self.push_skip(&entry);
                    run.state.skipped.push(entry);
                }
                self.nest
                    .write_file(&run.folder, &model_path(&category), encode(&entities)?)
                    .await?;
            }
            run.state.phase = ImportPhase::Authoring {
                category: Category::Posts,
                next_index: 0,
            };
            self.checkpoint(&mut run).await?;
        }

        // `Finished` and `Cancelled` are end states. Re-entering the loop — a
        // second spawn, or a `Resume` dispatched after a cancel — must not
        // rewrite one into the other, or a cancelled import would report
        // itself complete and the Done screen would claim records nobody
        // authored.
        if run.state.is_finished() {
            self.conclude(match run.state.phase {
                ImportPhase::Cancelled => RunState::Cancelled,
                _ => RunState::Completed,
            });
            return Ok(());
        }

        // Phase: authoring — oldest-first, posts → albums → events.
        //
        // A gated post written ahead of a create that was never confirmed is
        // replayed FIRST — before the dedup set is built, so its record lands
        // in the map exactly once (`InFlightPost`).
        if let Some(pending) = run.state.in_flight.clone() {
            self.replay_in_flight(&mut run, pending).await?;
        }
        let hidden_tiers = self.nest.supports_hidden_tiers().await?;
        let calendar_ready = self.nest.calendar_ready().await?;
        let dedup = self.dedup_ids(&run.marker.platform, &run.state).await?;
        let carried = if run.state.scope.categories.contains(&Category::Albums) {
            self.post_media_paths(&run.folder).await?
        } else {
            BTreeSet::new()
        };
        let scope = run.state.scope.clone();
        let ctx = RunContext {
            platform: run.marker.platform.clone(),
            scope: &scope,
            hidden_tiers,
            calendar_ready,
            dedup: &dedup,
            carried: &carried,
            source: &source,
        };
        let mut gates = TierGates::default();
        // The in-run half of dedup (`ctx.dedup` is frozen at run start): two
        // records carrying one external id inside a model file author once.
        let mut seen: BTreeSet<ExternalId> = BTreeSet::new();
        let mut settled_this_run: u64 = 0;

        for category in [Category::Posts, Category::Albums, Category::Events] {
            if !scope.categories.contains(&category) {
                continue;
            }
            let ImportPhase::Authoring {
                category: at,
                next_index,
            } = &run.state.phase
            else {
                break;
            };
            let (at, next_index) = (at.clone(), *next_index);
            // `Category`'s derived order is `Category::ALL`'s, which is the
            // run's: a phase past this category means it is already done.
            if at > category {
                continue;
            }
            let bytes = self
                .nest
                .read_file(&run.folder, &model_path(&category))
                .await?
                .ok_or_else(|| {
                    DispatchError::Archive(format!(
                        "{} is missing from the archive folder",
                        model_path(&category)
                    ))
                })?;
            let mut entities: Vec<Entity> = decode(&bytes)?;
            // Oldest first (§ The wizard, step 5). Deterministic over a
            // deterministic model file, which is what makes `next_index` a
            // valid resume cursor into *this* order.
            entities.sort_by_key(created_at_of);

            let start = if at == category {
                next_index as usize
            } else {
                0
            };
            let mut imported_here = count_imported(&run.state, &category);
            let mut skipped_here = count_skipped(&run.state, &category);
            self.note_progress(&run.state, &category, imported_here, skipped_here);

            for (i, entity) in entities.iter().enumerate().skip(start) {
                match self.stop_requested() {
                    StopRequest::Pause => {
                        run.state.phase = ImportPhase::Authoring {
                            category: category.clone(),
                            next_index: i as u64,
                        };
                        self.checkpoint(&mut run).await?;
                        self.conclude(RunState::Paused);
                        return Ok(());
                    }
                    StopRequest::Cancel => {
                        // Cancel keeps what landed (§ The wizard, step 5).
                        run.state.phase = ImportPhase::Cancelled;
                        self.checkpoint(&mut run).await?;
                        self.conclude(RunState::Cancelled);
                        return Ok(());
                    }
                    StopRequest::None => {}
                }
                let outcome = match (entity, external_id_of(entity)) {
                    // A record a newer parser wrote (`Entity::Unknown`): the
                    // file decoded around it, and it is counted as skipped —
                    // never authored, never a failure of the file.
                    (Entity::Unknown(_), _) => Err(AuthorOutcome::Skip(SkipEntry {
                        category: category.clone(),
                        external_id: None,
                        reason: UNKNOWN_ENTITY_REASON.to_string(),
                    })),
                    (_, Some(id)) if !seen.insert(id.clone()) => Err(skip(
                        category.clone(),
                        id,
                        "a duplicate record in the archive — authored once",
                    )),
                    _ => self.author(entity, &ctx, &mut gates, &mut run).await,
                };
                match outcome {
                    Ok(Some(record)) => {
                        run.state.imported.push(record);
                        imported_here += 1;
                    }
                    Ok(None) => {}
                    Err(AuthorOutcome::Skip(entry)) => {
                        skipped_here += 1;
                        self.push_skip(&entry);
                        run.state.skipped.push(entry);
                    }
                    Err(AuthorOutcome::Fatal(e)) => {
                        // The failing record is where the resume restarts —
                        // and whatever was written ahead for it stays, so the
                        // resume replays rather than re-authors.
                        run.state.phase = ImportPhase::Authoring {
                            category: category.clone(),
                            next_index: i as u64,
                        };
                        self.checkpoint(&mut run).await?;
                        return Err(e);
                    }
                }
                // The record is settled either way: what was written ahead
                // for it is spent.
                run.state.in_flight = None;
                run.state.phase = ImportPhase::Authoring {
                    category: category.clone(),
                    next_index: i as u64 + 1,
                };
                self.checkpoint(&mut run).await?;
                self.note_progress(&run.state, &category, imported_here, skipped_here);
                settled_this_run += 1;
                if self.test_pause_due(settled_this_run) {
                    *self.stop.lock().expect("stop mutex") = StopRequest::Pause;
                }
            }
        }

        // The run genuinely IS complete here — every in-scope category ran
        // out of entities, so there is nothing left to pause or cancel. Spend
        // any un-fired test arm (an N at or past the run's total never fires
        // the loop's own check) and clear a stop request that arrived after
        // the last record settled (a `Pause`/`Cancel` racing the final
        // iteration): neither has anything left to act on, and leaving either
        // set would leak into the next run — `resume`/`start` reset `stop`
        // too (this is the third door).
        *self.test_pause_after.lock().expect("test pause mutex") = None;
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        run.state.phase = ImportPhase::Finished;
        self.checkpoint(&mut run).await?;
        self.conclude(RunState::Completed);
        Ok(())
    }

    /// `Pause` — the loop stops at the next record with the folder resumable.
    pub(crate) fn pause(&self) -> Result<(), DispatchError> {
        self.request_stop(StopRequest::Pause, "no import to pause")
    }

    /// `Cancel` — the loop stops and marks the folder's state cancelled;
    /// everything already authored stays (§ The wizard, step 5).
    ///
    /// With no loop in flight — paused, errored, or a folder found at hydrate
    /// — nobody would ever read the flag, so the durable transition is made
    /// here: the folder stops being a resume candidate on every device, and
    /// the page can leave Progress (`Back`). The `MailImportMachine::cancel`
    /// precedent commits synchronously too. A loop that IS in flight sees the
    /// flag at its next record and makes the same transition itself; the two
    /// converge on `Cancelled` whichever runs first.
    pub(crate) async fn cancel(&self) -> Result<(), DispatchError> {
        self.request_stop(StopRequest::Cancel, "no import to cancel")?;
        if self.running.load(Ordering::Acquire) {
            return Ok(());
        }
        let Some(mut run) = self.run.lock().expect("run mutex").clone() else {
            return Ok(());
        };
        if run.state.is_finished() {
            // Already an end state: nothing to write, the page reflects it.
            self.conclude(match run.state.phase {
                ImportPhase::Cancelled => RunState::Cancelled,
                _ => RunState::Completed,
            });
            return Ok(());
        }
        run.state.phase = ImportPhase::Cancelled;
        self.checkpoint(&mut run).await?;
        self.conclude(RunState::Cancelled);
        Ok(())
    }

    fn request_stop(&self, request: StopRequest, absent: &str) -> Result<(), DispatchError> {
        if self.run.lock().expect("run mutex").is_none() {
            return Err(DispatchError::InvalidState(absent.into()));
        }
        *self.stop.lock().expect("stop mutex") = request;
        Ok(())
    }

    /// `Resume` — clears the stop flag and puts the page back on Running; the
    /// app spawns [`Self::run_import`] again after it.
    ///
    /// A resume needs the archive itself. In-process it is still open; after a
    /// restart the folder's own `raw/` copy can serve range reads on platforms
    /// that can bridge them, and where it cannot the user hands the archive
    /// back through the Archive step's path field.
    pub(crate) async fn resume(&self) -> Result<(), DispatchError> {
        let Some((folder, raw_name)) = self
            .run
            .lock()
            .expect("run mutex")
            .as_ref()
            .map(|r| (r.folder.clone(), r.marker.raw_file_name.clone()))
        else {
            return Err(DispatchError::InvalidState("no import to resume".into()));
        };
        // Resume is for a run that stopped. Against one still in flight it
        // would only invite the app to spawn a second loop over the same
        // folder, which `run_import` then refuses — better to say so here,
        // where the button is.
        if self.inner.lock().expect("snapshot mutex").run_state == Some(RunState::Running) {
            return Err(DispatchError::InvalidState(
                "the import is already running".into(),
            ));
        }
        if self.source.lock().expect("source mutex").is_none() {
            match self
                .nest
                .open_folder_archive(&folder, &raw_path(&raw_name))
                .await?
            {
                Some(source) => *self.source.lock().expect("source mutex") = Some(source),
                None => {
                    return Err(DispatchError::InvalidState(
                        "open the archive again to resume".into(),
                    ));
                }
            }
        }
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.step = ArchiveImportStep::Progress;
        snap.folder_name = Some(folder);
        snap.run_state = Some(RunState::Running);
        snap.status = ArchiveImportStatus::Working;
        Ok(())
    }

    // --- the per-record authoring -------------------------------------

    async fn author(
        &self,
        entity: &Entity,
        ctx: &RunContext<'_>,
        gates: &mut TierGates,
        run: &mut RunHandle,
    ) -> Result<Option<ImportedRecord>, AuthorOutcome> {
        match entity {
            Entity::Post(post) => self.author_post(post, ctx, gates, run).await,
            Entity::Album(album) => self.author_album(album, ctx, gates, run).await,
            Entity::Event(event) => self.author_event(event, ctx).await,
            // Model-only in phase one: comments, reactions, threads, messages,
            // groups, friends and the profile stay in the folder for phase
            // two (§ What each category becomes).
            _ => Ok(None),
        }
    }

    async fn author_post(
        &self,
        post: &ArchivePost,
        ctx: &RunContext<'_>,
        gates: &mut TierGates,
        run: &mut RunHandle,
    ) -> Result<Option<ImportedRecord>, AuthorOutcome> {
        ctx.admit(Category::Posts, &post.external_id, at(post.created_at))?;
        let audience = map_audience(post.audience, &ctx.scope.audience_mode);
        ctx.admit_audience(Category::Posts, &post.external_id, audience)?;

        let mut text = post.text.clone().unwrap_or_default();
        // The export files a shared link beside the text rather than in it; a
        // post that already quotes its own link is left alone.
        let unquoted: Vec<&str> = post
            .links
            .iter()
            .map(String::as_str)
            .filter(|link| !text.contains(*link))
            .collect();
        if !unquoted.is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&unquoted.join("\n"));
        }

        self.author_content(
            Authored {
                category: Category::Posts,
                external_id: &post.external_id,
                created_at: at(post.created_at),
                audience,
                text,
                media: &post.media,
            },
            ctx,
            gates,
            run,
        )
        .await
    }

    async fn author_album(
        &self,
        album: &ArchiveAlbum,
        ctx: &RunContext<'_>,
        gates: &mut TierGates,
        run: &mut RunHandle,
    ) -> Result<Option<ImportedRecord>, AuthorOutcome> {
        ctx.admit(Category::Albums, &album.external_id, at(album.created_at))?;
        // "Album media not attached to a post" — media a post already carried
        // is that post's, and re-authoring it here would duplicate it.
        let media: Vec<ArchiveMediaRef> = album
            .media
            .iter()
            .filter(|m| !ctx.carried.contains(&m.path))
            .cloned()
            .collect();
        if media.is_empty() {
            // Settled, not silent: the record counts toward the total the
            // Confirm step showed, so it lands in the log as what it is —
            // otherwise the progress bar could never reach that total.
            return Err(skip(
                Category::Albums,
                &album.external_id,
                "nothing to import — every photo in this album already belongs to a post",
            ));
        }
        let audience = map_audience(album.audience, &ctx.scope.audience_mode);
        ctx.admit_audience(Category::Albums, &album.external_id, audience)?;

        let mut text = album.name.clone();
        if let Some(description) = album.description.as_deref().filter(|d| !d.is_empty()) {
            text.push('\n');
            text.push_str(description);
        }
        self.author_content(
            Authored {
                category: Category::Albums,
                external_id: &album.external_id,
                created_at: at(album.created_at),
                audience,
                text,
                media: &media,
            },
            ctx,
            gates,
            run,
        )
        .await
    }

    /// One calendar event, owner-only by construction: it rides the actor's
    /// own calendar, not a tier (§ What each category becomes, the events row).
    async fn author_event(
        &self,
        event: &ArchiveEvent,
        ctx: &RunContext<'_>,
    ) -> Result<Option<ImportedRecord>, AuthorOutcome> {
        ctx.admit(Category::Events, &event.external_id, at(event.start))?;
        if !ctx.calendar_ready {
            return Err(skip(
                Category::Events,
                &event.external_id,
                "calendar not enabled on this nest — enable it, then re-import",
            ));
        }
        let uid = format!("archive:{}:{}", ctx.platform.token(), event.external_id.id);
        let imported = ImportedEvent {
            uid: uid.clone(),
            summary: event.title.clone(),
            description: event.description.clone(),
            start: at(event.start),
            end: event.end.map(at),
            location: event.place.as_ref().map(|p| p.name.clone()),
            url: event.url.clone(),
            rsvp: rsvp_token(event.rsvp).to_string(),
        };
        self.nest
            .put_event(&imported)
            .await
            .map_err(|e| classify(e, Category::Events, &event.external_id))?;
        Ok(Some(ImportedRecord {
            external_id: event.external_id.clone(),
            target: ImportTarget::Event {
                uid_hash: blake3::hash(uid.as_bytes()).to_hex().to_string(),
            },
        }))
    }

    /// The one authoring path posts and albums share: media up first (public
    /// or sealed for the post's own audience), then the signed post — a gated
    /// one written ahead into the folder before its create leaves.
    async fn author_content(
        &self,
        item: Authored<'_>,
        ctx: &RunContext<'_>,
        gates: &mut TierGates,
        run: &mut RunHandle,
    ) -> Result<Option<ImportedRecord>, AuthorOutcome> {
        let paths: Vec<String> = item.media.iter().map(|m| m.path.clone()).collect();
        let blobs = if paths.is_empty() {
            Vec::new()
        } else {
            read_media(ctx.source, &paths).map_err(|reason| {
                AuthorOutcome::Skip(SkipEntry {
                    category: item.category.clone(),
                    external_id: Some(item.external_id.clone()),
                    reason,
                })
            })?
        };
        let authoring = PostAuthoring::imported(item.created_at, ctx.platform.token());
        let mut items = Vec::with_capacity(blobs.len());

        let post_bytes = match item.audience {
            PostAudience::Public => {
                for (name, bytes) in &blobs {
                    let uploaded = self
                        .nest
                        .upload_public_media(name, bytes.clone())
                        .await
                        .map_err(|e| classify(e, item.category.clone(), item.external_id))?;
                    items.push(media_item(uploaded));
                }
                if items.is_empty() {
                    build_post_at(&self.keypair, &item.text, &[], None, &authoring)
                } else {
                    build_post_with_media_at(
                        &self.keypair,
                        &item.text,
                        items,
                        &[],
                        None,
                        &authoring,
                    )
                }
                .map_err(|e| AuthorOutcome::Fatal(DispatchError::Archive(e.to_string())))?
            }
            gated => {
                gates
                    .provision(gated, &*self.nest)
                    .await
                    .map_err(|e| classify(e, item.category.clone(), item.external_id))?;
                let gate = gates.get(gated).expect("provisioned just above");
                // One seal id per post, as the compose leg mints one: the
                // media and the sealed body must open under the same key.
                let mut seal_id = [0u8; 32];
                getrandom::fill(&mut seal_id)
                    .map_err(|e| AuthorOutcome::Fatal(DispatchError::Archive(e.to_string())))?;
                for (name, bytes) in &blobs {
                    let seal = MediaSeal {
                        seal_id,
                        tier: gate.tier.clone(),
                        period_version: gate.period_version,
                        period_key: gate.period_key.clone(),
                    };
                    let uploaded = self
                        .nest
                        .upload_sealed_media(name, bytes.clone(), &seal)
                        .await
                        .map_err(|e| classify(e, item.category.clone(), item.external_id))?;
                    items.push(media_item(uploaded));
                }
                let full_body = if items.is_empty() {
                    PostBody::Text {
                        content: item.text.clone(),
                        facets: vec![],
                    }
                } else {
                    PostBody::TextWithMedia {
                        content: item.text.clone(),
                        facets: vec![],
                        items,
                    }
                };
                let build = build_gated_post_at(
                    &self.keypair,
                    &preview_of(&ctx.platform),
                    full_body,
                    &gate.tier,
                    gate.rank,
                    gate.key_blob_ref,
                    &gate.period_key,
                    seal_id,
                    &authoring,
                )
                .map_err(|e| AuthorOutcome::Fatal(DispatchError::Archive(e.to_string())))?;
                // Written ahead: from here on a crash replays these exact
                // bytes instead of sealing a second post (`InFlightPost`).
                self.write_ahead(
                    run,
                    InFlightPost {
                        external_id: item.external_id.clone(),
                        post_bytes: build.post_bytes.clone(),
                        sealed_body: build.encrypted_blob.clone(),
                    },
                )
                .await?;
                let echoed = self
                    .nest
                    .upload_gated_body(build.encrypted_blob)
                    .await
                    .map_err(|e| classify(e, item.category.clone(), item.external_id))?;
                // The blob store is content-addressed: a reply naming other
                // bytes than the ones the signed post points at would leave a
                // post nobody could ever open.
                if echoed != hex::encode(build.encrypted_ref) {
                    return Err(AuthorOutcome::Fatal(DispatchError::Archive(
                        "the nest's sealed-body reply does not name the uploaded blob".into(),
                    )));
                }
                build.post_bytes
            }
        };

        let post_id = self
            .nest
            .create_post(post_bytes)
            .await
            .map_err(|e| classify(e, item.category.clone(), item.external_id))?;
        Ok(Some(ImportedRecord {
            external_id: item.external_id.clone(),
            target: ImportTarget::Post { post_id },
        }))
    }

    /// The write-ahead for a gated post ([`InFlightPost`]): its bytes reach
    /// the folder before its create leaves, so a crash in between replays
    /// them rather than re-sealing a second post. A failed folder write is
    /// fatal — authoring past it would reopen exactly the window it closes.
    async fn write_ahead(
        &self,
        run: &mut RunHandle,
        pending: InFlightPost,
    ) -> Result<(), AuthorOutcome> {
        run.state.in_flight = Some(pending);
        self.checkpoint(run).await.map_err(AuthorOutcome::Fatal)
    }

    /// A gated post whose bytes were written ahead but whose create was never
    /// confirmed: re-send the same bytes. The sealed body's store and
    /// `fauna.posts.create` are content-addressed and the nest upserts by
    /// hash, so a create that did land is a no-op and one that did not lands
    /// now — exactly one post either way. A permanent refusal skip-logs the
    /// record (its bytes are dropped); a transport fault keeps it in flight
    /// for the next resume, with nothing checkpointed.
    async fn replay_in_flight(
        &self,
        run: &mut RunHandle,
        pending: InFlightPost,
    ) -> Result<(), DispatchError> {
        let category = category_of_kind(&pending.external_id.kind).unwrap_or(Category::Posts);
        let resent = async {
            let echoed = self
                .nest
                .upload_gated_body(pending.sealed_body.clone())
                .await?;
            Ok::<_, ArchiveNestError>((
                echoed,
                self.nest.create_post(pending.post_bytes.clone()).await?,
            ))
        }
        .await;
        match resent {
            Ok((echoed, post_id)) => {
                // The same check the first attempt made: the store must name
                // the blob the signed post points at.
                if gated_ref_of(&pending.post_bytes).is_some_and(|r| echoed != hex::encode(r)) {
                    return Err(DispatchError::Archive(
                        "the nest's sealed-body reply does not name the uploaded blob".into(),
                    ));
                }
                run.state.imported.push(ImportedRecord {
                    external_id: pending.external_id,
                    target: ImportTarget::Post { post_id },
                });
            }
            Err(e) => match classify(e, category, &pending.external_id) {
                AuthorOutcome::Skip(entry) => {
                    self.push_skip(&entry);
                    run.state.skipped.push(entry);
                }
                AuthorOutcome::Fatal(e) => return Err(e),
            },
        }
        run.state.in_flight = None;
        if let ImportPhase::Authoring { next_index, .. } = &mut run.state.phase {
            *next_index += 1;
        }
        self.checkpoint(run).await
    }

    // --- folder + snapshot bookkeeping --------------------------------

    /// Republishes the handle, then writes `state/import.cbor` back into the
    /// folder, so a `Resume` after this point continues from exactly here.
    ///
    /// Called after **every** authored record, and at every phase boundary.
    /// That is what makes "a crash re-authors nothing" true rather than
    /// aspirational: the record that was in flight is either already in the
    /// imported map — so the resume's dedup skips it — or was never created,
    /// or (gated) was written ahead and is replayed byte for byte. A batched
    /// cadence would leave the records authored since the last write absent
    /// from the map and re-authored on resume, as duplicate signed posts the
    /// user can only delete one by one.
    ///
    /// The handle is published BEFORE the write: on a write failure the
    /// in-memory handle still knows what the nest holds, so an in-process
    /// `Resume` continues after the record instead of re-authoring it; the
    /// folder is then one record behind, which the write-ahead (gated) and
    /// the byte-identical re-authoring (public, events) make safe.
    ///
    /// The price is honest, not small: every write re-seals the whole state
    /// (linear in records imported so far, so quadratic over a run) and lands
    /// one more version of the path in the folder's change log — ten thousand
    /// posts are ten thousand versions of `state/import.cbor`. A coalescing
    /// writer behind the seam, and a per-folder version cap, are the captured
    /// follow-up; the cadence itself is not a knob.
    ///
    /// **Never over a newer build's state.** A state or marker holding a value
    /// this build cannot read is refused here, before anything is published
    /// or written ([`refuse_newer`]): the stored bytes stay exactly as the
    /// newer build wrote them. The hydrate never hands such a state to a run
    /// in the first place; this is the write-side half of that rule.
    async fn checkpoint(&self, run: &mut RunHandle) -> Result<(), DispatchError> {
        refuse_newer(run)?;
        run.state.updated_at = Timestamp::now();
        *self.run.lock().expect("run mutex") = Some(run.clone());
        let bytes = encode(&run.state)?;
        self.nest.write_file(&run.folder, STATE_PATH, bytes).await?;
        Ok(())
    }

    /// The union of every same-platform archive folder's imported map with
    /// this run's own (§ Storage — "the wizard lists existing archive folders
    /// of that platform and consults their `state/import.cbor` maps").
    async fn dedup_ids(
        &self,
        platform: &Platform,
        state: &ImportState,
    ) -> Result<BTreeSet<ExternalId>, DispatchError> {
        let mut ids: BTreeSet<ExternalId> = state
            .imported
            .iter()
            .map(|r| r.external_id.clone())
            .collect();
        for folder in self.nest.list_archive_folders().await? {
            if &folder.marker.platform != platform {
                continue;
            }
            let Some(bytes) = self.nest.read_file(&folder.folder, STATE_PATH).await? else {
                continue;
            };
            let Ok(other) = fauna_cbor::decode_strict::<ImportState>(&bytes) else {
                continue;
            };
            ids.extend(other.imported.into_iter().map(|r| r.external_id));
        }
        Ok(ids)
    }

    async fn post_media_paths(&self, folder: &str) -> Result<BTreeSet<String>, DispatchError> {
        let Some(bytes) = self
            .nest
            .read_file(folder, &model_path(&Category::Posts))
            .await?
        else {
            return Ok(BTreeSet::new());
        };
        let entities: Vec<Entity> = decode(&bytes)?;
        Ok(entities
            .iter()
            .filter_map(|e| match e {
                Entity::Post(p) => Some(p.media.iter().map(|m| m.path.clone())),
                _ => None,
            })
            .flatten()
            .collect())
    }

    fn stop_requested(&self) -> StopRequest {
        *self.stop.lock().expect("stop mutex")
    }

    fn note_progress(
        &self,
        state: &ImportState,
        category: &Category,
        imported_here: u64,
        skipped_here: u64,
    ) {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.imported = state.imported.len() as u64;
        snap.skipped = state.skipped.len() as u64;
        snap.current_category = Some(category.token().to_string());
        if let Some(row) = snap
            .categories
            .iter_mut()
            .find(|r| r.token == category.token())
        {
            row.imported = imported_here;
            row.skipped = skipped_here;
        }
    }

    /// `archive-import-error-log`, newest last.
    fn push_skip(&self, entry: &SkipEntry) {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.skip_log.push(skip_line(entry));
    }

    /// The run is over for now. A completed run advances to step 6; a paused
    /// or cancelled one stays on Progress with what landed, so `Resume` (or a
    /// re-import) is one tap away (§ The wizard, steps 5–6).
    fn conclude(&self, state: RunState) {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        if state == RunState::Completed {
            snap.step = ArchiveImportStep::Done;
        }
        snap.run_state = Some(state);
        snap.current_category = None;
        snap.status = ArchiveImportStatus::Idle;
    }
}

// --- free helpers ------------------------------------------------------

/// The skip-log reason for a model record this build cannot read.
pub(crate) const UNKNOWN_ENTITY_REASON: &str =
    "a record written by a newer version of Fauna — not imported by this one";

/// Refuses a run whose state or marker holds a value this build cannot read
/// (`ImportState::holds_unknown`): such an import is not resumable by this
/// build, and nothing here may rewrite its state (`transport.md` § Schema
/// and forward-compat discipline → *Rule 3 in full*).
pub(crate) fn refuse_newer(run: &RunHandle) -> Result<(), DispatchError> {
    if run.state.holds_unknown() || run.marker.holds_unknown() {
        return Err(DispatchError::NewerImport);
    }
    Ok(())
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, DispatchError> {
    fauna_cbor::encode_canonical(value).map_err(|e| DispatchError::Archive(e.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, DispatchError> {
    fauna_cbor::decode_strict(bytes).map_err(|e| DispatchError::Archive(e.to_string()))
}

/// One `archive-import-error-log` line. Shared with the resume, which rebuilds
/// the whole log from the folder's own skip list, so a line the user reads
/// after a restart is the line the run wrote.
pub(crate) fn skip_line(entry: &SkipEntry) -> String {
    format!("{}: {}", entry.category.token(), entry.reason)
}

fn media_item(uploaded: UploadedMedia) -> MediaItem {
    MediaItem {
        blob_hash: ContentHash::from_digest_raw(uploaded.blob_hash),
        media_type: uploaded.media_type,
        size_bytes: uploaded.size_bytes,
        dimensions: None,
        thumbnail: None,
        remote_url: None,
        alt: None,
    }
}

/// A gated import's public preview — a neutral placeholder, **never any of
/// the post's own text**.
///
/// `build_gated_post_at`'s `preview` becomes the post's public plaintext body
/// (`ui/feed.md` § Encryption at rest): it is readable by the nest and by
/// everyone who can see the actor's posts, gate or no gate. On the compose leg
/// a teaser there is the author's deliberate choice; an import cannot make
/// that choice for them, so publishing the opening of a post the user marked
/// "Only me" — or of a friends-only post, past the followers tier — would be a
/// leak wearing a teaser's clothes (`archive-import.md` § Audience mapping).
fn preview_of(platform: &Platform) -> String {
    format!("{} import", platform.label())
}

/// The archive's RSVP vocabulary onto `fauna_status`'s.
fn rsvp_token(rsvp: Rsvp) -> &'static str {
    match rsvp {
        Rsvp::Joined | Rsvp::Hosted => "going",
        Rsvp::Interested => "interested",
        Rsvp::Declined => "declined",
        Rsvp::Invited | Rsvp::Unknown => "invited",
    }
}

/// The `encrypted_ref` a gated post names — what the sealed-body store's reply
/// must echo. `None` for anything that does not decode as a gated post.
fn gated_ref_of(post_bytes: &[u8]) -> Option<[u8; 32]> {
    fauna_client_core::post::decode_post(post_bytes)
        .ok()
        .and_then(|(post, _)| post.gated.map(|g| g.encrypted_ref.digest()))
}

/// The external id of a record phase one authors; `None` for the model-only
/// kinds, which the run never authors and so never needs to dedup.
fn external_id_of(entity: &Entity) -> Option<&ExternalId> {
    match entity {
        Entity::Post(p) => Some(&p.external_id),
        Entity::Album(a) => Some(&a.external_id),
        Entity::Event(e) => Some(&e.external_id),
        _ => None,
    }
}

/// The instant a record is filed under — what "oldest-first" sorts on.
fn created_at_of(entity: &Entity) -> Timestamp {
    let t = match entity {
        Entity::Post(p) => Some(p.created_at),
        Entity::Album(a) => Some(a.created_at),
        Entity::Event(e) => Some(e.start),
        Entity::Comment(c) => Some(c.created_at),
        Entity::Reaction(r) => Some(r.created_at),
        Entity::Message(m) => Some(m.created_at),
        Entity::Thread(t) => t.first_at,
        Entity::Profile(p) => p.registered_at,
        Entity::Group(g) => g.joined_at,
        Entity::Friendship(f) => f.since,
        // A record this build cannot read sorts first; it is skipped anyway.
        Entity::Unknown(_) => None,
    };
    t.map(at).unwrap_or(Timestamp(0))
}

fn count_imported(state: &ImportState, category: &Category) -> u64 {
    state
        .imported
        .iter()
        .filter(|r| category_of_kind(&r.external_id.kind).as_ref() == Some(category))
        .count() as u64
}

fn count_skipped(state: &ImportState, category: &Category) -> u64 {
    state
        .skipped
        .iter()
        .filter(|s| &s.category == category)
        .count() as u64
}

/// The Scope step, as the folder stores it.
fn scope_of(snap: &ArchiveImportSnapshot) -> Result<ImportScope, DispatchError> {
    let categories = snap
        .categories
        .iter()
        .filter(|row| row.importable && row.selected)
        .filter_map(|row| {
            Category::ALL
                .iter()
                .find(|c| c.token() == row.token)
                .cloned()
        })
        .collect();
    let (date_from, date_until_exclusive) = resolve_date_range(&snap.date_from, &snap.date_to)?;
    Ok(ImportScope {
        categories,
        audience_mode: match snap.audience_mode {
            AudienceMode::Original => StoredAudienceMode::Original,
            AudienceMode::OnlyMe => StoredAudienceMode::OnlyMe,
        },
        date_from,
        date_until_exclusive,
        extra: Default::default(),
    })
}

/// The whole archive as bytes — what the raw upload sends today, and what a
/// resume's handed-back check hashes. Positional, so it works over a file
/// source and an in-memory one alike.
pub(crate) fn read_all(source: &SharedSource) -> Result<Vec<u8>, DispatchError> {
    let len = usize::try_from(source.len())
        .map_err(|_| DispatchError::Archive("the archive is too large for this device".into()))?;
    let mut bytes = vec![0u8; len];
    let mut at = 0usize;
    while at < len {
        let read = source
            .read_at(at as u64, &mut bytes[at..])
            .map_err(|e| DispatchError::Archive(e.to_string()))?;
        if read == 0 {
            break;
        }
        at += read;
    }
    bytes.truncate(at);
    Ok(bytes)
}

/// One category's records and its per-record parse failures. Synchronous, and
/// the reader it opens dies with it (see the module docs).
fn collect_category(
    source: &SharedSource,
    category: Category,
) -> Result<(Vec<Entity>, Vec<fauna_archive::EntityError>), DispatchError> {
    let readable: &dyn ArchiveSource = &**source;
    let mut reader =
        ArchiveReader::open(readable).map_err(|e| DispatchError::Archive(e.to_string()))?;
    let (parser, _) = fauna_archive::detect(reader.directory())
        .ok_or_else(|| DispatchError::Archive("archive no longer recognized".into()))?;
    let mut entities = Vec::new();
    let mut errors = Vec::new();
    for record in parser.stream(&mut reader, category) {
        match record {
            Ok(entity) => entities.push(entity),
            Err(error) => errors.push(error),
        }
    }
    Ok((entities, errors))
}

/// The most media bytes ONE record's members may occupy at once.
///
/// `fauna_archive::reader::MAX_MEMBER_BYTES` bounds a single member; nothing
/// bounded the sum, so a record naming N members reached N × 256 MiB in memory
/// simultaneously — and the archive's own records choose N. 64 MiB is far above
/// any real post (a phone photo is single-digit MiB) and is the aggregate the
/// per-member cap was mistakenly read as already providing.
const MAX_RECORD_MEDIA_BYTES: u64 = 64 * 1024 * 1024;

/// The bytes of one record's media members, as `(file name, bytes)`.
///
/// Bounded in aggregate by [`MAX_RECORD_MEDIA_BYTES`]: the caller maps an `Err`
/// here onto a `SkipEntry`, so an oversized record is skipped with its reason
/// rather than failing the import (§ Parser contract rule 1).
///
/// A member the directory does not name under the path the parser recorded is
/// retried under [`fauna_archive::text::repair_facebook_mojibake`]: the export
/// escapes each UTF-8 byte of a non-ASCII path separately, so an album called
/// `Fotky z časové osy` reaches the model one way and the zip the other. Both
/// failing is the record's skip, carrying the member error.
fn read_media(source: &SharedSource, paths: &[String]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let readable: &dyn ArchiveSource = &**source;
    let mut reader = ArchiveReader::open(readable).map_err(|e| e.to_string())?;
    // `Vec::with_capacity(paths.len())` is safe to size from the record because
    // the parser caps that list (`MAX_RECORD_MEDIA_REFS`); it is the BYTES the
    // members carry that the record does not bound, hence the running budget.
    let mut out = Vec::with_capacity(paths.len());
    let mut held: u64 = 0;
    for path in paths {
        let (member, declared) = match reader.member_size(path) {
            Ok(size) => (path.clone(), size),
            Err(first) => {
                let repaired = fauna_archive::text::repair_facebook_mojibake(path);
                let size = reader
                    .member_size(&repaired)
                    .map_err(|_| first.to_string())?;
                (repaired.into_owned(), size)
            }
        };
        // Checked on the DECLARED size, before the read — the reader never
        // returns more than that — so the member that would cross the budget
        // is refused unread and the budget is the true peak. Checked after the
        // read, the crossing member was already in memory: the budget plus up
        // to one whole `MAX_MEMBER_BYTES` member.
        let would_hold = held.saturating_add(declared);
        if would_hold > MAX_RECORD_MEDIA_BYTES {
            // Refuse the RECORD, not the import: the caller turns this string
            // into a `SkipEntry` carrying the reason (§ Parser contract rule 1),
            // so the user sees which post was too large and the rest still runs.
            return Err(format!(
                "post's media exceeds the {} MiB a single record may hold at once \
                 (reached {} MiB over {} of {} files)",
                MAX_RECORD_MEDIA_BYTES / (1024 * 1024),
                would_hold / (1024 * 1024),
                out.len() + 1,
                paths.len(),
            ));
        }
        let bytes = reader.read_member(&member).map_err(|e| e.to_string())?;
        held = held.saturating_add(bytes.len() as u64);
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
        out.push((name, bytes));
    }
    Ok(out)
}

/// `YYYY-MM-DD` of today, UTC — the archive folder's name suffix. The
/// workspace's one civil-calendar implementation (`caltime`, priority #2),
/// integer-only, so the name is identical on every target.
fn today_ymd() -> String {
    let days = (Timestamp::now().0 / 1_000_000 / 86_400) as i64;
    let (year, month, day) = fauna_core::caltime::civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fauna_archive::testing::fixture_zip;
    use fauna_core::data::{PostBody, Timestamp};
    use fauna_core::subscription::OWNER_ONLY_TIER;

    use crate::machine::{ArchiveImportMachine, StopRequest};
    use crate::nest::fakes::{FakeNest, VecOpener};
    use crate::snapshot::{ArchiveImportAction as Act, ArchiveImportStep, AudienceMode, RunState};
    use crate::state::{ImportPhase, model_path};

    const ARCHIVE_PATH: &str = "/exports/facebook.zip";

    fn opener_for(zip: Vec<u8>) -> VecOpener {
        let mut opener = VecOpener::default();
        opener.insert(ARCHIVE_PATH, zip);
        opener
    }

    fn machine_over(nest: Arc<FakeNest>, zip: Vec<u8>) -> ArchiveImportMachine {
        ArchiveImportMachine::new(
            nest,
            Arc::new(opener_for(zip)),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        )
    }

    /// The wizard walked to the Confirm step over `nest`: hydrate, enter the
    /// path, open the archive, Next → Scope, Next → Confirm.
    async fn ready_on(nest: Arc<FakeNest>, zip: Vec<u8>) -> ArchiveImportMachine {
        let m = machine_over(nest, zip);
        m.hydrate().await.unwrap();
        m.dispatch(Act::SetArchivePath {
            value: ARCHIVE_PATH.into(),
        })
        .await
        .unwrap();
        m.dispatch(Act::OpenArchive).await.unwrap();
        m.dispatch(Act::Next).await.unwrap();
        m.dispatch(Act::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ArchiveImportStep::Confirm);
        m
    }

    async fn ready(zip: Vec<u8>) -> (ArchiveImportMachine, Arc<FakeNest>) {
        let nest = Arc::new(FakeNest::new());
        let m = ready_on(nest.clone(), zip).await;
        (m, nest)
    }

    /// No external id is authored twice — the property a per-record checkpoint
    /// exists to hold, and the one a duplicate signed post would break.
    fn assert_unique_imports(state: &crate::state::ImportState) {
        let mut seen = std::collections::BTreeSet::new();
        for record in &state.imported {
            assert!(
                seen.insert(record.external_id.clone()),
                "{:?} was imported twice",
                record.external_id
            );
        }
    }

    #[tokio::test]
    async fn a_full_run_authors_posts_albums_and_events_at_their_original_audiences() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.unwrap();
        let s = m.snapshot();
        assert_eq!(s.step, ArchiveImportStep::Done);
        assert_eq!(s.run_state, Some(RunState::Completed));

        // Folder + files.
        let folder = s.folder_name.clone().expect("folder");
        assert!(folder.starts_with("Facebook archive "), "{folder}");
        let files = nest.files_in(&folder);
        assert!(files.contains(&"fauna-archive.cbor".to_string()));
        assert!(files.contains(&"raw/facebook.zip".to_string()));
        assert!(files.contains(&"state/import.cbor".to_string()));
        for c in fauna_archive::Category::ALL {
            assert!(files.contains(&model_path(&c)), "{c:?}");
        }
        assert!(files.contains(&"model/summary.cbor".to_string()));

        // Posts: 3 good posts (1 skip from the parser) + 1 album post.
        let posts = nest.posts();
        assert_eq!(posts.len(), 4);
        let friends = posts
            .iter()
            .find(|p| p.gated.as_ref().is_some_and(|g| g.tier == "followers"))
            .expect("the friends-only post");
        assert_eq!(friends.created_at, Timestamp(1_600_000_000_000_000));
        assert_eq!(
            friends.origin.as_ref().map(|o| o.platform.as_str()),
            Some("facebook")
        );
        // The public body of a gated import is the neutral placeholder: the
        // preview is plaintext on the wire, so NONE of the sealed text may
        // appear in it (§ Audience mapping).
        assert_eq!(friends.body_text(), "Facebook import");
        for word in ["Dobrý", "den", "přátelé"] {
            assert!(
                !friends.body_text().contains(word),
                "the friends-only post leaks {word:?} in its public body: {:?}",
                friends.body_text()
            );
        }

        let park = posts
            .iter()
            .find(|p| p.gated.is_none() && p.has_media())
            .expect("the public photo post");
        assert_eq!(park.created_at, Timestamp(1_610_000_000_000_000));
        assert!(
            matches!(&park.body, PostBody::TextWithMedia { content, items, .. }
                if content == "At the park\n\nhttps://example.invalid/link" && items.len() == 1),
            "{:?}",
            park.body
        );

        let only_me: Vec<_> = posts
            .iter()
            .filter(|p| p.gated.as_ref().is_some_and(|g| g.tier == OWNER_ONLY_TIER))
            .collect();
        assert_eq!(
            only_me.len(),
            2,
            "the only-me post + the album (Unknown audience)"
        );
        for post in &only_me {
            assert_eq!(post.body_text(), "Facebook import");
            for word in ["Only me note", "Mobile uploads", "Album description"] {
                assert!(
                    !post.body_text().contains(word),
                    "an owner-only post leaks {word:?}: {:?}",
                    post.body_text()
                );
            }
        }

        // Media: one public, two sealed (the album's), none for the text posts.
        assert_eq!(
            nest.media_uploads()
                .iter()
                .filter(|(_, _, sealed)| !*sealed)
                .count(),
            1
        );
        assert_eq!(
            nest.media_uploads()
                .iter()
                .filter(|(_, _, sealed)| *sealed)
                .count(),
            2
        );

        // Events: three, owner-only by construction.
        assert_eq!(nest.events().len(), 3);
        assert!(
            nest.events()
                .iter()
                .any(|e| e.summary == "Fixture Picnic" && e.rsvp == "going")
        );

        // State: every authored record in the map, the parser skip in the log.
        let state = nest.read_state(&folder);
        assert_eq!(state.phase, ImportPhase::Finished);
        assert_eq!(state.imported.len(), 4 + 3);
        assert_eq!(state.skipped.len(), 1);
        assert!(state.skipped[0].reason.contains("timestamp"));
        assert_eq!((s.imported, s.skipped), (7, 1));

        // Tier provisioning: each reserved tier once.
        assert_eq!(nest.owner_only_calls(), 1);
        assert_eq!(nest.followers_calls(), 1);
    }

    #[tokio::test]
    async fn only_me_mode_gates_everything_to_the_owner_only_tier() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        m.dispatch(Act::SetAudienceMode {
            mode: AudienceMode::OnlyMe,
        })
        .await
        .unwrap();
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.unwrap();

        let posts = nest.posts();
        assert_eq!(posts.len(), 4);
        for post in &posts {
            assert_eq!(
                post.gated.as_ref().map(|g| g.tier.as_str()),
                Some(OWNER_ONLY_TIER),
                "{:?}",
                post.body_text()
            );
        }
        assert_eq!(
            nest.followers_calls(),
            0,
            "no followers tier is ever minted"
        );
        assert_eq!(nest.owner_only_calls(), 1);
        // Even the public photo post's media is sealed under only-me.
        assert!(
            nest.media_uploads().iter().all(|(_, _, sealed)| *sealed),
            "{:?}",
            nest.media_uploads()
        );
    }

    #[tokio::test]
    async fn a_nest_without_hidden_tiers_imports_public_only_and_skip_logs_the_rest() {
        let nest = Arc::new(FakeNest::new());
        nest.set_supports_hidden_tiers(false);
        let m = ready_on(nest.clone(), fixture_zip("facebook-json")).await;
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.unwrap();

        let posts = nest.posts();
        assert_eq!(posts.len(), 1, "the public photo post only");
        assert!(posts[0].gated.is_none());
        let state = nest.read_state(&m.snapshot().folder_name.unwrap());
        assert!(
            state
                .skipped
                .iter()
                .filter(|s| s.reason.contains("hidden tiers"))
                .count()
                >= 3
        );
        assert_eq!(
            nest.owner_only_calls() + nest.followers_calls(),
            0,
            "never mints against such a nest"
        );
        // Events ride the actor's own calendar, not a tier, so an old nest
        // imports them in full (§ Audience mapping).
        assert_eq!(nest.events().len(), 3);
    }

    #[tokio::test]
    async fn a_re_import_skips_what_the_folder_already_maps() {
        let nest = Arc::new(FakeNest::new());
        let first = ready_on(nest.clone(), fixture_zip("facebook-json")).await;
        first.dispatch(Act::Start).await.unwrap();
        first.run_import().await.unwrap();
        let first_folder = first.snapshot().folder_name.expect("folder");

        // A second export of the same platform lands in its own folder and
        // adds only what is new — here, nothing (§ Storage → re-imports).
        let again = ready_on(nest.clone(), fixture_zip("facebook-json")).await;
        again.dispatch(Act::Start).await.unwrap();
        again.run_import().await.unwrap();
        let second_folder = again.snapshot().folder_name.expect("folder");
        assert_ne!(second_folder, first_folder);

        assert_eq!(nest.posts().len(), 4, "no new posts");
        assert_eq!(nest.events().len(), 3, "no new events");
        let state = nest.read_state(&second_folder);
        assert!(state.imported.is_empty());
        assert_eq!(
            state
                .skipped
                .iter()
                .filter(|s| s.reason.contains("already imported"))
                .count(),
            7
        );
        assert_eq!(again.snapshot().imported, 0);
    }

    #[tokio::test]
    async fn a_run_resumes_from_the_folder_state_after_a_crash_mid_authoring() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        nest.fail_post_after(2);
        m.dispatch(Act::Start).await.unwrap();
        let err = m
            .run_import()
            .await
            .expect_err("the simulated fault surfaces");
        assert!(err.to_string().contains("simulated"), "{err}");
        let folder = m.snapshot().folder_name.expect("folder");
        let before = nest.read_state(&folder);
        assert!(matches!(before.phase, ImportPhase::Authoring { .. }));
        // Every authored record is checkpointed, so the map holds exactly what
        // the nest holds — the two posts that landed, no more and no fewer.
        assert_eq!(before.imported.len(), 2, "checkpointed before the fault");
        assert_eq!(nest.posts().len(), 2);
        assert_unique_imports(&before);
        assert_eq!(m.snapshot().run_state, Some(RunState::Errored));

        // A NEW machine (a restart) hydrates, finds the run, resumes: the two
        // already-authored posts are not re-created, everything else lands.
        nest.clear_fault();
        let m2 = machine_over(nest.clone(), fixture_zip("facebook-json"));
        m2.hydrate().await.unwrap();
        let resumed = m2.snapshot();
        assert!(resumed.resume_available);
        assert_eq!(resumed.step, ArchiveImportStep::Progress);
        assert_eq!(resumed.folder_name.as_deref(), Some(folder.as_str()));
        assert_eq!(resumed.total, 4 + 1 + 3, "posts + albums + events in scope");
        assert!(
            resumed.categories.iter().any(|c| c.token == "posts"),
            "the progress rows come back from model/summary.cbor"
        );
        // The rendered log matches its own count: the parser skip the model
        // phase logged is read back out of the folder, not lost with the
        // process (§ Storage — the folder is the session).
        assert_eq!(resumed.skipped, before.skipped.len() as u64);
        assert_eq!(resumed.skip_log.len(), before.skipped.len());
        assert!(
            resumed.skip_log[0].contains("timestamp"),
            "{:?}",
            resumed.skip_log
        );

        // The archive is handed back through the Archive step's path field.
        m2.dispatch(Act::SetArchivePath {
            value: ARCHIVE_PATH.into(),
        })
        .await
        .unwrap();
        m2.dispatch(Act::OpenArchive).await.unwrap();
        assert_eq!(m2.snapshot().step, ArchiveImportStep::Progress);
        m2.dispatch(Act::Resume).await.unwrap();
        m2.run_import().await.unwrap();

        // The resume creates exactly the records that were still missing: the
        // two already in the map are never re-authored, so there is no
        // duplicate signed post for the user to hunt down and delete.
        assert_eq!(nest.posts().len(), 4);
        let after = nest.read_state(&folder);
        assert_eq!(after.phase, ImportPhase::Finished);
        assert_eq!(after.imported.len(), 4 + 3);
        assert_unique_imports(&after);
        for record in &before.imported {
            assert!(
                after.imported.contains(record),
                "the resume rewrote {:?}",
                record.external_id
            );
        }
        assert_eq!(
            nest.files_in(&folder)
                .iter()
                .filter(|f| f.starts_with("raw/"))
                .count(),
            1,
            "the raw zip is not re-uploaded"
        );
    }

    /// A **hard** crash: the process dies where it stands, so none of the
    /// error paths' checkpoints run. The folder is then exactly as of the last
    /// per-record checkpoint — which is why that checkpoint is per-record.
    /// Everything authored before the record in flight is in the map and is
    /// not re-authored. The one record whose `create_post` had returned but
    /// whose checkpoint had not — here the only-me note, a GATED post — was
    /// written ahead, so the resume replays its exact bytes: the nest already
    /// holds them and upserts by hash, so no second post appears, where a
    /// re-*authoring* would have sealed the body under a fresh nonce into a
    /// duplicate. (A batched cadence would widen the in-flight window from
    /// one record to the whole batch.)
    ///
    /// Scheduler-dependent by design: on the current-thread runtime
    /// `#[tokio::test]` gives, `abort()` lands at the fake's `yield_now`
    /// inside `create_post` — after the post is stored, before the record's
    /// checkpoint. A multi-thread flavour would race the abort against the
    /// checkpoint and no longer pin the window.
    #[tokio::test]
    async fn a_hard_crash_replays_the_record_that_was_in_flight_without_a_duplicate() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        let m = Arc::new(m);
        m.dispatch(Act::Start).await.unwrap();
        let folder = m.snapshot().folder_name.expect("folder");

        let notify = nest.on_post_created();
        let running = tokio::spawn({
            let m = Arc::clone(&m);
            async move { m.run_import().await }
        });
        // Park on the third post — created, not yet checkpointed — and kill
        // the task there. Each barrier is re-armed before the next record can
        // run, so this is causal, not timed (e2e convention 14).
        for _ in 0..3 {
            let barrier = notify.notified();
            tokio::pin!(barrier);
            barrier.as_mut().enable();
            barrier.await;
        }
        running.abort();
        let _ = running.await;

        assert_eq!(nest.posts().len(), 3, "three posts reached the nest");
        let crashed = nest.read_state(&folder);
        assert_eq!(
            crashed.imported.len(),
            2,
            "the two records whose checkpoints landed"
        );
        let pending = crashed
            .in_flight
            .as_ref()
            .expect("the gated post in flight was written ahead of its create");
        assert!(
            nest.posts().iter().any(|p| {
                fauna_client_core::post::decode_post(&pending.post_bytes)
                    .map(|(q, _)| q == *p)
                    .unwrap_or(false)
            }),
            "the written-ahead bytes are the post the nest holds"
        );

        // A restart resumes from the folder and finishes the import.
        let m2 = machine_over(nest.clone(), fixture_zip("facebook-json"));
        m2.hydrate().await.unwrap();
        m2.dispatch(Act::SetArchivePath {
            value: ARCHIVE_PATH.into(),
        })
        .await
        .unwrap();
        m2.dispatch(Act::OpenArchive).await.unwrap();
        m2.dispatch(Act::Resume).await.unwrap();
        m2.run_import().await.unwrap();

        // Four records' worth of posts and not one more: the in-flight record
        // was replayed byte for byte, so the nest deduplicated it. A fifth
        // would be the re-authored duplicate the write-ahead exists to
        // prevent.
        assert_eq!(
            nest.posts().len(),
            4,
            "the in-flight record is replayed, never re-authored"
        );
        let state = nest.read_state(&folder);
        assert_eq!(state.phase, ImportPhase::Finished);
        assert_eq!(state.imported.len(), 4 + 3);
        assert_eq!(state.in_flight, None, "nothing left in flight");
        assert_unique_imports(&state);
    }

    /// Cancel with no loop in flight — an errored run here, but a paused one
    /// or a folder found at hydrate are the same case — makes the folder's
    /// state Cancelled itself, so the folder stops being a resume candidate on
    /// every device, and Back leaves the Progress screen for a fresh wizard.
    /// Without this the flag was never read, every hydrate re-landed on
    /// Progress, and the only way out was to let the run finish.
    #[tokio::test]
    async fn cancel_on_a_stopped_run_is_durable_and_back_leaves_the_progress_screen() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        nest.fail_post_after(1);
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.expect_err("the simulated fault");
        let folder = m.snapshot().folder_name.expect("folder");
        assert_eq!(m.snapshot().run_state, Some(RunState::Errored));

        m.dispatch(Act::Cancel).await.unwrap();
        assert_eq!(
            nest.read_state(&folder).phase,
            ImportPhase::Cancelled,
            "durable without a loop to read the flag"
        );
        let s = m.snapshot();
        assert_eq!(s.run_state, Some(RunState::Cancelled));
        assert_eq!(s.step, ArchiveImportStep::Progress, "cancel keeps the page");

        // Back from a concluded Progress screen is a fresh wizard.
        m.dispatch(Act::Back).await.unwrap();
        let s = m.snapshot();
        assert_eq!(s.step, ArchiveImportStep::Source);
        assert_eq!(s.folder_name, None);
        assert!(!s.resume_available);
        assert_eq!(s.run_state, None);

        // No device is ever offered this folder again; what landed stays.
        let m2 = machine_over(nest.clone(), fixture_zip("facebook-json"));
        m2.hydrate().await.unwrap();
        assert!(!m2.snapshot().resume_available);
        assert_eq!(nest.posts().len(), 1);
    }

    /// A permanent provisioning refusal — here the followers key is lost, the
    /// seam's `Rejected` — is that record's skip, never the run's end: the
    /// public post, the owner-only ones and the events still land, and the
    /// refusal is asked once (§ Audience mapping — never a silent downgrade,
    /// and never a run that stops at the same record on every resume).
    #[tokio::test]
    async fn a_permanent_provisioning_refusal_skip_logs_the_record_and_the_run_goes_on() {
        let nest = Arc::new(FakeNest::new());
        nest.refuse_followers_provision(crate::nest::ArchiveNestError::Rejected {
            code: "fauna.subscriptions.no_period_key".into(),
            detail: "no client-held period key for tier \"followers\"".into(),
        });
        let m = ready_on(nest.clone(), fixture_zip("facebook-json")).await;
        m.dispatch(Act::Start).await.unwrap();
        m.run_import()
            .await
            .expect("a refusal is a skip, not a fault");

        let posts = nest.posts();
        assert_eq!(
            posts.len(),
            3,
            "the public post, the only-me note, the album"
        );
        assert!(
            posts
                .iter()
                .all(|p| p.gated.as_ref().is_none_or(|g| g.tier == OWNER_ONLY_TIER)),
            "nothing was gated to followers"
        );
        let state = nest.read_state(&m.snapshot().folder_name.unwrap());
        assert_eq!(state.phase, ImportPhase::Finished);
        assert_eq!(
            state
                .skipped
                .iter()
                .filter(|s| s.reason.contains("no_period_key"))
                .count(),
            1,
            "the friends-only post is skip-logged with the seam's own reason"
        );
        assert_eq!(nest.followers_calls(), 1, "asked once, then remembered");
        assert_eq!(nest.events().len(), 3);
    }

    /// `Refresh` while a run is in flight — a page repaint, a navigation
    /// away and back — leaves the run exactly as it is. Re-scanning the folder
    /// would replace the loop's live counts with the last checkpoint's and
    /// flip the page to Paused with a Resume button the run then refuses.
    #[tokio::test]
    async fn refresh_during_a_run_keeps_it_running() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        let m = Arc::new(m);
        m.dispatch(Act::Start).await.unwrap();

        let first_post = nest.on_post_created();
        let first_post = first_post.notified();
        tokio::pin!(first_post);
        first_post.as_mut().enable();
        let running = tokio::spawn({
            let m = Arc::clone(&m);
            async move { m.run_import().await }
        });
        first_post.await;
        m.dispatch(Act::Refresh).await.unwrap();
        let s = m.snapshot();
        assert_eq!(s.run_state, Some(RunState::Running));
        assert_eq!(s.step, ArchiveImportStep::Progress);
        assert_eq!(s.nest_supports_hidden_tiers, Some(true));

        running
            .await
            .expect("the run task")
            .expect("the run finishes");
        assert_eq!(m.snapshot().step, ArchiveImportStep::Done);
        assert_eq!(nest.posts().len(), 4);
    }

    /// The archive handed back to a resume must be the export the import
    /// started from: the folder's model files came from it, and media is read
    /// from the zip by member path. A different zip is refused by the marker's
    /// length + hash, and the run stays where it was — still asking for the
    /// archive.
    #[tokio::test]
    async fn a_resume_refuses_an_archive_that_is_not_the_one_the_import_started_from() {
        let nest = Arc::new(FakeNest::new());
        nest.fail_post_after(1);
        let m = ready_on(nest.clone(), fixture_zip("facebook-json")).await;
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.expect_err("the simulated fault");
        nest.clear_fault();
        let folder = m.snapshot().folder_name.expect("folder");
        let marker: crate::state::ArchiveMarker = {
            use crate::nest::ArchiveNest as _;
            fauna_cbor::decode_strict(
                &nest
                    .read_file(&folder, crate::state::MARKER_PATH)
                    .await
                    .unwrap()
                    .expect("the marker was written"),
            )
            .expect("decode marker")
        };
        assert!(
            marker.raw_len.is_some() && marker.raw_blake3.is_some(),
            "the raw upload recorded the zip's length and hash: {marker:?}"
        );

        // A restart handed a DIFFERENT zip.
        let other = fauna_archive::testing::zip_of(&[("notes/todo.json", b"[]")]).0;
        let m2 = machine_over(nest.clone(), other);
        m2.hydrate().await.unwrap();
        assert!(m2.snapshot().resume_available);
        m2.dispatch(Act::SetArchivePath {
            value: ARCHIVE_PATH.into(),
        })
        .await
        .unwrap();
        let err = m2
            .dispatch(Act::OpenArchive)
            .await
            .expect_err("a different export is refused");
        assert!(err.to_string().contains("not the archive"), "{err}");
        let err = m2
            .dispatch(Act::Resume)
            .await
            .expect_err("nothing was opened");
        assert!(err.to_string().contains("open the archive again"), "{err}");

        // The right zip still resumes.
        let m3 = machine_over(nest.clone(), fixture_zip("facebook-json"));
        m3.hydrate().await.unwrap();
        m3.dispatch(Act::SetArchivePath {
            value: ARCHIVE_PATH.into(),
        })
        .await
        .unwrap();
        m3.dispatch(Act::OpenArchive).await.unwrap();
        m3.dispatch(Act::Resume).await.unwrap();
        m3.run_import().await.unwrap();
        assert_eq!(nest.read_state(&folder).phase, ImportPhase::Finished);
    }

    /// Two records carrying one external id inside a model file — a parser
    /// emitting the same post twice — author once: the in-run half of dedup,
    /// which the run-start set (frozen before any record lands) cannot see.
    #[tokio::test]
    async fn a_duplicate_record_inside_one_model_file_is_authored_once() {
        use crate::nest::ArchiveNest as _;
        let nest = Arc::new(FakeNest::new());
        let source: crate::nest::SharedSource =
            Arc::new(fauna_archive::VecSource(fixture_zip("facebook-json")));
        let (entities, _) =
            super::collect_category(&source, fauna_archive::Category::Posts).unwrap();
        let text_post = entities
            .iter()
            .find(|e| matches!(e, fauna_archive::Entity::Post(p) if p.media.is_empty()))
            .cloned()
            .expect("a text post in the fixture");
        let doubled = vec![text_post.clone(), text_post];

        // A folder paused at the start of its posts, whose model file holds
        // the doubled record.
        let folder = "Facebook archive doubled";
        nest.seed_folder(
            folder,
            &crate::state::ArchiveMarker {
                platform: fauna_archive::Platform::Facebook,
                owner: fauna_archive::ExternalActorRef::new(
                    fauna_archive::Platform::Facebook,
                    Some("test.owner.fixture".into()),
                    "Test Owner",
                ),
                import_id: "00".repeat(32),
                parser_version: fauna_archive::PARSER_VERSION,
                raw_file_name: "facebook.zip".into(),
                created_at: Timestamp(1),
                raw_len: None,
                raw_blake3: None,
            },
            &crate::state::ImportState {
                import_id: "00".repeat(32),
                scope: crate::state::ImportScope {
                    categories: vec![fauna_archive::Category::Posts],
                    audience_mode: crate::state::StoredAudienceMode::Original,
                    date_from: None,
                    date_until_exclusive: None,
                    extra: Default::default(),
                },
                phase: ImportPhase::Authoring {
                    category: fauna_archive::Category::Posts,
                    next_index: 0,
                },
                imported: vec![],
                skipped: vec![],
                updated_at: Timestamp(1),
                in_flight: None,
                extra: Default::default(),
            },
        );
        nest.write_file(
            folder,
            &model_path(&fauna_archive::Category::Posts),
            fauna_cbor::encode_canonical(&doubled).unwrap(),
        )
        .await
        .unwrap();

        let m = machine_over(nest.clone(), fixture_zip("facebook-json"));
        m.hydrate().await.unwrap();
        assert!(m.snapshot().resume_available);
        m.dispatch(Act::SetArchivePath {
            value: ARCHIVE_PATH.into(),
        })
        .await
        .unwrap();
        m.dispatch(Act::OpenArchive).await.unwrap();
        m.dispatch(Act::Resume).await.unwrap();
        m.run_import().await.unwrap();

        assert_eq!(nest.posts().len(), 1, "one post for one external id");
        let state = nest.read_state(folder);
        assert_eq!(state.imported.len(), 1);
        assert_unique_imports(&state);
        assert_eq!(
            state
                .skipped
                .iter()
                .filter(|s| s.reason.contains("duplicate record"))
                .count(),
            1
        );
    }

    /// Two loops over one folder would each drive their own `RunHandle` clone,
    /// authoring the same records twice and checkpointing over each other.
    #[tokio::test]
    async fn a_second_concurrent_run_over_the_same_folder_is_refused() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        let m = Arc::new(m);
        m.dispatch(Act::Start).await.unwrap();

        let one = tokio::spawn({
            let m = Arc::clone(&m);
            async move { m.run_import().await }
        });
        let two = tokio::spawn({
            let m = Arc::clone(&m);
            async move { m.run_import().await }
        });
        let results = [
            one.await.expect("run task one"),
            two.await.expect("run task two"),
        ];
        let refused: Vec<String> = results
            .iter()
            .filter_map(|r| r.as_ref().err().map(|e| e.to_string()))
            .collect();
        assert_eq!(refused.len(), 1, "exactly one loop runs: {refused:?}");
        assert!(refused[0].contains("already running"), "{}", refused[0]);

        // The surviving loop finished normally, once.
        assert_eq!(nest.posts().len(), 4);
        assert_eq!(nest.events().len(), 3);
        let folder = m.snapshot().folder_name.expect("folder");
        let state = nest.read_state(&folder);
        assert_eq!(state.phase, ImportPhase::Finished);
        assert_eq!(state.imported.len(), 4 + 3);
        assert_unique_imports(&state);
    }

    /// The other half of single-flight: `Resume` refuses at the button rather
    /// than inviting the app to spawn a second loop.
    #[tokio::test]
    async fn resume_is_refused_while_the_run_is_still_running() {
        let (m, _) = ready(fixture_zip("facebook-json")).await;
        m.dispatch(Act::Start).await.unwrap();
        assert_eq!(m.snapshot().run_state, Some(RunState::Running));
        let err = m
            .dispatch(Act::Resume)
            .await
            .expect_err("the run has not stopped");
        assert!(err.to_string().contains("already running"), "{err}");
    }

    /// Step 4's button belongs to step 4: reaching it from anywhere else would
    /// create a folder and upload an archive the user never confirmed.
    #[tokio::test]
    async fn start_is_refused_before_the_confirm_step() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        m.dispatch(Act::Back).await.unwrap();
        assert_eq!(m.snapshot().step, ArchiveImportStep::Scope);
        let err = m
            .dispatch(Act::Start)
            .await
            .expect_err("not the Confirm step");
        assert!(
            err.to_string().contains("confirm the import first"),
            "{err}"
        );
        assert_eq!(m.snapshot().folder_name, None);
        assert!(nest.posts().is_empty());
    }

    #[tokio::test]
    async fn resuming_without_the_archive_says_so_rather_than_half_running() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        nest.fail_post_after(1);
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.expect_err("the simulated fault");
        let m2 = machine_over(nest.clone(), fixture_zip("facebook-json"));
        m2.hydrate().await.unwrap();
        // The fake cannot bridge range reads onto the async seam, so the
        // folder-resident copy is unavailable and the user must hand the
        // archive back.
        let err = m2
            .dispatch(Act::Resume)
            .await
            .expect_err("no source, no folder-resident archive");
        assert!(err.to_string().contains("open the archive again"), "{err}");
    }

    #[tokio::test]
    async fn pause_stops_at_the_next_record_and_resume_continues_in_process() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        let m = Arc::new(m);
        m.dispatch(Act::Start).await.unwrap();

        // The barrier is the fake's own post-created signal, armed before the
        // run starts, so the pause lands after exactly one record whatever the
        // machine's speed (e2e convention 14 — no settle sleep).
        let first_post = nest.on_post_created();
        let first_post = first_post.notified();
        tokio::pin!(first_post);
        first_post.as_mut().enable();
        let running = tokio::spawn({
            let m = Arc::clone(&m);
            async move { m.run_import().await }
        });
        first_post.await;
        m.dispatch(Act::Pause).await.unwrap();
        running
            .await
            .expect("the run task")
            .expect("pause is not an error");

        let s = m.snapshot();
        assert_eq!(s.run_state, Some(RunState::Paused));
        assert_eq!(s.step, ArchiveImportStep::Progress);
        assert_eq!(s.imported, 1);
        assert_eq!(nest.posts().len(), 1);
        let folder = s.folder_name.clone().expect("folder");
        assert!(
            matches!(
                nest.read_state(&folder).phase,
                ImportPhase::Authoring { next_index: 1, .. }
            ),
            "{:?}",
            nest.read_state(&folder).phase
        );

        m.dispatch(Act::Resume).await.unwrap();
        assert_eq!(m.snapshot().run_state, Some(RunState::Running));
        m.run_import().await.unwrap();
        assert_eq!(nest.posts().len(), 4);
        assert_eq!(m.snapshot().step, ArchiveImportStep::Done);
        assert_eq!(nest.read_state(&folder).phase, ImportPhase::Finished);
    }

    /// The e2e restart-resume anchor (convention 14): armed before Start, the
    /// hook pauses the run through its ORDINARY pause arm after exactly N
    /// settled records, and is spent — so the resume runs to the end.
    #[tokio::test]
    async fn the_test_pause_hook_pauses_after_n_records_once() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        m.set_test_pause_after_records(2);
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.expect("a pause is not an error");

        let s = m.snapshot();
        assert_eq!(s.run_state, Some(RunState::Paused));
        assert_eq!(s.step, ArchiveImportStep::Progress);
        // 1 (the fixture's pre-existing Model-phase parser skip, present
        // before the authoring loop even starts — see the `state.skipped`
        // assertions in `a_full_run_authors_posts_albums_and_events_...`
        // above) + the 2 records settled by this loop.
        assert_eq!(s.imported + s.skipped, 3);
        assert_eq!(nest.posts().len(), 2);
        let folder = s.folder_name.clone().expect("folder");
        assert!(matches!(
            nest.read_state(&folder).phase,
            ImportPhase::Authoring { next_index: 2, .. }
        ));

        m.dispatch(Act::Resume).await.unwrap();
        m.run_import().await.unwrap();
        assert_eq!(
            m.snapshot().step,
            ArchiveImportStep::Done,
            "the hook is one-shot"
        );
        assert_eq!(nest.posts().len(), 4);
        assert_eq!(nest.read_state(&folder).phase, ImportPhase::Finished);
    }

    /// An arm set past the run's total record count never fires the loop's
    /// own check (there's no settled record left for it to fire on) — the run
    /// completes normally, and the tail of `run_import_inner` spends the
    /// un-fired arm and clears `stop` so neither leaks into a later run.
    #[tokio::test]
    async fn an_arm_larger_than_the_run_is_spent_when_the_run_completes() {
        let (m, _nest) = ready(fixture_zip("facebook-json")).await;
        m.set_test_pause_after_records(1_000);
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.unwrap();

        let s = m.snapshot();
        assert_eq!(s.step, ArchiveImportStep::Done);
        assert_eq!(s.run_state, Some(RunState::Completed));
        assert!(!m.test_pause_due(u64::MAX));
        assert_eq!(*m.stop.lock().unwrap(), StopRequest::None);
    }

    #[tokio::test]
    async fn cancel_keeps_what_landed_and_marks_the_state_cancelled() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        let m = Arc::new(m);
        m.dispatch(Act::Start).await.unwrap();

        let first_post = nest.on_post_created();
        let first_post = first_post.notified();
        tokio::pin!(first_post);
        first_post.as_mut().enable();
        let running = tokio::spawn({
            let m = Arc::clone(&m);
            async move { m.run_import().await }
        });
        first_post.await;
        m.dispatch(Act::Cancel).await.unwrap();
        running
            .await
            .expect("the run task")
            .expect("cancel is not an error");

        let s = m.snapshot();
        assert_eq!(s.run_state, Some(RunState::Cancelled));
        assert_eq!(s.step, ArchiveImportStep::Progress, "cancel keeps the page");
        assert_eq!(nest.posts().len(), 1, "what landed stays");
        let folder = s.folder_name.clone().expect("folder");
        assert_eq!(nest.read_state(&folder).phase, ImportPhase::Cancelled);
        assert_eq!(nest.read_state(&folder).imported.len(), 1);

        // Cancelled is an end state: running the loop again over it must not
        // report the import complete, nor author anything more.
        m.dispatch(Act::Resume).await.unwrap();
        m.run_import().await.unwrap();
        assert_eq!(nest.read_state(&folder).phase, ImportPhase::Cancelled);
        assert_eq!(m.snapshot().run_state, Some(RunState::Cancelled));
        assert_eq!(m.snapshot().step, ArchiveImportStep::Progress);
        assert_eq!(nest.posts().len(), 1);
    }

    #[tokio::test]
    async fn the_date_range_filters_records_and_the_skip_log_says_so() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        m.dispatch(Act::SetDateFrom {
            value: "2021-01-01".into(),
        })
        .await
        .unwrap();
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.unwrap();

        // 2021: the park post (Jan) and the only-me note (May). 2020: the
        // first status (Sep) and the album (Jan) — both outside.
        let posts = nest.posts();
        assert_eq!(
            posts.len(),
            2,
            "{:?}",
            posts.iter().map(|p| p.body_text()).collect::<Vec<_>>()
        );
        assert!(
            posts
                .iter()
                .all(|p| p.created_at >= Timestamp(1_609_459_200_000_000))
        );
        let state = nest.read_state(&m.snapshot().folder_name.unwrap());
        assert_eq!(
            state
                .skipped
                .iter()
                .filter(|s| s.reason.contains("outside the date range"))
                .count(),
            2
        );
        assert_eq!(nest.events().len(), 3, "every fixture event is after it");
    }

    /// `mail-export.md` § UX shape step 2's rule, which `archive-import.md`
    /// step 3 adopts: `until` names a whole UTC day, so the park post at
    /// 06:13:20 on 2021-01-07 is inside "until 2021-01-07".
    #[tokio::test]
    async fn the_until_date_includes_the_whole_day_it_names() {
        let (m, nest) = ready(fixture_zip("facebook-json")).await;
        m.dispatch(Act::SetDateFrom {
            value: "2021-01-01".into(),
        })
        .await
        .unwrap();
        m.dispatch(Act::SetDateTo {
            value: "2021-01-07".into(),
        })
        .await
        .unwrap();
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.unwrap();

        let posts = nest.posts();
        assert_eq!(
            posts.iter().map(|p| p.created_at).collect::<Vec<_>>(),
            vec![Timestamp(1_610_000_000_000_000)],
            "only the park post"
        );
    }

    #[tokio::test]
    async fn events_are_skipped_when_the_calendar_is_not_ready() {
        let nest = Arc::new(FakeNest::new());
        nest.set_calendar_ready(false);
        let m = ready_on(nest.clone(), fixture_zip("facebook-json")).await;
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.unwrap();

        assert!(nest.events().is_empty());
        let state = nest.read_state(&m.snapshot().folder_name.unwrap());
        assert_eq!(
            state
                .skipped
                .iter()
                .filter(|s| s.reason.contains("calendar"))
                .count(),
            3
        );
        assert_eq!(nest.posts().len(), 4, "posts are unaffected");
    }

    // ── Aggregate resource bounds ──
    //
    // Lifted from the review that red-verified them, with each BUDGET now the
    // real constant instead of "any ceiling at all". `MAX_MEMBER_BYTES` bounds
    // ONE member; these bound what a single record can make the importer hold
    // together, which is the number the archive's own records choose.

    /// An `ArchiveSource` that counts the bytes read through it.
    struct CountingSource {
        inner: fauna_archive::source::VecSource,
        read: std::sync::atomic::AtomicU64,
    }

    impl fauna_archive::ArchiveSource for CountingSource {
        fn len(&self) -> u64 {
            fauna_archive::ArchiveSource::len(&self.inner)
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = fauna_archive::ArchiveSource::read_at(&self.inner, offset, buf)?;
            self.read
                .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            Ok(n)
        }
    }

    /// Arm 1 — `read_media` materializes every media member of one record at
    /// once, so the sum needs its own ceiling: the only other cap,
    /// `MAX_MEMBER_BYTES`, is per member, so this shape reached N × 256 MiB.
    /// And the ceiling is checked on a member's declared size BEFORE it is
    /// read, so the member that would cross it never enters memory — checked
    /// after the read, the true peak was the budget plus one whole member.
    ///
    /// Mutating `MAX_MEMBER_BYTES` does NOT redden this: that is the point.
    #[test]
    fn read_media_refuses_a_record_whose_media_exceeds_the_aggregate_budget() {
        // Two members either side of the budget, so the refusal is the budget's
        // and not a member's.
        const EACH: usize = 48 * 1024 * 1024;
        let blob = vec![0u8; EACH];
        let names: Vec<String> = (0..2).map(|i| format!("media/{i}.jpg")).collect();
        let members: Vec<(&str, &[u8])> = names
            .iter()
            .map(|n| (n.as_str(), blob.as_slice()))
            .collect();
        let counting = std::sync::Arc::new(CountingSource {
            inner: fauna_archive::testing::zip_of(&members),
            read: std::sync::atomic::AtomicU64::new(0),
        });
        let source: crate::nest::SharedSource = counting.clone();

        assert!(
            (EACH as u64) < fauna_archive::reader::MAX_MEMBER_BYTES,
            "each member must be individually legal, or this tests the wrong cap"
        );

        let err = super::read_media(&source, &names)
            .expect_err("two 48 MiB members exceed the 64 MiB one record may hold at once");
        assert!(
            err.contains("media exceeds"),
            "the skip reason must say what was exceeded, so the user can see \
             which post was dropped and why; got {err:?}"
        );
        let read = counting.read.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            read < (EACH + EACH / 2) as u64,
            "the member that would cross the budget must be refused on its \
             declared size, never read: {read} bytes came through for a record \
             refused after its first {EACH}-byte member"
        );

        // One member alone stays under the budget and still reads.
        let out = super::read_media(&source, &names[..1]).expect("one member is within budget");
        assert_eq!(out.len(), 1, "a normal record is unaffected");
    }

    /// Arm 2 — what made arm 1's N attacker-chosen: the parser copied every
    /// `attachments[].data[].media` a record named, with no cap on the count, so
    /// one record decided how many members `read_media` would later hold.
    ///
    /// `serde_json` is not a dev-dependency of this crate, so the JSON is text.
    #[test]
    fn a_post_record_may_not_name_unboundedly_many_media_members() {
        const N: usize = 5_000;
        let cap = fauna_archive::facebook::posts::MAX_RECORD_MEDIA_REFS;

        let data = (0..N)
            .map(|i| format!(r#"{{"media":{{"uri":"media/{i}.jpg"}}}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let bytes = format!(
            r#"[{{"timestamp":1600000000,"data":[{{"post":"hello"}}],"attachments":[{{"data":[{data}]}}]}}]"#
        )
        .into_bytes();

        // An otherwise empty archive: the media members are absent, so hashing
        // leaves them unhashed — the count is what is under test.
        let empty = fauna_archive::testing::zip_of(&[("posts/your_posts_1.json", b"[]")]);
        let readable: &dyn fauna_archive::ArchiveSource = &empty;
        let mut reader = fauna_archive::ArchiveReader::open(readable).expect("open");

        let out = fauna_archive::facebook::posts::parse_posts(
            &mut reader,
            "posts/your_posts_1.json",
            bytes,
        );

        let post = out
            .iter()
            .find_map(|r| match r {
                Ok(fauna_archive::Entity::Post(p)) => Some(p),
                _ => None,
            })
            .expect("the post still imports — rule 1 does not fail a whole record over this");
        assert_eq!(
            post.media.len(),
            cap,
            "the per-record media list must be capped at {cap}"
        );

        // …and the truncation is VISIBLE. A silent trim would be data loss the
        // user never learns about (§ Architectural rules — everything in the
        // folder is user-visible; § Parser contract rule 1 — with the reason).
        let note = out
            .iter()
            .find_map(|r| r.as_ref().err())
            .expect("the dropped tail must land in the skip log, not vanish");
        assert!(
            note.reason.contains("skipped") && note.reason.contains(&(N - cap).to_string()),
            "the skip line must name how many were dropped; got {:?}",
            note.reason
        );
    }

    // --- a newer build's folder (`transport.md` § Rule 3 in full) -------

    fn facebook_marker() -> crate::state::ArchiveMarker {
        crate::state::ArchiveMarker {
            platform: fauna_archive::Platform::Facebook,
            owner: fauna_archive::ExternalActorRef::new(
                fauna_archive::Platform::Facebook,
                Some("test.owner.fixture".into()),
                "Test Owner",
            ),
            import_id: "00".repeat(32),
            parser_version: fauna_archive::PARSER_VERSION,
            raw_file_name: "facebook.zip".into(),
            created_at: Timestamp(1),
            raw_len: None,
            raw_blake3: None,
        }
    }

    fn paused_at_posts(categories: Vec<fauna_archive::Category>) -> crate::state::ImportState {
        crate::state::ImportState {
            import_id: "00".repeat(32),
            scope: crate::state::ImportScope {
                categories,
                audience_mode: crate::state::StoredAudienceMode::Original,
                date_from: None,
                date_until_exclusive: None,
                extra: Default::default(),
            },
            phase: ImportPhase::Authoring {
                category: fauna_archive::Category::Posts,
                next_index: 0,
            },
            imported: vec![],
            skipped: vec![],
            updated_at: Timestamp(1),
            in_flight: None,
            extra: Default::default(),
        }
    }

    /// The fixture's posts, as the model phase would write them.
    fn fixture_posts() -> Vec<fauna_archive::model::ArchivePost> {
        let source: crate::nest::SharedSource =
            Arc::new(fauna_archive::VecSource(fixture_zip("facebook-json")));
        let (entities, _) =
            super::collect_category(&source, fauna_archive::Category::Posts).unwrap();
        entities
            .into_iter()
            .filter_map(|e| match e {
                fauna_archive::Entity::Post(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    /// An unfinished import a newer build wrote — a phase and a scope field
    /// this build cannot read — is readable for dedup, refused for resume, and
    /// never rewritten: its bytes are the newer build's after a hydrate, a
    /// Resume, a Cancel and a whole fresh import of the same archive.
    #[tokio::test]
    async fn a_newer_builds_import_still_dedups_but_is_never_resumed_or_rewritten() {
        use crate::nest::ArchiveNest as _;
        use crate::state::{ImportTarget, ImportedRecord, STATE_PATH};

        let already = fixture_posts()
            .first()
            .expect("a post in the fixture")
            .external_id
            .clone();
        let nest = Arc::new(FakeNest::new());
        let folder = "Facebook archive newer";
        nest.seed_folder(
            folder,
            &facebook_marker(),
            &paused_at_posts(vec![fauna_archive::Category::Posts]),
        );
        let newer = crate::state::unknown_arm_tests::newer_state_bytes(&[ImportedRecord {
            external_id: already.clone(),
            target: ImportTarget::Post {
                post_id: "aa".into(),
            },
        }]);
        nest.write_file(folder, STATE_PATH, newer.clone())
            .await
            .unwrap();
        let stored = || async { nest.read_file(folder, STATE_PATH).await.unwrap().unwrap() };

        // Not a resume candidate: the wizard starts fresh, and neither Resume
        // nor Cancel has a run to act on.
        let m = machine_over(nest.clone(), fixture_zip("facebook-json"));
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert!(!snap.resume_available);
        assert_eq!(snap.step, ArchiveImportStep::Source);
        assert!(m.dispatch(Act::Resume).await.is_err());
        assert!(m.dispatch(Act::Cancel).await.is_err());
        assert_eq!(stored().await, newer);

        // A fresh import of the same archive dedups against the newer
        // build's imported map: what was imported stays imported.
        let m = ready_on(nest.clone(), fixture_zip("facebook-json")).await;
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.unwrap();
        let fresh = m.snapshot().folder_name.expect("the fresh import's folder");
        assert_ne!(fresh, folder);
        let state = nest.read_state(&fresh);
        assert_eq!(state.phase, ImportPhase::Finished);
        assert!(!state.has(&already), "authored again: {already:?}");
        assert!(
            state
                .skipped
                .iter()
                .any(|s| s.external_id.as_ref() == Some(&already) && s.reason == "already imported"),
            "{:?}",
            state.skipped
        );
        assert_eq!(stored().await, newer);
    }

    /// The write-side half: a run handed a newer build's state — however it
    /// got one — refuses with the typed error before writing anything.
    #[tokio::test]
    async fn a_run_over_a_newer_builds_state_refuses_and_writes_nothing() {
        use crate::nest::ArchiveNest as _;
        use crate::state::STATE_PATH;

        let nest = Arc::new(FakeNest::new());
        let folder = "Facebook archive newer";
        nest.seed_folder(
            folder,
            &facebook_marker(),
            &paused_at_posts(vec![fauna_archive::Category::Posts]),
        );
        let newer = crate::state::unknown_arm_tests::newer_state_bytes(&[]);
        nest.write_file(folder, STATE_PATH, newer.clone())
            .await
            .unwrap();
        let state: crate::state::ImportState = fauna_cbor::decode_strict(&newer).unwrap();
        assert!(state.holds_unknown());

        let m = machine_over(nest.clone(), fixture_zip("facebook-json"));
        *m.run.lock().unwrap() = Some(crate::machine::RunHandle {
            folder: folder.into(),
            state,
            marker: facebook_marker(),
        });
        assert!(matches!(
            m.cancel().await,
            Err(crate::machine::DispatchError::NewerImport)
        ));
        assert!(matches!(
            m.run_import().await,
            Err(crate::machine::DispatchError::NewerImport)
        ));
        assert_eq!(
            nest.read_file(folder, STATE_PATH).await.unwrap().unwrap(),
            newer
        );

        // A marker naming a platform this build lacks refuses the same way.
        let mut marker = facebook_marker();
        marker.platform = fauna_archive::Platform::Other("mastodon".into());
        *m.run.lock().unwrap() = Some(crate::machine::RunHandle {
            folder: folder.into(),
            state: paused_at_posts(vec![fauna_archive::Category::Posts]),
            marker,
        });
        assert!(matches!(
            m.cancel().await,
            Err(crate::machine::DispatchError::NewerImport)
        ));
        assert_eq!(
            nest.read_file(folder, STATE_PATH).await.unwrap().unwrap(),
            newer
        );
    }

    /// A model file holding an entity a newer parser wrote decodes with the
    /// rest: the known records import, the unknown one is counted as skipped,
    /// and neither the authoring read nor the album pass's read of the posts
    /// file fails over it.
    #[tokio::test]
    async fn a_model_file_with_an_unknown_entity_imports_the_rest_and_skips_it() {
        use crate::nest::ArchiveNest as _;
        use fauna_archive::Category;

        #[derive(serde::Serialize)]
        #[serde(rename_all = "snake_case")]
        enum NewerEntity {
            Post(Box<fauna_archive::model::ArchivePost>),
            Story { seen: u64, tags: Vec<String> },
        }

        let posts: Vec<_> = fixture_posts().into_iter().take(2).collect();
        assert_eq!(posts.len(), 2, "two posts in the fixture");
        let nest = Arc::new(FakeNest::new());
        let folder = "Facebook archive mixed";
        nest.seed_folder(
            folder,
            &facebook_marker(),
            &paused_at_posts(vec![Category::Posts, Category::Albums]),
        );
        let mixed = vec![
            NewerEntity::Post(Box::new(posts[0].clone())),
            NewerEntity::Story {
                seen: 3,
                tags: vec!["x".into()],
            },
            NewerEntity::Post(Box::new(posts[1].clone())),
        ];
        nest.write_file(
            folder,
            &model_path(&Category::Posts),
            fauna_cbor::encode_canonical(&mixed).unwrap(),
        )
        .await
        .unwrap();
        nest.write_file(
            folder,
            &model_path(&Category::Albums),
            fauna_cbor::encode_canonical(&Vec::<fauna_archive::Entity>::new()).unwrap(),
        )
        .await
        .unwrap();

        let m = machine_over(nest.clone(), fixture_zip("facebook-json"));
        m.hydrate().await.unwrap();
        assert!(m.snapshot().resume_available);
        m.dispatch(Act::SetArchivePath {
            value: ARCHIVE_PATH.into(),
        })
        .await
        .unwrap();
        m.dispatch(Act::OpenArchive).await.unwrap();
        m.dispatch(Act::Resume).await.unwrap();
        m.run_import().await.unwrap();

        let state = nest.read_state(folder);
        assert_eq!(state.phase, ImportPhase::Finished);
        assert_eq!(state.imported.len(), 2, "{:?}", state.skipped);
        assert!(state.has(&posts[0].external_id) && state.has(&posts[1].external_id));
        let unknown: Vec<_> = state
            .skipped
            .iter()
            .filter(|s| s.reason == super::UNKNOWN_ENTITY_REASON)
            .collect();
        assert_eq!(unknown.len(), 1, "{:?}", state.skipped);
        assert_eq!(unknown[0].category, Category::Posts);
        assert_eq!(unknown[0].external_id, None);
        assert_eq!(nest.posts().len(), 2);
    }
}
