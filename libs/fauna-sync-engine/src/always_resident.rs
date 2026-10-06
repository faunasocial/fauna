//! The **always-resident** sync loop: every file kept on disk, local edits watched
//! and uploaded. The download-only twin is the on-demand hydration host
//! (`docs/goal/behavior/file-sync.md` § On-Demand Files); this is the half that
//! *writes*.
//!
//! Lifted here from `apps/fauna-linux/src/sync.rs` (2026-07-13) so the Windows
//! sync host (now `fauna-sync-agent`) can drive the **same** loop rather than grow a second copy
//! (priority #2 — maximize shared Rust; priority #4 — resolve drift, don't match
//! it). Both apps now multiplex it over the shared
//! [`engine_host::EngineHost`](crate::engine_host::EngineHost), which is already
//! "generic over the per-engine loop" exactly so a platform can supply this one
//! (`file-sync.md` § On-Demand Files — *Hosting multiple on-demand folders*).
//!
//! Nothing here is platform-specific: [`FsWatcher`](crate::watcher::FsWatcher) is
//! `notify`-backed (inotify / FSEvents / `ReadDirectoryChangesW`) and
//! [`normalize_rel`](crate::watcher::normalize_rel) already folds Windows `\` to
//! `/` so `path_hash` matches across clients (`file-sync.md` § Content-Addressed
//! Storage).
//!
//! ## Why this is split into two calls, not one
//!
//! [`LocalWriteHost::converge`] then [`run_watch_loop`], with the caller's pre-bind
//! (M2) re-seal pass sandwiched between: the re-seal must run **after**
//! [`LocalWriteHost::converge`] — it re-seals the very rows that pass materializes
//! — and **before** the watch loop, and it belongs to the caller (the sync agent's
//! `run_always_resident_root`) rather than this module. The pass is
//! [`SyncEngine::reseal_pending_under_current`], called **ungated on every engine
//! start** by the bearer-only agent on every platform: it needs only the pushed
//! content keys + the local `SyncDb`, is idempotent (it skips rows already stamped
//! at the target generation, and a successful record stamps them) and returns `0`
//! for an unbound set. (Until 2026-09-25 an identity-holding in-process host gated
//! the same pass on a sealed sentinel row it could clear; that host and the
//! sentinel are retired, so the ungated shape is the only one.)
//! `docs/goal/architecture/mls-group-key-material.md` § M2.
//!
//! ## The write half is a trait, and that is load-bearing
//!
//! [`LocalWriteHost`] is the engine surface this module's uploader drives. The Windows
//! on-demand (cfapi) host's `HydrationHost` **requires it as a supertrait**, so an
//! on-demand root that cannot upload no longer compiles — the ratified rule that *a change
//! to a tracked file MUST be uploaded* is enforced by the type system rather than by a
//! comment. Both roots therefore share one watcher, one debouncer and one uploader
//! (priorities #2/#4). `file-sync.md` § On-Demand Files → *Sync direction*.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::engine::SyncEngine;
use crate::watcher::{FsEvent, FsWatcher, event_rel_path};

/// Coalesce a burst of writes to one file into a single upload. An editor's
/// save-temp-then-rename storm is one logical change, not five.
pub const DEBOUNCE_DELAY: Duration = Duration::from_millis(2000);

/// [`DEBOUNCE_DELAY`], overridable by `FAUNA_E2E_DEBOUNCE_MS` so a tier_3 test can hold a file
/// mid-debounce indefinitely (set huge) — the deterministic precondition a delete-vs-edit
/// conflict-arm test needs and no fixed constant can give it. Compile-gated outer, env inner
/// (`testing.md` § point 15 / convention 15): a production release build never reads the var.
/// An unset or unparseable value falls back to [`DEBOUNCE_DELAY`], so a missing override is
/// production-safe rather than a startup failure.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn debounce_delay() -> Duration {
    std::env::var("FAUNA_E2E_DEBOUNCE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEBOUNCE_DELAY)
}

#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
fn debounce_delay() -> Duration {
    DEBOUNCE_DELAY
}

/// The periodic full-reconcile cadence every resident host ticks at:
/// [`fauna_client_folders::DEFAULT_RESCAN_INTERVAL`] (300 s) — a hard-coded
/// constant since phase 5 of the folders re-model retired the per-folder
/// choice (`file-sync.md` § Config, the phase-5 block) — overridable by
/// `FAUNA_E2E_RESCAN_MS`, the [`debounce_delay`] twin. A tier_3 test needs it
/// in both directions: pushed PAST its budget so the tick's own
/// `converge → upload_pending` pass cannot resolve a mid-debounce edit the
/// test is holding (`test_filesync_delete_declined_debounce.py`), and pulled
/// SHORT so a scan-driven path (a mass delete inotify never itemizes, a
/// missed watcher event) lands inside a named poll budget instead of up to
/// five minutes later (`test_filesync_mass_delete_floor.py`; the harness
/// defaults every app launch to 30 s — all five of `drivers/{tui,linux,
/// windows,macos,ios}.py`). A test that spawns an agent ITSELF gets neither
/// default and must set the var, which `tests/platform/sync/helpers.py` now
/// does per spawn; the package that did not do so spent weeks running two
/// rescan regression pins against a 300 s tick they never waited long enough
/// to see.
/// Before phase 5 those tests chose the cadence through the wizard's
/// frequency picker; the knob retired, the need did not. Compile-gated outer,
/// env inner (`testing.md` § point 15 / convention 15): a production release
/// build never reads the var, and an unset or unparseable value falls back to
/// the constant, so a missing override is production-safe.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn rescan_interval() -> Duration {
    std::env::var("FAUNA_E2E_RESCAN_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(fauna_client_folders::DEFAULT_RESCAN_INTERVAL)
}

#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
pub fn rescan_interval() -> Duration {
    fauna_client_folders::DEFAULT_RESCAN_INTERVAL
}

/// Log the cadence a resident host's rescan tick was armed with — once per
/// folder, at arming, by every host that ticks (the always-resident
/// [`run_watch_loop`] and the agent's on-demand root alike).
///
/// The tick itself is silent, so this line is the only observable that says
/// which cadence a seat ADOPTED. The e2e seat harness reads its
/// `rescan_interval_ms=` field back (`tests/e2e-unified/helpers/sync_seats.py`,
/// `adopted_rescan_ms`) to pin that a seat launched with a
/// `FAUNA_E2E_RESCAN_MS` value really runs at it — the pin the removed headless
/// daemon's applied-row line used to carry, and without which the cadence went dead
/// config twice unnoticed. `folder_r` is the already-redacted folder name.
pub fn log_rescan_armed(folder_r: &str, interval: Duration) {
    tracing::info!(
        rescan_interval_ms = u64::try_from(interval.as_millis()).unwrap_or(u64::MAX),
        "{folder_r}: rescan tick armed"
    );
}

/// The resident root's pending-download pass
/// ([`SyncEngine::materialize_placeholder_rows`]), logged. `folder_r` is the
/// already-redacted folder name.
async fn materialize_placeholder_rows(engine: &SyncEngine, folder_r: &str) {
    match engine.materialize_placeholder_rows().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(fetched = n, "{folder_r}: pending placeholders downloaded"),
        Err(e) => tracing::error!("pending placeholders ({folder_r}): {e}"),
    }
}
/// How often the debouncer is swept for entries whose quiet period has elapsed.
pub const DEBOUNCE_TICK: Duration = Duration::from_millis(500);
/// Files uploaded concurrently by a catch-up pass. Deliberately small: each file
/// is itself chunked + sealed + uploaded in parallel by the transfer pool, so a
/// large fan-out here mostly starves that.
pub const MAX_CONCURRENT_FILES: usize = 2;

/// The **audience corpus convergence** (folders re-model phase 4) — make an
/// audience flip real for bytes recorded before it: a `public`-armed engine
/// declassifies its sealed back-catalogue to the world-readable plaintext
/// shape; a sealed-posture engine re-seals a corpus left plaintext by a public
/// window. One `meta`-row read in steady state; the projected audience is the
/// cross-device *signal*, so there is no sentinel to stage or lose — but it is
/// not the *authority*: the owner's `→public` flip carries a signature, and
/// the posture this pass reads may arm public only on that attestation
/// verified (`encryption-at-rest.md` § Readable classes → *The
/// declassification is owner-ATTESTED*; its § Implementation status today
/// says which readers verify yet).
/// Runs directly after [`crate::engine::SyncEngine::refresh_sync_mode`] — the
/// same ordering, and for the same reason, as its sibling
/// [`converge_corpus_to_website`]: that refresh is what installs the audience
/// this pass reads, so pairing them is what carries a flip onto a RUNNING seat
/// within one tick. Until 2026-08-21 this pass ran *only* in the callers'
/// once-per-start catch-up, so a flip on a live seat moved the write path (which
/// does ride the refresh) while the back-catalogue waited for a process restart
/// — the flip-back's re-seal was reachable only by restarting the
/// agent. Convergent and idempotent, so running it on every tick costs one
/// `meta`-row read once converged.
pub async fn converge_corpus_to_audience(engine: &SyncEngine, folder: &str) {
    let folder_r = fauna_core::log_redact::log_folder_name(folder);
    match engine.converge_corpus_to_audience().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            "folder {folder_r}: converged {n} file(s) to the folder's current audience shape"
        ),
        Err(e) => tracing::error!(
            "folder {folder_r}: audience corpus convergence failed (retries next tick): {e}"
        ),
    }
}

/// The **website corpus convergence** (`web-content-hosting.md` § Content
/// model) — make a website toggle-ON real for a SEALED folder's back-catalogue,
/// which the nest's own enable-time backfill structurally cannot reach (a sealed
/// head rests no plaintext name, S9). Re-records every live head as a
/// no-novel-content reissue so sync-time routing lands it in `web_files` with
/// the sealed `content_key_version` intact. One `meta`-row read in steady state;
/// the projected toggle itself is the cross-device signal.
///
/// Runs directly after [`crate::engine::SyncEngine::refresh_sync_mode`], which
/// is what installs the toggle it reads — that ordering is what carries an
/// owner's flip onto a RUNNING seat within one tick instead of waiting for a
/// process restart.
pub async fn converge_corpus_to_website(engine: &SyncEngine, folder: &str) {
    let folder_r = fauna_core::log_redact::log_folder_name(folder);
    match engine.converge_corpus_to_website().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            "folder {folder_r}: re-recorded {n} file(s) so the website's back-catalogue reaches \
             the nest's web_files projection"
        ),
        Err(e) => tracing::error!(
            "folder {folder_r}: website corpus convergence failed (retries next tick): {e}"
        ),
    }
}

/// The **post-succession corpus re-seal** — move this set's bytes off the
/// retired owner root a succession left them under (`succession-aftermath.md`
/// § Re-key scope: "client-driven and urgent"). Needs only the `BackupKey` plus
/// the retired ones the capability carries, so it is bearer-only safe and runs
/// identically on every drive shape. A no-op for any engine holding no
/// predecessor keys — i.e. every identity that never succeeded.
pub async fn reseal_predecessor_sealed(engine: &SyncEngine, folder: &str) {
    let folder_r = fauna_core::log_redact::log_folder_name(folder);
    match engine.reseal_predecessor_sealed().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            "folder {folder_r}: re-sealed {n} file(s) off a predecessor's BackupKey after an \
             identity succession"
        ),
        Err(e) => tracing::error!(
            "folder {folder_r}: post-succession re-seal pass failed (retries next start): {e}"
        ),
    }
}

/// The **re-record leg** of ruling (g), logged
/// ([`SyncEngine::rerecord_under_live_nonce`]): this device's heads signed
/// under a retired set nonce re-signed onto the live one. First of the start
/// passes, so the re-seal pass behind it reads heads under the live nonce.
pub async fn rerecord_under_live_nonce(engine: &SyncEngine, folder: &str) {
    match engine.rerecord_under_live_nonce().await {
        Ok(0) => {}
        Ok(n) => {
            tracing::info!("folder {folder}: re-recorded {n} head(s) under the set's live nonce")
        }
        Err(e) => tracing::error!(
            "folder {folder}: re-record under the live nonce failed (retries next engine \
             start): {e}"
        ),
    }
}

/// The **pre-bind (M2) re-seal** pass, logged
/// ([`SyncEngine::reseal_pending_under_current`]; `mls-group-key-material.md`
/// § M2 *Pre-bind re-seal migration*). Ungated and run on every engine start:
/// it needs only the pushed content keys + the local `SyncDb`, is idempotent
/// (a recorded row is stamped and skipped next time) and returns `0` for an
/// unbound set, so re-driving it is safe — skipping it would be the unsafe
/// option, since an owner who shares a set they already populated would leave
/// the pre-bind back-catalogue sealed under their `BackupKey`, undecryptable
/// for every joiner.
pub async fn reseal_pending_under_current(engine: &SyncEngine, folder: &str) {
    match engine.reseal_pending_under_current().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            "folder {folder}: re-sealed {n} pre-bind file(s) under the current content key"
        ),
        Err(e) => tracing::error!(
            "folder {folder}: pre-bind re-seal pass failed (retries next engine start): {e}"
        ),
    }
}

/// The **set-name stamp**, logged ([`SyncEngine::stamp_sealed_set_name`];
/// `path-sealing.md` § the set-name plane): the set's name sealed under the
/// root this engine started on. A share binds a set to its M2 audience and
/// rebuilds the engine, so this start is where the name moves to the key the
/// members hold — without it a member cannot open the name of a set shared
/// with them, and the set never reaches their Folders page. Convergent and
/// best-effort inside, so repeating it on every start is free.
pub async fn stamp_sealed_set_name(engine: &SyncEngine, folder: &str) {
    let folder_r = fauna_core::log_redact::log_folder_name(folder);
    match engine.stamp_sealed_set_name().await {
        Ok(true) => tracing::debug!("folder {folder_r}: stamped sealed set name"),
        Ok(false) => {}
        Err(e) => tracing::warn!(
            "folder {folder_r}: sealed set-name stamp failed (retries next start): {e}"
        ),
    }
}

/// **The once-per-start corpus passes — every root, whichever mode.** Run once
/// the control plane is open and the local-write catch-up (`converge`) has
/// uploaded what the folder holds: the pre-bind re-seal, the audience
/// convergence (an audience flip made while this seat was down reaches the
/// back-catalogue here), the post-succession re-seal and the set-name stamp. One home for the
/// sequence, so the always-resident root and the on-demand hydration root
/// cannot drift apart on what a start owes the corpus: until 2026-09-27 only
/// the always-resident root ran these, so an on-demand root — the windows
/// default since that day — never declassified or re-sealed a byte on an
/// audience flip, and a public window's back-catalogue stayed sealed at rest
/// (found by `test_folder_bound_flip_back.py` on tui-on-windows). Every pass
/// is placeholder-safe: a cloud-only entry's bytes are fetched from the nest
/// rather than read off this disk (`ResealSource::LocalFile`'s fall-through).
pub async fn converge_corpus_at_start(engine: &SyncEngine, folder: &str) {
    rerecord_under_live_nonce(engine, folder).await;
    reseal_pending_under_current(engine, folder).await;
    converge_corpus_to_audience(engine, folder).await;
    reseal_predecessor_sealed(engine, folder).await;
    // Last, after the content re-seals: a set that just bound stamps its name
    // under the generation it landed on, not the one it was leaving.
    stamp_sealed_set_name(engine, folder).await;
}

/// **The per-tick posture refresh + corpus pair — every root, whichever mode.**
/// [`SyncEngine::refresh_sync_mode`] first — the ONE read that installs this
/// seat's mode, its folder's audience and its website toggle on the running
/// engine — then the pair that reads them: audience convergence and website
/// convergence. The refresh is inseparable from the pair, which is why they
/// share one fn: the passes read `is_public_audience()` / `is_website_enabled()`
/// off the engine, and without the refresh a running root keeps its build-time
/// posture for ever, so a flip made on a live seat never reaches its
/// back-catalogue (the hydration root ran the pair without the refresh for one
/// e2e cycle on 2026-09-27 and converged nothing). One `meta`-row read each per
/// tick once converged; a re-seal / declassify / reissue on the tick that first
/// sees the flip. The always-resident loop runs it at entry and on every rescan
/// tick — before the pull, so a tick that re-delivers a held tombstone resolves
/// the seat first (`file-sync.md` § 4); the on-demand hydration loop runs it at
/// startup and on its rescan tick (see [`converge_corpus_at_start`]).
pub async fn refresh_and_converge_corpus(engine: &SyncEngine, folder: &str) {
    engine.refresh_sync_mode().await;
    converge_corpus_to_audience(engine, folder).await;
    converge_corpus_to_website(engine, folder).await;
}

/// What the folder's local writes are asking the engine to do.
#[derive(Debug)]
pub enum LocalWrite {
    /// These paths have gone quiet for [`DEBOUNCE_DELAY`] and are ready to upload.
    Upload(Vec<String>),
    /// This path was removed locally.
    Delete(String),
    /// The watcher channel closed — the OS watch is gone, so the loop must end.
    WatcherClosed,
}

/// **The engine surface a two-way root's write half drives** — and the reason there is
/// exactly one uploader in the tree.
///
/// On-demand and always-resident are the *same* contract on this side: a change to a
/// tracked file **must** be uploaded, and no mode, folder setting or platform host may
/// suppress it (`file-sync.md` § On-Demand Files → *Sync direction*, USER-ratified
/// 2026-07-14 — on-demand is a **storage** choice, never a **direction** one). The two
/// roots differ only in *how bytes reach the disk*.
///
/// So both loops take this trait rather than a concrete engine, and the Windows on-demand
/// host's `HydrationHost` (`fauna-sync-agent`'s `bridge.rs`) **requires it as a
/// supertrait**. That is deliberate: it makes a download-only on-demand host *fail to
/// typecheck*, which is the shape this codebase reaches for whenever a rule is iron-clad
/// (cf. the placeholder upload choke point in [`SyncEngine::upload_file`], and cfapi's
/// unrepresentable-if-empty `FileIdentity`). The old download-only root was not a policy —
/// it was a missing capability that a comment had to police, and comments do not fail builds.
///
/// Implemented by [`SyncEngine`]; faked in the loop-level unit tests, and in the live
/// cfapi harness **delegated to a real engine** so the shipped `upload_file` is what runs.
#[async_trait::async_trait(?Send)]
pub trait LocalWriteHost {
    /// `.faunaignore` / dotfiles — never ours to upload.
    fn is_ignored(&self, rel: &str) -> bool;
    /// Did *we* just write this file (the classic download echo)? Consume-on-check.
    fn was_recent_download(&self, rel: &str) -> bool;
    /// Did *we* just remove this file, applying a delete that came FROM the nest?
    /// Consume-on-check, and deliberately a **separate** channel from
    /// [`Self::was_recent_download`] — see [`crate::engine::SyncEngine`]'s
    /// `recent_removals` field for why sharing one token re-recorded applied
    /// deletes back to the nest.
    fn was_recent_removal(&self, rel: &str) -> bool;
    /// Upload a locally-modified file. Refuses a cloud-only placeholder at its own choke
    /// point, whichever caller reaches it. The outcome says whether the change RECORD
    /// reached the nest — the signal a platform sync-state flip must key on
    /// ([`crate::engine::UploadOutcome`]).
    async fn upload_file(&self, rel: &str) -> anyhow::Result<crate::engine::UploadOutcome>;
    /// Record a local deletion on the nest.
    async fn handle_delete(&self, rel: &str) -> anyhow::Result<()>;
    /// **The catch-up backstop**: resume interrupted uploads, reconcile the folder against
    /// the `SyncDb`, then upload whatever that leaves pending.
    ///
    /// This is what catches the edits the watcher could not see — writes made while the
    /// service was down, and watcher misses. Both roots run it at startup and on their
    /// rescan tick. On an on-demand root it is safe precisely because the placeholder guard
    /// landed first: `reconcile` skips cloud-only placeholders on their OS attributes
    /// (never opening one, which would stall 60 s) while still *counting* them, so
    /// delete-detection cannot mistake a dehydrated file for a deleted one.
    ///
    /// Call it **before** any re-seal pass: the re-seal walks `Synced` rows, and this is
    /// what materializes them.
    ///
    /// Returns the rels whose upload's change record reached the nest in this pass —
    /// the provably-synced set a platform sync-state flip keys on, exactly as
    /// [`apply_local_write`] surfaces `recorded` for the watcher path. The windows
    /// on-demand host flips these ✅ (the startup / offline / rescan-catch-up edits the
    /// watcher never saw); an always-resident root has no placeholder surface to flip
    /// and ignores the return.
    async fn converge(&self, folder: &str) -> Vec<String>;

    /// Open this flush's **exclusive-edit write window** (`file-sync.md`
    /// § Exclusive editing) — acquire the folder's lease if its owner turned
    /// exclusive editing on and this device does not already hold it.
    ///
    /// Called once per [`LocalWrite`] the loop applies, never per file: a
    /// debounced flush is a batch of rels and one window covers all of them,
    /// which is the rule the whole lease design turns on. An un-governed folder
    /// — every folder until an owner opts in — answers
    /// [`crate::folder_lease::LeaseWindow::NotGoverned`] without touching the
    /// nest.
    ///
    /// **Defaults to un-governed.** A host whose writes do not go through a
    /// real engine (every implementor here but [`SyncEngine`] is a test double)
    /// governs nothing, and the default says so without each one restating it.
    /// The default is not a hole: the engine's own upload choke point refuses a
    /// write on a folder another device holds whichever caller reaches it, so
    /// the worst a host that never opens a window can do is fail to *reserve*
    /// the folder — never write through somebody else's lease.
    async fn open_lease_window(&self) -> crate::folder_lease::LeaseWindow {
        crate::folder_lease::LeaseWindow::NotGoverned
    }

    /// Close this flush's write window — release the lease this device holds,
    /// if it holds one. Idempotent, never fatal (the nest expires the row on
    /// its own TTL), and a no-op for the default un-governed implementation.
    async fn close_lease_window(&self) {}

    /// Re-read the set's content-key floor before this batch seals anything
    /// (decision 2's pre-seal edge, `on-demand-files.md` § Shared sets on a
    /// capability host). **Defaults to a no-op**, like the lease window: every
    /// implementor but [`SyncEngine`] is a test double, and the engine's seal-root
    /// resolver refuses a seal behind the floor whichever caller reaches it.
    async fn refresh_seal_floor(&self) {}
}

#[async_trait::async_trait(?Send)]
impl LocalWriteHost for SyncEngine {
    fn is_ignored(&self, rel: &str) -> bool {
        SyncEngine::is_ignored(self, rel)
    }

    fn was_recent_download(&self, rel: &str) -> bool {
        SyncEngine::was_recent_download(self, rel)
    }

    fn was_recent_removal(&self, rel: &str) -> bool {
        SyncEngine::was_recent_removal(self, rel)
    }

    async fn upload_file(&self, rel: &str) -> anyhow::Result<crate::engine::UploadOutcome> {
        SyncEngine::upload_file(self, rel).await
    }

    async fn handle_delete(&self, rel: &str) -> anyhow::Result<()> {
        // Under the off-disk placeholder posture a `Remove` for a `Placeholder`
        // row is the provider's own unlink — a dehydrate whose suppression token
        // was already spent — never a user's delete (`on-demand-files.md` § Linux
        // FUSE binding, the dehydrate rule, obligation 4). The user's delete of a
        // placeholder on that root reaches the inherent `SyncEngine::handle_delete`
        // through the mount's `unlink`, not through this rail.
        if self.placeholders_off_disk()
            && self
                .db()
                .get_entry(rel)?
                .is_some_and(|e| e.state == crate::db::SyncState::Placeholder)
        {
            tracing::debug!(
                path = %fauna_core::log_redact::log_path(rel),
                "dropping the watcher's remove of an off-disk placeholder (the provider's own unlink)"
            );
            return Ok(());
        }
        // The trait keeps `Result<()>`: a not-recorded delete leaves the row in
        // place (record-first, `SyncEngine::handle_delete`), and the reconcile
        // pass on the next converge/rescan tick is this loop's retry path — no
        // per-call ack surface to thread it to.
        SyncEngine::handle_delete(self, rel).await.map(|_| ())
    }

    async fn open_lease_window(&self) -> crate::folder_lease::LeaseWindow {
        SyncEngine::open_lease_window(self).await
    }

    async fn close_lease_window(&self) {
        SyncEngine::close_lease_window(self).await
    }

    async fn refresh_seal_floor(&self) {
        SyncEngine::refresh_seal_floor(self).await
    }

    async fn converge(&self, folder: &str) -> Vec<String> {
        let drained = self.drain_pending_uploads().await.is_ok();
        let reconciled = match self.reconcile().await {
            Ok(_stats) => true,
            Err(e) => {
                tracing::error!(
                    "reconcile ({}): {e}",
                    fauna_core::log_redact::log_folder_name(folder)
                );
                false
            }
        };
        // Surface the rels whose record landed so an on-demand host can flip them ✅.
        // `unwrap_or_default` preserves the old `let _ = upload_pending(..)` behavior on
        // error (empty flip set, retried on the next converge).
        match self.upload_pending(MAX_CONCURRENT_FILES).await {
            Ok((recorded, _bytes)) => {
                // Every step of this pass ran clean and reconcile scanned the
                // local disk — if nothing is left pending, this device just
                // verified itself consistent: stamp `last_clean_pass_at` (the
                // idle half of the wire's `last_sync`). A pass with a failed
                // step proves nothing and stamps nothing.
                if drained
                    && reconciled
                    && let Err(e) = self.db().mark_clean_pass_if_drained()
                {
                    tracing::warn!(error = ?e, "failed to stamp last_clean_pass_at");
                }
                recorded
            }
            Err(_) => Vec::new(),
        }
    }
}

/// **The local-write half of a two-way root**, reduced to a single `select!` arm.
///
/// Shared, and deliberately so: on-demand and always-resident differ in *how bytes get onto the
/// disk* and in **nothing else** — a local edit uploads either way (`file-sync.md` § On-Demand
/// Files → *Sync direction*: on-demand is a **storage** choice, never a **direction** one). So
/// there is one watcher, one debouncer and one uploader for both roots (priorities #2/#4), not
/// a second copy grown inside the hydration host.
///
/// [`Self::next`] is **cancel-safe**: it only ever awaits an mpsc `recv` and an interval `tick`,
/// both cancel-safe, so racing it against a cancellation token (or a hydration command channel)
/// in a `select!` can never drop an event it had already taken.
pub struct LocalWrites {
    watcher: FsWatcher,
    watch_dir: PathBuf,
    debouncer: crate::debouncer::EventDebouncer,
    debounce_tick: tokio::time::Interval,
}

impl LocalWrites {
    /// Start watching `watch_dir`. Dropping the returned value releases the OS watch handle.
    pub fn start(watch_dir: &std::path::Path) -> anyhow::Result<Self> {
        // FSEvents (macOS) delivers fully-resolved event paths
        // (`/private/var/...`), so a symlinked watch_dir (`/var/...`,
        // `/tmp/...`) would fail every `strip_prefix` below and silently drop
        // every event. Canonicalize once so the event paths and the prefix
        // agree on every platform; fall back to the given path if the dir
        // cannot be resolved (it is about to error in `FsWatcher::start`
        // anyway).
        //
        // A descriptor reach is the exception, and stays exactly as given: it
        // names the directory UNDER a mount the host has placed over it (the
        // linux on-demand root), and its canonical form is the mount point's
        // own text — which, once the mount is up, names the mounted view. The
        // watch must stay on the directory beneath.
        let watch_dir = if is_descriptor_reach(watch_dir) {
            watch_dir.to_path_buf()
        } else {
            watch_dir
                .canonicalize()
                .unwrap_or_else(|_| watch_dir.to_path_buf())
        };
        let watcher = FsWatcher::start(&watch_dir)?;
        // `interval` fires its first tick immediately. Nothing consumes it here (this is not
        // async), and nothing needs to: an unsolicited tick just drains an empty debouncer and
        // `next` loops back into the select.
        let debounce_tick = tokio::time::interval(DEBOUNCE_TICK);
        Ok(Self {
            watcher,
            watch_dir,
            debouncer: crate::debouncer::EventDebouncer::new(debounce_delay()),
            debounce_tick,
        })
    }

    /// Should a filesystem event for `rel` become an upload at all?
    ///
    /// Four ways a write into the sync root is **not** the user editing their file:
    ///
    /// - it is a **directory** — a folder carries directories implicitly via child
    ///   paths, so a directory is never uploadable content; and since every write
    ///   *inside* a directory also raises a Modified event on the directory itself,
    ///   letting one through re-queued the parent and logged a fresh "reading <dir>"
    ///   ERROR on every child edit (live 2026-07-17);
    /// - it is ignored (`.faunaignore`, dotfiles) — never ours to upload;
    /// - we *just downloaded* it (`was_recent_download`) — the classic echo;
    /// - it is a **cloud-only placeholder**, which on an on-demand root is the common case: our
    ///   own `CfExecute(TRANSFER_PLACEHOLDERS)` materializes files in the root and the OS
    ///   reports each one as an ordinary `Created` event. `was_recent_download` does not cover
    ///   placeholder *creation*, so without this the host would try to upload every file it had
    ///   just advertised — each attempt stalling 60 s on a fetch that never arrives
    ///   ([`crate::placeholder`]). `upload_file` refuses these anyway; catching them here keeps
    ///   the debouncer and the log clean.
    fn is_user_write<H: LocalWriteHost>(&self, host: &H, rel: &str) -> bool {
        !crate::ignore::has_hidden_component(rel)
            && !host.is_ignored(rel)
            && !host.was_recent_download(rel)
            && !self.watch_dir.join(rel).is_dir()
            && !crate::placeholder::path_is_cloud_placeholder(&self.watch_dir.join(rel))
    }

    /// Queue every file already inside `dir`, a directory that just appeared.
    ///
    /// A recursive OS watch learns about a new directory only when it appears and
    /// adds its own watch after that, so a file written into a fresh tree before the
    /// watch lands — `mkdir -p` then a write, a copied-in folder, an unzip — raises no
    /// event of its own and would otherwise wait for the reconcile backstop. So a
    /// `Created` directory is walked once. Hidden and ignored subtrees are skipped, as
    /// their own events would be. The download-echo token is deliberately NOT consulted
    /// here: it is consume-on-check, and spending it on the walk would leave the
    /// file's own `Created` event unsuppressed. A just-downloaded file this walk
    /// queues costs one hash: `upload_file`'s already-synced short-circuit skips it.
    fn touch_new_subtree<H: LocalWriteHost>(&mut self, host: &H, dir: &std::path::Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(rel) = event_rel_path(&path, &self.watch_dir) else {
                continue;
            };
            if crate::ignore::has_hidden_component(&rel) || host.is_ignored(&rel) {
                continue;
            }
            // `file_type` does not follow symlinks, so a link to a directory is
            // never walked into.
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                self.touch_new_subtree(host, &path);
            } else if ft.is_file() && !crate::placeholder::path_is_cloud_placeholder(&path) {
                self.debouncer.touch(&rel);
            }
        }
    }

    /// Await the next thing the folder's local writes are asking for. See the struct doc for
    /// the cancel-safety contract.
    pub async fn next<H: LocalWriteHost>(&mut self, host: &H) -> LocalWrite {
        loop {
            tokio::select! {
                event = self.watcher.events.recv() => {
                    match event {
                        // A directory that APPEARED may already hold files its
                        // own watch was too late to see. Only `Created`: every
                        // write inside a directory also raises `Modified` on it,
                        // and walking on that would re-queue the whole tree.
                        Some(FsEvent::Created(path)) if path.is_dir() => {
                            if let Some(rel) = event_rel_path(&path, &self.watch_dir)
                                && !crate::ignore::has_hidden_component(&rel)
                                && !host.is_ignored(&rel)
                            {
                                self.touch_new_subtree(host, &path);
                            }
                        }
                        Some(FsEvent::Created(path) | FsEvent::Modified(path)) => {
                            if let Some(rel) = event_rel_path(&path, &self.watch_dir)
                                && self.is_user_write(host, &rel)
                            {
                                self.debouncer.touch(&rel);
                            }
                        }
                        Some(FsEvent::Removed(path)) => {
                            if let Some(rel) = event_rel_path(&path, &self.watch_dir) {
                                // A removal cannot be a placeholder (the file is gone), so it
                                // takes the narrower filter, not `is_user_write`.
                                //
                                // It consults the REMOVAL channel, never the download one:
                                // the two are independent one-shot tokens precisely because
                                // a download's `Created` event would otherwise consume the
                                // token this arm needs (see `was_recent_removal`).
                                if !host.is_ignored(&rel) && !host.was_recent_removal(&rel) {
                                    self.debouncer.cancel(&rel);
                                    return LocalWrite::Delete(rel);
                                }
                            }
                        }
                        None => return LocalWrite::WatcherClosed,
                    }
                }

                _ = self.debounce_tick.tick() => {
                    let ready = self.debouncer.drain_ready();
                    if !ready.is_empty() {
                        return LocalWrite::Upload(ready);
                    }
                }
            }
        }
    }
}

/// Is `path` a **descriptor reach** — `/proc/self/fd/<n>`, a directory named
/// through a descriptor this process holds open on it? A host that mounts a view
/// over a directory (the linux on-demand root, `on-demand-files.md` § Linux FUSE
/// binding) reaches the directory beneath through one; resolving it by text
/// lands on the view instead, so it must never be canonicalized.
fn is_descriptor_reach(path: &std::path::Path) -> bool {
    path.starts_with("/proc/self/fd")
}

/// What [`apply_local_write`] did with one [`LocalWrite`].
#[derive(Debug, Default)]
pub struct LocalWriteApplied {
    /// False when the watcher channel closed and the caller's loop should end.
    pub continue_watching: bool,
    /// Rels whose upload's change record reached the nest in this batch —
    /// provably synced end-to-end, safe for a platform in-sync flip
    /// (the windows on-demand root marks these ✅). Empty for deletes and for
    /// uploads whose record failed or was skipped.
    pub recorded: Vec<String>,
}

/// Apply one [`LocalWrite`] through the **shared** uploader.
///
/// # The exclusive-edit window
///
/// A debounced flush is one write window (`file-sync.md` § Exclusive editing):
/// the whole batch of rels takes **one** lease, not one per file, and gives it
/// back when the batch drains. A local **delete** is a write to the folder too,
/// so it takes the same window — a device that may not upload into a folder
/// somebody else is editing may not record deletions out of it either, and
/// giving the two rails one rule is what keeps that from becoming two rules
/// that disagree.
///
/// A window this flush could not get defers the whole batch: nothing is
/// uploaded, nothing is recorded, nothing on disk is touched, and the next
/// converge tick's `reconcile` re-detects every one of these paths and re-drives
/// it. That is the same path an offline flush takes, which is the point — a
/// lease may make a write *wait*, never make it *vanish*.
pub async fn apply_local_write<H: LocalWriteHost>(
    host: &H,
    folder: &str,
    write: LocalWrite,
) -> LocalWriteApplied {
    // Nothing to reserve the folder for — and `WatcherClosed` must stay a pure
    // signal, never a nest round-trip.
    if matches!(write, LocalWrite::WatcherClosed) {
        return LocalWriteApplied::default();
    }

    // Redacted once and reused by every log line below (`path-sealing.md` §
    // Sealed names & paths, S7).
    let folder_r = fauna_core::log_redact::log_folder_name(folder);

    // The pre-seal edge: a rotation since the last tick must hold these seals,
    // not wait for the tick to notice it (decision 2 — *before a seal*).
    host.refresh_seal_floor().await;
    let window = host.open_lease_window().await;
    if !window.may_write() {
        tracing::info!(
            folder = %folder_r,
            "exclusive editing: deferring this flush — the local edits stay on disk and the \
             next converge pass re-drives them"
        );
        // `continue_watching: true` — a deferral is not a watcher fault. The
        // loop keeps watching; only this batch waits.
        return LocalWriteApplied {
            continue_watching: true,
            recorded: Vec::new(),
        };
    }

    let applied = match write {
        LocalWrite::Upload(rels) => {
            let mut recorded = Vec::new();
            for rel in rels {
                match host.upload_file(&rel).await {
                    Ok(outcome) if outcome.recorded => recorded.push(rel),
                    Ok(_) => {}
                    Err(e) => {
                        tracing::error!(
                            "upload ({folder_r}) {}: {e}",
                            fauna_core::log_redact::log_path(&rel)
                        );
                    }
                }
            }
            LocalWriteApplied {
                continue_watching: true,
                recorded,
            }
        }
        LocalWrite::Delete(rel) => {
            if let Err(e) = host.handle_delete(&rel).await {
                tracing::error!(
                    "delete ({folder_r}) {}: {e}",
                    fauna_core::log_redact::log_path(&rel)
                );
            }
            LocalWriteApplied {
                continue_watching: true,
                recorded: Vec::new(),
            }
        }
        LocalWrite::WatcherClosed => unreachable!("handled above, before the window"),
    };

    if window.holds_lease() {
        host.close_lease_window().await;
    }
    applied
}

/// Await the control plane's next transition INTO
/// [`fauna_client::ConnectionState::Connected`].
///
/// # Why this arm exists
///
/// [`SyncEngine::refresh_share_writer_roster`] silently no-ops unless the
/// control plane is `Connected`, and until this arm landed its only two
/// triggers were engine build and the rescan tick. Build typically runs
/// *before* the control plane is up, so between it and the first tick that
/// happens to catch `Connected` there is a hole in which the member's cached
/// writer roster stays empty — and an empty roster refuses **every** row
/// **every** peer serves, before weighing any of them (`judge_peer_row`'s
/// fail-closed arm).
///
/// **If the nest goes down inside that hole, the hole is permanent**: offline,
/// the refresh short-circuits on the very same check, so the member can never
/// receive anything peer-to-peer for that set — exactly the situation the share
/// plane exists for — with no heal path and nothing on screen saying so. Under
/// load the tick body can outrun its own interval, which widens the hole well
/// past what the cadence suggests. Measured 2026-08-22 on
/// `test_share_pump_two_actor.py`: seven `control plane not connected; cache
/// untouched` lines and **zero** cached ones across a failing run.
///
/// Refreshing on the transition closes it in milliseconds instead of a tick.
///
/// # Cancel-safety and the dropped-sender trap
///
/// `watch::Receiver::changed()` is cancel-safe, so `select!` may drop this
/// future freely. But once the sender is dropped it returns `Err`
/// **immediately and forever** — an arm that merely returned on that would be
/// re-polled on every loop iteration and spin a core. So the caller parks the
/// arm by taking the receiver out of the `Option`, after which this parks on
/// `pending()` and never fires again. `None` therefore means *"stop polling
/// me"*, not *"nothing happened"*.
async fn next_connected(
    conn_rx: &mut Option<tokio::sync::watch::Receiver<fauna_client::ConnectionState>>,
) -> Option<()> {
    let Some(rx) = conn_rx.as_mut() else {
        return std::future::pending().await;
    };
    loop {
        if rx.changed().await.is_err() {
            return None; // sender gone — the caller parks this arm for good
        }
        if matches!(
            *rx.borrow_and_update(),
            fauna_client::ConnectionState::Connected
        ) {
            return Some(());
        }
    }
}

/// A command a host routes INTO a running resident engine over its per-folder
/// channel — the invoke-and-reply sibling of the wake nudge (fire-and-forget).
/// Defined in the shared crate so every resident host (the per-user agent
/// today, any in-process host later) shares one vocabulary rather than each
/// growing its own side channel.
pub enum EngineCommand {
    /// The user confirmed the *"your folder emptied — apply N deletions"*
    /// affordance (`delete-propagation.md` § the mass-delete floor:
    /// propagation of a held set is an explicit user action). Runs
    /// [`SyncEngine::apply_held_deletes`] on the loop's engine and replies with
    /// its outcome. A dropped receiver (the caller went away) is fine — the
    /// apply still ran; only the report is lost.
    ApplyHeldDeletes {
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::engine::AppliedHeldDeletes>>,
    },
    /// One page of ACCEPTED peer-served share rows, handed over for the
    /// engine's provisional ingest (slice E — the app owns the peer channel,
    /// the engine owns the state writes, and on an out-of-process-agent app
    /// this channel is the boundary between them). `rows` are canonical
    /// dag-cbor `fauna_protocol::peer_share::PeerShareChange` encodings (an
    /// undecodable row is refused, counted); bodies come from the
    /// caller-populated spool ([`crate::peer_share_store::SpoolFetcher`]).
    /// Advances the per-peer pull cursor to the page's highest sequenced seq
    /// after the ingest, and replies with the report + the new cursor.
    #[cfg(feature = "p2p-share")]
    ShareIngest {
        proven_actor_hex: String,
        rows: Vec<Vec<u8>>,
        spool_dir: PathBuf,
        reply: tokio::sync::oneshot::Sender<
            anyhow::Result<(crate::peer_share_store::PeerIngestReport, i64)>,
        >,
    },
    /// *Free up space* for the file at `rel` — the user's verb on an on-demand
    /// root whose platform keeps no pin or dehydrate of its own (the linux FUSE
    /// root; `on-demand-files.md` § Linux FUSE binding, the dehydrate rule). The
    /// bytes are freed only when that is provably lossless, a pinned file
    /// refuses, and a file already cloud-only answers `Ok` (nothing to lose).
    /// The on-demand loop answers it; a resident root holds every file locally
    /// by definition and refuses.
    FreeSpace {
        rel: String,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
    },
    /// Pin (`true` — *always keep on this device*: a cloud-only file hydrates
    /// now) or unpin (`false` — the bytes are freed when that is lossless) the
    /// file at `rel`, on a root whose pins live in the row (the linux FUSE root;
    /// same owner as [`Self::FreeSpace`]). The on-demand loop answers it; a
    /// resident root refuses.
    SetPinned {
        rel: String,
        pinned: bool,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
    },
    /// The byte half of restoring a version a retired identity of this account
    /// signed (`writer-signed-change-records.md` ruling (8)(d)): open the bytes
    /// `manifest_hash` names under the roots `signed_as` may reach and re-seal
    /// them under the current owner root
    /// ([`SyncEngine::reseal_inherited_version`]). Replies with the manifest
    /// the caller records in the historical one's place, or the refusal;
    /// records nothing itself. Routed here because the running engine owns the
    /// set's byte plane and keys — the host builds no second one.
    ResealInheritedVersion {
        rel: String,
        manifest_hash: [u8; 32],
        signed_as: Option<[u8; 32]>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::engine::ResealedVersion>>,
    },
}

/// Answer one [`EngineCommand`] on `engine` and send the reply — the command
/// arm of EVERY engine loop, in one place: the resident [`run_watch_loop`] and
/// the agent's on-demand hydration loop both call it, so a folder's mode never
/// decides which verbs its engine answers (`delete-propagation.md` § *The floor
/// on an on-demand root*, point 4). Runs on the calling loop, so the engine is
/// never driven from two tasks at once; a caller that went away just drops the
/// reply receiver (the command still ran — only the report is lost).
pub async fn answer_engine_command(engine: &SyncEngine, cmd: EngineCommand) {
    match cmd {
        EngineCommand::ApplyHeldDeletes { reply } => {
            let _ = reply.send(engine.apply_held_deletes().await);
        }
        #[cfg(feature = "p2p-share")]
        EngineCommand::ShareIngest {
            proven_actor_hex,
            rows,
            spool_dir,
            reply,
        } => {
            let _ =
                reply.send(ingest_share_page(engine, &proven_actor_hex, &rows, spool_dir).await);
        }
        // The on-demand loop intercepts both before they reach this answer; a
        // resident root has every file on its disk and no pin to keep.
        EngineCommand::FreeSpace { reply, .. } | EngineCommand::SetPinned { reply, .. } => {
            let _ = reply.send(Err(anyhow::anyhow!(
                "this folder keeps every file on this device (always-resident): there is \
                 nothing to free and nothing to pin"
            )));
        }
        EngineCommand::ResealInheritedVersion {
            rel,
            manifest_hash,
            signed_as,
            reply,
        } => {
            let _ = reply.send(
                engine
                    .reseal_inherited_version(
                        &rel,
                        fauna_core::data::ContentHash::from_digest_raw(manifest_hash),
                        signed_as,
                    )
                    .await,
            );
        }
    }
}

/// [`EngineCommand::ShareIngest`]'s body — public because it IS the ingest
/// door: the agent's command arm calls it here, and an in-process deployment
/// (linux's driver, tests) calls it directly as its
/// `share_pump::ShareIngestDoor` impl. Decode the page (an undecodable row
/// counts as refused — a cross-version encoding difference must surface in
/// the report, never vanish), ingest through the engine's provisional door
/// with the spool-backed fetcher, then advance the per-peer pull cursor to
/// the page's highest sequenced seq (pending rows carry no coordinate and
/// never advance it).
#[cfg(feature = "p2p-share")]
pub async fn ingest_share_page(
    engine: &SyncEngine,
    proven_actor_hex: &str,
    rows: &[Vec<u8>],
    spool_dir: PathBuf,
) -> anyhow::Result<(crate::peer_share_store::PeerIngestReport, i64)> {
    ingest_share_page_landing(engine, proven_actor_hex, rows, spool_dir, None).await
}

/// [`ingest_share_page`] with its landing named: `None` is the resident tree
/// (every accepted body lands), `Some` an on-demand replica's
/// (`provider_face::owned_tree::OwnedTree::share_ingest` — rows always,
/// bodies by policy). One decode, one provenance door, one cursor rule.
#[cfg(feature = "p2p-share")]
pub(crate) async fn ingest_share_page_landing(
    engine: &SyncEngine,
    proven_actor_hex: &str,
    rows: &[Vec<u8>],
    spool_dir: PathBuf,
    landing: Option<&crate::engine::OnDemandLanding<'_>>,
) -> anyhow::Result<(crate::peer_share_store::PeerIngestReport, i64)> {
    let mut accepted = Vec::with_capacity(rows.len());
    let mut undecodable = 0usize;
    for bytes in rows {
        match fauna_core::encoding::canonical_decode::<fauna_protocol::peer_share::PeerShareChange>(
            bytes,
        ) {
            Ok(row) => accepted.push(row),
            Err(e) => {
                tracing::warn!(error = %e, "share ingest: undecodable peer row refused");
                undecodable += 1;
            }
        }
    }
    let fetcher = crate::peer_share_store::SpoolFetcher::new(spool_dir);
    let mut report = engine
        .ingest_peer_share_rows_landing(&accepted, proven_actor_hex, &fetcher, landing)
        .await?;
    report.refused += undecodable;
    let page_max = accepted
        .iter()
        .filter(|r| r.sequenced)
        .map(|r| r.change.seq)
        .max();
    // A sequenced row whose bytes did not arrive (the transfer was cut, or the
    // peer could not produce a body) holds the cursor just below it: the
    // cursor is the next pass's `since`, so carrying it past that row would
    // mean the peer never serves it again, and with the nest down it would
    // never arrive at all (`p2p.md` § Cross-user shared-set transfer — an
    // interrupted transfer resumes). Rows above it that did land are
    // re-served and judged already current, so nothing that arrived is
    // fetched twice. The cursor is forward-only, so a floor at or below it
    // simply leaves it where it is.
    let page_max = match (page_max, report.retry_floor) {
        (Some(max), Some(floor)) => Some(max.min(floor - 1)),
        (max, _) => max,
    };
    let cursor = match page_max {
        Some(seq) => engine.advance_share_pull_cursor(proven_actor_hex, seq)?,
        None => engine.share_pull_cursor(proven_actor_hex)?,
    };
    Ok((report, cursor))
}

/// Steady state: watch the folder and upload local edits (debounced), with a
/// periodic full reconcile + remote pull as the catch-up backstop.
///
/// `rescan_interval` is the constant every resident host ticks at —
/// [`rescan_interval()`] (phase 5's de-knob: `file-sync.md` § Config, the
/// phase-5 block); hosts pass it through so a test seam can inject one.
/// It is **not** the live-edit latency: the watcher already forwards an edit within
/// [`DEBOUNCE_DELAY`]; this tick catches watcher misses, offline changes, and pulls
/// remote ones.
///
/// Returns when the watcher channel closes. **Cancellation is the caller's job** —
/// both apps race this against their `EngineSpec`'s `CancellationToken`, so
/// dropping the future drops the `FsWatcher` (and with it the OS watch handle).
///
/// (This doc previously sat, misattached, above the now-removed `recv_nudge`
/// helper that had been inserted directly below it.)
pub async fn run_watch_loop(
    // `Arc` rather than by value so a caller — in production a host that wants
    // to read its own engine while the loop drives it, in a test the only way
    // to observe what the loop DID — keeps a handle. It used to be moved in
    // whole, which made every effect of this loop unobservable from outside:
    // the `Connected` arm's mode heal (below) sat unpinned at tier_1 for
    // exactly that reason, since with the engine gone there was nothing left to
    // read the healed `ModeResolution` back off. Costs one refcount; the
    // ownership story is unchanged (dropping the loop future still drops the
    // last handle, and with it the `FsWatcher`'s OS watch handle, which is what
    // every caller's cancellation path relies on).
    engine: Arc<SyncEngine>,
    watch_dir: PathBuf,
    folder: String,
    rescan_interval: Duration,
    // Best-effort remote-change nudge (`file-sync.md` § Remote-change nudge): a
    // `()` here schedules an immediate off-cadence `pull_remote_changes`, so a
    // participant sees another device's/member's save within seconds instead of
    // waiting out `rescan_interval`. The nest fires `PushEvent::SyncChanged`; the
    // host routes it here. `None` = no nudge wired; the rescan tick remains the
    // correctness backstop either way, so a missed/absent nudge costs only latency.
    mut wake_rx: Option<tokio::sync::mpsc::Receiver<()>>,
    // Invoke-and-reply commands ([`EngineCommand`]) — the agent's pipe server
    // routes a user-confirmed verb (apply-held-deletes) to the engine that owns
    // the folder. `None` = no command surface (the one-shot/lifecycle callers).
    mut cmd_rx: Option<tokio::sync::mpsc::Receiver<EngineCommand>>,
) {
    // Redacted once and reused by every log line below (`path-sealing.md` §
    // Sealed names & paths, S7): a log line is a confidentiality boundary
    // exactly like the wire and the DB.
    let folder_r = fauna_core::log_redact::log_folder_name(&folder);
    let mut local = match LocalWrites::start(&watch_dir) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("watcher start ({folder_r}): {e}");
            return;
        }
    };

    // ── Close the start-up window ──
    //
    // Every caller converges the local half BEFORE this loop exists, so a file
    // written between that catch-up's scan and the watcher above reached
    // neither: it waited out a whole `rescan_interval`. One converge now, with
    // the watcher already up, covers that gap — anything earlier is on disk for
    // this pass to find, anything later raises a watcher event. Idempotent: a
    // pass that finds nothing uploads nothing. The window opens on every engine
    // restart, and a content-key re-key restarts the engine
    // (`on-demand-files.md` § Shared sets on a capability host → *One
    // mechanism*), so an edit saved while the set re-keys is exactly what it
    // used to strand.
    let _ = LocalWriteHost::converge(&*engine, &folder).await;

    log_rescan_armed(&folder_r, rescan_interval);
    let mut rescan_tick = tokio::time::interval(rescan_interval);
    rescan_tick.tick().await; // consume the immediate first tick

    // ── One eager remote pull before settling into the cadence ──
    //
    // Every caller's startup path converges the **local** half first
    // (`LocalWriteHost::converge` — drain, reconcile, upload), but nothing had
    // ever pulled the **remote** half, and the tick above is consumed — so a
    // freshly-started engine showed the user *nothing* from the nest until a
    // whole `rescan_interval` had elapsed. A member who has just bound a folder
    // to a shared set did so precisely to receive what is already in it, and
    // instead watched an empty folder: measured at exactly 300.1 s for a
    // cross-nest member in `cross_nest_agent_capstone` (300 s because a foreign
    // set's cadence is not readable at all — see that test's notes).
    //
    // The one-shot (iOS) shape already pulls eagerly right after the same
    // catch-up (`engine_lifecycle::run_one_shot_pass`); this is the resident
    // shape agreeing with it instead of being the odd one out.
    // Idempotent, so an engine restart (key rotation, mode flip) just re-runs it.
    //
    // Resolve THIS seat's sync mode from the authoritative nest rows first —
    // above the eager pull, because that pull can deliver the very peer delete
    // the mode decides about (`file-sync.md` § 4; `SyncEngine::refresh_sync_mode`
    // owns the failure posture). The rescan tick re-resolves below, which is
    // what carries a role the user changes in the wizard onto a RUNNING engine
    // within a cadence instead of a process restart.
    // An audience flip or a website toggle the owner made while this seat was
    // down reaches the back-catalogue here, on the same refresh and for the same
    // reason: the refresh is what installs the audience/toggle the pair reads,
    // so the pair runs right after it and before the pull, and the reissues are
    // in flight when the tick cadence takes over. The caller's catch-up already
    // ran the audience pass once, off the build-time posture; the refresh can
    // install a NEWER one, and both passes are convergent, so re-running is free
    // and never wrong. The same fn the on-demand root runs.
    refresh_and_converge_corpus(&engine, &folder).await;
    // The share leg's cached writer roster rides the same cadence (B2 —
    // `refresh_share_writer_roster` owns the skip/staleness posture).
    #[cfg(feature = "p2p-share")]
    engine.refresh_share_writer_roster().await;
    // Subscribed here, right after the eager refresh above has had its chance,
    // so a `Connected` transition that lands during startup still reaches the
    // arm: a `watch::Receiver` reports every change after the point it was
    // taken, and this one is taken while the plane may well still be
    // connecting. See [`next_connected`] for why the transition — not just the
    // tick — has to be a trigger at all.
    //
    // Taken UNCONDITIONALLY since 2026-08-28: the arm's other job — re-resolving
    // this seat's sync mode — is owed on every build, not just share-feature
    // ones. It used to be `None` without `p2p-share`, which parked the arm
    // forever and left the mode with no healing trigger but the rescan tick.
    let mut conn_rx = Some(engine.control_plane().connection_state());
    if let Err(e) = engine.pull_remote_changes().await {
        tracing::error!("initial pull ({folder_r}): {e}");
    }
    // A `Placeholder` row on a resident root is a pending download, never a
    // delete — what an on-demand→always flip leaves behind, past the anchor the
    // pull reads from (`on-demand-files.md` § Linux FUSE binding, the flips
    // rule). After the pull, so a head it just moved is the one fetched; the
    // tick below retries what failed.
    materialize_placeholder_rows(&engine, &folder_r).await;
    // Third-party deposits parked while this seat was down — after the pull,
    // so an item another seat already adopted is found landed, not re-adopted.
    adopt_parked_deposits(&engine, &folder_r).await;

    loop {
        tokio::select! {
            // Terminal park (D4, `file-sync.md` § Multi-writer shared sets): the
            // authoritative nest refused this set's write grant — at the
            // cross-nest byte-plane mint or at either record plane. Leave the
            // loop rather than keep watching and re-refusing: a parked engine
            // does no further remote work, and its host surfaces the state so
            // the folder is *visibly* — never silently — untracked. Nothing on
            // disk is touched; the user's files and any pending local edits stay
            // exactly as they are. Recovery is a fresh bind, which re-runs the
            // eager mint verify (D3).
            _ = engine.access_gate().wait_revoked() => {
                tracing::warn!(
                    folder = %folder_r,
                    "write access revoked; parking this folder's engine (local files untouched)"
                );
                break;
            }

            write = local.next(&*engine) => {
                // `recorded` is deliberately unused here: an always-resident root
                // has no platform placeholder surface to flip (that is the windows
                // on-demand host's move).
                if !apply_local_write(&*engine, &folder, write).await.continue_watching {
                    break; // watcher channel closed
                }
            }

            _ = rescan_tick.tick() => {
                // The same catch-up backstop the on-demand root runs (`LocalWriteHost::converge`
                // — one implementation, both roots), plus the always-resident root's own remote
                // pull. The on-demand root pulls differently: it folds `changes.list` into
                // placeholders rather than downloading bytes.
                // The always-resident root has no placeholder surface, so it ignores
                // converge's recorded rels (flipping them ✅ is the windows on-demand
                // host's move — see `fauna-sync-agent`'s hydration loop).
                let _ = LocalWriteHost::converge(&*engine, &folder).await;
                // Refresh the mode BEFORE the pull, so a tick that re-delivers a
                // held tombstone (the unresolved-mode cap) resolves the seat
                // first — the healing order the § 4 hold is designed around, and
                // the live path a wizard role change takes to a running engine.
                // The live path an AUDIENCE flip and a WEBSITE toggle-ON take to
                // a RUNNING seat — the leg-1 lesson applied to both
                // projections: the refresh installs the new audience / toggle on
                // the write path within one tick, but until 2026-08-21 the
                // back-catalogue converged only in the once-per-start catch-up,
                // so a flip-back left the public window's plaintext
                // at rest until the agent restarted. One meta-row read each per
                // tick once converged; a re-seal, declassify or reissue on the
                // tick that first sees the flip.
                refresh_and_converge_corpus(&engine, &folder).await;
                // The cached writer roster re-reads on the same tick (B2).
                #[cfg(feature = "p2p-share")]
                engine.refresh_share_writer_roster().await;
                if let Err(e) = engine.pull_remote_changes().await {
                    tracing::error!("pull ({folder_r}): {e}");
                }
                materialize_placeholder_rows(&engine, &folder_r).await;
                adopt_parked_deposits(&engine, &folder_r).await;
            }

            // Off-cadence roster refresh: the control plane just reached
            // `Connected`, which is the one state `refresh_share_writer_roster`
            // needs and the one the rescan tick can only catch by luck. Closing
            // this window is what keeps a member from refusing every peer row
            // for the life of a set — see [`next_connected`].
            connected = next_connected(&mut conn_rx) => {
                match connected {
                    Some(()) => {
                        // Re-resolve the seat's mode FIRST, in the tick arm's
                        // healing order and for a sharper reason: this
                        // transition is the ONLY thing that heals a seat whose
                        // very first resolution raced this very connect.
                        //
                        // `run_watch_loop` resolves once at entry and then only
                        // on the rescan tick. An in-process host reaches entry
                        // while its control plane is still connecting, so both
                        // authoritative reads fail, and a seat with nothing
                        // cached resolves `Unresolved` — which declines every
                        // peer delete and holds the anchor (`file-sync.md` § 4).
                        // In production the 300 s tick eventually heals it; a
                        // deployment whose tick is longer than the window that
                        // matters never does. Found live 2026-08-28: every
                        // `native`/`tui` cell of `test_seats_converge` red at
                        // the delete leg with `old=Resolved(Sync)
                        // new=Unresolved` logged a millisecond after
                        // `authenticated WS connect`, while every headless
                        // `engine` cell passed — the (since-removed) daemon
                        // resolved inside `run_ws_session`, after its session
                        // was already up.
                        engine.refresh_sync_mode().await;
                        // Then PULL, because resolving alone changes nothing a
                        // peer can see: an unresolved seat HELD its anchor at
                        // the first declined tombstone precisely so the
                        // tombstone re-delivers "once the role is readable" —
                        // and this is the moment it became readable. Without
                        // this the held changes wait for the next nudge or
                        // tick, and the nudge that would have carried them has
                        // already been consumed and declined. It is also the
                        // ordinary reconnect catch-up (§ Offline Catch-Up).
                        if let Err(e) = engine.pull_remote_changes().await {
                            tracing::error!("reconnect pull ({folder_r}): {e}");
                        }
                        #[cfg(feature = "p2p-share")]
                        engine.refresh_share_writer_roster().await;
                    }
                    // The control plane is gone for good. Park the arm rather
                    // than let a dropped watch sender spin the loop.
                    None => conn_rx = None,
                }
            }

            // Off-cadence remote-change nudge: the nest signalled a new sync
            // record landed in this set, so pull the remote half now instead of
            // waiting out the rescan tick. Pull-only (no local converge) — the
            // nudge is a *download* latency win; the local half already rides the
            // watcher. Best-effort: a failed pull is retried by the next tick.
            Some(()) = fauna_core::select_pending::recv_or_pending(&mut wake_rx) => {
                if let Err(e) = engine.pull_remote_changes().await {
                    tracing::error!("nudge pull ({folder_r}): {e}");
                }
                // A deposit's nudge reaches the owner's seats alone; the pass
                // is one list call when nothing is parked.
                adopt_parked_deposits(&engine, &folder_r).await;
            }

            // Invoke-and-reply command from the host (the pipe server's caller
            // awaits the oneshot) — the one shared answer, which the on-demand
            // hydration loop runs too.
            Some(cmd) = fauna_core::select_pending::recv_or_pending(&mut cmd_rx) => {
                answer_engine_command(&engine, cmd).await;
            }
        }
    }
}

/// One third-party deposit adoption pass (`SyncEngine::adopt_deposits`,
/// `file-sync.md` § Third-party deposit ingress), on the resident root's entry,
/// tick and nudge. Best-effort: a failed pass is retried by the next one, and
/// nothing is retired that is not durable.
async fn adopt_parked_deposits(engine: &SyncEngine, folder: &str) {
    match engine.adopt_deposits().await {
        Ok(done) if done != crate::engine::DepositAdoption::default() => {
            tracing::info!(
                folder,
                adopted = done.adopted,
                already_landed = done.already_landed,
                deferred = done.deferred,
                "deposit adoption pass"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("deposit adoption ({folder}): {e:#}"),
    }
}

/// The `Connected`-transition arm that closes the writer-roster hole
/// ([`next_connected`]).
///
/// Both properties here are ones the resident loop cannot assert about itself:
/// the loop needs a live engine, a nest and a folder, while the question is
/// purely about the watch receiver's edges.
#[cfg(test)]
mod connected_arm_tests {
    use super::next_connected;
    use fauna_client::ConnectionState;
    use futures_util::FutureExt;
    use tokio::sync::watch;

    /// The arm fires on the transition INTO `Connected` and on nothing else.
    ///
    /// The non-firing half is what matters: `refresh_share_writer_roster`
    /// silently no-ops unless the plane is `Connected`, so an arm that woke on
    /// every state change would spend a reconnect storm calling a refresh that
    /// cannot do anything — and would still not have closed the hole.
    #[tokio::test]
    async fn fires_on_the_transition_into_connected_and_not_on_other_states() {
        let (tx, rx) = watch::channel(ConnectionState::Disconnected);
        let mut rx = Some(rx);

        tx.send(ConnectionState::Connecting).unwrap();
        assert!(
            next_connected(&mut rx).now_or_never().is_none(),
            "a non-Connected transition must not wake the loop"
        );

        tx.send(ConnectionState::Connected).unwrap();
        assert_eq!(
            next_connected(&mut rx).now_or_never(),
            Some(Some(())),
            "reaching Connected is the edge the roster refresh hangs on"
        );
    }

    /// A dropped control plane parks the arm instead of spinning the loop.
    ///
    /// `watch::Receiver::changed()` returns `Err` **immediately and forever**
    /// once the sender is gone. An arm that merely returned on that would be
    /// re-polled on every iteration of the `select!` loop and would pin a core
    /// for the life of the process. The contract is therefore two-part, and
    /// both parts are asserted: the helper answers `None` once, the caller
    /// takes the receiver out, and from then on the future never resolves.
    #[tokio::test]
    async fn a_dropped_control_plane_parks_the_arm_rather_than_spinning() {
        let (tx, rx) = watch::channel(ConnectionState::Disconnected);
        let mut rx = Some(rx);
        drop(tx);

        assert_eq!(
            next_connected(&mut rx).now_or_never(),
            Some(None),
            "a dropped sender must be reported once, so the caller can park the arm"
        );

        // What the caller does with that `None` — the `select!` arm's own body.
        rx = None;

        assert!(
            next_connected(&mut rx).now_or_never().is_none(),
            "a parked arm must never resolve again — resolving IS the busy-spin"
        );
    }

    /// Without a receiver the arm is inert from the start — the shape a build
    /// with the share feature off gets, so the loop never wakes for a refresh
    /// that does not exist.
    #[tokio::test]
    async fn no_receiver_is_an_inert_arm() {
        let mut rx = None;
        assert!(
            next_connected(&mut rx).now_or_never().is_none(),
            "an unsubscribed arm must park, not fire"
        );
    }
}

#[cfg(test)]
mod dir_event_tests {
    use super::*;

    /// A descriptor reach, and anything under one, is recognized — and an
    /// ordinary path that merely mentions `/proc` is not.
    #[test]
    fn a_descriptor_reach_is_recognized_by_its_prefix() {
        use std::path::Path;
        assert!(is_descriptor_reach(Path::new("/proc/self/fd/7")));
        assert!(is_descriptor_reach(Path::new("/proc/self/fd/7/sub")));
        assert!(!is_descriptor_reach(Path::new("/home/u/proc/self/fd/7")));
        assert!(!is_descriptor_reach(Path::new("/proc/self/fdinfo/7")));
    }

    struct NopHost;

    #[async_trait::async_trait(?Send)]
    impl LocalWriteHost for NopHost {
        fn is_ignored(&self, _rel: &str) -> bool {
            false
        }
        fn was_recent_download(&self, _rel: &str) -> bool {
            false
        }
        fn was_recent_removal(&self, _rel: &str) -> bool {
            false
        }
        async fn upload_file(&self, _rel: &str) -> anyhow::Result<crate::engine::UploadOutcome> {
            Ok(Default::default())
        }
        async fn handle_delete(&self, _rel: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn converge(&self, _folder: &str) -> Vec<String> {
            Vec::new()
        }
    }

    /// A directory Created/Modified event must never become an upload: a folder
    /// carries directories implicitly via child paths, and `upload_file` on one
    /// fails at the read ("reading <dir>") — and since every write *inside* a
    /// directory also raises a Modified event on the directory itself, each child
    /// edit re-queued the parent and logged a fresh ERROR (live 2026-07-17: an
    /// error per child write inside any new folder).
    #[tokio::test]
    async fn a_directory_event_is_not_a_user_write() {
        let tmp = tempfile::tempdir().unwrap();
        let lw = LocalWrites::start(tmp.path()).unwrap();
        std::fs::create_dir(tmp.path().join("New folder")).unwrap();
        std::fs::write(tmp.path().join("real.txt"), b"x").unwrap();

        assert!(
            !lw.is_user_write(&NopHost, "New folder"),
            "a directory is not uploadable content"
        );
        assert!(
            lw.is_user_write(&NopHost, "real.txt"),
            "a plain file still is"
        );
    }

    /// **A directory that appears already holding files queues every one of
    /// them.** A recursive OS watch learns about a new directory only when it
    /// appears, and adds its watch after that — so a file written into a fresh
    /// tree before the watch lands (`mkdir -p` + a write, a copied-in folder, an
    /// unzip) raises no event of its own, and waited for the reconcile backstop
    /// (300 s in production). Found by the two-seat `nested-dirs` scenario
    /// (`tests/e2e-unified/helpers/seat_scenarios.py`, 2026-09-30): the file three
    /// new directories down never left the writer. Hidden and ignored
    /// subtrees stay out, exactly as their own events would.
    #[tokio::test]
    async fn a_new_directory_queues_the_files_already_inside_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut lw = LocalWrites::start(tmp.path()).unwrap();
        lw.debouncer = crate::debouncer::EventDebouncer::new(std::time::Duration::ZERO);

        let root = lw.watch_dir.clone();
        std::fs::create_dir_all(root.join("new/deeper/deepest")).unwrap();
        std::fs::write(root.join("new/top.txt"), b"a").unwrap();
        std::fs::write(root.join("new/deeper/deepest/file.txt"), b"b").unwrap();
        std::fs::create_dir_all(root.join("new/.git")).unwrap();
        std::fs::write(root.join("new/.git/HEAD"), b"c").unwrap();
        std::fs::write(root.join("new/.hidden"), b"d").unwrap();

        lw.touch_new_subtree(&NopHost, &root.join("new"));

        let mut queued = lw.debouncer.drain_ready();
        queued.sort();
        assert_eq!(
            queued,
            vec![
                "new/deeper/deepest/file.txt".to_string(),
                "new/top.txt".to_string()
            ],
            "every visible file under the new directory is queued, hidden ones never"
        );
    }

    /// **The download echo and the removal echo are independent one-shot
    /// channels.** Both suppressions are consume-on-check, so sharing one set
    /// let the *download*'s `Created` event eat the token the *removal*'s
    /// `Removed` event needed.
    ///
    /// That is not hypothetical. Applying a remote delete for a path this
    /// device downloaded moments earlier — the ordinary case when a fresh
    /// folder binds to a set whose history contains deletes — queues both
    /// events against one token: `Created` consumed it, `Removed` came up
    /// empty and became a `LocalWrite::Delete`, and the engine re-recorded a
    /// delete it had just *applied* from the nest. Observed live 2026-07-24
    /// (macOS multiseat seat, run `macosfix1`): 24 applied remote deletes, 24
    /// deletes re-recorded back to the nest ~0.5 s later.
    #[test]
    fn the_download_echo_does_not_disarm_the_removal_echo() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = crate::pull_remote_changes_test::test_engine(tmp.path().to_path_buf());

        // A download writes the file, then an applied remote delete removes it:
        // both arm their own suppression for the same path.
        engine.note_recent_download("f.txt");
        engine.note_recent_removal("f.txt");

        // The watcher drains the download's Created event first.
        assert!(
            engine.was_recent_download("f.txt"),
            "the Created event must find the download token"
        );
        // ...and the removal's Removed event must STILL be suppressed.
        assert!(
            engine.was_recent_removal("f.txt"),
            "the Removed event's own token must survive the Created event — else \
             the engine re-records a delete it just applied from the nest"
        );

        // Both are one-shot: a genuine later user delete is not suppressed.
        assert!(
            !engine.was_recent_removal("f.txt"),
            "the removal token is consume-on-check"
        );
    }
}
