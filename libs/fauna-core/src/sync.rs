//! Shared sync helpers used by both the client-side sync engine and
//! fauna-nest (server): path normalization and hashing, and the reserved
//! folder-name convention.

/// Normalize an already-folder-relative path to the canonical forward-slash
/// form that [`path_hash`] hashes.
///
/// `to_string_lossy` yields the OS separator, so on Windows a raw conversion
/// would produce `sub\file` and diverge from every other app's `sub/file`.
/// On Unix `MAIN_SEPARATOR` is already `/`, so this is a no-op there and
/// preserves any literal backslash in a Unix filename.
pub fn normalize_rel_path(relative: &std::path::Path) -> String {
    relative
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// BLAKE3 of a **normalized** folder-relative path — the key every file-scoped
/// sync row is addressed by (`sync_changes.path_hash`, `file_versions.path_hash`,
/// and the `path_hash` field of the `fauna.files.versions.*` wire kinds).
///
/// `normalized_rel_path` MUST already be forward-slash-separated (run it through
/// [`normalize_rel_path`] first). Cross-app agreement depends on it: the same
/// file must hash identically on every platform, so a Windows app converts
/// `\` to `/` before hashing.
///
/// Authority: `docs/goal/behavior/file-sync.md` § File Versions.
pub fn path_hash(normalized_rel_path: &str) -> [u8; 32] {
    *blake3::hash(normalized_rel_path.as_bytes()).as_bytes()
}

/// True iff `name` is a **reserved** internal folder name.
///
/// Reserved folders are minted with a `__` prefix — `__<kind>` for the
/// per-actor message-kind sets (`__index`, `__mail`, …) and
/// `__conv/<channel_hex>` for the per-channel conversation sets. They anchor
/// internal sync surfaces, are never user backup targets, and are **routing
/// constants**: user-facing projections exclude them, and they never seal
/// (`docs/goal/architecture/encryption-at-rest.md` § Carve-outs → *Seal
/// file-sync name/path metadata*: "reserved `__` names are routing constants and
/// stay").
///
/// **This is the single source of truth for the `__` convention — callers MUST
/// NOT hardcode the prefix.** It lives in `fauna-core` rather than nest-side
/// because both the nest's ~28 routing/projection decision sites *and* the
/// shared seal funnel ([`crate::label_custody::seal_set_name`]) have to agree on
/// it; a second copy is how a routing constant would end up sealed.
pub fn is_reserved_folder_name(name: &str) -> bool {
    name.starts_with("__")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the wire-visible derivation so no client can silently drift off it.
    /// A change here breaks every stored `path_hash` — it is a data migration,
    /// not a refactor.
    #[test]
    fn path_hash_is_blake3_of_the_normalized_path_bytes() {
        assert_eq!(
            hex::encode(path_hash("docs/report.txt")),
            "a740487188cd9b41ae6d544e6ba11329db7756ed4caeb48315816da454a504ea"
        );
    }

    #[test]
    fn path_hash_distinguishes_paths() {
        assert_ne!(path_hash("a/b.txt"), path_hash("a/c.txt"));
        // A separator is part of the hashed bytes: `a/b` is not `ab`.
        assert_ne!(path_hash("a/b"), path_hash("ab"));
    }

    #[test]
    fn normalize_rel_path_yields_forward_slashes_on_every_platform() {
        // Built with the native separator, so this is the cross-platform
        // assertion that a Windows `sub\file` hashes as `sub/file`.
        let native: std::path::PathBuf = ["sub", "dir", "file.txt"].iter().collect();
        assert_eq!(normalize_rel_path(&native), "sub/dir/file.txt");
        // Already-normalized input is unchanged.
        assert_eq!(
            normalize_rel_path(std::path::Path::new("sub/dir/file.txt")),
            "sub/dir/file.txt"
        );
    }

    #[test]
    fn normalized_windows_path_hashes_like_the_unix_one() {
        let native: std::path::PathBuf = ["sub", "file.txt"].iter().collect();
        assert_eq!(
            path_hash(&normalize_rel_path(&native)),
            path_hash("sub/file.txt")
        );
    }
}
