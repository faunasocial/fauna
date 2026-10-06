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

    pub const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
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

    /// The reason is a fixed class name, never row content — it lands in a
    /// conflict row and a log line.
    #[test]
    fn every_class_name_is_content_free() {
        for marker in [
            PermanentApplyFailure::MANIFEST_TOO_NEW,
            PermanentApplyFailure::CONTENT_UNADDRESSED,
            PermanentApplyFailure::PATH_REFUSED,
            PermanentApplyFailure::NO_KEY_MATERIAL,
        ] {
            assert!(!marker.reason.is_empty());
            assert!(
                marker.reason.is_ascii() && !marker.reason.contains('/'),
                "a class name must not look like a path: {marker:?}"
            );
        }
    }
}
