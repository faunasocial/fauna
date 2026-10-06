//! Path traversal guard for incoming sync paths.

use std::path::{Component, Path, PathBuf};

/// Returns `true` if `relative_path` is safe to join with a root directory.
///
/// Rejects paths containing `..` components, absolute paths, or paths that
/// would escape the root when joined.
pub fn is_safe_relative_path(relative_path: &str) -> bool {
    let path = Path::new(relative_path);

    // Reject absolute paths
    if path.is_absolute() {
        return false;
    }

    // Reject any component that is ParentDir (..)
    for component in path.components() {
        match component {
            Component::ParentDir => return false,
            Component::RootDir | Component::Prefix(_) => return false,
            _ => {}
        }
    }

    // Reject empty paths
    if relative_path.is_empty() {
        return false;
    }

    true
}

/// Resolve `root.join(relative_path)` and confirm the target stays **within
/// `root` after symlinks are followed** — the check the lexical
/// [`is_safe_relative_path`] cannot make. Returns the path to
/// write to when it is contained, `None` when it escapes or when `root` itself
/// cannot be canonicalized (fail-closed).
///
/// `is_safe_relative_path` rejects `..` / absolute / empty by string shape and
/// **never touches the filesystem**, so a symlinked *intermediate* directory
/// component (`root/linkdir -> /elsewhere`, then a row at `linkdir/x`) still
/// redirects the write outside `root`. This resolves the deepest ancestor of
/// the target that exists on disk — the target and its intermediate dirs
/// legitimately may not exist yet — and requires that real ancestor to stay
/// under the real `root`. Because it resolves the deepest *existing* ancestor,
/// it is safe to call **before** `create_dir_all`, so an escaping symlink
/// never even gets a directory created beyond the root.
///
/// The returned path is the plain `root.join(relative_path)` (symlinks
/// unresolved), so callers keep writing at the same path the rest of the
/// engine reads — the canonical form is used only for the containment test.
/// The final component is not itself canonicalized — the target legitimately
/// may not exist yet, and a **dangling** symlink planted at that name defeats
/// `canonicalize` anyway (it fails, so the ancestor walk falls back to `root`
/// and permits the write). That last hop is therefore contained by the
/// **caller**, not here: every host must end in a `rename`
/// (`fauna_sync_engine::atomic_write::atomic_write_file`, or an equivalent
/// `persist`), which replaces a symlink rather than following it. A host that
/// ends in a direct `std::fs::write` does **not** get that containment and must
/// canonicalize the final component itself — measured on the
/// two snapshot-restore hosts, the only ones that wrote directly until both
/// moved onto `atomic_write_file`. Note the asymmetry: a *resolvable* final
/// symlink IS caught here, because `canonicalize` follows it out of the root —
/// only the dangling case reaches the caller, and that is the case an attacker
/// plants.
pub fn resolved_target_within_root(root: &Path, relative_path: &str) -> Option<PathBuf> {
    if !is_safe_relative_path(relative_path) {
        return None;
    }
    let canonical_root = root.canonicalize().ok()?;
    let target = root.join(relative_path);
    // Deepest existing ancestor: walk up until one canonicalizes (the target
    // and its yet-uncreated intermediate dirs won't). `root` itself always
    // canonicalizes — we just checked — so the loop terminates at `root` at
    // the latest.
    let mut ancestor = target.as_path();
    let real_ancestor = loop {
        match ancestor.canonicalize() {
            Ok(p) => break p,
            Err(_) => ancestor = ancestor.parent()?,
        }
    };
    real_ancestor.starts_with(&canonical_root).then_some(target)
}

/// [`resolved_target_within_root`] for an **applier**: the same containment,
/// with its refusal classified for the catch-up anchor
/// (`docs/goal/behavior/file-sync.md` § *A failed change must not strand the
/// device*; [`crate::apply_failure`]).
///
/// `resolved_target_within_root` folds two opposite failures into one `None`:
///
/// - the row's path escapes the root — lexically, or through a symlinked
///   intermediate directory. A property of the row against this tree:
///   **permanent**, [`PermanentApplyFailure::PATH_REFUSED`];
/// - the root itself does not canonicalize — an unmounted drive, a directory
///   briefly renamed away. Nothing about the change is wrong, and the same
///   change applies once the root is back: **transient**. Marking it permanent
///   would skip, and so silently lose on this device, every change a pull
///   delivers while the root is away.
///
/// Every applier's write door resolves its target through this, so the two
/// stay apart the same way on every host.
///
/// [`PermanentApplyFailure::PATH_REFUSED`]: crate::apply_failure::PermanentApplyFailure::PATH_REFUSED
pub fn contained_apply_target(root: &Path, relative_path: &str) -> anyhow::Result<PathBuf> {
    use crate::apply_failure::{PermanentApplyFailure, permanent};
    if !is_safe_relative_path(relative_path) {
        return Err(permanent(
            PermanentApplyFailure::PATH_REFUSED,
            format!(
                "unsafe path rejected: {}",
                crate::log_redact::log_path(relative_path)
            ),
        ));
    }
    if let Some(target) = resolved_target_within_root(root, relative_path) {
        return Ok(target);
    }
    if root.canonicalize().is_err() {
        anyhow::bail!("the sync root is unavailable: {}", root.display());
    }
    Err(permanent(
        PermanentApplyFailure::PATH_REFUSED,
        format!(
            "path escapes the sync root after symlink resolution: {}",
            crate::log_redact::log_path(relative_path)
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_paths() {
        assert!(is_safe_relative_path("file.txt"));
        assert!(is_safe_relative_path("subdir/file.txt"));
        assert!(is_safe_relative_path("a/b/c/d.txt"));
        assert!(is_safe_relative_path("file with spaces.txt"));
    }

    #[test]
    fn traversal_attacks() {
        assert!(!is_safe_relative_path("../../etc/passwd"));
        assert!(!is_safe_relative_path("../secret"));
        assert!(!is_safe_relative_path("foo/../../bar"));
        assert!(!is_safe_relative_path("foo/../../../etc/shadow"));
    }

    #[test]
    fn absolute_paths() {
        assert!(!is_safe_relative_path("/etc/passwd"));
        assert!(!is_safe_relative_path("/tmp/file"));
    }

    #[test]
    fn edge_cases() {
        assert!(!is_safe_relative_path(""));
        assert!(is_safe_relative_path(".")); // current dir component is fine
        assert!(is_safe_relative_path("./file.txt"));
    }

    // `Component::RootDir | Component::Prefix(_) => return false` (line 21) is
    // unreachable on any Unix target: a string producing those components is
    // already `path.is_absolute()` on Unix and caught one line earlier, and
    // `Component::Prefix` (drive letters, UNC roots) never occurs on Unix at
    // all. Only a real Windows build exercises it — win-only-verifiable.
    #[cfg(windows)]
    #[test]
    fn windows_drive_and_unc_paths_are_rejected() {
        // Drive-absolute ("C:\Windows\System32"): caught by `is_absolute()`
        // (line 13) before the loop ever sees its `Prefix` component — still
        // the guard doing its job, just via the earlier check.
        assert!(!is_safe_relative_path("C:\\Windows\\System32"));
        // UNC path ("\\server\share\file"): also `is_absolute()` on Windows
        // (prefix + root), same early-return path as the drive-absolute case.
        assert!(!is_safe_relative_path("\\\\server\\share\\file"));
        // Drive-RELATIVE ("C:foo"): has a `Prefix` component but is NOT
        // `is_absolute()` (Windows: "relative to the current directory on
        // drive C"), so this is the one case that actually reaches the
        // loop's `Component::Prefix(_) => return false` arm.
        assert!(!is_safe_relative_path("C:foo\\bar"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_backslash_traversal_is_rejected() {
        // On Windows, backslash IS a path separator, so this parses into
        // real `ParentDir` components — the same defect class as the
        // forward-slash `traversal_attacks` cases above, just via the
        // separator Windows actually uses.
        assert!(!is_safe_relative_path("..\\..\\secret.txt"));
        assert!(!is_safe_relative_path("foo\\..\\..\\bar"));
    }

    // NOTE for whoever next touches this file on a non-Windows platform: the
    // two strings above are NOT rejected off-Windows, deliberately and
    // safely — backslash is not a separator there, so e.g.
    // `Path::new("..\\..\\secret.txt")` parses as ONE oddly-named literal
    // filename, not a `ParentDir` sequence, and the join still confines the
    // result to `watch_dir`. That is correct DIFFERENTLY per platform path
    // semantics, not inconsistently — don't "fix" it into matching the
    // Windows assertions above. A `#[cfg(not(windows))]` sibling test
    // asserting `is_safe_relative_path` returns `true` for the same strings
    // was deliberately NOT added here: this was written on a Windows build,
    // where that cfg arm compiles to nothing, so it could not be verified
    // green — the same "don't commit what you can't run" bar that kept the
    // Windows-only tests above out of a non-Windows build in the first place.

    // ── resolved_target_within_root ──────────────────
    // These need real symlinks, which the platform APIs differ on; gated to
    // unix, where the two live write doors run and the incident was measured.
    #[cfg(unix)]
    mod resolved {
        use super::super::resolved_target_within_root;
        use std::os::unix::fs::symlink;

        #[test]
        fn an_ordinary_relative_path_resolves_within_root() {
            let root = tempfile::tempdir().unwrap();
            let got = resolved_target_within_root(root.path(), "a/b/c.txt");
            assert_eq!(got, Some(root.path().join("a/b/c.txt")));
        }

        #[test]
        fn a_symlinked_intermediate_directory_escaping_root_is_refused() {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            // root/linkdir -> outside; a row at linkdir/sub/pwned.txt would land
            // outside the root once the intermediate symlink is followed.
            symlink(outside.path(), root.path().join("linkdir")).unwrap();
            assert_eq!(
                resolved_target_within_root(root.path(), "linkdir/sub/pwned.txt"),
                None,
                "the escaping intermediate symlink must be refused"
            );
        }

        #[test]
        fn a_symlinked_intermediate_staying_within_root_is_allowed() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("real")).unwrap();
            // root/link -> root/real (both inside root): legitimate, allowed.
            symlink(root.path().join("real"), root.path().join("link")).unwrap();
            let got = resolved_target_within_root(root.path(), "link/x.txt");
            assert_eq!(got, Some(root.path().join("link/x.txt")));
        }

        #[test]
        fn the_lexical_rejects_still_short_circuit() {
            let root = tempfile::tempdir().unwrap();
            assert_eq!(resolved_target_within_root(root.path(), "../escape"), None);
            assert_eq!(resolved_target_within_root(root.path(), "/abs"), None);
            assert_eq!(resolved_target_within_root(root.path(), ""), None);
        }

        #[test]
        fn a_nonexistent_root_fails_closed() {
            let root = tempfile::tempdir().unwrap();
            let missing = root.path().join("does-not-exist");
            assert_eq!(resolved_target_within_root(&missing, "a.txt"), None);
        }

        mod for_an_applier {
            use super::super::super::contained_apply_target;
            use super::symlink;
            use crate::apply_failure::{PermanentApplyFailure, permanent_reason};

            #[test]
            fn a_contained_path_resolves() {
                let root = tempfile::tempdir().unwrap();
                let got = contained_apply_target(root.path(), "a/b.txt").unwrap();
                assert_eq!(got, root.path().join("a/b.txt"));
            }

            #[test]
            fn an_escape_is_permanent() {
                let root = tempfile::tempdir().unwrap();
                let outside = tempfile::tempdir().unwrap();
                symlink(outside.path(), root.path().join("linkdir")).unwrap();
                for rel in ["linkdir/pwned.txt", "../escape.txt", "/abs.txt"] {
                    let err = contained_apply_target(root.path(), rel).unwrap_err();
                    assert_eq!(
                        permanent_reason(&err),
                        Some(PermanentApplyFailure::PATH_REFUSED.reason),
                        "{rel}: {err:#}"
                    );
                }
            }

            /// An unmounted or renamed-away root refuses the write (fail
            /// closed) but must NOT skip the change: it applies once the root
            /// is back.
            #[test]
            fn an_unavailable_root_is_transient() {
                let root = tempfile::tempdir().unwrap();
                let missing = root.path().join("unmounted");
                let err = contained_apply_target(&missing, "a.txt").unwrap_err();
                assert_eq!(permanent_reason(&err), None, "{err:#}");
            }
        }
    }
}
