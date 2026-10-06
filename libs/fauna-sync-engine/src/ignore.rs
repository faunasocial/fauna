//! `.faunaignore` file parser — gitignore-compatible glob patterns.

use std::path::Path;

use anyhow::Result;
use globset::{Glob, GlobSet, GlobSetBuilder};

#[cfg(test)]
mod load_error_classification_tests {
    use super::*;

    /// A Windows cloud-files (cfapi) error on the `.faunaignore` read means the name
    /// is not materialized locally and the filter could not ask a provider — and since
    /// dotfiles never enter a folder (`watcher::scan_recursive_filtered` skips them,
    /// so `.faunaignore` is never uploaded and can never come back as a placeholder),
    /// that is equivalent to "no ignore file": load must yield the default matcher,
    /// not an error. The error path bricked every engine build on an orphaned
    /// placeholder root dir (live incident 2026-07-17: folder permanently inert,
    /// Explorer showed "The cloud file provider exited unexpectedly").
    #[test]
    fn cloud_files_error_classifies_as_no_ignore_file() {
        // ERROR_CLOUD_FILE_* family occupies 362..=399 in winerror.h
        // (362 = ERROR_CLOUD_FILE_METADATA_CORRUPT … 396 = ERROR_CLOUD_FILE_US_MESSAGE_TIMEOUT).
        for code in [362, 380, 395, 399] {
            let e = std::io::Error::from_raw_os_error(code);
            assert!(
                io_error_means_no_ignore_file(&e),
                "cloud-files os error {code} must classify as \"no ignore file\""
            );
        }
    }

    #[test]
    fn not_found_classifies_as_no_ignore_file() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(io_error_means_no_ignore_file(&e));
    }

    #[test]
    fn ordinary_io_errors_still_fail_the_load() {
        // ERROR_ACCESS_DENIED / ERROR_SHARING_VIOLATION and friends must keep
        // failing the load: they say nothing about the file's absence, and
        // silently defaulting would upload ignored files.
        for code in [5, 32, 361, 400] {
            let e = std::io::Error::from_raw_os_error(code);
            assert!(
                !io_error_means_no_ignore_file(&e),
                "os error {code} must remain a load error"
            );
        }
    }

    #[test]
    fn load_returns_default_matcher_when_file_absent() {
        let dir = std::env::temp_dir().join(format!(
            "fauna-ignore-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t").len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let m = IgnoreMatcher::load(&dir).expect("absent .faunaignore is not an error");
        assert!(!m.is_ignored("anything.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod default_ignore_patterns_tests {
    use super::*;

    fn load_in_temp_dir(faunaignore_body: Option<&str>) -> IgnoreMatcher {
        let dir = std::env::temp_dir().join(format!(
            "fauna-default-ignore-test-{}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t").len(),
            faunaignore_body.map(|s| s.len()).unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(body) = faunaignore_body {
            std::fs::write(dir.join(".faunaignore"), body).unwrap();
        }
        let m = IgnoreMatcher::load(&dir).expect("load must not error");
        std::fs::remove_dir_all(&dir).ok();
        m
    }

    #[test]
    fn office_lock_files_are_ignored_by_default() {
        let m = load_in_temp_dir(None);
        assert!(
            m.is_ignored("~$report.docx"),
            "Office owner-lock files must be filtered by default"
        );
    }

    #[test]
    fn atomic_save_tmp_files_are_ignored_by_default() {
        let m = load_in_temp_dir(None);
        assert!(
            m.is_ignored("report.tmp"),
            "atomic-save-intermediate .tmp files must be filtered by default"
        );
    }

    #[test]
    fn thumbs_db_is_ignored_by_default() {
        let m = load_in_temp_dir(None);
        assert!(m.is_ignored("Thumbs.db"));
    }

    #[test]
    fn desktop_ini_is_ignored_by_default() {
        let m = load_in_temp_dir(None);
        assert!(m.is_ignored("desktop.ini"));
    }

    #[test]
    fn ordinary_files_are_not_ignored_by_default() {
        let m = load_in_temp_dir(None);
        assert!(!m.is_ignored("report.docx"));
        assert!(!m.is_ignored("src/main.rs"));
    }

    #[test]
    fn built_ins_match_at_any_depth() {
        // gitignore semantics: a pattern with no `/` matches in every
        // directory, not just the root — Office writes its lock files next to
        // the document, wherever that is.
        let m = load_in_temp_dir(None);
        assert!(m.is_ignored("docs/~$report.docx"));
        assert!(m.is_ignored("a/b/Thumbs.db"));
        assert!(m.is_ignored("sub/desktop.ini"));
        assert!(m.is_ignored("sub/dir/report.tmp"));
        assert!(!m.is_ignored("docs/report.docx"));
    }

    #[test]
    fn hidden_component_predicate_matches_scanner_semantics() {
        assert!(has_hidden_component(".DS_Store"));
        assert!(has_hidden_component(".faunaignore"));
        assert!(has_hidden_component("docs/.DS_Store"));
        assert!(has_hidden_component(".git/config"));
        assert!(!has_hidden_component("docs/report.docx"));
        assert!(!has_hidden_component("not.hidden/file.txt"));
        assert!(!has_hidden_component(""));
    }

    #[test]
    fn built_in_defaults_still_apply_alongside_a_user_faunaignore() {
        // A user's own .faunaignore is additive, never a replacement for the
        // built-in class — a user who never thought to list Thumbs.db still
        // gets it filtered.
        let m = load_in_temp_dir(Some("*.custom-user-pattern\n"));
        assert!(m.is_ignored("Thumbs.db"));
        assert!(m.is_ignored("~$report.docx"));
        assert!(m.is_ignored("notes.custom-user-pattern"));
        assert!(!m.is_ignored("report.docx"));
    }
}

/// Does any path component of this (forward-slash, set-relative) rel start with
/// a dot? The scanner's categorical dotfile exclusion
/// (`watcher::scan_recursive_filtered` skips hidden files/dirs at every level —
/// what keeps `.DS_Store` and `.faunaignore` itself out of a folder),
/// expressed as a pure rel predicate so write paths that never run a scan (the
/// Apple File Provider callbacks; the watcher event filter) apply the same rule
/// at any depth.
pub fn has_hidden_component(rel: &str) -> bool {
    rel.split('/').any(|c| c.starts_with('.'))
}

/// Matches file paths against a set of ignore patterns.
#[derive(Clone)]
pub struct IgnoreMatcher {
    set: GlobSet,
    config_exclude: GlobSet,
    include_prefixes: Vec<String>,
    /// The raw strings [`Self::config_exclude`] was last compiled from —
    /// `config_exclude` itself is a compiled `GlobSet` with no way to recover
    /// the patterns that built it, and [`crate::engine::install_selective_sync`]
    /// needs the actual list back (not just whether it is empty) to reapply
    /// this dimension unchanged while replacing the other.
    config_exclude_paths: Vec<String>,
}

impl Default for IgnoreMatcher {
    fn default() -> Self {
        Self {
            set: GlobSet::empty(),
            config_exclude: GlobSet::empty(),
            include_prefixes: Vec::new(),
            config_exclude_paths: Vec::new(),
        }
    }
}

/// Does this IO error mean "there is no `.faunaignore` to load"?
///
/// Two families qualify:
/// - `NotFound` — the plain missing-file case.
/// - The Windows cloud-files (cfapi) errors, `ERROR_CLOUD_FILE_*` = raw os error
///   362..=399 (winerror.h). Inside an on-demand placeholder directory a lookup of a
///   name with no on-disk entry must be answered by the sync provider; when the
///   provider is not connected (engine build runs *before* the cfapi root connects,
///   or the root is orphaned) the open fails with one of these instead of
///   `NotFound`. Since dotfiles never enter a folder — the shared scan
///   (`watcher::scan_recursive_filtered`) skips them, so `.faunaignore` is never
///   uploaded and can never come back as a placeholder — a `.faunaignore` that
///   exists is always an ordinary local file, readable without any provider. So a
///   cloud-files error on this read can only mean the file is absent. Treating it
///   as an error instead bricked every engine build on an orphaned placeholder
///   root dir (live incident 2026-07-17: folders permanently inert, Explorer
///   showed "The cloud file provider exited unexpectedly").
///
/// Everything else (access denied, sharing violation, …) says nothing about the
/// file's absence and must keep failing the load — silently defaulting there would
/// upload ignored files.
fn io_error_means_no_ignore_file(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::NotFound {
        return true;
    }
    matches!(e.raw_os_error(), Some(362..=399))
}

/// Built-in default ignore patterns (`file-sync.md` § Built-in default ignores).
/// Application/OS temp litter that should never sync, hard-coded per the
/// no-operator invariant — never a config surface. Dotfiles are already excluded
/// categorically by the scanner (`watcher::scan_recursive_filtered`), so they are
/// not listed here; `.faunaignore` remains the per-folder user override layered
/// on top of this set, never a replacement for it.
const DEFAULT_IGNORE_PATTERNS: &[&str] = &[
    "~$*",         // Office owner-lock files
    "*.tmp",       // atomic-save intermediates
    "Thumbs.db",   // Windows folder thumbnail cache
    "desktop.ini", // Windows folder display settings
];

impl IgnoreMatcher {
    /// Load patterns from `.faunaignore` in the given directory, always unioned
    /// with [`DEFAULT_IGNORE_PATTERNS`] — the built-ins apply whether or not a
    /// `.faunaignore` exists, and whether or not it names them itself.
    /// A missing file (including the Windows cloud-files error family, which is
    /// "does not exist" wearing a placeholder-directory costume — see
    /// [`io_error_means_no_ignore_file`]) contributes no user patterns.
    pub fn load(watch_dir: &Path) -> Result<Self> {
        let ignore_path = watch_dir.join(".faunaignore");
        let user_patterns: Vec<String> = match std::fs::read_to_string(&ignore_path) {
            Ok(content) => content
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect(),
            Err(e) if io_error_means_no_ignore_file(&e) => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let mut patterns: Vec<&str> = DEFAULT_IGNORE_PATTERNS.to_vec();
        patterns.extend(user_patterns.iter().map(String::as_str));
        Ok(Self::from_patterns(&patterns))
    }

    /// Build a matcher from a list of glob patterns.
    pub fn from_patterns(patterns: &[&str]) -> Self {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            let pat = pattern.trim_end_matches('/');
            // Add the pattern as-is for file matches
            if let Ok(glob) = Glob::new(pat) {
                builder.add(glob);
            }
            // Also add with /** suffix for directory matches
            if let Ok(glob) = Glob::new(&format!("{pat}/**")) {
                builder.add(glob);
            }
            // gitignore semantics: a pattern with no `/` matches at ANY depth,
            // not just the root — `Thumbs.db` must catch `sub/Thumbs.db` too.
            // globset alone anchors the whole path, so `~$*` / `Thumbs.db` /
            // `desktop.ini` silently matched only top-level litter.
            if !pat.contains('/') {
                if let Ok(glob) = Glob::new(&format!("**/{pat}")) {
                    builder.add(glob);
                }
                if let Ok(glob) = Glob::new(&format!("**/{pat}/**")) {
                    builder.add(glob);
                }
            }
        }
        Self {
            set: builder.build().unwrap_or_else(|_| GlobSet::empty()),
            config_exclude: GlobSet::empty(),
            include_prefixes: Vec::new(),
            config_exclude_paths: Vec::new(),
        }
    }

    /// Create a matcher from config-level include/exclude path lists only (no .faunaignore).
    pub fn from_config_patterns(include_paths: &[String], exclude_paths: &[String]) -> Self {
        let mut m = Self::default();
        m.apply_config_patterns(include_paths, exclude_paths);
        m
    }

    /// Merge config-level include/exclude paths into this matcher.
    pub fn with_config_patterns(
        mut self,
        include_paths: &[String],
        exclude_paths: &[String],
    ) -> Self {
        self.apply_config_patterns(include_paths, exclude_paths);
        self
    }

    /// Merge config-level include/exclude paths into this matcher **in place**.
    ///
    /// Both config fields are rebuilt wholesale rather than accumulated, so a
    /// re-apply is idempotent — which is what lets a *running* engine re-install
    /// the folder row's lists on every posture refresh
    /// ([`crate::engine::SyncEngine::refresh_sync_mode`]) without reloading
    /// `.faunaignore`: the `set` field (built-ins + the on-disk file) is not
    /// touched here.
    pub(crate) fn apply_config_patterns(
        &mut self,
        include_paths: &[String],
        exclude_paths: &[String],
    ) {
        let mut builder = GlobSetBuilder::new();
        for p in exclude_paths {
            let pat = p.trim_end_matches('/');
            if let Ok(glob) = Glob::new(pat) {
                builder.add(glob);
            }
            if let Ok(glob) = Glob::new(&format!("{pat}/**")) {
                builder.add(glob);
            }
        }
        if let Ok(set) = builder.build() {
            self.config_exclude = set;
            self.config_exclude_paths = exclude_paths.to_vec();
        }
        self.include_prefixes = include_paths
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| p.trim_end_matches('/').to_string())
            .collect();
    }

    /// The raw `include_paths` this matcher was last configured with — the
    /// [`Self::include_prefixes`] field verbatim, exposed so a caller can
    /// reapply this dimension unchanged while replacing the other
    /// ([`crate::engine::install_selective_sync`]'s per-field substitution).
    pub fn include_paths(&self) -> &[String] {
        &self.include_prefixes
    }

    /// The raw `exclude_paths` this matcher was last configured with — see
    /// [`Self::include_paths`].
    pub fn exclude_paths(&self) -> &[String] {
        &self.config_exclude_paths
    }

    /// Returns `true` if the given relative path should be ignored.
    pub fn is_ignored(&self, relative_path: &str) -> bool {
        if self.set.is_match(relative_path) {
            return true;
        }
        if self.config_exclude.is_match(relative_path) {
            return true;
        }
        if !self.include_prefixes.is_empty() {
            let matches_include = self.include_prefixes.iter().any(|prefix| {
                relative_path == prefix || relative_path.starts_with(&format!("{}/", prefix))
            });
            if !matches_include {
                return true;
            }
        }
        false
    }

    /// Returns `true` if this matcher has no patterns (accepts everything).
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.config_exclude.is_empty() && self.include_prefixes.is_empty()
    }
}

#[cfg(test)]
mod pattern_tests {
    use super::*;

    #[test]
    fn ignore_matcher_basic_patterns() {
        let matcher = IgnoreMatcher::from_patterns(&["*.log", "build", "temp/**"]);
        assert!(matcher.is_ignored("debug.log"));
        assert!(matcher.is_ignored("build"));
        assert!(matcher.is_ignored("build/output.o"));
        assert!(matcher.is_ignored("temp/cache/file.dat"));
        assert!(!matcher.is_ignored("readme.md"));
        assert!(!matcher.is_ignored("src/main.rs"));
    }

    #[test]
    fn ignore_matcher_load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".faunaignore"),
            "# comment\n*.log\nbuild/\n",
        )
        .unwrap();
        let matcher = IgnoreMatcher::load(dir.path()).unwrap();
        assert!(!matcher.is_empty());
        assert!(matcher.is_ignored("app.log"));
        assert!(matcher.is_ignored("build/output.o"));
        assert!(!matcher.is_ignored("src/main.rs"));
    }

    #[test]
    fn selective_sync_include_exclude() {
        let matcher = IgnoreMatcher::from_config_patterns(
            &["Documents".to_string(), "Photos".to_string()],
            &["Documents/drafts".to_string()],
        );

        assert!(!matcher.is_ignored("Documents/report.pdf"));
        assert!(!matcher.is_ignored("Photos/cat.jpg"));
        assert!(matcher.is_ignored("Documents/drafts/wip.txt"));
        assert!(matcher.is_ignored("Downloads/file.zip"));
        assert!(matcher.is_ignored("Music/song.mp3"));
        assert!(matcher.is_ignored("random.txt"));
    }

    #[test]
    fn selective_sync_empty_include_means_all() {
        let matcher = IgnoreMatcher::from_config_patterns(
            &[],
            &["node_modules".to_string(), ".git".to_string()],
        );

        assert!(!matcher.is_ignored("src/main.rs"));
        assert!(!matcher.is_ignored("README.md"));
        assert!(matcher.is_ignored("node_modules/foo/bar.js"));
        assert!(matcher.is_ignored(".git/config"));
    }
}
