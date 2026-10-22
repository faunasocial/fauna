//! The two failure classes a catch-up anchor turns on.
//!
//! `docs/goal/behavior/file-sync.md` § *A failed change must not strand the
//! device* rules that the classes do **opposite** things, so an applier that
//! cannot tell them apart is wrong whichever way it guesses:
//!
//! - guess **transient** for a permanent failure and the anchor freezes below
//!   the bad row forever — the device pulls nothing after it (measured live
//!   2026-07-31: one change from 2026-07-18 blocked every later one, on three
//!   fresh device ids);
//! - guess **permanent** for a transient one and the anchor advances past a row
//!   this device never read — the change is lost here for good, and every later
//!   local edit stamps `derived_through` at an anchor that claims it
//!   (`conflicts.md` § the causal watermark), so peers fast-forward over their
//!   own unread work.
//!
//! **Transient is the default, and permanent is opted into site by site.** The
//! permanent set is small, closed and enumerated by the goal doc; every other
//! failure — a socket, a disk, a nest that is down — is transient by nature.
//! Getting the default backwards would make every unclassified error a silent
//! data loss, where the default this way costs at worst a retry.
//!
//! A site that KNOWS its failure is permanent for that one change returns
//! [`PermanentApplyFailure`]; the applier asks [`permanent_reason`]. The marker
//! rides the error's `source()` chain, so ordinary `.context(…)` wrapping on
//! the way up neither hides nor fakes it.

use std::fmt;

/// A failure that will recur identically on every later pull of this change,
/// on this device, whatever arrives in the meantime.
///
/// ⚠ **Only for a cause nothing can change.** A key that has not synced *yet*,
/// a generation a member may still be granted, a nest that is down — none of
/// those belong here: marking one permanent skips a change the device could
/// have applied, and the anchor moves past it for good. When in doubt, leave
/// it unmarked; a needless retry is recoverable and a needless skip is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermanentApplyFailure {
    /// A stable, content-free class name for the log and the recorded
    /// `catchup_failed` conflict row. Never a path or a label (the S7 log
    /// scrub).
    pub reason: &'static str,
}

impl PermanentApplyFailure {
    /// A manifest stamped for a reader version this binary does not implement.
    /// No key changes that — only a newer binary, which is a different device
    /// state, not a later pull.
    pub const MANIFEST_TOO_NEW: Self = Self {
        reason: "manifest requires a newer reader",
    };

    /// A sealed-chunk manifest in a shape no writer emits: one carrying its
    /// plaintext hashes (`ChunkManifest::check_hash_shape`). A property of the
    /// stored bytes; refetching them reproduces it.
    pub const MANIFEST_SHAPE_REFUSED: Self = Self {
        reason: "manifest shape refused",
    };

    /// A manifest whose size table contradicts its chunk list
    /// (`ChunkManifest::check_sizes`): a `total_size` that is not the sum of
    /// the chunk sizes, a count mismatch, or an oversize chunk. A property of
    /// the stored bytes; refetching them reproduces it.
    pub const MANIFEST_SIZE_REFUSED: Self = Self {
        reason: "manifest sizes refused",
    };

    /// The reassembled bytes do not address the hash the change recorded. The
    /// bytes are wrong at rest; refetching them reproduces the same mismatch.
    pub const CONTENT_UNADDRESSED: Self = Self {
        reason: "content does not address its recorded hash",
    };

    /// The change names a path outside the sync root (lexically, or through a
    /// symlinked intermediate directory). A property of the row, permanent by
    /// construction.
    pub const PATH_REFUSED: Self = Self {
        reason: "path escapes the sync root",
    };

    /// A sealed manifest reaching a reader that holds **no** key material of
    /// any kind — no `BackupKey`, no content keys. Distinct from "this holder
    /// lacks generation *v*", which stays transient: a generation arrives over
    /// the wire, whereas a reader with no owner key never acquires one by
    /// syncing.
    pub const NO_KEY_MATERIAL: Self = Self {
        reason: "sealed content and this reader holds no key material",
    };

    /// An unstamped record signed as a **retired identity** of this account
    /// that opens under none of the roots that identity may open —
    /// `mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(c): sealed under a later root (the current one, or a
    /// predecessor nearer than the signer), or admitted on a host that holds
    /// no root of that identity at all. The bound is a property of the
    /// verdict and this host's chain, not of anything a pull delivers: a hold
    /// here would stall every later row of the set behind one this host can
    /// never open, so the ruling makes it a noted skip.
    pub const SIGNER_BOUND: Self = Self {
        reason: "sealed under a root its signer may not open",
    };

    /// A record carrying a `content_key_version` reaching an **owner-only**
    /// reader that holds no key of that generation —
    /// `writer-signed-change-records.md` ruling (10)(c): a stamped record
    /// opens under its generation or not at all, never under an owner root.
    /// An owner-only reader's generations are the retired ones its own
    /// custody carries — the same custody row that made the set owner-only —
    /// so no pull delivers the missing one; a hold here would let one stamp
    /// (a retired seed's, a lying nest's) stall every later row of the set.
    pub const STAMP_BOUND: Self = Self {
        reason: "stamped with a generation this owner-only reader does not hold",
    };

    /// A plaintext manifest (`stored_hashes = None`) reaching a **sealed-only**
    /// reader (`FileDownloadKeys::sealed_only`) — an owner-only reader whose
    /// manifest address came from a party's unsigned word, where only the seal
    /// authenticates the bytes. A property of the stored bytes; refetching
    /// them reproduces it.
    pub const PLAINTEXT_REFUSED: Self = Self {
        reason: "plaintext manifest refused by a sealed-only reader",
    };

    /// A snapshot entry that no judged change row of the snapshot's set names
    /// — exactly its manifest under the hash of the path it restores to
    /// (`backup-restore.md` § Security Properties → Integrity). The entry's
    /// address is the home nest's unsigned word, and a seal proves only that
    /// the owner sealed the bytes under the set's root, never that they are
    /// this path's at this version — so the row is the licence, sealed or
    /// plaintext, and without one the nest built the manifest, listed another
    /// path's or version's, or serves a history that does not name it. A
    /// property of the listing against the history the nest serves; reading
    /// it again reproduces it (a history read that *fails* stays transient).
    pub const ENTRY_UNLICENSED: Self = Self {
        reason: "snapshot entry named by no judged change row of its set",
    };

    /// A peer-served file the transfer gate's per-operation cap kept out of
    /// the pull (`p2p.md` § Cross-user shared-set transfer, the metering
    /// paragraph). The cap is a rule a rule-setter may later lift, so this
    /// is the one marker whose cause CAN change — but the ruling makes a
    /// capped file a skip that lets the cursor pass, never a hold: a hold
    /// would re-list the same page on every pass and stall every file
    /// behind it while the cap stands, and the skipped file still reaches
    /// the device by the nest path.
    pub const PER_OPERATION_CAP: Self = Self {
        reason: "file kept out by the per-operation transfer cap",
    };

    /// A row no key this reader holds opens, held past [`KEY_PENDING_HOLD`]
    /// — the KEY-PENDING class (`file-sync.md` § *A failed change must not
    /// strand the device*, TRANSIENT IS THE DEFAULT). The one marker no open
    /// site raises: the failure is transient ([`KeyPendingOpen`]) until the
    /// applier's bound runs out, and then the APPLIER marks it. Passing it
    /// drops nothing, because a content-key gain re-owes the passed-heads
    /// re-judge, which opens and folds it then
    /// (`writer-signed-change-records.md` ruling (8)(f)).
    pub const KEY_PENDING: Self = Self {
        reason: "no key this reader holds opens it yet",
    };

    pub const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

/// How long a KEY-PENDING row may hold the catch-up anchor, in wall time from
/// the first such failure of the same lowest held seq, before the applier
/// passes it as [`PermanentApplyFailure::KEY_PENDING`]. A constant, never a
/// setting: passing loses nothing (the re-judge re-folds the row on a key
/// gain), so the bound only trades a short wait for an honest key that is
/// still syncing against how long a hostile row can stall a device.
pub const KEY_PENDING_HOLD: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// A **transient** crypto-open failure of the KEY-PENDING class: the row's
/// bytes were all fetched, and no root this reader holds opens them — a sealed
/// manifest that opens under no candidate root, a stamped generation this
/// reader lacks, or a chunk whose tag fails under every candidate root. It
/// says nothing about the writer (a same-version key can still arrive by a
/// custody merge), so it holds the anchor like any transient failure, for at
/// most [`KEY_PENDING_HOLD`]. A fetch or transport failure is never this.
///
/// Unlike [`PermanentApplyFailure`], [`permanent_reason`] never reports it;
/// [`is_key_pending`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPendingOpen;

impl fmt::Display for KeyPendingOpen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no key this reader holds opens it (key pending)")
    }
}

impl std::error::Error for KeyPendingOpen {}

/// Raise `message` as a KEY-PENDING open failure — the marker underneath, the
/// message on top, as [`permanent`] does.
pub fn key_pending(message: impl fmt::Display + Send + Sync + 'static) -> anyhow::Error {
    anyhow::Error::new(KeyPendingOpen).context(message)
}

/// Whether `err` was raised as a KEY-PENDING open failure, either attachment
/// style (see [`permanent_reason`]). A permanent marker on the same chain
/// wins at the applier, which asks [`permanent_reason`] first.
pub fn is_key_pending(err: &anyhow::Error) -> bool {
    err.downcast_ref::<KeyPendingOpen>().is_some()
        || err
            .chain()
            .any(|e| e.downcast_ref::<KeyPendingOpen>().is_some())
}

impl fmt::Display for PermanentApplyFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "permanently un-appliable: {}", self.reason)
    }
}

impl std::error::Error for PermanentApplyFailure {}

/// Raise `message` as a failure marked permanent.
///
/// The marker rides **underneath** the message in the `source()` chain, so the
/// error still reads as itself: `{err}` and `{err:#}` are exactly what they
/// were before the site was classified, and every log line and recorded
/// conflict row keeps the text that explains the failure. Attaching the marker
/// as `.context(…)` instead would put "permanently un-appliable: …" on top and
/// bury the reason — classification is a decision the applier reads, never a
/// replacement for what a human reads.
pub fn permanent(
    marker: PermanentApplyFailure,
    message: impl fmt::Display + Send + Sync + 'static,
) -> anyhow::Error {
    anyhow::Error::new(marker).context(message)
}

/// The class name when `err` was raised at a site that marked it permanent,
/// else `None` — i.e. transient.
///
/// **Both attachment styles are found, deliberately.** A site with nothing
/// else to say raises the marker itself (`Err(PermanentApplyFailure::X.into())`,
/// where it lands in the `source()` chain); a site keeping an existing error's
/// message attaches it as context (`e.context(PermanentApplyFailure::X)`,
/// where it lands in anyhow's context chain instead). Only one of the two
/// lookups sees each, and a marker the applier cannot see is a marker that
/// does nothing — so this asks both, and the later `.context(…)` layers every
/// call site stacks on the way up hide neither.
pub fn permanent_reason(err: &anyhow::Error) -> Option<&'static str> {
    if let Some(marker) = err.downcast_ref::<PermanentApplyFailure>() {
        return Some(marker.reason);
    }
    err.chain()
        .find_map(|e| e.downcast_ref::<PermanentApplyFailure>())
        .map(|p| p.reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn an_unmarked_failure_is_transient() {
        let err = anyhow::anyhow!("connection reset").context("fetching chunks");
        assert_eq!(permanent_reason(&err), None);
    }

    /// The marker must survive the `.context(…)` layers every call site adds
    /// on the way up — otherwise a site marks its failure permanent and the
    /// applier, three frames later, freezes on it anyway.
    #[test]
    fn the_marker_survives_the_context_layers_stacked_over_it() {
        let err = anyhow::Error::new(PermanentApplyFailure::MANIFEST_TOO_NEW)
            .context("decoding manifest 9f3a…")
            .context("downloading photos/a.jpg")
            .context("catch-up pass");
        assert_eq!(
            permanent_reason(&err),
            Some("manifest requires a newer reader")
        );
    }

    /// …and through a `?` that re-raises it from a nested `anyhow::Result`,
    /// which is how every real site reaches the applier.
    #[test]
    fn the_marker_survives_the_question_mark_chain() {
        fn inner() -> anyhow::Result<()> {
            Err(PermanentApplyFailure::CONTENT_UNADDRESSED.into())
        }
        fn outer() -> anyhow::Result<()> {
            inner().context("reassembling")?;
            Ok(())
        }
        let err = outer().unwrap_err().context("applying change 71");
        assert_eq!(
            permanent_reason(&err),
            Some("content does not address its recorded hash")
        );
    }

    /// The other attachment style: a site that keeps the underlying error's
    /// message and only adds the classification. ⚠ This one rides anyhow's
    /// *context* chain, not `source()`, so a classifier that walked only
    /// `chain()` would miss it — and a missed marker is a freeze.
    #[test]
    fn the_marker_is_found_when_attached_as_context() {
        let err = anyhow::anyhow!("manifest min_reader 4 > this build's 3")
            .context(PermanentApplyFailure::MANIFEST_TOO_NEW)
            .context("decoding manifest")
            .context("catch-up pass");
        assert_eq!(
            permanent_reason(&err),
            Some("manifest requires a newer reader")
        );
        assert!(
            format!("{err:#}").contains("min_reader 4"),
            "marking a failure must not eat the message that explains it"
        );
    }

    /// ⚠ Classifying a site must not change what it SAYS. `permanent` puts the
    /// marker under the message, not over it — the first attempt attached it
    /// as context and "permanently un-appliable: …" replaced every
    /// fail-closed message in the logs (caught by
    /// `file_download::tests::sealed_file_without_key_fails_closed`).
    #[test]
    fn marking_a_site_permanent_leaves_its_message_on_top() {
        let err = permanent(
            PermanentApplyFailure::NO_KEY_MATERIAL,
            "sealed manifest but this reader holds no BackupKey",
        );
        assert_eq!(
            err.to_string(),
            "sealed manifest but this reader holds no BackupKey"
        );
        assert_eq!(
            permanent_reason(&err),
            Some("sealed content and this reader holds no key material")
        );
    }

    /// A KEY-PENDING open is transient to every classifier that asks only for
    /// a permanent reason, and found through the context layers stacked on it.
    #[test]
    fn a_key_pending_open_is_transient_and_found_under_context() {
        let err = key_pending("chunk 0 failed under every candidate root")
            .context("downloading photos/a.jpg")
            .context("catch-up pass");
        assert_eq!(permanent_reason(&err), None);
        assert!(is_key_pending(&err));
        assert!(format!("{err:#}").contains("every candidate root"));
        assert!(!is_key_pending(&anyhow::anyhow!("connection reset")));
    }

    /// The reason is a fixed class name, never row content — it lands in a
    /// conflict row and a log line.
    #[test]
    fn every_class_name_is_content_free() {
        for marker in [
            PermanentApplyFailure::MANIFEST_TOO_NEW,
            PermanentApplyFailure::CONTENT_UNADDRESSED,
            PermanentApplyFailure::PATH_REFUSED,
            PermanentApplyFailure::NO_KEY_MATERIAL,
            PermanentApplyFailure::KEY_PENDING,
        ] {
            assert!(!marker.reason.is_empty());
            assert!(
                marker.reason.is_ascii() && !marker.reason.contains('/'),
                "a class name must not look like a path: {marker:?}"
            );
        }
    }
}
