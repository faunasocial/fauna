//! Progress reporting for sync transfers.

/// Events emitted during sync operations for progress tracking.
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// A file upload or download is starting.
    FileStarted {
        path: String,
        size: u64,
        chunk_count: usize,
    },
    /// A single chunk was transferred successfully.
    ChunkDone { path: String, bytes: u64 },
    /// A file completed upload or download.
    FileDone { path: String },
    /// A full sync cycle completed.
    CycleDone {
        files: usize,
        bytes: u64,
        elapsed_secs: f64,
    },
    /// What the **mass-delete floor** held on the reconcile pass that just
    /// finished — `0` when it did not engage (`file-sync.md` § Files Appear
    /// Automatically, ratified 2026-08-02).
    ///
    /// Emitted on **every** pass, zero included, and that is the whole point:
    /// the hold is *derived, never stored*, so a surface learns the folder came
    /// back only by being told the new count. Were this emitted on the held
    /// branch alone, "N deletions held" would stay painted after a remount.
    ///
    /// It rides the progress channel rather than a stored field for the same
    /// reason — the report then has exactly the hold's own lifetime, and cannot
    /// outlive the pass that produced it (which would hand the app-side
    /// *"apply N deletions"* affordance a count no live pass stands behind).
    DeletesHeld { held: u64 },
    /// How many synced rows the pass that just finished withheld from delete
    /// detection because it could not READ where they live — a
    /// subdirectory turned mode-0, a disk returning `EIO`, an `ESTALE` network
    /// or FUSE mount, an unreadable watch root.
    ///
    /// A separate report from [`Self::DeletesHeld`] because it is a separate
    /// user story. A hold says *"your folder looks empty — reconnect it, or
    /// apply the N deletions"*; this says *"part of your folder cannot be read,
    /// and nothing about it has been changed anywhere"*. Offering the apply
    /// affordance for these rows would be the bug rather than the fix: nobody
    /// has looked at those files, so there is nothing for a user to confirm.
    ///
    /// Emitted on **every** pass, zero included, for the same derived-state
    /// reason [`Self::DeletesHeld`] is: nothing is stored, so a zero is the
    /// only thing that ever retracts a report once the path reads again.
    DeletesSkippedUnreadable { skipped: u64 },
    /// The engine's live **public-audience write arm** — the owner-attested
    /// verdict it seals (or does not seal) by
    /// ([`crate::engine::SyncEngine::is_public_audience`]). Reported so the
    /// sync agent can answer "may this file carry a share link?" from the same
    /// fact the nest's serve gate agrees with, without holding an engine handle
    /// (`apps/windows.md` § Shell Extension → *The Share hand-off*, step 1).
    ///
    /// Emitted on every audience read — the resident tick's
    /// `refresh_sync_mode`, an on-demand root's populate — `false` included,
    /// since a `false` is what retracts a set the owner flipped back private.
    PublicAudience { armed: bool },
}

/// Optional progress sender. `None` means progress reporting is disabled.
pub type ProgressTx = Option<tokio::sync::mpsc::UnboundedSender<ProgressEvent>>;

/// Fire a progress event if the sender is active.
#[inline]
pub fn emit(tx: &ProgressTx, event: ProgressEvent) {
    if let Some(sender) = tx {
        let _ = sender.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_event_send_receive() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
        let ptx: ProgressTx = Some(tx);

        emit(
            &ptx,
            ProgressEvent::FileStarted {
                path: "test.txt".into(),
                size: 1024,
                chunk_count: 2,
            },
        );
        emit(
            &ptx,
            ProgressEvent::ChunkDone {
                path: "test.txt".into(),
                bytes: 512,
            },
        );
        emit(
            &ptx,
            ProgressEvent::FileDone {
                path: "test.txt".into(),
            },
        );
        emit(
            &ptx,
            ProgressEvent::CycleDone {
                files: 1,
                bytes: 1024,
                elapsed_secs: 0.5,
            },
        );

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], ProgressEvent::FileStarted { .. }));
        assert!(matches!(events[3], ProgressEvent::CycleDone { .. }));
    }
}
