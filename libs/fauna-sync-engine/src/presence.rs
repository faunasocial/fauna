//! Three-valued on-disk presence — the only question the **destructive** side
//! of sync may ask.
//!
//! [`std::path::Path::exists`] is a two-valued answer to a three-valued
//! question. It folds *"the OS told me there is nothing at this path"*
//! (`ENOENT`) together with *"the OS would not tell me"* — `EACCES` under a
//! directory whose mode or ownership changed, `EIO` on a failing disk,
//! `ESTALE`/`ENOTCONN` on a network or FUSE mount that blipped, `ELOOP`, an
//! unmounted removable volume — into the same `false`.
//!
//! On the *read* side that conflation is harmless: the read fails either way.
//! On the destructive side it is `docs/goal/principles.md` § No user-data loss
//! turned inside out. A path this process cannot `stat` gets recorded as a path
//! the user deleted, and that tombstone travels to the nest and to every other
//! device of the set — for a shared set, to every member's copy. One `chmod
//! 000` on a synced subdirectory was enough.
//!
//! So every site that decides *"record a delete"* asks here instead, and only
//! [`Presence::Absent`] — a definite `NotFound` — licenses the destructive
//! branch. [`Presence::Unknown`] is treated exactly like [`Presence::Present`]:
//! nothing is recorded, and the next pass (by which time the mount is back, or
//! the mode restored) decides on real evidence. That direction is free — a
//! delete deferred one pass costs a pass; a delete invented costs the file.
//!
//! **Deliberately not `SyncEngine::path_is_materialized`**, whose
//! `Materialization::{Present, Absent, Unknown}` makes this same split one file
//! over: that predicate maps a cloud-only placeholder to `Absent`, which is the
//! right answer to *"are the bytes on this disk"* and a catastrophic one to
//! *"did the user delete this"* — a dehydrated file is emphatically present,
//! and reading it as absent is the "free up space means erase" bug the delete
//! path already pins (`a_dehydrated_file_is_never_recorded_as_a_delete`). The
//! shape is shared; the placeholder rule is not.

use std::path::Path;

/// What the filesystem was able to say about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// `stat` succeeded — something holds this path.
    Present,
    /// `stat` said `NotFound` — definitively nothing holds this path. The only
    /// answer that licenses a destructive branch.
    Absent,
    /// `stat` failed with anything else, carrying the kind for the log. This
    /// process does not know what is at the path and must not claim it does.
    Unknown(std::io::ErrorKind),
}

impl Presence {
    /// `true` only for a definite `NotFound`.
    ///
    /// Spelled as its own predicate rather than `== Presence::Absent` at each
    /// call site so the destructive guard reads as one shared decision: the
    /// name states what the branch is allowed to conclude, and a future variant
    /// cannot quietly join the "gone" side by being added to a match.
    pub fn is_absent(&self) -> bool {
        matches!(self, Presence::Absent)
    }
}

/// Ask the filesystem about `path`, without following a final symlink.
///
/// `symlink_metadata`, not `metadata`, for two reasons. It matches what the
/// callers' enumerations already classify by — the engine's scan
/// (`DirEntry::file_type`) judges the link itself, never its target — so the re-stat cannot disagree with the walk that
/// produced the path. And a tracked name replaced by a *dangling* symlink reads
/// `Present`: something holds that name, so this pass records nothing, and the
/// next reconcile — whose scan lists no file there — routes it through the
/// ordinary per-file delete path with the mass-delete floor in front of it.
/// Conservative in the one direction that is cheap to be wrong in.
pub fn path_presence(path: &Path) -> Presence {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Presence::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Presence::Absent,
        Err(e) => Presence::Unknown(e.kind()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the type: a file that EXISTS but cannot be `stat`ed
    /// must not answer the same as one that is gone. `Path::exists()` answers
    /// `false` to both.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_path_is_unknown_not_absent() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        let file = locked.join("still-here.txt");
        std::fs::write(&file, b"present").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Precondition, asserted rather than assumed: root ignores mode bits,
        // so without this the fixture would silently degrade to "the file is
        // readable" and the test would pass for the wrong reason.
        let probe = std::fs::symlink_metadata(&file);
        let precondition_holds =
            matches!(&probe, Err(e) if e.kind() != std::io::ErrorKind::NotFound);
        if !precondition_holds {
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            panic!(
                "fixture precondition: stat under a mode-0 parent must fail with something \
                 other than NotFound (are these tests running as root?); got {probe:?}"
            );
        }

        let presence = path_presence(&file);
        // The comparison that motivates the type at all — read while the
        // directory is STILL locked, since that is the only moment the
        // conflation exists.
        let exists_says = file.exists();
        // Restore before asserting, or a failure leaves an unremovable tempdir.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            matches!(presence, Presence::Unknown(_)),
            "a file under a mode-0 parent is unknown, not absent; got {presence:?}"
        );
        assert!(
            !presence.is_absent(),
            "is_absent() is the destructive guard — an unreadable path must never pass it"
        );
        assert!(
            !exists_says,
            "Path::exists() answers false for this very file — the conflation this module \
             exists to replace, and the reason a chmod could be read as a delete"
        );
    }

    #[test]
    fn a_missing_path_is_absent_and_a_real_one_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("here.txt");
        std::fs::write(&file, b"body").unwrap();

        assert_eq!(path_presence(&file), Presence::Present);
        assert!(!path_presence(&file).is_absent());

        std::fs::remove_file(&file).unwrap();
        assert_eq!(path_presence(&file), Presence::Absent);
        assert!(
            path_presence(&file).is_absent(),
            "a genuine removal must still license the delete — the guard tightens the \
             error case, it does not stop deletes propagating"
        );
    }
}
