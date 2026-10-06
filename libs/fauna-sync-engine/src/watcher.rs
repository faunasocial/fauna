use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

/// Events emitted by the filesystem watcher.
#[derive(Debug, Clone)]
pub enum FsEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
}

/// Watches a directory for filesystem changes.
pub struct FsWatcher {
    _watcher: RecommendedWatcher,
    pub events: mpsc::Receiver<FsEvent>,
    dropped: Arc<AtomicU64>,
}

/// How many events this watcher dropped because the queue was full.
///
/// Non-zero means the consumer fell behind and the named paths sync at the next
/// rescan instead of in real time — the deliberate trade made in
/// [`forward_event`]. It is a health signal, not an error: zero is the normal
/// steady state, and a climbing count is what tells a future diagnosis that the
/// consumer, not the watcher, is the slow half.
impl FsWatcher {
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Hand one classified event to the consumer without ever blocking the caller.
///
/// **This is an OS callback thread**, and that is the whole reason this function
/// exists. The obvious `blocking_send` parks *this* thread until the consumer
/// drains — so one stalled consumer wedges the watcher permanently, every later
/// event is lost with no drop and no log, and the only recovery is a process
/// restart. `try_send` inverts the failure: the queue's tail is dropped, loudly
/// and counted, and the watcher keeps delivering everything after it.
///
/// The trade is favourable *because the rescan heals a drop and nothing heals a
/// wedge*. The rescan interval (300 s in production) re-lists the folder and
/// reconciles creates, modifies **and** deletes — the delete leg via the engine's
/// delete-detection pass — so a dropped event costs latency, bounded by one rescan. A wedged
/// callback thread costs every subsequent event until restart, unbounded. Owner
/// doc: `docs/goal/behavior/file-sync.md` § Implementation status today.
///
/// Returns `true` if the event reached the consumer.
fn forward_event(tx: &mpsc::Sender<FsEvent>, event: FsEvent, dropped: &AtomicU64) -> bool {
    match tx.try_send(event) {
        Ok(()) => true,
        Err(TrySendError::Full(e)) => {
            // Log the first drop of a burst and then on powers of two, so a
            // sustained overflow leaves a proportional trace instead of one line
            // per event — a flood that buries its own diagnosis is how the
            // 2026-08-05 blackout stayed unexplained for four sessions.
            let n = dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                let (event_kind, path) = match &e {
                    FsEvent::Created(p) => ("created", p),
                    FsEvent::Modified(p) => ("modified", p),
                    FsEvent::Removed(p) => ("removed", p),
                };
                tracing::warn!(
                    dropped_total = n,
                    event_kind,
                    path = %fauna_core::log_redact::log_path(&path.to_string_lossy()),
                    "watcher: queue full — dropping this event; the consumer is behind. \
                     The path it named syncs at the next rescan, not in real time"
                );
            }
            false
        }
        Err(TrySendError::Closed(_)) => {
            tracing::warn!(
                "watcher: dropped an event — the consumer is gone; \
                 the path it named syncs only at the next rescan"
            );
            false
        }
    }
}

impl FsWatcher {
    /// Start watching the given directory. Events are sent to the returned receiver.
    pub fn start(watch_dir: &Path) -> Result<Self> {
        let (tx, rx) = mpsc::channel(1024);

        let dropped = Arc::new(AtomicU64::new(0));
        let dropped_tx = Arc::clone(&dropped);
        let tx_clone = tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                // Every way an event can DIE here is logged, deliberately.
                //
                // A watcher event that vanishes with no trace is the worst
                // failure this loop has: the file simply never syncs, no log
                // line anywhere says why, and the only recovery is the rescan
                // tick — 300 s in production, longer than any test's budget, so
                // a single lost event reads as a total sync failure. The one
                // such disappearance on record (a macOS 3-seat run, 2026-08-05,
                // where a freshly written file appeared in no seat's log at all)
                // was root-caused to a mispinned macOS backend running kqueue
                // instead of FSEvents, and fixed — it was not a
                // queue failure. These arms are what let the NEXT occurrence be
                // diagnosed instead of guessed at.
                match res {
                    Ok(event) => {
                        for path in event.paths {
                            match classify_event(&event.kind, &path) {
                                Some(e) => {
                                    // Never blocks this callback thread — a full
                                    // queue drops the tail, counted and logged,
                                    // and the watcher keeps delivering. Why that
                                    // is the safer half of the trade, and the
                                    // `blocking_send` wedge it replaced (built
                                    // 2026-08-11): [`forward_event`].
                                    forward_event(&tx_clone, e, &dropped_tx);
                                }
                                // Not a kind we act on (access, metadata-only,
                                // and `Any`/`Other`, which is what a coalescing
                                // backend falls back to when it cannot say what
                                // happened). Trace, not warn: the common case
                                // here is genuinely uninteresting.
                                None => tracing::trace!(
                                    kind = ?event.kind,
                                    "watcher: unclassified event kind, ignored"
                                ),
                            }
                        }
                    }
                    // The OS telling us it could not keep up. FSEvents and
                    // inotify both surface a queue overflow this way, and it is
                    // precisely the "events were lost" signal — dropping it
                    // silently is how a lost write becomes unexplainable.
                    Err(e) => tracing::warn!(
                        error_kind = ?e.kind,
                        paths = ?e
                            .paths
                            .iter()
                            .map(|p| fauna_core::log_redact::log_path(&p.to_string_lossy()))
                            .collect::<Vec<_>>(),
                        "watcher: the OS reported a watch error — events may have been \
                         LOST; anything missed syncs only at the next rescan"
                    ),
                }
            })?;

        watcher.watch(watch_dir, RecursiveMode::Recursive)?;

        Ok(Self {
            _watcher: watcher,
            events: rx,
            dropped,
        })
    }
}

/// Classify a raw `notify` event kind + path into our coarse [`FsEvent`].
///
/// Renames are the subtle case. `notify` reports a rename as
/// `Modify(Name(..))` — Windows `ReadDirectoryChangesW` emits
/// `RENAMED_OLD_NAME` + `RENAMED_NEW_NAME`, Linux inotify emits
/// `IN_MOVED_FROM` + `IN_MOVED_TO` — NOT as a Remove + Create pair. The old
/// name no longer exists and the new name does, so we classify a `Name` event
/// by on-disk existence rather than the platform-varying `RenameMode`: a gone
/// path is a remove, a present one a create. Without this, the old name falls
/// into the generic `Modify` arm, the engine then fails to read the now-absent
/// file (the change is silently dropped), and the delete never propagates — so
/// a renamed-away file lingers forever on every other device.
///
/// ⚠ *"Gone"* here means [`crate::presence::Presence::Absent`] — a definite
/// `NotFound` — never the `Path::exists()` this used to ask. Both
/// arms below choose between `Created` (harmless: the read that follows either
/// works or drops the change) and `Removed` (a delete that propagates to the
/// nest and every device), so an *unreadable* path — a parent whose mode just
/// changed, a mount mid-blip — must fall on the `Created` side. Absence of
/// evidence is not evidence of a delete, and reconcile's scan will record the
/// real one later, behind the mass-delete floor.
fn classify_event(kind: &notify::EventKind, path: &Path) -> Option<FsEvent> {
    use notify::EventKind;
    use notify::event::ModifyKind;
    let still_there = || !crate::presence::path_presence(path).is_absent();
    match kind {
        EventKind::Create(_) => Some(FsEvent::Created(path.to_path_buf())),
        EventKind::Modify(ModifyKind::Name(_)) => {
            if still_there() {
                Some(FsEvent::Created(path.to_path_buf()))
            } else {
                Some(FsEvent::Removed(path.to_path_buf()))
            }
        }
        EventKind::Modify(_) => Some(FsEvent::Modified(path.to_path_buf())),
        EventKind::Remove(_) => {
            if still_there() {
                // FSEvents (macOS) coalesces flags per path: an atomic replace
                // (`rename` over an existing file — every editor's save dance)
                // surfaces the replaced inode's unlink as `ItemRemoved`
                // alongside `ItemRenamed` on the SAME path, and the derived
                // Remove event would cancel the pending upload and tombstone a
                // file that is right there on disk. Same rule as the rename
                // arm: classify by on-disk existence, not the
                // platform-varying event semantics.
                Some(FsEvent::Created(path.to_path_buf()))
            } else {
                Some(FsEvent::Removed(path.to_path_buf()))
            }
        }
        _ => None,
    }
}

/// Perform a full scan of a directory, returning relative paths and metadata.
///
/// [`full_scan_filtered`] with nothing ignored, rather than a second walk of its
/// own. It WAS a second walk — a copy carrying the identical
/// `PermissionDenied => return Ok(())` arm that made an unreadable directory
/// read as an empty one — and although only tests reach this entry point today,
/// a duplicated walk beside a fixed one is a loaded gun: the next caller to
/// pick the shorter name would silently re-open the finding. One walk, so the
/// two cannot diverge again.
pub fn full_scan(watch_dir: &Path) -> Result<Vec<ScannedFile>> {
    Ok(full_scan_filtered(watch_dir, &crate::ignore::IgnoreMatcher::from_patterns(&[]))?.files)
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub relative_path: String,
    pub size_bytes: u64,
    pub mtime_secs: i64,
    /// The file is here, but its **bytes are not** — a cloud-only placeholder on an on-demand
    /// root ([`crate::placeholder`]).
    ///
    /// Reported rather than filtered out, deliberately, because the two things a caller must do
    /// with it point opposite ways: **never open it** (the provider's own read of its own
    /// placeholder blocks for cfapi's 60 s timeout and fails), yet **never treat it as absent**
    /// either (dropping it from the scan makes `reconcile`'s delete-detection record a delete on
    /// the nest and erase the user's file from every device). Present, unreadable, and flagged.
    pub is_placeholder: bool,
}

/// Normalize an already-`root`-relative `path` to the forward-slash wire/db key.
///
/// The wire/db keys files by relative path and must be byte-identical across
/// all platforms (file-sync.md: `path_hash` = BLAKE3 of the *normalized* path).
/// `to_string_lossy` yields the OS separator, so on Windows a raw conversion
/// would produce `sub\file` and diverge from every other app's `sub/file`.
/// On Unix `MAIN_SEPARATOR` is already `/`, so this is a no-op there and
/// preserves any literal backslash in a Unix filename.
///
/// Every relative-path consumer (the `full_scan` scan path here, plus the
/// `notify` *event*-path conversions) routes through this one function so scan and event paths never diverge.
///
/// The rule itself lives in [`fauna_core::sync::normalize_rel_path`], beside the
/// [`fauna_core::sync::path_hash`] that consumes it — the nest hashes whatever
/// string the client normalizes here, so one crate must own both halves.
pub fn normalize_rel(relative: &Path) -> String {
    fauna_core::sync::normalize_rel_path(relative)
}

/// Build a sync-engine relative path from an absolute `path` under `root`,
/// normalized to forward slashes via [`normalize_rel`].
fn relative_path_str(path: &Path, root: &Path) -> String {
    normalize_rel(path.strip_prefix(root).unwrap_or(path))
}

/// The watch-dir-relative key for a watcher **event** path, or `None` when the
/// event is about the watch root itself rather than anything inside it.
///
/// Every event consumer must route through this instead of hand-rolling
/// `strip_prefix` + [`normalize_rel`], because that pair silently yields the
/// EMPTY STRING for the root: `strip_prefix(root)` on `root` succeeds with `""`,
/// which then passes every downstream filter (`""` does not start with `'.'`,
/// no ignore rule matches it) and is queued as if it were a file. The host
/// then retries reading the watch *directory* as a file on every debounce tick,
/// logging `WS file change failed path=path~af1349b9f5f9 error=reading …`
/// forever — `af1349b9f5f9` being BLAKE3("")'s prefix, the fingerprint of this
/// bug in any log. Worse, a Removed event for the root (its own teardown, an
/// unmount, a replaced folder) would notify the nest of a `delete` for the
/// empty path.
///
/// macOS surfaces it far more than Linux because FSEvents reports
/// directory-granular events for the watched root, but nothing here is
/// platform-specific: the guard belongs to the shared funnel so every consumer
/// gets it once. Found 2026-08-05 by the `[3seat-engine+engine+engine]` cell on macOS.
/// The second reason this funnel exists: an event path and the configured root
/// can name the same directory through **different symlink spellings**, and a
/// bare `strip_prefix` then rejects EVERY event for the watch dir. macOS is where
/// this bites — FSEvents reports fully-resolved paths, so a root of
/// `/var/folders/…` (itself a symlink to `/private/var/folders/…`, as every
/// `TMPDIR` and every `tempfile::tempdir()` is) sees each event arrive as
/// `/private/var/folders/…` and strips to `None`. The failure is total and
/// silent: the watcher is healthy, events flow, and not one of them is ever
/// attributed to a file. It is not test-only — a user whose synced folder is
/// reached through any symlink (`/tmp`, `/home`, an external volume alias, a
/// symlinked home) hits exactly the same wall.
///
/// So a failed direct strip falls back to comparing against the **canonicalized**
/// root before giving up. Canonicalizing the *root* (not the event path) is
/// deliberate: the root is a directory that certainly exists, while the event
/// path may name a file already deleted — which `canonicalize` cannot resolve, so
/// a Removed event would still be dropped. Deliberately NOT cached in a process
/// -wide `static`: one process legitimately watches several roots (a client with
/// several folders, and the test binary), so a single cached root would be
/// applied to every other one. The syscall runs only on the branch that would
/// otherwise lose the event — a canonically-spelled root never reaches it.
pub fn event_rel_path(path: &Path, root: &Path) -> Option<String> {
    let canonical;
    let stripped = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => {
            // Only reached when the spellings differ; see the symlink note above.
            canonical = std::fs::canonicalize(root).ok()?;
            path.strip_prefix(&canonical).ok()?
        }
    };
    let rel = normalize_rel(stripped);
    (!rel.is_empty()).then_some(rel)
}

/// What one [`full_scan_filtered`] pass could actually see.
///
/// Two lists, because a scan has two outcomes and only one of them is visible
/// in a file list. A directory the scan could not enumerate contributes **zero
/// files** — indistinguishable, in a bare `Vec<ScannedFile>`, from a directory
/// that is genuinely empty — and the delete-detection pass downstream reads
/// *"not in the scan"* as *"the user deleted it"*. So one `chmod 000` on a
/// synced subdirectory recorded a delete for every file under it, on the nest
/// and on every other device of the set.
///
/// Carrying the prefixes beside the files makes that difference
/// **representable**, and [`SyncEngine::missing_from_scan`] withholds every
/// synced row under one. The alternative — failing the whole scan on an
/// unreadable directory — is safe for deletes but stops the entire folder
/// syncing over one directory that may hold no synced rows at all, and may even
/// be ignored; the prefix keeps the blast radius the size of the fault.
///
/// [`SyncEngine::missing_from_scan`]: crate::engine::SyncEngine
#[derive(Debug, Default, Clone)]
pub struct DirectoryScan {
    /// Every file the scan enumerated, sorted by relative path.
    pub files: Vec<ScannedFile>,
    /// Watch-dir-relative paths the scan could **not** enumerate, sorted. A
    /// directory whose `read_dir` failed, a directory whose iteration failed
    /// part-way (we no longer know what it holds), or a single entry whose
    /// `stat` failed. The empty string is the watch root itself — it prefixes
    /// every path, so an unreadable root withholds the whole set.
    pub unreadable_prefixes: Vec<String>,
}

impl DirectoryScan {
    /// `true` when `relative_path` sits under something this scan could not
    /// enumerate, so its absence from [`Self::files`] is *no evidence* about
    /// the file.
    pub fn hides(&self, relative_path: &str) -> bool {
        is_under_unreadable_prefix(relative_path, &self.unreadable_prefixes)
    }
}

/// Does `relative_path` sit at or under one of `prefixes`?
///
/// Segment-aware on purpose: a raw `starts_with` would let the unreadable
/// directory `photos` withhold the unrelated sibling row `photos-backup/a.txt`,
/// quietly suppressing a real delete. The empty prefix is the watch root and
/// matches everything.
///
/// Public so every delete rail asks this one question: two hosts' delete
/// rails have drifted apart once already (`delete-propagation.md`, the
/// 2026-08-05 asymmetry).
pub fn is_under_unreadable_prefix(relative_path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        prefix.is_empty()
            || relative_path == prefix.as_str()
            || (relative_path.len() > prefix.len()
                && relative_path.starts_with(prefix.as_str())
                && relative_path.as_bytes()[prefix.len()] == b'/')
    })
}

/// Like `full_scan` but skips files matching the ignore patterns.
pub fn full_scan_filtered(
    watch_dir: &Path,
    ignore: &crate::ignore::IgnoreMatcher,
) -> Result<DirectoryScan> {
    let mut scan = DirectoryScan::default();
    scan_recursive_filtered(watch_dir, watch_dir, ignore, &mut scan)?;
    scan.files
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    scan.unreadable_prefixes.sort();
    scan.unreadable_prefixes.dedup();
    Ok(scan)
}

/// Record `dir` (or a single entry under it) as something this pass could not
/// read, and say so loudly — a withheld delete is a *deferred* one, and the log
/// is where a stuck folder is diagnosed.
fn note_unreadable(root: &Path, path: &Path, error: &std::io::Error, scan: &mut DirectoryScan) {
    let relative = relative_path_str(path, root);
    tracing::warn!(
        path = %fauna_core::log_redact::log_path(&relative),
        error = %error,
        "scan: cannot enumerate this path — treating everything under it as PRESENT, not \
         deleted; its synced rows are withheld from delete detection until it reads again"
    );
    scan.unreadable_prefixes.push(relative);
}

fn scan_recursive_filtered(
    root: &Path,
    dir: &Path,
    ignore: &crate::ignore::IgnoreMatcher,
    scan: &mut DirectoryScan,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // A MISSING root still errors the whole scan, exactly as
        // `delete-propagation.md` § the mass-delete floor says it does: the
        // folder is gone (unmounted, renamed), the caller has nothing to
        // reconcile against, and aborting is both the safe and the honest
        // answer. Every OTHER root failure — the root turned mode-0, its mount
        // went EIO/ESTALE — used to be read as "the folder is empty", which is
        // strictly worse than the missing-root case it was assumed to share:
        // every synced row reads missing at once, the mass-delete floor HOLDS
        // (it engages precisely on totality), the apps render *"your folder
        // emptied — apply N deletions"*, and one click erases the set. It becomes the empty prefix instead, which withholds every
        // row, so the floor sees nothing missing and no surface ever offers the
        // click.
        Err(e) if dir == root && e.kind() == std::io::ErrorKind::NotFound => {
            return Err(e.into());
        }
        Err(e) => {
            note_unreadable(root, dir, &e, scan);
            return Ok(());
        }
    };

    for entry in entries {
        // Iteration failed part-way: we no longer know what this directory
        // holds, so the DIRECTORY (not the entry we failed to get) is what
        // becomes unreadable — the files we did enumerate before the fault are
        // still reported, but nothing under here may be read as deleted.
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                note_unreadable(root, dir, &e, scan);
                return Ok(());
            }
        };
        let path = entry.path();

        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }

        let relative = relative_path_str(&path, root);

        if ignore.is_ignored(&relative) {
            continue;
        }

        // `file_type` and `metadata` are `lstat`s on most filesystems, so they
        // fail for the same reasons `read_dir` does (a parent's mode changed
        // under the walk, a mount blipped). Withhold that one path rather than
        // aborting the pass: as a prefix it covers the entry whether it turned
        // out to be a file or a directory.
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                note_unreadable(root, &path, &e, scan);
                continue;
            }
        };

        if ft.is_dir() {
            scan_recursive_filtered(root, &path, ignore, scan)?;
        } else if ft.is_file() {
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(e) => {
                    note_unreadable(root, &path, &e, scan);
                    continue;
                }
            };
            let mtime = meta
                .modified()
                .ok()
                .map(fauna_core::data::Timestamp::secs_or_zero)
                .unwrap_or(0);

            scan.files.push(ScannedFile {
                relative_path: relative,
                size_bytes: meta.len(),
                mtime_secs: mtime,
                is_placeholder: crate::placeholder::is_cloud_placeholder(&meta),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A stalled consumer must not wedge the OS callback thread, and the
    /// watcher must keep working once that consumer catches up.
    ///
    /// This is the property the `blocking_send` → `try_send` change bought
    /// (2026-08-11). It is asserted as *state*, never as timing — no sleep, no
    /// deadline, nothing load-bearing about how fast anything runs.
    ///
    /// Red-verified 2026-08-11 by reverting `forward_event` to `blocking_send`:
    /// the test fails at the first overflowing send. Note *how* it fails, since
    /// it is not what the production hazard looks like — on a `#[tokio::test]`
    /// runtime thread tokio detects the blocking call and panics outright
    /// ("Cannot block the current thread from within a runtime"), whereas the
    /// real watcher callback is a plain OS thread with no runtime to object, so
    /// there the same call parks silently and forever. The test therefore
    /// catches the defect *earlier and louder* than production would; do not
    /// read its clean panic as evidence that a wedge would be noisy in the
    /// field. It would be perfectly silent, which is the whole reason this
    /// changed.
    #[tokio::test]
    async fn a_full_queue_drops_and_counts_instead_of_wedging_the_callback_thread() {
        let (tx, mut rx) = mpsc::channel(2);
        let dropped = AtomicU64::new(0);

        // Fill the queue to capacity.
        assert!(forward_event(&tx, FsEvent::Created("a".into()), &dropped));
        assert!(forward_event(&tx, FsEvent::Created("b".into()), &dropped));
        assert_eq!(dropped.load(Ordering::Relaxed), 0, "nothing dropped yet");

        // Now overflow it. Reaching the next line at all IS the assertion that
        // matters — `blocking_send` would have parked here forever.
        assert!(!forward_event(&tx, FsEvent::Created("c".into()), &dropped));
        assert!(!forward_event(&tx, FsEvent::Modified("d".into()), &dropped));
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            2,
            "each overflowed event is counted, so a diagnosis can see the consumer fell behind"
        );

        // The events that fit are intact and in order — a drop costs the tail,
        // never the queue's contents.
        assert!(matches!(rx.recv().await, Some(FsEvent::Created(p)) if p == Path::new("a")));
        assert!(matches!(rx.recv().await, Some(FsEvent::Created(p)) if p == Path::new("b")));

        // And the watcher recovers: once the consumer drains, later events are
        // delivered again. This is what makes the drop a bounded latency cost
        // (healed at the next rescan) rather than a permanent one.
        assert!(forward_event(&tx, FsEvent::Removed("e".into()), &dropped));
        assert!(matches!(rx.recv().await, Some(FsEvent::Removed(p)) if p == Path::new("e")));
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            2,
            "recovery does not retroactively change the drop count"
        );
    }

    /// A gone consumer is reported, and is not counted as a queue-pressure drop
    /// — the two failures have different remedies and must stay distinguishable.
    #[tokio::test]
    async fn a_closed_receiver_is_not_counted_as_a_full_queue_drop() {
        let (tx, rx) = mpsc::channel::<FsEvent>(2);
        drop(rx);
        let dropped = AtomicU64::new(0);

        assert!(!forward_event(&tx, FsEvent::Created("a".into()), &dropped));
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            0,
            "a shut-down consumer is not the queue falling behind"
        );
    }

    #[test]
    fn normalize_rel_uses_forward_slashes() {
        // A nested relative path built with the OS separator must serialize to
        // the forward-slash wire/db key on every platform (the `path_hash`
        // invariant). On Windows `a\b\c` must become `a/b/c`; on Unix it is
        // already `a/b/c`. This is the one normalizer every scan- and
        // event-path consumer shares, so they can never diverge.
        let rel = std::path::Path::new("a").join("b").join("c");
        assert_eq!(normalize_rel(&rel), "a/b/c");
    }

    #[test]
    fn event_rel_path_refuses_the_watch_root_itself() {
        // The regression this pins: `strip_prefix(root)` on `root` SUCCEEDS
        // with an empty path, so the naive `strip_prefix` + `normalize_rel`
        // pair hands its caller `""` — a "relative path" that passes the
        // `starts_with('.')` and ignore-rule filters and is then queued as a
        // changed file. Measured cost on macOS: the host retried reading the
        // watch DIRECTORY as a file on every debounce tick for the whole run.
        let root = std::path::Path::new("/watch");
        assert_eq!(event_rel_path(root, root), None);

        // A trailing-separator spelling of the same directory is still the
        // root — the naive pair yields `""` for this one too.
        assert_eq!(event_rel_path(std::path::Path::new("/watch/"), root), None);
    }

    #[test]
    fn event_rel_path_keeps_real_paths_and_rejects_outsiders() {
        let root = std::path::Path::new("/watch");
        assert_eq!(
            event_rel_path(&root.join("a.txt"), root).as_deref(),
            Some("a.txt")
        );
        // Nested paths keep the shared forward-slash normalization.
        assert_eq!(
            event_rel_path(&root.join("sub").join("b.txt"), root).as_deref(),
            Some("sub/b.txt")
        );
        // A path outside the root is not silently re-rooted: the old
        // `unwrap_or(path)` fallback in `relative_path_str` would have turned
        // it into an absolute-looking "relative" key.
        assert_eq!(
            event_rel_path(std::path::Path::new("/elsewhere/c.txt"), root),
            None
        );
    }

    #[test]
    fn full_scan_finds_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), "bbb").unwrap();
        // Hidden file should be skipped
        std::fs::write(dir.path().join(".hidden"), "xxx").unwrap();

        let files = full_scan(dir.path()).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert!(paths.contains(&"a.txt"));
        assert!(paths.contains(&"sub/b.txt"));
        assert!(!paths.contains(&".hidden"));
    }

    #[test]
    fn full_scan_filtered_excludes_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(dir.path().join("skip.log"), "skip").unwrap();
        std::fs::create_dir(dir.path().join("build")).unwrap();
        std::fs::write(dir.path().join("build/out.o"), "obj").unwrap();

        let matcher = crate::ignore::IgnoreMatcher::from_patterns(&["*.log", "build"]);
        let scan = full_scan_filtered(dir.path(), &matcher).unwrap();
        let paths: Vec<&str> = scan
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect();
        assert!(paths.contains(&"keep.txt"));
        assert!(!paths.contains(&"skip.log"));
        assert!(!paths.contains(&"build/out.o"));
        assert!(
            scan.unreadable_prefixes.is_empty(),
            "an IGNORED directory is not an unreadable one — the scan read it fine and chose to \
             skip it, so its rows must stay eligible for ordinary delete detection"
        );
    }

    #[tokio::test]
    async fn watcher_detects_create() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = FsWatcher::start(dir.path()).unwrap();

        // Give the watcher time to set up
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Create a file
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();

        // Should receive at least one event
        let event = tokio::time::timeout(Duration::from_secs(2), w.events.recv()).await;
        assert!(event.is_ok(), "should receive filesystem event");
    }

    #[cfg(unix)]
    #[test]
    fn event_rel_path_resolves_a_symlinked_root() {
        // A root reached through a symlink and an event path that is already
        // resolved must still meet. This is what FSEvents hands us on macOS for
        // any `TMPDIR`/`/tmp`/`/var` root — and for a user whose synced folder is
        // reached through a symlink on any platform. Before the canonical
        // fallback, `strip_prefix` failed for EVERY event and the host
        // attributed none of them to a file: a healthy watcher, events flowing,
        // and nothing ever syncing. Unix-only: creating the symlink needs
        // `std::os::unix`, and on Windows an unprivileged symlink isn't a given.
        let real = tempfile::tempdir().unwrap();
        let link_parent = tempfile::tempdir().unwrap();
        let link = link_parent.path().join("via-symlink");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        std::fs::write(real.path().join("a.txt"), b"x").unwrap();
        // The event path as a resolved-path backend reports it...
        let resolved = std::fs::canonicalize(real.path().join("a.txt")).unwrap();
        // ...against the root as the config spells it: through the symlink.
        assert_eq!(event_rel_path(&resolved, &link).as_deref(), Some("a.txt"));

        // The root itself is still refused through the fallback, not turned into
        // the empty key the doc comment above describes.
        let resolved_root = std::fs::canonicalize(real.path()).unwrap();
        assert_eq!(event_rel_path(&resolved_root, &link), None);

        // A genuine outsider is still rejected — the fallback widens the accepted
        // spellings of the root, never the set of paths under it.
        assert_eq!(
            event_rel_path(std::path::Path::new("/elsewhere/c.txt"), &link),
            None
        );
    }

    #[tokio::test]
    async fn every_file_created_in_the_watch_root_is_reported() {
        // The invariant the whole real-time plane rests on: a file that appears
        // in the watch dir is REPORTED. Not "an event arrives" — `watcher_detects_create`
        // above already asserts that, and it is exactly the shape that let this
        // defect hide: one file, one `is_ok()`, no identity check. A backend that
        // reports the first file and silently loses the next nine passes it.
        //
        // What it pins, and why it is not a timing test (convention 14): the
        // assertion is on the SET of paths reported, polled to a generous
        // deadline that a healthy run never pays. A lost event is not "late" —
        // no later poll ever produces it, because the only other recovery path
        // is the periodic rescan, which is minutes away by design.
        //
        // Measured cost of not having this: A file written into a seat's watch dir appeared in NO seat's
        // log at all, three of four macOS runs — diagnosed across four sessions
        // as a wedged sync host, a full channel, and FSEvents coalescing, while
        // the host sat healthy and the event had simply never been delivered.
        let dir = tempfile::tempdir().unwrap();
        let mut w = FsWatcher::start(dir.path()).unwrap();
        // Let the watch register before the first write — a genuine setup
        // precondition, not a settle-sleep standing in for an assertion.
        tokio::time::sleep(Duration::from_millis(200)).await;

        const FILES: usize = 10;
        let expected: Vec<String> = (0..FILES).map(|i| format!("file-{i}.txt")).collect();
        for name in &expected {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }

        // Generous ceiling; a healthy backend reports all ten in milliseconds and
        // the loop exits on the first poll that has them.
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        let mut raw: Vec<PathBuf> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while seen.len() < FILES && tokio::time::Instant::now() < deadline {
            match tokio::time::timeout_at(deadline, w.events.recv()).await {
                Ok(Some(FsEvent::Created(p) | FsEvent::Modified(p))) => {
                    raw.push(p.clone());
                    if let Some(rel) = event_rel_path(&p, dir.path()) {
                        seen.insert(rel);
                    }
                }
                Ok(Some(FsEvent::Removed(_))) => {}
                Ok(None) | Err(_) => break,
            }
        }

        let missing: Vec<&String> = expected.iter().filter(|n| !seen.contains(*n)).collect();
        assert!(
            missing.is_empty(),
            // `missing.len()` fills the one positional; FILES/missing/seen are captured.
            "the watcher never reported {}/{FILES} of the files created in the watch \
             root: {missing:?}\nreported: {seen:?}\n\
             A file the watcher never reports does not sync at all until the next \
             periodic rescan — minutes. (A missed DELETE is recovered at that same \
             cadence and no longer stands apart: the engine's reconcile diffs its \
             synced rows against the scan, and the daemon gained the matching sweep \
             in 2026-08-05's `sweep_deletes_ws`. Both are floor-guarded.) \
             Check which notify backend this build selected \
             (workspace `Cargo.toml`: notify's `macos_kqueue` feature takes macOS \
             OFF FSEvents and onto kqueue, which discovers new directory entries \
             only by re-scanning and drops them under concurrent writes).\n\
             watch root: {:?}\nraw event paths: {:?}",
            missing.len(),
            dir.path(),
            raw,
        );
    }

    #[test]
    fn classify_rename_resolves_by_existence() {
        use notify::EventKind;
        use notify::event::{DataChange, ModifyKind, RenameMode};

        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("new.txt");
        std::fs::write(&present, "x").unwrap();
        let gone = dir.path().join("old.txt"); // never created

        // A rename's NEW name (on disk) is a create; the OLD name (gone) is a
        // remove — NOT a modify — so the delete propagates. The platform-varying
        // RenameMode (From / To / Any / Both) is resolved purely by existence.
        assert!(matches!(
            classify_event(
                &EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                &present
            ),
            Some(FsEvent::Created(_))
        ));
        for mode in [RenameMode::From, RenameMode::Any, RenameMode::Other] {
            assert!(
                matches!(
                    classify_event(&EventKind::Modify(ModifyKind::Name(mode)), &gone),
                    Some(FsEvent::Removed(_))
                ),
                "a renamed-away (gone) path must classify as Removed for {mode:?}"
            );
        }

        // A content modify of an existing file stays a Modify; an explicit
        // Remove stays a Remove; a Create stays a Create.
        assert!(matches!(
            classify_event(
                &EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                &present
            ),
            Some(FsEvent::Modified(_))
        ));
        assert!(matches!(
            classify_event(&EventKind::Remove(notify::event::RemoveKind::Any), &gone),
            Some(FsEvent::Removed(_))
        ));
        assert!(matches!(
            classify_event(&EventKind::Create(notify::event::CreateKind::Any), &present),
            Some(FsEvent::Created(_))
        ));
    }
}

// ---------------------------------------------------------------------------
// "An unreadable folder is not an empty one" — the scan's half.
//
// The shape these pin: a directory the scan CANNOT READ contributes zero files,
// which in a bare file list is indistinguishable from a directory that is
// genuinely empty — and every consumer downstream reads "not in the scan" as
// "the user deleted it". The engine-level consequence is pinned in
// `mass_delete_floor_test`; these pin the primitive that makes the difference
// representable at all.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod unreadable_scan_tests {
    use super::*;

    /// Put `dir` beyond reach, run `body`, restore the mode whatever happened —
    /// an un-restored mode-0 directory makes the `TempDir` undeletable, so a
    /// failing assertion would leak a directory per run.
    ///
    /// Asserts the fixture's own precondition first: root ignores mode bits, so
    /// without this check the whole family would silently degrade to "the
    /// directory is readable" and pass for the wrong reason.
    #[cfg(unix)]
    fn while_unreadable<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
        use std::os::unix::fs::PermissionsExt;

        let restore = std::fs::metadata(dir).unwrap().permissions();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        let probe = std::fs::read_dir(dir).err().map(|e| e.kind());
        if !matches!(probe, Some(std::io::ErrorKind::PermissionDenied)) {
            std::fs::set_permissions(dir, restore).unwrap();
            panic!(
                "fixture precondition: read_dir on a mode-0 directory must fail with \
                 PermissionDenied (are these tests running as root?); got {probe:?}"
            );
        }

        let out = body();
        std::fs::set_permissions(dir, restore).unwrap();
        out
    }

    /// The defect itself: a synced subdirectory goes mode-0, and the scan used
    /// to return `Ok` with its files simply *gone* from the list.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_subdirectory_is_reported_not_silently_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), b"kept").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("c.txt"), b"gamma").unwrap();
        std::fs::write(sub.join("d.txt"), b"delta").unwrap();
        let ignore = crate::ignore::IgnoreMatcher::from_patterns(&[]);

        let scan = while_unreadable(&sub, || full_scan_filtered(dir.path(), &ignore).unwrap());

        let paths: Vec<&str> = scan
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["keep.txt"],
            "the readable part of the folder still scans — the blast radius is the fault, \
             not the whole folder (which is why this is a prefix and not a scan error)"
        );
        assert_eq!(
            scan.unreadable_prefixes,
            vec!["sub".to_string()],
            "the directory the scan could not enumerate must be REPORTED; before the fix \
             the PermissionDenied arm returned Ok(()) and said nothing"
        );
        assert!(
            scan.hides("sub/c.txt") && scan.hides("sub/d.txt"),
            "every row under the unreadable prefix is hidden, so their absence from the \
             file list is evidence of nothing"
        );
        assert!(
            !scan.hides("keep.txt"),
            "a readable row is NOT hidden — ordinary delete detection must still work"
        );
    }

    /// The worse half the finding's own write-up understated: an unreadable
    /// **root**. `read_dir` on a mode-0 root took the same silent-`Ok` arm, so
    /// EVERY synced row read missing at once — which is precisely the shape the
    /// mass-delete floor engages on, so the apps would render *"your folder
    /// emptied — apply N deletions"* and one click would erase the whole set.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_root_hides_everything_rather_than_reading_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
        let ignore = crate::ignore::IgnoreMatcher::from_patterns(&[]);

        let scan = while_unreadable(dir.path(), || full_scan_filtered(dir.path(), &ignore));

        let scan = scan.expect("an unreadable root is reported, not an error");
        assert!(scan.files.is_empty());
        assert_eq!(
            scan.unreadable_prefixes,
            vec![String::new()],
            "the root is the EMPTY prefix, which by construction prefixes every path"
        );
        assert!(
            scan.hides("a.txt") && scan.hides("deep/nested/file.bin"),
            "an unreadable root hides every row there could ever be"
        );
    }

    /// The boundary the fix must NOT move: a **missing** root still errors.
    /// `delete-propagation.md` § the mass-delete floor leans on that — the
    /// folder is gone (unmounted, renamed), there is nothing to reconcile
    /// against, and aborting is the honest answer. Only the unreadable case
    /// changed.
    #[test]
    fn a_missing_root_still_errors_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("never-existed");
        let ignore = crate::ignore::IgnoreMatcher::from_patterns(&[]);

        assert!(
            full_scan_filtered(&gone, &ignore).is_err(),
            "a missing root aborts the scan, as it always has"
        );
    }

    /// Segment-aware, because a raw `starts_with` would let the unreadable
    /// directory `photos` withhold the unrelated sibling row
    /// `photos-backup/a.txt` — quietly suppressing a delete that is real.
    #[test]
    fn a_prefix_matches_whole_path_segments_only() {
        let prefixes = vec!["photos".to_string()];
        assert!(is_under_unreadable_prefix("photos", &prefixes));
        assert!(is_under_unreadable_prefix("photos/a.jpg", &prefixes));
        assert!(is_under_unreadable_prefix("photos/2026/a.jpg", &prefixes));
        assert!(
            !is_under_unreadable_prefix("photos-backup/a.jpg", &prefixes),
            "a name that merely STARTS WITH the prefix is a different directory"
        );
        assert!(!is_under_unreadable_prefix("photography.txt", &prefixes));
        assert!(!is_under_unreadable_prefix("other/photos/a.jpg", &prefixes));

        // The root prefix is the one that matches everything.
        assert!(is_under_unreadable_prefix(
            "anything/at/all",
            &[String::new()]
        ));
        assert!(!is_under_unreadable_prefix("anything", &[]));
    }

    /// The watcher's own copy of the same conflation: `classify_event` chose
    /// between `Created` (harmless) and `Removed` (a delete that propagates) by
    /// `Path::exists()`, so an event arriving just after its parent directory
    /// went mode-0 classified as a removal.
    #[test]
    #[cfg(unix)]
    fn an_event_under_an_unreadable_parent_is_not_classified_as_a_removal() {
        use notify::EventKind;
        use notify::event::{ModifyKind, RenameMode};

        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let file = sub.join("still-here.txt");
        std::fs::write(&file, b"present").unwrap();

        let (removal, rename) = while_unreadable(&sub, || {
            // The conflation, demonstrated on the very input the fix is about:
            // the file is RIGHT THERE, and `exists()` says otherwise.
            assert!(
                !file.exists(),
                "fixture: Path::exists() answers false for an unreadable path — that is the \
                 bug being pinned, so if this ever stops holding the pin is vacuous"
            );
            (
                classify_event(&EventKind::Remove(notify::event::RemoveKind::Any), &file),
                classify_event(&EventKind::Modify(ModifyKind::Name(RenameMode::Any)), &file),
            )
        });

        assert!(
            matches!(removal, Some(FsEvent::Created(_))),
            "a Remove event for a path that cannot be stat'ed must NOT become a delete — \
             absence of evidence is not evidence of a delete; got {removal:?}"
        );
        assert!(
            matches!(rename, Some(FsEvent::Created(_))),
            "same rule in the rename arm; got {rename:?}"
        );

        // And the guard still lets a GENUINE removal through — the fix tightens
        // the error case without stopping deletes propagating.
        std::fs::remove_file(&file).unwrap();
        assert!(matches!(
            classify_event(&EventKind::Remove(notify::event::RemoveKind::Any), &file),
            Some(FsEvent::Removed(_))
        ));
    }
}
