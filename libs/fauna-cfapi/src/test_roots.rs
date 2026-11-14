//! The dev fleet's sweep of the **test** sync roots a killed test run leaves
//! registered — the leftover class `fleet-janitor.md` § *The fleet cleans up
//! what it caused* hands to a mechanism, run on Windows with no session by the
//! merge-gate check's pass (`merge-gate-check-win.md` § Merge-gate check (win),
//! the cfapi test-root sweep).
//!
//! Test scaffolding, gated like `fauna_ipc::test_support`: present in debug
//! builds and wherever a consumer forwards `test-helpers`, absent from a shipped
//! artifact — no user's machine carries the test-root rule, and the product
//! never calls it.
//!
//! **What it removes.** A `Fauna!` shell registration ([`list_shell_sync_roots`])
//! whose folder lies in a Rust `tempfile` directory directly under the user's
//! temp dir ([`test_root_tempdir`]): every live test builds its root in
//! `tempfile::tempdir()`, and a product root never lives there. Each one is
//! unregistered whole ([`unregister_shell_root`]: filter, then shell), then its
//! tempdir is deleted.
//!
//! **The liveness rule.** A registration outlives the process that made it, so
//! the registry cannot tell a dead run's root from a live test's. The tests say
//! so instead: every automated test holds [`SHELL_TEST_MUTEX`] for as long as
//! it holds a shell registration, and the one human-supervised harness that
//! cannot (`hold_a_live_root_for_explorer` under `FAUNA_HOLD_SHELL=1`, which
//! holds its root for minutes) holds [`HOLD_MUTEX`] instead, so it never stalls
//! another checkout's suite. The sweep reads and touches nothing unless it holds
//! both: a root it sees under them belongs to a run that is gone.
//!
//! **The gap.** A filter-only registration (`register_sync_root` with no shell
//! half — the harness's `register_and_connect`, the tier3 modules, this crate's
//! live tests) writes no `SyncRootManager` entry, and the registry is the only
//! readable sync-root surface an unpackaged process has, so a killed filter-only
//! run is not found here. It renders nothing in Explorer; its tempdir stays
//! until the temp dir is cleaned.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use fauna_ipc::test_support::NamedMutexTestGuard;

use crate::{list_shell_sync_roots, unregister_shell_root};

/// Held by every automated test for the whole lifetime of a shell registration
/// it makes (`Local\` scope: every checkout's `cargo test` on this login).
pub const SHELL_TEST_MUTEX: &str = "FaunaCfapiShellSyncRootTest";

/// Held by the human-supervised hold harness for its hold window, in place of
/// [`SHELL_TEST_MUTEX`], which it would hold for minutes.
pub const HOLD_MUTEX: &str = "FaunaCfapiShellSyncRootHold";

/// What one [`sweep_leftover_test_roots`] did.
#[derive(Debug)]
pub enum Sweep {
    /// A live test (or the hold harness) holds this mutex: nothing was read or
    /// touched, and the next sweep tries again.
    Held(&'static str),
    /// The sweep held both mutexes and went through every `Fauna!` entry.
    Swept(SweepReport),
}

/// The entries one sweep went through.
#[derive(Debug, Default)]
pub struct SweepReport {
    /// Ids of the test roots unregistered (filter and shell).
    pub removed: Vec<String>,
    /// Test roots whose unregister failed: Id and error. The next sweep retries.
    pub failed: Vec<(String, String)>,
    /// `Fauna!` entries that are not test roots, left alone.
    pub spared: usize,
    /// Tempdirs of removed roots that could not be deleted: path and error.
    pub tempdirs_left: Vec<(PathBuf, String)>,
}

impl std::fmt::Display for Sweep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Sweep::Held(m) => write!(f, "skipped: {m} is held (a live test or the hold harness)"),
            Sweep::Swept(r) => {
                write!(
                    f,
                    "removed {} test root(s), {} failed, {} other Fauna root(s) spared",
                    r.removed.len(),
                    r.failed.len(),
                    r.spared
                )?;
                for id in &r.removed {
                    write!(f, "; removed {id}")?;
                }
                for (id, e) in &r.failed {
                    write!(f, "; FAILED {id}: {e}")?;
                }
                for (p, e) in &r.tempdirs_left {
                    write!(f, "; tempdir left {}: {e}", p.display())?;
                }
                Ok(())
            }
        }
    }
}

/// Remove every leftover test root, if no live test holds one.
///
/// Waits up to `wait` for [`SHELL_TEST_MUTEX`] (a test holds it for seconds) and
/// not at all for [`HOLD_MUTEX`] (the hold harness holds it for minutes);
/// either unobtainable is [`Sweep::Held`]. Call it from a thread that holds
/// neither: a Win32 mutex is re-entrant for its owner.
pub fn sweep_leftover_test_roots(wait: Duration) -> Result<Sweep> {
    let Some(_tests) = NamedMutexTestGuard::try_acquire(SHELL_TEST_MUTEX, wait) else {
        return Ok(Sweep::Held(SHELL_TEST_MUTEX));
    };
    let Some(_hold) = NamedMutexTestGuard::try_acquire(HOLD_MUTEX, Duration::ZERO) else {
        return Ok(Sweep::Held(HOLD_MUTEX));
    };
    let temp_dirs = user_temp_dirs();
    let mut report = SweepReport::default();
    for root in list_shell_sync_roots()? {
        let Some(tempdir) = root
            .path
            .as_deref()
            .and_then(|p| test_root_tempdir(p, &temp_dirs))
        else {
            report.spared += 1;
            continue;
        };
        match unregister_shell_root(&root) {
            Ok(()) => {
                tracing::info!(id = %root.id, "swept a leftover test sync root");
                report.removed.push(root.id);
                match std::fs::remove_dir_all(&tempdir) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => report.tempdirs_left.push((tempdir, e.to_string())),
                }
            }
            Err(e) => report.failed.push((root.id, format!("{e:#}"))),
        }
    }
    Ok(Sweep::Swept(report))
}

/// The user's temp directories a test root may sit under: the process's
/// `temp_dir()` and `%LOCALAPPDATA%\Temp` (the same place unless a launcher
/// re-pointed `TEMP`), each as given and resolved (long names, no `\\?\`).
pub fn user_temp_dirs() -> Vec<PathBuf> {
    let mut given = vec![std::env::temp_dir()];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        given.push(Path::new(&local).join("Temp"));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in given {
        let resolved = std::fs::canonicalize(&dir).ok().map(|p| {
            let s = p.to_string_lossy();
            PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
        });
        for d in std::iter::once(dir).chain(resolved) {
            if !out.iter().any(|o| norm(o) == norm(&d)) {
                out.push(d);
            }
        }
    }
    out
}

/// The tempdir to delete when `path` is a test root's folder: the Rust
/// `tempfile` directory (`.tmp` and six alphanumerics, `tempfile::tempdir()`'s
/// name) directly under one of `temp_dirs` that holds it. `None` for anything
/// else — a product root, a pytest or Python temp dir (`pytest-of-*`, `tmp*`),
/// a deeper or differently named directory. Compared case-insensitively,
/// separators either way, trailing ones ignored.
pub fn test_root_tempdir(path: &str, temp_dirs: &[PathBuf]) -> Option<PathBuf> {
    // Matched on the lower-cased form, cut from the as-given one: the two have
    // the same byte length, so the tempdir keeps its own name's case.
    let given = canon(Path::new(path));
    let p = given.to_ascii_lowercase();
    for dir in temp_dirs {
        let d = canon(dir).to_ascii_lowercase();
        if d.is_empty() || !p.starts_with(&d) || p.as_bytes().get(d.len()) != Some(&b'\\') {
            continue;
        }
        let rest = &given[d.len() + 1..];
        let first = rest.split('\\').next().unwrap_or_default();
        if is_tempfile_name(first) {
            return Some(dir.join(first));
        }
    }
    None
}

/// `tempfile::tempdir()`'s default name: `.tmp` and six ASCII alphanumerics.
fn is_tempfile_name(name: &str) -> bool {
    name.strip_prefix(".tmp")
        .is_some_and(|r| r.len() == 6 && r.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Backslash-separated, no `\\?\` prefix, no trailing separator; case kept.
fn canon(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.trim_end_matches('\\').to_string()
}

/// [`canon`], lower-cased (ASCII): the form two paths are compared in.
fn norm(p: &Path) -> String {
    canon(p).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temps() -> Vec<PathBuf> {
        vec![PathBuf::from(r"C:\Users\User\AppData\Local\Temp")]
    }

    #[test]
    fn a_root_in_a_tempfile_dir_under_temp_is_a_test_root() {
        assert_eq!(
            test_root_tempdir(
                r"C:\Users\User\AppData\Local\Temp\.tmp5NbzHV\root",
                &temps()
            ),
            Some(PathBuf::from(
                r"C:\Users\User\AppData\Local\Temp\.tmp5NbzHV"
            ))
        );
        // Case, separators and a trailing one do not matter; the tempdir itself
        // registered as the root counts.
        assert_eq!(
            test_root_tempdir(r"c:/users/user/appdata/local/temp/.tmpAb12Cd/", &temps()),
            Some(PathBuf::from(
                r"C:\Users\User\AppData\Local\Temp\.tmpAb12Cd"
            ))
        );
        assert_eq!(
            test_root_tempdir(
                r"\\?\C:\Users\User\AppData\Local\Temp\.tmpAb12Cd\a\b",
                &temps()
            ),
            Some(PathBuf::from(
                r"C:\Users\User\AppData\Local\Temp\.tmpAb12Cd"
            ))
        );
    }

    #[test]
    fn anything_else_is_not_a_test_root() {
        for p in [
            // A product root.
            r"C:\Users\User\Fauna\docs",
            // pytest's and Python's temp dirs.
            r"C:\Users\User\AppData\Local\Temp\pytest-of-User\pytest-3\root",
            r"C:\Users\User\AppData\Local\Temp\tmpab12cd34\root",
            // Not tempfile's shape: wrong length, not alphanumeric, not directly under temp.
            r"C:\Users\User\AppData\Local\Temp\.tmp12345\root",
            r"C:\Users\User\AppData\Local\Temp\.tmp1234567\root",
            r"C:\Users\User\AppData\Local\Temp\.tmp12-456\root",
            r"C:\Users\User\AppData\Local\Temp\sub\.tmpAb12Cd\root",
            // Temp itself, and a sibling sharing its prefix.
            r"C:\Users\User\AppData\Local\Temp",
            r"C:\Users\User\AppData\Local\Temp2\.tmpAb12Cd\root",
        ] {
            assert_eq!(test_root_tempdir(p, &temps()), None, "{p}");
        }
    }
}
