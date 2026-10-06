//! Shared "create + chmod 0600" prelude for this crate's two file locks
//! ([`crate::instance_lock`], [`crate::mutation_lock`]) — the same
//! kernel-arbitrated mechanism (`flock` on unix, `LockFileEx` on windows) as
//! `fauna_ipc::unix_transport::InstanceLock`, whose own doc comment names
//! this one as its twin. The mint itself is the workspace-shared
//! [`fauna_core::fs_lock`] (one implementation of the never-truncate /
//! never-delete / owner-only invariants, lifted 2026-08-15 when the MLS role
//! lock became its third consumer).

use std::fs::File;
use std::path::Path;

/// Open (creating if absent) the lock file at `path`, restricting its
/// permissions to owner-only on unix. Callers take the OS-level lock
/// themselves — non-blocking `try_lock()` or blocking `lock()`, whichever
/// their contract needs.
pub(crate) fn open_lock_file(path: &Path) -> std::io::Result<File> {
    fauna_core::fs_lock::open_lock_file(path)
}
