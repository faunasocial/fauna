//! What a debounced local-write flush does inside — and outside — an
//! exclusive-edit write window (`file-sync.md` § Exclusive editing).
//!
//! These assert the shared [`apply_local_write`] against a recording host
//! rather than a live engine, because the load-bearing facts here are
//! *absences*: a refused flush uploads **nothing**, a twelve-file flush takes
//! **one** lease rather than twelve, and a `WatcherClosed` signal reaches for
//! the nest **not at all**. An engine-level test can show that a file did not
//! arrive; only this level can show that nothing even tried, and that the
//! counts are what the *per pass, never per file* rule requires.

use crate::always_resident::{LocalWrite, LocalWriteHost, apply_local_write};
use crate::folder_lease::LeaseWindow;
use std::cell::RefCell;

/// A host that records what [`apply_local_write`] asked it to do and hands back
/// a scripted window.
struct RecordingHost {
    window: LeaseWindow,
    uploaded: RefCell<Vec<String>>,
    deleted: RefCell<Vec<String>>,
    opened: RefCell<usize>,
    closed: RefCell<usize>,
}

impl RecordingHost {
    fn with(window: LeaseWindow) -> Self {
        Self {
            window,
            uploaded: RefCell::new(Vec::new()),
            deleted: RefCell::new(Vec::new()),
            opened: RefCell::new(0),
            closed: RefCell::new(0),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl LocalWriteHost for RecordingHost {
    fn is_ignored(&self, _rel: &str) -> bool {
        false
    }
    fn was_recent_download(&self, _rel: &str) -> bool {
        false
    }
    fn was_recent_removal(&self, _rel: &str) -> bool {
        false
    }
    async fn upload_file(&self, rel: &str) -> anyhow::Result<crate::engine::UploadOutcome> {
        self.uploaded.borrow_mut().push(rel.to_string());
        Ok(crate::engine::UploadOutcome {
            recorded: true,
            ..Default::default()
        })
    }
    async fn handle_delete(&self, rel: &str) -> anyhow::Result<()> {
        self.deleted.borrow_mut().push(rel.to_string());
        Ok(())
    }
    async fn converge(&self, _folder: &str) -> Vec<String> {
        Vec::new()
    }
    async fn open_lease_window(&self) -> LeaseWindow {
        *self.opened.borrow_mut() += 1;
        self.window.clone()
    }
    async fn close_lease_window(&self) {
        *self.closed.borrow_mut() += 1;
    }
}

/// The ordinary case: an un-governed folder takes one window (which costs no
/// nest round-trip), uploads the whole batch, and owes no release.
#[tokio::test]
async fn an_ungoverned_flush_uploads_the_whole_batch_and_releases_nothing() {
    let host = RecordingHost::with(LeaseWindow::NotGoverned);
    let applied = apply_local_write(
        &host,
        "docs",
        LocalWrite::Upload(vec!["a.txt".into(), "b.txt".into()]),
    )
    .await;

    assert_eq!(*host.uploaded.borrow(), vec!["a.txt", "b.txt"]);
    assert_eq!(applied.recorded, vec!["a.txt", "b.txt"]);
    assert_eq!(*host.opened.borrow(), 1, "one window for the whole flush");
    assert_eq!(
        *host.closed.borrow(),
        0,
        "an un-governed folder never took a lease, so it owes no release"
    );
}

/// The rule the whole design turns on: a debounced flush of N files takes
/// **one** lease, not N. A regression here would put a nest round-trip in front
/// of every write — exactly the shape § Exclusive editing's *Never* list
/// forbids.
#[tokio::test]
async fn a_governed_flush_takes_one_lease_for_the_whole_batch() {
    let host = RecordingHost::with(LeaseWindow::Held);
    let batch: Vec<String> = (0..12).map(|i| format!("f{i}.bin")).collect();
    apply_local_write(&host, "vault", LocalWrite::Upload(batch.clone())).await;

    assert_eq!(*host.uploaded.borrow(), batch);
    assert_eq!(
        (*host.opened.borrow(), *host.closed.borrow()),
        (1, 1),
        "twelve files, one acquire and one release"
    );
}

/// The sentence § Folders promises, from the refused seat's side: it uploads
/// **nothing**. What it must not do is upload part of the batch, or read the
/// refusal as a watcher fault and stop watching.
#[tokio::test]
async fn a_refused_flush_uploads_nothing_and_keeps_watching() {
    let host = RecordingHost::with(LeaseWindow::Refused);
    let applied = apply_local_write(
        &host,
        "vault",
        LocalWrite::Upload(vec!["db.sqlite".into(), "db.sqlite-wal".into()]),
    )
    .await;

    assert!(
        host.uploaded.borrow().is_empty(),
        "a refused flush must not upload a single file — the edits wait on disk"
    );
    assert!(
        applied.recorded.is_empty(),
        "nothing reached the nest, so nothing may be reported as recorded"
    );
    assert!(
        applied.continue_watching,
        "a deferral is not a watcher fault; the loop keeps watching"
    );
    assert_eq!(
        *host.closed.borrow(),
        0,
        "a refused seat never took the lease, so it must not try to release one — releasing a \
         lease it does not hold is how a seat frees another device's"
    );
}

/// The offline arm, which defers for a different reason and must stay a
/// different reason. The *action* is identical to a refusal; conflating the two
/// is what would render an offline seat's own folder as somebody else's.
#[tokio::test]
async fn an_unavailable_window_defers_the_flush_without_claiming_a_holder() {
    let host = RecordingHost::with(LeaseWindow::Unavailable);
    let applied =
        apply_local_write(&host, "vault", LocalWrite::Upload(vec!["notes.md".into()])).await;

    assert!(host.uploaded.borrow().is_empty());
    assert!(applied.continue_watching);
    assert_eq!(*host.closed.borrow(), 0);
}

/// A delete is a write to the folder, so it takes the same window: a device
/// that may not upload into a folder somebody else is editing may not record
/// deletions out of it either.
#[tokio::test]
async fn a_refused_delete_is_deferred_too() {
    let host = RecordingHost::with(LeaseWindow::Refused);
    apply_local_write(&host, "vault", LocalWrite::Delete("gone.bin".into())).await;

    assert!(
        host.deleted.borrow().is_empty(),
        "a delete recorded while another device holds the lease is a write through it"
    );
}

/// The same delete, with the lease in hand, still goes through — and gives the
/// folder back afterwards.
#[tokio::test]
async fn a_held_delete_is_recorded_and_the_lease_released() {
    let host = RecordingHost::with(LeaseWindow::Held);
    apply_local_write(&host, "vault", LocalWrite::Delete("gone.bin".into())).await;

    assert_eq!(*host.deleted.borrow(), vec!["gone.bin"]);
    assert_eq!((*host.opened.borrow(), *host.closed.borrow()), (1, 1));
}

/// `WatcherClosed` is a pure signal about the watcher, not a write — it must
/// never reach for the nest. A window opened for it would be an RPC fired on
/// the way out of a dying loop, on every engine teardown.
#[tokio::test]
async fn a_closed_watcher_never_opens_a_window() {
    let host = RecordingHost::with(LeaseWindow::Held);
    let applied = apply_local_write(&host, "vault", LocalWrite::WatcherClosed).await;

    assert!(!applied.continue_watching);
    assert_eq!(*host.opened.borrow(), 0);
    assert_eq!(*host.closed.borrow(), 0);
}
